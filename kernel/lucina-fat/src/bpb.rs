//! The boot sector (BIOS parameter block): parsing, validation and the geometry derived from it.
//!
//! Behaviour follows Linux `fat_read_bpb` and the Microsoft FAT specification (fatgen103). The FAT width is decided
//! like Linux does: a BPB with a 32-bit FAT size and no fixed root directory is FAT32; otherwise the cluster count
//! picks FAT12 (at most 4084 clusters) or FAT16.

use crate::error::{Error, Result};

/// Width of a FAT entry.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FatType {
    Fat12,
    Fat16,
    Fat32,
}

impl FatType {
    pub fn bits(self) -> u32 {
        match self {
            FatType::Fat12 => 12,
            FatType::Fat16 => 16,
            FatType::Fat32 => 32,
        }
    }
}

/// Largest FAT12 cluster count (Linux `MAX_FAT12`).
const MAX_FAT12: u32 = 0xFF4;
/// Largest FAT16 cluster count (Linux `MAX_FAT16`).
const MAX_FAT16: u32 = 0xFFF4;
/// Largest FAT32 cluster count (Linux `MAX_FAT32`).
const MAX_FAT32: u32 = 0x0FFF_FFF6;

/// Everything the file system code needs to know about the layout of a volume.
#[derive(Clone, Debug)]
pub struct Geometry {
    pub fat_type: FatType,
    pub bytes_per_sector: u32,
    pub sectors_per_cluster: u32,
    pub cluster_size: u32,
    pub num_fats: u32,
    /// Sectors per FAT copy.
    pub fat_sectors: u32,
    pub root_entries: u32,
    pub total_sectors: u64,
    /// Number of data clusters (clusters 2 ..= `max_cluster`).
    pub clusters: u32,
    /// Highest valid cluster number.
    pub max_cluster: u32,
    /// Byte offset of the first FAT copy.
    pub fat_start: u64,
    /// Size of one FAT copy in bytes.
    pub fat_bytes: u64,
    /// Byte offset of the fixed root directory (FAT12/16) or of where it would be.
    pub root_start: u64,
    /// Size of the fixed root directory in bytes (0 on FAT32).
    pub root_bytes: u32,
    /// Byte offset of cluster 2.
    pub data_start: u64,
    /// FAT32 root directory cluster (0 on FAT12/16).
    pub root_cluster: u32,
    /// FAT32 FSInfo sector number (0 = none).
    pub fsinfo_sector: u32,
    /// FAT copy that holds the live table (FAT32 with mirroring disabled), else 0.
    pub active_fat: u32,
    /// Whether all FAT copies are meant to be equal (FAT32 can turn that off).
    pub mirrored: bool,
    /// Byte offset of the "volume dirty" state byte in the boot sector.
    pub state_offset: u64,
    /// The byte at `state_offset` when the volume was mounted.
    pub state_at_mount: u8,
    pub volume_id: u32,
}

fn u16_at(b: &[u8], o: usize) -> u32 {
    u32::from(u16::from_le_bytes([b[o], b[o + 1]]))
}

fn u32_at(b: &[u8], o: usize) -> u32 {
    u32::from_le_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]])
}

impl Geometry {
    /// Parses and validates a boot sector (the first `bytes_per_sector` bytes of the volume, at least 512 of them).
    /// `device_size` is the size of the volume's device; a volume that claims to be larger is rejected.
    pub fn parse(b: &[u8], device_size: u64) -> Result<Geometry> {
        if b.len() < 512 {
            return Err(Error::Unsupported);
        }
        let bytes_per_sector = u16_at(b, 11);
        if !matches!(bytes_per_sector, 512 | 1024 | 2048 | 4096) {
            return Err(Error::Unsupported);
        }
        // The signature is at byte 510; on larger sectors some formatters also or only put it at the sector's end.
        let end = bytes_per_sector as usize;
        let at_end = b.len() >= end && b[end - 2] == 0x55 && b[end - 1] == 0xAA;
        if (b[510] != 0x55 || b[511] != 0xAA) && !at_end {
            return Err(Error::Unsupported);
        }
        let spc = u32::from(b[13]);
        if spc == 0 || !spc.is_power_of_two() || spc > 128 {
            return Err(Error::Unsupported);
        }
        let reserved = u16_at(b, 14);
        let num_fats = u32::from(b[16]);
        let root_entries = u16_at(b, 17);
        let total16 = u16_at(b, 19);
        let media = b[21];
        let fat16_sectors = u16_at(b, 22);
        let total32 = u32_at(b, 32);
        if reserved == 0 || num_fats == 0 || num_fats > 8 {
            return Err(Error::Unsupported);
        }
        // Linux (fat_valid_media): 0xF8..=0xFF and 0xF0.
        if media != 0xF0 && media < 0xF8 {
            return Err(Error::Unsupported);
        }
        let total_sectors = if total16 != 0 {
            u64::from(total16)
        } else {
            u64::from(total32)
        };
        let is_fat32_layout = fat16_sectors == 0;
        let (fat_sectors, root_cluster, fsinfo_sector, ext_flags) = if is_fat32_layout {
            if root_entries != 0 {
                return Err(Error::Unsupported);
            }
            // Version 0.0 is the only one defined.
            if u16_at(b, 42) != 0 {
                return Err(Error::Unsupported);
            }
            (u32_at(b, 36), u32_at(b, 44), u16_at(b, 48), u16_at(b, 40))
        } else {
            if root_entries == 0 {
                return Err(Error::Unsupported);
            }
            (fat16_sectors, 0, 0, 0)
        };
        if fat_sectors == 0 || total_sectors == 0 {
            return Err(Error::Unsupported);
        }
        let bps = u64::from(bytes_per_sector);
        let root_bytes = root_entries * 32;
        let root_sectors = u64::from(root_bytes).div_ceil(bps);
        let meta_sectors =
            u64::from(reserved) + u64::from(num_fats) * u64::from(fat_sectors) + root_sectors;
        if meta_sectors >= total_sectors {
            return Err(Error::Unsupported);
        }
        if total_sectors * bps > device_size {
            return Err(Error::Unsupported);
        }
        let mut clusters =
            ((total_sectors - meta_sectors) / u64::from(spc)).min(u64::from(u32::MAX)) as u32;
        let fat_type = if is_fat32_layout {
            FatType::Fat32
        } else if clusters > MAX_FAT12 {
            FatType::Fat16
        } else {
            FatType::Fat12
        };
        let max_clusters = match fat_type {
            FatType::Fat12 => MAX_FAT12,
            FatType::Fat16 => MAX_FAT16,
            FatType::Fat32 => MAX_FAT32,
        };
        clusters = clusters.min(max_clusters);
        // The FAT has to have room for every cluster (entries 0 and 1 are reserved).
        let fat_bytes = u64::from(fat_sectors) * bps;
        let fat_entries = fat_bytes * 8 / u64::from(fat_type.bits());
        if fat_entries < 2 {
            return Err(Error::Unsupported);
        }
        clusters = clusters.min((fat_entries - 2).min(u64::from(u32::MAX)) as u32);
        if clusters == 0 {
            return Err(Error::Unsupported);
        }
        let max_cluster = clusters + 1;
        if is_fat32_layout && (root_cluster < 2 || root_cluster > max_cluster) {
            return Err(Error::Unsupported);
        }
        let fat_start = u64::from(reserved) * bps;
        let root_start = fat_start + u64::from(num_fats) * fat_bytes;
        let data_start = root_start + root_sectors * bps;
        // ExtFlags bit 7: mirroring disabled, bits 0..=3 name the live FAT.
        let active_fat = if fat_type == FatType::Fat32 && ext_flags & 0x80 != 0 {
            let n = ext_flags & 0xF;
            if n >= num_fats {
                return Err(Error::Unsupported);
            }
            n
        } else {
            0
        };
        let (state_offset, volume_id) = if is_fat32_layout {
            (0x41, u32_at(b, 67))
        } else {
            (0x25, u32_at(b, 39))
        };
        Ok(Geometry {
            fat_type,
            bytes_per_sector,
            sectors_per_cluster: spc,
            cluster_size: spc * bytes_per_sector,
            num_fats,
            fat_sectors,
            root_entries,
            total_sectors,
            clusters,
            max_cluster,
            fat_start,
            fat_bytes,
            root_start,
            root_bytes,
            data_start,
            root_cluster,
            fsinfo_sector,
            active_fat,
            mirrored: !(fat_type == FatType::Fat32 && ext_flags & 0x80 != 0),
            state_offset,
            state_at_mount: b[state_offset as usize],
            volume_id,
        })
    }

    /// Byte offset of the first byte of data cluster `n` (`2..=max_cluster`).
    pub fn cluster_offset(&self, n: u32) -> u64 {
        self.data_start + u64::from(n - 2) * u64::from(self.cluster_size)
    }

    /// The value that ends a chain, as written by this implementation.
    pub fn eoc(&self) -> u32 {
        match self.fat_type {
            FatType::Fat12 => 0xFFF,
            FatType::Fat16 => 0xFFFF,
            FatType::Fat32 => 0x0FFF_FFFF,
        }
    }

    /// Whether `v` (a FAT entry) is an end-of-chain mark.
    pub fn is_eoc(&self, v: u32) -> bool {
        match self.fat_type {
            FatType::Fat12 => v >= 0xFF8,
            FatType::Fat16 => v >= 0xFFF8,
            FatType::Fat32 => v >= 0x0FFF_FFF8,
        }
    }

    /// Whether `v` is the bad-cluster mark.
    pub fn is_bad(&self, v: u32) -> bool {
        match self.fat_type {
            FatType::Fat12 => v == 0xFF7,
            FatType::Fat16 => v == 0xFFF7,
            FatType::Fat32 => v == 0x0FFF_FFF7,
        }
    }
}
