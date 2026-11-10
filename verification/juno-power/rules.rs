// Verus proof of the SHIPPED code, not a model (project rule D-011): this file only names the source files, and Verus checks them as
// the kernel compiles them. What is proved is in each function's `ensures`, and in the lemma below: the PMIC writes of the wake
// interrupts never reach a register outside the power key's and the alarm's interrupt and alarm registers (no regulator, charger,
// reset or thermal register), and the PDC words change only in the bits they mean to.
#![feature(proc_macro_hygiene)]
#[path = "../../kernel/lucina-power/src/level.rs"]
pub mod level;
#[path = "../../kernel/lucina-power/src/wake_rules.rs"]
pub mod wake_rules;
#[path = "../../kernel/lucina-spmi/src/allow.rs"]
pub mod allow;

use vstd::prelude::*;

verus! {

/// What `lucina_spmi::irq::write` lets through: every byte it writes is an allowed register of the PON (0x800) or the alarm
/// (0x6100), and of the one peripheral `ppid` names.
proof fn allowed_writes_stay_in_the_wake_registers(ppid: u16, addr: u16, len: usize)
    requires
        allow::allowed_spec(ppid, addr, len),
    ensures
        ppid == allow::PON_PPID || ppid == allow::ALARM_PPID,
        (addr >> 8) == (ppid & 0xff),
        1 <= len <= 4,
        forall|i: int| 0 <= i < len ==> #[trigger] allow::reg_allowed_spec(ppid, ((addr & 0xff) as int + i) as u16),
{
    assert forall|i: int| 0 <= i < len implies #[trigger] allow::reg_allowed_spec(ppid, ((addr & 0xff) as int + i) as u16) by {
        if i == 0 {
        } else if i == 1 {
        } else if i == 2 {
        } else {
        }
    }
}

/// `with_type`: the type is in bits 2..0, and no other bit of the word changes.
proof fn with_type_keeps_the_other_bits(cfg: u32, t: wake_rules::PdcType)
    ensures
        ((cfg & !7u32) | wake_rules::type_bits_spec(t)) & 7u32 == wake_rules::type_bits_spec(t),
        ((cfg & !7u32) | wake_rules::type_bits_spec(t)) & !7u32 == cfg & !7u32,
{
    let tb = wake_rules::type_bits_spec(t);
    assert(tb <= 7u32);
    assert(((cfg & !7u32) | tb) & 7u32 == tb && ((cfg & !7u32) | tb) & !7u32 == cfg & !7u32) by (bit_vector)
        requires tb <= 7u32;
}

/// `with_enable`: only the pin's enable bit changes, and it ends as asked.
proof fn with_enable_changes_one_bit(old: u32, b: u32)
    ensures
        (old | b) & !b == old & !b,
        (old | b) & b == b,
        (old & !b) & !b == old & !b,
        (old & !b) & b == 0,
{
    assert((old | b) & !b == old & !b && (old | b) & b == b && (old & !b) & !b == old & !b && (old & !b) & b == 0)
        by (bit_vector);
}

/// The enable bit is one bit: pin % 32 before version 3.2, bit 3 from then on.
proof fn the_enable_bit_is_one_bit(version: u32, pin: u32)
    ensures
        version < 0x30200 ==> wake_rules::enable_bit_spec(version, pin) == (1u32 << (pin % 32)),
        version >= 0x30200 ==> wake_rules::enable_bit_spec(version, pin) == 8u32,
{
}

/// An allowed register is an interrupt register (SET_TYPE..EN_CLR) of either, or one of the alarm's own.
proof fn an_allowed_register_is_an_interrupt_or_alarm_register(ppid: u16, reg: u16)
    requires
        allow::reg_allowed_spec(ppid, reg),
    ensures
        (ppid == 0x008 && 0x11 <= reg <= 0x16)
            || (ppid == 0x061 && ((0x11 <= reg <= 0x16) || (0x40 <= reg <= 0x43) || reg == 0x46 || reg == 0x48)),
{
}

/// The power thread (patch 1193) waits for an interrupt only with every wake line armed, and only after ten quiet polls; without
/// the lines it polls every time, as before.
proof fn the_power_thread_waits_only_when_armed_and_quiet(armed: bool, quiet: u32)
    ensures
        !armed ==> !wake_rules::may_wait_spec(armed, quiet),
        quiet < 10 ==> !wake_rules::may_wait_spec(armed, quiet),
        armed && quiet >= 10 ==> wake_rules::may_wait_spec(armed, quiet),
        wake_rules::QUIET_POLLS == 10,
{
}

} // verus!

fn main() {}
