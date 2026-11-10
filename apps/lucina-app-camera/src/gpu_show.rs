//! The preview through the Adreno, as the Video app shows its pictures: the decoder's I420 planes are copied as they are into a
//! buffer the GPU reads, and the GPU converts them to RGB (full-range BT.601, as JPEG has it) and scales them onto the screen,
//! bilinear. The CPU's part per frame is the JPEG decode (on its own thread) and the copy. The camera-on mark is a small picture with
//! alpha, drawn over the frame in the same batch.
//!
//! Opt-in with `LUCINA_CAMERA_GPU=1` until a lit-room measurement shows it at least as fast as the CPU path (docs/research/
//! juno-camera.md section 6); otherwise, or without `/dev/lucina-gpu`, the preview converts and scales on the CPU.

use std::io;

use lucina_app::Rect;
use lucina_app::gpu::{Batch, Buffer, Gpu, YuvLayout, yuv_matrix};
use lucina_camera::convert::{self, I420};

use crate::preview::{MARK_RED, WHITE};

pub struct GpuShow {
    gpu: Gpu,
    /// The buffer of the last picture size, and where its planes are in it.
    held: Option<(Buffer, YuvLayout)>,
    /// The mark's picture and its radius: made at the first frame, for the size the picture has on the screen.
    mark: Option<(Buffer, usize)>,
    batch: Batch,
    /// Wait for the panel's scan to pass the picture before drawing, so that it does not tear.
    behind_scan: bool,
    /// Whether the screen around the picture is black yet.
    cleared: bool,
}

/// The camera-on mark's radius for a picture `w` x `h` on the screen, as [`crate::preview::draw_mark`] draws it.
pub fn mark_radius(w: usize, h: usize) -> usize {
    (w.min(h) / 60).max(6)
}

/// The mark's pixels for radius `r`: a square of side `2 * (r + 2) + 1`, `0xAARRGGBB`, red dot, white ring, transparent outside.
pub fn mark_pixels(r: usize) -> (usize, Vec<u32>) {
    let ring = (r + 2) as i64;
    let side = 2 * ring as usize + 1;
    let r = r as i64;
    let mut px = vec![0u32; side * side];
    for y in 0..side {
        for x in 0..side {
            let (dx, dy) = (x as i64 - ring, y as i64 - ring);
            let d2 = dx * dx + dy * dy;
            px[y * side + x] = if d2 <= r * r {
                0xff00_0000 | MARK_RED
            } else if d2 <= ring * ring {
                0xff00_0000 | WHITE
            } else {
                0
            };
        }
    }
    (side, px)
}

/// Where the mark of radius `r` goes on the screen for a picture at `pic` (x, y, w, h): centred `3 r` in from its top right
/// corner, as on the CPU path.
pub fn mark_rect(pic: (usize, usize, usize, usize), r: usize) -> Rect {
    let (x, y, w, _) = pic;
    let ring = r + 2;
    let (cx, cy) = (x + w - 3 * r, y + 3 * r);
    Rect {
        x: (cx - ring) as i32,
        y: (cy - ring) as i32,
        w: (2 * ring + 1) as i32,
        h: (2 * ring + 1) as i32,
    }
}

/// `p`'s planes into `out` where `layout` puts them, row by row (the GPU wants lines in multiples of 64 bytes). Every plane is
/// checked first (`lucina_uvc::rules::i420_size` and `rows_fit`, Verus-checked): `false`, and nothing copied, if any plane is
/// shorter than its size says or would land outside `out`.
pub fn copy_planes(p: &I420, layout: &YuvLayout, out: &mut [u8]) -> bool {
    use lucina_uvc::rules::{i420_size, rows_fit};
    let (w, h) = (p.width, p.height);
    let (cw, ch) = (w.div_ceil(2), h.div_ceil(2));
    let size = i420_size(w, h);
    let planes = [
        (&p.y[..], w, h, layout.y_offset, layout.y_pitch),
        (&p.u[..], cw, ch, layout.c_offset, layout.c_pitch),
        (&p.v[..], cw, ch, layout.v_offset, layout.c_pitch),
    ];
    if !size.ok
        || p.y.len() < size.y
        || p.u.len() < size.c
        || p.v.len() < size.c
        || !planes.iter().all(|&(src, row, rows, at, pitch)| {
            rows_fit(src.len(), row, rows, at, pitch, out.len())
        })
    {
        return false;
    }
    for (src, row, rows, at, pitch) in planes {
        for (y, line) in src.chunks_exact(row).take(rows).enumerate() {
            out[at + y * pitch..][..row].copy_from_slice(line);
        }
    }
    true
}

impl GpuShow {
    /// `None` without a GPU, or unless `LUCINA_CAMERA_GPU=1`.
    pub fn open() -> Option<GpuShow> {
        if std::env::var_os("LUCINA_CAMERA_GPU").is_none_or(|v| v != "1") {
            return None;
        }
        let mut gpu = Gpu::open()?;
        let behind_scan =
            gpu.has_vblank() && std::env::var_os("LUCINA_CAMERA_VBLANK").is_none_or(|v| v != "0");
        Some(GpuShow {
            gpu,
            held: None,
            mark: None,
            batch: Batch::new(),
            behind_scan,
            cleared: false,
        })
    }

    /// Something else drew on the whole screen (the photo's white flash): clear it again before the next picture.
    pub fn screen_changed(&mut self) {
        self.cleared = false;
    }

    /// Copies a picture's planes into the GPU's buffer, making one for a new size.
    pub fn upload(&mut self, p: &I420) -> io::Result<()> {
        if !lucina_uvc::rules::i420_size(p.width, p.height).ok {
            return Err(io::Error::other("a picture with no size, or too large"));
        }
        let layout = YuvLayout::packed(p.width as u32, p.height as u32, false);
        if self.held.as_ref().is_none_or(|(_, l)| *l != layout) {
            if let Some((old, _)) = self.held.take() {
                self.gpu.free(old)?;
            }
            let buffer = self.gpu.bytes_buffer(layout.bytes())?;
            self.held = Some((buffer, layout));
        }
        let (buffer, layout) = self.held.as_mut().expect("just made");
        if copy_planes(p, layout, buffer.bytes_mut()) {
            Ok(())
        } else {
            Err(io::Error::other(
                "a picture whose planes do not match its size",
            ))
        }
    }

    /// Draws the uploaded picture fitted to the screen (`sw` x `sh`), with the mark, and returns when it is there.
    pub fn draw(&mut self, sw: usize, sh: usize) -> io::Result<()> {
        let Some((buffer, layout)) = &self.held else {
            return Err(io::Error::other("no picture uploaded"));
        };
        let (w, h) = (layout.width as usize, layout.height as usize);
        let pic = convert::fit(w, h, sw, sh);
        let r = mark_radius(pic.2, pic.3);
        if self.mark.as_ref().is_none_or(|(_, mr)| *mr != r) {
            if let Some((old, _)) = self.mark.take() {
                self.gpu.free(old)?;
            }
            let (side, px) = mark_pixels(r);
            let mut b = self.gpu.buffer(side as u32, side as u32)?;
            for (y, line) in px.chunks_exact(side).enumerate() {
                b.row(y as u32)[..side].copy_from_slice(line);
            }
            self.mark = Some((b, r));
        }
        let (mark, _) = self.mark.as_ref().expect("just made");
        let to = Rect {
            x: pic.0 as i32,
            y: pic.1 as i32,
            w: pic.2 as i32,
            h: pic.3 as i32,
        };
        let from = Rect {
            x: 0,
            y: 0,
            w: w as i32,
            h: h as i32,
        };
        let rows = layout.bytes().div_ceil(buffer.pitch as usize) as u32;
        self.batch.clear();
        if !self.cleared {
            self.batch.fill(
                Rect {
                    x: 0,
                    y: 0,
                    w: sw as i32,
                    h: sh as i32,
                },
                0,
            );
            self.cleared = true;
        }
        self.batch
            .clean(buffer, 0, rows)
            .clean(mark, 0, mark.height);
        if self.behind_scan {
            self.batch.vblank((to.y + to.h) as u32);
        }
        self.batch
            .yuv(buffer, layout, from, to, &yuv_matrix(false, true), true)
            .quad(mark, mark.bounds(), mark_rect(pic, r), !0, true, false);
        self.gpu.run(&self.batch)
    }
}
