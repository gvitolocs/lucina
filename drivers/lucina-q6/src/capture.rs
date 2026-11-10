//! One capture from the internal microphone, in the order Linux's sound stack sends it for a stream from `VA_CODEC_DMA_TX_2`, as a state
//! machine like [`crate::stream`]: [`Capture::start`] gives the first packet, each answer given to [`Capture::on_packet`] gives the next. The
//! order is [`rules::capture_after_start`] and [`rules::capture_after_stop`], which Verus checks.
//!
//! | Step | What sends it in Linux |
//! |---|---|
//! | [`CaptureStep::PortConfig`], [`CaptureStep::PortStart`] | `dpcm_be_dai_prepare`, `q6afe_dai_prepare`: `q6afe_cdc_dma_port_prepare`, `q6afe_port_start` |
//! | [`CaptureStep::MapMemory`], [`CaptureStep::OpenRead`] | `q6asm_dai_prepare` |
//! | [`CaptureStep::AdmOpen`], [`CaptureStep::Matrix`] | `q6routing_stream_open`, with `ADM_PATH_LIVE_REC` |
//! | [`CaptureStep::EncoderConfig`] | `q6asm_dai_prepare`: `q6asm_enc_cfg_blk_pcm_format_support` |
//! | [`Capture::read`], from [`CaptureStep::Prepared`] | `q6asm_dai_prepare`: one `q6asm_read` for each period |
//! | [`CaptureStep::Run`] | `q6asm_dai_trigger` |
//!
//! [`Capture::stop`] sends `ASM_DATA_CMD_EOS` with no wait, then as `dpcm_fe_dai_shutdown`: the port, then the stream, its memory and the
//! ADM copp. There is no clock here: the port is the DSP's codec DMA.
//!
//! Before the port, once, what Linux's VA macro and clock controller take at probe and hold for as long as they are bound: the core-hardware
//! votes for the macro and the digital codec blocks (`q6afe_vote_lpass_core_hw`, 3 and 4) and the clock their device tree names (`0x3a`,
//! AFE clock `0x30D`, 19.2 MHz), then the macro's own `mclk` as Linux's boards give it (`0x30C`, 19.2 MHz). The DSP alone, as Windows seems to ask it, gave no data on the laptop (section 13). No register is written.

use alloc::vec::Vec;

use crate::apr::{self, Packet};
use crate::rules;
pub use crate::rules::CaptureStep;

// q6afe.c
const AFE_PORT_CMD_DEVICE_START: u32 = 0x0001_00E5;
const AFE_PORT_CMD_DEVICE_STOP: u32 = 0x0001_00E6;
const AFE_PORT_CMD_SET_PARAM_V2: u32 = 0x0001_00EF;
const AFE_MODULE_AUDIO_DEV_INTERFACE: u32 = 0x0001_020C;
const AFE_PARAM_ID_CODEC_DMA_CONFIG: u32 = 0x0001_02B8;
const AFE_API_VERSION_CODEC_DMA_CONFIG: u32 = 1;
/// `sizeof(union afe_port_config)`, which `q6afe_port_start` sends whichever member is in use.
const AFE_PORT_CONFIG_SIZE: usize = 36;
const AFE_CMD_REMOTE_LPASS_CORE_HW_VOTE_REQUEST: u32 = 0x0001_00F4;
const AFE_CMD_RSP_REMOTE_LPASS_CORE_HW_VOTE_REQUEST: u32 = 0x0001_00F5;
const AFE_SVC_CMD_SET_PARAM: u32 = 0x0001_00F3;
const AFE_MODULE_CLOCK_SET: u32 = 0x0001_028F;
const AFE_PARAM_ID_CLOCK_SET: u32 = 0x0001_0290;
/// `AFE_CLK_TOKEN`: a clock set for no port.
const AFE_CLK_TOKEN: u32 = 1024;
/// `Q6AFE_LPASS_CORE_HW_MACRO_BLOCK`, `Q6AFE_LPASS_CORE_HW_DCODEC_BLOCK`.
const HW_MACRO_BLOCK: u32 = 3;
const HW_DCODEC_BLOCK: u32 = 4;
/// `LPASS_CLK_ID_TX_CORE_NPL_MCLK`, the `bus` clock (`q6afecc 0x3a`) of the device tree's clock controller.
const BUS_CLOCK_ID: u32 = 0x30D;
/// `LPASS_CLK_ID_TX_CORE_MCLK`, the VA macro's `mclk` on Linux's boards (`sm8250.dtsi`: `<&q6afecc LPASS_CLK_ID_TX_CORE_MCLK ..>`). Without
/// it the macro's first register write gave the laptop an SError (generation 24, 2026-11-04).
const MCLK_ID: u32 = 0x30C;
const CLOCK_HZ: u32 = 19_200_000;
// q6adm.c
const ADM_CMD_MATRIX_MAP_ROUTINGS_V5: u32 = 0x0001_0325;
const ADM_CMD_DEVICE_OPEN_V5: u32 = 0x0001_0326;
const ADM_CMD_DEVICE_CLOSE_V5: u32 = 0x0001_0327;
const ADM_CMDRSP_DEVICE_OPEN_V5: u32 = 0x0001_0329;
const ADM_MATRIX_ID_AUDIO_TX: u32 = 1;
const ADM_PATH_LIVE_REC: u16 = 2;
const NULL_COPP_TOPOLOGY: u32 = 0x0001_0312;
const INVALID_COPP_ID: u16 = 0xFF;
// q6asm.c
const ASM_STREAM_CMD_CLOSE: u32 = 0x0001_0BCD;
const ASM_DATA_CMD_EOS: u32 = 0x0001_0BDB;
const ASM_NULL_POPP_TOPOLOGY: u32 = 0x0001_0C68;
const ASM_STREAM_CMD_SET_ENCDEC_PARAM: u32 = 0x0001_0C10;
const ASM_CMD_SHARED_MEM_MAP_REGIONS: u32 = 0x0001_0D92;
const ASM_CMDRSP_SHARED_MEM_MAP_REGIONS: u32 = 0x0001_0D93;
const ASM_CMD_SHARED_MEM_UNMAP_REGIONS: u32 = 0x0001_0D94;
const ASM_DATA_EVENT_READ_DONE_V2: u32 = 0x0001_0D9A;
const ASM_PARAM_ID_ENCDEC_ENC_CFG_BLK_V2: u32 = 0x0001_0DA3;
const ASM_MEDIA_FMT_MULTI_CHANNEL_PCM_V2: u32 = 0x0001_0DA5;
const ASM_SESSION_CMD_RUN_V2: u32 = 0x0001_0DAA;
const ASM_DATA_CMD_READ_V2: u32 = 0x0001_0DAC;
const ASM_STREAM_CMD_OPEN_READ_V3: u32 = 0x0001_0DB4;
const ADSP_MEMORY_MAP_SHMEM8_4K_POOL: u16 = 3;
/// `SNDRV_PCM_STREAM_CAPTURE`, the `dir` in a memory map's token.
const DIR_CAPTURE: u32 = 1;
/// `q6dsp_map_channels(.., 1)`: `PCM_CHANNEL_FC`.
const MONO_MAP: [u8; 8] = [3, 0, 0, 0, 0, 0, 0, 0];

/// `TIMEOUT_MS` of `q6afe.c` and `q6adm.c`, `5 * HZ` of `q6asm.c`.
const AFE_TIMEOUT_MS: u64 = 3000;
const ADM_TIMEOUT_MS: u64 = 1000;
const ASM_TIMEOUT_MS: u64 = 5000;

pub const RATE: u32 = 48_000;
pub const CHANNELS: u16 = 1;
pub const BITS: u16 = 16;
pub const FRAME_BYTES: u32 = 2;

/// Where a capture comes from and where it goes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Config {
    pub port_id: u16,
    /// Its index in `dt-bindings/sound/qcom,q6dsp-lpass-ports.h`, the token of AFE and ADM commands.
    pub port_index: u32,
    /// The ASM session: Linux's is the front end's number plus one. Playback has 1.
    pub session: u8,
    pub stream: u8,
    /// Where the periods are, one after the other, as the DSP sees it: from [`rules::dsp_address`].
    pub phys: u64,
    pub period_bytes: u32,
    pub periods: u32,
    /// `active_channels_mask` of the codec DMA port: a bit per lane, and the VA macro's decimator n comes in on lane n. Windows on this
    /// laptop computes it as 1 << the decimator's place in the port's list of the four VA decimators (`qcauddev7180.sys`); Linux's
    /// fallback for a mono port is 1.
    pub channel_mask: u16,
}

/// The internal microphone: ACDB device `0x0b` of this laptop's Windows tables, `VA_CODEC_DMA_TX_2`.
pub const INTERNAL_MIC: Config = Config {
    port_id: 0xB025,
    port_index: 112,
    session: 2,
    stream: 1,
    phys: 0,
    period_bytes: 0,
    periods: 0,
    channel_mask: 1,
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Event {
    None,
    /// Set up: [`Capture::read`] every period, then [`Capture::run`].
    Prepared,
    Running,
    /// A period came in, at the address it was given: its bytes are in `slot`, and the caller gives it back with [`Capture::read`].
    ReadDone {
        slot: u32,
    },
    /// One that names another address: `q6asm_stream_callback` drops it.
    WrongReadDone {
        slot: u32,
        low: u32,
        high: u32,
    },
    Stopped,
    Failed {
        opcode: u32,
        status: u32,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Outcome {
    pub send: Option<Vec<u8>>,
    pub event: Event,
}

#[derive(Debug)]
pub struct Capture {
    cfg: Config,
    step: CaptureStep,
    /// The votes and the clock were taken by an earlier capture and are held.
    votes_held: bool,
    /// Only the VA macro's `mclk` (`0x30C`), no bus clock: what Windows asks for a VA DMIC recording, clock node 10 alone
    /// (`qcauddev7180.sys` `(address not published)`, juno-audio.md section 28).
    mclk_only: bool,
    map_handle: u32,
    copp: Option<u16>,
    waiting: bool,
    eos_sent: bool,
    next_slot: u32,
    port_started: bool,
    asm_open: bool,
    mapped: bool,
}

fn le16(out: &mut Vec<u8>, v: u16) {
    out.extend_from_slice(&v.to_le_bytes());
}

fn le32(out: &mut Vec<u8>, v: u32) {
    out.extend_from_slice(&v.to_le_bytes());
}

impl Capture {
    pub fn new(cfg: Config) -> Capture {
        Capture {
            cfg,
            step: CaptureStep::Idle,
            votes_held: false,
            mclk_only: false,
            map_handle: 0,
            copp: None,
            waiting: false,
            eos_sent: false,
            next_slot: 0,
            port_started: false,
            asm_open: false,
            mapped: false,
        }
    }

    /// For a capture after the first: the votes and the clock are held already.
    pub fn with_votes_held(cfg: Config) -> Capture {
        let mut capture = Capture::new(cfg);
        capture.votes_held = true;
        capture
    }

    /// Whether the votes and the clock have been answered: they are held from then on.
    pub fn votes_held(&self) -> bool {
        self.votes_held
    }

    /// The clocks as Windows asks them for this laptop's VA DMIC: `0x30C` alone, the bus clock step skipped.
    pub fn with_mclk_only(mut self) -> Capture {
        self.mclk_only = true;
        self
    }

    pub fn mclk_only(&self) -> bool {
        self.mclk_only
    }

    pub fn step(&self) -> CaptureStep {
        self.step
    }

    pub fn is_waiting(&self) -> bool {
        self.waiting
    }

    pub fn config(&self) -> &Config {
        &self.cfg
    }

    pub fn answer_timeout_ms(&self) -> u64 {
        match self.step {
            CaptureStep::VoteMacro | CaptureStep::VoteDcodec | CaptureStep::BusClock | CaptureStep::Mclk => {
                AFE_TIMEOUT_MS
            }
            CaptureStep::PortConfig | CaptureStep::PortStart | CaptureStep::PortStop => {
                AFE_TIMEOUT_MS
            }
            CaptureStep::AdmOpen | CaptureStep::Matrix | CaptureStep::AdmClose => ADM_TIMEOUT_MS,
            _ => ASM_TIMEOUT_MS,
        }
    }

    pub fn start(&mut self) -> Vec<u8> {
        let mut first = rules::capture_after_start(CaptureStep::Idle);
        if self.votes_held {
            while matches!(
                first,
                CaptureStep::VoteMacro | CaptureStep::VoteDcodec | CaptureStep::BusClock | CaptureStep::Mclk
            ) {
                first = rules::capture_after_start(first);
            }
        }
        self.next(first).expect("the first step has a command")
    }

    /// `q6afe_vote_lpass_core_hw`: the block, and the client's name cut to 8 bytes by `strscpy`, as Linux's are.
    fn vote(&self, block: u32) -> Vec<u8> {
        let mut p = Vec::new();
        le32(&mut p, block);
        p.extend_from_slice(b"LPASS_H\0");
        apr::build(
            apr::SVC_AFE,
            0,
            0,
            block,
            AFE_CMD_REMOTE_LPASS_CORE_HW_VOTE_REQUEST,
            &p,
        )
    }

    /// `q6afe_set_lpass_clock`: a clock for no port.
    fn lpass_clock(&self, id: u32) -> Vec<u8> {
        const CLK_SET_SIZE: usize = 20;
        const PDATA_SIZE: usize = 12;
        let mut p = Vec::new();
        le32(&mut p, (PDATA_SIZE + CLK_SET_SIZE) as u32);
        le32(&mut p, 0);
        le32(&mut p, 0);
        le32(&mut p, 0);
        le32(&mut p, AFE_MODULE_CLOCK_SET);
        le32(&mut p, AFE_PARAM_ID_CLOCK_SET);
        le16(&mut p, CLK_SET_SIZE as u16);
        le16(&mut p, 0);
        le32(&mut p, 1); // clk_set_minor_version
        le32(&mut p, id);
        le32(&mut p, CLOCK_HZ);
        le16(&mut p, 1); // Q6AFE_LPASS_CLK_ATTRIBUTE_COUPLE_NO
        le16(&mut p, 0); // root
        le32(&mut p, 1); // enable
        apr::build(apr::SVC_AFE, 0, 0, AFE_CLK_TOKEN, AFE_SVC_CMD_SET_PARAM, &p)
    }

    /// From [`CaptureStep::Prepared`], after a [`Self::read`] for every period.
    pub fn run(&mut self) -> Option<Vec<u8>> {
        if self.step != CaptureStep::Prepared {
            return None;
        }
        self.next(rules::capture_after_start(CaptureStep::Prepared))
    }

    /// `q6asm_dai_trigger` STOP: `ASM_DATA_CMD_EOS` with no wait, once, while it runs. The VA macro stops after it and before
    /// [`Self::stop`], which does not send it again.
    pub fn eos(&mut self) -> Option<Vec<u8>> {
        if self.eos_sent || !matches!(self.step, CaptureStep::Run | CaptureStep::Running) {
            return None;
        }
        self.eos_sent = true;
        Some(self.asm(ASM_DATA_CMD_EOS, &[]))
    }

    /// What to send now to end it, in order. Empty: nothing was started.
    pub fn stop(&mut self) -> Vec<Vec<u8>> {
        let mut out = Vec::new();
        out.extend(self.eos());
        out.extend(self.advance_stop(CaptureStep::PortStop));
        out
    }

    pub fn on_timeout(&mut self) {
        self.waiting = false;
        self.step = CaptureStep::Stopped;
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
        (u32::from(self.cfg.session) << 8) | DIR_CAPTURE
    }

    /// `q6afe_cdc_dma_port_prepare` then the first half of `q6afe_port_start`: the whole union, the codec DMA member first.
    fn port_config(&self) -> Vec<u8> {
        const PDATA_SIZE: usize = 12;
        let mut p = Vec::new();
        le16(&mut p, self.cfg.port_id);
        le16(&mut p, (PDATA_SIZE + AFE_PORT_CONFIG_SIZE) as u16);
        le32(&mut p, 0);
        le32(&mut p, 0);
        le32(&mut p, 0);
        le32(&mut p, AFE_MODULE_AUDIO_DEV_INTERFACE);
        le32(&mut p, AFE_PARAM_ID_CODEC_DMA_CONFIG);
        le16(&mut p, AFE_PORT_CONFIG_SIZE as u16);
        le16(&mut p, 0);
        let config_at = p.len();
        le32(&mut p, AFE_API_VERSION_CODEC_DMA_CONFIG);
        le32(&mut p, RATE);
        le16(&mut p, BITS);
        le16(&mut p, 0); // data_format
        le16(&mut p, CHANNELS);
        le16(&mut p, self.cfg.channel_mask); // active_channels_mask
        p.resize(config_at + AFE_PORT_CONFIG_SIZE, 0);
        self.afe(AFE_PORT_CMD_SET_PARAM_V2, &p)
    }

    fn port_cmd(&self, opcode: u32) -> Vec<u8> {
        let mut p = Vec::new();
        le16(&mut p, self.cfg.port_id);
        le16(&mut p, 0);
        self.afe(opcode, &p)
    }

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

    /// `__q6asm_open_read`: legacy session, from the device matrix, no pre-processing.
    fn open_read(&self) -> Vec<u8> {
        let mut p = Vec::new();
        le32(&mut p, 0); // mode_flags
        le32(&mut p, 0); // src_endpointype: the matrix
        le32(&mut p, ASM_NULL_POPP_TOPOLOGY);
        le32(&mut p, ASM_MEDIA_FMT_MULTI_CHANNEL_PCM_V2);
        le16(&mut p, BITS);
        le16(&mut p, 0);
        self.asm(ASM_STREAM_CMD_OPEN_READ_V3, &p)
    }

    fn adm_open(&self) -> Vec<u8> {
        let mut p = Vec::new();
        le16(&mut p, 0); // flags: legacy session
        le16(&mut p, ADM_PATH_LIVE_REC);
        le16(&mut p, self.cfg.port_id);
        le16(&mut p, 0);
        le32(&mut p, NULL_COPP_TOPOLOGY);
        le16(&mut p, CHANNELS);
        le16(&mut p, BITS);
        le32(&mut p, RATE);
        p.extend_from_slice(&MONO_MAP);
        apr::build(
            apr::SVC_ADM,
            self.cfg.port_id,
            self.cfg.port_id,
            self.cfg.port_index << 16,
            ADM_CMD_DEVICE_OPEN_V5,
            &p,
        )
    }

    fn matrix(&self) -> Vec<u8> {
        let mut p = Vec::new();
        le32(&mut p, ADM_MATRIX_ID_AUDIO_TX);
        le32(&mut p, 1);
        le16(&mut p, u16::from(self.cfg.session));
        le16(&mut p, 1);
        le16(&mut p, self.copp.unwrap_or(0));
        le16(&mut p, 0);
        apr::build(apr::SVC_ADM, 0, 0, 0, ADM_CMD_MATRIX_MAP_ROUTINGS_V5, &p)
    }

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

    /// `q6asm_enc_cfg_blk_pcm_format_support`.
    fn encoder_config(&self) -> Vec<u8> {
        let mut p = Vec::new();
        le32(&mut p, ASM_PARAM_ID_ENCDEC_ENC_CFG_BLK_V2);
        le32(&mut p, 28); // param_size
        le32(&mut p, 0); // frames_per_buf
        le32(&mut p, 20); // enc_cfg_blk_size
        le16(&mut p, CHANNELS);
        le16(&mut p, BITS);
        le32(&mut p, RATE);
        le16(&mut p, 1); // signed
        le16(&mut p, 0);
        p.extend_from_slice(&MONO_MAP);
        self.asm(ASM_STREAM_CMD_SET_ENCDEC_PARAM, &p)
    }

    fn run_command(&self) -> Vec<u8> {
        self.asm(ASM_SESSION_CMD_RUN_V2, &[0; 12])
    }

    /// `q6asm_read`: a period for the DSP to fill, the next slot in turn. `None` unless [`rules::capture_may_read`].
    pub fn read(&mut self) -> Option<Vec<u8>> {
        if !rules::capture_may_read(self.step) {
            return None;
        }
        let slot = self.next_slot;
        let addr = self.slot_address(slot)?;
        let mut p = Vec::new();
        le32(&mut p, addr as u32);
        le32(&mut p, (addr >> 32) as u32);
        le32(&mut p, self.map_handle);
        le32(&mut p, self.cfg.period_bytes);
        le32(&mut p, slot);
        self.next_slot = rules::next_slot(slot, self.cfg.periods);
        let ports = self.asm_ports();
        Some(apr::build(
            apr::SVC_ASM,
            ports,
            ports,
            slot,
            ASM_DATA_CMD_READ_V2,
            &p,
        ))
    }

    pub fn next_slot(&self) -> u32 {
        self.next_slot
    }

    /// Where a slot's period starts in the buffer, in bytes.
    pub fn slot_offset(&self, slot: u32) -> Option<u32> {
        rules::slot_offset(slot, self.cfg.period_bytes, self.cfg.periods)
    }

    fn slot_address(&self, slot: u32) -> Option<u64> {
        Some(self.cfg.phys + u64::from(self.slot_offset(slot)?))
    }

    fn next(&mut self, step: CaptureStep) -> Option<Vec<u8>> {
        let sent = rules::capture_sent_step(step, self.mclk_only);
        if sent != step {
            return self.next(sent);
        }
        self.step = step;
        self.waiting = true;
        let packet = match step {
            CaptureStep::VoteMacro => self.vote(HW_MACRO_BLOCK),
            CaptureStep::VoteDcodec => self.vote(HW_DCODEC_BLOCK),
            CaptureStep::BusClock => self.lpass_clock(BUS_CLOCK_ID),
            CaptureStep::Mclk => self.lpass_clock(MCLK_ID),
            CaptureStep::PortConfig => self.port_config(),
            CaptureStep::PortStart => self.port_cmd(AFE_PORT_CMD_DEVICE_START),
            CaptureStep::MapMemory => self.map_memory(),
            CaptureStep::OpenRead => self.open_read(),
            CaptureStep::AdmOpen => self.adm_open(),
            CaptureStep::Matrix => self.matrix(),
            CaptureStep::EncoderConfig => self.encoder_config(),
            CaptureStep::Run => self.run_command(),
            CaptureStep::PortStop => self.port_cmd(AFE_PORT_CMD_DEVICE_STOP),
            CaptureStep::Close => self.asm(ASM_STREAM_CMD_CLOSE, &[]),
            CaptureStep::Unmap => self.unmap_memory(),
            CaptureStep::AdmClose => self.adm_close(),
            CaptureStep::Idle
            | CaptureStep::Prepared
            | CaptureStep::Running
            | CaptureStep::Stopped => {
                self.waiting = false;
                return None;
            }
        };
        Some(packet)
    }

    fn stop_step_due(&self, step: CaptureStep) -> bool {
        match step {
            CaptureStep::PortStop => self.port_started,
            CaptureStep::Close => self.asm_open,
            CaptureStep::Unmap => self.mapped,
            CaptureStep::AdmClose => self.copp.is_some(),
            _ => false,
        }
    }

    fn advance_stop(&mut self, from: CaptureStep) -> Option<Vec<u8>> {
        let mut step = from;
        while step != CaptureStep::Stopped {
            if self.stop_step_due(step) {
                return self.next(step);
            }
            step = rules::capture_after_stop(step);
        }
        self.step = CaptureStep::Stopped;
        self.waiting = false;
        None
    }

    fn after_stop_step(&mut self, step: CaptureStep) -> Outcome {
        match step {
            CaptureStep::PortStop => self.port_started = false,
            CaptureStep::Close => self.asm_open = false,
            CaptureStep::Unmap => self.mapped = false,
            _ => self.copp = None,
        }
        let send = self.advance_stop(rules::capture_after_stop(step));
        let event = if self.step == CaptureStep::Stopped {
            Event::Stopped
        } else {
            Event::None
        };
        Outcome { send, event }
    }

    /// `q6asm_stream_callback`: the address in the event must be the slot's.
    fn read_done(&self, packet: &Packet) -> Event {
        let slot = packet.token;
        let words = |i: usize| {
            packet
                .payload
                .get(i * 4..i * 4 + 4)
                .and_then(|b| b.try_into().ok())
                .map(u32::from_le_bytes)
        };
        let (low, high) = (words(1).unwrap_or(0), words(2).unwrap_or(0));
        match self.slot_address(slot) {
            Some(addr) if addr as u32 == low && (addr >> 32) as u32 == high => {
                Event::ReadDone { slot }
            }
            _ => Event::WrongReadDone { slot, low, high },
        }
    }

    fn answer_is_ours(&self, step: CaptureStep, packet: &Packet) -> bool {
        let session = (packet.dest_port >> 8) as u8;
        match step {
            CaptureStep::VoteMacro => packet.token == HW_MACRO_BLOCK,
            CaptureStep::VoteDcodec => packet.token == HW_DCODEC_BLOCK,
            CaptureStep::BusClock | CaptureStep::Mclk => packet.token == AFE_CLK_TOKEN,
            CaptureStep::PortConfig | CaptureStep::PortStart | CaptureStep::PortStop => {
                packet.token == self.cfg.port_index
            }
            CaptureStep::MapMemory | CaptureStep::Unmap => {
                session == 0 && packet.token == self.map_token()
            }
            CaptureStep::AdmOpen | CaptureStep::AdmClose => {
                packet.token == self.cfg.port_index << 16
            }
            CaptureStep::Matrix => true,
            _ => session == self.cfg.session,
        }
    }

    pub fn on_packet(&mut self, packet: &Packet) -> Outcome {
        let quiet = Outcome {
            send: None,
            event: Event::None,
        };
        if packet.opcode == ASM_DATA_EVENT_READ_DONE_V2 {
            if packet.src_svc != apr::SVC_ASM || (packet.dest_port >> 8) as u8 != self.cfg.session {
                return quiet;
            }
            return Outcome {
                send: None,
                event: self.read_done(packet),
            };
        }
        let step = self.step;
        let (expected_svc, opcode, rsp) = match step {
            CaptureStep::VoteMacro | CaptureStep::VoteDcodec => (
                apr::SVC_AFE,
                AFE_CMD_REMOTE_LPASS_CORE_HW_VOTE_REQUEST,
                AFE_CMD_RSP_REMOTE_LPASS_CORE_HW_VOTE_REQUEST,
            ),
            CaptureStep::BusClock | CaptureStep::Mclk => (apr::SVC_AFE, AFE_SVC_CMD_SET_PARAM, 0),
            CaptureStep::PortConfig => (apr::SVC_AFE, AFE_PORT_CMD_SET_PARAM_V2, 0),
            CaptureStep::PortStart => (apr::SVC_AFE, AFE_PORT_CMD_DEVICE_START, 0),
            CaptureStep::PortStop => (apr::SVC_AFE, AFE_PORT_CMD_DEVICE_STOP, 0),
            CaptureStep::MapMemory => (
                apr::SVC_ASM,
                ASM_CMD_SHARED_MEM_MAP_REGIONS,
                ASM_CMDRSP_SHARED_MEM_MAP_REGIONS,
            ),
            CaptureStep::Unmap => (apr::SVC_ASM, ASM_CMD_SHARED_MEM_UNMAP_REGIONS, 0),
            CaptureStep::OpenRead => (apr::SVC_ASM, ASM_STREAM_CMD_OPEN_READ_V3, 0),
            CaptureStep::EncoderConfig => (apr::SVC_ASM, ASM_STREAM_CMD_SET_ENCDEC_PARAM, 0),
            CaptureStep::Run => (apr::SVC_ASM, ASM_SESSION_CMD_RUN_V2, 0),
            CaptureStep::Close => (apr::SVC_ASM, ASM_STREAM_CMD_CLOSE, 0),
            CaptureStep::AdmOpen => (
                apr::SVC_ADM,
                ADM_CMD_DEVICE_OPEN_V5,
                ADM_CMDRSP_DEVICE_OPEN_V5,
            ),
            CaptureStep::Matrix => (apr::SVC_ADM, ADM_CMD_MATRIX_MAP_ROUTINGS_V5, 0),
            CaptureStep::AdmClose => (apr::SVC_ADM, ADM_CMD_DEVICE_CLOSE_V5, 0),
            CaptureStep::Idle
            | CaptureStep::Prepared
            | CaptureStep::Running
            | CaptureStep::Stopped => return quiet,
        };
        if packet.src_svc != expected_svc || !self.answer_is_ours(step, packet) {
            return quiet;
        }
        let status = if packet.opcode == apr::BASIC_RSP_RESULT {
            match packet.result() {
                Some((op, status)) if op == opcode => status,
                _ => return quiet,
            }
        } else if rsp == AFE_CMD_RSP_REMOTE_LPASS_CORE_HW_VOTE_REQUEST && packet.opcode == rsp {
            packet.result().map_or(0, |(_, status)| status)
        } else if rsp != 0 && packet.opcode == rsp {
            0
        } else {
            return quiet;
        };
        self.waiting = false;
        if matches!(
            step,
            CaptureStep::PortStop | CaptureStep::Close | CaptureStep::Unmap | CaptureStep::AdmClose
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
            CaptureStep::Mclk => self.votes_held = true,
            CaptureStep::PortStart => self.port_started = true,
            CaptureStep::MapMemory => {
                let Some(handle) = packet
                    .payload
                    .get(..4)
                    .and_then(|b| b.try_into().ok())
                    .map(u32::from_le_bytes)
                else {
                    return Outcome {
                        send: None,
                        event: Event::Failed { opcode, status: 0 },
                    };
                };
                self.map_handle = handle;
                self.mapped = true;
            }
            CaptureStep::OpenRead => self.asm_open = true,
            CaptureStep::AdmOpen => {
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
        let next = rules::capture_after_start(step);
        let event = match next {
            CaptureStep::Prepared => Event::Prepared,
            CaptureStep::Running => Event::Running,
            _ => Event::None,
        };
        Outcome {
            send: self.next(next),
            event,
        }
    }
}
