//! CodeCor's data, pure so it is tested without a screen: the projects on buildhost (agentd's `/v1/agents/projects` plus the
//! agents labelled for them) and the folders in `/data/projects` ("On Juno"), and the Lucina-project rule that greys Codex out.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use lucina_agents::bucket::Bucket;
use lucina_agents::types::AgentListItem;
use lucina_agents_client::model::{self, ProjectRecord};

/// The label that puts an agent in a CodeCor project: `codecor.project=<repo basename>`.
pub const PROJECT_LABEL: &str = "codecor.project";
/// The label that makes an agent the project's coordinator: `codecor.role=coordinator` (anything else is a worker).
pub const ROLE_LABEL: &str = "codecor.role";

/// A checkout of a Lucina repo, as agentd lists it under this root.
pub const LUCINA_ROOT: &str = "/home/user/Projects/Lucina/";
/// The worktrees of those repos live here: `/home/user/Projects/.worktrees/<Repo>/…`.
pub const WORKTREES_ROOT: &str = "/home/user/Projects/.worktrees/";

/// A folder in `/data/projects`: a project that can also be opened in local mode, in the terminal.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LocalProject {
    pub name: String,
    pub path: PathBuf,
}

/// Which project a screen is about: one of agentd's, or a folder on Juno.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ProjectKey {
    /// agentd's `projectId`.
    Remote(String),
    /// A folder in `/data/projects`.
    Local(PathBuf),
}

/// The folders in `root`, by name. Files and unreadable entries are left out; a missing root is an empty list.
pub fn local_projects(root: &Path) -> Vec<LocalProject> {
    let mut out = Vec::new();
    if let Ok(entries) = std::fs::read_dir(root) {
        for e in entries.flatten() {
            if !e.file_type().is_ok_and(|t| t.is_dir()) {
                continue;
            }
            out.push(LocalProject {
                name: e.file_name().to_string_lossy().into_owned(),
                path: e.path(),
            });
        }
    }
    out.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
    out
}

/// The agents of one project: the items whose `codecor.project` label is `repo`. The coordinator (from `codecor.role`)
/// comes first, then the workers by last activity. At most one coordinator is shown: if several are labelled, the newest
/// is the coordinator, the rest are workers, and `warning` says so.
pub struct ProjectAgents {
    pub coordinator: Option<AgentListItem>,
    pub workers: Vec<AgentListItem>,
    pub warning: Option<String>,
}

impl ProjectAgents {
    pub fn all(&self) -> impl Iterator<Item = &AgentListItem> {
        self.coordinator.iter().chain(self.workers.iter())
    }

    pub fn len(&self) -> usize {
        self.all().count()
    }
}

pub fn agents_of_project<'a, I>(items: I, repo: &str) -> ProjectAgents
where
    I: IntoIterator<Item = &'a AgentListItem>,
{
    let members: Vec<&AgentListItem> = items
        .into_iter()
        .filter(|a| {
            a.archived_at.is_none() && a.labels.get(PROJECT_LABEL).is_some_and(|v| v == repo)
        })
        .collect();
    let mut coordinators: Vec<&AgentListItem> = members
        .iter()
        .copied()
        .filter(|a| a.labels.get(ROLE_LABEL).is_some_and(|v| v == "coordinator"))
        .collect();
    // Newest first, so the first is the one shown as the coordinator.
    coordinators.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
    let (coordinator, demoted) = match coordinators.split_first() {
        None => (None, Vec::new()),
        Some((first, rest)) => (
            Some((*first).clone()),
            rest.iter().map(|c| (*c).clone()).collect(),
        ),
    };
    let coordinator_ids: std::collections::HashSet<&str> =
        coordinators.iter().map(|c| c.id.as_str()).collect();
    let mut workers: Vec<AgentListItem> = members
        .into_iter()
        .filter(|a| !coordinator_ids.contains(a.id.as_str()))
        .cloned()
        .collect();
    workers.extend(demoted);
    // By last activity, newest first.
    workers.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
    let warning = (coordinators.len() > 1).then(|| {
        format!(
            "{} agents are labelled coordinator; the newest is shown as the coordinator",
            coordinators.len()
        )
    });
    ProjectAgents {
        coordinator,
        workers,
        warning,
    }
}

/// The archived agents of one project (the Sessions screen): the items whose `codecor.project` label is `repo` and
/// that are archived, newest first.
pub fn archived_of_project<'a, I>(items: I, repo: &str) -> Vec<AgentListItem>
where
    I: IntoIterator<Item = &'a AgentListItem>,
{
    let mut v: Vec<AgentListItem> = items
        .into_iter()
        .filter(|a| {
            a.archived_at.is_some() && a.labels.get(PROJECT_LABEL).is_some_and(|l| l == repo)
        })
        .cloned()
        .collect();
    v.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
    v
}

/// One row of the "On buildhost" list: a project agentd knows, with its CodeCor agent count and how many need the owner.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RemoteRow {
    pub key: ProjectKey,
    pub name: String,
    pub path: String,
    pub agents: usize,
    pub needs_you: usize,
    /// A Lucina repo: Codex is not used on it (owner rule).
    pub lucina: bool,
}

/// The "On buildhost" list, in agentd's order: every project with the agents labelled for its repo's basename.
pub fn remote_rows(
    projects: &[ProjectRecord],
    items: &BTreeMap<String, AgentListItem>,
) -> Vec<RemoteRow> {
    let repos = lucina_repos(projects);
    projects
        .iter()
        .map(|p| {
            let repo = model::base_name(&p.root_path);
            let agents = agents_of_project(items.values(), repo);
            let needs_you = agents
                .all()
                .filter(|a| a.bucket == Bucket::NeedsInput)
                .count();
            RemoteRow {
                key: ProjectKey::Remote(p.project_id.clone()),
                name: p.name.clone(),
                path: p.root_path.clone(),
                agents: agents.len(),
                needs_you,
                lucina: is_lucina_project(&p.root_path, &repos),
            }
        })
        .collect()
}

/// Find one of agentd's projects by what a caller may know it by: its `projectId`, its `rootPath`, its name, or the
/// basename of its path — exactly first, then case-insensitively. `None` when nothing matches.
pub fn resolve_remote<'a>(projects: &'a [ProjectRecord], arg: &str) -> Option<&'a ProjectRecord> {
    let arg = arg.trim();
    if arg.is_empty() {
        return None;
    }
    let base = model::base_name(arg);
    projects
        .iter()
        .find(|p| {
            p.project_id == arg
                || p.root_path == arg
                || p.name == arg
                || model::base_name(&p.root_path) == base
        })
        .or_else(|| {
            let low = arg.to_lowercase();
            let low_base = base.to_lowercase();
            projects.iter().find(|p| {
                p.name.to_lowercase() == low
                    || model::base_name(&p.root_path).to_lowercase() == low_base
                    || p.root_path.to_lowercase() == low
            })
        })
}

/// The basenames of the Lucina repos agentd lists under `/home/user/Projects/Lucina/` (e.g. `SecretaryIOS`).
pub fn lucina_repos(projects: &[ProjectRecord]) -> Vec<String> {
    let mut out: Vec<String> = projects
        .iter()
        .filter(|p| p.root_path.starts_with(LUCINA_ROOT))
        .map(|p| model::base_name(&p.root_path).to_owned())
        .collect();
    out.sort();
    out.dedup();
    out
}

/// Whether a path is a Lucina project: under `/home/user/Projects/Lucina/`, or a worktree of one of those repos under
/// `/home/user/Projects/.worktrees/<Repo>/…`. Codex is not used on these (owner rule).
pub fn is_lucina_project(path: &str, lucina_repos: &[String]) -> bool {
    if path.starts_with(LUCINA_ROOT) {
        return true;
    }
    if let Some(rest) = path.strip_prefix(WORKTREES_ROOT) {
        let repo = rest.split('/').next().unwrap_or("");
        if !repo.is_empty() && lucina_repos.iter().any(|r| r == repo) {
            return true;
        }
    }
    false
}
