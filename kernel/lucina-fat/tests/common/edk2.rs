//! What UEFI firmware's FAT driver accepts and reads: a small model of edk2 `FatPkg/EnhancedFatDxe`, read-only.
//!
//! The behaviour follows `Init.c` (`FatOpenDevice`), `FileSpace.c` (`FatGetFatEntry`, `FatOFilePosition`) and
//! `DirectoryManage.c` (`FatLoadNextDirEnt`, `FatLoadLongNameEntry`) of edk2 master, read on 2026-11-01. The code here is
//! written for these tests, not copied. It is stricter than the driver in one way: the driver treats an FSInfo free count
//! it believes as the truth, so [`Fw::check_free_hint`] requires that a count the driver would believe is right.
//!
//! Only FAT\[0\] is read, as the driver does: it writes every copy but never reads the others.
#![allow(dead_code)]

use std::{fs::File, os::unix::fs::FileExt, path::Path};

const FAT_MAX_FAT12_CLUSTER: u32 = 0xFF5;
const FAT_MAX_FAT16_CLUSTER: u32 = 0xFFF5;
const FAT_MIN_CLUSTER: u32 = 2;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FwType {
    Fat12,
    Fat16,
    Fat32,
}

pub struct Fw {
    file: File,
    pub fat_type: FwType,
    fat_pos: u64,
    root_pos: u64,
    root_entries: u32,
    root_cluster: u32,
    first_cluster_pos: u64,
    pub max_cluster: u32,
    cluster_size: u64,
    free_info: Option<u32>,
}

fn u16_at(b: &[u8], o: usize) -> u32 {
    u32::from(u16::from_le_bytes([b[o], b[o + 1]]))
}

fn u32_at(b: &[u8], o: usize) -> u32 {
    u32::from_le_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]])
}

/// `FatCheckSum`.
fn checksum(n: &[u8]) -> u8 {
    n[..11]
        .iter()
        .fold(0u8, |s, &c| s.rotate_right(1).wrapping_add(c))
}

pub struct FwEntry {
    pub name: String,
    pub attr: u8,
    pub cluster: u32,
    pub size: u32,
}

impl Fw {
    /// `FatOpenDevice`.
    pub fn open(path: &Path) -> Result<Fw, String> {
        let file = File::open(path).map_err(|e| e.to_string())?;
        let mut b = [0u8; 512];
        file.read_exact_at(&mut b, 0).map_err(|e| e.to_string())?;
        let mut sectors = u16_at(&b, 19);
        if sectors == 0 {
            sectors = u32_at(&b, 32);
        }
        let mut per_fat = u16_at(&b, 22);
        let mut is32 = false;
        if per_fat == 0 {
            per_fat = u32_at(&b, 36);
            is32 = true;
        }
        let (bps, spc, reserved, nfats, root_entries, media) = (
            u16_at(&b, 11),
            u32::from(b[13]),
            u16_at(&b, 14),
            u32::from(b[16]),
            u16_at(&b, 17),
            b[21],
        );
        if reserved == 0 || nfats == 0 || sectors == 0 {
            return Err("reserved, FAT count or sector count is 0".into());
        }
        if !bps.is_power_of_two() || !(512..=4096).contains(&bps) {
            return Err(format!("sector size {bps}"));
        }
        if !spc.is_power_of_two() || spc > 128 {
            return Err(format!("sectors per cluster {spc}"));
        }
        if media <= 0xF7 && media != 0xF0 && media != 0 && media != 1 {
            return Err(format!("media {media:#x}"));
        }
        let mut root_cluster = 0;
        if is32 {
            if per_fat == 0 || u16_at(&b, 42) != 0 || u16_at(&b, 40) & 0x80 != 0 {
                return Err("FAT32 with no FAT, a version or mirroring off".into());
            }
            root_cluster = u32_at(&b, 44);
        } else {
            if root_entries == 0 {
                return Err("no root entries".into());
            }
        }
        let root_entries = if is32 { 0 } else { root_entries };
        let root_dir_sectors = (root_entries * 32).div_ceil(bps);
        let root_lba = reserved + nfats * per_fat;
        let first_lba = root_lba + root_dir_sectors;
        let max_cluster = (sectors - first_lba) / spc;
        let fat_type = if is32 {
            if max_cluster < FAT_MAX_FAT16_CLUSTER {
                return Err(format!("FAT32 with only {max_cluster} clusters"));
            }
            FwType::Fat32
        } else {
            if max_cluster >= FAT_MAX_FAT16_CLUSTER {
                return Err(format!("not FAT32 with {max_cluster} clusters"));
            }
            if max_cluster < FAT_MAX_FAT12_CLUSTER {
                FwType::Fat12
            } else {
                FwType::Fat16
            }
        };
        let bps64 = u64::from(bps);
        let mut free_info = None;
        if is32 && u16_at(&b, 48) != 0 {
            let mut fi = [0u8; 512];
            file.read_exact_at(&mut fi, u64::from(u16_at(&b, 48)) * bps64)
                .map_err(|e| e.to_string())?;
            if u32_at(&fi, 0) == 0x4161_5252
                && u32_at(&fi, 484) == 0x6141_7272
                && u32_at(&fi, 508) == 0xAA55_0000
                && u32_at(&fi, 488) <= max_cluster
            {
                free_info = Some(u32_at(&fi, 488));
            }
        }
        Ok(Fw {
            file,
            fat_type,
            fat_pos: u64::from(reserved) * bps64,
            root_pos: u64::from(root_lba) * bps64,
            root_entries,
            root_cluster,
            first_cluster_pos: u64::from(first_lba) * bps64,
            max_cluster,
            cluster_size: u64::from(spc) * bps64,
            free_info,
        })
    }

    /// `FatGetFatEntry`, with the special values widened to the FAT32 range.
    fn fat(&self, n: u32) -> Result<u32, String> {
        if n > self.max_cluster + 1 {
            return Ok(u32::MAX);
        }
        let mut b = [0u8; 4];
        match self.fat_type {
            FwType::Fat12 => {
                let pos = self.fat_pos + u64::from(n) + u64::from(n) / 2;
                self.file
                    .read_exact_at(&mut b[..2], pos)
                    .map_err(|e| e.to_string())?;
                let w = u16_at(&b, 0);
                let v = if n & 1 == 1 { w >> 4 } else { w & 0xFFF };
                Ok(if v >= 0xFF7 { v | 0x0FFF_F000 } else { v })
            }
            FwType::Fat16 => {
                self.file
                    .read_exact_at(&mut b[..2], self.fat_pos + u64::from(n) * 2)
                    .map_err(|e| e.to_string())?;
                let v = u16_at(&b, 0);
                Ok(if v >= 0xFFF7 { v | 0x0FFF_0000 } else { v })
            }
            FwType::Fat32 => {
                self.file
                    .read_exact_at(&mut b, self.fat_pos + u64::from(n) * 4)
                    .map_err(|e| e.to_string())?;
                Ok(u32_at(&b, 0) & 0x0FFF_FFFF)
            }
        }
    }

    /// Bytes `[0, len)` of a chain, failing like `FatOFilePosition` when the chain does not reach them.
    fn read_chain(&self, first: u32, len: u64) -> Result<Vec<u8>, String> {
        let mut out = vec![0u8; len as usize];
        let mut cluster = first;
        let mut done = 0u64;
        while done < len {
            if !(FAT_MIN_CLUSTER..=self.max_cluster + 1).contains(&cluster) {
                return Err(format!(
                    "the chain from {first} is corrupt at byte {done}: {cluster:#x}"
                ));
            }
            let pos = self.first_cluster_pos + u64::from(cluster - 2) * self.cluster_size;
            let n = self.cluster_size.min(len - done);
            self.file
                .read_exact_at(&mut out[done as usize..(done + n) as usize], pos)
                .map_err(|e| e.to_string())?;
            done += n;
            if done < len {
                cluster = self.fat(cluster)?;
            }
        }
        Ok(out)
    }

    /// Bytes of a directory, as far as the chain goes (`FatAccessEntry` past the end reads a zeroed entry).
    fn dir_bytes(&self, first: u32) -> Result<Vec<u8>, String> {
        if first == 0 {
            let mut out = vec![0u8; self.root_entries as usize * 32];
            self.file
                .read_exact_at(&mut out, self.root_pos)
                .map_err(|e| e.to_string())?;
            return Ok(out);
        }
        let mut out = Vec::new();
        let mut cluster = first;
        let mut steps = 0u32;
        loop {
            if !(FAT_MIN_CLUSTER..=self.max_cluster + 1).contains(&cluster) {
                return Err(format!(
                    "directory chain from {first} corrupt: {cluster:#x}"
                ));
            }
            let mut c = vec![0u8; self.cluster_size as usize];
            let pos = self.first_cluster_pos + u64::from(cluster - 2) * self.cluster_size;
            self.file
                .read_exact_at(&mut c, pos)
                .map_err(|e| e.to_string())?;
            out.extend_from_slice(&c);
            let next = self.fat(cluster)?;
            if next > 0x0FFF_FFF7 {
                return Ok(out);
            }
            cluster = next;
            steps += 1;
            if steps > self.max_cluster {
                return Err("directory chain loops".into());
            }
        }
    }

    /// `FatLoadNextDirEnt` in a loop, with `FatLoadLongNameEntry` for each entry.
    pub fn list(&self, first: u32) -> Result<Vec<FwEntry>, String> {
        let d = self.dir_bytes(first)?;
        let mut v = Vec::new();
        for i in 0..d.len() / 32 {
            let e = &d[i * 32..(i + 1) * 32];
            if e[0] == 0xE5 || e[11] & 0x08 != 0 {
                continue;
            }
            if e[0] == 0 {
                break;
            }
            let hi = if self.fat_type == FwType::Fat32 {
                u16_at(e, 20) << 16
            } else {
                0
            };
            let sum = checksum(e);
            let mut units = Vec::new();
            let mut ord = 1u8;
            let mut pos = i;
            let mut ok = false;
            while pos > 0 && ord <= 20 {
                pos -= 1;
                let l = &d[pos * 32..(pos + 1) * 32];
                if l[11] != 0x0F || u16_at(l, 26) != 0 || l[13] != sum || l[0] & !0x40 != ord {
                    break;
                }
                for o in (1..11)
                    .step_by(2)
                    .chain((14..26).step_by(2))
                    .chain((28..32).step_by(2))
                {
                    units.push(u16::from_le_bytes([l[o], l[o + 1]]));
                }
                ord += 1;
                if l[0] & 0x40 != 0 {
                    ok = true;
                    break;
                }
            }
            let name = if ok {
                let end = units.iter().position(|&u| u == 0).unwrap_or(units.len());
                String::from_utf16_lossy(&units[..end])
            } else {
                short_name(e)
            };
            v.push(FwEntry {
                name,
                attr: e[11],
                cluster: hi | u16_at(e, 26),
                size: u32_at(e, 28),
            });
        }
        Ok(v)
    }

    /// Opens `\a\b\c` (names ignore case) and reads the whole file. `Ok(None)`: no such name.
    pub fn read_file(&self, path: &str) -> Result<Option<Vec<u8>>, String> {
        let parts: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
        let mut dir = self.root_cluster;
        for (i, part) in parts.iter().enumerate() {
            let list = self.list(dir)?;
            let Some(e) = list.iter().find(|e| e.name.eq_ignore_ascii_case(part)) else {
                return Ok(None);
            };
            if i + 1 == parts.len() {
                if e.attr & 0x10 != 0 {
                    return Err(format!("{path} is a directory"));
                }
                return self.read_chain(e.cluster, u64::from(e.size)).map(Some);
            }
            if e.attr & 0x10 == 0 {
                return Err(format!("{part} in {path} is not a directory"));
            }
            dir = e.cluster;
        }
        Err("empty path".into())
    }

    /// A free count the driver would believe must be the real one: `FatGrowEof` answers EFI_VOLUME_FULL from it.
    pub fn check_free_hint(&self) -> Result<(), String> {
        let Some(hint) = self.free_info else {
            return Ok(());
        };
        let mut real = 0u32;
        for c in 2..=self.max_cluster + 1 {
            if self.fat(c)? == 0 {
                real += 1;
            }
        }
        if hint == real {
            Ok(())
        } else {
            Err(format!(
                "FSInfo says {hint} clusters are free, there are {real}"
            ))
        }
    }
}

/// The name of an entry without a long name; the case bits do not matter here since names ignore case.
fn short_name(e: &[u8]) -> String {
    let base: String = e[..8].iter().map(|&c| c as char).collect();
    let ext: String = e[8..11].iter().map(|&c| c as char).collect();
    let (base, ext) = (base.trim_end(), ext.trim_end());
    if ext.is_empty() {
        base.to_string()
    } else {
        format!("{base}.{ext}")
    }
}
