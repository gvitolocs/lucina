//! The control loop: what the kernel's thread runs every [`Controller::tick_checked`], against any [`Smbus`].
//!
//! Every tick reads the charger's IDs and the gauge; a pass (at most every [`PERIOD`](crate::PERIOD)) relays the
//! gauge's requests. A stop reason acts on the next tick, up to [`READ_PERIOD_MS`] later, not on the next pass.
//!
//! This file gathers the facts and carries out the answer. What may be written is decided by [`plan`] in `rules.rs`,
//! which Verus proves.
//!
//! Safety, beyond that:
//!
//! * Only [`Mode::On`] writes, and only MaxChargeVoltage and ChargeCurrent. Nothing is ever written before the chip has
//!   answered with the BQ25710's IDs.
//! * The only values that can reach the voltage and current registers pass through [`Setpoint::checked`].
//! * Anything unexpected writes nothing, or ChargeCurrent = 0 if the charger is charging: a bus error, the IDs
//!   changing, a watchdog that is not armed, the charger holding more than the caps, a value that does not read back,
//!   the gauge measuring more than the charger was told. The last three mean that something else drives this chip, or
//!   that the bus is lying: they latch until reboot, as does a gauge that stops changing while it is charged.
//! * The original tolerates one failed pass because the charger's watchdog covers it; here a failed read while
//!   charging writes 0 at once, and a good tick resumes. A NACK alone is retried first ([`READ_TRIES`]).
//!
//! If the kernel says it is not healthy, if this thread is held up, or if the kernel dies, nothing refreshes the
//! charger, and its own watchdog stops charging within 175 s.

use core::fmt;

use crate::rules::{
    CHARGE_LIMIT_MS, Facts, PRECHARGE_BELOW_MV, PRECHARGE_LIMIT_MS, current_over, full_after, gauge_fresh,
};
use crate::supply::Snapshot;
use crate::{Action, Band, Blocked, Mode, Options, Reading, Smbus, bq25710, next_band, plan, sbs};

/// How often the gauge is read: the time a stop rule, or a plugged adapter on the status bar, may wait.
pub const READ_PERIOD_MS: u64 = 10_000;
/// After [`BACKOFF_AFTER`] failed ticks in a row: a timed-out transfer spins for a second, so a dead bus is not polled
/// every 10 s. Still below the watchdog's 175 s.
pub const BACKOFF_MS: u64 = 60_000;
pub const BACKOFF_AFTER: u32 = 3;
/// Tries per word read, for errors [`Smbus::may_retry`] allows.
pub const READ_TRIES: u32 = 3;
/// A tick later than this after the last one means this thread was held up: that round writes no setpoint.
pub const LATE_TICK_MS: u64 = 90_000;
/// Rounds in a row the gauge may measure too much before that counts: its reading lags a change of the register.
pub const OVER_CURRENT_ROUNDS: u32 = 3;
/// How often the charger's ADC converts by default, while the adapter is in: power figures once a second, as Chromium's EC
/// reports them on demand.
pub const ADC_PERIOD_MS: u64 = 1000;

const PASS_MS: u64 = crate::PERIOD.as_millis() as u64;

/// Values for the charger, within the caps.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Setpoint {
    mv: u16,
    ma: u16,
}

impl Setpoint {
    /// `None` unless both are within the hard caps and on the registers' steps. [`plan`] cannot produce such
    /// values; this holds even if it someday could.
    pub fn checked(mv: u16, ma: u16) -> Option<Setpoint> {
        let on_steps = mv.is_multiple_of(8) && ma.is_multiple_of(64);
        (on_steps && mv <= crate::VMAX_MV && ma <= crate::DEFAULT_CAP_MA)
            .then_some(Setpoint { mv, ma })
    }
}

/// What the last tick came to, so that a line is printed when that changes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Outcome {
    Bus { write: bool, addr: u16, reg: u8 },
    NotBq25710 { manufacturer: u16, device: u16 },
    Readback { reg: u8, wrote: u16, read: u16 },
    Blocked(Blocked),
    Charging { mv: u16, ma: u16 },
}

/// What a tick leaves for the kernel.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Tick {
    pub snapshot: Snapshot,
    /// Milliseconds until the next tick.
    pub next_ms: u64,
}

/// What the gauge said last, to see whether it still says anything new.
type GaugeSigns = (u16, Option<i16>, u16, Option<u16>);

pub struct Controller {
    options: Options,
    /// The time of the last completed pass.
    last_pass_ms: Option<u64>,
    last_tick_ms: Option<u64>,
    /// This round stopped something unexpected: the next good round passes at once.
    resume_now: bool,
    /// The charger may be charging because of a write of ours.
    charging: bool,
    /// The chip has answered with the right IDs at least once.
    verified: bool,
    /// Something drives this chip or the bus lies: never again more than ChargeCurrent = 0.
    latched: bool,
    failures: u32,
    band: Band,
    full: bool,
    last_adapter: Option<bool>,
    /// The first setpoint since boot, for `lucina.charge.window`.
    first_write_ms: Option<u64>,
    /// When this charge began, and when it last began below the precharge voltage.
    charge_since_ms: Option<u64>,
    precharge_since_ms: Option<u64>,
    over_current_rounds: u32,
    gauge_signs: Option<GaugeSigns>,
    gauge_changed_ms: u64,
    identified: bool,
    last: Option<Outcome>,
    last_ids: Option<(u16, u16, u16)>,
    last_registers: Option<(u16, u16, Option<u16>)>,
    last_read_only: Option<[Option<u16>; 8]>,
    last_battery: Option<(Option<u16>, Option<&'static str>)>,
    snapshot: Snapshot,
    /// The ADC is off for good: a conversion changed a charge register.
    adc_off: bool,
    last_adc: Option<AdcOutcome>,
    last_adc_line_ms: Option<u64>,
    /// The input limit last said in report, so that it is said once.
    said_iin: Option<u16>,
    /// Since when the adapter has been in without a positive battery current.
    dry_since_ms: Option<u64>,
}

/// What the last conversion came to, so that a line is printed when that changes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum AdcOutcome {
    Fine,
    LowPower,
    Failed,
}

/// One line for the screen, without the `charge:` prefix.
pub type Log<'a> = dyn FnMut(fmt::Arguments<'_>) + 'a;

fn rd<B: Smbus>(bus: &mut B, addr: u16, reg: u8) -> Result<u16, B::Error> {
    let mut tries = READ_TRIES;
    loop {
        match bus.read_word(addr, reg) {
            Ok(value) => return Ok(value),
            Err(e) if tries > 1 && B::may_retry(&e) => tries -= 1,
            Err(e) => return Err(e),
        }
    }
}

impl Controller {
    pub fn new(mode: Mode) -> Controller {
        Controller::with_options(Options {
            mode,
            cap_ma: crate::DEFAULT_CAP_MA,
            window_ms: None,
            trace_ms: None,
            adc_ms: None,
            iin_on: false,
        })
    }

    pub fn with_options(options: Options) -> Controller {
        Controller {
            options,
            last_pass_ms: None,
            last_tick_ms: None,
            resume_now: false,
            charging: false,
            verified: false,
            latched: false,
            failures: 0,
            band: Band::Normal,
            full: false,
            last_adapter: None,
            first_write_ms: None,
            charge_since_ms: None,
            precharge_since_ms: None,
            over_current_rounds: 0,
            gauge_signs: None,
            gauge_changed_ms: 0,
            identified: false,
            last: None,
            last_ids: None,
            last_registers: None,
            last_read_only: None,
            last_battery: None,
            snapshot: Snapshot::EMPTY,
            adc_off: false,
            last_adc: None,
            last_adc_line_ms: None,
            said_iin: None,
            dry_since_ms: None,
        }
    }

    /// How often [`Controller::adc`] wants to run, if at all.
    pub fn adc_period_ms(&self) -> Option<u64> {
        match self.options.mode {
            Mode::Off => None,
            _ => self.options.adc_ms,
        }
    }

    /// One ADC conversion at `now_ms`, for the power figures. Only after this round's IDs were the BQ25710's, and never in
    /// `off`. Its only write is ADCOption ([`crate::adc`]). `wait_ms` sleeps between polls.
    pub fn adc<B: Smbus>(&mut self, bus: &mut B, now_ms: u64, wait_ms: &mut dyn FnMut(u64), log: &mut Log<'_>) -> Snapshot
    where
        B::Error: fmt::Display,
    {
        let known = self.last_ids.is_some_and(|(_, _, known)| known == 1);
        if self.adc_period_ms().is_none() || self.adc_off || !known || self.failures >= BACKOFF_AFTER {
            self.snapshot.adc = None;
            return self.snapshot;
        }
        let result = crate::adc::convert(bus, wait_ms);
        let outcome = match &result {
            Ok(_) => AdcOutcome::Fine,
            Err(crate::adc::AdcError::LowPower) => AdcOutcome::LowPower,
            Err(_) => AdcOutcome::Failed,
        };
        let changed = self.last_adc != Some(outcome);
        self.last_adc = Some(outcome);
        match result {
            Ok(c) => {
                self.snapshot.adc = Some((now_ms, c.adc));
                let minute = self.last_adc_line_ms.is_none_or(|t| now_ms.saturating_sub(t) >= PASS_MS);
                if changed || (minute && self.options.mode == Mode::Report) {
                    self.last_adc_line_ms = Some(now_ms);
                    let a = c.adc;
                    log(format_args!(
                        "adc: VBUS {} mV, IIN {} mA, {} mW in, VSYS {} mV, VBAT {} mV, PSYS {}; ADCOption 0x{:04x} -> {}, {} polls of 2 ms",
                        Num(a.vbus_mv),
                        a.iin_ma,
                        Num(a.power_mw()),
                        a.vsys_mv,
                        a.vbat_mv,
                        match a.psys_mv {
                            Some(_) => "on",
                            None => "off (ChargeOption1 EN_PSYS clear)",
                        },
                        c.option_before,
                        match c.wrote {
                            Some(v) => AdcWrote(Some(v)),
                            None => AdcWrote(None),
                        },
                        c.polls,
                    ));
                }
            }
            Err(e) => {
                self.snapshot.adc = None;
                use crate::adc::AdcError;
                if let AdcError::ChargeChanged { .. } = e {
                    self.adc_off = true;
                }
                if changed || matches!(e, AdcError::ChargeChanged { .. }) {
                    match e {
                        AdcError::LowPower => log(format_args!(
                            "adc: battery only, low power mode: no ADC, nothing written"
                        )),
                        AdcError::Bus { write, reg, error } => log(format_args!(
                            "adc: error: {} 0x09 reg 0x{reg:02x}: {error}",
                            if write { "write" } else { "read" }
                        )),
                        AdcError::Timeout { option } => log(format_args!(
                            "adc: ADC_START still set after {} ms (ADCOption 0x{option:04x})",
                            crate::adc::POLLS as u64 * crate::adc::POLL_MS
                        )),
                        AdcError::ReadBack { wrote, read } => log(format_args!(
                            "adc: ADCOption written 0x{wrote:04x}, reads 0x{read:04x}"
                        )),
                        AdcError::ChargeChanged { reg, before, after } => log(format_args!(
                            "adc: unexpected: register 0x{reg:02x} went 0x{before:04x} -> 0x{after:04x} across a conversion; ADC off until restart"
                        )),
                    }
                }
            }
        }
        self.snapshot
    }

    /// One round at `now_ms` (monotonic), for a kernel that is healthy.
    pub fn tick<B: Smbus>(&mut self, bus: &mut B, now_ms: u64, log: &mut Log<'_>) -> Tick
    where
        B::Error: fmt::Display,
    {
        self.tick_checked(bus, now_ms, true, log)
    }

    /// One round at `now_ms` (monotonic). `healthy`: the kernel's own view of itself. Without it no setpoint goes out,
    /// so that the charger's watchdog ends charging on a machine that is failing.
    pub fn tick_checked<B: Smbus>(
        &mut self,
        bus: &mut B,
        now_ms: u64,
        healthy: bool,
        log: &mut Log<'_>,
    ) -> Tick
    where
        B::Error: fmt::Display,
    {
        if self.options.mode == Mode::Off {
            return Tick {
                snapshot: Snapshot::EMPTY,
                next_ms: u64::MAX,
            };
        }
        let due = self
            .last_pass_ms
            .is_none_or(|t| now_ms.saturating_sub(t) >= PASS_MS);
        let on_time = self
            .last_tick_ms
            .is_none_or(|t| now_ms.saturating_sub(t) <= LATE_TICK_MS);
        self.last_tick_ms = Some(now_ms);
        if self.round(bus, now_ms, due, healthy && on_time, log).is_ok() {
            self.failures = 0;
            if due {
                self.last_pass_ms = Some(now_ms);
            }
        } else {
            self.failures = self.failures.saturating_add(1);
        }
        if core::mem::take(&mut self.resume_now) {
            self.last_pass_ms = None;
        }
        let adc = self.snapshot.adc;
        if self.failures >= BACKOFF_AFTER {
            self.snapshot = Snapshot::EMPTY;
        } else {
            // A tick's fresh reading keeps the last conversion; [`Controller::adc`] replaces it.
            self.snapshot.adc = adc;
        }
        Tick {
            snapshot: self.snapshot,
            next_ms: if self.failures >= BACKOFF_AFTER {
                BACKOFF_MS
            } else {
                self.options.trace_ms.unwrap_or(READ_PERIOD_MS)
            },
        }
    }

    /// The outcome is new: its line should be printed.
    fn changed(&mut self, outcome: Outcome) -> bool {
        let changed = self.last != Some(outcome);
        self.last = Some(outcome);
        changed
    }

    fn round<B: Smbus>(
        &mut self,
        bus: &mut B,
        now_ms: u64,
        mut due: bool,
        healthy: bool,
        log: &mut Log<'_>,
    ) -> Result<(), ()>
    where
        B::Error: fmt::Display,
    {
        let mode = self.options.mode;
        let c = bq25710::ADDR;
        let manufacturer = self.read_or_stop(bus, c, bq25710::MANUFACTURER_ID, log)?;
        let device = self.read_or_stop(bus, c, bq25710::DEVICE_ID, log)?;
        let known = (manufacturer, device) == (bq25710::TI, bq25710::BQ25710);
        let ids = (manufacturer, device, u16::from(known));
        if self.last_ids != Some(ids) {
            self.last_ids = Some(ids);
            log(format_args!(
                "charger 0x{c:02x}: ManufacturerID 0x{manufacturer:04x}, DeviceID 0x{device:04x}{}",
                if known {
                    " (BQ25710)"
                } else {
                    " (not a BQ25710: nothing is written to it)"
                }
            ));
        }
        self.verified |= known;

        let reading = match Reading::read(|reg| rd(bus, sbs::ADDR, reg).map_err(|e| (reg, e))) {
            Ok(r) => r,
            Err((reg, e)) => {
                let cc = if known {
                    rd(bus, c, bq25710::CHARGE_CURRENT).ok()
                } else {
                    None
                };
                self.bus_error(bus, false, sbs::ADDR, reg, &e, cc, log);
                return Err(());
            }
        };
        self.read_state(bus, &reading);
        self.identify(bus, log);
        if due && mode == Mode::Report {
            log(format_args!(
                "gauge 0x{:02x}: status 0x{:04x}, {} C, battery {} mV, asks {} mV {} mA",
                sbs::ADDR,
                reading.status,
                reading.temp_c(),
                reading.battery_mv,
                reading.req_mv,
                reading.req_ma
            ));
        }
        if !known {
            self.battery_line(due, log);
            if self.changed(Outcome::NotBq25710 {
                manufacturer,
                device,
            }) {
                log(format_args!(
                    "unexpected chip at 0x{c:02x}: leaving it alone"
                ));
            }
            self.unexpected(bus, None, log);
            return Ok(());
        }

        let option0 = self.read_or_stop(bus, c, bq25710::CHARGE_OPTION_0, log)?;
        let cc = self.read_or_stop(bus, c, bq25710::CHARGE_CURRENT, log)?;
        let cv = self.read_or_stop(bus, c, bq25710::MAX_CHARGE_VOLTAGE, log)?;
        let status = rd(bus, c, bq25710::CHARGER_STATUS).ok();
        let adapter = status.map(|s| s & bq25710::AC_STAT != 0);
        self.snapshot.ac_online = adapter;
        let flowing = self.snapshot.current_ma.is_some_and(|c| c > 0);
        // Dry counts from the cable too: a cable in whose power never comes is a fault after the 5 s.
        self.dry_since_ms = match crate::rules::cable_online(self.snapshot.cable, adapter) {
            Some(true) if !flowing => Some(self.dry_since_ms.unwrap_or(now_ms)),
            _ => None,
        };
        self.snapshot.dry_ms = self.dry_since_ms.map(|t| now_ms.saturating_sub(t));
        self.battery_line(due, log);
        let registers = (cc, cv, status);
        if self.last_registers != Some(registers) || (due && mode == Mode::Report) {
            self.last_registers = Some(registers);
            log(format_args!(
                "charger now: ChargeOption0 0x{option0:04x}, ChargeCurrent {cc} mA, MaxChargeVoltage {cv} mV, \
                 ChargerStatus {}",
                Hex(status)
            ));
        }
        if due {
            self.read_only_line(bus, log);
        }
        if self.options.trace_ms.is_some() {
            log(format_args!(
                "t {now_ms} ms: {} C, {} mV, {} mA, {}%, gauge 0x{:04x} asks {} mV {} mA; charger {cv} mV {cc} mA 0x{:04x}",
                Tenths(self.snapshot.temp_tenths_c()),
                reading.battery_mv,
                Num(self.snapshot.current_ma),
                Num(self.snapshot.capacity),
                reading.status,
                reading.req_mv,
                reading.req_ma,
                status.unwrap_or(0),
            ));
        }

        // A plug put back: the chip cleared ChargeCurrent, so a pass now. A plug pulled: that charge is over.
        if adapter == Some(true) && self.last_adapter == Some(false) {
            due = true;
        }
        if adapter == Some(false) {
            self.charge_since_ms = None;
            self.precharge_since_ms = None;
            self.charging = false;
        }
        self.last_adapter = adapter;
        if adapter == Some(true) {
            self.input_limit(bus, mode, log);
        } else {
            self.said_iin = None;
        }

        self.band = next_band(self.band, reading.temp_dk);
        self.full = full_after(self.full, reading.status, self.snapshot.capacity);
        if self.full {
            self.charge_since_ms = None;
            self.precharge_since_ms = None;
        }
        if reading.battery_mv >= PRECHARGE_BELOW_MV {
            self.precharge_since_ms = None;
        }

        // The gauge's numbers count as frozen only while they should be moving.
        let signs = (
            reading.battery_mv,
            self.snapshot.current_ma,
            reading.temp_dk,
            self.snapshot.charge_now_mah,
        );
        if self.gauge_signs != Some(signs) || !self.charging || cc == 0 {
            self.gauge_changed_ms = now_ms;
        }
        self.gauge_signs = Some(signs);

        let measured_over = self
            .snapshot
            .current_ma
            .is_some_and(|ma| current_over(ma, cc));
        self.over_current_rounds = if self.charging && measured_over {
            self.over_current_rounds.saturating_add(1)
        } else {
            0
        };

        let within = |since: Option<u64>, limit: u64| {
            since.is_none_or(|t| now_ms.saturating_sub(t) < limit)
        };
        let facts = Facts {
            ids_known: true,
            verified: self.verified,
            latched: self.latched,
            healthy,
            watchdog_set: option0 & bq25710::WATCHDOG_MASK == bq25710::WATCHDOG_MASK,
            chip_ma: cc,
            chip_mv: cv,
            adapter: adapter == Some(true),
            charging: self.charging,
            over_current: self.over_current_rounds >= OVER_CURRENT_ROUNDS,
            fresh: gauge_fresh(now_ms, self.gauge_changed_ms),
            timer_left: within(self.charge_since_ms, CHARGE_LIMIT_MS)
                && within(self.precharge_since_ms, PRECHARGE_LIMIT_MS),
            window_open: self
                .options
                .window_ms
                .is_none_or(|window| within(self.first_write_ms, window)),
            full: self.full,
            due,
            cap_ma: self.options.cap_ma,
            band: self.band,
        };
        let plan = plan(&facts, &reading);
        if let Some(why) = plan.blocked {
            self.say_blocked(why, &reading, option0, cc, cv, due, log);
            // Over the caps before our first write is the firmware's own boot value (2880 mA seen on 2026-11-05): it is
            // stopped, and our setpoint replaces it at the next pass. After a write of ours it means something else drives
            // the chip.
            let latch = match why {
                Blocked::OverCaps => self.first_write_ms.is_some(),
                Blocked::OverCurrent | Blocked::Stale => true,
                _ => false,
            };
            if mode == Mode::On && latch {
                self.latched = true;
            }
        }
        match plan.action {
            Action::Nothing => Ok(()),
            Action::Zero { announce } => {
                self.write_current_zero(bus, log, announce);
                // The next good round may resume at once, unless this was an ordinary stop.
                self.resume_now = !matches!(plan.blocked, Some(Blocked::Stop(_) | Blocked::WaitRecharge));
                Ok(())
            }
            Action::Write { mv, ma } => {
                self.pass(bus, &reading, now_ms, mv, ma, log)?;
                if due {
                    self.last_pass_ms = Some(now_ms);
                }
                Ok(())
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn say_blocked(
        &mut self,
        why: Blocked,
        r: &Reading,
        option0: u16,
        cc: u16,
        cv: u16,
        due: bool,
        log: &mut Log<'_>,
    ) {
        let again = due && self.options.mode == Mode::Report && matches!(why, Blocked::Stop(_));
        if !self.changed(Outcome::Blocked(why)) && !again {
            return;
        }
        match why {
            Blocked::UnknownChip => log(format_args!("unexpected chip: leaving it alone")),
            Blocked::Latched => log(format_args!(
                "refusing to charge until reboot; see the line above"
            )),
            Blocked::Unhealthy => log(format_args!(
                "the kernel is not healthy: no refresh, the charger's watchdog ends charging within 175 s"
            )),
            Blocked::OverCaps => log(format_args!(
                "unexpected: the charger holds {cv} mV, {cc} mA, over the caps ({} mV, {} mA)",
                crate::VMAX_MV,
                crate::DEFAULT_CAP_MA
            )),
            Blocked::WatchdogNotSet => log(format_args!(
                "unexpected: ChargeOption0 0x{option0:04x}: the 175 s watchdog is not set"
            )),
            Blocked::OverCurrent => log(format_args!(
                "unexpected: the gauge measures {} mA, the charger holds {cc} mA",
                Num(self.snapshot.current_ma)
            )),
            Blocked::Stale => log(format_args!(
                "stop: the gauge's numbers have not changed for {} s of charging",
                crate::rules::FROZEN_LIMIT_MS / 1000
            )),
            Blocked::Timer => log(format_args!(
                "stop: this charge has lasted too long; it starts over when the adapter comes back"
            )),
            Blocked::NoAdapter => log(format_args!("no adapter: nothing to charge from")),
            Blocked::Window => log(format_args!(
                "stop: lucina.charge.window is over; only a restart charges again"
            )),
            Blocked::WaitRecharge => log(format_args!(
                "full: charges again at {}%",
                crate::rules::RECHARGE_AT_PERCENT
            )),
            Blocked::Stop(stop) => log(format_args!(
                "stop: {stop} (battery {} mV, status 0x{:04x})",
                r.battery_mv, r.status
            )),
        }
    }

    /// The numbers beside the reading that the stop rules do not depend on: `None` where a read fails.
    fn read_state<B: Smbus>(&mut self, bus: &mut B, r: &Reading) {
        let mut word = |reg| rd(bus, sbs::ADDR, reg).ok();
        let capacity = word(sbs::RELATIVE_STATE_OF_CHARGE);
        let current_ma = word(sbs::CURRENT).map(|c| c as i16);
        let cycle_count = word(sbs::CYCLE_COUNT);
        // Charges are in mAh only with the capacity mode bit clear, else in 10 mWh, which are not charges.
        let amps = word(sbs::BATTERY_MODE).is_some_and(|m| m & sbs::CAPACITY_MODE == 0);
        let mut charge = |reg| if amps { word(reg) } else { None };
        self.snapshot = Snapshot {
            battery_status: Some(r.status),
            capacity,
            voltage_mv: Some(r.battery_mv),
            current_ma,
            temp_dk: Some(r.temp_dk),
            cycle_count,
            charge_now_mah: charge(sbs::REMAINING_CAPACITY),
            charge_full_mah: charge(sbs::FULL_CHARGE_CAPACITY),
            charge_full_design_mah: charge(sbs::DESIGN_CAPACITY),
            ac_online: self.snapshot.ac_online,
            cable: self.snapshot.cable,
            dry_ms: self.snapshot.dry_ms,
            adc: self.snapshot.adc,
        };
    }

    /// Once: which battery this is. Samsung's limits depend on its name.
    fn identify<B: Smbus>(&mut self, bus: &mut B, log: &mut Log<'_>) {
        if self.identified {
            return;
        }
        self.identified = true;
        let design_mv = rd(bus, sbs::ADDR, sbs::DESIGN_VOLTAGE).ok();
        let spec = rd(bus, sbs::ADDR, sbs::SPECIFICATION_INFO).ok();
        let date = rd(bus, sbs::ADDR, sbs::MANUFACTURE_DATE).ok();
        let mut name = [0u8; 33];
        let mut maker = [0u8; 33];
        let mut chemistry = [0u8; 33];
        let mut block = |reg, buf: &mut [u8; 33]| {
            bus.read_block(sbs::ADDR, reg, buf)
                .is_some_and(|result| result.is_ok())
        };
        let named = block(sbs::DEVICE_NAME, &mut name);
        let made = block(sbs::MANUFACTURER_NAME, &mut maker);
        let chem = block(sbs::DEVICE_CHEMISTRY, &mut chemistry);
        log(format_args!(
            "gauge is \"{}\" by \"{}\", {}, design {} mV, spec {}, made {}",
            Text(named.then_some(&name)),
            Text(made.then_some(&maker)),
            Text(chem.then_some(&chemistry)),
            Num(design_mv),
            Hex(spec),
            Hex(date),
        ));
    }

    /// What the firmware left in the registers this code never writes, when that changes. The last two of
    /// [`bq25710::READ_ONLY`] move with the load, so they do not count as a change.
    fn read_only_line<B: Smbus>(&mut self, bus: &mut B, log: &mut Log<'_>) {
        let mut values = [None; bq25710::READ_ONLY.len()];
        for (value, (reg, _)) in values.iter_mut().zip(bq25710::READ_ONLY) {
            *value = rd(bus, bq25710::ADDR, reg).ok();
        }
        let mut steady = [None; 8];
        steady.copy_from_slice(&values[..8]);
        if self.last_read_only == Some(steady) {
            return;
        }
        self.last_read_only = Some(steady);
        log(format_args!(
            "charger also: {}",
            ReadOnly(&values, &bq25710::READ_ONLY)
        ));
    }

    fn battery_line(&mut self, due: bool, log: &mut Log<'_>) {
        let s = self.snapshot;
        let key = (s.capacity, s.status_text());
        if self.last_battery != Some(key) || (due && self.options.mode == Mode::Report) {
            self.last_battery = Some(key);
            log(format_args!(
                "battery: {}%, {}, {} mV, {} mA, {} C, {}/{} mAh of {} design, {} cycles, adapter {}",
                Num(s.capacity),
                s.status_text().unwrap_or("?"),
                Num(s.voltage_mv),
                Num(s.current_ma),
                Tenths(s.temp_tenths_c()),
                Num(s.charge_now_mah),
                Num(s.charge_full_mah),
                Num(s.charge_full_design_mah),
                Num(s.cycle_count),
                match s.ac_online {
                    Some(true) => "in",
                    Some(false) => "out",
                    None => "?",
                },
            ));
        }
    }

    /// Relays a request. Voltage first, and current only if the voltage reads back.
    fn pass<B: Smbus>(
        &mut self,
        bus: &mut B,
        r: &Reading,
        now_ms: u64,
        mv: u16,
        ma: u16,
        log: &mut Log<'_>,
    ) -> Result<(), ()>
    where
        B::Error: fmt::Display,
    {
        let Some(point) = Setpoint::checked(mv, ma) else {
            // plan() keeps this from happening; a test holds it to that.
            log(format_args!("internal: {mv} mV, {ma} mA refused"));
            self.unexpected(bus, None, log);
            return Err(());
        };
        let (vreg, creg) = (bq25710::MAX_CHARGE_VOLTAGE, bq25710::CHARGE_CURRENT);
        if self.options.mode == Mode::Report {
            log(format_args!(
                "report: would write MaxChargeVoltage {} mV (0x{:04x}), ChargeCurrent {} mA (0x{:04x}); nothing written",
                point.mv, point.mv, point.ma, point.ma
            ));
            self.last = Some(Outcome::Charging { mv, ma });
            return Ok(());
        }
        let c = bq25710::ADDR;
        let quiet = self.last == Some(Outcome::Charging { mv, ma });

        if let Err(e) = bus.write_word(c, vreg, point.mv) {
            self.bus_error(bus, true, c, vreg, &e, None, log);
            return Err(());
        }
        let read = match rd(bus, c, vreg) {
            Ok(v) => v,
            Err(e) => {
                self.bus_error(bus, false, c, vreg, &e, None, log);
                return Err(());
            }
        };
        if read != point.mv {
            return self.readback_failed(bus, vreg, point.mv, read, log);
        }
        if !quiet {
            log(format_args!(
                "wrote MaxChargeVoltage {} mV, reads back {read} mV",
                point.mv
            ));
        }

        // A failed write may still have reached the chip.
        self.charging = true;
        self.first_write_ms.get_or_insert(now_ms);
        self.charge_since_ms.get_or_insert(now_ms);
        if r.battery_mv < PRECHARGE_BELOW_MV {
            self.precharge_since_ms.get_or_insert(now_ms);
        }
        if let Err(e) = bus.write_word(c, creg, point.ma) {
            self.bus_error(bus, true, c, creg, &e, None, log);
            return Err(());
        }
        let read = match rd(bus, c, creg) {
            Ok(v) => v,
            Err(e) => {
                self.bus_error(bus, false, c, creg, &e, None, log);
                return Err(());
            }
        };
        // Less is what the charger does when the adapter is gone: it resets ChargeCurrent to 0.
        if read > point.ma {
            return self.readback_failed(bus, creg, point.ma, read, log);
        }
        self.charging = point.ma != 0;
        if !quiet {
            log(format_args!(
                "wrote ChargeCurrent {} mA, reads back {read} mA",
                point.ma
            ));
        }
        let status = rd(bus, c, bq25710::CHARGER_STATUS).ok();
        if self.changed(Outcome::Charging { mv, ma }) {
            // DEBIAN.md's own result line.
            log(format_args!(
                "{mv} mV, {ma} mA (gauge asks {} mV {} mA; battery {} mV, {} C, charger status {})",
                r.req_mv,
                r.req_ma,
                r.battery_mv,
                r.temp_c(),
                Hex(status)
            ));
        }
        Ok(())
    }

    /// The next round is a pass: the PD thread saw a plug, an unplug or a new contract, after which the charger may have cleared
    /// ChargeCurrent (it does when VBUS drops), so the setpoint goes out now and not at the next minute's keep-alive.
    pub fn pass_now(&mut self) {
        self.last_pass_ms = None;
    }

    /// The PD controller's view of the cable, for the status ([`crate::supply::Snapshot::online`]).
    pub fn set_cable(&mut self, cable: Option<bool>) {
        self.snapshot.cable = cable;
    }

    /// EmuEC's input limit after a PD contract (`ChangeInputCurrent` (address not published): IIN_HOST = mA / 50 in bits 14:8),
    /// from the contract the PD thread confirmed ([`crate::pdic::pd_iin_host`]), decided by
    /// [`crate::pdic_rules::iin_write`]. Written only with `lucina.charge=on` and `lucina.charge.iin=on`; otherwise said
    /// once per value.
    fn input_limit<B: Smbus>(&mut self, bus: &mut B, mode: Mode, log: &mut Log<'_>)
    where
        B::Error: fmt::Display,
    {
        let c = bq25710::ADDR;
        let Ok(now) = rd(bus, c, bq25710::IIN_HOST) else {
            return;
        };
        let Some(v) = crate::pdic_rules::iin_write(crate::pdic::pd_iin_host(), now, true) else {
            return;
        };
        let ma = u32::from(v >> 8) * 50;
        if mode != Mode::On || !self.options.iin_on {
            if self.said_iin != Some(v) {
                self.said_iin = Some(v);
                log(format_args!(
                    "report: would write IIN_HOST 0x{v:04x} ({ma} mA) from the PD contract, now 0x{now:04x}; \
                     nothing written (lucina.charge.iin=on writes it)"
                ));
            }
            return;
        }
        match bus.write_word(c, bq25710::IIN_HOST, v) {
            Ok(()) => {
                let back = rd(bus, c, bq25710::IIN_HOST).ok();
                log(format_args!(
                    "wrote IIN_HOST 0x{v:04x} ({ma} mA) from the PD contract, was 0x{now:04x}, reads back {}",
                    Hex(back)
                ));
            }
            Err(e) => log(format_args!("IIN_HOST write failed: {e}")),
        }
    }

    fn readback_failed<B: Smbus>(
        &mut self,
        bus: &mut B,
        reg: u8,
        wrote: u16,
        read: u16,
        log: &mut Log<'_>,
    ) -> Result<(), ()>
    where
        B::Error: fmt::Display,
    {
        self.latched = true;
        if self.changed(Outcome::Readback { reg, wrote, read }) {
            log(format_args!(
                "unexpected: register 0x{reg:02x} written 0x{wrote:04x}, reads 0x{read:04x}"
            ));
        }
        // The current may not have been written yet, but what the chip does is no longer known.
        self.write_current_zero(bus, log, true);
        Err(())
    }

    fn read_or_stop<B: Smbus>(
        &mut self,
        bus: &mut B,
        addr: u16,
        reg: u8,
        log: &mut Log<'_>,
    ) -> Result<u16, ()>
    where
        B::Error: fmt::Display,
    {
        rd(bus, addr, reg).map_err(|e| self.bus_error(bus, false, addr, reg, &e, None, log))
    }

    #[allow(clippy::too_many_arguments)]
    fn bus_error<B: Smbus>(
        &mut self,
        bus: &mut B,
        write: bool,
        addr: u16,
        reg: u8,
        error: &B::Error,
        charge_current: Option<u16>,
        log: &mut Log<'_>,
    ) where
        B::Error: fmt::Display,
    {
        if self.changed(Outcome::Bus { write, addr, reg }) {
            log(format_args!(
                "error: {} 0x{addr:02x} reg 0x{reg:02x}: {error}",
                if write { "write" } else { "read" },
            ));
        }
        self.unexpected(bus, charge_current, log);
    }

    /// Something is not as it should be: ChargeCurrent = 0 if the charger is charging, else nothing.
    fn unexpected<B: Smbus>(&mut self, bus: &mut B, charge_current: Option<u16>, log: &mut Log<'_>)
    where
        B::Error: fmt::Display,
    {
        if self.charging || charge_current.is_some_and(|cc| cc != 0) {
            self.write_current_zero(bus, log, true);
            self.resume_now = true;
        }
    }

    /// ChargeCurrent = 0, the one write that is always safe once the chip has been identified.
    fn write_current_zero<B: Smbus>(&mut self, bus: &mut B, log: &mut Log<'_>, announce: bool)
    where
        B::Error: fmt::Display,
    {
        if self.options.mode != Mode::On {
            if announce {
                log(format_args!(
                    "report: would write ChargeCurrent 0; nothing written"
                ));
            }
            return;
        }
        if !self.verified {
            return;
        }
        match bus.write_word(bq25710::ADDR, bq25710::CHARGE_CURRENT, 0) {
            Ok(()) => {
                self.charging = false;
                if announce {
                    log(format_args!("wrote ChargeCurrent 0"));
                }
            }
            Err(e) => log(format_args!(
                "error: ChargeCurrent 0 not written: {e}; the charger's watchdog stops charging within 175 s"
            )),
        }
    }
}

/// A number or `?`.
struct Num<T>(Option<T>);

impl<T: fmt::Display> fmt::Display for Num<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.0 {
            Some(value) => value.fmt(f),
            None => f.write_str("?"),
        }
    }
}

/// What a conversion wrote.
struct AdcWrote(Option<u16>);

impl fmt::Display for AdcWrote {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.0 {
            Some(v) => write!(f, "wrote 0x{v:04x}, back to 0x{:04x}", v & !crate::adc::ADC_START),
            None => f.write_str("continuous already, nothing written"),
        }
    }
}

/// A register value or `unread`.
struct Hex(Option<u16>);

impl fmt::Display for Hex {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.0 {
            Some(value) => write!(f, "0x{value:04x}"),
            None => f.write_str("unread"),
        }
    }
}

/// An SMBus block as text: printable ASCII only, whatever the gauge sent.
struct Text<'a>(Option<&'a [u8; 33]>);

impl fmt::Display for Text<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let Some(block) = self.0 else {
            return f.write_str("?");
        };
        let len = usize::from(block[0]).min(32);
        for &byte in &block[1..=len] {
            if byte.is_ascii_graphic() || byte == b' ' {
                fmt::Write::write_char(f, char::from(byte))?;
            }
        }
        Ok(())
    }
}

struct ReadOnly<'a>(&'a [Option<u16>], &'a [(u8, &'a str)]);

impl fmt::Display for ReadOnly<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (value, (_, name)) in self.0.iter().zip(self.1) {
            write!(f, "{name} {} ", Hex(*value))?;
        }
        Ok(())
    }
}

/// Tenths as a decimal, or `?`.
struct Tenths(Option<i32>);

impl fmt::Display for Tenths {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.0 {
            Some(t) => {
                let sign = if t < 0 { "-" } else { "" };
                write!(f, "{sign}{}.{}", t.abs() / 10, t.abs() % 10)
            }
            None => f.write_str("?"),
        }
    }
}
