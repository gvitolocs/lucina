// The buffer rules of the camera path: where a payload's data starts in an isochronous packet, how much of it a frame buffer
// takes, where each packet lies in a transfer's buffer, how many packets a transfer has, and the three frame buffers that the
// USB thread fills and a reader copies out of, so that a frame being read is never written. Verus checks this very file: `//@`
// lines hold the proof, see scripts/verify-shipped.sh (D-011). The kernel's UVC function and [`crate::stream`] call these
// functions and hold no copy of the rules.
//
// The behaviour is Linux's `drivers/media/usb/uvc/uvc_video.c` (v6.16): `uvc_video_decode_start` (a payload is at least two
// bytes and its header length is at least 2 and at most the packet), `uvc_video_decode_data` (copy at most what is left of the
// buffer; more than that completes the frame with the error flag), `uvc_init_video_isoc` (packets of the endpoint's size, as
// many as the frame needs, at most `UVC_MAX_PACKETS` = 32). The three buffers stand in for V4L2's queue: Linux hands a full
// buffer to the reader and takes the next free one from the queue; here the reader always gets the newest whole frame.
//
// Not proved: that the bytes copied are the packet's (the slices are taken with the spans proved here and checked by Rust).

/// `UVC_MAX_PACKETS` of `uvcvideo.h`.
pub const MAX_PACKETS: usize = 32;
/// Three frame buffers; `NONE` is the index of none of them.
pub const SLOTS: u8 = 3;
pub const NONE: u8 = 3;

/// Where the data of one payload lies in its packet.
#[derive(Clone, Copy, Debug, PartialEq, Eq)] //@omit
pub struct Payload {
    /// The packet holds a valid header; `false` drops it (Linux counts it in `nb_invalid`).
    pub ok: bool,
    /// The header's length: the data starts here.
    pub start: usize,
    /// The data's length, after the header.
    pub data: usize,
}

/// A packet of `len` bytes whose first byte, `bHeaderLength`, is `header`.
pub fn payload(len: usize, header: u8) -> Payload //@ret
//@ ensures ret.ok == (2 <= len && 2 <= header && header as usize <= len),
//@     ret.ok ==> ret.start == header as usize && ret.start + ret.data == len,
//@     !ret.ok ==> ret.start == 0 && ret.data == 0,
{
    if len < 2 || header < 2 || header as usize > len {
        return Payload { ok: false, start: 0, data: 0 };
    }
    Payload { ok: true, start: header as usize, data: len - header as usize }
}

/// How much of a payload's data a frame buffer takes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)] //@omit
pub struct Take {
    pub n: usize,
    /// The data did not fit: the frame is complete, and marked bad.
    pub overflow: bool,
}

/// `len` more bytes for a buffer of `cap` bytes that holds `used`.
pub fn take(used: usize, cap: usize, len: usize) -> Take //@ret
//@ requires used <= cap,
//@ ensures ret.n <= len, used + ret.n <= cap, ret.overflow == (len > cap - used), !ret.overflow ==> ret.n == len,
//@     ret.overflow ==> used + ret.n == cap,
{
    let room = cap - used;
    if len > room {
        Take { n: room, overflow: true }
    } else {
        Take { n: len, overflow: false }
    }
}

/// Where packet `index` of an isochronous transfer lies in its buffer: packet `i` starts at `i * size`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)] //@omit
pub struct Span {
    pub ok: bool,
    pub start: usize,
    pub len: usize,
}

/// Packet `index` of packets of `size` bytes, of which the controller says `actual` arrived, in a buffer of `buffer` bytes. A
/// packet that claims more than its size, or lies past the buffer, is refused, never read.
pub fn packet_span(index: usize, size: usize, actual: usize, buffer: usize) -> Span //@ret
//@ ensures ret.ok ==> ret.start + ret.len <= buffer && ret.len == actual && actual <= size && ret.start == index * size,
//@     !ret.ok ==> ret.len == 0,
{
    if actual > size {
        return Span { ok: false, start: 0, len: 0 };
    }
    let Some(start) = index.checked_mul(size) else {
        return Span { ok: false, start: 0, len: 0 };
    };
    if start > buffer || actual > buffer - start {
        return Span { ok: false, start: 0, len: 0 };
    }
    Span { ok: true, start, len: actual }
}

/// The number of packets of a transfer: as many as a frame of `frame` bytes needs in packets of `size` bytes, from 1 to
/// [`MAX_PACKETS`] (Linux `uvc_init_video_isoc`).
pub fn packets(frame: usize, size: usize) -> usize //@ret
//@ requires size > 0,
//@ ensures 1 <= ret <= MAX_PACKETS, frame <= ret * size || ret == MAX_PACKETS,
{
    let whole = frame / size;
    let rest = frame % size;
    if whole >= MAX_PACKETS {
        return MAX_PACKETS;
    }
    //@ assert(frame == whole * size + rest && rest < size) by (nonlinear_arith) requires whole == frame / size, rest == frame % size, size > 0;
    let need = if rest != 0 { whole + 1 } else { whole };
    //@ assert(frame <= need * size) by (nonlinear_arith) requires frame == whole * size + rest, rest < size, (rest == 0 && need == whole) || need == whole + 1;
    if need == 0 {
        //@ assert(frame <= 1 * size) by (nonlinear_arith) requires frame <= need * size, need == 0, size > 0;
        1
    } else {
        need
    }
}

/// Whether an alternate setting whose endpoint moves `size` bytes per (micro)frame is a better one for a stream that needs
/// `bandwidth` than the best so far, `best`: the smallest that is big enough (Linux `uvc_video_start_transfer`).
pub fn better_alt(size: u32, bandwidth: u32, best: u32) -> bool //@ret
//@ ensures ret == (size >= bandwidth && size < best),
{
    size >= bandwidth && size < best
}

/// The three frame buffers: `fill` is the one the USB thread writes into, `ready` holds the newest whole frame, `held` the one a
/// reader copies out of. The one that is none of them is free.
#[derive(Clone, Copy, Debug, PartialEq, Eq)] //@omit
pub struct Slots {
    pub fill: u8,
    pub ready: u8,
    pub held: u8,
}

//@ pub open spec fn some(i: u8) -> bool { i < SLOTS }
//@ pub open spec fn slots_ok(s: Slots) -> bool {
//@     &&& some(s.fill)
//@     &&& (some(s.ready) || s.ready == NONE)
//@     &&& (some(s.held) || s.held == NONE)
//@     &&& s.fill != s.ready && s.fill != s.held
//@     &&& (some(s.ready) ==> s.ready != s.held)
//@ }

/// At the start: buffer 0 fills, nothing is ready or held.
pub fn slots_new() -> Slots //@ret
//@ ensures slots_ok(ret), ret.ready == NONE, ret.held == NONE,
{
    Slots { fill: 0, ready: NONE, held: NONE }
}

/// The lowest buffer that is neither `a` nor `b`.
fn other(a: u8, b: u8) -> u8 //@ret
//@ requires some(a), some(b) || b == NONE, a != b,
//@ ensures some(ret), ret != a, ret != b,
{
    if a != 0 && b != 0 {
        0
    } else if a != 1 && b != 1 {
        1
    } else {
        2
    }
}

/// The filled buffer holds a whole frame: it becomes the ready one, and the USB thread goes on in the frame it replaces (an
/// older ready frame nobody took is dropped) or in the free one.
pub fn publish(s: Slots) -> Slots //@ret
//@ requires slots_ok(s),
//@ ensures slots_ok(ret), ret.ready == s.fill, ret.held == s.held, ret.fill != s.fill,
{
    let fill = if s.ready != NONE { s.ready } else { other(s.fill, s.held) };
    Slots { fill, ready: s.fill, held: s.held }
}

/// What a reader got.
#[derive(Clone, Copy, Debug, PartialEq, Eq)] //@omit
pub struct Acquired {
    pub slots: Slots,
    pub got: bool,
}

/// A reader takes the newest whole frame, if there is one, and gives back the one it held.
pub fn acquire(s: Slots) -> Acquired //@ret
//@ requires slots_ok(s),
//@ ensures slots_ok(ret.slots), ret.slots.fill == s.fill, ret.got == (s.ready != NONE),
//@     ret.got ==> ret.slots.held == s.ready && ret.slots.ready == NONE,
//@     !ret.got ==> ret.slots == s,
{
    if s.ready == NONE {
        return Acquired { slots: s, got: false };
    }
    Acquired { slots: Slots { fill: s.fill, ready: NONE, held: s.ready }, got: true }
}

/// The reader is done with the frame it held.
pub fn release(s: Slots) -> Slots //@ret
//@ requires slots_ok(s),
//@ ensures slots_ok(ret), ret.held == NONE, ret.fill == s.fill, ret.ready == s.ready,
{
    Slots { fill: s.fill, ready: s.ready, held: NONE }
}

/// The stream starts again (a new format, or after a stop): the filled buffer is emptied and nothing is ready. A held frame stays
/// held until its reader lets it go.
pub fn restart(s: Slots) -> Slots //@ret
//@ requires slots_ok(s),
//@ ensures slots_ok(ret), ret.ready == NONE, ret.held == s.held,
{
    Slots { fill: s.fill, ready: NONE, held: s.held }
}

// The frame boundaries of `uvc_video_decode_start` (Linux v6.16), on a payload whose header is valid.

/// `last` when no payload has been taken yet (Linux `stream->last_fid` = -1).
pub const NO_FID: u8 = 2;

/// What a valid payload does to the frame being assembled.
#[derive(Clone, Copy, Debug, PartialEq, Eq)] //@omit
pub enum Fid {
    /// No frame is being filled and the FID has not toggled since the last one: Linux "Dropping payload (out of sync)".
    Drop,
    /// The FID toggled while the frame holds data: that frame is complete, and this payload is taken again for the next one.
    EndFrame,
    /// The payload's data belongs to the frame being filled (or starts one).
    Data,
}

/// A payload with FID bit `fid` when the previous one had `last` (or [`NO_FID`]), the frame is being filled if `active`, and it
/// holds `used` bytes.
pub fn fid_step(active: bool, last: u8, fid: u8, used: usize) -> Fid //@ret
//@ requires fid <= 1, last <= NO_FID,
//@ ensures (ret == Fid::Drop) == (!active && last == fid),
//@     (ret == Fid::EndFrame) == ((active || last != fid) && last != fid && used != 0),
//@     ret == Fid::Data ==> (last == fid ==> active) && (last != fid ==> used == 0),
{
    if !active && last == fid {
        return Fid::Drop;
    }
    if last != fid && used != 0 {
        return Fid::EndFrame;
    }
    Fid::Data
}

/// Whether a completed frame is flagged (Linux `buf->error`): an error seen while filling it, or an uncompressed frame that is not
/// exactly `frame_size` bytes (`uvc_video_validate_buffer`).
pub fn frame_bad(compressed: bool, used: usize, frame_size: usize, error: bool) -> bool //@ret
//@ ensures ret == (error || (!compressed && used != frame_size)),
{
    error || (!compressed && used != frame_size)
}

/// Whether the preview shows a frame: never one the kernel flagged. A flagged MJPEG frame has a hole and decodes to a grey band, a
/// flagged YUY2 frame is short or lost data; Linux hands them on flagged (`uvc_no_drop_param` = 1) and leaves it to the reader.
pub fn shown(error: bool) -> bool //@ret
//@ ensures ret == !error,
{
    !error
}

// The sizes a frame must have before a converter or the GPU reads it. The sides come from the frame header, which the kernel
// fills from the committed format; these checks keep a wrong one from reading past a buffer or overflowing a product.

/// Larger than any side a UVC frame descriptor gives this camera, and than the GPU samples (`yuv_rules::MAX_SIDE`).
pub const MAX_SIDE: usize = 0x4000;

/// The bytes of a YUY2 frame of `w` x `h` (two bytes a pixel, pixels in pairs), or 0 if those sides cannot be one.
pub fn yuy2_len(w: usize, h: usize) -> usize //@ret
//@ ensures (ret != 0) == (0 < w < MAX_SIDE && 0 < h < MAX_SIDE && w % 2 == 0),
//@     ret != 0 ==> ret == w * h * 2,
{
    if w == 0 || h == 0 || w >= MAX_SIDE || h >= MAX_SIDE || !w.is_multiple_of(2) {
        return 0;
    }
    //@ assert(0 < w * h * 2 < 0x4000 * 0x4000 * 2) by (nonlinear_arith) requires 0 < w < 0x4000, 0 < h < 0x4000;
    w * h * 2
}

/// The plane sizes of an I420 picture.
#[derive(Clone, Copy, Debug, PartialEq, Eq)] //@omit
pub struct I420Size {
    pub ok: bool,
    /// Luma bytes: `w` x `h`.
    pub y: usize,
    /// Bytes of each chroma plane: half the width and half the height, rounded up.
    pub c: usize,
}

/// The planes of a `w` x `h` I420 picture; `ok` is false (and the sizes 0) when the sides cannot be a picture.
pub fn i420_size(w: usize, h: usize) -> I420Size //@ret
//@ ensures ret.ok == (0 < w < MAX_SIDE && 0 < h < MAX_SIDE),
//@     ret.ok ==> ret.y == w * h && ret.c == ((w + 1) / 2) * ((h + 1) / 2) && ret.c <= ret.y,
//@     !ret.ok ==> ret.y == 0 && ret.c == 0,
{
    if w == 0 || h == 0 || w >= MAX_SIDE || h >= MAX_SIDE {
        return I420Size { ok: false, y: 0, c: 0 };
    }
    let (cw, ch) = (w / 2 + w % 2, h / 2 + h % 2);
    //@ assert(cw == (w + 1) / 2 && ch == (h + 1) / 2 && cw <= w && ch <= h);
    //@ assert(w * h < 0x4000 * 0x4000) by (nonlinear_arith) requires 0 < w < 0x4000, 0 < h < 0x4000;
    //@ assert(cw * ch <= w * h) by (nonlinear_arith) requires cw <= w, ch <= h;
    I420Size { ok: true, y: w * h, c: cw * ch }
}

/// Whether `rows` lines of `row` bytes, packed in a source of `src_len` bytes, can be copied to lines of `pitch` bytes from `offset`
/// in a buffer of `dst_len` bytes: every byte read and written is inside its slice.
pub fn rows_fit(src_len: usize, row: usize, rows: usize, offset: usize, pitch: usize, dst_len: usize) -> bool //@ret
//@ ensures ret ==> row <= pitch && rows * row <= src_len && offset <= dst_len && rows * pitch <= dst_len - offset,
{
    if row > pitch || rows >= MAX_SIDE || pitch >= 4 * MAX_SIDE || offset > dst_len {
        return false;
    }
    //@ assert(rows * pitch < 0x4000 * 0x10000) by (nonlinear_arith) requires rows < 0x4000, pitch < 0x10000;
    //@ assert(rows * row <= rows * pitch) by (nonlinear_arith) requires row <= pitch;
    rows * row <= src_len && rows * pitch <= dst_len - offset
}
