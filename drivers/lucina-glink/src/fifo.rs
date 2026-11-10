// Derived from Linux drivers/rpmsg/qcom_glink_smem.c (v7.1): the SMEM item numbers, the descriptor layout, the
// ring arithmetic (`glink_smem_rx_avail`, `_rx_peek`, `_rx_advance`, `_tx_avail`, `_tx_write`) and the reserves.
// That file is GPL-2.0, Copyright (c) 2016, Linaro Ltd. This is a re-implementation in Rust, under the same licence.

//! The shared-memory FIFO pair of a GLINK edge, as `qcom_glink_smem.c` sets it up.
//!
//! Three SMEM items, all keyed by the remote's processor id (the modem is 1 on SC7180), make one edge:
//!
//! | item | what | who allocates it |
//! |---|---|---|
//! | 478, [`SMEM_ITEM_DESCRIPTOR`] | 32 bytes: `tx_tail`, `tx_head`, `rx_tail`, `rx_head` (u32 each) | the AP asks for it (`qcom_smem_alloc`, `-EEXIST` is fine), so whoever came first |
//! | 479, [`SMEM_ITEM_FIFO_0`] | the AP-to-remote FIFO, 16 KiB when the AP allocates it | the AP, the same way |
//! | 480, [`SMEM_ITEM_FIFO_1`] | the remote-to-AP FIFO | the remote; the AP only calls `qcom_smem_get` and takes the size from SMEM |
//!
//! The AP never allocates item 480, and it looks it up lazily: `glink_smem_rx_avail` retries `qcom_smem_get` until
//! the item exists and answers "nothing available" meanwhile. [`Shared::rx_fifo`] is that retry. Who really
//! allocates what on the Galaxy Book Go, and in which order, is not known until the laptop runs it.
//!
//! Each FIFO is a ring of `len` bytes (a multiple of 8) with one writer and one reader. The writer owns `head`, the
//! reader owns `tail`; `head == tail` is empty, so a ring never holds more than `len - 8` bytes. For the AP, from
//! `qcom_glink_smem_register`: `tx_pipe->tail = descs[0]`, `tx_pipe->head = descs[1]`, `rx_pipe->tail = descs[2]`,
//! `rx_pipe->head = descs[3]`. So the AP writes `descs[1]` and `descs[2]`, the remote `descs[0]` and `descs[3]`. At
//! registration the AP clears the two it owns (`*rx_pipe->tail = 0; *tx_pipe->head = 0`) and nothing else.
//!
//! The remote's head and tail are untrusted here. The C trusts them: a head beyond the ring makes
//! `glink_smem_rx_avail` return more than the ring holds and `glink_smem_rx_peek` read outside it. This code checks
//! every index it reads, and it also notices when an index that only the AP writes was changed by someone else
//! (the remote re-initialised the descriptor) or when the remote's tail moves past what was written. Those cases
//! are returned as a [`Fault`]; the layer above turns them into a peer reset.

use core::sync::atomic::{Ordering, fence};

/// `SMEM_GLINK_NATIVE_XPRT_DESCRIPTOR` (`qcom_glink_smem.c:34`).
pub const SMEM_ITEM_DESCRIPTOR: u32 = 478;
/// `SMEM_GLINK_NATIVE_XPRT_FIFO_0` (`qcom_glink_smem.c:35`): the AP's transmit FIFO.
pub const SMEM_ITEM_FIFO_0: u32 = 479;
/// `SMEM_GLINK_NATIVE_XPRT_FIFO_1` (`qcom_glink_smem.c:36`): the AP's receive FIFO.
pub const SMEM_ITEM_FIFO_1: u32 = 480;
/// The descriptor item is exactly this long (`qcom_glink_smem.c:267,282`).
pub const DESCRIPTOR_SIZE: usize = 32;
/// The size the AP asks for when it allocates item 479 (`SZ_16K`, `qcom_glink_smem.c:292`).
pub const TX_FIFO_SIZE: usize = 16 * 1024;
/// Frames start on multiples of this (`FIFO_ALIGNMENT`, and the `ALIGN(head, 8)` of `glink_smem_tx_write`).
pub const FIFO_ALIGNMENT: usize = 8;
/// `FIFO_FULL_RESERVE` (`qcom_glink_smem.c:30`): kept free so that a full ring never looks empty.
pub const FIFO_FULL_RESERVE: usize = 8;
/// `TX_BLOCKED_CMD_RESERVE` (`qcom_glink_smem.c:32`): kept free for the `READ_NOTIF` a blocked writer sends.
pub const TX_BLOCKED_CMD_RESERVE: usize = 8;

/// The smallest ring this code accepts; a name-carrying `OPEN` frame and the two reserves have to fit.
pub const MIN_FIFO_LEN: usize = 64;

/// Memory barriers the hardware mapping may need. The C has one `wmb()` (before the head update in
/// `glink_smem_tx_write`) and relies on the I/O accessors for the rest; the three points are named so the kernel
/// integration can use `dsb`-class barriers if the SMEM mapping needs them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Barrier {
    /// After reading an index the remote owns (rx head, tx tail), before touching the data it covers.
    AfterRemoteIndexRead,
    /// After the data of a frame is written, before `head` is published (`wmb()` in `glink_smem_tx_write`).
    BeforeHeadWrite,
    /// After the data of a frame is read, before `tail` is published (the remote may overwrite it then).
    BeforeTailWrite,
}

/// Word access to the shared memory of one edge, by byte offset in whatever flat space the integration chose
/// (the descriptor and both FIFOs are located inside it by [`Layout`]).
///
/// Only aligned 32-bit accesses are made, because the real mapping is device memory that cannot do unaligned or
/// narrower access. A value is what a little-endian load of that word returns (so the byte at the lowest address is
/// the least significant byte); on the AArch64 target that is a plain `ldr`/`str`.
pub trait Shared {
    fn read32(&mut self, off: usize) -> u32;
    fn write32(&mut self, off: usize, value: u32);

    /// A barrier point; the default is a `core::sync::atomic::fence`, which is a `dmb ish` on AArch64.
    fn barrier(&mut self, kind: Barrier) {
        match kind {
            Barrier::AfterRemoteIndexRead => fence(Ordering::Acquire),
            Barrier::BeforeHeadWrite | Barrier::BeforeTailWrite => fence(Ordering::Release),
        }
    }

    /// The receive FIFO, once the remote has allocated item 480. Called on each poll until it returns `Some`
    /// (`glink_smem_rx_avail` calls `qcom_smem_get(.., SMEM_GLINK_NATIVE_XPRT_FIFO_1, ..)` the same way).
    /// Ignored when [`Layout::rx`] is already known.
    fn rx_fifo(&mut self) -> Option<Region> {
        None
    }
}

/// The doorbell: `glink_smem_tx_kick`, a mailbox message that raises the remote's interrupt (on SC7180 the APCS IPC
/// register, bit 12 for the modem edge: `mboxes = <&apss_shared 12>`).
pub trait Doorbell {
    fn ring(&mut self);
}

/// A byte range of the shared memory: `off` is where it starts, `len` how long it is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Region {
    pub off: usize,
    pub len: usize,
}

/// Which end of the edge this [`Fifo`] is. The AP side is the one `qcom_glink_smem.c` implements. The remote side
/// reads the same descriptor mirrored (its `tx` is the AP's `rx`), which is what a loopback test or a model of the
/// modem needs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Side {
    Ap,
    Remote,
}

impl Side {
    /// Word indexes in the descriptor of (tx_tail, tx_head, rx_tail, rx_head) as seen from this side.
    const fn indexes(self) -> [usize; 4] {
        match self {
            Side::Ap => [0, 1, 2, 3],
            Side::Remote => [2, 3, 0, 1],
        }
    }
}

/// Where everything is, in the flat space of a [`Shared`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Layout {
    pub side: Side,
    /// Start of the 32-byte descriptor.
    pub desc: usize,
    /// The ring this side writes (item 479 for the AP).
    pub tx: Region,
    /// The ring this side reads (item 480 for the AP), or `None` until [`Shared::rx_fifo`] finds it.
    pub rx: Option<Region>,
}

/// Why a [`Layout`] or a region found later was refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LayoutError {
    /// An offset is not a multiple of 4.
    Misaligned,
    /// A ring is shorter than [`MIN_FIFO_LEN`], not a multiple of 8, or too long for 32-bit indexes.
    BadLength,
    /// Two of the three areas overlap.
    Overlap,
}

fn check_region(r: Region) -> Result<(), LayoutError> {
    if !r.off.is_multiple_of(4) {
        return Err(LayoutError::Misaligned);
    }
    if r.len < MIN_FIFO_LEN || !r.len.is_multiple_of(FIFO_ALIGNMENT) || r.len >= (1 << 31) {
        return Err(LayoutError::BadLength);
    }
    Ok(())
}

fn overlaps(a_off: usize, a_len: usize, b_off: usize, b_len: usize) -> bool {
    a_off < b_off.saturating_add(b_len) && b_off < a_off.saturating_add(a_len)
}

impl Layout {
    pub fn validate(&self) -> Result<(), LayoutError> {
        if !self.desc.is_multiple_of(4) {
            return Err(LayoutError::Misaligned);
        }
        check_region(self.tx)?;
        if overlaps(self.desc, DESCRIPTOR_SIZE, self.tx.off, self.tx.len) {
            return Err(LayoutError::Overlap);
        }
        if let Some(rx) = self.rx {
            self.check_rx(rx)?;
        }
        Ok(())
    }

    fn check_rx(&self, rx: Region) -> Result<(), LayoutError> {
        check_region(rx)?;
        if overlaps(self.desc, DESCRIPTOR_SIZE, rx.off, rx.len)
            || overlaps(self.tx.off, self.tx.len, rx.off, rx.len)
        {
            return Err(LayoutError::Overlap);
        }
        Ok(())
    }
}

/// Why the shared state cannot be trusted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Fault {
    /// An index outside its ring: the remote's head or tail is junk (or ours was overwritten with junk).
    Index,
    /// An index only this side writes is not what it last wrote: the descriptor was re-initialised underneath us.
    OwnIndexChanged,
    /// The remote's tail moved past data this side never wrote.
    TailBeyondHead,
}

/// The ring arithmetic of both directions over a [`Shared`]. It owns no memory; every method takes the accessor.
#[derive(Debug)]
pub struct Fifo {
    layout: Layout,
    idx: [usize; 4],
    /// What this side last wrote to its own `head` and `tail`; compared with the shared copy on every use.
    tx_head: u32,
    rx_tail: u32,
    /// The remote's tail as last accepted, to bound how far it may move.
    tx_tail_seen: u32,
}

const TX_TAIL: usize = 0;
const TX_HEAD: usize = 1;
const RX_TAIL: usize = 2;
const RX_HEAD: usize = 3;

impl Fifo {
    /// Checks the layout and clears the two indexes this side owns, as `qcom_glink_smem_register` does.
    pub fn new<S: Shared>(io: &mut S, layout: Layout) -> Result<Fifo, LayoutError> {
        layout.validate()?;
        let mut f = Fifo {
            layout,
            idx: layout.side.indexes(),
            tx_head: 0,
            rx_tail: 0,
            tx_tail_seen: 0,
        };
        io.write32(f.word(RX_TAIL), 0);
        io.write32(f.word(TX_HEAD), 0);
        f.tx_tail_seen = io.read32(f.word(TX_TAIL));
        Ok(f)
    }

    fn word(&self, which: usize) -> usize {
        self.layout.desc + 4 * self.idx[which]
    }

    fn tx_len(&self) -> usize {
        self.layout.tx.len
    }

    /// The receive ring, asking the accessor for it if it was not known at construction.
    fn rx_region<S: Shared>(&mut self, io: &mut S) -> Option<Region> {
        if self.layout.rx.is_none() {
            let found = io.rx_fifo()?;
            // A region the remote's SMEM item claims but that does not fit is treated as "not there yet".
            if self.layout.check_rx(found).is_err() {
                return None;
            }
            self.layout.rx = Some(found);
        }
        self.layout.rx
    }

    /// The largest frame the transmit ring can ever take, in bytes (before the reserves are subtracted it is
    /// `len`; a ring that is empty has `len - FIFO_FULL_RESERVE - TX_BLOCKED_CMD_RESERVE` available).
    pub fn tx_capacity(&self) -> usize {
        self.tx_len() - FIFO_FULL_RESERVE - TX_BLOCKED_CMD_RESERVE
    }

    /// The most the receive ring can ever hold unread (zero until the ring is known).
    pub fn rx_capacity(&self) -> usize {
        self.layout.rx.map_or(0, |r| r.len - FIFO_ALIGNMENT)
    }

    /// Re-synchronises after a reset. Our own unsent data is abandoned (the head becomes the remote's tail). The
    /// remote's unread data is abandoned too (the tail becomes the remote's head) unless `keep_unread`, which keeps
    /// the tail as it stands in the descriptor when that is a sane index: after a remote restart the first thing
    /// it wrote, its VERSION, is already there and must not be thrown away. Fails with the [`Fault`] if an index
    /// needed for that is junk; the caller tries again later.
    pub fn resync<S: Shared>(&mut self, io: &mut S, keep_unread: bool) -> Result<(), Fault> {
        let tx_tail = io.read32(self.word(TX_TAIL));
        if tx_tail as usize >= self.tx_len() {
            return Err(Fault::Index);
        }
        let new_rx_tail = match self.rx_region(io) {
            Some(rx) => {
                let head = io.read32(self.word(RX_HEAD));
                if head as usize >= rx.len {
                    return Err(Fault::Index);
                }
                let tail = io.read32(self.word(RX_TAIL));
                if keep_unread && (tail as usize) < rx.len {
                    tail
                } else {
                    head
                }
            }
            None => 0,
        };
        io.barrier(Barrier::BeforeHeadWrite);
        io.write32(self.word(TX_HEAD), tx_tail);
        io.write32(self.word(RX_TAIL), new_rx_tail);
        self.tx_head = tx_tail;
        self.tx_tail_seen = tx_tail;
        self.rx_tail = new_rx_tail;
        Ok(())
    }

    /// `glink_smem_rx_avail`: bytes the remote has written and we have not consumed.
    pub fn rx_avail<S: Shared>(&mut self, io: &mut S) -> Result<usize, Fault> {
        let Some(rx) = self.rx_region(io) else {
            return Ok(0);
        };
        let head = io.read32(self.word(RX_HEAD));
        let tail = io.read32(self.word(RX_TAIL));
        if tail != self.rx_tail {
            return Err(Fault::OwnIndexChanged);
        }
        if head as usize >= rx.len || tail as usize >= rx.len {
            return Err(Fault::Index);
        }
        io.barrier(Barrier::AfterRemoteIndexRead);
        let (head, tail) = (head as usize, tail as usize);
        Ok(if head < tail {
            rx.len - tail + head
        } else {
            head - tail
        })
    }

    /// `glink_smem_rx_peek`: copy `out.len()` bytes starting `offset` bytes after the tail, without consuming them.
    /// The caller has checked with [`Fifo::rx_avail`] that they are there. Reads whole words only.
    pub fn rx_peek<S: Shared>(&mut self, io: &mut S, offset: usize, out: &mut [u8]) {
        let Some(rx) = self.layout.rx else {
            out.fill(0);
            return;
        };
        let start = (self.rx_tail as usize + offset) % rx.len;
        read_bytes(io, rx, start, out);
    }

    /// `glink_smem_rx_advance`: consume `count` bytes.
    pub fn rx_advance<S: Shared>(&mut self, io: &mut S, count: usize) {
        let Some(rx) = self.layout.rx else {
            return;
        };
        let tail = ((self.rx_tail as usize + count) % rx.len) as u32;
        io.barrier(Barrier::BeforeTailWrite);
        io.write32(self.word(RX_TAIL), tail);
        self.rx_tail = tail;
    }

    /// Free bytes in the transmit ring as the C computes them (`glink_smem_tx_avail` before the reserves), checking
    /// the indexes first.
    fn tx_free<S: Shared>(&mut self, io: &mut S) -> Result<usize, Fault> {
        let len = self.tx_len();
        let head = io.read32(self.word(TX_HEAD));
        let tail = io.read32(self.word(TX_TAIL));
        if head != self.tx_head {
            return Err(Fault::OwnIndexChanged);
        }
        if head as usize >= len || tail as usize >= len {
            return Err(Fault::Index);
        }
        // The reader may only have consumed what is in flight: from its previous tail up to our head.
        let in_flight = (head as usize + len - self.tx_tail_seen as usize) % len;
        let consumed = (tail as usize + len - self.tx_tail_seen as usize) % len;
        if consumed > in_flight {
            return Err(Fault::TailBeyondHead);
        }
        self.tx_tail_seen = tail;
        io.barrier(Barrier::AfterRemoteIndexRead);
        let (head, tail) = (head as usize, tail as usize);
        Ok(if tail <= head {
            len - head + tail
        } else {
            tail - head
        })
    }

    /// `glink_smem_tx_avail`: bytes a frame may use right now, after both reserves.
    pub fn tx_avail<S: Shared>(&mut self, io: &mut S) -> Result<usize, Fault> {
        Ok(self
            .tx_free(io)?
            .saturating_sub(FIFO_FULL_RESERVE + TX_BLOCKED_CMD_RESERVE))
    }

    /// `glink_smem_tx_write` guarded by the `qcom_glink_tx_avail` check: write the concatenation of `parts` as one
    /// frame, zero-padded to a multiple of 8, and publish the new head. Returns `Ok(false)` without writing when it
    /// does not fit.
    ///
    /// The room needed is the *padded* length; the C compares the unpadded length, and the padding is then what the
    /// reserve is for. `blocked_notice` is the one frame allowed into the `TX_BLOCKED_CMD_RESERVE`: the
    /// `READ_NOTIF` a blocked writer sends (the C writes it without checking at all).
    pub fn tx_write<S: Shared>(
        &mut self,
        io: &mut S,
        parts: &[&[u8]],
        blocked_notice: bool,
    ) -> Result<bool, Fault> {
        let total: usize = parts.iter().map(|p| p.len()).sum();
        let padded = (total + FIFO_ALIGNMENT - 1) & !(FIFO_ALIGNMENT - 1);
        let free = self.tx_free(io)?;
        let reserve = if blocked_notice {
            FIFO_FULL_RESERVE
        } else {
            FIFO_FULL_RESERVE + TX_BLOCKED_CMD_RESERVE
        };
        if padded == 0 || free < reserve || padded > free - reserve {
            return Ok(false);
        }
        let len = self.tx_len();
        let base = self.layout.tx;
        let head = self.tx_head as usize;
        let mut it = parts.iter().flat_map(|p| p.iter().copied());
        let mut i = 0;
        while i < padded {
            let mut w = [0u8; 4];
            for b in &mut w {
                *b = it.next().unwrap_or(0);
            }
            // `head` and `len` are multiples of 8, so a word never straddles the end of the ring.
            io.write32(base.off + (head + i) % len, u32::from_le_bytes(w));
            i += 4;
        }
        io.barrier(Barrier::BeforeHeadWrite);
        let new_head = ((head + padded) % len) as u32;
        io.write32(self.word(TX_HEAD), new_head);
        self.tx_head = new_head;
        Ok(true)
    }
}

/// Copies `out.len()` bytes out of `ring` starting at byte `start` of it, wrapping, with word reads.
fn read_bytes<S: Shared>(io: &mut S, ring: Region, start: usize, out: &mut [u8]) {
    let mut pos = start;
    let mut n = 0;
    while n < out.len() {
        let word_pos = pos & !3;
        let w = io.read32(ring.off + word_pos).to_le_bytes();
        let mut b = pos & 3;
        while b < 4 && n < out.len() {
            out[n] = w[b];
            n += 1;
            b += 1;
        }
        // The ring length is a multiple of 4: the next word is the first one if this was the last.
        pos = (word_pos + 4) % ring.len;
    }
}
