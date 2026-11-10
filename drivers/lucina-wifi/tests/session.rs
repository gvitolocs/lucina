//! A whole station session through [`lucina_wifi::Wifi`]: the model firmware of `lucina-ath10k` on the rings below it,
//! the model AP of `lucina-wifi-mac` at the far end of the air, and this file's glue between the two. Frames the firmware
//! takes by WMI management tx or HTT tx go to the AP; what the AP sends comes back as WMI management rx events and HTT rx
//! indications, native wifi after decap, marked decrypted only if the key the firmware was given is the AP's key.
//!
//! These are models, written from the Linux sources by the authors of the code they test. A test here shows that
//! `lucina-wifi`, `lucina-ath10k` and `lucina-wifi-mac` agree with those readings and with each other, not that a modem
//! or a real access point does. Everything is driven through `Wifi`'s public API with a fake clock.

#[path = "../../../drivers/lucina-wifi-mac/tests/common/mod.rs"]
mod common;
#[path = "../../../drivers/lucina-ath10k/tests/model/mod.rs"]
mod model;

use std::collections::VecDeque;

use common::{
    AP, ModelAp, PASSPHRASE, SSID, STA, beacon, ht_cap_ie, ht_oper_ie, nwifi_ip, wmm_param_ie,
};
use lucina_ath10k::driver::{Config, Driver};
use lucina_wifi::Wifi;
use lucina_wifi_mac::crypto::hash::pbkdf2_hmac_sha1;
use model::{Model, ModelCfg, RamPlatform, RxSpec, WmiCmd, id, word};

const NO_ENCRYPT: u8 = 1 << 2;
const WMM_CONF: u32 = lucina_ath10k::wmi_tlv::cmd::VDEV_SET_WMM_PARAMS;
const WRONG_PASSPHRASE: &[u8] = b"incorrect pony staple";
const ENCTYPE_CCMP: u32 = 6;
const ENCTYPE_NONE: u32 = 7;
const REMOTE: [u8; 6] = [0x02, 0xee, 0xee, 0xee, 0xee, 0xee];
const BROADCAST: [u8; 6] = [0xff; 6];

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// What the AP looks like and does.
#[derive(Clone)]
struct ApCfg {
    channel: u8,
    ht_offset_bits: u8,
    width_any: bool,
    ht: bool,
    wmm: bool,
    open: bool,
    /// Heard during scans.
    on_air: bool,
    /// Beacon period once associated. `None` is a firmware that filters beacons, which is what `CONNECTION_MONITOR` is
    /// about in Linux.
    beacon_ms: Option<u64>,
    answer_probes: bool,
    auth_status: u16,
    assoc_status: u16,
    aid: u16,
    /// Deauthenticate on message 2 with a bad MIC, as hostapd ends up doing, rather than stay quiet.
    deauth_on_bad_mic: bool,
    /// Send message 1 ahead of the association response: data and management take different paths.
    msg1_overtakes: bool,
}

impl Default for ApCfg {
    fn default() -> Self {
        ApCfg {
            channel: 6,
            ht_offset_bits: 0,
            width_any: false,
            ht: true,
            wmm: true,
            open: false,
            on_air: true,
            beacon_ms: Some(100),
            answer_probes: true,
            auth_status: 0,
            assoc_status: 0,
            aid: 5,
            deauth_on_bad_mic: true,
            msg1_overtakes: false,
        }
    }
}

/// A frame the AP handed to the air, waiting for an rx buffer.
struct Down {
    frame: Vec<u8>,
    mcast: bool,
    /// Encrypted on the air.
    protected: bool,
    /// Under which key: `None` means what the AP currently uses.
    pn: Option<u64>,
}

/// A data frame as the firmware model fetched it.
#[derive(Clone)]
struct TxSeen {
    frame: Vec<u8>,
    no_encrypt: bool,
    tid: u16,
}

/// The two models and the air between them.
struct Rig {
    wifi: Wifi<RamPlatform>,
    fw: Model,
    now: u64,
    cfg: ApCfg,
    ap: ModelAp,
    /// Other networks' beacons, heard in scans: `(channel, frame)`.
    others: Vec<(u8, Vec<u8>)>,
    /// Complete scans by themselves.
    auto_scan: bool,
    wmi_seen: usize,
    scans: Vec<WmiCmd>,
    mgmt_seen: Vec<Vec<u8>>,
    data_seen: Vec<TxSeen>,
    /// Non-EAPOL frames that reached the AP, in order.
    ap_rx: Vec<Vec<u8>>,
    /// The keys the firmware holds, in the firmware's own decoding of the install command.
    fw_ptk: Option<Vec<u8>>,
    fw_gtk: Option<Vec<u8>>,
    /// The keys the AP encrypts with.
    ap_tk: Option<[u8; 16]>,
    associated: bool,
    /// Waiting for message 2, so that other EAPOL is not taken for it.
    msg1_sent: bool,
    pn: u64,
    down: VecDeque<Down>,
    logs: Vec<String>,
    /// Must never appear in a log line or in the status.
    secrets: Vec<String>,
    /// Use the in-order rx path (`WMI_SERVICE_RX_FULL_REORDER`) instead of rx indications.
    in_ord: bool,
}

impl Rig {
    fn with(cfg: ApCfg, mcfg: ModelCfg) -> Rig {
        // Few in flight, so that a test can fill them.
        let dcfg = Config {
            max_pending_tx: 64,
            ce4_entries: 256,
            ..Config::default()
        };
        let plat = RamPlatform::new(Driver::<RamPlatform>::arena_bytes(&dcfg));
        let mut wifi = Wifi::new(plat, dcfg).unwrap();
        wifi.start().unwrap();
        let in_ord = mcfg.services.contains(&65);
        let ap = ModelAp::new(PASSPHRASE);
        let mut r = Rig {
            wifi,
            fw: Model::new(mcfg),
            now: 1000,
            cfg,
            ap,
            others: Vec::new(),
            auto_scan: true,
            wmi_seen: 0,
            scans: Vec::new(),
            mgmt_seen: Vec::new(),
            data_seen: Vec::new(),
            ap_rx: Vec::new(),
            fw_ptk: None,
            fw_gtk: None,
            ap_tk: None,
            associated: false,
            msg1_sent: false,
            pn: 0,
            down: VecDeque::new(),
            logs: Vec::new(),
            secrets: Vec::new(),
            in_ord,
        };
        r.ap.rsn_ie = common::RSN_PSK_CCMP.to_vec();
        r.keep_secret(PASSPHRASE);
        r.keep_secret(WRONG_PASSPHRASE);
        let pmk = r.ap.pmk;
        r.keep_secret(&pmk);
        let gtk = r.ap.gtk.clone();
        r.keep_secret(&gtk);
        r
    }

    fn model_cfg() -> ModelCfg {
        ModelCfg {
            mac: STA,
            ..ModelCfg::default()
        }
    }

    fn up(cfg: ApCfg) -> Rig {
        let mut r = Rig::with(cfg, Rig::model_cfg());
        r.boot();
        r
    }

    /// Ready, and interface up done: mac80211's default EDCA set (four `conf_tx`) has reached the firmware.
    fn boot(&mut self) {
        assert!(
            self.run_until(2000, |r| r.wifi.is_ready()),
            "not ready: {}",
            self.wifi.status()
        );
        assert!(
            // ath10k_add_interface's three sta ps params, then four per conf_tx
            self.run_until(200, |r| r.count(WMM_CONF) == 4 && r.count(id::STA_PS_PARAM) == 3 + 4 * 4),
            "interface-up WMM defaults: {}",
            self.count(WMM_CONF)
        );
    }

    fn keep_secret(&mut self, b: &[u8]) {
        self.secrets.push(hex(b));
        self.secrets.push(format!("{:02X?}", b));
        self.secrets
            .push(format!("{b:?}")[1..].trim_end_matches(']').to_string());
        if let Ok(s) = core::str::from_utf8(b) {
            self.secrets.push(s.to_string());
        }
    }

    fn beacon_bytes(&self, subtype: u8) -> Vec<u8> {
        let mut extra = Vec::new();
        if !self.cfg.open {
            extra.push(self.ap.rsn_ie.clone());
        }
        if self.cfg.ht {
            extra.push(ht_cap_ie());
            extra.push(ht_oper_ie(
                self.cfg.channel,
                self.cfg.ht_offset_bits,
                self.cfg.width_any,
            ));
        }
        if self.cfg.wmm {
            extra.push(wmm_param_ie());
        }
        let cap = if self.cfg.open { 0x0401 } else { 0x0411 };
        beacon(subtype, &AP, SSID, self.cfg.channel, cap, &extra)
    }

    fn assoc_resp(&self) -> Vec<u8> {
        let mut f = self.ap.assoc_resp(self.cfg.assoc_status, self.cfg.aid);
        if self.cfg.ht {
            f.extend(ht_cap_ie());
            f.extend(ht_oper_ie(
                self.cfg.channel,
                self.cfg.ht_offset_bits,
                self.cfg.width_any,
            ));
        }
        if self.cfg.wmm {
            f.extend(wmm_param_ie());
        }
        f
    }

    fn air_mgmt(&mut self, frame: &[u8]) {
        self.fw.mgmt_rx(u32::from(self.cfg.channel), 50, 0, frame);
    }

    /// The firmware's side of a pass: new commands, and frames to hand over to the AP.
    fn observe(&mut self) {
        while self.wmi_seen < self.fw.wmi_log.len() {
            let c = self.fw.wmi_log[self.wmi_seen].clone();
            self.wmi_seen += 1;
            match c.id {
                id::START_SCAN => self.on_scan(c),
                id::INSTALL_KEY => {
                    let t = c.tlv();
                    let flags = word(&t[0].1, 4);
                    let len = word(&t[0].1, 20) as usize;
                    let key = t[1].1[..len].to_vec();
                    if flags == 0 {
                        self.fw_ptk = Some(key);
                    } else {
                        self.fw_gtk = Some(key);
                    }
                }
                id::PEER_DELETE => {
                    self.fw_ptk = None;
                    self.fw_gtk = None;
                }
                _ => {}
            }
        }
        for (_, frame) in core::mem::take(&mut self.fw.mgmt_tx) {
            self.ap_mgmt(&frame);
            self.mgmt_seen.push(frame);
        }
        for t in core::mem::take(&mut self.fw.frames_tx) {
            let seen = TxSeen {
                no_encrypt: t.flags0 & NO_ENCRYPT != 0,
                tid: (t.flags1 >> 6) & 0x1f,
                frame: t.frame,
            };
            self.ap_data(&seen);
            self.data_seen.push(seen);
        }
    }

    fn on_scan(&mut self, c: WmiCmd) {
        if self.auto_scan {
            let channels: Vec<u32> = c.tlv()[1].1.chunks(4).map(|b| word(b, 0)).collect();
            let freq = |ch: u8| {
                if ch < 14 {
                    2407 + 5 * u32::from(ch)
                } else {
                    5000 + 5 * u32::from(ch)
                }
            };
            if self.cfg.on_air && channels.contains(&freq(self.cfg.channel)) {
                let b = self.beacon_bytes(0x80);
                self.air_mgmt(&b);
            }
            for (ch, f) in self.others.clone() {
                if channels.contains(&freq(ch)) {
                    self.fw.mgmt_rx(u32::from(ch), 30, 0, &f);
                }
            }
            self.fw.scan_event(2, 0, 0);
        }
        self.scans.push(c);
    }

    fn ap_mgmt(&mut self, f: &[u8]) {
        match f[0] {
            0xb0 => {
                let r = self.ap.auth_resp(self.cfg.auth_status);
                self.air_mgmt(&r);
            }
            0x00 => {
                let status = self.cfg.assoc_status;
                let r = self.assoc_resp();
                if status == 0 {
                    self.associated = true;
                    self.ap_tk = None;
                    self.ap.snonce = None;
                    if self.cfg.msg1_overtakes && !self.cfg.open {
                        // queue the data frame first and let it go ahead of the event
                        self.send_msg1();
                        self.flush_down();
                    }
                }
                self.air_mgmt(&r);
                if status == 0 && !self.cfg.open && !self.cfg.msg1_overtakes {
                    self.send_msg1();
                }
            }
            0x40 => {
                if self.cfg.answer_probes {
                    let r = self.beacon_bytes(0x50);
                    self.air_mgmt(&r);
                }
            }
            0xc0 | 0xa0 => {
                self.associated = false;
                self.ap_tk = None;
            }
            _ => {}
        }
    }

    fn send_msg1(&mut self) {
        let m = self.ap.msg1();
        self.msg1_sent = true;
        self.send_down(m);
    }

    fn send_down(&mut self, frame: Vec<u8>) {
        let protected = self.ap_tk.is_some();
        self.down.push_back(Down {
            frame,
            mcast: false,
            protected,
            pn: None,
        });
    }

    /// An AP-to-station frame with the given payload, unicast or to the group.
    fn send_data(&mut self, da: &[u8; 6], ethertype: u16, payload: &[u8]) {
        let mut f = vec![0x08, 0x02, 0, 0];
        f.extend_from_slice(da);
        f.extend_from_slice(&AP);
        f.extend_from_slice(&REMOTE);
        f.extend_from_slice(&[0, 0]);
        f.extend_from_slice(&[0xaa, 0xaa, 0x03, 0, 0, 0]);
        f.extend_from_slice(&ethertype.to_be_bytes());
        f.extend_from_slice(payload);
        let mcast = da[0] & 1 != 0;
        self.down.push_back(Down {
            frame: f,
            mcast,
            protected: !self.cfg.open,
            pn: None,
        });
    }

    fn ap_data(&mut self, t: &TxSeen) {
        let f = &t.frame;
        assert_eq!(f[0], 0x08, "native wifi data");
        assert_eq!(f[1] & 0x03, 0x01, "To-DS");
        assert_eq!(&f[4..10], &AP);
        assert_eq!(&f[10..16], &STA);
        if f.len() >= 32 && f[30..32] == [0x88, 0x8e] {
            self.ap_eapol(t);
            return;
        }
        // the firmware encrypts what is not marked otherwise: it needs the key for that
        assert!(
            !t.no_encrypt,
            "stack data goes through the firmware's crypto"
        );
        if self.cfg.open {
            assert_eq!(f[1] & 0x40, 0, "no Protected bit on an open network");
        } else {
            assert_eq!(f[1] & 0x40, 0x40, "Protected");
            assert_eq!(
                self.fw_ptk.as_deref(),
                self.ap_tk.as_ref().map(|k| &k[..]),
                "the key the data goes out under"
            );
        }
        self.ap_rx.push(f.clone());
    }

    fn ap_eapol(&mut self, t: &TxSeen) {
        let f = &t.frame;
        let protected = f[1] & 0x40 != 0;
        // EAPOL goes through the crypto only when marked protected: before the key, in the clear with NO_ENCRYPT
        assert_eq!(
            t.no_encrypt, !protected,
            "no_encrypt is the opposite of Protected on EAPOL"
        );
        if self.cfg.wmm {
            assert_eq!(t.tid, 7, "EAPOL at the highest TID of a QoS peer");
        } else {
            assert_eq!(t.tid, 16, "the one queue of a non-QoS peer");
        }
        let e = f[32..].to_vec();
        let info = u16::from_be_bytes([e[5], e[6]]);
        let pairwise = info & 0x0008 != 0;
        let secure = info & 0x0200 != 0;
        if pairwise && !secure {
            assert!(self.msg1_sent, "message 2 with no message 1 out");
            self.msg1_sent = false;
            if self.ap.take_msg2(f) {
                let ptk = self.ap.ptk();
                self.keep_secret(&ptk);
                self.keep_secret(&ptk[..16]);
                self.keep_secret(&ptk[16..32]);
                self.keep_secret(&ptk[32..48]);
                let m3 = self.ap.msg3();
                self.send_down(m3);
            } else if self.cfg.deauth_on_bad_mic {
                self.associated = false;
                let d = self.ap.deauth(15);
                self.air_mgmt(&d);
            }
        } else if pairwise && secure {
            assert!(self.ap.check_mic(&e), "message 4's MIC");
            self.ap_tk = Some(self.ap.tk());
        } else {
            assert!(self.ap.check_mic(&e), "group message 2's MIC");
        }
    }

    fn rx_spec(&mut self, d: &Down) -> RxSpec {
        let (mut attention, enctype, hdr);
        attention = model::ATT_DONE | model::ATT_FIRST_MPDU | model::ATT_LAST_MPDU;
        if d.mcast {
            attention |= model::ATT_MCAST;
        }
        if d.protected {
            enctype = ENCTYPE_CCMP;
            let good = if d.mcast {
                self.fw_gtk.is_some() && self.fw_gtk.as_deref() == Some(&self.ap.gtk[..])
            } else {
                self.fw_ptk.is_some()
                    && self.fw_ptk.as_deref() == self.ap_tk.as_ref().map(|k| &k[..])
            };
            if !good {
                attention |= model::ATT_DECRYPT_ERR;
            }
            let pn = d.pn.unwrap_or_else(|| {
                self.pn += 1;
                self.pn
            });
            let p = pn.to_le_bytes();
            hdr = air_hdr(
                &d.frame,
                true,
                &[p[0], p[1], 0, 0x20, p[2], p[3], p[4], p[5]],
            );
        } else {
            enctype = ENCTYPE_NONE;
            hdr = air_hdr(&d.frame, false, &[]);
        }
        RxSpec {
            payload: d.frame.clone(),
            attention,
            enctype,
            decap: 1,
            hdr,
            ..RxSpec::default()
        }
    }

    /// What the firmware does for frames from the air, a few per pass as a radio would: the host refills in between.
    fn flush_down(&mut self) {
        for _ in 0..4 {
            let Some(d) = self.down.pop_front() else {
                return;
            };
            let s = self.rx_spec(&d);
            self.inject_rx(&s);
        }
    }

    fn inject_rx(&mut self, s: &RxSpec) {
        let plat = self.wifi.platform_mut();
        let addrs = self.fw.fill_rx(plat, s);
        if self.in_ord {
            self.fw
                .rx_in_ord(&[(addrs[0], s.payload.len() as u16)], false);
        } else {
            self.fw.rx_ind(&[1], 0);
        }
    }

    /// One pass of everything, 2 ms.
    fn tick(&mut self) {
        self.fw.step(self.wifi.platform_mut());
        self.observe();
        if !self.wifi.is_dead() {
            self.flush_down();
        }
        if self.associated
            && let Some(p) = self.cfg.beacon_ms
            && self.now.is_multiple_of(p)
        {
            let b = self.beacon_bytes(0x80);
            self.air_mgmt(&b);
        }
        self.wifi.poll(self.now);
        self.audit();
        self.now += 2;
    }

    fn audit(&mut self) {
        while let Some(line) = self.wifi.pop_log() {
            self.check_secret(&line);
            self.logs.push(line);
        }
        let status = self.wifi.status();
        self.check_secret(&status);
    }

    fn check_secret(&self, text: &str) {
        for s in &self.secrets {
            assert!(!text.contains(s.as_str()), "the secret {s} in: {text}");
        }
    }

    fn run(&mut self, ms: u64) {
        for _ in 0..ms / 2 {
            self.tick();
        }
    }

    fn run_until(&mut self, ms: u64, mut done: impl FnMut(&Rig) -> bool) -> bool {
        for _ in 0..ms / 2 {
            if done(self) {
                return true;
            }
            self.tick();
        }
        done(self)
    }

    fn state(&self) -> String {
        let s = self.wifi.status();
        s.lines()
            .next()
            .unwrap()
            .strip_prefix("state ")
            .unwrap()
            .to_string()
    }

    fn failed(&self) -> Option<String> {
        self.wifi
            .status()
            .lines()
            .find_map(|l| l.strip_prefix("failed ").map(str::to_string))
    }

    fn connect(&mut self) {
        self.connect_with(PASSPHRASE, [0x42; 32]);
    }

    fn connect_with(&mut self, pass: &[u8], entropy: [u8; 32]) {
        self.keep_secret(&entropy);
        self.wifi.connect(SSID, pass, entropy);
    }

    #[track_caller]
    fn connected(&mut self) {
        self.connect();
        self.wait_link();
    }

    #[track_caller]
    fn wait_link(&mut self) {
        assert!(
            self.run_until(5000, |r| r.wifi.link_up()),
            "no link: {} {:?}",
            self.wifi.status(),
            self.logs
        );
        assert_eq!(self.state(), "connected");
        assert_eq!(self.failed(), None);
    }

    /// Everything is torn down and nothing is waiting for anything: the next connection goes all the way.
    #[track_caller]
    fn reconnect_works(&mut self, entropy: u8) {
        assert!(
            self.run_until(6000, |r| r.state() == "idle"),
            "stuck: {}",
            self.wifi.status()
        );
        let before = self.ap_rx.len();
        self.connect_with(PASSPHRASE, [entropy; 32]);
        self.wait_link();
        self.run(20);
        assert_eq!(self.fw_ptk.as_deref(), Some(&self.ap.tk()[..]));
        assert!(self.wifi.can_tx());
        assert!(
            self.wifi
                .tx_ethernet(&eth(&REMOTE, 0x0800, b"after the reconnect"))
        );
        self.send_data(&STA, 0x0800, b"from the far side");
        assert!(self.run_until(200, |r| r.ap_rx.len() > before));
        let e = self.wait_rx();
        assert_eq!(&e[14..], b"from the far side");
    }

    #[track_caller]
    fn wait_rx(&mut self) -> Vec<u8> {
        for _ in 0..500 {
            if let Some(f) = self.wifi.pop_rx() {
                return f;
            }
            self.tick();
        }
        panic!(
            "nothing arrived: {} {:?}",
            self.wifi.status(),
            self.fw.pending_len()
        );
    }

    fn count(&self, id: u32) -> usize {
        self.fw.cmds(id).len()
    }

    /// Index of the last command `id` in the firmware's log.
    fn last(&self, id: u32) -> Option<usize> {
        self.fw.wmi_log.iter().rposition(|c| c.id == id)
    }

    fn vdev_params(&self, param: u32) -> Vec<u32> {
        self.fw
            .cmds(id::VDEV_SET_PARAM)
            .iter()
            .filter(|c| c.w(1) == param)
            .map(|c| c.w(2))
            .collect()
    }

    /// Each `vdev_wmm_conf` as `(cwmin, cwmax, aifs, txop)` per AC, BE, BK, VI, VO.
    fn wmm_sets(&self) -> Vec<Vec<(u32, u32, u32, u32)>> {
        self.fw
            .cmds(WMM_CONF)
            .iter()
            .map(|c| {
                let t = c.tlv();
                (0..4usize)
                    .map(|i| {
                        let w = |k: usize| word(&t[0].1, 1 + 7 * i + k);
                        (w(1), w(2), w(3), w(4))
                    })
                    .collect()
            })
            .collect()
    }

    fn peer_params(&self, param: u32) -> Vec<u32> {
        self.fw
            .cmds(id::PEER_SET_PARAM)
            .iter()
            .filter(|c| word(&c.tlv()[0].1, 3) == param)
            .map(|c| word(&c.tlv()[0].1, 4))
            .collect()
    }

    fn sec_ind(&mut self, pairwise: bool) {
        let mut m = vec![0x0b, 6 | if pairwise { 0x80 } else { 0 }];
        m.extend_from_slice(&self.fw.peer_id.to_le_bytes());
        m.resize(28, 0);
        self.fw.queue_htt(&m);
    }
}

fn air_hdr(nwifi: &[u8], protected: bool, ccmp: &[u8]) -> Vec<u8> {
    // the frame as it was on the air: QoS, and the Protected bit
    let fc: u16 = 0x0288 | if protected { 0x4000 } else { 0 };
    let mut h = fc.to_le_bytes().to_vec();
    let mut rest = nwifi.get(2..).unwrap_or(&[]).to_vec();
    rest.resize(22, 0);
    h.extend_from_slice(&rest);
    h.extend_from_slice(&[0, 0]);
    h.extend_from_slice(ccmp);
    h
}

/// An Ethernet frame from our network stack.
fn eth(da: &[u8; 6], ethertype: u16, payload: &[u8]) -> Vec<u8> {
    let mut f = da.to_vec();
    f.extend_from_slice(&STA);
    f.extend_from_slice(&ethertype.to_be_bytes());
    f.extend_from_slice(payload);
    f
}

fn seq_payload(n: usize, len: usize) -> Vec<u8> {
    let mut p = vec![(n % 251) as u8; len.max(4)];
    p[..4].copy_from_slice(&(n as u32).to_le_bytes());
    p
}

// ---- 1. boot and scan ----------------------------------------------------------------------------------------------

#[test]
fn boot_to_ready_then_a_scan_over_every_leg_lists_each_network() {
    let mut r = Rig::with(ApCfg::default(), Rig::model_cfg());
    assert_eq!(r.state(), "starting");
    r.boot();
    assert_eq!(r.wifi.mac(), STA);
    assert_eq!(r.state(), "idle");
    assert!(!r.wifi.link_up());
    assert!(!r.wifi.can_tx(), "no link, no sending");
    assert!(
        r.logs.iter().any(|l| l.contains("firmware ready")),
        "{:?}",
        r.logs
    );

    let open = [0x02, 1, 1, 1, 1, 1];
    let sae = [0x02, 2, 2, 2, 2, 2];
    let wep = [0x02, 3, 3, 3, 3, 3];
    let five = [0x02, 4, 4, 4, 4, 4];
    let rsn_sae = vec![
        0x30, 20, 1, 0, 0, 0x0f, 0xac, 4, 1, 0, 0, 0x0f, 0xac, 4, 1, 0, 0, 0x0f, 0xac, 8, 0xc0, 0,
    ];
    r.others = vec![
        (1, beacon(0x80, &open, b"open-net", 1, 0x0401, &[])),
        (11, beacon(0x80, &sae, b"sae-net", 11, 0x0411, &[rsn_sae])),
        (6, beacon(0x80, &wep, b"wep-net", 6, 0x0411, &[])),
        (
            36,
            beacon(
                0x80,
                &five,
                b"five-net",
                36,
                0x0411,
                &[common::RSN_PSK_CCMP.to_vec()],
            ),
        ),
    ];

    r.wifi.scan();
    assert_eq!(r.state(), "scanning");
    r.run(30);
    assert_eq!(r.state(), "idle");

    // one start per band, the 2 GHz band first, one after the other
    assert_eq!(r.scans.len(), 2);
    let channels =
        |c: &WmiCmd| -> Vec<u32> { c.tlv()[1].1.chunks(4).map(|b| word(b, 0)).collect() };
    let leg0 = channels(&r.scans[0]);
    let leg1 = channels(&r.scans[1]);
    assert!(
        leg0.contains(&2412) && leg0.contains(&2437) && leg0.iter().all(|f| *f < 3000),
        "{leg0:?}"
    );
    assert!(
        leg1.contains(&5180) && leg1.iter().all(|f| *f > 5000),
        "{leg1:?}"
    );
    // the wildcard SSID, active, and the band's own rates in the probe elements
    for s in &r.scans {
        let t = s.tlv();
        assert_eq!(word(&t[0].1, 14), 0x10 ^ 0x20, "active");
        assert_eq!(t[2].1.len(), 36);
        assert_eq!(word(&t[2].1, 0), 0, "wildcard");
    }
    assert_eq!(&r.scans[0].tlv()[4].1[..6], &[1, 8, 2, 4, 11, 22]);
    assert_eq!(&r.scans[1].tlv()[4].1[..6], &[1, 8, 12, 18, 24, 36]);

    let status = r.wifi.status();
    for line in [
        format!("bss {} 2437 -45 wpa2-psk {}", hex(&AP), hex(SSID)),
        format!("bss {} 2412 -65 open {}", hex(&open), hex(b"open-net")),
        format!("bss {} 2462 -65 wpa3 {}", hex(&sae), hex(b"sae-net")),
        format!("bss {} 2437 -65 other {}", hex(&wep), hex(b"wep-net")),
        format!("bss {} 5180 -65 wpa2-psk {}", hex(&five), hex(b"five-net")),
    ] {
        assert!(status.lines().any(|l| l == line), "{line} not in\n{status}");
    }
}

// ---- 2. connect ----------------------------------------------------------------------------------------------------

#[test]
fn connect_sends_the_commands_the_firmware_expects_with_the_right_values() {
    let mut r = Rig::up(ApCfg::default());
    let before = r.fw.wmi_log.len();
    r.connected();
    r.run(20);
    assert!(r.wifi.status().contains(&format!("ssid {}", hex(SSID))));

    // scan for the SSID on every leg, then the join
    assert_eq!(r.scans.len(), 2);
    for s in &r.scans {
        let t = s.tlv();
        assert_eq!(word(&t[2].1, 0), 9);
        assert_eq!(&t[2].1[4..13], SSID);
    }

    let ids: Vec<u32> = r.fw.wmi_log[before..].iter().map(|c| c.id).collect();
    let pos = |w: u32| {
        ids.iter()
            .position(|i| *i == w)
            .unwrap_or_else(|| panic!("no {w:#x} in {ids:x?}"))
    };
    assert!(pos(id::START_SCAN) < pos(id::VDEV_START));
    assert!(pos(id::VDEV_START) < pos(id::PEER_CREATE));
    assert!(pos(id::PEER_CREATE) < pos(id::MGMT_TX_SEND));
    assert!(pos(id::MGMT_TX_SEND) < pos(id::PEER_ASSOC));
    assert!(pos(id::PEER_ASSOC) < pos(id::VDEV_UP));
    assert!(pos(id::VDEV_UP) < pos(id::INSTALL_KEY));

    // vdev start: the BSS's channel; the timing is 0 on a first join (mac80211 reports it after the channel
    // context) and goes as vdev params: beacon interval after the start, DTIM with the association
    let vs = r.fw.cmds(id::VDEV_START)[0].tlv();
    assert_eq!(word(&vs[0].1, 2), 0, "beacon interval");
    assert_eq!(word(&vs[0].1, 3), 0, "DTIM");
    assert_eq!(r.vdev_params(3), vec![100], "beacon interval param");
    assert_eq!(r.vdev_params(13), vec![2], "DTIM param");
    assert_eq!(word(&vs[1].1, 0), 2437, "frequency");
    assert_eq!(word(&vs[1].1, 1), 2437, "centre");
    assert_eq!(word(&vs[1].1, 3) & 0x3f, 5, "MODE_11NG_HT20");
    assert_eq!(vs[1].1[17], 40, "20 dBm in half dBm");
    let pc = r.fw.cmds(id::PEER_CREATE)[0].tlv();
    assert_eq!(&pc[0].1[4..10], &AP);

    // authentication and association through the air
    assert_eq!(r.mgmt_seen[0][0], 0xb0);
    assert_eq!(&r.mgmt_seen[0][4..10], &AP);
    assert_eq!(&r.mgmt_seen[0][10..16], &STA);
    let assoc = &r.mgmt_seen[1];
    assert_eq!(assoc[0], 0x00);
    assert_eq!(
        &assoc[24..26],
        &0x0431u16.to_le_bytes(),
        "ESS, privacy, short preamble and slot"
    );
    let ies = &assoc[28..];
    assert!(
        ies.windows(22).any(|w| w == common::RSN_PSK_CCMP),
        "our RSN element"
    );

    // peer assoc: what the AP and the radio have in common
    let pa = r.fw.cmds(id::PEER_ASSOC)[0].tlv();
    assert_eq!(&pa[0].1[..6], &AP);
    let w = |i: usize| word(&pa[0].1, 2 + i);
    assert_eq!(w(1), 1, "new association");
    assert_eq!(w(2), 5, "AID");
    assert_eq!(
        w(3),
        0x0001 | 0x0002 | 0x0004 | 0x1000,
        "auth, QoS, 4-way, HT"
    );
    assert_eq!(w(4), 0x0411, "the AP's capability");
    assert_eq!(w(5), 1, "listen interval");
    assert_eq!(
        w(6),
        0x002c,
        "HT caps: SM PS off, SGI 20; no 40 MHz on a 20 MHz BSS"
    );
    assert_eq!(w(7), (1 << 16) - 1, "A-MPDU factor 3");
    assert_eq!(w(8), 4, "MPDU density 4 us");
    assert_eq!(w(9), 0x08 | 0x04 | 0x01, "HT, SGI, double stream");
    assert_eq!(w(10), 2, "two streams");
    assert_eq!(w(12), 5, "MODE_11NG_HT20");
    assert_eq!(
        pa[1].1[..12],
        [0x82, 0x84, 0x8b, 0x96, 12, 18, 24, 36, 48, 72, 96, 108]
    );
    assert_eq!(
        pa[2].1[..16],
        (0..16).collect::<Vec<u8>>()[..],
        "sixteen MCS"
    );

    // vdev up with the same AID, for the AP
    let up = r.fw.cmds(id::VDEV_UP)[0].tlv();
    assert_eq!(word(&up[0].1, 1), 5);
    assert_eq!(&up[0].1[8..14], &AP);

    // the 4-way, with the real derivation at both ends: the AP accepted message 2 and 4, and so the keys match
    let eapol: Vec<&TxSeen> = r
        .data_seen
        .iter()
        .filter(|t| t.frame[30..32] == [0x88, 0x8e])
        .collect();
    assert_eq!(eapol.len(), 2, "messages 2 and 4");
    for t in &eapol {
        assert!(t.no_encrypt, "before the key is in");
        assert_eq!(t.frame[1] & 0x40, 0);
    }

    // the keys
    let keys = r.fw.cmds(id::INSTALL_KEY);
    assert_eq!(keys.len(), 2);
    let pk = keys[0].tlv();
    assert_eq!(word(&pk[0].1, 0), 0, "vdev");
    assert_eq!(&pk[0].1[4..10], &AP, "the AP's peer");
    assert_eq!(word(&pk[0].1, 3), 0, "index 0");
    assert_eq!(word(&pk[0].1, 4), 0, "WMI_KEY_PAIRWISE");
    assert_eq!(word(&pk[0].1, 5), 4, "AES-CCM");
    assert_eq!(
        (word(&pk[0].1, 20), word(&pk[0].1, 21), word(&pk[0].1, 22)),
        (16, 0, 0),
        "lengths"
    );
    assert_eq!(pk[1].1[..16], r.ap.tk());
    let gk = keys[1].tlv();
    assert_eq!(word(&gk[0].1, 3), 1, "the AP's group key index");
    assert_eq!(word(&gk[0].1, 4), 1, "WMI_KEY_GROUP");
    assert_eq!(word(&gk[0].1, 5), 4, "AES-CCM");
    assert_eq!(
        (word(&gk[0].1, 20), word(&gk[0].1, 21), word(&gk[0].1, 22)),
        (16, 0, 0),
        "lengths"
    );
    assert_eq!(gk[1].1[..16], r.ap.gtk[..]);
    assert_eq!(
        r.peer_params(lucina_ath10k::wmi_tlv::peer_param::AUTHORIZE),
        vec![1],
        "authorized after the pairwise key"
    );
    assert!(r.logs.iter().any(|l| l == "the port is open"));
}

#[test]
fn a_40_mhz_bss_on_5_ghz_gets_its_mode_and_centre() {
    let mut r = Rig::up(ApCfg {
        channel: 36,
        ht_offset_bits: 1,
        width_any: true,
        ..ApCfg::default()
    });
    r.connected();
    let vs = r.fw.cmds(id::VDEV_START)[0].tlv();
    assert_eq!(word(&vs[1].1, 0), 5180);
    assert_eq!(word(&vs[1].1, 1), 5190, "10 MHz up");
    assert_eq!(word(&vs[1].1, 3) & 0x3f, 6, "MODE_11NA_HT40");
    let pa = r.fw.cmds(id::PEER_ASSOC)[0].tlv();
    let w = |i: usize| word(&pa[0].1, 2 + i);
    assert_eq!(
        w(3),
        0x0001 | 0x0002 | 0x0004 | 0x1000 | 0x2000,
        "auth, QoS, 4-way, HT, 40 MHz"
    );
    assert_eq!(w(6), 0x002e, "the width stays");
    assert_eq!(
        w(9),
        0x08 | 0x04 | 0x01 | 0x02,
        "HT, SGI, double stream, CW40"
    );
    assert_eq!(w(12), 6, "MODE_11NA_HT40");
    assert_eq!(pa[1].1[..8], [12, 18, 24, 36, 48, 72, 96, 108], "OFDM only");
}

#[test]
fn a_bss_without_wmm_or_ht_is_legacy_and_has_one_queue() {
    let mut r = Rig::up(ApCfg {
        ht: false,
        wmm: false,
        ..ApCfg::default()
    });
    r.connected();
    let vs = r.fw.cmds(id::VDEV_START)[0].tlv();
    assert_eq!(word(&vs[1].1, 3) & 0x3f, 1, "MODE_11G");
    let pa = r.fw.cmds(id::PEER_ASSOC)[0].tlv();
    let w = |i: usize| word(&pa[0].1, 2 + i);
    assert_eq!(w(3), 0x0001 | 0x0004, "auth, 4-way");
    assert_eq!(w(12), 1, "MODE_11G");
    assert!(r.wifi.tx_ethernet(&eth(&REMOTE, 0x0800, b"data")));
    assert!(r.run_until(100, |r| !r.ap_rx.is_empty()));
    assert_eq!(r.data_seen.last().unwrap().tid, 16, "NON_QOS_MCAST_BCAST");
}

#[test]
fn an_open_network_has_no_keys_and_no_protection() {
    let mut r = Rig::up(ApCfg {
        open: true,
        ..ApCfg::default()
    });
    assert!(r.wifi.status().lines().all(|l| !l.starts_with("bss")));
    r.wifi.connect(SSID, b"", [1; 32]);
    r.wait_link();
    // the port opens with the association; the four conf_tx ahead of peer_assoc take a few credits
    assert!(r.run_until(200, |r| r.count(id::VDEV_UP) == 1));
    assert_eq!(r.count(id::INSTALL_KEY), 0);
    let pa = r.fw.cmds(id::PEER_ASSOC)[0].tlv();
    assert_eq!(word(&pa[0].1, 2 + 3) & 0x0004, 0, "no 4-way");
    assert!(r.wifi.tx_ethernet(&eth(&REMOTE, 0x0800, b"clear")));
    assert!(r.run_until(100, |r| !r.ap_rx.is_empty()));
    r.send_data(&STA, 0x0800, b"clear back");
    // the firmware model only reports "decrypted" for encrypted frames; an open network's frames are not
    r.down.back_mut().unwrap().protected = false;
    assert_eq!(&r.wait_rx()[14..], b"clear back");
}

#[test]
fn message_1_may_overtake_the_association_response() {
    let mut r = Rig::up(ApCfg {
        msg1_overtakes: true,
        ..ApCfg::default()
    });
    r.connected();
    assert_eq!(r.fw_ptk.as_deref(), Some(&r.ap.tk()[..]));
}

#[test]
fn the_link_comes_up_only_after_both_keys_are_acknowledged() {
    let mut r = Rig::up(ApCfg::default());
    r.fw.cfg.silent.no_sec_ind = true;
    r.connect();
    assert!(r.run_until(3000, |r| r.count(id::INSTALL_KEY) == 1));
    assert_eq!(
        word(&r.fw.cmds(id::INSTALL_KEY)[0].tlv()[0].1, 4),
        0,
        "pairwise first"
    );
    // message 4 is on its way, and nothing else moves until the firmware answers
    r.run(1000);
    assert!(!r.wifi.link_up());
    assert!(!r.wifi.can_tx());
    assert!(
        !r.wifi.tx_ethernet(&eth(&REMOTE, 0x0800, b"too early")),
        "the port is not open yet"
    );
    assert_eq!(r.count(id::INSTALL_KEY), 1, "one at a time");
    assert!(r.ap_tk.is_some(), "the AP has message 4");
    assert!(
        r.peer_params(lucina_ath10k::wmi_tlv::peer_param::AUTHORIZE)
            .is_empty()
    );

    r.sec_ind(true);
    assert!(r.run_until(100, |r| r.count(id::INSTALL_KEY) == 2));
    assert_eq!(
        word(&r.fw.cmds(id::INSTALL_KEY)[1].tlv()[0].1, 4),
        1,
        "then the group key"
    );
    assert_eq!(
        r.peer_params(lucina_ath10k::wmi_tlv::peer_param::AUTHORIZE),
        vec![1]
    );
    r.run(1000);
    assert!(!r.wifi.link_up());
    assert!(!r.logs.iter().any(|l| l == "the port is open"));

    r.sec_ind(false);
    assert!(r.run_until(100, |r| r.wifi.link_up()));
    assert_eq!(r.state(), "connected");
    assert!(r.wifi.can_tx());
}

// ---- 3. traffic ----------------------------------------------------------------------------------------------------

fn traffic(r: &mut Rig) {
    let sizes = [28usize, 64, 342, 576, 1500];
    let total = 700;
    let mut sent = 0;
    let mut got = 0;
    let base_tx = r.ap_rx.len();
    let mut to_send = 0;
    let mut guard = 0;
    while sent < total || got < total || r.ap_rx.len() - base_tx < total {
        guard += 1;
        assert!(
            guard < 20_000,
            "stuck: sent {sent} got {got} {}",
            r.ap_rx.len() - base_tx
        );
        if sent < total && r.wifi.can_tx() {
            let len = sizes[sent % sizes.len()];
            let (da, ty) = if sent % 7 == 0 {
                (BROADCAST, 0x0806)
            } else {
                (REMOTE, 0x0800)
            };
            assert!(r.wifi.tx_ethernet(&eth(&da, ty, &seq_payload(sent, len))));
            sent += 1;
        }
        if to_send < total && r.down.len() < 8 {
            let len = sizes[to_send % sizes.len()];
            let da = if to_send % 7 == 0 { BROADCAST } else { STA };
            r.send_data(&da, 0x0800, &seq_payload(to_send, len));
            to_send += 1;
        }
        r.tick();
        while let Some(f) = r.wifi.pop_rx() {
            let len = sizes[got % sizes.len()];
            let da = if got % 7 == 0 { BROADCAST } else { STA };
            assert_eq!(&f[..6], &da);
            assert_eq!(&f[6..12], &REMOTE);
            assert_eq!(&f[12..14], &[8, 0]);
            assert_eq!(f[14..], seq_payload(got, len)[..], "frame {got}");
            got += 1;
        }
    }
    for (i, f) in r.ap_rx[base_tx..].iter().enumerate() {
        let len = sizes[i % sizes.len()];
        let (da, ty) = if i % 7 == 0 {
            (BROADCAST, [8, 6])
        } else {
            (REMOTE, [8, 0])
        };
        assert_eq!(&f[16..22], &da, "frame {i}");
        assert_eq!(&f[24..30], &[0xaa, 0xaa, 3, 0, 0, 0]);
        assert_eq!(&f[30..32], &ty);
        assert_eq!(f[32..], seq_payload(i, len)[..], "frame {i}");
    }
}

#[test]
fn ethernet_both_ways_around_every_ring() {
    let mut r = Rig::up(ApCfg::default());
    r.connected();
    traffic(&mut r);
    assert_eq!(r.data_seen.last().unwrap().tid, 0, "best effort");
    assert!(r.wifi.can_tx());
}

#[test]
fn the_same_over_the_in_order_rx_path() {
    let mut mc = Rig::model_cfg();
    mc.services.push(65); // WMI_SERVICE_RX_FULL_REORDER
    let mut r = Rig::with(ApCfg::default(), mc);
    r.boot();
    r.connected();
    traffic(&mut r);
}

#[test]
fn eapol_and_oversize_are_not_for_the_stack_to_send() {
    let mut r = Rig::up(ApCfg::default());
    assert!(!r.wifi.tx_ethernet(&eth(&REMOTE, 0x0800, b"no link")));
    r.connected();
    assert!(!r.wifi.tx_ethernet(&eth(&REMOTE, 0x888e, b"pretend EAPOL")));
    assert!(!r.wifi.tx_ethernet(&[1, 2, 3]));
    // too long for a tx buffer: taken and dropped with a line in the log, nothing breaks
    let big = eth(&REMOTE, 0x0800, &vec![0u8; 4000]);
    r.wifi.tx_ethernet(&big);
    r.run(20);
    assert!(r.ap_rx.is_empty());
    assert!(r.wifi.tx_ethernet(&eth(&REMOTE, 0x0800, b"still fine")));
    assert!(r.run_until(100, |r| r.ap_rx.len() == 1));
}

#[test]
fn backpressure_stops_the_stack_and_loses_nothing() {
    let mut r = Rig::up(ApCfg::default());
    r.connected();
    r.run(20);
    let base = r.data_seen.len();
    r.fw.auto_tx_complete = false;
    let mut sent = 0;
    while r.wifi.can_tx() {
        assert!(
            r.wifi
                .tx_ethernet(&eth(&REMOTE, 0x0800, &seq_payload(sent, 100)))
        );
        sent += 1;
        assert!(sent <= 64, "more in flight than tx buffers");
        r.tick();
    }
    assert_eq!(sent, 64, "max_pending_tx");
    assert!(
        !r.wifi.tx_ethernet(&eth(&REMOTE, 0x0800, b"one too many")),
        "can_tx said no, so the frame is not taken"
    );
    r.run(50);
    assert!(!r.wifi.can_tx());
    assert_eq!(r.data_seen.len() - base, 64);

    // room comes back one completion at a time
    let ids: Vec<u16> = (0..64).collect();
    let mut next = 0;
    let mut released = 0;
    while next < 200 {
        if r.wifi.can_tx() {
            assert!(
                r.wifi
                    .tx_ethernet(&eth(&REMOTE, 0x0800, &seq_payload(sent + next, 100)))
            );
            next += 1;
            r.tick();
            continue;
        }
        let id = ids[released % 64];
        released += 1;
        r.fw.tx_complete(&[id], 0);
        r.run(4);
    }
    r.fw.tx_complete(&ids, 0);
    r.run(20);
    for (i, f) in r.ap_rx[r.ap_rx.len() - (64 + 200)..].iter().enumerate() {
        assert_eq!(f[32..36], (i as u32).to_le_bytes(), "frame {i} in order");
    }
}

#[test]
fn receive_keeps_going_while_transmit_is_full() {
    let mut r = Rig::up(ApCfg::default());
    r.connected();
    r.run(20);
    r.fw.auto_tx_complete = false;
    let mut sent = 0;
    while r.wifi.can_tx() {
        r.wifi
            .tx_ethernet(&eth(&REMOTE, 0x0800, &seq_payload(sent, 100)));
        sent += 1;
    }
    r.run(10);
    // a group rekey: its reply has nowhere to go
    r.ap.gtk = vec![0x77; 16];
    r.ap.gtk_idx = 2;
    let g = r.ap.group1();
    r.send_down(g);
    r.run(20);
    assert_eq!(
        r.fw_gtk.as_deref(),
        Some(&[0x77u8; 16][..]),
        "the key is in"
    );
    r.send_data(&STA, 0x0800, b"coming in anyway");
    assert_eq!(&r.wait_rx()[14..], b"coming in anyway");
    // and the reply goes when there is room
    let before = r.data_seen.len();
    r.fw.tx_complete(&[0], 0);
    assert!(r.run_until(100, |r| r.data_seen.len() > before));
    let reply = &r.data_seen[before];
    assert_eq!(reply.frame[30..32], [0x88, 0x8e]);
    assert!(!reply.no_encrypt, "protected by the PTK");
    assert!(r.ap.check_mic(&reply.frame[32..]));
}

#[test]
fn what_the_firmware_did_not_decrypt_stays_out() {
    let mut r = Rig::up(ApCfg::default());
    r.connected();
    r.run(20);
    // in the clear after the keys
    r.send_data(&STA, 0x0800, b"clear");
    r.down.back_mut().unwrap().protected = false;
    // under a key the firmware does not have
    r.ap_tk = Some([0x11; 16]);
    r.send_data(&STA, 0x0800, b"other key");
    r.run(20);
    assert!(r.wifi.pop_rx().is_none());
    r.ap_tk = Some(r.ap.tk());
    // a replay
    r.send_data(&STA, 0x0800, b"first");
    r.down.back_mut().unwrap().pn = Some(100);
    r.send_data(&STA, 0x0800, b"replayed");
    r.down.back_mut().unwrap().pn = Some(100);
    r.send_data(&STA, 0x0800, b"next");
    r.down.back_mut().unwrap().pn = Some(101);
    assert_eq!(&r.wait_rx()[14..], b"first");
    assert_eq!(&r.wait_rx()[14..], b"next");
}

#[test]
fn rekeying_both_keys_goes_through_the_firmware() {
    let mut r = Rig::up(ApCfg::default());
    r.connected();
    r.run(20);
    // group
    r.ap.gtk = vec![0x77; 16];
    r.ap.gtk_idx = 2;
    r.keep_secret(&[0x77; 16]);
    let g = r.ap.group1();
    r.send_down(g);
    assert!(r.run_until(100, |r| r.count(id::INSTALL_KEY) == 3));
    let gk = r.fw.cmds(id::INSTALL_KEY)[2].tlv();
    assert_eq!(
        (word(&gk[0].1, 3), word(&gk[0].1, 4)),
        (2, 1),
        "index 2, group"
    );
    r.run(20);
    r.send_data(&BROADCAST, 0x0800, b"new group key");
    assert_eq!(&r.wait_rx()[14..], b"new group key");
    // pairwise
    let old = r.ap.tk();
    r.ap.anonce = [0xb6; 32];
    r.send_msg1();
    assert!(r.run_until(300, |r| r.count(id::INSTALL_KEY) == 4));
    r.run(20);
    assert_ne!(r.ap.tk(), old);
    assert_eq!(r.fw_ptk.as_deref(), Some(&r.ap.tk()[..]));
    assert_eq!(r.ap_tk, Some(r.ap.tk()));
    let eapol: Vec<&TxSeen> = r
        .data_seen
        .iter()
        .rev()
        .filter(|t| t.frame[30..32] == [0x88, 0x8e])
        .take(2)
        .collect();
    assert!(
        eapol.iter().all(|t| !t.no_encrypt),
        "a rekey goes under the old key"
    );
    assert!(r.wifi.link_up());
    r.send_data(&STA, 0x0800, b"new pairwise key");
    assert_eq!(&r.wait_rx()[14..], b"new pairwise key");
    assert!(r.wifi.tx_ethernet(&eth(&REMOTE, 0x0800, b"out under it")));
    r.run(10);
}

// ---- 4. wrong passphrase ---------------------------------------------------------------------------------------------

#[test]
fn a_wrong_passphrase_is_a_failure_line_and_the_right_one_then_works() {
    let mut r = Rig::up(ApCfg::default());
    let mut wrong_pmk = [0u8; 32];
    pbkdf2_hmac_sha1(WRONG_PASSPHRASE, SSID, 4096, &mut wrong_pmk);
    r.keep_secret(&wrong_pmk);
    r.connect_with(WRONG_PASSPHRASE, [7; 32]);
    assert!(
        r.run_until(2000, |r| r.failed().is_some()),
        "{}",
        r.wifi.status()
    );
    let why = r.failed().unwrap();
    assert!(
        why.contains("Deauthenticated") && why.contains("during_handshake: true"),
        "{why}"
    );
    assert!(!r.wifi.link_up());
    assert_eq!(r.count(id::INSTALL_KEY), 0);
    assert!(r.run_until(100, |r| r.state() == "idle"));
    // the first line of the check runs on every pass; this says so where it is read
    let everything = r.logs.join("\n") + &r.wifi.status();
    assert!(!everything.contains(core::str::from_utf8(WRONG_PASSPHRASE).unwrap()));
    assert!(!everything.contains(&hex(&wrong_pmk)));
    r.reconnect_works(8);
    assert_eq!(r.failed(), None, "the failure is gone with the new attempt");
}

#[test]
fn an_ap_that_stays_quiet_on_a_wrong_passphrase_times_the_handshake_out() {
    let mut r = Rig::up(ApCfg {
        deauth_on_bad_mic: false,
        ..ApCfg::default()
    });
    r.connect_with(WRONG_PASSPHRASE, [7; 32]);
    assert!(
        r.run_until(12_000, |r| r.failed().is_some()),
        "{}",
        r.wifi.status()
    );
    assert_eq!(r.failed().unwrap(), "HandshakeTimeout");
    r.run(20);
    assert!(
        r.mgmt_seen.iter().any(|f| f[0] == 0xc0),
        "a deauthentication told the AP"
    );
    r.reconnect_works(8);
}

#[test]
fn a_passphrase_that_cannot_be_one_is_refused_without_repeating_it() {
    let mut r = Rig::up(ApCfg::default());
    r.keep_secret(b"tiny");
    r.wifi.connect(SSID, b"tiny", [1; 32]);
    assert!(r.failed().is_some());
    assert_eq!(r.state(), "idle");
    assert!(!r.wifi.status().contains("tiny"));
    assert!(r.scans.is_empty());
}

#[test]
fn a_network_that_is_not_there_ends_in_a_failure() {
    let mut r = Rig::up(ApCfg {
        on_air: false,
        ..ApCfg::default()
    });
    r.connect();
    assert!(r.run_until(500, |r| r.failed().is_some()));
    assert_eq!(r.failed().unwrap(), "connect: NoSuchNetwork");
    assert_eq!(r.state(), "idle");
    r.cfg.on_air = true;
    r.reconnect_works(2);
}

// ---- 5. the link goes away ---------------------------------------------------------------------------------------------

#[test]
fn deauthenticated_by_the_ap_then_connects_again() {
    let mut r = Rig::up(ApCfg::default());
    r.connected();
    r.run(20);
    let deletes = r.count(id::PEER_DELETE);
    let d = r.ap.deauth(7);
    r.associated = false;
    r.ap_tk = None;
    r.air_mgmt(&d);
    assert!(r.run_until(100, |r| !r.wifi.link_up()));
    assert!(!r.wifi.can_tx());
    assert!(!r.wifi.tx_ethernet(&eth(&REMOTE, 0x0800, b"gone")));
    let why = r.failed().unwrap();
    assert!(
        why.contains("Deauthenticated") && why.contains("reason: 7"),
        "{why}"
    );
    assert!(r.run_until(100, |r| r.count(id::VDEV_STOP) == 1));
    assert_eq!(r.count(id::PEER_DELETE), deletes + 1);
    assert!(
        !r.mgmt_seen.iter().any(|f| f[0] == 0xc0),
        "no deauthentication in answer to one"
    );
    r.reconnect_works(9);
}

#[test]
fn a_firmware_that_filters_beacons_does_not_lose_the_link() {
    // `CONNECTION_MONITOR`: mac80211 runs no silence timer for ath10k, the firmware watches the beacons
    let mut r = Rig::up(ApCfg {
        beacon_ms: None,
        answer_probes: false,
        ..ApCfg::default()
    });
    r.connected();
    let seen = r.mgmt_seen.len();
    r.run(30_000);
    assert!(r.wifi.link_up(), "{}", r.wifi.status());
    assert_eq!(r.mgmt_seen.len(), seen, "no probing either");
}

#[test]
fn beacon_loss_with_a_live_ap_is_answered_by_a_probe() {
    let mut r = Rig::up(ApCfg {
        beacon_ms: None,
        ..ApCfg::default()
    });
    r.connected();
    let seen = r.mgmt_seen.len();
    r.fw.roam(2); // WMI_ROAM_REASON_BEACON_MISS
    r.run(5000);
    assert!(r.wifi.link_up());
    assert_eq!(
        r.mgmt_seen.len(),
        seen + 1,
        "one probe, and the response settles it"
    );
    assert_eq!(r.mgmt_seen[seen][0], 0x40);
    assert_eq!(&r.mgmt_seen[seen][4..10], &AP, "the AP itself first");
}

#[test]
fn beacon_loss_with_a_gone_ap_ends_the_link_after_the_probes() {
    let mut r = Rig::up(ApCfg {
        beacon_ms: None,
        answer_probes: false,
        ..ApCfg::default()
    });
    r.connected();
    let seen = r.mgmt_seen.len();
    r.fw.roam(2);
    assert!(
        r.run_until(4000, |r| !r.wifi.link_up()),
        "{}",
        r.wifi.status()
    );
    let probes: Vec<&Vec<u8>> = r.mgmt_seen[seen..]
        .iter()
        .filter(|f| f[0] == 0x40)
        .collect();
    assert_eq!(probes.len(), 5);
    assert_eq!(&probes[1][4..10], &AP);
    assert_eq!(&probes[4][4..10], &BROADCAST, "the last ones broadcast");
    assert_eq!(r.failed().unwrap(), "BeaconLost");
    r.associated = false;
    r.cfg.answer_probes = true;
    r.reconnect_works(10);
}

#[test]
fn a_kickout_only_ends_the_link_if_the_ap_does_not_answer() {
    // Linux: ieee80211_report_low_ack, a CQM event wpa_supplicant ignores on a station. The link is not dropped on it.
    let mut r = Rig::up(ApCfg {
        beacon_ms: None,
        ..ApCfg::default()
    });
    r.connected();
    r.fw.kickout(&AP);
    r.run(5000);
    assert!(r.wifi.link_up());
    assert_eq!(r.count(id::PEER_DELETE), 0);
    // someone else's kickout is nothing
    r.fw.kickout(&REMOTE);
    r.run(100);
    assert!(r.wifi.link_up());

    r.cfg.answer_probes = false;
    r.fw.kickout(&AP);
    assert!(
        r.run_until(4000, |r| !r.wifi.link_up()),
        "{}",
        r.wifi.status()
    );
    assert_eq!(r.failed().unwrap(), "BeaconLost");
    r.associated = false;
    r.cfg.answer_probes = true;
    r.reconnect_works(11);
}

#[test]
fn disconnect_says_goodbye_tears_down_in_order_and_connects_again() {
    let mut r = Rig::up(ApCfg::default());
    r.connected();
    r.run(20);
    let keys = r.fw_ptk.clone();
    r.wifi.disconnect();
    assert!(!r.wifi.link_up());
    assert!(r.run_until(100, |r| r.count(id::VDEV_STOP) == 1));
    let deauth = r
        .mgmt_seen
        .iter()
        .rposition(|f| f[0] == 0xc0)
        .expect("a deauthentication");
    assert_eq!(&r.mgmt_seen[deauth][24..26], &3u16.to_le_bytes(), "leaving");
    let send = r.last(id::MGMT_TX_SEND).unwrap();
    let delete = r.last(id::PEER_DELETE).unwrap();
    let down = r.last(id::VDEV_DOWN).unwrap();
    let stop = r.last(id::VDEV_STOP).unwrap();
    assert!(
        send < delete && delete < down && down < stop,
        "{send} {delete} {down} {stop}"
    );
    assert_eq!(r.failed(), None, "asked for");
    assert_eq!(r.state(), "idle");
    r.reconnect_works(12);
    assert_ne!(r.fw_ptk, keys, "a new key for a new association");
    // disconnecting twice, and when not connected, is nothing
    r.wifi.disconnect();
    r.wifi.disconnect();
    r.run(50);
    r.wifi.disconnect();
    r.reconnect_works(13);
}

#[test]
fn connecting_while_connected_leaves_first() {
    let mut r = Rig::up(ApCfg::default());
    r.connected();
    r.run(20);
    r.connect_with(PASSPHRASE, [3; 32]);
    assert!(
        r.run_until(500, |r| r.count(id::VDEV_START) == 2),
        "{}",
        r.wifi.status()
    );
    assert!(r.last(id::VDEV_STOP).unwrap() < r.last(id::VDEV_START).unwrap());
    r.wait_link();
}

// ---- 6. things that do not go through --------------------------------------------------------------------------------

#[test]
fn a_refused_vdev_start_ends_the_attempt_without_a_frame_on_the_air() {
    let mut mc = Rig::model_cfg();
    mc.vdev_start_status = 1;
    let mut r = Rig::with(ApCfg::default(), mc);
    r.boot();
    r.connect();
    assert!(
        r.run_until(1000, |r| r.failed().is_some()),
        "{}",
        r.wifi.status()
    );
    assert_eq!(r.failed().unwrap(), "the join failed");
    r.run(1000);
    assert!(
        r.mgmt_seen.is_empty(),
        "no vdev, no authentication: {:x?}",
        r.mgmt_seen
    );
    assert_eq!(r.state(), "idle");
    r.fw.cfg.vdev_start_status = 0;
    r.reconnect_works(2);
}

#[test]
fn a_peer_that_never_appears_stops_the_vdev_again() {
    let mut r = Rig::up(ApCfg::default());
    r.fw.cfg.silent.no_peer_map = true;
    r.connect();
    assert!(
        r.run_until(4000, |r| r.failed().is_some()),
        "{}",
        r.wifi.status()
    );
    assert_eq!(r.failed().unwrap(), "the join failed");
    assert!(r.mgmt_seen.is_empty());
    assert!(
        r.run_until(200, |r| r.count(id::VDEV_STOP) == 1),
        "the vdev runs, so it is stopped"
    );
    r.fw.cfg.silent.no_peer_map = false;
    r.reconnect_works(2);
    // never started twice without a stop in between
    let starts = r.count(id::VDEV_START);
    assert_eq!(starts, 2);
    assert!(
        r.fw.wmi_log
            .iter()
            .position(|c| c.id == id::VDEV_STOP)
            .unwrap()
            < r.last(id::VDEV_START).unwrap()
    );
}

#[test]
fn a_slow_join_does_not_count_against_the_ap() {
    let mut r = Rig::up(ApCfg::default());
    r.fw.cfg.silent.no_peer_map = true;
    r.connect();
    assert!(r.run_until(200, |r| r.count(id::PEER_CREATE) == 1));
    // longer than the station's three tries at authentication, and within the 3 s of the peer wait
    r.run(2000);
    assert_eq!(r.state(), "connecting", "{}", r.wifi.status());
    assert!(r.mgmt_seen.is_empty(), "nothing before the peer exists");
    let mut m = vec![0x03, 0];
    m.extend_from_slice(&r.fw.peer_id.to_le_bytes());
    m.extend_from_slice(&AP);
    m.extend_from_slice(&[0, 0]);
    r.fw.queue_htt(&m);
    r.fw.cfg.silent.no_peer_map = false;
    r.wait_link();
    assert_eq!(
        r.mgmt_seen.iter().filter(|f| f[0] == 0xb0).count(),
        1,
        "one authentication"
    );
}

#[test]
fn an_ap_that_refuses_authentication_or_association_is_a_failure() {
    let mut r = Rig::up(ApCfg {
        auth_status: 17,
        ..ApCfg::default()
    });
    r.connect();
    assert!(r.run_until(1000, |r| r.failed().is_some()));
    assert_eq!(r.failed().unwrap(), "AuthRejected { status: 17 }");
    r.cfg.auth_status = 0;
    r.cfg.assoc_status = 17;
    r.run(100);
    r.connect();
    assert!(r.run_until(1000, |r| r.failed().is_some()));
    assert_eq!(r.failed().unwrap(), "AssocRejected { status: 17 }");
    assert_eq!(r.count(id::PEER_ASSOC), 0);
    r.cfg.assoc_status = 0;
    r.reconnect_works(3);
}

#[test]
fn a_pairwise_key_never_acknowledged_fails_and_opens_nothing() {
    let mut r = Rig::up(ApCfg::default());
    r.fw.cfg.silent.no_sec_ind = true;
    r.connect();
    assert!(
        r.run_until(6000, |r| r.failed().is_some()),
        "{}",
        r.wifi.status()
    );
    assert_eq!(r.failed().unwrap(), "the pairwise key did not go in");
    assert_eq!(
        r.count(id::INSTALL_KEY),
        1,
        "the group key stays out of a failed link"
    );
    assert!(
        !r.logs.iter().any(|l| l == "the port is open"),
        "{:?}",
        r.logs
    );
    assert!(!r.wifi.link_up());
    assert!(r.run_until(200, |r| r.count(id::VDEV_STOP) == 1));
    r.associated = false;
    r.ap_tk = None;
    r.fw.cfg.silent.no_sec_ind = false;
    r.reconnect_works(2);
}

#[test]
fn a_group_key_never_acknowledged_fails_and_opens_nothing() {
    let mut r = Rig::up(ApCfg::default());
    r.fw.cfg.silent.no_sec_ind = true;
    r.connect();
    assert!(r.run_until(3000, |r| r.count(id::INSTALL_KEY) == 1));
    r.sec_ind(true);
    assert!(r.run_until(100, |r| r.count(id::INSTALL_KEY) == 2));
    assert!(
        r.run_until(4000, |r| r.failed().is_some()),
        "{}",
        r.wifi.status()
    );
    assert_eq!(r.failed().unwrap(), "the group key did not go in");
    assert!(
        !r.logs.iter().any(|l| l == "the port is open"),
        "{:?}",
        r.logs
    );
    assert!(!r.wifi.link_up());
    r.associated = false;
    r.ap_tk = None;
    r.fw.cfg.silent.no_sec_ind = false;
    r.reconnect_works(2);
}

#[test]
fn deauth_while_a_key_is_on_its_way_still_leaves() {
    let mut r = Rig::up(ApCfg::default());
    r.fw.cfg.silent.no_sec_ind = true;
    r.connect();
    assert!(r.run_until(3000, |r| r.count(id::INSTALL_KEY) == 1));
    let d = r.ap.deauth(1);
    r.associated = false;
    r.ap_tk = None;
    r.air_mgmt(&d);
    assert!(
        r.run_until(4000, |r| r.count(id::VDEV_STOP) == 1),
        "{}",
        r.wifi.status()
    );
    assert!(
        r.failed().unwrap().contains("Deauthenticated"),
        "the cause, not what followed from it"
    );
    assert!(!r.logs.iter().any(|l| l == "the port is open"));
    r.fw.cfg.silent.no_sec_ind = false;
    r.reconnect_works(2);
}

#[test]
fn a_scan_the_firmware_never_starts_is_given_up_on_every_leg() {
    let mut r = Rig::up(ApCfg::default());
    r.fw.cfg.silent.no_scan_events = true;
    r.auto_scan = false;
    r.connect();
    assert!(
        r.run_until(3000, |r| r.failed().is_some()),
        "{}",
        r.wifi.status()
    );
    assert_eq!(r.scans.len(), 2, "each leg is tried");
    assert_eq!(r.count(id::STOP_SCAN), 2);
    assert_eq!(r.failed().unwrap(), "connect: NoSuchNetwork");
    assert_eq!(r.state(), "idle");
    assert!(
        r.logs.iter().filter(|l| *l == "scan aborted").count() == 2,
        "{:?}",
        r.logs
    );
    r.fw.cfg.silent.no_scan_events = false;
    r.auto_scan = true;
    r.reconnect_works(2);
}

#[test]
fn a_scan_the_firmware_refuses_or_never_ends_finishes_anyway() {
    let mut r = Rig::up(ApCfg::default());
    r.fw.cfg.silent.no_scan_events = true;
    r.auto_scan = false;
    r.wifi.scan();
    r.run(10);
    r.fw.scan_event(0x40, 0, 0); // START_FAILED
    r.run(10);
    assert_eq!(r.scans.len(), 2, "on to the next leg");
    r.fw.scan_event(0x40, 0, 0);
    r.run(10);
    assert_eq!(r.state(), "idle");

    // started, and then nothing: ath10k_scan_timeout_work
    r.fw.cfg.silent.no_scan_events = false;
    r.wifi.scan();
    assert_eq!(r.state(), "scanning");
    assert!(
        r.run_until(45_000, |r| r.state() == "idle"),
        "{}",
        r.wifi.status()
    );
    assert_eq!(r.scans.len(), 4);
    r.auto_scan = true;
    r.reconnect_works(2);
}

#[test]
fn connecting_before_the_firmware_is_ready_does_not_stick() {
    let mut r = Rig::with(ApCfg::default(), Rig::model_cfg());
    r.wifi.connect(SSID, PASSPHRASE, [1; 32]);
    assert_eq!(r.failed().unwrap(), "the firmware is not ready");
    r.boot();
    r.run(100);
    assert_eq!(r.state(), "idle", "{}", r.wifi.status());
    assert!(r.scans.is_empty());
    r.reconnect_works(2);
}

#[track_caller]
fn assert_dead(r: &mut Rig) {
    assert!(
        r.run_until(200, |r| r.wifi.is_dead()),
        "{}",
        r.wifi.status()
    );
    assert!(!r.wifi.link_up());
    assert!(!r.wifi.can_tx());
    assert!(!r.wifi.tx_ethernet(&eth(&REMOTE, 0x0800, b"no")));
    assert!(
        r.failed().unwrap().starts_with("firmware: "),
        "{}",
        r.wifi.status()
    );
    assert_eq!(r.state(), "idle", "{}", r.wifi.status());
    // what comes afterwards has nowhere to go, and says so
    r.wifi.scan();
    assert_eq!(r.state(), "idle");
    r.wifi.connect(SSID, PASSPHRASE, [1; 32]);
    assert_eq!(r.state(), "idle");
    assert!(r.failed().is_some());
    r.wifi.disconnect();
    r.run(5000);
    assert_eq!(r.state(), "idle");
    assert!(r.wifi.is_dead());
    assert!(!r.wifi.link_up());
}

#[test]
fn a_firmware_error_while_connected_ends_cleanly() {
    let mut r = Rig::up(ApCfg::default());
    r.connected();
    r.run(20);
    // an rx indication with no frame behind it: rx_confused
    r.fw.rx_ind(&[1], 0);
    assert_dead(&mut r);
    assert!(
        r.failed().unwrap().contains("RxConfused"),
        "{}",
        r.wifi.status()
    );
}

#[test]
fn a_firmware_error_while_scanning_ends_cleanly() {
    let mut r = Rig::up(ApCfg::default());
    r.auto_scan = false;
    r.connect();
    r.run(10);
    assert_eq!(r.state(), "scanning");
    r.fw.rx_ind(&[1], 0);
    assert_dead(&mut r);
}

#[test]
fn a_firmware_error_while_a_key_is_on_its_way_ends_cleanly() {
    let mut r = Rig::up(ApCfg::default());
    r.fw.cfg.silent.no_sec_ind = true;
    r.connect();
    assert!(r.run_until(3000, |r| r.count(id::INSTALL_KEY) == 1));
    r.fw.rx_ind(&[1], 0);
    assert_dead(&mut r);
    assert!(
        r.failed().unwrap().contains("RxConfused"),
        "the first cause stays"
    );
}

#[test]
fn commands_that_get_no_credit_are_a_firmware_error() {
    let mut r = Rig::up(ApCfg::default());
    r.connected();
    r.run(20);
    r.fw.cfg.silent.no_credit = true;
    r.wifi.disconnect();
    assert!(
        r.run_until(4000, |r| r.wifi.is_dead()),
        "{}",
        r.wifi.status()
    );
    assert!(
        r.failed().unwrap().contains("WmiStall"),
        "{}",
        r.wifi.status()
    );
    assert_dead(&mut r);
}

#[test]
fn a_firmware_that_stops_completing_data_does_not_hold_up_the_end_of_the_link() {
    let mut r = Rig::up(ApCfg::default());
    r.connected();
    r.run(20);
    r.fw.auto_tx_complete = false;
    let mut n = 0;
    while r.wifi.can_tx() {
        r.wifi
            .tx_ethernet(&eth(&REMOTE, 0x0800, &seq_payload(n, 64)));
        n += 1;
    }
    // a rekey's reply queues behind the full ring
    r.ap.gtk = vec![0x77; 16];
    r.ap.gtk_idx = 2;
    let g = r.ap.group1();
    r.send_down(g);
    r.run(50);
    r.wifi.disconnect();
    assert!(
        r.run_until(500, |r| r.count(id::VDEV_STOP) == 1),
        "{}",
        r.wifi.status()
    );
    assert_eq!(r.state(), "idle");
    assert!(r.mgmt_seen.iter().any(|f| f[0] == 0xc0));
}

#[test]
fn a_firmware_that_stops_completing_management_frames_does_not_stick() {
    let mut r = Rig::up(ApCfg {
        beacon_ms: None,
        answer_probes: false,
        ..ApCfg::default()
    });
    r.connected();
    r.run(20);
    r.fw.auto_mgmt_complete = false;
    // nothing answers, and every frame keeps its slot: eight and then some
    for _ in 0..4 {
        r.fw.roam(2);
        assert!(
            r.run_until(4000, |r| !r.wifi.link_up()),
            "{}",
            r.wifi.status()
        );
        assert!(
            r.run_until(500, |r| r.state() == "idle"),
            "{}",
            r.wifi.status()
        );
        r.associated = false;
        r.cfg.answer_probes = false;
        r.connect();
        assert!(
            r.run_until(3000, |r| r.wifi.link_up() || r.failed().is_some()),
            "{}",
            r.wifi.status()
        );
        // authentication may have been dropped for lack of slots; retries or a failure both end it
        if !r.wifi.link_up() {
            break;
        }
    }
    assert!(
        r.run_until(3000, |r| r.state() == "idle" || r.state() == "connected"),
        "{}",
        r.wifi.status()
    );
}

// ---- 7. garbage --------------------------------------------------------------------------------------------------------

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
    fn bytes(&mut self, len: usize) -> Vec<u8> {
        (0..len).map(|_| self.next() as u8).collect()
    }
    /// A valid frame with a few things wrong with it.
    fn mutate(&mut self, f: &[u8]) -> Vec<u8> {
        let mut o = f.to_vec();
        for _ in 0..1 + self.below(6) {
            if o.is_empty() {
                break;
            }
            let i = self.below(o.len());
            match self.below(4) {
                0 => o[i] ^= 1 << self.below(8),
                1 => o[i] = self.next() as u8,
                2 => o.truncate(i),
                _ => {
                    let x = self.bytes(self.0 as usize % 16);
                    o.splice(i..i, x);
                }
            }
        }
        o.truncate(1500);
        o
    }
}

#[test]
fn garbage_from_the_air_breaks_nothing() {
    let mut r = Rig::up(ApCfg::default());
    r.connected();
    r.run(20);
    let mut g = Rng(0x1234_5678_9abc_def1);
    let mut frames = 0;
    while frames < 6000 {
        for _ in 0..3 {
            let tmpl = match g.below(6) {
                0 => r.beacon_bytes(0x80),
                1 => r.beacon_bytes(0x50),
                2 => r.ap.auth_resp(0),
                3 => r.assoc_resp(),
                4 => r.ap.deauth(1),
                _ => {
                    let len = g.below(300);
                    g.bytes(len)
                }
            };
            let f = g.mutate(&tmpl);
            r.fw.mgmt_rx(6, 30, 0, &f);
            frames += 1;
        }
        let tmpl = match g.below(5) {
            0 => {
                let m = r.ap.msg1();
                g.mutate(&m)
            }
            1 if r.ap.snonce.is_some() => {
                let m = r.ap.msg3();
                g.mutate(&m)
            }
            2 if r.ap.snonce.is_some() => {
                let m = r.ap.group1();
                g.mutate(&m)
            }
            3 => {
                let len = g.below(1400);
                g.bytes(len)
            }
            _ => g.mutate(&nwifi_ip(b"some ordinary data")),
        };
        r.down.push_back(Down {
            frame: tmpl,
            mcast: g.below(4) == 0,
            protected: g.below(2) == 0,
            pn: None,
        });
        frames += 1;
        if g.below(10) == 0 {
            let len = g.below(1600);
            let f = g.bytes(len);
            r.wifi.tx_ethernet(&f);
        }
        r.msg1_sent = true; // whatever the station answers to the noise, the AP model reads
        r.tick();
        while r.wifi.pop_rx().is_some() {}
    }
    assert!(!r.wifi.is_dead(), "{}", r.wifi.status());
    // everything from the noise has aged out of the scan table and the station is wherever it landed
    r.down.clear();
    r.wifi.disconnect();
    r.run(31_000);
    r.ap = ModelAp::new(PASSPHRASE);
    r.pn = 1 << 20;
    r.associated = false;
    r.ap_tk = None;
    r.msg1_sent = false;
    r.ap_rx.clear();
    r.reconnect_works(5);
}

fn firmware_noise(r: &mut Rig, g: &mut Rng) {
    match g.below(7) {
        0 => {
            let pipe = [1, 2, 9, 10][g.below(4)];
            let len = g.below(2000);
            let b = g.bytes(len);
            r.fw.queue_raw(pipe, b);
        }
        1 => {
            let len = g.below(500);
            let b = g.bytes(len);
            r.fw.queue_raw(5, b);
        }
        2 => {
            let len = g.below(500);
            let b = g.bytes(len);
            r.fw.queue_htt_ce5(&b);
        }
        3 | 4 => {
            let ids = [
                id::SERVICE_READY_EV,
                id::READY_EV,
                id::SERVICE_AVAILABLE_EV,
                id::SCAN_EV,
                id::VDEV_START_RESP_EV,
                id::VDEV_STOPPED_EV,
                id::PEER_KICKOUT_EV,
                id::PEER_DELETE_RESP_EV,
                id::MGMT_RX_EV,
                id::MGMT_TX_COMPL_EV,
                id::MGMT_TX_BUNDLE_EV,
                id::ROAM_EV,
            ];
            let which = ids[g.below(ids.len())];
            // TLVs with believable tags, whose lengths and values are noise
            let mut b = Vec::new();
            for _ in 0..g.below(5) {
                let len = g.below(80);
                let claimed = if g.below(8) == 0 { g.below(4000) } else { len } as u16;
                b.extend_from_slice(&claimed.to_le_bytes());
                let tag = [
                    16u16, 17, 18, 32, 33, 34, 35, 36, 40, 41, 43, 44, 48, 423, 552, 559,
                ][g.below(16)];
                b.extend_from_slice(&tag.to_le_bytes());
                b.extend(g.bytes(len));
            }
            r.fw.queue_wmi_event(which, &b);
        }
        5 => {
            let kinds = [0u8, 1, 2, 3, 4, 7, 9, 0xb, 0xd, 0xe, 0x12, 0x1a];
            let len = 2 + g.below(120);
            let mut b = g.bytes(len);
            b[0] = kinds[g.below(kinds.len())];
            // an indication that claims more than there is kills the data path, which is allowed, once in a while
            if b[0] == 1 && g.below(8) != 0 {
                b[0] = 7;
            }
            r.fw.queue_htt(&b);
        }
        _ => {
            if r.wifi.is_ready() && !r.wifi.is_dead() {
                let payload = g.bytes(g.0 as usize % 1600);
                let s = RxSpec {
                    payload,
                    attention: model::ATT_DONE
                        | (g.next() as u32 & 0x7fff_ffff & !(1 << 27) & !(1 << 17)),
                    first_msdu: g.below(4) != 0,
                    last_msdu: true,
                    enctype: g.below(11) as u32,
                    decap: g.below(4) as u32,
                    tid: g.below(16) as u32,
                    hdr: g.bytes(64),
                    rssi_comb: g.next() as u8,
                    ppdu_info1: g.next() as u32,
                    ppdu_info2: g.next() as u32,
                    ppdu_info3: g.next() as u32,
                    ..RxSpec::default()
                };
                r.inject_rx(&s);
            }
        }
    }
}

#[test]
fn garbage_from_the_firmware_breaks_nothing() {
    let mut frames = 0;
    let mut survived = 0;
    let mut g = Rng(0xfeed_face_cafe_b0b1);
    for round in 0..12 {
        let mut r = if round % 3 == 0 {
            // during the bring-up too
            Rig::with(ApCfg::default(), Rig::model_cfg())
        } else {
            Rig::up(ApCfg::default())
        };
        if round % 3 == 2 {
            r.connected();
            r.run(20);
        }
        for i in 0..500 {
            firmware_noise(&mut r, &mut g);
            frames += 1;
            if i % 50 == 0 && r.wifi.can_tx() {
                r.wifi.tx_ethernet(&eth(&REMOTE, 0x0800, b"meanwhile"));
            }
            if i == 250 {
                r.wifi.scan();
                r.wifi.connect(SSID, PASSPHRASE, [9; 32]);
            }
            r.tick();
            while r.wifi.pop_rx().is_some() {}
        }
        // whatever state it is in, it keeps answering, and it does not wait forever
        r.run(6000);
        let _ = r.wifi.status();
        r.wifi.disconnect();
        r.run(6000);
        if !r.wifi.is_dead() {
            survived += 1;
        }
    }
    assert!(frames >= 6000);
    assert!(survived > 0, "the noise is not all fatal");
}

/// `(cwmin, cwmax, aifs, txop)` for every AC.
fn all(x: (u32, u32, u32, u32)) -> Vec<(u32, u32, u32, u32)> {
    vec![x; 4]
}

#[test]
fn interface_up_programs_mac80211s_default_edca_one_conf_tx_per_ac() {
    let r = Rig::up(ApCfg::default());
    let sets = r.wmm_sets();
    // ieee80211_set_wmm_default without QoS: aifs 2, cw 15..1023, txop 0; VO first, the set filling up per AC
    let d = (15, 1023, 2, 0);
    let z = (0, 0, 0, 0);
    assert_eq!(
        sets,
        vec![
            vec![z, z, z, d],
            vec![z, z, d, d],
            vec![d, z, d, d],
            all(d)
        ]
    );
    // and per AC the U-APSD parameters (none asked for)
    let ps: Vec<(u32, u32)> = r
        .fw
        .cmds(id::STA_PS_PARAM)
        .iter()
        .map(|c| (c.w(1), c.w(2)))
        .collect();
    assert_eq!(
        ps.iter().filter(|p| **p == (4, 0)).count(),
        4,
        "UAPSD 0 once per AC: {ps:?}"
    );
}

#[test]
fn an_ap_without_wmm_gets_the_qos_defaults_at_association() {
    let mut r = Rig::up(ApCfg {
        ht: false,
        wmm: false,
        ..ApCfg::default()
    });
    r.connected();
    r.run(20);
    let sets = r.wmm_sets();
    assert_eq!(sets.len(), 8, "four at interface up, four at association");
    // ieee80211_set_wmm_default(link, false, true): an 11g BSS, so aCWmin 15; txop in us (units of 32 us times 32)
    assert_eq!(
        sets[7],
        vec![
            (15, 1023, 3, 0),
            (15, 1023, 7, 0),
            (7, 15, 2, 3008),
            (3, 7, 2, 1504)
        ]
    );
    // before peer_assoc, as mac80211 does it in ieee80211_assoc_success
    let last_wmm = r.last(WMM_CONF).unwrap();
    assert!(last_wmm < r.last(id::PEER_ASSOC).unwrap());
}

#[test]
fn the_aps_set_goes_out_at_association_and_defaults_after_disassociation() {
    let mut r = Rig::up(ApCfg::default());
    r.connected();
    r.run(20);
    let sets = r.wmm_sets();
    assert_eq!(sets.len(), 8);
    // wmm_param_ie: BE aifs 3 ecw 4/10, BK 7 4/10, VI 2 3/4 txop 94, VO 2 2/3 txop 47
    assert_eq!(
        sets[7],
        vec![
            (15, 1023, 3, 0),
            (15, 1023, 7, 0),
            (7, 15, 2, 94 * 32),
            (3, 7, 2, 47 * 32)
        ]
    );
    r.wifi.disconnect();
    assert!(r.run_until(200, |r| r.count(id::VDEV_STOP) == 1));
    let sets = r.wmm_sets();
    assert_eq!(sets.len(), 12);
    assert_eq!(sets[11], all((15, 1023, 2, 0)), "ieee80211_set_disassoc's defaults");
    // after vdev_down, before the vdev stops (the channel context is released after)
    let first = r
        .fw
        .wmi_log
        .iter()
        .enumerate()
        .filter(|(_, c)| c.id == WMM_CONF)
        .map(|(i, _)| i)
        .nth(8)
        .unwrap();
    assert!(r.last(id::VDEV_DOWN).unwrap() < first);
    assert!(r.last(WMM_CONF).unwrap() < r.last(id::VDEV_STOP).unwrap());
}
