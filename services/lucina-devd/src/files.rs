//! `list_dir`, `read_file`, `write_file`, `delete_file`: anywhere the user reaches but the refused places ([`crate::places`]).
use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};

use serde_json::{Value, json};

use crate::places::{Places, plain, temp_name};

pub const MAX_READ: u64 = 256 << 10;
pub const MAX_WRITE: usize = 4 << 20;
pub const MAX_ENTRIES: usize = 500;

fn kind(ft: std::fs::FileType) -> &'static str {
    if ft.is_symlink() {
        "link"
    } else if ft.is_dir() {
        "folder"
    } else if ft.is_file() {
        "file"
    } else {
        "other"
    }
}

pub fn list_dir(places: &Places, path: &str) -> Result<Value, String> {
    let dir = places.listing(path)?;
    let opened = File::open(&dir).map_err(|e| format!("{path}: {}", plain(&e)))?;
    places.still_inside(&opened, &places.anywhere())?;
    if !opened.metadata().map_err(|e| plain(&e))?.is_dir() {
        return Err(format!("{path}: not a folder"));
    }
    let mut entries = Vec::new();
    let mut total = 0usize;
    for e in std::fs::read_dir(&dir)
        .map_err(|e| format!("{path}: {}", plain(&e)))?
        .flatten()
    {
        total += 1;
        if entries.len() >= MAX_ENTRIES {
            continue;
        }
        let Ok(meta) = e.path().symlink_metadata() else {
            continue;
        };
        entries.push(json!({
            "name": e.file_name().to_string_lossy(),
            "kind": kind(meta.file_type()),
            "size": meta.len(),
            "mtime": meta.mtime(),
        }));
    }
    entries.sort_by(|a, b| a["name"].as_str().cmp(&b["name"].as_str()));
    let shown = entries.len();
    let mut text = format!("{}: {total} entries", dir.display());
    if shown < total {
        text += &format!(", the first {shown} shown");
    }
    for e in &entries {
        let (name, k) = (
            e["name"].as_str().unwrap_or(""),
            e["kind"].as_str().unwrap_or(""),
        );
        text += &match k {
            "folder" => format!("\n{name}/"),
            _ => format!("\n{name} ({k}, {} bytes)", e["size"]),
        };
    }
    Ok(
        json!({"text": text, "path": dir.display().to_string(), "entries": entries, "total": total, "truncated": shown < total}),
    )
}

pub fn read_file(
    places: &Places,
    path: &str,
    offset: u64,
    max_bytes: u64,
) -> Result<Value, String> {
    let file_path = places.existing(path)?;
    let mut f = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(&file_path)
        .map_err(|e| format!("{path}: {}", plain(&e)))?;
    places.still_inside(&f, &places.anywhere())?;
    let meta = f.metadata().map_err(|e| plain(&e))?;
    if !meta.is_file() {
        return Err(format!("{path}: not a file"));
    }
    let size = meta.len();
    let want = max_bytes.clamp(1, MAX_READ);
    f.seek(SeekFrom::Start(offset)).map_err(|e| plain(&e))?;
    let mut bytes = Vec::new();
    f.take(want)
        .read_to_end(&mut bytes)
        .map_err(|e| format!("{path}: {}", plain(&e)))?;
    let end = offset.saturating_add(bytes.len() as u64);
    let more = end < size;
    let tail = if more {
        format!(", more from offset {end}")
    } else {
        String::new()
    };
    let name = file_path.display().to_string();
    match String::from_utf8(bytes) {
        Ok(text) => Ok(json!({
            "text": format!("{name} ({size} bytes; {}..{end}{tail}):\n{text}", offset.min(size)),
            "path": name, "size": size, "offset": offset, "bytes": end.saturating_sub(offset), "more": more, "encoding": "utf-8", "content": text,
        })),
        Err(e) => {
            let bytes = e.into_bytes();
            Ok(json!({
                "text": format!("{name} ({size} bytes; {}..{end}{tail}): not text, given as base64 in `base64`", offset.min(size)),
                "path": name, "size": size, "offset": offset, "bytes": bytes.len(), "more": more, "encoding": "base64", "base64": crate::b64::encode(&bytes),
            }))
        }
    }
}

/// Created as a new name beside it, flushed to the disk, then renamed over: a reader sees the old file or the new one.
pub fn write_file(
    places: &Places,
    path: &str,
    bytes: &[u8],
    overwrite: bool,
) -> Result<Value, String> {
    if bytes.len() > MAX_WRITE {
        return Err(format!("at most {} MiB per write", MAX_WRITE >> 20));
    }
    let target = places.entry(path)?;
    let parent = target.parent().ok_or("no folder")?.to_path_buf();
    let name = target.file_name().ok_or("no name")?.to_os_string();
    let existed = match target.symlink_metadata() {
        Ok(m) if m.file_type().is_symlink() => {
            return Err(format!("{path}: a link; write to what it names instead"));
        }
        Ok(m) if !m.is_file() => return Err(format!("{path}: not a file")),
        Ok(_) if !overwrite => {
            return Err(format!(
                "{path}: already there; replacing it needs overwrite: true"
            ));
        }
        Ok(_) => true,
        Err(_) => false,
    };
    let dir = File::open(&parent).map_err(|e| format!("{}: {}", parent.display(), plain(&e)))?;
    places.still_inside(&dir, &places.anywhere())?;
    let tmp = parent.join(temp_name(&name));
    let _ = std::fs::remove_file(&tmp);
    let written = (|| -> std::io::Result<()> {
        let mut f = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o644)
            .custom_flags(libc::O_NOFOLLOW)
            .open(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()?;
        std::fs::rename(&tmp, &target)?;
        dir.sync_all()
    })();
    if let Err(e) = written {
        let _ = std::fs::remove_file(&tmp);
        return Err(format!("{path}: {}", plain(&e)));
    }
    let what = if existed { "replaced" } else { "created" };
    Ok(
        json!({"text": format!("{} {what}, {} bytes", target.display(), bytes.len()), "path": target.display().to_string(), "bytes": bytes.len(), "replaced": existed}),
    )
}

pub fn delete_file(places: &Places, path: &str) -> Result<Value, String> {
    let target = places.entry(path)?;
    let meta = target
        .symlink_metadata()
        .map_err(|e| format!("{path}: {}", plain(&e)))?;
    let what = if meta.is_dir() {
        std::fs::remove_dir(&target).map_err(|e| match e.raw_os_error() {
            Some(libc::ENOTEMPTY) | Some(libc::EEXIST) => {
                format!("{path}: the folder is not empty")
            }
            _ => format!("{path}: {}", plain(&e)),
        })?;
        "folder"
    } else {
        std::fs::remove_file(&target).map_err(|e| format!("{path}: {}", plain(&e)))?;
        if meta.file_type().is_symlink() {
            "link"
        } else {
            "file"
        }
    };
    Ok(
        json!({"text": format!("{} removed ({what})", target.display()), "path": target.display().to_string()}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::places::tests::machine;
    use std::os::unix::fs::symlink;

    #[test]
    fn a_folder_lists_its_entries_with_kind_size_and_time() {
        let (root, places) = machine("list");
        std::fs::create_dir(root.join("data/home/Docs/sub")).unwrap();
        let v = list_dir(&places, "Docs").unwrap();
        assert_eq!(v["total"], 2);
        assert_eq!(v["entries"][0]["name"], "a.txt");
        assert_eq!(v["entries"][0]["kind"], "file");
        assert_eq!(v["entries"][0]["size"], 5);
        assert!(v["entries"][0]["mtime"].as_i64().unwrap() > 0);
        assert_eq!(v["entries"][1]["kind"], "folder");
        assert!(v["text"].as_str().unwrap().contains("sub/"));
        assert!(
            list_dir(&places, "Docs/a.txt")
                .unwrap_err()
                .contains("not a folder")
        );
    }

    #[test]
    fn a_long_folder_is_cut_at_500() {
        let (root, places) = machine("many");
        for i in 0..520 {
            std::fs::write(root.join(format!("data/home/Docs/f{i:04}")), b"").unwrap();
        }
        let v = list_dir(&places, "Docs").unwrap();
        assert_eq!(v["entries"].as_array().unwrap().len(), MAX_ENTRIES);
        assert_eq!(v["total"], 521);
        assert_eq!(v["truncated"], true);
    }

    #[test]
    fn text_comes_as_text_and_bytes_as_base64_in_pieces() {
        let (root, places) = machine("read");
        let v = read_file(&places, "Docs/a.txt", 0, MAX_READ).unwrap();
        assert_eq!(v["content"], "hello");
        assert_eq!(v["more"], false);
        let v = read_file(&places, "Docs/a.txt", 1, 2).unwrap();
        assert_eq!(v["content"], "el");
        assert_eq!(v["more"], true);
        assert!(v["text"].as_str().unwrap().contains("more from offset 3"));
        std::fs::write(root.join("data/home/b.bin"), [0xff, 0x00, 0x10]).unwrap();
        let v = read_file(&places, "b.bin", 0, 10).unwrap();
        assert_eq!(v["encoding"], "base64");
        assert_eq!(v["base64"], "/wAQ");
        let v = read_file(&places, "Docs/a.txt", 100, 10).unwrap();
        assert_eq!(v["bytes"], 0);
    }

    #[test]
    fn a_read_is_at_most_256_kib() {
        let (root, places) = machine("big");
        std::fs::write(root.join("data/home/big.txt"), vec![b'a'; 300 << 10]).unwrap();
        let v = read_file(&places, "big.txt", 0, 10 << 20).unwrap();
        assert_eq!(v["bytes"], MAX_READ);
        assert_eq!(v["more"], true);
    }

    #[test]
    fn reading_through_a_link_out_of_the_home_or_the_sign_in_is_refused() {
        let (root, places) = machine("readdeny");
        symlink(
            root.join("data/lucina/secretary/credential"),
            root.join("data/home/p"),
        )
        .unwrap();
        assert!(
            read_file(&places, "p", 0, 10)
                .unwrap_err()
                .contains("refused")
        );
        let cred = root
            .join("data/lucina/secretary/credential")
            .display()
            .to_string();
        assert!(
            read_file(&places, &cred, 0, 10)
                .unwrap_err()
                .contains("refused")
        );
    }

    #[test]
    fn a_write_creates_and_replaces_only_when_asked() {
        let (root, places) = machine("write");
        let v = write_file(&places, "Docs/new.txt", b"one", false).unwrap();
        assert_eq!(v["replaced"], false);
        assert_eq!(
            std::fs::read(root.join("data/home/Docs/new.txt")).unwrap(),
            b"one"
        );
        let e = write_file(&places, "Docs/new.txt", b"two", false).unwrap_err();
        assert!(e.contains("overwrite: true"), "{e}");
        assert_eq!(
            write_file(&places, "Docs/new.txt", b"two", true).unwrap()["replaced"],
            true
        );
        assert_eq!(
            std::fs::read(root.join("data/home/Docs/new.txt")).unwrap(),
            b"two"
        );
        // No temporary file is left behind.
        let names: Vec<String> = std::fs::read_dir(root.join("data/home/Docs"))
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        assert!(names.iter().all(|n| !n.contains(".devd-")), "{names:?}");
    }

    #[test]
    fn a_write_never_goes_through_a_link_or_into_a_refused_place() {
        let (root, places) = machine("writedeny");
        symlink(root.join("etc/passwd"), root.join("data/home/p")).unwrap();
        assert!(
            write_file(&places, "p", b"x", true)
                .unwrap_err()
                .contains("a link")
        );
        assert_eq!(std::fs::read(root.join("etc/passwd")).unwrap(), b"root");
        // Out of the home is the user's to write; a link to the firmware is refused like the firmware.
        symlink(root.join("etc"), root.join("data/home/etc")).unwrap();
        write_file(&places, "etc/new", b"x", false).unwrap();
        assert_eq!(std::fs::read(root.join("etc/new")).unwrap(), b"x");
        std::fs::create_dir_all(root.join("lib/firmware/qcom")).unwrap();
        symlink(root.join("lib/firmware/qcom"), root.join("data/home/fw")).unwrap();
        assert!(
            write_file(&places, "fw/evil", b"x", false)
                .unwrap_err()
                .contains("off-limits")
        );
        assert!(!root.join("lib/firmware/qcom/evil").exists());
        let devd = root
            .join("data/lucina/devd/audit.jsonl")
            .display()
            .to_string();
        assert!(
            write_file(&places, &devd, b"x", true)
                .unwrap_err()
                .contains("refused")
        );
        assert!(
            write_file(&places, "Docs", b"x", true)
                .unwrap_err()
                .contains("not a file")
        );
        assert!(write_file(&places, "missing/x", b"x", false).is_err());
        assert!(write_file(&places, "x", &vec![0; MAX_WRITE + 1], false).is_err());
    }

    #[test]
    fn delete_takes_a_file_a_link_or_an_empty_folder() {
        let (root, places) = machine("delete");
        let home = root.join("data/home");
        std::fs::create_dir(home.join("empty")).unwrap();
        symlink(root.join("etc/passwd"), home.join("p")).unwrap();
        assert!(
            delete_file(&places, "Docs")
                .unwrap_err()
                .contains("not empty")
        );
        assert!(delete_file(&places, "Docs/a.txt").is_ok());
        assert!(delete_file(&places, "empty").is_ok());
        assert!(
            delete_file(&places, "p").unwrap()["text"]
                .as_str()
                .unwrap()
                .contains("link")
        );
        assert!(root.join("etc/passwd").exists());
        assert!(delete_file(&places, "Docs/a.txt").is_err());
        assert!(delete_file(&places, &home.display().to_string()).is_err());
        let cred = root
            .join("data/lucina/secretary/credential")
            .display()
            .to_string();
        assert!(delete_file(&places, &cred).unwrap_err().contains("refused"));
        assert!(root.join("data/lucina/secretary/credential").exists());
    }
}
