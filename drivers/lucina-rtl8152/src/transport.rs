//! What the driver needs from the USB host stack: one RTL8152 in its vendor configuration with the interface claimed
//! ([`crate::desc::find_vendor_function`]), its default control pipe and its two bulk endpoints, a millisecond delay
//! and a log line.

use alloc::vec::Vec;
use core::fmt;

/// A vendor control request to the device (`bmRequestType` 0x40 for OUT, 0xc0 for IN: vendor, device recipient).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct VendorRequest {
    /// `bRequest`: always [`crate::regs::REQ_REGS`].
    pub request: u8,
    /// `wValue`: the register address.
    pub value: u16,
    /// `wIndex`: the register space ([`crate::regs::Mcu::index`]) and, on writes, the byte enables.
    pub index: u16,
}

/// Why a transfer failed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TransportError {
    /// No completion within the transport's timeout.
    Timeout,
    /// The endpoint stalled.
    Stall,
    /// The device was unplugged.
    Disconnected,
    /// Any other host controller error.
    Other,
}

pub trait UsbTransport {
    /// Vendor OUT request with `data`. Returns the bytes the device accepted.
    fn control_out(&mut self, req: VendorRequest, data: &[u8]) -> Result<usize, TransportError>;

    /// Vendor IN request that reads up to `buf.len()` bytes. Returns the bytes received.
    fn control_in(&mut self, req: VendorRequest, buf: &mut [u8]) -> Result<usize, TransportError>;

    /// One bulk OUT transfer, handed over whole so that it is not copied again. The transport may queue it, and
    /// waits only a bounded time for room. No zero-length packet is added: the descriptor tells the chip where the
    /// frame ends (U-Boot, Linux and OpenBSD add none either).
    fn bulk_out(&mut self, data: Vec<u8>) -> Result<(), TransportError>;

    /// Whether [`UsbTransport::bulk_out`] would find room right now. Must not wait.
    fn bulk_out_ready(&mut self) -> Result<bool, TransportError> {
        Ok(true)
    }

    /// Polls the bulk IN endpoint. `Ok(None)`: no transfer has completed yet. `Ok(Some(n))`: one transfer completed
    /// with `n` bytes, now in [`UsbTransport::rx_data`]. The previous transfer's buffer goes back to the transport
    /// either way. Transfers stay queued between calls, so that reception does not stop while the driver is not
    /// polling.
    fn bulk_in(&mut self) -> Result<Option<usize>, TransportError>;

    /// The bytes of the transfer that [`UsbTransport::bulk_in`] last returned. The driver parses them in place.
    fn rx_data(&self) -> &[u8];

    /// `wMaxPacketSize` of the bulk OUT endpoint: 512 at high speed, 64 at full speed.
    fn bulk_max_packet(&self) -> u16;

    /// Waits at least `ms` milliseconds (only during bring-up and link changes).
    fn delay_ms(&mut self, ms: u32);

    /// One line for the kernel log: what the laptop test reads off the screen.
    fn log(&mut self, _message: fmt::Arguments<'_>) {}
}
