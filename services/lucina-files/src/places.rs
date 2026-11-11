//! Where things are kept. `/data` is the laptop's persistent partition, but it comes up in the background and may fail, so every
//! path has an answer for a boot without it: memory, gone at a restart.

use std::path::{Path, PathBuf};

const DATA_READY: &str = "/run/lucina/data-ready";
const DATA_HOME: &str = "/data/home";
const DATA_CACHE: &str = "/data/cache";
const RAM_HOME: &str = "/run/lucina/files";
const RAM_CACHE: &str = "/run/lucina/cache";
/// Tests and test images: a folder standing in for `/data`.
const DATA_ENV: &str = "LUCINA_FILES_ROOT";

fn stand_in() -> Option<PathBuf> {
    std::env::var_os(DATA_ENV).map(PathBuf::from)
}

/// Whether this boot's files outlast a restart.
pub fn persistent() -> bool {
    stand_in().is_some() || Path::new(DATA_READY).exists()
}

/// The owner's files: `$HOME` on `/data`, or a folder in memory.
pub fn home() -> PathBuf {
    if let Some(root) = stand_in() {
        return root.join("home");
    }
    PathBuf::from(if persistent() { DATA_HOME } else { RAM_HOME })
}

/// An app's cache folder, private to the owner, which it may empty at any time. Made here.
pub fn cache_dir(app: &str) -> Option<PathBuf> {
    use std::os::unix::fs::DirBuilderExt;
    let base = match stand_in() {
        Some(root) => root.join("cache"),
        None => PathBuf::from(if persistent() { DATA_CACHE } else { RAM_CACHE }),
    };
    let dir = base.join(app);
    std::fs::DirBuilder::new().recursive(true).mode(0o700).create(&dir).ok()?;
    Some(dir)
}

/// A place the picker lists on its left.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Place {
    pub name: String,
    pub path: PathBuf,
}

impl Place {
    pub fn new(name: &str, path: impl Into<PathBuf>) -> Place {
        Place { name: name.into(), path: path.into() }
    }
}

/// The folders every picker offers.
pub fn standard() -> Vec<Place> {
    let home = home();
    vec![
        Place::new("Home", &home),
        Place::new("Downloads", home.join("Downloads")),
        Place::new("Pictures", home.join("Pictures")),
        Place::new("Documents", home.join("Documents")),
    ]
}
