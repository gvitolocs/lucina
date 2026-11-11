//! The screens: `web/src/views/Agents.tsx` and `components/agents/*`. Sections on the left; in Runs the agents beside the open one,
//! with its chat, changes, files, terminal and scripts. Requests go out on a thread each and come back as closures run on this
//! thread (`Msg::Apply`), one stream keeps the store live (`state/agents.ts`), and when it stays away the REST routes say what the
//! page shows: nothing for a short drop, a banner and polling otherwise.

use std::collections::{BTreeMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use lucina_account::ApiError;
use lucina_account::sse::Block;
use lucina_account::ui::SignInView;
use lucina_agents::bucket::Bucket;
use lucina_agents::schedule::Schedule;
use lucina_agents::timeline::TimelineRow;
use lucina_agents::types::{AgentListItem, AgentSnapshot};
use lucina_app::{App, Face, ListState, Rect, Rgb, Sender, TextEdit, Ui, keys};
use serde::de::DeserializeOwned;
use serde_json::{Value, json};

use crate::api::{Api, B, enc, not_deployed};
use crate::model::{self, DiffResult, DiffRow, FileContent, FileEntry, PermissionView, PipelineBoard, ProjectRecord, PromptEntry, ProviderInfo, Script, SearchResult, Service, Terminal, UsageReport, Workspace, agent_title, ansi_to_text, diff_rows, size_text, status_label};
use crate::store::{Effect, Store, Timeline, TlStatus};
use lucina_agents_client::pages::{self, Act, Line};
use lucina_agents_client::ui::{bucket_colour, draw, height};

/// `streamTiming`.
const QUIET: Duration = Duration::from_secs(5);
const POLL: Duration = Duration::from_secs(15);
const RETRY: Duration = Duration::from_secs(3);
/// Longer than the web's 15 s: the account's limit of 60 requests a minute is shared by all apps.
const RETRY_MAX: Duration = Duration::from_secs(30);
/// `PipelinePage`'s `REFRESH_MS`.
const BOARD_REFRESH: Duration = Duration::from_secs(30);
/// Streamed text arrives in many small pieces: the screen follows at this pace.
const STREAM_PACE: Duration = Duration::from_millis(100);
/// A search waits for the typing to rest. The web's 250 ms is too eager for the account's limit.
const SEARCH_REST: Duration = Duration::from_millis(600);
const TERM_MAX: usize = 256 * 1024;
const PAGE: usize = 500;

pub type Apply = Box<dyn FnOnce(&mut Agents) + Send>;
type Work = Box<dyn FnOnce() -> Apply + Send>;

pub enum Msg {
    Changed,
    SignedIn,
    Bus(Value),
    Apply(Apply),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Section {
    Runs,
    Permissions,
    Schedules,
    Usage,
    Pipeline,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tab {
    Chat,
    Changes,
    Files,
    Terminal,
    Scripts,
}

/// `agentsBanner`'s states.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Health {
    Live,
    Reconnecting,
    Polling,
    Down,
}

pub struct ListRow {
    pub id: String,
    pub title: String,
    pub sub: String,
    pub state: String,
    pub bucket: Bucket,
}

#[derive(Default)]
pub struct DiffPane {
    pub mode: usize,
    pub ignore_ws: bool,
    pub result: Option<DiffResult>,
    pub error: Option<String>,
    pub loading: bool,
    pub selected: usize,
    pub rows: Vec<DiffRow>,
    pub files_list: ListState,
    pub rows_list: ListState,
}

#[derive(Default)]
pub struct FilesPane {
    pub dir: String,
    pub entries: Option<Vec<FileEntry>>,
    pub error: Option<String>,
    pub open: Option<String>,
    pub file: Option<FileContent>,
    pub file_error: Option<String>,
    pub lines: Vec<String>,
    pub list: ListState,
    pub view: ListState,
}

#[derive(Default)]
pub struct TermPane {
    pub selected: Option<String>,
    pub raw: String,
    pub lines: Vec<String>,
    pub exit: Option<Option<i64>>,
    pub live: bool,
    pub input: TextEdit,
    pub note: Option<String>,
    pub list: ListState,
    generation: Arc<AtomicU64>,
}

pub struct NewRun {
    pub providers: Option<Vec<ProviderInfo>>,
    pub projects: Vec<ProjectRecord>,
    pub provider: usize,
    pub model: Option<usize>,
    pub mode: Option<usize>,
    pub target: usize,
    pub prompt: TextEdit,
    pub busy: bool,
    pub problem: Option<String>,
    focus: bool,
}

pub struct Agents {
    api: Option<Arc<dyn Api>>,
    tx: Option<Sender<Msg>>,
    /// One request at a time, in order. Reads that can take seconds (the board, a diff) have their own.
    queue: Option<mpsc::Sender<Work>>,
    slow: Option<mpsc::Sender<Work>>,
    sign_in: Option<SignInView>,
    start_sign_in: Option<Box<dyn FnMut() -> SignInView>>,
    make_api: Option<Box<dyn FnMut() -> Option<Arc<dyn Api>>>>,
    pub store: Store,
    pub loaded: bool,
    pub load_error: Option<String>,
    pub health: Health,
    connected: bool,
    lost_at: Option<Instant>,
    next_poll: Option<Instant>,
    last_id: Arc<Mutex<Option<String>>>,
    pub section: Section,
    pub agent: Option<String>,
    pub tab: Tab,
    pub missing: bool,
    pub query: TextEdit,
    pub show_archived: bool,
    pub remote: Option<Vec<SearchResult>>,
    pub search_error: Option<String>,
    search_at: Option<Instant>,
    pub search_available: bool,
    pub prompts_available: bool,
    pub terminals_available: bool,
    pub schedules_available: bool,
    pub detach_available: bool,
    pub rows: Vec<ListRow>,
    pub agents_list: ListState,
    pub main: ListState,
    pub lines: Vec<Line>,
    stale: bool,
    soft: bool,
    built_at: Option<Instant>,
    pub pace: Duration,
    pub expanded: HashSet<u64>,
    seen: Vec<String>,
    closed: BTreeMap<String, String>,
    busy: HashSet<String>,
    pub composer: TextEdit,
    pub composer_busy: bool,
    pub note: Option<String>,
    pub follow: bool,
    follow_selected: Option<usize>,
    last_scroll: i32,
    last_h: i32,
    pub schedules: Option<Vec<Schedule>>,
    schedule_problem: Option<String>,
    schedule_busy: Option<String>,
    usage_snaps: Vec<AgentSnapshot>,
    usage_report: Option<UsageReport>,
    usage_loading: bool,
    usage_problem: Option<String>,
    pub board: Option<PipelineBoard>,
    board_problem: Option<String>,
    board_at: Option<Instant>,
    board_when: Option<String>,
    board_loading: bool,
    pub diff: DiffPane,
    pub files: FilesPane,
    pub term: TermPane,
    pub scripts: Vec<Script>,
    services: Vec<Service>,
    scripts_error: Option<String>,
    pub rename: Option<TextEdit>,
    pub prompts: Option<(Option<Vec<PromptEntry>>, ListState)>,
    pub new_run: Option<NewRun>,
    focus: Option<&'static str>,
    refocus: u8,
    modal_was: bool,
    sample: bool,
    pub fixed_now: Option<i64>,
}

fn clock_now() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_millis() as i64)
}

fn describe(e: &ApiError) -> String {
    e.to_string()
}

fn lax_list<T: DeserializeOwned>(v: &Value) -> Vec<T> {
    // One row a newer server added a kind to must not lose the rest.
    v.as_array().map(|a| a.iter().filter_map(|x| serde_json::from_value(x.clone()).ok()).collect()).unwrap_or_default()
}

struct Page {
    epoch: String,
    rows: Vec<TimelineRow>,
    count: usize,
}

impl Agents {
    pub fn new(api: Option<Arc<dyn Api>>, tx: Option<Sender<Msg>>) -> Agents {
        Agents {
            api,
            queue: tx.clone().map(worker),
            slow: tx.clone().map(worker),
            tx,
            sign_in: None,
            start_sign_in: None,
            make_api: None,
            store: Store::default(),
            loaded: false,
            load_error: None,
            health: Health::Live,
            connected: false,
            lost_at: None,
            next_poll: None,
            last_id: Arc::default(),
            section: Section::Runs,
            agent: None,
            tab: Tab::Chat,
            missing: false,
            query: TextEdit::new().placeholder("Search agents and conversations"),
            show_archived: false,
            remote: None,
            search_error: None,
            search_at: None,
            search_available: true,
            prompts_available: true,
            terminals_available: false,
            schedules_available: true,
            detach_available: true,
            rows: Vec::new(),
            agents_list: ListState::new(),
            main: ListState::new(),
            lines: Vec::new(),
            stale: true,
            soft: false,
            built_at: None,
            pace: STREAM_PACE,
            expanded: HashSet::new(),
            seen: Vec::new(),
            closed: BTreeMap::new(),
            busy: HashSet::new(),
            composer: TextEdit::new(),
            composer_busy: false,
            note: None,
            follow: true,
            follow_selected: None,
            last_scroll: 0,
            last_h: 0,
            schedules: None,
            schedule_problem: None,
            schedule_busy: None,
            usage_snaps: Vec::new(),
            usage_report: None,
            usage_loading: false,
            usage_problem: None,
            board: None,
            board_problem: None,
            board_at: None,
            board_when: None,
            board_loading: false,
            diff: DiffPane::default(),
            files: FilesPane::default(),
            term: TermPane::default(),
            scripts: Vec::new(),
            services: Vec::new(),
            scripts_error: None,
            rename: None,
            prompts: None,
            new_run: None,
            focus: Some("agents"),
            refocus: 0,
            modal_was: false,
            sample: false,
            fixed_now: None,
        }
    }

    pub fn sample(mut self) -> Agents {
        self.sample = true;
        self
    }

    pub fn with_sign_in(mut self, start_sign_in: Box<dyn FnMut() -> SignInView>, make_api: Box<dyn FnMut() -> Option<Arc<dyn Api>>>) -> Agents {
        self.start_sign_in = Some(start_sign_in);
        self.make_api = Some(make_api);
        if self.api.is_none() {
            self.sign_in = self.start_sign_in.as_mut().map(|f| f());
        }
        self
    }

    fn now(&self) -> i64 {
        self.fixed_now.unwrap_or_else(clock_now)
    }

    /// A request off the screen's thread, its answer parsed there too; `then` runs on this thread. In tests there is no thread.
    fn call<T: DeserializeOwned + Send + 'static>(&mut self, method: &'static str, path: String, body: Option<Value>, then: impl FnOnce(&mut Agents, Result<T, ApiError>) + Send + 'static) {
        self.call_on(false, method, path, body, then);
    }

    fn call_on<T: DeserializeOwned + Send + 'static>(&mut self, slow: bool, method: &'static str, path: String, body: Option<Value>, then: impl FnOnce(&mut Agents, Result<T, ApiError>) + Send + 'static) {
        let Some(api) = self.api.clone() else { return };
        let work: Work = Box::new(move || -> Apply {
            let r = api.call(method, &path, body.as_ref()).and_then(|v| serde_json::from_value::<T>(v).map_err(|e| ApiError::Protocol(e.to_string())));
            Box::new(move |a: &mut Agents| {
                if let Err(ApiError::Unauthorized(_)) = &r {
                    a.signed_out();
                } else {
                    then(a, r);
                }
            })
        });
        self.submit(slow, work);
    }

    fn submit(&mut self, slow: bool, work: Work) {
        match if slow { &self.slow } else { &self.queue } {
            Some(q) => {
                let _ = q.send(work);
            }
            None => {
                let f = work();
                f(self);
            }
        }
    }

    fn signed_out(&mut self) {
        self.api = None;
        crate::account::rejected();
        self.sign_in = self.start_sign_in.as_mut().map(|f| f());
    }

    // ---- Loading: `refreshAll` and friends ----

    /// The first look, and the stream.
    pub fn start(&mut self) {
        self.refresh_all();
        self.open_stream();
        self.detect_schedules();
    }

    pub fn refresh_all(&mut self) {
        self.load_error = None;
        self.call("GET", format!("{B}/items"), None, |a, r: Result<Value, _>| match r {
            Ok(v) => {
                a.store.set_items(lax_list(&v["agents"]));
                a.loaded = true;
                a.mark_stale();
            }
            Err(e) => {
                a.load_error = Some(describe(&e));
                a.loaded = true;
                a.mark_stale();
            }
        });
        self.load_permissions();
        self.load_workspaces();
        self.load_terminals();
        // Only the one on screen: the others are read when opened, for the account's limit.
        if let Some(id) = self.agent.clone() {
            self.load_snapshot(id.clone());
            self.load_timeline(id);
        }
    }

    fn load_permissions(&mut self) {
        self.call("GET", format!("{B}/permissions?all=true"), None, |a, r: Result<Value, _>| {
            if let Ok(v) = r {
                let list: Vec<PermissionView> = lax_list(&v["permissions"]);
                a.store.permissions = list.into_iter().map(|p| (p.key(), p)).collect();
                a.remember_seen();
                a.mark_stale();
            }
        });
    }

    fn load_workspaces(&mut self) {
        self.call("GET", format!("{B}/workspaces"), None, |a, r: Result<Value, _>| {
            if let Ok(v) = r {
                let list: Vec<Workspace> = lax_list(&v["workspaces"]);
                a.store.workspaces = list.into_iter().map(|w| (w.workspace_id.clone(), w)).collect();
                a.mark_stale();
            }
        });
    }

    fn load_terminals(&mut self) {
        self.call("GET", format!("{B}/terminals"), None, |a, r: Result<Value, ApiError>| match r {
            Ok(v) => {
                a.terminals_available = true;
                let list: Vec<Terminal> = lax_list(&v["terminals"]);
                a.store.terminals = list.into_iter().map(|t| (t.id.clone(), t)).collect();
                a.mark_stale();
            }
            Err(e) if not_deployed(&e) => a.terminals_available = false,
            Err(_) => {}
        });
    }

    fn detect_schedules(&mut self) {
        self.load_schedules();
    }

    fn load_snapshot(&mut self, id: String) {
        let path = format!("{B}/{}", enc(&id));
        let want = id;
        self.call("GET", path, None, move |a, r: Result<AgentSnapshot, ApiError>| match r {
            Ok(s) => {
                a.store.apply_snapshot(s);
                a.mark_stale();
            }
            Err(e) if e.status() == Some(404) => {
                if a.agent.as_deref() == Some(&want) {
                    a.missing = true;
                }
                a.mark_stale();
            }
            Err(_) => {}
        });
    }

    /// `loadTimeline`: all of it on the first call, then only what is new. A rebuilt timeline starts over.
    pub fn load_timeline(&mut self, id: String) {
        match self.store.timelines.get_mut(&id) {
            Some(t) if t.status == TlStatus::Loading => return,
            Some(t) => t.status = TlStatus::Loading,
            None => {
                self.store.timelines.insert(id.clone(), Timeline { epoch: String::new(), rows: Vec::new(), status: TlStatus::Loading, error: None });
            }
        }
        self.timeline_page(id, 0);
    }

    fn timeline_page(&mut self, id: String, guard: u32) {
        self.store.dirty.remove(&id);
        let after = self.store.timelines.get(&id).and_then(|t| t.rows.last()).map_or(0, |r| r.seq);
        let path = format!("{B}/{}/timeline?afterSeq={after}&limit={PAGE}", enc(&id));
        let want = id;
        let Some(api) = self.api.clone() else { return };
        let work: Work = Box::new(move || -> Apply {
            let r = api.call("GET", &path, None).map(|v| {
                let list = v["rows"].as_array().map_or(0, Vec::len);
                Page { epoch: v["epoch"].as_str().unwrap_or("").to_owned(), rows: lax_list(&v["rows"]), count: list }
            });
            Box::new(move |a: &mut Agents| a.timeline_answer(want, guard, r))
        });
        self.submit(false, work);
    }

    fn timeline_answer(&mut self, id: String, guard: u32, r: Result<Page, ApiError>) {
        let page = match r {
            Ok(p) => p,
            Err(ApiError::Unauthorized(_)) => return self.signed_out(),
            Err(e) => {
                if let Some(t) = self.store.timelines.get_mut(&id) {
                    t.status = TlStatus::Error;
                    t.error = Some(describe(&e));
                }
                self.mark_stale();
                return;
            }
        };
        let Some(t) = self.store.timelines.get_mut(&id) else { return };
        if !t.epoch.is_empty() && page.epoch != t.epoch && !t.rows.is_empty() {
            t.epoch = page.epoch;
            t.rows.clear();
            return self.timeline_page(id, guard + 1);
        }
        t.epoch = page.epoch;
        crate::store::merge_rows(&mut t.rows, page.rows);
        if (page.count >= PAGE || self.store.dirty.contains(&id)) && guard < 60 {
            return self.timeline_page(id, guard + 1);
        }
        if let Some(t) = self.store.timelines.get_mut(&id) {
            t.status = TlStatus::Ready;
            t.error = None;
        }
        self.mark_stale();
    }

    // ---- The stream ----

    /// `openStream`: resumes with `Last-Event-ID`, and retries by itself.
    fn open_stream(&mut self) {
        let (Some(api), Some(tx)) = (self.api.clone(), self.tx.clone()) else { return };
        let last = self.last_id.clone();
        std::thread::spawn(move || {
            let mut wait = RETRY;
            loop {
                let id = last.lock().unwrap_or_else(|p| p.into_inner()).clone();
                match api.stream(&format!("{B}/stream"), id.as_deref()) {
                    Ok(mut s) => {
                        wait = RETRY;
                        tx.send(Msg::Apply(Box::new(Agents::mark_live)));
                        while let Ok(Some(b)) = s.next_block() {
                            if let Some(i) = &b.id {
                                *last.lock().unwrap_or_else(|p| p.into_inner()) = Some(i.clone());
                            }
                            tx.send(block_msg(&b));
                        }
                        tx.send(Msg::Apply(Box::new(Agents::mark_lost)));
                    }
                    Err(ApiError::Unauthorized(_)) => {
                        tx.send(Msg::Apply(Box::new(Agents::signed_out)));
                        return;
                    }
                    Err(_) => {
                        tx.send(Msg::Apply(Box::new(Agents::mark_lost)));
                        wait = (wait * 2).min(RETRY_MAX);
                    }
                }
                std::thread::sleep(wait);
            }
        });
    }

    /// `markLive`: the replay covers what the stream missed; a page that showed a problem reads again.
    pub fn mark_live(&mut self) {
        let was_away = self.health != Health::Live;
        self.connected = true;
        self.health = Health::Live;
        self.lost_at = None;
        self.next_poll = None;
        if was_away || self.load_error.is_some() {
            self.refresh_all();
        }
    }

    /// `markLost`: nothing shows for a drop shorter than `QUIET`.
    pub fn mark_lost(&mut self) {
        self.connected = false;
        if self.health == Health::Live && self.lost_at.is_none() {
            self.lost_at = Some(Instant::now());
        }
    }

    /// `checkBackend`: the REST routes tell a broken stream from a backend that is down.
    fn check_backend(&mut self) {
        self.lost_at = None;
        if self.connected {
            return;
        }
        if self.health == Health::Live {
            self.health = Health::Reconnecting;
        }
        self.next_poll = Some(Instant::now() + POLL);
        self.call("GET", format!("{B}/items"), None, |a, r: Result<Value, ApiError>| {
            if a.connected {
                return;
            }
            match r {
                Ok(v) => {
                    a.store.set_items(lax_list(&v["agents"]));
                    a.load_error = None;
                    a.health = Health::Polling;
                }
                Err(e) => {
                    a.load_error = Some(describe(&e));
                    a.health = Health::Down;
                }
            }
            a.mark_stale();
        });
        if self.health != Health::Down {
            self.load_permissions();
            if let Some(id) = self.agent.clone() {
                self.load_snapshot(id.clone());
                self.load_timeline(id);
            }
        }
    }

    pub fn banner(&self) -> Option<(&'static str, bool)> {
        match self.health {
            Health::Live => None,
            Health::Reconnecting => Some(("Reconnecting…", false)),
            Health::Polling => Some(("Live updates are paused. Refreshing every 15 seconds.", false)),
            Health::Down => Some(("Lucina Agents isn't answering. Trying again…", true)),
        }
    }

    fn mark_stale(&mut self) {
        self.stale = true;
    }

    fn remember_seen(&mut self) {
        for p in self.store.pending_permissions() {
            let k = p.key();
            if !self.seen.contains(&k) {
                self.seen.push(k);
            }
        }
    }

    // ---- Moving around ----

    pub fn switch(&mut self, s: Section) {
        self.section = s;
        self.main = ListState::new();
        self.note = None;
        self.seen.clear();
        self.remember_seen();
        self.mark_stale();
        self.focus = Some(if s == Section::Runs { "agents" } else { "main" });
        match s {
            Section::Schedules => self.load_schedules(),
            Section::Usage => self.load_usage(),
            Section::Pipeline => self.load_board(),
            _ => {}
        }
    }

    pub fn open_agent(&mut self, id: String) {
        self.section = Section::Runs;
        self.missing = false;
        self.note = None;
        self.tab = Tab::Chat;
        self.expanded.clear();
        self.closed.clear();
        self.seen.clear();
        self.remember_seen();
        self.composer.clear();
        self.follow = true;
        self.main = ListState::new();
        self.agent = Some(id.clone());
        self.term = TermPane { generation: self.term.generation.clone(), ..TermPane::default() };
        self.term.generation.fetch_add(1, Ordering::SeqCst);
        self.diff = DiffPane::default();
        self.files = FilesPane::default();
        self.scripts.clear();
        self.mark_stale();
        self.load_snapshot(id.clone());
        self.load_timeline(id);
        self.focus = Some("main");
    }

    pub fn tabs(&self) -> Vec<(Tab, &'static str)> {
        let mut out = vec![(Tab::Chat, "Chat")];
        if self.agent.as_ref().is_some_and(|a| self.store.workspace_of(a).is_some()) {
            out.push((Tab::Changes, "Changes"));
            out.push((Tab::Files, "Files"));
            if self.terminals_available {
                out.push((Tab::Terminal, "Terminal"));
                out.push((Tab::Scripts, "Scripts"));
            }
        }
        out
    }

    pub fn switch_tab(&mut self, tab: Tab) {
        self.tab = tab;
        self.note = None;
        self.main = ListState::new();
        self.mark_stale();
        match tab {
            Tab::Chat => self.focus = Some("main"),
            Tab::Changes => {
                self.focus = Some("diff-files");
                self.load_diff();
            }
            Tab::Files => {
                self.focus = Some("files");
                self.load_files();
            }
            Tab::Terminal => {
                self.focus = Some("term-input");
                self.pick_terminal();
            }
            Tab::Scripts => {
                self.focus = Some("main");
                self.load_scripts();
            }
        }
    }

    fn cycle_tab(&mut self, step: i32) {
        let tabs = self.tabs();
        let at = tabs.iter().position(|(t, _)| *t == self.tab).unwrap_or(0) as i32;
        let next = (at + step).rem_euclid(tabs.len() as i32) as usize;
        self.switch_tab(tabs[next].0);
    }

    fn workspace_id(&self) -> Option<String> {
        self.agent.as_ref().and_then(|a| self.store.workspace_of(a)).map(|w| w.workspace_id.clone())
    }

    // ---- Actions: each says what went wrong, or nothing ----

    fn act_then_note<T: DeserializeOwned + Send + 'static>(&mut self, method: &'static str, path: String, body: Option<Value>, then: impl FnOnce(&mut Agents, T) + Send + 'static) {
        self.note = None;
        self.call(method, path, body, move |a, r: Result<T, ApiError>| match r {
            Ok(v) => {
                then(a, v);
                a.mark_stale();
            }
            Err(e) => {
                a.note = Some(describe(&e));
                a.mark_stale();
            }
        });
    }

    /// `Composer`'s submit: Send while idle; while the agent runs, Steer adds to the running turn.
    pub fn send_prompt(&mut self) {
        let Some(id) = self.agent.clone() else { return };
        let prompt = self.composer.text.trim().to_owned();
        if prompt.is_empty() || self.composer_busy || self.store.archived(&id) {
            return;
        }
        self.composer_busy = true;
        self.note = None;
        self.follow = true;
        let body = if self.store.running(&id) { json!({"prompt": prompt, "activeTurnBehavior": "steer"}) } else { json!({"prompt": prompt}) };
        self.call("POST", format!("{B}/{}/send", enc(&id)), Some(body), |a, r: Result<Value, ApiError>| {
            a.composer_busy = false;
            match r {
                Ok(_) => a.composer.clear(),
                Err(e) => a.note = Some(describe(&e)),
            }
            a.mark_stale();
        });
    }

    pub fn stop_agent(&mut self) {
        let Some(id) = self.agent.clone() else { return };
        self.act_then_note("POST", format!("{B}/{}/stop", enc(&id)), None, |_, _: Value| {});
    }

    pub fn toggle_archive(&mut self) {
        let Some(id) = self.agent.clone() else { return };
        if self.store.archived(&id) {
            self.act_then_note("POST", format!("{B}/{}/unarchive", enc(&id)), None, |a, s: AgentSnapshot| a.store.apply_snapshot(s));
        } else {
            self.act_then_note("POST", format!("{B}/{}/archive", enc(&id)), None, move |a, _: Value| a.load_snapshot(id));
        }
    }

    fn detach(&mut self) {
        let Some(id) = self.agent.clone() else { return };
        self.note = None;
        self.call("POST", format!("{B}/{}/detach", enc(&id)), None, |a, r: Result<AgentSnapshot, ApiError>| {
            match r {
                Ok(s) => a.store.apply_snapshot(s),
                Err(e) if not_deployed(&e) => {
                    a.detach_available = false;
                    a.note = Some("Not available on this server yet.".to_owned());
                }
                Err(e) => a.note = Some(describe(&e)),
            }
            a.mark_stale();
        });
    }

    fn save_title(&mut self) {
        let (Some(id), Some(edit)) = (self.agent.clone(), self.rename.take()) else { return };
        let next = edit.text.trim().to_owned();
        let same = self.store.items.get(&id).and_then(|i| i.title.as_deref()) == Some(next.as_str());
        if !next.is_empty() && !same {
            self.act_then_note("PATCH", format!("{B}/{}", enc(&id)), Some(json!({"title": next})), |a, s: AgentSnapshot| a.store.apply_snapshot(s));
        }
    }

    /// `answerPermission`: 403, 404, 409 and 410 are told apart; all but 403 close the card.
    pub fn answer(&mut self, key: &str, allow: bool) {
        let Some(p) = self.store.permissions.get(key).cloned() else { return };
        if !self.busy.insert(key.to_owned()) {
            return;
        }
        self.note = None;
        let behavior = if allow { "allow" } else { "deny" };
        let path = match &p.agent_id {
            Some(a) => format!("{B}/{}/permissions/{}", enc(a), enc(&p.id)),
            None => format!("{B}/permissions/{}", enc(&p.id)),
        };
        let key = key.to_owned();
        self.mark_stale();
        self.call("POST", path, Some(json!({"behavior": behavior})), move |a, r: Result<Value, ApiError>| {
            a.busy.remove(&key);
            match r {
                Ok(_) => {
                    if let Some(x) = a.store.permissions.get_mut(&key).filter(|x| x.status == "pending") {
                        x.status = if allow { "allowed".into() } else { "denied".into() };
                    }
                }
                Err(e) => match e.status() {
                    Some(403) => a.note = Some("This device may not answer it.".to_owned()),
                    Some(409) => a.closed_by(key, "Already answered elsewhere."),
                    Some(410) => a.closed_by(key, "This request expired."),
                    Some(404) => a.closed_by(key, "This request is gone."),
                    _ => a.note = Some(describe(&e)),
                },
            }
            a.mark_stale();
        });
    }

    fn closed_by(&mut self, key: String, why: &str) {
        self.closed.insert(key, why.to_owned());
        self.load_permissions();
    }

    fn load_schedules(&mut self) {
        self.schedule_problem = None;
        self.call("GET", format!("{B}/schedules"), None, |a, r: Result<Value, ApiError>| {
            match r {
                Ok(v) => {
                    a.schedules_available = true;
                    a.schedules = Some(lax_list(&v["schedules"]));
                }
                Err(e) if not_deployed(&e) => a.schedules_available = false,
                Err(e) => {
                    a.schedule_problem = Some(describe(&e));
                    a.schedules.get_or_insert_with(Vec::new);
                }
            }
            a.mark_stale();
        });
    }

    fn schedule_action(&mut self, id: String, action: &'static str) {
        self.schedule_busy = Some(id.clone());
        self.schedule_problem = None;
        self.mark_stale();
        self.call("POST", format!("{B}/schedules/{}/{action}", enc(&id)), None, move |a, r: Result<Schedule, ApiError>| {
            a.schedule_busy = None;
            match r {
                Ok(next) => {
                    if let Some(x) = a.schedules.as_mut().and_then(|l| l.iter_mut().find(|x| x.id == id)) {
                        *x = next;
                    }
                }
                Err(e) => {
                    a.schedule_problem = Some(describe(&e));
                    a.load_schedules();
                }
            }
            a.mark_stale();
        });
    }

    fn load_usage(&mut self) {
        self.usage_loading = true;
        self.usage_problem = None;
        self.call("GET", B.to_owned(), None, |a, r: Result<Value, ApiError>| {
            a.usage_loading = false;
            if let Ok(v) = r {
                a.usage_snaps = lax_list(&v["agents"]);
            }
            a.mark_stale();
        });
        self.call("GET", format!("{B}/usage"), None, |a, r: Result<UsageReport, ApiError>| {
            match r {
                Ok(u) => a.usage_report = Some(u),
                Err(e) if not_deployed(&e) => a.usage_report = None,
                Err(e) => a.usage_problem = Some(describe(&e)),
            }
            a.mark_stale();
        });
    }

    fn load_board(&mut self) {
        if self.board_loading {
            return;
        }
        self.board_loading = true;
        self.board_at = Some(Instant::now());
        self.call_on(true, "GET", format!("{B}/pipeline/board"), None, |a, r: Result<PipelineBoard, ApiError>| {
            a.board_loading = false;
            match r {
                Ok(b) => {
                    a.board = Some(b);
                    a.board_problem = None;
                    a.board_when = Some(crate::api::iso_now(a.now()));
                }
                Err(e) => a.board_problem = Some(describe(&e)),
            }
            a.mark_stale();
        });
    }

    fn load_diff(&mut self) {
        let Some(ws) = self.workspace_id() else { return };
        let mode = ["all", "uncommitted", "base"][self.diff.mode];
        let ignore = if self.diff.ignore_ws { "&ignoreWhitespace=true" } else { "" };
        self.diff.loading = true;
        self.diff.error = None;
        self.call_on(true, "GET", format!("{B}/workspaces/{}/diff?mode={mode}{ignore}", enc(&ws)), None, |a, r: Result<DiffResult, ApiError>| {
            a.diff.loading = false;
            match r {
                Ok(d) => {
                    a.diff.selected = 0;
                    a.diff.files_list = ListState::new();
                    if !d.files.is_empty() {
                        a.diff.files_list.select(0);
                    }
                    a.diff.rows = d.files.first().map(diff_rows).unwrap_or_default();
                    a.diff.result = Some(d);
                    a.diff.rows_list = ListState::new();
                }
                Err(e) => a.diff.error = Some(describe(&e)),
            }
        });
    }

    pub fn pick_diff_file(&mut self, i: usize) {
        if let Some(f) = self.diff.result.as_ref().and_then(|d| d.files.get(i)) {
            self.diff.selected = i;
            self.diff.rows = diff_rows(f);
            self.diff.rows_list = ListState::new();
        }
    }

    fn load_files(&mut self) {
        let Some(ws) = self.workspace_id() else { return };
        let dir = self.files.dir.clone();
        self.files.error = None;
        self.call("GET", format!("{B}/workspaces/{}/files?path={}", enc(&ws), enc(&dir)), None, |a, r: Result<Value, ApiError>| match r {
            Ok(v) => {
                let mut e: Vec<FileEntry> = lax_list(&v["entries"]);
                // `entries`: folders first, then by name.
                e.sort_by(|x, y| (x.kind != "directory").cmp(&(y.kind != "directory")).then_with(|| x.name.to_lowercase().cmp(&y.name.to_lowercase())));
                a.files.list = ListState::new();
                if !e.is_empty() {
                    a.files.list.select(0);
                }
                a.files.entries = Some(e);
            }
            Err(err) => a.files.error = Some(describe(&err)),
        });
    }

    pub fn open_entry(&mut self, i: usize) {
        let Some(e) = self.files.entries.as_ref().and_then(|l| l.get(i)).cloned() else { return };
        if e.kind == "directory" {
            self.files.dir = e.path;
            self.files.open = None;
            self.files.file = None;
            self.load_files();
            return;
        }
        let Some(ws) = self.workspace_id() else { return };
        self.files.open = Some(e.path.clone());
        self.files.file = None;
        self.files.lines.clear();
        self.files.file_error = None;
        self.files.view = ListState::new();
        self.call("GET", format!("{B}/workspaces/{}/file?path={}&maxBytes=262144", enc(&ws), enc(&e.path)), None, |a, r: Result<FileContent, ApiError>| match r {
            Ok(f) => {
                if f.encoding != "base64" {
                    a.files.lines = f.content.as_deref().unwrap_or("").lines().map(str::to_owned).collect();
                }
                a.files.file = Some(f);
            }
            Err(err) => a.files.file_error = Some(describe(&err)),
        });
    }

    fn workspace_terminals(&self) -> Vec<Terminal> {
        let Some(ws) = self.workspace_id() else { return Vec::new() };
        self.store.terminals.values().filter(|t| t.workspace_id == ws).cloned().collect()
    }

    fn pick_terminal(&mut self) {
        let list = self.workspace_terminals();
        if self.term.selected.as_ref().is_some_and(|s| list.iter().any(|t| &t.id == s)) {
            return;
        }
        let next = list.first().map(|t| t.id.clone());
        self.select_terminal(next);
    }

    pub fn select_terminal(&mut self, id: Option<String>) {
        self.term.selected.clone_from(&id);
        self.term.raw.clear();
        self.term.lines.clear();
        self.term.exit = None;
        self.term.live = false;
        self.term.list = ListState::new();
        let generation = self.term.generation.fetch_add(1, Ordering::SeqCst) + 1;
        let (Some(id), Some(api), Some(tx)) = (id, self.api.clone(), self.tx.clone()) else { return };
        let gen_now = self.term.generation.clone();
        std::thread::spawn(move || {
            let mut wait = RETRY;
            while gen_now.load(Ordering::SeqCst) == generation {
                match api.stream(&format!("{B}/terminals/{}/stream", enc(&id)), None) {
                    Ok(mut s) => {
                        wait = RETRY;
                        tx.send(Msg::Apply(Box::new(|a| a.term.live = true)));
                        while let Ok(Some(b)) = s.next_block() {
                            if gen_now.load(Ordering::SeqCst) != generation {
                                return;
                            }
                            // An exited terminal has nothing more to say.
                            let exited = b.event == "exit";
                            tx.send(Msg::Apply(Box::new(move |a| a.term_block(&b))));
                            if exited {
                                return;
                            }
                        }
                        tx.send(Msg::Apply(Box::new(|a| a.term.live = false)));
                    }
                    Err(_) => {
                        tx.send(Msg::Apply(Box::new(|a| a.term.live = false)));
                        wait = (wait * 2).min(RETRY_MAX);
                    }
                }
                std::thread::sleep(wait);
            }
        });
    }

    /// `TerminalOutput`: a `snapshot`, then `output` bytes, then `exit`.
    pub fn term_block(&mut self, b: &Block) {
        let v: Value = serde_json::from_str(&b.data).unwrap_or(Value::Null);
        match b.event.as_str() {
            "snapshot" => self.term.raw = v["data"].as_str().unwrap_or("").to_owned(),
            "output" => {
                let bytes = model::base64(v["b64"].as_str().unwrap_or(""));
                self.term.raw.push_str(&String::from_utf8_lossy(&bytes));
                if self.term.raw.len() > TERM_MAX {
                    let mut cut = self.term.raw.len() - TERM_MAX;
                    while !self.term.raw.is_char_boundary(cut) {
                        cut += 1;
                    }
                    self.term.raw.drain(..cut);
                }
            }
            "exit" => self.term.exit = Some(v["exitCode"].as_i64()),
            _ => return,
        }
        self.term.lines = ansi_to_text(&self.term.raw).split('\n').map(str::to_owned).collect();
        self.term.list.scroll_to_end();
    }

    pub fn term_send_line(&mut self) {
        let Some(id) = self.term.selected.clone() else { return };
        let line = self.term.input.text.clone();
        self.term.input.clear();
        let keys = if line.is_empty() { json!("Enter") } else { json!([line, "Enter"]) };
        self.term_keys(&id, keys);
    }

    fn term_keys(&mut self, id: &str, keys: Value) {
        self.term.note = None;
        self.call("POST", format!("{B}/terminals/{}/input", enc(id)), Some(json!({"keys": keys})), |a, r: Result<Value, ApiError>| {
            if let Err(e) = r {
                a.term.note = Some(describe(&e));
            }
        });
    }

    fn new_terminal(&mut self) {
        let Some(ws) = self.workspace_id() else { return };
        self.term.note = None;
        self.call("POST", format!("{B}/terminals"), Some(json!({"workspaceId": ws})), |a, r: Result<Terminal, ApiError>| match r {
            Ok(t) => {
                let id = t.id.clone();
                a.store.terminals.insert(t.id.clone(), t);
                a.select_terminal(Some(id));
            }
            Err(e) => a.term.note = Some(describe(&e)),
        });
    }

    fn close_terminal(&mut self) {
        let Some(id) = self.term.selected.clone() else { return };
        self.call("DELETE", format!("{B}/terminals/{}", enc(&id)), None, move |a, r: Result<Value, ApiError>| match r {
            Ok(_) => {
                a.store.terminals.remove(&id);
                a.term.selected = None;
                a.pick_terminal();
            }
            Err(e) => a.term.note = Some(describe(&e)),
        });
    }

    fn load_scripts(&mut self) {
        let Some(ws) = self.workspace_id() else { return };
        self.scripts_error = None;
        self.call("GET", format!("{B}/workspaces/{}/scripts", enc(&ws)), None, |a, r: Result<Value, ApiError>| {
            match r {
                Ok(v) => a.scripts = lax_list(&v["scripts"]),
                Err(e) if not_deployed(&e) => a.scripts.clear(),
                Err(e) => a.scripts_error = Some(describe(&e)),
            }
            a.mark_stale();
        });
        self.call("GET", format!("{B}/workspaces/{}/services", enc(&ws)), None, |a, r: Result<Value, ApiError>| {
            if let Ok(v) = r {
                a.services = lax_list(&v["services"]);
                a.mark_stale();
            }
        });
    }

    fn script_action(&mut self, name: String, start: bool) {
        let Some(ws) = self.workspace_id() else { return };
        let action = if start { "start" } else { "stop" };
        self.scripts_error = None;
        self.call("POST", format!("{B}/workspaces/{}/scripts/{}/{action}", enc(&ws), enc(&name)), None, |a, r: Result<Value, ApiError>| {
            match r {
                Ok(_) => a.load_scripts(),
                Err(e) => a.scripts_error = Some(describe(&e)),
            }
            a.mark_stale();
        });
    }

    pub fn open_prompts(&mut self) {
        let Some(id) = self.agent.clone() else { return };
        self.prompts = Some((None, ListState::new()));
        self.call("GET", format!("{B}/{}/prompts", enc(&id)), None, |a, r: Result<Value, ApiError>| {
            let list: Vec<PromptEntry> = match r {
                Ok(v) => lax_list(&v["prompts"]),
                Err(e) => {
                    if not_deployed(&e) {
                        a.prompts_available = false;
                    }
                    Vec::new()
                }
            };
            if let Some((p, state)) = &mut a.prompts {
                if !list.is_empty() {
                    state.select(0);
                }
                *p = Some(list);
            }
        });
    }

    /// `jump`: to the owner's message in the conversation.
    pub fn jump_to_prompt(&mut self, preview: &str) {
        let head = preview.trim_end_matches('…');
        if let Some(i) = self.lines.iter().position(|l| matches!(l, Line::User(t) if t.starts_with(head))) {
            self.follow = false;
            self.main.select(i);
        }
        self.prompts = None;
        self.focus = Some("main");
    }

    pub fn open_new_run(&mut self) {
        self.new_run = Some(NewRun { providers: None, projects: Vec::new(), provider: 0, model: None, mode: None, target: 0, prompt: TextEdit::multiline().placeholder("What should the agent do?"), busy: false, problem: None, focus: true });
        self.load_workspaces();
        self.call("GET", format!("{B}/providers"), None, |a, r: Result<Vec<ProviderInfo>, ApiError>| {
            if let Some(n) = &mut a.new_run {
                match r {
                    Ok(p) => n.providers = Some(p),
                    Err(e) => {
                        n.providers = Some(Vec::new());
                        n.problem = Some(describe(&e));
                    }
                }
            }
        });
        self.call("GET", format!("{B}/projects"), None, |a, r: Result<Value, ApiError>| {
            if let (Some(n), Ok(v)) = (&mut a.new_run, r) {
                n.projects = lax_list(&v["projects"]);
            }
        });
    }

    /// `workspaces` and the new-worktree choices of the form, in the order it shows them.
    pub fn run_targets(&self) -> Vec<(String, String)> {
        let mut out: Vec<(String, String)> = self.store.workspaces.values().filter(|w| w.archived_at.is_none() && w.background != Some(true)).map(|w| (format!("ws:{}", w.workspace_id), w.label())).collect();
        if let Some(n) = &self.new_run {
            out.extend(n.projects.iter().map(|p| (format!("new:{}", p.project_id), format!("New worktree in {}", p.name))));
        }
        out
    }

    /// `NewRun`'s submit: a new worktree first if that was chosen, then the agent.
    pub fn start_run(&mut self) {
        let targets = self.run_targets();
        let Some(n) = &mut self.new_run else { return };
        let Some(p) = n.providers.as_ref().and_then(|l| l.get(n.provider)).cloned() else { return };
        let Some((choice, _)) = targets.get(n.target).cloned() else { return };
        if n.busy {
            return;
        }
        n.busy = true;
        n.problem = None;
        let prompt = n.prompt.text.trim().to_owned();
        let models: Vec<_> = p.models.iter().filter(|m| m.is_selectable != Some(false)).collect();
        let model = n.model.and_then(|i| models.get(i)).map(|m| m.id.clone());
        let mode = n.mode.and_then(|i| p.modes.get(i)).map(|m| m.id.clone());
        let body = move |cwd: String, ws: String| {
            let mut b = json!({"provider": p.id, "cwd": cwd, "workspaceId": ws});
            if !prompt.is_empty() {
                b["prompt"] = prompt.clone().into();
            }
            if let Some(m) = &model {
                b["model"] = m.clone().into();
            }
            if let Some(m) = &mode {
                b["modeId"] = m.clone().into();
            }
            b
        };
        if let Some(project) = choice.strip_prefix("new:") {
            let provider = n.providers.as_ref().and_then(|l| l.get(n.provider)).map(|x| x.id.clone()).unwrap_or_default();
            let mut w = json!({"isolation": "worktree", "projectId": project, "provider": provider});
            if !n.prompt.text.trim().is_empty() {
                w["prompt"] = n.prompt.text.trim().into();
            }
            self.call("POST", format!("{B}/workspaces"), Some(w), move |a, r: Result<Value, ApiError>| match r {
                Ok(v) => match serde_json::from_value::<Workspace>(v["workspace"].clone()) {
                    Ok(ws) => {
                        a.store.workspaces.insert(ws.workspace_id.clone(), ws.clone());
                        a.create_agent(body(ws.cwd, ws.workspace_id));
                    }
                    Err(e) => a.run_failed(e.to_string()),
                },
                Err(e) => a.run_failed(describe(&e)),
            });
        } else {
            let id = choice.strip_prefix("ws:").unwrap_or("");
            match self.store.workspaces.get(id).cloned() {
                Some(w) => self.create_agent(body(w.cwd, w.workspace_id)),
                None => self.run_failed("Pick a workspace.".to_owned()),
            }
        }
    }

    fn run_failed(&mut self, m: String) {
        if let Some(n) = &mut self.new_run {
            n.busy = false;
            n.problem = Some(m);
        }
    }

    fn create_agent(&mut self, body: Value) {
        self.call("POST", B.to_owned(), Some(body), |a, r: Result<AgentSnapshot, ApiError>| match r {
            Ok(s) => {
                let id = s.id.clone();
                a.store.apply_snapshot(s);
                a.new_run = None;
                a.open_agent(id);
            }
            Err(e) => a.run_failed(describe(&e)),
        });
    }

    // ---- Building what is on the screen ----

    fn rebuild_rows(&mut self) {
        let now = self.now();
        let q = self.query.text.trim().to_owned();
        let pending = |id: &str| self.store.permissions.values().filter(|p| p.agent_id.as_deref() == Some(id) && p.status == "pending").count();
        let row_of = |a: &AgentListItem| -> ListRow {
            let n = pending(&a.id);
            let label = status_label(a.bucket, a.status);
            ListRow {
                id: a.id.clone(),
                title: agent_title(a.title.as_deref(), &a.id, &a.cwd),
                sub: format!("{} · {} · {}", a.provider, model::base_name(&a.cwd), model::ago(&a.updated_at, now)),
                state: if n > 0 { format!("{label} ({n})") } else { label.to_owned() },
                bucket: a.bucket,
            }
        };
        self.rows = if let Some(found) = &self.remote {
            found
                .iter()
                .map(|r| match self.store.items.get(&r.agent_id) {
                    Some(a) => {
                        let mut row = row_of(a);
                        if let Some(m) = r.matches.first() {
                            row.sub.clone_from(&m.snippet);
                        }
                        row
                    }
                    None => ListRow {
                        id: r.agent_id.clone(),
                        title: agent_title(r.title.as_deref(), &r.agent_id, &r.cwd),
                        sub: r.matches.first().map_or_else(|| r.provider.clone(), |m| m.snippet.clone()),
                        state: r.status.clone(),
                        bucket: match r.status.as_str() {
                            "running" => Bucket::Running,
                            "error" => Bucket::Failed,
                            _ => Bucket::Done,
                        },
                    },
                })
                .collect()
        } else {
            self.store
                .agent_list(self.show_archived)
                .into_iter()
                .filter(|a| q.is_empty() || model::matches(&format!("{} {} {} {} {}", a.title.as_deref().unwrap_or(""), a.provider, a.model.as_deref().unwrap_or(""), a.cwd, a.short_id), &q))
                .map(row_of)
                .collect()
        };
        if self.agents_list.selected.is_none() && !self.rows.is_empty() {
            self.agents_list.select(0);
        }
    }

    fn rebuild(&mut self) {
        self.stale = false;
        self.soft = false;
        self.built_at = Some(Instant::now());
        self.rebuild_rows();
        let now = self.now();
        let lines = match (self.section, self.tab, &self.agent) {
            (Section::Runs, Tab::Chat, Some(id)) => pages::chat(&self.store, id, &self.expanded, &self.seen, &self.closed, &self.busy, now),
            (Section::Runs, Tab::Scripts, Some(_)) => pages::scripts(&self.scripts, &self.services, self.scripts_error.as_deref()),
            (Section::Runs, _, _) => Vec::new(),
            (Section::Permissions, ..) => pages::permissions(&self.store, &self.seen, &self.closed, &self.busy, now),
            (Section::Schedules, ..) => pages::schedules(self.schedules.as_deref(), self.schedules_available, self.schedule_problem.as_deref(), self.schedule_busy.as_deref(), now),
            (Section::Usage, ..) => pages::usage(&self.usage_snaps, self.usage_report.as_ref(), self.usage_loading, self.usage_problem.as_deref()),
            (Section::Pipeline, ..) => pages::pipeline(self.board.as_ref(), self.board_when.as_deref(), self.board_problem.as_deref(), now),
        };
        let grew = lines.len() != self.lines.len() || lines != self.lines;
        self.lines = lines;
        let chat = self.section == Section::Runs && self.tab == Tab::Chat;
        if grew {
            self.main.invalidate();
            if self.follow && chat {
                self.main.scroll_to_end();
            }
        }
        if let Some(i) = self.main.selected {
            if i >= self.lines.len() {
                self.main.selected = self.lines.len().checked_sub(1);
            }
        }
        if chat && self.follow {
            // The newest line, and never Allow or Deny: an Enter that was meant for the list must not answer anything.
            self.main.selected = self.lines.iter().rposition(|l| !matches!(l, Line::Button { .. }));
            self.follow_selected = self.main.selected;
        } else if self.main.selected.is_none() && !self.lines.is_empty() {
            let first = self.lines.iter().position(|l| matches!(l, Line::Row { act, .. } if *act != Act::None)).unwrap_or(0);
            self.main.select(first);
        }
    }

    pub fn do_act(&mut self, act: Act) {
        match act {
            Act::None => {}
            Act::OpenAgent(id) => self.open_agent(id),
            Act::Toggle(seq) => {
                if !self.expanded.remove(&seq) {
                    self.expanded.insert(seq);
                }
                self.follow = false;
                self.mark_stale();
            }
            Act::Answer { key, allow } => self.answer(&key, allow),
            Act::Schedule { id, action } => self.schedule_action(id, action),
            Act::Script { name, start } => self.script_action(name, start),
        }
    }

    /// For the tests: what the 250 ms wait in `view` would do.
    #[cfg(test)]
    pub fn search_now(&mut self) {
        self.run_search();
        self.mark_stale();
    }

    /// For the tests: what the wait after a lost stream does.
    #[cfg(test)]
    pub fn check_backend_now(&mut self) {
        self.check_backend();
        self.mark_stale();
    }

    #[cfg(test)]
    pub fn focus_nothing(&mut self) {
        self.focus = None;
    }

    fn run_search(&mut self) {
        self.search_at = None;
        let text = self.query.text.trim().to_owned();
        if text.chars().count() < 2 || !self.search_available {
            self.remote = None;
            return;
        }
        self.call("GET", format!("{B}/search?q={}&limit=20", enc(&text)), None, move |a, r: Result<Value, ApiError>| {
            if a.query.text.trim() != text {
                return;
            }
            match r {
                Ok(v) => {
                    a.remote = Some(lax_list(&v["results"]));
                    a.search_error = None;
                }
                Err(e) if not_deployed(&e) => a.search_available = false,
                Err(e) => a.search_error = Some(describe(&e)),
            }
            a.mark_stale();
        });
    }

    // ---- Views ----

    fn view_rail(&mut self, ui: &mut Ui) {
        let t = ui.theme();
        let rail = ui.cut_left(t.px(160.0));
        ui.fill(rail, t.bar);
        let open = self.store.pending_permissions().len();
        let mut chosen = None;
        let mut new_run = false;
        ui.within(rail.inset(t.px(8.0)), |ui| {
            if ui.primary_button("New run") {
                new_run = true;
            }
            ui.space(8.0);
            let perm = if open > 0 { format!("Permissions ({open})") } else { "Permissions".to_owned() };
            let items: [(Section, &str, bool); 5] = [(Section::Runs, "Runs", true), (Section::Permissions, &perm, true), (Section::Schedules, "Schedules", self.schedules_available), (Section::Usage, "Usage", true), (Section::Pipeline, "Pipeline", true)];
            for (i, (s, label, show)) in items.iter().enumerate() {
                if !*show {
                    continue;
                }
                let text = if self.section == *s { format!("› {label}") } else { (*label).to_owned() };
                if ui.button_id(("section", i), &text) {
                    chosen = Some(*s);
                }
            }
        });
        if new_run {
            self.open_new_run();
        }
        if let Some(s) = chosen {
            self.switch(s);
        }
    }

    fn view_agents(&mut self, ui: &mut Ui) {
        let t = ui.theme();
        let col = ui.cut_left(t.px(290.0));
        ui.fill(col, t.panel);
        let mut open = None;
        let mut search_changed = false;
        let mut archived_changed = false;
        ui.within(col.inset(t.px(6.0)), |ui| {
            if self.search_available {
                let r = ui.cut_top(t.px(32.0));
                search_changed = ui.text_field_at(r, "search", &mut self.query).changed;
                ui.space(4.0);
            }
            if ui.checkbox("Show archived", &mut self.show_archived) {
                archived_changed = true;
            }
            if let Some(e) = self.load_error.as_ref().or(self.search_error.as_ref()) {
                ui.wrapped(Face::Sans, t.pt(13.0), t.danger, e);
            }
            if self.rows.is_empty() {
                let text = if !self.loaded {
                    "Loading…"
                } else if !self.query.text.trim().is_empty() {
                    "Nothing matches."
                } else if self.show_archived {
                    "No archived agents."
                } else {
                    "No agents yet. Start a new run."
                };
                ui.dim(text);
            }
            let r = ui.area();
            if self.focus == Some("agents") && !self.rows.is_empty() {
                ui.focus("agents");
                self.focus = None;
            }
            let rows = &self.rows;
            let current = self.agent.clone();
            let res = ui.list("agents", &mut self.agents_list, r, rows.len(), 58.0, |ui, i, selected| {
                let row = ui.area();
                let x = &rows[i];
                if selected || current.as_deref() == Some(x.id.as_str()) {
                    ui.fill_round(Rect::new(row.x, row.y + 1, row.w, row.h - 2), t.px(6.0), t.selection);
                }
                let side = t.px(10.0);
                ui.fill_round(Rect::new(row.x + t.px(6.0), row.y + t.px(10.0), side, side), side / 2, bucket_colour(x.bucket, &t));
                let left = row.x + side + t.px(12.0);
                let w = row.right() - left - t.px(4.0);
                ui.text_in(Rect::new(left, row.y + t.px(3.0), w, t.px(22.0)), Face::Sans, t.pt(14.0), t.text, &x.title);
                ui.text_in(Rect::new(left, row.y + t.px(23.0), w, t.px(16.0)), Face::Sans, t.pt(11.0), if selected { t.item } else { t.dim }, &x.sub);
                ui.text_in(Rect::new(left, row.y + t.px(38.0), w, t.px(16.0)), Face::Sans, t.pt(11.0), bucket_colour(x.bucket, &t), &x.state);
            });
            if let Some(i) = res.activated {
                open = Some(rows[i].id.clone());
            }
        });
        if search_changed {
            self.search_at = Some(Instant::now());
            self.search_error = None;
            if self.query.text.trim().chars().count() < 2 {
                self.remote = None;
            }
            self.mark_stale();
        }
        if archived_changed {
            self.mark_stale();
            if self.show_archived {
                self.call("GET", format!("{B}/items?archived=true"), None, |a, r: Result<Value, ApiError>| {
                    if let Ok(v) = r {
                        let list: Vec<AgentListItem> = lax_list(&v["agents"]);
                        for x in list {
                            a.store.items.insert(x.id.clone(), x);
                        }
                        a.mark_stale();
                    }
                });
            }
        }
        if let Some(id) = open {
            self.open_agent(id);
        }
    }

    fn view_detail(&mut self, ui: &mut Ui) {
        let t = ui.theme();
        let Some(id) = self.agent.clone() else {
            let a = ui.area();
            ui.within(Rect::new(a.x + t.px(20.0), a.y + t.px(20.0), a.w - t.px(40.0), a.h), |ui| ui.dim("Pick an agent, or start a new run."));
            return;
        };
        if self.missing {
            let a = ui.area();
            ui.within(Rect::new(a.x + t.px(20.0), a.y + t.px(20.0), a.w - t.px(40.0), a.h), |ui| ui.wrapped(Face::Sans, t.pt(14.0), t.danger, "No agent with that id."));
            return;
        }
        let pane = ui.area();
        let pane = Rect::new(pane.x + t.px(10.0), pane.y + t.px(4.0), pane.w - t.px(16.0), pane.h - t.px(6.0));
        let item = self.store.items.get(&id).cloned();
        let archived = self.store.archived(&id);
        let running = self.store.running(&id);
        let parent = self.store.snapshots.get(&id).map(|s| &s.labels).or(item.as_ref().map(|i| &i.labels)).and_then(|l| l.get("lucina.parent-agent-id")).is_some();
        let tabs = self.tabs();
        let ws = self.store.workspace_of(&id).map(Workspace::label);
        let has_rows = self.store.timelines.get(&id).is_some_and(|t| !t.rows.is_empty());
        let (mut rename, mut stop, mut archive, mut detach, mut prompts, mut tab) = (false, false, false, false, false, None);
        ui.within(pane, |ui| {
            let head = ui.cut_top(t.px(26.0));
            let title = item.as_ref().map_or_else(|| id.chars().take(7).collect(), |a| agent_title(a.title.as_deref(), &a.id, &a.cwd));
            ui.text_in(head, Face::Bold, t.pt(16.0), t.text, &title);
            if let Some(a) = &item {
                let label = status_label(a.bucket, a.status);
                let w = ui.measure(Face::Sans, t.pt(12.0), label);
                ui.text_in(Rect::new(head.right() - w, head.y, w, head.h), Face::Sans, t.pt(12.0), bucket_colour(a.bucket, &t), label);
                let mut meta = a.provider.clone();
                if let Some(m) = &a.model {
                    meta += &format!(" · {m}");
                }
                match &ws {
                    Some(w) => meta += &format!(" · {w}"),
                    None => meta += &format!(" · {}", a.cwd),
                }
                let r = ui.cut_top(t.px(18.0));
                ui.text_in(r, Face::Sans, t.pt(12.0), t.dim, &meta);
            }
            ui.row(30.0, |ui| {
                if ui.button("Rename") {
                    rename = true;
                }
                if self.prompts_available && has_rows && ui.button("Prompts") {
                    prompts = true;
                }
                if running && self.tab != Tab::Chat && ui.button_id("stop-head", "Stop") {
                    stop = true;
                }
                if parent && self.detach_available && ui.button("Detach from parent") {
                    detach = true;
                }
                if ui.button(if archived { "Unarchive" } else { "Archive" }) {
                    archive = true;
                }
            });
            if tabs.len() > 1 {
                ui.row(30.0, |ui| {
                    for (i, (x, label)) in tabs.iter().enumerate() {
                        let text = if self.tab == *x { format!("› {label}") } else { (*label).to_owned() };
                        if ui.button_id(("tab", i), &text) {
                            tab = Some(*x);
                        }
                    }
                });
            }
            if let Some(n) = &self.note {
                ui.wrapped(Face::Sans, t.pt(13.0), t.danger, n);
            }
            match self.tab {
                Tab::Chat => self.view_chat(ui, &id, archived, running),
                Tab::Changes => self.view_diff(ui),
                Tab::Files => self.view_files(ui),
                Tab::Terminal => self.view_terminal(ui),
                Tab::Scripts => self.view_lines(ui),
            }
        });
        if rename {
            let cur = item.as_ref().and_then(|a| a.title.clone()).unwrap_or_default();
            self.rename = Some(TextEdit::with_text(&cur));
        }
        if stop {
            self.stop_agent();
        }
        if archive {
            self.toggle_archive();
        }
        if detach {
            self.detach();
        }
        if prompts {
            self.open_prompts();
        }
        if let Some(x) = tab {
            self.switch_tab(x);
        }
    }

    /// The page list every line-based page shares.
    fn view_lines(&mut self, ui: &mut Ui) {
        let t = ui.theme();
        let r = ui.area();
        if self.focus == Some("main") && !self.lines.is_empty() {
            ui.focus("main");
            self.focus = None;
        }
        let lines = &self.lines;
        let res = ui.list_measured("main", &mut self.main, r, lines.len(), |text, i, w| height(text, &lines[i], w, &t), |ui, i, selected| draw(ui, &lines[i], selected));
        let act = res.activated.and_then(|i| lines[i].act().cloned());
        if let Some(a) = act {
            self.do_act(a);
        }
    }

    fn view_chat(&mut self, ui: &mut Ui, _id: &str, archived: bool, running: bool) {
        let t = ui.theme();
        let compose = ui.cut_bottom(t.px(42.0));
        let r = ui.area();
        self.view_lines(ui);
        // Scrolling up stops following; a send, End or reaching the end starts it again.
        if self.main.scroll < self.last_scroll && r.h == self.last_h && self.main.scroll != 0 {
            self.follow = false;
        }
        self.last_scroll = self.main.scroll;
        self.last_h = r.h;
        if self.follow && self.main.selected != self.follow_selected {
            self.follow = false;
        }
        let mut send = false;
        let mut stop = false;
        ui.within(compose, |ui| {
            if archived {
                ui.dim("Unarchive this agent to message it.");
                return;
            }
            let bw = t.px(74.0);
            let rx = compose.right() - if running { 2 * bw + t.px(6.0) } else { bw };
            self.composer.placeholder = if running { "Steer the agent while it works".to_owned() } else { "Message the agent".to_owned() };
            let res = ui.text_field_at(Rect::new(compose.x, compose.y + t.px(6.0), rx - compose.x - t.px(6.0), t.px(32.0)), "composer", &mut self.composer);
            if self.focus == Some("composer") {
                ui.focus("composer");
                self.focus = None;
            }
            if res.submitted {
                send = true;
            }
            if ui.button_at(Rect::new(rx, compose.y + t.px(6.0), bw, t.px(32.0)), "send", if running { "Steer" } else { "Send" }) {
                send = true;
            }
            if running && ui.button_at(Rect::new(rx + bw + t.px(6.0), compose.y + t.px(6.0), bw, t.px(32.0)), "stop", "Stop") {
                stop = true;
            }
        });
        if send {
            self.send_prompt();
        }
        if stop {
            self.stop_agent();
        }
    }

    fn view_diff(&mut self, ui: &mut Ui) {
        const MODES: [&str; 3] = ["All changes", "Uncommitted", "Since base"];
        let t = ui.theme();
        let mut mode = None;
        let mut reload = false;
        let totals = self.diff.result.as_ref().map(|d| (d.files.len(), d.files.iter().map(|f| f.additions).sum::<u64>(), d.files.iter().map(|f| f.deletions).sum::<u64>()));
        ui.row(30.0, |ui| {
            for (i, m) in MODES.iter().enumerate() {
                let text = if self.diff.mode == i { format!("› {m}") } else { (*m).to_owned() };
                if ui.button_id(("diff-mode", i), &text) {
                    mode = Some(i);
                }
            }
            if ui.checkbox("Ignore whitespace", &mut self.diff.ignore_ws) {
                reload = true;
            }
            if ui.button("Refresh") {
                reload = true;
            }
        });
        if let Some((n, add, del)) = totals.filter(|x| x.0 > 0) {
            ui.dim(&format!("{n} files +{add} −{del}"));
        }
        let mut pick = None;
        let (d, loading, error) = (self.diff.result.as_ref(), self.diff.loading, self.diff.error.as_deref());
        if let Some(e) = error {
            ui.wrapped(Face::Sans, t.pt(13.0), t.danger, e);
        }
        if let Some(e) = d.and_then(|d| d.error.as_deref()) {
            ui.wrapped(Face::Sans, t.pt(13.0), t.danger, e);
        }
        if d.is_some_and(|d| d.diff_too_large) {
            ui.wrapped(Face::Sans, t.pt(13.0), t.accent, "The diff is too large; some files are left out.");
        }
        match d.filter(|d| !d.files.is_empty()) {
            None => {
                if loading {
                    ui.dim("Loading…");
                } else if d.is_some() {
                    ui.dim("No changes.");
                }
            }
            Some(d) => {
                let left = ui.cut_left(t.px(200.0));
                let files = &d.files;
                if self.focus == Some("diff-files") {
                    ui.focus("diff-files");
                    self.focus = None;
                }
                let selected = self.diff.selected;
                let res = ui.list("diff-files", &mut self.diff.files_list, left, files.len(), 42.0, |ui, i, sel| {
                    let row = ui.area();
                    if sel || i == selected {
                        ui.fill_round(Rect::new(row.x, row.y + 1, row.w, row.h - 2), t.px(5.0), t.selection);
                    }
                    let f = &files[i];
                    ui.text_in(Rect::new(row.x + t.px(6.0), row.y + t.px(2.0), row.w - t.px(10.0), t.px(20.0)), Face::Sans, t.pt(13.0), t.text, model::base_name(&f.path));
                    let state = if f.is_new { " new" } else if f.is_deleted { " deleted" } else { "" };
                    ui.text_in(Rect::new(row.x + t.px(6.0), row.y + t.px(21.0), row.w - t.px(10.0), t.px(18.0)), Face::Sans, t.pt(11.0), t.dim, &format!("+{} −{}{state}", f.additions, f.deletions));
                });
                if res.selection_changed {
                    pick = self.diff.files_list.selected;
                }
                if let Some(i) = res.activated {
                    pick = Some(i);
                }
                let cur = files.get(selected);
                let head = ui.cut_top(t.px(22.0));
                if let Some(f) = cur {
                    let mut title = f.path.clone();
                    if let Some(o) = f.old_path.as_ref().filter(|o| **o != f.path) {
                        title += &format!(" ← {o}");
                    }
                    ui.text_in(Rect::new(head.x + t.px(8.0), head.y, head.w - t.px(8.0), head.h), Face::Mono, t.pt(12.0), t.item, &title);
                }
                let r = ui.area();
                let body = Rect::new(r.x + t.px(6.0), r.y, r.w - t.px(6.0), r.h);
                if cur.is_some_and(|f| f.status != "ok" && !f.status.is_empty()) {
                    let s = cur.map_or("", |f| f.status.as_str());
                    ui.within(body, |ui| ui.dim(if s == "binary" { "Binary file." } else { "This file's diff is too large to show." }));
                } else {
                    let rows = &self.diff.rows;
                    ui.list("diff-rows", &mut self.diff.rows_list, body, rows.len(), 19.0, |ui, i, sel| {
                        let row = ui.area();
                        let x = &rows[i];
                        let (bg, fg, sign) = match x.kind.as_str() {
                            "add" => (Some(Rgb::hex(0x103824)), t.text, "+"),
                            "remove" => (Some(Rgb::hex(0x461a1e)), t.text, "−"),
                            "hunk" => (None, t.accent, ""),
                            "header" => (None, t.dim, ""),
                            _ => (None, t.item, ""),
                        };
                        if let Some(c) = bg {
                            ui.fill(row, c);
                        }
                        if sel {
                            ui.fill(Rect::new(row.x, row.y, t.px(3.0), row.h), t.accent);
                        }
                        let nw = t.px(42.0);
                        ui.text_in(Rect::new(row.x + t.px(4.0), row.y, nw, row.h), Face::Mono, t.pt(11.0), t.dim, &x.n);
                        ui.text_in(Rect::new(row.x + nw, row.y, t.px(12.0), row.h), Face::Mono, t.pt(12.0), fg, sign);
                        ui.text_in(Rect::new(row.x + nw + t.px(12.0), row.y, row.w - nw - t.px(12.0), row.h), Face::Mono, t.pt(12.0), fg, &x.content);
                    });
                }
            }
        }
        if let Some(m) = mode {
            self.diff.mode = m;
            reload = true;
        }
        if let Some(i) = pick {
            self.pick_diff_file(i);
        }
        if reload {
            self.load_diff();
        }
    }

    fn view_files(&mut self, ui: &mut Ui) {
        let t = ui.theme();
        let mut up = false;
        let dir = self.files.dir.clone();
        ui.row(30.0, |ui| {
            if !dir.is_empty() && ui.button_id("files-up", "Up") {
                up = true;
            }
            ui.dim(if dir.is_empty() { "/" } else { &dir });
        });
        if let Some(e) = &self.files.error {
            ui.wrapped(Face::Sans, t.pt(13.0), t.danger, e);
        }
        let left = ui.cut_left(t.px(220.0));
        let mut open = None;
        match &self.files.entries {
            None => ui.within(left, |ui| ui.dim("Loading…")),
            Some(e) if e.is_empty() => ui.within(left, |ui| ui.dim("This folder is empty.")),
            Some(entries) => {
                if self.focus == Some("files") {
                    ui.focus("files");
                    self.focus = None;
                }
                let cur = self.files.open.clone();
                let res = ui.list("files", &mut self.files.list, left, entries.len(), 28.0, |ui, i, sel| {
                    let row = ui.area();
                    let e = &entries[i];
                    if sel || cur.as_deref() == Some(e.path.as_str()) {
                        ui.fill_round(Rect::new(row.x, row.y + 1, row.w, row.h - 2), t.px(4.0), t.selection);
                    }
                    let name = if e.kind == "directory" { format!("{}/", e.name) } else { e.name.clone() };
                    let size = if e.kind == "directory" { String::new() } else { size_text(e.size) };
                    let sw = ui.measure(Face::Sans, t.pt(11.0), &size);
                    ui.text_in(Rect::new(row.x + t.px(6.0), row.y, row.w - sw - t.px(14.0), row.h), Face::Sans, t.pt(13.0), if e.kind == "directory" { t.item } else { t.text }, &name);
                    ui.text_in(Rect::new(row.right() - sw - t.px(4.0), row.y, sw, row.h), Face::Sans, t.pt(11.0), t.dim, &size);
                });
                open = res.activated;
            }
        }
        let r = ui.area();
        let body = Rect::new(r.x + t.px(8.0), r.y, r.w - t.px(8.0), r.h);
        if let Some(path) = self.files.open.clone() {
            let head = Rect::new(body.x, body.y, body.w, t.px(22.0));
            ui.text_in(head, Face::Mono, t.pt(12.0), t.item, &path);
            let view = Rect::new(body.x, body.y + t.px(24.0), body.w, body.h - t.px(24.0));
            if let Some(e) = &self.files.file_error {
                ui.within(view, |ui| ui.wrapped(Face::Sans, t.pt(13.0), t.danger, e));
            } else if let Some(f) = &self.files.file {
                if f.encoding == "base64" || f.content.is_none() {
                    ui.within(view, |ui| ui.dim(&format!("Binary file, {}.", size_text(f.size))));
                } else {
                    let lines = &self.files.lines;
                    ui.list("file-view", &mut self.files.view, view, lines.len(), 19.0, |ui, i, sel| {
                        let row = ui.area();
                        if sel {
                            ui.fill(Rect::new(row.x, row.y, t.px(3.0), row.h), t.accent);
                        }
                        ui.text_in(Rect::new(row.x + t.px(6.0), row.y, row.w - t.px(6.0), row.h), Face::Mono, t.pt(12.0), t.text, &lines[i]);
                    });
                }
            } else {
                ui.within(view, |ui| ui.dim("Loading…"));
            }
        }
        if up {
            self.files.dir = model::parent_of(&dir);
            self.files.open = None;
            self.load_files();
        }
        if let Some(i) = open {
            self.open_entry(i);
        }
    }

    fn view_terminal(&mut self, ui: &mut Ui) {
        const KEYS: [(&str, &str); 7] = [("C-c", "Ctrl-C"), ("C-d", "Ctrl-D"), ("Tab", "Tab"), ("Escape", "Esc"), ("Up", "↑"), ("Down", "↓"), ("Enter", "Enter")];
        let t = ui.theme();
        let list = self.workspace_terminals();
        if self.term.selected.as_ref().is_none_or(|s| !list.iter().any(|x| &x.id == s)) && !list.is_empty() {
            self.pick_terminal();
        }
        let (mut new, mut close, mut next, mut key) = (false, false, false, None);
        let current = list.iter().find(|x| Some(&x.id) == self.term.selected.as_ref());
        ui.row(30.0, |ui| {
            if list.len() > 1 {
                let label = current.map_or_else(String::new, |x| {
                    let n = x.title.clone().unwrap_or_else(|| x.name.clone());
                    if x.status == "exited" { format!("{n} (exited)") } else { n }
                });
                if ui.button_id("term-next", &label) {
                    next = true;
                }
            }
            if ui.button("New terminal") {
                new = true;
            }
            if current.is_some() && ui.button("Close terminal") {
                close = true;
            }
        });
        if let Some(n) = &self.term.note {
            ui.wrapped(Face::Sans, t.pt(13.0), t.danger, n);
        }
        if current.is_none() {
            ui.dim("No terminal is open in this workspace.");
        } else {
            let status = match self.term.exit {
                Some(c) => format!("The terminal exited (code {}).", c.map_or_else(|| "?".to_owned(), |c| c.to_string())),
                None if self.term.live => "Connected".to_owned(),
                None => "Connecting…".to_owned(),
            };
            ui.dim(&status);
            let bottom = ui.cut_bottom(t.px(72.0));
            let r = ui.area();
            ui.fill(r, Rgb::hex(0x0a0c14));
            let lines = &self.term.lines;
            ui.list("term-output", &mut self.term.list, r, lines.len(), 18.0, |ui, i, _| {
                let row = ui.area();
                ui.text_in(Rect::new(row.x + t.px(6.0), row.y, row.w - t.px(6.0), row.h), Face::Mono, t.pt(12.0), t.text, &lines[i]);
            });
            let mut send = false;
            ui.within(bottom, |ui| {
                let field = ui.cut_top(t.px(36.0));
                let bw = t.px(70.0);
                if self.focus == Some("term-input") {
                    ui.focus("term-input");
                    self.focus = None;
                }
                self.term.input.placeholder = "Type a command".to_owned();
                let res = ui.text_field_at(Rect::new(field.x, field.y + t.px(4.0), field.w - bw - t.px(6.0), t.px(30.0)), "term-input", &mut self.term.input);
                if res.submitted {
                    send = true;
                }
                if ui.button_at(Rect::new(field.right() - bw, field.y + t.px(4.0), bw, t.px(30.0)), "term-send", "Send") {
                    send = true;
                }
                ui.row(30.0, |ui| {
                    for (i, (k, label)) in KEYS.iter().enumerate() {
                        if ui.button_id(("term-key", i), label) {
                            key = Some(*k);
                        }
                    }
                });
            });
            if send {
                self.term_send_line();
            }
        }
        if new {
            self.new_terminal();
        }
        if close {
            self.close_terminal();
        }
        if next {
            if let Some(i) = list.iter().position(|x| Some(&x.id) == self.term.selected.as_ref()) {
                self.select_terminal(Some(list[(i + 1) % list.len()].id.clone()));
            }
        }
        if let (Some(k), Some(id)) = (key, self.term.selected.clone()) {
            self.term_keys(&id, json!(k));
        }
    }

    fn view_page(&mut self, ui: &mut Ui, title: &str) {
        let t = ui.theme();
        let a = ui.area();
        ui.within(Rect::new(a.x + t.px(12.0), a.y + t.px(4.0), a.w - t.px(20.0), a.h - t.px(8.0)), |ui| {
            let head = ui.cut_top(t.px(28.0));
            ui.text_in(head, Face::Bold, t.pt(16.0), t.text, title);
            if let Some(n) = &self.note {
                ui.wrapped(Face::Sans, t.pt(13.0), t.danger, n);
            }
            self.view_lines(ui);
        });
    }

    fn view_new_run(&mut self, ui: &mut Ui) {
        let t = ui.theme();
        let targets = self.run_targets();
        let Some(n) = &mut self.new_run else { return };
        let (mut cancel, mut start) = (false, false);
        ui.modal(680.0, 480.0, |ui| {
            let head = ui.cut_top(t.px(28.0));
            ui.text_in(head, Face::Bold, t.pt(16.0), t.accent, "New run");
            let Some(list) = n.providers.clone() else {
                ui.dim("Loading…");
                return;
            };
            if let Some(p) = &n.problem {
                ui.wrapped(Face::Sans, t.pt(13.0), t.danger, p);
            }
            let cur = list.get(n.provider).cloned();
            let models: Vec<_> = cur.as_ref().map_or_else(Vec::new, |p| p.models.iter().filter(|m| m.is_selectable != Some(false)).cloned().collect());
            let modes = cur.as_ref().map_or_else(Vec::new, |p| p.modes.clone());
            ui.row(32.0, |ui| {
                if ui.button_id("nr-provider", &format!("Provider: {}", cur.as_ref().map_or("?", |p| p.label.as_str()))) && !list.is_empty() {
                    n.provider = (n.provider + 1) % list.len();
                    n.model = None;
                    n.mode = None;
                }
            });
            ui.row(32.0, |ui| {
                let label = n.model.and_then(|i| models.get(i)).map_or("Default", |m| m.label.as_str());
                if ui.button_id("nr-model", &format!("Model: {label}")) {
                    n.model = match n.model {
                        None if !models.is_empty() => Some(0),
                        Some(i) if i + 1 < models.len() => Some(i + 1),
                        _ => None,
                    };
                }
            });
            if !modes.is_empty() {
                ui.row(32.0, |ui| {
                    let label = n.mode.and_then(|i| modes.get(i)).map_or("Default", |m| m.label.as_str());
                    if ui.button_id("nr-mode", &format!("Mode: {label}")) {
                        n.mode = match n.mode {
                            None => Some(0),
                            Some(i) if i + 1 < modes.len() => Some(i + 1),
                            _ => None,
                        };
                    }
                });
            }
            ui.row(32.0, |ui| {
                let label = targets.get(n.target).map_or("?", |x| x.1.as_str());
                if ui.button_id("nr-target", &format!("Workspace: {label}")) && !targets.is_empty() {
                    n.target = (n.target + 1) % targets.len();
                }
            });
            ui.space(4.0);
            ui.dim("Prompt (Ctrl+Enter starts it)");
            if std::mem::take(&mut n.focus) {
                ui.focus("nr-prompt");
            }
            let res = ui.text_area("nr-prompt", &mut n.prompt, 5);
            if res.submitted {
                start = true;
            }
            ui.space(4.0);
            ui.row(34.0, |ui| {
                if n.busy {
                    ui.dim("Starting…");
                } else {
                    if ui.primary_button("Start") && cur.is_some() && !targets.is_empty() {
                        start = true;
                    }
                    if ui.button("Cancel") {
                        cancel = true;
                    }
                }
            });
            if ui.key(keys::KEY_ESC) {
                cancel = true;
            }
        });
        if cancel {
            self.new_run = None;
        }
        if start {
            self.start_run();
        }
    }

    fn view_rename(&mut self, ui: &mut Ui) {
        let t = ui.theme();
        let Some(e) = &mut self.rename else { return };
        let (mut save, mut cancel) = (false, false);
        ui.modal(520.0, 150.0, |ui| {
            let head = ui.cut_top(t.px(26.0));
            ui.text_in(head, Face::Bold, t.pt(15.0), t.accent, "Rename");
            ui.focus("rename");
            ui.row(32.0, |ui| {
                if ui.text_field("rename", e).submitted {
                    save = true;
                }
            });
            ui.space(4.0);
            if ui.key(keys::KEY_ESC) {
                cancel = true;
            }
        });
        if save {
            self.save_title();
        }
        if cancel {
            self.rename = None;
        }
    }

    fn view_prompts(&mut self, ui: &mut Ui) {
        let t = ui.theme();
        let Some((list, state)) = &mut self.prompts else { return };
        let mut jump = None;
        let mut close = false;
        ui.modal(640.0, 420.0, |ui| {
            let head = ui.cut_top(t.px(26.0));
            ui.text_in(head, Face::Bold, t.pt(15.0), t.accent, "Prompts");
            match list {
                None => ui.dim("Loading…"),
                Some(l) if l.is_empty() => ui.dim("No prompts."),
                Some(l) => {
                    ui.focus("prompt-list");
                    let r = ui.area();
                    let rows = &*l;
                    let res = ui.list("prompt-list", state, r, rows.len(), 32.0, |ui, i, sel| {
                        let row = ui.area();
                        if sel {
                            ui.fill_round(Rect::new(row.x, row.y + 1, row.w, row.h - 2), t.px(5.0), t.selection);
                        }
                        ui.text_in(Rect::new(row.x + t.px(6.0), row.y, row.w - t.px(10.0), row.h), Face::Sans, t.pt(13.0), t.text, &rows[i].preview);
                    });
                    if let Some(i) = res.activated {
                        jump = Some(rows[i].preview.clone());
                    }
                }
            }
            if ui.key(keys::KEY_ESC) {
                close = true;
            }
        });
        if let Some(p) = jump {
            self.jump_to_prompt(&p);
        }
        if close {
            self.prompts = None;
        }
    }
}

/// A thread that does one request at a time and hands the answers to the screen's thread.
fn worker(tx: Sender<Msg>) -> mpsc::Sender<Work> {
    let (jobs, rx) = mpsc::channel::<Work>();
    std::thread::spawn(move || {
        while let Ok(work) = rx.recv() {
            tx.send(Msg::Apply(work()));
        }
    });
    jobs
}

fn block_msg(b: &Block) -> Msg {
    // In lucina-agents-client, so CodeCor's stream reads blocks the same way.
    match lucina_agents_client::stream::block_event(b) {
        Some(v) => Msg::Bus(v),
        None => Msg::Changed,
    }
}

impl App for Agents {
    type Msg = Msg;

    fn message(&mut self, m: Msg) {
        match m {
            Msg::Changed => {}
            Msg::SignedIn => {
                self.sign_in = None;
                if self.api.is_none() {
                    self.api = self.make_api.as_mut().and_then(|f| f());
                }
                self.start();
            }
            Msg::Apply(f) => f(self),
            Msg::Bus(v) => {
                match self.store.apply_event(&v) {
                    Effect::None => {}
                    Effect::RefreshAll => self.refresh_all(),
                    Effect::LoadTimeline(id) => self.load_timeline(id),
                }
                self.remember_seen();
                self.soft = true;
            }
        }
    }

    fn view(&mut self, ui: &mut Ui) {
        ui.header(if self.sample { "Agents (sample data)" } else { "Agents" });
        if let Some(s) = &mut self.sign_in {
            s.view(ui, "The coding agents are in your account.");
            return;
        }
        if self.lost_at.is_some_and(|t| t.elapsed() >= QUIET) || self.next_poll.is_some_and(|t| Instant::now() >= t) {
            self.next_poll = None;
            self.check_backend();
        }
        if self.search_at.is_some_and(|t| t.elapsed() >= SEARCH_REST) {
            self.run_search();
        }
        if self.section == Section::Pipeline && self.board_at.is_none_or(|t| t.elapsed() >= BOARD_REFRESH) {
            self.load_board();
        }
        let pending = !self.store.pending_permissions().is_empty();
        let ticking = pending && (self.section == Section::Permissions || (self.section == Section::Runs && self.tab == Tab::Chat));
        let tick = if ticking { Duration::from_secs(1) } else { Duration::from_secs(30) };
        if self.built_at.is_none_or(|t| t.elapsed() >= tick) {
            self.stale = true;
        }
        if self.stale || (self.soft && self.built_at.is_none_or(|t| t.elapsed() >= self.pace)) {
            self.rebuild();
        }
        ui.wake_after(if self.soft {
            self.pace
        } else if self.search_at.is_some() {
            SEARCH_REST
        } else if self.lost_at.is_some() {
            Duration::from_secs(1)
        } else {
            tick
        });
        let modal = self.new_run.is_some() || self.rename.is_some() || self.prompts.is_some();
        if std::mem::replace(&mut self.modal_was, modal) && !modal {
            self.refocus = 2;
        }
        if self.refocus > 0 {
            self.refocus -= 1;
            self.focus = Some(if self.section == Section::Runs && self.agent.is_none() { "agents" } else { "main" });
        }

        if let Some((text, alert)) = self.banner() {
            let t = ui.theme();
            let r = ui.cut_top(t.px(24.0));
            ui.fill(r, t.panel_hi);
            ui.text_in(Rect::new(r.x + t.px(10.0), r.y, r.w - t.px(20.0), r.h), Face::Sans, t.pt(13.0), if alert { t.danger } else { t.item }, text);
        }
        self.view_rail(ui);
        match self.section {
            Section::Runs => {
                self.view_agents(ui);
                self.view_detail(ui);
            }
            Section::Permissions => self.view_page(ui, "Permissions"),
            Section::Schedules => self.view_page(ui, "Schedules"),
            Section::Usage => self.view_page(ui, "Usage"),
            Section::Pipeline => self.view_page(ui, "Pipeline"),
        }
        if self.new_run.is_some() {
            self.view_new_run(ui);
        } else if self.rename.is_some() {
            self.view_rename(ui);
        } else if self.prompts.is_some() {
            self.view_prompts(ui);
        } else {
            if ui.key(keys::KEY_ESC) {
                self.focus = Some("agents");
                if self.section != Section::Runs {
                    self.switch(Section::Runs);
                }
            }
            if ui.key(keys::KEY_N) {
                self.open_new_run();
            }
            if ui.key(keys::KEY_LEFTBRACE) && self.agent.is_some() {
                self.cycle_tab(-1);
            }
            if ui.key(keys::KEY_RIGHTBRACE) && self.agent.is_some() {
                self.cycle_tab(1);
            }
            if ui.key(keys::KEY_I) && self.agent.is_some() && self.tab == Tab::Chat {
                self.focus = Some("composer");
            }
            if ui.key(keys::KEY_END) {
                self.follow = true;
                self.main.scroll_to_end();
            }
            const SECTION_KEYS: [(u16, Section); 5] = [(keys::KEY_1, Section::Runs), (keys::KEY_2, Section::Permissions), (keys::KEY_3, Section::Schedules), (keys::KEY_4, Section::Usage), (keys::KEY_5, Section::Pipeline)];
            for (code, s) in SECTION_KEYS {
                if ui.key(code) {
                    self.switch(s);
                }
            }
        }
    }
}
