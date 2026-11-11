//! The app on a small vault in a temporary folder, through the same jobs and messages as on the device.

use std::path::Path;
use std::sync::mpsc;

use lucina_app::Harness;
use lucina_app::keys::{self, Mods};

use crate::app::{Dialog, Middle, Tab, VaultApp};
use crate::doc;
use crate::graphview;
use crate::worker::{Job, Worker};

struct Rig {
    h: Harness<VaultApp>,
    w: Worker,
    rx: mpsc::Receiver<Job>,
    dir: tempfile::TempDir,
    _state: tempfile::TempDir,
}

fn write(root: &Path, rel: &str, text: &str) {
    let p = root.join(rel);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, text).unwrap();
}

impl Rig {
    fn new() -> Rig {
        let dir = tempfile::tempdir().unwrap();
        let r = dir.path();
        write(
            r,
            ".obsidian/app.json",
            "{\"newFileLocation\":\"folder\",\"newFileFolderPath\":\"Inbox\"}",
        );
        write(
            r,
            "00 Home.md",
            "---\ntags: [hub]\naliases: [Start]\n---\n# Home title\n\nSee [[Alpha]] and [[Alpha#Two|second]] and [[Missing]] #home.\n\n- [ ] a task [[Beta]]\n- [x] done\n\n> [!note] Callout\n> body\n\n| a | b |\n|---|---|\n| 1 | [[Beta\\|two]] |\n\n```rust\nfn main() {}\n```\n\n![[pic.png]]\n\n![remote](https://example.com/t.png)\n",
        );
        write(r, "Alpha.md", "# One\n\nBack to [[00 Home]].\n\n# Two\nx\n");
        write(
            r,
            "Folder/Beta.md",
            "Beta text with needle. [[Alpha]] #deep/tag\n",
        );
        write(r, "Folder/Sub/Gamma.md", "Gamma.\n");
        write(r, "Folder/report.pdf", "PDF");
        std::fs::write(r.join("pic.png"), [0x89, b'P', b'N', b'G']).unwrap();
        let state = tempfile::tempdir().unwrap();
        let (tx, rx) = mpsc::channel();
        let app = VaultApp::new(r.to_path_buf(), tx);
        let h = Harness::new(app, 1920, 1080);
        let w = Worker::new(state.path().to_path_buf());
        let mut rig = Rig {
            h,
            w,
            rx,
            dir,
            _state: state,
        };
        rig.pump();
        rig
    }

    fn pump(&mut self) {
        for _ in 0..4 {
            self.h.core.input(lucina_app::Input::Wheel(0));
            self.h.core.paint();
            while let Ok(job) = self.rx.try_recv() {
                for m in self.w.handle(job) {
                    self.h.core.message(m);
                }
            }
        }
        self.h.core.input(lucina_app::Input::Wheel(0));
        self.h.core.paint();
    }

    fn ctrl(&mut self, code: u16) {
        self.h.press(
            code,
            Mods {
                ctrl: true,
                ..Mods::default()
            },
        );
        self.pump();
    }
}

#[test]
fn it_opens_the_vault_and_shows_home_with_its_links() {
    let mut r = Rig::new();
    let a = r.h.app();
    assert_eq!(a.counts.as_ref().unwrap().notes, 4);
    assert!(!a.writable, "a vault opens read-only");
    let n = a.note.as_ref().unwrap();
    assert_eq!(n.path, "00 Home.md");
    assert!(n.backlinks.iter().any(|b| b.from == "Alpha.md"));
    assert_eq!(n.aliases, vec!["Start"]);
    assert!(
        a.rows
            .iter()
            .any(|x| matches!(&x.kind, doc::Kind::Table { .. }))
    );
    assert!(a.rows.iter().any(|x| x.marker.as_deref() == Some("[ ]")));
    assert!(a.rows.iter().any(|x| x.callout));
    // A remote picture stays a line of text: nothing here goes to the network.
    assert!(
        a.rows
            .iter()
            .any(|x| matches!(&x.kind, doc::Kind::Embed { external: true, .. }))
    );
    // The tree: folders first, closed; files after.
    assert_eq!(a.tree[0].name, "Folder");
    assert!(a.tree[0].folder);
    assert!(a.tree.iter().any(|x| x.name == "Alpha"));
    assert!(!a.tree.iter().any(|x| x.name.contains("Beta")));
}

#[test]
fn keyboard_walks_the_tree_and_opens_a_folder() {
    let mut r = Rig::new();
    r.h.press(keys::KEY_F2, Mods::default());
    r.pump();
    r.h.key(keys::KEY_ENTER);
    r.pump();
    assert!(
        r.h.app().tree.iter().any(|x| x.path == "Folder/Beta.md"),
        "Enter opens the folder"
    );
    r.h.key(keys::KEY_DOWN);
    r.pump();
    r.h.key(keys::KEY_DOWN);
    r.pump();
    let path = r.h.app().note.as_ref().unwrap().path.clone();
    assert_eq!(path, "Folder/Beta.md", "the note follows the selection");
}

#[test]
fn search_finds_text_and_names_and_tags_lead_to_notes() {
    let mut r = Rig::new();
    r.h.key(keys::KEY_F3);
    r.pump();
    assert_eq!(r.h.app().tab, Tab::Search);
    r.h.type_str("needle");
    r.pump();
    assert_eq!(r.h.app().hits.len(), 1);
    assert_eq!(r.h.app().hits[0].path, "Folder/Beta.md");
    r.h.key(crate::app::k::F4);
    r.pump();
    assert!(
        r.h.app().tags.iter().any(|t| t.tag == "deep"),
        "parents count as in Obsidian"
    );
    let i =
        r.h.app()
            .tags
            .iter()
            .position(|t| t.tag == "deep/tag")
            .unwrap();
    for _ in 0..=i {
        r.h.key(keys::KEY_DOWN);
    }
    r.h.key(keys::KEY_ENTER);
    r.pump();
    assert_eq!(r.h.app().tab, Tab::Search);
    assert_eq!(r.h.app().query.text, "tag:deep/tag");
    assert_eq!(r.h.app().hits[0].path, "Folder/Beta.md");
}

#[test]
fn a_clicked_wikilink_opens_its_note() {
    let mut r = Rig::new();
    // Follow through the same job a click sends, then the side panel's list by keyboard.
    r.h.app();
    let job = Job::Follow {
        from: "00 Home.md".into(),
        link: "Alpha#Two".into(),
    };
    for m in r.w.handle(job) {
        r.h.core.message(m);
    }
    r.pump();
    assert_eq!(r.h.app().note.as_ref().unwrap().path, "Alpha.md");
    let job = Job::Follow {
        from: "Alpha.md".into(),
        link: "Nothing".into(),
    };
    for m in r.w.handle(job) {
        r.h.core.message(m);
    }
    r.pump();
    assert!(r.h.app().said.contains("Nothing"));
    assert_eq!(
        r.h.app().note.as_ref().unwrap().path,
        "Alpha.md",
        "still there"
    );
    r.h.press(
        keys::KEY_LEFT,
        Mods {
            alt: true,
            ..Mods::default()
        },
    );
    r.pump();
    assert_eq!(
        r.h.app().note.as_ref().unwrap().path,
        "00 Home.md",
        "Alt+Left comes back"
    );
}

#[test]
fn editing_asks_first_then_saves_and_real_files_change() {
    let mut r = Rig::new();
    r.ctrl(crate::app::k::E);
    assert!(
        matches!(r.h.app().dialog, Some(Dialog::AllowEditing)),
        "read-only asks"
    );
    assert_eq!(r.h.app().middle, Middle::Read);
    r.h.key(keys::KEY_ENTER);
    r.pump();
    assert!(r.h.app().writable);
    r.ctrl(crate::app::k::E);
    assert_eq!(r.h.app().middle, Middle::Edit);
    r.h.type_str("Hello. ");
    r.pump();
    r.ctrl(keys::KEY_S);
    assert_eq!(r.h.app().middle, Middle::Read);
    let on_disk = std::fs::read_to_string(r.dir.path().join("00 Home.md")).unwrap();
    assert!(on_disk.contains("Hello. "), "{on_disk}");
    assert!(r.h.app().note.as_ref().unwrap().text.contains("Hello. "));
}

#[test]
fn a_change_on_disk_while_editing_keeps_both() {
    let mut r = Rig::new();
    r.ctrl(crate::app::k::E);
    r.h.key(keys::KEY_ENTER);
    r.pump();
    r.ctrl(crate::app::k::E);
    r.h.type_str("mine ");
    std::fs::write(r.dir.path().join("00 Home.md"), "theirs\n").unwrap();
    r.ctrl(keys::KEY_S);
    assert!(matches!(r.h.app().dialog, Some(Dialog::Conflict { .. })));
    assert_eq!(
        std::fs::read_to_string(r.dir.path().join("00 Home.md")).unwrap(),
        "theirs\n"
    );
    r.h.key(keys::KEY_ENTER);
    r.pump();
    let copy = r.h.app().note.as_ref().unwrap().path.clone();
    assert!(copy.starts_with("00 Home (conflict "), "{copy}");
    assert!(
        std::fs::read_to_string(r.dir.path().join(&copy))
            .unwrap()
            .contains("mine ")
    );
    assert_eq!(
        std::fs::read_to_string(r.dir.path().join("00 Home.md")).unwrap(),
        "theirs\n"
    );
}

#[test]
fn the_graph_draws_and_finds_a_node() {
    let mut r = Rig::new();
    r.h.key(crate::app::k::F5);
    r.pump();
    assert_eq!(r.h.app().middle, Middle::Graph);
    r.pump();
    let a = r.h.app();
    let g = a.graph.as_ref().unwrap();
    assert_eq!(g.data.nodes.len(), 3);
    assert!(
        g.picture.iter().any(|p| *p == 0x7f8fe0),
        "the open note is gold"
    );
    let v = g.view.unwrap();
    let n = &g.data.nodes[0];
    let p = v.project(n.x, n.y, 400, 300);
    assert_eq!(graphview::nearest(&g.data, v, 400, 300, p.0, p.1), Some(0));
    r.h.key(keys::KEY_EQUAL);
    r.pump();
    assert!(r.h.app().graph.as_ref().unwrap().view.unwrap().zoom > v.zoom);
    r.h.key(crate::app::k::F5);
    r.pump();
    assert_eq!(r.h.app().middle, Middle::Read);
}

#[test]
fn a_missing_vault_says_where_it_looked() {
    let (tx, rx) = mpsc::channel();
    let state = tempfile::tempdir().unwrap();
    let mut w = Worker::new(state.path().to_path_buf());
    let mut h = Harness::new(VaultApp::new("/nowhere/vault".into(), tx), 800, 600);
    while let Ok(j) = rx.try_recv() {
        for m in w.handle(j) {
            h.core.message(m);
        }
    }
    h.core.paint();
    assert!(!h.ppm().is_empty());
}

#[test]
fn wrapping_keeps_runs_and_every_word() {
    use lucina_vault::markdown::Span;
    let spans = vec![
        Span {
            text: "one two three ".into(),
            ..Default::default()
        },
        Span {
            text: "link words".into(),
            link: Some("x".into()),
            ..Default::default()
        },
        Span {
            text: "\nnext".into(),
            ..Default::default()
        },
    ];
    let lines = doc::wrap(&spans, 0, 50, |_, s| s.chars().count() as i32 * 10);
    let all: Vec<String> = lines
        .iter()
        .map(|l| l.iter().map(|p| p.text.as_str()).collect())
        .collect();
    assert_eq!(all.join("|"), "one|two|three|link|words|next");
    assert_eq!(lines[3][0].span, 1);
    // A word wider than the line is cut, not lost.
    let long = vec![Span {
        text: "abcdefghijkl".into(),
        ..Default::default()
    }];
    let l = doc::wrap(&long, 0, 50, |_, s| s.chars().count() as i32 * 10);
    assert_eq!(l.len(), 3);
    assert_eq!(
        l.iter()
            .flatten()
            .map(|p| p.text.as_str())
            .collect::<String>(),
        "abcdefghijkl"
    );
}
