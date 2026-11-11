//! The shell's control socket (`apps/lucina-shell/src/control.rs`): one JSON line out, one back.
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::time::Duration;

use serde_json::{Value, json};

pub const SOCKET: &str = "/run/lucina/shell.sock";

pub fn ask(socket: &Path, request: &Value) -> Result<Value, String> {
    let mut s = UnixStream::connect(socket)
        .map_err(|e| format!("the shell does not answer ({})", crate::places::plain(&e)))?;
    s.set_read_timeout(Some(Duration::from_secs(3)))
        .map_err(|e| e.to_string())?;
    s.set_write_timeout(Some(Duration::from_secs(3)))
        .map_err(|e| e.to_string())?;
    let mut line = request.to_string();
    line.push('\n');
    s.write_all(line.as_bytes())
        .map_err(|e| format!("the shell: {}", crate::places::plain(&e)))?;
    let mut reply = String::new();
    // An answer is a line of a few KiB; the list of every app fits in 1 MiB.
    BufReader::new(std::io::Read::take(s, 1 << 20))
        .read_line(&mut reply)
        .map_err(|e| format!("the shell: {}", crate::places::plain(&e)))?;
    let v: Value =
        serde_json::from_str(reply.trim()).map_err(|_| "the shell gave no answer".to_string())?;
    if v["ok"] == true {
        Ok(v)
    } else {
        Err(v["error"]
            .as_str()
            .unwrap_or("the shell refused")
            .to_owned())
    }
}

pub fn list(socket: &Path) -> Result<Value, String> {
    ask(socket, &json!({"op": "list"}))
}

/// The id of the app in front, or None on the home screen.
pub fn front(socket: &Path) -> Result<Option<String>, String> {
    Ok(list(socket)?["front"].as_str().map(str::to_owned))
}

/// The id of the app in `list` (the shell's `list` answer) whose id or title is `name`, case-insensitively and trimmed;
/// otherwise an error that lists the apps as "id (title)".
pub fn resolve(list: &Value, name: &str) -> Result<String, String> {
    let want = name.trim().to_lowercase();
    let apps = list["apps"].as_array().map(Vec::as_slice).unwrap_or(&[]);
    let field = |a: &Value, k: &str| a[k].as_str().unwrap_or("").to_owned();
    // An id wins over another app's title.
    let by = |k: &str| apps.iter().find(|a| field(a, k).to_lowercase() == want);
    if let Some(a) = by("id").or_else(|| by("title")) {
        return Ok(field(a, "id"));
    }
    let known: Vec<String> = apps
        .iter()
        .map(|a| format!("{} ({})", field(a, "id"), field(a, "title")))
        .collect();
    Err(format!(
        "there is no app {}; the apps are: {}",
        name.trim(),
        if known.is_empty() {
            "none".to_owned()
        } else {
            known.join(", ")
        }
    ))
}

/// `name` as an app's id, through the shell's list.
pub fn app_id(socket: &Path, name: &str) -> Result<String, String> {
    resolve(&list(socket)?, name)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn apps() -> Value {
        json!({"ok": true, "front": null, "apps": [
            {"id": "gallery", "title": "Gallery"},
            {"id": "chat", "title": "Lucina"},
            {"id": "video", "title": "Video"},
        ]})
    }

    #[test]
    fn an_app_is_found_by_its_id_or_its_title() {
        assert_eq!(resolve(&apps(), "Gallery").unwrap(), "gallery");
        assert_eq!(resolve(&apps(), "LUCINA").unwrap(), "chat");
        assert_eq!(resolve(&apps(), " chat ").unwrap(), "chat");
        assert_eq!(resolve(&apps(), "video").unwrap(), "video");
    }

    #[test]
    fn an_unknown_name_lists_the_apps() {
        let e = resolve(&apps(), "nope").unwrap_err();
        assert_eq!(
            e,
            "there is no app nope; the apps are: gallery (Gallery), chat (Lucina), video (Video)"
        );
    }
}
