//! PMIC LDOs behind the RPMh VRM accelerator: the votes `qcom-rpmh-regulator` sends for voltage, enable and mode.
//!
//! Derived from Linux 7.1 `drivers/regulator/qcom-rpmh-regulator.c` (GPL-2.0; Copyright The Linux Foundation,
//! Qualcomm Technologies), with the voltage mapping of `drivers/regulator/helpers.c` and `lib/linear_ranges.c`, and the
//! constraint handling of `drivers/regulator/core.c` (`set_machine_constraints`, `machine_constraints_voltage`). No
//! Scarlet source covers regulators.
//!
//! # What Linux sends for one LDO, in order
//!
//! A VRM resource has three registers: voltage at the resource address, enable at +4, mode at +8
//! ([`REG_VRM_VOLTAGE`], [`REG_ENABLE`], [`REG_VRM_MODE`]).
//!
//! 1. **At registration** (`devm_regulator_register`, before any consumer): the regulator core applies the device tree
//!    constraints. `machine_constraints_voltage` calls `set_voltage_sel` for `regulator-min-microvolt` ..
//!    `regulator-max-microvolt`, the lowest selector whose voltage is at least the minimum. While the regulator has
//!    not been enabled (`vreg->enabled == -EINVAL`) the driver only remembers that selector and **sends nothing**.
//!    Then `regulator-initial-mode` is applied: `rpmh_regulator_vrm_set_mode` sends the **mode** (`wait_for_ack`
//!    true). For `<RPMH_REGULATOR_MODE_HPM>` that is `of_map_mode` HPM -> `REGULATOR_MODE_NORMAL` ->
//!    `pmic_mode_map_pmic5_ldo[NORMAL]` = `PMIC5_LDO_MODE_HPM` = 7.
//! 2. **At the consumer's `regulator_enable`**: `rpmh_regulator_set_enable_state(true)` sees `enabled == -EINVAL`
//!    with a remembered selector, so it first sends the **voltage** (millivolts, rounded up; `wait_for_ack` true),
//!    then the **enable** (data 1, `wait_for_ack` = the enable flag = true).
//!
//! So one LDO is three requests: mode, voltage, enable, each a single-command `rpmh_write` that waits for the
//! acknowledgement. The mode comes first only because Linux sets it at probe; it is one fixed order and not a
//! dependency. `rpmh_write` of a disable is the one asynchronous request (`wait_for_ack` false, unless the node has
//! `qcom,always-wait-for-ack`, which none of the five LDOs here has).
//!
//! The ath10k driver (`snoc.c`) only calls `regulator_bulk_enable`; it never sets a voltage or a load. So the voltage
//! on the wire is the *minimum* of the board's constraint, not a nominal value. Which of the five Wi-Fi supplies
//! that makes how low is in [`WCN3990_LDOS`]. `regulator_bulk_enable` enables its supplies in parallel
//! (`async_schedule_domain`), so the order between *different* LDOs is not defined in Linux; the table order here is
//! the order of `ath10k_regulators[]`.
//!
//! The hardware data (voltage ranges, mode maps) is what `pm6150_vreg_data` and `pm6150l_vreg_data` reference:
//! pm6150 `ldo9` is `pmic5_nldo`; pm6150l `ldo1` is `pmic5_pldo_lv`, `ldo2` is `pmic5_nldo`, `ldo10` and `ldo11` are
//! `pmic5_pldo`. All four use `pmic_mode_map_pmic5_ldo` and `rpmh_regulator_pmic4_ldo_of_map_mode`.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use lucina_qcom_soc::cmd_db::CmdDb;
use lucina_qcom_soc::io::RegIo;

use crate::rsc::TcsCmd;

/// `RPMH_REGULATOR_REG_VRM_VOLTAGE`, `RPMH_REGULATOR_REG_ENABLE`, `RPMH_REGULATOR_REG_VRM_MODE`.
pub const REG_VRM_VOLTAGE: u32 = 0x0;
pub const REG_ENABLE: u32 = 0x4;
pub const REG_VRM_MODE: u32 = 0x8;

/// `PMIC5_LDO_MODE_*`.
pub const PMIC5_LDO_MODE_RETENTION: i32 = 3;
pub const PMIC5_LDO_MODE_LPM: i32 = 4;
pub const PMIC5_LDO_MODE_HPM: i32 = 7;

/// Regulator framework modes (`linux/regulator/consumer.h`).
pub const REGULATOR_MODE_INVALID: u32 = 0x0;
pub const REGULATOR_MODE_FAST: u32 = 0x1;
pub const REGULATOR_MODE_NORMAL: u32 = 0x2;
pub const REGULATOR_MODE_IDLE: u32 = 0x4;
pub const REGULATOR_MODE_STANDBY: u32 = 0x8;

/// `dt-bindings/regulator/qcom,rpmh-regulator.h`.
pub const RPMH_REGULATOR_MODE_RET: u32 = 0;
pub const RPMH_REGULATOR_MODE_LPM: u32 = 1;
pub const RPMH_REGULATOR_MODE_AUTO: u32 = 2;
pub const RPMH_REGULATOR_MODE_HPM: u32 = 3;

const EINVAL: i32 = -22;

/// `pmic_mode_map_pmic5_ldo`, indexed by framework mode. The array has `REGULATOR_MODE_STANDBY + 1` = 9 entries; the
/// designated initialisers leave the indexes that are not a mode (3, 5, 6, 7) at 0. The regulator core only passes
/// the four real modes and [`Vreg::set_mode`] does the same.
pub const PMIC_MODE_MAP_PMIC5_LDO: [i32; 9] = [
    /* INVALID */ EINVAL,
    /* FAST */ EINVAL,
    /* NORMAL */ PMIC5_LDO_MODE_HPM,
    0,
    /* IDLE */ PMIC5_LDO_MODE_LPM,
    0,
    0,
    0,
    /* STANDBY */ PMIC5_LDO_MODE_RETENTION,
];

/// `rpmh_regulator_pmic4_ldo_of_map_mode`: device tree `regulator-initial-mode` to framework mode.
pub fn of_map_mode_ldo(rpmh_mode: u32) -> u32 {
    match rpmh_mode {
        RPMH_REGULATOR_MODE_HPM => REGULATOR_MODE_NORMAL,
        RPMH_REGULATOR_MODE_LPM => REGULATOR_MODE_IDLE,
        RPMH_REGULATOR_MODE_RET => REGULATOR_MODE_STANDBY,
        _ => REGULATOR_MODE_INVALID,
    }
}

/// `struct linear_range`.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct LinearRange {
    pub min: u32,
    pub min_sel: u32,
    pub max_sel: u32,
    pub step: u32,
}

impl LinearRange {
    /// `linear_range_get_max_value`.
    fn max_value(&self) -> u32 {
        self.min
            .wrapping_add((self.max_sel - self.min_sel).wrapping_mul(self.step))
    }

    /// `linear_range_get_value`.
    fn value(&self, sel: u32) -> Option<u32> {
        if self.min_sel > sel || self.max_sel < sel {
            return None;
        }
        Some(self.min + (sel - self.min_sel) * self.step)
    }

    /// `linear_range_get_selector_high`: the lowest selector whose value is at least `val`.
    fn selector_high(&self, val: u32) -> Option<u32> {
        if self.max_value() < val {
            return None;
        }
        if self.min > val {
            return Some(self.min_sel);
        }
        if self.step == 0 {
            return Some(self.max_sel);
        }
        Some((val - self.min).div_ceil(self.step) + self.min_sel)
    }
}

/// The part of `struct rpmh_vreg_hw_data` that matters for an LDO.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct HwData {
    /// The Linux name, such as `pmic5_nldo`.
    pub name: &'static str,
    pub ranges: &'static [LinearRange],
    pub n_voltages: u32,
    pub hpm_min_load_ua: u32,
    pub pmic_mode_map: &'static [i32; 9],
}

/// `pmic5_nldo`.
pub const PMIC5_NLDO: HwData = HwData {
    name: "pmic5_nldo",
    ranges: &[LinearRange {
        min: 320000,
        min_sel: 0,
        max_sel: 123,
        step: 8000,
    }],
    n_voltages: 124,
    hpm_min_load_ua: 30000,
    pmic_mode_map: &PMIC_MODE_MAP_PMIC5_LDO,
};

/// `pmic5_pldo`.
pub const PMIC5_PLDO: HwData = HwData {
    name: "pmic5_pldo",
    ranges: &[LinearRange {
        min: 1504000,
        min_sel: 0,
        max_sel: 255,
        step: 8000,
    }],
    n_voltages: 256,
    hpm_min_load_ua: 10000,
    pmic_mode_map: &PMIC_MODE_MAP_PMIC5_LDO,
};

/// `pmic5_pldo_lv`.
pub const PMIC5_PLDO_LV: HwData = HwData {
    name: "pmic5_pldo_lv",
    ranges: &[LinearRange {
        min: 1504000,
        min_sel: 0,
        max_sel: 62,
        step: 8000,
    }],
    n_voltages: 63,
    hpm_min_load_ua: 10000,
    pmic_mode_map: &PMIC_MODE_MAP_PMIC5_LDO,
};

impl HwData {
    /// `regulator_desc_list_voltage_linear_range`: microvolts of a selector.
    pub fn list_voltage(&self, sel: u32) -> Option<u32> {
        self.ranges.iter().find_map(|r| r.value(sel))
    }

    /// `regulator_map_voltage_linear_range`: the selector `_regulator_do_set_voltage` picks for a request.
    pub fn map_voltage(&self, min_uv: u32, max_uv: u32) -> Option<u32> {
        for r in self.ranges {
            let Some(sel) = r.selector_high(min_uv) else {
                continue;
            };
            // Map back into a voltage to verify we're still in bounds.
            if let Some(v) = self.list_voltage(sel)
                && v >= min_uv
                && v <= max_uv
            {
                return Some(sel);
            }
        }
        None
    }
}

/// A Wi-Fi supply: where it comes from and the constraints the board's device tree gives it.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct LdoSpec {
    /// The wifi node's supply (`ath10k_regulators[]`).
    pub supply: &'static str,
    /// `qcom,pmic-id` of the regulators node: "a" is pm6150, "c" is pm6150l on this board.
    pub pmic_id: &'static str,
    /// The regulator node's number (`ldoN`).
    pub index: u32,
    pub hw: HwData,
    /// `regulator-min-microvolt`, `regulator-max-microvolt`.
    pub min_uv: u32,
    pub max_uv: u32,
    /// `regulator-initial-mode`.
    pub initial_mode: u32,
}

/// The five supplies of the WCN3990 on the Samsung Galaxy Book Go, in the order of `ath10k_regulators[]`.
///
/// Phandles resolved in the decompiled device tree: `vdd-0.8-cx-mx` is phandle 0xd4, `ldo9` in `regulators-0`
/// (`qcom,pm6150-rpmh-regulators`, id "a"); the other four are in `regulators-1` (`qcom,pm6150l-rpmh-regulators`,
/// id "c"): 0x52 `ldo1`, 0x53 `ldo2`, 0x54 `ldo10`, 0xd5 `ldo11`. **`ldo10` and `ldo11` belong to pm6150l**, not pm6150.
///
/// | supply | resource | hardware data | min .. max (mV) | selector | vote (mV) |
/// | --- | --- | --- | --- | --- | --- |
/// | vdd-0.8-cx-mx | `ldoa9` | `pmic5_nldo` | 488 .. 800 | 21 | 488 |
/// | vdd-1.8-xo | `ldoc1` | `pmic5_pldo_lv` | 1616 .. 1984 | 14 | 1616 |
/// | vdd-1.3-rfa | `ldoc2` | `pmic5_nldo` | 1168 .. 1304 | 106 | 1168 |
/// | vdd-3.3-ch0 | `ldoc10` | `pmic5_pldo` | 3000 .. 3400 | 187 | 3000 |
/// | vdd-3.3-ch1 | `ldoc11` | `pmic5_pldo` | 3000 .. 3400 | 187 | 3000 |
///
/// All five have `regulator-initial-mode = <3>` (HPM) and no `regulator-always-on`, `regulator-boot-on` or
/// `qcom,always-wait-for-ack`.
pub const WCN3990_LDOS: [LdoSpec; 5] = [
    LdoSpec {
        supply: "vdd-0.8-cx-mx",
        pmic_id: "a",
        index: 9,
        hw: PMIC5_NLDO,
        min_uv: 488000,
        max_uv: 800000,
        initial_mode: RPMH_REGULATOR_MODE_HPM,
    },
    LdoSpec {
        supply: "vdd-1.8-xo",
        pmic_id: "c",
        index: 1,
        hw: PMIC5_PLDO_LV,
        min_uv: 1616000,
        max_uv: 1984000,
        initial_mode: RPMH_REGULATOR_MODE_HPM,
    },
    LdoSpec {
        supply: "vdd-1.3-rfa",
        pmic_id: "c",
        index: 2,
        hw: PMIC5_NLDO,
        min_uv: 1168000,
        max_uv: 1304000,
        initial_mode: RPMH_REGULATOR_MODE_HPM,
    },
    LdoSpec {
        supply: "vdd-3.3-ch0",
        pmic_id: "c",
        index: 10,
        hw: PMIC5_PLDO,
        min_uv: 3000000,
        max_uv: 3400000,
        initial_mode: RPMH_REGULATOR_MODE_HPM,
    },
    LdoSpec {
        supply: "vdd-3.3-ch1",
        pmic_id: "c",
        index: 11,
        hw: PMIC5_PLDO,
        min_uv: 3000000,
        max_uv: 3400000,
        initial_mode: RPMH_REGULATOR_MODE_HPM,
    },
];

/// The command DB name of an LDO (`rpmh_regulator_init_vreg`): `ldo<pmic id><index>` normally, `L<index><pmic id>`
/// for a PMIC id that contains `_E` in its first four characters.
pub fn resource_name(pmic_id: &str, index: u32) -> String {
    let head = pmic_id.as_bytes();
    let head = &head[..head.len().min(4)];
    let has_e = head.windows(2).any(|w| w == b"_E");
    if pmic_id.len() > 1 && has_e {
        format!("L{index}{pmic_id}")
    } else {
        format!("ldo{pmic_id}{index}")
    }
}

/// Which register of the resource a request sets.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Kind {
    Voltage,
    Enable,
    Mode,
}

/// One request the driver makes (`rpmh_regulator_send_request`); `cmd.wait` is `wait_for_ack`.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct Send {
    pub kind: Kind,
    pub cmd: TcsCmd,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum VrmError {
    /// No command DB entry, or address 0, for this resource (`-ENODEV`).
    Missing(String),
    /// No selector satisfies the constraints (`-EINVAL`).
    Unsupportable { min_uv: u32, max_uv: u32 },
    /// A mode the regulator core would not pass, or the hardware has no code for.
    BadMode(u32),
}

impl core::fmt::Display for VrmError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            VrmError::Missing(name) => write!(f, "command DB has no address for {name}"),
            VrmError::Unsupportable { min_uv, max_uv } => {
                write!(f, "no voltage selector within {min_uv}..{max_uv} uV")
            }
            VrmError::BadMode(m) => write!(f, "regulator mode {m:#x} is not supported"),
        }
    }
}

/// `struct rpmh_vreg`: the state that decides what a call sends.
#[derive(Copy, Clone, Debug)]
pub struct Vreg {
    addr: u32,
    hw: HwData,
    always_wait_for_ack: bool,
    /// `enabled`: `None` is `-EINVAL`, "never enabled or disabled".
    enabled: Option<bool>,
    /// `voltage_selector`: `None` is `-ENOTRECOVERABLE`.
    voltage_selector: Option<u32>,
    mode: u32,
}

impl Vreg {
    /// `vreg->addr` from the command DB, as `rpmh_regulator_init_vreg` sets up a fresh regulator.
    pub fn new(addr: u32, hw: HwData, always_wait_for_ack: bool) -> Self {
        Vreg {
            addr,
            hw,
            always_wait_for_ack,
            enabled: None,
            voltage_selector: None,
            mode: REGULATOR_MODE_INVALID,
        }
    }

    /// `rpmh_regulator_send_request`.
    fn request(&self, kind: Kind, reg: u32, data: u32, wait_for_ack: bool, out: &mut Vec<Send>) {
        out.push(Send {
            kind,
            cmd: TcsCmd {
                addr: self.addr + reg,
                data,
                wait: wait_for_ack || self.always_wait_for_ack,
            },
        });
    }

    /// `_rpmh_regulator_vrm_set_voltage_sel`: the voltage register takes millivolts, rounded up.
    fn send_voltage(
        &mut self,
        sel: u32,
        wait_for_ack: bool,
        out: &mut Vec<Send>,
    ) -> Result<(), VrmError> {
        let uv = self.hw.list_voltage(sel).ok_or(VrmError::Unsupportable {
            min_uv: 0,
            max_uv: 0,
        })?;
        self.request(
            Kind::Voltage,
            REG_VRM_VOLTAGE,
            uv.div_ceil(1000),
            wait_for_ack,
            out,
        );
        self.voltage_selector = Some(sel);
        Ok(())
    }

    /// `rpmh_regulator_vrm_set_voltage_sel`.
    pub fn set_voltage_sel(&mut self, sel: u32, out: &mut Vec<Send>) -> Result<(), VrmError> {
        if self.enabled.is_none() {
            // Cache the voltage and send it later when the regulator is enabled or disabled.
            self.voltage_selector = Some(sel);
            return Ok(());
        }
        let wait = self.voltage_selector.is_none_or(|cur| sel > cur);
        self.send_voltage(sel, wait, out)
    }

    /// `rpmh_regulator_set_enable_state`.
    fn set_enable_state(&mut self, enable: bool, out: &mut Vec<Send>) -> Result<(), VrmError> {
        if self.enabled.is_none()
            && let Some(sel) = self.voltage_selector
        {
            self.send_voltage(sel, true, out)?;
        }
        self.request(Kind::Enable, REG_ENABLE, u32::from(enable), enable, out);
        self.enabled = Some(enable);
        Ok(())
    }

    pub fn enable(&mut self, out: &mut Vec<Send>) -> Result<(), VrmError> {
        self.set_enable_state(true, out)
    }

    pub fn disable(&mut self, out: &mut Vec<Send>) -> Result<(), VrmError> {
        self.set_enable_state(false, out)
    }

    /// `rpmh_regulator_vrm_set_mode` (not bypassed). `mode` is a framework mode.
    pub fn set_mode(&mut self, mode: u32, out: &mut Vec<Send>) -> Result<(), VrmError> {
        if mode == self.mode {
            return Ok(());
        }
        // `regulator_mode_constrain` lets only the four real modes through to the driver.
        if !matches!(
            mode,
            REGULATOR_MODE_FAST
                | REGULATOR_MODE_NORMAL
                | REGULATOR_MODE_IDLE
                | REGULATOR_MODE_STANDBY
        ) {
            return Err(VrmError::BadMode(mode));
        }
        let pmic_mode = self.hw.pmic_mode_map[mode as usize];
        if pmic_mode < 0 {
            return Err(VrmError::BadMode(mode));
        }
        self.request(Kind::Mode, REG_VRM_MODE, pmic_mode as u32, true, out);
        self.mode = mode;
        Ok(())
    }

    /// What `devm_regulator_register` does with the node's constraints: apply the voltage range (cached, nothing
    /// sent, while the regulator is not enabled), then `regulator-initial-mode` (sent).
    pub fn register(
        &mut self,
        min_uv: u32,
        max_uv: u32,
        initial_mode: u32,
        out: &mut Vec<Send>,
    ) -> Result<(), VrmError> {
        if min_uv != 0 && max_uv != 0 {
            let sel = self
                .hw
                .map_voltage(min_uv, max_uv)
                .ok_or(VrmError::Unsupportable { min_uv, max_uv })?;
            self.set_voltage_sel(sel, out)?;
        }
        let mode = of_map_mode_ldo(initial_mode);
        if mode == REGULATOR_MODE_INVALID {
            return Err(VrmError::BadMode(initial_mode));
        }
        self.set_mode(mode, out)
    }
}

/// Everything Linux sends to bring one Wi-Fi supply up: registration (mode), then the consumer's enable (voltage,
/// enable). The address comes from the command DB entry `ldo<pmic><index>`.
pub fn ldo_votes<R: RegIo>(db: &CmdDb<R>, spec: &LdoSpec) -> Result<Vec<Send>, VrmError> {
    let name = resource_name(spec.pmic_id, spec.index);
    let addr = match db.find(&name) {
        Some(e) if e.addr != 0 => e.addr,
        _ => return Err(VrmError::Missing(name)),
    };
    let mut vreg = Vreg::new(addr, spec.hw, false);
    let mut out = Vec::new();
    vreg.register(spec.min_uv, spec.max_uv, spec.initial_mode, &mut out)?;
    vreg.enable(&mut out)?;
    Ok(out)
}
