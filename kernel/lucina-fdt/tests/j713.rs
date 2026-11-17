//! The MacBook Air M4 board's own device tree, read by our own FDT reader.
//!
//! Stage S1 of the real-hardware path (scripts/mac-payload.sh) compiles `t8132-j713.dts` out of Asahi's Linux clone to
//! `out/mac-payload/j713.dtb`; this is the same file the payload carries and the same bytes the kernel will get in x0
//! from m1n1, parsed by the reader the kernel uses. The DTB is a build output under `out/` and is never shipped in the
//! repository, so without that file the test says it skipped.
//!
//! The facts match tests/mac-payload/check.sh, which reads the tree through dtc instead: two readers of one file. What
//! m1n1 later rewrites in it (memory, chosen, framebuffer, CPUs, MACs and reserved regions,
//! docs/research/mac-m4/stage0.md:102) is asserted here as the *placeholder* the shipped tree carries, because that is
//! what our reader sees before m1n1 has touched it.

use std::fs;

use lucina_fdt::{Fdt, Node};

fn path() -> String {
    std::env::var("LUCINA_J713_DTB").unwrap_or_else(|_| {
        format!(
            "{}/../../out/mac-payload/j713.dtb",
            env!("CARGO_MANIFEST_DIR")
        )
    })
}

fn strings(node: Node<'_>, prop: &str) -> Vec<String> {
    node.property(prop)
        .unwrap()
        .map(|p| p.strings().map(|s| s.unwrap().to_string()).collect())
        .unwrap_or_default()
}

fn compatible(node: Node<'_>) -> Vec<String> {
    strings(node, "compatible")
}

fn children(node: Node<'_>) -> Vec<Node<'_>> {
    node.children().map(|c| c.unwrap()).collect()
}

/// The children whose name starts with `base`, whether or not they carry a unit address.
fn named<'a>(node: Node<'a>, base: &str) -> Vec<Node<'a>> {
    children(node)
        .into_iter()
        .filter(|c| c.name().split('@').next() == Some(base))
        .collect()
}

fn strings_of(fdt: &Fdt<'_>, path: &str, prop: &str) -> Vec<String> {
    strings(fdt.node(path).unwrap().expect(path), prop)
}

#[test]
fn the_board_tree_reads_and_says_what_the_payload_needs() {
    let path = path();
    let Ok(bytes) = fs::read(&path) else {
        eprintln!("skipped: {path} is not here; run scripts/mac-payload.sh dtb first");
        return;
    };
    let fdt = Fdt::new(&bytes).expect("the real j713 tree must parse with our reader");
    assert_eq!(
        fdt.total_size(),
        bytes.len(),
        "the header's totalsize is the file"
    );
    assert_eq!(fdt.version(), 17, "the tree dtc 1.7 writes");

    // The gate m1n1 applies to every appended device tree: the root compatible has to name this board.
    let root = fdt.root().unwrap();
    let root_compat = compatible(root);
    assert_eq!(
        root_compat.first().map(String::as_str),
        Some("apple,j713"),
        "{root_compat:?}"
    );
    assert!(
        root_compat.contains(&"apple,t8132".into()),
        "{root_compat:?}"
    );

    // The RAM, as the tree says it before m1n1 patches it: 8 GiB from the Mac's 32 GiB base.
    let mem: Vec<_> = fdt
        .memory()
        .unwrap()
        .map(|r| {
            let r = r.unwrap();
            (r.base, r.size)
        })
        .collect();
    assert_eq!(mem, [(0x8_0000_0000, 0x2_0000_0000)], "memory@800000000");
    assert_eq!(
        fdt.mem_reservations().count(),
        0,
        "nothing reserved in the blob itself"
    );

    // `/chosen`: the console is named by an alias, and neither the arguments nor the initramfs nor the display are
    // there yet - m1n1 puts all three in.
    let chosen = fdt.chosen().unwrap().expect("the tree has a /chosen");
    assert_eq!(chosen.bootargs, None, "m1n1 brings the boot arguments");
    assert_eq!(chosen.initrd, None, "m1n1 brings the initramfs");
    assert_eq!(chosen.stdout_path, Some("serial0"), "the alias, not a path");
    assert_eq!(
        chosen.framebuffer, None,
        "the placeholder node is declined, as patch 2021 says a frameless one must be"
    );
    assert_eq!(
        strings_of(&fdt, "/aliases", "serial0"),
        ["/soc/serial@3ad200000".to_string()],
        "the alias points at the Apple UART"
    );

    // The bus the devices hang on decodes two-cell addresses, which is why every `reg` below reads 2+2.
    let soc = fdt.node("/soc").unwrap().expect("/soc");
    assert_eq!(
        (soc.address_cells().unwrap(), soc.size_cells().unwrap()),
        (2, 2)
    );

    // The interrupt controller our AIC driver is declared for, with both windows as patch 2031 maps them.
    let aic: Vec<_> = children(soc)
        .into_iter()
        .filter(|c| compatible(*c).iter().any(|s| s == "apple,t8122-aic3"))
        .collect();
    assert_eq!(
        aic.len(),
        1,
        "one AIC, and its list names the t8122 generation we drive it as"
    );
    let aic = aic[0];
    assert_eq!(compatible(aic), ["apple,t8132-aic3", "apple,t8122-aic3"]);
    assert_eq!(
        aic.property("#interrupt-cells")
            .unwrap()
            .unwrap()
            .as_u32()
            .unwrap(),
        3
    );
    let windows: Vec<_> = aic.reg(2, 2).unwrap().map(|r| (r.base, r.size)).collect();
    assert_eq!(
        windows,
        [(0x3_8100_0000, 0x1c_c000), (0x3_8104_0000, 0x4000)]
    );
    assert!(
        windows[0].0 < windows[1].0 && windows[1].0 + windows[1].1 <= windows[0].0 + windows[0].1,
        "the event window is a slice of the core one: {windows:?}"
    );
    assert_eq!(strings(aic, "reg-names"), ["core", "event"]);
    let phandle = aic.property("phandle").unwrap().unwrap().as_u32().unwrap();

    // The console device itself: the one node our UART driver's compatible picks out.
    let uart: Vec<_> = children(soc)
        .into_iter()
        .filter(|c| compatible(*c).iter().any(|s| s == "apple,s5l-uart"))
        .collect();
    assert_eq!(uart.len(), 1, "one SART UART in the tree");
    assert_eq!(uart[0].name(), "serial@3ad200000");
    let uart_reg: Vec<_> = uart[0]
        .reg(2, 2)
        .unwrap()
        .map(|r| (r.base, r.size))
        .collect();
    assert_eq!(uart_reg, [(0x3_ad20_0000, 0x1000)]);

    // The framebuffer the payload will get filled in by m1n1: present, and empty of everything but its compatible list.
    let chosen_node = fdt.node("/chosen").unwrap().unwrap();
    assert_eq!(
        (
            chosen_node.address_cells().unwrap(),
            chosen_node.size_cells().unwrap()
        ),
        (2, 2),
        "the container declares the cells its child's reg is written in"
    );
    let fb = named(chosen_node, "framebuffer");
    assert_eq!(fb.len(), 1, "one framebuffer child of /chosen");
    assert_eq!(
        compatible(fb[0]),
        ["apple,simple-framebuffer", "simple-framebuffer"]
    );
    for missing in ["width", "height", "stride", "format"] {
        assert_eq!(
            fb[0].property(missing).unwrap(),
            None,
            "{missing} is m1n1's to write"
        );
    }
    assert_eq!(
        fb[0].property("status").unwrap().unwrap().as_str().unwrap(),
        "disabled"
    );
    let fb_reg: Vec<_> = fb[0].reg(2, 2).unwrap().map(|r| (r.base, r.size)).collect();
    assert_eq!(fb_reg, [(0, 0)], "the address placeholder");

    // The CPUs: ten nodes, every one a spin table whose release word m1n1 writes before it starts them.
    let cpus = fdt.node("/cpus").unwrap().expect("/cpus");
    assert_eq!(
        (cpus.address_cells().unwrap(), cpus.size_cells().unwrap()),
        (2, 0)
    );
    let cores = named(cpus, "cpu");
    assert_eq!(
        cores.len(),
        10,
        "the M4 of this board has four performance and six efficiency cores"
    );
    for c in &cores {
        assert_eq!(
            c.property("enable-method")
                .unwrap()
                .unwrap()
                .as_str()
                .unwrap(),
            "spin-table",
            "{} is started by a spin table, not by PSCI",
            c.name()
        );
        assert_eq!(
            c.property("cpu-release-addr")
                .unwrap()
                .unwrap()
                .as_u64()
                .unwrap(),
            0
        );
    }

    // The arch timer names the AIC as its own interrupt parent, which is what patch 2031 reads.
    let timer = fdt.node("/timer").unwrap().expect("the timer node");
    assert_eq!(compatible(timer), ["arm,armv8-timer"]);
    assert_eq!(
        timer
            .property("interrupt-parent")
            .unwrap()
            .unwrap()
            .as_u32()
            .unwrap(),
        phandle
    );
    assert_eq!(
        strings_of(&fdt, "/timer", "interrupt-names"),
        ["phys", "virt", "hyp-phys", "hyp-virt"]
    );
    let spec = timer.property("interrupts").unwrap().unwrap();
    assert_eq!(
        spec.value.len(),
        12 * 4,
        "four specifiers of three cells each"
    );
    assert!(
        spec.value
            .as_chunks::<12>()
            .0
            .iter()
            .all(|group| group[..4] == [0, 0, 0, 1]),
        "every timer interrupt is a PPI"
    );

    // The reserved-memory container the tree ships: there, addressing, and empty for m1n1 to fill.
    let reserved = fdt
        .node("/reserved-memory")
        .unwrap()
        .expect("the container is there");
    assert_eq!(
        (
            reserved.address_cells().unwrap(),
            reserved.size_cells().unwrap()
        ),
        (2, 2)
    );
    assert!(
        reserved.property("ranges").unwrap().is_some(),
        "the empty ranges is the tree's own"
    );
    assert!(
        children(reserved).is_empty(),
        "m1n1 fills it in from the ADT"
    );
    assert_eq!(
        fdt.reserved_memory()
            .unwrap()
            .expect("the container")
            .count(),
        0,
        "the reader finds no region inside it"
    );

    // And the report our own tool prints from these bytes, which is what the QEMU step and the note read.
    let report = lucina_fdt::report::info(&bytes).unwrap();
    assert!(report.contains("0x800000000 +0x200000000"), "{report}");
    assert!(report.contains("stdout-path = \"serial0\""), "{report}");
    assert!(
        !report.contains("bootargs"),
        "m1n1 has not been asked for them yet"
    );
    assert!(
        !report.contains("simple-framebuffer"),
        "the placeholder is declined"
    );
    assert!(report.contains("memreserve:\n  (none)"), "{report}");
}
