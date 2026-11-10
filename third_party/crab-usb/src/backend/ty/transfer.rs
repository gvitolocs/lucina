#[cfg(any(kmod, umod))]
use alloc::vec::Vec;

#[cfg(any(kmod, umod))]
pub use usb_if::endpoint::TransferKind;

#[cfg_attr(umod, derive(Clone))]
#[cfg(any(kmod, umod))]
pub struct Transfer {
    pub kind: TransferKind,
    pub direction: usb_if::transfer::Direction,
    #[cfg(kmod)]
    pub mapping: Option<dma_api::StreamingMap<u8>>,
    /// The caller's buffer, which `mapping` points into: it goes wherever the TD goes (freed with it, or into the
    /// quarantine with it), so it lives as long as anything may still read or write it. Declared after `mapping`, so
    /// the mapping is unmapped before the buffer is freed.
    #[cfg(kmod)]
    pub owned: Vec<u8>,
    #[cfg(umod)]
    pub buffer: Option<(std::ptr::NonNull<u8>, usize)>,
    pub transfer_len: usize,
    pub iso_packet_actual_lengths: Vec<usize>,
}
