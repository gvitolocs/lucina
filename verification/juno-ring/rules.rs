// Verus proof of the SHIPPED code, not a model (project rule D-011): this file names the source file, and Verus checks it as the
// kernel compiles it. What is proved of each function is in its `ensures`; what follows adds, on top of those, that a ring
// copying through `chunk` gives back exactly the bytes written, in order.
#![feature(proc_macro_hygiene)]
#[path = "../../kernel/lucina-ring/src/lib.rs"]
pub mod ring;

use ring::*;
use vstd::prelude::*;

verus! {

/// Where position `pos` lives, (page, offset): what `locate` returns.
pub open spec fn slot(pos: int, cap: usize) -> (int, int) {
    (page_of(pos, cap), pos % PAGE as int)
}

/// The piece of a copy at `pos` with `left` to go: what `chunk` returns.
pub open spec fn piece(pos: usize, left: int, cap: usize) -> (int, int, int) {
    let off = pos % PAGE;
    (page_of(pos as int, cap), off as int, if left < PAGE - off { left } else { PAGE - off })
}

/// The ring's state: the counters, the bytes held (in order), and the pages' bytes, a map from (page, offset).
pub struct Ring {
    pub head: usize,
    pub tail: usize,
    pub contents: Seq<u8>,
    pub storage: spec_fn((int, int)) -> u8,
}

/// Byte `i` of the contents is what the page holds at the slot of position `head + i`.
pub open spec fn inv(m: Ring, cap: usize) -> bool {
    &&& dist(m.head, m.tail) == m.contents.len()
    &&& m.contents.len() <= cap
    &&& forall|i: int| 0 <= i < m.contents.len() ==> #[trigger] (m.storage)(slot(at(m.head, i), cap)) == m.contents[i]
}

/// `bytes` copied into `page` from offset `off`.
pub open spec fn put(s: spec_fn((int, int)) -> u8, page: int, off: int, bytes: Seq<u8>) -> spec_fn((int, int)) -> u8 {
    |k: (int, int)| if k.0 == page && off <= k.1 < off + bytes.len() { bytes[k.1 - off] } else { s(k) }
}

/// A write of `data` at `pos`, one `chunk` at a time.
pub open spec fn store(s: spec_fn((int, int)) -> u8, pos: usize, data: Seq<u8>, cap: usize) -> spec_fn((int, int)) -> u8
    decreases data.len(),
{
    if data.len() == 0 || !cap_ok(cap) {
        s
    } else {
        let (page, off, n) = piece(pos, data.len() as int, cap);
        store(put(s, page, off, data.take(n)), at(pos, n) as usize, data.skip(n), cap)
    }
}

/// A read of `n` bytes at `pos`, one `chunk` at a time.
pub open spec fn load(s: spec_fn((int, int)) -> u8, pos: usize, n: int, cap: usize) -> Seq<u8>
    decreases n,
{
    if n <= 0 || !cap_ok(cap) {
        Seq::empty()
    } else {
        let (page, off, b) = piece(pos, n, cap);
        Seq::new(b as nat, |k: int| s((page, off + k))) + load(s, at(pos, b) as usize, n - b, cap)
    }
}

/// Writing `data` at tail; refused when it does not fit.
pub open spec fn write(m: Ring, data: Seq<u8>, cap: usize) -> Option<Ring> {
    if data.len() > cap - dist(m.head, m.tail) {
        None
    } else {
        Some(Ring {
            head: m.head,
            tail: at(m.tail, data.len() as int) as usize,
            contents: m.contents + data,
            storage: store(m.storage, m.tail, data, cap),
        })
    }
}

/// Reading `n` bytes at head: what is returned, and the ring after.
pub open spec fn read(m: Ring, n: int, cap: usize) -> (Seq<u8>, Ring) {
    (
        load(m.storage, m.head, n, cap),
        Ring { head: at(m.head, n) as usize, tail: m.tail, contents: m.contents.skip(n), storage: m.storage },
    )
}

/// `chunk` returns `piece`.
fn chunk_is_piece(pos: usize, left: usize, cap: usize)
    requires
        cap_ok(cap),
{
    let r = chunk(pos, left, cap);
    assert((r.0 as int, r.1 as int, r.2 as int) == piece(pos, left as int, cap));
    let l = locate(pos, cap);
    assert((l.0 as int, l.1 as int) == slot(pos as int, cap));
}

pub proof fn lemma_at_at(b: usize, i: int, j: int)
    requires
        0 <= i,
        0 <= j,
    ensures
        at(at(b, i) as usize, j) == at(b, i + j),
        0 <= at(b, i) < wrap(),
{
    lemma_wrap();
    vstd::arithmetic::div_mod::lemma_fundamental_div_mod(b + i, wrap());
    lemma_shift(at(b, i) + j, (b + i) / wrap());
}

/// No overlap: positions fewer than `cap` bytes apart live in different slots.
pub proof fn lemma_no_overlap_at(b: usize, i: int, j: int, cap: usize)
    requires
        cap_ok(cap),
        0 <= i < j,
        j - i < cap,
    ensures
        slot(at(b, i), cap) != slot(at(b, j), cap),
{
    lemma_cap(cap);
    lemma_wrap();
    lemma_at_at(b, i, 0);
    lemma_at_at(b, j, 0);
    lemma_page(at(b, i), cap);
    lemma_page(at(b, j), cap);
    lemma_at_mod(b, i, b + i, cap as int);
    lemma_at_mod(b, j, b + j, cap as int);
    lemma_mod_distinct(b + j, b + i, cap as int);
}

/// No overlap, on two counters: `p != q` with `q` fewer than `cap` bytes after `p` live in different slots.
pub proof fn lemma_no_overlap(p: usize, q: usize, cap: usize)
    requires
        cap_ok(cap),
        p != q,
        dist(p, q) < cap,
    ensures
        slot(p as int, cap) != slot(q as int, cap),
{
    lemma_wrap();
    lemma_after(p, q, 0);
    lemma_mod_range(q as int, wrap());
    lemma_mod_range(p as int, wrap());
    lemma_at_at(p, 0, 0);
    lemma_no_overlap_at(p, 0, dist(p, q), cap);
}

/// The same on what `locate` returns.
fn no_overlap(p: usize, q: usize, cap: usize)
    requires
        cap_ok(cap),
        p != q,
        dist(p, q) < cap,
{
    let a = locate(p, cap);
    let b = locate(q, cap);
    proof {
        lemma_no_overlap(p, q, cap);
    }
    assert(a != b);
}

/// A write through `chunk`: byte `j` of `data` lands at the slot of `pos + j`, and nothing else changes.
pub proof fn lemma_store(s: spec_fn((int, int)) -> u8, pos: usize, data: Seq<u8>, cap: usize)
    requires
        cap_ok(cap),
        data.len() <= cap,
    ensures
        forall|j: int| 0 <= j < data.len() ==> #[trigger] store(s, pos, data, cap)(slot(at(pos, j), cap)) == data[j],
        forall|k: (int, int)| (forall|j: int| 0 <= j < data.len() ==> k != #[trigger] slot(at(pos, j), cap))
            ==> #[trigger] store(s, pos, data, cap)(k) == s(k),
    decreases data.len(),
{
    if data.len() > 0 {
        lemma_wrap();
        let (page, off, n) = piece(pos, data.len() as int, cap);
        lemma_piece_len(pos, data.len() as int, cap);
        let s2 = put(s, page, off, data.take(n));
        let next = at(pos, n) as usize;
        let rest = data.skip(n);
        let out = store(s, pos, data, cap);
        assert(out == store(s2, next, rest, cap));
        // The piece stays in its page: byte k < n of it is at the slot of `pos + k`.
        assert forall|k: int| 0 <= k < n implies #[trigger] slot(at(pos, k), cap) == (page, off + k) by {
            chunk_slots(pos, data.len() as int, cap, k);
        }
        lemma_store(s2, next, rest, cap);
        assert forall|j: int| 0 <= j < rest.len() implies #[trigger] slot(at(next, j), cap) == slot(at(pos, n + j), cap) by {
            lemma_at_at(pos, n, j);
        }
        assert forall|j: int| 0 <= j < data.len() implies #[trigger] out(slot(at(pos, j), cap)) == data[j] by {
            if j < n {
                let key = slot(at(pos, j), cap);
                assert forall|j2: int| 0 <= j2 < rest.len() implies key != #[trigger] slot(at(next, j2), cap) by {
                    lemma_no_overlap_at(pos, j, n + j2, cap);
                }
                assert(out(key) == s2(key));
                assert(key == (page, off + j));
                assert(s2(key) == data.take(n)[j]);
            } else {
                assert(slot(at(next, j - n), cap) == slot(at(pos, j), cap));
                assert(out(slot(at(next, j - n), cap)) == rest[j - n]);
            }
        }
        assert forall|k: (int, int)| (forall|j: int| 0 <= j < data.len() ==> k != #[trigger] slot(at(pos, j), cap)) implies
            #[trigger] out(k) == s(k) by {
            assert forall|j: int| 0 <= j < rest.len() implies k != #[trigger] slot(at(next, j), cap) by {
                assert(k != slot(at(pos, n + j), cap));
            }
            if k.0 == page && off <= k.1 < off + n {
                assert(k == slot(at(pos, k.1 - off), cap));
            }
        }
    }
}

/// Byte `k` of a piece is at the slot of `pos + k`, by `chunk`'s promise.
proof fn chunk_slots(pos: usize, left: int, cap: usize, k: int)
    requires
        cap_ok(cap),
        0 < left,
        0 <= k < piece(pos, left, cap).2,
    ensures
        slot(at(pos, k), cap) == (piece(pos, left, cap).0, piece(pos, left, cap).1 + k),
{
    lemma_cap(cap);
    lemma_wrap();
    lemma_page_at(pos, k, pos + k, cap);
    lemma_page(pos as int, cap);
    lemma_at_mod(pos, k, pos + k, PAGE as int);
    assert((pos + k) / 4096 == pos / 4096);
}

/// A read through `chunk`: byte `j` returned is what the slot of `pos + j` holds.
pub proof fn lemma_load(s: spec_fn((int, int)) -> u8, pos: usize, n: int, cap: usize)
    requires
        cap_ok(cap),
        0 <= n,
    ensures
        load(s, pos, n, cap).len() == n,
        forall|j: int| 0 <= j < n ==> #[trigger] load(s, pos, n, cap)[j] == s(slot(at(pos, j), cap)),
    decreases n,
{
    if n > 0 {
        let (page, off, b) = piece(pos, n, cap);
        let next = at(pos, b) as usize;
        assert forall|k: int| 0 <= k < b implies #[trigger] slot(at(pos, k), cap) == (page, off + k) by {
            chunk_slots(pos, n, cap, k);
        }
        lemma_piece_len(pos, n, cap);
        lemma_load(s, next, n - b, cap);
        assert forall|j: int| 0 <= j < n implies #[trigger] load(s, pos, n, cap)[j] == s(slot(at(pos, j), cap)) by {
            if j >= b {
                lemma_at_at(pos, b, j - b);
            }
        }
    }
}

/// A piece of a copy that has bytes to go takes at least one and at most what is left.
proof fn lemma_piece_len(pos: usize, n: int, cap: usize)
    requires
        0 < n,
    ensures
        0 < piece(pos, n, cap).2 <= n,
{
}

/// A write that fits keeps the invariant and the capacity; one that does not is refused.
pub proof fn lemma_write(m: Ring, data: Seq<u8>, cap: usize)
    requires
        cap_ok(cap),
        inv(m, cap),
    ensures
        write(m, data, cap) is None <==> data.len() > cap - dist(m.head, m.tail),
        write(m, data, cap) matches Some(m2) ==> inv(m2, cap) && m2.contents.len() <= cap
            && m2.contents == m.contents + data,
{
    if data.len() <= cap - dist(m.head, m.tail) {
        lemma_wrap();
        let m2 = write(m, data, cap).unwrap();
        let len = m.contents.len() as int;
        lemma_store(m.storage, m.tail, data, cap);
        lemma_after(m.head, m.tail, 0);
        lemma_mod_range(m.tail as int, wrap());
        lemma_at_at(m.head, 0, len);
        // tail is head + len, so the new tail is head + len + data.len().
        assert(m.tail as int == at(m.head, len));
        lemma_at_at(m.head, len, data.len() as int);
        lemma_at_at(m.tail, data.len() as int, 0);
        lemma_after(m.head, m2.tail, 0);
        lemma_mod_range(m2.tail as int, wrap());
        lemma_mod_eq(dist(m.head, m2.tail), len + data.len(), wrap());
        assert forall|i: int| 0 <= i < m2.contents.len() implies #[trigger] (m2.storage)(slot(at(m2.head, i), cap)) == m2.contents[i] by {
            if i < len {
                assert forall|j: int| 0 <= j < data.len() implies slot(at(m.head, i), cap) != #[trigger] slot(at(m.tail, j), cap) by {
                    lemma_at_at(m.head, len, j);
                    lemma_no_overlap_at(m.head, i, len + j, cap);
                }
            } else {
                lemma_at_at(m.head, len, i - len);
                assert(slot(at(m.tail, i - len), cap) == slot(at(m.head, i), cap));
            }
        }
    }
}

/// A read of at most what is held returns the oldest bytes, in order, and keeps the invariant.
pub proof fn lemma_read(m: Ring, n: int, cap: usize)
    requires
        cap_ok(cap),
        inv(m, cap),
        0 <= n <= m.contents.len(),
    ensures
        read(m, n, cap).0 == m.contents.take(n),
        inv(read(m, n, cap).1, cap),
{
    lemma_read_bytes(m, n, cap);
    lemma_read_keeps(m, n, cap);
}

proof fn lemma_read_bytes(m: Ring, n: int, cap: usize)
    requires
        cap_ok(cap),
        inv(m, cap),
        0 <= n <= m.contents.len(),
    ensures
        read(m, n, cap).0 == m.contents.take(n),
{
    let got = read(m, n, cap).0;
    lemma_load(m.storage, m.head, n, cap);
    assert forall|j: int| 0 <= j < n implies #[trigger] got[j] == m.contents.take(n)[j] by {
        assert(got[j] == (m.storage)(slot(at(m.head, j), cap)));
    }
    assert(got =~= m.contents.take(n));
}

proof fn lemma_read_keeps(m: Ring, n: int, cap: usize)
    requires
        cap_ok(cap),
        inv(m, cap),
        0 <= n <= m.contents.len(),
    ensures
        inv(read(m, n, cap).1, cap),
{
    lemma_wrap();
    let m2 = read(m, n, cap).1;
    let len = m.contents.len() as int;
    lemma_at_at(m.head, n, 0);
    lemma_after(m.head, m.tail, 0);
    lemma_mod_range(m.tail as int, wrap());
    lemma_at_at(m.head, 0, len);
    lemma_after(m2.head, m.tail, 0);
    lemma_at_at(m.head, n, len - n);
    lemma_mod_eq(dist(m2.head, m.tail), len - n, wrap());
    assert forall|i: int| 0 <= i < m2.contents.len() implies #[trigger] (m2.storage)(slot(at(m2.head, i), cap)) == m2.contents[i] by {
        lemma_at_at(m.head, n, i);
        assert((m.storage)(slot(at(m.head, n + i), cap)) == m.contents[n + i]);
    }
}

/// End to end: bytes written into an empty ring come back exactly, in order.
pub proof fn lemma_fifo(head: usize, s: spec_fn((int, int)) -> u8, data: Seq<u8>, cap: usize)
    requires
        cap_ok(cap),
        data.len() <= cap,
    ensures
        ({
            let m = Ring { head, tail: head, contents: Seq::empty(), storage: s };
            let m2 = write(m, data, cap).unwrap();
            read(m2, data.len() as int, cap).0 == data
        }),
{
    let m = Ring { head, tail: head, contents: Seq::empty(), storage: s };
    lemma_write(m, data, cap);
    let m2 = write(m, data, cap).unwrap();
    assert(m2.contents =~= data);
    lemma_read(m2, data.len() as int, cap);
    assert(data.take(data.len() as int) =~= data);
}

/// The ring through the shipped counters: room is `free_len`, a write beyond it is refused, and room plus length is the
/// capacity.
fn room(m: Ghost<Ring>, head: usize, tail: usize, n: usize, cap: usize)
    requires
        cap_ok(cap),
        inv(m@, cap),
        m@.head == head,
        m@.tail == tail,
{
    let held = len(head, tail);
    let free = free_len(head, tail, cap);
    assert(free + held == cap);
    proof {
        lemma_write(m@, Seq::new(n as nat, |i: int| 0u8), cap);
    }
    if n > free {
        assert(write(m@, Seq::new(n as nat, |i: int| 0u8), cap) is None);
    } else {
        assert(write(m@, Seq::new(n as nat, |i: int| 0u8), cap).unwrap().contents.len() <= cap);
    }
}

} // verus!

fn main() {}
