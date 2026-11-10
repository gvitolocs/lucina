//! Fortuna's screens, drawn from plain models so the host (previews, tests) and the kernel draw the same pixels.
//!
//! Top screen (shows): the Home dashboard (the clock, Wi-Fi, uptime, activity with its ring, the watchdog), a status
//! strip over every other top screen, System's name and version, and the boot screen. Bottom screen (touch and
//! buttons): Home (the last game with the round Play, Fortuna's own games pinned), System's rows and Restart, the
//! segmented tab bar, dialogs. Files, Games and search are in `browse`.
//!
//! Copy follows docs/design/rules.md: one fact per card plus one short line, sentence case, buttons are verbs, a state
//! is a mark and a word (never colour alone), each fact once per screen pair.

use core::fmt::Write;

use crate::browse::{self, Game};
use crate::canvas::{Canvas, Rgba};
use crate::icons::{self as ic, Icon};
use crate::layout::{self as l, Rect, SCREEN_H, SCREEN_W};
use crate::skin::{self as s, ACCENT, ACCENT_FILL, INK, INK_2, INK_3, LIVE, POSITIVE, WARNING};
use crate::tokens::color as c;

pub(crate) struct Buf<const N: usize> {
    b: [u8; N],
    n: usize,
}
impl<const N: usize> Buf<N> {
    pub(crate) fn new() -> Self {
        Buf { b: [0; N], n: 0 }
    }
    pub(crate) fn s(&self) -> &str {
        core::str::from_utf8(&self.b[..self.n]).unwrap_or("")
    }
}
/// Counts text cut because a buffer was full (the host test holds it at 0).
#[cfg(test)]
pub(crate) static CUTS: core::sync::atomic::AtomicU32 = core::sync::atomic::AtomicU32::new(0);

impl<const N: usize> Write for Buf<N> {
    fn write_str(&mut self, s: &str) -> core::fmt::Result {
        if s.len() <= N - self.n {
            self.b[self.n..self.n + s.len()].copy_from_slice(s.as_bytes());
            self.n += s.len();
            return Ok(());
        }
        // full: what fits, then "…" (3 bytes), never a silent cut
        #[cfg(test)]
        CUTS.fetch_add(1, core::sync::atomic::Ordering::Relaxed);
        let room = (N - self.n).saturating_sub(3);
        let mut k = s.len().min(room);
        while !s.is_char_boundary(k) {
            k -= 1;
        }
        self.b[self.n..self.n + k].copy_from_slice(&s.as_bytes()[..k]);
        self.n += k;
        while self.n > N - 3 {
            self.n -= 1;
            while !self.s_ok() {
                self.n -= 1;
            }
        }
        self.b[self.n..self.n + 3].copy_from_slice("…".as_bytes());
        self.n += 3;
        Ok(())
    }
}

impl<const N: usize> Buf<N> {
    fn s_ok(&self) -> bool {
        core::str::from_utf8(&self.b[..self.n]).is_ok()
    }
}

/// `n` seconds the way a person says it: "40 s", "14 min", "2 h 14 min", "3 d 2 h".
pub(crate) fn span(out: &mut impl Write, n: u32) {
    let _ = match n {
        0..=59 => write!(out, "{n} s"),
        60..=3599 => write!(out, "{} min", n / 60),
        3600..=86399 => write!(out, "{} h {} min", n / 3600, n % 3600 / 60),
        _ => write!(out, "{} d {} h", n / 86400, n % 86400 / 3600),
    };
}

// ---- models -------------------------------------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Wifi {
    Off,
    Connecting,
    NoAp,
    Connected,
}

/// What the status strip shows.
#[derive(Clone, Copy, Debug)]
pub struct Bar {
    pub hh: u8,
    pub mm: u8,
    pub wifi: Wifi,
    /// The DSi's battery level 0..=15 (`getBatteryLevel() & 0xF`).
    pub battery: u8,
    pub charging: bool,
}

impl Bar {
    pub fn percent(&self) -> u32 {
        self.battery.min(15) as u32 * 100 / 15
    }
}

/// A watched service.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Service {
    Ok,
    /// Silent for this many seconds.
    Silent(u32),
}

/// What the activity card shows.
#[derive(Clone, Copy, Debug)]
pub enum Activity<'a> {
    Idle,
    /// An FTP transfer: its file, how much so far, the speed, and the second (for the ring's sweep: FTP does not say a
    /// file's size).
    Transfer { name: &'a str, receiving: bool, kib: u32, kib_s: u32, tick: u32 },
    /// The Games index filling: games found, folders done of folders seen.
    Indexing { found: u32, done: u32, seen: u32 },
}

/// The Home dashboard on the top screen.
#[derive(Clone, Copy, Debug)]
pub struct Dash<'a> {
    pub bar: Bar,
    pub date: &'a str,
    pub ip: [u8; 4],
    pub up_s: u32,
    pub activity: Activity<'a>,
    /// main loop, Wi-Fi link, FTP server.
    pub wd: [Service; 3],
    pub wd_paused_s: u32,
    /// What the watchdog did before this boot, if anything.
    pub recovered: bool,
}

// ---- pieces -------------------------------------------------------------------------------------------------------

/// A battery: an outline, a nub and a fill in proportion. Low is urgent (red), charging is positive.
pub(crate) fn battery(cv: &mut Canvas, x: i32, y: i32, b: &Bar) {
    let (w, h) = (17, 8);
    let ink = INK_2;
    cv.round_rect(x, y, w, h, 2, ink);
    cv.round_rect(x + 1, y + 1, w - 2, h - 2, 1, s::BOTTOM_BG);
    cv.fill(x + w, y + 2, 1, h - 4, ink);
    let fill = (b.battery.min(15) as i32 * (w - 4) + 7) / 15;
    let tint = if b.charging {
        POSITIVE
    } else if b.battery <= 3 {
        LIVE
    } else {
        INK
    };
    if fill > 0 {
        cv.fill(x + 2, y + 2, fill, h - 4, tint);
    }
}

/// The status strip over every top screen but Home: the clock, Wi-Fi and the battery, small.
pub fn strip(cv: &mut Canvas, b: &Bar) {
    let mut t = Buf::<8>::new();
    let _ = write!(t, "{:02}:{:02}", b.hh, b.mm);
    cv.text(s::CAPTION, l::EDGE + 2, 11, t.s(), INK);
    battery(cv, SCREEN_W - l::EDGE - 20, 3, b);
    let wi = if b.wifi == Wifi::Connected { INK } else { INK_3 };
    cv.icon(&ic::WIFI_12, SCREEN_W - l::EDGE - 38, 1, wi);
    if b.wifi == Wifi::NoAp {
        cv.text_right(s::FOOTNOTE, SCREEN_W - l::EDGE - 42, 10, "No network", INK_2);
    }
}

/// A card's label: its icon and a sentence-case caption.
pub(crate) fn label(cv: &mut Canvas, x: i32, y: i32, icon: &'static Icon, text: &str) {
    cv.icon(icon, x, y, INK_2);
    cv.text(s::CAPTION, x + 16, y + 10, text, INK_2);
}

/// A state as a mark and a word, never colour alone: a check or a cross before the word.
pub(crate) fn state(cv: &mut Canvas, x: i32, baseline: i32, ok: bool, word: &str) -> i32 {
    let (icon, col) = if ok { (&ic::CHECK_12, POSITIVE) } else { (&ic::CROSS_12, LIVE) };
    cv.icon(icon, x, baseline - 10, col);
    14 + cv.text(s::HEADLINE, x + 14, baseline, word, INK)
}

/// The segmented control along the bottom: a glass capsule, the active segment an accent pill.
pub fn tab_bar(cv: &mut Canvas, active: usize, focused: bool) {
    let r = l::TAB_BAR;
    s::glass(cv, r.x, r.y, r.w, r.h, r.h / 2);
    for (i, name) in l::TAB_NAMES.iter().enumerate() {
        let t = l::tab(i);
        let ink = if i == active {
            cv.pill(t.x, t.y, t.w, t.h, ACCENT_FILL);
            INK
        } else {
            INK_2
        };
        cv.text_centre(s::HEADLINE, t.cx(), t.y + 16, name, ink);
        if focused && i == active {
            s::focus(cv, t.x, t.y, t.w, t.h, t.h / 2);
        }
    }
}

// ---- top: the Home dashboard ----------------------------------------------------------------------------------------

/// The top screen on Home. Each fact once: the clock is here and not in a strip; the version is on System.
pub fn dashboard(cv: &mut Canvas, d: &Dash) {
    s::top_backdrop(cv);
    let g = l::EDGE;
    // the clock, the date and the battery
    let head = Rect::new(g, 6, SCREEN_W - 2 * g, 58);
    s::material(cv, head.x, head.y, head.w, head.h, l::PANEL_R);
    let mut t = Buf::<8>::new();
    let _ = write!(t, "{:02}:{:02}", d.bar.hh, d.bar.mm);
    cv.text(s::DISPLAY_XL, head.x + 9, head.y + 46, t.s(), INK);
    cv.text_right(s::BODY, head.right() - 10, head.y + 22, d.date, INK_2);
    let mut p = Buf::<8>::new();
    let _ = write!(p, "{} %", d.bar.percent());
    let pw = cv.text_right(s::FOOTNOTE, head.right() - 10, head.y + 46, p.s(), INK_2);
    battery(cv, head.right() - 10 - pw - 24, head.y + 39, &d.bar);

    // Wi-Fi and uptime
    let row = head.bottom() + g;
    let half = (head.w - g) / 2;
    let wf = Rect::new(g, row, half, 52);
    let up = Rect::new(g + half + g, row, head.w - half - g, 52);
    s::material(cv, wf.x, wf.y, wf.w, wf.h, l::PANEL_R);
    s::material(cv, up.x, up.y, up.w, up.h, l::PANEL_R);
    label(cv, wf.x + 9, wf.y + 7, &ic::WIFI_12, "Wi-Fi");
    let mut ip = Buf::<24>::new();
    match d.bar.wifi {
        Wifi::Connected => {
            // the check says connected; the address is the fact
            let mut a = Buf::<24>::new();
            let _ = write!(a, "{}.{}.{}.{}", d.ip[0], d.ip[1], d.ip[2], d.ip[3]);
            state(cv, wf.x + 9, wf.y + 35, true, a.s());
        }
        Wifi::Connecting => {
            cv.ring(wf.x as f32 + 15.0, wf.y as f32 + 31.0, 6.0, 2.0, 0, 1, 4, ACCENT, INK_3);
            cv.text(s::HEADLINE, wf.x + 23, wf.y + 35, "Connecting", INK);
            let _ = write!(ip, "FTP starts after");
        }
        Wifi::NoAp => {
            state(cv, wf.x + 9, wf.y + 35, false, "No network");
            let _ = write!(ip, "Retrying");
        }
        Wifi::Off => {
            state(cv, wf.x + 9, wf.y + 35, false, "Off");
        }
    }
    cv.text(s::FOOTNOTE, wf.x + 9, wf.y + 47, ip.s(), INK_2);
    label(cv, up.x + 9, up.y + 7, &ic::CLOCK_12, "Uptime");
    let mut u = Buf::<24>::new();
    span(&mut u, d.up_s);
    cv.text_fit(s::VALUE, up.x + 9, up.y + 42, u.s(), up.w - 18, INK);

    // activity (with the ring) and the watchdog
    let row = wf.bottom() + g;
    let act = Rect::new(g, row, 160, SCREEN_H - 6 - row);
    let wd = Rect::new(act.right() + g, row, head.right() - act.right() - g, act.h);
    s::material(cv, act.x, act.y, act.w, act.h, l::PANEL_R);
    s::material(cv, wd.x, wd.y, wd.w, wd.h, l::PANEL_R);
    let (rx, ry) = (act.right() as f32 - 30.0, act.y as f32 + act.h as f32 / 2.0);
    match d.activity {
        Activity::Idle => {
            label(cv, act.x + 9, act.y + 7, &ic::DOWNLOAD_12, "Activity");
            cv.text(s::HEADLINE, act.x + 9, act.y + 36, "Idle", INK);
        }
        Activity::Transfer { name, receiving, kib, kib_s, tick } => {
            cv.ring(rx, ry, 19.0, 3.0, tick % 8, 2, 8, ACCENT, s::HAIR);
            cv.icon(if receiving { &ic::DOWNLOAD_16 } else { &ic::UPLOAD_16 }, rx as i32 - 8, ry as i32 - 8, INK);
            let max = rx as i32 - 24 - (act.x + 9);
            cv.text_fit(s::HEADLINE, act.x + 9, act.y + 30, name, max, INK);
            let mut l1 = Buf::<32>::new();
            let _ = write!(l1, "{}.{} MB · {kib_s} KB/s", kib / 1024, (kib % 1024) * 10 / 1024);
            cv.text_fit(s::FOOTNOTE, act.x + 9, act.y + 43, l1.s(), max, INK_2);
        }
        Activity::Indexing { found, done, seen } => {
            cv.ring(rx, ry, 19.0, 3.0, 0, done, seen.max(1), ACCENT, s::HAIR);
            cv.icon(&ic::GAMEPAD_16, rx as i32 - 8, ry as i32 - 8, INK);
            cv.text(s::HEADLINE, act.x + 9, act.y + 30, "Indexing games", INK);
            let mut l1 = Buf::<24>::new();
            let _ = write!(l1, "{found} found");
            cv.text(s::FOOTNOTE, act.x + 9, act.y + 43, l1.s(), INK_2);
        }
    }
    label(cv, wd.x + 9, wd.y + 7, &ic::INFO_12, "Watchdog");
    let names = ["Main", "Wi-Fi", "FTP"];
    let mut line = Buf::<24>::new();
    if d.wd_paused_s > 0 {
        cv.text(s::HEADLINE, wd.x + 9, wd.y + 36, "Paused", INK);
        let _ = write!(line, "{}:{:02} left", d.wd_paused_s / 60, d.wd_paused_s % 60);
        cv.text(s::FOOTNOTE, wd.x + 9, wd.y + 48, line.s(), INK_2);
    } else if let Some(i) = d.wd.iter().position(|w| *w != Service::Ok) {
        state(cv, wd.x + 9, wd.y + 36, false, names[i]);
        if let Service::Silent(n) = d.wd[i] {
            let _ = write!(line, "Stalled {n} s");
        }
        cv.text(s::FOOTNOTE, wd.x + 9, wd.y + 48, line.s(), LIVE);
    } else {
        state(cv, wd.x + 9, wd.y + 36, true, "OK");
        if d.recovered {
            cv.text(s::FOOTNOTE, wd.x + 9, wd.y + 48, "Recovered", WARNING);
        }
    }
}

// ---- bottom: Home -------------------------------------------------------------------------------------------------

/// A pinned game on Home: Fortuna's own builds, with a coloured app icon.
#[derive(Clone, Copy, Debug)]
pub struct Pin<'a> {
    pub name: &'a str,
    pub icon: &'static Icon,
    pub tint: Rgba,
}

/// What Home's focus ring is on (from the buttons).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HomeFocus {
    Play,
    Pin(usize),
    Tabs,
}

/// Home: the last game played with the round Play, Fortuna's own games pinned under it, the tab bar.
pub fn home(cv: &mut Canvas, last: Option<&Game>, pins: &[Pin], focus: Option<HomeFocus>) {
    s::bottom_backdrop(cv);
    let k = l::CONTINUE;
    s::material(cv, k.x, k.y, k.w, k.h, l::PANEL_R);
    match last {
        Some(g) => {
            browse::cover(cv, l::CONTINUE_COVER, g);
            let x = l::CONTINUE_COVER.right() + 10;
            let max = l::PLAY.x - 8 - x;
            let n = cv.text_wrap(s::GAME_TITLE, x, k.y + 28, g.name_shown(), max, 2, 18, INK);
            let mut when = Buf::<24>::new();
            browse::played(&mut when, g.played_ago);
            cv.text(s::FOOTNOTE, x, k.y + 28 + n * 18 + 2, when.s(), INK_2);
            play_button(cv, l::PLAY);
            if focus == Some(HomeFocus::Play) {
                s::focus(cv, l::PLAY.x, l::PLAY.y, l::PLAY.w, l::PLAY.h, l::PLAY.h / 2);
            }
        }
        None => {
            cv.text(s::HEADLINE, k.x + 12, k.y + 38, "No game played yet", INK);
            cv.text(s::FOOTNOTE, k.x + 12, k.y + 52, "Games lists every game on the card", INK_2);
        }
    }
    // Fortuna's own games as icons only, on a glass shelf like a Dock; the focused one shows its name above it
    let shelf = Rect::new(l::pin(0).x, l::pin(0).y + 4, l::pin(l::PINS - 1).right() - l::pin(0).x, l::PIN_H - 8);
    s::glass(cv, shelf.x, shelf.y, shelf.w, shelf.h, 14);
    let mut named: Option<(Rect, &str)> = None;
    for (i, p) in pins.iter().take(l::PINS).enumerate() {
        let a = pin_icon(i);
        cv.round_rect(a.x, a.y, a.w, a.h, 10, p.tint);
        let sz = p.icon.size as i32;
        cv.icon(p.icon, a.cx() - sz / 2, a.cy() - sz / 2, s::ON_LIGHT);
        if focus == Some(HomeFocus::Pin(i)) {
            s::focus(cv, a.x, a.y, a.w, a.h, 10);
            named = Some((a, p.name));
        }
    }
    if let Some((a, n)) = named {
        s::name_label(cv, a.x, a.y - 2, a.w, n);
    }
    tab_bar(cv, l::TAB_HOME, focus == Some(HomeFocus::Tabs));
}

/// Where pin `i`'s app icon sits (40 px, centred in its tap area).
pub const fn pin_icon(i: usize) -> Rect {
    let r = l::pin(i);
    Rect::new(r.x + (r.w - 40) / 2, r.y + (r.h - 40) / 2, 40, 40)
}

/// The round primary button: accentFill with the play glyph.
pub fn play_button(cv: &mut Canvas, r: Rect) {
    cv.note_control();
    cv.dot(r.x, r.y, r.w, ACCENT_FILL);
    let icon = if r.w >= 40 { &ic::PLAY_24 } else { &ic::PLAY_16 };
    let sz = icon.size as i32;
    cv.icon(icon, r.cx() - sz / 2 + 1, r.cy() - sz / 2, INK);
}

// ---- Lucina -------------------------------------------------------------------------------------------------------

/// The Lucina tab's bottom screen until its chat and agents screens land: her mark, her name, the tab bar.
pub fn lucina(cv: &mut Canvas, focused: bool) {
    s::bottom_backdrop(cv);
    let mid = l::CONTENT_BOTTOM / 2;
    cv.icon(&ic::LUCINA_24, SCREEN_W / 2 - 12, mid - 30, INK_2);
    cv.text_centre(s::HEADLINE, SCREEN_W / 2, mid + 12, "Lucina", INK);
    cv.text_centre(s::FOOTNOTE, SCREEN_W / 2, mid + 28, "Chat and agents", INK_2);
    tab_bar(cv, l::TAB_LUCINA, focused);
}

/// The Lucina tab's top screen: her name and one line (content only: the top screen takes no touch).
pub fn lucina_top(cv: &mut Canvas, bar: &Bar, line: &str) {
    s::top_backdrop(cv);
    strip(cv, bar);
    let g = l::EDGE;
    let head = Rect::new(g, 18, SCREEN_W - 2 * g, 70);
    s::material(cv, head.x, head.y, head.w, head.h, l::PANEL_R);
    cv.icon(&ic::LUCINA_24, head.x + 10, head.y + 14, INK);
    cv.text(s::DISPLAY, head.x + 42, head.y + 36, "Lucina", INK);
    cv.text(s::FOOTNOTE, head.x + 12, head.y + 56, line, INK_2);
}

// ---- System -------------------------------------------------------------------------------------------------------

/// One row of a grouped list (System, Files, search): an icon, a label, an optional state mark, a value on the right.
#[derive(Clone, Copy, Debug)]
pub struct Row<'a> {
    pub icon: &'static Icon,
    pub label: &'a str,
    /// A state mark before the value: true = check, false = cross.
    pub ok: Option<bool>,
    pub value: &'a str,
    /// A chevron at the right: the row opens something.
    pub opens: bool,
}

/// Rows in one material group, `first_row` being the on-screen slot of the first; `focus` is the focused slot.
pub fn rows(cv: &mut Canvas, rows: &[Row], first_slot: i32, focus: Option<i32>) {
    let n = rows.len() as i32;
    if n == 0 {
        return;
    }
    let top = l::row(first_slot);
    s::material(cv, top.x, top.y, top.w, n * l::ROW_H, l::PANEL_R);
    for (i, r) in rows.iter().enumerate() {
        let b = l::row(first_slot + i as i32);
        if i > 0 {
            cv.fill(b.x + 30, b.y, b.w - 30, 1, s::HAIR);
        }
        row(cv, b, r);
    }
    // the ring after every row, so no later separator is drawn over it
    if let Some(f) = focus.filter(|f| (first_slot..first_slot + n).contains(f)) {
        let b = l::row(f);
        s::focus(cv, b.x + 1, b.y + 1, b.w - 2, b.h - 2, 6);
    }
}

fn row(cv: &mut Canvas, b: Rect, r: &Row) {
    cv.icon(r.icon, b.x + 8, b.y + 4, INK_2);
    let mut right = b.right() - 9;
    if r.opens {
        cv.icon(&ic::NEXT_12, right - 10, b.y + 6, INK_2);
        right -= 14;
    }
    // the value takes at most three fifths of the row, cut with "…" past that; the label gets the rest
    let vmax = (right - (b.x + 30)) * 3 / 5;
    let vw = cv.text_right_fit(s::FOOTNOTE, right, b.y + 16, r.value, vmax, INK_2);
    let mut lmax = right - vw - 6 - (b.x + 30);
    if let Some(ok) = r.ok {
        let (icon, col) = if ok { (&ic::CHECK_12, POSITIVE) } else { (&ic::CROSS_12, LIVE) };
        cv.icon(icon, right - vw - 15, b.y + 6, col);
        lmax -= 15;
    }
    cv.text_fit(s::BODY, b.x + 30, b.y + 16, r.label, lmax, INK);
}

/// The System screen's bottom: its rows and the Restart button. Restart is red only while a transfer would be cut.
pub fn system(cv: &mut Canvas, list: &[Row], transfer_running: bool, focus: Option<i32>) {
    s::bottom_backdrop(cv);
    rows(cv, &list[..list.len().min(l::SYSTEM_ROWS as usize)], 0, focus);
    let b = l::RESTART;
    let ink = if transfer_running {
        s::glass(cv, b.x, b.y, b.w, b.h, b.h / 2);
        LIVE
    } else {
        cv.pill(b.x, b.y, b.w, b.h, ACCENT_FILL);
        INK
    };
    let w = s::HEADLINE.width("Restart") + 18;
    cv.icon(&ic::RESTART_16, b.cx() - w / 2, b.y + 4, ink);
    cv.text(s::HEADLINE, b.cx() - w / 2 + 18, b.y + 16, "Restart", ink);
    if focus == Some(l::SYSTEM_ROWS) {
        s::focus(cv, b.x, b.y, b.w, b.h, b.h / 2);
    }
    tab_bar(cv, l::TAB_SYSTEM, focus == Some(-1));
}

/// One stat card: an icon and label, a value in the display face, a short line after it.
#[derive(Clone, Copy, Debug)]
pub struct Stat<'a> {
    pub icon: &'static Icon,
    pub label: &'a str,
    pub value: &'a str,
    pub line: &'a str,
}

/// System's top screen: the name and version (the one place they show), and one stat card.
pub fn system_top(cv: &mut Canvas, bar: &Bar, version: &str, boots: u32, stat: &Stat) {
    s::top_backdrop(cv);
    strip(cv, bar);
    let g = l::EDGE;
    let head = Rect::new(g, 18, SCREEN_W - 2 * g, 70);
    s::material(cv, head.x, head.y, head.w, head.h, l::PANEL_R);
    cv.text(s::DISPLAY, head.x + 10, head.y + 36, "Fortuna", INK);
    let mut v = Buf::<40>::new();
    if boots > 0 {
        let _ = write!(v, "Version {version} · boot {boots}");
    } else {
        let _ = write!(v, "Version {version}");
    }
    cv.text(s::FOOTNOTE, head.x + 12, head.y + 56, v.s(), INK_2);
    let card = Rect::new(g, head.bottom() + g, head.w, 56);
    s::material(cv, card.x, card.y, card.w, card.h, l::PANEL_R);
    label(cv, card.x + 9, card.y + 7, stat.icon, stat.label);
    let w = cv.text(s::VALUE, card.x + 9, card.y + 40, stat.value, INK);
    cv.text(s::FOOTNOTE, card.x + 9 + w + 6, card.y + 40, stat.line, INK_2);
}

// ---- dialogs ------------------------------------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Button {
    /// Cancel: a quiet field pill.
    Plain,
    /// The main action: accentFill.
    Primary,
    /// An action that would cut something running: red text.
    Destructive,
}

/// What the scrim keeps of the page under a dialog: tokens.json scrim #00000099.
const SCRIM: Rgba = Rgba::new(0, 0, 0, c::SCRIM_ALPHA);
/// tokens.json field (#76768029): a quiet button.
const FIELD: Rgba = Rgba::new(0x76, 0x76, 0x80, c::FIELD_ALPHA);

/// A dialog over whatever the frame holds: the scrim, a glass card with a title, one or two short lines and one or two
/// verb buttons; `focus` is the focused button. Returns the buttons' rectangles.
pub fn alert(cv: &mut Canvas, title: &str, line: &str, buttons: &[(&str, Button)], focus: Option<usize>) -> [Rect; 2] {
    // the page under a dialog is flattened into the base: the dialog's own changes redraw only the dialog
    cv.flatten();
    cv.fill(0, 0, SCREEN_W, SCREEN_H, SCRIM);
    let room = l::ALERT_W - 24;
    let lines = if s::FOOTNOTE.width(line) > room { 2 } else { 1 };
    let h = 14 + 16 + lines * 12 + 12 + l::TAP + 10;
    let a = l::alert(h);
    s::sheet(cv, a.x, a.y, a.w, a.h, 16);
    let f = if s::TITLE.width(title) <= room { s::TITLE } else { s::HEADLINE };
    let tw = f.width(title).min(room);
    cv.text_fit(f, a.cx() - tw / 2, a.y + 28, title, room, INK);
    let mut rest = line;
    for i in 0..lines {
        let part = if i + 1 == lines { rest } else { crate::canvas::wrap_line(s::FOOTNOTE, rest, room) };
        let pw = s::FOOTNOTE.width(part).min(room);
        cv.text_fit(s::FOOTNOTE, a.cx() - pw / 2, a.y + 44 + i * 12, part, room, INK_2);
        rest = rest[part.len()..].trim_start();
    }
    let mut out = [Rect::default(); 2];
    let n = buttons.len().clamp(1, 2) as i32;
    for (i, (label, kind)) in buttons.iter().take(2).enumerate() {
        let b = l::alert_button(a, i as i32, n);
        out[i] = b;
        let (fill, ink) = match kind {
            Button::Plain => (FIELD, INK),
            Button::Primary => (ACCENT_FILL, INK),
            Button::Destructive => (FIELD, LIVE),
        };
        cv.note_control();
        cv.pill(b.x, b.y, b.w, b.h, fill);
        cv.text_centre(s::HEADLINE, b.cx(), b.y + 16, label, ink);
        if focus == Some(i) {
            s::focus(cv, b.x, b.y, b.w, b.h, b.h / 2);
        }
    }
    out
}

// ---- boot ---------------------------------------------------------------------------------------------------------

/// The boot screen on the top: the name in the display face, a ring for the steps, what is starting.
pub fn boot(cv: &mut Canvas, step: u32, steps: u32, what: &str) {
    s::top_backdrop(cv);
    cv.text_centre(s::DISPLAY_XL, SCREEN_W / 2, 92, "Fortuna", INK);
    cv.ring(SCREEN_W as f32 / 2.0, 124.0, 10.0, 2.5, 0, step, steps.max(1), ACCENT, s::HAIR);
    cv.text_centre(s::FOOTNOTE, SCREEN_W / 2, 152, what, INK_2);
}
