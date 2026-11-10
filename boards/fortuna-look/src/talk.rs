//! Talking to Lucina on the DSi, in the Lucina tab (the coordinator's layout, 2026-11-08: one labelled tab, a "Chat |
//! Agents" control inside it): Chat (messages as cards on the chat background, the composer, the hold-to-talk
//! microphone with its meter and timer), Agents (Lucina Agents: the sessions with their state, one session's newest
//! output, a waiting question and its answers) and the sign-in by QR pairing. Drawn from plain models, like
//! `screens` and `browse`, so the host previews and tests draw the same pixels as the DSi.
//!
//! The screens are in `chat`, `agents` and `pair`; this module holds what they share (where things are, the models,
//! the "Chat | Agents" control, buttons) and re-exports them.
//!
//! Layout: the top screen holds what to read (the conversation, an agent's output and its waiting question); the
//! bottom one what to touch. No control is ever drawn on the top screen (`Canvas::note_control`, tested).
//! Copy: sentence case, verbs on buttons, states as a mark and a word, icons without labels where the owner cut them
//! (the tab bar, the microphone).

use core::fmt::Write;

use crate::canvas::Canvas;
use crate::icons::{self as ic, Icon};
use crate::layout::{self as l, Rect, SCREEN_W};
use crate::screens::Buf;
use crate::skin::{self as s, ACCENT, ACCENT_FILL, INK, INK_2, LIVE, POSITIVE, WARNING};

// ---- where things are --------------------------------------------------------------------------------------------

/// The "Chat | Agents" control at the top of the Lucina tab (not shown inside an open agent, the keyboard or the
/// sign-in).
pub const SEG_W: i32 = 150;
pub const fn seg(i: usize) -> Rect {
    let w = SEG_W / 2;
    Rect::new((SCREEN_W - SEG_W) / 2 + i as i32 * w, 4, w, l::TAP)
}
pub const SEG_NAMES: [&str; 2] = ["Chat", "Agents"];
/// Content starts under the control.
pub const BELOW_SEG: i32 = 4 + l::TAP + 6;
/// The message field (also the record bar while recording).
pub const FIELD: Rect = Rect::new(l::EDGE, BELOW_SEG, SCREEN_W - 2 * l::EDGE - l::TAP - 6, l::TAP + 4);
/// Send, beside the field (shown when there is a draft).
pub const SEND: Rect = Rect::new(SCREEN_W - l::EDGE - l::TAP - 4, BELOW_SEG, l::TAP + 4, l::TAP + 4);
/// The microphone: hold to record, let go to send.
pub const MIC: Rect = Rect::new(SCREEN_W / 2 - 30, FIELD.y + FIELD.h + 6, 60, 60);
/// The field while typing (the keyboard below it).
pub const FIELD_TYPING: Rect = Rect::new(l::EDGE, 4, SCREEN_W - 2 * l::EDGE, l::TAP);
/// Agents: rows under the control.
pub const AGENT_ROWS: i32 = (l::CONTENT_BOTTOM - BELOW_SEG) / l::ROW_H;
pub const fn agent_row(i: i32) -> Rect {
    Rect::new(l::EDGE, BELOW_SEG + i * l::ROW_H, SCREEN_W - 2 * l::EDGE, l::ROW_H)
}
/// An open agent: Back and Reply.
pub const AGENT_BACK: Rect = Rect::new(l::EDGE, l::CONTENT_BOTTOM - l::TAP - 2, 78, l::TAP);
pub const AGENT_REPLY: Rect = Rect::new(l::EDGE + 84, l::CONTENT_BOTTOM - l::TAP - 2, SCREEN_W - 2 * l::EDGE - 84, l::TAP);
/// Answer buttons of a waiting question, from the top; at most three (a fourth would sit on Back).
pub const ANSWERS: usize = 3;
pub const fn answer(i: usize) -> Rect {
    Rect::new(l::EDGE, 46 + i as i32 * (l::TAP + 4), SCREEN_W - 2 * l::EDGE, l::TAP)
}
/// Sign-in: a new code (after a denial or five unanswered ones).
pub const PAIR_AGAIN: Rect = Rect::new(SCREEN_W / 2 - 60, 116, 120, l::TAP);

// ---- models -------------------------------------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MsgState {
    Sent,
    Sending,
    Failed,
}

#[derive(Clone, Copy, Debug)]
pub struct Msg<'a> {
    pub lucina: bool,
    pub text: &'a str,
    /// A voice note and its length. A voice note draws no text, only its length and `wave`.
    pub voice_ms: Option<u32>,
    /// A voice note's waveform strip (0..=255 a byte, at most 64; empty: flat bars).
    pub wave: &'a [u8],
    pub state: MsgState,
    /// A sensitive reply: never drawn here.
    pub hidden: bool,
    /// Pictures or files with it: on the phone.
    pub others: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Rec {
    Off,
    /// Recording: the length so far and the meter's bars (0..=12).
    On { ms: u32, bars: u8 },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChatFocus {
    Seg(usize),
    Field,
    Send,
    Mic,
    Tabs,
}

/// The Agents list's focus.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ListFocus {
    Seg(usize),
    Row(i32),
    Tabs,
}

/// The "Chat | Agents" control: `active` 0 or 1, `focus` the focused half.
pub fn segment(cv: &mut Canvas, active: usize, focus: Option<usize>) {
    let all = Rect::new(seg(0).x, seg(0).y, SEG_W, l::TAP);
    s::glass(cv, all.x, all.y, all.w, all.h, all.h / 2);
    for (i, name) in SEG_NAMES.iter().enumerate() {
        cv.note_control();
        let r = seg(i);
        let ink = if i == active {
            cv.pill(r.x, r.y, r.w, r.h, ACCENT_FILL);
            INK
        } else {
            INK_2
        };
        cv.text_centre(s::HEADLINE, r.cx(), r.y + 16, name, ink);
        if focus == Some(i) {
            s::focus(cv, r.x, r.y, r.w, r.h, r.h / 2);
        }
    }
}

/// What the Agents list shows of a session.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AgentState {
    NeedsYou,
    Failed,
    Running,
    Look,
    Done,
}

impl AgentState {
    pub fn word(self) -> &'static str {
        match self {
            AgentState::NeedsYou => "Needs you",
            AgentState::Failed => "Failed",
            AgentState::Running => "Running",
            AgentState::Look => "Look",
            AgentState::Done => "Done",
        }
    }
    pub(crate) fn mark(self) -> (&'static Icon, crate::canvas::Rgba) {
        match self {
            AgentState::NeedsYou => (&ic::INFO_12, WARNING),
            AgentState::Failed => (&ic::CROSS_12, LIVE),
            AgentState::Running => (&ic::CLOCK_12, ACCENT),
            AgentState::Look => (&ic::INFO_12, ACCENT),
            AgentState::Done => (&ic::CHECK_12, POSITIVE),
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Agent<'a> {
    pub title: &'a str,
    pub provider: &'a str,
    pub state: AgentState,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LineKind {
    Said,
    Asked,
    Tool,
    Error,
}

#[derive(Clone, Copy, Debug)]
pub struct Line<'a> {
    pub kind: LineKind,
    pub text: &'a str,
}

/// A waiting request: a question with its options, or a tool or plan to allow.
#[derive(Clone, Copy, Debug)]
pub struct Waiting<'a> {
    pub question: &'a str,
    /// Empty: a permission (Deny / Allow).
    pub options: &'a [&'a str],
}

// ---- small pieces ---------------------------------------------------------------------------------------------------

pub(crate) fn button(cv: &mut Canvas, r: Rect, label: &str, primary: bool, focused: bool) {
    cv.note_control();
    if primary {
        cv.pill(r.x, r.y, r.w, r.h, ACCENT_FILL);
    } else {
        s::glass(cv, r.x, r.y, r.w, r.h, r.h / 2);
    }
    cv.text_fit(s::HEADLINE, r.cx() - s::HEADLINE.width(label).min(r.w - 12) / 2, r.y + 16, label, r.w - 12, INK);
    if focused {
        s::focus(cv, r.x, r.y, r.w, r.h, r.h / 2);
    }
}

pub(crate) fn mmss(ms: u32) -> Buf<8> {
    let mut b = Buf::<8>::new();
    let secs = ms / 1000;
    let _ = write!(b, "{}:{:02}", secs / 60, secs % 60);
    b
}

/// The 12 bars of the level meter, `lit` of them on.
pub(crate) fn meter(cv: &mut Canvas, x: i32, cy: i32, lit: u8) {
    for i in 0..12 {
        let h = 4 + (i % 6) * 2 + if i >= 6 { 2 } else { 0 };
        let c = if (i as u8) < lit { INK } else { s::HAIR };
        cv.fill(x + i * 6, cy - h / 2, 3, h, c);
    }
}

pub use crate::agents::*;
pub use crate::chat::*;
pub use crate::pair::*;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::canvas::Canvas;
    use crate::layout::{self as l, SCREEN_H, SCREEN_W};
    use crate::screens::Bar;
    use crate::canvas::Audit;
    use crate::screens::Wifi;

    const W: usize = SCREEN_W as usize;
    const H: usize = SCREEN_H as usize;

    fn bar() -> Bar {
        Bar { hh: 12, mm: 34, battery: 3, charging: false, wifi: Wifi::Connected }
    }

    fn check(name: &str, draw: impl FnOnce(&mut Canvas)) {
        check_side(name, draw);
    }

    /// Draws one screen with the audit on; returns the controls it drew.
    fn check_side(name: &str, draw: impl FnOnce(&mut Canvas)) -> u32 {
        let mut px = std::vec![0x8000u16; W * H];
        let mut a = Audit::default();
        {
            let mut cv = Canvas::new(&mut px, SCREEN_W, SCREEN_H, SCREEN_W);
            cv.audit = Some(&mut a);
            draw(&mut cv);
        }
        assert!(a.n > 0, "{name}: no text");
        assert_eq!(a.splits, 0, "{name}: a wrapped line split a word");
        for r in &a.recs[..a.n] {
            assert!(r.ratio >= r.floor(), "{name}: {:?} at {}/100, floor {}", r.text(), r.ratio, r.floor());
        }
        a.controls
    }

    #[test]
    fn no_control_on_a_top_screen_and_some_on_every_bottom() {
        let m = msgs();
        let agents = [Agent { title: "Fix the DSi build", provider: "claude", state: AgentState::NeedsYou }];
        let lines = [Line { kind: LineKind::Said, text: "Done." }];
        assert_eq!(check_side("chat top", |cv| chat_top(cv, &bar(), &m, true)), 0);
        assert_eq!(check_side("agents top", |cv| agents_top(cv, &bar(), &agents, true)), 0);
        assert_eq!(check_side("agent top", |cv| agent_top(cv, &bar(), &agents[0], &lines, Some("Which?"), true)), 0);
        assert_eq!(check_side("pair top", |cv| pair_top(cv, &bar())), 0);
        assert!(check_side("chat bottom", |cv| chat_bottom(cv, "", Rec::Off, None, None)) >= 5);
        assert!(check_side("agents bottom", |cv| agents_bottom(cv, &agents, 0, None, None)) >= 3);
        let opts = ["Now", "Later"];
        assert!(check_side("agent bottom", |cv| { agent_bottom(cv, Some(&Waiting { question: "Which?", options: &opts }), None, None); }) >= 4);
        assert!(check_side("pair idle", |cv| pair_bottom(cv, &Pair::Idle, false, false)) >= 1);
    }

    fn msgs() -> [Msg<'static>; 4] {
        [
            Msg { lucina: false, text: "What is on my list today?", voice_ms: None, wave: &[], state: MsgState::Sent, hidden: false, others: 0 },
            Msg { lucina: true, text: "Three things: the laundry, the parcel at the post office, and the shopping for Teresa.", voice_ms: None, wave: &[], state: MsgState::Sent, hidden: false, others: 0 },
            Msg { lucina: false, text: "remind me to buy milk", voice_ms: Some(4200), wave: &[20, 90, 200, 255, 140, 60, 10], state: MsgState::Sending, hidden: false, others: 0 },
            Msg { lucina: true, text: "", voice_ms: None, wave: &[], state: MsgState::Sent, hidden: true, others: 0 },
        ]
    }

    #[test]
    fn every_screen_meets_its_contrast_floor() {
        let m = msgs();
        check("chat top", |cv| chat_top(cv, &bar(), &m, true));
        check("chat top empty", |cv| chat_top(cv, &bar(), &[], false));
        check("chat bottom", |cv| chat_bottom(cv, "", Rec::Off, None, Some(ChatFocus::Mic)));
        check("chat bottom draft", |cv| chat_bottom(cv, "hello", Rec::Off, Some("Not sent: no Wi-Fi"), Some(ChatFocus::Send)));
        check("chat recording", |cv| chat_bottom(cv, "", Rec::On { ms: 7000, bars: 7 }, None, None));
        check("chat typing", |cv| chat_typing(cv, "Hello Lucina, how are you", false, None));
        let agents = [
            Agent { title: "Fix the DSi build", provider: "claude", state: AgentState::NeedsYou },
            Agent { title: "Juno Wi-Fi speed", provider: "codex", state: AgentState::Running },
            Agent { title: "Port the reader", provider: "vibe", state: AgentState::Done },
            Agent { title: "A failing one", provider: "claude", state: AgentState::Failed },
        ];
        check("agents top", |cv| agents_top(cv, &bar(), &agents, true));
        check("agents bottom", |cv| agents_bottom(cv, &agents, 0, None, Some(ListFocus::Row(1))));
        check("agents seg", |cv| agents_bottom(cv, &agents, 0, None, Some(ListFocus::Seg(0))));
        let lines = [
            Line { kind: LineKind::Asked, text: "Can you fix the build on the DSi?" },
            Line { kind: LineKind::Tool, text: "Bash · completed" },
            Line { kind: LineKind::Said, text: "The build fails at the link step: a missing symbol in the ARM7 core." },
            Line { kind: LineKind::Error, text: "Turn failed" },
        ];
        check("agent top", |cv| agent_top(cv, &bar(), &agents[0], &lines, Some("Which rollout should we use?"), true));
        let opts = ["Now", "Behind a flag"];
        check("agent question", |cv| {
            agent_bottom(cv, Some(&Waiting { question: "Which rollout should we use?", options: &opts }), None, Some(AgentFocus::Answer(0)));
        });
        check("agent permission", |cv| {
            agent_bottom(cv, Some(&Waiting { question: "Run cargo test?", options: &[] }), None, None);
        });
        let modules = std::vec![true; 33 * 33];
        check("pair top", |cv| pair_top(cv, &bar()));
        check("pair ready", |cv| pair_bottom(cv, &Pair::Ready { modules: &modules, width: 33, code: "123456", left_s: 98 }, false, false));
        check("pair denied", |cv| pair_bottom(cv, &Pair::Denied, true, false));
        check("pair idle", |cv| pair_bottom(cv, &Pair::Idle, false, true));
        check("pair clock", |cv| pair_bottom(cv, &Pair::ClockUnset, false, false));
    }

    /// A real approve link (the gateway's codes are 43 characters) as the QR code Fortuna draws.
    fn real_qr() -> (std::vec::Vec<bool>, usize) {
        use qrcodegen::{QrCode, QrCodeEcc};
        let url = "https://getsecretary.tech/approve?c=AbCdEfGhIjKlMnOpQrStUvWxYz0123456789-_AbCdEfG";
        let q = QrCode::encode_text(url, QrCodeEcc::Medium).unwrap();
        let w = q.size() as usize;
        ((0..w * w).map(|i| q.get_module((i % w) as i32, (i / w) as i32)).collect(), w)
    }

    #[test]
    fn the_qr_rectangles_cover_exactly_the_dark_modules() {
        let (m, w) = real_qr();
        let mut grid = std::vec![false; w * w];
        let mut n = 0;
        crate::pair::qr_rects(&m, w, |x, y, rw, rh| {
            n += 1;
            for yy in y..y + rh {
                for xx in x..x + rw {
                    let i = yy as usize * w + xx as usize;
                    assert!(!grid[i], "covered twice at {xx},{yy}");
                    grid[i] = true;
                }
            }
        });
        assert_eq!(grid, m);
        assert!(n < 300, "{n} rectangles for a {w}x{w} code");
    }

    #[test]
    fn the_pairing_screen_fits_the_display_list() {
        // recorded as the kernel records it: a list that overflows drops the rest of the screen
        let (m, w) = real_qr();
        let mut list = std::boxed::Box::new(crate::compose::List::new());
        let mut px = std::vec![0x8000u16; W * H];
        {
            let mut cv = Canvas::new(&mut px, SCREEN_W, SCREEN_H, SCREEN_W);
            cv.rec = Some(&mut list);
            pair_bottom(&mut cv, &Pair::Ready { modules: &m, width: w, code: "123456", left_s: 99 }, false, false);
        }
        assert!(!list.overflow, "{} ops for a {w}x{w} code", list.len());
        assert!(list.len() < crate::compose::MAX_OPS * 3 / 4, "{} ops: too close to the list's size", list.len());
    }

    #[test]
    fn every_control_meets_the_one_minimum() {
        for r in [FIELD, SEND, MIC, FIELD_TYPING, AGENT_BACK, AGENT_REPLY, PAIR_AGAIN, answer(0), answer(ANSWERS - 1), seg(0), seg(1), agent_row(AGENT_ROWS - 1)] {
            assert!(r.w >= l::TAP && r.h >= l::TAP, "{r:?}");
            assert!(r.bottom() <= l::CONTENT_BOTTOM || r == FIELD_TYPING, "{r:?} under the tab bar");
        }
        assert!(answer(ANSWERS - 1).bottom() <= AGENT_BACK.y, "answers above Back");
        for i in 0..l::TABS {
            let t = l::tab(i);
            assert!(t.w >= l::TAP && t.h >= l::TAP);
        }
    }
}
