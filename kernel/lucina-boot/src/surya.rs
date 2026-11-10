//! The device tree appended to LucinaBoot in the surya boot image (host only).
//!
//! ABL picks an appended device tree by `qcom,msm-id` and `qcom,board-id`, fills in `/memory` and `/chosen`, and
//! passes it to the "kernel" in `x0` (`docs/research/boot-chain.md` sections 4-5). For LucinaBoot that tree only
//! has to say what LucinaBoot itself needs: PSCI over SMC (for the reset), the boot framebuffer, and every
//! firmware carve-out it must not place anything on. The full SoC description for the kernel travels in the
//! payload package as the panel device trees.
//!
//! The reserved map is section 2 of `docs/research/surya-hardware-inventory.md` (mainline `sm7150.dtsi` and
//! `sm7150-xiaomi-surya.dtsi`, checked against the downstream FDT dump), with the downstream sizes where they are
//! larger. It still has to be compared with the tree ABL really hands over (T-DEV-00).

use lucina_fdt::{Error, NodeBuf, Tree, cells32, string};

/// `QCOM_ID_SM7150` (`include/dt-bindings/arm/qcom,ids.h`).
pub const MSM_ID: u32 = 365;
/// surya's board id (mainline `sm7150-xiaomi-surya.dtsi`; the stock FDT reports the same).
pub const BOARD_ID: u32 = 34;
/// The ABL splash framebuffer: 1080x2400 a8r8g8b8, stride 4320.
pub const FB_BASE: u64 = 0x9c00_0000;
pub const FB_WIDTH: u32 = 1080;
pub const FB_HEIGHT: u32 = 2400;
pub const FB_STRIDE: u32 = 4320;

/// `(node name, base, size)`: every region is `no-map`.
pub const RESERVED: &[(&str, u64, u64)] = &[
    ("hyp", 0x8570_0000, 0x60_0000),
    // Mainline: XBL 0x85d00000 (2 MiB), AOP 0x85f00000, cmd-db 0x85f20000; downstream: xbl_aop 0x85e00000 + 0x1ff000.
    // Their union, as one region.
    ("xbl-aop-cmd-db", 0x85d0_0000, 0x2f_f000),
    ("sec-apps", 0x85ff_f000, 0x1000),
    ("smem", 0x8600_0000, 0x20_0000),
    ("tz", 0x8620_0000, 0x2d0_0000),
    ("camera", 0x8ab0_0000, 0x50_0000),
    ("mpss", 0x8b00_0000, 0x840_0000),
    ("venus", 0x9340_0000, 0x50_0000),
    ("cdsp", 0x9390_0000, 0x1e0_0000),
    ("adsp", 0x9570_0000, 0x1e0_0000),
    ("wlan-msa", 0x9750_0000, 0x18_0000),
    ("npu", 0x9768_0000, 0x8_0000),
    ("ipa-fw-gsi-gpu", 0x9770_0000, 0x1_7000),
    ("cont-splash", FB_BASE, 0x170_0000),
    ("dfps-data", 0x9d70_0000, 0x10_0000),
    ("ramoops", 0x9d80_0000, 0x40_0000),
    ("qseecom", 0x9e40_0000, 0x140_0000),
    ("sec-cdsp", 0x9f80_0000, 0x1e0_0000),
    ("ramdump-fb", 0xaf00_0000, 0x100_0000),
    ("rmtfs", 0xfde0_1000, 0x20_0000),
];

fn reg(base: u64, size: u64) -> Vec<u8> {
    cells32(&[
        (base >> 32) as u32,
        base as u32,
        (size >> 32) as u32,
        size as u32,
    ])
}

/// The appended device tree, as a version-17 blob.
pub fn abl_stub_dtb() -> Result<Vec<u8>, Error> {
    let mut root = NodeBuf::new("");
    root.set_prop("#address-cells", cells32(&[2]));
    root.set_prop("#size-cells", cells32(&[2]));
    root.set_prop("model", string("Xiaomi POCO X3 NFC (LucinaBoot)"));
    root.set_prop(
        "compatible",
        [string("xiaomi,surya"), string("qcom,sm7150")].concat(),
    );
    root.set_prop("qcom,msm-id", cells32(&[MSM_ID, 0]));
    root.set_prop("qcom,board-id", cells32(&[BOARD_ID, 0]));

    let mut chosen = NodeBuf::new("chosen");
    chosen.set_prop("#address-cells", cells32(&[2]));
    chosen.set_prop("#size-cells", cells32(&[2]));
    chosen.set_prop("ranges", Vec::new());
    let mut fb = NodeBuf::new(&format!("framebuffer@{FB_BASE:x}"));
    fb.set_prop("compatible", string("simple-framebuffer"));
    fb.set_prop(
        "reg",
        reg(FB_BASE, u64::from(FB_STRIDE) * u64::from(FB_HEIGHT)),
    );
    fb.set_prop("width", cells32(&[FB_WIDTH]));
    fb.set_prop("height", cells32(&[FB_HEIGHT]));
    fb.set_prop("stride", cells32(&[FB_STRIDE]));
    fb.set_prop("format", string("a8r8g8b8"));
    chosen.children.push(fb);
    root.children.push(chosen);

    // ABL fills in the size (and a second bank on 6 GB units).
    let mut memory = NodeBuf::new("memory@80000000");
    memory.set_prop("device_type", string("memory"));
    memory.set_prop("reg", reg(0x8000_0000, 0));
    root.children.push(memory);

    let mut psci = NodeBuf::new("psci");
    psci.set_prop("compatible", string("arm,psci-1.0"));
    psci.set_prop("method", string("smc"));
    root.children.push(psci);

    let mut rm = NodeBuf::new("reserved-memory");
    rm.set_prop("#address-cells", cells32(&[2]));
    rm.set_prop("#size-cells", cells32(&[2]));
    rm.set_prop("ranges", Vec::new());
    for &(name, base, size) in RESERVED {
        let mut n = NodeBuf::new(&format!("{name}@{base:x}"));
        n.set_prop("reg", reg(base, size));
        n.set_prop("no-map", Vec::new());
        rm.children.push(n);
    }
    root.children.push(rm);

    Tree {
        root,
        ..Tree::default()
    }
    .to_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Range;
    use lucina_fdt::Fdt;

    #[test]
    fn stub_has_what_abl_and_lucinaboot_read() {
        let dtb = abl_stub_dtb().unwrap();
        let fdt = Fdt::new(&dtb).unwrap();
        let root = fdt.root().unwrap();
        let msm = root.property("qcom,msm-id").unwrap().unwrap();
        assert_eq!(msm.value, cells32(&[365, 0]));
        let board = root.property("qcom,board-id").unwrap().unwrap();
        assert_eq!(board.value, cells32(&[34, 0]));
        assert_eq!(
            fdt.property("/psci", "method")
                .unwrap()
                .unwrap()
                .as_str()
                .unwrap(),
            "smc"
        );
        let fb = fdt.chosen().unwrap().unwrap().framebuffer.unwrap();
        assert_eq!(
            (fb.base, fb.width, fb.height, fb.stride, fb.format),
            (FB_BASE, 1080, 2400, 4320, "a8r8g8b8")
        );
        let regions: Vec<_> = fdt
            .reserved_memory()
            .unwrap()
            .unwrap()
            .map(|r| {
                let r = r.unwrap();
                assert!(r.no_map);
                let reg = r.regs().unwrap().next().unwrap();
                Range::new(reg.base, reg.size).unwrap()
            })
            .collect();
        assert_eq!(regions.len(), RESERVED.len());
        // Sorted and disjoint, all inside the first 4 GiB, none where the kernel goes (0x80200000).
        for w in regions.windows(2) {
            assert!(
                w[0].end <= w[1].start,
                "{} and {} overlap or are unsorted",
                w[0],
                w[1]
            );
        }
        let kernel = Range::new(0x8020_0000, 64 << 20).unwrap();
        assert!(
            regions
                .iter()
                .all(|r| !r.overlaps(&kernel) && r.end <= 1 << 32)
        );
        // The size ABL fills in starts at 0: LucinaBoot ignores an empty region rather than using it.
        assert_eq!(fdt.memory().unwrap().next().unwrap().unwrap().size, 0);
    }
}
