//! Lucina's chat: the conversation on the top screen, the composer and the hold-to-talk microphone on the bottom.

use crate::browse;
use crate::canvas::Canvas;
use crate::icons as ic;
use crate::layout::{self as l, Key, SCREEN_H, SCREEN_W};
use crate::screens::{self, Bar};
use crate::skin::{self as s, ACCENT, ACCENT_FILL, INK, INK_2, LIVE};
use crate::talk::*;

const CARD_W: i32 = 196;
pub(crate) const LINE_STEP: i32 = 13;

/// The lines a message's text takes at most, by where it is: the newest gets more.
pub(crate) fn card_lines(m: &Msg, newest: bool) -> i32 {
    if m.hidden || m.text.is_empty() {
        return 1;
    }
    let lines = if newest { 6 } else { 3 };
    let mut rest = m.text.trim();
    let mut n = 0;
    while !rest.is_empty() && n < lines {
        let line = crate::canvas::wrap_line(s::BODY, rest, CARD_W - 16);
        rest = rest[line.len()..].trim_start();
        n += 1;
    }
    n.max(1)
}

/// A voice note's strip: this many bars, each `BAR_W` wide with a one-pixel gap, up to `BAR_H` tall.
const BARS: usize = 24;
const BAR_W: i32 = 2;
const BAR_H: i32 = 12;
/// A voice note's card: the microphone, the length, the strip.
const VOICE_W: i32 = 24 + 30 + BARS as i32 * (BAR_W + 1) + 8;

fn card_h(m: &Msg, newest: bool) -> i32 {
    // a voice note is the note alone (the owner, 2026-11-08): no text, not even a file name
    let body = if m.voice_ms.is_some() { 16 } else { card_lines(m, newest) * LINE_STEP };
    10 + body + if m.state != MsgState::Sent { 12 } else { 0 }
}

/// `wave` as `BARS` heights in pixels: the loudest byte of each slice; an empty strip is flat.
pub(crate) fn bars(wave: &[u8]) -> [i32; BARS] {
    core::array::from_fn(|b| {
        if wave.is_empty() {
            return 2;
        }
        let n = wave.len();
        let start = b * n / BARS;
        let end = ((b + 1) * n / BARS).max(start + 1).min(n);
        let v = wave[start.min(n - 1)..end.max(start.min(n - 1) + 1)].iter().copied().max().unwrap_or(0) as i32;
        2 + v * (BAR_H - 2) / 255
    })
}

fn card(cv: &mut Canvas, m: &Msg, y: i32, newest: bool) {
    let h = card_h(m, newest);
    let text_w = if m.hidden || m.text.is_empty() { 0 } else { s::BODY.width(m.text) };
    let w = if m.voice_ms.is_some() { VOICE_W } else { (text_w + 16).clamp(70, CARD_W) };
    let x = if m.lucina { l::EDGE } else { SCREEN_W - l::EDGE - w };
    if m.lucina {
        s::material(cv, x, y, w, h, 10);
    } else {
        cv.round_rect(x, y, w, h, 10, ACCENT_FILL);
    }
    let mut ty = y + 15;
    if let Some(ms) = m.voice_ms {
        cv.icon(&ic::MIC_12, x + 8, ty - 10, INK);
        let t = mmss(ms);
        cv.text(s::HEADLINE, x + 24, ty, t.s(), INK);
        let bx = x + 24 + 30;
        for (i, h) in bars(m.wave).iter().enumerate() {
            cv.fill(bx + i as i32 * (BAR_W + 1), ty - 5 - h / 2, BAR_W, *h, INK);
        }
        ty += 1;
    } else if m.hidden {
        cv.text_fit(s::BODY, x + 8, ty, "Open it on your phone", w - 16, INK);
    } else if !m.text.is_empty() {
        let lines = card_lines(m, newest);
        cv.text_wrap(s::BODY, x + 8, ty, m.text, w - 16, lines, LINE_STEP, INK);
        ty += (lines - 1) * LINE_STEP;
    } else if m.voice_ms.is_none() && m.others > 0 {
        cv.text_fit(s::BODY, x + 8, ty, "Sent a file", w - 16, INK);
    }
    match m.state {
        MsgState::Sent => {}
        MsgState::Sending => {
            cv.text(s::FOOTNOTE, x + 8, ty + 12, "Sending", INK);
        }
        MsgState::Failed => {
            cv.icon(&ic::CROSS_12, x + 6, ty + 2, LIVE);
            cv.text(s::FOOTNOTE, x + 20, ty + 12, "Not sent", INK);
        }
    }
}

/// The top screen of Chat: the conversation, the newest at the bottom, as many cards as fit under the strip.
pub fn chat_top(cv: &mut Canvas, bar: &Bar, msgs: &[Msg], thinking: bool) {
    s::top_backdrop(cv);
    screens::strip(cv, bar);
    if msgs.is_empty() && !thinking {
        cv.text_centre(s::DISPLAY, SCREEN_W / 2, 92, "Lucina", INK);
        cv.text_centre(s::FOOTNOTE, SCREEN_W / 2, 112, "Say something below", INK_2);
        return;
    }
    let top = 20;
    let mut y = SCREEN_H - 4;
    if thinking {
        y -= 22;
        s::material(cv, l::EDGE, y, 46, 18, 9);
        for i in 0..3 {
            cv.dot(l::EDGE + 13 + i * 9, y + 6, 6, INK_2);
        }
        y -= 4;
    }
    for (k, m) in msgs.iter().rev().enumerate() {
        let h = card_h(m, k == 0);
        if y - h < top {
            break;
        }
        y -= h;
        card(cv, m, y, k == 0);
        y -= 4;
    }
}

/// The bottom screen of Chat when not typing: the field (or, recording, the record bar), Send when there is a draft,
/// the microphone in the middle, the last problem in one line.
pub fn chat_bottom(cv: &mut Canvas, draft: &str, rec: Rec, problem: Option<&str>, focus: Option<ChatFocus>) {
    s::bottom_backdrop(cv);
    segment(cv, 0, if let Some(ChatFocus::Seg(i)) = focus { Some(i) } else { None });
    let f = FIELD;
    match rec {
        Rec::Off => {
            cv.note_control();
            s::glass(cv, f.x, f.y, f.w, f.h, f.h / 2);
            if draft.is_empty() {
                cv.text(s::BODY, f.x + 12, f.y + 18, "Message", INK_2);
            } else {
                cv.text_fit(s::BODY, f.x + 12, f.y + 18, draft, f.w - 24, INK);
            }
            if focus == Some(ChatFocus::Field) {
                s::focus(cv, f.x, f.y, f.w, f.h, f.h / 2);
            }
            let b = SEND;
            cv.note_control();
            if draft.is_empty() {
                s::glass(cv, b.x, b.y, b.w, b.h, b.h / 2);
                cv.icon(&ic::SEND_16, b.cx() - 8, b.cy() - 8, s::INK_3);
            } else {
                cv.pill(b.x, b.y, b.w, b.h, ACCENT_FILL);
                cv.icon(&ic::SEND_16, b.cx() - 8, b.cy() - 8, INK);
            }
            if focus == Some(ChatFocus::Send) {
                s::focus(cv, b.x, b.y, b.w, b.h, b.h / 2);
            }
        }
        Rec::On { ms, bars } => {
            // overlays: the timer and the meter change every frame, the bar under them does not
            s::glass(cv, f.x, f.y, f.w + SEND.w + 6, f.h, f.h / 2);
            cv.begin_overlay();
            cv.dot(f.x + 10, f.y + 9, 10, LIVE);
            cv.end_overlay();
            let t = mmss(ms);
            cv.text(s::HEADLINE, f.x + 28, f.y + 18, t.s(), INK);
            cv.begin_overlay();
            meter(cv, f.x + 72, f.cy(), bars);
            cv.end_overlay();
        }
    }
    let m = MIC;
    cv.note_control();
    let live = matches!(rec, Rec::On { .. });
    cv.round_rect(m.x, m.y, m.w, m.h, m.w / 2, if live { LIVE } else { ACCENT_FILL });
    cv.icon(&ic::MIC_24, m.cx() - 12, m.cy() - 12, INK);
    if focus == Some(ChatFocus::Mic) {
        s::focus(cv, m.x, m.y, m.w, m.h, m.w / 2);
    }
    if let Some(p) = problem {
        let w = s::FOOTNOTE.width(p).min(SCREEN_W - 2 * l::EDGE - 16);
        cv.icon(&ic::CROSS_12, SCREEN_W / 2 - w / 2 - 14, m.bottom() + 3, LIVE);
        cv.text_fit(s::FOOTNOTE, SCREEN_W / 2 - w / 2, m.bottom() + 13, p, SCREEN_W - 2 * l::EDGE - 16, INK_2);
    }
    screens::tab_bar(cv, l::TAB_LUCINA, focus == Some(ChatFocus::Tabs));
}

/// The bottom screen while typing a message: the field and the keyboard (Done sends).
pub fn chat_typing(cv: &mut Canvas, draft: &str, shift: bool, pressed: Option<Key>) {
    s::bottom_backdrop(cv);
    let r = FIELD_TYPING;
    cv.note_control();
    s::glass(cv, r.x, r.y, r.w, r.h, r.h / 2);
    // the end of a long draft stays in view
    let mut start = 0;
    while s::BODY.width(&draft[start..]) > r.w - 24 {
        start += draft[start..].chars().next().map_or(1, |c| c.len_utf8());
    }
    let w = cv.text(s::BODY, r.x + 12, r.y + 16, &draft[start..], INK);
    cv.begin_overlay();
    cv.fill(r.x + 12 + w + 1, r.y + 6, 1, 13, ACCENT);
    cv.end_overlay();
    s::focus(cv, r.x, r.y, r.w, r.h, r.h / 2);
    browse::keyboard(cv, shift, pressed);
}


#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::{SCREEN_H, SCREEN_W};

    #[test]
    fn a_strip_keeps_the_loudest_of_each_slice() {
        assert_eq!(bars(&[]), [2; BARS]);
        let mut w = [0u8; 64];
        w[40] = 255;
        let b = bars(&w);
        assert_eq!(b.iter().filter(|h| **h == BAR_H).count(), 1);
        assert_eq!(b[40 * BARS / 64], BAR_H);
        // fewer bytes than bars: each byte spreads over its bars
        let b = bars(&[255, 0, 0]);
        assert!(b[..BARS / 3].iter().all(|h| *h == BAR_H) && b[BARS / 3..].iter().all(|h| *h == 2));
    }

    #[test]
    fn a_screen_of_voice_notes_fits_the_display_list() {
        let wave: std::vec::Vec<u8> = (0..64).map(|i| (i * 4) as u8).collect();
        let msgs: std::vec::Vec<Msg> = (0..12)
            .map(|i| Msg { lucina: false, text: "", voice_ms: Some(1000 * i), wave: &wave, state: MsgState::Sent, hidden: false, others: 0 })
            .collect();
        let mut list = std::boxed::Box::new(crate::compose::List::new());
        let mut px = std::vec![0x8000u16; (SCREEN_W * SCREEN_H) as usize];
        {
            let mut cv = Canvas::new(&mut px, SCREEN_W, SCREEN_H, SCREEN_W);
            cv.rec = Some(&mut list);
            chat_top(&mut cv, &Bar { hh: 18, mm: 50, wifi: crate::screens::Wifi::Connected, battery: 15, charging: false }, &msgs, true);
        }
        assert!(!list.overflow, "{} ops", list.len());
        assert!(list.len() < crate::compose::MAX_OPS * 3 / 4, "{} ops: too close to the list's size", list.len());
    }
}
