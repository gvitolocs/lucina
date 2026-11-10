//! One Wi-Fi station on Juno: [`lucina_ath10k::driver::Driver`] below, [`lucina_wifi_mac::Station`] above, and the order
//! between them that `net/mac80211` and `ath10k/mac.c` keep in Linux. `no_std` with `alloc`, no `unsafe`, no I/O beyond the
//! driver's [`Platform`], no clock of its own.
//!
//! The owner calls [`Wifi::poll`] in a loop, and between calls [`Wifi::scan`], [`Wifi::connect`], [`Wifi::disconnect`],
//! [`Wifi::tx_ethernet`] and [`Wifi::pop_rx`]. Outputs of the station run strictly in order, and two of them hold the rest
//! back until the driver has answered: a join (nothing may be sent before the vdev runs and the peer exists) and a key
//! (the port must not open before the key is in, `HTT_T2H_MSG_TYPE_SEC_IND`).
//!
//! `tests/session.rs` runs a whole session against the two crates' models. They are models: nothing here has met a modem,
//! a real access point or the WCN3990's firmware.

#![no_std]
#![forbid(unsafe_code)]

extern crate alloc;

use alloc::{
    collections::VecDeque,
    format,
    string::{String, ToString},
    vec::Vec,
};
use core::fmt::Write;

use lucina_ath10k::{
    driver::{
        Bandwidth, Cipher, Config, Decap, Disconnect, Driver, DriverError, Event, HtInfo,
        JoinParams, KeySpec, PeerAssoc, Platform, RxRate, ScanParams, VhtInfo,
    },
    htt, wmi_tlv as wmi,
};
use lucina_wifi_mac::{
    ChannelWidth, Credentials, GroupCipher, HwCaps, JoinInfo, Output, PeerHt, PeerInfo, PhyMode,
    RxFrame, RxMeta, ScanLeg, Security, Station, StationState,
    mlme::{use_11b, wmm_default},
};

/// `IEEE80211_HT_MAX_AMPDU_FACTOR`.
const HT_MAX_AMPDU_FACTOR: u32 = 13;
/// Frame control's Protected bit.
const FC_PROTECTED: u16 = 0x4000;
/// What mac80211 gives EAPOL on a QoS peer: `skb->priority = 7`.
const EAPOL_TID: u8 = 7;
const BEST_EFFORT_TID: u8 = 0;
/// Frames from the network stack waiting for the driver. Beyond this [`Wifi::can_tx`] says no.
const TX_BACKLOG: usize = 32;
const RX_BACKLOG: usize = 256;

/// `chan_to_phymode` for what the station decided.
fn phymode(mode: PhyMode) -> u8 {
    match mode {
        PhyMode::B11 => wmi::phy_mode::MODE_11B,
        PhyMode::G11 => wmi::phy_mode::MODE_11G,
        PhyMode::A11 => wmi::phy_mode::MODE_11A,
        PhyMode::NgHt20 => wmi::phy_mode::MODE_11NG_HT20,
        PhyMode::NgHt40 => wmi::phy_mode::MODE_11NG_HT40,
        PhyMode::NaHt20 => wmi::phy_mode::MODE_11NA_HT20,
        PhyMode::NaHt40 => wmi::phy_mode::MODE_11NA_HT40,
        PhyMode::AcVht80 => wmi::phy_mode::MODE_11AC_VHT80,
    }
}

fn join_params(j: &JoinInfo) -> JoinParams {
    JoinParams {
        bssid: j.bssid,
        freq: u32::from(j.freq_mhz),
        center_freq1: u32::from(j.center_freq_mhz),
        phymode: phymode(j.phy_mode),
        max_power_dbm: j.max_power_dbm,
        beacon_interval: u32::from(j.beacon_interval_tu),
        dtim_period: u32::from(j.dtim_period),
        short_slot: j.short_slot,
        short_preamble: j.short_preamble,
    }
}

/// The inverse of `ath10k_parse_mpdudensity`, which the driver applies again.
fn density_code(usec: u8) -> u8 {
    match usec {
        0 => 0,
        1 => 3,
        2 => 4,
        4 => 5,
        8 => 6,
        _ => 7,
    }
}

/// The HT values as the driver takes them: the intersection the station computed, in the form of the IE.
fn ht_info(ht: &PeerHt) -> HtInfo {
    // max_mpdu is (1 << (13 + factor)) - 1.
    let bits = 32 - ht.max_mpdu.leading_zeros();
    let mut rx_mask = [0u8; 10];
    for mcs in &ht.mcs {
        if let Some(byte) = rx_mask.get_mut(usize::from(*mcs / 8)) {
            *byte |= 1 << (*mcs % 8);
        }
    }
    HtInfo {
        cap: ht.cap_info,
        ampdu_factor: bits.saturating_sub(HT_MAX_AMPDU_FACTOR).min(3) as u8,
        ampdu_density: density_code(ht.mpdu_density),
        rx_mask,
    }
}

/// `ath10k_conf_tx`, from what `ieee80211_sta_wmm_params` takes out of the AP's parameters: the contention window from its exponent, the
/// TXOP from units of 32 us.
fn wmm_ac(ac: lucina_wifi_mac::ie::AcParams) -> wmi::WmmAc {
    wmi::WmmAc {
        cwmin: (1u32 << ac.ecw_min.min(15)) - 1,
        cwmax: (1u32 << ac.ecw_max.min(15)) - 1,
        aifs: u32::from(ac.aifsn),
        txop: u32::from(ac.txop_limit) * 32,
    }
}

fn peer_assoc(p: &PeerInfo, freq: u32) -> PeerAssoc {
    PeerAssoc {
        bssid: p.addr,
        aid: p.aid,
        capability: p.capability,
        rates: p.legacy_rates.clone(),
        ht: p.ht.as_ref().map(ht_info),
        vht: p.vht.map(|v| VhtInfo {
            cap: v.cap,
            rx_mcs_map: v.rx_mcs_map,
            rx_highest: v.rx_highest,
            tx_mcs_map: v.tx_mcs_map,
            tx_highest: v.tx_highest,
        }),
        bandwidth: if p.vht.is_some() {
            Bandwidth::Bw80
        } else if p.ht.as_ref().is_some_and(|h| h.bw40) {
            Bandwidth::Bw40
        } else {
            Bandwidth::Bw20
        },
        wme: p.qos,
        rsn: p.need_ptk_4way,
        wpa: false,
        freq,
    }
}

/// `tid` as `ath10k_htt_tx_get_tid` gets it: a non-QoS peer has one queue, a QoS peer best effort, EAPOL highest.
fn tx_tid(qos: bool, eapol: bool) -> u8 {
    if !qos {
        htt::ext_tid::NON_QOS_MCAST_BCAST
    } else if eapol {
        EAPOL_TID
    } else {
        BEST_EFFORT_TID
    }
}

/// `ATH10K_SKB_F_NO_HWCRYPT`: EAPOL the station did not mark protected.
fn no_encrypt(frame: &[u8], eapol: bool) -> bool {
    let fc = frame
        .get(..2)
        .map_or(0, |b| u16::from_le_bytes([b[0], b[1]]));
    eapol && fc & FC_PROTECTED == 0
}

fn credentials(secret: &[u8]) -> Result<Credentials, String> {
    if secret.is_empty() {
        return Ok(Credentials::open());
    }
    if secret.len() == 64 && secret.iter().all(u8::is_ascii_hexdigit) {
        return Credentials::psk_hex(secret)
            .map_err(|_| "the PSK is not 64 hex digits".to_string());
    }
    Credentials::passphrase(secret)
        .map_err(|_| "a passphrase is 8 to 63 printable characters".to_string())
}

fn security_name(s: Security) -> &'static str {
    match s {
        Security::Open => "open",
        Security::Wpa2Psk => "wpa2-psk",
        Security::Wpa2PskSha256 => "wpa2-psk-sha256",
        Security::Wpa3Only => "wpa3",
        Security::Other => "other",
    }
}

fn hex(data: &[u8]) -> String {
    let mut out = String::with_capacity(data.len() * 2);
    for b in data {
        let _ = write!(out, "{b:02x}");
    }
    out
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Wait {
    None,
    /// [`Driver::join`] until `VdevStarted` or `JoinFailed`.
    Join,
    /// [`Driver::install_key`] until `KeyInstalled` or `KeyFailed`.
    Key,
}

/// A connection asked for and waiting for its scan. Not `Debug`: it holds the credentials.
struct Pending {
    ssid: Vec<u8>,
    creds: Credentials,
    entropy: [u8; 32],
}

pub struct Wifi<P: Platform> {
    driver: Driver<P>,
    station: Option<Station>,
    mac: [u8; 6],
    now: u64,
    outbox: VecDeque<Output>,
    wait: Wait,
    legs: VecDeque<ScanLeg>,
    ssids: Vec<Vec<u8>>,
    scanning: bool,
    pending: Option<Pending>,
    ssid: Vec<u8>,
    join_freq: u32,
    qos: bool,
    /// While associated: `use_11b` of that association, for mac80211's defaults at disassociation (the channel
    /// context is still assigned when `ieee80211_set_disassoc` sets them).
    assoc_11b: Option<bool>,
    link_up: bool,
    failed: Option<String>,
    fatal: bool,
    /// Frames and bytes through the network stack, rx and tx.
    counters: [u64; 4],
    /// `lucina.wifi.vht=off`: join as HT.
    no_vht: bool,
    no_wmm: bool,
    /// Data frames per rate the air used towards this station, for telling a slow radio from a slow host.
    rx_rates: Vec<(RxRate, u64)>,
    rx: VecDeque<Vec<u8>>,
    log: VecDeque<String>,
}

impl<P: Platform> Wifi<P> {
    pub fn new(plat: P, cfg: Config) -> Result<Self, DriverError> {
        Ok(Self {
            no_vht: !cfg.vht,
            no_wmm: !cfg.wmm,
            driver: Driver::new(plat, cfg)?,
            station: None,
            mac: [0; 6],
            now: 0,
            outbox: VecDeque::new(),
            wait: Wait::None,
            legs: VecDeque::new(),
            ssids: Vec::new(),
            scanning: false,
            pending: None,
            ssid: Vec::new(),
            join_freq: 0,
            qos: false,
            assoc_11b: None,
            link_up: false,
            failed: None,
            fatal: false,
            counters: [0; 4],
            rx_rates: Vec::new(),
            rx: VecDeque::new(),
            log: VecDeque::new(),
        })
    }

    /// `ath10k_core_start` and the rest of the bring-up, which goes on through [`Wifi::poll`].
    pub fn start(&mut self) -> Result<(), DriverError> {
        self.driver.start()
    }

    /// The stack may send, and frames are coming.
    pub fn link_up(&self) -> bool {
        self.link_up
    }

    pub fn is_ready(&self) -> bool {
        self.station.is_some()
    }

    /// The address the firmware gave, once [`Wifi::is_ready`].
    pub fn mac(&self) -> [u8; 6] {
        self.mac
    }

    pub fn can_tx(&self) -> bool {
        self.link_up && self.outbox.len() < TX_BACKLOG && self.driver.can_tx()
    }

    pub fn pop_rx(&mut self) -> Option<Vec<u8>> {
        let frame = self.rx.pop_front()?;
        self.counters[0] += 1;
        self.counters[1] += frame.len() as u64;
        Some(frame)
    }

    /// MSDUs the firmware has not finished with: more work comes without any new frame.
    pub fn tx_pending(&self) -> usize {
        self.driver.tx_pending()
    }

    /// The next line for the kernel log. No line holds a key or a passphrase.
    pub fn pop_log(&mut self) -> Option<String> {
        self.log.pop_front()
    }

    /// One Ethernet frame from the network stack. False if it was not taken.
    pub fn tx_ethernet(&mut self, frame: &[u8]) -> bool {
        // The stack that ignores `can_tx` is held back here: nothing grows without limit, and nothing goes out ahead
        // of the port opening, which the station already knows of while the keys are still on their way.
        if !self.can_tx() {
            return false;
        }
        let Some(station) = self.station.as_mut() else {
            return false;
        };
        if station.tx_ethernet(frame).is_err() {
            return false;
        }
        self.counters[2] += 1;
        self.counters[3] += frame.len() as u64;
        self.pull_outputs();
        true
    }

    /// Scans every channel the regulatory rules allow. The results land in [`Wifi::status`].
    pub fn scan(&mut self) {
        if self.scanning || self.fatal {
            return;
        }
        self.failed = None;
        self.begin_scan(None);
    }

    /// Scans for `ssid` and joins the best BSS. `secret` is a passphrase or 64 hex digits, empty for an open network.
    /// `entropy` is 32 random bytes.
    pub fn connect(&mut self, ssid: &[u8], secret: &[u8], entropy: [u8; 32]) {
        if self.fatal {
            return;
        }
        self.failed = None;
        let creds = match credentials(secret) {
            Ok(creds) => creds,
            Err(why) => {
                self.failed = Some(why);
                return;
            }
        };
        self.pending = Some(Pending {
            ssid: ssid.to_vec(),
            creds,
            entropy,
        });
        if !self.scanning {
            self.begin_scan(Some(ssid.to_vec()));
        }
    }

    pub fn disconnect(&mut self) {
        self.pending = None;
        if let Some(station) = self.station.as_mut() {
            station.disconnect();
        }
        self.pull_outputs();
    }

    fn begin_scan(&mut self, ssid: Option<Vec<u8>>) {
        let now = self.now;
        let Some(station) = self.station.as_mut() else {
            self.failed = Some("the firmware is not ready".to_string());
            self.pending = None;
            return;
        };
        let req = station.start_scan(ssid.as_deref(), now);
        self.ssids = req.ssids;
        self.legs = req.legs.into();
        self.scanning = true;
        self.next_leg();
    }

    /// Each leg is one `ath10k_start_scan`: mac80211 scans a band at a time.
    fn next_leg(&mut self) {
        let Some(leg) = self.legs.pop_front() else {
            self.scan_finished();
            return;
        };
        let params = ScanParams {
            ssids: self.ssids.clone(),
            channels: leg.channels.iter().map(|c| c.freq_mhz).collect(),
            ie: leg.ies,
        };
        if let Err(err) = self.driver.start_scan(&params) {
            self.log.push_back(format!("scan: {err:?}"));
            self.next_leg();
        }
    }

    fn scan_finished(&mut self) {
        self.scanning = false;
        let now = self.now;
        let Some(station) = self.station.as_mut() else {
            return;
        };
        station.scan_done();
        if let Some(p) = self.pending.take() {
            self.ssid = p.ssid.clone();
            if let Err(err) = station.connect(&p.ssid, p.creds, p.entropy, now) {
                self.failed = Some(format!("connect: {err:?}"));
            }
        }
    }

    /// One pass: the driver's events, the station's clock, the outputs. Call often: the driver polls here.
    /// When [`Wifi::poll`] must run again by itself: the driver's timers, and the station's while no join is on its way (its
    /// clock stops then, as in `poll`). `None`: nothing until the device or a command moves it.
    pub fn next_deadline(&self) -> Option<u64> {
        let joining = self.wait == Wait::Join || matches!(self.outbox.front(), Some(Output::JoinBss(_)));
        let station = if joining {
            None
        } else {
            self.station.as_ref().and_then(|s| s.next_deadline())
        };
        match (self.driver.next_deadline(), station) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, None) => a,
            (None, b) => b,
        }
    }

    /// Whether the last [`Wifi::poll`] left work the next one does at once (see `Driver::has_work`).
    pub fn has_work(&self) -> bool {
        self.driver.has_work()
    }

    /// The copy engines whose interrupt came (bit per engine): acknowledged before their lines are unmasked.
    pub fn ack_interrupts(&mut self, mask: u32) {
        self.driver.ack_interrupts(mask);
    }

    pub fn poll(&mut self, now_ms: u64) {
        self.now = now_ms;
        for event in self.driver.poll(now_ms) {
            self.on_event(event);
        }
        // The station's clock stops while a join is on its way: see `Station::on_joined`.
        let joining =
            self.wait == Wait::Join || matches!(self.outbox.front(), Some(Output::JoinBss(_)));
        if !joining && let Some(station) = self.station.as_mut() {
            station.tick(now_ms);
        }
        self.pull_outputs();
    }

    fn on_event(&mut self, event: Event) {
        let now = self.now;
        match event {
            Event::Ready { mac, caps } => {
                self.mac = mac;
                let chains = caps.num_rf_chains.clamp(1, 8) as u8;
                let mask = ((1u16 << chains) - 1) as u8;
                self.log.push_back(format!(
                    "firmware ready: address {}, {} chains, HT caps {:#x}, regulatory {:#x}",
                    hex(&mac),
                    chains,
                    caps.ht_cap_info,
                    caps.regdomain
                ));
                let mut hw = HwCaps::from_wmi(caps.ht_cap_info, caps.vht_cap_info, chains, mask, mask);
                if self.no_vht {
                    hw.vht = None;
                }
                self.station = Some(Station::new(hw, mac));
                // ieee80211_do_open after drv_add_interface: ieee80211_set_wmm_default without QoS and with no
                // channel context (aifs 2, cw 15..1023, txop 0 for every AC)
                if !self.no_wmm
                    && let Err(err) = self.driver.set_wmm(&wmm_default(false, false).map(wmm_ac))
                {
                    self.log.push_back(format!("wmm: {err:?}"));
                }
            }
            Event::MgmtRx { frame, rssi, freq } => {
                if let Some(s) = self.station.as_mut() {
                    s.on_mgmt_rx(&frame, rssi.clamp(-128, 127) as i8, freq as u16, now);
                }
            }
            Event::ScanDone { aborted } => {
                if aborted {
                    self.log.push_back("scan aborted".to_string());
                }
                if self.scanning {
                    self.next_leg();
                }
            }
            Event::VdevStarted => {
                if self.wait == Wait::Join {
                    self.wait = Wait::None;
                    if let Some(s) = self.station.as_mut() {
                        s.on_joined(now);
                    }
                }
            }
            Event::JoinFailed => {
                self.log
                    .push_back("the vdev did not start or its peer did not appear".to_string());
                self.wait = Wait::None;
                self.fail_link("the join failed");
            }
            Event::PeerAssocDone => {}
            Event::KeyInstalled { .. } => {
                if self.wait == Wait::Key {
                    self.wait = Wait::None;
                }
            }
            Event::KeyFailed { pairwise } => {
                self.wait = Wait::None;
                self.fail_link(if pairwise {
                    "the pairwise key did not go in"
                } else {
                    "the group key did not go in"
                });
            }
            Event::MgmtTxDone { ok } => {
                if let Some(s) = self.station.as_mut() {
                    s.on_mgmt_tx_status(ok, now);
                }
            }
            Event::TxFree { .. } => {}
            Event::DataRx(frame) => {
                self.note_rx_rate(frame.rate);
                let meta = RxMeta {
                    decrypted: frame.decrypted,
                };
                let form = match frame.decap {
                    Decap::NativeWifi => Some(RxFrame::NativeWifi(&frame.frame)),
                    Decap::Raw => Some(RxFrame::Raw80211 {
                        frame: &frame.frame,
                        crypto_header: 0,
                    }),
                    Decap::Ethernet2 | Decap::Snap8023 => None,
                };
                if let (Some(s), Some(form)) = (self.station.as_mut(), form) {
                    s.on_data_rx(form, meta);
                }
            }
            Event::BeaconLoss => {
                if let Some(s) = self.station.as_mut() {
                    s.on_beacon_loss(now);
                }
            }
            Event::Disconnected(Disconnect::Kickout) => {
                // Linux does not drop the link on this: `ath10k_wmi_event_peer_sta_kickout` is
                // `ieee80211_report_low_ack`, a CQM event wpa_supplicant ignores on a station. Frames went unacked, so
                // ask the AP: the probes end the link if it is really gone.
                self.log
                    .push_back("the firmware kicked the AP out: probing it".to_string());
                if let Some(s) = self.station.as_mut() {
                    s.on_beacon_loss(now);
                }
            }
            Event::Disconnected(Disconnect::Left) => {}
            Event::FwError(fault) => {
                self.fatal = true;
                self.link_up = false;
                self.failed = Some(format!("firmware: {fault:?}"));
                self.log.push_back(format!("firmware fault: {fault:?}"));
                // Nothing the firmware owed an answer to will get one.
                self.wait = Wait::None;
                self.outbox.clear();
                self.scanning = false;
                self.legs.clear();
                self.pending = None;
                if let Some(s) = self.station.as_mut() {
                    s.scan_done();
                    s.on_connection_lost();
                }
            }
        }
    }

    /// The driver cannot carry the link any further. What was waiting behind the failed step is for a link that will
    /// not be: an authentication after a join that did not happen, or the rest of the keys and the port opening after a
    /// key that did not go in. A leave already on its way stays, and with it the reason it gave: the first cause is the
    /// one reported, not what followed from it.
    fn fail_link(&mut self, why: &str) {
        let leaving = self.outbox.iter().any(|o| matches!(o, Output::LeaveBss));
        if !leaving && self.failed.is_none() {
            self.failed = Some(why.to_string());
        }
        self.outbox
            .retain(|o| matches!(o, Output::LeaveBss | Output::Disconnected { .. }));
        if let Some(s) = self.station.as_mut() {
            s.on_connection_lost();
        }
    }

    fn pull_outputs(&mut self) {
        if let Some(station) = self.station.as_mut() {
            while let Some(output) = station.poll_output() {
                match output {
                    // Receive does not wait for transmit: nothing ahead of it in the queue is what it depends on, and
                    // the station only lets frames through once the port is open.
                    Output::RxEthernet(frame) => {
                        if self.rx.len() < RX_BACKLOG {
                            self.rx.push_back(frame);
                        }
                    }
                    // What has not gone out yet is for a link that is ending, and a full ring must not hold that up:
                    // mac80211 flushes or drops on `ieee80211_set_disassoc`.
                    Output::LeaveBss => {
                        self.outbox.retain(|o| !matches!(o, Output::TxData { .. }));
                        self.outbox.push_back(Output::LeaveBss);
                    }
                    other => self.outbox.push_back(other),
                }
            }
        }
        while self.wait == Wait::None
            && let Some(output) = self.outbox.pop_front()
        {
            if let Some(again) = self.act(output) {
                self.outbox.push_front(again);
                break;
            }
        }
    }

    /// Does one output. Gives it back if the driver is busy now.
    fn act(&mut self, output: Output) -> Option<Output> {
        match output {
            // A full queue drops the frame (`ath10k_mac_tx_wmi_mgmt`: "wmi mgmt tx queue is full"), and the station's
            // timers do what they do about a frame that got no answer. Holding it would stop everything behind it for
            // as long as the firmware owes completions.
            Output::TxMgmt(frame) => {
                if let Err(err) = self.driver.tx_mgmt(&frame) {
                    self.log.push_back(format!("management frame: {err:?}"));
                }
            }
            Output::TxData { frame, eapol } => {
                let sent =
                    self.driver
                        .tx_data(&frame, tx_tid(self.qos, eapol), no_encrypt(&frame, eapol));
                match sent {
                    Err(DriverError::Busy) => return Some(Output::TxData { frame, eapol }),
                    Err(err) => self.log.push_back(format!("data frame: {err:?}")),
                    Ok(_) => {}
                }
            }
            Output::JoinBss(info) => {
                self.join_freq = u32::from(info.freq_mhz);
                let width = match info.width {
                    ChannelWidth::NoHt20 => "20 MHz, no HT",
                    ChannelWidth::Ht20 => "HT20",
                    ChannelWidth::Ht40 => "HT40",
                    ChannelWidth::Vht80 => "VHT80",
                };
                self.log.push_back(format!(
                    "joining {} on {} MHz, {}",
                    hex(&info.bssid),
                    info.freq_mhz,
                    width
                ));
                match self.driver.join(&join_params(&info)) {
                    Err(DriverError::Busy) => return Some(Output::JoinBss(info)),
                    Err(err) => self.fail_link(&format!("join: {err:?}")),
                    Ok(()) => self.wait = Wait::Join,
                }
            }
            Output::PeerAssoc(info) => {
                self.qos = info.qos;
                self.log.push_back(format!(
                    "associated: AID {}, {} rates, HT {}, QoS {}",
                    info.aid,
                    info.legacy_rates.len(),
                    info.ht.is_some(),
                    info.qos
                ));
                // ieee80211_assoc_success: the AP's checked WMM parameters, or, when it sent none or an invalid set,
                // ieee80211_set_wmm_default with QoS on ("we might have HT/VHT")
                let b11 = use_11b(self.join_freq < 3000, &info.legacy_rates);
                if !self.no_wmm {
                    let acs = info
                        .wmm_params
                        .unwrap_or_else(|| wmm_default(true, b11))
                        .map(wmm_ac);
                    match self.driver.set_wmm(&acs) {
                        Err(DriverError::Busy) => return Some(Output::PeerAssoc(info)),
                        Err(err) => self.fail_link(&format!("wmm: {err:?}")),
                        Ok(()) => {}
                    }
                }
                self.assoc_11b = Some(b11);
                match self.driver.peer_assoc(&peer_assoc(&info, self.join_freq)) {
                    Err(DriverError::Busy) => return Some(Output::PeerAssoc(info)),
                    Err(err) => self.fail_link(&format!("peer assoc: {err:?}")),
                    Ok(()) => {
                        if let Err(err) = self.driver.up(info.aid, &info.addr) {
                            self.fail_link(&format!("vdev up: {err:?}"));
                        }
                    }
                }
            }
            Output::InstallPairwise { peer, tk } => {
                let spec = KeySpec {
                    pairwise: true,
                    key_idx: 0,
                    cipher: Cipher::Ccmp,
                    key: tk.expose().to_vec(),
                };
                match self.driver.install_key(&spec) {
                    Err(DriverError::Busy) => return Some(Output::InstallPairwise { peer, tk }),
                    Err(err) => self.fail_link(&format!("pairwise key: {err:?}")),
                    Ok(()) => self.wait = Wait::Key,
                }
            }
            Output::InstallGroup {
                idx,
                key,
                cipher,
                rsc,
            } => {
                let spec = KeySpec {
                    pairwise: false,
                    key_idx: idx,
                    cipher: match cipher {
                        GroupCipher::Ccmp => Cipher::Ccmp,
                        GroupCipher::Tkip => Cipher::Tkip,
                    },
                    key: key.expose().to_vec(),
                };
                match self.driver.install_key(&spec) {
                    Err(DriverError::Busy) => {
                        return Some(Output::InstallGroup {
                            idx,
                            key,
                            cipher,
                            rsc,
                        });
                    }
                    Err(err) => self.fail_link(&format!("group key: {err:?}")),
                    Ok(()) => self.wait = Wait::Key,
                }
            }
            Output::PortAuthorized => {
                self.link_up = true;
                self.log.push_back("the port is open".to_string());
            }
            // `pull_outputs` takes these before they get here.
            Output::RxEthernet(_) => {}
            Output::MicFailureReported { group } => {
                self.log
                    .push_back(format!("Michael MIC failure reported, group {group}"));
            }
            Output::SetWmm(params) => {
                if !self.no_wmm
                    && let Err(err) = self.driver.set_wmm(&params.map(wmm_ac))
                {
                    self.log.push_back(format!("wmm: {err:?}"));
                }
            }
            Output::LeaveBss => {
                self.link_up = false;
                if let Err(err) = self.driver.leave() {
                    self.log.push_back(format!("leave: {err:?}"));
                }
                // ieee80211_set_disassoc: the defaults without QoS; the driver holds them until vdev_down
                if let Some(b11) = self.assoc_11b.take()
                    && !self.no_wmm
                    && let Err(err) = self.driver.set_wmm(&wmm_default(false, b11).map(wmm_ac))
                {
                    self.log.push_back(format!("wmm: {err:?}"));
                }
            }
            Output::Disconnected { reason } => {
                self.link_up = false;
                self.log.push_back(format!("disconnected: {reason:?}"));
                // The first cause stays: what `fail_link` said beats the `BeaconLost` it made the station report.
                if !matches!(reason, lucina_wifi_mac::DisconnectReason::Requested)
                    && self.failed.is_none()
                {
                    self.failed = Some(format!("{reason:?}"));
                }
            }
        }
        None
    }

    /// The text `/dev/lucina-wifi` reads, one `key value` per line.
    pub fn status(&self) -> String {
        let mut out = String::new();
        let state = match self.station.as_ref().map(Station::state) {
            None => "starting",
            Some(_) if self.scanning => "scanning",
            Some(StationState::Idle) if self.pending.is_some() => "scanning",
            Some(StationState::Idle) => "idle",
            Some(StationState::Scanning) => "scanning",
            Some(
                StationState::Authenticating
                | StationState::Associating
                | StationState::Handshaking,
            ) => "connecting",
            Some(StationState::Connected) => "connected",
        };
        let _ = writeln!(out, "state {state}");
        let _ = writeln!(out, "mac {}", hex(&self.mac));
        let _ = writeln!(out, "rx {} frames {} bytes", self.counters[0], self.counters[1]);
        let _ = writeln!(out, "tx {} frames {} bytes", self.counters[2], self.counters[3]);
        if let Some(why) = &self.failed {
            let _ = writeln!(out, "failed {why}");
        }
        if !self.ssid.is_empty() {
            let _ = writeln!(out, "ssid {}", hex(&self.ssid));
        }
        if let Some(station) = self.station.as_ref() {
            for bss in station.scan_results() {
                let _ = writeln!(
                    out,
                    "bss {} {} {} {} {}",
                    hex(&bss.bssid),
                    bss.freq_mhz,
                    bss.rssi_dbm,
                    security_name(bss.security),
                    hex(&bss.ssid),
                );
            }
        }
        out
    }

    /// Whether the firmware has gone away and only a restart of the modem can help.
    pub fn is_dead(&self) -> bool {
        self.fatal
    }

    /// A scan is running or waiting.
    pub fn is_scanning(&self) -> bool {
        self.scanning
    }

    /// One readable line per network heard. Network names are public; they may hold any bytes.
    pub fn scan_lines(&self) -> Vec<String> {
        let Some(station) = self.station.as_ref() else {
            return Vec::new();
        };
        station
            .scan_results()
            .iter()
            .map(|bss| {
                let name: String = String::from_utf8_lossy(&bss.ssid)
                    .chars()
                    .map(|c| if c.is_control() { '?' } else { c })
                    .collect();
                format!(
                    "{} {} MHz {} dBm {} \"{}\"",
                    hex(&bss.bssid),
                    bss.freq_mhz,
                    bss.rssi_dbm,
                    security_name(bss.security),
                    name
                )
            })
            .collect()
    }

    /// For a log: how the driver is doing.
    pub fn health(&self) -> String {
        format!(
            "tx pending {}, rx fill {}, backlog {}, {:?}, rx rates {:?}",
            self.driver.tx_pending(),
            self.driver.rx_fill(),
            self.outbox.len(),
            self.driver.stats(),
            self.rx_rates
        )
    }

    fn note_rx_rate(&mut self, rate: RxRate) {
        const MOST_RATES: usize = 32;
        if let Some((_, count)) = self.rx_rates.iter_mut().find(|(known, _)| *known == rate) {
            *count += 1;
        } else if self.rx_rates.len() < MOST_RATES {
            self.rx_rates.push((rate, 1));
        }
    }

    /// See `Driver::ce_report`.
    pub fn ce_report(&mut self) -> Vec<String> {
        self.driver.ce_report()
    }

    /// The platform, for a look.
    pub fn platform(&self) -> &P {
        self.driver.platform()
    }

    /// The platform, for the owner's teardown.
    pub fn platform_mut(&mut self) -> &mut P {
        self.driver.platform_mut()
    }
}

#[cfg(test)]
mod tests {
    extern crate std;

    use alloc::vec;

    use lucina_wifi_mac::caps::PeerHt;

    use super::*;

    fn peer_ht() -> PeerHt {
        PeerHt {
            cap_info: 0x1234,
            max_mpdu: (1 << 15) - 1,
            mpdu_density: 8,
            mcs: vec![0, 1, 7, 8, 15],
            ldpc: false,
            bw40: true,
            stbc: false,
            rate_caps: 0,
            num_spatial_streams: 2,
        }
    }

    // Whatever the driver computes from HtInfo must come back to what the station intersected.
    #[test]
    fn ht_values_survive_the_way_through_the_ie_form() {
        let info = ht_info(&peer_ht());
        assert_eq!(info.cap, 0x1234);
        assert_eq!(
            (1u32 << (13 + u32::from(info.ampdu_factor))) - 1,
            (1 << 15) - 1
        );
        assert_eq!(info.ampdu_density, 6);
        assert_eq!(info.rx_mask[0], 0b1000_0011);
        assert_eq!(info.rx_mask[1], 0b1000_0001);
        assert_eq!(info.rx_mask[2], 0);
    }

    #[test]
    fn every_density_maps_back() {
        for (usec, code) in [(0, 0), (1, 3), (2, 4), (4, 5), (8, 6), (16, 7)] {
            assert_eq!(density_code(usec), code);
        }
    }

    #[test]
    fn eapol_goes_clear_only_when_not_marked_protected_and_at_the_right_tid() {
        assert!(no_encrypt(&[0x08, 0x01], true));
        assert!(!no_encrypt(&[0x08, 0x41], true));
        assert!(!no_encrypt(&[0x08, 0x01], false));
        assert_eq!(tx_tid(true, true), 7);
        assert_eq!(tx_tid(true, false), 0);
        assert_eq!(tx_tid(false, true), htt::ext_tid::NON_QOS_MCAST_BCAST);
    }

    #[test]
    fn credentials_tell_passphrase_from_psk() {
        assert!(credentials(b"short").is_err());
        assert!(credentials(b"long enough passphrase").is_ok());
        assert!(credentials([b'a'; 64].as_slice()).is_ok());
        assert!(credentials(b"").is_ok());
    }
}
