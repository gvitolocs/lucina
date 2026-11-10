// What must hold whatever the firmware or a file does: the arithmetic of the three shared queues, the addresses the video block is
// given, and where in a buffer a frame it hands back may lie. Verus checks this very file: `//@` lines hold the proof, see
// scripts/verify-shipped.sh (D-011). The rest of the crate and the kernel call these functions and hold no copy of the rules.
//
// The queue rules are `venus_write_queue` and `venus_read_queue` of Linux's hfi_venus.c, with three differences, all refusals Linux
// does not make: the size of a queue is ours and never read back from the memory the firmware writes; an index at or past it stops the
// queue (Linux checks only that the first word lies inside); and a packet longer than what was written, or of no length, is dropped as
// Linux drops one too long (Linux reads past the write index for the first, and for the second returns an error without moving, so that
// the next interrupt meets the same packet for ever).
//
// Not proved: that the bytes of a packet mean what they should (tests/oracle.rs compares them with Linux's own code), the order of the
// barriers around the indices (the kernel's `Shared` does them as Linux does), and what the firmware does with any of it.

/// `IFACEQ_QUEUE_SIZE / 4`: each queue holds this many 32-bit words (`IFACEQ_MAX_PKT_SIZE * IFACEQ_MAX_BUF_COUNT *
/// IFACEQ_MAX_PARALLEL_CLNTS` bytes).
pub const QUEUE_WORDS: u32 = 1024 * 50 * 16 / 4;
/// `IFACEQ_VAR_HUGE_PKT_SIZE / 4`: the longest packet read or written.
pub const MAX_PACKET_WORDS: u32 = 12 * 1024 / 4;
/// A packet's size and type: `struct hfi_pkt_hdr`.
pub const MIN_PACKET_WORDS: u32 = 2;

/// Where a packet goes in a queue, or comes from: `first` words from `at`, then `second` words from the start, and the index after it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)] //@omit
pub struct Span {
    pub at: u32,
    pub first: u32,
    pub second: u32,
    pub next: u32,
}

/// Why a queue was not written or read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)] //@omit
pub enum QueueError {
    /// Not enough room: Linux's `-ENOSPC`, and the writer asks to be told when there is (`tx_req`).
    Full,
    /// A packet of the host's own that is too short, too long or not whole words.
    BadPacket,
    /// An index at or past the end of the queue: the firmware wrote nonsense, and nothing more is read or written.
    Corrupt,
}

//@ pub open spec fn used(rd: u32, wr: u32) -> int { if wr >= rd { wr - rd } else { QUEUE_WORDS - rd + wr } }

/// Words free for a writer: Linux's `empty_space`. A queue is never filled to the last word, or full would look empty.
pub fn free_words(rd: u32, wr: u32) -> u32 //@ret
//@ requires rd < QUEUE_WORDS, wr < QUEUE_WORDS,
//@ ensures ret == QUEUE_WORDS - used(rd, wr), ret >= 1, ret <= QUEUE_WORDS,
{
    if wr >= rd { QUEUE_WORDS - (wr - rd) } else { rd - wr }
}

/// Where a packet of `bytes` goes, the indices being what the queue header says now.
pub fn write_span(rd: u32, wr: u32, bytes: u32) -> Result<Span, QueueError> //@ret
//@ ensures match ret {
//@     Ok(s) => rd < QUEUE_WORDS && wr < QUEUE_WORDS && bytes % 4 == 0
//@         && MIN_PACKET_WORDS <= bytes / 4 && bytes / 4 <= MAX_PACKET_WORDS
//@         && s.at == wr && s.first + s.second == bytes / 4 && s.at + s.first <= QUEUE_WORDS
//@         && (s.second > 0 ==> s.at + s.first == QUEUE_WORDS)
//@         && s.next < QUEUE_WORDS && s.next == (if wr + bytes / 4 < QUEUE_WORDS { wr + bytes / 4 } else { wr + bytes / 4 - QUEUE_WORDS })
//@         && used(rd, s.next) == used(rd, wr) + bytes / 4 && used(rd, s.next) < QUEUE_WORDS,
//@     Err(QueueError::Full) => rd < QUEUE_WORDS && wr < QUEUE_WORDS && used(rd, wr) + bytes / 4 >= QUEUE_WORDS,
//@     Err(QueueError::Corrupt) => rd >= QUEUE_WORDS || wr >= QUEUE_WORDS,
//@     Err(QueueError::BadPacket) => bytes % 4 != 0 || bytes / 4 < MIN_PACKET_WORDS || bytes / 4 > MAX_PACKET_WORDS,
//@ },
{
    if bytes % 4 != 0 || bytes / 4 < MIN_PACKET_WORDS || bytes / 4 > MAX_PACKET_WORDS {
        return Err(QueueError::BadPacket);
    }
    if rd >= QUEUE_WORDS || wr >= QUEUE_WORDS {
        return Err(QueueError::Corrupt);
    }
    let words = bytes / 4;
    if free_words(rd, wr) <= words {
        return Err(QueueError::Full);
    }
    let room = QUEUE_WORDS - wr;
    if words < room {
        Ok(Span { at: wr, first: words, second: 0, next: wr + words })
    } else {
        Ok(Span { at: wr, first: room, second: words - room, next: words - room })
    }
}

/// What reading a queue finds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)] //@omit
pub enum Found {
    /// Nothing written since the last read: Linux's `-ENODATA`.
    Empty,
    /// A packet at this span.
    Packet(Span),
    /// A packet that cannot be right: everything written so far is skipped, and the read index moves to `next`, the write index.
    Dropped(u32),
}

/// What is at the read index, `head` being the word there (a packet's size in bytes), the indices being what the queue header says now.
pub fn read_span(rd: u32, wr: u32, head: u32) -> Result<Found, QueueError> //@ret
//@ ensures match ret {
//@     Ok(Found::Empty) => rd < QUEUE_WORDS && wr < QUEUE_WORDS && rd == wr,
//@     Ok(Found::Packet(s)) => rd < QUEUE_WORDS && wr < QUEUE_WORDS && rd != wr && head % 4 == 0
//@         && MIN_PACKET_WORDS <= head / 4 && head / 4 <= MAX_PACKET_WORDS && head / 4 <= used(rd, wr)
//@         && s.at == rd && s.first + s.second == head / 4 && s.at + s.first <= QUEUE_WORDS
//@         && (s.second > 0 ==> s.at + s.first == QUEUE_WORDS)
//@         && s.next < QUEUE_WORDS && s.next == (if rd + head / 4 < QUEUE_WORDS { rd + head / 4 } else { rd + head / 4 - QUEUE_WORDS })
//@         && used(s.next, wr) == used(rd, wr) - head / 4,
//@     Ok(Found::Dropped(next)) => rd < QUEUE_WORDS && wr < QUEUE_WORDS && rd != wr && next == wr,
//@     Err(QueueError::Corrupt) => rd >= QUEUE_WORDS || wr >= QUEUE_WORDS,
//@     Err(_) => false,
//@ },
{
    if rd >= QUEUE_WORDS || wr >= QUEUE_WORDS {
        return Err(QueueError::Corrupt);
    }
    if rd == wr {
        return Ok(Found::Empty);
    }
    let words = head / 4;
    let written = if wr >= rd { wr - rd } else { QUEUE_WORDS - rd + wr };
    if head % 4 != 0 || words < MIN_PACKET_WORDS || words > MAX_PACKET_WORDS || words > written {
        return Ok(Found::Dropped(wr));
    }
    let room = QUEUE_WORDS - rd;
    if words < room {
        Ok(Found::Packet(Span { at: rd, first: words, second: 0, next: rd + words }))
    } else {
        Ok(Found::Packet(Span { at: rd, first: room, second: words - room, next: words - room }))
    }
}

/// The first address the video block's non-secure side may be given: `cp_start + cp_size` of Linux's `sc7180_res`. Below it the
/// firmware keeps its content-protected range (`qcom_scm_mem_protect_video_var(0, 0x70800000, ...)`).
pub const DEVICE_LOW: u64 = 0x7080_0000;
/// One past the last address: `dma_mask = 0xe0000000 - 1` of `sc7180_res`. Linux's translated addresses all lie below; here the stream
/// goes untranslated, so the physical address is the address and the memory itself must lie below.
pub const DEVICE_END: u64 = 0xe000_0000;

/// The 32-bit address to give the firmware for `bytes` of memory at `phys`, or `None` if the block must not be given it.
pub fn device_address(phys: u64, bytes: u64) -> Option<u32> //@ret
//@ ensures match ret {
//@     Some(a) => a == phys && bytes > 0 && 0x7080_0000 <= phys && phys + bytes <= 0xe000_0000,
//@     None => bytes == 0 || phys < 0x7080_0000 || phys >= 0xe000_0000 || bytes > 0xe000_0000 - phys,
//@ },
{
    if bytes == 0 || phys < DEVICE_LOW || phys >= DEVICE_END || bytes > DEVICE_END - phys {
        return None;
    }
    Some(phys as u32)
}

/// Whether what the firmware says it filled lies inside the buffer it was given: `offset` and `filled` of a fill-buffer-done message,
/// `alloc` being the buffer's size as the host gave it, not as the message repeats it.
pub fn filled_within(offset: u32, filled: u32, alloc: u32) -> bool //@ret
//@ ensures ret == (offset <= alloc && filled <= alloc - offset),
{
    offset <= alloc && filled <= alloc - offset
}

// Sizes a file decides. The firmware reads the picture size from the bitstream and says it in the sequence-changed event; nothing below
// lets a size reach an allocation or the hardware before these have passed it.

/// The decoder's limits on SC7180 (`hfi_platform_v4.c`, H.264, HEVC, VP8 and VP9: 96 to 4096 each way, at most 36864 macroblocks a
/// picture) and `lucina_video::limits::MAX_LUMA_SAMPLES` (4096 x 2304, which is 36864 macroblocks too).
pub const MIN_SIDE: u32 = 96;
pub const MAX_SIDE: u32 = 4096;
pub const MAX_MACROBLOCKS: u32 = 36864;
/// Buffers of one kind a session may ask for: `VB2_MAX_FRAME`, what Linux tells the firmware for the outputs.
pub const MAX_BUFFERS: u32 = 32;
/// `vdec_try_fmt_common`'s ceiling on an input buffer, `SZ_8M`.
pub const MAX_INPUT_BYTES: u32 = 0x80_0000;

/// Whether a picture of `w` x `h` may be decoded here.
pub fn picture_ok(w: u32, h: u32) -> bool //@ret
//@ ensures ret ==> 96 <= w <= 4096 && 96 <= h <= 4096 && ((w + 15) / 16) * ((h + 15) / 16) <= 36864,
{
    if w < MIN_SIDE || w > MAX_SIDE || h < MIN_SIDE || h > MAX_SIDE {
        return false;
    }
    //@ assert(((w + 15) / 16) * ((h + 15) / 16) <= 256 * 256) by (nonlinear_arith) requires (w + 15) / 16 <= 256, (h + 15) / 16 <= 256;
    (w + 15) / 16 * ((h + 15) / 16) <= MAX_MACROBLOCKS
}

/// Where the planes of a linear NV12 picture lie in its buffer (`get_framesize_raw_nv12`): Y from 0, CbCr from `uv_offset`, both
/// lines `stride` bytes apart.
#[derive(Clone, Copy, Debug, PartialEq, Eq)] //@omit
pub struct Nv12 {
    pub stride: u32,
    pub y_lines: u32,
    pub uv_offset: u32,
    pub uv_lines: u32,
    pub size: u32,
}

/// The linear NV12 layout for a picture [`picture_ok`] took. Every byte of the picture's Y lines and CbCr lines lies inside `size`.
pub fn nv12(w: u32, h: u32) -> Option<Nv12> //@ret
//@ ensures match ret {
//@     Some(n) => 96 <= w <= 4096 && 96 <= h <= 4096 && n.stride >= w && n.y_lines >= h && n.uv_lines * 2 >= h
//@         && n.uv_offset == n.stride * n.y_lines && n.uv_offset + n.stride * n.uv_lines <= n.size && n.size % 4096 == 0
//@         && n.size <= 0x200_0000,
//@     None => true,
//@ },
{
    if !picture_ok(w, h) {
        return None;
    }
    let stride = (w + 127) / 128 * 128;
    let y_lines = (h + 31) / 32 * 32;
    let uv_lines = ((h + 1) / 2 + 15) / 16 * 16;
    //@ assert(stride <= 4096 && y_lines <= 4096 && uv_lines <= 2048);
    //@ assert(stride * y_lines <= 4096 * 4096) by (nonlinear_arith) requires stride <= 4096, y_lines <= 4096;
    //@ assert(stride * uv_lines <= 4096 * 2048) by (nonlinear_arith) requires stride <= 4096, uv_lines <= 2048;
    let uv_offset = stride * y_lines;
    let bytes = uv_offset + stride * uv_lines;
    let size = (bytes + 4095) / 4096 * 4096;
    Some(Nv12 { stride, y_lines, uv_offset, uv_lines, size })
}

/// The UBWC reference picture's size (`get_framesize_raw_nv12_ubwc`), the metadata planes and the tail included.
pub fn ubwc_size(w: u32, h: u32) -> Option<u32> //@ret
//@ ensures match ret { Some(s) => 96 <= w <= 4096 && 96 <= h <= 4096 && s % 4096 == 0 && s <= 0x400_0000, None => true },
{
    if !picture_ok(w, h) {
        return None;
    }
    let y_meta_stride = ((w + 31) / 32 + 63) / 64 * 64;
    let y_meta_lines = ((h + 7) / 8 + 15) / 16 * 16;
    let y_stride = (w + 127) / 128 * 128;
    let y_lines = (h + 31) / 32 * 32;
    let uv_meta_stride = ((w / 2 + 15) / 16 + 63) / 64 * 64;
    let uv_meta_lines = ((h / 2 + 7) / 8 + 15) / 16 * 16;
    let uv_lines = (h / 2 + 31) / 32 * 32;
    //@ assert(y_meta_stride <= 192 && y_meta_lines <= 512 && uv_meta_stride <= 192 && uv_meta_lines <= 256);
    //@ assert(y_stride <= 4096 && y_lines <= 4096 && uv_lines <= 2048);
    //@ assert(y_meta_stride * y_meta_lines <= 192 * 512) by (nonlinear_arith) requires y_meta_stride <= 192, y_meta_lines <= 512;
    //@ assert(uv_meta_stride * uv_meta_lines <= 192 * 256) by (nonlinear_arith) requires uv_meta_stride <= 192, uv_meta_lines <= 256;
    //@ assert(y_stride * y_lines <= 4096 * 4096) by (nonlinear_arith) requires y_stride <= 4096, y_lines <= 4096;
    //@ assert(y_stride * uv_lines <= 4096 * 2048) by (nonlinear_arith) requires y_stride <= 4096, uv_lines <= 2048;
    //@ assert(y_stride * 48 <= 4096 * 48) by (nonlinear_arith) requires y_stride <= 4096;
    let y_meta = (y_meta_stride * y_meta_lines + 4095) / 4096 * 4096;
    let y_plane = (y_stride * y_lines + 4095) / 4096 * 4096;
    let uv_meta = (uv_meta_stride * uv_meta_lines + 4095) / 4096 * 4096;
    let uv_plane = (y_stride * uv_lines + 4095) / 4096 * 4096;
    let extra = if y_stride * 48 > 16384 { y_stride * 48 } else { 16384 };
    Some((y_meta + y_plane + uv_meta + uv_plane + extra + 4095) / 4096 * 4096)
}

/// An input buffer's size (`venus_helper_get_framesz` for a compressed format, at least what `vdec_try_fmt_common` asks, at most
/// [`MAX_INPUT_BYTES`]).
pub fn input_size(w: u32, h: u32) -> Option<u32> //@ret
//@ ensures match ret { Some(s) => s % 4096 == 0 && 0 < s <= 0x80_0000, None => true },
{
    if !picture_ok(w, h) {
        return None;
    }
    let aw = (w + 31) / 32 * 32;
    let ah = (h + 31) / 32 * 32;
    //@ assert(aw * ah <= 4096 * 4096) by (nonlinear_arith) requires aw <= 4096, ah <= 4096;
    let mut size = ah * aw * 3 / 2 / 2;
    if w < 1280 || h < 720 {
        size = if size > MAX_INPUT_BYTES / 8 { MAX_INPUT_BYTES } else { size * 8 };
    }
    let size = (size + 4095) / 4096 * 4096;
    //@ assert(size % 4096 == 0);
    if size > MAX_INPUT_BYTES {
        Some(MAX_INPUT_BYTES)
    } else if size == 0 {
        Some(4096)
    } else {
        Some(size)
    }
}

/// Whether the firmware's count for a kind of buffer is one this driver allocates: at least one, at most [`MAX_BUFFERS`].
pub fn count_ok(count: u32) -> bool //@ret
//@ ensures ret == (1 <= count <= 32),
{
    count >= 1 && count <= MAX_BUFFERS
}
