//! The tile's screen: the videos on this laptop, a folder at a time. A video plays in this same program started again over the list, so
//! the list is an ordinary toolkit app and the player owns the screen outright while it runs.

use std::path::{Path, PathBuf};

use lucina_app::intent::{ArgSpec, Call, Effect, Entity, IntentSpec, Reply, Responder};
use lucina_app::program::{Ended, Program};
use lucina_app::{App, ListState, Ui, keys};
use serde_json::json;

pub fn is_video_name(name: &str) -> bool {
    let name = name.to_ascii_lowercase();
    [".mp4", ".m4v", ".mov", ".mkv", ".webm", ".ivf"].iter().any(|e| name.ends_with(e))
}

/// The intents Video offers Lucina, listed by `<program> --intents` and named in `manifest.app`. The list and the player answer
/// the same list.
pub const INTENTS: &[IntentSpec] = &[
    IntentSpec {
        name: "video.play",
        description: "Play a video file on this laptop, from the start or from a position in seconds, and return the file.",
        args: &[
            ArgSpec::path("path", "The video file to play.", true),
            ArgSpec::int("at", "Start this many seconds in.", false, 0, 86_400),
        ],
        effect: Effect::Write,
        shows: true,
        inverse: None,
    },
    IntentSpec {
        name: "video.now",
        description: "Say what is playing now: whether it plays, the file, and the position.",
        args: &[],
        effect: Effect::Read,
        shows: false,
        inverse: None,
    },
];

enum Row {
    Up(PathBuf),
    Dir(String, PathBuf),
    Video(String, PathBuf),
}

pub struct List {
    folder: PathBuf,
    rows: Vec<Row>,
    list: ListState,
    message: Option<String>,
    /// A `video.play` intent's file, started on the next frame the way choosing its row does.
    pending_play: Option<PathBuf>,
}

/// The playlist of a video chosen in the list: the videos of its folder in the list's order, and where the file is among them.
/// A file that is not in its folder's listing (a hidden one, or one that has gone) is a playlist of itself.
pub fn playlist(file: &Path) -> (Vec<PathBuf>, usize) {
    let Some(dir) = file.parent() else { return (vec![file.to_owned()], 0) };
    let videos: Vec<PathBuf> = rows_of(dir)
        .into_iter()
        .filter_map(|r| match r {
            Row::Video(_, p) => Some(p),
            _ => None,
        })
        .collect();
    match videos.iter().position(|p| p == file) {
        Some(at) => (videos, at),
        None => (vec![file.to_owned()], 0),
    }
}

fn rows_of(dir: &Path) -> Vec<Row> {
    let (mut dirs, mut videos) = (Vec::new(), Vec::new());
    for e in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        if name.starts_with('.') {
            continue;
        }
        let path = e.path();
        if path.is_dir() {
            dirs.push((name, path));
        } else if is_video_name(&name) {
            videos.push((name, path));
        }
    }
    dirs.sort();
    videos.sort();
    let mut rows = Vec::new();
    if let Some(up) = dir.parent() {
        rows.push(Row::Up(up.to_owned()));
    }
    rows.extend(dirs.into_iter().map(|(n, p)| Row::Dir(n, p)));
    rows.extend(videos.into_iter().map(|(n, p)| Row::Video(n, p)));
    rows
}

impl List {
    pub fn new(folder: PathBuf) -> List {
        let mut l = List { folder: PathBuf::new(), rows: Vec::new(), list: ListState::new(), message: None, pending_play: None };
        l.enter(folder);
        l
    }

    /// Up a folder, with the one we left selected.
    fn up(&mut self) {
        let Some(parent) = self.folder.parent().map(Path::to_owned) else { return };
        let came = self.folder.clone();
        self.enter(parent);
        if let Some(row) = self.rows.iter().position(|r| matches!(r, Row::Dir(_, p) if *p == came)) {
            self.list.select(row);
        }
    }

    fn enter(&mut self, dir: PathBuf) {
        self.rows = rows_of(&dir);
        self.folder = dir;
        self.list = ListState::new();
        // Up is first: start on the first thing there is to open.
        let first = self.rows.iter().position(|r| !matches!(r, Row::Up(_))).or((!self.rows.is_empty()).then_some(0));
        self.list.selected = first;
    }
}

impl App for List {
    type Msg = ();

    fn intents() -> &'static [IntentSpec] {
        INTENTS
    }

    fn intent(&mut self, call: Call, reply: Responder) {
        match call.name.as_str() {
            "video.play" => {
                let path = PathBuf::from(call.args["path"].as_str().unwrap_or(""));
                let name = path
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default();
                if !path.is_file() {
                    return reply.send(Reply::failed(format!("{} is not a file", path.display())));
                }
                if !is_video_name(&name) {
                    return reply.send(Reply::failed(format!(
                        "{name} is not a video file Video plays"
                    )));
                }
                // Started on the next frame, the same way as choosing its row.
                self.pending_play = Some(path.clone());
                reply.send(
                    Reply::done(format!("Playing {name}")).with_entity(Entity::new(
                        "file",
                        &path.display().to_string(),
                        &name,
                    )),
                );
            }
            "video.now" => {
                reply.send(
                    Reply::done("Nothing is playing in the list")
                        .with_value(json!({"playing": false})),
                );
            }
            _ => reply.send(Reply::failed(format!("no intent {} here", call.name))),
        }
    }

    fn view(&mut self, ui: &mut Ui) {
        if let Some(e) = ui.program_ended() {
            self.message = match e {
                Ended::Ok => None,
                Ended::Failed(m) | Ended::NotStarted(m) => Some(m),
            };
        }
        if let Some(path) = self.pending_play.take() {
            let arg = path.display().to_string();
            match Program::app("video", &[&arg]) {
                Some(program) => ui.run_program(program),
                None => {
                    self.message = Some("The player's program is not on this laptop.".to_owned())
                }
            }
        }
        ui.header("Video");
        ui.pad(14.0);
        let folder = self.folder.display().to_string();
        ui.dim(&folder);
        if let Some(m) = &self.message {
            ui.label(&m.clone());
        }
        if self.rows.iter().all(|r| matches!(r, Row::Up(_))) {
            ui.label("No videos or folders here. MP4, MKV, WebM and IVF files show in this list.");
        }
        let r = ui.area();
        let rows = &self.rows;
        let res = ui.list("rows", &mut self.list, r, rows.len(), 30.0, |ui, i, _| match &rows[i] {
            Row::Up(_) => ui.dim(".."),
            Row::Dir(n, _) => ui.label(&format!("{n}/")),
            Row::Video(n, _) => ui.label(n),
        });
        if let Some(i) = res.activated {
            match &self.rows[i] {
                Row::Up(p) | Row::Dir(_, p) => {
                    let p = p.clone();
                    self.enter(p);
                }
                Row::Video(_, p) => {
                    let path = p.display().to_string();
                    // `--browse`: the player gets the folder's videos to step through with the arrows.
                    match Program::app("video", &["--browse", &path]) {
                        Some(program) => ui.run_program(program),
                        None => self.message = Some("The player's program is not on this laptop.".to_owned()),
                    }
                }
            }
        }
        // Backspace and Alt+Left go up a folder. Left and Right stay on the list rather than walking the focus.
        if let Some(p) = ui.press() {
            let no_mods = !p.mods.ctrl && !p.mods.alt && !p.mods.meta && !p.mods.shift;
            if (p.mods.alt && !p.mods.ctrl && !p.mods.meta && !p.mods.shift && p.code == keys::KEY_LEFT) || (no_mods && p.code == keys::KEY_BACKSPACE) {
                ui.take_press();
                self.up();
            } else if no_mods && matches!(p.code, keys::KEY_LEFT | keys::KEY_RIGHT) {
                ui.take_press();
            }
        }
        if ui.key(keys::KEY_ESC) {
            ui.quit();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lucina_app::Harness;

    #[test]
    fn lists_folders_and_videos_and_starts_on_the_first_of_them() {
        let d = std::env::temp_dir().join(format!("lucina-video-list-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(d.join("Films")).unwrap();
        std::fs::write(d.join("a.mp4"), "x").unwrap();
        std::fs::write(d.join("b.MKV"), "x").unwrap();
        std::fs::write(d.join("notes.txt"), "x").unwrap();
        std::fs::write(d.join(".hidden.mp4"), "x").unwrap();
        let mut h = Harness::new(List::new(d.clone()), 800, 600);
        let names: Vec<String> = h.app().rows.iter().map(|r| match r {
            Row::Up(_) => "..".to_owned(),
            Row::Dir(n, _) | Row::Video(n, _) => n.clone(),
        }).collect();
        assert_eq!(names, ["..", "Films", "a.mp4", "b.MKV"]);
        assert_eq!(h.app().list.selected, Some(1));
        h.key(keys::KEY_TAB);
        h.key(keys::KEY_ENTER);
        assert_eq!(h.app().folder, d.join("Films"));
        assert_eq!(h.app().list.selected, Some(0));
        h.key(keys::KEY_ESC);
        assert!(h.core.quit_requested());
    }

    #[test]
    fn the_playlist_is_the_folders_videos_in_the_lists_order_and_the_list_goes_up() {
        let d = std::env::temp_dir().join(format!("lucina-video-playlist-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(d.join("Films")).unwrap();
        for n in ["b.mp4", "a.mkv", "c.webm", "notes.txt", ".hidden.mp4"] {
            std::fs::write(d.join(n), "x").unwrap();
        }
        let (files, at) = playlist(&d.join("b.mp4"));
        assert_eq!(files, [d.join("a.mkv"), d.join("b.mp4"), d.join("c.webm")]);
        assert_eq!(at, 1);
        assert_eq!(playlist(&d.join("gone.mp4")), (vec![d.join("gone.mp4")], 0));

        let mut h = Harness::new(List::new(d.clone()), 800, 600);
        // ..  Films  a.mkv  b.mp4  c.webm: End, then Down stays on the last row (no wrap), and Left and Right keep the list.
        h.key(keys::KEY_END);
        h.key(keys::KEY_DOWN);
        h.key(keys::KEY_RIGHT);
        h.key(keys::KEY_LEFT);
        assert_eq!(h.app().list.selected, Some(4));
        h.key(keys::KEY_HOME);
        h.key(keys::KEY_DOWN);
        h.key(keys::KEY_ENTER);
        assert_eq!(h.app().folder, d.join("Films"));
        h.press(keys::KEY_LEFT, keys::Mods { alt: true, ..keys::Mods::default() });
        assert_eq!(h.app().folder, d);
        let at = h.app().list.selected.unwrap();
        assert!(matches!(&h.app().rows[at], Row::Dir(n, _) if n == "Films"));
        h.key(keys::KEY_ENTER);
        h.key(keys::KEY_BACKSPACE);
        assert_eq!(h.app().folder, d);
    }

    #[test]
    fn the_intents_play_a_video_file_and_say_nothing_plays() {
        let d = std::env::temp_dir().join(format!("lucina-video-intents-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join("film.mp4"), "x").unwrap();
        std::fs::write(d.join("notes.txt"), "x").unwrap();
        let mut h = Harness::new(List::new(d.clone()), 800, 600);

        let r = h.intent("video.now", serde_json::json!({})).unwrap();
        assert!(
            r.ok && r.value["playing"] == serde_json::json!(false),
            "{r:?}"
        );

        let r = h
            .intent(
                "video.play",
                serde_json::json!({"path": d.join("film.mp4").display().to_string()}),
            )
            .unwrap();
        assert!(r.ok && r.text.contains("Playing film.mp4"), "{r:?}");
        assert_eq!(
            r.entities,
            vec![lucina_app::intent::Entity::new(
                "file",
                &d.join("film.mp4").display().to_string(),
                "film.mp4"
            )]
        );
        assert!(h.app().pending_play.is_some());
        h.core.input(lucina_app::Input::Wheel(0));
        h.core.paint();
        assert!(
            h.app().pending_play.is_none(),
            "the file is started on the next frame"
        );

        let r = h
            .intent(
                "video.play",
                serde_json::json!({"path": d.join("gone.mp4").display().to_string()}),
            )
            .unwrap();
        assert!(!r.ok && r.text.contains("not a file"), "{r:?}");
        let r = h
            .intent(
                "video.play",
                serde_json::json!({"path": d.join("notes.txt").display().to_string()}),
            )
            .unwrap();
        assert!(!r.ok && r.text.contains("not a video file"), "{r:?}");
    }

    #[test]
    fn knows_a_video_by_its_name() {
        assert!(is_video_name("a.MP4") && is_video_name("a.webm") && !is_video_name("a.mp3"));
    }
}
