//! The RTL8152 as a `lucina-usbnet` backend: what the kernel's USB network glue drives.

use alloc::format;
use alloc::string::String;

use lucina_usbnet::{BackendError, EthernetBackend};

use crate::driver::{Error, Rtl8152};
use crate::frame::MAX_RX_FRAME;
use crate::transport::{TransportError, UsbTransport};

fn backend_error(error: Error) -> BackendError {
    match error {
        Error::Transport(TransportError::Disconnected) => BackendError::Gone,
        _ => BackendError::Failed,
    }
}

impl<T: UsbTransport> EthernetBackend for Rtl8152<T> {
    fn mac_addr(&self) -> [u8; 6] {
        Rtl8152::mac_addr(self)
    }

    fn send(&mut self, frame: &[u8]) -> Result<(), BackendError> {
        Rtl8152::send(self, frame).map_err(backend_error)
    }

    fn send_frames(
        &mut self,
        next: &mut dyn FnMut() -> Option<alloc::vec::Vec<u8>>,
        limit: usize,
    ) -> Result<usize, BackendError> {
        Rtl8152::send_frames(self, next, limit).map_err(backend_error)
    }

    fn receive(&mut self, out: &mut [u8]) -> Result<Option<usize>, BackendError> {
        // No bulk IN before the MAC is enabled (Linux: `rtl_start_rx` follows `enable` when the carrier comes up).
        if self.link().is_none() {
            return Ok(None);
        }
        Rtl8152::receive(self, out).map_err(backend_error)
    }

    fn max_frame_len(&self) -> usize {
        MAX_RX_FRAME
    }

    fn poll_link(&mut self, elapsed_ms: u32) -> Result<bool, BackendError> {
        Rtl8152::poll_link(self, elapsed_ms)
            .map(|link| link.is_some())
            .map_err(backend_error)
    }

    /// `RTL8152 VER_02, 100/full`: what section 4 of the test plan reads.
    fn link_text(&self) -> String {
        match (self.version(), self.link()) {
            (Some(version), Some(link)) => format!("{}, {}", version, link),
            _ => String::from("down"),
        }
    }

    fn stats_text(&self) -> String {
        let s = self.stats();
        format!(
            "tx_frames {} tx_transfers {} tx_refused {} rx_frames {} rx_transfers {} rx_frame_errors {} wait_timeouts {}",
            s.tx_frames,
            s.tx_transfers,
            s.tx_refused,
            s.rx_frames,
            s.rx_transfers,
            s.rx_frame_errors,
            s.wait_timeouts
        )
    }
}
