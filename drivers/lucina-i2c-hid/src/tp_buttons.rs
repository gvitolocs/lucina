//! libinput 1.28.1's clickpad software buttons (`src/evdev-mt-touchpad-buttons.c`), ported to no_std.
//!
//! Only the bottom software-button area is ported: this pad has no trackpoint, so
//! `tp->buttons.has_topbuttons = false` and `tp_init_top_softbuttons` sets `top_area.bottom_edge = INT_MIN`,
//! which makes every top-area test false and the `BUTTON_STATE_TOP*`/`IGNORE` states unreachable. Clickfinger is
//! not this pad's default click method (`tp_click_get_default_method` returns BUTTON_AREAS for a clickpad without
//! the listed quirks), and clickpad middle-button emulation is disabled by default, so both are left out; the
//! middle *software area* is still initialized, as `tp_init_softbuttons` does while emulation is off.

/// `tp->ntouches` for a precision touchpad: ten slots.
pub const MAX_TOUCHES: usize = 10;

/// `DEFAULT_BUTTON_ENTER_TIMEOUT`, ms2us(100). Only used by the top-button states, which are unreachable here.
#[allow(dead_code)]
const BUTTON_ENTER_TIMEOUT_US: u64 = 100_000;
/// `DEFAULT_BUTTON_LEAVE_TIMEOUT`, ms2us(300). Only used by the top-button states, which are unreachable here.
#[allow(dead_code)]
const BUTTON_LEAVE_TIMEOUT_US: u64 = 300_000;
/// ms2us(80) of `tp_button_release_other_bottom_touches`.
const RELEASE_OTHER_TIMEOUT_US: u64 = 80_000;
/// `tp_button_check_for_movement`: moved more than 5 mm, compared squared (core has no `hypot`).
const MOVEMENT_THRESHOLD_MM_SQ: f64 = 25.0;

/// A software button libinput would post.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ClickButton {
    Left,
    Right,
    Middle,
}

/// What happened to a touch in this frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ButtonPhase {
    Begin,
    Update,
    End,
}

/// A touch that changed in this frame, in device units.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ButtonTouch {
    pub slot: u8,
    pub phase: ButtonPhase,
    pub x: i32,
    pub y: i32,
}

/// `enum button_event`, minus the top-area events (unreachable without top buttons).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ButtonEvent {
    InBottomR,
    InBottomM,
    InBottomL,
    InArea,
    Up,
    Press,
    Release,
    Timeout,
}

/// `enum button_state`, minus the top states (unreachable without top buttons).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ButtonState {
    None,
    Area,
    Bottom,
}

/// `struct tp_touch`'s `button` member.
#[derive(Clone, Copy)]
struct TouchButton {
    state: ButtonState,
    /// `button.current`; `None` is libinput's `current = 0`.
    current: Option<ButtonEvent>,
    /// `button.timer`'s deadline, None when cancelled.
    deadline: Option<u64>,
    initial: (i32, i32),
    has_moved: bool,
    initial_time: u64,
}

impl TouchButton {
    const fn new() -> Self {
        TouchButton {
            state: ButtonState::None,
            current: None,
            deadline: None,
            initial: (0, 0),
            has_moved: false,
            initial_time: 0,
        }
    }
}

/// The clickpad's software buttons: `tp->buttons` plus the per-touch `button` state.
pub struct SoftButtons {
    touches: [TouchButton; MAX_TOUCHES],
    /// `buttons.bottom_area.top_edge`, device units.
    top_edge: i32,
    /// `buttons.bottom_area.rightbutton_left_edge`, device units.
    rightbutton_left_edge: i32,
    /// `buttons.bottom_area.middlebutton_left_edge`, device units.
    middlebutton_left_edge: i32,
    /// Axis resolutions in units/mm, for the movement threshold.
    res_x: i32,
    res_y: i32,
    /// `buttons.state` (bit 0: the one physical button, BTN_LEFT).
    state: u32,
    /// `buttons.old_state`.
    old_state: u32,
    /// `buttons.click_pending`.
    click_pending: bool,
    /// `buttons.active`, the button a press posted, for the release.
    active: Option<ClickButton>,
    /// `queued & TOUCHPAD_EVENT_BUTTON_RELEASE` / `..._PRESS`.
    queued_release: bool,
    queued_press: bool,
}

/// `is_inside_bottom_button_area`.
fn in_bottom_area(top_edge: i32, y: i32) -> bool {
    y >= top_edge
}

/// `is_inside_bottom_right_area`.
fn in_bottom_right_area(top_edge: i32, rightbutton_left_edge: i32, x: i32, y: i32) -> bool {
    in_bottom_area(top_edge, y) && x > rightbutton_left_edge
}

/// `is_inside_bottom_middle_area`.
fn in_bottom_middle_area(
    top_edge: i32,
    rightbutton_left_edge: i32,
    middlebutton_left_edge: i32,
    x: i32,
    y: i32,
) -> bool {
    in_bottom_area(top_edge, y)
        && !in_bottom_right_area(top_edge, rightbutton_left_edge, x, y)
        && x > middlebutton_left_edge
}

impl SoftButtons {
    /// `tp_init_softbuttons` for a pad of this range and resolution (units per mm). Middle-button emulation is
    /// off, so the middle software area is initialized (the `else` branch: no visible-marker quirk on this pad).
    pub fn new(x_max: i32, y_max: i32, res_x: i32, res_y: i32) -> SoftButtons {
        // `evdev_device_get_size`: absinfo_convert_to_mm(maximum) = maximum / resolution.
        let width = x_max as f64 / res_x as f64;
        let height = y_max as f64 / res_y as f64;

        // `evdev_device_mm_to_units`: mm * resolution + minimum (minimum is 0), truncated as a C cast does.
        let mm_to_units_x = |mm: f64| (mm * res_x as f64) as i32;
        let mm_to_units_y = |mm: f64| (mm * res_y as f64) as i32;

        // Button height: 10mm or 15% of the height, whichever is smaller.
        let top_mm = if height * 0.15 > 10.0 {
            height - 10.0
        } else {
            height * 0.85
        };
        let top_edge = mm_to_units_y(top_mm);
        // tp_init_softbuttons first sets rightbutton_left_edge to width/2, but with middle-button
        // emulation off it is overwritten by the middle area's right edge; the middle button is 25%
        // of the width, centered.
        let middlebutton_left_edge = mm_to_units_x(width * 0.375);
        let rightbutton_left_edge = mm_to_units_x(width * 0.625);

        SoftButtons {
            touches: [TouchButton::new(); MAX_TOUCHES],
            top_edge,
            rightbutton_left_edge,
            middlebutton_left_edge,
            res_x,
            res_y,
            state: 0,
            old_state: 0,
            click_pending: false,
            active: None,
            queued_release: false,
            queued_press: false,
        }
    }

    /// `tp_button_handle_state` for one frame's dirty touches.
    pub fn handle_frame(&mut self, now_us: u64, touches: &[ButtonTouch]) {
        for t in touches {
            let Some(index) = (t.slot as usize).checked_sub(0).filter(|&i| i < MAX_TOUCHES) else {
                continue;
            };
            match t.phase {
                ButtonPhase::Begin => {
                    self.touches[index].initial = (t.x, t.y);
                    self.touches[index].initial_time = now_us;
                    self.touches[index].has_moved = false;
                    let event = self.event_for(t.x, t.y);
                    self.check_for_movement(index, t.x, t.y);
                    self.handle_event(index, event, now_us);
                }
                ButtonPhase::Update => {
                    let event = self.event_for(t.x, t.y);
                    self.check_for_movement(index, t.x, t.y);
                    self.handle_event(index, event, now_us);
                }
                ButtonPhase::End => {
                    self.handle_event(index, ButtonEvent::Up, now_us);
                }
            }
        }
        // The queued button events go to every touch, as tp_button_handle_state does.
        if self.queued_release {
            for index in 0..MAX_TOUCHES {
                self.handle_event(index, ButtonEvent::Release, now_us);
            }
        }
        if self.queued_press {
            for index in 0..MAX_TOUCHES {
                self.handle_event(index, ButtonEvent::Press, now_us);
            }
        }
        self.queued_release = false;
        self.queued_press = false;
    }

    /// `tp_process_button` (a clickpad only has BTN_LEFT) plus `tp_post_clickpadbutton_buttons`:
    /// the physical button changed in this frame. Calls `out` with the button events libinput posts and returns
    /// libinput's return value.
    pub fn click(&mut self, now_us: u64, pressed: bool, out: &mut dyn FnMut(ClickButton, bool)) -> bool {
        let _ = now_us;
        if pressed {
            self.state |= 1;
            self.queued_press = true;
        } else {
            self.state &= !1;
            self.queued_release = true;
        }

        let current = self.state;
        let old = self.old_state;

        let mut posted = false;
        if !self.click_pending && current == old {
            posted = false;
        } else if current != 0 {
            // The area bits of tp_post_clickpadbutton_buttons.
            const AREA: u8 = 0x01;
            const LEFT: u8 = 0x02;
            const MIDDLE: u8 = 0x04;
            const RIGHT: u8 = 0x08;
            let mut area = 0u8;
            for t in &self.touches {
                match t.current {
                    Some(ButtonEvent::InArea) => area |= AREA,
                    Some(ButtonEvent::InBottomL) => area |= LEFT,
                    Some(ButtonEvent::InBottomM) => area |= MIDDLE,
                    Some(ButtonEvent::InBottomR) => area |= RIGHT,
                    _ => {}
                }
            }
            if area == 0 {
                // No touches, wait for a touch before processing.
                self.click_pending = true;
                self.old_state = current;
                return false;
            }
            // Middle-button emulation is off and there are no top buttons, so the
            // `(middlebutton.enabled || is_top) && LEFT && RIGHT` branch is out.
            let button = if area & MIDDLE != 0 {
                ClickButton::Middle
            } else if area & RIGHT != 0 {
                ClickButton::Right
            } else {
                // LEFT, or AREA only, is always left.
                ClickButton::Left
            };
            self.active = Some(button);
            out(button, true);
            posted = true;
        } else {
            if let Some(button) = self.active.take() {
                out(button, false);
                posted = true;
            }
        }
        self.click_pending = false;
        self.old_state = current;
        posted
    }

    /// `tp_button_touch_active`: whether this slot's touch counts for pointer motion and gestures.
    pub fn touch_active(&self, slot: u8) -> bool {
        match self.touches.get(slot as usize) {
            Some(t) => t.state == ButtonState::Area || t.has_moved,
            None => false,
        }
    }

    /// The earliest pending button timer deadline, if any.
    pub fn deadline(&self) -> Option<u64> {
        self.touches.iter().filter_map(|t| t.deadline).min()
    }

    /// `tp_button_handle_timeout`: the button timer fired.
    pub fn timeout(&mut self, now_us: u64) {
        for index in 0..MAX_TOUCHES {
            if self.touches[index].deadline.is_some_and(|d| d <= now_us) {
                self.handle_event(index, ButtonEvent::Timeout, now_us);
            }
        }
    }

    /// The right and middle areas in device units: (top y of the bottom area, left x of the right area,
    /// middle area x range).
    pub fn areas(&self) -> (i32, i32, Option<(i32, i32)>) {
        (
            self.top_edge,
            self.rightbutton_left_edge,
            Some((self.middlebutton_left_edge, self.rightbutton_left_edge)),
        )
    }

    /// The `button_event` for a position, as `tp_button_handle_state` computes it. Top-area events cannot occur:
    /// `top_area.bottom_edge` is INT_MIN without top buttons.
    fn event_for(&self, x: i32, y: i32) -> ButtonEvent {
        if in_bottom_area(self.top_edge, y) {
            if in_bottom_right_area(self.top_edge, self.rightbutton_left_edge, x, y) {
                ButtonEvent::InBottomR
            } else if in_bottom_middle_area(
                self.top_edge,
                self.rightbutton_left_edge,
                self.middlebutton_left_edge,
                x,
                y,
            ) {
                ButtonEvent::InBottomM
            } else {
                ButtonEvent::InBottomL
            }
        } else {
            ButtonEvent::InArea
        }
    }

    /// `tp_button_check_for_movement`.
    fn check_for_movement(&mut self, index: usize, x: i32, y: i32) {
        let t = &self.touches[index];
        if t.has_moved || t.state != ButtonState::Bottom {
            return;
        }
        let dx = x - t.initial.0;
        let dy = y - t.initial.1;
        // `evdev_device_unit_delta_to_mm`: units / resolution.
        let mm_x = dx as f64 / self.res_x as f64;
        let mm_y = dy as f64 / self.res_y as f64;
        if mm_x * mm_x + mm_y * mm_y > MOVEMENT_THRESHOLD_MM_SQ {
            self.touches[index].has_moved = true;
            let initial_time = self.touches[index].initial_time;
            self.release_other_bottom_touches(initial_time);
        }
    }

    /// `tp_button_release_other_bottom_touches`.
    fn release_other_bottom_touches(&mut self, other_start_time: u64) {
        for t in &mut self.touches {
            if t.state != ButtonState::Bottom || t.has_moved {
                continue;
            }
            let tdelta = t.initial_time.abs_diff(other_start_time);
            if tdelta > RELEASE_OTHER_TIMEOUT_US {
                continue;
            }
            t.has_moved = true;
        }
    }

    /// `tp_button_set_state`: change state, cancel the timer, and the on-entry behavior.
    fn set_state(&mut self, index: usize, new_state: ButtonState, event: ButtonEvent, _time: u64) {
        let t = &mut self.touches[index];
        t.deadline = None;
        t.state = new_state;
        t.current = match new_state {
            ButtonState::None => None,
            ButtonState::Area => Some(ButtonEvent::InArea),
            ButtonState::Bottom => Some(event),
        };
    }

    /// `tp_button_handle_event` for the reachable states.
    fn handle_event(&mut self, index: usize, event: ButtonEvent, time: u64) {
        match self.touches[index].state {
            ButtonState::None => match event {
                ButtonEvent::InBottomR | ButtonEvent::InBottomM | ButtonEvent::InBottomL => {
                    self.set_state(index, ButtonState::Bottom, event, time);
                }
                ButtonEvent::InArea => {
                    self.set_state(index, ButtonState::Area, event, time);
                }
                ButtonEvent::Up => {
                    self.set_state(index, ButtonState::None, event, time);
                }
                ButtonEvent::Press | ButtonEvent::Release | ButtonEvent::Timeout => {}
            },
            ButtonState::Area => {
                if event == ButtonEvent::Up {
                    self.set_state(index, ButtonState::None, event, time);
                }
            }
            ButtonState::Bottom => match event {
                ButtonEvent::InBottomR | ButtonEvent::InBottomM | ButtonEvent::InBottomL => {
                    if Some(event) != self.touches[index].current {
                        self.set_state(index, ButtonState::Bottom, event, time);
                    }
                }
                ButtonEvent::InArea => {
                    self.set_state(index, ButtonState::Area, event, time);
                    // One finger went from BOTTOM to AREA: release other fingers in BOTTOM
                    // that started simultaneously with it, they're part of a gesture.
                    let initial_time = self.touches[index].initial_time;
                    self.release_other_bottom_touches(initial_time);
                }
                ButtonEvent::Up => {
                    self.set_state(index, ButtonState::None, event, time);
                }
                ButtonEvent::Press | ButtonEvent::Release | ButtonEvent::Timeout => {}
            },
        }
    }
}

#[cfg(test)]
mod tests {
    extern crate std;

    use super::*;
    use std::vec::Vec;

    /// The Galaxy Book Go touchpad: X 0..1876, Y 0..1164, 16 units/mm.
    fn pad() -> SoftButtons {
        SoftButtons::new(1876, 1164, 16, 16)
    }

    fn touch(slot: u8, phase: ButtonPhase, x: i32, y: i32) -> ButtonTouch {
        ButtonTouch { slot, phase, x, y }
    }

    /// Runs `click` and collects the posted (button, pressed) pairs.
    fn click(sb: &mut SoftButtons, now_us: u64, pressed: bool) -> (bool, Vec<(ClickButton, bool)>) {
        let mut events = Vec::new();
        let posted = sb.click(now_us, pressed, &mut |b, p| events.push((b, p)));
        (posted, events)
    }

    #[test]
    fn areas_match_libinput() {
        // width 117.25mm, height 72.75mm: top edge at 62.75mm = 1004 units,
        // middle area 43.96875..73.28125mm = 703..1172 units, right area above 1172.
        let sb = pad();
        assert_eq!(sb.areas(), (1004, 1172, Some((703, 1172))));
    }

    /// Mirrors libinput's clickpad_softbutton_right.
    #[test]
    fn clickpad_softbutton_right() {
        let mut sb = pad();
        sb.handle_frame(0, &[touch(0, ButtonPhase::Begin, 1500, 1100)]);
        let (posted, events) = click(&mut sb, 0, true);
        assert!(posted);
        assert_eq!(events, [(ClickButton::Right, true)]);
        let (posted, events) = click(&mut sb, 1000, false);
        assert!(posted);
        assert_eq!(events, [(ClickButton::Right, false)]);
    }

    /// Mirrors libinput's clickpad_softbutton_left, plus a click with the finger above the area
    /// (which libinput also posts as left: the else branch of tp_post_clickpadbutton_buttons).
    #[test]
    fn clickpad_softbutton_left() {
        let mut sb = pad();
        sb.handle_frame(0, &[touch(0, ButtonPhase::Begin, 100, 1100)]);
        let (posted, events) = click(&mut sb, 0, true);
        assert!(posted);
        assert_eq!(events, [(ClickButton::Left, true)]);
        let (posted, events) = click(&mut sb, 1000, false);
        assert!(posted);
        assert_eq!(events, [(ClickButton::Left, false)]);

        // Anywhere above the button area is left too.
        let mut sb = pad();
        sb.handle_frame(0, &[touch(0, ButtonPhase::Begin, 1500, 500)]);
        let (_, events) = click(&mut sb, 0, true);
        assert_eq!(events, [(ClickButton::Left, true)]);
    }

    /// Mirrors libinput's clickpad_softbutton_middle.
    #[test]
    fn clickpad_softbutton_middle() {
        let mut sb = pad();
        sb.handle_frame(0, &[touch(0, ButtonPhase::Begin, 900, 1100)]);
        let (posted, events) = click(&mut sb, 0, true);
        assert!(posted);
        assert_eq!(events, [(ClickButton::Middle, true)]);
        let (posted, events) = click(&mut sb, 1000, false);
        assert!(posted);
        assert_eq!(events, [(ClickButton::Middle, false)]);
    }

    /// Mirrors libinput's clickpad_softbutton_left_1st_fg_move: a finger that starts in the area and moves
    /// out becomes AREA (active again) and the release posts the same button that was pressed.
    #[test]
    fn clickpad_softbutton_left_1st_fg_move() {
        let mut sb = pad();
        sb.handle_frame(0, &[touch(0, ButtonPhase::Begin, 100, 1100)]);
        assert!(!sb.touch_active(0));
        let (_, events) = click(&mut sb, 0, true);
        assert_eq!(events, [(ClickButton::Left, true)]);

        // Move out of the area: BOTTOM -> AREA, so the touch is active again.
        sb.handle_frame(1000, &[touch(0, ButtonPhase::Update, 100, 500)]);
        assert!(sb.touch_active(0));

        let (posted, events) = click(&mut sb, 2000, false);
        assert!(posted);
        assert_eq!(events, [(ClickButton::Left, false)]);
    }

    /// Mirrors libinput's clickpad_softbutton_left_to_right: the button is decided by where the fingers
    /// are at the click, not where they started.
    #[test]
    fn clickpad_softbutton_left_to_right() {
        let mut sb = pad();
        sb.handle_frame(0, &[touch(0, ButtonPhase::Begin, 100, 1100)]);
        sb.handle_frame(1000, &[touch(0, ButtonPhase::Update, 1500, 1100)]);
        let (posted, events) = click(&mut sb, 2000, true);
        assert!(posted);
        assert_eq!(events, [(ClickButton::Right, true)]);
        let (_, events) = click(&mut sb, 3000, false);
        assert_eq!(events, [(ClickButton::Right, false)]);
    }

    /// Two fingers, one resting in the bottom-left area and one in the main area: the click is left,
    /// as tp_post_clickpadbutton_buttons ORs the touches' areas (cf. clickpad_softbutton_left_2nd_fg_move).
    #[test]
    fn clickpad_softbutton_two_fingers_one_in_area() {
        let mut sb = pad();
        sb.handle_frame(0, &[
            touch(0, ButtonPhase::Begin, 100, 1100),
            touch(1, ButtonPhase::Begin, 900, 500),
        ]);
        let (posted, events) = click(&mut sb, 0, true);
        assert!(posted);
        assert_eq!(events, [(ClickButton::Left, true)]);
        assert!(!sb.touch_active(0));
        assert!(sb.touch_active(1));
        let (_, events) = click(&mut sb, 1000, false);
        assert_eq!(events, [(ClickButton::Left, false)]);
    }
}
