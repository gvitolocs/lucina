//! The shell's own screen as an AccessKit tree, for Lucina (stage B of docs/research/juno-ai-native.md).
//!
//! The shell is not built on lucina-app's `Ui`: it draws itself. So the tree is made from the `App`'s state in one
//! function, [`build`], and an action is carried out by calling the same method a click calls, in [`answer`]. The
//! socket is `/run/lucina/ax/shell.sock`.

use lucina_app::gfx::Rect;
use lucina_ax::accesskit::{Action, Role};
use lucina_ax::rules::{Act, Kind};
use lucina_ax::tree::{self, Builder, Widget};
use lucina_ax::{Request, Snapshot};

use crate::App;
use crate::ui::{self, View};

pub const SOCKET: &str = "/run/lucina/ax/shell.sock";

/// What a node does when Lucina clicks it: the same thing the pointer does.
#[derive(Clone, Copy, Debug)]
#[allow(clippy::enum_variant_names)]
pub enum Do {
    Open(View),
    /// The tile or power button `i` of Home's focus order.
    Activate(usize),
    Reopen,
    Section(usize),
    ConfirmCancel,
    ConfirmDo,
    /// Item `i` of the home dashboard.
    Dash(usize),
    /// The second page of Home: the apps.
    AppsPage,
}

fn bounds(r: Rect) -> (i32, i32, i32, i32) {
    (r.x, r.y, r.w, r.h)
}

/// The status bar's right side as one line: the camera mark first, then clock, battery, network.
fn status_line(app: &App) -> String {
    let mut parts: Vec<String> = Vec::new();
    if let Some(program) = &app.status.camera {
        parts.push(format!("camera on ({program})"));
    }
    if !app.status.clock.is_empty() {
        parts.push(app.status.clock.clone());
    }
    let battery = app.status.battery.as_ref().map(|b| b.text());
    if let Some(b) = &battery {
        parts.push(b.clone());
    }
    if !app.status.network.is_empty() {
        parts.push(app.status.network.clone());
    }
    parts.join(" ")
}

/// The screen as a tree, and what each node with actions does. Read-only: nothing of the `App` changes.
pub fn build(app: &App) -> (Snapshot, Vec<(u64, Do)>) {
    let l = &app.layout;
    let mut b = Builder::new("shell", l.w, l.h);
    let mut acts: Vec<(u64, Do)> = Vec::new();
    // Behind the confirmation dialog every other node is inert.
    let inert = app.confirm.is_some();

    // The bar: Home, and what the status side shows.
    let mut home = Widget::new(tree::key("bar:home"), Role::Button);
    home.name = "Home";
    home.bounds = bounds(l.bar_home);
    home.actions = &[Action::Click];
    home.inert = inert;
    let p = b.leaf(&home);
    if p.recorded {
        acts.push((p.id, Do::Open(View::Home)));
    }
    let status = status_line(app);
    let mut label = Widget::new(tree::key("bar:status"), Role::Label);
    label.name = &status;
    label.bounds = bounds(Rect::new(
        l.bar_home.right(),
        l.bar.y,
        l.w - l.bar_home.right(),
        l.bar.h,
    ));
    label.inert = inert;
    b.leaf(&label);
    // The camera mark, when something captures: same place as the status label, its own node.
    if let Some(program) = &app.status.camera {
        let name = format!("Camera on: {program}");
        let mut w = Widget::new(tree::key("bar:camera"), Role::Label);
        w.name = &name;
        w.bounds = bounds(Rect::new(
            l.bar_home.right(),
            l.bar.y,
            l.w - l.bar_home.right(),
            l.bar.h,
        ));
        w.inert = inert;
        b.leaf(&w);
    }

    if let Some(r) = &app.running {
        // An app has the screen: the shell draws nothing of its own.
        let line = format!("{} is open", r.manifest.title);
        let mut w = Widget::new(tree::key("running:app"), Role::Label);
        w.name = &line;
        w.bounds = bounds(l.window);
        w.inert = inert;
        b.leaf(&w);
    } else {
        match app.view {
            View::Home if app.dash.page == crate::dash::Page::Dash => {
                let items = app.dash.ax_items();
                let mut group = Widget::new(tree::key("home:dashboard"), Role::Group);
                group.name = "Home";
                group.bounds = bounds(l.window);
                group.inert = inert;
                b.open(&group);
                for (i, (name, r)) in items.iter().enumerate() {
                    let mut w = Widget::new(tree::key(&format!("dash:{i}:{name}")), Role::Button);
                    w.name = name;
                    w.bounds = bounds(*r);
                    w.actions = &[Action::Click];
                    w.inert = inert;
                    let p = b.leaf(&w);
                    if p.recorded {
                        acts.push((p.id, Do::Dash(i)));
                    }
                }
                let mut w = Widget::new(tree::key("home:apps-page"), Role::Button);
                w.name = "Apps";
                w.bounds = bounds(l.window);
                w.actions = &[Action::Click];
                w.inert = inert;
                let p = b.leaf(&w);
                if p.recorded {
                    acts.push((p.id, Do::AppsPage));
                }
                b.close();
            }
            View::Home => {
                let area = l.apps_area();
                let mut group = Widget::new(tree::key("home:tiles"), Role::Group);
                group.name = "Apps";
                group.bounds = bounds(area);
                group.inert = inert;
                b.open(&group);
                for (&g, r) in app.home.header_groups.iter().zip(&app.home.headers) {
                    if r.intersect(&area).is_empty() {
                        continue;
                    }
                    let name = crate::app_groups::Group::ALL[g].name();
                    let mut w = Widget::new(tree::key(&format!("apps:group:{g}")), Role::Label);
                    w.name = name;
                    w.bounds = bounds(*r);
                    w.inert = inert;
                    b.leaf(&w);
                }
                for (i, tile) in app.home.tiles.iter().enumerate() {
                    let Some(r) = app.home.placed.get(i).copied() else {
                        continue;
                    };
                    if r.intersect(&area).is_empty() {
                        continue;
                    }
                    let mut w = Widget::new(tree::key(&format!("tile:{}", tile.id)), Role::Button);
                    w.name = &tile.title;
                    w.bounds = bounds(r);
                    w.actions = &[Action::Click, Action::Focus];
                    w.focused = app.home.sel == i;
                    w.inert = inert;
                    let p = b.leaf(&w);
                    if p.recorded {
                        acts.push((p.id, Do::Activate(i)));
                    }
                }
                b.close();
                // The power buttons: only the owner's own hands press them.
                b.begin_owner_only();
                for (k, p) in ui::POWER_BUTTONS.iter().enumerate() {
                    let key = match p {
                        ui::Power::Restart => "power:restart",
                        ui::Power::ShutDown => "power:off",
                    };
                    let mut w = Widget::new(tree::key(key), Role::Button);
                    w.name = p.name();
                    w.bounds = bounds(l.power[k]);
                    w.actions = &[Action::Click];
                    let placed = b.leaf(&w);
                    if placed.recorded {
                        acts.push((placed.id, Do::Activate(app.home.tiles.len() + k)));
                    }
                }
                b.end_owner_only();
                if let Some(note) = &app.home.note {
                    let mut w = Widget::new(tree::key("home:note"), Role::Label);
                    w.name = note;
                    w.bounds = (l.margin, l.h - l.px(50.0), l.w - 2 * l.margin, l.px(20.0));
                    w.inert = inert;
                    b.leaf(&w);
                }
            }
            View::Ended => {
                if let Some(e) = &app.ended {
                    let line = format!("{} stopped: {}", e.title, e.what);
                    let mut w = Widget::new(tree::key("ended:what"), Role::Label);
                    w.name = &line;
                    w.bounds = bounds(l.window);
                    w.inert = inert;
                    b.leaf(&w);
                    let (reopen, home) = ui::ended_buttons(l);
                    let mut w = Widget::new(tree::key("ended:reopen"), Role::Button);
                    w.name = "Open again";
                    w.bounds = bounds(reopen);
                    w.actions = &[Action::Click];
                    w.inert = inert;
                    let p = b.leaf(&w);
                    if p.recorded {
                        acts.push((p.id, Do::Reopen));
                    }
                    let mut w = Widget::new(tree::key("ended:home"), Role::Button);
                    w.name = "Home";
                    w.bounds = bounds(home);
                    w.actions = &[Action::Click];
                    w.inert = inert;
                    let p = b.leaf(&w);
                    if p.recorded {
                        acts.push((p.id, Do::Open(View::Home)));
                    }
                }
            }
            View::Settings => {
                // The page's text is not recorded: it may hold addresses or keys.
                let mut list = Widget::new(tree::key("settings:list"), Role::ListBox);
                list.name = "Settings";
                list.bounds = bounds(l.list);
                list.inert = inert;
                b.open(&list);
                for (i, (name, _)) in app.settings.sections.iter().enumerate() {
                    let mut w =
                        Widget::new(tree::key(&format!("settings:{name}")), Role::ListBoxOption);
                    w.name = name;
                    w.bounds = (
                        l.list.x,
                        l.list.y + i as i32 * l.list_row,
                        l.list.w,
                        l.list_row,
                    );
                    w.actions = &[Action::Click];
                    w.selected = Some(app.settings_sel == i);
                    w.inert = inert;
                    let p = b.leaf(&w);
                    if p.recorded {
                        acts.push((p.id, Do::Section(i)));
                    }
                }
                b.close();
            }
            View::Terminal => {
                // What is typed and shown there never leaves the shell.
                let mut w = Widget::new(tree::key("term:screen"), Role::Terminal);
                w.kind = Kind::Private;
                w.name = "Terminal";
                w.bounds = bounds(l.window);
                b.leaf(&w);
            }
        }
    }

    if let Some((action, _)) = app.confirm {
        let question = format!("{}?", action.name());
        let mut d = Widget::new(tree::key("confirm:dialog"), Role::Dialog);
        d.name = &question;
        d.bounds = bounds(l.dialog);
        b.open(&d);
        b.begin_owner_only();
        let mut cancel = Widget::new(tree::key("confirm:cancel"), Role::Button);
        cancel.name = "Cancel";
        cancel.bounds = bounds(l.dialog_buttons[ui::CONFIRM_CANCEL]);
        cancel.actions = &[Action::Click];
        let p = b.leaf(&cancel);
        if p.recorded {
            acts.push((p.id, Do::ConfirmCancel));
        }
        let mut ok = Widget::new(tree::key("confirm:do"), Role::Button);
        ok.name = action.name();
        ok.bounds = bounds(l.dialog_buttons[ui::CONFIRM_DO]);
        ok.actions = &[Action::Click];
        let p = b.leaf(&ok);
        if p.recorded {
            acts.push((p.id, Do::ConfirmDo));
        }
        b.end_owner_only();
        b.close();
    }

    (b.finish(), acts)
}

/// The app on screen, for the reply's "front".
fn front(app: &App) -> serde_json::Value {
    match &app.running {
        Some(r) => serde_json::json!({ "app": r.manifest.id, "pid": r.pid() }),
        None => serde_json::Value::Null,
    }
}

/// Carry out what a node does, with the same methods a click calls.
fn run(app: &mut App, d: Do, act: Act) {
    match (act, d) {
        (Act::Click, Do::Open(v)) => app.open(v),
        (Act::Click, Do::Activate(i)) => app.activate(i),
        (Act::Click, Do::Reopen) => app.reopen(),
        (Act::Click, Do::Section(i)) => app.select_section(i),
        (Act::Click, Do::ConfirmCancel) => {
            app.confirm = None;
            app.damage.push(app.layout.dialog);
        }
        (Act::Click, Do::ConfirmDo) => {
            if let Some((action, _)) = app.confirm.take() {
                app.damage.push(app.layout.dialog);
                app.leave(action);
            }
        }
        (Act::Focus, Do::Activate(i)) if i < app.home.tiles.len() => app.focus_home(i),
        (Act::Click, Do::Dash(i)) => app.dash_activate(i),
        (Act::Click, Do::AppsPage) => app.go_page(crate::dash::Page::Apps),
        _ => {}
    }
}

/// One request from Lucina, answered on the shell's own thread.
pub fn answer(app: &mut App, req: Result<Request, String>) -> serde_json::Value {
    match req {
        Err(why) => lucina_ax::proto::refusal("bad_request", &why, None),
        // The shell's own intents (shell.open, shell.close, shell.apps) are lucina-devd's, through the control socket.
        Ok(Request::Intent { .. }) => lucina_ax::proto::refusal(
            "not_supported",
            "the shell takes no intents here: lucina-devd runs shell.* itself",
            None,
        ),
        Ok(Request::Tree) => {
            let (snap, _) = build(app);
            let mut v = snap.reply(std::process::id());
            v["front"] = front(app);
            v
        }
        Ok(Request::Act {
            id,
            action,
            value,
            version,
            ..
        }) => {
            let (snap, acts) = build(app);
            match snap.plan(&id, &action, value.as_deref(), &version) {
                Err(r) => lucina_ax::proto::refusal(
                    r.code(),
                    &r.why(),
                    Some(snap.reply(std::process::id())),
                ),
                Ok(p) => {
                    let name = snap
                        .facts
                        .get(&p.pending.node)
                        .map(|f| f.name.clone())
                        .unwrap_or_default();
                    if let Some((_, d)) = acts.iter().find(|(n, _)| *n == p.pending.node) {
                        run(app, *d, p.pending.act);
                    }
                    app.log(&format!("control: ax {action} {name}"));
                    let (snap, _) = build(app);
                    let mut v = snap.reply(std::process::id());
                    v["done"] = serde_json::json!(action);
                    v
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lucina_app::manifest::Manifest;
    use lucina_ax::Refused;
    use lucina_ax::rules::Refusal;

    fn test_apps() -> Vec<Manifest> {
        ["chat|Lucina", "tasks|Tasks"]
            .iter()
            .map(|s| {
                let (id, title) = s.split_once('|').unwrap();
                Manifest::parse(&format!("id = {id}\ntitle = {title}\nicon = {id}\n")).unwrap()
            })
            .collect()
    }

    fn act(id: u64, action: &str, version: &str) -> Result<Request, String> {
        Ok(Request::Act {
            id: tree::id_text(id),
            action: action.to_owned(),
            value: None,
            version: version.to_owned(),
            source: None,
        })
    }

    #[test]
    fn the_dashboard_tree_has_its_items_and_the_apps_page() {
        let mut app = App::for_test(1024, 768, test_apps());
        app.compose_dash();
        let (snap, acts) = build(&app);
        // "Open" (the apps page) is always there; the Lucina segment because chat is installed; no tiles on this page.
        for name in ["Open", "Lucina", "Apps"] {
            let id = snap.find(Role::Button, name).expect(name);
            assert!(acts.iter().any(|(n, _)| *n == id), "{name}");
        }
        assert!(snap.find(Role::Button, "Settings").is_none());
        let id = snap.find(Role::Button, "Apps").unwrap();
        let reply = answer(&mut app, act(id, "click", &snap.version));
        assert_eq!(reply["ok"], serde_json::json!(true));
        assert_eq!(app.dash.page, crate::dash::Page::Apps);
    }

    #[test]
    fn home_tree_has_tiles_and_owner_only_power() {
        let mut app = App::for_test(1024, 768, test_apps());
        app.dash.page = crate::dash::Page::Apps;
        let (snap, acts) = build(&app);
        for t in &app.home.tiles {
            let id = snap.find(Role::Button, &t.title).expect(&t.title);
            assert!(acts.iter().any(|(n, _)| *n == id), "{}", t.title);
        }
        for name in ["Restart", "Shut down"] {
            let id = snap.find(Role::Button, name).unwrap();
            let extra = snap.extras.get(&tree::id_text(id)).unwrap();
            assert!(extra.owner_only, "{name}");
            let r = snap.plan(&tree::id_text(id), "click", None, &snap.version);
            assert_eq!(r, Err(Refused::Rule(Refusal::OwnerOnly)), "{name}");
        }
    }

    #[test]
    fn act_click_on_the_settings_tile_opens_settings() {
        let mut app = App::for_test(1024, 768, test_apps());
        app.dash.page = crate::dash::Page::Apps;
        let (snap, _) = build(&app);
        let id = snap.find(Role::Button, "Settings").unwrap();
        let reply = answer(&mut app, act(id, "click", &snap.version));
        assert_eq!(reply["ok"], serde_json::json!(true));
        assert_eq!(reply["done"], serde_json::json!("click"));
        assert_eq!(app.view, View::Settings);
        let (snap, _) = build(&app);
        assert!(snap.find(Role::ListBox, "Settings").is_some());
    }

    #[test]
    fn terminal_view_is_one_private_node() {
        let mut app = App::for_test(1024, 768, test_apps());
        app.dash.page = crate::dash::Page::Apps;
        app.view = View::Terminal;
        let (snap, acts) = build(&app);
        let privates: Vec<&String> = snap
            .extras
            .iter()
            .filter(|(_, e)| e.private)
            .map(|(k, _)| k)
            .collect();
        assert_eq!(privates.len(), 1);
        let id = tree::parse_id(privates[0]).unwrap();
        let node = snap.node(id).unwrap();
        assert_eq!(node.role(), Role::Terminal);
        assert!(node.children().is_empty());
        assert!(node.value().is_none());
        assert!(acts.iter().all(|(n, _)| *n != id));
        // Root, the bar's two nodes, the private terminal: no terminal text anywhere.
        assert_eq!(snap.len(), 4);
    }

    #[test]
    fn ids_are_stable_and_a_stale_version_is_refused() {
        let mut app = App::for_test(1024, 768, test_apps());
        app.dash.page = crate::dash::Page::Apps;
        let (s1, a1) = build(&app);
        let (s2, a2) = build(&app);
        assert_eq!(s1.version, s2.version);
        assert_eq!(a1.len(), a2.len());
        for ((i1, _), (i2, _)) in a1.iter().zip(&a2) {
            assert_eq!(i1, i2);
        }
        let r = s1.plan(&tree::id_text(a1[0].0), "click", None, "000000000000");
        assert!(matches!(r, Err(Refused::Rule(Refusal::Stale))));
        assert!(r.unwrap_err().stale());
    }

    #[test]
    fn a_camera_on_status_leads_the_status_line() {
        let mut app = App::for_test(1024, 768, test_apps());
        app.dash.page = crate::dash::Page::Apps;
        app.status.camera = Some("lucina-app-came".into());
        assert!(status_line(&app).starts_with("camera on (lucina-app-came)"));
        // The tree carries the mark as its own label, next to the status line's.
        let (snap, _) = build(&app);
        assert!(
            snap.find(Role::Label, "Camera on: lucina-app-came")
                .is_some()
        );
        // Without a capturing program there is no such node.
        app.status.camera = None;
        let (snap, _) = build(&app);
        assert!(
            snap.find(Role::Label, "Camera on: lucina-app-came")
                .is_none()
        );
    }

    #[test]
    fn an_open_dialog_makes_the_tiles_inert() {
        let mut app = App::for_test(1024, 768, test_apps());
        app.dash.page = crate::dash::Page::Apps;
        app.confirm = Some((ui::Power::ShutDown, ui::CONFIRM_CANCEL));
        let (snap, _) = build(&app);
        let id = snap.find(Role::Button, "Settings").unwrap();
        let r = snap.plan(&tree::id_text(id), "click", None, &snap.version);
        assert_eq!(r, Err(Refused::Rule(Refusal::Inert)));
        let cancel = snap.find(Role::Button, "Cancel").unwrap();
        let r = snap.plan(&tree::id_text(cancel), "click", None, &snap.version);
        assert_eq!(r, Err(Refused::Rule(Refusal::OwnerOnly)));
    }
}
