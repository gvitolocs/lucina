//! Generated backdrops (the night sky, the stairs in space), so the concept carries no photographs: deterministic for a seed, at any pixel size. Value
//! noise and fbm make the clouds, a few shapes make the stairs. Backdrops only: no illustrations (rules.md, rule 8). Swap any of them for a real picture
//! later: a screen only needs a [`Canvas`] the size of the phone.

use crate::canvas::{Canvas, Color};

/// xorshift64*: a tiny deterministic generator.
struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Rng {
        Rng(seed.wrapping_mul(0x9e37_79b9_7f4a_7c15) | 1)
    }
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_f491_4f6c_dd1d)
    }
    /// 0..1.
    fn f(&mut self) -> f32 {
        (self.next() >> 40) as f32 / (1u64 << 24) as f32
    }
}

/// A lattice value in 0..1 for (`x`, `y`) and `seed`.
fn hash(x: i32, y: i32, seed: u64) -> f32 {
    let mut h = (x as u64).wrapping_mul(0x8da6_b343)
        ^ (y as u64).wrapping_mul(0xd816_3841)
        ^ seed.wrapping_mul(0xcb1a_b31f);
    h ^= h >> 13;
    h = h.wrapping_mul(0x5bd1_e995);
    h ^= h >> 15;
    (h & 0xffff) as f32 / 65535.0
}

/// Smooth value noise, 0..1.
fn noise(x: f32, y: f32, seed: u64) -> f32 {
    let (xi, yi) = (x.floor() as i32, y.floor() as i32);
    let (tx, ty) = (x - xi as f32, y - yi as f32);
    let (sx, sy) = (tx * tx * (3.0 - 2.0 * tx), ty * ty * (3.0 - 2.0 * ty));
    let a = hash(xi, yi, seed);
    let b = hash(xi + 1, yi, seed);
    let c = hash(xi, yi + 1, seed);
    let d = hash(xi + 1, yi + 1, seed);
    let top = a + (b - a) * sx;
    let bot = c + (d - c) * sx;
    top + (bot - top) * sy
}

/// Five octaves of noise, 0..1.
fn fbm(x: f32, y: f32, seed: u64) -> f32 {
    let (mut v, mut amp, mut f, mut norm) = (0.0, 0.5, 1.0, 0.0);
    for o in 0..5 {
        v += noise(x * f, y * f, seed.wrapping_add(o)) * amp;
        norm += amp;
        amp *= 0.5;
        f *= 2.03;
    }
    v / norm
}

fn smooth(e0: f32, e1: f32, x: f32) -> f32 {
    let t = ((x - e0) / (e1 - e0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

fn lerp3(a: (f32, f32, f32), b: (f32, f32, f32), t: f32) -> (f32, f32, f32) {
    (
        a.0 + (b.0 - a.0) * t,
        a.1 + (b.1 - a.1) * t,
        a.2 + (b.2 - a.2) * t,
    )
}

fn rgb(c: u32) -> (f32, f32, f32) {
    (
        ((c >> 16) & 0xff) as f32,
        ((c >> 8) & 0xff) as f32,
        (c & 0xff) as f32,
    )
}

fn pack(c: (f32, f32, f32)) -> u32 {
    let q = |v: f32| v.round().clamp(0.0, 255.0) as u32;
    (q(c.0) << 16) | (q(c.1) << 8) | q(c.2)
}

/// A vertical ground from `top` to `bottom`.
fn ground(cv: &mut Canvas, top: u32, bottom: u32) {
    let (a, b) = (rgb(top), rgb(bottom));
    let (w, h) = (cv.w as usize, cv.h as usize);
    for y in 0..h {
        let c = pack(lerp3(a, b, y as f32 / h.max(1) as f32));
        cv.px[y * w..(y + 1) * w].fill(c);
    }
}

/// `n` stars per million pixels, 1-2 dp, where `keep(x, y)` (pixels) allows them.
fn stars(cv: &mut Canvas, per_mpx: f32, seed: u64, keep: impl Fn(f32, f32) -> bool) {
    let mut r = Rng::new(seed ^ 0x5747);
    let n = (cv.w as f32 * cv.h as f32 / 1e6 * per_mpx) as usize;
    for _ in 0..n {
        let (x, y) = (r.f() * cv.w as f32, r.f() * cv.h as f32);
        if !keep(x, y) {
            continue;
        }
        let b = r.f();
        let size = 0.35 + b * b * 0.9;
        let c = Color::hexa(
            if r.f() < 0.2 { 0xcfe0ff } else { 0xffffff },
            (60.0 + 195.0 * b * b) as u8,
        );
        cv.fill_circle(x / cv.k, y / cv.k, size, c);
        if b > 0.93 {
            cv.glow(x / cv.k, y / cv.k, 4.0, c.alpha(0.5));
        }
    }
}

/// The night sky: near-black navy fading to black, many small crisp stars of varied brightness (a few faintly blue), and a very
/// faint dark-blue haze along the diagonal from the lower left to the upper right. No planet, horizon, moon or coloured nebula:
/// it stays quiet under the glass. Deterministic for a seed, at any pixel size; the shell renders it once per panel and keeps it.
/// The night sky's ground alone (navy to black, no haze, no stars): a fill per row, for a first frame that cannot wait for the
/// sky.
pub fn night_ground(w: u32, h: u32, k: f32) -> Canvas {
    let mut cv = Canvas::new(w, h, k);
    ground(&mut cv, 0x070b18, 0x010205);
    cv
}

pub fn night_sky(w: u32, h: u32, k: f32, seed: u64) -> Canvas {
    sky(w, h, k, seed, true)
}

/// The night sky without its stars: what the glass shows through its blur (the stars are too small to survive it). Draw it at
/// half the size for [`crate::home`]'s glass.
pub fn night_sky_ground(w: u32, h: u32, k: f32, seed: u64) -> Canvas {
    sky(w, h, k, seed, false)
}

/// How many haze samples across the picture and down it: the haze is slow, so a coarse grid interpolated per pixel looks the
/// same as computing it everywhere, at a hundredth of the cost. The shell makes the sky at start instead of reading 10 MB.
const HAZE_X: usize = 241;
const HAZE_Y: usize = 137;

fn sky(w: u32, h: u32, k: f32, seed: u64, with_stars: bool) -> Canvas {
    let mut cv = Canvas::new(w, h, k);
    ground(&mut cv, 0x070b18, 0x010205);
    // The haze's strength at each grid point: strongest on the diagonal, broken up by slow noise, never more than a few levels
    // above the ground. Positions are in this picture's pixels and the noise scale in dp, so any size gives the same sky.
    let (fw, fh) = (w as f32, h as f32);
    let len = (fw * fw + fh * fh).sqrt();
    let scale = 1.0 / (260.0 * k);
    let mut grid = vec![0.0f32; HAZE_X * HAZE_Y];
    for j in 0..HAZE_Y {
        for i in 0..HAZE_X {
            let fx = i as f32 / (HAZE_X - 1) as f32 * fw;
            let fy = j as f32 / (HAZE_Y - 1) as f32 * fh;
            // Distance from the line through the lower-left and upper-right corners, as a share of the diagonal.
            let d = ((fx * fh + fy * fw - fw * fh) / len).abs() / len;
            let band = (-(d / 0.16).powi(2)).exp();
            let cloud = smooth(0.35, 0.8, fbm(fx * scale, fy * scale, seed ^ 0x4a5e));
            grid[j * HAZE_X + i] = band * (0.25 + 0.75 * cloud) * 0.32;
        }
    }
    let haze = rgb(0x0e1d44);
    let (sx, sy) = (
        (HAZE_X - 1) as f32 / fw.max(1.0),
        (HAZE_Y - 1) as f32 / fh.max(1.0),
    );
    let mut row_a = [0.0f32; HAZE_X];
    let mut lut = [0u32; 256];
    for y in 0..h as usize {
        let gy = (y as f32 + 0.5) * sy;
        let j = (gy.floor() as usize).min(HAZE_Y - 2);
        let ty = gy - j as f32;
        for (i, v) in row_a.iter_mut().enumerate() {
            let (t, b) = (grid[j * HAZE_X + i], grid[(j + 1) * HAZE_X + i]);
            *v = t + (b - t) * ty;
        }
        // The row's ground under each of 256 haze strengths: one table per row instead of the blend per pixel.
        let base = rgb(cv.px[y * w as usize]);
        if row_a.iter().all(|&v| v <= 0.004) {
            continue;
        }
        for (q, c) in lut.iter_mut().enumerate() {
            *c = pack(lerp3(base, haze, q as f32 / 255.0));
        }
        let row = &mut cv.px[y * w as usize..(y + 1) * w as usize];
        for (x, px) in row.iter_mut().enumerate() {
            let gx = (x as f32 + 0.5) * sx;
            let i = (gx as usize).min(HAZE_X - 2);
            let tx = gx - i as f32;
            let a = row_a[i] + (row_a[i + 1] - row_a[i]) * tx;
            if a > 0.004 {
                *px = lut[((a * 255.0 + 0.5) as usize).min(255)];
            }
        }
    }
    if !with_stars {
        return cv;
    }
    // Stars: many and small, most of them dim; a rare bright one keeps a tiny glow.
    let mut r = Rng::new(seed ^ 0x51a7);
    let n = (fw * fh / 1e6 * 900.0) as usize;
    for _ in 0..n {
        let (x, y) = (r.f() * fw, r.f() * fh);
        let b = r.f();
        let b3 = b * b * b;
        let size = 0.28 + b3 * 0.55;
        let tint = if r.f() < 0.18 { 0xc4d6ff } else { 0xf4f6ff };
        let c = Color::hexa(tint, (34.0 + 150.0 * b3) as u8);
        cv.fill_circle(x / k, y / k, size, c);
        if b > 0.985 {
            cv.glow(x / k, y / k, 2.6, c.alpha(0.35));
        }
    }
    cv
}

/// White stairs in space: near-black space with soft stars, a white staircase rising from the lower left toward the upper
/// right with nothing holding it, a pale planet crescent far away. No figures (docs/design/rules.md, rule 8).
pub fn stairs_in_space(w: u32, h: u32, k: f32, seed: u64) -> Canvas {
    let mut cv = Canvas::new(w, h, k);
    ground(&mut cv, 0x08090c, 0x15161b);
    stars(&mut cv, 140.0, seed, |_, _| true);
    let b = cv.bounds();
    let (bw, bh) = (b.w, b.h);
    // The crescent: a pale arc, upper left.
    cv.arc(
        bw * 0.2,
        bh * 0.16,
        bw * 0.13,
        1.6,
        3.6,
        2.1,
        Color::hexa(0xcfd8e6, 100),
    );
    cv.glow(bw * 0.12, bh * 0.2, bw * 0.12, Color::hexa(0x8fa4c8, 22));
    // The staircase, in dp: step i's front-top corner, a step's run and rise, and the depth vector into the scene.
    let n = 12;
    let (x0, y0) = (bw * -0.04, bh * 0.93);
    let (sx, sy) = (bw * 0.072, bh * 0.043);
    let depth = (bw * 0.26, -bh * 0.075);
    let top = Color::hex(0xf4f4f2);
    let riser = Color::hex(0xdedfdc);
    let side = Color::hex(0xc4c5c2);
    let corner = |i: usize| (x0 + i as f32 * sx, y0 - i as f32 * sy);
    // The long soft shadow, down to the right.
    let (lx, ly) = corner(n);
    cv.polygon(
        &[
            (x0 + bw * 0.1, y0 + bh * 0.04),
            (lx + bw * 0.25, ly + bh * 0.25),
            (lx + bw * 0.55, ly + bh * 0.32),
            (x0 + bw * 0.42, y0 + bh * 0.09),
        ],
        Color::hexa(0x000000, 70),
    );
    for i in (0..n).rev() {
        let (x, y) = corner(i + 1);
        // Riser: the vertical face of this step.
        cv.polygon(
            &[
                (x, y),
                (x + depth.0, y + depth.1),
                (x + depth.0, y + depth.1 + sy),
                (x, y + sy),
            ],
            riser,
        );
        // Tread: the top face.
        cv.polygon(
            &[
                (x, y),
                (x + sx, y),
                (x + sx + depth.0, y + depth.1),
                (x + depth.0, y + depth.1),
            ],
            top,
        );
        cv.line(
            x,
            y,
            x + depth.0,
            y + depth.1,
            0.5,
            Color::hexa(0xffffff, 200),
        );
    }
    // The near side: the stairs' profile and a slanted underside.
    let mut profile = Vec::new();
    for i in 0..n {
        let (x, y) = corner(i + 1);
        profile.push((x, y + sy));
        profile.push((x, y));
        profile.push((x + sx, y));
    }
    let (ex, ey) = corner(n + 1);
    profile.push((ex, ey + sy * 2.4));
    profile.push((x0 + sx, y0 + sy * 2.6));
    cv.polygon(&profile, side);
    cv
}

#[cfg(test)]
mod tests {
    use super::*;

    fn distinct(cv: &Canvas) -> usize {
        let mut v = cv.px.clone();
        v.sort_unstable();
        v.dedup();
        v.len()
    }

    fn mean(cv: &Canvas, y0: u32, y1: u32) -> f32 {
        let mut s = 0.0;
        let mut n = 0.0;
        for y in y0..y1 {
            for x in 0..cv.w {
                let p = cv.px[(y * cv.w + x) as usize];
                s += (((p >> 16) & 0xff) + ((p >> 8) & 0xff) + (p & 0xff)) as f32;
                n += 1.0;
            }
        }
        s / n
    }

    #[test]
    fn deterministic_and_rich() {
        type Make = fn(u32, u32, f32, u64) -> Canvas;
        // The night sky is quiet on purpose: fewer colours than the stairs' picture, still a smooth ground, not bands.
        let all: [(Make, usize); 2] = [(night_sky, 300), (stairs_in_space, 600)];
        for (f, least) in all {
            for (w, h) in [(360, 800), (400, 300)] {
                let a = f(w, h, 1.0, 7);
                let b = f(w, h, 1.0, 7);
                assert!(a.px == b.px, "not deterministic at {w}x{h}");
                assert!(
                    distinct(&a) >= least,
                    "{w}x{h}: only {} colours",
                    distinct(&a)
                );
            }
        }
    }

    #[test]
    fn the_night_sky_is_dark_and_quiet() {
        let cv = night_sky(480, 270, 1.0, 7);
        // Dark on average, and no pixel brighter than a dim star: nothing competes with the glass.
        assert!(mean(&cv, 0, 270) < 45.0, "mean {}", mean(&cv, 0, 270));
        let brightest = cv
            .px
            .iter()
            .map(|p| ((p >> 16) & 0xff).max((p >> 8) & 0xff).max(p & 0xff))
            .max()
            .unwrap();
        assert!(brightest <= 215, "brightest {brightest}");
        // Stars are there: far more bright points than the ground and haze alone would give.
        let lit = cv.px.iter().filter(|p| ((*p >> 8) & 0xff) > 60).count();
        assert!(lit > 40, "only {lit} stars");
    }
}
