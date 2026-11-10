// SPDX-License-Identifier: GPL-2.0-only
//! The interface a program's GPU driver (the wgpu backend, the `lucina-gpu3d` proof tool) uses on `/dev/lucina-gpu`: ioctls
//! for buffer objects, submits and waits, after Linux msm's `GEM_NEW`, `GEM_CPU_PREP`, `GEM_SUBMIT`, `WAIT_FENCE` and
//! `GET_PARAM` (`include/uapi/drm/msm_drm.h`), smaller: one context per open file, one queue, the program picks its own GPU
//! addresses inside its window (as msm's `VM_BIND` does) and the kernel refuses any that overlap or leave it.
//!
//! Every structure is little-endian bytes of a fixed size, so that the kernel (which reads them as `[u8; N]`) and
//! userspace share this one definition. Numbers are Linux's `_IOC` encoding with magic `'L'`.

/// `_IOC(dir, 'L', nr, size)`.
pub const fn ioc(dir: u32, nr: u32, size: u32) -> u32 {
    (dir << 30) | (size << 16) | ((b'L' as u32) << 8) | nr
}
const WRITE: u32 = 1;
const READ: u32 = 2;

/// `GET_PARAM`: `[param u64, value u64]`, the value out.
pub const IOCTL_PARAM: u32 = ioc(READ | WRITE, 0x40, 16);
/// `GEM_NEW` with the address: [`BoNew`].
pub const IOCTL_BO_NEW: u32 = ioc(READ | WRITE, 0x41, BoNew::SIZE as u32);
/// `GEM_CLOSE`: the handle as a u32 (little-endian, 4 bytes).
pub const IOCTL_BO_FREE: u32 = ioc(WRITE, 0x42, 4);
/// `GEM_CPU_PREP`/`FINI`: [`BoSync`].
pub const IOCTL_BO_SYNC: u32 = ioc(WRITE, 0x43, BoSync::SIZE as u32);
/// `GEM_SUBMIT`: [`Submit`]; the sequence comes back in it.
pub const IOCTL_SUBMIT: u32 = ioc(READ | WRITE, 0x44, Submit::SIZE as u32);
/// `WAIT_FENCE`: [`Wait`].
pub const IOCTL_WAIT: u32 = ioc(WRITE, 0x45, Wait::SIZE as u32);

/// `MSM_PARAM_CHIP_ID`: 0x06010800 for the A618.
pub const PARAM_CHIP_ID: u64 = 1;
/// `MSM_PARAM_GMEM_SIZE`.
pub const PARAM_GMEM_SIZE: u64 = 2;
/// The first address a buffer object may take ([`crate::gpu_rules::BO_BASE`]).
pub const PARAM_VA_START: u64 = 3;
/// One past the last ([`crate::gpu_rules::VA_END`]).
pub const PARAM_VA_END: u64 = 4;
/// `MSM_PARAM_FAULTS`: faults put down to this program (SMMU faults, hangs). A count that grew means "device lost" to Vulkan.
pub const PARAM_FAULTS: u64 = 5;
/// The last sequence known finished.
pub const PARAM_RETIRED: u64 = 6;
/// `MSM_PARAM_GMEM_BASE`.
pub const PARAM_GMEM_BASE: u64 = 7;

/// Buffer object flags. Read-only for the GPU (`MSM_BO_GPU_READONLY`).
pub const BO_GPU_READ_ONLY: u32 = 1;

/// Submit flags of a named object: clean the CPU's cache of it before the GPU runs (the program wrote it on the CPU).
pub const SUBMIT_BO_CLEAN: u32 = 1;

/// Sync directions: the CPU wrote, the GPU will read (clean); the GPU wrote, the CPU will read (invalidate).
pub const SYNC_TO_GPU: u32 = 1;
pub const SYNC_TO_CPU: u32 = 2;

fn u32_at(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]])
}
fn u64_at(b: &[u8], at: usize) -> u64 {
    u64::from(u32_at(b, at)) | (u64::from(u32_at(b, at + 4)) << 32)
}
fn put32(b: &mut [u8], at: usize, v: u32) {
    b[at..at + 4].copy_from_slice(&v.to_le_bytes());
}
fn put64(b: &mut [u8], at: usize, v: u64) {
    b[at..at + 8].copy_from_slice(&v.to_le_bytes());
}

/// `{ size: u64, iova: u64, flags: u32, handle: u32 (out), mmap_offset: u64 (out) }`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct BoNew {
    pub size: u64,
    pub iova: u64,
    pub flags: u32,
    pub handle: u32,
    pub mmap_offset: u64,
}

impl BoNew {
    pub const SIZE: usize = 32;
    pub fn decode(b: &[u8; Self::SIZE]) -> Self {
        Self { size: u64_at(b, 0), iova: u64_at(b, 8), flags: u32_at(b, 16), handle: u32_at(b, 20), mmap_offset: u64_at(b, 24) }
    }
    pub fn encode(&self) -> [u8; Self::SIZE] {
        let mut b = [0; Self::SIZE];
        put64(&mut b, 0, self.size);
        put64(&mut b, 8, self.iova);
        put32(&mut b, 16, self.flags);
        put32(&mut b, 20, self.handle);
        put64(&mut b, 24, self.mmap_offset);
        b
    }
}

/// `{ handle: u32, dir: u32, offset: u64, len: u64 }`; `len` 0 is the whole object.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct BoSync {
    pub handle: u32,
    pub dir: u32,
    pub offset: u64,
    pub len: u64,
}

impl BoSync {
    pub const SIZE: usize = 24;
    pub fn decode(b: &[u8; Self::SIZE]) -> Self {
        Self { handle: u32_at(b, 0), dir: u32_at(b, 4), offset: u64_at(b, 8), len: u64_at(b, 16) }
    }
    pub fn encode(&self) -> [u8; Self::SIZE] {
        let mut b = [0; Self::SIZE];
        put32(&mut b, 0, self.handle);
        put32(&mut b, 4, self.dir);
        put64(&mut b, 8, self.offset);
        put64(&mut b, 16, self.len);
        b
    }
}

/// `{ cmds: u64 (user pointer to [CommandEntry]), cmds_count: u32, bos_count: u32, bos: u64 (pointer to [BoEntry]),
/// sequence: u64 (out) }`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Submit {
    pub cmds: u64,
    pub cmds_count: u32,
    pub bos_count: u32,
    pub bos: u64,
    pub sequence: u64,
}

impl Submit {
    pub const SIZE: usize = 32;
    pub fn decode(b: &[u8; Self::SIZE]) -> Self {
        Self { cmds: u64_at(b, 0), cmds_count: u32_at(b, 8), bos_count: u32_at(b, 12), bos: u64_at(b, 16), sequence: u64_at(b, 24) }
    }
    pub fn encode(&self) -> [u8; Self::SIZE] {
        let mut b = [0; Self::SIZE];
        put64(&mut b, 0, self.cmds);
        put32(&mut b, 8, self.cmds_count);
        put32(&mut b, 12, self.bos_count);
        put64(&mut b, 16, self.bos);
        put64(&mut b, 24, self.sequence);
        b
    }
}

/// One command buffer: `{ handle: u32, _pad: u32, offset: u64, dwords: u64 }`.
pub const COMMAND_ENTRY_SIZE: usize = 24;
/// One named object: `{ handle: u32, flags: u32 }`.
pub const BO_ENTRY_SIZE: usize = 8;

pub fn decode_command(b: &[u8]) -> crate::submit::CommandBuffer {
    crate::submit::CommandBuffer { bo: u32_at(b, 0), offset: u64_at(b, 8), dwords: u64_at(b, 16) }
}
pub fn encode_command(c: &crate::submit::CommandBuffer) -> [u8; COMMAND_ENTRY_SIZE] {
    let mut b = [0; COMMAND_ENTRY_SIZE];
    put32(&mut b, 0, c.bo);
    put64(&mut b, 8, c.offset);
    put64(&mut b, 16, c.dwords);
    b
}
pub fn decode_bo_entry(b: &[u8]) -> (u32, u32) {
    (u32_at(b, 0), u32_at(b, 4))
}
pub fn encode_bo_entry(handle: u32, flags: u32) -> [u8; BO_ENTRY_SIZE] {
    let mut b = [0; BO_ENTRY_SIZE];
    put32(&mut b, 0, handle);
    put32(&mut b, 4, flags);
    b
}

/// `{ sequence: u64, timeout_ns: u64 }`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Wait {
    pub sequence: u64,
    pub timeout_ns: u64,
}

impl Wait {
    pub const SIZE: usize = 16;
    pub fn decode(b: &[u8; Self::SIZE]) -> Self {
        Self { sequence: u64_at(b, 0), timeout_ns: u64_at(b, 8) }
    }
    pub fn encode(&self) -> [u8; Self::SIZE] {
        let mut b = [0; Self::SIZE];
        put64(&mut b, 0, self.sequence);
        put64(&mut b, 8, self.timeout_ns);
        b
    }
}

/// Where a buffer object's pages are for `mmap`: far above the tile buffers' offsets (`(id + 1) << 40`).
pub const BO_MMAP_BASE: u64 = 1 << 52;
pub const fn bo_mmap_offset(handle: u32) -> u64 {
    BO_MMAP_BASE + ((handle as u64) << 32)
}
/// The handle an `mmap` offset names, if it is one of [`bo_mmap_offset`]'s.
pub const fn bo_of_mmap_offset(offset: u64) -> Option<u32> {
    if offset < BO_MMAP_BASE || offset & 0xffff_ffff != 0 {
        return None;
    }
    let handle = (offset - BO_MMAP_BASE) >> 32;
    if handle == 0 || handle > u32::MAX as u64 { None } else { Some(handle as u32) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numbers_are_linuxs_ioc_encoding() {
        // _IOWR('L', 0x41, 32)
        assert_eq!(IOCTL_BO_NEW, 0xc020_4c41);
        assert_eq!(IOCTL_BO_FREE, 0x4004_4c42);
        assert_eq!(IOCTL_WAIT, 0x4010_4c45);
    }

    #[test]
    fn layouts_round_trip() {
        let n = BoNew { size: 0x1000, iova: 0x10_0020_0000, flags: 1, handle: 7, mmap_offset: bo_mmap_offset(7) };
        assert_eq!(BoNew::decode(&n.encode()), n);
        let s = Submit { cmds: 0x1234, cmds_count: 2, bos_count: 3, bos: 0x5678, sequence: 9 };
        assert_eq!(Submit::decode(&s.encode()), s);
        let c = crate::submit::CommandBuffer { bo: 4, offset: 0x40, dwords: 12 };
        assert_eq!(decode_command(&encode_command(&c)), c);
        assert_eq!(decode_bo_entry(&encode_bo_entry(5, SUBMIT_BO_CLEAN)), (5, 1));
        let w = Wait { sequence: 3, timeout_ns: 1_000_000 };
        assert_eq!(Wait::decode(&w.encode()), w);
        let y = BoSync { handle: 2, dir: SYNC_TO_CPU, offset: 0x100, len: 0x200 };
        assert_eq!(BoSync::decode(&y.encode()), y);
    }

    #[test]
    fn mmap_offsets() {
        assert_eq!(bo_of_mmap_offset(bo_mmap_offset(1)), Some(1));
        assert_eq!(bo_of_mmap_offset(bo_mmap_offset(4096)), Some(4096));
        assert_eq!(bo_of_mmap_offset(BO_MMAP_BASE), None);
        assert_eq!(bo_of_mmap_offset(bo_mmap_offset(1) + 0x1000), None);
        assert_eq!(bo_of_mmap_offset(3 << 40), None);
    }
}
