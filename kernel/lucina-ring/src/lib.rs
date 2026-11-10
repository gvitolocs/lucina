// SPDX-License-Identifier: MPL-2.0

//! Index arithmetic of a paged ring buffer, for the kernel's pipes and local sockets (`docs/research/juno-ring.md`).
//!
//! # One page per slot, not one contiguous block
//!
//! Asterinas' ring buffer (`kernel/libs/ring-buffer`) takes one physically contiguous block for the whole capacity with
//! `alloc_segment(..).unwrap()`, so a fragmented memory panics the kernel (`NoMemory at kernel/libs/ring-buffer/src/lib.rs:104`
//! on the laptop). Linux keeps one page per `pipe_buffer` (`fs/pipe.c`), takes it with `alloc_page` when a write needs it
//! and returns ENOMEM to the writer when there is none, and frees it once it has been read. The ring here does the same:
//! slot `i` of a ring of `cap` bytes is page `i`, allocated when first written and freed when no unread byte lives in it.
//!
//! This file is the arithmetic only: where a byte position lives, how a copy splits at page ends, how much is held and how
//! much room is left, and which pages a read finished. Head and tail are free-running counters that wrap at `usize::MAX`,
//! as `Wrapping<usize>` in the kernel; `cap` is a power of two, a multiple of [`PAGE`], at most [`MAX_CAP`]. Verus proves
//! this very file (`verification/juno-ring/rules.rs`, which adds the FIFO proof); what each function promises is in its
//! `ensures`.
#![no_std]
#![allow(unexpected_cfgs)]

#[cfg(verus_keep_ghost)]
use vstd::arithmetic::div_mod::*;
#[cfg(verus_keep_ghost)]
use vstd::prelude::*;

/// Bytes in a page, the unit that is allocated and freed.
#[cfg_attr(verus_keep_ghost, verus_verify)]
pub const PAGE: usize = 4096;
/// The largest capacity, 1 GiB.
#[cfg_attr(verus_keep_ghost, verus_verify)]
pub const MAX_CAP: usize = 0x4000_0000;

#[cfg(verus_keep_ghost)]
verus! {

/// Positions wrap at this.
pub open spec fn wrap() -> int {
    usize::MAX as int + 1
}

/// A capacity the ring takes: a multiple of a page dividing 2^30, that is a power of two from a page to 1 GiB.
pub open spec fn cap_ok(cap: usize) -> bool {
    PAGE <= cap && cap % PAGE == 0 && MAX_CAP % cap == 0
}

/// Bytes from `from` to `to` on the wrapping counter.
pub open spec fn dist(from: usize, to: usize) -> int {
    if to >= from { to - from } else { to - from + wrap() }
}

/// The position `i` bytes after `base`.
pub open spec fn at(base: usize, i: int) -> int {
    (base + i) % wrap()
}

/// The page that position `pos` lives in.
pub open spec fn page_of(pos: int, cap: usize) -> int {
    (pos % cap as int) / PAGE as int
}

/// The page global page number `x / PAGE` maps to.
pub open spec fn gp(x: int, cap: usize) -> int {
    (x / PAGE as int) % (cap / PAGE) as int
}

/// Page `k` of those [`finished_pages`] reports from `first`.
pub open spec fn freed(first: usize, k: int, cap: usize) -> int {
    page_of(at(first, k * PAGE), cap)
}

/// The page of the position `x` bytes after `o`.
pub open spec fn gpos(o: int, x: int, cap: usize) -> int {
    gp(o + x, cap)
}

/// Global page `k` from `gs`, as a page of the ring.
pub open spec fn rep(gs: int, k: int, cap: usize) -> int {
    gp(4096 * (gs + k), cap)
}

/// Whether page `page` holds an unread byte of [head, tail).
pub open spec fn in_use(page: int, head: usize, tail: usize, cap: usize) -> bool {
    exists|i: int| 0 <= i < dist(head, tail) && page_of(#[trigger] at(head, i), cap) == page
}

pub proof fn lemma_wrap()
    ensures
        wrap() % (MAX_CAP as int) == 0,
        wrap() % (PAGE as int) == 0,
        wrap() >= 0x1_0000_0000,
{
    assert(usize::MAX as int == 0xffff_ffff || usize::MAX as int == 0xffff_ffff_ffff_ffff);
    assert(0x1_0000_0000int % 0x4000_0000 == 0 && 0x1_0000_0000int % 4096 == 0) by (compute);
    assert(0x1_0000_0000_0000_0000int % 0x4000_0000 == 0 && 0x1_0000_0000_0000_0000int % 4096 == 0) by (compute);
}

/// `x` mod `m` for `0 <= x < 2m`.
pub proof fn lemma_mod_range(x: int, m: int)
    requires
        0 < m,
        0 <= x < 2 * m,
    ensures
        x % m == if x < m { x } else { x - m },
{
    if x < m {
        lemma_fundamental_div_mod_converse(x, m, 0, x);
    } else {
        lemma_fundamental_div_mod_converse(x, m, 1, x - m);
    }
}

/// Values that agree modulo `m` agree modulo every divisor `c` of `m`.
pub proof fn lemma_mod_cong(x: int, y: int, m: int, c: int)
    requires
        0 < c,
        0 < m,
        m % c == 0,
        x % m == y % m,
    ensures
        x % c == y % c,
{
    lemma_fundamental_div_mod(m, c);
    lemma_fundamental_div_mod(x, m);
    lemma_fundamental_div_mod(y, m);
    let k = m / c;
    assert(x == c * (k * (x / m)) + x % m) by (nonlinear_arith)
        requires
            m == c * k,
            x == m * (x / m) + x % m,
    ;
    assert(y == c * (k * (y / m)) + y % m) by (nonlinear_arith)
        requires
            m == c * k,
            y == m * (y / m) + y % m,
    ;
    lemma_mod_multiples_vanish(k * (x / m), x % m, c);
    lemma_mod_multiples_vanish(k * (y / m), y % m, c);
}

/// Values in `[0, m)` that agree modulo `m` are equal.
pub proof fn lemma_mod_eq(x: int, y: int, m: int)
    requires
        0 < m,
        0 <= x < m,
        0 <= y < m,
        x % m == y % m,
    ensures
        x == y,
{
    lemma_fundamental_div_mod_converse(x, m, 0, x);
    lemma_fundamental_div_mod_converse(y, m, 0, y);
}

/// Values less than `m` apart and not equal differ modulo `m`.
pub proof fn lemma_mod_distinct(x: int, y: int, m: int)
    requires
        0 < m,
        0 <= x,
        0 <= y,
        x != y,
        -m < x - y < m,
    ensures
        x % m != y % m,
{
    lemma_fundamental_div_mod(x, m);
    lemma_fundamental_div_mod(y, m);
    if x % m == y % m {
        assert(x - y == m * (x / m - y / m)) by (nonlinear_arith)
            requires
                x == m * (x / m) + x % m,
                y == m * (y / m) + y % m,
                x % m == y % m,
        ;
        let q = x / m - y / m;
        assert(false) by (nonlinear_arith)
            requires
                x - y == m * q,
                -m < x - y < m,
                x != y,
                0 < m,
        ;
    }
}

/// The facts about a capacity the proofs use: it divides the wrap and is `pages` whole pages.
pub proof fn lemma_cap(cap: usize)
    requires
        cap_ok(cap),
    ensures
        wrap() % (cap as int) == 0,
        cap as int == PAGE * (cap / PAGE),
        1 <= cap / PAGE,
        cap <= MAX_CAP,
{
    lemma_wrap();
    let c = cap as int;
    let mc = MAX_CAP as int;
    lemma_fundamental_div_mod(mc, c);
    lemma_fundamental_div_mod(wrap(), mc);
    lemma_fundamental_div_mod(c, PAGE as int);
    if c > mc {
        lemma_fundamental_div_mod_converse(mc, c, 0, mc);
    }
    let k = (mc / c) * (wrap() / mc);
    assert(wrap() == c * k + 0) by (nonlinear_arith)
        requires
            mc == c * (mc / c) + mc % c,
            mc % c == 0,
            wrap() == mc * (wrap() / mc) + wrap() % mc,
            wrap() % mc == 0,
            k == (mc / c) * (wrap() / mc),
    ;
    lemma_fundamental_div_mod_converse(wrap(), c, k, 0);
}

/// Position `at(b, i)` taken modulo a capacity is `b + i` modulo it.
pub proof fn lemma_at_mod(b: usize, i: int, x: int, c: int)
    requires
        0 < c,
        wrap() % c == 0,
        (b + i) % wrap() == x % wrap(),
    ensures
        at(b, i) % c == x % c,
{
    lemma_wrap();
    let y = at(b, i);
    lemma_fundamental_div_mod(b + i, wrap());
    lemma_fundamental_div_mod_converse(y, wrap(), 0, y);
    lemma_mod_cong(y, x, wrap(), c);
}

/// The page of `x` is its global page number `x / PAGE` modulo the pages of the ring, and its offset in the page is `x % PAGE`.
pub proof fn lemma_page(x: int, cap: usize)
    requires
        cap_ok(cap),
        0 <= x,
    ensures
        page_of(x, cap) == (x / PAGE as int) % (cap / PAGE) as int,
        (x % cap as int) % PAGE as int == x % PAGE as int,
        page_of(x, cap) * PAGE + x % PAGE as int == x % cap as int,
        0 <= page_of(x, cap) < cap / PAGE,
{
    lemma_cap(cap);
    let c = cap as int;
    let p = PAGE as int;
    let np = (cap / PAGE) as int;
    let q = x / c;
    let s = x % c;
    let u = s / p;
    let v = s % p;
    lemma_fundamental_div_mod(x, c);
    lemma_fundamental_div_mod(s, p);
    assert(x == p * (np * q + u) + v) by (nonlinear_arith)
        requires
            x == c * q + s,
            s == p * u + v,
            c == p * np,
    ;
    assert(u < np) by (nonlinear_arith)
        requires
            s == p * u + v,
            0 <= v,
            s < c,
            c == p * np,
            p > 0,
    ;
    assert(0 <= q) by (nonlinear_arith)
        requires
            x == c * q + s,
            s < c,
            0 <= x,
            c > 0,
    ;
    assert(0 <= u) by (nonlinear_arith)
        requires
            s == p * u + v,
            v < p,
            0 <= s,
            p > 0,
    ;
    lemma_fundamental_div_mod_converse(x, p, np * q + u, v);
    lemma_fundamental_div_mod_converse(np * q + u, np, q, u);
}

/// The page of position `at(b, i)`, for `b + i` known as `x` modulo the wrap.
pub proof fn lemma_page_at(b: usize, i: int, x: int, cap: usize)
    requires
        cap_ok(cap),
        0 <= x,
        (b + i) % wrap() == x % wrap(),
    ensures
        page_of(at(b, i), cap) == (x / PAGE as int) % (cap / PAGE) as int,
{
    lemma_cap(cap);
    lemma_at_mod(b, i, x, cap as int);
    lemma_fundamental_div_mod(b + i, wrap());
    lemma_page(at(b, i), cap);
    lemma_page(x, cap);
}

/// Adding a multiple of the wrap does not move a position.
pub proof fn lemma_shift(x: int, w: int)
    ensures
        (x + wrap() * w) % wrap() == x % wrap(),
{
    lemma_wrap();
    lemma_mod_multiples_vanish(w, x, wrap());
}

/// `page_in_use`, on ring offsets: page `page` holds a byte of the `n` bytes from ring offset `h` exactly when the page
/// start is fewer than `n` bytes after `h`, or `h` is inside the page.
pub proof fn lemma_in_use(page: int, head: usize, tail: usize, cap: usize, d: int, e: int)
    requires
        cap_ok(cap),
        0 <= page < cap / PAGE,
        dist(head, tail) <= cap,
        d == (page * PAGE + cap - head % cap) % cap as int,
        e == (head % cap + cap - page * PAGE) % cap as int,
    ensures
        in_use(page, head, tail, cap) == (dist(head, tail) > 0 && (d < dist(head, tail) || e < PAGE)),
{
    lemma_cap(cap);
    let c = cap as int;
    let p = PAGE as int;
    let n = dist(head, tail);
    let h = head as int % c;
    let s = page * p;
    let np = c / p;
    assert(s + p <= c) by (nonlinear_arith)
        requires
            page < np,
            c == p * np,
            s == page * p,
            p > 0,
    ;
    lemma_fundamental_div_mod(head as int, c);
    lemma_mod_range(s + c - h, c);
    lemma_mod_range(h + c - s, c);
    // Bytes `i` after head live at ring offset (h + i) % c.
    assert forall|i: int| 0 <= i < n implies page_of(#[trigger] at(head, i), cap) == ((h + i) % c) / p by {
        lemma_at_mod(head, i, head + i, c);
        lemma_mod_multiples_vanish(head as int / c, h + i, c);
        assert(head + i == c * (head as int / c) + (h + i));
    }
    if n > 0 && d < n {
        lemma_mod_range(h + d, c);
        lemma_fundamental_div_mod_converse(s, p, page, 0);
        assert(page_of(at(head, d), cap) == page);
    } else if n > 0 && e < p {
        lemma_mod_range(h, c);
        lemma_fundamental_div_mod_converse(h, p, page, h - s);
        assert(page_of(at(head, 0), cap) == page);
    } else {
        assert forall|i: int| 0 <= i < n implies page_of(#[trigger] at(head, i), cap) != page by {
            lemma_mod_range(h + i, c);
            let y = (h + i) % c;
            lemma_fundamental_div_mod(y, p);
            if y / p == page {
                assert(s <= y < s + p) by (nonlinear_arith)
                    requires
                        y == p * (y / p) + y % p,
                        0 <= y % p < p,
                        y / p == page,
                        s == page * p,
                ;
            }
        }
    }
}

/// A read of `r` bytes from `o`, with the data unread up to `l`, on global page numbers: the pages [`finished_pages`]
/// reports (`count` of them from `gs`) are at most the ring's, and none holds an unread byte.
pub proof fn lemma_finished(o: int, r: int, l: int, cap: usize, ext: bool, excl: bool, gs: int, count: int)
    requires
        cap_ok(cap),
        0 <= o,
        0 <= r <= l <= cap,
        ext == (r == l && (o + r) % PAGE as int != 0 && r != 0),
        excl == ((o + r) / PAGE as int + (if ext { 1int } else { 0 }) - o / PAGE as int > 0 && l + o % PAGE as int > cap),
        gs == o / PAGE as int + if excl { 1int } else { 0 },
        count == (o + r) / PAGE as int + (if ext { 1int } else { 0 }) - gs,
    ensures
        0 <= count <= cap / PAGE,
        forall|k: int, t: int| 0 <= k < count && r <= t < l
            ==> #[trigger] gpos(o, t, cap) != #[trigger] rep(gs, k, cap),
{
    lemma_cap(cap);
    let c = cap as int;
    let np = (cap / PAGE) as int;
    let a = o % 4096;
    let g0 = o / 4096;
    let hr = (o + r) / 4096;
    assert(o == 4096 * g0 + a);
    assert(c == 4096 * np);
    assert(0 <= count <= np);
    assert forall|k: int, t: int| 0 <= k < count && r <= t < l
        implies #[trigger] gpos(o, t, cap) != #[trigger] rep(gs, k, cap) by {
        assert((o + t) / 4096 == g0 + (a + t) / 4096);
        assert((4096 * (gs + k)) / 4096 == gs + k);
        lemma_mod_distinct((o + t) / 4096, gs + k, np);
    }
}

/// And every byte `x` the read took whose page holds no unread byte is in a reported page.
proof fn lemma_finished_one(o: int, r: int, l: int, cap: usize, ext: bool, excl: bool, gs: int, count: int, x: int)
    requires
        cap_ok(cap),
        0 <= o,
        0 <= r <= l <= cap,
        ext == (r == l && (o + r) % PAGE as int != 0 && r != 0),
        excl == ((o + r) / PAGE as int + (if ext { 1int } else { 0 }) - o / PAGE as int > 0 && l + o % PAGE as int > cap),
        gs == o / PAGE as int + if excl { 1int } else { 0 },
        count == (o + r) / PAGE as int + (if ext { 1int } else { 0 }) - gs,
        0 <= x < r,
        forall|t: int| r <= t < l ==> #[trigger] gpos(o, t, cap) != gpos(o, x, cap),
    ensures
        exists|k: int| 0 <= k < count && #[trigger] rep(gs, k, cap) == gpos(o, x, cap),
{
    lemma_cap(cap);
    let c = cap as int;
    let np = (cap / PAGE) as int;
    let a = o % 4096;
    let g0 = o / 4096;
    let hr = (o + r) / 4096;
    assert(o == 4096 * g0 + a);
    assert(c == 4096 * np);
        let gx = (o + x) / 4096;
    assert(gx == g0 + (a + x) / 4096);
    if gx < hr && !(excl && gx == g0) {
        let k = gx - gs;
        assert((4096 * (gs + k)) / 4096 == gs + k);
        assert(0 <= k < count);
            assert(rep(gs, k, cap) == gpos(o, x, cap));
    } else if gx < hr {
        // The first page is dropped only when it is in use, or reported again as the last.
        lemma_mod_multiples_vanish(1, g0, np);
        if r < l {
            let t = if r <= c - a { c - a } else { r };
            assert((o + t) / 4096 == g0 + (a + t) / 4096);
            assert((o + t) / 4096 == g0 + np);
            assert(gpos(o, t, cap) == gpos(o, x, cap));
            assert(false);
        } else {
            assert(hr == g0 + np);
            let k = np - 1;
            assert((4096 * (gs + k)) / 4096 == gs + k);
            assert(0 <= k < count);
            assert(rep(gs, k, cap) == gpos(o, x, cap));
        }
    } else {
        assert(gx == hr);
        if r < l {
            assert(gpos(o, r, cap) == gpos(o, x, cap));
            assert(false);
        } else {
            let k = hr - gs;
            assert((4096 * (gs + k)) / 4096 == gs + k);
            assert(0 <= k < count);
            assert(rep(gs, k, cap) == gpos(o, x, cap));
        }
    }
}

} // verus!

/// Pages a ring of `cap` bytes has.
#[cfg_attr(verus_keep_ghost, verus_spec(r =>
    requires
        cap_ok(cap),
    ensures
        r == cap / PAGE,
        1 <= r,
        r * PAGE == cap,
))]
pub fn pages(cap: usize) -> usize {
    #[cfg(verus_keep_ghost)]
    proof! { lemma_cap(cap); }
    cap / PAGE
}

/// Where byte `pos` lives: (page, offset in the page).
#[cfg_attr(verus_keep_ghost, verus_spec(r =>
    requires
        cap_ok(cap),
    ensures
        r.0 == page_of(pos as int, cap),
        r.1 == pos % PAGE,
        r.0 < cap / PAGE,
        r.1 < PAGE,
        r.0 * PAGE + r.1 == pos % cap,
))]
pub fn locate(pos: usize, cap: usize) -> (usize, usize) {
    #[cfg(verus_keep_ghost)]
    proof! { lemma_page(pos as int, cap); }
    ((pos % cap) / PAGE, pos % PAGE)
}

/// The next piece of a copy at `pos` with `left` bytes to go: (page, offset, bytes), the bytes up to the end of the page
/// or of the copy, whichever is first. Byte `k < bytes` of the piece is at offset `offset + k` of `page`.
#[cfg_attr(verus_keep_ghost, verus_spec(r =>
    requires
        cap_ok(cap),
    ensures
        r.0 == page_of(pos as int, cap),
        r.1 == pos % PAGE,
        r.0 < cap / PAGE,
        r.1 + r.2 <= PAGE,
        r.2 <= left,
        left > 0 ==> r.2 > 0,
        r.2 == left || r.1 + r.2 == PAGE,
        forall|k: int| 0 <= k < r.2 ==> page_of(at(pos, k), cap) == r.0 && #[trigger] at(pos, k) % PAGE as int == r.1 + k,
))]
pub fn chunk(pos: usize, left: usize, cap: usize) -> (usize, usize, usize) {
    let (page, offset) = locate(pos, cap);
    let room = PAGE - offset;
    let bytes = if left < room { left } else { room };
    #[cfg(verus_keep_ghost)]
    proof! {
        assert forall|k: int| 0 <= k < bytes implies page_of(at(pos, k), cap) == page && #[trigger] at(pos, k) % PAGE as int == offset + k by {
            lemma_cap(cap);
            lemma_wrap();
            lemma_page_at(pos, k, pos + k, cap);
            lemma_page(pos as int, cap);
            lemma_at_mod(pos, k, pos + k, PAGE as int);
            assert((pos + k) / 4096 == pos / 4096);
        }
    }
    (page, offset, bytes)
}

/// Bytes held between `head` and `tail`.
#[cfg_attr(verus_keep_ghost, verus_spec(r =>
    ensures
        r == dist(head, tail),
))]
pub fn len(head: usize, tail: usize) -> usize {
    tail.wrapping_sub(head)
}

/// Room left in a ring of `cap` bytes.
#[cfg_attr(verus_keep_ghost, verus_spec(r =>
    requires
        dist(head, tail) <= cap,
    ensures
        r + dist(head, tail) == cap,
))]
pub fn free_len(head: usize, tail: usize, cap: usize) -> usize {
    cap - len(head, tail)
}

/// Whether page `page` holds an unread byte of [head, tail): the page must stay allocated.
#[cfg_attr(verus_keep_ghost, verus_spec(r =>
    requires
        cap_ok(cap),
        page < cap / PAGE,
        dist(head, tail) <= cap,
    ensures
        r == in_use(page as int, head, tail, cap),
))]
pub fn page_in_use(page: usize, head: usize, tail: usize, cap: usize) -> bool {
    #[cfg(verus_keep_ghost)]
    proof! {
        lemma_cap(cap);
        lemma_wrap();
        let np = cap / PAGE;
        assert(page * PAGE + PAGE <= cap) by (nonlinear_arith)
            requires page < np, cap == PAGE * np;
    }
    let held = len(head, tail);
    let start = page * PAGE;
    let h = head % cap;
    // How far the page start is after head, and head after the page start, around the ring.
    let ahead = (start + cap - h) % cap;
    let behind = (h + cap - start) % cap;
    #[cfg(verus_keep_ghost)]
    proof! { lemma_in_use(page as int, head, tail, cap, ahead as int, behind as int); }
    held > 0 && (ahead < held || behind < PAGE)
}

/// The pages a read finished, as (first, count): it moved head from `old_head` to `head`, with the data up to `tail`
/// unread. Page `k < count` is the one holding position `first + k * PAGE` (wrapping; `first` is page aligned), and the
/// kernel frees them. They are exactly the pages the read touched in which no unread byte lives, each once: the page of
/// `old_head` is left out when the data wrapped round into it, and the page of `head` is in when the ring is now empty.
#[cfg_attr(verus_keep_ghost, verus_spec(r =>
    requires
        cap_ok(cap),
        dist(old_head, tail) <= cap,
        dist(old_head, head) <= dist(old_head, tail),
    ensures
        r.0 % PAGE == 0,
        r.1 <= cap / PAGE,
        forall|k: int| 0 <= k < r.1 ==> !in_use(#[trigger] freed(r.0, k, cap), head, tail, cap),
        forall|k1: int, k2: int| 0 <= k1 < r.1 && 0 <= k2 < r.1 && k1 != k2
            ==> #[trigger] freed(r.0, k1, cap) != #[trigger] freed(r.0, k2, cap),
        forall|x: int| 0 <= x < dist(old_head, head) && !in_use(#[trigger] page_of(at(old_head, x), cap), head, tail, cap)
            ==> exists|k: int| 0 <= k < r.1 && #[trigger] freed(r.0, k, cap) == page_of(at(old_head, x), cap),
))]
// `%` rather than `is_multiple_of`, which Verus has no specification for.
#[allow(clippy::manual_is_multiple_of)]
pub fn finished_pages(old_head: usize, head: usize, tail: usize, cap: usize) -> (usize, usize) {
    let start = old_head - old_head % PAGE;
    let head_page = head - head % PAGE;
    // An emptied ring frees the page head stopped in too.
    let end = if head == tail && head % PAGE != 0 && head != old_head {
        head_page.wrapping_add(PAGE)
    } else {
        head_page
    };
    let mut first = start;
    let mut count = end.wrapping_sub(start) / PAGE;
    // Data that wrapped round into the first page keeps it; when the ring is empty, that page is the last one again.
    if count > 0 && tail.wrapping_sub(start) > cap {
        first = start.wrapping_add(PAGE);
        count -= 1;
    }
    #[cfg(verus_keep_ghost)]
    proof! { finished_proof(old_head, head, tail, cap, start, end, first, count); }
    (first, count)
}

#[cfg(verus_keep_ghost)]
verus! {

/// `p` is `dist(o, p)` bytes after `o`.
pub proof fn lemma_after(o: usize, p: usize, i: int)
    ensures
        (p + i) % wrap() == (o + dist(o, p) + i) % wrap(),
{
    if p < o {
        lemma_shift(p + i, 1);
    }
}

/// The counters [`finished_pages`] works on, as distances from `old_head`.
proof fn lemma_counts(old_head: usize, head: usize, tail: usize, cap: usize, f0: usize, end: usize)
    requires
        cap_ok(cap),
        dist(old_head, tail) <= cap,
        dist(old_head, head) <= dist(old_head, tail),
        f0 == old_head - old_head % PAGE,
        end == if head == tail && head % PAGE != 0 && head != old_head {
            (head - head % PAGE + PAGE) % wrap()
        } else {
            head - head % PAGE
        },
    ensures
        ({
            let o = old_head as int;
            let r = dist(old_head, head);
            let l = dist(old_head, tail);
            let ext = r == l && (o + r) % 4096 != 0 && r != 0;
            &&& f0 == 4096 * (o / 4096)
            &&& dist(f0, end) == 4096 * ((o + r) / 4096 + (if ext { 1int } else { 0 }) - o / 4096)
            &&& dist(f0, tail) == l + o % 4096
            &&& dist(head, tail) == l - r
        }),
{
    lemma_cap(cap);
    lemma_wrap();
    let o = old_head as int;
    let r = dist(old_head, head);
    let l = dist(old_head, tail);
    let w = wrap();
    let hw = if o + r < w { 0int } else { 1int };
    assert(head == o + r - w * hw);
    let tw = if o + l < w { 0int } else { 1int };
    assert(tail == o + l - w * tw);
    lemma_shift(head as int, hw);
    lemma_mod_cong(head as int, o + r, w, 4096);
    lemma_fundamental_div_mod_converse(head as int, w, 0, head as int);
    lemma_fundamental_div_mod(o + r, w);
    assert(head as int % 4096 == (o + r) % 4096);
    assert((head == tail) == (r == l));
    assert((head == old_head) == (r == 0));
    let ext = r == l && (o + r) % 4096 != 0 && r != 0;
    let g0 = o / 4096;
    let hr = (o + r) / 4096;
    assert(f0 == 4096 * g0);
    let hp = head - head % 4096;
    assert(hp == 4096 * hr - w * hw);
    let c0 = hr + (if ext { 1int } else { 0 }) - g0;
    assert(0 <= c0 <= cap / PAGE + 1);
    if ext {
        lemma_mod_range(hp + 4096, w);
        assert(end == 4096 * (hr + 1) - w * hw || end == 4096 * (hr + 1) - w * hw - w);
    } else {
        assert(end == 4096 * hr - w * hw);
    }
    assert(dist(f0, end) == 4096 * c0);
    assert(dist(f0, tail) == l + o % 4096);
    assert(dist(head, tail) == l - r);
}

/// Ties the values [`finished_pages`] computes on wrapping counters to [`lemma_finished`] on plain integers.
proof fn finished_proof(old_head: usize, head: usize, tail: usize, cap: usize, f0: usize, end: usize, first: usize, count: usize)
    requires
        cap_ok(cap),
        dist(old_head, tail) <= cap,
        dist(old_head, head) <= dist(old_head, tail),
        f0 == old_head - old_head % PAGE,
        end == if head == tail && head % PAGE != 0 && head != old_head {
            (head - head % PAGE + PAGE) % wrap()
        } else {
            head - head % PAGE
        },
        if dist(f0, end) / PAGE as int > 0 && dist(f0, tail) > cap {
            first == (f0 + PAGE) % wrap() && count == dist(f0, end) / PAGE as int - 1
        } else {
            first == f0 && count == dist(f0, end) / PAGE as int
        },
    ensures
        first % PAGE == 0,
        count <= cap / PAGE,
        forall|k: int| 0 <= k < count ==> !in_use(#[trigger] freed(first, k, cap), head, tail, cap),
        forall|k1: int, k2: int| 0 <= k1 < count && 0 <= k2 < count && k1 != k2
            ==> #[trigger] freed(first, k1, cap) != #[trigger] freed(first, k2, cap),
        forall|x: int| 0 <= x < dist(old_head, head) && !in_use(#[trigger] page_of(at(old_head, x), cap), head, tail, cap)
            ==> exists|k: int| 0 <= k < count && #[trigger] freed(first, k, cap) == page_of(at(old_head, x), cap),
{
    lemma_cap(cap);
    lemma_wrap();
    lemma_counts(old_head, head, tail, cap, f0, end);
    let o = old_head as int;
    let r = dist(old_head, head);
    let l = dist(old_head, tail);
    let np = (cap / PAGE) as int;
    let g0 = o / 4096;
    let ext = r == l && (o + r) % 4096 != 0 && r != 0;
    let c0 = (o + r) / 4096 + (if ext { 1int } else { 0 }) - g0;
    let excl = c0 > 0 && l + o % 4096 > cap;
    let gs = g0 + if excl { 1int } else { 0 };
    assert(dist(f0, end) / PAGE as int == c0);
    assert(count == c0 - if excl { 1int } else { 0 });
    lemma_finished(o, r, l, cap, ext, excl, gs, count as int);
    let fw = if first == 4096 * gs { 0int } else { 1int };
    if excl {
        lemma_mod_range(f0 + 4096, wrap());
    }
    assert(first + wrap() * fw == 4096 * gs);
    // The reported pages, the unread bytes and the bytes read, on global page numbers.
    assert forall|k: int| #![trigger freed(first, k, cap)] 0 <= k implies freed(first, k, cap) == rep(gs, k, cap) by {
        lemma_shift(first + k * 4096, fw);
        lemma_page_at(first, k * PAGE, 4096 * (gs + k), cap);
    }
    assert forall|i: int| #![trigger at(head, i)] 0 <= i implies page_of(at(head, i), cap) == gpos(o, r + i, cap) by {
        lemma_after(old_head, head, i);
        lemma_page_at(head, i, o + r + i, cap);
    }
    assert forall|x: int| #![trigger at(old_head, x)] 0 <= x implies page_of(at(old_head, x), cap) == gpos(o, x, cap) by {
        lemma_page_at(old_head, x, o + x, cap);
    }
    assert(first % PAGE == 0) by {
        lemma_shift(first as int, fw);
        lemma_mod_cong(first as int, 4096 * gs, wrap(), 4096);
        lemma_mod_multiples_vanish(gs, 0, 4096);
    }
    assert forall|k: int| 0 <= k < count implies !in_use(#[trigger] freed(first, k, cap), head, tail, cap) by {
        assert forall|i: int| 0 <= i < dist(head, tail) implies page_of(#[trigger] at(head, i), cap) != freed(first, k, cap) by {
            assert(gpos(o, r + i, cap) != rep(gs, k, cap));
        }
    }
    assert forall|k1: int, k2: int| 0 <= k1 < count && 0 <= k2 < count && k1 != k2
        implies #[trigger] freed(first, k1, cap) != #[trigger] freed(first, k2, cap) by {
        assert((4096 * (gs + k1)) / 4096 == gs + k1);
        assert((4096 * (gs + k2)) / 4096 == gs + k2);
        lemma_mod_distinct(gs + k1, gs + k2, np);
    }
    assert forall|x: int| 0 <= x < dist(old_head, head) && !in_use(#[trigger] page_of(at(old_head, x), cap), head, tail, cap)
        implies exists|k: int| 0 <= k < count && #[trigger] freed(first, k, cap) == page_of(at(old_head, x), cap) by {
        assert forall|t: int| r <= t < l implies #[trigger] gpos(o, t, cap) != gpos(o, x, cap) by {
            assert(page_of(at(head, t - r), cap) == gpos(o, t, cap));
        }
        lemma_finished_one(o, r, l, cap, ext, excl, gs, count as int, x);
        let k = choose|k: int| 0 <= k < count && #[trigger] rep(gs, k, cap) == gpos(o, x, cap);
        assert(freed(first, k, cap) == rep(gs, k, cap));
    }
}

} // verus!

#[cfg(test)]
mod tests {
    use super::*;

    /// The pages `finished_pages` reports, as a mask of ring pages; panics on a page reported twice.
    fn finished_mask(old_head: usize, head: usize, tail: usize, cap: usize) -> u64 {
        let (first, count) = finished_pages(old_head, head, tail, cap);
        assert_eq!(first % PAGE, 0);
        let mut mask = 0u64;
        for k in 0..count {
            let page = locate(first.wrapping_add(k * PAGE), cap).0;
            assert!(mask & 1 << page == 0, "page {page} reported twice");
            mask |= 1 << page;
        }
        mask
    }

    #[test]
    fn pages_of_a_ring() {
        assert_eq!(pages(4096), 1);
        assert_eq!(pages(65536), 16);
        assert_eq!(pages(MAX_CAP), 262144);
    }

    #[test]
    fn a_position_lives_in_its_slot() {
        let cap = 65536;
        assert_eq!(locate(0, cap), (0, 0));
        assert_eq!(locate(4095, cap), (0, 4095));
        assert_eq!(locate(4096, cap), (1, 0));
        assert_eq!(locate(cap - 1, cap), (15, 4095));
        assert_eq!(locate(cap, cap), (0, 0));
        assert_eq!(locate(cap + 4097, cap), (1, 1));
    }

    #[test]
    fn positions_wrap_at_usize_max() {
        let cap = 65536;
        assert_eq!(locate(usize::MAX, cap), (15, 4095));
        assert_eq!(locate(usize::MAX.wrapping_add(1), cap), (0, 0));
        assert_eq!(locate(usize::MAX - 4096, cap), (14, 4095));
    }

    #[test]
    fn a_chunk_stops_at_the_page_end_or_the_copy_end() {
        let cap = 65536;
        assert_eq!(chunk(0, 4096, cap), (0, 0, 4096));
        assert_eq!(chunk(0, 10_000, cap), (0, 0, 4096));
        assert_eq!(chunk(4095, 2, cap), (0, 4095, 1));
        assert_eq!(chunk(4096 + 10, 5, cap), (1, 10, 5));
        assert_eq!(chunk(100, 0, cap), (0, 100, 0));
        assert_eq!(chunk(usize::MAX, 7, cap), (15, 4095, 1));
    }

    /// 10000 bytes copied from just below `usize::MAX`: every piece in one page, together exactly the copy, and the bytes
    /// land in consecutive slots around the ring.
    #[test]
    fn a_copy_across_the_wrap_goes_piece_by_piece() {
        let cap = 16384;
        let mut pos = usize::MAX - 5000;
        let mut left = 10_000;
        let mut copied = 0;
        let mut pieces = 0;
        while left > 0 {
            let (page, offset, bytes) = chunk(pos, left, cap);
            assert!(bytes > 0 && bytes <= left && offset + bytes <= PAGE);
            assert_eq!(bytes, left.min(PAGE - offset));
            assert_eq!(
                page * PAGE + offset,
                (copied + (usize::MAX - 5000) % cap) % cap
            );
            pos = pos.wrapping_add(bytes);
            left -= bytes;
            copied += bytes;
            pieces += 1;
        }
        assert_eq!(copied, 10_000);
        assert_eq!(pieces, 4);
    }

    #[test]
    fn length_and_room() {
        let cap = 65536;
        assert_eq!((len(0, 100), free_len(0, 100, cap)), (100, cap - 100));
        assert_eq!((len(7, 7), free_len(7, 7, cap)), (0, cap));
        let head = usize::MAX - 10;
        let tail = head.wrapping_add(100);
        assert_eq!(
            (len(head, tail), free_len(head, tail, cap)),
            (100, cap - 100)
        );
    }

    #[test]
    fn a_full_ring_has_no_room() {
        let cap = 65536;
        assert_eq!((len(0, cap), free_len(0, cap, cap)), (cap, 0));
        let head = usize::MAX - 300;
        let tail = head.wrapping_add(cap);
        assert_eq!((len(head, tail), free_len(head, tail, cap)), (cap, 0));
        for held in [0, 1, 4096, cap - 1, cap] {
            assert_eq!(
                len(head, head.wrapping_add(held)) + free_len(head, head.wrapping_add(held), cap),
                cap
            );
        }
    }

    #[test]
    fn pages_in_use() {
        let cap = 16384;
        let used = |head: usize, tail: usize| -> [bool; 4] {
            core::array::from_fn(|p| page_in_use(p, head, tail, cap))
        };
        // Data in page 1 only, from its start or from its middle.
        assert_eq!(used(4096, 8192), [false, true, false, false]);
        assert_eq!(used(4096 + 100, 4096 + 200), [false, true, false, false]);
        // Data from page 3 round into page 0.
        assert_eq!(used(3 * 4096 + 5, 4 * 4096 + 5), [true, false, false, true]);
        // An empty ring holds nothing, not even in the page of head.
        assert_eq!(used(4096 + 100, 4096 + 100), [false; 4]);
        // A full ring holds every page, also when head is mid-page.
        assert_eq!(used(0, cap), [true; 4]);
        assert_eq!(used(100, 100 + cap), [true; 4]);
        // Head just below usize::MAX, the data wrapped past it.
        let head = usize::MAX - 100;
        assert_eq!(
            used(head, head.wrapping_add(1000)),
            [true, false, false, true]
        );
    }

    #[test]
    fn a_read_inside_a_page_finishes_nothing() {
        assert_eq!(finished_mask(100, 200, 300, 16384), 0);
        assert_eq!(finished_mask(100, 100, 300, 16384), 0);
    }

    #[test]
    fn a_read_across_a_page_end_finishes_the_page_left() {
        assert_eq!(finished_mask(100, 4096 + 10, 8192, 16384), 0b0001);
        assert_eq!(finished_mask(0, 4096, 12288, 16384), 0b0001);
        assert_eq!(finished_mask(100, 3 * 4096, 3 * 4096 + 1, 16384), 0b0111);
    }

    #[test]
    fn a_read_that_empties_the_ring_finishes_the_page_head_stopped_in() {
        assert_eq!(finished_mask(100, 4096 + 10, 4096 + 10, 16384), 0b0011);
        assert_eq!(finished_mask(100, 200, 200, 16384), 0b0001);
        // Stopping on a page boundary leaves no page behind head.
        assert_eq!(finished_mask(100, 4096, 4096, 16384), 0b0001);
    }

    /// Cap of one page: the tail has wrapped round into the page the read started in, which keeps its new bytes.
    #[test]
    fn data_wrapped_into_the_first_page_keeps_it() {
        assert_eq!(finished_pages(100, 4096, 4096 + 50, 4096).1, 0);
        assert_eq!(
            finished_mask(100, 2 * 4096 + 10, 4 * 4096 + 50, 16384),
            0b0010
        );
    }

    /// A full ring read to empty from mid-page frees every page once.
    #[test]
    fn a_full_ring_read_to_empty_frees_every_page_once() {
        let cap = 16384;
        assert_eq!(finished_mask(100, 100 + cap, 100 + cap, cap), 0b1111);
        let head = usize::MAX - 5000;
        assert_eq!(
            finished_mask(head, head.wrapping_add(cap), head.wrapping_add(cap), cap),
            0b1111
        );
    }

    /// Every read on a ring of four pages, from heads around 0 and around `usize::MAX`: the reported pages are exactly the
    /// pages the read touched that hold no unread byte, each once.
    #[test]
    fn finished_pages_are_exactly_the_touched_pages_not_in_use() {
        let cap = 16384;
        let steps = [0, 1, 99, 4095, 4096, 4097, 8191, 8192, 12000, 16383, 16384];
        for base in [
            0,
            100,
            4095,
            4096,
            9000,
            usize::MAX - 16383,
            usize::MAX - 5000,
            usize::MAX,
        ] {
            for &held in &steps {
                for &taken in &steps {
                    if taken > held {
                        continue;
                    }
                    let tail = base.wrapping_add(held);
                    let head = base.wrapping_add(taken);
                    let mut touched = 0u64;
                    for i in 0..taken {
                        touched |= 1 << locate(base.wrapping_add(i), cap).0;
                    }
                    let mut in_use = 0u64;
                    for p in 0..pages(cap) {
                        if page_in_use(p, head, tail, cap) {
                            in_use |= 1 << p;
                        }
                    }
                    let mut unread = 0u64;
                    for i in 0..held - taken {
                        unread |= 1 << locate(head.wrapping_add(i), cap).0;
                    }
                    assert_eq!(in_use, unread, "base {base} held {held} taken {taken}");
                    assert_eq!(
                        finished_mask(base, head, tail, cap),
                        touched & !in_use,
                        "base {base} held {held} taken {taken}"
                    );
                }
            }
        }
    }
}
