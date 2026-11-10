//! Adding LucinaOS's partitions to LUN 0's GUID partition table (UEFI 2.10 section 5.3). Pure: it takes the blocks the disk
//! holds and says which blocks to write, in which order; `lucina-install-disk gpt` does the reading and the writing.
//!
//! The table only grows: the EFI system partition's entry stays byte for byte, and nothing but the free area is given away. So
//! whichever copy of the table the firmware reads after a cut, it finds the same ESP at the same place.
//!
//! The order, so that at least one copy is sound at every moment:
//! 1. the backup array, then the backup header, flush. Until the header is written the backup copy is wrong and the primary
//!    is untouched and sound;
//! 2. the primary array, then the primary header, flush. Between those two the primary copy is wrong and the backup copy is
//!    sound and already new.
//!
//! The partition GUIDs come from the disk's GUID and the partition's name, so the table that `gpt plan` prints is the table that
//! `gpt write` writes, and writing it twice writes nothing the second time.

use lucina_ufs::gpt;
use sha2::{Digest, Sha256};

use crate::{
    disk_rules::{self, Fields},
    install::CONFIG_BLOCK,
};

pub const BLOCK: usize = CONFIG_BLOCK;
const ENTRY_SIZE: usize = 128;

pub const ROOT_A: &str = "lucina-root-a";
pub const ROOT_B: &str = "lucina-root-b";
pub const DATA: &str = "lucina-data";
/// The Discoverable Partitions Specification's root partition for AArch64: B921B045-1DF0-41C3-AF44-4C6F280D3FAE.
const ROOT_TYPE: [u8; 16] = [
    0x45, 0xb0, 0x21, 0xb9, 0xf0, 0x1d, 0xc3, 0x41, 0xaf, 0x44, 0x4c, 0x6f, 0x28, 0x0d, 0x3f, 0xae,
];
/// Linux file system data: 0FC63DAF-8483-4772-8E79-3D69D8477DE4.
const DATA_TYPE: [u8; 16] = [
    0xaf, 0x3d, 0xc6, 0x0f, 0x83, 0x84, 0x72, 0x47, 0x8e, 0x79, 0x3d, 0x69, 0xd8, 0x47, 0x7d, 0xe4,
];
pub const EFI_SYSTEM: &str = "C12A7328-F81F-11D2-BA4B-00A0C93EC93B";

/// What the disk holds in the table's blocks.
pub struct OnDisk<'a> {
    /// The number of logical blocks of the whole unit.
    pub disk_blocks: u64,
    pub primary_header: &'a [u8],
    pub primary_array: &'a [u8],
    pub backup_header: &'a [u8],
    pub backup_array: &'a [u8],
}

/// One partition of the planned table.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Planned {
    pub index: u32,
    pub name: &'static str,
    pub first: u64,
    pub last: u64,
}

/// A write of one block.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BlockWrite {
    pub lba: u64,
    pub data: Vec<u8>,
}

#[derive(Debug)]
/// What to do to the disk. `backup` goes first and is flushed, then `primary`, flushed.
pub struct Plan {
    pub partitions: Vec<Planned>,
    pub backup: Vec<BlockWrite>,
    pub primary: Vec<BlockWrite>,
    /// SHA-256 of the blocks to write, in order, with their addresses: `gpt write` must be told it.
    pub sha256: String,
    pub already: bool,
}

fn le32(d: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(d[at..at + 4].try_into().unwrap())
}

fn le64(d: &[u8], at: usize) -> u64 {
    u64::from_le_bytes(d[at..at + 8].try_into().unwrap())
}

fn header_crc(header: &[u8]) -> u32 {
    let size = le32(header, 12) as usize;
    let mut h = header[..size].to_vec();
    h[16..20].fill(0);
    gpt::crc32(&h)
}

/// The fields `disk_rules` judges. `header` is at least [`BLOCK`] long.
fn fields_of(header: &[u8]) -> Fields {
    Fields {
        header_size: le32(header, 12),
        my_lba: le64(header, 24),
        other_lba: le64(header, 32),
        first_usable: le64(header, 40),
        last_usable: le64(header, 48),
        array_lba: le64(header, 72),
        entries: le32(header, 80),
        entry_size: le32(header, 84),
    }
}

/// One copy, checked on its own: signature, then the fields by the proved rules, then the two CRCs over the spans those rules allow.
fn check_header(what: &str, header: &[u8], array: &[u8], disk_blocks: u64, primary: bool) -> Result<(), String> {
    if header.len() < BLOCK || &header[..8] != b"EFI PART" {
        return Err(format!("the {what} GPT header has no signature"));
    }
    let f = fields_of(header);
    let bytes = disk_rules::check_fields(&f, disk_blocks, primary, array.len() as u64).map_err(|why| {
        format!(
            "the {what} GPT header's fields are refused ({why:?}): it is at LBA {}, its copy at {}, its array at {}, {} entries of {} bytes, usable {}-{}",
            f.my_lba, f.other_lba, f.array_lba, f.entries, f.entry_size, f.first_usable, f.last_usable
        )
    })?;
    if header_crc(header) != le32(header, 16) {
        return Err(format!("the {what} GPT header's CRC is wrong"));
    }
    if gpt::crc32(&array[..bytes as usize]) != le32(header, 88) {
        return Err(format!("the {what} GPT array's CRC is wrong"));
    }
    Ok(())
}

/// Both copies sound and the same table: everything `plan` needs before it may write.
pub fn check_both(d: &OnDisk) -> Result<(), String> {
    check_header("primary", d.primary_header, d.primary_array, d.disk_blocks, true)?;
    check_header("backup", d.backup_header, d.backup_array, d.disk_blocks, false)?;
    let (p, b) = (d.primary_header, d.backup_header);
    if p[40..72] != b[40..72] || p[80..92] != b[80..92] {
        return Err("the two GPT headers disagree on the disk's GUID, its usable area or the array".into());
    }
    let bytes = le32(p, 80) as usize * le32(p, 84) as usize;
    if d.primary_array[..bytes] != d.backup_array[..bytes] {
        return Err("the two GPT arrays differ".into());
    }
    Ok(())
}

/// The last gate of every table write: one whole block each, on the disk, outside the usable area of a sound copy.
fn check_writes<'a>(
    sound_header: &[u8],
    disk_blocks: u64,
    writes: impl Iterator<Item = &'a BlockWrite>,
) -> Result<(), String> {
    let f = fields_of(sound_header);
    for w in writes {
        if w.data.len() != BLOCK || !disk_rules::table_write_allowed(w.lba, disk_blocks, f.first_usable, f.last_usable) {
            return Err(format!(
                "a table write of {} bytes at LBA {} is refused: the usable area is {}-{}",
                w.data.len(),
                w.lba,
                f.first_usable,
                f.last_usable
            ));
        }
    }
    Ok(())
}

/// The partition's GUID, from the disk's and its name: version 5's bits on SHA-256's first bytes.
fn unique_guid(disk_guid: &[u8], name: &str) -> [u8; 16] {
    let mut h = Sha256::new();
    h.update(disk_guid);
    h.update(name.as_bytes());
    let sum = h.finalize();
    let mut g = [0u8; 16];
    g.copy_from_slice(&sum[..16]);
    g[7] = (g[7] & 0x0f) | 0x50;
    g[8] = (g[8] & 0x3f) | 0x80;
    g
}

fn write_entry(slot: &mut [u8], type_guid: &[u8; 16], unique: &[u8; 16], first: u64, last: u64, name: &str) {
    slot.fill(0);
    slot[0..16].copy_from_slice(type_guid);
    slot[16..32].copy_from_slice(unique);
    slot[32..40].copy_from_slice(&first.to_le_bytes());
    slot[40..48].copy_from_slice(&last.to_le_bytes());
    for (i, unit) in name.encode_utf16().enumerate() {
        slot[56 + i * 2..58 + i * 2].copy_from_slice(&unit.to_le_bytes());
    }
}

/// The header for a new array: the old one with the array's CRC and its own replaced.
fn new_header(old: &[u8], array_crc: u32) -> Vec<u8> {
    let mut h = old[..BLOCK].to_vec();
    h[88..92].copy_from_slice(&array_crc.to_le_bytes());
    h[16..20].fill(0);
    let crc = header_crc(&h);
    h[16..20].copy_from_slice(&crc.to_le_bytes());
    h
}

/// The blocks of `new` that differ from `old`, as writes starting at `lba`.
fn changed_blocks(lba: u64, old: &[u8], new: &[u8]) -> Vec<BlockWrite> {
    old.chunks(BLOCK)
        .zip(new.chunks(BLOCK))
        .enumerate()
        .filter(|(_, (o, n))| o != n)
        .map(|(i, (_, n))| BlockWrite {
            lba: lba + i as u64,
            data: n.to_vec(),
        })
        .collect()
}

/// Plans the table: the ESP where it is, then two root partitions of `root_blocks`, then `lucina-data` to the end of the usable
/// area, on 1 MiB boundaries. Refuses unless both copies are sound, and the ESP at `esp` is the only partition or this very
/// table is there already.
pub fn plan(d: &OnDisk, esp: (u64, u64), root_blocks: u64) -> Result<Plan, String> {
    check_both(d)?;
    let header = gpt::parse_header(d.primary_header).ok_or("the primary GPT header does not parse")?;
    let bytes = header.entries as usize * header.entry_size as usize;
    let used = gpt::parse_entries(&header, &d.primary_array[..bytes]);
    let found = used
        .iter()
        .find(|e| gpt::guid(&e.type_guid) == EFI_SYSTEM)
        .ok_or("LUN 0 has no EFI system partition")?;
    if (found.first_lba, found.last_lba) != esp {
        return Err(format!(
            "the EFI system partition is at LBA {}-{}, expected {}-{}",
            found.first_lba, found.last_lba, esp.0, esp.1
        ));
    }
    let l = disk_rules::layout(esp.1, root_blocks, header.first_usable, header.last_usable).ok_or_else(|| {
        format!(
            "the free area ends at LBA {}: no room for two root partitions of {root_blocks} blocks and the data partition on 1 MiB boundaries",
            header.last_usable
        )
    })?;
    if found.index as usize + 3 > bytes / ENTRY_SIZE {
        return Err(format!("the GPT array has no entries left after number {}", found.index));
    }
    let partitions = vec![
        Planned {
            index: found.index + 1,
            name: ROOT_A,
            first: l.a_first,
            last: l.a_last,
        },
        Planned {
            index: found.index + 2,
            name: ROOT_B,
            first: l.b_first,
            last: l.b_last,
        },
        Planned {
            index: found.index + 3,
            name: DATA,
            first: l.data_first,
            last: l.data_last,
        },
    ];
    let mut array = d.primary_array[..bytes].to_vec();
    for p in &partitions {
        let at = (p.index as usize - 1) * ENTRY_SIZE;
        let type_guid = if p.name == DATA { &DATA_TYPE } else { &ROOT_TYPE };
        write_entry(
            &mut array[at..at + ENTRY_SIZE],
            type_guid,
            &unique_guid(&header.disk_guid, p.name),
            p.first,
            p.last,
            p.name,
        );
    }
    let already = array == d.primary_array[..bytes];
    if !already && used.len() != 1 {
        return Err(format!(
            "LUN 0 has {} partitions and they are not this table: nothing is changed",
            used.len()
        ));
    }
    let crc = gpt::crc32(&array);
    let last = d.disk_blocks - 1;
    let (mut backup, mut primary) = (Vec::new(), Vec::new());
    if !already {
        backup = changed_blocks(le64(d.backup_header, 72), &d.backup_array[..bytes], &array);
        backup.push(BlockWrite {
            lba: last,
            data: new_header(d.backup_header, crc),
        });
        primary = changed_blocks(2, &d.primary_array[..bytes], &array);
        primary.push(BlockWrite {
            lba: 1,
            data: new_header(d.primary_header, crc),
        });
    }
    check_writes(d.primary_header, d.disk_blocks, backup.iter().chain(&primary))?;
    let mut sum = Sha256::new();
    for w in backup.iter().chain(&primary) {
        sum.update(w.lba.to_le_bytes());
        sum.update(&w.data);
    }
    let sha256 = crate::stream::hex(&sum.finalize());
    Ok(Plan {
        partitions,
        backup,
        primary,
        sha256,
        already,
    })
}

/// What to write when one copy is damaged and the other is sound: the sound copy's table, restated at the damaged copy's place.
/// Two sound copies that differ are a cut between the backup's header and the primary's array: the backup is the newer, because
/// `gpt write` writes it first. `None` if both are sound and the same. Refuses if neither is.
pub fn repair(d: &OnDisk) -> Result<Option<Vec<BlockWrite>>, String> {
    let last = d
        .disk_blocks
        .checked_sub(1)
        .ok_or("the disk has no blocks")?;
    let primary = check_header("primary", d.primary_header, d.primary_array, d.disk_blocks, true);
    let backup = check_header("backup", d.backup_header, d.backup_array, d.disk_blocks, false);
    let gated = |sound: &[u8], writes: Vec<BlockWrite>| {
        check_writes(sound, d.disk_blocks, writes.iter()).map(|()| Some(writes))
    };
    let restate = |from: &[u8], my: u64, other: u64, array: &[u8], to_array_lba: u64| {
        let mut h = from[..BLOCK].to_vec();
        h[24..32].copy_from_slice(&my.to_le_bytes());
        h[32..40].copy_from_slice(&other.to_le_bytes());
        h[72..80].copy_from_slice(&to_array_lba.to_le_bytes());
        h[16..20].fill(0);
        let crc = header_crc(&h);
        h[16..20].copy_from_slice(&crc.to_le_bytes());
        let bytes = le32(from, 80) as usize * le32(from, 84) as usize;
        let mut writes: Vec<BlockWrite> = array[..bytes]
            .chunks(BLOCK)
            .enumerate()
            .map(|(i, c)| BlockWrite {
                lba: to_array_lba + i as u64,
                data: c.to_vec(),
            })
            .collect();
        writes.push(BlockWrite { lba: my, data: h });
        writes
    };
    match (primary, backup) {
        (Ok(()), Ok(())) if check_both(d).is_ok() => Ok(None),
        (_, Ok(())) => gated(d.backup_header, restate(d.backup_header, 1, last, d.backup_array, 2)),
        (Ok(()), Err(_)) => {
            let bytes = le32(d.primary_header, 80) as u64 * le32(d.primary_header, 84) as u64;
            gated(
                d.primary_header,
                restate(d.primary_header, last, 1, d.primary_array, last - bytes.div_ceil(BLOCK as u64)),
            )
        }
        (Err(a), Err(b)) => Err(format!("both copies are damaged: {a}; {b}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DISK: u64 = 31191040;
    const LAST: u64 = DISK - 1;
    const ESP_TYPE: [u8; 16] = [
        0x28, 0x73, 0x2a, 0xc1, 0x1f, 0xf8, 0xd2, 0x11, 0xba, 0x4b, 0x00, 0xa0, 0xc9, 0x3e, 0xc9, 0x3b,
    ];

    struct Disk {
        ph: Vec<u8>,
        pa: Vec<u8>,
        bh: Vec<u8>,
        ba: Vec<u8>,
    }

    impl Disk {
        fn view(&self) -> OnDisk<'_> {
            OnDisk {
                disk_blocks: DISK,
                primary_header: &self.ph,
                primary_array: &self.pa,
                backup_header: &self.bh,
                backup_array: &self.ba,
            }
        }

        /// Applies writes as the disk would.
        fn apply(&mut self, writes: &[BlockWrite]) {
            for w in writes {
                match w.lba {
                    1 => self.ph = w.data.clone(),
                    LAST => self.bh = w.data.clone(),
                    l @ 2..=5 => self.pa[(l as usize - 2) * BLOCK..][..BLOCK].copy_from_slice(&w.data),
                    l if (LAST - 4..LAST).contains(&l) => {
                        self.ba[(l - (LAST - 4)) as usize * BLOCK..][..BLOCK].copy_from_slice(&w.data)
                    }
                    l => panic!("a write to LBA {l}"),
                }
            }
        }
    }

    fn header(my: u64, other: u64, array_lba: u64, array: &[u8]) -> Vec<u8> {
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
        h[88..92].copy_from_slice(&gpt::crc32(array).to_le_bytes());
        let crc = header_crc(&h);
        h[16..20].copy_from_slice(&crc.to_le_bytes());
        h
    }

    /// LUN 0 as the laptop has it.
    fn laptop() -> Disk {
        let mut a = vec![0u8; 128 * 128];
        write_entry(&mut a[..128], &ESP_TYPE, &[1; 16], 256, 66815, "efi");
        Disk {
            ph: header(1, LAST, 2, &a),
            bh: header(LAST, 1, LAST - 4, &a),
            pa: a.clone(),
            ba: a,
        }
    }

    const ROOT: u64 = 8 << 18; // 8 GiB

    #[test]
    fn the_plan_gives_away_only_the_free_area() {
        let d = laptop();
        let p = plan(&d.view(), (256, 66815), ROOT).unwrap();
        assert!(!p.already);
        let q = |i: usize| (p.partitions[i].index, p.partitions[i].first, p.partitions[i].last);
        assert_eq!(q(0), (2, 66816, 2163967));
        assert_eq!(q(1), (3, 2163968, 4261119));
        assert_eq!(q(2), (4, 4261120, 31190783));
        // Only the array's first block changes, in both copies, and the two headers.
        let lbas = |w: &[BlockWrite]| w.iter().map(|w| w.lba).collect::<Vec<_>>();
        assert_eq!(lbas(&p.backup), [LAST - 4, LAST]);
        assert_eq!(lbas(&p.primary), [2, 1]);
    }

    #[test]
    fn after_the_writes_both_copies_are_sound_and_the_esp_entry_is_the_same_bytes() {
        let mut d = laptop();
        let before = d.pa[..128].to_vec();
        let p = plan(&d.view(), (256, 66815), ROOT).unwrap();
        d.apply(&p.backup);
        d.apply(&p.primary);
        check_both(&d.view()).unwrap();
        assert_eq!(d.pa[..128], before);
        let h = gpt::parse_header(&d.ph).unwrap();
        let names: Vec<_> = gpt::parse_entries(&h, &d.pa).into_iter().map(|e| e.name).collect();
        assert_eq!(names[1..], [ROOT_A, ROOT_B, DATA]);
        // The same table again writes nothing.
        let again = plan(&d.view(), (256, 66815), ROOT).unwrap();
        assert!(again.already);
        assert!(again.backup.is_empty() && again.primary.is_empty());
    }

    #[test]
    fn at_every_cut_one_copy_is_sound_and_names_the_esp() {
        let d0 = laptop();
        let p = plan(&d0.view(), (256, 66815), ROOT).unwrap();
        let writes: Vec<_> = p.backup.iter().chain(&p.primary).cloned().collect();
        for n in 0..=writes.len() {
            let mut d = laptop();
            d.apply(&writes[..n]);
            let v = d.view();
            let primary = check_header("primary", v.primary_header, v.primary_array, DISK, true);
            let backup = check_header("backup", v.backup_header, v.backup_array, DISK, false);
            assert!(primary.is_ok() || backup.is_ok(), "after {n} writes");
            let (h, a) = if primary.is_ok() { (&d.ph, &d.pa) } else { (&d.bh, &d.ba) };
            let hdr = gpt::parse_header(h).unwrap();
            let e = &gpt::parse_entries(&hdr, a)[0];
            assert_eq!((e.first_lba, e.last_lba), (256, 66815), "after {n} writes");
            // repair brings it back to two sound copies that are the same table.
            if let Some(fix) = repair(&d.view()).unwrap() {
                d.apply(&fix);
            }
            check_both(&d.view()).unwrap_or_else(|e| panic!("after {n} writes: {e}"));
        }
    }

    #[test]
    fn it_refuses_what_it_does_not_recognise() {
        let d = laptop();
        assert!(plan(&d.view(), (256, 66816), ROOT).unwrap_err().contains("expected 256-66816"));
        // A partition in the free area: not this table.
        let mut d = laptop();
        write_entry(&mut d.pa[128..256], &[9; 16], &[2; 16], 70000, 70100, "other");
        d.ba = d.pa.clone();
        d.ph = header(1, LAST, 2, &d.pa);
        d.bh = header(LAST, 1, LAST - 4, &d.ba);
        assert!(plan(&d.view(), (256, 66815), ROOT).unwrap_err().contains("not this table"));
        // Damage to either copy stops the plan.
        let mut d = laptop();
        d.bh[100] ^= 1;
        d.bh[40] ^= 1;
        assert!(plan(&d.view(), (256, 66815), ROOT).unwrap_err().contains("backup"));
        let mut d = laptop();
        d.pa[300] ^= 1;
        assert!(plan(&d.view(), (256, 66815), ROOT).unwrap_err().contains("primary"));
        // Copies that disagree.
        let mut d = laptop();
        d.bh[56] ^= 1;
        let crc = {
            d.bh[16..20].fill(0);
            header_crc(&d.bh)
        };
        d.bh[16..20].copy_from_slice(&crc.to_le_bytes());
        assert!(plan(&d.view(), (256, 66815), ROOT).unwrap_err().contains("disagree"));
        // No room.
        let d = laptop();
        assert!(plan(&d.view(), (256, 66815), 64 << 18).unwrap_err().contains("no room"));
    }

    #[test]
    fn repair_needs_one_sound_copy() {
        let mut d = laptop();
        assert!(repair(&d.view()).unwrap().is_none());
        d.ph[40] ^= 1;
        d.bh[40] ^= 1;
        assert!(repair(&d.view()).unwrap_err().contains("both"));
    }
}
