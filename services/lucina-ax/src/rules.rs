// What must hold whatever an app draws or Lucina asks: which parts of a widget leave the app, how node ids are told apart, which
// widget an action reaches, and how big a tree can get. Verus checks this very file: `//@` lines hold the proof, see
// scripts/verify-shipped.sh (D-011). The recorder in lucina-app, the egui bridge and the server call these functions and hold no
// copy of the rules.
//
// Not proved: that the recorder and the serialiser follow what these functions decide (tests in lucina-app and lucina-ax check
// that no password, nor a value or drawn text of a private area, shows up in the JSON), that the 64-bit mix of a key with its parent has no collision
// (the recorder drops a second node with an id already used, and says so), and what an app does with an action it is given.

/// At most this many nodes in one app's tree; the rest are cut and the tree says so.
pub const MAX_NODES: u32 = 4096;
/// A web page's tree (LucinaNet, stage F) may hold more: pages are bigger than apps. With [`NODE_BYTES`] and the text limit this
/// is exactly what fits in [`MAX_REPLY_BYTES`].
pub const MAX_WEB_NODES: u32 = 8192;
/// Nodes nested deeper than this are not recorded.
pub const MAX_DEPTH: u32 = 64;
/// At most this many children under one node.
pub const MAX_CHILDREN: u32 = 512;
/// One name, value or description: at most this many characters.
pub const MAX_TEXT_CHARS: u32 = 1024;
/// All the text of one tree together.
pub const MAX_TOTAL_CHARS: u32 = 262_144;
/// What one node costs in a reply besides its text: role, id, bounds, states, actions, its id in its parent's children, the
/// Juno extras, and its line in the text rendering. The serialiser's own test checks a node with every field set stays under it.
pub const NODE_BYTES: u64 = 640;
/// A character of text, escaped for JSON at worst (`\u001f`), once in the AccessKit update and once in the text rendering.
pub const CHAR_BYTES: u64 = 12;
/// The largest reply the server sends.
pub const MAX_REPLY_BYTES: u64 = 8 * 1024 * 1024;

/// What sort of widget a node is, for what of it may leave the app.
#[derive(Clone, Copy)]
#[derive(Debug, PartialEq, Eq)] //@omit
pub enum Kind {
    /// Labels, buttons, fields, lists: everything.
    Plain,
    /// A password field: there is a field, with its role and name, never what is in it or how long it is.
    Password,
    /// The area `ui.private` makes: its widgets and their actions are there, the values and drawn text inside are not.
    Private,
}

/// Where a widget sits: inside a private area, or inside a password field (whose children would be its text).
#[derive(Clone, Copy)]
#[derive(Debug, PartialEq, Eq)] //@omit
pub struct Scope {
    pub private: bool,
    pub dropped: bool,
}

/// What of a node goes into the tree. What is held back is marked `secret`, so that she knows something is there.
#[derive(Clone, Copy)]
#[derive(Debug, PartialEq, Eq)] //@omit
pub struct Shown {
    /// The node is in the tree at all.
    pub node: bool,
    /// Its name. Held back for drawn text inside a private area, which is the owner's data there (a code, an address).
    pub name: bool,
    /// Its value (a field's text, a slider's number) and anything that tells its length.
    pub value: bool,
    /// The nodes inside it.
    pub children: bool,
    /// The actions it takes.
    pub actions: bool,
    /// Something of it is held back.
    pub secret: bool,
}

/// What of a widget of `kind` is shown, where `scope` puts it; `content` says its name is text the app drew from the owner's
/// data rather than its own label.
pub fn shown(kind: Kind, scope: Scope, content: bool) -> Shown //@ret
//@ ensures
//@     scope.dropped ==> !ret.node && !ret.name && !ret.value && !ret.children && !ret.actions,
//@     !scope.dropped ==> ret.node && ret.actions,
//@     (kind == Kind::Password || kind == Kind::Private || scope.private) ==> !ret.value && ret.secret,
//@     (scope.private && content) ==> !ret.name && ret.secret,
//@     kind == Kind::Password ==> !ret.children,
//@     !ret.secret ==> ret.node && ret.name && ret.value,
//@     (!scope.private && !scope.dropped && kind == Kind::Plain) ==> ret.node && ret.name && ret.value && ret.children && !ret.secret,
{
    if scope.dropped {
        return Shown { node: false, name: false, value: false, children: false, actions: false, secret: true };
    }
    let secret = scope.private || !matches!(kind, Kind::Plain);
    Shown {
        node: true,
        name: !(scope.private && content),
        value: !secret,
        children: !matches!(kind, Kind::Password),
        actions: true,
        secret,
    }
}

/// The scope of a widget whose parent is in `parent` and of `parent_kind`.
pub fn inside(parent: Scope, parent_kind: Kind) -> Scope //@ret
//@ ensures
//@     ret.private == (parent.private || parent_kind == Kind::Private),
//@     ret.dropped == (parent.dropped || parent_kind == Kind::Password),
{
    Scope {
        private: parent.private || matches!(parent_kind, Kind::Private),
        dropped: parent.dropped || matches!(parent_kind, Kind::Password),
    }
}

//@ pub open spec fn count(s: Seq<u64>, k: u64) -> nat
//@     decreases s.len(),
//@ {
//@     if s.len() == 0 { 0 } else { count(s.drop_last(), k) + if s.last() == k { 1nat } else { 0nat } }
//@ }

/// How many of a node's siblings before it have the same key: the third part of its id, with its parent's id and its key. A
/// widget keeps its id while its key, its parent and the number of same-keyed siblings before it stay the same, whatever else
/// is added or removed around it.
pub fn occurrence(earlier: &[u64], key: u64) -> u32 //@ret
//@ requires earlier@.len() <= 512,
//@ ensures ret as nat == count(earlier@, key), ret <= earlier@.len(),
{
    let mut i: usize = 0;
    let mut n: u32 = 0;
    while i < earlier.len()
    //@ invariant
    //@     i <= earlier@.len(), earlier@.len() <= 512, n <= i,
    //@     n as nat == count(earlier@.subrange(0, i as int), key),
    //@ decreases earlier@.len() - i,
    {
        //@ assert(earlier@.subrange(0, i as int + 1).drop_last() =~= earlier@.subrange(0, i as int));
        if earlier[i] == key {
            n += 1;
        }
        i += 1;
    }
    //@ assert(earlier@.subrange(0, earlier@.len() as int) =~= earlier@);
    n
}

//@ pub proof fn count_append(s: Seq<u64>, t: Seq<u64>, k: u64)
//@     ensures count(s + t, k) == count(s, k) + count(t, k),
//@     decreases t.len(),
//@ {
//@     if t.len() == 0 {
//@         assert(s + t =~= s);
//@     } else {
//@         assert((s + t).drop_last() =~= s + t.drop_last());
//@         count_append(s, t.drop_last(), k);
//@     }
//@ }

//@ pub proof fn count_positive(s: Seq<u64>, j: int, k: u64)
//@     requires 0 <= j < s.len(), s[j] == k,
//@     ensures count(s, k) >= 1,
//@     decreases s.len(),
//@ {
//@     if j < s.len() - 1 { count_positive(s.drop_last(), j, k); }
//@ }

// Two siblings with the same key never get the same occurrence, so with their parent and key their ids differ.
//@ pub proof fn occurrences_differ(s: Seq<u64>, i: int, j: int)
//@     requires 0 <= i < j < s.len(), s[i] == s[j],
//@     ensures count(s.subrange(0, i), s[i]) < count(s.subrange(0, j), s[j]),
//@ {
//@     let a = s.subrange(0, i);
//@     let b = s.subrange(i, j);
//@     assert(s.subrange(0, j) =~= a + b);
//@     count_append(a, b, s[i]);
//@     assert(b[0] == s[i]);
//@     count_positive(b, 0, s[i]);
//@ }

// Siblings with other keys around a widget do not change its occurrence: only the ones with its key count.
//@ pub proof fn others_do_not_count(s: Seq<u64>, k: u64, other: u64)
//@     requires other != k,
//@     ensures count(s.push(other), k) == count(s, k),
//@ {
//@     assert(s.push(other).drop_last() =~= s);
//@ }

/// How many nodes a tree may hold: an app's, or a web page's.
pub fn node_limit(web: bool) -> u32 //@ret
//@ ensures ret <= 8192, !web ==> ret == 4096, web ==> ret == 8192,
{
    if web { MAX_WEB_NODES } else { MAX_NODES }
}

/// Whether another node fits: `nodes` already in the tree, its depth, the children its parent has, and the tree's
/// [`node_limit`].
pub fn admit(nodes: u32, depth: u32, siblings: u32, limit: u32) -> bool //@ret
//@ requires limit <= 8192,
//@ ensures ret == (nodes < limit && depth < 64 && siblings < 512), ret ==> nodes < 8192,
{
    nodes < limit && depth < MAX_DEPTH && siblings < MAX_CHILDREN
}

/// How many characters of a text of `chars` to keep, `used` being what the tree holds already.
pub fn keep_chars(used: u32, chars: u32) -> u32 //@ret
//@ requires used <= 262144,
//@ ensures ret <= chars, ret <= 1024, used + ret <= 262144,
//@     (chars <= 1024 && used + chars <= 262144) ==> ret == chars,
{
    let room = MAX_TOTAL_CHARS - used;
    let mut keep = if chars > MAX_TEXT_CHARS { MAX_TEXT_CHARS } else { chars };
    if keep > room {
        keep = room;
    }
    keep
}

/// The most a reply can hold for a tree that [`admit`] and [`keep_chars`] built: it fits in [`MAX_REPLY_BYTES`].
pub fn reply_bound(nodes: u32, chars: u32) -> u64 //@ret
//@ requires nodes <= 8192, chars <= 262144,
//@ ensures ret == nodes as u64 * 640 + chars as u64 * 12, ret <= 8 * 1024 * 1024,
{
    //@ assert(nodes as u64 * 640 <= 8192 * 640) by (nonlinear_arith) requires nodes <= 8192;
    //@ assert(chars as u64 * 12 <= 262144 * 12) by (nonlinear_arith) requires chars <= 262144;
    nodes as u64 * NODE_BYTES + chars as u64 * CHAR_BYTES
}

/// What Lucina asks a node to do.
#[derive(Clone, Copy)]
#[derive(Debug, PartialEq, Eq)] //@omit
pub enum Act {
    /// What a click or Enter does: press a button, flip a check box, pick and open a row.
    Click,
    /// Give it the keyboard; on a list row, select the row without opening it.
    Focus,
    /// Put this text in a field.
    SetValue,
    /// Scroll a list or a text box.
    Scroll,
}

/// Why an action was not carried out.
#[derive(Clone, Copy)]
#[derive(Debug, PartialEq, Eq)] //@omit
pub enum Refusal {
    /// The tree she read is not the one on screen: read it again.
    Stale,
    /// No node with that id in this view.
    NoNode,
    /// Pairing, sign-in, approving: only the owner's own hands.
    OwnerOnly,
    /// A password field takes no text from her.
    Password,
    /// A dialog is open and the node is behind it.
    Inert,
    /// The node does not take that action.
    Unsupported,
}

/// What is known about the node an action names, at the moment it would be carried out.
#[derive(Clone, Copy)]
#[derive(Debug, PartialEq, Eq)] //@omit
pub struct Target {
    /// The version she read matches the view now.
    pub current: bool,
    /// A node with that id is in this view.
    pub found: bool,
    pub kind: Kind,
    /// Inside `ui.owner_only`.
    pub owner_only: bool,
    /// Behind an open dialog.
    pub inert: bool,
    /// The node lists this action.
    pub supports: bool,
}

//@ pub open spec fn may(t: Target, a: Act) -> bool {
//@     t.current && t.found && !t.owner_only && !t.inert && t.supports
//@         && !(t.kind == Kind::Password && a == Act::SetValue)
//@ }

/// Whether Lucina's action `a` on `t` is carried out, and if not, why. Checked in this order, so that the answer says the
/// first thing that is wrong.
pub fn decide(t: Target, a: Act) -> Result<(), Refusal> //@ret
//@ ensures ret.is_ok() == may(t, a),
//@     t.owner_only && t.current && t.found ==> ret == Err::<(), Refusal>(Refusal::OwnerOnly),
{
    if !t.current {
        return Err(Refusal::Stale);
    }
    if !t.found {
        return Err(Refusal::NoNode);
    }
    if t.owner_only {
        return Err(Refusal::OwnerOnly);
    }
    if t.inert {
        return Err(Refusal::Inert);
    }
    if matches!(t.kind, Kind::Password) && matches!(a, Act::SetValue) {
        return Err(Refusal::Password);
    }
    if !t.supports {
        return Err(Refusal::Unsupported);
    }
    Ok(())
}

/// An action on its way to its widget during one pass of `view`.
#[derive(Clone, Copy)]
#[derive(Debug, PartialEq, Eq)] //@omit
pub struct Pending {
    /// The node id it names.
    pub node: u64,
    pub act: Act,
}

//@ pub open spec fn takes_spec(p: Option<Pending>, node: u64, allowed: bool) -> bool {
//@     p.is_some() && p.unwrap().node == node && allowed
//@ }

/// Each widget asks this with its own node id; `allowed` is [`decide`]'s answer for it. The one whose id the action names takes
/// it, and then nobody else can: the pending action is gone.
pub fn take(pending: &mut Option<Pending>, node: u64, allowed: bool) -> Option<Act> //@ret
//@ ensures
//@     ret.is_some() == takes_spec(*old(pending), node, allowed),
//@     ret.is_some() ==> ret.unwrap() == old(pending).unwrap().act && final(pending).is_none(),
//@     ret.is_none() ==> *final(pending) == *old(pending),
{
    match *pending {
        Some(p) if p.node == node && allowed => {
            *pending = None;
            Some(p.act)
        }
        _ => None,
    }
}

//@ pub open spec fn after(p: Option<Pending>, node: u64, allowed: bool) -> Option<Pending> {
//@     if takes_spec(p, node, allowed) { None } else { p }
//@ }

// How many of the widgets of one pass, in order, take the action: each is (its node id, whether it may).
//@ pub open spec fn takers(p: Option<Pending>, ws: Seq<(u64, bool)>) -> nat
//@     decreases ws.len(),
//@ {
//@     if ws.len() == 0 { 0 } else {
//@         (if takes_spec(p, ws[0].0, ws[0].1) { 1nat } else { 0nat }) + takers(after(p, ws[0].0, ws[0].1), ws.drop_first())
//@     }
//@ }

// Nothing takes an action that is gone.
//@ pub proof fn nobody_after_none(ws: Seq<(u64, bool)>)
//@     ensures takers(None, ws) == 0,
//@     decreases ws.len(),
//@ {
//@     if ws.len() > 0 { nobody_after_none(ws.drop_first()); }
//@ }

// In one pass at most one widget takes an action, and only a widget with the id it names and the right to it.
//@ pub proof fn at_most_one_taker(p: Option<Pending>, ws: Seq<(u64, bool)>)
//@     ensures takers(p, ws) <= 1,
//@         takers(p, ws) == 1 ==> exists|i: int| #![auto] 0 <= i < ws.len() && ws[i].0 == p.unwrap().node && ws[i].1,
//@     decreases ws.len(),
//@ {
//@     if ws.len() > 0 {
//@         if takes_spec(p, ws[0].0, ws[0].1) {
//@             nobody_after_none(ws.drop_first());
//@         } else {
//@             at_most_one_taker(p, ws.drop_first());
//@             if takers(p, ws) == 1 {
//@                 let i = choose|i: int| #![auto] 0 <= i < ws.drop_first().len() && ws.drop_first()[i].0 == p.unwrap().node && ws.drop_first()[i].1;
//@                 assert(ws[i + 1] == ws.drop_first()[i]);
//@             }
//@         }
//@     }
//@ }

/// Whether an input event reaches a widget: inside `ui.owner_only`, nothing from Lucina's injection device does.
pub fn input_reaches(owner_only: bool, injected: bool) -> bool //@ret
//@ ensures ret == !(owner_only && injected),
{
    !(owner_only && injected)
}
