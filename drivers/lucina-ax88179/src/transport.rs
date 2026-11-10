//! What the driver needs from the USB host stack: one configured AX88179 (configuration 1, interface 0 claimed),
//! its default control pipe and its two bulk endpoints, plus a millisecond delay.
//!
//! The kernel's xHCI stack implements [`UsbTransport`] for a device whose IDs pass [`crate::matches`]. All transfers
//! are synchronous from the driver's point of view except [`UsbTransport::bulk_in`], which polls.

/// A vendor control request to the device (`bmRequestType` 0x40 for OUT, 0xc0 for IN: vendor, device recipient).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct VendorRequest {
    /// `bRequest`: [`crate::regs::ACCESS_MAC`] or [`crate::regs::ACCESS_PHY`].
    pub request: u8,
    pub value: u16,
    pub index: u16,
}

/// Why a transfer failed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TransportError {
    /// No completion within the transport's timeout (U-Boot uses 5 s for control and bulk transfers).
    Timeout,
    /// The endpoint stalled.
    Stall,
    /// The device was unplugged.
    Disconnected,
    /// Any other host controller error.
    Other,
}

pub trait UsbTransport {
    /// Vendor OUT request with `data` (may be empty). Returns the bytes the device accepted.
    fn control_out(&mut self, req: VendorRequest, data: &[u8]) -> Result<usize, TransportError>;

    /// Vendor IN request that reads up to `buf.len()` bytes. Returns the bytes received.
    fn control_in(&mut self, req: VendorRequest, buf: &mut [u8]) -> Result<usize, TransportError>;

    /// One bulk OUT transfer on endpoint [`crate::EP_BULK_OUT`], blocking until it completed. No zero-length packet
    /// is added: the frame header tells the device where each frame ends.
    fn bulk_out(&mut self, data: &[u8]) -> Result<usize, TransportError>;

    /// Polls bulk IN endpoint [`crate::EP_BULK_IN`]. `Ok(None)`: no transfer has completed yet. `Ok(Some(n))`: one
    /// transfer completed and its `n` bytes were copied to the start of `buf`.
    ///
    /// The transport keeps a transfer of `buf.len()` bytes in flight between calls (and queues the next one when it
    /// hands one over), so reception does not stop while the driver is not polling. A completed transfer may be any
    /// length up to `buf.len()`; the device ends it with a short packet.
    fn bulk_in(&mut self, buf: &mut [u8]) -> Result<Option<usize>, TransportError>;

    /// `wMaxPacketSize` of the bulk OUT endpoint: 1024 at SuperSpeed, 512 at high speed.
    fn bulk_out_max_packet(&self) -> u16;

    /// Waits at least `ms` milliseconds (only during probe and link setup).
    fn delay_ms(&mut self, ms: u32);
}

impl<T: UsbTransport + ?Sized> UsbTransport for &mut T {
    fn control_out(&mut self, req: VendorRequest, data: &[u8]) -> Result<usize, TransportError> {
        (**self).control_out(req, data)
    }
    fn control_in(&mut self, req: VendorRequest, buf: &mut [u8]) -> Result<usize, TransportError> {
        (**self).control_in(req, buf)
    }
    fn bulk_out(&mut self, data: &[u8]) -> Result<usize, TransportError> {
        (**self).bulk_out(data)
    }
    fn bulk_in(&mut self, buf: &mut [u8]) -> Result<Option<usize>, TransportError> {
        (**self).bulk_in(buf)
    }
    fn bulk_out_max_packet(&self) -> u16 {
        (**self).bulk_out_max_packet()
    }
    fn delay_ms(&mut self, ms: u32) {
        (**self).delay_ms(ms)
    }
}
