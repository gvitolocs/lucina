//! lucina-app: the toolkit every Juno app is built on, and what `lucina-shell` draws with.
//!
//! An app is one static program: its state, implementing [`App`], and a `main` that calls [`run`].
//! See `docs/research/juno-apps.md` and the template `apps/lucina-app-hello`.
//!
//! - [`ui`]: widgets, in immediate mode: labels, buttons, check boxes, text fields, lists, dialogs.
//! - [`edit`]: what a text field holds: text, cursor, selection.
//! - [`app`]: the loop: screen, input, redraw, messages from other threads, and a [`Harness`] for tests.
//! - [`camera_mark`]: who captures from the webcam, from `/proc/lucina-camera`, for the "camera on" mark.
//! - [`image`]: JPEG, PNG and WebP pictures.
//! - [`icons`]: the apps' icons, one mapped pack for every program.
//! - [`time`]: local time from `/etc/lucina/timezone`.
//! - [`manifest`]: the description of an app that the home screen reads.
//! - [`screen`]: the framebuffer, with the GPU showing large changes where the kernel offers one.
//! - [`dirty`], [`fb`], [`gfx`], [`gpu`], [`input`], [`keys`]: the layers below.
//! - [`prof`]: where a frame's time goes, for `view_bench --parts`; inert without the `prof` feature.

pub mod app;
pub mod camera_mark;
pub mod dirty;
pub mod edit;
pub mod fb;
pub mod gfx;
pub mod gpu;
pub mod hold;
pub mod icons;
pub mod image;
pub mod input;
pub mod input_watch;
pub mod intent;
pub mod keys;
pub mod manifest;
pub mod prof;
pub mod program;
pub mod screen;
pub mod theme;
pub mod time;
pub mod ui;

pub use app::{App, Harness, Load, Sender, run};
pub use edit::TextEdit;
pub use gfx::{Face, Rect, Rgb};
pub use image::Image;
/// What an app tells Lucina happened (`events::emit("download.done")...send()`), and the present report.
pub use lucina_events as events;
pub use manifest::Manifest;
pub use ui::{Hit, Id, Input, ListResponse, ListState, TextResponse, Theme, Ui};
