//! The screen for Lucina: `/dev/fb0` read as XRGB (`bgr0`), as PNG, and voxl's `wait`.

use std::fs::{File, OpenOptions};
use std::io;
use std::os::fd::AsRawFd;
use std::os::unix::fs::{FileExt, OpenOptionsExt};
use std::time::{Duration, Instant};

const FBIOGET_VSCREENINFO: libc::c_ulong = 0x4600;
const FBIOGET_FSCREENINFO: libc::c_ulong = 0x4602;

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

/// One `bgr0` pixel (bytes B, G, R, X; X is not alpha) as XRGB u32.
fn bgr0_to_xrgb(px: &[u8]) -> u32 {
    (px[2] as u32) << 16 | (px[1] as u32) << 8 | px[0] as u32
}

/// Pixel source, so tests can use a fake.
pub trait Frame {
    /// (width, height) in pixels.
    fn size(&self) -> (usize, usize);
    /// Row `y` as XRGB u32 (0x00RRGGBB; the top byte ignored). `out.len() == width`.
    fn read_row(&mut self, y: usize, out: &mut [u32]) -> io::Result<()>;
}

/// `/dev/fb0` (or another path) through the fb ioctls and pread.
pub struct Fb {
    file: File,
    width: usize,
    height: usize,
    pitch: usize,
}

impl Fb {
    pub fn open(path: &str) -> Result<Fb, String> {
        let file = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_CLOEXEC)
            .open(path)
            .map_err(|e| format!("open {path}: {e}"))?;
        let fd = file.as_raw_fd();
        let mut var = VarScreenInfo::default();
        // SAFETY: FBIOGET_VSCREENINFO fills a struct fb_var_screeninfo, which VarScreenInfo mirrors.
        if unsafe { libc::ioctl(fd, FBIOGET_VSCREENINFO as _, &mut var as *mut VarScreenInfo) } < 0
        {
            return Err(format!(
                "FBIOGET_VSCREENINFO on {path}: {}",
                io::Error::last_os_error()
            ));
        }
        // SAFETY: integers only, so all-zero is valid; the ioctl fills it.
        let mut fix: FixScreenInfo = unsafe { std::mem::zeroed() };
        if unsafe { libc::ioctl(fd, FBIOGET_FSCREENINFO as _, &mut fix as *mut FixScreenInfo) } < 0
        {
            return Err(format!(
                "FBIOGET_FSCREENINFO on {path}: {}",
                io::Error::last_os_error()
            ));
        }
        let (width, height) = (var.xres as usize, var.yres as usize);
        let pitch = fix.line_length as usize;
        if var.bits_per_pixel != 32 {
            return Err(format!("{path}: {} bits per pixel, not 32", var.bits_per_pixel));
        }
        if width == 0 || height == 0 || pitch < width * 4 {
            return Err(format!(
                "{path}: unusable mode {width}x{height} pitch {pitch}"
            ));
        }
        Ok(Fb {
            file,
            width,
            height,
            pitch,
        })
    }
}

impl Frame for Fb {
    fn size(&self) -> (usize, usize) {
        (self.width, self.height)
    }

    fn read_row(&mut self, y: usize, out: &mut [u32]) -> io::Result<()> {
        if y >= self.height || out.len() != self.width {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("row {y} into {} pixels for {}x{}", out.len(), self.width, self.height),
            ));
        }
        let mut buf = vec![0u8; self.pitch];
        let mut done = 0;
        while done < buf.len() {
            let n = self
                .file
                .read_at(&mut buf[done..], (y * self.pitch + done) as u64)?;
            if n == 0 {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    format!("short read on row {y}"),
                ));
            }
            done += n;
        }
        for (o, px) in out.iter_mut().zip(buf.as_chunks::<4>().0.iter()) {
            *o = bgr0_to_xrgb(px);
        }
        Ok(())
    }
}

/// The whole screen as a PNG (RGB8, no alpha), scaled down (box filter / averaging) so the width is at most
/// `max_width` (never scaled up; keep the aspect ratio; height rounded). Returns (png bytes, out_width, out_height).
pub fn png(frame: &mut dyn Frame, max_width: usize) -> Result<(Vec<u8>, usize, usize), String> {
    let (w, h) = frame.size();
    if w == 0 || h == 0 {
        return Err("empty frame".into());
    }
    let ow = w.min(max_width.max(1));
    let oh = ((h as u64 * ow as u64 + w as u64 / 2) / w as u64).max(1) as usize;
    let mut rows = vec![vec![0u32; w]; h];
    for (y, row) in rows.iter_mut().enumerate() {
        frame.read_row(y, row).map_err(|e| format!("row {y}: {e}"))?;
    }
    let mut rgb = vec![0u8; ow * oh * 3];
    for oy in 0..oh {
        let y0 = oy * h / oh;
        let y1 = ((oy + 1) * h / oh).max(y0 + 1);
        for ox in 0..ow {
            let x0 = ox * w / ow;
            let x1 = ((ox + 1) * w / ow).max(x0 + 1);
            let (mut r, mut g, mut b) = (0u64, 0u64, 0u64);
            for row in &rows[y0..y1] {
                for &p in &row[x0..x1] {
                    r += ((p >> 16) & 0xff) as u64;
                    g += ((p >> 8) & 0xff) as u64;
                    b += (p & 0xff) as u64;
                }
            }
            let n = ((x1 - x0) * (y1 - y0)) as u64;
            let i = (oy * ow + ox) * 3;
            rgb[i] = (r / n) as u8;
            rgb[i + 1] = (g / n) as u8;
            rgb[i + 2] = (b / n) as u8;
        }
    }
    let mut out = Vec::new();
    let mut encoder = png::Encoder::new(&mut out, ow as u32, oh as u32);
    encoder.set_color(png::ColorType::Rgb);
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder.write_header().map_err(|e| e.to_string())?;
    writer.write_image_data(&rgb).map_err(|e| e.to_string())?;
    drop(writer);
    Ok((out, ow, oh))
}

/// A cheap fingerprint of the screen: FNV-1a 64 of every 4th row (rows 0, 4, 8, … and always the last row),
/// top byte masked.
pub fn fingerprint(frame: &mut dyn Frame) -> Result<u64, String> {
    let (w, h) = frame.size();
    if w == 0 || h == 0 {
        return Err("empty frame".into());
    }
    let mut hash = 0xcbf2_9ce4_8422_2325u64;
    let mut row = vec![0u32; w];
    let hash_row = |row: &[u32], hash: &mut u64| {
        for &p in row {
            for byte in (p & 0x00ff_ffff).to_le_bytes() {
                *hash ^= byte as u64;
                *hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
            }
        }
    };
    let mut y = 0;
    while y < h {
        frame.read_row(y, &mut row).map_err(|e| format!("row {y}: {e}"))?;
        hash_row(&row, &mut hash);
        y += 4;
    }
    if (h - 1) % 4 != 0 {
        frame
            .read_row(h - 1, &mut row)
            .map_err(|e| format!("row {}: {e}", h - 1))?;
        hash_row(&row, &mut hash);
    }
    Ok(hash)
}

/// Options for [`wait`].
#[derive(Debug, Clone)]
pub struct WaitOptions {
    pub timeout_ms: u64,
    pub settle_ms: u64,
    pub since: Option<u64>,
    pub settle_only: bool,
    pub poll_ms: u64,
}

impl Default for WaitOptions {
    fn default() -> Self {
        WaitOptions {
            timeout_ms: 10_000,
            settle_ms: 500,
            since: None,
            settle_only: false,
            poll_ms: 100,
        }
    }
}

/// What [`wait`] saw.
#[derive(Debug, Clone, serde::Serialize)]
pub struct WaitResult {
    pub changed: bool,
    pub settled: bool,
    pub fingerprint: u64,
    pub elapsed_ms: u64,
}

/// voxl's `/wait`, sans HTTP: poll `fingerprint` every `poll_ms`. Phase 1 waits until the fingerprint differs
/// from `start` (skipped when `since` is None and `settle_only`); phase 2 waits until it stays equal for
/// `settle_ms`. Stops at `timeout_ms` total (max 60_000) in any phase.
pub fn wait(frame: &mut dyn Frame, o: &WaitOptions) -> Result<WaitResult, String> {
    let timeout = o.timeout_ms.min(60_000);
    let poll = Duration::from_millis(o.poll_ms.max(1));
    let t0 = Instant::now();
    let start = match o.since {
        Some(s) => s,
        None => fingerprint(frame)?,
    };
    let skip_phase1 = o.since.is_none() && o.settle_only;
    let mut changed = false;
    let mut settled = false;
    let mut current = start;
    if !skip_phase1 {
        while (t0.elapsed().as_millis() as u64) < timeout {
            std::thread::sleep(poll);
            let fp = fingerprint(frame)?;
            if fp != start {
                current = fp;
                changed = true;
                break;
            }
        }
    }
    if changed || skip_phase1 {
        let mut last = current;
        let mut stable_since: Option<Instant> = None;
        while (t0.elapsed().as_millis() as u64) < timeout {
            std::thread::sleep(poll);
            let fp = fingerprint(frame)?;
            if fp != last {
                last = fp;
                stable_since = None;
            } else {
                let since = stable_since.get_or_insert_with(Instant::now);
                if since.elapsed().as_millis() as u64 >= o.settle_ms {
                    settled = true;
                    break;
                }
            }
        }
        current = last;
    }
    Ok(WaitResult {
        changed,
        settled,
        fingerprint: current,
        elapsed_ms: t0.elapsed().as_millis() as u64,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Fake {
        w: usize,
        h: usize,
        pixel: u32,
        reads: usize,
        change_after: usize,
    }

    impl Fake {
        fn new(w: usize, h: usize, pixel: u32) -> Fake {
            Fake {
                w,
                h,
                pixel,
                reads: 0,
                change_after: usize::MAX,
            }
        }
    }

    impl Frame for Fake {
        fn size(&self) -> (usize, usize) {
            (self.w, self.h)
        }
        fn read_row(&mut self, _y: usize, out: &mut [u32]) -> io::Result<()> {
            self.reads += 1;
            let p = if self.reads > self.change_after {
                0x00aa_bbcc
            } else {
                self.pixel
            };
            out.fill(p);
            Ok(())
        }
    }

    fn decode(bytes: &[u8]) -> (Vec<u8>, u32, u32) {
        let decoder = png::Decoder::new(std::io::Cursor::new(bytes));
        let mut reader = decoder.read_info().unwrap();
        let mut buf = vec![0u8; reader.output_buffer_size().unwrap()];
        let info = reader.next_frame(&mut buf).unwrap();
        buf.truncate(info.buffer_size());
        (buf, info.width, info.height)
    }

    #[test]
    fn bgr0_with_zero_x_gives_rgb() {
        assert_eq!(bgr0_to_xrgb(&[0x33, 0x22, 0x11, 0x00]), 0x0011_2233);
        let mut fake = Fake::new(4, 4, 0x0011_2233);
        let (bytes, w, h) = png(&mut fake, 100).unwrap();
        assert_eq!((w, h), (4, 4));
        let (rgb, w, h) = decode(&bytes);
        assert_eq!((w, h), (4, 4));
        assert_eq!(&rgb[..3], &[0x11, 0x22, 0x33]);
    }

    #[test]
    fn scales_down_but_never_up() {
        let mut big = Fake::new(1920, 1080, 0x00ff_0000);
        let (_, w, h) = png(&mut big, 1280).unwrap();
        assert_eq!((w, h), (1280, 720));
        let mut small = Fake::new(800, 600, 0x0000_ff00);
        let (_, w, h) = png(&mut small, 1280).unwrap();
        assert_eq!((w, h), (800, 600));
    }

    #[test]
    fn fingerprint_ignores_top_byte_and_tracks_rows() {
        let mut a = Fake::new(8, 8, 0xff11_2233);
        let mut b = Fake::new(8, 8, 0x0011_2233);
        assert_eq!(fingerprint(&mut a).unwrap(), fingerprint(&mut b).unwrap());
        let mut c = Fake::new(8, 8, 0x0011_2234);
        assert_ne!(fingerprint(&mut b).unwrap(), fingerprint(&mut c).unwrap());
    }

    #[test]
    fn wait_sees_change_and_settle() {
        let mut fake = Fake::new(8, 8, 0x0011_2233);
        fake.change_after = 6; // the third fingerprint (rows 0, 4, 7) sees the new pixel
        let o = WaitOptions {
            timeout_ms: 5_000,
            settle_ms: 5,
            since: None,
            settle_only: false,
            poll_ms: 1,
        };
        let r = wait(&mut fake, &o).unwrap();
        assert!(r.changed);
        assert!(r.settled);
        assert_eq!(r.fingerprint, fingerprint(&mut fake).unwrap());
    }

    #[test]
    fn wait_times_out_when_nothing_changes() {
        let mut fake = Fake::new(8, 8, 0x0011_2233);
        let o = WaitOptions {
            timeout_ms: 50,
            settle_ms: 500,
            since: None,
            settle_only: false,
            poll_ms: 1,
        };
        let r = wait(&mut fake, &o).unwrap();
        assert!(!r.changed);
        assert!(!r.settled);
        assert!(r.elapsed_ms >= 50);
    }
}
