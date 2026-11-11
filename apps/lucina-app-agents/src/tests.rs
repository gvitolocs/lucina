//! The app against sample mode's server, through the same JSON as the wire, on `lucina-agents`' real types, projection and
//! buckets. The expectations follow `web/src/views/Agents.test.tsx`, `web/src/lib/agentsTimeline.test.ts` and `state/agents.ts`.

use std::sync::Arc;

use lucina_account::ApiError;
use lucina_account::sse::Block;
use lucina_app::{Harness, keys};
use serde_json::{Value, json};

use crate::api::{Api, B, Blocks, Sample, b64, enc};
use crate::app::{Agents, Health, Msg, Section, Tab};
use crate::model;
use lucina_agents_client::pages::{Act, Line};

/// 2026-11-02 12:00 UTC.
const NOW: i64 = 1_793_620_800_000;
const RUNNING: &str = "agt_sample_running";
const FAILED: &str = "agt_sample_failed";
const DONE: &str = "agt_sample_done";

struct T {
    h: Harness<Agents>,
    s: Sample,
}

fn start() -> T {
    let s = Sample::new(NOW);
    let api: Arc<dyn Api> = Arc::new(s.clone());
    let mut a = Agents::new(Some(api), None);
    a.fixed_now = Some(NOW);
    a.pace = std::time::Duration::ZERO;
    let mut t = T { h: Harness::new(a, 1280, 800), s };
    t.h.app().start();
    t.settle();
    t
}

impl T {
    fn settle(&mut self) {
        for _ in 0..3 {
            self.h.core.input(lucina_app::Input::Wheel(0));
            self.h.core.paint();
        }
    }

    fn app(&mut self) -> &mut Agents {
        self.h.app()
    }

    fn text(&mut self) -> String {
        format!("{:?}", self.app().lines)
    }

    fn open(&mut self, id: &str) {
        self.app().open_agent(id.to_owned());
        self.settle();
    }

    /// Walks the main list to the line and presses Enter.
    fn enter_on(&mut self, what: &str) {
        let at = self.app().lines.iter().position(|l| match l {
            Line::Row { title, .. } => title.contains(what),
            Line::Button { label, .. } => label.contains(what),
            _ => false,
        });
        let at = at.unwrap_or_else(|| panic!("no line with {what}"));
        self.app().main.select(at);
        self.settle();
        self.h.key(keys::KEY_ENTER);
        self.settle();
    }

    fn bus(&mut self, v: Value) {
        self.h.core.message(Msg::Bus(v));
        self.settle();
    }
}

#[test]
fn the_list_puts_what_needs_the_owner_first() {
    let mut t = start();
    let titles: Vec<String> = t.app().rows.iter().map(|r| r.title.clone()).collect();
    assert_eq!(titles, ["Upload retry", "Index rebuild", "Tidy the docs"]);
    let r = &t.app().rows[0];
    assert_eq!(r.state, "Needs permission (1)");
    assert_eq!(t.app().rows[1].state, "Error");
    assert_eq!(t.app().rows[2].state, "Idle");
}

#[test]
fn the_keyboard_alone_opens_an_agent() {
    let mut t = start();
    // The first row is selected when the list arrives.
    t.h.key(keys::KEY_ENTER);
    t.settle();
    assert_eq!(t.app().agent.as_deref(), Some(RUNNING));
    assert!(t.text().contains("Add a retry to the upload"));
}

#[test]
fn the_timeline_collapses_chunks_and_tool_updates_as_the_web_does() {
    let mut t = start();
    t.open(RUNNING);
    let text = t.text();
    assert!(text.contains("I'll read the upload code first."), "chunks join");
    assert!(text.contains("▸ Thinking"));
    assert!(!text.contains("try again three times"), "thinking starts closed");
    assert!(text.contains("Conversation compacted"));
    assert!(text.contains("✓ Add the retry"));
    assert!(text.contains("○ Run the tests"));
    t.enter_on("Thinking");
    assert!(t.text().contains("should try again three times"));
}

#[test]
fn a_tool_call_opens_with_enter() {
    let mut t = start();
    t.open(RUNNING);
    assert!(!t.text().contains("test result: ok"));
    t.enter_on("cargo test");
    let text = t.text();
    assert!(text.contains("$ cargo test -p sample"));
    assert!(text.contains("test result: ok. 12 passed"));
    assert!(text.contains("Exit code 0"));
}

#[test]
fn allow_goes_to_the_agents_route_and_the_card_says_how_it_ended() {
    let mut t = start();
    t.open(RUNNING);
    assert!(t.text().contains("git push origin sample-branch"));
    t.enter_on("Allow");
    let writes = t.s.writes();
    assert_eq!(writes.len(), 1);
    assert_eq!(writes[0].0, format!("{B}/{RUNNING}/permissions/perm-1"));
    assert_eq!(writes[0].1, json!({"behavior": "allow"}));
    t.settle();
    let text = t.text();
    assert!(text.contains("Allowed"), "{text}");
    assert!(!text.contains("Button { label: \"Allow\""));
}

#[test]
fn a_stray_enter_in_the_chat_never_answers_a_permission() {
    let mut t = start();
    t.open(RUNNING);
    assert!(t.text().contains("Allow"));
    t.h.key(keys::KEY_ENTER);
    t.settle();
    assert!(t.s.writes().is_empty(), "{:?}", t.s.writes());
}

#[test]
fn deny_on_an_external_request_goes_to_its_own_route() {
    let mut t = start();
    t.app().switch(Section::Permissions);
    t.settle();
    assert!(t.text().contains("Hand SAMPLE_NAME to a service"));
    t.app().answer("ext-1", false);
    t.settle();
    let writes = t.s.writes();
    assert_eq!(writes[0].0, format!("{B}/permissions/ext-1"));
    assert_eq!(writes[0].1, json!({"behavior": "deny"}));
    assert!(t.text().contains("Denied"));
}

#[test]
fn a_request_answered_elsewhere_closes_the_card() {
    let mut t = start();
    t.open(RUNNING);
    t.s.call("POST", &format!("{B}/{RUNNING}/permissions/perm-1"), Some(&json!({"behavior": "deny"}))).unwrap();
    // This device still shows it as pending, and answers.
    t.app().answer(&format!("{RUNNING}/perm-1"), true);
    t.settle();
    assert!(t.text().contains("Already answered elsewhere."), "{}", t.text());
}

#[test]
fn send_while_idle_and_steer_while_running() {
    let mut t = start();
    t.open(DONE);
    t.app().composer.set("Now the changelog");
    t.app().send_prompt();
    t.settle();
    let writes = t.s.writes();
    assert_eq!(writes.last().unwrap().1, json!({"prompt": "Now the changelog"}));
    assert_eq!(t.app().composer.text, "");
    t.open(RUNNING);
    t.app().composer.set("Skip the docs");
    t.app().send_prompt();
    t.settle();
    let writes = t.s.writes();
    assert_eq!(writes.last().unwrap().1, json!({"prompt": "Skip the docs", "activeTurnBehavior": "steer"}));
}

#[test]
fn the_composer_works_from_the_keyboard() {
    let mut t = start();
    t.open(DONE);
    t.h.type_str("i");
    t.settle();
    t.h.type_str("Hello agent");
    t.h.key(keys::KEY_ENTER);
    t.settle();
    let writes = t.s.writes();
    assert_eq!(writes.last().unwrap().0, format!("{B}/{DONE}/send"));
    assert_eq!(writes.last().unwrap().1["prompt"], "Hello agent");
}

#[test]
fn an_archived_agent_cannot_be_messaged_until_unarchived() {
    let mut t = start();
    t.open(DONE);
    t.app().toggle_archive();
    t.settle();
    assert!(t.app().store.archived(DONE));
    t.app().composer.set("x");
    t.app().send_prompt();
    assert!(!t.s.writes().iter().any(|(p, _)| p.ends_with("/send")));
    t.app().toggle_archive();
    t.settle();
    assert!(!t.app().store.archived(DONE));
}

#[test]
fn the_stream_adds_rows_in_order_and_a_gap_reads_again() {
    let mut t = start();
    t.open(DONE);
    let row = |seq: u64, text: &str| json!({"type": "agent_stream", "agentId": DONE, "seq": seq, "epoch": "sample-epoch", "timestamp": "2026-11-02T12:00:00.000Z", "event": {"type": "timeline", "turnId": "t9", "item": {"type": "assistant_message", "text": text}}});
    t.bus(row(2, "Second row."));
    assert!(t.text().contains("Second row."));
    let reads = t.s.reads(&format!("{B}/{DONE}/timeline"));
    t.bus(row(9, "Far ahead."));
    assert_eq!(t.s.reads(&format!("{B}/{DONE}/timeline")), reads + 1, "a gap reads the timeline again");
    assert!(!t.text().contains("Far ahead."));
    // A row already known changes nothing.
    t.bus(row(2, "Second row again."));
    assert!(!t.text().contains("Second row again."));
}

#[test]
fn a_new_epoch_starts_the_timeline_over() {
    let mut t = start();
    t.open(DONE);
    let reads = t.s.reads(&format!("{B}/{DONE}/timeline"));
    t.bus(json!({"type": "agent_stream", "agentId": DONE, "seq": 2, "epoch": "another", "timestamp": "x", "event": {"type": "timeline", "item": {"type": "error", "message": "m"}}}));
    assert_eq!(t.s.reads(&format!("{B}/{DONE}/timeline")), reads + 1);
}

#[test]
fn events_keep_the_list_live() {
    let mut t = start();
    let snap = t.s.call("GET", &format!("{B}/{FAILED}"), None).unwrap();
    let mut next = snap.clone();
    next["status"] = "idle".into();
    next["title"] = "Index rebuilt".into();
    t.bus(json!({"type": "agent_state", "agent": next}));
    assert!(t.app().rows.iter().any(|r| r.title == "Index rebuilt"));
    t.bus(json!({"type": "agent_deleted", "agentId": DONE}));
    assert!(!t.app().rows.iter().any(|r| r.id == DONE));
    t.bus(json!({"type": "permission", "permission": {"id": "n", "kind": "external", "title": "New one", "status": "pending", "createdAt": "2026-11-02T12:00:00.000Z", "expiresAt": "2026-11-02T13:00:00.000Z"}}));
    assert_eq!(t.app().store.pending_permissions().len(), 3);
    // An unknown frame does nothing.
    t.bus(json!({"type": "agent_command", "command": {}}));
    t.bus(json!({"type": "from_the_future"}));
    let reads = t.s.reads(&format!("{B}/items"));
    t.bus(json!({"type": "stream.reset"}));
    assert_eq!(t.s.reads(&format!("{B}/items")), reads + 1, "a reset reads everything again");
}

#[test]
fn a_short_drop_says_nothing_and_a_down_backend_says_so() {
    let mut t = start();
    t.app().mark_lost();
    t.settle();
    assert_eq!(t.app().banner(), None, "quiet for a drop");
    assert_eq!(t.app().health, Health::Live);
    t.app().mark_live();
    assert_eq!(t.app().health, Health::Live);
}

#[test]
fn a_stream_that_stays_away_is_checked_against_the_rest_routes() {
    // The routes answer: only the stream is broken.
    let mut t = start();
    t.app().mark_lost();
    t.app().check_backend_now();
    t.settle();
    assert_eq!(t.app().health, Health::Polling);
    assert_eq!(t.app().banner(), Some(("Live updates are paused. Refreshing every 15 seconds.", false)));
    let reads = t.s.reads(&format!("{B}/items"));
    t.app().mark_live();
    t.settle();
    assert_eq!(t.app().health, Health::Live);
    assert_eq!(t.app().banner(), None);
    assert_eq!(t.s.reads(&format!("{B}/items")), reads + 1, "back from away, everything is read again");

    // The routes fail too: the backend is down, and the page says so.
    let api: Arc<dyn Api> = Arc::new(Without { inner: Sample::new(NOW), route: "/items" });
    let mut h = Harness::new(Agents::new(Some(api), None), 1280, 800);
    h.app().mark_lost();
    h.app().check_backend_now();
    assert_eq!(h.app().health, Health::Down);
    assert_eq!(h.app().banner(), Some(("Lucina Agents isn't answering. Trying again…", true)));
}

#[test]
fn search_filters_here_and_asks_the_server_after_two_letters() {
    let mut t = start();
    t.app().query.set("index");
    t.app().focus_nothing();
    t.app().search_now();
    t.settle();
    let found: Vec<String> = t.app().rows.iter().map(|r| r.id.clone()).collect();
    assert_eq!(found, [FAILED]);
    assert!(t.s.reads(&format!("{B}/search?q=index")) >= 1);
}

#[test]
fn changes_show_the_files_and_numbered_lines() {
    let mut t = start();
    t.open(RUNNING);
    t.app().switch_tab(Tab::Changes);
    t.settle();
    let d = t.app().diff.result.clone().unwrap();
    assert_eq!(d.files.len(), 3);
    let rows = t.app().diff.rows.clone();
    assert_eq!(rows[0].content, "@@ -1,3 +1,3 @@");
    assert_eq!((rows[2].kind.as_str(), rows[2].n.as_str(), rows[2].content.as_str()), ("remove", "2", "    send()"));
    assert_eq!((rows[3].kind.as_str(), rows[3].n.as_str()), ("add", "2"));
    t.h.key(keys::KEY_DOWN);
    t.settle();
    assert_eq!(t.app().diff.selected, 1);
    assert_eq!(t.app().diff.rows[1].content, "# Retry");
}

#[test]
fn files_browse_and_read_and_say_when_a_file_is_binary() {
    let mut t = start();
    t.open(RUNNING);
    t.app().switch_tab(Tab::Files);
    t.settle();
    let names: Vec<String> = t.app().files.entries.clone().unwrap().iter().map(|e| e.name.clone()).collect();
    assert_eq!(names, ["src", "logo.png", "README.md"], "folders first, then by name");
    t.app().open_entry(2);
    t.settle();
    assert_eq!(t.app().files.lines[0], "# Sample");
    t.app().open_entry(1);
    t.settle();
    assert_eq!(t.app().files.file.as_ref().unwrap().encoding, "base64");
    t.app().open_entry(0);
    t.settle();
    assert_eq!(t.app().files.dir, "src");
    assert_eq!(t.app().files.entries.clone().unwrap()[0].name, "sample.rs");
}

#[test]
fn the_terminal_shows_text_without_escapes_and_sends_a_line_and_enter() {
    let mut t = start();
    t.open(RUNNING);
    t.app().switch_tab(Tab::Terminal);
    t.settle();
    assert_eq!(t.app().term.selected.as_deref(), Some("term-1"));
    t.app().term_block(&Block { event: "snapshot".into(), data: json!({"data": "\u{1b}[32m$\u{1b}[0m ls\r\nfile\r\n"}).to_string(), id: None });
    assert_eq!(t.app().term.lines[0], "$ ls");
    assert_eq!(t.app().term.lines[1], "file");
    t.app().term_block(&Block { event: "output".into(), data: json!({"b64": b64(b"more")}).to_string(), id: None });
    assert_eq!(t.app().term.lines[2], "more");
    t.app().term_block(&Block { event: "exit".into(), data: json!({"exitCode": 3}).to_string(), id: None });
    assert_eq!(t.app().term.exit, Some(Some(3)));
    t.app().term.input.set("whoami");
    t.app().term_send_line();
    t.settle();
    assert_eq!(t.s.writes().last().unwrap().1, json!({"keys": ["whoami", "Enter"]}));
}

#[test]
fn scripts_start_and_stop() {
    let mut t = start();
    t.open(RUNNING);
    t.app().switch_tab(Tab::Scripts);
    t.settle();
    let text = t.text();
    assert!(text.contains("npm run dev"));
    assert!(text.contains("Open ports"));
    t.enter_on("Stop dev");
    assert!(t.s.writes().last().unwrap().0.ends_with("/scripts/dev/stop"));
}

#[test]
fn new_run_sends_what_was_chosen() {
    let mut t = start();
    t.app().open_new_run();
    t.settle();
    assert_eq!(t.app().run_targets().len(), 2, "the workspace and a new worktree");
    {
        let n = t.app().new_run.as_mut().unwrap();
        n.prompt.set("Fix the flaky test");
        n.model = Some(1);
        n.mode = Some(1);
    }
    t.app().start_run();
    t.settle();
    assert!(t.app().new_run.is_none());
    let (path, body) = t.s.writes().last().cloned().unwrap();
    assert_eq!(path, B);
    assert_eq!(body["provider"], "claude");
    assert_eq!(body["model"], "sample-model-2");
    assert_eq!(body["modeId"], "plan");
    assert_eq!(body["workspaceId"], "wks_sample");
    assert_eq!(body["prompt"], "Fix the flaky test");
    assert!(t.app().agent.as_deref().unwrap().starts_with("agt_new_"));
}

#[test]
fn new_run_in_a_new_worktree_makes_the_workspace_first() {
    let mut t = start();
    t.app().open_new_run();
    t.settle();
    t.app().new_run.as_mut().unwrap().target = 1;
    t.app().start_run();
    t.settle();
    let writes = t.s.writes();
    assert_eq!(writes[0].0, format!("{B}/workspaces"));
    assert_eq!(writes[0].1["isolation"], "worktree");
    assert_eq!(writes[1].0, B);
}

#[test]
fn schedules_usage_and_the_pipeline_say_what_the_web_says() {
    let mut t = start();
    t.app().switch(Section::Schedules);
    t.settle();
    let text = t.text();
    assert!(text.contains("Nightly tests"));
    assert!(text.contains("0 3 * * * (UTC)"));
    assert!(text.contains("next in 2 hr"));
    t.enter_on("Pause");
    assert!(t.text().contains("Paused"));
    assert!(t.text().contains("Resume"));

    t.app().switch(Section::Usage);
    t.settle();
    let text = t.text();
    assert!(text.contains("2 agents (1 running) · Input 183K · Cached 96K · Output 12.4K"));
    assert!(text.contains("$1.27"));
    assert!(text.contains("Schedules: 1 active, 0 paused, 2 completed."));

    t.app().switch(Section::Pipeline);
    t.settle();
    let text = t.text();
    assert!(text.contains("2 workspaces · 1 in the queue"));
    assert!(text.contains("Merge other-branch"));
    assert!(text.contains("Open (1)"));
    assert!(text.contains("Stale (1)"));
    assert!(text.contains("+3 −0 · uncommitted changes"));
    assert!(text.contains("1 main checkouts have uncommitted files"));
    assert!(text.contains("Deploy sample-server"));
}

/// A server where one route is not deployed.
struct Without {
    inner: Sample,
    route: &'static str,
}

impl Api for Without {
    fn call(&self, method: &str, path: &str, body: Option<&Value>) -> Result<Value, ApiError> {
        if path.contains(self.route) {
            return Err(ApiError::Http(404, "not_found".into(), "no route".into()));
        }
        self.inner.call(method, path, body)
    }
    fn stream(&self, path: &str, last_id: Option<&str>) -> Result<Box<dyn Blocks>, ApiError> {
        self.inner.stream(path, last_id)
    }
}

#[test]
fn a_route_that_is_not_deployed_hides_its_feature() {
    let api: Arc<dyn Api> = Arc::new(Without { inner: Sample::new(NOW), route: "/schedules" });
    let mut h = Harness::new(Agents::new(Some(api), None), 1280, 800);
    h.app().start();
    assert!(!h.app().schedules_available);
    let api: Arc<dyn Api> = Arc::new(Without { inner: Sample::new(NOW), route: "/terminals" });
    let mut h = Harness::new(Agents::new(Some(api), None), 1280, 800);
    h.app().start();
    h.app().open_agent(RUNNING.into());
    assert!(!h.app().tabs().iter().any(|(t, _)| *t == Tab::Terminal), "no Terminal tab");
}

#[test]
fn an_unknown_agent_says_so() {
    let mut t = start();
    t.open("agt_gone");
    assert!(t.app().missing);
}

#[test]
fn a_timeline_row_of_a_new_kind_does_not_lose_the_rest() {
    let rows = vec![
        json!({"seq": 1, "timestamp": "t", "item": {"type": "user_message", "text": "kept"}}),
        json!({"seq": 2, "timestamp": "t", "item": {"type": "kind_from_the_future", "x": 1}}),
        json!({"seq": 3, "timestamp": "t", "item": {"type": "assistant_message", "text": "also kept"}}),
    ];
    let parsed: Vec<lucina_agents::timeline::TimelineRow> = rows.iter().filter_map(|r| serde_json::from_value(r.clone()).ok()).collect();
    assert_eq!(parsed.len(), 2);
}

#[test]
fn rules_match_the_web() {
    assert_eq!(model::agent_title(Some("  "), "abcdefghi", "/a/b"), "b");
    assert_eq!(model::agent_title(None, "abcdefghi", ""), "abcdefg");
    assert_eq!(model::agent_title(Some("T"), "x", "/y"), "T");
    assert_eq!(model::ansi_to_text("a\u{1b}[31mb\u{1b}[0m\r\nc\rd\u{8}e\u{1b}]0;title\u{7}f"), "ab\nef");
    assert_eq!(model::base64(&b64(b"hello world")), b"hello world");
    assert_eq!(model::compact(183_000.0), "183K");
    assert_eq!(model::compact(12_400.0), "12.4K");
    assert_eq!(model::compact(310.0), "310");
    assert_eq!(model::usd(1.27), "$1.27");
    assert_eq!(model::size_text(2048), "2.0 KB");
    assert_eq!(model::parent_of("a/b/c"), "a/b");
    assert_eq!(model::parent_of("a"), "");
    assert_eq!(model::clock(125), "2:05");
    assert!(model::matches("Upload retry claude /srv", "RETRY claude"));
    assert!(!model::matches("Upload retry", "retry codex"));
    assert_eq!(model::permission_text(None, Some("ls")), "ls");
    assert_eq!(enc("a b/c"), "a%20b%2Fc");
    assert_eq!(Act::None, Act::None);
}

#[test]
fn every_page_paints_at_both_sizes() {
    for (w, h) in [(800, 600), (1920, 1080)] {
        let s = Sample::new(NOW);
        let api: Arc<dyn Api> = Arc::new(s);
        let mut a = Agents::new(Some(api), None);
        a.fixed_now = Some(NOW);
        let mut h = Harness::new(a, w, h);
        h.app().start();
        for sec in [Section::Runs, Section::Permissions, Section::Schedules, Section::Usage, Section::Pipeline] {
            h.app().switch(sec);
            h.core.input(lucina_app::Input::Wheel(0));
            h.core.paint();
        }
        h.app().open_agent(RUNNING.into());
        for tab in [Tab::Chat, Tab::Changes, Tab::Files, Tab::Terminal, Tab::Scripts] {
            h.app().switch_tab(tab);
            h.core.input(lucina_app::Input::Wheel(0));
            h.core.paint();
        }
        h.app().open_new_run();
        h.core.input(lucina_app::Input::Wheel(0));
        h.core.paint();
        let _ = h.ppm();
    }
}

/// Run by `scripts/agents-contract.sh`, on what the real daemon answered. Prints counts only: the answers are real data.
#[test]
#[ignore]
fn contract_with_the_real_daemon() {
    use lucina_agents::schedule::Schedule;
    use lucina_agents::timeline::TimelineRow;
    use lucina_agents::types::{AgentListItem, AgentSnapshot};
    use model::*;

    let dir = std::env::var("LUCINA_AGENTS_DUMP").expect("run by scripts/agents-contract.sh");
    let read = |name: &str| -> Option<Value> { serde_json::from_slice(&std::fs::read(format!("{dir}/{name}.json")).ok()?).ok() };
    /// Every row must parse, and `check` says the fields that matter arrived under the names expected.
    fn rows<T: serde::de::DeserializeOwned>(what: &str, v: &Value, check: impl Fn(&T) -> bool) {
        let list = v.as_array().unwrap_or_else(|| panic!("{what}: not a list"));
        let mut bad = 0;
        let mut empty = 0;
        for r in list {
            match serde_json::from_value::<T>(r.clone()) {
                Ok(x) if check(&x) => {}
                Ok(_) => empty += 1,
                Err(_) => bad += 1,
            }
        }
        println!("contract: {what}: {} rows, {bad} did not parse, {empty} missing a field", list.len());
        assert_eq!((bad, empty), (0, 0), "{what}");
    }
    if let Some(v) = read("items") {
        rows::<AgentListItem>("items", &v["agents"], |a| !a.id.is_empty());
    }
    if let Some(v) = read("snapshots") {
        rows::<AgentSnapshot>("snapshots", &v["agents"], |a| !a.id.is_empty());
    }
    if let Some(v) = read("snapshot") {
        serde_json::from_value::<AgentSnapshot>(v).expect("one snapshot");
    }
    if let Some(v) = read("timeline") {
        rows::<TimelineRow>("timeline", &v["rows"], |_| true);
        assert!(v["epoch"].is_string());
    }
    if let Some(v) = read("prompts") {
        rows::<PromptEntry>("prompts", &v["prompts"], |p| p.seq > 0);
    }
    if let Some(v) = read("permissions") {
        rows::<PermissionView>("permissions", &v["permissions"], |p| !p.id.is_empty() && !p.status.is_empty() && !p.kind.is_empty() && !p.created_at.is_empty());
    }
    if let Some(v) = read("workspaces") {
        rows::<Workspace>("workspaces", &v["workspaces"], |w| !w.workspace_id.is_empty() && !w.cwd.is_empty() && !w.label().is_empty());
    }
    if let Some(v) = read("projects") {
        rows::<ProjectRecord>("projects", &v["projects"], |p| !p.project_id.is_empty() && !p.name.is_empty());
    }
    if let Some(v) = read("providers") {
        rows::<ProviderInfo>("providers", &v, |p| !p.id.is_empty() && !p.label.is_empty());
    }
    if let Some(v) = read("terminals") {
        rows::<Terminal>("terminals", &v["terminals"], |t| !t.id.is_empty() && !t.workspace_id.is_empty());
    }
    if let Some(v) = read("schedules") {
        rows::<Schedule>("schedules", &v["schedules"], |s| !s.id.is_empty());
    }
    if let Some(v) = read("usage") {
        let u: UsageReport = serde_json::from_value(v).expect("usage");
        let p = u.providers.unwrap_or_default();
        println!("contract: usage: {} providers", p.len());
        assert!(p.iter().all(|x| !x.provider.is_empty()));
    }
    if let Some(v) = read("board") {
        rows::<BoardWorkspace>("board workspaces", &v["workspaces"], |w| !w.workspace_id.is_empty() && !w.state.is_empty());
        rows::<BoardJob>("board queue", &v["queue"], |j| !j.job_id.is_empty() && !j.state.is_empty());
        rows::<BoardJob>("board jobs", &v["recentJobs"], |j| !j.job_id.is_empty() && !j.state.is_empty());
        rows::<BoardDevice>("board leases", &v["leases"], |d| !d.device.is_empty());
        rows::<BoardDeploy>("board deploys", &v["deploys"], |d| !d.runtime.is_empty() && !d.commit.is_empty());
        serde_json::from_value::<PipelineBoard>(v).expect("the board");
    }
    if let Some(v) = read("search") {
        rows::<SearchResult>("search", &v["results"], |r| !r.agent_id.is_empty());
    }
    if let Some(v) = read("diff") {
        let d: DiffResult = serde_json::from_value(v).expect("the diff");
        println!("contract: diff: {} files", d.files.len());
        assert!(d.files.iter().all(|f| !f.path.is_empty() && !f.status.is_empty()));
    }
    if let Some(v) = read("files") {
        rows::<FileEntry>("files", &v["entries"], |e| !e.name.is_empty() && !e.kind.is_empty());
    }
    if let Some(v) = read("scripts") {
        rows::<Script>("scripts", &v["scripts"], |s| !s.script_name.is_empty());
    }
    if let Some(v) = read("services") {
        rows::<Service>("services", &v["services"], |s| !s.name.is_empty());
    }
}
