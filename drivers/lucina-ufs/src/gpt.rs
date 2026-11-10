//! GUID partition tables (UEFI 2.10 section 5.3), read for the bring-up log: on the laptop the screen is the only
//! place to see what the disk holds. The block layer parses the table again for the partitions it registers.

use alloc::{string::String, vec::Vec};
use core::fmt::Write;

/// The primary header (from LBA 1).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Header {
    pub disk_guid: [u8; 16],
    pub first_usable: u64,
    pub last_usable: u64,
    pub backup_lba: u64,
    pub entries_lba: u64,
    pub entries: u32,
    pub entry_size: u32,
    pub entries_crc: u32,
    /// The header's own CRC32 matched.
    pub crc_ok: bool,
}

/// A used partition entry.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    pub index: u32,
    pub type_guid: [u8; 16],
    pub first_lba: u64,
    pub last_lba: u64,
    pub name: String,
}

fn le32(d: &[u8], at: usize) -> u32 {
    u32::from_le_bytes([d[at], d[at + 1], d[at + 2], d[at + 3]])
}

fn le64(d: &[u8], at: usize) -> u64 {
    le32(d, at) as u64 | (le32(d, at + 4) as u64) << 32
}

/// CRC-32 (IEEE 802.3, reflected), as the GPT uses.
pub fn crc32(data: &[u8]) -> u32 {
    let mut crc = !0u32;
    for &b in data {
        crc ^= b as u32;
        for _ in 0..8 {
            crc = if crc & 1 != 0 {
                (crc >> 1) ^ 0xedb8_8320
            } else {
                crc >> 1
            };
        }
    }
    !crc
}

/// Parses the header at the start of `lba1` (the whole of LBA 1).
pub fn parse_header(lba1: &[u8]) -> Option<Header> {
    if lba1.len() < 92 || &lba1[0..8] != b"EFI PART" {
        return None;
    }
    let size = le32(lba1, 12) as usize;
    if !(92..=lba1.len()).contains(&size) {
        return None;
    }
    let mut copy = lba1[..size].to_vec();
    copy[16..20].fill(0);
    let mut disk_guid = [0u8; 16];
    disk_guid.copy_from_slice(&lba1[56..72]);
    Some(Header {
        disk_guid,
        first_usable: le64(lba1, 40),
        last_usable: le64(lba1, 48),
        backup_lba: le64(lba1, 32),
        entries_lba: le64(lba1, 72),
        entries: le32(lba1, 80),
        entry_size: le32(lba1, 84),
        entries_crc: le32(lba1, 88),
        crc_ok: crc32(&copy) == le32(lba1, 16),
    })
}

/// The used entries in `array` (the partition entry array, or its first part).
pub fn parse_entries(header: &Header, array: &[u8]) -> Vec<Entry> {
    let size = header.entry_size as usize;
    if size < 128 {
        return Vec::new();
    }
    array
        .chunks_exact(size)
        .take(header.entries as usize)
        .enumerate()
        .filter(|(_, e)| e[0..16] != [0u8; 16])
        .map(|(i, e)| {
            let mut type_guid = [0u8; 16];
            type_guid.copy_from_slice(&e[0..16]);
            let units: Vec<u16> = e[56..128]
                .as_chunks::<2>()
                .0
                .iter()
                .map(|c| u16::from_le_bytes([c[0], c[1]]))
                .take_while(|&u| u != 0)
                .collect();
            Entry {
                index: i as u32 + 1,
                type_guid,
                first_lba: le64(e, 32),
                last_lba: le64(e, 40),
                name: char::decode_utf16(units)
                    .map(|c| c.unwrap_or('?'))
                    .collect(),
            }
        })
        .collect()
}

/// A GUID in its usual text form (the first three fields are little endian on disk).
pub fn guid(g: &[u8; 16]) -> String {
    let mut s = String::new();
    let _ = write!(
        s,
        "{:08X}-{:04X}-{:04X}-",
        le32(g, 0),
        u16::from_le_bytes([g[4], g[5]]),
        u16::from_le_bytes([g[6], g[7]])
    );
    for (i, b) in g[8..].iter().enumerate() {
        if i == 2 {
            s.push('-');
        }
        let _ = write!(s, "{b:02X}");
    }
    s
}

/// The name of a few partition types worth recognising in a log.
pub fn type_name(g: &[u8; 16]) -> &'static str {
    match guid(g).as_str() {
        "C12A7328-F81F-11D2-BA4B-00A0C93EC93B" => "EFI system",
        "0FC63DAF-8483-4772-8E79-3D69D8477DE4" => "Linux data",
        "EBD0A0A2-B9E5-4433-87C0-68B6B72699C7" => "Microsoft basic data",
        "E3C9E316-0B5C-4DB8-817D-F92DF00215AE" => "Microsoft reserved",
        "DE94BBA4-06D1-4D40-A16A-BFD50179D6AC" => "Windows recovery",
        _ => "other",
    }
}
