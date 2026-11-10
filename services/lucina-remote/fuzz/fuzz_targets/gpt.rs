//! The table's checks, plan and repair on anything: they must never panic, and whatever they say to write must stay out of the
//! usable area. The input overwrites parts of the laptop's table and the CRCs are put right afterwards, or nearly every input would
//! stop at the first CRC and the rules behind it would never run. The last input byte decides which copies get their CRCs fixed.
#![no_main]

use libfuzzer_sys::fuzz_target;
use lucina_remote::gptw::{self, BLOCK, BlockWrite, OnDisk};
use lucina_ufs::gpt;

const DISK: u64 = 31191040;
const LAST: u64 = DISK - 1;
const ARRAY: usize = 128 * 128;
const ESP: (u64, u64) = (256, 66815);
const ESP_TYPE: [u8; 16] = [
    0x28, 0x73, 0x2a, 0xc1, 0x1f, 0xf8, 0xd2, 0x11, 0xba, 0x4b, 0x00, 0xa0, 0xc9, 0x3e, 0xc9, 0x3b,
];

fn array() -> Vec<u8> {
    let mut a = vec![0u8; ARRAY];
    a[..16].copy_from_slice(&ESP_TYPE);
    a[16..32].fill(1);
    a[32..40].copy_from_slice(&ESP.0.to_le_bytes());
    a[40..48].copy_from_slice(&ESP.1.to_le_bytes());
    a
}

fn header(my: u64, other: u64, array_lba: u64) -> Vec<u8> {
    let mut h = vec![0u8; BLOCK];
    h[0..8].copy_from_slice(b"EFI PART");
    h[8..12].copy_from_slice(&0x0001_0000u32.to_le_bytes());
    h[12..16].copy_from_slice(&92u32.to_le_bytes());
    h[24..32].copy_from_slice(&my.to_le_bytes());
    h[32..40].copy_from_slice(&other.to_le_bytes());
    h[40..48].copy_from_slice(&6u64.to_le_bytes());
    h[48..56].copy_from_slice(&(DISK - 6).to_le_bytes());
    h[56..72].copy_from_slice(&[7; 16]);
    h[72..80].copy_from_slice(&array_lba.to_le_bytes());
    h[80..84].copy_from_slice(&128u32.to_le_bytes());
    h[84..88].copy_from_slice(&128u32.to_le_bytes());
    h
}

/// Records of (which buffer, 16-bit offset, value), applied one after the other.
fn patch(buffers: &mut [&mut Vec<u8>; 4], data: &[u8]) {
    for r in data.chunks_exact(4) {
        let buffer = &mut buffers[r[0] as usize % 4];
        let at = u16::from_le_bytes([r[1], r[2]]) as usize;
        let len = buffer.len();
        buffer[at % len] = r[3];
    }
}

fn fix_crcs(h: &mut [u8], a: &[u8]) {
    let bytes = (u32::from_le_bytes(h[80..84].try_into().unwrap()) as usize)
        .saturating_mul(u32::from_le_bytes(h[84..88].try_into().unwrap()) as usize)
        .min(a.len());
    let crc = gpt::crc32(&a[..bytes]);
    h[88..92].copy_from_slice(&crc.to_le_bytes());
    h[16..20].fill(0);
    let size = (u32::from_le_bytes(h[12..16].try_into().unwrap()) as usize).clamp(92, BLOCK);
    let crc = gpt::crc32(&h[..size]);
    h[16..20].copy_from_slice(&crc.to_le_bytes());
}

fn check(view: &OnDisk, what: &str, writes: &[BlockWrite]) {
    // Only reached for a table a sound copy vouches for: its usable area ends behind the ESP's.
    for w in writes {
        assert!(w.lba >= 1 && w.lba < view.disk_blocks, "{what}: LBA {}", w.lba);
        assert_eq!(w.data.len(), BLOCK, "{what}: {} bytes", w.data.len());
    }
}

fuzz_target!(|data: &[u8]| {
    let (mut ph, mut pa, mut bh, mut ba) = (header(1, LAST, 2), array(), header(LAST, 1, LAST - 4), array());
    patch(&mut [&mut ph, &mut pa, &mut bh, &mut ba], data);
    let fix = data.last().copied().unwrap_or(3);
    if fix & 1 != 0 {
        fix_crcs(&mut ph, &pa);
    }
    if fix & 2 != 0 {
        fix_crcs(&mut bh, &ba);
    }
    let _ = gpt::parse_entries(&gpt::parse_header(&ph).unwrap_or_else(|| gpt::parse_header(&header(1, LAST, 2)).unwrap()), &pa);
    let view = OnDisk {
        disk_blocks: DISK,
        primary_header: &ph,
        primary_array: &pa,
        backup_header: &bh,
        backup_array: &ba,
    };
    let _ = gptw::check_both(&view);
    if let Ok(plan) = gptw::plan(&view, ESP, 8 << 18) {
        let writes: Vec<_> = plan.backup.iter().chain(&plan.primary).cloned().collect();
        check(&view, "plan", &writes);
        for p in &plan.partitions {
            assert!(p.first > ESP.1 && p.first <= p.last, "{p:?}");
        }
    }
    if let Ok(Some(writes)) = gptw::repair(&view) {
        check(&view, "repair", &writes);
    }
});
