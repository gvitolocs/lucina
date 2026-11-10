//! Finding what changed on the screen, with the behaviour of lucina-app-desk's `host/capture.rs`: reading the framebuffer is
//! the slow part (write-combined memory: 73 ms for the whole 1080p screen on the laptop), so a round reads one row of each
//! band of 16, a different row each round, and reads a whole band only when that row differs. A change as small as a text
//! cursor shows within a few rounds. voxl hears of changes from the X server instead (XDamage); there is nothing like it here.

use crate::Screen;

/// Rows of each band of 16 read to find out nothing changed. One a band is about 5 ms on the laptop.
const SAMPLES: usize = 1;

pub struct Capture {
    w: usize,
    h: usize,
    cols: usize,
    bands: usize,
    /// The screen as last read, XRGB.
    px: Vec<u32>,
    row: Vec<u32>,
    /// 16x16 blocks that changed in the last scan, row-major.
    changed: Vec<bool>,
    round: usize,
}

impl Capture {
    pub fn new(w: usize, h: usize) -> Capture {
        let (cols, bands) = (w.div_ceil(16), h.div_ceil(16));
        Capture { w, h, cols, bands, px: vec![0; w * h], row: vec![0; w], changed: vec![false; cols * bands], round: 0 }
    }

    /// Looks at the screen and marks the blocks that changed; returns how many. `exact` reads every row, as after a pause
    /// with nobody watching or for a screenshot, so that nothing drawn meanwhile is missed.
    pub fn scan(&mut self, screen: &mut dyn Screen, exact: bool) -> usize {
        self.changed.iter_mut().for_each(|c| *c = false);
        self.round = self.round.wrapping_add(1);
        let mut n = 0;
        for band in 0..self.bands {
            let top = band * 16;
            let rows = (self.h - top).min(16);
            let mut differs = exact;
            if !differs {
                for s in 0..SAMPLES {
                    // Spread over the band, and moved on one row each round.
                    let r = (self.round + s * 16 / SAMPLES + band) % rows;
                    screen.read_row(top + r, &mut self.row);
                    if self.row != self.px[(top + r) * self.w..][..self.w] {
                        differs = true;
                        break;
                    }
                }
            }
            if !differs {
                continue;
            }
            for r in 0..rows {
                screen.read_row(top + r, &mut self.row);
                let line = &mut self.px[(top + r) * self.w..][..self.w];
                if *line == *self.row {
                    continue;
                }
                for (col, (now, was)) in self.row.chunks(16).zip(line.chunks_mut(16)).enumerate() {
                    if now != was {
                        let c = &mut self.changed[band * self.cols + col];
                        n += !*c as usize;
                        *c = true;
                        was.copy_from_slice(now);
                    }
                }
            }
        }
        n
    }

    /// The screen as last read.
    pub fn screen(&self) -> &[u32] {
        &self.px
    }

    pub fn changed(&self) -> &[bool] {
        &self.changed
    }

    pub fn cols(&self) -> usize {
        self.cols
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Flat {
        w: usize,
        px: Vec<u32>,
        rows_read: usize,
    }

    impl Screen for Flat {
        fn size(&self) -> (usize, usize) {
            (self.w, self.px.len() / self.w)
        }
        fn read_row(&mut self, y: usize, out: &mut [u32]) {
            self.rows_read += 1;
            out.copy_from_slice(&self.px[y * self.w..][..self.w]);
        }
    }

    #[test]
    fn nothing_changed_reads_one_row_a_band_and_a_change_marks_only_its_block() {
        let mut s = Flat { w: 64, px: vec![0xff10_2030; 64 * 64], rows_read: 0 };
        let mut c = Capture::new(64, 64);
        assert_eq!(c.scan(&mut s, true), 16);
        s.rows_read = 0;
        assert_eq!(c.scan(&mut s, false), 0);
        assert_eq!(s.rows_read, 4 * SAMPLES);
        // One pixel, as a text cursor's corner would be: found within a band's rounds, as block (2, 1).
        s.px[20 * 64 + 40] = 0xffff_ffff;
        let found = (0..16).find_map(|_| (c.scan(&mut s, false) > 0).then(|| c.changed().to_vec())).unwrap();
        let marked: Vec<usize> = found.iter().enumerate().filter(|(_, v)| **v).map(|(i, _)| i).collect();
        assert_eq!(marked, vec![4 + 2]);
        assert_eq!(c.screen()[20 * 64 + 40], 0xffff_ffff);
    }

    #[test]
    fn exact_finds_a_one_pixel_change_at_once_and_odd_sizes_work() {
        let mut s = Flat { w: 50, px: vec![0; 50 * 37], rows_read: 0 };
        let mut c = Capture::new(50, 37);
        assert_eq!(c.scan(&mut s, true), 0);
        s.px[36 * 50 + 49] = 1;
        assert_eq!(c.scan(&mut s, true), 1);
        assert!(c.changed()[2 * 4 + 3]);
    }
}
