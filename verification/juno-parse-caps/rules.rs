// Verus proof of the SHIPPED code, not a model of it (project rule D-011): this file includes
// kernel/lucina-parse-caps/src/lib.rs by #[path], so Verus checks the guards exactly as the Craft apps compile them; what is
// proved of each is in its `ensures`. On top of that, four models whose steps call the shipped guards prove what the apps
// rely on, for every input:
// - a budget asked for any sequence of amounts never hands out more than it started with;
// - a capped read from a source of any length, returning any amounts, never holds more than its cap plus one byte;
// - an archive walked entry by entry through `count_next` and `zip_entry` accepts at most its entry count, no entry over its
//   cap or its ratio, and never more declared bytes in all than its total;
// - a recursive descent through `enter` into a document nested to any depth never recurses deeper than its limit.
#![feature(proc_macro_hygiene)]
#[path = "../../kernel/lucina-parse-caps/src/lib.rs"]
pub mod caps;

use caps::*;
use vstd::prelude::*;

verus! {

/// A budget of `cap`, asked for `asks[0]`, `asks[1]`, … in turn: what it grants adds up to at most `cap`.
fn spend_all(cap: u64, asks: &Vec<u64>) -> (given: u64)
    ensures
        given <= cap,
{
    let mut left = cap;
    let mut given: u64 = 0;
    let mut i: usize = 0;
    while i < asks.len()
        invariant
            given as int + left as int == cap as int,
            i <= asks.len(),
        decreases asks.len() - i,
    {
        match take(left, asks[i]) {
            Some(l) => {
                given = given + asks[i];
                left = l;
            },
            None => {},
        }
        i += 1;
    }
    given
}

/// A capped read: each call asks the source for what `read_next` allows (wanting 64 KiB), and the source returns any
/// amount up to that (`source[i]`, cut to the ask). However long the source, the buffer never holds more than `cap + 1`.
fn read_capped(cap: u64, source: &Vec<u64>) -> (have: u64)
    requires
        cap < u64::MAX,
    ensures
        have <= cap + 1,
{
    let mut have: u64 = 0;
    let mut i: usize = 0;
    while i < source.len()
        invariant
            have <= cap + 1,
            cap < u64::MAX,
            i <= source.len(),
        decreases source.len() - i,
    {
        let ask = read_next(have, 65536, cap);
        let got = if source[i] < ask { source[i] } else { ask };
        have = have + got;
        i += 1;
    }
    have
}

/// An archive with entries declaring `declared[i]` bytes from `packed[i]`: the walk accepts at most `max_entries` of
/// them, and the bytes it accepts add up to at most `max_total`.
fn walk_archive(declared: &Vec<u64>, packed: &Vec<u64>, max_entries: u64, max_entry: u64, max_ratio: u64, max_total: u64)
    -> (r: (u64, u64))
    requires
        declared.len() == packed.len(),
    ensures
        r.0 <= max_entries,
        r.1 <= max_total,
{
    let mut count: u64 = 0;
    let mut left = max_total;
    let mut i: usize = 0;
    while i < declared.len()
        invariant
            count <= max_entries,
            left <= max_total,
            i <= declared.len(),
            declared.len() == packed.len(),
        decreases declared.len() - i,
    {
        match count_next(count, max_entries) {
            None => return (count, max_total - left),
            Some(c) => {
                match zip_entry(declared[i], packed[i], max_entry, max_ratio, left) {
                    Some(l) => {
                        count = c;
                        left = l;
                    },
                    // a refused entry refuses the archive
                    None => return (count, max_total - left),
                }
            },
        }
        i += 1;
    }
    (count, max_total - left)
}

/// A recursive descent at `depth` into a document nested `nested` levels deeper: every level goes through `enter`, so
/// however deep the document, the recursion stops at `max`.
fn descend(depth: u32, max: u32, nested: u64) -> (deepest: u32)
    requires
        depth <= max,
    ensures
        depth <= deepest <= max,
    decreases max - depth,
{
    if nested == 0 {
        return depth;
    }
    match enter(depth, max) {
        Some(d) => descend(d, max, nested - 1),
        None => depth,
    }
}

} // verus!

fn main() {}
