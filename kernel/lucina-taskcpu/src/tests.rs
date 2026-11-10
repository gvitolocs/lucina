use super::*;

#[test]
fn decode_cases() {
    assert_eq!(decode(NONE), Slot::Free);
    assert_eq!(decode(DEAD), Slot::Dead);
    assert_eq!(decode(0), Slot::On(0));
    assert_eq!(decode(DEAD - 1), Slot::On(DEAD - 1));
}

#[test]
fn encode_on_round_trips() {
    for cpu in [0u32, 1, 7, DEAD - 1] {
        assert_eq!(encode_on(cpu), cpu);
        assert_eq!(decode(encode_on(cpu)), Slot::On(cpu));
    }
}

#[test]
fn failed_claim_cases() {
    assert_eq!(after_failed_claim(DEAD), Wake::Refuse);
    assert_eq!(after_failed_claim(3), Wake::Recheck(3));
    assert_eq!(after_failed_claim(DEAD - 1), Wake::Recheck(DEAD - 1));
}
