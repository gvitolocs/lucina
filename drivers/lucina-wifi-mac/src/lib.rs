//! What mac80211 and wpa_supplicant do above ath10k, for one station on WPA2-PSK. `no_std` with `alloc`, no `unsafe`,
//! no external crates, no I/O and no clock: time comes in as a `now_ms` argument and everything the owner must do
//! comes out of [`Station::poll_output`] as an [`Output`], in order.
//!
//! The owner, a kernel thread beside `lucina-ath10k`, does this:
//!
//! ```text
//! let mut sta = Station::new(hw_caps, mac);
//! let req = sta.start_scan(None, now);          // run each req.legs entry as one WMI start_scan
//! sta.on_mgmt_rx(frame, rssi, freq, now);       // every management frame the driver receives
//! sta.scan_done();
//! sta.connect(b"ssid", creds, entropy, now)?;   // 32 random bytes from the kernel
//! loop {
//!     sta.on_mgmt_rx(..); sta.on_mgmt_tx_status(acked, now);
//!     sta.on_data_rx(RxFrame::NativeWifi(frame), RxMeta { decrypted }); sta.tx_ethernet(eth);
//!     sta.tick(now);
//!     while let Some(o) = sta.poll_output() { /* act on it */ }
//! }
//! ```
//!
//! # Modules
//!
//! * [`frame`], [`ie`]: headers, the fixed management frames, elements, RSN, WMM, HT.
//! * [`regulatory`]: the world domain cut by ath10k's channel table, beacon hints, Country as a restriction.
//! * [`scan`]: [`ScanResult`], the dedupe table, [`ScanRequest`] with probe IEs.
//! * [`mlme`], [`caps`]: mac80211's timing, the association request, and `ath10k_peer_assoc_h_*`'s results.
//! * [`wpa`]: the 4-way and group key handshakes. [`crypto`] is their SHA-1, AES and friends, written here.
//! * [`data`]: Ethernet II <-> native-WiFi, raw 802.11 with QoS, A-MSDU.
//! * [`station`]: the state machine that uses all of them.
//!
//! # Not here
//!
//! PMF: an AP that requires it is refused with [`ConnectError::PmfRequired`]. SAE, FT, roaming, power save, U-APSD,
//! and block-ack management (ath10k's firmware owns aggregation) are out. An AP on 80 MHz is joined with VHT, an AP on less as HT40 or HT20; 160 MHz is
//! not used. Action frames are ignored rather than rejected.
//!
//! # What has been proved
//!
//! Crypto: published vectors, and on a real public capture (`tests/fixtures/wpa-induction.txt`): the derived PTK
//! verifies every MIC in it, the supplicant's messages match byte for byte after accounting for the capturing client's
//! own two choices, and the TK decrypts the capture's CCMP data. MLME and data-path behaviour has met only a model AP
//! written for the tests and that one capture, and has never run on the WCN3990. No real AP has answered this code.

#![no_std]
#![forbid(unsafe_code)]

extern crate alloc;

pub mod caps;
pub mod crypto;
pub mod data;
pub mod frame;
pub mod ie;
pub mod mlme;
pub mod regulatory;
pub mod scan;
pub mod station;
pub mod wpa;

pub use caps::{HwCaps, PeerHt, PhyMode};
pub use frame::MacAddr;
pub use mlme::{ChannelWidth, JoinInfo, PeerInfo};
pub use regulatory::{Band, ChannelState, Regulatory};
pub use scan::{ScanChannel, ScanLeg, ScanRequest, ScanResult, ScanTable, Security};
pub use station::{
    ConnectError, Credentials, CredentialsError, DisconnectReason, Output, RxFrame, RxMeta,
    Station, StationState, Stats, TxError,
};
pub use wpa::GroupCipher;
