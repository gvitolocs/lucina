// The arithmetic of `mse.rs` that a page decides: how long a box is, where a sample lies in the bytes held, how much may be held. A
// page appends whatever it likes, so every size and offset here is a number from it. Verus checks this very file: `//@` lines hold the
// proof, see scripts/verify-shipped.sh (D-011). `mse.rs` calls these functions and holds no copy of the rules.
//
// Not proved: that the boxes and elements mean what `mse.rs` reads into them (tests/mse.rs compares the frames with the file
// demuxers'), and the EBML variable-length numbers, which `mkv.rs` reads.

/// The most bytes held for one SourceBuffer before an append is refused.
pub const MAX_PENDING: u64 = 134217728; // 128 MiB, as a literal: Verus does not evaluate a shift in a constant

/// What a box header says, read from the bytes held so far.
#[derive(Clone, Copy, Debug, PartialEq, Eq)] //@omit
pub enum BoxSize {
    /// Not enough bytes yet to say.
    NeedMore,
    /// A size that cannot be a box's: smaller than its own header, or "to the end of the file", which a stream has not.
    Bad,
    /// Larger than `limit`.
    TooLarge,
    /// The header is `head` bytes, the whole box `size`.
    Box { head: u64, size: u64 },
}

/// A box header: `small` the 32-bit size, `large` the 64-bit one when `small` is 1 and 16 bytes are held, `held` how many bytes are.
pub fn box_size(small: u32, large: Option<u64>, held: u64, limit: u64) -> BoxSize //@ret
//@ ensures match ret {
//@     BoxSize::Box { head, size } => held >= head && (head == 8 || head == 16) && head <= size && size <= limit
//@         && (head == 8 ==> size == small) && (head == 16 ==> small == 1 && large == Some(size)),
//@     BoxSize::TooLarge => (small > 1 && small > limit) || (small == 1 && large.is_some() && large.unwrap() > limit),
//@     BoxSize::NeedMore => held < 8 || (small == 1 && (held < 16 || large.is_none())),
//@     BoxSize::Bad => held >= 8 && (small == 0 || (small > 1 && small < 8) || (small == 1 && large.is_some() && large.unwrap() < 16)),
//@ },
{
    if held < 8 {
        return BoxSize::NeedMore;
    }
    if small == 0 {
        return BoxSize::Bad;
    }
    if small == 1 {
        if held < 16 {
            return BoxSize::NeedMore;
        }
        match large {
            None => BoxSize::NeedMore,
            Some(size) => {
                if size < 16 {
                    BoxSize::Bad
                } else if size > limit {
                    BoxSize::TooLarge
                } else {
                    BoxSize::Box { head: 16, size }
                }
            }
        }
    } else if small < 8 {
        BoxSize::Bad
    } else if small as u64 > limit {
        BoxSize::TooLarge
    } else {
        BoxSize::Box {
            head: 8,
            size: small as u64,
        }
    }
}

/// Where an element ends in the stream: `at` its start, `head` its header, `size` its body. `None` past what a u64 holds.
pub fn end_of(at: u64, head: u64, size: u64) -> Option<u64> //@ret
//@ ensures match ret { Some(e) => e == at + head + size, None => at + head + size > u64::MAX },
{
    if head > u64::MAX - at {
        return None;
    }
    if size > u64::MAX - (at + head) {
        return None;
    }
    Some(at + head + size)
}

/// Where a `trun`'s data starts: `base` moved by its signed data offset. `None` before the stream's first byte or past a u64.
pub fn data_offset(base: u64, delta: i32) -> Option<u64> //@ret
//@ ensures match ret { Some(o) => o == base + delta, None => base + delta < 0 || base + delta > u64::MAX },
{
    if delta >= 0 {
        let d = delta as u64;
        if d > u64::MAX - base {
            None
        } else {
            Some(base + d)
        }
    } else {
        let d = (0 - (delta as i64)) as u64;
        if d > base { None } else { Some(base - d) }
    }
}

/// The sample after one of `size` bytes at `offset`. `None` past a u64.
pub fn next_offset(offset: u64, size: u32) -> Option<u64> //@ret
//@ ensures match ret { Some(o) => o == offset + size, None => offset + size > u64::MAX },
{
    if size as u64 > u64::MAX - offset {
        None
    } else {
        Some(offset + size as u64)
    }
}

/// Where the sample `[offset, offset + size)` lies in the bytes held, which start at stream position `held_at` and are `held_len`
/// long, when it lies inside the `mdat` body `[start, end)` and that body is held: `Some((from, to))` with `from <= to <= held_len`.
pub fn sample_in(
    offset: u64,
    size: u64,
    start: u64,
    end: u64,
    held_at: u64,
    held_len: u64,
) -> Option<(u64, u64)> //@ret
//@ ensures match ret {
//@     Some((from, to)) => start <= offset && offset + size <= end && held_at <= start && end <= held_at + held_len
//@         && from == offset - held_at && to == from + size && from <= to && to <= held_len,
//@     None => !(start <= offset && offset + size <= end) || !(held_at <= start && end <= held_at + held_len),
//@ },
{
    if start > end || held_at > start || end - held_at > held_len {
        return None;
    }
    if offset < start || offset > end || size > end - offset {
        return None;
    }
    let from = offset - held_at;
    Some((from, from + size))
}

/// Whether `held` bytes may grow by `n` more: at most [`MAX_PENDING`] in all.
pub fn may_grow(held: u64, n: u64) -> bool //@ret
//@ ensures ret == (held + n <= 134217728), // the value, not the name: changing the limit must be refused
{
    held <= MAX_PENDING && n <= MAX_PENDING - held
}

/// Whether a fragment holding `count` samples may take `n` more: at most `max` in all.
pub fn samples_ok(count: u64, n: u64, max: u64) -> bool //@ret
//@ ensures ret == (count + n <= max),
{
    count <= max && n <= max - count
}

/// Whether an element of `size` bytes may be held whole when the most is `limit`.
pub fn size_ok(size: u64, limit: u64) -> bool //@ret
//@ ensures ret == (size <= limit),
{
    size <= limit
}
