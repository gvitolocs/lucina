//! For lucina-devd's `ui_tree` and `ui_act`: which program is in front, and its tree. The shell answers on a fixed socket and
//! says which app is in front; that app answers on `/run/lucina/ax/<pid>.sock`. A program with no socket has no tree, and the
//! answer says so, so that Lucina falls back to a screenshot.

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::{Value, json};

use crate::proto::{self, DIR};

/// The shell's socket.
pub const SHELL: &str = "/run/lucina/ax/shell.sock";

const TIMEOUT: Duration = Duration::from_secs(3);

/// Where the program in front answers, and its name: the app's socket when one is in front and has one, else the shell's.
/// `Err` when the app in front publishes no tree.
pub fn front(dir: &Path) -> Result<(PathBuf, String), String> {
    let shell = dir.join("shell.sock");
    let reply = proto::request(&shell, &json!({"op": "tree"}), TIMEOUT).map_err(|e| format!("the shell's tree: {e}"))?;
    match reply.get("front") {
        Some(Value::Object(f)) => {
            let app = f.get("app").and_then(Value::as_str).unwrap_or("app").to_owned();
            let Some(pid) = f.get("pid").and_then(Value::as_u64) else {
                return Err(format!("{app} is in front, and the shell does not know its pid"));
            };
            let sock = dir.join(format!("{pid}.sock"));
            if sock.exists() {
                Ok((sock, app))
            } else {
                Err(format!("{app} is in front and publishes no tree: use screenshot and click"))
            }
        }
        _ => Ok((shell, "shell".into())),
    }
}

/// `ui_tree`: the tree of the program in front (or of `target`: "shell", or an app's pid).
pub fn tree(target: Option<&str>) -> Value {
    tree_in(Path::new(DIR), target)
}

pub fn tree_in(dir: &Path, target: Option<&str>) -> Value {
    match socket_for(dir, target) {
        Ok(sock) => proto::request(&sock, &json!({"op": "tree"}), TIMEOUT)
            .unwrap_or_else(|e| proto::refusal("unreachable", &format!("{}: {e}", sock.display()), None)),
        Err(why) => proto::refusal("no_tree", &why, None),
    }
}

/// `ui_act`: one action on a node of the program in front (or of `target`).
pub fn act(target: Option<&str>, id: &str, action: &str, value: Option<&str>, version: &str) -> Value {
    act_in(Path::new(DIR), target, id, action, value, version)
}

pub fn act_in(dir: &Path, target: Option<&str>, id: &str, action: &str, value: Option<&str>, version: &str) -> Value {
    let req = json!({"op": "act", "id": id, "action": action, "value": value, "version": version, "source": "lucina"});
    match socket_for(dir, target) {
        Ok(sock) => proto::request(&sock, &req, TIMEOUT)
            .unwrap_or_else(|e| proto::refusal("unreachable", &format!("{}: {e}", sock.display()), None)),
        Err(why) => proto::refusal("no_tree", &why, None),
    }
}

fn socket_for(dir: &Path, target: Option<&str>) -> Result<PathBuf, String> {
    match target {
        None | Some("front") => front(dir).map(|(s, _)| s),
        Some("shell") => Ok(dir.join("shell.sock")),
        Some(pid) if pid.bytes().all(|b| b.is_ascii_digit()) && !pid.is_empty() => {
            let s = dir.join(format!("{pid}.sock"));
            if s.exists() { Ok(s) } else { Err(format!("process {pid} publishes no tree")) }
        }
        Some(other) => Err(format!("target {other:?}: front, shell or a pid")),
    }
}
