//! HID over I2C (Microsoft's "HID over I2C Protocol Specification" v1.0, as Linux's `i2c-hid-core.c` uses it) for
//! keyboards and touchpads, such as the Galaxy Book Go's internal ones (`keyboard@5`, 0CF2:9028, and `touchpad@40`).
//!
//! * [`Device::probe`] first does Linux's `i2c_hid_probe_address`: a 1-byte read, and on failure one more after
//!   500 us. It then reads the 30-byte HID descriptor from the register the device tree names (`hid-descr-addr`)
//!   once, as Linux's `i2c_hid_fetch_hid_descriptor` does, and checks its length and version.
//! * [`Device::set_power`], [`Device::reset`] are the two commands Linux's probe sends; the reset is acknowledged by
//!   an empty input report ([`Input::Empty`]). SET_POWER(ON) is tried twice, 500 us apart, as `i2c_hid_set_power`
//!   does, and takes Linux's 60 ms once it went through.
//! * [`Device::hwreset`] is Linux's `i2c_hid_parse` loop around them: power on, reset, up to 1 s for the acknowledgement,
//!   power on, up to four times, with a second's pause after a failed start.
//! * [`Device::report_descriptor`], [`Device::read_input`]: the report descriptor and input reports (each starts
//!   with its own length). [`Device::poll`] reads while the interrupt line is low, as Linux does on its interrupt, and
//!   every 100 ms too until the line has been seen to move ([`ReadGate`]). Such a read gets idle bytes from some devices
//!   (the Book Go's keyboard: length 0x8102), so what it cannot use is dropped quietly.
//! * [`KeyboardFormat::parse`] finds the keyboard fields (Usage Page 0x07: modifier bits, key arrays or key bitmaps)
//!   in a report descriptor, wherever the report ID puts them, and [`Keyboard`] turns reports into key presses and
//!   releases with Linux key codes (`hid_keyboard[]` of `drivers/hid/hid-input.c`, Linux 7.1). Consumer Control
//!   (Usage Page 0x0C: brightness and volume keys, as arrays or as bits) is kept too, under its own report ID, and
//!   mapped as `hidinput_configure_usage` of `drivers/hid/hid-input.c` maps HID_UP_CONSUMER. Other collections
//!   (the keyboard's stray absolute axis) are skipped.
//! * [`PointerFormat::parse`] finds a touchpad or mouse (a mouse collection first, else a touch pad collection's
//!   first contact), and [`Pointer`] turns its reports into relative moves, wheel steps and button changes.

#![no_std]
#![forbid(unsafe_code)]

use core::fmt;

/// The I2C transactions a HID device needs, at its address.
pub trait Bus {
    type Error;
    fn write(&mut self, data: &[u8]) -> Result<(), Self::Error>;
    fn read(&mut self, buf: &mut [u8]) -> Result<(), Self::Error>;
    /// A write, a repeated start, a read.
    fn write_read(&mut self, data: &[u8], buf: &mut [u8]) -> Result<(), Self::Error>;
    /// Sleeps: the waits the protocol asks for between transactions.
    fn sleep_ms(&mut self, ms: u32);
    /// Sleeps a number of microseconds, for Linux's 400-500 us pauses. The default rounds up to whole milliseconds.
    fn sleep_us(&mut self, us: u32) {
        self.sleep_ms(us.div_ceil(1000));
    }
    /// Whether a failed transaction is worth another try soon: a NACK is, a timeout that has burnt its second is not.
    fn may_retry(error: &Self::Error) -> bool {
        let _ = error;
        true
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error<E> {
    Bus(E),
    /// The descriptor's length field is not 30.
    DescriptorLength(u16),
    /// The descriptor's version is not 1.00.
    DescriptorVersion(u16),
    /// An input report's length field is out of range (1, or more than wMaxInputLength).
    BadInput(u16),
    /// The report descriptor's length is 0 or above [`MAX_REPORT_DESC_LEN`], as `i2c_hid_parse` refuses.
    BadReportDescLen(u16),
    /// The buffer is smaller than the report descriptor or wMaxInputLength.
    BufferTooSmall,
    /// GET_REPORT answered with another report ID than the one asked for (`i2c_hid_get_report`, "device returned
    /// incorrect report").
    WrongReport { asked: u8, got: u8 },
}

impl<E: fmt::Display> fmt::Display for Error<E> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Bus(e) => write!(f, "I2C: {e}"),
            Error::DescriptorLength(l) => write!(f, "HID descriptor length {l}, expected 30"),
            Error::DescriptorVersion(v) => {
                write!(f, "HID descriptor version {v:#06x}, expected 0x0100")
            }
            Error::BadInput(l) => write!(f, "input report length {l}"),
            Error::BadReportDescLen(l) => {
                write!(f, "report descriptor length {l}, refused (0 or above 4096)")
            }
            Error::BufferTooSmall => f.write_str("buffer too small"),
            Error::WrongReport { asked, got } => {
                write!(f, "asked for report {asked}, the device answered with report {got}")
            }
        }
    }
}

/// The HID descriptor (30 bytes, little endian).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Descriptor {
    pub report_desc_len: u16,
    pub report_desc_reg: u16,
    pub input_reg: u16,
    pub max_input_len: u16,
    pub output_reg: u16,
    pub max_output_len: u16,
    pub command_reg: u16,
    pub data_reg: u16,
    pub vendor_id: u16,
    pub product_id: u16,
    pub version_id: u16,
}

impl Descriptor {
    pub fn parse<E>(b: &[u8; 30]) -> Result<Self, Error<E>> {
        let w = |i: usize| u16::from_le_bytes([b[i], b[i + 1]]);
        if w(0) != 30 {
            return Err(Error::DescriptorLength(w(0)));
        }
        if w(2) != 0x0100 {
            return Err(Error::DescriptorVersion(w(2)));
        }
        Ok(Descriptor {
            report_desc_len: w(4),
            report_desc_reg: w(6),
            input_reg: w(8),
            max_input_len: w(10),
            output_reg: w(12),
            max_output_len: w(14),
            command_reg: w(16),
            data_reg: w(18),
            vendor_id: w(20),
            product_id: w(22),
            version_id: w(24),
        })
    }
}

/// What [`Device::read_input`] read.
#[derive(Debug, PartialEq, Eq)]
pub enum Input<'b> {
    /// A zero length: the reset acknowledgement, or nothing pending.
    Empty,
    /// The report (its report ID first, if the device uses report IDs).
    Report(&'b [u8]),
}

/// Command opcodes (the high byte of the command).
const OP_RESET: u8 = 0x01;
const OP_GET_REPORT: u8 = 0x02;
const OP_SET_REPORT: u8 = 0x03;
const OP_SET_POWER: u8 = 0x08;
/// Report types in a command's low byte, bits 5:4 (`i2c_hid_encode_command`'s `report_type`; HID_FEATURE_REPORT is 3).
const REPORT_TYPE_FEATURE: u8 = 0x03;
/// Most bytes a SET_REPORT this crate sends carries after the report ID: the mode reports of a Precision Touchpad are one
/// byte each.
const MAX_SET_REPORT: usize = 32;

/// `i2c_hid_encode_command`: the report type and ID in the low byte and the opcode in the high byte; an ID of 15 or more
/// goes in a third byte, with 15 in the low byte. Returns the bytes and how many there are.
fn encode_command(opcode: u8, report_type: u8, report_id: u8) -> ([u8; 3], usize) {
    if report_id < 0x0f {
        ([report_type << 4 | report_id, opcode, 0], 2)
    } else {
        ([report_type << 4 | 0x0f, opcode, report_id], 3)
    }
}

/// `i2c_hid_probe_address` and `i2c_hid_set_power` sleep 400-500 us before their one retry; 500 us here.
const RETRY_US: u32 = 500;
/// `i2c_hid_set_power`'s sleep after every SET_POWER(ON).
const POWER_ON_MS: u32 = 60;
/// Linux's `HID_MAX_DESCRIPTOR_SIZE` (`include/uapi/linux/hid.h`): `i2c_hid_parse` refuses a report descriptor
/// length of 0 or above it.
const MAX_REPORT_DESC_LEN: u16 = 4096;
/// `i2c_hid_parse`: `tries = 3; do { ... } while (tries-- > 0 && ret)` runs at most four times, and pauses a second only
/// after a start that failed.
const RESET_TRIES: u32 = 4;
const RESET_RETRY_MS: u32 = 1000;
/// `i2c_hid_finish_hwreset`'s wait for the acknowledgement.
const RESET_ACK_MS: u32 = 1000;

/// How the wait for a reset acknowledgement ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ResetAck {
    Acknowledged,
    /// Nothing within a second, which Linux only warns about. `read_empty`: reads with the interrupt line high got the
    /// empty report, which may be the acknowledgement or may be a device with nothing to say.
    NotAcknowledged {
        read_empty: bool,
    },
}

/// What [`Device::hwreset`] did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HwReset {
    pub ack: ResetAck,
    /// Runs of the sequence, 1 if nothing failed.
    pub tries: u32,
}

/// A HID-over-I2C device.
pub struct Device<B: Bus> {
    bus: B,
    desc: Descriptor,
    gate: ReadGate,
    /// The first read made with the line high that failed, until [`Device::take_first_dropped`] takes it.
    first_dropped: Option<Error<B::Error>>,
    told_dropped: bool,
}

impl<B: Bus> Device<B> {
    /// Linux's `__i2c_hid_core_probe` first: `i2c_hid_probe_address` is a 1-byte read, and on failure one more after
    /// 500 us; only if both fail is there nothing at this address. Then the HID descriptor at register
    /// `hid_desc_reg` is read once, as `i2c_hid_fetch_hid_descriptor` does, and checked.
    pub fn probe(mut bus: B, hid_desc_reg: u16) -> Result<Self, Error<B::Error>> {
        let mut byte = [0u8; 1];
        if let Err(err) = bus.read(&mut byte) {
            if !B::may_retry(&err) {
                return Err(Error::Bus(err));
            }
            bus.sleep_us(RETRY_US);
            bus.read(&mut byte).map_err(Error::Bus)?;
        }
        let mut raw = [0u8; 30];
        bus.write_read(&hid_desc_reg.to_le_bytes(), &mut raw)
            .map_err(Error::Bus)?;
        let desc = Descriptor::parse(&raw)?;
        Ok(Device {
            bus,
            desc,
            gate: ReadGate::new(),
            first_dropped: None,
            told_dropped: false,
        })
    }

    pub fn descriptor(&self) -> &Descriptor {
        &self.desc
    }

    pub fn bus_mut(&mut self) -> &mut B {
        &mut self.bus
    }

    fn command(&mut self, low: u8, opcode: u8) -> Result<(), Error<B::Error>> {
        let [r0, r1] = self.desc.command_reg.to_le_bytes();
        self.bus.write(&[r0, r1, low, opcode]).map_err(Error::Bus)
    }

    /// SET_POWER ON or SLEEP, as `i2c_hid_set_power`: ON is sent twice, 500 us apart, if the first is not
    /// acknowledged, and sleeps 60 ms once it went through.
    pub fn set_power(&mut self, on: bool) -> Result<(), Error<B::Error>> {
        if !on {
            return self.command(1, OP_SET_POWER);
        }
        let [r0, r1] = self.desc.command_reg.to_le_bytes();
        let send = |bus: &mut B| bus.write(&[r0, r1, 0, OP_SET_POWER]);
        match send(&mut self.bus) {
            Err(err) if B::may_retry(&err) => {
                self.bus.sleep_us(RETRY_US);
                send(&mut self.bus).map_err(Error::Bus)?;
            }
            result => result.map_err(Error::Bus)?,
        }
        self.bus.sleep_ms(POWER_ON_MS);
        Ok(())
    }

    /// GET_REPORT of feature report `report_id`, as `i2c_hid_get_report` does it: one write of the command register, the
    /// encoded command and the data register, a repeated start, and a read of `len` + 2 bytes (`len` is the report's
    /// length with its ID byte, as hid-core asks: `hid_report_len`). The device answers with a 2-byte length, then the
    /// report. Returns the report, its ID first, cut to what the device said it sent; empty if it sent nothing. A report
    /// with another ID is refused, as `i2c_hid_get_report` and `mt_get_feature` refuse it.
    pub fn get_feature<'b>(
        &mut self,
        report_id: u8,
        len: usize,
        buf: &'b mut [u8],
    ) -> Result<&'b [u8], Error<B::Error>> {
        let [c0, c1] = self.desc.command_reg.to_le_bytes();
        let [d0, d1] = self.desc.data_reg.to_le_bytes();
        let (cmd, n) = encode_command(OP_GET_REPORT, REPORT_TYPE_FEATURE, report_id);
        let mut out = [0u8; 7];
        out[..2].copy_from_slice(&[c0, c1]);
        out[2..2 + n].copy_from_slice(&cmd[..n]);
        out[2 + n..4 + n].copy_from_slice(&[d0, d1]);
        let buf = buf.get_mut(..len + 2).ok_or(Error::BufferTooSmall)?;
        self.bus
            .write_read(&out[..4 + n], buf)
            .map_err(Error::Bus)?;
        let count = usize::from(u16::from_le_bytes([buf[0], buf[1]]));
        if count <= 2 {
            return Ok(&buf[2..2]);
        }
        let got = len.min(count - 2);
        let report = &buf[2..2 + got];
        if report_id != 0 && got != 0 && report[0] != report_id {
            return Err(Error::WrongReport {
                asked: report_id,
                got: report[0],
            });
        }
        Ok(report)
    }

    /// SET_REPORT of feature report `report_id` with `data` (the report without its ID), as
    /// `i2c_hid_set_or_send_report` with `do_set`: the command register, the encoded command, the data register, then
    /// the report as `i2c_hid_format_report` lays it out (a 2-byte length counting itself, the ID if not 0, the data), in
    /// one write.
    pub fn set_feature(&mut self, report_id: u8, data: &[u8]) -> Result<(), Error<B::Error>> {
        if data.len() > MAX_SET_REPORT {
            return Err(Error::BufferTooSmall);
        }
        let [c0, c1] = self.desc.command_reg.to_le_bytes();
        let [d0, d1] = self.desc.data_reg.to_le_bytes();
        let (cmd, n) = encode_command(OP_SET_REPORT, REPORT_TYPE_FEATURE, report_id);
        let mut out = [0u8; 7 + 3 + MAX_SET_REPORT];
        let mut at = 0;
        let mut put = |bytes: &[u8]| {
            out[at..at + bytes.len()].copy_from_slice(bytes);
            at += bytes.len();
        };
        put(&[c0, c1]);
        put(&cmd[..n]);
        put(&[d0, d1]);
        let id_len = usize::from(report_id != 0);
        let length = (2 + id_len + data.len()) as u16;
        put(&length.to_le_bytes());
        if report_id != 0 {
            put(&[report_id]);
        }
        put(data);
        self.bus.write(&out[..at]).map_err(Error::Bus)
    }

    /// RESET. The device acknowledges with an empty input report within a second.
    pub fn reset(&mut self) -> Result<(), Error<B::Error>> {
        self.command(0, OP_RESET)
    }

    /// Linux's reset sequence with the retries of `i2c_hid_parse`: power on, reset, wait for the acknowledgement, power
    /// on. `line` is the interrupt line at this moment, true if asserted, `None` if there is none to look at. `buf` holds
    /// an input report, as for [`Device::read_input`].
    pub fn hwreset(
        &mut self,
        buf: &mut [u8],
        mut line: impl FnMut() -> Option<bool>,
    ) -> Result<HwReset, Error<B::Error>> {
        let mut tries = 0;
        loop {
            tries += 1;
            let done = match self.start_hwreset() {
                Ok(()) => {
                    let ack = self.wait_reset_ack(buf, &mut line);
                    self.set_power(true).map(|()| ack)
                }
                Err(err) => {
                    self.bus.sleep_ms(RESET_RETRY_MS);
                    Err(err)
                }
            };
            match done {
                Ok(ack) => return Ok(HwReset { ack, tries }),
                Err(err) if tries >= RESET_TRIES => return Err(err),
                Err(_) => {}
            }
        }
    }

    /// `i2c_hid_start_hwreset`: a reset that failed puts the device back to sleep.
    fn start_hwreset(&mut self) -> Result<(), Error<B::Error>> {
        self.set_power(true)?;
        self.reset().inspect_err(|_| {
            let _ = self.command(1, OP_SET_POWER);
        })
    }

    /// Waits for the empty report that acknowledges a reset. An empty read counts only with no line to look at or the
    /// line asserted: the device says the same when it has nothing to say, and may well not be done resetting.
    fn wait_reset_ack(
        &mut self,
        buf: &mut [u8],
        line: &mut impl FnMut() -> Option<bool>,
    ) -> ResetAck {
        let mut read_empty = false;
        for _ in 0..RESET_ACK_MS / ReadGate::TICK_MS {
            self.bus.sleep_ms(ReadGate::TICK_MS);
            let level = line();
            if self.gate.tick(level) == 0 {
                continue;
            }
            if let Ok(Input::Empty) = self.read_input(buf) {
                if level == Some(false) {
                    read_empty = true;
                } else {
                    return ResetAck::Acknowledged;
                }
            }
        }
        ResetAck::NotAcknowledged { read_empty }
    }

    /// Reads the report descriptor into `buf`. `i2c_hid_parse` refuses a length of 0 or above
    /// [`MAX_REPORT_DESC_LEN`] before reading it.
    pub fn report_descriptor<'b>(
        &mut self,
        buf: &'b mut [u8],
    ) -> Result<&'b [u8], Error<B::Error>> {
        let len = self.desc.report_desc_len;
        if len == 0 || len > MAX_REPORT_DESC_LEN {
            return Err(Error::BadReportDescLen(len));
        }
        let len = usize::from(len);
        let buf = buf.get_mut(..len).ok_or(Error::BufferTooSmall)?;
        self.bus
            .write_read(&self.desc.report_desc_reg.to_le_bytes(), buf)
            .map_err(Error::Bus)?;
        Ok(buf)
    }

    /// Reads one input report, a plain read of `wMaxInputLength` bytes. Linux's `i2c_hid_get_input` reads
    /// `min(wMaxInputLength, bufsize)`, `bufsize` being at least 64 and the largest input, output or feature report of the
    /// parsed descriptor (a precision touchpad's 256-byte certification report makes it large). This crate does not size
    /// reports, so it reads `wMaxInputLength`, which is never less than what Linux reads: the device pads the rest.
    pub fn read_input<'b>(&mut self, buf: &'b mut [u8]) -> Result<Input<'b>, Error<B::Error>> {
        let max = usize::from(self.desc.max_input_len).max(2);
        let buf = buf.get_mut(..max).ok_or(Error::BufferTooSmall)?;
        self.bus.read(buf).map_err(Error::Bus)?;
        let len = u16::from_le_bytes([buf[0], buf[1]]);
        match usize::from(len) {
            0 => Ok(Input::Empty),
            2 => Ok(Input::Report(&buf[2..2])),
            l if l < 2 || l > max => Err(Error::BadInput(len)),
            l => Ok(Input::Report(&buf[2..l])),
        }
    }

    /// One tick of [`ReadGate::TICK_MS`]: reads what the interrupt line says is there, and calls `on_report` with each
    /// report and the line it was read at. `line` is as for [`Device::hwreset`].
    ///
    /// Linux reads only on the interrupt, and logs a length it cannot use (`i2c_hid_get_input`, "incomplete report"), so
    /// such a read with the line asserted is returned as an error. With the line high nothing was promised: a read that
    /// fails is dropped, the first for [`Device::take_first_dropped`], and so is a bad length with no line to look at.
    pub fn poll(
        &mut self,
        buf: &mut [u8],
        mut line: impl FnMut() -> Option<bool>,
        mut on_report: impl FnMut(&[u8], Option<bool>),
    ) -> Result<(), Error<B::Error>> {
        let mut level = line();
        for read in 0..self.gate.tick(level) {
            if read > 0 {
                level = line();
                if level == Some(false) {
                    break;
                }
            }
            match self.read_input(buf) {
                Ok(Input::Report(report)) => {
                    if !self.gate.fresh(report, level) {
                        break;
                    }
                    on_report(report, level);
                }
                Ok(Input::Empty) => break,
                Err(err) => {
                    let promised = match level {
                        Some(asserted) => asserted,
                        None => !matches!(err, Error::BadInput(_)),
                    };
                    if promised {
                        return Err(err);
                    }
                    if !self.told_dropped {
                        self.told_dropped = true;
                        self.first_dropped = Some(err);
                    }
                    break;
                }
            }
        }
        Ok(())
    }

    /// The first read dropped by [`Device::poll`], once.
    pub fn take_first_dropped(&mut self) -> Option<Error<B::Error>> {
        self.first_dropped.take()
    }

    /// Whether the interrupt line has been seen to move, which ends the reads it did not ask for.
    pub fn line_moved(&self) -> bool {
        self.gate.line_moved
    }
}

/// Most bytes of a report [`ReadGate::fresh`] compares.
const GATE_REPORT_BYTES: usize = 64;

/// When to read a device for input reports, called once per [`ReadGate::TICK_MS`]: while the interrupt line is asserted,
/// and every 100 ms until the line has been seen to move, so that a pad or line that does not behave as the device tree
/// says costs latency, not the keyboard. A line that moves works, and a level interrupt cannot be missed, so from then on
/// only the line decides, as in Linux. With no line to look at, every tick.
pub struct ReadGate {
    since_read_ms: u32,
    level: Option<bool>,
    line_moved: bool,
    last: [u8; GATE_REPORT_BYTES],
    last_len: usize,
}

impl Default for ReadGate {
    fn default() -> Self {
        Self::new()
    }
}

impl ReadGate {
    pub const TICK_MS: u32 = 10;
    /// How long the line may stay high before a read anyway.
    pub const BLIND_READ_MS: u32 = 100;
    /// Reads one tick makes at most while the line stays asserted.
    pub const MAX_READS: u32 = 8;

    pub const fn new() -> Self {
        ReadGate {
            since_read_ms: 0,
            level: None,
            line_moved: false,
            last: [0; GATE_REPORT_BYTES],
            last_len: 0,
        }
    }

    /// Advances a tick. `line` is the interrupt line now (true: asserted). Returns how many reads to make: more than
    /// one only while the line stays asserted, which the caller checks again between them.
    pub fn tick(&mut self, line: Option<bool>) -> u32 {
        self.since_read_ms += Self::TICK_MS;
        if line.is_some() {
            self.line_moved |= self.level.is_some() && self.level != line;
            self.level = line;
        }
        let reads = match line {
            None => 1,
            Some(true) => Self::MAX_READS,
            Some(false) if !self.line_moved && self.since_read_ms >= Self::BLIND_READ_MS => 1,
            Some(false) => 0,
        };
        if reads != 0 {
            self.since_read_ms = 0;
        }
        reads
    }

    /// Whether to act on `report`, read with the line at `line`. A device may answer a read it did not ask for with its
    /// last report again. To [`Keyboard::feed`] and a touch pad's contact that changes nothing, but a relative pointer
    /// would move again, so a report read with the line high that equals the one before is stale.
    pub fn fresh(&mut self, report: &[u8], line: Option<bool>) -> bool {
        let n = report.len().min(GATE_REPORT_BYTES);
        let same = self.last_len == report.len() && self.last[..n] == report[..n];
        self.last[..n].copy_from_slice(&report[..n]);
        self.last_len = report.len();
        !(same && line == Some(false))
    }
}

/// Most keyboard fields kept from a report descriptor.
const MAX_FIELDS: usize = 4;

/// A keyboard input field: bits of one report.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Field {
    /// Bit offset in the report, after the report ID byte.
    pub bit_offset: u32,
    pub size: u32,
    pub count: u32,
    pub usage_min: u32,
    pub logical_min: i32,
    /// One bit per usage (modifiers, n-key-rollover bitmaps) rather than an array of usage numbers.
    pub variable: bool,
}

/// A consumer control input field (Usage Page 0x0C): bits of one report.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ConsumerField {
    /// Bit offset in the report, after the report ID byte.
    pub bit_offset: u32,
    pub size: u32,
    pub count: u32,
    pub usage_min: u32,
    pub logical_min: i32,
    /// One bit per usage rather than an array of usage numbers.
    pub variable: bool,
    usages: [u16; MAX_USAGES],
    nusages: usize,
}

impl ConsumerField {
    /// The usage of element `n` of this item: from the usage list, else counting from Usage Minimum.
    fn usage(&self, n: u32) -> u32 {
        if self.nusages > 0 {
            u32::from(self.usages[(n as usize).min(self.nusages - 1)])
        } else {
            self.usage_min + n
        }
    }
}

/// Where a report descriptor puts the keyboard's keys.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct KeyboardFormat {
    /// The report ID of the keyboard report, if the device uses report IDs.
    pub report_id: Option<u8>,
    fields: [Field; MAX_FIELDS],
    nfields: usize,
    /// The report ID of the consumer control report, if the descriptor has one.
    pub consumer_report_id: Option<u8>,
    consumer_fields: [ConsumerField; MAX_FIELDS],
    nconsumer_fields: usize,
    /// The Wireless Radio Button (Generic Desktop 0xC6, `HID_GD_RFKILL_BTN`): its report ID (0 without IDs) and bit offset
    /// after the report ID byte, if the descriptor has one as a one-bit variable input.
    pub radio: Option<(u8, u32)>,
}

#[derive(Clone, Copy, Default)]
struct Globals {
    usage_page: u32,
    logical_min: i32,
    logical_max: i32,
    physical_min: i32,
    physical_max: i32,
    unit_exponent: i32,
    unit: u32,
    report_size: u32,
    report_count: u32,
    report_id: u8,
}

/// Most usages kept per main item (later ones repeat the last, as HID 1.11 section 6.2.2.8 says).
const MAX_USAGES: usize = 16;

const fn usage(page: u32, id: u32) -> u32 {
    (page << 16) | id
}

const GD_POINTER: u32 = usage(0x01, 0x01);
const GD_MOUSE: u32 = usage(0x01, 0x02);
const GD_X: u32 = usage(0x01, 0x30);
const GD_Y: u32 = usage(0x01, 0x31);
const GD_WHEEL: u32 = usage(0x01, 0x38);
const DIGITIZER_TOUCH_PAD: u32 = usage(0x0d, 0x05);
const DIGITIZER_TIP_SWITCH: u32 = usage(0x0d, 0x42);

/// Main item tags (HID 1.11 section 6.2.2.4).
const MAIN_INPUT: u8 = 0x8;
const MAIN_FEATURE: u8 = 0xb;

/// One Input, Output or Feature main item of a report descriptor.
#[derive(Clone, Copy)]
struct InputItem {
    /// [`MAIN_INPUT`], 0x9 (Output) or [`MAIN_FEATURE`].
    kind: u8,
    report_id: u8,
    /// Which Logical collection (HID 1.11 section 6.2.2.6, type 0x02) this item is in, counting from 1 in the order they
    /// open; 0 outside any. A Precision Touchpad puts each finger's fields in one.
    logical: u32,
    physical_min: i32,
    physical_max: i32,
    unit_exponent: i32,
    unit: u32,
    /// The usage of the outermost collection this item is in.
    application: u32,
    bit_offset: u32,
    size: u32,
    count: u32,
    constant: bool,
    variable: bool,
    relative: bool,
    logical_min: i32,
    logical_max: i32,
    usage_min: Option<u32>,
    usages: [u32; MAX_USAGES],
    nusages: usize,
    usage_page: u32,
}

impl InputItem {
    /// The usage of element `n` of this item: from the usage list, else counting from Usage Minimum.
    fn usage(&self, n: u32) -> u32 {
        if self.nusages > 0 {
            self.usages[(n as usize).min(self.nusages - 1)]
        } else {
            self.usage_min.unwrap_or(usage(self.usage_page, 0)) + n
        }
    }
}

/// Calls `f` for each Input main item of a report descriptor (HID 1.11 section 6.2.2). None if it is truncated.
fn walk_inputs(desc: &[u8], mut f: impl FnMut(&InputItem)) -> Option<()> {
    walk_items(desc, |item| {
        if item.kind == MAIN_INPUT {
            f(item)
        }
    })
}

/// Calls `f` for each Input, Output and Feature main item of a report descriptor. Each kind counts its bit offsets per
/// report ID on its own, as the three are separate reports. None if the descriptor is truncated.
fn walk_items(desc: &[u8], mut f: impl FnMut(&InputItem)) -> Option<()> {
    let mut g = Globals::default();
    let mut stack = [Globals::default(); 4];
    let mut depth = 0;
    let mut usage_min = None::<u32>;
    let mut usages = [0u32; MAX_USAGES];
    let mut nusages = 0;
    // Input, Output, Feature.
    let mut offsets = [[0u32; 256]; 3];
    let mut collections = 0u32;
    let mut application = 0;
    // The Logical collection each open collection level is (0 for other types), and how many have opened.
    let mut logical_at = [0u32; 8];
    let mut logicals = 0u32;
    let mut i = 0;
    while i < desc.len() {
        let prefix = desc[i];
        if prefix == 0xfe {
            // Long item: data size, tag, data.
            let size = usize::from(*desc.get(i + 1)?);
            i += 3 + size;
            continue;
        }
        let size = match prefix & 3 {
            3 => 4,
            s => usize::from(s),
        };
        let data = desc.get(i + 1..i + 1 + size)?;
        i += 1 + size;
        let mut raw = [0u8; 4];
        raw[..size].copy_from_slice(data);
        let unsigned = u32::from_le_bytes(raw);
        let signed = match size {
            1 => i32::from(raw[0] as i8),
            2 => i32::from(i16::from_le_bytes([raw[0], raw[1]])),
            _ => unsigned as i32,
        };
        // A short usage is on the current usage page; a 4-byte one carries its own in the high half.
        let full_usage = if size == 4 {
            unsigned
        } else {
            usage(g.usage_page, unsigned)
        };
        match (prefix >> 2) & 3 {
            // Main
            0 => {
                match prefix >> 4 {
                    kind @ (0x8 | 0x9 | 0xb) => {
                        let which = match kind {
                            MAIN_INPUT => 0,
                            0x9 => 1,
                            _ => 2,
                        };
                        let off = &mut offsets[which][usize::from(g.report_id)];
                        let level = (collections as usize).min(logical_at.len());
                        f(&InputItem {
                            kind,
                            report_id: g.report_id,
                            logical: logical_at[..level].iter().rev().copied().find(|&l| l != 0).unwrap_or(0),
                            physical_min: g.physical_min,
                            physical_max: g.physical_max,
                            unit_exponent: g.unit_exponent,
                            unit: g.unit,
                            application,
                            bit_offset: *off,
                            size: g.report_size,
                            count: g.report_count,
                            constant: unsigned & 1 != 0,
                            variable: unsigned & 2 != 0,
                            relative: unsigned & 4 != 0,
                            logical_min: g.logical_min,
                            logical_max: g.logical_max,
                            usage_min,
                            usages,
                            nusages,
                            usage_page: g.usage_page,
                        });
                        *off += g.report_size * g.report_count;
                    }
                    0xa => {
                        if collections == 0 {
                            application = if nusages > 0 { usages[0] } else { 0 };
                        }
                        if let Some(slot) = logical_at.get_mut(collections as usize) {
                            *slot = if unsigned == 0x02 {
                                logicals += 1;
                                logicals
                            } else {
                                0
                            };
                        }
                        collections += 1;
                    }
                    0xc => collections = collections.saturating_sub(1),
                    _ => {}
                }
                // Every main item ends the local items.
                usage_min = None;
                nusages = 0;
            }
            // Global
            1 => match prefix >> 4 {
                0x0 => g.usage_page = unsigned,
                0x1 => g.logical_min = signed,
                // Linux's hid_parser_global: a maximum is unsigned unless its minimum is negative.
                0x2 => g.logical_max = if g.logical_min < 0 { signed } else { unsigned as i32 },
                0x3 => g.physical_min = signed,
                0x4 => g.physical_max = if g.physical_min < 0 { signed } else { unsigned as i32 },
                // Linux's hid_parser_global: a unit exponent of 0..15 is a 4-bit two's complement number (HID 1.11
                // 6.2.2.7 is often read that way), anything else is taken as it is.
                0x5 => {
                    g.unit_exponent = if signed & !0xf == 0 {
                        (signed << 28) >> 28
                    } else {
                        signed
                    }
                }
                0x6 => g.unit = unsigned,
                0x7 => g.report_size = unsigned,
                0x8 => g.report_id = unsigned as u8,
                0x9 => g.report_count = unsigned,
                0xa if depth < stack.len() => {
                    stack[depth] = g;
                    depth += 1;
                }
                0xb if depth > 0 => {
                    depth -= 1;
                    g = stack[depth];
                }
                _ => {}
            },
            // Local
            2 => match prefix >> 4 {
                0x0 if nusages < MAX_USAGES => {
                    usages[nusages] = full_usage;
                    nusages += 1;
                }
                0x1 => usage_min = Some(full_usage),
                _ => {}
            },
            _ => {}
        }
    }
    Some(())
}

impl KeyboardFormat {
    pub fn fields(&self) -> &[Field] {
        &self.fields[..self.nfields]
    }

    /// The consumer control fields (Usage Page 0x0C) of the same descriptor.
    pub fn consumer_fields(&self) -> &[ConsumerField] {
        &self.consumer_fields[..self.nconsumer_fields]
    }

    /// The keyboard fields of a report descriptor, or None if it has no Usage Page 0x07 input. Consumer Control
    /// fields (Usage Page 0x0C) are kept too, under their own report ID. Fields of other report IDs than the first
    /// keyboard or consumer one are ignored.
    pub fn parse(desc: &[u8]) -> Option<KeyboardFormat> {
        let mut fmt = KeyboardFormat::default();
        let mut keyboard_id = None::<u8>;
        let mut consumer_id = None::<u8>;
        walk_inputs(desc, |item| {
            if item.constant || item.size == 0 {
                return;
            }
            match item.usage(0) >> 16 {
                0x07
                    if keyboard_id.is_none_or(|id| id == item.report_id)
                        && fmt.nfields < MAX_FIELDS =>
                {
                    keyboard_id = Some(item.report_id);
                    fmt.fields[fmt.nfields] = Field {
                        bit_offset: item.bit_offset,
                        size: item.size,
                        count: item.count,
                        usage_min: item.usage(0) & 0xffff,
                        logical_min: item.logical_min,
                        variable: item.variable,
                    };
                    fmt.nfields += 1;
                }
                0x01 if item.usage(0) & 0xffff == 0xc6 && item.variable && item.size == 1 && fmt.radio.is_none() => {
                    fmt.radio = Some((item.report_id, item.bit_offset));
                }
                0x0c
                    if consumer_id.is_none_or(|id| id == item.report_id)
                        && fmt.nconsumer_fields < MAX_FIELDS =>
                {
                    consumer_id = Some(item.report_id);
                    fmt.consumer_fields[fmt.nconsumer_fields] = ConsumerField {
                        bit_offset: item.bit_offset,
                        size: item.size,
                        count: item.count,
                        usage_min: item.usage(0) & 0xffff,
                        logical_min: item.logical_min,
                        variable: item.variable,
                        usages: item.usages.map(|u| u as u16),
                        nusages: item.nusages,
                    };
                    fmt.nconsumer_fields += 1;
                }
                _ => {}
            }
        })?;
        let id = keyboard_id?;
        fmt.report_id = (id != 0).then_some(id);
        if let Some(id) = consumer_id {
            fmt.consumer_report_id = (id != 0).then_some(id);
        }
        Some(fmt)
    }
}

/// A number in a report.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Scalar {
    pub bit_offset: u32,
    pub size: u32,
    pub signed: bool,
}

impl Scalar {
    fn read(&self, payload: &[u8]) -> Option<i32> {
        let v = bits(payload, self.bit_offset, self.size)?;
        Some(
            if self.signed && self.size < 32 && v >> (self.size - 1) != 0 {
                (v as i64 - (1i64 << self.size)) as i32
            } else {
                v as i32
            },
        )
    }
}

/// How a report descriptor's pointer moves.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Motion {
    /// A mouse collection: X and Y are deltas.
    Relative,
    /// A touch pad collection: X and Y are the first contact's position, valid while `tip` is set, from 0 to the
    /// maxima.
    Absolute { tip: Scalar, x_max: i32, y_max: i32 },
}

/// Where a report descriptor puts a pointer: a touchpad or mouse.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PointerFormat {
    /// The report ID of the pointer report, if the device uses report IDs.
    pub report_id: Option<u8>,
    pub motion: Motion,
    pub x: Scalar,
    pub y: Scalar,
    pub wheel: Option<Scalar>,
    /// The Button page's bits: bit offset of Button 1, and how many.
    pub buttons: Option<(u32, u32)>,
}

#[derive(Clone, Copy, Default)]
struct PointerCandidate {
    report_id: u8,
    relative: bool,
    x: Option<Scalar>,
    y: Option<Scalar>,
    x_max: i32,
    y_max: i32,
    tip: Option<Scalar>,
    wheel: Option<Scalar>,
    buttons: Option<(u32, u32)>,
}

impl PointerFormat {
    /// The pointer of a report descriptor, or None if it has none. A mouse collection wins over a touch pad
    /// collection: touch pads start in mouse mode (Microsoft's Precision Touchpad spec, Input Mode 0), and only
    /// the first contact of a touch pad report is used.
    pub fn parse(desc: &[u8]) -> Option<PointerFormat> {
        const MAX_REPORTS: usize = 8;
        let mut reports = [PointerCandidate::default(); MAX_REPORTS];
        let mut n = 0;
        walk_inputs(desc, |item| {
            let relative = match item.application {
                GD_MOUSE | GD_POINTER => true,
                DIGITIZER_TOUCH_PAD => false,
                _ => return,
            };
            if item.constant || item.size == 0 || !item.variable {
                return;
            }
            let index = match reports[..n]
                .iter()
                .position(|r| r.report_id == item.report_id && r.relative == relative)
            {
                Some(index) => index,
                None if n < MAX_REPORTS => {
                    reports[n] = PointerCandidate {
                        report_id: item.report_id,
                        relative,
                        ..Default::default()
                    };
                    n += 1;
                    n - 1
                }
                None => return,
            };
            let report = &mut reports[index];
            for k in 0..item.count {
                let scalar = Scalar {
                    bit_offset: item.bit_offset + k * item.size,
                    size: item.size,
                    signed: item.logical_min < 0,
                };
                let u = item.usage(k);
                match u {
                    GD_X if report.x.is_none() && item.relative == relative => {
                        report.x = Some(scalar);
                        report.x_max = item.logical_max;
                    }
                    GD_Y if report.y.is_none() && item.relative == relative => {
                        report.y = Some(scalar);
                        report.y_max = item.logical_max;
                    }
                    GD_WHEEL if report.wheel.is_none() && item.relative => {
                        report.wheel = Some(scalar);
                    }
                    DIGITIZER_TIP_SWITCH if report.tip.is_none() => report.tip = Some(scalar),
                    u if u == usage(0x09, 1) && item.size == 1 && report.buttons.is_none() => {
                        report.buttons = Some((scalar.bit_offset, item.count - k));
                    }
                    _ => {}
                }
            }
        })?;
        let usable = |r: &&PointerCandidate| {
            r.x.is_some() && r.y.is_some() && (r.relative || r.tip.is_some())
        };
        let found = reports[..n]
            .iter()
            .filter(usable)
            .find(|r| r.relative)
            .or_else(|| reports[..n].iter().find(usable))?;
        Some(PointerFormat {
            report_id: (found.report_id != 0).then_some(found.report_id),
            motion: if found.relative {
                Motion::Relative
            } else {
                Motion::Absolute {
                    tip: found.tip?,
                    x_max: found.x_max,
                    y_max: found.y_max,
                }
            },
            x: found.x?,
            y: found.y?,
            wheel: found.wheel,
            buttons: found.buttons,
        })
    }
}

/// What a pointer report changed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PointerEvent {
    Move {
        dx: i32,
        dy: i32,
    },
    Wheel(i32),
    /// `index` 0 is Button 1 (left), 1 right, 2 middle.
    Button {
        index: u8,
        pressed: bool,
    },
}

/// Pointer units a contact moves when it crosses the whole pad, whatever the pad's logical range is.
const PAD_WIDTH_UNITS: i64 = 1600;

/// Turns pointer reports into relative moves and button changes. A touch pad's absolute contact becomes relative
/// too: the kernel's input core has no absolute events yet.
pub struct Pointer {
    format: PointerFormat,
    buttons: u32,
    last: Option<(i32, i32)>,
    /// What scaling a touch pad's delta left over, in logical units times [`PAD_WIDTH_UNITS`].
    rest: (i64, i64),
}

impl Pointer {
    pub fn new(format: PointerFormat) -> Self {
        Pointer {
            format,
            buttons: 0,
            last: None,
            rest: (0, 0),
        }
    }

    /// Decodes one input report and calls `out` for each change. Reports with another report ID and too short
    /// ones change nothing.
    pub fn feed(&mut self, report: &[u8], mut out: impl FnMut(PointerEvent)) {
        let f = self.format;
        let payload = match f.report_id {
            Some(id) => match report.split_first() {
                Some((&first, rest)) if first == id => rest,
                _ => return,
            },
            None => report,
        };
        let (Some(x), Some(y)) = (f.x.read(payload), f.y.read(payload)) else {
            return;
        };
        match f.motion {
            Motion::Relative => {
                if x != 0 || y != 0 {
                    out(PointerEvent::Move { dx: x, dy: y });
                }
            }
            Motion::Absolute { tip, x_max, y_max } => {
                let Some(touching) = tip.read(payload) else {
                    return;
                };
                if touching == 0 {
                    self.last = None;
                    self.rest = (0, 0);
                } else {
                    if let Some((lx, ly)) = self.last {
                        let dx = scale(x - lx, x_max, &mut self.rest.0);
                        // The same units on both axes: Y is scaled by the pad's width too.
                        let dy = scale(y - ly, x_max.max(y_max), &mut self.rest.1);
                        if dx != 0 || dy != 0 {
                            out(PointerEvent::Move { dx, dy });
                        }
                    }
                    self.last = Some((x, y));
                }
            }
        }
        if let Some(w) = f.wheel.and_then(|w| w.read(payload)).filter(|w| *w != 0) {
            out(PointerEvent::Wheel(w));
        }
        if let Some((offset, count)) = f.buttons {
            let count = count.min(8);
            let Some(now) = bits(payload, offset, count) else {
                return;
            };
            for index in 0..count {
                let bit = 1 << index;
                if (self.buttons ^ now) & bit != 0 {
                    out(PointerEvent::Button {
                        index: index as u8,
                        pressed: now & bit != 0,
                    });
                }
            }
            self.buttons = now;
        }
    }
}

/// `delta` in a logical range of `max` as pointer units, carrying the remainder in `rest`.
fn scale(delta: i32, max: i32, rest: &mut i64) -> i32 {
    let range = i64::from(max.max(1));
    let total = i64::from(delta) * PAD_WIDTH_UNITS + *rest;
    *rest = total % range;
    (total / range) as i32
}

/// A key press or release.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct KeyEvent {
    /// HID usage on the Keyboard/Keypad or Consumer page.
    pub usage: u8,
    /// Linux key code (`KEY_*`).
    pub code: u16,
    pub pressed: bool,
}

/// Turns keyboard and consumer control reports into key events.
pub struct Keyboard {
    format: KeyboardFormat,
    pressed: [u32; 8],
    /// Consumer page usages held now, one bit per usage 0..1024.
    consumer_pressed: [u32; 32],
    radio_pressed: bool,
}

impl Keyboard {
    pub fn new(format: KeyboardFormat) -> Self {
        Keyboard {
            format,
            pressed: [0; 8],
            consumer_pressed: [0; 32],
            radio_pressed: false,
        }
    }

    /// A Wireless Radio Button report: [`KEY_RFKILL`] pressed while its bit is 1 (`hid-input.c` maps `HID_GD_RFKILL_BTN`
    /// to `KEY_RFKILL`). Whether `report` was that report.
    fn feed_radio(&mut self, report: &[u8], out: &mut impl FnMut(KeyEvent)) -> bool {
        let Some((id, bit)) = self.format.radio else {
            return false;
        };
        let payload = match (id, report.split_first()) {
            (0, _) if self.format.report_id.is_none() => report,
            (id, Some((&first, rest))) if id != 0 && first == id => rest,
            _ => return false,
        };
        let Some(value) = bits(payload, bit, 1) else {
            return true;
        };
        let is = value != 0;
        if is != self.radio_pressed {
            self.radio_pressed = is;
            out(KeyEvent { usage: 0xc6, code: KEY_RFKILL, pressed: is });
        }
        true
    }

    /// Decodes one input report (as [`Input::Report`] gives it) and calls `out` for each key that changed:
    /// releases first, then presses. Keyboard and consumer control reports are told apart by their report ID.
    /// Reports with another report ID, too short ones and phantom-state reports (ErrorRollOver) change nothing.
    pub fn feed(&mut self, report: &[u8], mut out: impl FnMut(KeyEvent)) {
        if self.format.report_id.is_some() && self.feed_radio(report, &mut out) {
            return;
        }
        let (payload, consumer) = match self.format.report_id {
            None => (report, false),
            Some(id) => match report.split_first() {
                Some((&first, rest)) if first == id => (rest, false),
                _ => match self.format.consumer_report_id {
                    Some(cid) => match report.split_first() {
                        Some((&first, rest)) if first == cid => (rest, true),
                        _ => return,
                    },
                    None => return,
                },
            },
        };
        if consumer {
            self.feed_consumer(payload, &mut out);
            return;
        }
        let mut now = [0u32; 8];
        for f in self.format.fields() {
            for n in 0..f.count {
                let Some(value) = bits(payload, f.bit_offset + n * f.size, f.size) else {
                    return;
                };
                let usage = if f.variable {
                    if value == 0 {
                        continue;
                    }
                    f.usage_min + n
                } else {
                    let index = value as i64 - i64::from(f.logical_min);
                    if index < 0 {
                        continue;
                    }
                    f.usage_min + index as u32
                };
                match usage {
                    0 => {}
                    // ErrorRollOver: too many keys; the report says nothing about which.
                    1 => return,
                    u if u < 256 => now[(u / 32) as usize] |= 1 << (u % 32),
                    _ => {}
                }
            }
        }
        for pass in [false, true] {
            for u in 0..256u32 {
                let (w, b) = ((u / 32) as usize, 1 << (u % 32));
                let (was, is) = (self.pressed[w] & b != 0, now[w] & b != 0);
                if was != is && is == pass {
                    let code = u16::from(HID_KEYBOARD[u as usize]);
                    if code != 0 {
                        out(KeyEvent {
                            usage: u as u8,
                            code,
                            pressed: is,
                        });
                    }
                }
            }
        }
        self.pressed = now;
    }

    /// One consumer control report: an array lists the usages held now, a variable bit per usage is 1 while held.
    /// Usages Linux does not map are ignored and do not disturb the diffing of the mapped ones.
    fn feed_consumer(&mut self, payload: &[u8], out: &mut impl FnMut(KeyEvent)) {
        let mut now = [0u32; 32];
        for f in self.format.consumer_fields() {
            for n in 0..f.count {
                let Some(value) = bits(payload, f.bit_offset + n * f.size, f.size) else {
                    return;
                };
                let usage = if f.variable {
                    if value == 0 {
                        continue;
                    }
                    f.usage(n)
                } else {
                    let index = value as i64 - i64::from(f.logical_min);
                    if index < 0 {
                        continue;
                    }
                    f.usage_min + index as u32
                };
                if usage == 0 || usage >= 1024 {
                    continue;
                }
                if consumer_key(usage as u16).is_some() {
                    now[(usage / 32) as usize] |= 1 << (usage % 32);
                }
            }
        }
        for pass in [false, true] {
            for u in 0..1024u32 {
                let (w, b) = ((u / 32) as usize, 1 << (u % 32));
                let (was, is) = (self.consumer_pressed[w] & b != 0, now[w] & b != 0);
                if was != is && is == pass && let Some(code) = consumer_key(u as u16) {
                    out(KeyEvent {
                        usage: u as u8,
                        code,
                        pressed: is,
                    });
                }
            }
        }
        self.consumer_pressed = now;
    }
}

/// `size` bits (at most 32) at bit `offset` of `data`, least significant bit first.
fn bits(data: &[u8], offset: u32, size: u32) -> Option<u32> {
    if size == 0 || size > 32 {
        return None;
    }
    let mut v = 0u64;
    for k in 0..size {
        let bit = offset + k;
        let byte = *data.get((bit / 8) as usize)?;
        v |= u64::from((byte >> (bit % 8)) & 1) << k;
    }
    Some(v as u32)
}

/// KEY_UNKNOWN.
const UNK: u8 = 240;

/// HID usage (Keyboard/Keypad page) to Linux key code: `hid_keyboard[]` of `drivers/hid/hid-input.c` (Linux 7.1.10).
#[rustfmt::skip]
const HID_KEYBOARD: [u8; 256] = [
      0,  0,  0,  0, 30, 48, 46, 32, 18, 33, 34, 35, 23, 36, 37, 38,
     50, 49, 24, 25, 16, 19, 31, 20, 22, 47, 17, 45, 21, 44,  2,  3,
      4,  5,  6,  7,  8,  9, 10, 11, 28,  1, 14, 15, 57, 12, 13, 26,
     27, 43, 43, 39, 40, 41, 51, 52, 53, 58, 59, 60, 61, 62, 63, 64,
     65, 66, 67, 68, 87, 88, 99, 70,119,110,102,104,111,107,109,106,
    105,108,103, 69, 98, 55, 74, 78, 96, 79, 80, 81, 75, 76, 77, 71,
     72, 73, 82, 83, 86,127,116,117,183,184,185,186,187,188,189,190,
    191,192,193,194,134,138,130,132,128,129,131,137,133,135,136,113,
    115,114,UNK,UNK,UNK,121,UNK, 89, 93,124, 92, 94, 95,UNK,UNK,UNK,
    122,123, 90, 91, 85,UNK,UNK,UNK,UNK,UNK,UNK,UNK,111,UNK,UNK,UNK,
    UNK,UNK,UNK,UNK,UNK,UNK,UNK,UNK,UNK,UNK,UNK,UNK,UNK,UNK,UNK,UNK,
    UNK,UNK,UNK,UNK,UNK,UNK,179,180,UNK,UNK,UNK,UNK,UNK,UNK,UNK,UNK,
    UNK,UNK,UNK,UNK,UNK,UNK,UNK,UNK,UNK,UNK,UNK,UNK,UNK,UNK,UNK,UNK,
    UNK,UNK,UNK,UNK,UNK,UNK,UNK,UNK,111,UNK,UNK,UNK,UNK,UNK,UNK,UNK,
     29, 42, 56,125, 97, 54,100,126,164,166,165,163,161,115,114,113,
    150,158,159,128,136,177,178,176,142,152,173,140,UNK,UNK,UNK,UNK,
];

/// KEY_BRIGHTNESSDOWN (`include/uapi/linux/input-event-codes.h`).
pub const KEY_BRIGHTNESSDOWN: u16 = 224;
/// KEY_RFKILL (`include/uapi/linux/input-event-codes.h`): the Wireless Radio Button.
pub const KEY_RFKILL: u16 = 247;

/// KEY_BRIGHTNESSUP (`include/uapi/linux/input-event-codes.h`).
pub const KEY_BRIGHTNESSUP: u16 = 225;

/// HID usage on the Consumer page (0x0C) to its Linux key code, as `hidinput_configure_usage` in
/// `drivers/hid/hid-input.c` maps the HID_UP_CONSUMER usages. None for the usages Linux leaves unmapped.
pub fn consumer_key(usage: u16) -> Option<u16> {
    Some(match usage {
        0x006f => KEY_BRIGHTNESSUP,
        0x0070 => KEY_BRIGHTNESSDOWN,
        0x0072 => 431, // KEY_BRIGHTNESS_TOGGLE
        0x0073 => 592, // KEY_BRIGHTNESS_MIN
        0x0074 => 593, // KEY_BRIGHTNESS_MAX
        0x0075 => 244, // KEY_BRIGHTNESS_AUTO
        0x00b5 => 163, // KEY_NEXTSONG
        0x00b6 => 165, // KEY_PREVIOUSSONG
        0x00b7 => 166, // KEY_STOPCD
        0x00cd => 164, // KEY_PLAYPAUSE
        0x00e2 => 113, // KEY_MUTE
        0x00e9 => 115, // KEY_VOLUMEUP
        0x00ea => 114, // KEY_VOLUMEDOWN
        0x0183 => 171, // KEY_CONFIG
        0x018a => 155, // KEY_MAIL
        0x0192 => 140, // KEY_CALC
        0x0194 => 144, // KEY_FILE
        0x0196 => 150, // KEY_WWW
        0x0221 => 217, // KEY_SEARCH
        0x0223 => 172, // KEY_HOMEPAGE
        0x0224 => 158, // KEY_BACK
        0x0225 => 159, // KEY_FORWARD
        0x0227 => 173, // KEY_REFRESH
        _ => return None,
    })
}

/// Every key code [`consumer_key`] can produce, for the input device's capabilities.
pub fn consumer_keys() -> impl Iterator<Item = u16> {
    CONSUMER_KEYS.iter().copied()
}

/// The codes of [`consumer_key`], in usage order.
const CONSUMER_KEYS: [u16; 23] = [
    225, 224, 431, 592, 593, 244, 163, 165, 166, 164, 113, 115, 114, 171, 155, 140, 144, 150, 217, 172,
    158, 159, 173,
];

pub mod fnrow;
pub mod setup;
pub use setup::{SETUP_RETRY_MS, SETUP_TRIES, setup_retry_ms};
pub mod wait;
pub mod ptp;
pub mod samsung_ec;
pub mod touchpad;
pub mod tp_accel;
pub mod tp_tap;
pub mod tp_buttons;
pub mod tp_rules;

#[cfg(test)]
mod tests;
