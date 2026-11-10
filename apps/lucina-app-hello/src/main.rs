//! lucina-app-hello: the template for a Juno app, and the proof of the app model. Copy this crate to
//! start an app (`docs/research/juno-apps.md`).
//!
//! A filter field over a long list, a detail pane, a picture, messages from a worker thread and three
//! ways to go wrong: a panic, a segmentation fault and a hang, which the shell must survive.

// Juno with no C library: Eyra is the libc and the program start, in Rust (docs/research/juno-pure-rust.md).
#[cfg(all(target_os = "linux", target_env = "gnu", target_arch = "aarch64"))]
extern crate eyra;

use std::time::Duration;

use lucina_app::keys;
use lucina_app::{App, Harness, Image, ListState, Sender, TextEdit, Ui};

/// A picture small enough to live in the source: decoded by the toolkit like any other.
const LOGO: &[u8] = include_bytes!("logo.png");

/// What the worker thread reports.
enum Msg {
    Tick(u32),
}

struct Hello {
    filter: TextEdit,
    note: TextEdit,
    list: ListState,
    rows: Vec<String>,
    shown: Vec<usize>,
    logo: Option<Image>,
    ticks: u32,
    remember: bool,
    confirm_crash: bool,
    last_opened: Option<String>,
}

impl Hello {
    fn new(tx: Option<Sender<Msg>>) -> Hello {
        let rows: Vec<String> = (1..=2000)
            .map(|i| {
                format!(
                    "Row {i}: {}",
                    ["apples", "bread", "coffee", "dates", "eggs"][i % 5]
                )
            })
            .collect();
        let shown = (0..rows.len()).collect();
        // The usual shape of slow work: a thread that sends its results, here once a second.
        if let Some(tx) = tx {
            std::thread::spawn(move || {
                let mut n = 0;
                loop {
                    std::thread::sleep(Duration::from_secs(1));
                    n += 1;
                    tx.send(Msg::Tick(n));
                }
            });
        }
        Hello {
            filter: TextEdit::new().placeholder("Filter the list"),
            note: TextEdit::multiline().placeholder("A note on several lines"),
            list: ListState::new(),
            rows,
            shown,
            logo: Image::decode(LOGO).ok().map(|i| i.fit_within(96, 96)),
            ticks: 0,
            remember: true,
            confirm_crash: false,
            last_opened: None,
        }
    }

    fn apply_filter(&mut self) {
        let f = self.filter.text.to_lowercase();
        self.shown = (0..self.rows.len())
            .filter(|&i| self.rows[i].to_lowercase().contains(&f))
            .collect();
        self.list.scroll = 0;
        self.list.selected = None;
    }
}

#[allow(unconditional_panic)]
fn segfault() {
    // SAFETY: not safe at all, on purpose: this is the crash the shell has to survive.
    unsafe { std::ptr::null_mut::<u8>().write_volatile(1) };
}

impl App for Hello {
    type Msg = Msg;

    fn message(&mut self, m: Msg) {
        match m {
            Msg::Tick(n) => self.ticks = n,
        }
    }

    fn on_quit(&mut self) {
        eprintln!("lucina-app-hello: saving, and leaving");
    }

    fn view(&mut self, ui: &mut Ui) {
        ui.header("Hello");
        ui.pad(14.0);

        let right = ui.px(330.0);
        let pane = ui.cut_right(right);
        let gap = ui.px(14.0);
        ui.cut_right(gap);

        // The left side: a filter and the list under it.
        if ui.text_field("filter", &mut self.filter).changed {
            self.apply_filter();
        }
        let r = ui.area();
        let (shown, rows) = (&self.shown, &self.rows);
        let res = ui.list(
            "list",
            &mut self.list,
            r,
            shown.len(),
            30.0,
            |ui, i, selected| {
                let a = ui.area();
                let t = ui.theme();
                let c = if selected { t.text } else { t.item };
                let inner = lucina_app::Rect::new(a.x + t.px(8.0), a.y, a.w - t.px(8.0), a.h);
                ui.text_in(
                    inner,
                    lucina_app::Face::Sans,
                    t.pt(15.0),
                    c,
                    &rows[shown[i]],
                );
            },
        );
        if let Some(i) = res.activated {
            let row = self.rows[self.shown[i]].clone();
            eprintln!("lucina-app-hello: opened {row}");
            self.last_opened = Some(row);
        }

        // The right side.
        ui.within(pane, |ui| {
            ui.heading("The template app");
            ui.paragraph(
                "Everything every app needs: a text field with a cursor and a selection, a list \
                 of any length, buttons, focus with Tab and arrows, the pointer, wrapped text, \
                 pictures and a worker thread. F1 goes home from anywhere.",
            );
            if let Some(logo) = &self.logo {
                ui.image(logo);
            }
            ui.label(&format!("The worker says: {} s", self.ticks));
            if let Some(row) = &self.last_opened {
                ui.dim(&format!("Opened: {row}"));
            }
            ui.checkbox("Remember this", &mut self.remember);
            ui.text_area("note", &mut self.note, 3);
            ui.row(34.0, |ui| {
                if ui.primary_button("Quit") {
                    ui.quit();
                }
                if ui.button("Crash") {
                    self.confirm_crash = true;
                }
                if ui.button("Hang") {
                    eprintln!("lucina-app-hello: hanging on purpose");
                    loop {
                        std::thread::sleep(Duration::from_secs(3600));
                    }
                }
            });
        });

        if self.confirm_crash {
            ui.modal(420.0, 170.0, |ui| {
                ui.heading("Crash on purpose?");
                ui.paragraph("The shell must take the screen back and offer to reopen.");
                ui.row(34.0, |ui| {
                    if ui.button("Panic") {
                        panic!("deliberate panic from the Crash button");
                    }
                    if ui.button("Segfault") {
                        eprintln!("lucina-app-hello: writing to address 0 on purpose");
                        segfault();
                    }
                    if ui.button("Cancel") {
                        self.confirm_crash = false;
                    }
                });
            });
            if ui.key(keys::KEY_ESC) {
                self.confirm_crash = false;
            }
        }
    }
}

fn main() {
    let mut args = std::env::args().skip(1);
    if args.next().as_deref() == Some("--render") {
        // On the host, without a device: what the app looks like, as a PPM file.
        let path = args.next().unwrap_or_else(|| "hello.ppm".into());
        let mut h = Harness::new(Hello::new(None), 1920, 1080);
        h.type_str("cof");
        std::fs::write(path, h.ppm()).expect("write");
        return;
    }
    lucina_app::run("lucina-app-hello", |tx| Hello::new(Some(tx)));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn app() -> Harness<Hello> {
        Harness::new(Hello::new(None), 1920, 1080)
    }

    #[test]
    fn typing_filters_the_list_and_only_redraws_what_changed() {
        let mut h = app();
        assert_eq!(h.app().shown.len(), 2000);
        h.type_str("coffee");
        assert_eq!(h.app().filter.text, "coffee");
        assert_eq!(h.app().shown.len(), 400);
        let dirty = h.key(keys::KEY_BACKSPACE);
        assert_eq!(h.app().filter.text, "coffe");
        let px: i32 = dirty.iter().map(|r| r.w * r.h).sum();
        assert!(px < 1920 * 1080 / 2, "{px} pixels for one key");
    }

    #[test]
    fn tab_and_arrows_walk_the_focus_and_enter_opens_a_row() {
        let mut h = app();
        h.key(keys::KEY_DOWN); // From the filter to the list (the filter does not use Down).
        h.key(keys::KEY_DOWN); // The first row.
        h.key(keys::KEY_DOWN);
        assert_eq!(h.app().list.selected, Some(1));
        h.key(keys::KEY_ENTER);
        assert_eq!(h.app().last_opened.as_deref(), Some("Row 2: coffee"));
        h.key(keys::KEY_END);
        assert_eq!(h.app().list.selected, Some(1999));
        assert!(h.app().list.scroll > 0);
    }

    #[test]
    fn the_pointer_reaches_rows_and_the_wheel_scrolls() {
        let mut h = app();
        // The list starts under the bar, the padding and the filter field.
        h.click(200, 300);
        assert!(h.app().last_opened.is_some());
        assert_eq!(h.app().list.scroll, 0);
        h.wheel(-3);
        let scrolled = h.app().list.scroll;
        assert!(scrolled > 0);
        h.wheel(3);
        assert_eq!(h.app().list.scroll, 0);
    }

    #[test]
    fn a_dialog_takes_all_input() {
        let mut h = app();
        h.app().confirm_crash = true;
        h.core.input(lucina_app::Input::Wheel(0));
        h.core.paint();
        let scroll = h.app().list.scroll;
        // A click on the list behind it does nothing.
        h.click(200, 300);
        assert!(h.app().last_opened.is_none());
        assert_eq!(h.app().list.scroll, scroll);
        h.key(keys::KEY_ESC);
        assert!(!h.app().confirm_crash);
        h.click(200, 300);
        assert!(h.app().last_opened.is_some());
    }

    #[test]
    fn the_logo_decodes() {
        assert!(Hello::new(None).logo.is_some());
    }
}
