//! The sign-in by QR pairing: Sign in, the code and the six digits, New code.

use core::fmt::Write;

use crate::canvas::Canvas;
use crate::icons as ic;
use crate::layout::{self as l, SCREEN_W};
use crate::screens::{self, Bar, Buf};
use crate::skin::{self as s, INK, INK_2};
use crate::talk::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Pair<'a> {
    /// Signed out, nothing asked yet: a Sign in button (a code is asked for only on a press).
    Idle,
    Loading,
    /// The QR code's modules (`width` x `width`, row by row) and the six digits.
    Ready { modules: &'a [bool], width: usize, code: &'a str, left_s: u32 },
    Denied,
    Paused,
    Unavailable { retry_s: u32 },
    /// The DSi's clock was never set: no certificate can be checked, so nothing connects.
    ClockUnset,
}

/// The top screen while signing in.
pub fn pair_top(cv: &mut Canvas, bar: &Bar) {
    s::top_backdrop(cv);
    screens::strip(cv, bar);
    cv.text_centre(s::DISPLAY, SCREEN_W / 2, 84, "Sign in", INK);
    cv.text_centre(s::BODY, SCREEN_W / 2, 108, "Approve this DSi in Secretary", INK_2);
    cv.text_centre(s::FOOTNOTE, SCREEN_W / 2, 124, "Account, then Scan QR code", INK_2);
}

/// The bottom screen while signing in: the QR code and the six digits, or what went wrong with a button.
pub fn pair_bottom(cv: &mut Canvas, p: &Pair, focus_again: bool, tabs_focused: bool) {
    s::bottom_backdrop(cv);
    match *p {
        Pair::Ready { modules, width, code, left_s } => {
            // quiet zone of 2 modules on white; as large as the content area allows
            let q = 2;
            let side = (l::CONTENT_BOTTOM - 8).min(150);
            let scale = (side / (width as i32 + 2 * q)).max(1);
            let full = scale * (width as i32 + 2 * q);
            let (x0, y0) = (l::EDGE + 2, (l::CONTENT_BOTTOM - full) / 2);
            cv.fill(x0, y0, full, full, crate::canvas::Rgba::hex(0xffffff));
            qr_rects(modules, width, |mx, my, w, h| {
                cv.fill(x0 + (mx + q) * scale, y0 + (my + q) * scale, w * scale, h * scale, crate::canvas::Rgba::hex(0x000000));
            });
            let cx = (x0 + full + SCREEN_W - l::EDGE) / 2;
            let shown = if code.len() == 6 { [&code[..3], " ", &code[3..]] } else { [code, "", ""] };
            let mut b = Buf::<16>::new();
            for part in shown {
                let _ = b.write_str(part);
            }
            cv.text_centre(s::VALUE, cx, 64, b.s(), INK);
            cv.text_centre(s::FOOTNOTE, cx, 82, "or type the code", INK_2);
            let t = mmss(left_s * 1000);
            cv.text_centre(s::FOOTNOTE, cx, 112, t.s(), INK_2);
        }
        Pair::Idle => {
            cv.icon(&ic::LUCINA_24, SCREEN_W / 2 - 12, 44, INK_2);
            cv.text_centre(s::BODY, SCREEN_W / 2, 92, "Not signed in", INK_2);
            button(cv, PAIR_AGAIN, "Sign in", true, focus_again);
        }
        Pair::Loading => {
            cv.text_centre(s::BODY, SCREEN_W / 2, 76, "Asking for a code", INK_2);
        }
        Pair::Unavailable { retry_s } => {
            let mut b = Buf::<32>::new();
            let _ = write!(b, "No answer, again in {retry_s} s");
            cv.text_centre(s::BODY, SCREEN_W / 2, 76, b.s(), INK_2);
        }
        Pair::ClockUnset => {
            cv.text_centre(s::BODY, SCREEN_W / 2, 70, "Set the DSi's clock", INK);
            cv.text_centre(s::FOOTNOTE, SCREEN_W / 2, 88, "System Settings, then come back here", INK_2);
        }
        Pair::Denied | Pair::Paused => {
            let line = if matches!(p, Pair::Denied) { "Declined on the phone" } else { "No answer for a while" };
            cv.text_centre(s::BODY, SCREEN_W / 2, 84, line, INK_2);
            button(cv, PAIR_AGAIN, "New code", true, focus_again);
        }
    }
    screens::tab_bar(cv, l::TAB_LUCINA, tabs_focused);
}


/// The dark modules of a QR code as few rectangles: each row's runs of dark modules, a run merged with the one
/// right above it when both cover the same columns. One fill a module (a version 4 code: ~550) overflowed the
/// compositor's display list (`compose::MAX_OPS`) on the DSi's first real pairing (v1.58), cutting the code off
/// and dropping the six digits and the tab bar.
pub fn qr_rects(modules: &[bool], width: usize, mut emit: impl FnMut(i32, i32, i32, i32)) {
    // open rectangles: (first column, columns, first row); a row has at most width / 2 + 1 runs
    const MAX_W: usize = 64;
    if width == 0 || width > MAX_W || modules.len() < width * width {
        return;
    }
    let mut open = [(0i32, 0i32, 0i32); MAX_W / 2 + 1];
    let mut n_open = 0;
    for y in 0..=width {
        let mut runs = [(0i32, 0i32); MAX_W / 2 + 1];
        let mut n_runs = 0;
        if y < width {
            let row = &modules[y * width..(y + 1) * width];
            let mut x = 0;
            while x < width {
                if row[x] {
                    let s = x;
                    while x < width && row[x] {
                        x += 1;
                    }
                    runs[n_runs] = (s as i32, (x - s) as i32);
                    n_runs += 1;
                } else {
                    x += 1;
                }
            }
        }
        // close the open rectangles this row does not continue; keep the ones it does
        let mut kept = [(0i32, 0i32, 0i32); MAX_W / 2 + 1];
        let mut n_kept = 0;
        let mut used = [false; MAX_W / 2 + 1];
        for &(ox, ow, oy) in &open[..n_open] {
            match runs[..n_runs].iter().position(|&(rx, rw)| rx == ox && rw == ow) {
                Some(k) => {
                    used[k] = true;
                    kept[n_kept] = (ox, ow, oy);
                    n_kept += 1;
                }
                None => emit(ox, oy, ow, y as i32 - oy),
            }
        }
        for (k, &(rx, rw)) in runs[..n_runs].iter().enumerate() {
            if !used[k] {
                kept[n_kept] = (rx, rw, y as i32);
                n_kept += 1;
            }
        }
        open = kept;
        n_open = n_kept;
    }
}
