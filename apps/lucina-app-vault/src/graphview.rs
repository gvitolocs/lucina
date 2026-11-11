//! The graph as a picture: lines and discs into our own pixels, because the toolkit has no lines. Drawn when the view changes, not on
//! each frame.

use lucina_vault::graph::Graph;

const BG: u32 = 0x0c1028;
const EDGE: u32 = 0x2a3468;
const NODE: u32 = 0xc3cbee;
const HOT: u32 = 0x7f8fe0;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct View {
    pub zoom: f32,
    pub x: f32,
    pub y: f32,
}

/// The whole graph in a `w` x `h` picture.
pub fn fit(g: &Graph, w: i32, h: i32) -> View {
    let (mut x0, mut y0, mut x1, mut y1) = (f32::MAX, f32::MAX, f32::MIN, f32::MIN);
    for n in &g.nodes {
        x0 = x0.min(n.x);
        x1 = x1.max(n.x);
        y0 = y0.min(n.y);
        y1 = y1.max(n.y);
    }
    if g.nodes.is_empty() {
        return View {
            zoom: 1.0,
            x: 0.0,
            y: 0.0,
        };
    }
    let zoom = ((w as f32 - 40.0) / (x1 - x0).max(1.0))
        .min((h as f32 - 40.0) / (y1 - y0).max(1.0))
        .min(2.0);
    View {
        zoom,
        x: (x0 + x1) / 2.0,
        y: (y0 + y1) / 2.0,
    }
}

impl View {
    pub fn project(&self, x: f32, y: f32, w: i32, h: i32) -> (i32, i32) {
        (
            ((x - self.x) * self.zoom) as i32 + w / 2,
            ((y - self.y) * self.zoom) as i32 + h / 2,
        )
    }
}

fn line(px: &mut [u32], w: i32, h: i32, a: (i32, i32), b: (i32, i32), c: u32) {
    // Off-picture on one side: nothing to clip for.
    if (a.0 < 0 && b.0 < 0)
        || (a.1 < 0 && b.1 < 0)
        || (a.0 >= w && b.0 >= w)
        || (a.1 >= h && b.1 >= h)
    {
        return;
    }
    let (mut x, mut y) = a;
    let dx = (b.0 - a.0).abs();
    let dy = -(b.1 - a.1).abs();
    let sx = if a.0 < b.0 { 1 } else { -1 };
    let sy = if a.1 < b.1 { 1 } else { -1 };
    let mut err = dx + dy;
    // A very long edge when zoomed in: bounded, it only leaves the picture.
    for _ in 0..(dx - dy).min(8192) {
        if x >= 0 && y >= 0 && x < w && y < h {
            px[(y * w + x) as usize] = c;
        }
        if x == b.0 && y == b.1 {
            break;
        }
        let e2 = 2 * err;
        if e2 >= dy {
            err += dy;
            x += sx;
        }
        if e2 <= dx {
            err += dx;
            y += sy;
        }
    }
}

fn disc(px: &mut [u32], w: i32, h: i32, at: (i32, i32), r: i32, c: u32) {
    for dy in -r..=r {
        let y = at.1 + dy;
        if y < 0 || y >= h {
            continue;
        }
        for dx in -r..=r {
            let x = at.0 + dx;
            if x >= 0 && x < w && dx * dx + dy * dy <= r * r {
                px[(y * w + x) as usize] = c;
            }
        }
    }
}

pub fn radius(degree: u32, zoom: f32) -> i32 {
    let by_links = 1.0 + (degree as f32 + 1.0).log2() * 0.8;
    ((by_links * zoom.clamp(0.4, 2.0)).round() as i32).clamp(1, 12)
}

/// `hot` is the open note, in gold.
pub fn draw(g: &Graph, v: View, w: i32, h: i32, hot: Option<usize>) -> Vec<u32> {
    let mut px = vec![BG; (w.max(0) * h.max(0)) as usize];
    let at: Vec<(i32, i32)> = g.nodes.iter().map(|n| v.project(n.x, n.y, w, h)).collect();
    for &(a, b) in &g.edges {
        line(&mut px, w, h, at[a as usize], at[b as usize], EDGE);
    }
    for (i, n) in g.nodes.iter().enumerate() {
        let c = if Some(i) == hot { HOT } else { NODE };
        disc(&mut px, w, h, at[i], radius(n.degree, v.zoom), c);
    }
    px
}

/// The node nearest a point, within reach of a pointer.
pub fn nearest(g: &Graph, v: View, w: i32, h: i32, x: i32, y: i32) -> Option<usize> {
    let mut best = None;
    let mut best_d = 400;
    for (i, n) in g.nodes.iter().enumerate() {
        let p = v.project(n.x, n.y, w, h);
        let d = (p.0 - x).pow(2) + (p.1 - y).pow(2);
        if d < best_d {
            best_d = d;
            best = Some(i);
        }
    }
    best
}
