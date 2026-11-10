//! The GPU, where the kernel offers it (`lucina.gpu=on`, `/dev/lucina-gpu`): rectangles on the screen without the CPU touching them.
//! Fills and moves take about a millisecond or less for the whole screen, and the kernel returns when the pixels are in memory.
//! An app that gets `None` from [`Gpu::open`] draws as before. The GPU writes the screen itself, so what it draws is not in a
//! [`crate::fb`] frame: keep them in step, or use it for what is redrawn whole anyway.

use std::fs::{File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::FileExt;

use crate::gfx::Rect;

pub const DEVICE: &str = "/dev/lucina-gpu";
/// A write from this file offset on puts pixels in the kernel's offscreen surface, at the offset's remainder.
const UPLOAD_BASE: u64 = 1 << 32;
/// `mmap` of buffer `id` is at file offset `(id + 1) * BUFFER_MMAP_STEP`.
const BUFFER_MMAP_STEP: i64 = 1 << 40;
/// Commands in one write; the kernel refuses more.
pub const BATCH_COMMANDS: usize = 128;

pub struct Gpu {
    file: File,
    /// The surfaces' line, in bytes.
    pitch: u32,
    /// Buffers made and not freed, by id.
    held: Vec<u32>,
}

/// Pixels the program paints, 0xAARRGGBB, not premultiplied, with rows `pitch` bytes apart. See [`Gpu::buffer`].
pub struct Buffer {
    pub id: u32,
    pub width: u32,
    pub height: u32,
    pub pitch: u32,
    map: *mut u8,
    len: usize,
}

/// Where a decoded picture's 4:2:0 planes lie in a buffer, for [`Batch::yuv`]: byte offsets from the buffer's start and lines in
/// bytes, all multiples of 64, as the kernel checks. [`YuvLayout::packed`] makes one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct YuvLayout {
    /// NV12, Cb and Cr interleaved in one plane, as hardware decoders give it; otherwise I420, a plane of each.
    pub nv12: bool,
    pub width: u32,
    pub height: u32,
    pub y_offset: usize,
    pub y_pitch: usize,
    pub c_offset: usize,
    pub c_pitch: usize,
    /// The Cr plane of I420.
    pub v_offset: usize,
}

impl YuvLayout {
    /// The planes one after another from the buffer's start, each line rounded up to 64 bytes.
    pub fn packed(width: u32, height: u32, nv12: bool) -> YuvLayout {
        let (w, h) = (width as usize, height as usize);
        let chroma = w.div_ceil(2);
        let y_pitch = w.next_multiple_of(64);
        let c_pitch = (if nv12 { 2 * chroma } else { chroma }).next_multiple_of(64);
        let c_offset = y_pitch * h;
        YuvLayout {
            nv12,
            width,
            height,
            y_offset: 0,
            y_pitch,
            c_offset,
            c_pitch,
            v_offset: if nv12 { 0 } else { c_offset + c_pitch * h.div_ceil(2) },
        }
    }

    /// Lines of chroma.
    pub fn chroma_rows(&self) -> usize {
        (self.height as usize).div_ceil(2)
    }

    /// Bytes from the buffer's start to the end of the last plane.
    pub fn bytes(&self) -> usize {
        let chroma = self.c_pitch * self.chroma_rows();
        let mut end = (self.y_offset + self.y_pitch * self.height as usize).max(self.c_offset + chroma);
        if !self.nv12 {
            end = end.max(self.v_offset + chroma);
        }
        end
    }
}

/// The rows of [`Batch::yuv`]'s matrix: BT.709 or BT.601, video range (16 to 235) or full. Each row is red, green and blue in turn, as
/// `Y`, `Cb`, `Cr` and a constant, all from 0 to 1.
pub fn yuv_matrix(bt709: bool, full_range: bool) -> [f32; 12] {
    let (kr, kb) = if bt709 { (0.2126f64, 0.0722) } else { (0.299, 0.114) };
    let kg = 1.0 - kr - kb;
    // From a sample in 0..1 to Y' in 0..1 and Cb', Cr' in -0.5..0.5.
    let (ya, yb, ca, cb) = if full_range {
        (1.0, 0.0, 1.0, -128.0 / 255.0)
    } else {
        (255.0 / 219.0, -16.0 / 219.0, 255.0 / 224.0, -128.0 / 224.0)
    };
    let rows = [
        [1.0, 0.0, 2.0 * (1.0 - kr)],
        [1.0, -2.0 * kb * (1.0 - kb) / kg, -2.0 * kr * (1.0 - kr) / kg],
        [1.0, 2.0 * (1.0 - kb), 0.0],
    ];
    let mut out = [0f32; 12];
    for (i, [y, u, v]) in rows.iter().enumerate() {
        out[i * 4] = (y * ya) as f32;
        out[i * 4 + 1] = (u * ca) as f32;
        out[i * 4 + 2] = (v * ca) as f32;
        out[i * 4 + 3] = (y * yb + (u + v) * cb) as f32;
    }
    out
}

impl Buffer {
    /// All of it as bytes, for pictures that are not pixels.
    pub fn bytes_mut(&mut self) -> &mut [u8] {
        // SAFETY: as in `pixels`.
        unsafe { std::slice::from_raw_parts_mut(self.map, self.len) }
    }

    /// All rows, to read.
    pub fn as_slice(&self) -> &[u32] {
        // SAFETY: as in `pixels`.
        unsafe { std::slice::from_raw_parts(self.map.cast::<u32>(), self.len / 4) }
    }

    /// All rows, `pitch / 4` pixels each.
    pub fn pixels(&mut self) -> &mut [u32] {
        // SAFETY: the mapping is this long, page aligned and lives as long as self; the GPU only reads it.
        unsafe { std::slice::from_raw_parts_mut(self.map.cast::<u32>(), self.len / 4) }
    }

    /// One row, `width` pixels.
    pub fn row(&mut self, y: u32) -> &mut [u32] {
        let (stride, width) = (self.pitch as usize / 4, self.width as usize);
        &mut self.pixels()[y as usize * stride..][..width]
    }

    /// The buffer as a rectangle.
    pub fn bounds(&self) -> Rect {
        Rect {
            x: 0,
            y: 0,
            w: self.width as i32,
            h: self.height as i32,
        }
    }
}

impl Drop for Buffer {
    fn drop(&mut self) {
        // SAFETY: unmaps exactly what `Gpu::buffer` mapped.
        unsafe { libc::munmap(self.map.cast(), self.len) };
    }
}

/// Drawing to send at once, in order: see [`Gpu::run`]. Quads in a row with the same source and blending cost one state setup, so
/// sort by buffer where the picture allows.
#[derive(Default)]
pub struct Batch {
    text: String,
    commands: usize,
}

impl Batch {
    pub fn new() -> Batch {
        Batch::default()
    }

    /// How many commands so far: [`BATCH_COMMANDS`] fit in one.
    pub fn len(&self) -> usize {
        self.commands
    }

    pub fn is_empty(&self) -> bool {
        self.commands == 0
    }

    pub fn clear(&mut self) {
        self.text.clear();
        self.commands = 0;
    }

    /// Hands changed rows to the GPU. Rows that are not named here may reach it stale.
    pub fn clean(&mut self, buffer: &Buffer, y: u32, h: u32) -> &mut Batch {
        self.line(format_args!("clean b{} {} {}", buffer.id, y, h))
    }

    /// Fills a rectangle of the screen.
    pub fn fill(&mut self, r: Rect, colour: u32) -> &mut Batch {
        self.line(format_args!(
            "fill screen {} {} {} {} ff{:06x}",
            r.x,
            r.y,
            r.w,
            r.h,
            colour & 0xff_ffff
        ))
    }

    /// Copies `from` of a buffer to `to` on the screen, scaling if the sizes differ, with no blending: the cheapest way to show a tile.
    pub fn blit(&mut self, buffer: &Buffer, from: Rect, to: Rect, filter: bool) -> &mut Batch {
        self.line(format_args!(
            "blit b{} {} {} {} {} screen {} {} {} {}{}",
            buffer.id,
            from.x,
            from.y,
            from.w,
            from.h,
            to.x,
            to.y,
            to.w,
            to.h,
            if filter { " filter" } else { "" }
        ))
    }

    /// Draws `from` of a buffer over `to` on the screen, multiplied by `tint` (0xAARRGGBB; `!0` for none), blended by alpha if `blend`.
    /// A `filter` samples beside `from`: keep a transparent pixel around what is not a whole buffer.
    pub fn quad(
        &mut self,
        buffer: &Buffer,
        from: Rect,
        to: Rect,
        tint: u32,
        blend: bool,
        filter: bool,
    ) -> &mut Batch {
        self.line(format_args!(
            "quad b{} {} {} {} {} screen {} {} {} {} {:08x}{}{}",
            buffer.id,
            from.x,
            from.y,
            from.w,
            from.h,
            to.x,
            to.y,
            to.w,
            to.h,
            tint,
            if blend { " blend" } else { "" },
            if filter { " filter" } else { "" }
        ))
    }

    /// Like [`Batch::quad`] with blending, for pixels whose colour is already multiplied by their alpha, as WebRender's and Vello's are.
    /// The tint multiplies colour too: for opacity `a`, give `0xaaaaaaaa`.
    pub fn quad_premultiplied(
        &mut self,
        buffer: &Buffer,
        from: Rect,
        to: Rect,
        tint: u32,
        filter: bool,
    ) -> &mut Batch {
        self.line(format_args!(
            "quad b{} {} {} {} {} screen {} {} {} {} {:08x} premul{}",
            buffer.id,
            from.x,
            from.y,
            from.w,
            from.h,
            to.x,
            to.y,
            to.w,
            to.h,
            tint,
            if filter { " filter" } else { "" }
        ))
    }

    /// Draws `from` of a decoded picture whose planes `layout` places in `buffer` over `to` on the screen, through `matrix` (see
    /// [`yuv_matrix`]), with a bilinear filter for luma and chroma if `filter`. Clean the rows the planes are in first. A batch takes
    /// at most four.
    pub fn yuv(
        &mut self,
        buffer: &Buffer,
        layout: &YuvLayout,
        from: Rect,
        to: Rect,
        matrix: &[f32; 12],
        filter: bool,
    ) -> &mut Batch {
        self.yuv_line(buffer, layout, from, to, matrix, if filter { " filter" } else { "" })
    }

    /// Holds this batch, for at most a frame, until the panel's scan is below screen row `bottom` or in the blanking before row 0, so
    /// that what it draws from the top down lands behind the scan. Only where [`Gpu::has_vblank`].
    pub fn vblank(&mut self, bottom: u32) -> &mut Batch {
        self.line(format_args!("vblank {bottom}"))
    }

    /// [`Batch::yuv`] by the 2D engine, with no matrix: a probe of what the engine does with planes, nothing to show pictures with.
    pub fn yuv_by_a2d(&mut self, buffer: &Buffer, layout: &YuvLayout, from: Rect, to: Rect) -> &mut Batch {
        self.yuv_line(buffer, layout, from, to, &[0.0; 12], " a2d")
    }

    fn yuv_line(
        &mut self,
        buffer: &Buffer,
        layout: &YuvLayout,
        from: Rect,
        to: Rect,
        matrix: &[f32; 12],
        options: &str,
    ) -> &mut Batch {
        let m: Vec<String> = matrix.iter().map(|v| format!("{:x}", v.to_bits())).collect();
        self.line(format_args!(
            "yuv b{} {} {} {} {} {} {} {} {} {} {} {} {} screen {} {} {} {} {}{}",
            buffer.id,
            if layout.nv12 { "nv12" } else { "i420" },
            layout.width,
            layout.height,
            layout.y_offset,
            layout.y_pitch,
            layout.c_offset,
            layout.c_pitch,
            layout.v_offset,
            from.x,
            from.y,
            from.w,
            from.h,
            to.x,
            to.y,
            to.w,
            to.h,
            m.join(" "),
            options
        ))
    }

    fn line(&mut self, line: std::fmt::Arguments) -> &mut Batch {
        use std::fmt::Write as _;
        let _ = writeln!(self.text, "{line}");
        self.commands += 1;
        self
    }
}

impl Gpu {
    /// `None` if this kernel has no GPU to offer.
    pub fn open() -> Option<Gpu> {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .open(DEVICE)
            .ok()?;
        let mut gpu = Gpu {
            file,
            pitch: 0,
            held: Vec::new(),
        };
        let status = gpu.status().ok()?;
        let screen = status
            .lines()
            .find_map(|line| line.strip_prefix("screen "))?;
        gpu.pitch = screen.split_ascii_whitespace().nth(2)?.parse().ok()?;
        Some(gpu)
    }

    /// Lines of the device's language, one batch: see `lucina_adreno::command` in the kernel's crates.
    pub fn submit(&mut self, batch: &str) -> io::Result<()> {
        self.file.write_all(batch.as_bytes())
    }

    /// `colour` is 0xRRGGBB.
    pub fn fill(&mut self, r: Rect, colour: u32) -> io::Result<()> {
        self.submit(&format!(
            "fill screen {} {} {} {} ff{:06x}\n",
            r.x,
            r.y,
            r.w,
            r.h,
            colour & 0xff_ffff
        ))
    }

    /// Moves a rectangle of the screen to (`x`, `y`). It may overlap where it was: this is scrolling.
    pub fn move_rect(&mut self, r: Rect, x: i32, y: i32) -> io::Result<()> {
        self.submit(&format!(
            "move {} {} {} {} {} {}\n",
            r.x, r.y, r.w, r.h, x, y
        ))
    }

    /// Copies rows of 0xAARRGGBB pixels into the offscreen surface, whose pitch is the screen's, at (`x`, `y`). This costs the CPU as much as writing the screen does: it is for what is then drawn many times.
    pub fn upload(&mut self, x: u32, y: u32, width: usize, px: &[u32]) -> io::Result<()> {
        for (row, line) in px.chunks_exact(width).enumerate() {
            // SAFETY: u32 to bytes, same memory, alignment going down.
            let bytes =
                unsafe { std::slice::from_raw_parts(line.as_ptr().cast::<u8>(), line.len() * 4) };
            let offset = u64::from(y + row as u32) * u64::from(self.pitch) + u64::from(x) * 4;
            self.file.write_all_at(bytes, UPLOAD_BASE + offset)?;
        }
        Ok(())
    }

    /// Draws `from` of the offscreen surface over `to` on the screen, multiplied by `tint` (0xAARRGGBB, `0xffffffff` for none), blended
    /// by alpha if `blend`, with a bilinear filter if `filter`. The pipeline's state makes the first of a batch cost more.
    pub fn quad(
        &mut self,
        from: Rect,
        to: Rect,
        tint: u32,
        blend: bool,
        filter: bool,
    ) -> io::Result<()> {
        self.submit(&format!(
            "quad off {} {} {} {} screen {} {} {} {} {:08x}{}{}
",
            from.x,
            from.y,
            from.w,
            from.h,
            to.x,
            to.y,
            to.w,
            to.h,
            tint,
            if blend { " blend" } else { "" },
            if filter { " filter" } else { "" },
        ))
    }

    /// A buffer this program paints and the GPU reads with no copy: normal cached memory, so painting costs what it costs anywhere. Say
    /// which rows changed with [`Batch::clean`] before the GPU reads them. It ends when it is dropped, or with this `Gpu`.
    pub fn buffer(&mut self, width: u32, height: u32) -> io::Result<Buffer> {
        self.submit(&format!("buffer {width} {height}\n"))?;
        let status = self.status()?;
        // The highest id with this size that this `Gpu` does not hold yet.
        let line = status
            .lines()
            .filter_map(|line| line.strip_prefix("buffer "))
            .map(|line| {
                line.split_ascii_whitespace()
                    .filter_map(|w| w.parse::<u32>().ok())
                    .collect::<Vec<_>>()
            })
            .filter(|f| {
                f.len() == 4 && f[1] == width && f[2] == height && !self.held.contains(&f[0])
            })
            .last()
            .ok_or_else(|| io::Error::other("the new buffer is not listed"))?;
        let (id, pitch) = (line[0], line[3]);
        let len = pitch as usize * height as usize;
        // SAFETY: the kernel maps the buffer's own pages, of at least this length, shared and writable.
        let map = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                len,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_SHARED,
                self.file.as_raw_fd(),
                (i64::from(id) + 1) * BUFFER_MMAP_STEP,
            )
        };
        if map == libc::MAP_FAILED {
            return Err(io::Error::last_os_error());
        }
        self.held.push(id);
        Ok(Buffer {
            id,
            width,
            height,
            pitch,
            map: map.cast(),
            len,
        })
    }

    /// A buffer of at least `len` bytes, for something other than pixels, such as a decoded picture's planes: 4096 bytes a row.
    pub fn bytes_buffer(&mut self, len: usize) -> io::Result<Buffer> {
        let rows = u32::try_from(len.div_ceil(4096).max(1)).map_err(io::Error::other)?;
        self.buffer(1024, rows)
    }

    /// Ends a buffer. Its mapping goes with it.
    pub fn free(&mut self, buffer: Buffer) -> io::Result<()> {
        self.held.retain(|id| *id != buffer.id);
        self.submit(&format!("free b{}\n", buffer.id))
    }

    /// Sends a batch and returns when its pixels are in memory.
    pub fn run(&mut self, batch: &Batch) -> io::Result<()> {
        self.submit(&batch.text)
    }

    /// Whether the kernel knows [`Batch::vblank`].
    pub fn has_vblank(&mut self) -> bool {
        self.status().is_ok_and(|s| s.lines().any(|l| l == "vblank yes"))
    }

    /// What the driver says about itself.
    pub fn status(&mut self) -> io::Result<String> {
        let mut text = String::new();
        self.file.seek(SeekFrom::Start(0))?;
        self.file.read_to_string(&mut text)?;
        Ok(text)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn apply(m: &[f32; 12], y: u8, u: u8, v: u8) -> [i32; 3] {
        let s = [y as f32 / 255.0, u as f32 / 255.0, v as f32 / 255.0];
        let mut out = [0; 3];
        for (i, c) in out.iter_mut().enumerate() {
            let r = &m[i * 4..i * 4 + 4];
            *c = ((r[0] * s[0] + r[1] * s[1] + r[2] * s[2] + r[3]) * 255.0).round() as i32;
        }
        out
    }

    #[test]
    fn the_matrices_give_the_colours_the_standards_name() {
        // Video range: black, white, and BT.601's and BT.709's own 75% red.
        for bt709 in [false, true] {
            let m = yuv_matrix(bt709, false);
            assert_eq!(apply(&m, 16, 128, 128), [0, 0, 0]);
            assert_eq!(apply(&m, 235, 128, 128), [255, 255, 255]);
        }
        let near = |a: [i32; 3], b: [i32; 3]| a.iter().zip(b).all(|(x, y)| (x - y).abs() <= 1);
        assert!(near(apply(&yuv_matrix(false, false), 65, 100, 212), [191, 0, 0]), "{:?}", apply(&yuv_matrix(false, false), 65, 100, 212));
        assert!(near(apply(&yuv_matrix(true, false), 51, 109, 212), [191, 0, 0]), "{:?}", apply(&yuv_matrix(true, false), 51, 109, 212));
        assert_eq!(apply(&yuv_matrix(true, true), 255, 128, 128), [255, 255, 255]);
    }

    #[test]
    fn packed_planes_follow_one_another_on_64_byte_lines() {
        let i420 = YuvLayout::packed(1281, 535, false);
        assert_eq!((i420.y_pitch, i420.c_pitch), (1344, 704));
        assert_eq!(i420.c_offset, 1344 * 535);
        assert_eq!(i420.v_offset, 1344 * 535 + 704 * 268);
        assert_eq!(i420.bytes(), 1344 * 535 + 2 * 704 * 268);
        let nv12 = YuvLayout::packed(1920, 1080, true);
        assert_eq!((nv12.c_pitch, nv12.bytes()), (1920, 1920 * 1080 * 3 / 2));
    }
}
