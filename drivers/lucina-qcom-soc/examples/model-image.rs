//! Writes the stand-in register image for `scripts/qemu-wifi.sh`: SMEM (2 MiB), then the command DB, then the AOSS
//! message RAM, then an RSC block. Passive RAM: it proves the kernel's read path, not any protocol.

use std::{env, fs};

#[path = "../tests/model/mod.rs"]
mod model;

fn main() {
    let path = env::args()
        .nth(1)
        .expect("usage: model-image <out> [running]");
    let running = env::args().nth(2).as_deref() == Some("running");
    let mut out = model::smem_image(running);
    out.extend(model::cmd_db_image());
    out.resize(out.len() + 0x1_0000, 0);
    let qmp_at = 0x20_0000 + 0x2_0000;
    let qmp = model::qmp_image();
    out[qmp_at..qmp_at + qmp.len()].copy_from_slice(&qmp);
    // DRV_PRNT_CHLD_CONFIG (offset 0xc of the v2 register table) of an RSC block: 3 CPs << 27 and 10 TCSes for DRV 2.
    let mut rsc = vec![0u8; 0x1_0000];
    rsc[0xc..0x10].copy_from_slice(&((3u32 << 27) | (10u32 << 12)).to_le_bytes());
    out.extend(rsc);
    fs::write(path, out).expect("write");
}
