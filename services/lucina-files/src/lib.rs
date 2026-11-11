//! What Juno's file-handling apps share (`docs/research/juno-files.md`). Behaviour comes from SecretaryIOS `web/src`; each function
//! names the web function it follows.
//!
//! - [`media`]: the media server's routes behind a trait: the real account, or [`fake`] in sample mode.
//! - [`drive`]: the Drive's wire model, `web/src/lib/drive.ts`.
//! - [`places`]: where files and caches live: `/data` when this boot has it, memory when it does not.
//! - [`cache`]: fetched pictures on disk, with a size limit and a check on read.
//! - [`pictures`]: pictures wanted on screen, fetched and decoded off the screen's thread, nearest to the screen first.
//! - [`picker`]: the file picker: open or save, on this laptop or in the Drive, for any app on the toolkit.
//! - [`engine`]: the real `lucina-media` on temporary folders, for tests.

// An item's JSON in `fake`, as long as the server's.
#![recursion_limit = "512"]

pub mod cache;
pub mod drive;
pub mod engine;
pub mod fake;
pub mod media;
pub mod picker;
pub mod pictures;
pub mod places;

#[cfg(test)]
mod tests;
