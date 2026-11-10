// SPDX-License-Identifier: BSD-3-Clause
//
// From userspace/crates/lucina-qcom/src/rmtfs.rs, after linux-msm rmtfs's rmtfs.c, storage.c, sharedmem.c and
// qmi_rmtfs.qmi (Copyright (c) 2016, Linaro Ltd).

//! The remote file system ("rmtfs"): the modem keeps its EFS (calibration and settings) on partitions it cannot
//! reach itself, so it asks the host to copy sectors between them and a reserved shared-memory window.
//!
//! Always as Debian's unit runs it, `rmtfs -r -P`: read only, every modem write stays in memory. [`Partitions`] has
//! no write method, so there is nothing that could reach a disk.
//!
//! As in the C, a BYE or DEL_CLIENT closes nothing here: a restarted modem opens its partitions again, gets the same
//! caller ids and finds its writes.
//!
//! Differences from the userspace crate and the C, on purpose:
//! * GET_DEV_ERROR answers for an open caller; the C test is inverted (an open caller gets an error, an unknown one
//!   dereferences NULL);
//! * a read error of the backing store fails the request instead of being taken for a short read;
//! * the whole image is not read into a shadow when opened. Sectors the modem wrote are kept by themselves, and the
//!   rest comes from the partition when asked for, [`CHUNK_SECTORS`] at a time. Reading gives the same bytes: what
//!   was written, else the partition, else zeros past its end. All of that memory together stops at
//!   [`SHADOW_MAX_BYTES`]; beyond it writes fail. The C's per-file limit, [`STORAGE_MAX_SIZE`], stays;
//! * the window is reached by offset from its start, checked before each access;
//! * no `-S` slot suffixes: the Book Go has none.

use alloc::boxed::Box;
use alloc::collections::BTreeMap;
use alloc::format;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

use crate::qmi::{self, Builder, Message};
use crate::{IoError, Log, Partitions, Reply, RmtfsMem};

pub const SERVICE: u32 = 14;
pub const VERSION: u16 = 1;
pub const INSTANCE: u16 = 0;
/// `qrtr_open(RMTFS_QMI_SERVICE)`: the C binds this port rather than an automatic one.
pub const PORT: u32 = 14;

/// `qcom,rmtfs-mem` in the Book Go's device tree.
pub const RMTFS_MEM_PHYS: u64 = 0x9460_0000;
pub const RMTFS_MEM_SIZE: u64 = 0x20_0000;

pub const OPEN: u16 = 1;
pub const CLOSE: u16 = 2;
pub const RW_IOVEC: u16 = 3;
pub const ALLOC_BUFF: u16 = 4;
pub const GET_DEV_ERROR: u16 = 5;
pub const FORCE_SYNC_IND: u16 = 6;

pub const ERR_INTERNAL: u16 = 1;
pub const ERR_MALFORMED: u16 = 2;

pub const SECTOR_SIZE: usize = 512;
pub const MAX_CALLERS: usize = 10;
/// The most a read-only shadow may grow to, per partition (`storage.c`).
pub const STORAGE_MAX_SIZE: u64 = 16 * 1024 * 1024;
/// The most written sectors kept in memory, over all partitions.
pub const SHADOW_MAX_BYTES: usize = 16 * 1024 * 1024;
/// Sectors a read asks of [`Partitions`] and the window at once: one per call would be a block read per 512 bytes.
pub const CHUNK_SECTORS: u64 = 64;
const PATH_MAX: usize = 255;

/// The paths the modem asks for: (requested path, GPT name).
pub const PARTITIONS: [(&str, &str); 10] = [
    ("/boot/modem_fs1", "modemst1"),
    ("/boot/modem_fs2", "modemst2"),
    ("/boot/modem_fsc", "fsc"),
    ("/boot/modem_fsg", "fsg"),
    ("/boot/modem_study", "study"),
    ("/boot/modem_tunning", "tunning"),
    ("/boot/modem_tng", "tunning"),
    ("/oem/nvbk/static", "oem_stanvbk"),
    ("/oem/nvbk/dynamic", "oem_dycnvbk"),
    ("/oppo/oem_partion", "oem_stanvbk"),
];

/// One `iovec_entry`: `num_sector` sectors from `sector_addr` on the storage side, at physical address `phys_offset`
/// in the window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IovecEntry {
    pub sector_addr: u32,
    pub phys_offset: u32,
    pub num_sector: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Request {
    Open {
        path: String,
    },
    Close {
        caller_id: u32,
    },
    Iovec {
        caller_id: u32,
        write: bool,
        entries: Vec<IovecEntry>,
        force_sync: bool,
    },
    AllocBuf {
        caller_id: u32,
        size: u32,
    },
    DevError {
        caller_id: u32,
    },
}

/// Decode a request whose header says `msg_id`.
pub fn decode_request(m: &Message<'_>) -> Result<Request, qmi::Error> {
    Ok(match m.header.msg_id {
        OPEN => {
            m.check_known(&[1])?;
            Request::Open {
                path: m.need_string(1, PATH_MAX)?,
            }
        }
        CLOSE => {
            m.check_known(&[1])?;
            Request::Close {
                caller_id: m.need_u32(1)?,
            }
        }
        RW_IOVEC => {
            m.check_known(&[1, 2, 3, 4])?;
            let v = m.get(3).ok_or(qmi::Error::Missing(3))?;
            let (&n, rest) = v.split_first().ok_or(qmi::Error::BadValue(3))?;
            if rest.len() != usize::from(n) * 12 {
                return Err(qmi::Error::BadValue(3));
            }
            let w = |c: &[u8], i: usize| {
                u32::from_le_bytes([c[4 * i], c[4 * i + 1], c[4 * i + 2], c[4 * i + 3]])
            };
            Request::Iovec {
                caller_id: m.need_u32(1)?,
                write: m.need_u8(2)? != 0,
                entries: rest
                    .as_chunks::<12>()
                    .0
                    .iter()
                    .map(|c| IovecEntry {
                        sector_addr: w(c, 0),
                        phys_offset: w(c, 1),
                        num_sector: w(c, 2),
                    })
                    .collect(),
                force_sync: m.need_u8(4)? != 0,
            }
        }
        ALLOC_BUFF => {
            m.check_known(&[1, 2])?;
            Request::AllocBuf {
                caller_id: m.need_u32(1)?,
                size: m.need_u32(2)?,
            }
        }
        GET_DEV_ERROR => {
            m.check_known(&[1])?;
            Request::DevError {
                caller_id: m.need_u32(1)?,
            }
        }
        _ => return Err(qmi::Error::Unknown(0)),
    })
}

/// A request as the modem sends it (for tests and the C fixtures).
pub fn encode_request(txn: u16, r: &Request) -> Vec<u8> {
    let mut b = Builder::new();
    let id = match r {
        Request::Open { path } => {
            b.tlv(1, path.as_bytes());
            OPEN
        }
        Request::Close { caller_id } => {
            b.u32(1, *caller_id);
            CLOSE
        }
        Request::Iovec {
            caller_id,
            write,
            entries,
            force_sync,
        } => {
            let mut v = vec![entries.len() as u8];
            for e in entries {
                for x in [e.sector_addr, e.phys_offset, e.num_sector] {
                    v.extend_from_slice(&x.to_le_bytes());
                }
            }
            b.u32(1, *caller_id)
                .u8(2, u8::from(*write))
                .tlv(3, &v)
                .u8(4, u8::from(*force_sync));
            RW_IOVEC
        }
        Request::AllocBuf { caller_id, size } => {
            b.u32(1, *caller_id).u32(2, *size);
            ALLOC_BUFF
        }
        Request::DevError { caller_id } => {
            b.u32(1, *caller_id);
            GET_DEV_ERROR
        }
    };
    b.finish(qmi::REQUEST, txn, id)
}

/// The response to one request: result, then the optional value when it succeeded (open: caller id, alloc: buffer
/// address, dev error: status).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Value {
    None,
    CallerId(u32),
    Address(u64),
    Status(u8),
}

pub fn encode_response(txn: u16, msg_id: u16, error: Option<u16>, value: Value) -> Vec<u8> {
    let mut b = Builder::new();
    match error {
        Some(e) => {
            b.result(qmi::RESULT_FAILURE, e);
        }
        None => {
            b.result(qmi::RESULT_SUCCESS, 0);
            match value {
                Value::None => {}
                Value::CallerId(c) => {
                    b.u32(0x10, c);
                }
                Value::Address(a) => {
                    b.u64(0x10, a);
                }
                Value::Status(s) => {
                    b.u8(0x10, s);
                }
            }
        }
    }
    b.finish(qmi::RESPONSE, txn, msg_id)
}

#[derive(Debug)]
struct Open {
    node: u32,
    partition: usize,
    len: u64,
    /// What the modem wrote, by sector. Never leaves memory.
    written: BTreeMap<u64, Box<[u8; SECTOR_SIZE]>>,
    dev_error: u8,
}

/// The rmtfs service. `P`: where images are read from. `M`: the window.
#[derive(Debug)]
pub struct Rmtfs<P, M> {
    pub partitions: P,
    pub mem: M,
    slots: [Option<Open>; MAX_CALLERS],
    /// Bytes in every `written`.
    shadow_bytes: usize,
}

impl<P: Partitions, M: RmtfsMem> Rmtfs<P, M> {
    pub fn new(partitions: P, mem: M) -> Self {
        Rmtfs {
            partitions,
            mem,
            slots: Default::default(),
            shadow_bytes: 0,
        }
    }

    /// Bytes of modem writes held in memory.
    pub fn shadow_bytes(&self) -> usize {
        self.shadow_bytes
    }

    /// Answer one datagram. Nothing goes back to something that is not QMI or a message rmtfs does not know.
    pub fn on_message(
        &mut self,
        src_node: u32,
        src_port: u32,
        payload: &[u8],
        log: &mut Log,
    ) -> Vec<Reply> {
        match self.handle(src_node, payload, log) {
            Ok(payload) => vec![Reply {
                dst_node: src_node,
                dst_port: src_port,
                payload,
            }],
            Err(e) => {
                log.push(&format!("rmtfs: {e}"));
                Vec::new()
            }
        }
    }

    /// The response to a QMI request from `node`; `Err` explains a datagram that gets no answer.
    pub fn handle(&mut self, node: u32, buf: &[u8], log: &mut Log) -> Result<Vec<u8>, String> {
        let m = qmi::parse(buf).map_err(|e| format!("bad QMI packet: {e:?}"))?;
        let h = m.header;
        if !matches!(
            h.msg_id,
            OPEN | CLOSE | RW_IOVEC | ALLOC_BUFF | GET_DEV_ERROR
        ) {
            return Err(format!("unknown request: {}", h.msg_id));
        }
        let req = if h.kind == qmi::REQUEST {
            decode_request(&m)
        } else {
            Err(qmi::Error::Unknown(h.kind))
        };
        let Ok(req) = req else {
            return Ok(encode_response(
                h.txn,
                h.msg_id,
                Some(ERR_MALFORMED),
                Value::None,
            ));
        };
        let (error, value) = match req {
            Request::Open { path } => match self.open(node, &path) {
                Ok(id) => (None, Value::CallerId(id)),
                Err(e) => {
                    log.push(&format!("rmtfs: {e}"));
                    (Some(ERR_INTERNAL), Value::None)
                }
            },
            Request::Close { caller_id } => (
                if self.close(node, caller_id) {
                    None
                } else {
                    Some(ERR_INTERNAL)
                },
                Value::None,
            ),
            Request::Iovec {
                caller_id,
                write,
                entries,
                ..
            } => match self.iovec(node, caller_id, write, &entries) {
                Ok(()) => (None, Value::None),
                Err(e) => {
                    log.push(&format!("rmtfs: {e}"));
                    (Some(ERR_INTERNAL), Value::None)
                }
            },
            Request::AllocBuf { size, .. } => {
                // rmtfs_mem_alloc(): the window's base if it is large enough
                if u64::from(size) <= self.mem.size() {
                    (None, Value::Address(self.mem.phys()))
                } else {
                    log.push(&format!(
                        "rmtfs: shared memory too small for {size:#x} bytes"
                    ));
                    (Some(ERR_INTERNAL), Value::None)
                }
            }
            Request::DevError { caller_id } => match self.slot(node, caller_id) {
                Some(o) => (None, Value::Status(o.dev_error)),
                None => (Some(ERR_INTERNAL), Value::None),
            },
        };
        Ok(encode_response(h.txn, h.msg_id, error, value))
    }

    /// Open `path` for `node`; the caller id, or why not. The same node asking for the same partition again gets
    /// the same id.
    fn open(&mut self, node: u32, path: &str) -> Result<u32, String> {
        let partition = PARTITIONS
            .iter()
            .position(|(p, _)| *p == path)
            .ok_or_else(|| format!("request for unknown partition '{path}', rejecting"))?;
        if let Some(i) = self.slots.iter().position(|s| {
            s.as_ref()
                .is_some_and(|o| o.node == node && o.partition == partition)
        }) {
            return Ok(i as u32);
        }
        let free = self
            .slots
            .iter()
            .position(Option::is_none)
            .ok_or_else(|| String::from("out of free rmtfd handles"))?;
        let name = PARTITIONS[partition].1;
        let len = self
            .partitions
            .partition_len(name)
            .ok_or_else(|| format!("failed to open {path}: no partition named {name}"))?;
        self.slots[free] = Some(Open {
            node,
            partition,
            len,
            written: BTreeMap::new(),
            dev_error: 0,
        });
        Ok(free as u32)
    }

    fn slot(&mut self, node: u32, caller_id: u32) -> Option<&mut Open> {
        self.slots
            .get_mut(usize::try_from(caller_id).ok()?)?
            .as_mut()
            .filter(|o| o.node == node)
    }

    fn close(&mut self, node: u32, caller_id: u32) -> bool {
        let Some(o) = self.slot(node, caller_id) else {
            return false;
        };
        let freed = o.written.len() * SECTOR_SIZE;
        self.slots[caller_id as usize] = None;
        self.shadow_bytes -= freed;
        true
    }

    fn iovec(
        &mut self,
        node: u32,
        caller: u32,
        write: bool,
        entries: &[IovecEntry],
    ) -> Result<(), String> {
        if self.slot(node, caller).is_none() {
            return Err(String::from("iovec request for non-existing caller"));
        }
        for e in entries {
            let mut done = 0u64;
            while done < u64::from(e.num_sector) {
                let sector = u64::from(e.sector_addr) + done;
                let phys = u64::from(e.phys_offset) + done * SECTOR_SIZE as u64;
                let sectors = if write {
                    1
                } else {
                    CHUNK_SECTORS.min(u64::from(e.num_sector) - done)
                };
                let r = if write {
                    self.write_sector(node, caller, sector, phys)
                } else {
                    self.read_sectors(node, caller, sector, sectors, phys)
                };
                r.map_err(|what| {
                    format!(
                        "failed to {} sector {sector}: {what}",
                        if write { "write" } else { "read" },
                    )
                })?;
                done += sectors;
            }
        }
        Ok(())
    }

    /// Where `len` bytes at the physical address `phys` are in the window.
    fn window_offset(&self, phys: u64, len: u64) -> Result<u64, &'static str> {
        let base = self.mem.phys();
        let offset = phys.checked_sub(base).ok_or("outside the window")?;
        if offset.saturating_add(len) > self.mem.size() {
            return Err("outside the window");
        }
        Ok(offset)
    }

    fn read_sectors(
        &mut self,
        node: u32,
        caller: u32,
        sector: u64,
        sectors: u64,
        phys: u64,
    ) -> Result<(), &'static str> {
        let total = sectors as usize * SECTOR_SIZE;
        let window = self.window_offset(phys, total as u64)?;
        let start = sector * SECTOR_SIZE as u64;
        let mut buf = vec![0u8; total];
        let Rmtfs {
            partitions, slots, ..
        } = self;
        let o = slots
            .get(caller as usize)
            .and_then(Option::as_ref)
            .filter(|o| o.node == node)
            .ok_or("no such caller")?;
        if start < o.len {
            // Past the end of the partition reads zeros.
            let n = (o.len - start).min(total as u64) as usize;
            partitions
                .read_at(PARTITIONS[o.partition].1, start, &mut buf[..n])
                .map_err(|IoError| "the partition read failed")?;
        }
        for (s, data) in o.written.range(sector..sector + sectors) {
            let at = (s - sector) as usize * SECTOR_SIZE;
            buf[at..at + SECTOR_SIZE].copy_from_slice(&data[..]);
        }
        self.mem
            .write(window, &buf)
            .map_err(|IoError| "the window write failed")
    }

    /// A modem write: from the window into memory, and no further.
    fn write_sector(
        &mut self,
        node: u32,
        caller: u32,
        sector: u64,
        phys: u64,
    ) -> Result<(), &'static str> {
        let window = self.window_offset(phys, SECTOR_SIZE as u64)?;
        let end = (sector + 1) * SECTOR_SIZE as u64;
        if end >= STORAGE_MAX_SIZE {
            return Err("write exceeds the maximum size");
        }
        let mut data = Box::new([0u8; SECTOR_SIZE]);
        self.mem
            .read(window, &mut data[..])
            .map_err(|IoError| "the window read failed")?;
        let shadow = self.shadow_bytes;
        let o = self.slot(node, caller).ok_or("no such caller")?;
        if !o.written.contains_key(&sector) && shadow + SECTOR_SIZE > SHADOW_MAX_BYTES {
            return Err("out of memory for modem writes");
        }
        if o.written.insert(sector, data).is_none() {
            self.shadow_bytes += SECTOR_SIZE;
        }
        Ok(())
    }
}
