// Verus proof of the SHIPPED code, not a model of it (project rule D-011): this file includes
// kernel/lucina-crash/src/witness.rs by #[path], so Verus checks `bounded_len`, `reason_len`, `printable` and `first_time`
// as the kernel compiles them (patch 1057, the modem witness). On top of them:
//   1. the handler's text, built as the kernel builds it (read at most `bounded_len` bytes, keep up to the first NUL, map
//      each kept byte), is at most MAX_REASON bytes, every byte printable, and read no further than the item and the buffer;
//   2. with the kernel's `fetch_or`, however many times either cause fires, at most one record per cause is written.
#![feature(proc_macro_hygiene)]
#![allow(unexpected_cfgs)]
#[path = "../../kernel/lucina-crash/src/witness.rs"]
pub mod witness;

use vstd::prelude::*;
use witness::*;

verus! {

/// 1. The handler's text: `raw` stands for the buffer `read_item_into` filled with `got` bytes of an item of `item_len`.
fn handler_text(item_len: u32, raw: &[u8]) -> (out: Vec<u8>)
    requires
        raw.len() == MAX_REASON,
    ensures
        out.len() <= MAX_REASON,
        out.len() as int <= item_len as int,
        forall|i: int| 0 <= i < out.len() ==> 0x20 <= #[trigger] out[i] <= 0x7e,
        forall|i: int| 0 <= i < out.len() ==> (0x20 <= raw[i] <= 0x7e ==> out[i] == raw[i]),
{
    let got = bounded_len(item_len, MAX_REASON);
    let kept = reason_len(raw, got);
    let mut out: Vec<u8> = Vec::new();
    let mut i = 0;
    while i < kept
        invariant
            0 <= i <= kept,
            kept <= got,
            got <= MAX_REASON,
            got as int <= item_len as int,
            kept <= raw.len(),
            out.len() == i,
            forall|j: int| 0 <= j < i ==> 0x20 <= #[trigger] out[j] <= 0x7e,
            forall|j: int| 0 <= j < i ==> (0x20 <= raw[j] <= 0x7e ==> out[j] == raw[j]),
        decreases kept - i,
    {
        let b = printable(raw[i]);
        out.push(b);
        i += 1;
    }
    out
}

/// The bit facts of the gate: a set bit stays set under any further `|`.
proof fn lemma_or_keeps(m: u8, b: u8, x: u8)
    ensures
        b != 0u8 ==> (m | b) & b != 0u8,
        m & b != 0u8 ==> (m | x) & b != 0u8,
{
    assert(b != 0u8 ==> (m | b) & b != 0u8) by (bit_vector);
    assert(m & b != 0u8 ==> (m | x) & b != 0u8) by (bit_vector);
}

/// How many of the calls `bits[0..k]`, starting from mask `m`, find their bit clear (and would write a record), counting
/// only the calls for bit `b`.
pub open spec fn writes(m: u8, bits: Seq<u8>, b: u8) -> nat
    decreases bits.len(),
{
    if bits.len() == 0 {
        0
    } else {
        let x = bits[0];
        let here: nat = if x == b && m & x == 0u8 { 1 } else { 0 };
        here + writes(m | x, bits.drop_first(), b)
    }
}

/// Once bit `b` is set in the mask, no later call for it writes.
proof fn lemma_none_after(m: u8, bits: Seq<u8>, b: u8)
    requires
        m & b != 0u8,
    ensures
        writes(m, bits, b) == 0,
    decreases bits.len(),
{
    if bits.len() > 0 {
        let x = bits[0];
        lemma_or_keeps(m, b, x);
        lemma_none_after(m | x, bits.drop_first(), b);
    }
}

/// 2. Whatever the order and number of the calls (both causes mixed), each cause writes at most once.
proof fn theorem_once_per_cause(m: u8, bits: Seq<u8>, b: u8)
    requires
        b == WATCHDOG || b == FATAL,
    ensures
        writes(m, bits, b) <= 1,
    decreases bits.len(),
{
    if bits.len() > 0 {
        let x = bits[0];
        let rest = bits.drop_first();
        if x == b && m & x == 0u8 {
            lemma_or_keeps(m, b, x);
            assert((m | x) & b != 0u8);
            lemma_none_after(m | x, rest, b);
        } else {
            theorem_once_per_cause(m | x, rest, b);
        }
    }
}

/// The shipped gate agrees with the model's count: it says yes exactly when the bit is clear.
fn gate(seen: u8, bit: u8) -> (r: bool)
    ensures
        r == (seen & bit == 0u8),
{
    first_time(seen, bit)
}

} // verus!

fn main() {}
