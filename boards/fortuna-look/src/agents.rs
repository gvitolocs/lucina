//! Lucina Agents: the sessions with their state (bottom), one session's newest output and its waiting question (top),
//! the answers, Back and Reply (bottom).

use core::fmt::Write;

use crate::canvas::Canvas;
use crate::icons as ic;
use crate::layout::{self as l, Rect, SCREEN_H, SCREEN_W};
use crate::screens::{self, Bar, Buf};
use crate::skin::{self as s, ACCENT_FILL, INK, INK_2, LIVE, WARNING};
use crate::talk::*;

/// The top screen of Agents with none open: how many sessions are in each state.
pub fn agents_top(cv: &mut Canvas, bar: &Bar, agents: &[Agent], loaded: bool) {
    s::top_backdrop(cv);
    screens::strip(cv, bar);
    cv.text(s::DISPLAY, l::EDGE + 4, 56, "Agents", INK);
    if !loaded {
        cv.text(s::FOOTNOTE, l::EDGE + 4, 76, "Loading", INK_2);
        return;
    }
    if agents.is_empty() {
        cv.text(s::FOOTNOTE, l::EDGE + 4, 76, "No sessions", INK_2);
        return;
    }
    let mut y = 92;
    for st in [AgentState::NeedsYou, AgentState::Failed, AgentState::Running, AgentState::Look, AgentState::Done] {
        let n = agents.iter().filter(|a| a.state == st).count();
        if n == 0 {
            continue;
        }
        let (icon, col) = st.mark();
        s::material(cv, l::EDGE, y - 16, SCREEN_W - 2 * l::EDGE, 24, 8);
        cv.icon(icon, l::EDGE + 8, y - 10, col);
        cv.text(s::BODY, l::EDGE + 26, y, st.word(), INK);
        let mut b = Buf::<8>::new();
        let _ = write!(b, "{n}");
        cv.text_right(s::HEADLINE, SCREEN_W - l::EDGE - 10, y, b.s(), INK);
        y += 28;
        if y > SCREEN_H - 8 {
            break;
        }
    }
}

/// The list of sessions on the bottom screen, `first` the first shown.
pub fn agents_bottom(cv: &mut Canvas, agents: &[Agent], first: usize, problem: Option<&str>, focus: Option<ListFocus>) {
    s::bottom_backdrop(cv);
    segment(cv, 1, if let Some(ListFocus::Seg(i)) = focus { Some(i) } else { None });
    let shown = &agents[first.min(agents.len())..];
    let n = (shown.len() as i32).min(AGENT_ROWS);
    if n == 0 {
        cv.text_centre(s::BODY, SCREEN_W / 2, 80, problem.unwrap_or("No sessions"), INK_2);
    } else {
        let top = agent_row(0);
        s::material(cv, top.x, top.y, top.w, n * l::ROW_H, l::PANEL_R);
        for (i, a) in shown.iter().take(n as usize).enumerate() {
            cv.note_control();
            let b = agent_row(i as i32);
            if i > 0 {
                cv.fill(b.x + 8, b.y, b.w - 16, 1, s::HAIR);
            }
            let (icon, col) = a.state.mark();
            let word = a.state.word();
            let ww = s::FOOTNOTE.width(word);
            cv.text_right(s::FOOTNOTE, b.right() - 22, b.y + 16, word, INK_2);
            cv.icon(icon, b.right() - 22 - ww - 15, b.y + 6, col);
            cv.text_fit(s::BODY, b.x + 10, b.y + 16, a.title, b.w - 10 - 22 - ww - 22, INK);
            cv.icon(&ic::NEXT_12, b.right() - 16, b.y + 6, INK_2);
        }
        if let Some(ListFocus::Row(f)) = focus.filter(|f| matches!(f, ListFocus::Row(r) if (0..n).contains(r))) {
            let b = agent_row(f);
            s::focus(cv, b.x + 1, b.y + 1, b.w - 2, b.h - 2, 6);
        }
    }
    screens::tab_bar(cv, l::TAB_LUCINA, focus == Some(ListFocus::Tabs));
}

/// One session open, top screen: its title and state, the question it waits on (when it does), then its newest lines
/// (what it said, its tools, its errors). The answers are on the bottom screen.
pub fn agent_top(cv: &mut Canvas, bar: &Bar, a: &Agent, lines: &[Line], waiting: Option<&str>, loaded: bool) {
    s::top_backdrop(cv);
    screens::strip(cv, bar);
    let (icon, col) = a.state.mark();
    cv.text_fit(s::TITLE, l::EDGE + 4, 38, a.title, SCREEN_W - 2 * l::EDGE - 90, INK);
    let ww = s::FOOTNOTE.width(a.state.word());
    cv.text_right(s::FOOTNOTE, SCREEN_W - l::EDGE - 4, 36, a.state.word(), INK_2);
    cv.icon(icon, SCREEN_W - l::EDGE - 4 - ww - 15, 26, col);
    if !loaded {
        cv.text(s::FOOTNOTE, l::EDGE + 4, 60, "Loading", INK_2);
        return;
    }
    // the question, under the title, in the accent's soft card
    let mut floor = 46;
    if let Some(q) = waiting {
        let lines_n = 2;
        let h = 12 + lines_n * LINE_STEP;
        s::material(cv, l::EDGE, 46, SCREEN_W - 2 * l::EDGE, h, 10);
        cv.icon(&ic::INFO_12, l::EDGE + 6, 50, WARNING);
        cv.text_wrap(s::HEADLINE, l::EDGE + 22, 60, q, SCREEN_W - 2 * l::EDGE - 30, lines_n, LINE_STEP, INK);
        floor = 46 + h + 4;
    }
    // the newest at the bottom; what it said gets a card, tools and errors one line each
    let mut y = SCREEN_H - 4;
    for ln in lines.iter().rev() {
        match ln.kind {
            LineKind::Said | LineKind::Asked => {
                let mine = ln.kind == LineKind::Asked;
                let width = if mine { 200 } else { SCREEN_W - 2 * l::EDGE };
                let m = Msg { lucina: !mine, text: ln.text, voice_ms: None, wave: &[], state: MsgState::Sent, hidden: false, others: 0 };
                let lines_n = card_lines(&m, false).min(3);
                let _ = width;
                let h = 10 + lines_n * LINE_STEP;
                if y - h < floor {
                    break;
                }
                y -= h;
                let (x, w) = if mine { (SCREEN_W - l::EDGE - 200, 200) } else { (l::EDGE, SCREEN_W - 2 * l::EDGE) };
                if mine {
                    cv.round_rect(x, y, w, h, 10, ACCENT_FILL);
                } else {
                    s::material(cv, x, y, w, h, 10);
                }
                cv.text_wrap(s::BODY, x + 8, y + 15, ln.text, w - 16, lines_n, LINE_STEP, INK);
            }
            LineKind::Tool | LineKind::Error => {
                if y - 16 < floor {
                    break;
                }
                y -= 16;
                let (icon, col) = if ln.kind == LineKind::Tool { (&ic::AGENTS_12, INK_2) } else { (&ic::CROSS_12, LIVE) };
                cv.icon(icon, l::EDGE + 6, y + 1, col);
                cv.text_fit(s::FOOTNOTE, l::EDGE + 22, y + 11, ln.text, SCREEN_W - 2 * l::EDGE - 26, INK_2);
            }
        }
        y -= 4;
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AgentFocus {
    Answer(usize),
    Back,
    Reply,
    Tabs,
}

/// One session open, bottom screen: the answers to its waiting question (or Deny / Allow; the question is on the top
/// screen), Back and Reply.
pub fn agent_bottom(cv: &mut Canvas, waiting: Option<&Waiting>, problem: Option<&str>, focus: Option<AgentFocus>) -> usize {
    s::bottom_backdrop(cv);
    let mut buttons = 0;
    match waiting {
        Some(w) => {
            if w.options.is_empty() {
                let half = (SCREEN_W - 2 * l::EDGE - 6) / 2;
                let deny = Rect::new(l::EDGE, answer(0).y, half, l::TAP);
                let allow = Rect::new(l::EDGE + half + 6, answer(0).y, half, l::TAP);
                button(cv, deny, "Deny", false, focus == Some(AgentFocus::Answer(0)));
                button(cv, allow, "Allow", true, focus == Some(AgentFocus::Answer(1)));
                buttons = 2;
            } else {
                for (i, o) in w.options.iter().take(ANSWERS).enumerate() {
                    button(cv, answer(i), o, i == 0, focus == Some(AgentFocus::Answer(i)));
                    buttons += 1;
                }
            }
        }
        None => {
            if let Some(p) = problem {
                cv.text_centre(s::FOOTNOTE, SCREEN_W / 2, 60, p, INK_2);
            }
        }
    }
    button(cv, AGENT_BACK, "Back", false, focus == Some(AgentFocus::Back));
    let r = AGENT_REPLY;
    cv.note_control();
    s::glass(cv, r.x, r.y, r.w, r.h, r.h / 2);
    cv.text(s::BODY, r.x + 12, r.y + 16, "Reply", INK_2);
    if focus == Some(AgentFocus::Reply) {
        s::focus(cv, r.x, r.y, r.w, r.h, r.h / 2);
    }
    screens::tab_bar(cv, l::TAB_LUCINA, focus == Some(AgentFocus::Tabs));
    buttons
}

/// Where the answer buttons of `agent_bottom` are, for touch and the d-pad.
pub fn answer_rects(waiting: &Waiting, out: &mut [Rect; ANSWERS]) -> usize {
    if waiting.options.is_empty() {
        let half = (SCREEN_W - 2 * l::EDGE - 6) / 2;
        out[0] = Rect::new(l::EDGE, answer(0).y, half, l::TAP);
        out[1] = Rect::new(l::EDGE + half + 6, answer(0).y, half, l::TAP);
        return 2;
    }
    let n = waiting.options.len().min(ANSWERS);
    for (i, o) in out.iter_mut().take(n).enumerate() {
        *o = answer(i);
    }
    n
}

