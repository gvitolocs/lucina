//! lucina-app-camera: the Galaxy Book Go's webcam on Juno. A live preview on the whole screen and photos, through the kernel's
//! `/dev/lucina-camera0` (`lucina-camera`). The screen is held on while the preview runs, and the preview draws the camera-on
//! mark itself, since an app that has the whole screen hides the shell's bar. Photos are the camera's own JPEGs, in
//! `/data/home/Pictures/Camera` (`LUCINA_CAMERA_DIR` overrides it).

// Juno with no C library: Eyra is the libc and the program start, in Rust (docs/research/juno-pure-rust.md).
#[cfg(all(target_os = "linux", target_env = "gnu", target_arch = "aarch64"))]
extern crate eyra;

mod gpu_show;
mod preview;
#[cfg(test)]
mod tests;

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use lucina_app::app::FullScreen;
use lucina_app::hold::DisplayHold;
use lucina_app::{App, Ui};

fn photos_dir() -> PathBuf {
    std::env::var_os("LUCINA_CAMERA_DIR").map_or_else(
        || PathBuf::from("/data/home/Pictures/Camera"),
        PathBuf::from,
    )
}

/// What the last preview left for the normal view.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Outcome {
    pub photos: Vec<PathBuf>,
    pub error: Option<String>,
    /// Frames shown and the time they took, for "N frames at F fps".
    pub frames: u64,
    pub fps: f32,
}

/// What the person asked of the preview when it starts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Start {
    Preview,
    /// Take one photo at once and come back (Lucina's intent, or the button).
    Photo,
}

struct Camera {
    start: Option<Start>,
    hold: Arc<Mutex<DisplayHold>>,
    ended: Arc<Mutex<Option<Outcome>>>,
    last: Outcome,
}

impl Camera {
    fn new(start_at_once: bool) -> Camera {
        Camera {
            start: start_at_once.then_some(Start::Preview),
            hold: Arc::new(Mutex::new(DisplayHold::from_env())),
            ended: Arc::new(Mutex::new(None)),
            last: Outcome::default(),
        }
    }
}

impl App for Camera {
    type Msg = ();

    fn take_screen(&mut self) -> Option<Box<dyn FnOnce(&mut FullScreen)>> {
        let start = self.start.take()?;
        let hold = self.hold.clone();
        let ended = self.ended.clone();
        Some(Box::new(move |fs: &mut FullScreen| {
            let outcome = preview::run(fs, start, &photos_dir(), &hold);
            if let Ok(mut h) = hold.lock() {
                h.set(false, "");
            }
            if let Ok(mut e) = ended.lock() {
                *e = Some(outcome);
            }
        }))
    }

    fn view(&mut self, ui: &mut Ui) {
        if let Some(o) = self.ended.lock().ok().and_then(|mut e| e.take()) {
            self.last = o;
        }
        ui.header("Camera");
        ui.pad(14.0);
        let side = ((ui.area().w - ui.px(620.0)) / 2).max(0);
        ui.cut_left(side);
        ui.cut_right(side);
        ui.dim("The webcam above the screen. Space or Enter takes a photo, Esc comes back here.");
        ui.pad(10.0);
        if ui.primary_button("Preview") {
            self.start = Some(Start::Preview);
        }
        ui.pad(6.0);
        if ui.button("Take a photo") {
            self.start = Some(Start::Photo);
        }
        ui.pad(14.0);
        if let Some(e) = &self.last.error {
            ui.label(&format!("The camera did not work: {e}"));
        }
        if self.last.frames > 0 {
            ui.dim(&format!(
                "Last preview: {} frames at {:.1} fps.",
                self.last.frames, self.last.fps
            ));
        }
        for p in self.last.photos.iter().rev().take(5) {
            ui.label(&format!("Saved {}", p.display()));
        }
        ui.dim(&format!("Photos go to {}.", photos_dir().display()));
    }
}

fn main() {
    // `--photo` takes one photo and ends (tests and Lucina); otherwise the preview starts with the app.
    let photo_only = std::env::args().any(|a| a == "--photo");
    lucina_app::run("lucina-app-camera", move |_tx| {
        let mut app = Camera::new(true);
        if photo_only {
            app.start = Some(Start::Photo);
        }
        app
    });
}
