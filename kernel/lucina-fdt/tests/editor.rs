//! Reader and editor against the DTB QEMU generates for `-machine virt,gic-version=3 -cpu cortex-a72 -m 8G`
//! (dumped with `dumpdtb=` from the pinned asterinas/kernel-dev image; see `data/README`-less note in lib.rs).

use lucina_fdt::{Error, Fdt, NodeBuf, Region, RegionSpec, Tree, cells32, string};

const QEMU: &[u8] = include_bytes!("data/qemu-virt-gicv3-8g.dtb");

#[test]
fn reads_qemu_virt() {
    let fdt = Fdt::new(QEMU).unwrap();
    assert_eq!(fdt.version(), 17);
    let mem: Vec<Region> = fdt.memory().unwrap().map(Result::unwrap).collect();
    assert_eq!(
        mem,
        [Region {
            base: 0x4000_0000,
            size: 0x2_0000_0000
        }]
    );
    assert!(fdt.reserved_memory().unwrap().is_none());
    let chosen = fdt.chosen().unwrap().unwrap();
    assert_eq!(chosen.stdout_path, Some("/pl011@9000000"));
    assert_eq!(chosen.bootargs, None);
    let root = fdt.root().unwrap();
    assert_eq!(root.address_cells().unwrap(), 2);
    assert_eq!(root.size_cells().unwrap(), 2);
    let compat: Vec<&str> = root
        .property("compatible")
        .unwrap()
        .unwrap()
        .strings()
        .map(Result::unwrap)
        .collect();
    assert_eq!(compat, ["linux,dummy-virt"]);
    // Path lookup, exact and without the unit address.
    assert_eq!(
        fdt.node("/memory@40000000").unwrap().unwrap().name(),
        "memory@40000000"
    );
    assert_eq!(
        fdt.node("/memory").unwrap().unwrap().name(),
        "memory@40000000"
    );
    assert!(fdt.node("/nope").unwrap().is_none());
    assert_eq!(fdt.node("nope").err(), Some(Error::BadPath));
}

#[test]
fn round_trip_is_stable() {
    let tree = Tree::parse(QEMU).unwrap();
    let bytes = tree.to_bytes().unwrap();
    let again = Tree::parse(&bytes).unwrap();
    assert_eq!(tree, again);
    // Serializing is deterministic and the output is canonical (a second pass is byte-identical).
    assert_eq!(again.to_bytes().unwrap(), bytes);
    let fdt = Fdt::new(&bytes).unwrap();
    assert_eq!(fdt.version(), 17);
    assert_eq!(fdt.total_size(), bytes.len());
    // Property names are deduplicated: the strings block holds each name once.
    let strings_len = u32::from_be_bytes(bytes[32..36].try_into().unwrap()) as usize;
    let off = u32::from_be_bytes(bytes[12..16].try_into().unwrap()) as usize;
    let names: Vec<&[u8]> = bytes[off..off + strings_len]
        .split(|&b| b == 0)
        .filter(|s| !s.is_empty())
        .collect();
    let mut sorted = names.clone();
    sorted.sort();
    sorted.dedup();
    assert_eq!(names.len(), sorted.len());
}

#[test]
fn memreserve_and_header_survive() {
    let mut tree = Tree {
        boot_cpuid_phys: 3,
        reservations: vec![(0x8000_0000, 0x1000), (0x9000_0000, 0x20)],
        ..Tree::default()
    };
    tree.root.set_prop("#address-cells", cells32(&[1]));
    let bytes = tree.to_bytes().unwrap();
    let fdt = Fdt::new(&bytes).unwrap();
    assert_eq!(fdt.boot_cpuid_phys(), 3);
    assert_eq!(
        fdt.mem_reservations().collect::<Vec<_>>(),
        [(0x8000_0000, 0x1000), (0x9000_0000, 0x20)]
    );
    assert_eq!(Tree::parse(&bytes).unwrap(), tree);
}

#[test]
fn add_reserved_is_visible_through_the_reader() {
    let mut tree = Tree::parse(QEMU).unwrap();
    let regions = [
        RegionSpec::parse("fw@0x48000000+0x200000,no-map").unwrap(),
        RegionSpec::parse("pool@50000000+100000").unwrap(),
    ];
    tree.add_reserved_regions(&regions).unwrap();
    let bytes = tree.to_bytes().unwrap();
    let fdt = Fdt::new(&bytes).unwrap();

    let rm = fdt.node("/reserved-memory").unwrap().unwrap();
    assert_eq!(rm.address_cells().unwrap(), 2);
    assert_eq!(rm.size_cells().unwrap(), 2);
    assert!(rm.property("ranges").unwrap().unwrap().is_empty());

    let got: Vec<_> = fdt
        .reserved_memory()
        .unwrap()
        .unwrap()
        .map(|r| {
            let r = r.unwrap();
            (r.name(), r.no_map, r.regs().unwrap().collect::<Vec<_>>())
        })
        .collect();
    assert_eq!(
        got,
        [
            (
                "fw@48000000",
                true,
                vec![Region {
                    base: 0x4800_0000,
                    size: 0x20_0000
                }]
            ),
            (
                "pool@50000000",
                false,
                vec![Region {
                    base: 0x5000_0000,
                    size: 0x10_0000
                }]
            ),
        ]
    );
    // Everything else is untouched.
    let mem: Vec<_> = fdt.memory().unwrap().map(Result::unwrap).collect();
    assert_eq!(mem.len(), 1);

    // Adding the same region again is refused, and the existing node is reused otherwise.
    assert_eq!(tree.add_reserved_regions(&regions[..1]), Err(Error::Exists));
    tree.add_reserved_regions(&[RegionSpec::parse("more@60000000+1000,no-map").unwrap()])
        .unwrap();
    assert_eq!(
        tree.find("/reserved-memory")
            .unwrap()
            .unwrap()
            .children
            .len(),
        3
    );
}

#[test]
fn reserved_region_must_fit_the_cells() {
    let mut tree = Tree::default();
    tree.root.set_prop("#address-cells", cells32(&[1]));
    tree.root.set_prop("#size-cells", cells32(&[1]));
    let big = RegionSpec::parse("x@100000000+1000").unwrap();
    assert_eq!(tree.add_reserved_regions(&[big]), Err(Error::BadCells));
}

#[test]
fn region_spec_parsing() {
    let r = RegionSpec::parse("fw@0x48000000+0x200000,no-map").unwrap();
    assert_eq!(
        (r.base, r.size, r.no_map, r.node_name().as_str()),
        (0x4800_0000, 0x20_0000, true, "fw@48000000")
    );
    for bad in [
        "",
        "fw",
        "fw@1",
        "fw@+1",
        "fw@1+",
        "fw@1+0",
        "@1+1",
        "fw@g+1",
        "fw@1+1,map",
        "fw@ffffffffffffffff+2",
    ] {
        assert_eq!(RegionSpec::parse(bad), Err(Error::BadRegion), "{bad}");
    }
}

#[test]
fn edit_operations() {
    let mut tree = Tree::parse(QEMU).unwrap();

    // Add a property, then replace it in place.
    tree.set_prop("/chosen", "bootargs", string("console=ttyAMA0"))
        .unwrap();
    tree.set_prop("/chosen", "bootargs", string("quiet"))
        .unwrap();
    assert_eq!(tree.set_prop("/missing", "x", vec![]), Err(Error::NotFound));

    // Add a node with its missing parents.
    tree.add_node("/a/b/c@1")
        .unwrap()
        .set_prop("p", cells32(&[7]));

    let bytes = tree.to_bytes().unwrap();
    let fdt = Fdt::new(&bytes).unwrap();
    assert_eq!(fdt.chosen().unwrap().unwrap().bootargs, Some("quiet"));
    assert_eq!(
        fdt.property("/a/b/c@1", "p").unwrap().unwrap().as_u32(),
        Ok(7)
    );
    assert_eq!(fdt.node("/a/b").unwrap().unwrap().children().count(), 1);

    // Delete a property and a node.
    assert!(tree.remove_prop("/chosen", "bootargs").unwrap());
    assert!(!tree.remove_prop("/chosen", "bootargs").unwrap());
    assert!(tree.remove_node("/a/b").unwrap());
    assert!(!tree.remove_node("/a/b").unwrap());
    assert!(!tree.remove_node("/nope/deeper").unwrap());
    assert_eq!(tree.remove_node("/"), Err(Error::BadPath));
    let bytes = tree.to_bytes().unwrap();
    let fdt = Fdt::new(&bytes).unwrap();
    assert_eq!(fdt.chosen().unwrap().unwrap().bootargs, None);
    assert!(fdt.node("/a/b").unwrap().is_none());
    assert!(fdt.node("/a").unwrap().is_some());
}

#[test]
fn serializer_rejects_bad_names_and_depth() {
    let mut tree = Tree::default();
    tree.root.children.push(NodeBuf::new("a/b"));
    assert_eq!(tree.to_bytes(), Err(Error::BadString));
    let mut tree = Tree::default();
    tree.root.children.push(NodeBuf::new(""));
    assert_eq!(tree.to_bytes(), Err(Error::BadString));
    let mut tree = Tree::default();
    tree.root.set_prop("bad\0name", vec![]);
    assert_eq!(tree.to_bytes(), Err(Error::BadString));
    let deep = format!("/{}", vec!["n"; 70].join("/"));
    assert_eq!(Tree::default().add_node(&deep).err(), Some(Error::TooDeep));
}

#[test]
fn reg_decoding() {
    let mut tree = Tree::default();
    tree.root.set_prop(
        "reg1",
        [cells32(&[0x10, 0x20]), cells32(&[0x30, 0x40])].concat(),
    );
    tree.root.set_prop("reg2", cells32(&[0x1, 0x2, 0x3]));
    let bytes = tree.to_bytes().unwrap();
    let fdt = Fdt::new(&bytes).unwrap();
    let root = fdt.root().unwrap();
    let r: Vec<_> = root
        .property("reg1")
        .unwrap()
        .unwrap()
        .reg(1, 1)
        .unwrap()
        .collect();
    assert_eq!(
        r,
        [
            Region {
                base: 0x10,
                size: 0x20
            },
            Region {
                base: 0x30,
                size: 0x40
            }
        ]
    );
    let r: Vec<_> = root
        .property("reg1")
        .unwrap()
        .unwrap()
        .reg(2, 0)
        .unwrap()
        .collect();
    assert_eq!(
        r,
        [
            Region {
                base: 0x10_0000_0020,
                size: 0
            },
            Region {
                base: 0x30_0000_0040,
                size: 0
            }
        ]
    );
    assert_eq!(
        root.property("reg2").unwrap().unwrap().reg(1, 1).err(),
        Some(Error::BadLength)
    );
    assert_eq!(
        root.property("reg2").unwrap().unwrap().reg(3, 0).err(),
        Some(Error::BadCells)
    );
    assert_eq!(
        root.property("reg2").unwrap().unwrap().reg(0, 1).err(),
        Some(Error::BadCells)
    );
}

#[test]
fn rejects_bad_headers() {
    let mut b = QEMU.to_vec();
    b[0] ^= 1;
    assert_eq!(Fdt::new(&b).err(), Some(Error::BadMagic));
    let mut b = QEMU.to_vec();
    b[23] = 16; // version 16
    assert_eq!(Fdt::new(&b).err(), Some(Error::UnsupportedVersion(16)));
    let mut b = QEMU.to_vec();
    b[27] = 17; // last_comp_version 17
    assert_eq!(Fdt::new(&b).err(), Some(Error::IncompatibleVersion(17)));
    let mut b = QEMU.to_vec();
    b[11] = 0x42; // off_dt_struct not 4-aligned
    assert_eq!(Fdt::new(&b).err(), Some(Error::Misaligned));
    assert_eq!(
        Fdt::new(&QEMU[..QEMU.len() - 1]).err(),
        Some(Error::BadTotalSize)
    );
    assert_eq!(Fdt::new(&[]).err(), Some(Error::Truncated));
}
