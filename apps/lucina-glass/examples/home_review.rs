//! The review set for Juno's glass home, rendered on the host from the real [`Home`] and the fixture data: the sixteen animations of
//! the concept as they run in the shell, a sheet of every strip, the settled dashboard and the icon sheet.
//!
//! `cargo run --release -p lucina-glass --features fixture --example home_review -- <out-dir> [--apps <apps-page.ppm>] [--only <demo>]`
//!
//! Under `<out-dir>`:
//!
//! - `anim/<name>/fNNN.png`: every frame at 30 fps, cropped to the component (the two screen moves and sheet-rise: the whole screen
//!   at half size);
//! - `anim/<name>-strip.png`: six evenly spaced frames side by side on carbon;
//! - `components.png`: every strip with its name and a line about it, one under another, 2400 px wide;
//! - `variants/juno-render.png`: the settled dashboard at full size;
//! - `icons.png`: the icon sheet.
//!
//! `review-mp4.sh` (scratch) turns each frame folder into `mp4/<name>.mp4`.
use std::path::Path;

use lucina_glass::home::{self, Home, HomeData, Key, Note, Pointer, fixture};
use lucina_glass::layouts::pane;
use lucina_glass::tokens::color;
use lucina_glass::widgets::{cell_x, chrome::VSlider};
use lucina_glass::{Canvas, Color, Font, Paint, R, Type, imagery};

/// When demos start: long enough after the home is made that every entrance has settled.
const SETTLED: f64 = 3.0;
const FPS: f64 = 30.0;
const PX: (u32, u32) = (1920, 1080);

type Step = Box<dyn Fn(&mut Home, f64)>;

struct Demo {
    name: &'static str,
    what: &'static str,
    dur: f64,
    /// The data the home starts from.
    data: fn() -> HomeData,
    /// Keys pressed before the demo starts (the keyboard focus, without a change on screen).
    pre: bool,
    steps: Vec<(f64, Step)>,
    /// The part to look at, dp.
    crop: R,
    /// Whole screen, half size.
    half: bool,
    /// A level slider over the right edge.
    slider: bool,
}

/// Where things are, from the layout's fixed positions (`layout` in home/mod.rs).
struct Where {
    build: R,
    seg: R,
    actions: R,
    days: R,
    grid: R,
    title: R,
    stack: R,
    row: R,
    stats_right: R,
    inner: R,
    dock: R,
    timer: R,
    media: R,
}

fn wh() -> Where {
    let area = R::new(20.0, 48.0, home::W - 40.0, 576.0);
    let left = pane(area, 12, 1, 16.0, 0, 0, 4, 1);
    let mid = pane(area, 12, 1, 16.0, 4, 0, 4, 1);
    let right = pane(area, 12, 1, 16.0, 8, 0, 4, 1);
    let build = R::new(left.x, left.y, left.w, 166.0);
    let seg = R::new(left.x, build.bottom() + 12.0, left.w, 44.0);
    let actions = R::new(left.x, seg.bottom() + 32.0, left.w, 64.0);
    let days = R::new(mid.x, mid.y + 204.0, mid.w, 36.0);
    let grid = R::new(mid.x, days.bottom() + 12.0, mid.w, area.bottom() - days.bottom() - 12.0);
    let inner = right.inset(16.0);
    let stats_right = R::new(inner.x, inner.y, inner.w, 200.0);
    let title = R::new(inner.x, inner.y + 220.0, inner.w, 48.0);
    let stack = R::new(inner.x, title.bottom() + 20.0, inner.w, 96.0);
    let row = R::new(inner.x, stack.bottom() + 56.0, inner.w, 64.0);
    let dock = R::new(20.0, 638.0, home::W - 40.0, 64.0);
    let timer = R::new(dock.x + 20.0 + 162.0, dock.y + 6.0, 170.0, 52.0);
    let media = R::new(dock.cx() - 190.0, dock.y + 8.0, 380.0, 48.0);
    Where { build, seg, actions, days, grid, title, stack, row, stats_right, inner, dock, timer, media }
}

fn union(a: R, b: R) -> R {
    let (x, y) = (a.x.min(b.x), a.y.min(b.y));
    R::new(x, y, a.right().max(b.right()) - x, a.bottom().max(b.bottom()) - y)
}

fn pad(r: R, d: f32) -> R {
    R::new(r.x - d, r.y - d, r.w + 2.0 * d, r.h + 2.0 * d)
}

fn notes(n: usize) -> Vec<Note> {
    (0..n)
        .map(|i| Note { id: format!("n{i}"), fact: format!("Fixture notice {}", i + 1), line: format!("{} min ago", 2 + i * 7), urgent: i == 0, app: None, at: None })
        .collect()
}

fn plain() -> HomeData {
    fixture::full(0.0)
}

fn five_notes() -> HomeData {
    HomeData { notes: Some(notes(5)), ..fixture::full(0.0) }
}

fn four_notes() -> HomeData {
    HomeData { notes: Some(notes(4)), ..fixture::full(0.0) }
}

/// A countdown whose ring visibly sweeps within the demo: 20 s window, 14 s left when the demo starts.
fn short_timer() -> HomeData {
    let mut d = fixture::full(0.0);
    d.countdown = Some(home::Countdown { at: SETTLED + 14.3, window: 20 });
    d
}

fn set(f: impl Fn(&mut HomeData) + 'static) -> Step {
    Box::new(move |h, t| {
        let mut d = h.data.clone();
        f(&mut d);
        h.set_data(t, d);
    })
}

fn key(k: Key) -> Step {
    Box::new(move |h, t| {
        h.key(t, k);
    })
}

fn ptr(p: Pointer) -> Step {
    Box::new(move |h, t| {
        h.pointer(t, p);
    })
}

fn enter() -> Step {
    Box::new(|h, t| h.enter(t))
}

fn click(x: f32, y: f32, at: f64, hold: f64) -> Vec<(f64, Step)> {
    vec![(at, ptr(Pointer::Move(x, y))), (at, ptr(Pointer::Down(x, y))), (at + hold, ptr(Pointer::Up(x, y)))]
}

fn demos(w: &Where) -> Vec<Demo> {
    let build_card = pad(w.build, 14.0);
    let plan = {
        let (x, cw) = cell_x(w.actions, 4, 12.0, 1, 1);
        (x + cw / 2.0, w.actions.y + 22.0)
    };
    let chip = |i: u32| {
        let (x, cw) = cell_x(w.days, 5, 6.0, i, 1);
        (x + cw / 2.0, w.days.cy())
    };
    let lucina = {
        let (x, cw) = cell_x(w.seg.inset(4.0), 2, 0.0, 1, 1);
        (x + cw / 2.0, w.seg.cy())
    };
    let play = (w.media.right() - 26.0, w.media.cy());
    let slider_r = R::new(home::W - 56.0 - 12.0, (home::H - 270.0) / 2.0, 56.0, 270.0);

    let mut out: Vec<Demo> = Vec::new();
    let mut add = |name, what, dur, data: fn() -> HomeData, pre, steps: Vec<(f64, Step)>, crop: R| {
        out.push(Demo { name, what, dur, data, pre, steps, crop, half: false, slider: false });
    };

    // 1. action-button
    let mut s = vec![(0.1, ptr(Pointer::Move(plan.0, plan.1))), (0.55, ptr(Pointer::Down(plan.0, plan.1))), (0.75, ptr(Pointer::Up(plan.0, plan.1)))];
    s.push((1.1, ptr(Pointer::Move(plan.0, plan.1 - 200.0))));
    add("action-button", "Action row: hover lifts and rims Plan, press shrinks it into the accent", 1.4, plain, false, s, pad(w.actions, 12.0));
    // 2. calendar-stagger
    add("calendar-stagger", "Agenda tiles rise in, staggered; dots pop; the booked share counts up", 1.9, plain, false, vec![(0.0, enter())], pad(w.grid, 10.0));
    // 3. card-stack
    add(
        "card-stack",
        "Notification stack: the top card is thrown, the rest move up",
        1.4,
        four_notes,
        true,
        vec![(0.1, key(Key::Delete))],
        R::new(w.inner.x - 16.0, w.stack.y - 12.0, w.inner.w + 32.0, w.stack.h + 44.0),
    );
    // 4. day-chips
    let mut s = Vec::new();
    s.extend(click(chip(3).0, chip(3).1, 0.1, 0.05));
    s.extend(click(chip(0).0, chip(0).1, 0.7, 0.05));
    add("day-chips", "Day chips: the pill stretches to Thu and back to Mon; the summary tile counts to the day's share", 1.4, plain, false, s, pad(union(w.days, w.grid), 6.0));
    // 5. live-pill
    add("live-pill", "Live pill: halo swells and fades, the dot breathes", 1.8, plain, false, vec![], R::new(w.build.right() - 16.0 - 100.0, w.build.y + 6.0, 120.0, 44.0));
    // 6. media-press
    let mut s = vec![(0.1, ptr(Pointer::Move(play.0, play.1))), (0.2, ptr(Pointer::Down(play.0, play.1))), (0.4, ptr(Pointer::Up(play.0, play.1)))];
    s.push((0.4, set(|d| {
        if let Some(m) = &mut d.media {
            m.playing = !m.playing;
        }
    })));
    s.push((1.0, set(|d| {
        if let Some(m) = &mut d.media {
            m.playing = !m.playing;
        }
    })));
    add("media-press", "Media pill: the button shrinks and lights, play and pause cross-fade", 1.6, plain, false, s, pad(w.media, 12.0));
    // 7. notification-enter
    add(
        "notification-enter",
        "The title rises in, then a new notification arrives at the front of the stack",
        1.9,
        plain,
        false,
        vec![
            (0.0, enter()),
            (0.8, set(|d| {
                let mut n = notes(1);
                n[0].id = "new".into();
                n[0].fact = "Fixture arrival".into();
                n[0].line = "Just now".into();
                if let Some(v) = &mut d.notes {
                    v.insert(0, n.remove(0));
                }
            })),
        ],
        R::new(w.inner.x - 16.0, w.title.y - 6.0, w.inner.w + 32.0, w.stack.bottom() + 36.0 - w.title.y),
    );
    // 8. pager-dots
    add("pager-dots", "Pager: the width passes from dot to dot as the stack is paged", 1.1, five_notes, true, vec![(0.1, key(Key::Right)), (0.5, key(Key::Right))], R::new(w.row.x - 10.0, w.row.y - 2.0, 180.0, 52.0));
    // 9. phase-advance
    add(
        "phase-advance",
        "Next phase: the finished phase completes, the next dot grows into a pill",
        1.5,
        plain,
        false,
        vec![(0.1, set(|d| {
            if let Some(b) = &mut d.build {
                b.phase = 3;
                b.line = "Phase 3 of 3".into();
            }
        }))],
        build_card,
    );
    // 10. progress-count-up
    add(
        "progress-count-up",
        "The percentage counts up from 64 to 88 while the track fills",
        1.6,
        plain,
        false,
        vec![(0.1, set(|d| {
            if let Some(b) = &mut d.build {
                b.percent = 88;
            }
        }))],
        build_card,
    );
    // 13. segmented
    let mut s = click(lucina.0, lucina.1, 0.1, 0.05);
    s.push((0.9, ptr(Pointer::Move(lucina.0, lucina.1 - 300.0))));
    add("segmented", "Segmented control: the thumb slides on a spring, the row of actions changes", 1.4, plain, false, s, pad(union(w.seg, w.actions), 12.0));
    // 15. timer-ring
    add("timer-ring", "Timer: the digits roll each second, the ring sweeps", 2.4, short_timer, false, vec![], pad(w.timer, 8.0));

    let whole = R::new(0.0, 0.0, home::W, home::H);
    out.push(Demo { name: "sheet-rise", what: "The dock rises, the stat cards fade up (whole screen, half size)", dur: 1.4, data: plain, pre: false, steps: vec![(0.0, enter())], crop: whole, half: true, slider: false });
    out.push(Demo {
        name: "vertical-slider",
        what: "Level panel: the thumb steps on a spring",
        dur: 1.0,
        data: plain,
        pre: false,
        steps: vec![],
        crop: pad(slider_r, 16.0),
        half: false,
        slider: true,
    });
    out.sort_by_key(|d| d.name);
    let _ = &w.stats_right;
    let _ = &w.dock;
    out
}

fn crop(cv: &Canvas, r: R) -> Canvas {
    let x0 = ((r.x * cv.k).round().max(0.0) as u32 & !1).min(cv.w);
    let y0 = ((r.y * cv.k).round().max(0.0) as u32 & !1).min(cv.h);
    let w = (((r.w * cv.k).round() as u32).min(cv.w - x0)) & !1;
    let h = (((r.h * cv.k).round() as u32).min(cv.h - y0)) & !1;
    let mut out = Canvas::new(w, h, cv.k);
    for y in 0..h {
        let src = ((y0 + y) * cv.w + x0) as usize;
        out.px[(y * w) as usize..((y + 1) * w) as usize].copy_from_slice(&cv.px[src..src + w as usize]);
    }
    out
}

fn save(cv: &Canvas, path: &Path) -> std::io::Result<()> {
    if let Some(d) = path.parent() {
        std::fs::create_dir_all(d)?;
    }
    std::fs::write(path, cv.png())
}

/// Six frames side by side, 12 px apart on carbon.
fn strip(frames: &[Canvas]) -> Canvas {
    let picks: Vec<&Canvas> = (0..6).map(|i| &frames[(i * (frames.len() - 1)) / 5]).collect();
    let gap = 12;
    let (fw, fh) = (picks[0].w, picks[0].h);
    let mut out = Canvas::new(fw * 6 + gap * 7, fh + gap * 2, 1.0);
    out.clear(color::CARBON);
    for (i, f) in picks.iter().enumerate() {
        let x = gap + i as u32 * (fw + gap);
        for y in 0..fh {
            let dst = ((gap + y) * out.w + x) as usize;
            out.px[dst..dst + fw as usize].copy_from_slice(&f.px[(y * fw) as usize..((y + 1) * fw) as usize]);
        }
    }
    out
}

/// Strips stacked with a title over each, scaled to one width.
fn sheet(strips: &[(&str, &str, Canvas)], ty: &mut Type) -> Canvas {
    let width = 2400u32;
    let label = 70u32;
    let scaled: Vec<Canvas> = strips
        .iter()
        .map(|(_, _, s)| {
            let h = (s.h as f32 * width as f32 / s.w as f32).round() as u32;
            s.resized(width, h.max(1))
        })
        .collect();
    let total: u32 = scaled.iter().map(|s| s.h + label).sum::<u32>() + 40;
    let mut out = Canvas::new(width, total, 2.0);
    out.clear(Color::hex(0x08090b));
    let mut y = 20u32;
    for ((name, what, _), s) in strips.iter().zip(&scaled) {
        ty.draw(&mut out, Font::Mono, 13.0, 1.0, 12.0, (y + 26) as f32 / 2.0, color::LIVE, &name.to_uppercase());
        ty.draw(&mut out, Font::Regular, 13.0, 0.0, 12.0, (y + 56) as f32 / 2.0, color::INK_2, what);
        y += label;
        for row in 0..s.h {
            let dst = ((y + row) * width) as usize;
            out.px[dst..dst + width as usize].copy_from_slice(&s.px[(row * width) as usize..((row + 1) * width) as usize]);
        }
        y += s.h;
    }
    out
}

fn read_ppm(path: &str) -> Vec<u32> {
    let b = std::fs::read(path).unwrap_or_else(|e| panic!("{path}: {e}"));
    let mut pos = 0;
    let mut tok = || {
        while b[pos].is_ascii_whitespace() {
            pos += 1;
        }
        let s = pos;
        while !b[pos].is_ascii_whitespace() {
            pos += 1;
        }
        String::from_utf8_lossy(&b[s..pos]).to_string()
    };
    assert_eq!(tok(), "P6", "{path}: not a P6 file");
    let (w, h): (usize, usize) = (tok().parse().unwrap(), tok().parse().unwrap());
    assert_eq!(tok(), "255");
    pos += 1;
    assert_eq!((w, h), (PX.0 as usize, PX.1 as usize), "{path}: expected 1920x1080");
    let d = &b[pos..pos + w * h * 3];
    (0..w * h).map(|i| (d[i * 3] as u32) << 16 | (d[i * 3 + 1] as u32) << 8 | d[i * 3 + 2] as u32).collect()
}

/// The keys that put the keyboard focus on the notification stack: found by trying sequences until Delete clears a note there.
fn focus_stack_keys() -> Vec<Key> {
    let arrows = [Key::Left, Key::Right, Key::Up, Key::Down];
    let mut level: Vec<Vec<Key>> = vec![Vec::new()];
    for _ in 0..10 {
        let mut next = Vec::new();
        for seq in &level {
            for a in arrows {
                let mut s = seq.clone();
                s.push(a);
                let mut h = Home::new(0.0);
                h.set_data(0.0, five_notes());
                for k in &s {
                    h.key(1.0, *k);
                }
                if matches!(h.key(1.0, Key::Delete), home::Outcome::Dismissed(_)) {
                    return s;
                }
                next.push(s);
            }
        }
        // Many sequences end on the same item: keep a spread, not all of them.
        let cap = 6000;
        if next.len() > cap {
            let step = next.len() / cap;
            next = next.into_iter().step_by(step).collect();
        }
        level = next;
    }
    panic!("no key sequence reaches the notification stack");
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut dir = None;
    let (mut apps, mut only) = (None, None);
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--apps" => apps = it.next().cloned(),
            "--only" => only = it.next().cloned(),
            _ => dir = Some(a.clone()),
        }
    }
    let dir = std::path::PathBuf::from(dir.expect("usage: home_review <out-dir> [--apps <apps-page.ppm>] [--only <demo>]"));
    std::fs::create_dir_all(&dir).unwrap();
    let t0 = std::time::Instant::now();
    let (pw, ph) = PX;
    let bg = imagery::night_sky(pw, ph, home::K, 7);
    let blurred = bg.blurred((lucina_glass::tokens::glass::BLUR * home::K / 3.0).round().max(1.0) as u32);
    let mut ty = Type::new();
    let w = wh();

    // The settled dashboard, and the icons.
    let draw_settled = |ty: &mut Type| {
        let mut cv = Canvas::new(pw, ph, home::K);
        let mut h = Home::new(0.0);
        h.set_data(0.0, plain());
        h.draw(&mut cv, ty, SETTLED, &bg, &blurred);
        cv
    };
    let dash = draw_settled(&mut ty);
    save(&dash, &dir.join("variants/juno-render.png")).unwrap();
    {
        let all = lucina_glass::icons::ALL;
        let rows = all.len().div_ceil(9) as u32;
        let mut cv = Canvas::new(9 * 96 * 3, rows * 96 * 3, 3.0);
        cv.clear(color::CARBON);
        for (i, icon) in all.iter().enumerate() {
            let (x, y) = ((i % 9) as f32 * 96.0 + 48.0, (i / 9) as f32 * 96.0 + 40.0);
            lucina_glass::icons::draw(&mut cv, *icon, x, y, 40.0, color::INK);
            ty.draw_centered(&mut cv, Font::Mono, 8.0, 0.0, x, y + 42.0, color::INK_3, &format!("{icon:?}"));
        }
        save(&cv, &dir.join("icons.png")).unwrap();
    }

    let focus = if only.as_deref().is_none_or(|o| o == "card-stack" || o == "pager-dots") { focus_stack_keys() } else { Vec::new() };
    eprintln!("focus keys to the stack: {focus:?}");

    let mut strips: Vec<(&str, &str, Canvas)> = Vec::new();
    let mut emit = |name: &'static str, what: &'static str, frames: Vec<Canvas>, dir: &Path| {
        for (i, f) in frames.iter().enumerate() {
            save(f, &dir.join(format!("anim/{name}/f{i:03}.png"))).unwrap();
        }
        let s = strip(&frames);
        save(&s, &dir.join(format!("anim/{name}-strip.png"))).unwrap();
        strips.push((name, what, s));
    };

    let mut list = demos(&w);
    // The two screen moves come from the apps page, when there is one.
    let mut moves: Vec<(&'static str, &'static str, i32)> = Vec::new();
    if apps.is_some() {
        moves.push(("screen-back", "The apps page slides away, the home comes back (whole screen, half size)", -1));
        moves.push(("screen-forward", "The home to the apps page: the new page slides over, the old sinks back (whole screen, half size)", 1));
    } else {
        eprintln!("no --apps: screen-forward and screen-back skipped");
    }
    let mut all_names: Vec<&'static str> = list.iter().map(|d| d.name).chain(moves.iter().map(|m| m.0)).collect();
    all_names.sort();

    let mut done: std::collections::HashMap<&str, (&'static str, Vec<Canvas>)> = Default::default();
    for d in list.drain(..) {
        if only.as_deref().is_some_and(|o| o != d.name) {
            continue;
        }
        let t = std::time::Instant::now();
        let mut h = Home::new(0.0);
        h.set_data(0.0, (d.data)());
        if d.pre {
            for k in &focus {
                h.key(2.0, *k);
            }
        }
        let mut cv = Canvas::new(pw, ph, home::K);
        let mut slider = VSlider::new(8, 2);
        let slider_r = R::new(home::W - 56.0 - 12.0, (home::H - 270.0) / 2.0, 56.0, 270.0);
        let n = (d.dur * FPS).ceil() as usize + 1;
        let mut next = 0;
        let mut frames = Vec::with_capacity(n);
        let mut steps = d.steps;
        steps.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap());
        let mut slid = [false; 2];
        for i in 0..n {
            let tt = i as f64 / FPS;
            while next < steps.len() && steps[next].0 <= tt {
                (steps[next].1)(&mut h, SETTLED + steps[next].0);
                next += 1;
            }
            if d.slider {
                for (j, at) in [0.1, 0.5].into_iter().enumerate() {
                    if !slid[j] && at <= tt {
                        slid[j] = true;
                        slider.step(SETTLED + at, 1);
                    }
                }
            }
            let now = SETTLED + tt;
            cv.push_clip(d.crop, 0.0);
            h.draw(&mut cv, &mut ty, now, &bg, &blurred);
            if d.slider {
                let mut p = Paint { cv: &mut cv, ty: &mut ty, now, backdrop: Some(&blurred) };
                slider.draw(&mut p, slider_r);
            }
            cv.pop_clip();
            let c = crop(&cv, d.crop);
            frames.push(if d.half { c.resized(c.w / 2, c.h / 2) } else { c });
        }
        eprintln!("demo {}: {} frames, {} ms", d.name, frames.len(), t.elapsed().as_millis());
        done.insert(d.name, (d.what, frames));
    }

    for (name, what, dirn) in &moves {
        if only.as_deref().is_some_and(|o| o != *name) {
            continue;
        }
        let page = read_ppm(apps.as_deref().unwrap());
        let t = std::time::Instant::now();
        let (front, back) = if *dirn > 0 { (page.clone(), dash.px.clone()) } else { (dash.px.clone(), page) };
        // forward: the apps page arrives over the home; back: the home arrives over the apps page.
        let dur = 0.62;
        let n = (dur * FPS).ceil() as usize + 1;
        let mut out = vec![0u32; (pw * ph) as usize];
        let mut frames = Vec::new();
        for i in 0..n {
            let tt = (i as f32 / (n - 1) as f32).clamp(0.0, 1.0);
            home::slide_compose(&mut out, &back, &front, pw as usize, ph as usize, tt, *dirn);
            let mut c = Canvas::new(pw, ph, home::K);
            c.px.copy_from_slice(&out);
            frames.push(c.resized(pw / 2, ph / 2));
        }
        eprintln!("demo {name}: {} frames, {} ms", frames.len(), t.elapsed().as_millis());
        done.insert(name, (what, frames));
    }

    for name in all_names {
        if let Some((what, frames)) = done.remove(name) {
            emit(name, what, frames, &dir);
        }
    }
    if only.is_none() {
        let s = sheet(&strips, &mut ty);
        save(&s, &dir.join("components.png")).unwrap();
    }
    eprintln!("done in {:.1} s", t0.elapsed().as_secs_f32());
}
