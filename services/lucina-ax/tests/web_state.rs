//! A page's tree through `Snapshot::from_update_with` (LucinaNet, stage F), and an app's own published state (`Builder::state`).

use lucina_ax::accesskit::{Action, Node, NodeId, Role, TreeId, TreeInfo, TreeUpdate};
use lucina_ax::tree::{Builder, id_text};
use lucina_ax::{Snapshot, Source};

fn page(links: usize) -> TreeUpdate {
    let mut root = Node::new(Role::Window);
    let mut nodes = Vec::new();
    let mut kids = Vec::new();
    let mut search = Node::new(Role::SearchInput);
    search.set_placeholder("Search the site");
    search.add_action(Action::Focus);
    search.add_action(Action::SetValue);
    search.set_required();
    nodes.push((NodeId(2), search));
    kids.push(NodeId(2));
    let mut pw = Node::new(Role::PasswordInput);
    pw.set_value("pw-SECRET");
    nodes.push((NodeId(3), pw));
    kids.push(NodeId(3));
    // A one-time code Servo marks secret, sent with a value by mistake: held back all the same.
    let mut otp = Node::new(Role::TextInput);
    otp.set_class_name("secret");
    otp.set_value("otp-123456");
    otp.add_action(Action::SetValue);
    nodes.push((NodeId(4), otp));
    kids.push(NodeId(4));
    for i in 0..links {
        let mut a = Node::new(Role::Link);
        a.set_label(format!("Link {i}"));
        a.set_url(format!("https://example.org/{i}"));
        a.add_action(Action::Click);
        a.add_action(Action::ScrollIntoView);
        let id = NodeId(100 + i as u64);
        nodes.push((id, a));
        kids.push(id);
    }
    root.set_children(kids);
    nodes.insert(0, (NodeId(1), root));
    TreeUpdate { nodes, tree: Some(TreeInfo { root: NodeId(1), toolkit_name: None, toolkit_version: None }), tree_id: TreeId::ROOT, focus: NodeId(1) }
}

fn web() -> Source<'static> {
    Source { toolkit: "servo", provenance: Some("web https://example.org"), web: true }
}

#[test]
fn a_page_keeps_its_links_hints_and_provenance_and_no_password() {
    let snap = Snapshot::from_update_with("lucinanet", &page(3), &web());
    let json = serde_json::to_string(&snap.reply(1)).unwrap();
    assert!(!json.contains("pw-SECRET"), "{json}");
    assert!(!json.contains("otp-123456"), "{json}");
    assert!(snap.extras[&id_text(4)].secret);
    assert!(snap.plan(&id_text(4), "set_value", Some("1"), &snap.version).is_err());
    let text = snap.text();
    assert!(text.contains("hint \"Search the site\""), "{text}");
    assert!(text.contains("required"), "{text}");
    assert!(text.contains("-> https://example.org/1"), "{text}");
    assert_eq!(snap.extras[&id_text(100)].provenance.as_deref(), Some("web https://example.org"));
    assert!(snap.extras[&id_text(3)].secret);
    assert_eq!(snap.update.tree.as_ref().unwrap().toolkit_name.as_deref(), Some("servo"));
    assert!(snap.plan(&id_text(101), "scroll_into_view", None, &snap.version).is_ok());
    assert!(snap.plan(&id_text(2), "blur", None, &snap.version).is_err(), "the field does not list blur");
}

#[test]
fn a_page_may_be_bigger_than_an_app_but_not_unbounded() {
    // 300 children per node fit under 512; spread 9000 links over 30 groups.
    let mut root = Node::new(Role::Window);
    let mut nodes = Vec::new();
    let mut groups = Vec::new();
    for g in 0..30u64 {
        let gid = NodeId(10_000 + g);
        let mut group = Node::new(Role::Group);
        let mut kids = Vec::new();
        for i in 0..300u64 {
            let id = NodeId(100_000 + g * 1000 + i);
            let mut a = Node::new(Role::Link);
            a.set_label("x");
            nodes.push((id, a));
            kids.push(id);
        }
        group.set_children(kids);
        nodes.push((gid, group));
        groups.push(gid);
    }
    root.set_children(groups);
    nodes.insert(0, (NodeId(1), root));
    let u = TreeUpdate { nodes, tree: Some(TreeInfo { root: NodeId(1), toolkit_name: None, toolkit_version: None }), tree_id: TreeId::ROOT, focus: NodeId(1) };
    let page = Snapshot::from_update_with("lucinanet", &u, &web());
    assert!(page.len() > 4096 && page.len() <= 8192, "{}", page.len());
    assert!(page.truncated);
    let app = Snapshot::from_update("egui", &u);
    assert!(app.len() <= 4096 && app.truncated);
    assert!(serde_json::to_vec(&page.reply(1)).unwrap().len() < 8 * 1024 * 1024);
}

#[test]
fn an_app_publishes_its_own_state() {
    let mut b = Builder::new("gameboy", 800, 600);
    let p = b.state("map_id", "0x26 Pallet Town");
    let snap = b.finish();
    assert!(snap.extras[&id_text(p.id)].state);
    let text = snap.text();
    assert!(text.contains("\"map_id\" = \"0x26 Pallet Town\""), "{text}");
    assert!(text.contains("(app state)"), "{text}");
}
