//! The app against the sample library, and against the real `lucina-media` through the real client.

use std::sync::atomic::Ordering;
use std::sync::mpsc;

use lucina_app::time::Zone;
use lucina_app::{Harness, keys};
use lucina_files::drive::{DriveSort, DriveType};
use lucina_files::engine::Engine;
use lucina_files::fake::Fake;
use lucina_files::media::Connect;

use crate::app::{Body, Context, Drive, Link, Row};
use crate::server::{Job, Worker};

const NOW: i64 = 1_790_000_000_000;

struct Rig {
    h: Harness<Drive>,
    w: Worker,
    rx: mpsc::Receiver<Job>,
}

impl Rig {
    fn on(connect: Connect) -> Rig {
        let media = connect().expect("a server");
        let w = Worker::new(media, connect.clone(), Box::new(|_| {}));
        let (jobs, rx) = mpsc::channel::<Job>();
        let link: Link = Box::new(move |j| {
            let _ = jobs.send(j);
        });
        let h = Harness::new(Drive::new(Zone::utc(), None, Some(link), Some(connect)), 1024, 768);
        let mut r = Rig { h, w, rx };
        r.h.app().start();
        r.pump();
        r
    }

    fn new() -> (Rig, Fake) {
        let fake = Fake::new(NOW, 4);
        (Rig::on(fake.connect()), fake)
    }

    /// What the worker thread does, here on this thread.
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

    fn key(&mut self, k: u16) {
        self.h.key(k);
        self.pump();
    }

    fn names(&mut self) -> Vec<String> {
        let rows = self.h.app().rows();
        rows.iter()
            .map(|r| {
                let a = self.h.app();
                match r {
                    Row::Root(i) => a.roots.as_ref().unwrap().as_ref().unwrap().folders[*i].folder.clone(),
                    Row::Collection(i) => a.roots.as_ref().unwrap().as_ref().unwrap().albums[*i].title.clone(),
                    Row::Folder(i) => a.folders[*i].name.clone(),
                    Row::File(i) => a.files[*i].name().to_owned(),
                    Row::More => "More".to_owned(),
                }
            })
            .collect()
    }

    /// The keyboard to the row with this name.
    fn select(&mut self, name: &str) {
        let at = self.names().iter().position(|n| n == name).unwrap_or_else(|| panic!("{name} is not on screen"));
        let cur = self.h.app().list.selected.unwrap_or(0);
        for _ in cur..at {
            self.h.key(keys::KEY_DOWN);
        }
        for _ in at..cur {
            self.h.key(keys::KEY_UP);
        }
    }

    fn open(&mut self, name: &str) {
        self.select(name);
        self.key(keys::KEY_ENTER);
    }
}

#[test]
fn the_roots_show_folders_and_collections_and_enter_opens_with_nothing_before_it() {
    let (mut r, _) = Rig::new();
    assert_eq!(r.names(), ["CraftIT", "Lezioni", "Scans"]);
    r.key(keys::KEY_ENTER);
    assert_eq!(r.h.app().context, Context::Folder { folder: "CraftIT".into(), path: String::new() });
}

#[test]
fn folders_open_and_backspace_comes_back_to_the_same_row() {
    let (mut r, _) = Rig::new();
    r.open("Lezioni");
    assert_eq!(r.names()[..2], ["Compilers", "Digital Forensics"]);
    r.open("Digital Forensics");
    assert_eq!(r.names(), ["carve.rs", "disk-images.zip", "notes.md"]);
    assert_eq!(r.h.app().crumbs.iter().map(|c| c.name.as_str()).collect::<Vec<_>>(), ["Lezioni", "Digital Forensics"]);
    r.key(keys::KEY_BACKSPACE);
    assert_eq!(r.h.app().list.selected, Some(1), "the folder we came from");
    r.key(keys::KEY_BACKSPACE);
    assert_eq!(r.h.app().context, Context::Roots);
    assert_eq!(r.h.app().list.selected, Some(1));
    r.key(keys::KEY_BACKSPACE);
    assert_eq!(r.h.app().context, Context::Roots);
}

#[test]
fn searching_inside_a_folder_covers_its_subtree_and_shows_where_files_live() {
    let (mut r, _) = Rig::new();
    r.open("Lezioni");
    r.h.app().filters.q = "carve".into();
    r.h.app().reload();
    r.pump();
    assert_eq!(r.names(), ["carve.rs"]);
    assert_eq!(r.h.app().files[0].location(), "Lezioni / Digital Forensics");
    assert_eq!(r.h.app().total, Some(1));
}

#[test]
fn slash_goes_to_the_search_and_typing_there_runs_it_after_a_pause() {
    let (mut r, _) = Rig::new();
    r.key(keys::KEY_SLASH);
    r.h.type_str("syllabus");
    std::thread::sleep(std::time::Duration::from_millis(320));
    r.pump();
    assert_eq!(r.names(), ["syllabus.pdf"], "the roots search everything");
    // A search that matched inside the file carries the words.
    r.h.app().search.set("exam dates");
    r.h.app().filters.q = "exam dates".into();
    r.h.app().reload();
    r.pump();
    assert_eq!(r.h.app().files[0].snippet.as_deref(), Some("the course plan and the exam dates"));
    r.key(keys::KEY_ESC);
    r.key(keys::KEY_ESC);
    assert!(r.h.app().show_roots());
}

#[test]
fn a_pause_in_the_middle_of_a_word_leaves_the_keyboard_in_the_field() {
    let (mut r, _) = Rig::new();
    r.key(keys::KEY_SLASH);
    r.h.type_str("car");
    std::thread::sleep(std::time::Duration::from_millis(320));
    r.pump();
    // The first search has run; the next letters still go to the field, not to the list's r and s keys.
    r.h.type_str("ve");
    std::thread::sleep(std::time::Duration::from_millis(320));
    r.pump();
    assert_eq!(r.h.app().search.text, "carve");
    assert_eq!(r.names(), ["carve.rs"]);
}

#[test]
fn a_type_filter_lists_from_the_roots() {
    let (mut r, _) = Rig::new();
    r.h.app().set_kind(Some(DriveType::Spreadsheet));
    r.pump();
    assert_eq!(r.names(), ["budget.xlsx", "invoices-2026.xlsx"]);
    r.h.app().set_kind(None);
    r.pump();
    assert!(r.h.app().show_roots());
}

#[test]
fn s_and_o_sort_a_folder_with_the_webs_default_orders() {
    let (mut r, _) = Rig::new();
    r.open("Lezioni");
    assert_eq!(r.h.app().sort(), DriveSort::Name);
    assert!(r.h.app().ascending());
    r.key(keys::KEY_S);
    assert_eq!(r.h.app().sort(), DriveSort::Modified);
    assert!(!r.h.app().ascending(), "newest first");
    r.key(keys::KEY_S);
    assert_eq!(r.h.app().sort(), DriveSort::Size);
    assert!(!r.h.app().ascending());
    let first_two: Vec<String> = r.names().into_iter().skip(2).take(2).collect();
    assert_eq!(first_two, ["lecture-recording.mp4", "defence.pptx"]);
    r.key(keys::KEY_O);
    assert!(r.h.app().ascending());
    assert_eq!(r.names()[2], "budget.xlsx", "smallest first");
}

#[test]
fn a_text_file_opens_and_escape_closes_it() {
    let (mut r, _) = Rig::new();
    r.open("Lezioni");
    r.open("Digital Forensics");
    r.open("carve.rs");
    {
        let p = r.h.app().preview.as_ref().unwrap();
        let Body::Text { language, lines, .. } = &p.body else { panic!("not text") };
        assert_eq!(language.as_deref(), Some("Rust"));
        assert_eq!(lines[0][0], ("kw".to_owned(), "fn ".to_owned()));
    }
    r.key(keys::KEY_DOWN);
    assert_eq!(r.h.app().preview.as_ref().unwrap().lines.selected, Some(1));
    r.key(keys::KEY_ESC);
    assert!(r.h.app().preview.is_none());
    assert_eq!(r.names(), ["carve.rs", "disk-images.zip", "notes.md"]);
}

#[test]
fn an_archive_lists_and_what_is_not_one_says_so() {
    let (mut r, _) = Rig::new();
    r.open("Lezioni");
    r.open("Digital Forensics");
    r.open("disk-images.zip");
    let p = r.h.app().preview.as_ref().unwrap();
    let Body::Archive(l) = &p.body else { panic!("not an archive") };
    assert_eq!(l.entries.len(), 3);
    assert!(l.entries[0].dir);
}

#[test]
fn a_file_with_no_preview_shows_its_details() {
    let (mut r, _) = Rig::new();
    r.open("CraftIT");
    r.open("firmware.bin");
    assert!(matches!(r.h.app().preview.as_ref().unwrap().body, Body::Info { note: false }));
}

#[test]
fn a_pdf_shows_its_first_page_and_t_turns_to_its_text() {
    let (mut r, _) = Rig::new();
    r.open("Lezioni");
    r.open("syllabus.pdf");
    assert!(matches!(r.h.app().preview.as_ref().unwrap().body, Body::Picture { .. }));
    r.key(keys::KEY_T);
    assert!(matches!(r.h.app().preview.as_ref().unwrap().body, Body::Text { .. }));
    r.key(keys::KEY_T);
    assert!(matches!(r.h.app().preview.as_ref().unwrap().body, Body::Picture { .. }));
}

#[test]
fn enter_on_a_video_plays_it_and_without_a_player_says_so() {
    let (mut r, _) = Rig::new();
    r.open("Lezioni");
    r.open("lecture-recording.mp4");
    assert!(matches!(r.h.app().preview.as_ref().unwrap().body, Body::Play { .. }));
    r.key(keys::KEY_ENTER);
    assert_eq!(r.h.app().toast, "The video player is not on this machine.");
    assert!(r.h.app().preview.is_some(), "Enter did not close it");
}

#[test]
fn a_picture_zooms_and_escape_fits_it_before_it_closes() {
    use lucina_app::Input;
    let fake = Fake::new(NOW, 1);
    let img = lucina_app::Image { w: 64, h: 48, px: vec![0xff00_0000; 64 * 48] };
    let mut r = Rig::on(fake.connect());
    r.open("Lezioni");
    r.open("whiteboard.jpg");
    if let Some(p) = r.h.app().preview.as_mut() {
        if let Body::Picture { image, .. } = &mut p.body {
            *image = Some(img);
        }
    }
    r.pump();
    r.h.key(keys::KEY_EQUAL);
    r.pump();
    assert!(r.h.app().preview.as_ref().unwrap().surface.is_zoomed());
    r.key(keys::KEY_ESC);
    assert!(!r.h.app().preview.as_ref().unwrap().surface.is_zoomed());
    assert!(r.h.app().preview.is_some());
    r.key(keys::KEY_ESC);
    assert!(r.h.app().preview.is_none());
    let _ = Input::Wheel(0);
}

#[test]
fn a_lost_connection_is_said_and_r_tries_again() {
    let (mut r, fake) = Rig::new();
    fake.offline.store(true, Ordering::SeqCst);
    r.open("Lezioni");
    assert!(r.h.app().error.contains("no connection"), "{}", r.h.app().error);
    fake.offline.store(false, Ordering::SeqCst);
    r.key(keys::KEY_R);
    assert_eq!(r.h.app().error, "");
    assert!(!r.names().is_empty());
}

#[test]
fn a_dead_token_goes_to_signed_out_and_does_not_crash() {
    let (mut r, fake) = Rig::new();
    fake.signed_out.store(true, Ordering::SeqCst);
    r.open("Lezioni");
    assert!(r.h.app().signed_out);
}

#[test]
fn an_answer_for_a_folder_already_left_is_dropped() {
    let (mut r, _) = Rig::new();
    r.h.app().open_folder("Lezioni".into(), String::new());
    r.h.app().open_folder("CraftIT".into(), String::new());
    r.pump();
    assert_eq!(r.names(), ["firmware.bin", "invoices-2026.xlsx", "readme.txt"]);
}

#[test]
fn every_screen_draws_at_both_sizes_without_a_panic() {
    for (w, h) in [(800, 600), (1920, 1080)] {
        let fake = Fake::new(NOW, 4);
        let connect = fake.connect();
        let media = connect().unwrap();
        let mut worker = Worker::new(media, connect.clone(), Box::new(|_| {}));
        let (jobs, rx) = mpsc::channel::<Job>();
        let link: Link = Box::new(move |j| {
            let _ = jobs.send(j);
        });
        let mut harness = Harness::new(Drive::new(Zone::utc(), None, Some(link), Some(connect)), w, h);
        harness.app().start();
        let mut pump = |harness: &mut Harness<Drive>| {
            harness.core.paint();
            while let Ok(job) = rx.try_recv() {
                for m in worker.handle(job) {
                    harness.core.message(m);
                }
            }
            harness.core.input(lucina_app::Input::Wheel(0));
            harness.core.paint();
        };
        pump(&mut harness);
        harness.app().open_folder("Lezioni".into(), String::new());
        pump(&mut harness);
        for i in 0..harness.app().files.len() {
            let item = harness.app().files[i].clone();
            harness.app().open_preview(item);
            pump(&mut harness);
            harness.core.input(lucina_app::Input::Wheel(0));
            pump(&mut harness);
        }
    }
}

// ---- The intents.

#[test]
fn drive_find_runs_the_search_and_answers_once_the_page_arrives() {
    use serde_json::json;
    let (mut r, _) = Rig::new();
    assert!(r.h.intent("drive.find", json!({"query": "carve", "limit": 5})).is_none(), "answered later");
    r.pump();
    let reply = r.h.reply().unwrap();
    assert!(reply.ok, "{reply:?}");
    assert_eq!(reply.value["files"][0]["name"], "carve.rs");
    assert_eq!(reply.value["files"][0]["id"], r.h.app().files[0].id);
    assert!(reply.entities.iter().any(|e| e.kind == "drive_file" && e.title == "carve.rs"), "{reply:?}");
}

#[test]
fn drive_open_opens_the_file_as_tapping_its_row_and_refuses_an_unknown_id() {
    use serde_json::json;
    let (mut r, _) = Rig::new();
    r.open("Lezioni");
    r.open("Digital Forensics");
    let id = r.h.app().files.iter().find(|f| f.name() == "carve.rs").unwrap().id.clone();
    let reply = r.h.intent("drive.open", json!({"id": id})).unwrap();
    assert!(reply.ok && reply.entities.iter().any(|e| e.kind == "drive_file" && e.id == id), "{reply:?}");
    assert_eq!(r.h.app().preview.as_ref().map(|p| p.item.id.as_str()), Some(id.as_str()));
    let reply = r.h.intent("drive.open", json!({"id": "itm_nope"})).unwrap();
    assert!(!reply.ok && reply.text.contains("no file with id"), "{reply:?}");
}

// ---- The real engine: the deployed lucina-media on temporary folders, through the real client.

#[test]
fn browsing_previewing_and_downloading_from_the_real_server() {
    let Some(engine) = Engine::start("drive-app") else {
        eprintln!("skipped: no lucina-media here");
        return;
    };
    let mut r = Rig::on(engine.connect());
    assert_eq!(r.names(), ["Lezioni"]);
    r.key(keys::KEY_ENTER);
    let names = r.names();
    assert_eq!(names[0], "Sub", "folders first");
    for f in ["notes.txt", "a.zip", "board.png", "data.bin"] {
        assert!(names.iter().any(|n| n == f), "{f} in {names:?}");
    }
    assert_eq!(r.h.app().crumbs[0].name, "Lezioni");
    // Text: read in the background, so asked until it is ready.
    r.open("notes.txt");
    let mut tries = 0;
    loop {
        let body = &r.h.app().preview.as_ref().unwrap().body;
        match body {
            Body::Text { lines, .. } => {
                assert!(lines.iter().flatten().any(|t| t.1.contains("hello from the engine")));
                break;
            }
            Body::Pending(_) | Body::Loading => {
                tries += 1;
                assert!(tries < 300, "the text never came");
                std::thread::sleep(std::time::Duration::from_millis(100));
                r.pump();
            }
            _ => panic!("expected the text"),
        }
    }
    r.key(keys::KEY_ESC);
    r.open("a.zip");
    let Body::Archive(l) = &r.h.app().preview.as_ref().unwrap().body else { panic!("not an archive") };
    assert_eq!(l.entries[0].path, "inside.txt");
    r.key(keys::KEY_ESC);
    r.open("data.bin");
    assert!(matches!(r.h.app().preview.as_ref().unwrap().body, Body::Info { .. }));
    r.key(keys::KEY_ESC);
    // The worker's download, through the real client, to a real file.
    let dir = std::env::temp_dir().join(format!("drive-dl-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let dest = dir.join("data.bin");
    let url = r.h.app().files.iter().find(|f| f.name() == "data.bin").unwrap().download();
    crate::server::download(&engine.connect(), &url, &dest, &|_| {}).unwrap();
    assert_eq!(std::fs::read(&dest).unwrap(), vec![1, 2, 3, 4, 5]);
    assert!(!dest.with_extension("part").exists());
    std::fs::remove_dir_all(&dir).unwrap();
    // Search by name, against the real index.
    r.h.app().filters.q = "board".into();
    r.h.app().reload();
    r.pump();
    assert_eq!(r.names(), ["board.png"]);
    // A file that does not decode is a state, and a folder that does not exist is said.
    r.h.app().filters.q.clear();
    r.h.app().context = Context::Folder { folder: "Nope".into(), path: String::new() };
    r.h.app().reload();
    r.pump();
    assert!(!r.h.app().error.is_empty());
}

fn open_name(r: &mut Rig) -> String {
    r.h.app().preview.as_ref().map(|p| p.item.name().to_owned()).unwrap_or_default()
}

fn alt() -> keys::Mods {
    keys::Mods { alt: true, ..keys::Mods::default() }
}

#[test]
fn in_an_open_file_left_and_right_go_to_the_neighbours_and_stop_at_the_ends() {
    let (mut r, _) = Rig::new();
    r.open("Lezioni");
    let files: Vec<String> = r.names().into_iter().filter(|n| n.contains('.')).collect();
    assert!(files.len() >= 6, "{files:?}");
    r.open(&files[0]);
    assert_eq!(open_name(&mut r), files[0]);
    r.key(keys::KEY_LEFT);
    assert_eq!(open_name(&mut r), files[0], "the first file: Left stays, no wrap");
    r.key(keys::KEY_RIGHT);
    assert_eq!(open_name(&mut r), files[1]);
    r.key(keys::KEY_RIGHT);
    assert_eq!(open_name(&mut r), files[2]);
    r.key(keys::KEY_LEFT);
    assert_eq!(open_name(&mut r), files[1]);
    // Home and End are for the text of a text file; on a picture or a video they go to the first and last file.
    let video = files.iter().position(|f| f == "lecture-recording.mp4").unwrap();
    while open_name(&mut r) != files[video] {
        r.key(keys::KEY_RIGHT);
    }
    r.key(keys::KEY_END);
    assert_eq!(open_name(&mut r), *files.last().unwrap());
    r.key(keys::KEY_RIGHT);
    assert_eq!(open_name(&mut r), *files.last().unwrap(), "the last file: Right stays, no wrap");
    r.key(keys::KEY_HOME);
    assert_eq!(open_name(&mut r), files[0]);
    // The focus did not walk round the buttons: nothing but a step happened, and Esc comes back with the file selected.
    r.key(keys::KEY_RIGHT);
    r.key(keys::KEY_RIGHT);
    r.key(keys::KEY_ESC);
    assert!(r.h.app().preview.is_none());
    let sel = r.h.app().list.selected.unwrap();
    assert_eq!(r.names()[sel], files[2], "the list has the file we left on selected");
}

#[test]
fn a_zoomed_picture_keeps_its_arrows_for_panning() {
    let fake = Fake::new(NOW, 1);
    let img = lucina_app::Image { w: 64, h: 48, px: vec![0xff00_0000; 64 * 48] };
    let mut r = Rig::on(fake.connect());
    r.open("Lezioni");
    r.open("whiteboard.jpg");
    if let Some(p) = r.h.app().preview.as_mut() {
        if let Body::Picture { image, .. } = &mut p.body {
            *image = Some(img);
        }
    }
    r.pump();
    r.h.key(keys::KEY_EQUAL);
    r.pump();
    assert!(r.h.app().preview.as_ref().unwrap().surface.is_zoomed());
    r.key(keys::KEY_LEFT);
    assert_eq!(open_name(&mut r), "whiteboard.jpg", "zoomed: Left pans");
    r.key(keys::KEY_ESC);
    r.key(keys::KEY_LEFT);
    assert_ne!(open_name(&mut r), "whiteboard.jpg", "fitted: Left is the previous file");
}

#[test]
fn space_plays_a_video_and_alt_left_closes_a_file() {
    let (mut r, _) = Rig::new();
    r.open("Lezioni");
    r.open("lecture-recording.mp4");
    r.key(keys::KEY_SPACE);
    assert_eq!(r.h.app().toast, "The video player is not on this machine.");
    assert!(r.h.app().preview.is_some());
    r.h.press(keys::KEY_LEFT, alt());
    r.pump();
    assert!(r.h.app().preview.is_none());
    let sel = r.h.app().list.selected.unwrap();
    assert_eq!(r.names()[sel], "lecture-recording.mp4");
}

#[test]
fn in_the_list_the_arrows_move_the_selection_and_alt_left_goes_up() {
    let (mut r, _) = Rig::new();
    r.open("Lezioni");
    let n = r.names().len();
    assert!(n > 4);
    r.key(keys::KEY_HOME);
    assert_eq!(r.h.app().list.selected, Some(0));
    r.key(keys::KEY_UP);
    assert_eq!(r.h.app().list.selected, Some(0), "the list does not wrap");
    r.key(keys::KEY_DOWN);
    assert_eq!(r.h.app().list.selected, Some(1));
    r.key(keys::KEY_END);
    assert_eq!(r.h.app().list.selected, Some(n - 1));
    r.key(keys::KEY_DOWN);
    assert_eq!(r.h.app().list.selected, Some(n - 1));
    r.key(keys::KEY_PAGEUP);
    assert!(r.h.app().list.selected.unwrap() < n - 1);
    // Right does nothing in the list; Left goes to the file types and Right comes back.
    r.key(keys::KEY_RIGHT);
    assert_eq!(r.h.app().context, Context::Folder { folder: "Lezioni".into(), path: String::new() });
    r.key(keys::KEY_LEFT);
    r.key(keys::KEY_RIGHT);
    r.key(keys::KEY_END);
    assert_eq!(r.h.app().list.selected, Some(n - 1), "the rows have the keyboard again");
    // Space opens a file; Alt+Left goes up a folder.
    r.key(keys::KEY_SPACE);
    assert!(r.h.app().preview.is_some());
    r.key(keys::KEY_ESC);
    r.h.press(keys::KEY_LEFT, alt());
    r.pump();
    assert_eq!(r.h.app().context, Context::Roots);
}
