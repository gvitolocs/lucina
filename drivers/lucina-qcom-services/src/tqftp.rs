// SPDX-License-Identifier: BSD-3-Clause
//
// From the tqftpserv half of userspace/crates/lucina-qcom/src/daemon.rs, after linux-msm tqftpserv's tqftpserv.c
// (Copyright (c) 2018, Linaro Ltd.).

//! The TFTP service: requests come to the published port; each transfer then runs on its own router port connected
//! to the client, like a TFTP TID (the C opens a socket per transfer; here [`Router::bind`]).
//!
//! The router may refuse a packet with [`SendError::WouldBlock`] until the modem's RESUME_TX. A packet that was
//! refused waits in its transfer and the rest of the window is not read yet: [`Tftp::pump`] goes on when the flow
//! reopens. So at most one block per transfer sits here, however large the window, and a transfer holds no file.
//!
//! Differences from the userspace daemon:
//! * `/readwrite` is [`RamStore`], where the daemon had a directory on the rescue initramfs's RAM. Same answers: a
//!   write creates a file a later read finds, writes into subdirectories fail. Over its limits a write fails like a
//!   full disk. Nothing reaches a disk: there is no way to do so from here.
//! * at most [`MAX_TRANSFERS`] at once (the C has its file descriptor limit).
//! * an error for a client without a transfer waits like any other packet when its flow is closed, on a throwaway
//!   port that counts as a transfer. When a later ACK of a write overtakes an earlier one still waiting, only the
//!   later one goes: ACKs are cumulative.

use alloc::collections::BTreeMap;
use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

use lucina_qrtr::router::{Router, SendError};

use crate::tftp::{self, AckStep, Op, Reader, Refusal, WriteStep, Writer};
use crate::translate::{self, Target};
use crate::{Files, IoError, Log};

pub const SERVICE: u32 = 4096;
pub const VERSION: u16 = 1;
pub const INSTANCE: u16 = 0;

/// Transfers at once, throwaway error ports included.
pub const MAX_TRANSFERS: usize = 64;
/// Files in [`RamStore`].
pub const RW_MAX_FILES: usize = 32;
/// Bytes in [`RamStore`], all files together; also the most one write window may hold.
pub const RW_MAX_BYTES: usize = 1024 * 1024;

/// The modem's `/readwrite` scratch files, in RAM. Flat, as the daemon's directory was.
#[derive(Debug, Default)]
pub struct RamStore {
    files: BTreeMap<String, Vec<u8>>,
    bytes: usize,
}

impl RamStore {
    pub fn len_of(&self, name: &str) -> Option<u64> {
        self.files.get(name).map(|f| f.len() as u64)
    }

    /// `O_CREAT | O_TRUNC`: false for a name in a subdirectory, or when the store is out of files.
    pub fn create(&mut self, name: &str) -> bool {
        if name.is_empty() || name.contains('/') {
            return false;
        }
        if let Some(f) = self.files.get_mut(name) {
            self.bytes -= f.len();
            f.clear();
            return true;
        }
        if self.files.len() >= RW_MAX_FILES {
            return false;
        }
        self.files.insert(name.to_string(), Vec::new());
        true
    }

    /// False when the store is full; then nothing was added.
    pub fn append(&mut self, name: &str, data: &[u8]) -> bool {
        let Some(f) = self.files.get_mut(name) else {
            return false;
        };
        if self.bytes.saturating_add(data.len()) > RW_MAX_BYTES {
            return false;
        }
        f.extend_from_slice(data);
        self.bytes += data.len();
        true
    }

    pub fn read_at(&self, name: &str, offset: u64, buf: &mut [u8]) -> Result<usize, IoError> {
        let f = self.files.get(name).ok_or(IoError)?;
        let Some(rest) = usize::try_from(offset).ok().and_then(|o| f.get(o..)) else {
            return Ok(0);
        };
        let n = buf.len().min(rest.len());
        buf[..n].copy_from_slice(&rest[..n]);
        Ok(n)
    }
}

#[derive(Debug)]
enum Source {
    Files(String),
    Ram(String),
}

#[derive(Debug)]
enum Kind {
    Read(Reader, Source),
    /// With the name in [`RamStore`].
    Write(Writer, String),
    /// A throwaway port for one ERROR.
    Error,
}

#[derive(Debug)]
struct Transfer {
    peer: (u32, u32),
    name: String,
    kind: Kind,
    /// A packet the router has not taken yet.
    pending: Option<Vec<u8>>,
    /// Close once `pending` is out.
    closing: bool,
}

/// See the [module documentation](self).
#[derive(Debug, Default)]
pub struct Tftp {
    transfers: BTreeMap<u32, Transfer>,
    ram: RamStore,
}

impl Tftp {
    pub fn new() -> Tftp {
        Tftp::default()
    }

    /// Whether `port` is a transfer's.
    pub fn owns_port(&self, port: u32) -> bool {
        self.transfers.contains_key(&port)
    }

    pub fn transfer_ports(&self) -> impl Iterator<Item = u32> + '_ {
        self.transfers.keys().copied()
    }

    /// Whether a transfer talks to `(node, port)`.
    pub fn has_peer(&self, node: u32, port: u32) -> bool {
        self.transfers.values().any(|t| t.peer == (node, port))
    }

    pub fn ram(&self) -> &RamStore {
        &self.ram
    }

    /// A datagram on the published port: the service socket of the C's main loop.
    pub fn on_service_message<F: Files>(
        &mut self,
        router: &mut Router,
        files: &mut F,
        log: &mut Log,
        from: (u32, u32),
        buf: &[u8],
    ) {
        match tftp::opcode(buf) {
            Some(tftp::RRQ | tftp::WRQ) => self.start(router, files, log, from, buf),
            Some(tftp::ERROR) if buf.len() >= 4 => {
                let msg = String::from_utf8_lossy(&buf[4..]);
                log.push(&format!(
                    "tqftpserv: error {} from {}:{}: {}",
                    tftp::block_of(buf).unwrap_or(0),
                    from.0,
                    from.1,
                    msg.trim_end_matches('\0')
                ));
            }
            Some(op) => log.push(&format!(
                "tqftpserv: unhandled op {op} from {}:{}",
                from.0, from.1
            )),
            None => {}
        }
    }

    fn start<F: Files>(
        &mut self,
        router: &mut Router,
        files: &mut F,
        log: &mut Log,
        from: (u32, u32),
        buf: &[u8],
    ) {
        let req = match tftp::parse_request(buf) {
            Ok(r) => r,
            Err(Refusal::Malformed) => {
                log.push(&format!(
                    "tqftpserv: malformed request from {}:{}",
                    from.0, from.1
                ));
                return;
            }
            Err(Refusal::Error(code, msg)) => {
                log.push(&format!(
                    "tqftpserv: refused a request from {}:{}: {msg}",
                    from.0, from.1
                ));
                self.send_error_to(router, files, log, from, tftp::error(code, msg));
                return;
            }
        };
        if self.transfers.len() >= MAX_TRANSFERS {
            log.push(&format!(
                "tqftpserv: too many transfers, dropping {}",
                req.filename
            ));
            return;
        }
        let target = translate::classify(&req.filename);
        let mut opts = req.opts;
        let kind = match (req.op, target) {
            (Op::Read, Some(Target::ReadOnly(f))) => {
                files.file_len(&f).map(|len| (Source::Files(f), len))
            }
            (Op::Read, Some(Target::ReadWrite(f))) => {
                self.ram.len_of(&f).map(|len| (Source::Ram(f), len))
            }
            (Op::Read, None) => None,
            (Op::Write, Some(Target::ReadWrite(f))) => {
                if self.ram.create(&f) {
                    let writer = Writer::new(opts, RW_MAX_BYTES);
                    let Ok(port) = router.bind(0) else {
                        log.push("tqftpserv: no free port for a transfer");
                        return;
                    };
                    let first = if req.has_options {
                        tftp::oack(&opts)
                    } else {
                        tftp::ack(0).to_vec()
                    };
                    log.push(&format!(
                        "tqftpserv: {} opened for writing from {}:{}",
                        req.filename, from.0, from.1
                    ));
                    self.insert(
                        router,
                        files,
                        log,
                        port,
                        Transfer {
                            peer: from,
                            name: req.filename,
                            kind: Kind::Write(writer, f),
                            pending: Some(first),
                            closing: false,
                        },
                    );
                    return;
                }
                None
            }
            (Op::Write, _) => None,
        };
        let Some((source, len)) = kind else {
            log.push(&format!(
                "tqftpserv: unable to open {}, reject",
                req.filename
            ));
            let pkt = match req.op {
                Op::Read => tftp::error(tftp::ERR_ENOENT, "file not found"),
                Op::Write => tftp::error(tftp::ERR_EACCESS, "Access violation"),
            };
            self.send_error_to(router, files, log, from, pkt);
            return;
        };
        let Ok(port) = router.bind(0) else {
            log.push("tqftpserv: no free port for a transfer");
            return;
        };
        if opts.tsize.is_some() {
            opts.tsize = Some(len);
        }
        let mut reader = Reader::new(opts);
        let pending = if req.has_options {
            Some(tftp::oack(&opts))
        } else {
            reader.start_first();
            None
        };
        log.push(&format!(
            "tqftpserv: {} opened for reading from {}:{}",
            req.filename, from.0, from.1
        ));
        self.insert(
            router,
            files,
            log,
            port,
            Transfer {
                peer: from,
                name: req.filename,
                kind: Kind::Read(reader, source),
                pending,
                closing: false,
            },
        );
    }

    /// An ERROR from a throwaway port connected to the client (`tftp_send_error_to()`).
    fn send_error_to<F: Files>(
        &mut self,
        router: &mut Router,
        files: &mut F,
        log: &mut Log,
        to: (u32, u32),
        pkt: Vec<u8>,
    ) {
        if self.transfers.len() >= MAX_TRANSFERS {
            return;
        }
        let Ok(port) = router.bind(0) else {
            return;
        };
        self.insert(
            router,
            files,
            log,
            port,
            Transfer {
                peer: to,
                name: String::new(),
                kind: Kind::Error,
                pending: Some(pkt),
                closing: true,
            },
        );
    }

    fn insert<F: Files>(
        &mut self,
        router: &mut Router,
        files: &mut F,
        log: &mut Log,
        port: u32,
        t: Transfer,
    ) {
        self.transfers.insert(port, t);
        self.pump_one(router, files, log, port);
    }

    /// A datagram on a transfer's port (`step()` of the daemon). Foreign senders are ignored.
    pub fn on_transfer_message<F: Files>(
        &mut self,
        router: &mut Router,
        files: &mut F,
        log: &mut Log,
        port: u32,
        from: (u32, u32),
        pkt: &[u8],
    ) {
        let Some(t) = self.transfers.get_mut(&port) else {
            return;
        };
        if from != t.peer {
            return;
        }
        match &mut t.kind {
            Kind::Read(reader, _) => {
                if pkt.len() < 4 {
                    t.pending = None;
                    t.closing = true;
                } else {
                    match tftp::opcode(pkt) {
                        Some(tftp::ERROR) => {
                            let code = tftp::block_of(pkt).unwrap_or(0);
                            if code != tftp::ERR_END_OF_TRANSFER {
                                let msg = String::from_utf8_lossy(&pkt[4..]);
                                log.push(&format!(
                                    "tqftpserv: {}: remote error {code}: {}",
                                    t.name,
                                    msg.trim_end_matches('\0')
                                ));
                            }
                            t.pending = None;
                            t.closing = true;
                        }
                        Some(tftp::ACK) => {
                            if reader.on_ack(tftp::block_of(pkt).unwrap_or(0)) == AckStep::Done {
                                t.pending = None;
                                t.closing = true;
                            }
                        }
                        other => {
                            log.push(&format!(
                                "tqftpserv: {}: expected ACK, got {other:?}",
                                t.name
                            ));
                            t.pending = Some(tftp::error(tftp::ERR_EBADOP, "Expected ACK opcode"));
                            t.closing = true;
                        }
                    }
                }
            }
            Kind::Write(writer, name) => match writer.on_packet(pkt) {
                WriteStep::More => {}
                WriteStep::Flush { bytes, ack, done } => {
                    if self.ram.append(name, &bytes) {
                        t.pending = Some(ack.to_vec());
                        t.closing = done;
                    } else {
                        log.push(&format!("tqftpserv: {}: failed to write data", t.name));
                        t.pending =
                            Some(tftp::error(tftp::ERR_ENOSPACE, "Disk full or write error"));
                        t.closing = true;
                    }
                }
                WriteStep::Abort(err) => {
                    t.pending = Some(err);
                    t.closing = true;
                }
            },
            Kind::Error => {}
        }
        self.pump_one(router, files, log, port);
    }

    /// DEL_CLIENT: the transfers of that client end.
    pub fn on_client_gone(&mut self, router: &mut Router, node: u32, port: u32) {
        self.drop_where(router, |t| t.peer == (node, port));
    }

    /// BYE: the transfers of that node end.
    pub fn on_bye(&mut self, router: &mut Router, node: u32) {
        self.drop_where(router, |t| t.peer.0 == node);
    }

    fn drop_where(&mut self, router: &mut Router, gone: impl Fn(&Transfer) -> bool) {
        let ports: Vec<u32> = self
            .transfers
            .iter()
            .filter(|(_, t)| gone(t))
            .map(|(p, _)| *p)
            .collect();
        for p in ports {
            self.transfers.remove(&p);
            router.close(p);
        }
    }

    /// Send what the flow control allows, on every transfer.
    pub fn pump<F: Files>(&mut self, router: &mut Router, files: &mut F, log: &mut Log) {
        let ports: Vec<u32> = self.transfers.keys().copied().collect();
        for p in ports {
            self.pump_one(router, files, log, p);
        }
    }

    fn pump_one<F: Files>(&mut self, router: &mut Router, files: &mut F, log: &mut Log, port: u32) {
        let Some(t) = self.transfers.get_mut(&port) else {
            return;
        };
        let keep = loop {
            if t.pending.is_none() && !t.closing {
                t.pending = next_packet(t, files, &self.ram, log);
            }
            let Some(p) = t.pending.as_ref() else {
                break !t.closing;
            };
            match router.send(port, t.peer.0, t.peer.1, p) {
                Ok(()) => t.pending = None,
                Err(SendError::WouldBlock) => break true,
                Err(e) => {
                    log.push(&format!("tqftpserv: {}: send failed: {e:?}", t.name));
                    break false;
                }
            }
        };
        if !keep {
            self.transfers.remove(&port);
            router.close(port);
        }
    }
}

/// The next block of a read; a failed read becomes the ERROR that ends the transfer.
fn next_packet<F: Files>(
    t: &mut Transfer,
    files: &mut F,
    ram: &RamStore,
    log: &mut Log,
) -> Option<Vec<u8>> {
    let Kind::Read(reader, source) = &mut t.kind else {
        return None;
    };
    let read = reader.next_data(|offset, buf| match &*source {
        Source::Files(name) => files.read_at(name, offset, buf),
        Source::Ram(name) => ram.read_at(name, offset, buf),
    });
    match read {
        Ok(p) => p,
        Err(IoError) => {
            log.push(&format!("tqftpserv: {}: read failed", t.name));
            t.closing = true;
            Some(tftp::error(tftp::ERR_UNDEF, "Read error"))
        }
    }
}
