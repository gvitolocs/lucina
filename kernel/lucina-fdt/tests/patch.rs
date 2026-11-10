//! The no_std rewriter ([`lucina_fdt::patch`]) against the QEMU virt fixture and small hand-built trees.

use lucina_fdt::{Edit, Error, Fdt, MAX_EDITS, NodeBuf, Region, Tree, cells32, patch, string};

const QEMU: &[u8] = include_bytes!("data/qemu-virt-gicv3-8g.dtb");

fn patched(src: &[u8], edits: &[Edit<'_>]) -> Vec<u8> {
    let fdt = Fdt::new(src).unwrap();
    let mut out = vec![0u8; src.len() + 4096];
    let n = patch(&fdt, edits, &mut out).unwrap();
    out.truncate(n);
    out
}

/// The tree QEMU hands to a kernel booted with `-initrd` and `-append`: same fixture, `/chosen` filled.
fn qemu_with_initrd() -> Vec<u8> {
    let mut t = Tree::parse(QEMU).unwrap();
    t.set_prop("/chosen", "bootargs", string("console=ttyAMA0"))
        .unwrap();
    t.set_prop("/chosen", "linux,initrd-start", cells32(&[0x4800_0000]))
        .unwrap();
    t.set_prop("/chosen", "linux,initrd-end", cells32(&[0x4810_0000]))
        .unwrap();
    t.to_bytes().unwrap()
}

#[test]
fn no_edits_round_trips() {
    let out = patched(QEMU, &[]);
    assert_eq!(Tree::parse(&out).unwrap(), Tree::parse(QEMU).unwrap());
    let fdt = Fdt::new(&out).unwrap();
    assert_eq!(fdt.version(), 17);
    assert_eq!(
        fdt.boot_cpuid_phys(),
        Fdt::new(QEMU).unwrap().boot_cpuid_phys()
    );
    assert_eq!(fdt.total_size(), out.len());
}

#[test]
fn replaces_adds_and_removes_in_chosen() {
    let src = qemu_with_initrd();
    let start = 0x4900_0000u64.to_be_bytes();
    let end = 0x4920_0000u64.to_be_bytes();
    let out = patched(
        &src,
        &[
            Edit::Set {
                node: "/chosen",
                name: "bootargs",
                value: b"SHELL=/bin/sh -- sh\0",
            },
            Edit::Set {
                node: "/chosen",
                name: "linux,initrd-start",
                value: &start,
            },
            Edit::Set {
                node: "/chosen",
                name: "linux,initrd-end",
                value: &end,
            },
            Edit::Set {
                node: "/chosen",
                name: "lucina,loader",
                value: b"LucinaBoot\0",
            },
            Edit::Remove {
                node: "/chosen",
                name: "rng-seed",
            },
        ],
    );
    let fdt = Fdt::new(&out).unwrap();
    let chosen = fdt.chosen().unwrap().unwrap();
    assert_eq!(chosen.bootargs, Some("SHELL=/bin/sh -- sh"));
    assert_eq!(chosen.initrd, Some((0x4900_0000, 0x4920_0000)));
    assert_eq!(chosen.stdout_path, Some("/pl011@9000000"));
    assert_eq!(
        fdt.property("/chosen", "lucina,loader")
            .unwrap()
            .unwrap()
            .as_str()
            .unwrap(),
        "LucinaBoot"
    );
    assert!(fdt.property("/chosen", "rng-seed").unwrap().is_none());

    // Everything else is untouched.
    let mut expect = Tree::parse(&src).unwrap();
    expect
        .set_prop("/chosen", "bootargs", string("SHELL=/bin/sh -- sh"))
        .unwrap();
    expect
        .set_prop("/chosen", "linux,initrd-start", start.to_vec())
        .unwrap();
    expect
        .set_prop("/chosen", "linux,initrd-end", end.to_vec())
        .unwrap();
    expect
        .set_prop("/chosen", "lucina,loader", string("LucinaBoot"))
        .unwrap();
    expect.remove_prop("/chosen", "rng-seed").unwrap();
    assert_eq!(Tree::parse(&out).unwrap(), expect);
}

#[test]
fn memory_path_without_unit_address() {
    let reg = cells32(&[0, 0x8000_0000, 0, 0x2000_0000]);
    let out = patched(
        QEMU,
        &[Edit::Set {
            node: "/memory",
            name: "reg",
            value: &reg,
        }],
    );
    let fdt = Fdt::new(&out).unwrap();
    let mem: Vec<Region> = fdt.memory().unwrap().map(Result::unwrap).collect();
    assert_eq!(
        mem,
        [Region {
            base: 0x8000_0000,
            size: 0x2000_0000
        }]
    );
}

#[test]
fn creates_missing_node_with_its_properties() {
    let mut t = Tree::parse(QEMU).unwrap();
    assert!(t.remove_node("/chosen").unwrap());
    let src = t.to_bytes().unwrap();
    let out = patched(
        &src,
        &[
            Edit::Set {
                node: "/chosen",
                name: "bootargs",
                value: b"a\0",
            },
            Edit::Set {
                node: "/chosen",
                name: "stdout-path",
                value: b"/pl011@9000000\0",
            },
            Edit::Set {
                node: "/chosen",
                name: "bootargs",
                value: b"b\0",
            },
            // A Remove on a node that does not exist is a no-op.
            Edit::Remove {
                node: "/nothing",
                name: "x",
            },
        ],
    );
    let fdt = Fdt::new(&out).unwrap();
    let chosen = fdt.chosen().unwrap().unwrap();
    assert_eq!(chosen.bootargs, Some("b"), "the last edit wins");
    assert_eq!(chosen.stdout_path, Some("/pl011@9000000"));
    let n = fdt.node("/chosen").unwrap().unwrap();
    assert_eq!(n.properties().count(), 2, "no duplicate bootargs");
}

#[test]
fn added_properties_come_before_children() {
    // `/a` has a child; a property added to `/a` must be written before it, or readers never see it.
    let mut a = NodeBuf::new("a");
    a.set_prop("x", cells32(&[1]));
    a.children.push(NodeBuf::new("b"));
    let mut t = Tree::parse(QEMU).unwrap();
    t.root.children.push(a);
    let src = t.to_bytes().unwrap();
    let out = patched(
        &src,
        &[Edit::Set {
            node: "/a",
            name: "y",
            value: &[0, 0, 0, 2],
        }],
    );
    let fdt = Fdt::new(&out).unwrap();
    let a = fdt.node("/a").unwrap().unwrap();
    let names: Vec<&str> = a.properties().map(|p| p.unwrap().name).collect();
    assert_eq!(names, ["x", "y"]);
    assert!(fdt.node("/a/b").unwrap().is_some());
}

#[test]
fn nops_are_dropped() {
    // QEMU overwrites the fixture's /memory with NOPs at boot; the reader skips NOPs, the patcher drops them.
    // Here a 4-byte property `pad` (16 bytes of structure: tag, len, nameoff, value) becomes four NOPs.
    let mut t = Tree::parse(QEMU).unwrap();
    t.set_prop("/chosen", "pad", vec![0; 4]).unwrap();
    let mut src = t.to_bytes().unwrap();
    let fdt = Fdt::new(&src).unwrap();
    let pad = fdt.property("/chosen", "pad").unwrap().unwrap();
    let tag_off = pad.value.as_ptr() as usize - src.as_ptr() as usize - 12;
    for k in 0..4 {
        src[tag_off + 4 * k..tag_off + 4 * k + 4].copy_from_slice(&4u32.to_be_bytes());
    }
    assert!(
        Fdt::new(&src)
            .unwrap()
            .property("/chosen", "pad")
            .unwrap()
            .is_none()
    );
    let out = patched(&src, &[]);
    assert_eq!(out.len(), src.len() - 16);
    assert_eq!(Tree::parse(&out).unwrap(), Tree::parse(QEMU).unwrap());
}

#[test]
fn small_buffers_fail_cleanly() {
    let src = qemu_with_initrd();
    let fdt = Fdt::new(&src).unwrap();
    let edits = [Edit::Set {
        node: "/chosen",
        name: "lucina,new",
        value: b"value\0",
    }];
    let mut big = vec![0u8; src.len() + 64];
    let need = patch(&fdt, &edits, &mut big).unwrap();
    for len in (0..need).step_by(7).chain([need - 1]) {
        let mut out = vec![0u8; len];
        assert_eq!(
            patch(&fdt, &edits, &mut out),
            Err(Error::BufferTooSmall),
            "len {len}"
        );
    }
    let mut exact = vec![0u8; need];
    assert_eq!(patch(&fdt, &edits, &mut exact), Ok(need));
}

#[test]
fn rejects_bad_edits() {
    let fdt = Fdt::new(QEMU).unwrap();
    let mut out = vec![0u8; QEMU.len() + 4096];
    let set = |node, name| Edit::Set {
        node,
        name,
        value: &[],
    };
    assert_eq!(
        patch(&fdt, &[set("chosen", "x")], &mut out),
        Err(Error::BadPath)
    );
    assert_eq!(
        patch(&fdt, &[set("/chosen", "")], &mut out),
        Err(Error::BadPath)
    );
    assert_eq!(
        patch(&fdt, &[set("/chosen", "a/b")], &mut out),
        Err(Error::BadPath)
    );
    assert_eq!(
        patch(&fdt, &[set("/no/such/node", "x")], &mut out),
        Err(Error::NotFound)
    );
    let many = [set("/chosen", "x"); MAX_EDITS + 1];
    assert_eq!(patch(&fdt, &many, &mut out), Err(Error::TooLarge));
}

#[test]
fn new_names_are_shared_and_existing_names_reused() {
    let src = qemu_with_initrd();
    let strings_len = u32::from_be_bytes(src[32..36].try_into().unwrap()) as usize;
    let out = patched(
        &src,
        &[
            Edit::Set {
                node: "/chosen",
                name: "lucina,a",
                value: &[],
            },
            Edit::Set {
                node: "/",
                name: "lucina,a",
                value: &[],
            },
            // `bootargs` is already in the strings block.
            Edit::Set {
                node: "/",
                name: "bootargs",
                value: b"x\0",
            },
        ],
    );
    let new_strings = u32::from_be_bytes(out[32..36].try_into().unwrap()) as usize;
    assert_eq!(new_strings, strings_len + "lucina,a".len() + 1);
    let fdt = Fdt::new(&out).unwrap();
    assert!(fdt.property("/", "lucina,a").unwrap().is_some());
    assert!(fdt.property("/chosen", "lucina,a").unwrap().is_some());
    assert_eq!(
        fdt.property("/", "bootargs")
            .unwrap()
            .unwrap()
            .as_str()
            .unwrap(),
        "x"
    );
}
