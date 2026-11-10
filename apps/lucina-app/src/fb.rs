//! The screen: `/dev/fb0`, which Asterinas backs with the firmware framebuffer (the UEFI GOP,
//! passed by LucinaUEFI as a device-tree `simple-framebuffer`), and the VT's graphics mode, which
//! stops the kernel's text console from drawing over us.
//!
//! The structures and ioctl numbers are those of Linux's `include/uapi/linux/fb.h` and `kd.h`.

use std::fs::{File, OpenOptions};
use std::io;
use std::os::fd::AsRawFd;
use std::os::unix::fs::OpenOptionsExt;

use crate::gfx::{Frame, Rect};

const FBIOGET_VSCREENINFO: libc::c_ulong = 0x4600;
const FBIOGET_FSCREENINFO: libc::c_ulong = 0x4602;
const KDSETMODE: libc::c_ulong = 0x4b3a;
const KDGETMODE: libc::c_ulong = 0x4b3b;
const KD_TEXT: libc::c_ulong = 0;
const KD_GRAPHICS: libc::c_ulong = 1;
const KDGKBMODE: libc::c_ulong = 0x4b44;
const KDSKBMODE: libc::c_ulong = 0x4b45;
const K_OFF: libc::c_int = 4;
const K_UNICODE: libc::c_int = 3;

fn os_error(what: &str) -> io::Error {
    let e = io::Error::last_os_error();
    io::Error::new(e.kind(), format!("{what}: {e}"))
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct Bitfield {
    offset: u32,
    length: u32,
    msb_right: u32,
}

/// `struct fb_var_screeninfo`, 160 bytes.
#[repr(C)]
#[derive(Clone, Copy, Default)]
struct VarScreenInfo {
    xres: u32,
    yres: u32,
    xres_virtual: u32,
    yres_virtual: u32,
    xoffset: u32,
    yoffset: u32,
    bits_per_pixel: u32,
    grayscale: u32,
    red: Bitfield,
    green: Bitfield,
    blue: Bitfield,
    transp: Bitfield,
    nonstd: u32,
    activate: u32,
    height: u32,
    width: u32,
    accel_flags: u32,
    timing: [u32; 11],
    reserved: [u32; 4],
}

/// `struct fb_fix_screeninfo`.
#[repr(C)]
#[derive(Clone, Copy)]
struct FixScreenInfo {
    id: [u8; 16],
    smem_start: libc::c_ulong,
    smem_len: u32,
    type_: u32,
    type_aux: u32,
    visual: u32,
    xpanstep: u16,
    ypanstep: u16,
    ywrapstep: u16,
    line_length: u32,
    mmio_start: libc::c_ulong,
    mmio_len: u32,
    accel: u32,
    capabilities: u16,
    reserved: [u16; 2],
}

/// Where red, green and blue sit in a pixel: (bit offset, bit length) each.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PixelLayout {
    pub bpp: u32,
    pub r: (u32, u32),
    pub g: (u32, u32),
    pub b: (u32, u32),
}

impl PixelLayout {
    /// The frame's own layout: rows are copied as they are.
    pub fn is_xrgb8888(&self) -> bool {
        self.bpp == 32 && self.r == (16, 8) && self.g == (8, 8) && self.b == (0, 8)
    }

    /// One XRGB8888 pixel in this layout.
    pub fn pack(&self, p: u32) -> u32 {
        let ch = |v: u32, (off, len): (u32, u32)| {
            if len == 0 {
                0
            } else {
                ((v & 0xff) >> (8 - len.min(8))) << off
            }
        };
        ch(p >> 16, self.r) | ch(p >> 8, self.g) | ch(p, self.b)
    }

    pub fn describe(&self) -> String {
        if self.is_xrgb8888() {
            "XRGB8888".into()
        } else {
            format!(
                "{} bpp (R{}:{} G{}:{} B{}:{})",
                self.bpp, self.r.0, self.r.1, self.g.0, self.g.1, self.b.0, self.b.1
            )
        }
    }
}

/// Where finished frames go. All drawing happens in a [`Frame`] in memory; an output only copies changed
/// rectangles. A GPU's blit replaces the copy here, and its fill the loops in `Frame`, with nothing above
/// these two types changing.
pub trait Surface {
    /// Copy `area` of the frame (the same size as the screen) to the screen.
    fn present(&mut self, frame: &Frame, area: Rect);
    /// Copy a small image (`w` pixels wide) to (`x`, `y`), over what the screen shows.
    fn present_image(&mut self, x: i32, y: i32, w: usize, px: &[u32]);
    /// Copy several areas of the frame, as one piece of work if the output can.
    fn present_all(&mut self, frame: &Frame, areas: &[Rect]) {
        for area in areas {
            self.present(frame, *area);
        }
    }
}

impl Surface for Fb {
    fn present(&mut self, frame: &Frame, area: Rect) {
        Fb::present(self, frame, area);
    }
    fn present_image(&mut self, x: i32, y: i32, w: usize, px: &[u32]) {
        Fb::present_image(self, x, y, w, px);
    }
}

/// A mapped framebuffer.
pub struct Fb {
    _file: File,
    map: *mut u8,
    map_len: usize,
    pub width: u32,
    pub height: u32,
    pitch: usize,
    /// Byte offset of the visible screen (panning).
    origin: usize,
    pub layout: PixelLayout,
    pub name: String,
}

impl Fb {
    pub fn open(path: &str) -> io::Result<Fb> {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .custom_flags(libc::O_CLOEXEC)
            .open(path)?;
        let fd = file.as_raw_fd();
        let mut var = VarScreenInfo::default();
        // SAFETY: FBIOGET_VSCREENINFO fills a struct fb_var_screeninfo, which VarScreenInfo mirrors.
        if unsafe { libc::ioctl(fd, FBIOGET_VSCREENINFO as _, &mut var as *mut VarScreenInfo) } < 0
        {
            return Err(os_error("FBIOGET_VSCREENINFO"));
        }
        // SAFETY: integers only, so all-zero is valid; the ioctl fills it.
        let mut fix: FixScreenInfo = unsafe { std::mem::zeroed() };
        if unsafe { libc::ioctl(fd, FBIOGET_FSCREENINFO as _, &mut fix as *mut FixScreenInfo) } < 0
        {
            return Err(os_error("FBIOGET_FSCREENINFO"));
        }
        let layout = PixelLayout {
            bpp: var.bits_per_pixel,
            r: (var.red.offset, var.red.length),
            g: (var.green.offset, var.green.length),
            b: (var.blue.offset, var.blue.length),
        };
        if !matches!(layout.bpp, 16 | 32) {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                format!("{} bits per pixel", layout.bpp),
            ));
        }
        let bytes_pp = layout.bpp as usize / 8;
        let pitch = fix.line_length as usize;
        let origin = var.yoffset as usize * pitch + var.xoffset as usize * bytes_pp;
        let map_len = fix.smem_len as usize;
        if var.xres == 0
            || var.yres == 0
            || pitch < var.xres as usize * bytes_pp
            || map_len < origin + var.yres as usize * pitch
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "unusable mode {}x{} pitch {pitch} memory {map_len}",
                    var.xres, var.yres
                ),
            ));
        }
        // SAFETY: maps the framebuffer memory the driver reported, shared and writable.
        let map = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                map_len,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_SHARED,
                fd,
                0,
            )
        };
        if map == libc::MAP_FAILED {
            return Err(os_error("mmap"));
        }
        let end = fix.id.iter().position(|&b| b == 0).unwrap_or(16);
        Ok(Fb {
            _file: file,
            map: map.cast(),
            map_len,
            width: var.xres,
            height: var.yres,
            pitch,
            origin,
            layout,
            name: String::from_utf8_lossy(&fix.id[..end]).into_owned(),
        })
    }

    /// Byte offset of the visible screen in its memory: not 0 when panned.
    pub fn origin(&self) -> usize {
        self.origin
    }

    /// What the screen's memory holds at (`x`, `y`), raw. For checks: reading this memory is slow.
    pub fn read_pixel(&self, x: u32, y: u32) -> u32 {
        let off =
            self.origin + y as usize * self.pitch + x as usize * (self.layout.bpp as usize / 8);
        assert!(off + 4 <= self.map_len && x < self.width && y < self.height);
        // Four byte reads, not one u32 read: on a 16-bpp screen `off` is only 2-aligned, and `read_volatile` needs an aligned
        // pointer. The same four bytes, little-endian, as the u32 read gave where it was aligned.
        let mut bytes = [0u8; 4];
        for (i, byte) in bytes.iter_mut().enumerate() {
            // SAFETY: `off + 4 <= map_len` (checked just above), so `off + i` is inside the mapping for every i < 4; the
            // mapping is readable for as long as `self` lives (unmapped only in Drop), and a `u8` has alignment 1.
            *byte = unsafe { self.map.add(off + i).read_volatile() };
        }
        u32::from_le_bytes(bytes)
    }

    /// One row of the screen's memory, raw, as much as `out` holds. For a program that copies the screen: reading this memory is slow.
    pub fn read_row(&self, y: u32, out: &mut [u32]) {
        let bpp = self.layout.bpp as usize / 8;
        let off = self.origin + y as usize * self.pitch;
        let n = out.len().min(self.width as usize);
        assert!(bpp == 4 && off + n * 4 <= self.map_len && y < self.height);
        // SAFETY: the `n * 4` source bytes are inside the mapping (checked just above), `out` holds at least `n` u32s, and the
        // two do not overlap (one is the device mapping, the other a Rust slice). Copied as bytes: neither `origin` nor the
        // pitch is promised to be a multiple of 4, and a byte copy needs no alignment. A copy, not volatile reads: nothing
        // here depends on the order, and burst reads are what makes this memory bearable.
        unsafe { std::ptr::copy_nonoverlapping(self.map.add(off), out.as_mut_ptr().cast::<u8>(), n * 4) };
    }

    /// Copy `rows` (pixels starting at column `x`) to the screen, one slice per row from `y`.
    fn write_rows<'a>(&mut self, x: i32, rows: impl Iterator<Item = (i32, &'a [u32])>) {
        let bpp = self.layout.bpp as usize / 8;
        for (y, src) in rows {
            if y < 0 || y >= self.height as i32 || x < 0 {
                continue;
            }
            let n = src.len().min((self.width as i32 - x).max(0) as usize);
            let off = self.origin + y as usize * self.pitch + x as usize * bpp;
            if off + n * bpp > self.map_len {
                continue;
            }
            // SAFETY: off .. off + n * bpp is inside the mapping (checked just above).
            unsafe {
                let dst = self.map.add(off);
                if self.layout.is_xrgb8888() {
                    std::ptr::copy_nonoverlapping(src.as_ptr().cast::<u8>(), dst, n * 4);
                } else if bpp == 4 {
                    for (i, &p) in src[..n].iter().enumerate() {
                        dst.cast::<u32>()
                            .add(i)
                            .write_unaligned(self.layout.pack(p));
                    }
                } else {
                    for (i, &p) in src[..n].iter().enumerate() {
                        dst.cast::<u16>()
                            .add(i)
                            .write_unaligned(self.layout.pack(p) as u16);
                    }
                }
            }
        }
    }

    /// Copy `area` of the frame (the same size as the screen) to the screen.
    pub fn present(&mut self, frame: &Frame, area: Rect) {
        let r = area.intersect(&frame.bounds());
        if !r.is_empty() {
            self.write_rows(r.x, frame.rows(r));
        }
    }

    /// Copy a small image (`w` pixels wide) to (`x`, `y`): the pointer drawn over the frame.
    pub fn present_image(&mut self, x: i32, y: i32, w: usize, px: &[u32]) {
        if w == 0 {
            return;
        }
        // Clip on the left by skipping columns.
        let skip = (-x).max(0) as usize;
        if skip >= w {
            return;
        }
        let rows = px
            .chunks_exact(w)
            .enumerate()
            .map(|(i, row)| (y + i as i32, &row[skip..]));
        self.write_rows(x.max(0), rows);
    }
}

impl Drop for Fb {
    fn drop(&mut self) {
        // SAFETY: unmaps exactly what open() mapped.
        unsafe { libc::munmap(self.map.cast(), self.map_len) };
    }
}

/// The foreground VT in KD_GRAPHICS until dropped: the kernel stops drawing its console (log
/// lines, the old prompt) on the framebuffer, and redraws it when we hand the screen back.
/// Gives the screen and the keyboard back to the kernel's console, which then shows what it was sent (a boot with
/// `lucina.quiet=1` keeps it all). For whoever takes over from a program that could not do it itself.
pub fn text_console() -> io::Result<()> {
    let tty = OpenOptions::new()
        .read(true)
        .write(true)
        .custom_flags(libc::O_NOCTTY | libc::O_CLOEXEC)
        .open("/dev/tty0")?;
    // SAFETY: both ioctls take an integer argument.
    if unsafe { libc::ioctl(tty.as_raw_fd(), KDSKBMODE as _, K_UNICODE) } < 0 {
        return Err(os_error("KDSKBMODE"));
    }
    // SAFETY: as above.
    if unsafe { libc::ioctl(tty.as_raw_fd(), KDSETMODE as _, KD_TEXT) } < 0 {
        return Err(os_error("KDSETMODE"));
    }
    Ok(())
}

pub struct GraphicsMode {
    tty: File,
    /// The console's keyboard mode before, to put back.
    keyboard: libc::c_int,
    /// The VT was already in graphics mode: the screen is someone else's (the shell, with a program started by hand
    /// from its terminal), so it stays that way when we end.
    screen_was_taken: bool,
}

impl GraphicsMode {
    pub fn enter() -> io::Result<GraphicsMode> {
        let tty = OpenOptions::new()
            .read(true)
            .write(true)
            // We must not make the VT our controlling terminal.
            .custom_flags(libc::O_NOCTTY | libc::O_CLOEXEC)
            .open("/dev/tty0")?;
        let mut mode: libc::c_int = KD_TEXT as _;
        // SAFETY: KDGETMODE writes one int. If it fails, `mode` says text, as before this check.
        unsafe { libc::ioctl(tty.as_raw_fd(), KDGETMODE as _, &mut mode) };
        let screen_was_taken = mode != KD_TEXT as libc::c_int;
        // SAFETY: KDSETMODE takes an integer argument.
        if unsafe { libc::ioctl(tty.as_raw_fd(), KDSETMODE as _, KD_GRAPHICS) } < 0 {
            return Err(os_error("KDSETMODE"));
        }
        // The keys are ours now. Left on, the console also takes them: Ctrl+C there is SIGINT to init's own group, and
        // Alt+F1 switches console. As Linux's display servers do.
        let mut keyboard: libc::c_int = -1;
        // SAFETY: KDGKBMODE writes one int; KDSKBMODE takes an integer argument.
        unsafe {
            libc::ioctl(tty.as_raw_fd(), KDGKBMODE as _, &mut keyboard);
            if libc::ioctl(tty.as_raw_fd(), KDSKBMODE as _, K_OFF) < 0 {
                eprintln!("lucina-shell: {}", os_error("KDSKBMODE"));
            }
        }
        Ok(GraphicsMode {
            tty,
            keyboard,
            screen_was_taken,
        })
    }
}

impl Drop for GraphicsMode {
    fn drop(&mut self) {
        // SAFETY: as in enter().
        unsafe {
            if !self.screen_was_taken {
                libc::ioctl(self.tty.as_raw_fd(), KDSETMODE as _, KD_TEXT);
            }
            if self.keyboard >= 0 {
                libc::ioctl(self.tty.as_raw_fd(), KDSKBMODE as _, self.keyboard);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn structures_match_the_kernel() {
        assert_eq!(std::mem::size_of::<VarScreenInfo>(), 160);
        #[cfg(target_pointer_width = "64")]
        assert_eq!(std::mem::size_of::<FixScreenInfo>(), 80);
    }

    #[test]
    fn packs_other_layouts() {
        let bgr = PixelLayout {
            bpp: 32,
            r: (0, 8),
            g: (8, 8),
            b: (16, 8),
        };
        assert!(!bgr.is_xrgb8888());
        assert_eq!(bgr.pack(0x00112233), 0x00332211);
        let rgb565 = PixelLayout {
            bpp: 16,
            r: (11, 5),
            g: (5, 6),
            b: (0, 5),
        };
        assert_eq!(rgb565.pack(0x00ffffff), 0xffff);
        assert_eq!(rgb565.pack(0x00ff0000), 0xf800);
        assert_eq!(rgb565.pack(0x0000ff00), 0x07e0);
        assert_eq!(rgb565.describe(), "16 bpp (R11:5 G5:6 B0:5)");
    }
}

/// `FBIOBLANK`: the screen off (`FB_BLANK_POWERDOWN`) or back on (`FB_BLANK_UNBLANK`). The picture stays. Fails where the kernel
/// has nothing to switch.
pub fn blank(off: bool) -> std::io::Result<()> {
    const FBIOBLANK: libc::c_ulong = 0x4611;
    const FB_BLANK_UNBLANK: libc::c_ulong = 0;
    const FB_BLANK_POWERDOWN: libc::c_ulong = 4;
    let file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open("/dev/fb0")?;
    let arg = if off { FB_BLANK_POWERDOWN } else { FB_BLANK_UNBLANK };
    // SAFETY: FBIOBLANK takes its argument by value and touches no memory.
    let r = unsafe { libc::ioctl(std::os::fd::AsRawFd::as_raw_fd(&file), FBIOBLANK as _, arg) };
    if r == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}
