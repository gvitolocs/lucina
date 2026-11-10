//! xHCI Root Hub 实现
//!
//! 实现 xHCI 控制器的 Root Hub 功能，遵循 xHCI 规范第 4.19 章。
//!
//! LucinaOS: the port handling follows Linux's hub driver on its xHCI root hubs (drivers/usb/core/hub.c,
//! drivers/usb/host/xhci-hub.c): power on, the power-good wait, a debounce on every connect, and the port reset
//! of each enumeration attempt (`reset_port`, called by the core's retry loop). Every PORTSC write goes through
//! `port_state_to_neutral`.

use alloc::{sync::Arc, vec, vec::Vec};
use core::{
    cell::UnsafeCell,
    sync::atomic::{AtomicBool, Ordering},
    time::Duration,
};

use futures::{FutureExt, future::BoxFuture, task::AtomicWaker};
use usb_if::{err::USBError, host::hub::Speed};

use super::reg::{
    MemMapper, PortStatusRegisters, XhciRegisters, apply_supported_protocol,
    scan_extended_capabilities,
};
use crate::{
    backend::kmod::hub::{
        HubInfo, HubOp, PortChangeInfo, PortEvent, PortState,
        port::{
            Debounce, DebounceStep, HUB_BH_RESET_TIME, HUB_DEBOUNCE_STEP, HUB_LONG_RESET_TIME,
            HUB_ROOT_RESET_TIME, PORT_RESET_TRIES, RESET_RECOVERY_TIME, ResetSample, ResetWait,
            ResetWaitResult, classify_reset, power_on_good_delay, reset_try_ends,
            xhci_root_pwr_on_2_pwr_good,
        },
    },
    osal::Kernel,
};

// PORTSC bits (xHCI 5.4.8).
const PORT_CONNECT: u32 = 1 << 0;
const PORT_PE: u32 = 1 << 1;
const PORT_RESET: u32 = 1 << 4;
const PORT_PLS_SHIFT: u32 = 5;
const PORT_PLS_MASK: u32 = 0xf << PORT_PLS_SHIFT;
const PORT_POWER: u32 = 1 << 9;
const PORT_LINK_STROBE: u32 = 1 << 16;
const PORT_SPEED_SHIFT: u32 = 10;
const PORT_SPEED_MASK: u32 = 0xf << PORT_SPEED_SHIFT;
const PORT_CSC: u32 = 1 << 17;
const PORT_WRC: u32 = 1 << 19;
const PORT_RC: u32 = 1 << 21;
const PORT_PLC: u32 = 1 << 22;
const PORT_CAS: u32 = 1 << 24;
const PORT_WR: u32 = 1 << 31;
// Port link states (xhci-port.h).
const XDEV_U3: u32 = 0x3;
const XDEV_INACTIVE: u32 = 0x6;
const XDEV_COMP_MODE: u32 = 0xa;

/// Whether a USB 3 root port needs a warm reset: SS.Inactive or Compliance Mode (`hub_port_warm_reset_required`),
/// or Cold Attach Status, which Linux's xHCI hub reports as Compliance Mode (`xhci_get_usb3_port_status`).
fn warm_reset_required(portsc: u32) -> bool {
    let pls = (portsc & PORT_PLS_MASK) >> PORT_PLS_SHIFT;
    pls == XDEV_INACTIVE || pls == XDEV_COMP_MODE || portsc & PORT_CAS != 0
}

fn reset_sample(portsc: u32, usb3: bool) -> ResetSample {
    ResetSample {
        resetting: portsc & PORT_RESET != 0,
        connected: portsc & PORT_CONNECT != 0,
        enabled: portsc & PORT_PE != 0,
        connect_change: portsc & PORT_CSC != 0,
        warm_reset_required: usb3 && warm_reset_required(portsc),
    }
}

fn port_speed(portsc: u32) -> Speed {
    Speed::from_xhci_portsc(((portsc & PORT_SPEED_MASK) >> PORT_SPEED_SHIFT) as u8)
}

pub struct PortChangeWaker {
    ports: Arc<UnsafeCell<Vec<Port>>>,
}

unsafe impl Send for PortChangeWaker {}
unsafe impl Sync for PortChangeWaker {}

impl PortChangeWaker {
    #[allow(clippy::arc_with_non_send_sync)]
    pub fn new(port_num: u8) -> Self {
        let mut ports = Vec::with_capacity(port_num as usize);
        for i in 0..port_num {
            ports.push(Port {
                port_id: i + 1,
                change_waker: AtomicWaker::new(),
                changed: AtomicBool::new(false),
                state: PortState::Uninit,
            });
        }
        Self {
            ports: Arc::new(UnsafeCell::new(ports)),
        }
    }

    pub fn set_port_changed(&self, port_id: u8) {
        let ports = unsafe { &*self.ports.get() };
        let idx = (port_id - 1) as usize;
        debug!("Setting port {} changed", port_id);
        ports[idx].changed.store(true, Ordering::Release);
        ports[idx].change_waker.wake();
    }
}

pub struct Port {
    port_id: u8,
    change_waker: AtomicWaker,
    changed: AtomicBool,
    state: PortState,
}

/// xHCI Root Hub
///
/// Root Hub 是集成在 xHCI 控制器中的虚拟 Hub。
pub struct XhciRootHub {
    portsc: PortStatusRegisters<MemMapper>,

    ports: Arc<UnsafeCell<Vec<Port>>>,

    kernel: Kernel,

    /// Per port: a USB 3 port (Supported Protocol major revision 3), which Linux puts on its USB 3 root hub.
    usb3: Vec<bool>,

    /// Per port: a connect to handle (Linux's `hub_activate` marks every connected port at start; later a
    /// connect change does).
    pending_connect: Vec<bool>,
}

unsafe impl Send for XhciRootHub {}

impl XhciRootHub {
    fn ports(&self) -> &[Port] {
        unsafe { &*self.ports.get() }
    }

    fn ports_mut(&mut self) -> &mut [Port] {
        unsafe { &mut *self.ports.get() }
    }
}

impl HubOp for XhciRootHub {
    fn changed_ports(&mut self) -> BoxFuture<'_, Result<Vec<PortEvent>, USBError>> {
        self._changed_ports().boxed()
    }

    fn init(&mut self, info: HubInfo) -> BoxFuture<'_, Result<HubInfo, USBError>> {
        async {
            let mut info = info;
            info.speed = Speed::SuperSpeedPlus;
            debug!("Powering the ports of the xHCI root hub");

            // `hub_power_on`: set PP on every port (it is RWS; one already set stays as it is), then wait the
            // power-on-to-power-good time. Linux's USB 2 and USB 3 root hubs do this side by side: 20 ms and 100 ms.
            for idx in 0..self.portsc.len() {
                if self.portsc.read_raw_at(idx) & PORT_POWER == 0 {
                    trace!("Powering on port {}", idx + 1);
                    self.portsc.write_neutral_or(idx, PORT_POWER);
                }
            }
            self.kernel
                .delay(Duration::from_millis(self.power_on_good_delay_all()));

            // Nothing is reset here. A connected port is debounced and then reset by the enumeration attempt
            // that follows (`hub_port_init`); an empty port is left alone.
            for idx in 0..self.portsc.len() {
                self.pending_connect[idx] = self.portsc.read_raw_at(idx) & PORT_CONNECT != 0;
            }

            Ok(info)
        }
        .boxed()
    }

    fn slot_id(&self) -> u8 {
        0
    }

    fn reset_port_for_enumeration(
        &mut self,
        port_id: u8,
        power_cycle: bool,
    ) -> BoxFuture<'_, Result<PortChangeInfo, USBError>> {
        async move {
            let idx = self.port_index(port_id)?;
            if power_cycle {
                self.power_cycle(idx);
            }
            if self.portsc.read_raw_at(idx) & PORT_CONNECT == 0 {
                return Err(USBError::NotFound);
            }
            // `hub_port_init`: a root port's first wait is HUB_ROOT_RESET_TIME, a low speed device's
            // HUB_LONG_RESET_TIME.
            let delay = if port_speed(self.portsc.read_raw_at(idx)) == Speed::Low {
                HUB_LONG_RESET_TIME
            } else {
                HUB_ROOT_RESET_TIME
            };
            match self.reset_root_port(idx, false, delay, true) {
                ResetWaitResult::Done => Ok(PortChangeInfo {
                    root_port_id: port_id,
                    port_id,
                    port_speed: port_speed(self.portsc.read_raw_at(idx)),
                }),
                ResetWaitResult::NotConnected => Err(USBError::NotFound),
                other => {
                    warn!("Root port {port_id}: cannot enable ({other:?})");
                    Err(USBError::Timeout)
                }
            }
        }
        .boxed()
    }

    fn give_up_port(&mut self, port_id: u8) -> BoxFuture<'_, ()> {
        async move {
            let Ok(idx) = self.port_index(port_id) else {
                return;
            };
            // `hub_port_connect` ends with `hub_port_disable`: on a USB 2 port PED (RW1C) is written to disable
            // it; a USB 3 port's link is sent to U3 instead (PLS with the Link Write Strobe), on an enabled port.
            let portsc = self.portsc.read_raw_at(idx);
            if portsc & PORT_PE != 0 {
                if self.usb3[idx] {
                    self.portsc.write_neutral_replace(
                        idx,
                        PORT_PLS_MASK,
                        PORT_LINK_STROBE | (XDEV_U3 << PORT_PLS_SHIFT),
                    );
                } else {
                    self.portsc.write_neutral_or(idx, PORT_PE);
                }
            }
            self.ports_mut()[idx].state = PortState::Uninit;
            self.pending_connect[idx] = false;
        }
        .boxed()
    }
}

impl XhciRootHub {
    /// 创建新的 xHCI Root Hub
    pub fn new(reg: XhciRegisters, kernel: &Kernel) -> Result<Self, USBError> {
        let portsc = reg.port_status_registers();
        let port_num = portsc.len();
        let ports = PortChangeWaker::new(port_num as _).ports.clone();

        let caps = scan_extended_capabilities(
            reg.mmio_base,
            reg.capability.hccparams1.read_volatile(),
            MemMapper {},
        );
        let mut majors = vec![0u8; port_num];
        for (dword0, dword2) in caps.supported_protocols {
            apply_supported_protocol(dword0, dword2, &mut majors);
        }
        let usb3 = majors.iter().map(|&major| major == 3).collect::<Vec<_>>();
        debug!("xHCI root hub: USB 3 ports {usb3:?}");

        Ok(Self {
            portsc,
            ports,
            kernel: kernel.clone(),
            usb3,
            pending_connect: vec![false; port_num],
        })
    }

    pub fn waker(&self) -> PortChangeWaker {
        PortChangeWaker {
            ports: self.ports.clone(),
        }
    }

    fn port_index(&self, port_id: u8) -> Result<usize, USBError> {
        let idx = usize::from(port_id)
            .checked_sub(1)
            .ok_or(USBError::InvalidParameter)?;
        if idx < self.portsc.len() {
            Ok(idx)
        } else {
            Err(USBError::InvalidParameter)
        }
    }

    fn power_on_good_delay(&self, idx: usize) -> u64 {
        power_on_good_delay(xhci_root_pwr_on_2_pwr_good(self.usb3[idx]), true)
    }

    fn power_on_good_delay_all(&self) -> u64 {
        (0..self.portsc.len())
            .map(|idx| self.power_on_good_delay(idx))
            .max()
            .unwrap_or(0)
    }

    /// The power cycle halfway through `hub_port_connect`'s tries: power off, wait twice the power-good time, power
    /// on, wait the power-good time.
    fn power_cycle(&mut self, idx: usize) {
        let delay = self.power_on_good_delay(idx);
        info!("Root port {}: power cycle", idx + 1);
        self.portsc.write_neutral_and_not(idx, PORT_POWER);
        self.kernel.delay(Duration::from_millis(2 * delay));
        self.portsc.write_neutral_or(idx, PORT_POWER);
        self.kernel.delay(Duration::from_millis(delay));
    }

    /// `hub_port_debounce_be_stable` on a root port: the connect change bit is CSC.
    fn debounce(&mut self, idx: usize) -> bool {
        let mut debounce = Debounce::new();
        loop {
            let portsc = self.portsc.read_raw_at(idx);
            let step = debounce.sample(portsc & PORT_CONNECT != 0, portsc & PORT_CSC != 0);
            let clear_change = match step {
                DebounceStep::Stable => return true,
                DebounceStep::TimedOut { clear_change } | DebounceStep::Again { clear_change } => {
                    clear_change
                }
            };
            if clear_change {
                self.portsc.write_neutral_or(idx, PORT_CSC);
            }
            if matches!(step, DebounceStep::TimedOut { .. }) {
                return false;
            }
            self.kernel
                .delay(Duration::from_millis(u64::from(HUB_DEBOUNCE_STEP)));
        }
    }

    /// `hub_port_wait_reset` on a root port.
    fn wait_reset(&mut self, idx: usize, delay: u64) -> ResetWaitResult {
        let usb3 = self.usb3[idx];
        let mut wait = ResetWait::new(delay);
        let mut sleep = wait.first_sleep();
        loop {
            self.kernel.delay(Duration::from_millis(sleep));
            let sample = reset_sample(self.portsc.read_raw_at(idx), usb3);
            if let Some(next) = wait.after_sample(&sample) {
                sleep = next;
                continue;
            }
            let result = classify_reset(&sample, usb3);
            if result == ResetWaitResult::Again {
                self.portsc.write_neutral_or(idx, PORT_CSC);
            }
            return result;
        }
    }

    /// `hub_port_reset` on a root port: SetPortFeature(PORT_RESET) is PR, BH_PORT_RESET is WPR, the change bits
    /// are cleared through the neutral value. `for_device` is Linux's `udev != NULL` (an enumeration reset, which
    /// also clears the connect change of a USB 3 port).
    fn reset_root_port(
        &mut self,
        idx: usize,
        warm: bool,
        delay: u64,
        for_device: bool,
    ) -> ResetWaitResult {
        let usb3 = self.usb3[idx];
        let mut warm = warm && usb3;
        if usb3 && !warm && warm_reset_required(self.portsc.read_raw_at(idx)) {
            warm = true;
        }
        let mut delay = delay;
        let mut result = ResetWaitResult::Busy;
        let mut finished = false;
        for try_index in 0..PORT_RESET_TRIES {
            self.portsc
                .write_neutral_or(idx, if warm { PORT_WR } else { PORT_RESET });
            result = self.wait_reset(idx, delay);
            if reset_try_ends(try_index, result) {
                self.portsc.write_neutral_or(idx, PORT_RC);
                if !usb3 {
                    finished = true;
                    break;
                }
                self.portsc.write_neutral_or(idx, PORT_WRC | PORT_PLC);
                if for_device {
                    self.portsc.write_neutral_or(idx, PORT_CSC);
                }
                if !warm_reset_required(self.portsc.read_raw_at(idx)) {
                    finished = true;
                    break;
                }
                // A USB 3 link that fell into SS.Inactive or Compliance after the reset: warm reset.
                warm = true;
            }
            debug!(
                "Root port {}: not enabled ({result:?}), trying {}reset again",
                idx + 1,
                if warm { "warm " } else { "" }
            );
            delay = HUB_LONG_RESET_TIME;
        }
        if !finished {
            warn!(
                "Root port {}: cannot enable. Maybe the USB cable is bad?",
                idx + 1
            );
        }
        if result == ResetWaitResult::Done {
            self.kernel
                .delay(Duration::from_millis(RESET_RECOVERY_TIME));
        }
        result
    }

    async fn _changed_ports(&mut self) -> Result<Vec<PortEvent>, USBError> {
        let mut events = self.handle_disconnected();
        for idx in 0..self.portsc.len() {
            if let Some(info) = self.handle_port(idx) {
                events.push(PortEvent::Connected(info));
            }
        }
        Ok(events)
    }

    /// Linux's `hub_port_connect_change` for a port with a device: the device goes when the port is empty, and also when
    /// its connection changed (CSC) and the port is no longer enabled, which is a device unplugged and plugged in again
    /// between two looks (a new device is not enabled before its reset). Its CSC stays set, so the next look connects the
    /// new device. A change on a port that is still enabled keeps the device (Linux: "Nothing to do"), and its CSC is
    /// cleared.
    fn handle_disconnected(&mut self) -> Vec<PortEvent> {
        let probed = self
            .ports()
            .iter()
            .filter(|port| matches!(port.state, PortState::Probed))
            .map(|port| port.port_id)
            .collect::<Vec<_>>();
        let mut disconnected = Vec::new();
        for port_id in probed {
            let index = usize::from(port_id - 1);
            match probed_port(self.portsc.read_raw_at(index)) {
                ProbedPort::Gone => disconnected.push(port_id),
                ProbedPort::KeepClearChange => self.portsc.write_neutral_or(index, PORT_CSC),
                ProbedPort::Keep => {}
            }
        }
        for port_id in &disconnected {
            self.ports_mut()[usize::from(*port_id - 1)].state = PortState::Uninit;
        }
        disconnected
            .into_iter()
            .map(|port_id| PortEvent::Disconnected { port_id })
            .collect()
    }

    /// Linux's `port_event` for a root port without a device: a USB 3 link that needs it gets a warm reset, and a
    /// connect is debounced; a stable connect is reported, and the enumeration attempt resets the port.
    fn handle_port(&mut self, idx: usize) -> Option<PortChangeInfo> {
        if !matches!(self.ports()[idx].state, PortState::Uninit) {
            return None;
        }
        let portsc = self.portsc.read_raw_at(idx);
        // Only on a change (link state, warm reset, connect) or a connect to handle, as Linux's `port_event`: a
        // port that stays in Compliance would otherwise cost blocking resets on every poll.
        if self.usb3[idx]
            && warm_reset_required(portsc)
            && (portsc & (PORT_PLC | PORT_WRC | PORT_CSC) != 0 || self.pending_connect[idx])
        {
            debug!("Root port {}: link needs a warm reset", idx + 1);
            let _ = self.reset_root_port(idx, true, HUB_BH_RESET_TIME, false);
            self.pending_connect[idx] = true;
        }
        let portsc = self.portsc.read_raw_at(idx);
        if portsc & PORT_CSC != 0 {
            self.pending_connect[idx] = true;
        }
        if !core::mem::take(&mut self.pending_connect[idx]) {
            return None;
        }
        // Only a connected port is debounced. Linux debounces a disconnect too, up to 2 s in its hub thread;
        // here that would stall the one USB thread, and an empty port has nothing to enumerate.
        if self.portsc.read_raw_at(idx) & PORT_CONNECT == 0 {
            if self.portsc.read_raw_at(idx) & PORT_CSC != 0 {
                self.portsc.write_neutral_or(idx, PORT_CSC);
            }
            return None;
        }
        if !self.debounce(idx) {
            if self.portsc.read_raw_at(idx) & PORT_CONNECT != 0 {
                warn!(
                    "Root port {}: connect never stable, ignored until it changes",
                    idx + 1
                );
            }
            return None;
        }
        let portsc = self.portsc.read_raw_at(idx);
        debug!("Root port {} connected: {portsc:#010x}", idx + 1);
        self.ports_mut()[idx].state = PortState::Probed;
        Some(PortChangeInfo {
            root_port_id: idx as u8 + 1,
            port_id: idx as u8 + 1,
            port_speed: port_speed(portsc),
        })
    }
}

/// What a root port with a device does, from its PORTSC (see `handle_disconnected`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ProbedPort {
    /// The device is gone: the port is empty, or its connection changed and it is no longer enabled.
    Gone,
    /// The connection changed on a port still enabled: the device stays, the change is cleared.
    KeepClearChange,
    /// Nothing changed.
    Keep,
}

pub(crate) fn probed_port(portsc: u32) -> ProbedPort {
    if portsc & PORT_CONNECT == 0 || (portsc & PORT_CSC != 0 && portsc & PORT_PE == 0) {
        ProbedPort::Gone
    } else if portsc & PORT_CSC != 0 {
        ProbedPort::KeepClearChange
    } else {
        ProbedPort::Keep
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn usb3_links_in_inactive_compliance_or_cold_attach_need_a_warm_reset() {
        assert!(warm_reset_required(XDEV_INACTIVE << PORT_PLS_SHIFT));
        assert!(warm_reset_required(XDEV_COMP_MODE << PORT_PLS_SHIFT));
        assert!(warm_reset_required(PORT_CAS | (0x5 << PORT_PLS_SHIFT)));
        // U0 and RxDetect (the empty SS port read on the laptop, 0x002802a0) do not.
        assert!(!warm_reset_required(PORT_CONNECT | PORT_PE));
        assert!(!warm_reset_required(0x0028_02a0));
    }

    #[test]
    fn a_root_port_sample_reads_portsc() {
        let sample = reset_sample(
            PORT_CONNECT | PORT_PE | PORT_CSC | (3 << PORT_SPEED_SHIFT),
            false,
        );
        assert_eq!(
            sample,
            ResetSample {
                resetting: false,
                connected: true,
                enabled: true,
                connect_change: true,
                warm_reset_required: false,
            }
        );
        assert!(reset_sample(PORT_RESET, false).resetting);
        assert!(!reset_sample(XDEV_INACTIVE << PORT_PLS_SHIFT, false).warm_reset_required);
        assert!(reset_sample(XDEV_INACTIVE << PORT_PLS_SHIFT, true).warm_reset_required);
        assert_eq!(port_speed(3 << PORT_SPEED_SHIFT), Speed::High);
    }

    #[test]
    fn a_probed_root_port_goes_only_when_empty_or_changed_and_disabled() {
        assert_eq!(probed_port(0), ProbedPort::Gone);
        assert_eq!(probed_port(PORT_CSC), ProbedPort::Gone);
        assert_eq!(probed_port(PORT_CONNECT | PORT_CSC), ProbedPort::Gone);
        assert_eq!(
            probed_port(PORT_CONNECT | PORT_PE | PORT_CSC),
            ProbedPort::KeepClearChange
        );
        assert_eq!(probed_port(PORT_CONNECT | PORT_PE), ProbedPort::Keep);
        assert_eq!(probed_port(PORT_CONNECT), ProbedPort::Keep);
    }
}
