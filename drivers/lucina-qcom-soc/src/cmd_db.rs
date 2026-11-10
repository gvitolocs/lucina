//! The RPMh command DB, Linux 7.1 `drivers/soc/qcom/cmd-db.c` (GPL-2.0-only). Adapted from
//! petitstrawberry/scarlet-project-chromebook `drivers/soc/qcom-cmd-db` (GPL-2.0-only), which runs on a real SC7180;
//! byte reads became word reads through [`RegIo`]. The database sits in the `no-map` `qcom,cmd-db` region (0x80820000
//! here) and the XPU protects it from writes: never map it write-back, and this module never writes.

use crate::io::{RegIo, read_bytes};

pub const MAGIC: [u8; 4] = [0xdb, 0x30, 0x03, 0x0c];
pub const MAX_SLV_ID: u32 = 8;
const RSC_HDR: u32 = 16;
const ENTRY: u32 = 24;
const HEADER: u32 = 8 + MAX_SLV_ID * RSC_HDR + 8;

/// `enum cmd_db_hw_type`: which accelerator serves a resource.
pub const HW_ARC: u32 = 3;
pub const HW_VRM: u32 = 4;
pub const HW_BCM: u32 = 5;

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    /// The resource's address on the RPMh bus (`cmd_db_read_addr`).
    pub addr: u32,
    /// Offset and length of its auxiliary data in the region.
    pub data_offset: u32,
    pub data_len: u32,
}

impl Entry {
    /// `cmd_db_read_slave_id`.
    pub fn slave_id(&self) -> u32 {
        (self.addr >> 16) & 0x7
    }
}

/// One accelerator's header (`struct rsc_hdr`).
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct Slave {
    pub id: u32,
    pub entries: u32,
    header_offset: u32,
    data_offset: u32,
}

pub struct CmdDb<R> {
    mem: R,
    size: u32,
}

impl<R: RegIo> CmdDb<R> {
    /// `cmd_db_dev_probe`: checks the magic.
    pub fn probe(mem: R, size: u32) -> Option<Self> {
        if size < HEADER || mem.read32(4).to_le_bytes() != MAGIC {
            return None;
        }
        Some(Self { mem, size })
    }

    pub fn version(&self) -> u32 {
        self.mem.read32(0)
    }

    /// Header `i`, or `None` past the last (a `slv_id` of 0 ends the list).
    pub fn slave(&self, i: u32) -> Option<Slave> {
        if i >= MAX_SLV_ID {
            return None;
        }
        let at = 8 + i * RSC_HDR;
        let (w0, w1) = (self.mem.read32(at), self.mem.read32(at + 4));
        let id = w0 & 0xffff;
        if id == 0 {
            return None;
        }
        Some(Slave {
            id,
            header_offset: w0 >> 16,
            data_offset: w1 & 0xffff,
            entries: w1 >> 16,
        })
    }

    /// `cmd_db_get_header`: the entry for an id of at most 8 bytes, such as `cx.lvl` or `ldoa9`.
    pub fn find(&self, id: &str) -> Option<Entry> {
        let q = id.as_bytes();
        if q.is_empty() || q.len() > 8 {
            return None;
        }
        let mut key = [0u8; 8];
        key[..q.len()].copy_from_slice(q);
        for i in 0..MAX_SLV_ID {
            let s = self.slave(i)?;
            for j in 0..s.entries {
                let e = HEADER + s.header_offset + j * ENTRY;
                if e.checked_add(ENTRY).is_none_or(|x| x > self.size) {
                    return None;
                }
                let mut name = [0u8; 8];
                read_bytes(&self.mem, e, &mut name);
                if name != key {
                    continue;
                }
                let w = self.mem.read32(e + 20);
                let data_offset = HEADER + s.data_offset + (w >> 16);
                let data_len = w & 0xffff;
                if data_offset + data_len > self.size {
                    return None;
                }
                return Some(Entry {
                    addr: self.mem.read32(e + 16),
                    data_offset,
                    data_len,
                });
            }
        }
        None
    }

    /// Every entry, in table order: the slave header's id, the name without its NUL padding, and the entry. For reports: Linux has
    /// the same walk in `cmd_db_debugfs_dump`. An entry whose name is not text, or whose data lies outside the region, is left out.
    pub fn each(&self, mut f: impl FnMut(u32, &str, Entry)) {
        for i in 0..MAX_SLV_ID {
            let Some(s) = self.slave(i) else {
                return;
            };
            for j in 0..s.entries {
                let e = HEADER + s.header_offset + j * ENTRY;
                if e.checked_add(ENTRY).is_none_or(|x| x > self.size) {
                    return;
                }
                let mut name = [0u8; 8];
                read_bytes(&self.mem, e, &mut name);
                let len = name.iter().position(|&b| b == 0).unwrap_or(name.len());
                let Ok(name) = core::str::from_utf8(&name[..len]) else {
                    continue;
                };
                let w = self.mem.read32(e + 20);
                let data_offset = HEADER + s.data_offset + (w >> 16);
                let data_len = w & 0xffff;
                if data_offset + data_len > self.size {
                    continue;
                }
                f(s.id, name, Entry { addr: self.mem.read32(e + 16), data_offset, data_len });
            }
        }
    }

    /// `cmd_db_read_aux_data`, into `buf`; returns how many bytes it holds.
    pub fn aux(&self, e: Entry, buf: &mut [u8]) -> usize {
        let n = (e.data_len as usize).min(buf.len());
        read_bytes(&self.mem, e.data_offset, &mut buf[..n]);
        n
    }
}
