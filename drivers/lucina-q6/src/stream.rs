//! One playback on the DSP, in the order Linux's sound stack sends it for a stream routed to tertiary MI2S: [`Playback::start`] gives the
//! first packet, each answer given to [`Playback::on_packet`] gives the next. The order of the steps is [`rules`], where Verus checks it.
//!
//! | Step | What sends it in Linux |
//! |---|---|
//! | [`Step::ClockOn`] | `dpcm_be_dai_startup`, `sc7180_qdsp_snd_startup`: `Q6AFE_LPASS_CLK_ID_TER_MI2S_IBIT`, 1.536 MHz |
//! | [`Step::PortConfig`], [`Step::PortStart`] | `dpcm_be_dai_prepare`, `q6afe_dai_prepare`, `q6afe_port_start` |
//! | [`Step::MapMemory`], [`Step::OpenWrite`] | `q6asm_dai_prepare` |
//! | [`Step::AdmOpen`], [`Step::Matrix`] | `q6routing_stream_open` |
//! | [`Step::MediaFormat`] | `q6asm_dai_prepare` |
//! | [`Playback::write`] from [`Step::Prepared`] | `q6asm_dai_ack`: the first periods go before the stream runs |
//! | [`Step::Run`] | `q6asm_dai_trigger` |
//!
//! ALSA prepares the back end before the front end, which is why the port comes first. [`Playback::stop`] sends what `q6asm_dai_trigger`
//! sends on stop, `ASM_DATA_CMD_EOS` with no wait for an answer, then unwinds as `dpcm_fe_dai_shutdown` does, back end first:
//! `q6afe_dai_shutdown`, `sc7180_qdsp_snd_shutdown`, then `q6asm_dai_close`, `q6routing_stream_close`. It carries on past a refused step:
//! the aim then is to leave nothing running.

use alloc::vec::Vec;

use crate::apr::{self, Packet};
use crate::rules;
pub use crate::rules::{DSP_MEMORY_LIMIT, Step, dsp_address};

// q6afe.c
const AFE_PORT_CMD_DEVICE_START: u32 = 0x0001_00E5;
const AFE_PORT_CMD_DEVICE_STOP: u32 = 0x0001_00E6;
const AFE_PORT_CMD_SET_PARAM_V2: u32 = 0x0001_00EF;
const AFE_SVC_CMD_SET_PARAM: u32 = 0x0001_00F3;
const AFE_MODULE_AUDIO_DEV_INTERFACE: u32 = 0x0001_020C;
const AFE_PARAM_ID_I2S_CONFIG: u32 = 0x0001_020D;
const AFE_MODULE_CLOCK_SET: u32 = 0x0001_028F;
const AFE_PARAM_ID_CLOCK_SET: u32 = 0x0001_0290;
const AFE_API_VERSION_I2S_CONFIG: u32 = 1;
const AFE_API_VERSION_CLOCK_SET: u32 = 1;
const AFE_PORT_I2S_STEREO: u16 = 1;
const AFE_PORT_CONFIG_I2S_WS_SRC_INTERNAL: u16 = 1;
const AFE_LINEAR_PCM_DATA: u16 = 0;
/// `sizeof(union afe_port_config)`, which `q6afe_port_start` sends whichever member is in use.
const AFE_PORT_CONFIG_SIZE: usize = 36;
/// `Q6AFE_LPASS_CLK_ATTRIBUTE_COUPLE_NO`.
const CLK_ATTRIBUTE_COUPLE_NO: u16 = 1;
/// `Q6AFE_LPASS_CLK_ROOT_DEFAULT`.
const CLK_ROOT_DEFAULT: u16 = 0;
// q6adm.c
const ADM_CMD_MATRIX_MAP_ROUTINGS_V5: u32 = 0x0001_0325;
const ADM_CMD_DEVICE_OPEN_V5: u32 = 0x0001_0326;
const ADM_CMD_DEVICE_CLOSE_V5: u32 = 0x0001_0327;
const ADM_CMDRSP_DEVICE_OPEN_V5: u32 = 0x0001_0329;
const ADM_MATRIX_ID_AUDIO_RX: u32 = 0;
const ADM_PATH_PLAYBACK: u16 = 1;
const NULL_COPP_TOPOLOGY: u32 = 0x0001_0312;
const INVALID_COPP_ID: u16 = 0xFF;
// q6asm.c
const ASM_STREAM_CMD_CLOSE: u32 = 0x0001_0BCD;
const ASM_DATA_CMD_EOS: u32 = 0x0001_0BDB;
const ASM_NULL_POPP_TOPOLOGY: u32 = 0x0001_0C68;
const ASM_CMD_SHARED_MEM_MAP_REGIONS: u32 = 0x0001_0D92;
const ASM_CMDRSP_SHARED_MEM_MAP_REGIONS: u32 = 0x0001_0D93;
const ASM_CMD_SHARED_MEM_UNMAP_REGIONS: u32 = 0x0001_0D94;
const ASM_DATA_CMD_MEDIA_FMT_UPDATE_V2: u32 = 0x0001_0D98;
const ASM_DATA_EVENT_WRITE_DONE_V2: u32 = 0x0001_0D99;
const ASM_MEDIA_FMT_MULTI_CHANNEL_PCM_V2: u32 = 0x0001_0DA5;
const ASM_SESSION_CMD_RUN_V2: u32 = 0x0001_0DAA;
const ASM_DATA_CMD_WRITE_V2: u32 = 0x0001_0DAB;
const ASM_STREAM_CMD_OPEN_WRITE_V3: u32 = 0x0001_0DB3;
const ADSP_MEMORY_MAP_SHMEM8_4K_POOL: u16 = 3;
const ASM_WRITE_TOKEN_LEN_SHIFT: u32 = 16;
/// `ASM_WRITE_TOKEN_MASK`.
const ASM_WRITE_TOKEN_MASK: u32 = 0xffff;
/// `SNDRV_PCM_STREAM_PLAYBACK`, the `dir` in a memory map's token.
const DIR_PLAYBACK: u32 = 0;
/// `PCM_CHANNEL_FL`, `PCM_CHANNEL_FR`.
const STEREO_MAP: [u8; 8] = [1, 2, 0, 0, 0, 0, 0, 0];

/// `TIMEOUT_MS` of `q6afe.c`.
const AFE_TIMEOUT_MS: u64 = 3000;
/// `TIMEOUT_MS` of `q6adm.c`.
const ADM_TIMEOUT_MS: u64 = 1000;
/// `5 * HZ` in `q6asm.c`.
const ASM_TIMEOUT_MS: u64 = 5000;

/// Where a stream goes and how much memory it has. All the audio is 16-bit stereo at 48 kHz: the speakers stay silent on
/// other formats, and `sc7180_qdsp_be_hw_params_fixup` fixes the port's rate and channels the same way.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Config {
    /// The AFE port's id.
    pub port_id: u16,
    /// Its index in `dt-bindings/sound/qcom,q6afe.h`, the token of AFE and ADM commands.
    pub port_index: u32,
    pub clock_id: u32,
    pub bit_clock_hz: u32,
    /// `AFE_PORT_I2S_SD0`.. of the data line.
    pub channel_mode: u16,
    /// The session; Linux's is the front end's number plus one.
    pub session: u8,
    pub stream: u8,
    /// Where the periods are, one after the other, as the DSP sees it: from [`dsp_address`].
    pub phys: u64,
    pub period_bytes: u32,
    pub periods: u32,
}

/// The speakers: tertiary MI2S on data line 1, from Samsung's calibration tables (`docs/research/juno-audio.md`).
pub const SPEAKERS: Config = Config {
    port_id: 0x1004,
    port_index: 20,
    clock_id: 0x104,
    bit_clock_hz: 1_536_000,
    channel_mode: 2,
    session: 1,
    stream: 1,
    phys: 0,
    period_bytes: 0,
    periods: 0,
};

pub const RATE: u32 = 48_000;
pub const CHANNELS: u16 = 2;
pub const BITS: u16 = 16;
pub const FRAME_BYTES: u32 = 4;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Event {
    /// Nothing to report.
    None,
    /// Everything is set up: [`Playback::write`] the first periods, then [`Playback::run`].
    Prepared,
    /// The DSP says the stream runs.
    Running,
    /// A period went through, as the DSP was asked. `slot` is free again.
    WriteDone { slot: u32 },
    /// The DSP says a period went through at an address that is not that slot's: `q6asm_stream_callback` drops it. It proves nothing.
    WrongWriteDone { slot: u32, low: u32, high: u32 },
    /// The stream is down.
    Stopped,
    /// The DSP refused a command while starting, or sent something nonsensical. [`Playback::stop`] clears up.
    Failed { opcode: u32, status: u32 },
}

/// What came of a packet.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Outcome {
    /// The next command to send, if any.
    pub send: Option<Vec<u8>>,
    pub event: Event,
}

#[derive(Debug)]
pub struct Playback {
    cfg: Config,
    step: Step,
    map_handle: u32,
    copp: Option<u16>,
    /// The last command sent and not yet answered, for the caller's timeout.
    waiting: bool,
    next_slot: u32,
    /// How far [`Self::start`]'s steps got, so that [`Self::stop`] undoes only those.
    port_started: bool,
    clock_on: bool,
    asm_open: bool,
    mapped: bool,
}

fn le16(out: &mut Vec<u8>, v: u16) {
    out.extend_from_slice(&v.to_le_bytes());
}

fn le32(out: &mut Vec<u8>, v: u32) {
    out.extend_from_slice(&v.to_le_bytes());
}

impl Playback {
    pub fn new(cfg: Config) -> Playback {
        Playback {
            cfg,
            step: Step::Idle,
            map_handle: 0,
            copp: None,
            waiting: false,
            next_slot: 0,
            port_started: false,
            clock_on: false,
            asm_open: false,
            mapped: false,
        }
    }

    pub fn step(&self) -> Step {
        self.step
    }

    /// A command is out, and the caller should time it.
    pub fn is_waiting(&self) -> bool {
        self.waiting
    }

    pub fn config(&self) -> &Config {
        &self.cfg
    }

    /// How long the command out may wait for its answer: each of Linux's files has its own limit.
    pub fn answer_timeout_ms(&self) -> u64 {
        match self.step {
            Step::ClockOn
            | Step::PortConfig
            | Step::PortStart
            | Step::PortStop
            | Step::ClockOff => AFE_TIMEOUT_MS,
            Step::AdmOpen | Step::Matrix | Step::AdmClose => ADM_TIMEOUT_MS,
            _ => ASM_TIMEOUT_MS,
        }
    }

    /// Sends the first command.
    pub fn start(&mut self) -> Vec<u8> {
        self.next(rules::after_start(Step::Idle))
            .expect("the first step has a command")
    }

    /// From [`Step::Prepared`], after the first [`Self::write`]s: the stream runs. `None` from anywhere else.
    pub fn run(&mut self) -> Option<Vec<u8>> {
        if self.step != Step::Prepared {
            return None;
        }
        self.next(rules::after_start(Step::Prepared))
    }

    /// Begins taking the stream down, from whatever it has reached: what to send now, in order. Empty: there was nothing to take down.
    pub fn stop(&mut self) -> Vec<Vec<u8>> {
        let mut out = Vec::new();
        if matches!(self.step, Step::Run | Step::Running) {
            out.push(self.asm(ASM_DATA_CMD_EOS, &[]));
        }
        out.extend(self.advance_stop(Step::PortStop));
        out
    }

    /// A command that was not answered in time: the DSP is gone or stuck, so nothing more goes to it.
    pub fn on_timeout(&mut self) {
        self.waiting = false;
        self.step = Step::Stopped;
    }

    fn asm_ports(&self) -> u16 {
        (u16::from(self.cfg.session) << 8) | u16::from(self.cfg.stream)
    }

    fn afe(&self, opcode: u32, payload: &[u8]) -> Vec<u8> {
        apr::build(apr::SVC_AFE, 0, 0, self.cfg.port_index, opcode, payload)
    }

    fn asm(&self, opcode: u32, payload: &[u8]) -> Vec<u8> {
        let ports = self.asm_ports();
        apr::build(
            apr::SVC_ASM,
            ports,
            ports,
            u32::from(self.cfg.session),
            opcode,
            payload,
        )
    }

    fn map_token(&self) -> u32 {
        (u32::from(self.cfg.session) << 8) | DIR_PLAYBACK
    }

    /// `q6afe_port_set_sysclk` for a clock id: through `AFE_SVC_CMD_SET_PARAM`, `struct afe_clk_set`. A rate of 0 turns it off.
    fn clock(&self, hz: u32) -> Vec<u8> {
        const CLK_SET_SIZE: usize = 20;
        const PDATA_SIZE: usize = 12;
        let mut p = Vec::new();
        le32(&mut p, (PDATA_SIZE + CLK_SET_SIZE) as u32); // payload_size
        le32(&mut p, 0);
        le32(&mut p, 0);
        le32(&mut p, 0);
        le32(&mut p, AFE_MODULE_CLOCK_SET);
        le32(&mut p, AFE_PARAM_ID_CLOCK_SET);
        le16(&mut p, CLK_SET_SIZE as u16);
        le16(&mut p, 0);
        le32(&mut p, AFE_API_VERSION_CLOCK_SET);
        le32(&mut p, self.cfg.clock_id);
        le32(&mut p, hz);
        le16(&mut p, CLK_ATTRIBUTE_COUPLE_NO);
        le16(&mut p, CLK_ROOT_DEFAULT);
        le32(&mut p, u32::from(hz != 0));
        self.afe(AFE_SVC_CMD_SET_PARAM, &p)
    }

    /// `q6afe_i2s_port_prepare` then the first half of `q6afe_port_start`: `AFE_PORT_CMD_SET_PARAM_V2` with the whole union.
    fn port_config(&self) -> Vec<u8> {
        const PDATA_SIZE: usize = 12;
        let mut p = Vec::new();
        le16(&mut p, self.cfg.port_id);
        le16(&mut p, (PDATA_SIZE + AFE_PORT_CONFIG_SIZE) as u16);
        le32(&mut p, 0);
        le32(&mut p, 0);
        le32(&mut p, 0);
        le32(&mut p, AFE_MODULE_AUDIO_DEV_INTERFACE);
        le32(&mut p, AFE_PARAM_ID_I2S_CONFIG);
        le16(&mut p, AFE_PORT_CONFIG_SIZE as u16);
        le16(&mut p, 0);
        let config_at = p.len();
        le32(&mut p, AFE_API_VERSION_I2S_CONFIG);
        le16(&mut p, BITS);
        le16(&mut p, self.cfg.channel_mode);
        le16(&mut p, AFE_PORT_I2S_STEREO);
        le16(&mut p, AFE_PORT_CONFIG_I2S_WS_SRC_INTERNAL);
        le32(&mut p, RATE);
        le16(&mut p, AFE_LINEAR_PCM_DATA);
        le16(&mut p, 0);
        p.resize(config_at + AFE_PORT_CONFIG_SIZE, 0);
        self.afe(AFE_PORT_CMD_SET_PARAM_V2, &p)
    }

    fn port_cmd(&self, opcode: u32) -> Vec<u8> {
        let mut p = Vec::new();
        le16(&mut p, self.cfg.port_id);
        le16(&mut p, 0);
        self.afe(opcode, &p)
    }

    /// `__q6asm_memory_map_regions`, one region, its size aligned to 4 KiB.
    fn map_memory(&self) -> Vec<u8> {
        let bytes = rules::map_bytes(self.cfg.period_bytes, self.cfg.periods).unwrap_or(0);
        let mut p = Vec::new();
        le16(&mut p, ADSP_MEMORY_MAP_SHMEM8_4K_POOL);
        le16(&mut p, 1);
        le32(&mut p, 0);
        le32(&mut p, self.cfg.phys as u32);
        le32(&mut p, (self.cfg.phys >> 32) as u32);
        le32(&mut p, bytes);
        apr::build(
            apr::SVC_ASM,
            0,
            0,
            self.map_token(),
            ASM_CMD_SHARED_MEM_MAP_REGIONS,
            &p,
        )
    }

    fn unmap_memory(&self) -> Vec<u8> {
        apr::build(
            apr::SVC_ASM,
            0,
            0,
            self.map_token(),
            ASM_CMD_SHARED_MEM_UNMAP_REGIONS,
            &self.map_handle.to_le_bytes(),
        )
    }

    fn open_write(&self) -> Vec<u8> {
        let mut p = Vec::new();
        le32(&mut p, 0); // mode_flags: legacy stream, not gapless
        le16(&mut p, 0); // sink: the device matrix
        le16(&mut p, BITS);
        le32(&mut p, ASM_NULL_POPP_TOPOLOGY);
        le32(&mut p, ASM_MEDIA_FMT_MULTI_CHANNEL_PCM_V2);
        self.asm(ASM_STREAM_CMD_OPEN_WRITE_V3, &p)
    }

    /// `q6adm_device_open`: `src_port` and `dest_port` are the AFE port, the token the port's index and copp 0.
    fn adm_open(&self) -> Vec<u8> {
        let mut p = Vec::new();
        le16(&mut p, 0); // flags: legacy session
        le16(&mut p, ADM_PATH_PLAYBACK);
        le16(&mut p, self.cfg.port_id);
        le16(&mut p, 0); // endpoint_id_2
        le32(&mut p, NULL_COPP_TOPOLOGY);
        le16(&mut p, CHANNELS);
        le16(&mut p, BITS);
        le32(&mut p, RATE);
        p.extend_from_slice(&STEREO_MAP);
        apr::build(
            apr::SVC_ADM,
            self.cfg.port_id,
            self.cfg.port_id,
            self.cfg.port_index << 16,
            ADM_CMD_DEVICE_OPEN_V5,
            &p,
        )
    }

    /// `q6adm_matrix_map`: its size counts each copp as four bytes though it fills in two.
    fn matrix(&self) -> Vec<u8> {
        let mut p = Vec::new();
        le32(&mut p, ADM_MATRIX_ID_AUDIO_RX);
        le32(&mut p, 1);
        le16(&mut p, u16::from(self.cfg.session));
        le16(&mut p, 1);
        le16(&mut p, self.copp.unwrap_or(0));
        le16(&mut p, 0);
        apr::build(apr::SVC_ADM, 0, 0, 0, ADM_CMD_MATRIX_MAP_ROUTINGS_V5, &p)
    }

    /// `q6adm_device_close`: `src_port` is the port's index here, not its id.
    fn adm_close(&self) -> Vec<u8> {
        apr::build(
            apr::SVC_ADM,
            self.cfg.port_index as u16,
            self.copp.unwrap_or(0),
            self.cfg.port_index << 16,
            ADM_CMD_DEVICE_CLOSE_V5,
            &[],
        )
    }

    fn media_format(&self) -> Vec<u8> {
        let mut p = Vec::new();
        le32(&mut p, 20); // fmt_blk_size: what follows
        le16(&mut p, CHANNELS);
        le16(&mut p, BITS);
        le32(&mut p, RATE);
        le16(&mut p, 1); // signed
        le16(&mut p, 0);
        p.extend_from_slice(&STEREO_MAP);
        self.asm(ASM_DATA_CMD_MEDIA_FMT_UPDATE_V2, &p)
    }

    fn run_command(&self) -> Vec<u8> {
        self.asm(ASM_SESSION_CMD_RUN_V2, &[0; 12])
    }

    /// `q6asm_write_async`: the next period, of `len` bytes. Says which slot the caller must have filled: slots go round in
    /// order, so call [`Self::next_slot`] first. `None` unless [`rules::may_write`].
    pub fn write(&mut self, len: u32) -> Option<Vec<u8>> {
        if !rules::may_write(self.step) {
            return None;
        }
        let slot = self.next_slot;
        let addr = self.slot_address(slot)?;
        let mut p = Vec::new();
        le32(&mut p, addr as u32);
        le32(&mut p, (addr >> 32) as u32);
        le32(&mut p, self.map_handle);
        le32(&mut p, len);
        le32(&mut p, slot);
        le32(&mut p, 0);
        le32(&mut p, 0);
        le32(&mut p, 0);
        let ports = self.asm_ports();
        let token = slot | (len << ASM_WRITE_TOKEN_LEN_SHIFT);
        self.next_slot = rules::next_slot(slot, self.cfg.periods);
        Some(apr::build(
            apr::SVC_ASM,
            ports,
            ports,
            token,
            ASM_DATA_CMD_WRITE_V2,
            &p,
        ))
    }

    fn slot_address(&self, slot: u32) -> Option<u64> {
        let offset = rules::slot_offset(slot, self.cfg.period_bytes, self.cfg.periods)?;
        Some(self.cfg.phys + u64::from(offset))
    }

    pub fn next_slot(&self) -> u32 {
        self.next_slot
    }

    fn next(&mut self, step: Step) -> Option<Vec<u8>> {
        self.step = step;
        self.waiting = true;
        let packet = match step {
            Step::ClockOn => self.clock(self.cfg.bit_clock_hz),
            Step::PortConfig => self.port_config(),
            Step::PortStart => self.port_cmd(AFE_PORT_CMD_DEVICE_START),
            Step::MapMemory => self.map_memory(),
            Step::OpenWrite => self.open_write(),
            Step::AdmOpen => self.adm_open(),
            Step::Matrix => self.matrix(),
            Step::MediaFormat => self.media_format(),
            Step::Run => self.run_command(),
            Step::Close => self.asm(ASM_STREAM_CMD_CLOSE, &[]),
            Step::Unmap => self.unmap_memory(),
            Step::AdmClose => self.adm_close(),
            Step::PortStop => self.port_cmd(AFE_PORT_CMD_DEVICE_STOP),
            Step::ClockOff => self.clock(0),
            Step::Idle | Step::Prepared | Step::Running | Step::Stopped => {
                self.waiting = false;
                return None;
            }
        };
        Some(packet)
    }

    /// Whether a step of the way down has anything to undo.
    fn stop_step_due(&self, step: Step) -> bool {
        match step {
            Step::PortStop => self.port_started,
            Step::ClockOff => self.clock_on,
            Step::Close => self.asm_open,
            Step::Unmap => self.mapped,
            Step::AdmClose => self.copp.is_some(),
            _ => false,
        }
    }

    /// The first step of the way down, from `from`, that has something to undo.
    fn advance_stop(&mut self, from: Step) -> Option<Vec<u8>> {
        let mut step = from;
        while step != Step::Stopped {
            if self.stop_step_due(step) {
                return self.next(step);
            }
            step = rules::after_stop(step);
        }
        self.step = Step::Stopped;
        self.waiting = false;
        None
    }

    fn after_stop_step(&mut self, step: Step) -> Outcome {
        match step {
            Step::PortStop => self.port_started = false,
            Step::ClockOff => self.clock_on = false,
            Step::Close => self.asm_open = false,
            Step::Unmap => self.mapped = false,
            _ => self.copp = None,
        }
        let send = self.advance_stop(rules::after_stop(step));
        let event = if self.step == Step::Stopped {
            Event::Stopped
        } else {
            Event::None
        };
        Outcome { send, event }
    }

    /// `q6asm_stream_callback`: for this session, and for the address that slot was given. The slot comes from the token.
    fn write_done(&self, packet: &Packet) -> Event {
        let slot = packet.token & ASM_WRITE_TOKEN_MASK;
        let (low, high) = packet.result().unwrap_or((0, 0));
        match self.slot_address(slot) {
            Some(addr) if addr as u32 == low && (addr >> 32) as u32 == high => {
                Event::WriteDone { slot }
            }
            _ => Event::WrongWriteDone { slot, low, high },
        }
    }

    /// `q6asm_srvc_callback` sends what has a session in `dest_port` to that session; `q6afe_callback` finds the port by the token and
    /// `q6adm_callback` the port and copp. An answer for someone else is not an answer.
    fn answer_is_ours(&self, step: Step, packet: &Packet) -> bool {
        let session = (packet.dest_port >> 8) as u8;
        match step {
            Step::ClockOn
            | Step::ClockOff
            | Step::PortConfig
            | Step::PortStart
            | Step::PortStop => packet.token == self.cfg.port_index,
            Step::MapMemory | Step::Unmap => session == 0 && packet.token == self.map_token(),
            Step::AdmOpen | Step::AdmClose => packet.token == self.cfg.port_index << 16,
            Step::Matrix => true,
            _ => session == self.cfg.session,
        }
    }

    /// An answer from the DSP. Anything that is not an answer to the command out is ignored.
    pub fn on_packet(&mut self, packet: &Packet) -> Outcome {
        let quiet = Outcome {
            send: None,
            event: Event::None,
        };
        let step = self.step;
        let (expected_svc, opcode, rsp) = match step {
            Step::ClockOn | Step::ClockOff => (apr::SVC_AFE, AFE_SVC_CMD_SET_PARAM, 0),
            Step::PortConfig => (apr::SVC_AFE, AFE_PORT_CMD_SET_PARAM_V2, 0),
            Step::PortStart => (apr::SVC_AFE, AFE_PORT_CMD_DEVICE_START, 0),
            Step::PortStop => (apr::SVC_AFE, AFE_PORT_CMD_DEVICE_STOP, 0),
            Step::MapMemory => (
                apr::SVC_ASM,
                ASM_CMD_SHARED_MEM_MAP_REGIONS,
                ASM_CMDRSP_SHARED_MEM_MAP_REGIONS,
            ),
            Step::Unmap => (apr::SVC_ASM, ASM_CMD_SHARED_MEM_UNMAP_REGIONS, 0),
            Step::OpenWrite => (apr::SVC_ASM, ASM_STREAM_CMD_OPEN_WRITE_V3, 0),
            Step::MediaFormat => (apr::SVC_ASM, ASM_DATA_CMD_MEDIA_FMT_UPDATE_V2, 0),
            Step::Run => (apr::SVC_ASM, ASM_SESSION_CMD_RUN_V2, 0),
            Step::Close => (apr::SVC_ASM, ASM_STREAM_CMD_CLOSE, 0),
            Step::AdmOpen => (
                apr::SVC_ADM,
                ADM_CMD_DEVICE_OPEN_V5,
                ADM_CMDRSP_DEVICE_OPEN_V5,
            ),
            Step::Matrix => (apr::SVC_ADM, ADM_CMD_MATRIX_MAP_ROUTINGS_V5, 0),
            Step::AdmClose => (apr::SVC_ADM, ADM_CMD_DEVICE_CLOSE_V5, 0),
            Step::Running | Step::Prepared | Step::Idle | Step::Stopped => (apr::SVC_ASM, 0, 0),
        };
        if packet.opcode == ASM_DATA_EVENT_WRITE_DONE_V2 {
            if packet.src_svc != apr::SVC_ASM {
                return quiet;
            }
            if (packet.dest_port >> 8) as u8 != self.cfg.session {
                return quiet;
            }
            return Outcome {
                send: None,
                event: self.write_done(packet),
            };
        }
        if packet.src_svc != expected_svc
            || matches!(step, Step::Running | Step::Prepared)
            || !self.answer_is_ours(step, packet)
        {
            return quiet;
        }
        let (answered, status) = if packet.opcode == apr::BASIC_RSP_RESULT {
            match packet.result() {
                Some((op, status)) if op == opcode => (true, status),
                _ => return quiet,
            }
        } else if rsp != 0 && packet.opcode == rsp {
            (true, 0)
        } else {
            return quiet;
        };
        if !answered {
            return quiet;
        }
        self.waiting = false;
        if matches!(
            step,
            Step::PortStop | Step::ClockOff | Step::Close | Step::Unmap | Step::AdmClose
        ) {
            return self.after_stop_step(step);
        }
        if status != 0 {
            return Outcome {
                send: None,
                event: Event::Failed { opcode, status },
            };
        }
        match step {
            Step::ClockOn => self.clock_on = true,
            Step::PortStart => self.port_started = true,
            Step::MapMemory => {
                // `port->mem_map_handle = result->opcode`: the handle is the first word.
                let Some((handle, _)) = packet.result().or_else(|| first_word(packet)) else {
                    return Outcome {
                        send: None,
                        event: Event::Failed { opcode, status: 0 },
                    };
                };
                self.map_handle = handle;
                self.mapped = true;
            }
            Step::OpenWrite => self.asm_open = true,
            Step::AdmOpen => {
                // `struct adm_cmd_rsp_device_open_v5`: status, copp id.
                let Some((status, id)) = packet.result() else {
                    return Outcome {
                        send: None,
                        event: Event::Failed { opcode, status: 0 },
                    };
                };
                let id = id as u16;
                if status != 0 || id == INVALID_COPP_ID {
                    return Outcome {
                        send: None,
                        event: Event::Failed { opcode, status },
                    };
                }
                self.copp = Some(id);
            }
            _ => {}
        }
        let next = rules::after_start(step);
        let event = match next {
            Step::Prepared => Event::Prepared,
            Step::Running => Event::Running,
            _ => Event::None,
        };
        Outcome {
            send: self.next(next),
            event,
        }
    }
}

fn first_word(packet: &Packet) -> Option<(u32, u32)> {
    let word = packet.payload.get(..4)?;
    Some((u32::from_le_bytes(word.try_into().ok()?), 0))
}
