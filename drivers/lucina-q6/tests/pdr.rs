//! The service registry messages, in `qcom_pdr_msg.c`'s layout.

use lucina_q6::pdr::{self, Notice};

fn message(kind: u8, txn: u16, id: u16, body: &[u8]) -> Vec<u8> {
    let mut out = vec![kind];
    out.extend_from_slice(&txn.to_le_bytes());
    out.extend_from_slice(&id.to_le_bytes());
    out.extend_from_slice(&(body.len() as u16).to_le_bytes());
    out.extend_from_slice(body);
    out
}

fn tlv(t: u8, v: &[u8]) -> Vec<u8> {
    let mut out = vec![t];
    out.extend_from_slice(&(v.len() as u16).to_le_bytes());
    out.extend_from_slice(v);
    out
}

#[test]
fn the_requests_are_what_the_tables_say() {
    let mut body = tlv(1, &[1]);
    body.extend(tlv(2, pdr::AUDIO_PD_PATH.as_bytes()));
    assert_eq!(
        pdr::register_listener(7, pdr::AUDIO_PD_PATH),
        message(0, 7, 0x20, &body)
    );

    let mut body = tlv(1, pdr::AUDIO_PD_PATH.as_bytes());
    body.extend(tlv(2, &0x1234u16.to_le_bytes()));
    assert_eq!(
        pdr::set_ack(8, pdr::AUDIO_PD_PATH, 0x1234),
        message(0, 8, 0x23, &body)
    );
    assert_eq!(pdr::AUDIO_PD_INSTANCE_WORD, 1 | (74 << 8));
}

#[test]
fn answers_and_changes_are_read() {
    let result = |r: u16, e: u16| {
        let mut v = r.to_le_bytes().to_vec();
        v.extend_from_slice(&e.to_le_bytes());
        tlv(2, &v)
    };
    let mut body = result(0, 0);
    body.extend(tlv(0x10, &pdr::STATE_UP.to_le_bytes()));
    assert_eq!(
        pdr::on_message(&message(2, 7, 0x20, &body)),
        Notice::Registered {
            state: Some(pdr::STATE_UP)
        }
    );
    assert_eq!(
        pdr::on_message(&message(2, 7, 0x20, &result(0, 0))),
        Notice::Registered { state: None }
    );
    assert_eq!(
        pdr::on_message(&message(2, 7, 0x20, &result(1, 3))),
        Notice::Refused { error: 3 }
    );

    let mut body = tlv(1, &pdr::STATE_UP.to_le_bytes());
    body.extend(tlv(2, pdr::AUDIO_PD_PATH.as_bytes()));
    body.extend(tlv(3, &5u16.to_le_bytes()));
    assert_eq!(
        pdr::on_message(&message(4, 0, 0x22, &body)),
        Notice::Changed {
            state: pdr::STATE_UP,
            transaction_id: 5
        }
    );
    for junk in [
        vec![],
        vec![4u8; 3],
        message(4, 0, 0x99, &[]),
        message(4, 0, 0x22, &[]),
    ] {
        assert_eq!(pdr::on_message(&junk), Notice::Other);
    }
}
