// Derived from Linux (v7.1) drivers/rpmsg/qcom_glink_smem.c, drivers/rpmsg/qcom_glink_native.c,
// drivers/rpmsg/qcom_glink_native.h and, for the channel name it binds, net/qrtr/smd.c. All GPL-2.0; this crate is a
// re-implementation of their behaviour in Rust and stays under the same licence. See the notice at the top of each
// module for the exact files.

//! Qualcomm GLINK over SMEM for the in-kernel Wi-Fi bring-up of the Galaxy Book Go (SC7180): the WCN3990 firmware
//! runs on the modem DSP, and the kernel reaches it with QRTR over GLINK over SMEM. This crate is the lower two of
//! those three, following Linux's `qcom_glink_smem.c` and `qcom_glink_native.c`, with the channel QRTR binds
//! (`IPCRTR`, `net/qrtr/smd.c`) as the one it is built around.
//!
//! * [`Shared`] is word access to the shared memory and [`Doorbell`] the interrupt to the remote (on SC7180 the
//!   APCS IPC register, bit 12 for the modem edge). Nothing here touches hardware; the kernel integration
//!   implements both, and also finds or allocates the three SMEM items ([`fifo`] says which is whose).
//! * [`Fifo`] is the pair of rings: `glink_smem_rx_avail`, `_rx_peek`, `_rx_advance`, `_tx_avail`, `_tx_write`, with
//!   every index from the remote validated, and word-only accesses.
//! * [`wire`] is the command table and the frame layouts of `struct glink_msg`.
//! * [`Glink`] is the protocol as a non-blocking state machine: version negotiation, the open/close handshakes,
//!   receive intents and fragmented `TX_DATA`. [`Glink::poll`] is the receive interrupt; [`Glink::tick`] is the
//!   caller's clock for the timeouts the C has. Everything it learns comes out as an [`Event`].
//!
//! Wiring it into QRTR: construct a [`Glink`], call `poll` from the interrupt and after the doorbell, call `tick`
//! with a millisecond clock now and then (see [`Glink::next_deadline`]), turn [`Event::ChannelOpened`] for `IPCRTR`
//! into a QRTR endpoint, [`Event::Data`] into `qrtr_endpoint_post`, and the QRTR transmit hook into [`Glink::send`].
//!
//! Nothing here has run against a modem. The ring sizes, the order in which the modem allocates its SMEM items and
//! whether it behaves like Linux's reading of the protocol are all unknown until the laptop runs it; the tests
//! check this code against a model of the remote written from the C, which is not the same thing.

#![no_std]
#![forbid(unsafe_code)]

extern crate alloc;

pub mod fifo;
mod glink;
pub mod wire;

pub use fifo::{
    Barrier, DESCRIPTOR_SIZE, Doorbell, Fault, Fifo, Layout, LayoutError, Region,
    SMEM_ITEM_DESCRIPTOR, SMEM_ITEM_FIFO_0, SMEM_ITEM_FIFO_1, Shared, Side, TX_FIFO_SIZE,
};
pub use glink::{
    ChannelId, ChannelState, Config, Error, Event, Glink, INTENT_REQUEST_TIMEOUT_MS, LinkState,
    MAX_CHUNK, OPEN_TIMEOUT_MS, OpenFailure, ResetReason, SendFailure,
};

#[cfg(test)]
extern crate std;

#[cfg(test)]
mod tests;
