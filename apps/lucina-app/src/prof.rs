//! Where a frame's CPU time goes, for `view_bench --parts`: clocked regions and counters, read by
//! the example. The markers are calls into this module, and each one is a `cfg!`-false branch in a
//! build without the `prof` feature, so the shipped toolkit is the unmeasured one.
//!
//! A region is bound to a scope and covers whole drawing calls, never a pixel or a glyph: the clock
//! itself costs about 15 ns, so a marker in a per-glyph loop would measure the marker. Where a part
//! has to be split further it is split by counting, and the counts are per string or per op.
//!
//! Nesting, and only this: [`REPLAY`] holds [`FILL`], [`SHAPE`], [`WALK_DRAW`] and [`RASTER`];
//! [`VIEW`] holds [`WALK_REC`], [`RASTER`] and, in the dirty-tile pass, [`REPLAY`] is outside it.
//! In the whole-frame redraw the drawing happens inside [`VIEW`] and there is no [`REPLAY`].

use std::sync::atomic::{AtomicU64, Ordering::Relaxed};

/// The app's `view` pass: it draws, or writes down what would draw.
pub const VIEW: usize = 0;
/// Which tiles of the frame changed, by pixel hash or by op hash.
pub const TILE: usize = 1;
/// Drawing what changed: the recorded ops, over the changed tiles.
pub const REPLAY: usize = 2;
/// A `fill_rect`'s pixel loop.
pub const FILL: usize = 3;
/// A rounded corner or a disc, blended pixel by pixel.
pub const SHAPE: usize = 4;
/// A string's glyphs while the frame is being written down: the cache lookup and the advance, no pixels.
pub const WALK_REC: usize = 5;
/// A string's glyphs as they are put on the frame: the lookup, the advance and the blit.
pub const WALK_DRAW: usize = 6;
/// A glyph's outline, drawn once on a cache miss.
pub const RASTER: usize = 7;
/// An op written down for the dirty-tile pass: its hash and the push.
pub const RECORD: usize = 8;

/// The regions, named in order.
pub const REGIONS: [&str; 9] = [
    "view pass",
    "tile diff",
    "replay",
    "flat fill",
    "rounded shape",
    "glyph walk (record)",
    "glyph walk (draw)",
    "glyph rasterise",
    "record + hash",
];

/// Strings drawn or written down.
pub const STRINGS: usize = 0;
/// Characters those strings hold: one cache lookup each.
pub const CHARS: usize = 1;
/// Lookups that had to draw the outline first.
pub const MISS: usize = 2;
/// Ops written down, all kinds.
pub const OPS: usize = 3;
/// Of those, the ones that touch a single pixel: a rounded corner is that many ops.
pub const OPS_BLEND: usize = 4;
/// Of those, whole rectangles.
pub const OPS_FILL: usize = 5;
/// Of those, rounded rectangles and discs.
pub const OPS_SHAPE: usize = 6;
/// Of those, strings.
pub const OPS_TEXT: usize = 7;
/// Of those, pictures and pixel copies.
pub const OPS_PIX: usize = 8;
/// Calls that composite a translucent token over a surface (`ui::tint`, `ui::row_selected`).
pub const TOKEN: usize = 9;

/// The counters, named in order.
pub const COUNTERS: [&str; 10] = [
    "strings",
    "characters",
    "cache misses",
    "ops",
    "of which one-pixel blends",
    "of which rectangles",
    "of which rounded",
    "of which strings",
    "of which pictures",
    "token composites",
];

static NS: [AtomicU64; REGIONS.len()] = [const { AtomicU64::new(0) }; REGIONS.len()];
static CALLS: [AtomicU64; REGIONS.len()] = [const { AtomicU64::new(0) }; REGIONS.len()];
static COUNT: [AtomicU64; COUNTERS.len()] = [const { AtomicU64::new(0) }; COUNTERS.len()];

fn now_ns() -> u64 {
    let mut ts = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: writes one timespec the caller owns.
    unsafe { libc::clock_gettime(libc::CLOCK_PROCESS_CPUTIME_ID, &mut ts) };
    ts.tv_sec as u64 * 1_000_000_000 + ts.tv_nsec as u64
}

/// A clocked region: it times the scope its binding lives in.
pub struct Region {
    i: usize,
    t0: u64,
}

/// Start region `i`.
#[inline(always)]
pub fn region(i: usize) -> Region {
    Region {
        i,
        t0: if cfg!(feature = "prof") { now_ns() } else { 0 },
    }
}

impl Drop for Region {
    #[inline(always)]
    fn drop(&mut self) {
        if cfg!(feature = "prof") {
            NS[self.i].fetch_add(now_ns() - self.t0, Relaxed);
            CALLS[self.i].fetch_add(1, Relaxed);
        }
    }
}

/// Add `n` to counter `i`.
#[inline(always)]
pub fn count(i: usize, n: u64) {
    if cfg!(feature = "prof") {
        COUNT[i].fetch_add(n, Relaxed);
    }
}

/// The regions in ns and call counts, and the counters, since the last [`reset`](self).
pub fn read() -> (Vec<u64>, Vec<u64>, Vec<u64>) {
    let ns = NS.iter().map(|a| a.load(Relaxed)).collect();
    let calls = CALLS.iter().map(|a| a.load(Relaxed)).collect();
    let count = COUNT.iter().map(|a| a.load(Relaxed)).collect();
    (ns, calls, count)
}

/// Forget everything measured so far.
pub fn reset() {
    for a in NS.iter().chain(CALLS.iter()).chain(COUNT.iter()) {
        a.store(0, Relaxed);
    }
}

/// What one region costs before any of the work it times: the two clock readings and the two adds.
/// Measured by running empty regions, so a part that is smaller than this is not a part but a
/// rounding; [`reset`] then clears what the calibration itself recorded.
pub fn calibrate() -> u64 {
    if !cfg!(feature = "prof") {
        return 0;
    }
    let n = 200_000u64;
    for _ in 0..n {
        let _r = region(RECORD);
    }
    let ns = NS[RECORD].load(Relaxed) / n;
    reset();
    ns
}
