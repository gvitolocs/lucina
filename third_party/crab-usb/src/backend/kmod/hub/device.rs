//! Hub 设备
//!
//! 表示一个 Hub 设备（Root Hub 或 External Hub），管理端口状态和设备枚举。

use alloc::{boxed::Box, vec, vec::Vec};
use core::time::Duration;

use futures::{FutureExt, future::BoxFuture};
use usb_if::{
    descriptor::{Class, ConfigurationDescriptor, DeviceDescriptor, EndpointType},
    err::USBError,
    host::{
        ControlSetup,
        hub::{HubDescriptor, PortFeature, PortStatus, PortStatusChange, Speed},
    },
    transfer::{Recipient, Request, RequestType},
};

use super::{
    HubOp, PortEvent,
    port::{
        Debounce, DebounceStep, HUB_BH_RESET_TIME, HUB_DEBOUNCE_STEP, HUB_LONG_RESET_TIME,
        HUB_SHORT_RESET_TIME, PORT_RESET_TRIES, RESET_RECOVERY_TIME, ResetSample, ResetWait,
        ResetWaitResult, classify_reset, power_on_good_delay, reset_try_ends,
    },
};
use crate::{
    Device, InterfaceSession,
    backend::kmod::hub::{HubInfo, PortChangeInfo},
    osal::Kernel,
};

// USB 2 and USB 3 hub port status and feature values (Linux include/uapi/linux/usb/ch11.h).
const USB_PORT_STAT_CONNECTION: u16 = 0x0001;
const USB_PORT_STAT_ENABLE: u16 = 0x0002;
const USB_PORT_STAT_RESET: u16 = 0x0010;
const USB_PORT_STAT_LOW_SPEED: u16 = 0x0200;
const USB_PORT_STAT_HIGH_SPEED: u16 = 0x0400;
const USB_PORT_STAT_LINK_STATE: u16 = 0x01e0;
const USB_SS_PORT_LS_U3: u16 = 0x0060;
const USB_SS_PORT_LS_SS_INACTIVE: u16 = 0x00c0;
const USB_SS_PORT_LS_COMP_MOD: u16 = 0x0140;
const USB_PORT_STAT_C_CONNECTION: u16 = 0x0001;
const USB_PORT_STAT_C_BH_RESET: u16 = 0x0020;
const USB_PORT_STAT_C_LINK_STATE: u16 = 0x0040;
const USB_PORT_FEAT_LINK_STATE: u16 = 5;
const USB_PORT_FEAT_C_PORT_LINK_STATE: u16 = 25;
const USB_PORT_FEAT_BH_PORT_RESET: u16 = 28;
const USB_PORT_FEAT_C_BH_PORT_RESET: u16 = 29;

// Hub Device descriptor
// USB Hub class device protocols
const HUB_PR_FS: u8 = 0; /* Full speed hub */
const HUB_PR_HS_SINGLE_TT: u8 = 1; /* Hi-speed hub with single TT */
const HUB_PR_HS_MULTI_TT: u8 = 2; /* Hi-speed hub with multiple TT */
const HUB_PR_SS: u8 = 3; /* Super speed hub */

/// Hub 设备
///
/// 表示一个 Hub 设备（Root Hub 或 External Hub）。
pub struct HubDevice {
    settings: HubSettings,
    data: Box<Inner>,
    kernel: Kernel,
}

struct Inner {
    /// Hub 状态
    pub state: HubState,

    /// 端口数量
    pub num_ports: u8,

    /// 端口列表
    pub ports: Vec<Port>,

    pub dev: Device,

    pub interface_session: Option<InterfaceSession>,

    pub descriptor: HubDescriptor,

    pub parent_hub_slot_id: u8,

    /// Root Hub 端口 ID（如果这是外部 Hub）
    pub root_port_id: u8,
}

pub struct HubSettings {
    pub config_value: u8,
    pub interface_number: u8,
    pub alt_setting: u8,
}

impl HubOp for HubDevice {
    fn slot_id(&self) -> u8 {
        self.data.dev.slot_id()
    }

    fn init(&mut self, info: HubInfo) -> BoxFuture<'_, Result<HubInfo, USBError>> {
        self.configure(info).boxed()
    }

    fn changed_ports<'a>(&'a mut self) -> BoxFuture<'a, Result<Vec<PortEvent>, USBError>> {
        self.changed_ports().boxed()
    }

    fn disconnect(&mut self) -> BoxFuture<'_, Result<(), USBError>> {
        self.data.dev.disconnect().boxed()
    }

    fn reset_port_for_enumeration(
        &mut self,
        port_id: u8,
        power_cycle: bool,
    ) -> BoxFuture<'_, Result<PortChangeInfo, USBError>> {
        self.reset_for_enumeration(port_id, power_cycle).boxed()
    }

    fn give_up_port(&mut self, port_id: u8) -> BoxFuture<'_, ()> {
        async move {
            if let Err(err) = self.disable_port(port_id).await {
                warn!("Hub port {port_id}: cannot disable: {err:?}");
            }
            if let Some(port) = self.data.ports.get_mut(usize::from(port_id) - 1) {
                port.state = PortState::Uninit;
                port.pending_connect = false;
            }
        }
        .boxed()
    }
}

impl HubDevice {
    /// returns (config_value, interface_number) if the device is a hub
    pub fn is_hub(
        desc: &DeviceDescriptor,
        configs: &[ConfigurationDescriptor],
    ) -> Option<HubSettings> {
        if !matches!(desc.class(), Class::Hub(_)) {
            return None;
        }
        let Some(config) = configs.first() else {
            warn!("Hub device has no configurations");
            return None;
        };

        for interface in &config.interfaces {
            for alt in &interface.alt_settings {
                if alt.subclass != 0x00 && alt.protocol != 0x00 {
                    continue;
                }

                if alt.num_endpoints != 1 {
                    continue;
                }

                if alt.endpoints[0].transfer_type != EndpointType::Interrupt
                    || alt.endpoints[0].direction != usb_if::transfer::Direction::In
                {
                    continue;
                }

                return Some(HubSettings {
                    config_value: config.configuration_value,
                    interface_number: interface.interface_number,
                    alt_setting: alt.alternate_setting,
                });
            }
        }

        None
    }

    /// 创建新的 Hub 设备
    pub async fn new(
        dev: Device,
        settings: HubSettings,
        root_port_id: u8,
        parent_hub_slot_id: u8,
        kernel: &Kernel,
    ) -> Result<Self, USBError> {
        Ok(Self {
            settings,
            data: Box::new(Inner {
                state: HubState::Uninitialized,
                num_ports: 0,
                ports: vec![],
                dev,
                interface_session: None,
                descriptor: unsafe { core::mem::zeroed() },
                parent_hub_slot_id,
                root_port_id,
            }),
            kernel: kernel.clone(),
        })
    }

    pub async fn changed_ports(&mut self) -> Result<Vec<PortEvent>, USBError> {
        let mut changed_ports = vec![];

        // 收集所有端口号，避免借用冲突

        for port_idx in 0..self.data.num_ports {
            let port_id = port_idx + 1;
            // A failure on one port does not drop the other ports' events of this hub.
            if let Err(e) = self.process_port(port_idx, &mut changed_ports).await {
                warn!("Hub port {port_id}: {e:?}");
            }
        }

        Ok(changed_ports)
    }

    async fn process_port(
        &mut self,
        port_idx: u8,
        changed_ports: &mut Vec<PortEvent>,
    ) -> Result<(), USBError> {
        let port_id = port_idx + 1;
        let (status, change) = self.get_port_status(port_id).await?;

        debug!("Port {} status: {:?}", port_id, status);

        if change.connection_changed {
            info!("Port {} connection changed: {}", port_id, status.connected);
            // 清除连接变化标志
            self.clear_port_feature(port_id, PortFeature::CConnection)
                .await?;
            self.data.ports[port_idx as usize].pending_connect = true;
        }

        // `hub_port_connect_change` for a port with a device: it goes when the port is empty, or when its connection
        // changed and the port is no longer enabled (unplugged and plugged in again between two looks; `pending_connect`
        // then connects the new device next time). A change on a port still enabled keeps the device.
        if self.data.ports[port_idx as usize].state == PortState::Probed {
            if hub_port_device_gone(status.connected, change.connection_changed, status.enabled) {
                self.data.ports[port_idx as usize].state = PortState::Uninit;
                changed_ports.push(PortEvent::Disconnected { port_id });
                return Ok(());
            }
            if change.connection_changed {
                self.data.ports[port_idx as usize].pending_connect = false;
            }
        }

        if self.is_superspeed() && self.data.ports[port_idx as usize].state == PortState::Uninit {
            // Linux's `port_event`: a USB 3 link in SS.Inactive or Compliance gets a warm reset first.
            // Only on a change or a connect to handle (Linux's `port_event`), not on every poll of a stuck port.
            let (raw_status, raw_change) = self.get_port_status_raw(port_id).await?;
            let changed = raw_change & (USB_PORT_STAT_C_BH_RESET | USB_PORT_STAT_C_LINK_STATE) != 0
                || self.data.ports[port_idx as usize].pending_connect;
            if changed && self.warm_reset_required(raw_status) {
                debug!("Port {port_id}: link needs a warm reset");
                let _ = self
                    .hub_port_reset(port_id, true, HUB_BH_RESET_TIME, false)
                    .await?;
                self.data.ports[port_idx as usize].pending_connect = true;
            }
        }

        // Only a connected port is debounced (see the root hub: a disconnect's 2 s debounce would stall the USB
        // thread).
        if self.data.ports[port_idx as usize].state == PortState::Uninit
            && core::mem::take(&mut self.data.ports[port_idx as usize].pending_connect)
            && status.connected
        {
            // `hub_port_connect_change`: debounce, then report the port; the enumeration attempt resets it.
            match self.debounce_port(port_id).await? {
                Some(raw_status) => {
                    info!("Port {port_id} connected (status {raw_status:#06x})");
                    self.data.ports[port_idx as usize].state = PortState::Probed;
                    changed_ports.push(PortEvent::Connected(PortChangeInfo {
                        root_port_id: self.root_port_id(),
                        port_id,
                        port_speed: self.port_speed(raw_status),
                    }));
                }
                None => {
                    warn!("Port {port_id}: connect never stable, ignored until it changes");
                }
            }
        }

        if change.enabled_changed {
            info!("Port {} enabled changed: {}", port_id, status.enabled);
            self.clear_port_feature(port_id, PortFeature::CEnable)
                .await?;
            if let Some(port) = self.data.ports.iter_mut().find(|p| p.id == port_id) {
                port.status = status;
            }
        }

        if change.reset_complete {
            debug!("Port {} reset complete", port_id);
            self.clear_port_feature(port_id, PortFeature::CReset)
                .await?;
            if let Some(port) = self.data.ports.iter_mut().find(|p| p.id == port_id) {
                port.status = status;
            }
        }

        Ok(())
    }

    pub fn is_superspeed(&self) -> bool {
        self.data.dev.descriptor().protocol == 3
    }

    pub async fn configure(&mut self, info: HubInfo) -> Result<HubInfo, USBError> {
        // 第二阶段：获取 Hub 描述符（带重试）
        debug!("Configuring hub device, depth={}...", info.hub_depth);
        let mut info = info;

        trace!(
            "settings: config_value={}, interface_number={}, alt_setting={}",
            self.settings.config_value, self.settings.interface_number, self.settings.alt_setting
        );

        let descriptor = self.get_hub_descriptor().await?;

        self.data.descriptor = descriptor;

        if self.hub_descriptor().bNbrPorts == 0 {
            return Err(USBError::from("Hub has zero ports"));
        }
        self.data.num_ports = self.hub_descriptor().bNbrPorts;

        // 解析 Hub 特性和配置参数（参考 U-Boot usb_hub_configure）
        let characteristics = self.data.descriptor.hub_characteristics();

        // 解析 TT 思考时间（Bits[6:5]，Linux hub.c:1609-1635）: 8/16/24/32 FS bit times map to 666 x (bits + 1)
        // ns; Linux leaves 0 only for 8 bit times on a full speed hub (bDeviceProtocol 0).
        let ttt_bits = (characteristics >> 5) & 0x03;
        info.tt.think_time_ns = tt_think_time_ns(
            ttt_bits as u8,
            self.data.dev.descriptor().protocol == HUB_PR_FS,
        ) as usize;

        // 判断 Multi-TT（参考 U-Boot）
        // protocol = 0: Full Speed Hub (no TT)
        // protocol = 1: High Speed Single TT
        // protocol = 2: High Speed Multi TT
        // protocol = 3: SuperSpeed Hub (no TT)
        let device_protocol = self.data.dev.descriptor().protocol;

        match device_protocol {
            HUB_PR_FS => {
                info.speed = Speed::Full;
            }
            HUB_PR_HS_SINGLE_TT => {
                info.speed = Speed::High;
                debug!("Hub is High Speed with Single TT");
            }
            HUB_PR_HS_MULTI_TT => {
                info.speed = Speed::High;
                debug!("Hub is High Speed with Multiple TTs");
                match self.data.dev.claim_interface(0, 1).await {
                    Ok(session) => {
                        self.data.interface_session = Some(session);
                        debug!("TT per port");
                        info.tt.multi = true;
                    }
                    Err(e) => {
                        debug!("Using single TT due to claim interface failure: {e}");
                    }
                }
            }
            HUB_PR_SS => {
                info.speed = Speed::SuperSpeed;
            }
            _ => {
                warn!("Unknown hub protocol: {}", device_protocol);
            }
        }

        debug!(
            "Hub parameters: ports={}, protocol={}, multi_tt={}, tt_think_time={}ns",
            self.data.num_ports, device_protocol, info.tt.multi, info.tt.think_time_ns
        );

        let status = self.get_hub_status().await?;

        debug!(
            "local power source: {}",
            if status.local_power_source() {
                "lost (inactive)"
            } else {
                "good"
            }
        );

        debug!(
            "over current condition exists: {}",
            if status.over_current() { "" } else { "no " }
        );

        // 构造 HubParams
        let params = crate::backend::ty::HubParams {
            num_ports: self.data.num_ports,
            multi_tt: info.tt.multi,
            tt_think_time_ns: info.tt.think_time_ns as _,
            parent_hub_slot_id: self.data.parent_hub_slot_id,
            root_hub_port_number: self.data.root_port_id,
        };

        // 更新 xHCI Slot Context（如果后端支持）
        self.data.dev.update_hub(params).await?;

        if info.hub_depth > -1 && self.is_superspeed() {
            assert!(
                info.hub_depth < 5,
                "Hub depth too large: {}",
                info.hub_depth
            );

            // 外部 SuperSpeed Hub 需要设置 Hub 深度
            self.set_hub_depth(info.hub_depth as _).await?;
            debug!("Set hub depth to {}", info.hub_depth);
        }

        // 第三阶段：初始化端口状态（参考 Linux hub_activate）
        // 初始化所有端口为 Disconnected 状态
        self.data.ports = (1..=self.data.num_ports).map(Port::new).collect();

        self.hub_power_on().await?;

        // 标记 Hub 为运行状态
        self.data.state = HubState::Running;
        debug!("Hub initialized with {} ports", self.data.num_ports);
        Ok(info)
    }

    async fn set_hub_depth(&mut self, depth: u8) -> Result<(), USBError> {
        self.data
            .dev
            .ctrl_ep_mut()
            .control_out(
                ControlSetup {
                    request_type: RequestType::Class,
                    recipient: Recipient::Device,
                    request: Request::Other(0x0c),
                    value: depth as _,
                    index: 0,
                },
                &[],
            )
            .await?;
        Ok(())
    }

    fn hub_descriptor(&self) -> &HubDescriptor {
        &self.data.descriptor
    }

    /// 获取 Hub 描述符（参考 Linux 内核实现）
    async fn get_hub_descriptor(&mut self) -> Result<HubDescriptor, USBError> {
        let mut buff = vec![0u8; 4]; // Hub 描述符最小长度
        self.read_hub_descriptor_raw(&mut buff).await?;
        let desc_len = buff[0] as usize;
        trace!("Hub descriptor length from initial read: {}", desc_len);

        let mut full_buff = vec![0u8; desc_len];

        self.read_hub_descriptor_raw(&mut full_buff).await?;

        let desc = unsafe { (full_buff.as_ptr() as *const HubDescriptor).read_unaligned() };
        Ok(desc)
    }

    async fn read_hub_descriptor_raw(&mut self, buff: &mut [u8]) -> Result<(), USBError> {
        const DT_SS_HUB: u16 = 0x0a;
        const DT_HUB: u16 = 0x9;
        const TYPE_CLASS: u16 = 1 << 5;

        let dtype = if self.is_superspeed() {
            DT_SS_HUB
        } else {
            DT_HUB
        } | TYPE_CLASS;

        let n = self
            .data
            .dev
            .ctrl_ep_mut()
            .control_in(
                ControlSetup {
                    request_type: RequestType::Class,
                    recipient: Recipient::Device,
                    request: Request::GetDescriptor,
                    value: dtype << 8,
                    index: 0,
                },
                buff,
            )
            .await?;
        trace!("Hub raw descriptor read {n} bytes");

        Ok(())
    }

    async fn hub_power_on(&mut self) -> Result<(), USBError> {
        for port_id in 1..=self.data.num_ports {
            self.set_port_feature(port_id, PortFeature::Power).await?;
            debug!("Powered on port {}", port_id);
        }

        self.kernel
            .delay(Duration::from_millis(self.power_on_good_delay()));
        Ok(())
    }

    async fn get_hub_status(&mut self) -> Result<HubStatus, USBError> {
        let mut buffer = vec![0u8; size_of::<HubStatus>()];

        self.data
            .dev
            .ctrl_ep_mut()
            .control_in(
                ControlSetup {
                    request_type: RequestType::Class,
                    recipient: Recipient::Device,
                    request: Request::GetStatus,
                    value: 0,
                    index: 0,
                },
                &mut buffer,
            )
            .await?;

        let status = u16::from_le_bytes([buffer[0], buffer[1]]);
        let change = u16::from_le_bytes([buffer[2], buffer[3]]);
        trace!("Hub raw status: 0x{:04x}, change: 0x{:04x}", status, change);
        Ok(HubStatus { status, change })
    }

    // ========== 端口状态获取方法 ==========

    /// 获取端口状态 (参照 Linux usb_hub_port_status)
    ///
    /// 返回: (端口状态, 状态变化标志)
    async fn get_port_status(
        &mut self,
        port_id: u8,
    ) -> Result<(PortStatus, PortStatusChange), USBError> {
        let (status_raw, change_raw) = self.get_port_status_raw(port_id).await?;
        Ok((
            self.parse_port_status(status_raw),
            self.parse_port_change(change_raw),
        ))
    }

    /// GetPortStatus: (wPortStatus, wPortChange).
    async fn get_port_status_raw(&mut self, port_id: u8) -> Result<(u16, u16), USBError> {
        let mut buffer = vec![0u8; 4]; // wPortStatus (2字节) + wPortChange (2字节)

        self.data
            .dev
            .ctrl_ep_mut()
            .control_in(
                ControlSetup {
                    request_type: RequestType::Class,
                    recipient: Recipient::Other, // Port
                    request: Request::GetStatus,
                    value: 0,
                    index: port_id as u16,
                },
                &mut buffer,
            )
            .await?;

        // 解析端口状态和变化
        let status_raw = u16::from_le_bytes([buffer[0], buffer[1]]);
        let change_raw = u16::from_le_bytes([buffer[2], buffer[3]]);

        trace!(
            "Port {} raw status: 0x{:04x}, change: 0x{:04x}",
            port_id, status_raw, change_raw
        );

        Ok((status_raw, change_raw))
    }

    /// 解析端口状态原始数据
    fn parse_port_status(&self, raw: u16) -> PortStatus {
        parse_port_status_raw(raw, self.is_superspeed())
    }

    /// 解析端口状态变化标志
    fn parse_port_change(&self, raw: u16) -> PortStatusChange {
        PortStatusChange {
            connection_changed: (raw & 0x0001) != 0,
            enabled_changed: (raw & 0x0002) != 0,
            suspend_changed: (raw & 0x0004) != 0,
            over_current_changed: (raw & 0x0008) != 0,
            reset_complete: (raw & 0x0010) != 0,
        }
    }

    /// 设置端口特性
    async fn set_port_feature(
        &mut self,
        port_index: u8,
        feature: PortFeature,
    ) -> Result<(), USBError> {
        self.set_port_feature_raw(u16::from(port_index), feature as u16)
            .await
    }

    /// SetPortFeature with a raw feature selector and wIndex (USB 3 features and link states are not in
    /// `PortFeature`).
    async fn set_port_feature_raw(&mut self, index: u16, feature: u16) -> Result<(), USBError> {
        self.data
            .dev
            .ctrl_ep_mut()
            .control_out(
                ControlSetup {
                    request_type: RequestType::Class,
                    recipient: Recipient::Other,
                    request: Request::SetFeature,
                    value: feature,
                    index,
                },
                &[],
            )
            .await
            .map_err(USBError::from)?;
        Ok(())
    }

    /// 清除端口特性
    async fn clear_port_feature(
        &mut self,
        port_id: u8,
        feature: PortFeature,
    ) -> Result<(), USBError> {
        self.clear_port_feature_raw(port_id, feature as u16).await
    }

    async fn clear_port_feature_raw(&mut self, port_id: u8, feature: u16) -> Result<(), USBError> {
        self.data
            .dev
            .ctrl_ep_mut()
            .control_out(
                ControlSetup {
                    request_type: RequestType::Class,
                    recipient: Recipient::Other,
                    request: Request::ClearFeature,
                    value: feature,
                    index: port_id as u16,
                },
                &[],
            )
            .await
            .map_err(USBError::from)?;
        Ok(())
    }

    // ========== 防抖动机制 ==========

    /// `hub_port_debounce_be_stable`: `Some(wPortStatus)` once the port has been connected for 100 ms, `None` if
    /// it is not by 2 s. Every connect change seen is cleared.
    async fn debounce_port(&mut self, port_id: u8) -> Result<Option<u16>, USBError> {
        let mut debounce = Debounce::new();
        loop {
            let (status, change) = self.get_port_status_raw(port_id).await?;
            let step = debounce.sample(
                status & USB_PORT_STAT_CONNECTION != 0,
                change & USB_PORT_STAT_C_CONNECTION != 0,
            );
            let clear_change = match step {
                DebounceStep::Stable => return Ok(Some(status)),
                DebounceStep::TimedOut { clear_change } | DebounceStep::Again { clear_change } => {
                    clear_change
                }
            };
            if clear_change {
                self.clear_port_feature(port_id, PortFeature::CConnection)
                    .await?;
            }
            if matches!(step, DebounceStep::TimedOut { .. }) {
                return Ok(None);
            }
            self.kernel
                .delay(Duration::from_millis(u64::from(HUB_DEBOUNCE_STEP)));
        }
    }

    // ========== 设备枚举核心方法 ==========

    /// `hub_power_on_good_delay` for this hub.
    fn power_on_good_delay(&self) -> u64 {
        power_on_good_delay(self.hub_descriptor().bPwrOn2PwrGood, false)
    }

    /// `hub_port_warm_reset_required`: a USB 3 port in SS.Inactive or Compliance Mode.
    fn warm_reset_required(&self, status: u16) -> bool {
        self.is_superspeed() && ss_link_needs_warm_reset(status)
    }

    /// The device speed after a reset (`hub_port_wait_reset`): SuperSpeed on a SuperSpeed hub, else from the
    /// USB 2 high and low speed bits.
    fn port_speed(&self, status: u16) -> Speed {
        speed_after_reset(status, self.is_superspeed())
    }

    fn reset_sample(&self, status: u16, change: u16) -> ResetSample {
        ResetSample {
            resetting: status & USB_PORT_STAT_RESET != 0,
            connected: status & USB_PORT_STAT_CONNECTION != 0,
            enabled: status & USB_PORT_STAT_ENABLE != 0,
            connect_change: change & USB_PORT_STAT_C_CONNECTION != 0,
            warm_reset_required: self.warm_reset_required(status),
        }
    }

    /// `hub_port_wait_reset`.
    async fn wait_reset(
        &mut self,
        port_id: u8,
        delay: u64,
    ) -> Result<(ResetWaitResult, u16), USBError> {
        let mut wait = ResetWait::new(delay);
        let mut sleep = wait.first_sleep();
        loop {
            self.kernel.delay(Duration::from_millis(sleep));
            let (status, change) = self.get_port_status_raw(port_id).await?;
            let sample = self.reset_sample(status, change);
            if let Some(next) = wait.after_sample(&sample) {
                sleep = next;
                continue;
            }
            let result = classify_reset(&sample, self.is_superspeed());
            if result == ResetWaitResult::Again {
                self.clear_port_feature(port_id, PortFeature::CConnection)
                    .await?;
            }
            return Ok((result, status));
        }
    }

    /// `hub_port_reset`: hot reset, or warm (BH) reset on a SuperSpeed hub when the link needs it, up to
    /// `PORT_RESET_TRIES` times, then the reset recovery time. `for_device` is Linux's `udev != NULL`.
    async fn hub_port_reset(
        &mut self,
        port_id: u8,
        warm: bool,
        delay: u64,
        for_device: bool,
    ) -> Result<(ResetWaitResult, u16), USBError> {
        let superspeed = self.is_superspeed();
        let mut warm = warm && superspeed;
        if superspeed && !warm {
            let (status, _) = self.get_port_status_raw(port_id).await?;
            warm = self.warm_reset_required(status);
        }
        let mut delay = delay;
        let mut outcome = (ResetWaitResult::Busy, 0);
        let mut finished = false;
        for try_index in 0..PORT_RESET_TRIES {
            let feature = if warm {
                USB_PORT_FEAT_BH_PORT_RESET
            } else {
                PortFeature::Reset as u16
            };
            self.set_port_feature_raw(u16::from(port_id), feature)
                .await?;
            outcome = self.wait_reset(port_id, delay).await?;
            if reset_try_ends(try_index, outcome.0) {
                self.clear_port_feature(port_id, PortFeature::CReset)
                    .await?;
                if !superspeed {
                    finished = true;
                    break;
                }
                self.clear_port_feature_raw(port_id, USB_PORT_FEAT_C_BH_PORT_RESET)
                    .await?;
                self.clear_port_feature_raw(port_id, USB_PORT_FEAT_C_PORT_LINK_STATE)
                    .await?;
                if for_device {
                    self.clear_port_feature(port_id, PortFeature::CConnection)
                        .await?;
                }
                let (status, _) = self.get_port_status_raw(port_id).await?;
                if !self.warm_reset_required(status) {
                    finished = true;
                    break;
                }
                warm = true;
            }
            debug!(
                "Hub port {port_id}: not enabled ({:?}), trying {}reset again",
                outcome.0,
                if warm { "warm " } else { "" }
            );
            delay = HUB_LONG_RESET_TIME;
        }
        if !finished {
            warn!("Hub port {port_id}: cannot enable. Maybe the USB cable is bad?");
        }
        if outcome.0 == ResetWaitResult::Done {
            self.kernel
                .delay(Duration::from_millis(RESET_RECOVERY_TIME));
        }
        Ok(outcome)
    }

    /// The start of each enumeration attempt (`hub_port_connect` / `hub_port_init`) on this hub's port.
    async fn reset_for_enumeration(
        &mut self,
        port_id: u8,
        power_cycle: bool,
    ) -> Result<PortChangeInfo, USBError> {
        if power_cycle {
            let delay = self.power_on_good_delay();
            info!("Hub port {port_id}: power cycle");
            self.clear_port_feature(port_id, PortFeature::Power).await?;
            self.kernel.delay(Duration::from_millis(2 * delay));
            self.set_port_feature(port_id, PortFeature::Power).await?;
            self.kernel.delay(Duration::from_millis(delay));
        }
        let (status, _) = self.get_port_status_raw(port_id).await?;
        if status & USB_PORT_STAT_CONNECTION == 0 {
            return Err(USBError::NotFound);
        }
        // `hub_port_init`: HUB_SHORT_RESET_TIME, or HUB_LONG_RESET_TIME for a low speed device.
        let delay = if self.port_speed(status) == Speed::Low {
            HUB_LONG_RESET_TIME
        } else {
            HUB_SHORT_RESET_TIME
        };
        match self.hub_port_reset(port_id, false, delay, true).await? {
            (ResetWaitResult::Done, status) => Ok(PortChangeInfo {
                root_port_id: self.root_port_id(),
                port_id,
                port_speed: self.port_speed(status),
            }),
            (ResetWaitResult::NotConnected, _) => Err(USBError::NotFound),
            (other, _) => {
                warn!("Hub port {port_id}: cannot enable ({other:?})");
                Err(USBError::Timeout)
            }
        }
    }

    /// `hub_port_disable`: ClearPortFeature(ENABLE) on a USB 2 hub, the link to U3 on a SuperSpeed hub.
    async fn disable_port(&mut self, port_id: u8) -> Result<(), USBError> {
        if self.is_superspeed() {
            self.set_port_feature_raw(
                u16::from(port_id) | (USB_SS_PORT_LS_U3 << 3),
                USB_PORT_FEAT_LINK_STATE,
            )
            .await
        } else {
            self.clear_port_feature(port_id, PortFeature::Enable).await
        }
    }

    /// 获取 Hub 的 root_port_id
    pub fn root_port_id(&self) -> u8 {
        self.data.root_port_id
    }
}

/// 解析端口状态原始数据（纯函数，便于测试）
///
/// SuperSpeed hubs use the USB3 layout (ch11.h): bit 9 is PORT_POWER, bits 8:5 are the
/// link state and there are no low/high speed bits; the port speed is SuperSpeed. Every
/// other hub uses the USB2 layout (bit 8 power, bits 9/10 low/high speed).
fn parse_port_status_raw(raw: u16, superspeed: bool) -> PortStatus {
    if superspeed {
        return PortStatus {
            connected: (raw & 0x0001) != 0,
            enabled: (raw & 0x0002) != 0,
            suspended: (raw & 0x0004) != 0,
            over_current: (raw & 0x0008) != 0,
            resetting: (raw & 0x0010) != 0,
            powered: (raw & 0x0200) != 0,
            low_speed: false,
            high_speed: false,
            speed: Speed::SuperSpeed,
            change: PortStatusChange {
                connection_changed: false,
                enabled_changed: false,
                reset_complete: false,
                suspend_changed: false,
                over_current_changed: false,
            },
        };
    }
    PortStatus {
        connected: (raw & 0x0001) != 0,
        enabled: (raw & 0x0002) != 0,
        suspended: (raw & 0x0004) != 0,
        over_current: (raw & 0x0008) != 0,
        resetting: (raw & 0x0010) != 0,
        powered: (raw & 0x0100) != 0,
        low_speed: (raw & 0x0200) != 0,
        high_speed: (raw & 0x0400) != 0,
        speed: Speed::from_usb2_hub_status(raw),
        change: PortStatusChange {
            connection_changed: false,
            enabled_changed: false,
            reset_complete: false,
            suspend_changed: false,
            over_current_changed: false,
        },
    }
}

/// TT think time in ns for a hub descriptor's TT bits (hub.c:1609-1635): 8/16/24/32 FS bit times are
/// 666 x (bits + 1) ns, except that a full speed hub (bDeviceProtocol 0) keeps 0 for 8 bit times.
fn tt_think_time_ns(ttt_bits: u8, is_full_speed_hub: bool) -> u32 {
    let bits = u32::from(ttt_bits & 0x03);
    if bits == 0 && is_full_speed_hub {
        return 0;
    }
    666 * (bits + 1)
}

/// SS.Inactive or Compliance Mode in a SuperSpeed hub's wPortStatus (`hub_port_warm_reset_required`).
fn ss_link_needs_warm_reset(status: u16) -> bool {
    let link_state = status & USB_PORT_STAT_LINK_STATE;
    link_state == USB_SS_PORT_LS_SS_INACTIVE || link_state == USB_SS_PORT_LS_COMP_MOD
}

/// `hub_port_wait_reset`'s speed rule.
fn speed_after_reset(status: u16, superspeed_hub: bool) -> Speed {
    if superspeed_hub {
        Speed::SuperSpeed
    } else if status & USB_PORT_STAT_HIGH_SPEED != 0 {
        Speed::High
    } else if status & USB_PORT_STAT_LOW_SPEED != 0 {
        Speed::Low
    } else {
        Speed::Full
    }
}

/// Hub 状态
#[derive(Debug)]
pub enum HubState {
    /// 未初始化
    Uninitialized,

    /// 运行中
    Running,
}

/// 端口
pub struct Port {
    /// 端口号（1-based）
    pub id: u8,

    /// 端口状态
    pub status: PortStatus,

    /// 端口状态机
    pub state: PortState,

    /// A connect to handle: every port when the hub starts (Linux `hub_activate`), then each connect change.
    pub pending_connect: bool,
}

impl Port {
    /// 创建新端口
    pub fn new(index: u8) -> Self {
        Self {
            id: index,
            pending_connect: true,
            status: PortStatus {
                connected: false,
                enabled: false,
                suspended: false,
                over_current: false,
                resetting: false,
                powered: false,
                low_speed: false,
                high_speed: false,
                speed: usb_if::host::hub::Speed::Full,
                change: usb_if::host::hub::PortStatusChange {
                    connection_changed: false,
                    enabled_changed: false,
                    reset_complete: false,
                    suspend_changed: false,
                    over_current_changed: false,
                },
            },
            state: PortState::Uninit,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PortState {
    #[default]
    Uninit,
    Reseted,
    Probed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(C)]
struct HubStatus {
    status: u16,
    change: u16,
}

impl HubStatus {
    #[allow(dead_code)]
    fn local_power_source(&self) -> bool {
        (self.status & 0x0001) != 0
    }

    #[allow(dead_code)]
    fn over_current(&self) -> bool {
        (self.status & 0x0002) != 0
    }
}

/// Whether the device on an external hub's port is gone (`hub_port_connect_change`): the port is empty, or its connection
/// changed and it is no longer enabled (unplugged and plugged in again between two looks).
pub(crate) fn hub_port_device_gone(connected: bool, connection_changed: bool, enabled: bool) -> bool {
    !connected || (connection_changed && !enabled)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn superspeed_hub_status_uses_the_usb3_layout() {
        // SS hub, powered (bit 9), connected, enabled, link state bits 5:8 set.
        let raw = 0x0001 | 0x0002 | 0x0200 | (0x3 << 5);
        let status = parse_port_status_raw(raw, true);
        assert!(status.connected);
        assert!(status.enabled);
        assert!(status.powered, "bit 9 is PORT_POWER on a SuperSpeed hub");
        assert!(
            !status.low_speed,
            "bit 9 is not LOW_SPEED on a SuperSpeed hub"
        );
        assert!(!status.high_speed);
        assert_eq!(status.speed, Speed::SuperSpeed);
    }

    #[test]
    fn usb2_hub_status_uses_the_usb2_layout() {
        // USB2 hub, powered (bit 8), connected, low speed (bit 9).
        let raw = 0x0001 | 0x0100 | 0x0200;
        let status = parse_port_status_raw(raw, false);
        assert!(status.powered, "bit 8 is PORT_POWER on a USB2 hub");
        assert!(status.low_speed);
        assert_eq!(status.speed, Speed::Low);

        let raw = 0x0001 | 0x0100 | 0x0400;
        let status = parse_port_status_raw(raw, false);
        assert!(status.high_speed);
        assert_eq!(status.speed, Speed::High);
    }

    #[test]
    fn tt_think_time_maps_8_16_24_32_fs_bits() {
        // hub.c:1611-1635: 666 x (bits + 1); xhci.c:5338 then writes (ns / 666) - 1, giving
        // slot field values 0, 1, 2, 3.
        assert_eq!(tt_think_time_ns(0, false), 666);
        assert_eq!(tt_think_time_ns(1, false), 1332);
        assert_eq!(tt_think_time_ns(2, false), 1998);
        assert_eq!(tt_think_time_ns(3, false), 2664);
        for bits in 0..4 {
            let ns = tt_think_time_ns(bits, false);
            assert_eq!(
                (ns / 666) - 1,
                u32::from(bits),
                "slot field for bits {bits}"
            );
        }
        // A full speed hub keeps 0 for 8 bit times only, as Linux.
        assert_eq!(tt_think_time_ns(0, true), 0);
        assert_eq!(tt_think_time_ns(3, true), 2664);
    }

    #[test]
    fn superspeed_hub_ports_in_inactive_or_compliance_need_a_warm_reset() {
        assert!(ss_link_needs_warm_reset(
            0x0200 | USB_SS_PORT_LS_SS_INACTIVE
        ));
        assert!(ss_link_needs_warm_reset(USB_SS_PORT_LS_COMP_MOD | 0x0001));
        assert!(
            !ss_link_needs_warm_reset(0x0203),
            "U0, powered, connected, enabled"
        );
        assert!(!ss_link_needs_warm_reset(0x02a0), "RxDetect");
    }

    #[test]
    fn speed_after_reset_follows_hub_port_wait_reset() {
        assert_eq!(
            speed_after_reset(0x0203, true),
            Speed::SuperSpeed,
            "bit 9 is power on an SS hub"
        );
        assert_eq!(speed_after_reset(0x0503, false), Speed::High);
        assert_eq!(speed_after_reset(0x0303, false), Speed::Low);
        assert_eq!(speed_after_reset(0x0103, false), Speed::Full);
    }

    #[test]
    fn a_hub_port_device_goes_only_when_empty_or_changed_and_disabled() {
        for bits in 0..8u32 {
            let connected = bits & 0b001 != 0;
            let connection_changed = bits & 0b010 != 0;
            let enabled = bits & 0b100 != 0;
            assert_eq!(
                hub_port_device_gone(connected, connection_changed, enabled),
                !connected || (connection_changed && !enabled),
                "connected={connected} changed={connection_changed} enabled={enabled}"
            );
        }
    }
}
