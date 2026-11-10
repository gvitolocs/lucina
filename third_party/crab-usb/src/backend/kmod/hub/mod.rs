pub mod device;
pub(crate) mod port;

use alloc::{boxed::Box, collections::btree_map::BTreeMap, vec::Vec};
use core::{any::Any, fmt::Debug};

// 重新导出常用类型
pub use device::{HubDevice, PortState};
use futures::future::BoxFuture;
use usb_if::{err::USBError, host::hub::Speed};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct HubId(usize);

impl HubId {
    pub(crate) const fn new(raw: usize) -> Self {
        Self(raw)
    }
}

pub trait HubOp: Send + 'static + Any {
    fn init<'a>(&'a mut self, info: HubInfo) -> BoxFuture<'a, Result<HubInfo, USBError>>;
    fn changed_ports<'a>(&'a mut self) -> BoxFuture<'a, Result<Vec<PortEvent>, USBError>>;
    fn disconnect(&mut self) -> BoxFuture<'_, Result<(), USBError>> {
        Box::pin(async { Ok(()) })
    }
    fn slot_id(&self) -> u8;

    /// The port reset that starts every enumeration attempt of Linux's `hub_port_connect` (`hub_port_init`); with
    /// `power_cycle` the port's power is switched off and on first, as Linux does halfway through the attempts.
    /// Returns the port after the reset; `USBError::NotFound` if it has no device any more. A hub that resets its
    /// ports itself before it reports them (the default) returns `NotSupported`, and gets no retries.
    fn reset_port_for_enumeration(
        &mut self,
        _port_id: u8,
        _power_cycle: bool,
    ) -> BoxFuture<'_, Result<PortChangeInfo, USBError>> {
        Box::pin(async { Err(USBError::NotSupported) })
    }

    /// Every attempt failed: Linux disables the port, and it is left alone until its connection changes.
    fn give_up_port(&mut self, _port_id: u8) -> BoxFuture<'_, ()> {
        Box::pin(async {})
    }
}

#[derive(Debug, Clone)]
pub enum PortEvent {
    Connected(PortChangeInfo),
    Disconnected { port_id: u8 },
}

#[derive(Debug, Clone)]
pub struct PortChangeInfo {
    pub root_port_id: u8,
    pub port_id: u8,
    pub port_speed: Speed,
}

pub struct Hub {
    pub info: HubInfo,
    pub backend: Box<dyn HubOp>,
}
impl Hub {
    pub fn new(
        backend: Box<dyn HubOp>,
        infos: &BTreeMap<HubId, HubInfo>,
        port_id: u8,
        parent: Option<HubId>,
    ) -> Self {
        let slot_id;
        let mut hub_depth = 0;
        if parent.is_none() {
            hub_depth = -1;
            slot_id = 0;
        } else {
            slot_id = backend.slot_id();
            let mut current_parent = parent;
            while let Some(p) = current_parent {
                let parent = infos.get(&p).expect("parent hub info must exist");
                if parent.hub_depth == -1 {
                    break;
                }

                hub_depth += 1;
                current_parent = infos.get(&p).and_then(|info| info.parent);
            }
        }

        Self {
            backend,
            info: HubInfo {
                parent,
                port_id,
                slot_id,
                hub_depth,
                speed: Speed::Full,
                tt: UsbTt {
                    multi: false,
                    think_time_ns: 0,
                },
            },
        }
    }
}

#[derive(Debug, Clone)]
pub struct HubInfo {
    /// 若为 None, 则表示 Root Hub
    pub parent: Option<HubId>,
    pub slot_id: u8,
    pub hub_depth: isize,
    pub speed: Speed,
    pub port_id: u8,
    pub tt: UsbTt,
}

#[derive(Debug, Clone, Copy)]
pub struct UsbTt {
    pub multi: bool,
    pub think_time_ns: usize,
}
