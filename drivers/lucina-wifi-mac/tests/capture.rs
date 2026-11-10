//! A real exchange: `wpa-Induction.pcap` from Wireshark's sample captures, a real AP (00:0c:41:82:b2:55) and client
//! (00:0d:93:82:36:3a) on SSID "Coherer", passphrase "Induction", both published on that page. The capture was made
//! by someone else on hardware we have never touched, so nothing in it was shaped by this code. See the fixture's
//! header for the source and hash.
//!
//! The capture's client differs from wpa_supplicant in two choices, EAPOL version 2 and a Key Length of 16 in
//! messages 2 and 4. Comparisons account for that by setting those two fields and re-signing, and then require
//! byte equality with the capture.

use lucina_wifi_mac::crypto::aes::Aes128;
use lucina_wifi_mac::wpa::{
    Akm, EapolKey, GroupCipher, Ptk, compute_mic, derive_psk, pmkid, verify_mic,
};
use lucina_wifi_mac::{
    Credentials, DisconnectReason, HwCaps, Output, RxFrame, RxMeta, ScanResult, Security, Station,
    StationState,
};

const AP: [u8; 6] = [0x00, 0x0c, 0x41, 0x82, 0xb2, 0x55];
const STA: [u8; 6] = [0x00, 0x0d, 0x93, 0x82, 0x36, 0x3a];
/// LLC/SNAP after a 24-byte header: EAPOL starts 32 bytes in.
const EAPOL_OFFSET: usize = 32;

fn hex(s: &str) -> Vec<u8> {
    (0..s.len() / 2)
        .map(|i| u8::from_str_radix(&s[i * 2..i * 2 + 2], 16).unwrap())
        .collect()
}

fn frame(number: u32) -> Vec<u8> {
    let fixture = include_str!("fixtures/wpa-induction.txt");
    for line in fixture.lines().filter(|l| !l.starts_with('#')) {
        let (n, h) = line.split_once(' ').unwrap();
        if n.parse::<u32>().unwrap() == number {
            return hex(h);
        }
    }
    panic!("frame {number} is not in the fixture");
}

fn pmk() -> [u8; 32] {
    *derive_psk(b"Induction", b"Coherer").expose()
}

fn eapol_of(number: u32) -> Vec<u8> {
    let f = frame(number);
    // The fixture keeps padding; the PDU's own length decides.
    let key = EapolKey::parse(&f[EAPOL_OFFSET..]).unwrap();
    key.pdu.to_vec()
}

fn nonce_of(number: u32) -> [u8; 32] {
    let e = eapol_of(number);
    EapolKey::parse(&e).unwrap().nonce.try_into().unwrap()
}

fn ptk() -> Ptk {
    Ptk::derive(Akm::Psk, &pmk(), &AP, &STA, &nonce_of(87), &nonce_of(89))
}

fn hw() -> HwCaps {
    HwCaps {
        vht: None,
        band_2ghz: true,
        band_5ghz: false,
        ht: None,
        reports_tx_ack: true,
        spectrum_mgmt: true,
        ht40: false,
        listen_interval: 5,
        connection_monitor: false,
    }
}

#[test]
fn pmk_and_pmkid() {
    // PMK value from Python's hashlib; the MICs in the next test are what prove it matches this network.
    assert_eq!(
        pmk().to_vec(),
        hex("a288fcf0caaacda9a9f58633ff35e8992a01d9c10ba5e02efdf8cb5d730ce7bc")
    );
    // The formula, against Python's hmac. This AP's message 1 does carry a PMKID KDE, but its value is not this: it
    // does not come from the PSK by 802.11's formula (its MICs do). Nothing relies on it: wpa_supplicant ignores
    // the PMKID on PSK, and so does this supplicant, as the replay below shows.
    assert_eq!(
        pmkid(&pmk(), &AP, &STA).to_vec(),
        hex("e3872f0daf57ddd88d936865f72af980")
    );
    let msg1 = eapol_of(87);
    let key = EapolKey::parse(&msg1).unwrap();
    assert_eq!(&key.key_data[..6], &[0xdd, 0x14, 0x00, 0x0f, 0xac, 0x04]);
}

#[test]
fn ptk_verifies_every_mic_in_the_capture() {
    let ptk = ptk();
    for n in [89, 92, 94] {
        assert!(verify_mic(Akm::Psk, ptk.kck(), &eapol_of(n)), "frame {n}");
    }
    // A wrong passphrase, or one flipped bit, does not.
    let wrong = Ptk::derive(
        Akm::Psk,
        derive_psk(b"Induction!", b"Coherer").expose(),
        &AP,
        &STA,
        &nonce_of(87),
        &nonce_of(89),
    );
    assert!(!verify_mic(Akm::Psk, wrong.kck(), &eapol_of(92)));
    let mut bad = eapol_of(92);
    bad[50] ^= 1;
    assert!(!verify_mic(Akm::Psk, ptk.kck(), &bad));
}

#[test]
fn beacon_parses_as_a_real_ap_sent_it() {
    let b = ScanResult::parse(&frame(1), -40, 2412, 0).unwrap();
    assert_eq!(b.bssid, AP);
    assert_eq!(b.ssid, b"Coherer");
    assert_eq!(b.channel, 1);
    assert_eq!(b.beacon_interval_tu, 100);
    assert_eq!(b.security, Security::Wpa2Psk);
    assert_eq!(b.supported_rates, hex("82848b962430486c0c121860"));
    assert_eq!(
        b.rsn_ie.unwrap(),
        hex("30180100000fac020200000fac04000fac020100000fac020000")
    );
    let rsn = b.rsn.unwrap();
    assert_eq!(rsn.group, 0x000f_ac02);
    assert_eq!(rsn.pairwise, [0x000f_ac04, 0x000f_ac02]);
    assert_eq!(rsn.akm, [0x000f_ac02]);
    assert!(!rsn.mfp_capable());
    assert_eq!(b.dtim_period, 1);
    assert!(b.wmm.is_none());
    assert!(b.ht_cap.is_none());
}

/// Message N as this code built it, changed only in the two choices the captured client made differently, re-signed.
fn as_the_capture_would_send(mut pdu: Vec<u8>, ptk: &Ptk) -> Vec<u8> {
    assert_eq!(pdu[0], 1, "EAPOL version as wpa_supplicant sends it");
    assert_eq!(&pdu[7..9], &[0, 0], "Key Length as wpa_supplicant sends it");
    pdu[0] = 2;
    pdu[8] = 16;
    pdu[81..97].fill(0);
    let mic = compute_mic(Akm::Psk, ptk.kck(), &pdu);
    pdu[81..97].copy_from_slice(&mic);
    pdu
}

/// The whole captured exchange, with the station in the client's place: every frame the AP really sent goes in, and
/// everything the station emits is compared with what the client really sent.
#[test]
fn station_replays_the_captured_association() {
    let mut sta = Station::new(hw(), STA);
    let mut now = 1000;
    sta.on_mgmt_rx(&frame(1), -40, 2412, now);
    sta.set_fixed_snonce(nonce_of(89));
    assert_eq!(
        sta.connect(
            b"Coherer",
            Credentials::passphrase(b"Induction").unwrap(),
            [0; 32],
            now
        ),
        Ok(AP)
    );

    let Some(Output::JoinBss(join)) = sta.poll_output() else {
        panic!("JoinBss first")
    };
    assert_eq!((join.bssid, join.freq_mhz), (AP, 2412));
    // Capability 0x0411: ESS, privacy, short slot. No short preamble.
    assert!(join.short_slot && !join.short_preamble);
    let Some(Output::TxMgmt(auth)) = sta.poll_output() else {
        panic!("auth next")
    };
    // Same body and addresses as frame 78; only sequence numbers differ.
    let captured = frame(78);
    assert_eq!(auth[4..22], captured[4..22]);
    assert_eq!(auth[24..], captured[24..]);
    assert_eq!(auth[..2], captured[..2]);
    assert_eq!(sta.state(), StationState::Authenticating);

    now += 5;
    sta.on_mgmt_rx(&frame(80), -40, 2412, now);
    let Some(Output::TxMgmt(assoc)) = sta.poll_output() else {
        panic!("assoc request next")
    };
    let captured = frame(82);
    // Same frame type, addresses, capability bits (0x0431), SSID and RSN element as the real client's.
    assert_eq!(assoc[..2], captured[..2]);
    assert_eq!(assoc[4..22], captured[4..22]);
    assert_eq!(assoc[24..26], captured[24..26]);
    let rsn = hex("30140100000fac020100000fac040100000fac020000");
    let contains = |h: &[u8], n: &[u8]| h.windows(n.len()).any(|w| w == n);
    assert!(contains(&assoc, &rsn) && contains(&captured, &rsn));
    assert!(contains(&assoc, &hex("0007436f6865726572")));
    assert_eq!(sta.state(), StationState::Associating);

    now += 5;
    sta.on_mgmt_rx(&frame(84), -40, 2412, now);
    let Some(Output::PeerAssoc(peer)) = sta.poll_output() else {
        panic!("peer assoc next")
    };
    assert_eq!(
        peer.aid, 1,
        "0xc001 on the air: the top two bits are not the AID"
    );
    assert_eq!(peer.addr, AP);
    assert_eq!(peer.capability, 0x0411);
    assert!(peer.need_ptk_4way);
    assert_eq!(peer.legacy_rates.len(), 12);
    assert_eq!(peer.basic_rates, [2, 4, 11, 22]);
    assert_eq!(sta.state(), StationState::Handshaking);

    let ptk = ptk();
    sta.on_data_rx(RxFrame::NativeWifi(&frame(87)), RxMeta { decrypted: false });
    let Some(Output::TxData {
        frame: msg2,
        eapol: true,
    }) = sta.poll_output()
    else {
        panic!("message 2")
    };
    assert_eq!(msg2[1] & 0x40, 0, "message 2 is not protected");
    assert_eq!(
        as_the_capture_would_send(msg2[EAPOL_OFFSET..].to_vec(), &ptk),
        eapol_of(89)
    );

    sta.on_data_rx(RxFrame::NativeWifi(&frame(92)), RxMeta { decrypted: false });
    let Some(Output::TxData {
        frame: msg4,
        eapol: true,
    }) = sta.poll_output()
    else {
        panic!("message 4")
    };
    assert_eq!(
        msg4[1] & 0x40,
        0,
        "message 4 of a first handshake is not protected"
    );
    assert_eq!(
        as_the_capture_would_send(msg4[EAPOL_OFFSET..].to_vec(), &ptk),
        eapol_of(94)
    );

    let Some(Output::InstallPairwise { peer, tk }) = sta.poll_output() else {
        panic!("pairwise key")
    };
    assert_eq!(peer, AP);
    assert_eq!(*tk.expose(), *ptk.tk().expose());
    let Some(Output::InstallGroup {
        idx,
        key,
        cipher,
        rsc,
    }) = sta.poll_output()
    else {
        panic!("group key")
    };
    assert_eq!(cipher, GroupCipher::Tkip);
    assert_eq!(key.len(), 32);
    assert_eq!(
        idx, 2,
        "the KDE's key id: the AP's broadcasts in the capture use key index 2"
    );
    assert_eq!(rsc, [0xcf, 0x02, 0, 0, 0, 0], "Key RSC of frame 92");
    assert!(matches!(sta.poll_output(), Some(Output::PortAuthorized)));
    assert!(sta.poll_output().is_none());
    assert_eq!(sta.state(), StationState::Connected);

    // The next thing the client did was send a DHCP request: decrypt it with the TK the station came up with.
    let plain = ccmp_decrypt(&frame(99), tk.expose());
    assert_eq!(&plain[..8], &[0xaa, 0xaa, 0x03, 0, 0, 0, 0x08, 0x00]);
    let plain = ccmp_decrypt(&frame(102), tk.expose());
    assert_eq!(&plain[..8], &[0xaa, 0xaa, 0x03, 0, 0, 0, 0x08, 0x00]);
    // After the LLC comes IPv4, and past its 20-byte header and UDP's 8 bytes the DHCP magic cookie at offset 236.
    let ip = &plain[8..];
    assert_eq!(ip[0], 0x45);
    assert_eq!(&ip[20 + 8 + 236..20 + 8 + 240], &[0x63, 0x82, 0x53, 0x63]);

    // The same frame as data, after decryption, through the station as native WiFi.
    let mut nwifi = frame(102)[..24].to_vec();
    nwifi[1] &= !0x40;
    nwifi.extend_from_slice(&plain);
    sta.on_data_rx(RxFrame::NativeWifi(&nwifi), RxMeta { decrypted: true });
    let Some(Output::RxEthernet(eth)) = sta.poll_output() else {
        panic!("decrypted DHCP ACK comes out as Ethernet")
    };
    assert_eq!(&eth[..6], &STA);
    assert_eq!(
        &eth[6..12],
        &[0x00, 0x0c, 0x41, 0x82, 0xb2, 0x53],
        "SA is address 3 in From-DS"
    );
    assert_eq!(&eth[12..14], &[0x08, 0x00]);

    sta.disconnect();
    assert!(matches!(sta.poll_output(), Some(Output::TxMgmt(_))));
    assert!(matches!(sta.poll_output(), Some(Output::LeaveBss)));
    assert!(matches!(
        sta.poll_output(),
        Some(Output::Disconnected {
            reason: DisconnectReason::Requested
        })
    ));
}

/// Enough CCMP (802.11-2020 12.5.3, RFC 3610 with M = 8, L = 2) to prove a TK is right on real traffic: the crate
/// leaves CCMP to the hardware. Checks the MIC and returns the plaintext.
fn ccmp_decrypt(f: &[u8], tk: &[u8; 16]) -> Vec<u8> {
    let aes = Aes128::new(tk);
    let hdr = &f[..24];
    let iv = &f[24..32];
    let cipher = &f[32..f.len() - 8];
    let mic = &f[f.len() - 8..];
    assert_ne!(iv[3] & 0x20, 0, "ExtIV bit of a CCMP header");
    let pn = [iv[7], iv[6], iv[5], iv[4], iv[1], iv[0]];
    let mut nonce = [0u8; 13];
    nonce[1..7].copy_from_slice(&hdr[10..16]);
    nonce[7..13].copy_from_slice(&pn);

    let block_a = |ctr: u16| {
        let mut a = [0u8; 16];
        a[0] = 1;
        a[1..14].copy_from_slice(&nonce);
        a[14..].copy_from_slice(&ctr.to_be_bytes());
        a
    };
    let mut plain = cipher.to_vec();
    for (i, chunk) in plain.chunks_mut(16).enumerate() {
        let mut s = block_a(i as u16 + 1);
        aes.encrypt_block(&mut s);
        for (p, k) in chunk.iter_mut().zip(s.iter()) {
            *p ^= k;
        }
    }

    // AAD: frame control with subtype, retry, power and more-data masked, protected kept; addresses; fragment number.
    let mut aad = vec![hdr[0] & 0x8f, hdr[1] & 0xc7];
    aad.extend_from_slice(&hdr[4..22]);
    aad.extend_from_slice(&[hdr[22] & 0x0f, 0]);

    let mut x = [0u8; 16];
    x[0] = 0x59;
    x[1..14].copy_from_slice(&nonce);
    x[14..].copy_from_slice(&(plain.len() as u16).to_be_bytes());
    aes.encrypt_block(&mut x);
    let mut aad_blocks = (aad.len() as u16).to_be_bytes().to_vec();
    aad_blocks.extend_from_slice(&aad);
    for src in [aad_blocks.as_slice(), plain.as_slice()] {
        for chunk in src.chunks(16) {
            for (a, b) in x.iter_mut().zip(chunk.iter()) {
                *a ^= b;
            }
            aes.encrypt_block(&mut x);
        }
    }
    let mut s0 = block_a(0);
    aes.encrypt_block(&mut s0);
    let got: Vec<u8> = x[..8].iter().zip(s0.iter()).map(|(a, b)| a ^ b).collect();
    assert_eq!(got, mic, "CCMP MIC");
    plain
}
