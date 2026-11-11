//! Showing a picture through the Adreno, where the kernel offers it: the planes are copied as they are into a buffer the GPU reads, and
//! the GPU converts them to RGB and scales them onto the screen, bilinear unless S asks for the plain stretch. The CPU's part is the copy.
//! Without a GPU, after it fails, or with `LUCINA_VIDEO_GPU=0`, the player converts on the CPU as before (`lucina_video::Scaler`).

use std::io;

use lucina_app::Rect;
use lucina_app::gpu::{Batch, Buffer, Gpu, YuvLayout, yuv_matrix};
use lucina_video::Planes;

pub struct GpuPresent {
    gpu: Gpu,
    /// The buffer of the last picture size, and where its planes are in it.
    held: Option<(Buffer, YuvLayout)>,
    batch: Batch,
    /// Draw behind the panel's scan, top to bottom in strips, where the kernel can: no tear across the picture.
    behind_scan: bool,
}

impl GpuPresent {
    /// `None` without `/dev/lucina-gpu`, or when `LUCINA_VIDEO_GPU=0`.
    pub fn open() -> Option<GpuPresent> {
        if std::env::var_os("LUCINA_VIDEO_GPU").is_some_and(|v| v == "0") {
            return None;
        }
        let mut gpu = Gpu::open()?;
        let behind_scan = gpu.has_vblank() && std::env::var_os("LUCINA_VIDEO_VBLANK").is_none_or(|v| v != "0");
        Some(GpuPresent { gpu, held: None, batch: Batch::new(), behind_scan })
    }

    pub fn behind_scan(&self) -> bool {
        self.behind_scan
    }

    /// Copies a picture's planes into the GPU's buffer, making one for a new size.
    pub fn upload(&mut self, p: &Planes) -> io::Result<YuvLayout> {
        let layout = YuvLayout::packed(p.w as u32, p.h as u32, false);
        if self.held.as_ref().is_none_or(|(_, l)| *l != layout) {
            if let Some((old, _)) = self.held.take() {
                self.gpu.free(old)?;
            }
            let buffer = self.gpu.bytes_buffer(layout.bytes())?;
            self.held = Some((buffer, layout));
        }
        let (buffer, layout) = self.held.as_mut().expect("just made");
        copy_planes(p, layout, buffer.bytes_mut());
        Ok(*layout)
    }

    /// The video block's NV12 picture into the GPU's buffer with one copy, its own layout kept (lines and the chroma's offset are
    /// multiples of 64, which `yuv_rules::fits` asks). The layout spans the crop's corner too, so that `from` can start there.
    pub fn upload_nv12(&mut self, n: &crate::decode::Nv12Picture) -> io::Result<YuvLayout> {
        let layout = YuvLayout {
            nv12: true,
            width: (n.crop.0 + n.w) as u32,
            height: (n.crop.1 + n.h) as u32,
            y_offset: 0,
            y_pitch: n.stride,
            c_offset: n.uv_offset,
            c_pitch: n.stride,
            v_offset: 0,
        };
        if layout.bytes() > n.data.len() {
            return Err(io::Error::other("an NV12 picture shorter than its layout"));
        }
        if self.held.as_ref().is_none_or(|(_, l)| *l != layout) {
            if let Some((old, _)) = self.held.take() {
                self.gpu.free(old)?;
            }
            let buffer = self.gpu.bytes_buffer(n.data.len())?;
            self.held = Some((buffer, layout));
        }
        let (buffer, layout) = self.held.as_mut().expect("just made");
        let out = buffer.bytes_mut();
        let len = n.data.len().min(out.len());
        out[..len].copy_from_slice(&n.data[..len]);
        Ok(*layout)
    }

    /// Draws `from` of the uploaded picture over `to` on the screen, and returns when it is there.
    pub fn draw(&mut self, from: Rect, to: Rect, filter: bool) -> io::Result<()> {
        let Some((buffer, layout)) = &self.held else {
            return Err(io::Error::other("no picture uploaded"));
        };
        let rows = layout.bytes().div_ceil(buffer.pitch as usize) as u32;
        // Video this size is HD unless it says otherwise, as on the CPU path (`lucina_video::Matrix::for_size`).
        let matrix = yuv_matrix(layout.height > 576, false);
        self.batch.clear();
        self.batch.clean(buffer, 0, rows);
        if self.behind_scan {
            self.batch.vblank((to.y + to.h) as u32);
            for (from, to) in strips(from, to, STRIPS) {
                self.batch.yuv(buffer, layout, from, to, &matrix, filter);
            }
        } else {
            self.batch.yuv(buffer, layout, from, to, &matrix, filter);
        }
        self.gpu.run(&self.batch)
    }
}

/// `p`'s planes into `out` where `layout` puts them, row by row: the decoder's lines are usually longer than the picture.
pub fn copy_planes(p: &Planes, layout: &YuvLayout, out: &mut [u8]) {
    let (cw, ch) = (p.w.div_ceil(2), p.h.div_ceil(2));
    let rows = |src: &[u8], stride: usize, w: usize, h: usize, at: usize, pitch: usize, out: &mut [u8]| {
        for y in 0..h {
            out[at + y * pitch..][..w].copy_from_slice(&src[y * stride..][..w]);
        }
    };
    rows(p.y, p.y_stride, p.w, p.h, layout.y_offset, layout.y_pitch, out);
    rows(p.u, p.uv_stride, cw, ch, layout.c_offset, layout.c_pitch, out);
    rows(p.v, p.uv_stride, cw, ch, layout.v_offset, layout.c_pitch, out);
}

/// A batch takes four pictures; four strips put the GPU's writes in the scan's order.
const STRIPS: i32 = 4;

/// `from` and `to` cut into `n` bands from the top, each band's source rows whole and its screen rows where those rows land.
pub fn strips(from: Rect, to: Rect, n: i32) -> Vec<(Rect, Rect)> {
    let n = n.clamp(1, from.h.max(1));
    let at = |i: i32| {
        let src = from.y + from.h * i / n;
        let dst = to.y + ((src - from.y) as i64 * to.h as i64 / from.h as i64) as i32;
        (src, dst)
    };
    (0..n)
        .map(|i| {
            let ((s0, d0), (s1, d1)) = (at(i), at(i + 1));
            (Rect { y: s0, h: s1 - s0, ..from }, Rect { y: d0, h: d1 - d0, ..to })
        })
        .filter(|(f, t)| f.h > 0 && t.h > 0)
        .collect()
}

/// The part of a picture of `src_h` rows, shown at `to`, that lies above screen row `limit` (the bar's top): what to sample and where.
/// `None` if none of it does.
pub fn above(src_w: u32, src_h: u32, to: Rect, limit: i32) -> Option<(Rect, Rect)> {
    let whole = Rect { x: 0, y: 0, w: src_w as i32, h: src_h as i32 };
    if to.y + to.h <= limit {
        return Some((whole, to));
    }
    let shown = limit - to.y;
    if shown <= 0 {
        return None;
    }
    let rows = ((shown as i64 * src_h as i64 + to.h as i64 / 2) / to.h as i64).clamp(1, src_h as i64) as i32;
    Some((Rect { h: rows, ..whole }, Rect { h: shown, ..to }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn planes_land_where_the_layout_says() {
        let (w, h) = (5usize, 3usize);
        let y: Vec<u8> = (0..8 * h as u8).collect();
        let u: Vec<u8> = (100..100 + 4 * 2).collect();
        let v: Vec<u8> = (200..200 + 4 * 2).collect();
        let p = Planes { w, h, y: &y, u: &u, v: &v, y_stride: 8, uv_stride: 4 };
        let layout = YuvLayout::packed(w as u32, h as u32, false);
        let mut out = vec![0u8; layout.bytes()];
        copy_planes(&p, &layout, &mut out);
        assert_eq!(&out[64..69], &[8, 9, 10, 11, 12]);
        assert_eq!(&out[layout.c_offset + 64..][..3], &[104, 105, 106]);
        assert_eq!(&out[layout.v_offset..][..3], &[200, 201, 202]);
        assert_eq!(out[69], 0, "nothing past the picture's width");
    }

    #[test]
    fn the_bar_cuts_the_picture_not_the_scale() {
        let to = Rect { x: 0, y: 140, w: 1920, h: 800 };
        assert_eq!(above(1920, 800, to, 1080), Some((Rect { x: 0, y: 0, w: 1920, h: 800 }, to)));
        let (from, cut) = above(1280, 534, to, 840).unwrap();
        assert_eq!(cut, Rect { x: 0, y: 140, w: 1920, h: 700 });
        assert_eq!(from.h, 467);
        assert_eq!(above(1280, 534, to, 100), None);
    }

    #[test]
    fn strips_cover_the_picture_once_from_the_top() {
        let (from, to) = (Rect { x: 0, y: 0, w: 1280, h: 534 }, Rect { x: 0, y: 273, w: 1920, h: 801 });
        let s = strips(from, to, 4);
        assert_eq!(s.len(), 4);
        assert_eq!((s[0].0.y, s[0].1.y), (0, 273));
        for w in s.windows(2) {
            assert_eq!(w[0].0.y + w[0].0.h, w[1].0.y);
            assert_eq!(w[0].1.y + w[0].1.h, w[1].1.y);
        }
        let last = s[3];
        assert_eq!((last.0.y + last.0.h, last.1.y + last.1.h), (534, 273 + 801));
        assert_eq!(strips(Rect { h: 2, ..from }, to, 4).len(), 2);
    }
}
