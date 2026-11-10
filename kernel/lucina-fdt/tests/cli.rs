use std::{fs, os::unix::fs::symlink, path::PathBuf, process::Command};

const QEMU: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/data/qemu-virt-gicv3-8g.dtb"
);

fn bin() -> Command {
    Command::new(env!("CARGO_BIN_EXE_lucina-fdt"))
}

fn scratch(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("lucina-fdt-cli-{name}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&d);
    fs::create_dir_all(&d).unwrap();
    d
}

#[test]
fn info_prints_memory_and_chosen() {
    let out = bin().args(["info", QEMU]).output().unwrap();
    assert!(out.status.success());
    let s = String::from_utf8(out.stdout).unwrap();
    assert!(s.contains("0x40000000 +0x200000000"), "{s}");
    assert!(s.contains("reserved-memory:\n  (none)"), "{s}");
    assert!(s.contains("stdout-path = \"/pl011@9000000\""), "{s}");
}

#[test]
fn add_reserved_then_info() {
    let d = scratch("add");
    let out = d.join("out.dtb");
    let st = bin()
        .args(["add-reserved", QEMU, "-o"])
        .arg(&out)
        .args([
            "--region",
            "fw@48000000+200000,no-map",
            "--region",
            "pool@0x50000000+0x100000",
        ])
        .status()
        .unwrap();
    assert!(st.success());
    let s = String::from_utf8(bin().arg("info").arg(&out).output().unwrap().stdout).unwrap();
    assert!(
        s.contains("  fw@48000000: 0x48000000 +0x200000 no-map\n"),
        "{s}"
    );
    assert!(s.contains("  pool@50000000: 0x50000000 +0x100000\n"), "{s}");
    fs::remove_dir_all(d).unwrap();
}

#[test]
fn refuses_dev_symlink_and_bad_arguments() {
    let d = scratch("refuse");
    let target = d.join("target");
    fs::write(&target, b"keep").unwrap();
    symlink(&target, d.join("link")).unwrap();
    for out in [PathBuf::from("/dev/null"), d.join("link")] {
        let st = bin()
            .args(["add-reserved", QEMU, "-o"])
            .arg(&out)
            .args(["--region", "fw@48000000+200000"])
            .output()
            .unwrap();
        assert_eq!(st.status.code(), Some(1), "{}", out.display());
    }
    assert_eq!(fs::read(&target).unwrap(), b"keep");
    // Usage errors exit 2; a bad region or a missing input exit 1 or 2 without writing anything.
    assert_eq!(
        bin().args(["add-reserved", QEMU]).status().unwrap().code(),
        Some(2)
    );
    assert_eq!(bin().status().unwrap().code(), Some(2));
    assert_eq!(
        bin()
            .args(["info", "/nonexistent.dtb"])
            .status()
            .unwrap()
            .code(),
        Some(1)
    );
    assert_eq!(
        bin().args(["info", "/dev/zero"]).status().unwrap().code(),
        Some(1)
    );
    fs::remove_dir_all(d).unwrap();
}

#[test]
fn add_framebuffer_reads_back_through_the_reader() {
    use lucina_fdt::{Fdt, Region};
    let d = scratch("fb");
    let out = d.join("out.dtb");
    let st = bin()
        .args(["add-framebuffer", QEMU, "-o"])
        .arg(&out)
        .args([
            "--addr",
            "0x7f000000",
            "--width",
            "1080",
            "--height",
            "2400",
        ])
        .args(["--format", "a8r8g8b8", "--no-map"])
        .status()
        .unwrap();
    assert!(st.success());
    let bytes = fs::read(&out).unwrap();
    let fdt = Fdt::new(&bytes).unwrap();
    let size = 1080 * 4 * 2400u64;

    let node = fdt.node("/chosen/framebuffer@7f000000").unwrap().unwrap();
    let p = |n: &str| node.property(n).unwrap().unwrap();
    assert_eq!(p("compatible").as_str(), Ok("simple-framebuffer"));
    assert_eq!(p("width").as_u32(), Ok(1080));
    assert_eq!(p("height").as_u32(), Ok(2400));
    assert_eq!(p("stride").as_u32(), Ok(4320));
    assert_eq!(p("format").as_str(), Ok("a8r8g8b8"));
    assert_eq!(p("status").as_str(), Ok("okay"));
    let reg: Vec<_> = p("reg").reg(2, 2).unwrap().collect();
    assert_eq!(
        reg,
        [Region {
            base: 0x7f00_0000,
            size
        }]
    );

    let rm: Vec<_> = fdt
        .reserved_memory()
        .unwrap()
        .unwrap()
        .map(Result::unwrap)
        .collect();
    assert_eq!(rm.len(), 1);
    assert_eq!(rm[0].name(), "framebuffer@7f000000");
    assert!(rm[0].no_map);
    assert_eq!(rm[0].regs().unwrap().collect::<Vec<_>>(), reg);

    let fb = fdt.chosen().unwrap().unwrap().framebuffer.unwrap();
    assert_eq!(
        (fb.base, fb.size, fb.stride, fb.format),
        (0x7f00_0000, size, 4320, "a8r8g8b8")
    );
    let info = String::from_utf8(bin().arg("info").arg(&out).output().unwrap().stdout).unwrap();
    assert!(
        info.contains("simple-framebuffer framebuffer@7f000000: 0x7f000000"),
        "{info}"
    );

    // Without --no-map there is no reserved-memory node; r5g6b5 uses 2 bytes per pixel.
    let out2 = d.join("out2.dtb");
    assert!(
        bin()
            .args(["add-framebuffer", QEMU, "-o"])
            .arg(&out2)
            .args([
                "--addr", "7f000000", "--width", "100", "--height", "10", "--format", "r5g6b5"
            ])
            .status()
            .unwrap()
            .success()
    );
    let bytes = fs::read(&out2).unwrap();
    let fdt = Fdt::new(&bytes).unwrap();
    assert!(fdt.reserved_memory().unwrap().is_none());
    assert_eq!(
        fdt.property("/chosen/framebuffer@7f000000", "stride")
            .unwrap()
            .unwrap()
            .as_u32(),
        Ok(200)
    );

    // Bad parameters fail without writing.
    let out3 = d.join("out3.dtb");
    let code = bin()
        .args(["add-framebuffer", QEMU, "-o"])
        .arg(&out3)
        .args([
            "--addr", "1000", "--width", "0", "--height", "10", "--format", "r5g6b5",
        ])
        .status()
        .unwrap()
        .code();
    assert_eq!(code, Some(1));
    assert!(!out3.exists());
    fs::remove_dir_all(d).unwrap();
}
