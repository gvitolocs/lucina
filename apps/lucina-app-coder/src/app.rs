//! CodeCor's screens, phases A to E: **Projects** (the repos agentd knows, "On buildhost", and the folders in
//! `/data/projects`, "On Juno"), **Project** (the agents labelled `codecor.project=<repo>`, the coordinator first with a
//! badge, then the workers), the **New agent** form (N; C sets the coordinator), which creates the worktree first and
//! then the agent, the **Agent** screen (Enter on a row: Chat and Changes, `[`/`]`, as the Agents app, with diff
//! approvals on permission cards), **Sessions** (S: the project's archived agents, Enter unarchives and opens one), and
//! the **local Agent** screen (Enter on an "On Juno" folder: `oc-cli acp` in CodeCor's own chat, `crate::acp`, with the
//! same Chat tab, permission cards and diffs; T shows the terminal line as a fallback). Phase D adds the intents
//! (`crate::intents`, listed by `--intents`), which call the same functions the screens call.
//! Keyboard first, as the Agents app: the arrows walk a list, Enter opens, Esc goes back. Requests go out on one worker
//! thread and come back as closures run on this thread (`Msg::Apply`); one stream keeps the store live, with its
//! back-off, and the lists are read again every `POLL`.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use lucina_account::ApiError;
use lucina_account::sse::Block;
use lucina_account::ui::SignInView;
use lucina_agents::timeline::TimelineRow;
use lucina_agents::types::{AgentListItem, AgentSnapshot};
use lucina_app::intent::{Entity, Reply, Responder};
use lucina_app::{App, Face, ListState, Rect, Sender, Ui, keys};
use serde::de::DeserializeOwned;
use serde_json::{Value, json};

use lucina_agents_client::agent::{AgentPane, PaneAct, Tab};
use lucina_agents_client::api::{self, Api, B, Blocks, Sample, enc};
use lucina_agents_client::diff::diff_path;
use lucina_agents_client::model::{
    self, DiffResult, PermissionView, ProjectRecord, ProviderInfo, Workspace, agent_title,
    diff_rows, status_label,
};
use lucina_agents_client::store::{Effect, Store, Timeline, TlStatus, merge_rows};
use lucina_agents_client::stream::block_event;
use lucina_agents_client::ui::bucket_colour;

use crate::acp::{self, AcpEvent};
use crate::new_agent::{self, NewAgentForm, Role};
use crate::projects::{self, LocalProject, ProjectKey, RemoteRow};

/// How often the lists are read again.
const POLL: Duration = Duration::from_secs(15);
/// `streamTiming`: nothing shows for a stream drop shorter than this.
const QUIET: Duration = Duration::from_secs(5);
/// The stream's retry, doubling up to `RETRY_MAX` (longer than the web's 15 s: the account's limit is shared by all apps).
const RETRY: Duration = Duration::from_secs(3);
const RETRY_MAX: Duration = Duration::from_secs(30);
/// A timeline page.
const PAGE: usize = 500;

pub type Apply = Box<dyn FnOnce(&mut CodeCor) + Send>;
type Work = Box<dyn FnOnce() -> Apply + Send>;

pub enum Msg {
    Changed,
    SignedIn,
    Bus(Value),
    Apply(Apply),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Screen {
    Projects,
    Project(ProjectKey),
    /// The Agent screen: one agent's Chat and Changes, opened from its row in the Project screen.
    Agent {
        project: ProjectKey,
        id: String,
    },
    /// The Sessions screen: the project's archived agents (S in the Project screen).
    Sessions(ProjectKey),
    /// Phase E: a folder on Juno opened in CodeCor's own chat, driven by `oc-cli acp` (`crate::local::start_acp`).
    Local(PathBuf),
}

/// `agentsBanner`'s states.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Health {
    Live,
    Reconnecting,
    Polling,
    Down,
}

struct Page {
    epoch: String,
    rows: Vec<TimelineRow>,
    count: usize,
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
    match block_event(b) {
        Some(v) => Msg::Bus(v),
        None => Msg::Changed,
    }
}

fn clock_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as i64)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Focus {
    Nezopt,
    Juno,
}

/// One row of the Project screen.
pub struct AgentRow {
    pub item: AgentListItem,
    pub coordinator: bool,
    pub branch: String,
    pub pending: usize,
}

/// Phase E: one "On Juno" folder's local agent, as an ACP session (`oc-cli acp`) whose events feed the store under
/// `id` (`local:<path>`), so the same Chat tab, permission cards and diffs draw as for the agents on buildhost.
pub struct LocalChat {
    pub path: PathBuf,
    /// The agent; `None` in the sample, which has canned events instead.
    pub agent: Option<crate::local::LocalAgent>,
    /// The store id the ACP updates map onto.
    pub id: String,
    /// A turn is running (Send is refused until Stop).
    pub running: bool,
    init_id: Option<i64>,
    new_id: Option<i64>,
    prompt_id: Option<i64>,
    /// The open `session/request_permission` request ids.
    open_requests: Vec<i64>,
}

pub struct CodeCor {
    api: Option<Arc<dyn Api>>,
    tx: Option<Sender<Msg>>,
    /// The one worker thread the requests go through (the account's limit is shared by all apps).
    queue: Option<mpsc::Sender<Work>>,
    sign_in: Option<SignInView>,
    start_sign_in: Option<Box<dyn FnMut() -> SignInView>>,
    #[allow(clippy::type_complexity)]
    make_api: Option<Box<dyn FnMut() -> Option<Arc<dyn Api>>>>,
    pub store: Store,
    pub projects: Vec<ProjectRecord>,
    pub local: Vec<LocalProject>,
    pub screen: Screen,
    pub buildhost: ListState,
    pub juno: ListState,
    pub focus: Focus,
    focus_want: Option<&'static str>,
    pub agents: ListState,
    /// The Agent screen's pane: Chat and Changes.
    pub pane: AgentPane,
    /// The Sessions screen's list.
    pub sessions: ListState,
    /// The stream's last event id, for the resume.
    last_id: Arc<Mutex<Option<String>>>,
    health: Health,
    connected: bool,
    lost_at: Option<Instant>,
    next_poll: Option<Instant>,
    /// The Agent screen's agent is gone from the server.
    pub missing: bool,
    /// For the tests and the sample: the clock.
    pub fixed_now: Option<i64>,
    /// The **New agent** form, open over the Project screen (N, or C for the coordinator).
    pub new_agent: Option<NewAgentForm>,
    pub note: Option<String>,
    pub load_error: Option<String>,
    /// Whether the first list has answered (the intents wait for it).
    pub loaded: bool,
    last_load: Option<Instant>,
    sample: bool,
    /// The intents whose answer waits for the store (docs/research/juno-intents.md); answered from `App::message`.
    pub pending: Vec<crate::intents::Pending>,
    /// Phase E: the local Agent screen's ACP session, while it is open.
    pub local_chat: Option<LocalChat>,
}

fn describe(e: &ApiError) -> String {
    e.to_string()
}

/// One row a newer server added a kind to must not lose the rest.
fn lax_list<T: DeserializeOwned>(v: &Value) -> Vec<T> {
    v.as_array()
        .map(|a| {
            a.iter()
                .filter_map(|x| serde_json::from_value(x.clone()).ok())
                .collect()
        })
        .unwrap_or_default()
}

impl CodeCor {
    pub fn new(api: Option<Arc<dyn Api>>, tx: Option<Sender<Msg>>) -> CodeCor {
        CodeCor {
            queue: tx.clone().map(worker),
            api,
            tx,
            sign_in: None,
            start_sign_in: None,
            make_api: None,
            store: Store::default(),
            projects: Vec::new(),
            local: Vec::new(),
            screen: Screen::Projects,
            buildhost: ListState::new(),
            juno: ListState::new(),
            focus: Focus::Nezopt,
            focus_want: Some("buildhost"),
            agents: ListState::new(),
            pane: AgentPane::new(),
            sessions: ListState::new(),
            last_id: Arc::default(),
            health: Health::Live,
            connected: false,
            lost_at: None,
            next_poll: None,
            missing: false,
            fixed_now: None,
            new_agent: None,
            note: None,
            load_error: None,
            loaded: false,
            last_load: None,
            sample: false,
            pending: Vec::new(),
            local_chat: None,
        }
    }

    /// Whether there is an account to read the agents from (the intents refuse without one).
    pub fn signed_in(&self) -> bool {
        self.api.is_some()
    }

    pub fn sample(mut self) -> CodeCor {
        self.sample = true;
        self.local = vec![
            LocalProject {
                name: "demo-app".into(),
                path: PathBuf::from("/data/projects/demo-app"),
            },
            LocalProject {
                name: "notes".into(),
                path: PathBuf::from("/data/projects/notes"),
            },
        ];
        self
    }

    pub fn with_sign_in(
        mut self,
        start_sign_in: Box<dyn FnMut() -> SignInView>,
        make_api: Box<dyn FnMut() -> Option<Arc<dyn Api>>>,
    ) -> CodeCor {
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

    /// A request off the screen's thread, its answer parsed there too; `then` runs on this thread. In tests there is no
    /// thread, so the work runs at once.
    fn call<T: DeserializeOwned + Send + 'static>(
        &mut self,
        method: &'static str,
        path: String,
        body: Option<Value>,
        then: impl FnOnce(&mut CodeCor, Result<T, ApiError>) + Send + 'static,
    ) {
        let Some(api) = self.api.clone() else { return };
        let work: Work = Box::new(move || {
            let r = api.call(method, &path, body.as_ref()).and_then(|v| {
                serde_json::from_value::<T>(v).map_err(|e| ApiError::Protocol(e.to_string()))
            });
            Box::new(move |a: &mut CodeCor| {
                if let Err(ApiError::Unauthorized(_)) = &r {
                    a.signed_out();
                } else {
                    then(a, r);
                }
            })
        });
        self.submit(work);
    }

    /// Onto the one worker thread, or at once when there is none.
    fn submit(&mut self, work: Work) {
        match &self.queue {
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

    // ---- Loading ----

    /// The first look: the lists, and the stream that keeps them live.
    pub fn start(&mut self) {
        self.refresh();
        self.open_stream();
    }

    pub fn refresh(&mut self) {
        self.last_load = Some(Instant::now());
        self.load_error = None;
        self.call(
            "GET",
            format!("{B}/projects"),
            None,
            |a, r: Result<Value, ApiError>| match r {
                Ok(v) => a.projects = lax_list(&v["projects"]),
                Err(e) => a.load_error = Some(describe(&e)),
            },
        );
        self.call(
            "GET",
            format!("{B}/items"),
            None,
            |a, r: Result<Value, ApiError>| match r {
                Ok(v) => {
                    a.store.set_items(lax_list(&v["agents"]));
                    a.loaded = true;
                }
                Err(e) => {
                    a.load_error = Some(describe(&e));
                    a.loaded = true;
                }
            },
        );
        self.load_permissions();
        self.call(
            "GET",
            format!("{B}/workspaces"),
            None,
            |a, r: Result<Value, ApiError>| {
                if let Ok(v) = r {
                    a.store.workspaces = lax_list::<Workspace>(&v["workspaces"])
                        .into_iter()
                        .map(|w| (w.workspace_id.clone(), w))
                        .collect();
                }
            },
        );
        if !self.sample {
            self.local = projects::local_projects(std::path::Path::new("/data/projects"));
        }
        // Only the one on screen: the others are read when opened, for the account's limit.
        if let Screen::Agent { id, .. } = self.screen.clone() {
            self.load_snapshot(id.clone());
            self.load_timeline(id);
        }
    }

    fn load_permissions(&mut self) {
        self.call(
            "GET",
            format!("{B}/permissions?all=true"),
            None,
            |a, r: Result<Value, ApiError>| {
                if let Ok(v) = r {
                    a.store.permissions = lax_list::<PermissionView>(&v["permissions"])
                        .into_iter()
                        .map(|p| (p.key(), p))
                        .collect();
                    a.remember_seen();
                }
            },
        );
    }

    fn load_snapshot(&mut self, id: String) {
        let path = format!("{B}/{}", enc(&id));
        let want = id;
        self.call(
            "GET",
            path,
            None,
            move |a, r: Result<AgentSnapshot, ApiError>| match r {
                Ok(s) => {
                    a.store.apply_snapshot(s);
                    a.pane.mark_stale();
                }
                Err(e) if e.status() == Some(404) => {
                    if matches!(&a.screen, Screen::Agent { id, .. } if *id == want) {
                        a.missing = true;
                    }
                    a.pane.mark_stale();
                }
                Err(_) => {}
            },
        );
    }

    /// `loadTimeline`: all of it on the first call, then only what is new. A rebuilt timeline starts over.
    pub fn load_timeline(&mut self, id: String) {
        match self.store.timelines.get_mut(&id) {
            Some(t) if t.status == TlStatus::Loading => return,
            Some(t) => t.status = TlStatus::Loading,
            None => {
                self.store.timelines.insert(
                    id.clone(),
                    Timeline {
                        epoch: String::new(),
                        rows: Vec::new(),
                        status: TlStatus::Loading,
                        error: None,
                    },
                );
            }
        }
        self.timeline_page(id, 0);
    }

    fn timeline_page(&mut self, id: String, guard: u32) {
        self.store.dirty.remove(&id);
        let after = self
            .store
            .timelines
            .get(&id)
            .and_then(|t| t.rows.last())
            .map_or(0, |r| r.seq);
        let path = format!("{B}/{}/timeline?afterSeq={after}&limit={PAGE}", enc(&id));
        let want = id;
        let Some(api) = self.api.clone() else { return };
        let work: Work = Box::new(move || -> Apply {
            let r = api.call("GET", &path, None).map(|v| {
                let list = v["rows"].as_array().map_or(0, Vec::len);
                Page {
                    epoch: v["epoch"].as_str().unwrap_or("").to_owned(),
                    rows: lax_list(&v["rows"]),
                    count: list,
                }
            });
            Box::new(move |a: &mut CodeCor| a.timeline_answer(want, guard, r))
        });
        self.submit(work);
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
                self.pane.mark_stale();
                return;
            }
        };
        let Some(t) = self.store.timelines.get_mut(&id) else {
            return;
        };
        if !t.epoch.is_empty() && page.epoch != t.epoch && !t.rows.is_empty() {
            t.epoch = page.epoch;
            t.rows.clear();
            return self.timeline_page(id, guard + 1);
        }
        t.epoch = page.epoch;
        merge_rows(&mut t.rows, page.rows);
        if (page.count >= PAGE || self.store.dirty.contains(&id)) && guard < 60 {
            return self.timeline_page(id, guard + 1);
        }
        if let Some(t) = self.store.timelines.get_mut(&id) {
            t.status = TlStatus::Ready;
            t.error = None;
        }
        self.pane.mark_stale();
    }

    // ---- The stream ----

    /// `openStream`: resumes with `Last-Event-ID`, and retries by itself.
    fn open_stream(&mut self) {
        let (Some(api), Some(tx)) = (self.api.clone(), self.tx.clone()) else {
            return;
        };
        let last = self.last_id.clone();
        std::thread::spawn(move || {
            let mut wait = RETRY;
            loop {
                let id = last.lock().unwrap_or_else(|p| p.into_inner()).clone();
                match api.stream(&format!("{B}/stream"), id.as_deref()) {
                    Ok(mut s) => {
                        wait = RETRY;
                        tx.send(Msg::Apply(Box::new(CodeCor::mark_live)));
                        while let Ok(Some(b)) = s.next_block() {
                            if let Some(i) = &b.id {
                                *last.lock().unwrap_or_else(|p| p.into_inner()) = Some(i.clone());
                            }
                            tx.send(block_msg(&b));
                        }
                        tx.send(Msg::Apply(Box::new(CodeCor::mark_lost)));
                    }
                    Err(ApiError::Unauthorized(_)) => {
                        tx.send(Msg::Apply(Box::new(CodeCor::signed_out)));
                        return;
                    }
                    Err(_) => {
                        tx.send(Msg::Apply(Box::new(CodeCor::mark_lost)));
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
            self.refresh();
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
        self.call(
            "GET",
            format!("{B}/items"),
            None,
            |a, r: Result<Value, ApiError>| {
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
                a.pane.mark_stale();
            },
        );
        if self.health != Health::Down {
            self.load_permissions();
            if let Screen::Agent { id, .. } = self.screen.clone() {
                self.load_snapshot(id.clone());
                self.load_timeline(id);
            }
        }
    }

    pub fn banner(&self) -> Option<(&'static str, bool)> {
        match self.health {
            Health::Live => None,
            Health::Reconnecting => Some(("Reconnecting…", false)),
            Health::Polling => Some((
                "Live updates are paused. Refreshing every 15 seconds.",
                false,
            )),
            Health::Down => Some(("Lucina Agents isn't answering. Trying again…", true)),
        }
    }

    fn remember_seen(&mut self) {
        for p in self.store.pending_permissions() {
            let k = p.key();
            if !self.pane.seen.contains(&k) {
                self.pane.seen.push(k);
            }
        }
    }

    // ---- Moving around ----

    pub fn open_project(&mut self, key: ProjectKey) {
        self.screen = Screen::Project(key);
        self.agents = ListState::new();
        self.agents.select(0);
        self.note = None;
        self.focus_want = Some("agents");
    }

    pub fn back(&mut self) {
        match self.screen.clone() {
            Screen::Agent { project, .. } | Screen::Sessions(project) => {
                self.screen = Screen::Project(project);
                self.focus_want = Some("agents");
            }
            Screen::Local(_) => {
                self.local_chat = None;
                self.screen = Screen::Projects;
                self.focus_want = Some("juno");
            }
            Screen::Project(_) => {
                self.screen = Screen::Projects;
                self.focus_want = Some(if self.focus == Focus::Nezopt {
                    "buildhost"
                } else {
                    "juno"
                });
            }
            Screen::Projects => {}
        }
        self.note = None;
    }

    /// Enter on an agent row: the Agent screen, its Chat and Changes.
    pub fn open_agent(&mut self, project: ProjectKey, id: String) {
        self.screen = Screen::Agent {
            project,
            id: id.clone(),
        };
        self.missing = false;
        self.note = None;
        self.pane.open();
        self.remember_seen();
        self.load_snapshot(id.clone());
        self.load_timeline(id);
        self.focus_want = Some("main");
    }

    /// S in the Project screen: the project's archived agents, from the list with `?archived=true`.
    pub fn open_sessions(&mut self, key: ProjectKey) {
        self.screen = Screen::Sessions(key);
        self.sessions = ListState::new();
        self.sessions.select(0);
        self.note = None;
        self.focus_want = Some("sessions");
        self.call(
            "GET",
            format!("{B}/items?archived=true"),
            None,
            |a, r: Result<Value, ApiError>| {
                if let Ok(v) = r {
                    a.store.set_items(lax_list(&v["agents"]));
                }
            },
        );
    }

    /// The Sessions screen's rows: the project's archived agents, newest first.
    pub fn sessions_rows(&self, key: &ProjectKey) -> Vec<AgentRow> {
        let ProjectKey::Remote(id) = key else {
            return Vec::new();
        };
        let Some(p) = self.projects.iter().find(|p| &p.project_id == id) else {
            return Vec::new();
        };
        let repo = model::base_name(&p.root_path);
        projects::archived_of_project(self.store.items.values(), repo)
            .into_iter()
            .map(|item| {
                let branch = self
                    .store
                    .workspace_of(&item.id)
                    .and_then(|w| w.branch.clone())
                    .unwrap_or_else(|| model::base_name(&item.cwd).to_owned());
                let coordinator = item
                    .labels
                    .get(projects::ROLE_LABEL)
                    .is_some_and(|v| v == "coordinator");
                AgentRow {
                    item,
                    coordinator,
                    branch,
                    pending: 0,
                }
            })
            .collect()
    }

    /// Enter on a session: unarchive it and open it. Sending a message resumes it (agentd resumes on send).
    pub fn unarchive_and_open(&mut self, project: ProjectKey, id: String) {
        self.call(
            "POST",
            format!("{B}/{}/unarchive", enc(&id)),
            None,
            move |a, r: Result<AgentSnapshot, ApiError>| match r {
                Ok(s) => {
                    a.store.apply_snapshot(s);
                    a.open_agent(project, id);
                }
                Err(e) => a.note = Some(describe(&e)),
            },
        );
    }

    // ---- Actions: each says what went wrong, or nothing ----

    /// The composer's Send.
    pub fn send_prompt(&mut self, id: &str) {
        let prompt = self.pane.composer.text.trim().to_owned();
        self.send_text(id, &prompt, None);
    }

    /// The send behind the composer's Send and `codecor.send`: while the agent runs, Steer adds to the running turn. An
    /// archived agent is not held back here — agentd resumes it on send; the intent refuses it before it gets here.
    /// `reply`, when the call came from an intent, is answered once the server has.
    pub fn send_text(&mut self, id: &str, text: &str, reply: Option<Responder>) {
        let prompt = text.trim().to_owned();
        if prompt.is_empty() || self.pane.composer_busy {
            if let Some(reply) = reply {
                reply.send(Reply::failed("There is nothing to send."));
            }
            return;
        }
        self.pane.composer_busy = true;
        self.note = None;
        self.pane.follow = true;
        let body = if self.store.running(id) {
            json!({"prompt": prompt, "activeTurnBehavior": "steer"})
        } else {
            json!({"prompt": prompt})
        };
        let id = id.to_owned();
        self.call(
            "POST",
            format!("{B}/{}/send", enc(&id)),
            Some(body),
            move |a, r: Result<Value, ApiError>| {
                a.pane.composer_busy = false;
                match r {
                    Ok(_) => {
                        a.pane.composer.clear();
                        if let Some(reply) = reply {
                            let title = a
                                .store
                                .items
                                .get(&id)
                                .map(|i| agent_title(i.title.as_deref(), &i.id, &i.cwd))
                                .unwrap_or_else(|| id.clone());
                            reply.send(
                                Reply::done(format!("Sent to {title}"))
                                    .with_entity(Entity::new("agent", &id, &title)),
                            );
                        }
                    }
                    Err(e) => {
                        let msg = describe(&e);
                        a.note = Some(msg.clone());
                        if let Some(reply) = reply {
                            reply.send(Reply::failed(msg));
                        }
                    }
                }
                a.pane.mark_stale();
            },
        );
    }

    pub fn stop_agent(&mut self, id: &str) {
        self.note = None;
        self.call(
            "POST",
            format!("{B}/{}/stop", enc(id)),
            None,
            |a, r: Result<Value, ApiError>| {
                if let Err(e) = r {
                    a.note = Some(describe(&e));
                }
                a.pane.mark_stale();
            },
        );
    }

    /// `answerPermission`: 403, 404, 409 and 410 are told apart; all but 403 close the card. Owner only, never through
    /// an intent.
    pub fn answer(&mut self, key: &str, allow: bool) {
        let Some(p) = self.store.permissions.get(key).cloned() else {
            return;
        };
        if !self.pane.busy.insert(key.to_owned()) {
            return;
        }
        self.note = None;
        let behavior = if allow { "allow" } else { "deny" };
        let path = match &p.agent_id {
            Some(a) => format!("{B}/{}/permissions/{}", enc(a), enc(&p.id)),
            None => format!("{B}/permissions/{}", enc(&p.id)),
        };
        let key = key.to_owned();
        self.pane.mark_stale();
        self.call(
            "POST",
            path,
            Some(json!({"behavior": behavior})),
            move |a, r: Result<Value, ApiError>| {
                a.pane.busy.remove(&key);
                match r {
                    Ok(_) => {
                        if let Some(x) = a
                            .store
                            .permissions
                            .get_mut(&key)
                            .filter(|x| x.status == "pending")
                        {
                            x.status = if allow {
                                "allowed".into()
                            } else {
                                "denied".into()
                            };
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
                a.pane.mark_stale();
            },
        );
    }

    fn closed_by(&mut self, key: String, why: &str) {
        self.pane.closed.insert(key, why.to_owned());
        self.load_permissions();
    }

    /// The Changes tab's read: `GET /v1/agents/workspaces/{ws}/diff?mode=…`.
    fn load_diff(&mut self, id: &str) {
        let Some(ws) = self.store.workspace_of(id).map(|w| w.workspace_id.clone()) else {
            return;
        };
        let mode = self.pane.diff.mode;
        let ignore_ws = self.pane.diff.ignore_ws;
        self.pane.diff.loading = true;
        self.pane.diff.error = None;
        self.call(
            "GET",
            diff_path(&ws, mode, ignore_ws),
            None,
            |a, r: Result<DiffResult, ApiError>| {
                a.pane.diff.loading = false;
                match r {
                    Ok(d) => {
                        a.pane.diff.selected = 0;
                        a.pane.diff.files_list = ListState::new();
                        if !d.files.is_empty() {
                            a.pane.diff.files_list.select(0);
                        }
                        a.pane.diff.rows = d.files.first().map(diff_rows).unwrap_or_default();
                        a.pane.diff.result = Some(d);
                        a.pane.diff.rows_list = ListState::new();
                    }
                    Err(e) => a.pane.diff.error = Some(describe(&e)),
                }
            },
        );
    }

    /// `[` and `]` walk the Agent screen's tabs.
    fn cycle_agent_tab(&mut self, step: i32, id: &str) {
        let has_ws = self.store.workspace_of(id).is_some();
        self.pane.cycle_tab(step, has_ws);
        if self.pane.tab == Tab::Changes {
            self.load_diff(id);
        }
    }

    /// The tab directly (for `--render`).
    pub fn switch_agent_tab(&mut self, id: &str, tab: Tab) {
        if self.pane.switch_tab(tab) {
            self.load_diff(id);
        }
    }

    /// The "On buildhost" rows, from what is loaded.
    pub fn remote_rows(&self) -> Vec<RemoteRow> {
        projects::remote_rows(&self.projects, &self.store.items)
    }

    /// The Project screen's rows: the coordinator first with its badge, then the workers by last activity.
    pub fn agent_rows(&self, key: &ProjectKey) -> Vec<AgentRow> {
        let ProjectKey::Remote(id) = key else {
            return Vec::new();
        };
        let Some(p) = self.projects.iter().find(|p| &p.project_id == id) else {
            return Vec::new();
        };
        let repo = model::base_name(&p.root_path);
        let pa = projects::agents_of_project(self.store.items.values(), repo);
        pa.all()
            .map(|item| {
                let branch = self
                    .store
                    .workspace_of(&item.id)
                    .and_then(|w| w.branch.clone())
                    .unwrap_or_else(|| model::base_name(&item.cwd).to_owned());
                let pending = self
                    .store
                    .permissions
                    .values()
                    .filter(|p| {
                        p.agent_id.as_deref() == Some(item.id.as_str()) && p.status == "pending"
                    })
                    .count();
                AgentRow {
                    item: item.clone(),
                    coordinator: pa.coordinator.as_ref().is_some_and(|c| c.id == item.id),
                    branch,
                    pending,
                }
            })
            .collect()
    }

    /// The warning line when several agents are labelled coordinator.
    pub fn coordinator_warning(&self, key: &ProjectKey) -> Option<String> {
        let ProjectKey::Remote(id) = key else {
            return None;
        };
        let p = self.projects.iter().find(|p| &p.project_id == id)?;
        projects::agents_of_project(self.store.items.values(), model::base_name(&p.root_path))
            .warning
    }

    /// "Open in terminal (local Qwen)": shows the line to type in Juno's terminal (F2). The shell has no request that
    /// types an app's command with arguments, on purpose (`open` refuses arguments for terminal programs), so CodeCor
    /// does not type it itself. See docs/research/juno-coder.md.
    pub fn open_in_terminal(&mut self, dir: &std::path::Path) {
        let exe = std::env::current_exe()
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_else(|_| "/usr/bin/lucina-app-coder".to_owned());
        let app_dir = std::env::var_os("LUCINA_APP_DIR")
            .map(|v| v.to_string_lossy().into_owned())
            .unwrap_or_else(|| "/usr/share/lucina/apps".to_owned());
        let line = crate::local::terminal_command_line(&exe, &app_dir, &dir.to_string_lossy());
        self.note = Some(format!("In the Terminal (F2), type: {}", line.trim_end()));
    }

    // ---- Phase E: the local Agent screen (`oc-cli acp`) ----

    /// Enter on an "On Juno" folder: the local Agent screen. The preparation (package copy to RAM, the machine's SSH
    /// key, the route check, the gateway key, the tunnel supervisor) runs off the screen's thread; a failure shows the
    /// same sentence `--terminal` prints.
    pub fn open_local(&mut self, path: PathBuf) {
        self.screen = Screen::Local(path.clone());
        self.focus_want = Some("main");
        self.note = None;
        self.pane.open();
        self.local_chat = None;
        let work = path.clone();
        let work2 = path.clone();
        self.submit(Box::new(move || -> Apply {
            let r = crate::local::start_acp(&work);
            Box::new(move |a: &mut CodeCor| match r {
                Ok(agent) => a.open_local_agent(work2, agent),
                Err(e) => {
                    a.note = Some(e);
                    a.pane.mark_stale();
                }
            })
        }));
    }

    /// The local Agent screen with a running agent: the store's local rows, then `initialize` and `session/new`.
    pub fn open_local_agent(&mut self, path: PathBuf, agent: crate::local::LocalAgent) {
        let id = format!("local:{}", path.display());
        let now = self.now();
        acp::ensure_local(&mut self.store, &id, &path, now);
        self.screen = Screen::Local(path.clone());
        self.local_chat = Some(LocalChat {
            path: path.clone(),
            agent: Some(agent),
            id: id.clone(),
            running: false,
            init_id: None,
            new_id: None,
            prompt_id: None,
            open_requests: Vec::new(),
        });
        self.pane.open();
        self.remember_seen();
        self.note = None;
        self.focus_want = Some("main");
        let (init, new) = {
            let chat = self.local_chat.as_mut().expect("just set");
            let path = chat.path.clone();
            let agent = chat.agent.as_mut().expect("just set");
            (
                agent.acp.initialize().ok(),
                agent.acp.new_session(&path).ok(),
            )
        };
        let chat = self.local_chat.as_mut().expect("just set");
        chat.init_id = init;
        chat.new_id = new;
        self.pane.mark_stale();
    }

    /// The sample's local Agent screen: canned ACP events, including a permission card that carries a diff.
    pub fn open_local_sample(&mut self, path: PathBuf) {
        let id = format!("local:{}", path.display());
        let now = self.now();
        acp::ensure_local(&mut self.store, &id, &path, now);
        for u in [
            json!({"sessionUpdate":"user_message_chunk","content":{"type":"text","text":"Add a retry to the upload"}}),
            json!({"sessionUpdate":"agent_thought_chunk","content":{"type":"text","text":"The upload fails once in a while; a retry around send() should fix it."}}),
            json!({"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"I'll add a retry in src/sample.rs."}}),
            json!({"sessionUpdate":"tool_call","toolCallId":"call_1","title":"edit","kind":"edit","status":"pending","rawInput":{"path":"src/sample.rs","oldString":"    send()","newString":"    retry(3, send)"}}),
        ] {
            acp::apply_update(&mut self.store, &id, &u, now);
        }
        acp::apply_permission(
            &mut self.store,
            &id,
            100,
            &json!({
                "toolCallId": "call_1", "title": "edit", "kind": "edit", "status": "pending",
                "rawInput": {"path": "src/sample.rs", "oldString": "    send()", "newString": "    retry(3, send)"},
                "content": [
                    {"type": "text", "text": "Adding a retry to the upload"},
                    {"type": "diff", "path": "/data/projects/demo-app/src/sample.rs",
                     "oldText": "fn upload() -> Result<(), Error> {\n    send()\n}",
                     "newText": "fn upload() -> Result<(), Error> {\n    retry(3, send)\n}"},
                ],
            }),
            now,
        );
        self.screen = Screen::Local(path.clone());
        self.local_chat = Some(LocalChat {
            path,
            agent: None,
            id: id.clone(),
            running: true,
            init_id: None,
            new_id: None,
            prompt_id: None,
            open_requests: vec![100],
        });
        self.pane.open();
        self.remember_seen();
        self.note = None;
        self.focus_want = Some("main");
        self.pane.mark_stale();
    }

    /// The agent's events that have arrived, onto the store. Called from `message` and `view`.
    pub fn pump_local(&mut self) {
        let mut events = Vec::new();
        if let Some(chat) = self.local_chat.as_mut()
            && let Some(agent) = chat.agent.as_mut()
        {
            while let Some(ev) = agent.acp.try_recv() {
                events.push(ev);
            }
        }
        if events.is_empty() {
            return;
        }
        let id = self
            .local_chat
            .as_ref()
            .map(|c| c.id.clone())
            .unwrap_or_default();
        let now = self.now();
        // A turn's end is a definite state change, not stream noise: draw its last events now rather than at the
        // stream's pace, so the transcript never shows "not running" with the answer still missing.
        let mut turn_ended = false;
        for ev in events {
            match ev {
                AcpEvent::Update(u) => acp::apply_update(&mut self.store, &id, &u, now),
                AcpEvent::Permission {
                    id: rid, tool_call, ..
                } => {
                    acp::apply_permission(&mut self.store, &id, rid, &tool_call, now);
                    if let Some(chat) = self.local_chat.as_mut() {
                        chat.open_requests.push(rid);
                    }
                }
                AcpEvent::Response {
                    id: rid,
                    result,
                    error,
                } => {
                    let mut note = None;
                    if let Some(chat) = self.local_chat.as_mut() {
                        if chat.init_id == Some(rid) {
                            chat.init_id = None;
                            if let Some(e) = &error {
                                note = Some(format!("The agent refused initialize: {e}"));
                            }
                        }
                        if chat.new_id == Some(rid) {
                            chat.new_id = None;
                            match result.as_ref().and_then(|r| r["sessionId"].as_str()) {
                                Some(sid) => {
                                    if let Some(agent) = chat.agent.as_mut() {
                                        agent.acp.session_id = Some(sid.to_owned());
                                    }
                                }
                                None => note = Some("The agent made no session.".to_owned()),
                            }
                        }
                        if chat.prompt_id == Some(rid) {
                            chat.prompt_id = None;
                            chat.running = false;
                            turn_ended = true;
                        }
                    }
                    if let Some(n) = note {
                        self.note = Some(n);
                    }
                }
                AcpEvent::Closed => {
                    let mut ended = false;
                    if let Some(chat) = self.local_chat.as_mut() {
                        ended = chat.running || chat.prompt_id.is_some();
                        chat.running = false;
                        chat.prompt_id = None;
                    }
                    if ended {
                        self.note = Some("The agent's process ended.".to_owned());
                    }
                }
            }
        }
        self.pane.mark_soft();
        if turn_ended {
            self.pane.mark_stale();
        }
    }

    /// The composer's Send on the local screen: `session/prompt`. One turn at a time.
    pub fn local_send(&mut self) {
        let text = self.pane.composer.text.trim().to_owned();
        if text.is_empty() {
            return;
        }
        enum Out {
            Sent(i64),
            Busy,
            NoSession,
            Err(String),
        }
        let out = match self.local_chat.as_mut() {
            Some(chat) if chat.running => Out::Busy,
            Some(chat) => match chat.agent.as_mut() {
                Some(agent) => match agent.acp.prompt(&text) {
                    Ok(Some(id)) => Out::Sent(id),
                    Ok(None) => Out::NoSession,
                    Err(e) => Out::Err(e.to_string()),
                },
                None => Out::NoSession,
            },
            None => return,
        };
        match out {
            Out::Sent(id) => {
                if let Some(chat) = self.local_chat.as_mut() {
                    chat.prompt_id = Some(id);
                    chat.running = true;
                }
                self.pane.composer.clear();
                self.pane.follow = true;
                self.note = None;
            }
            Out::Busy => self.note = Some("One turn at a time — Stop first.".to_owned()),
            Out::NoSession => self.note = Some("No session yet.".to_owned()),
            Out::Err(e) => self.note = Some(format!("Can't send: {e}")),
        }
        self.pane.mark_stale();
    }

    /// The composer's Stop: `session/cancel`, and the open permission requests are answered cancelled.
    pub fn local_stop(&mut self) {
        let requests = self
            .local_chat
            .as_ref()
            .map(|c| c.open_requests.clone())
            .unwrap_or_default();
        if let Some(chat) = self.local_chat.as_mut() {
            if let Some(agent) = chat.agent.as_mut() {
                for rid in &requests {
                    let _ = agent.acp.cancel_permission(*rid);
                }
                let _ = agent.acp.cancel();
            }
            chat.open_requests.clear();
            chat.running = false;
            chat.prompt_id = None;
        }
        self.pane.mark_stale();
    }

    /// Allow / Deny on a local permission card: the answer goes to that request id.
    pub fn local_answer(&mut self, key: &str, allow: bool) {
        let Some(id) = self.local_chat.as_ref().map(|c| c.id.clone()) else {
            return;
        };
        let Some(rest) = key.strip_prefix(&format!("{id}/")) else {
            return;
        };
        let Ok(rid) = rest.parse::<i64>() else {
            return;
        };
        if let Some(chat) = self.local_chat.as_mut() {
            if let Some(agent) = chat.agent.as_mut() {
                let _ = agent.acp.answer_permission(rid, allow);
            }
            chat.open_requests.retain(|r| *r != rid);
        }
        acp::answer_permission(&mut self.store, &id, rid, allow);
        if !self.pane.seen.contains(&key.to_owned()) {
            self.pane.seen.push(key.to_owned());
        }
        self.pane.busy.remove(key);
        self.pane.mark_stale();
    }

    // ---- New agent ----

    /// Whether the project already has a coordinator (at most one is offered).
    pub fn has_coordinator(&self, key: &ProjectKey) -> bool {
        let ProjectKey::Remote(id) = key else {
            return false;
        };
        let Some(p) = self.projects.iter().find(|p| &p.project_id == id) else {
            return false;
        };
        projects::agents_of_project(self.store.items.values(), model::base_name(&p.root_path))
            .coordinator
            .is_some()
    }

    /// N opens the form as a worker, C as the coordinator (only when the project has none). The providers come from
    /// `GET /v1/agents/providers`.
    pub fn open_new_agent(&mut self, key: ProjectKey, role: Role) {
        let ProjectKey::Remote(id) = &key else { return };
        let Some(p) = self.projects.iter().find(|p| &p.project_id == id).cloned() else {
            return;
        };
        let lucina =
            projects::is_lucina_project(&p.root_path, &projects::lucina_repos(&self.projects));
        let can_coordinator = !self.has_coordinator(&key);
        self.new_agent = Some(NewAgentForm::new(
            p.project_id.clone(),
            lucina,
            can_coordinator,
            role,
        ));
        self.call(
            "GET",
            format!("{B}/providers"),
            None,
            |a, r: Result<Vec<ProviderInfo>, ApiError>| {
                if let Some(f) = &mut a.new_agent {
                    match r {
                        Ok(list) => {
                            f.provider = new_agent::initial_provider(&list, f.lucina);
                            f.providers = Some(list);
                        }
                        Err(e) => {
                            f.providers = Some(Vec::new());
                            f.problem = Some(new_agent::server_message(&e));
                        }
                    }
                }
            },
        );
    }

    /// The form's Create: the checks and the fields, then [`CodeCor::create_agent`].
    pub fn submit_new_agent(&mut self) {
        let Some(f) = &mut self.new_agent else { return };
        if f.busy {
            return;
        }
        let Some(p) = f.provider_info().cloned() else {
            return;
        };
        if new_agent::codex_blocked(f.lucina, std::slice::from_ref(&p)).is_some() {
            // Codex cannot be chosen on a Lucina project.
            f.problem = Some(new_agent::CODEX_RULE_LINE.to_owned());
            return;
        }
        if let Some(line) = f.juno_problem() {
            f.problem = Some(line.to_owned());
            return;
        }
        f.busy = true;
        f.problem = None;
        let title = f.title.text.trim().to_owned();
        let owner_prompt = f.prompt.text.trim().to_owned();
        let role = f.role;
        let project_id = f.project_id.clone();
        let provider = p.id.clone();
        let models = new_agent::selectable_models(&p);
        let model = f.model.and_then(|i| models.get(i)).map(|m| m.id.clone());
        let mode = f.mode.and_then(|i| p.modes.get(i)).map(|m| m.id.clone());
        let juno = f.juno;
        let Some(project) = self
            .projects
            .iter()
            .find(|p| p.project_id == project_id)
            .cloned()
        else {
            self.new_agent_failed("The project is no longer on buildhost.".to_owned());
            return;
        };
        self.create_agent(
            project,
            provider,
            model,
            mode,
            role,
            title,
            owner_prompt,
            juno,
            None,
        );
    }

    /// The create behind the form's Create and `codecor.new_agent`: the worktree first (`POST /v1/agents/workspaces`,
    /// branch off main, named after the title), then the agent (`POST /v1/agents`) with that `workspaceId` and the
    /// CodeCor labels. A coordinator's prompt is the fixed preamble plus the owner's text. On success the new agent's
    /// row is selected and `reply` (the intent's) is answered; on a refusal the server's message goes under the form,
    /// which keeps what was typed, and to `reply`.
    #[allow(clippy::too_many_arguments)]
    pub fn create_agent(
        &mut self,
        project: ProjectRecord,
        provider: String,
        model: Option<String>,
        mode: Option<String>,
        role: Role,
        title: String,
        prompt: String,
        juno: bool,
        reply: Option<Responder>,
    ) {
        let repo = model::base_name(&project.root_path).to_owned();
        let project_id = project.project_id.clone();
        let prompt = if role == Role::Coordinator {
            let workers = projects::agents_of_project(self.store.items.values(), &repo)
                .workers
                .iter()
                .map(|w| crate::coordinator::WorkerRef {
                    id: w.id.clone(),
                    title: agent_title(w.title.as_deref(), &w.id, &w.cwd),
                })
                .collect::<Vec<_>>();
            crate::coordinator::prompt(&repo, &workers, &prompt)
        } else {
            prompt
        };
        let ws_req = new_agent::workspace_request(&project_id, &title, &prompt, &provider);
        self.call(
            "POST",
            new_agent::workspaces_path(),
            Some(ws_req),
            move |a, r: Result<Value, ApiError>| match r {
                Ok(v) => match serde_json::from_value::<Workspace>(v["workspace"].clone()) {
                    Ok(ws) => {
                        a.store
                            .workspaces
                            .insert(ws.workspace_id.clone(), ws.clone());
                        let body = new_agent::agent_request(
                            &ws, &provider, model, mode, &title, &prompt, &repo, role, juno,
                        );
                        a.call(
                            "POST",
                            new_agent::agents_path(),
                            Some(body),
                            move |a, r: Result<AgentSnapshot, ApiError>| match r {
                                Ok(s) => {
                                    let id = s.id.clone();
                                    let title = agent_title(s.title.as_deref(), &id, &s.cwd);
                                    a.store.apply_snapshot(s);
                                    a.new_agent = None;
                                    a.refresh();
                                    // The new agent's row is selected on the Project screen.
                                    if let Screen::Project(key) = a.screen.clone() {
                                        let at = a
                                            .agent_rows(&key)
                                            .iter()
                                            .position(|row| row.item.id == id);
                                        if let Some(i) = at {
                                            a.agents.select(i);
                                        }
                                    }
                                    if let Some(reply) = reply {
                                        reply.send(
                                            Reply::done(format!("Made the agent {title}"))
                                                .with_entity(Entity::new("agent", &id, &title))
                                                .with_value(json!({ "id": id })),
                                        );
                                    }
                                }
                                Err(e) => {
                                    let msg = new_agent::server_message(&e);
                                    a.new_agent_failed(msg.clone());
                                    if let Some(reply) = reply {
                                        reply.send(Reply::failed(msg));
                                    }
                                }
                            },
                        );
                    }
                    Err(e) => {
                        let msg = format!("Unexpected answer: {e}");
                        a.new_agent_failed(msg.clone());
                        if let Some(reply) = reply {
                            reply.send(Reply::failed(msg));
                        }
                    }
                },
                Err(e) => {
                    let msg = new_agent::server_message(&e);
                    a.new_agent_failed(msg.clone());
                    if let Some(reply) = reply {
                        reply.send(Reply::failed(msg));
                    }
                }
            },
        );
    }

    /// A create was refused: the server's message under the form, which keeps what was typed.
    fn new_agent_failed(&mut self, msg: String) {
        if let Some(f) = &mut self.new_agent {
            f.busy = false;
            f.problem = Some(msg);
        }
    }

    // ---- Views ----

    fn view_projects(&mut self, ui: &mut Ui) {
        let t = ui.theme();
        let a = ui.area();
        ui.within(
            Rect::new(
                a.x + t.px(12.0),
                a.y + t.px(4.0),
                a.w - t.px(20.0),
                a.h - t.px(8.0),
            ),
            |ui| {
                let area = ui.area();
                let half = (area.h / 2).max(t.px(120.0));
                let top = ui.cut_top(half);
                let rows = self.remote_rows();
                let mut open = None;
                ui.within(top, |ui| {
                    let head = ui.cut_top(t.px(26.0));
                    ui.text_in(head, Face::Bold, t.pt(15.0), t.accent, "On buildhost");
                    if rows.is_empty() {
                        ui.dim(if self.loaded {
                            "No projects on buildhost yet."
                        } else {
                            "Loading…"
                        });
                        return;
                    }
                    let r = ui.area();
                    let res = ui.list(
                        "buildhost",
                        &mut self.buildhost,
                        r,
                        rows.len(),
                        46.0,
                        |ui, i, sel| {
                            let row = ui.area();
                            let x = &rows[i];
                            if sel {
                                ui.fill_round(
                                    Rect::new(row.x, row.y + 1, row.w, row.h - 2),
                                    t.px(6.0),
                                    t.selection,
                                );
                            }
                            let left = row.x + t.px(10.0);
                            let mut meta = if x.needs_you > 0 {
                                format!("{} agents · needs you {}", x.agents, x.needs_you)
                            } else {
                                format!("{} agents", x.agents)
                            };
                            if x.lucina {
                                meta += " · Lucina project";
                            }
                            let mw = ui.measure(Face::Sans, t.pt(11.0), &meta);
                            let w = row.right() - left - mw - t.px(16.0);
                            ui.text_in(
                                Rect::new(left, row.y + t.px(4.0), w, t.px(20.0)),
                                Face::Sans,
                                t.pt(14.0),
                                t.text,
                                &x.name,
                            );
                            ui.text_in(
                                Rect::new(left, row.y + t.px(24.0), w, t.px(16.0)),
                                Face::Sans,
                                t.pt(11.0),
                                t.dim,
                                &x.path,
                            );
                            ui.text_in(
                                Rect::new(
                                    row.right() - mw - t.px(8.0),
                                    row.y + t.px(4.0),
                                    mw,
                                    t.px(20.0),
                                ),
                                Face::Sans,
                                t.pt(11.0),
                                if x.needs_you > 0 { t.accent } else { t.dim },
                                &meta,
                            );
                        },
                    );
                    if let Some(i) = res.activated {
                        open = Some(rows[i].key.clone());
                    }
                });
                let bottom = ui.area();
                let mut open_local = None;
                ui.within(bottom, |ui| {
                    let head = ui.cut_top(t.px(26.0));
                    ui.text_in(head, Face::Bold, t.pt(15.0), t.accent, "On Juno");
                    if self.local.is_empty() {
                        ui.dim("No folders in /data/projects.");
                        return;
                    }
                    let r = ui.area();
                    let local = &self.local;
                    let res = ui.list(
                        "juno",
                        &mut self.juno,
                        r,
                        local.len(),
                        46.0,
                        |ui, i, sel| {
                            let row = ui.area();
                            let x = &local[i];
                            if sel {
                                ui.fill_round(
                                    Rect::new(row.x, row.y + 1, row.w, row.h - 2),
                                    t.px(6.0),
                                    t.selection,
                                );
                            }
                            let left = row.x + t.px(10.0);
                            let w = row.right() - left - t.px(8.0);
                            ui.text_in(
                                Rect::new(left, row.y + t.px(4.0), w, t.px(20.0)),
                                Face::Sans,
                                t.pt(14.0),
                                t.text,
                                &x.name,
                            );
                            ui.text_in(
                                Rect::new(left, row.y + t.px(24.0), w, t.px(16.0)),
                                Face::Sans,
                                t.pt(11.0),
                                t.dim,
                                &x.path.to_string_lossy(),
                            );
                        },
                    );
                    if let Some(i) = res.activated {
                        open_local = Some(local[i].path.clone());
                    }
                });
                if let Some(key) = open {
                    self.open_project(key);
                }
                if let Some(path) = open_local {
                    if self.sample {
                        self.open_local_sample(path);
                    } else {
                        self.open_local(path);
                    }
                }
            },
        );
        // Which list the keys go to: a click on a list sets it, Tab swaps them.
        if ui.is_focused("juno") {
            self.focus = Focus::Juno;
        } else if ui.is_focused("buildhost") {
            self.focus = Focus::Nezopt;
        }
        if ui.key(keys::KEY_TAB) {
            self.focus = match self.focus {
                Focus::Nezopt => Focus::Juno,
                Focus::Juno => Focus::Nezopt,
            };
            ui.focus(if self.focus == Focus::Nezopt {
                "buildhost"
            } else {
                "juno"
            });
        }
    }

    fn view_project(&mut self, ui: &mut Ui, key: &ProjectKey) {
        let t = ui.theme();
        let a = ui.area();
        ui.within(
            Rect::new(
                a.x + t.px(12.0),
                a.y + t.px(4.0),
                a.w - t.px(20.0),
                a.h - t.px(8.0),
            ),
            |ui| match key {
                ProjectKey::Remote(id) => {
                    let Some(p) = self.projects.iter().find(|p| &p.project_id == id).cloned()
                    else {
                        ui.dim("No project with that id.");
                        return;
                    };
                    let head = ui.cut_top(t.px(28.0));
                    ui.text_in(head, Face::Bold, t.pt(16.0), t.text, &p.name);
                    let sub = ui.cut_top(t.px(20.0));
                    ui.text_in(sub, Face::Sans, t.pt(12.0), t.dim, &p.root_path);
                    if let Some(w) = self.coordinator_warning(key) {
                        ui.wrapped(Face::Sans, t.pt(13.0), t.danger, &w);
                    }
                    let foot = ui.cut_bottom(t.px(22.0));
                    let rows = self.agent_rows(key);
                    if rows.is_empty() {
                        ui.dim(if self.loaded {
                            "No CodeCor agents in this project yet."
                        } else {
                            "Loading…"
                        });
                        ui.text_in(
                            foot,
                            Face::Sans,
                            t.pt(11.0),
                            t.dim,
                            "N New agent · C Set coordinator · S Sessions · Esc back",
                        );
                        return;
                    }
                    let r = ui.area();
                    let mut open_at = None;
                    let res = ui.list(
                        "agents",
                        &mut self.agents,
                        r,
                        rows.len(),
                        46.0,
                        |ui, i, sel| {
                            let row = ui.area();
                            let x = &rows[i];
                            if sel {
                                ui.fill_round(
                                    Rect::new(row.x, row.y + 1, row.w, row.h - 2),
                                    t.px(6.0),
                                    t.selection,
                                );
                            }
                            let mut left = row.x + t.px(10.0);
                            if x.coordinator {
                                let label = "coordinator";
                                let bw = ui.measure(Face::Sans, t.pt(10.0), label) + t.px(12.0);
                                let badge = Rect::new(left, row.y + t.px(6.0), bw, t.px(18.0));
                                ui.fill_round(badge, t.px(9.0), t.accent);
                                ui.text_in(badge, Face::Sans, t.pt(10.0), t.bg, label);
                                left += bw + t.px(6.0);
                            }
                            let state = status_label(x.item.bucket, x.item.status);
                            let meta = if x.pending > 0 {
                                format!("{state} · {pending} waiting", pending = x.pending)
                            } else {
                                state.to_owned()
                            };
                            let mw = ui.measure(Face::Sans, t.pt(11.0), &meta);
                            let w = row.right() - left - mw - t.px(16.0);
                            let title =
                                agent_title(x.item.title.as_deref(), &x.item.id, &x.item.cwd);
                            ui.text_in(
                                Rect::new(left, row.y + t.px(4.0), w, t.px(20.0)),
                                Face::Sans,
                                t.pt(14.0),
                                t.text,
                                &title,
                            );
                            let model = x.item.model.clone().unwrap_or_default();
                            ui.text_in(
                                Rect::new(left, row.y + t.px(24.0), w, t.px(16.0)),
                                Face::Sans,
                                t.pt(11.0),
                                t.dim,
                                &format!("{} · {} · {}", x.item.provider, model, x.branch),
                            );
                            ui.text_in(
                                Rect::new(
                                    row.right() - mw - t.px(8.0),
                                    row.y + t.px(4.0),
                                    mw,
                                    t.px(20.0),
                                ),
                                Face::Sans,
                                t.pt(11.0),
                                bucket_colour(x.item.bucket, &t),
                                &meta,
                            );
                        },
                    );
                    if let Some(i) = res.activated {
                        open_at = Some(i);
                    }
                    ui.text_in(
                        foot,
                        Face::Sans,
                        t.pt(11.0),
                        t.dim,
                        "Enter open · N New agent · C Set coordinator · S Sessions · Esc back",
                    );
                    if let Some(i) = open_at {
                        self.open_agent(key.clone(), rows[i].item.id.clone());
                    }
                }
                ProjectKey::Local(path) => {
                    let head = ui.cut_top(t.px(28.0));
                    ui.text_in(
                        head,
                        Face::Bold,
                        t.pt(16.0),
                        t.text,
                        model::base_name(&path.to_string_lossy()),
                    );
                    let sub = ui.cut_top(t.px(20.0));
                    ui.text_in(sub, Face::Sans, t.pt(12.0), t.dim, &path.to_string_lossy());
                    ui.wrapped(
                        Face::Sans,
                        t.pt(13.0),
                        t.dim,
                        "Local Qwen: the terminal program, through the tunnel to buildhost.",
                    );
                    ui.space(8.0);
                    if ui.primary_button("Open in terminal (local Qwen)") {
                        self.open_in_terminal(path);
                    }
                }
            },
        );
    }

    /// The **Agent** screen: the title with the coordinator badge, the provider · model · role · branch line, and the
    /// pane with the Chat and Changes tabs. What the pane wants done (send, stop, answer, read the diff) is done here.
    fn view_agent(&mut self, ui: &mut Ui, id: &str) {
        let t = ui.theme();
        let a = ui.area();
        ui.within(
            Rect::new(
                a.x + t.px(12.0),
                a.y + t.px(4.0),
                a.w - t.px(20.0),
                a.h - t.px(8.0),
            ),
            |ui| {
                let item = self.store.items.get(id).cloned();
                let snap = self.store.snapshots.get(id).cloned();
                let cwd = item
                    .as_ref()
                    .map(|i| i.cwd.as_str())
                    .or_else(|| snap.as_ref().map(|s| s.cwd.as_str()))
                    .unwrap_or("");
                let title = item
                    .as_ref()
                    .and_then(|i| i.title.clone())
                    .or_else(|| snap.as_ref().and_then(|s| s.title.clone()))
                    .unwrap_or_else(|| agent_title(None, id, cwd));
                let role = item
                    .as_ref()
                    .and_then(|i| i.labels.get(projects::ROLE_LABEL).cloned());
                let head = ui.cut_top(t.px(28.0));
                ui.text_in(
                    Rect::new(head.x + t.px(2.0), head.y, head.w - t.px(130.0), head.h),
                    Face::Bold,
                    t.pt(16.0),
                    t.text,
                    &title,
                );
                if role.as_deref() == Some("coordinator") {
                    let label = "coordinator";
                    let bw = ui.measure(Face::Sans, t.pt(10.0), label) + t.px(12.0);
                    let badge = Rect::new(
                        head.right() - bw - t.px(8.0),
                        head.y + t.px(5.0),
                        bw,
                        t.px(18.0),
                    );
                    ui.fill_round(badge, t.px(9.0), t.accent);
                    ui.text_in(badge, Face::Sans, t.pt(10.0), t.bg, label);
                }
                let sub = ui.cut_top(t.px(20.0));
                let provider = item
                    .as_ref()
                    .map(|i| i.provider.clone())
                    .or_else(|| snap.as_ref().map(|s| s.provider.clone()))
                    .unwrap_or_default();
                let model = item
                    .as_ref()
                    .and_then(|i| i.model.clone())
                    .or_else(|| snap.as_ref().and_then(|s| s.model.clone()))
                    .unwrap_or_default();
                let branch = self
                    .store
                    .workspace_of(id)
                    .and_then(|w| w.branch.clone())
                    .unwrap_or_else(|| model::base_name(cwd).to_owned());
                let role_text = role.unwrap_or_else(|| "worker".to_owned());
                ui.text_in(
                    sub,
                    Face::Sans,
                    t.pt(12.0),
                    t.dim,
                    &format!("{provider} · {model} · {role_text} · {branch}"),
                );
                if self.missing {
                    ui.dim("This agent is gone from the server.");
                    return;
                }
                let archived = self.store.archived(id);
                let running = self.store.running(id);
                let act = self.pane.view(ui, &self.store, id, archived, running);
                match act {
                    PaneAct::None => {}
                    PaneAct::Send => self.send_prompt(id),
                    PaneAct::Stop => self.stop_agent(id),
                    PaneAct::Answer { key, allow } => self.answer(&key, allow),
                    PaneAct::LoadDiff => self.load_diff(id),
                }
            },
        );
    }

    /// The **local Agent** screen (phase E): the folder's name, "Qwen (on Juno)", and the same Chat pane as the Agents
    /// app, fed from the ACP updates. T shows the terminal line as a fallback.
    fn view_local(&mut self, ui: &mut Ui, path: &Path) {
        let t = ui.theme();
        let a = ui.area();
        ui.within(
            Rect::new(
                a.x + t.px(12.0),
                a.y + t.px(4.0),
                a.w - t.px(20.0),
                a.h - t.px(8.0),
            ),
            |ui| {
                let head = ui.cut_top(t.px(28.0));
                ui.text_in(
                    Rect::new(head.x + t.px(2.0), head.y, head.w - t.px(130.0), head.h),
                    Face::Bold,
                    t.pt(16.0),
                    t.text,
                    model::base_name(&path.to_string_lossy()),
                );
                let sub = ui.cut_top(t.px(20.0));
                ui.text_in(
                    sub,
                    Face::Sans,
                    t.pt(12.0),
                    t.dim,
                    &format!("Qwen (on Juno) · {}", path.to_string_lossy()),
                );
                let Some(chat) = self.local_chat.as_ref() else {
                    ui.dim("Starting the local agent…");
                    return;
                };
                let running = chat.running;
                let id = chat.id.clone();
                let act = self.pane.view(ui, &self.store, &id, false, running);
                match act {
                    PaneAct::None => {}
                    PaneAct::Send => self.local_send(),
                    PaneAct::Stop => self.local_stop(),
                    PaneAct::Answer { key, allow } => self.local_answer(&key, allow),
                    PaneAct::LoadDiff => {}
                }
            },
        );
    }

    /// The **Sessions** screen: the project's archived agents; Enter unarchives one and opens it.
    fn view_sessions(&mut self, ui: &mut Ui, key: &ProjectKey) {
        let t = ui.theme();
        let a = ui.area();
        ui.within(
            Rect::new(
                a.x + t.px(12.0),
                a.y + t.px(4.0),
                a.w - t.px(20.0),
                a.h - t.px(8.0),
            ),
            |ui| {
                let name = match key {
                    ProjectKey::Remote(id) => self
                        .projects
                        .iter()
                        .find(|p| &p.project_id == id)
                        .map(|p| p.name.clone())
                        .unwrap_or_default(),
                    ProjectKey::Local(path) => model::base_name(&path.to_string_lossy()).to_owned(),
                };
                let head = ui.cut_top(t.px(28.0));
                ui.text_in(
                    head,
                    Face::Bold,
                    t.pt(16.0),
                    t.text,
                    &format!("Sessions — {name}"),
                );
                let sub = ui.cut_top(t.px(20.0));
                ui.text_in(
                    sub,
                    Face::Sans,
                    t.pt(12.0),
                    t.dim,
                    "The archived agents of this project.",
                );
                let foot = ui.cut_bottom(t.px(22.0));
                let rows = self.sessions_rows(key);
                if rows.is_empty() {
                    ui.dim("No archived agents in this project.");
                } else {
                    let r = ui.area();
                    let mut open_at = None;
                    let res = ui.list(
                        "sessions",
                        &mut self.sessions,
                        r,
                        rows.len(),
                        46.0,
                        |ui, i, sel| {
                            let row = ui.area();
                            let x = &rows[i];
                            if sel {
                                ui.fill_round(
                                    Rect::new(row.x, row.y + 1, row.w, row.h - 2),
                                    t.px(6.0),
                                    t.selection,
                                );
                            }
                            let mut left = row.x + t.px(10.0);
                            if x.coordinator {
                                let label = "coordinator";
                                let bw = ui.measure(Face::Sans, t.pt(10.0), label) + t.px(12.0);
                                let badge = Rect::new(left, row.y + t.px(6.0), bw, t.px(18.0));
                                ui.fill_round(badge, t.px(9.0), t.accent);
                                ui.text_in(badge, Face::Sans, t.pt(10.0), t.bg, label);
                                left += bw + t.px(6.0);
                            }
                            let title =
                                agent_title(x.item.title.as_deref(), &x.item.id, &x.item.cwd);
                            let w = row.right() - left - t.px(8.0);
                            ui.text_in(
                                Rect::new(left, row.y + t.px(4.0), w, t.px(20.0)),
                                Face::Sans,
                                t.pt(14.0),
                                t.text,
                                &title,
                            );
                            let model = x.item.model.clone().unwrap_or_default();
                            ui.text_in(
                                Rect::new(left, row.y + t.px(24.0), w, t.px(16.0)),
                                Face::Sans,
                                t.pt(11.0),
                                t.dim,
                                &format!("{} · {} · {}", x.item.provider, model, x.branch),
                            );
                        },
                    );
                    if let Some(i) = res.activated {
                        open_at = Some(i);
                    }
                    if let Some(i) = open_at {
                        self.unarchive_and_open(key.clone(), rows[i].item.id.clone());
                    }
                }
                ui.text_in(
                    foot,
                    Face::Sans,
                    t.pt(11.0),
                    t.dim,
                    "Enter unarchive and open · Esc back",
                );
            },
        );
    }

    /// The **New agent** form, as a modal over the Project screen: provider (from `/v1/agents/providers`, Codex greyed
    /// out on a Lucina project), model and mode where the provider lists them, the Juno shell for a Claude agent (every
    /// call asks the owner first), role, title, first prompt. Create posts the worktree first, then the agent; a
    /// refusal shows the server's message under the form.
    fn view_new_agent(&mut self, ui: &mut Ui) {
        let t = ui.theme();
        let Some(f) = &mut self.new_agent else { return };
        let (mut cancel, mut create) = (false, false);
        ui.modal(680.0, 560.0, |ui| {
            let head = ui.cut_top(t.px(28.0));
            ui.text_in(head, Face::Bold, t.pt(16.0), t.accent, "New agent");
            let Some(list) = f.providers.clone() else {
                ui.dim("Loading…");
                return;
            };
            if let Some(p) = &f.problem {
                ui.wrapped(Face::Sans, t.pt(13.0), t.danger, p);
            }
            let cur = list.get(f.provider).cloned();
            ui.row(32.0, |ui| {
                let label = format!(
                    "Provider: {}",
                    cur.as_ref().map_or("?", |p| p.label.as_str())
                );
                if ui.button_id("na-provider", &label) && !list.is_empty() {
                    f.cycle_provider();
                }
            });
            if let Some(line) = f.codex_line() {
                ui.wrapped(Face::Sans, t.pt(12.0), t.dim, line);
            }
            let models = f.models().into_iter().cloned().collect::<Vec<_>>();
            ui.row(32.0, |ui| {
                let label = f
                    .model
                    .and_then(|i| models.get(i))
                    .map_or("Default", |m| m.label.as_str());
                if ui.button_id("na-model", &format!("Model: {label}")) {
                    f.cycle_model();
                }
            });
            let modes = f.modes().into_iter().cloned().collect::<Vec<_>>();
            if !modes.is_empty() {
                ui.row(32.0, |ui| {
                    let label = f
                        .mode
                        .and_then(|i| modes.get(i))
                        .map_or("Default", |m| m.label.as_str());
                    if ui.button_id("na-mode", &format!("Mode: {label}")) {
                        f.cycle_mode();
                    }
                });
            }
            if f.juno_shown() {
                ui.row(32.0, |ui| {
                    let label = if f.juno {
                        "Juno shell: on (each call asks you)"
                    } else {
                        "Juno shell: off"
                    };
                    if ui.button_id("na-juno", label) {
                        f.toggle_juno();
                    }
                });
            }
            ui.row(32.0, |ui| {
                if f.can_coordinator {
                    if ui.button_id("na-role", &format!("Role: {}", f.role.label())) {
                        f.cycle_role();
                    }
                } else {
                    ui.dim(&format!(
                        "Role: {} (this project already has a coordinator)",
                        f.role.label()
                    ));
                }
            });
            ui.space(4.0);
            ui.dim("Title (the worktree branch is named after it)");
            if f.take_focus() {
                ui.focus("na-title");
            }
            if ui.text_field("na-title", &mut f.title).submitted {
                create = true;
            }
            ui.space(4.0);
            ui.dim("First prompt (Ctrl+Enter creates)");
            if ui.text_area("na-prompt", &mut f.prompt, 5).submitted {
                create = true;
            }
            ui.space(4.0);
            ui.row(34.0, |ui| {
                if f.busy {
                    ui.dim("Creating…");
                } else {
                    if ui.primary_button("Create") && cur.is_some() {
                        create = true;
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
            self.new_agent = None;
        }
        if create {
            self.submit_new_agent();
        }
    }
}

impl App for CodeCor {
    type Msg = Msg;

    fn message(&mut self, m: Msg) {
        self.pump_local();
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
                    Effect::RefreshAll => self.refresh(),
                    Effect::LoadTimeline(id) => self.load_timeline(id),
                }
                self.remember_seen();
                self.pane.mark_soft();
            }
        }
        // An intent that was waiting for the store answers now that this message moved it.
        crate::intents::answer_pending(self);
    }

    fn intents() -> &'static [lucina_app::intent::IntentSpec] {
        crate::intents::INTENTS
    }

    fn intent(&mut self, call: lucina_app::intent::Call, reply: lucina_app::intent::Responder) {
        crate::intents::intent(self, call, reply);
    }

    fn view(&mut self, ui: &mut Ui) {
        self.pump_local();
        ui.header(if self.sample {
            "CodeCor (sample data)"
        } else {
            "CodeCor"
        });
        if let Some(s) = &mut self.sign_in {
            s.view(ui, "CodeCor reads the coding agents in your account.");
            return;
        }
        // The stream's health: a lost stream is checked against the REST routes.
        if self.lost_at.is_some_and(|t| t.elapsed() >= QUIET)
            || self.next_poll.is_some_and(|t| Instant::now() >= t)
        {
            self.next_poll = None;
            self.check_backend();
        }
        if self.last_load.is_none_or(|t| t.elapsed() >= POLL) {
            self.refresh();
        }
        // The Agent screen's lines rebuild on their own clock: every second while a permission waits, else every 30 s,
        // and a soft change (a stream event) at the stream's pace.
        let pending = !self.store.pending_permissions().is_empty();
        let ticking = pending
            && matches!(&self.screen, Screen::Agent { .. } | Screen::Local(_))
            && self.pane.tab == Tab::Chat;
        let tick = if ticking {
            Duration::from_secs(1)
        } else {
            Duration::from_secs(30)
        };
        if self.pane.built_at.is_none_or(|t| t.elapsed() >= tick) {
            self.pane.stale = true;
        }
        let pane_id = match self.screen.clone() {
            Screen::Agent { id, .. } => Some(id),
            Screen::Local(_) => self.local_chat.as_ref().map(|c| c.id.clone()),
            _ => None,
        };
        if (self.pane.stale
            || (self.pane.soft
                && self
                    .pane
                    .built_at
                    .is_none_or(|t| t.elapsed() >= self.pane.pace)))
            && let Some(id) = pane_id
        {
            let now = self.now();
            self.pane.rebuild(&self.store, &id, now);
        }
        ui.wake_after(if self.pane.soft {
            self.pane.pace
        } else if self.local_chat.is_some() {
            // The ACP events arrive on their own thread: follow them at the stream's pace.
            Duration::from_millis(100)
        } else if self.lost_at.is_some() {
            Duration::from_secs(1)
        } else {
            tick
        });
        if let Some(f) = self.focus_want.take() {
            ui.focus(f);
        }
        let t = ui.theme();
        if let Some((text, alert)) = self.banner() {
            let r = ui.cut_top(t.px(24.0));
            ui.fill(r, t.panel_hi);
            ui.text_in(
                Rect::new(r.x + t.px(10.0), r.y, r.w - t.px(20.0), r.h),
                Face::Sans,
                t.pt(13.0),
                if alert { t.danger } else { t.item },
                text,
            );
        }
        if let Some(e) = &self.load_error {
            let r = ui.cut_top(t.px(24.0));
            ui.fill(r, t.panel_hi);
            ui.text_in(
                Rect::new(r.x + t.px(10.0), r.y, r.w - t.px(20.0), r.h),
                Face::Sans,
                t.pt(13.0),
                t.danger,
                e,
            );
        }
        if let Some(n) = &self.note {
            let r = ui.cut_top(t.px(24.0));
            ui.fill(r, t.panel_hi);
            ui.text_in(
                Rect::new(r.x + t.px(10.0), r.y, r.w - t.px(20.0), r.h),
                Face::Sans,
                t.pt(13.0),
                t.item,
                n,
            );
        }
        match self.screen.clone() {
            Screen::Projects => self.view_projects(ui),
            Screen::Project(key) => self.view_project(ui, &key),
            Screen::Agent { id, .. } => self.view_agent(ui, &id),
            Screen::Sessions(key) => self.view_sessions(ui, &key),
            Screen::Local(path) => self.view_local(ui, &path),
        }
        if self.new_agent.is_some() {
            self.view_new_agent(ui);
        } else {
            if ui.key(keys::KEY_ESC) && self.screen != Screen::Projects {
                self.back();
            }
            match self.screen.clone() {
                Screen::Project(key) => {
                    if ui.key(keys::KEY_N) {
                        self.open_new_agent(key.clone(), Role::Worker);
                    }
                    if ui.key(keys::KEY_C) {
                        if self.has_coordinator(&key) {
                            self.note = Some("This project already has a coordinator.".to_owned());
                        } else {
                            self.open_new_agent(key.clone(), Role::Coordinator);
                        }
                    }
                    if ui.key(keys::KEY_S) {
                        self.open_sessions(key.clone());
                    }
                }
                Screen::Agent { id, .. } => {
                    if ui.key(keys::KEY_LEFTBRACE) {
                        self.cycle_agent_tab(-1, &id);
                    }
                    if ui.key(keys::KEY_RIGHTBRACE) {
                        self.cycle_agent_tab(1, &id);
                    }
                    if ui.key(keys::KEY_I) && self.pane.tab == Tab::Chat {
                        self.pane.focus = Some("composer");
                    }
                    if ui.key(keys::KEY_END) {
                        self.pane.follow = true;
                        self.pane.main.scroll_to_end();
                    }
                }
                Screen::Local(path) => {
                    // The terminal line stays available as a secondary action, for when ACP fails.
                    if ui.key(keys::KEY_T) {
                        self.open_in_terminal(&path);
                    }
                    if ui.key(keys::KEY_I) && self.pane.tab == Tab::Chat {
                        self.pane.focus = Some("composer");
                    }
                    if ui.key(keys::KEY_END) {
                        self.pane.follow = true;
                        self.pane.main.scroll_to_end();
                    }
                }
                _ => {}
            }
        }
    }
}

/// The sample server with CodeCor's labels on its agents, a second project and a Lucina project (so the Codex rule
/// shows), a pending permission that carries a diff, and an archived session, so the screens have something to show.
pub fn sample_api(now: i64) -> Arc<dyn Api> {
    Arc::new(CodecorSample::new(now))
}

#[derive(Clone)]
pub(crate) struct CodecorSample {
    inner: Sample,
    now: i64,
    /// Whether the sample's archived session was unarchived.
    unarchived: Arc<AtomicBool>,
}

/// The diff the sample's permission request carries.
const SAMPLE_DIFF_PATH: &str = "/srv/work/sample-project/src/sample.rs";
const SAMPLE_DIFF_OLD: &str = "fn upload() -> Result<(), Error> {\n    send()\n}";
const SAMPLE_DIFF_NEW: &str = "fn upload() -> Result<(), Error> {\n    retry(3, send)\n}";

/// The ACP tool call content with the `diff` block.
fn diff_input() -> Value {
    json!({"content": [
        {"type": "text", "text": "Adding a retry to the upload"},
        {"type": "diff", "path": SAMPLE_DIFF_PATH, "oldText": SAMPLE_DIFF_OLD, "newText": SAMPLE_DIFF_NEW},
    ]})
}

/// The CodeCor labels of a sample agent, by id (the list and the snapshot carry the same ones).
fn sample_labels(id: &str) -> Value {
    match id {
        "agt_sample_running" => {
            json!({ projects::PROJECT_LABEL: "sample-project", projects::ROLE_LABEL: "coordinator" })
        }
        "agt_sample_failed" => {
            json!({ projects::PROJECT_LABEL: "sample-project", projects::ROLE_LABEL: "worker" })
        }
        _ => json!({ projects::PROJECT_LABEL: "sample-project" }),
    }
}

/// The permission book's view of the diff request.
fn perm_diff_view(now: i64) -> Value {
    json!({
        "id": "perm-diff", "kind": "agent", "agentId": "agt_sample_running", "name": "edit",
        "title": "Edit src/sample.rs", "description": "Adds a retry to the upload",
        "details": diff_input(),
        "status": "pending",
        "createdAt": api::iso_now(now - 30_000), "expiresAt": api::iso_now(now + 570_000),
    })
}

/// The snapshot's pending permission carrying the same diff.
fn perm_diff_request() -> Value {
    json!({
        "id": "perm-diff", "provider": "oc-cli", "name": "edit", "kind": "tool",
        "title": "Edit src/sample.rs", "description": "Adds a retry to the upload",
        "input": diff_input(),
        "detail": {"type": "edit", "filePath": SAMPLE_DIFF_PATH, "oldString": SAMPLE_DIFF_OLD, "newString": SAMPLE_DIFF_NEW},
    })
}

/// The sample project's archived agent, as a list item.
fn archived_item(now: i64, archived: bool) -> Value {
    let at = api::iso_now(now - 7_200_000);
    let mut v = json!({
        "id": "agt_sample_archived", "shortId": "agt_sam", "title": "Old session: the index rebuild",
        "provider": "claude", "model": "sample-model-1", "status": "idle", "cwd": "/srv/work/sample-project",
        "createdAt": at, "updatedAt": at,
        "labels": { projects::PROJECT_LABEL: "sample-project", projects::ROLE_LABEL: "worker" },
        "bucket": "done",
    });
    if archived {
        v["archivedAt"] = api::iso_now(now - 3_600_000).into();
    }
    v
}

/// The archived agent's snapshot.
fn archived_snapshot(now: i64, archived: bool) -> Value {
    let at = api::iso_now(now - 7_200_000);
    let mut v = json!({
        "id": "agt_sample_archived", "provider": "claude", "cwd": "/srv/work/sample-project", "workspaceId": "wks_sample",
        "model": "sample-model-1", "createdAt": at, "updatedAt": at, "lastUserMessageAt": at, "status": "idle",
        "capabilities": {"supportsStreaming": true, "supportsSessionPersistence": true, "supportsDynamicModes": false,
            "supportsMcpServers": true, "supportsReasoningStream": true, "supportsToolInvocations": true},
        "currentModeId": null, "availableModes": [], "pendingPermissions": [], "persistence": null,
        "title": "Old session: the index rebuild",
        "labels": { projects::PROJECT_LABEL: "sample-project", projects::ROLE_LABEL: "worker" },
    });
    if archived {
        v["archivedAt"] = api::iso_now(now - 3_600_000).into();
    }
    v
}

impl CodecorSample {
    pub(crate) fn new(now: i64) -> CodecorSample {
        CodecorSample {
            inner: Sample::new(now),
            now,
            unarchived: Arc::new(AtomicBool::new(false)),
        }
    }

    /// What was asked of the sample server, in order, without the reads.
    #[cfg(test)]
    pub(crate) fn writes(&self) -> Vec<(String, Value)> {
        self.inner.writes()
    }

    /// How often a read was asked of the sample server.
    #[cfg(test)]
    pub(crate) fn reads(&self, path_starts: &str) -> usize {
        self.inner.reads(path_starts)
    }
}

impl Api for CodecorSample {
    fn call(&self, method: &str, path: &str, body: Option<&Value>) -> Result<Value, ApiError> {
        // The permission book carries a diff request (ACP `diff` content) on the running agent.
        if method == "GET" && path.starts_with(&format!("{B}/permissions")) {
            let mut v = self.inner.call(method, path, body)?;
            if let Some(list) = v.get_mut("permissions").and_then(|p| p.as_array_mut()) {
                list.push(perm_diff_view(self.now));
            }
            return Ok(v);
        }
        // A sample agent's snapshot carries the CodeCor labels (the list's labels would be wiped by it), and the
        // running one carries the diff request as a pending permission too.
        if method == "GET"
            && path
                .strip_prefix(&format!("{B}/"))
                .is_some_and(|rest| rest.starts_with("agt_sample_") && !rest.contains('/'))
        {
            let id = path.strip_prefix(&format!("{B}/")).unwrap_or("");
            let mut v = self.inner.call(method, path, body)?;
            v["labels"] = sample_labels(id);
            if id == "agt_sample_running"
                && let Some(pp) = v
                    .get_mut("pendingPermissions")
                    .and_then(|p| p.as_array_mut())
            {
                pp.push(perm_diff_request());
            }
            return Ok(v);
        }
        // The archived session, in the list with `?archived=true` until it is unarchived.
        if method == "GET" && path == format!("{B}/items?archived=true") {
            let mut v = self.inner.call(method, path, body)?;
            if let Some(agents) = v.get_mut("agents").and_then(|a| a.as_array_mut()) {
                agents.clear();
                if !self.unarchived.load(Ordering::SeqCst) {
                    agents.push(archived_item(self.now, true));
                }
            }
            return Ok(v);
        }
        if method == "POST" && path == format!("{B}/agt_sample_archived/unarchive") {
            let _ = self.inner.call(method, path, body); // recorded
            self.unarchived.store(true, Ordering::SeqCst);
            return Ok(archived_snapshot(self.now, false));
        }
        if method == "GET" && path == format!("{B}/agt_sample_archived") {
            if let Ok(v) = self.inner.call(method, path, body) {
                return Ok(v);
            }
            let archived = !self.unarchived.load(Ordering::SeqCst);
            return Ok(archived_snapshot(self.now, archived));
        }
        if method == "GET" && path.starts_with(&format!("{B}/agt_sample_archived/timeline")) {
            if let Ok(v) = self.inner.call(method, path, body) {
                return Ok(v);
            }
            let at = api::iso_now(self.now - 3_600_000);
            return Ok(json!({"epoch": "sample-epoch", "rows": [
                {"seq": 1, "timestamp": at, "turnId": "t1", "item": {"type": "user_message", "text": "Rebuild the index."}},
                {"seq": 2, "timestamp": at, "turnId": "t1", "item": {"type": "assistant_message", "text": "The index is rebuilt."}}
            ], "nextSeq": 3}));
        }
        // Answering the diff request: recorded by the inner server, answered here.
        if method == "POST"
            && (path == format!("{B}/agt_sample_running/permissions/perm-diff")
                || path == format!("{B}/permissions/perm-diff"))
        {
            let _ = self.inner.call(method, path, body); // recorded
            return Ok(json!({
                "agentId": "agt_sample_running",
                "requestId": "perm-diff",
                "behavior": body.and_then(|b| b.get("behavior")).cloned().unwrap_or(Value::Null),
            }));
        }
        let v = self.inner.call(method, path, body)?;
        if method == "GET" && path == format!("{B}/projects") {
            return Ok(json!({"projects": [
                {"projectId": "prj-sample", "rootPath": "/srv/work/sample-project", "name": "Sample project"},
                {"projectId": "prj-other", "rootPath": "/srv/work/other-project", "name": "Other project"},
                {"projectId": "prj-lucina", "rootPath": "/home/user/Projects/Lucina/LucinaOS", "name": "LucinaOS"},
            ]}));
        }
        if method == "GET" && path == format!("{B}/items") {
            let mut v = v;
            if let Some(agents) = v.get_mut("agents").and_then(|a| a.as_array_mut()) {
                for a in agents.iter_mut() {
                    a["labels"] = sample_labels(a["id"].as_str().unwrap_or(""));
                }
                // Once unarchived, the session is in the active list too.
                if self.unarchived.load(Ordering::SeqCst) {
                    agents.push(archived_item(self.now, false));
                }
            }
            return Ok(v);
        }
        Ok(v)
    }

    fn stream(&self, path: &str, last_id: Option<&str>) -> Result<Box<dyn Blocks>, ApiError> {
        self.inner.stream(path, last_id)
    }
}
