#![cfg_attr(not(feature = "umod"), no_std)]
extern crate alloc;
#[macro_use]
extern crate log;
#[macro_use]
extern crate anyhow;

use core::{
    ptr::NonNull,
    sync::atomic::{AtomicU32, AtomicU64, Ordering},
};

pub use usb_if;

#[macro_use]
mod _macros;

pub(crate) mod backend;
#[cfg(test)]
mod test_lock_runtime;
pub mod device;
pub mod err;
mod host;

pub use host::*;

#[allow(unused_imports)]
#[cfg(kmod)]
pub use crate::backend::kmod::*;
pub use crate::backend::ty::{
    Event,
    ep::{EndpointHandle, OwnedCompletion, OwnedRequest, SubmitError},
};

define_int_type!(BusAddr, u64);

pub type Mmio = NonNull<u8>;

/// Which LucinaOS revision of crab-usb 0.12.1 this is (see LUCINA-CHANGES.md). It exists only in this copy: `aster-usb`
/// uses it, so the kernel cannot compile against the crate from crates.io.
pub const LUCINA_REVISION: u32 = 5;

/// For testing recovery from an event off its ring, which real hardware does not do on demand: every Nth change of
/// endpoint between transfer events (0: none), the event gets the previous event's pointer, as a torn read of the event
/// ring would. A LucinaOS addition; see LUCINA-CHANGES.md.
pub static LUCINA_TEAR_TRANSFER_EVENTS: AtomicU32 = AtomicU32::new(0);

fn lucina_tear_transfer_event(ep_id: u8, ptr: u64) -> u64 {
    static SEEN: AtomicU32 = AtomicU32::new(0);
    static LAST_EP: AtomicU32 = AtomicU32::new(0);
    static LAST_PTR: AtomicU64 = AtomicU64::new(0);
    let every = LUCINA_TEAR_TRANSFER_EVENTS.load(Ordering::Relaxed);
    // DCI 1 is the control endpoint: its callers wait for the event, and nothing can get past it.
    if every == 0 || ep_id <= 1 {
        return ptr;
    }
    let last_ep = LAST_EP.swap(ep_id as u32, Ordering::Relaxed);
    let last_ptr = LAST_PTR.swap(ptr, Ordering::Relaxed);
    let tear = last_ep != 0
        && last_ep != ep_id as u32
        && SEEN.fetch_add(1, Ordering::Relaxed) % every == every - 1;
    if tear { last_ptr } else { ptr }
}
