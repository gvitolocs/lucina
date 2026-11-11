//! The one thread that touches the vault: disk reads and writes never reach `view`. Jobs in, messages out, one at a time, so each
//! save sees the revision of the one before.

use std::path::{Path, PathBuf};

use lucina_app::Image;
use lucina_vault::graph::{Graph, GraphOptions};
use lucina_vault::markdown::{self, Block};
use lucina_vault::search::Hit;
use lucina_vault::vault::{Backlink, Counts, TagCount};
use lucina_vault::{Error, Options, Vault};

pub enum Job {
    Open(PathBuf),
    Refresh,
    Note(String),
    /// A link clicked in the note at `from`.
    Follow {
        from: String,
        link: String,
    },
    Search {
        q: String,
        generation: u64,
    },
    Graph {
        around: Option<String>,
        depth: u32,
    },
    Image {
        target: String,
        from: String,
        w: u32,
        h: u32,
    },
    AllowWrites,
    Save {
        path: String,
        text: String,
        revision: String,
        quitting: bool,
    },
    /// Text that lost a save: a new note beside the original, which stays as it is.
    KeepCopy {
        path: String,
        text: String,
    },
    Create {
        name: String,
        folder: String,
    },
    Rename {
        from: String,
        to: String,
    },
    Trash(String),
    /// Bring the vault folder at this root in step with the owner's vault on buildhost (`crate::sync`).
    Sync(PathBuf),
}

#[derive(Clone)]
pub struct OutLink {
    pub label: String,
    pub dest: Option<String>,
}

pub struct NoteData {
    pub path: String,
    pub name: String,
    pub text: String,
    pub revision: String,
    pub blocks: Vec<Block>,
    pub tags: Vec<String>,
    pub aliases: Vec<String>,
    pub backlinks: Vec<Backlink>,
    pub outgoing: Vec<OutLink>,
}

pub struct Listing {
    pub root: String,
    pub notes: Vec<String>,
    pub files: Vec<String>,
    pub tags: Vec<TagCount>,
    pub counts: Counts,
    pub writable: bool,
}

pub enum Msg {
    Opened(Box<Listing>),
    /// No vault there. The screen says where it looked.
    Missing(String),
    Note(Box<NoteData>),
    Hits {
        generation: u64,
        names: Vec<String>,
        hits: Vec<Hit>,
    },
    Graph(Graph),
    Image {
        target: String,
        image: Option<Image>,
    },
    Saved {
        path: String,
        revision: String,
    },
    Conflict {
        path: String,
    },
    /// Something happened that the status line says.
    Said(String),
    Failed(String),
}

pub struct Worker {
    vault: Option<Vault>,
    state: PathBuf,
}

/// Today in UTC, for a conflict copy's name.
fn today() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64);
    // Civil date from days, proleptic Gregorian.
    let z = secs.div_euclid(86_400) + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}-{m:02}-{d:02}")
}

impl Worker {
    pub fn new(state: PathBuf) -> Worker {
        Worker { vault: None, state }
    }

    /// Editing is off until this device was told it may, per vault: a file outside the vault, which holds nothing of ours.
    fn writes_allowed(&self, root: &Path) -> bool {
        std::fs::read_to_string(self.state.join("writable"))
            .is_ok_and(|s| s.lines().any(|l| Path::new(l) == root))
    }

    fn allow_writes(&self, root: &Path) -> std::io::Result<()> {
        std::fs::create_dir_all(&self.state)?;
        let p = self.state.join("writable");
        let mut s = std::fs::read_to_string(&p).unwrap_or_default();
        s.push_str(&format!("{}\n", root.display()));
        std::fs::write(p, s)
    }

    fn listing(v: &Vault) -> Listing {
        Listing {
            root: v.root().display().to_string(),
            notes: v.notes().map(|n| n.path.clone()).collect(),
            files: v.attachments().to_vec(),
            tags: v.tags(),
            counts: v.counts(),
            writable: v.writable(),
        }
    }

    fn note(v: &Vault, path: &str) -> Result<NoteData, Error> {
        let n = v
            .note(path)
            .ok_or_else(|| Error::NotFound(path.to_owned()))?;
        let t = v.read(path)?;
        let outgoing = v
            .outgoing(path)
            .into_iter()
            .filter(|(l, _)| !l.path().is_empty())
            .map(|(l, d)| OutLink {
                label: l
                    .display
                    .clone()
                    .filter(|_| !l.wiki)
                    .unwrap_or_else(|| l.link.clone()),
                dest: d.map(str::to_owned),
            })
            .collect();
        Ok(NoteData {
            path: path.to_owned(),
            name: n.name().to_owned(),
            blocks: markdown::blocks(&t.text),
            text: t.text,
            revision: t.revision,
            tags: n.scan.tags.clone(),
            aliases: n.scan.aliases.clone(),
            backlinks: v.backlinks(path),
            outgoing,
        })
    }

    /// One sync run, then the vault read again if anything came or went. Works before a vault is open: the first sync is
    /// how `/data/vault` gets its notes.
    fn sync(&mut self, root: &Path) -> Vec<Msg> {
        let Some(client) = lucina_account::system::client() else {
            return vec![Msg::Said("Sign in to keep this vault in step with the server.".into())];
        };
        if root == Path::new(crate::sync::SYNCED_ROOT) {
            if let Err(e) = std::fs::create_dir_all(root) {
                return vec![Msg::Failed(format!("could not make {}: {e}", root.display()))];
            }
        }
        let state = crate::sync::state_path(&self.state, root);
        let mut server = crate::sync::Server::new(client);
        match crate::sync::run(root, &state, &mut server, &today(), &mut |_| {}) {
            Ok(r) => {
                let said = Msg::Said(crate::sync::summary(&r));
                if !r.changed() {
                    return vec![said];
                }
                let mut out = match self.vault.as_mut() {
                    Some(v) if v.root() == root => match v.refresh() {
                        Ok(_) => vec![Msg::Opened(Box::new(Self::listing(v)))],
                        Err(e) => vec![Msg::Failed(e.to_string())],
                    },
                    _ => self.handle(Job::Open(root.to_path_buf())),
                };
                out.push(said);
                out
            }
            Err(e) => vec![Msg::Said(format!("Sync: {e}"))],
        }
    }

    pub fn handle(&mut self, job: Job) -> Vec<Msg> {
        if let Job::Sync(root) = &job {
            let root = root.clone();
            return self.sync(&root);
        }
        if let Job::Open(root) = &job {
            let wanted = root.exists();
            if !wanted {
                return vec![Msg::Missing(root.display().to_string())];
            }
            let writable = std::fs::canonicalize(root).is_ok_and(|c| self.writes_allowed(&c));
            let opts = Options {
                writable,
                ..Default::default()
            };
            return match Vault::open(root, opts) {
                Ok(v) => {
                    let l = Self::listing(&v);
                    self.vault = Some(v);
                    vec![Msg::Opened(Box::new(l))]
                }
                Err(e) => vec![Msg::Missing(format!("{}: {e}", root.display()))],
            };
        }
        let Some(v) = self.vault.as_mut() else {
            return vec![Msg::Failed("the vault is not open".into())];
        };
        let done = |v: &Vault, said: String| {
            vec![Msg::Opened(Box::new(Self::listing(v))), Msg::Said(said)]
        };
        match job {
            Job::Open(_) | Job::Sync(_) => Vec::new(),
            Job::Refresh => match v.refresh() {
                Ok(n) => done(v, format!("{n} notes read again")),
                Err(e) => vec![Msg::Failed(e.to_string())],
            },
            Job::Note(path) => match Self::note(v, &path) {
                Ok(n) => vec![Msg::Note(Box::new(n))],
                Err(e) => vec![Msg::Failed(e.to_string())],
            },
            Job::Follow { from, link } => {
                if link.starts_with('#') {
                    return vec![Msg::Said("That link is in this note.".into())];
                }
                match v.resolve(&from, &link).map(str::to_owned) {
                    Some(dest) if v.note(&dest).is_some() => match Self::note(v, &dest) {
                        Ok(n) => vec![Msg::Note(Box::new(n))],
                        Err(e) => vec![Msg::Failed(e.to_string())],
                    },
                    Some(dest) => vec![Msg::Said(format!("{dest} is a file, not a note."))],
                    None => vec![Msg::Said(format!("No note called {link} yet."))],
                }
            }
            Job::Search { q, generation } => vec![Msg::Hits {
                generation,
                names: v.quick_switch(&q, 8),
                hits: v.search(&q, 200),
            }],
            Job::Graph { around, depth } => {
                let o = GraphOptions {
                    around,
                    depth,
                    ..Default::default()
                };
                vec![Msg::Graph(v.graph(&o))]
            }
            Job::Image { target, from, w, h } => {
                let image = v
                    .resolve(&from, &target)
                    .and_then(|d| v.file_path(d).ok())
                    .filter(|p| std::fs::metadata(p).is_ok_and(|m| m.len() < 64 * 1024 * 1024))
                    .and_then(|p| Image::open(&p.display().to_string()).ok())
                    .map(|i| i.fit_within(w, h));
                vec![Msg::Image { target, image }]
            }
            Job::AllowWrites => {
                let root = v.root().to_path_buf();
                if let Err(e) = self.allow_writes(&root) {
                    return vec![Msg::Failed(format!("could not note that: {e}"))];
                }
                let opts = Options {
                    writable: true,
                    ..Default::default()
                };
                match Vault::open(&root, opts) {
                    Ok(n) => {
                        let m = done(&n, "Editing is on for this vault on this device.".into());
                        self.vault = Some(n);
                        m
                    }
                    Err(e) => vec![Msg::Failed(e.to_string())],
                }
            }
            Job::Save {
                path,
                text,
                revision,
                quitting,
            } => match v.save(&path, &text, &revision) {
                Ok(revision) => vec![Msg::Saved { path, revision }],
                Err(Error::Conflict { .. }) if quitting => {
                    // No one left to ask: keep both.
                    let _ = v.save_conflict_copy(&path, &text, &today(), "juno");
                    Vec::new()
                }
                Err(Error::Conflict { path, .. }) => vec![Msg::Conflict { path }],
                Err(e) => vec![Msg::Failed(e.to_string())],
            },
            Job::KeepCopy { path, text } => {
                match v.save_conflict_copy(&path, &text, &today(), "juno") {
                    Ok(copy) => {
                        let mut m = done(v, format!("Your text is in {copy}"));
                        if let Ok(n) = Self::note(v, &copy) {
                            m.push(Msg::Note(Box::new(n)));
                        }
                        m
                    }
                    Err(e) => vec![Msg::Failed(e.to_string())],
                }
            }
            Job::Create { name, folder } => {
                let path = v.new_note_path(&name, &folder);
                match v.create(&path, b"") {
                    Ok(_) => {
                        let mut m = done(v, format!("Created {path}"));
                        if let Ok(n) = Self::note(v, &path) {
                            m.push(Msg::Note(Box::new(n)));
                        }
                        m
                    }
                    Err(e) => vec![Msg::Failed(e.to_string())],
                }
            }
            Job::Rename { from, to } => {
                let out = v.plan_rename(&from, &to).and_then(|p| v.rename(&p));
                match out {
                    Ok(updated) => {
                        let mut m =
                            done(v, format!("Moved to {to}, {} notes updated", updated.len()));
                        if let Ok(n) = Self::note(v, &to) {
                            m.push(Msg::Note(Box::new(n)));
                        }
                        m
                    }
                    Err(e) => vec![Msg::Failed(e.to_string())],
                }
            }
            Job::Trash(path) => match v.trash(&path) {
                Ok(to) => done(v, format!("Moved to {to}")),
                Err(e) => vec![Msg::Failed(e.to_string())],
            },
        }
    }
}
