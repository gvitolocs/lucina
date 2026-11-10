//! The loader's marker around the firmware's `GetRNG`: a firmware that hangs or faults in that call costs one restart, and the
//! machine never makes that call again. The firmware's read loop has no time limit, and this is the one firmware service a boot
//! needs that nothing but this boot ever exercised.
//!
//! Before the call a record says `Calling`; after it returns, whatever it returned, `Clear`. A boot that finds `Calling` knows the
//! call before it never came back: it writes `Skip`, and from then on the seed comes from the kernel's own hardware read instead.
//! A power cut in that window looks the same on the disk and costs the same.
//!
//! The record follows the trial's rules (`trial.rs`): two copies, each one block, each in a folder of its own, overwritten in place
//! so that neither the file's size nor its clusters nor a directory entry changes, every write going to the older copy, so that a
//! torn write leaves the newer copy whole. **No record that can be read means no call**, and neither does a marker that did not
//! reach the disk. The loader never creates these files and never writes UEFI variables; the installer makes the files.
//!
//! Verus proves [`newest`], [`plan`] and [`after_call`] in this very file, not a copy of it (`verification/juno-random/rules.rs`).
//! Not proved: that the firmware's flush reached the medium, and that the firmware's watchdog fires.
#![allow(unexpected_cfgs)]

#[cfg(verus_keep_ghost)]
use vstd::prelude::*;

/// The two copies, from the root of the EFI partition.
pub const COPY_A: &str = "\\lucinaos\\rng-a\\RECORD";
pub const COPY_B: &str = "\\lucinaos\\rng-b\\RECORD";
/// One logical block, as the trial's.
pub const RECORD_BYTES: usize = 4096;

const MAGIC: &[u8; 8] = b"LUCRNGMK";
const VERSION: u32 = 1;

#[cfg_attr(verus_keep_ghost, verus_verify)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum State {
    /// The last call returned, or there has been none.
    Clear,
    /// The call is about to be made, or is being made.
    Calling,
    /// A call never came back: no more calls.
    Skip,
}

#[cfg_attr(verus_keep_ghost, verus_verify)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Record {
    /// Grows with every write; the larger of two whole copies is the record.
    pub seq: u32,
    pub state: State,
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

/// Why there is no call on this boot.
#[cfg_attr(verus_keep_ghost, verus_verify)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Why {
    /// Neither copy reads: never installed, or damaged.
    NoRecord,
    /// The record said `Calling`: the last call did not come back.
    DidNotReturn,
    /// The record says `Skip`.
    SkippedBefore,
    /// The sequence number has no room left.
    OutOfNumbers,
}

/// What this boot does.
#[cfg_attr(verus_keep_ghost, verus_verify)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Plan {
    /// Write this record, and make the call only if it reached the disk.
    Call(Record),
    /// No call; write this, if anything.
    NoCall(Why, Option<Record>),
}

/// The newer of two copies, either of which may be damaged. The next write goes to the other one, so the newer is never overwritten.
#[cfg_attr(verus_keep_ghost, verus_spec(r =>
    ensures
        a is None && b is None ==> r.record is None,
        a matches Some(x) ==> (b is None ==> r.record == Some(x) && r.write_to == Copy::B),
        b matches Some(y) ==> (a is None ==> r.record == Some(y) && r.write_to == Copy::A),
        a matches Some(x) ==> (b matches Some(y) ==> (x.seq >= y.seq ==> r.record == Some(x) && r.write_to == Copy::B)),
        a matches Some(x) ==> (b matches Some(y) ==> (x.seq < y.seq ==> r.record == Some(y) && r.write_to == Copy::A)),
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

/// What to do, from the newest record. **Only a record that says `Clear` leads to a call**; `Calling` and `Skip` lead to `Skip` for
/// good, and nothing here ever turns them back.
#[cfg_attr(verus_keep_ghost, verus_spec(p =>
    ensures
        // A call only from Clear, with a marker that says so and is newer, so that it is what the next boot reads.
        p matches Plan::Call(m) ==> (record matches Some(r) && r.state == State::Clear && m.state == State::Calling && m.seq > r.seq),
        // No record, no call and nothing written.
        record is None ==> p == Plan::NoCall(Why::NoRecord, None),
        // A call that did not come back: never again, whatever the state said before.
        record matches Some(r) ==> (r.state == State::Calling && r.seq < u32::MAX
            ==> (p matches Plan::NoCall(w, Some(n)) && w == Why::DidNotReturn && n.state == State::Skip && n.seq > r.seq)),
        record matches Some(r) ==> (r.state == State::Calling ==> !(p is Call)),
        record matches Some(r) ==> (r.state == State::Skip ==> p == Plan::NoCall(Why::SkippedBefore, None)),
        // Whatever is written is newer, and only a call's marker comes from a Clear.
        p matches Plan::NoCall(_, Some(n)) ==> (n.state == State::Skip && (record matches Some(r) && n.seq > r.seq)),
))]
pub fn plan(record: Option<Record>) -> Plan {
    let Some(r) = record else {
        return Plan::NoCall(Why::NoRecord, None);
    };
    match r.state {
        State::Clear => {
            if r.seq < u32::MAX - 1 {
                Plan::Call(Record { seq: r.seq + 1, state: State::Calling })
            } else {
                Plan::NoCall(Why::OutOfNumbers, None)
            }
        }
        State::Calling => {
            if r.seq < u32::MAX {
                Plan::NoCall(Why::DidNotReturn, Some(Record { seq: r.seq + 1, state: State::Skip }))
            } else {
                Plan::NoCall(Why::DidNotReturn, None)
            }
        }
        State::Skip => Plan::NoCall(Why::SkippedBefore, None),
    }
}

/// What to write when the call came back, whatever it returned.
#[cfg_attr(verus_keep_ghost, verus_spec(r =>
    requires
        marker.seq < u32::MAX,
    ensures
        r.state == State::Clear,
        r.seq > marker.seq,
))]
pub fn after_call(marker: Record) -> Record {
    Record { seq: marker.seq + 1, state: State::Clear }
}

/// The first record the installer writes.
pub const FIRST: Record = Record { seq: 1, state: State::Clear };

/// The other copy: the one to write after a write to `copy`.
pub fn other(copy: Copy) -> Copy {
    match copy {
        Copy::A => Copy::B,
        Copy::B => Copy::A,
    }
}

pub fn path(copy: Copy) -> &'static str {
    match copy {
        Copy::A => COPY_A,
        Copy::B => COPY_B,
    }
}

const SIGNED: usize = 20;

impl Record {
    /// The block the file holds.
    pub fn encode(&self) -> [u8; RECORD_BYTES] {
        let mut b = [0u8; RECORD_BYTES];
        b[0..8].copy_from_slice(MAGIC);
        b[8..12].copy_from_slice(&VERSION.to_le_bytes());
        b[12..16].copy_from_slice(&self.seq.to_le_bytes());
        let state: u32 = match self.state {
            State::Clear => 1,
            State::Calling => 2,
            State::Skip => 3,
        };
        b[16..20].copy_from_slice(&state.to_le_bytes());
        let crc = crate::trial::crc32(&b[..SIGNED]);
        b[SIGNED..SIGNED + 4].copy_from_slice(&crc.to_le_bytes());
        b
    }

    /// `None` for anything but a whole record: a torn block, a missing file's zeros, another version.
    pub fn decode(b: &[u8]) -> Option<Record> {
        let word = |at: usize| -> Option<u32> { Some(u32::from_le_bytes(b.get(at..at + 4)?.try_into().ok()?)) };
        if b.get(..8)? != MAGIC || word(8)? != VERSION || word(SIGNED)? != crate::trial::crc32(b.get(..SIGNED)?) {
            return None;
        }
        let state = match word(16)? {
            1 => State::Clear,
            2 => State::Calling,
            3 => State::Skip,
            _ => return None,
        };
        Some(Record { seq: word(12)?, state })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The disk: two copies; a write goes to the older one.
    #[derive(Clone, Copy)]
    struct Disk {
        a: Option<Record>,
        b: Option<Record>,
    }

    impl Disk {
        fn put(&mut self, copy: Copy, r: Option<Record>) {
            match copy {
                Copy::A => self.a = r,
                Copy::B => self.b = r,
            }
        }
    }

    /// One boot whose call returns. Returns whether it called.
    fn boot_returning(d: &mut Disk) -> bool {
        let n = newest(d.a, d.b);
        match plan(n.record) {
            Plan::Call(m) => {
                d.put(n.write_to, Some(m));
                d.put(other(n.write_to), Some(after_call(m)));
                true
            }
            Plan::NoCall(_, write) => {
                if let Some(w) = write {
                    d.put(n.write_to, Some(w));
                }
                false
            }
        }
    }

    fn installed() -> Disk {
        Disk { a: Some(FIRST), b: None }
    }

    #[test]
    fn a_call_that_returns_leaves_the_next_boot_free_to_call() {
        let mut d = installed();
        for _ in 0..20 {
            assert!(boot_returning(&mut d));
        }
    }

    #[test]
    fn a_call_that_never_returns_is_never_made_again() {
        let mut d = installed();
        let n = newest(d.a, d.b);
        let Plan::Call(m) = plan(n.record) else { panic!() };
        d.put(n.write_to, Some(m));
        // It hangs here. The restart: no call, a Skip goes down, and no later boot calls either.
        for _ in 0..5 {
            assert!(!boot_returning(&mut d));
        }
        assert_eq!(newest(d.a, d.b).record.unwrap().state, State::Skip);
    }

    #[test]
    fn a_cut_at_any_write_never_leads_to_a_second_call_after_a_marker_reached_the_disk() {
        // A torn marker: the old Clear stays newest and the call was not made, so the next boot may make it.
        let mut d = installed();
        let n = newest(d.a, d.b);
        d.put(n.write_to, None);
        assert_eq!(newest(d.a, d.b).record, Some(FIRST));
        assert!(boot_returning(&mut d));

        // A whole marker and a torn Clear: the marker is still newest, so no more calls.
        let mut d = installed();
        let n = newest(d.a, d.b);
        let Plan::Call(m) = plan(n.record) else { panic!() };
        d.put(n.write_to, Some(m));
        d.put(other(n.write_to), None);
        assert!(!boot_returning(&mut d));

        // A torn Skip: Calling is still newest, and the next boot writes Skip again.
        let mut d = Disk { a: Some(Record { seq: 5, state: State::Calling }), b: Some(Record { seq: 4, state: State::Clear }) };
        let n = newest(d.a, d.b);
        d.put(n.write_to, None);
        assert_eq!(plan(newest(d.a, d.b).record), Plan::NoCall(Why::DidNotReturn, Some(Record { seq: 6, state: State::Skip })));
    }

    #[test]
    fn no_record_no_call_and_nothing_written() {
        assert_eq!(plan(None), Plan::NoCall(Why::NoRecord, None));
        let mut d = Disk { a: None, b: None };
        assert!(!boot_returning(&mut d));
        assert_eq!(d.a, None);
    }

    #[test]
    fn the_numbers_cannot_wrap_into_a_call() {
        assert_eq!(plan(Some(Record { seq: u32::MAX - 1, state: State::Clear })), Plan::NoCall(Why::OutOfNumbers, None));
        assert_eq!(plan(Some(Record { seq: u32::MAX, state: State::Calling })), Plan::NoCall(Why::DidNotReturn, None));
    }

    #[test]
    fn the_bytes() {
        for state in [State::Clear, State::Calling, State::Skip] {
            let r = Record { seq: 7, state };
            assert_eq!(Record::decode(&r.encode()), Some(r));
        }
        let b = FIRST.encode();
        assert_eq!(b.len(), RECORD_BYTES);
        for at in [0, 9, 13, 17, 21] {
            let mut bad = b;
            bad[at] ^= 1;
            assert_eq!(Record::decode(&bad), None, "{at}");
        }
        assert_eq!(Record::decode(&[0u8; RECORD_BYTES]), None);
        assert_eq!(Record::decode(&b[..10]), None);
        // A trial record is not one of these, or the other way round.
        let trial = crate::trial::Record { seq: 1, generation: 1, state: crate::trial::State::Good, tries: 0, settle: 0 };
        assert_eq!(Record::decode(&trial.encode()), None);
        assert_eq!(crate::trial::Record::decode(&b), None);
    }
}
