//! A program's own GPU work on Juno's Adreno 618, through `/dev/lucina-gpu` (`lucina_adreno::uapi`, kernel patch 1171): a
//! [`Device`] is one context with its own GPU address space; [`Bo`]s are buffer objects at addresses the program picks
//! ([`Va`] hands them out), mapped into the program; [`Device::submit`] puts command buffers on the GPU's ring and returns a
//! sequence that [`Device::wait`] waits for.
//!
//! This is the layer the wgpu backend (stage 3 of `docs/research/juno-gpu-3d.md`) stands on. Memory is cached for the CPU:
//! what the CPU wrote is cleaned for the GPU by naming the object with [`Use::Clean`] in a submit (command buffers always are),
//! and what the GPU wrote is read after [`Bo::invalidate`].

use std::{
    ffi::CString,
    io,
    os::fd::{AsRawFd, FromRawFd, OwnedFd},
    time::Duration,
};

pub use lucina_adreno::{
    a2d, gpu_rules, pm4,
    submit::CommandBuffer,
    uapi,
};

pub const DEVICE: &str = "/dev/lucina-gpu";

/// One context: its own address space, objects and queue. Closing it frees them all.
pub struct Device {
    fd: OwnedFd,
}

/// A buffer object, mapped into the program.
pub struct Bo {
    fd: i32,
    pub handle: u32,
    pub iova: u64,
    pub size: u64,
    map: *mut u8,
}

// The mapping is plain memory shared with the GPU; the program orders its use with fences.
unsafe impl Send for Bo {}

/// How a submit uses an object it names.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Use {
    /// The GPU reads or writes it; the CPU's cache is not touched.
    Gpu,
    /// The CPU wrote it: clean the CPU's cache first.
    Clean,
}

fn ioctl(fd: i32, cmd: u32, arg: &mut [u8]) -> io::Result<()> {
    // SAFETY: `arg` is exactly the size the command encodes, and the kernel reads and writes only that.
    let r = unsafe { libc::ioctl(fd, cmd as _, arg.as_mut_ptr()) };
    if r < 0 { Err(io::Error::last_os_error()) } else { Ok(()) }
}

impl Device {
    pub fn open() -> io::Result<Self> {
        let path = CString::new(DEVICE).unwrap();
        // SAFETY: a valid C string; the descriptor is owned below.
        let fd = unsafe { libc::open(path.as_ptr(), libc::O_RDWR | libc::O_CLOEXEC) };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: `fd` was just opened and is not owned elsewhere.
        Ok(Self { fd: unsafe { OwnedFd::from_raw_fd(fd) } })
    }

    pub fn param(&self, which: u64) -> io::Result<u64> {
        let mut b = [0u8; 16];
        b[..8].copy_from_slice(&which.to_le_bytes());
        ioctl(self.fd.as_raw_fd(), uapi::IOCTL_PARAM, &mut b)?;
        Ok(u64::from_le_bytes(b[8..].try_into().unwrap()))
    }

    /// An object of `size` bytes at `iova` (page aligned, inside the window, not overlapping another), mapped.
    pub fn bo(&self, iova: u64, size: u64, gpu_read_only: bool) -> io::Result<Bo> {
        let flags = if gpu_read_only { uapi::BO_GPU_READ_ONLY } else { 0 };
        let mut b = uapi::BoNew { size, iova, flags, ..Default::default() }.encode();
        ioctl(self.fd.as_raw_fd(), uapi::IOCTL_BO_NEW, &mut b)?;
        let made = uapi::BoNew::decode(&b);
        // SAFETY: a shared mapping of the object's pages at the offset the kernel gave; unmapped in `Drop`.
        let map = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                made.size as usize,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_SHARED,
                self.fd.as_raw_fd(),
                made.mmap_offset as libc::off_t,
            )
        };
        let bo = Bo { fd: self.fd.as_raw_fd(), handle: made.handle, iova: made.iova, size: made.size, map: map.cast() };
        if map == libc::MAP_FAILED {
            let err = io::Error::last_os_error();
            drop(bo);
            return Err(err);
        }
        Ok(bo)
    }

    /// Puts command buffers on the ring; `named` are the other objects the work uses. Returns the fence's sequence.
    pub fn submit(&self, cmds: &[CommandBuffer], named: &[(&Bo, Use)]) -> io::Result<u64> {
        let cmd_bytes: Vec<u8> = cmds.iter().flat_map(uapi::encode_command).collect();
        let bo_bytes: Vec<u8> = named
            .iter()
            .flat_map(|(bo, u)| uapi::encode_bo_entry(bo.handle, if *u == Use::Clean { uapi::SUBMIT_BO_CLEAN } else { 0 }))
            .collect();
        let mut b = uapi::Submit {
            cmds: cmd_bytes.as_ptr() as u64,
            cmds_count: cmds.len() as u32,
            bos: bo_bytes.as_ptr() as u64,
            bos_count: named.len() as u32,
            sequence: 0,
        }
        .encode();
        ioctl(self.fd.as_raw_fd(), uapi::IOCTL_SUBMIT, &mut b)?;
        Ok(uapi::Submit::decode(&b).sequence)
    }

    /// Waits until the fence of `sequence` arrived, for at most `timeout`. `TimedOut` if it did not.
    pub fn wait(&self, sequence: u64, timeout: Duration) -> io::Result<()> {
        let mut b = uapi::Wait { sequence, timeout_ns: timeout.as_nanos() as u64 }.encode();
        ioctl(self.fd.as_raw_fd(), uapi::IOCTL_WAIT, &mut b)
    }

    /// Faults the kernel put down to this context so far.
    pub fn faults(&self) -> io::Result<u64> {
        self.param(uapi::PARAM_FAULTS)
    }
}

impl Bo {
    pub fn words(&mut self) -> &mut [u32] {
        // SAFETY: the mapping is `size` bytes, page aligned, and lives as long as `self`.
        unsafe { std::slice::from_raw_parts_mut(self.map.cast::<u32>(), self.size as usize / 4) }
    }

    pub fn bytes(&mut self) -> &mut [u8] {
        // SAFETY: as above.
        unsafe { std::slice::from_raw_parts_mut(self.map, self.size as usize) }
    }

    fn sync(&self, dir: u32, offset: u64, len: u64) -> io::Result<()> {
        let mut b = uapi::BoSync { handle: self.handle, dir, offset, len }.encode();
        ioctl(self.fd, uapi::IOCTL_BO_SYNC, &mut b)
    }

    /// Before the CPU reads what the GPU wrote.
    pub fn invalidate(&self) -> io::Result<()> {
        self.sync(uapi::SYNC_TO_CPU, 0, 0)
    }

    /// After the CPU wrote, for work submitted without [`Use::Clean`].
    pub fn clean(&self) -> io::Result<()> {
        self.sync(uapi::SYNC_TO_GPU, 0, 0)
    }
}

impl Drop for Bo {
    fn drop(&mut self) {
        if self.map.cast::<libc::c_void>() != libc::MAP_FAILED {
            // SAFETY: the mapping made in `Device::bo`.
            unsafe { libc::munmap(self.map.cast(), self.size as usize) };
        }
        let mut b = self.handle.to_le_bytes();
        let _ = ioctl(self.fd, uapi::IOCTL_BO_FREE, &mut b);
    }
}

/// GPU addresses for a program's objects: a bump allocator over the window, 64 KiB aligned.
pub struct Va {
    next: u64,
}

impl Default for Va {
    fn default() -> Self {
        Self { next: gpu_rules::BO_BASE }
    }
}

impl Va {
    pub fn take(&mut self, size: u64) -> u64 {
        let at = self.next;
        self.next = (at + size).next_multiple_of(0x1_0000);
        at
    }
}

/// A command buffer being written: PM4 words.
#[derive(Default)]
pub struct Pm4 {
    pub words: Vec<u32>,
}

impl Pm4 {
    /// `CP_MEM_WRITE`: `data` to `iova`.
    pub fn mem_write(&mut self, iova: u64, data: &[u32]) {
        self.words.push(pm4::pkt7(pm4::CP_MEM_WRITE, 2 + data.len() as u32));
        self.words.push(iova as u32);
        self.words.push((iova >> 32) as u32);
        self.words.extend_from_slice(data);
    }

    /// A raw type 7 packet.
    pub fn pkt7(&mut self, opcode: u8, payload: &[u32]) {
        self.words.push(pm4::pkt7(opcode, payload.len() as u32));
        self.words.extend_from_slice(payload);
    }

    /// A raw type 4 packet: `values` to registers from `reg` on.
    pub fn pkt4(&mut self, reg: u32, values: &[u32]) {
        self.words.push(pm4::pkt4(reg, values.len() as u32));
        self.words.extend_from_slice(values);
    }

    /// Copies the words into `bo` at word `at`, and names them as a command buffer.
    pub fn into_bo(&self, bo: &mut Bo, at: usize) -> CommandBuffer {
        bo.words()[at..at + self.words.len()].copy_from_slice(&self.words);
        CommandBuffer { bo: bo.handle, offset: (at * 4) as u64, dwords: self.words.len() as u64 }
    }
}
