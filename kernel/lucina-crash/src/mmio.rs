//! The register trace (patch 1188): each CPU's last few memory-mapped register accesses, kept in the saved log's region
//! across a reset, after Qualcomm's `msm_rtb` (the downstream kernels' register trace buffer, made for exactly this).
//!
//! A CPU that loads from or stores to a block whose clock or power is off never completes the access: no interrupt, no
//! bark, no record, and the firmware resets the board (gen 42's boot 256, 2026-11-08). The kernel notes each access before
//! it makes it, so the newest entry of a CPU that hung there names the register, and with it the block and the driver.
//!
//! The kernel writes the area through a write-combining mapping (`ostd/src/crash/mmio_trace.rs`); this crate has the
//! layout and the decoding, so that both are tested on the host. The RAM here loses about 1.6% of its bits over a firmware
//! reset (crash-recovery.md, 2026-11-02), so the header and every word are written [`COPIES`] times and read back by a
//! bitwise majority: a bit is wrong only if 3 of its 5 copies flipped.
//!
//! The area is the tail of the region that the store leaves free ([`crate::store::spare_tail`]); it is laid out as
//! - the header, [`COPIES`] words at 0: the format's magic in the low half, the kernel image's hash in the high half;
//! - from [`HEADER_LEN`], [`COPIES`] copies of a `[cpu][slot]` array of [`ENTRY_LEN`]-byte entries, one copy after the other;
//! - the remote processors' slots (patch 1210), [`COPIES`] copies of a `[remote]` array;
//! - in format 3, the idle stamps ([`Idle`]), [`COPIES`] copies of a `[cpu]` array, then the spin-lock waits ([`Wait`]) the same way.
//!
//! Format 2 (patch 1189) added the counter's value to each entry, so that the record says how long before the newest access
//! of all each CPU's own newest one was: all within microseconds means the reset came to a machine at work; one CPU far
//! behind means it had stopped. Format 3 (patch 1213, in format 4's larger tail) keeps [`ENTRIES`] = 8 entries per CPU
//! instead of 4, and adds what each CPU was doing besides register accesses: whether it sat in WFI, since when and with
//! the tick stopped, and which spin lock it had waited on for a second or more. A boot that upgrades the region reads the
//! format-2 area the previous kernel left ([`Trace::read`] takes either format) before it lays out format 3.

use core::fmt::{self, Write};

use crate::store::Mem;
use crate::waits;

/// CPUs traced; a CPU beyond these is not.
pub const CPUS: usize = 8;
/// Entries per CPU in format 3, used as a ring.
pub const ENTRIES: usize = 8;
/// Entries per CPU in format 2 (patches 1189-1212).
pub const ENTRIES_V2: usize = 4;
/// Copies of every word.
pub const COPIES: usize = 5;
/// Bytes per entry: three words.
pub const ENTRY_LEN: usize = 24;
/// Bytes before the entries.
pub const HEADER_LEN: usize = 64;
/// Remote processors whose SMP2P state is kept after the entries (patch 1210): the modem (MPSS, which also runs the
/// Wi-Fi firmware on sc7180) and the audio DSP (ADSP).
pub const REMOTES: [&str; 2] = ["mpss", "adsp"];
/// Bytes per remote slot: its state word and the time it was read.
pub const REMOTE_LEN: usize = 16;
/// Bytes per idle stamp: its state word and the time.
pub const IDLE_LEN: usize = 16;
/// Bytes per spin-lock wait: the lock, the caller, the time the wait began.
pub const WAIT_LEN: usize = 24;
/// `RP` in the top bits of a remote slot's state word: a slot that was written.
pub const REMOTE_TAG: u64 = 0x5250 << 48;
/// `ID` in the top bits of an idle stamp's state word.
pub const IDLE_TAG: u64 = 0x4944 << 48;
/// `LW` in the top bits of a spin-lock wait's first word.
pub const WAIT_TAG: u64 = 0x4c57 << 48;
const TAG_MASK: u64 = 0xffff << 48;
const ADDR_MASK: u64 = 0xffff_ffff_ffff;

/// `MMI3` in the low half of the header word: format 3 (patch 1213).
pub const MAGIC: u32 = u32::from_le_bytes(*b"MMI3");
/// `MMI2`: format 2, read once on the boot that upgrades the region. Format 1's traces are not read.
pub const MAGIC_V2: u32 = u32::from_le_bytes(*b"MMI2");
/// Sequence numbers count modulo this.
pub const SEQ_MOD: u16 = 1 << 13;

/// Where a format keeps what, from the area's start.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Layout {
    pub magic: u32,
    /// Entries per CPU.
    pub entries: usize,
    /// The bytes up to the end of the entries: an area smaller than this holds no trace.
    pub area_len: usize,
    pub remote_at: usize,
    /// Where the idle stamps start; 0 in a format without them.
    pub idle_at: usize,
    pub wait_at: usize,
    /// The bytes with everything the format has.
    pub full_len: usize,
}

const fn layout(magic: u32, entries: usize, stamps: bool) -> Layout {
    let area_len = HEADER_LEN + COPIES * CPUS * entries * ENTRY_LEN;
    let after_remotes = area_len + COPIES * REMOTES.len() * REMOTE_LEN;
    let (idle_at, wait_at, full_len) = if stamps {
        let wait_at = after_remotes + COPIES * CPUS * IDLE_LEN;
        (after_remotes, wait_at, wait_at + COPIES * CPUS * WAIT_LEN)
    } else {
        (0, 0, after_remotes)
    };
    Layout {
        magic,
        entries,
        area_len,
        remote_at: area_len,
        idle_at,
        wait_at,
        full_len,
    }
}

/// Format 3, which this kernel writes.
pub const V3: Layout = layout(MAGIC, ENTRIES, true);
/// Format 2, as patches 1189-1212 wrote it.
pub const V2: Layout = layout(MAGIC_V2, ENTRIES_V2, false);
/// The bytes format 3's entries need.
pub const AREA_LEN: usize = V3.area_len;
/// The bytes format 3 needs with everything.
pub const FULL_LEN: usize = V3.full_len;

impl Layout {
    /// Where copy `copy` of CPU `cpu`'s slot `slot` is.
    pub const fn entry_offset(&self, copy: usize, cpu: usize, slot: usize) -> usize {
        HEADER_LEN + ((copy * CPUS + cpu) * self.entries + slot) * ENTRY_LEN
    }

    /// Where copy `copy` of remote `remote`'s slot is.
    pub const fn remote_offset(&self, copy: usize, remote: usize) -> usize {
        self.remote_at + (copy * REMOTES.len() + remote) * REMOTE_LEN
    }

    /// Where copy `copy` of CPU `cpu`'s idle stamp is (format 3).
    pub const fn idle_offset(&self, copy: usize, cpu: usize) -> usize {
        self.idle_at + (copy * CPUS + cpu) * IDLE_LEN
    }

    /// Where copy `copy` of CPU `cpu`'s spin-lock wait is (format 3).
    pub const fn wait_offset(&self, copy: usize, cpu: usize) -> usize {
        self.wait_at + (copy * CPUS + cpu) * WAIT_LEN
    }

    pub const fn has_stamps(&self) -> bool {
        self.idle_at != 0
    }
}

/// Where copy `copy` of the header word is, from the area's start.
pub const fn header_offset(copy: usize) -> usize {
    copy * 8
}

/// Where copy `copy` of CPU `cpu`'s slot `slot` is in format 3.
pub const fn entry_offset(copy: usize, cpu: usize, slot: usize) -> usize {
    V3.entry_offset(copy, cpu, slot)
}

/// Where copy `copy` of remote `remote`'s slot is in format 3.
pub const fn remote_offset(copy: usize, remote: usize) -> usize {
    V3.remote_offset(copy, remote)
}

/// Where copy `copy` of CPU `cpu`'s idle stamp is in format 3.
pub const fn idle_offset(copy: usize, cpu: usize) -> usize {
    V3.idle_offset(copy, cpu)
}

/// Where copy `copy` of CPU `cpu`'s spin-lock wait is in format 3.
pub const fn wait_offset(copy: usize, cpu: usize) -> usize {
    V3.wait_offset(copy, cpu)
}

/// The format-3 header word for a kernel image whose hash is `image_hash`.
pub const fn header_word(image_hash: u32) -> u64 {
    header_word_for(MAGIC, image_hash)
}

/// The header word of the format whose magic is `magic`.
pub const fn header_word_for(magic: u32, image_hash: u32) -> u64 {
    magic as u64 | ((image_hash as u64) << 32)
}

/// A remote processor's inbound SMP2P bits, as last read, and the counter's value then (patch 1210).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Remote {
    /// Its slave-kernel bits: Linux's sc7180 tree names bit 0 fatal, 1 ready, 2 handover, 3 stop-ack (MPSS also 7
    /// shutdown-ack).
    pub bits: u32,
    pub time: u64,
}

impl Remote {
    pub const fn encode(&self) -> [u64; 2] {
        [REMOTE_TAG | self.bits as u64, self.time]
    }

    pub const fn decode(words: [u64; 2]) -> Option<Self> {
        if words[0] & TAG_MASK != REMOTE_TAG {
            return None;
        }
        Some(Self {
            bits: words[0] as u32,
            time: words[1],
        })
    }
}

/// A CPU's last WFI entry or exit (patch 1213): written as it enters WFI and as it leaves it, so that a CPU that stopped
/// in WFI and one that stopped running are told apart.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Idle {
    /// It was in WFI: the stamp was written on the way in.
    pub in_wfi: bool,
    /// Its tick was stopped (no timer due) when it went in.
    pub tick_stopped: bool,
    /// Its WFI entries, modulo 2^32.
    pub count: u32,
    /// The counter's value at that entry or exit.
    pub time: u64,
}

impl Idle {
    pub const fn encode(&self) -> [u64; 2] {
        [
            IDLE_TAG
                | ((self.count as u64) << 16)
                | ((self.tick_stopped as u64) << 1)
                | self.in_wfi as u64,
            self.time,
        ]
    }

    pub const fn decode(words: [u64; 2]) -> Option<Self> {
        if words[0] & TAG_MASK != IDLE_TAG {
            return None;
        }
        Some(Self {
            in_wfi: words[0] & 1 == 1,
            tick_stopped: (words[0] >> 1) & 1 == 1,
            count: (words[0] >> 16) as u32,
            time: words[1],
        })
    }
}

/// A spin lock a CPU had waited on for about a second or more (patch 1214's spinwatch notes it, 1213 keeps it). Cleared
/// when the CPU takes the lock.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Wait {
    /// The lock's address; kept in 48 bits and given back sign-extended, as the kernel's addresses are.
    pub lock: u64,
    /// The low half of the address of the waiting caller's `core::panic::Location`.
    pub location: u32,
    /// The counter's value when the wait began.
    pub since: u64,
}

impl Wait {
    pub const fn encode(&self) -> [u64; 3] {
        [
            WAIT_TAG | (self.lock & ADDR_MASK),
            self.location as u64,
            self.since,
        ]
    }

    pub const fn decode(words: [u64; 3]) -> Option<Self> {
        if words[0] & TAG_MASK != WAIT_TAG {
            return None;
        }
        let low = words[0] & ADDR_MASK;
        let lock = if low & (1 << 47) != 0 {
            low | TAG_MASK
        } else {
            low
        };
        Some(Self {
            lock,
            location: words[1] as u32,
            since: words[2],
        })
    }
}

/// One access, noted before it was made.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Entry {
    /// The register's physical address (48 bits).
    pub pa: u64,
    /// log2 of the access's bytes: 0 to 3.
    pub size_log2: u8,
    pub write: bool,
    /// Counts the CPU's accesses modulo [`SEQ_MOD`].
    pub seq: u16,
    /// The low half of the address of the caller's `core::panic::Location`, which names it on a kernel of the same build.
    pub location: u32,
    /// For a write, the low 32 bits of the value.
    pub value: u32,
    /// The counter's value (`CNTVCT_EL0`) when it was noted.
    pub time: u64,
}

impl Entry {
    /// Its three words. An entry with a zero address encodes as zero, which reads back as no entry.
    pub const fn encode(&self) -> [u64; 3] {
        let w0 = (self.pa & ADDR_MASK)
            | ((self.size_log2 as u64 & 3) << 48)
            | ((self.write as u64) << 50)
            | (((self.seq % SEQ_MOD) as u64) << 51);
        [
            w0,
            self.location as u64 | ((self.value as u64) << 32),
            self.time,
        ]
    }

    /// The entry three words hold, or `None` for an empty slot (no address).
    pub const fn decode(words: [u64; 3]) -> Option<Self> {
        let pa = words[0] & ADDR_MASK;
        if pa == 0 {
            return None;
        }
        Some(Self {
            pa,
            size_log2: ((words[0] >> 48) & 3) as u8,
            write: (words[0] >> 50) & 1 == 1,
            seq: (words[0] >> 51) as u16,
            location: words[1] as u32,
            value: (words[1] >> 32) as u32,
            time: words[2],
        })
    }
}

/// The bitwise majority of the copies: each bit as most copies have it.
pub fn majority(copies: &[u64; COPIES]) -> u64 {
    let mut out = 0u64;
    for bit in 0..64 {
        let ones = copies.iter().filter(|w| (*w >> bit) & 1 == 1).count();
        if ones * 2 > COPIES {
            out |= 1 << bit;
        }
    }
    out
}

fn read_word(mem: &impl Mem, at: usize) -> u64 {
    let mut b = [0u8; 8];
    mem.read(at, &mut b);
    u64::from_le_bytes(b)
}

/// The majority of the [`COPIES`] words that `at` places, for each copy.
fn vote(mem: &impl Mem, at: impl Fn(usize) -> usize) -> u64 {
    majority(&core::array::from_fn(|copy| read_word(mem, at(copy))))
}

/// What a boot left in the area.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Trace {
    /// 2 or 3.
    pub format: u8,
    /// The hash of the kernel image that wrote it.
    pub image_hash: u32,
    /// Each CPU's slots, as found; a format-2 trace fills the first [`ENTRIES_V2`].
    pub cpus: [[Option<Entry>; ENTRIES]; CPUS],
    /// Each of [`REMOTES`], if the area has room for them and a kernel wrote one (patch 1210).
    pub remotes: [Option<Remote>; 2],
    /// Each CPU's last WFI entry or exit (format 3).
    pub idle: [Option<Idle>; CPUS],
    /// Each CPU's spin-lock wait of a second or more, if it was still waiting (format 3).
    pub waits: [Option<Wait>; CPUS],
}

impl Trace {
    /// Reads the area at `at` in `mem`, in either format; `None` if it holds no trace (no magic after the vote) or does
    /// not fit.
    pub fn read(mem: &impl Mem, at: usize) -> Option<Self> {
        if at.checked_add(HEADER_LEN)? > mem.size() {
            return None;
        }
        let header = vote(mem, |copy| at + header_offset(copy));
        let (format, l) = match header as u32 {
            MAGIC => (3, V3),
            MAGIC_V2 => (2, V2),
            _ => return None,
        };
        if at + l.area_len > mem.size() {
            return None;
        }
        let mut cpus = [[None; ENTRIES]; CPUS];
        for (cpu, slots) in cpus.iter_mut().enumerate() {
            for (slot, entry) in slots.iter_mut().enumerate().take(l.entries) {
                let word =
                    |w: usize| vote(mem, |copy| at + l.entry_offset(copy, cpu, slot) + w * 8);
                *entry = Entry::decode([word(0), word(1), word(2)]);
            }
        }
        let mut trace = Self {
            format,
            image_hash: (header >> 32) as u32,
            cpus,
            remotes: [None; 2],
            idle: [None; CPUS],
            waits: [None; CPUS],
        };
        if at + l.full_len > mem.size() {
            return Some(trace);
        }
        for (remote, slot) in trace.remotes.iter_mut().enumerate() {
            let word = |w: usize| vote(mem, |copy| at + l.remote_offset(copy, remote) + w * 8);
            *slot = Remote::decode([word(0), word(1)]);
        }
        if l.has_stamps() {
            for cpu in 0..CPUS {
                let word = |w: usize| vote(mem, |copy| at + l.idle_offset(copy, cpu) + w * 8);
                trace.idle[cpu] = Idle::decode([word(0), word(1)]);
                let word = |w: usize| vote(mem, |copy| at + l.wait_offset(copy, cpu) + w * 8);
                trace.waits[cpu] = Wait::decode([word(0), word(1), word(2)]);
            }
        }
        Some(trace)
    }

    /// CPU `cpu`'s entries, newest first. The newest is the one whose successor is not there; an older one follows by how
    /// far back its sequence number is.
    pub fn newest_first(&self, cpu: usize) -> ([Option<Entry>; ENTRIES], usize) {
        let mut out = [None; ENTRIES];
        let Some(slots) = self.cpus.get(cpu) else {
            return (out, 0);
        };
        let present = |seq: u16| slots.iter().flatten().any(|e| e.seq == seq);
        let Some(newest) = slots
            .iter()
            .flatten()
            .find(|e| !present((e.seq + 1) % SEQ_MOD))
            .copied()
        else {
            return (out, 0);
        };
        let mut n = 0;
        for back in 0..ENTRIES as u16 {
            let seq = (newest.seq + SEQ_MOD - back) % SEQ_MOD;
            if let Some(e) = slots.iter().flatten().find(|e| e.seq == seq) {
                out[n] = Some(*e);
                n += 1;
            } else {
                break;
            }
        }
        (out, n)
    }

    /// The newest counter value anywhere in the trace: what "before the newest of all" counts back from.
    fn latest(&self) -> u64 {
        let entries = (0..CPUS)
            .filter_map(|cpu| self.newest_first(cpu).0[0])
            .map(|e| e.time);
        let remotes = self.remotes.iter().flatten().map(|r| r.time);
        let idle = self.idle.iter().flatten().map(|i| i.time);
        let waits = self.waits.iter().flatten().map(|w| w.since);
        entries
            .chain(remotes)
            .chain(idle)
            .chain(waits)
            .max()
            .unwrap_or(0)
    }

    /// The text for a record: for each CPU with entries, up to `per_cpu` of them, newest first, each with how long before the
    /// newest event of all it was (`frequency` is the counter's, in Hz; 0 leaves the times out); then the remote
    /// processors, the idle stamps and the spin-lock waits. `name` gives a caller's file and line from its location word,
    /// where the build is the same.
    pub fn write_text<'a>(
        &self,
        w: &mut impl Write,
        per_cpu: usize,
        frequency: u64,
        name: impl Fn(u32) -> Option<(&'a str, u32)>,
    ) -> fmt::Result {
        let latest = self.latest();
        let ago = |w: &mut dyn Write, prefix: &str, t: u64| -> fmt::Result {
            if frequency > 0 && latest > 0 {
                let us = (latest.saturating_sub(t) as u128 * 1_000_000 / frequency as u128) as u64;
                write!(w, "{prefix}-{}.{:03} ms", us / 1000, us % 1000)?;
            }
            Ok(())
        };
        let caller = |w: &mut dyn Write, location: u32| match name(location) {
            Some((file, line)) => write!(w, " {file}:{line}"),
            None => write!(w, " @{location:#x}"),
        };
        writeln!(
            w,
            "last register accesses, newest first, with how long before the newest of all (a CPU hung in an access stops at it):"
        )?;
        let mut any = false;
        for cpu in 0..CPUS {
            let (entries, n) = self.newest_first(cpu);
            if n == 0 {
                continue;
            }
            any = true;
            write!(w, "  cpu {cpu}:")?;
            for (i, e) in entries.iter().take(n.min(per_cpu)).flatten().enumerate() {
                if i > 0 {
                    write!(w, ";")?;
                }
                ago(w, " ", e.time)?;
                write!(
                    w,
                    " {} {:#x}/{}",
                    if e.write { "w" } else { "r" },
                    e.pa,
                    1u32 << e.size_log2
                )?;
                if e.write {
                    write!(w, "={:#x}", e.value)?;
                }
                caller(w, e.location)?;
            }
            writeln!(w)?;
        }
        if !any {
            writeln!(w, "  none")?;
        }
        for (name, remote) in REMOTES.iter().zip(self.remotes) {
            let Some(r) = remote else { continue };
            write!(w, "  {name}: smp2p in {:#x}", r.bits)?;
            let flags = [
                (0, "fatal"),
                (1, "ready"),
                (2, "handover"),
                (3, "stop-ack"),
                (7, "shutdown-ack"),
            ];
            let set: [&str; 5] = core::array::from_fn(|i| {
                if r.bits & (1 << flags[i].0) != 0 {
                    flags[i].1
                } else {
                    ""
                }
            });
            let mut first = true;
            for f in set.iter().filter(|f| !f.is_empty()) {
                write!(w, "{}{f}", if first { " (" } else { ", " })?;
                first = false;
            }
            if !first {
                write!(w, ")")?;
            }
            ago(w, ", read ", r.time)?;
            writeln!(w)?;
        }
        if self.idle.iter().any(Option::is_some) {
            writeln!(w, "idle, as each CPU last entered or left WFI:")?;
            for (cpu, idle) in self.idle.iter().enumerate() {
                let Some(i) = idle else { continue };
                if i.in_wfi {
                    write!(
                        w,
                        "  cpu {cpu}: in WFI{}",
                        if i.tick_stopped { ", tick stopped" } else { "" }
                    )?;
                    ago(w, ", since ", i.time)?;
                } else {
                    write!(w, "  cpu {cpu}: running")?;
                    ago(w, ", left WFI ", i.time)?;
                }
                writeln!(w, ", {} entries", i.count)?;
            }
        }
        if self.waits.iter().any(Option::is_some) {
            writeln!(w, "spin-lock waits (a second or more):")?;
            for (cpu, wait) in self.waits.iter().enumerate() {
                let Some(x) = wait else { continue };
                write!(w, "  cpu {cpu}: lock {:#x}, from", x.lock)?;
                caller(w, x.location)?;
                ago(w, ", waiting since ", x.since)?;
                writeln!(w)?;
            }
            let (waiting, in_wfi, seen) = self.wait_chain();
            let waiters = waiting.iter().filter(|l| **l != 0).count();
            if let (Some(lock), true) = (waits::same_lock(&waiting), waiters > 1) {
                writeln!(w, "  all {waiters} waiters wait on lock {lock:#x}")?;
            }
            write!(w, "  may hold it (traced, not waiting, not in WFI):")?;
            let mut from = 0;
            let mut none = true;
            while let Some(cpu) = waits::next_holder(from, &waiting, &in_wfi, &seen) {
                write!(w, " cpu {cpu}")?;
                none = false;
                from = cpu + 1;
            }
            writeln!(w, "{}", if none { " none traced" } else { "" })?;
        }
        Ok(())
    }

    /// What the wait-chain reader ([`waits`]) takes: each CPU's waited-on lock (0 for none), whether it sat in WFI, and
    /// whether it left any trace.
    pub fn wait_chain(&self) -> ([u64; CPUS], [bool; CPUS], [bool; CPUS]) {
        let waiting = core::array::from_fn(|cpu| self.waits[cpu].map_or(0, |x| x.lock));
        let in_wfi = core::array::from_fn(|cpu| self.idle[cpu].is_some_and(|i| i.in_wfi));
        let seen = core::array::from_fn(|cpu| {
            self.idle[cpu].is_some() || self.newest_first(cpu).1 > 0 || self.waits[cpu].is_some()
        });
        (waiting, in_wfi, seen)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Ram([u8; FULL_LEN + 64]);

    impl Mem for Ram {
        fn size(&self) -> usize {
            self.0.len()
        }
        fn read(&self, offset: usize, out: &mut [u8]) {
            out.copy_from_slice(&self.0[offset..offset + out.len()]);
        }
        fn write(&mut self, offset: usize, data: &[u8]) {
            self.0[offset..offset + data.len()].copy_from_slice(data);
        }
    }

    fn ram() -> Ram {
        Ram([0; FULL_LEN + 64])
    }

    fn put(mem: &mut Ram, at: impl Fn(usize) -> usize, words: &[u64]) {
        for copy in 0..COPIES {
            for (w, word) in words.iter().enumerate() {
                mem.write(at(copy) + w * 8, &word.to_le_bytes());
            }
        }
    }

    /// What the kernel does for a CPU's access number `count`: every copy of slot `count % entries`, sequence number
    /// `count % SEQ_MOD`.
    fn note_in(
        mem: &mut Ram,
        l: Layout,
        at: usize,
        cpu: usize,
        count: u32,
        pa: u64,
        time: Option<u64>,
    ) {
        let mut e = entry((count % SEQ_MOD as u32) as u16, pa);
        if let Some(t) = time {
            e.time = t;
        }
        put(
            mem,
            |copy| at + l.entry_offset(copy, cpu, count as usize % l.entries),
            &e.encode(),
        );
    }

    fn note(mem: &mut Ram, at: usize, cpu: usize, count: u32, pa: u64) {
        note_in(mem, V3, at, cpu, count, pa, None);
    }

    fn note_at(mem: &mut Ram, at: usize, cpu: usize, count: u32, pa: u64, time: u64) {
        note_in(mem, V3, at, cpu, count, pa, Some(time));
    }

    fn start(mem: &mut Ram, at: usize, hash: u32) {
        put(mem, |copy| at + header_offset(copy), &[header_word(hash)]);
    }

    fn entry(seq: u16, pa: u64) -> Entry {
        Entry {
            pa,
            size_log2: 2,
            write: seq.is_multiple_of(2),
            seq,
            location: 0x1234_5678,
            value: 7,
            time: 1000 + seq as u64 * 19_200,
        }
    }

    fn text(t: &Trace, per_cpu: usize, frequency: u64) -> Text {
        let mut s = Text {
            buf: [0; 4096],
            len: 0,
        };
        t.write_text(&mut s, per_cpu, frequency, |loc| {
            (loc == 0x1234_5678).then_some(("kernel/x.rs", 12))
        })
        .unwrap();
        s
    }

    #[test]
    fn format_3_fits_format_4s_tail_and_format_2_fit_format_3s() {
        const _: () = assert!(V2.full_len <= 4096);
        const _: () = assert!(FULL_LEN <= 64 * 1024);
        // Format 2's offsets are the ones patches 1189-1212 wrote.
        assert_eq!(
            (V2.area_len, V2.remote_at, V2.full_len),
            (64 + 5 * 8 * 4 * 24, 3904, 4064)
        );
        assert_eq!(V2.entry_offset(1, 2, 3), 64 + ((8 + 2) * 4 + 3) * 24);
        assert!(!V2.has_stamps() && V3.has_stamps());
        // Format 3's parts follow one another without overlap.
        assert_eq!(V3.remote_at, V3.area_len);
        assert_eq!(
            V3.idle_at,
            V3.remote_offset(COPIES - 1, REMOTES.len() - 1) + REMOTE_LEN
        );
        assert_eq!(V3.wait_at, V3.idle_offset(COPIES - 1, CPUS - 1) + IDLE_LEN);
        assert_eq!(V3.full_len, V3.wait_offset(COPIES - 1, CPUS - 1) + WAIT_LEN);
    }

    #[test]
    fn a_remote_survives_its_encoding_and_an_unwritten_slot_is_none() {
        let r = Remote {
            bits: 0x86,
            time: 123_456,
        };
        assert_eq!(Remote::decode(r.encode()), Some(r));
        assert_eq!(Remote::decode([0x86, 1]), None);
    }

    #[test]
    fn stamps_and_waits_survive_their_encoding() {
        let i = Idle {
            in_wfi: true,
            tick_stopped: true,
            count: u32::MAX,
            time: u64::MAX,
        };
        assert_eq!(Idle::decode(i.encode()), Some(i));
        let i = Idle {
            in_wfi: false,
            tick_stopped: false,
            count: 3,
            time: 9,
        };
        assert_eq!(Idle::decode(i.encode()), Some(i));
        assert_eq!(Idle::decode([0, 0]), None);
        for lock in [0xffff_8000_0012_3440, 0x0000_7fff_ffff_fff8, 0x40] {
            let x = Wait {
                lock,
                location: 0xdead_beef,
                since: 77,
            };
            assert_eq!(Wait::decode(x.encode()), Some(x), "{lock:#x}");
        }
        assert_eq!(Wait::decode([0, 1, 2]), None);
        // A wait never reads as a stamp or a remote, and the other way round.
        let x = Wait {
            lock: 0x40,
            location: 1,
            since: 2,
        }
        .encode();
        assert_eq!(Idle::decode([x[0], x[1]]), None);
        assert_eq!(Remote::decode([x[0], x[1]]), None);
    }

    #[test]
    fn the_text_has_each_remotes_bits_and_age() {
        let mut mem = ram();
        start(&mut mem, 0, 1);
        note(&mut mem, 0, 0, 2, 0xaf0_0040);
        // The modem: ready and handover, read 2 ms before CPU 0's newest access (time 39400).
        put(
            &mut mem,
            |copy| remote_offset(copy, 0),
            &Remote {
                bits: 0x6,
                time: 39_400 - 38_400,
            }
            .encode(),
        );
        let t = Trace::read(&mem, 0).unwrap();
        assert_eq!(t.remotes[1], None);
        let s = text(&t, 1, 19_200_000);
        assert!(
            s.as_str()
                .contains("  mpss: smp2p in 0x6 (ready, handover), read -2.000 ms\n"),
            "{}",
            s.as_str()
        );
        assert!(!s.as_str().contains("adsp"));
        assert!(
            !s.as_str().contains("idle") && !s.as_str().contains("spin-lock"),
            "{}",
            s.as_str()
        );
    }

    #[test]
    fn an_entry_survives_its_encoding() {
        let e = Entry {
            pa: 0xab_c000_0f04,
            size_log2: 3,
            write: true,
            seq: 8191,
            location: 0xdead_beef,
            value: 0xffff_ffff,
            time: u64::MAX - 3,
        };
        assert_eq!(Entry::decode(e.encode()), Some(e));
        assert_eq!(Entry::decode([0, 5, 6]), None);
    }

    #[test]
    fn no_magic_no_trace() {
        let mem = ram();
        assert_eq!(Trace::read(&mem, 0), None);
    }

    #[test]
    fn the_newest_comes_first_and_the_ring_wraps() {
        let mut mem = ram();
        start(&mut mem, 32, 0x99);
        // CPU 3 made 19 accesses: slots hold 11..=18, the newest 18.
        for count in 0..19 {
            note(&mut mem, 32, 3, count, 0x1000 + count as u64 * 4);
        }
        // Across the sequence numbers' wrap on CPU 5: 8190, 8191, 0.
        for count in [8190, 8191, 8192] {
            note(&mut mem, 32, 5, count, 0x2000 + count as u64);
        }
        let t = Trace::read(&mem, 32).unwrap();
        assert_eq!((t.format, t.image_hash), (3, 0x99));
        let (e, n) = t.newest_first(3);
        assert_eq!(n, ENTRIES);
        assert_eq!(e[0].unwrap().seq, 18);
        assert_eq!(e[7].unwrap().seq, 11);
        let (e, n) = t.newest_first(5);
        assert_eq!((n, e[0].unwrap().seq, e[2].unwrap().seq), (3, 0, 8190));
        assert_eq!(t.newest_first(0).1, 0);
    }

    #[test]
    fn a_format_2_area_reads_as_it_was_written() {
        // What a kernel of patches 1189-1212 left: 4 entries per CPU, the remotes right after them.
        let mut mem = ram();
        put(
            &mut mem,
            |copy| 16 + header_offset(copy),
            &[header_word_for(MAGIC_V2, 0x42)],
        );
        for count in 0..9 {
            note_in(&mut mem, V2, 16, 1, count, 0x3000 + count as u64 * 4, None);
        }
        put(
            &mut mem,
            |copy| 16 + V2.remote_offset(copy, 1),
            &Remote { bits: 1, time: 5 }.encode(),
        );
        let t = Trace::read(&mem, 16).unwrap();
        assert_eq!((t.format, t.image_hash), (2, 0x42));
        let (e, n) = t.newest_first(1);
        assert_eq!(
            (n, e[0].unwrap().seq, e[3].unwrap().seq),
            (ENTRIES_V2, 8, 5)
        );
        assert_eq!(t.remotes, [None, Some(Remote { bits: 1, time: 5 })]);
        assert_eq!((t.idle, t.waits), ([None; CPUS], [None; CPUS]));
        // In a tail as small as format 3's, which is where the upgrading boot finds it.
        struct Small<'a>(&'a Ram);
        impl Mem for Small<'_> {
            fn size(&self) -> usize {
                16 + 4080
            }
            fn read(&self, offset: usize, out: &mut [u8]) {
                assert!(offset + out.len() <= self.size());
                self.0.read(offset, out);
            }
            fn write(&mut self, _: usize, _: &[u8]) {}
        }
        assert_eq!(Trace::read(&Small(&mem), 16), Some(t));
        // A format-3 area does not fit there.
        let mut mem3 = ram();
        start(&mut mem3, 16, 1);
        assert_eq!(Trace::read(&Small(&mem3), 16), None);
    }

    #[test]
    fn two_flipped_copies_of_five_are_outvoted() {
        let mut mem = ram();
        start(&mut mem, 0, 1);
        note(&mut mem, 0, 2, 4, 0x0b2a_0010);
        put(
            &mut mem,
            |copy| idle_offset(copy, 2),
            &Idle {
                in_wfi: true,
                tick_stopped: false,
                count: 6,
                time: 40,
            }
            .encode(),
        );
        // Flip every bit of two copies of the entry, the stamp and the header.
        for copy in [1, 4] {
            let mut flip = |at: usize| {
                let mut b = [0u8; 8];
                mem.read(at, &mut b);
                mem.write(at, &(!u64::from_le_bytes(b)).to_le_bytes());
            };
            for w in 0..3 {
                flip(entry_offset(copy, 2, 4 % ENTRIES) + w * 8);
            }
            for w in 0..2 {
                flip(idle_offset(copy, 2) + w * 8);
            }
            mem.write(header_offset(copy), &0u64.to_le_bytes());
        }
        let t = Trace::read(&mem, 0).unwrap();
        assert_eq!(t.newest_first(2).0[0], Some(entry(4, 0x0b2a_0010)));
        assert_eq!(
            t.idle[2],
            Some(Idle {
                in_wfi: true,
                tick_stopped: false,
                count: 6,
                time: 40
            })
        );
    }

    #[test]
    fn the_text_names_the_hung_access_first() {
        let mut mem = ram();
        start(&mut mem, 0, 1);
        note(&mut mem, 0, 0, 1, 0x1000);
        note(&mut mem, 0, 0, 2, 0xaf0_0040);
        // CPU 3's newest access was 1.5 ms before CPU 0's (counter at 19.2 MHz: 28800 ticks).
        note_at(&mut mem, 0, 3, 7, 0x2000, 1000 + 2 * 19_200 - 28_800);
        let t = Trace::read(&mem, 0).unwrap();
        let s = text(&t, 3, 19_200_000);
        let text_ = s.as_str();
        assert!(text_.contains("cpu 0: -0.000 ms w 0xaf00040/4=0x7 kernel/x.rs:12; -1.000 ms r 0x1000/4 kernel/x.rs:12"), "{text_}");
        assert!(
            text_.contains("cpu 3: -1.500 ms r 0x2000/4 kernel/x.rs:12"),
            "{text_}"
        );
        assert!(!text_.contains("cpu 1"));
        let mut s = Text {
            buf: [0; 4096],
            len: 0,
        };
        Trace::read(&mem, 0)
            .unwrap()
            .write_text(&mut s, 1, 0, |_| None)
            .unwrap();
        assert!(
            s.as_str()
                .contains("cpu 0: w 0xaf00040/4=0x7 @0x12345678\n"),
            "{}",
            s.as_str()
        );
    }

    #[test]
    fn the_text_says_who_sat_in_wfi_and_who_waited_on_which_lock() {
        // A pile-up: CPU 1 holds the lock and runs; CPUs 2 and 3 wait on it; CPU 0 sits in WFI with the tick stopped;
        // CPU 4 left only a stamp from before. Counter at 19.2 MHz, the newest event at 192_000_000 (10 s).
        let f = 19_200_000;
        let mut mem = ram();
        start(&mut mem, 0, 1);
        note_at(&mut mem, 0, 1, 0, 0x1000, 10 * f);
        let stamp = |in_wfi, tick_stopped, count, time| {
            Idle {
                in_wfi,
                tick_stopped,
                count,
                time,
            }
            .encode()
        };
        put(
            &mut mem,
            |copy| idle_offset(copy, 0),
            &stamp(true, true, 900, 10 * f - f / 2),
        );
        put(
            &mut mem,
            |copy| idle_offset(copy, 1),
            &stamp(false, false, 40, 10 * f - 3 * f),
        );
        put(
            &mut mem,
            |copy| idle_offset(copy, 4),
            &stamp(true, false, 7, 10 * f - f),
        );
        let lock = 0xffff_8000_0040_1230;
        put(
            &mut mem,
            |copy| wait_offset(copy, 2),
            &Wait {
                lock,
                location: 0x1234_5678,
                since: 10 * f - 4 * f,
            }
            .encode(),
        );
        put(
            &mut mem,
            |copy| wait_offset(copy, 3),
            &Wait {
                lock,
                location: 0x99,
                since: 10 * f - 2 * f,
            }
            .encode(),
        );
        let t = Trace::read(&mem, 0).unwrap();
        let s = text(&t, 2, f);
        let out = s.as_str();
        assert!(out.contains("idle, as each CPU last entered or left WFI:\n  cpu 0: in WFI, tick stopped, since -500.000 ms, 900 entries\n  cpu 1: running, left WFI -3000.000 ms, 40 entries\n  cpu 4: in WFI, since -1000.000 ms, 7 entries\n"), "{out}");
        assert!(out.contains("spin-lock waits (a second or more):\n  cpu 2: lock 0xffff800000401230, from kernel/x.rs:12, waiting since -4000.000 ms\n  cpu 3: lock 0xffff800000401230, from @0x99, waiting since -2000.000 ms\n"), "{out}");
        assert!(
            out.contains("  all 2 waiters wait on lock 0xffff800000401230\n"),
            "{out}"
        );
        assert!(
            out.contains("  may hold it (traced, not waiting, not in WFI): cpu 1\n"),
            "{out}"
        );
        // Two locks: no "all wait on" line; nobody traced who could hold either.
        put(
            &mut mem,
            |copy| wait_offset(copy, 1),
            &Wait {
                lock: 0xffff_8000_0000_0040,
                location: 1,
                since: 10 * f,
            }
            .encode(),
        );
        let s = text(&Trace::read(&mem, 0).unwrap(), 2, f);
        assert!(!s.as_str().contains("waiters wait on"), "{}", s.as_str());
        assert!(
            s.as_str().contains("not in WFI): none traced\n"),
            "{}",
            s.as_str()
        );
    }

    struct Text {
        buf: [u8; 4096],
        len: usize,
    }

    impl Text {
        fn as_str(&self) -> &str {
            core::str::from_utf8(&self.buf[..self.len]).unwrap()
        }
    }

    impl Write for Text {
        fn write_str(&mut self, s: &str) -> fmt::Result {
            self.buf[self.len..self.len + s.len()].copy_from_slice(s.as_bytes());
            self.len += s.len();
            Ok(())
        }
    }
}
