//! A new generation boots on trial: what LucinaUEFI decides from the record on the EFI partition, and the record's bytes.
//!
//! The installer writes a record, with its tries, before it commits `BOOT.CFG`. The loader takes a try off before it starts the
//! kernel, so a crash, a hang and a hand power cycle all count. `lucina-trial` marks the generation good once the shell has lasted.
//! With no tries left the loader starts `PREV.CFG` instead, and never tries that generation again.
//!
//! The record lives in two files, each one block, each in a folder of its own so that writing one never rewrites the other's directory
//! entry. Every write goes to the older copy, so a power cut leaves the newer one whole.
//!
//! Verus proves [`decide`] and [`newest`] in this very file, not a copy of it (`verification/juno-disk/rules.rs`; the attributes are only
//! seen by Verus). What each promises is in its `ensures`. Not proved: the bytes on the disk and what the firmware does with a write.
#![allow(unexpected_cfgs)]

#[cfg(verus_keep_ghost)]
use vstd::prelude::*;

/// The two copies, from the root of the EFI partition.
pub const COPY_A: &str = "\\lucinaos\\trial-a\\RECORD";
pub const COPY_B: &str = "\\lucinaos\\trial-b\\RECORD";
/// How many boots a new generation gets.
pub const TRIES: u32 = 3;
/// One logical block: an overwrite in place never changes the file's size or its clusters.
pub const RECORD_BYTES: usize = 4096;

const MAGIC: &[u8; 8] = b"LUCTRIAL";
const VERSION: u32 = 1;

#[cfg_attr(verus_keep_ghost, verus_verify)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum State {
    /// Booting with [`Record::tries`] left.
    Trial,
    /// It lasted: no more counting.
    Good,
    /// Out of tries: never started again.
    Failed,
}

#[cfg_attr(verus_keep_ghost, verus_verify)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Record {
    /// Grows with every write; the larger of two whole copies is the record.
    pub seq: u32,
    /// The generation it is about: BOOT.CFG's, or the record means nothing.
    pub generation: u32,
    pub state: State,
    pub tries: u32,
    /// Boots left in a good generation's settle window (finding AB-3, docs/spec/juno-ab-boot.md). While it is open the loader takes a
    /// try off at every start, as on trial; a start that lasts gives the tries back and uses one boot of the window. Kept after the
    /// version-1 fields under its own CRC, so a loader from before it reads the record as before and this field as 0.
    pub settle: u32,
}

/// What to start.
#[cfg_attr(verus_keep_ghost, verus_verify)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Start {
    Current,
    Previous,
}

/// What the loader does: what it starts, and what it writes first.
#[cfg_attr(verus_keep_ghost, verus_verify)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Decision {
    pub start: Start,
    pub write: Option<Record>,
}

/// Which copy.
#[cfg_attr(verus_keep_ghost, verus_verify)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Copy {
    A,
    B,
}

/// The record, and the copy the next write goes to.
#[cfg_attr(verus_keep_ghost, verus_verify)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Newest {
    pub record: Option<Record>,
    pub write_to: Copy,
}

/// The newer of two copies, either of which may be damaged. The next write goes to the other one, so the newer is never overwritten.
#[cfg_attr(verus_keep_ghost, verus_spec(r =>
    ensures
        a is None && b is None ==> r.record is None,
        a matches Some(x) ==> (b is None ==> r.record == Some(x) && r.write_to == Copy::B),
        b matches Some(y) ==> (a is None ==> r.record == Some(y) && r.write_to == Copy::A),
        a matches Some(x) ==> (b matches Some(y) ==> (r.record matches Some(z) && z.seq >= x.seq && z.seq >= y.seq)),
        a matches Some(x) ==> (b matches Some(y) ==> (x.seq >= y.seq ==> r.write_to == Copy::B)),
        a matches Some(x) ==> (b matches Some(y) ==> (x.seq < y.seq ==> r.write_to == Copy::A)),
))]
pub fn newest(a: Option<Record>, b: Option<Record>) -> Newest {
    match (a, b) {
        (None, None) => Newest { record: None, write_to: Copy::A },
        (Some(x), None) => Newest { record: Some(x), write_to: Copy::B },
        (None, Some(y)) => Newest { record: Some(y), write_to: Copy::A },
        (Some(x), Some(y)) => {
            if x.seq >= y.seq {
                Newest { record: Some(x), write_to: Copy::B }
            } else {
                Newest { record: Some(y), write_to: Copy::A }
            }
        }
    }
}

#[cfg(verus_keep_ghost)]
verus! {

/// A generation that may not start again, while there is something else to start.
pub open spec fn must_not_start(record: Option<Record>, generation: u32, have_previous: bool) -> bool {
    &&& have_previous
    &&& record matches Some(r)
    &&& r.generation == generation
    &&& (r.state == State::Failed || (r.state == State::Trial && (r.tries == 0 || r.seq == u32::MAX))
        || (r.state == State::Good && r.settle > 0 && r.tries == 0))
}

} // verus!

/// What to do with `generation`, BOOT.CFG's. `record` is the newest copy, `have_previous` whether PREV.CFG can boot.
#[cfg_attr(verus_keep_ghost, verus_spec(d =>
    ensures
        // A record about something else, or a good generation: as if there were no trial, and nothing is written.
        record is None ==> d.start == Start::Current && d.write is None,
        record matches Some(r) ==> (r.generation != generation ==> d.start == Start::Current && d.write is None),
        record matches Some(r) ==> (r.generation == generation && r.state == State::Good && r.settle == 0 ==> d.start == Start::Current && d.write is None),
        // A good generation in its settle window: every start takes a try off, as on trial (AB-3).
        record matches Some(r) ==> (r.generation == generation && r.state == State::Good && r.settle > 0 && r.tries > 0 && r.seq < u32::MAX
            ==> (d.start == Start::Current && (d.write matches Some(w) && w.state == State::Good && w.tries == r.tries - 1
                && w.settle == r.settle && w.generation == generation && w.seq == r.seq + 1))),
        // Failed or out of tries, never again, as long as there is something else.
        must_not_start(record, generation, have_previous) ==> d.start == Start::Previous,
        // A try is counted before the kernel starts: every start of a generation on trial writes one try less.
        record matches Some(r) ==> (r.generation == generation && r.state == State::Trial && r.tries > 0 && r.seq < u32::MAX
            ==> (d.start == Start::Current && (d.write matches Some(w) && w.state == State::Trial && w.tries == r.tries - 1 && w.generation == generation && w.seq == r.seq + 1))),
        // Whatever is written is newer, and about this generation, and never adds a try.
        d.write matches Some(w) ==> (record matches Some(r) && w.seq > r.seq && w.generation == generation && w.tries <= r.tries),
))]
pub fn decide(record: Option<Record>, generation: u32, have_previous: bool) -> Decision {
    let Some(r) = record else {
        return Decision { start: Start::Current, write: None };
    };
    if r.generation != generation {
        return Decision { start: Start::Current, write: None };
    }
    let fallback = if have_previous { Start::Previous } else { Start::Current };
    match r.state {
        State::Good => {
            // Settled, or no window: as if there were no trial.
            if r.settle == 0 {
                return Decision { start: Start::Current, write: None };
            }
            if r.tries > 0 {
                if r.seq == u32::MAX {
                    return Decision { start: Start::Current, write: None };
                }
                Decision {
                    start: Start::Current,
                    write: Some(Record {
                        seq: r.seq + 1,
                        generation,
                        state: State::Good,
                        tries: r.tries - 1,
                        settle: r.settle,
                    }),
                }
            } else if have_previous {
                // Its starts in the window died before they lasted: back, and never again (AB-3).
                Decision {
                    start: Start::Previous,
                    write: if r.seq == u32::MAX {
                        None
                    } else {
                        Some(Record { seq: r.seq + 1, generation, state: State::Failed, tries: 0, settle: 0 })
                    },
                }
            } else {
                Decision { start: Start::Current, write: None }
            }
        }
        State::Failed => Decision { start: fallback, write: None },
        State::Trial => {
            if r.seq == u32::MAX {
                return Decision { start: fallback, write: None };
            }
            if r.tries > 0 {
                Decision {
                    start: Start::Current,
                    write: Some(Record {
                        seq: r.seq + 1,
                        generation,
                        state: State::Trial,
                        tries: r.tries - 1,
                        settle: 0,
                    }),
                }
            } else {
                Decision {
                    start: fallback,
                    write: Some(Record {
                        seq: r.seq + 1,
                        generation,
                        state: State::Failed,
                        tries: 0,
                        settle: 0,
                    }),
                }
            }
        }
    }
}

/// The good mark (`lucina-trial`, once the shell has lasted): the settle window opens, with [`SETTLE_BAD`] starts that may die before
/// they last, and [`SETTLE_BOOTS`] boots (AB-3).
#[cfg_attr(verus_keep_ghost, verus_spec(w =>
    requires r.seq < u32::MAX,
    ensures
        w.seq == r.seq + 1 && w.generation == r.generation && w.state == State::Good,
        w.tries == SETTLE_BAD && w.settle == SETTLE_BOOTS,
))]
pub fn mark_good(r: Record) -> Record {
    Record { seq: r.seq + 1, generation: r.generation, state: State::Good, tries: SETTLE_BAD, settle: SETTLE_BOOTS }
}

/// A start of `generation` that lasted (`lucina-trial`'s 60 s of the same shell): in the settle window its starts are full
/// again and one boot of the window is used. Nothing to write otherwise.
#[cfg_attr(verus_keep_ghost, verus_spec(w =>
    ensures
        w matches Some(x) ==> (r.generation == generation && r.state == State::Good && r.settle > 0 && r.seq < u32::MAX
            && x.seq == r.seq + 1 && x.generation == generation && x.state == State::Good
            && x.tries == SETTLE_BAD && x.settle == r.settle - 1),
        (r.generation == generation && r.state == State::Good && r.settle > 0 && r.seq < u32::MAX) ==> w is Some,
))]
pub fn lasted(r: Record, generation: u32) -> Option<Record> {
    // `match`, not `==`: Verus has no spec for a derived `PartialEq`.
    let good = match r.state {
        State::Good => true,
        _ => false,
    };
    if r.generation == generation && good && r.settle > 0 && r.seq < u32::MAX {
        Some(Record { seq: r.seq + 1, generation, state: State::Good, tries: SETTLE_BAD, settle: r.settle - 1 })
    } else {
        None
    }
}

/// How many boots of a good generation are watched, the one that marked it good included.
#[cfg_attr(verus_keep_ghost, verus_verify)]
pub const SETTLE_BOOTS: u32 = 3;
/// How many of those that ended badly (a crash, or a reset that left no record) send it back.
#[cfg_attr(verus_keep_ghost, verus_verify)]
pub const SETTLE_BAD: u32 = 2;

/// After the good mark: a generation that then crashes or resets over and over, past the 60 s the trial watches, goes back too.
/// Generation 32 (2026-11-05) was marked good and then vanished every 100 s; nothing rolled it back. Kept by `lucina-trial` on /data.
#[cfg_attr(verus_keep_ghost, verus_verify)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Settle {
    pub generation: u32,
    /// Boots seen since the good mark, that one included.
    pub boots: u32,
    /// How many of the earlier ones ended badly.
    pub bad: u32,
}

/// What a boot does with it.
#[cfg_attr(verus_keep_ghost, verus_verify)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SettleStep {
    pub next: Option<Settle>,
    pub roll_back: bool,
}

/// The boot that marks `generation` good starts the count.
#[cfg_attr(verus_keep_ghost, verus_spec(s =>
    ensures s.generation == generation && s.boots == 1 && s.bad == 0,
))]
pub fn settle_start(generation: u32) -> Settle {
    Settle { generation, boots: 1, bad: 0 }
}

/// A later boot of `generation`, BOOT.CFG's and good. `last_bad` says the boot before this one ended in a crash or a reset with no
/// record; `have_previous`, that PREV.CFG names something else that can boot.
#[cfg_attr(verus_keep_ghost, verus_spec(s =>
    ensures
        // A count about another generation, or none: nothing to do, and nothing kept.
        prev is None ==> s.next is None && !s.roll_back,
        prev matches Some(p) ==> (p.generation != generation ==> s.next is None && !s.roll_back),
        // The window is closed: unchanged, never sends anything back.
        prev matches Some(p) ==> (p.generation == generation && p.boots >= SETTLE_BOOTS ==> s.next == prev && !s.roll_back),
        // Inside it: one boot more, a bad one more if the last ended badly, and never fewer.
        prev matches Some(p) ==> (p.generation == generation && p.boots < SETTLE_BOOTS ==> (s.next matches Some(n)
            && n.generation == generation && n.boots == p.boots + 1 && n.bad == (if last_bad && p.bad < u32::MAX { (p.bad + 1) as u32 } else { p.bad }))),
        // Back only for this generation, inside the window, on a boot that follows a bad one, with enough bad ones, and somewhere to go.
        s.roll_back ==> (have_previous && last_bad && (prev matches Some(p) && p.generation == generation && p.boots < SETTLE_BOOTS)
            && (s.next matches Some(n) && n.bad >= SETTLE_BAD)),
        // And always then.
        prev matches Some(p) ==> (p.generation == generation && p.boots < SETTLE_BOOTS && have_previous && last_bad
            && p.bad + 1 >= SETTLE_BAD ==> s.roll_back),
))]
pub fn settle_step(prev: Option<Settle>, generation: u32, last_bad: bool, have_previous: bool) -> SettleStep {
    let Some(p) = prev else {
        return SettleStep { next: None, roll_back: false };
    };
    if p.generation != generation {
        return SettleStep { next: None, roll_back: false };
    }
    if p.boots >= SETTLE_BOOTS {
        return SettleStep { next: prev, roll_back: false };
    }
    // `boots` is below SETTLE_BOOTS here; `bad` comes from a file, so it stops at its largest value instead of wrapping.
    let bad = if last_bad && p.bad < u32::MAX { p.bad + 1 } else { p.bad };
    let next = Settle { generation, boots: p.boots + 1, bad };
    let roll_back = have_previous && last_bad && bad >= SETTLE_BAD;
    SettleStep { next: Some(next), roll_back }
}

pub(crate) fn crc32(data: &[u8]) -> u32 {
    let mut crc = !0u32;
    for &b in data {
        crc ^= b as u32;
        for _ in 0..8 {
            crc = if crc & 1 != 0 { (crc >> 1) ^ 0xedb8_8320 } else { crc >> 1 };
        }
    }
    !crc
}

const SIGNED: usize = 28;
/// After the version-1 record and its CRC: the settle window, and a CRC over everything before it.
const EXT: usize = 32;
const EXT_SIGNED: usize = 36;

impl Record {
    /// The block the file holds.
    pub fn encode(&self) -> [u8; RECORD_BYTES] {
        let mut b = [0u8; RECORD_BYTES];
        b[0..8].copy_from_slice(MAGIC);
        b[8..12].copy_from_slice(&VERSION.to_le_bytes());
        b[12..16].copy_from_slice(&self.seq.to_le_bytes());
        b[16..20].copy_from_slice(&self.generation.to_le_bytes());
        let state: u32 = match self.state {
            State::Trial => 1,
            State::Good => 2,
            State::Failed => 3,
        };
        b[20..24].copy_from_slice(&state.to_le_bytes());
        b[24..28].copy_from_slice(&self.tries.to_le_bytes());
        let crc = crc32(&b[..SIGNED]);
        b[SIGNED..SIGNED + 4].copy_from_slice(&crc.to_le_bytes());
        b[EXT..EXT + 4].copy_from_slice(&self.settle.to_le_bytes());
        let crc = crc32(&b[..EXT_SIGNED]);
        b[EXT_SIGNED..EXT_SIGNED + 4].copy_from_slice(&crc.to_le_bytes());
        b
    }

    /// `None` for anything but a whole record: a torn block, a missing file's zeros, another version.
    pub fn decode(b: &[u8]) -> Option<Record> {
        let word = |at: usize| -> Option<u32> { Some(u32::from_le_bytes(b.get(at..at + 4)?.try_into().ok()?)) };
        if b.get(..8)? != MAGIC || word(8)? != VERSION || word(SIGNED)? != crc32(b.get(..SIGNED)?) {
            return None;
        }
        let state = match word(20)? {
            1 => State::Trial,
            2 => State::Good,
            3 => State::Failed,
            _ => return None,
        };
        // The settle field counts only under its own CRC: a record from before it (zeros there) has no window.
        let settle = match (word(EXT), word(EXT_SIGNED), b.get(..EXT_SIGNED)) {
            (Some(s), Some(c), Some(signed)) if c == crc32(signed) => s,
            _ => 0,
        };
        Some(Record { seq: word(12)?, generation: word(16)?, state, tries: word(24)?, settle })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn trial(seq: u32, generation: u32, tries: u32) -> Record {
        Record { seq, generation, state: State::Trial, tries, settle: 0 }
    }

    #[test]
    fn three_tries_then_the_previous_one_for_good() {
        let mut record = trial(1, 16, TRIES);
        for left in [2, 1, 0] {
            let d = decide(Some(record), 16, true);
            assert_eq!(d.start, Start::Current);
            record = d.write.unwrap();
            assert_eq!(record.tries, left);
        }
        let d = decide(Some(record), 16, true);
        assert_eq!(d.start, Start::Previous);
        let failed = d.write.unwrap();
        assert_eq!(failed.state, State::Failed);
        // And it stays that way, with nothing more to write.
        assert_eq!(decide(Some(failed), 16, true), Decision { start: Start::Previous, write: None });
    }

    #[test]
    fn good_and_foreign_records_change_nothing() {
        let good = Record { seq: 9, generation: 16, state: State::Good, tries: 2, settle: 0 };
        let as_before = Decision { start: Start::Current, write: None };
        assert_eq!(decide(Some(good), 16, true), as_before);
        assert_eq!(decide(None, 16, true), as_before);
        // The installer's cut before its commit, and a rollback by hand.
        assert_eq!(decide(Some(trial(5, 17, 3)), 16, true), as_before);
    }

    #[test]
    fn with_nothing_to_go_back_to_it_still_boots() {
        let d = decide(Some(trial(5, 16, 0)), 16, false);
        assert_eq!(d.start, Start::Current);
        assert_eq!(d.write.unwrap().state, State::Failed);
        assert_eq!(decide(Some(Record { seq: 6, generation: 16, state: State::Failed, tries: 0, settle: 0 }), 16, false).start, Start::Current);
    }

    #[test]
    fn the_newest_copy_and_the_other_one_to_write() {
        let (old, new) = (trial(3, 16, 2), trial(4, 16, 1));
        assert_eq!(newest(Some(old), Some(new)), Newest { record: Some(new), write_to: Copy::A });
        assert_eq!(newest(Some(new), Some(old)), Newest { record: Some(new), write_to: Copy::B });
        assert_eq!(newest(None, Some(old)), Newest { record: Some(old), write_to: Copy::A });
        assert_eq!(newest(Some(old), None), Newest { record: Some(old), write_to: Copy::B });
        assert_eq!(newest(None, None).record, None);
    }

    #[test]
    fn a_power_cut_at_any_write_counts_or_not_but_never_gains_a_try() {
        // Copy A holds 3 tries. Each boot writes the older copy; a cut leaves that copy torn, or the old bytes.
        let mut a = Some(trial(1, 16, 3));
        let mut b = None;
        for _ in 0..3 {
            let n = newest(a, b);
            let w = decide(n.record, 16, true).write.unwrap();
            // Torn: the newer copy still holds the tries as they were, and the next boot writes the same copy again.
            let torn = None;
            let m = newest(if n.write_to == Copy::A { torn } else { a }, if n.write_to == Copy::B { torn } else { b });
            assert_eq!(m.record, n.record);
            assert_eq!(m.write_to, n.write_to);
            match n.write_to {
                Copy::A => a = Some(w),
                Copy::B => b = Some(w),
            }
            assert!(newest(a, b).record.unwrap().tries < n.record.unwrap().tries);
        }
    }

    #[test]
    fn the_bytes() {
        let r = Record { seq: 7, generation: 16, state: State::Failed, tries: 0, settle: 0 };
        let b = r.encode();
        assert_eq!(b.len(), RECORD_BYTES);
        assert_eq!(Record::decode(&b), Some(r));
        for at in [0, 9, 13, 17, 21, 25, 29] {
            let mut bad = b;
            bad[at] ^= 1;
            assert_eq!(Record::decode(&bad), None, "{at}");
        }
        assert_eq!(Record::decode(&[0u8; RECORD_BYTES]), None);
        assert_eq!(Record::decode(&b[..10]), None);
    }
    #[test]
    fn a_good_generation_that_keeps_vanishing_goes_back() {
        // Generation 32: marked good on its first boot, which then vanished, and so did the next.
        let s = settle_start(32);
        let two = settle_step(Some(s), 32, true, true);
        assert_eq!(two.next, Some(Settle { generation: 32, boots: 2, bad: 1 }));
        assert!(!two.roll_back);
        let three = settle_step(two.next, 32, true, true);
        assert_eq!(three.next, Some(Settle { generation: 32, boots: 3, bad: 2 }));
        assert!(three.roll_back);
    }

    #[test]
    fn clean_restarts_one_bad_boot_and_late_trouble_do_not_send_it_back() {
        let s = settle_start(7);
        let a = settle_step(Some(s), 7, false, true);
        let b = settle_step(a.next, 7, true, true);
        assert!(!a.roll_back && !b.roll_back);
        assert_eq!(b.next, Some(Settle { generation: 7, boots: 3, bad: 1 }));
        // The window is closed: later crashes are the crash screen's business, not the trial's.
        let c = settle_step(b.next, 7, true, true);
        assert_eq!(c.next, b.next);
        assert!(!c.roll_back);
    }

    #[test]
    fn nothing_to_go_back_to_or_another_generation_changes_nothing() {
        let s = settle_start(9);
        let a = settle_step(Some(s), 9, true, false);
        let b = settle_step(a.next, 9, true, false);
        assert!(!a.roll_back && !b.roll_back);
        assert_eq!(settle_step(Some(s), 10, true, true), SettleStep { next: None, roll_back: false });
        assert_eq!(settle_step(None, 9, true, true), SettleStep { next: None, roll_back: false });
        // A count read from a damaged file never wraps.
        let odd = Settle { generation: 9, boots: 1, bad: u32::MAX };
        let r = settle_step(Some(odd), 9, true, true);
        assert_eq!(r.next.map(|n| n.bad), Some(u32::MAX));
        assert!(r.roll_back);
    }
    /// AB-3: generation 15's night, a good generation that then dies in the kernel at every start, goes back.
    #[test]
    fn a_good_generation_that_dies_before_userspace_goes_back() {
        let good = mark_good(Record { seq: 4, generation: 15, state: State::Trial, tries: 1, settle: 0 });
        assert_eq!((good.state, good.tries, good.settle), (State::Good, SETTLE_BAD, SETTLE_BOOTS));
        let mut record = good;
        for _ in 0..SETTLE_BAD {
            let d = decide(Some(record), 15, true);
            assert_eq!(d.start, Start::Current);
            record = d.write.unwrap();
        }
        let d = decide(Some(record), 15, true);
        assert_eq!(d.start, Start::Previous);
        assert_eq!(d.write.unwrap().state, State::Failed);
        // With nowhere to go it keeps starting, and writes nothing.
        assert_eq!(decide(Some(record), 15, false), Decision { start: Start::Current, write: None });
    }

    #[test]
    fn a_start_that_lasts_gives_the_tries_back_and_closes_the_window() {
        let mut record = mark_good(Record { seq: 1, generation: 20, state: State::Trial, tries: 2, settle: 0 });
        for left in (0..SETTLE_BOOTS).rev() {
            record = decide(Some(record), 20, true).write.unwrap();
            record = lasted(record, 20).unwrap();
            assert_eq!((record.tries, record.settle), (SETTLE_BAD, left));
        }
        // Settled: no more counting.
        assert_eq!(decide(Some(record), 20, true), Decision { start: Start::Current, write: None });
        assert_eq!(lasted(record, 20), None);
        assert_eq!(lasted(record, 21), None);
    }

    #[test]
    fn the_settle_field_round_trips_and_old_records_read_as_settled() {
        let r = Record { seq: 3, generation: 41, state: State::Good, tries: 1, settle: 2 };
        assert_eq!(Record::decode(&r.encode()), Some(r));
        // A version-1 record, as an older lucina-trial or installer wrote it: zeros after the first CRC.
        let mut old = r.encode();
        old[EXT..EXT + 8].fill(0);
        assert_eq!(Record::decode(&old), Some(Record { settle: 0, ..r }));
        // A torn settle field is ignored, not trusted.
        let mut torn = r.encode();
        torn[EXT] ^= 1;
        assert_eq!(Record::decode(&torn).map(|x| x.settle), Some(0));
    }
}
