//! The ChipIdea USB device controller of the i.MX6: queue heads, transfer descriptors and setup packets.
//!
//! Each endpoint direction has one transfer at a time, in memory laid out at fixed offsets ([`buf`]). That is slower
//! than a queue, as the controller NAKs between transfers, but it needs none of the controller's add-to-a-running-list
//! protocol, and a shell over the network does not need more.

use crate::{DmaMem, Error, Hw};

/// The first register of USB OTG1.
const USB: usize = 0x0218_4000;
const SBUSCFG: usize = USB + 0x090;
const USBCMD: usize = USB + 0x140;
const USBSTS: usize = USB + 0x144;
const USBINTR: usize = USB + 0x148;
const DEVICEADDR: usize = USB + 0x154;
const ENDPTLISTADDR: usize = USB + 0x158;
const BURSTSIZE: usize = USB + 0x160;
const PORTSC: usize = USB + 0x184;
const OTGSC: usize = USB + 0x1a4;
const USBMODE: usize = USB + 0x1a8;
const ENDPTSETUPSTAT: usize = USB + 0x1ac;
const ENDPTPRIME: usize = USB + 0x1b0;
const ENDPTFLUSH: usize = USB + 0x1b4;
const ENDPTSTAT: usize = USB + 0x1b8;
const ENDPTCOMPLETE: usize = USB + 0x1bc;
const ENDPTCTRL0: usize = USB + 0x1c0;

/// OTGSC's interrupt enables, which `USBINTR` does not mask, and its write-1-to-clear statuses.
const OTGSC_INT_ENABLES: u32 = 0x7f << 24;
const OTGSC_INT_STATUS: u32 = 0x7f << 16;

const CMD_RS: u32 = 1 << 0;
const CMD_RST: u32 = 1 << 1;
const CMD_SUTW: u32 = 1 << 13;
const CMD_ITC_MASK: u32 = 0xff << 16;
/// Interrupt threshold: one microframe, Linux's default.
const CMD_ITC_1: u32 = 1 << 16;

const STS_UI: u32 = 1 << 0;
const STS_UEI: u32 = 1 << 1;
const STS_PCI: u32 = 1 << 2;
const STS_URI: u32 = 1 << 6;
const STS_SLI: u32 = 1 << 8;
/// The interrupts this driver takes, and the status bits it clears: all of them are write-1-to-clear.
const INTERRUPTS: u32 = STS_UI | STS_UEI | STS_PCI | STS_URI | STS_SLI;

const PORTSC_HSP: u32 = 1 << 9;
const PORTSC_PHCD: u32 = 1 << 23;
/// PORTSC's write-1-to-clear bits, which a read-modify-write must not send back.
const PORTSC_W1C: u32 = (1 << 1) | (1 << 3) | (1 << 5);

const MODE_CM_MASK: u32 = 3;
const MODE_CM_DEVICE: u32 = 2;
const MODE_SLOM: u32 = 1 << 3;

const EPCTRL_RXS: u32 = 1 << 0;
const EPCTRL_RXT_SHIFT: u32 = 2;
const EPCTRL_RXR: u32 = 1 << 6;
const EPCTRL_RXE: u32 = 1 << 7;
const EPCTRL_TXS: u32 = 1 << 16;
const EPCTRL_TXT_SHIFT: u32 = 18;
const EPCTRL_TXR: u32 = 1 << 22;
const EPCTRL_TXE: u32 = 1 << 23;

const QH_SIZE: usize = 64;
const QH_CAP: usize = 0x00;
const QH_NEXT: usize = 0x08;
const QH_TOKEN: usize = 0x0c;
const QH_SETUP: usize = 0x28;
const CAP_IOS: u32 = 1 << 15;
const CAP_MPS_SHIFT: u32 = 16;
/// The hardware adds no zero-length packets; software does, as [`Dcd::start_transfer`]'s `zlp`.
const CAP_ZLT: u32 = 1 << 29;

const TD_SIZE: usize = 32;
const TD_NEXT: usize = 0x00;
const TD_TOKEN: usize = 0x04;
const TD_PAGES: usize = 0x08;
const TD_TERMINATE: u32 = 1;
const TOKEN_ACTIVE: u32 = 1 << 7;
const TOKEN_HALTED: u32 = 1 << 6;
const TOKEN_ERRORS: u32 = TOKEN_HALTED | (1 << 5) | (1 << 3);
const TOKEN_IOC: u32 = 1 << 15;
const TOKEN_BYTES_SHIFT: u32 = 16;
const TOKEN_BYTES_MASK: u32 = 0x7fff;

/// Where things are in the DMA memory.
pub mod buf {
    /// The queue heads, of which there are 16. The hardware needs them 2 KiB aligned.
    pub const QH: usize = 0;
    /// The transfer descriptors, two per endpoint direction.
    pub const TD: usize = 0x800;
    pub const EP0_IN: usize = 0x1000;
    pub const EP0_OUT: usize = 0x1200;
    pub const EP0_SIZE: usize = 512;
    pub const NOTIFY: usize = 0x1400;
    pub const NOTIFY_SIZE: usize = 64;
    pub const ACM_RX: usize = 0x1500;
    pub const ACM_TX: usize = 0x1700;
    pub const ACM_SIZE: usize = 512;
    pub const ECM_RX: usize = 0x2000;
    pub const ECM_TX: usize = 0x2800;
    pub const ECM_SIZE: usize = 2048;
}

/// The size of the DMA memory that [`Dcd`] uses.
pub const DMA_SIZE: usize = 0x3000;

/// An endpoint and direction.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Ep {
    pub num: u8,
    pub is_in: bool,
}

impl Ep {
    pub const fn new(address: u8) -> Self {
        Self {
            num: address & 0x0f,
            is_in: address & 0x80 != 0,
        }
    }

    const fn index(self) -> usize {
        self.num as usize * 2 + self.is_in as usize
    }

    /// The endpoint's bit in the priming, flushing, status and completion registers.
    pub const fn bit(self) -> u32 {
        (1 << self.num) << if self.is_in { 16 } else { 0 }
    }

    const fn qh(self) -> usize {
        buf::QH + self.index() * QH_SIZE
    }

    const fn td(self, n: usize) -> usize {
        buf::TD + (self.index() * 2 + n) * TD_SIZE
    }
}

pub const EP0_OUT: Ep = Ep::new(0x00);
pub const EP0_IN: Ep = Ep::new(0x80);

/// An endpoint's transfer type, as the control register encodes it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EpType {
    Bulk = 2,
    Interrupt = 3,
}

/// The bus speed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Speed {
    Full,
    High,
}

/// A finished transfer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Done {
    pub ep: Ep,
    /// The bytes transferred.
    pub len: usize,
    /// Whether the controller reported an error.
    pub failed: bool,
}

/// What [`Dcd::poll`] found.
#[derive(Debug, Default)]
pub struct Events {
    pub bus_reset: bool,
    pub speed: Option<Speed>,
    pub suspended: bool,
    /// Bit per [`Ep`] index: the transfers that finished, read with [`Dcd::finish`].
    pub completed: u32,
    pub setup: Option<[u8; 8]>,
}

pub struct Dcd<H, M> {
    pub hw: H,
    pub mem: M,
    /// The length of the running transfer per [`Ep`] index, without the zero-length packet.
    lengths: [usize; 16],
    /// Whether the transfer per [`Ep`] index has a second, zero-length descriptor.
    has_zlp: u16,
}

impl<H: Hw, M: DmaMem> Dcd<H, M> {
    pub fn new(hw: H, mem: M) -> Self {
        Self {
            hw,
            mem,
            lengths: [0; 16],
            has_zlp: 0,
        }
    }

    /// Stops any earlier use, resets the controller, puts it in device mode and connects to the host.
    pub fn start(&mut self) -> Result<(), Error> {
        let portsc = self.hw.read32(PORTSC);
        if portsc & PORTSC_PHCD != 0 {
            self.hw.write32(PORTSC, portsc & !PORTSC_PHCD & !PORTSC_W1C);
            self.hw.delay_us(1000);
        }
        self.hw.write32(USBINTR, 0);
        // An OTG interrupt left on by an earlier system would keep the shared line up.
        let otgsc = self.hw.read32(OTGSC);
        self.hw
            .write32(OTGSC, (otgsc & !OTGSC_INT_ENABLES) | OTGSC_INT_STATUS);
        self.set_cmd(|cmd| cmd & !CMD_RS);
        self.hw.write32(ENDPTFLUSH, u32::MAX);
        self.set_cmd(|cmd| cmd | CMD_RST);
        if !self.poll_register(|cmd| cmd & CMD_RST == 0, USBCMD) {
            return Err(Error::ControllerReset);
        }
        // The mode goes through idle, and it only latches with a running PHY clock.
        self.hw.write32(USBMODE, 0);
        self.hw.write32(USBMODE, MODE_CM_DEVICE);
        self.hw.write32(USBMODE, MODE_CM_DEVICE | MODE_SLOM);
        if self.hw.read32(USBMODE) & MODE_CM_MASK != MODE_CM_DEVICE {
            return Err(Error::NotDeviceMode);
        }
        self.set_cmd(|cmd| (cmd & !CMD_ITC_MASK) | CMD_ITC_1);
        let sbuscfg = self.hw.read32(SBUSCFG);
        self.hw.write32(SBUSCFG, sbuscfg & !7);
        self.hw.write32(BURSTSIZE, 0x1010);

        for ep in [EP0_OUT, EP0_IN] {
            self.reset_qh(ep, (64 << CAP_MPS_SHIFT) | CAP_IOS | CAP_ZLT);
        }
        self.hw.barrier();
        let list = self.mem.paddr() + buf::QH as u32;
        self.hw.write32(ENDPTLISTADDR, list);
        self.hw.write32(USBSTS, u32::MAX);
        self.hw.write32(USBINTR, INTERRUPTS);
        self.set_cmd(|cmd| cmd | CMD_RS);
        Ok(())
    }

    fn hw_read_cmd(&mut self) -> u32 {
        self.hw.read32(USBCMD)
    }

    fn set_cmd(&mut self, update: impl FnOnce(u32) -> u32) {
        let cmd = self.hw.read32(USBCMD);
        self.hw.write32(USBCMD, update(cmd));
    }

    /// Writes a write-1-to-clear register's own value back, which clears every bit that is set.
    fn ack(&mut self, reg: usize) {
        let val = self.hw.read32(reg);
        self.hw.write32(reg, val);
    }

    fn poll_register(&mut self, done: impl Fn(u32) -> bool, reg: usize) -> bool {
        for _ in 0..1000 {
            if done(self.hw.read32(reg)) {
                return true;
            }
            self.hw.delay_us(10);
        }
        false
    }

    fn reset_qh(&mut self, ep: Ep, cap: u32) {
        let qh = ep.qh();
        self.mem.write32(qh + QH_CAP, cap);
        self.mem.write32(qh + QH_NEXT, TD_TERMINATE);
        self.mem.write32(qh + QH_TOKEN, 0);
    }

    /// For the interrupt handler: quiets the level interrupt until [`Dcd::unmask_interrupts`]. Takes the register access
    /// alone, as the handler does not own the driver.
    pub fn mask_interrupts(hw: &mut H) {
        hw.write32(USBINTR, 0);
    }

    /// Lets interrupts through again, after [`Dcd::poll`].
    pub fn unmask_interrupts(&mut self) {
        self.hw.write32(USBINTR, INTERRUPTS);
    }

    /// Reads and acknowledges what has happened.
    pub fn poll(&mut self) -> Events {
        let status = self.hw.read32(USBSTS) & INTERRUPTS;
        self.hw.write32(USBSTS, status);
        let mut events = Events::default();
        if status & STS_URI != 0 {
            self.bus_reset();
            events.bus_reset = true;
        }
        if status & STS_PCI != 0 {
            events.speed = Some(if self.hw.read32(PORTSC) & PORTSC_HSP != 0 {
                Speed::High
            } else {
                Speed::Full
            });
        }
        // Completions are read even without the status bit, as the mask may have hidden it.
        let complete = self.hw.read32(ENDPTCOMPLETE);
        if complete != 0 {
            self.hw.write32(ENDPTCOMPLETE, complete);
            for ep in all_eps() {
                if complete & ep.bit() != 0 {
                    events.completed |= 1 << ep.index();
                }
            }
        }
        let setup = self.hw.read32(ENDPTSETUPSTAT) & 1 != 0;
        // These registers announce what is in DMA memory, and ARMv7 does not order the two kinds of memory.
        self.hw.barrier();
        if setup {
            events.setup = Some(self.read_setup());
        }
        if status & STS_SLI != 0 {
            events.suspended = true;
        }
        events
    }

    /// On a bus reset: address 0, nothing pending. Endpoints other than ep0 go off; the gadget configures them again.
    fn bus_reset(&mut self) {
        self.hw.write32(DEVICEADDR, 0);
        self.ack(ENDPTSETUPSTAT);
        self.ack(ENDPTCOMPLETE);
        self.flush(u32::MAX);
        for num in 1..8 {
            self.hw.write32(ENDPTCTRL0 + num * 4, 0);
        }
        self.lengths = [0; 16];
        self.has_zlp = 0;
    }

    /// Reads a setup packet: the status bit is cleared first, and the tripwire says if a newer packet overwrote this one
    /// during the copy, in which case it is read again.
    fn read_setup(&mut self) -> [u8; 8] {
        self.hw.write32(ENDPTSETUPSTAT, 1);
        let mut setup = [0; 8];
        loop {
            self.set_cmd(|cmd| cmd | CMD_SUTW);
            self.mem.read(EP0_OUT.qh() + QH_SETUP, &mut setup);
            let cmd = self.hw_read_cmd();
            self.hw.write32(USBCMD, cmd & !CMD_SUTW);
            if cmd & CMD_SUTW != 0 {
                return setup;
            }
        }
    }

    /// Stops the transfers of these endpoints (bits as in [`Ep`]).
    pub fn flush(&mut self, bits: u32) {
        for _ in 0..1000 {
            self.hw.write32(ENDPTFLUSH, bits);
            if !self.poll_register(|flush| flush & bits == 0, ENDPTFLUSH) {
                return;
            }
            if self.hw.read32(ENDPTSTAT) & bits == 0 {
                return;
            }
        }
    }

    /// Ends whatever ep0 was doing, for a new setup packet.
    pub fn flush_ep0(&mut self) {
        self.flush(EP0_OUT.bit() | EP0_IN.bit());
    }

    /// Answers a control request with a stall, until the next setup packet.
    pub fn stall_ep0(&mut self) {
        let ctrl = self.hw.read32(ENDPTCTRL0);
        self.hw.write32(ENDPTCTRL0, ctrl | EPCTRL_RXS | EPCTRL_TXS);
    }

    /// Enables an endpoint. The unused direction of its pair is set to the same type, which the controller requires of
    /// a half that is not in use: a control type there breaks the other half.
    pub fn enable_ep(&mut self, ep: Ep, kind: EpType, max_packet: u32) {
        self.flush(ep.bit());
        self.reset_qh(ep, (max_packet << CAP_MPS_SHIFT) | CAP_ZLT);
        self.hw.barrier();
        let reg = ENDPTCTRL0 + ep.num as usize * 4;
        let mut ctrl = self.hw.read32(reg);
        let kind = kind as u32;
        if ep.is_in {
            ctrl &= !(3 << EPCTRL_TXT_SHIFT);
            ctrl |= (kind << EPCTRL_TXT_SHIFT) | EPCTRL_TXR | EPCTRL_TXE;
            if ctrl & EPCTRL_RXE == 0 {
                ctrl = (ctrl & !(3 << EPCTRL_RXT_SHIFT)) | (kind << EPCTRL_RXT_SHIFT);
            }
        } else {
            ctrl &= !(3 << EPCTRL_RXT_SHIFT);
            ctrl |= (kind << EPCTRL_RXT_SHIFT) | EPCTRL_RXR | EPCTRL_RXE;
            if ctrl & EPCTRL_TXE == 0 {
                ctrl = (ctrl & !(3 << EPCTRL_TXT_SHIFT)) | (kind << EPCTRL_TXT_SHIFT);
            }
        }
        self.hw.write32(reg, ctrl);
    }

    /// Disables an endpoint.
    pub fn disable_ep(&mut self, ep: Ep) {
        self.flush(ep.bit());
        let reg = ENDPTCTRL0 + ep.num as usize * 4;
        let ctrl = self.hw.read32(reg);
        let enable = if ep.is_in { EPCTRL_TXE } else { EPCTRL_RXE };
        self.hw.write32(reg, ctrl & !enable);
        self.lengths[ep.index()] = 0;
    }

    /// Stalls an endpoint or clears its stall; clearing also resets its data toggle, as `CLEAR_FEATURE` asks.
    pub fn set_halt(&mut self, ep: Ep, halted: bool) {
        let reg = ENDPTCTRL0 + ep.num as usize * 4;
        let mut ctrl = self.hw.read32(reg);
        let (stall, reset) = if ep.is_in {
            (EPCTRL_TXS, EPCTRL_TXR)
        } else {
            (EPCTRL_RXS, EPCTRL_RXR)
        };
        if halted {
            ctrl |= stall;
        } else {
            ctrl = (ctrl & !stall) | reset;
        }
        self.hw.write32(reg, ctrl);
    }

    /// Whether an endpoint is stalled.
    pub fn is_halted(&mut self, ep: Ep) -> bool {
        let ctrl = self.hw.read32(ENDPTCTRL0 + ep.num as usize * 4);
        ctrl & if ep.is_in { EPCTRL_TXS } else { EPCTRL_RXS } != 0
    }

    fn write_td(&mut self, td: usize, next: u32, token: u32, buf_addr: u32) {
        self.mem.write32(td + TD_NEXT, next);
        self.mem.write32(td + TD_TOKEN, token);
        // A 5-page buffer pointer list: the first keeps the offset in its page, the others do not.
        self.mem.write32(td + TD_PAGES, buf_addr);
        for page in 1..5u32 {
            self.mem.write32(
                td + TD_PAGES + page as usize * 4,
                (buf_addr & !0xfff) + page * 0x1000,
            );
        }
    }

    /// Starts a transfer of `len` bytes at `buf_offset` in the DMA memory, with a zero-length packet after it if `zlp`.
    pub fn start_transfer(&mut self, ep: Ep, buf_offset: usize, len: usize, zlp: bool) {
        let base = self.mem.paddr();
        let buffer = base + buf_offset as u32;
        let token = ((len as u32) << TOKEN_BYTES_SHIFT) | TOKEN_ACTIVE;
        let first = ep.td(0);
        if zlp {
            let second = ep.td(1);
            self.write_td(second, TD_TERMINATE, TOKEN_ACTIVE | TOKEN_IOC, buffer);
            self.write_td(first, base + second as u32, token, buffer);
            self.has_zlp |= 1 << ep.index();
        } else {
            self.write_td(first, TD_TERMINATE, token | TOKEN_IOC, buffer);
            self.has_zlp &= !(1 << ep.index());
        }
        self.lengths[ep.index()] = len;
        let qh = ep.qh();
        self.mem.write32(qh + QH_NEXT, base + first as u32);
        self.mem.write32(qh + QH_TOKEN, 0);
        self.hw.barrier();
        self.hw.write32(ENDPTPRIME, ep.bit());
        // Ep0's OUT priming clears by itself when a setup packet arrives, so this does not wait for ever.
        self.poll_register(|prime| prime & ep.bit() == 0, ENDPTPRIME);
    }

    /// Reads the result of a transfer that [`Events::completed`] named.
    pub fn finish(&mut self, ep: Ep) -> Done {
        let token = self.mem.read32(ep.td(0) + TD_TOKEN);
        let remaining = ((token >> TOKEN_BYTES_SHIFT) & TOKEN_BYTES_MASK) as usize;
        let requested = self.lengths[ep.index()];
        Done {
            ep,
            len: requested.saturating_sub(remaining),
            failed: token & TOKEN_ERRORS != 0,
        }
    }

    /// Sets the device's address, once the status stage of `SET_ADDRESS` is over.
    pub fn set_address(&mut self, address: u8) {
        self.hw.write32(DEVICEADDR, (address as u32) << 25);
    }

    /// Takes the controller off the bus: the host sees an unplug.
    pub fn disconnect(&mut self) {
        self.set_cmd(|cmd| cmd & !CMD_RS);
    }
}

/// Every endpoint and direction, for going through a bit mask.
pub fn all_eps() -> impl Iterator<Item = Ep> {
    (0..16u8).map(|index| Ep {
        num: index / 2,
        is_in: index % 2 == 1,
    })
}

/// An [`Ep`]'s bit in [`Events::completed`].
pub const fn completed_bit(ep: Ep) -> u32 {
    1 << ep.index()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Model;

    #[test]
    fn an_otg_interrupt_left_on_goes_off_and_its_status_is_cleared() {
        const OTHER_BIT: u32 = 1 << 8;
        let mut model = Model::new();
        model.set(OTGSC, OTGSC_INT_ENABLES | OTHER_BIT);
        let mem = model.mem(DMA_SIZE);
        let mut dcd = Dcd::new(model, mem);
        dcd.start().unwrap();
        assert_eq!(dcd.hw.get(OTGSC), OTGSC_INT_STATUS | OTHER_BIT);
    }
}
