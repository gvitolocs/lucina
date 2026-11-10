//! The top: one station on one network. Feed it frames and time, drain its outputs in order.

use crate::caps::HwCaps;
use crate::crypto::secret::{SecretBytes, SecretVec, wipe};
use crate::data::{
    DataHeader, ethernet_to_nwifi, ethertype, nwifi_to_ethernet, raw80211_to_ethernet,
};
use crate::frame::{
    AuthBody, BROADCAST, ETH_HLEN, ETH_P_EAPOL, FCTL_FROMDS, FCTL_TODS, MacAddr, MgmtFrame,
    REASON_DEAUTH_LEAVING, REASON_MICHAEL_MIC_FAILURE, STATUS_ASSOC_REJECTED_TEMPORARILY,
    STATUS_SUCCESS, STYPE_ASSOC_RESP, STYPE_AUTH, STYPE_BEACON, STYPE_DEAUTH, STYPE_DISASSOC,
    STYPE_PROBE_RESP, STYPE_REASSOC_RESP, WLAN_AUTH_OPEN, build_auth, build_deauth, is_multicast,
    le16_at, mac_at, set_sequence,
};
use crate::ie::{AcParams, EID_TIMEOUT_INTERVAL, build_rsn_ie, find, parse_wmm};
use crate::mlme::{
    ASSOC_MAX_TRIES, ASSOC_TIMEOUT_LONG_MS, ASSOC_TIMEOUT_MS, ASSOC_TIMEOUT_SHORT_MS,
    AUTH_MAX_TRIES, AUTH_TIMEOUT_LONG_MS, AUTH_TIMEOUT_MS, AUTH_TIMEOUT_SHORT_MS, AssocRespBody,
    BEACON_LOSS_COUNT, ConnPlan, HANDSHAKE_TIMEOUT_MS, JoinInfo, MAX_PROBE_TRIES,
    MIC_FAILURE_WINDOW_MS, PROBE_WAIT_MS, PeerInfo, build_assoc_req, build_probe_req, join_info,
    peer_info, plan_connection, validate_wmm,
};
use crate::regulatory::{Band, ChannelState, Regulatory};
use crate::scan::{ScanRequest, ScanResult, ScanTable, Security, build_scan_request};
use crate::wpa::{Akm, GroupCipher, Supplicant, SupplicantConfig, WpaEvent, derive_psk};
use alloc::collections::VecDeque;
use alloc::vec::Vec;
use core::fmt;

/// The PAE group address EAPOL can be sent to.
const PAE_GROUP: MacAddr = [0x01, 0x80, 0xc2, 0x00, 0x00, 0x03];

/// Bound on queued outputs that the owner has not drained. Beyond this, received data is dropped, never control.
const RX_QUEUE_LIMIT: usize = 512;

/// How to prove membership of the network.
pub struct Credentials(CredentialsInner);

enum CredentialsInner {
    Open,
    Passphrase(SecretVec),
    Psk(SecretBytes<32>),
}

/// Why a passphrase or PSK string was not accepted. It does not repeat the string.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CredentialsError {
    /// A passphrase is 8 to 63 printable ASCII characters.
    BadPassphrase,
    /// A hex PSK is exactly 64 hex digits.
    BadHexPsk,
}

impl Credentials {
    pub fn open() -> Self {
        Self(CredentialsInner::Open)
    }

    /// A WPA2 passphrase: 8 to 63 printable ASCII characters, as `wpa_supplicant` requires.
    pub fn passphrase(text: &[u8]) -> Result<Self, CredentialsError> {
        if !(8..=63).contains(&text.len()) || text.iter().any(|c| !(32..=126).contains(c)) {
            return Err(CredentialsError::BadPassphrase);
        }
        Ok(Self(CredentialsInner::Passphrase(SecretVec::from_slice(
            text,
        ))))
    }

    /// The PSK as 64 hex digits.
    pub fn psk_hex(text: &[u8]) -> Result<Self, CredentialsError> {
        if text.len() != 64 {
            return Err(CredentialsError::BadHexPsk);
        }
        let mut psk = SecretBytes::<32>::zero();
        for (out, pair) in psk.expose_mut().iter_mut().zip(text.as_chunks::<2>().0) {
            let hi = hex_digit(pair[0]).ok_or(CredentialsError::BadHexPsk)?;
            let lo = hex_digit(pair[1]).ok_or(CredentialsError::BadHexPsk)?;
            *out = (hi << 4) | lo;
        }
        Ok(Self(CredentialsInner::Psk(psk)))
    }

    /// The PSK as raw bytes.
    pub fn psk(bytes: [u8; 32]) -> Self {
        Self(CredentialsInner::Psk(SecretBytes::new(bytes)))
    }

    fn is_open(&self) -> bool {
        matches!(self.0, CredentialsInner::Open)
    }

    fn pmk(&self, ssid: &[u8]) -> Option<SecretBytes<32>> {
        match &self.0 {
            CredentialsInner::Open => None,
            CredentialsInner::Passphrase(p) => Some(derive_psk(p.expose(), ssid)),
            CredentialsInner::Psk(k) => Some(k.clone()),
        }
    }
}

fn hex_digit(c: u8) -> Option<u8> {
    match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'a'..=b'f' => Some(c - b'a' + 10),
        b'A'..=b'F' => Some(c - b'A' + 10),
        _ => None,
    }
}

impl fmt::Debug for Credentials {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(if self.is_open() {
            "Credentials(open)"
        } else {
            "Credentials(<secret>)"
        })
    }
}

/// Why `connect` could not start.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConnectError {
    /// No BSS with this SSID in a fresh scan.
    NoSuchNetwork,
    /// The SSID is longer than 32 bytes.
    BadSsid,
    /// Open credentials for a secured BSS, or a passphrase for an open one.
    SecurityMismatch(Security),
    /// The AP sets MFPR: it requires PMF, which this station does not do.
    PmfRequired,
    /// The AP offers only SAE.
    Wpa3Only,
    /// WEP, WPA1, 802.1X or an unusable cipher.
    UnsupportedSecurity,
    /// The BSS is on a channel the regulatory state or the hardware rules out.
    ChannelNotAllowed,
    /// The AP's rates and ours do not meet.
    NoCommonRates,
    /// Two Michael MIC failures: joining stays blocked for 60 s.
    Countermeasures,
}

/// Why the association ended. Each comes with [`Output::Disconnected`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DisconnectReason {
    /// [`Station::disconnect`] was called.
    Requested,
    AuthTimeout,
    AuthRejected {
        status: u16,
    },
    AssocTimeout,
    AssocRejected {
        status: u16,
    },
    /// The AP deauthenticated us. During the handshake this usually means a wrong passphrase, as wpa_supplicant
    /// reports it ("4-Way Handshake failed - pre-shared key may be incorrect").
    Deauthenticated {
        reason: u16,
        during_handshake: bool,
    },
    Disassociated {
        reason: u16,
        during_handshake: bool,
    },
    /// No acceptable message 3 within 10 s.
    HandshakeTimeout,
    /// Message 3 was authentic but unusable: its RSN element differs from the beacon's (reason 17), or similar.
    HandshakeFailed {
        reason: u16,
    },
    /// Beacons stopped and probes went unanswered, or the firmware said so.
    BeaconLost,
    /// Second Michael MIC failure in 60 s.
    MicCountermeasures,
}

/// One thing for the owner to do. Drain [`Station::poll_output`] after every call and act in order: a key may not be
/// installed before an earlier frame has been handed to the hardware.
#[derive(Debug)]
pub enum Output {
    /// A management frame to send over WMI or HTT, sequence number already in place.
    TxMgmt(Vec<u8>),
    /// A native-WiFi data frame. `eapol` frames must be sent even though no key is in place yet; the Protected bit
    /// in the frame says whether firmware should encrypt.
    TxData {
        frame: Vec<u8>,
        eapol: bool,
    },
    /// Start the vdev on this channel and create the AP's peer. Comes before the first [`Output::TxMgmt`].
    JoinBss(JoinInfo),
    /// Association succeeded: send peer_assoc, then `vdev_up` with the AID.
    PeerAssoc(PeerInfo),
    /// The AP's WMM parameter set changed in a beacon (its parameter set count moved): send it again, BE, BK, VI, VO.
    SetWmm([AcParams; 4]),
    /// Pairwise CCMP key for the AP's peer, key index 0, `WMI_KEY_PAIRWISE`.
    InstallPairwise {
        peer: MacAddr,
        tk: SecretBytes<16>,
    },
    /// Group key: `WMI_KEY_GROUP`. For TKIP, `key` has its Michael keys in the order a station's driver expects.
    InstallGroup {
        idx: u8,
        key: SecretVec,
        cipher: GroupCipher,
        rsc: [u8; 6],
    },
    /// The network stack may send and will receive from here on.
    PortAuthorized,
    /// An Ethernet II frame for the network stack.
    RxEthernet(Vec<u8>),
    /// A Michael MIC failure was reported to the AP.
    MicFailureReported {
        group: bool,
    },
    /// Delete the peer and stop the vdev. Any key dies with them. Comes before [`Output::Disconnected`].
    LeaveBss,
    Disconnected {
        reason: DisconnectReason,
    },
}

/// Where the station is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StationState {
    Idle,
    Scanning,
    Authenticating,
    Associating,
    /// Associated, waiting for the 4-way handshake to finish.
    Handshaking,
    /// Associated with the port open.
    Connected,
}

/// Why [`Station::tx_ethernet`] refused a frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TxError {
    /// Not associated, or the 4-way has not finished.
    PortClosed,
    /// Shorter than an Ethernet header, or an EAPOL frame: the supplicant owns that ethertype.
    BadFrame,
}

/// A received data frame, in whichever form the driver has it.
#[derive(Clone, Copy, Debug)]
pub enum RxFrame<'a> {
    /// ath10k's native-WiFi decap: 3-address header, no QoS, LLC/SNAP.
    NativeWifi(&'a [u8]),
    /// Raw 802.11 with QoS and possibly an A-MSDU. `crypto_header` is the IV length if still present. MIC and FCS gone.
    Raw80211 {
        frame: &'a [u8],
        crypto_header: usize,
    },
    /// Already Ethernet II: ath10k's Ethernet decap.
    Ethernet(&'a [u8]),
}

/// What the hardware says about a received frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RxMeta {
    /// The hardware decrypted it and the MIC checked. A frame without this is dropped once a key is in, EAPOL aside
    /// (`ieee80211_drop_unencrypted`).
    pub decrypted: bool,
}

/// What was dropped, for debugging. Never contains frame contents.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Stats {
    pub rx_port_closed: u64,
    pub rx_unprotected: u64,
    pub rx_foreign: u64,
    pub rx_malformed: u64,
    pub rx_queue_full: u64,
    pub eapol_rx: u64,
    pub tx_data: u64,
}

enum Phase {
    Auth {
        tries: u8,
        deadline: u64,
    },
    Assoc {
        tries: u8,
        deadline: u64,
        comeback: bool,
    },
    Associated {
        handshake_deadline: Option<u64>,
    },
}

struct Conn {
    bss: ScanResult,
    plan: ConnPlan,
    phase: Phase,
    rsn_ie: Option<Vec<u8>>,
    supplicant: Option<Supplicant>,
    port_open: bool,
    pending_eapol: Option<Vec<u8>>,
    last_beacon: u64,
    probe_tries: u8,
    next_probe: u64,
    /// The firmware reported a beacon miss: probe without waiting for the silence timer.
    beacon_lost: bool,
    /// `wmm_last_param_set` while beacons are followed for WMM changes; `None` before the association and when
    /// mac80211 would have `disable_wmm_tracking` (the association carried no valid set).
    wmm_last_param_set: Option<u8>,
}

/// One 802.11 station on WPA2-PSK. See the crate documentation.
pub struct Station {
    hw: HwCaps,
    mac: MacAddr,
    reg: Regulatory,
    table: ScanTable,
    scanning: bool,
    conn: Option<Conn>,
    out: VecDeque<Output>,
    seq: u16,
    stats: Stats,
    last_mic_failure: Option<u64>,
    blocked_until: u64,
    fixed_snonce: Option<[u8; 32]>,
}

impl fmt::Debug for Station {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Station")
            .field("mac", &self.mac)
            .field("state", &self.state())
            .field("stats", &self.stats)
            .finish_non_exhaustive()
    }
}

impl Station {
    pub fn new(hw: HwCaps, mac: MacAddr) -> Self {
        Self {
            hw,
            mac,
            reg: Regulatory::new(),
            table: ScanTable::new(),
            scanning: false,
            conn: None,
            out: VecDeque::new(),
            seq: 0,
            stats: Stats::default(),
            last_mic_failure: None,
            blocked_until: 0,
            fixed_snonce: None,
        }
    }

    /// Makes the first SNonce of the next connection this value: to replay a capture in tests, never for anything else.
    #[doc(hidden)]
    pub fn set_fixed_snonce(&mut self, snonce: [u8; 32]) {
        self.fixed_snonce = Some(snonce);
    }

    pub fn mac(&self) -> &MacAddr {
        &self.mac
    }

    pub fn state(&self) -> StationState {
        match &self.conn {
            Some(c) => match &c.phase {
                Phase::Auth { .. } => StationState::Authenticating,
                Phase::Assoc { .. } => StationState::Associating,
                Phase::Associated { .. } if c.port_open => StationState::Connected,
                Phase::Associated { .. } => StationState::Handshaking,
            },
            None if self.scanning => StationState::Scanning,
            None => StationState::Idle,
        }
    }

    pub fn stats(&self) -> Stats {
        self.stats
    }

    /// The channel list for the firmware (`ath10k_update_channel_list`), as it stands now.
    pub fn channels(&self) -> Vec<ChannelState> {
        self.reg
            .channels()
            .into_iter()
            .filter(|c| match c.band {
                Band::Ghz2 => self.hw.band_2ghz,
                Band::Ghz5 => self.hw.band_5ghz,
            })
            .collect()
    }

    pub fn regulatory(&self) -> &Regulatory {
        &self.reg
    }

    /// The next thing to do, or `None`.
    pub fn poll_output(&mut self) -> Option<Output> {
        self.out.pop_front()
    }

    /// Starts a scan. Results collect through [`Station::on_mgmt_rx`]. `ssid_filter` becomes the probe SSID (hidden
    /// networks need it); without it probes use the wildcard.
    pub fn start_scan(&mut self, ssid_filter: Option<&[u8]>, now_ms: u64) -> ScanRequest {
        self.table.expire(now_ms);
        self.scanning = true;
        build_scan_request(ssid_filter, &self.hw, &self.reg)
    }

    /// The driver finished every leg of the scan.
    pub fn scan_done(&mut self) {
        self.scanning = false;
    }

    pub fn scan_results(&self) -> &[ScanResult] {
        self.table.entries()
    }

    fn band_on(&self, band: Band) -> bool {
        match band {
            Band::Ghz2 => self.hw.band_2ghz,
            Band::Ghz5 => self.hw.band_5ghz,
        }
    }

    /// Whether this station could join this BSS with these credentials, and the AKM it would use.
    fn usable(&self, bss: &ScanResult, creds: &Credentials) -> Result<Option<Akm>, ConnectError> {
        let akm = match (bss.security, creds.is_open()) {
            (Security::Open, true) => None,
            (Security::Open, false) => return Err(ConnectError::SecurityMismatch(Security::Open)),
            (Security::Wpa3Only, false) => return Err(ConnectError::Wpa3Only),
            (Security::Other, false) => return Err(ConnectError::UnsupportedSecurity),
            (s, true) => return Err(ConnectError::SecurityMismatch(s)),
            (Security::Wpa2Psk, false) => Some(Akm::Psk),
            (Security::Wpa2PskSha256, false) => Some(Akm::PskSha256),
        };
        if bss.pmf_required() {
            return Err(ConnectError::PmfRequired);
        }
        if !self.band_on(bss.band) || self.reg.channel(bss.freq_mhz).is_none() {
            return Err(ConnectError::ChannelNotAllowed);
        }
        if plan_connection(bss, &self.hw, &self.reg).is_none() {
            return Err(ConnectError::NoCommonRates);
        }
        Ok(akm)
    }

    /// Starts joining `ssid`: picks the best BSS in the scan table and begins authentication. `entropy` is 32 random
    /// bytes from the kernel's generator, the source of every SNonce on this association. PBKDF2 for a passphrase takes
    /// milliseconds here; use [`Credentials::psk`] to avoid it. On success any previous association has been left.
    pub fn connect(
        &mut self,
        ssid: &[u8],
        creds: Credentials,
        entropy: [u8; 32],
        now_ms: u64,
    ) -> Result<MacAddr, ConnectError> {
        if ssid.is_empty() || ssid.len() > 32 {
            return Err(ConnectError::BadSsid);
        }
        if now_ms < self.blocked_until {
            return Err(ConnectError::Countermeasures);
        }
        self.table.expire(now_ms);
        let mut best: Option<(&ScanResult, Option<Akm>)> = None;
        let mut first_error = None;
        for bss in self.table.iter().filter(|b| b.ssid == ssid && b.is_ess()) {
            match self.usable(bss, &creds) {
                Ok(akm) => {
                    if best.is_none_or(|(b, _)| score(bss) > score(b)) {
                        best = Some((bss, akm));
                    }
                }
                Err(e) => first_error = first_error.or(Some(e)),
            }
        }
        let Some((bss, akm)) = best else {
            return Err(first_error.unwrap_or(ConnectError::NoSuchNetwork));
        };
        let bss = bss.clone();

        // Leave anything current before touching state for the new BSS.
        if self.conn.is_some() {
            self.disconnect();
        }

        let plan = plan_connection(&bss, &self.hw, &self.reg).ok_or(ConnectError::NoCommonRates)?;
        let (rsn_ie, supplicant) = match (akm, creds.pmk(ssid)) {
            (Some(akm), Some(pmk)) => {
                let rsn = bss.rsn.as_ref().ok_or(ConnectError::UnsupportedSecurity)?;
                let group =
                    GroupCipher::from_suite(rsn.group).ok_or(ConnectError::UnsupportedSecurity)?;
                let ap_rsn_ie = bss
                    .rsn_ie
                    .clone()
                    .ok_or(ConnectError::UnsupportedSecurity)?;
                let own = build_rsn_ie(group.suite(), akm.suite());
                let mut sup = Supplicant::new(SupplicantConfig {
                    pmk,
                    spa: self.mac,
                    aa: bss.bssid,
                    akm,
                    group,
                    own_rsn_ie: own.clone(),
                    ap_rsn_ie,
                    entropy,
                });
                if let Some(n) = self.fixed_snonce.take() {
                    sup.set_fixed_snonce(n);
                }
                (Some(own), Some(sup))
            }
            _ => (None, None),
        };
        let mut entropy = entropy;
        wipe(&mut entropy);

        let bssid = bss.bssid;
        self.out
            .push_back(Output::JoinBss(join_info(&bss, &plan, &self.reg)));
        self.scanning = false;
        self.conn = Some(Conn {
            bss,
            plan,
            phase: Phase::Auth {
                tries: 0,
                deadline: 0,
            },
            rsn_ie,
            supplicant,
            port_open: false,
            pending_eapol: None,
            last_beacon: now_ms,
            probe_tries: 0,
            next_probe: 0,
            beacon_lost: false,
            wmm_last_param_set: None,
        });
        self.send_auth(now_ms);
        Ok(bssid)
    }

    /// Leaves the current network: a Deauthentication goes out, then the usual [`Output::LeaveBss`].
    pub fn disconnect(&mut self) {
        if self.conn.is_some() {
            self.teardown(DisconnectReason::Requested, Some(REASON_DEAUTH_LEAVING));
        }
    }

    fn queue_mgmt(&mut self, mut frame: Vec<u8>) {
        set_sequence(&mut frame, self.seq);
        self.seq = (self.seq + 1) & 0x0fff;
        self.out.push_back(Output::TxMgmt(frame));
    }

    fn teardown(&mut self, reason: DisconnectReason, tx_deauth: Option<u16>) {
        let Some(conn) = self.conn.take() else {
            return;
        };
        if let Some(code) = tx_deauth {
            self.queue_mgmt(build_deauth(&conn.bss.bssid, &self.mac, code));
        }
        self.reg.clear_country();
        self.out.push_back(Output::LeaveBss);
        self.out.push_back(Output::Disconnected { reason });
    }

    fn send_auth(&mut self, now_ms: u64) {
        let Some(conn) = &mut self.conn else {
            return;
        };
        let Phase::Auth { tries, deadline } = &mut conn.phase else {
            return;
        };
        *tries += 1;
        if *tries > AUTH_MAX_TRIES {
            self.teardown(DisconnectReason::AuthTimeout, None);
            return;
        }
        *deadline = now_ms
            + if self.hw.reports_tx_ack {
                AUTH_TIMEOUT_LONG_MS
            } else {
                AUTH_TIMEOUT_MS
            };
        let frame = build_auth(&conn.bss.bssid, &self.mac);
        self.queue_mgmt(frame);
    }

    fn start_assoc(&mut self, now_ms: u64) {
        if let Some(conn) = &mut self.conn {
            conn.phase = Phase::Assoc {
                tries: 0,
                deadline: 0,
                comeback: false,
            };
        }
        self.send_assoc(now_ms);
    }

    fn send_assoc(&mut self, now_ms: u64) {
        let Some(conn) = &mut self.conn else {
            return;
        };
        let Phase::Assoc {
            tries,
            deadline,
            comeback,
        } = &mut conn.phase
        else {
            return;
        };
        *tries += 1;
        *comeback = false;
        if *tries > ASSOC_MAX_TRIES {
            self.teardown(DisconnectReason::AssocTimeout, None);
            return;
        }
        *deadline = now_ms
            + if self.hw.reports_tx_ack {
                ASSOC_TIMEOUT_LONG_MS
            } else {
                ASSOC_TIMEOUT_MS
            };
        let power = self
            .reg
            .channel(conn.bss.freq_mhz)
            .map_or(0, |c| c.max_power_dbm);
        let frame = build_assoc_req(
            &conn.bss,
            &self.mac,
            &self.hw,
            &conn.plan,
            conn.rsn_ie.as_deref(),
            power,
        );
        self.queue_mgmt(frame);
    }

    /// The hardware's verdict on the last management frame: mac80211 retries at once on no ack, and shortens its wait
    /// on an ack (`ieee80211_mgd_conn_tx_status` into `ieee80211_sta_work`).
    pub fn on_mgmt_tx_status(&mut self, acked: bool, now_ms: u64) {
        let Some(conn) = &mut self.conn else {
            return;
        };
        match &mut conn.phase {
            Phase::Auth { deadline, .. } => {
                *deadline = if acked {
                    now_ms + AUTH_TIMEOUT_SHORT_MS
                } else {
                    now_ms
                };
            }
            Phase::Assoc {
                deadline,
                comeback: false,
                ..
            } => {
                *deadline = if acked {
                    now_ms + ASSOC_TIMEOUT_SHORT_MS
                } else {
                    now_ms
                };
            }
            _ => {}
        }
    }

    /// A management frame from the air, as received. `rssi_dbm` and `freq_mhz` come from the driver's RX metadata.
    pub fn on_mgmt_rx(&mut self, frame: &[u8], rssi_dbm: i8, freq_mhz: u16, now_ms: u64) {
        let Some(f) = MgmtFrame::parse(frame) else {
            self.stats.rx_malformed += 1;
            return;
        };
        match f.subtype() {
            STYPE_BEACON | STYPE_PROBE_RESP => {
                if let Some(bss) = ScanResult::parse(frame, rssi_dbm, freq_mhz, now_ms) {
                    if f.subtype() == STYPE_BEACON {
                        self.reg.on_beacon(bss.freq_mhz);
                    }
                    if let Some(conn) = &mut self.conn
                        && conn.bss.bssid == bss.bssid
                    {
                        conn.last_beacon = now_ms;
                        conn.probe_tries = 0;
                        conn.beacon_lost = false;
                        // ieee80211_rx_mgmt_beacon -> ieee80211_sta_wmm_params: a parameter set count other than
                        // the last one means new EDCA parameters; the count is kept even when the set fails the
                        // checks, and then nothing is sent
                        if f.subtype() == STYPE_BEACON
                            && let Some(last) = conn.wmm_last_param_set.as_mut()
                            && let Some(w) = &bss.wmm
                            && let Some(params) = &w.params
                        {
                            let count = w.qos_info & 0x0f;
                            if *last != count {
                                *last = count;
                                if let Some(valid) = validate_wmm(params) {
                                    self.out.push_back(Output::SetWmm(valid));
                                }
                            }
                        }
                    }
                    self.table.update(bss);
                }
            }
            STYPE_AUTH => self.on_auth(&f, now_ms),
            STYPE_ASSOC_RESP | STYPE_REASSOC_RESP => self.on_assoc_resp(&f, now_ms),
            STYPE_DEAUTH | STYPE_DISASSOC => self.on_deauth_or_disassoc(&f),
            _ => {}
        }
    }

    fn is_from_our_ap(&self, f: &MgmtFrame<'_>) -> bool {
        self.conn
            .as_ref()
            .is_some_and(|c| f.bssid == c.bss.bssid && f.sa == c.bss.bssid)
    }

    fn on_auth(&mut self, f: &MgmtFrame<'_>, now_ms: u64) {
        if !self.is_from_our_ap(f) || f.da != self.mac {
            return;
        }
        if !matches!(
            self.conn.as_ref().map(|c| &c.phase),
            Some(Phase::Auth { .. })
        ) {
            return;
        }
        let Some(auth) = AuthBody::parse(f.body) else {
            self.stats.rx_malformed += 1;
            return;
        };
        if auth.algorithm != WLAN_AUTH_OPEN || auth.transaction != 2 {
            return;
        }
        if auth.status != STATUS_SUCCESS {
            self.teardown(
                DisconnectReason::AuthRejected {
                    status: auth.status,
                },
                None,
            );
            return;
        }
        self.start_assoc(now_ms);
    }

    fn on_assoc_resp(&mut self, f: &MgmtFrame<'_>, now_ms: u64) {
        if !self.is_from_our_ap(f) || f.da != self.mac {
            return;
        }
        if !matches!(
            self.conn.as_ref().map(|c| &c.phase),
            Some(Phase::Assoc { .. })
        ) {
            return;
        }
        let Some(resp) = AssocRespBody::parse(f.body) else {
            self.stats.rx_malformed += 1;
            return;
        };
        if resp.status == STATUS_ASSOC_REJECTED_TEMPORARILY {
            // Timeout Interval of type "association comeback": wait that long, then the next try.
            if let Some(t) =
                find(resp.ies, EID_TIMEOUT_INTERVAL).filter(|t| t.len() >= 5 && t[0] == 3)
            {
                let tu = u32::from_le_bytes([t[1], t[2], t[3], t[4]]) as u64;
                if let Some(Conn {
                    phase:
                        Phase::Assoc {
                            deadline, comeback, ..
                        },
                    ..
                }) = &mut self.conn
                {
                    *deadline = now_ms + tu * 1024 / 1000;
                    *comeback = true;
                }
                return;
            }
        }
        if resp.status != STATUS_SUCCESS {
            self.teardown(
                DisconnectReason::AssocRejected {
                    status: resp.status,
                },
                None,
            );
            return;
        }
        let Some(conn) = &mut self.conn else {
            return;
        };
        let info = peer_info(&conn.bss, &conn.plan, &self.hw, &resp);
        // cfg80211 hints the AP's country on connect; it can only restrict further.
        if let Some(c) = conn
            .bss
            .country
            .as_deref()
            .and_then(crate::regulatory::Country::parse)
        {
            self.reg.set_country(Some(c));
        }
        let secured = conn.supplicant.is_some();
        conn.last_beacon = now_ms;
        // ieee80211_assoc_success: the set the association used gives the count beacons are compared with; with no
        // valid set mac80211 programs its defaults and stops following beacons (disable_wmm_tracking)
        conn.wmm_last_param_set = if info.wmm_params.is_some() {
            parse_wmm(resp.ies)
                .filter(|w| w.params.is_some())
                .or(conn.bss.wmm)
                .map(|w| w.qos_info & 0x0f)
        } else {
            None
        };
        conn.phase = Phase::Associated {
            handshake_deadline: secured.then_some(now_ms + HANDSHAKE_TIMEOUT_MS),
        };
        conn.port_open = !secured;
        let pending = conn.pending_eapol.take();
        self.out.push_back(Output::PeerAssoc(info));
        if !secured {
            self.out.push_back(Output::PortAuthorized);
        }
        if let Some(eapol) = pending {
            self.feed_supplicant(&eapol);
        }
    }

    fn on_deauth_or_disassoc(&mut self, f: &MgmtFrame<'_>) {
        if !self.is_from_our_ap(f) || (f.da != self.mac && !is_multicast(&f.da)) {
            return;
        }
        let Some(code) = le16_at(f.body, 0) else {
            self.stats.rx_malformed += 1;
            return;
        };
        let during = self.state() == StationState::Handshaking;
        let reason = if f.subtype() == STYPE_DEAUTH {
            DisconnectReason::Deauthenticated {
                reason: code,
                during_handshake: during,
            }
        } else {
            DisconnectReason::Disassociated {
                reason: code,
                during_handshake: during,
            }
        };
        self.teardown(reason, None);
    }

    /// Firmware reports the AP gone (`WMI_ROAM_REASON_BMISS`, what `ath10k_mac_handle_beacon_miss` turns into
    /// `ieee80211_beacon_loss`): probe it now rather than wait for the beacon timer.
    pub fn on_beacon_loss(&mut self, now_ms: u64) {
        if let Some(conn) = &mut self.conn
            && matches!(conn.phase, Phase::Associated { .. })
        {
            conn.beacon_lost = true;
        }
        self.tick(now_ms);
    }

    /// The driver has done [`Output::JoinBss`]: the vdev runs and the peer exists. The authentication queued behind it
    /// goes out now, so its timer starts now. In Linux the join is over before the first frame is built
    /// (`ieee80211_prep_connection`, then `ieee80211_auth`); here it is an output like any other, and however long it
    /// took must not count as the AP not answering. Don't [`Station::tick`] between the two.
    pub fn on_joined(&mut self, now_ms: u64) {
        let wait = if self.hw.reports_tx_ack {
            AUTH_TIMEOUT_LONG_MS
        } else {
            AUTH_TIMEOUT_MS
        };
        if let Some(Conn {
            phase: Phase::Auth { deadline, .. },
            ..
        }) = &mut self.conn
        {
            *deadline = now_ms + wait;
        }
    }

    /// Firmware says the connection is gone for good (`ieee80211_connection_loss`).
    pub fn on_connection_lost(&mut self) {
        self.teardown(DisconnectReason::BeaconLost, None);
    }

    /// A beacon from the current AP, for drivers that pass beacon arrival separately from [`Station::on_mgmt_rx`].
    pub fn on_beacon_seen(&mut self, now_ms: u64) {
        if let Some(conn) = &mut self.conn {
            conn.last_beacon = now_ms;
            conn.probe_tries = 0;
            conn.beacon_lost = false;
        }
    }

    /// When [`Station::tick`] must run next: an authentication or association retry, the end of the handshake, the next
    /// lost-beacon probe, or, without the firmware's connection monitor, the silence after which probing starts
    /// (`beacon_monitor`). A time in the past means now. `None`: nothing until a frame or an event comes.
    pub fn next_deadline(&self) -> Option<u64> {
        let conn = self.conn.as_ref()?;
        match &conn.phase {
            Phase::Auth { deadline, .. } | Phase::Assoc { deadline, .. } => Some(*deadline),
            Phase::Associated { handshake_deadline } => {
                let handshake = if conn.port_open { None } else { *handshake_deadline };
                let probe = if conn.probe_tries > 0 {
                    Some(conn.next_probe)
                } else if conn.beacon_lost {
                    Some(0)
                } else if self.hw.connection_monitor {
                    None
                } else {
                    let interval = match conn.bss.beacon_interval_tu {
                        0 => 100,
                        tu => tu as u64,
                    };
                    Some(conn.last_beacon + BEACON_LOSS_COUNT * interval * 1024 / 1000)
                };
                match (handshake, probe) {
                    (Some(a), Some(b)) => Some(a.min(b)),
                    (a, None) => a,
                    (None, b) => b,
                }
            }
        }
    }

    /// Time passes. Call at least every 50 ms while connecting and every few hundred once connected.
    pub fn tick(&mut self, now_ms: u64) {
        let Some(conn) = &mut self.conn else {
            return;
        };
        match &conn.phase {
            Phase::Auth { deadline, .. } => {
                if now_ms >= *deadline {
                    self.send_auth(now_ms);
                }
            }
            Phase::Assoc { deadline, .. } => {
                if now_ms >= *deadline {
                    self.send_assoc(now_ms);
                }
            }
            Phase::Associated { handshake_deadline } => {
                if !conn.port_open && handshake_deadline.is_some_and(|d| now_ms >= d) {
                    self.teardown(
                        DisconnectReason::HandshakeTimeout,
                        Some(REASON_DEAUTH_LEAVING),
                    );
                    return;
                }
                self.beacon_monitor(now_ms);
            }
        }
    }

    /// mac80211's connection monitor, reduced: silence for 7 beacon intervals (or the firmware's word, with
    /// [`HwCaps::connection_monitor`]), then up to 5 probes 500 ms apart, the last 3 broadcast because "some buggy APs
    /// only support broadcast probe requests".
    fn beacon_monitor(&mut self, now_ms: u64) {
        let Some(conn) = &mut self.conn else {
            return;
        };
        let interval = match conn.bss.beacon_interval_tu {
            0 => 100,
            tu => tu as u64,
        };
        let silence = BEACON_LOSS_COUNT * interval * 1024 / 1000;
        if !conn.beacon_lost && (self.hw.connection_monitor || now_ms < conn.last_beacon + silence)
        {
            return;
        }
        if conn.probe_tries > 0 && now_ms < conn.next_probe {
            return;
        }
        if conn.probe_tries >= MAX_PROBE_TRIES {
            self.teardown(DisconnectReason::BeaconLost, None);
            return;
        }
        let unicast_limit = MAX_PROBE_TRIES.saturating_sub(3).max(1);
        let dst = if conn.probe_tries >= unicast_limit {
            BROADCAST
        } else {
            conn.bss.bssid
        };
        conn.probe_tries += 1;
        conn.next_probe = now_ms + PROBE_WAIT_MS;
        let frame = build_probe_req(&dst, &self.mac, &conn.bss.ssid, conn.bss.band, &conn.plan);
        self.queue_mgmt(frame);
    }

    /// A data frame from the hardware. EAPOL goes to the supplicant; the rest comes out as [`Output::RxEthernet`] if
    /// the port is open.
    pub fn on_data_rx(&mut self, frame: RxFrame<'_>, meta: RxMeta) {
        let bssid = match &self.conn {
            Some(c) => c.bss.bssid,
            None => {
                self.stats.rx_foreign += 1;
                return;
            }
        };
        match frame {
            RxFrame::Ethernet(eth) => self.deliver(eth, &meta),
            RxFrame::NativeWifi(f) => {
                if !self.check_data_header(f, &bssid) {
                    return;
                }
                match nwifi_to_ethernet(f) {
                    Some(eth) => self.deliver(&eth, &meta),
                    None => self.stats.rx_malformed += 1,
                }
            }
            RxFrame::Raw80211 {
                frame: f,
                crypto_header,
            } => {
                if !self.check_data_header(f, &bssid) {
                    return;
                }
                let frames = raw80211_to_ethernet(f, crypto_header);
                if frames.is_empty() {
                    self.stats.rx_malformed += 1;
                }
                for eth in frames {
                    self.deliver(&eth, &meta);
                }
            }
        }
    }

    /// A station takes data From-DS only, transmitted by its AP.
    fn check_data_header(&mut self, frame: &[u8], bssid: &MacAddr) -> bool {
        let Some(h) = DataHeader::parse(frame) else {
            self.stats.rx_malformed += 1;
            return false;
        };
        if h.fc & (FCTL_TODS | FCTL_FROMDS) != FCTL_FROMDS || h.ta != *bssid {
            self.stats.rx_foreign += 1;
            return false;
        }
        true
    }

    fn deliver(&mut self, eth: &[u8], meta: &RxMeta) {
        let (Some(da), Some(sa), Some(proto)) = (mac_at(eth, 0), mac_at(eth, 6), ethertype(eth))
        else {
            self.stats.rx_malformed += 1;
            return;
        };
        if da != self.mac && !is_multicast(&da) {
            self.stats.rx_foreign += 1;
            return;
        }
        // Our own multicast coming back through the AP (`ieee80211_data_to_8023_exthdr`).
        if is_multicast(&da) && sa == self.mac {
            self.stats.rx_foreign += 1;
            return;
        }
        if proto == ETH_P_EAPOL {
            self.on_eapol(eth, &da, &sa);
            return;
        }
        let Some(conn) = &self.conn else {
            self.stats.rx_foreign += 1;
            return;
        };
        if !conn.port_open {
            self.stats.rx_port_closed += 1;
            return;
        }
        if conn
            .supplicant
            .as_ref()
            .is_some_and(Supplicant::pairwise_installed)
            && !meta.decrypted
        {
            self.stats.rx_unprotected += 1;
            return;
        }
        if self.out.len() >= RX_QUEUE_LIMIT {
            self.stats.rx_queue_full += 1;
            return;
        }
        self.out.push_back(Output::RxEthernet(Vec::from(eth)));
    }

    fn on_eapol(&mut self, eth: &[u8], da: &MacAddr, sa: &MacAddr) {
        self.stats.eapol_rx += 1;
        let Some(conn) = &mut self.conn else {
            return;
        };
        if *sa != conn.bss.bssid || (*da != self.mac && *da != PAE_GROUP) {
            self.stats.rx_foreign += 1;
            return;
        }
        let eapol = &eth[ETH_HLEN..];
        match conn.phase {
            // Data and management take different paths through the firmware, so message 1 can overtake the
            // association response. wpa_supplicant keeps it for the same reason (`pending_eapol_rx`).
            Phase::Auth { .. } | Phase::Assoc { .. } => conn.pending_eapol = Some(Vec::from(eapol)),
            Phase::Associated { .. } => self.feed_supplicant(eapol),
        }
    }

    fn feed_supplicant(&mut self, eapol: &[u8]) {
        let Some(sup) = self.conn.as_mut().and_then(|c| c.supplicant.as_mut()) else {
            return;
        };
        for event in sup.on_eapol(eapol) {
            match event {
                WpaEvent::Tx { pdu, protected } => self.queue_eapol(&pdu, protected),
                WpaEvent::InstallPairwise(tk) => {
                    if let Some(conn) = &self.conn {
                        self.out.push_back(Output::InstallPairwise {
                            peer: conn.bss.bssid,
                            tk,
                        });
                    }
                }
                WpaEvent::InstallGroup {
                    idx,
                    key,
                    cipher,
                    rsc,
                } => {
                    self.out.push_back(Output::InstallGroup {
                        idx,
                        key,
                        cipher,
                        rsc,
                    });
                }
                WpaEvent::Authorized => {
                    if let Some(conn) = &mut self.conn {
                        conn.port_open = true;
                        if let Phase::Associated { handshake_deadline } = &mut conn.phase {
                            *handshake_deadline = None;
                        }
                    }
                    self.out.push_back(Output::PortAuthorized);
                }
                WpaEvent::Failed { reason } => {
                    self.teardown(DisconnectReason::HandshakeFailed { reason }, Some(reason));
                    return;
                }
            }
        }
    }

    /// Wraps an EAPOL PDU into a native-WiFi frame to the AP.
    fn queue_eapol(&mut self, pdu: &[u8], protected: bool) {
        let Some(conn) = &self.conn else {
            return;
        };
        let bssid = conn.bss.bssid;
        let mut eth = Vec::with_capacity(ETH_HLEN + pdu.len());
        eth.extend_from_slice(&bssid);
        eth.extend_from_slice(&self.mac);
        eth.extend_from_slice(&ETH_P_EAPOL.to_be_bytes());
        eth.extend_from_slice(pdu);
        if let Some(mut frame) = ethernet_to_nwifi(&eth, &bssid, &self.mac, protected) {
            set_sequence(&mut frame, self.seq);
            self.seq = (self.seq + 1) & 0x0fff;
            self.out.push_back(Output::TxData { frame, eapol: true });
        }
    }

    /// An Ethernet II frame from the network stack. Comes out as [`Output::TxData`], encrypted by firmware once a key
    /// is in.
    pub fn tx_ethernet(&mut self, eth: &[u8]) -> Result<(), TxError> {
        let Some(conn) = &self.conn else {
            return Err(TxError::PortClosed);
        };
        if !conn.port_open {
            return Err(TxError::PortClosed);
        }
        if eth.len() < ETH_HLEN || ethertype(eth) == Some(ETH_P_EAPOL) {
            return Err(TxError::BadFrame);
        }
        let protected = conn
            .supplicant
            .as_ref()
            .is_some_and(Supplicant::pairwise_installed);
        let mut frame = ethernet_to_nwifi(eth, &conn.bss.bssid, &self.mac, protected)
            .ok_or(TxError::BadFrame)?;
        set_sequence(&mut frame, self.seq);
        self.seq = (self.seq + 1) & 0x0fff;
        self.stats.tx_data += 1;
        self.out.push_back(Output::TxData {
            frame,
            eapol: false,
        });
        Ok(())
    }

    /// The hardware saw a Michael MIC failure (TKIP group traffic only: pairwise is CCMP, whose failures are plain
    /// drops). Reports it to the AP; a second within 60 s ends the association and blocks joining for 60 s, as
    /// wpa_supplicant's countermeasures do.
    pub fn on_mic_failure(&mut self, group: bool, now_ms: u64) {
        let Some(sup) = self.conn.as_mut().and_then(|c| c.supplicant.as_mut()) else {
            return;
        };
        let protected = sup.pairwise_installed();
        if let Some(pdu) = sup.mic_failure_report(!group) {
            self.queue_eapol(&pdu, protected);
            self.out.push_back(Output::MicFailureReported { group });
        }
        let second = self
            .last_mic_failure
            .is_some_and(|t| now_ms.saturating_sub(t) < MIC_FAILURE_WINDOW_MS);
        self.last_mic_failure = Some(now_ms);
        if second {
            self.blocked_until = now_ms + MIC_FAILURE_WINDOW_MS;
            self.teardown(
                DisconnectReason::MicCountermeasures,
                Some(REASON_MICHAEL_MIC_FAILURE),
            );
        }
    }
}

/// Picks among BSSes of one network: signal, with a 10 dB bonus for 5 GHz, whose throughput usually wins there. A
/// simple stand-in for wpa_supplicant's estimated-throughput comparison.
fn score(bss: &ScanResult) -> i32 {
    bss.rssi_dbm as i32 + if bss.band == Band::Ghz5 { 10 } else { 0 }
}
