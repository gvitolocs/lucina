//! CodeCor phase A and B, against the sample server through the same JSON as the wire: the project grouping by label,
//! the coordinator-first ordering, the local project listing, the Lucina-project rule, the New agent form (the worktree
//! request before the agent request, the labels, the Codex rule and a refusal shown as the server said it), and
//! `--render` of the screens.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use lucina_account::ApiError;
use lucina_agents::bucket::Bucket;
use lucina_agents::types::{AgentListItem, AgentStatus};
use lucina_app::{App, Harness, keys};
use serde_json::{Map, Value, json};

use lucina_agents_client::agent::Tab;
use lucina_agents_client::api::{Api, B, Blocks, Sample};
use lucina_agents_client::model::{ProjectRecord, ProviderInfo, Workspace};
use lucina_agents_client::pages::{Act, Line};

use crate::app::{CodeCor, CodecorSample, Msg, Screen, sample_api};
use crate::coordinator;
use crate::new_agent::{self, NewAgentForm, Role};
use crate::projects::{self, ProjectKey};

/// 2026-11-02 12:00 UTC.
const NOW: i64 = 1_793_620_800_000;

fn item(
    id: &str,
    labels: &[(&str, &str)],
    updated: &str,
    bucket: Bucket,
    status: AgentStatus,
) -> AgentListItem {
    let mut l = Map::new();
    for (k, v) in labels {
        l.insert((*k).to_owned(), (*v).into());
    }
    serde_json::from_value(json!({
        "id": id,
        "shortId": &id[..7.min(id.len())],
        "title": format!("Title {id}"),
        "provider": "claude",
        "model": "model-1",
        "status": status,
        "cwd": "/srv/work/repo",
        "createdAt": "2026-11-02T10:00:00.000Z",
        "updatedAt": updated,
        "labels": l,
        "bucket": bucket,
    }))
    .unwrap()
}

fn items(v: Vec<AgentListItem>) -> BTreeMap<String, AgentListItem> {
    v.into_iter().map(|a| (a.id.clone(), a)).collect()
}

#[test]
fn agents_are_grouped_by_the_project_label() {
    let list = items(vec![
        item(
            "a1",
            &[("codecor.project", "repo"), ("codecor.role", "worker")],
            "2026-11-02T11:00:00.000Z",
            Bucket::Done,
            AgentStatus::Idle,
        ),
        item(
            "a2",
            &[("codecor.project", "other")],
            "2026-11-02T11:30:00.000Z",
            Bucket::Running,
            AgentStatus::Running,
        ),
        item(
            "a3",
            &[("codecor.project", "repo")],
            "2026-11-02T10:00:00.000Z",
            Bucket::Done,
            AgentStatus::Idle,
        ),
        item(
            "a4",
            &[],
            "2026-11-02T12:00:00.000Z",
            Bucket::Done,
            AgentStatus::Idle,
        ),
    ]);
    let pa = projects::agents_of_project(list.values(), "repo");
    assert_eq!(pa.len(), 2);
    assert!(pa.all().all(|a| a.labels["codecor.project"] == "repo"));
    assert_eq!(projects::agents_of_project(list.values(), "nope").len(), 0);
}

#[test]
fn the_coordinator_comes_first_then_the_workers_by_last_activity() {
    let list = items(vec![
        item(
            "w-old",
            &[("codecor.project", "repo")],
            "2026-11-02T09:00:00.000Z",
            Bucket::Done,
            AgentStatus::Idle,
        ),
        item(
            "c",
            &[("codecor.project", "repo"), ("codecor.role", "coordinator")],
            "2026-11-02T08:00:00.000Z",
            Bucket::Running,
            AgentStatus::Running,
        ),
        item(
            "w-new",
            &[("codecor.project", "repo"), ("codecor.role", "worker")],
            "2026-11-02T11:00:00.000Z",
            Bucket::Done,
            AgentStatus::Idle,
        ),
        item(
            "w-mid",
            &[("codecor.project", "repo")],
            "2026-11-02T10:00:00.000Z",
            Bucket::Done,
            AgentStatus::Idle,
        ),
    ]);
    let pa = projects::agents_of_project(list.values(), "repo");
    assert_eq!(pa.coordinator.as_ref().map(|c| c.id.as_str()), Some("c"));
    assert_eq!(pa.warning, None);
    let order: Vec<&str> = pa.workers.iter().map(|a| a.id.as_str()).collect();
    assert_eq!(order, ["w-new", "w-mid", "w-old"]);
}

#[test]
fn several_coordinators_show_the_newest_as_coordinator_and_warn() {
    let list = items(vec![
        item(
            "c-old",
            &[("codecor.project", "repo"), ("codecor.role", "coordinator")],
            "2026-11-02T09:00:00.000Z",
            Bucket::Done,
            AgentStatus::Idle,
        ),
        item(
            "c-new",
            &[("codecor.project", "repo"), ("codecor.role", "coordinator")],
            "2026-11-02T11:00:00.000Z",
            Bucket::Running,
            AgentStatus::Running,
        ),
        item(
            "w",
            &[("codecor.project", "repo")],
            "2026-11-02T10:00:00.000Z",
            Bucket::Done,
            AgentStatus::Idle,
        ),
    ]);
    let pa = projects::agents_of_project(list.values(), "repo");
    assert_eq!(
        pa.coordinator.as_ref().map(|c| c.id.as_str()),
        Some("c-new")
    );
    let order: Vec<&str> = pa.workers.iter().map(|a| a.id.as_str()).collect();
    assert_eq!(
        order,
        ["w", "c-old"],
        "the demoted coordinator is a worker, by last activity"
    );
    let warning = pa.warning.expect("a warning");
    assert!(
        warning.contains("2 agents are labelled coordinator"),
        "{warning}"
    );
}

#[test]
fn local_projects_are_the_folders_sorted_by_name() {
    let root = tempdir("projects");
    std::fs::create_dir_all(root.join("zeta")).unwrap();
    std::fs::create_dir_all(root.join("alpha")).unwrap();
    std::fs::write(root.join("a-file"), b"x").unwrap();
    let found = projects::local_projects(&root);
    let names: Vec<&str> = found.iter().map(|p| p.name.as_str()).collect();
    assert_eq!(names, ["alpha", "zeta"]);
    assert_eq!(found[0].path, root.join("alpha"));
    assert!(projects::local_projects(&root.join("missing")).is_empty());
}

#[test]
fn a_lucina_project_is_a_checkout_or_a_worktree_of_a_lucina_repo() {
    let repos = vec!["SecretaryIOS".to_owned(), "LucinaOS".to_owned()];
    assert!(projects::is_lucina_project(
        "/home/user/Projects/Lucina/SecretaryIOS",
        &repos
    ));
    assert!(projects::is_lucina_project(
        "/home/user/Projects/Lucina/SecretaryIOS/crates/x",
        &repos
    ));
    assert!(projects::is_lucina_project(
        "/home/user/Projects/.worktrees/LucinaOS/codecor",
        &repos
    ));
    assert!(projects::is_lucina_project(
        "/home/user/Projects/.worktrees/SecretaryIOS/agents-p7/x",
        &repos
    ));
    assert!(!projects::is_lucina_project(
        "/home/user/Projects/.worktrees/OtherRepo/x",
        &repos
    ));
    assert!(!projects::is_lucina_project(
        "/home/user/Projects/.worktrees",
        &repos
    ));
    assert!(!projects::is_lucina_project("/srv/work/repo", &repos));
    assert!(
        !projects::is_lucina_project("/home/user/Projects/Lucina", &repos),
        "the root itself is not a project"
    );
}

#[test]
fn the_lucina_repos_are_the_basenames_of_the_projects_under_lucina() {
    let projects = vec![
        record("p1", "/home/user/Projects/Lucina/SecretaryIOS"),
        record("p2", "/home/user/Projects/Lucina/LucinaOS"),
        record("p3", "/srv/work/repo"),
        record("p4", "/home/user/Projects/Lucina/SecretaryIOS"), // the same repo twice
    ];
    assert_eq!(
        projects::lucina_repos(&projects),
        ["LucinaOS", "SecretaryIOS"]
    );
}

#[test]
fn the_remote_rows_count_the_agents_and_the_ones_that_need_you() {
    let projects = vec![
        record("p1", "/srv/work/repo"),
        record("p2", "/home/user/Projects/Lucina/LucinaOS"),
    ];
    let list = items(vec![
        item(
            "a1",
            &[("codecor.project", "repo")],
            "2026-11-02T11:00:00.000Z",
            Bucket::NeedsInput,
            AgentStatus::Running,
        ),
        item(
            "a2",
            &[("codecor.project", "repo")],
            "2026-11-02T10:00:00.000Z",
            Bucket::Done,
            AgentStatus::Idle,
        ),
        item(
            "a3",
            &[("codecor.project", "LucinaOS")],
            "2026-11-02T10:00:00.000Z",
            Bucket::Done,
            AgentStatus::Idle,
        ),
    ]);
    let rows = projects::remote_rows(&projects, &list);
    assert_eq!(rows.len(), 2);
    assert_eq!((rows[0].agents, rows[0].needs_you), (2, 1));
    assert!(!rows[0].lucina);
    assert_eq!((rows[1].agents, rows[1].needs_you), (1, 0));
    assert!(
        rows[1].lucina,
        "a checkout under /home/user/Projects/Lucina/ is a Lucina project"
    );
    assert_eq!(rows[0].key, ProjectKey::Remote("p1".to_owned()));
    assert_eq!(rows[0].name, "repo");
}

#[test]
fn the_app_on_the_sample_server_groups_the_sample_project() {
    let api: Arc<dyn Api> = sample_api(NOW);
    let mut h = Harness::new(CodeCor::new(Some(api), None).sample(), 1280, 800);
    h.app().start();
    for _ in 0..3 {
        h.core.input(lucina_app::Input::Wheel(0));
        h.core.paint();
    }
    let rows = h.app().remote_rows();
    assert_eq!(rows.len(), 3);
    let sample = rows
        .iter()
        .find(|r| r.key == ProjectKey::Remote("prj-sample".to_owned()))
        .expect("the sample project");
    assert_eq!(
        (sample.agents, sample.needs_you),
        (3, 1),
        "three agents, one waiting for the owner"
    );
    let lucina = rows
        .iter()
        .find(|r| r.key == ProjectKey::Remote("prj-lucina".to_owned()))
        .expect("the Lucina project of the sample");
    assert!(lucina.lucina, "a checkout under /home/user/Projects/Lucina/");
    h.app()
        .open_project(ProjectKey::Remote("prj-sample".to_owned()));
    for _ in 0..3 {
        h.core.input(lucina_app::Input::Wheel(0));
        h.core.paint();
    }
    assert_eq!(
        h.app().screen,
        Screen::Project(ProjectKey::Remote("prj-sample".to_owned()))
    );
    let rows = h
        .app()
        .agent_rows(&ProjectKey::Remote("prj-sample".to_owned()));
    assert_eq!(rows.len(), 3);
    assert_eq!(rows[0].item.id, "agt_sample_running");
    assert!(rows[0].coordinator, "the running one is the coordinator");
    assert_eq!(rows[0].branch, "sample-branch");
    assert_eq!(rows[0].pending, 2, "the shell request and the diff request");
    assert!(!rows[1].coordinator && !rows[2].coordinator);
    // Enter on an agent row opens the Agent screen; Esc goes back to the project.
    h.key(keys::KEY_ENTER);
    for _ in 0..2 {
        h.core.input(lucina_app::Input::Wheel(0));
        h.core.paint();
    }
    assert_eq!(
        h.app().screen,
        Screen::Agent {
            project: ProjectKey::Remote("prj-sample".to_owned()),
            id: "agt_sample_running".to_owned()
        }
    );
    h.key(keys::KEY_ESC);
    assert_eq!(
        h.app().screen,
        Screen::Project(ProjectKey::Remote("prj-sample".to_owned()))
    );
    // N opens the New agent form; C says the project already has a coordinator.
    h.app()
        .open_project(ProjectKey::Remote("prj-sample".to_owned()));
    for _ in 0..2 {
        h.core.input(lucina_app::Input::Wheel(0));
        h.core.paint();
    }
    h.key(keys::KEY_N);
    for _ in 0..2 {
        h.core.input(lucina_app::Input::Wheel(0));
        h.core.paint();
    }
    assert!(h.app().new_agent.is_some(), "N opens the form");
    assert_eq!(h.app().new_agent.as_ref().unwrap().role, Role::Worker);
    h.key(keys::KEY_ESC);
    for _ in 0..2 {
        h.core.input(lucina_app::Input::Wheel(0));
        h.core.paint();
    }
    assert!(h.app().new_agent.is_none(), "Esc closes the form");
    h.key(keys::KEY_C);
    for _ in 0..2 {
        h.core.input(lucina_app::Input::Wheel(0));
        h.core.paint();
    }
    assert_eq!(
        h.app().note.as_deref(),
        Some("This project already has a coordinator.")
    );
    assert!(h.app().new_agent.is_none());
}

#[test]
fn a_local_project_opens_in_the_terminal_through_the_shell_and_falls_back_to_the_line() {
    let api: Arc<dyn Api> = sample_api(NOW);
    let mut h = Harness::new(CodeCor::new(Some(api), None).sample(), 1280, 800);
    h.app().start();
    for _ in 0..3 {
        h.core.input(lucina_app::Input::Wheel(0));
        h.core.paint();
    }
    let local = h.app().local.clone();
    assert_eq!(local.len(), 2, "the sample's On Juno list");
    h.app()
        .open_project(ProjectKey::Local(local[0].path.clone()));
    for _ in 0..2 {
        h.core.input(lucina_app::Input::Wheel(0));
        h.core.paint();
    }
    // The button shows the line to type in the Terminal; CodeCor types nothing itself.
    h.app().open_in_terminal(&local[0].path);
    let note = h.app().note.clone().expect("the note with the line");
    assert!(note.starts_with("In the Terminal (F2), type: "), "{note}");
    assert!(
        note.contains("--terminal '/data/projects/demo-app'"),
        "{note}"
    );
    assert!(note.contains("LUCINA_APP_DIR="), "{note}");
}

#[test]
fn render_draws_the_screens_as_pictures() {
    for (what, name) in [
        ("projects", "projects"),
        ("project", "project"),
        ("new-agent", "new-agent"),
        ("agent", "agent"),
        ("changes", "changes"),
        ("local", "local"),
    ] {
        let path = std::env::temp_dir().join(format!(
            "codecor-render-test-{}-{name}.ppm",
            std::process::id()
        ));
        crate::render_to(&path.to_string_lossy(), what);
        let bytes = std::fs::read(&path).expect("the picture");
        assert!(bytes.starts_with(b"P6\n"), "{what}: a binary PPM");
        assert!(bytes.len() > 500_000, "{what}: {} bytes", bytes.len());
        let _ = std::fs::remove_file(&path);
    }
}

// ---- Phase B: the New agent form ----

fn settle(h: &mut Harness<CodeCor>) {
    for _ in 0..3 {
        h.core.input(lucina_app::Input::Wheel(0));
        h.core.paint();
    }
}

fn sample_app() -> (Harness<CodeCor>, CodecorSample) {
    let s = CodecorSample::new(NOW);
    let api: Arc<dyn Api> = Arc::new(s.clone());
    let mut h = Harness::new(CodeCor::new(Some(api), None).sample(), 1280, 800);
    h.app().start();
    settle(&mut h);
    (h, s)
}

/// The labels of the ui tree on screen now, joined.
fn tree_text(h: &mut Harness<CodeCor>) -> String {
    h.tree()
        .update
        .nodes
        .iter()
        .filter_map(|(_, n)| n.label())
        .collect::<Vec<_>>()
        .join("\n")
}

fn ws() -> Workspace {
    serde_json::from_value(json!({
        "workspaceId": "wks_1", "projectId": "prj-1", "cwd": "/wt/repo/codecor-fix", "name": "codecor-fix"
    }))
    .unwrap()
}

#[test]
fn the_branch_name_comes_from_the_title() {
    assert_eq!(
        new_agent::branch_name("Fix the Login Bug!"),
        "codecor/fix-the-login-bug"
    );
    assert_eq!(
        new_agent::branch_name("  --Ünïcode__x  "),
        "codecor/n-code-x"
    );
    assert_eq!(new_agent::branch_name("retry 3x"), "codecor/retry-3x");
    assert_eq!(
        new_agent::branch_name(""),
        "codecor/agent",
        "an empty slug falls back"
    );
    assert_eq!(new_agent::branch_name("!!!"), "codecor/agent");
    let long = new_agent::branch_name(&"a".repeat(100));
    assert_eq!(
        long,
        format!("codecor/{}", "a".repeat(40)),
        "max 40 chars from the title"
    );
    let words = new_agent::branch_name(&format!("{} tail", "word ".repeat(30)));
    assert!(words.starts_with("codecor/word-word"));
    assert!(words.len() <= "codecor/".len() + 40);
    for b in [
        new_agent::branch_name("Fix the Login Bug!"),
        new_agent::branch_name(&"x".repeat(200)),
        new_agent::branch_name("Ærøskøbing — ø!"),
    ] {
        assert!(b.starts_with("codecor/"));
        assert!(
            b.chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '/'),
            "{b}"
        );
    }
}

#[test]
fn the_workspace_request_is_a_worktree_branching_off_main() {
    let w = new_agent::workspace_request("prj-1", "Fix the login bug", "Do it", "claude");
    assert_eq!(w["isolation"], "worktree");
    assert_eq!(w["projectId"], "prj-1");
    assert_eq!(w["mode"], "branch-off");
    assert_eq!(w["newBranch"], "codecor/fix-the-login-bug");
    assert_eq!(w["base"], "main");
    assert_eq!(w["title"], "Fix the login bug");
    assert_eq!(w["prompt"], "Do it");
    assert_eq!(w["provider"], "claude");
    let w = new_agent::workspace_request("prj-1", "  ", "", "claude");
    assert_eq!(w["newBranch"], "codecor/agent");
    assert!(w.as_object().unwrap().get("title").is_none());
    assert!(w.as_object().unwrap().get("prompt").is_none());
}

#[test]
fn the_agent_request_carries_the_workspace_and_the_labels() {
    let b = new_agent::agent_request(
        &ws(),
        "claude",
        Some("m1".to_owned()),
        Some("plan".to_owned()),
        "Fix login",
        "Do it",
        "repo",
        Role::Worker,
        false,
    );
    assert_eq!(b["provider"], "claude");
    assert_eq!(b["cwd"], "/wt/repo/codecor-fix");
    assert_eq!(b["workspaceId"], "wks_1");
    assert_eq!(b["model"], "m1");
    assert_eq!(b["modeId"], "plan");
    assert_eq!(b["title"], "Fix login");
    assert_eq!(b["prompt"], "Do it");
    assert_eq!(
        b["labels"],
        json!({ projects::PROJECT_LABEL: "repo", projects::ROLE_LABEL: "worker" })
    );
    assert!(
        b.as_object().unwrap().get(new_agent::JUNO_LABEL).is_none(),
        "no Juno label when the shell is off"
    );
    let b = new_agent::agent_request(
        &ws(),
        "claude",
        None,
        None,
        "",
        "",
        "repo",
        Role::Worker,
        true,
    );
    assert_eq!(
        b["labels"][new_agent::JUNO_LABEL],
        "bash",
        "the Juno shell's label"
    );
    let b = new_agent::agent_request(
        &ws(),
        "mistral",
        None,
        None,
        "",
        "",
        "repo",
        Role::Worker,
        false,
    );
    assert!(b.as_object().unwrap().get("model").is_none());
    assert!(b.as_object().unwrap().get("modeId").is_none());
    assert!(b.as_object().unwrap().get("title").is_none());
    assert!(b.as_object().unwrap().get("prompt").is_none());
}

#[test]
fn the_coordinator_preamble_names_the_project_and_lists_the_workers_by_id_and_title() {
    assert!(coordinator::COORDINATOR_PREAMBLE.contains("{project}"));
    assert!(coordinator::COORDINATOR_PREAMBLE.contains("{workers}"));
    let workers = vec![
        coordinator::WorkerRef {
            id: "agt_a".to_owned(),
            title: "One".to_owned(),
        },
        coordinator::WorkerRef {
            id: "agt_b".to_owned(),
            title: "Two".to_owned(),
        },
    ];
    let p = coordinator::preamble("LucinaOS", &workers);
    assert!(
        p.contains("You coordinate the CodeCor project LucinaOS."),
        "{p}"
    );
    assert!(p.contains("- agt_a — One"), "{p}");
    assert!(p.contains("- agt_b — Two"), "{p}");
    assert!(p.contains("lagent merge"), "{p}");
    assert!(p.contains("never answer permissions"), "{p}");
    assert!(p.contains("lucina_decide"), "{p}");
    assert!(coordinator::preamble("repo", &[]).contains("- (none yet)"));
    // The owner's text follows the preamble.
    let full = coordinator::prompt("repo", &workers, "  Split the work  ");
    assert!(full.ends_with("Split the work"), "{full}");
    assert!(full.contains("lucina_decide.\n\nSplit the work"), "{full}");
    let b = new_agent::agent_request(
        &ws(),
        "claude",
        None,
        None,
        "Coord",
        &full,
        "repo",
        Role::Coordinator,
        false,
    );
    assert_eq!(b["labels"][projects::ROLE_LABEL], "coordinator");
    assert_eq!(b["prompt"], full);
}

#[test]
fn codex_is_greyed_on_a_lucina_project_and_allowed_on_another() {
    let providers: Vec<ProviderInfo> = serde_json::from_value(json!([
        {"id": "claude", "label": "Claude", "modes": [], "models": []},
        {"id": "codex", "label": "Codex", "modes": [], "models": []},
    ]))
    .unwrap();
    assert_eq!(
        new_agent::CODEX_RULE_LINE,
        "Codex is not used on Lucina projects (owner rule). Pick Claude, Mistral or Qwen."
    );
    assert_eq!(
        new_agent::codex_blocked(true, &providers),
        Some(new_agent::CODEX_RULE_LINE)
    );
    assert_eq!(new_agent::codex_blocked(false, &providers), None);
    assert_eq!(
        new_agent::codex_blocked(true, &providers[..1]),
        None,
        "no Codex listed, no line"
    );
    // The choice skips Codex on a Lucina project and can reach it on another.
    assert_eq!(new_agent::initial_provider(&providers, true), 0);
    assert_eq!(
        new_agent::next_provider(&providers, 0, true),
        0,
        "Codex is skipped"
    );
    assert_eq!(new_agent::next_provider(&providers, 0, false), 1);
    assert_eq!(
        new_agent::next_provider(&providers[1..], 0, true),
        0,
        "nothing else to choose"
    );
}

#[test]
fn the_juno_shell_row_is_claude_only_and_needs_a_mode_that_asks() {
    let providers: Vec<ProviderInfo> = serde_json::from_value(json!([
        {"id": "claude", "label": "Claude", "modes": [{"id": "default", "label": "Default"}, {"id": "yolo", "label": "YOLO"}], "models": []},
        {"id": "mistral", "label": "Mistral", "modes": [], "models": []},
    ]))
    .unwrap();
    let mut f = NewAgentForm::new("prj".to_owned(), false, false, Role::Worker);
    f.providers = Some(providers);
    // On Claude the row is shown and the toggle works.
    assert!(f.juno_shown());
    assert!(!f.juno);
    f.toggle_juno();
    assert!(f.juno);
    // No mode is the provider's own default: fine. The asking modes are fine, the rest is not.
    assert!(new_agent::juno_mode_ok(None));
    for m in new_agent::JUNO_MODES {
        assert!(new_agent::juno_mode_ok(Some(m)), "{m} asks");
    }
    assert!(!new_agent::juno_mode_ok(Some("yolo")));
    // A mode that does not ask: the refusal under the form, so Create is refused locally.
    f.mode = Some(1);
    assert_eq!(f.juno_problem(), Some(new_agent::JUNO_MODE_LINE));
    f.mode = Some(0);
    assert_eq!(f.juno_problem(), None);
    // The shell off: any mode is fine.
    f.toggle_juno();
    f.mode = Some(1);
    assert_eq!(f.juno_problem(), None);
    // Off Claude the row is hidden, the toggle does nothing, and cycling back resets the flag.
    f.cycle_provider();
    assert_eq!(f.provider_info().unwrap().id.as_str(), "mistral");
    assert!(!f.juno);
    assert!(!f.juno_shown());
    f.juno = true;
    f.toggle_juno();
    assert!(f.juno, "the toggle does nothing off Claude");
    f.cycle_provider();
    assert_eq!(f.provider_info().unwrap().id.as_str(), "claude");
    assert!(!f.juno, "cycling providers resets the shell");
}

#[test]
fn a_refusal_message_is_shown_as_the_server_said_it() {
    let msg = "Codex is not used on Lucina projects (owner rule). Pick Claude, Mistral or Qwen for LucinaOS.";
    let e = ApiError::Http(403, "CODEX_NOT_ALLOWED".to_owned(), msg.to_owned());
    assert_eq!(new_agent::server_message(&e), msg);
    let e = ApiError::Http(400, "WORKSPACE_GUARD".to_owned(), "Nope".to_owned());
    assert_eq!(new_agent::server_message(&e), "Nope");
    let juno = "The Juno shell refused the call: it may only run on Claude agents.";
    let e = ApiError::Http(403, "JUNO_TOOL_REFUSED".to_owned(), juno.to_owned());
    assert_eq!(
        new_agent::server_message(&e),
        juno,
        "the server's words, as is"
    );
    let e = ApiError::Network("down".to_owned());
    assert_eq!(new_agent::server_message(&e), "No connection: down");
}

#[test]
fn creating_an_agent_makes_the_workspace_first_then_the_agent_with_its_id() {
    let (mut h, s) = sample_app();
    let key = ProjectKey::Remote("prj-sample".to_owned());
    h.app().open_project(key.clone());
    h.app().open_new_agent(key.clone(), Role::Worker);
    settle(&mut h);
    {
        let f = h.app().new_agent.as_mut().expect("the form is open");
        assert!(f.providers.is_some(), "the providers loaded");
        assert_eq!(f.provider_info().map(|p| p.id.as_str()), Some("claude"));
        f.title.set("Fix the flaky test");
        f.prompt.set("Make it pass");
        f.model = Some(1);
        f.mode = Some(1);
    }
    h.app().submit_new_agent();
    settle(&mut h);
    assert!(h.app().new_agent.is_none(), "the form closed on success");
    let writes = s.writes();
    assert_eq!(writes.len(), 2, "{writes:?}");
    assert_eq!(writes[0].0, format!("{B}/workspaces"), "the worktree first");
    assert_eq!(writes[0].1["isolation"], "worktree");
    assert_eq!(writes[0].1["projectId"], "prj-sample");
    assert_eq!(writes[0].1["mode"], "branch-off");
    assert_eq!(writes[0].1["newBranch"], "codecor/fix-the-flaky-test");
    assert_eq!(writes[0].1["base"], "main");
    assert_eq!(writes[0].1["provider"], "claude");
    assert_eq!(writes[1].0, B, "then the agent");
    assert_eq!(
        writes[1].1["workspaceId"], "wks_sample",
        "with the workspace's id"
    );
    assert_eq!(writes[1].1["cwd"], "/srv/work/sample-project");
    assert_eq!(writes[1].1["provider"], "claude");
    assert_eq!(writes[1].1["model"], "sample-model-2");
    assert_eq!(writes[1].1["modeId"], "plan");
    assert_eq!(writes[1].1["title"], "Fix the flaky test");
    assert_eq!(writes[1].1["prompt"], "Make it pass");
    assert_eq!(
        writes[1].1["labels"],
        json!({ projects::PROJECT_LABEL: "sample-project", projects::ROLE_LABEL: "worker" })
    );
    // The new agent's row is selected in the Project screen.
    let rows = h.app().agent_rows(&key);
    let at = h.app().agents.selected.expect("a selected row");
    assert!(
        rows[at].item.id.starts_with("agt_new_"),
        "{}",
        rows[at].item.id
    );
    assert_eq!(
        rows[at].item.labels[projects::PROJECT_LABEL],
        "sample-project"
    );
}

#[test]
fn on_a_lucina_project_codex_is_greyed_out_and_cannot_be_chosen() {
    let (mut h, _s) = sample_app();
    let key = ProjectKey::Remote("prj-lucina".to_owned());
    h.app().open_project(key.clone());
    h.app().open_new_agent(key.clone(), Role::Worker);
    settle(&mut h);
    {
        let f = h.app().new_agent.as_ref().unwrap();
        assert!(f.lucina);
        assert_eq!(f.codex_line(), Some(new_agent::CODEX_RULE_LINE));
        assert_ne!(f.provider_info().map(|p| p.id.as_str()), Some("codex"));
        let n = f.providers.as_ref().unwrap().len();
        for _ in 0..n {
            h.app().new_agent.as_mut().unwrap().cycle_provider();
            assert_ne!(
                h.app()
                    .new_agent
                    .as_ref()
                    .unwrap()
                    .provider_info()
                    .map(|p| p.id.as_str()),
                Some("codex"),
                "cycling never lands on Codex"
            );
        }
        // The form on screen shows the line under the provider choice.
        let text = tree_text(&mut h);
        assert!(text.contains("New agent"), "{text}");
        assert!(text.contains("Provider: Claude"), "{text}");
        assert!(text.contains(new_agent::CODEX_RULE_LINE), "{text}");
    }
    // On another project the line is gone and Codex can be chosen.
    let key = ProjectKey::Remote("prj-sample".to_owned());
    h.app().open_new_agent(key.clone(), Role::Worker);
    settle(&mut h);
    let f = h.app().new_agent.as_ref().unwrap();
    assert!(!f.lucina);
    assert_eq!(f.codex_line(), None);
    h.app().new_agent.as_mut().unwrap().cycle_provider();
    assert_eq!(
        h.app()
            .new_agent
            .as_ref()
            .unwrap()
            .provider_info()
            .map(|p| p.id.as_str()),
        Some("codex")
    );
}

/// A server that still refuses Codex with `CODEX_NOT_ALLOWED`, as agentd does for a repo the client cannot see.
struct CodexRefused {
    inner: Sample,
}

impl Api for CodexRefused {
    fn call(&self, method: &str, path: &str, body: Option<&Value>) -> Result<Value, ApiError> {
        if method == "POST" && path == B {
            let _ = self.inner.call(method, path, body); // so the sample records the write
            return Err(ApiError::Http(
                403,
                "CODEX_NOT_ALLOWED".to_owned(),
                "Codex is not used on Lucina projects (owner rule). Pick Claude, Mistral or Qwen for sample-project.".to_owned(),
            ));
        }
        self.inner.call(method, path, body)
    }

    fn stream(&self, path: &str, last_id: Option<&str>) -> Result<Box<dyn Blocks>, ApiError> {
        self.inner.stream(path, last_id)
    }
}

#[test]
fn a_codex_refusal_shows_the_servers_message_and_keeps_the_form() {
    let s = Sample::new(NOW);
    let api: Arc<dyn Api> = Arc::new(CodexRefused { inner: s.clone() });
    let mut h = Harness::new(CodeCor::new(Some(api), None).sample(), 1280, 800);
    h.app().start();
    settle(&mut h);
    let key = ProjectKey::Remote("prj-sample".to_owned()); // not a Lucina project: Codex can be chosen here
    h.app().open_project(key.clone());
    h.app().open_new_agent(key.clone(), Role::Worker);
    settle(&mut h);
    {
        let f = h.app().new_agent.as_mut().unwrap();
        let codex = f
            .providers
            .as_ref()
            .unwrap()
            .iter()
            .position(|p| p.id == "codex")
            .unwrap();
        f.provider = codex;
        f.title.set("Try codex");
        f.prompt.set("hello");
    }
    h.app().submit_new_agent();
    settle(&mut h);
    let f = h.app().new_agent.as_ref().expect("the form is still open");
    assert!(!f.busy);
    assert_eq!(
        f.problem.as_deref(),
        Some(
            "Codex is not used on Lucina projects (owner rule). Pick Claude, Mistral or Qwen for sample-project."
        )
    );
    assert_eq!(f.title.text, "Try codex", "the form keeps what was typed");
    assert_eq!(f.prompt.text, "hello");
    let writes = s.writes();
    assert_eq!(writes.len(), 2, "{writes:?}");
    assert_eq!(
        writes[0].0,
        format!("{B}/workspaces"),
        "the worktree went first"
    );
    assert_eq!(writes[1].0, B);
    assert_eq!(writes[1].1["provider"], "codex");
}

#[test]
fn c_opens_the_form_as_coordinator_and_its_prompt_gets_the_preamble() {
    let (mut h, s) = sample_app();
    let key = ProjectKey::Remote("prj-other".to_owned()); // no agents, so no coordinator yet
    h.app().open_project(key.clone());
    assert!(!h.app().has_coordinator(&key));
    h.app().open_new_agent(key.clone(), Role::Coordinator);
    settle(&mut h);
    {
        let f = h.app().new_agent.as_mut().unwrap();
        assert_eq!(f.role, Role::Coordinator);
        assert!(
            f.can_coordinator,
            "the project has none, so the role can be picked"
        );
        f.title.set("Coordinate the project");
        f.prompt.set("Split the work");
    }
    h.app().submit_new_agent();
    settle(&mut h);
    assert!(h.app().new_agent.is_none());
    let writes = s.writes();
    assert_eq!(writes.len(), 2, "{writes:?}");
    assert_eq!(writes[1].0, B);
    assert_eq!(writes[1].1["labels"][projects::ROLE_LABEL], "coordinator");
    assert_eq!(
        writes[1].1["labels"][projects::PROJECT_LABEL],
        "other-project"
    );
    let prompt = writes[1].1["prompt"].as_str().unwrap();
    assert!(
        prompt.contains("You coordinate the CodeCor project other-project."),
        "{prompt}"
    );
    assert!(prompt.contains("- (none yet)"), "{prompt}");
    assert!(prompt.contains("lagent merge"), "{prompt}");
    assert!(prompt.ends_with("Split the work"), "{prompt}");
    assert_eq!(writes[0].1["newBranch"], "codecor/coordinate-the-project");
}

// ---- Phase C: the Agent screen, diff approvals, Sessions ----

/// The app on the sample server, on the running agent's Agent screen.
fn agent_app() -> (Harness<CodeCor>, CodecorSample) {
    let (mut h, s) = sample_app();
    h.app().fixed_now = Some(NOW);
    h.app().pane.pace = std::time::Duration::ZERO;
    let key = ProjectKey::Remote("prj-sample".to_owned());
    h.app().open_project(key.clone());
    settle(&mut h);
    h.app().open_agent(key, "agt_sample_running".to_owned());
    settle(&mut h);
    (h, s)
}

#[test]
fn the_agent_screen_shows_the_diff_card_above_allow_and_deny() {
    let (mut h, _s) = agent_app();
    let app = h.app();
    assert_eq!(
        app.screen,
        Screen::Agent {
            project: ProjectKey::Remote("prj-sample".to_owned()),
            id: "agt_sample_running".to_owned()
        }
    );
    let text = format!("{:?}", app.pane.lines);
    assert!(text.contains("retry(3, send)"), "{text}");
    assert!(
        text.contains("/srv/work/sample-project/src/sample.rs"),
        "{text}"
    );
    let first_diff = app
        .pane
        .lines
        .iter()
        .position(|l| matches!(l, Line::Diff(_)))
        .expect("the card's diff rows");
    let allow = app
        .pane
        .lines
        .iter()
        .position(|l| matches!(l, Line::Button { act: Act::Answer { key, allow: true, .. }, .. } if key == "agt_sample_running/perm-diff"))
        .expect("the diff card's Allow");
    assert!(first_diff < allow, "the diff is above Allow");
    // Numbered old/new lines: the remove row carries the old number, the add row the new one.
    let rows: Vec<String> = app
        .pane
        .lines
        .iter()
        .filter_map(|l| match l {
            Line::Diff(d) => Some(format!("{}:{}:{}", d.kind, d.n, d.content)),
            _ => None,
        })
        .collect();
    assert!(rows.contains(&"remove:2:    send()".to_owned()), "{rows:?}");
    assert!(
        rows.contains(&"add:2:    retry(3, send)".to_owned()),
        "{rows:?}"
    );
}

#[test]
fn allow_on_the_diff_card_posts_the_same_request_as_the_agents_app() {
    let (mut h, s) = agent_app();
    let at = h
        .app()
        .pane
        .lines
        .iter()
        .position(|l| matches!(l, Line::Button { act: Act::Answer { key, allow: true, .. }, .. } if key == "agt_sample_running/perm-diff"))
        .expect("the diff card's Allow");
    h.app().pane.main.select(at);
    settle(&mut h);
    h.key(keys::KEY_ENTER);
    settle(&mut h);
    let writes = s.writes();
    assert!(
        writes.iter().any(
            |(p, b)| p == &format!("{B}/agt_sample_running/permissions/perm-diff")
                && *b == json!({"behavior": "allow"})
        ),
        "{writes:?}"
    );
    let text = format!("{:?}", h.app().pane.lines);
    assert!(
        text.contains("Allowed"),
        "the card says how it ended: {text}"
    );
}

#[test]
fn the_brackets_walk_the_tabs_and_changes_reads_the_workspace_diff() {
    let (mut h, s) = agent_app();
    assert_eq!(h.app().pane.tab, Tab::Chat);
    h.key(keys::KEY_RIGHTBRACE);
    settle(&mut h);
    assert_eq!(h.app().pane.tab, Tab::Changes);
    assert!(
        s.reads(&format!("{B}/workspaces/wks_sample/diff")) >= 1,
        "the Changes tab read the workspace's diff"
    );
    let app = h.app();
    assert_eq!(
        app.pane.diff.result.as_ref().map(|d| d.files.len()),
        Some(3)
    );
    assert_eq!(app.pane.diff.rows[0].content, "@@ -1,3 +1,3 @@");
    h.key(keys::KEY_LEFTBRACE);
    settle(&mut h);
    assert_eq!(h.app().pane.tab, Tab::Chat);
}

#[test]
fn s_lists_the_archived_sessions_and_enter_unarchives_and_opens_one() {
    let (mut h, s) = sample_app();
    h.app().fixed_now = Some(NOW);
    let key = ProjectKey::Remote("prj-sample".to_owned());
    h.app().open_project(key.clone());
    settle(&mut h);
    h.key(keys::KEY_S);
    settle(&mut h);
    assert_eq!(h.app().screen, Screen::Sessions(key.clone()));
    let rows = h.app().sessions_rows(&key);
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].item.id, "agt_sample_archived");
    assert!(rows[0].item.archived_at.is_some());
    h.key(keys::KEY_ENTER);
    settle(&mut h);
    let writes = s.writes();
    assert!(
        writes
            .iter()
            .any(|(p, _)| p == &format!("{B}/agt_sample_archived/unarchive")),
        "{writes:?}"
    );
    assert_eq!(
        h.app().screen,
        Screen::Agent {
            project: key.clone(),
            id: "agt_sample_archived".to_owned()
        }
    );
    // Sending a message resumes it: the send is not held back for an archived agent.
    h.app().pane.composer.set("continue where you left off");
    h.app().send_prompt("agt_sample_archived");
    settle(&mut h);
    let writes = s.writes();
    assert!(
        writes
            .iter()
            .any(|(p, b)| p == &format!("{B}/agt_sample_archived/send")
                && b["prompt"] == "continue where you left off"),
        "{writes:?}"
    );
    assert_eq!(h.app().pane.composer.text, "");
}

#[test]
fn archived_sessions_are_filtered_by_the_project_label_newest_first() {
    let mut list = items(vec![
        item(
            "a1",
            &[("codecor.project", "repo")],
            "2026-11-02T11:00:00.000Z",
            Bucket::Done,
            AgentStatus::Idle,
        ),
        item(
            "a2",
            &[("codecor.project", "repo")],
            "2026-11-02T12:00:00.000Z",
            Bucket::Done,
            AgentStatus::Idle,
        ),
        item(
            "a3",
            &[("codecor.project", "other")],
            "2026-11-02T13:00:00.000Z",
            Bucket::Done,
            AgentStatus::Idle,
        ),
    ]);
    list.get_mut("a2").unwrap().archived_at = Some("2026-11-02T12:30:00.000Z".to_owned());
    list.get_mut("a3").unwrap().archived_at = Some("2026-11-02T13:30:00.000Z".to_owned());
    let found = projects::archived_of_project(list.values(), "repo");
    assert_eq!(found.len(), 1, "only the archived one of this project");
    assert_eq!(found[0].id, "a2");
    list.get_mut("a1").unwrap().archived_at = Some("2026-11-02T11:30:00.000Z".to_owned());
    let found = projects::archived_of_project(list.values(), "repo");
    let ids: Vec<&str> = found.iter().map(|a| a.id.as_str()).collect();
    assert_eq!(ids, ["a2", "a1"], "newest first");
    assert!(projects::archived_of_project(list.values(), "nope").is_empty());
}

fn record(id: &str, root: &str) -> ProjectRecord {
    let name = Path::new(root)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    serde_json::from_value(json!({"projectId": id, "rootPath": root, "name": name})).unwrap()
}

fn tempdir(name: &str) -> PathBuf {
    use std::sync::atomic::{AtomicU64, Ordering};
    static N: AtomicU64 = AtomicU64::new(0);
    let d = std::env::temp_dir().join(format!(
        "codecor-test-{}-{}-{name}",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

// ---- Phase D: the intents, the labels in the ui tree ----

#[test]
fn the_intents_are_the_six_of_the_manifest() {
    let names: Vec<&str> = crate::intents::INTENTS.iter().map(|s| s.name).collect();
    assert_eq!(
        names,
        [
            "codecor.projects",
            "codecor.open",
            "codecor.agents",
            "codecor.new_agent",
            "codecor.send",
            "codecor.status"
        ]
    );
    let manifest =
        std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/manifest.app")).unwrap();
    // The manifest parses, its id is the intents' prefix, and it lists exactly these.
    let m = lucina_app::manifest::Manifest::parse(&manifest).unwrap();
    assert_eq!(m.id, "codecor");
    assert_eq!(m.exec, "/usr/bin/lucina-app-coder");
    assert_eq!(m.intents, names);
    for s in crate::intents::INTENTS {
        assert!(s.name.starts_with("codecor."), "{}", s.name);
        assert!(s.inverse.is_none(), "no intent undoes anything: {}", s.name);
    }
    // The arguments check as the listing says they do.
    assert!(
        crate::intents::INTENTS[3]
            .check(&json!({
                "project": "p", "provider": "claude", "prompt": "hi"
            }))
            .is_ok()
    );
    assert!(
        crate::intents::INTENTS[3]
            .check(&json!({"project": "p", "provider": "claude"}))
            .is_err()
    );
    // The Juno shell is an optional boolean.
    assert!(
        crate::intents::INTENTS[3]
            .check(&json!({
                "project": "p", "provider": "claude", "prompt": "hi", "juno": true
            }))
            .is_ok()
    );
    assert!(
        crate::intents::INTENTS[4]
            .check(&json!({"agent": "agt_1", "text": "hi"}))
            .is_ok()
    );
    assert!(
        crate::intents::INTENTS[4]
            .check(&json!({"agent": "agt 1", "text": "hi"}))
            .is_err()
    );
}

#[test]
fn codecor_projects_lists_both_lists_with_names_paths_and_counts() {
    let (mut h, _s) = sample_app();
    let r = h
        .intent("codecor.projects", json!({}))
        .expect("answered at once");
    assert!(r.ok, "{r:?}");
    let buildhost = r.value["buildhost"].as_array().unwrap();
    assert_eq!(buildhost.len(), 3);
    assert_eq!(buildhost[0]["id"], json!("prj-sample"));
    assert_eq!(buildhost[0]["name"], json!("Sample project"));
    assert_eq!(buildhost[0]["path"], json!("/srv/work/sample-project"));
    assert_eq!(buildhost[0]["agents"], json!(3));
    assert_eq!(buildhost[0]["needs_you"], json!(1));
    assert_eq!(
        buildhost[2]["lucina"],
        json!(true),
        "prj-lucina is a Lucina project"
    );
    let juno = r.value["juno"].as_array().unwrap();
    assert_eq!(juno.len(), 2);
    assert_eq!(juno[0]["name"], json!("demo-app"));
    assert_eq!(juno[0]["path"], json!("/data/projects/demo-app"));
    let ids: Vec<&str> = r.entities.iter().map(|e| e.id.as_str()).collect();
    assert!(
        ids.contains(&"prj-sample") && ids.contains(&"/data/projects/demo-app"),
        "{ids:?}"
    );
}

#[test]
fn codecor_open_opens_the_project_screen_by_name_path_basename_or_id() {
    let (mut h, _s) = sample_app();
    for arg in [
        "Sample project",
        "/srv/work/sample-project",
        "sample-project",
        "prj-sample",
    ] {
        let r = h.intent("codecor.open", json!({ "project": arg })).unwrap();
        assert!(r.ok, "{arg}: {r:?}");
        assert_eq!(
            h.app().screen,
            Screen::Project(ProjectKey::Remote("prj-sample".to_owned())),
            "{arg}"
        );
        assert_eq!(r.entities[0].kind, "project");
        assert_eq!(r.entities[0].id, "prj-sample");
        h.app().back();
    }
    // A folder on Juno opens too.
    let r = h
        .intent("codecor.open", json!({ "project": "demo-app" }))
        .unwrap();
    assert!(r.ok, "{r:?}");
    assert_eq!(
        h.app().screen,
        Screen::Project(ProjectKey::Local(PathBuf::from("/data/projects/demo-app")))
    );
    // Unknown: refused, and the screen does not move.
    h.app().back();
    let before = h.app().screen.clone();
    let r = h
        .intent("codecor.open", json!({ "project": "nope" }))
        .unwrap();
    assert!(!r.ok && r.text.contains("no project"), "{r:?}");
    assert_eq!(h.app().screen, before);
}

#[test]
fn codecor_agents_lists_the_project_agents_with_their_fields() {
    let (mut h, _s) = sample_app();
    let r = h
        .intent("codecor.agents", json!({ "project": "prj-sample" }))
        .unwrap();
    assert!(r.ok, "{r:?}");
    let agents = r.value["agents"].as_array().unwrap();
    assert_eq!(agents.len(), 3);
    let first = &agents[0];
    assert_eq!(
        first["id"],
        json!("agt_sample_running"),
        "the coordinator first"
    );
    assert_eq!(first["title"], json!("Upload retry"));
    assert_eq!(first["role"], json!("coordinator"));
    assert_eq!(first["provider"], json!("claude"));
    assert_eq!(first["model"], json!("sample-model-1"));
    assert_eq!(first["state"], json!("Needs permission"));
    assert_eq!(first["branch"], json!("sample-branch"));
    assert_eq!(first["pending"], json!(2), "perm-1 and the diff request");
    let ids: Vec<&str> = agents.iter().map(|a| a["id"].as_str().unwrap()).collect();
    assert!(
        ids.contains(&"agt_sample_failed") && ids.contains(&"agt_sample_done"),
        "{ids:?}"
    );
    assert_eq!(r.entities.len(), 3);
    // An unknown project, and a folder on Juno: refused.
    let r = h
        .intent("codecor.agents", json!({ "project": "nope" }))
        .unwrap();
    assert!(!r.ok && r.text.contains("no project"), "{r:?}");
    let r = h
        .intent("codecor.agents", json!({ "project": "demo-app" }))
        .unwrap();
    assert!(!r.ok && r.text.contains("folder on Juno"), "{r:?}");
}

#[test]
fn codecor_new_agent_creates_the_worktree_then_the_agent() {
    let (mut h, s) = sample_app();
    let r = h
        .intent(
            "codecor.new_agent",
            json!({
                "project": "Sample project",
                "provider": "claude",
                "title": "Fix the login",
                "prompt": "Fix it and run the tests."
            }),
        )
        .expect("answered once the server has the agent");
    assert!(r.ok, "{r:?}");
    assert_eq!(r.value["id"], json!("agt_new_3"));
    assert_eq!(r.entities[0].kind, "agent");
    assert_eq!(r.entities[0].id, "agt_new_3");
    let writes = s.writes();
    assert_eq!(writes.len(), 2, "{writes:?}");
    assert_eq!(writes[0].0, format!("{B}/workspaces"), "the worktree first");
    assert_eq!(writes[0].1["newBranch"], json!("codecor/fix-the-login"));
    assert_eq!(writes[1].0, B);
    assert_eq!(writes[1].1["provider"], json!("claude"));
    assert_eq!(writes[1].1["workspaceId"], json!("wks_sample"));
    assert_eq!(
        writes[1].1["labels"][projects::PROJECT_LABEL],
        json!("sample-project")
    );
    assert_eq!(writes[1].1["labels"][projects::ROLE_LABEL], json!("worker"));
    assert_eq!(writes[1].1["prompt"], json!("Fix it and run the tests."));
    assert!(h.app().store.items.contains_key("agt_new_3"));
    // A coordinator through the intent gets the same preamble as the form.
    let r = h.intent(
        "codecor.new_agent",
        json!({ "project": "prj-other", "provider": "claude", "role": "coordinator", "prompt": "Split the work" }),
    )
    .unwrap();
    assert!(r.ok, "{r:?}");
    let writes = s.writes();
    assert_eq!(writes.len(), 4, "{writes:?}");
    let prompt = writes[3].1["prompt"].as_str().unwrap();
    assert!(
        prompt.contains("You coordinate the CodeCor project other-project."),
        "{prompt}"
    );
    assert!(prompt.ends_with("Split the work"), "{prompt}");
    assert_eq!(
        writes[3].1["labels"][projects::ROLE_LABEL],
        json!("coordinator")
    );
    // The Juno shell: the label goes on the agent.
    let r = h.intent(
        "codecor.new_agent",
        json!({ "project": "prj-other", "provider": "claude", "juno": true, "title": "With the shell", "prompt": "Run it" }),
    )
    .unwrap();
    assert!(r.ok, "{r:?}");
    let writes = s.writes();
    assert_eq!(writes.len(), 6, "{writes:?}");
    assert_eq!(writes[5].1["labels"][new_agent::JUNO_LABEL], json!("bash"));
}

#[test]
fn codecor_new_agent_refuses_codex_on_a_lucina_project_a_second_coordinator_and_an_unknown_project()
{
    let (mut h, s) = sample_app();
    // Codex on a Lucina project: the same sentence as the form's.
    let r = h
        .intent(
            "codecor.new_agent",
            json!({ "project": "LucinaOS", "provider": "codex", "prompt": "hello" }),
        )
        .unwrap();
    assert!(!r.ok, "{r:?}");
    assert_eq!(r.text, new_agent::CODEX_RULE_LINE);
    // A second coordinator.
    let r = h.intent(
        "codecor.new_agent",
        json!({ "project": "prj-sample", "provider": "claude", "role": "coordinator", "prompt": "hello" }),
    )
    .unwrap();
    assert!(
        !r.ok && r.text.contains("already has a coordinator"),
        "{r:?}"
    );
    // The Juno shell off Claude: the same sentence as the form's.
    let r = h.intent(
        "codecor.new_agent",
        json!({ "project": "prj-other", "provider": "mistral", "juno": true, "prompt": "hello" }),
    )
    .unwrap();
    assert!(!r.ok, "{r:?}");
    assert_eq!(r.text, new_agent::JUNO_MODE_LINE);
    // An unknown project.
    let r = h
        .intent(
            "codecor.new_agent",
            json!({ "project": "nope", "provider": "claude", "prompt": "hello" }),
        )
        .unwrap();
    assert!(!r.ok && r.text.contains("no project"), "{r:?}");
    assert!(s.writes().is_empty(), "a refusal writes nothing");
}

#[test]
fn codecor_send_sends_to_the_agent_and_refuses_an_archived_one_and_an_unknown_one() {
    let (mut h, s) = agent_app();
    let r = h
        .intent(
            "codecor.send",
            json!({ "agent": "agt_sample_running", "text": "Push it now" }),
        )
        .unwrap();
    assert!(r.ok, "{r:?}");
    assert_eq!(r.entities[0].kind, "agent");
    assert_eq!(r.entities[0].id, "agt_sample_running");
    let writes = s.writes();
    assert_eq!(writes.len(), 1, "{writes:?}");
    assert_eq!(writes[0].0, format!("{B}/agt_sample_running/send"));
    assert_eq!(writes[0].1["prompt"], json!("Push it now"));
    assert_eq!(
        writes[0].1["activeTurnBehavior"],
        json!("steer"),
        "the agent runs, so the send steers"
    );
    // An archived agent is refused.
    let key = ProjectKey::Remote("prj-sample".to_owned());
    h.app().open_sessions(key.clone());
    settle(&mut h);
    assert!(h.app().store.archived("agt_sample_archived"));
    let r = h
        .intent(
            "codecor.send",
            json!({ "agent": "agt_sample_archived", "text": "hi" }),
        )
        .unwrap();
    assert!(!r.ok && r.text.contains("archived"), "{r:?}");
    // An unknown agent.
    let r = h
        .intent("codecor.send", json!({ "agent": "agt_nope", "text": "hi" }))
        .unwrap();
    assert!(!r.ok && r.text.contains("no agent"), "{r:?}");
    assert_eq!(s.writes().len(), 1, "the refusals wrote nothing");
}

#[test]
fn codecor_status_answers_the_state_the_last_message_preview_and_the_pending_count() {
    let (mut h, _s) = agent_app();
    let r = h
        .intent("codecor.status", json!({ "agent": "agt_sample_running" }))
        .unwrap();
    assert!(r.ok, "{r:?}");
    assert_eq!(r.value["id"], json!("agt_sample_running"));
    assert_eq!(r.value["state"], json!("Needs permission"));
    assert_eq!(r.value["pending"], json!(2));
    assert_eq!(
        r.value["last_message"],
        json!("The retry is in and the tests pass. I want to push the branch next.")
    );
    assert_eq!(r.entities[0].kind, "agent");
    assert_eq!(r.entities[0].title, "Upload retry");
    // An unknown agent.
    let r = h
        .intent("codecor.status", json!({ "agent": "agt_nope" }))
        .unwrap();
    assert!(!r.ok && r.text.contains("no agent"), "{r:?}");
}

#[test]
fn an_intent_that_arrives_before_the_first_list_is_answered_once_the_store_loads() {
    let api: Arc<dyn Api> = sample_api(NOW);
    let mut h = Harness::new(CodeCor::new(Some(api), None).sample(), 1280, 800);
    // As a start-up intent that arrived before the first list answered: the call waits.
    h.app().loaded = false;
    assert!(
        h.intent("codecor.projects", json!({})).is_none(),
        "it waits"
    );
    assert!(h.reply().is_none());
    // The first list answers; the next message moves the store and the waiting intent runs.
    h.app().loaded = true;
    h.app().message(Msg::Changed);
    let r = h.reply().expect("answered after the load");
    assert!(r.ok, "{r:?}");
    assert_eq!(r.value["buildhost"].as_array().unwrap().len(), 3);
}

#[test]
fn every_row_and_button_on_every_screen_has_a_readable_label_in_the_tree() {
    let (mut h, _s) = sample_app();
    let buttons_have_labels = |h: &mut Harness<CodeCor>| {
        h.tree()
            .update
            .nodes
            .iter()
            .map(|(_, n)| n)
            .filter(|n| format!("{:?}", n.role()) == "Button")
            .all(|n| n.label().is_some_and(|l| !l.is_empty()))
    };
    // The Projects screen: both lists' rows.
    let text = tree_text(&mut h);
    for want in [
        "On buildhost",
        "On Juno",
        "Sample project",
        "LucinaOS",
        "demo-app",
        "notes",
    ] {
        assert!(text.contains(want), "{want} in:\n{text}");
    }
    assert!(buttons_have_labels(&mut h), "the Projects screen");
    // The Project screen (remote): the agent rows, the badge, the foot.
    let key = ProjectKey::Remote("prj-sample".to_owned());
    h.app().open_project(key.clone());
    settle(&mut h);
    let text = tree_text(&mut h);
    for want in [
        "Upload retry",
        "Index rebuild",
        "Tidy the docs",
        "coordinator",
        "Enter open · N New agent · C Set coordinator · S Sessions · Esc back",
    ] {
        assert!(text.contains(want), "{want} in:\n{text}");
    }
    assert!(buttons_have_labels(&mut h), "the Project screen");
    // The Project screen (local): the terminal button.
    h.app()
        .open_project(ProjectKey::Local(PathBuf::from("/data/projects/demo-app")));
    settle(&mut h);
    let text = tree_text(&mut h);
    assert!(text.contains("Open in terminal (local Qwen)"), "{text}");
    assert!(buttons_have_labels(&mut h), "the local Project screen");
    // The Agent screen: the pane's buttons and the permission card's.
    h.app().open_project(key.clone());
    settle(&mut h);
    h.app()
        .open_agent(key.clone(), "agt_sample_running".to_owned());
    settle(&mut h);
    let text = tree_text(&mut h);
    for want in ["Upload retry", "Steer", "Stop", "Allow", "Deny"] {
        assert!(text.contains(want), "{want} in:\n{text}");
    }
    assert!(buttons_have_labels(&mut h), "the Agent screen");
    // The Sessions screen: the archived row.
    h.app().open_sessions(key.clone());
    settle(&mut h);
    let text = tree_text(&mut h);
    assert!(text.contains("Old session: the index rebuild"), "{text}");
    assert!(buttons_have_labels(&mut h), "the Sessions screen");
    // The New agent form: every field and button.
    let lucina = ProjectKey::Remote("prj-lucina".to_owned());
    h.app().open_project(lucina.clone());
    settle(&mut h);
    h.app().open_new_agent(lucina, Role::Worker);
    settle(&mut h);
    let text = tree_text(&mut h);
    for want in [
        "New agent",
        "Provider: Claude",
        "Model:",
        "Role:",
        "Create",
        "Cancel",
    ] {
        assert!(text.contains(want), "{want} in:\n{text}");
    }
    assert!(buttons_have_labels(&mut h), "the New agent form");
}

// ---- Phase E: the local Agent screen (`oc-cli acp` in CodeCor's own chat) ----

/// Paint until `cond` holds, pumping the ACP events as the frames do.
fn pump_until(h: &mut Harness<CodeCor>, mut cond: impl FnMut(&CodeCor) -> bool) {
    for _ in 0..150 {
        if cond(h.app()) {
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
        h.core.input(lucina_app::Input::Wheel(0));
        h.core.paint();
    }
    assert!(cond(h.app()), "the condition never came");
}

#[test]
fn enter_on_a_juno_row_opens_the_local_agent_screen_with_the_diff_card() {
    let (mut h, _s) = sample_app();
    h.key(keys::KEY_TAB); // focus the "On Juno" list
    settle(&mut h);
    h.key(keys::KEY_DOWN); // select the first folder
    settle(&mut h);
    h.key(keys::KEY_ENTER);
    settle(&mut h);
    assert_eq!(
        h.app().screen,
        Screen::Local(PathBuf::from("/data/projects/demo-app"))
    );
    assert!(h.app().local_chat.is_some(), "the local chat is open");
    let app = h.app();
    let text = format!("{:?}", app.pane.lines);
    assert!(text.contains("retry(3, send)"), "{text}");
    assert!(
        text.contains("/data/projects/demo-app/src/sample.rs"),
        "{text}"
    );
    let first_diff = app
        .pane
        .lines
        .iter()
        .position(|l| matches!(l, Line::Diff(_)))
        .expect("the card's diff rows");
    let allow = app
        .pane
        .lines
        .iter()
        .position(|l| {
            matches!(
                l,
                Line::Button {
                    act: Act::Answer { allow: true, .. },
                    ..
                }
            )
        })
        .expect("the card's Allow");
    assert!(first_diff < allow, "the diff is above Allow");
    // The header: the folder's name as the title, "Qwen (on Juno)" as the provider.
    let text = tree_text(&mut h);
    assert!(text.contains("demo-app"), "{text}");
    assert!(text.contains("Qwen (on Juno)"), "{text}");
    assert!(text.contains("Allow") && text.contains("Deny"), "{text}");
    assert!(text.contains("Stop"), "a turn is running: {text}");
    // Esc goes back to the Projects screen and closes the local chat.
    h.key(keys::KEY_ESC);
    settle(&mut h);
    assert_eq!(h.app().screen, Screen::Projects);
    assert!(h.app().local_chat.is_none());
}

#[test]
fn t_on_the_local_screen_shows_the_terminal_line_as_the_fallback() {
    let (mut h, _s) = sample_app();
    h.app()
        .open_local_sample(PathBuf::from("/data/projects/demo-app"));
    settle(&mut h);
    h.key(keys::KEY_T);
    settle(&mut h);
    let note = h.app().note.clone().expect("the note with the line");
    assert!(note.starts_with("In the Terminal (F2), type: "), "{note}");
    assert!(
        note.contains("--terminal '/data/projects/demo-app'"),
        "{note}"
    );
    assert!(note.contains("LUCINA_APP_DIR="), "{note}");
}

#[test]
fn the_local_screen_drives_a_whole_turn_over_acp_allow_then_stop() {
    let log = tempdir("acplog").join("stdin.log");
    let acp = crate::acp::tests::fake_agent(&log);
    let (mut h, _s) = sample_app();
    h.app().fixed_now = Some(NOW);
    h.app().open_local_agent(
        PathBuf::from("/data/projects/demo-app"),
        crate::local::LocalAgent::from_acp(acp),
    );
    settle(&mut h);
    let id = "local:/data/projects/demo-app".to_owned();
    // initialize and session/new went out; the session id comes back.
    pump_until(&mut h, |a| {
        a.local_chat
            .as_ref()
            .and_then(|c| c.agent.as_ref())
            .is_some_and(|ag| ag.acp.session_id.is_some())
    });
    assert_eq!(
        h.app()
            .local_chat
            .as_ref()
            .unwrap()
            .agent
            .as_ref()
            .unwrap()
            .acp
            .session_id
            .as_deref(),
        Some("sess_fake")
    );
    // Send through the composer: session/prompt, one turn at a time.
    h.app().pane.composer.set("fix it");
    h.app().local_send();
    assert!(h.app().local_chat.as_ref().unwrap().running);
    let key = format!("{id}/100");
    let pending = |a: &CodeCor| {
        a.store
            .permissions
            .get(&key)
            .is_some_and(|p| p.status == "pending")
    };
    pump_until(&mut h, pending);
    // The card's diff rows are on the screen, above Allow and Deny.
    pump_until(&mut h, |a| {
        a.pane.lines.iter().any(|l| matches!(l, Line::Diff(_)))
    });
    let text = format!("{:?}", h.app().pane.lines);
    assert!(text.contains("/w/a.txt"), "{text}");
    assert!(text.contains("the fix"), "{text}");
    // A second send while the turn runs is refused.
    h.app().pane.composer.set("again");
    h.app().local_send();
    assert_eq!(
        h.app().note.as_deref(),
        Some("One turn at a time — Stop first.")
    );
    // Allow: allow_once for that request id, and the turn ends.
    h.app().local_answer(&key, true);
    pump_until(&mut h, |a| !a.local_chat.as_ref().unwrap().running);
    let text = format!("{:?}", h.app().pane.lines);
    assert!(text.contains("fixed"), "the assistant's answer: {text}");
    assert!(
        text.contains("Allowed"),
        "the card says how it ended: {text}"
    );
    let sent = std::fs::read_to_string(&log).unwrap();
    assert!(
        sent.contains(r#"{"id":100,"jsonrpc":"2.0","result":{"outcome":{"optionId":"allow_once","outcome":"selected"}}}"#),
        "allow_once for the request id: {sent}"
    );
    assert!(sent.contains(r#""method":"session/prompt""#), "{sent}");
    // Stop: session/cancel, and the open request is answered cancelled.
    h.app().pane.composer.set("again");
    h.app().local_send();
    pump_until(&mut h, pending);
    h.app().local_stop();
    // The cancel lines go to the child's stdin; wait until the fake agent has read them.
    let mut sent = String::new();
    for _ in 0..150 {
        sent = std::fs::read_to_string(&log).unwrap();
        if sent.contains("session/cancel") {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
        h.core.input(lucina_app::Input::Wheel(0));
        h.core.paint();
    }
    assert!(
        sent.contains(r#""method":"session/cancel","params":{"sessionId":"sess_fake"}"#),
        "session/cancel: {sent}"
    );
    assert!(
        sent.contains(r#"{"id":100,"jsonrpc":"2.0","result":{"outcome":{"outcome":"cancelled"}}}"#),
        "the open request answered cancelled: {sent}"
    );
}
