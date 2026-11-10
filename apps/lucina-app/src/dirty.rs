//! Which parts of the frame changed since the last time: a hash per tile, so an app simply redraws
//! everything and only the tiles that differ are copied to the (slow, write-combined) framebuffer.
//! This costs a few KiB instead of a second 8 MiB frame.
//!
//! An app's tiles hash what it asked for ([`Tiles::diff_ops`]), which is cheap, and only what changed is then drawn. The shell
//! draws straight into its frame, so its tiles hash the pixels ([`Tiles::diff`]).

use crate::gfx::{Frame, Rect, Recording};

const TILE_W: usize = 64;
const TILE_H: usize = 32;

/// The hash of every tile of the last frame presented.
#[derive(Default)]
pub struct Tiles {
    cols: usize,
    rows: usize,
    hashes: Vec<u64>,
    now: Vec<u64>,
}

pub(crate) fn mix(h: u64, v: u64) -> u64 {
    (h ^ v).wrapping_mul(0x9e37_79b9_7f4a_7c15).rotate_left(29)
}

impl Tiles {
    /// The rectangles of `frame` that changed, joined where they can be. The first call returns the
    /// whole frame. Assumes that what these rectangles say is then copied to the screen.
    pub fn diff(&mut self, frame: &Frame) -> Vec<Rect> {
        let (w, h) = (frame.w as usize, frame.h as usize);
        if w == 0 || h == 0 {
            return Vec::new();
        }
        let (cols, rows) = (w.div_ceil(TILE_W), h.div_ceil(TILE_H));
        if (cols, rows) != (self.cols, self.rows) {
            self.cols = cols;
            self.rows = rows;
            self.hashes.clear();
        }
        self.now.clear();
        self.now.resize(cols * rows, 0);
        for y in 0..h {
            let row_hashes = &mut self.now[(y / TILE_H) * cols..][..cols];
            let line = &frame.px[y * w..(y + 1) * w];
            for (tile, px) in row_hashes.iter_mut().zip(line.chunks(TILE_W)) {
                let mut t = *tile;
                let (pairs, last) = px.as_chunks::<2>();
                for p in pairs {
                    t = mix(t, (p[0] as u64) << 32 | p[1] as u64);
                }
                if let [p] = last {
                    t = mix(t, *p as u64);
                }
                // Tiles are zero-based: a blank tile must not hash to its starting value.
                *tile = mix(t, 1);
            }
        }
        self.changed(w, h)
    }

    /// The same for a frame of `w` x `h` about to be drawn from `rec`: which tiles would come out differently.
    pub(crate) fn diff_ops(&mut self, w: usize, h: usize, rec: &Recording) -> Vec<Rect> {
        if w == 0 || h == 0 {
            return Vec::new();
        }
        let (cols, rows) = (w.div_ceil(TILE_W), h.div_ceil(TILE_H));
        if (cols, rows) != (self.cols, self.rows) {
            self.cols = cols;
            self.rows = rows;
            self.hashes.clear();
        }
        self.now.clear();
        self.now.resize(cols * rows, 0);
        for op in &rec.ops {
            let b = op.bounds;
            // `bounds` is inside the frame: it was clipped to it.
            let (c0, c1) = (b.x as usize / TILE_W, (b.right() as usize - 1) / TILE_W);
            let (r0, r1) = (b.y as usize / TILE_H, (b.bottom() as usize - 1) / TILE_H);
            for r in r0..=r1 {
                for tile in &mut self.now[r * cols + c0..=r * cols + c1] {
                    *tile = mix(*tile, op.hash);
                }
            }
        }
        self.changed(w, h)
    }

    /// `now` against `hashes`, and `now` becomes `hashes`.
    fn changed(&mut self, w: usize, h: usize) -> Vec<Rect> {
        let (cols, rows) = (self.cols, self.rows);
        let mut out: Vec<Rect> = Vec::new();
        let tile_rect = |c0: usize, c1: usize, r: usize| {
            let x = (c0 * TILE_W) as i32;
            let y = (r * TILE_H) as i32;
            let right = ((c1 + 1) * TILE_W).min(w) as i32;
            let bottom = ((r + 1) * TILE_H).min(h) as i32;
            Rect::new(x, y, right - x, bottom - y)
        };
        let first = self.hashes.is_empty();
        for r in 0..rows {
            let mut c = 0;
            while c < cols {
                let i = r * cols + c;
                if !first && self.now[i] == self.hashes[i] {
                    c += 1;
                    continue;
                }
                let start = c;
                while c + 1 < cols
                    && (first || self.now[r * cols + c + 1] != self.hashes[r * cols + c + 1])
                {
                    c += 1;
                }
                let run = tile_rect(start, c, r);
                // A run exactly under an earlier one of the same width extends it.
                if let Some(prev) = out
                    .iter_mut()
                    .rev()
                    .find(|p| p.x == run.x && p.w == run.w && p.bottom() == run.y)
                {
                    prev.h += run.h;
                } else {
                    out.push(run);
                }
                c += 1;
            }
        }
        std::mem::swap(&mut self.hashes, &mut self.now);
        out
    }

    /// Forget the frame: the next [`diff`](Self::diff) returns everything.
    pub fn reset(&mut self) {
        self.hashes.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gfx::Rgb;

    #[test]
    fn first_is_everything_then_only_what_changed() {
        let mut f = Frame::new(200, 100);
        let mut t = Tiles::default();
        assert_eq!(t.diff(&f), vec![Rect::new(0, 0, 200, 100)]);
        assert!(t.diff(&f).is_empty());
        f.fill_rect(Rect::new(70, 40, 5, 5), Rgb::hex(0x123456));
        assert_eq!(t.diff(&f), vec![Rect::new(64, 32, 64, 32)]);
        assert!(t.diff(&f).is_empty());
        // Back to what it was: that is a change too.
        f.fill_rect(Rect::new(70, 40, 5, 5), Rgb(0));
        assert_eq!(t.diff(&f), vec![Rect::new(64, 32, 64, 32)]);
    }

    #[test]
    fn runs_join_across_columns_and_rows() {
        let mut f = Frame::new(256, 128);
        let mut t = Tiles::default();
        t.diff(&f);
        f.fill_rect(Rect::new(0, 0, 256, 2), Rgb::hex(1));
        assert_eq!(t.diff(&f), vec![Rect::new(0, 0, 256, 32)]);
        f.fill_rect(Rect::new(70, 10, 2, 100), Rgb::hex(2));
        assert_eq!(t.diff(&f), vec![Rect::new(64, 0, 64, 128)]);
    }

    #[test]
    fn the_last_partial_tile_is_clipped_to_the_frame() {
        let mut f = Frame::new(70, 40);
        let mut t = Tiles::default();
        t.diff(&f);
        f.fill_rect(Rect::new(69, 39, 1, 1), Rgb::hex(3));
        assert_eq!(t.diff(&f), vec![Rect::new(64, 32, 6, 8)]);
        t.reset();
        assert_eq!(t.diff(&f), vec![Rect::new(0, 0, 70, 40)]);
    }
}
