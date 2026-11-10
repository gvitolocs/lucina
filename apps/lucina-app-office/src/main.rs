//! lucina-app-office: the start of LucinaOffice on Juno. It lists the documents on the laptop and in the Secretary Drive, fetches
//! the one you pick and hands the process over to the editor (built from `office/`; this crate has none of its code). With
//! `--choose` it is the editor's File > Open: it prints the path it was given and ends. See `docs/research/juno-office.md`.

// Juno with no C library: Eyra is the libc and the program start, in Rust (docs/research/juno-pure-rust.md).
#[cfg(all(target_os = "linux", target_env = "gnu", target_arch = "aarch64"))]
extern crate eyra;

mod intents;
mod member_path;
mod package;

use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use lucina_app::{App, Harness, Sender, Ui};
use lucina_files::media::{Connect, system_connect};
use lucina_files::picker::{self, FilePicker, Mode, Picked, Setup};
use lucina_files::places::{self, Place};

/// The editor in a test image or on a root file system.
const ENGINE: &str = "/usr/bin/ibis-office";
/// The editor as unpacked from `office.pkg`.
const UNPACKED_ENGINE: &str = "/tmp/lucina-office/ibis-office";
const UNPACKED_DIR: &str = "/tmp/lucina-office";
/// Documents that come with Juno.
const SAMPLES: &str = "/tmp/lucina-office/samples";
/// What was fetched with no `/data` to keep it on: in memory, gone at a restart.
const WORK: &str = "/run/lucina/office";

/// What ibisOffice opens (`ibis-office-app`'s `read_file`).
const EXTENSIONS: &[&str] = &[
    "docx", "docm", "dotx", "dotm", "doc", "odt", "rtf", "xlsx", "xlsm", "xltx", "xltm", "xlsb", "xls", "ods", "csv", "tsv", "pptx", "pptm",
    "ppsx", "ppsm", "potx", "potm", "ppt", "odp",
];

/// The file picker, for documents: the samples first, then the owner's folders, then the Drive.
fn setup(drive: Option<Connect>) -> Setup {
    let mut list = vec![Place::new("Samples", SAMPLES)];
    list.extend(places::standard());
    Setup {
        title: "Open a document".to_owned(),
        mode: Mode::Open { extensions: EXTENSIONS.iter().map(|e| (*e).to_owned()).collect() },
        places: list,
        drive,
        fetch_to: picker::fetch_dir("office").or_else(|| Some(PathBuf::from(WORK))),
    }
}

fn find_engine() -> Option<PathBuf> {
    [UNPACKED_ENGINE, ENGINE].iter().map(PathBuf::from).find(|p| p.exists())
}

fn exec_engine(engine: &Path, path: &Path, error: &mut Option<String>) {
    // Only comes back if it failed. The editor takes over this process, so the shell's F1 and crash handling still apply.
    let e = std::process::Command::new(engine).arg(path).exec();
    *error = Some(format!("{}: {e}", engine.display()));
}

/// `office.pkg`, beside this app's program on the disk: the editor, its fonts and the samples, a tar.gz for BusyBox to read. Into
/// `/tmp`: it costs RAM from then on, and is gone at a restart. Each start of the app unpacks it again: an unpacked copy that
/// was already there could be anybody's. This program goes in too, as the editor's File > Open.
fn unpack_engine() -> Result<(), String> {
    let dir = std::env::var_os("LUCINA_APP_DIR").map(PathBuf::from).ok_or("the editor is not installed")?;
    let pkg = dir.join("office.pkg");
    if !pkg.exists() {
        return Err(format!("the editor is not installed ({} is missing)", pkg.display()));
    }
    let me = std::env::current_exe().map_err(|e| e.to_string())?;
    package::install(&pkg, env!("LUCINA_OFFICE_PKG_SHA256"), Path::new(UNPACKED_DIR), Path::new("/bin/tar"), Some((&me, "list")))
}

enum Msg {
    Prepared(Result<(), String>),
    Changed,
}

struct Office {
    tx: Option<Sender<Msg>>,
    picker: FilePicker,
    error: Option<String>,
    /// `--choose`: the path goes to stdout.
    choosing: bool,
    /// `office.pkg` is being unpacked.
    preparing: bool,
    /// What to open when it is.
    pending: Option<PathBuf>,
    status: String,
}

impl Office {
    fn new(tx: Option<Sender<Msg>>, setup: Setup) -> Office {
        let waker = tx.clone();
        let wake: Arc<dyn Fn() + Send + Sync> = Arc::new(move || {
            if let Some(tx) = &waker {
                tx.send(Msg::Changed);
            }
        });
        Office { tx, picker: FilePicker::start(setup, wake), error: None, choosing: false, preparing: false, pending: None, status: String::new() }
    }

    fn open(&mut self, path: &Path) {
        if self.choosing {
            println!("{}", path.display());
            std::process::exit(0);
        }
        if self.preparing {
            self.pending = Some(path.to_owned());
            self.status = "Starting the editor…".into();
            return;
        }
        match find_engine() {
            Some(engine) => exec_engine(&engine, path, &mut self.error),
            None => self.error = Some("the editor is not installed".into()),
        }
    }

    /// The first time: the editor and the sample documents out of the package, on a thread, with the list already up.
    fn prepare(&mut self) {
        let Some(tx) = self.tx.clone() else { return };
        // A test image has the editor on its root; the one in `/tmp` is never taken as it is.
        if self.choosing || Path::new(ENGINE).exists() {
            return;
        }
        self.preparing = true;
        tx.spawn(|| Msg::Prepared(unpack_engine()));
    }
}

impl App for Office {
    type Msg = Msg;

    fn intents() -> &'static [lucina_app::intent::IntentSpec] {
        intents::INTENTS
    }

    fn intent(&mut self, call: lucina_app::intent::Call, reply: lucina_app::intent::Responder) {
        intents::intent(self, call, reply);
    }

    fn message(&mut self, m: Msg) {
        match m {
            Msg::Prepared(result) => {
                self.preparing = false;
                self.status.clear();
                // The samples are there now.
                self.picker.refresh();
                if let Err(e) = result {
                    self.error = Some(e);
                } else if let Some(path) = self.pending.take() {
                    self.open(&path);
                }
            }
            Msg::Changed => {}
        }
    }

    fn view(&mut self, ui: &mut Ui) {
        ui.header("LucinaOffice");
        ui.pad(14.0);
        if !self.status.is_empty() {
            ui.dim(&self.status);
        }
        match self.picker.view(ui) {
            Some(Picked::File(path)) => self.open(&path),
            Some(Picked::Cancelled) => ui.quit(),
            None => {}
        }
        if let Some(e) = self.error.clone() {
            ui.modal(460.0, 170.0, |ui| {
                ui.heading("Cannot open it");
                ui.paragraph(&e);
                if ui.primary_button("OK") {
                    self.error = None;
                }
            });
        }
    }
}

fn main() {
    let mut args = std::env::args().skip(1);
    if args.next().as_deref() == Some("--render") {
        let out = args.next().unwrap_or_else(|| "office.ppm".into());
        let h = Harness::new(Office::new(None, setup(None)), 1920, 1080);
        std::fs::write(out, h.ppm()).expect("write");
        return;
    }
    let choosing = std::env::args().nth(1).as_deref() == Some("--choose");
    lucina_app::run("lucina-app-office", move |tx| {
        let mut office = Office::new(Some(tx), setup(Some(system_connect())));
        office.choosing = choosing;
        office.prepare();
        office
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use lucina_app::keys;

    fn temp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("office-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn here(dir: &Path) -> Setup {
        Setup { places: vec![Place::new("Here", dir)], ..setup(None) }
    }

    fn settle(h: &mut Harness<Office>) {
        for _ in 0..200 {
            h.core.input(lucina_app::Input::Wheel(0));
            h.core.paint();
            if !h.app().picker.is_busy() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        h.core.input(lucina_app::Input::Wheel(0));
        h.core.paint();
    }

    #[test]
    fn the_list_has_the_keyboard_and_shows_documents_only() {
        let d = temp("list");
        std::fs::write(d.join("b.pptx"), b"12").unwrap();
        std::fs::write(d.join("A.docx"), b"1").unwrap();
        std::fs::write(d.join("c.txt"), b"1").unwrap();
        let mut h = Harness::new(Office::new(None, here(&d)), 800, 600);
        settle(&mut h);
        // A.docx, b.pptx. Enter on the second asks to open it: with no editor here, that is said.
        h.key(keys::KEY_DOWN);
        h.key(keys::KEY_ENTER);
        settle(&mut h);
        assert_eq!(h.app().error.as_deref(), Some("the editor is not installed"));
        std::fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn it_opens_what_ibis_opens() {
        let Mode::Open { extensions } = setup(None).mode else { panic!("not opening") };
        assert!(extensions.iter().any(|e| e == "xlsx"));
        assert!(extensions.iter().any(|e| e == "odp"));
        assert!(!extensions.iter().any(|e| e == "txt"));
    }

    #[test]
    fn the_samples_come_first_and_then_the_owners_folders() {
        let s = setup(None);
        assert_eq!(s.places[0].name, "Samples");
        assert_eq!(s.places[1].name, "Home");
    }

    #[test]
    fn create_writes_a_document_and_its_write_takes_it_away_again() {
        use lucina_app::intent::{rules, there};
        use serde_json::json;
        let d = temp("create");
        let path = d.join("notes/new.docx");
        let mut h = Harness::new(Office::new(None, here(&d)), 800, 600);
        let r = h.intent("office.create", json!({"path": path.to_str().unwrap()})).unwrap();
        assert!(r.ok, "{r:?}");
        assert!(r.entities.iter().any(|e| e.kind == "file" && e.id == path.to_string_lossy()), "{r:?}");
        let bytes = std::fs::read(&path).unwrap();
        assert!(intents::is_zip(&bytes), "a docx is a zip");
        let w = r.write.unwrap();
        let entry = w.entry().unwrap();
        assert_eq!(entry.op, rules::UndoOp::TakeAway);
        // The undo works while the file is what the write left, and is refused once it changed.
        assert!(rules::undo_to(there(&bytes), entry).is_some());
        std::fs::write(&path, b"changed").unwrap();
        assert!(rules::undo_to(there(b"changed"), entry).is_none());
        std::fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn create_refuses_an_existing_path_and_a_bad_extension() {
        use serde_json::json;
        let d = temp("refuse");
        std::fs::write(d.join("there.docx"), b"x").unwrap();
        let mut h = Harness::new(Office::new(None, here(&d)), 800, 600);
        let r = h.intent("office.create", json!({"path": d.join("there.docx").to_str().unwrap()})).unwrap();
        assert!(!r.ok && r.text.contains("is there already"), "{r:?}");
        let r = h.intent("office.create", json!({"path": d.join("a.txt").to_str().unwrap()})).unwrap();
        assert!(!r.ok && r.text.contains("not a document type"), "{r:?}");
        assert!(!d.join("a.txt").exists());
        std::fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn open_answers_with_the_file_and_refuses_a_missing_one() {
        use serde_json::json;
        let d = temp("open");
        std::fs::write(d.join("b.docx"), b"12").unwrap();
        let mut h = Harness::new(Office::new(None, here(&d)), 800, 600);
        let r = h.intent("office.open", json!({"path": d.join("b.docx").to_str().unwrap()})).unwrap();
        assert!(r.ok && r.text.contains("Opening b.docx"), "{r:?}");
        assert!(r.entities.iter().any(|e| e.kind == "file" && e.title == "b.docx"), "{r:?}");
        // No editor here, so the exec never happens: the app says so instead.
        assert_eq!(h.app().error.as_deref(), Some("the editor is not installed"));
        let r = h.intent("office.open", json!({"path": d.join("nope.docx").to_str().unwrap()})).unwrap();
        assert!(!r.ok && r.text.contains("is not a file"), "{r:?}");
        std::fs::write(d.join("c.txt"), b"1").unwrap();
        let r = h.intent("office.open", json!({"path": d.join("c.txt").to_str().unwrap()})).unwrap();
        assert!(!r.ok && r.text.contains("not a document"), "{r:?}");
        std::fs::remove_dir_all(&d).unwrap();
    }
}
