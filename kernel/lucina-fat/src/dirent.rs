//! Directory entries: 32-byte slots, short entries and the long-name slots that precede them.

use alloc::{string::String, vec, vec::Vec};

use crate::{
    name::checksum,
    time::{self, Time},
};

pub(crate) const SLOT: usize = 32;

pub const ATTR_RO: u8 = 0x01;
pub const ATTR_HIDDEN: u8 = 0x02;
pub const ATTR_SYSTEM: u8 = 0x04;
pub const ATTR_VOLUME: u8 = 0x08;
pub const ATTR_DIR: u8 = 0x10;
pub const ATTR_ARCH: u8 = 0x20;
const ATTR_LFN: u8 = 0x0F;

/// Most slots a directory may have (Linux `FAT_MAX_DIR_ENTRIES`).
pub(crate) const MAX_DIR_SLOTS: usize = 65_536;

/// A directory entry as the file system presents it: the long name (or the decoded short name) and the short
/// entry's fields.
#[derive(Clone, Debug)]
pub struct DirEntry {
    pub name: String,
    pub short: [u8; 11],
    pub attr: u8,
    pub lcase: u8,
    pub first_cluster: u32,
    pub size: u32,
    pub ctime: Time,
    pub mtime: Time,
    pub atime: Time,
    /// Slot index of the first slot of the entry (its first long-name slot, or the short entry).
    pub first_slot: u32,
    /// Slot index of the short entry.
    pub short_slot: u32,
}

impl DirEntry {
    pub fn is_dir(&self) -> bool {
        self.attr & ATTR_DIR != 0
    }

    /// Number of slots the entry occupies.
    pub fn slots(&self) -> u32 {
        self.short_slot - self.first_slot + 1
    }
}

fn le16(b: &[u8], o: usize) -> u16 {
    u16::from_le_bytes([b[o], b[o + 1]])
}

fn le32(b: &[u8], o: usize) -> u32 {
    u32::from_le_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]])
}

pub(crate) fn is_dot_entry(raw: &[u8]) -> bool {
    raw[0] == b'.' && (raw[..11] == *b".          " || raw[..11] == *b"..         ")
}

fn parse_short(b: &[u8], fat32: bool, tz: i64) -> DirEntry {
    let mut short = [0u8; 11];
    short.copy_from_slice(&b[..11]);
    let hi = if fat32 {
        u32::from(le16(b, 20)) << 16
    } else {
        0
    };
    let first_cluster = hi | u32::from(le16(b, 26));
    DirEntry {
        name: String::new(),
        short,
        attr: b[11],
        lcase: b[12],
        first_cluster,
        size: le32(b, 28),
        ctime: time::from_dos(le16(b, 16), le16(b, 14), b[13], tz),
        mtime: time::from_dos(le16(b, 24), le16(b, 22), 0, tz),
        atime: time::from_dos(le16(b, 18), 0, 0, tz),
        first_slot: 0,
        short_slot: 0,
    }
}

/// Reads the entries of a directory image (`data` is a whole number of slots) in order. `f` returns `false` to
/// stop. Deleted slots, the volume label, `.` and `..` and long-name slots without a matching short entry are
/// skipped.
pub(crate) fn scan(data: &[u8], fat32: bool, tz: i64, f: &mut dyn FnMut(DirEntry) -> bool) {
    let mut lfn: Vec<u16> = Vec::new();
    let mut lfn_start = 0u32;
    let mut lfn_sum = 0u8;
    // The sequence number the next long-name slot must have; 0 once the sequence is complete.
    let mut next_seq = 0u8;
    let mut active = false;
    for i in 0..data.len() / SLOT {
        let b = &data[i * SLOT..(i + 1) * SLOT];
        match b[0] {
            0x00 => return,
            0xE5 => {
                active = false;
                continue;
            }
            _ => {}
        }
        if b[11] & 0x3F == ATTR_LFN {
            let seq = b[0];
            let n = seq & 0x3F;
            if seq & 0x40 != 0 {
                if n == 0 || n > 20 {
                    active = false;
                    continue;
                }
                lfn = vec![0xFFFF; usize::from(n) * 13];
                lfn_start = i as u32;
                lfn_sum = b[13];
                active = true;
            } else if !active || n == 0 || n != next_seq || b[13] != lfn_sum {
                active = false;
                continue;
            }
            next_seq = n - 1;
            let base = usize::from(n - 1) * 13;
            for (k, o) in (1..11)
                .step_by(2)
                .chain((14..26).step_by(2))
                .chain((28..32).step_by(2))
                .enumerate()
            {
                lfn[base + k] = le16(b, o);
            }
            continue;
        }
        let complete = active && next_seq == 0;
        let valid_lfn = complete && lfn_sum == checksum(b[..11].try_into().expect("11 bytes"));
        active = false;
        if b[11] & ATTR_VOLUME != 0 || is_dot_entry(b) {
            continue;
        }
        let mut e = parse_short(b, fat32, tz);
        if e.short[0] == 0x05 {
            e.short[0] = 0xE5;
        }
        e.short_slot = i as u32;
        if valid_lfn {
            e.first_slot = lfn_start;
            let end = lfn.iter().position(|&u| u == 0).unwrap_or(lfn.len());
            e.name = char::decode_utf16(lfn[..end].iter().copied())
                .map(|r| r.unwrap_or('\u{fffd}'))
                .collect();
        } else {
            e.first_slot = i as u32;
            e.name = crate::name::decode_short(&e.short, e.lcase);
        }
        if !f(e) {
            return;
        }
    }
}

/// The fields of a short entry that this code writes.
#[derive(Clone, Debug)]
pub(crate) struct ShortFields {
    pub short: [u8; 11],
    pub attr: u8,
    pub lcase: u8,
    pub first_cluster: u32,
    pub size: u32,
    pub ctime: Time,
    pub mtime: Time,
    pub atime: Time,
}

pub(crate) fn short_bytes(f: &ShortFields, fat32: bool, tz: i64) -> [u8; SLOT] {
    let mut b = [0u8; SLOT];
    b[..11].copy_from_slice(&f.short);
    b[11] = f.attr;
    b[12] = f.lcase;
    let (cd, ct, cc) = time::to_dos(f.ctime, tz);
    let (md, mt, _) = time::to_dos(f.mtime, tz);
    let (ad, _, _) = time::to_dos(f.atime, tz);
    b[13] = cc;
    b[14..16].copy_from_slice(&ct.to_le_bytes());
    b[16..18].copy_from_slice(&cd.to_le_bytes());
    b[18..20].copy_from_slice(&ad.to_le_bytes());
    if fat32 {
        b[20..22].copy_from_slice(&((f.first_cluster >> 16) as u16).to_le_bytes());
    }
    b[22..24].copy_from_slice(&mt.to_le_bytes());
    b[24..26].copy_from_slice(&md.to_le_bytes());
    b[26..28].copy_from_slice(&(f.first_cluster as u16).to_le_bytes());
    b[28..32].copy_from_slice(&f.size.to_le_bytes());
    b
}

/// Builds the long-name slots for `units` in on-disk order (last piece first).
pub(crate) fn lfn_slots(units: &[u16], short: &[u8; 11]) -> Vec<u8> {
    let sum = checksum(short);
    let n = units.len().div_ceil(13);
    let mut out = Vec::with_capacity(n * SLOT);
    for i in (1..=n).rev() {
        let mut b = [0u8; SLOT];
        b[0] = i as u8 | if i == n { 0x40 } else { 0 };
        b[11] = ATTR_LFN;
        b[13] = sum;
        let base = (i - 1) * 13;
        for (k, o) in (1..11)
            .step_by(2)
            .chain((14..26).step_by(2))
            .chain((28..32).step_by(2))
            .enumerate()
        {
            let u = match (base + k).cmp(&units.len()) {
                core::cmp::Ordering::Less => units[base + k],
                core::cmp::Ordering::Equal => 0x0000,
                core::cmp::Ordering::Greater => 0xFFFF,
            };
            b[o..o + 2].copy_from_slice(&u.to_le_bytes());
        }
        out.extend_from_slice(&b);
    }
    out
}
