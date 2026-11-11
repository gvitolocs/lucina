// SPDX-License-Identifier: MPL-2.0

//! The touchpad gesture record and the queue behind it: what `/dev/lucina-gesture` carries
//! (`docs/research/juno-touchpad.md`).
//!
//! The kernel's touchpad driver recognises swipes, pinches and holds and writes them to that character device; the shell
//! reads the device and the window-manager rules match on what it reads. This crate is the definition all three share, so
//! a record cannot mean one thing to the writer and another to the reader. The kernel build copies it into its own tree,
//! so it is `no_std`, `unsafe`-free, with no dependencies and no `alloc`: `core` only. The host tests are its one use of
//! `std`.
//!
//! * [`GestureRecord`] is one record; [`GestureRecord::to_bytes`] and [`GestureRecord::from_bytes`] are the 32 bytes it
//!   travels as, and [`DecodeError`] what the reader refuses.
//! * [`units_to_fixed`], [`fixed_to_units`], [`to_q16`] and [`from_q16`] move between the driver's floats and the record's
//!   two fixed points.
//! * [`Header`], [`StreamReader`] and [`write_stream`] are the stream a reader gets: a 32-byte header, then records.
//! * [`rules::encode_step`] is the rule the driver's encoder follows (one open gesture, paired begins and ends), proved
//!   with Verus.
//! * [`Queue`] is what the driver holds while nobody reads, and what a reader that cannot keep up pays for: motion, never a
//!   gesture's begin or its end.
//!
//! # The 32 bytes, little endian
//!
//! | Offset | Type | Field | Meaning |
//! | --- | --- | --- | --- |
//! | 0 | `u64` | [`time_us`](GestureRecord::time_us) | `CLOCK_MONOTONIC` microseconds of the touch frame |
//! | 8 | `u8` | [`kind`](GestureRecord::kind) | 1 swipe, 2 pinch, 3 hold; 4 is reserved (three-finger drag) and never sent |
//! | 9 | `u8` | [`phase`](GestureRecord::phase) | 1 begin, 2 update, 3 end |
//! | 10 | `u8` | [`fingers`](GestureRecord::fingers) | 1 to 4 |
//! | 11 | `u8` | [`flags`](GestureRecord::flags) | bit 0 [`FLAG_CANCELLED`], bit 1 [`FLAG_NATURAL`], bit 2 [`FLAG_MERGED`] |
//! | 12 | `i32` | [`dx`](GestureRecord::dx) | unaccelerated motion in 1/256 of a libinput unit; 0 on begin and end |
//! | 16 | `i32` | [`dy`](GestureRecord::dy) | the same for y, positive down the pad |
//! | 20 | `i32` | [`scale`](GestureRecord::scale) | Q16 pinch scale since the begin; [`SCALE_ONE`] for every other kind |
//! | 24 | `i32` | [`angle`](GestureRecord::angle) | Q16 degrees changed since the previous record, clockwise positive; 0 otherwise |
//! | 28 | `u32` | [`seq`](GestureRecord::seq) | the gesture's number: the same on its begin, updates and end |
//!
//! A libinput unit at 1000 dpi is 0.0254 mm, so the record's `dx` of 256 is one libinput unit, a millimetre of motion.

#![cfg_attr(not(test), no_std)]
#![forbid(unsafe_code)]

mod queue;
pub mod rules;
mod stream;

pub use queue::Queue;
pub use stream::{
    FORMAT_VERSION, HEADER_LEN, Header, HeaderError, MAGIC, Mismatch, StreamReader, check_header,
    rebase_to_zero, write_stream,
};

/// Bytes in one record: the device's unit, and what [`GestureRecord::from_bytes`] needs to see before it decodes.
pub const RECORD_LEN: usize = 32;
/// Byte 11, bit 0: this end interrupted the gesture rather than finishing it (end phase only).
pub const FLAG_CANCELLED: u8 = 1 << 0;
/// Byte 11, bit 1: natural scrolling is on, so the motion already points the way the content moves.
pub const FLAG_NATURAL: u8 = 1 << 1;
/// Byte 11, bit 2: the queue ran full and one of this gesture's records did not stay in it on its own — its motion is
/// added into this record, or was lost when nothing could take it. See [`Queue`].
pub const FLAG_MERGED: u8 = 1 << 2;
/// Q16's 1.0: [`GestureRecord::scale`] at it means the fingers have not moved apart.
pub const SCALE_ONE: i32 = 65536;
/// How many records the driver queues before it starts folding updates into each other.
pub const QUEUE_LEN: usize = 128;

/// What the driver recognised, byte 8.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum Kind {
    /// One or two fingers sliding: a swipe, or a two-finger scroll.
    Swipe = 1,
    /// Two fingers spreading or closing.
    Pinch = 2,
    /// Fingers held still after the tap-and-hold, until they lift.
    Hold = 3,
}

/// Where the gesture is in its life, byte 9.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum Phase {
    /// The gesture started. `dx` and `dy` are 0, `scale` is [`SCALE_ONE`], `angle` is 0.
    Begin = 1,
    /// The fingers moved since the previous record.
    Update = 2,
    /// The gesture finished, cancelled or not: see [`FLAG_CANCELLED`]. `dx` and `dy` are 0.
    End = 3,
}

impl Kind {
    /// The kind byte `v` stands for, or None for 0, 4 and above (4 is the reserved three-finger drag).
    fn from_u8(v: u8) -> Option<Kind> {
        match v {
            1 => Some(Kind::Swipe),
            2 => Some(Kind::Pinch),
            3 => Some(Kind::Hold),
            _ => None,
        }
    }
}

impl Phase {
    /// The phase byte `v` stands for, or None for 0 and 4 and above.
    fn from_u8(v: u8) -> Option<Phase> {
        match v {
            1 => Some(Phase::Begin),
            2 => Some(Phase::Update),
            3 => Some(Phase::End),
            _ => None,
        }
    }
}

/// One gesture record: what the driver recognised in one touch frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GestureRecord {
    /// `CLOCK_MONOTONIC` microseconds of the touch frame this came from.
    pub time_us: u64,
    /// Swipe, pinch or hold.
    pub kind: Kind,
    /// Begin, update or end.
    pub phase: Phase,
    /// How many fingers, 1 to 4.
    pub fingers: u8,
    /// The `FLAG_` bits.
    pub flags: u8,
    /// Unaccelerated motion in 1/256 of a libinput unit, from [`units_to_fixed`]; 0 on begin and end.
    pub dx: i32,
    /// The same for y, positive down the pad.
    pub dy: i32,
    /// Q16 pinch scale since this gesture's begin, from [`to_q16`]; [`SCALE_ONE`] for every other kind and on a pinch begin.
    pub scale: i32,
    /// Q16 degrees the pinch turned since the previous record, clockwise positive, from [`to_q16`]; 0 otherwise.
    pub angle: i32,
    /// This gesture's number: the same on its begin, updates and end, one more (wrapping) than the gesture before it.
    pub seq: u32,
}

/// What [`GestureRecord::from_bytes`] refused about the bytes it was given.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DecodeError {
    /// Fewer than [`RECORD_LEN`] bytes: a partial read, which the reader has to finish before it can decode.
    Short,
    /// Byte 8 held this, which is no [`Kind`]: 0, or 4 and above.
    Kind(u8),
    /// Byte 9 held this, which is no [`Phase`].
    Phase(u8),
    /// Byte 10 held this finger count: 0, or above 4.
    Fingers(u8),
}

impl GestureRecord {
    /// The 32 bytes of the table above.
    pub fn to_bytes(&self) -> [u8; RECORD_LEN] {
        let mut b = [0u8; RECORD_LEN];
        b[0..8].copy_from_slice(&self.time_us.to_le_bytes());
        b[8] = self.kind as u8;
        b[9] = self.phase as u8;
        b[10] = self.fingers;
        b[11] = self.flags;
        b[12..16].copy_from_slice(&self.dx.to_le_bytes());
        b[16..20].copy_from_slice(&self.dy.to_le_bytes());
        b[20..24].copy_from_slice(&self.scale.to_le_bytes());
        b[24..28].copy_from_slice(&self.angle.to_le_bytes());
        b[28..32].copy_from_slice(&self.seq.to_le_bytes());
        b
    }

    /// One record out of the bytes the reader got, reading the first [`RECORD_LEN`] and leaving any rest for the next
    /// record. Refuses fewer bytes than that, an unknown kind, an unknown phase, and fingers 0 or above 4.
    pub fn from_bytes(b: &[u8]) -> Result<GestureRecord, DecodeError> {
        if b.len() < RECORD_LEN {
            return Err(DecodeError::Short);
        }
        let kind = Kind::from_u8(b[8]).ok_or(DecodeError::Kind(b[8]))?;
        let phase = Phase::from_u8(b[9]).ok_or(DecodeError::Phase(b[9]))?;
        if b[10] == 0 || b[10] > 4 {
            return Err(DecodeError::Fingers(b[10]));
        }
        Ok(GestureRecord {
            time_us: le(b, 0, 8),
            kind,
            phase,
            fingers: b[10],
            flags: b[11],
            dx: le(b, 12, 4) as i32,
            dy: le(b, 16, 4) as i32,
            scale: le(b, 20, 4) as i32,
            angle: le(b, 24, 4) as i32,
            seq: le(b, 28, 4) as u32,
        })
    }

    /// Whether this end interrupted the gesture rather than finishing it; meaningless on a begin or an update.
    pub fn cancelled(&self) -> bool {
        self.flags & FLAG_CANCELLED != 0
    }
}

/// `at` through `len` bytes as one little-endian number, widened to `u64`. The caller has checked the slice is long
/// enough, which is why there is no `Result` here.
fn le(b: &[u8], at: usize, len: usize) -> u64 {
    let mut x = 0u64;
    let mut i = len;
    while i > 0 {
        i -= 1;
        x = (x << 8) | b[at + i] as u64;
    }
    x
}

/// libinput units (1000 dpi, so one unit is 0.0254 mm) as the record's 1/256 units: rounded to the nearest, ties away
/// from zero, saturating at `i32::MIN` and `i32::MAX`. Not-a-number is 0, which is no motion.
pub fn units_to_fixed(units: f64) -> i32 {
    round_fixed(units, 256.0)
}

/// The record's 1/256 units back as libinput units, exact for every value the record can hold.
pub fn fixed_to_units(v: i32) -> f64 {
    v as f64 / 256.0
}

/// A pinch factor (1.0 at [`SCALE_ONE`]) or an angle in degrees as Q16, with the same rounding and saturation as
/// [`units_to_fixed`].
pub fn to_q16(v: f64) -> i32 {
    round_fixed(v, 65536.0)
}

/// Q16 back as a factor or degrees.
pub fn from_q16(v: i32) -> f64 {
    v as f64 / 65536.0
}

/// `v` times `k` to the nearest integer, ties away from zero, saturating at `i32::MIN` and `i32::MAX`; not-a-number is 0.
///
/// The truncating cast and one comparison do the rounding, not `f64::round`: that is a `std` method, and it would be a
/// call to a libc `round` the kernel does not have.
fn round_fixed(v: f64, k: f64) -> i32 {
    let x = v * k;
    if x.is_nan() {
        return 0;
    }
    if x >= i32::MAX as f64 {
        return i32::MAX;
    }
    if x <= i32::MIN as f64 {
        return i32::MIN;
    }
    let t = x as i32;
    let frac = x - t as f64;
    if frac >= 0.5 {
        t + 1
    } else if frac <= -0.5 {
        t - 1
    } else {
        t
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const KINDS: [Kind; 3] = [Kind::Swipe, Kind::Pinch, Kind::Hold];
    const PHASES: [Phase; 3] = [Phase::Begin, Phase::Update, Phase::End];
    const FINGERS: [u8; 4] = [1, 2, 3, 4];

    fn record() -> GestureRecord {
        GestureRecord {
            time_us: 1_234_567,
            kind: Kind::Swipe,
            phase: Phase::Update,
            fingers: 2,
            flags: FLAG_NATURAL,
            dx: -256,
            dy: 512,
            scale: SCALE_ONE,
            angle: 0,
            seq: 41,
        }
    }

    /// Every field, at each of its extremes, through the bytes and back.
    #[test]
    fn every_field_round_trips_at_its_extremes() {
        let times = [0u64, u64::MAX, 1 << 63, 1];
        let flags = [
            0u8,
            FLAG_CANCELLED,
            FLAG_NATURAL,
            FLAG_MERGED,
            0b111,
            u8::MAX,
        ];
        let ints = [i32::MIN, i32::MAX, 0, -1, SCALE_ONE];
        let seqs = [0u32, 1, u32::MAX, 1 << 31];
        for (i, kind) in KINDS.iter().enumerate() {
            for (j, phase) in PHASES.iter().enumerate() {
                for (k, fingers) in FINGERS.iter().enumerate() {
                    let r = GestureRecord {
                        time_us: times[(i + 2 * j + k) % times.len()],
                        kind: *kind,
                        phase: *phase,
                        fingers: *fingers,
                        flags: flags[(i + j + k) % flags.len()],
                        dx: ints[(i + j) % ints.len()],
                        dy: ints[(i + k + 1) % ints.len()],
                        scale: ints[(j + k) % ints.len()],
                        angle: ints[(i + j + k + 2) % ints.len()],
                        seq: seqs[(i + k) % seqs.len()],
                    };
                    let b = r.to_bytes();
                    assert_eq!(b.len(), RECORD_LEN, "{r:?}");
                    assert_eq!(GestureRecord::from_bytes(&b), Ok(r), "{r:?} as {b:02x?}");
                    // a read that got a whole record and more still decodes the first one.
                    let mut with_tail = b.to_vec();
                    with_tail.extend_from_slice(&[0xaa; 9]);
                    assert_eq!(
                        GestureRecord::from_bytes(&with_tail),
                        Ok(r),
                        "{r:?} with a tail"
                    );
                }
            }
        }
    }

    /// The table, byte for byte: each field where its offset says, on one record whose bytes are written out here rather
    /// than produced by the code under test.
    #[test]
    fn the_bytes_are_the_table() {
        let r = GestureRecord {
            time_us: 0x0123_4567_89ab_cdef,
            kind: Kind::Pinch,
            phase: Phase::Update,
            fingers: 3,
            flags: FLAG_CANCELLED | FLAG_MERGED,
            dx: 0x0b0c_0d0e,
            dy: -0x0102_0304,
            scale: -SCALE_ONE,
            angle: 0x0100_0000,
            seq: 0x1122_3344,
        };
        let b = r.to_bytes();
        assert_eq!(&b[0..8], &[0xef, 0xcd, 0xab, 0x89, 0x67, 0x45, 0x23, 0x01]); // 0   time_us, u64
        assert_eq!(b[8], 2); // 8   kind, u8: pinch
        assert_eq!(b[9], 2); // 9   phase, u8: update
        assert_eq!(b[10], 3); // 10  fingers, u8
        assert_eq!(b[11], 0b101); // 11  flags, u8: cancelled and merged
        assert_eq!(&b[12..16], &[0x0e, 0x0d, 0x0c, 0x0b]); // 12  dx, i32
        assert_eq!(&b[16..20], &[0xfc, 0xfc, 0xfd, 0xfe]); // 16  dy, i32: the two's complement of 0x0102_0304
        assert_eq!(&b[20..24], &[0x00, 0x00, 0xff, 0xff]); // 20  scale, i32: 0xffff_0000
        assert_eq!(&b[24..28], &[0x00, 0x00, 0x00, 0x01]); // 24  angle, i32: 256.0 degrees in Q16
        assert_eq!(&b[28..32], &[0x44, 0x33, 0x22, 0x11]); // 28  seq, u32
        assert_eq!(b.len(), RECORD_LEN);
        // the same bytes back into the same record, and one byte short of them refused.
        assert_eq!(GestureRecord::from_bytes(&b), Ok(r));
        assert_eq!(
            GestureRecord::from_bytes(&b[..RECORD_LEN - 1]),
            Err(DecodeError::Short)
        );
    }

    /// The good bytes with byte `at` replaced by `v`.
    fn bytes_with(at: usize, v: u8) -> [u8; RECORD_LEN] {
        let mut b = record().to_bytes();
        b[at] = v;
        b
    }

    #[test]
    fn decode_refuses_what_the_table_does_not_allow() {
        let ok = record().to_bytes();
        assert_eq!(GestureRecord::from_bytes(&[]), Err(DecodeError::Short));
        assert_eq!(
            GestureRecord::from_bytes(&ok[..RECORD_LEN - 1]),
            Err(DecodeError::Short)
        );
        assert_eq!(GestureRecord::from_bytes(&ok[..1]), Err(DecodeError::Short));
        for v in [0u8, 4, 5, u8::MAX] {
            // 4 is the reserved three-finger drag: the driver never sends it, the reader never takes it.
            assert_eq!(
                GestureRecord::from_bytes(&bytes_with(8, v)),
                Err(DecodeError::Kind(v)),
                "kind byte {v}"
            );
        }
        for v in [0u8, 4, 23, u8::MAX] {
            assert_eq!(
                GestureRecord::from_bytes(&bytes_with(9, v)),
                Err(DecodeError::Phase(v)),
                "phase byte {v}"
            );
        }
        for v in [0u8, 5, 23, u8::MAX] {
            assert_eq!(
                GestureRecord::from_bytes(&bytes_with(10, v)),
                Err(DecodeError::Fingers(v)),
                "fingers byte {v}"
            );
        }
        // nothing else is checked: bits the flags do not define, and a scale or angle that is not the kind's.
        assert_eq!(
            GestureRecord::from_bytes(&bytes_with(11, 0b1111_1000)).map(|r| r.flags),
            Ok(0b1111_1000)
        );
        assert_eq!(
            GestureRecord::from_bytes(&bytes_with(9, 1)).map(|r| r.phase),
            Ok(Phase::Begin)
        );
    }

    #[test]
    fn cancelled_is_bit_0_and_nothing_else() {
        for (flags, want) in [
            (0u8, false),
            (FLAG_CANCELLED, true),
            (FLAG_CANCELLED | FLAG_NATURAL | FLAG_MERGED, true),
            (FLAG_NATURAL, false),
            (FLAG_MERGED, false),
            (0b1111_1110, false),
        ] {
            let r = GestureRecord { flags, ..record() };
            assert_eq!(r.cancelled(), want, "flags {flags:08b}");
        }
    }

    #[test]
    fn units_convert_to_the_records_quarter_of_a_unit() {
        assert_eq!(units_to_fixed(1.0), 256);
        assert_eq!(units_to_fixed(-1.5), -384);
        assert_eq!(units_to_fixed(0.001953125), 1); // half of 1/256: a tie, and ties go away from zero
        assert_eq!(units_to_fixed(-0.001953125), -1);
        assert_eq!(units_to_fixed(0.001953124), 0);
        assert_eq!(units_to_fixed(0.0), 0);
        assert_eq!(units_to_fixed(-0.0), 0);
        assert_eq!(units_to_fixed(3.7), 947); // 947.2 rounds down
        assert_eq!(units_to_fixed(-3.7), -947);
        assert_eq!(units_to_fixed(1e12), i32::MAX);
        assert_eq!(units_to_fixed(-1e12), i32::MIN);
        assert_eq!(units_to_fixed(f64::INFINITY), i32::MAX);
        assert_eq!(units_to_fixed(f64::NEG_INFINITY), i32::MIN);
        assert_eq!(units_to_fixed(f64::NAN), 0);
        assert_eq!(units_to_fixed(f64::MAX), i32::MAX);
        assert_eq!(fixed_to_units(256), 1.0);
        assert_eq!(fixed_to_units(-384), -1.5);
        assert_eq!(fixed_to_units(1), 0.00390625); // one of the record's units is 1/256 of a libinput one
        assert_eq!(fixed_to_units(i32::MIN), -8388608.0);
        for v in [0i32, 1, -1, 256, -384, i32::MAX, i32::MIN] {
            assert_eq!(
                units_to_fixed(fixed_to_units(v)),
                v,
                "fixed_to_units then back for {v}"
            );
        }
    }

    #[test]
    fn q16_convert_a_factor_and_an_angle() {
        assert_eq!(to_q16(1.0), 65536);
        assert_eq!(to_q16(-90.0), -5898240);
        assert_eq!(to_q16(0.0), 0);
        assert_eq!(to_q16(0.5), 32768);
        assert_eq!(to_q16(0.0000152587890625), 1); // one Q16 step
        assert_eq!(to_q16(0.00002288818359375), 2); // 1.5 steps: a tie, away from zero
        assert_eq!(to_q16(-0.00002288818359375), -2);
        assert_eq!(to_q16(-1e12), i32::MIN);
        assert_eq!(to_q16(1e12), i32::MAX);
        assert_eq!(to_q16(f64::NAN), 0);
        assert_eq!(from_q16(65536), 1.0);
        assert_eq!(from_q16(-5898240), -90.0);
        assert_eq!(from_q16(SCALE_ONE), 1.0);
        for v in [0i32, 1, -1, 65536, -5898240, i32::MAX, i32::MIN] {
            assert_eq!(to_q16(from_q16(v)), v, "from_q16 then back for {v}");
        }
    }
}
