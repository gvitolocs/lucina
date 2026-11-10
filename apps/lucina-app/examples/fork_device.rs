//! Writes to a shared device mapping after a fork, as the shell does to its GPU buffer once an app it started has closed. A kernel that
//! write-protects the mapping at the fork never lets the write through, and the alarm ends this program. Run by scripts/qemu-fork-device.sh.

use std::os::fd::AsRawFd;

const DRM_IOCTL_MODE_CREATE_DUMB: libc::c_ulong = 0xc020_64b2;
const DRM_IOCTL_MODE_MAP_DUMB: libc::c_ulong = 0xc010_64b3;

#[repr(C)]
#[derive(Default)]
struct CreateDumb {
    height: u32,
    width: u32,
    bpp: u32,
    flags: u32,
    handle: u32,
    pitch: u32,
    size: u64,
}

#[repr(C)]
#[derive(Default)]
struct MapDumb {
    handle: u32,
    pad: u32,
    offset: u64,
}

fn main() {
    let card = std::fs::OpenOptions::new().read(true).write(true).open("/dev/dri/card0").expect("open /dev/dri/card0");
    let mut create = CreateDumb { height: 64, width: 64, bpp: 32, ..Default::default() };
    // SAFETY: both ioctls fill the struct they are given, of the size their number says.
    assert_eq!(unsafe { libc::ioctl(card.as_raw_fd(), DRM_IOCTL_MODE_CREATE_DUMB as _, &mut create) }, 0, "create dumb");
    let mut map = MapDumb { handle: create.handle, ..Default::default() };
    // SAFETY: as above.
    assert_eq!(unsafe { libc::ioctl(card.as_raw_fd(), DRM_IOCTL_MODE_MAP_DUMB as _, &mut map) }, 0, "map dumb");
    let len = create.size as usize;
    // SAFETY: maps the buffer just made, shared and writable, at the offset the driver gave.
    let p = unsafe { libc::mmap(std::ptr::null_mut(), len, libc::PROT_READ | libc::PROT_WRITE, libc::MAP_SHARED, card.as_raw_fd(), map.offset as _) };
    assert_ne!(p, libc::MAP_FAILED, "mmap");
    let px = p.cast::<u32>();
    // SAFETY: inside the mapping. The page has to be present before the fork: that is what the fork protects.
    unsafe { px.write_volatile(1) };
    // SAFETY: this program has one thread, and the child only ends.
    match unsafe { libc::fork() } {
        0 => unsafe { libc::_exit(0) },
        pid if pid > 0 => {
            // SAFETY: waits for the child just made.
            unsafe { libc::waitpid(pid, std::ptr::null_mut(), 0) };
        }
        _ => panic!("fork"),
    }
    eprintln!("fork-device: writing after the fork");
    // SAFETY: both are plain calls; the write is inside the mapping.
    unsafe {
        libc::alarm(20);
        px.write_volatile(2);
        libc::alarm(0);
        assert_eq!(px.read_volatile(), 2);
    }
    eprintln!("fork-device: PASS");
}
