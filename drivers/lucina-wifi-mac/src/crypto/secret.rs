//! Buffers holding key material: wiped on drop, never printed.
//!
//! `forbid(unsafe_code)` rules out `write_volatile`, so the wipe is a plain overwrite followed by `black_box` and
//! a compiler fence. That stops the optimiser eliding the store in practice, but it is not the guarantee a
//! volatile write gives, and copies made by moves of the old value are not reached. The owner should keep these
//! values in place and drop them when done.

use alloc::vec::Vec;
use core::fmt;
use core::sync::atomic::{Ordering, compiler_fence};

/// Overwrites `buf` with zeros.
pub fn wipe(buf: &mut [u8]) {
    for b in buf.iter_mut() {
        *b = 0;
    }
    core::hint::black_box(&mut *buf);
    compiler_fence(Ordering::SeqCst);
}

/// Overwrites `buf` with zeros.
pub fn wipe_u32(buf: &mut [u32]) {
    for w in buf.iter_mut() {
        *w = 0;
    }
    core::hint::black_box(&mut *buf);
    compiler_fence(Ordering::SeqCst);
}

/// Comparison whose time does not depend on where the slices differ. Slices of different length are unequal.
pub fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b) {
        diff |= x ^ y;
    }
    core::hint::black_box(diff) == 0
}

/// A fixed-size secret.
#[derive(Clone)]
pub struct SecretBytes<const N: usize>([u8; N]);

impl<const N: usize> SecretBytes<N> {
    pub fn new(value: [u8; N]) -> Self {
        Self(value)
    }

    pub fn zero() -> Self {
        Self([0; N])
    }

    pub fn expose(&self) -> &[u8; N] {
        &self.0
    }

    pub fn expose_mut(&mut self) -> &mut [u8; N] {
        &mut self.0
    }
}

impl<const N: usize> Drop for SecretBytes<N> {
    fn drop(&mut self) {
        wipe(&mut self.0);
    }
}

impl<const N: usize> fmt::Debug for SecretBytes<N> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "<{N} secret bytes>")
    }
}

impl<const N: usize> PartialEq for SecretBytes<N> {
    fn eq(&self, other: &Self) -> bool {
        ct_eq(&self.0, &other.0)
    }
}

impl<const N: usize> Eq for SecretBytes<N> {}

/// A secret of run-time length. Built once at its full size: growing would leave the old allocation unwiped.
#[derive(Clone)]
pub struct SecretVec(Vec<u8>);

impl SecretVec {
    /// Takes ownership of `value`. Its allocation is what gets wiped.
    pub fn new(value: Vec<u8>) -> Self {
        Self(value)
    }

    pub fn from_slice(value: &[u8]) -> Self {
        Self(Vec::from(value))
    }

    pub fn expose(&self) -> &[u8] {
        &self.0
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl Drop for SecretVec {
    fn drop(&mut self) {
        wipe(&mut self.0);
    }
}

impl fmt::Debug for SecretVec {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "<{} secret bytes>", self.0.len())
    }
}

impl PartialEq for SecretVec {
    fn eq(&self, other: &Self) -> bool {
        ct_eq(&self.0, &other.0)
    }
}

impl Eq for SecretVec {}
