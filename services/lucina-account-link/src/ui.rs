//! The sign-in screen every Juno program that needs the account shows: the QR code, the six digits, and what is going on. It
//! shows the one pairing `lucina-accountd` runs, so every screen has the same code and approving it signs in the whole
//! machine. The token does not pass through here: it only tells the app that the machine is signed in.
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use lucina_app::{Face, Rect, Rgb, Ui};

use crate::{Link, Pairing, Phase, Qr, Status, QUIET};

pub struct SignInView {
    phase: Arc<Mutex<Phase>>,
    again: Arc<AtomicBool>,
    stop: Arc<AtomicBool>,
}

impl Drop for SignInView {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

/// How often the screen's thread looks up: the countdown, the button, the view going away.
const TICK: Duration = Duration::from_millis(250);

fn phase_of(status: &Status) -> Phase {
    match status {
        Status::Starting | Status::Checking | Status::SignedOut => Phase::Loading,
        Status::SignedIn { .. } => Phase::SignedIn,
        Status::SigningIn { pairing } => match pairing {
            Pairing::Loading => Phase::Loading,
            Pairing::Ready { approve_url, short_code, seconds_left } => match Qr::encode(approve_url) {
                Some(qr) => Phase::Ready { qr: Box::new(qr), short_code: short_code.clone(), seconds_left: *seconds_left },
                None => Phase::Unavailable { message: "The sign-in link does not fit a QR code".into(), rate_limited: false },
            },
            Pairing::Denied => Phase::Denied,
            Pairing::Paused => Phase::Paused,
            Pairing::Unavailable { message, rate_limited } => {
                Phase::Unavailable { message: message.clone(), rate_limited: *rate_limited }
            }
        },
    }
}

impl SignInView {
    /// Asks the service to pair, for as long as this view lives. `done` is called once on that thread when the machine is
    /// signed in, at once if it already is; `changed` after any change.
    pub fn start(changed: Arc<dyn Fn() + Send + Sync>, done: Arc<dyn Fn() + Send + Sync>) -> SignInView {
        let view = SignInView {
            phase: Arc::new(Mutex::new(Phase::Loading)),
            again: Arc::new(AtomicBool::new(false)),
            stop: Arc::new(AtomicBool::new(false)),
        };
        let (phase, again, stop) = (view.phase.clone(), view.again.clone(), view.stop.clone());
        std::thread::spawn(move || {
            let link = Link::system();
            let set = |p: Phase| {
                *phase.lock().unwrap_or_else(|e| e.into_inner()) = p;
                changed();
            };
            while !stop.load(Ordering::Relaxed) {
                let Ok(mut watch) = link.watch(true) else {
                    set(Phase::Unavailable { message: "The account service is not running.".into(), rate_limited: false });
                    std::thread::sleep(Duration::from_secs(1));
                    continue;
                };
                // Seconds are counted here: the service only says when there is a new code.
                let mut expires: Option<Instant> = None;
                let mut shown_secs = u64::MAX;
                while !stop.load(Ordering::Relaxed) {
                    if again.swap(false, Ordering::Relaxed) {
                        let _ = link.new_code();
                    }
                    match watch.next(TICK) {
                        Ok(Some(status)) => {
                            let next = phase_of(&status);
                            expires = match &next {
                                Phase::Ready { seconds_left, .. } => Some(Instant::now() + Duration::from_secs(*seconds_left)),
                                _ => None,
                            };
                            let signed_in = next == Phase::SignedIn;
                            set(next);
                            if signed_in {
                                done();
                                return;
                            }
                        }
                        Ok(None) => {
                            let Some(until) = expires else { continue };
                            let secs = until.saturating_duration_since(Instant::now()).as_secs();
                            if secs != shown_secs {
                                shown_secs = secs;
                                if let Phase::Ready { seconds_left, .. } = &mut *phase.lock().unwrap_or_else(|e| e.into_inner()) {
                                    *seconds_left = secs;
                                }
                                changed();
                            }
                        }
                        Err(_) => break,
                    }
                }
            }
        });
        view
    }

    /// The whole area: left the code, right how to use it. `what` says what the app wants the account for.
    pub fn view(&mut self, ui: &mut Ui, what: &str) {
        let t = ui.theme();
        let area = ui.area();
        let column = Rect::new(area.x + (area.w - t.px(520.0)) / 2, area.y + t.px(26.0), t.px(520.0), area.h - t.px(26.0));
        let phase = self.phase.lock().unwrap_or_else(|p| p.into_inner());
        let mut ask_again = false;
        ui.within(column, |ui| {
            ui.heading("Sign in to Secretary");
            ui.paragraph(what);
            ui.space(8.0);
            match &*phase {
                Phase::Loading => ui.label("One moment…"),
                Phase::Ready { qr, short_code, seconds_left } => {
                    let side = t.px(300.0);
                    let r = ui.cut_top(side);
                    let box_ = Rect::new(r.x, r.y, side, side);
                    ui.private("Sign-in code", |ui| draw_qr(ui, box_, qr));
                    let text = Rect::new(r.x + side + t.px(22.0), r.y, r.w - side - t.px(22.0), side);
                    ui.within(text, |ui| {
                        ui.space(4.0);
                        ui.paragraph("On your phone, in Secretary: Scan QR code, or type this:");
                        ui.space(8.0);
                        if let Some(code) = short_code {
                            ui.private("Sign-in code", |ui| {
                                let shown = format!("{} {}", code.get(..3).unwrap_or(code), code.get(3..).unwrap_or(""));
                                let h = t.px(46.0);
                                let c = ui.cut_top(h);
                                ui.text_in(c, Face::Mono, t.pt(32.0), t.accent, &shown);
                                ui.space(8.0);
                            });
                        }
                        ui.dim(&format!("A new code comes in {seconds_left} s."));
                        ui.space(8.0);
                        ui.dim("No password is typed here.");
                    });
                }
                Phase::Denied => {
                    ui.label("Declined on the phone.");
                    ask_again = ui.primary_button("New code");
                }
                Phase::Paused => {
                    ui.label("No one answered for a while, so it stopped asking.");
                    ask_again = ui.primary_button("New code");
                }
                Phase::Unavailable { message, .. } => {
                    ui.wrapped(Face::Sans, t.pt(15.0), t.item, message);
                    ui.dim("Trying again by itself.");
                }
                Phase::SignedIn => ui.label("Signed in."),
            }
        });
        drop(phase);
        if ask_again {
            self.again.store(true, Ordering::Relaxed);
            *self.phase.lock().unwrap_or_else(|p| p.into_inner()) = Phase::Loading;
        }
    }
}

fn draw_qr(ui: &mut Ui, r: Rect, qr: &Qr) {
    let modules = (qr.width + 2 * QUIET) as i32;
    let m = (r.w / modules).max(1);
    let side = m * modules;
    let at = Rect::new(r.x, r.y, side, side);
    ui.fill(at, Rgb::hex(0xffffff));
    let black = Rgb::hex(0x000000);
    let q = QUIET as i32 * m;
    for y in 0..qr.width {
        for x in 0..qr.width {
            if qr.is_dark(x, y) {
                ui.fill(Rect::new(at.x + q + x as i32 * m, at.y + q + y as i32 * m, m, m), black);
            }
        }
    }
}
