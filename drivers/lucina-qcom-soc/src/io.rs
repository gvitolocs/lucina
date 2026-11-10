//! The register seam. A window of 32-bit words addressed by byte offset, for MMIO in the kernel and RAM in tests.
//! Adapted from narf `drivers/soc/qcom/src/io.rs` (GPL-2.0-or-later) without its async waits: no wait in this crate
//! sleeps by itself, the caller provides [`Delay`].

/// A 32-bit register window addressed by byte offset (`readl`/`writel`). Offsets are 4-byte aligned.
pub trait RegIo {
    fn read32(&self, off: u32) -> u32;
    fn write32(&self, off: u32, value: u32);
}

impl<T: RegIo + ?Sized> RegIo for &T {
    fn read32(&self, off: u32) -> u32 {
        (**self).read32(off)
    }
    fn write32(&self, off: u32, value: u32) {
        (**self).write32(off, value)
    }
}

/// Sleeps between polls.
pub trait Delay {
    fn sleep_ms(&self, ms: u32);
}

/// Bytes through aligned word reads (`memcpy_fromio` on memory that only takes words).
pub fn read_bytes<M: RegIo + ?Sized>(mem: &M, off: u32, buf: &mut [u8]) {
    for (i, byte) in buf.iter_mut().enumerate() {
        let at = off.wrapping_add(i as u32);
        *byte = mem.read32(at & !3).to_le_bytes()[(at & 3) as usize];
    }
}
