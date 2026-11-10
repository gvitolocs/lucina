// What must hold whatever an app declares or Lucina asks for: which class of effect a call has once the app and the server have
// both spoken, whether it runs, needs a decision or is refused; whether its arguments are acceptable; what undoing one of her
// writes does; and that the undo journal only grows, in order, with each action undone at most once. Verus checks this very file:
// `//@` lines hold the proof, see scripts/verify-shipped.sh (D-011). The rest of the crate, the toolkit and lucina-devd call these
// functions and hold no copy of the rules.
//
// Not proved: that an app's intent does what its name says (each app's tests call each intent through `Harness`), that a state's
// `stamp` really is the target's content hash or version (`journal.rs` and the apps make it), that the JSON on disk is what was
// parsed into [`Record`]s (`journal.rs` tests), and anything the server does with a decision.

/// What a call can do to the world, least strict first. The order is the plan's (docs/research/juno-ai-native.md, Security model).
#[derive(Clone, Copy)]
#[derive(Debug, PartialEq, Eq, Hash)] //@omit
pub enum Effect {
    /// Looks only.
    Read,
    /// Changes something on the machine that the journal can undo.
    Write,
    /// Deletes or overwrites what is not hers to lose: a decision first.
    Destructive,
    /// Sends, posts, pays or orders outside the machine: cannot be undone, so a decision first.
    External,
    /// Approving a sign-in, a pairing, a decision; Secrets' reveal. Never hers, whatever the rights.
    OwnerOnly,
}

//@ pub open spec fn rank(e: Effect) -> int {
//@     match e { Effect::Read => 0, Effect::Write => 1, Effect::Destructive => 2, Effect::External => 3, Effect::OwnerOnly => 4 }
//@ }

/// The class's place in the order.
pub fn effect_rank(e: Effect) -> u8 //@ret
//@ ensures ret as int == rank(e),
{
    match e {
        Effect::Read => 0,
        Effect::Write => 1,
        Effect::Destructive => 2,
        Effect::External => 3,
        Effect::OwnerOnly => 4,
    }
}

/// The stricter of two classes: what the app declared and what the server found. So neither side can talk a call down.
pub fn stricter(a: Effect, b: Effect) -> Effect //@ret
//@ ensures
//@     rank(ret) >= rank(a) && rank(ret) >= rank(b),
//@     ret == a || ret == b,
//@     rank(ret) == if rank(a) >= rank(b) { rank(a) } else { rank(b) },
{
    if effect_rank(a) >= effect_rank(b) { a } else { b }
}

/// What happens to a call.
#[derive(Clone, Copy)]
#[derive(Debug, PartialEq, Eq)] //@omit
pub enum Gate {
    Run,
    /// `lucina_decide` first, bound to the hash of this exact call.
    NeedsDecision,
    Refuse,
}

/// What happens to a call whose app declared `declared` and whose server classification is `server`. `decided` is true only when
/// the server has checked that a decision answered "go" for this very call.
pub fn gate(declared: Effect, server: Effect, decided: bool) -> Gate //@ret
//@ ensures ({
//@     let r = if rank(declared) >= rank(server) { rank(declared) } else { rank(server) };
//@     &&& (r <= 1 ==> ret is Run)
//@     &&& ((r == 2 || r == 3) && decided ==> ret is Run)
//@     &&& ((r == 2 || r == 3) && !decided ==> ret is NeedsDecision)
//@     &&& (r == 4 ==> ret is Refuse)
//@ }),
{
    match stricter(declared, server) {
        Effect::Read | Effect::Write => Gate::Run,
        Effect::Destructive | Effect::External => {
            if decided { Gate::Run } else { Gate::NeedsDecision }
        }
        Effect::OwnerOnly => Gate::Refuse,
    }
}

// ---- Arguments ----

/// Longest path an intent takes.
pub const MAX_PATH: usize = 4096;
/// Longest entity id (`n_42`, `itm_…`, a Drive file id).
pub const MAX_ID: usize = 128;

/// No NUL in it: what every text argument must be, since paths and C strings end there.
pub fn text_clean(s: &[u8]) -> bool //@ret
//@ ensures ret <==> forall|k: int| 0 <= k < s@.len() ==> s@[k] != 0u8,
{
    let n = s.len();
    let mut i = 0;
    while i < n
    //@ invariant 0 <= i <= n, n == s@.len(), forall|k: int| 0 <= k < i ==> s@[k] != 0u8,
    //@ decreases n - i,
    {
        if s[i] == 0 {
            return false;
        }
        i += 1;
    }
    true
}

//@ pub open spec fn starts_part(s: Seq<u8>, i: int) -> bool { s[i - 1] == 47u8 }
//@ pub open spec fn ends_part(s: Seq<u8>, i: int) -> bool { i == s.len() || s[i] == 47u8 }
//@ /// What an acceptable path may not have at byte k (k >= 1; byte 0 is the leading `/`).
//@ pub open spec fn path_byte_ok(s: Seq<u8>, k: int) -> bool {
//@     &&& s[k] != 0u8
//@     &&& (s[k] == 47u8 ==> k + 1 < s.len() && s[k + 1] != 47u8)
//@     &&& (starts_part(s, k) && s[k] == 46u8 ==> !ends_part(s, k + 1))
//@     &&& (starts_part(s, k) && s[k] == 46u8 && k + 1 < s.len() && s[k + 1] == 46u8 ==> !ends_part(s, k + 2))
//@ }

/// A file an intent may name: absolute, at most [`MAX_PATH`] bytes, no NUL, no empty part (`//`), no `.` or `..` part, not
/// ending in `/`. What lies under which folder is lucina-devd's `places` (the off-limits paths); this only makes sure the path
/// says plainly where it goes.
pub fn path_ok(s: &[u8]) -> bool //@ret
//@ ensures ret ==> 1 < s@.len() <= 4096 && s@[0] == 47u8 && forall|k: int| 1 <= k < s@.len() ==> path_byte_ok(s@, k),
{
    let n = s.len();
    if n < 2 || n > MAX_PATH || s[0] != b'/' {
        return false;
    }
    if s[1] == b'/' {
        return false;
    }
    let mut i = 1;
    while i < n
    //@ invariant 1 <= i <= n, n == s@.len(), 1 < n <= 4096, s@[0] == 47u8, s@[1] != 47u8,
    //@     forall|k: int| 1 <= k < i ==> path_byte_ok(s@, k),
    //@ decreases n - i,
    {
        let c = s[i];
        if c == 0 {
            return false;
        }
        if c == b'/' && (i + 1 == n || s[i + 1] == b'/') {
            return false;
        }
        if c == b'.' && s[i - 1] == b'/' {
            let one = i + 1 == n || s[i + 1] == b'/';
            let two = i + 1 < n && s[i + 1] == b'.' && (i + 2 == n || s[i + 2] == b'/');
            if one || two {
                return false;
            }
        }
        i += 1;
    }
    true
}

//@ pub open spec fn id_byte(c: u8) -> bool {
//@     (48u8 <= c <= 57u8) || (65u8 <= c <= 90u8) || (97u8 <= c <= 122u8) || c == 95u8 || c == 45u8 || c == 46u8 || c == 58u8
//@ }

/// An entity id: 1 to [`MAX_ID`] bytes of letters, digits and `_ - . :`, so that it can be a file name part or a URL segment.
pub fn id_ok(s: &[u8]) -> bool //@ret
//@ ensures ret <==> (0 < s@.len() <= 128 && forall|k: int| 0 <= k < s@.len() ==> id_byte(s@[k])),
{
    let n = s.len();
    if n == 0 || n > MAX_ID {
        return false;
    }
    let mut i = 0;
    while i < n
    //@ invariant 0 <= i <= n, n == s@.len(), 0 < n <= 128, forall|k: int| 0 <= k < i ==> id_byte(s@[k]),
    //@ decreases n - i,
    {
        let c = s[i];
        let ok = (c >= b'0' && c <= b'9') || (c >= b'A' && c <= b'Z') || (c >= b'a' && c <= b'z') || c == b'_' || c == b'-' || c == b'.' || c == b':';
        if !ok {
            return false;
        }
        i += 1;
    }
    true
}

/// The type an argument has in an intent's schema.
#[derive(Clone, Copy)]
#[derive(Debug, PartialEq, Eq)] //@omit
pub enum Kind {
    Text,
    Path,
    Id,
    Int,
    Bool,
}

/// One argument of an intent: its type, whether it must be given, and its bounds (`max_len` for texts and paths, `min`/`max` for
/// numbers).
#[derive(Clone, Copy)]
#[derive(Debug, PartialEq, Eq)] //@omit
pub struct ArgRule {
    pub kind: Kind,
    pub required: bool,
    pub max_len: u64,
    pub min: i64,
    pub max: i64,
}

/// What a call gave for one argument, already looked at: a text carries its length and what [`text_clean`], [`path_ok`] and
/// [`id_ok`] said of it.
#[derive(Clone, Copy)]
#[derive(Debug, PartialEq, Eq)] //@omit
pub enum Given {
    Missing,
    Null,
    Bool,
    Int(i64),
    Text { len: u64, clean: bool, path: bool, id: bool },
    /// An array, an object, a fraction: no intent argument takes one yet.
    Other,
}

//@ pub open spec fn spec_arg_ok(r: ArgRule, g: Given) -> bool {
//@     match g {
//@         Given::Missing | Given::Null => !r.required,
//@         Given::Bool => r.kind is Bool,
//@         Given::Int(v) => r.kind is Int && r.min <= v && v <= r.max,
//@         Given::Text { len, clean, path, id } => match r.kind {
//@             Kind::Text => clean && len <= r.max_len,
//@             Kind::Path => path && clean && len <= r.max_len,
//@             Kind::Id => id,
//@             _ => false,
//@         },
//@         Given::Other => false,
//@     }
//@ }

/// Whether one argument is acceptable.
pub fn arg_ok(r: ArgRule, g: Given) -> bool //@ret
//@ ensures ret == spec_arg_ok(r, g),
{
    match g {
        Given::Missing | Given::Null => !r.required,
        Given::Bool => matches!(r.kind, Kind::Bool),
        Given::Int(v) => matches!(r.kind, Kind::Int) && r.min <= v && v <= r.max,
        Given::Text { len, clean, path, id } => match r.kind {
            Kind::Text => clean && len <= r.max_len,
            Kind::Path => path && clean && len <= r.max_len,
            Kind::Id => id,
            _ => false,
        },
        Given::Other => false,
    }
}

/// Whether a call's arguments are acceptable: one given value per rule, in the rules' order, every one acceptable, and no
/// argument the intent does not have (`unknown` counts those).
pub fn args_ok(rules: &[ArgRule], given: &[Given], unknown: u64) -> bool //@ret
//@ ensures ret <==> (unknown == 0 && rules@.len() == given@.len() && forall|k: int| 0 <= k < rules@.len() ==> spec_arg_ok(rules@[k], given@[k])),
{
    if unknown != 0 || rules.len() != given.len() {
        return false;
    }
    let n = rules.len();
    let mut i = 0;
    while i < n
    //@ invariant 0 <= i <= n, n == rules@.len(), n == given@.len(), unknown == 0, forall|k: int| 0 <= k < i ==> spec_arg_ok(rules@[k], given@[k]),
    //@ decreases n - i,
    {
        if !arg_ok(rules[i], given[i]) {
            return false;
        }
        i += 1;
    }
    true
}

// ---- Undo ----

/// One target as the journal sees it: whether it is there, and a stamp of its content (a hash or a version; 0 when it is not
/// there).
#[derive(Clone, Copy)]
#[derive(Debug, PartialEq, Eq)] //@omit
pub struct State {
    pub exists: bool,
    pub stamp: u64,
}

//@ pub open spec fn normal(s: State) -> bool { !s.exists ==> s.stamp == 0 }

/// What kind of write she made.
#[derive(Clone, Copy)]
#[derive(Debug, PartialEq, Eq)] //@omit
pub enum WriteKind {
    /// Something new: a note, a file that was not there.
    Create,
    /// New content for something that was there: a note's text, a file overwritten, a setting.
    Change,
    /// Something taken away: a note to the trash, a file removed. Its old content is kept (the trash, a blob).
    Remove,
}

/// What undoing a write does to its target.
#[derive(Clone, Copy)]
#[derive(Debug, PartialEq, Eq)] //@omit
pub enum UndoOp {
    /// Take away what she created.
    TakeAway,
    /// Put the old content back.
    PutBack,
    /// Bring back what she removed.
    BringBack,
}

/// The journal's undo entry for one write: the target before and after, and what undoing it does.
#[derive(Clone, Copy)]
#[derive(Debug, PartialEq, Eq)] //@omit
pub struct Entry {
    pub before: State,
    pub after: State,
    pub op: UndoOp,
}

//@ pub open spec fn shape_ok(kind: WriteKind, before: State, after: State) -> bool {
//@     normal(before) && normal(after) && match kind {
//@         WriteKind::Create => !before.exists && after.exists,
//@         WriteKind::Change => before.exists && after.exists && before.stamp != after.stamp,
//@         WriteKind::Remove => before.exists && !after.exists,
//@     }
//@ }

//@ /// The target once an undo has run on it: what the op does, whatever the entry says the target was.
//@ pub open spec fn run_op(op: UndoOp, before: State) -> State {
//@     match op { UndoOp::TakeAway => State { exists: false, stamp: 0 }, UndoOp::PutBack => before, UndoOp::BringBack => before }
//@ }

/// The undo entry for a write of `kind` that took the target from `before` to `after`; `None` when the states don't fit the kind
/// (a "create" of something that was there, a "change" that changed nothing), so a wrong entry is never written.
pub fn entry_for(kind: WriteKind, before: State, after: State) -> Option<Entry> //@ret
//@ ensures match ret {
//@     Some(e) => shape_ok(kind, before, after) && e.before == before && e.after == after && run_op(e.op, before) == before
//@         && (kind is Create ==> e.op is TakeAway) && (kind is Change ==> e.op is PutBack) && (kind is Remove ==> e.op is BringBack),
//@     None => !shape_ok(kind, before, after),
//@ },
{
    let normal = (before.exists || before.stamp == 0) && (after.exists || after.stamp == 0);
    if !normal {
        return None;
    }
    let op = match kind {
        WriteKind::Create => {
            if before.exists || !after.exists {
                return None;
            }
            UndoOp::TakeAway
        }
        WriteKind::Change => {
            if !before.exists || !after.exists || before.stamp == after.stamp {
                return None;
            }
            UndoOp::PutBack
        }
        WriteKind::Remove => {
            if !before.exists || after.exists {
                return None;
            }
            UndoOp::BringBack
        }
    };
    Some(Entry { before, after, op })
}

/// Undo `e` on the target as it is `now`: the state it must be brought to, or `None` when it is no longer what her write left
/// (the owner or an app changed it since), so an undo never throws away anyone else's work.
pub fn undo_to(now: State, e: Entry) -> Option<State> //@ret
//@ ensures match ret {
//@     Some(s) => now == e.after && s == run_op(e.op, e.before),
//@     None => now != e.after,
//@ },
{
    if now.exists != e.after.exists || now.stamp != e.after.stamp {
        return None;
    }
    match e.op {
        UndoOp::TakeAway => Some(State { exists: false, stamp: 0 }),
        UndoOp::PutBack | UndoOp::BringBack => Some(e.before),
    }
}

/// The class of running an undo: a write, since it only takes away or replaces what her own write left there ([`undo_to`]
/// refuses otherwise) and puts back what was there before.
pub fn undo_effect(_op: UndoOp) -> Effect //@ret
//@ ensures ret is Write,
{
    Effect::Write
}

//@ /// Undoing her write brings the target back to what it was before it: for every kind of write.
//@ pub proof fn undo_restores(kind: WriteKind, before: State, after: State, e: Entry)
//@     requires shape_ok(kind, before, after), e.before == before, e.after == after, run_op(e.op, before) == before,
//@     ensures run_op(e.op, e.before) == before,
//@ {
//@ }

// ---- The journal ----

/// One line of the journal as the rules see it: its sequence number, the sequence number of the write it undoes (0 when it is a
/// write itself), and whether it can be undone.
#[derive(Clone, Copy)]
#[derive(Debug, PartialEq, Eq)] //@omit
pub struct Record {
    pub seq: u64,
    pub undoes: u64,
    pub undoable: bool,
}

//@ pub open spec fn seq_ok_at(s: Seq<Record>, i: int) -> bool {
//@     if i == 0 { s[0].seq >= 1 } else { s[i].seq == s[i - 1].seq + 1 }
//@ }
//@ /// Record i, if it is an undo, undoes an earlier undoable write that nothing before it undid, and is not undoable itself.
//@ pub open spec fn undo_ok_at(s: Seq<Record>, i: int) -> bool {
//@     s[i].undoes != 0 ==> ({
//@         let j = s[i].undoes - s[0].seq;
//@         &&& 0 <= j < i
//@         &&& s[j].seq == s[i].undoes
//@         &&& s[j].undoes == 0
//@         &&& s[j].undoable
//@         &&& !s[i].undoable
//@         &&& forall|k: int| 0 <= k < i ==> s[k].undoes != s[i].undoes
//@     })
//@ }
//@ pub open spec fn ok_upto(s: Seq<Record>, n: int) -> bool {
//@     forall|i: int| 0 <= i < n ==> seq_ok_at(s, i) && undo_ok_at(s, i)
//@ }
//@ pub open spec fn journal_spec(s: Seq<Record>) -> bool { ok_upto(s, s.len() as int) }

/// Whether record `i` fits after the `i` records before it.
fn record_ok_at(r: &[Record], i: usize) -> bool //@ret
//@ requires i < r@.len(),
//@ ensures ret <==> (seq_ok_at(r@, i as int) && undo_ok_at(r@, i as int)),
{
    let seq_ok = if i == 0 { r[0].seq >= 1 } else { r[i - 1].seq < u64::MAX && r[i].seq == r[i - 1].seq + 1 };
    if !seq_ok {
        return false;
    }
    let t = r[i].undoes;
    if t == 0 {
        return true;
    }
    if t < r[0].seq || r[i].undoable {
        return false;
    }
    let j = t - r[0].seq;
    if j >= i as u64 {
        return false;
    }
    let j = j as usize;
    if r[j].seq != t || r[j].undoes != 0 || !r[j].undoable {
        return false;
    }
    let mut k = 0;
    while k < i
    //@ invariant 0 <= k <= i, i < r@.len(), t != 0, t == r@[i as int].undoes, forall|m: int| 0 <= m < k ==> r@[m].undoes != t,
    //@ decreases i - k,
    {
        if r[k].undoes == t {
            return false;
        }
        k += 1;
    }
    true
}

/// Whether a journal, as read from disk, is one: numbered from 1 up without a gap, every undo after the write it undoes, each write
/// undone at most once, no undo undone.
pub fn journal_ok(r: &[Record]) -> bool //@ret
//@ ensures ret <==> journal_spec(r@),
{
    let n = r.len();
    let mut i = 0;
    while i < n
    //@ invariant 0 <= i <= n, n == r@.len(), ok_upto(r@, i as int),
    //@ decreases n - i,
    {
        if !record_ok_at(r, i) {
            //@ assert(!(seq_ok_at(r@, i as int) && undo_ok_at(r@, i as int)));
            return false;
        }
        i += 1;
    }
    true
}

//@ proof fn push_keeps(s: Seq<Record>, rec: Record)
//@     requires journal_spec(s),
//@     ensures ok_upto(s.push(rec), s.len() as int),
//@ {
//@     let t = s.push(rec);
//@     assert forall|i: int| 0 <= i < s.len() implies seq_ok_at(t, i) && undo_ok_at(t, i) by {
//@         assert(seq_ok_at(s, i) && undo_ok_at(s, i));
//@         assert forall|k: int| 0 <= k <= i implies t[k] == s[k] by {}
//@     }
//@ }

/// Whether `rec` may go at the end of the journal `r`: only appending, and only what keeps it a journal ([`journal_ok`]).
pub fn may_append(r: &[Record], rec: Record) -> bool //@ret
//@ requires journal_spec(r@),
//@ ensures ret ==> journal_spec(r@.push(rec)),
{
    let n = r.len();
    let first = if n == 0 { rec.seq } else { r[0].seq };
    if n == 0 {
        if rec.seq < 1 || rec.undoes != 0 {
            return false;
        }
    } else if r[n - 1].seq == u64::MAX || rec.seq != r[n - 1].seq + 1 {
        return false;
    }
    let t = rec.undoes;
    if t != 0 {
        if t < first || rec.undoable {
            return false;
        }
        let j = t - first;
        if j >= n as u64 {
            return false;
        }
        let j = j as usize;
        if r[j].seq != t || r[j].undoes != 0 || !r[j].undoable {
            return false;
        }
        let mut k = 0;
        while k < n
        //@ invariant 0 <= k <= n, n == r@.len(), t == rec.undoes, forall|m: int| 0 <= m < k ==> r@[m].undoes != t,
        //@ decreases n - k,
        {
            if r[k].undoes == t {
                return false;
            }
            k += 1;
        }
    }
    //@ proof {
    //@     push_keeps(r@, rec);
    //@     let s = r@.push(rec);
    //@     assert(s[n as int] == rec);
    //@     assert(n > 0 ==> s[0] == r@[0]);
    //@     assert(forall|m: int| 0 <= m < n ==> s[m] == r@[m]);
    //@     assert(seq_ok_at(s, n as int));
    //@     assert(undo_ok_at(s, n as int));
    //@     assert(ok_upto(s, s.len() as int));
    //@ }
    true
}

/// Whether the write numbered `seq` can be undone now: it is in the journal, is a write that can be undone, and nothing undid it.
pub fn can_undo(r: &[Record], seq: u64) -> bool //@ret
//@ requires journal_spec(r@),
//@ ensures ret ==> (r@.len() > 0 && r@[r@.len() - 1].seq < u64::MAX
//@     && journal_spec(r@.push(Record { seq: (r@[r@.len() - 1].seq + 1) as u64, undoes: seq, undoable: false }))),
{
    let n = r.len();
    if n == 0 || seq == 0 || r[n - 1].seq == u64::MAX {
        return false;
    }
    let rec = Record { seq: r[n - 1].seq + 1, undoes: seq, undoable: false };
    may_append(r, rec)
}
