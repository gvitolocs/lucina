//! A screen as an AccessKit tree: the [`Builder`] toolkits fill while `view` runs, the [`Snapshot`] it makes, its version, its
//! text for Lucina, and [`Snapshot::from_update`] for a toolkit that makes its own AccessKit update (egui).
//!
//! What of a widget is shown, how ids are told apart, the limits and which actions are allowed are decided in [`crate::rules`],
//! which Verus checks; this file only carries the decisions out.

use std::collections::{BTreeMap, HashMap, HashSet};

use accesskit::{Action, Node, NodeId, Rect, Role, Toggled, TreeId, TreeInfo, TreeUpdate};
use serde::{Deserialize, Serialize};

use crate::rules::{self, Act, Kind, Refusal, Scope, Target};

const FNV_START: u64 = 0xcbf2_9ce4_8422_2325;

fn fnv(mut h: u64, bytes: &[u8]) -> u64 {
    for &b in bytes {
        h = (h ^ b as u64).wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

/// A key from a string, for widgets the toolkit names itself (`"window"`, `"label"`).
pub fn key(s: &str) -> u64 {
    fnv(FNV_START, s.as_bytes())
}

/// A node's id: its parent's, its key, and how many siblings before it had the same key ([`rules::occurrence`]).
pub fn mix(parent: u64, key: u64, occurrence: u32) -> u64 {
    let h = fnv(FNV_START, &parent.to_le_bytes());
    let h = fnv(h, &key.to_le_bytes());
    fnv(h, &occurrence.to_le_bytes())
}

/// A node id as Lucina sees and sends it: 16 hex digits (a JSON number would lose bits past 2^53 in some clients).
pub fn id_text(id: u64) -> String {
    format!("{id:016x}")
}

pub fn parse_id(s: &str) -> Option<u64> {
    let s = s.trim().trim_start_matches('#');
    (!s.is_empty() && s.len() <= 16).then(|| u64::from_str_radix(s, 16).ok()).flatten()
}

/// What Juno adds to a node beside AccessKit's fields.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Extra {
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub owner_only: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub private: bool,
    /// `destructive` for buttons drawn in the danger style.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub class: Option<String>,
    /// `content` when the node's text is the owner's data (a note's title, a field's text) rather than the app's own label: Lucina
    /// reads it as data, never as instructions.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provenance: Option<String>,
    /// Something of the node is held back: a password's value, or a value or drawn text inside a private area. Its role and
    /// name (where the name is the app's label) are there; what it holds is not.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub secret: bool,
    /// What the node shows, as the app's intents name it (`note:n_42`), so that "open this one" resolves from the screen.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entity: Option<String>,
    /// A field of the app's own state that it publishes for her (`ui.state`), not something drawn: a game's map id, a player's
    /// position. Read it like any value.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub state: bool,
}

impl Extra {
    fn is_empty(&self) -> bool {
        *self == Extra::default()
    }
}

/// One widget, as the toolkit describes it to the builder.
#[derive(Clone, Debug)]
pub struct Widget<'a> {
    /// The widget's own key: its toolkit id, or a key of its role for widgets without one.
    pub key: u64,
    pub role: Role,
    pub name: &'a str,
    pub value: Option<&'a str>,
    /// x, y, width, height in screen pixels.
    pub bounds: (i32, i32, i32, i32),
    pub actions: &'a [Action],
    pub kind: Kind,
    pub toggled: Option<bool>,
    pub selected: Option<bool>,
    pub disabled: bool,
    pub focused: bool,
    /// Behind an open dialog.
    pub inert: bool,
    /// The name is the owner's data, not the app's label.
    pub content: bool,
    pub destructive: bool,
    /// What it shows, as the app's intents name it (`note:n_42`).
    pub entity: Option<&'a str>,
    /// A container with no name of its own (a list row) is named by the text inside it when it closes, so that its name, and
    /// the tree's version, change when the row shows something else.
    pub name_from_children: bool,
}

impl<'a> Widget<'a> {
    pub fn new(key: u64, role: Role) -> Widget<'a> {
        Widget {
            key,
            role,
            name: "",
            value: None,
            bounds: (0, 0, 0, 0),
            actions: &[],
            kind: Kind::Plain,
            toggled: None,
            selected: None,
            disabled: false,
            focused: false,
            inert: false,
            content: false,
            destructive: false,
            entity: None,
            name_from_children: false,
        }
    }
}

/// Where a widget went: its node id, and whether an action may reach it (for [`rules::take`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Placed {
    pub id: u64,
    pub may_act: bool,
    /// It is in the tree.
    pub recorded: bool,
}

/// What [`rules::decide`] needs to know about a node in the tree.
#[derive(Clone, Debug)]
pub struct Facts {
    pub kind: Kind,
    /// Inside a private area: its values and drawn text are held back.
    pub in_private: bool,
    pub owner_only: bool,
    pub inert: bool,
    pub actions: Vec<Action>,
    pub role: Role,
    pub name: String,
}

struct Open {
    /// Its place in `nodes`, when it is recorded.
    index: Option<usize>,
    id: u64,
    keys: Vec<u64>,
    /// Inside a private area, or a password field.
    scope: Scope,
    kind: Kind,
    owner_only: bool,
    depth: u32,
    name_from_children: bool,
}

/// Fills a tree while `view` runs. Every widget is placed, recorded or not, so that its id is the same in the pass that
/// records the tree and in the pass that carries an action out.
pub struct Builder {
    app: String,
    nodes: Vec<(NodeId, Node)>,
    children: Vec<Vec<NodeId>>,
    extras: BTreeMap<String, Extra>,
    facts: HashMap<u64, Facts>,
    stack: Vec<Open>,
    used_chars: u32,
    truncated: bool,
    seen: HashSet<u64>,
    focus: Option<u64>,
    owner_only: u32,
    collisions: u32,
}

/// The root's key.
pub fn root_key() -> u64 {
    key("window")
}

impl Builder {
    /// `app` names the root, a window as wide as the screen.
    pub fn new(app: &str, w: i32, h: i32) -> Builder {
        let mut b = Builder {
            app: app.to_owned(),
            nodes: Vec::new(),
            children: Vec::new(),
            extras: BTreeMap::new(),
            facts: HashMap::new(),
            stack: Vec::new(),
            used_chars: 0,
            truncated: false,
            seen: HashSet::new(),
            focus: None,
            owner_only: 0,
            collisions: 0,
        };
        let id = mix(0, root_key(), 0);
        let mut node = Node::new(Role::Window);
        let name = b.cut(app);
        node.set_label(name);
        node.set_bounds(rect((0, 0, w, h)));
        b.nodes.push((NodeId(id), node));
        b.children.push(Vec::new());
        b.seen.insert(id);
        b.stack.push(Open {
            index: Some(0),
            id,
            keys: Vec::new(),
            scope: Scope { private: false, dropped: false },
            kind: Kind::Plain,
            owner_only: false,
            depth: 0,
            name_from_children: false,
        });
        b
    }

    fn cut(&mut self, s: &str) -> String {
        let chars = s.chars().count().min(u32::MAX as usize) as u32;
        let keep = rules::keep_chars(self.used_chars, chars);
        self.used_chars += keep;
        if keep < chars {
            self.truncated = true;
        }
        s.chars().take(keep as usize).collect()
    }

    /// Everything from here to [`end_owner_only`](Self::end_owner_only) only the owner's own hands can work.
    pub fn begin_owner_only(&mut self) {
        self.owner_only += 1;
    }

    pub fn end_owner_only(&mut self) {
        self.owner_only = self.owner_only.saturating_sub(1);
    }

    /// Whether widgets placed now are inside `owner_only`.
    pub fn in_owner_only(&self) -> bool {
        self.owner_only > 0 || self.stack.last().is_some_and(|o| o.owner_only)
    }

    /// A widget with no children.
    pub fn leaf(&mut self, w: &Widget) -> Placed {
        self.place(w, false)
    }

    /// A widget with children: every widget until the matching [`close`](Self::close) goes inside it.
    pub fn open(&mut self, w: &Widget) -> Placed {
        self.place(w, true)
    }

    pub fn close(&mut self) {
        if self.stack.len() > 1
            && let Some(open) = self.stack.pop()
            && open.name_from_children
            && let Some(i) = open.index
            && self.nodes[i].1.label().is_none()
        {
            let mut name = String::new();
            self.collect_text(i, &mut name);
            if !name.is_empty() {
                let name = self.cut(&name);
                self.nodes[i].1.set_label(name);
            }
        }
    }

    /// The names of the nodes under `i`, joined, up to 120 characters.
    fn collect_text(&self, i: usize, out: &mut String) {
        for c in &self.children[i] {
            if out.chars().count() >= 120 {
                return;
            }
            let Some(ci) = self.nodes.iter().position(|(id, _)| id == c) else { continue };
            if let Some(l) = self.nodes[ci].1.label() {
                if !out.is_empty() {
                    out.push_str(" · ");
                }
                out.extend(l.chars().take(120));
            }
            self.collect_text(ci, out);
        }
    }

    /// One field of the app's own state, published for her: `name` and its value, with no pixels behind it. A field the app
    /// would not show the owner either has no business here; secret fields are not published at all.
    pub fn state(&mut self, name: &str, value: &str) -> Placed {
        let mut w = Widget::new(key("state"), Role::Label);
        w.name = name;
        w.value = Some(value);
        let placed = self.leaf(&w);
        if placed.recorded {
            self.extras.entry(id_text(placed.id)).or_default().state = true;
        }
        placed
    }

    /// Tags the widget placed last with what it shows, as the app's intents name it (`note:n_42`).
    pub fn entity_last(&mut self, entity: &str) {
        if self.nodes.len() > 1 {
            let id = id_text(self.nodes[self.nodes.len() - 1].0.0);
            self.extras.entry(id).or_default().entity = Some(entity.chars().take(120).collect());
        }
    }

    /// Names the widget placed last, when the toolkit could not: a custom area (`ui.hit`), a slider.
    pub fn describe_last(&mut self, name: &str) {
        if self.nodes.len() > 1 {
            let name = self.cut(name);
            let i = self.nodes.len() - 1;
            self.nodes[i].1.set_label(name);
            let id = self.nodes[i].0.0;
            if let Some(f) = self.facts.get_mut(&id) {
                f.name = self.nodes[i].1.label().unwrap_or_default().chars().take(80).collect();
            }
        }
    }

    /// How many nodes are open, root included: for putting the stack back after a closure that may have left some open.
    pub fn depth(&self) -> usize {
        self.stack.len()
    }

    pub fn close_to(&mut self, depth: usize) {
        while self.stack.len() > depth.max(1) {
            self.close();
        }
    }

    fn place(&mut self, w: &Widget, container: bool) -> Placed {
        let owner_only = self.in_owner_only();
        let parent = self.stack.last_mut().expect("the root is always open");
        let siblings = parent.keys.len();
        let (id, fits_siblings) = if siblings < rules::MAX_CHILDREN as usize {
            let occ = rules::occurrence(&parent.keys, w.key);
            parent.keys.push(w.key);
            (mix(parent.id, w.key, occ), true)
        } else {
            (mix(parent.id, w.key, u32::MAX), false)
        };
        let scope = rules::inside(parent.scope, parent.kind);
        let depth = parent.depth + 1;
        let parent_index = parent.index;
        let shown = rules::shown(w.kind, scope, w.content);
        let mut recorded = false;
        if shown.node
            && fits_siblings
            && let Some(pi) = parent_index
        {
            let kids = self.children[pi].len().min(u32::MAX as usize) as u32;
            if !rules::admit(self.nodes.len() as u32, depth, kids, rules::node_limit(false)) {
                self.truncated = true;
            } else if !self.seen.insert(id) {
                // Two widgets with the same id: the second stays out of the tree and takes no action.
                self.collisions += 1;
            } else {
                recorded = true;
            }
        }
        let may_act = !scope.dropped && !owner_only && !w.inert && recorded;
        let index = if recorded {
            let mut node = Node::new(w.role);
            if shown.name && !w.name.is_empty() {
                let name = self.cut(w.name);
                node.set_label(name);
            }
            if shown.value
                && let Some(v) = w.value
            {
                let v = self.cut(v);
                node.set_value(v);
            }
            node.set_bounds(rect(w.bounds));
            let mut actions = Vec::new();
            if shown.actions {
                for &a in w.actions {
                    node.add_action(a);
                    actions.push(a);
                }
            }
            if let Some(t) = w.toggled {
                node.set_toggled(if t { Toggled::True } else { Toggled::False });
            }
            if let Some(s) = w.selected {
                node.set_selected(s);
            }
            if w.disabled {
                node.set_disabled();
            }
            if w.focused {
                self.focus = Some(id);
            }
            let extra = Extra {
                owner_only,
                private: matches!(w.kind, Kind::Private),
                class: w.destructive.then(|| "destructive".to_owned()),
                provenance: (w.content || (shown.value && w.value.is_some())).then(|| "content".to_owned()),
                secret: shown.secret,
                entity: w.entity.map(|e| e.chars().take(120).collect()),
                state: false,
            };
            if !extra.is_empty() {
                self.extras.insert(id_text(id), extra);
            }
            self.facts.insert(
                id,
                Facts {
                    kind: w.kind,
                    in_private: scope.private,
                    owner_only,
                    inert: w.inert,
                    actions,
                    role: w.role,
                    name: if shown.name { w.name.chars().take(80).collect() } else { String::new() },
                },
            );
            let pi = parent_index.expect("recorded nodes have a recorded parent");
            self.children[pi].push(NodeId(id));
            self.nodes.push((NodeId(id), node));
            self.children.push(Vec::new());
            Some(self.nodes.len() - 1)
        } else {
            None
        };
        if container {
            self.stack.push(Open {
                index,
                id,
                keys: Vec::new(),
                scope,
                kind: w.kind,
                owner_only,
                depth,
                name_from_children: w.name_from_children,
            });
        }
        Placed { id, may_act, recorded }
    }

    pub fn finish(mut self) -> Snapshot {
        if self.collisions > 0 {
            eprintln!("lucina-ax: {}: {} widgets share an id with another and are left out of the tree", self.app, self.collisions);
        }
        for (i, kids) in std::mem::take(&mut self.children).into_iter().enumerate() {
            if !kids.is_empty() {
                self.nodes[i].1.set_children(kids);
            }
        }
        let root = self.nodes[0].0;
        let update = TreeUpdate {
            nodes: self.nodes,
            tree: Some(TreeInfo { root, toolkit_name: Some("lucina-app".into()), toolkit_version: None }),
            tree_id: TreeId::ROOT,
            focus: NodeId(self.focus.unwrap_or(root.0)),
        };
        Snapshot::new(self.app, update, self.extras, self.facts, self.truncated)
    }
}

fn rect((x, y, w, h): (i32, i32, i32, i32)) -> Rect {
    Rect { x0: x as f64, y0: y as f64, x1: (x + w.max(0)) as f64, y1: (y + h.max(0)) as f64 }
}

/// One node's change between two trees ([`Snapshot::diff`]).
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Change {
    pub kind: ChangeKind,
    pub id: u64,
    #[serde(serialize_with = "ser_role")]
    pub role: Role,
    pub name: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ChangeKind {
    Added,
    Removed,
    Changed,
}

fn ser_role<S: serde::Serializer>(r: &Role, s: S) -> Result<S::Ok, S::Error> {
    s.serialize_str(&role_name(*r))
}

/// Where a tree from another toolkit comes from ([`Snapshot::from_update_with`]).
#[derive(Clone, Debug)]
pub struct Source<'a> {
    /// `egui`, `servo`.
    pub toolkit: &'a str,
    /// Put on every node with text: `web https://example.org` for a page, which she reads as data, never as instructions.
    pub provenance: Option<&'a str>,
    /// A web page: the larger node limit.
    pub web: bool,
}

/// One screen's tree.
#[derive(Clone, Debug)]
pub struct Snapshot {
    pub app: String,
    pub update: TreeUpdate,
    /// Juno's additions, by node id text.
    pub extras: BTreeMap<String, Extra>,
    pub facts: HashMap<u64, Facts>,
    pub truncated: bool,
    pub version: String,
}

/// An action Lucina asked for, checked against the tree, ready for the widgets.
#[derive(Clone, Debug, PartialEq)]
pub struct Planned {
    pub pending: rules::Pending,
    pub action: Action,
    pub value: Option<String>,
}

/// Why an `act` request was not carried out.
#[derive(Clone, Debug, PartialEq)]
pub enum Refused {
    Rule(Refusal),
    /// The request itself is wrong: an id that is not one, an action with no such name.
    Bad(String),
}

impl Refused {
    pub fn why(&self) -> String {
        match self {
            Refused::Rule(r) => why(*r).to_owned(),
            Refused::Bad(s) => s.clone(),
        }
    }

    pub fn stale(&self) -> bool {
        matches!(self, Refused::Rule(Refusal::Stale))
    }

    /// The reply's `error`: the same codes as stage F's Servo socket, so that one reader serves both.
    pub fn code(&self) -> &'static str {
        match self {
            Refused::Rule(Refusal::Stale) => "stale",
            Refused::Rule(Refusal::NoNode) => "no_node",
            Refused::Rule(Refusal::OwnerOnly) => "refused_owner_only",
            Refused::Rule(Refusal::Password) => "refused_sensitive",
            Refused::Rule(Refusal::Inert) => "inert",
            Refused::Rule(Refusal::Unsupported) => "not_supported",
            Refused::Bad(_) => "bad_request",
        }
    }
}

pub fn why(r: Refusal) -> &'static str {
    match r {
        Refusal::Stale => "the view changed: read the tree again",
        Refusal::NoNode => "no node with that id in this view",
        Refusal::OwnerOnly => "only the owner can do that, by hand",
        Refusal::Password => "a password field takes no text from Lucina",
        Refusal::Inert => "a dialog is open in front of it",
        Refusal::Unsupported => "that node does not take this action",
    }
}

/// The action names of the protocol, and what each is in AccessKit and in the rules.
pub fn parse_action(name: &str) -> Option<(Act, Action)> {
    Some(match name {
        "click" | "press" => (Act::Click, Action::Click),
        "focus" | "select" => (Act::Focus, Action::Focus),
        "blur" => (Act::Focus, Action::Blur),
        "scroll_into_view" => (Act::Scroll, Action::ScrollIntoView),
        "set_value" => (Act::SetValue, Action::SetValue),
        "scroll_down" => (Act::Scroll, Action::ScrollDown),
        "scroll_up" => (Act::Scroll, Action::ScrollUp),
        _ => return None,
    })
}

pub fn action_name(a: Action) -> &'static str {
    match a {
        Action::Click => "click",
        Action::Focus => "focus",
        Action::Blur => "blur",
        Action::SetValue => "set_value",
        Action::ScrollDown => "scroll_down",
        Action::ScrollUp => "scroll_up",
        Action::ScrollIntoView => "scroll_into_view",
        Action::Expand => "expand",
        Action::Collapse => "collapse",
        Action::Increment => "increment",
        Action::Decrement => "decrement",
        Action::ShowContextMenu => "context_menu",
        Action::ReplaceSelectedText => "replace_selected_text",
        Action::SetTextSelection => "set_text_selection",
        _ => "other",
    }
}

fn role_name(r: Role) -> String {
    let d = format!("{r:?}");
    let mut out = String::with_capacity(d.len() + 4);
    for (i, c) in d.chars().enumerate() {
        if c.is_ascii_uppercase() {
            if i > 0 {
                out.push('_');
            }
            out.push(c.to_ascii_lowercase());
        } else {
            out.push(c);
        }
    }
    out
}

/// A node another toolkit sends: a password field, or a field it marks secret (`class_name` "secret": Servo's one-time codes and
/// card numbers), is held back as a password is, whatever value came with it.
fn kind_of(n: &Node) -> Kind {
    if n.role() == Role::PasswordInput || n.class_name() == Some("secret") { Kind::Password } else { Kind::Plain }
}

impl Snapshot {
    fn new(
        app: String,
        update: TreeUpdate,
        extras: BTreeMap<String, Extra>,
        facts: HashMap<u64, Facts>,
        truncated: bool,
    ) -> Snapshot {
        let mut s = Snapshot { app, update, extras, facts, truncated, version: String::new() };
        s.version = s.compute_version();
        s
    }

    /// A tree another toolkit made (egui's `accesskit_update`, a full tree), made safe by the same rules as ours: a password
    /// field keeps no value and no children, the size limits hold, text is cut. Nodes the root does not reach are dropped.
    pub fn from_update(app: &str, update: &TreeUpdate) -> Snapshot {
        Snapshot::from_update_with(app, update, &Source { toolkit: "egui", provenance: None, web: false })
    }

    /// [`from_update`](Self::from_update) for a page or another toolkit: `source.provenance` (`web https://example.org`) goes on
    /// every node with text, and a web page may hold up to [`rules::MAX_WEB_NODES`] nodes.
    pub fn from_update_with(app: &str, update: &TreeUpdate, source: &Source) -> Snapshot {
        let by_id: HashMap<NodeId, &Node> = update.nodes.iter().map(|(i, n)| (*i, n)).collect();
        let root = update.tree.as_ref().map(|t| t.root).or_else(|| update.nodes.first().map(|n| n.0));
        let mut out: Vec<(NodeId, Node)> = Vec::new();
        let mut facts = HashMap::new();
        let mut extras = BTreeMap::new();
        let mut used = 0u32;
        let mut truncated = false;
        let mut cut = |s: &str, truncated: &mut bool| -> String {
            let chars = s.chars().count().min(u32::MAX as usize) as u32;
            let keep = rules::keep_chars(used, chars);
            used += keep;
            if keep < chars {
                *truncated = true;
            }
            s.chars().take(keep as usize).collect()
        };
        // Breadth first, so that a cut tree keeps what is near the top.
        let mut queue: std::collections::VecDeque<(NodeId, Scope, u32)> = std::collections::VecDeque::new();
        let mut seen = HashSet::new();
        if let Some(r) = root {
            queue.push_back((r, Scope { private: false, dropped: false }, 0));
        }
        let mut index: HashMap<NodeId, usize> = HashMap::new();
        let mut parent_of: HashMap<NodeId, NodeId> = HashMap::new();
        while let Some((id, scope, depth)) = queue.pop_front() {
            let Some(src) = by_id.get(&id) else { continue };
            if !seen.insert(id) {
                continue;
            }
            let kind = kind_of(src);
            let shown = rules::shown(kind, scope, false);
            if !shown.node {
                continue;
            }
            let siblings = parent_of
                .get(&id)
                .and_then(|p| index.get(p))
                .map_or(0, |&pi| out[pi].1.children().len().min(u32::MAX as usize) as u32);
            if !rules::admit(out.len() as u32, depth, siblings, rules::node_limit(source.web)) {
                truncated = true;
                continue;
            }
            let mut node = Node::new(src.role());
            if let Some(l) = src.label() {
                node.set_label(cut(l, &mut truncated));
            }
            if shown.value
                && let Some(v) = src.value()
            {
                node.set_value(cut(v, &mut truncated));
            }
            if let Some(b) = src.bounds() {
                node.set_bounds(b);
            }
            // What a page needs her to know beside name and value: where a link goes, a field's hint, a heading's level.
            if let Some(u) = src.url() {
                node.set_url(cut(u, &mut truncated));
            }
            if let Some(p) = src.placeholder() {
                node.set_placeholder(cut(p, &mut truncated));
            }
            if let Some(d) = src.description() {
                node.set_description(cut(d, &mut truncated));
            }
            if let Some(l) = src.level() {
                node.set_level(l);
            }
            if let Some(e) = src.is_expanded() {
                node.set_expanded(e);
            }
            if src.is_required() {
                node.set_required();
            }
            if src.is_read_only() {
                node.set_read_only();
            }
            let mut actions = Vec::new();
            if shown.actions {
                for a in [
                    Action::Click,
                    Action::Focus,
                    Action::Blur,
                    Action::SetValue,
                    Action::ScrollDown,
                    Action::ScrollUp,
                    Action::ScrollIntoView,
                ] {
                    if src.supports_action(a) {
                        node.add_action(a);
                        actions.push(a);
                    }
                }
            }
            if let Some(t) = src.toggled() {
                node.set_toggled(t);
            }
            if let Some(s) = src.is_selected() {
                node.set_selected(s);
            }
            if src.is_disabled() {
                node.set_disabled();
            }
            let child_scope = rules::inside(scope, kind);
            if shown.children {
                for &c in src.children() {
                    parent_of.insert(c, id);
                    queue.push_back((c, child_scope, depth + 1));
                }
            }
            facts.insert(
                id.0,
                Facts {
                    kind,
                    in_private: scope.private,
                    owner_only: false,
                    inert: false,
                    actions,
                    role: src.role(),
                    name: src.label().unwrap_or_default().chars().take(80).collect(),
                },
            );
            let has_text = src.label().is_some() || src.description().is_some() || (src.value().is_some() && shown.value);
            if has_text || shown.secret {
                let provenance = match source.provenance {
                    Some(p) if has_text => Some(p.to_owned()),
                    _ => (src.value().is_some() && shown.value).then(|| "content".into()),
                };
                extras.insert(id_text(id.0), Extra { provenance, secret: shown.secret, ..Extra::default() });
            }
            if let Some(&p) = parent_of.get(&id)
                && let Some(&pi) = index.get(&p)
            {
                out[pi].1.push_child(id);
            }
            index.insert(id, out.len());
            out.push((id, node));
        }
        let root = out.first().map(|n| n.0).unwrap_or(NodeId(0));
        let focus = if index.contains_key(&update.focus) { update.focus } else { root };
        let update = TreeUpdate {
            nodes: out,
            tree: Some(TreeInfo { root, toolkit_name: Some(source.toolkit.into()), toolkit_version: None }),
            tree_id: TreeId::ROOT,
            focus,
        };
        Snapshot::new(app.to_owned(), update, extras, facts, truncated)
    }

    /// Hashes what can be acted on, in order: ids, roles, names and actions of the nodes that take actions. Static text and
    /// values are left out, so that a clock or a counter does not make every action stale; a sort that moves rows does.
    fn compute_version(&self) -> String {
        let mut h = FNV_START;
        for (id, n) in &self.update.nodes {
            let Some(f) = self.facts.get(&id.0) else { continue };
            if f.actions.is_empty() {
                continue;
            }
            h = fnv(h, &id.0.to_le_bytes());
            h = fnv(h, role_name(n.role()).as_bytes());
            h = fnv(h, n.label().unwrap_or_default().as_bytes());
            for a in &f.actions {
                h = fnv(h, action_name(*a).as_bytes());
            }
            h = fnv(h, &[f.owner_only as u8, f.inert as u8]);
        }
        format!("{:012x}", h & 0xffff_ffff_ffff)
    }

    /// The node count.
    pub fn len(&self) -> usize {
        self.update.nodes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.update.nodes.is_empty()
    }

    /// The node with this id.
    pub fn node(&self, id: u64) -> Option<&Node> {
        self.update.nodes.iter().find(|(i, _)| i.0 == id).map(|(_, n)| n)
    }

    /// The first node, depth first, with this role and name: for tests and for finding a button by what it says.
    pub fn find(&self, role: Role, name: &str) -> Option<u64> {
        self.update.nodes.iter().find(|(_, n)| n.role() == role && n.label() == Some(name)).map(|(i, _)| i.0)
    }

    /// Checks Lucina's request against this tree, the one on screen now.
    pub fn plan(&self, id: &str, action: &str, value: Option<&str>, version: &str) -> Result<Planned, Refused> {
        let node = parse_id(id).ok_or_else(|| Refused::Bad(format!("{id:?} is not a node id (16 hex digits)")))?;
        let (act, ax) = parse_action(action).ok_or_else(|| {
            Refused::Bad(format!("no action {action:?}: click, focus, blur, set_value, scroll_down, scroll_up or scroll_into_view"))
        })?;
        if act == Act::SetValue && value.is_none() {
            return Err(Refused::Bad("set_value needs a value".into()));
        }
        let facts = self.facts.get(&node);
        let t = Target {
            current: version.trim() == self.version,
            found: facts.is_some(),
            kind: facts.map_or(Kind::Plain, |f| f.kind),
            owner_only: facts.is_some_and(|f| f.owner_only),
            inert: facts.is_some_and(|f| f.inert),
            supports: facts.is_some_and(|f| f.actions.contains(&ax)),
        };
        rules::decide(t, act).map_err(Refused::Rule)?;
        Ok(Planned { pending: rules::Pending { node, act }, action: ax, value: value.map(str::to_owned) })
    }

    /// The tree as text, one line per node, for Lucina: role, id, name, value, states and actions. Containers with no name and
    /// no actions are left out and their children moved up.
    pub fn text(&self) -> String {
        let by_id: HashMap<NodeId, &Node> = self.update.nodes.iter().map(|(i, n)| (*i, n)).collect();
        let mut out = String::new();
        let root = self.update.tree.as_ref().map(|t| t.root);
        out.push_str(&format!(
            "{}  [version {}, {} nodes{}]\n",
            self.app,
            self.version,
            self.len(),
            if self.truncated { ", cut to the limits" } else { "" }
        ));
        if let Some(r) = root
            && let Some(n) = by_id.get(&r)
        {
            for &c in n.children() {
                self.line(&by_id, c, 1, &mut out);
            }
        }
        out
    }

    fn line(&self, by_id: &HashMap<NodeId, &Node>, id: NodeId, depth: usize, out: &mut String) {
        let Some(n) = by_id.get(&id) else { return };
        let facts = self.facts.get(&id.0);
        let extra = self.extras.get(&id_text(id.0));
        let actions: Vec<&str> = facts.map(|f| f.actions.iter().map(|a| action_name(*a)).collect()).unwrap_or_default();
        let plain = matches!(n.role(), Role::GenericContainer | Role::Group)
            && n.label().is_none()
            && actions.is_empty()
            && !extra.is_some_and(|e| e.private);
        let mut depth_kids = depth;
        if !plain {
            let mut l = "  ".repeat(depth);
            l.push_str(&role_name(n.role()));
            l.push(' ');
            l.push_str(&id_text(id.0));
            if let Some(name) = n.label() {
                l.push_str(&format!(" {:?}", name));
            }
            if let Some(v) = n.value() {
                l.push_str(&format!(" = {:?}", v));
            }
            if let Some(p) = n.placeholder() {
                l.push_str(&format!(" hint {:?}", p));
            }
            if let Some(u) = n.url() {
                l.push_str(&format!(" -> {u}"));
            }
            if let Some(lv) = n.level() {
                l.push_str(&format!(" level {lv}"));
            }
            match n.is_expanded() {
                Some(true) => l.push_str(" expanded"),
                Some(false) => l.push_str(" collapsed"),
                None => {}
            }
            if n.is_required() {
                l.push_str(" required");
            }
            if n.is_read_only() {
                l.push_str(" read-only");
            }
            if extra.is_some_and(|e| e.state) {
                l.push_str(" (app state)");
            }
            if n.role() == Role::PasswordInput {
                l.push_str(" (password)");
            }
            if extra.is_some_and(|e| e.private) {
                l.push_str(" (private area: values and text inside are held back)");
            } else if extra.is_some_and(|e| e.secret) && n.role() != Role::PasswordInput {
                l.push_str(" (secret)");
            }
            if let Some(e) = extra.and_then(|e| e.entity.as_deref()) {
                l.push_str(&format!(" entity {e}"));
            }
            match n.toggled() {
                Some(Toggled::True) => l.push_str(" checked"),
                Some(Toggled::False) => l.push_str(" unchecked"),
                _ => {}
            }
            if n.is_selected() == Some(true) {
                l.push_str(" selected");
            }
            if n.is_disabled() {
                l.push_str(" disabled");
            }
            if self.update.focus == id {
                l.push_str(" focused");
            }
            if facts.is_some_and(|f| f.inert) {
                l.push_str(" behind-dialog");
            }
            if extra.is_some_and(|e| e.owner_only) {
                l.push_str(" owner-only");
            }
            if extra.and_then(|e| e.class.as_deref()) == Some("destructive") {
                l.push_str(" destructive");
            }
            if !actions.is_empty() && !facts.is_some_and(|f| f.owner_only || f.inert) {
                l.push_str("  actions: ");
                l.push_str(&actions.join(", "));
            }
            l.push('\n');
            out.push_str(&l);
            depth_kids = depth + 1;
        }
        for &c in n.children() {
            self.line(by_id, c, depth_kids, out);
        }
    }

    /// What changed from this tree to `newer`, for stage D's `node.changed` events: nodes added, removed, or whose role, name,
    /// value, states or children changed. Held-back values and text are in neither tree, so they never show up here.
    pub fn diff(&self, newer: &Snapshot) -> Vec<Change> {
        let old: HashMap<u64, &Node> = self.update.nodes.iter().map(|(i, n)| (i.0, n)).collect();
        let new: HashMap<u64, &Node> = newer.update.nodes.iter().map(|(i, n)| (i.0, n)).collect();
        let mut out = Vec::new();
        let change = |kind, id: u64, n: &Node| Change { kind, id, role: n.role(), name: n.label().map(str::to_owned) };
        for (id, n) in &newer.update.nodes {
            match old.get(&id.0) {
                None => out.push(change(ChangeKind::Added, id.0, n)),
                Some(o) if *o != n => out.push(change(ChangeKind::Changed, id.0, n)),
                _ => {}
            }
        }
        for (id, n) in &self.update.nodes {
            if !new.contains_key(&id.0) {
                out.push(change(ChangeKind::Removed, id.0, n));
            }
        }
        out
    }

    /// The reply to `tree`, and the body of the others.
    pub fn reply(&self, pid: u32) -> serde_json::Value {
        serde_json::json!({
            "ok": true,
            "app": self.app,
            "pid": pid,
            "version": self.version,
            "truncated": self.truncated,
            "text": self.text(),
            "juno": self.extras,
            "update": self.update,
        })
    }
}
