// Verus proof of the SHIPPED code, not a model (project rule D-011): this file names the source file, and Verus checks it as the
// apps compile it. What is proved of each function is in its `ensures`; what follows adds runs of presses on top: whatever keys
// are pressed in whatever order, the selection stays inside the list, an empty list has none, and a key only moves the way it points.
#![feature(proc_macro_hygiene)]
#[path = "../../services/lucina-nav/src/lib.rs"]
pub mod nav;

use nav::*;
use vstd::prelude::*;

verus! {

/// What a key does to a list.
#[derive(Clone, Copy)]
pub enum Key {
    Up,
    Down,
    Home,
    End,
    PageUp,
    PageDown,
    RowUp,
    RowDown,
}

fn apply(cur: Option<usize>, len: usize, page: usize, cols: usize, k: Key) -> (r: Option<usize>)
    ensures
        sel_ok(r, len),
        len == 0 ==> r == None::<usize>,
        len > 0 ==> r is Some,
        match k {
            Key::Up | Key::PageUp | Key::RowUp => sel_ok(cur, len) ==> at(r) <= at(cur) || cur is None,
            Key::Down | Key::PageDown | Key::RowDown => sel_ok(cur, len) ==> at(r) >= at(cur),
            _ => true,
        },
{
    match k {
        Key::Up => prev(cur, len),
        Key::Down => next(cur, len),
        Key::Home => first(len),
        Key::End => last(len),
        Key::PageUp => page_up(cur, len, page),
        Key::PageDown => page_down(cur, len, page),
        Key::RowUp => row_up(cur, len, cols),
        Key::RowDown => row_down(cur, len, cols),
    }
}

/// Any run of keys on any list, from any start (even a stale one): the selection never leaves 0..len, and a list with no rows
/// never has one.
fn presses(start: Option<usize>, len: usize, page: usize, cols: usize, keys: &Vec<Key>) -> (r: Option<usize>)
    ensures
        sel_ok(r, len),
        len == 0 ==> r == None::<usize>,
{
    let mut cur = clamp(start, len);
    let mut i = 0;
    while i < keys.len()
        invariant
            sel_ok(cur, len),
            len == 0 ==> cur == None::<usize>,
        decreases keys.len() - i,
    {
        cur = apply(cur, len, page, cols, keys[i]);
        i += 1;
    }
    cur
}

/// Down on the last row, Up on the first, and Next on the last file all stay: no wrap.
fn no_wrap(len: usize, ok: &[bool])
    requires
        len > 0,
{
    let end = next(Some((len - 1) as usize), len);
    assert(end == Some((len - 1) as usize));
    let top = prev(Some(0usize), len);
    assert(top == Some(0usize));
    let down = page_down(Some((len - 1) as usize), len, 7);
    assert(down == Some((len - 1) as usize));
    let up = page_up(Some(0usize), len, 7);
    assert(up == Some(0usize));
}

/// A viewer on the last file of its kind stays; on the first, stays; walking never leaves the folder.
fn viewer_stays(ok: &[bool], at: usize)
    requires
        at < ok.len(),
        forall|k: int| at < k && k < ok.len() ==> !ok[k],
{
    let to = walk(ok, at, Step::Next);
    assert(to == at);
}

fn viewer_stays_first(ok: &[bool], at: usize)
    requires
        at < ok.len(),
        forall|k: int| 0 <= k && k < at ==> !ok[k],
{
    let to = walk(ok, at, Step::Prev);
    assert(to == at);
}

} // verus!

fn main() {}
