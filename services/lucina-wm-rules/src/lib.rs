//! Juno's window manager rules (`docs/design/juno-window-manager.md`, section 7): everything the shell decides about spaces, gestures and layout,
//! with no I/O, no clock and no allocation in the per-frame functions, so that the phone (touchscreen) and a Mac build reuse it. Behaviour
//! comes from niri, GNOME Shell and Wayfire (`docs/research/juno-window-manager.md`); no code is taken from them.
#![no_std]

pub mod controller;
pub mod mru_rules;
pub mod overview;
pub mod rubber;
pub mod snap_rules;
pub mod spaces;
pub mod spring;
pub mod switcher;
pub mod tile;
pub mod tile_rules;
pub mod tracker;
