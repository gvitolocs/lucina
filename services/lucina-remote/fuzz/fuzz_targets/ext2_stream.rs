//! What the installer reads before it trusts an image: the superblock, the stream's header and its extents. It must never panic,
//! and an extent that `extent_fits` passes must lead to byte addresses that fit.
#![no_main]

use libfuzzer_sys::fuzz_target;
use lucina_remote::{
    disk_rules,
    image::{self, Header, read_extent},
};

fuzz_target!(|data: &[u8]| {
    if let Some(info) = image::parse_superblock(data) {
        let _ = image::uuid_hex(&info.uuid);
    }
    let _ = image::with_state(data.get(..4096).unwrap_or(&[0; 4096]), 0);
    let mut input = data;
    let Ok(header) = Header::read(&mut input) else {
        return;
    };
    let room = 2097152;
    for _ in 0..header.extents.min(1000) {
        let Ok(e) = read_extent(&mut input) else {
            return;
        };
        if disk_rules::extent_fits(e.first, e.count, header.blocks, room) {
            assert!(e.first + e.count <= room);
            assert!((e.first + e.count).checked_mul(4096).is_some());
        }
    }
});
