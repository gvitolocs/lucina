// What must hold whatever a program or the DSP does: the addresses the DSP is given, the buffer arithmetic, the order of the commands,
// when the amplifier may be on, and how loud a sample can be. Verus checks this very file: `//@` lines hold the proof, see
// scripts/verify-shipped.sh (D-011). The rest of the crate and the kernel call these functions and hold no copy of the rules.
//
// Not proved: that the pin follows the gate (`qcom_audio/output.rs` drives it from `gate_is_on` alone), that the bytes of a command mean
// what they should (tests/oracle.rs compares them with Linux's own code), and what the DSP does with any of it.

/// What the DSP can address: bits 32 to 35 of an address it is given are not address but its stream on the SMMU.
pub const DSP_MEMORY_LIMIT: u64 = 0x1_0000_0000;
/// `SID_MASK_DEFAULT` of `q6asm-dai.c`, as a count.
const STREAMS: u32 = 16;

/// The address to give the DSP for memory at `phys`, `iommu_stream` being the first argument of `iommus` of the `dais` node:
/// `q6asm_dai_open`'s `dma_buffer.addr | (sid << 32)`. `None` for memory it cannot reach: Linux's addresses are its own low IOVAs,
/// while here the stream goes untranslated, so that the physical address is the address.
pub fn dsp_address(phys: u64, iommu_stream: u32) -> Option<u64> //@ret
//@ ensures match ret {
//@     Some(a) => phys < 0x1_0000_0000 && a % 0x1_0000_0000 == phys && a / 0x1_0000_0000 == iommu_stream % 16,
//@     None => phys >= 0x1_0000_0000,
//@ },
{
    if phys >= DSP_MEMORY_LIMIT {
        return None;
    }
    let stream = (iommu_stream % STREAMS) as u64;
    Some(stream * DSP_MEMORY_LIMIT + phys)
}

/// All of `bytes` from `phys` is memory the DSP can reach.
pub fn region_reachable(phys: u64, bytes: u64) -> bool //@ret
//@ ensures ret ==> bytes > 0 && phys + bytes <= 0x1_0000_0000,
{
    bytes > 0 && phys < DSP_MEMORY_LIMIT && bytes <= DSP_MEMORY_LIMIT - phys
}

/// `PLAYBACK_MAX_PERIOD_SIZE` and `PLAYBACK_MAX_NUM_PERIODS` of `q6asm-dai.c`.
pub const MAX_PERIOD_BYTES: u32 = 65536;
pub const MAX_PERIODS: u32 = 8;

/// `__q6asm_memory_map_regions`: "DSP expects size should be aligned to 4K".
pub fn map_bytes(period_bytes: u32, periods: u32) -> Option<u32> //@ret
//@ ensures match ret {
//@     Some(b) => period_bytes > 0 && periods > 0 && b % 4096 == 0 && b >= period_bytes * periods && b < period_bytes * periods + 4096,
//@     None => true,
//@ },
{
    if period_bytes == 0 || period_bytes > MAX_PERIOD_BYTES || periods == 0 || periods > MAX_PERIODS
    {
        return None;
    }
    //@ assert(period_bytes * periods <= 65536 * 8) by (nonlinear_arith) requires period_bytes <= 65536, periods <= 8;
    let bytes = period_bytes * periods;
    Some((bytes + 4095) / 4096 * 4096)
}

/// `q6asm_write_async`: `dsp_buf` goes round.
pub fn next_slot(slot: u32, periods: u32) -> u32 //@ret
//@ requires slot < periods,
//@ ensures ret < periods,
{
    if slot + 1 >= periods { 0 } else { slot + 1 }
}

/// Where in the buffer a slot's period starts. `None` for a slot or a size [`map_bytes`] would not map.
pub fn slot_offset(slot: u32, period_bytes: u32, periods: u32) -> Option<u32> //@ret
//@ ensures match ret {
//@     Some(o) => slot < periods && o + period_bytes <= period_bytes * periods && o == slot * period_bytes,
//@     None => true,
//@ },
{
    if period_bytes == 0
        || period_bytes > MAX_PERIOD_BYTES
        || periods == 0
        || periods > MAX_PERIODS
        || slot >= periods
    {
        return None;
    }
    //@ assert(slot * period_bytes + period_bytes <= period_bytes * periods) by (nonlinear_arith) requires slot < periods, period_bytes > 0;
    //@ assert(period_bytes * periods <= 65536 * 8) by (nonlinear_arith) requires period_bytes <= 65536, periods <= 8;
    Some(slot * period_bytes)
}

/// One playback's commands, in the order they go out. See `stream.rs` for what sends each in Linux.
#[derive(Clone, Copy, Debug, PartialEq, Eq)] //@omit
pub enum Step {
    Idle,
    ClockOn,
    PortConfig,
    PortStart,
    MapMemory,
    OpenWrite,
    AdmOpen,
    Matrix,
    MediaFormat,
    /// Everything is set up and nothing runs: the first periods go to the DSP now, as `q6asm_dai_ack` queues them.
    Prepared,
    Run,
    Running,
    PortStop,
    ClockOff,
    Close,
    Unmap,
    AdmClose,
    Stopped,
}

//@ pub open spec fn start_rank(s: Step) -> int {
//@     match s {
//@         Step::Idle => 0, Step::ClockOn => 1, Step::PortConfig => 2, Step::PortStart => 3, Step::MapMemory => 4, Step::OpenWrite => 5,
//@         Step::AdmOpen => 6, Step::Matrix => 7, Step::MediaFormat => 8, Step::Prepared => 9, Step::Run => 10, Step::Running => 11,
//@         _ => -1,
//@     }
//@ }
//@ pub open spec fn stop_rank(s: Step) -> int {
//@     match s {
//@         Step::PortStop => 0, Step::ClockOff => 1, Step::Close => 2, Step::Unmap => 3, Step::AdmClose => 4, Step::Stopped => 5,
//@         _ => -1,
//@     }
//@ }

/// The step after `step` on the way up. So no step can be skipped or repeated: the rank goes up by exactly one.
pub fn after_start(step: Step) -> Step //@ret
//@ ensures
//@     0 <= start_rank(step) < 11 ==> start_rank(ret) == start_rank(step) + 1,
//@     !(0 <= start_rank(step) < 11) ==> ret == step,
{
    match step {
        Step::Idle => Step::ClockOn,
        Step::ClockOn => Step::PortConfig,
        Step::PortConfig => Step::PortStart,
        Step::PortStart => Step::MapMemory,
        Step::MapMemory => Step::OpenWrite,
        Step::OpenWrite => Step::AdmOpen,
        Step::AdmOpen => Step::Matrix,
        Step::Matrix => Step::MediaFormat,
        Step::MediaFormat => Step::Prepared,
        Step::Prepared => Step::Run,
        Step::Run => Step::Running,
        other => other,
    }
}

/// The step after `step` on the way down: `dpcm_fe_dai_shutdown` closes the back end, the port and its clock, before the front end.
pub fn after_stop(step: Step) -> Step //@ret
//@ ensures
//@     0 <= stop_rank(step) < 5 ==> stop_rank(ret) == stop_rank(step) + 1,
//@     !(0 <= stop_rank(step) < 5) ==> ret is Stopped,
{
    match step {
        Step::PortStop => Step::ClockOff,
        Step::ClockOff => Step::Close,
        Step::Close => Step::Unmap,
        Step::Unmap => Step::AdmClose,
        _ => Step::Stopped,
    }
}

/// A period may go to the DSP from the moment everything is set up, until the way down begins.
pub fn may_write(step: Step) -> bool //@ret
//@ ensures ret <==> (step is Prepared || step is Run || step is Running),
{
    matches!(step, Step::Prepared | Step::Run | Step::Running)
}

/// Whether the amplifier may be on. Only these functions make or change one, and the pin follows [`gate_is_on`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)] //@omit
pub struct AmpGate {
    pub on: bool,
    pub stream_running: bool,
    pub confirmed: u32,
}

//@ pub open spec fn gate_sound(g: AmpGate) -> bool { g.on ==> g.stream_running && g.confirmed > 0 }

/// As it is at start, after a stop and after the DSP's audio domain went down.
pub fn gate_off() -> AmpGate //@ret
//@ ensures !ret.on, !ret.stream_running, ret.confirmed == 0, gate_sound(ret),
{
    AmpGate {
        on: false,
        stream_running: false,
        confirmed: 0,
    }
}

/// The DSP has said the stream runs. No period of it is confirmed yet, so this never turns anything on.
pub fn gate_running() -> AmpGate //@ret
//@ ensures !ret.on, ret.stream_running, ret.confirmed == 0, gate_sound(ret),
{
    AmpGate {
        on: false,
        stream_running: true,
        confirmed: 0,
    }
}

/// The DSP says a period went through. `genuine`: for the address and slot it was given. Anything else changes nothing, and neither
/// does an answer while no stream runs.
pub fn gate_write_done(g: AmpGate, genuine: bool) -> AmpGate //@ret
//@ requires gate_sound(g),
//@ ensures
//@     gate_sound(ret),
//@     ret.on ==> g.stream_running && (g.on || genuine),
//@     (!genuine || !g.stream_running) ==> ret == g,
{
    if !genuine || !g.stream_running {
        return g;
    }
    let confirmed = if g.confirmed == u32::MAX {
        u32::MAX
    } else {
        g.confirmed + 1
    };
    AmpGate {
        on: true,
        stream_running: true,
        confirmed,
    }
}

pub fn gate_is_on(g: AmpGate) -> bool //@ret
//@ ensures ret == g.on,
{
    g.on
}

/// The loudest a program can ask for, in decibels below full scale.
pub const CEILING_DB: i32 = -12;
/// Below this a stream is silent.
pub const SILENCE_DB: i32 = -90;
/// `round(32768 * 10^(-12/20))`.
pub const CEILING_Q15: u16 = 8231;
/// The largest a scaled sample can be, by magnitude: `8231 * 32768 / 32768`.
pub const CEILING_PEAK: i32 = 8231;

/// Which entry of a table from [`CEILING_DB`] down to [`SILENCE_DB`] a volume means: anything above the ceiling means the ceiling.
pub fn gain_index(db: i32) -> Option<usize> //@ret
//@ ensures match ret {
//@     Some(i) => db >= -90 && i < 79 && (db >= -12 ==> i == 0) && (db < -12 ==> i == -12 - db),
//@     None => db < -90,
//@ },
{
    if db < SILENCE_DB {
        return None;
    }
    let db = if db > CEILING_DB { CEILING_DB } else { db };
    Some((CEILING_DB - db) as usize)
}

/// A table entry, which can then be no louder than the ceiling whatever the table holds.
pub fn capped_q15(raw: u16) -> u16 //@ret
//@ ensures ret <= 8231, raw <= 8231 ==> ret == raw,
{
    if raw > CEILING_Q15 { CEILING_Q15 } else { raw }
}

/// `(s * q15) >> 15`, written so that Verus can see through it. tests/level.rs compares the two.
pub fn scale(s: i16, q15: u16) -> i16 //@ret
//@ requires q15 <= 8231,
//@ ensures -8231 <= ret <= 8231, s >= 0 ==> ret >= 0, s <= 0 ==> ret <= 0, q15 == 0 ==> ret == 0,
{
    let (negative, magnitude) = if s < 0 {
        (true, (-(s as i32)) as u32)
    } else {
        (false, s as u32)
    };
    //@ assert(magnitude * (q15 as int) <= 32768 * 8231) by (nonlinear_arith) requires magnitude <= 32768, q15 <= 8231;
    let product = magnitude * q15 as u32;
    //@ assert(q15 == 0 ==> magnitude * (q15 as int) == 0) by (nonlinear_arith);
    //@ assert(magnitude == 0 ==> magnitude * (q15 as int) == 0) by (nonlinear_arith);
    if negative {
        let down = (product + 32767) / 32768;
        -(down as i32) as i16
    } else {
        (product / 32768) as i16
    }
}

/// The test tone's peak: `round(32768 * 10^(-30/20))`.
pub const TONE_PEAK: i32 = 1036;
/// The tone's fade in and out.
pub const TONE_RAMP_FRAMES: i32 = 240;

/// One sample of the tone from its table and how far into a ramp it is: never more than [`TONE_PEAK`], whatever the table holds.
pub fn tone_sample(raw: i16, ramp: u32) -> i16 //@ret
//@ ensures -1036 <= ret <= 1036,
{
    let negative = raw < 0;
    let magnitude = if negative { -(raw as i32) } else { raw as i32 };
    let magnitude = if magnitude > TONE_PEAK {
        TONE_PEAK
    } else {
        magnitude
    } as u32;
    let ramp = if ramp > TONE_RAMP_FRAMES as u32 {
        TONE_RAMP_FRAMES as u32
    } else {
        ramp
    };
    //@ assert(magnitude * ramp <= 1036 * 240) by (nonlinear_arith) requires magnitude <= 1036, ramp <= 240;
    //@ assert(magnitude * ramp / 240 <= magnitude) by (nonlinear_arith) requires ramp <= 240, magnitude <= 1036;
    let scaled = (magnitude * ramp / TONE_RAMP_FRAMES as u32) as i32;
    (if negative { -scaled } else { scaled }) as i16
}

/// One capture's commands, in the order they go out. See `capture.rs` for what sends each in Linux.
#[derive(Clone, Copy, Debug, PartialEq, Eq)] //@omit
pub enum CaptureStep {
    Idle,
    /// The core-hardware votes and the clock Linux's VA macro and clock controller take at probe and hold: sent once, then held.
    VoteMacro,
    VoteDcodec,
    BusClock,
    /// The VA macro's `mclk`: `TX_CORE_MCLK`, which `va_macro_probe` sets to 19.2 MHz before any of its registers is touched.
    Mclk,
    PortConfig,
    PortStart,
    MapMemory,
    OpenRead,
    AdmOpen,
    Matrix,
    EncoderConfig,
    /// Set up: a read goes to the DSP for every period now, as `q6asm_dai_prepare` sends them.
    Prepared,
    Run,
    Running,
    PortStop,
    Close,
    Unmap,
    AdmClose,
    Stopped,
}

//@ pub open spec fn capture_start_rank(s: CaptureStep) -> int {
//@     match s {
//@         CaptureStep::Idle => 0, CaptureStep::VoteMacro => 1, CaptureStep::VoteDcodec => 2, CaptureStep::BusClock => 3,
//@         CaptureStep::Mclk => 4, CaptureStep::PortConfig => 5, CaptureStep::PortStart => 6, CaptureStep::MapMemory => 7,
//@         CaptureStep::OpenRead => 8, CaptureStep::AdmOpen => 9, CaptureStep::Matrix => 10, CaptureStep::EncoderConfig => 11,
//@         CaptureStep::Prepared => 12, CaptureStep::Run => 13, CaptureStep::Running => 14,
//@         _ => -1,
//@     }
//@ }
//@ pub open spec fn capture_stop_rank(s: CaptureStep) -> int {
//@     match s {
//@         CaptureStep::PortStop => 0, CaptureStep::Close => 1, CaptureStep::Unmap => 2, CaptureStep::AdmClose => 3, CaptureStep::Stopped => 4,
//@         _ => -1,
//@     }
//@ }

/// The quietest and loudest gain Linux's "VA_DECn Volume" control takes (`lpass-va-macro.c`: `SOC_SINGLE_S8_TLV(..,
/// CDC_VA_TXn_TX_VOL_CTL, -84, 40, digital_gain)`, `DECLARE_TLV_DB_SCALE(digital_gain, -8400, 100, -8400)`): 1 dB steps.
pub const MIC_GAIN_MIN_DB: i32 = -84;
pub const MIC_GAIN_MAX_DB: i32 = 40;

/// The value of a VA decimator's `TX_VOL_CTL` for `db`: the signed dB in the register's 8 bits, held to the control's range
/// (0 is 0 dB, the reset value, which is also what Windows leaves for this laptop's mic: its path has no gain object).
// Spelled out rather than `clamp`, so that Verus proves each branch.
#[allow(clippy::manual_clamp)]
pub fn mic_gain_reg(db: i32) -> u32 //@ret
//@ ensures
//@     ret < 256,
//@     ret <= 40 || 172 <= ret,
//@     MIC_GAIN_MIN_DB <= db < 0 ==> ret as int == db + 256,
//@     0 <= db <= MIC_GAIN_MAX_DB ==> ret as int == db,
//@     db > MIC_GAIN_MAX_DB ==> ret as int == MIC_GAIN_MAX_DB,
//@     db < MIC_GAIN_MIN_DB ==> ret as int == MIC_GAIN_MIN_DB + 256,
{
    let held = if db > MIC_GAIN_MAX_DB {
        MIC_GAIN_MAX_DB
    } else if db < MIC_GAIN_MIN_DB {
        MIC_GAIN_MIN_DB
    } else {
        db
    };
    if held >= 0 { held as u32 } else { (held + 256) as u32 }
}

/// Windows' AFE clock for a VA DMIC recording (`qcauddev7180.sys` `(address not published)`): clock node 9 only for a path whose property
/// 0x11 is 1, node 10 otherwise, and AUDD sends node + 0x302 as the clock id (juno-audio.md section 28). Node 10 is `0x30C`, the
/// VA macro's `mclk`; Linux's SC7280 VA macro takes that clock alone too (section 29).
pub fn windows_va_clock_id(property_0x11_is_1: bool) -> u32 //@ret
//@ ensures
//@     !property_0x11_is_1 ==> ret == 0x30C,
//@     property_0x11_is_1 ==> ret == 0x30B,
{
    let node: u32 = if property_0x11_is_1 { 9 } else { 10 };
    node + 0x302
}

/// The step a capture sends for `step`. With `mclk_only` (Windows' one clock for this laptop's VA DMIC) the bus clock is never sent:
/// the VA macro's `mclk` comes straight after the dcodec vote. Every other step is sent as it is.
pub fn capture_sent_step(step: CaptureStep, mclk_only: bool) -> CaptureStep //@ret
//@ ensures
//@     mclk_only ==> ret != CaptureStep::BusClock,
//@     mclk_only && step == CaptureStep::BusClock ==> ret == CaptureStep::Mclk,
//@     !(mclk_only && step == CaptureStep::BusClock) ==> ret == step,
//@     0 <= capture_start_rank(step) ==> capture_start_rank(ret) >= capture_start_rank(step),
{
    if mclk_only {
        match step {
            CaptureStep::BusClock => CaptureStep::Mclk,
            _ => step,
        }
    } else {
        step
    }
}

/// The step after `step` on the way up: the rank goes up by exactly one.
pub fn capture_after_start(step: CaptureStep) -> CaptureStep //@ret
//@ ensures
//@     0 <= capture_start_rank(step) < 14 ==> capture_start_rank(ret) == capture_start_rank(step) + 1,
//@     !(0 <= capture_start_rank(step) < 14) ==> ret == step,
{
    match step {
        CaptureStep::Idle => CaptureStep::VoteMacro,
        CaptureStep::VoteMacro => CaptureStep::VoteDcodec,
        CaptureStep::VoteDcodec => CaptureStep::BusClock,
        CaptureStep::BusClock => CaptureStep::Mclk,
        CaptureStep::Mclk => CaptureStep::PortConfig,
        CaptureStep::PortConfig => CaptureStep::PortStart,
        CaptureStep::PortStart => CaptureStep::MapMemory,
        CaptureStep::MapMemory => CaptureStep::OpenRead,
        CaptureStep::OpenRead => CaptureStep::AdmOpen,
        CaptureStep::AdmOpen => CaptureStep::Matrix,
        CaptureStep::Matrix => CaptureStep::EncoderConfig,
        CaptureStep::EncoderConfig => CaptureStep::Prepared,
        CaptureStep::Prepared => CaptureStep::Run,
        CaptureStep::Run => CaptureStep::Running,
        other => other,
    }
}

/// The step after `step` on the way down: the back end's port first, then the front end, as `dpcm_fe_dai_shutdown`.
pub fn capture_after_stop(step: CaptureStep) -> CaptureStep //@ret
//@ ensures
//@     0 <= capture_stop_rank(step) < 4 ==> capture_stop_rank(ret) == capture_stop_rank(step) + 1,
//@     !(0 <= capture_stop_rank(step) < 4) ==> ret is Stopped,
{
    match step {
        CaptureStep::PortStop => CaptureStep::Close,
        CaptureStep::Close => CaptureStep::Unmap,
        CaptureStep::Unmap => CaptureStep::AdmClose,
        _ => CaptureStep::Stopped,
    }
}

/// A period may be handed to the DSP from the moment everything is set up, until the way down begins.
pub fn capture_may_read(step: CaptureStep) -> bool //@ret
//@ ensures ret <==> (step is Prepared || step is Run || step is Running),
{
    matches!(
        step,
        CaptureStep::Prepared | CaptureStep::Run | CaptureStep::Running
    )
}

/// What the VA macro does before the command of `step` goes out (`va.rs`): its open, pins and runtime resume before the port's
/// configuration (`__soc_pcm_open` and the BE's hw_params come before `q6afe_dai_prepare`), and DAPM's stream start between the port's
/// start and the front end (`__soc_pcm_prepare` of the BE, before `q6asm_dai_prepare`). Its stop goes after the EOS and before the port
/// stops (`soc_pcm_hw_clean` of the BE, before `dpcm_be_dai_shutdown`), which [`crate::capture::Capture::eos`] leaves room for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)] //@omit
pub enum CodecPhase {
    Nothing,
    Open,
    Start,
}

pub fn codec_before(step: CaptureStep) -> CodecPhase //@ret
//@ ensures
//@     (ret is Open) <==> (step is PortConfig),
//@     (ret is Start) <==> (step is MapMemory),
{
    match step {
        CaptureStep::PortConfig => CodecPhase::Open,
        CaptureStep::MapMemory => CodecPhase::Start,
        _ => CodecPhase::Nothing,
    }
}

/// The microphone's codec may touch only these: a register of the VA macro (to `VA_MAX_OFFSET`, word aligned), or the configuration
/// register (+0) or value register (+4, which `lpi_config_set` writes first for an output) of one of the six DMIC pins of the LPI TLMM
/// (gpio6, 7, 8, 9, 12 and 13, `0x1000` apart). The kernel asks this of every access before it makes it: nothing else in LPASS, no
/// PMIC, no voltage.
pub fn va_access_allowed(lpi: bool, offset: u32) -> bool //@ret
//@ ensures
//@     lpi ==> (ret <==> ((offset % 0x1000 == 0 || offset % 0x1000 == 4) && (offset / 0x1000 == 6 || offset / 0x1000 == 7 || offset / 0x1000 == 8 || offset / 0x1000 == 9 || offset / 0x1000 == 12 || offset / 0x1000 == 13))),
//@     !lpi ==> (ret <==> (offset <= 0x7A8 && offset % 4 == 0)),
{
    if lpi {
        let pin = offset / 0x1000;
        let reg = offset % 0x1000;
        (reg == 0 || reg == 4) && (pin == 6 || pin == 7 || pin == 8 || pin == 9 || pin == 12 || pin == 13)
    } else {
        offset <= 0x7A8 && offset % 4 == 0
    }
}
