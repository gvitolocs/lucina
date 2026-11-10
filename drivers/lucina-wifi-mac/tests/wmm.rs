//! WMM as mac80211 does it for a station: `ieee80211_set_wmm_default`, the checks of `_ieee80211_sta_wmm_params` and
//! following the parameter set count in beacons. Values read off `net/mac80211/util.c` and `mlme.c`.

mod common;

use common::*;
use lucina_wifi_mac::ie::AcParams;
use lucina_wifi_mac::mlme::{use_11b, validate_wmm, wmm_default};
use lucina_wifi_mac::*;

fn ac(aifsn: u8, ecw_min: u8, ecw_max: u8, txop_limit: u16) -> AcParams {
    AcParams {
        aifsn,
        acm: false,
        ecw_min,
        ecw_max,
        txop_limit,
    }
}

#[test]
fn defaults_are_table_7_37() {
    // no QoS (interface up, disassociation): aifs 2, cw 15..1023, txop 0 everywhere
    assert_eq!(wmm_default(false, false), [ac(2, 4, 10, 0); 4]);
    assert_eq!(wmm_default(false, true), [ac(2, 5, 10, 0); 4]);
    // QoS (association without a valid set), BE, BK, VI, VO
    assert_eq!(
        wmm_default(true, false),
        [ac(3, 4, 10, 0), ac(7, 4, 10, 0), ac(2, 3, 4, 94), ac(2, 2, 3, 47)]
    );
    // 802.11b: aCWmin 31 and the longer TXOPs
    assert_eq!(
        wmm_default(true, true),
        [ac(3, 5, 10, 0), ac(7, 5, 10, 0), ac(2, 4, 5, 188), ac(2, 3, 4, 102)]
    );
}

#[test]
fn use_11b_needs_2ghz_and_nothing_above_11_mbps() {
    assert!(use_11b(true, &[0x82, 0x84, 0x8b, 0x96]));
    // 6 and 9 Mb/s are not above 11
    assert!(use_11b(true, &[0x82, 0x84, 12, 18]));
    assert!(!use_11b(true, &[0x82, 0x84, 0x8b, 0x96, 24]));
    assert!(!use_11b(false, &[0x82, 0x84]));
}

#[test]
fn aifs_below_two_becomes_two_and_bad_cw_means_defaults() {
    let good = [ac(1, 4, 10, 0), ac(0, 4, 10, 0), ac(2, 3, 4, 94), ac(2, 2, 3, 47)];
    let v = validate_wmm(&good).unwrap();
    assert_eq!(v[0].aifsn, 2);
    assert_eq!(v[1].aifsn, 2);
    assert_eq!(v[2..], good[2..]);
    // cw_min 0
    let mut bad = good;
    bad[2].ecw_min = 0;
    assert_eq!(validate_wmm(&bad), None);
    // cw_min above cw_max
    let mut bad = good;
    bad[3] = ac(2, 5, 4, 0);
    assert_eq!(validate_wmm(&bad), None);
    // a missing AC is parsed as zeros, which is cw_min 0
    let mut bad = good;
    bad[1] = ac(0, 0, 0, 0);
    assert_eq!(validate_wmm(&bad), None);
}

/// A WMM parameter element with set count `count`; `vi_ecw` replaces VI's ECW byte.
fn wmm_ie(count: u8, vi_ecw: u8) -> Vec<u8> {
    let mut ie = wmm_param_ie();
    ie[8] = (ie[8] & 0xf0) | count;
    // records start at byte 10: ACI 2 is the third, its ECW byte the second of the record
    ie[10 + 2 * 4 + 1] = vi_ecw;
    ie
}

fn wmm_beacon(subtype_bits: u8, count: u8, vi_ecw: u8) -> Vec<u8> {
    beacon(
        subtype_bits,
        &AP,
        SSID,
        6,
        0x0411,
        &[RSN_PSK_CCMP.to_vec(), wmm_ie(count, vi_ecw)],
    )
}

fn set_wmm(out: &[Output]) -> Vec<[AcParams; 4]> {
    out.iter()
        .filter_map(|o| match o {
            Output::SetWmm(p) => Some(*p),
            _ => None,
        })
        .collect()
}

/// Auth and assoc against an AP whose beacon carries `beacon`; returns the station and its association.
fn associate(beacon: &[u8]) -> (Station, PeerInfo, Vec<Output>) {
    let ap = ModelAp::new(PASSPHRASE);
    let mut sta = Station::new(hw_basic(), STA);
    sta.on_mgmt_rx(beacon, -50, FREQ, 0);
    sta.connect(
        SSID,
        Credentials::passphrase(PASSPHRASE).unwrap(),
        [0x5a; 32],
        0,
    )
    .unwrap();
    drain(&mut sta);
    sta.on_mgmt_rx(&ap.auth_resp(0), -50, FREQ, 1);
    // beacons before the association: nothing to follow yet
    sta.on_mgmt_rx(&wmm_beacon(0x80, 9, 0x43), -50, FREQ, 1);
    let early = drain(&mut sta);
    sta.on_mgmt_rx(&ap.assoc_resp(0, 5), -50, FREQ, 2);
    let out = drain(&mut sta);
    let info = out
        .iter()
        .find_map(|o| match o {
            Output::PeerAssoc(i) => Some(i.clone()),
            _ => None,
        })
        .unwrap_or_else(|| panic!("no association: {out:?}"));
    (sta, info, early)
}

#[test]
fn beacons_with_a_new_count_send_the_new_set() {
    let (mut sta, info, early) = associate(&wmm_beacon(0x80, 1, 0x43));
    assert!(set_wmm(&early).is_empty(), "{early:?}");
    let params = info.wmm_params.expect("valid set");
    assert_eq!(params[2], ac(2, 3, 4, 94));
    // same count: nothing
    sta.on_mgmt_rx(&wmm_beacon(0x80, 1, 0x54), -50, FREQ, 3);
    assert!(set_wmm(&drain(&mut sta)).is_empty());
    // a probe response does not count, only beacons
    sta.on_mgmt_rx(&wmm_beacon(0x50, 2, 0x54), -50, FREQ, 4);
    assert!(set_wmm(&drain(&mut sta)).is_empty());
    // new count: the new set, once
    sta.on_mgmt_rx(&wmm_beacon(0x80, 2, 0x54), -50, FREQ, 5);
    let sets = set_wmm(&drain(&mut sta));
    assert_eq!(sets.len(), 1);
    assert_eq!(sets[0][2], ac(2, 4, 5, 94));
    sta.on_mgmt_rx(&wmm_beacon(0x80, 2, 0x54), -50, FREQ, 6);
    assert!(set_wmm(&drain(&mut sta)).is_empty());
    // a new count with an invalid set: the count moves on, nothing is sent, and the next good set goes out
    sta.on_mgmt_rx(&wmm_beacon(0x80, 3, 0x40), -50, FREQ, 7);
    assert!(set_wmm(&drain(&mut sta)).is_empty());
    sta.on_mgmt_rx(&wmm_beacon(0x80, 3, 0x43), -50, FREQ, 8);
    assert!(set_wmm(&drain(&mut sta)).is_empty(), "same count as the invalid one");
    sta.on_mgmt_rx(&wmm_beacon(0x80, 4, 0x43), -50, FREQ, 9);
    assert_eq!(set_wmm(&drain(&mut sta)).len(), 1);
}

#[test]
fn an_invalid_set_at_association_means_defaults_and_no_tracking() {
    // VI with cw_min 0: mac80211 uses its defaults and sets disable_wmm_tracking
    let (mut sta, info, _) = associate(&wmm_beacon(0x80, 1, 0x40));
    assert_eq!(info.wmm_params, None);
    assert!(info.qos);
    sta.on_mgmt_rx(&wmm_beacon(0x80, 2, 0x43), -50, FREQ, 3);
    assert!(set_wmm(&drain(&mut sta)).is_empty());
}
