// SPDX-License-Identifier: MPL-2.0

//! The queue between a touch frame and the reader of `/dev/lucina-gesture`: [`Queue`], and the tests of what it holds.
//!
//! [`QUEUE_LEN`] records is about a second and a half of a moving finger, so a reader that wakes a little late loses
//! nothing. One that stays asleep is what the folding is for: the queue never grows past its cap, and what it gives up is
//! motion, never a gesture's begin or its end.
//!
//! One merge operation does all the folding, [`Queue::merge_into`]: `dx`, `dy` and `angle` add with saturation, and the
//! scale, the time and [`FLAG_MERGED`] come from the record being folded in. Two places use it. An incoming Update that
//! cannot be queued joins the newest queued Update of its own gesture. And when a Begin, an End or an Update of another
//! gesture has to have room made, the oldest queued Update of any gesture joins the next queued record of its own gesture,
//! or is lost if nothing of that gesture follows it — in both cases the queue is one record shorter and the new record fits.

use crate::{FLAG_MERGED, GestureRecord, Kind, Phase, QUEUE_LEN, SCALE_ONE};

/// A record for the slots that hold nothing: the array is filled on creation and `len` bounds every read, so a reader
/// never sees this.
const FILLER: GestureRecord = GestureRecord {
    time_us: 0,
    kind: Kind::Swipe,
    phase: Phase::Begin,
    fingers: 1,
    flags: 0,
    dx: 0,
    dy: 0,
    scale: SCALE_ONE,
    angle: 0,
    seq: 0,
};

/// [`QUEUE_LEN`] is a power of two, so the ring's index is a mask rather than a division.
const MASK: usize = QUEUE_LEN - 1;

/// The records the driver has written and the reader has not taken, oldest first, at most [`QUEUE_LEN`] of them.
pub struct Queue {
    slots: [GestureRecord; QUEUE_LEN],
    /// Where the oldest record is.
    head: usize,
    /// How many there are, from `head` forward.
    len: usize,
}

/// So that a driver can hold a `Queue` in a struct that derives `Default`, as an empty queue is the only state there is.
impl Default for Queue {
    fn default() -> Queue {
        Queue::new()
    }
}

impl Queue {
    /// An empty queue, ready for the driver's first record.
    pub const fn new() -> Queue {
        Queue {
            slots: [FILLER; QUEUE_LEN],
            head: 0,
            len: 0,
        }
    }

    /// How many records are waiting for the reader.
    pub fn len(&self) -> usize {
        self.len
    }

    /// Whether the reader has taken everything the driver wrote.
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Queue `r`. With room to spare it is appended, and nothing is ever folded while the queue still has room.
    ///
    /// Full, an Update of a gesture the queue already holds is folded into the newest queued Update of that gesture, so a
    /// stream the reader cannot keep up with costs records, not motion. Any other record has room made for it by giving up
    /// the oldest queued Update, of any gesture; see the module docs for what happens to that record.
    ///
    /// Room can therefore only fail to be made when the queue holds nothing but Begins and Ends, which a driver with one
    /// open gesture at a time does not reach, but a sleeping reader and a quick finger do: 64 gestures that all closed
    /// inside 128 records. Then `push` changes nothing and returns false, and the driver drops the record on the floor.
    ///
    /// True when `r`, or its merge, is now in the queue.
    pub fn push(&mut self, r: GestureRecord) -> bool {
        if self.len < QUEUE_LEN {
            self.put(self.len, r);
            self.len += 1;
            return true;
        }
        if r.phase == Phase::Update
            && let Some(i) = self.newest_update(r.seq)
        {
            self.merge_into(i, r);
            return true;
        }
        if !self.give_up_oldest_update() {
            return false;
        }
        self.put(self.len, r);
        self.len += 1;
        true
    }

    /// The oldest record, which is what the reader waiting at the device gets next. None when the queue is empty.
    pub fn pop(&mut self) -> Option<GestureRecord> {
        if self.len == 0 {
            return None;
        }
        let r = self.slots[self.head];
        self.head = (self.head + 1) & MASK;
        self.len -= 1;
        Some(r)
    }

    /// Forget every queued record: what the driver does when the reader closes the device, because nothing is queued
    /// while it is closed and a gesture that ended before the close must not open the next reader's stream.
    pub fn clear(&mut self) {
        self.head = 0;
        self.len = 0;
    }

    /// The index into `slots` of the `i`th queued record, `i` counting from the oldest.
    fn at(&self, i: usize) -> usize {
        (self.head + i) & MASK
    }

    fn get(&self, i: usize) -> GestureRecord {
        self.slots[self.at(i)]
    }

    fn put(&mut self, i: usize, r: GestureRecord) {
        self.slots[self.at(i)] = r;
    }

    /// The newest queued Update of gesture `seq`, counting from the oldest record, or None when the queue holds no
    /// update of that gesture.
    fn newest_update(&self, seq: u32) -> Option<usize> {
        (0..self.len).rev().find(|&i| {
            let r = self.get(i);
            r.phase == Phase::Update && r.seq == seq
        })
    }

    /// The oldest queued Update, of any gesture, counting from the oldest record, or None when the queue holds only
    /// Begins and Ends and so has nothing left to give up.
    fn oldest_update(&self) -> Option<usize> {
        (0..self.len).find(|&i| self.get(i).phase == Phase::Update)
    }

    /// Free one slot by giving up [`oldest_update`](Self::oldest_update): its motion joins the next queued record of its
    /// gesture when that is also an Update; when something else follows — its End, or the next gesture's Begin — the
    /// Update is lost and whatever follows it in its own gesture carries [`FLAG_MERGED`] to say a record is missing. False
    /// when there was no Update to give up and so no slot to free.
    fn give_up_oldest_update(&mut self) -> bool {
        let Some(i) = self.oldest_update() else {
            return false;
        };
        let u = self.get(i);
        // The records of one gesture stay together in the queue, so the next record of this gesture is the next record,
        // and there is none when the gesture has already had its last record queued here.
        let next = if i + 1 < self.len && self.get(i + 1).seq == u.seq {
            Some(i + 1)
        } else {
            None
        };
        match next {
            Some(j) if self.get(j).phase == Phase::Update => self.merge_into(j, u),
            Some(j) => self.mark_merged(j),
            None => {}
        }
        self.remove(i);
        true
    }

    /// Fold `src` into the queued record at `i`, which stands for the same gesture: the motion adds with saturation, the
    /// scale and the time become `src`'s, and [`FLAG_MERGED`] is set.
    fn merge_into(&mut self, i: usize, src: GestureRecord) {
        let mut d = self.get(i);
        d.dx = d.dx.saturating_add(src.dx);
        d.dy = d.dy.saturating_add(src.dy);
        d.angle = d.angle.saturating_add(src.angle);
        d.scale = src.scale;
        d.time_us = src.time_us;
        d.flags |= FLAG_MERGED;
        self.put(i, d);
    }

    /// Say of the record at `i` that a record of its gesture is not in the queue.
    fn mark_merged(&mut self, i: usize) {
        let mut r = self.get(i);
        r.flags |= FLAG_MERGED;
        self.put(i, r);
    }

    /// Take the record at `i` out of the queue, keeping the order of the rest.
    fn remove(&mut self, i: usize) {
        if i == 0 {
            self.head = (self.head + 1) & MASK;
        } else {
            for j in i..self.len - 1 {
                let r = self.get(j + 1);
                self.put(j, r);
            }
        }
        self.len -= 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::FLAG_CANCELLED;

    /// A record of gesture `seq` in `phase`, with no motion.
    fn rec(seq: u32, phase: Phase) -> GestureRecord {
        GestureRecord {
            time_us: 1000 + seq as u64,
            kind: Kind::Swipe,
            phase,
            fingers: 2,
            flags: 0,
            dx: 0,
            dy: 0,
            scale: SCALE_ONE,
            angle: 0,
            seq,
        }
    }

    /// An Update of gesture `seq`: `dx` in both axes, `scale`, `angle`, at `time_us`.
    fn upd(seq: u32, time_us: u64, dx: i32, scale: i32, angle: i32) -> GestureRecord {
        GestureRecord {
            time_us,
            dx,
            dy: dx,
            scale,
            angle,
            ..rec(seq, Phase::Update)
        }
    }

    fn drain(q: &mut Queue) -> Vec<GestureRecord> {
        let mut v = Vec::new();
        while let Some(r) = q.pop() {
            v.push(r);
        }
        v
    }

    fn merged(q: &[GestureRecord]) -> usize {
        q.iter().filter(|r| r.flags & FLAG_MERGED != 0).count()
    }

    #[test]
    fn records_come_out_as_they_went_in() {
        let mut q = Queue::new();
        assert_eq!(q.len(), 0);
        assert!(q.is_empty());
        assert_eq!(q.pop(), None, "an empty queue has nothing to give");
        let sent: Vec<GestureRecord> = vec![
            rec(1, Phase::Begin),
            upd(1, 11, 3, SCALE_ONE + 3, 1),
            rec(1, Phase::End),
            rec(2, Phase::Begin),
            upd(2, 12, -4, SCALE_ONE - 4, -1),
        ];
        for r in &sent {
            assert!(q.push(*r));
        }
        assert_eq!(q.len(), sent.len());
        assert_eq!(drain(&mut q), sent);
        assert!(q.is_empty());
        assert_eq!(q.pop(), None);
    }

    /// Full, an Update of a gesture the queue holds joins that gesture's newest queued Update: the motion of the whole
    /// stream survives, no record is dropped, and nothing older is touched.
    #[test]
    fn a_full_queue_folds_an_update_into_the_newest_of_its_gesture() {
        let mut q = Queue::new();
        for t in 0..QUEUE_LEN as u64 {
            assert!(q.push(upd(7, t, 1, SCALE_ONE + t as i32, 2)));
        }
        assert_eq!(q.len(), QUEUE_LEN);
        assert!(q.push(upd(7, 9999, 5, -7, 7)));
        assert_eq!(q.len(), QUEUE_LEN, "a fold queues nothing new");
        let got = drain(&mut q);
        let last = got[QUEUE_LEN - 1];
        assert_eq!((last.seq, last.phase), (7, Phase::Update));
        assert_eq!(last.dx, 6, "1 + 5");
        assert_eq!(last.dy, 6);
        assert_eq!(last.angle, 9, "2 + 7");
        assert_eq!(last.scale, -7, "the new record's scale");
        assert_eq!(last.time_us, 9999, "the new record's time");
        assert_eq!(last.flags, FLAG_MERGED);
        // the newest update of the gesture, not an older one: its neighbour is untouched.
        assert_eq!(
            got[QUEUE_LEN - 2],
            upd(
                7,
                QUEUE_LEN as u64 - 2,
                1,
                SCALE_ONE + QUEUE_LEN as i32 - 2,
                2
            )
        );
        assert_eq!(merged(&got), 1);
    }

    #[test]
    fn a_fold_saturates_rather_than_wrapping() {
        for (dx, add, want) in [(i32::MAX, 1, i32::MAX), (i32::MIN, -1, i32::MIN)] {
            let mut q = Queue::new();
            for _ in 0..QUEUE_LEN {
                assert!(q.push(upd(9, 1, dx, SCALE_ONE, dx)));
            }
            assert!(q.push(upd(9, 2, add, SCALE_ONE + add, add)));
            let last = drain(&mut q).pop().unwrap();
            assert_eq!(last.dx, want);
            assert_eq!(last.dy, want);
            assert_eq!(last.angle, want);
            assert_eq!(last.scale, SCALE_ONE + add, "a replaced value does not add");
            assert_eq!(last.time_us, 2);
        }
    }

    /// A Begin into a full queue of updates pays for its place with the oldest update, whose motion goes to the next
    /// record of its own gesture; the rest of the queue keeps its order.
    #[test]
    fn a_begin_makes_room_with_the_oldest_update_and_keeps_the_order() {
        let mut q = Queue::new();
        for seq in 1..=4u32 {
            for t in 0..32u64 {
                assert!(q.push(upd(seq, t, 1, SCALE_ONE + t as i32, 2)));
            }
        }
        assert_eq!(q.len(), QUEUE_LEN);
        assert!(
            q.push(rec(5, Phase::Begin)),
            "a Begin was refused though the queue held updates"
        );
        assert_eq!(q.len(), QUEUE_LEN);
        let got = drain(&mut q);
        assert_eq!(got.len(), QUEUE_LEN);
        let first = got[0];
        assert_eq!((first.seq, first.phase), (1, Phase::Update));
        assert_eq!(first.dx, 2, "the oldest update's 1 plus the next one's 1");
        assert_eq!(first.dy, 2);
        assert_eq!(first.angle, 4);
        assert_eq!(
            first.scale, SCALE_ONE,
            "the folded record's: 65536 + 0, the older of the two"
        );
        assert_eq!(first.time_us, 0, "and the same for its time");
        assert_eq!(first.flags, FLAG_MERGED);
        assert_eq!(merged(&got), 1);
        let seqs: Vec<u32> = got.iter().map(|r| r.seq).collect();
        let mut want: Vec<u32> = vec![1; 31];
        for s in 2..5u32 {
            want.extend(core::iter::repeat_n(s, 32));
        }
        want.push(5);
        assert_eq!(seqs, want, "FIFO, and the Begin last");
        assert_eq!(got[QUEUE_LEN - 1], rec(5, Phase::Begin));
    }

    /// The oldest update of a gesture whose end is already queued cannot fold into it: it is lost, and the end says so.
    #[test]
    fn an_update_that_cannot_fold_marks_what_follows_it_in_its_gesture() {
        let mut q = Queue::new();
        assert!(q.push(upd(1, 10, 5, SCALE_ONE + 5, 3)));
        assert!(q.push(rec(1, Phase::End)));
        while q.len() < QUEUE_LEN {
            let t = q.len() as u64;
            assert!(q.push(upd(2, t, 1, SCALE_ONE, 1)));
        }
        assert!(q.push(rec(3, Phase::Begin)));
        assert_eq!(q.len(), QUEUE_LEN);
        let got = drain(&mut q);
        assert_eq!(
            got[0],
            GestureRecord {
                flags: FLAG_MERGED,
                ..rec(1, Phase::End)
            },
            "the end is kept, and flagged"
        );
        assert_eq!(got[1].seq, 2);
        assert_eq!(got[QUEUE_LEN - 1], rec(3, Phase::Begin));
        assert_eq!(
            got.iter()
                .filter(|r| r.seq == 1 && r.phase == Phase::Update)
                .count(),
            0,
            "its motion is gone"
        );
        assert_eq!(merged(&got), 1);
    }

    /// The oldest update of a gesture with nothing of it left in the queue is lost without a trace but its own: there is
    /// no later record of that gesture to flag.
    #[test]
    fn an_update_with_nothing_of_its_gesture_after_it_is_simply_lost() {
        let mut q = Queue::new();
        assert!(q.push(upd(1, 10, 5, SCALE_ONE, 5)));
        assert!(q.push(rec(2, Phase::Begin)));
        while q.len() < QUEUE_LEN {
            assert!(q.push(upd(2, 20, 1, SCALE_ONE, 1)));
        }
        assert!(
            q.push(upd(3, 30, 7, SCALE_ONE + 7, 7)),
            "an update of another gesture folds into nothing"
        );
        assert_eq!(q.len(), QUEUE_LEN);
        let got = drain(&mut q);
        assert_eq!(
            got[0],
            rec(2, Phase::Begin),
            "the dropped update was the oldest record"
        );
        assert_eq!(got.iter().filter(|r| r.seq == 1).count(), 0);
        assert_eq!(
            merged(&got),
            0,
            "nothing was folded, so nothing claims to be a fold"
        );
        assert_eq!(got[QUEUE_LEN - 1], upd(3, 30, 7, SCALE_ONE + 7, 7));
    }

    #[test]
    fn an_end_is_never_lost_and_keeps_its_place_last() {
        let mut q = Queue::new();
        for t in 0..QUEUE_LEN as u64 {
            assert!(q.push(upd(7, t, 1, SCALE_ONE + t as i32, 1)));
        }
        let end = GestureRecord {
            flags: FLAG_CANCELLED,
            ..rec(7, Phase::End)
        };
        assert!(
            q.push(end),
            "an End was refused though the queue held updates"
        );
        let got = drain(&mut q);
        assert_eq!(got.len(), QUEUE_LEN);
        assert_eq!(
            got[QUEUE_LEN - 1],
            end,
            "the End itself, cancelled as it was written"
        );
        assert_eq!(got.iter().filter(|r| r.phase == Phase::End).count(), 1);
        assert_eq!(
            got[0].dx, 2,
            "the End paid with the oldest update, into the next one"
        );
        assert_eq!(got[0].flags, FLAG_MERGED);
        assert_eq!(merged(&got), 1);
    }

    /// The queue full of nothing but Begins and Ends has no record left to give up: 64 closed gestures fill it exactly,
    /// and then even an End is refused — with nothing replaced.
    #[test]
    fn a_queue_of_only_begins_and_ends_refuses_without_replacing_anything() {
        let mut q = Queue::new();
        for seq in 1..=64u32 {
            assert!(q.push(rec(seq, Phase::Begin)));
            assert!(q.push(GestureRecord {
                flags: FLAG_CANCELLED,
                ..rec(seq, Phase::End)
            }));
        }
        assert_eq!(q.len(), QUEUE_LEN);
        let want: Vec<GestureRecord> = (1..=64u32)
            .flat_map(|seq| {
                [
                    rec(seq, Phase::Begin),
                    GestureRecord {
                        flags: FLAG_CANCELLED,
                        ..rec(seq, Phase::End)
                    },
                ]
            })
            .collect();
        assert_eq!(want.len(), QUEUE_LEN);
        for r in [
            upd(65, 9, 1, SCALE_ONE, 1),
            rec(65, Phase::Begin),
            GestureRecord {
                flags: FLAG_CANCELLED,
                ..rec(65, Phase::End)
            },
        ] {
            assert!(!q.push(r), "{r:?} should have been refused");
            assert_eq!(q.len(), QUEUE_LEN);
        }
        assert_eq!(drain(&mut q), want, "and the queue is exactly what it was");
        assert_eq!(merged(&want), 0);
    }

    #[test]
    fn clear_empties_and_the_queue_runs_from_new() {
        let mut q = Queue::new();
        for t in 0..QUEUE_LEN as u64 {
            assert!(q.push(upd(1, t, 1, SCALE_ONE, 1)));
        }
        assert!(q.push(rec(1, Phase::End)));
        assert_eq!(q.len(), QUEUE_LEN);
        q.clear();
        assert!(q.is_empty());
        assert_eq!(q.len(), 0);
        assert_eq!(q.pop(), None, "and nothing came back");
        // From new again: the first records are appended, not folded, and come out in the order they went in, with the
        // ring's head back where the writer starts after the push that filled the queue.
        let sent = vec![
            rec(2, Phase::Begin),
            upd(2, 5, 3, SCALE_ONE + 3, 0),
            rec(2, Phase::End),
        ];
        for r in &sent {
            assert!(q.push(*r));
        }
        assert_eq!(q.len(), sent.len());
        assert_eq!(drain(&mut q), sent);
    }

    /// Knuth's 64-bit linear congruential generator: no dependencies, and the shift keeps its better high bits.
    fn rand(seed: &mut u64) -> u64 {
        *seed = seed
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        *seed >> 11
    }

    /// What one random run of the queue saw.
    struct Tally {
        sent_begin: u32,
        sent_update: u32,
        sent_end: u32,
        took_begin: u32,
        took_update: u32,
        took_end: u32,
        refused: u32,
        merged: u32,
        full: u32,
        /// The gesture the records popped last make up, and the shape they have to keep: a record of another gesture
        /// opens a new run, and within a run the phase only moves forward, once at its ends.
        run_seq: u32,
        run_rank: u8,
        run_begin: u8,
        run_end: u8,
    }

    /// Take one record out, if there is one, and check the order it arrived in.
    fn took(q: &mut Queue, t: &mut Tally) {
        let Some(r) = q.pop() else { return };
        let seq = r.seq;
        if r.flags & FLAG_MERGED != 0 {
            t.merged += 1;
        }
        if seq != t.run_seq {
            t.run_seq = seq;
            t.run_rank = 0;
            t.run_begin = 0;
            t.run_end = 0;
        }
        let rank = r.phase as u8;
        let prev = t.run_rank;
        assert!(rank >= prev, "seq {seq}: phase rank {rank} after {prev}");
        match r.phase {
            Phase::Begin => {
                t.took_begin += 1;
                t.run_begin += 1;
                assert!(t.run_begin == 1, "seq {seq} had two Begins in the queue");
            }
            Phase::Update => t.took_update += 1,
            Phase::End => {
                t.took_end += 1;
                t.run_end += 1;
                assert!(t.run_end == 1, "seq {seq} had two Ends in the queue");
            }
        }
        t.run_rank = rank;
    }

    /// 10 000 records of complete gestures — begin, one to six updates, end — pushed at random with pops between them:
    /// the queue never grows past its cap, never loses an accepted begin or end, and never reorders a gesture.
    #[test]
    fn ten_thousand_random_records_keep_every_gesture_in_order() {
        let mut seed = 0x9e37_79b9_7f4a_7c15;
        let mut q = Queue::new();
        let mut t = Tally {
            sent_begin: 0,
            sent_update: 0,
            sent_end: 0,
            took_begin: 0,
            took_update: 0,
            took_end: 0,
            refused: 0,
            merged: 0,
            full: 0,
            run_seq: 0,
            run_rank: 0,
            run_begin: 0,
            run_end: 0,
        };
        let mut seq = 1u32;
        let mut stage = 0u32;
        let mut left = 0u32;
        let mut time_us = 0u64;
        for i in 0..10_000u32 {
            time_us += 1 + rand(&mut seed) % 500;
            let d = (rand(&mut seed) % 2001) as i32 - 1000;
            let r = match stage {
                0 => {
                    left = 1 + (rand(&mut seed) % 6) as u32;
                    stage = 1;
                    rec(seq, Phase::Begin)
                }
                1 => {
                    left -= 1;
                    if left == 0 {
                        stage = 2;
                    }
                    upd(seq, time_us, d, SCALE_ONE + d, d)
                }
                _ => {
                    stage = 0;
                    let end = rec(seq, Phase::End);
                    seq += 1;
                    end
                }
            };
            if q.push(r) {
                match r.phase {
                    Phase::Begin => t.sent_begin += 1,
                    Phase::Update => t.sent_update += 1,
                    Phase::End => t.sent_end += 1,
                }
            } else {
                t.refused += 1;
                assert_eq!(
                    q.len(),
                    QUEUE_LEN,
                    "a record was refused with room in the queue"
                );
            }
            assert!(q.len() <= QUEUE_LEN, "the queue grew past its cap");
            if q.len() == QUEUE_LEN {
                t.full += 1;
            }
            // The reader wakes most times for one record, which is slower than the finger and fills the queue, and takes
            // everything every 2000 records, so the run sees the folding and the room to spare.
            if rand(&mut seed) % 8 < 6 {
                took(&mut q, &mut t);
            }
            if (i + 1) % 2000 == 0 {
                while !q.is_empty() {
                    took(&mut q, &mut t);
                }
            }
        }
        while !q.is_empty() {
            took(&mut q, &mut t);
        }
        assert!(
            t.full > 0,
            "the run never filled the queue, so it tested nothing of the folding"
        );
        assert!(t.merged > 0, "the queue filled without folding anything");
        assert_eq!(
            t.sent_begin + t.sent_update + t.sent_end + t.refused,
            10_000,
            "every record the run made is accounted for"
        );
        assert_eq!(
            t.took_begin, t.sent_begin,
            "a Begin the queue took never reached the reader"
        );
        assert_eq!(
            t.took_end, t.sent_end,
            "an End the queue took never reached the reader"
        );
        assert!(t.took_update <= t.sent_update, "more updates out than in");
    }
}
