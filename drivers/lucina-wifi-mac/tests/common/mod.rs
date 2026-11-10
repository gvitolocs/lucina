//! A model AP. It does the AP side of 802.11-2020 12.7.6 from its own offsets and its own key derivation, sharing only
//! the vector-tested primitives with the crate. It is a test partner written from the standard: it is not hostapd and
//! not a real AP.

#![allow(dead_code)]

use lucina_wifi_mac::crypto::cmac::aes128_cmac;
use lucina_wifi_mac::crypto::hash::{hmac_sha1, pbkdf2_hmac_sha1};
use lucina_wifi_mac::crypto::keywrap::aes_wrap;
use lucina_wifi_mac::crypto::prf::{kdf_sha256, prf_sha1};
use lucina_wifi_mac::{Credentials, HwCaps, Output, RxFrame, RxMeta, Station};

pub const AP: [u8; 6] = [0x02, 0xaa, 0xbb, 0xcc, 0xdd, 0x01];
pub const STA: [u8; 6] = [0x02, 0x11, 0x22, 0x33, 0x44, 0x55];
pub const SSID: &[u8] = b"model-net";
pub const PASSPHRASE: &[u8] = b"correct horse battery";
pub const FREQ: u16 = 2437;

pub const RSN_PSK_CCMP: [u8; 22] = [
    0x30, 20, 1, 0, 0x00, 0x0f, 0xac, 4, 1, 0, 0x00, 0x0f, 0xac, 4, 1, 0, 0x00, 0x0f, 0xac, 2, 0, 0,
];

/// Which AKM the model AP runs.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ApAkm {
    Psk,
    PskSha256,
}

/// A basic HT capability on the AP's side: 2 streams, 20/40, SGI.
pub fn ht_cap_ie() -> Vec<u8> {
    let mut ie = vec![45, 26];
    ie.extend_from_slice(&(0x0002u16 | 0x0020 | 0x0040 | 0x000c).to_le_bytes());
    ie.push(0x03 | (5 << 2));
    ie.extend_from_slice(&[0xff, 0xff, 0, 0, 0, 0, 0, 0, 0, 0]);
    ie.extend_from_slice(&[0; 2]);
    ie.push(0x01);
    ie.extend_from_slice(&[0; 3 + 2 + 4 + 1]);
    assert_eq!(ie.len(), 28);
    ie
}

/// HT operation: primary channel, secondary offset bits, width-any bit.
pub fn ht_oper_ie(primary: u8, offset_bits: u8, width_any: bool) -> Vec<u8> {
    let mut ie = vec![
        61,
        22,
        primary,
        offset_bits | if width_any { 0x04 } else { 0 },
    ];
    ie.extend_from_slice(&[0; 20]);
    ie
}

/// A WMM parameter element with distinct values per AC.
pub fn wmm_param_ie() -> Vec<u8> {
    let mut ie = vec![221, 24, 0x00, 0x50, 0xf2, 2, 1, 1, 0x80, 0];
    for (aci, aifsn, ecw, txop) in [
        (0u8, 3u8, 0xa4u8, 0u16),
        (1, 7, 0xa4, 0),
        (2, 2, 0x43, 94),
        (3, 2, 0x32, 47),
    ] {
        ie.push((aci << 5) | aifsn);
        ie.push(ecw);
        ie.extend_from_slice(&txop.to_le_bytes());
    }
    assert_eq!(ie.len(), 26);
    ie
}

/// A beacon or probe response with arbitrary extra elements.
pub fn beacon(
    subtype_bits: u8,
    bssid: &[u8; 6],
    ssid: &[u8],
    channel: u8,
    capability: u16,
    extra: &[Vec<u8>],
) -> Vec<u8> {
    let mut f = vec![subtype_bits, 0, 0, 0];
    f.extend_from_slice(if subtype_bits == 0x80 {
        &[0xff; 6]
    } else {
        &STA
    });
    f.extend_from_slice(bssid);
    f.extend_from_slice(bssid);
    f.extend_from_slice(&[0, 0]);
    f.extend_from_slice(&[0; 8]);
    f.extend_from_slice(&100u16.to_le_bytes());
    f.extend_from_slice(&capability.to_le_bytes());
    f.extend_from_slice(&[0, ssid.len() as u8]);
    f.extend_from_slice(ssid);
    if channel < 14 {
        f.extend_from_slice(&[1, 8, 0x82, 0x84, 0x8b, 0x96, 0x0c, 0x12, 0x18, 0x24]);
        f.extend_from_slice(&[50, 4, 0x30, 0x48, 0x60, 0x6c]);
        f.extend_from_slice(&[3, 1, channel]);
    } else {
        f.extend_from_slice(&[1, 8, 0x8c, 0x12, 0x98, 0x24, 0xb0, 0x48, 0x60, 0x6c]);
    }
    f.extend_from_slice(&[5, 4, 0, 2, 0, 0]);
    for e in extra {
        f.extend_from_slice(e);
    }
    f
}

pub struct ModelAp {
    pub pmk: [u8; 32],
    pub akm: ApAkm,
    pub group_suite: u32,
    pub rsn_ie: Vec<u8>,
    pub gtk: Vec<u8>,
    pub gtk_idx: u8,
    pub anonce: [u8; 32],
    pub snonce: Option<[u8; 32]>,
    pub replay: u64,
    pub gtk_rsc: [u8; 6],
}

impl ModelAp {
    pub fn new(passphrase: &[u8]) -> Self {
        let mut pmk = [0u8; 32];
        pbkdf2_hmac_sha1(passphrase, SSID, 4096, &mut pmk);
        Self {
            pmk,
            akm: ApAkm::Psk,
            group_suite: 0x000f_ac04,
            rsn_ie: RSN_PSK_CCMP.to_vec(),
            gtk: vec![0x47; 16],
            gtk_idx: 1,
            anonce: [0xa5; 32],
            snonce: None,
            replay: 0,
            gtk_rsc: [1, 2, 3, 4, 5, 6],
        }
    }

    pub fn beacon_frame(&self) -> Vec<u8> {
        beacon(
            0x80,
            &AP,
            SSID,
            6,
            0x0411,
            std::slice::from_ref(&self.rsn_ie),
        )
    }

    pub fn auth_resp(&self, status: u16) -> Vec<u8> {
        let mut f = mgmt(0xb0, &STA, &AP);
        f.extend_from_slice(&0u16.to_le_bytes());
        f.extend_from_slice(&2u16.to_le_bytes());
        f.extend_from_slice(&status.to_le_bytes());
        f
    }

    pub fn assoc_resp(&self, status: u16, aid: u16) -> Vec<u8> {
        let mut f = mgmt(0x10, &STA, &AP);
        f.extend_from_slice(&0x0411u16.to_le_bytes());
        f.extend_from_slice(&status.to_le_bytes());
        f.extend_from_slice(&(aid | 0xc000).to_le_bytes());
        f.extend_from_slice(&[1, 8, 0x82, 0x84, 0x8b, 0x96, 0x0c, 0x12, 0x18, 0x24]);
        f.extend_from_slice(&[50, 4, 0x30, 0x48, 0x60, 0x6c]);
        f
    }

    pub fn deauth(&self, reason: u16) -> Vec<u8> {
        let mut f = mgmt(0xc0, &STA, &AP);
        f.extend_from_slice(&reason.to_le_bytes());
        f
    }

    /// The PTK from the AP's side: its own byte order logic, not the crate's.
    pub fn ptk(&self) -> [u8; 48] {
        let snonce = self.snonce.expect("message 2 came first");
        let mut data = Vec::new();
        data.extend_from_slice(AP.min(STA).as_slice());
        data.extend_from_slice(AP.max(STA).as_slice());
        if self.anonce.as_slice() < snonce.as_slice() {
            data.extend_from_slice(&self.anonce);
            data.extend_from_slice(&snonce);
        } else {
            data.extend_from_slice(&snonce);
            data.extend_from_slice(&self.anonce);
        }
        let mut out = [0u8; 48];
        match self.akm {
            ApAkm::Psk => prf_sha1(&self.pmk, b"Pairwise key expansion", &data, &mut out),
            ApAkm::PskSha256 => kdf_sha256(&self.pmk, b"Pairwise key expansion", &data, &mut out),
        }
        out
    }

    pub fn tk(&self) -> [u8; 16] {
        self.ptk()[32..48].try_into().unwrap()
    }

    fn version(&self) -> u16 {
        match self.akm {
            ApAkm::Psk => 2,
            ApAkm::PskSha256 => 3,
        }
    }

    fn mic(&self, pdu: &[u8]) -> [u8; 16] {
        let ptk = self.ptk();
        match self.akm {
            ApAkm::Psk => hmac_sha1(&ptk[..16], pdu)[..16].try_into().unwrap(),
            ApAkm::PskSha256 => aes128_cmac(ptk[..16].try_into().unwrap(), pdu),
        }
    }

    /// Lays out an EAPOL-Key PDU by its offsets in the standard.
    fn key_pdu(
        &self,
        info: u16,
        key_len: u16,
        replay: u64,
        nonce: &[u8; 32],
        rsc: &[u8; 6],
        data: &[u8],
    ) -> Vec<u8> {
        let mut p = vec![2u8, 3];
        p.extend_from_slice(&((95 + data.len()) as u16).to_be_bytes());
        p.push(2);
        p.extend_from_slice(&info.to_be_bytes());
        p.extend_from_slice(&key_len.to_be_bytes());
        p.extend_from_slice(&replay.to_be_bytes());
        p.extend_from_slice(nonce);
        p.extend_from_slice(&[0; 16]);
        p.extend_from_slice(rsc);
        p.extend_from_slice(&[0; 2]);
        p.extend_from_slice(&[0; 8]);
        p.extend_from_slice(&[0; 16]);
        p.extend_from_slice(&(data.len() as u16).to_be_bytes());
        p.extend_from_slice(data);
        assert_eq!(p.len(), 99 + data.len());
        p
    }

    /// Message 1: Pairwise and Ack.
    pub fn msg1(&mut self) -> Vec<u8> {
        self.replay += 1;
        let info = self.version() | 0x0008 | 0x0080;
        let p = self.key_pdu(info, 16, self.replay, &self.anonce.clone(), &[0; 6], &[]);
        nwifi(&p, &STA)
    }

    /// Reads a station's message 2 and takes its SNonce. Returns false if its MIC does not check out.
    pub fn take_msg2(&mut self, frame: &[u8]) -> bool {
        let e = &frame[32..];
        assert_eq!(e[1], 3);
        let info = u16::from_be_bytes([e[5], e[6]]);
        assert_eq!(info, self.version() | 0x0008 | 0x0100, "message 2 key info");
        let mut s = [0u8; 32];
        s.copy_from_slice(&e[17..49]);
        self.snonce = Some(s);
        self.check_mic(e)
    }

    /// True if the MIC on this EAPOL PDU is right under the current PTK.
    pub fn check_mic(&self, e: &[u8]) -> bool {
        let mut copy = e.to_vec();
        copy[81..97].fill(0);
        self.mic(&copy) == e[81..97]
    }

    fn key_data(&self, include_rsn: bool) -> Vec<u8> {
        let mut plain = Vec::new();
        if include_rsn {
            plain.extend_from_slice(&self.rsn_ie);
        }
        plain.extend_from_slice(&[
            0xdd,
            (6 + self.gtk.len()) as u8,
            0x00,
            0x0f,
            0xac,
            1,
            self.gtk_idx,
            0,
        ]);
        plain.extend_from_slice(&self.gtk);
        // Padding per 12.7.2: 0xdd then zeros up to a multiple of 8, at least 16 bytes.
        if plain.len() % 8 != 0 || plain.len() < 16 {
            plain.push(0xdd);
            while plain.len() % 8 != 0 || plain.len() < 16 {
                plain.push(0);
            }
        }
        plain
    }

    fn wrap(&self, plain: &[u8]) -> Vec<u8> {
        let ptk = self.ptk();
        aes_wrap(ptk[16..32].try_into().unwrap(), plain).unwrap()
    }

    /// Message 3: Pairwise, Install, Ack, MIC, Secure, Encrypted Key Data, with the RSN element and GTK KDE.
    pub fn msg3(&mut self) -> Vec<u8> {
        self.msg3_with(self.rsn_ie.clone())
    }

    pub fn msg3_with(&mut self, rsn: Vec<u8>) -> Vec<u8> {
        self.replay += 1;
        let saved = core::mem::replace(&mut self.rsn_ie, rsn);
        let data = self.wrap(&self.key_data(true));
        self.rsn_ie = saved;
        let info = self.version() | 0x0008 | 0x0040 | 0x0080 | 0x0100 | 0x0200 | 0x1000;
        let mut p = self.key_pdu(
            info,
            16,
            self.replay,
            &self.anonce.clone(),
            &self.gtk_rsc.clone(),
            &data,
        );
        let mic = self.mic(&p);
        p[81..97].copy_from_slice(&mic);
        nwifi(&p, &STA)
    }

    /// Group message 1: Ack, MIC, Secure, Encrypted Key Data, GTK KDE only. Sent protected in reality; the model does
    /// not encrypt, the hardware would.
    pub fn group1(&mut self) -> Vec<u8> {
        self.replay += 1;
        let data = self.wrap(&self.key_data(false));
        let info = self.version() | 0x0080 | 0x0100 | 0x0200 | 0x1000;
        let mut p = self.key_pdu(info, 0, self.replay, &[0; 32], &self.gtk_rsc.clone(), &data);
        let mic = self.mic(&p);
        p[81..97].copy_from_slice(&mic);
        nwifi(&p, &STA)
    }
}

pub fn mgmt(subtype_bits: u8, da: &[u8; 6], bssid: &[u8; 6]) -> Vec<u8> {
    let mut f = vec![subtype_bits, 0, 0, 0];
    f.extend_from_slice(da);
    f.extend_from_slice(bssid);
    f.extend_from_slice(bssid);
    f.extend_from_slice(&[0, 0]);
    f
}

/// A From-DS native-WiFi frame carrying an EAPOL PDU.
pub fn nwifi(eapol: &[u8], da: &[u8; 6]) -> Vec<u8> {
    let mut f = vec![0x08, 0x02, 0, 0];
    f.extend_from_slice(da);
    f.extend_from_slice(&AP);
    f.extend_from_slice(&AP);
    f.extend_from_slice(&[0, 0]);
    f.extend_from_slice(&[0xaa, 0xaa, 0x03, 0, 0, 0, 0x88, 0x8e]);
    f.extend_from_slice(eapol);
    f
}

/// A From-DS native-WiFi frame carrying IPv4.
pub fn nwifi_ip(payload: &[u8]) -> Vec<u8> {
    let mut f = vec![0x08, 0x02, 0, 0];
    f.extend_from_slice(&STA);
    f.extend_from_slice(&AP);
    f.extend_from_slice(&[0x02, 0xee, 0xee, 0xee, 0xee, 0xee]);
    f.extend_from_slice(&[0, 0]);
    f.extend_from_slice(&[0xaa, 0xaa, 0x03, 0, 0, 0, 0x08, 0x00]);
    f.extend_from_slice(payload);
    f
}

pub fn ethernet_ip(payload: &[u8]) -> Vec<u8> {
    let mut f = vec![0x02, 0xee, 0xee, 0xee, 0xee, 0xee];
    f.extend_from_slice(&STA);
    f.extend_from_slice(&[0x08, 0x00]);
    f.extend_from_slice(payload);
    f
}

pub fn hw_basic() -> HwCaps {
    HwCaps {
        vht: None,
        band_2ghz: true,
        band_5ghz: true,
        ht: None,
        reports_tx_ack: true,
        spectrum_mgmt: true,
        ht40: true,
        listen_interval: 5,
        connection_monitor: false,
    }
}

pub fn drain(sta: &mut Station) -> Vec<Output> {
    let mut v = Vec::new();
    while let Some(o) = sta.poll_output() {
        v.push(o);
    }
    v
}

pub fn only_tx_data(sta: &mut Station) -> Vec<u8> {
    let out = drain(sta);
    assert_eq!(out.len(), 1, "expected one frame, got {out:?}");
    match out.into_iter().next() {
        Some(Output::TxData { frame, eapol: true }) => frame,
        o => panic!("expected an EAPOL frame, got {o:?}"),
    }
}

pub fn rx(sta: &mut Station, frame: &[u8]) {
    sta.on_data_rx(RxFrame::NativeWifi(frame), RxMeta { decrypted: false });
}

pub fn rx_secure(sta: &mut Station, frame: &[u8]) {
    sta.on_data_rx(RxFrame::NativeWifi(frame), RxMeta { decrypted: true });
}

/// A station that has heard the AP's beacon and started connecting.
pub fn connecting(ap: &ModelAp, passphrase: &[u8]) -> Station {
    let mut sta = Station::new(hw_basic(), STA);
    sta.on_mgmt_rx(&ap.beacon_frame(), -50, FREQ, 0);
    sta.connect(
        SSID,
        Credentials::passphrase(passphrase).unwrap(),
        [0x5a; 32],
        0,
    )
    .unwrap();
    sta
}

/// Runs auth and assoc to the point where the handshake is due.
pub fn associated(ap: &ModelAp, passphrase: &[u8]) -> Station {
    let mut sta = connecting(ap, passphrase);
    drain(&mut sta);
    sta.on_mgmt_rx(&ap.auth_resp(0), -50, FREQ, 1);
    drain(&mut sta);
    sta.on_mgmt_rx(&ap.assoc_resp(0, 5), -50, FREQ, 2);
    drain(&mut sta);
    sta
}

/// Runs the whole 4-way, checking the AP's view of it. The station ends Connected and its outputs drained.
pub fn connected(ap: &mut ModelAp) -> Station {
    let mut sta = associated(ap, PASSPHRASE);
    let m1 = ap.msg1();
    rx(&mut sta, &m1);
    let m2 = only_tx_data(&mut sta);
    assert!(ap.take_msg2(&m2));
    let m3 = ap.msg3();
    rx(&mut sta, &m3);
    drain(&mut sta);
    sta
}
