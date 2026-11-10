// SPDX-License-Identifier: MPL-2.0

//! The SPMI arbiter's interrupts, version 5: Linux's `drivers/spmi/spmi-pmic-arb.c` (7.1) interrupt chip (`qpnpint_irq_*`) and
//! its chained handler (`pmic_arb_chained_irq`, `periph_interrupt`). The register work only; the kernel maps the blocks and
//! implements [`Periph`] and [`Arbiter`].
//!
//! A peripheral's interrupt registers (`:600-607`) are written through this EE's write channel of its APID, and only where
//! [`crate::allow`] says so. The arbiter's per-APID registers live in that APID's page of the "chnls" block: ACC_ENABLE
//! (`:1624-1628`), IRQ_STATUS (`:1658-1662`), IRQ_CLEAR (`:1692-1696`); this EE's accumulated status words are in the "intr"
//! block (`:1600-1603`).

use crate::{Error, allow, backlight::WriteChannel};

/// The qpnpint registers every peripheral has (`QPNPINT_REG_*`, `:600-607`).
pub const RT_STS: u16 = 0x10;
pub const SET_TYPE: u16 = 0x11;
pub const POLARITY_HIGH: u16 = 0x12;
pub const POLARITY_LOW: u16 = 0x13;
pub const LATCHED_CLR: u16 = 0x14;
pub const EN_SET: u16 = 0x15;
pub const EN_CLR: u16 = 0x16;
pub const LATCHED_STS: u16 = 0x18;

/// In an APID's page of "chnls": `SPMI_PIC_ACC_ENABLE` (v5 `acc_enable` `:1624-1628`; bit 0, `SPMI_PIC_ACC_ENABLE_BIT` `:117`),
/// `irq_status` (`:1658-1662`), `irq_clear` (`:1692-1696`).
pub const ACC_ENABLE: usize = 0x100;
pub const ACC_ENABLE_BIT: u32 = 1 << 0;
pub const IRQ_STATUS: usize = 0x104;
pub const IRQ_CLEAR: usize = 0x108;

/// The APID map entry's IRQ-owner flag, BIT(24) (`:57`), read in `_pmic_arb_read_apid_map`
/// `:1230-1236`).
pub const IRQ_OWNER: u32 = 1 << 24;

/// This EE's accumulated status word `word` (APIDs 32*word .. 32*word+31) in the "intr" block (`pmic_arb_owner_acc_status_v5`,
/// `:1600-1603`).
pub fn owner_acc_status_offset(ee: u32, word: u32) -> usize {
    0x10000 * ee as usize + 4 * word as usize
}

/// The EE that gets an APID's interrupts: its owner when the map entry has the IRQ-owner flag, none otherwise (`:1230-1236`).
pub fn irq_ee(map_entry: u32, write_ee: u32) -> Option<u32> {
    (map_entry & IRQ_OWNER != 0).then_some(write_ee)
}

/// One peripheral's registers by full address (peripheral << 8 | register) on its SID: reads through the observer channel,
/// writes through this EE's write channel of that APID.
pub trait Periph {
    fn read(&mut self, addr: u16, out: &mut [u8]) -> Result<(), Error>;
    fn write(&mut self, addr: u16, data: &[u8]) -> Result<(), Error>;
}

/// The arbiter's interrupt registers: an APID's page in "chnls" ([`ACC_ENABLE`], [`IRQ_STATUS`], [`IRQ_CLEAR`]) and this EE's
/// accumulated status words in "intr".
pub trait Arbiter {
    fn apid_read(&mut self, apid: u32, offset: usize) -> u32;
    fn apid_write(&mut self, apid: u32, offset: usize, value: u32);
    fn owner_acc_status(&mut self, word: u32) -> u32;
}

const CHANNEL_CMD: usize = 0x00;
const CHANNEL_WDATA0: usize = 0x10;
const OP_EXT_WRITEL: u32 = 0;

/// Writes 1 to 4 bytes at `addr` of peripheral `ppid` through its write channel: `pmic_arb_write_cmd` with v5's command
/// (`pmic_arb_fmt_cmd_v2`, `:1576-1579`: opcode << 27 | address low byte << 4 | byte count - 1), the bytes in WDATA0, little
/// endian. Refused before the channel is touched unless [`allow::allowed`].
pub fn write<C: WriteChannel>(channel: &mut C, ppid: u16, addr: u16, data: &[u8]) -> Result<(), Error> {
    if !allow::allowed(ppid, addr, data.len()) {
        return Err(Error::NotAllowed);
    }
    let mut word = [0u8; 4];
    word[..data.len()].copy_from_slice(data);
    channel.write(CHANNEL_WDATA0, u32::from_le_bytes(word));
    channel.write(CHANNEL_CMD, OP_EXT_WRITEL << 27 | (u32::from(addr) & 0xff) << 4 | (data.len() as u32 - 1));
    crate::wait_done(channel)
}

/// An interrupt's trigger, as Linux's `IRQ_TYPE_*`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Trigger {
    EdgeRising,
    EdgeFalling,
    EdgeBoth,
    LevelHigh,
    LevelLow,
}

/// `qpnpint_irq_set_type` (`:818-856`): the new {SET_TYPE, POLARITY_HIGH, POLARITY_LOW} from the old, only bit `irq` changed. An
/// edge sets the type bit and the polarity of each edge; a level clears the type bit and sets one polarity.
pub fn type_bytes(old: [u8; 3], irq: u8, t: Trigger) -> [u8; 3] {
    let bit = 1u8 << irq;
    let (ty, high, low) = match t {
        Trigger::EdgeRising => (bit, bit, 0),
        Trigger::EdgeFalling => (bit, 0, bit),
        Trigger::EdgeBoth => (bit, bit, bit),
        Trigger::LevelHigh => (0, bit, 0),
        Trigger::LevelLow => (0, 0, bit),
    };
    // `pmic_arb_masked_write` (`:588-589`): (old & !mask) | (new & mask), with the mask `bit` in all three.
    [(old[0] & !bit) | ty, (old[1] & !bit) | high, (old[2] & !bit) | low]
}

/// `qpnpint_irq_domain_activate` (`:881-904`): the interrupt masked and its latch cleared, before anything else. `per` is the
/// peripheral's base address (e.g. 0x0800).
pub fn activate<P: Periph>(p: &mut P, per: u16, irq: u8) -> Result<(), Error> {
    let bit = 1u8 << irq;
    p.write(per + EN_CLR, &[bit])?;
    p.write(per + LATCHED_CLR, &[bit])
}

/// `qpnpint_irq_set_type`: read the three type registers, write them back with bit `irq` set for `t`, as one masked write.
pub fn set_type<P: Periph>(p: &mut P, per: u16, irq: u8, t: Trigger) -> Result<(), Error> {
    let mut old = [0u8; 3];
    p.read(per + SET_TYPE, &mut old)?;
    p.write(per + SET_TYPE, &type_bytes(old, irq, t))
}

/// `qpnpint_irq_unmask` (`:793-816`): ACC_ENABLE on in the arbiter, then, if the peripheral's EN_SET does not have the bit yet,
/// LATCHED_CLR and EN_SET together (two bytes from 0x14).
pub fn unmask<P: Periph, A: Arbiter>(p: &mut P, a: &mut A, apid: u32, per: u16, irq: u8) -> Result<(), Error> {
    let bit = 1u8 << irq;
    a.apid_write(apid, ACC_ENABLE, ACC_ENABLE_BIT);
    let mut en = [0u8; 1];
    p.read(per + EN_SET, &mut en)?;
    if en[0] & bit == 0 {
        p.write(per + LATCHED_CLR, &[bit, bit])?;
    }
    Ok(())
}

/// `qpnpint_irq_mask` (`:785-791`): EN_CLR.
pub fn mask<P: Periph>(p: &mut P, per: u16, irq: u8) -> Result<(), Error> {
    p.write(per + EN_CLR, &[1u8 << irq])
}

/// `qpnpint_irq_ack` (`:771-783`): IRQ_CLEAR in the arbiter, then LATCHED_CLR in the peripheral. Writes only, so it can run
/// where a read through the observer channel cannot.
pub fn ack<P: Periph, A: Arbiter>(p: &mut P, a: &mut A, apid: u32, per: u16, irq: u8) -> Result<(), Error> {
    let bit = 1u8 << irq;
    a.apid_write(apid, IRQ_CLEAR, u32::from(bit));
    p.write(per + LATCHED_CLR, &[bit])
}

/// The peripheral's real-time status (`RT_STS`).
pub fn rt_status<P: Periph>(p: &mut P, per: u16) -> Result<u8, Error> {
    let mut status = [0u8; 1];
    p.read(per + RT_STS, &mut status)?;
    Ok(status[0])
}

/// `pmic_arb_chained_irq` (`:698-769`) with `periph_interrupt` (`:672-696`): every APID with a bit in this EE's accumulated status
/// words, from `first_apid` to `last_apid`, whose ACC_ENABLE is on and whose IRQ_STATUS is not zero, goes to `f(apid, status)`.
/// When no word had a bit, each APID of `fallback` (the ones whose interrupts this EE owns) is read the same way (`:742-763`).
/// Nothing is cleared here: that is [`ack`], or [`clear_unknown`]. Returns how many calls `f` got.
pub fn pending<A: Arbiter>(a: &mut A, first_apid: u32, last_apid: u32, fallback: &[u32], mut f: impl FnMut(u32, u32)) -> u32 {
    let mut calls = 0;
    let mut any = false;
    for word in first_apid >> 5..=last_apid >> 5 {
        let mut status = a.owner_acc_status(word);
        while status != 0 {
            any = true;
            let bit = status.trailing_zeros();
            status &= !(1 << bit);
            let apid = 32 * word + bit;
            if apid < first_apid || apid > last_apid {
                continue;
            }
            calls += interrupt_of(a, apid, &mut f);
        }
    }
    if !any {
        for &apid in fallback {
            calls += interrupt_of(a, apid, &mut f);
        }
    }
    calls
}

/// `periph_interrupt` for one APID: skipped unless ACC_ENABLE is on; `f` gets IRQ_STATUS when it is not zero.
fn interrupt_of<A: Arbiter>(a: &mut A, apid: u32, f: &mut impl FnMut(u32, u32)) -> u32 {
    if a.apid_read(apid, ACC_ENABLE) & ACC_ENABLE_BIT == 0 {
        return 0;
    }
    let status = a.apid_read(apid, IRQ_STATUS);
    if status == 0 {
        return 0;
    }
    f(apid, status);
    1
}

/// A status nobody asked for: IRQ_CLEAR in the arbiter. Linux's `cleanup_irq` (`:659-670`) also writes LATCHED_CLR to that
/// peripheral; that write is left out here, because no peripheral outside [`crate::allow`] may be written.
pub fn clear_unknown<A: Arbiter>(a: &mut A, apid: u32, status: u32) {
    a.apid_write(apid, IRQ_CLEAR, status);
}
