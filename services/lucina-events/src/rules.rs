// The rules every event on Juno goes through, in the hub (`lucina-eventd`) and in the kernel's ring: how a bounded queue drops and
// counts what it drops, what a reader is told it missed, the order of each source's events, what a filter lets through and which
// fields may leave the program that made them. Verus checks this very file: `//@` lines hold the proof, see
// scripts/verify-shipped.sh (D-011). The hub, the kernel and the clients call these functions and hold no copy of the rules.
//
// After D-Bus's and Android's broadcast model: an event is a name and a few values, sent by whoever owns the change, and nobody is
// told about an event they did not ask for (D-Bus match rules, Android intent filters). Unlike either, a full queue never blocks
// or silently drops: the oldest event goes and every reader that had not read it is told how many it lost (D-Bus disconnects a
// slow client, Android drops broadcasts without saying).
//
// Not proved: that the queue's storage holds the events the window says (the hub's VecDeque and the kernel's ring keep it in step
// and are tested), the JSON, and the loop that applies a filter's lists (tests/filter.rs).

use alloc::vec::Vec; //@omit

/// What made an event. A reader can ask for some of them only.
pub const SRC_APP: u8 = 0;
pub const SRC_TOOLKIT: u8 = 1;
pub const SRC_SHELL: u8 = 2;
pub const SRC_KERNEL: u8 = 3;
pub const SOURCES: u8 = 4;

/// A field's visibility, as the program that made it marks it.
pub const VIS_PUBLIC: u8 = 0;
/// Leaves nothing: not the value, not the length (a note's text in a `node.changed`, a file's name in a private folder).
pub const VIS_PRIVATE: u8 = 1;
/// A password field, a token, a pairing code: as private, and the toolkit marks them so itself.
pub const VIS_SECRET: u8 = 2;

/// An app's manifest key `introspect`.
pub const INTROSPECT_ALL: u8 = 0;
pub const INTROSPECT_TREE: u8 = 1;
pub const INTROSPECT_NONE: u8 = 2;

/// The longest kind, and the longest pattern a reader may give.
pub const MAX_KIND: usize = 64;

/// The queue: the events with numbers from `head` up to, not with, `next`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)] //@omit
pub struct Window {
    pub head: u64,
    pub next: u64,
}

//@ pub open spec fn window_ok(w: Window, cap: u64) -> bool { 0 < cap && w.head <= w.next && w.next - w.head <= cap }

/// After one more event: the window, and whether the oldest went to make room for it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)] //@omit
pub struct Pushed {
    pub window: Window,
    pub dropped: bool,
}

/// A new event takes number `w.next`. A full queue drops its oldest, never the new one: a reader that is behind loses the past, not
/// what is happening now.
pub fn push(w: Window, cap: u64) -> Pushed //@ret
//@ requires window_ok(w, cap), w.next < u64::MAX,
//@ ensures window_ok(ret.window, cap), ret.window.next == w.next + 1,
//@     ret.dropped == (w.next - w.head == cap),
//@     ret.window.head == (if ret.dropped { w.head + 1 } else { w.head as int }),
{
    let dropped = w.next - w.head == cap;
    let head = if dropped { w.head + 1 } else { w.head };
    Pushed { window: Window { head, next: w.next + 1 }, dropped }
}

/// What a reader at `since` gets: `count` events from number `start`, and `lost` events it will never see.
#[derive(Clone, Copy, Debug, PartialEq, Eq)] //@omit
pub struct Plan {
    pub start: u64,
    pub count: u64,
    pub lost: u64,
    /// `since` is past anything this queue has numbered: the hub started again. The reader gets the queue from its start and
    /// cannot know what it lost.
    pub reset: bool,
}

/// The read of at most `max` events after a reader's cursor `since`. Its next cursor is `start + count`.
pub fn read_plan(w: Window, since: u64, max: u64) -> Plan //@ret
//@ requires w.head <= w.next,
//@ ensures w.head <= ret.start, ret.start + ret.count <= w.next, ret.count <= max,
//@     ret.count < max ==> ret.start + ret.count == w.next,
//@     ret.reset == (since > w.next),
//@     ret.reset ==> ret.start == w.head && ret.lost == 0,
//@     !ret.reset ==> ret.start == (if since < w.head { w.head as int } else { since as int }) && ret.lost == ret.start - since,
//@     !ret.reset ==> ret.lost + ret.count + (w.next - (ret.start + ret.count)) == w.next - since,
{
    let reset = since > w.next;
    let start = if reset || since < w.head { w.head } else { since };
    let lost = if reset { 0 } else { start - since };
    let left = w.next - start;
    let count = if left < max { left } else { max };
    Plan { start, count, lost, reset }
}

/// Where a source's next event stands against the last one the hub took from it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)] //@omit
pub enum Order {
    /// The very next one.
    InOrder,
    /// After this many the hub never got: the source's own buffer was full, or it sent while nobody listened.
    Gap(u64),
    /// Not after the last one: refused, so that what a reader sees of one source is always in the source's order.
    Stale,
}

/// Each source numbers its events from 0. `last` is the number of the last one taken, if any.
pub fn order(last: Option<u64>, got: u64) -> Order //@ret
//@ ensures match ret {
//@     Order::InOrder => match last { None => got == 0, Some(l) => l < u64::MAX && got == l + 1 },
//@     Order::Gap(n) => n > 0 && match last { None => n == got, Some(l) => got > l + 1 && n == got - l - 1 },
//@     Order::Stale => match last { None => false, Some(l) => got <= l },
//@ },
{
    match last {
        None => {
            if got == 0 { Order::InOrder } else { Order::Gap(got) }
        }
        Some(l) => {
            if got <= l {
                Order::Stale
            } else if got - l == 1 {
                Order::InOrder
            } else {
                Order::Gap(got - l - 1)
            }
        }
    }
}

/// Whether the hub keeps an event at all: an app with `introspect = none` is in no layer, events included. The kernel's and the
/// shell's events about it (that it started, that it is in front) are facts about the machine and stay.
pub fn may_record(src: u8, introspect: u8) -> bool //@ret
//@ ensures ret == (src < SOURCES && !((src == SRC_APP || src == SRC_TOOLKIT) && introspect == INTROSPECT_NONE)),
{
    src < SOURCES && !((src == SRC_APP || src == SRC_TOOLKIT) && introspect == INTROSPECT_NONE)
}

//@ pub open spec fn reads_owner(kind: Seq<u8>) -> bool { kind == seq![111u8, 119u8, 110u8, 101u8, 114u8, 46u8, 105u8, 110u8, 112u8, 117u8, 116u8] }

/// `owner.input`: the owner is typing or pointing. It never carries a field, whatever the sender put in it.
pub fn content_free(kind: &[u8]) -> bool //@ret
//@ ensures ret == reads_owner(kind@),
{
    let r = kind.len() == 11
        && kind[0] == b'o' && kind[1] == b'w' && kind[2] == b'n' && kind[3] == b'e' && kind[4] == b'r' && kind[5] == b'.'
        && kind[6] == b'i' && kind[7] == b'n' && kind[8] == b'p' && kind[9] == b'u' && kind[10] == b't';
    //@ if r { assert(kind@ =~= seq![111u8, 119u8, 110u8, 101u8, 114u8, 46u8, 105u8, 110u8, 112u8, 117u8, 116u8]); }
    r
}

/// The fields of an event that may leave the hub, by their place in `vis`: public ones, in order, none of a content-free event.
pub fn exposed_fields(vis: &[u8], content_free: bool) -> Vec<usize> //@ret
//@ ensures forall|k: int| 0 <= k < ret@.len() ==> ret@[k] < vis@.len() && vis@[ret@[k] as int] == VIS_PUBLIC,
//@     forall|k: int, j: int| 0 <= k < j < ret@.len() ==> ret@[k] < ret@[j],
//@     content_free ==> ret@.len() == 0,
//@     !content_free ==> forall|i: int| 0 <= i < vis@.len() && vis@[i] == VIS_PUBLIC ==> exists|k: int| 0 <= k < ret@.len() && ret@[k] == i,
{
    let mut out: Vec<usize> = Vec::new();
    if content_free {
        return out;
    }
    let mut i = 0;
    while i < vis.len()
    //@ invariant 0 <= i <= vis@.len(),
    //@     forall|k: int| 0 <= k < out@.len() ==> out@[k] < i && vis@[out@[k] as int] == VIS_PUBLIC,
    //@     forall|k: int, j: int| 0 <= k < j < out@.len() ==> out@[k] < out@[j],
    //@     forall|m: int| 0 <= m < i && vis@[m] == VIS_PUBLIC ==> exists|k: int| 0 <= k < out@.len() && out@[k] == m,
    //@ decreases vis@.len() - i,
    {
        //@ let ghost before = out@;
        if vis[i] == VIS_PUBLIC {
            out.push(i);
            //@ assert(out@[out@.len() - 1] == i);
            //@ assert(forall|k: int| 0 <= k < before.len() ==> out@[k] == before[k]);
        }
        //@ assert(forall|m: int| 0 <= m < i && vis@[m] == VIS_PUBLIC ==> exists|k: int| 0 <= k < before.len() && before[k] == m);
        //@ assert(forall|m: int| 0 <= m < i && vis@[m] == VIS_PUBLIC ==> exists|k: int| 0 <= k < out@.len() && out@[k] == m) by {
        //@     assert forall|m: int| 0 <= m < i && vis@[m] == VIS_PUBLIC implies exists|k: int| 0 <= k < out@.len() && out@[k] == m by {
        //@         let k = choose|k: int| 0 <= k < before.len() && before[k] == m;
        //@         assert(out@[k] == m);
        //@     }
        //@ }
        i += 1;
    }
    out
}

//@ pub open spec fn is_prefix_pattern(p: Seq<u8>) -> bool { p.len() >= 2 && p[p.len() - 1] == 42u8 && p[p.len() - 2] == 46u8 }
//@ /// `download.*` is every kind that starts with `download.`; any other pattern is one kind exactly.
//@ pub open spec fn kind_match(p: Seq<u8>, k: Seq<u8>) -> bool {
//@     if is_prefix_pattern(p) { p.len() - 1 <= k.len() && k.subrange(0, p.len() - 1) == p.subrange(0, p.len() - 1) } else { p == k }
//@ }

/// Whether a reader's pattern takes this kind: `media.ended` exactly, or `media.*` for every `media.` kind. `media.*` does not take
/// `media` itself, nor `mediaplayer.x`.
pub fn kind_matches(pattern: &[u8], kind: &[u8]) -> bool //@ret
//@ ensures ret == kind_match(pattern@, kind@),
{
    let p = pattern.len();
    let prefix = p >= 2 && pattern[p - 1] == b'*' && pattern[p - 2] == b'.';
    let n = if prefix { p - 1 } else { p };
    if prefix {
        if kind.len() < n {
            return false;
        }
    } else if kind.len() != n {
        //@ assert(pattern@.len() != kind@.len());
        return false;
    }
    let mut i = 0;
    while i < n
    //@ invariant 0 <= i <= n, n <= pattern@.len(), n <= kind@.len(), p == pattern@.len(),
    //@     prefix == is_prefix_pattern(pattern@), n == (if prefix { p - 1 } else { p as int }),
    //@     !prefix ==> kind@.len() == n,
    //@     forall|k: int| 0 <= k < i ==> pattern@[k] == kind@[k],
    //@ decreases n - i,
    {
        if pattern[i] != kind[i] {
            //@ assert(pattern@.subrange(0, n as int)[i as int] != kind@.subrange(0, n as int)[i as int]);
            return false;
        }
        i += 1;
    }
    //@ if prefix { assert(kind@.subrange(0, n as int) =~= pattern@.subrange(0, n as int)); } else { assert(kind@ =~= pattern@); }
    true
}

//@ pub open spec fn kind_byte(c: u8) -> bool { (97u8 <= c && c <= 122u8) || (48u8 <= c && c <= 57u8) || c == 95u8 || c == 46u8 }
//@ pub open spec fn double_dot(k: Seq<u8>, i: int) -> bool { k[i] == 46u8 && k[i + 1] == 46u8 }
//@ pub open spec fn kind_ok(k: Seq<u8>) -> bool {
//@     &&& 0 < k.len() <= MAX_KIND
//@     &&& k[0] != 46u8 && k[k.len() - 1] != 46u8
//@     &&& forall|i: int| 0 <= i < k.len() ==> kind_byte(k[i])
//@     &&& forall|i: int| 0 <= i < k.len() - 1 ==> !double_dot(k, i)
//@ }

/// A kind a sender may use: 1-64 bytes of `a-z`, `0-9`, `_` and `.`, words between single dots. So a kind is never a pattern.
pub fn valid_kind(kind: &[u8]) -> bool //@ret
//@ ensures ret == kind_ok(kind@),
{
    let n = kind.len();
    if n == 0 || n > MAX_KIND || kind[0] == b'.' || kind[n - 1] == b'.' {
        return false;
    }
    let mut i = 0;
    while i < n
    //@ invariant 0 <= i <= n, n == kind@.len(), 0 < n <= MAX_KIND, kind@[0] != 46u8, kind@[n - 1] != 46u8,
    //@     forall|k: int| 0 <= k < i ==> kind_byte(kind@[k]),
    //@     forall|k: int| 0 <= k < i && k < n - 1 ==> !double_dot(kind@, k),
    //@ decreases n - i,
    {
        let c = kind[i];
        let byte = (c >= b'a' && c <= b'z') || (c >= b'0' && c <= b'9') || c == b'_' || c == b'.';
        if !byte {
            //@ assert(!kind_byte(kind@[i as int]));
            return false;
        }
        if c == b'.' && i + 1 < n && kind[i + 1] == b'.' {
            //@ assert(double_dot(kind@, i as int));
            return false;
        }
        i += 1;
    }
    true
}

/// A source mask a reader gives: bit `src` set takes that source. 0 means every source.
pub fn source_matches(mask: u8, src: u8) -> bool //@ret
//@ requires src < SOURCES,
//@ ensures ret == (mask == 0 || (mask >> src) & 1u8 == 1u8),
{
    mask == 0 || (mask >> src) & 1 == 1
}

/// A reader's subject prefix: empty takes everything.
pub fn subject_matches(prefix: &[u8], subject: &[u8]) -> bool //@ret
//@ ensures ret == (prefix@.len() <= subject@.len() && subject@.subrange(0, prefix@.len() as int) == prefix@),
{
    let n = prefix.len();
    if subject.len() < n {
        return false;
    }
    let mut i = 0;
    while i < n
    //@ invariant 0 <= i <= n, n == prefix@.len(), n <= subject@.len(), forall|k: int| 0 <= k < i ==> prefix@[k] == subject@[k],
    //@ decreases n - i,
    {
        if prefix[i] != subject[i] {
            //@ assert(subject@.subrange(0, n as int)[i as int] != prefix@[i as int]);
            return false;
        }
        i += 1;
    }
    //@ assert(subject@.subrange(0, n as int) =~= prefix@);
    true
}

/// A flood guard per app: `tokens` events may go now, one more every `every_ms`, never more than `burst` saved up.
#[derive(Clone, Copy, Debug, PartialEq, Eq)] //@omit
pub struct Bucket {
    pub tokens: u32,
    pub at_ms: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)] //@omit
pub struct Admitted {
    pub bucket: Bucket,
    pub ok: bool,
}

/// Whether an app's event goes into the queue now. A refused one is counted as lost for every reader, as a dropped one is.
pub fn admit(b: Bucket, now_ms: u64, every_ms: u64, burst: u32) -> Admitted //@ret
//@ requires 0 < every_ms, 0 < burst, b.tokens <= burst,
//@ ensures ret.bucket.tokens <= burst, ret.ok ==> ret.bucket.tokens < burst,
//@     !ret.ok ==> ret.bucket.tokens == 0,
//@     now_ms <= b.at_ms ==> (ret.ok == (b.tokens > 0)) && (ret.ok ==> ret.bucket.tokens == b.tokens - 1),
//@     ret.bucket.at_ms >= b.at_ms,
{
    let mut tokens = b.tokens;
    let mut at = b.at_ms;
    if now_ms > b.at_ms {
        let gained = (now_ms - b.at_ms) / every_ms;
        if gained > 0 {
            let room = (burst - tokens) as u64;
            tokens = if gained >= room { burst } else { tokens + gained as u32 };
            // The part of a period already waited is not kept: a flood guard may err on the strict side.
            at = now_ms;
        }
    }
    if tokens == 0 {
        Admitted { bucket: Bucket { tokens: 0, at_ms: at }, ok: false }
    } else {
        Admitted { bucket: Bucket { tokens: tokens - 1, at_ms: at }, ok: true }
    }
}

/// `wait` without an event: the screen has settled when some program presented after the reader's present number and none has
/// for `settle_ms` since.
pub fn settled(since_present: u64, present_seq: u64, last_present_ms: u64, now_ms: u64, settle_ms: u64) -> bool //@ret
//@ ensures ret == (present_seq > since_present && now_ms >= last_present_ms && now_ms - last_present_ms >= settle_ms),
{
    present_seq > since_present && now_ms >= last_present_ms && now_ms - last_present_ms >= settle_ms
}

/// The kernel reports the battery when its level enters another step of 5 % and has moved by 2 or more since the last report, so a
/// gauge wavering between 20 and 19 says it once. The first reading (`last` = none) is always reported.
pub fn battery_report(last: Option<u8>, now: u8) -> bool //@ret
//@ requires now <= 100,
//@ ensures ret == match last { None => true, Some(l) => now / 5 != l / 5 && (now >= l + 2 || l >= now + 2) },
{
    match last {
        None => true,
        Some(l) => now / 5 != l / 5 && (now as u16 >= l as u16 + 2 || l as u16 >= now as u16 + 2),
    }
}
