use super::*;

fn queue_n(mut l: Ledger, p: u32, data: &[bool]) -> (Ledger, [u32; 8]) {
    let mut slots = [u32::MAX; 8];
    for (i, has) in data.iter().enumerate() {
        slots[i] = next_slot(l, p);
        l = queue(l, p, *has);
    }
    (l, slots)
}

#[test]
fn empty_has_nothing_out() {
    assert_eq!(empty(8), Ledger { oldest: 0, count: 0, data: 0 });
    assert!(!data_out(empty(8), 8));
}

#[test]
fn slots_go_out_in_turn() {
    let (l, slots) = queue_n(empty(4), 4, &[true, false, true, false]);
    assert_eq!(&slots[..4], &[0, 1, 2, 3]);
    assert_eq!(l.count, 4);
    assert_eq!(l.data, 0b0101);
}

#[test]
fn only_the_oldest_comes_back() {
    assert_eq!(done(empty(4), 4, 0), None);
    let (l, _) = queue_n(empty(4), 4, &[true, false]);
    assert_eq!(done(l, 4, 1), None);
    assert_eq!(done(l, 4, 7), None);
    let (n, had) = done(l, 4, 0).unwrap();
    assert!(had);
    assert_eq!((n.oldest, n.count, n.data & 1), (1, 1, 0));
    let (n, had) = done(n, 4, 1).unwrap();
    assert!(!had);
    assert_eq!(done(n, 4, 2), None);
}

#[test]
fn the_ring_wraps_and_never_reuses_a_slot_out() {
    let (mut l, _) = queue_n(empty(3), 3, &[false, false, false]);
    l = done(l, 3, 0).unwrap().0;
    l = done(l, 3, 1).unwrap().0;
    assert_eq!(next_slot(l, 3), 0);
    l = queue(l, 3, false);
    assert_eq!(next_slot(l, 3), 1);
    l = queue(l, 3, false);
    for s in [2, 0, 1] {
        assert_eq!(done(l, 3, (s + 1) % 3), None);
        l = done(l, 3, s).unwrap().0;
    }
    assert_eq!(l.count, 0);
}

#[test]
fn data_out_follows_the_periods_with_samples() {
    let l = queue(empty(8), 8, true);
    assert!(data_out(l, 8));
    assert!(!data_out(done(l, 8, 0).unwrap().0, 8));
    let (l, _) = queue_n(empty(8), 8, &[false, false, false]);
    assert!(!data_out(l, 8));
}

#[test]
fn may_queue_stops_at_the_limit() {
    let mut l = empty(8);
    for _ in 0..3 {
        assert!(may_queue(l, 3));
        l = queue(l, 8, false);
    }
    assert!(!may_queue(l, 3));
}

#[test]
fn underrun_only_for_a_late_empty_period() {
    for bytes in [0u32, 1920] {
        for had in [false, true] {
            for tone in [false, true] {
                for finished in [false, true] {
                    let want = bytes == 0 && had && !tone && !finished;
                    assert_eq!(is_underrun(bytes, had, tone, finished), want, "{bytes} {had} {tone} {finished}");
                }
            }
        }
    }
}

#[test]
fn mic_drops_exactly_what_does_not_fit() {
    assert_eq!(mic_push(0, 100, 192_000), (100, 0));
    assert_eq!(mic_push(191_950, 100, 192_000), (192_000, 50));
    assert_eq!(mic_push(192_000, 1920, 192_000), (192_000, 1920));
    assert_eq!(mic_push(10, 0, 100), (10, 0));
}

#[test]
fn undo_takes_each_step_once_in_the_original_order() {
    for taken in 0..=5u32 {
        let rev: [u32; 5] = core::array::from_fn(|i| if (i as u32) < taken { undo_step(Undo::Reverse, taken, i as u32) } else { u32::MAX });
        let fwd: [u32; 5] = core::array::from_fn(|i| if (i as u32) < taken { undo_step(Undo::AsTaken, taken, i as u32) } else { u32::MAX });
        for i in 0..taken as usize {
            assert_eq!(rev[i], taken - 1 - i as u32);
            assert_eq!(fwd[i], i as u32);
        }
        for step in 0..taken {
            assert_eq!(rev[..taken as usize].iter().filter(|&&s| s == step).count(), 1);
            assert_eq!(fwd[..taken as usize].iter().filter(|&&s| s == step).count(), 1);
        }
    }
}
