// The gesture encoder's rule, that must hold whatever the engine sends: at most one gesture is open, every begin is closed by
// exactly one end (also on a driver reset or a suspend, cancelled), an update or an end belongs to the open gesture and carries
// its kind, finger count and number, the number goes up by one per begin, and the finger count is 1 to 4 from begin to end.
// Verus checks this very file: `//@` lines hold the proof, see scripts/verify-shipped.sh (D-011). The driver's encoder
// (drivers/lucina-i2c-hid/src/gesture_out.rs) calls `encode_step` for every notification of the touchpad engine.
//
// The original: libinput pairs `gesture_notify_*_begin` with `_end` in `tp_gesture_handle_event`; the engine's own rule is
// proved in drivers/lucina-i2c-hid/src/tp_rules.rs. This rule is the defence behind it: it holds the pairing for the record
// stream even if the engine ever did not.
//
// Not proved: the numbers in a record (motion, scale, angle), which the encoder converts and the tests check.

/// The gestures a record can carry (`Kind` in lib.rs, without the discriminant).
#[derive(Clone, Copy, PartialEq, Eq)]
#[derive(Debug)] //@omit
pub enum RuleKind {
    Swipe,
    Pinch,
    Hold,
}

/// The phase of a record to emit.
#[derive(Clone, Copy, PartialEq, Eq)]
#[derive(Debug)] //@omit
pub enum RulePhase {
    Begin,
    Update,
    End,
}

/// What the engine told the encoder.
#[derive(Clone, Copy, PartialEq, Eq)]
#[derive(Debug)] //@omit
pub enum Op {
    Begin,
    Update,
    End,
    /// The driver reset or the device suspended: whatever is open ends, cancelled.
    Reset,
}

/// One notification: `kind` and `fingers` are for a begin, `kind` for an update, `kind` and `cancelled` for an end; a reset
/// has none of them.
#[derive(Clone, Copy)]
#[derive(Debug)] //@omit
pub struct Event {
    pub op: Op,
    pub kind: RuleKind,
    pub fingers: u8,
    pub cancelled: bool,
}

/// The one gesture that is open.
#[derive(Clone, Copy)]
#[derive(Debug)] //@omit
pub struct Open {
    pub kind: RuleKind,
    pub fingers: u8,
    pub seq: u32,
}

/// A record to write, without its numbers.
#[derive(Clone, Copy)]
#[derive(Debug)] //@omit
pub struct Emit {
    pub phase: RulePhase,
    pub kind: RuleKind,
    pub fingers: u8,
    pub seq: u32,
    pub cancelled: bool,
}

/// What a notification does: the gesture that is open afterwards, the number the next begin takes, and up to two records
/// to write in this order: `first` is an end (the open gesture's, or the update), `second` is the begin.
#[derive(Clone, Copy)]
#[derive(Debug)] //@omit
pub struct Step {
    pub open: Option<Open>,
    pub next_seq: u32,
    pub first: Option<Emit>,
    pub second: Option<Emit>,
}

/// A begin of `kind` with `fingers` fingers.
pub fn begin(kind: RuleKind, fingers: u8) -> Event {
    Event {
        op: Op::Begin,
        kind,
        fingers,
        cancelled: false,
    }
}

/// An update of `kind`.
pub fn update(kind: RuleKind) -> Event {
    Event {
        op: Op::Update,
        kind,
        fingers: 0,
        cancelled: false,
    }
}

/// An end of `kind`, `cancelled` as the engine says.
pub fn end(kind: RuleKind, cancelled: bool) -> Event {
    Event {
        op: Op::End,
        kind,
        fingers: 0,
        cancelled,
    }
}

/// A driver reset or a suspend.
pub fn reset() -> Event {
    Event {
        op: Op::Reset,
        kind: RuleKind::Swipe,
        fingers: 0,
        cancelled: false,
    }
}

/// Whether two kinds are the same, spelled out so that Verus knows what `==` means for the exec code.
#[allow(clippy::match_like_matches_macro)]
fn same_kind(a: RuleKind, b: RuleKind) -> bool //@ret
//@ ensures ret == (a == b),
{
    match (a, b) {
        (RuleKind::Swipe, RuleKind::Swipe) => true,
        (RuleKind::Pinch, RuleKind::Pinch) => true,
        (RuleKind::Hold, RuleKind::Hold) => true,
        _ => false,
    }
}

/// One notification `ev` with the gesture `open` and the number `next_seq` for the next begin.
///
/// A begin with 1 to 4 fingers opens a gesture numbered `next_seq` (and `next_seq` goes up by one, wrapping); if one was
/// open, its cancelled end comes first. An update of the open gesture's kind is written with its kind, fingers and number;
/// an update of another kind, or with nothing open, is dropped. The same for an end, which also closes the gesture. A reset
/// writes the cancelled end of the open gesture, if there is one.
#[allow(clippy::collapsible_if)] // Verus has no let chains
pub fn encode_step(open: Option<Open>, next_seq: u32, ev: Event) -> Step //@ret
//@ requires open.is_some() ==> 1 <= open.unwrap().fingers && open.unwrap().fingers <= 4,
//@ ensures
//@     // an update is written only while its gesture is open, with that gesture's kind, fingers and number; nothing else changes
//@     ret.first.is_some() && ret.first.unwrap().phase == RulePhase::Update
//@         ==> ev.op == Op::Update && open.is_some() && ev.kind == open.unwrap().kind
//@             && ret.first.unwrap().kind == open.unwrap().kind && ret.first.unwrap().fingers == open.unwrap().fingers
//@             && ret.first.unwrap().seq == open.unwrap().seq && !ret.first.unwrap().cancelled
//@             && ret.open.is_some() && ret.open.unwrap().kind == open.unwrap().kind
//@             && ret.open.unwrap().fingers == open.unwrap().fingers && ret.open.unwrap().seq == open.unwrap().seq
//@             && ret.second.is_none() && ret.next_seq == next_seq,
//@     ev.op == Op::Update && open.is_some() && ev.kind == open.unwrap().kind
//@         ==> ret.first.is_some() && ret.first.unwrap().phase == RulePhase::Update,
//@     ev.op == Op::Update && !(open.is_some() && ev.kind == open.unwrap().kind)
//@         ==> ret.first.is_none() && ret.second.is_none() && ret.next_seq == next_seq
//@             && ret.open.is_some() == open.is_some()
//@             && (open.is_some() ==> ret.open.unwrap().kind == open.unwrap().kind
//@                 && ret.open.unwrap().fingers == open.unwrap().fingers && ret.open.unwrap().seq == open.unwrap().seq),
//@     // a begin with 1 to 4 fingers opens a gesture numbered next_seq, and next_seq goes up by one, wrapping, per begin and only then
//@     ev.op == Op::Begin && 1 <= ev.fingers && ev.fingers <= 4
//@         ==> ret.open.is_some() && ret.open.unwrap().kind == ev.kind && ret.open.unwrap().fingers == ev.fingers
//@             && ret.open.unwrap().seq == next_seq
//@             && ret.second.is_some() && ret.second.unwrap().phase == RulePhase::Begin
//@             && ret.second.unwrap().kind == ev.kind && ret.second.unwrap().fingers == ev.fingers
//@             && ret.second.unwrap().seq == next_seq && !ret.second.unwrap().cancelled
//@             && ret.next_seq == (if next_seq == 4294967295u32 { 0u32 } else { (next_seq + 1) as u32 }),
//@     // a begin while one is open ends that one first, cancelled; with none open there is nothing to end
//@     ev.op == Op::Begin && 1 <= ev.fingers && ev.fingers <= 4 && open.is_some()
//@         ==> ret.first.is_some() && ret.first.unwrap().phase == RulePhase::End && ret.first.unwrap().cancelled
//@             && ret.first.unwrap().kind == open.unwrap().kind && ret.first.unwrap().fingers == open.unwrap().fingers
//@             && ret.first.unwrap().seq == open.unwrap().seq,
//@     ev.op == Op::Begin && 1 <= ev.fingers && ev.fingers <= 4 && open.is_none() ==> ret.first.is_none(),
//@     // a begin with no fingers or more than four writes nothing and changes nothing
//@     ev.op == Op::Begin && !(1 <= ev.fingers && ev.fingers <= 4)
//@         ==> ret.first.is_none() && ret.second.is_none() && ret.next_seq == next_seq
//@             && ret.open.is_some() == open.is_some()
//@             && (open.is_some() ==> ret.open.unwrap().kind == open.unwrap().kind
//@                 && ret.open.unwrap().fingers == open.unwrap().fingers && ret.open.unwrap().seq == open.unwrap().seq),
//@     ev.op != Op::Begin ==> ret.next_seq == next_seq && ret.second.is_none(),
//@     // an end closes the open gesture with its kind, fingers and number, and nothing is open after it; an end of another kind,
//@     // or with nothing open, writes nothing
//@     ev.op == Op::End && open.is_some() && ev.kind == open.unwrap().kind
//@         ==> ret.first.is_some() && ret.first.unwrap().phase == RulePhase::End
//@             && ret.first.unwrap().kind == open.unwrap().kind && ret.first.unwrap().fingers == open.unwrap().fingers
//@             && ret.first.unwrap().seq == open.unwrap().seq && ret.first.unwrap().cancelled == ev.cancelled
//@             && ret.open.is_none(),
//@     ev.op == Op::End && !(open.is_some() && ev.kind == open.unwrap().kind)
//@         ==> ret.first.is_none() && ret.second.is_none() && ret.open.is_some() == open.is_some()
//@             && (open.is_some() ==> ret.open.unwrap().kind == open.unwrap().kind
//@                 && ret.open.unwrap().fingers == open.unwrap().fingers && ret.open.unwrap().seq == open.unwrap().seq),
//@     // a reset ends the open gesture, cancelled, once, and leaves nothing open; with none open it writes nothing
//@     ev.op == Op::Reset && open.is_some()
//@         ==> ret.first.is_some() && ret.first.unwrap().phase == RulePhase::End && ret.first.unwrap().cancelled
//@             && ret.first.unwrap().kind == open.unwrap().kind && ret.first.unwrap().fingers == open.unwrap().fingers
//@             && ret.first.unwrap().seq == open.unwrap().seq && ret.second.is_none() && ret.open.is_none(),
//@     ev.op == Op::Reset && open.is_none() ==> ret.first.is_none() && ret.second.is_none() && ret.open.is_none(),
//@     // an end is written only for the open gesture, as above, or when a begin displaces it
//@     ret.first.is_some() && ret.first.unwrap().phase == RulePhase::End
//@         ==> open.is_some() && ret.first.unwrap().kind == open.unwrap().kind
//@             && ret.first.unwrap().fingers == open.unwrap().fingers && ret.first.unwrap().seq == open.unwrap().seq,
//@     // the finger count of everything written and of what stays open is 1 to 4
//@     ret.first.is_some() ==> 1 <= ret.first.unwrap().fingers && ret.first.unwrap().fingers <= 4,
//@     ret.second.is_some() ==> 1 <= ret.second.unwrap().fingers && ret.second.unwrap().fingers <= 4,
//@     ret.open.is_some() ==> 1 <= ret.open.unwrap().fingers && ret.open.unwrap().fingers <= 4,
//@     // the first is never a begin and the second always is
//@     ret.first.is_some() ==> ret.first.unwrap().phase != RulePhase::Begin,
//@     ret.second.is_some() ==> ret.second.unwrap().phase == RulePhase::Begin,
{
    let mut r = Step {
        open,
        next_seq,
        first: None,
        second: None,
    };
    match ev.op {
        Op::Begin => {
            if ev.fingers >= 1 && ev.fingers <= 4 {
                if let Some(o) = open {
                    r.first = Some(Emit {
                        phase: RulePhase::End,
                        kind: o.kind,
                        fingers: o.fingers,
                        seq: o.seq,
                        cancelled: true,
                    });
                }
                r.second = Some(Emit {
                    phase: RulePhase::Begin,
                    kind: ev.kind,
                    fingers: ev.fingers,
                    seq: next_seq,
                    cancelled: false,
                });
                r.open = Some(Open {
                    kind: ev.kind,
                    fingers: ev.fingers,
                    seq: next_seq,
                });
                r.next_seq = if next_seq == 4294967295 {
                    0
                } else {
                    next_seq + 1
                };
            }
        }
        Op::Update => {
            if let Some(o) = open {
                if same_kind(o.kind, ev.kind) {
                    r.first = Some(Emit {
                        phase: RulePhase::Update,
                        kind: o.kind,
                        fingers: o.fingers,
                        seq: o.seq,
                        cancelled: false,
                    });
                }
            }
        }
        Op::End => {
            if let Some(o) = open {
                if same_kind(o.kind, ev.kind) {
                    r.first = Some(Emit {
                        phase: RulePhase::End,
                        kind: o.kind,
                        fingers: o.fingers,
                        seq: o.seq,
                        cancelled: ev.cancelled,
                    });
                    r.open = None;
                }
            }
        }
        Op::Reset => {
            if let Some(o) = open {
                r.first = Some(Emit {
                    phase: RulePhase::End,
                    kind: o.kind,
                    fingers: o.fingers,
                    seq: o.seq,
                    cancelled: true,
                });
            }
            r.open = None;
        }
    }
    r
}
