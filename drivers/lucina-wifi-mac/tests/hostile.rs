//! Air is hostile: nothing received may panic, in any state. Every real and model frame, cut at every length and with
//! bytes corrupted, and a deterministic stream of garbage, thrown at every entry point.

mod common;

use common::*;
use lucina_wifi_mac::data::{nwifi_to_ethernet, raw80211_to_ethernet, split_amsdu};
use lucina_wifi_mac::ie::{HtCaps, HtOperation, RsnInfo, elements, parse_wmm};
use lucina_wifi_mac::mlme::AssocRespBody;
use lucina_wifi_mac::regulatory::Country;
use lucina_wifi_mac::wpa::EapolKey;
use lucina_wifi_mac::*;

fn hex(s: &str) -> Vec<u8> {
    (0..s.len() / 2)
        .map(|i| u8::from_str_radix(&s[i * 2..i * 2 + 2], 16).unwrap())
        .collect()
}

/// A small xorshift, so a failure replays.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn bytes(&mut self, len: usize) -> Vec<u8> {
        (0..len).map(|_| self.next() as u8).collect()
    }
}

fn capture_frames() -> Vec<Vec<u8>> {
    include_str!("fixtures/wpa-induction.txt")
        .lines()
        .filter(|l| !l.starts_with('#'))
        .map(|l| hex(l.split_once(' ').unwrap().1))
        .collect()
}

/// A station in each state, freshly made so one test's frames do not change another's.
fn stations() -> Vec<Station> {
    let mut ap = ModelAp::new(PASSPHRASE);
    let mut list = vec![Station::new(hw_basic(), STA)];
    let mut heard = Station::new(hw_basic(), STA);
    heard.on_mgmt_rx(&ap.beacon_frame(), -50, FREQ, 0);
    list.push(heard);
    list.push(connecting(&ap, PASSPHRASE));
    let mut assoc = connecting(&ap, PASSPHRASE);
    assoc.on_mgmt_rx(&ap.auth_resp(0), -50, FREQ, 1);
    list.push(assoc);
    list.push(associated(&ap, PASSPHRASE));
    let mut half = associated(&ap, PASSPHRASE);
    let m1 = ap.msg1();
    rx(&mut half, &m1);
    list.push(half);
    list.push(connected(&mut ap));
    list
}

fn feed(sta: &mut Station, f: &[u8], now: u64) {
    sta.on_mgmt_rx(f, -50, FREQ, now);
    sta.on_data_rx(RxFrame::NativeWifi(f), RxMeta { decrypted: true });
    sta.on_data_rx(RxFrame::NativeWifi(f), RxMeta { decrypted: false });
    sta.on_data_rx(
        RxFrame::Raw80211 {
            frame: f,
            crypto_header: 0,
        },
        RxMeta { decrypted: true },
    );
    sta.on_data_rx(
        RxFrame::Raw80211 {
            frame: f,
            crypto_header: 8,
        },
        RxMeta { decrypted: true },
    );
    sta.on_data_rx(RxFrame::Ethernet(f), RxMeta { decrypted: true });
    while sta.poll_output().is_some() {}
}

fn parsers(f: &[u8]) {
    let _ = ScanResult::parse(f, -50, 2437, 0);
    let _ = ScanResult::parse(f, -50, 5180, 0);
    let _ = EapolKey::parse(f);
    let _ = RsnInfo::parse(f);
    let _ = HtCaps::parse(f);
    let _ = HtOperation::parse(f);
    let _ = Country::parse(f);
    let _ = parse_wmm(f);
    let _ = AssocRespBody::parse(f);
    let _ = nwifi_to_ethernet(f);
    let _ = raw80211_to_ethernet(f, 8);
    let _ = split_amsdu(f, Some(&STA), Some(&AP));
    let _ = elements(f).count();
}

#[test]
fn truncated_frames_never_panic_in_any_state() {
    let mut ap = ModelAp::new(PASSPHRASE);
    let m1 = ap.msg1();
    let mut sta = associated(&ap, PASSPHRASE);
    rx(&mut sta, &m1);
    assert!(ap.take_msg2(&only_tx_data(&mut sta)));
    let m3 = ap.msg3();
    let g = ap.group1();
    let mut frames = capture_frames();
    frames.extend([
        ap.beacon_frame(),
        ap.auth_resp(0),
        ap.assoc_resp(0, 3),
        ap.deauth(1),
        m1,
        m3,
        g,
    ]);
    frames.push(beacon(
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
    ));
    for mut sta in stations() {
        for f in &frames {
            for len in 0..=f.len() {
                parsers(&f[..len]);
                feed(&mut sta, &f[..len], 10);
            }
        }
    }
}

#[test]
fn eapol_key_with_lying_lengths() {
    let mut ap = ModelAp::new(PASSPHRASE);
    let m1 = ap.msg1();
    let mut sta = associated(&ap, PASSPHRASE);
    rx(&mut sta, &m1);
    assert!(ap.take_msg2(&only_tx_data(&mut sta)));
    let m3 = ap.msg3();
    let eapol = &m3[32..];
    // Body length and key data length: too long, too short, zero, huge.
    for off in [2usize, 97] {
        for value in [0u16, 1, 94, 95, 96, 200, 0xffff] {
            let mut f = m3.clone();
            f[32 + off..32 + off + 2].copy_from_slice(&value.to_be_bytes());
            rx(&mut sta, &f);
            assert_eq!(
                drain(&mut sta)
                    .iter()
                    .filter(|o| matches!(o, Output::InstallPairwise { .. }))
                    .count(),
                0,
                "off {off} value {value}"
            );
        }
    }
    assert!(EapolKey::parse(&eapol[..98]).is_none());
    // A wrong packet or descriptor type never gets anywhere.
    for (i, v) in [(1usize, 0u8), (4, 254)] {
        let mut e = eapol.to_vec();
        e[i] = v;
        assert!(EapolKey::parse(&e).is_none());
    }
    // Padding after the PDU is outside the MIC and does not change anything.
    let mut padded = m3.clone();
    padded.extend_from_slice(&[0; 7]);
    rx(&mut sta, &padded);
    assert!(
        drain(&mut sta)
            .iter()
            .any(|o| matches!(o, Output::InstallPairwise { .. }))
    );
}

#[test]
fn garbage_stream_in_every_state() {
    let mut rng = Rng(0x1234_5678_9abc_def1);
    for mut sta in stations() {
        for round in 0..3000u64 {
            let len = (rng.next() % 300) as usize;
            let mut f = rng.bytes(len);
            // Half the time, aim at the frame classes that get past the first checks.
            if round % 2 == 0 && f.len() >= 24 {
                f[0] = [0x00u8, 0x10, 0x50, 0x80, 0xa0, 0xb0, 0xc0, 0x08, 0x88]
                    [(rng.next() % 9) as usize];
                f[1] = [0u8, 2][(rng.next() % 2) as usize];
                f[4..10].copy_from_slice(&STA);
                f[10..16].copy_from_slice(&AP);
                f[16..22].copy_from_slice(&AP);
            }
            parsers(&f);
            feed(&mut sta, &f, 100 + round);
            if round % 50 == 0 {
                sta.tick(100 + round * 20);
                sta.on_mgmt_tx_status(round % 100 == 0, 100 + round * 20);
                while sta.poll_output().is_some() {}
            }
        }
    }
}

#[test]
fn bit_flips_of_real_frames_never_panic() {
    let mut rng = Rng(0xfeed_face_cafe_beef);
    let frames = capture_frames();
    for mut sta in stations() {
        for f in &frames {
            for _ in 0..200 {
                let mut g = f.clone();
                for _ in 0..1 + rng.next() % 4 {
                    let i = rng.next() as usize % g.len();
                    g[i] ^= 1 << (rng.next() % 8);
                }
                parsers(&g);
                feed(&mut sta, &g, 10);
            }
        }
    }
}

#[test]
fn time_going_backwards_or_jumping_is_harmless() {
    for mut sta in stations() {
        for now in [u64::MAX / 2, 0, 5, u64::MAX / 4, 1] {
            sta.tick(now);
            sta.on_mgmt_tx_status(true, now);
            sta.on_beacon_loss(now);
            sta.on_mic_failure(true, now);
            while sta.poll_output().is_some() {}
        }
    }
}
