use super::*;

#[test]
fn every_engine_has_its_own_bit() {
    let mut all = 0;
    for id in 0..CE_COUNT {
        let bit = ce(id);
        assert_eq!(bit.count_ones(), 1);
        assert_eq!(all & bit, 0);
        all |= bit;
    }
    assert_eq!(all, CE_BITS);
    assert_eq!(ce(CE_COUNT), 0);
    assert_eq!(CE_BITS & (GLINK | SMP2P | TX | CONTROL), 0);
    assert_eq!(CE_BITS | GLINK | SMP2P | TX | CONTROL, ALL);
}

#[test]
fn raise_then_drain_loses_nothing() {
    let p = raise(raise(0, ce(3)), GLINK);
    assert_eq!(raise(p, 1 << 20), p);
    let (taken, left) = drain(p);
    assert_eq!(taken, ce(3) | GLINK);
    assert_eq!(left, 0);
    assert!(may_sleep(left, false));
    assert!(!may_sleep(left, true));
    assert!(!may_sleep(raise(left, TX), false));
}

#[test]
fn interrupts_sleep_until_the_earliest_deadline() {
    assert_eq!(plan(Mode::Interrupts, false, false, None, 10), Sleep::Forever);
    assert_eq!(plan(Mode::Interrupts, false, true, None, 10), Sleep::Yield);
    assert_eq!(plan(Mode::Interrupts, false, true, Some(25), 10), Sleep::Yield);
    assert_eq!(plan(Mode::Interrupts, false, false, Some(25), 10), Sleep::For(15));
    assert_eq!(plan(Mode::Interrupts, false, false, Some(10), 10), Sleep::No);
    assert_eq!(plan(Mode::Interrupts, true, false, None, 10), Sleep::No);
    assert_eq!(earliest(Some(40), earliest(None, Some(25))), Some(25));
}

#[test]
fn the_old_modes_stay() {
    assert_eq!(plan(Mode::Poll, false, false, None, 0), Sleep::For(1));
    assert_eq!(plan(Mode::Poll, false, true, None, 0), Sleep::Yield);
    assert_eq!(plan(Mode::Yield, false, false, Some(5), 0), Sleep::Yield);
}
