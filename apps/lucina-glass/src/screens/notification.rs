//! Screen 3, "Notification", on the black page: a left rail (NOW pill with its count, the vertical slider, two toggles),
//! two stat cards (one fact each), the thin title, the stacked notification card, pager dots and two actions. No
//! taglines and no illustrations (docs/design/rules.md, rules 3 and 8).

use crate::anim::{Ease, Secs, phase};
use crate::canvas::{Canvas, R};
use crate::icons::Icon;
use crate::text::{Font, Type};
use crate::tokens::{color, space};
use crate::widgets::card_stack::Card;
use crate::widgets::chrome::{self, VSlider};
use crate::widgets::{ActionButton, CardStack, LivePill, PagerDots, RoundButton};
use crate::Paint;

use super::{Pointer, Screen, content as c, home_indicator};

pub struct Notification {
    now_pill: LivePill,
    slider: VSlider,
    round: [RoundButton; 2],
    pub stack: CardStack,
    pub pager: PagerDots,
    pub actions: [ActionButton; 2],
    /// When the screen came in: the title rises in.
    entered: Secs,
    drag: Option<(f32, f32, Secs)>,
    pressed: Option<usize>,
}

/// The NOW pill's label with its count: the pill says what it counts (rule 2).
fn now_label() -> String {
    format!("{} {}", c::NOW, c::PENDING)
}

impl Notification {
    pub fn new(now: Secs) -> Notification {
        Notification {
            now_pill: LivePill::new(&now_label(), now),
            slider: VSlider::new(5, 1),
            round: [RoundButton::new(Icon::Bolt), RoundButton::new(Icon::Link)],
            stack: CardStack::new(c::CARDS.iter().map(|&(f, s)| Card { fact: f.into(), secondary: s.into(), urgent: true }).collect()),
            pager: PagerDots::new(c::PAGES, c::PAGE),
            actions: [ActionButton::new(Icon::Upload, c::NOTIFY_ACTIONS[0]), ActionButton::new(Icon::Clock, c::NOTIFY_ACTIONS[1])],
            entered: now,
            drag: None,
            pressed: None,
        }
    }
}

impl Screen for Notification {
    fn name(&self) -> &'static str {
        "notification"
    }

    fn enter(&mut self, now: Secs) {
        self.entered = now;
        self.now_pill.since = now;
    }

    fn act(&mut self, now: Secs, action: &str) -> bool {
        if let Some(n) = action.strip_prefix("page-") {
            if let Ok(i) = n.parse::<usize>() {
                self.pager.go(now, i.saturating_sub(1));
                return true;
            }
            return false;
        }
        match action {
            "swipe" => self.stack.throw(now, -1.0, -400.0),
            "swipe-right" => self.stack.throw(now, 1.0, 400.0),
            "slider-down" => self.slider.step(now, 1),
            "slider-up" => self.slider.step(now, -1),
            "press-upload" => self.actions[0].press(now),
            "release-upload" => self.actions[0].release(now),
            "hover-upload" => self.actions[0].set_hover(now, true),
            _ => return false,
        }
        true
    }

    fn pointer(&mut self, now: Secs, ev: Pointer) -> bool {
        match ev {
            Pointer::Down(x, y) => {
                if let Some(d) = self.slider.hit(x, y) {
                    self.slider.step(now, d);
                    return true;
                }
                if let Some(i) = self.pager.hit(x, y) {
                    self.pager.go(now, i);
                    return true;
                }
                if self.stack.hit(x, y) {
                    self.drag = Some((x, x, now));
                    return true;
                }
                if let Some(i) = self.actions.iter().position(|b| b.hit(x, y)) {
                    self.actions[i].press(now);
                    self.pressed = Some(i);
                    return true;
                }
                if let Some(i) = self.round.iter().position(|b| b.hit(x, y)) {
                    self.round[i].press(now);
                    self.pressed = Some(10 + i);
                    return true;
                }
                false
            }
            Pointer::Move(x, y) => {
                for b in &mut self.actions {
                    let on = b.hit(x, y);
                    b.set_hover(now, on);
                }
                if let Some((x0, _, _)) = self.drag {
                    self.stack.drag(now, x - x0);
                    self.drag = Some((x0, x, now));
                }
                true
            }
            Pointer::Up(x, _) => {
                if let Some((x0, last, t)) = self.drag.take() {
                    let vx = if now > t { (x - last) / (now - t) as f32 } else { 0.0 };
                    self.stack.release(now, x - x0, vx);
                }
                match self.pressed.take() {
                    Some(i) if i < 10 => self.actions[i].release(now),
                    Some(i) => self.round[i - 10].release(now),
                    None => {}
                }
                true
            }
        }
    }

    fn draw(&mut self, cv: &mut Canvas, ty: &mut Type, now: Secs) {
        cv.clear(color::VOID);
        let full = cv.bounds();
        let mut p = Paint { cv, ty, now, backdrop: None };
        chrome::status_bar(&mut p, R::new(0.0, 0.0, full.w, 46.0), color::INK);

        // The rail.
        let rail_x = space::EDGE;
        self.now_pill.draw(&mut p, rail_x, 58.0);
        self.slider.draw(&mut p, R::new(rail_x + 2.0, 92.0, 48.0, 206.0));
        self.round[0].draw(&mut p, rail_x + 26.0, 330.0, 22.0);
        self.round[1].draw(&mut p, rail_x + 26.0, 382.0, 22.0);

        // Two stat cards: one fact each.
        let col = R::new(84.0, 58.0, full.w - 84.0 - space::EDGE, 300.0);
        let top = R::new(col.x, col.y, col.w, 144.0);
        let below = R::new(col.x, col.y + 144.0 + space::M, col.w, col.h - 144.0 - space::M);
        chrome::stat_tile(&mut p, top, Icon::Person, c::ACTIVITY.0, c::ACTIVITY.1);
        chrome::stat_tile(&mut p, below, Icon::Send, c::TEAMS.0, c::TEAMS.1);

        // The title rises in.
        let k = Ease::OutCubic.at(phase(now, self.entered + 0.1, 0.6));
        let o = p.cv.opacity;
        p.cv.opacity = o * k;
        p.text(Font::Thin, (60.0, -1.8), space::EDGE - 4.0, 486.0 + 18.0 * (1.0 - k), color::INK, c::NOTIFY_TITLE);
        p.cv.opacity = o;

        // The stack, pager and actions.
        self.stack.draw(&mut p, R::new(space::EDGE, 512.0, full.w - 2.0 * space::EDGE, 96.0));
        self.pager.draw(&mut p, space::EDGE, 690.0);
        let (aw, _) = ActionButton::size();
        self.actions[0].draw(&mut p, full.right() - space::EDGE - aw * 1.5 - space::S, 660.0);
        self.actions[1].draw(&mut p, full.right() - space::EDGE - aw / 2.0, 660.0);
        home_indicator(p.cv, color::INK);
    }
}
