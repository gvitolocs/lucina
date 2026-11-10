//! The framing layers and copy engine rings of Linux's `ath10k` driver, for the one chip LucinaOS needs: the
//! Qualcomm WCN3990 Wi-Fi block of the SC7180 (Samsung Galaxy Book Go), which Linux drives through ath10k's `snoc`
//! bus (`drivers/net/wireless/ath/ath10k/snoc.c`, kernel 7.1). Host-tested only: no hardware, no real firmware.
//!
//! Every module names the Linux files it derives from and their licence (ISC for all of ath10k), and says in its
//! doc comment which Linux function each item follows. The tests do not trust this crate's transcription: the
//! `oracle/` directory holds C programs that include the real headers, or compile the real functions, with
//! stand-ins for the kernel API, and print what they produce into `tests/fixtures/`; the Rust tests assert equality
//! (`oracle/README.md` says how to regenerate them).
//!
//! * [`snoc_config`] is the bus configuration of `snoc.c`: the host and target copy engine tables, the
//!   service-to-pipe map and its lookup, the shadow register table, the interrupt layout and the register window.
//! * [`ce`] is `ce.c` for this chip: 64-bit descriptors, source and destination rings, the shadow-register send
//!   doorbell, read indices in host memory (RRI), ring initialisation and the interrupt enable/ack sequence, over
//!   two traits, [`ce::Regs`] (the 8 MiB register window) and [`ce::DescMem`] (shared ring memory with a barrier hook).
//! * [`htc`] is `htc.c`: header, control messages, trailer records, credit accounting and the handshake as a pure
//!   state machine.
//! * [`wmi_tlv`] is the WMI-TLV framing and the bring-up commands and events of `wmi-tlv.c`: TLV walking with the C's
//!   checks, service ready / ready / scan event parsers, and encoders for init (with the resource configuration),
//!   scan, regulatory domain, vdev, peer and management transmit commands.
//! * [`htt`] is the HTT framing of `htt_tx.c`/`htt_rx.c`: version request, rx ring setup, the 64-bit tx descriptor,
//!   frag bank, aggregation, the indications and the rx descriptor's layout.
//! * [`driver`] puts it together for one kernel thread as a station: bring-up in `ath10k_core_start`'s order, scan,
//!   join, keys, the data path, over a [`driver::Platform`] the kernel implements. A model firmware in
//!   `tests/driver.rs` runs a whole session through its rings.
//!
//! **What is proven and what is not.** The wire formats agree with Linux's own encoders. The driver runs against a
//! model written from reading the same C. Neither has met the firmware: nothing here has run on the chip.
//!
//! Not here: mac80211-level logic (`lucina-wifi-mac` has it), MLME, real DMA, QMI (the firmware loading side of SNOC), and
//! the parts of WMI and HTT that a station does not need. Everything that decodes bytes written by the firmware
//! or by the copy engine returns an error for malformed input and never panics; where the C trusts a length or an
//! index, the module doc says what this crate does instead.

#![no_std]
#![forbid(unsafe_code)]

extern crate alloc;

pub mod board;
pub mod ce;
pub mod driver;
pub mod htc;
pub mod htt;
pub mod snoc_config;
pub mod wmi_tlv;
