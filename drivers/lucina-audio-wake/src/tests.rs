use super::*;

#[test]
fn raise_keeps_earlier_bits_and_adds_the_source() {
    assert_eq!(raise(GLINK, FILES), GLINK | FILES);
    assert_ne!(raise(0, SMP2P), 0);
    assert_eq!(raise(raise(0, WATCHDOG), WATCHDOG), raise(0, WATCHDOG));
}

#[test]
fn the_sources_are_four_bits() {
    let all = [GLINK, SMP2P, WATCHDOG, FILES];
    for (i, a) in all.iter().enumerate() {
        assert_eq!(a.count_ones(), 1);
        for b in &all[i + 1..] {
            assert_eq!(a & b, 0);
        }
    }
}

#[test]
fn drain_takes_everything_and_leaves_nothing() {
    assert_eq!(drain(0b1011), (0b1011, 0));
    assert_eq!(drain(0), (0, 0));
}

#[test]
fn may_sleep_only_with_nothing_raised_and_nothing_left() {
    assert!(may_sleep(0, false));
    assert!(!may_sleep(0, true));
    assert!(!may_sleep(GLINK, false));
    assert!(!may_sleep(FILES, true));
}

#[test]
fn a_raise_during_the_pass_is_seen_by_the_next() {
    let mut pending = raise(0, SMP2P);
    let (taken, left) = drain(pending);
    pending = left;
    assert_eq!(taken, SMP2P);
    // The sources are read here; GLINK's interrupt comes meanwhile.
    pending = raise(pending, GLINK);
    assert!(!may_sleep(pending, false));
    assert_eq!(drain(pending).0, GLINK);
}

#[test]
fn reading_first_and_clearing_after_loses_the_raise() {
    // The wrong order, as an example of why the drain comes first: read, the interrupt comes, then clear.
    let raised = raise(0, GLINK);
    assert_ne!(raised, 0);
    let pending = 0; // cleared after the read: the raise is gone
    assert!(
        may_sleep(pending, false),
        "the thread would sleep with GLINK's news unread"
    );
}

#[test]
fn first_after_is_the_first_millisecond_past_the_limit() {
    assert_eq!(first_after(100, 50), 151);
    for now in 100..=200u64 {
        assert_eq!(now >= 151, now - 100 > 50, "now {now}");
    }
    assert_eq!(first_after(u64::MAX - 10, 20), u64::MAX);
    assert_eq!(first_after(u64::MAX, 0), u64::MAX);
}

#[test]
fn first_at_is_the_first_millisecond_at_the_limit() {
    assert_eq!(first_at(100, 50), 150);
    for now in 100..=200u64 {
        assert_eq!(now >= 150, now - 100 >= 50, "now {now}");
    }
    assert_eq!(first_at(u64::MAX - 5, 5), u64::MAX);
    assert_eq!(first_at(u64::MAX - 5, 6), u64::MAX);
}

#[test]
fn an_answer_is_due_only_while_waiting() {
    assert_eq!(answer_due(false, 10, 1000), None);
    assert_eq!(answer_due(true, 10, 1000), Some(1011));
}

#[test]
fn a_log_line_is_due_only_while_open() {
    assert_eq!(beat_due(false, 0, 5000), None);
    assert_eq!(beat_due(true, 0, 5000), Some(5000));
}

#[test]
fn earliest_of_two() {
    assert_eq!(earliest(None, None), None);
    assert_eq!(earliest(Some(7), None), Some(7));
    assert_eq!(earliest(None, Some(9)), Some(9));
    assert_eq!(earliest(Some(7), Some(9)), Some(7));
    assert_eq!(earliest(Some(9), Some(7)), Some(7));
    assert_eq!(earliest(Some(5), Some(5)), Some(5));
}

fn idle() -> Due {
    Due {
        ready: true,
        ask_at: None,
        give_up_at: 99,
        stream: None,
        mic: None,
        link: None,
        beat_at: None,
    }
}

#[test]
fn ready_with_nothing_open_has_no_deadline() {
    assert_eq!(next_due(idle()), None);
    assert_eq!(sleep_for(next_due(idle()), 1_000_000), Sleep::Forever);
}

#[test]
fn starting_up_counts_giving_up_and_asking_again() {
    assert_eq!(
        next_due(Due {
            ready: false,
            ..idle()
        }),
        Some(99)
    );
    assert_eq!(
        next_due(Due {
            ready: false,
            ask_at: Some(10),
            ..idle()
        }),
        Some(10)
    );
    // Once ready, neither counts.
    assert_eq!(
        next_due(Due {
            ask_at: Some(10),
            ..idle()
        }),
        None
    );
}

#[test]
fn the_earliest_of_everything_open() {
    let busy = Due {
        stream: Some(40),
        mic: Some(30),
        link: Some(50),
        beat_at: Some(20),
        ..idle()
    };
    assert_eq!(next_due(busy), Some(20));
    assert_eq!(
        next_due(Due {
            ready: false,
            ask_at: Some(10),
            ..busy
        }),
        Some(10)
    );
    assert_eq!(
        next_due(Due {
            beat_at: None,
            ..busy
        }),
        Some(30)
    );
}

#[test]
fn sleep_until_the_deadline() {
    assert_eq!(sleep_for(None, 5), Sleep::Forever);
    assert_eq!(sleep_for(Some(30), 10), Sleep::For(20));
    assert_eq!(sleep_for(Some(10), 10), Sleep::No);
    assert_eq!(sleep_for(Some(3), 10), Sleep::No);
}
