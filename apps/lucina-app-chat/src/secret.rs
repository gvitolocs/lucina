//! The paste-and-OK card (`web/src/components/SecretCard.tsx`) and `/secret`'s sheet: one form above the composer.
//!
//! The value lives in this form's password field only, never in the chat, a draft or a log. It leaves once, in the body of
//! `POST /v1/secrets`, and the field is emptied when it does. A card that names a known project goes straight to OK; replacing
//! a secret that exists takes one more tap; a card whose project the catalog does not have can never be saved elsewhere.
use lucina_account::engine::{Engine, SaveResult};
use lucina_account::secrets::{self, Project, SaveSecret, Secret, SecretQueue, SecretRequest};
use lucina_app::{Face, Rect, TextEdit, Theme, Ui};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    Idle,
    Saving,
    Replace,
}

struct Open {
    /// The queue entry; `None` for `/secret`.
    request: Option<SecretRequest>,
    value: TextEdit,
    name: TextEdit,
    folder: TextEdit,
    project: Option<usize>,
    environment: Option<String>,
    phase: Phase,
    /// The value field takes the keys when the card appears.
    focus: bool,
    /// Waiting for that one more tap: the save as it was, value and all.
    replacing: Option<SaveSecret>,
    error: Option<String>,
    /// Hidden by "not now" on a request: it stays closed until the queue moves on.
    manual: bool,
}

pub struct SecretPanel {
    open: Option<Open>,
    /// Requests answered or put off here, which the queue may still list for a moment.
    closed: Vec<String>,
}

impl Open {
    fn new(request: Option<SecretRequest>, name: Option<&str>) -> Open {
        let mut value = TextEdit::new().placeholder("Paste the value here");
        value.password = true;
        let default_name = request
            .as_ref()
            .map(|r| r.name.clone())
            .or(name.map(str::to_owned))
            .unwrap_or_default();
        let path = request
            .as_ref()
            .map(|r| r.path.clone())
            .filter(|p| !p.is_empty())
            .unwrap_or_else(|| "/".to_owned());
        Open {
            environment: request
                .as_ref()
                .map(|r| r.environment.clone())
                .filter(|e| !e.is_empty()),
            manual: request.is_none(),
            request,
            value,
            name: TextEdit::with_text(&default_name).placeholder("Name"),
            folder: TextEdit::with_text(&path),
            project: None,
            phase: Phase::Idle,
            focus: true,
            replacing: None,
            error: None,
        }
    }

    fn wipe(&mut self) {
        // Overwritten in place, then emptied: this field is the only copy on this side.
        let n = self.value.text.len();
        self.value.text.clear();
        self.value.text.extend(std::iter::repeat_n('\0', n));
        self.value.clear();
    }
}

impl SecretPanel {
    pub fn new() -> SecretPanel {
        SecretPanel {
            open: None,
            closed: Vec::new(),
        }
    }

    pub fn needs_catalog(&self) -> bool {
        self.open.is_some()
    }

    pub fn height(&self, t: &Theme) -> i32 {
        match &self.open {
            None => 0,
            Some(o)
                if o.request.as_ref().is_some_and(|r| !r.project.is_empty())
                    && o.request.as_ref().is_some_and(|r| !r.name.is_empty()) =>
            {
                t.px(150.0)
            }
            Some(_) => t.px(226.0),
        }
    }

    /// The queue moved: the active request opens its card, unless one is being worked on.
    pub fn sync(&mut self, queue: Option<&SecretQueue>) {
        if let Some(open) = &self.open {
            if open.manual || open.phase != Phase::Idle || !open.value.text.is_empty() {
                return;
            }
        }
        let active = queue
            .and_then(|q| q.active.as_deref().and_then(|a| q.find(a)))
            .filter(|r| r.status == "active" && !self.closed.contains(&r.id));
        match (active, &self.open) {
            (Some(r), Some(o)) if o.request.as_ref().is_some_and(|x| x.id == r.id) => {}
            (Some(r), _) => self.open = Some(Open::new(Some(r.clone()), None)),
            (None, Some(_)) => self.open = None,
            (None, None) => {}
        }
    }

    /// `/secret NAME VALUE`, or a button that names one. The value is taken and never kept in the composer.
    pub fn open_manual(&mut self, name: Option<&str>, value: Option<&str>) {
        let mut o = Open::new(None, name);
        if let Some(v) = value {
            o.value.set(v);
        }
        self.open = Some(o);
    }

    /// The guard's "Save it as a secret".
    pub fn open_with_value(&mut self, value: &str) {
        let name = secrets::suggest_name(value);
        self.open_manual((!name.is_empty()).then_some(name), Some(value));
    }

    pub fn wipe(&mut self) {
        if let Some(o) = &mut self.open {
            o.wipe();
        }
    }

    fn take_finished(&mut self, engine: &Engine) {
        let Some((_, result)) = engine.state().save_result.take() else {
            return;
        };
        match result {
            SaveResult::Saved(_) => {
                if let Some(mut o) = self.open.take() {
                    o.wipe();
                    if let Some(r) = o.request {
                        self.closed.push(r.id);
                    }
                }
            }
            SaveResult::Exists(save) => {
                if let Some(o) = &mut self.open {
                    o.phase = Phase::Replace;
                    o.replacing = Some(save);
                }
            }
            SaveResult::Failed(e) => {
                if let Some(o) = &mut self.open {
                    o.phase = Phase::Idle;
                    o.error = Some(e);
                }
            }
        }
    }

    fn save(&mut self, projects: &[Project], engine: &Engine) {
        let Some(o) = &mut self.open else { return };
        let Some(project) = o.project.and_then(|i| projects.get(i)) else {
            return;
        };
        let Some(path) = secrets::normalize_folder(&o.folder.text) else {
            return;
        };
        let Some(environment) = o.environment.clone() else {
            return;
        };
        // `take`: from here on the only copy is the one going out.
        let value = Secret::new(std::mem::take(&mut o.value.text));
        o.value.clear();
        o.phase = Phase::Saving;
        o.error = None;
        engine.save_secret(SaveSecret {
            name: o.name.text.trim().to_owned(),
            project: project.id.clone(),
            environment,
            path,
            overwrite: false,
            value,
            request: o.request.as_ref().map(|r| r.id.clone()),
        });
    }

    pub fn view(&mut self, ui: &mut Ui, engine: &Engine) {
        self.take_finished(engine);
        let t = ui.theme();
        let catalog = engine.state().catalog.clone();
        let projects: &[Project] = catalog.as_deref().unwrap_or(&[]);
        let mut do_save = false;
        let mut do_replace = false;
        let mut not_now = false;
        let mut close = false;
        let Some(o) = &mut self.open else { return };
        // The card's project, or the first one; and its environment.
        if o.project.is_none() && !projects.is_empty() {
            match o
                .request
                .as_ref()
                .map(|r| r.project.as_str())
                .filter(|p| !p.is_empty())
            {
                Some(named) => {
                    o.project = secrets::find_project(named, projects)
                        .and_then(|f| projects.iter().position(|p| p.id == f.id))
                }
                None => o.project = Some(0),
            }
            if o.environment.is_none() {
                o.environment = o
                    .project
                    .and_then(|i| projects[i].default_environment())
                    .map(str::to_owned);
            }
        }
        let area = ui.area();
        ui.fill(area, t.bar);
        let inner = Rect::new(
            area.x + t.px(10.0),
            area.y + t.px(6.0),
            area.w - t.px(20.0),
            area.h - t.px(10.0),
        );
        let missing = o.request.as_ref().is_some_and(|r| !r.project.is_empty())
            && catalog.is_some()
            && o.project.is_none();
        let env_missing = o.environment.as_ref().is_some_and(|e| {
            o.project
                .and_then(|i| projects.get(i))
                .is_some_and(|p| !p.environments.contains(e))
        });
        let folder_ok = secrets::normalize_folder(&o.folder.text).is_some();
        let compact = o
            .request
            .as_ref()
            .is_some_and(|r| !r.project.is_empty() && !r.name.is_empty());
        ui.within(inner, |ui| {
            let title = match o.request.as_ref().map(|r| r.name.as_str()) {
                Some(n) if !n.is_empty() => format!("Enter the secret for {n}"),
                _ => "Add a secret".to_owned(),
            };
            let head = ui.cut_top(t.px(22.0));
            ui.text_in(head, Face::Bold, t.pt(14.0), t.accent, &title);
            if let Some(r) = &o.request {
                if !r.who.is_empty() && !r.name.is_empty() {
                    ui.dim(&format!("{} asks: {}", r.who, r.reason));
                }
            }
            if !compact {
                ui.row(30.0, |ui| {
                    ui.text_field("secret-name", &mut o.name);
                });
            }
            if std::mem::take(&mut o.focus) {
                ui.focus("secret-value");
            }
            // Enter in the value saves too: Lucina's injected keys do not reach it.
            let submitted = ui.owner_only(|ui| ui.text_field("secret-value", &mut o.value).submitted);
            let place = {
                let name = o.project.and_then(|i| projects.get(i)).map_or("?", |p| p.name.as_str());
                format!("{name} / {} / {}", o.environment.as_deref().unwrap_or("?"), o.folder.text.trim())
            };
            if compact {
                ui.dim(&format!("Goes to {place}"));
            } else {
                ui.row(30.0, |ui| {
                    if ui.button_id("secret-project", &format!("Project: {}", o.project.and_then(|i| projects.get(i)).map_or("?", |p| p.name.as_str()))) && !projects.is_empty() {
                        let next = o.project.map_or(0, |i| (i + 1) % projects.len());
                        o.project = Some(next);
                        o.environment = projects[next].default_environment().map(str::to_owned);
                    }
                    if ui.button_id("secret-env", &format!("Env: {}", o.environment.as_deref().unwrap_or("?"))) {
                        if let Some(p) = o.project.and_then(|i| projects.get(i)) {
                            if !p.environments.is_empty() {
                                let at = o.environment.as_ref().and_then(|e| p.environments.iter().position(|x| x == e));
                                let next = at.map_or(0, |i| (i + 1) % p.environments.len());
                                o.environment = Some(p.environments[next].clone());
                            }
                        }
                    }
                    ui.text_field("secret-folder", &mut o.folder);
                });
            }
            if missing {
                ui.wrapped(Face::Sans, t.pt(13.0), t.danger, &format!("The project “{}” is not in the catalog, so this will not be saved anywhere else. Create it on your phone.", o.request.as_ref().map_or("", |r| r.project.as_str())));
            } else if env_missing {
                ui.wrapped(Face::Sans, t.pt(13.0), t.danger, "That environment is not in this project.");
            } else if !folder_ok {
                ui.wrapped(Face::Sans, t.pt(13.0), t.danger, "That folder has a character it cannot have.");
            }
            if let Some(e) = &o.error {
                ui.wrapped(Face::Sans, t.pt(13.0), t.danger, e);
            }
            let secret_len = o.value.text.trim_end_matches(['\r', '\n']).len();
            let ready = secret_len > 0
                && secret_len <= secrets::MAX_VALUE
                && secrets::valid_name(o.name.text.trim())
                && o.project.is_some()
                && o.environment.is_some()
                && folder_ok
                && !env_missing
                && !missing;
            ui.row(30.0, |ui| match o.phase {
                Phase::Saving => ui.dim("Saving…"),
                Phase::Replace => {
                    ui.label("It exists already.");
                    if ui.owner_only(|ui| ui.primary_button("Replace it")) {
                        do_replace = true;
                    }
                    if ui.button("Keep the old one") {
                        // Dropping the save wipes the value it held.
                        o.replacing = None;
                        o.phase = Phase::Idle;
                    }
                }
                Phase::Idle => {
                    // Only the owner's own press stores it: Lucina cannot save a secret through this dialog.
                    if (ui.owner_only(|ui| ui.primary_button("OK")) || submitted) && ready {
                        do_save = true;
                    }
                    if o.manual {
                        if ui.button("Cancel") {
                            close = true;
                        }
                    } else if ui.button("Not now") {
                        not_now = true;
                    }
                }
            });
        });
        if do_save {
            self.save(projects, engine);
        }
        if do_replace {
            if let Some(o) = &mut self.open {
                if let Some(mut save) = o.replacing.take() {
                    save.overwrite = true;
                    o.phase = Phase::Saving;
                    engine.save_secret(save);
                }
            }
        }
        if close {
            self.wipe();
            self.open = None;
        }
        if not_now {
            self.wipe();
            if let Some(r) = self.open.take().and_then(|o| o.request) {
                engine.dismiss_secret(&r.id);
                self.closed.push(r.id);
            }
        }
    }
}
