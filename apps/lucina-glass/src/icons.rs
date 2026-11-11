//! Vector icons drawn with the canvas's shapes, so they scale to any density. Outline glyphs in the manner of a regular
//! system symbol set: strokes of 8.5 % of the box, round caps, about 80 % of the box used. Generic glyphs only: no
//! third-party marks (the calendar's "design tool", "mail", "ball", "send" and "doc" stand for apps without being them).

use std::f32::consts::{FRAC_PI_2, PI, TAU};

use crate::canvas::{Canvas, Color};

/// Every icon of the concept.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Icon {
    PlusCircle,
    ArrowUpRight,
    ArrowRight,
    Shuffle,
    Prev,
    Play,
    Pause,
    Next,
    Repeat,
    Volume,
    Heart,
    Share,
    ChevronUp,
    ChevronDown,
    Pen,
    Mail,
    Ball,
    Send,
    Doc,
    Bell,
    Bolt,
    Code,
    Link,
    Person,
    Upload,
    Clock,
    // The browser chrome's set (docs/design/browser-glass.md in LucinaNet).
    Back,
    Forward,
    Reload,
    Close,
    Plus,
    Minus,
    Lock,
    Search,
    Star,
    StarFill,
    Dots,
    Download,
    Gear,
    Private,
    Tabs,
    Check,
    Camera,
    Pin,
    Popup,
    Folder,
    Globe,
    ChevronRight,
}

/// Every icon, in declaration order (for the gallery's sheet).
pub const ALL: [Icon; 48] = [
    Icon::PlusCircle,
    Icon::ArrowUpRight,
    Icon::ArrowRight,
    Icon::Shuffle,
    Icon::Prev,
    Icon::Play,
    Icon::Pause,
    Icon::Next,
    Icon::Repeat,
    Icon::Volume,
    Icon::Heart,
    Icon::Share,
    Icon::ChevronUp,
    Icon::ChevronDown,
    Icon::Pen,
    Icon::Mail,
    Icon::Ball,
    Icon::Send,
    Icon::Doc,
    Icon::Bell,
    Icon::Bolt,
    Icon::Code,
    Icon::Link,
    Icon::Person,
    Icon::Upload,
    Icon::Clock,
    Icon::Back,
    Icon::Forward,
    Icon::Reload,
    Icon::Close,
    Icon::Plus,
    Icon::Minus,
    Icon::Lock,
    Icon::Search,
    Icon::Star,
    Icon::StarFill,
    Icon::Dots,
    Icon::Download,
    Icon::Gear,
    Icon::Private,
    Icon::Tabs,
    Icon::Check,
    Icon::Camera,
    Icon::Pin,
    Icon::Popup,
    Icon::Folder,
    Icon::Globe,
    Icon::ChevronRight,
];

/// Draw `icon` centred on (`cx`, `cy`) in a `size` dp box, strokes and fills in `color`.
pub fn draw(cv: &mut Canvas, icon: Icon, cx: f32, cy: f32, size: f32, c: Color) {
    // Coordinates relative to the centre, in units of the box (-0.5..0.5).
    let p = |x: f32, y: f32| (cx + x * size, cy + y * size);
    let sw = size * 0.085;
    let line = |cv: &mut Canvas, a: (f32, f32), b: (f32, f32)| {
        let (a, b) = (p(a.0, a.1), p(b.0, b.1));
        cv.line(a.0, a.1, b.0, b.1, sw, c);
    };
    let poly = |cv: &mut Canvas, pts: &[(f32, f32)]| {
        let v: Vec<(f32, f32)> = pts.iter().map(|&(x, y)| p(x, y)).collect();
        cv.polyline(&v, sw, c);
    };
    let fill = |cv: &mut Canvas, pts: &[(f32, f32)]| {
        let v: Vec<(f32, f32)> = pts.iter().map(|&(x, y)| p(x, y)).collect();
        cv.polygon(&v, c);
    };
    let arc = |cv: &mut Canvas, x: f32, y: f32, r: f32, start: f32, sweep: f32| {
        let (x, y) = p(x, y);
        cv.arc(x, y, r * size, sw, start, sweep, c);
    };
    let dot = |cv: &mut Canvas, x: f32, y: f32, r: f32| {
        let (x, y) = p(x, y);
        cv.fill_circle(x, y, r * size, c);
    };
    let boxr = |x: f32, y: f32, w: f32, h: f32| {
        let (px, py) = p(x, y);
        crate::canvas::R::new(px, py, w * size, h * size)
    };
    // A two-stroke arrow head at `tip` pointing along angle `dir` (radians, 0 = right, y down).
    let head = |cv: &mut Canvas, tip: (f32, f32), dir: f32, len: f32| {
        for s in [-1.0f32, 1.0] {
            let a = dir + PI + s * 0.75;
            line(cv, tip, (tip.0 + len * a.cos(), tip.1 + len * a.sin()));
        }
    };
    match icon {
        Icon::PlusCircle => {
            arc(cv, 0.0, 0.0, 0.4, 0.0, TAU);
            line(cv, (-0.17, 0.0), (0.17, 0.0));
            line(cv, (0.0, -0.17), (0.0, 0.17));
        }
        Icon::ArrowUpRight => {
            line(cv, (-0.3, 0.3), (0.3, -0.3));
            poly(cv, &[(-0.12, -0.3), (0.3, -0.3), (0.3, 0.12)]);
        }
        Icon::ArrowRight => {
            line(cv, (-0.36, 0.0), (0.36, 0.0));
            head(cv, (0.36, 0.0), 0.0, 0.26);
        }
        Icon::Shuffle => {
            let curve = |y0: f32, y1: f32| -> Vec<(f32, f32)> {
                (0..=8)
                    .map(|i| {
                        let t = i as f32 / 8.0;
                        let s = t * t * (3.0 - 2.0 * t);
                        (-0.38 + 0.7 * t, y0 + (y1 - y0) * s)
                    })
                    .collect()
            };
            poly(cv, &curve(0.24, -0.24));
            poly(cv, &curve(-0.24, 0.24));
            head(cv, (0.36, -0.24), 0.0, 0.14);
            head(cv, (0.36, 0.24), 0.0, 0.14);
        }
        Icon::Prev => {
            fill(cv, &[(0.3, -0.3), (0.3, 0.3), (-0.18, 0.0)]);
            let (a, b) = (p(-0.26, -0.3), p(-0.26, 0.3));
            cv.line(a.0, a.1, b.0, b.1, size * 0.11, c);
        }
        Icon::Play => {
            fill(cv, &[(-0.22, -0.34), (-0.22, 0.34), (0.36, 0.0)]);
        }
        Icon::Pause => {
            for x in [-0.15, 0.15] {
                let (a, b) = (p(x, -0.28), p(x, 0.28));
                cv.line(a.0, a.1, b.0, b.1, size * 0.15, c);
            }
        }
        Icon::Next => {
            fill(cv, &[(-0.3, -0.3), (-0.3, 0.3), (0.18, 0.0)]);
            let (a, b) = (p(0.26, -0.3), p(0.26, 0.3));
            cv.line(a.0, a.1, b.0, b.1, size * 0.11, c);
        }
        Icon::Repeat => {
            // A loop: top line left to right, round right end, bottom line right to left, round left end, with a gap.
            line(cv, (-0.2, -0.17), (0.22, -0.17));
            arc(cv, 0.22, 0.0, 0.17, 0.0, PI * 0.6);
            line(cv, (0.2, 0.17), (-0.22, 0.17));
            arc(cv, -0.22, 0.0, 0.17, PI, PI * 0.6);
            head(cv, (0.22, -0.17), 0.0, 0.12);
            head(cv, (-0.22, 0.17), PI, 0.12);
        }
        Icon::Volume => {
            fill(
                cv,
                &[
                    (-0.38, -0.1),
                    (-0.24, -0.1),
                    (-0.06, -0.28),
                    (-0.06, 0.28),
                    (-0.24, 0.1),
                    (-0.38, 0.1),
                ],
            );
            arc(cv, -0.04, 0.0, 0.16, FRAC_PI_2 - 0.85, 1.7);
            arc(cv, -0.04, 0.0, 0.32, FRAC_PI_2 - 0.85, 1.7);
        }
        Icon::Heart => {
            let pts: Vec<(f32, f32)> = (0..48)
                .map(|i| {
                    let t = i as f32 / 48.0 * TAU;
                    let x = 16.0 * t.sin().powi(3);
                    let y = 13.0 * t.cos()
                        - 5.0 * (2.0 * t).cos()
                        - 2.0 * (3.0 * t).cos()
                        - (4.0 * t).cos();
                    (x / 40.0, -y / 40.0 + 0.03)
                })
                .collect();
            fill(cv, &pts);
        }
        Icon::Share => {
            poly(
                cv,
                &[
                    (-0.14, -0.06),
                    (-0.28, -0.06),
                    (-0.28, 0.36),
                    (0.28, 0.36),
                    (0.28, -0.06),
                    (0.14, -0.06),
                ],
            );
            line(cv, (0.0, 0.14), (0.0, -0.38));
            head(cv, (0.0, -0.38), -FRAC_PI_2, 0.15);
        }
        Icon::ChevronUp => poly(cv, &[(-0.26, 0.13), (0.0, -0.13), (0.26, 0.13)]),
        Icon::ChevronDown => poly(cv, &[(-0.26, -0.13), (0.0, 0.13), (0.26, -0.13)]),
        Icon::Pen => {
            poly(
                cv,
                &[
                    (0.0, -0.4),
                    (0.28, 0.0),
                    (0.12, 0.36),
                    (-0.12, 0.36),
                    (-0.28, 0.0),
                    (0.0, -0.4),
                ],
            );
            line(cv, (0.0, -0.4), (0.0, -0.02));
            arc(cv, 0.0, 0.06, 0.07, 0.0, TAU);
        }
        Icon::Mail => {
            let (a, b) = (p(-0.38, -0.26), p(0.38, 0.26));
            cv.stroke_rrect(
                crate::canvas::R::new(a.0, a.1, b.0 - a.0, b.1 - a.1),
                size * 0.08,
                sw,
                c,
            );
            poly(cv, &[(-0.34, -0.2), (0.0, 0.06), (0.34, -0.2)]);
        }
        Icon::Ball => {
            arc(cv, 0.0, 0.0, 0.38, 0.0, TAU);
            arc(cv, -0.55, 0.0, 0.42, FRAC_PI_2 - 0.9, 1.8);
            arc(cv, 0.55, 0.0, 0.42, -FRAC_PI_2 - 0.9, 1.8);
        }
        Icon::Send => {
            poly(
                cv,
                &[
                    (-0.38, -0.02),
                    (0.38, -0.34),
                    (0.1, 0.36),
                    (-0.02, 0.06),
                    (-0.38, -0.02),
                ],
            );
            line(cv, (-0.02, 0.06), (0.38, -0.34));
        }
        Icon::Doc => {
            poly(
                cv,
                &[
                    (0.08, -0.4),
                    (-0.28, -0.4),
                    (-0.28, 0.4),
                    (0.28, 0.4),
                    (0.28, -0.2),
                    (0.08, -0.4),
                    (0.08, -0.2),
                    (0.28, -0.2),
                ],
            );
            for y in [0.0, 0.13, 0.26] {
                line(cv, (-0.15, y), (0.15, y));
            }
        }
        Icon::Bell => {
            arc(cv, 0.0, -0.06, 0.24, -FRAC_PI_2, PI);
            line(cv, (-0.24, -0.06), (-0.26, 0.2));
            line(cv, (0.24, -0.06), (0.26, 0.2));
            line(cv, (-0.36, 0.22), (0.36, 0.22));
            let (x, y) = p(0.0, 0.34);
            cv.fill_circle(x, y, size * 0.07, c);
            line(cv, (0.0, -0.38), (0.0, -0.32));
        }
        Icon::Bolt => fill(
            cv,
            &[
                (0.08, -0.42),
                (-0.26, 0.06),
                (-0.02, 0.06),
                (-0.1, 0.42),
                (0.26, -0.08),
                (0.02, -0.08),
            ],
        ),
        Icon::Code => {
            poly(cv, &[(-0.18, -0.22), (-0.38, 0.0), (-0.18, 0.22)]);
            poly(cv, &[(0.18, -0.22), (0.38, 0.0), (0.18, 0.22)]);
            line(cv, (0.07, -0.3), (-0.07, 0.3));
        }
        Icon::Link => {
            // Two capsules on a 45° line, overlapping.
            for (dx, dy) in [(-0.12, 0.12), (0.12, -0.12)] {
                let pts: Vec<(f32, f32)> = (0..=24)
                    .map(|i| {
                        let t = i as f32 / 24.0 * TAU;
                        let (u, v) = (0.22 * t.cos(), 0.1 * t.sin());
                        let r = -PI / 4.0;
                        (
                            dx + u * r.cos() - v * r.sin(),
                            dy + u * r.sin() + v * r.cos(),
                        )
                    })
                    .collect();
                poly(cv, &pts);
            }
        }
        Icon::Person => {
            arc(cv, 0.0, -0.16, 0.15, 0.0, TAU);
            arc(cv, 0.0, 0.42, 0.3, -FRAC_PI_2 * 0.92, PI * 0.92);
        }
        Icon::Upload => {
            line(cv, (0.0, 0.18), (0.0, -0.34));
            head(cv, (0.0, -0.34), -FRAC_PI_2, 0.18);
            poly(
                cv,
                &[(-0.32, 0.12), (-0.32, 0.34), (0.32, 0.34), (0.32, 0.12)],
            );
        }
        Icon::Clock => {
            arc(cv, 0.0, 0.0, 0.38, 0.0, TAU);
            poly(cv, &[(0.0, -0.22), (0.0, 0.0), (0.16, 0.1)]);
        }
        Icon::Back => poly(cv, &[(0.12, -0.32), (-0.16, 0.0), (0.12, 0.32)]),
        Icon::Forward | Icon::ChevronRight => {
            poly(cv, &[(-0.12, -0.32), (0.16, 0.0), (-0.12, 0.32)])
        }
        Icon::Reload => {
            let (start, sweep) = (-1.0f32, 4.7f32);
            arc(cv, 0.0, 0.0, 0.33, start, sweep);
            let a = start + sweep;
            let (px, py) = (0.33 * a.cos(), 0.33 * a.sin());
            let (tx, ty) = (-a.sin(), a.cos());
            let (rx, ry) = (a.cos(), a.sin());
            fill(
                cv,
                &[
                    (px + tx * 0.16, py + ty * 0.16),
                    (px + rx * 0.12, py + ry * 0.12),
                    (px - rx * 0.12, py - ry * 0.12),
                ],
            );
        }
        Icon::Close => {
            line(cv, (-0.32, -0.32), (0.32, 0.32));
            line(cv, (0.32, -0.32), (-0.32, 0.32));
        }
        Icon::Plus => {
            line(cv, (-0.3, 0.0), (0.3, 0.0));
            line(cv, (0.0, -0.3), (0.0, 0.3));
        }
        Icon::Minus => line(cv, (-0.3, 0.0), (0.3, 0.0)),
        Icon::Lock => {
            // 0.64 wide and 0.82 high of the box: at 20 dp it is 12.8 x 16.4 dp, as large as the star beside it.
            let b = boxr(-0.32, -0.02, 0.64, 0.44);
            cv.fill_rrect(b, 0.09 * size, c);
            arc(cv, 0.0, -0.18, 0.21, PI, PI);
            line(cv, (-0.21, -0.18), (-0.21, -0.02));
            line(cv, (0.21, -0.18), (0.21, -0.02));
        }
        Icon::Search => {
            arc(cv, -0.07, -0.07, 0.27, 0.0, TAU);
            line(cv, (0.13, 0.13), (0.36, 0.36));
        }
        Icon::Star | Icon::StarFill => {
            let pts: Vec<(f32, f32)> = (0..10)
                .map(|i| {
                    let a = -FRAC_PI_2 + i as f32 * PI / 5.0;
                    let r = if i % 2 == 0 { 0.4 } else { 0.18 };
                    (r * a.cos(), r * a.sin() + 0.03)
                })
                .collect();
            if icon == Icon::StarFill {
                fill(cv, &pts);
            }
            let mut closed = pts.clone();
            closed.push(pts[0]);
            poly(cv, &closed);
        }
        Icon::Dots => {
            for y in [-0.3, 0.0, 0.3] {
                dot(cv, 0.0, y, 0.065);
            }
        }
        Icon::Download => {
            line(cv, (0.0, -0.36), (0.0, 0.12));
            poly(cv, &[(-0.2, -0.08), (0.0, 0.14), (0.2, -0.08)]);
            poly(
                cv,
                &[(-0.34, 0.14), (-0.34, 0.34), (0.34, 0.34), (0.34, 0.14)],
            );
        }
        Icon::Gear => {
            arc(cv, 0.0, 0.0, 0.16, 0.0, TAU);
            for i in 0..8 {
                let a = i as f32 * TAU / 8.0;
                line(
                    cv,
                    (0.27 * a.cos(), 0.27 * a.sin()),
                    (0.38 * a.cos(), 0.38 * a.sin()),
                );
            }
            arc(cv, 0.0, 0.0, 0.27, 0.0, TAU);
        }
        Icon::Private => {
            line(cv, (-0.4, -0.06), (0.4, -0.06));
            poly(
                cv,
                &[(-0.2, -0.06), (-0.13, -0.34), (0.13, -0.34), (0.2, -0.06)],
            );
            arc(cv, -0.15, 0.21, 0.1, 0.0, TAU);
            arc(cv, 0.15, 0.21, 0.1, 0.0, TAU);
        }
        Icon::Tabs => {
            let b = boxr(-0.31, -0.31, 0.62, 0.62);
            cv.stroke_rrect(b, 0.12 * size, sw, c);
        }
        Icon::Check => poly(cv, &[(-0.3, 0.02), (-0.1, 0.24), (0.3, -0.22)]),
        Icon::Camera => {
            let b = boxr(-0.4, -0.2, 0.8, 0.52);
            cv.stroke_rrect(b, 0.1 * size, sw, c);
            arc(cv, 0.0, 0.06, 0.12, 0.0, TAU);
            poly(
                cv,
                &[(-0.12, -0.2), (-0.07, -0.32), (0.07, -0.32), (0.12, -0.2)],
            );
        }
        Icon::Pin => {
            arc(cv, 0.0, -0.1, 0.22, 0.0, TAU);
            poly(cv, &[(-0.2, 0.0), (0.0, 0.4), (0.2, 0.0)]);
            dot(cv, 0.0, -0.1, 0.06);
        }
        Icon::Popup => {
            cv.stroke_rrect(boxr(-0.38, -0.3, 0.56, 0.46), 0.08 * size, sw, c);
            cv.stroke_rrect(boxr(-0.18, -0.12, 0.56, 0.46), 0.08 * size, sw, c);
        }
        Icon::Folder => poly(
            cv,
            &[
                (-0.38, -0.24),
                (-0.38, 0.3),
                (0.38, 0.3),
                (0.38, -0.14),
                (0.04, -0.14),
                (-0.04, -0.24),
                (-0.38, -0.24),
            ],
        ),
        Icon::Globe => {
            arc(cv, 0.0, 0.0, 0.38, 0.0, TAU);
            arc(cv, 0.0, 0.0, 0.16, 0.0, TAU);
            line(cv, (-0.38, 0.0), (0.38, 0.0));
            line(cv, (0.0, -0.38), (0.0, 0.38));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_icon_draws_inside_its_box() {
        for icon in ALL {
            let mut cv = Canvas::new(64, 64, 1.0);
            cv.clear(Color::hex(0));
            draw(&mut cv, icon, 32.0, 32.0, 48.0, Color::hex(0xffffff));
            assert!(cv.px.iter().any(|&p| p != 0), "{icon:?} drew nothing");
            for y in 0..64 {
                for x in 0..64 {
                    if !(4..60).contains(&x) || !(4..60).contains(&y) {
                        assert_eq!(cv.px[y * 64 + x], 0, "{icon:?} spills at {x},{y}");
                    }
                }
            }
        }
    }

    /// The browser chrome's icons at the smallest size they are drawn (16 dp, at 3 px/dp): none may shrink to a speck, and
    /// the ink must stay inside the box.
    #[test]
    fn browser_icons_keep_a_16_dp_visual_size() {
        const NEW: [Icon; 22] = [
            Icon::Back,
            Icon::Forward,
            Icon::Reload,
            Icon::Close,
            Icon::Plus,
            Icon::Minus,
            Icon::Lock,
            Icon::Search,
            Icon::Star,
            Icon::StarFill,
            Icon::Dots,
            Icon::Download,
            Icon::Gear,
            Icon::Private,
            Icon::Tabs,
            Icon::Check,
            Icon::Camera,
            Icon::Pin,
            Icon::Popup,
            Icon::Folder,
            Icon::Globe,
            Icon::ChevronRight,
        ];
        for icon in NEW {
            // 16 dp box centred in a 24 dp canvas at 3 px/dp.
            let mut cv = Canvas::new(72, 72, 3.0);
            cv.clear(Color::hex(0));
            draw(&mut cv, icon, 12.0, 12.0, 16.0, Color::hex(0xffffff));
            let (mut x0, mut y0, mut x1, mut y1) = (72usize, 72usize, 0usize, 0usize);
            for y in 0..72 {
                for x in 0..72 {
                    if cv.px[y * 72 + x] != 0 {
                        (x0, y0, x1, y1) = (x0.min(x), y0.min(y), x1.max(x), y1.max(y));
                    }
                }
            }
            assert!(x1 >= x0, "{icon:?} drew nothing");
            let (w, h) = ((x1 - x0 + 1) as f32 / 3.0, (y1 - y0 + 1) as f32 / 3.0);
            // At least half of the box in its longer side, and nothing outside the 16 dp box plus a stroke's overhang.
            assert!(w.max(h) >= 8.0, "{icon:?} is only {w} x {h} dp at 16 dp");
            assert!(w <= 17.0 && h <= 17.0, "{icon:?} spills: {w} x {h} dp");
        }
    }

    #[test]
    fn nothing_but_the_filled_star_is_filled_like_it() {
        let ink = |icon| {
            let mut cv = Canvas::new(64, 64, 1.0);
            cv.clear(Color::hex(0));
            draw(&mut cv, icon, 32.0, 32.0, 48.0, Color::hex(0xffffff));
            cv.px.iter().filter(|&&p| p != 0).count()
        };
        assert!(
            ink(Icon::StarFill) > ink(Icon::Star),
            "the filled star has more ink than the outline"
        );
    }
}
