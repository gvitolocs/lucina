//! The two notices, byte for byte, from the C structures and QMI's TLV encoding (`qmi_encode`): little-endian, a one-byte type and a
//! two-byte length per TLV, a seven-byte header of kind, transaction, message and length.

use lucina_q6::ssr::{self, AFTER_SHUTDOWN, BEFORE_SHUTDOWN};

#[test]
fn do_cleanup_is_the_c_structure() {
    let msg = ssr::do_cleanup(7, "lpass");
    assert_eq!(msg.len(), 48);
    assert_eq!(
        &msg[..16],
        &[0, 0, 0, 0, 0, 0, 0, 0, 7, 0, 0, 0, 5, 0, 0, 0]
    );
    assert_eq!(&msg[16..21], b"lpass");
    assert!(msg[21..].iter().all(|b| *b == 0));
    // strscpy leaves room for the NUL.
    let long = ssr::do_cleanup(1, &"x".repeat(40));
    assert_eq!(u32::from_le_bytes(long[12..16].try_into().unwrap()), 31);
    assert_eq!(long[47], 0);
}

#[test]
fn only_a_version_0_cleanup_done_counts() {
    let done = [0u32, 1, 9]
        .iter()
        .flat_map(|w| w.to_le_bytes())
        .collect::<Vec<_>>();
    assert_eq!(ssr::cleanup_done(&done), Some(9));
    let other_version = [1u32, 1, 9]
        .iter()
        .flat_map(|w| w.to_le_bytes())
        .collect::<Vec<_>>();
    assert_eq!(ssr::cleanup_done(&other_version), None);
    let not_done = [0u32, 0, 9]
        .iter()
        .flat_map(|w| w.to_le_bytes())
        .collect::<Vec<_>>();
    assert_eq!(ssr::cleanup_done(&not_done), None);
    assert_eq!(ssr::cleanup_done(&[0; 8]), None);
}

#[test]
fn the_ssctl_event_is_linuxs_tlvs() {
    let msg = ssr::subsys_event(0x0102, "adsp", BEFORE_SHUTDOWN);
    #[rustfmt::skip]
    let want = [
        0x00, 0x02, 0x01, 0x23, 0x00, 22, 0x00, // request, txn 0x102, message 0x23, 22 bytes of TLVs
        0x01, 0x05, 0x00, 4, b'a', b'd', b's', b'p', // the name, its length first
        0x02, 0x04, 0x00, 2, 0, 0, 0, // the event
        0x10, 0x04, 0x00, 0, 0, 0, 0, // forced
    ];
    assert_eq!(msg, want);
}

#[test]
fn the_answer_is_matched_by_transaction() {
    let ok = [0x02, 0x05, 0x00, 0x23, 0x00, 7, 0, 0x02, 4, 0, 0, 0, 0, 0];
    assert_eq!(ssr::subsys_event_answer(&ok, 5), Some(true));
    assert_eq!(ssr::subsys_event_answer(&ok, 6), None);
    let refused = [
        0x02, 0x05, 0x00, 0x23, 0x00, 7, 0, 0x02, 4, 0, 1, 0, 0x2a, 0,
    ];
    assert_eq!(ssr::subsys_event_answer(&refused, 5), Some(false));
    let _ = AFTER_SHUTDOWN;
}
