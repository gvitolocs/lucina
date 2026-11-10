use core::{
    mem::size_of,
    num::NonZeroUsize,
    ops::{Deref, DerefMut},
    ptr::NonNull,
};

use xhci::{
    accessor::{Mapper, single},
    extended_capabilities::usb_legacy_support_capability::UsbLegacySupport,
    registers::{capability::CapabilityParameters1, operational::PortStatusAndControlRegister},
};

use super::SlotId;

#[derive(Debug, Clone, Copy)]
pub struct MemMapper;
impl Mapper for MemMapper {
    unsafe fn map(&mut self, phys_start: usize, _bytes: usize) -> NonZeroUsize {
        unsafe { NonZeroUsize::new_unchecked(phys_start) }
    }
    fn unmap(&mut self, _virt_start: usize, _bytes: usize) {}
}

pub(super) struct ExtendedCapabilities<M>
where
    M: Mapper + Clone,
{
    pub(super) count: usize,
    pub(super) usb_legacy_support: Option<UsbLegacySupport<M>>,
    /// Dwords 0 and 2 of every Supported Protocol capability (xHCI 7.2): major revision and the root ports it
    /// covers. Linux builds its USB2 and USB3 root hubs from them (`xhci_setup_port_arrays`).
    pub(super) supported_protocols: alloc::vec::Vec<(u32, u32)>,
}

/// Scans xHCI extended capabilities using one 32-bit MMIO read per header.
///
/// RK3588's DWC3 wrapper can lock up on widened MMIO transactions. Keeping the volatile value a
/// `u32` prevents aggregate header reads from being lowered to 64-bit or 128-bit loads.
pub(super) fn scan_extended_capabilities<M>(
    mmio_base: usize,
    hccparams1: CapabilityParameters1,
    mapper: M,
) -> ExtendedCapabilities<M>
where
    M: Mapper + Clone,
{
    const USB_LEGACY_SUPPORT_ID: u8 = 1;
    const SUPPORTED_PROTOCOL_ID: u8 = 2;
    const MAX_CAPABILITIES: usize = 256;

    let pointer_dwords = usize::from(hccparams1.xhci_extended_capabilities_pointer());
    if pointer_dwords == 0 {
        return ExtendedCapabilities {
            count: 0,
            usb_legacy_support: None,
            supported_protocols: alloc::vec::Vec::new(),
        };
    }

    let mut address = mmio_base + pointer_dwords * size_of::<u32>();
    let mut count = 0;
    let mut usb_legacy_support = None;
    let mut supported_protocols = alloc::vec::Vec::new();
    for _ in 0..MAX_CAPABILITIES {
        let header = {
            // SAFETY: The xHCI capability pointer and each next offset identify an aligned
            // 32-bit MMIO header. The accessor is dropped before any capability accessor is made.
            let header = unsafe { single::ReadOnly::<u32, M>::new(address, mapper.clone()) };
            header.read_volatile()
        };
        count += 1;

        let capability_id = header as u8;
        if capability_id == USB_LEGACY_SUPPORT_ID && usb_legacy_support.is_none() {
            // SAFETY: The capability ID in the just-read header identifies USB Legacy Support,
            // and the temporary header accessor has already been dropped.
            usb_legacy_support = Some(unsafe { UsbLegacySupport::new(address, mapper.clone()) });
        }
        if capability_id == SUPPORTED_PROTOCOL_ID {
            // SAFETY: a Supported Protocol capability is at least four dwords long (xHCI 7.2), so its third dword
            // is aligned MMIO inside it.
            let dword2 = unsafe {
                single::ReadOnly::<u32, M>::new(address + 2 * size_of::<u32>(), mapper.clone())
            }
            .read_volatile();
            supported_protocols.push((header, dword2));
        }

        let next_dwords = usize::from((header >> 8) as u8);
        if next_dwords == 0 {
            break;
        }
        address += next_dwords * size_of::<u32>();
    }

    ExtendedCapabilities {
        count,
        usb_legacy_support,
        supported_protocols,
    }
}

/// Records, for each root port a Supported Protocol capability covers, its USB major revision (dword 0 bits 31:24;
/// dword 2: Compatible Port Offset, 1-based, in bits 7:0 and Compatible Port Count in bits 15:8, xHCI 7.2).
/// `majors[i]` is root port i + 1. Ports past `majors` are ignored.
pub(super) fn apply_supported_protocol(dword0: u32, dword2: u32, majors: &mut [u8]) {
    let major = (dword0 >> 24) as u8;
    let offset = (dword2 & 0xff) as usize;
    let count = ((dword2 >> 8) & 0xff) as usize;
    if offset == 0 {
        return;
    }
    let ports = majors.len();
    for port in (offset..offset + count).take_while(|&port| port <= ports) {
        majors[port - 1] = major;
    }
}

// PORTSC bit classes, as Linux's xhci-hub.c XHCI_PORT_* masks (xhci-hub.c:395-424).
// Read-only status bits: connect status, over-current active, port speed, device removable.
const XHCI_PORT_RO: u32 = (1 << 0) | (1 << 3) | (0xf << 10) | (1 << 30);
// Read/write-sticky bits: writing 0 clears, writing 1 sets: link state, port power,
// port indicator, wake-on enables.
const XHCI_PORT_RWS: u32 = (0xf << 5) | (1 << 9) | (0x3 << 14) | (0x7 << 25);

/// Port of Linux's `xhci_port_state_to_neutral` (xhci-hub.c:445): given a PORTSC value read
/// from hardware, return the value that may be written back without changing port state.
/// RW1C bits (bit 1 PED and change bits 17-23) and RW1S/RW/RsvdZ bits are dropped, so they can
/// never be written back by accident; writing PED=1 would disable an enabled port.
pub(super) fn port_state_to_neutral(portsc: u32) -> u32 {
    (portsc & XHCI_PORT_RO) | (portsc & XHCI_PORT_RWS)
}

fn portsc_raw(register: &PortStatusAndControlRegister) -> u32 {
    // SAFETY: PortStatusAndControlRegister is repr(transparent) over u32.
    unsafe { core::mem::transmute_copy(register) }
}

fn portsc_from_raw(raw: u32) -> PortStatusAndControlRegister {
    // SAFETY: PortStatusAndControlRegister is repr(transparent) over u32 and accepts every bit
    // pattern.
    unsafe { core::mem::transmute(raw) }
}

/// Accesses only the 32-bit PORTSC register in each xHCI port register set.
///
/// This intentionally avoids volatile access to the aggregate 16-byte port register set, which a
/// compiler may lower to widened MMIO transactions that the RK3588 DWC3 wrapper cannot handle.
pub(super) struct PortStatusRegisters<M>
where
    M: Mapper + Clone,
{
    base: usize,
    len: usize,
    mapper: M,
}

impl<M> PortStatusRegisters<M>
where
    M: Mapper + Clone,
{
    /// # Safety
    ///
    /// `base` must point to `len` valid xHCI port register sets, and no other accessor may
    /// concurrently access those registers.
    pub unsafe fn new(base: usize, len: usize, mapper: M) -> Self {
        Self { base, len, mapper }
    }

    pub fn len(&self) -> usize {
        self.len
    }

    pub fn read_volatile_at(&self, index: usize) -> PortStatusAndControlRegister {
        self.register(index).read_volatile()
    }

    /// The raw PORTSC value of port `index`.
    pub fn read_raw_at(&self, index: usize) -> u32 {
        portsc_raw(&self.read_volatile_at(index))
    }

    /// Writes `port_state_to_neutral(PORTSC) | bits`, the way Linux's xHCI hub code sets a feature (PR, WPR, PP)
    /// or clears a change bit (RW1C: CSC, PRC, WRC, PLC; PED disables the port).
    pub fn write_neutral_or(&mut self, index: usize, bits: u32) {
        self.register(index).update_volatile(|current| {
            *current = portsc_from_raw(port_state_to_neutral(portsc_raw(current)) | bits);
        });
    }

    /// Writes `port_state_to_neutral(PORTSC) & !bits`: clears an RWS bit, e.g. port power.
    pub fn write_neutral_and_not(&mut self, index: usize, bits: u32) {
        self.write_neutral_replace(index, bits, 0);
    }

    /// Writes `(port_state_to_neutral(PORTSC) & !clear) | set` in one write, as Linux's `xhci_set_link_state`
    /// replaces the link state field.
    pub fn write_neutral_replace(&mut self, index: usize, clear: u32, set: u32) {
        self.register(index).update_volatile(|current| {
            *current = portsc_from_raw((port_state_to_neutral(portsc_raw(current)) & !clear) | set);
        });
    }

    fn register(&self, index: usize) -> single::ReadWrite<PortStatusAndControlRegister, M> {
        const PORT_REGISTER_SET_STRIDE: usize = 0x10;

        assert!(index < self.len, "port register index out of range");
        let address = self.base + index * PORT_REGISTER_SET_STRIDE;
        // SAFETY: `new` guarantees that every PORTSC address in this range is valid and uniquely
        // accessed through this wrapper. Each temporary accessor covers only the first 32-bit
        // register in a port register set.
        unsafe { single::ReadWrite::new(address, self.mapper.clone()) }
    }
}

type Registers = xhci::Registers<MemMapper>;
// type RegistersExtList = xhci::extended_capabilities::List<MemMapper>;
// type SupportedProtocol = xhci::extended_capabilities::XhciSupportedProtocol<MemMapper>;
pub(crate) type XhciRegistersShared = alloc::sync::Arc<ax_sync::SpinRwLock<XhciRegisters>>;

/// Order of the two 32-bit halves of ERSTBA, the one 64-bit register whose order Linux makes a platform choice.
///
/// Linux writes every 64-bit xHCI register as two 32-bit writes, low word first (`xhci_write_64` is
/// `lo_hi_writeq`, xhci.h). Only ERSTBA follows the `XHCI_WRITE_64_HI_LO` quirk (xhci-mem.c:2346), which
/// xhci-plat sets from the `write-64-hi-lo-quirk` property (xhci-plat.c:271) and the DWC3 host glue always
/// passes (dwc3/host.c:168). DCBAAP, CRCR and ERDP stay low word first in both cases.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Write64Order {
    /// Low word first, then high word: Linux's plain xHCI, and the default.
    #[default]
    LoHi,
    /// High word first, then low word: `write-64-hi-lo-quirk`, set for every DWC3 core.
    HiLo,
}

/// The two 32-bit halves of a 64-bit register write, in write order, as
/// `(value, byte offset from the register start)`. Pure, so the order is unit-testable.
pub(crate) fn write64_halves(order: Write64Order, value: u64) -> [(u32, usize); 2] {
    let lo = value as u32;
    let hi = (value >> 32) as u32;
    match order {
        Write64Order::LoHi => [(lo, 0), (hi, 4)],
        Write64Order::HiLo => [(hi, 4), (lo, 0)],
    }
}

pub(crate) struct XhciRegisters {
    pub mmio_base: usize,
    reg: Registers,
    write64_order: Write64Order,
}

impl Clone for XhciRegisters {
    fn clone(&self) -> Self {
        Self {
            mmio_base: self.mmio_base,
            reg: self.new_reg(),
            write64_order: self.write64_order,
        }
    }
}

impl XhciRegisters {
    pub fn new(mmio_base: NonNull<u8>) -> Self {
        let mmio_base = mmio_base.as_ptr() as usize;
        let mapper = MemMapper {};
        let reg = unsafe { Registers::new(mmio_base, mapper) };
        Self {
            mmio_base,
            reg,
            write64_order: Write64Order::LoHi,
        }
    }

    /// Sets the order of ERSTBA's two halves (see [`Write64Order`]); the platform glue chooses it.
    pub fn set_write64_order(&mut self, order: Write64Order) {
        self.write64_order = order;
    }

    /// Writes a 64-bit register at `offset` from the MMIO base as two 32-bit volatile writes in `order`, as
    /// Linux's `lo_hi_writeq` / `hi_lo_writeq`.
    fn write_64_at(&self, offset: usize, value: u64, order: Write64Order) {
        for (half, half_offset) in write64_halves(order, value) {
            // SAFETY: `offset` comes from the xHCI register layout (CAPLENGTH or RTSOFF plus the register's
            // fixed offset) and points at an aligned 64-bit register inside the mapped MMIO.
            unsafe {
                core::ptr::write_volatile((self.mmio_base + offset + half_offset) as *mut u32, half)
            }
        }
    }

    fn operational_offset(&self) -> usize {
        usize::from(self.capability.caplength.read_volatile().get())
    }

    fn interrupter0_offset(&self) -> usize {
        // RTSOFF bits 31:5; interrupter 0 starts 0x20 into the runtime registers (xHCI 5.5).
        (self.capability.rtsoff.read_volatile().get() as usize & !0x1f) + 0x20
    }

    /// DCBAAP (operational + 0x30), low word first as Linux's `xhci_write_64`.
    pub fn write_dcbaap(&self, value: u64) {
        self.write_64_at(self.operational_offset() + 0x30, value, Write64Order::LoHi);
    }

    /// CRCR (operational + 0x18), low word first as Linux's `xhci_write_64`. The low word carries RCS, CS, CA.
    pub fn write_crcr(&self, value: u64) {
        self.write_64_at(self.operational_offset() + 0x18, value, Write64Order::LoHi);
    }

    /// Reads a 64-bit register at `offset` from the MMIO base as two 32-bit reads, low word first (Linux
    /// `xhci_read_64` is `lo_hi_readq`).
    fn read_64_at(&self, offset: usize) -> u64 {
        let base = self.mmio_base + offset;
        // SAFETY: as in `write_64_at`.
        let lo = unsafe { core::ptr::read_volatile(base as *const u32) };
        // SAFETY: as above, the high half of the same register.
        let hi = unsafe { core::ptr::read_volatile((base + 4) as *const u32) };
        (u64::from(hi) << 32) | u64::from(lo)
    }

    /// CRCR as two 32-bit reads. Only CRR (bit 3) and the RsvdP bits read back; the pointer reads 0.
    pub fn read_crcr(&self) -> u64 {
        self.read_64_at(self.operational_offset() + 0x18)
    }

    /// ERSTBA of interrupter 0 as two 32-bit reads.
    pub fn read_erstba(&self) -> u64 {
        self.read_64_at(self.interrupter0_offset() + 0x10)
    }

    /// ERSTBA of interrupter 0 (runtime + 0x20 + 0x10), in the platform's order (the one quirk Linux has here).
    pub fn write_erstba(&self, value: u64) {
        self.write_64_at(self.interrupter0_offset() + 0x10, value, self.write64_order);
    }

    /// ERDP of interrupter 0 (runtime + 0x20 + 0x18), low word first as Linux's `xhci_write_64`.
    pub fn write_erdp(&self, value: u64) {
        self.write_64_at(self.interrupter0_offset() + 0x18, value, Write64Order::LoHi);
    }

    fn new_reg(&self) -> Registers {
        let mapper = MemMapper {};
        unsafe { Registers::new(self.mmio_base, mapper) }
    }

    pub(super) fn port_status_registers(&self) -> PortStatusRegisters<MemMapper> {
        let len = self.port_register_set.len();
        let base =
            self.mmio_base + usize::from(self.capability.caplength.read_volatile().get()) + 0x400;
        // SAFETY: The xHCI capability registers provide the base and number of port register
        // sets. Callers use this view only for each set's PORTSC dword.
        unsafe { PortStatusRegisters::new(base, len, MemMapper) }
    }

    pub fn disable_irq_guard(&mut self) -> DisableIrqGuard {
        let mut enable = true;
        self.operational.usbcmd.update_volatile(|r| {
            enable = r.interrupter_enable();
            r.clear_interrupter_enable();
        });
        DisableIrqGuard {
            reg: self.new_reg(),
            enable,
        }
    }
}

impl Deref for XhciRegisters {
    type Target = Registers;

    fn deref(&self) -> &Self::Target {
        &self.reg
    }
}

impl DerefMut for XhciRegisters {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.reg
    }
}

pub struct DisableIrqGuard {
    reg: Registers,
    enable: bool,
}
impl Drop for DisableIrqGuard {
    fn drop(&mut self) {
        if self.enable {
            self.reg.operational.usbcmd.update_volatile(|r| {
                r.set_interrupter_enable();
            });
        }
    }
}

pub struct SlotBell {
    slot_id: SlotId,
    reg: XhciRegisters,
}

impl SlotBell {
    pub fn new(slot_id: SlotId, reg: XhciRegisters) -> Self {
        Self { slot_id, reg }
    }

    pub fn ring(&mut self, bell: xhci::registers::doorbell::Register) {
        self.reg
            .doorbell
            .write_volatile_at(self.slot_id.as_usize(), bell);
    }
}

#[cfg(test)]
mod tests {
    extern crate std;

    use core::{mem::size_of, num::NonZeroUsize};
    use std::{cell::RefCell, rc::Rc, vec::Vec};

    use super::*;

    #[test]
    fn xhci_extended_capability_scan_uses_dword_mmio_only() {
        const EXT_CAP_POINTER_DWORDS: u32 = 1;
        const USB_LEGACY_SUPPORT_ID: u32 = 1;
        const SUPPORTED_PROTOCOL_ID: u32 = 2;
        const NEXT_CAPABILITY_DWORDS: u32 = 4;

        let mut registers = [0_u32; 9];
        registers[EXT_CAP_POINTER_DWORDS as usize] =
            USB_LEGACY_SUPPORT_ID | (NEXT_CAPABILITY_DWORDS << 8);
        registers[(EXT_CAP_POINTER_DWORDS + NEXT_CAPABILITY_DWORDS) as usize] =
            SUPPORTED_PROTOCOL_ID;

        let mapped_widths = Rc::new(RefCell::new(Vec::new()));
        let mapper = RecordingMapper {
            mapped_widths: mapped_widths.clone(),
        };
        let hccparams1 = capability_parameters_with_pointer(EXT_CAP_POINTER_DWORDS as u16);

        let capabilities =
            scan_extended_capabilities(registers.as_mut_ptr() as usize, hccparams1, mapper);

        assert_eq!(capabilities.count, 2);
        assert!(capabilities.usb_legacy_support.is_some());
        assert!(
            mapped_widths
                .borrow()
                .iter()
                .all(|&bytes| bytes == size_of::<u32>()),
            "xHCI extended capability scan used non-dword MMIO accesses: {:?}",
            mapped_widths.borrow()
        );
    }

    #[test]
    fn xhci_port_status_accesses_use_dword_mmio_only() {
        let mut registers = [0_u32; 8];
        registers[4] = 1;
        let mapped_widths = Rc::new(RefCell::new(Vec::new()));
        let mapper = RecordingMapper {
            mapped_widths: mapped_widths.clone(),
        };
        let mut portsc =
            unsafe { PortStatusRegisters::new(registers.as_mut_ptr() as usize, 2, mapper) };

        portsc.write_neutral_or(0, 1 << 9);
        let second_port = portsc.read_volatile_at(1);

        assert_eq!(registers[0], 1 << 9);
        assert!(second_port.current_connect_status());
        assert!(
            mapped_widths
                .borrow()
                .iter()
                .all(|&bytes| bytes == size_of::<u32>()),
            "xHCI PORTSC used non-dword MMIO accesses: {:?}",
            mapped_widths.borrow()
        );
    }

    #[derive(Clone)]
    struct RecordingMapper {
        mapped_widths: Rc<RefCell<Vec<usize>>>,
    }

    impl Mapper for RecordingMapper {
        unsafe fn map(&mut self, phys_start: usize, bytes: usize) -> NonZeroUsize {
            self.mapped_widths.borrow_mut().push(bytes);
            NonZeroUsize::new(phys_start).expect("test MMIO address must be non-zero")
        }

        fn unmap(&mut self, _virt_start: usize, _bytes: usize) {}
    }

    fn capability_parameters_with_pointer(pointer_dwords: u16) -> CapabilityParameters1 {
        let raw = u32::from(pointer_dwords) << 16;
        // SAFETY: CapabilityParameters1 is repr(transparent) over u32 and accepts every bit pattern.
        unsafe { core::mem::transmute::<u32, CapabilityParameters1>(raw) }
    }

    #[test]
    fn port_state_to_neutral_never_keeps_rw1c_or_ped() {
        // Every single-bit input: the neutral value must never contain bit 1 (PED, RW1C) or any change bit 17-23
        // (RW1C), nor RW1S bit 4 (PR) and 31 (WPR), RW bit 16, or RsvdZ bits 2, 24, 28, 29. Bit 30 (Device
        // Removable) is read-only and kept, as in Linux's XHCI_PORT_RO.
        for bit in 0..32 {
            let neutral = port_state_to_neutral(1 << bit);
            assert_eq!(
                neutral & (1 << 1),
                0,
                "PED (bit 1) must never survive neutralization, bit {bit}"
            );
            assert_eq!(
                neutral & (0x7f << 17),
                0,
                "RW1C change bits 17-23 must never survive, bit {bit}"
            );
            assert_eq!(
                neutral & (1 << 4),
                0,
                "RW1S bit 4 must be dropped, bit {bit}"
            );
            assert_eq!(
                neutral & (1 << 16),
                0,
                "RW bit 16 must be dropped, bit {bit}"
            );
            assert_eq!(
                neutral & (1 << 2),
                0,
                "RsvdZ bit 2 must be dropped, bit {bit}"
            );
            assert_eq!(
                neutral & (1 << 24),
                0,
                "RsvdZ bit 24 must be dropped, bit {bit}"
            );
            assert_eq!(
                neutral & (0xb << 28),
                0,
                "bits 28, 29 and 31 (WPR) must be dropped, bit {bit}"
            );
        }
    }

    #[test]
    fn port_state_to_neutral_keeps_ro_and_rws_bits() {
        // RO: bits 0, 3, 10-13, 30. RWS: bits 5-9, 14-15, 25-27.
        let ro_rws = (1 << 0)
            | (1 << 3)
            | (0xf << 10)
            | (1 << 30)
            | (0xf << 5)
            | (1 << 9)
            | (0x3 << 14)
            | (0x7 << 25);
        assert_eq!(port_state_to_neutral(ro_rws), ro_rws);
        // A live enabled port (PED=1) with change bits set keeps its state bits but drops
        // PED and the change bits, so writing it back cannot disable the port.
        let live = ro_rws | (1 << 1) | (0x7f << 17) | (1 << 4);
        assert_eq!(port_state_to_neutral(live), ro_rws);
    }

    #[test]
    fn portsc_writes_back_only_neutral_bits_plus_the_bit_being_set() {
        let mut registers = [0u32; 8];
        // Simulate a live port: CCS, PED, PP, speed 3, PRC and CSC set.
        registers[0] = (1 << 0) | (1 << 1) | (1 << 9) | (3 << 10) | (1 << 17) | (1 << 21);
        let mut portsc =
            unsafe { PortStatusRegisters::new(registers.as_mut_ptr() as usize, 1, MemMapper) };

        portsc.write_neutral_or(0, 1 << 4);

        let written = registers[0];
        assert_eq!(written & (1 << 1), 0, "PED must be written as 0");
        assert_eq!(
            written & (0x7f << 17),
            0,
            "change bits must be written as 0"
        );
        assert_ne!(written & (1 << 4), 0, "PR is RW1S and is the bit being set");
        assert_ne!(written & (1 << 9), 0, "PP (RWS) is kept");
        // RO bits are kept in the written value; hardware ignores writes to them.
        assert_ne!(
            written & (1 << 0),
            0,
            "CCS (RO) is kept; writes to RO bits are ignored"
        );
    }

    #[test]
    fn supported_protocols_map_root_ports_to_usb2_and_usb3() {
        let usb2 = (0x02 << 24) | 2;
        let usb3 = (0x03 << 24) | 2;
        let mut majors = [0u8; 3];
        apply_supported_protocol(usb2, 1 | (2 << 8), &mut majors);
        apply_supported_protocol(usb3, 3 | (1 << 8), &mut majors);
        assert_eq!(majors, [2, 2, 3]);

        let mut majors = [0u8; 4];
        apply_supported_protocol(usb3, 3 | (5 << 8), &mut majors);
        assert_eq!(majors, [0, 0, 3, 3], "ports past MaxPorts are ignored");
        apply_supported_protocol(usb2, 2 << 8, &mut majors);
        apply_supported_protocol(usb2, 1, &mut majors);
        apply_supported_protocol(usb2, 255 | (255 << 8), &mut majors);
        assert_eq!(
            majors,
            [0, 0, 3, 3],
            "offset 0, count 0 or out of range change nothing"
        );
        let mut majors = [0u8; 1];
        apply_supported_protocol(0x0310_0802, 1 | (1 << 8), &mut majors);
        assert_eq!(majors, [3]);
    }

    #[test]
    fn the_64_bit_registers_are_at_their_xhci_offsets() {
        // A register file: CAPLENGTH 0x20, RTSOFF 0x100, DBOFF 0x200, one port. It is page aligned like real MMIO:
        // the xhci crate asserts that the interrupter registers (RTSOFF + 0x20) are 32-byte aligned, and a plain
        // `Vec<u32>` is only 4-byte aligned, so the test passed or failed with the allocator's placement.
        #[repr(C, align(4096))]
        struct RegisterFile([u32; 0x400 / 4]);
        let mut file = alloc::boxed::Box::new(RegisterFile([0; 0x400 / 4]));
        let mmio = &mut file.0;
        mmio[0] = 0x20 | (0x0100 << 16);
        mmio[1] = 1 | (1 << 24); // HCSPARAMS1: 1 slot, 1 interrupter... MaxPorts 1
        mmio[0x14 / 4] = 0x200;
        mmio[0x18 / 4] = 0x100;
        let base = NonNull::new(mmio.as_mut_ptr().cast::<u8>()).unwrap();
        let mut regs = XhciRegisters::new(base);

        regs.write_dcbaap(0x1111_2222_3333_4440);
        regs.write_crcr(0x5555_6666_7777_8881);
        regs.write_erstba(0x9999_aaaa_bbbb_cc00);
        regs.write_erdp(0xdddd_eeee_ffff_0008);
        assert_eq!(
            &mmio[(0x20 + 0x30) / 4..][..2],
            &[0x3333_4440, 0x1111_2222],
            "DCBAAP"
        );
        assert_eq!(
            &mmio[(0x20 + 0x18) / 4..][..2],
            &[0x7777_8881, 0x5555_6666],
            "CRCR"
        );
        assert_eq!(mmio[(0x20 + 0x38) / 4], 0, "CONFIG is not touched");
        assert_eq!(
            &mmio[(0x100 + 0x30) / 4..][..2],
            &[0xbbbb_cc00, 0x9999_aaaa],
            "ERSTBA"
        );
        assert_eq!(
            &mmio[(0x100 + 0x38) / 4..][..2],
            &[0xffff_0008, 0xdddd_eeee],
            "ERDP"
        );
        assert_eq!(regs.read_crcr(), 0x5555_6666_7777_8881);

        regs.set_write64_order(Write64Order::HiLo);
        regs.write_erstba(0x0123_4567_89ab_cd00);
        assert_eq!(regs.read_erstba(), 0x0123_4567_89ab_cd00);
    }

    #[test]
    fn write64_halves_order_follows_the_platform_parameter() {
        let value = 0x1122_3344_5566_7788u64;
        assert_eq!(
            write64_halves(Write64Order::LoHi, value),
            [(0x5566_7788, 0), (0x1122_3344, 4)],
            "Linux plain xHCI writes the low word first"
        );
        assert_eq!(
            write64_halves(Write64Order::HiLo, value),
            [(0x1122_3344, 4), (0x5566_7788, 0)],
            "the DWC3 write-64-hi-lo-quirk writes the high word first"
        );
    }
}
