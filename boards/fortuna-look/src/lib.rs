//! Fortuna in the Lucina look, fitted to the DSi's two 256x192 RGB555 screens: the house palette and the Glass concept's
//! materials (generated, `tokens`), DejaVu and Lato Light faces (`text`, glyphs generated in `font_gen`), icons
//! (`icons`, generated in `icon_gen`), corner masks (`corner`), a dithering software canvas (`canvas`), the look's
//! roles (`skin`), where things are and how the d-pad moves (`layout`), and the screens (`screens`, `browse`).
//! no_std, no I/O: the kernel and the host previews draw the same pixels.
#![cfg_attr(not(test), no_std)]

pub mod agents;
pub mod browse;
pub mod canvas;
pub mod chat;
pub mod compose;
pub mod corner;
pub mod font_gen;
pub mod icon_gen;
pub mod icons;
pub mod layout;
pub mod pair;
pub mod ring_gen;
/// The layout and dirty-rectangle rules Verus checks in the shipped file.
#[allow(clippy::implicit_saturating_sub)] // the verified file keeps the if form Verus checks
pub mod rules;
#[cfg(any(test, feature = "sample"))]
pub mod sample;
pub mod screens;
pub mod skin;
pub mod talk;
pub mod text;
pub mod tokens;
