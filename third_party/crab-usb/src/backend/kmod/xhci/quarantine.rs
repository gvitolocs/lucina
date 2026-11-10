//! DMA memory the controller may still read or write: rings of endpoints whose slot could not be disabled, a slot's
//! device context, and the TDs of a controller that died without halting.
//!
//! Adapted from Linux, which frees these (`xhci_free_virt_device` after a failed Disable Slot, the URBs that
//! `xhci_hc_died` gives back): there the IOMMU turns a late DMA into a fault. On Juno (SC7180) the controller sits
//! on the SMMU in bypass, so device addresses are physical, and a late write lands in whatever reuses the frames
//! (page tables, heap), which resets the machine without a word. A leak is the lesser evil.

use alloc::{boxed::Box, sync::Arc, vec::Vec};
use core::mem;

use ax_sync::SpinLock;

/// One per controller, shared by its command ring, devices and endpoints, so it outlives any `Device`. Nothing is
/// ever taken out: this stack has no reset of a dead controller that would prove (HCH, then HCRST done) that it
/// stopped. Not even dropping the last handle frees what it holds.
#[derive(Clone, Default)]
pub(crate) struct DmaQuarantine(Arc<Kept>);

#[derive(Default)]
struct Kept(SpinLock<Vec<Box<dyn Send>>>);

impl Drop for Kept {
    fn drop(&mut self) {
        // The controller is not known to have stopped: forget, never free.
        mem::forget(mem::take(self.0.get_mut()));
    }
}

impl DmaQuarantine {
    /// Keeps `item` (and the DMA memory it owns) alive for good.
    pub(crate) fn keep(&self, item: impl Send + 'static) {
        self.0.0.lock().push(Box::new(item));
    }

    /// How many things are kept.
    pub(crate) fn len(&self) -> usize {
        self.0.0.lock().len()
    }
}
