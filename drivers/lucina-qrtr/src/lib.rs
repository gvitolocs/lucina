//! Qualcomm IPC router (QRTR) for the in-kernel Wi-Fi bring-up of the Galaxy Book Go: the packet format of
//! Linux's `net/qrtr`, and a small router with a name service to sit between the modem's `IPCRTR` channel and
//! the local QMI clients. `no_std` (with `alloc`), no `unsafe`, no I/O, no timers.
//!
//! On this machine the modem (node 0) registers WLFW as QMI service 0x45, and looks up pd-mapper (64),
//! tqftpserv (4096) and rmtfs (14) on the host.
//!
//! * [`packet`]: [`packet::decode`] is the receive side of `qrtr_endpoint_post()` (v1 and v2 headers, the
//!   length, padding, control-packet and destination checks); [`packet::encode_v1`] is the transmit side of
//!   `qrtr_node_enqueue()`; [`packet::CtrlPkt`] is `struct qrtr_ctrl_pkt`; the type numbers, the node and port
//!   constants and the flow-control marks are `qrtr.h` and `af_qrtr.c`.
//! * [`router`]: [`router::Router`], one endpoint and local ports, the name service of `ns.c`, and the flow
//!   control of `qrtr_tx_wait()` and `qrtr_tx_resume()`. Bytes in with [`router::Router::handle_rx`], bytes out
//!   with [`router::Router::poll_tx`], what local ports see from [`router::Router::poll_event`].
//!
//! Packet bytes were checked against the C struct layouts and the packet building of Linux (`oracle/`, whose
//! output is in `tests/fixtures/`). The router's logic follows the C by reading it; no Linux code runs
//! against it, and nothing here has been tried against the real modem.

#![no_std]
#![forbid(unsafe_code)]

extern crate alloc;

pub mod packet;
pub mod router;
