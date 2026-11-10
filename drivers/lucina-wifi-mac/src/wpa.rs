//! The WPA2-PSK supplicant: the 4-way and group key handshakes, following wpa_supplicant's `rsn_supp/wpa.c`
//! (2.11), and IEEE 802.11-2020 section 12.7.
//!
//! The rules taken from wpa_supplicant, on purpose:
//!
//! * The replay check runs on every EAPOL-Key frame before anything else; the stored counter moves only when a
//!   MIC has verified (`wpa_sm_rx_eapol`, `wpa_supplicant_verify_eapol_key_mic`).
//! * Message 1 derives a temporary PTK. It replaces the real one only when message 3's MIC verifies against it.
//!   A forged message 1, which has no MIC, can therefore not disturb an established PTK.
//! * The same SNonce is used for repeats of message 1 until a message 3 has been accepted.
//! * Message 3's RSN element must equal the one in the beacon, byte for byte, or the association ends with reason
//!   17: it is the downgrade protection.
//! * The same PTK or GTK is never installed twice: reinstalling would reset the packet numbers (KRACK).
//! * Message 4 goes out before the PTK is installed, so on a first handshake it leaves in the clear.

use crate::crypto::cmac::aes128_cmac;
use crate::crypto::hash::{hmac_sha1, pbkdf2_hmac_sha1};
use crate::crypto::keywrap::aes_unwrap;
use crate::crypto::prf::{kdf_sha256, prf_sha1};
use crate::crypto::secret::{SecretBytes, SecretVec, ct_eq, wipe};
use crate::frame::{MacAddr, REASON_IE_DIFFERENT, REASON_UNSPECIFIED, be16_at};
use crate::ie::{AKM_PSK, AKM_PSK_SHA256, CIPHER_CCMP, CIPHER_TKIP, EID_RSN, EID_VENDOR, Suite};
use alloc::vec;
use alloc::vec::Vec;
use core::fmt;

/// EAPOL packet type for EAPOL-Key.
pub const EAPOL_KEY: u8 = 3;
/// Key descriptor type for RSN.
pub const KEY_DESC_RSN: u8 = 2;

pub const KEY_INFO_VERSION_MASK: u16 = 0x0007;
pub const KEY_INFO_VERSION_HMAC_SHA1_AES: u16 = 2;
pub const KEY_INFO_VERSION_AES_128_CMAC: u16 = 3;
pub const KEY_INFO_PAIRWISE: u16 = 0x0008;
pub const KEY_INFO_INDEX_MASK: u16 = 0x0030;
pub const KEY_INFO_INSTALL: u16 = 0x0040;
pub const KEY_INFO_ACK: u16 = 0x0080;
pub const KEY_INFO_MIC: u16 = 0x0100;
pub const KEY_INFO_SECURE: u16 = 0x0200;
pub const KEY_INFO_ERROR: u16 = 0x0400;
pub const KEY_INFO_REQUEST: u16 = 0x0800;
pub const KEY_INFO_ENCR_KEY_DATA: u16 = 0x1000;

/// Offsets in an EAPOL-Key PDU, EAPOL header included.
const OFF_KEY_INFO: usize = 5;
const OFF_KEY_LEN: usize = 7;
const OFF_REPLAY: usize = 9;
const OFF_NONCE: usize = 17;
const OFF_RSC: usize = 65;
const OFF_MIC: usize = 81;
const OFF_DATA_LEN: usize = 97;
/// EAPOL header (4) plus EAPOL-Key fixed part (95) with a 16-byte MIC.
pub const EAPOL_KEY_HDR_LEN: usize = 99;

/// CCMP-128 temporal key length, and the PTK's: KCK 16, KEK 16, TK 16.
pub const TK_LEN: usize = 16;
pub const PTK_LEN: usize = 48;

/// RSN KDE selector for the GTK.
const KDE_OUI: [u8; 3] = [0x00, 0x0f, 0xac];
const KDE_GTK: u8 = 1;

/// The AKM in use. It decides how the PTK is derived and how the MIC is computed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Akm {
    /// 00-0F-AC:2. PRF-SHA1, HMAC-SHA1-128, key descriptor version 2.
    Psk,
    /// 00-0F-AC:6. KDF-SHA256, AES-128-CMAC, key descriptor version 3.
    PskSha256,
}

impl Akm {
    pub fn suite(self) -> Suite {
        match self {
            Self::Psk => AKM_PSK,
            Self::PskSha256 => AKM_PSK_SHA256,
        }
    }

    fn version(self) -> u16 {
        match self {
            Self::Psk => KEY_INFO_VERSION_HMAC_SHA1_AES,
            Self::PskSha256 => KEY_INFO_VERSION_AES_128_CMAC,
        }
    }
}

/// A group cipher this station takes. Pairwise is always CCMP.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GroupCipher {
    Ccmp,
    /// Mixed-mode WPA/WPA2 networks. The hardware does the Michael and TKIP work.
    Tkip,
}

impl GroupCipher {
    pub fn from_suite(suite: Suite) -> Option<Self> {
        match suite {
            CIPHER_CCMP => Some(Self::Ccmp),
            CIPHER_TKIP => Some(Self::Tkip),
            _ => None,
        }
    }

    pub fn suite(self) -> Suite {
        match self {
            Self::Ccmp => CIPHER_CCMP,
            Self::Tkip => CIPHER_TKIP,
        }
    }

    pub fn key_len(self) -> usize {
        match self {
            Self::Ccmp => 16,
            Self::Tkip => 32,
        }
    }
}

/// The PMK, from `wpa_passphrase`'s derivation: PBKDF2-HMAC-SHA1, 4096 rounds, the SSID as salt, 32 bytes.
pub fn derive_psk(passphrase: &[u8], ssid: &[u8]) -> SecretBytes<32> {
    let mut pmk = SecretBytes::zero();
    pbkdf2_hmac_sha1(passphrase, ssid, 4096, pmk.expose_mut());
    pmk
}

/// PMKID = HMAC-SHA1-128(PMK, "PMK Name" || AA || SPA). Not used by the handshake; it is a check on a PMK.
pub fn pmkid(pmk: &[u8; 32], aa: &MacAddr, spa: &MacAddr) -> [u8; 16] {
    let mut data = [0u8; 20];
    data[..8].copy_from_slice(b"PMK Name");
    data[8..14].copy_from_slice(aa);
    data[14..20].copy_from_slice(spa);
    let mut out = [0u8; 16];
    out.copy_from_slice(&hmac_sha1(pmk, &data)[..16]);
    out
}

/// The three keys of a CCMP PTK. No `Debug` output of the keys, wiped on drop.
#[derive(Clone, PartialEq, Eq)]
pub struct Ptk {
    bytes: SecretBytes<PTK_LEN>,
}

impl fmt::Debug for Ptk {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Ptk(<secret>)")
    }
}

impl Ptk {
    /// Derives the PTK: min/max of the addresses, min/max of the nonces, 384 bits.
    pub fn derive(
        akm: Akm,
        pmk: &[u8; 32],
        aa: &MacAddr,
        spa: &MacAddr,
        anonce: &[u8; 32],
        snonce: &[u8; 32],
    ) -> Self {
        let mut data = [0u8; 76];
        let (a1, a2) = if aa < spa { (aa, spa) } else { (spa, aa) };
        data[..6].copy_from_slice(a1);
        data[6..12].copy_from_slice(a2);
        let (n1, n2) = if anonce < snonce {
            (anonce, snonce)
        } else {
            (snonce, anonce)
        };
        data[12..44].copy_from_slice(n1);
        data[44..76].copy_from_slice(n2);
        let mut bytes = SecretBytes::<PTK_LEN>::zero();
        match akm {
            Akm::Psk => prf_sha1(pmk, b"Pairwise key expansion", &data, bytes.expose_mut()),
            Akm::PskSha256 => kdf_sha256(pmk, b"Pairwise key expansion", &data, bytes.expose_mut()),
        }
        Self { bytes }
    }

    pub fn kck(&self) -> &[u8] {
        &self.bytes.expose()[..16]
    }

    pub fn kek(&self) -> [u8; 16] {
        let mut k = [0u8; 16];
        k.copy_from_slice(&self.bytes.expose()[16..32]);
        k
    }

    pub fn tk(&self) -> SecretBytes<TK_LEN> {
        let mut k = SecretBytes::<TK_LEN>::zero();
        k.expose_mut().copy_from_slice(&self.bytes.expose()[32..48]);
        k
    }
}

/// The MIC of an EAPOL-Key PDU whose MIC field is already zero.
pub fn compute_mic(akm: Akm, kck: &[u8], pdu: &[u8]) -> [u8; 16] {
    match akm {
        Akm::Psk => {
            let mut out = [0u8; 16];
            out.copy_from_slice(&hmac_sha1(kck, pdu)[..16]);
            out
        }
        Akm::PskSha256 => {
            let mut key = [0u8; 16];
            key.copy_from_slice(kck);
            let mic = aes128_cmac(&key, pdu);
            wipe(&mut key);
            mic
        }
    }
}

/// True if `pdu`'s MIC is right for this KCK.
pub fn verify_mic(akm: Akm, kck: &[u8], pdu: &[u8]) -> bool {
    let Some(sent) = pdu.get(OFF_MIC..OFF_MIC + 16) else {
        return false;
    };
    let mut copy = Vec::from(pdu);
    copy[OFF_MIC..OFF_MIC + 16].fill(0);
    ct_eq(&compute_mic(akm, kck, &copy), sent)
}

/// A parsed EAPOL-Key frame: only its framing is checked here.
#[derive(Clone, Copy, Debug)]
pub struct EapolKey<'a> {
    pub key_info: u16,
    pub key_length: u16,
    pub replay: [u8; 8],
    pub nonce: &'a [u8],
    pub rsc: &'a [u8],
    pub key_data: &'a [u8],
    /// The PDU's own length from its header: trailing padding is outside it and outside the MIC.
    pub pdu: &'a [u8],
}

impl<'a> EapolKey<'a> {
    /// `frame` starts at the EAPOL header. `None` for anything but a well-framed RSN EAPOL-Key PDU.
    pub fn parse(frame: &'a [u8]) -> Option<Self> {
        if *frame.get(1)? != EAPOL_KEY {
            return None;
        }
        let body_len = be16_at(frame, 2)? as usize;
        let pdu = frame.get(..4 + body_len)?;
        if pdu.len() < EAPOL_KEY_HDR_LEN || pdu[4] != KEY_DESC_RSN {
            return None;
        }
        let data_len = be16_at(pdu, OFF_DATA_LEN)? as usize;
        let key_data = pdu.get(EAPOL_KEY_HDR_LEN..EAPOL_KEY_HDR_LEN + data_len)?;
        let mut replay = [0u8; 8];
        replay.copy_from_slice(&pdu[OFF_REPLAY..OFF_REPLAY + 8]);
        Some(Self {
            key_info: be16_at(pdu, OFF_KEY_INFO)?,
            key_length: be16_at(pdu, OFF_KEY_LEN)?,
            replay,
            nonce: &pdu[OFF_NONCE..OFF_NONCE + 32],
            rsc: &pdu[OFF_RSC..OFF_RSC + 8],
            key_data,
            pdu,
        })
    }
}

/// A GTK KDE, decoded. Wiped on drop.
#[derive(Clone)]
struct Gtk {
    idx: u8,
    key: SecretVec,
}

/// Walks RSN key data: `Rsn` element and GTK KDE; the rest is ignored (`wpa_parse_kde_ies`).
fn parse_key_data(data: &[u8]) -> (Option<&[u8]>, Option<Gtk>) {
    let mut rsn = None;
    let mut gtk = None;
    let mut pos = 0;
    while pos < data.len() {
        // 0xdd alone, or 0xdd 00, starts the padding of key-wrapped data.
        if data[pos] == EID_VENDOR && (pos + 1 == data.len() || data[pos + 1] == 0) {
            break;
        }
        let Some(&len) = data.get(pos + 1) else {
            break;
        };
        let Some(body) = data.get(pos + 2..pos + 2 + len as usize) else {
            break;
        };
        if data[pos] == EID_RSN && rsn.is_none() {
            rsn = data.get(pos..pos + 2 + len as usize);
        } else if data[pos] == EID_VENDOR
            && body.len() >= 6
            && body[..3] == KDE_OUI
            && body[3] == KDE_GTK
        {
            gtk = Some(Gtk {
                idx: body[4] & 3,
                key: SecretVec::from_slice(&body[6..]),
            });
        }
        pos += 2 + len as usize;
    }
    (rsn, gtk)
}

/// What the supplicant wants done, in this order.
#[derive(Debug)]
pub enum WpaEvent {
    /// An EAPOL PDU to send to the AP. `protected` is whether a TK was in the hardware when it was built: message 4
    /// of a first handshake goes out in the clear, and the Install that follows does not change that.
    Tx {
        pdu: Vec<u8>,
        protected: bool,
    },
    InstallPairwise(SecretBytes<TK_LEN>),
    InstallGroup {
        idx: u8,
        /// Key bytes as the driver wants them: for TKIP the Michael keys are already swapped for a station.
        key: SecretVec,
        cipher: GroupCipher,
        /// The first six bytes of Key RSC: the receive packet number, little-endian.
        rsc: [u8; 6],
    },
    /// Message 3 was accepted: open the port.
    Authorized,
    /// The handshake cannot go on: deauthenticate with this reason.
    Failed {
        reason: u16,
    },
}

/// Fixed parameters of one association.
pub struct SupplicantConfig {
    pub pmk: SecretBytes<32>,
    pub spa: MacAddr,
    pub aa: MacAddr,
    pub akm: Akm,
    pub group: GroupCipher,
    /// The RSN element we sent in the association request, repeated in message 2.
    pub own_rsn_ie: Vec<u8>,
    /// The RSN element the AP advertised, for the check in message 3.
    pub ap_rsn_ie: Vec<u8>,
    /// 32 random bytes from the owner: the source of every SNonce. This crate has no random number generator.
    pub entropy: [u8; 32],
}

/// The supplicant for one association.
pub struct Supplicant {
    pmk: SecretBytes<32>,
    spa: MacAddr,
    aa: MacAddr,
    akm: Akm,
    group: GroupCipher,
    own_rsn_ie: Vec<u8>,
    ap_rsn_ie: Vec<u8>,
    nonce_seed: SecretBytes<32>,
    nonce_counter: u64,
    fixed_snonce: Option<[u8; 32]>,
    snonce: [u8; 32],
    /// The ANonce of the last message 1, which `tptk` was derived from. Anyone can send a message 1.
    tptk_anonce: [u8; 32],
    /// The ANonce `ptk` was derived from. Moves with `ptk`, so that a forged message 1 after the handshake cannot make
    /// an authentic retransmitted message 3 look wrong, which in wpa_supplicant would end the association.
    ptk_anonce: [u8; 32],
    renew_snonce: bool,
    tptk: Option<Ptk>,
    ptk: Option<Ptk>,
    rx_replay: Option<[u8; 8]>,
    request_counter: u64,
    installed_tk: Option<SecretBytes<TK_LEN>>,
    installed_gtk: Option<Gtk>,
    authorized: bool,
}

impl Supplicant {
    pub fn new(config: SupplicantConfig) -> Self {
        Self {
            pmk: config.pmk,
            spa: config.spa,
            aa: config.aa,
            akm: config.akm,
            group: config.group,
            own_rsn_ie: config.own_rsn_ie,
            ap_rsn_ie: config.ap_rsn_ie,
            nonce_seed: SecretBytes::new(config.entropy),
            nonce_counter: 0,
            fixed_snonce: None,
            snonce: [0; 32],
            tptk_anonce: [0; 32],
            ptk_anonce: [0; 32],
            renew_snonce: true,
            tptk: None,
            ptk: None,
            rx_replay: None,
            request_counter: 0,
            installed_tk: None,
            installed_gtk: None,
            authorized: false,
        }
    }

    /// Makes the next SNonce this value: to replay a capture in tests, never for anything else.
    #[doc(hidden)]
    pub fn set_fixed_snonce(&mut self, snonce: [u8; 32]) {
        self.fixed_snonce = Some(snonce);
    }

    /// True once a message 3 has been accepted.
    pub fn is_authorized(&self) -> bool {
        self.authorized
    }

    /// True once a TK is in the hardware: EAPOL frames from here on go out protected.
    pub fn pairwise_installed(&self) -> bool {
        self.installed_tk.is_some()
    }

    fn next_snonce(&mut self) -> [u8; 32] {
        if let Some(fixed) = self.fixed_snonce.take() {
            return fixed;
        }
        // PRF-256(seed, "Init Counter", own address || counter), the shape 802.11 suggests for nonces.
        let mut data = [0u8; 14];
        data[..6].copy_from_slice(&self.spa);
        data[6..].copy_from_slice(&self.nonce_counter.to_be_bytes());
        self.nonce_counter += 1;
        let mut out = [0u8; 32];
        prf_sha1(self.nonce_seed.expose(), b"Init Counter", &data, &mut out);
        out
    }

    /// Handles an EAPOL PDU from the AP. Anything wrong with the frame is dropped silently, as wpa_supplicant
    /// does, so a forged frame cannot end the association.
    pub fn on_eapol(&mut self, frame: &[u8]) -> Vec<WpaEvent> {
        let Some(key) = EapolKey::parse(frame) else {
            return Vec::new();
        };
        if key.key_info & KEY_INFO_VERSION_MASK != self.akm.version() {
            return Vec::new();
        }
        if let Some(last) = &self.rx_replay
            && key.replay <= *last
        {
            return Vec::new();
        }
        let info = key.key_info;
        if info & KEY_INFO_REQUEST != 0 || info & KEY_INFO_ERROR != 0 {
            return Vec::new();
        }
        if info & KEY_INFO_PAIRWISE != 0 {
            if info & KEY_INFO_INDEX_MASK != 0 {
                return Vec::new();
            }
            if info & (KEY_INFO_MIC | KEY_INFO_ACK) == KEY_INFO_MIC | KEY_INFO_ACK {
                self.on_msg3(&key)
            } else if info & (KEY_INFO_MIC | KEY_INFO_ACK) == KEY_INFO_ACK {
                self.on_msg1(&key)
            } else {
                Vec::new()
            }
        } else if info & (KEY_INFO_MIC | KEY_INFO_ACK) == KEY_INFO_MIC | KEY_INFO_ACK {
            self.on_group1(&key)
        } else {
            Vec::new()
        }
    }

    fn on_msg1(&mut self, key: &EapolKey<'_>) -> Vec<WpaEvent> {
        self.tptk_anonce.copy_from_slice(key.nonce);
        if self.renew_snonce {
            self.snonce = self.next_snonce();
            self.renew_snonce = false;
        }
        let tptk = Ptk::derive(
            self.akm,
            self.pmk.expose(),
            &self.aa,
            &self.spa,
            &self.tptk_anonce,
            &self.snonce,
        );
        // Message 2: our nonce and our RSN element, Key Length 0 as wpa_supplicant_send_2_of_4 does for RSN.
        let pdu = self.build_key(
            self.akm.version() | KEY_INFO_PAIRWISE | KEY_INFO_MIC,
            &key.replay,
            &self.snonce,
            &self.own_rsn_ie,
            tptk.kck(),
        );
        self.tptk = Some(tptk);
        Vec::from([WpaEvent::Tx {
            pdu,
            protected: self.pairwise_installed(),
        }])
    }

    /// Checks the MIC against the temporary PTK, then the current one. On success the temporary PTK becomes
    /// current and the replay counter moves.
    fn verify(&mut self, key: &EapolKey<'_>) -> bool {
        if let Some(tptk) = &self.tptk
            && verify_mic(self.akm, tptk.kck(), key.pdu)
        {
            self.ptk = self.tptk.take();
            self.ptk_anonce = self.tptk_anonce;
            self.rx_replay = Some(key.replay);
            return true;
        }
        if let Some(ptk) = &self.ptk
            && verify_mic(self.akm, ptk.kck(), key.pdu)
        {
            self.rx_replay = Some(key.replay);
            return true;
        }
        false
    }

    /// Unwraps encrypted key data with the current KEK. `None` if it is not encrypted or does not unwrap.
    fn decrypt_key_data(&self, key: &EapolKey<'_>) -> Option<SecretVec> {
        if key.key_info & KEY_INFO_ENCR_KEY_DATA == 0 {
            return None;
        }
        let mut kek = self.ptk.as_ref()?.kek();
        let plain = aes_unwrap(&kek, key.key_data);
        wipe(&mut kek);
        plain
    }

    fn rsc_of(key: &EapolKey<'_>) -> [u8; 6] {
        let r = key.rsc;
        // wpa_supplicant_rsc_relaxation: some APs send the RSC in the wrong byte order, or as if it had 8 bytes. Taken
        // literally it would make the hardware drop every group frame, so a value of that shape counts as 0.
        if (r[5] != 0 && r[0] == 0) || r[6] != 0 || r[7] != 0 {
            return [0; 6];
        }
        let mut rsc = [0u8; 6];
        rsc.copy_from_slice(&r[..6]);
        rsc
    }

    fn on_msg3(&mut self, key: &EapolKey<'_>) -> Vec<WpaEvent> {
        if !self.verify(key) {
            return Vec::new();
        }
        let failed = |reason| Vec::from([WpaEvent::Failed { reason }]);
        if key.nonce != self.ptk_anonce {
            return failed(REASON_UNSPECIFIED);
        }
        if key.key_length as usize != TK_LEN {
            return failed(REASON_UNSPECIFIED);
        }
        // Without these two a port could open with no key behind it.
        if key.key_info & KEY_INFO_INSTALL == 0 || key.key_info & KEY_INFO_SECURE == 0 {
            return failed(REASON_UNSPECIFIED);
        }
        let Some(plain) = self.decrypt_key_data(key) else {
            return failed(REASON_UNSPECIFIED);
        };
        let (rsn, gtk) = parse_key_data(plain.expose());
        if rsn != Some(self.ap_rsn_ie.as_slice()) {
            return failed(REASON_IE_DIFFERENT);
        }
        let Some(gtk) = gtk else {
            return failed(REASON_UNSPECIFIED);
        };
        if gtk.key.len() != self.group.key_len() {
            return failed(REASON_UNSPECIFIED);
        }

        let mut events = Vec::with_capacity(4);
        // Message 4: Secure as in message 3, no nonce, no key data, sent before the key changes.
        let info = self.akm.version() | KEY_INFO_PAIRWISE | KEY_INFO_MIC | KEY_INFO_SECURE;
        let protected = self.pairwise_installed();
        if let Some(ptk) = &self.ptk {
            let pdu = self.build_key(info, &key.replay, &[0; 32], &[], ptk.kck());
            events.push(WpaEvent::Tx { pdu, protected });
        }
        // Install only what is not already in: a retransmitted message 3 must not reset the packet numbers.
        if let Some(tk) = self.ptk.as_ref().map(Ptk::tk)
            && self.installed_tk.as_ref() != Some(&tk)
        {
            self.installed_tk = Some(tk.clone());
            events.push(WpaEvent::InstallPairwise(tk));
        }
        // The group key goes in before the port opens, so the first broadcast reply (DHCP) can be read.
        self.install_gtk(gtk, key, &mut events);
        if !self.authorized {
            self.authorized = true;
            events.push(WpaEvent::Authorized);
        }
        self.renew_snonce = true;
        events
    }

    fn on_group1(&mut self, key: &EapolKey<'_>) -> Vec<WpaEvent> {
        // Group keys only over a PTK that message 3 has confirmed: a temporary PTK does not count here.
        if !self.authorized || !self.verify_current_only(key) {
            return Vec::new();
        }
        let Some(plain) = self.decrypt_key_data(key) else {
            return Vec::new();
        };
        let (_, gtk) = parse_key_data(plain.expose());
        let Some(gtk) = gtk else {
            return Vec::new();
        };
        if gtk.key.len() != self.group.key_len() {
            return Vec::new();
        }
        let mut events = Vec::with_capacity(2);
        self.install_gtk(gtk, key, &mut events);
        // Group message 2: Secure and MIC set, no pairwise bit, no key data.
        let info = self.akm.version() | KEY_INFO_MIC | KEY_INFO_SECURE;
        if let Some(ptk) = &self.ptk {
            let pdu = self.build_key(info, &key.replay, &[0; 32], &[], ptk.kck());
            events.push(WpaEvent::Tx {
                pdu,
                protected: true,
            });
        }
        events
    }

    fn verify_current_only(&mut self, key: &EapolKey<'_>) -> bool {
        let ok = self
            .ptk
            .as_ref()
            .is_some_and(|ptk| verify_mic(self.akm, ptk.kck(), key.pdu));
        if ok {
            self.rx_replay = Some(key.replay);
        }
        ok
    }

    fn install_gtk(&mut self, gtk: Gtk, key: &EapolKey<'_>, events: &mut Vec<WpaEvent>) {
        if let Some(old) = &self.installed_gtk
            && old.idx == gtk.idx
            && old.key == gtk.key
        {
            return;
        }
        let wire = match self.group {
            GroupCipher::Ccmp => gtk.key.clone(),
            GroupCipher::Tkip => {
                // wpa_supplicant_install_gtk: "Swap Tx/Rx keys for Michael MIC".
                let g = gtk.key.expose();
                let mut swapped = vec![0u8; 32];
                swapped[..16].copy_from_slice(&g[..16]);
                swapped[16..24].copy_from_slice(&g[24..32]);
                swapped[24..32].copy_from_slice(&g[16..24]);
                SecretVec::new(swapped)
            }
        };
        events.push(WpaEvent::InstallGroup {
            idx: gtk.idx,
            key: wire,
            cipher: self.group,
            rsc: Self::rsc_of(key),
        });
        self.installed_gtk = Some(gtk);
    }

    /// A Michael MIC failure seen by the hardware: the report frame `wpa_sm_key_request(sm, 1, pairwise)` sends.
    /// `None` before there is a PTK to sign it with.
    pub fn mic_failure_report(&mut self, pairwise: bool) -> Option<Vec<u8>> {
        self.ptk.as_ref()?;
        let mut info =
            self.akm.version() | KEY_INFO_REQUEST | KEY_INFO_ERROR | KEY_INFO_SECURE | KEY_INFO_MIC;
        if pairwise {
            info |= KEY_INFO_PAIRWISE;
        }
        let replay = self.request_counter.to_be_bytes();
        self.request_counter += 1;
        let ptk = self.ptk.as_ref()?;
        Some(self.build_key(info, &replay, &[0; 32], &[], ptk.kck()))
    }

    /// Builds an EAPOL-Key PDU and signs it.
    fn build_key(
        &self,
        info: u16,
        replay: &[u8; 8],
        nonce: &[u8; 32],
        data: &[u8],
        kck: &[u8],
    ) -> Vec<u8> {
        let mut pdu = vec![0u8; EAPOL_KEY_HDR_LEN + data.len()];
        pdu[0] = 1; // wpa_supplicant's default eapol_version: 802.1X-2001, which every AP takes.
        pdu[1] = EAPOL_KEY;
        pdu[2..4].copy_from_slice(&((EAPOL_KEY_HDR_LEN - 4 + data.len()) as u16).to_be_bytes());
        pdu[4] = KEY_DESC_RSN;
        pdu[OFF_KEY_INFO..OFF_KEY_INFO + 2].copy_from_slice(&info.to_be_bytes());
        // Key Length stays 0: wpa_supplicant's RSN messages 2 and 4 and group message 2 all send 0.
        pdu[OFF_REPLAY..OFF_REPLAY + 8].copy_from_slice(replay);
        pdu[OFF_NONCE..OFF_NONCE + 32].copy_from_slice(nonce);
        pdu[OFF_DATA_LEN..OFF_DATA_LEN + 2].copy_from_slice(&(data.len() as u16).to_be_bytes());
        pdu[EAPOL_KEY_HDR_LEN..].copy_from_slice(data);
        let mic = compute_mic(self.akm, kck, &pdu);
        pdu[OFF_MIC..OFF_MIC + 16].copy_from_slice(&mic);
        pdu
    }
}

impl Drop for Supplicant {
    fn drop(&mut self) {
        wipe(&mut self.snonce);
    }
}

impl fmt::Debug for Supplicant {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Supplicant")
            .field("akm", &self.akm)
            .field("group", &self.group)
            .field("authorized", &self.authorized)
            .finish_non_exhaustive()
    }
}
