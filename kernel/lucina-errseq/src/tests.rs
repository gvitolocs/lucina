use super::*;

const EIO: i32 = -5;
const ENOSPC: i32 = -28;

#[test]
fn constants_match_linux() {
    assert_eq!(SHIFT, 12);
    assert_eq!(SEEN, 0x1000);
    assert_eq!(CTR_INC, 0x2000);
}

#[test]
fn new_is_zero() {
    let e = ErrSeq::new();
    assert_eq!(e.load(), 0);
    assert_eq!(e.sample(), 0);
    assert_eq!(e.check(0), 0);
}

#[test]
fn first_set_stores_errno_without_counter() {
    let e = ErrSeq::new();
    assert_eq!(e.set(EIO), 0);
    assert_eq!(e.load(), 5);
}

#[test]
fn two_holders_each_see_one_error_once() {
    let e = ErrSeq::new();
    let mut a = e.sample();
    let mut b = e.sample();
    e.set(EIO);
    assert_eq!(e.check_and_advance(&mut a), EIO);
    assert_eq!(e.check_and_advance(&mut a), 0);
    assert_eq!(e.check_and_advance(&mut b), EIO);
    assert_eq!(e.check_and_advance(&mut b), 0);
}

#[test]
fn sampling_after_unseen_error_still_sees_it_once() {
    let e = ErrSeq::new();
    e.set(EIO);
    let mut late = e.sample();
    assert_eq!(late, 0);
    assert_eq!(e.check_and_advance(&mut late), EIO);
    assert_eq!(e.check_and_advance(&mut late), 0);
}

#[test]
fn sampling_after_seen_error_does_not_see_it() {
    let e = ErrSeq::new();
    let mut first = e.sample();
    e.set(EIO);
    assert_eq!(e.check_and_advance(&mut first), EIO);
    let mut late = e.sample();
    assert_eq!(late, e.load());
    assert_eq!(e.check(late), 0);
    assert_eq!(e.check_and_advance(&mut late), 0);
}

#[test]
fn second_error_after_seen_first_is_seen_again() {
    let e = ErrSeq::new();
    let mut h = e.sample();
    e.set(EIO);
    assert_eq!(e.check_and_advance(&mut h), EIO);
    e.set(ENOSPC);
    assert_eq!(e.check_and_advance(&mut h), ENOSPC);
    assert_eq!(e.check_and_advance(&mut h), 0);
}

#[test]
fn same_error_again_after_seen_is_seen_again() {
    let e = ErrSeq::new();
    let mut h = e.sample();
    e.set(EIO);
    assert_eq!(e.check_and_advance(&mut h), EIO);
    e.set(EIO);
    assert_eq!(e.check_and_advance(&mut h), EIO);
}

#[test]
fn set_zero_is_a_no_op() {
    let e = ErrSeq::new();
    assert_eq!(e.set(0), 0);
    assert_eq!(e.load(), 0);
    e.set(EIO);
    let before = e.load();
    assert_eq!(e.set(0), before);
    assert_eq!(e.load(), before);
}

#[test]
fn set_out_of_range_is_refused() {
    let e = ErrSeq::new();
    e.set(EIO);
    let before = e.load();
    for bad in [1, 5, i32::MAX, -4096, -5000, i32::MIN, i32::MIN + 1] {
        assert_eq!(e.set(bad), before, "err {bad}");
        assert_eq!(e.load(), before, "err {bad}");
    }
}

#[test]
fn set_range_edges_accepted() {
    let e = ErrSeq::new();
    e.set(-1);
    assert_eq!(e.load(), 1);
    e.set(-4095);
    assert_eq!(e.load(), 4095);
    assert_eq!(e.check(0), -4095);
}

#[test]
fn check_does_not_advance_or_mark_seen() {
    let e = ErrSeq::new();
    let since = e.sample();
    e.set(EIO);
    assert_eq!(e.check(since), EIO);
    assert_eq!(e.check(since), EIO);
    assert_eq!(e.load() & SEEN, 0);
}

#[test]
fn seen_bit_transitions() {
    let e = ErrSeq::new();
    let mut h = 0;
    e.set(EIO);
    assert_eq!(e.load() & SEEN, 0);
    // sample does not mark seen
    let _ = e.sample();
    assert_eq!(e.load() & SEEN, 0);
    assert_eq!(e.check_and_advance(&mut h), EIO);
    assert_eq!(e.load() & SEEN, SEEN);
    assert_eq!(h, e.load());
    e.set(EIO);
    assert_eq!(e.load() & SEEN, 0);
}

#[test]
fn counter_increments_only_when_seen() {
    let e = ErrSeq::new();
    let mut h = 0;
    e.set(EIO);
    e.set(ENOSPC);
    e.set(EIO);
    assert_eq!(e.load() >> (SHIFT + 1), 0);
    assert_eq!(e.check_and_advance(&mut h), EIO);
    e.set(ENOSPC);
    assert_eq!(e.load() >> (SHIFT + 1), 1);
    e.set(EIO);
    assert_eq!(e.load() >> (SHIFT + 1), 1);
    e.check_and_advance(&mut h);
    e.set(EIO);
    assert_eq!(e.load() >> (SHIFT + 1), 2);
    assert_eq!(e.load() & MAX_ERRNO, 5);
}

#[test]
fn unseen_errors_collapse_for_a_holder() {
    let e = ErrSeq::new();
    let mut h = e.sample();
    e.set(EIO);
    e.set(ENOSPC);
    assert_eq!(e.check_and_advance(&mut h), ENOSPC);
    assert_eq!(e.check_and_advance(&mut h), 0);
}

#[test]
fn set_returns_previous_value() {
    let e = ErrSeq::new();
    assert_eq!(e.set(EIO), 0);
    assert_eq!(e.set(EIO), 5); // no change: returns the (same) value
    let mut h = 0;
    e.check_and_advance(&mut h);
    assert_eq!(e.set(EIO), 5 | SEEN);
}

#[test]
fn counter_wraps_like_unsigned_arithmetic() {
    let e = ErrSeq::new();
    // Counter all ones, errno 5, SEEN set: the next set wraps the counter to zero.
    e.0.store(!(MAX_ERRNO | SEEN) | 5 | SEEN, Ordering::Relaxed);
    assert_eq!(e.load() >> (SHIFT + 1), (1 << 19) - 1);
    e.set(EIO);
    assert_eq!(e.load(), 5);
}

#[test]
fn full_wrap_can_hide_an_error_from_a_stale_sample() {
    let e = ErrSeq::new();
    let mut h = 0;
    e.set(EIO);
    e.check_and_advance(&mut h);
    let stale = h;
    // 2^19 see-then-set rounds bring the counter back to where it was.
    let mut other = 0;
    for _ in 0..(1u32 << 19) {
        e.set(EIO);
        e.check_and_advance(&mut other);
    }
    assert_eq!(e.load(), stale);
    assert_eq!(e.check(stale), 0);
}

#[test]
fn check_and_advance_without_change_leaves_value_alone() {
    let e = ErrSeq::new();
    let mut h = 0;
    assert_eq!(e.check_and_advance(&mut h), 0);
    assert_eq!(e.load(), 0);
    assert_eq!(h, 0);
}

#[test]
fn check_and_advance_stores_seen_sample() {
    let e = ErrSeq::new();
    let mut h = 0;
    e.set(EIO);
    e.check_and_advance(&mut h);
    assert_eq!(h, 5 | SEEN);
    assert_eq!(e.load(), h);
}

#[test]
fn concurrent_sets_keep_a_valid_value() {
    extern crate std;
    let e = std::sync::Arc::new(ErrSeq::new());
    let mut hs = std::vec::Vec::new();
    for i in 0..4 {
        let e = e.clone();
        hs.push(std::thread::spawn(move || {
            let mut since = 0;
            for _ in 0..10_000 {
                e.set(-(i + 1));
                let r = e.check_and_advance(&mut since);
                assert!((-4..=0).contains(&r));
            }
        }));
    }
    for h in hs {
        h.join().unwrap();
    }
    assert!((1..=4).contains(&(e.load() & MAX_ERRNO)));
}
