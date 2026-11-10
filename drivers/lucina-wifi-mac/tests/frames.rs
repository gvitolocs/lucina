//! Frame conversions, element parsing, the regulatory table and the scan table, each against the Linux or standard
//! behaviour it comes from.

mod common;

use common::{AP, RSN_PSK_CCMP, STA, beacon, ht_cap_ie, ht_oper_ie, hw_basic, wmm_param_ie};
use lucina_wifi_mac::data::{
    ethernet_to_nwifi, nwifi_to_ethernet, raw80211_to_ethernet, split_amsdu,
};
use lucina_wifi_mac::frame::{BROADCAST, hdrlen};
use lucina_wifi_mac::ie::{RsnInfo, elements, find_with_header, parse_wmm};
use lucina_wifi_mac::regulatory::{Country, WORLD_CHANNELS, rule_for_freq};
use lucina_wifi_mac::scan::{SCAN_TABLE_MAX, build_scan_request, probe_ies};
use lucina_wifi_mac::*;

const REMOTE: [u8; 6] = [0x02, 0xee, 0xee, 0xee, 0xee, 0xee];

fn eth(proto: u16, payload: &[u8]) -> Vec<u8> {
    let mut e = REMOTE.to_vec();
    e.extend_from_slice(&STA);
    e.extend_from_slice(&proto.to_be_bytes());
    e.extend_from_slice(payload);
    e
}

#[test]
fn ethernet_to_nwifi_layout() {
    let f = ethernet_to_nwifi(&eth(0x0800, b"abc"), &AP, &STA, false).unwrap();
    assert_eq!(&f[..2], &[0x08, 0x01], "data, no QoS, To-DS");
    assert_eq!(&f[2..4], &[0, 0]);
    assert_eq!(&f[4..10], &AP, "address 1: BSSID");
    assert_eq!(&f[10..16], &STA, "address 2: SA");
    assert_eq!(&f[16..22], &REMOTE, "address 3: DA");
    assert_eq!(&f[24..30], &[0xaa, 0xaa, 0x03, 0, 0, 0]);
    assert_eq!(&f[30..32], &[0x08, 0x00]);
    assert_eq!(&f[32..], b"abc");
    assert_eq!(
        ethernet_to_nwifi(&eth(0x0800, b"abc"), &AP, &STA, true).unwrap()[1],
        0x41
    );
}

#[test]
fn bridge_tunnel_for_aarp_and_ipx_and_no_snap_for_8023() {
    for proto in [0x80f3u16, 0x8137] {
        let f = ethernet_to_nwifi(&eth(proto, b"x"), &AP, &STA, false).unwrap();
        assert_eq!(&f[24..30], &[0xaa, 0xaa, 0x03, 0, 0, 0xf8], "{proto:#x}");
    }
    // A length in the type field: the payload has its own LLC, and nothing is added.
    let f = ethernet_to_nwifi(&eth(3, &[0x42, 0x42, 3]), &AP, &STA, false).unwrap();
    assert_eq!(&f[24..], &[0x42, 0x42, 3]);
    assert_eq!(ethernet_to_nwifi(&[0; 13], &AP, &STA, false), None);
}

/// A From-DS frame with the given body.
fn from_ds(fc0: u8, qos: Option<u8>, body: &[u8]) -> Vec<u8> {
    let mut f = vec![fc0, 0x02, 0, 0];
    f.extend_from_slice(&STA);
    f.extend_from_slice(&AP);
    f.extend_from_slice(&REMOTE);
    f.extend_from_slice(&[0, 0]);
    if let Some(q) = qos {
        f.extend_from_slice(&[q, 0]);
    }
    f.extend_from_slice(body);
    f
}

#[test]
fn nwifi_to_ethernet_undoes_it() {
    let mut body = vec![0xaa, 0xaa, 3, 0, 0, 0, 0x08, 0x00];
    body.extend_from_slice(b"payload");
    let e = nwifi_to_ethernet(&from_ds(0x08, None, &body)).unwrap();
    assert_eq!(&e[..6], &STA, "DA is address 1 in From-DS");
    assert_eq!(&e[6..12], &REMOTE, "SA is address 3 in From-DS");
    assert_eq!(&e[12..14], &[8, 0]);
    assert_eq!(&e[14..], b"payload");

    // Bridge-tunnel and RFC 1042's AARP/IPX exceptions.
    let e = nwifi_to_ethernet(&from_ds(
        0x08,
        None,
        &[0xaa, 0xaa, 3, 0, 0, 0xf8, 0x81, 0x37, 7],
    ))
    .unwrap();
    assert_eq!(&e[12..], &[0x81, 0x37, 7]);
    let plain_llc = [0xaa, 0xaa, 3, 0, 0, 0, 0x81, 0x37, 7];
    let e = nwifi_to_ethernet(&from_ds(0x08, None, &plain_llc)).unwrap();
    assert_eq!(
        &e[12..14],
        &(plain_llc.len() as u16).to_be_bytes(),
        "kept as 802.3: IPX under RFC 1042 is not translated"
    );
    assert_eq!(&e[14..], &plain_llc);
    // No SNAP at all: an 802.3 frame with its length.
    let e = nwifi_to_ethernet(&from_ds(0x08, None, &[1, 2, 3, 4])).unwrap();
    assert_eq!(&e[12..], &[0, 4, 1, 2, 3, 4]);
    // The empty payload and a too-short frame.
    assert!(nwifi_to_ethernet(&from_ds(0x08, None, &[])).is_some());
    assert!(nwifi_to_ethernet(&[8, 2, 0]).is_none());
}

#[test]
fn round_trip_through_a_mirror() {
    // What goes out To-DS comes back the same when an AP turns it around.
    let original = eth(0x86dd, &[9; 100]);
    let out = ethernet_to_nwifi(&original, &AP, &STA, false).unwrap();
    let mut back = from_ds(0x08, None, &out[24..]);
    back[4..10].copy_from_slice(&REMOTE);
    back[16..22].copy_from_slice(&STA);
    assert_eq!(nwifi_to_ethernet(&back).unwrap(), original);
}

#[test]
fn nulls_and_other_types_are_not_data() {
    assert!(
        nwifi_to_ethernet(&from_ds(0x48, None, &[])).is_none(),
        "null function"
    );
    assert!(
        nwifi_to_ethernet(&from_ds(0xc8, Some(0), &[])).is_none(),
        "QoS null"
    );
    assert!(
        nwifi_to_ethernet(&from_ds(0x80, None, &[0; 8])).is_none(),
        "beacon"
    );
    assert!(
        nwifi_to_ethernet(&from_ds(0xd4, None, &[])).is_none(),
        "ack"
    );
}

#[test]
fn raw_qos_frames() {
    let mut body = vec![0xaa, 0xaa, 3, 0, 0, 0, 0x08, 0x00];
    body.extend_from_slice(b"qos!");
    let f = from_ds(0x88, Some(5), &body);
    assert_eq!(hdrlen(0x0288), Some(26));
    let frames = raw80211_to_ethernet(&f, 0);
    assert_eq!(frames.len(), 1);
    assert_eq!(&frames[0][14..], b"qos!");
    assert_eq!(&frames[0][..6], &STA);

    // With a CCMP header still in front.
    let mut with_iv = vec![0; 8];
    with_iv.extend_from_slice(&body);
    let f = from_ds(0x88, Some(5), &with_iv);
    assert_eq!(raw80211_to_ethernet(&f, 8), frames);
    assert!(
        raw80211_to_ethernet(&f[..30], 8).is_empty(),
        "shorter than header and IV"
    );

    // A four-address frame's SA is address 4, and QoS Control comes after it.
    let mut four = vec![0x88, 0x03, 0, 0];
    four.extend_from_slice(&STA);
    four.extend_from_slice(&AP);
    four.extend_from_slice(&REMOTE);
    four.extend_from_slice(&[0, 0]);
    four.extend_from_slice(&[0x11; 6]);
    four.extend_from_slice(&[0, 0]);
    four.extend_from_slice(&body);
    let frames = raw80211_to_ethernet(&four, 0);
    assert_eq!(&frames[0][..6], &REMOTE);
    assert_eq!(&frames[0][6..12], &[0x11; 6]);
    assert_eq!(&frames[0][14..], b"qos!");
}

/// One A-MSDU subframe with its padding.
fn subframe(da: &[u8; 6], sa: &[u8; 6], msdu: &[u8], pad: bool) -> Vec<u8> {
    let mut s = da.to_vec();
    s.extend_from_slice(sa);
    s.extend_from_slice(&(msdu.len() as u16).to_be_bytes());
    s.extend_from_slice(msdu);
    if pad {
        while !s.len().is_multiple_of(4) {
            s.push(0);
        }
    }
    s
}

fn ip_msdu(data: &[u8]) -> Vec<u8> {
    let mut m = vec![0xaa, 0xaa, 3, 0, 0, 0, 0x08, 0x00];
    m.extend_from_slice(data);
    m
}

#[test]
fn amsdu_splits_and_pads() {
    let a = ip_msdu(b"one");
    let b = ip_msdu(b"second!");
    let c = ip_msdu(b"3");
    let mut p = subframe(&STA, &REMOTE, &a, true);
    p.extend(subframe(&STA, &REMOTE, &b, true));
    p.extend(subframe(&STA, &REMOTE, &c, false));
    let out = split_amsdu(&p, Some(&STA), None);
    assert_eq!(out.len(), 3);
    assert_eq!(&out[0][14..], b"one");
    assert_eq!(&out[1][14..], b"second!");
    assert_eq!(&out[2][14..], b"3");
    assert_eq!(&out[1][..6], &STA);
    assert_eq!(&out[1][6..12], &REMOTE);
    assert_eq!(&out[1][12..14], &[8, 0]);

    // Through a raw frame with the A-MSDU bit, and padded last subframe tolerated.
    let mut q = subframe(&STA, &REMOTE, &a, true);
    q.extend(subframe(&STA, &REMOTE, &b, true));
    let f = from_ds(0x88, Some(0x80), &q);
    let frames = raw80211_to_ethernet(&f, 0);
    assert_eq!(frames.len(), 2);
}

#[test]
fn amsdu_filters_by_address_and_refuses_damage() {
    let m = ip_msdu(b"data");
    let other = [0x02, 1, 2, 3, 4, 5];
    let mut p = subframe(&other, &REMOTE, &m, true);
    p.extend(subframe(&STA, &REMOTE, &m, true));
    p.extend(subframe(&BROADCAST, &REMOTE, &m, false));
    let out = split_amsdu(&p, Some(&STA), None);
    assert_eq!(
        out.len(),
        2,
        "unicast to someone else is skipped, multicast is not"
    );
    assert_eq!(
        split_amsdu(&p, Some(&STA), Some(&AP)).len(),
        0,
        "wrong SA on all"
    );

    // A subframe running past the end throws away the whole A-MSDU, including what parsed before it.
    let mut bad = subframe(&STA, &REMOTE, &m, true);
    let mut tail = subframe(&STA, &REMOTE, &m, false);
    tail[13] = 200;
    bad.extend(tail);
    assert!(split_amsdu(&bad, Some(&STA), None).is_empty());
    assert!(split_amsdu(&[1, 2, 3], Some(&STA), None).is_empty());
    assert!(split_amsdu(&[], Some(&STA), None).is_empty());
}

/// The FragAttacks A-MSDU injection: an attacker flips the A-MSDU bit on a normal frame, so its LLC reads as a
/// subframe DA and what the attacker controls further on reads as real subframes. The first one would be skipped for
/// its address anyway: the check matters for the ones after it.
#[test]
fn amsdu_aggregation_attack_is_refused() {
    let mut body = vec![0xaa, 0xaa, 3, 0, 0, 0];
    body.extend_from_slice(&[0x08, 0x00, 0x45, 0, 0, 0]);
    body.extend_from_slice(&[0, 2]);
    body.extend_from_slice(&[0, 0]);
    body.extend(subframe(&STA, &REMOTE, &ip_msdu(b"injected"), false));
    assert!(split_amsdu(&body, Some(&STA), None).is_empty());
    assert!(raw80211_to_ethernet(&from_ds(0x88, Some(0x80), &body), 0).is_empty());
    // Without that LLC at the front, the same bytes are two subframes and the second would come through.
    body[0] = 0x02;
    assert_eq!(split_amsdu(&body, Some(&STA), None).len(), 1);
}

#[test]
fn rsn_parsing() {
    let r = RsnInfo::parse(&RSN_PSK_CCMP[2..]).unwrap();
    assert_eq!(r.group, 0x000f_ac04);
    assert_eq!(r.pairwise, [0x000f_ac04]);
    assert_eq!(r.akm, [0x000f_ac02]);
    assert_eq!(r.capabilities, 0);
    assert!(!r.mfp_required());

    // Defaults for what is left off: 9.4.2.24.1, CCMP and 802.1X.
    let r = RsnInfo::parse(&[1, 0]).unwrap();
    assert_eq!(
        (r.group, r.pairwise.clone(), r.akm.clone()),
        (0x000f_ac04, vec![0x000f_ac04], vec![0x000f_ac01])
    );
    let r = RsnInfo::parse(&[1, 0, 0, 0x0f, 0xac, 2]).unwrap();
    assert_eq!(r.group, 0x000f_ac02);

    // MFPR and MFPC.
    let mut caps = RSN_PSK_CCMP[2..].to_vec();
    caps[18] = 0xc0;
    let r = RsnInfo::parse(&caps).unwrap();
    assert!(r.mfp_capable() && r.mfp_required());

    // Wrong version, partial suites, counts that run off the end.
    assert!(RsnInfo::parse(&[2, 0]).is_none());
    assert!(RsnInfo::parse(&[1]).is_none());
    assert!(RsnInfo::parse(&[1, 0, 0, 0x0f]).is_none());
    assert!(RsnInfo::parse(&[1, 0, 0, 0x0f, 0xac, 4, 9, 0, 0, 0x0f, 0xac, 4]).is_none());
    assert!(RsnInfo::parse(&[1, 0, 0, 0x0f, 0xac, 4, 0, 0]).is_none());
    assert!(RsnInfo::parse(&[1, 0, 0, 0x0f, 0xac, 4, 1]).is_none());
}

#[test]
fn a_corrupt_rsn_element_is_not_taken_for_open() {
    let bad = vec![48, 2, 9, 9];
    let f = beacon(0x80, &AP, b"x", 6, 0x0411, &[bad]);
    let b = ScanResult::parse(&f, -50, 2437, 0).unwrap();
    assert_eq!(b.security, Security::Other);
    assert!(b.rsn.is_none());
    assert_eq!(b.rsn_ie.as_deref(), Some([48, 2, 9, 9].as_slice()));
    // Even with the privacy bit lying.
    let f = beacon(0x80, &AP, b"x", 6, 0x0401, &[vec![48, 2, 9, 9]]);
    assert_eq!(
        ScanResult::parse(&f, -50, 2437, 0).unwrap().security,
        Security::Other
    );
}

#[test]
fn element_walk_keeps_what_is_valid() {
    let ies = [0, 2, b'a', b'b', 3, 1, 6, 48, 200, 1];
    let got: Vec<(u8, &[u8])> = elements(&ies).collect();
    assert_eq!(got, [(0u8, b"ab".as_slice()), (3, [6u8].as_slice())]);
    assert_eq!(find_with_header(&ies, 3), Some([3u8, 1, 6].as_slice()));
    assert_eq!(find_with_header(&ies, 48), None);
    assert_eq!(elements(&[]).count(), 0);
    assert_eq!(elements(&[1]).count(), 0);
}

#[test]
fn wmm_is_recognised_by_oui_type_and_version() {
    let w = parse_wmm(&wmm_param_ie()).unwrap();
    assert_eq!(w.qos_info, 0x80);
    let p = w.params.unwrap();
    assert_eq!(p[2].txop_limit, 94);
    assert_eq!((p[1].aifsn, p[1].acm), (7, false));
    let info = [221, 7, 0x00, 0x50, 0xf2, 2, 0, 1, 0x0f];
    assert_eq!(parse_wmm(&info).unwrap().params, None);
    assert_eq!(
        parse_wmm(&[221, 7, 0x00, 0x50, 0xf2, 1, 0, 1, 0]),
        None,
        "WPA1, not WMM"
    );
    assert_eq!(
        parse_wmm(&[221, 7, 0x00, 0x50, 0xf2, 2, 0, 2, 0]),
        None,
        "version 2"
    );
    assert_eq!(
        parse_wmm(&[221, 10, 0x00, 0x50, 0xf2, 2, 1, 1, 0, 0, 0, 0]),
        None,
        "parameter element too short"
    );
}

#[test]
fn world_channels_match_the_regdom_and_the_driver_table() {
    let reg = Regulatory::new();
    let list = reg.channels();
    assert_eq!(list.len(), 38);
    let active: Vec<u8> = list
        .iter()
        .filter(|c| !c.passive)
        .map(|c| c.channel)
        .collect();
    assert_eq!(
        active,
        (1..=11).collect::<Vec<u8>>(),
        "only 1-11 are free of NO_IR in the world domain"
    );
    let dfs: Vec<u8> = list.iter().filter(|c| c.dfs).map(|c| c.channel).collect();
    assert_eq!(
        dfs,
        [
            52, 56, 60, 64, 100, 104, 108, 112, 116, 120, 124, 128, 132, 136, 140, 144
        ]
    );
    // Excluded on purpose.
    for freq in [2484, 5845, 5865, 5150, 4920, 0] {
        assert!(reg.channel(freq).is_none(), "{freq}");
    }
    for c in &WORLD_CHANNELS {
        let expect = if c.band == Band::Ghz2 {
            2407 + 5 * c.channel as u16
        } else {
            5000 + 5 * c.channel as u16
        };
        assert_eq!(c.freq_mhz, expect, "channel {}", c.channel);
        assert_eq!(c.max_power_dbm, 20);
    }
    assert_eq!(rule_for_freq(2472).unwrap().channel, 13);
}

#[test]
fn beacon_hints_follow_cfg80211() {
    let mut reg = Regulatory::new();
    let passive = |r: &Regulatory, f| r.channel(f).unwrap().passive;
    // 12 and 13 and 5 GHz non-DFS lift; radar channels do not; 1-11 have nothing to lift.
    for f in [2467, 5180, 5745] {
        assert!(passive(&reg, f));
        reg.on_beacon(f);
        assert!(!passive(&reg, f), "{f}");
    }
    for f in [5260, 5500, 5720] {
        reg.on_beacon(f);
        assert!(passive(&reg, f), "DFS {f} stays passive");
    }
    reg.on_beacon(2437);
    assert!(!passive(&reg, 2437));
    reg.on_beacon(9999);
}

#[test]
fn beacon_hint_through_the_station_opens_probing_there() {
    let mut sta = Station::new(hw_basic(), STA);
    let before = sta.start_scan(None, 0);
    let on = |r: &ScanRequest, f: u16| {
        r.legs
            .iter()
            .flat_map(|l| l.channels.iter())
            .find(|c| c.freq_mhz == f)
            .unwrap()
            .passive
    };
    assert!(on(&before, 5180));
    sta.on_mgmt_rx(&beacon(0x80, &AP, b"x", 36, 0x0001, &[]), -50, 5180, 0);
    sta.on_mgmt_rx(
        &beacon(0x80, &[2, 0, 0, 0, 0, 9], b"y", 100, 0x0001, &[]),
        -50,
        5500,
        0,
    );
    let after = sta.start_scan(None, 0);
    assert!(!on(&after, 5180));
    assert!(on(&after, 5500), "radar channel");
    // A probe response is not a beacon hint.
    let mut sta = Station::new(hw_basic(), STA);
    sta.on_mgmt_rx(&beacon(0x50, &AP, b"x", 36, 0x0001, &[]), -50, 5180, 0);
    assert!(on(&sta.start_scan(None, 0), 5180));
}

#[test]
fn country_only_restricts() {
    let mut reg = Regulatory::new();
    // DK-style: 1-13, 36-48 (4 channels spaced by 4), an operating-class triplet to skip.
    let c = Country::parse(&[b'D', b'K', b' ', 1, 13, 17, 36, 4, 23, 201, 1, 2]).unwrap();
    assert_eq!(c.alpha2, *b"DK");
    reg.set_country(Some(c));
    assert_eq!(reg.channel(2412).unwrap().max_power_dbm, 17);
    assert_eq!(reg.channel(2472).unwrap().max_power_dbm, 17);
    assert!(
        reg.channel(2472).unwrap().passive,
        "listed by the AP: still passive without a beacon there"
    );
    assert_eq!(
        reg.channel(5240).unwrap().max_power_dbm,
        20,
        "23 is above the world limit"
    );
    assert!(reg.channel(5260).is_none());
    assert!(reg.channel(5745).is_none());
    // 2.4 GHz numbers in a triplet do not leak into 5 GHz.
    let c = Country::parse(&[b'X', b'X', b' ', 36, 1, 30]).unwrap();
    reg.set_country(Some(c));
    assert!(reg.channel(2412).is_none());
    assert!(reg.channel(5180).is_some());
    assert!(reg.channel(5200).is_none());
    // Odd channels of a 5 GHz band are not "in" it.
    let c = Country::parse(&[b'X', b'X', b' ', 149, 5, 30]).unwrap();
    assert_eq!(c.alpha2, *b"XX");
    reg.set_country(Some(c));
    assert!(reg.channel(5825).is_some(), "165 = 149 + 4 * 4");
    assert!(reg.channel(5745).is_some());
    reg.clear_country();
    assert!(reg.channel(2412).is_some());

    assert!(Country::parse(b"DK").is_none());
    assert!(
        Country::parse(b"DK ").is_none(),
        "no triplets: no restriction at all"
    );
    assert!(Country::parse(&[b'D', b'K', b' ', 0, 0, 0, 201, 1, 2]).is_none());

    // Through the IE walk.
    let mut reg = Regulatory::new();
    reg.apply_country_ie(&[7, 6, b'J', b'P', b' ', 1, 11, 20]);
    assert!(reg.channel(2467).is_none());
    assert!(reg.channel(2462).is_some());
    reg.apply_country_ie(&[]);
    assert!(reg.channel(2467).is_some());
}

#[test]
fn scan_request_follows_ath10k_and_mac80211() {
    let mut hw = hw_basic();
    hw.ht = HwCaps::ht_from_wmi(0x0001 | 0x0002, 0, 2, 3, 3);
    let reg = Regulatory::new();
    let r = build_scan_request(Some(b"net"), &hw, &reg);
    assert_eq!(r.ssids, [b"net".to_vec()]);
    assert_eq!(r.legs.len(), 2);
    assert_eq!(r.legs[0].band, Band::Ghz2);
    assert_eq!(r.legs[0].channels.len(), 13);
    assert_eq!(r.legs[1].channels.len(), 25);
    assert!(!r.legs[0].channels[0].passive);
    assert!(r.legs[0].channels[12].passive);
    assert_eq!(
        (
            r.dwell_active_ms,
            r.dwell_passive_ms,
            r.min_rest_ms,
            r.max_rest_ms,
            r.probe_delay_ms,
            r.max_scan_ms
        ),
        (50, 150, 50, 500, 5, 20_000),
        "ath10k_wmi_start_scan_init"
    );
    // Wildcard.
    assert_eq!(
        build_scan_request(None, &hw, &reg).ssids,
        [Vec::<u8>::new()]
    );

    // Probe IEs: rates split at 8 on 2.4 GHz; 5 GHz has only OFDM; HT caps at the end; no DS element.
    let ids: Vec<u8> = elements(&r.legs[0].ies).map(|(i, _)| i).collect();
    assert_eq!(ids, [1, 50, 45]);
    assert_eq!(
        elements(&r.legs[0].ies).next().unwrap().1,
        [2, 4, 11, 22, 12, 18, 24, 36]
    );
    let ids: Vec<u8> = elements(&r.legs[1].ies).map(|(i, _)| i).collect();
    assert_eq!(ids, [1, 45]);
    let mut no_ht = hw;
    no_ht.ht = None;
    let ids: Vec<u8> = elements(&probe_ies(Band::Ghz5, &no_ht))
        .map(|(i, _)| i)
        .collect();
    assert_eq!(ids, [1]);

    // A band the hardware lacks is not scanned.
    let mut one = hw;
    one.band_5ghz = false;
    let r = build_scan_request(None, &one, &reg);
    assert_eq!(r.legs.len(), 1);
}

#[test]
fn scan_result_fields() {
    let f = beacon(
        0x80,
        &AP,
        b"lab",
        44,
        0x0111,
        &[
            RSN_PSK_CCMP.to_vec(),
            ht_cap_ie(),
            ht_oper_ie(44, 1, true),
            vec![191, 12, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12],
            vec![192, 5, 1, 42, 0, 0, 0],
            vec![7, 6, b'D', b'K', b' ', 36, 4, 23],
            wmm_param_ie(),
        ],
    );
    let r = ScanResult::parse(&f, -61, 5220, 77).unwrap();
    assert_eq!(r.bssid, AP);
    assert_eq!(r.ssid, b"lab");
    assert_eq!((r.freq_mhz, r.channel, r.band), (5220, 44, Band::Ghz5));
    assert_eq!(r.rssi_dbm, -61);
    assert_eq!(r.security, Security::Wpa2Psk);
    assert_eq!(r.beacon_interval_tu, 100);
    assert_eq!(r.capability, 0x0111);
    assert_eq!(
        r.supported_rates,
        [0x8c, 0x12, 0x98, 0x24, 0xb0, 0x48, 0x60, 0x6c]
    );
    assert_eq!(r.ht_cap.as_ref().unwrap().len(), 26);
    assert_eq!(r.ht_oper.as_ref().unwrap().len(), 22);
    assert_eq!(r.vht_cap.as_ref().unwrap().len(), 12);
    assert_eq!(r.vht_oper.as_ref().unwrap().len(), 5);
    assert_eq!(r.country.as_ref().unwrap()[..2], *b"DK");
    assert!(r.wmm.is_some());
    assert_eq!(r.dtim_period, 2);
    assert_eq!(r.last_seen_ms, 77);
    assert!(r.is_ess());
}

#[test]
fn channel_comes_from_the_frame_like_cfg80211() {
    // Heard on 2412 while off-channel: the DS element says 6.
    let f = beacon(0x80, &AP, b"x", 6, 0x0001, &[]);
    let r = ScanResult::parse(&f, -50, 2412, 0).unwrap();
    assert_eq!((r.channel, r.freq_mhz), (6, 2437));
    // A 2.4 GHz frame claiming a channel we do not use is dropped.
    assert!(
        ScanResult::parse(
            &beacon(0x80, &AP, b"x", 13, 0x0001, &[vec![3, 1, 14]]),
            -50,
            2412,
            0
        )
        .is_some()
    );
    let mut bad = beacon(0x80, &AP, b"x", 6, 0x0001, &[]);
    // Find the DS element and overwrite its channel.
    let pos = bad.windows(3).position(|w| w == [3, 1, 6]).unwrap();
    bad[pos + 2] = 14;
    assert!(ScanResult::parse(&bad, -50, 2412, 0).is_none());
    // A frequency we do not use at all.
    assert!(ScanResult::parse(&beacon(0x80, &AP, b"x", 6, 0x0001, &[]), -50, 2484, 0).is_none());
    // No SSID element: dropped, as a BSS without an SSID element is not valid.
    let mut no_ssid = beacon(0x80, &AP, b"", 6, 0x0001, &[]);
    no_ssid.drain(36..38);
    assert!(
        ScanResult::parse(&no_ssid, -50, 2437, 0).is_none()
            || ScanResult::parse(&no_ssid, -50, 2437, 0)
                .unwrap()
                .ssid
                .is_empty()
    );
    // Oversize SSID.
    let long = beacon(0x80, &AP, &[b'a'; 33], 6, 0x0001, &[]);
    assert!(ScanResult::parse(&long, -50, 2437, 0).is_none());
    // Not a beacon.
    let mut assoc = beacon(0x80, &AP, b"x", 6, 0x0001, &[]);
    assoc[0] = 0x10;
    assert!(ScanResult::parse(&assoc, -50, 2437, 0).is_none());
}

#[test]
fn table_dedupes_keeps_hidden_names_and_stays_bounded() {
    let mut t = ScanTable::new();
    let parse = |ssid: &[u8], rssi, now, sub| {
        ScanResult::parse(&beacon(sub, &AP, ssid, 6, 1, &[]), rssi, 2437, now).unwrap()
    };
    t.update(parse(b"named", -70, 0, 0x50));
    assert_eq!(t.entries().len(), 1);
    // The next beacon hides the SSID: the probe response's name stays, the rest updates.
    t.update(parse(b"", -60, 5, 0x80));
    assert_eq!(t.entries().len(), 1);
    assert_eq!(t.entries()[0].ssid, b"named");
    assert_eq!(t.entries()[0].rssi_dbm, -60);
    t.update(parse(&[0, 0, 0], -55, 6, 0x80));
    assert_eq!(t.entries()[0].ssid, b"named", "zeros hide it too");
    // A different channel is a different entry.
    t.update(ScanResult::parse(&beacon(0x80, &AP, b"x", 11, 1, &[]), -60, 2462, 7).unwrap());
    assert_eq!(t.entries().len(), 2);
    t.expire(30_006);
    assert_eq!(
        t.entries().len(),
        1,
        "the entry last heard at 6 ms has just reached 30 s"
    );
    assert_eq!(t.entries()[0].last_seen_ms, 7);
    t.clear();

    // A flood of fake BSSIDs cannot grow it, and evicts the oldest.
    for i in 0..(SCAN_TABLE_MAX as u32 + 50) {
        let bssid = [2, 0, 0, 0, (i >> 8) as u8, i as u8];
        t.update(
            ScanResult::parse(&beacon(0x80, &bssid, b"f", 6, 1, &[]), -50, 2437, i as u64).unwrap(),
        );
    }
    assert_eq!(t.entries().len(), SCAN_TABLE_MAX);
    assert!(t.iter().all(|e| e.last_seen_ms >= 50));
}
