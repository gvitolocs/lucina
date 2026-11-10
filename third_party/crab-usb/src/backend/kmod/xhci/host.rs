use alloc::{boxed::Box, sync::Arc};
use core::{
    cell::UnsafeCell,
    sync::atomic::{AtomicBool, AtomicU8, Ordering},
    time::Duration,
};

use ::xhci::{
    extended_capabilities::usb_legacy_support_capability::UsbLegacySupport,
    registers::{
        doorbell,
        runtime::InterrupterManagementRegister,
    },
    ring::trb::{command, event::CommandCompletion},
};
use ax_sync::{RawSpinLockGuard, SpinLock, SpinLockGuard, SpinRwLock as RwLock};
use dma_api::{DeviceDma, DmaDirection};
use futures::{FutureExt, future::BoxFuture};
use mbarrier::mb;
use usb_if::err::{TransferError, USBError};

use super::{
    Device, SlotId,
    cmd::CommandRing,
    context::{DeviceContextList, ScratchpadBufferArray},
    event::{EventRing, EventRingInfo},
    hub::{PortChangeWaker, XhciRootHub},
    reg::{MemMapper, Write64Order, XhciRegisters, scan_extended_capabilities},
    transfer::TransferResultHandler,
};
use crate::{
    DeviceAddressInfo, KernelOp, Mmio,
    backend::{
        kmod::{hub::HubOp, kcore::CoreOp, xhci::reg::SlotBell},
        ty::{ControllerIrqState, DeviceOp, Event, EventHandlerOp},
    },
    err::Result,
    osal::{HandshakeError, Kernel, handshake, narrow_dma_capability, wait_until},
    queue::Finished,
};

// Time limits, as Linux's. Every wait of the bring-up is bounded by one of them.
/// `XHCI_MAX_HALT_USEC` (xhci-ext-caps.h:12): HCH after R/S is cleared (`xhci_halt`) or set (`xhci_start`).
const XHCI_MAX_HALT: Duration = Duration::from_micros(32 * 1000);
/// `XHCI_RESET_LONG_USEC` (xhci.h:152): HCRST and then CNR clear, the timeout `xhci_gen_setup` gives `xhci_reset`.
const XHCI_RESET_LONG: Duration = Duration::from_micros(10 * 1000 * 1000);
/// BIOS handoff: 1 s for BIOS Owned to clear, then the OS takes the controller anyway (pci-quirks.c:1202-1212).
const XHCI_BIOS_HANDOFF: Duration = Duration::from_micros(1000 * 1000);
/// Poll interval of the waits up to 1 s; ours. Linux polls every microsecond (`xhci_handshake`) or every 10 us
/// (BIOS handoff); the limits above count the sleeps, so a longer step keeps the real wait close to the limit.
const POLL_STEP_SHORT: Duration = Duration::from_micros(100);
/// Poll interval of the 10 s waits; ours, for the same reason.
const POLL_STEP_LONG: Duration = Duration::from_millis(1);

// USBSTS bits (xHCI 5.4.2).
const USBSTS_HCH: u32 = 1 << 0;
const USBSTS_HSE: u32 = 1 << 2;
const USBSTS_CNR: u32 = 1 << 11;
const USBSTS_HCE: u32 = 1 << 12;
// USBCMD bits (xHCI 5.4.1).
const USBCMD_HCRST: u32 = 1 << 1;

fn usbsts_raw(reg: &XhciRegisters) -> u32 {
    let sts = reg.operational.usbsts.read_volatile();
    // SAFETY: UsbStatusRegister is repr(transparent) over u32.
    unsafe { core::mem::transmute_copy(&sts) }
}

fn usbcmd_raw(reg: &XhciRegisters) -> u32 {
    let cmd = reg.operational.usbcmd.read_volatile();
    // SAFETY: UsbCommandRegister is repr(transparent) over u32.
    unsafe { core::mem::transmute_copy(&cmd) }
}

// Bits of CRCR and ERSTBA that Linux keeps as read when it writes the pointer (`CMD_RING_RSVD_BITS`,
// `ERST_BASE_RSVDP`).
const CRCR_RSVD_BITS: u64 = 0x3f;
const ERSTBA_RSVD_BITS: u64 = 0x3f;

/// A register handshake of the bring-up (Linux `xhci_handshake`), logged and turned into the crate's error.
async fn reg_handshake(
    kernel: &Kernel,
    what: &str,
    step: Duration,
    limit: Duration,
    read: impl FnMut() -> u32,
    mask: u32,
    value: u32,
) -> Result {
    match handshake(kernel, step, limit, read, mask, value).await {
        Ok(()) => Ok(()),
        Err(HandshakeError::Gone) => {
            error!("xHCI: {what}: registers read all ones, controller gone");
            Err(USBError::TransferError(TransferError::NoDevice))
        }
        Err(HandshakeError::Timeout) => {
            error!("xHCI: {what} did not finish within {limit:?}");
            Err(USBError::Timeout)
        }
    }
}

/// The ERDP value to write: the dequeue pointer, the ERST segment index (DESI) and, when `clear_ehb`, EHB as 1
/// (RW1C: clears it). EHB written as 0 keeps it.
fn erdp_write_value(dequeue: u64, segment_index: u8, clear_ehb: bool) -> u64 {
    (dequeue & !0xf) | u64::from(segment_index & 0x7) | if clear_ehb { 1 << 3 } else { 0 }
}

/// What Linux's `xhci_irq` makes of a USBSTS value, if it is a fault.
fn controller_fault(status: u32) -> Option<&'static str> {
    if status == u32::MAX {
        Some("registers read all ones, host controller gone")
    } else if status & USBSTS_HCE != 0 {
        Some("Host Controller Error")
    } else if status & USBSTS_HSE != 0 {
        Some("Host System Error")
    } else {
        None
    }
}

/// Linux's `xhci_halt` from a context that cannot wait for the executor: clear R/S and the interrupt enables,
/// then poll HCH up to `XHCI_MAX_HALT`. Returns whether HCH was seen: if not (or the registers read all ones),
/// the controller may still be doing DMA.
pub(super) fn halt_now(reg: &mut XhciRegisters, kernel: &Kernel) -> bool {
    reg.operational.usbcmd.update_volatile(|c| {
        c.clear_run_stop();
        c.clear_interrupter_enable();
        c.clear_host_system_error_enable();
        c.clear_enable_wrap_event();
    });
    let mut waited = Duration::ZERO;
    loop {
        let status = usbsts_raw(reg);
        if status == u32::MAX {
            warn!("xHCI: halt after a fault: registers read all ones, HCH not seen");
            return false;
        }
        if status & USBSTS_HCH != 0 {
            return true;
        }
        if waited >= XHCI_MAX_HALT {
            warn!("xHCI: halt after a fault did not finish within {XHCI_MAX_HALT:?}");
            return false;
        }
        kernel.delay(POLL_STEP_SHORT);
        waited += POLL_STEP_SHORT;
    }
}

pub struct Xhci {
    pub(crate) reg: Arc<RwLock<XhciRegisters>>,
    pub(crate) kernel: Kernel,
    pub(crate) cmd: CommandRing,
    dev_ctx: Option<DeviceContextList>,
    event_handler: Option<EventHandler>,
    event_ring_info: EventRingInfo,
    scratchpad_buf_arr: Option<ScratchpadBufferArray>,
    pub(crate) transfer_result_handler: TransferResultHandler,
    root_hub: Option<XhciRootHub>,
    irq_state: ControllerIrqState,
    irq_mask: Arc<XhciIrqMaskState>,
    running: Arc<AtomicBool>,
}

impl CoreOp for Xhci {
    fn root_hub(&mut self) -> Box<dyn HubOp> {
        Box::new(
            self.root_hub
                .take()
                .expect("Root hub can only be taken once"),
        )
    }

    fn prepare_controller<'a>(&'a mut self) -> BoxFuture<'a, core::result::Result<(), USBError>> {
        self._init().boxed()
    }

    fn new_addressed_device<'a>(
        &'a mut self,
        addr: DeviceAddressInfo,
    ) -> BoxFuture<'a, Result<Box<dyn DeviceOp>>> {
        self.new_device(addr).boxed()
    }

    fn create_event_handler(&mut self) -> Box<dyn EventHandlerOp> {
        Box::new(
            self.event_handler
                .take()
                .expect("Event handler can only be created once"),
        )
    }

    fn enable_irq(&mut self) -> Result<()> {
        Self::enable_irq(self);
        Ok(())
    }

    fn disable_irq(&mut self) -> Result<()> {
        Self::disable_irq(self);
        Ok(())
    }

    fn kernel(&self) -> &Kernel {
        &self.kernel
    }
}

impl Xhci {
    pub fn new(mmio: Mmio, dma: DeviceDma, runtime: &'static dyn KernelOp) -> Result<Self> {
        // Default 64-bit write order is Linux's plain xHCI: low word first.
        Self::new_with_write64_order(mmio, dma, runtime, Write64Order::LoHi)
    }

    /// `write64_order` is the order of ERSTBA's two 32-bit halves (see [`Write64Order`]): `HiLo` for a DWC3
    /// core, whose glue in Linux always sets `write-64-hi-lo-quirk`.
    pub fn new_with_write64_order(
        mmio: Mmio,
        dma: DeviceDma,
        runtime: &'static dyn KernelOp,
        write64_order: Write64Order,
    ) -> Result<Self> {
        let mut reg = XhciRegisters::new(mmio);
        reg.set_write64_order(write64_order);

        // 检查 xHCI 控制器的寻址能力（HCCPARAMS1 寄存器）
        let hccparams1 = reg.capability.hccparams1.read_volatile();
        let ac64 = hccparams1.addressing_capability(); // Bit[0]: 64-bit Addressing Capability

        info!(
            "xHCI: Addressing Capability (AC64) = {} ({}-bit addressing)",
            ac64,
            if ac64 { "64" } else { "32" }
        );

        // 根据 AC64 位调整 DMA mask
        let dma_mask = if ac64 {
            u64::MAX as usize
        } else {
            // 控制器只支持 32 位地址，强制限制在 32 位
            u32::MAX as usize
        };

        let kernel = Kernel::new(narrow_dma_capability(&dma, dma_mask as u64), runtime);

        let reg_shared = Arc::new(RwLock::new(reg.clone()));

        let dead = Arc::new(AtomicBool::new(false));
        let cmd = CommandRing::new(
            DmaDirection::Bidirectional,
            &kernel,
            reg_shared.clone(),
            dead.clone(),
        )?;
        let cmd_finished = cmd.finished_handle();
        let halted = cmd.halted_flag();
        let max_event_ring_segments = reg
            .capability
            .hcsparams2
            .read_volatile()
            .event_ring_segment_table_max() as usize;
        let event_ring = EventRing::new(max_event_ring_segments, &kernel)?;
        let event_ring_info = event_ring.info();

        let root_hub = XhciRootHub::new(reg.clone(), &kernel)?;

        let transfer_result_handler = TransferResultHandler::new(reg_shared.clone());
        let ports = root_hub.waker();
        let irq_state = ControllerIrqState::new(false);
        let irq_mask = Arc::new(XhciIrqMaskState::masked());

        let event_kernel = kernel.clone();
        let running = Arc::new(AtomicBool::new(false));
        Ok(Xhci {
            reg: reg_shared,
            kernel,
            cmd,
            dev_ctx: None,
            transfer_result_handler: transfer_result_handler.clone(),
            event_handler: Some(EventHandler::new(
                reg,
                cmd_finished,
                event_ring,
                transfer_result_handler,
                ports,
                irq_state.clone(),
                irq_mask.clone(),
                dead.clone(),
                halted,
                running.clone(),
                event_kernel,
            )),
            root_hub: Some(root_hub),
            event_ring_info,
            scratchpad_buf_arr: None,
            irq_state,
            irq_mask,
            running,
        })
    }

    async fn _init(&mut self) -> Result {
        // Runtime interrupter registers are not writable until reset completes
        // and CNR clears, so mask only the operational global gate here.
        self.reg.write().operational.usbcmd.update_volatile(|r| {
            r.clear_interrupter_enable();
        });
        // 4.2 Host Controller Initialization
        self.init_ext_caps().await?;
        // After Chip Hardware Reset6 wait until the Controller Not Ready (CNR) flag
        // in the USBSTS is ‘0’ before writing any xHC Operational or Runtime
        // registers.
        self.chip_hardware_reset().await?;

        self.disable_irq();

        // Program the Max Device Slots Enabled (MaxSlotsEn) field in the CONFIG
        // register (5.4.7) to enable the device slots that system software is going to
        // use.
        let max_slots = self.setup_max_device_slots();
        self.dev_ctx = Some(DeviceContextList::new(max_slots as _, self.kernel())?);

        // Program the Device Context Base Address Array Pointer (DCBAAP)
        // register (5.4.6) with a 64-bit address pointing to where the Device
        // Context Base Address Array is located.
        self.setup_dcbaap()?;

        // Define the Command Ring Dequeue Pointer by programming the
        // Command Ring Control Register (5.4.5) with a 64-bit address pointing to
        // the starting address of the first TRB of the Command Ring.
        self.set_cmd_ring()?;
        self.init_irq()?;
        self.setup_scratchpads()?;
        // At this point, the host controller is up and running and the Root Hub ports
        // (5.4.8) will begin reporting device connects, etc., and system software may begin
        // enumerating devices. System software may follow the procedures described in
        // section 4.3, to enumerate attached devices.
        self.start();
        mb();

        self.wait_for_running().await?;

        Ok(())
    }

    async fn new_device(&mut self, info: DeviceAddressInfo) -> Result<Box<dyn DeviceOp>> {
        let mut device = Device::new(self).await?;
        if let Err(err) = device.init(self, &info).await {
            // Linux frees the device of a failed attempt (`hub_free_dev` in `hub_port_connect`), and the xHCI
            // driver disables its slot. The slot's endpoint routes go too, or the next attempt, which gets the
            // same slot ID back from Enable Slot, could not register EP0 again.
            device.release_failed().await;
            return Err(err);
        }

        Ok(Box::new(device))
    }

    async fn init_ext_caps(&mut self) -> Result {
        let (mmio_base, hccparams1) = {
            let reg = self.reg.read();
            (reg.mmio_base, reg.capability.hccparams1.read_volatile())
        };
        let caps = scan_extended_capabilities(mmio_base, hccparams1, MemMapper {});
        debug!("Extended capabilities: {:?}", caps.count);

        if let Some(usb_legacy_support) = caps.usb_legacy_support {
            self.legacy_init(usb_legacy_support).await?;
        }

        Ok(())
    }

    /// Linux's `xhci_halt`: clears R/S and the interrupt enables (`xhci_quiesce`), then waits for HCH.
    async fn halt(&mut self) -> Result {
        self.reg.write().operational.usbcmd.update_volatile(|c| {
            c.clear_run_stop();
            c.clear_interrupter_enable();
            c.clear_host_system_error_enable();
            c.clear_enable_wrap_event();
        });
        let reg = self.reg.clone();
        reg_handshake(
            &self.kernel,
            "halt",
            POLL_STEP_SHORT,
            XHCI_MAX_HALT,
            move || usbsts_raw(&reg.read()),
            USBSTS_HCH,
            USBSTS_HCH,
        )
        .await
    }

    /// Linux's `xhci_gen_setup` order: halt, then `xhci_reset` with `XHCI_RESET_LONG_USEC`.
    async fn chip_hardware_reset(&mut self) -> Result {
        debug!("Reset begin ...");
        self.halt().await?;
        debug!("Halted");

        let status = usbsts_raw(&self.reg.read());
        if status == u32::MAX {
            error!("xHCI: registers read all ones, controller gone; not reset");
            return Err(USBError::TransferError(TransferError::NoDevice));
        }
        if status & USBSTS_HCH == 0 {
            // Linux warns and goes on without the reset.
            warn!("xHCI: controller not halted, reset skipped");
            return Ok(());
        }

        self.reg.write().operational.usbcmd.update_volatile(|f| {
            f.set_host_controller_reset();
        });
        debug!("Reset HC");

        let reg = self.reg.clone();
        reg_handshake(
            &self.kernel,
            "reset",
            POLL_STEP_LONG,
            XHCI_RESET_LONG,
            move || usbcmd_raw(&reg.read()),
            USBCMD_HCRST,
            0,
        )
        .await?;

        // No doorbell or operational register other than USBSTS may be written until CNR clears.
        let reg = self.reg.clone();
        reg_handshake(
            &self.kernel,
            "controller ready",
            POLL_STEP_LONG,
            XHCI_RESET_LONG,
            move || usbsts_raw(&reg.read()),
            USBSTS_CNR,
            0,
        )
        .await?;

        debug!("Reset finish");
        Ok(())
    }

    async fn legacy_init(&mut self, mut usb_legacy_support: UsbLegacySupport<MemMapper>) -> Result {
        debug!("legacy init");
        // Linux's handoff (pci-quirks.c `quirk_usb_handoff_xhci`): only if the BIOS owns the controller, ask for
        // it and wait up to 1 s for BIOS Owned to clear; a BIOS that does not let go loses it anyway.
        if usb_legacy_support.usblegsup.read_volatile().hc_bios_owned_semaphore() {
            usb_legacy_support.usblegsup.update_volatile(|r| {
                r.set_hc_os_owned_semaphore();
            });
            let released = wait_until(&self.kernel, POLL_STEP_SHORT, XHCI_BIOS_HANDOFF, || {
                !usb_legacy_support
                    .usblegsup
                    .read_volatile()
                    .hc_bios_owned_semaphore()
            })
            .await;
            if released.is_err() {
                warn!("xHCI: BIOS handoff failed (BIOS bug?), taking the controller");
                usb_legacy_support.usblegsup.update_volatile(|r| {
                    r.clear_hc_bios_owned_semaphore();
                });
            }
        }

        debug!("claimed ownership from BIOS");

        usb_legacy_support.usblegctlsts.update_volatile(|r| {
            r.clear_usb_smi_enable();
            r.clear_smi_on_host_system_error_enable();
            r.clear_smi_on_os_ownership_enable();
            r.clear_smi_on_pci_command_enable();
            r.clear_smi_on_bar_enable();

            r.clear_smi_on_bar();
            r.clear_smi_on_pci_command();
            r.clear_smi_on_os_ownership_change();
        });

        Ok(())
    }

    fn setup_max_device_slots(&mut self) -> u8 {
        let mut regs = self.reg.write();
        let max_slots = regs
            .capability
            .hcsparams1
            .read_volatile()
            .number_of_device_slots();

        regs.operational.config.update_volatile(|r| {
            r.set_max_device_slots_enabled(max_slots);
        });

        debug!("Max device slots: {max_slots}");

        max_slots
    }

    pub(crate) fn dev(&self) -> Result<&DeviceContextList> {
        self.dev_ctx.as_ref().ok_or(USBError::NotInitialized)
    }

    pub(crate) fn dev_mut(&mut self) -> Result<&mut DeviceContextList> {
        self.dev_ctx.as_mut().ok_or(USBError::NotInitialized)
    }

    pub fn disable_irq(&mut self) {
        debug!("Disable interrupts");
        self.irq_state.set_enabled(false, || {
            self.irq_mask.begin_masking();
            let mut regs = self.reg.write();
            let mut primary = regs.interrupter_register_set.interrupter_mut(0);
            primary
                .iman
                .update_volatile(|register| prepare_iman_rearm(register, false));
            let _flushed = primary.iman.read_volatile();
            self.irq_mask.finish_masking();

            regs.operational.usbcmd.update_volatile(|r| {
                r.clear_interrupter_enable();
            });
        });
    }

    pub fn enable_irq(&mut self) {
        debug!("Enable interrupts");
        self.irq_state.set_enabled(true, || {
            let mut regs = self.reg.write();
            regs.operational.usbcmd.update_volatile(|r| {
                r.set_interrupter_enable();
            });
            mb();

            if !self.irq_mask.begin_rearm() {
                return;
            }
            let mut primary = regs.interrupter_register_set.interrupter_mut(0);
            primary
                .iman
                .update_volatile(|register| prepare_iman_rearm(register, true));
            let _flushed = primary.iman.read_volatile();
            if self.irq_mask.finish_rearm() == RearmCompletion::Remask {
                primary
                    .iman
                    .update_volatile(|register| prepare_iman_rearm(register, false));
                let _flushed = primary.iman.read_volatile();
            }
        });
    }

    fn setup_dcbaap(&mut self) -> Result {
        let dcbaa_addr = self.dev()?.bus_addr();
        debug!("DCBAAP: {dcbaa_addr}");
        self.reg.read().write_dcbaap(dcbaa_addr);
        Ok(())
    }

    fn set_cmd_ring(&mut self) -> Result {
        let crcr = self.cmd.bus_addr();
        let cycle = self.cmd.cycle();

        debug!("CRCR: {crcr:?}");
        // As Linux's `xhci_set_cmd_ring_deq`: keep the low six bits read back, then the pointer and RCS.
        let reg = self.reg.read();
        let value = (reg.read_crcr() & CRCR_RSVD_BITS)
            | (crcr.raw() & !CRCR_RSVD_BITS)
            | u64::from(cycle);
        reg.write_crcr(value);

        Ok(())
    }

    fn init_irq(&mut self) -> Result {
        let erstz = self.event_ring_info.erstz;
        let erdp = self.event_ring_info.erdp;
        let erstba = self.event_ring_info.erstba;

        {
            let mut reg = self.reg.write();

            debug!("ERSTZ: {erstz:x}");
            reg.interrupter_register_set
                .interrupter_mut(0)
                .erstsz
                .update_volatile(|r| r.set(erstz as _));
            debug!("ERSTBA: {erstba:X}");
            // Two 32-bit writes in the platform's order; the low six bits are RsvdP and stay as read
            // (xhci-mem.c:2341-2347).
            let erstba = (reg.read_erstba() & ERSTBA_RSVD_BITS) | (erstba & !ERSTBA_RSVD_BITS);
            reg.write_erstba(erstba);

            debug!("ERDP: {erdp:x}");
            // EHB is RW1C and written as 0: a pending handler-busy indication is kept.
            reg.write_erdp(erdp_write_value(erdp, (erdp & 0x7) as u8, false));

            reg.interrupter_register_set
                .interrupter_mut(0)
                .imod
                .update_volatile(|im| {
                    // Linux's default: imod_interval 40000 ns / 250 = 160 (xhci-plat.c:254, xhci.c:349-367). crab-usb had
                    // 0x1F (7.75 us), which only mattered while this stack polled; Juno takes the interrupt since patch 1195.
                    im.set_interrupt_moderation_interval(160);
                    im.set_interrupt_moderation_counter(0);
                });
        }

        {
            debug!("Masking primary interrupter during controller prepare.");
            self.reg
                .write()
                .interrupter_register_set
                .interrupter_mut(0)
                .iman
                .update_volatile(prepare_iman_initialization);
        }

        // Set the HCD state before we enable the irqs. HSEE and EWE are ours, from crab-usb; Linux's
        // `xhci_run_finished` sets EIE only. HSEE raises an interrupt on a host system error (the poll checks
        // USBSTS.HSE itself); EWE adds MFINDEX wrap events, which the event loop ignores.
        self.reg.write().operational.usbcmd.update_volatile(|r| {
            r.set_host_system_error_enable();
            r.set_enable_wrap_event();
        });
        Ok(())
    }

    fn setup_scratchpads(&mut self) -> Result {
        let scratchpad_buf_arr = {
            let buf_count = {
                let count = self
                    .reg
                    .read()
                    .capability
                    .hcsparams2
                    .read_volatile()
                    .max_scratchpad_buffers();
                debug!("Scratch buf count: {count}");
                count
            };
            if buf_count == 0 {
                return Ok(());
            }
            let scratchpad_buf_arr = ScratchpadBufferArray::new(buf_count as _, &self.kernel)?;

            let bus_addr = scratchpad_buf_arr.bus_addr();

            self.dev_mut()?.set_scratchpad_array(bus_addr);

            debug!("Setting up {buf_count} scratchpads, at {bus_addr:#0x}");
            scratchpad_buf_arr
        };

        self.scratchpad_buf_arr = Some(scratchpad_buf_arr);

        Ok(())
    }

    fn start(&mut self) {
        self.reg.write().operational.usbcmd.update_volatile(|r| {
            r.set_run_stop();
        });
        debug!("Start run");
    }

    async fn wait_for_running(&mut self) -> Result {
        // Linux `xhci_start`: HCH clears within XHCI_MAX_HALT_USEC.
        let reg = self.reg.clone();
        reg_handshake(
            &self.kernel,
            "start",
            POLL_STEP_SHORT,
            XHCI_MAX_HALT,
            move || usbsts_raw(&reg.read()),
            USBSTS_HCH,
            0,
        )
        .await?;

        info!("Running");
        self.running.store(true, Ordering::Release);

        // 200 ms is ours, from crab-usb ("otherwise port enable = false"); no Linux path waits here. Linux has
        // the root hub's power-on-to-good delay instead, which the root hub now waits too.
        self.kernel.delay(Duration::from_millis(200));

        self.reg
            .write()
            .doorbell
            .write_volatile_at(0, doorbell::Register::default());
        Ok(())
    }

    pub(crate) fn cmd_request(
        &mut self,
        trb: command::Allowed,
    ) -> impl Future<Output = core::result::Result<CommandCompletion, TransferError>> {
        self.cmd.cmd_request(trb)
    }

    pub(crate) fn is_64bit_ctx(&self) -> bool {
        self.reg
            .read()
            .capability
            .hccparams1
            .read_volatile()
            .context_size()
    }

    pub(crate) fn new_slot_bell(&self, slot: SlotId) -> SlotBell {
        SlotBell::new(slot, self.reg.read().clone())
    }

    pub(crate) async fn device_slot_assignment(
        &mut self,
    ) -> core::result::Result<SlotId, TransferError> {
        // enable slot
        let result = self
            .cmd_request(command::Allowed::EnableSlot(command::EnableSlot::default()))
            .await?;

        let slot_id = result.slot_id();
        trace!("assigned slot id: {slot_id}");
        Ok(slot_id.into())
    }
}

pub struct EventHandler {
    event_reg: UnsafeCell<XhciRegisters>,
    irq_ack_reg: SpinLock<XhciRegisters>,
    irq_rearm_reg: UnsafeCell<XhciRegisters>,
    cmd_finished: Finished<CommandCompletion>,
    event_ring: UnsafeCell<EventRing>,
    transfer_result_handler: TransferResultHandler,
    ports: PortChangeWaker,
    irq_state: ControllerIrqState,
    irq_mask: Arc<XhciIrqMaskState>,
    dead: Arc<AtomicBool>,
    /// The command ring's: set when the halt after a fault saw HCH (`CommandRing::is_halted`).
    halted: Arc<AtomicBool>,
    /// Set once the controller runs; Linux's interrupt handler, whose checks the poll makes, only runs from then
    /// (a Host System Error left by the firmware before the reset is not a fault of ours).
    running: Arc<AtomicBool>,
    kernel: Kernel,
    task_gate: SpinLock<()>,
    event_gate: SpinLock<()>,
}

// SAFETY: `task_gate` serializes every task-context entry. `event_gate`
// additionally protects the event register view and event ring, while the
// rearm register view is used only under `task_gate`. Hard IRQ owns the
// independently mapped acknowledgement view behind `irq_ack_reg`; it touches
// only USBSTS and IMAN. `irq_mask` orders the one shared IMAN hardware field
// across acknowledgement and rearm.
unsafe impl Send for EventHandler {}
unsafe impl Sync for EventHandler {}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
enum XhciIrqMaskPhase {
    Unmasked = 0,
    Masking  = 1,
    Masked   = 2,
    Rearming = 3,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RearmCompletion {
    Unmasked,
    Remask,
}

struct XhciIrqMaskState {
    phase: AtomicU8,
}

fn prepare_iman_rearm(register: &mut InterrupterManagementRegister, enabled: bool) {
    // IP is RW1C. Rearm/disable must write zero so an event that arrived while
    // the source was masked remains pending and is delivered after IE opens.
    register.set_0_interrupt_pending();
    if enabled {
        register.set_interrupt_enable();
    } else {
        register.clear_interrupt_enable();
    }
}

fn prepare_iman_initialization(register: &mut InterrupterManagementRegister) {
    register.set_0_interrupt_pending();
    register.clear_interrupt_enable();
}

impl XhciIrqMaskState {
    #[cfg(test)]
    const fn unmasked() -> Self {
        Self {
            phase: AtomicU8::new(XhciIrqMaskPhase::Unmasked as u8),
        }
    }

    const fn masked() -> Self {
        Self {
            phase: AtomicU8::new(XhciIrqMaskPhase::Masked as u8),
        }
    }

    fn begin_masking(&self) {
        self.phase
            .store(XhciIrqMaskPhase::Masking as u8, Ordering::Release);
    }

    fn finish_masking(&self) {
        self.phase
            .store(XhciIrqMaskPhase::Masked as u8, Ordering::Release);
    }

    fn begin_rearm(&self) -> bool {
        self.phase
            .compare_exchange(
                XhciIrqMaskPhase::Masked as u8,
                XhciIrqMaskPhase::Rearming as u8,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_ok()
    }

    fn cancel_rearm(&self) {
        let _result = self.phase.compare_exchange(
            XhciIrqMaskPhase::Rearming as u8,
            XhciIrqMaskPhase::Masked as u8,
            Ordering::AcqRel,
            Ordering::Acquire,
        );
    }

    fn finish_rearm(&self) -> RearmCompletion {
        if self
            .phase
            .compare_exchange(
                XhciIrqMaskPhase::Rearming as u8,
                XhciIrqMaskPhase::Unmasked as u8,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_ok()
        {
            RearmCompletion::Unmasked
        } else {
            RearmCompletion::Remask
        }
    }

    #[cfg(test)]
    fn phase(&self) -> XhciIrqMaskPhase {
        match self.phase.load(Ordering::Acquire) {
            value if value == XhciIrqMaskPhase::Unmasked as u8 => XhciIrqMaskPhase::Unmasked,
            value if value == XhciIrqMaskPhase::Masking as u8 => XhciIrqMaskPhase::Masking,
            value if value == XhciIrqMaskPhase::Masked as u8 => XhciIrqMaskPhase::Masked,
            value if value == XhciIrqMaskPhase::Rearming as u8 => XhciIrqMaskPhase::Rearming,
            _ => unreachable!("invalid xHCI IRQ mask phase"),
        }
    }
}

impl EventHandler {
    #[allow(clippy::too_many_arguments)]
    fn new(
        reg: XhciRegisters,
        cmd_finished: Finished<CommandCompletion>,
        event_ring: EventRing,
        transfer_result_handler: TransferResultHandler,
        ports: PortChangeWaker,
        irq_state: ControllerIrqState,
        irq_mask: Arc<XhciIrqMaskState>,
        dead: Arc<AtomicBool>,
        halted: Arc<AtomicBool>,
        running: Arc<AtomicBool>,
        kernel: Kernel,
    ) -> Self {
        Self {
            event_reg: UnsafeCell::new(reg.clone()),
            irq_ack_reg: SpinLock::new(reg.clone()),
            irq_rearm_reg: UnsafeCell::new(reg),
            cmd_finished,
            event_ring: UnsafeCell::new(event_ring),
            transfer_result_handler,
            ports,
            irq_state,
            irq_mask,
            dead,
            halted,
            running,
            kernel,
            task_gate: SpinLock::new(()),
            event_gate: SpinLock::new(()),
        }
    }

    /// The checks of Linux's `xhci_irq`, made on every pass because this stack polls: USBSTS reading all ones
    /// (`xhci_hc_died`), Host Controller Error or Host System Error (`xhci_halt`). Any of them halts the
    /// controller and marks it dead: commands and transfers then fail with `NoDevice`, as Linux gives every URB
    /// back with -ESHUTDOWN. Returns whether the controller is dead.
    fn check_controller_fault(&self, _guard: &SpinLockGuard<'_, ()>) -> bool {
        if self.dead.load(Ordering::Acquire) {
            return true;
        }
        if !self.running.load(Ordering::Acquire) {
            return false;
        }
        // SAFETY: `_guard` proves that `event_gate` serializes this register view.
        let reg = unsafe { &mut *self.event_reg.get() };
        let status = usbsts_raw(reg);
        let Some(fault) = controller_fault(status) else {
            return false;
        };
        error!("xHCI: {fault} (USBSTS={status:#010x}), controller dead");
        self.dead.store(true, Ordering::Release);
        // All ones: the controller cannot be told to stop, nor seen to. Its memory stays quarantined.
        if status != u32::MAX && halt_now(reg, &self.kernel) {
            self.halted.store(true, Ordering::Release);
        }
        true
    }

    #[allow(clippy::mut_from_ref)]
    fn event_ring(&self, _guard: &SpinLockGuard<'_, ()>) -> &mut EventRing {
        // SAFETY: the private entry points can obtain this guard only from
        // `register_gate`, which serializes every event-ring access.
        unsafe { &mut *self.event_ring.get() }
    }

    fn update_erdp(&self, guard: &SpinLockGuard<'_, ()>, clear_ehb: bool) {
        let erdp = self.event_ring(guard).erdp();
        let segment_index = self.event_ring(guard).segment_index();
        // SAFETY: `guard` proves that `event_gate` serializes this register
        // view. It accesses only ERDP, disjoint from the hard-IRQ USBSTS/IMAN
        // endpoint.
        let reg = unsafe { &*self.event_reg.get() };
        reg.write_erdp(erdp_write_value(erdp, segment_index, clear_ehb));
    }

    fn clean_event_ring(&self, guard: &SpinLockGuard<'_, ()>) -> Event {
        use xhci::ring::trb::event::Allowed;
        let mut event = Event::Nothing;
        let mut command_events = 0usize;
        let mut port_events = 0usize;
        let mut transfer_events = 0usize;
        let mut other_events = 0usize;
        let mut controller_fault = false;
        let mut event_loop = 0usize;

        while let Some(allowed) = self.event_ring(guard).next() {
            match allowed {
                Allowed::CommandCompletion(c) => {
                    command_events += 1;
                    let addr = c.command_trb_pointer();
                    trace!(
                        "xhci: event command ptr={:#x} slot={} code={:?}",
                        addr,
                        c.slot_id(),
                        c.completion_code()
                    );
                    self.cmd_finished.set_finished(addr.into(), c);
                }
                Allowed::PortStatusChange(st) => {
                    port_events += 1;
                    let port_id = st.port_id();
                    trace!("xhci: event port status change port={}", port_id);
                    self.ports.set_port_changed(port_id);

                    event = Event::PortChange {
                        port: st.port_id() as _,
                    };
                }
                Allowed::TransferEvent(c) => {
                    transfer_events += 1;
                    let slot_id = c.slot_id();
                    let ep_id = c.endpoint_id();
                    let ptr = crate::lucina_tear_transfer_event(ep_id, c.trb_pointer());
                    trace!(
                        "xhci: event transfer slot={} ep={} ptr={:#x} code={:?} len={} \
                         event_data={}",
                        slot_id,
                        ep_id,
                        ptr,
                        c.completion_code(),
                        c.trb_transfer_length(),
                        c.event_data()
                    );

                    // Interrupts synchronize queue state only. Do not call
                    // into OS glue or take manager/file/device locks here; the
                    // waiter that owns the queue will advance the transfer flow.
                    let routed = unsafe {
                        self.transfer_result_handler
                            .set_finished(slot_id, ep_id, ptr.into(), c)
                    };
                    if !routed {
                        controller_fault = true;
                    }
                }
                _ => {
                    other_events += 1;
                    trace!("xhci: event other {:?}", allowed);
                }
            }
            event_loop += 1;
            if event_loop > super::ring::TRBS_PER_SEGMENT / 2 {
                self.update_erdp(guard, false);
                event_loop = 0;
            }
        }
        trace!(
            "xhci: event ring drained command={} port={} transfer={} other={} erdp={:#x}",
            command_events,
            port_events,
            transfer_events,
            other_events,
            self.event_ring(guard).erst_dequeue_pointer()
        );
        if controller_fault {
            event = Event::Stopped;
        } else if matches!(event, Event::Nothing) && transfer_events > 0 {
            event = Event::TransferActivity {
                count: transfer_events,
            };
        }
        event
    }
}

impl EventHandlerOp for EventHandler {
    fn acknowledge_irq(&self) -> bool {
        let Some(mut irq_reg): Option<RawSpinLockGuard<'_, XhciRegisters>> = (unsafe {
            // SAFETY: hard IRQ is the only context which accesses this
            // independently mapped acknowledgement endpoint. Interrupt and
            // preemption exclusion are therefore already owned by the caller.
            self.irq_ack_reg.try_lock_raw()
        }) else {
            // Only another acknowledgement can own this endpoint. That owner
            // will mask and acknowledge the same level-triggered source.
            return false;
        };
        let sts = irq_reg.operational.usbsts.read_volatile();
        let has_event_interrupt = sts.event_interrupt();

        if !has_event_interrupt {
            return false;
        }

        self.irq_mask.begin_masking();
        // Everything set is cleared as `xhci_irq` writes USBSTS back, except a Host System Error: Linux reads its fatal bits
        // before that write, and here `check_controller_fault` in the drain reads it after, so HSE is written as 0 and
        // stays. (A read-modify-write cleared it here, before the drain could see it.)
        irq_reg.operational.usbsts.update_volatile(|r| {
            r.set_0_host_system_error();
            r.clear_event_interrupt();
        });

        // GIC level delivery requires clearing IMAN.IP explicitly, matching
        // Linux xhci_irq() after USBSTS.EINT is acknowledged.
        let mut irq = irq_reg.interrupter_register_set.interrupter_mut(0);
        irq.iman.update_volatile(|r| {
            r.clear_interrupt_enable();
            r.clear_interrupt_pending();
        });
        // Match Linux xhci_disable_interrupter(): IMAN may be posted MMIO, so
        // do not publish Masked until the write has reached the controller.
        let _flushed = irq.iman.read_volatile();
        self.irq_mask.finish_masking();
        true
    }

    fn drain_event(&self) -> Event {
        let _task_guard = self.task_gate.lock();
        let event_guard = self.event_gate.lock();
        if self.check_controller_fault(&event_guard) {
            return Event::Stopped;
        }
        let event = if self.event_ring(&event_guard).has_pending_event() {
            self.clean_event_ring(&event_guard)
        } else {
            Event::Nothing
        };
        self.update_erdp(&event_guard, true);
        event
    }

    fn rearm_irq(&self) {
        let _task_guard = self.task_gate.lock();
        if !self.irq_mask.begin_rearm() {
            return;
        }
        self.irq_state.apply_enabled(|enabled| {
            mb();
            // SAFETY: `_task_guard` serializes rearm calls. IMAN sharing with
            // hard IRQ is governed by `irq_mask`, and each side owns a
            // distinct accessor.
            let mut irq = unsafe { &mut *self.irq_rearm_reg.get() }
                .interrupter_register_set
                .interrupter_mut(0);
            irq.iman
                .update_volatile(|register| prepare_iman_rearm(register, enabled));
            // Match Linux xhci_enable_interrupter()/disable_interrupter().
            // The state transition must not outrun a posted IMAN write.
            let _flushed = irq.iman.read_volatile();

            if !enabled {
                self.irq_mask.cancel_rearm();
            } else if self.irq_mask.finish_rearm() == RearmCompletion::Remask {
                // A new hard acknowledgement won while IMAN was being
                // enabled. Reconcile the hardware with its Masking/Masked
                // publication before releasing task ownership.
                irq.iman
                    .update_volatile(|register| prepare_iman_rearm(register, false));
                let _flushed = irq.iman.read_volatile();
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn acknowledgement_during_rearm_requires_a_final_hardware_mask() {
        let state = XhciIrqMaskState::masked();
        assert!(state.begin_rearm());

        state.begin_masking();
        state.finish_masking();

        assert_eq!(state.finish_rearm(), RearmCompletion::Remask);
        assert_eq!(state.phase(), XhciIrqMaskPhase::Masked);
    }

    #[test]
    fn stale_rearm_cannot_enter_while_acknowledgement_is_masking() {
        let state = XhciIrqMaskState::unmasked();
        state.begin_masking();

        assert!(!state.begin_rearm());
        state.finish_masking();
        assert!(state.begin_rearm());
    }

    #[test]
    fn rearm_write_preserves_a_new_hardware_pending_bit() {
        // SAFETY: InterrupterManagementRegister is repr(transparent) over u32.
        let mut register = unsafe { core::mem::transmute::<u32, InterrupterManagementRegister>(1) };

        prepare_iman_rearm(&mut register, true);

        // SAFETY: InterrupterManagementRegister is repr(transparent) over u32.
        let write_value =
            unsafe { core::mem::transmute::<InterrupterManagementRegister, u32>(register) };
        assert_eq!(
            write_value & 1,
            0,
            "RW1C IP must be written as zero so rearm cannot consume a new event"
        );
    }

    #[test]
    fn initial_interrupter_setup_keeps_source_masked() {
        // SAFETY: InterrupterManagementRegister is repr(transparent) over u32.
        let mut register = unsafe { core::mem::transmute::<u32, InterrupterManagementRegister>(0) };

        prepare_iman_initialization(&mut register);

        // SAFETY: InterrupterManagementRegister is repr(transparent) over u32.
        let write_value =
            unsafe { core::mem::transmute::<InterrupterManagementRegister, u32>(register) };
        assert_eq!(write_value & 0b10, 0, "prepare must leave IMAN.IE masked");
    }

    #[test]
    fn initial_interrupter_setup_preserves_pending_and_ehb() {
        // SAFETY: InterrupterManagementRegister is repr(transparent) over u32.
        let mut iman = unsafe { core::mem::transmute::<u32, InterrupterManagementRegister>(1) };

        prepare_iman_initialization(&mut iman);
        let erdp_write = erdp_write_value(0x1000, 0, false);

        // SAFETY: See the representation argument above.
        let iman_write =
            unsafe { core::mem::transmute::<InterrupterManagementRegister, u32>(iman) };
        assert_eq!(iman_write & 1, 0, "prepare must not acknowledge IMAN.IP");
        assert_eq!(erdp_write & (1 << 3), 0, "prepare must not clear ERDP.EHB");
    }

    #[test]
    fn erdp_value_carries_pointer_segment_and_ehb() {
        assert_eq!(erdp_write_value(0x1234_5670, 1, false), 0x1234_5671);
        assert_eq!(erdp_write_value(0x1234_5670, 1, true), 0x1234_5679);
        assert_eq!(erdp_write_value(0x1234_567f, 9, false), 0x1234_5671, "low bits of the pointer and DESI are masked");
    }

    #[repr(align(4096))]
    struct Mmio([u32; 4096]);

    /// A register file the constructor accepts: CAPLENGTH 0x20, one slot and one port, DBOFF 0x800, RTSOFF 0x400.
    fn fake_controller() -> (alloc::boxed::Box<Mmio>, Xhci) {
        use crate::backend::kmod::xhci::endpoint::tests::{TEST_KERNEL, test_device_dma};
        let mut mmio = alloc::boxed::Box::new(Mmio([0; 4096]));
        mmio.0[0] = 0x20 | (0x0100 << 16);
        mmio.0[1] = 1 | (1 << 24);
        mmio.0[0x14 / 4] = 0x800;
        mmio.0[0x18 / 4] = 0x400;
        let base = core::ptr::NonNull::new(mmio.0.as_mut_ptr().cast::<u8>()).unwrap();
        let xhci = Xhci::new(base, test_device_dma(), &TEST_KERNEL).unwrap();
        (mmio, xhci)
    }

    const USBCMD: usize = 0x20 / 4;
    const USBSTS: usize = (0x20 + 4) / 4;

    #[test]
    fn a_host_system_error_halts_the_controller_and_marks_it_dead() {
        let (mut mmio, mut xhci) = fake_controller();
        let handler = xhci.create_event_handler();
        xhci.running.store(true, Ordering::Release);
        mmio.0[USBCMD] = 1; // R/S
        assert!(!matches!(handler.drain_event(), Event::Stopped));
        assert!(!xhci.cmd.is_dead());

        // HSE, and HCH as the halt would leave it.
        mmio.0[USBSTS] = USBSTS_HSE | USBSTS_HCH;
        assert!(matches!(handler.drain_event(), Event::Stopped));
        assert!(xhci.cmd.is_dead(), "commands and transfers now fail with NoDevice");
        assert!(xhci.cmd.is_halted(), "HCH seen: its memory may be freed");
        assert_eq!(mmio.0[USBCMD] & 1, 0, "R/S cleared, as Linux's xhci_halt");
        assert!(matches!(handler.drain_event(), Event::Stopped));
    }

    #[test]
    fn a_fault_before_the_controller_runs_is_not_taken_for_dead() {
        // A Host System Error the firmware left behind is cleared by the reset; Linux's handler only runs later.
        let (mut mmio, mut xhci) = fake_controller();
        let handler = xhci.create_event_handler();
        mmio.0[USBSTS] = USBSTS_HSE;
        let _ = handler.drain_event();
        assert!(!xhci.cmd.is_dead());
    }

    #[test]
    fn registers_reading_all_ones_mark_the_controller_dead() {
        let (mut mmio, mut xhci) = fake_controller();
        let handler = xhci.create_event_handler();
        xhci.running.store(true, Ordering::Release);
        mmio.0[USBSTS] = u32::MAX;
        assert!(matches!(handler.drain_event(), Event::Stopped));
        assert!(xhci.cmd.is_dead());
        assert!(!xhci.cmd.is_halted(), "never seen to halt: its memory is quarantined");
        assert!(xhci.cmd.may_still_dma());
    }

    #[test]
    fn a_fault_without_hch_leaves_the_controller_dead_but_not_halted() {
        let (mut mmio, mut xhci) = fake_controller();
        let handler = xhci.create_event_handler();
        xhci.running.store(true, Ordering::Release);
        // HCE, and HCH never sets (the fake register file keeps what it was given).
        mmio.0[USBSTS] = USBSTS_HCE;
        assert!(matches!(handler.drain_event(), Event::Stopped));
        assert!(xhci.cmd.is_dead());
        assert!(!xhci.cmd.is_halted());
        assert!(xhci.cmd.may_still_dma());
    }

    #[test]
    fn usbsts_faults_follow_linux_xhci_irq() {
        assert!(controller_fault(u32::MAX).is_some(), "all ones: the host is gone");
        assert!(controller_fault(USBSTS_HCE).is_some());
        assert!(controller_fault(USBSTS_HSE).is_some());
        assert!(controller_fault(USBSTS_HSE | USBSTS_HCH).is_some());
        assert!(controller_fault(0).is_none());
        assert!(controller_fault(USBSTS_HCH | USBSTS_CNR | (1 << 3) | (1 << 4)).is_none());
    }
}
