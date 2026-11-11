use alloc::{collections::BTreeMap, sync::Arc, vec, vec::Vec};
use core::{mem::ManuallyDrop, time::Duration};

use ax_sync::SpinLock as Mutex;
use futures::{FutureExt, future::BoxFuture};
use mbarrier::mb;
use usb_if::{
    descriptor::{
        ConfigurationDescriptor, DescriptorType, DeviceDescriptor, DeviceDescriptorBase,
        EndpointDescriptor, EndpointType,
    },
    endpoint::EndpointInfo,
    err::{TransferError, USBError},
    host::{ControlSetup, hub::Speed},
    transfer::{Recipient, RequestType},
};
use xhci::ring::trb::command;

use super::{
    SlotId, Xhci,
    cmd::CommandRing,
    context::ContextData,
    endpoint::{Endpoint as XhciEndpoint, EndpointDescriptorExt},
    parse_default_max_packet_size_from_port_speed,
    reg::SlotBell,
    transfer::TransferResultHandler,
};
use crate::{
    DeviceAddressInfo,
    backend::{
        Dci,
        kmod::hub::port::{
            GET_DESCRIPTOR_RETRY_DELAY, GET_DESCRIPTOR_TRIES, GET_MAXPACKET0_TRIES,
            SET_ADDRESS_RETRY_DELAY, SET_ADDRESS_SETTLE_TIME, SET_ADDRESS_TRIES,
        },
        ty::{DeviceOp, HubParams, ep::EndpointHandle},
    },
    err::Result,
    osal::Kernel,
};

fn endpoint_address_dci(address: u8) -> u8 {
    let endpoint_number = address & 0x0f;
    endpoint_number * 2 + u8::from(address & 0x80 != 0)
}

pub struct Device {
    id: SlotId,
    /// Dropped by hand (`Drop for Device`): the controller writes the output context while the slot is enabled.
    ctx: ManuallyDrop<ContextData>,
    /// Disable Slot completed: the controller no longer touches this slot's context or rings.
    slot_disabled: bool,
    desc: DeviceDescriptor,
    ctrl_ep: Option<EndpointHandle>,
    transfer_result_handler: TransferResultHandler,
    bell: Arc<Mutex<SlotBell>>,
    kernel: Kernel,
    current_config_value: Option<u8>,
    config_desc: Vec<ConfigurationDescriptor>,
    port_speed: Speed,
    eps: BTreeMap<u8, EndpointHandle>,
    ep_interfaces: BTreeMap<u8, u8>,
    interface_alternates: BTreeMap<u8, u8>,
    /// The controller's DMA quarantine is `cmd.quarantine()`: it outlives this device, which a `Vec` here did
    /// not (`new_device` drops the device right after `release_failed`).
    cmd: CommandRing,
    /// A low or full speed device behind an external high speed hub's TT (Linux: `tt_info & TT_SLOT` in the slot
    /// context). Its endpoints get no soft retry.
    behind_tt: bool,
}

impl Device {
    pub(crate) async fn new(host: &mut Xhci) -> Result<Self> {
        let slot_id = host.device_slot_assignment().await?;
        debug!("Slot {slot_id} assigned");
        let is_64 = host.is_64bit_ctx();
        debug!(
            "Creating new context for slot {slot_id}, {}",
            if is_64 { "64-bit" } else { "32-bit" }
        );
        let dma = host.kernel.clone();
        let ctx = host.dev_mut()?.new_ctx(slot_id, is_64, &dma)?;
        let bell = host.new_slot_bell(slot_id);
        let bell = Arc::new(Mutex::new(bell));
        // let port_speed = host.port_speed(port);
        let desc = unsafe { core::mem::zeroed() };

        Ok(Self {
            id: slot_id,
            ctx: ManuallyDrop::new(ctx),
            slot_disabled: false,
            bell,
            ctrl_ep: None,
            desc,
            kernel: dma,
            transfer_result_handler: host.transfer_result_handler.clone(),
            current_config_value: None,
            config_desc: vec![],
            port_speed: Speed::Full,
            eps: BTreeMap::new(),
            ep_interfaces: BTreeMap::new(),
            interface_alternates: BTreeMap::new(),
            cmd: host.cmd.clone(),
            behind_tt: false,
        })
    }

    fn create_ep(&self, dci: Dci) -> Result<XhciEndpoint> {
        let mut ep = XhciEndpoint::new(
            self.id,
            dci,
            &self.kernel,
            self.bell.clone(),
            self.cmd.clone(),
        )?;
        ep.set_out_context(self.ctx.out_context());
        Ok(ep)
    }

    fn create_registered_ep(&self, dci: Dci) -> Result<XhciEndpoint> {
        let ep = self.create_ep(dci)?;
        self.transfer_result_handler
            .register_queue(self.id.as_u8(), dci.as_u8(), ep.ring())?;
        Ok(ep)
    }

    fn control_endpoint(&self) -> &EndpointHandle {
        self.ctrl_ep.as_ref().unwrap()
    }

    fn control_endpoint_mut(&mut self) -> &mut EndpointHandle {
        self.ctrl_ep.as_mut().unwrap()
    }

    pub(crate) async fn init(&mut self, host: &mut Xhci, info: &DeviceAddressInfo) -> Result {
        // Keep the raw PORTSC.PortSpeed encoding for interval calculations
        self.port_speed = info.port_speed;
        // let speed = info.port_speed.to_xhci_portsc_value();

        let ep = self.create_registered_ep(Dci::CTRL)?;
        self.ctrl_ep = Some(EndpointHandle::new(EndpointInfo::control(), ep));
        // Linux `hub_port_init` with Set Address first (this crate's order, Linux's "old scheme"): up to
        // GET_DESCRIPTOR_TRIES rounds 100 ms apart; each sets the address unless done (SET_ADDRESS_TRIES, 200 ms
        // after a failure; a round without an address fails the attempt), waits 10 ms and reads the first 8 bytes
        // of the device descriptor (`get_bMaxPacketSize0`).
        let mut addressed = false;
        let mut base = Err(USBError::NotInitialized);
        for round in 0..GET_DESCRIPTOR_TRIES {
            if round > 0 {
                self.kernel
                    .delay(Duration::from_millis(GET_DESCRIPTOR_RETRY_DELAY));
            }
            if !addressed {
                self.set_address(host, info).await?;
                addressed = true;
            }
            self.kernel
                .delay(Duration::from_millis(SET_ADDRESS_SETTLE_TIME));
            base = self.read_max_packet0(round == 0).await;
            match &base {
                Ok(_) => break,
                Err(e) => warn!(
                    "xHCI slot {}: device descriptor read/8 failed: {e:?}",
                    self.id.as_u8()
                ),
            }
        }
        let base = base?;
        debug!("Device Descriptor Base: {:#x?}", base);

        self.setup_max_packet(base).await?;

        // 读取当前配置（应该返回 0，表示未配置）
        let current_config = self.get_configuration().await?;
        debug!("Current configuration value: {}", current_config);

        self.read_descriptor().await?;

        // 读取所有配置描述符
        for i in 0..self.desc.num_configurations {
            let config_desc = self
                .control_endpoint_mut()
                .get_configuration_descriptor(i)
                .await?;
            self.config_desc.push(config_desc);
        }

        // 设置配置为第一个配置（大多数设备只有一个配置）
        // 参考 USB 2.0 规范第 9.1.1 节和 u-boot 的 usb_set_configure_device
        if !self.config_desc.is_empty() {
            let config_value = self.config_desc[0].configuration_value;
            debug!("Setting device configuration to {}", config_value);
            self._set_configuration(config_value).await?;
        }

        debug!("device descriptor ok");
        Ok(())
    }

    /// Undoes an `init` that failed: stops EP0, disables the slot and drops the slot's endpoint routes, as
    /// `_disconnect` does for a device that goes away.
    pub(crate) async fn release_failed(&mut self) {
        if let Err(err) = self._disconnect().await {
            warn!("xHCI: releasing slot {} after a failed enumeration: {err}", self.id.as_u8());
        }
    }

    async fn evaluate(&mut self) -> Result {
        mb();
        debug!("Evaluating context for slot {}", self.id.as_u8());
        let _result = self
            .cmd
            .cmd_request(command::Allowed::EvaluateContext(
                *command::EvaluateContext::default()
                    .set_slot_id(self.id.into())
                    .set_input_context_pointer(self.ctx.input_bus_addr()),
            ))
            .await?;
        debug!("Evaluate context ok");
        Ok(())
    }

    /// The Address Device command, retried as `hub_port_init` retries `hub_set_address`.
    async fn set_address(&mut self, host: &mut Xhci, info: &DeviceAddressInfo) -> Result {
        let mut result = Ok(());
        for operation in 0..SET_ADDRESS_TRIES {
            result = self.address(host, info).await;
            match &result {
                Ok(()) => break,
                Err(e) => {
                    warn!(
                        "xHCI slot {}: device not accepting address ({}/{SET_ADDRESS_TRIES}): {e:?}",
                        self.id.as_u8(),
                        operation + 1
                    );
                    self.kernel
                        .delay(Duration::from_millis(SET_ADDRESS_RETRY_DELAY));
                }
            }
        }
        result
    }

    /// Linux `get_bMaxPacketSize0`: up to GET_MAXPACKET0_TRIES reads of the first 8 bytes of the device
    /// descriptor, until one is a device descriptor with a valid bMaxPacketSize0. A timeout on the first round of a
    /// device faster than full speed ends it at once (Linux: such a device needs another reset). A failed read can
    /// leave EP0 halted (Stall, or the errors of `completion_code_halts`); Linux's xHCI resets a halted EP0 before
    /// the next request, here the next read does (`reset` does nothing on an endpoint that is not halted).
    async fn read_max_packet0(&mut self, first_round: bool) -> Result<DeviceDescriptorBase> {
        let mut result = Err(USBError::NotInitialized);
        for _ in 0..GET_MAXPACKET0_TRIES {
            self.control_endpoint().reset().await?;
            result = self.get_device_descriptor_base().await.and_then(|base| {
                if max_packet0_is_valid(base.descriptor_type, base.max_packet_size_0) {
                    Ok(base)
                } else {
                    Err(anyhow!(
                        "invalid device descriptor header: type {:#x}, bMaxPacketSize0 {}",
                        base.descriptor_type,
                        base.max_packet_size_0
                    )
                    .into())
                }
            });
            let timed_out = matches!(
                result,
                Err(USBError::Timeout | USBError::TransferError(TransferError::Timeout))
            );
            if result.is_ok()
                || (timed_out && first_round && !matches!(self.port_speed, Speed::Low | Speed::Full))
            {
                break;
            }
        }
        result
    }

    async fn setup_max_packet(&mut self, desc: DeviceDescriptorBase) -> Result {
        let default = parse_default_max_packet_size_from_port_speed(self.port_speed);
        let packet_size = ep0_max_packet_size(self.port_speed, default, desc.max_packet_size_0)
            .ok_or_else(|| anyhow!("invalid ep0 maxpacket: {}", desc.max_packet_size_0))?;
        if packet_size == default {
            // Linux changes EP0 (`usb_ep0_reinit`) only when the guess was wrong.
            return Ok(());
        }
        debug!("xHCI slot {}: ep0 maxpacket {packet_size}", self.id.as_u8());
        self.ctx.perper_change();

        let dci = Dci::CTRL;
        self.ctx.with_input(|input| {
            input.control_mut().set_add_context_flag(1); // Endpoint 0 Context

            let endpoint = input.device_mut().endpoint_mut(dci.as_usize());
            endpoint.set_max_packet_size(packet_size);
        });

        self.evaluate().await?;

        Ok(())
    }

    async fn address(&mut self, host: &mut Xhci, info: &DeviceAddressInfo) -> Result {
        // 直接使用 DeviceSpeed 枚举计算默认 max packet size
        let max_packet_size = parse_default_max_packet_size_from_port_speed(info.port_speed);

        // Route String 由拓扑决定（root hub 端口不计入）
        let mut route_string = 0u32;
        let mut parent_id = info.parent_hub;
        let mut port_id = info.port_id;

        while let Some(pid) = parent_id {
            let parent_hub = info.infos.get(&pid).unwrap();
            if parent_hub.hub_depth == -1 {
                break;
            }
            if port_id > 15 {
                port_id = 15;
            }
            route_string |= (port_id as u32) << (parent_hub.hub_depth * 4);
            port_id = parent_hub.port_id;
            parent_id = parent_hub.parent;
        }

        let ctrl_ring_addr = self
            .control_endpoint_mut()
            .with_raw_mut::<XhciEndpoint, _>(|ep| ep.bus_addr());
        let mut behind_tt = false;
        // ctrl dci
        let dci = Dci::CTRL;
        // 1. Allocate an Input Context data structure (6.2.5) and initialize all fields to
        // ‘0’.
        self.ctx.with_empty_input(|input| {
            let control_context = input.control_mut();
            // Initialize the Input Control Context (6.2.5.1) of the Input Context by
            // setting the A0 and A1 flags to ‘1’. These flags indicate that the Slot
            // Context and the Endpoint 0 Context of the Input Context are affected by
            // the command.
            control_context.set_add_context_flag(0);
            control_context.set_add_context_flag(1);
            for i in 2..32 {
                control_context.clear_drop_context_flag(i);
            }

            // Initialize the Input Slot Context data structure (6.2.2).
            // • Root Hub Port Number = Topology defined.
            // • Route String = Topology defined. Refer to section 8.9 in the USB3 spec. Note
            // that the Route String does not include the Root Hub Port Number.
            // • Context Entries = 1.
            let slot_context = input.device_mut().slot_mut();
            slot_context.clear_multi_tt();
            slot_context.clear_hub();
            slot_context.set_route_string(route_string);
            slot_context.set_context_entries(1);
            slot_context.set_max_exit_latency(0);
            slot_context.set_root_hub_port_number(info.root_port_id);
            slot_context.set_number_of_ports(0);
            slot_context.set_parent_hub_slot_id(0);

            // TT info is only valid for LS/FS devices behind a HS hub.
            if matches!(info.port_speed, Speed::Low | Speed::Full) {
                let mut parent_id = info.parent_hub;
                let mut tt_port = info.port_id;
                let mut hs_parent = None;

                while let Some(p) = parent_id {
                    let parent_hub = info.infos.get(&p).unwrap();
                    if parent_hub.hub_depth == -1 {
                        break;
                    }
                    if matches!(parent_hub.speed, Speed::High) {
                        hs_parent = Some(p);
                        break;
                    }
                    tt_port = parent_hub.port_id;
                    parent_id = parent_hub.parent;
                }

                if let Some(hs_id) = hs_parent {
                    let parent = info.infos.get(&hs_id).unwrap();
                    let slot_id = parent.slot_id;
                    if parent.tt.multi {
                        slot_context.set_multi_tt();
                    }

                    slot_context.set_parent_hub_slot_id(slot_id);
                    slot_context.set_parent_port_number(tt_port);
                    behind_tt = true;
                    debug!(
                        "Setting parent_port_number (TT): {}, parent_hub_slot_id: {}",
                        tt_port, slot_id
                    );
                }
            }

            slot_context.set_tt_think_time(0);
            slot_context.set_interrupter_target(0);
            // 转换为 xHCI Slot Context 速度值
            slot_context.set_speed(info.port_speed.to_xhci_slot_value());

            // Initialize the Input default control Endpoint 0 Context (6.2.3).
            let endpoint_0 = input.device_mut().endpoint_mut(dci.as_usize());
            // • EP Type = Control.
            endpoint_0.set_endpoint_type(xhci::context::EndpointType::Control);
            // • Max Packet Size = The default maximum packet size for the Default Control EndpointHandle,
            //   as function of the PORTSC Port Speed field.
            endpoint_0.set_max_packet_size(max_packet_size);
            // • Max Burst Size = 0.
            endpoint_0.set_max_burst_size(0);
            // • TR Dequeue Pointer = Start address of first segment of the Default Control
            //   EndpointHandle Transfer Ring.
            endpoint_0.set_tr_dequeue_pointer(ctrl_ring_addr.raw());
            // • Dequeue Cycle State (DCS) = 1. Reflects Cycle bit state for valid TRBs written
            //   by software.
            // if ring_cycle_bit {
            endpoint_0.set_dequeue_cycle_state();
            // } else {
            //     endpoint_0.clear_dequeue_cycle_state();
            // }
            // • Interval = 0.
            endpoint_0.set_interval(0);
            // • Max Primary Streams (MaxPStreams) = 0.
            endpoint_0.set_max_primary_streams(0);
            // • Mult = 0.
            endpoint_0.set_mult(0);
            // • Error Count (CErr) = 3.
            endpoint_0.set_error_count(3);
            // • Average TRB Length = 8 (xHCI spec 6.2.3).
            endpoint_0.set_average_trb_length(8);
        });
        self.behind_tt = behind_tt;

        debug!(
            r#"Address device {:?}
    root port: {}
    route string: {:#x}
    ctrl ring: {:x?}
    port speed: {:?}
    max packet size: {}"#,
            self.id,
            info.root_port_id,
            route_string,
            ctrl_ring_addr,
            info.port_speed,
            max_packet_size
        );

        mb();

        let input_bus_addr = self.ctx.input_bus_addr();
        trace!("Input context bus address: {input_bus_addr:#x?}");
        let result = host
            .cmd_request(command::Allowed::AddressDevice(
                *command::AddressDevice::new()
                    .set_slot_id(self.id.into())
                    .set_input_context_pointer(input_bus_addr),
            ))
            .await?;

        debug!("Address slot ok {result:x?}");

        Ok(())
    }

    async fn read_descriptor(&mut self) -> Result<()> {
        self.desc = self.control_endpoint_mut().get_device_descriptor().await?;
        Ok(())
    }
    async fn get_device_descriptor_base(&mut self) -> Result<DeviceDescriptorBase> {
        let mut data = vec![0u8; 8];

        // DMA 传输
        let actual = self
            .control_endpoint_mut()
            .get_descriptor(DescriptorType::DEVICE, 0, 0, data.as_mut_slice())
            .await?;
        if actual != data.len() {
            return Err(anyhow!(
                "short device descriptor header: expected {} bytes, got {actual}",
                data.len()
            )
            .into());
        }

        let desc = unsafe { (data.as_ptr() as *const DeviceDescriptorBase).read_unaligned() };

        Ok(desc)
    }

    async fn get_configuration(&mut self) -> Result<u8> {
        let val = self.control_endpoint_mut().get_configuration().await?;
        self.current_config_value = Some(val);
        Ok(val)
    }

    async fn _set_configuration(&mut self, configuration_value: u8) -> Result {
        let old_endpoints = self.eps.clone();
        let old_descriptors = self
            .interface_alternates
            .iter()
            .map(|(interface, alternate)| {
                self.find_interface_endpoints(*interface, *alternate)
                    .map(<[_]>::to_vec)
            })
            .collect::<Result<Vec<_>>>()?
            .into_iter()
            .flatten()
            .collect::<Vec<_>>();
        for endpoint in old_endpoints.values() {
            endpoint.revoke();
        }
        if self.stop_endpoints(old_endpoints.values()).await.is_err() {
            return Err(USBError::InterfaceBroken);
        }

        self.prepare_configure_context(0, 0, &old_descriptors, &[], &BTreeMap::new());
        if self.configure_endpoint().await.is_err() {
            if self
                .resume_stopped_endpoints(old_endpoints.values())
                .await
                .is_err()
            {
                return Err(USBError::InterfaceBroken);
            }
            return Err(USBError::Other(anyhow!(
                "xHCI failed to disable endpoints before SET_CONFIGURATION"
            )));
        }

        if let Err(err) = self
            .control_endpoint_mut()
            .set_configuration(configuration_value)
            .await
        {
            self.prepare_configure_context(0, 0, &[], &old_descriptors, &old_endpoints);
            if self.configure_endpoint().await.is_err()
                || self
                    .resume_stopped_endpoints(old_endpoints.values())
                    .await
                    .is_err()
            {
                return Err(USBError::InterfaceBroken);
            }
            return Err(err.into());
        }

        self.publish_endpoint_routes(&old_endpoints, &BTreeMap::new())?;
        self.eps.clear();
        self.ep_interfaces.clear();
        self.interface_alternates.clear();

        self.ctx.perper_change();
        self.ctx.with_input(|input| {
            let c = input.control_mut();
            c.set_configuration_value(configuration_value);
        });
        if self.evaluate().await.is_err() {
            return Err(USBError::InterfaceBroken);
        }
        self.current_config_value = Some(configuration_value);
        debug!("Device configuration set to {configuration_value}");
        Ok(())
    }

    async fn _claim_interface(
        &mut self,
        interface: u8,
        alternate: u8,
    ) -> Result<BTreeMap<u8, EndpointHandle>> {
        let new_descriptors = self
            .find_interface_endpoints(interface, alternate)?
            .to_vec();
        self.validate_endpoint_addresses(interface, &new_descriptors)?;
        let pending_endpoints = self.prepare_endpoints(&new_descriptors)?;
        let old_alternate = self.interface_alternates.get(&interface).copied();
        let old_descriptors = old_alternate
            .map(|old| {
                self.find_interface_endpoints(interface, old)
                    .map(<[_]>::to_vec)
            })
            .transpose()?
            .unwrap_or_default();
        let stale_endpoints = self
            .ep_interfaces
            .iter()
            .filter_map(|(address, ep_interface)| (*ep_interface == interface).then_some(*address))
            .collect::<Vec<_>>();
        let mut old_endpoints = BTreeMap::new();
        for address in &stale_endpoints {
            if let Some(endpoint) = self.eps.get(address).cloned() {
                endpoint.revoke();
                old_endpoints.insert(*address, endpoint);
            }
        }

        if let Err(err) = self.stop_endpoints(old_endpoints.values()).await {
            for endpoint in old_endpoints.values() {
                endpoint.reactivate();
            }
            return Err(err);
        }

        self.prepare_configure_context(
            interface,
            alternate,
            &old_descriptors,
            &new_descriptors,
            &pending_endpoints,
        );

        if let Err(err) = self.configure_endpoint().await {
            self.resume_stopped_endpoints(old_endpoints.values())
                .await?;
            return Err(err);
        }

        let set_interface_result = self
            .control_endpoint_mut()
            .control_out(
                ControlSetup {
                    request_type: RequestType::Standard,
                    recipient: Recipient::Interface,
                    request: usb_if::transfer::Request::SetInterface,
                    value: alternate.into(),
                    index: interface.into(),
                },
                &[],
            )
            .await;
        if let Err(err) = set_interface_result {
            let rollback = self
                .rollback_interface_configuration(
                    interface,
                    old_alternate,
                    &new_descriptors,
                    &old_descriptors,
                    &old_endpoints,
                )
                .await;
            if let Err(rollback_err) = rollback {
                for endpoint in pending_endpoints.values() {
                    endpoint.revoke();
                    self.cmd.quarantine().keep(endpoint.clone());
                }
                warn!(
                    "SET_INTERFACE {interface}:{alternate} failed ({err}); xHCI rollback failed: \
                     {rollback_err}"
                );
                return Err(USBError::InterfaceBroken);
            }
            return Err(err.into());
        }

        if self
            .publish_endpoint_routes(&old_endpoints, &pending_endpoints)
            .is_err()
        {
            for endpoint in pending_endpoints.values() {
                endpoint.revoke();
                self.cmd.quarantine().keep(endpoint.clone());
            }
            return Err(USBError::InterfaceBroken);
        }
        for address in stale_endpoints {
            self.eps.remove(&address);
            self.ep_interfaces.remove(&address);
        }
        for (address, endpoint) in &pending_endpoints {
            self.eps.insert(*address, endpoint.clone());
            self.ep_interfaces.insert(*address, interface);
        }
        self.interface_alternates.insert(interface, alternate);
        debug!("Interface {interface} alternate {alternate} committed");
        Ok(pending_endpoints)
    }

    async fn _release_interface(&mut self, interface: u8) -> Result {
        let stale_addresses = self
            .ep_interfaces
            .iter()
            .filter_map(|(address, owner)| (*owner == interface).then_some(*address))
            .collect::<Vec<_>>();
        let old_endpoints = stale_addresses
            .iter()
            .filter_map(|address| self.eps.get(address).cloned().map(|ep| (*address, ep)))
            .collect::<BTreeMap<_, _>>();
        for endpoint in old_endpoints.values() {
            endpoint.revoke();
        }
        if let Err(err) = self.stop_endpoints(old_endpoints.values()).await {
            for endpoint in old_endpoints.values() {
                endpoint.reactivate();
            }
            return Err(err);
        }

        let old_alternate = self
            .interface_alternates
            .get(&interface)
            .copied()
            .unwrap_or(0);
        let old_descriptors = self
            .find_interface_endpoints(interface, old_alternate)?
            .to_vec();
        self.prepare_configure_context(
            interface,
            old_alternate,
            &old_descriptors,
            &[],
            &BTreeMap::new(),
        );
        if self.configure_endpoint().await.is_err() {
            if self
                .resume_stopped_endpoints(old_endpoints.values())
                .await
                .is_err()
            {
                return Err(USBError::InterfaceBroken);
            }
            return Err(USBError::Other(anyhow!(
                "xHCI failed to disable interface {interface}"
            )));
        }
        self.publish_endpoint_routes(&old_endpoints, &BTreeMap::new())?;
        for address in stale_addresses {
            self.eps.remove(&address);
            self.ep_interfaces.remove(&address);
        }
        self.interface_alternates.remove(&interface);
        Ok(())
    }

    async fn _disconnect(&mut self) -> Result {
        let mut old_endpoints = self.eps.clone();
        if let Some(control) = &self.ctrl_ep {
            old_endpoints.insert(0, control.clone());
        }
        for endpoint in old_endpoints.values() {
            endpoint.revoke();
        }

        // Flush every endpoint first, matching usb_hcd_flush_endpoint(). A
        // physical disconnect does not prevent the controller from executing
        // Stop Endpoint for the slot; if a stop command fails, Disable Slot is
        // still the final hardware-ownership boundary for all remaining rings.
        for endpoint in old_endpoints.values() {
            let future = endpoint.with_raw_mut::<XhciEndpoint, _>(|raw| raw.stop_future());
            if future.await.is_ok() {
                endpoint.with_raw_mut::<XhciEndpoint, _>(XhciEndpoint::retire_all_after_stop);
            }
        }

        let disable = self
            .cmd
            .cmd_request(command::Allowed::DisableSlot(
                *command::DisableSlot::default().set_slot_id(self.id.into()),
            ))
            .await;
        if let Err(err) = disable {
            // The slot may still be enabled, so the controller may still run these rings and write their TDs'
            // buffers. Linux frees them anyway (`xhci_free_dev`): its IOMMU faults a late DMA. Ours is in bypass,
            // so they go into the controller's quarantine, which outlives this device, for good. `Drop` keeps the
            // slot's context the same way. A dead controller seen to halt does no DMA: they go with the device.
            if !self.cmd.is_halted() {
                for endpoint in old_endpoints.values() {
                    self.cmd.quarantine().keep(endpoint.clone());
                }
            }
            warn!(
                "xHCI Disable Slot failed during disconnect: {err}; slot {} rings {}",
                self.id.as_u8(),
                if self.cmd.is_halted() { "freed, the controller is halted" } else { "quarantined" }
            );
            return Err(USBError::InterfaceBroken);
        }
        self.slot_disabled = true;

        for endpoint in old_endpoints.values() {
            endpoint.with_raw_mut::<XhciEndpoint, _>(XhciEndpoint::retire_all_after_stop);
        }
        if self
            .publish_endpoint_routes(&old_endpoints, &BTreeMap::new())
            .is_err()
        {
            // Disable Slot has stopped hardware ownership, but a mismatched
            // software route means the endpoint registry is no longer a
            // trustworthy release boundary. Keep every ring quarantined and
            // fail closed instead of dropping DMA-backed queue ownership.
            for endpoint in old_endpoints.values() {
                self.cmd.quarantine().keep(endpoint.clone());
            }
            return Err(USBError::InterfaceBroken);
        }
        self.eps.clear();
        self.ep_interfaces.clear();
        self.interface_alternates.clear();
        Ok(())
    }

    fn prepare_endpoints(
        &self,
        descriptors: &[EndpointDescriptor],
    ) -> Result<BTreeMap<u8, EndpointHandle>> {
        let mut endpoints = BTreeMap::new();
        for desc in descriptors {
            let dci = desc.dci();
            let mut ep_raw = self.create_ep(dci.into())?;
            let periodic_burst_size = match self.port_speed {
                Speed::High
                    if matches!(
                        desc.transfer_type,
                        EndpointType::Isochronous | EndpointType::Interrupt
                    ) =>
                {
                    desc.packets_per_microframe.saturating_sub(1)
                }
                _ => 0,
            };
            ep_raw.configure_periodic(
                desc.max_packet_size as usize,
                periodic_burst_size,
                desc.interval,
            );
            // Linux soft-retries a transaction error on bulk and interrupt endpoints only, and not behind a TT
            // (`process_bulk_intr_td`).
            ep_raw.set_soft_retry(
                matches!(
                    desc.transfer_type,
                    EndpointType::Bulk | EndpointType::Interrupt
                ) && !self.behind_tt,
            );
            endpoints.insert(desc.address, EndpointHandle::new(desc.into(), ep_raw));
        }
        Ok(endpoints)
    }

    fn prepare_configure_context(
        &mut self,
        interface: u8,
        alternate: u8,
        drop_descriptors: &[EndpointDescriptor],
        add_descriptors: &[EndpointDescriptor],
        endpoints: &BTreeMap<u8, EndpointHandle>,
    ) {
        self.ctx.perper_change();
        let drop_dcis = drop_descriptors
            .iter()
            .map(EndpointDescriptor::dci)
            .collect::<Vec<_>>();
        self.ctx.with_input(|input| {
            let control = input.control_mut();
            control.set_interface_number(interface);
            control.set_alternate_setting(alternate);
            for dci in &drop_dcis {
                control.set_drop_context_flag((*dci).into());
            }
        });

        for descriptor in add_descriptors {
            let endpoint = endpoints
                .get(&descriptor.address)
                .expect("prepared endpoint must match its descriptor");
            let ring_addr = endpoint.with_raw_mut::<XhciEndpoint, _>(|raw| raw.bus_addr());
            let dci = descriptor.dci();
            let xhci_interval = self.calculate_xhci_interval(
                descriptor.interval,
                descriptor.transfer_type,
                descriptor.interval,
            );
            let periodic_burst_size = match self.port_speed {
                Speed::High
                    if matches!(
                        descriptor.transfer_type,
                        EndpointType::Isochronous | EndpointType::Interrupt
                    ) =>
                {
                    descriptor.packets_per_microframe.saturating_sub(1)
                }
                _ => 0,
            };
            self.ctx.with_input(|input| {
                input.control_mut().set_add_context_flag(dci.into());
                let endpoint_context = input.device_mut().endpoint_mut(dci.into());
                endpoint_context.set_interval(xhci_interval);
                endpoint_context.set_endpoint_type(descriptor.endpoint_type());
                endpoint_context.set_tr_dequeue_pointer(ring_addr.raw());
                endpoint_context.set_max_packet_size(descriptor.max_packet_size);
                endpoint_context.set_error_count(3);
                endpoint_context.set_dequeue_cycle_state();
                if matches!(
                    descriptor.transfer_type,
                    EndpointType::Isochronous | EndpointType::Interrupt
                ) {
                    endpoint_context
                        .set_max_burst_size(periodic_burst_size.min(u8::MAX as usize) as u8);
                    endpoint_context.set_mult(0);
                    let max_esit_payload =
                        descriptor.max_packet_size as usize * (periodic_burst_size + 1);
                    endpoint_context
                        .set_average_trb_length(max_esit_payload.min(u16::MAX as usize) as u16);
                    endpoint_context.set_max_endpoint_service_time_interval_payload_low(
                        max_esit_payload.min(u16::MAX as usize) as u16,
                    );
                }
                if matches!(descriptor.transfer_type, EndpointType::Isochronous) {
                    endpoint_context.set_error_count(0);
                }
            });
        }

        let max_dci = self
            .eps
            .keys()
            .map(|address| endpoint_address_dci(*address))
            .chain(add_descriptors.iter().map(EndpointDescriptor::dci))
            .max()
            .unwrap_or(1);
        self.ctx.with_input(|input| {
            input
                .device_mut()
                .slot_mut()
                .set_context_entries(max_dci + 1)
        });
        mb();
    }

    async fn configure_endpoint(&mut self) -> Result {
        self.cmd
            .cmd_request(command::Allowed::ConfigureEndpoint(
                *command::ConfigureEndpoint::default()
                    .set_slot_id(self.id.into())
                    .set_input_context_pointer(self.ctx.input_bus_addr()),
            ))
            .await?;
        Ok(())
    }

    async fn stop_endpoints<'a>(
        &self,
        endpoints: impl Iterator<Item = &'a EndpointHandle>,
    ) -> Result {
        let endpoints = endpoints.cloned().collect::<Vec<_>>();
        let mut stopped = Vec::new();
        for endpoint in endpoints {
            let future = endpoint.with_raw_mut::<XhciEndpoint, _>(|raw| raw.stop_future());
            if let Err(err) = future.await {
                self.resume_stopped_endpoints(stopped.iter()).await?;
                return Err(err.into());
            }
            endpoint.with_raw_mut::<XhciEndpoint, _>(XhciEndpoint::retire_all_after_stop);
            stopped.push(endpoint);
        }
        Ok(())
    }

    async fn resume_stopped_endpoints<'a>(
        &self,
        endpoints: impl Iterator<Item = &'a EndpointHandle>,
    ) -> Result {
        for endpoint in endpoints {
            let future = endpoint.with_raw_mut::<XhciEndpoint, _>(|raw| raw.resume_future());
            future.await?;
            endpoint.reactivate();
        }
        Ok(())
    }

    async fn rollback_interface_configuration(
        &mut self,
        interface: u8,
        old_alternate: Option<u8>,
        new_descriptors: &[EndpointDescriptor],
        old_descriptors: &[EndpointDescriptor],
        old_endpoints: &BTreeMap<u8, EndpointHandle>,
    ) -> Result {
        let rollback_alternate = old_alternate.unwrap_or(0);
        self.prepare_configure_context(
            interface,
            rollback_alternate,
            new_descriptors,
            old_descriptors,
            old_endpoints,
        );
        self.configure_endpoint().await?;
        if let Some(old_alternate) = old_alternate {
            self.control_endpoint_mut()
                .control_out(
                    ControlSetup {
                        request_type: RequestType::Standard,
                        recipient: Recipient::Interface,
                        request: usb_if::transfer::Request::SetInterface,
                        value: old_alternate.into(),
                        index: interface.into(),
                    },
                    &[],
                )
                .await?;
        }
        for endpoint in old_endpoints.values() {
            endpoint.reactivate();
        }
        Ok(())
    }

    fn publish_endpoint_routes(
        &self,
        old_endpoints: &BTreeMap<u8, EndpointHandle>,
        new_endpoints: &BTreeMap<u8, EndpointHandle>,
    ) -> Result {
        let old_routes = old_endpoints.values().map(|endpoint| {
            let dci = endpoint_address_dci(endpoint.info().address.raw());
            let queue =
                endpoint.with_raw_mut::<XhciEndpoint, _>(|raw| raw.ring().finished_handle());
            (dci, queue)
        });
        let new_routes = new_endpoints.values().map(|endpoint| {
            let dci = endpoint_address_dci(endpoint.info().address.raw());
            let queue =
                endpoint.with_raw_mut::<XhciEndpoint, _>(|raw| raw.ring().finished_handle());
            (dci, queue)
        });
        self.transfer_result_handler
            .replace_queues(self.id.as_u8(), old_routes, new_routes)?;
        Ok(())
    }

    fn validate_endpoint_addresses(
        &self,
        interface: u8,
        descriptors: &[EndpointDescriptor],
    ) -> Result {
        if let Some((address, owner)) =
            endpoint_conflict(&self.ep_interfaces, interface, descriptors)
        {
            warn!(
                "claim interface {interface}: endpoint {address:#04x} belongs to interface {owner}"
            );
            return Err(USBError::InvalidParameter);
        }

        Ok(())
    }

    fn find_interface_endpoints(
        &self,
        interface: u8,
        alternate: u8,
    ) -> Result<&[EndpointDescriptor]> {
        interface_endpoints(
            &self.config_desc,
            self.current_config_value,
            interface,
            alternate,
        )
        .ok_or(USBError::NotFound)
    }

    /// 根据 XHCI 规范计算端点的 interval 值
    /// 参考 xHCI 规范第 6.2.3.6 节
    fn calculate_xhci_interval(
        &self,
        binterval: u8,
        transfer_type: EndpointType,
        default: u8,
    ) -> u8 {
        match transfer_type {
            EndpointType::Isochronous => {
                match self.port_speed {
                    Speed::High | Speed::SuperSpeed | Speed::SuperSpeedPlus => {
                        // HS/SS bInterval is one-based; xHCI Interval is zero-based.
                        let interval = binterval.clamp(1, 16) - 1;
                        debug!(
                            "ISO endpoint HS/SS: bInterval={} -> XHCI interval={}",
                            binterval, interval
                        );
                        interval
                    }
                    _ => {
                        let interval = binterval.max(1).ilog2() as u8 + 3;
                        debug!(
                            "ISO endpoint FS/LS: bInterval={} -> XHCI interval={}",
                            binterval, interval
                        );
                        interval
                    }
                }
            }
            EndpointType::Interrupt => match self.port_speed {
                Speed::High | Speed::SuperSpeed | Speed::SuperSpeedPlus => {
                    let interval = binterval.clamp(1, 16) - 1;
                    debug!(
                        "INT endpoint HS/SS: bInterval={} -> XHCI interval={}",
                        binterval, interval
                    );
                    interval
                }
                _ => {
                    let interval = binterval.max(1).ilog2() as u8 + 3;
                    debug!(
                        "INT endpoint FS/LS: bInterval={} -> XHCI interval={}",
                        binterval, interval
                    );
                    interval
                }
            },
            _ => {
                // 控制和批量端点不使用 interval
                default
            }
        }
    }

    async fn update_hub_inner(&mut self, params: HubParams) -> Result<()> {
        debug!(
            "Updating hub context for slot {}: ports={}, multi_tt={}, tt_time={}ns",
            self.id.as_u8(),
            params.num_ports,
            params.multi_tt,
            params.tt_think_time_ns,
        );

        self.ctx.perper_change();
        // 2. 设置 Slot Context Hub 参数
        self.ctx.with_input(|input| {
            let slot_ctx = input.device_mut().slot_mut();

            // 设置 Hub 标志
            slot_ctx.set_hub();

            // 设置 Multi-TT 标志（参考 U-Boot）
            // 如果 hub->tt.multi 为真，设置 MTT
            // 对于 Full Speed Hub，必须清除 MTT（xHCI 规范 6.2.2）
            if params.multi_tt {
                slot_ctx.set_multi_tt();
            } else if matches!(self.port_speed, Speed::Full) {
                slot_ctx.clear_multi_tt();
            }

            // 设置端口数量
            slot_ctx.set_number_of_ports(params.num_ports);

            // 设置 TT 思考时间（参考 U-Boot xhci_update_hub_device）
            // xHCI spec: TT_THINK_TIME (Bits[16:17] of DWORD 2)
            // 0 = 8 FS bit times, 1 = 16 FS bit times, 2 = 24 FS bit times, 3 = 32 FS bit times
            // 只对 High Speed Hub 设置 TT 思考时间
            if matches!(self.port_speed, Speed::High) {
                // params.tt_think_time_ns is 666 x (TT bits + 1) (hub.c:1611-1635); the slot
                // field is (ns / 666) - 1, giving 0..3 for 8/16/24/32 FS bit times
                // (xhci.c:5338-5343).
                let think_time = if params.tt_think_time_ns > 0 {
                    ((params.tt_think_time_ns / 666) - 1) as u8
                } else {
                    0
                };
                slot_ctx.set_tt_think_time(think_time);
                debug!(
                    "Set TT think time: {} (tt_think_time_ns={}ns)",
                    think_time, params.tt_think_time_ns
                );
            }
        });

        self.evaluate().await?;
        Ok(())
    }
}

impl Drop for Device {
    fn drop(&mut self) {
        // SAFETY: `self.ctx` is taken exactly once, here in `drop`, and nothing reads it afterwards.
        let ctx = unsafe { ManuallyDrop::take(&mut self.ctx) };
        // A slot not known to be disabled may still be run by the controller: its output context and rings stay
        // DMA targets. Linux frees them (`xhci_free_virt_device`), its IOMMU faulting a late write; ours is in
        // bypass, so they are quarantined for good, unless the controller died and was seen to halt.
        if self.slot_disabled || self.cmd.is_halted() {
            drop(ctx);
            return;
        }
        warn!(
            "xHCI slot {}: dropped while not disabled, its context and rings quarantined",
            self.id.as_u8()
        );
        let quarantine = self.cmd.quarantine();
        quarantine.keep(ctx);
        if let Some(control) = &self.ctrl_ep {
            quarantine.keep(control.clone());
        }
        for endpoint in self.eps.values() {
            quarantine.keep(endpoint.clone());
        }
    }
}

impl DeviceOp for Device {
    fn id(&self) -> usize {
        self.id.as_usize()
    }

    fn backend_name(&self) -> &str {
        "xhci"
    }

    fn descriptor(&self) -> &DeviceDescriptor {
        &self.desc
    }

    fn ctrl_ep_ref(&self) -> &EndpointHandle {
        self.control_endpoint()
    }

    fn ctrl_ep_mut(&mut self) -> &mut EndpointHandle {
        self.control_endpoint_mut()
    }

    fn claim_interface<'a>(
        &'a mut self,
        interface: u8,
        alternate: u8,
    ) -> BoxFuture<'a, Result<BTreeMap<u8, EndpointHandle>>> {
        self._claim_interface(interface, alternate).boxed()
    }

    fn release_interface<'a>(&'a mut self, interface: u8) -> BoxFuture<'a, Result<()>> {
        self._release_interface(interface).boxed()
    }

    fn set_configuration<'a>(&'a mut self, configuration_value: u8) -> BoxFuture<'a, Result<()>> {
        self._set_configuration(configuration_value).boxed()
    }

    fn disconnect(&mut self) -> BoxFuture<'_, Result<()>> {
        self._disconnect().boxed()
    }

    fn configuration_descriptors(&self) -> &[ConfigurationDescriptor] {
        &self.config_desc
    }

    fn update_hub(&mut self, params: HubParams) -> BoxFuture<'_, Result<()>> {
        self.update_hub_inner(params).boxed()
    }
}

/// The first of `descriptors` that another interface already owns, with that interface.
fn endpoint_conflict(
    owners: &BTreeMap<u8, u8>,
    interface: u8,
    descriptors: &[EndpointDescriptor],
) -> Option<(u8, u8)> {
    descriptors.iter().find_map(|descriptor| {
        let owner = *owners.get(&descriptor.address)?;
        (owner != interface).then_some((descriptor.address, owner))
    })
}

/// The endpoints of an interface's alternate setting in the active configuration (`current`), or in any
/// configuration before one is selected. Configurations may reuse interface numbers with other endpoints.
fn interface_endpoints(
    configurations: &[ConfigurationDescriptor],
    current: Option<u8>,
    interface: u8,
    alternate: u8,
) -> Option<&[EndpointDescriptor]> {
    configurations
        .iter()
        .filter(|config| current.is_none_or(|value| config.configuration_value == value))
        .flat_map(|config| config.interfaces.iter())
        .filter(|iface| iface.interface_number == interface)
        .flat_map(|iface| iface.alt_settings.iter())
        .find(|alt| alt.alternate_setting == alternate)
        .map(|alt| alt.endpoints.as_slice())
}

/// Whether the first 8 bytes of a device descriptor are usable, as `get_bMaxPacketSize0` (hub.c): a device
/// descriptor whose bMaxPacketSize0 is 8, 16, 32, 64 or 9 (512 at SuperSpeed).
fn max_packet0_is_valid(descriptor_type: u8, max_packet_size_0: u8) -> bool {
    descriptor_type == DescriptorType::DEVICE.0 && matches!(max_packet_size_0, 8 | 16 | 32 | 64 | 9)
}

/// The EP0 max packet a device descriptor gives, as `hub_port_init` (hub.c): bMaxPacketSize0 is the size, or its
/// log2 at SuperSpeed and above. The default guess stands if it matches; full and high speed may change it to 8,
/// 16, 32 or 64 and SuperSpeed to any power of two up to 2^16; anything else is invalid (`None`, the attempt
/// fails).
fn ep0_max_packet_size(speed: Speed, default: u16, max_packet_size_0: u8) -> Option<u16> {
    let super_speed = matches!(speed, Speed::SuperSpeed | Speed::SuperSpeedPlus);
    let size = if super_speed {
        if max_packet_size_0 <= 16 {
            1u32 << max_packet_size_0
        } else {
            0
        }
    } else {
        u32::from(max_packet_size_0)
    };
    if size == u32::from(default) {
        return Some(default);
    }
    let valid = match speed {
        Speed::Full | Speed::High => matches!(size, 8 | 16 | 32 | 64),
        _ => super_speed && size > 0,
    };
    // 2^16 does not fit EP0's 16-bit Max Packet Size; Linux's u16 wMaxPacketSize would truncate it to 0.
    valid.then(|| u16::try_from(size).ok()).flatten()
}

#[cfg(test)]
mod lucina_tests {
    use alloc::{collections::BTreeMap, vec, vec::Vec};

    use usb_if::{
        descriptor::{ConfigurationDescriptor, EndpointDescriptor, EndpointType, InterfaceDescriptor, InterfaceDescriptors},
        transfer::Direction,
    };

    use super::{endpoint_conflict, interface_endpoints};

    fn endpoint(address: u8, transfer_type: EndpointType, max_packet_size: u16) -> EndpointDescriptor {
        EndpointDescriptor {
            address,
            max_packet_size,
            transfer_type,
            direction: Direction::from_address(address),
            packets_per_microframe: 1,
            interval: 0,
        }
    }

    fn interface(number: u8, alternate: u8, endpoints: Vec<EndpointDescriptor>) -> InterfaceDescriptor {
        InterfaceDescriptor {
            interface_number: number,
            alternate_setting: alternate,
            class: 0,
            subclass: 0,
            protocol: 0,
            string_index: None,
            string: None,
            num_endpoints: endpoints.len() as u8,
            endpoints,
        }
    }

    fn configuration(value: u8, interfaces: Vec<(u8, Vec<InterfaceDescriptor>)>) -> ConfigurationDescriptor {
        ConfigurationDescriptor {
            num_interfaces: interfaces.len() as u8,
            configuration_value: value,
            attributes: 0xa0,
            max_power: 50,
            string_index: None,
            string: None,
            interfaces: interfaces
                .into_iter()
                .map(|(interface_number, alt_settings)| InterfaceDescriptors {
                    interface_number,
                    alt_settings,
                })
                .collect(),
            raw: Vec::new(),
        }
    }

    /// A Realtek RTL8152 (0bda:8152), as the Galaxy Book Go's screen showed it:
    ///   cfg 1 if 0.0 ff/ff/00 81:Bulk:512 02:Bulk:512 83:Interrupt:2
    ///   cfg 2 if 0.0 02/06/00 83:Interrupt:16
    ///   cfg 2 if 1.0 0a/00/00
    ///   cfg 2 if 1.1 0a/00/00 81:Bulk:512 02:Bulk:512
    fn rtl8152() -> Vec<ConfigurationDescriptor> {
        let bulk_in = || endpoint(0x81, EndpointType::Bulk, 512);
        let bulk_out = || endpoint(0x02, EndpointType::Bulk, 512);
        vec![
            configuration(
                1,
                vec![(
                    0,
                    vec![interface(
                        0,
                        0,
                        vec![bulk_in(), bulk_out(), endpoint(0x83, EndpointType::Interrupt, 2)],
                    )],
                )],
            ),
            configuration(
                2,
                vec![
                    (0, vec![interface(0, 0, vec![endpoint(0x83, EndpointType::Interrupt, 16)])]),
                    (
                        1,
                        vec![
                            interface(1, 0, vec![]),
                            interface(1, 1, vec![bulk_in(), bulk_out()]),
                        ],
                    ),
                ],
            ),
        ]
    }

    /// crab-usb 0.12.1's lookup, kept to show the failure it caused: all configurations, first match wins.
    fn lookup_as_published<'a>(
        configs: &'a [ConfigurationDescriptor],
        interface: u8,
        alternate: u8,
    ) -> Option<&'a [EndpointDescriptor]> {
        for config in configs {
            for iface in &config.interfaces {
                if iface.interface_number == interface {
                    for alt in &iface.alt_settings {
                        if alt.alternate_setting == alternate {
                            return Some(&alt.endpoints);
                        }
                    }
                }
            }
        }
        None
    }

    /// What the CDC driver does once configuration 2 is selected: claim control interface 0, then data interface
    /// 1 alternate 1, with `lookup`. Returns the first ownership conflict, the check behind `InvalidParameter`.
    fn claim_like_the_cdc_driver<'a>(
        lookup: impl Fn(u8, u8) -> Option<&'a [EndpointDescriptor]>,
    ) -> Option<(u8, u8)> {
        let mut owners = BTreeMap::new();
        for (interface, alternate) in [(0, 0), (1, 1)] {
            let endpoints = lookup(interface, alternate).unwrap();
            if let Some(conflict) = endpoint_conflict(&owners, interface, endpoints) {
                return Some(conflict);
            }
            for endpoint in endpoints {
                owners.insert(endpoint.address, interface);
            }
        }
        None
    }

    /// The laptop's failure: "claim control interface 0: ok", "claim data interface 1.1: FAILED InvalidParameter".
    #[test]
    fn published_lookup_reproduces_the_laptop_failure() {
        let configs = rtl8152();
        assert_eq!(
            claim_like_the_cdc_driver(|i, a| lookup_as_published(&configs, i, a)),
            Some((0x81, 0))
        );
    }

    #[test]
    fn active_configuration_lookup_claims_both_interfaces() {
        let configs = rtl8152();
        assert_eq!(
            claim_like_the_cdc_driver(|i, a| interface_endpoints(&configs, Some(2), i, a)),
            None
        );
    }

    fn addresses(endpoints: Option<&[EndpointDescriptor]>) -> Option<Vec<u8>> {
        endpoints.map(|eps| eps.iter().map(|ep| ep.address).collect())
    }

    #[test]
    fn looks_only_in_the_active_configuration() {
        let configs = rtl8152();
        // The bug: interface 0 of configuration 2 used to give configuration 1's bulk endpoints.
        assert_eq!(addresses(interface_endpoints(&configs, Some(2), 0, 0)), Some(vec![0x83]));
        assert_eq!(addresses(interface_endpoints(&configs, Some(2), 1, 0)), Some(vec![]));
        assert_eq!(addresses(interface_endpoints(&configs, Some(2), 1, 1)), Some(vec![0x81, 0x02]));
        assert_eq!(
            addresses(interface_endpoints(&configs, Some(1), 0, 0)),
            Some(vec![0x81, 0x02, 0x83])
        );
    }

    #[test]
    fn what_another_configuration_has_is_not_found() {
        let configs = rtl8152();
        assert_eq!(addresses(interface_endpoints(&configs, Some(1), 1, 1)), None);
        assert_eq!(addresses(interface_endpoints(&configs, Some(2), 0, 1)), None);
        assert_eq!(addresses(interface_endpoints(&configs, Some(3), 0, 0)), None);
    }

    #[test]
    fn before_a_configuration_is_selected_the_first_match_counts() {
        let configs = rtl8152();
        assert_eq!(
            addresses(interface_endpoints(&configs, None, 0, 0)),
            Some(vec![0x81, 0x02, 0x83])
        );
    }

    #[test]
    fn device_descriptor_header_is_checked_as_get_bmaxpacketsize0() {
        use super::max_packet0_is_valid;
        for size in [8, 16, 32, 64, 9] {
            assert!(max_packet0_is_valid(1, size), "bMaxPacketSize0 {size}");
        }
        for size in [0, 7, 10, 128, 255] {
            assert!(!max_packet0_is_valid(1, size), "bMaxPacketSize0 {size}");
        }
        assert!(!max_packet0_is_valid(2, 64), "not a device descriptor");
    }

    #[test]
    fn ep0_max_packet_is_decoded_per_speed_as_hub_port_init() {
        use usb_if::host::hub::Speed;

        use super::{ep0_max_packet_size, parse_default_max_packet_size_from_port_speed as default};
        let size = |speed| move |max_packet_size_0| ep0_max_packet_size(speed, default(speed), max_packet_size_0);

        // SuperSpeed gives the log2: 9 is 512, the default, and was written as 9 bytes before.
        assert_eq!(size(Speed::SuperSpeed)(9), Some(512));
        assert_eq!(size(Speed::SuperSpeedPlus)(9), Some(512));
        assert_eq!(size(Speed::SuperSpeed)(8), Some(256));
        assert_eq!(size(Speed::SuperSpeed)(0), Some(1));
        assert_eq!(size(Speed::SuperSpeed)(16), None, "2^16 does not fit");
        assert_eq!(size(Speed::SuperSpeed)(17), None);
        // Full and high speed: the size itself, 8 to 64.
        for value in [8, 16, 32, 64] {
            assert_eq!(size(Speed::Full)(value), Some(u16::from(value)));
            assert_eq!(size(Speed::High)(value), Some(u16::from(value)));
        }
        assert_eq!(size(Speed::High)(9), None);
        assert_eq!(size(Speed::Full)(0), None, "0 no longer becomes 8");
        // Low speed: 8 only.
        assert_eq!(size(Speed::Low)(8), Some(8));
        assert_eq!(size(Speed::Low)(64), None);
    }

    #[test]
    fn enumeration_retry_counts_are_linux_hub_port_init() {
        use crate::backend::kmod::hub::port::{
            GET_DESCRIPTOR_RETRY_DELAY, GET_DESCRIPTOR_TRIES, GET_MAXPACKET0_TRIES, SET_ADDRESS_RETRY_DELAY,
            SET_ADDRESS_SETTLE_TIME, SET_ADDRESS_TRIES,
        };
        assert_eq!((SET_ADDRESS_TRIES, GET_DESCRIPTOR_TRIES, GET_MAXPACKET0_TRIES), (2, 2, 3));
        assert_eq!(
            (SET_ADDRESS_RETRY_DELAY, GET_DESCRIPTOR_RETRY_DELAY, SET_ADDRESS_SETTLE_TIME),
            (200, 100, 10)
        );
    }
}

#[cfg(test)]
mod quarantine_tests {
    use alloc::{boxed::Box, collections::BTreeMap, sync::Arc, vec};
    use core::{
        future::Future,
        pin::Pin,
        ptr::NonNull,
        sync::atomic::{AtomicBool, Ordering},
        task::{Context, Poll, Waker},
    };

    use ax_sync::{SpinLock as Mutex, SpinRwLock as RwLock};
    use dma_api::DmaDirection;
    use usb_if::{endpoint::EndpointInfo, host::hub::Speed};

    use super::{ContextData, Device, ManuallyDrop, XhciEndpoint};
    use crate::backend::{
        Dci,
        kmod::xhci::{
            SlotId,
            cmd::CommandRing,
            endpoint::tests::{live_coherent, test_kernel},
            reg::{SlotBell, XhciRegisters},
            transfer::TransferResultHandler,
        },
        ty::ep::EndpointHandle,
    };

    #[repr(align(64))]
    struct AlignedMmio([u32; 4096]);

    fn block_on_ready<F: Future>(mut future: F) -> F::Output {
        let mut context = Context::from_waker(Waker::noop());
        // SAFETY: `future` is not moved while pinned: it lives on this frame until it is dropped.
        match unsafe { Pin::new_unchecked(&mut future) }.poll(&mut context) {
            Poll::Ready(output) => output,
            Poll::Pending => panic!("test future unexpectedly pending"),
        }
    }

    /// An addressed device, as `Device::new` and `init` leave it, with EP0 and one bulk endpoint, on a controller
    /// whose death the test sets.
    fn device_on(mmio: &mut AlignedMmio, dead: Arc<AtomicBool>) -> (Device, CommandRing) {
        // The xHCI accessor requires a doorbell entry for slot one and a port array.
        mmio.0[1] = 2 | (1 << 24);
        let mmio_base = NonNull::new(mmio.0.as_mut_ptr().cast::<u8>()).unwrap();
        let registers = Arc::new(RwLock::new(XhciRegisters::new(mmio_base)));
        let kernel = test_kernel();
        let cmd =
            CommandRing::new(DmaDirection::Bidirectional, &kernel, registers.clone(), dead).unwrap();
        let id = SlotId::from(1);
        let bell = Arc::new(Mutex::new(SlotBell::new(id, registers.read().clone())));
        let endpoint = |dci: u8| {
            XhciEndpoint::new(id, Dci::from(dci), &kernel, bell.clone(), cmd.clone()).unwrap()
        };
        let control = EndpointHandle::new(EndpointInfo::control(), endpoint(1));
        let bulk = EndpointHandle::new(EndpointInfo::control(), endpoint(2));
        let device = Device {
            id,
            ctx: ManuallyDrop::new(ContextData::new(false, &kernel).unwrap()),
            slot_disabled: false,
            desc: unsafe { core::mem::zeroed() },
            ctrl_ep: Some(control),
            transfer_result_handler: TransferResultHandler::new(registers),
            bell,
            kernel,
            current_config_value: None,
            config_desc: vec![],
            port_speed: Speed::Full,
            eps: BTreeMap::from([(0x81, bulk)]),
            ep_interfaces: BTreeMap::new(),
            interface_alternates: BTreeMap::new(),
            cmd: cmd.clone(),
            behind_tt: false,
        };
        (device, cmd)
    }

    #[test]
    fn a_failed_disable_slot_keeps_the_rings_and_context_after_the_device_is_dropped() {
        let mut mmio = Box::new(AlignedMmio([0; 4096]));
        let dead = Arc::new(AtomicBool::new(false));
        let (mut device, cmd) = device_on(&mut mmio, dead.clone());
        let allocated = live_coherent();
        // Dead, never seen to halt: Stop Endpoint and Disable Slot both fail with NoDevice.
        dead.store(true, Ordering::Release);

        // As `new_device` does after a failed `init`.
        block_on_ready(device.release_failed());
        drop(device);

        assert_eq!(live_coherent(), allocated, "no ring, context or command ring freed");
        // Both endpoints from `_disconnect`, then the context and both endpoints again from `Drop`.
        assert_eq!(cmd.quarantine().len(), 5);
    }

    #[test]
    fn a_failed_disable_slot_on_a_halted_controller_frees_the_device() {
        let before = live_coherent();
        let mut mmio = Box::new(AlignedMmio([0; 4096]));
        let dead = Arc::new(AtomicBool::new(false));
        let (mut device, cmd) = device_on(&mut mmio, dead.clone());
        dead.store(true, Ordering::Release);
        cmd.halted_flag().store(true, Ordering::Release);

        block_on_ready(device.release_failed());
        drop(device);
        assert_eq!(cmd.quarantine().len(), 0);
        drop(cmd);

        assert_eq!(live_coherent(), before, "HCH seen: everything freed, as before");
    }
}
