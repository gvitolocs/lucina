//! The tree Lucina reads, and her actions by node id (docs/research/juno-ai-native.md, Layer 1), through the Harness: what
//! leaves the app, which actions reach which widget, and that a normal pass is unchanged.

use std::time::Duration;

use lucina_app::{App, Harness, ListState, Rect, Rgb, TextEdit, Ui};
use lucina_ax::accesskit::Role;
use lucina_ax::rules::Refusal;
use lucina_ax::{Refused, Snapshot};

fn json(s: &Snapshot) -> String {
    serde_json::to_string(&s.reply(1)).unwrap()
}

fn centre(s: &Snapshot, id: u64) -> (i32, i32) {
    let b = s
        .node(id)
        .and_then(|n| n.bounds())
        .expect("the node has bounds");
    (((b.x0 + b.x1) / 2.0) as i32, ((b.y0 + b.y1) / 2.0) as i32)
}

fn count(s: &Snapshot, role: Role, name: Option<&str>) -> usize {
    s.update
        .nodes
        .iter()
        .filter(|(_, n)| n.role() == role && name.is_none_or(|w| n.label() == Some(w)))
        .count()
}

const GREEN: Rgb = Rgb(0x00ff00);

#[derive(Default)]
struct Counter {
    n: u32,
    above: bool,
}

impl App for Counter {
    type Msg = ();

    fn view(&mut self, ui: &mut Ui) {
        if self.above {
            ui.label("Above");
        }
        if ui.button("Add") {
            self.n += 1;
        }
        ui.button("Same");
        ui.button("Same");
        if self.n > 0 {
            ui.fill(Rect::new(0, 500, 50, 50), GREEN);
        }
    }
}

#[test]
fn button_by_id_changes_state() {
    let mut h = Harness::new(Counter::default(), 800, 600);
    assert_ne!(h.core.frame.get(10, 510), Some(GREEN));
    let id = h
        .tree()
        .find(Role::Button, "Add")
        .expect("the button is in the tree");
    let after = h.act(id, "click", None).unwrap();
    assert_eq!(h.app().n, 1);
    assert_eq!(
        h.core.frame.get(10, 510),
        Some(GREEN),
        "the frame shows the press"
    );
    assert_eq!(after.find(Role::Button, "Add"), Some(id));
}

#[test]
fn ids_are_stable() {
    let mut h = Harness::new(Counter::default(), 800, 600);
    let a = h.tree();
    let b = h.tree();
    assert_eq!(a.version, b.version);
    let ids = |s: &Snapshot| s.update.nodes.iter().map(|(i, _)| i.0).collect::<Vec<_>>();
    assert_eq!(ids(&a), ids(&b));

    let add = a.find(Role::Button, "Add").unwrap();
    h.app().above = true;
    let c = h.tree();
    assert!(c.find(Role::Label, "Above").is_some());
    assert_eq!(
        c.find(Role::Button, "Add"),
        Some(add),
        "a label above does not move the button's id"
    );

    let same: Vec<u64> = c
        .update
        .nodes
        .iter()
        .filter(|(_, n)| n.role() == Role::Button && n.label() == Some("Same"))
        .map(|(i, _)| i.0)
        .collect();
    assert_eq!(same.len(), 2);
    assert_ne!(same[0], same[1]);
}

struct Login {
    pass: TextEdit,
}

impl App for Login {
    type Msg = ();

    fn view(&mut self, ui: &mut Ui) {
        ui.text_field("pass", &mut self.pass);
    }
}

#[test]
fn password_never_leaves() {
    let mut pass = TextEdit::new().placeholder("Password");
    pass.password = true;
    pass.set("hunter2-secret");
    let mut h = Harness::new(Login { pass }, 800, 600);
    let s = h.tree();
    let id = s
        .find(Role::PasswordInput, "Password")
        .expect("the field is in the tree");
    assert!(s.node(id).unwrap().value().is_none());
    let j = json(&s);
    assert!(!j.contains("hunter2"), "{j}");
    assert!(!j.contains(&"•".repeat(14)));
    assert!(!j.contains("\\u2022"));

    let r = h.act_on(Role::PasswordInput, "Password", "set_value", Some("x"));
    assert_eq!(r.unwrap_err(), Refused::Rule(Refusal::Password));
    assert_eq!(h.app().pass.text, "hunter2-secret");

    let s = h
        .act_on(Role::PasswordInput, "Password", "focus", None)
        .unwrap();
    assert_eq!(s.update.focus.0, id);
    assert!(!json(&s).contains("hunter2"));
}

#[derive(Default)]
struct Vault {
    revealed: bool,
}

impl App for Vault {
    type Msg = ();

    fn view(&mut self, ui: &mut Ui) {
        ui.label("Before");
        ui.private("Vault", |ui| {
            ui.label("SECRET-123");
            if ui.button_at(Rect::new(10, 300, 200, 30), "Reveal", "Reveal") {
                self.revealed = true;
            }
        });
    }
}

#[test]
fn private_holds_back_values_and_drawn_text_but_not_its_buttons() {
    let mut h = Harness::new(Vault::default(), 800, 600);
    let s = h.tree();
    let j = json(&s);
    // What the area draws is the owner's data there: held back, and marked secret.
    assert!(!j.contains("SECRET-123"), "{j}");
    let vault = s
        .find(Role::GenericContainer, "Vault")
        .expect("the private node is there");
    assert!(!s.node(vault).unwrap().children().is_empty(), "its widgets are there");
    assert!(s.text().contains("private area"));
    assert!(s.text().contains("(secret)"), "{}", s.text());
    // Its buttons keep their names and take her actions.
    let reveal = s.find(Role::Button, "Reveal").expect("the button inside is there");
    h.act(reveal, "click", None).expect("she may press it");
    assert!(h.app().revealed);
}

#[derive(Default)]
struct Pairing {
    approved: u32,
    other: u32,
}

impl App for Pairing {
    type Msg = ();

    fn view(&mut self, ui: &mut Ui) {
        ui.owner_only(|ui| {
            if ui.button_at(Rect::new(10, 10, 200, 30), "approve", "Approve") {
                self.approved += 1;
            }
        });
        if ui.button_at(Rect::new(10, 60, 200, 30), "other", "Other") {
            self.other += 1;
        }
    }
}

#[test]
fn owner_only_refuses_lucina() {
    let mut h = Harness::new(Pairing::default(), 800, 600);
    let s = h.tree();
    assert!(s.text().contains("owner-only"));
    let r = h.act_on(Role::Button, "Approve", "click", None);
    assert_eq!(r.unwrap_err(), Refused::Rule(Refusal::OwnerOnly));
    assert_eq!(h.app().approved, 0);

    h.click_injected(110, 25);
    assert_eq!(
        h.app().approved,
        0,
        "injected input does not reach an owner-only button"
    );
    h.click_injected(110, 75);
    assert_eq!(h.app().other, 1, "a normal button takes injected input");

    h.click(110, 25);
    assert_eq!(h.app().approved, 1, "the owner's own click works");
}

#[derive(Default)]
struct Dialog {
    open: bool,
    behind: u32,
}

impl App for Dialog {
    type Msg = ();

    fn view(&mut self, ui: &mut Ui) {
        if ui.button("Behind") {
            self.behind += 1;
        }
        if self.open {
            ui.modal(300.0, 200.0, |ui| {
                if ui.button("Close") {
                    self.open = false;
                }
            });
        }
    }
}

#[test]
fn modal_makes_behind_inert() {
    let mut h = Harness::new(
        Dialog {
            open: true,
            behind: 0,
        },
        800,
        600,
    );
    let s = h.tree();
    assert_eq!(count(&s, Role::Dialog, None), 1);
    let r = h.act_on(Role::Button, "Behind", "click", None);
    assert_eq!(r.unwrap_err(), Refused::Rule(Refusal::Inert));
    assert_eq!(h.app().behind, 0);

    let after = h.act_on(Role::Button, "Close", "click", None).unwrap();
    assert!(!h.app().open);
    assert_eq!(count(&after, Role::Dialog, None), 0);
    h.act_on(Role::Button, "Behind", "click", None).unwrap();
    assert_eq!(h.app().behind, 1);
}

#[test]
fn stale_version_refused() {
    let mut h = Harness::new(Counter::default(), 800, 600);
    let id = h.tree().find(Role::Button, "Add").unwrap();
    let id = lucina_ax::tree::id_text(id);
    let (r, now) = h
        .core
        .act("harness", &id, "click", None, "000000000000")
        .unwrap_err();
    assert_eq!(r, Refused::Rule(Refusal::Stale));
    assert!(r.stale());
    assert_eq!(
        now.version,
        h.tree().version,
        "the refusal carries the tree now"
    );
    assert_eq!(h.app().n, 0);
}

#[derive(Default)]
struct Rows {
    list: ListState,
    activated: Vec<usize>,
}

impl App for Rows {
    type Msg = ();

    fn view(&mut self, ui: &mut Ui) {
        let mut list = std::mem::take(&mut self.list);
        let r = ui.list(
            "items",
            &mut list,
            Rect::new(0, 0, 800, 400),
            1000,
            40.0,
            |ui, i, _| {
                ui.label(&format!("Item {i}"));
            },
        );
        self.list = list;
        if let Some(i) = r.activated {
            self.activated.push(i);
        }
    }
}

#[test]
fn long_list_only_visible_rows() {
    let mut h = Harness::new(Rows::default(), 800, 600);
    let s = h.tree();
    let rows = count(&s, Role::ListBoxOption, None);
    assert!((1..=30).contains(&rows), "{rows} rows in the tree");
    assert_eq!(count(&s, Role::ListBox, None), 1);
    assert!(
        s.find(Role::ListBoxOption, "Item 3").is_some(),
        "a row is named by its label"
    );
    assert!(s.find(Role::ListBoxOption, "Item 999").is_none());

    h.act_on(Role::ListBoxOption, "Item 3", "click", None)
        .unwrap();
    assert_eq!(h.app().list.selected, Some(3));
    assert_eq!(h.app().activated, vec![3]);

    let s = h
        .act_on(Role::ListBoxOption, "Item 5", "focus", None)
        .unwrap();
    assert_eq!(h.app().list.selected, Some(5));
    assert_eq!(h.app().activated, vec![3], "focus selects without opening");
    let five = s.find(Role::ListBoxOption, "Item 5").unwrap();
    assert_eq!(s.node(five).unwrap().is_selected(), Some(true));

    let list = s
        .update
        .nodes
        .iter()
        .find(|(_, n)| n.role() == Role::ListBox)
        .unwrap()
        .0
        .0;
    let s = h.act(list, "scroll_down", None).unwrap();
    assert!(h.app().list.scroll > 0);
    assert!(
        s.find(Role::ListBoxOption, "Item 3").is_none(),
        "the rows in view moved"
    );
}

#[derive(Default)]
struct Search {
    q: TextEdit,
    notes: TextEdit,
    changed: bool,
    submitted: bool,
}

impl App for Search {
    type Msg = ();

    fn view(&mut self, ui: &mut Ui) {
        if self.q.placeholder.is_empty() {
            self.q.placeholder = "Search".into();
            self.notes.placeholder = "Notes".into();
        }
        let r = ui.text_field("q", &mut self.q);
        self.changed |= r.changed;
        self.submitted |= r.submitted;
        let r = ui.text_area("notes", &mut self.notes, 4);
        self.submitted |= r.submitted;
    }
}

#[test]
fn set_value_and_submit() {
    let mut h = Harness::new(Search::default(), 800, 600);
    let s = h
        .act_on(Role::TextInput, "Search", "set_value", Some("milk"))
        .unwrap();
    assert_eq!(h.app().q.text, "milk");
    assert!(h.app().changed);
    assert!(!h.app().submitted);
    let id = s.find(Role::TextInput, "Search").unwrap();
    assert_eq!(s.node(id).unwrap().value(), Some("milk"));
    assert_eq!(h.tree().node(id).unwrap().value(), Some("milk"));
    assert_eq!(
        s.update.focus.0, id,
        "set_value gives the field the keyboard"
    );

    h.act_on(Role::MultilineTextInput, "Notes", "click", None)
        .unwrap();
    assert!(!h.app().submitted, "a click on a text box only focuses it");
    h.act_on(Role::TextInput, "Search", "click", None).unwrap();
    assert!(h.app().submitted, "a click on a one-line field is Enter");

    let r = h.act_on(Role::TextInput, "Search", "set_value", None);
    assert!(matches!(r, Err(Refused::Bad(_))));
}

/// 6000 labels in one view, in 12 tall rows of 500 each: more nodes than a tree may hold.
struct Crowd;

impl App for Crowd {
    type Msg = ();

    fn view(&mut self, ui: &mut Ui) {
        let mut list = ListState::new();
        ui.list(
            "crowd",
            &mut list,
            Rect::new(0, 0, 800, 600),
            12,
            50.0,
            |ui, row, _| {
                for i in 0..500 {
                    ui.label(&format!("L{row}-{i}"));
                }
            },
        );
    }
}

#[test]
fn huge_view_is_cut() {
    let mut h = Harness::new(Crowd, 800, 600);
    let s = h.tree();
    assert!(s.truncated);
    assert!(
        s.len() <= lucina_ax::rules::MAX_NODES as usize,
        "{} nodes",
        s.len()
    );
    assert!(s.len() > 1000);
    let bytes = serde_json::to_vec(&s.reply(1)).unwrap().len();
    assert!(bytes < 8 * 1024 * 1024, "{bytes} bytes");
}

#[test]
fn socket_round_trip() {
    let dir = std::env::temp_dir().join(format!("lucina-ax-test-{}", std::process::id()));
    let path = dir.join("x.sock");
    let mut server = lucina_ax::Server::bind_at(&path).unwrap();
    let mut h = Harness::new(Counter::default(), 800, 600);
    let asker = {
        let path = path.clone();
        std::thread::spawn(move || {
            lucina_ax::proto::request(
                &path,
                &serde_json::json!({"op": "tree"}),
                Duration::from_secs(5),
            )
        })
    };
    let mut served = 0;
    for _ in 0..500 {
        served += server.pump(|req| match req {
            Ok(lucina_ax::Request::Tree) => h.tree().reply(std::process::id()),
            Ok(_) => lucina_ax::proto::refusal("bad_request", "only tree here", None),
            Err(why) => lucina_ax::proto::refusal("bad_request", &why, None),
        });
        if served > 0 {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(served, 1);
    let reply = asker.join().unwrap().unwrap();
    assert_eq!(reply["ok"], serde_json::json!(true));
    assert_eq!(reply["version"].as_str(), Some(h.tree().version.as_str()));
    assert!(reply["text"].as_str().unwrap().contains("\"Add\""));
    drop(server);
    let _ = std::fs::remove_dir(&dir);
}

#[derive(Default)]
struct Plain {
    on: bool,
    pressed: u32,
}

impl App for Plain {
    type Msg = ();

    fn view(&mut self, ui: &mut Ui) {
        if ui.button_at(Rect::new(10, 200, 200, 30), "press", "Press") {
            self.pressed += 1;
        }
        let mut on = self.on;
        ui.checkbox("On", &mut on);
        self.on = on;
    }
}

#[test]
fn plain_click_without_tree() {
    let mut h = Harness::new(Plain::default(), 800, 600);
    h.click(110, 215);
    assert_eq!(h.app().pressed, 1);
    assert!(h.core.frame.get(5, 5).is_some());
    // The same app read as a tree: the check box is there with its state, and Lucina flips it.
    let s = h.tree();
    let cb = s.find(Role::CheckBox, "On").unwrap();
    let (x, y) = centre(&s, cb);
    h.act(cb, "click", None).unwrap();
    assert!(h.app().on);
    h.click(x, y);
    assert!(!h.app().on);
    h.click(110, 215);
    assert_eq!(h.app().pressed, 2);
}
