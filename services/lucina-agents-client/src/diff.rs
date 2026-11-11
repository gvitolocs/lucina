//! The diff a permission request carries, and the line diff that shows it: ACP agents (`oc-cli acp` and the others)
//! send the change as a `{"type":"diff", path, oldText, newText}` block in the tool call's `content`, Claude's edit tool
//! input is `{file_path, old_string, new_string}`, and agentd's own `ToolCallDetail::Edit` carries the same three fields.
//! Pure, so all of it is tested without a screen.

use lucina_agents::permission::PermissionRequest;
use lucina_agents::timeline::ToolCallDetail;
use serde_json::{Map, Value};

use crate::api::{B, enc};
use crate::model::DiffRow;

/// The change a permission request asks about: the file and its text before and after.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PendingDiff {
    pub path: String,
    pub old_text: String,
    pub new_text: String,
}

/// The diff of a permission request, from either shape: the ACP `diff` content block in the tool call's content, or
/// the Claude edit tool input (`file_path`, `old_string`, `new_string`), or agentd's `Edit` detail. `None` when the
/// request carries no change (a shell command, a question, …).
pub fn diff_from_request(req: &PermissionRequest) -> Option<PendingDiff> {
    if let Some(ToolCallDetail::Edit {
        file_path,
        old_string,
        new_string,
        ..
    }) = &req.detail
    {
        if !file_path.is_empty() && (old_string.is_some() || new_string.is_some()) {
            return Some(PendingDiff {
                path: file_path.clone(),
                old_text: old_string.clone().unwrap_or_default(),
                new_text: new_string.clone().unwrap_or_default(),
            });
        }
    }
    if let Some(input) = &req.input {
        if let Some(d) = diff_from_map(input) {
            return Some(d);
        }
    }
    if let Some(ToolCallDetail::Unknown { input, .. }) = &req.detail {
        if let Some(d) = diff_from_value(input) {
            return Some(d);
        }
    }
    None
}

/// The diff in one JSON object: the tool call's `content` array with a `diff` block, else the edit input's fields.
fn diff_from_map(m: &Map<String, Value>) -> Option<PendingDiff> {
    diff_from_value(&Value::Object(m.clone()))
}

fn diff_from_value(v: &Value) -> Option<PendingDiff> {
    // ACP: `{"type":"diff", path, oldText, newText}` in the tool call's content array.
    if let Some(items) = v.get("content").and_then(Value::as_array) {
        if let Some(d) = items.iter().find(|i| i["type"] == "diff") {
            let path = d["path"].as_str().unwrap_or_default();
            if !path.is_empty() {
                return Some(PendingDiff {
                    path: path.to_owned(),
                    old_text: d["oldText"].as_str().unwrap_or_default().to_owned(),
                    new_text: d["newText"].as_str().unwrap_or_default().to_owned(),
                });
            }
        }
    }
    // Claude's edit tool input: `file_path`, `old_string`, `new_string`.
    let path = v.get("file_path").and_then(Value::as_str)?;
    if path.is_empty() || (!v.get("old_string").is_some() && !v.get("new_string").is_some()) {
        return None;
    }
    Some(PendingDiff {
        path: path.to_owned(),
        old_text: v["old_string"].as_str().unwrap_or_default().to_owned(),
        new_text: v["new_string"].as_str().unwrap_or_default().to_owned(),
    })
}

/// The largest LCS table computed: past it the diff falls back to "everything changed".
const LCS_MAX_CELLS: usize = 1_000_000;

/// A line diff of `old` to `new`, as `model::diff_rows`-style rows: `remove` rows carry the old line number, `add`
/// and `context` rows the new one. A small LCS over the lines; past [`LCS_MAX_CELLS`] cells it is all removes then all
/// adds. An empty `old` (a new file) is all adds.
pub fn line_diff(old: &str, new: &str) -> Vec<DiffRow> {
    let a: Vec<&str> = old.lines().collect();
    let b: Vec<&str> = new.lines().collect();
    if a.len().saturating_mul(b.len()) > LCS_MAX_CELLS {
        let mut out = Vec::with_capacity(a.len() + b.len());
        for (i, l) in a.iter().enumerate() {
            out.push(DiffRow {
                kind: "remove".into(),
                n: (i + 1).to_string(),
                content: (*l).to_owned(),
            });
        }
        for (i, l) in b.iter().enumerate() {
            out.push(DiffRow {
                kind: "add".into(),
                n: (i + 1).to_string(),
                content: (*l).to_owned(),
            });
        }
        return out;
    }
    // lcs[i][j]: the LCS length of a[i..] and b[j..].
    let (n, m) = (a.len(), b.len());
    let mut lcs = vec![0u32; (n + 1) * (m + 1)];
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            lcs[i * (m + 1) + j] = if a[i] == b[j] {
                lcs[(i + 1) * (m + 1) + (j + 1)] + 1
            } else {
                lcs[(i + 1) * (m + 1) + j].max(lcs[i * (m + 1) + (j + 1)])
            };
        }
    }
    let mut out = Vec::new();
    let (mut i, mut j) = (0, 0);
    while i < n && j < m {
        if a[i] == b[j] {
            out.push(DiffRow {
                kind: "context".into(),
                n: (j + 1).to_string(),
                content: a[i].to_owned(),
            });
            i += 1;
            j += 1;
        } else if lcs[(i + 1) * (m + 1) + j] >= lcs[i * (m + 1) + (j + 1)] {
            out.push(DiffRow {
                kind: "remove".into(),
                n: (i + 1).to_string(),
                content: a[i].to_owned(),
            });
            i += 1;
        } else {
            out.push(DiffRow {
                kind: "add".into(),
                n: (j + 1).to_string(),
                content: b[j].to_owned(),
            });
            j += 1;
        }
    }
    while i < n {
        out.push(DiffRow {
            kind: "remove".into(),
            n: (i + 1).to_string(),
            content: a[i].to_owned(),
        });
        i += 1;
    }
    while j < m {
        out.push(DiffRow {
            kind: "add".into(),
            n: (j + 1).to_string(),
            content: b[j].to_owned(),
        });
        j += 1;
    }
    out
}

/// The diff modes of the Changes tab, as the server's `mode=` takes them.
pub const DIFF_MODES: [&str; 3] = ["all", "uncommitted", "base"];

/// `GET /v1/agents/workspaces/{ws}/diff?mode=…`, the Changes tab's request.
pub fn diff_path(ws: &str, mode: usize, ignore_ws: bool) -> String {
    let mode = DIFF_MODES.get(mode).copied().unwrap_or("all");
    let ignore = if ignore_ws {
        "&ignoreWhitespace=true"
    } else {
        ""
    };
    format!("{B}/workspaces/{}/diff?mode={mode}{ignore}", enc(ws))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn request(input: Value, detail: Value) -> PermissionRequest {
        serde_json::from_value(json!({
            "id": "p1", "provider": "oc-cli", "name": "edit", "kind": "tool",
            "input": input, "detail": detail,
        }))
        .unwrap()
    }

    #[test]
    fn the_acp_diff_content_block_is_extracted() {
        let req = request(
            json!({"content": [
                {"type": "text", "text": "editing"},
                {"type": "diff", "path": "/w/src/a.rs", "oldText": "a\nb\n", "newText": "a\nc\n"},
            ]}),
            json!({"type": "unknown", "input": {}, "output": null}),
        );
        assert_eq!(
            diff_from_request(&req),
            Some(PendingDiff {
                path: "/w/src/a.rs".into(),
                old_text: "a\nb\n".into(),
                new_text: "a\nc\n".into(),
            })
        );
    }

    #[test]
    fn the_acp_diff_content_in_the_unknown_detail_input_is_extracted() {
        let req = request(
            json!({}),
            json!({"type": "unknown", "input": {"content": [{"type": "diff", "path": "/w/b.rs", "oldText": null, "newText": "new file\n"}]}, "output": null}),
        );
        assert_eq!(
            diff_from_request(&req),
            Some(PendingDiff {
                path: "/w/b.rs".into(),
                old_text: String::new(),
                new_text: "new file\n".into()
            })
        );
    }

    #[test]
    fn the_claude_edit_tool_input_is_extracted() {
        let req = request(
            json!({"file_path": "/w/src/a.rs", "old_string": "let x = 1;", "new_string": "let x = 2;"}),
            json!({"type": "unknown", "input": {}, "output": null}),
        );
        assert_eq!(
            diff_from_request(&req),
            Some(PendingDiff {
                path: "/w/src/a.rs".into(),
                old_text: "let x = 1;".into(),
                new_text: "let x = 2;".into(),
            })
        );
    }

    #[test]
    fn agentds_edit_detail_is_extracted() {
        let req = request(
            json!({}),
            json!({"type": "edit", "filePath": "/w/src/a.rs", "oldString": "a", "newString": "b"}),
        );
        assert_eq!(
            diff_from_request(&req),
            Some(PendingDiff {
                path: "/w/src/a.rs".into(),
                old_text: "a".into(),
                new_text: "b".into()
            })
        );
        // A shell request carries no diff.
        let req = request(
            json!({"command": "ls"}),
            json!({"type": "shell", "command": "ls"}),
        );
        assert_eq!(diff_from_request(&req), None);
    }

    #[test]
    fn line_diff_insert_delete_change_and_new_file() {
        // A change: one remove, one add, numbered from the old and the new text.
        let rows = line_diff("a\nb\nc", "a\nx\nc");
        let kinds: Vec<(&str, &str, &str)> = rows
            .iter()
            .map(|r| (r.kind.as_str(), r.n.as_str(), r.content.as_str()))
            .collect();
        assert_eq!(
            kinds,
            [
                ("context", "1", "a"),
                ("remove", "2", "b"),
                ("add", "2", "x"),
                ("context", "3", "c")
            ]
        );
        // An insert: only adds, numbered in the new text.
        let rows = line_diff("a\nc", "a\nb\nc");
        assert_eq!(
            rows.iter()
                .map(|r| (r.kind.as_str(), r.n.as_str(), r.content.as_str()))
                .collect::<Vec<_>>(),
            [
                ("context", "1", "a"),
                ("add", "2", "b"),
                ("context", "3", "c")
            ]
        );
        // A delete: only removes, numbered in the old text.
        let rows = line_diff("a\nb\nc", "a\nc");
        assert_eq!(
            rows.iter()
                .map(|r| (r.kind.as_str(), r.n.as_str(), r.content.as_str()))
                .collect::<Vec<_>>(),
            [
                ("context", "1", "a"),
                ("remove", "2", "b"),
                ("context", "2", "c")
            ]
        );
        // An empty old text (a new file): all adds from 1.
        let rows = line_diff("", "one\ntwo");
        assert_eq!(
            rows.iter()
                .map(|r| (r.kind.as_str(), r.n.as_str(), r.content.as_str()))
                .collect::<Vec<_>>(),
            [("add", "1", "one"), ("add", "2", "two")]
        );
        // Both empty: nothing.
        assert!(line_diff("", "").is_empty());
        // A trailing newline does not make a phantom line.
        assert_eq!(line_diff("a\n", "a\n").len(), 1);
    }

    #[test]
    fn a_huge_diff_falls_back_to_everything_changed() {
        let old = "x\n".repeat(2000);
        let new = "y\n".repeat(2000);
        let rows = line_diff(&old, &new);
        assert_eq!(rows.len(), 4000);
        assert!(rows[..2000].iter().all(|r| r.kind == "remove"));
        assert!(rows[2000..].iter().all(|r| r.kind == "add"));
    }

    #[test]
    fn the_changes_request_path_has_the_mode_and_ignore_whitespace() {
        assert_eq!(
            diff_path("wks 1", 0, false),
            format!("{B}/workspaces/wks%201/diff?mode=all")
        );
        assert_eq!(
            diff_path("wks1", 2, true),
            format!("{B}/workspaces/wks1/diff?mode=base&ignoreWhitespace=true")
        );
        assert_eq!(
            diff_path("wks1", 9, false),
            format!("{B}/workspaces/wks1/diff?mode=all")
        );
    }
}
