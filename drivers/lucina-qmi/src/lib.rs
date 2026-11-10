//! Qualcomm QMI for the in-kernel Wi-Fi bring-up of the Galaxy Book Go (WCN3990 on SC7180): the wire codec of
//! Linux's `qmi_encdec.c`, the WLFW v01 messages `ath10k/qmi.c` uses, and the client that sequences them.
//! `no_std` (with `alloc`), no `unsafe`, no I/O, no timers; the kernel side supplies a QRTR socket and a clock.
//!
//! * [`codec`]: [`codec::Header`] (`struct qmi_header`), [`codec::Writer`] and [`codec::Body`]
//!   (`qmi_encode()`), [`codec::Tlvs`] and [`codec::Cursor`] (`qmi_decode()`), [`codec::Message`] (what a
//!   `struct qmi_elem_info` table plus its C struct is), [`codec::encode_message`] and
//!   [`codec::decode_message`] (`qmi_encode_message()` and `qmi_decode_message()`). Decoding never panics on
//!   hostile input; every truncation is an [`codec::Error`].
//! * [`wlfw`]: service 0x45 version 1, every message id, the limits, and the typed messages of the bring-up:
//!   indication register, host capabilities (also the 8-bit quirk form), MSA info and ready, capabilities,
//!   board data download, calibration report, WLAN config and mode, firmware log mode, and the FW_READY and
//!   MSA_READY indications.
//! * [`client`]: [`client::WlfwClient`], the order of `ath10k_qmi_event_server_arrive()`,
//!   `ath10k_qmi_event_msa_ready()`, `ath10k_qmi_event_fw_ready_ind()` and `ath10k_qmi_wlan_enable()` as a
//!   machine from [`client::Input`] to [`client::Output`].
//!
//! The wire bytes were checked against Linux's own `qmi_encdec.c` and `qmi_wlfw_v01.c`, compiled on a host
//! (`oracle/`, whose output is in `tests/fixtures/`). Nothing here has been tried against the real firmware.

#![no_std]
#![forbid(unsafe_code)]

extern crate alloc;

pub mod client;
pub mod codec;
pub mod wlfw;
