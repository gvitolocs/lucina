// SPDX-License-Identifier: GPL-2.0-only
//! The memory the host and the firmware share: a table of three queues (commands to the firmware, messages and debug text back) and
//! the firmware's last words before a crash (SFR). `venus_interface_queues_init`, `venus_write_queue` and `venus_read_queue` of Linux's
//! hfi_venus.c; where a packet goes is [`rules::write_span`] and [`rules::read_span`], proved with Verus.
//!
//! One difference in layout: Linux allocates the queues (2 461 696 bytes) and the SFR (4 KiB) apart, then names 3 MiB from the queues'
//! start as the uncached region (`UC_REGION_SIZE = SHARED_QSIZE`). Here the 3 MiB are allocated whole and the SFR sits inside them,
//! after the queues, so that nothing in the region the firmware is told about belongs to anyone else: there is no SMMU here to leave
//! the rest unmapped.

use crate::rules::{self, Found, MAX_PACKET_WORDS, QUEUE_WORDS, QueueError};

/// Memory the firmware reads and writes, uncached, by byte offset from its start.
pub trait Shared {
    fn read32(&self, offset: u32) -> u32;
    fn write32(&self, offset: u32, value: u32);
    /// `dsb sy`: what was written reaches memory before what follows, and the other way around (Linux's `wmb`, `rmb` and `mb`).
    fn barrier(&self);
}

/// `IFACEQ_NUM`, and the queues in table order.
pub const QUEUES: u32 = 3;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Queue {
    Command = 0,
    Message = 1,
    Debug = 2,
}

/// `struct hfi_queue_table_header`: six words.
const TABLE_HEADER: u32 = 6 * 4;
/// `struct hfi_queue_header`: fourteen words.
const QUEUE_HEADER: u32 = 14 * 4;
/// `IFACEQ_TABLE_SIZE`.
pub const TABLE_SIZE: u32 = TABLE_HEADER + QUEUE_HEADER * QUEUES;
/// `IFACEQ_QUEUE_SIZE` in bytes.
pub const QUEUE_BYTES: u32 = QUEUE_WORDS * 4;
/// `ALIGNED_QUEUE_SIZE`.
pub const ALIGNED_QUEUE_SIZE: u32 = (TABLE_SIZE + QUEUE_BYTES * QUEUES).next_multiple_of(4096);
/// `ALIGNED_SFR_SIZE`.
pub const SFR_SIZE: u32 = 4096;
/// `SHARED_QSIZE`: `ALIGN(ALIGNED_SFR_SIZE + ALIGNED_QUEUE_SIZE + ALIGNED_QDSS_SIZE, SZ_1M)`, what `UC_REGION_SIZE` is told.
pub const SHARED_SIZE: u32 = (SFR_SIZE + ALIGNED_QUEUE_SIZE + 4096).next_multiple_of(1 << 20);
/// Where the SFR sits in the region (see the module's note).
pub const SFR_OFFSET: u32 = ALIGNED_QUEUE_SIZE;

/// `IFACEQ_DFLT_QHDR`.
const DEFAULT_TYPE: u32 = 0x0101_0000;

/// Word offsets in a queue header.
mod qhdr {
    pub const STATUS: u32 = 0;
    pub const START_ADDR: u32 = 1;
    pub const TYPE: u32 = 2;
    pub const Q_SIZE: u32 = 3;
    pub const PKT_SIZE: u32 = 4;
    pub const PKT_DROP_CNT: u32 = 5;
    pub const RX_WM: u32 = 6;
    pub const TX_WM: u32 = 7;
    pub const RX_REQ: u32 = 8;
    pub const TX_REQ: u32 = 9;
    pub const RX_IRQ_STATUS: u32 = 10;
    pub const TX_IRQ_STATUS: u32 = 11;
    pub const READ_IDX: u32 = 12;
    pub const WRITE_IDX: u32 = 13;
}

fn header(q: Queue, word: u32) -> u32 {
    TABLE_HEADER + QUEUE_HEADER * q as u32 + word * 4
}

fn ring(q: Queue) -> u32 {
    TABLE_SIZE + QUEUE_BYTES * q as u32
}

/// The shared region, `SHARED_SIZE` bytes of [`Shared`] memory whose device address is `device`.
pub struct Iface<S> {
    mem: S,
    device: u32,
}

impl<S: Shared> Iface<S> {
    /// `venus_interface_queues_init`: the table, each queue's header with Linux's defaults, the debug queue asking for no interrupt, and
    /// the SFR's size. The memory must be zeroed.
    pub fn init(mem: S, device: u32) -> Self {
        for q in [Queue::Command, Queue::Message, Queue::Debug] {
            // venus_set_qhdr_defaults, then the address and the queue's type.
            mem.write32(header(q, qhdr::STATUS), 1);
            mem.write32(header(q, qhdr::TYPE), DEFAULT_TYPE | q as u32);
            mem.write32(header(q, qhdr::Q_SIZE), QUEUE_WORDS);
            mem.write32(header(q, qhdr::PKT_SIZE), 0);
            mem.write32(header(q, qhdr::PKT_DROP_CNT), 0);
            mem.write32(header(q, qhdr::RX_WM), 1);
            mem.write32(header(q, qhdr::TX_WM), 1);
            mem.write32(header(q, qhdr::RX_REQ), if q == Queue::Debug { 0 } else { 1 });
            mem.write32(header(q, qhdr::TX_REQ), 0);
            mem.write32(header(q, qhdr::RX_IRQ_STATUS), 0);
            mem.write32(header(q, qhdr::TX_IRQ_STATUS), 0);
            mem.write32(header(q, qhdr::READ_IDX), 0);
            mem.write32(header(q, qhdr::WRITE_IDX), 0);
            mem.write32(header(q, qhdr::START_ADDR), device + ring(q));
        }
        mem.write32(0, 0);
        mem.write32(4, TABLE_SIZE);
        mem.write32(8, TABLE_HEADER);
        mem.write32(12, QUEUE_HEADER);
        mem.write32(16, QUEUES);
        mem.write32(20, QUEUES);
        // struct hfi_sfr: buf_size, then the text.
        mem.write32(SFR_OFFSET, SFR_SIZE);
        mem.barrier();
        Self { mem, device }
    }

    /// The device address of the queue table: `UC_REGION_ADDR` and `CPU_CS_SCIACMDARG2`.
    pub fn table_address(&self) -> u32 {
        self.device
    }

    /// The device address of the SFR: `SFR_ADDR`.
    pub fn sfr_address(&self) -> u32 {
        self.device + SFR_OFFSET
    }

    /// `venus_write_queue` for the command queue: `Ok(true)` when the firmware asked to be woken (`rx_req`), so the caller rings
    /// `CPU_IC_SOFTINT`. `sync` also asks the firmware to interrupt for the answer (`venus_iface_cmdq_write_nolock`).
    pub fn write_command(&self, packet: &[u32], sync: bool) -> Result<bool, QueueError> {
        let q = Queue::Command;
        let bytes = packet.first().copied().ok_or(QueueError::BadPacket)?;
        if bytes as usize != packet.len() * 4 {
            return Err(QueueError::BadPacket);
        }
        let rd = self.mem.read32(header(q, qhdr::READ_IDX));
        let wr = self.mem.read32(header(q, qhdr::WRITE_IDX));
        self.mem.barrier();
        let span = match rules::write_span(rd, wr, bytes) {
            Err(QueueError::Full) => {
                self.mem.write32(header(q, qhdr::TX_REQ), 1);
                self.mem.barrier();
                return Err(QueueError::Full);
            }
            other => other?,
        };
        self.mem.write32(header(q, qhdr::TX_REQ), 0);
        self.mem.barrier();
        let (first, second) = packet.split_at(span.first as usize);
        for (i, word) in first.iter().enumerate() {
            self.mem.write32(ring(q) + (span.at + i as u32) * 4, *word);
        }
        for (i, word) in second.iter().enumerate() {
            self.mem.write32(ring(q) + i as u32 * 4, *word);
        }
        self.mem.barrier();
        self.mem.write32(header(q, qhdr::WRITE_IDX), span.next);
        let rx_req = self.mem.read32(header(q, qhdr::RX_REQ)) != 0;
        if sync {
            self.mem.write32(header(Queue::Message, qhdr::RX_REQ), 1);
        }
        self.mem.barrier();
        Ok(rx_req)
    }

    /// `venus_read_queue` for the message or debug queue: the packet's words into `out`, and whether the firmware waits to be told
    /// there is room (`tx_req`), so the caller rings `CPU_IC_SOFTINT`. `Ok(None)` when it is empty. A packet that cannot be right is
    /// skipped with everything after it and reported as [`QueueError::BadPacket`], so that the next read starts clean.
    pub fn read(&self, q: Queue, out: &mut [u32; MAX_PACKET_WORDS as usize]) -> Result<Option<(usize, bool)>, QueueError> {
        let rd = self.mem.read32(header(q, qhdr::READ_IDX));
        let wr = self.mem.read32(header(q, qhdr::WRITE_IDX));
        self.mem.barrier();
        // Linux does not ask for an interrupt for debug text: the firmware would raise one with no message waiting.
        let wake = if q == Queue::Message { 1 } else { 0 };
        let head = if rd < QUEUE_WORDS { self.mem.read32(ring(q) + rd * 4) } else { 0 };
        let found = rules::read_span(rd, wr, head)?;
        let span = match found {
            Found::Empty => {
                self.mem.write32(header(q, qhdr::RX_REQ), wake);
                self.mem.barrier();
                return Ok(None);
            }
            Found::Dropped(next) => {
                self.mem.write32(header(q, qhdr::READ_IDX), next);
                self.mem.barrier();
                return Err(QueueError::BadPacket);
            }
            Found::Packet(span) => span,
        };
        for i in 0..span.first {
            out[i as usize] = self.mem.read32(ring(q) + (span.at + i) * 4);
        }
        for i in 0..span.second {
            out[(span.first + i) as usize] = self.mem.read32(ring(q) + i * 4);
        }
        self.mem.barrier();
        self.mem.write32(header(q, qhdr::READ_IDX), span.next);
        self.mem.barrier();
        let wr = self.mem.read32(header(q, qhdr::WRITE_IDX));
        self.mem.barrier();
        self.mem.write32(header(q, qhdr::RX_REQ), if span.next != wr { 0 } else { wake });
        let tx_req = self.mem.read32(header(q, qhdr::TX_REQ)) != 0;
        self.mem.barrier();
        Ok(Some(((span.first + span.second) as usize, tx_req)))
    }

    /// The firmware's last words: the SFR's text up to its first NUL, at most its size (`venus_sfr_print`).
    pub fn sfr(&self, out: &mut [u8; SFR_SIZE as usize]) -> usize {
        let mut len = 0;
        for word in 1..SFR_SIZE / 4 {
            let bytes = self.mem.read32(SFR_OFFSET + word * 4).to_le_bytes();
            for b in bytes {
                if b == 0 {
                    return len;
                }
                out[len] = b;
                len += 1;
            }
        }
        len
    }

    /// The indices of each queue, for a report.
    pub fn indices(&self) -> [(u32, u32); QUEUES as usize] {
        [Queue::Command, Queue::Message, Queue::Debug]
            .map(|q| (self.mem.read32(header(q, qhdr::READ_IDX)), self.mem.read32(header(q, qhdr::WRITE_IDX))))
    }
}
