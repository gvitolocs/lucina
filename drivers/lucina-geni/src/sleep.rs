//! One serial engine's bus, as a system sleep sees it (patch 1356).
//!
//! Linux does this in two places: the adapter driver marks its adapter suspended, which makes every later transfer fail
//! (`i2c_mark_adapter_suspended`, `drivers/i2c/busses/i2c-qcom-geni.c:1011`, whose `__i2c_transfer` answer is `-EAGAIN`,
//! `drivers/i2c/i2c-core-base.c:2284-2287`), and the transfers already going are finished before the clocks go, because
//! the PM core runs the client's `suspend` before the adapter's `suspend_noirq` (`drivers/base/power/main.c:1720`,
//! `:1339`) and its own xfer path holds the adapter (`i2c-qcom-geni.c:692`). This kernel has no runtime PM and no adapter
//! lock: the owner of an engine is a kernel thread that keeps running through a sleep (`thread/freezer.rs:9-11`), so the
//! three facts Linux knows about a bus - down or up, a transfer going on, the owner quiesced - are counted here instead,
//! and the hook waits for the last two before it touches the clocks.
//!
//! The states are the two Linux has for the adapter (`gi2c->suspended`, `:969` and `:995`); `parked` is the rendezvous
//! that replaces the callback order, and it is the owner's word that it has said whatever its device needs to the bus -
//! for an I2C HID device the `SET_POWER SLEEP` command (`i2c_hid_core_suspend`, `i2c-hid-core.c:957`) - and will not
//! start another transfer until the bus is up again.

use core::sync::atomic::{AtomicU8, AtomicU32, Ordering};

/// `gi2c->suspended`, and Linux's adapter `SUSPENDED` flag in one bit less.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum State {
    /// Transfers go through.
    Running = 0,
    /// The engine is down: its clocks may be off, so a transfer is refused rather than tried (`:1011`).
    Down = 1,
}

impl State {
    fn of(v: u8) -> State {
        if v == State::Down as u8 {
            State::Down
        } else {
            State::Running
        }
    }
}

/// A serial engine's bus, shared between the thread that drives it and the sleep hook.
#[derive(Debug)]
pub struct Gate {
    state: AtomicU8,
    /// Transfers in flight. A count, not a flag: Linux's adapter serves several callers in turn under one lock, and a
    /// second transfer on the same engine is normal while the first is finishing.
    inflight: AtomicU32,
    /// The owner has said it is done with the bus for this sleep.
    parked: AtomicU8,
}

impl Gate {
    pub const fn new() -> Self {
        Gate {
            state: AtomicU8::new(State::Running as u8),
            inflight: AtomicU32::new(0),
            parked: AtomicU8::new(0),
        }
    }

    pub fn state(&self) -> State {
        State::of(self.state.load(Ordering::Acquire))
    }

    pub fn is_down(&self) -> bool {
        self.state() == State::Down
    }

    /// Starts a transfer: `None` when the engine is down, which is what a caller turns into
    /// [`crate::Error::Suspended`] instead of touching an unclocked engine (`docs` in the crate's header: an access to
    /// one can hang the bus for good).
    pub fn try_use(&self) -> Option<Use<'_>> {
        // The count is taken before the state is read and re-checked after it, so a `down` between the two sees this
        // transfer and refuses to go down; the same shape as Linux's `pm_runtime_get_if_in_use` racing a suspend.
        if self.state() == State::Down {
            return None;
        }
        self.inflight.fetch_add(1, Ordering::AcqRel);
        if self.state() == State::Down {
            self.end();
            return None;
        }
        // Starting a transfer is the owner taking its word back: `parked` means no transfer is coming, so it cannot
        // stay said while one is going on.
        self.parked.store(0, Ordering::Release);
        Some(Use { gate: self })
    }

    /// Whether nothing is being transferred.
    pub fn is_idle(&self) -> bool {
        self.inflight.load(Ordering::Acquire) == 0
    }

    /// How many transfers are going on, for the log of a sleep that waited.
    pub fn inflight(&self) -> u32 {
        self.inflight.load(Ordering::Acquire)
    }

    /// The owner is quiesced: what it had to say to its device over this bus is said.
    pub fn park(&self) {
        self.parked.store(1, Ordering::Release);
    }

    /// Whether the owner has given that word for the sleep that is coming.
    pub fn is_parked(&self) -> bool {
        self.parked.load(Ordering::Acquire) != 0
    }

    /// Takes the bus down. `false` when a transfer is still going: the caller waits, as Linux's PM core waits for the
    /// client's callback before it reaches the adapter's. A transfer that started just before the store is caught by
    /// [`Gate::try_use`]'s second look, so nothing reaches the registers once this returns `true`.
    pub fn down(&self) -> bool {
        if !self.is_idle() {
            return false;
        }
        self.state.store(State::Down as u8, Ordering::Release);
        true
    }

    /// The bus runs again, and the owner's word is not owed until the next sleep.
    pub fn up(&self) {
        self.parked.store(0, Ordering::Release);
        self.state.store(State::Running as u8, Ordering::Release);
    }

    fn end(&self) {
        self.inflight.fetch_sub(1, Ordering::AcqRel);
    }
}

impl Default for Gate {
    fn default() -> Self {
        Self::new()
    }
}

/// One transfer's claim on a [`Gate`]; it gives it back when it is dropped, error or not.
pub struct Use<'a> {
    gate: &'a Gate,
}

impl Drop for Use<'_> {
    fn drop(&mut self) {
        self.gate.end();
    }
}
