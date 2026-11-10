// SPDX-License-Identifier: GPL-2.0-only
//! One decode session, in the order Linux's vdec.c and helpers.c run it for 4xx (`docs/research/juno-venus.md` section 3.5):
//! `vdec_session_init`, `vdec_start_output` (cores, properties, the output configuration, `vdec_verify_conf`, buffer counts, internal
//! buffers, load and start), then on the sequence-changed event `vdec_start_capture` (the output configuration for the real size, the
//! scratch buffers again, the reference pictures, continue), and `vdec_session_release` at the end. The reference pictures are UBWC
//! and stay with the driver; what a program sees are the linear NV12 pictures of the second output.
//!
//! The sizes a file decides come only through [`rules`]: a picture the decoder or this machine cannot take, a firmware count past
//! [`rules::MAX_BUFFERS`], or a size the firmware asks for above Linux's own formula stops the session before anything is allocated.

use alloc::{collections::VecDeque, vec::Vec};

use lucina_qcom_soc::io::RegIo;

use crate::{
    Error, Hal, Result,
    cpu::{self, Core},
    err,
    hfi::{self, Message, cmd, msg},
    iface::Shared,
    rules::{self, MAX_PACKET_WORDS, Nv12},
    session::{self, Input, InputDone, OutputDone, Requirement, Sequence, buffer, event, property},
};

/// What a session needs from the machine: contiguous memory the block may be given, and power and clock for its codec core.
pub trait Machine {
    /// `bytes` of zeroed, uncached, contiguous memory in the block's address range; its device address.
    fn alloc(&mut self, bytes: u32) -> Result<u32>;
    /// Gives back what [`Machine::alloc`] gave, once the firmware has handed it back.
    fn free(&mut self, address: u32);
    /// Keeps what [`Machine::alloc`] gave because the firmware may still be writing it: the default leaks the memory, a machine
    /// with an IOMMU in front of the block can instead park the address where a late write faults.
    fn quarantine(&mut self, _address: u32) {}
    /// `venus_pm_acquire_core` / `release_core`: vcodec0's switch and clocks.
    fn codec_power(&mut self, on: bool) -> Result<()>;
    /// `venus_pm_load_scale`: the core clock and bandwidth for this many macroblocks a second, `payload` being the largest input queued
    /// so far. Linux changes nothing while that is 0 (`load_scale_v4` returns early for a decoder with no filled input).
    fn load(&mut self, macroblocks_per_second: u32, payload: u32) -> Result<()>;
}

/// What came back from the firmware, in the order it came.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Event {
    InputDone(InputDone),
    /// A linear picture, or the end of the stream (`flags` has `EOS`).
    Picture(OutputDone),
    /// A reference picture handed back (after a flush).
    Reference(u32),
    Sequence(Sequence),
    /// `HFI_EVENT_RELEASE_BUFFER_REFERENCE`: the picture at this address and tag may be filled again.
    Released { address: u32, tag: u32 },
    /// `HFI_EVENT_SESSION_ERROR` with its HFI error.
    Error(u32),
    Flushed(u32),
}

/// Where the session is in vdec.c's order (`inst->codec_state`, `VENUS_DEC_STATE_*`): after [`Session::open`] it is `Init`, the
/// first sequence-changed event makes it `CaptureSetup`, [`Session::reconfigure`] makes it `Decoding`, a seek's flush makes it
/// `Seek`, and a sequence-changed event while decoding makes it `Drc`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum State {
    Init,
    CaptureSetup,
    Decoding,
    Drain,
    Stopped,
    Seek,
    Drc,
}

/// Pictures beyond the firmware's minimum, as a V4L2 client asks for a few more than `fw_min_cnt` (GStreamer asks for 2 more, FFmpeg
/// for more): the time between a picture's done message and the program giving it back.
pub const PICTURE_SPARE: u32 = 4;

/// First tag of the reference pictures: `ida_alloc_min(.., VB2_MAX_FRAME)` keeps them apart from the 0..31 of the program's buffers.
pub const REFERENCE_TAG: u32 = 32;
/// Every allocation of one session together, above which it refuses: 4K's reference pictures and outputs fit with room.
pub const MEMORY_BUDGET: u64 = 512 << 20;
/// Session errors `hfi_session_error` lets pass: invalid scale factor, unsupported buffer type, unsupported setting, no upscale.
const BENIGN_ERRORS: [u32; 4] = [0x1012, 0x1010, 0x1008, 0x1013];
/// `HFI_EVENT_DATA_SEQUENCE_CHANGED_SUFFICIENT_BUF_RESOURCES` asked for needs firmware 5.4.51 or later (`vdec_set_properties`).
const SUFFICIENT_EVENT_SINCE: (u32, u32, u32) = (5, 4, 51);
/// `NUM_MBS_720P`: at most this many macroblocks a picture, work mode 1.
const NUM_MBS_720P: u32 = 80 * 46;

pub struct Session {
    pub id: u32,
    pub codec: u32,
    /// The picture's size as the event or the container said it, and `ALIGN(height, 32)` as Linux keeps `inst->height`.
    pub width: u32,
    pub height: u32,
    pub nv12: Nv12,
    pub ubwc: u32,
    pub input_bytes: u32,
    pub inputs: u32,
    /// How many linear pictures the program should give: the firmware's needs, at most [`rules::MAX_BUFFERS`].
    pub outputs: u32,
    internal: Vec<(u32, u32, u32)>,
    references: Vec<u32>,
    events: VecDeque<Event>,
    state: State,
    /// `vdec_event_change`'s flush in DRC, sent once the event has been read (`hfi_session_flush(.., HFI_FLUSH_OUTPUT, false)`).
    flush_output_pending: bool,
    used: u64,
    fps: u32,
    /// The words of the last message that could not be read, for the kernel's log.
    pub last_bad: Vec<u32>,
    /// The largest input queued so far (`inst->payloads` of `venus_helper_vb2_buf_queue`), for the load.
    payload: u32,
}

fn find(reqs: &[Requirement], kind: u32) -> Option<Requirement> {
    reqs.iter().copied().find(|r| r.kind == kind)
}

impl Session {
    /// `vdec_session_init` and `vdec_start_output`, up to the session's start: the stream is `width` x `height` as its container says,
    /// with `inputs` input buffers. `version` is the firmware's (`hfi::parse_version`).
    #[allow(clippy::too_many_arguments)]
    pub fn open<R: RegIo, S: Shared, H: Hal, M: Machine>(
        core: &mut Core<R, S>,
        hal: &H,
        machine: &mut M,
        id: u32,
        codec: u32,
        width: u32,
        height: u32,
        inputs: u32,
        version: (u32, u32, u32),
    ) -> Result<Session> {
        if !rules::picture_ok(width, height) {
            return err("a picture size this decoder cannot take", width << 16 | height);
        }
        let h32 = height.next_multiple_of(32).min(rules::MAX_SIDE);
        let nv12 = rules::nv12(width, h32).ok_or(Error { what: "no NV12 layout", value: width })?;
        let ubwc = rules::ubwc_size(width, h32).ok_or(Error { what: "no UBWC size", value: width })?;
        let input_bytes = rules::input_size(width, height).ok_or(Error { what: "no input size", value: width })?;
        if !rules::count_ok(inputs) {
            return err("an input buffer count this driver does not give", inputs);
        }
        let mut s = Session {
            id,
            codec,
            width,
            height: h32,
            nv12,
            ubwc,
            input_bytes,
            inputs,
            outputs: 0,
            internal: Vec::new(),
            references: Vec::new(),
            events: VecDeque::new(),
            state: State::Init,
            flush_output_pending: false,
            used: 0,
            fps: 30,
            payload: 0,
            last_bad: Vec::new(),
        };
        // venus_session_init: the debug level again, then the session; vdec_session_init: the input's placeholder size.
        core.send(&hfi::sys_debug_config(hfi::DEBUG_MODE_QUEUE, hfi::DEBUG_ERROR_FATAL), false)?;
        s.call(core, hal, &session::init(id, codec), msg::SYS_SESSION_INIT)?;
        let mut powered = false;
        match s.start(core, hal, machine, version, &mut powered) {
            Ok(()) => Ok(s),
            Err(e) => {
                // vdec_session_release on an error: abort, then what was allocated goes back, then the core's power.
                if !core.is_dead() {
                    let _ = s.call(core, hal, &session::command(cmd::SYS_SESSION_ABORT, id), msg::SYS_SESSION_ABORT);
                    for (_, _, address) in core::mem::take(&mut s.internal) {
                        machine.free(address);
                    }
                }
                if powered {
                    let _ = machine.codec_power(false);
                }
                Err(e)
            }
        }
    }

    /// `vdec_start_output` after the session exists.
    fn start<R: RegIo, S: Shared, H: Hal, M: Machine>(
        &mut self,
        core: &mut Core<R, S>,
        hal: &H,
        machine: &mut M,
        version: (u32, u32, u32),
        powered: &mut bool,
    ) -> Result<()> {
        let s = self;
        let (id, inputs) = (s.id, s.inputs);
        s.set(core, property::FRAME_SIZE, &[buffer::INPUT, rules::MIN_SIDE, rules::MIN_SIDE])?;
        // venus_pm_acquire_core: decide_core says which core, then the core is powered.
        s.set(core, property::VIDEOCORES_USAGE, &[1])?;
        machine.codec_power(true)?;
        *powered = true;
        // vdec_set_properties
        if version >= SUFFICIENT_EVENT_SINCE {
            s.set(core, property::SUFFICIENT_SEQCHANGE_EVENT, &[1])?;
        }
        let conceal: u32 = 0x0802_0010;
        let c8 = (conceal & 0xff) | (((conceal >> 10) & 0xff) << 8) | (((conceal >> 20) & 0xff) << 16);
        s.set(core, property::CONCEAL_COLOR, &[c8, conceal])?;
        s.output_conf(core, hal)?;
        // vdec_verify_conf
        let reqs = s.requirements(core, hal)?;
        let out = find(&reqs, buffer::OUTPUT).ok_or(Error { what: "no requirements for the reference pictures", value: 0 })?;
        let reqs = s.requirements(core, hal)?;
        let inp = find(&reqs, buffer::INPUT).ok_or(Error { what: "no requirements for the input", value: 0 })?;
        // Linux: `num_output_bufs >= count_actual` and `>= count_min`, with VB2_MAX_FRAME buffers at most; before the stream's size is
        // known the firmware says 0 for both.
        if out.count_actual > rules::MAX_BUFFERS || out.count_min > rules::MAX_BUFFERS {
            return err("a count for the outputs this driver does not give", out.count_actual.max(out.count_min));
        }
        if inputs < inp.count_min {
            return err("fewer input buffers than the firmware needs", inp.count_min);
        }
        s.outputs = out.count_actual.max(out.count_min);
        s.set_counts(core)?;
        for kind in buffer::INTERNAL {
            s.set_internal(core, hal, machine, kind)?;
        }
        machine.load(s.macroblocks_per_second(), s.payload)?;
        s.call(core, hal, &session::command(cmd::SESSION_LOAD_RESOURCES, id), msg::SESSION_LOAD_RESOURCES)?;
        s.call(core, hal, &session::command(cmd::SESSION_START, id), msg::SESSION_START)?;
        Ok(())
    }

    /// `vdec_start_capture` after a sequence-changed event: the configuration for the real size, the scratch buffers again, the
    /// reference pictures, continue, and each reference picture to the firmware. The program then gives its linear pictures
    /// ([`Self::fill`]); their size is [`Self::nv12`]'s.
    pub fn reconfigure<R: RegIo, S: Shared, H: Hal, M: Machine>(
        &mut self,
        core: &mut Core<R, S>,
        hal: &H,
        machine: &mut M,
        seq: &Sequence,
    ) -> Result<()> {
        if seq.bit_depth != 0 {
            return err("a picture above 8 bits", seq.bit_depth);
        }
        if !rules::picture_ok(seq.width, seq.height) {
            return err("the stream's picture size is one this decoder cannot take", seq.width << 16 | seq.height);
        }
        let [left, top, cw, ch] = seq.crop;
        if cw != 0 && (left > seq.width || cw > seq.width - left || top > seq.height || ch > seq.height - top) {
            return err("a crop outside the picture", cw << 16 | ch);
        }
        let h32 = seq.height.next_multiple_of(32).min(rules::MAX_SIDE);
        self.nv12 = rules::nv12(seq.width, h32).ok_or(Error { what: "no NV12 layout", value: seq.width })?;
        self.ubwc = rules::ubwc_size(seq.width, h32).ok_or(Error { what: "no UBWC size", value: seq.width })?;
        self.width = seq.width;
        self.height = h32;
        self.output_conf(core, hal)?;
        self.set_counts(core)?;
        // venus_helper_intbufs_realloc: every internal buffer but the persistent ones back, then asked for again.
        let scratch: Vec<(u32, u32, u32)> = self.internal.iter().copied().filter(|b| b.0 != buffer::PERSIST && b.0 != buffer::PERSIST_1).collect();
        for (kind, size, address) in &scratch {
            self.call(core, hal, &session::release_buffer(self.id, *kind, *size, *address), msg::SESSION_RELEASE_BUFFERS)?;
            machine.free(*address);
            self.used -= u64::from(*size);
        }
        self.internal.retain(|b| b.0 == buffer::PERSIST || b.0 == buffer::PERSIST_1);
        for kind in [buffer::SCRATCH, buffer::SCRATCH_1, buffer::SCRATCH_2] {
            self.set_internal(core, hal, machine, kind)?;
        }
        machine.load(self.macroblocks_per_second(), self.payload)?;
        // venus_helper_alloc_dpb_bufs: count_min of the reference type, each the UBWC size.
        let reqs = self.requirements(core, hal)?;
        let dpb = find(&reqs, buffer::OUTPUT).ok_or(Error { what: "no requirements for the reference pictures", value: 0 })?;
        if !rules::count_ok(dpb.count_min) {
            return err("a reference picture count this driver does not give", dpb.count_min);
        }
        for address in core::mem::take(&mut self.references) {
            machine.free(address);
            self.used -= u64::from(self.ubwc).next_multiple_of(4096);
        }
        for _ in 0..dpb.count_min {
            let address = self.alloc(machine, self.ubwc)?;
            self.references.push(address);
        }
        // The program's pictures, as `vdec_queue_setup` counts them for the capture queue after the event: at least `count_min` of the
        // outputs and the event's `buf_count` (`fw_min_cnt`, which vdec_event_change sets to 11 for VP9 "due to fw bug"), plus a few for
        // a client's own holding. The firmware's `count_actual` only says back the VB2_MAX_FRAME of `set_counts`: Linux does not make that
        // many, and nor does this.
        let out2 = find(&reqs, buffer::OUTPUT2).unwrap_or_default();
        let fw_min = if self.codec == session::codec::VP9 { 11 } else { seq.buffer_count };
        let needed = out2.count_min.max(dpb.count_min).max(fw_min) + PICTURE_SPARE;
        if !rules::count_ok(needed) {
            return err("a picture count this driver does not give", needed);
        }
        self.outputs = needed;
        core.send(&session::command(cmd::SESSION_CONTINUE, self.id), false)?;
        for (i, address) in self.references.iter().enumerate() {
            core.send(&session::fill_buffer(self.id, buffer::OUTPUT, self.ubwc, REFERENCE_TAG + i as u32, *address), false)?;
        }
        self.state = State::Decoding;
        Ok(())
    }

    /// `vdec_stop_output` for a seek (STREAMOFF on the bitstream queue): `SESSION_FLUSH(FLUSH_ALL)`, waited for, and the session
    /// is in Linux's SEEK state until [`Self::resume_after_seek`]. The firmware hands back every input, every reference picture
    /// and every program picture; their done messages may arrive before the flush answer and are kept as events.
    pub fn flush_for_seek<R: RegIo, S: Shared, H: Hal>(&mut self, core: &mut Core<R, S>, hal: &H) -> Result<()> {
        self.call(core, hal, &session::flush(self.id, session::FLUSH_ALL), msg::SESSION_FLUSH)?;
        self.state = State::Seek;
        Ok(())
    }

    /// `vdec_start_output`'s SEEK branch with the capture side streaming: the state is Decoding again and every reference picture
    /// is given to the firmware again, in the order [`Self::reconfigure`] gave them. A session that has seen no sequence event
    /// has no references and sends nothing here. The caller then gives its program pictures again with [`Self::fill`] and queues
    /// input from a key frame.
    pub fn resume_after_seek<R: RegIo, S: Shared>(&mut self, core: &Core<R, S>) -> Result<()> {
        if self.state != State::Seek {
            return err("a resume without a seek", self.state as u32);
        }
        self.state = State::Decoding;
        for (i, address) in self.references.iter().enumerate() {
            core.send(&session::fill_buffer(self.id, buffer::OUTPUT, self.ubwc, REFERENCE_TAG + i as u32, *address), false)?;
        }
        Ok(())
    }

    /// `vdec_stop_capture` in DRC and then `vdec_start_capture` from CAPTURE_SETUP: the output flush is waited for, the old
    /// reference pictures are freed once the firmware has answered (so none of them is with the firmware any more), and the
    /// session is configured for `seq` again. The caller makes new program pictures of the new [`Self::nv12`] size and gives them
    /// with [`Self::fill`].
    pub fn change_resolution<R: RegIo, S: Shared, H: Hal, M: Machine>(
        &mut self,
        core: &mut Core<R, S>,
        hal: &H,
        machine: &mut M,
        seq: &Sequence,
    ) -> Result<()> {
        if self.state != State::Drc {
            return err("a resolution change outside DRC", self.state as u32);
        }
        self.call(core, hal, &session::flush(self.id, session::FLUSH_OUTPUT), msg::SESSION_FLUSH)?;
        // venus_helper_free_dpb_bufs
        for address in core::mem::take(&mut self.references) {
            machine.free(address);
            self.used -= u64::from(self.ubwc).next_multiple_of(4096);
        }
        self.state = State::CaptureSetup;
        self.reconfigure(core, hal, machine, seq)
    }

    /// One compressed buffer to the firmware (`session_process_buf` for the input): `alloc_len` is the buffer's whole size.
    pub fn empty<R: RegIo, S: Shared, M: Machine>(&mut self, core: &Core<R, S>, machine: &mut M, input: &Input) -> Result<()> {
        if input.offset > input.alloc_len || input.filled_len > input.alloc_len - input.offset {
            return err("an input buffer's data past its end", input.filled_len);
        }
        // session_process_buf: the load is worked out again for every buffer queued, before it goes.
        if input.filled_len > self.payload {
            self.payload = input.filled_len;
            machine.load(self.macroblocks_per_second(), self.payload)?;
        }
        core.send(&session::empty_buffer(self.id, input), false)
    }

    /// The end of the stream (`vdec_decoder_cmd` STOP): an empty input with the EOS flag at Linux's dummy address.
    pub fn end_of_stream<R: RegIo, S: Shared>(&self, core: &Core<R, S>) -> Result<()> {
        let eos = Input { time_us: 0, flags: session::flag::EOS, offset: 0, alloc_len: 0, filled_len: 0, tag: 0, address: session::EOS_ADDRESS };
        core.send(&session::empty_buffer(self.id, &eos), false)
    }

    /// One linear picture buffer to the firmware, `tag` below [`REFERENCE_TAG`].
    pub fn fill<R: RegIo, S: Shared>(&self, core: &Core<R, S>, tag: u32, address: u32) -> Result<()> {
        if tag >= REFERENCE_TAG {
            return err("a picture tag among the reference pictures'", tag);
        }
        core.send(&session::fill_buffer(self.id, buffer::OUTPUT2, self.nv12.size, tag, address), false)
    }

    /// Whether events wait to be taken by [`Self::poll`] without reading the queues.
    pub fn has_events(&self) -> bool {
        !self.events.is_empty()
    }

    /// Whether the DRC flush of a sequence-changed event is still to be sent, as `vdec_event_change` sends it after the event.
    pub fn flush_pending(&self) -> bool {
        self.flush_output_pending
    }

    /// Reads what the firmware has sent, without waiting, and returns the next event.
    pub fn poll<R: RegIo, S: Shared, H: Hal>(&mut self, core: &mut Core<R, S>, hal: &H) -> Result<Option<Event>> {
        // The DRC flush goes out once the events read with it have been taken, as vdec_event_change sends it after the event.
        if self.events.is_empty() {
            self.send_pending_flush(core)?;
        }
        if let Some(e) = self.events.pop_front() {
            return Ok(Some(e));
        }
        let i = core.acknowledge();
        if i.watchdog() {
            return err("the firmware's watchdog fired", i.status);
        }
        let mut buf = [0u32; MAX_PACKET_WORDS as usize];
        let mut bad = None;
        core.messages(
            &mut buf,
            |m| {
                if let Err(e) = self.take(m) {
                    bad = Some(e);
                }
            },
            |text| hal.log(format_args!("venus: firmware: {}", core::str::from_utf8(text).unwrap_or("(not text)"))),
        )?;
        if let Some(e) = bad {
            return Err(e);
        }
        if core.is_dead() {
            return err("the firmware reported a system error", 0);
        }
        Ok(self.events.pop_front())
    }

    /// `vdec_stop_streaming` and `vdec_session_release`: flush, stop, release the resources and every internal buffer, end, then the
    /// memory, each after the answer that says the firmware let go of it. The program's own pictures are its to free after this.
    pub fn close<R: RegIo, S: Shared, H: Hal, M: Machine>(mut self, core: &mut Core<R, S>, hal: &H, machine: &mut M) -> Result<()> {
        let result = self.release(core, hal, machine);
        let mut aborted = false;
        if result.is_err() && !core.is_dead() {
            // vdec_session_release: on any error, abort.
            aborted = self.call(core, hal, &session::command(cmd::SYS_SESSION_ABORT, self.id), msg::SYS_SESSION_ABORT).is_ok();
        }
        if !core.is_dead() {
            // Linux frees here unconditionally because the IOMMU turns a late firmware write into a fault; this block has no
            // translation (its device addresses are physical), so a buffer the firmware never confirmed it let go of is kept
            // instead of freed, or a late write would corrupt whatever the memory is reused for.
            let mut give_back = |address| {
                if result.is_ok() || aborted {
                    machine.free(address);
                } else {
                    machine.quarantine(address);
                }
            };
            for address in core::mem::take(&mut self.references) {
                give_back(address);
            }
            for (_, _, address) in core::mem::take(&mut self.internal) {
                give_back(address);
            }
            // vdec_session_release: `venus_pm_load_scale` once the session is stopped, so its load is 0: the lowest clock and the
            // bandwidth floor.
            let _ = machine.load(0, self.payload);
        }
        let _ = machine.codec_power(false);
        result
    }

    fn release<R: RegIo, S: Shared, H: Hal, M: Machine>(&mut self, core: &mut Core<R, S>, hal: &H, _machine: &mut M) -> Result<()> {
        self.call(core, hal, &session::flush(self.id, session::FLUSH_ALL), msg::SESSION_FLUSH)?;
        self.call(core, hal, &session::command(cmd::SESSION_STOP, self.id), msg::SESSION_STOP)?;
        self.call(core, hal, &session::command(cmd::SESSION_RELEASE_RESOURCES, self.id), msg::SESSION_RELEASE_RESOURCES)?;
        for (kind, size, address) in self.internal.clone() {
            self.call(core, hal, &session::release_buffer(self.id, kind, size, address), msg::SESSION_RELEASE_BUFFERS)?;
        }
        self.call(core, hal, &session::command(cmd::SYS_SESSION_END, self.id), msg::SYS_SESSION_END)?;
        Ok(())
    }

    /// `vdec_output_conf` for 4xx: work mode, the linear second output, the first output off and the second on, UBWC references, the
    /// second output's size, the firmware's requirements checked against Linux's sizes, then both sizes.
    fn output_conf<R: RegIo, S: Shared, H: Hal>(&mut self, core: &mut Core<R, S>, hal: &H) -> Result<()> {
        let mbs = self.width.div_ceil(16) * self.height.div_ceil(16);
        let mode = if self.codec == session::codec::MPEG2 || mbs <= NUM_MBS_720P { 1 } else { 2 };
        self.set(core, property::WORK_MODE, &[mode])?;
        self.set(core, property::UNCOMPRESSED_FORMAT_SELECT, &[buffer::OUTPUT2, session::NV12])?;
        self.set(core, property::MULTI_STREAM, &[buffer::OUTPUT, 0])?;
        self.set(core, property::MULTI_STREAM, &[buffer::OUTPUT2, 1])?;
        self.set(core, property::UNCOMPRESSED_FORMAT_SELECT, &[buffer::OUTPUT, session::NV12_UBWC])?;
        self.set(core, property::FRAME_SIZE, &[buffer::OUTPUT2, self.width, self.height])?;
        let reqs = self.requirements(core, hal)?;
        for r in &reqs {
            hal.log(format_args!(
                "venus: session {}: {}x{} needs type {} size {} min {} actual {} align {}",
                self.id, self.width, self.height, r.kind, r.size, r.count_min, r.count_actual, r.alignment
            ));
        }
        if find(&reqs, buffer::OUTPUT).is_some_and(|r| r.size > self.ubwc) {
            return err("the firmware wants reference pictures larger than Linux's formula", find(&reqs, buffer::OUTPUT).unwrap().size);
        }
        if find(&reqs, buffer::OUTPUT2).is_some_and(|r| r.size > self.nv12.size) {
            return err("the firmware wants pictures larger than Linux's formula", find(&reqs, buffer::OUTPUT2).unwrap().size);
        }
        self.set(core, property::BUFFER_SIZE_ACTUAL, &[buffer::OUTPUT2, self.nv12.size])?;
        self.set(core, property::BUFFER_SIZE_ACTUAL, &[buffer::OUTPUT, self.ubwc])
    }

    /// `venus_helper_set_num_bufs(inst, inputs, VB2_MAX_FRAME, VB2_MAX_FRAME)`.
    fn set_counts<R: RegIo, S: Shared>(&self, core: &Core<R, S>) -> Result<()> {
        for (kind, count) in [(buffer::INPUT, self.inputs), (buffer::OUTPUT, rules::MAX_BUFFERS), (buffer::OUTPUT2, rules::MAX_BUFFERS)] {
            self.set(core, property::BUFFER_COUNT_ACTUAL, &[kind, count, count])?;
        }
        Ok(())
    }

    /// `intbufs_set_buffer`: the firmware's requirement for one internal kind, then that many buffers of that size, one packet each.
    fn set_internal<R: RegIo, S: Shared, H: Hal, M: Machine>(&mut self, core: &mut Core<R, S>, hal: &H, machine: &mut M, kind: u32) -> Result<()> {
        let reqs = self.requirements(core, hal)?;
        let Some(r) = find(&reqs, kind) else {
            return Ok(());
        };
        if r.size == 0 {
            return Ok(());
        }
        if !rules::count_ok(r.count_actual) {
            return err("an internal buffer count this driver does not give", r.count_actual);
        }
        for _ in 0..r.count_actual {
            let address = self.alloc(machine, r.size)?;
            self.internal.push((kind, r.size, address));
            core.send(&session::set_buffer(self.id, kind, r.size, address), false)?;
        }
        Ok(())
    }

    fn alloc<M: Machine>(&mut self, machine: &mut M, bytes: u32) -> Result<u32> {
        let size = u64::from(bytes).next_multiple_of(4096);
        if self.used + size > MEMORY_BUDGET {
            return err("more memory than a session may take", bytes);
        }
        let address = machine.alloc(bytes)?;
        self.used += size;
        Ok(address)
    }

    /// `load_per_instance`: macroblocks a picture times the frame rate, Linux's 30 when nothing says otherwise.
    fn macroblocks_per_second(&self) -> u32 {
        self.width.div_ceil(16) * self.height.div_ceil(16) * self.fps
    }

    fn set<R: RegIo, S: Shared>(&self, core: &Core<R, S>, property: u32, payload: &[u32]) -> Result<()> {
        core.send(&session::set_property(self.id, property, payload), false)
    }

    /// `venus_helper_get_bufreq`: the whole table, which Linux asks for again each time it needs one entry.
    fn requirements<R: RegIo, S: Shared, H: Hal>(&mut self, core: &mut Core<R, S>, hal: &H) -> Result<Vec<Requirement>> {
        let words = self.call(core, hal, &session::get_property(self.id, property::BUFFER_REQUIREMENTS), msg::SESSION_PROPERTY_INFO)?;
        session::requirements(&Message { words: &words }).map_err(|_| Error { what: "a requirements table that cannot be right", value: 0 })
    }

    /// A synchronous command: sent asking for an interrupt, then its answer, every other message of the session kept as an event.
    fn call<R: RegIo, S: Shared, H: Hal>(&mut self, core: &mut Core<R, S>, hal: &H, packet: &[u32], answer: u32) -> Result<Vec<u32>> {
        let sync = !matches!(packet[1], cmd::SESSION_SET_PROPERTY | cmd::SESSION_SET_BUFFERS | cmd::SESSION_CONTINUE);
        core.send(packet, sync)?;
        let mut buf = [0u32; MAX_PACKET_WORDS as usize];
        let mut found = Vec::new();
        let mut bad = None;
        let id = self.id;
        cpu::wait_for(
            core,
            hal,
            answer,
            cpu::ANSWER_TIMEOUT_US,
            &mut buf,
            |m| found = m.words.to_vec(),
            |m| {
                if m.word(2) == Some(id)
                    && let Err(e) = self.take(m)
                {
                    bad = Some(e);
                }
            },
        )?;
        if let Some(e) = bad {
            return Err(e);
        }
        self.send_pending_flush(core)?;
        if found.get(2) != Some(&id) {
            return err("an answer for another session", found.get(2).copied().unwrap_or(0));
        }
        // Every session answer has its error in the fourth word, but a property's answer has the count there.
        if answer != msg::SESSION_PROPERTY_INFO && found.get(3).is_some_and(|e| *e != 0) {
            return err("the firmware refused a session command", found[3]);
        }
        Ok(found)
    }

    /// `vdec_event_change`'s flush in DRC, sent once the sequence-changed event has been read: `hfi_session_flush(..,
    /// HFI_FLUSH_OUTPUT, false)`.
    fn send_pending_flush<R: RegIo, S: Shared>(&mut self, core: &Core<R, S>) -> Result<()> {
        if self.flush_output_pending {
            self.flush_output_pending = false;
            core.send(&session::flush(self.id, session::FLUSH_OUTPUT), false)?;
        }
        Ok(())
    }

    /// One message of this session, as an event.
    fn take(&mut self, m: &Message) -> Result<()> {
        let r = self.read(m);
        if r.is_err() {
            self.last_bad = m.words.to_vec();
        }
        r
    }

    fn read(&mut self, m: &Message) -> Result<()> {
        let bad = |what| {
            move |b: session::Bad| Error {
                what,
                value: match b {
                    session::Bad::Short(v) | session::Bad::Field(v) | session::Bad::Property(v) => v,
                },
            }
        };
        let e = match m.kind() {
            msg::SESSION_EMPTY_BUFFER => Event::InputDone(session::input_done(m).map_err(bad("an input-done that cannot be right"))?),
            msg::SESSION_FILL_BUFFER => {
                let o = session::output_done(m).map_err(bad("a picture-done that cannot be right"))?;
                if o.stream == 0 {
                    Event::Reference(o.tag)
                } else {
                    if !rules::filled_within(o.offset, o.filled_len, self.nv12.size) {
                        return err("a picture said to lie past its buffer", o.filled_len);
                    }
                    Event::Picture(o)
                }
            }
            msg::SESSION_FLUSH => Event::Flushed(m.word(4).unwrap_or(0)),
            msg::EVENT_NOTIFY => {
                let (_, id, data1, _) = session::event(m).map_err(bad("an event that cannot be right"))?;
                match id {
                    event::SEQUENCE_CHANGED => {
                        let q = session::sequence(m).map_err(bad("a sequence change that cannot be read"))?;
                        // vdec_event_change: INIT becomes CAPTURE_SETUP; DECODING or DRAIN becomes DRC, and the firmware is
                        // told to flush its output without waiting for the answer (`next_buf_last`).
                        match self.state {
                            State::Init => self.state = State::CaptureSetup,
                            State::Decoding | State::Drain => {
                                self.state = State::Drc;
                                self.flush_output_pending = true;
                            }
                            _ => {}
                        }
                        Event::Sequence(q)
                    }
                    event::RELEASE_BUFFER_REFERENCE => {
                        let address = m.word(6).ok_or(Error { what: "a release without its buffer", value: id })?;
                        Event::Released { address, tag: m.word(8).unwrap_or(u32::MAX) }
                    }
                    event::SESSION_ERROR if BENIGN_ERRORS.contains(&data1) => return Ok(()),
                    event::SESSION_ERROR => Event::Error(data1),
                    _ => return Ok(()),
                }
            }
            _ => return Ok(()),
        };
        self.events.push_back(e);
        Ok(())
    }
}
