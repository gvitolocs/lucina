//! The station against the model AP (`common/mod.rs`): the handshake's rules and the MLME's timing. The model is a test
//! partner written from the standard, not a real AP.

mod common;

use common::*;
use lucina_wifi_mac::crypto::keywrap::aes_wrap;
use lucina_wifi_mac::frame::{REASON_4WAY_HANDSHAKE_TIMEOUT, REASON_IE_DIFFERENT};
use lucina_wifi_mac::ie::{HtCaps, elements};
use lucina_wifi_mac::mlme::{
    ASSOC_MAX_TRIES, ASSOC_TIMEOUT_LONG_MS, ASSOC_TIMEOUT_SHORT_MS, AUTH_MAX_TRIES,
    AUTH_TIMEOUT_LONG_MS, AUTH_TIMEOUT_MS, AUTH_TIMEOUT_SHORT_MS, BEACON_LOSS_COUNT,
    HANDSHAKE_TIMEOUT_MS, PROBE_WAIT_MS,
};
use lucina_wifi_mac::*;

fn tk_of(out: &[Output]) -> Option<[u8; 16]> {
    out.iter().find_map(|o| match o {
        Output::InstallPairwise { tk, .. } => Some(*tk.expose()),
        _ => None,
    })
}

fn has_group(out: &[Output]) -> bool {
    out.iter().any(|o| matches!(o, Output::InstallGroup { .. }))
}

fn disconnect_reason(out: &[Output]) -> Option<DisconnectReason> {
    out.iter().find_map(|o| match o {
        Output::Disconnected { reason } => Some(*reason),
        _ => None,
    })
}

/// mac80211's and wpa_supplicant's numbers, written out: the tests below use the names and would follow a wrong value.
#[test]
fn timing_constants_are_the_ones_in_the_sources() {
    use lucina_wifi_mac::mlme::*;
    assert_eq!(
        (
            AUTH_TIMEOUT_MS,
            AUTH_TIMEOUT_LONG_MS,
            AUTH_TIMEOUT_SHORT_MS,
            AUTH_MAX_TRIES
        ),
        (200, 500, 100, 3)
    );
    assert_eq!(
        (
            ASSOC_TIMEOUT_MS,
            ASSOC_TIMEOUT_LONG_MS,
            ASSOC_TIMEOUT_SHORT_MS,
            ASSOC_MAX_TRIES
        ),
        (200, 500, 100, 3)
    );
    assert_eq!(
        (BEACON_LOSS_COUNT, MAX_PROBE_TRIES, PROBE_WAIT_MS),
        (7, 5, 500)
    );
    assert_eq!(
        (HANDSHAKE_TIMEOUT_MS, MIC_FAILURE_WINDOW_MS, MAX_AID),
        (10_000, 60_000, 2007)
    );
}

#[test]
fn aid_keeps_eleven_bits_only() {
    use lucina_wifi_mac::mlme::AssocRespBody;
    let body = [0x11, 0x04, 0, 0, 0xff, 0xff, 1, 0];
    let r = AssocRespBody::parse(&body).unwrap();
    assert_eq!((r.capability, r.status, r.aid), (0x0411, 0, 0x07ff));
    assert_eq!(r.ies, &[1, 0]);
    assert!(AssocRespBody::parse(&body[..5]).is_none());
}

#[test]
fn full_connection_in_the_right_order() {
    let mut ap = ModelAp::new(PASSPHRASE);
    let mut sta = Station::new(hw_basic(), STA);
    sta.on_mgmt_rx(&ap.beacon_frame(), -50, FREQ, 0);
    assert_eq!(sta.state(), StationState::Idle);
    sta.connect(
        SSID,
        Credentials::passphrase(PASSPHRASE).unwrap(),
        [0x5a; 32],
        0,
    )
    .unwrap();
    let out = drain(&mut sta);
    assert!(
        matches!(&out[0], Output::JoinBss(j) if j.bssid == AP && j.channel == 6 && j.freq_mhz == 2437)
    );
    assert!(
        matches!(&out[1], Output::TxMgmt(f) if f[0] == 0xb0 && f[24..30] == [0, 0, 1, 0, 0, 0])
    );
    assert_eq!(out.len(), 2);

    sta.on_mgmt_rx(&ap.auth_resp(0), -50, FREQ, 1);
    let out = drain(&mut sta);
    assert!(matches!(&out[0], Output::TxMgmt(f) if f[0] == 0x00));
    assert_eq!(sta.state(), StationState::Associating);

    sta.on_mgmt_rx(&ap.assoc_resp(0, 5), -50, FREQ, 2);
    let out = drain(&mut sta);
    assert!(matches!(&out[0], Output::PeerAssoc(p) if p.aid == 5 && p.need_ptk_4way));
    assert_eq!(out.len(), 1);
    assert_eq!(sta.state(), StationState::Handshaking);

    let m1 = ap.msg1();
    rx(&mut sta, &m1);
    let m2 = only_tx_data(&mut sta);
    assert!(ap.take_msg2(&m2), "the AP accepts message 2's MIC");
    // Message 2 repeats the association's RSN element.
    let e = &m2[32..];
    assert_eq!(&e[99..], &RSN_PSK_CCMP);
    assert_eq!(m2[1] & 0x40, 0);

    let m3 = ap.msg3();
    rx(&mut sta, &m3);
    let out = drain(&mut sta);
    assert_eq!(out.len(), 4, "{out:?}");
    let Output::TxData {
        frame: m4,
        eapol: true,
    } = &out[0]
    else {
        panic!("message 4 first: the key may only go in after it");
    };
    assert_eq!(
        m4[1] & 0x40,
        0,
        "message 4 in the clear on a first handshake"
    );
    assert!(ap.check_mic(&m4[32..]));
    assert_eq!(
        u16::from_be_bytes([m4[32 + 5], m4[32 + 6]]),
        0x0002 | 0x0008 | 0x0100 | 0x0200
    );
    let Output::InstallPairwise { peer, tk } = &out[1] else {
        panic!("pairwise key second");
    };
    assert_eq!((*peer, *tk.expose()), (AP, ap.tk()));
    let Output::InstallGroup {
        idx,
        key,
        cipher,
        rsc,
    } = &out[2]
    else {
        panic!("group key third");
    };
    assert_eq!(*idx, 1);
    assert_eq!(key.expose(), ap.gtk.as_slice());
    assert_eq!(*cipher, GroupCipher::Ccmp);
    assert_eq!(*rsc, ap.gtk_rsc);
    assert!(matches!(out[3], Output::PortAuthorized));
    assert_eq!(sta.state(), StationState::Connected);

    // Data both ways, with the Protected bit set for the firmware.
    sta.tx_ethernet(&ethernet_ip(b"hello")).unwrap();
    let out = drain(&mut sta);
    let Output::TxData {
        frame,
        eapol: false,
    } = &out[0]
    else {
        panic!("data frame");
    };
    assert_eq!(frame[0], 0x08);
    assert_eq!(frame[1], 0x41, "To-DS and Protected");
    assert_eq!(&frame[4..10], &AP);
    assert_eq!(&frame[10..16], &STA);
    assert_eq!(&frame[24..32], &[0xaa, 0xaa, 3, 0, 0, 0, 8, 0]);
    assert_eq!(&frame[32..], b"hello");

    rx_secure(&mut sta, &nwifi_ip(b"world"));
    let out = drain(&mut sta);
    let Output::RxEthernet(eth) = &out[0] else {
        panic!("data up");
    };
    assert_eq!(&eth[..6], &STA);
    assert_eq!(&eth[12..14], &[8, 0]);
    assert_eq!(&eth[14..], b"world");
}

#[test]
fn repeated_message_1_keeps_the_snonce() {
    let mut ap = ModelAp::new(PASSPHRASE);
    let mut sta = associated(&ap, PASSPHRASE);
    let m1 = ap.msg1();
    rx(&mut sta, &m1);
    let first = only_tx_data(&mut sta);
    let m1 = ap.msg1();
    rx(&mut sta, &m1);
    let second = only_tx_data(&mut sta);
    assert_eq!(
        first[32 + 17..32 + 49],
        second[32 + 17..32 + 49],
        "same SNonce"
    );
    assert_ne!(
        first[32 + 9..32 + 17],
        second[32 + 9..32 + 17],
        "replay counter echoed"
    );
    assert!(ap.take_msg2(&second));
    let m3 = ap.msg3();
    rx(&mut sta, &m3);
    assert_eq!(tk_of(&drain(&mut sta)), Some(ap.tk()));
}

/// KRACK: a retransmitted message 3 (higher counter) must get a new message 4 but never reinstall the key, which would
/// reset the packet numbers.
#[test]
fn retransmitted_message_3_does_not_reinstall() {
    let mut ap = ModelAp::new(PASSPHRASE);
    let mut sta = connected(&mut ap);
    let m3 = ap.msg3();
    rx(&mut sta, &m3);
    let out = drain(&mut sta);
    assert_eq!(out.len(), 1, "{out:?}");
    assert!(matches!(&out[0], Output::TxData { frame, eapol: true } if ap.check_mic(&frame[32..])));
    assert!(tk_of(&out).is_none() && !has_group(&out));
    // And message 4 after a key is in goes out protected.
    let Output::TxData { frame, .. } = &out[0] else {
        unreachable!()
    };
    assert_eq!(frame[1] & 0x40, 0x40);
}

#[test]
fn replayed_message_3_is_ignored() {
    let mut ap = ModelAp::new(PASSPHRASE);
    let mut sta = associated(&ap, PASSPHRASE);
    let m1 = ap.msg1();
    rx(&mut sta, &m1);
    assert!(ap.take_msg2(&only_tx_data(&mut sta)));
    let m3 = ap.msg3();
    rx(&mut sta, &m3);
    drain(&mut sta);
    rx(&mut sta, &m3);
    assert!(
        drain(&mut sta).is_empty(),
        "same replay counter: dropped before anything happens"
    );
}

#[test]
fn replayed_group_key_is_ignored_and_new_one_is_installed() {
    let mut ap = ModelAp::new(PASSPHRASE);
    let mut sta = connected(&mut ap);
    ap.gtk = vec![0x77; 16];
    ap.gtk_idx = 2;
    let g = ap.group1();
    rx(&mut sta, &g);
    let out = drain(&mut sta);
    assert!(
        matches!(&out[0], Output::InstallGroup { idx: 2, key, cipher: GroupCipher::Ccmp, .. } if key.expose() == [0x77; 16])
    );
    let Output::TxData { frame, eapol: true } = &out[1] else {
        panic!("group message 2");
    };
    assert_eq!(frame[1] & 0x40, 0x40, "protected by the PTK");
    assert!(ap.check_mic(&frame[32..]));
    assert_eq!(
        u16::from_be_bytes([frame[32 + 5], frame[32 + 6]]),
        0x0002 | 0x0100 | 0x0200
    );

    rx(&mut sta, &g);
    assert!(drain(&mut sta).is_empty(), "replay");

    // The same GTK again with a fresh counter is acknowledged and not reinstalled.
    let g = ap.group1();
    rx(&mut sta, &g);
    let out = drain(&mut sta);
    assert_eq!(out.len(), 1);
    assert!(!has_group(&out));
}

#[test]
fn ptk_rekey_gives_a_new_key_under_the_old_one() {
    let mut ap = ModelAp::new(PASSPHRASE);
    let mut sta = connected(&mut ap);
    let old_snonce = ap.snonce;
    let old_tk = ap.tk();
    ap.anonce = [0xb6; 32];
    let m1 = ap.msg1();
    rx(&mut sta, &m1);
    let m2 = only_tx_data(&mut sta);
    assert_eq!(m2[1] & 0x40, 0x40, "protected by the current key");
    assert!(ap.take_msg2(&m2));
    assert_ne!(ap.snonce, old_snonce, "a new SNonce for a new PTK");
    let m3 = ap.msg3();
    rx(&mut sta, &m3);
    let out = drain(&mut sta);
    let tk = tk_of(&out).expect("the new TK is installed");
    assert_eq!(tk, ap.tk());
    assert_ne!(tk, old_tk);
    assert!(!has_group(&out), "the same GTK stays in");
    assert!(
        !out.iter().any(|o| matches!(o, Output::PortAuthorized)),
        "the port was never closed"
    );
    assert_eq!(sta.state(), StationState::Connected);
}

#[test]
fn forged_message_1_does_not_disturb_the_ptk() {
    let mut ap = ModelAp::new(PASSPHRASE);
    let mut sta = connected(&mut ap);
    let keep = ap.snonce;
    // An attacker can send message 1, which has no MIC. It gets a message 2 and nothing else changes.
    let mut forged = ModelAp::new(PASSPHRASE);
    forged.replay = ap.replay + 10;
    forged.anonce = [0xcc; 32];
    let m = forged.msg1();
    rx(&mut sta, &m);
    drain(&mut sta);
    // A group rekey under the real PTK still verifies: its counter is above message 3's and no MIC moved it up.
    ap.snonce = keep;
    ap.gtk = vec![0x99; 16];
    let g = ap.group1();
    rx(&mut sta, &g);
    assert!(has_group(&drain(&mut sta)));
    assert_eq!(sta.state(), StationState::Connected);
}

#[test]
fn forged_message_1_does_not_spoil_a_retransmitted_message_3() {
    let mut ap = ModelAp::new(PASSPHRASE);
    let mut sta = connected(&mut ap);
    let mut forged = ModelAp::new(PASSPHRASE);
    forged.replay = ap.replay + 10;
    forged.anonce = [0xcc; 32];
    let m = forged.msg1();
    rx(&mut sta, &m);
    drain(&mut sta);
    let m3 = ap.msg3();
    rx(&mut sta, &m3);
    let out = drain(&mut sta);
    assert_eq!(out.len(), 1, "a message 4 and nothing else: {out:?}");
    assert!(matches!(&out[0], Output::TxData { eapol: true, .. }));
    assert_eq!(sta.state(), StationState::Connected);
}

#[test]
fn wrong_passphrase_never_gets_a_key() {
    let mut ap = ModelAp::new(PASSPHRASE);
    let mut sta = associated(&ap, b"not the passphrase");
    let m1 = ap.msg1();
    rx(&mut sta, &m1);
    let m2 = only_tx_data(&mut sta);
    assert!(!ap.take_msg2(&m2), "the AP sees a bad MIC");
    let m3 = ap.msg3();
    rx(&mut sta, &m3);
    assert!(
        drain(&mut sta).is_empty(),
        "message 3's MIC fails: silently dropped"
    );
    assert_eq!(sta.state(), StationState::Handshaking);
    sta.tx_ethernet(&ethernet_ip(b"x")).unwrap_err();
    sta.on_mgmt_rx(&ap.deauth(REASON_4WAY_HANDSHAKE_TIMEOUT), -50, FREQ, 100);
    let out = drain(&mut sta);
    assert!(matches!(out[0], Output::LeaveBss));
    assert_eq!(
        disconnect_reason(&out),
        Some(DisconnectReason::Deauthenticated {
            reason: REASON_4WAY_HANDSHAKE_TIMEOUT,
            during_handshake: true
        })
    );
    assert_eq!(sta.state(), StationState::Idle);
}

#[test]
fn tampered_message_3_is_dropped() {
    let mut ap = ModelAp::new(PASSPHRASE);
    let mut sta = associated(&ap, PASSPHRASE);
    let m1 = ap.msg1();
    rx(&mut sta, &m1);
    assert!(ap.take_msg2(&only_tx_data(&mut sta)));
    let m3 = ap.msg3();
    for byte in [32 + 6, 32 + 9 + 7, 32 + 20, 32 + 90, m3.len() - 1] {
        let mut bad = m3.clone();
        bad[byte] ^= 1;
        rx(&mut sta, &bad);
        assert!(drain(&mut sta).is_empty(), "byte {byte}");
    }
    // Clearing the MIC bit makes it a message 1, which needs no MIC and anyone can send: it gets a message 2 back. It
    // still installs nothing, and it does not spoil the handshake, because the ANonce is the same.
    let mut as_msg1 = m3.clone();
    as_msg1[32 + 5] ^= 1;
    rx(&mut sta, &as_msg1);
    assert!(tk_of(&drain(&mut sta)).is_none());
    // A forged frame must not have moved the counter: the real one still works.
    rx(&mut sta, &m3);
    assert_eq!(tk_of(&drain(&mut sta)), Some(ap.tk()));
}

#[test]
fn message_3_with_another_rsn_element_ends_the_association() {
    let mut ap = ModelAp::new(PASSPHRASE);
    let mut sta = associated(&ap, PASSPHRASE);
    let m1 = ap.msg1();
    rx(&mut sta, &m1);
    assert!(ap.take_msg2(&only_tx_data(&mut sta)));
    // Authentic, and not what the beacon advertised: a downgrade.
    let mut other = RSN_PSK_CCMP.to_vec();
    other[7] = 2;
    let m3 = ap.msg3_with(other);
    rx(&mut sta, &m3);
    let out = drain(&mut sta);
    assert!(tk_of(&out).is_none());
    assert!(
        matches!(&out[0], Output::TxMgmt(f) if f[0] == 0xc0 && f[24..26] == REASON_IE_DIFFERENT.to_le_bytes())
    );
    assert_eq!(
        disconnect_reason(&out),
        Some(DisconnectReason::HandshakeFailed {
            reason: REASON_IE_DIFFERENT
        })
    );
}

/// Builds a message 3 in pieces, for flags and contents the normal builder would never produce.
fn message3_variant(ap: &mut ModelAp, edit: impl Fn(&mut Vec<u8>)) -> Vec<u8> {
    let mut f = ap.msg3();
    edit(&mut f);
    f
}

fn resign(ap: &ModelAp, f: &mut [u8]) {
    let ptk = ap.ptk();
    let e = &mut f[32..];
    e[81..97].fill(0);
    let mic = lucina_wifi_mac::crypto::hash::hmac_sha1(&ptk[..16], e);
    e[81..97].copy_from_slice(&mic[..16]);
}

#[test]
fn message_3_without_install_or_with_wrong_anonce_fails() {
    for edit in 0..3 {
        let mut ap = ModelAp::new(PASSPHRASE);
        let mut sta = associated(&ap, PASSPHRASE);
        let m1 = ap.msg1();
        rx(&mut sta, &m1);
        assert!(ap.take_msg2(&only_tx_data(&mut sta)));
        let f = {
            let ap_ref = ModelAp {
                snonce: ap.snonce,
                ..ModelAp::new(PASSPHRASE)
            };
            message3_variant(&mut ap, |f| {
                match edit {
                    0 => f[32 + 6] &= !0x40, // Install
                    1 => f[32 + 20] ^= 1,    // ANonce
                    _ => f[32 + 8] = 8,      // Key Length
                }
                resign(&ap_ref, f);
            })
        };
        rx(&mut sta, &f);
        let out = drain(&mut sta);
        assert!(tk_of(&out).is_none(), "edit {edit}");
        assert_eq!(
            disconnect_reason(&out),
            Some(DisconnectReason::HandshakeFailed { reason: 1 }),
            "edit {edit}: authentic but unusable"
        );
    }
}

#[test]
fn message_3_without_gtk_or_with_bad_gtk_length_fails() {
    for gtk_len in [0usize, 8] {
        let mut ap = ModelAp::new(PASSPHRASE);
        let mut sta = associated(&ap, PASSPHRASE);
        let m1 = ap.msg1();
        rx(&mut sta, &m1);
        assert!(ap.take_msg2(&only_tx_data(&mut sta)));
        ap.gtk = vec![1; gtk_len];
        let m3 = ap.msg3();
        rx(&mut sta, &m3);
        let out = drain(&mut sta);
        assert!(tk_of(&out).is_none());
        assert_eq!(
            disconnect_reason(&out),
            Some(DisconnectReason::HandshakeFailed { reason: 1 })
        );
    }
    // No GTK KDE at all, under a correct wrap.
    let mut ap = ModelAp::new(PASSPHRASE);
    let mut sta = associated(&ap, PASSPHRASE);
    let m1 = ap.msg1();
    rx(&mut sta, &m1);
    assert!(ap.take_msg2(&only_tx_data(&mut sta)));
    let mut plain = ap.rsn_ie.clone();
    plain.push(0xdd);
    while !plain.len().is_multiple_of(8) {
        plain.push(0);
    }
    let ptk = ap.ptk();
    let wrapped = aes_wrap(ptk[16..32].try_into().unwrap(), &plain).unwrap();
    let mut f = ap.msg3();
    let e_data = 32 + 99;
    f.truncate(e_data);
    f.extend_from_slice(&wrapped);
    let total = 95 + wrapped.len();
    f[32 + 2..32 + 4].copy_from_slice(&(total as u16).to_be_bytes());
    f[32 + 97..32 + 99].copy_from_slice(&(wrapped.len() as u16).to_be_bytes());
    resign(&ap, &mut f);
    rx(&mut sta, &f);
    assert_eq!(
        disconnect_reason(&drain(&mut sta)),
        Some(DisconnectReason::HandshakeFailed { reason: 1 })
    );
}

#[test]
fn group_message_before_the_4way_is_ignored() {
    let mut ap = ModelAp::new(PASSPHRASE);
    let mut sta = associated(&ap, PASSPHRASE);
    let m1 = ap.msg1();
    rx(&mut sta, &m1);
    assert!(ap.take_msg2(&only_tx_data(&mut sta)));
    let g = ap.group1();
    rx(&mut sta, &g);
    assert!(drain(&mut sta).is_empty());
    let m3 = ap.msg3();
    rx(&mut sta, &m3);
    assert!(tk_of(&drain(&mut sta)).is_some());
}

#[test]
fn port_control() {
    let mut ap = ModelAp::new(PASSPHRASE);
    let mut sta = associated(&ap, PASSPHRASE);
    assert_eq!(
        sta.tx_ethernet(&ethernet_ip(b"x")),
        Err(TxError::PortClosed)
    );
    rx_secure(&mut sta, &nwifi_ip(b"early"));
    assert!(drain(&mut sta).is_empty());
    assert_eq!(sta.stats().rx_port_closed, 1);

    let m1 = ap.msg1();
    rx(&mut sta, &m1);
    assert!(ap.take_msg2(&only_tx_data(&mut sta)));
    let m3 = ap.msg3();
    rx(&mut sta, &m3);
    drain(&mut sta);

    // Plaintext once a key is in: dropped, unless it is EAPOL.
    rx(&mut sta, &nwifi_ip(b"plain"));
    assert!(drain(&mut sta).is_empty());
    assert_eq!(sta.stats().rx_unprotected, 1);

    // The network stack may not send EAPOL.
    let mut eth = ethernet_ip(b"x");
    eth[12..14].copy_from_slice(&[0x88, 0x8e]);
    assert_eq!(sta.tx_ethernet(&eth), Err(TxError::BadFrame));
    assert_eq!(sta.tx_ethernet(&[0; 13]), Err(TxError::BadFrame));
}

#[test]
fn eapol_and_data_from_strangers_are_dropped() {
    let mut ap = ModelAp::new(PASSPHRASE);
    let mut sta = associated(&ap, PASSPHRASE);
    let m1 = ap.msg1();

    // Another transmitter.
    let mut bad = m1.clone();
    bad[10] ^= 1;
    rx(&mut sta, &bad);
    assert!(drain(&mut sta).is_empty());
    // To-DS, which no AP sends a station.
    let mut bad = m1.clone();
    bad[1] = 0x01;
    rx(&mut sta, &bad);
    assert!(drain(&mut sta).is_empty());
    // For someone else.
    let mut bad = m1.clone();
    bad[4] ^= 2;
    rx(&mut sta, &bad);
    assert!(drain(&mut sta).is_empty());
    // SA from Address 3 not the AP: a bridged EAPOL is not the AP's.
    let mut bad = m1.clone();
    bad[16] ^= 2;
    rx(&mut sta, &bad);
    assert!(drain(&mut sta).is_empty());
    // As Ethernet, directly, with the PAE group address: fine, from the AP.
    let mut eth = Vec::new();
    eth.extend_from_slice(&[0x01, 0x80, 0xc2, 0, 0, 3]);
    eth.extend_from_slice(&AP);
    eth.extend_from_slice(&[0x88, 0x8e]);
    eth.extend_from_slice(&m1[32..]);
    sta.on_data_rx(RxFrame::Ethernet(&eth), RxMeta { decrypted: false });
    assert_eq!(drain(&mut sta).len(), 1);
    // Our own multicast coming back is dropped.
    let mut ap2 = ModelAp::new(PASSPHRASE);
    let mut sta2 = connected(&mut ap2);
    let mut echo = nwifi_ip(b"x");
    echo[4..10].copy_from_slice(&[0xff; 6]);
    echo[16..22].copy_from_slice(&STA);
    rx_secure(&mut sta2, &echo);
    assert!(drain(&mut sta2).is_empty());
    let before = sta2.stats();
    assert!(before.rx_foreign >= 1);
}

#[test]
fn message_1_may_overtake_the_association_response() {
    let mut ap = ModelAp::new(PASSPHRASE);
    let mut sta = connecting(&ap, PASSPHRASE);
    drain(&mut sta);
    sta.on_mgmt_rx(&ap.auth_resp(0), -50, FREQ, 1);
    drain(&mut sta);
    let m1 = ap.msg1();
    rx(&mut sta, &m1);
    assert!(drain(&mut sta).is_empty());
    sta.on_mgmt_rx(&ap.assoc_resp(0, 5), -50, FREQ, 2);
    let out = drain(&mut sta);
    assert!(matches!(out[0], Output::PeerAssoc(_)));
    assert!(
        matches!(&out[1], Output::TxData { eapol: true, .. }),
        "message 2 follows at once"
    );
}

#[test]
fn auth_retries_and_times_out_as_mac80211_does() {
    let ap = ModelAp::new(PASSPHRASE);
    let mut sta = connecting(&ap, PASSPHRASE);
    let out = drain(&mut sta);
    assert_eq!(out.len(), 2);
    let mut now = 0;
    for try_no in 2..=AUTH_MAX_TRIES {
        sta.tick(now + AUTH_TIMEOUT_LONG_MS - 1);
        assert!(drain(&mut sta).is_empty(), "not yet");
        now += AUTH_TIMEOUT_LONG_MS;
        sta.tick(now);
        let out = drain(&mut sta);
        assert!(
            matches!(&out[0], Output::TxMgmt(f) if f[0] == 0xb0),
            "try {try_no}"
        );
    }
    now += AUTH_TIMEOUT_LONG_MS;
    sta.tick(now);
    let out = drain(&mut sta);
    assert!(matches!(out[0], Output::LeaveBss));
    assert_eq!(disconnect_reason(&out), Some(DisconnectReason::AuthTimeout));
    assert_eq!(sta.state(), StationState::Idle);
}

#[test]
fn tx_status_steers_the_retry() {
    let ap = ModelAp::new(PASSPHRASE);
    let mut sta = connecting(&ap, PASSPHRASE);
    drain(&mut sta);
    // Acked: wait the short time.
    sta.on_mgmt_tx_status(true, 10);
    sta.tick(10 + AUTH_TIMEOUT_SHORT_MS - 1);
    assert!(drain(&mut sta).is_empty());
    sta.tick(10 + AUTH_TIMEOUT_SHORT_MS);
    assert_eq!(drain(&mut sta).len(), 1);
    // Not acked: retry at the next tick.
    sta.on_mgmt_tx_status(false, 200);
    sta.tick(200);
    assert_eq!(drain(&mut sta).len(), 1);

    // Same for association.
    sta.on_mgmt_rx(&ap.auth_resp(0), -50, FREQ, 201);
    drain(&mut sta);
    sta.on_mgmt_tx_status(true, 210);
    sta.tick(210 + ASSOC_TIMEOUT_SHORT_MS - 1);
    assert!(drain(&mut sta).is_empty());
    sta.tick(210 + ASSOC_TIMEOUT_SHORT_MS);
    assert_eq!(drain(&mut sta).len(), 1);
    sta.on_mgmt_tx_status(false, 400);
    sta.tick(400);
    assert_eq!(drain(&mut sta).len(), 1);
    sta.tick(400 + ASSOC_TIMEOUT_LONG_MS);
    assert_eq!(
        disconnect_reason(&drain(&mut sta)),
        Some(DisconnectReason::AssocTimeout)
    );
}

#[test]
fn without_ack_reports_the_short_timer_runs() {
    let ap = ModelAp::new(PASSPHRASE);
    let mut hw = hw_basic();
    hw.reports_tx_ack = false;
    let mut sta = Station::new(hw, STA);
    sta.on_mgmt_rx(&ap.beacon_frame(), -50, FREQ, 0);
    sta.connect(
        SSID,
        Credentials::passphrase(PASSPHRASE).unwrap(),
        [1; 32],
        0,
    )
    .unwrap();
    drain(&mut sta);
    sta.tick(AUTH_TIMEOUT_MS - 1);
    assert!(drain(&mut sta).is_empty());
    sta.tick(AUTH_TIMEOUT_MS);
    assert_eq!(drain(&mut sta).len(), 1);
}

#[test]
fn assoc_retries_three_times() {
    let ap = ModelAp::new(PASSPHRASE);
    let mut sta = connecting(&ap, PASSPHRASE);
    drain(&mut sta);
    sta.on_mgmt_rx(&ap.auth_resp(0), -50, FREQ, 0);
    let mut sent = drain(&mut sta).len();
    let mut now = 0;
    while sta.state() == StationState::Associating {
        now += ASSOC_TIMEOUT_LONG_MS;
        sta.tick(now);
        let out = drain(&mut sta);
        sent += out
            .iter()
            .filter(|o| matches!(o, Output::TxMgmt(_)))
            .count();
        if sta.state() == StationState::Idle {
            assert_eq!(
                disconnect_reason(&out),
                Some(DisconnectReason::AssocTimeout)
            );
        }
    }
    assert_eq!(sent, ASSOC_MAX_TRIES as usize);
}

#[test]
fn rejections_are_reported_with_their_status() {
    let ap = ModelAp::new(PASSPHRASE);
    let mut sta = connecting(&ap, PASSPHRASE);
    drain(&mut sta);
    sta.on_mgmt_rx(&ap.auth_resp(17), -50, FREQ, 1);
    assert_eq!(
        disconnect_reason(&drain(&mut sta)),
        Some(DisconnectReason::AuthRejected { status: 17 })
    );

    let mut sta = connecting(&ap, PASSPHRASE);
    drain(&mut sta);
    sta.on_mgmt_rx(&ap.auth_resp(0), -50, FREQ, 1);
    drain(&mut sta);
    sta.on_mgmt_rx(&ap.assoc_resp(17, 0), -50, FREQ, 2);
    assert_eq!(
        disconnect_reason(&drain(&mut sta)),
        Some(DisconnectReason::AssocRejected { status: 17 })
    );
}

#[test]
fn association_comeback_waits_the_time_asked() {
    let ap = ModelAp::new(PASSPHRASE);
    let mut sta = connecting(&ap, PASSPHRASE);
    drain(&mut sta);
    sta.on_mgmt_rx(&ap.auth_resp(0), -50, FREQ, 0);
    drain(&mut sta);
    let mut resp = ap.assoc_resp(30, 0);
    // Timeout Interval, type 3 (association comeback), 2000 TU.
    resp.extend_from_slice(&[56, 5, 3]);
    resp.extend_from_slice(&2000u32.to_le_bytes());
    sta.on_mgmt_rx(&resp, -50, FREQ, 100);
    assert!(drain(&mut sta).is_empty());
    // A late ack must not shorten it.
    sta.on_mgmt_tx_status(true, 110);
    sta.tick(100 + 2000 * 1024 / 1000 - 1);
    assert!(drain(&mut sta).is_empty());
    sta.tick(100 + 2000 * 1024 / 1000);
    assert!(matches!(&drain(&mut sta)[0], Output::TxMgmt(f) if f[0] == 0));
}

#[test]
fn frames_with_other_addresses_do_not_move_the_mlme() {
    let ap = ModelAp::new(PASSPHRASE);
    let mut sta = connecting(&ap, PASSPHRASE);
    drain(&mut sta);
    let good = ap.auth_resp(0);

    let mut wrong_ap = good.clone();
    wrong_ap[16] ^= 1;
    wrong_ap[10] ^= 1;
    sta.on_mgmt_rx(&wrong_ap, -50, FREQ, 1);
    let mut wrong_sta = good.clone();
    wrong_sta[4] ^= 1;
    sta.on_mgmt_rx(&wrong_sta, -50, FREQ, 1);
    let mut wrong_seq = good.clone();
    wrong_seq[26] = 4;
    sta.on_mgmt_rx(&wrong_seq, -50, FREQ, 1);
    let mut wrong_alg = good.clone();
    wrong_alg[24] = 3;
    sta.on_mgmt_rx(&wrong_alg, -50, FREQ, 1);
    sta.on_mgmt_rx(&ap.assoc_resp(0, 1), -50, FREQ, 1);
    let mut wrong_deauth = ap.deauth(1);
    wrong_deauth[16] ^= 1;
    wrong_deauth[10] ^= 1;
    sta.on_mgmt_rx(&wrong_deauth, -50, FREQ, 1);
    assert!(drain(&mut sta).is_empty());
    assert_eq!(sta.state(), StationState::Authenticating);
}

#[test]
fn deauth_and_disassoc_end_it_after_the_handshake() {
    let mut ap = ModelAp::new(PASSPHRASE);
    let mut sta = connected(&mut ap);
    sta.on_mgmt_rx(&ap.deauth(7), -50, FREQ, 100);
    assert_eq!(
        disconnect_reason(&drain(&mut sta)),
        Some(DisconnectReason::Deauthenticated {
            reason: 7,
            during_handshake: false
        })
    );
    assert_eq!(
        sta.tx_ethernet(&ethernet_ip(b"x")),
        Err(TxError::PortClosed)
    );

    let mut ap = ModelAp::new(PASSPHRASE);
    let mut sta = connected(&mut ap);
    let mut d = ap.deauth(8);
    d[0] = 0xa0;
    sta.on_mgmt_rx(&d, -50, FREQ, 100);
    assert_eq!(
        disconnect_reason(&drain(&mut sta)),
        Some(DisconnectReason::Disassociated {
            reason: 8,
            during_handshake: false
        })
    );
}

#[test]
fn handshake_times_out_after_ten_seconds() {
    let ap = ModelAp::new(PASSPHRASE);
    let mut sta = associated(&ap, PASSPHRASE);
    // Beacons keep arriving, so the connection monitor stays quiet.
    sta.on_mgmt_rx(&ap.beacon_frame(), -50, FREQ, HANDSHAKE_TIMEOUT_MS - 2);
    sta.tick(2 + HANDSHAKE_TIMEOUT_MS - 1);
    assert!(drain(&mut sta).is_empty());
    sta.tick(2 + HANDSHAKE_TIMEOUT_MS);
    let out = drain(&mut sta);
    assert!(
        matches!(&out[0], Output::TxMgmt(f) if f[0] == 0xc0),
        "the AP is told"
    );
    assert_eq!(
        disconnect_reason(&out),
        Some(DisconnectReason::HandshakeTimeout)
    );
}

#[test]
fn beacon_loss_probes_then_gives_up() {
    let mut ap = ModelAp::new(PASSPHRASE);
    let mut sta = connected(&mut ap);
    // 7 intervals of 100 TU = 716 ms of silence, from the association at t=2.
    sta.tick(2 + 716);
    assert!(
        matches!(&drain(&mut sta)[0], Output::TxMgmt(f) if f[0] == 0x40 && f[4..10] == AP),
        "directed probe"
    );

    // A beacon answers it and silence starts over.
    sta.on_mgmt_rx(&ap.beacon_frame(), -50, FREQ, 800);
    sta.tick(1000);
    assert!(drain(&mut sta).is_empty());

    let mut now = 800 + 716;
    let mut dsts = Vec::new();
    loop {
        sta.tick(now);
        let out = drain(&mut sta);
        if let Some(reason) = disconnect_reason(&out) {
            assert_eq!(reason, DisconnectReason::BeaconLost);
            assert!(
                !out.iter().any(|o| matches!(o, Output::TxMgmt(_))),
                "the AP is gone: no frame"
            );
            break;
        }
        for o in out {
            if let Output::TxMgmt(f) = o {
                dsts.push(f[4]);
            }
        }
        now += 500;
    }
    assert_eq!(dsts.len(), 5);
    assert_eq!(dsts[..2], [AP[0]; 2], "two directed");
    assert_eq!(dsts[2..], [0xff; 3], "then broadcast");
}

#[test]
fn firmware_reports_of_loss() {
    let mut ap = ModelAp::new(PASSPHRASE);
    let mut sta = connected(&mut ap);
    sta.on_beacon_loss(10);
    assert!(matches!(&drain(&mut sta)[0], Output::TxMgmt(f) if f[0] == 0x40));
    sta.on_beacon_seen(20);
    sta.tick(30);
    assert!(drain(&mut sta).is_empty());
    sta.on_connection_lost();
    assert_eq!(
        disconnect_reason(&drain(&mut sta)),
        Some(DisconnectReason::BeaconLost)
    );
}

#[test]
fn michael_mic_failures_report_then_countermeasures() {
    let mut ap = ModelAp::new(PASSPHRASE);
    let mut sta = connected(&mut ap);
    sta.on_mic_failure(true, 1000);
    let out = drain(&mut sta);
    let Output::TxData { frame, eapol: true } = &out[0] else {
        panic!("report");
    };
    assert_eq!(frame[1] & 0x40, 0x40);
    let e = &frame[32..];
    assert_eq!(
        u16::from_be_bytes([e[5], e[6]]),
        0x0002 | 0x0100 | 0x0200 | 0x0400 | 0x0800
    );
    assert!(ap.check_mic(e));
    assert_eq!(e[9..17], [0; 8], "first request counter");
    assert!(matches!(out[1], Output::MicFailureReported { group: true }));

    sta.on_mic_failure(true, 1000 + 59_999);
    let out = drain(&mut sta);
    let e = match &out[0] {
        Output::TxData { frame, .. } => &frame[32..],
        o => panic!("{o:?}"),
    };
    assert_eq!(e[9..17], [0, 0, 0, 0, 0, 0, 0, 1], "request counter counts");
    assert!(
        out.iter()
            .any(|o| matches!(o, Output::TxMgmt(f) if f[0] == 0xc0 && f[24] == 14))
    );
    assert_eq!(
        disconnect_reason(&out),
        Some(DisconnectReason::MicCountermeasures)
    );

    // And joining stays blocked for 60 s.
    sta.on_mgmt_rx(&ap.beacon_frame(), -50, FREQ, 60_999);
    assert_eq!(
        sta.connect(
            SSID,
            Credentials::passphrase(PASSPHRASE).unwrap(),
            [1; 32],
            61_000
        )
        .unwrap_err(),
        ConnectError::Countermeasures
    );
    sta.on_mgmt_rx(&ap.beacon_frame(), -50, FREQ, 120_998);
    assert!(
        sta.connect(
            SSID,
            Credentials::passphrase(PASSPHRASE).unwrap(),
            [1; 32],
            120_999
        )
        .is_ok()
    );
}

#[test]
fn secrets_stay_out_of_debug_output() {
    let mut ap = ModelAp::new(PASSPHRASE);
    let mut sta = associated(&ap, PASSPHRASE);
    let printed = format!("{:?}", Credentials::passphrase(PASSPHRASE).unwrap());
    assert!(!printed.contains("correct"));
    let m1 = ap.msg1();
    rx(&mut sta, &m1);
    assert!(ap.take_msg2(&only_tx_data(&mut sta)));
    let m3 = ap.msg3();
    rx(&mut sta, &m3);
    let printed = format!("{:?} {:?}", drain(&mut sta), sta);
    let bytes: Vec<String> = ap.tk().iter().map(|b| b.to_string()).collect();
    assert!(
        !printed.contains(&bytes[..4].join(", ")),
        "the TK as a Debug list"
    );
    assert!(
        !printed.contains("71, 71, 71, 71"),
        "the GTK as a Debug list"
    );
    assert!(printed.contains("secret bytes"));
}

fn scanned(frames: &[(Vec<u8>, i8, u16)]) -> Station {
    let mut sta = Station::new(hw_basic(), STA);
    for (f, rssi, freq) in frames {
        sta.on_mgmt_rx(f, *rssi, *freq, 0);
    }
    sta
}

fn creds() -> Credentials {
    Credentials::passphrase(PASSPHRASE).unwrap()
}

#[test]
fn refusals_say_why() {
    let beacon_with =
        |cap: u16, extra: &[Vec<u8>]| (beacon(0x80, &AP, SSID, 6, cap, extra), -50, FREQ);
    let rsn = |akm: u8, caps: u16| {
        let mut r = RSN_PSK_CCMP.to_vec();
        r[19] = akm;
        r[20..22].copy_from_slice(&caps.to_le_bytes());
        r
    };

    let mut sta = scanned(&[]);
    assert_eq!(
        sta.connect(SSID, creds(), [0; 32], 0),
        Err(ConnectError::NoSuchNetwork)
    );
    assert_eq!(
        sta.connect(b"", creds(), [0; 32], 0),
        Err(ConnectError::BadSsid)
    );
    assert_eq!(
        sta.connect(&[b'x'; 33], creds(), [0; 32], 0),
        Err(ConnectError::BadSsid)
    );

    // Required PMF: MFPC and MFPR.
    let mut sta = scanned(&[beacon_with(0x0411, &[rsn(2, 0x00c0)])]);
    assert_eq!(
        sta.connect(SSID, creds(), [0; 32], 0),
        Err(ConnectError::PmfRequired)
    );
    // PMF capable but not required: joins, without advertising it.
    let mut sta = scanned(&[beacon_with(0x0411, &[rsn(2, 0x0080)])]);
    assert!(sta.connect(SSID, creds(), [0; 32], 0).is_ok());
    // SAE only.
    let mut sta = scanned(&[beacon_with(0x0411, &[rsn(8, 0x00c0)])]);
    assert_eq!(
        sta.connect(SSID, creds(), [0; 32], 0),
        Err(ConnectError::Wpa3Only)
    );
    // 802.1X.
    let mut sta = scanned(&[beacon_with(0x0411, &[rsn(1, 0)])]);
    assert_eq!(
        sta.connect(SSID, creds(), [0; 32], 0),
        Err(ConnectError::UnsupportedSecurity)
    );
    // WEP: privacy and no RSN.
    let mut sta = scanned(&[beacon_with(0x0411, &[])]);
    assert_eq!(
        sta.connect(SSID, creds(), [0; 32], 0),
        Err(ConnectError::UnsupportedSecurity)
    );
    // WPA1 vendor element only.
    let wpa1 = vec![221, 8, 0x00, 0x50, 0xf2, 1, 1, 0, 0, 0];
    let mut sta = scanned(&[beacon_with(0x0411, &[wpa1])]);
    assert_eq!(
        sta.connect(SSID, creds(), [0; 32], 0),
        Err(ConnectError::UnsupportedSecurity)
    );
    // TKIP-only pairwise.
    let mut tkip = RSN_PSK_CCMP.to_vec();
    tkip[13] = 2;
    let mut sta = scanned(&[beacon_with(0x0411, &[tkip])]);
    assert_eq!(
        sta.connect(SSID, creds(), [0; 32], 0),
        Err(ConnectError::UnsupportedSecurity)
    );
    // GCMP group.
    let mut gcmp = RSN_PSK_CCMP.to_vec();
    gcmp[7] = 8;
    let mut sta = scanned(&[beacon_with(0x0411, &[gcmp])]);
    assert_eq!(
        sta.connect(SSID, creds(), [0; 32], 0),
        Err(ConnectError::UnsupportedSecurity)
    );
    // Passphrase for an open network and the other way around.
    let mut sta = scanned(&[beacon_with(0x0401, &[])]);
    assert_eq!(
        sta.connect(SSID, creds(), [0; 32], 0),
        Err(ConnectError::SecurityMismatch(Security::Open))
    );
    let mut sta = scanned(&[beacon_with(0x0411, &[RSN_PSK_CCMP.to_vec()])]);
    assert_eq!(
        sta.connect(SSID, Credentials::open(), [0; 32], 0),
        Err(ConnectError::SecurityMismatch(Security::Wpa2Psk))
    );
    // An IBSS is not something to join here.
    let mut sta = scanned(&[beacon_with(0x0402, &[])]);
    assert_eq!(
        sta.connect(SSID, Credentials::open(), [0; 32], 0),
        Err(ConnectError::NoSuchNetwork)
    );
    // Band switched off in the hardware.
    let mut hw = hw_basic();
    hw.band_2ghz = false;
    let mut sta = Station::new(hw, STA);
    let (f, r, freq) = beacon_with(0x0411, &[RSN_PSK_CCMP.to_vec()]);
    sta.on_mgmt_rx(&f, r, freq, 0);
    assert_eq!(
        sta.connect(SSID, creds(), [0; 32], 0),
        Err(ConnectError::ChannelNotAllowed)
    );
}

#[test]
fn transition_mode_joins_on_psk() {
    let mut mixed = vec![
        0x30, 24, 1, 0, 0x00, 0x0f, 0xac, 4, 1, 0, 0x00, 0x0f, 0xac, 4, 2, 0,
    ];
    mixed.extend_from_slice(&[0x00, 0x0f, 0xac, 2, 0x00, 0x0f, 0xac, 8, 0x80, 0]);
    let mut sta = scanned(&[(beacon(0x80, &AP, SSID, 6, 0x0411, &[mixed]), -50, FREQ)]);
    sta.connect(SSID, creds(), [1; 32], 0).unwrap();
    assert_eq!(sta.scan_results()[0].security, Security::Wpa2Psk);
    drain(&mut sta);
    sta.on_mgmt_rx(&ModelAp::new(PASSPHRASE).auth_resp(0), -50, FREQ, 1);
    let out = drain(&mut sta);
    let Output::TxMgmt(req) = &out[0] else {
        panic!("assoc request");
    };
    assert!(
        req.windows(22).any(|w| w == RSN_PSK_CCMP),
        "our RSN element asks for AKM 2 and no PMF"
    );
}

#[test]
fn best_bss_wins() {
    let a = [0x02, 0, 0, 0, 0, 0xa];
    let b = [0x02, 0, 0, 0, 0, 0xb];
    let c = [0x02, 0, 0, 0, 0, 0xc];
    let d = [0x02, 0, 0, 0, 0, 0xd];
    let rsn = RSN_PSK_CCMP.to_vec();
    let mut sta = scanned(&[
        (
            beacon(0x80, &a, SSID, 1, 0x0411, std::slice::from_ref(&rsn)),
            -70,
            2412,
        ),
        (
            beacon(0x80, &b, SSID, 6, 0x0411, std::slice::from_ref(&rsn)),
            -55,
            2437,
        ),
        (
            beacon(0x80, &c, b"other", 11, 0x0411, std::slice::from_ref(&rsn)),
            -10,
            2462,
        ),
        (beacon(0x80, &d, SSID, 44, 0x0011, &[rsn]), -60, 5220),
    ]);
    // 5 GHz at -60 beats 2.4 GHz at -55 by the 10 dB bonus.
    assert_eq!(sta.connect(SSID, creds(), [1; 32], 0), Ok(d));
    drain(&mut sta);

    let mut hw = hw_basic();
    hw.band_5ghz = false;
    let mut sta = Station::new(hw, STA);
    sta.on_mgmt_rx(
        &beacon(0x80, &b, SSID, 6, 0x0411, &[RSN_PSK_CCMP.to_vec()]),
        -55,
        2437,
        0,
    );
    sta.on_mgmt_rx(
        &beacon(0x80, &d, SSID, 44, 0x0011, &[RSN_PSK_CCMP.to_vec()]),
        -20,
        5220,
        0,
    );
    assert_eq!(
        sta.connect(SSID, creds(), [1; 32], 0),
        Ok(b),
        "a 5 GHz BSS is unusable: the 2.4 GHz one is chosen"
    );
}

#[test]
fn stale_scan_results_are_not_joined() {
    let ap = ModelAp::new(PASSPHRASE);
    let mut sta = Station::new(hw_basic(), STA);
    sta.on_mgmt_rx(&ap.beacon_frame(), -50, FREQ, 0);
    assert_eq!(
        sta.connect(SSID, creds(), [1; 32], 30_000),
        Err(ConnectError::NoSuchNetwork)
    );
}

#[test]
fn open_network_joins_without_a_handshake() {
    let ap = ModelAp::new(PASSPHRASE);
    let mut sta = Station::new(hw_basic(), STA);
    sta.on_mgmt_rx(&beacon(0x80, &AP, SSID, 6, 0x0401, &[]), -50, FREQ, 0);
    sta.connect(SSID, Credentials::open(), [1; 32], 0).unwrap();
    drain(&mut sta);
    sta.on_mgmt_rx(&ap.auth_resp(0), -50, FREQ, 1);
    let out = drain(&mut sta);
    let Output::TxMgmt(req) = &out[0] else {
        panic!();
    };
    assert_eq!(req[24] & 0x10, 0, "no privacy bit");
    assert!(
        !elements(&req[28..]).any(|(id, _)| id == 48),
        "no RSN element"
    );
    sta.on_mgmt_rx(&ap.assoc_resp(0, 3), -50, FREQ, 2);
    let out = drain(&mut sta);
    assert!(matches!(&out[0], Output::PeerAssoc(p) if !p.need_ptk_4way));
    assert!(matches!(out[1], Output::PortAuthorized));
    assert_eq!(sta.state(), StationState::Connected);
    sta.tx_ethernet(&ethernet_ip(b"x")).unwrap();
    let out = drain(&mut sta);
    assert!(
        matches!(&out[0], Output::TxData { frame, eapol: false } if frame[1] == 0x01),
        "not protected"
    );
    rx(&mut sta, &nwifi_ip(b"y"));
    assert!(matches!(drain(&mut sta)[0], Output::RxEthernet(_)));
}

#[test]
fn psk_sha256_runs_with_cmac_and_the_kdf() {
    let mut ap = ModelAp::new(PASSPHRASE);
    ap.akm = ApAkm::PskSha256;
    ap.rsn_ie[19] = 6;
    let mut sta = associated(&ap, PASSPHRASE);
    let m1 = ap.msg1();
    rx(&mut sta, &m1);
    let m2 = only_tx_data(&mut sta);
    assert_eq!(m2[32 + 6] & 7, 3, "key descriptor version 3");
    assert_eq!(&m2[32 + 99 + 19..32 + 99 + 20], &[6]);
    assert!(ap.take_msg2(&m2));
    let m3 = ap.msg3();
    rx(&mut sta, &m3);
    let out = drain(&mut sta);
    assert!(matches!(&out[0], Output::TxData { frame, .. } if ap.check_mic(&frame[32..])));
    assert_eq!(tk_of(&out), Some(ap.tk()));
    assert_eq!(sta.state(), StationState::Connected);

    // A version 2 frame is not accepted on an AKM 6 association.
    let mut ap2 = ModelAp::new(PASSPHRASE);
    ap2.rsn_ie[19] = 6;
    let mut sta = associated(&ap2, PASSPHRASE);
    let m1 = ap2.msg1();
    rx(&mut sta, &m1);
    assert!(drain(&mut sta).is_empty());
}

#[test]
fn a_big_endian_rsc_counts_as_zero() {
    let mut ap = ModelAp::new(PASSPHRASE);
    ap.gtk_rsc = [0, 0, 0, 0, 0, 0x12];
    let mut sta = connected(&mut ap);
    ap.gtk = vec![0x31; 16];
    let g = ap.group1();
    rx(&mut sta, &g);
    let out = drain(&mut sta);
    assert!(
        matches!(
            &out[0],
            Output::InstallGroup {
                rsc: [0, 0, 0, 0, 0, 0],
                ..
            }
        ),
        "{out:?}"
    );
    // A normal one stays.
    ap.gtk = vec![0x32; 16];
    ap.gtk_rsc = [9, 1, 0, 0, 0, 0];
    let g = ap.group1();
    rx(&mut sta, &g);
    assert!(matches!(
        &drain(&mut sta)[0],
        Output::InstallGroup {
            rsc: [9, 1, 0, 0, 0, 0],
            ..
        }
    ));
}

#[test]
fn group_tkip_key_gets_its_michael_keys_swapped() {
    let mut ap = ModelAp::new(PASSPHRASE);
    ap.rsn_ie[7] = 2; // group TKIP
    ap.gtk = (0..32).collect();
    let mut sta = associated(&ap, PASSPHRASE);
    let m1 = ap.msg1();
    rx(&mut sta, &m1);
    let m2 = only_tx_data(&mut sta);
    assert_eq!(
        m2[32 + 99 + 7],
        2,
        "our RSN element carries the AP's group cipher"
    );
    assert!(ap.take_msg2(&m2));
    let m3 = ap.msg3();
    rx(&mut sta, &m3);
    let out = drain(&mut sta);
    let key = out.iter().find_map(|o| match o {
        Output::InstallGroup {
            key,
            cipher: GroupCipher::Tkip,
            ..
        } => Some(key),
        _ => None,
    });
    let mut want: Vec<u8> = (0..16).collect();
    want.extend(24..32);
    want.extend(16..24);
    assert_eq!(key.unwrap().expose(), want.as_slice());
}

fn ht_bss(channel: u8, freq: u16, offset_bits: u8, width_any: bool) -> ScanResult {
    let f = beacon(
        0x80,
        &AP,
        SSID,
        channel,
        0x0011,
        &[
            RSN_PSK_CCMP.to_vec(),
            ht_cap_ie(),
            ht_oper_ie(channel, offset_bits, width_any),
            wmm_param_ie(),
        ],
    );
    ScanResult::parse(&f, -50, freq, 0).unwrap()
}

fn hw_ht() -> HwCaps {
    // The shape of ath10k's values for a 2x2 chip with SGI on both widths, LDPC and STBC.
    HwCaps::from_wmi(
        0x0001 | 0x0002 | 0x0008 | 0x0010 | 0x0040 | 0x0800,
        0,
        2,
        3,
        3,
    )
}

#[test]
fn ht_caps_from_wmi_as_ath10k_builds_them() {
    let ht = hw_ht().ht.unwrap();
    assert_eq!(ht.cap_info & 0x0002, 0x0002, "20/40");
    assert_eq!(ht.cap_info & 0x1000, 0x1000, "DSSS/CCK in 40");
    assert_eq!(ht.cap_info & 0x000c, 0x000c, "SM power save disabled");
    assert_eq!(ht.cap_info & 0x0060, 0x0060, "SGI 20 and 40");
    assert_eq!(ht.cap_info & 0x0080, 0x0080, "TX STBC with two chains");
    assert_eq!((ht.cap_info >> 8) & 3, 1, "RX STBC one stream");
    assert_eq!(ht.cap_info & 0x0001, 1, "LDPC");
    assert_eq!(ht.ampdu_params, 3 | (6 << 2));
    assert_eq!(ht.rx_mask[..3], [0xff, 0xff, 0]);
    assert_eq!(ht.tx_params, 1);
    assert_eq!(HwCaps::ht_from_wmi(0, 0, 2, 3, 3), None);
    let single = HwCaps::ht_from_wmi(1 | 8, 0, 2, 1, 1).unwrap();
    assert_eq!(single.rx_mask[..2], [0xff, 0], "one chain");
    assert_eq!(single.cap_info & 0x0080, 0, "no TX STBC with one chain");
}

#[test]
fn association_request_elements_follow_mac80211() {
    let ap = ModelAp::new(PASSPHRASE);
    let mut sta = Station::new(hw_ht(), STA);
    let f = beacon(
        0x80,
        &AP,
        SSID,
        36,
        0x0111,
        &[
            RSN_PSK_CCMP.to_vec(),
            ht_cap_ie(),
            ht_oper_ie(36, 1, true),
            wmm_param_ie(),
        ],
    );
    sta.on_mgmt_rx(&f, -50, 5180, 0);
    sta.connect(SSID, creds(), [1; 32], 0).unwrap();
    let out = drain(&mut sta);
    let Output::JoinBss(j) = &out[0] else {
        panic!();
    };
    assert_eq!(j.width, ChannelWidth::Ht40);
    assert_eq!(j.center_freq_mhz, 5190);
    assert_eq!(j.phy_mode, PhyMode::NaHt40);
    assert!(
        !j.passive,
        "NO_IR on channel 36 was lifted by the beacon just heard"
    );

    sta.on_mgmt_rx(&ap.auth_resp(0), -50, 5180, 1);
    let out = drain(&mut sta);
    let Output::TxMgmt(req) = &out[0] else {
        panic!();
    };
    assert_eq!(&req[..2], &[0, 0]);
    assert_eq!(&req[4..10], &AP);
    assert_eq!(&req[10..16], &STA);
    assert_eq!(&req[16..22], &AP);
    // ESS, privacy, spectrum management because the AP has it, and no short-slot/preamble bits on 5 GHz.
    assert_eq!(
        u16::from_le_bytes([req[24], req[25]]),
        0x0001 | 0x0010 | 0x0100
    );
    assert_eq!(u16::from_le_bytes([req[26], req[27]]), 5);
    let ids: Vec<u8> = elements(&req[28..]).map(|(id, _)| id).collect();
    assert_eq!(ids, [0, 1, 33, 36, 48, 45, 221]);
    let get = |want: u8| elements(&req[28..]).find(|(id, _)| *id == want).unwrap().1;
    assert_eq!(get(0), SSID);
    assert_eq!(get(1), [12, 18, 24, 36, 48, 72, 96, 108]);
    assert_eq!(get(33), [0, 20]);
    assert_eq!(get(36).len(), 25 * 2, "the band's world channels");
    assert_eq!(&get(36)[..4], &[36, 1, 40, 1]);
    assert_eq!(get(221), [0x00, 0x50, 0xf2, 2, 0, 1, 0]);
    let ht = HtCaps::parse(get(45)).unwrap();
    assert_eq!(ht.cap_info & 0x0002, 0x0002, "40 MHz: we plan to use it");
    assert_eq!(ht.cap_info & 0x000c, 0x000c, "SMPS off");
    assert_eq!(ht.rx_mask[..2], [0xff, 0xff]);
}

#[test]
fn twenty_mhz_is_advertised_as_twenty_only() {
    let reg = Regulatory::new();
    let hw = hw_ht();
    for (bss, why) in [
        (ht_bss(6, 2437, 0, false), "no secondary channel"),
        (
            ht_bss(6, 2437, 1, false),
            "the AP's STA channel width says 20",
        ),
        (ht_bss(11, 2462, 1, true), "secondary would be 15"),
        (
            ht_bss(9, 2452, 1, true),
            "secondary 13 is 20 MHz only in the world rule",
        ),
        (ht_bss(165, 5825, 1, true), "no channel 169"),
    ] {
        let plan = lucina_wifi_mac::mlme::plan_connection(&bss, &hw, &reg).unwrap();
        assert_eq!(plan.width, ChannelWidth::Ht20, "{why}");
        assert_eq!(plan.center_freq_mhz, bss.freq_mhz);
        let cap = plan.ht.unwrap().cap_info;
        assert_eq!(cap & 0x0042, 0, "{why}: no 20/40 and no SGI-40");
        assert_eq!(cap & 0x0020, 0x0020, "SGI-20 stays");
    }
    // Below on channel 6 works.
    let plan =
        lucina_wifi_mac::mlme::plan_connection(&ht_bss(6, 2437, 3, true), &hw, &reg).unwrap();
    assert_eq!(
        (plan.width, plan.center_freq_mhz),
        (ChannelWidth::Ht40, 2427)
    );
    // A driver without 40 MHz.
    let mut hw20 = hw;
    hw20.ht40 = false;
    let plan =
        lucina_wifi_mac::mlme::plan_connection(&ht_bss(6, 2437, 3, true), &hw20, &reg).unwrap();
    assert_eq!(plan.width, ChannelWidth::Ht20);
}

#[test]
fn ht_needs_wmm_as_in_mac80211() {
    let f = beacon(
        0x80,
        &AP,
        SSID,
        6,
        0x0411,
        &[RSN_PSK_CCMP.to_vec(), ht_cap_ie(), ht_oper_ie(6, 0, false)],
    );
    let bss = ScanResult::parse(&f, -50, FREQ, 0).unwrap();
    let plan = lucina_wifi_mac::mlme::plan_connection(&bss, &hw_ht(), &Regulatory::new()).unwrap();
    assert_eq!(plan.ht, None);
    assert_eq!(plan.width, ChannelWidth::NoHt20);
    assert!(!plan.wmm);
}

#[test]
fn peer_info_is_what_ath10k_computes() {
    let ap = ModelAp::new(PASSPHRASE);
    let mut sta = Station::new(hw_ht(), STA);
    let f = beacon(
        0x80,
        &AP,
        SSID,
        36,
        0x0011,
        &[
            RSN_PSK_CCMP.to_vec(),
            ht_cap_ie(),
            ht_oper_ie(36, 1, true),
            wmm_param_ie(),
        ],
    );
    sta.on_mgmt_rx(&f, -50, 5180, 0);
    sta.connect(SSID, creds(), [1; 32], 0).unwrap();
    drain(&mut sta);
    sta.on_mgmt_rx(&ap.auth_resp(0), -50, 5180, 1);
    drain(&mut sta);

    // The response leaves HT and WMM out: mac80211 falls back to the beacon's for those.
    sta.on_mgmt_rx(&ap.assoc_resp(0, 2007), -50, 5180, 2);
    let out = drain(&mut sta);
    let Output::PeerAssoc(p) = &out[0] else {
        panic!();
    };
    assert_eq!(p.aid, 2007);
    assert!(p.qos);
    assert_eq!(p.phy_mode, PhyMode::NaHt40);
    assert_eq!(p.num_spatial_streams, 2);
    let ht = p.ht.as_ref().unwrap();
    assert!(ht.bw40);
    assert_eq!(ht.mcs, (0..16).collect::<Vec<u8>>());
    assert_eq!(ht.max_mpdu, (1 << 16) - 1, "factor 3: 2^(13+3) - 1");
    assert_eq!(ht.mpdu_density, 4, "code 5 is 4 us");
    assert!(!ht.ldpc && !ht.stbc, "the AP has neither");
    // HT | CW40 | SGI | DS, from WMI_RC_*.
    assert_eq!(ht.rate_caps, 0x08 | 0x02 | 0x04 | 0x01);
    let wmm = p.wmm_params.unwrap();
    assert_eq!(
        (
            wmm[0].aifsn,
            wmm[0].ecw_min,
            wmm[0].ecw_max,
            wmm[0].txop_limit
        ),
        (3, 4, 10, 0)
    );
    assert_eq!(
        (
            wmm[3].aifsn,
            wmm[3].ecw_min,
            wmm[3].ecw_max,
            wmm[3].txop_limit
        ),
        (2, 2, 3, 47)
    );
    // The response's rates are 2.4 GHz ones; on 5 GHz only the common OFDM ones count.
    assert_eq!(p.legacy_rates, [12, 18, 24, 36, 48, 72, 96, 108]);
    assert_eq!(
        p.basic_rates,
        Vec::<u8>::new(),
        "none of the OFDM rates in this response carry the basic bit"
    );

    // AID out of range: 0, as `broken_ap`.
    let mut sta = connecting(&ap, PASSPHRASE);
    drain(&mut sta);
    sta.on_mgmt_rx(&ap.auth_resp(0), -50, FREQ, 1);
    drain(&mut sta);
    sta.on_mgmt_rx(&ap.assoc_resp(0, 2008), -50, FREQ, 2);
    assert!(
        matches!(&drain(&mut sta)[0], Output::PeerAssoc(p) if p.aid == 0 && p.phy_mode == PhyMode::G11)
    );
}

#[test]
fn country_of_the_ap_restricts_once_associated_and_clears_after() {
    let mut extra = vec![7, 9, b'D', b'K', b' ', 1, 13, 18, 36, 4, 23];
    extra[1] = 9;
    let f = beacon(0x80, &AP, SSID, 6, 0x0411, &[RSN_PSK_CCMP.to_vec(), extra]);
    let ap = ModelAp::new(PASSPHRASE);
    let mut sta = Station::new(hw_basic(), STA);
    sta.on_mgmt_rx(&f, -50, FREQ, 0);
    assert!(sta.channels().iter().any(|c| c.channel == 149));
    sta.connect(SSID, creds(), [1; 32], 0).unwrap();
    drain(&mut sta);
    sta.on_mgmt_rx(&ap.auth_resp(0), -50, FREQ, 1);
    drain(&mut sta);
    sta.on_mgmt_rx(&ap.assoc_resp(0, 1), -50, FREQ, 2);
    drain(&mut sta);
    let list = sta.channels();
    assert!(list.iter().any(|c| c.channel == 13));
    assert!(
        !list.iter().any(|c| c.channel == 149),
        "Denmark's element lists nothing above 48 here"
    );
    assert_eq!(
        list.iter().find(|c| c.channel == 6).unwrap().max_power_dbm,
        18,
        "lower than the world's 20"
    );
    assert_eq!(
        list.iter().find(|c| c.channel == 36).unwrap().max_power_dbm,
        20,
        "23 does not raise the world's"
    );
    sta.disconnect();
    assert!(sta.channels().iter().any(|c| c.channel == 149));
    assert_eq!(sta.regulatory().country(), None);
}

#[test]
fn reconnect_leaves_the_old_network_first() {
    let mut ap = ModelAp::new(PASSPHRASE);
    let mut sta = connected(&mut ap);
    sta.on_mgmt_rx(&ap.beacon_frame(), -50, FREQ, 10);
    sta.connect(SSID, creds(), [2; 32], 10).unwrap();
    let out = drain(&mut sta);
    assert!(matches!(&out[0], Output::TxMgmt(f) if f[0] == 0xc0));
    assert!(matches!(out[1], Output::LeaveBss));
    assert!(matches!(
        out[2],
        Output::Disconnected {
            reason: DisconnectReason::Requested
        }
    ));
    assert!(matches!(out[3], Output::JoinBss(_)));
    assert!(matches!(&out[4], Output::TxMgmt(f) if f[0] == 0xb0));
}

#[test]
fn hex_psk_and_passphrase_are_the_same_key() {
    let ap = ModelAp::new(PASSPHRASE);
    let hex_text: String = ap.pmk.iter().map(|b| format!("{b:02x}")).collect();
    let mut ap = ap;
    let mut sta = Station::new(hw_basic(), STA);
    sta.on_mgmt_rx(&ap.beacon_frame(), -50, FREQ, 0);
    sta.connect(
        SSID,
        Credentials::psk_hex(hex_text.as_bytes()).unwrap(),
        [3; 32],
        0,
    )
    .unwrap();
    drain(&mut sta);
    sta.on_mgmt_rx(&ap.auth_resp(0), -50, FREQ, 1);
    drain(&mut sta);
    sta.on_mgmt_rx(&ap.assoc_resp(0, 5), -50, FREQ, 2);
    drain(&mut sta);
    let m1 = ap.msg1();
    rx(&mut sta, &m1);
    assert!(ap.take_msg2(&only_tx_data(&mut sta)));

    assert_eq!(
        Credentials::psk_hex(b"xyz").unwrap_err(),
        CredentialsError::BadHexPsk
    );
    assert_eq!(
        Credentials::psk_hex(&[b'g'; 64]).unwrap_err(),
        CredentialsError::BadHexPsk
    );
    assert_eq!(
        Credentials::passphrase(b"short").unwrap_err(),
        CredentialsError::BadPassphrase
    );
    assert_eq!(
        Credentials::passphrase(&[b'a'; 64]).unwrap_err(),
        CredentialsError::BadPassphrase
    );
    assert_eq!(
        Credentials::passphrase(b"has\x07control").unwrap_err(),
        CredentialsError::BadPassphrase
    );
}

#[test]
fn entropy_decides_the_snonce() {
    let ap = ModelAp::new(PASSPHRASE);
    let nonce = |entropy: [u8; 32]| {
        let mut ap = ModelAp::new(PASSPHRASE);
        let mut sta = Station::new(hw_basic(), STA);
        sta.on_mgmt_rx(&ap.beacon_frame(), -50, FREQ, 0);
        sta.connect(SSID, creds(), entropy, 0).unwrap();
        drain(&mut sta);
        sta.on_mgmt_rx(&ap.auth_resp(0), -50, FREQ, 1);
        drain(&mut sta);
        sta.on_mgmt_rx(&ap.assoc_resp(0, 5), -50, FREQ, 2);
        drain(&mut sta);
        let m1 = ap.msg1();
        rx(&mut sta, &m1);
        only_tx_data(&mut sta)[32 + 17..32 + 49].to_vec()
    };
    let _ = ap;
    assert_ne!(nonce([1; 32]), nonce([2; 32]));
    assert_eq!(
        nonce([1; 32]),
        nonce([1; 32]),
        "no hidden state: same entropy, same nonce"
    );
    assert_ne!(nonce([1; 32]), vec![0u8; 32]);
}

#[test]
fn sequence_numbers_count_across_frames() {
    let ap = ModelAp::new(PASSPHRASE);
    let mut sta = connecting(&ap, PASSPHRASE);
    let out = drain(&mut sta);
    let Output::TxMgmt(a1) = &out[1] else {
        panic!();
    };
    sta.tick(AUTH_TIMEOUT_LONG_MS);
    let out = drain(&mut sta);
    let Output::TxMgmt(a2) = &out[0] else {
        panic!();
    };
    let seq = |f: &[u8]| u16::from_le_bytes([f[22], f[23]]) >> 4;
    assert_eq!(seq(a2), seq(a1) + 1);
    assert_eq!(a2[22] & 0x0f, 0, "fragment 0");
}

// ---- VHT ------------------------------------------------------------------------------------------------------------

/// The shape of ath10k service ready on a 2x2 chip: 80 MHz, short guard interval, LDPC, RX STBC, beamformee.
const VHT_INFO: u32 = 0x0000_1120;

fn hw_vht() -> HwCaps {
    HwCaps::from_wmi(
        0x0001 | 0x0002 | 0x0008 | 0x0010 | 0x0040 | 0x0800,
        VHT_INFO,
        2,
        3,
        3,
    )
}

/// Channel 52 as the laptop has seen it, with the secondary above and the 80 MHz centred on channel 58.
fn vht_bss(width: u8, seg0: u8) -> lucina_wifi_mac::ScanResult {
    let mut bss = ht_bss(52, 5260, 1, true);
    let mut cap = 0x0000_01a0u32.to_le_bytes().to_vec();
    cap.extend_from_slice(&[0xfa, 0xff, 0, 0, 0xfa, 0xff, 0, 0]);
    bss.vht_cap = Some(cap);
    bss.vht_oper = Some(vec![width, seg0, 0, 0xfa, 0xff]);
    bss
}

#[test]
fn vht_caps_from_wmi_as_ath10k_builds_them() {
    let vht = hw_vht().vht.unwrap();
    assert_eq!(vht.rx_mcs_map, 0xfffa, "MCS 0-9 on two streams");
    assert_eq!(vht.tx_mcs_map, 0xfffa);
    assert_eq!(
        (vht.cap >> 13) & 7,
        3,
        "a beamformee that names no streams has up to 4"
    );
    assert_eq!(HwCaps::vht_from_wmi(0, 2, 3), None, "no VHT on this chip");
    let one_chain = HwCaps::vht_from_wmi(VHT_INFO | (1 << 7), 2, 1).unwrap();
    assert_eq!(one_chain.rx_mcs_map, 0xfffe);
    assert_eq!(one_chain.cap & (1 << 7), 0, "no TX STBC with one chain");
}

#[test]
fn an_ap_on_80_mhz_is_joined_on_80_mhz() {
    let reg = Regulatory::new();
    let plan = lucina_wifi_mac::mlme::plan_connection(&vht_bss(1, 58), &hw_vht(), &reg).unwrap();
    assert_eq!(plan.width, lucina_wifi_mac::ChannelWidth::Vht80);
    assert_eq!(plan.center_freq_mhz, 5290);
    assert!(plan.vht.is_some());
}

#[test]
fn anything_less_stays_on_40_mhz() {
    let reg = Regulatory::new();
    let plan = |bss, hw: &HwCaps| lucina_wifi_mac::mlme::plan_connection(&bss, hw, &reg).unwrap();
    for (name, bss, hw) in [
        ("an AP on 40 MHz", vht_bss(0, 0), hw_vht()),
        ("160 MHz in the old form", vht_bss(2, 50), hw_vht()),
        ("a centre that leaves the primary out", vht_bss(1, 42), hw_vht()),
        ("no such channels", vht_bss(1, 200), hw_vht()),
        ("a chip without VHT", vht_bss(1, 58), hw_ht()),
        ("an AP without VHT", ht_bss(52, 5260, 1, true), hw_vht()),
    ] {
        let got = plan(bss, &hw);
        assert_eq!(got.width, lucina_wifi_mac::ChannelWidth::Ht40, "{name}");
        assert_eq!(got.center_freq_mhz, 5270, "{name}");
        assert_eq!(got.vht, None, "{name}");
    }
}

#[test]
fn the_association_request_carries_vht_only_on_80_mhz() {
    let reg = Regulatory::new();
    let hw = hw_vht();
    let bss = vht_bss(1, 58);
    let plan = lucina_wifi_mac::mlme::plan_connection(&bss, &hw, &reg).unwrap();
    let frame = lucina_wifi_mac::mlme::build_assoc_req(&bss, &STA, &hw, &plan, None, 20);
    let ies = &frame[24 + 4..];
    let vht = lucina_wifi_mac::ie::find(ies, lucina_wifi_mac::ie::EID_VHT_CAPABILITY).unwrap();
    assert_eq!(vht.len(), 12);
    assert_eq!(
        lucina_wifi_mac::ie::VhtCaps::parse(vht).unwrap(),
        hw.vht.unwrap()
    );

    let bss = vht_bss(0, 0);
    let plan = lucina_wifi_mac::mlme::plan_connection(&bss, &hw, &reg).unwrap();
    let frame = lucina_wifi_mac::mlme::build_assoc_req(&bss, &STA, &hw, &plan, None, 20);
    assert!(lucina_wifi_mac::ie::find(&frame[24 + 4..], lucina_wifi_mac::ie::EID_VHT_CAPABILITY).is_none());
}

#[test]
fn vht_rates_are_what_both_ends_can_do() {
    let own = hw_vht().vht.unwrap();
    let peer = lucina_wifi_mac::ie::VhtCaps {
        // Three streams at MCS 0-8, with a short guard interval and TX STBC, a 7-subframe limit in the exponent.
        cap: (1 << 5) | (1 << 7) | (7 << 23),
        rx_mcs_map: 0xffd5,
        tx_mcs_map: 0xffd5,
        ..Default::default()
    };
    let got = lucina_wifi_mac::caps::intersect_vht(&own, &peer).unwrap();
    assert_eq!(got.rx_mcs_map, 0xfff5, "two streams, the lower of 0-8 and 0-9");
    assert_eq!(got.tx_mcs_map, 0xfff5);
    assert_ne!(got.cap & (1 << 5), 0, "both have the short guard interval");
    assert_eq!((got.cap >> 23) & 7, 7, "the exponent is taken as it is");
    assert_ne!(got.cap & (1 << 7), 0, "our RX STBC takes their TX STBC");
    let nothing = lucina_wifi_mac::ie::VhtCaps {
        rx_mcs_map: 0xffff,
        tx_mcs_map: 0xffff,
        ..Default::default()
    };
    assert_eq!(lucina_wifi_mac::caps::intersect_vht(&own, &nothing), None);
}

/// An open network (no RSN element, no privacy bit, open credentials), associated at t=2.
fn open_connected(hw: HwCaps) -> Station {
    let ap = ModelAp::new(PASSPHRASE);
    let mut sta = Station::new(hw, STA);
    sta.on_mgmt_rx(&beacon(0x80, &AP, SSID, 6, 0x0401, &[]), -50, FREQ, 0);
    sta.connect(SSID, Credentials::open(), [1; 32], 0).unwrap();
    drain(&mut sta);
    sta.on_mgmt_rx(&ap.auth_resp(0), -50, FREQ, 1);
    drain(&mut sta);
    sta.on_mgmt_rx(&ap.assoc_resp(0, 5), -50, FREQ, 2);
    drain(&mut sta);
    assert_eq!(sta.state(), StationState::Connected);
    sta
}

#[test]
fn next_deadline_is_none_without_a_connection() {
    let sta = Station::new(hw_basic(), STA);
    assert_eq!(sta.next_deadline(), None);
}

#[test]
fn next_deadline_while_authenticating_is_the_auth_retry() {
    let ap = ModelAp::new(PASSPHRASE);
    let mut sta = connecting(&ap, PASSPHRASE);
    assert_eq!(drain(&mut sta).len(), 2, "join and the first auth");
    let Some(d) = sta.next_deadline() else {
        panic!("the auth retry is due")
    };
    assert_eq!(d, AUTH_TIMEOUT_LONG_MS);
    sta.tick(d - 1);
    assert!(drain(&mut sta).is_empty(), "one ms short: no resend");
    sta.tick(d);
    assert!(
        matches!(&drain(&mut sta)[0], Output::TxMgmt(f) if f[0] == 0xb0),
        "at the deadline the authentication goes out again"
    );
}

#[test]
fn next_deadline_with_the_firmware_monitor_waits_for_its_loss_report() {
    let mut hw = hw_basic();
    hw.connection_monitor = true;
    let mut sta = open_connected(hw);
    // The firmware owns the silence timer: nothing is due here.
    assert_eq!(sta.next_deadline(), None);
    let t = 10_000;
    sta.on_beacon_loss(t);
    // on_beacon_loss ticks at once: the first directed probe has already gone out, and the
    // next one is due PROBE_WAIT_MS later.
    assert!(
        matches!(&drain(&mut sta)[0], Output::TxMgmt(f) if f[0] == 0x40 && f[4..10] == AP),
        "directed probe"
    );
    assert_eq!(sta.next_deadline(), Some(t + PROBE_WAIT_MS));
    sta.tick(t);
    assert!(drain(&mut sta).is_empty(), "too early for the next probe");
    assert_eq!(sta.next_deadline(), Some(t + PROBE_WAIT_MS));
}

#[test]
fn next_deadline_without_a_monitor_is_the_beacon_silence() {
    let mut sta = open_connected(hw_basic()); // connection_monitor: false
    // The last beacon is the association at t=2; the model beacon advertises 100 TU.
    assert_eq!(sta.next_deadline(), Some(2 + BEACON_LOSS_COUNT * 100 * 1024 / 1000));
}
