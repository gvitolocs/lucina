// SPDX-License-Identifier: MPL-2.0

//! The touchpad engine's gestures as `/dev/lucina-gesture` records: [`GestureEncoder`].
//!
//! The engine ([`crate::touchpad`]) notifies libinput's hold, swipe and pinch begin, update and end as
//! [`Output::Gesture`]. The encoder turns each into the records of `lucina-gesture`: the pairing, the finger count and the
//! gesture's number come from `lucina_gesture::rules::encode_step` (proved with Verus), the numbers in a record from the
//! engine's. The kernel driver will also call [`GestureEncoder::reset`] on a reset or a suspend (patch 1250, not yet written),
//! after [`Touchpad::cancel_gestures`](crate::touchpad::Touchpad::cancel_gestures), so that no begin is left without its end.

use crate::touchpad::{Gesture, GestureKind, GesturePhase};
use lucina_gesture::rules::{self, Emit, Open, RuleKind, RulePhase};
use lucina_gesture::{
    FLAG_CANCELLED, FLAG_NATURAL, GestureRecord, Kind, Phase, SCALE_ONE, to_q16, units_to_fixed,
};

/// Holds the open gesture and the number of the next one.
#[derive(Clone, Copy, Debug, Default)]
pub struct GestureEncoder {
    open: Option<Open>,
    next_seq: u32,
    /// The pinch scale of the latest record, which the end repeats.
    last_scale: i32,
}

fn rule_kind(k: GestureKind) -> RuleKind {
    match k {
        GestureKind::Swipe => RuleKind::Swipe,
        GestureKind::Pinch => RuleKind::Pinch,
        GestureKind::Hold => RuleKind::Hold,
    }
}

fn record_kind(k: RuleKind) -> Kind {
    match k {
        RuleKind::Swipe => Kind::Swipe,
        RuleKind::Pinch => Kind::Pinch,
        RuleKind::Hold => Kind::Hold,
    }
}

impl GestureEncoder {
    pub const fn new() -> GestureEncoder {
        GestureEncoder {
            open: None,
            next_seq: 0,
            last_scale: SCALE_ONE,
        }
    }

    /// Whether a gesture is open: a begin has been written and its end has not.
    pub fn is_open(&self) -> bool {
        self.open.is_some()
    }

    /// One gesture notification of the engine at `time_us`, with natural scrolling `natural`. Calls `out` with the records
    /// to write: none, one, or two (a cancelled end of the gesture that was open, then the begin).
    pub fn feed(
        &mut self,
        g: &Gesture,
        time_us: u64,
        natural: bool,
        out: &mut dyn FnMut(GestureRecord),
    ) {
        let kind = rule_kind(g.kind);
        let ev = match g.phase {
            GesturePhase::Begin => rules::begin(kind, g.fingers),
            GesturePhase::Update => rules::update(kind),
            GesturePhase::End => rules::end(kind, g.cancelled),
        };
        self.step(ev, Some(g), time_us, natural, out);
    }

    /// The driver reset or the device is suspending: ends the open gesture, cancelled, if there is one.
    pub fn reset(&mut self, time_us: u64, natural: bool, out: &mut dyn FnMut(GestureRecord)) {
        self.step(rules::reset(), None, time_us, natural, out);
    }

    fn step(
        &mut self,
        ev: rules::Event,
        g: Option<&Gesture>,
        time_us: u64,
        natural: bool,
        out: &mut dyn FnMut(GestureRecord),
    ) {
        let s = rules::encode_step(self.open, self.next_seq, ev);
        self.open = s.open;
        self.next_seq = s.next_seq;
        for e in [s.first, s.second].into_iter().flatten() {
            let r = self.record(&e, g, time_us, natural);
            out(r);
        }
    }

    /// The record for `e`: the engine's numbers on an update (a pinch's scale and angle too), the last scale on an end,
    /// nothing on a begin.
    fn record(
        &mut self,
        e: &Emit,
        g: Option<&Gesture>,
        time_us: u64,
        natural: bool,
    ) -> GestureRecord {
        let pinch = e.kind == RuleKind::Pinch;
        let (mut dx, mut dy, mut scale, mut angle) = (0, 0, SCALE_ONE, 0);
        match e.phase {
            RulePhase::Begin => self.last_scale = SCALE_ONE,
            RulePhase::Update => {
                if let Some(g) = g {
                    dx = units_to_fixed(g.dx);
                    dy = units_to_fixed(g.dy);
                    if pinch {
                        self.last_scale = to_q16(g.scale);
                        angle = to_q16(g.angle);
                    }
                }
                if pinch {
                    scale = self.last_scale;
                }
            }
            RulePhase::End => {
                if pinch {
                    scale = self.last_scale;
                }
            }
        }
        let mut flags = 0;
        if e.cancelled {
            flags |= FLAG_CANCELLED;
        }
        if natural {
            flags |= FLAG_NATURAL;
        }
        GestureRecord {
            time_us,
            kind: record_kind(e.kind),
            phase: match e.phase {
                RulePhase::Begin => Phase::Begin,
                RulePhase::Update => Phase::Update,
                RulePhase::End => Phase::End,
            },
            fingers: e.fingers,
            flags,
            dx,
            dy,
            scale,
            angle,
            seq: e.seq,
        }
    }
}

#[cfg(test)]
mod tests {
    extern crate alloc;
    use super::*;
    use alloc::vec::Vec;

    fn pinch(phase: GesturePhase, scale: f64) -> Gesture {
        Gesture {
            kind: GestureKind::Pinch,
            phase,
            fingers: 2,
            cancelled: false,
            dx: 0.0,
            dy: 0.0,
            scale,
            angle: 0.0,
        }
    }

    fn feed_all(enc: &mut GestureEncoder, gs: &[Gesture]) -> Vec<GestureRecord> {
        let mut v = Vec::new();
        for g in gs {
            enc.feed(g, 0, false, &mut |r| v.push(r));
        }
        v
    }

    #[test]
    fn pinch_end_carries_the_last_scale() {
        let mut enc = GestureEncoder::new();
        let rs = feed_all(
            &mut enc,
            &[
                pinch(GesturePhase::Begin, 1.0),
                pinch(GesturePhase::Update, 1.5),
                pinch(GesturePhase::Update, 2.0),
                // The End's own scale is ignored: libinput repeats the last one it notified.
                pinch(GesturePhase::End, 7.0),
            ],
        );
        assert_eq!(rs.len(), 4);
        assert_eq!(rs[0].scale, SCALE_ONE);
        assert_eq!(rs[2].scale, to_q16(2.0));
        assert_eq!(rs[3].phase, Phase::End);
        assert_eq!(rs[3].scale, to_q16(2.0));
    }

    #[test]
    fn pinch_end_without_updates_is_one() {
        let mut enc = GestureEncoder::new();
        let rs = feed_all(
            &mut enc,
            &[
                pinch(GesturePhase::Begin, 1.0),
                pinch(GesturePhase::End, 3.0),
            ],
        );
        assert_eq!(rs.len(), 2);
        assert_eq!(rs[1].phase, Phase::End);
        assert_eq!(rs[1].scale, SCALE_ONE);
    }

    #[test]
    fn a_new_pinch_does_not_inherit_the_last_scale() {
        let mut enc = GestureEncoder::new();
        feed_all(
            &mut enc,
            &[
                pinch(GesturePhase::Begin, 1.0),
                pinch(GesturePhase::Update, 2.0),
                pinch(GesturePhase::End, 2.0),
            ],
        );
        let rs = feed_all(
            &mut enc,
            &[
                pinch(GesturePhase::Begin, 1.0),
                pinch(GesturePhase::End, 1.0),
            ],
        );
        assert_eq!(rs[1].scale, SCALE_ONE);
    }

    #[test]
    fn non_pinch_end_keeps_scale_one() {
        let mut enc = GestureEncoder::new();
        let mut g = pinch(GesturePhase::Begin, 1.0);
        g.kind = GestureKind::Swipe;
        g.fingers = 3;
        let mut e = g;
        e.phase = GesturePhase::End;
        e.scale = 4.0;
        let rs = feed_all(&mut enc, &[g, e]);
        assert_eq!(rs[1].phase, Phase::End);
        assert_eq!(rs[1].scale, SCALE_ONE);
    }
}
