//! Payloads to frames: Linux `uvc_video.c` `uvc_video_decode_isoc` with `uvc_video_decode_start`, `uvc_video_decode_data`,
//! `uvc_video_decode_end` and `uvc_video_validate_buffer` (v6.16), for a camera without quirks, with the clock and metadata
//! parts left out.
//!
//! Linux decodes into the V4L2 buffer at the head of its queue and moves to the next one when a frame is complete. Here the
//! frame goes into the buffer a [`Sink`] lends: the kernel's three frame buffers, whose turns are [`crate::rules`]. Frames with
//! the error flag are handed on with it (`uvc_no_drop_param` is 1 in Linux: they reach the reader, flagged).

use crate::rules;

/// `bmHeaderInfo` bits (UVC payload header, section 2.4.3.3).
pub const FID: u8 = 1 << 0;
pub const EOF: u8 = 1 << 1;
pub const ERR: u8 = 1 << 6;

/// Where a frame goes.
pub trait Sink {
    /// The buffer the current frame is written into. Its length is the frame's capacity.
    fn buffer(&mut self) -> &mut [u8];
    /// The frame in [`Sink::buffer`] is complete: `len` bytes of it. The next call of `buffer` must lend the next one.
    fn complete(&mut self, frame: Completed);
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Completed {
    pub len: usize,
    /// Linux `buf->error`: a lost packet, the error bit, an overflow, or an uncompressed frame of the wrong size.
    pub error: bool,
    /// Linux `buf->buf.sequence`: increases at every FID change, so a gap means frames were lost.
    pub sequence: u32,
}

/// Counters, as Linux's `stream->stats`, for logs and `/proc/lucina-camera`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Stats {
    pub packets: u64,
    pub lost: u64,
    pub invalid: u64,
    pub out_of_sync: u64,
    pub frames: u64,
    pub error_frames: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum State {
    /// Linux `UVC_BUF_STATE_QUEUED`: waiting for the start of a frame.
    Queued,
    Active,
}

pub struct Assembler {
    /// Linux `stream->last_fid`, -1 at the start: `None`.
    last_fid: Option<u8>,
    sequence: u32,
    state: State,
    used: usize,
    error: bool,
    frame_sequence: u32,
    compressed: bool,
    /// `dwMaxVideoFrameSize` of the committed control: an uncompressed frame must be exactly this long.
    frame_size: usize,
    stats: Stats,
}

enum Start {
    Again,
    Drop,
    Data(usize),
}

impl Assembler {
    pub fn new(compressed: bool, frame_size: usize) -> Assembler {
        Assembler {
            last_fid: None,
            sequence: 0,
            state: State::Queued,
            used: 0,
            error: false,
            frame_sequence: 0,
            compressed,
            frame_size,
            stats: Stats::default(),
        }
    }

    pub fn stats(&self) -> Stats {
        self.stats
    }

    /// One isochronous packet. `ok` is the packet's own status (Linux `iso_frame_desc[i].status >= 0`).
    pub fn packet(&mut self, data: &[u8], ok: bool, sink: &mut dyn Sink) {
        self.stats.packets += 1;
        if !ok {
            // Linux: "USB isochronous frame lost", the buffer is marked faulty.
            self.stats.lost += 1;
            self.error = true;
            return;
        }
        let mut again = false;
        let start = loop {
            match self.decode_start(data, again, sink) {
                Start::Again => again = true,
                Start::Drop => return,
                Start::Data(start) => break start,
            }
        };
        self.decode_data(&data[start..], sink);
        // decode_end, unless decode_data already completed the frame on an overflow.
        if self.state == State::Active && data[1] & EOF != 0 && self.used != 0 {
            self.complete(sink);
        }
    }

    /// `again` is the second pass over a packet that ended the previous frame by toggling the FID. Linux counts the FID change
    /// in both passes, so its sequence jumps by two there; here it counts once, so that a gap means a lost frame only.
    fn decode_start(&mut self, data: &[u8], again: bool, sink: &mut dyn Sink) -> Start {
        let header = if data.is_empty() { 0 } else { data[0] };
        let p = rules::payload(data.len(), header);
        if !p.ok {
            self.stats.invalid += 1;
            return Start::Drop;
        }
        let info = data[1];
        let fid = info & FID;
        if self.last_fid != Some(fid) && !again {
            self.sequence = self.sequence.wrapping_add(1);
        }
        if info & ERR != 0 {
            self.error = true;
        }
        let last = self.last_fid.unwrap_or(rules::NO_FID);
        let step = rules::fid_step(self.state == State::Active, last, fid, self.used);
        if step == rules::Fid::Drop {
            self.stats.out_of_sync += 1;
            return Start::Drop;
        }
        if self.state != State::Active {
            self.frame_sequence = self.sequence;
            self.state = State::Active;
        }
        if step == rules::Fid::EndFrame {
            // The FID toggled: the previous frame is complete, and this packet starts the next one.
            self.complete(sink);
            return Start::Again;
        }
        self.last_fid = Some(fid);
        Start::Data(p.start)
    }

    fn decode_data(&mut self, data: &[u8], sink: &mut dyn Sink) {
        if data.is_empty() {
            return;
        }
        let buffer = sink.buffer();
        let used = self.used.min(buffer.len());
        let t = rules::take(used, buffer.len(), data.len());
        buffer[used..used + t.n].copy_from_slice(&data[..t.n]);
        self.used = used + t.n;
        if t.overflow {
            self.error = true;
            self.complete(sink);
        }
    }

    /// `uvc_video_next_buffers` with `uvc_video_validate_buffer`.
    fn complete(&mut self, sink: &mut dyn Sink) {
        self.error = rules::frame_bad(self.compressed, self.used, self.frame_size, self.error);
        let frame = Completed { len: self.used, error: self.error, sequence: self.frame_sequence };
        self.stats.frames += 1;
        if frame.error {
            self.stats.error_frames += 1;
        }
        sink.complete(frame);
        self.used = 0;
        self.error = false;
        self.state = State::Queued;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec::Vec;

    struct Frames {
        buf: Vec<u8>,
        got: Vec<(Vec<u8>, Completed)>,
    }

    impl Sink for Frames {
        fn buffer(&mut self) -> &mut [u8] {
            &mut self.buf
        }
        fn complete(&mut self, frame: Completed) {
            self.got.push((self.buf[..frame.len].to_vec(), frame));
        }
    }

    fn pkt(info: u8, data: &[u8]) -> Vec<u8> {
        let mut p = alloc::vec![2, info];
        p.extend_from_slice(data);
        p
    }

    fn sink(cap: usize) -> Frames {
        Frames { buf: alloc::vec![0; cap], got: Vec::new() }
    }

    #[test]
    fn eof_completes_a_frame() {
        let mut a = Assembler::new(true, 0);
        let mut s = sink(16);
        a.packet(&pkt(0, b"ab"), true, &mut s);
        a.packet(&pkt(EOF, b"cd"), true, &mut s);
        assert_eq!(s.got.len(), 1);
        assert_eq!(s.got[0].0, b"abcd");
        assert!(!s.got[0].1.error);
    }

    #[test]
    fn fid_toggle_completes_a_frame_without_eof() {
        let mut a = Assembler::new(true, 0);
        let mut s = sink(16);
        a.packet(&pkt(0, b"ab"), true, &mut s);
        a.packet(&pkt(FID, b"cd"), true, &mut s);
        assert_eq!(s.got.len(), 1);
        assert_eq!(s.got[0].0, b"ab");
        a.packet(&pkt(FID | EOF, b"ef"), true, &mut s);
        assert_eq!(s.got[1].0, b"cdef");
        assert_eq!(s.got[1].1.sequence, s.got[0].1.sequence + 1);
    }

    #[test]
    fn after_a_frame_the_same_fid_is_out_of_sync() {
        // Linux: a payload with the last FID while no buffer is active is dropped until the FID toggles.
        let mut a = Assembler::new(true, 0);
        let mut s = sink(16);
        a.packet(&pkt(EOF, b"ab"), true, &mut s);
        a.packet(&pkt(0, b"zz"), true, &mut s);
        a.packet(&pkt(FID | EOF, b"cd"), true, &mut s);
        assert_eq!(s.got.len(), 2);
        assert_eq!(s.got[1].0, b"cd");
        assert_eq!(a.stats().out_of_sync, 1);
    }

    #[test]
    fn invalid_headers_are_dropped() {
        let mut a = Assembler::new(true, 0);
        let mut s = sink(16);
        a.packet(&[1], true, &mut s);
        a.packet(&[9, 0, 1, 2], true, &mut s);
        a.packet(&[1, 0, 1, 2], true, &mut s);
        assert!(s.got.is_empty());
        assert_eq!(a.stats().invalid, 3);
    }

    #[test]
    fn lost_packet_and_error_bit_flag_the_frame() {
        let mut a = Assembler::new(true, 0);
        let mut s = sink(16);
        a.packet(&pkt(0, b"ab"), true, &mut s);
        a.packet(&[], false, &mut s);
        a.packet(&pkt(EOF, b"cd"), true, &mut s);
        assert!(s.got[0].1.error);
        a.packet(&pkt(FID | ERR, b"ef"), true, &mut s);
        a.packet(&pkt(FID | EOF, b"gh"), true, &mut s);
        assert!(s.got[1].1.error);
    }

    #[test]
    fn overflow_completes_with_error() {
        let mut a = Assembler::new(true, 0);
        let mut s = sink(3);
        a.packet(&pkt(0, b"abcd"), true, &mut s);
        assert_eq!(s.got.len(), 1);
        assert_eq!(s.got[0].0, b"abc");
        assert!(s.got[0].1.error);
    }

    #[test]
    fn uncompressed_frames_must_be_whole() {
        let mut a = Assembler::new(false, 4);
        let mut s = sink(8);
        a.packet(&pkt(EOF, b"abcd"), true, &mut s);
        a.packet(&pkt(FID | EOF, b"abc"), true, &mut s);
        assert!(!s.got[0].1.error);
        assert!(s.got[1].1.error);
    }

    #[test]
    fn header_only_eof_ends_the_frame() {
        let mut a = Assembler::new(true, 0);
        let mut s = sink(8);
        a.packet(&pkt(0, b"ab"), true, &mut s);
        a.packet(&pkt(EOF, b""), true, &mut s);
        assert_eq!(s.got.len(), 1);
        assert_eq!(s.got[0].0, b"ab");
    }
}
