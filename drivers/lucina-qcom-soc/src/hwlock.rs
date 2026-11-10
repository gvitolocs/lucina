//! The TCSR mutex (`qcom,tcsr-mutex`), Linux `drivers/hwspinlock/qcom_hwspinlock.c` (GPL-2.0-only). Adapted from narf
//! `drivers/soc/qcom/src/hwlock.rs` (GPL-2.0-or-later), made synchronous. Lock `n` is the register at `n * 0x1000`:
//! write our id, read it back; it is ours if the id stuck. SMEM allocation holds lock 3 on this laptop
//! (`hwlocks = <&tcsr_mutex 3>`, window 0x1f40000).

use crate::io::{Delay, RegIo};

/// `QCOM_MUTEX_APPS_PROC_ID`.
pub const APPS_PROC_ID: u32 = 1;
pub const NUM_LOCKS: u32 = 32;
pub const STRIDE: u32 = 0x1000;
/// `HWSPINLOCK_TIMEOUT` of `smem.c`, in milliseconds.
pub const SMEM_LOCK_TIMEOUT_MS: u32 = 1000;

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum HwLockError {
    BadId,
    /// Another processor kept the lock past the timeout.
    Timeout,
    /// The lock register held this id at unlock.
    NotOwner(u32),
}

/// What SMEM allocation needs from a lock.
pub trait HwSpinlock {
    fn lock(&self) -> Result<(), HwLockError>;
    fn unlock(&self) -> Result<(), HwLockError>;
}

/// One lock of the mutex block.
pub struct TcsrMutex<R, D> {
    regs: R,
    delay: D,
    id: u32,
}

impl<R: RegIo, D: Delay> TcsrMutex<R, D> {
    pub fn new(regs: R, delay: D, id: u32) -> Result<Self, HwLockError> {
        if id >= NUM_LOCKS {
            return Err(HwLockError::BadId);
        }
        Ok(Self { regs, delay, id })
    }

    /// `qcom_hwspinlock_trylock`.
    pub fn try_lock(&self) -> bool {
        self.regs.write32(self.id * STRIDE, APPS_PROC_ID);
        self.regs.read32(self.id * STRIDE) == APPS_PROC_ID
    }
}

impl<R: RegIo, D: Delay> HwSpinlock for TcsrMutex<R, D> {
    fn lock(&self) -> Result<(), HwLockError> {
        for _ in 0..SMEM_LOCK_TIMEOUT_MS {
            if self.try_lock() {
                return Ok(());
            }
            self.delay.sleep_ms(1);
        }
        Err(HwLockError::Timeout)
    }

    /// `qcom_hwspinlock_unlock`.
    fn unlock(&self) -> Result<(), HwLockError> {
        let owner = self.regs.read32(self.id * STRIDE);
        if owner != APPS_PROC_ID {
            return Err(HwLockError::NotOwner(owner));
        }
        self.regs.write32(self.id * STRIDE, 0);
        Ok(())
    }
}
