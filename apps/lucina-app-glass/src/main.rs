//! lucina-app-glass: the gallery of the glass phone concept (docs/design/phone-concept.md).
//!
//! On Juno it shows the phone at the screen's full height with the list of screens and demos beside it: click a demo to play
//! it, use the phone with the pointer, Left/Right to move between screens, Space to pause every animation, S for quarter
//! speed, R to replay the screen's entrance, V to cycle through the other devices (Juno, Diana as a tablet both ways, Fortuna).
//!
//! `lucina-app-glass --render <dir> [demo]` renders the review set with no screen (see `render.rs`).

// Juno with no C library: Eyra is the libc and the program start, in Rust (docs/research/juno-pure-rust.md).
#[cfg(all(target_os = "linux", target_env = "gnu", target_arch = "aarch64"))]
extern crate eyra;

mod render;
mod stage;

use std::time::{Duration, Instant};

use lucina_app::gfx::{Face, Rect, Rgb};
use lucina_app::keys;
use lucina_app::ui::Input;
use lucina_app::{App, Ui};
use lucina_glass::screens::Pointer;
use lucina_glass::layouts::{DianaTablet, Fortuna, Juno, Layout};
use lucina_glass::{Canvas, Clock, R};

use stage::{DEMOS, Stage};

struct Gallery {
    stage: Stage,
    clock: Clock,
    last: Instant,
    phone: Option<Canvas>,
    /// Where the phone is on the screen, and its pixels per dp.
    at: (Rect, f32),
    /// A demo playing: its index, when it started, its next step.
    playing: Option<(usize, f64, usize)>,
    /// 0: the phone; otherwise the device layout `variants[view - 1]`.
    view: usize,
    variants: Vec<Box<dyn Layout>>,
    shown: Option<Canvas>,
}

impl Gallery {
    fn new() -> Gallery {
        Gallery {
            stage: Stage::new(0.0),
            clock: Clock::manual(),
            last: Instant::now(),
            phone: None,
            at: (Rect::default(), 1.0),
            playing: None,
            view: 0,
            variants: vec![Box::new(Juno::new(0.0)), Box::new(DianaTablet::new(0.0, false)), Box::new(DianaTablet::new(0.0, true)), Box::new(Fortuna::new(0.0))],
            shown: None,
        }
    }

    fn play(&mut self, i: usize) {
        self.view = 0;
        let now = self.clock.now();
        let d = &DEMOS[i];
        if d.screen != self.stage.current {
            self.stage.current = d.screen;
            self.stage.moving = None;
        }
        self.playing = Some((i, now, 0));
    }

    /// Run the steps of the playing demo that are due.
    fn steps(&mut self) {
        let now = self.clock.now();
        if let Some((i, start, mut next)) = self.playing {
            let d = &DEMOS[i];
            while next < d.steps.len() && start + d.steps[next].0 <= now {
                let (at, a) = d.steps[next];
                self.stage.act(start + at, a);
                next += 1;
            }
            self.playing = if next < d.steps.len() || now < start + d.dur { Some((i, start, next)) } else { None };
        }
    }

    fn to_dp(&self, x: i32, y: i32) -> Option<(f32, f32)> {
        let (r, k) = self.at;
        r.contains(x, y).then(|| ((x - r.x) as f32 / k, (y - r.y) as f32 / k))
    }
}

impl App for Gallery {
    type Msg = ();

    fn view(&mut self, ui: &mut Ui) {
        let t = Instant::now();
        self.clock.advance(t.duration_since(self.last).as_secs_f64());
        self.last = t;
        self.steps();
        let now = self.clock.now();
        let th = ui.theme();

        // Keys.
        if let Some(p) = ui.press() {
            match p.code {
                keys::KEY_RIGHT => self.stage.go(now, (self.stage.current + 1).min(2)),
                keys::KEY_LEFT => self.stage.go(now, self.stage.current.saturating_sub(1)),
                keys::KEY_SPACE => {
                    if self.clock.is_paused() { self.clock.resume() } else { self.clock.pause() }
                }
                keys::KEY_S => self.clock.rate = if self.clock.rate < 1.0 { 1.0 } else { 0.25 },
                keys::KEY_V => self.view = (self.view + 1) % (self.variants.len() + 1),
                keys::KEY_R => {
                    self.stage.act(now, "enter");
                }
                _ => {}
            }
            ui.take_press();
        }

        // The list on the left.
        let area = ui.area();
        let list_w = th.px(300.0);
        let list = Rect::new(area.x, area.y, list_w, area.h);
        let pad = th.px(12.0);
        let row = th.px(30.0);
        let mut y = list.y + pad;
        if ui.painting() {
            ui.fill(list, th.panel);
            ui.text_at(list.x + pad, y + th.px(20.0), Face::Bold, th.pt(16.0), th.text, "Glass concept");
        }
        y += th.px(34.0);
        let names = self.stage.names();
        for (i, n) in names.iter().enumerate() {
            if ui.button_at(Rect::new(list.x + pad, y, list_w - 2 * pad, row - th.px(4.0)), ("screen", i), n) {
                self.view = 0;
                self.stage.go(now, i);
                self.playing = None;
            }
            y += row;
        }
        for i in 0..self.variants.len() {
            let n = self.variants[i].name();
            if ui.button_at(Rect::new(list.x + pad, y, list_w - 2 * pad, row - th.px(4.0)), ("variant", i), n) {
                self.view = i + 1;
            }
            y += row;
        }
        y += th.px(10.0);
        for (i, d) in DEMOS.iter().enumerate() {
            if y + row > list.bottom() - th.px(40.0) {
                break;
            }
            if ui.button_at(Rect::new(list.x + pad, y, list_w - 2 * pad, row - th.px(4.0)), ("demo", i), d.name) {
                self.play(i);
            }
            y += row;
        }
        if ui.painting() {
            let state = format!("{}{}", if self.clock.is_paused() { "paused" } else { "running" }, if self.clock.rate < 1.0 { ", 1/4 speed" } else { "" });
            ui.text_at(list.x + pad, list.bottom() - pad, Face::Sans, th.pt(12.0), th.dim, &state);
        }

        // The phone, at full height.
        let h = area.h - 2 * pad;
        let k = h as f32 / 800.0;
        let w = (360.0 * k).round() as i32;
        let x = list.right() + (area.right() - list.right() - w) / 2;
        let r = Rect::new(x, area.y + pad, w, h);
        self.at = (r, k);

        // The pointer, in dp.
        match ui.input() {
            Some(Input::Down(px, py)) => {
                if let Some((dx, dy)) = self.to_dp(px, py) {
                    self.stage.pointer(now, Pointer::Down(dx, dy));
                    ui.take_input();
                }
            }
            Some(Input::Up(px, py)) => {
                let (dx, dy) = self.to_dp(px, py).unwrap_or(((px - r.x) as f32 / k, (py - r.y) as f32 / k));
                self.stage.pointer(now, Pointer::Up(dx, dy));
            }
            Some(Input::Move(px, py)) => {
                if let Some((dx, dy)) = self.to_dp(px, py) {
                    self.stage.pointer(now, Pointer::Move(dx, dy));
                    ui.repaint();
                }
            }
            _ => {}
        }

        if ui.painting() && self.view > 0 {
            // Another device, fitted into the space right of the list (whole multiples for the DSi's pixels).
            ui.fill(Rect::new(list.right(), area.y, area.w - list_w, area.h), Rgb::hex(0x050608));
            let v = &mut self.variants[self.view - 1];
            let (vw, vh, vk) = v.size();
            let room = ((area.w - list_w - 2 * pad) as f32 / vw as f32).min((area.h - 2 * pad) as f32 / vh as f32);
            let s = if v.name() == "fortuna" { room.floor().max(1.0) } else { room.min(1.0) };
            let (w, h) = ((vw as f32 * s) as u32, (vh as f32 * s) as u32);
            if v.name() == "fortuna" {
                let mut small = Canvas::new(vw, vh, vk);
                v.draw(&mut small, &mut self.stage.ty, now);
                let mut big = Canvas::new(w, h, 1.0);
                let n = s as u32;
                for y in 0..h {
                    for x in 0..w {
                        big.px[(y * w + x) as usize] = small.px[((y / n) * vw + x / n) as usize];
                    }
                }
                self.shown = Some(big);
            } else {
                if self.shown.as_ref().is_none_or(|c| c.w != w || c.h != h) {
                    self.shown = Some(Canvas::new(w, h, vk * s));
                }
                v.draw(self.shown.as_mut().expect("shown"), &mut self.stage.ty, now);
            }
            let cv = self.shown.as_ref().expect("shown");
            let x = list.right() + (area.right() - list.right() - w as i32) / 2;
            let y = area.y + (area.h - h as i32) / 2;
            ui.pixels(Rect::new(x, y, w as i32, h as i32), w as usize, &cv.px);
        } else if ui.painting() {
            ui.fill(Rect::new(list.right(), area.y, area.w - list_w, area.h), Rgb::hex(0x050608));
            if self.phone.as_ref().is_none_or(|p| p.w != w as u32 || p.h != h as u32) {
                self.phone = Some(Canvas::new(w as u32, h as u32, k));
            }
            let cv = self.phone.as_mut().expect("phone");
            self.stage.draw(cv, now);
            // Round the phone's corners against the ground.
            let mut frame = Canvas::new(w as u32, h as u32, k);
            frame.clear(lucina_glass::Color::hex(0x050608));
            frame.blit(cv, R::new(0.0, 0.0, 360.0, 800.0), 44.0, 1.0);
            ui.pixels(r, w as usize, &frame.px);
        }
        if !self.clock.is_paused() {
            ui.wake_after(Duration::from_millis(16));
        }
    }
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.get(1).map(String::as_str) == Some("--render") {
        let dir = args.get(2).map(String::as_str).unwrap_or("out/glass");
        if let Err(e) = render::run(std::path::Path::new(dir), args.get(3).map(String::as_str)) {
            eprintln!("lucina-app-glass: {e}");
            std::process::exit(1);
        }
        return;
    }
    lucina_app::run("lucina-app-glass", |_| Gallery::new());
}

#[cfg(test)]
mod tests {
    use super::*;
    use lucina_app::Harness;

    #[test]
    fn gallery_draws_and_takes_input() {
        let mut h = Harness::new(Gallery::new(), 1280, 720);
        let (r, _) = h.app().at;
        assert!(r.w > 200 && r.h > 600, "{r:?}");
        // A click on the phone reaches the screen; Right moves to the next screen.
        h.click(r.x + r.w / 2, r.y + r.h / 3);
        h.key(keys::KEY_RIGHT);
        assert_eq!(h.app().stage.current, 1);
        // V shows the other devices in turn.
        for v in 1..=4 {
            h.key(keys::KEY_V);
            assert_eq!(h.app().view, v);
        }
        h.key(keys::KEY_V);
        assert_eq!(h.app().view, 0);
        // The phone was drawn: it is not all one colour.
        let ppm = h.ppm();
        let px = &ppm[ppm.len() / 2..ppm.len() / 2 + 3000];
        assert!(px.iter().any(|&b| b != px[0]));
    }

    #[test]
    fn every_demo_runs() {
        for d in DEMOS {
            let f = stage::frames(d, 1.0, 10.0);
            assert!(f.len() >= 2, "{}", d.name);
            assert!(f.iter().any(|c| c.px != f[0].px) || d.steps.is_empty() && d.name == "live-pill", "{} never moves", d.name);
        }
    }
}
