// Verus proof of the SHIPPED code, not a model (project rule D-011): this file names the source file, and Verus checks it as the
// kernel compiles it. On top of each function's `ensures`: a Fn Lock event read off the EC sets the row's swap right.
#![feature(proc_macro_hygiene)]
#[path = "../../drivers/lucina-i2c-hid/src/samsung_ec.rs"]
pub mod ec;

use ec::*;
use vstd::prelude::*;

verus! {

/// With the hot keys wanted, the keyboard's Fn Lock turning on (an EC event 0x20 with value 1) stops the swap, and its turning
/// off (value 0) starts it again: a plain F-key gives its hot key either way.
fn fn_lock_on_then_off() {
    let mut on_buf = [0u8; 12];
    on_buf[0] = 1;
    on_buf[2] = 0x20;
    on_buf[3] = 1;
    let e = parse(&on_buf);
    assert(e == Some(EcEvent::FnLock(Some(true))));
    let lock = next_state(false, Some(true));
    let s1 = swap_active(true, lock);
    assert(!s1);
    let lock = next_state(lock, Some(false));
    let s2 = swap_active(true, lock);
    assert(s2);
}

} // verus!

fn main() {}
