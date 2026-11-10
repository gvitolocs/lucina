//! Saving photos: the camera's own JPEGs, written as they came (with the standard Huffman tables put in, which UVC
//! streams leave out), durably.

use std::fs::{self, File};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use crate::convert::ensure_huffman_tables;

/// `IMG_20261107_143012.jpg` for the stamp `20261107_143012` (`YYYYMMDD_HHMMSS`, from the caller).
pub fn photo_name(stamp: &str) -> String {
    format!("IMG_{stamp}.jpg")
}

/// Writes `jpeg` to `<dir>/IMG_<stamp>.jpg` (or `_2`, `_3`, ... before `.jpg` if that name is taken) and returns the
/// path. The bytes go to a `.<name>.tmp` first, are `sync_all`ed, renamed, and then the directory itself is synced:
/// on ext4 a new file's directory entry needs that too, or a power cut loses the file.
pub fn save_jpeg(dir: &Path, stamp: &str, jpeg: &[u8]) -> io::Result<PathBuf> {
    fs::create_dir_all(dir)?;
    let bytes = ensure_huffman_tables(jpeg);
    let base = format!("IMG_{stamp}");
    let mut name = format!("{base}.jpg");
    let mut n = 1;
    while dir.join(&name).exists() {
        n += 1;
        name = format!("{base}_{n}.jpg");
    }
    let final_path = dir.join(&name);
    let tmp_path = dir.join(format!(".{name}.tmp"));
    {
        let mut f = File::create(&tmp_path)?;
        f.write_all(&bytes)?;
        f.sync_all()?;
    }
    fs::rename(&tmp_path, &final_path)?;
    File::open(dir)?.sync_all()?;
    Ok(final_path)
}
