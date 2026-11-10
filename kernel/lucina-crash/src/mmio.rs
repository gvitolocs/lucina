//! The register trace (patch 1188): each CPU's last few memory-mapped register accesses, kept in the saved log's region
//! across a reset, after Qualcomm's `msm_rtb` (the downstream kernels' register trace buffer, made for exactly this).
//!
//! A CPU that loads from or stores to a block whose clock or power is off never completes the access: no interrupt, no
//! bark, no record, and the firmware resets the board (gen 42's boot 256, 2026-11-08). The kernel notes each access before
//! it makes it, so the newest entry of a CPU that hung there names the register, and with it the block and the driver.
//!
//! The kernel writes the area through a write-combining mapping (`ostd/src/crash/mmio_trace.rs`); this crate has the
//! layout and the decoding, so that both are tested on the host. The RAM here loses about 1.6% of its bits over a firmware
//! reset (crash-recovery.md, 2026-11-02), so the header and every entry are written [`COPIES`] times and read back by a
//! bitwise majority: a bit is wrong only if 3 of its 5 copies flipped.
//!
//! The area is the tail of the region that the store leaves free ([`crate::store::spare_tail`]); it is laid out as
//! - the header, [`COPIES`] words at 0: [`MAGIC`] in the low half, the kernel image's hash in the high half;
//! - from [`HEADER_LEN`], [`COPIES`] copies of a `[cpu][slot]` array of [`ENTRY_LEN`]-byte entries, one copy after the other.
//!
//! Format 2 (patch 1189) adds the counter's value to each entry, so that the record says how long before the newest access
//! of all each CPU's own newest one was: all within microseconds means the reset came to a machine at work; one CPU far
//! behind means it had stopped.

use core::fmt::{self, Write};

use crate::store::Mem;

/// CPUs traced; a CPU beyond these is not.
pub const CPUS: usize = 8;
/// Entries per CPU, used as a ring.
pub const ENTRIES: usize = 4;
/// Copies of every word.
pub const COPIES: usize = 5;
/// Bytes per entry: three words.
pub const ENTRY_LEN: usize = 24;
/// Bytes before the entries.
pub const HEADER_LEN: usize = 64;
/// The bytes the area needs.
pub const AREA_LEN: usize = HEADER_LEN + COPIES * CPUS * ENTRIES * ENTRY_LEN;
/// Remote processors whose SMP2P state is kept after the entries (patch 1210): the modem (MPSS, which also runs the
/// Wi-Fi firmware on sc7180) and the audio DSP (ADSP).
pub const REMOTES: [&str; 2] = ["mpss", "adsp"];
/// Bytes per remote slot: its state word and the time it was read.
pub const REMOTE_LEN: usize = 16;
/// Where the remote slots start, from the area's start: right after the entries.
pub const REMOTE_AT: usize = AREA_LEN;
/// The bytes the area needs with the remote slots.
pub const FULL_LEN: usize = AREA_LEN + COPIES * REMOTES.len() * REMOTE_LEN;
/// `RP` in the top bits of a remote slot's state word: a slot that was written.
pub const REMOTE_TAG: u64 = 0x5250 << 48;

/// Where copy `copy` of remote `remote`'s slot is, from the area's start.
pub const fn remote_offset(copy: usize, remote: usize) -> usize {
    REMOTE_AT + (copy * REMOTES.len() + remote) * REMOTE_LEN
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
        if words[0] & (0xffff << 48) != REMOTE_TAG {
            return None;
        }
        Some(Self { bits: words[0] as u32, time: words[1] })
    }
}

/// `MMI2` in the low half of the header word: format 2, with times (patch 1189). Format 1's traces are not read.
pub const MAGIC: u32 = u32::from_le_bytes(*b"MMI2");
/// Sequence numbers count modulo this.
pub const SEQ_MOD: u16 = 1 << 13;

/// Where copy `copy` of the header word is, from the area's start.
pub const fn header_offset(copy: usize) -> usize {
    copy * 8
}

/// Where copy `copy` of CPU `cpu`'s slot `slot` is, from the area's start.
pub const fn entry_offset(copy: usize, cpu: usize, slot: usize) -> usize {
    HEADER_LEN + ((copy * CPUS + cpu) * ENTRIES + slot) * ENTRY_LEN
}

/// The header word for a kernel image whose hash is `image_hash`.
pub const fn header_word(image_hash: u32) -> u64 {
    MAGIC as u64 | ((image_hash as u64) << 32)
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
        let w0 = (self.pa & 0xffff_ffff_ffff)
            | ((self.size_log2 as u64 & 3) << 48)
            | ((self.write as u64) << 50)
            | (((self.seq % SEQ_MOD) as u64) << 51);
        [w0, self.location as u64 | ((self.value as u64) << 32), self.time]
    }

    /// The entry three words hold, or `None` for an empty slot (no address).
    pub const fn decode(words: [u64; 3]) -> Option<Self> {
        let pa = words[0] & 0xffff_ffff_ffff;
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

/// What a boot left in the area.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Trace {
    /// The hash of the kernel image that wrote it.
    pub image_hash: u32,
    /// Each CPU's slots, as found.
    pub cpus: [[Option<Entry>; ENTRIES]; CPUS],
    /// Each of [`REMOTES`], if the area has room for them and a kernel wrote one (patch 1210).
    pub remotes: [Option<Remote>; 2],
}

impl Trace {
    /// Reads the area at `at` in `mem`; `None` if it holds no trace (no magic after the vote) or does not fit.
    pub fn read(mem: &impl Mem, at: usize) -> Option<Self> {
        if at.checked_add(AREA_LEN)? > mem.size() {
            return None;
        }
        let header = majority(&core::array::from_fn(|copy| read_word(mem, at + header_offset(copy))));
        if header as u32 != MAGIC {
            return None;
        }
        let mut cpus = [[None; ENTRIES]; CPUS];
        for (cpu, slots) in cpus.iter_mut().enumerate() {
            for (slot, entry) in slots.iter_mut().enumerate() {
                let word = |w: usize| {
                    majority(&core::array::from_fn(|copy| read_word(mem, at + entry_offset(copy, cpu, slot) + w * 8)))
                };
                *entry = Entry::decode([word(0), word(1), word(2)]);
            }
        }
        let mut remotes = [None; 2];
        if at.checked_add(FULL_LEN).is_some_and(|end| end <= mem.size()) {
            for (remote, slot) in remotes.iter_mut().enumerate() {
                let word = |w: usize| majority(&core::array::from_fn(|copy| read_word(mem, at + remote_offset(copy, remote) + w * 8)));
                *slot = Remote::decode([word(0), word(1)]);
            }
        }
        Some(Self {
            image_hash: (header >> 32) as u32,
            cpus,
            remotes,
        })
    }

    /// CPU `cpu`'s entries, newest first. The newest is the one whose successor is not there; an older one follows by how
    /// far back its sequence number is.
    pub fn newest_first(&self, cpu: usize) -> ([Option<Entry>; ENTRIES], usize) {
        let mut out = [None; ENTRIES];
        let Some(slots) = self.cpus.get(cpu) else {
            return (out, 0);
        };
        let present = |seq: u16| slots.iter().flatten().any(|e| e.seq == seq);
        let Some(newest) = slots.iter().flatten().find(|e| !present((e.seq + 1) % SEQ_MOD)).copied() else {
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

    /// The text for a record: for each CPU with entries, up to `per_cpu` of them, newest first, each with how long before the
    /// newest access of all it was (`frequency` is the counter's, in Hz; 0 leaves the times out). `name` gives a caller's
    /// file and line from its location word, where the build is the same.
    pub fn write_text<'a>(
        &self,
        w: &mut impl Write,
        per_cpu: usize,
        frequency: u64,
        name: impl Fn(u32) -> Option<(&'a str, u32)>,
    ) -> fmt::Result {
        let latest = (0..CPUS).filter_map(|cpu| self.newest_first(cpu).0[0]).map(|e| e.time).max().unwrap_or(0);
        writeln!(w, "last register accesses, newest first, with how long before the newest of all (a CPU hung in an access stops at it):")?;
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
                if frequency > 0 {
                    let us = (latest.saturating_sub(e.time) as u128 * 1_000_000 / frequency as u128) as u64;
                    write!(w, " -{}.{:03} ms", us / 1000, us % 1000)?;
                }
                write!(w, " {} {:#x}/{}", if e.write { "w" } else { "r" }, e.pa, 1u32 << e.size_log2)?;
                if e.write {
                    write!(w, "={:#x}", e.value)?;
                }
                match name(e.location) {
                    Some((file, line)) => write!(w, " {file}:{line}")?,
                    None => write!(w, " @{:#x}", e.location)?,
                }
            }
            writeln!(w)?;
        }
        if !any {
            writeln!(w, "  none")?;
        }
        for (name, remote) in REMOTES.iter().zip(self.remotes) {
            let Some(r) = remote else { continue };
            write!(w, "  {name}: smp2p in {:#x}", r.bits)?;
            let flags = [(0, "fatal"), (1, "ready"), (2, "handover"), (3, "stop-ack"), (7, "shutdown-ack")];
            let set: [&str; 5] = core::array::from_fn(|i| if r.bits & (1 << flags[i].0) != 0 { flags[i].1 } else { "" });
            let mut first = true;
            for f in set.iter().filter(|f| !f.is_empty()) {
                write!(w, "{}{f}", if first { " (" } else { ", " })?;
                first = false;
            }
            if !first {
                write!(w, ")")?;
            }
            if frequency > 0 && latest > 0 {
                let us = (latest.saturating_sub(r.time) as u128 * 1_000_000 / frequency as u128) as u64;
                write!(w, ", read -{}.{:03} ms", us / 1000, us % 1000)?;
            }
            writeln!(w)?;
        }
        Ok(())
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

    /// What the kernel does for a CPU's access number `count`: every copy of slot `count % ENTRIES`, sequence number
    /// `count % SEQ_MOD`.
    fn note(mem: &mut Ram, at: usize, cpu: usize, count: u32, pa: u64) {
        let words = entry((count % SEQ_MOD as u32) as u16, pa).encode();
        for copy in 0..COPIES {
            for (w, word) in words.iter().enumerate() {
                mem.write(at + entry_offset(copy, cpu, count as usize % ENTRIES) + w * 8, &word.to_le_bytes());
            }
        }
    }

    /// As `note`, with an explicit counter value.
    fn note_at(mem: &mut Ram, at: usize, cpu: usize, count: u32, pa: u64, time: u64) {
        let mut e = entry((count % SEQ_MOD as u32) as u16, pa);
        e.time = time;
        let words = e.encode();
        for copy in 0..COPIES {
            for (w, word) in words.iter().enumerate() {
                mem.write(at + entry_offset(copy, cpu, count as usize % ENTRIES) + w * 8, &word.to_le_bytes());
            }
        }
    }

    fn t_check(mem: &Ram) {
        assert!(Trace::read(mem, 0).is_some());
    }

    fn start(mem: &mut Ram, at: usize, hash: u32) {
        for copy in 0..COPIES {
            mem.write(at + header_offset(copy), &header_word(hash).to_le_bytes());
        }
    }

    fn entry(seq: u16, pa: u64) -> Entry {
        Entry { pa, size_log2: 2, write: seq % 2 == 0, seq, location: 0x1234_5678, value: 7, time: 1000 + seq as u64 * 19_200 }
    }

    #[test]
    fn the_area_fits_the_spare_tail_of_a_2_mib_region() {
        assert!(FULL_LEN <= 4096, "{FULL_LEN}");
    }

    #[test]
    fn a_remote_survives_its_encoding_and_an_unwritten_slot_is_none() {
        let r = Remote { bits: 0x86, time: 123_456 };
        assert_eq!(Remote::decode(r.encode()), Some(r));
        assert_eq!(Remote::decode([0x86, 1]), None);
    }

    #[test]
    fn the_text_has_each_remotes_bits_and_age() {
        let mut mem = ram();
        start(&mut mem, 0, 1);
        note(&mut mem, 0, 0, 2, 0xaf0_0040);
        // The modem: ready and handover, read 2 ms before CPU 0's newest access (time 39400).
        let words = Remote { bits: 0x6, time: 39_400 - 38_400 }.encode();
        for copy in 0..COPIES {
            for (w, word) in words.iter().enumerate() {
                mem.write(remote_offset(copy, 0) + w * 8, &word.to_le_bytes());
            }
        }
        let t = Trace::read(&mem, 0).unwrap();
        assert_eq!(t.remotes[1], None);
        let mut s = Text { buf: [0; 1024], len: 0 };
        t.write_text(&mut s, 1, 19_200_000, |_| None).unwrap();
        assert!(s.as_str().contains("  mpss: smp2p in 0x6 (ready, handover), read -2.000 ms\n"), "{}", s.as_str());
        assert!(!s.as_str().contains("adsp"));
    }

    #[test]
    fn an_entry_survives_its_encoding() {
        let e = Entry { pa: 0xab_c000_0f04, size_log2: 3, write: true, seq: 8191, location: 0xdead_beef, value: 0xffff_ffff, time: u64::MAX - 3 };
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
        // CPU 3 made 9 accesses: slots hold 5..=8, the newest 8.
        for count in 0..9 {
            note(&mut mem, 32, 3, count, 0x1000 + count as u64 * 4);
        }
        // Across the sequence numbers' wrap on CPU 5: 8190, 8191, 0.
        for count in [8190, 8191, 8192] {
            note(&mut mem, 32, 5, count, 0x2000 + count as u64);
        }
        let t = Trace::read(&mem, 32).unwrap();
        assert_eq!(t.image_hash, 0x99);
        let (e, n) = t.newest_first(3);
        assert_eq!(n, ENTRIES);
        assert_eq!(e[0].unwrap().seq, 8);
        assert_eq!(e[3].unwrap().seq, 5);
        let (e, n) = t.newest_first(5);
        assert_eq!((n, e[0].unwrap().seq, e[2].unwrap().seq), (3, 0, 8190));
        assert_eq!(t.newest_first(0).1, 0);
    }

    #[test]
    fn two_flipped_copies_of_five_are_outvoted() {
        let mut mem = ram();
        start(&mut mem, 0, 1);
        note(&mut mem, 0, 2, 4, 0x0b2a_0010);
        // Flip every bit of two copies of the entry and of the header.
        for copy in [1, 4] {
            for w in 0..3 {
                let at = entry_offset(copy, 2, 4 % ENTRIES) + w * 8;
                let mut b = [0u8; 8];
                mem.read(at, &mut b);
                mem.write(at, &(!u64::from_le_bytes(b)).to_le_bytes());
            }
            mem.write(header_offset(copy), &0u64.to_le_bytes());
        }
        let t = Trace::read(&mem, 0).unwrap();
        assert_eq!(t.newest_first(2).0[0], Some(entry(4, 0x0b2a_0010)));
    }

    #[test]
    fn the_text_names_the_hung_access_first() {
        let mut mem = ram();
        start(&mut mem, 0, 1);
        note(&mut mem, 0, 0, 1, 0x1000);
        note(&mut mem, 0, 0, 2, 0xaf0_0040);
        let mut s = Text { buf: [0; 1024], len: 0 };
        // CPU 3's newest access was 1.5 ms before CPU 0's (counter at 19.2 MHz: 28800 ticks).
        note_at(&mut mem, 0, 3, 7, 0x2000, 1000 + 2 * 19_200 - 28_800);
        t_check(&mem);
        let t = Trace::read(&mem, 0).unwrap();
        t.write_text(&mut s, 3, 19_200_000, |loc| (loc == 0x1234_5678).then_some(("kernel/x.rs", 12))).unwrap();
        let text = s.as_str();
        assert!(text.contains("cpu 0: -0.000 ms w 0xaf00040/4=0x7 kernel/x.rs:12; -1.000 ms r 0x1000/4 kernel/x.rs:12"), "{text}");
        assert!(text.contains("cpu 3: -1.500 ms r 0x2000/4 kernel/x.rs:12"), "{text}");
        assert!(!text.contains("cpu 1"));
        let mut s = Text { buf: [0; 1024], len: 0 };
        Trace::read(&mem, 0).unwrap().write_text(&mut s, 1, 0, |_| None).unwrap();
        assert!(s.as_str().contains("cpu 0: w 0xaf00040/4=0x7 @0x12345678\n"), "{}", s.as_str());
    }

    struct Text {
        buf: [u8; 1024],
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
