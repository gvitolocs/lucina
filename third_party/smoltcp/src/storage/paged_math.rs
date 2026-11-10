// LucinaOS (patch 1053): the arithmetic of a ring buffer kept in pages (`RingBuffer::new_paged`), apart from the buffer
// so that Verus proves this very file (`verification/juno-tcpbuf/rules.rs` in the LucinaOS repository, which includes it
// by path). What each function promises is in its `ensures`; the proof file adds the buffer's accounting and the copy
// loops on top of it.
#![allow(unexpected_cfgs)]

#[cfg(verus_keep_ghost)]
use vstd::prelude::*;

/// Where index `at` of a storage of `cap` elements kept in pages of `page` elements lives, and how many elements follow
/// it without leaving its page or the storage: (page number, offset in the page, that length). Past the end the length is
/// 0. Page `i` holds `min(page, cap - i * page)` elements, the last one fewer when `cap` is not a multiple of `page`.
#[cfg_attr(verus_keep_ghost, verus_spec(r =>
    requires
        page > 0,
    ensures
        at >= cap ==> r.2 == 0,
        at < cap ==> {
            &&& r.0 == at / page
            &&& r.1 == at % page
            &&& r.2 >= 1
            &&& r.1 + r.2 <= page
            &&& at + r.2 <= cap
            &&& r.0 * page + r.1 == at
            &&& r.1 + r.2 <= cap - r.0 * page
            &&& (at + r.2 == cap || (at + r.2) % (page as int) == 0)
        },
))]
pub fn piece_at(at: usize, page: usize, cap: usize) -> (usize, usize, usize) {
    if at >= cap {
        return (0, 0, 0);
    }
    let index = at / page;
    let offset = at % page;
    #[cfg(verus_keep_ghost)]
    proof! {
        vstd::arithmetic::div_mod::lemma_fundamental_div_mod(at as int, page as int);
        vstd::arithmetic::div_mod::lemma_mod_bound(at as int, page as int);
    }
    let to_page_end = page - offset;
    let to_end = cap - at;
    let len = if to_page_end < to_end { to_page_end } else { to_end };
    #[cfg(verus_keep_ghost)]
    proof! {
        if len == to_page_end {
            // at + (page - at % page) == (at / page + 1) * page.
            vstd::arithmetic::mul::lemma_mul_is_distributive_add_other_way(page as int, at as int / page as int, 1);
            vstd::arithmetic::div_mod::lemma_mod_multiples_basic(at as int / page as int + 1, page as int);
        }
    }
    (index, offset, len)
}

/// How a length of `total` is kept in pieces of `page`: the number of pieces (at least one, so that even nothing has a place)
/// and the length of the last. Every piece but the last is a whole page. Used by the kernel's UDP copies (patch 1054).
#[cfg_attr(verus_keep_ghost, verus_spec(r =>
    requires
        page > 0,
    ensures
        r.0 >= 1,
        r.1 <= page,
        (r.0 - 1) * page + r.1 == total,
        total > 0 ==> r.1 >= 1,
))]
pub fn chunk_plan(total: usize, page: usize) -> (usize, usize) {
    if total == 0 {
        #[cfg(verus_keep_ghost)]
        proof! {
            assert(0 * page == 0) by (nonlinear_arith);
        }
        return (1, 0);
    }
    let whole = total / page;
    let rest = total % page;
    #[cfg(verus_keep_ghost)]
    proof! {
        vstd::arithmetic::div_mod::lemma_fundamental_div_mod(total as int, page as int);
        vstd::arithmetic::div_mod::lemma_mod_bound(total as int, page as int);
    }
    if rest == 0 {
        // `whole` pages, the last one full.
        #[cfg(verus_keep_ghost)]
        proof! {
            assert(whole >= 1) by (nonlinear_arith)
                requires whole * page == total, total > 0, page > 0;
            assert((whole - 1) * page + page == whole * page) by (nonlinear_arith)
                requires whole >= 1;
        }
        (whole, page)
    } else {
        #[cfg(verus_keep_ghost)]
        proof! {
            assert(whole <= whole * page) by (nonlinear_arith)
                requires page >= 1;
        }
        (whole + 1, rest)
    }
}

/// `len`, but no more than `cap` (patch 1054: syslog's buffer is at most the log ring).
#[cfg_attr(verus_keep_ghost, verus_spec(r =>
    ensures
        r <= cap,
        r <= len,
        r == len || r == cap,
))]
pub fn capped(len: usize, cap: usize) -> usize {
    if len < cap { len } else { cap }
}
