//! The driver through its rings against `model/mod.rs`, a firmware written from reading Linux's host side: it proves the
//! driver and that reading agree, not that the firmware does. Sequences expected here are `ath10k_core_start`,
//! `ath10k_start`, `ath10k_add_interface`, `ath10k_bss_assoc` and mac80211's teardown, read off the C.
//! Derived from Linux ath10k (SPDX ISC), see oracle/README.md.

mod common;
mod model;

use common::{hex, kvl, num};
use lucina_ath10k::ce::CopyEngine;
use lucina_ath10k::driver::*;
use lucina_ath10k::wmi_tlv as wmi;
use model::{Model, ModelCfg, RamPlatform, RxSpec, id, word};

type D = Driver<RamPlatform>;

const BSS: [u8; 6] = [0xde, 0xad, 0xbe, 0xef, 0x00, 0x01];
const ME: [u8; 6] = [0x02, 0xaa, 0xbb, 0xcc, 0xdd, 0xee];

struct Rig {
    d: D,
    m: Model,
    now: u64,
}

impl Rig {
    fn new(cfg: Config, mcfg: ModelCfg) -> Rig {
        let plat = RamPlatform::new(D::arena_bytes(&cfg));
        let mut d = D::new(plat, cfg).unwrap();
        d.start().unwrap();
        Rig {
            d,
            m: Model::new(mcfg),
            now: 1000,
        }
    }
    /// `n` rounds of firmware and driver at 2 ms.
    fn run(&mut self, n: usize) -> Vec<Event> {
        let mut ev = Vec::new();
        for _ in 0..n {
            self.m.step(self.d.platform_mut());
            ev.extend(self.d.poll(self.now));
            self.now += 2;
        }
        ev
    }
    /// Jump the clock.
    fn skip(&mut self, ms: u64) -> Vec<Event> {
        self.now += ms;
        self.d.poll(self.now)
    }
}

fn up_with(mcfg: ModelCfg) -> (Rig, Caps) {
    let mut r = Rig::new(Config::default(), mcfg);
    let ev = r.run(200);
    let caps = ev
        .iter()
        .find_map(|e| match e {
            Event::Ready { mac, caps } => {
                assert_eq!(*mac, ME);
                Some(*caps)
            }
            _ => None,
        })
        .unwrap_or_else(|| panic!("not ready: {ev:?}"));
    (r, caps)
}

fn up() -> Rig {
    up_with(ModelCfg::default()).0
}

fn join_params() -> JoinParams {
    JoinParams {
        bssid: BSS,
        freq: 2437,
        center_freq1: 2437,
        phymode: wmi::phy_mode::MODE_11NG_HT20,
        max_power_dbm: 20,
        beacon_interval: 100,
        dtim_period: 2,
        short_slot: true,
        short_preamble: false,
    }
}

fn joined() -> Rig {
    let mut r = up();
    r.d.join(&join_params()).unwrap();
    let ev = r.run(20);
    assert!(ev.contains(&Event::VdevStarted), "{ev:?}");
    r
}

fn nwifi(payload_len: usize) -> Vec<u8> {
    // data, from DS: addr1 us, addr2 BSSID, addr3 source
    let mut f = vec![0x08, 0x02, 0, 0];
    f.extend_from_slice(&ME);
    f.extend_from_slice(&BSS);
    f.extend_from_slice(&[1, 2, 3, 4, 5, 6]);
    f.extend_from_slice(&[0, 0]);
    f.extend_from_slice(&[0xaa, 0xaa, 3, 0, 0, 0, 8, 0]);
    f.resize(32 + payload_len, 0x5a);
    f
}

fn qos_air_hdr(protected: bool, pn: u64) -> Vec<u8> {
    let fc: u16 = 0x0288 | if protected { 0x4000 } else { 0 };
    let mut h = fc.to_le_bytes().to_vec();
    h.extend_from_slice(&[0, 0]);
    h.extend_from_slice(&ME);
    h.extend_from_slice(&BSS);
    h.extend_from_slice(&[1, 2, 3, 4, 5, 6]);
    h.extend_from_slice(&[0, 0, 0, 0]); // seq, QoS
    if protected {
        let p = pn.to_le_bytes();
        h.extend_from_slice(&[p[0], p[1], 0, 0x20, p[2], p[3], p[4], p[5]]);
    }
    h
}

fn data_rx(ev: &[Event]) -> Vec<&RxFrame> {
    ev.iter()
        .filter_map(|e| match e {
            Event::DataRx(f) => Some(f),
            _ => None,
        })
        .collect()
}

#[test]
fn bring_up_follows_core_start_and_start_and_add_interface() {
    let (r, caps) = up_with(ModelCfg::default());
    let m = &r.m;
    // HTT before WMI, as ath10k_core_start connects them
    assert_eq!(m.connect_order, vec![0x0300, 0x0100]);
    assert!(m.setup_done);
    // the command list with its parameters, in order
    let cmds: Vec<(u32, Vec<u32>)> = m
        .wmi_log
        .iter()
        .map(|c| {
            let v: Vec<u32> = match c.id {
                id::PDEV_SET_PARAM => vec![c.w(1), c.w(2)],
                id::VDEV_SET_PARAM | id::STA_PS_PARAM => vec![c.w(1), c.w(2)],
                _ => vec![],
            };
            (c.id, v)
        })
        .collect();
    let want: Vec<(u32, Vec<u32>)> = vec![
        (id::INIT, vec![]),
        (id::PDEV_SET_PARAM, vec![34, 1]), // pmf_qos
        (id::PDEV_SET_PARAM, vec![10, 1]), // dynamic_bw
        (0x3006, vec![]),                  // prob req oui: the extended service 155
        (id::PDEV_SET_PARAM, vec![48, 0]), // burst: service 37
        (id::PDEV_SET_PARAM, vec![44, 1]), // idle_ps_config
        (id::PDEV_SET_PARAM, vec![1, 3]),  // tx chain mask, two chains
        (id::PDEV_SET_PARAM, vec![2, 3]),  // rx chain mask
        (id::PDEV_SET_PARAM, vec![35, 0]), // arp_ac_override
        (id::PDEV_SET_PARAM, vec![37, 1]), // ani_enable
        (id::SCAN_CHAN_LIST, vec![]),
        (id::REGDOMAIN, vec![]),
        (id::VDEV_CREATE, vec![]),
        (id::STA_KEEPALIVE, vec![]),       // service 29
        (id::VDEV_SET_PARAM, vec![48, 1]), // tx_encap_type native wifi
        (id::VDEV_SET_PARAM, vec![34, 2]), // nss
        (id::STA_PS_PARAM, vec![0, 0]),
        (id::STA_PS_PARAM, vec![1, 1]),
        (id::STA_PS_PARAM, vec![2, 0]),
        (id::VDEV_SET_PARAM, vec![1, u32::MAX]), // rts threshold: wiphy default
        (id::VDEV_SET_PARAM, vec![7, 1]),        // slot time long: ieee80211_reset_erp_info at open
        (id::VDEV_SET_PARAM, vec![8, 1]),        // preamble long
    ];
    assert_eq!(cmds, want);
    // init: ABI, chunks after their NUM_UNITS_IS_*: peers + 1 of the first, 7 of the second
    let init = &m.cmds(id::INIT)[0];
    let t = init.tlv();
    assert_eq!(word(&t[0].1, 6), 2, "two chunks");
    assert_eq!(t[2].0, 18);
    let chunks = model::walk(&t[2].1);
    assert_eq!(word(&chunks[0].1, 0), 0x1000);
    assert_eq!(word(&chunks[0].1, 2), 256 * 34);
    assert_eq!(word(&chunks[1].1, 0), 0x1001);
    assert_eq!(word(&chunks[1].1, 2), 100 * 7);
    // extended service bits reached the resource configuration: ACK RSSI, and high address words
    let rc = &t[1].1;
    assert_eq!(word(rc, 43), (1 << 9) | (1 << 18), "host_capab");
    assert_eq!(word(rc, 2), 0, "no offload peers without full reorder");
    assert_eq!(word(rc, 28), 0x420, "num_msdu_desc stays Linux's");
    assert_eq!(word(rc, 13), 1, "native wifi decap");
    let paddr_hi = word(&chunks[0].1, 3);
    assert_eq!(paddr_hi, 1, "high word kept with SUPPORT_EXTEND_ADDRESS");
    // HTT after the ready event: version, frag bank, rx ring, aggregation
    let order: Vec<u8> = m.htt_log.iter().map(|b| b[0]).collect();
    assert_eq!(order, vec![0, 6, 2, 5]);
    assert_eq!(m.frag_bank.unwrap().1, 1055);
    assert_eq!(m.rx_ring.unwrap().2, 2048);
    assert_eq!(r.d.rx_fill(), 2047, "fill level is size - 1");
    assert_eq!(r.d.htt_version(), (3, 4));
    // regulatory: ETSI1_WORLD from the fixture's table, CTLs per band
    assert_eq!(caps.regdomain, 0x37);
    let rd = &m.cmds(id::REGDOMAIN)[0];
    assert_eq!(rd.w(1), 0x37);
    assert_eq!(rd.w(4), 0x30, "2 GHz CTL");
    assert_eq!(rd.w(5), 0x30, "5 GHz CTL");
    // the channel list went out as the driver reports it
    let cl = &m.cmds(id::SCAN_CHAN_LIST)[0];
    assert_eq!(cl.w(0) as usize, r.d.channels().len());
    assert_eq!(caps.num_rf_chains, 2);
}

#[test]
fn memory_is_what_the_docs_say_and_stop_gives_it_all_back() {
    let cfg = Config::default();
    let mut r = up();
    let (_, _, dma) = r.d.platform_mut().parts();
    let chunks = 256 * 34 + 100 * 7;
    // steady state: the table in the driver's docs, apart from the copies of commands not yet completed
    assert!(
        dma.live_bytes >= cfg.dma_bytes(),
        "{} {}",
        dma.live_bytes,
        cfg.dma_bytes()
    );
    assert!(
        dma.live_bytes <= cfg.dma_bytes() + chunks + 64 * 1024,
        "{} {}",
        dma.live_bytes,
        cfg.dma_bytes()
    );
    assert!(cfg.dma_bytes() < 11 * 1024 * 1024, "{}", cfg.dma_bytes());
    assert!(D::arena_bytes(&cfg) < 128 * 1024, "{}", D::arena_bytes(&cfg));
    r.d.stop();
    let (_, _, dma) = r.d.platform_mut().parts();
    assert_eq!(dma.live_bytes, 0);
    assert!(dma.bufs.is_empty());
    // every engine's ring size register is cleared
    assert_eq!(r.d.poll(0), vec![]);
}

#[test]
fn scan_and_management_rx() {
    let mut r = up();
    assert_eq!(r.d.start_scan(&ScanParams::default()), Ok(()));
    assert_eq!(
        r.d.start_scan(&ScanParams::default()),
        Err(DriverError::Busy)
    );
    r.run(4);
    let c = &r.m.cmds(id::START_SCAN)[0];
    assert_eq!(c.w(0), 0xa000, "scan id with its prefix");
    assert_eq!(c.w(1), 0xa001, "req id with its prefix");
    // no SSID: passive, and the probe request filter bit is flipped by the encoder
    assert_eq!(c.w(14), 0x11 ^ 0x20);
    assert_eq!(c.w(16) as usize, r.d.channels().len());

    // a beacon, on channel 6 and on 36; one with a CRC error; one on a channel that does not exist
    let mut beacon = vec![0x80, 0x00, 0, 0];
    beacon.extend_from_slice(&[0xff; 6]);
    beacon.extend_from_slice(&BSS);
    beacon.extend_from_slice(&BSS);
    beacon.extend_from_slice(&[0, 0, 1, 2, 3]);
    r.m.mgmt_rx(6, 50, 0, &beacon);
    r.m.mgmt_rx(36, 40, 0, &beacon);
    r.m.mgmt_rx(14, 40, 0, &beacon);
    r.m.mgmt_rx(6, 40, wmi::rx_status::ERR_CRC, &beacon);
    r.m.mgmt_rx(200, 40, 0, &beacon);
    let ev = r.run(4);
    let rx: Vec<(i32, u32)> = ev
        .iter()
        .filter_map(|e| match e {
            Event::MgmtRx { frame, rssi, freq } => {
                assert_eq!(frame, &beacon);
                Some((*rssi, *freq))
            }
            _ => None,
        })
        .collect();
    assert_eq!(rx, vec![(-45, 2437), (-55, 5180), (-55, 2484)]);
    assert_eq!(r.d.stats().mgmt_drop, 2);

    // COMPLETED while running ends it
    r.m.scan_event(2, 0, 0);
    let ev = r.run(4);
    assert!(ev.contains(&Event::ScanDone { aborted: false }), "{ev:?}");
    assert_eq!(r.d.start_scan(&ScanParams::default()), Ok(()));
    r.run(2);
    assert_eq!(r.d.stop_scan(), Ok(()));
    let ev = r.run(6);
    assert!(ev.contains(&Event::ScanDone { aborted: true }), "{ev:?}");
}

#[test]
fn completed_while_starting_is_ignored_as_linux_says() {
    let mut mc = ModelCfg::default();
    mc.silent.no_scan_events = true;
    let (mut r, _) = up_with(mc);
    r.d.start_scan(&ScanParams {
        ssids: vec![b"x".to_vec()],
        ..ScanParams::default()
    })
    .unwrap();
    r.run(2);
    let c = &r.m.cmds(id::START_SCAN)[0];
    assert_eq!(c.w(14), 0x10 ^ 0x20, "with an SSID: active");
    r.m.scan_event(2, 0, 0);
    let ev = r.run(4);
    assert!(!ev.iter().any(|e| matches!(e, Event::ScanDone { .. })));
    // no started event in 1 s: stop, and report once the firmware answers
    let ev = r.skip(1100);
    assert!(!ev.iter().any(|e| matches!(e, Event::ScanDone { .. })));
    let ev = r.run(6);
    assert!(ev.contains(&Event::ScanDone { aborted: true }), "{ev:?}");
    assert_eq!(r.m.cmds(id::STOP_SCAN).len(), 1);
}

#[test]
fn a_whole_station_session() {
    let mut r = up();
    let start = r.m.wmi_log.len();
    r.d.join(&join_params()).unwrap();
    assert_eq!(r.d.join(&join_params()), Err(DriverError::Busy));
    let ev = r.run(20);
    assert!(ev.contains(&Event::VdevStarted), "{ev:?}");
    // vdev start, then the beacon interval (BSS_CHANGED_BEACON_INT), power save off, the tx power limits, the peer
    let ids: Vec<u32> = r.m.wmi_log[start..].iter().map(|c| c.id).collect();
    assert_eq!(
        ids,
        vec![
            id::VDEV_START,
            id::VDEV_SET_PARAM,
            id::STA_PS_MODE,
            id::PDEV_SET_PARAM,
            id::PDEV_SET_PARAM,
            id::PEER_CREATE
        ]
    );
    let vs = r.m.cmds(id::VDEV_START)[0].tlv();
    assert_eq!(word(&vs[0].1, 2), 0, "first join: bcn_intval 0, as arvif has it");
    assert_eq!(word(&vs[0].1, 3), 0, "first join: dtim_period 0");
    assert_eq!(
        (r.m.wmi_log[start + 1].w(1), r.m.wmi_log[start + 1].w(2)),
        (3, 100),
        "beacon interval after the start"
    );
    assert_eq!(word(&vs[1].1, 0), 2437);
    assert_eq!(
        word(&vs[1].1, 3) & 0x3f,
        5,
        "11ng ht20 mode in the low bits"
    );
    assert_eq!(vs[1].1[17], 40, "max power, half dBm");
    // TX power is not touched by any of the station changes (owner's rule, audit D6): min, max, reg power, class,
    // antenna gain, max tx power as they were, and the pdev limits at twice the dBm
    assert_eq!(vs[1].1[16..24], [0, 40, 40, 0, 0, 40, 0, 0]);
    let limits: Vec<(u32, u32)> = r.m.wmi_log[start + 3..start + 5]
        .iter()
        .map(|c| (c.w(1), c.w(2)))
        .collect();
    assert_eq!(limits, vec![(3, 40), (4, 40)], "TXPOWER_LIMIT2G, TXPOWER_LIMIT5G");

    // auth and assoc over WMI by reference
    let mut auth = vec![0xb0, 0x00, 0, 0];
    auth.extend_from_slice(&BSS);
    auth.extend_from_slice(&ME);
    auth.extend_from_slice(&BSS);
    auth.extend_from_slice(&[0, 0, 0, 0, 1, 0, 0, 0, 0, 0]);
    r.d.tx_mgmt(&auth).unwrap();
    let ev = r.run(6);
    assert!(ev.contains(&Event::MgmtTxDone { ok: true }), "{ev:?}");
    assert_eq!(r.m.mgmt_tx[0].1, auth);

    let assoc = PeerAssoc {
        bssid: BSS,
        aid: 3,
        capability: 0x431,
        rates: vec![0x82, 0x84, 0x8b, 0x96, 12, 18, 24, 36],
        ht: Some(HtInfo {
            cap: 0x01ef | (3 << 2),
            ampdu_factor: 3,
            ampdu_density: 5,
            rx_mask: [0xff, 0xff, 0, 0, 0, 0, 0, 0, 0, 0],
        }),
        vht: None,
        bandwidth: Bandwidth::Bw20,
        wme: true,
        rsn: true,
        wpa: false,
        freq: 2437,
    };
    let before_pa = r.m.wmi_log.len();
    r.d.peer_assoc(&assoc).unwrap();
    let ev = r.run(6);
    assert!(ev.contains(&Event::PeerAssocDone), "{ev:?}");
    // BEACON_INFO's DTIM and the short slot (the long preamble is the reset value, so mac80211 reports no change),
    // handled before BSS_CHANGED_ASSOC's peer_assoc
    let head: Vec<(u32, u32, u32)> = r.m.wmi_log[before_pa..before_pa + 2]
        .iter()
        .map(|c| (c.id, c.w(1), c.w(2)))
        .collect();
    assert_eq!(
        head,
        vec![(id::VDEV_SET_PARAM, 13, 2), (id::VDEV_SET_PARAM, 7, 2)]
    );
    assert_eq!(r.m.wmi_log[before_pa + 2].id, id::PEER_ASSOC);
    let pa = r.m.cmds(id::PEER_ASSOC)[0].tlv();
    assert_eq!(&pa[0].1[..6], &BSS);
    let w = |i: usize| word(&pa[0].1, 2 + i);
    assert_eq!(w(0), 0, "vdev");
    assert_eq!(w(1), 1, "new assoc");
    assert_eq!(w(2), 3, "aid");
    assert_eq!(
        w(3),
        1 | 2 | 4 | 0x1000 | 0x10000 | 0x8000,
        "auth qos ptk ht ldpc stbc"
    );
    assert_eq!(w(5), 1, "listen interval is 1 for a station");
    assert_eq!(w(7), (1 << 16) - 1, "max mpdu: factor 3");
    assert_eq!(w(8), 4, "density 5 is 4 us");
    assert_eq!(w(10), 2, "two streams, limited by two chains");
    assert_eq!(w(12), u32::from(wmi::phy_mode::MODE_11NG_HT20));
    assert_eq!(pa[1].1[..8], [0x82, 0x84, 0x8b, 0x96, 12, 18, 24, 36]);
    assert_eq!(pa[2].1.len(), 16, "sixteen MCS");
    let ps = r.m.cmds(id::PEER_SET_PARAM);
    let smps = ps[0].tlv();
    assert_eq!(word(&smps[0].1, 3), 1, "smps state param");
    assert_eq!(word(&smps[0].1, 4), 0, "SM PS disabled is none");

    // txbf follows the AP's caps: no VHT, nothing to be, so 0 is written by this vdev having the capability
    let last_txbf: Vec<u32> =
        r.m.cmds(id::VDEV_SET_PARAM)
            .iter()
            .filter(|c| c.w(1) == 45)
            .map(|c| c.w(2))
            .collect();
    assert_eq!(last_txbf, vec![0]);

    let before_up = r.m.wmi_log.len();
    r.d.up(3, &BSS).unwrap();
    assert_eq!(r.d.up(3, &BSS), Err(DriverError::WrongState));
    r.run(8);
    let ids: Vec<u32> = r.m.wmi_log[before_up..].iter().map(|c| c.id).collect();
    assert_eq!(
        ids,
        vec![id::PDEV_SET_PARAM, id::VDEV_UP, id::PEER_SET_PARAM]
    );
    assert_eq!(r.m.cmds(id::VDEV_UP)[0].w(1), 3);
    // a station vif gets one peer_assoc, from ath10k_bss_assoc (ath10k_station_assoc is for AP, mesh and IBSS)
    assert_eq!(r.m.cmds(id::PEER_ASSOC).len(), 1);

    // EAPOL in the clear, native wifi
    let eapol = nwifi(99);
    let tx_id = r.d.tx_data(&eapol, 16, true).unwrap();
    let ev = r.run(4);
    assert!(
        ev.contains(&Event::TxFree {
            id: tx_id,
            acked: true
        }),
        "{ev:?}"
    );
    let f = &r.m.frames_tx[0];
    assert_eq!(f.frame, eapol);
    assert_eq!(f.id, tx_id);
    assert_eq!(
        f.flags0,
        (1 << 5) | 1 | 4,
        "native wifi, mac header present, no encrypt"
    );
    assert_eq!(f.flags1, (16 << 6) | (1 << 11), "tid 16, vdev 0, postponed");
    assert_eq!(f.prefetch.len(), 52, "50 rounded up");
    assert_eq!(f.eid, r.m.htt_eid);

    // the keys: one at a time, in order, each waits for its security indication
    let ptk = KeySpec {
        pairwise: true,
        key_idx: 0,
        cipher: Cipher::Ccmp,
        key: vec![7; 16],
    };
    let gtk = KeySpec {
        pairwise: false,
        key_idx: 1,
        cipher: Cipher::Ccmp,
        key: vec![9; 16],
    };
    r.d.install_key(&ptk).unwrap();
    r.d.install_key(&gtk).unwrap();
    let ev = r.run(12);
    let keys: Vec<&Event> = ev
        .iter()
        .filter(|e| matches!(e, Event::KeyInstalled { .. }))
        .collect();
    assert_eq!(
        keys,
        vec![
            &Event::KeyInstalled { pairwise: true },
            &Event::KeyInstalled { pairwise: false }
        ]
    );
    let inst = r.m.cmds(id::INSTALL_KEY);
    assert_eq!(inst.len(), 2);
    let t0 = inst[0].tlv();
    assert_eq!(&t0[0].1[4..10], &BSS, "key for the AP's peer");
    assert_eq!(word(&t0[0].1, 4), 0, "pairwise");
    assert_eq!(word(&t0[0].1, 5), 4, "AES CCM");
    assert_eq!(t0[1].1, vec![7; 16]);
    assert_eq!(word(&inst[1].tlv()[0].1, 3), 1, "group key index");
    // pairwise key: the peer is authorized
    let auth_cmds: Vec<(u32, u32)> =
        r.m.cmds(id::PEER_SET_PARAM)
            .iter()
            .map(|c| (word(&c.tlv()[0].1, 3), word(&c.tlv()[0].1, 4)))
            .collect();
    assert!(auth_cmds.contains(&(3, 1)), "{auth_cmds:?}");

    // a decrypted data frame comes up as the device left it
    let body = nwifi(1400);
    let spec = RxSpec {
        payload: body.clone(),
        enctype: 6,
        tid: 3,
        hdr: qos_air_hdr(true, 5),
        rssi_comb: 52,
        ppdu_info1: 8 << 24,
        ppdu_info2: 7 | (1 << 7),
        ppdu_info3: 1 << 7,
        ..RxSpec::default()
    };
    let fill = r.d.rx_fill();
    r.m.fill_rx(r.d.platform_mut(), &spec);
    r.m.rx_ind(&[1], 1);
    let ev = r.run(4);
    let got = data_rx(&ev);
    assert_eq!(got.len(), 1, "{ev:?}");
    let g = got[0];
    assert_eq!(g.frame, body);
    assert!(g.decrypted);
    assert_eq!(g.decap, Decap::NativeWifi);
    assert_eq!(g.payload_off, 24, "a 3-address header");
    assert_eq!(g.tid, 3);
    assert_eq!(g.rssi, -95 + 52);
    assert_eq!(g.freq, 2437);
    assert_eq!(g.pn, 5);
    assert_eq!(
        g.rate,
        RxRate::Ht {
            mcs: 7,
            bw40: true,
            sgi: true
        }
    );
    assert_eq!(r.d.rx_fill(), fill, "the ring is topped up again");

    // the same PN again is a replay; a lower one not yet seen is out of order and passes; the first again does not
    for (pn, seen) in [(5u64, false), (4, true), (4, false), (6, true)] {
        let s = RxSpec {
            hdr: qos_air_hdr(true, pn),
            ..spec.clone()
        };
        r.m.fill_rx(r.d.platform_mut(), &s);
        r.m.rx_ind(&[1], 1);
        let ev = r.run(4);
        assert_eq!(data_rx(&ev).len(), usize::from(seen), "pn {pn}: {ev:?}");
    }
    assert_eq!(r.d.stats().rx.replay, 2);

    // leave: peer, vdev down, vdev stop, as mac80211 orders the teardown
    let before = r.m.wmi_log.len();
    r.d.leave().unwrap();
    let ev = r.run(10);
    assert!(
        ev.contains(&Event::Disconnected(Disconnect::Left)),
        "{ev:?}"
    );
    let ids: Vec<u32> = r.m.wmi_log[before..].iter().map(|c| c.id).collect();
    assert_eq!(
        ids,
        vec![
            id::PEER_DELETE,
            id::VDEV_SET_PARAM,
            id::VDEV_SET_PARAM,
            id::VDEV_DOWN,
            id::VDEV_SET_PARAM,
            id::VDEV_STOP
        ]
    );
    let erp: Vec<(u32, u32)> = r.m.wmi_log[before + 1..before + 3]
        .iter()
        .map(|c| (c.w(1), c.w(2)))
        .collect();
    assert_eq!(erp, vec![(7, 1), (8, 1)], "reset_erp_info: long slot and preamble before vdev_down");
    // and a new join works; arvif keeps the timing it was told, so this vdev_start carries it
    let before = r.m.wmi_log.len();
    r.d.join(&join_params()).unwrap();
    let ev = r.run(20);
    assert!(ev.contains(&Event::VdevStarted), "{ev:?}");
    let vs = r.m.wmi_log[before].tlv();
    assert_eq!((word(&vs[0].1, 2), word(&vs[0].1, 3)), (100, 2));
}

#[test]
fn amsdu_chaining_and_the_drop_rules() {
    let mut r = joined();
    // an A-MSDU of three subframes: flags in the last descriptor rule
    let hdr = qos_air_hdr(false, 0);
    let first = RxSpec {
        payload: nwifi(200),
        last_msdu: false,
        hdr: hdr.clone(),
        ..RxSpec::default()
    };
    let mid = RxSpec {
        payload: nwifi(300),
        first_msdu: false,
        last_msdu: false,
        ..RxSpec::default()
    };
    let last = RxSpec {
        payload: nwifi(400),
        first_msdu: false,
        last_msdu: true,
        ..RxSpec::default()
    };
    r.m.fill_rx(r.d.platform_mut(), &first);
    r.m.fill_rx(r.d.platform_mut(), &mid);
    r.m.fill_rx(r.d.platform_mut(), &last);
    r.m.rx_ind(&[1], 1);
    let ev = r.run(4);
    let got = data_rx(&ev);
    assert_eq!(got.len(), 3, "{ev:?}");
    assert_eq!(got[1].frame.len(), 332);
    assert_eq!(got[2].air_hdr[..hdr.len()], hdr[..]);
    assert!(!got[0].decrypted);

    // a raw chained MSDU is put back together
    let raw = RxSpec {
        payload: vec![1; 1688],
        chained: vec![vec![2; 2048], vec![3; 100]],
        decap: 0,
        hdr: hdr.clone(),
        ..RxSpec::default()
    };
    r.m.fill_rx(r.d.platform_mut(), &raw);
    r.m.rx_ind(&[1], 1);
    let ev = r.run(4);
    let got = data_rx(&ev);
    assert_eq!(got.len(), 1, "{ev:?}");
    assert_eq!(got[0].frame.len(), 1688 + 2048 + 100);
    assert_eq!(got[0].decap, Decap::Raw);
    assert_eq!(got[0].frame[1688 + 2048], 3);
    assert_eq!(got[0].payload_off, 0);

    // chained but native wifi: Linux can only unchain raw, and drops
    let bad = RxSpec {
        decap: 1,
        ..raw.clone()
    };
    r.m.fill_rx(r.d.platform_mut(), &bad);
    r.m.rx_ind(&[1], 1);
    let ev = r.run(4);
    assert_eq!(data_rx(&ev).len(), 0);
    assert_eq!(r.d.stats().rx.unchain, 1);

    // each rule on the same frame
    let plain = RxSpec {
        payload: nwifi(64),
        hdr: hdr.clone(),
        ..RxSpec::default()
    };
    for (att, enc, rule) in [
        (model::ATT_FCS_ERR, 7, "fcs"),
        (model::ATT_MGMT, 7, "mgmt"),
        (model::ATT_MIC_ERR, 4, "mic"),
        (model::ATT_DECRYPT_ERR, 6, "crypto"),
        (model::ATT_PEER_INVALID, 6, "crypto"),
    ] {
        let s = RxSpec {
            attention: plain.attention | att,
            enctype: enc,
            ..plain.clone()
        };
        let before = r.d.stats().rx;
        r.m.fill_rx(r.d.platform_mut(), &s);
        r.m.rx_ind(&[1], 1);
        let ev = r.run(4);
        assert_eq!(data_rx(&ev).len(), 0, "{rule}");
        let after = r.d.stats().rx;
        let n = match rule {
            "fcs" => after.fcs - before.fcs,
            "mgmt" => after.mgmt - before.mgmt,
            "mic" => after.mic - before.mic,
            _ => after.crypto - before.crypto,
        };
        assert_eq!(n, 1, "{rule}");
    }

    // a flipped A-MSDU bit: the first subframe's DA is an LLC header (ath10k_htt_rx_validate_amsdu)
    let mut h = qos_air_hdr(false, 0);
    h.extend_from_slice(&[0xaa, 0xaa, 3, 0, 0, 0]);
    let s = RxSpec {
        last_msdu: false,
        hdr: h,
        payload: nwifi(64),
        ..RxSpec::default()
    };
    let end = RxSpec {
        first_msdu: false,
        payload: nwifi(64),
        ..RxSpec::default()
    };
    r.m.fill_rx(r.d.platform_mut(), &s);
    r.m.fill_rx(r.d.platform_mut(), &end);
    r.m.rx_ind(&[1], 1);
    let ev = r.run(4);
    assert_eq!(data_rx(&ev).len(), 0);
    assert_eq!(r.d.stats().rx.bad_amsdu, 1);

    // an A-MPDU's later MPDUs reuse the PPDU's status
    let a = RxSpec {
        payload: nwifi(64),
        hdr: hdr.clone(),
        rssi_comb: 30,
        attention: model::ATT_DONE | model::ATT_FIRST_MPDU,
        ..RxSpec::default()
    };
    let b = RxSpec {
        rssi_comb: 0,
        attention: model::ATT_DONE | model::ATT_LAST_MPDU,
        ..a.clone()
    };
    r.m.fill_rx(r.d.platform_mut(), &a);
    r.m.fill_rx(r.d.platform_mut(), &b);
    r.m.rx_ind(&[2], 1);
    let ev = r.run(4);
    let got = data_rx(&ev);
    assert_eq!(got.len(), 2, "{ev:?}");
    assert_eq!((got[0].rssi, got[1].rssi), (-65, -65));
}

#[test]
fn frames_before_a_channel_are_dropped_like_linux_no_channel() {
    let mut r = up();
    let spec = RxSpec {
        payload: nwifi(64),
        hdr: qos_air_hdr(false, 0),
        ..RxSpec::default()
    };
    r.m.fill_rx(r.d.platform_mut(), &spec);
    r.m.rx_ind(&[1], 1);
    let ev = r.run(4);
    assert_eq!(data_rx(&ev).len(), 0);
    assert_eq!(r.d.stats().rx.no_channel, 1);
}

#[test]
fn in_order_path_with_full_reorder_firmware() {
    let mut mc = ModelCfg::default();
    mc.services.push(65);
    let (mut r, _) = up_with(mc);
    // the resource configuration reflects the service
    let rc = r.m.cmds(id::INIT)[0].tlv()[1].1.clone();
    assert_eq!(word(&rc, 2), 4, "offload peers");
    r.d.join(&join_params()).unwrap();
    let ev = r.run(20);
    assert!(ev.contains(&Event::VdevStarted), "{ev:?}");
    let hdr = qos_air_hdr(true, 9);
    let a = RxSpec {
        payload: nwifi(100),
        last_msdu: false,
        enctype: 6,
        hdr: hdr.clone(),
        ..RxSpec::default()
    };
    let b = RxSpec {
        payload: nwifi(200),
        first_msdu: false,
        enctype: 6,
        ..RxSpec::default()
    };
    let pa = r.m.fill_rx(r.d.platform_mut(), &a)[0];
    let pb = r.m.fill_rx(r.d.platform_mut(), &b)[0];
    r.m.rx_in_ord(&[(pa, 132), (pb, 232)], false);
    let fill = r.d.rx_fill();
    let ev = r.run(4);
    let got = data_rx(&ev);
    assert_eq!(got.len(), 2, "{ev:?}");
    assert_eq!(got[0].frame.len(), 132);
    assert_eq!(got[1].frame.len(), 232);
    assert!(got[1].decrypted);
    assert_eq!(got[0].pn, 0, "the firmware does replay on this path");
    assert_eq!(r.d.rx_fill(), fill, "refilled");
    assert_eq!(r.d.stats().rx.replay, 0);

    // offloaded frames: a short header, protected bit stripped
    let (addr, _) = {
        let s = RxSpec::default();
        (r.m.fill_rx(r.d.platform_mut(), &s)[0], 0)
    };
    let mut f = nwifi(40);
    f[1] |= 0x40;
    let mut raw = (f.len() as u16).to_le_bytes().to_vec();
    raw.extend_from_slice(&r.m.peer_id.to_le_bytes());
    raw.extend_from_slice(&[0, 6, 0x40]);
    raw.extend_from_slice(&f);
    r.d.platform_mut().dma.dev_write(addr, &raw);
    r.m.rx_in_ord(&[(addr, 0)], true);
    let ev = r.run(4);
    let got = data_rx(&ev);
    assert_eq!(got.len(), 1, "{ev:?}");
    assert_eq!(got[0].frame[1] & 0x40, 0);
    assert_eq!(got[0].frame[4..], f[4..]);

    // an address the host never posted is the end of the data path, as in Linux
    r.m.rx_in_ord(&[(0xdead_0000, 10)], false);
    let ev = r.run(4);
    assert!(ev.contains(&Event::FwError(Fault::RxConfused)), "{ev:?}");
}

#[test]
fn unfinished_descriptor_confuses_the_ring_and_says_so() {
    let mut r = joined();
    let s = RxSpec {
        attention: 0,
        payload: nwifi(64),
        ..RxSpec::default()
    };
    r.m.fill_rx(r.d.platform_mut(), &s);
    r.m.rx_ind(&[1], 1);
    let ev = r.run(4);
    assert!(ev.contains(&Event::FwError(Fault::RxConfused)), "{ev:?}");
    assert_eq!(
        r.d.tx_data(&nwifi(10), 0, false),
        Err(DriverError::NotReady)
    );
    assert!(!r.d.can_tx());
}

#[test]
fn a_lying_msdu_length_stays_inside_the_buffer() {
    let mut r = joined();
    let s = RxSpec {
        payload: nwifi(64),
        msdu_len: Some(0x3fff),
        hdr: qos_air_hdr(false, 0),
        ..RxSpec::default()
    };
    r.m.fill_rx(r.d.platform_mut(), &s);
    r.m.rx_ind(&[1], 1);
    let ev = r.run(4);
    // the frame is cut at what the buffer can hold; FakeDma panics on anything past it
    let got = data_rx(&ev);
    assert_eq!(got.len(), 1);
    assert_eq!(got[0].frame.len(), 2048 - 360);
    // a count in the indication beyond the ring: pops run out, the ring is confused, and the loop ended
    r.m.rx_ind(&[255, 255, 255, 255], 1);
    let ev = r.run(4);
    assert!(ev.contains(&Event::FwError(Fault::RxConfused)), "{ev:?}");
}

#[test]
fn tx_backpressure_and_completions() {
    let mut r = joined();
    r.m.auto_tx_complete = false;
    let f = nwifi(500);
    let mut ids = Vec::new();
    while r.d.can_tx() {
        ids.push(r.d.tx_data(&f, 0, false).unwrap());
        assert!(ids.len() <= 1056);
    }
    // CE4 stops the sender before the id table does, as in Linux: 2047 entries, two per MSDU.
    assert_eq!(ids.len(), 1023);
    assert_eq!(r.d.tx_pending(), 1023);
    assert_eq!(r.d.tx_data(&f, 0, false), Err(DriverError::Busy));
    // The firmware sees them all
    r.run(2);
    assert_eq!(r.m.frames_tx.len(), 1023);
    let want: Vec<u16> = (0..1023).collect();
    let seen: Vec<u16> = r.m.frames_tx.iter().map(|t| t.id).collect();
    assert_eq!(seen, want);
    // odd and even completions, no-ack and discard
    r.m.tx_complete(&[0, 1, 2], 0);
    r.m.tx_complete(&[3], 2);
    r.m.tx_complete(&[4, 5], 1);
    // ids out of range and twice are counted, not panicked on
    r.m.tx_complete(&[2000, 0xffff], 0);
    r.m.tx_complete(&[0], 0);
    let ev = r.run(4);
    let done: Vec<(u16, bool)> = ev
        .iter()
        .filter_map(|e| match e {
            Event::TxFree { id, acked } => Some((*id, *acked)),
            _ => None,
        })
        .collect();
    assert_eq!(
        done,
        vec![
            (0, true),
            (1, true),
            (2, true),
            (3, false),
            (4, false),
            (5, false)
        ]
    );
    assert_eq!(r.d.stats().stale_tx, 3);
    assert!(r.d.can_tx());
    // the lowest free id is used next, as idr_alloc does
    assert_eq!(r.d.tx_data(&f, 0, false), Ok(0));
    // too long
    assert_eq!(
        r.d.tx_data(&vec![0; 3000], 0, false),
        Err(DriverError::Invalid)
    );
}

#[test]
fn management_details() {
    let mut r = joined();
    // a protected deauthentication gets its MIC room
    let mut de = vec![0xc0, 0x40, 0, 0];
    de.extend_from_slice(&BSS);
    de.extend_from_slice(&ME);
    de.extend_from_slice(&BSS);
    de.extend_from_slice(&[0, 0, 7, 0]);
    r.d.tx_mgmt(&de).unwrap();
    r.run(4);
    assert_eq!(r.m.mgmt_tx[0].1.len(), de.len() + 8);
    assert_eq!(r.m.mgmt_tx[0].1[..de.len()], de[..]);
    // too short, too long
    assert_eq!(r.d.tx_mgmt(&[0; 10]), Err(DriverError::Invalid));
    assert_eq!(r.d.tx_mgmt(&vec![0; 3000]), Err(DriverError::Invalid));
    // status comes from the status array of a bundle, which Linux reads wrongly
    r.m.auto_mgmt_complete = false;
    r.d.tx_mgmt(&de).unwrap();
    let ev = r.run(2);
    assert!(!ev.iter().any(|e| matches!(e, Event::MgmtTxDone { .. })));
    let id1 = r.m.mgmt_tx[1].0;
    r.m.complete_mgmt(id1, 1);
    let ev = r.run(4);
    assert!(ev.contains(&Event::MgmtTxDone { ok: false }), "{ev:?}");
    // single form, and an unknown id
    r.m.cfg.bundle_mgmt = false;
    r.d.tx_mgmt(&de).unwrap();
    r.run(2);
    let id2 = r.m.mgmt_tx[2].0;
    r.m.complete_mgmt(id2, 0);
    r.m.complete_mgmt(0xdead, 0);
    let ev = r.run(4);
    assert!(ev.contains(&Event::MgmtTxDone { ok: true }), "{ev:?}");
    assert_eq!(r.d.stats().stale_tx, 1);
    // slots run out, buffers come back
    r.m.auto_mgmt_complete = false;
    for _ in 0..8 {
        r.d.tx_mgmt(&de).unwrap();
    }
    assert_eq!(r.d.tx_mgmt(&de), Err(DriverError::Busy));
    let live = r.d.platform_mut().dma.live_bytes;
    r.run(6);
    let ids: Vec<u32> = r.m.mgmt_tx[3..].iter().map(|m| m.0).collect();
    for i in ids {
        r.m.complete_mgmt(i, 0);
    }
    r.run(6);
    assert!(r.d.platform_mut().dma.live_bytes < live);
    assert_eq!(r.d.tx_mgmt(&de), Ok(()));
}

#[test]
fn management_without_the_wmi_service_goes_through_htt() {
    let mut mc = ModelCfg::default();
    mc.services.retain(|s| *s != 76);
    let (mut r, _) = up_with(mc);
    r.d.join(&join_params()).unwrap();
    r.run(20);
    let mut auth = vec![0xb0, 0x00, 0, 0];
    auth.extend_from_slice(&BSS);
    auth.extend_from_slice(&ME);
    auth.extend_from_slice(&BSS);
    auth.extend_from_slice(&[0; 8]);
    r.d.tx_mgmt(&auth).unwrap();
    let ev = r.run(4);
    assert!(ev.contains(&Event::MgmtTxDone { ok: true }), "{ev:?}");
    assert_eq!(r.m.frames_tx[0].frame, auth);
    assert_eq!(r.m.frames_tx[0].flags0, (3 << 5) | 1);
    assert_eq!(r.m.frames_tx[0].flags1, (17 << 6) | (1 << 11));
    assert!(r.m.cmds(id::MGMT_TX_SEND).is_empty());
}

#[test]
fn link_events() {
    let mut r = joined();
    r.m.roam(2);
    r.m.roam(1); // better AP: ignored
    r.m.kickout(&[9; 6]); // not ours
    let ev = r.run(4);
    assert_eq!(ev.iter().filter(|e| **e == Event::BeaconLoss).count(), 1);
    assert!(!ev.iter().any(|e| matches!(e, Event::Disconnected(_))));
    r.m.kickout(&BSS);
    let ev = r.run(4);
    assert!(
        ev.contains(&Event::Disconnected(Disconnect::Kickout)),
        "{ev:?}"
    );
}

#[test]
fn timeouts_become_events() {
    // no READY from the target
    let mut mc = ModelCfg::default();
    mc.silent.no_ready = true;
    let mut r = Rig::new(Config::default(), mc);
    r.run(4);
    let ev = r.skip(3000);
    assert_eq!(
        ev,
        vec![Event::FwError(Fault::BootTimeout(BootStage::HtcReady))]
    );
    assert_eq!(r.skip(10_000), vec![], "once");

    // no service ready
    let mut mc = ModelCfg::default();
    mc.silent.no_service_ready = true;
    let mut r = Rig::new(Config::default(), mc);
    r.run(10);
    let ev = r.skip(6000);
    assert_eq!(
        ev,
        vec![Event::FwError(Fault::BootTimeout(BootStage::ServiceReady))]
    );

    // credits stop: "wmi command timeout, restarting hardware"
    let mut mc = ModelCfg::default();
    mc.silent.no_credit = true;
    let mut r = Rig::new(Config::default(), mc);
    r.run(30);
    assert_eq!(r.m.wmi_log.len(), 1, "one credit, one command");
    let _ = r.run(1);
    let ev = r.skip(3100);
    assert!(
        ev.contains(&Event::FwError(Fault::WmiStall))
            || r.skip(10).contains(&Event::FwError(Fault::WmiStall))
    );

    // vdev start refused
    let mc = ModelCfg {
        vdev_start_status: 1,
        ..ModelCfg::default()
    };
    let (mut r, _) = up_with(mc);
    r.d.join(&join_params()).unwrap();
    let ev = r.run(10);
    assert!(ev.contains(&Event::JoinFailed), "{ev:?}");

    // no peer map
    let mut mc = ModelCfg::default();
    mc.silent.no_peer_map = true;
    let (mut r, _) = up_with(mc);
    r.d.join(&join_params()).unwrap();
    r.run(10);
    let ev = r.skip(3100);
    assert!(ev.contains(&Event::JoinFailed), "{ev:?}");

    // no security indication
    let mut mc = ModelCfg::default();
    mc.silent.no_sec_ind = true;
    let (mut r, _) = up_with(mc);
    r.d.join(&join_params()).unwrap();
    r.run(20);
    let k = KeySpec {
        pairwise: true,
        key_idx: 0,
        cipher: Cipher::Ccmp,
        key: vec![1; 16],
    };
    r.d.install_key(&k).unwrap();
    r.run(4);
    let ev = r.skip(3100);
    assert!(ev.contains(&Event::KeyFailed { pairwise: true }), "{ev:?}");
    assert!(r.m.cmds(id::PEER_SET_PARAM).is_empty(), "not authorized");
}

#[test]
fn bad_answers_during_bring_up() {
    // HTT 4
    let mc = ModelCfg {
        htt_major: 4,
        ..ModelCfg::default()
    };
    let mut r = Rig::new(Config::default(), mc);
    let ev = r.run(100);
    assert!(ev.contains(&Event::FwError(Fault::HttVersion(4))), "{ev:?}");

    // a memory request beyond the cap
    let mc = ModelCfg {
        mem_reqs: vec![(1, 0xffff_0000, 0, 0xffff)],
        ..ModelCfg::default()
    };
    let mut r = Rig::new(Config::default(), mc);
    let ev = r.run(100);
    assert!(ev.contains(&Event::FwError(Fault::NoMem)), "{ev:?}");

    // out of memory: the platform says no
    let mut r = Rig::new(
        Config::default(),
        ModelCfg {
            mem_reqs: vec![(1, 4096, 0, 100)],
            ..ModelCfg::default()
        },
    );
    let live = r.d.platform_mut().dma.live_bytes;
    r.d.platform_mut().dma.limit = Some(live + 4096);
    let ev = r.run(100);
    assert!(ev.contains(&Event::FwError(Fault::NoMem)), "{ev:?}");
    r.d.stop();
    assert_eq!(r.d.platform_mut().dma.live_bytes, 0);
}

#[test]
fn hostile_messages_do_not_panic_and_the_driver_stays_up() {
    let mut r = joined();
    let eid_wmi = r.m.wmi_eid;
    let eid_htt = r.m.htt_eid;
    let mut l = common::Lfsr(0x1357_9bdf);
    for round in 0..4000 {
        let n = (l.next() % 200) as usize;
        let mut b = vec![0u8; n];
        l.fill(&mut b);
        match round % 4 {
            0 => {
                // a well-formed HTC header, random WMI event
                let mut f = vec![eid_wmi, 0];
                f.extend_from_slice(&(n as u16).to_le_bytes());
                f.extend_from_slice(&[0; 4]);
                if n >= 4 {
                    let ids = [
                        id::MGMT_RX_EV,
                        id::MGMT_TX_BUNDLE_EV,
                        id::SCAN_EV,
                        id::ROAM_EV,
                        id::PEER_KICKOUT_EV,
                        id::VDEV_START_RESP_EV,
                        id::SERVICE_READY_EV,
                        id::READY_EV,
                    ];
                    let e = ids[(l.byte() & 7) as usize];
                    b[..4].copy_from_slice(&e.to_le_bytes());
                }
                f.extend_from_slice(&b);
                r.m.queue_raw(2, f);
            }
            1 => {
                // a well-formed HTC header, random HTT message that does not touch the rx ring
                let kinds = [
                    0x03u8, 0x04, 0x07, 0x0b, 0x0e, 0x0f, 0x02, 0x05, 0x06, 0x15, 0x30,
                ];
                if n >= 1 {
                    b[0] = kinds[(l.byte() as usize) % kinds.len()];
                }
                let mut f = vec![eid_htt, 0];
                f.extend_from_slice(&(n as u16).to_le_bytes());
                f.extend_from_slice(&[0; 4]);
                f.extend_from_slice(&b);
                r.m.queue_raw(1, f);
            }
            2 => r.m.queue_raw(2, b), // a random HTC frame
            _ => {
                // a trailer claiming things
                let mut f = vec![eid_wmi, 2];
                f.extend_from_slice(&(n as u16).to_le_bytes());
                f.extend_from_slice(&[(n as u8).wrapping_sub(l.byte() & 3), 0, 0, 0]);
                f.extend_from_slice(&b);
                r.m.queue_raw(2, f);
            }
        }
        if round % 8 == 0 {
            r.run(2);
        }
    }
    let ev = r.run(40);
    assert!(
        !ev.iter().any(|e| matches!(e, Event::FwError(_))),
        "the data path survives: {ev:?}"
    );
    assert!(r.d.stats().bad_msg > 0);
    assert!(r.d.stats().htc_drop > 0);
    // HTT through CE5, whose HTC header nobody reads
    r.m.queue_htt_ce5(&[0x07, 0, 1, 0, 0xaa, 0xaa]);
    r.run(4);
}

#[test]
fn hostile_rx_descriptors_stay_in_bounds() {
    let mut l = common::Lfsr(0xfeed_0001);
    for _ in 0..30 {
        let mut r = joined();
        for _ in 0..50 {
            let mut hdr = vec![0u8; 64];
            l.fill(&mut hdr);
            let s = RxSpec {
                attention: l.next() | model::ATT_DONE,
                first_msdu: l.byte() & 1 != 0,
                last_msdu: l.byte() & 3 != 0,
                enctype: l.next() & 0xf,
                decap: l.next() & 3,
                tid: l.next() & 0xf,
                hdr,
                msdu_len: Some(l.next() & 0x3fff),
                ppdu_info1: l.next(),
                ppdu_info2: l.next(),
                ppdu_info3: l.next(),
                payload: vec![0x11; 30],
                ..RxSpec::default()
            };
            r.m.fill_rx(r.d.platform_mut(), &s);
            r.m.rx_ind(&[1], 1);
            let ev = r.run(2);
            if ev.iter().any(|e| matches!(e, Event::FwError(_))) {
                break;
            }
        }
        // whatever happened, every buffer is accounted for at the end
        r.d.stop();
        assert_eq!(r.d.platform_mut().dma.live_bytes, 0);
    }
}

#[test]
fn wrong_state_and_bad_arguments() {
    let plat = RamPlatform::new(1 << 16);
    assert!(matches!(
        D::new(
            plat,
            Config {
                htt_rx_ring_size: 100,
                ..Config::default()
            }
        ),
        Err(DriverError::Invalid)
    ));
    let plat = RamPlatform::new(1 << 16);
    assert!(matches!(
        D::new(
            plat,
            Config {
                ce4_entries: 2,
                ..Config::default()
            }
        ),
        Err(DriverError::Invalid)
    ));
    // too small an arena
    let plat = RamPlatform::new(64);
    let mut d = D::new(plat, Config::default()).unwrap();
    assert!(matches!(d.start(), Err(DriverError::Ce(_))));
    // before the bring-up nothing goes
    let mut r = Rig::new(Config::default(), ModelCfg::default());
    assert_eq!(r.d.start(), Err(DriverError::WrongState));
    assert_eq!(
        r.d.start_scan(&ScanParams::default()),
        Err(DriverError::NotReady)
    );
    assert_eq!(r.d.join(&join_params()), Err(DriverError::NotReady));
    assert_eq!(r.d.tx_mgmt(&[0; 30]), Err(DriverError::NotReady));
    assert_eq!(r.d.tx_data(&[0; 30], 0, false), Err(DriverError::NotReady));
    assert!(!r.d.can_tx());
    // after it, not before the join
    let mut r = up();
    let k = KeySpec {
        pairwise: true,
        key_idx: 0,
        cipher: Cipher::Ccmp,
        key: vec![0; 16],
    };
    assert_eq!(r.d.install_key(&k), Err(DriverError::WrongState));
    assert_eq!(r.d.up(1, &BSS), Err(DriverError::WrongState));
    assert_eq!(
        r.d.peer_assoc(&PeerAssoc::default()),
        Err(DriverError::WrongState)
    );
    assert_eq!(
        r.d.tx_data(&[0; 30], 0, false),
        Err(DriverError::WrongState)
    );
    assert_eq!(r.d.leave(), Ok(()));
    assert_eq!(r.skip(1), vec![Event::Disconnected(Disconnect::Left)]);
    let mut r = joined();
    assert_eq!(
        r.d.install_key(&KeySpec {
            key: vec![0; 5],
            ..k.clone()
        }),
        Err(DriverError::Invalid)
    );
    assert_eq!(
        r.d.install_key(&KeySpec { key_idx: 4, ..k }),
        Err(DriverError::Invalid)
    );
    assert_eq!(
        r.d.peer_assoc(&PeerAssoc {
            bssid: [1; 6],
            ..PeerAssoc::default()
        }),
        Err(DriverError::Invalid)
    );
}

#[test]
fn rings_wrap_under_traffic() {
    let mut r = joined();
    let live = r.d.platform_mut().dma.live_bytes;
    let tx = nwifi(1400);
    let rx = RxSpec {
        payload: nwifi(1000),
        hdr: qos_air_hdr(false, 0),
        ..RxSpec::default()
    };
    let (mut sent, mut got, mut freed) = (0usize, 0usize, 0usize);
    // well past CE1 (512), CE4 (2048) and the rx ring (2048), with a management frame now and then for CE3
    for i in 0..9000 {
        if r.d.can_tx() {
            r.d.tx_data(&tx, (i % 8) as u8, false).unwrap();
            sent += 1;
        }
        r.m.fill_rx(r.d.platform_mut(), &rx);
        r.m.rx_ind(&[1], 1);
        for e in r.run(1) {
            match e {
                Event::DataRx(f) => {
                    assert_eq!(f.frame.len(), rx.payload.len());
                    got += 1;
                }
                Event::TxFree { acked, .. } => {
                    assert!(acked);
                    freed += 1;
                }
                other => panic!("{other:?}"),
            }
        }
    }
    for e in r.run(4) {
        match e {
            Event::DataRx(_) => got += 1,
            Event::TxFree { .. } => freed += 1,
            other => panic!("{other:?}"),
        }
    }
    assert_eq!(sent, 9000);
    assert_eq!(got, 9000);
    assert_eq!(freed, 9000);
    assert_eq!(r.d.rx_fill(), 2047);
    assert_eq!(r.d.tx_pending(), 0);
    assert_eq!(r.m.frames_tx.len(), 9000);
    // nothing leaked or grew, whatever the buffers did
    assert_eq!(r.d.platform_mut().dma.live_bytes, live);
}

// ---- pure parts -------------------------------------------------------------------------------------------------

#[test]
fn replay_window() {
    let mut w = PnWindow::default();
    assert!(w.accept(100));
    assert!(!w.accept(100));
    assert!(w.accept(99));
    assert!(!w.accept(99));
    assert!(w.accept(101));
    assert!(w.accept(38), "63 back is inside");
    assert!(!w.accept(38));
    assert!(!w.accept(37), "64 back is outside");
    assert!(w.accept(1000));
    assert!(!w.accept(101));
    assert!(w.accept(u64::MAX));
    assert!(!w.accept(0));
}

#[test]
fn peer_assoc_vht_and_phymodes_match_the_c_branches() {
    // driven through the driver: what ath10k_peer_assoc_h_phymode decides per band and width
    let cases: &[(u32, Bandwidth, bool, bool, u8)] = &[
        (2437, Bandwidth::Bw20, false, false, wmi::phy_mode::MODE_11G),
        (
            2437,
            Bandwidth::Bw20,
            true,
            false,
            wmi::phy_mode::MODE_11NG_HT20,
        ),
        (
            2437,
            Bandwidth::Bw40,
            true,
            false,
            wmi::phy_mode::MODE_11NG_HT40,
        ),
        (
            2437,
            Bandwidth::Bw20,
            true,
            true,
            wmi::phy_mode::MODE_11AC_VHT20,
        ),
        (
            2437,
            Bandwidth::Bw40,
            true,
            true,
            wmi::phy_mode::MODE_11AC_VHT40,
        ),
        (5180, Bandwidth::Bw20, false, false, wmi::phy_mode::MODE_11A),
        (
            5180,
            Bandwidth::Bw40,
            true,
            false,
            wmi::phy_mode::MODE_11NA_HT40,
        ),
        (
            5180,
            Bandwidth::Bw20,
            true,
            false,
            wmi::phy_mode::MODE_11NA_HT20,
        ),
        (
            5180,
            Bandwidth::Bw80,
            true,
            true,
            wmi::phy_mode::MODE_11AC_VHT80,
        ),
        (
            5180,
            Bandwidth::Bw160,
            true,
            true,
            wmi::phy_mode::MODE_11AC_VHT160,
        ),
        (
            5180,
            Bandwidth::Bw20,
            true,
            true,
            wmi::phy_mode::MODE_11AC_VHT20,
        ),
    ];
    let mut r = joined();
    for (i, (freq, bw, ht, vht, want)) in cases.iter().enumerate() {
        let a = PeerAssoc {
            bssid: BSS,
            aid: 1,
            rates: vec![12],
            ht: ht.then(|| HtInfo {
                rx_mask: [0xff; 10],
                ..HtInfo::default()
            }),
            vht: vht.then(|| VhtInfo {
                cap: 3 << 23,
                rx_mcs_map: 0xfffa,
                rx_highest: 867,
                tx_mcs_map: 0xfffa,
                tx_highest: 866,
            }),
            bandwidth: *bw,
            freq: *freq,
            ..PeerAssoc::default()
        };
        r.d.peer_assoc(&a).unwrap();
        r.run(8);
        let c = r.m.cmds(id::PEER_ASSOC);
        let t = c[i].tlv();
        assert_eq!(word(&t[0].1, 14), u32::from(*want), "case {i}");
        if *vht {
            assert_eq!(word(&t[3].1, 0), 867, "rx_max_rate");
            assert_eq!(word(&t[3].1, 1), 0xfffa);
            assert_eq!(word(&t[3].1, 2), 866);
            // the larger of HT's and VHT's: exponent 3 gives 2^16 - 1, HT factor 0 gives 2^13 - 1
            assert_eq!(word(&t[0].1, 9), (1 << 16) - 1);
            // nss 2 from the map (two streams, then not supported), under our two chains
            assert_eq!(word(&t[0].1, 12), 2);            // ath10k_peer_assoc_h_vht: vht on both bands; the vht_2g flag is 0 in the TLV map, so a 2.4 GHz peer
            // gets no extra bit (0x0800_0000, WMI_PEER_VHT_2G elsewhere, is PMF here)
            let flags = word(&t[0].1, 5);
            assert_ne!(flags & wmi::peer_flags::VHT, 0, "case {i}");
            assert_eq!(flags & wmi::peer_flags::PMF, 0, "case {i}");
        }
        if *ht && !*vht {
            // ten rx mask bytes all set: 80 rates listed, but MAX_SUPPORTED_RATES allows it
            assert_eq!(word(&t[0].1, 18), 80);
        }
    }
    // the CCK rates carry bit 7, in units of 500 kb/s: ath10k_mac_bitrate_to_rate
    assert_eq!(
        [2u8, 4, 11, 22, 12, 108].map(|x| {
            let a = PeerAssoc {
                bssid: BSS,
                rates: vec![x | 0x80],
                freq: 2437,
                ..PeerAssoc::default()
            };
            r.d.peer_assoc(&a).unwrap();
            r.run(6);
            let c = r.m.cmds(id::PEER_ASSOC);
            let t = c[c.len() - 1].tlv();
            t[1].1[0]
        }),
        [0x82, 0x84, 0x8b, 0x96, 12, 108]
    );
}

#[test]
fn txbf_follows_the_c_with_both_ends_capable() {
    let mut r = joined();
    let a = PeerAssoc {
        bssid: BSS,
        vht: Some(VhtInfo {
            cap: (1 << 11) | (1 << 12) | (1 << 19),
            ..VhtInfo::default()
        }),
        freq: 5180,
        ..PeerAssoc::default()
    };
    r.d.peer_assoc(&a).unwrap();
    r.run(8);
    let v: Vec<u32> =
        r.m.cmds(id::VDEV_SET_PARAM)
            .iter()
            .filter(|c| c.w(1) == 45)
            .map(|c| c.w(2))
            .collect();
    // ours: SU bfee and SU bfer (ModelCfg). Theirs: SU bfer, SU bfee, MU bfer.
    // value: SU_BFEE, MU_BFEE (their MU bfer), then SU_BFER; MU_BFEE forces SU_BFEE.
    assert_eq!(v, vec![1 | 2 | 4]);
}

#[test]
fn channel_list_is_the_world_rules() {
    let c = regd::channels(0, 0);
    let f = |x: u32| c.iter().find(|a| a.freq == x).unwrap();
    assert_eq!(c.len(), 14 + 26);
    assert!(!f(2412).passive && f(2412).allow_ibss && f(2412).ht40plus);
    assert!(
        !f(2462).passive && !f(2462).ht40plus,
        "2482 is not a channel"
    );
    assert!(f(2467).passive && !f(2467).allow_ibss);
    assert!(f(2484).passive);
    assert_eq!(f(2412).max_power, 40);
    assert_eq!(f(2412).mode, wmi::phy_mode::MODE_11G);
    assert!(f(5180).passive && !f(5180).chan_radar);
    assert_eq!(f(5180).max_reg_power, 60);
    assert_eq!(f(5180).mode, wmi::phy_mode::MODE_11A);
    assert!(f(5260).chan_radar && f(5700).chan_radar && !f(5720).chan_radar);
    assert!(f(5180).ht40plus);
    assert!(!f(5845).ht40plus);
    assert!(
        c.iter().all(|a| a.freq != 5865),
        "outside the 5470-5850 rule"
    );
    // the firmware's own limits
    let l = regd::channels(5170, 5835);
    assert!(l.iter().all(|a| a.freq <= 5835));
    assert!(l.iter().any(|a| a.freq == 2412));
    assert!(wmi::gen_scan_chan_list(&c).is_ok());
}

#[test]
fn regulatory_pairs_match_the_table_in_the_header() {
    let fix = include_str!("fixtures/regd_oracle.txt");
    let mut n = 0;
    for l in fix.lines().filter(|l| l.starts_with("regpair ")) {
        let v: Vec<u64> = l.split(' ').skip(1).map(num).collect();
        let (a, b, c) = regd::REG_PAIRS[n];
        assert_eq!(
            (u64::from(a), u64::from(b), u64::from(c)),
            (v[0], v[1], v[2]),
            "{l}"
        );
        n += 1;
    }
    assert_eq!(n, regd::REG_PAIRS.len());
    let k = kvl(fix);
    assert_eq!(num(&k["const.WOR4_WORLD"]) as u16, regd::WORLD);
    // what the firmware reports
    assert_eq!(regd::regpair_for(0x37), (0x37, 0x30, 0x30));
    assert_eq!(regd::regpair_for(0x37 | 0x4000), (0x37, 0x30, 0x30));
    assert_eq!(regd::regpair_for(0), (0x64, 0xff, 0xff), "no EEPROM: world");
    assert_eq!(
        regd::regpair_for(0x8000),
        (0x64, 0xff, 0xff),
        "ath_regd_sanitize's case"
    );
    assert_eq!(
        regd::regpair_for(0x8000 | 276),
        (0x64, 0xff, 0xff),
        "a country code: world here"
    );
    assert_eq!(regd::regpair_for(0xabc), (0x64, 0xff, 0xff), "unknown");
}

#[test]
fn firmware_5_says_what_the_constants_say() {
    let fix = include_str!("fixtures/wcn3990_firmware5.txt");
    let line = fix.lines().find(|l| !l.starts_with('#')).unwrap();
    let file = hex(line);
    assert_eq!(file.len(), 60);
    let ies = fw_ie::parse(&file).unwrap();
    let get = |id: u32| ies.iter().find(|(i, _)| *i == id).map(|(_, v)| *v).unwrap();
    assert_eq!(word(get(5), 0), fw_ie::WMI_OP_VERSION);
    assert_eq!(word(get(6), 0), fw_ie::HTT_OP_VERSION);
    let f = get(2);
    let mut feats = 0u32;
    for (i, b) in f.iter().enumerate() {
        feats |= u32::from(*b) << (8 * i);
    }
    assert_eq!(feats, fw_ie::FEATURES);
    // bad files
    assert!(fw_ie::parse(b"short").is_none());
    assert!(fw_ie::parse(b"QCA-ATH10K\0\0\x01\0\0\0\xff\0\0\0").is_none());
}

#[test]
fn the_models_rx_descriptor_offsets_are_the_compilers() {
    let k = kvl(include_str!("fixtures/htt_oracle.txt"));
    let g = |n: &str| num(&k[n]) as usize;
    assert_eq!(g("rxd.attention"), 4);
    assert_eq!(g("rxd.frag_info.common.ring2_more_count"), 10);
    assert_eq!(g("rxd.mpdu_start.info0"), 16);
    assert_eq!(g("rxd.mpdu_start.info1"), 24);
    assert_eq!(g("rxd.msdu_start.common.info0"), 28);
    assert_eq!(g("rxd.msdu_start.common.info1"), 36);
    assert_eq!(g("rxd.msdu_end.common.info0"), 64);
    assert_eq!(g("rxd.ppdu_start.rssi_comb"), 124);
    assert_eq!(g("rxd.ppdu_start.info1"), 128);
    assert_eq!(g("rxd.ppdu_start.info2"), 132);
    assert_eq!(g("rxd.ppdu_start.info3"), 136);
    assert_eq!(g("rxd.rx_hdr_status"), 296);
    assert_eq!(g("rxd.msdu_payload"), 360);
    assert_eq!(
        g("const.RX_ATTENTION_FLAGS_MSDU_DONE") as u32,
        model::ATT_DONE
    );
    assert_eq!(
        g("const.RX_ATTENTION_FLAGS_FCS_ERR") as u32,
        model::ATT_FCS_ERR
    );
    assert_eq!(
        g("const.RX_ATTENTION_FLAGS_DECRYPT_ERR") as u32,
        model::ATT_DECRYPT_ERR
    );
    assert_eq!(
        g("const.RX_ATTENTION_FLAGS_TKIP_MIC_ERR") as u32,
        model::ATT_MIC_ERR
    );
    assert_eq!(
        g("const.RX_ATTENTION_FLAGS_MGMT_TYPE") as u32,
        model::ATT_MGMT
    );
    // and the model's WMI ids and tags
    let w = kvl(include_str!("fixtures/wmi_oracle.txt"));
    let i = |n: &str| num(&w[n]) as u32;
    assert_eq!(i("id.WMI_TLV_MGMT_TX_SEND_CMD"), id::MGMT_TX_SEND);
    assert_eq!(i("id.WMI_TLV_VDEV_START_REQUEST_CMDID"), id::VDEV_START);
    assert_eq!(i("id.WMI_TLV_VDEV_INSTALL_KEY_CMDID"), id::INSTALL_KEY);
    assert_eq!(i("id.WMI_TLV_PEER_ASSOC_CMDID"), id::PEER_ASSOC);
    assert_eq!(
        i("id.WMI_TLV_MGMT_TX_BUNDLE_COMPLETION_EVENTID"),
        id::MGMT_TX_BUNDLE_EV
    );
    assert_eq!(i("id.WMI_TLV_ROAM_EVENTID"), id::ROAM_EV);
}

#[test]
fn send_revert_takes_back_an_unannounced_gather() {
    use lucina_ath10k::ce::{RingPlacement, SEND_FLAG_GATHER};
    let mut p = RamPlatform::new(1 << 16);
    let attrs = lucina_ath10k::snoc_config::HOST_CE_CONFIG[3];
    let mut ce = CopyEngine::new();
    let (regs, mem, _) = p.parts();
    ce.alloc_pipe(
        3,
        &attrs,
        Some(RingPlacement {
            host_offset: 0,
            bus_addr: model::ARENA_BUS,
        }),
        None,
    )
    .unwrap();
    ce.alloc_rri(regs, 0x8000, model::ARENA_BUS + 0x8000);
    lucina_ath10k::ce::DescMem::write32(mem, 0x8000 + 3 * 4, 0);
    ce.init_pipe(regs, mem, 3, &attrs).unwrap();
    let free = ce.num_free_src_entries(3).unwrap();
    ce.send(regs, mem, 3, 1, 0x1000, 16, 0, SEND_FLAG_GATHER)
        .unwrap();
    assert_eq!(ce.num_free_src_entries(3).unwrap(), free - 1);
    ce.send_revert(regs, 3);
    assert_eq!(ce.num_free_src_entries(3).unwrap(), free);
    // nothing left to take back
    ce.send_revert(regs, 3);
    assert_eq!(ce.num_free_src_entries(3).unwrap(), free);
    // announced with a doorbell (the device mirrors the shadow write into its register): stays
    ce.send(regs, mem, 3, 2, 0x1000, 16, 0, 0).unwrap();
    regs.map.insert(0x24_0000 + 3 * 0x1000 + 0x3c, 1);
    ce.send_revert(regs, 3);
    assert_eq!(ce.num_free_src_entries(3).unwrap(), free - 1);
}

// The laptop died about 15 s after the firmware came up while the driver polled the engines' registers, and stayed up when
// it did not. Linux only reaches them from an interrupt. An idle driver must not touch the block at all.
#[test]
fn an_idle_driver_touches_no_register() {
    let mut r = up();
    // Let the bring-up settle: the model stays quiet from here.
    r.run(50);
    let before = (r.d.platform().regs.reads, r.d.platform().regs.writes);
    for _ in 0..2000 {
        r.d.poll(r.now);
        r.now += 1;
    }
    let after = (r.d.platform().regs.reads, r.d.platform().regs.writes);
    assert_eq!(before, after, "reads and writes while idle");
}

// A command rings a shadow register only, and its completion is found in memory.
#[test]
fn a_command_and_its_completion_need_only_the_doorbell() {
    let mut r = up();
    r.run(50);
    let reads = r.d.platform().regs.reads;
    r.d.start_scan(&ScanParams::default()).unwrap();
    r.d.poll(r.now);
    assert_eq!(r.d.platform().regs.reads, reads, "no register is read to send");
}

#[test]
fn power_save_is_enabled_after_pairwise_key_installation() {
    let mut r = joined();
    let ptk = KeySpec {
        pairwise: true,
        key_idx: 0,
        cipher: Cipher::Ccmp,
        key: vec![7; 16],
    };
    r.d.install_key(&ptk).unwrap();
    let ev = r.run(12);
    let keys: Vec<&Event> = ev
        .iter()
        .filter(|e| matches!(e, Event::KeyInstalled { .. }))
        .collect();
    assert_eq!(
        keys,
        vec![
            &Event::KeyInstalled { pairwise: true }
        ]
    );
    let inst = r.m.cmds(id::INSTALL_KEY);
    assert_eq!(inst.len(), 1);
    let t0 = inst[0].tlv();
    assert_eq!(&t0[0].1[4..10], &BSS, "key for the AP's peer");
    assert_eq!(word(&t0[0].1, 4), 0, "pairwise");
    assert_eq!(word(&t0[0].1, 5), 4, "AES CCM");
    assert_eq!(t0[1].1, vec![7; 16]);
    // pairwise key: the peer is authorized
    let auth_cmds: Vec<(u32, u32)> =
        r.m.cmds(id::PEER_SET_PARAM)
            .iter()
            .map(|c| (word(&c.tlv()[0].1, 3), word(&c.tlv()[0].1, 4)))
            .collect();
    assert!(auth_cmds.contains(&(3, 1)), "{auth_cmds:?}");
    // power save is enabled
    let ps_cmds: Vec<(u32, u32)> =
        r.m.cmds(id::STA_PS_PARAM)
            .iter()
            .map(|c| (word(&c.tlv()[0].1, 1), word(&c.tlv()[0].1, 2)))
            .collect();
    assert!(ps_cmds.contains(&(3, 100)), "{ps_cmds:?}");
    // Disabled at join, enabled once authorized: in that order, as mac80211 then ath10k_mac_vif_setup_ps.
    let modes: Vec<u32> = r.m.cmds(id::STA_PS_MODE).iter().map(|c| word(&c.tlv()[0].1, 1)).collect();
    assert_eq!(modes, vec![0, 1], "disabled at join, then enabled");
    let log: Vec<(u32, Vec<u8>)> = r.m.wmi_log.iter().map(|c| (c.id, c.tlv()[0].1.clone())).collect();
    type Pred<'a> = &'a dyn Fn(&(u32, Vec<u8>)) -> bool;
    let at = |pred: Pred| log.iter().position(pred).unwrap();
    let authorize = at(&|c| c.0 == id::PEER_SET_PARAM && word(&c.1, 3) == 3 && word(&c.1, 4) == 1);
    let inactivity = at(&|c| c.0 == id::STA_PS_PARAM && word(&c.1, 1) == 3 && word(&c.1, 2) == 100);
    let enable = at(&|c| c.0 == id::STA_PS_MODE && word(&c.1, 1) == 1);
    assert!(authorize < inactivity && inactivity < enable, "{authorize} {inactivity} {enable}");
}

#[test]
fn power_save_is_disabled_when_power_save_is_false() {
    let mut r = Rig::new(
        Config {
            power_save: false,
            ..Config::default()
        },
        ModelCfg::default()
    );
    let ev = r.run(200);
    let _caps = ev
        .iter()
        .find_map(|e| match e {
            Event::Ready { mac, caps } => {
                assert_eq!(*mac, ME);
                Some(*caps)
            }
            _ => None,
        })
        .unwrap_or_else(|| panic!("not ready: {ev:?}"));
    let start = r.m.wmi_log.len();
    r.d.join(&join_params()).unwrap();
    assert_eq!(r.d.join(&join_params()), Err(DriverError::Busy));
    let ev = r.run(20);
    assert!(ev.contains(&Event::VdevStarted), "{ev:?}");
    // Power save off at join, as with the default config.
    let modes: Vec<u32> = r.m.wmi_log[start..].iter().filter(|c| c.id == id::STA_PS_MODE).map(|c| word(&c.tlv()[0].1, 1)).collect();
    assert_eq!(modes, vec![0]);
    let vs = r.m.cmds(id::VDEV_START)[0].tlv();
    assert_eq!(word(&vs[1].1, 0), 2437);
    assert_eq!(
        word(&vs[1].1, 3) & 0x3f,
        5,
        "11ng ht20 mode in the low bits"
    );
    assert_eq!(vs[1].1[17], 40, "max power, half dBm");

    // auth and assoc over WMI by reference
    let mut auth = vec![0xb0, 0x00, 0, 0];
    auth.extend_from_slice(&BSS);
    auth.extend_from_slice(&ME);
    auth.extend_from_slice(&BSS);
    auth.extend_from_slice(&[0, 0, 0, 0, 1, 0, 0, 0, 0, 0]);
    r.d.tx_mgmt(&auth).unwrap();
    let ev = r.run(6);
    assert!(ev.contains(&Event::MgmtTxDone { ok: true }), "{ev:?}");
    assert_eq!(r.m.mgmt_tx[0].1, auth);

    let assoc = PeerAssoc {
        bssid: BSS,
        aid: 3,
        capability: 0x431,
        rates: vec![0x82, 0x84, 0x8b, 0x96, 12, 18, 24, 36],
        ht: Some(HtInfo {
            cap: 0x01ef | (3 << 2),
            ampdu_factor: 3,
            ampdu_density: 5,
            rx_mask: [0xff, 0xff, 0, 0, 0, 0, 0, 0, 0, 0],
        }),
        vht: None,
        bandwidth: Bandwidth::Bw20,
        wme: true,
        rsn: true,
        wpa: false,
        freq: 2437,
    };
    r.d.peer_assoc(&assoc).unwrap();
    let ev = r.run(6);
    assert!(ev.contains(&Event::PeerAssocDone), "{ev:?}");
    let pa = r.m.cmds(id::PEER_ASSOC)[0].tlv();
    assert_eq!(&pa[0].1[..6], &BSS);
    let w = |i: usize| word(&pa[0].1, 2 + i);
    assert_eq!(w(0), 0, "vdev");
    assert_eq!(w(1), 1, "new assoc");
    assert_eq!(w(2), 3, "aid");
    assert_eq!(
        w(3),
        1 | 2 | 4 | 0x1000 | 0x10000 | 0x8000,
        "auth qos ptk ht ldpc stbc"
    );
    assert_eq!(w(5), 1, "listen interval is 1 for a station");
    assert_eq!(w(7), (1 << 16) - 1, "max mpdu: factor 3");
    assert_eq!(w(8), 4, "density 5 is 4 us");
    assert_eq!(w(10), 2, "two streams, limited by two chains");
    assert_eq!(w(12), u32::from(wmi::phy_mode::MODE_11NG_HT20));
    assert_eq!(pa[1].1[..8], [0x82, 0x84, 0x8b, 0x96, 12, 18, 24, 36]);
    assert_eq!(pa[2].1.len(), 16, "sixteen MCS");
    let ps = r.m.cmds(id::PEER_SET_PARAM);
    let smps = ps[0].tlv();
    assert_eq!(word(&smps[0].1, 3), 1, "smps state param");
    assert_eq!(word(&smps[0].1, 4), 0, "SM PS disabled is none");

    // txbf follows the AP's caps: no VHT, nothing to be, so 0 is written by this vdev having the capability
    let last_txbf: Vec<u32> =
        r.m.cmds(id::VDEV_SET_PARAM)
            .iter()
            .filter(|c| c.w(1) == 45)
            .map(|c| c.w(2))
            .collect();
    assert_eq!(last_txbf, vec![0]);

    let before_up = r.m.wmi_log.len();
    r.d.up(3, &BSS).unwrap();
    assert_eq!(r.d.up(3, &BSS), Err(DriverError::WrongState));
    r.run(8);
    let ids: Vec<u32> = r.m.wmi_log[before_up..].iter().map(|c| c.id).collect();
    assert_eq!(
        ids,
        vec![id::PDEV_SET_PARAM, id::VDEV_UP, id::PEER_SET_PARAM]
    );
    assert_eq!(r.m.cmds(id::VDEV_UP)[0].w(1), 3);

    // EAPOL in the clear, native wifi
    let eapol = nwifi(99);
    let tx_id = r.d.tx_data(&eapol, 16, true).unwrap();
    let ev = r.run(4);
    assert!(
        ev.contains(&Event::TxFree {
            id: tx_id,
            acked: true
        }),
        "{ev:?}"
    );
    let f = &r.m.frames_tx[0];
    assert_eq!(f.frame, eapol);
    assert_eq!(f.id, tx_id);
    assert_eq!(
        f.flags0,
        (1 << 5) | 1 | 4,
        "native wifi, mac header present, no encrypt"
    );
    assert_eq!(f.flags1, (16 << 6) | (1 << 11), "tid 16, vdev 0, postponed");
    assert_eq!(f.prefetch.len(), 52, "50 rounded up");
    assert_eq!(f.eid, r.m.htt_eid);

    // the keys: one at a time, in order, each waits for its security indication
    let ptk = KeySpec {
        pairwise: true,
        key_idx: 0,
        cipher: Cipher::Ccmp,
        key: vec![7; 16],
    };
    let gtk = KeySpec {
        pairwise: false,
        key_idx: 1,
        cipher: Cipher::Ccmp,
        key: vec![9; 16],
    };
    r.d.install_key(&ptk).unwrap();
    r.d.install_key(&gtk).unwrap();
    let ev = r.run(12);
    let keys: Vec<&Event> = ev
        .iter()
        .filter(|e| matches!(e, Event::KeyInstalled { .. }))
        .collect();
    assert_eq!(
        keys,
        vec![
            &Event::KeyInstalled { pairwise: true },
            &Event::KeyInstalled { pairwise: false }
        ]
    );
    let inst = r.m.cmds(id::INSTALL_KEY);
    assert_eq!(inst.len(), 2);
    let t0 = inst[0].tlv();
    assert_eq!(&t0[0].1[4..10], &BSS, "key for the AP's peer");
    assert_eq!(word(&t0[0].1, 4), 0, "pairwise");
    assert_eq!(word(&t0[0].1, 5), 4, "AES CCM");
    assert_eq!(t0[1].1, vec![7; 16]);
    assert_eq!(word(&inst[1].tlv()[0].1, 3), 1, "group key index");
    // pairwise key: the peer is authorized
    let auth_cmds: Vec<(u32, u32)> =
        r.m.cmds(id::PEER_SET_PARAM)
            .iter()
            .map(|c| (word(&c.tlv()[0].1, 3), word(&c.tlv()[0].1, 4)))
            .collect();
    assert!(auth_cmds.contains(&(3, 1)), "{auth_cmds:?}");
    // power save is disabled
    let ps_cmds: Vec<(u32, u32)> =
        r.m.cmds(id::STA_PS_PARAM)
            .iter()
            .map(|c| (word(&c.tlv()[0].1, 1), word(&c.tlv()[0].1, 2)))
            .collect();
    assert!(!ps_cmds.contains(&(3, 100)), "{ps_cmds:?}");
    let modes: Vec<u32> = r.m.cmds(id::STA_PS_MODE).iter().map(|c| word(&c.tlv()[0].1, 1)).collect();
    assert_eq!(modes, vec![0], "only the join's disabled");
}

/// The `vdev_wmm_conf` set as `(cwmin, cwmax, aifs, txop)` per AC, BE, BK, VI, VO.
fn wmm_set(c: &model::WmiCmd) -> Vec<(u32, u32, u32, u32)> {
    let t = c.tlv();
    (0..4usize)
        .map(|i| {
            let w = |k: usize| word(&t[0].1, 1 + 7 * i + k);
            (w(1), w(2), w(3), w(4))
        })
        .collect()
}

fn wmm_acs() -> [wmi::WmmAc; 4] {
    let ac = |cwmin, cwmax, aifs, txop| wmi::WmmAc {
        cwmin,
        cwmax,
        aifs,
        txop,
    };
    // BE, BK, VI, VO
    [
        ac(15, 63, 3, 0),
        ac(15, 1023, 7, 0),
        ac(7, 15, 2, 3008),
        ac(3, 7, 2, 1504),
    ]
}

#[test]
fn conf_tx_goes_per_ac_in_mac80211_order_with_the_uapsd_calls() {
    let (mut r, _) = up_with(ModelCfg {
        services: vec![0, 27, 29, 37, 76],
        ..ModelCfg::default()
    });
    r.d.join(&join_params()).unwrap();
    r.run(20);
    let before = r.m.wmi_log.len();
    let acs = wmm_acs();
    r.d.set_wmm(&acs).unwrap();
    r.run(80);
    let log = &r.m.wmi_log[before..];
    // per AC: vdev_wmm_conf, the four sta ps params, the auto trigger off (service 27)
    assert_eq!(log.len(), 4 * 6);
    let to = |a: wmi::WmmAc| (a.cwmin, a.cwmax, a.aifs, a.txop);
    let zero = (0, 0, 0, 0);
    // mac80211 walks VO, VI, BE, BK; each command carries the whole arvif set as updated so far
    let sets: Vec<_> = log.chunks(6).map(|c| wmm_set(&c[0])).collect();
    assert_eq!(sets[0], vec![zero, zero, zero, to(acs[3])]);
    assert_eq!(sets[1], vec![zero, zero, to(acs[2]), to(acs[3])]);
    assert_eq!(sets[2], vec![to(acs[0]), zero, to(acs[2]), to(acs[3])]);
    assert_eq!(sets[3], acs.map(to).to_vec());
    for (n, (chunk, (wmm_ac, prio))) in log.chunks(6).zip([(3, 7), (2, 5), (1, 2), (0, 0)]).enumerate() {
        assert_eq!(chunk[0].id, wmi::cmd::VDEV_SET_WMM_PARAMS, "ac {n}");
        let ps: Vec<(u32, u32, u32)> = chunk[1..5].iter().map(|c| (c.id, c.w(1), c.w(2))).collect();
        assert_eq!(
            ps,
            vec![
                (id::STA_PS_PARAM, 4, 0), // UAPSD: this station asks for none
                (id::STA_PS_PARAM, 0, 0), // rx wake policy WAKE
                (id::STA_PS_PARAM, 1, 1), // tx wake threshold ALWAYS
                (id::STA_PS_PARAM, 2, 0), // PS-Poll count NO_MAX
            ],
            "ac {n}"
        );
        let trig = &chunk[5];
        assert_eq!(trig.id, wmi::cmd::STA_UAPSD_AUTO_TRIG, "ac {n}");
        let t = trig.tlv();
        assert_eq!(&t[0].1[4..10], &BSS);
        assert_eq!(word(&t[0].1, 3), 1, "num_ac");
        let p = model::walk(&t[1].1);
        let w = |k: usize| word(&p[0].1, k);
        assert_eq!((w(0), w(1), w(2), w(3), w(4)), (wmm_ac, prio, 0, u32::MAX, u32::MAX));
    }
}

#[test]
fn conf_tx_without_the_auto_trigger_services_sends_no_trigger_command() {
    let mut r = joined();
    let before = r.m.wmi_log.len();
    r.d.set_wmm(&wmm_acs()).unwrap();
    r.run(80);
    let ids: Vec<u32> = r.m.wmi_log[before..].iter().map(|c| c.id).collect();
    assert_eq!(ids.len(), 4 * 5);
    assert!(!ids.contains(&wmi::cmd::STA_UAPSD_AUTO_TRIG));
    assert_eq!(ids.iter().filter(|&&i| i == wmi::cmd::VDEV_SET_WMM_PARAMS).count(), 4);
}

#[test]
fn defaults_set_during_a_leave_go_after_vdev_down() {
    let mut r = joined();
    let assoc = PeerAssoc {
        bssid: BSS,
        aid: 1,
        rates: vec![12],
        freq: 2437,
        ..PeerAssoc::default()
    };
    r.d.peer_assoc(&assoc).unwrap();
    r.run(6);
    r.d.up(1, &BSS).unwrap();
    r.run(6);
    let before = r.m.wmi_log.len();
    r.d.leave().unwrap();
    r.d.set_wmm(&wmm_acs()).unwrap();
    let ev = r.run(80);
    assert!(ev.contains(&Event::Disconnected(Disconnect::Left)), "{ev:?}");
    let ids: Vec<u32> = r.m.wmi_log[before..].iter().map(|c| c.id).collect();
    let down = ids.iter().position(|&i| i == id::VDEV_DOWN).unwrap();
    let stop = ids.iter().position(|&i| i == id::VDEV_STOP).unwrap();
    let wmm: Vec<usize> = (0..ids.len()).filter(|&i| ids[i] == wmi::cmd::VDEV_SET_WMM_PARAMS).collect();
    assert_eq!(wmm.len(), 4);
    assert!(wmm.iter().all(|&i| i > down && i < stop), "{ids:x?}");
}

#[test]
fn rx_nss_counts_ht_mask_bytes_as_ieee80211_sta_init_nss() {
    // four chains so the chain limit does not hide the count: bytes 0 and 2 set make rx_nss 2 (not the highest
    // index, 3), and ath10k_peer_assoc_h_ht takes min(rx_nss, 3)
    let (mut r, _) = up_with(ModelCfg {
        num_rf_chains: 4,
        ..ModelCfg::default()
    });
    r.d.join(&join_params()).unwrap();
    r.run(20);
    let mut rx_mask = [0u8; 10];
    rx_mask[0] = 0xff;
    rx_mask[2] = 0xff;
    let a = PeerAssoc {
        bssid: BSS,
        aid: 1,
        rates: vec![12],
        ht: Some(HtInfo {
            rx_mask,
            ..HtInfo::default()
        }),
        freq: 2437,
        ..PeerAssoc::default()
    };
    r.d.peer_assoc(&a).unwrap();
    r.run(8);
    let t = r.m.cmds(id::PEER_ASSOC)[0].tlv();
    assert_eq!(word(&t[0].1, 12), 2);
}

#[test]
fn stop_writes_no_copy_engine_register() {
    // ath10k_snoc_hif_stop / _power_down: no CE register access once the firmware is stopped
    let mut r = up();
    let writes = r.d.platform_mut().regs.writes;
    r.d.stop();
    assert_eq!(r.d.platform_mut().regs.writes, writes);
    let (_, _, dma) = r.d.platform_mut().parts();
    assert_eq!(dma.live_bytes, 0);
    assert!(dma.coherent.is_empty());
}

#[test]
fn linux_coherent_structures_come_from_the_coherent_allocator() {
    // dma_alloc_coherent in htt_rx.c (ring, index), htt_tx.c (txbuf, frag bank), wmi.c (firmware chunks)
    let mut r = up();
    let cfg = Config::default();
    let ring = usize::from(cfg.htt_rx_ring_size) * 8;
    let n = usize::from(cfg.max_pending_tx);
    let (_, _, dma) = r.d.platform_mut().parts();
    let mut coherent: Vec<usize> = dma
        .bufs
        .iter()
        .filter(|(id, _)| dma.coherent.contains(id))
        .map(|(_, (_, b))| b.len())
        .collect();
    coherent.sort_unstable();
    // the chunks as the bring-up test reads them off init: peers + 1 units of the first, 7 of the second
    let mut want = vec![
        ring,
        4,
        n * lucina_ath10k::htt::TXBUF_64_LEN,
        n * lucina_ath10k::htt::MSDU_EXT_DESC_64_LEN,
        256 * 34,
        100 * 7,
    ];
    want.sort_unstable();
    assert_eq!(coherent, want);
    // packet buffers stay on the cached allocator
    let cached: Vec<usize> = dma
        .bufs
        .iter()
        .filter(|(id, _)| !dma.coherent.contains(id))
        .map(|(_, (_, b))| b.len())
        .collect();
    assert!(cached.len() > 1000, "{}", cached.len());
    assert!(!cached.contains(&ring));
}
