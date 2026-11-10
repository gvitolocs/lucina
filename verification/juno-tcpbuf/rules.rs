// Verus proof of the SHIPPED code, not a model of it (project rule D-011): this file includes
// third_party/smoltcp/src/storage/paged_math.rs by #[path], so Verus checks `piece_at` as the kernel compiles it (patch
// 1053: TCP socket buffers kept in pages). What it promises is in its `ensures`: a piece never leaves its page or the
// storage, and is never empty inside the storage. On top of it, the ring's accounting and its copy loops, as
// smoltcp's `RingBuffer` does them with paged storage (third_party/smoltcp/src/storage/ring_buffer.rs):
//   1. `read_at < cap` and `length <= cap` hold through every enqueue and dequeue, which never take more than the room
//      (`window`) or the data (`len`), nor more than one piece;
//   2. the copy loops (`enqueue_slice`, `dequeue_slice`, `write_unallocated`, `read_allocated`) move at least one element
//      per round while there is both room and something to copy, so they end, and move min(n, room) in all;
//   3-5. (patch 1054) the kernel's UDP copies and syslog: `chunk_plan` and `capped`, as the kernel calls them, keep every
//      allocation within a page and every buffer within its bound, for every length.
#![feature(proc_macro_hygiene)]
#![allow(unexpected_cfgs)]
#[path = "../../third_party/smoltcp/src/storage/paged_math.rs"]
pub mod paged_math;

use paged_math::*;
use vstd::prelude::*;

verus! {

/// The ring: where reading starts, how much it holds, in a storage of `cap` elements kept in pages of `page`.
pub struct Ring {
    pub read_at: usize,
    pub length: usize,
    pub cap: usize,
    pub page: usize,
}

pub open spec fn inv(r: Ring) -> bool {
    &&& r.page > 0
    &&& r.length <= r.cap
    &&& (r.cap == 0 ==> r.read_at == 0)
    &&& (r.cap > 0 ==> r.read_at < r.cap)
}

/// Where the next element goes (`get_idx(length)`).
fn write_idx(r: &Ring) -> (w: usize)
    requires
        inv(*r),
    ensures
        r.cap > 0 ==> w < r.cap,
{
    if r.cap == 0 {
        0
    } else {
        ((r.read_at as u128 + r.length as u128) % r.cap as u128) as usize
    }
}

/// `contiguous_window`: as much room as there is, no further than the piece at the write index.
fn contiguous_window(r: &Ring) -> (n: usize)
    requires
        inv(*r),
    ensures
        n <= r.cap - r.length,
        r.length < r.cap ==> n >= 1,
{
    let w = write_idx(r);
    let piece = piece_at(w, r.page, r.cap).2;
    let room = r.cap - r.length;
    if piece < room { piece } else { room }
}

/// `enqueue_many_with` taking `size` elements of the contiguous window.
fn enqueue(r: &mut Ring, size: usize)
    requires
        inv(*old(r)),
        size <= old(r).cap - old(r).length,
    ensures
        inv(*final(r)),
        final(r).length == old(r).length + size,
        final(r).cap == old(r).cap,
        final(r).page == old(r).page,
{
    if r.length == 0 {
        // Empty: smoltcp starts again at 0 for the most contiguous room.
        r.read_at = 0;
    }
    r.length = r.length + size;
}

/// `dequeue_many_with` taking `size` elements of the piece at `read_at`.
fn dequeue(r: &mut Ring, size: usize)
    requires
        inv(*old(r)),
        size <= old(r).length,
    ensures
        inv(*final(r)),
        final(r).length == old(r).length - size,
        final(r).cap == old(r).cap,
        final(r).page == old(r).page,
{
    if r.cap > 0 {
        r.read_at = ((r.read_at as u128 + size as u128) % r.cap as u128) as usize;
    } else {
        r.read_at = 0;
    }
    r.length = r.length - size;
}

/// `enqueue_slice`'s loop: one piece per round, as much of the rest as the piece takes, until a round copies nothing.
fn enqueue_slice(r: &mut Ring, n: usize) -> (done: usize)
    requires
        inv(*old(r)),
    ensures
        inv(*final(r)),
        done == if (n as int) < old(r).cap - old(r).length { n as int } else { old(r).cap - old(r).length },
        final(r).length == old(r).length + done,
        final(r).cap == old(r).cap,
{
    let ghost room0: int = r.cap - r.length;
    let mut done: usize = 0;
    loop
        invariant
            inv(*r),
            r.cap == old(r).cap,
            r.page == old(r).page,
            done <= n,
            r.length == old(r).length + done,
            room0 == old(r).cap - old(r).length,
            done <= room0,
        ensures
            done == if (n as int) < room0 { n as int } else { room0 },
            inv(*r),
            r.length == old(r).length + done,
        decreases n - done,
    {
        let window = contiguous_window(r);
        let rest = n - done;
        let size = if window < rest { window } else { rest };
        if size == 0 {
            // Nothing copied: either all of it is in (done == n) or there is no room (window == 0, so length == cap).
            break;
        }
        enqueue(r, size);
        done = done + size;
    }
    done
}

/// `dequeue_slice`'s loop: one piece per round, until a round copies nothing.
fn dequeue_slice(r: &mut Ring, n: usize) -> (done: usize)
    requires
        inv(*old(r)),
    ensures
        inv(*final(r)),
        done == if n < old(r).length { n as int } else { old(r).length as int },
        final(r).length == old(r).length - done,
        final(r).cap == old(r).cap,
{
    let mut done: usize = 0;
    loop
        invariant
            inv(*r),
            r.cap == old(r).cap,
            r.page == old(r).page,
            done <= n,
            done <= old(r).length,
            r.length == old(r).length - done,
        ensures
            done == if n < old(r).length { n as int } else { old(r).length as int },
            inv(*r),
            r.length == old(r).length - done,
        decreases n - done,
    {
        // `dequeue_many_with`: no more than is held, no further than the piece at `read_at`.
        let piece = piece_at(r.read_at, r.page, r.cap).2;
        let available = if piece < r.length { piece } else { r.length };
        let rest = n - done;
        let size = if available < rest { available } else { rest };
        if size == 0 {
            break;
        }
        dequeue(r, size);
        done = done + size;
    }
    done
}

/// A piece at an index inside a non-empty storage is never empty: the loops above always move while they can.
fn piece_moves(at: usize, page: usize, cap: usize)
    requires
        page > 0,
        at < cap,
{
    let p = piece_at(at, page, cap);
    assert(p.2 >= 1);
    assert(p.1 + p.2 <= page);
    assert(at + p.2 <= cap);
}

/// 3. (patch 1054) A UDP send the kernel takes (at most 64 KiB, `UDP_SEND_PAYLOAD_LEN`; more is EMSGSIZE before anything is
/// allocated) is staged in at most 16 chunks, each at most a page, that add up to its length.
fn udp_send_plan(total: usize)
    requires
        total <= 65536,
{
    let (n, last) = chunk_plan(total, 4096);
    assert(n <= 16) by (nonlinear_arith)
        requires
            (n - 1) * 4096 + last == total,
            total <= 65536,
            n >= 1,
            total > 0 ==> last >= 1,
            total == 0 ==> n == 1;
    assert(last <= 4096);
}

/// 4. (patch 1054) The receive staging, `chunk_plan(UDP_RECV_PAYLOAD_LEN, PAGE)`, is 16 whole pages: room for the largest
/// datagram the socket holds.
fn udp_recv_plan() {
    let (n, last) = chunk_plan(65536, 4096);
    assert(n == 16 && last == 4096) by (nonlinear_arith)
        requires
            (n - 1) * 4096 + last == 65536,
            1 <= last <= 4096,
            n >= 1;
}

/// 5. (patch 1054) syslog's buffer never asks for more than the log ring, whatever length the caller gives.
fn syslog_cap(len: usize) {
    let c = capped(len, 512 * 1024);
    assert(c <= 512 * 1024);
    assert(c <= len);
}

} // verus!

fn main() {}
