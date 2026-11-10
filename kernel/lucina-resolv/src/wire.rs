//! DNS messages as lucina-resolv sends and accepts them, as plain checks on bytes. Verus proves this very file, not a copy of it
//! (`verification/juno-resolv/rules.rs`; rustc ignores the attributes). What each function promises is in its `ensures`.
//!
//! The rules are musl's (`src/network/res_mkquery.c`, `res_msend.c`, `dns_parse.c`): a name of at most 253 bytes in labels of 1
//! to 63, one question, recursion desired; a reply counts only if its id, its QR bit and its whole question are the query's,
//! byte for byte, so that a stray or forged datagram for another question is dropped.
//!
//! Not proved here: the sockets, the timing and the retries (`lib.rs`), or that a server tells the truth.
#![allow(unexpected_cfgs)]

#[cfg(verus_keep_ghost)]
use vstd::prelude::*;

/// The fixed header of every DNS message.
#[cfg_attr(verus_keep_ghost, verus_verify)]
pub const HEADER: usize = 12;
/// Record types and the class this resolver asks for.
#[cfg_attr(verus_keep_ghost, verus_verify)]
pub const TYPE_A: u16 = 1;
#[cfg_attr(verus_keep_ghost, verus_verify)]
pub const TYPE_AAAA: u16 = 28;
#[cfg_attr(verus_keep_ghost, verus_verify)]
pub const CLASS_IN: u16 = 1;
/// The longest name, without its final dot, and the longest label.
#[cfg_attr(verus_keep_ghost, verus_verify)]
pub const MAX_NAME: usize = 253;
#[cfg_attr(verus_keep_ghost, verus_verify)]
pub const MAX_LABEL: usize = 63;
/// At most this many answers are read from one reply.
#[cfg_attr(verus_keep_ghost, verus_verify)]
pub const MAX_ANSWERS: usize = 64;

/// One address from an answer: `len` 4 for A, 16 for AAAA; `value` holds its bytes big-endian in the low `len` bytes.
#[cfg_attr(verus_keep_ghost, verus_verify)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Addr {
    pub len: u8,
    pub value: u128,
}

/// What a reply that matched its query says.
#[cfg_attr(verus_keep_ghost, verus_verify)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Answer {
    /// RCODE 0; the addresses may be none (the name exists, with no record of this type).
    Found,
    /// RCODE 3: the name does not exist.
    NoName,
    /// RCODE 2 (SERVFAIL): ask again, as musl does.
    Retry,
    /// Any other RCODE, or a reply cut short (TC): no answer from this server.
    Failed,
}

#[cfg(verus_keep_ghost)]
verus! {

/// How many bytes, none a dot, end at `i` (0 when `n[i]` is a dot or `i` is outside the name).
pub open spec fn run_at(n: Seq<u8>, i: int) -> int
    decreases i + 1,
{
    if i < 0 || i >= n.len() {
        0
    } else if n[i] == 0x2e {
        0
    } else {
        run_at(n, i - 1) + 1
    }
}

/// A name's bytes form labels of 1 to 63 bytes, joined by single dots, with no dot at either end, 253 bytes at most.
pub open spec fn valid_name(n: Seq<u8>) -> bool {
    &&& 1 <= n.len() <= 253
    &&& n[0] != 0x2e
    &&& n[n.len() - 1] != 0x2e
    &&& forall|i: int| 0 <= i < n.len() - 1 ==> !(#[trigger] n[i] == 0x2e && n[i + 1] == 0x2e)
    &&& forall|i: int| 0 <= i < n.len() ==> #[trigger] run_at(n, i) <= 63
}

/// A reply's header and question are the query's: same id, QR set, one question, and every byte of the question the same.
pub open spec fn matches_query(reply: Seq<u8>, query: Seq<u8>) -> bool {
    &&& query.len() >= 12
    &&& reply.len() >= query.len()
    &&& reply[0] == query[0]
    &&& reply[1] == query[1]
    &&& reply[2] & 0x80 != 0
    &&& reply[4] == 0
    &&& reply[5] == 1
    &&& forall|i: int| 12 <= i < query.len() ==> #[trigger] reply[i] == query[i]
}

} // verus!

/// Is `name` (no final dot) a name a query may carry? See `valid_name`.
#[cfg_attr(verus_keep_ghost, verus_spec(r =>
    ensures
        r == valid_name(name@),
))]
pub fn name_ok(name: &[u8]) -> bool {
    let n = name.len();
    if n == 0 || n > MAX_NAME || name[0] == b'.' || name[n - 1] == b'.' {
        return false;
    }
    // `run` is the length of the label read so far, which ends at i - 1.
    let mut run: usize = 0;
    let mut i: usize = 0;
    #[cfg_attr(verus_keep_ghost, verus_spec(
        invariant
            1 <= n <= 253,
            n == name@.len(),
            i <= n,
            name@[0] != 0x2e,
            name@[n - 1] != 0x2e,
            run as int == run_at(name@, i - 1),
            run <= 63,
            i > 0 && run == 0 ==> name@[i - 1] == 0x2e,
            forall|k: int| 0 <= k < i ==> #[trigger] run_at(name@, k) <= 63,
            forall|k: int| 0 <= k < i - 1 ==> !(#[trigger] name@[k] == 0x2e && name@[k + 1] == 0x2e),
        decreases n - i
    ))]
    while i < n {
        if name[i] == b'.' {
            if run == 0 {
                // A dot right after a dot (a dot at 0 was refused above).
                #[cfg(verus_keep_ghost)]
                proof! { assert(name@[i - 1] == 0x2e && name@[i as int] == 0x2e); }
                return false;
            }
            run = 0;
        } else {
            if run == MAX_LABEL {
                // This byte would make a label of 64.
                #[cfg(verus_keep_ghost)]
                proof! { assert(run_at(name@, i as int) == 64); }
                return false;
            }
            run = run + 1;
        }
        i = i + 1;
    }
    true
}

/// The query for `name` (no final dot) and `qtype`, with id `id` and recursion desired, appended to `out` (empty on entry).
/// `false`, and nothing written, when the name is not one a query may carry.
#[cfg_attr(verus_keep_ghost, verus_spec(r =>
    requires
        old(out)@.len() == 0,
    ensures
        r == valid_name(name@),
        r ==> final(out)@.len() == 12 + name@.len() + 2 + 4,
        r ==> final(out)@[0] == (id >> 8) as u8 && final(out)@[1] == (id & 0xff) as u8,
        r ==> final(out)@[4] == 0 && final(out)@[5] == 1,
        !r ==> final(out)@.len() == 0,
))]
pub fn query(id: u16, name: &[u8], qtype: u16, out: &mut Vec<u8>) -> bool {
    if !name_ok(name) {
        return false;
    }
    let n = name.len();
    // Header: id, RD set, one question, no answers, no authority, no additional records.
    out.push((id >> 8) as u8);
    out.push((id & 0xff) as u8);
    out.push(0x01);
    out.push(0x00);
    out.push(0);
    out.push(1);
    let mut z: usize = 0;
    #[cfg_attr(verus_keep_ghost, verus_spec(
        invariant
            z <= 6,
            out@.len() == 6 + z,
            out@[0] == (id >> 8) as u8,
            out@[1] == (id & 0xff) as u8,
            out@[4] == 0,
            out@[5] == 1,
        decreases 6 - z
    ))]
    while z < 6 {
        out.push(0);
        z = z + 1;
    }
    // The name: each label's length, then its bytes. A dot becomes the next label's length byte, so the name takes n + 2 bytes.
    let mut start: usize = 0;
    #[cfg_attr(verus_keep_ghost, verus_spec(
        invariant
            n == name@.len(),
            1 <= n <= 253,
            valid_name(name@),
            name@[n - 1] != 0x2e,
            start <= n,
            start > 0 ==> name@[start - 1] == 0x2e,
            out@.len() == 12 + start,
            out@[0] == (id >> 8) as u8,
            out@[1] == (id & 0xff) as u8,
            out@[4] == 0,
            out@[5] == 1,
        decreases n - start
    ))]
    while start < n {
        let mut end = start;
        #[cfg_attr(verus_keep_ghost, verus_spec(
            invariant
                start <= end <= n,
                n == name@.len(),
            decreases n - end
        ))]
        while end < n && name[end] != b'.' {
            end = end + 1;
        }
        out.push((end - start) as u8);
        let mut k = start;
        #[cfg_attr(verus_keep_ghost, verus_spec(
            invariant
                start <= k <= end,
                end <= n,
                n == name@.len(),
                out@.len() == 12 + k + 1,
                out@[0] == (id >> 8) as u8,
                out@[1] == (id & 0xff) as u8,
                out@[4] == 0,
                out@[5] == 1,
            decreases end - k
        ))]
        while k < end {
            out.push(name[k]);
            k = k + 1;
        }
        if end == n {
            // The terminating zero, then type and class.
            out.push(0);
            out.push((qtype >> 8) as u8);
            out.push((qtype & 0xff) as u8);
            out.push((CLASS_IN >> 8) as u8);
            out.push((CLASS_IN & 0xff) as u8);
            return true;
        }
        // name[end] is the dot that ended the label.
        start = end + 1;
    }
    // Not reached: a valid name does not end in a dot, so its last label returned above.
    true
}

/// Does `reply` answer `query`? See `matches_query`: id, QR, one question, and the question byte for byte.
#[cfg_attr(verus_keep_ghost, verus_spec(r =>
    ensures
        r == matches_query(reply@, query@),
))]
pub fn matches(reply: &[u8], query: &[u8]) -> bool {
    if query.len() < HEADER || reply.len() < query.len() {
        return false;
    }
    if reply[0] != query[0] || reply[1] != query[1] || reply[2] & 0x80 == 0 || reply[4] != 0 || reply[5] != 1 {
        return false;
    }
    let mut i = HEADER;
    #[cfg_attr(verus_keep_ghost, verus_spec(
        invariant
            12 <= i <= query@.len(),
            query@.len() <= reply@.len(),
            forall|k: int| 12 <= k < i ==> #[trigger] reply@[k] == query@[k],
        decreases query@.len() - i
    ))]
    while i < query.len() {
        if reply[i] != query[i] {
            return false;
        }
        i = i + 1;
    }
    true
}

/// The end of the name that starts at `pos`: past its zero byte, or past a compression pointer (two bytes), which ends a name.
/// `None` if the name runs off the message or has a label length this resolver does not know (0x40 to 0xbf).
#[cfg_attr(verus_keep_ghost, verus_spec(r =>
    ensures
        r matches Some(end) ==> pos < end <= buf@.len(),
))]
pub fn skip_name(buf: &[u8], pos: usize) -> Option<usize> {
    let mut p = pos;
    #[cfg_attr(verus_keep_ghost, verus_spec(
        invariant
            pos <= p,
        decreases buf@.len() - p
    ))]
    while p < buf.len() {
        let b = buf[p];
        if b == 0 {
            return Some(p + 1);
        }
        if b & 0xc0 == 0xc0 {
            if buf.len() - p >= 2 {
                return Some(p + 2);
            }
            return None;
        }
        if b & 0xc0 != 0 {
            return None;
        }
        // A label: its length byte and its bytes.
        if b as usize >= buf.len() - p {
            return None;
        }
        p = p + 1 + b as usize;
    }
    None
}

/// The addresses of type `qtype` in a reply whose question (`qlen` bytes from the start) matched the query, at most
/// `MAX_ANSWERS`, in the order the server gave them; CNAME and other records are passed over, as musl's `dns_parse` does. A
/// reply cut short (TC) is `Failed`: this resolver does not fall back to TCP.
#[cfg_attr(verus_keep_ghost, verus_spec(r =>
    requires
        12 <= qlen <= reply@.len(),
    ensures
        r.0 != Answer::Found ==> r.1@.len() == 0,
        r.1@.len() <= 64,
        forall|i: int| 0 <= i < r.1@.len() ==> (#[trigger] r.1@[i]).len == 4 || r.1@[i].len == 16,
))]
pub fn answers(reply: &[u8], qlen: usize, qtype: u16) -> (Answer, Vec<Addr>) {
    let mut out: Vec<Addr> = Vec::new();
    if reply[2] & 0x02 != 0 {
        return (Answer::Failed, out);
    }
    let rcode = reply[3] & 0x0f;
    if rcode == 3 {
        return (Answer::NoName, out);
    }
    if rcode == 2 {
        return (Answer::Retry, out);
    }
    if rcode != 0 {
        return (Answer::Failed, out);
    }
    let count = ((reply[6] as usize) << 8) | reply[7] as usize;
    let want_len: usize = if qtype == TYPE_AAAA { 16 } else { 4 };
    let mut p = qlen;
    let mut seen: usize = 0;
    #[cfg_attr(verus_keep_ghost, verus_spec(
        invariant
            out@.len() <= 64,
            want_len == 4 || want_len == 16,
            forall|i: int| 0 <= i < out@.len() ==> (#[trigger] out@[i]).len == 4 || out@[i].len == 16,
        decreases count - seen
    ))]
    while seen < count && out.len() < MAX_ANSWERS {
        let end = match skip_name(reply, p) {
            Some(e) => e,
            None => return (Answer::Found, out),
        };
        // Type, class, TTL, data length: 10 bytes.
        if reply.len() - end < 10 {
            return (Answer::Found, out);
        }
        let rtype = ((reply[end] as u16) << 8) | reply[end + 1] as u16;
        let rclass = ((reply[end + 2] as u16) << 8) | reply[end + 3] as u16;
        let rdlen = ((reply[end + 8] as usize) << 8) | reply[end + 9] as usize;
        let data = end + 10;
        if rdlen > reply.len() - data {
            return (Answer::Found, out);
        }
        if rtype == qtype && rclass == CLASS_IN && rdlen == want_len {
            let mut value: u128 = 0;
            let mut k: usize = 0;
            #[cfg_attr(verus_keep_ghost, verus_spec(
                invariant
                    k <= rdlen,
                    rdlen <= 16,
                    reply@.len() <= usize::MAX,
                    data <= reply@.len(),
                    data + rdlen <= reply@.len(),
                decreases rdlen - k
            ))]
            while k < rdlen {
                value = (value << 8u32) | reply[data + k] as u128;
                k = k + 1;
            }
            out.push(Addr { len: rdlen as u8, value });
        }
        p = data + rdlen;
        seen = seen + 1;
    }
    (Answer::Found, out)
}
