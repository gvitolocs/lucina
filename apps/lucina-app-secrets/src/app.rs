//! The screens: `web/src/views/Secrets.tsx` (names and places, values stay private) and the paste-and-OK card of
//! `components/SecretCard.tsx`, the same flow as Chat's. There is no "show" button: this app has no way to display a value,
//! typed or stored, and the server has no route that returns one.
//!
//! The value lives in the card's password field only, never in a file, a log or a message. It leaves once, in the body of
//! `POST /v1/secrets`, and the field is emptied when it does. A card that names a known project goes straight to OK; replacing a
//! secret that exists takes one more press; a card whose project the catalog does not have is never saved elsewhere.

use std::time::{Duration, Instant};

use lucina_account::secrets::{self, Folder, Project, SaveSecret, Secret, SecretQueue, SecretRequest};
use lucina_account::ui::SignInView;
use lucina_app::{App, Face, ListState, Rect, TextEdit, Ui, keys};

use crate::server::{Job, Link, Msg};

/// New requests have no stream here: the queue is read this often while the app is open. The account's limit of 60 requests
/// a minute is shared by all apps, and a request can wait this long.
const REFRESH: Duration = Duration::from_secs(30);
/// What a value usually fits in, so typing it does not leave copies behind in freed memory.
const VALUE_ROOM: usize = 4096;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    Idle,
    Saving,
    /// It exists: one more press.
    Replace,
    /// The save failed: the same save can go again.
    Retry,
}

pub struct Card {
    /// The queue entry; `None` for "Add secret".
    pub request: Option<SecretRequest>,
    pub value: TextEdit,
    pub name: TextEdit,
    pub folder: TextEdit,
    pub project: Option<String>,
    pub environment: Option<String>,
    pub phase: Phase,
    pub held: Option<Box<SaveSecret>>,
    pub error: Option<String>,
    focus: bool,
}

impl Card {
    fn new(request: Option<SecretRequest>, place: Option<&Place>) -> Card {
        let mut value = TextEdit::new().placeholder("Paste it here");
        value.password = true;
        value.text.reserve(VALUE_ROOM);
        let named_place = request.as_ref().is_some_and(|r| !r.project.is_empty());
        let path = request.as_ref().map(|r| r.path.clone()).filter(|p| !p.is_empty());
        let path = path.or_else(|| place.filter(|_| !named_place).map(|p| p.path.clone())).unwrap_or_else(|| "/".to_owned());
        Card {
            environment: request.as_ref().map(|r| r.environment.clone()).filter(|e| !e.is_empty()).or_else(|| place.filter(|_| !named_place).map(|p| p.environment.clone())),
            name: TextEdit::with_text(request.as_ref().map_or("", |r| r.name.as_str())).placeholder("OPENAI_API_KEY"),
            folder: TextEdit::with_text(&path),
            project: place.filter(|_| !named_place).map(|p| p.project.clone()),
            request,
            value,
            phase: Phase::Idle,
            held: None,
            error: None,
            focus: true,
        }
    }

    /// Overwritten in place, then emptied: this field is the only copy on this side.
    pub fn wipe(&mut self) {
        let n = self.value.text.len();
        self.value.text.clear();
        self.value.text.extend(std::iter::repeat_n('\0', n));
        self.value.clear();
        self.held = None;
    }

    fn compact(&self) -> bool {
        self.request.as_ref().is_some_and(|r| !r.project.is_empty() && !r.environment.is_empty() && !r.name.is_empty())
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Place {
    pub project: String,
    pub environment: String,
    pub path: String,
}

pub struct NewProject {
    pub name: TextEdit,
    pub confirm: bool,
    pub saving: bool,
    pub error: Option<String>,
    focus: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Entry {
    Request(String),
    Folder(String),
    Secret(String),
    Note(String),
}

pub struct Secrets {
    link: Option<Link>,
    sign_in: Option<SignInView>,
    start_sign_in: Option<Box<dyn FnMut() -> SignInView>>,
    start_worker: Option<Box<dyn FnMut() -> Option<Link>>>,
    pub projects: Option<Vec<Project>>,
    pub catalog_error: Option<String>,
    pub project: Option<String>,
    pub environment: Option<String>,
    pub path: String,
    pub folder: Option<Folder>,
    pub folder_error: Option<String>,
    pub folder_loading: bool,
    pub queue: Option<SecretQueue>,
    pub entries: Vec<Entry>,
    pub project_list: ListState,
    pub list: ListState,
    pub card: Option<Card>,
    pub new_project: Option<NewProject>,
    /// Requests answered or put off here, which the queue may still list for a moment.
    closed: Vec<String>,
    /// The last line a save left: names and a fingerprint.
    pub receipt: Option<String>,
    pub note: Option<String>,
    last_place: Option<Place>,
    save_place: Option<Box<dyn FnMut(&Place)>>,
    last_queue: Option<Instant>,
    focus_list: bool,
    modal_was: bool,
    /// Passes left to hand the keyboard back: the first pass after a dialog still treats the page as out of reach.
    refocus: u8,
    sample: bool,
}

impl Secrets {
    pub fn new(link: Option<Link>) -> Secrets {
        Secrets {
            link,
            sign_in: None,
            start_sign_in: None,
            start_worker: None,
            projects: None,
            catalog_error: None,
            project: None,
            environment: None,
            path: "/".into(),
            folder: None,
            folder_error: None,
            folder_loading: false,
            queue: None,
            entries: Vec::new(),
            project_list: ListState::new(),
            list: ListState::new(),
            card: None,
            new_project: None,
            closed: Vec::new(),
            receipt: None,
            note: None,
            last_place: None,
            save_place: None,
            last_queue: None,
            focus_list: true,
            modal_was: false,
            refocus: 0,
            sample: false,
        }
    }

    pub fn sample(mut self) -> Secrets {
        self.sample = true;
        self
    }

    pub fn with_sign_in(mut self, start_sign_in: Box<dyn FnMut() -> SignInView>, start_worker: Box<dyn FnMut() -> Option<Link>>) -> Secrets {
        self.start_sign_in = Some(start_sign_in);
        self.start_worker = Some(start_worker);
        if self.link.is_none() {
            self.sign_in = self.start_sign_in.as_mut().map(|f| f());
        }
        self
    }

    /// The last place used (`lastPlace`): names only.
    pub fn with_place(mut self, last: Option<Place>, save: Box<dyn FnMut(&Place)>) -> Secrets {
        self.last_place = last;
        self.save_place = Some(save);
        self
    }

    fn send(&mut self, job: Job) {
        if let Some(l) = &mut self.link {
            l(job);
        }
    }

    pub fn start(&mut self) {
        self.send(Job::Catalog);
        self.refresh_queue();
    }

    fn refresh_queue(&mut self) {
        self.last_queue = Some(Instant::now());
        self.send(Job::Queue);
    }

    fn load_folder(&mut self) {
        let (Some(project), Some(environment)) = (self.project.clone(), self.environment.clone()) else { return };
        self.folder_loading = true;
        self.folder_error = None;
        let path = self.path.clone();
        self.send(Job::Names { project, environment, path });
    }

    /// `selected` and `env`: the first project and its first environment until the owner picks.
    fn settle_place(&mut self) {
        let Some(list) = &self.projects else { return };
        if self.project.as_ref().is_none_or(|id| !list.iter().any(|p| &p.id == id)) {
            let last = self.last_place.as_ref().filter(|l| list.iter().any(|p| p.id == l.project));
            match (last, list.first()) {
                (Some(l), _) => {
                    self.project = Some(l.project.clone());
                    self.environment = Some(l.environment.clone());
                    self.path = l.path.clone();
                }
                (None, Some(p)) => {
                    self.project = Some(p.id.clone());
                    self.environment = p.environments.first().cloned();
                    self.path = "/".into();
                }
                (None, None) => {
                    self.project = None;
                    self.environment = None;
                }
            }
            self.load_folder();
        }
    }

    pub fn pick_project(&mut self, id: String) {
        if self.project.as_deref() == Some(&id) {
            return;
        }
        let env = self.projects.as_ref().and_then(|l| l.iter().find(|p| p.id == id)).and_then(|p| p.environments.first().cloned());
        self.project = Some(id);
        self.environment = env;
        self.path = "/".into();
        self.folder = None;
        self.rebuild();
        self.load_folder();
    }

    pub fn pick_environment(&mut self, e: String) {
        self.environment = Some(e);
        self.path = "/".into();
        self.folder = None;
        self.rebuild();
        self.load_folder();
    }

    pub fn go_to(&mut self, path: String) {
        self.path = path;
        self.folder = None;
        self.list = ListState::new();
        self.rebuild();
        self.load_folder();
    }

    pub fn rebuild(&mut self) {
        let mut e = Vec::new();
        if let Some(q) = &self.queue {
            for r in &q.requests {
                e.push(Entry::Request(r.id.clone()));
            }
        }
        if let Some(f) = &self.folder {
            e.extend(f.folders.iter().map(|x| Entry::Folder(x.clone())));
            e.extend(f.secrets.iter().map(|s| Entry::Secret(s.key.clone())));
            if f.folders.is_empty() && f.secrets.is_empty() {
                e.push(Entry::Note("No secrets in this folder yet".into()));
            }
        } else if self.folder_loading {
            e.push(Entry::Note("Loading…".into()));
        }
        self.entries = e;
        if let Some(i) = self.list.selected {
            if i >= self.entries.len() {
                self.list.selected = self.entries.len().checked_sub(1);
            }
        }
        if self.list.selected.is_none() && !self.entries.is_empty() {
            self.list.select(0);
        }
    }

    /// The queue moved: the active request opens its card, unless a card or form is being worked on.
    fn sync_card(&mut self) {
        if self.new_project.is_some() {
            return;
        }
        if let Some(c) = &self.card {
            if c.request.is_none() || c.phase != Phase::Idle || !c.value.text.is_empty() {
                return;
            }
        }
        let active = self.queue.as_ref().and_then(|q| q.active.as_deref().and_then(|a| q.find(a))).filter(|r| r.status == "active" && !self.closed.contains(&r.id)).cloned();
        match (active, &self.card) {
            (Some(r), Some(c)) if c.request.as_ref().is_some_and(|x| x.id == r.id) => {}
            (Some(r), _) => self.card = Some(Card::new(Some(r), None)),
            (None, Some(_)) => self.card = None,
            (None, None) => {}
        }
    }

    pub fn open_add(&mut self) {
        self.receipt = None;
        let place = match (&self.project, &self.environment) {
            (Some(p), Some(e)) => Some(Place { project: p.clone(), environment: e.clone(), path: self.path.clone() }),
            _ => self.last_place.clone(),
        };
        self.card = Some(Card::new(None, place.as_ref()));
    }

    pub fn open_request(&mut self, id: &str) {
        let r = self.queue.as_ref().and_then(|q| q.find(id)).cloned();
        if let Some(r) = r.filter(|r| r.status == "active") {
            self.closed.retain(|x| x != &r.id);
            self.card = Some(Card::new(Some(r), None));
        }
    }

    fn close_card(&mut self) {
        if let Some(mut c) = self.card.take() {
            c.wipe();
            if let Some(r) = c.request {
                // Esc hides it until opened again; the request stays with the server.
                self.closed.push(r.id);
            }
        }
    }

    /// The card's project: the named one must be in the catalog, or nothing can be saved.
    fn card_project(&mut self) {
        let Some(projects) = &self.projects else { return };
        let Some(c) = &mut self.card else { return };
        if c.project.is_some() {
            return;
        }
        match c.request.as_ref().map(|r| r.project.as_str()).filter(|p| !p.is_empty()) {
            Some(named) => c.project = secrets::find_project(named, projects).map(|p| p.id.clone()),
            None => c.project = projects.first().map(|p| p.id.clone()),
        }
        if c.environment.is_none() {
            c.environment = c.project.as_ref().and_then(|id| projects.iter().find(|p| &p.id == id)).and_then(|p| p.default_environment()).map(str::to_owned);
        }
    }

    /// Whether OK can go: `ready`.
    pub fn card_ready(&self) -> bool {
        let Some(c) = &self.card else { return false };
        let has_value = c.held.is_some() || {
            let n = c.value.text.trim_end_matches(['\r', '\n']).len();
            n > 0 && n <= secrets::MAX_VALUE
        };
        has_value
            && secrets::valid_name(c.name.text.trim())
            && c.project.is_some()
            && c.environment.is_some()
            && secrets::normalize_folder(&c.folder.text).is_some()
            && !self.env_missing()
            && !self.project_missing()
    }

    fn project_missing(&self) -> bool {
        self.card.as_ref().is_some_and(|c| c.project.is_none() && c.request.as_ref().is_some_and(|r| !r.project.is_empty())) && self.projects.is_some()
    }

    fn env_missing(&self) -> bool {
        let Some(c) = &self.card else { return false };
        let project = c.project.as_ref().and_then(|id| self.projects.as_ref()?.iter().find(|p| &p.id == id));
        match (&c.environment, project) {
            (Some(e), Some(p)) => !p.environments.contains(e),
            _ => false,
        }
    }

    /// `save`: from here on the only copy is the one going out.
    pub fn save_card(&mut self, ui: Option<&mut Ui>) {
        if !self.card_ready() {
            return;
        }
        let Some(c) = &mut self.card else { return };
        if c.phase == Phase::Saving {
            return;
        }
        if let Some(ui) = ui {
            ui.forget_clipboard_if(c.value.text.trim_end_matches(['\r', '\n']));
        }
        let (Some(project), Some(environment), Some(path)) = (c.project.clone(), c.environment.clone(), secrets::normalize_folder(&c.folder.text)) else { return };
        let value = Secret::new(std::mem::take(&mut c.value.text));
        c.value.clear();
        c.phase = Phase::Saving;
        c.error = None;
        let save = SaveSecret { name: c.name.text.trim().to_owned(), project: project.clone(), environment: environment.clone(), path: path.clone(), overwrite: false, value, request: c.request.as_ref().map(|r| r.id.clone()) };
        self.remember(Place { project, environment, path });
        self.send(Job::Save(Box::new(save)));
    }

    /// "Replace it", or "Try again": the same save, whose value is still only there.
    pub fn resend(&mut self, overwrite: bool) {
        let Some(c) = &mut self.card else { return };
        let Some(mut save) = c.held.take() else { return };
        save.overwrite = save.overwrite || overwrite;
        c.phase = Phase::Saving;
        c.error = None;
        self.send(Job::Save(save));
    }

    fn remember(&mut self, p: Place) {
        if let Some(s) = &mut self.save_place {
            s(&p);
        }
        self.last_place = Some(p);
    }

    /// `notNow`.
    pub fn not_now(&mut self) {
        let Some(mut c) = self.card.take() else { return };
        c.wipe();
        if let Some(r) = c.request {
            self.send(Job::Dismiss(r.id.clone()));
            self.closed.push(r.id);
        }
    }

    pub fn open_new_project(&mut self, name: &str) {
        self.new_project = Some(NewProject { name: TextEdit::with_text(name), confirm: false, saving: false, error: None, focus: true });
    }

    fn create_project_for_card(&mut self) {
        let Some(name) = self.card.as_ref().and_then(|c| c.request.as_ref()).map(|r| r.project.clone()) else { return };
        if let Some(c) = &mut self.card {
            c.error = None;
        }
        self.send(Job::CreateProject { name, for_card: true });
    }

    fn cycle_card_project(&mut self) {
        let Some(list) = &self.projects else { return };
        let Some(c) = &mut self.card else { return };
        if list.is_empty() {
            return;
        }
        let at = c.project.as_ref().and_then(|id| list.iter().position(|p| &p.id == id));
        let next = &list[at.map_or(0, |i| (i + 1) % list.len())];
        c.project = Some(next.id.clone());
        c.environment = next.default_environment().map(str::to_owned);
    }

    fn cycle_card_env(&mut self) {
        let Some(list) = &self.projects else { return };
        let Some(c) = &mut self.card else { return };
        let Some(p) = c.project.as_ref().and_then(|id| list.iter().find(|p| &p.id == id)) else { return };
        if p.environments.is_empty() {
            return;
        }
        let at = c.environment.as_ref().and_then(|e| p.environments.iter().position(|x| x == e));
        c.environment = Some(p.environments[at.map_or(0, |i| (i + 1) % p.environments.len())].clone());
    }

    fn view_card(&mut self, ui: &mut Ui) {
        self.card_project();
        let t = ui.theme();
        let queued = self.queue.as_ref().map_or(0, |q| q.queued);
        let project_missing = self.project_missing();
        let env_missing = self.env_missing();
        let ready = self.card_ready();
        let projects: Vec<Project> = self.projects.clone().unwrap_or_default();
        let compact = self.card.as_ref().is_some_and(Card::compact);
        let mut do_save = false;
        let mut do_replace = false;
        let mut do_retry = false;
        let mut do_keep = false;
        let mut do_not_now = false;
        let mut do_cancel = false;
        let mut do_create = false;
        let (mut next_project, mut next_env) = (false, false);
        let Some(c) = &mut self.card else { return };
        let h = if compact { 260.0 } else { 430.0 };
        ui.modal(640.0, h, |ui| {
            let (project_name, env_name) = (c.project.as_ref().and_then(|id| projects.iter().find(|p| &p.id == id)).map_or("?", |p| p.name.as_str()), c.environment.as_deref().unwrap_or("?"));
            let title = match c.request.as_ref().map(|r| r.name.as_str()) {
                Some(n) if !n.is_empty() => format!("Enter the secret for {n}"),
                _ => "Enter a secret".to_owned(),
            };
            let head = ui.cut_top(t.px(26.0));
            ui.text_in(head, Face::Bold, t.pt(16.0), t.accent, &title);
            if let Some(r) = c.request.as_ref().filter(|r| !r.who.is_empty() && !r.name.is_empty()) {
                ui.wrapped(Face::Sans, t.pt(13.0), t.item, &format!("{}: {}", r.who, r.reason));
            }
            if !compact {
                ui.dim("Name: letters, digits, _ . @ -");
                ui.row(32.0, |ui| {
                    ui.text_field("secret-name", &mut c.name);
                });
                ui.space(4.0);
            }
            ui.dim(if c.held.is_some() { "The value is still here, hidden. Type to replace it." } else { "Value" });
            if std::mem::take(&mut c.focus) {
                ui.focus(if compact || !c.name.text.is_empty() { "secret-value" } else { "secret-name" });
            }
            let res = ui.text_field("secret-value", &mut c.value);
            if res.changed {
                // A new value drops what was waiting, and wipes it.
                c.held = None;
                if matches!(c.phase, Phase::Replace | Phase::Retry) {
                    c.phase = Phase::Idle;
                }
            }
            let place = format!("{project_name} / {env_name} / {}", secrets::normalize_folder(&c.folder.text).unwrap_or_else(|| c.folder.text.clone()));
            if compact {
                ui.space(2.0);
                ui.dim(&place);
            } else {
                ui.space(4.0);
                ui.row(32.0, |ui| {
                    if ui.button_id("card-project", &format!("Project: {project_name}")) {
                        next_project = true;
                    }
                    if ui.button_id("card-env", &format!("Environment: {env_name}")) {
                        next_env = true;
                    }
                });
                ui.dim("Folder");
                ui.row(32.0, |ui| {
                    ui.text_field("card-folder", &mut c.folder);
                });
            }
            if project_missing {
                let named = c.request.as_ref().map_or("", |r| r.project.as_str());
                ui.wrapped(Face::Sans, t.pt(13.0), t.danger, &format!("{named} isn't one of your Infisical projects yet."));
                if ui.button("Create project") {
                    do_create = true;
                }
            }
            if env_missing {
                ui.wrapped(Face::Sans, t.pt(13.0), t.danger, &format!("This project has no environment {env_name}."));
            }
            if secrets::normalize_folder(&c.folder.text).is_none() {
                ui.wrapped(Face::Sans, t.pt(13.0), t.danger, "That folder has a character it cannot have.");
            }
            if let Some(e) = &c.error {
                ui.wrapped(Face::Sans, t.pt(13.0), t.danger, e);
            }
            if c.phase == Phase::Replace {
                ui.wrapped(Face::Sans, t.pt(13.0), t.accent, &format!("This replaces the current value of {}.", c.name.text.trim()));
            }
            ui.space(4.0);
            ui.row(34.0, |ui| match c.phase {
                Phase::Saving => ui.dim("Saving…"),
                Phase::Replace => {
                    if ui.primary_button("Replace") {
                        do_replace = true;
                    }
                    if ui.button("Keep the old one") {
                        do_keep = true;
                    }
                }
                Phase::Retry => {
                    if ui.primary_button("Try again") {
                        do_retry = true;
                    }
                    if ui.button("Forget it") {
                        do_keep = true;
                    }
                }
                Phase::Idle => {
                    if (ui.primary_button("OK") || res.submitted) && ready {
                        do_save = true;
                    }
                    if c.request.is_some() {
                        if ui.button("Not now") {
                            do_not_now = true;
                        }
                    } else if ui.button("Cancel") {
                        do_cancel = true;
                    }
                }
            });
            if queued > 0 && c.request.is_some() {
                ui.dim(&format!("{queued} more after this one."));
            }
            if c.phase != Phase::Saving && ui.key(keys::KEY_ESC) {
                do_cancel = true;
            }
        });
        if next_project {
            self.cycle_card_project();
        }
        if next_env {
            self.cycle_card_env();
        }
        if do_create {
            self.create_project_for_card();
        }
        if do_save {
            self.save_card(Some(ui));
        }
        if do_replace {
            self.resend(true);
        }
        if do_retry {
            self.resend(false);
        }
        if do_keep {
            if let Some(c) = &mut self.card {
                // Dropping the save wipes the value it held.
                c.held = None;
                c.phase = Phase::Idle;
                c.error = None;
            }
        }
        if do_not_now {
            self.not_now();
        }
        if do_cancel {
            self.close_card();
        }
    }

    fn view_new_project(&mut self, ui: &mut Ui) {
        let t = ui.theme();
        let Some(n) = &mut self.new_project else { return };
        let mut close = false;
        let mut create = None;
        ui.modal(560.0, 250.0, |ui| {
            let head = ui.cut_top(t.px(26.0));
            ui.text_in(head, Face::Bold, t.pt(16.0), t.accent, "New Infisical project");
            if n.confirm {
                ui.label(&format!("Create the Infisical project {}?", n.name.text.trim()));
            } else {
                ui.dim("Project name");
                if std::mem::take(&mut n.focus) {
                    ui.focus("project-name");
                }
                ui.row(32.0, |ui| {
                    ui.text_field("project-name", &mut n.name);
                });
            }
            ui.wrapped(Face::Sans, t.pt(13.0), t.dim, "Agents and services get no access to a new project until you say so.");
            if let Some(e) = &n.error {
                ui.wrapped(Face::Sans, t.pt(13.0), t.danger, e);
            }
            ui.space(6.0);
            let name = n.name.text.trim().to_owned();
            ui.row(34.0, |ui| {
                if n.saving {
                    ui.dim("Creating…");
                } else if n.confirm {
                    if ui.primary_button("Create project") {
                        create = Some(name.clone());
                    }
                    if ui.button("Back") {
                        n.confirm = false;
                    }
                } else {
                    if ui.primary_button("Review") && !name.is_empty() {
                        n.confirm = true;
                    }
                    if ui.button("Cancel") {
                        close = true;
                    }
                }
            });
            if !n.saving && ui.key(keys::KEY_ESC) {
                close = true;
            }
        });
        if close {
            self.new_project = None;
        }
        if let Some(name) = create {
            if let Some(n) = &mut self.new_project {
                n.saving = true;
                n.error = None;
            }
            self.send(Job::CreateProject { name, for_card: false });
        }
    }

    fn view_home(&mut self, ui: &mut Ui) {
        let t = ui.theme();
        let intro = ui.cut_top(t.px(28.0));
        ui.text_in(Rect::new(intro.x + t.px(14.0), intro.y, intro.w - t.px(20.0), intro.h), Face::Sans, t.pt(13.0), t.dim, "Browse names and locations; secret values stay private.");
        let left = ui.cut_left(t.px(280.0));
        ui.fill(left, t.bar);
        let mut chosen_env = None;
        let mut chosen_project = None;
        let mut retry = false;
        ui.within(left.inset(t.px(8.0)), |ui| {
            ui.label("Project");
            let Some(list) = &self.projects else {
                if let Some(e) = &self.catalog_error {
                    ui.wrapped(Face::Sans, t.pt(13.0), t.danger, "Couldn't load your projects.");
                    ui.wrapped(Face::Sans, t.pt(12.0), t.dim, e);
                    if ui.button("Try again") {
                        retry = true;
                    }
                } else {
                    ui.dim("Loading projects…");
                }
                return;
            };
            let envs = self.project.as_ref().and_then(|id| list.iter().find(|p| &p.id == id)).map_or(&[][..], |p| p.environments.as_slice());
            let env_h = t.px(34.0) * envs.len().div_ceil(2).max(1) as i32 + t.px(44.0);
            let list_r = Rect::new(ui.area().x, ui.area().y, ui.area().w, (ui.area().h - env_h).max(t.px(60.0)));
            let current = self.project.clone();
            let res = ui.list("projects", &mut self.project_list, list_r, list.len(), 32.0, |ui, i, selected| {
                let row = ui.area();
                if selected || current.as_deref() == Some(list[i].id.as_str()) {
                    ui.fill_round(Rect::new(row.x, row.y + 1, row.w, row.h - 2), t.px(5.0), t.selection);
                }
                ui.text_in(Rect::new(row.x + t.px(8.0), row.y, row.w - t.px(16.0), row.h), Face::Sans, t.pt(14.0), t.text, &list[i].name);
            });
            if let Some(i) = res.activated {
                chosen_project = Some(list[i].id.clone());
            }
            let below = Rect::new(list_r.x, list_r.bottom() + t.px(4.0), list_r.w, env_h);
            ui.within(below, |ui| {
                ui.label("Environment");
                for (k, pair) in envs.chunks(2).enumerate() {
                    ui.row(32.0, |ui| {
                        for (j, e) in pair.iter().enumerate() {
                            let label = if self.environment.as_ref() == Some(e) { format!("› {e}") } else { e.clone() };
                            if ui.button_id(("env", k * 2 + j), &label) {
                                chosen_env = Some(e.clone());
                            }
                        }
                    });
                }
            });
        });
        if let Some(id) = chosen_project {
            self.pick_project(id);
        }
        if let Some(e) = chosen_env {
            self.pick_environment(e);
        }
        if retry {
            self.catalog_error = None;
            self.send(Job::Catalog);
        }

        let pane = ui.area();
        let pane = Rect::new(pane.x + t.px(12.0), pane.y + t.px(4.0), pane.w - t.px(20.0), pane.h - t.px(8.0));
        let mut up = false;
        let mut add = false;
        let mut new_project = false;
        let mut opened = None;
        let mut try_folder = false;
        ui.within(pane, |ui| {
            let bar = ui.cut_top(t.px(36.0));
            let (b1, b2, b3) = (t.px(138.0), t.px(138.0), t.px(70.0));
            let path_w = (bar.w - b1 - b2 - b3 - t.px(24.0)).max(t.px(40.0));
            ui.text_in(Rect::new(bar.x, bar.y, path_w, bar.h), Face::Mono, t.pt(14.0), t.text, &self.path);
            if self.path != "/" && ui.button_at(Rect::new(bar.right() - b1 - b2 - b3 - t.px(12.0), bar.y + t.px(2.0), b3, t.px(30.0)), "up", "Up") {
                up = true;
            }
            if ui.button_at(Rect::new(bar.right() - b1 - b2 - t.px(6.0), bar.y + t.px(2.0), b2, t.px(30.0)), "new-project", "New project…") {
                new_project = true;
            }
            if ui.button_at(Rect::new(bar.right() - b1, bar.y + t.px(2.0), b1, t.px(30.0)), "add", "Add secret") {
                add = true;
            }
            if let Some(r) = &self.receipt {
                ui.wrapped(Face::Sans, t.pt(14.0), t.ok, r);
            }
            if let Some(n) = &self.note {
                ui.wrapped(Face::Sans, t.pt(13.0), t.danger, n);
            }
            if let Some(e) = &self.folder_error {
                ui.wrapped(Face::Sans, t.pt(13.0), t.danger, e);
                if ui.button("Try again") {
                    try_folder = true;
                }
            }
            let r = ui.area();
            if self.focus_list && !self.entries.is_empty() {
                self.focus_list = false;
                ui.focus("entries");
            }
            let entries = &self.entries;
            let queue = &self.queue;
            let folder = &self.folder;
            let res = ui.list("entries", &mut self.list, r, entries.len(), 44.0, |ui, i, selected| {
                let row = ui.area();
                if selected {
                    ui.fill_round(Rect::new(row.x, row.y + 1, row.w, row.h - 2), t.px(6.0), t.selection);
                }
                let x = row.x + t.px(8.0);
                let w = row.w - t.px(16.0);
                let top = Rect::new(x, row.y + t.px(3.0), w, t.px(22.0));
                let low = Rect::new(x, row.y + t.px(23.0), w, t.px(18.0));
                match &entries[i] {
                    Entry::Request(id) => {
                        let Some(q) = queue.as_ref().and_then(|q| q.find(id)) else { return };
                        let (text, colour) = request_line(q);
                        ui.text_in(top, Face::Bold, t.pt(14.0), if colour { t.accent } else if selected { t.text } else { t.dim }, &text);
                        ui.text_in(low, Face::Sans, t.pt(12.0), if selected { t.item } else { t.dim }, &format!("{} · {}", q.who, q.reason));
                    }
                    Entry::Folder(name) => {
                        ui.text_in(Rect::new(x, row.y, w, row.h), Face::Sans, t.pt(14.0), t.item, &format!("{name}/"));
                    }
                    Entry::Secret(key) => {
                        ui.text_in(top, Face::Mono, t.pt(14.0), t.text, key);
                        let when = folder.as_ref().and_then(|f| f.secrets.iter().find(|s| &s.key == key)).and_then(|s| s.updated_at.as_deref()).unwrap_or("");
                        ui.text_in(low, Face::Sans, t.pt(12.0), if selected { t.item } else { t.dim }, when.get(..16).unwrap_or(when));
                    }
                    Entry::Note(s) => {
                        ui.text_in(Rect::new(x, row.y, w, row.h), Face::Sans, t.pt(13.0), t.dim, s);
                    }
                }
            });
            if let Some(i) = res.activated {
                opened = Some(entries[i].clone());
            }
        });
        match opened {
            Some(Entry::Folder(f)) => {
                let next = secrets::join_folder(&self.path, &f);
                self.go_to(next);
            }
            Some(Entry::Request(id)) => self.open_request(&id),
            _ => {}
        }
        if up {
            self.go_to(secrets::parent_folder(&self.path));
        }
        if add {
            self.open_add();
        }
        if new_project {
            self.open_new_project("");
        }
        if try_folder {
            self.load_folder();
        }
        if self.card.is_none() && self.new_project.is_none() {
            if ui.key(keys::KEY_A) {
                self.open_add();
            } else if ui.key(keys::KEY_N) {
                self.open_new_project("");
            } else if ui.key(keys::KEY_BACKSPACE) && self.path != "/" {
                self.go_to(secrets::parent_folder(&self.path));
            } else if ui.key(keys::KEY_R) {
                self.start();
                self.load_folder();
            }
        }
    }
}

/// `SecretCard`'s closed states, as one line in the list.
fn request_line(r: &SecretRequest) -> (String, bool) {
    let name = if r.name.is_empty() { "Secret" } else { r.name.as_str() };
    match r.status.as_str() {
        "active" => (format!("Waiting: enter the secret for {name}"), true),
        "queued" => (format!("Next: {name}"), false),
        "saved" => {
            let verb = if r.action.as_deref() == Some("updated") { "replaced" } else { "saved" };
            (format!("{name} {verb}. Fingerprint {}.", r.fingerprint.as_deref().unwrap_or("?")), false)
        }
        "dismissed" => (format!("{name}: not saved."), false),
        _ => (format!("{name}: no longer asked."), false),
    }
}

impl App for Secrets {
    type Msg = Msg;

    fn message(&mut self, m: Msg) {
        match m {
            Msg::Changed => {}
            Msg::SignedIn => {
                self.sign_in = None;
                if self.link.is_none() {
                    self.link = self.start_worker.as_mut().and_then(|f| f());
                }
                self.start();
            }
            Msg::SignedOut => {
                self.close_card();
                self.new_project = None;
                self.link = None;
                crate::account::rejected();
                self.sign_in = self.start_sign_in.as_mut().map(|f| f());
            }
            Msg::Catalog(Ok(list)) => {
                self.catalog_error = None;
                self.projects = Some(list);
                self.settle_place();
                let at = self.projects.as_ref().and_then(|l| l.iter().position(|p| Some(&p.id) == self.project.as_ref()));
                if let Some(i) = at {
                    self.project_list.select(i);
                }
            }
            Msg::Catalog(Err(e)) => self.catalog_error = Some(e),
            Msg::Names { place, result } => {
                // An answer for a place already left says nothing about this one.
                if self.project.as_deref() == Some(&place.0) && self.environment.as_deref() == Some(&place.1) && self.path == place.2 {
                    self.folder_loading = false;
                    match result {
                        Ok(f) => {
                            self.folder = Some(f);
                            self.folder_error = None;
                        }
                        Err(_) => self.folder_error = Some("Couldn't load your projects.".to_owned()),
                    }
                    self.rebuild();
                }
            }
            Msg::Queue(Ok(q)) => {
                self.queue = Some(q);
                self.rebuild();
                self.sync_card();
            }
            // A drop of the connection says nothing about the queue: what is listed stays.
            Msg::Queue(Err(_)) => {}
            Msg::Dismissed(Ok(())) => self.refresh_queue(),
            Msg::Dismissed(Err(e)) => {
                self.note = Some(e);
                // It stays with the server, so its card can come back.
                self.closed.clear();
                self.refresh_queue();
            }
            Msg::Saved(r) => {
                let verb = if r.action == "updated" { "replaced" } else { "saved" };
                self.receipt = Some(format!("{} {verb}. Fingerprint {}.", r.key, r.fingerprint));
                self.note = None;
                if let Some(mut c) = self.card.take() {
                    c.wipe();
                    if let Some(req) = c.request {
                        self.closed.push(req.id);
                    }
                }
                self.refresh_queue();
                self.load_folder();
            }
            Msg::NotSaved { save, exists, message } => {
                if let Some(c) = &mut self.card {
                    c.held = Some(save);
                    c.value.clear();
                    if exists {
                        c.phase = Phase::Replace;
                    } else {
                        c.phase = Phase::Retry;
                        c.error = Some(message);
                    }
                }
            }
            Msg::Created { result, for_card } => match result {
                Ok(p) => {
                    let id = p.id.clone();
                    let envs = p.environments.join(", ");
                    let name = p.name.clone();
                    if let Some(list) = &mut self.projects {
                        list.retain(|x| x.id != p.id);
                        list.push(p);
                        list.sort_by(|a, b| a.name.cmp(&b.name));
                    }
                    self.receipt = Some(format!("Project {name} created ({envs})."));
                    if for_card {
                        if let Some(c) = &mut self.card {
                            c.project = Some(id);
                        }
                    } else {
                        self.new_project = None;
                        self.pick_project(id);
                    }
                }
                Err(e) => {
                    if for_card {
                        if let Some(c) = &mut self.card {
                            c.error = Some(e);
                        }
                    } else if let Some(n) = &mut self.new_project {
                        n.saving = false;
                        n.confirm = false;
                        n.error = Some(e);
                    }
                }
            },
        }
    }

    fn on_quit(&mut self) {
        if let Some(c) = &mut self.card {
            c.wipe();
        }
    }

    fn view(&mut self, ui: &mut Ui) {
        ui.header(if self.sample { "Secrets (sample data)" } else { "Secrets" });
        // Nothing of the Secrets app reaches Lucina: she reads one node, and the owner works it as before.
        ui.private("Secrets", |ui| {
            if let Some(s) = &mut self.sign_in {
                s.view(ui, "Saving a secret takes this laptop's account.");
                return;
            }
            if self.last_queue.is_none_or(|t| t.elapsed() >= REFRESH) {
                self.refresh_queue();
            }
            ui.wake_after(REFRESH);
            // Back from a dialog, the keyboard belongs to the list again.
            let modal = self.card.is_some() || self.new_project.is_some();
            if std::mem::replace(&mut self.modal_was, modal) && !modal {
                self.refocus = 2;
            }
            if self.refocus > 0 {
                self.refocus -= 1;
                self.focus_list = true;
            }
            self.view_home(ui);
            if self.card.is_some() {
                self.view_card(ui);
            } else if self.new_project.is_some() {
                self.view_new_project(ui);
            }
        });
    }
}
