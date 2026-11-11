//! The session on the screen: the app holds the whole panel, shows what the reader thread converted, draws the pointer and
//! sends input. F1 stays the shell's: it ends the app. Ctrl+Alt+Shift+Q goes back to the form.

use std::collections::BTreeSet;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use lucina_app::app::FullScreen;
use lucina_app::input::{self, EV_ABS, EV_KEY, EV_REL, EV_SYN};
use lucina_app::keys;

use crate::keymap::{self, Host};
use crate::link::Live;
use crate::video::{Fit, Scaler};

const MASK_MOVE: i32 = 0;
const MASK_DOWN: i32 = 1;
const MASK_UP: i32 = 2;
const MASK_WHEEL: i32 = 3;
const BUTTON_LEFT: i32 = 1;
const BUTTON_RIGHT: i32 = 2;
const BUTTON_MIDDLE: i32 = 4;

const BTN_MIDDLE: u16 = 0x112;
/// The first of evdev's button codes: everything from here up is not a keyboard key.
const FIRST_BUTTON: u16 = 0x100;
const KEY_Q: u16 = 16;

/// A plain arrow for when the host has sent no cursor: 'X' black, '.' white.
const ARROW: [&[u8; 11]; 16] = [
    b"X          ",
    b"XX         ",
    b"X.X        ",
    b"X..X       ",
    b"X...X      ",
    b"X....X     ",
    b"X.....X    ",
    b"X......X   ",
    b"X.......X  ",
    b"X........X ",
    b"X.....XXXXX",
    b"X..X..X    ",
    b"X.X X..X   ",
    b"XX  X..X   ",
    b"X    X..X  ",
    b"     XXXX  ",
];

/// A cursor ready to draw: straight ARGB with its hot spot.
struct Pic {
    hotx: i32,
    hoty: i32,
    w: i32,
    h: i32,
    argb: Vec<u32>,
}

fn arrow() -> Pic {
    let (w, h) = (11, ARROW.len() as i32);
    let mut argb = Vec::with_capacity((w * h) as usize);
    for row in ARROW {
        for b in row {
            argb.push(match b {
                b'X' => 0xff00_0000,
                b'.' => 0xffff_ffff,
                _ => 0,
            });
        }
    }
    Pic { hotx: 0, hoty: 0, w, h, argb }
}

fn from_rgba(hotx: i32, hoty: i32, w: i32, h: i32, rgba: &[u8]) -> Pic {
    let argb = rgba.chunks_exact(4).map(|p| (p[3] as u32) << 24 | (p[0] as u32) << 16 | (p[1] as u32) << 8 | p[2] as u32).collect();
    Pic { hotx, hoty, w, h, argb }
}

fn blend(under: u32, over: u32) -> u32 {
    let a = over >> 24;
    match a {
        0 => under,
        255 => over,
        _ => {
            let (ia, o) = (255 - a, over);
            let rb = (((o & 0xff00ff) * a + (under & 0xff00ff) * ia) / 255) & 0xff00ff;
            let g = (((o & 0xff00) * a + (under & 0xff00) * ia) / 255) & 0xff00;
            0xff00_0000 | rb | g
        }
    }
}

/// The pixels the panel should show at a point, without the pointer.
struct Panel {
    fit: Fit,
    scaler: Option<Scaler>,
}

impl Panel {
    fn pixel(&self, canvas: &[u32], x: i32, y: i32) -> u32 {
        let f = &self.fit;
        let (px, py) = (x - f.x, y - f.y);
        if px < 0 || py < 0 || px >= f.w as i32 || py >= f.h as i32 {
            return 0xff00_0000;
        }
        let (px, py) = (px as usize, py as usize);
        match &self.scaler {
            Some(s) => s.px[py * f.w + px],
            None => canvas.get(py * f.src_w + px).copied().unwrap_or(0xff00_0000),
        }
    }
}

/// Runs until the session ends or the person leaves; the reason is what the form then says.
pub fn run(live: &Arc<Live>, fs: &mut FullScreen) -> String {
    let (panel_w, panel_h) = (fs.screen.fb.width as usize, fs.screen.fb.height as usize);
    let host = Host::from_platform(&live.platform);

    let mut pipe = [-1; 2];
    // SAFETY: pipe2 fills the two-element array.
    if unsafe { libc::pipe2(pipe.as_mut_ptr(), libc::O_NONBLOCK | libc::O_CLOEXEC) } < 0 {
        return "no pipe for the session".into();
    }
    let [wake_r, wake_w] = pipe;
    live.wake_fd.store(wake_w, Ordering::Relaxed);

    let black = vec![0xff00_0000u32; panel_w];
    for y in 0..panel_h {
        fs.screen.fb.present_image(0, y as i32, panel_w, &black);
    }

    let mut panel: Option<Panel> = None;
    let mut pointer = lucina_app::input::Pointer::new(panel_w as i32, panel_h as i32);
    let mut pointer_shown: Option<(i32, i32, i32, i32)> = None;
    let mut pic = arrow();
    let mut keys_down: BTreeSet<u16> = BTreeSet::new();
    let mut buttons = 0i32;
    let mut evs = Vec::new();
    let mut held = (false, false, false);

    let started = Instant::now();
    let mut stats = Stats::new(started);

    let why = loop {
        if fs.terminating() {
            break "you left".to_string();
        }
        if let Ok(e) = live.ended.lock() {
            if let Some(w) = e.as_ref() {
                break w.clone();
            }
        }
        let now = Instant::now();
        fs.inputs.rescan(now);
        evs.clear();
        if fs.inputs.read(&mut evs) {
            release_all(live, host, &mut keys_down);
        }
        let mut moved = false;
        let mut leave = false;
        let to_host = |p: &lucina_app::input::Pointer, panel: &Option<Panel>| match panel {
            Some(pa) => {
                let (x, y) = pa.fit.to_host(p.x, p.y);
                (x + live.display.x, y + live.display.y)
            }
            None => (live.display.x, live.display.y),
        };
        for (dev, ev) in evs.drain(..) {
            match ev.type_ {
                EV_SYN if ev.code == input::SYN_DROPPED => release_all(live, host, &mut keys_down),
                EV_KEY if ev.code == keys::BTN_LEFT || ev.code == keys::BTN_RIGHT || ev.code == BTN_MIDDLE || ev.code == keys::BTN_TOUCH => {
                    if ev.value == 2 {
                        continue;
                    }
                    let b = if ev.code == keys::BTN_RIGHT {
                        BUTTON_RIGHT
                    } else if ev.code == BTN_MIDDLE {
                        BUTTON_MIDDLE
                    } else {
                        BUTTON_LEFT
                    };
                    // The press lands where the pointer is now, not where the last move left it.
                    let (x, y) = to_host(&pointer, &panel);
                    if moved {
                        live.send(|s| s.mouse(MASK_MOVE | buttons << 3, x, y));
                        moved = false;
                    }
                    let kind = if ev.value != 0 { MASK_DOWN } else { MASK_UP };
                    if ev.value != 0 {
                        buttons |= b;
                    } else {
                        buttons &= !b;
                    }
                    live.send(|s| s.mouse(kind | b << 3, x, y));
                }
                EV_KEY if ev.code < FIRST_BUTTON => {
                    if ev.code == keys::KEY_F1 {
                        // The shell's key: it ends this app, and the host must not see half of it.
                        continue;
                    }
                    match ev.code {
                        keys::KEY_LEFTCTRL | keys::KEY_RIGHTCTRL => held.0 = ev.value != 0,
                        keys::KEY_LEFTALT | keys::KEY_RIGHTALT => held.1 = ev.value != 0,
                        keys::KEY_LEFTSHIFT | keys::KEY_RIGHTSHIFT => held.2 = ev.value != 0,
                        _ => {}
                    }
                    if ev.code == KEY_Q && ev.value == 1 && held == (true, true, true) {
                        leave = true;
                        continue;
                    }
                    // The host repeats a held key itself, as it does for its own keyboard.
                    if ev.value == 2 {
                        continue;
                    }
                    if ev.value != 0 {
                        keys_down.insert(ev.code);
                    } else {
                        keys_down.remove(&ev.code);
                    }
                    if let Some(code) = keymap::host_code(host, ev.code) {
                        live.send(|s| s.key_map(code, ev.value != 0));
                        if ev.value != 0 {
                            stats.key_sent();
                        }
                    }
                }
                EV_REL => match ev.code {
                    input::REL_X => {
                        pointer.moved(ev.value, 0);
                        moved = true;
                    }
                    input::REL_Y => {
                        pointer.moved(0, ev.value);
                        moved = true;
                    }
                    input::REL_WHEEL => {
                        let (x, y) = (0, ev.value);
                        live.send(|s| s.mouse(MASK_WHEEL, x, y));
                    }
                    _ => {}
                },
                EV_ABS => {
                    if let Some(d) = fs.inputs.devices.get(dev) {
                        let range = if ev.code == input::ABS_X { d.abs_x } else { d.abs_y };
                        pointer.absolute(ev.code, ev.value, range);
                        moved = true;
                    }
                }
                _ => {}
            }
        }
        if leave {
            break "you left the session".to_string();
        }
        if moved {
            let (x, y) = to_host(&pointer, &panel);
            let b = buttons;
            live.send(|s| s.mouse(MASK_MOVE | b << 3, x, y));
        }

        // What the reader made.
        let mut touched_pointer = false;
        let t0 = Instant::now();
        let mut shown_rows = 0u64;
        if let Ok(mut canvas) = live.canvas.lock() {
            if canvas.w > 0 && panel.as_ref().is_none_or(|p| (p.fit.src_w, p.fit.src_h) != (canvas.w, canvas.h)) {
                let fit = Fit::new(canvas.w, canvas.h, panel_w, panel_h);
                eprintln!("lucina-app-desk: the desktop is {}x{}, shown as {}x{} at {},{}", canvas.w, canvas.h, fit.w, fit.h, fit.x, fit.y);
                let scaler = (!fit.is_one_to_one()).then(|| Scaler::new(fit));
                panel = Some(Panel { fit, scaler });
                canvas.mark_all();
            }
            if let Some(pa) = panel.as_mut() {
                let f = pa.fit;
                for (first, count) in canvas.take_dirty() {
                    let (top, n) = match pa.scaler.as_mut() {
                        Some(s) => {
                            let (top, n) = s.rows_for(first, count);
                            s.scale(&canvas.px, top, n);
                            fs.screen.fb.present_image(f.x, f.y + top as i32, f.w, &s.px[top * f.w..(top + n) * f.w]);
                            (top, n)
                        }
                        None => {
                            fs.screen.fb.present_image(f.x, f.y + first as i32, f.w, &canvas.px[first * f.w..(first + count) * f.w]);
                            (first, count)
                        }
                    };
                    shown_rows += n as u64;
                    if let Some((_, py, _, ph)) = pointer_shown {
                        let (a, b) = (f.y + top as i32, f.y + (top + n) as i32);
                        touched_pointer |= py < b && py + ph > a;
                    }
                }
            }
        }
        let present = t0.elapsed();
        if shown_rows > 0 {
            stats.shown(shown_rows, present);
        }

        // The host's cursor picture.
        let mut cursor_changed = false;
        if let Ok(mut cur) = live.cursor.lock() {
            if cur.changed {
                cur.changed = false;
                if let Some(id) = cur.current {
                    if let Some(p) = cur.pics.iter().find(|p| p.id == id) {
                        pic = from_rgba(p.hotx, p.hoty, p.w, p.h, &p.rgba);
                        cursor_changed = true;
                    }
                }
            }
        }

        if pointer.visible && (moved || touched_pointer || cursor_changed) {
            if let (Some(pa), Ok(canvas)) = (panel.as_ref(), live.canvas.lock()) {
                // Back what was under it, then draw it where it is now.
                if let Some((x, y, w, h)) = pointer_shown.take() {
                    let mut under = Vec::with_capacity((w * h) as usize);
                    for yy in y..y + h {
                        for xx in x..x + w {
                            under.push(pa.pixel(&canvas.px, xx, yy));
                        }
                    }
                    fs.screen.fb.present_image(x, y, w as usize, &under);
                }
                let (x, y) = (pointer.x - pic.hotx, pointer.y - pic.hoty);
                let (cx, cy) = (x.max(0), y.max(0));
                let w = (x + pic.w).min(panel_w as i32) - cx;
                let h = (y + pic.h).min(panel_h as i32) - cy;
                if w > 0 && h > 0 {
                    let mut img = Vec::with_capacity((w * h) as usize);
                    for yy in cy..cy + h {
                        for xx in cx..cx + w {
                            let i = ((yy - y) * pic.w + (xx - x)) as usize;
                            img.push(blend(pa.pixel(&canvas.px, xx, yy), pic.argb[i]));
                        }
                    }
                    fs.screen.fb.present_image(cx, cy, w as usize, &img);
                    pointer_shown = Some((cx, cy, w, h));
                }
            }
        }

        stats.tick(live);

        // No rescan deadline (event-driven, past the boot window): the one-second tick is the whole deadline.
        let deadline = fs
            .inputs
            .next_rescan(Instant::now())
            .unwrap_or(Instant::now() + Duration::from_secs(1))
            .min(Instant::now() + Duration::from_secs(1));
        let timeout = deadline.saturating_duration_since(Instant::now()).as_millis().min(i32::MAX as u128) as i32;
        let mut fds: Vec<libc::pollfd> = fs
            .inputs
            .fds()
            .into_iter()
            .chain([wake_r])
            .map(|fd| libc::pollfd { fd, events: libc::POLLIN, revents: 0 })
            .collect();
        // SAFETY: `fds` is a valid array for the duration of the call. An interrupted poll just goes round the loop.
        unsafe { libc::poll(fds.as_mut_ptr(), fds.len() as libc::nfds_t, timeout.max(1)) };
        if fds.last().is_some_and(|f| f.revents & libc::POLLIN != 0) {
            let mut sink = [0u8; 64];
            // SAFETY: reads into a live local buffer; the pipe is non-blocking.
            while unsafe { libc::read(wake_r, sink.as_mut_ptr().cast(), sink.len()) } > 0 {}
        }
    };

    release_all(live, host, &mut keys_down);
    if buttons != 0 {
        for b in [BUTTON_LEFT, BUTTON_RIGHT, BUTTON_MIDDLE] {
            if buttons & b != 0 {
                live.send(|s| s.mouse(MASK_UP | b << 3, 0, 0));
            }
        }
    }
    stats.report(live);
    live.wake_fd.store(-1, Ordering::Relaxed);
    live.hang_up();
    // SAFETY: both ends are ours and nothing writes after the store above, except a racing wake that then fails harmlessly.
    unsafe {
        libc::close(wake_r);
        libc::close(wake_w);
    }
    why
}

/// No key stays down on the host when this keyboard goes away or the session ends.
fn release_all(live: &Live, host: Host, down: &mut BTreeSet<u16>) {
    for code in std::mem::take(down) {
        if let Some(c) = keymap::host_code(host, code) {
            live.send(|s| s.key_map(c, false));
        }
    }
}

fn process_cpu() -> Duration {
    let mut t = libc::timespec { tv_sec: 0, tv_nsec: 0 };
    // SAFETY: fills a live timespec.
    unsafe { libc::clock_gettime(libc::CLOCK_PROCESS_CPUTIME_ID, &mut t) };
    Duration::new(t.tv_sec as u64, t.tv_nsec as u32)
}

/// One line in the log every ten seconds: what the numbers in `docs/research/juno-desk.md` come from.
struct Stats {
    since: Instant,
    cpu_at: Duration,
    frames_at: u64,
    bytes_at: u64,
    decode_at: u64,
    convert_at: u64,
    shown: u64,
    rows: u64,
    present: Duration,
    /// A key went out and no picture has come back since.
    key_at: Option<Instant>,
    key_to_screen: Duration,
    keys_answered: u32,
}

impl Stats {
    fn new(now: Instant) -> Stats {
        Stats { since: now, cpu_at: process_cpu(), frames_at: 0, bytes_at: 0, decode_at: 0, convert_at: 0, shown: 0, rows: 0, present: Duration::ZERO, key_at: None, key_to_screen: Duration::ZERO, keys_answered: 0 }
    }

    fn key_sent(&mut self) {
        self.key_at = Some(Instant::now());
    }

    fn shown(&mut self, rows: u64, took: Duration) {
        if let Some(t) = self.key_at.take() {
            self.key_to_screen += t.elapsed();
            self.keys_answered += 1;
        }
        self.shown += 1;
        self.rows += rows;
        self.present += took;
    }

    fn tick(&mut self, live: &Live) {
        if self.since.elapsed() >= Duration::from_secs(10) {
            self.report(live);
        }
    }

    fn report(&mut self, live: &Live) {
        let dt = self.since.elapsed().as_secs_f64().max(0.001);
        let cpu = process_cpu();
        let c = &live.counters;
        let (frames, bytes, dec, conv) = (
            c.frames.load(Ordering::Relaxed),
            c.bytes.load(Ordering::Relaxed),
            c.decode_us.load(Ordering::Relaxed),
            c.convert_us.load(Ordering::Relaxed),
        );
        let n = frames - self.frames_at;
        let per = |v: u64| if n > 0 { v as f64 / n as f64 / 1000.0 } else { 0.0 };
        eprintln!(
            "lucina-app-desk: stats {:.1} fps in, {:.1} shown, {:.0} kB/s, decode {:.1} ms, convert {:.1} ms, present {:.1} ms a time, worst decode {:.0} ms, key to screen {:.0} ms ({}), CPU {:.0}%",
            n as f64 / dt,
            self.shown as f64 / dt,
            (bytes - self.bytes_at) as f64 / 1000.0 / dt,
            per(dec - self.decode_at),
            per(conv - self.convert_at),
            if self.shown > 0 { self.present.as_secs_f64() * 1000.0 / self.shown as f64 } else { 0.0 },
            c.worst_decode_us.swap(0, Ordering::Relaxed) as f64 / 1000.0,
            if self.keys_answered > 0 { self.key_to_screen.as_secs_f64() * 1000.0 / self.keys_answered as f64 } else { 0.0 },
            self.keys_answered,
            (cpu - self.cpu_at).as_secs_f64() * 100.0 / dt,
        );
        *self = Stats { frames_at: frames, bytes_at: bytes, decode_at: dec, convert_at: conv, ..Stats::new(Instant::now()) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blending() {
        assert_eq!(blend(0xff11_2233, 0x00ff_ffff), 0xff11_2233);
        assert_eq!(blend(0xff11_2233, 0xff44_5566), 0xff44_5566);
        let half = blend(0xff00_0000, 0x80ff_ffff);
        assert!((half & 0xff) >= 0x7f && (half & 0xff) <= 0x81, "{half:x}");
    }

    #[test]
    fn a_cursor_from_rgba_keeps_its_channels() {
        let p = from_rgba(1, 2, 1, 1, &[0x11, 0x22, 0x33, 0x44]);
        assert_eq!(p.argb, vec![0x4411_2233]);
        assert_eq!((p.hotx, p.hoty), (1, 2));
    }

    #[test]
    fn the_arrow_is_one_shape() {
        let a = arrow();
        assert_eq!(a.argb.len() as i32, a.w * a.h);
        assert_eq!(a.argb[0], 0xff00_0000);
    }
}
