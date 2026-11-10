//! The lock runtime ax-sync's locks call, for the host tests. A kernel provides it (the LucinaOS one is in
//! aster-usb); here a spin lock is an `AtomicBool` and a read-write lock is a reader count with a writer bit.
//! Interrupts and preemption do not exist on the host, so the saved context is empty.

// The names are the symbols ax-crate-interface gives the interface functions.
#![allow(non_snake_case)]

use core::{
    hint::spin_loop,
    panic::Location,
    sync::atomic::{AtomicBool, AtomicUsize, Ordering},
};

use ax_sync::interface::{AcquireResult, ContextState, LOCK_MODE_READ, LockMetadata};

const WRITER: usize = 1 << (usize::BITS - 1);
const NO_CONTEXT: ContextState = ContextState::new(0, 0);

fn spin_try(locked: &AtomicBool) -> bool {
    locked
        .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
        .is_ok()
}

fn rw_try(state: &AtomicUsize, mode: u8) -> bool {
    if mode == LOCK_MODE_READ {
        let current = state.load(Ordering::Relaxed);
        current & WRITER == 0
            && state
                .compare_exchange(current, current + 1, Ordering::Acquire, Ordering::Relaxed)
                .is_ok()
    } else {
        state
            .compare_exchange(0, WRITER, Ordering::Acquire, Ordering::Relaxed)
            .is_ok()
    }
}

#[unsafe(no_mangle)]
extern "Rust" fn __SpinOps_acquire(
    locked: &AtomicBool,
    _metadata: &LockMetadata,
    _lock_addr: usize,
    _context: u8,
    _subclass: u32,
    _caller: &'static Location<'static>,
) -> ContextState {
    while !spin_try(locked) {
        spin_loop();
    }
    NO_CONTEXT
}

#[unsafe(no_mangle)]
extern "Rust" fn __SpinOps_try_acquire(
    locked: &AtomicBool,
    _metadata: &LockMetadata,
    _lock_addr: usize,
    _context: u8,
    _subclass: u32,
    _caller: &'static Location<'static>,
) -> AcquireResult {
    AcquireResult::new(spin_try(locked), NO_CONTEXT)
}

#[unsafe(no_mangle)]
extern "Rust" fn __SpinOps_release(
    locked: &AtomicBool,
    _lock_addr: usize,
    _context: u8,
    _context_state: ContextState,
) {
    locked.store(false, Ordering::Release);
}

#[unsafe(no_mangle)]
extern "Rust" fn __SpinOps_force_release(locked: &AtomicBool, _lock_addr: usize, _context: u8) {
    locked.store(false, Ordering::Release);
}

#[unsafe(no_mangle)]
extern "Rust" fn __SpinOps_is_locked(locked: &AtomicBool) -> bool {
    locked.load(Ordering::Relaxed)
}

#[unsafe(no_mangle)]
extern "Rust" fn __RwLockOps_acquire(
    state: &AtomicUsize,
    _metadata: &LockMetadata,
    _lock_addr: usize,
    _context: u8,
    mode: u8,
    _caller: &'static Location<'static>,
) -> ContextState {
    while !rw_try(state, mode) {
        spin_loop();
    }
    NO_CONTEXT
}

#[unsafe(no_mangle)]
extern "Rust" fn __RwLockOps_try_acquire(
    state: &AtomicUsize,
    _metadata: &LockMetadata,
    _lock_addr: usize,
    _context: u8,
    mode: u8,
    _caller: &'static Location<'static>,
) -> AcquireResult {
    AcquireResult::new(rw_try(state, mode), NO_CONTEXT)
}

#[unsafe(no_mangle)]
extern "Rust" fn __RwLockOps_release(
    state: &AtomicUsize,
    _lock_addr: usize,
    _context: u8,
    _context_state: ContextState,
    mode: u8,
) {
    if mode == LOCK_MODE_READ {
        state.fetch_sub(1, Ordering::Release);
    } else {
        state.store(0, Ordering::Release);
    }
}

#[unsafe(no_mangle)]
extern "Rust" fn __RwLockOps_force_read_decrement(
    state: &AtomicUsize,
    _lock_addr: usize,
    _context: u8,
) {
    state.fetch_sub(1, Ordering::Release);
}
