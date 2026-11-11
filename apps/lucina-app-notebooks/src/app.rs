//! The state and what it does: `web/src/views/Notebooks.tsx`, `Notebook.tsx` and `notebooks/*`. The drawing is in `screens.rs`.

use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

use lucina_account::ui::SignInView;
use lucina_app::time::Zone;
use lucina_app::{App, ListState, Load, TextEdit, Ui};
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::md::{self, Doc};
use crate::screens;
use crate::server::{Answer, Call, Failure, Job, Msg, Paging, Tag, api};
use crate::wire::{
    Chat, Citation, Detail, Hit, Insight, Message, NoteLink, Notebook, Profile, Source, Transformation, chunk_ordinal,
    insight_pending, next_context, ref_source_id, source_pending,
};

/// The web's `POLL_MS`, for the first half minute. Only one thing polls at a time, because the account's limit of 60 requests a minute
/// is shared by every app, and an episode takes many minutes: after that it asks less often.
pub const POLL: Duration = Duration::from_secs(3);
const POLL_SLOW: Duration = Duration::from_secs(10);
const POLLS_FAST: u32 = 10;
/// The list changes rarely, and the same limit applies.
pub const REFRESH: Duration = Duration::from_secs(300);
pub const SEARCH_WAIT: Duration = Duration::from_millis(300);
pub const PICK_WAIT: Duration = Duration::from_millis(250);
/// `PAGE` in SourceReader.tsx.
const PAGE: u32 = 40;
const MIN_MINUTES: u32 = 2;
const MAX_MINUTES: u32 = 30;
const DEFAULT_MINUTES: u32 = 8;
const KEPT_DETAILS: usize = 6;

pub type Link = Box<dyn FnMut(Job)>;

fn poll_gap(polls: u32) -> Duration {
    if polls < POLLS_FAST { POLL } else { POLL_SLOW }
}

/// What the next start shows before any network.
#[derive(Default, Serialize, Deserialize)]
pub struct Cached {
    pub active: Vec<Notebook>,
    pub archived: Vec<Notebook>,
    pub details: Vec<Detail>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tab {
    Sources,
    Notes,
    Chat,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Todo {
    Back,
    Reload,
    ReloadList,
    Filter(bool),
    OpenNotebook(usize),
    NewNotebook,
    CreateNotebook,
    Tab(Tab),
    Sheet(SheetKind),
    CloseSheet,
    OpenSource(usize),
    ToggleContext(usize),
    Retry(usize),
    AskRemoveSource(usize),
    RemoveSource(String),
    OpenNote(usize),
    Unlink(usize),
    Send,
    CycleModel,
    NewChat,
    PickChat(usize),
    OpenPicker,
    OpenRename,
    SaveRename,
    AskDeleteChat,
    DeleteChat,
    Cite(usize, usize),
    SaveReply(usize),
    CloseReader,
    CycleTransformation,
    RunTransformation,
    SaveInsight(usize),
    AskDeleteInsight(usize),
    DeleteInsight(String),
    MorePassages(Paging),
    ReaderPane(bool),
    SubmitAdd,
    AddMode(AddMode),
    SubmitWrite,
    SubmitLink(String),
    OpenHit(usize),
    CycleEpisode,
    CycleSpeaker,
    CreatePodcast,
    AskDeletePodcast(usize),
    DeletePodcast(String),
    ToggleTranscript(usize),
    SaveSettings,
    Archive,
    AskDeleteNotebook,
    DeleteNotebook,
    CloseConfirm,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SheetKind {
    Add,
    Write,
    Link,
    Search,
    Podcast,
    Settings,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AddMode {
    Link,
    Text,
    File,
    Path,
    Note,
}

pub struct NewForm {
    pub name: TextEdit,
    pub description: TextEdit,
    pub error: Option<String>,
    pub busy: bool,
    pub first: bool,
}

/// `NotePicker.tsx`: a search box over Keep and the matches. A typed slug that is not among them can be used as it is.
pub struct Picker {
    pub query: TextEdit,
    pub notes: Option<Vec<NoteLink>>,
    pub error: Option<String>,
    pub list: ListState,
    pub exclude: Vec<String>,
    pub due: Option<Instant>,
}

impl Picker {
    pub fn new(exclude: Vec<String>) -> Picker {
        Picker {
            query: TextEdit::new().placeholder("Search your notes, or type a slug"),
            notes: None,
            error: None,
            list: ListState::new(),
            exclude,
            due: Some(Instant::now()),
        }
    }

    /// `shown` and `typedSlug`: what can be picked, as (slug, title, excerpt).
    pub fn rows(&self) -> Vec<(String, String, String)> {
        let mut rows: Vec<(String, String, String)> = self
            .notes
            .iter()
            .flatten()
            .filter(|n| !self.exclude.contains(&n.slug))
            .take(30)
            .map(|n| {
                let title = if n.title.is_empty() { n.slug.clone() } else { n.title.clone() };
                let sub = md::plain_excerpt(&n.excerpt, &n.title);
                let sub = if sub.is_empty() { n.slug.clone() } else { sub.chars().take(90).collect() };
                (n.slug.clone(), title, sub)
            })
            .collect();
        let q = self.query.text.trim().to_lowercase();
        if is_slug(&q) && !rows.iter().any(|r| r.0 == q) {
            rows.push((q.clone(), format!("Use \"{q}\""), String::new()));
        }
        rows
    }
}

/// `SLUG_RE`: lower-case letters, digits and single hyphens between them, at most 64.
pub fn is_slug(s: &str) -> bool {
    let b = s.as_bytes();
    !b.is_empty()
        && b.len() <= 64
        && b.iter().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == b'-')
        && b[0] != b'-'
        && b[b.len() - 1] != b'-'
        && !s.contains("--")
}

pub struct AddForm {
    pub mode: AddMode,
    pub url: TextEdit,
    pub title: TextEdit,
    pub text: TextEdit,
    pub path: TextEdit,
    pub picker: Picker,
    pub error: Option<String>,
    pub busy: bool,
    pub first: bool,
}

impl AddForm {
    /// `valid` in AddSource.tsx.
    pub fn valid(&self) -> bool {
        match self.mode {
            AddMode::Link => is_http_url(self.url.text.trim()),
            AddMode::Text => !self.text.text.trim().is_empty(),
            AddMode::File => false,
            AddMode::Path => self.path.text.trim().starts_with('/'),
            AddMode::Note => false,
        }
    }
}

fn is_http_url(s: &str) -> bool {
    let lower = s.to_ascii_lowercase();
    let rest = lower.strip_prefix("https://").or_else(|| lower.strip_prefix("http://"));
    rest.is_some_and(|r| !r.is_empty()) && !s.chars().any(char::is_whitespace)
}

pub struct WriteForm {
    pub title: TextEdit,
    pub text: TextEdit,
    pub error: Option<String>,
    pub busy: bool,
    pub first: bool,
}

pub struct LinkForm {
    pub picker: Picker,
    pub error: Option<String>,
    pub busy: bool,
    pub first: bool,
}

pub struct SearchForm {
    pub query: TextEdit,
    pub hits: Option<Vec<Hit>>,
    pub error: Option<String>,
    pub list: ListState,
    pub due: Option<Instant>,
    pub first: bool,
}

pub struct PodcastForm {
    pub episodes: Vec<Profile>,
    pub speakers: Vec<Profile>,
    pub episode: usize,
    pub speaker: usize,
    pub minutes: TextEdit,
    pub briefing: TextEdit,
    pub error: Option<String>,
    pub busy: bool,
    pub list: ListState,
    pub open: Option<String>,
    pub first: bool,
}

pub struct SettingsForm {
    pub name: TextEdit,
    pub description: TextEdit,
    pub delete_exclusive: bool,
    pub error: Option<String>,
    pub busy: bool,
    pub first: bool,
}

pub enum Sheet {
    Add(Box<AddForm>),
    Write(WriteForm),
    Link(LinkForm),
    Search(SearchForm),
    Podcast(Box<PodcastForm>),
    Settings(SettingsForm),
    /// A linked note's excerpt: its text lives in Keep, and this laptop's Notes app opens it there.
    Note(NoteLink),
    /// The chat's name.
    Rename(TextEdit, bool),
}

pub struct Confirm {
    pub text: String,
    pub yes: String,
    pub action: Todo,
    pub first: bool,
}

pub struct ShownMessage {
    pub message: Message,
    pub doc: Doc,
    pub cited: Vec<Citation>,
}

impl ShownMessage {
    pub fn new(message: Message) -> ShownMessage {
        let doc = md::parse(&message.text);
        // `citedIn`: the citations a reply uses, in the order its numbers go.
        let cited = doc.refs.iter().flat_map(|r| message.citations.iter().filter(move |c| c.reference == *r)).cloned().collect();
        ShownMessage { message, doc, cited }
    }

    pub fn is_user(&self) -> bool {
        self.message.role == "user"
    }
}

pub struct Pending {
    pub user: Doc,
    pub reply: String,
    pub doc: Doc,
}

pub struct ChatPane {
    pub chats: Vec<Chat>,
    pub active: Option<String>,
    pub messages: Vec<ShownMessage>,
    pub loading: bool,
    pub pending: Option<Pending>,
    pub draft: TextEdit,
    pub model: &'static str,
    pub error: Option<String>,
    pub saved: HashMap<String, String>,
    /// Replies on their way to Keep: a second key must not make a second note.
    pub saving: HashSet<String>,
    pub log: ListState,
    pub picker: Option<ListState>,
    /// A question that got no answer. The server may have it all the same.
    unanswered: Option<String>,
    seeded: bool,
}

impl ChatPane {
    fn new() -> ChatPane {
        ChatPane {
            chats: Vec::new(),
            active: None,
            messages: Vec::new(),
            loading: false,
            pending: None,
            draft: TextEdit::multiline().placeholder("Ask about your sources"),
            model: "deepseek",
            error: None,
            saved: HashMap::new(),
            saving: HashSet::new(),
            log: ListState::new(),
            picker: None,
            unanswered: None,
            seeded: false,
        }
    }

    pub fn sending(&self) -> bool {
        self.pending.is_some()
    }

    fn active_title(&self) -> Option<&str> {
        let id = self.active.as_deref()?;
        self.chats.iter().find(|c| c.id == id).map(|c| c.title.as_str())
    }

    pub fn shown_title(&self) -> String {
        self.active_title().filter(|t| !t.is_empty()).unwrap_or("New chat").to_owned()
    }
}

fn by_recent(mut list: Vec<Chat>) -> Vec<Chat> {
    list.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
    list
}

pub struct ShownInsight {
    pub insight: Insight,
    pub doc: Doc,
}

pub struct Reader {
    pub id: String,
    pub around: Option<u32>,
    pub source: Option<Source>,
    pub chunks: Vec<crate::wire::Chunk>,
    pub insights: Vec<ShownInsight>,
    pub transformations: Vec<Transformation>,
    pub chosen: usize,
    pub loading: bool,
    pub error: Option<String>,
    pub saved: HashMap<String, String>,
    pub saving: HashSet<String>,
    pub text: ListState,
    pub list: ListState,
    /// On a narrow screen: insights instead of the text.
    pub show_insights: bool,
    pub first: bool,
    /// Who gets the keyboard back.
    pub from: &'static str,
    /// Waiting for `next_poll` or an answer.
    pub next_poll: Instant,
    pub polling: bool,
    polls: u32,
    /// A poll failed: it does not ask again by itself.
    poll_off: bool,
}

impl Reader {
    pub fn has_earlier(&self) -> bool {
        self.chunks.first().is_some_and(|c| c.ordinal > 0)
    }

    pub fn has_later(&self) -> bool {
        match (self.chunks.last(), &self.source) {
            (Some(l), Some(s)) => l.ordinal + 1 < s.chunks,
            _ => false,
        }
    }

    pub fn wants_poll(&self) -> bool {
        !self.poll_off && self.source_or_insights_pending()
    }

    fn source_or_insights_pending(&self) -> bool {
        self.source.as_ref().is_some_and(|s| source_pending(&s.status))
            || self.insights.iter().any(|i| insight_pending(&i.insight.status))
    }

    fn show_chunks(&mut self, chunks: Vec<crate::wire::Chunk>, paging: Paging) {
        match paging {
            Paging::Open => {
                self.chunks = chunks;
                let at = self.around.and_then(|a| self.chunks.iter().position(|c| c.ordinal == a));
                self.text.invalidate();
                if let Some(i) = at {
                    self.text.select(i);
                }
            }
            Paging::Later => self.chunks.extend(chunks),
            Paging::Earlier => {
                // The same passage stays selected, with the new ones above it.
                let n = chunks.len();
                let mut all = chunks;
                all.append(&mut self.chunks);
                self.chunks = all;
                self.text.invalidate();
                self.text.select(self.text.selected.map_or(n, |s| s + n));
            }
        }
    }
}

pub struct Page {
    pub id: String,
    pub detail: Load<Detail>,
    pub not_found: bool,
    pub notice: Option<String>,
    pub tab: Tab,
    pub sources: ListState,
    pub notes: ListState,
    pub chat: ChatPane,
    pub sheet: Option<Sheet>,
    pub reader: Option<Reader>,
    pub confirm: Option<Confirm>,
    /// A dialog's widget had the keyboard, and it has gone.
    pub refocus: bool,
    next_poll: Instant,
    polling: bool,
    polls: u32,
    /// A poll failed: it does not ask again by itself. Try again turns it back on.
    poll_off: bool,
}

pub struct Notebooks {
    pub zone: Zone,
    link: Option<Link>,
    sign_in: Option<SignInView>,
    /// Starts the sign-in screen again when the token dies.
    start_sign_in: Option<Box<dyn FnMut() -> Option<SignInView>>>,
    /// Starts the worker once there is a token.
    start_worker: Option<Box<dyn FnMut() -> Option<Link>>>,
    pub sample: bool,
    pub archived: bool,
    pub lists: [Load<Vec<Notebook>>; 2],
    pub list: ListState,
    pub new_form: Option<NewForm>,
    pub page: Option<Page>,
    pub want_focus: Option<&'static str>,
    /// `want_focus` was asked for once already. In the pass after a dialog closes nothing can take the keyboard yet, so it is asked twice.
    pub focus_asked: Option<&'static str>,
    cached: Vec<Detail>,
    last_list: Option<Instant>,
    save_cache: Option<Box<dyn FnMut(&Cached)>>,
}

impl Notebooks {
    pub fn new(zone: Zone, link: Option<Link>) -> Notebooks {
        Notebooks {
            zone,
            link,
            sign_in: None,
            start_sign_in: None,
            start_worker: None,
            sample: false,
            archived: false,
            lists: [Load::Idle, Load::Idle],
            list: ListState::new(),
            new_form: None,
            page: None,
            want_focus: Some("notebooks"),
            focus_asked: None,
            cached: Vec::new(),
            last_list: None,
            save_cache: None,
        }
    }

    pub fn sample(mut self) -> Notebooks {
        self.sample = true;
        self
    }

    pub fn with_sign_in(
        mut self,
        start_sign_in: Box<dyn FnMut() -> Option<SignInView>>,
        start_worker: Box<dyn FnMut() -> Option<Link>>,
    ) -> Notebooks {
        self.start_sign_in = Some(start_sign_in);
        self.start_worker = Some(start_worker);
        if self.link.is_none() {
            self.sign_in = self.start_sign_in.as_mut().and_then(|f| f());
        }
        self
    }

    pub fn with_cache(mut self, save: Box<dyn FnMut(&Cached)>) -> Notebooks {
        self.save_cache = Some(save);
        self
    }

    pub fn signing_in(&mut self) -> Option<&mut SignInView> {
        self.sign_in.as_mut()
    }

    /// What was fetched last time, shown before any network, as `createCached` does on the web.
    pub fn restore(&mut self, cached: Cached) {
        self.lists[0] = Load::Loading(Some(cached.active));
        self.lists[1] = Load::Loading(Some(cached.archived));
        self.cached = cached.details;
        self.select_first();
    }

    fn select_first(&mut self) {
        let n = self.current_list().map_or(0, Vec::len);
        match self.list.selected {
            None if n > 0 => self.list.select(0),
            Some(i) if i >= n && n > 0 => self.list.select(n - 1),
            _ => {}
        }
    }

    pub fn current_list(&self) -> Option<&Vec<Notebook>> {
        self.lists[usize::from(self.archived)].value()
    }

    pub fn current_load(&self) -> &Load<Vec<Notebook>> {
        &self.lists[usize::from(self.archived)]
    }

    fn call(&mut self, call: Call) {
        if let Some(l) = &mut self.link {
            l(Job::Call(call));
        }
    }

    pub fn refresh(&mut self) {
        self.last_list = Some(Instant::now());
        let archived = self.archived;
        self.lists[usize::from(archived)].start();
        self.call(api::list(archived));
    }

    fn reload(&mut self) {
        let Some(p) = &mut self.page else { return };
        p.polling = true;
        p.polls = 0;
        p.poll_off = false;
        p.next_poll = Instant::now() + POLL;
        p.detail.start();
        let id = p.id.clone();
        self.call(api::detail(&id));
    }

    pub fn open_notebook(&mut self, id: &str) {
        let cached = self.cached.iter().find(|d| d.notebook.id == id).cloned();
        let mut page = Page {
            id: id.to_owned(),
            detail: cached.clone().map_or(Load::Idle, Load::Ready),
            not_found: false,
            notice: None,
            tab: Tab::Sources,
            sources: ListState::new(),
            notes: ListState::new(),
            chat: ChatPane::new(),
            sheet: None,
            reader: None,
            confirm: None,
            refocus: false,
            next_poll: Instant::now(),
            polling: false,
            polls: 0,
            poll_off: false,
        };
        if let Some(d) = cached {
            select_rows(&mut page, &d);
        }
        self.page = Some(page);
        self.want_focus = Some("sources");
        if let Some(d) = self.page.as_ref().and_then(|p| p.detail.value().cloned()) {
            self.seed_chat(&d);
        }
        self.reload();
    }

    /// `ChatColumn`'s first state: the most recent chat.
    fn seed_chat(&mut self, d: &Detail) {
        let Some(p) = &mut self.page else { return };
        if p.chat.sending() {
            return;
        }
        p.chat.chats = by_recent(d.chats.clone());
        if p.chat.seeded {
            return;
        }
        p.chat.seeded = true;
        p.chat.active = p.chat.chats.first().map(|c| c.id.clone());
        if let Some(c) = p.chat.active.clone() {
            self.load_chat(&c);
        }
    }

    fn load_chat(&mut self, chat: &str) {
        let Some(p) = &mut self.page else { return };
        p.chat.messages.clear();
        p.chat.saved.clear();
        p.chat.loading = true;
        p.chat.log.invalidate();
        self.call(api::chat(chat));
    }

    pub fn open_sheet(&mut self, kind: SheetKind) {
        let Some(p) = &mut self.page else { return };
        let excluded: Vec<String> = p.detail.value().map_or(Vec::new(), |d| d.notes.iter().map(|n| n.slug.clone()).collect());
        let sheet = match kind {
            SheetKind::Add => Sheet::Add(Box::new(AddForm {
                mode: AddMode::Link,
                url: TextEdit::new().placeholder("https://"),
                title: TextEdit::new(),
                text: TextEdit::multiline(),
                path: TextEdit::new().placeholder("/home/cua/Documents/notes.pdf"),
                picker: Picker::new(Vec::new()),
                error: None,
                busy: false,
                first: true,
            })),
            SheetKind::Write => Sheet::Write(WriteForm { title: TextEdit::new(), text: TextEdit::multiline(), error: None, busy: false, first: true }),
            SheetKind::Link => Sheet::Link(LinkForm { picker: Picker::new(excluded), error: None, busy: false, first: true }),
            SheetKind::Search => Sheet::Search(SearchForm {
                query: TextEdit::new().placeholder("Search sources, notes and insights"),
                hits: None,
                error: None,
                list: ListState::new(),
                due: None,
                first: true,
            }),
            SheetKind::Podcast => {
                let mut minutes = TextEdit::with_text(&DEFAULT_MINUTES.to_string());
                minutes.select_all();
                let form = PodcastForm {
                    episodes: Vec::new(),
                    speakers: Vec::new(),
                    episode: 0,
                    speaker: 0,
                    minutes,
                    briefing: TextEdit::multiline().placeholder("What should the episode focus on?"),
                    error: None,
                    busy: false,
                    list: ListState::new(),
                    open: None,
                    first: true,
                };
                p.sheet = Some(Sheet::Podcast(Box::new(form)));
                self.call(api::profiles_of_podcasts());
                return;
            }
            SheetKind::Settings => {
                let Some(d) = p.detail.value() else { return };
                Sheet::Settings(SettingsForm {
                    name: TextEdit::with_text(&d.notebook.name),
                    description: TextEdit::with_text(&d.notebook.description),
                    delete_exclusive: false,
                    error: None,
                    busy: false,
                    first: true,
                })
            }
        };
        p.sheet = Some(sheet);
    }

    pub fn open_reader(&mut self, source: &str, around: Option<u32>, from: &'static str) {
        let Some(p) = &mut self.page else { return };
        let keep = p.reader.as_ref().map(|r| (r.transformations.clone(), r.chosen));
        let (transformations, chosen) = keep.unwrap_or_default();
        p.reader = Some(Reader {
            id: source.to_owned(),
            around,
            source: None,
            chunks: Vec::new(),
            insights: Vec::new(),
            transformations,
            chosen,
            loading: true,
            error: None,
            saved: HashMap::new(),
            saving: HashSet::new(),
            text: ListState::new(),
            list: ListState::new(),
            show_insights: false,
            first: true,
            from,
            next_poll: Instant::now() + POLL,
            polling: false,
            polls: 0,
            poll_off: false,
        });
        self.call(api::source(source));
        self.call(api::insights(source));
        self.call(api::transformations());
    }

    fn load_passages(&mut self, source: &str, around: Option<u32>) {
        let query = around.map_or(format!("from=0&limit={PAGE}"), |a| format!("around={a}"));
        self.call(api::chunks(source, &query, Paging::Open));
    }

    /// `send` in ChatColumn.tsx. The question is on the screen at once and the reply streams in.
    pub fn send(&mut self) {
        let Some(p) = &mut self.page else { return };
        let text = p.chat.draft.text.trim().to_owned();
        if text.is_empty() || p.chat.sending() {
            return;
        }
        p.chat.error = None;
        p.chat.draft.clear();
        p.chat.pending = Some(Pending { user: md::parse(&text), reply: String::new(), doc: Doc::default() });
        p.chat.log.invalidate();
        p.chat.log.scroll_to_end();
        let job = Job::Ask { notebook: p.id.clone(), chat: p.chat.active.clone(), text, model: p.chat.model };
        if let Some(l) = &mut self.link {
            l(job);
        }
    }

    pub fn apply(&mut self, todo: Todo) {
        match todo {
            Todo::Back => {
                self.page = None;
                self.want_focus = Some("notebooks");
                self.refresh();
            }
            Todo::Reload => self.reload(),
            Todo::ReloadList => self.refresh(),
            Todo::Filter(archived) => {
                self.archived = archived;
                self.list = ListState::new();
                self.refresh();
                self.select_first();
            }
            Todo::OpenNotebook(i) => {
                if let Some(id) = self.current_list().and_then(|l| l.get(i)).map(|n| n.id.clone()) {
                    self.open_notebook(&id);
                }
            }
            Todo::NewNotebook => {
                self.new_form = Some(NewForm { name: TextEdit::new(), description: TextEdit::multiline(), error: None, busy: false, first: true });
            }
            Todo::CreateNotebook => {
                let Some(f) = &mut self.new_form else { return };
                let name = f.name.text.trim().to_owned();
                if name.is_empty() || f.busy {
                    return;
                }
                f.busy = true;
                f.error = None;
                let description = f.description.text.trim().to_owned();
                self.call(api::create(&name, &description));
            }
            Todo::Tab(tab) => {
                if let Some(p) = &mut self.page {
                    p.tab = tab;
                }
                self.want_focus = Some(match tab {
                    Tab::Sources => "sources",
                    Tab::Notes => "notes",
                    Tab::Chat => "ask",
                });
            }
            Todo::Sheet(kind) => self.open_sheet(kind),
            Todo::CloseSheet => {
                if let Some(p) = &mut self.page {
                    p.sheet = None;
                    p.chat.picker = None;
                }
                self.want_focus = self.page_focus();
            }
            Todo::OpenSource(i) => {
                if let Some(id) = self.source_at(i).map(|s| s.id.clone()) {
                    self.open_reader(&id, None, "sources");
                }
            }
            Todo::ToggleContext(i) => {
                let Some((source, next)) = self.source_at(i).map(|s| (s.id.clone(), next_context(s.context.as_deref().unwrap_or("full")))) else {
                    return;
                };
                let id = self.page_id();
                self.call(api::set_context(&id, &source, next));
            }
            Todo::Retry(i) => {
                if let Some(id) = self.source_at(i).map(|s| s.id.clone()) {
                    self.call(api::retry_source(&id));
                    // The button goes when the source stops being failed.
                    self.want_focus = Some("sources");
                }
            }
            Todo::AskRemoveSource(i) => {
                if let Some((id, title)) = self.source_at(i).map(|s| (s.id.clone(), s.title.clone())) {
                    self.confirm(
                        format!("Remove \"{title}\" from this notebook? A source no other notebook uses is deleted."),
                        "Remove",
                        Todo::RemoveSource(id),
                    );
                }
            }
            Todo::RemoveSource(source) => {
                let id = self.page_id();
                self.call(api::remove_source(&id, &source));
            }
            Todo::OpenNote(i) => {
                let Some(p) = &mut self.page else { return };
                if let Some(n) = p.detail.value().and_then(|d| d.notes.get(i)).cloned() {
                    p.sheet = Some(Sheet::Note(n));
                }
            }
            Todo::Unlink(i) => {
                let slug = self.page.as_ref().and_then(|p| p.detail.value()).and_then(|d| d.notes.get(i)).map(|n| n.slug.clone());
                if let Some(slug) = slug {
                    let id = self.page_id();
                    self.call(api::unlink_note(&id, &slug));
                }
            }
            Todo::Send => self.send(),
            Todo::CycleModel => {
                if let Some(p) = &mut self.page {
                    p.chat.model = if p.chat.model == "deepseek" { "qwen" } else { "deepseek" };
                }
            }
            Todo::NewChat => {
                if let Some(p) = &mut self.page {
                    p.chat.active = None;
                    p.chat.messages.clear();
                    p.chat.saved.clear();
                    p.chat.loading = false;
                    p.chat.log.invalidate();
                }
                self.want_focus = Some("ask");
            }
            Todo::OpenPicker => {
                let Some(p) = &mut self.page else { return };
                let mut l = ListState::new();
                let at = p.chat.active.as_ref().and_then(|a| p.chat.chats.iter().position(|c| c.id == *a));
                l.select(at.unwrap_or(0));
                p.chat.picker = Some(l);
                self.want_focus = Some("chat-picker");
            }
            Todo::PickChat(i) => {
                let Some(p) = &mut self.page else { return };
                p.chat.picker = None;
                let id = p.chat.chats.get(i).map(|c| c.id.clone());
                if let Some(id) = id {
                    p.chat.active = Some(id.clone());
                    self.load_chat(&id);
                }
                self.want_focus = Some("ask");
            }
            Todo::OpenRename => {
                let Some(p) = &mut self.page else { return };
                if p.chat.active.is_some() {
                    let mut name = TextEdit::with_text(&p.chat.shown_title());
                    name.select_all();
                    p.sheet = Some(Sheet::Rename(name, true));
                }
            }
            Todo::SaveRename => {
                let Some(p) = &mut self.page else { return };
                let (Some(chat), Some(Sheet::Rename(e, _))) = (p.chat.active.clone(), &p.sheet) else { return };
                let title = e.text.trim().to_owned();
                p.sheet = None;
                if !title.is_empty() {
                    self.call(api::rename_chat(&chat, &title));
                }
                self.want_focus = Some("ask");
            }
            Todo::AskDeleteChat => {
                let Some(p) = &self.page else { return };
                if p.chat.active.is_some() {
                    let title = p.chat.shown_title();
                    self.confirm(format!("Delete the chat \"{title}\"?"), "Delete", Todo::DeleteChat);
                }
            }
            Todo::DeleteChat => {
                let chat = self.page.as_ref().and_then(|p| p.chat.active.clone());
                if let Some(c) = chat {
                    self.call(api::remove_chat(&c));
                }
            }
            Todo::Cite(i, k) => self.open_citation(i, k),
            Todo::SaveReply(i) => {
                let Some(p) = &mut self.page else { return };
                let (Some(chat), Some(m)) = (p.chat.active.clone(), p.chat.messages.get(i)) else { return };
                let message = m.message.id.clone();
                if p.chat.saved.contains_key(&message) || !p.chat.saving.insert(message.clone()) {
                    return;
                }
                p.chat.error = None;
                self.call(api::message_to_note(&chat, &message));
            }
            Todo::CloseReader => {
                let Some(p) = &mut self.page else { return };
                let from = p.reader.take().map(|r| r.from);
                self.want_focus = from;
            }
            Todo::CycleTransformation => {
                if let Some(r) = self.page.as_mut().and_then(|p| p.reader.as_mut()) {
                    let n = r.transformations.len().max(1);
                    r.chosen = (r.chosen + 1) % n;
                }
            }
            Todo::RunTransformation => {
                let Some(r) = self.page.as_mut().and_then(|p| p.reader.as_mut()) else { return };
                let Some(t) = r.transformations.get(r.chosen).map(|t| t.id.clone()) else { return };
                r.error = None;
                r.polls = 0;
                r.poll_off = false;
                let id = r.id.clone();
                self.call(api::run_transformation(&id, &t));
            }
            Todo::SaveInsight(i) => {
                let Some(r) = self.page.as_mut().and_then(|p| p.reader.as_mut()) else { return };
                let Some(id) = r.insights.get(i).map(|x| x.insight.id.clone()) else { return };
                if r.saved.contains_key(&id) || !r.saving.insert(id.clone()) {
                    return;
                }
                r.error = None;
                self.call(api::insight_to_note(&id));
            }
            Todo::AskDeleteInsight(i) => {
                let found = self.page.as_ref().and_then(|p| p.reader.as_ref()).and_then(|r| r.insights.get(i));
                if let Some((id, title)) = found.map(|x| (x.insight.id.clone(), x.insight.title.clone())) {
                    self.confirm(format!("Delete the insight \"{title}\"?"), "Delete", Todo::DeleteInsight(id));
                }
            }
            Todo::DeleteInsight(id) => self.call(api::remove_insight(&id)),
            Todo::MorePassages(paging) => {
                let Some(r) = self.page.as_ref().and_then(|p| p.reader.as_ref()) else { return };
                let id = r.id.clone();
                let query = match paging {
                    Paging::Later => r.chunks.last().map(|c| format!("from={}&limit={PAGE}", c.ordinal + 1)),
                    _ => r.chunks.first().map(|c| {
                        let from = c.ordinal.saturating_sub(PAGE);
                        format!("from={from}&limit={}", c.ordinal - from)
                    }),
                };
                if let Some(q) = query {
                    self.call(api::chunks(&id, &q, paging));
                }
            }
            Todo::ReaderPane(insights) => {
                if let Some(r) = self.page.as_mut().and_then(|p| p.reader.as_mut()) {
                    r.show_insights = insights;
                }
            }
            Todo::AddMode(mode) => {
                if let Some(Sheet::Add(f)) = self.page.as_mut().and_then(|p| p.sheet.as_mut()) {
                    f.mode = mode;
                    f.first = true;
                }
            }
            Todo::SubmitAdd => self.submit_add(),
            Todo::SubmitWrite => {
                let Some(p) = &mut self.page else { return };
                let Some(Sheet::Write(f)) = &mut p.sheet else { return };
                let (title, text) = (f.title.text.trim().to_owned(), f.text.text.clone());
                if title.is_empty() || text.trim().is_empty() || f.busy {
                    return;
                }
                f.busy = true;
                f.error = None;
                let id = p.id.clone();
                self.call(api::write_note(&id, &title, &text));
            }
            Todo::SubmitLink(slug) => {
                let Some(p) = &mut self.page else { return };
                let id = p.id.clone();
                match &mut p.sheet {
                    Some(Sheet::Link(f)) => {
                        if f.busy {
                            return;
                        }
                        f.busy = true;
                        f.error = None;
                        self.call(api::link_note(&id, &slug));
                    }
                    Some(Sheet::Add(f)) => {
                        if f.busy {
                            return;
                        }
                        f.busy = true;
                        f.error = None;
                        self.call(api::add_source(&id, json!({"kind": "note", "slug": slug})));
                    }
                    _ => {}
                }
            }
            Todo::OpenHit(i) => self.open_hit(i),
            Todo::CycleEpisode | Todo::CycleSpeaker => {
                if let Some(Sheet::Podcast(f)) = self.page.as_mut().and_then(|p| p.sheet.as_mut()) {
                    if todo == Todo::CycleEpisode {
                        f.episode = (f.episode + 1) % f.episodes.len().max(1);
                    } else {
                        f.speaker = (f.speaker + 1) % f.speakers.len().max(1);
                    }
                }
            }
            Todo::CreatePodcast => {
                let Some(p) = &mut self.page else { return };
                let id = p.id.clone();
                let Some(Sheet::Podcast(f)) = &mut p.sheet else { return };
                if f.busy {
                    return;
                }
                f.busy = true;
                f.error = None;
                let minutes = f.minutes.text.trim().parse::<f64>().ok().filter(|m| *m >= 0.5).map_or(DEFAULT_MINUTES, |m| m.round() as u32);
                let mut body = json!({"minutes": minutes.clamp(MIN_MINUTES, MAX_MINUTES)});
                if let Some(e) = f.episodes.get(f.episode) {
                    body["episode_profile"] = json!(e.id);
                }
                if let Some(s) = f.speakers.get(f.speaker) {
                    body["speaker_profile"] = json!(s.id);
                }
                if !f.briefing.text.trim().is_empty() {
                    body["briefing"] = json!(f.briefing.text.trim());
                }
                self.call(api::create_podcast(&id, body));
            }
            Todo::AskDeletePodcast(i) => {
                let found = self.page.as_ref().and_then(|p| p.detail.value()).and_then(|d| d.podcasts.get(i));
                if let Some((id, title)) = found.map(|x| (x.id.clone(), x.title.clone())) {
                    self.confirm(format!("Delete the episode \"{title}\"?"), "Delete", Todo::DeletePodcast(id));
                }
            }
            Todo::DeletePodcast(id) => self.call(api::remove_podcast(&id)),
            Todo::ToggleTranscript(i) => {
                let Some(p) = &mut self.page else { return };
                let id = p.detail.value().and_then(|d| d.podcasts.get(i)).map(|x| x.id.clone());
                if let Some(Sheet::Podcast(f)) = &mut p.sheet {
                    f.open = if f.open == id { None } else { id };
                    f.list.invalidate();
                }
            }
            Todo::SaveSettings => {
                let Some(p) = &mut self.page else { return };
                let id = p.id.clone();
                let Some(Sheet::Settings(f)) = &mut p.sheet else { return };
                let name = f.name.text.trim().to_owned();
                if name.is_empty() || f.busy {
                    return;
                }
                f.busy = true;
                f.error = None;
                let description = f.description.text.trim().to_owned();
                self.call(api::save_settings(&id, &name, &description));
            }
            Todo::Archive => {
                let Some(p) = &mut self.page else { return };
                let id = p.id.clone();
                let archived = p.detail.value().is_some_and(|d| d.notebook.archived);
                if let Some(Sheet::Settings(f)) = &mut p.sheet {
                    f.busy = true;
                    f.error = None;
                }
                self.call(api::archive(&id, !archived));
            }
            Todo::AskDeleteNotebook => {
                let name = self.page.as_ref().and_then(|p| p.detail.value()).map(|d| d.notebook.name.clone());
                if let Some(name) = name {
                    self.confirm(format!("Delete the notebook \"{name}\"? This can't be undone."), "Delete notebook", Todo::DeleteNotebook);
                }
            }
            Todo::DeleteNotebook => {
                let Some(p) = &mut self.page else { return };
                let id = p.id.clone();
                let exclusive = matches!(&p.sheet, Some(Sheet::Settings(f)) if f.delete_exclusive);
                if let Some(Sheet::Settings(f)) = &mut p.sheet {
                    f.busy = true;
                    f.error = None;
                }
                self.call(api::delete(&id, exclusive));
            }
            Todo::CloseConfirm => {
                if let Some(p) = &mut self.page {
                    p.confirm = None;
                    p.refocus = true;
                }
            }
        }
    }

    fn page_id(&self) -> String {
        self.page.as_ref().map_or(String::new(), |p| p.id.clone())
    }

    fn page_focus(&self) -> Option<&'static str> {
        self.page.as_ref().map(|p| match p.tab {
            Tab::Sources => "sources",
            Tab::Notes => "notes",
            Tab::Chat => "ask",
        })
    }

    fn source_at(&self, i: usize) -> Option<&Source> {
        self.page.as_ref()?.detail.value()?.sources.get(i)
    }

    fn confirm(&mut self, text: String, yes: &str, action: Todo) {
        if let Some(p) = &mut self.page {
            p.confirm = Some(Confirm { text, yes: yes.to_owned(), action, first: true });
        }
    }

    fn submit_add(&mut self) {
        let Some(p) = &mut self.page else { return };
        let id = p.id.clone();
        let Some(Sheet::Add(f)) = &mut p.sheet else { return };
        if !f.valid() || f.busy {
            return;
        }
        let body = match f.mode {
            AddMode::Link => json!({"kind": "url", "url": f.url.text.trim()}),
            AddMode::Text => {
                let mut b = json!({"kind": "text", "text": f.text.text});
                if !f.title.text.trim().is_empty() {
                    b["title"] = json!(f.title.text.trim());
                }
                b
            }
            AddMode::Path => json!({"kind": "sandbox", "path": f.path.text.trim()}),
            AddMode::File | AddMode::Note => return,
        };
        f.busy = true;
        f.error = None;
        self.call(api::add_source(&id, body));
    }

    /// `openCitation`: the source at the passage, or at the top for an insight's citation.
    fn open_citation(&mut self, i: usize, k: usize) {
        let Some(p) = &self.page else { return };
        let Some(c) = p.chat.messages.get(i).and_then(|m| m.cited.get(k)) else { return };
        let source = c.source_id.clone().or_else(|| ref_source_id(&c.reference));
        let around = if c.reference.starts_with("source:") { c.chunk.or_else(|| chunk_ordinal(&c.reference)) } else { None };
        if let Some(s) = source {
            self.open_reader(&s, around, "log");
        }
    }

    /// `open` in SearchSheet.tsx. A note has no reader, and opens in Keep on the web: here its excerpt shows.
    fn open_hit(&mut self, i: usize) {
        let Some(p) = &mut self.page else { return };
        let Some(Sheet::Search(f)) = &p.sheet else { return };
        let Some(hit) = f.hits.as_ref().and_then(|h| h.get(i)).cloned() else { return };
        if let (true, Some(slug)) = (hit.kind == "note", hit.slug.clone()) {
            p.sheet = Some(Sheet::Note(NoteLink { slug, title: hit.title, excerpt: hit.excerpt }));
            return;
        }
        let source = hit.source_id.clone().or_else(|| ref_source_id(&hit.reference));
        let around = if hit.kind == "source" { chunk_ordinal(&hit.reference) } else { None };
        let Some(source) = source else { return };
        p.sheet = None;
        let from = self.page_focus().unwrap_or("sources");
        self.open_reader(&source, around, from);
    }

    /// What runs by the clock: the polls, and the waits after a key in a search box. Says when to look again.
    pub fn tick(&mut self, ui: &mut Ui) {
        let now = Instant::now();
        if self.page.is_none() {
            if self.last_list.is_none_or(|t| t.elapsed() >= REFRESH) && self.link.is_some() {
                self.refresh();
            }
            ui.wake_after(REFRESH);
            return;
        }
        let mut calls: Vec<Call> = Vec::new();
        let Some(p) = &mut self.page else { return };
        let id = p.id.clone();
        let mut wake: Option<Duration> = None;
        let mut soon = |d: Duration| wake = Some(wake.map_or(d, |w: Duration| w.min(d)));
        match &mut p.sheet {
            Some(Sheet::Search(f)) => {
                if let Some(due) = f.due {
                    if due <= now {
                        f.due = None;
                        calls.push(api::search(&id, f.query.text.trim()));
                    } else {
                        soon(due - now);
                    }
                }
            }
            Some(Sheet::Link(LinkForm { picker, .. })) => tick_picker(picker, now, &mut calls, &mut soon),
            Some(Sheet::Add(f)) if f.mode == AddMode::Note => tick_picker(&mut f.picker, now, &mut calls, &mut soon),
            _ => {}
        }
        if let Some(r) = &mut p.reader {
            if r.wants_poll() {
                let gap = poll_gap(r.polls);
                if r.next_poll <= now && !r.polling {
                    r.polling = true;
                    r.polls += 1;
                    r.next_poll = now + gap;
                    if r.source.as_ref().is_some_and(|s| source_pending(&s.status)) {
                        calls.push(api::source(&r.id));
                    }
                    if r.insights.iter().any(|i| insight_pending(&i.insight.status)) {
                        calls.push(api::insights(&r.id));
                    }
                }
                soon(gap);
            }
        } else if !p.poll_off && p.detail.value().is_some_and(Detail::has_pending) {
            let gap = poll_gap(p.polls);
            if p.next_poll <= now && !p.polling {
                p.polling = true;
                p.polls += 1;
                p.next_poll = now + gap;
                calls.push(api::detail(&id));
            }
            soon(gap);
        }
        if let Some(w) = wake {
            ui.wake_after(w);
        }
        for c in calls {
            self.call(c);
        }
    }

    fn signed_out(&mut self) {
        crate::account::rejected();
        self.link = None;
        self.page = None;
        self.new_form = None;
        self.lists = [Load::Idle, Load::Idle];
        self.cached.clear();
        self.sign_in = self.start_sign_in.as_mut().and_then(|f| f());
    }

    fn signed_in(&mut self) {
        self.sign_in = None;
        self.link = self.start_worker.as_mut().and_then(|f| f());
        self.want_focus = Some("notebooks");
        self.refresh();
    }

    fn remember(&mut self, d: &Detail) {
        self.cached.retain(|c| c.notebook.id != d.notebook.id);
        self.cached.insert(0, d.clone());
        self.cached.truncate(KEPT_DETAILS);
    }

    fn done(&mut self, tag: Tag, r: Result<Answer, Failure>) {
        match (tag, r) {
            (Tag::List { archived }, r) => {
                let slot = &mut self.lists[usize::from(archived)];
                let had_rows = slot.value().is_some_and(|l| !l.is_empty());
                match r {
                    Ok(Answer::List(l)) => slot.finish(Ok(l)),
                    Err(e) => slot.finish(Err(e.text())),
                    Ok(_) => {}
                }
                self.select_first();
                // A list with no rows was not on screen to take the keyboard.
                if !had_rows && archived == self.archived && self.page.is_none() && self.new_form.is_none() {
                    self.want_focus = Some("notebooks");
                }
            }
            (Tag::Created, Ok(Answer::Notebook(nb))) => {
                self.new_form = None;
                self.open_notebook(&nb.id);
            }
            (Tag::Created, Err(e)) => {
                if let Some(f) = &mut self.new_form {
                    f.busy = false;
                    f.error = Some(e.text());
                }
            }
            (Tag::Detail { notebook }, r) => self.detail_done(&notebook, r),
            (Tag::SettingsSaved | Tag::Archived, r) => {
                let Some(p) = &mut self.page else { return };
                match r {
                    Ok(_) => {
                        p.sheet = None;
                        self.reload();
                    }
                    Err(e) => set_sheet_error(p, e.text()),
                }
            }
            (Tag::Deleted, r) => match r {
                Ok(_) => {
                    let id = self.page_id();
                    self.cached.retain(|c| c.notebook.id != id);
                    self.page = None;
                    self.want_focus = Some("notebooks");
                    self.refresh();
                }
                Err(e) => {
                    if let Some(p) = &mut self.page {
                        set_sheet_error(p, e.text());
                    }
                }
            },
            (Tag::SourceAdded, r) => {
                let Some(p) = &mut self.page else { return };
                match r {
                    Ok(_) => {
                        p.sheet = None;
                        self.want_focus = Some("sources");
                        self.reload();
                    }
                    Err(e) => set_sheet_error(p, e.text()),
                }
            }
            (Tag::SourceChanged | Tag::NoteUnlinked | Tag::PodcastDeleted | Tag::PodcastMade, r) => {
                let podcast_made = r.as_ref().is_ok_and(|a| matches!(a, Answer::Podcast(_)));
                let Some(p) = &mut self.page else { return };
                match r {
                    Ok(_) => {}
                    Err(e) => match &mut p.sheet {
                        Some(Sheet::Podcast(f)) => f.error = Some(e.text()),
                        _ => p.notice = Some(e.text()),
                    },
                }
                if let Some(Sheet::Podcast(f)) = &mut p.sheet {
                    f.busy = false;
                    if podcast_made {
                        f.briefing.clear();
                    }
                    f.list.invalidate();
                }
                self.reload();
            }
            (Tag::Source { source }, r) => self.source_done(&source, r),
            (Tag::Chunks { source, paging }, r) => {
                let Some(rd) = self.reader_of(&source) else { return };
                match r {
                    Ok(Answer::Chunks(c)) => {
                        rd.show_chunks(c, paging);
                        if paging == Paging::Open && !rd.show_insights {
                            self.want_focus = Some("passages");
                        }
                    }
                    Err(e) => rd.error = Some(e.text()),
                    Ok(_) => {}
                }
            }
            (Tag::Insights { source }, r) => {
                let Some(rd) = self.reader_of(&source) else { return };
                rd.polling = false;
                match r {
                    Ok(Answer::Insights(list)) => {
                        rd.insights = list.into_iter().map(shown_insight).collect();
                        rd.list.invalidate();
                        select_insight(rd);
                    }
                    Err(_) => rd.poll_off = true,
                    Ok(_) => {}
                }
            }
            (Tag::InsightRun { source }, r) => {
                let Some(rd) = self.reader_of(&source) else { return };
                match r {
                    Ok(Answer::Insight(i)) => {
                        rd.insights.insert(0, shown_insight(*i));
                        rd.list.invalidate();
                        select_insight(rd);
                        rd.next_poll = Instant::now() + POLL;
                    }
                    Err(e) => rd.error = Some(e.text()),
                    Ok(_) => {}
                }
            }
            (Tag::InsightNote { insight }, r) => {
                let Some(rd) = self.page.as_mut().and_then(|p| p.reader.as_mut()) else { return };
                rd.saving.remove(&insight);
                match r {
                    Ok(Answer::Link(l)) => {
                        rd.saved.insert(insight, l.slug);
                        // Its button has become a line of text.
                        self.want_focus = Some("insights");
                        self.reload();
                    }
                    Err(e) => rd.error = Some(e.text()),
                    Ok(_) => {}
                }
            }
            (Tag::InsightDeleted { insight }, r) => {
                let Some(rd) = self.page.as_mut().and_then(|p| p.reader.as_mut()) else { return };
                match r {
                    Ok(_) => {
                        rd.insights.retain(|i| i.insight.id != insight);
                        rd.list.invalidate();
                        select_insight(rd);
                    }
                    Err(e) => rd.error = Some(e.text()),
                }
            }
            (Tag::Transformations, r) => {
                let Some(rd) = self.page.as_mut().and_then(|p| p.reader.as_mut()) else { return };
                if let Ok(Answer::Transformations(t)) = r {
                    if rd.transformations.is_empty() {
                        rd.chosen = t.iter().position(|x| x.id == "t-dense-summary").unwrap_or(0);
                    }
                    rd.transformations = t;
                }
            }
            (Tag::NoteSaved, r) => {
                let Some(p) = &mut self.page else { return };
                match r {
                    Ok(_) => {
                        p.sheet = None;
                        self.want_focus = Some("notes");
                        self.reload();
                    }
                    Err(e) => set_sheet_error(p, e.text()),
                }
            }
            (Tag::Search { query }, r) => {
                let Some(Sheet::Search(f)) = self.page.as_mut().and_then(|p| p.sheet.as_mut()) else { return };
                if f.query.text.trim() != query {
                    return;
                }
                match r {
                    Ok(Answer::Hits(h)) => {
                        f.error = None;
                        f.list = ListState::new();
                        if !h.is_empty() {
                            f.list.select(0);
                        }
                        f.hits = Some(h);
                    }
                    Err(e) => f.error = Some(e.text()),
                    Ok(_) => {}
                }
            }
            (Tag::PickNotes { query }, r) => {
                let Some(p) = &mut self.page else { return };
                let picker = match &mut p.sheet {
                    Some(Sheet::Link(f)) => &mut f.picker,
                    Some(Sheet::Add(f)) => &mut f.picker,
                    _ => return,
                };
                if picker.query.text != query {
                    return;
                }
                match r {
                    Ok(Answer::Notes(n)) => {
                        picker.error = None;
                        picker.notes = Some(n);
                        picker.list = ListState::new();
                        picker.list.select(0);
                    }
                    Err(e) => picker.error = Some(e.text()),
                    Ok(_) => {}
                }
            }
            (Tag::Chats { notebook }, r) => {
                let Some(p) = &mut self.page else { return };
                if p.id == notebook {
                    // The list is only a convenience; the next action refreshes it.
                    if let Ok(Answer::Chats(c)) = r {
                        p.chat.chats = by_recent(c);
                    }
                }
            }
            (Tag::ChatLoaded { chat }, r) => {
                let Some(p) = &mut self.page else { return };
                if p.chat.active.as_deref() != Some(chat.as_str()) || p.chat.sending() {
                    return;
                }
                p.chat.loading = false;
                match r {
                    Ok(Answer::Chat(c)) => {
                        p.chat.messages = c.messages.into_iter().map(ShownMessage::new).collect();
                        p.chat.log.invalidate();
                        p.chat.log.scroll_to_end();
                        // The server finished a reply this client stopped waiting for. Sending it again would ask twice.
                        if let Some(q) = p.chat.unanswered.take() {
                            let arrived = p.chat.messages.iter().rev().find(|m| m.is_user()).is_some_and(|m| m.message.text == q);
                            if arrived && p.chat.draft.text == q {
                                p.chat.draft.clear();
                                p.chat.error = None;
                            }
                        }
                    }
                    Err(e) => p.chat.error = Some(e.text()),
                    Ok(_) => {}
                }
            }
            (Tag::ChatCreated { notebook }, r) => {
                let Some(p) = &mut self.page else { return };
                if p.id == notebook {
                    if let Ok(Answer::ChatRow(c)) = r {
                        p.chat.active = Some(c.id.clone());
                        p.chat.chats.insert(0, *c);
                    }
                }
            }
            (Tag::ChatRenamed { chat }, r) => {
                let Some(p) = &mut self.page else { return };
                match r {
                    Ok(Answer::ChatRow(c)) => {
                        if let Some(x) = p.chat.chats.iter_mut().find(|x| x.id == chat) {
                            x.title = c.title;
                        }
                    }
                    Err(e) => p.chat.error = Some(e.text()),
                    Ok(_) => {}
                }
            }
            (Tag::ChatDeleted { chat }, r) => {
                let Some(p) = &mut self.page else { return };
                match r {
                    Ok(_) => {
                        p.chat.chats.retain(|c| c.id != chat);
                        let next = p.chat.chats.first().map(|c| c.id.clone());
                        p.chat.active = next.clone();
                        p.chat.messages.clear();
                        p.chat.log.invalidate();
                        if let Some(c) = next {
                            self.load_chat(&c);
                        }
                    }
                    Err(e) => p.chat.error = Some(e.text()),
                }
            }
            (Tag::Asked { chat, text }, r) => self.asked(chat, text, r),
            (Tag::MessageNote { message }, r) => {
                let Some(p) = &mut self.page else { return };
                p.chat.saving.remove(&message);
                match r {
                    Ok(Answer::Link(l)) => {
                        p.chat.saved.insert(message, l.slug);
                        p.chat.log.invalidate();
                        self.reload();
                    }
                    Err(e) => p.chat.error = Some(e.text()),
                    Ok(_) => {}
                }
            }
            (Tag::Profiles, r) => {
                let Some(Sheet::Podcast(f)) = self.page.as_mut().and_then(|p| p.sheet.as_mut()) else { return };
                match r {
                    Ok(Answer::Profiles(e, s)) => {
                        f.episodes = e;
                        f.speakers = s;
                    }
                    Err(e) => f.error = Some(e.text()),
                    Ok(_) => {}
                }
            }
            _ => {}
        }
    }

    fn reader_of(&mut self, source: &str) -> Option<&mut Reader> {
        self.page.as_mut()?.reader.as_mut().filter(|r| r.id == source)
    }

    fn detail_done(&mut self, notebook: &str, r: Result<Answer, Failure>) {
        let Some(p) = &mut self.page else { return };
        if p.id != notebook {
            return;
        }
        p.polling = false;
        match r {
            Ok(Answer::Detail(d)) => {
                let d = *d;
                p.not_found = false;
                select_rows(p, &d);
                let had_rows = p.detail.value().is_some_and(|x| !x.sources.is_empty());
                p.detail.finish(Ok(d.clone()));
                if !had_rows && p.tab == Tab::Sources && p.sheet.is_none() && p.reader.is_none() && p.confirm.is_none() {
                    self.want_focus = Some("sources");
                }
                self.remember(&d);
                self.seed_chat(&d);
            }
            Err(e) => {
                p.not_found = e.not_found();
                p.detail.finish(Err(e.text()));
                p.poll_off = true;
            }
            Ok(_) => {}
        }
    }

    fn source_done(&mut self, source: &str, r: Result<Answer, Failure>) {
        let Some(rd) = self.reader_of(source) else { return };
        rd.polling = false;
        rd.loading = false;
        match r {
            Ok(Answer::Source(s)) => {
                let was_ready = rd.source.as_ref().is_some_and(|x| x.status == "ready");
                let ready = s.status == "ready";
                rd.source = Some(*s);
                let around = rd.around;
                if ready && !was_ready {
                    self.load_passages(source, around);
                }
            }
            Err(e) => {
                rd.error = Some(e.text());
                rd.poll_off = true;
            }
            Ok(_) => {}
        }
    }

    fn asked(&mut self, chat: Option<String>, text: String, r: Result<Answer, Failure>) {
        let Some(p) = &mut self.page else { return };
        p.chat.pending = None;
        p.chat.log.invalidate();
        p.chat.log.scroll_to_end();
        let notebook = p.id.clone();
        match r {
            Ok(Answer::Exchange(x)) => {
                p.chat.messages.push(ShownMessage::new(x.user));
                p.chat.messages.push(ShownMessage::new(x.reply));
                self.call(api::chats(&notebook));
            }
            Err(e) => {
                p.chat.error = Some(e.text());
                p.chat.draft.set(&text);
                // The server keeps a reply it finished after this client went away, so look.
                if let Some(c) = chat {
                    p.chat.unanswered = Some(text);
                    p.chat.loading = true;
                    self.call(api::chat(&c));
                }
            }
            Ok(_) => {}
        }
    }

    pub fn message_delta(&mut self, piece: &str) {
        let Some(pending) = self.page.as_mut().and_then(|p| p.chat.pending.as_mut()) else { return };
        pending.reply.push_str(piece);
        pending.doc = md::parse(&pending.reply);
        if let Some(p) = &mut self.page {
            p.chat.log.invalidate();
            p.chat.log.scroll_to_end();
        }
    }
}

fn tick_picker(picker: &mut Picker, now: Instant, calls: &mut Vec<Call>, soon: &mut impl FnMut(Duration)) {
    if let Some(due) = picker.due {
        if due <= now {
            picker.due = None;
            calls.push(api::pick_notes(&picker.query.text));
        } else {
            soon(due - now);
        }
    }
}

/// Save and Delete act on the selected insight.
fn select_insight(r: &mut Reader) {
    let n = r.insights.len();
    match r.list.selected {
        None if n > 0 => r.list.select(0),
        Some(i) if i >= n && n > 0 => r.list.select(n - 1),
        _ => {}
    }
}

fn shown_insight(insight: Insight) -> ShownInsight {
    let doc = md::parse(&insight.text);
    ShownInsight { insight, doc }
}

/// Enter acts on the selected row: without one, the keyboard could not open anything.
fn select_rows(p: &mut Page, d: &Detail) {
    for (st, n) in [(&mut p.sources, d.sources.len()), (&mut p.notes, d.notes.len())] {
        match st.selected {
            None if n > 0 => st.select(0),
            Some(i) if i >= n && n > 0 => st.select(n - 1),
            _ => {}
        }
    }
}

fn set_sheet_error(p: &mut Page, text: String) {
    match &mut p.sheet {
        Some(Sheet::Add(f)) => {
            f.busy = false;
            f.error = Some(text);
        }
        Some(Sheet::Write(f)) => {
            f.busy = false;
            f.error = Some(text);
        }
        Some(Sheet::Link(f)) => {
            f.busy = false;
            f.error = Some(text);
        }
        Some(Sheet::Settings(f)) => {
            f.busy = false;
            f.error = Some(text);
        }
        _ => p.notice = Some(text),
    }
}

impl App for Notebooks {
    type Msg = Msg;

    fn view(&mut self, ui: &mut Ui) {
        screens::view(self, ui);
    }

    fn message(&mut self, msg: Msg) {
        match msg {
            Msg::Changed => {}
            Msg::SignedIn => self.signed_in(),
            Msg::SignedOut => self.signed_out(),
            Msg::Delta(piece) => self.message_delta(&piece),
            Msg::Done(tag, r) => self.done(tag, r),
        }
    }

    fn on_quit(&mut self) {
        let Some(save) = &mut self.save_cache else { return };
        let list = |l: &Load<Vec<Notebook>>| l.value().cloned().unwrap_or_default();
        save(&Cached { active: list(&self.lists[0]), archived: list(&self.lists[1]), details: self.cached.clone() });
    }
}
