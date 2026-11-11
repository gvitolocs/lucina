//! The `/v1/secrets*` routes of `web/src/api/secrets.ts` behind a trait, the thread that sends them, and sample mode's server.
//!
//! Names and fingerprints come back, never values. A value is a [`Secret`] inside a [`SaveSecret`]: it goes to the worker, out
//! once in the body of the save, and is wiped when the save is dropped. It comes back only as that same save, for "Replace" or
//! "Try again", never as text.

use std::collections::{HashMap, HashSet};
use std::sync::mpsc;

use lucina_account::secrets::{Folder, Project, SaveSecret, SecretName, SecretQueue, SecretReceipt, SecretRequest};
use lucina_account::{ApiError, Client};
use lucina_app::Sender;

pub trait Server: Send {
    fn catalog(&mut self) -> Result<Vec<Project>, ApiError>;
    fn names(&mut self, project: &str, environment: &str, path: &str) -> Result<Folder, ApiError>;
    fn requests(&mut self) -> Result<SecretQueue, ApiError>;
    fn dismiss(&mut self, id: &str) -> Result<(), ApiError>;
    fn save(&mut self, save: &SaveSecret) -> Result<SecretReceipt, ApiError>;
    fn create_project(&mut self, name: &str) -> Result<Project, ApiError>;
}

pub struct Account {
    pub client: Client,
}

impl Server for Account {
    fn catalog(&mut self) -> Result<Vec<Project>, ApiError> {
        self.client.secrets_catalog()
    }

    fn names(&mut self, project: &str, environment: &str, path: &str) -> Result<Folder, ApiError> {
        self.client.secret_names(project, environment, path)
    }

    fn requests(&mut self) -> Result<SecretQueue, ApiError> {
        self.client.secret_requests()
    }

    fn dismiss(&mut self, id: &str) -> Result<(), ApiError> {
        self.client.secret_dismiss(id)
    }

    fn save(&mut self, save: &SaveSecret) -> Result<SecretReceipt, ApiError> {
        self.client.secret_save(save)
    }

    fn create_project(&mut self, name: &str) -> Result<Project, ApiError> {
        self.client.secret_create_project(name)
    }
}

pub enum Job {
    Catalog,
    Names { project: String, environment: String, path: String },
    Queue,
    Dismiss(String),
    Save(Box<SaveSecret>),
    CreateProject { name: String, for_card: bool },
}

pub enum Msg {
    Changed,
    SignedIn,
    SignedOut,
    Catalog(Result<Vec<Project>, String>),
    Names { place: (String, String, String), result: Result<Folder, String> },
    Queue(Result<SecretQueue, String>),
    Dismissed(Result<(), String>),
    Saved(SecretReceipt),
    /// The same save back: `exists` asks for one more press, anything else can be tried again. Never text.
    NotSaved { save: Box<SaveSecret>, exists: bool, message: String },
    Created { result: Result<Project, String>, for_card: bool },
}

/// `describe` in `components/SecretCard.tsx`.
pub fn describe(e: &ApiError) -> String {
    match e.code() {
        Some("secrets_unconfigured") => "Add secret isn't set up on the server yet.".to_owned(),
        Some("upstream_unauthorized") => "Infisical didn't allow it: the owner identity needs an organization role that can create projects.".to_owned(),
        _ => e.to_string(),
    }
}

/// Runs one job and says what happened. 401 ends in [`Msg::SignedOut`] whatever the job was.
pub fn run(server: &mut dyn Server, job: Job) -> Msg {
    fn or_out<T>(r: Result<T, ApiError>, f: impl FnOnce(Result<T, String>) -> Msg) -> Msg {
        match r {
            Err(ApiError::Unauthorized(_)) => Msg::SignedOut,
            r => f(r.map_err(|e| describe(&e))),
        }
    }
    match job {
        Job::Catalog => or_out(server.catalog(), Msg::Catalog),
        Job::Names { project, environment, path } => {
            let r = server.names(&project, &environment, &path);
            or_out(r, |result| Msg::Names { place: (project, environment, path), result })
        }
        Job::Queue => or_out(server.requests(), Msg::Queue),
        Job::Dismiss(id) => or_out(server.dismiss(&id), Msg::Dismissed),
        Job::CreateProject { name, for_card } => or_out(server.create_project(&name), |result| Msg::Created { result, for_card }),
        Job::Save(save) => match server.save(&save) {
            // `save` drops here, and the value with it.
            Ok(r) => Msg::Saved(r),
            Err(ApiError::Unauthorized(_)) => Msg::SignedOut,
            Err(e) => Msg::NotSaved { exists: e.code() == Some("exists"), message: describe(&e), save },
        },
    }
}

pub fn start_worker(tx: Sender<Msg>, mut server: Box<dyn Server>) -> Link {
    let (jobs, rx) = mpsc::channel::<Job>();
    std::thread::spawn(move || {
        while let Ok(job) = rx.recv() {
            tx.send(run(server.as_mut(), job));
        }
    });
    Box::new(move |j| {
        let _ = jobs.send(j);
    })
}

pub type Link = Box<dyn FnMut(Job)>;

fn request(id: &str, name: &str, status: &str, project: &str, reason: &str) -> SecretRequest {
    SecretRequest {
        id: id.into(),
        name: name.into(),
        project: project.into(),
        environment: "prod".into(),
        path: "/".into(),
        reason: reason.into(),
        who: "claude-code".into(),
        status: status.into(),
        fingerprint: None,
        action: None,
    }
}

/// Sample mode: made-up names in memory. A save learns the value's length and never reads it; the fingerprint is made up.
pub struct Sample {
    projects: Vec<Project>,
    folders: HashMap<String, Folder>,
    queue: SecretQueue,
    saved: HashSet<String>,
}

impl Sample {
    pub fn new() -> Sample {
        let project = |id: &str, name: &str, alias: &str, envs: &[&str]| Project {
            id: id.into(),
            name: name.into(),
            slug: Some(alias.into()),
            alias: Some(alias.into()),
            environments: envs.iter().map(|e| (*e).to_owned()).collect(),
        };
        let mut folders = HashMap::new();
        let secret = |k: &str| SecretName { key: k.into(), updated_at: Some("2026-10-28T09:30:00.000Z".into()) };
        folders.insert("p-sample/prod//".to_owned(), Folder { folders: vec!["mail".into(), "wifi".into()], secrets: vec![secret("SAMPLE_API_KEY"), secret("SAMPLE_WEBHOOK")] });
        folders.insert("p-sample/prod//wifi".to_owned(), Folder { folders: Vec::new(), secrets: vec![secret("HOME_PSK")] });
        Sample {
            projects: vec![project("p-sample", "Sample project", "sample", &["prod", "dev"]), project("p-other", "Second project", "other", &["dev"])],
            folders,
            queue: SecretQueue {
                requests: vec![
                    request("sr-1", "WIFI_PSK", "active", "sample", "The laptop joins the Wi-Fi"),
                    request("sr-2", "MAP_TOKEN", "queued", "sample", "Maps need a token"),
                ],
                active: Some("sr-1".into()),
                queued: 1,
            },
            saved: HashSet::new(),
        }
    }

    fn advance(&mut self) {
        self.queue.active = None;
        if let Some(next) = self.queue.requests.iter_mut().find(|r| r.status == "queued") {
            next.status = "active".into();
            self.queue.active = Some(next.id.clone());
        }
        self.queue.queued = self.queue.requests.iter().filter(|r| r.status == "queued").count() as u64;
    }
}

impl Server for Sample {
    fn catalog(&mut self) -> Result<Vec<Project>, ApiError> {
        Ok(self.projects.clone())
    }

    fn names(&mut self, project: &str, environment: &str, path: &str) -> Result<Folder, ApiError> {
        let mut f = self.folders.get(&format!("{project}/{environment}/{path}")).cloned().unwrap_or_default();
        for s in &self.saved {
            if s.starts_with(&format!("{project}/{environment}/{path}#")) {
                let key = s.rsplit('#').next().unwrap_or("").to_owned();
                if !f.secrets.iter().any(|x| x.key == key) {
                    f.secrets.push(SecretName { key, updated_at: None });
                }
            }
        }
        Ok(f)
    }

    fn requests(&mut self) -> Result<SecretQueue, ApiError> {
        Ok(self.queue.clone())
    }

    fn dismiss(&mut self, id: &str) -> Result<(), ApiError> {
        if let Some(r) = self.queue.requests.iter_mut().find(|r| r.id == id) {
            r.status = "dismissed".into();
        }
        self.advance();
        Ok(())
    }

    fn save(&mut self, save: &SaveSecret) -> Result<SecretReceipt, ApiError> {
        let key = format!("{}/{}/{}#{}", save.project, save.environment, save.path, save.name);
        let exists = self.saved.contains(&key) || self.names(&save.project, &save.environment, &save.path)?.secrets.iter().any(|s| s.key == save.name);
        if exists && !save.overwrite {
            return Err(ApiError::Http(409, "exists".into(), format!("{} exists", save.name)));
        }
        self.saved.insert(key);
        let fingerprint = format!("{:08x}", (save.value.len() as u32).wrapping_mul(2_654_435_761));
        let action = if exists { "updated" } else { "created" };
        if let Some(r) = save.request.as_ref().and_then(|id| self.queue.requests.iter_mut().find(|r| &r.id == id)) {
            r.status = "saved".into();
            r.fingerprint = Some(fingerprint.clone());
            r.action = Some(action.into());
            self.advance();
        }
        Ok(SecretReceipt { key: save.name.clone(), action: action.into(), fingerprint, next_item_id: None })
    }

    fn create_project(&mut self, name: &str) -> Result<Project, ApiError> {
        let p = Project { id: format!("p-{name}"), name: name.into(), slug: None, alias: None, environments: vec!["dev".into(), "prod".into()] };
        self.projects.push(p.clone());
        self.projects.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(p)
    }
}
