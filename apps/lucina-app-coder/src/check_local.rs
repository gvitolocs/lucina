//! Phase F: `--check-local <dir> <answer> <prompt…>`, a headless proof of the local Qwen chat on the device: exactly
//! what the local Agent screen runs, without a screen. `local::start_acp(dir)` (the package to RAM, the key, the route
//! check, the gateway key, the tunnel, `oc-cli acp`), then `initialize`, `session/new` in `dir` and `session/prompt`
//! with the prompt words joined by spaces, all through `crate::acp` as the screen drives it.
//!
//! One line per event on stdout, each with the seconds since start: `update <kind>` (the text of an
//! `agent_message_chunk` / `agent_thought_chunk` is never printed, only its byte count, summed on the `done` line),
//! `tool_call <title> <status>`, `tool_call_update <id> <status>`, and `permission <toolCallId> <kind> diff=<n files>`
//! with each diff's path and oldText / newText byte counts. Every permission is answered with `<answer>` (`allow` →
//! allow_once, `deny` → reject_once). At the response: `done <stopReason> <seconds> text=<n> thought=<m>`, the agent
//! and the tunnel are stopped (the `LocalAgent` dropped) and the exit code is 0. A start error or a JSON-RPC error
//! prints `error <message>` and exits 1; the 900 s limit prints `timeout` and exits 2 after stopping everything.
//! No model text, keys or tokens are printed.
//!
//! The line formats are pure ([`update_line`], [`permission_line`], [`done_line`], …), and [`run`] takes the started
//! agent, so the tests run the same code path against the fake ACP agent minus `prepare`.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde_json::Value;

use crate::acp::AcpEvent;
use crate::local::LocalAgent;

/// The limit of one run: fifteen minutes for one turn.
pub const LIMIT: Duration = Duration::from_secs(900);

const USAGE: &str =
    "usage: lucina-app-coder --check-local <dir> <answer> <prompt…>  (answer: allow | deny)";

/// The answer to every permission request: `allow` → allow_once, `deny` → reject_once.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Answer {
    Allow,
    Deny,
}

impl Answer {
    /// `allow` or `deny`; anything else is not an answer.
    pub fn parse(s: &str) -> Option<Answer> {
        match s {
            "allow" => Some(Answer::Allow),
            "deny" => Some(Answer::Deny),
            _ => None,
        }
    }

    /// Whether the agent gets allow_once (true) or reject_once (false).
    pub fn allow(self) -> bool {
        matches!(self, Answer::Allow)
    }
}

/// The parsed command line: `--check-local <dir> <answer> <prompt…>`.
#[derive(Debug, PartialEq, Eq)]
pub struct Args {
    pub dir: PathBuf,
    pub answer: Answer,
    pub prompt: String,
}

/// `<dir> <answer> <prompt…>`: the folder, the answer to every permission, and the prompt words joined by spaces.
pub fn parse_args(args: &[String]) -> Result<Args, String> {
    let [dir, answer, prompt @ ..] = args else {
        return Err(USAGE.to_owned());
    };
    if prompt.is_empty() {
        return Err(USAGE.to_owned());
    }
    let answer = Answer::parse(answer)
        .ok_or_else(|| format!("the answer is allow or deny, not {answer:?}"))?;
    Ok(Args {
        dir: PathBuf::from(dir),
        answer,
        prompt: prompt.join(" "),
    })
}

// ---- The lines: pure formats ----

/// The seconds since start, as every line ends with them.
fn secs(at: f64) -> String {
    format!("{at:.3}")
}

/// The line one `session/update` prints, with the byte counts it adds to the end sums (message text, thought text).
/// `tool_call` and `tool_call_update` print their own lines; the chunks print `update <kind>` without their text.
pub fn update_line(update: &Value, at: f64) -> (String, usize, usize) {
    let kind = update["sessionUpdate"].as_str().unwrap_or("");
    let text_bytes = |u: &Value| u["content"]["text"].as_str().map_or(0, str::len);
    match kind {
        "tool_call" => (
            format!(
                "tool_call {} {} {}",
                update["title"].as_str().unwrap_or("tool"),
                update["status"].as_str().unwrap_or(""),
                secs(at)
            ),
            0,
            0,
        ),
        "tool_call_update" => (
            format!(
                "tool_call_update {} {} {}",
                update["toolCallId"].as_str().unwrap_or(""),
                update["status"].as_str().unwrap_or(""),
                secs(at)
            ),
            0,
            0,
        ),
        "agent_message_chunk" => (format!("update {kind} {}", secs(at)), text_bytes(update), 0),
        "agent_thought_chunk" => (format!("update {kind} {}", secs(at)), 0, text_bytes(update)),
        _ => (format!("update {kind} {}", secs(at)), 0, 0),
    }
}

/// The line one `session/request_permission` prints: the tool call, the number of diff files, and each diff's path
/// with its oldText and newText byte counts.
pub fn permission_line(tool_call: &Value, at: f64) -> String {
    let id = tool_call["toolCallId"].as_str().unwrap_or("");
    let kind = tool_call["kind"].as_str().unwrap_or("tool");
    let diffs: Vec<&Value> = tool_call["content"]
        .as_array()
        .map(|c| c.iter().filter(|b| b["type"] == "diff").collect())
        .unwrap_or_default();
    let mut parts = vec![format!("permission {id} {kind} diff={}", diffs.len())];
    for d in diffs {
        let path = d["path"].as_str().unwrap_or("");
        let old = d["oldText"].as_str().map_or(0, str::len);
        let new = d["newText"].as_str().map_or(0, str::len);
        parts.push(format!("{path} old={old} new={new}"));
    }
    format!("{} {}", parts.join(" "), secs(at))
}

/// The last line of a run: the stop reason, the seconds since start, and the summed text bytes.
pub fn done_line(stop_reason: &str, at: f64, text_bytes: usize, thought_bytes: usize) -> String {
    format!(
        "done {stop_reason} {} text={text_bytes} thought={thought_bytes}",
        secs(at)
    )
}

/// A start error or a JSON-RPC error.
pub fn error_line(message: &str) -> String {
    format!("error {message}")
}

/// The limit was reached.
pub fn timeout_line() -> String {
    "timeout".to_owned()
}

// ---- The run ----

/// `--check-local` from the command line: parse the arguments, `local::start_acp(dir)`, then [`run`]. The exit code
/// is the process's.
pub fn main_args(args: &[String]) -> i32 {
    let parsed = match parse_args(args) {
        Ok(p) => p,
        Err(e) => {
            println!("{}", error_line(&e));
            return 1;
        }
    };
    let mut out = std::io::stdout();
    match crate::local::start_acp(&parsed.dir) {
        Ok(agent) => run(
            &mut out,
            agent,
            &parsed.dir,
            parsed.answer,
            &parsed.prompt,
            LIMIT,
        ),
        Err(e) => {
            println!("{}", error_line(&e));
            1
        }
    }
}

/// One headless turn on the already started `agent`: `initialize`, `session/new` in `dir`, `session/prompt` with
/// `prompt`, every event as one line on `out`, every permission answered with `answer`. Returns the exit code.
/// Dropping `agent` stops the agent's process and the tunnel.
pub fn run<W: Write>(
    out: &mut W,
    mut agent: LocalAgent,
    dir: &Path,
    answer: Answer,
    prompt: &str,
    limit: Duration,
) -> i32 {
    let start = Instant::now();
    let at = || start.elapsed().as_secs_f64();
    let mut text_bytes = 0usize;
    let mut thought_bytes = 0usize;
    if let Err(e) = agent.acp.initialize() {
        let _ = writeln!(
            out,
            "{}",
            error_line(&format!("can't send initialize: {e}"))
        );
        return 1;
    }
    let new_id = match agent.acp.new_session(dir) {
        Ok(id) => id,
        Err(e) => {
            let _ = writeln!(
                out,
                "{}",
                error_line(&format!("can't send session/new: {e}"))
            );
            return 1;
        }
    };
    let mut prompt_id: Option<i64> = None;
    let mut closed = false;
    let mut code: Option<i32> = None;
    let deadline = start + limit;
    while code.is_none() {
        while let Some(ev) = agent.acp.try_recv() {
            match ev {
                AcpEvent::Update(u) => {
                    let (line, t, th) = update_line(&u, at());
                    text_bytes += t;
                    thought_bytes += th;
                    let _ = writeln!(out, "{line}");
                }
                AcpEvent::Permission {
                    id,
                    tool_call,
                    options: _,
                } => {
                    let _ = writeln!(out, "{}", permission_line(&tool_call, at()));
                    if let Err(e) = agent.acp.answer_permission(id, answer.allow()) {
                        let _ = writeln!(
                            out,
                            "{}",
                            error_line(&format!("can't answer the permission: {e}"))
                        );
                        code = Some(1);
                        break;
                    }
                }
                AcpEvent::Response { id, result, error } => {
                    if let Some(e) = &error {
                        let msg = e["message"]
                            .as_str()
                            .unwrap_or("the agent refused the request");
                        let _ = writeln!(out, "{}", error_line(msg));
                        code = Some(1);
                        break;
                    }
                    if id == new_id {
                        match result.as_ref().and_then(|r| r["sessionId"].as_str()) {
                            Some(sid) => {
                                agent.acp.session_id = Some(sid.to_owned());
                                match agent.acp.prompt(prompt) {
                                    Ok(Some(pid)) => prompt_id = Some(pid),
                                    Ok(None) => {
                                        let _ =
                                            writeln!(out, "{}", error_line("no session to prompt"));
                                        code = Some(1);
                                    }
                                    Err(e) => {
                                        let _ = writeln!(
                                            out,
                                            "{}",
                                            error_line(&format!("can't send session/prompt: {e}"))
                                        );
                                        code = Some(1);
                                    }
                                }
                            }
                            None => {
                                let _ =
                                    writeln!(out, "{}", error_line("the agent made no session"));
                                code = Some(1);
                            }
                        }
                        if code.is_some() {
                            break;
                        }
                    }
                    if prompt_id == Some(id) {
                        let stop = result
                            .as_ref()
                            .and_then(|r| r["stopReason"].as_str())
                            .unwrap_or("unknown");
                        let _ =
                            writeln!(out, "{}", done_line(stop, at(), text_bytes, thought_bytes));
                        code = Some(0);
                        break;
                    }
                }
                AcpEvent::Closed => {
                    closed = true;
                    break;
                }
            }
        }
        if code.is_some() {
            break;
        }
        if closed {
            let _ = writeln!(out, "{}", error_line("the agent's process ended"));
            code = Some(1);
            break;
        }
        if Instant::now() >= deadline {
            let _ = writeln!(out, "{}", timeout_line());
            code = Some(2);
            break;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    // Stops the agent's process and the tunnel.
    drop(agent);
    code.unwrap_or(1)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::acp::Acp;
    use crate::acp::tests::fake_agent;
    use serde_json::json;
    use std::process::Command;

    fn args(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn the_arguments_parse() {
        let a = parse_args(&args(&[
            "/data/projects/demo",
            "allow",
            "fix",
            "the",
            "login",
        ]))
        .unwrap();
        assert_eq!(
            a,
            Args {
                dir: PathBuf::from("/data/projects/demo"),
                answer: Answer::Allow,
                prompt: "fix the login".to_owned(),
            }
        );
        let a = parse_args(&args(&["/w", "deny", "hi"])).unwrap();
        assert_eq!(a.dir, PathBuf::from("/w"));
        assert_eq!(a.answer, Answer::Deny);
        assert_eq!(a.prompt, "hi");
        // No prompt, no answer, no dir: the usage. An answer that is not allow or deny: named.
        assert!(
            parse_args(&args(&["/w", "allow"]))
                .unwrap_err()
                .contains("usage:")
        );
        assert!(parse_args(&args(&["/w"])).unwrap_err().contains("usage:"));
        assert!(parse_args(&args(&[])).unwrap_err().contains("usage:"));
        let e = parse_args(&args(&["/w", "maybe", "hi"])).unwrap_err();
        assert!(e.contains("allow or deny"), "{e}");
    }

    #[test]
    fn the_answer_maps_to_allow_once_or_reject_once() {
        assert_eq!(Answer::parse("allow"), Some(Answer::Allow));
        assert_eq!(Answer::parse("deny"), Some(Answer::Deny));
        assert_eq!(Answer::parse("Allow"), None);
        assert_eq!(Answer::parse(""), None);
        assert!(Answer::Allow.allow());
        assert!(!Answer::Deny.allow());
        // On the wire, for the request's id: allow_once or reject_once.
        let v: Value = serde_json::from_str(
            crate::acp::permission_result(7, Answer::Allow.allow()).trim_end(),
        )
        .unwrap();
        assert_eq!(v["result"]["outcome"]["optionId"], json!("allow_once"));
        let v: Value =
            serde_json::from_str(crate::acp::permission_result(7, Answer::Deny.allow()).trim_end())
                .unwrap();
        assert_eq!(v["result"]["outcome"]["optionId"], json!("reject_once"));
    }

    #[test]
    fn the_line_of_each_event_kind() {
        let (line, t, th) = update_line(
            &json!({"sessionUpdate":"user_message_chunk","content":{"type":"text","text":"fix it"}}),
            1.5,
        );
        assert_eq!(line, "update user_message_chunk 1.500");
        assert_eq!((t, th), (0, 0));
        // The chunk's text is not printed, only its byte count, summed at the end ("héllo" is 6 bytes).
        let (line, t, th) = update_line(
            &json!({"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"héllo"}}),
            0.25,
        );
        assert_eq!(line, "update agent_message_chunk 0.250");
        assert_eq!((t, th), (6, 0));
        let (line, t, th) = update_line(
            &json!({"sessionUpdate":"agent_thought_chunk","content":{"type":"text","text":"hmm"}}),
            2.0,
        );
        assert_eq!(line, "update agent_thought_chunk 2.000");
        assert_eq!((t, th), (0, 3));
        let (line, ..) = update_line(
            &json!({"sessionUpdate":"tool_call","toolCallId":"c1","title":"edit","kind":"edit","status":"pending"}),
            3.0,
        );
        assert_eq!(line, "tool_call edit pending 3.000");
        let (line, ..) = update_line(
            &json!({"sessionUpdate":"tool_call_update","toolCallId":"c1","status":"completed"}),
            4.0,
        );
        assert_eq!(line, "tool_call_update c1 completed 4.000");
        let (line, ..) = update_line(&json!({"sessionUpdate":"something_new"}), 5.0);
        assert_eq!(line, "update something_new 5.000");
    }

    #[test]
    fn the_permission_line_counts_the_diff_files_and_their_bytes() {
        let tc = json!({
            "toolCallId": "call_1", "title": "edit", "kind": "edit",
            "content": [
                {"type": "text", "text": "Fixing"},
                {"type": "diff", "path": "/w/a.txt", "oldText": "the bug", "newText": "the fix"},
                {"type": "diff", "path": "/w/b.txt", "oldText": "x", "newText": "yz"},
            ],
        });
        assert_eq!(
            permission_line(&tc, 1.0),
            "permission call_1 edit diff=2 /w/a.txt old=7 new=7 /w/b.txt old=1 new=2 1.000"
        );
        // No diff content: no files, no path.
        let tc = json!({"toolCallId": "call_2", "kind": "execute", "rawInput": {"command": "ls"}});
        assert_eq!(
            permission_line(&tc, 0.5),
            "permission call_2 execute diff=0 0.500"
        );
    }

    #[test]
    fn the_done_error_and_timeout_lines() {
        assert_eq!(
            done_line("end_turn", 12.3456, 42, 7),
            "done end_turn 12.346 text=42 thought=7"
        );
        assert_eq!(error_line("no session"), "error no session");
        assert_eq!(timeout_line(), "timeout");
    }

    #[test]
    fn a_whole_run_against_the_fake_agent_end_to_end() {
        let log = tempdir("log").join("stdin.log");
        let agent = crate::local::LocalAgent::from_acp(fake_agent(&log));
        let dir = tempdir("work");
        let mut out: Vec<u8> = Vec::new();
        let code = run(&mut out, agent, &dir, Answer::Allow, "fix it", LIMIT);
        assert_eq!(code, 0);
        let text = String::from_utf8(out).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        let kinds: Vec<&str> = lines
            .iter()
            .map(|l| l.split_whitespace().next().unwrap())
            .collect();
        assert_eq!(
            kinds,
            [
                "update",
                "update",
                "tool_call",
                "permission",
                "tool_call_update",
                "update",
                "done"
            ],
            "{text}"
        );
        assert!(lines[0].starts_with("update user_message_chunk "), "{text}");
        assert!(
            lines[1].starts_with("update agent_thought_chunk "),
            "{text}"
        );
        assert!(lines[2].starts_with("tool_call edit pending "), "{text}");
        assert!(
            lines[3].starts_with("permission call_1 edit diff=1 /w/a.txt old=7 new=7 "),
            "{text}"
        );
        assert!(
            lines[4].starts_with("tool_call_update call_1 completed "),
            "{text}"
        );
        assert!(
            lines[5].starts_with("update agent_message_chunk "),
            "{text}"
        );
        assert!(lines[6].starts_with("done end_turn "), "{text}");
        // The done line carries the summed bytes: "fixed" is 5, "thinking" is 8.
        assert!(
            lines[6].contains("text=5") && lines[6].contains("thought=8"),
            "{text}"
        );
        // Every line carries the seconds since start: at the end of the event lines, after the stop reason on `done`.
        for l in &lines[..6] {
            let secs = l
                .rsplit_once(' ')
                .map_or(false, |(_, t)| t.parse::<f64>().is_ok());
            assert!(secs, "{l}");
        }
        let done: Vec<&str> = lines[6].split_whitespace().collect();
        assert_eq!(done[0], "done");
        assert_eq!(done[1], "end_turn");
        assert!(done[2].parse::<f64>().is_ok(), "{}", lines[6]);
        // No model text on stdout.
        assert!(
            !text.contains("fixed") && !text.contains("thinking"),
            "{text}"
        );
        assert!(
            !text.contains("the bug") && !text.contains("the fix"),
            "{text}"
        );
        // The wire: initialize, session/new in dir, session/prompt with the joined words, allow_once for request 100.
        let sent = std::fs::read_to_string(&log).unwrap();
        assert!(sent.contains(r#""method":"initialize""#), "{sent}");
        assert!(
            sent.contains(&format!(
                r#""method":"session/new","params":{{"cwd":"{}","mcpServers":[]}}"#,
                dir.display()
            )),
            "{sent}"
        );
        assert!(sent.contains(r#""method":"session/prompt""#), "{sent}");
        assert!(sent.contains(r#""text":"fix it""#), "{sent}");
        assert!(
            sent.contains(r#"{"id":100,"jsonrpc":"2.0","result":{"outcome":{"optionId":"allow_once","outcome":"selected"}}}"#),
            "{sent}"
        );
    }

    #[test]
    fn a_denied_run_answers_reject_once_and_still_ends() {
        let log = tempdir("log").join("stdin.log");
        let agent = crate::local::LocalAgent::from_acp(fake_agent(&log));
        let dir = tempdir("work");
        let mut out: Vec<u8> = Vec::new();
        let code = run(&mut out, agent, &dir, Answer::Deny, "fix it", LIMIT);
        assert_eq!(code, 0);
        let text = String::from_utf8(out).unwrap();
        assert!(
            text.contains("permission call_1 edit diff=1 /w/a.txt old=7 new=7"),
            "{text}"
        );
        let done = text.lines().last().unwrap();
        assert!(
            done.starts_with("done end_turn ")
                && done.contains("text=0")
                && done.contains("thought=8"),
            "{text}"
        );
        let sent = std::fs::read_to_string(&log).unwrap();
        assert!(
            sent.contains(r#"{"id":100,"jsonrpc":"2.0","result":{"outcome":{"optionId":"reject_once","outcome":"selected"}}}"#),
            "{sent}"
        );
    }

    /// A fake agent that refuses `initialize`: the run prints `error <message>` and exits 1.
    #[test]
    fn a_json_rpc_error_prints_error_and_exits_1() {
        let dir = tempdir("fake");
        let script = dir.join("err.sh");
        std::fs::write(
            &script,
            "#!/bin/sh\nwhile IFS= read -r line; do\n  case \"$line\" in\n    *'\"method\":\"initialize\"'*) echo '{\"jsonrpc\":\"2.0\",\"id\":1,\"error\":{\"code\":-32602,\"message\":\"no protocol for you\"}}' ;;\n  esac\ndone\n",
        )
        .unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        let mut cmd = Command::new("sh");
        cmd.arg(&script);
        let agent = crate::local::LocalAgent::from_acp(Acp::spawn(&mut cmd).unwrap());
        let mut out: Vec<u8> = Vec::new();
        let code = run(&mut out, agent, Path::new("/w"), Answer::Allow, "hi", LIMIT);
        assert_eq!(code, 1);
        assert_eq!(
            String::from_utf8(out).unwrap().trim_end(),
            "error no protocol for you"
        );
    }

    /// A fake agent that never answers: the limit prints `timeout` and exits 2.
    #[test]
    fn the_limit_prints_timeout_and_exits_2() {
        let dir = tempdir("fake");
        let script = dir.join("quiet.sh");
        std::fs::write(&script, "#!/bin/sh\nwhile IFS= read -r line; do :; done\n").unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        let mut cmd = Command::new("sh");
        cmd.arg(&script);
        let agent = crate::local::LocalAgent::from_acp(Acp::spawn(&mut cmd).unwrap());
        let mut out: Vec<u8> = Vec::new();
        let code = run(
            &mut out,
            agent,
            Path::new("/w"),
            Answer::Allow,
            "hi",
            Duration::from_millis(300),
        );
        assert_eq!(code, 2);
        assert_eq!(String::from_utf8(out).unwrap().trim_end(), "timeout");
    }

    fn tempdir(name: &str) -> PathBuf {
        use std::sync::atomic::{AtomicU64, Ordering};
        static N: AtomicU64 = AtomicU64::new(0);
        let d = std::env::temp_dir().join(format!(
            "lucina-check-local-test-{}-{}-{name}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }
}
