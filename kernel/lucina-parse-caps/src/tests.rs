use super::*;

#[test]
fn take_cases() {
    assert_eq!(take(10, 3), Some(7));
    assert_eq!(take(10, 10), Some(0));
    assert_eq!(take(10, 11), None);
    assert_eq!(take(0, 0), Some(0));
    assert_eq!(take(u64::MAX, u64::MAX), Some(0));
}

#[test]
fn product_and_area_never_overflow() {
    assert_eq!(product(0, u64::MAX, 0), Some(0));
    assert_eq!(product(u64::MAX, u64::MAX, u64::MAX), None);
    assert_eq!(product(1 << 32, 1 << 32, u64::MAX), None);
    assert_eq!(product((1 << 32) - 1, 1 << 32, u64::MAX), Some(((1u64 << 32) - 1) << 32));
    assert_eq!(product(3, 4, 12), Some(12));
    assert_eq!(product(3, 4, 11), None);
    // 65535 × 65535 RGBA against 1 GiB
    assert_eq!(area(65535, 65535, 4, 1 << 30), None);
    assert_eq!(area(4096, 4096, 4, 1 << 30), Some(64 << 20));
    assert_eq!(area(u64::MAX, u64::MAX, 0, 0), Some(0));
    assert_eq!(area(u64::MAX, 2, 1, u64::MAX), None);
}

#[test]
fn range_cases() {
    assert_eq!(range(0, 0, 0), Some(0));
    assert_eq!(range(4, 4, 8), Some(8));
    assert_eq!(range(4, 5, 8), None);
    assert_eq!(range(9, 0, 8), None);
    assert_eq!(range(u64::MAX, 1, u64::MAX), None);
    assert_eq!(range(1, u64::MAX, u64::MAX), None);
}

#[test]
fn counts_and_depths() {
    assert_eq!(count_next(0, 1), Some(1));
    assert_eq!(count_next(1, 1), None);
    assert_eq!(count_next(u64::MAX, u64::MAX), None);
    assert_eq!(enter(0, 2), Some(1));
    assert_eq!(enter(2, 2), None);
    assert_eq!(enter(u32::MAX, u32::MAX), None);
}

#[test]
fn zip_entries() {
    // 1 GiB of zeros packed into 1 MiB, ratio cap 100: refused.
    assert_eq!(zip_entry(1 << 30, 1 << 20, u64::MAX, 100, u64::MAX), None);
    // over the entry cap
    assert_eq!(zip_entry(65 << 20, 64 << 20, 64 << 20, 100, u64::MAX), None);
    // over the archive budget
    assert_eq!(zip_entry(10, 10, 100, 100, 9), None);
    assert_eq!(zip_entry(10, 10, 100, 100, 10), Some(0));
    // huge packed size: the ratio product saturates instead of overflowing
    assert_eq!(zip_entry(5, u64::MAX, 10, u64::MAX, 100), Some(95));
    // an empty entry with no packed bytes is fine; a non-empty one from nothing is not
    assert_eq!(zip_entry(0, 0, 10, 10, 10), Some(10));
    assert_eq!(zip_entry(1, 0, 10, 10, 10), None);
}

#[test]
fn inflate_stops_at_the_declared_size() {
    assert_eq!(inflate_more(0, 10, 10), Some(10));
    assert_eq!(inflate_more(10, 1, 10), None);
    assert_eq!(inflate_more(u64::MAX, 1, u64::MAX), None);
}

#[test]
fn capped_read_holds_at_most_cap_plus_one() {
    let cap = 100;
    let mut have = 0;
    // a source that never ends, read 64 bytes at a time
    loop {
        let n = read_next(have, 64, cap);
        if n == 0 {
            break;
        }
        have += n;
        assert!(have <= cap + 1);
    }
    assert_eq!(have, cap + 1);
    assert!(over(have, cap));
    assert!(!over(cap, cap));
    assert_eq!(read_next(0, 5, u64::MAX - 1), 5);
}
