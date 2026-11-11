//! The intents CodeCor offers Lucina (docs/research/juno-intents.md): `codecor.projects`, `codecor.open`,
//! `codecor.agents`, `codecor.new_agent`, `codecor.send`, `codecor.status`. Each calls the same functions the screens
//! call, never a second copy. A call that arrived before the lists were loaded keeps its [`Responder`] in
//! [`CodeCor::pending`] and is answered from `App::message`, once the store has the first list — and a status whose
//! timeline is still loading waits the same way — so a headless run, which never calls `view`, gets its answer too.
//! No intent answers a permission, archives, deletes, merges or sets anything on the server beyond these.

use lucina_app::intent::{ArgSpec, Call, Effect, Entity, IntentSpec, Reply, Responder};
use serde_json::{Value, json};

use lucina_agents::timeline::TimelineItem;
use lucina_agents_client::model::{self, agent_title, status_label};
use lucina_agents_client::store::TlStatus;

use crate::app::CodeCor;
use crate::new_agent::{self, Role};
use crate::projects::{self, ProjectKey};

/// What CodeCor offers, listed by `lucina-app-coder --intents` and named in `manifest.app`.
pub const INTENTS: &[IntentSpec] = &[
    IntentSpec {
        name: "codecor.projects",
        description: "Both project lists: the projects on buildhost and the folders on Juno, with names, paths and agent counts.",
        args: &[],
        effect: Effect::Read,
        shows: false,
        inverse: None,
    },
    IntentSpec {
        name: "codecor.open",
        description: "Open the Project screen for a project, by its name, path or id.",
        args: &[ArgSpec::text(
            "project",
            "The project's name, path or id.",
            true,
            1000,
        )],
        effect: Effect::Read,
        shows: true,
        inverse: None,
    },
    IntentSpec {
        name: "codecor.agents",
        description: "The agents of a project on buildhost: id, title, role, provider, model, state, branch and pending approvals.",
        args: &[ArgSpec::text(
            "project",
            "The project's name, path or id.",
            true,
            1000,
        )],
        effect: Effect::Read,
        shows: false,
        inverse: None,
    },
    IntentSpec {
        name: "codecor.new_agent",
        description: "Create an agent in a project: the worktree first, then the agent. Refused for Codex on a Lucina project and for a second coordinator.",
        args: &[
            ArgSpec::text("project", "The project's name, path or id.", true, 1000),
            ArgSpec::text(
                "provider",
                "The provider's id, as /v1/agents/providers lists it.",
                true,
                100,
            ),
            ArgSpec::text(
                "role",
                "worker or coordinator (the default is worker).",
                false,
                20,
            ),
            ArgSpec::text(
                "title",
                "The agent's title; the worktree branch is named after it.",
                false,
                200,
            ),
            ArgSpec::text("prompt", "The first prompt.", true, 20_000),
            ArgSpec::flag(
                "juno",
                "Give a Claude agent Juno's shell; each call asks the owner first.",
            ),
        ],
        effect: Effect::Write,
        shows: false,
        inverse: None,
    },
    IntentSpec {
        name: "codecor.send",
        description: "Send a message to an agent. An archived agent is refused.",
        args: &[
            ArgSpec::id("agent", "The agent's id.", true),
            ArgSpec::text("text", "The message.", true, 20_000),
        ],
        effect: Effect::Write,
        shows: false,
        inverse: None,
    },
    IntentSpec {
        name: "codecor.status",
        description: "An agent's state, a preview of its last message and how many approvals wait for the owner.",
        args: &[ArgSpec::id("agent", "The agent's id.", true)],
        effect: Effect::Read,
        shows: false,
        inverse: None,
    },
];

/// An intent whose answer waits for the store: the first list, or an agent's timeline.
pub enum Pending {
    /// Any intent that arrived before the lists were loaded: run once `loaded`.
    Waiting { call: Call, reply: Responder },
    /// A status whose timeline is still loading: answer once it is not.
    Status { agent: String, reply: Responder },
}

/// One of [`INTENTS`], with its arguments already checked. The same door `dispatch` uses.
pub fn intent(app: &mut CodeCor, call: Call, reply: Responder) {
    if !app.signed_in() {
        return reply.send(Reply::failed("CodeCor is not signed in to an account."));
    }
    // A start-up intent can arrive before the first list: act on nothing until the store is loaded, or an open would
    // find no project and a create would label an agent for a repo it cannot see.
    if !app.loaded {
        app.pending.push(Pending::Waiting { call, reply });
        return;
    }
    match call.name.as_str() {
        "codecor.projects" => projects_list(app, reply),
        "codecor.open" => open(app, &call, reply),
        "codecor.agents" => agents(app, &call, reply),
        "codecor.new_agent" => new_agent(app, &call, reply),
        "codecor.send" => send(app, &call, reply),
        "codecor.status" => status(app, &call, reply),
        _ => reply.send(Reply::failed(format!("no intent {} here", call.name))),
    }
}

/// Answer the intents that were waiting, now that a message has moved the store. Called from `App::message`.
pub fn answer_pending(app: &mut CodeCor) {
    let mut i = 0;
    while i < app.pending.len() {
        let ready = match &app.pending[i] {
            Pending::Waiting { .. } => app.loaded,
            Pending::Status { agent, .. } => app
                .store
                .timelines
                .get(agent)
                .is_some_and(|t| t.status != TlStatus::Loading),
        };
        if !ready {
            i += 1;
            continue;
        }
        match app.pending.remove(i) {
            Pending::Waiting { call, reply } => intent(app, call, reply),
            Pending::Status { agent, reply } => answer_status(app, &agent, reply),
        }
    }
}

/// Find a project by what a caller may know it by: one of agentd's, or a folder on Juno.
fn resolve_key(app: &CodeCor, arg: &str) -> Option<ProjectKey> {
    if let Some(p) = projects::resolve_remote(&app.projects, arg) {
        return Some(ProjectKey::Remote(p.project_id.clone()));
    }
    let arg = arg.trim();
    let base = model::base_name(arg);
    app.local
        .iter()
        .find(|l| {
            l.path.to_string_lossy() == arg
                || l.name == arg
                || model::base_name(&l.path.to_string_lossy()) == base
        })
        .map(|l| ProjectKey::Local(l.path.clone()))
        .or_else(|| {
            let low = arg.to_lowercase();
            app.local
                .iter()
                .find(|l| l.name.to_lowercase() == low)
                .map(|l| ProjectKey::Local(l.path.clone()))
        })
}

/// A key's display name and the id its entity carries.
fn key_name(app: &CodeCor, key: &ProjectKey) -> (String, String) {
    match key {
        ProjectKey::Remote(id) => {
            let name = app
                .projects
                .iter()
                .find(|p| &p.project_id == id)
                .map(|p| p.name.clone())
                .unwrap_or_else(|| id.clone());
            (name, id.clone())
        }
        ProjectKey::Local(path) => {
            let s = path.to_string_lossy().into_owned();
            (model::base_name(&s).to_owned(), s)
        }
    }
}

/// `codecor.projects`: both lists, names and paths, agent counts.
fn projects_list(app: &CodeCor, reply: Responder) {
    let rows = app.remote_rows();
    let buildhost: Vec<Value> = rows
        .iter()
        .map(|r| {
            let id = match &r.key {
                ProjectKey::Remote(id) => id.clone(),
                ProjectKey::Local(_) => String::new(),
            };
            json!({
                "id": id,
                "name": r.name,
                "path": r.path,
                "agents": r.agents,
                "needs_you": r.needs_you,
                "lucina": r.lucina,
            })
        })
        .collect();
    let juno: Vec<Value> = app
        .local
        .iter()
        .map(|l| {
            json!({
                "name": l.name,
                "path": l.path.to_string_lossy(),
                "agents": 0,
            })
        })
        .collect();
    let n = buildhost.len();
    let j = juno.len();
    let mut r = Reply::done(format!("{n} projects on buildhost, {j} folders on Juno"))
        .with_value(json!({ "buildhost": buildhost, "juno": juno }));
    for row in &rows {
        let (name, id) = key_name(app, &row.key);
        r = r.with_entity(Entity::new("project", &id, &name));
    }
    for l in &app.local {
        r = r.with_entity(Entity::new("project", &l.path.to_string_lossy(), &l.name));
    }
    reply.send(r);
}

/// `codecor.open`: the Project screen for a project by name or path, as Enter on its row does.
fn open(app: &mut CodeCor, call: &Call, reply: Responder) {
    let arg = call.args["project"].as_str().unwrap_or("");
    let Some(key) = resolve_key(app, arg) else {
        return reply.send(Reply::failed(format!("no project {arg:?}")));
    };
    app.open_project(key.clone());
    let (name, id) = key_name(app, &key);
    reply.send(
        Reply::done(format!("Opened {name}")).with_entity(Entity::new("project", &id, &name)),
    );
}

/// `codecor.agents`: the project's rows, as the Project screen lists them.
fn agents(app: &mut CodeCor, call: &Call, reply: Responder) {
    let arg = call.args["project"].as_str().unwrap_or("");
    let Some(p) = projects::resolve_remote(&app.projects, arg).cloned() else {
        if resolve_key(app, arg).is_some() {
            return reply.send(Reply::failed(format!(
                "{arg:?} is a folder on Juno; its agents run locally, there is no list on buildhost"
            )));
        }
        return reply.send(Reply::failed(format!("no project {arg:?}")));
    };
    let key = ProjectKey::Remote(p.project_id.clone());
    let rows = app.agent_rows(&key);
    let list: Vec<Value> = rows
        .iter()
        .map(|r| {
            let role = if r.coordinator {
                "coordinator".to_owned()
            } else {
                r.item
                    .labels
                    .get(projects::ROLE_LABEL)
                    .cloned()
                    .unwrap_or_else(|| "worker".to_owned())
            };
            json!({
                "id": r.item.id,
                "title": agent_title(r.item.title.as_deref(), &r.item.id, &r.item.cwd),
                "role": role,
                "provider": r.item.provider,
                "model": r.item.model.clone().unwrap_or_default(),
                "state": status_label(r.item.bucket, r.item.status),
                "branch": r.branch,
                "pending": r.pending,
            })
        })
        .collect();
    let n = list.len();
    let mut r =
        Reply::done(format!("{n} agents in {}", p.name)).with_value(json!({ "agents": list }));
    for row in &rows {
        let title = agent_title(row.item.title.as_deref(), &row.item.id, &row.item.cwd);
        r = r.with_entity(Entity::new("agent", &row.item.id, &title));
    }
    reply.send(r);
}

/// `codecor.new_agent`: the phase-B create path, with its two refusals.
fn new_agent(app: &mut CodeCor, call: &Call, reply: Responder) {
    let arg = call.args["project"].as_str().unwrap_or("");
    let Some(p) = projects::resolve_remote(&app.projects, arg).cloned() else {
        return reply.send(Reply::failed(format!("no project {arg:?}")));
    };
    let provider = call.args["provider"]
        .as_str()
        .unwrap_or("")
        .trim()
        .to_owned();
    let role = match call.args["role"].as_str().unwrap_or("worker") {
        "coordinator" => Role::Coordinator,
        _ => Role::Worker,
    };
    let title = call.args["title"].as_str().unwrap_or("").trim().to_owned();
    let prompt = call.args["prompt"].as_str().unwrap_or("").trim().to_owned();
    let juno = call.args["juno"].as_bool().unwrap_or(false);
    // The same refusals the form makes: Codex on a Lucina project, the Juno shell off Claude, a second coordinator.
    let lucina = projects::is_lucina_project(&p.root_path, &projects::lucina_repos(&app.projects));
    if lucina && provider == "codex" {
        return reply.send(Reply::failed(new_agent::CODEX_RULE_LINE));
    }
    if juno && provider != "claude" {
        return reply.send(Reply::failed(new_agent::JUNO_MODE_LINE));
    }
    let key = ProjectKey::Remote(p.project_id.clone());
    if role == Role::Coordinator && app.has_coordinator(&key) {
        return reply.send(Reply::failed("This project already has a coordinator."));
    }
    app.create_agent(
        p,
        provider,
        None,
        None,
        role,
        title,
        prompt,
        juno,
        Some(reply),
    );
}

/// `codecor.send`: the composer's send, refused for an archived agent.
fn send(app: &mut CodeCor, call: &Call, reply: Responder) {
    let id = call.args["agent"].as_str().unwrap_or("").to_owned();
    let text = call.args["text"].as_str().unwrap_or("").trim().to_owned();
    if app.store.items.get(&id).is_none() && app.store.snapshots.get(&id).is_none() {
        return reply.send(Reply::failed(format!("no agent {id:?}")));
    }
    if app.store.archived(&id) {
        return reply.send(Reply::failed(format!(
            "{id} is archived; unarchive it in the app before sending"
        )));
    }
    app.send_text(&id, &text, Some(reply));
}

/// `codecor.status`: the state, the last message's preview (its first 300 characters) and the pending approvals count.
fn status(app: &mut CodeCor, call: &Call, reply: Responder) {
    let id = call.args["agent"].as_str().unwrap_or("").to_owned();
    if app.store.items.get(&id).is_none() && app.store.snapshots.get(&id).is_none() {
        return reply.send(Reply::failed(format!("no agent {id:?}")));
    }
    // The preview needs the timeline: read it when it is not there yet, answer at once when it is.
    if !app
        .store
        .timelines
        .get(&id)
        .is_some_and(|t| t.status != TlStatus::Loading)
    {
        app.load_timeline(id.clone());
    }
    if app
        .store
        .timelines
        .get(&id)
        .is_some_and(|t| t.status != TlStatus::Loading)
    {
        answer_status(app, &id, reply);
    } else {
        app.pending.push(Pending::Status { agent: id, reply });
    }
}

fn answer_status(app: &CodeCor, id: &str, reply: Responder) {
    use lucina_agents::bucket::{BucketInput, state_bucket};
    let item = app.store.items.get(id);
    let snap = app.store.snapshots.get(id);
    let cwd = item
        .map(|i| i.cwd.as_str())
        .or_else(|| snap.map(|s| s.cwd.as_str()))
        .unwrap_or("");
    let title = item
        .and_then(|i| i.title.clone())
        .or_else(|| snap.and_then(|s| s.title.clone()))
        .unwrap_or_else(|| agent_title(None, id, cwd));
    let pending = app
        .store
        .permissions
        .values()
        .filter(|p| p.agent_id.as_deref() == Some(id) && p.status == "pending")
        .count();
    let (status, bucket) = match (item, snap) {
        (Some(i), _) => (i.status, i.bucket),
        // Only the snapshot is known: the bucket the list would derive from it.
        (None, Some(s)) => (
            s.status,
            state_bucket(BucketInput {
                status: s.status,
                pending_permission_count: pending,
                requires_attention: s.requires_attention.unwrap_or(false),
                attention_reason: s.attention_reason,
            }),
        ),
        (None, None) => return reply.send(Reply::failed(format!("no agent {id:?}"))),
    };
    let state = status_label(bucket, status);
    let preview = last_message(app, id);
    reply.send(
        Reply::done(format!("{title}: {state}, {pending} waiting"))
            .with_value(json!({
                "id": id,
                "state": state,
                "last_message": preview,
                "pending": pending,
            }))
            .with_entity(Entity::new("agent", id, &title)),
    );
}

/// The last user or assistant message of the timeline, its first 300 characters.
fn last_message(app: &CodeCor, id: &str) -> String {
    let rows = app
        .store
        .timelines
        .get(id)
        .map(|t| t.rows.as_slice())
        .unwrap_or(&[]);
    rows.iter()
        .rev()
        .find_map(|r| match &r.item {
            TimelineItem::UserMessage { text, .. }
            | TimelineItem::AssistantMessage { text, .. } => {
                Some(text.chars().take(300).collect::<String>())
            }
            _ => None,
        })
        .unwrap_or_default()
}
