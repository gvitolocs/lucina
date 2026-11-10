//! The AX88179 as a `lucina-usbnet` backend: what the kernel's USB network glue drives.
//!
//! The link comes up through [`Ax88179::poll_link_timed`] (the glue polls about once a second) instead of the
//! blocking [`Ax88179::start`], because the USB thread also serves keyboards and disks.

use alloc::format;
use alloc::string::String;

use lucina_usbnet::{BackendError, EthernetBackend};

use crate::driver::{Ax88179, Error, Link, Speed};
use crate::frame::MAX_RX_FRAME;
use crate::transport::{TransportError, UsbTransport};

fn backend_error(error: Error) -> BackendError {
    match error {
        Error::Transport(TransportError::Disconnected) => BackendError::Gone,
        _ => BackendError::Failed,
    }
}

/// `1000/full, usb Super, medium 0x013b, bulkin 07 4f 00 12 ff`: what section 4 of the test plan reads.
pub fn link_text(link: &Link) -> String {
    let speed = match link.speed {
        Speed::Mbps10 => 10,
        Speed::Mbps100 => 100,
        Speed::Mbps1000 => 1000,
    };
    let b = link.bulkin;
    format!(
        "{}/{}, usb {:?}, medium {:#06x}, bulkin {:02x} {:02x} {:02x} {:02x} {:02x}",
        speed,
        if link.full_duplex { "full" } else { "half" },
        link.usb,
        link.medium,
        b[0],
        b[1],
        b[2],
        b[3],
        b[4]
    )
}

impl<T: UsbTransport> EthernetBackend for Ax88179<T> {
    fn mac_addr(&self) -> [u8; 6] {
        Ax88179::mac_addr(self)
    }

    fn send(&mut self, frame: &[u8]) -> Result<(), BackendError> {
        Ax88179::send(self, frame).map_err(backend_error)
    }

    fn receive(&mut self, out: &mut [u8]) -> Result<Option<usize>, BackendError> {
        // No bulk IN before the link is configured: the transfer must be the size of the link's bulk-in queue
        // (24 KiB at high speed, 26 KiB at 100 Mbit/s), or a full batch would be split over two transfers.
        if self.link().is_none() {
            return Ok(None);
        }
        Ax88179::receive(self, out).map_err(backend_error)
    }

    fn max_frame_len(&self) -> usize {
        // The MAC runs at MTU 1500 (no JUMBO_EN), but the RX header can describe more; take what it can.
        MAX_RX_FRAME
    }

    fn poll_link(&mut self, elapsed_ms: u32) -> Result<bool, BackendError> {
        self.poll_link_timed(elapsed_ms)
            .map(|link| link.is_some())
            .map_err(backend_error)
    }

    fn link_text(&self) -> String {
        match self.link() {
            Some(link) => link_text(&link),
            None => String::from("down"),
        }
    }

    fn stats_text(&self) -> String {
        let s = self.stats();
        format!(
            "tx_frames {} rx_frames {} rx_transfers {} rx_frame_errors {} rx_batch_errors {}",
            s.tx_frames, s.rx_frames, s.rx_transfers, s.rx_frame_errors, s.rx_batch_errors
        )
    }
}
