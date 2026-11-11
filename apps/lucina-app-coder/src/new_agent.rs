//! The **New agent** form of the Project screen, and the pure helpers behind it: the worktree branch name made from the
//! title, the two create requests (the workspace first, then the agent with that `workspaceId` and the CodeCor labels),
//! the Codex rule that greys Codex out on a Lucina project, and the server's refusal message shown as is. The words and
//! the request shapes are the Agents app's "new run" form's (`apps/lucina-app-agents/src/app.rs`).

use lucina_account::ApiError;
use lucina_agents_client::api::B;
use lucina_agents_client::model::{Choice, ProviderInfo, Workspace};
use lucina_app::TextEdit;
use serde_json::{Value, json};

use crate::projects;

/// The line shown under the provider choice when the project is a Lucina project: Codex is not used there (owner rule).
pub const CODEX_RULE_LINE: &str =
    "Codex is not used on Lucina projects (owner rule). Pick Claude, Mistral or Qwen.";

/// The label that gives a Claude agent Juno's shell: every shell call asks the owner first.
pub const JUNO_LABEL: &str = "codecor.juno";
/// The shell the owner approves, call by call.
pub const JUNO_SHELL: &str = "bash";
/// The modes the Juno shell works with: the ones that ask before each call.
pub const JUNO_MODES: &[&str] = &["default", "plan", "acceptEdits"];
/// The refusal when the Juno shell is asked with a mode that does not ask.
pub const JUNO_MODE_LINE: &str = "The Juno shell needs a mode that asks before each call.";

/// Whether the chosen mode works with the Juno shell: no mode (the provider's own default) or one of [`JUNO_MODES`].
pub fn juno_mode_ok(mode: Option<&str>) -> bool {
    match mode {
        None => true,
        Some(m) => JUNO_MODES.contains(&m),
    }
}

/// The branch prefix of every CodeCor worktree.
pub const BRANCH_PREFIX: &str = "codecor/";
/// The longest part of the branch name that is made from the title.
pub const MAX_SLUG: usize = 40;

/// The agent's role in the project: `codecor.role`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role {
    Worker,
    Coordinator,
}

impl Role {
    pub fn as_str(&self) -> &'static str {
        match self {
            Role::Worker => "worker",
            Role::Coordinator => "coordinator",
        }
    }

    pub fn label(&self) -> &'static str {
        match self {
            Role::Worker => "worker",
            Role::Coordinator => "coordinator",
        }
    }
}

/// The **New agent** form, opened from the Project screen with N (worker) or C (coordinator).
pub struct NewAgentForm {
    /// agentd's `projectId`, for the workspace request.
    pub project_id: String,
    /// A Lucina project: Codex is greyed out (owner rule).
    pub lucina: bool,
    /// Whether the owner may pick the coordinator role: only when the project has none.
    pub can_coordinator: bool,
    pub role: Role,
    pub providers: Option<Vec<ProviderInfo>>,
    pub provider: usize,
    pub model: Option<usize>,
    pub mode: Option<usize>,
    /// Juno's shell for a Claude agent: every shell call asks the owner first. Shown only while Claude is chosen.
    pub juno: bool,
    pub title: TextEdit,
    pub prompt: TextEdit,
    pub busy: bool,
    /// The server's message when a create was refused; the form keeps what was typed.
    pub problem: Option<String>,
    focus: bool,
}

impl NewAgentForm {
    pub fn new(
        project_id: String,
        lucina: bool,
        can_coordinator: bool,
        role: Role,
    ) -> NewAgentForm {
        NewAgentForm {
            project_id,
            lucina,
            can_coordinator,
            role,
            providers: None,
            provider: 0,
            model: None,
            mode: None,
            juno: false,
            title: TextEdit::new().placeholder("Title"),
            prompt: TextEdit::multiline().placeholder("What should the agent do?"),
            busy: false,
            problem: None,
            focus: true,
        }
    }

    /// The chosen provider, when the list has arrived and the choice is on it.
    pub fn provider_info(&self) -> Option<&ProviderInfo> {
        self.providers.as_ref()?.get(self.provider)
    }

    /// The Codex rule line, when this project is a Lucina project and the server lists Codex: Codex is greyed out and
    /// cannot be chosen.
    pub fn codex_line(&self) -> Option<&'static str> {
        codex_blocked(self.lucina, self.providers.as_deref().unwrap_or(&[]))
    }

    /// The next choosable provider after `current`: Codex is skipped on a Lucina project.
    pub fn cycle_provider(&mut self) {
        if let Some(list) = &self.providers {
            self.provider = next_provider(list, self.provider, self.lucina);
            self.model = None;
            self.mode = None;
            self.juno = false;
        }
    }

    /// The Juno shell row is visible only while a Claude agent is chosen.
    pub fn juno_shown(&self) -> bool {
        self.provider_info().is_some_and(|p| p.id == "claude")
    }

    /// The toggle of that row: it works only while a Claude agent is chosen.
    pub fn toggle_juno(&mut self) {
        if self.juno_shown() {
            self.juno = !self.juno;
        }
    }

    /// The refusal under the form when the Juno shell is on but the chosen mode does not ask before each call.
    pub fn juno_problem(&self) -> Option<&'static str> {
        let mode = self
            .mode
            .and_then(|i| self.provider_info().and_then(|p| p.modes.get(i)))
            .map(|m| m.id.as_str());
        if self.juno && !juno_mode_ok(mode) {
            Some(JUNO_MODE_LINE)
        } else {
            None
        }
    }

    /// The models the chosen provider lists as selectable.
    pub fn models(&self) -> Vec<&Choice> {
        self.provider_info()
            .map_or_else(Vec::new, selectable_models)
    }

    /// The modes the chosen provider lists.
    pub fn modes(&self) -> Vec<&Choice> {
        self.provider_info()
            .map_or_else(Vec::new, |p| p.modes.iter().collect())
    }

    pub fn cycle_model(&mut self) {
        let n = self.models().len();
        if n == 0 {
            self.model = None;
            return;
        }
        self.model = match self.model {
            None => Some(0),
            Some(i) if i + 1 < n => Some(i + 1),
            _ => None,
        };
    }

    pub fn cycle_mode(&mut self) {
        let n = self.modes().len();
        if n == 0 {
            self.mode = None;
            return;
        }
        self.mode = match self.mode {
            None => Some(0),
            Some(i) if i + 1 < n => Some(i + 1),
            _ => None,
        };
    }

    pub fn cycle_role(&mut self) {
        if self.can_coordinator {
            self.role = match self.role {
                Role::Worker => Role::Coordinator,
                Role::Coordinator => Role::Worker,
            };
        }
    }

    /// Take the focus once, for the first field.
    pub fn take_focus(&mut self) -> bool {
        std::mem::take(&mut self.focus)
    }
}

/// Whether Codex is greyed out: on a Lucina project, when the server lists it. The line to show, else `None`.
pub fn codex_blocked(lucina: bool, providers: &[ProviderInfo]) -> Option<&'static str> {
    if lucina && providers.iter().any(|p| p.id == "codex") {
        Some(CODEX_RULE_LINE)
    } else {
        None
    }
}

/// The first choosable provider: Codex is skipped on a Lucina project.
pub fn initial_provider(providers: &[ProviderInfo], lucina: bool) -> usize {
    providers
        .iter()
        .position(|p| !(lucina && p.id == "codex"))
        .unwrap_or(0)
}

/// The provider after `current`, wrapping around and skipping Codex on a Lucina project.
pub fn next_provider(providers: &[ProviderInfo], current: usize, lucina: bool) -> usize {
    let n = providers.len();
    if n == 0 {
        return 0;
    }
    for step in 1..=n {
        let i = (current + step) % n;
        if !(lucina && providers[i].id == "codex") {
            return i;
        }
    }
    current
}

/// The models a provider lists as selectable.
pub fn selectable_models(p: &ProviderInfo) -> Vec<&Choice> {
    p.models
        .iter()
        .filter(|m| m.is_selectable != Some(false))
        .collect()
}

/// The worktree branch name made from the title: lower-case, `[a-z0-9-]` (every other run becomes one `-`), at most
/// [`MAX_SLUG`] characters, with the `codecor/` prefix. An empty slug falls back to `agent`.
pub fn branch_name(title: &str) -> String {
    let mut slug = String::new();
    let mut gap = false;
    for c in title.to_lowercase().chars() {
        if c.is_ascii_lowercase() || c.is_ascii_digit() {
            if gap && !slug.is_empty() {
                slug.push('-');
            }
            slug.push(c);
            gap = false;
        } else {
            gap = true;
        }
    }
    if slug.len() > MAX_SLUG {
        let cut = &slug[..MAX_SLUG];
        slug = match cut.rfind('-') {
            Some(i) if i > MAX_SLUG / 2 => cut[..i].to_owned(),
            _ => cut.trim_end_matches('-').to_owned(),
        };
    }
    if slug.is_empty() {
        slug.push_str("agent");
    }
    format!("{BRANCH_PREFIX}{slug}")
}

/// `POST /v1/agents/workspaces`: an isolation worktree in the project, branching off `main`, the branch named after
/// the title (the shape the Agents app's "new run" form posts, plus the branch CodeCor names itself).
pub fn workspace_request(project_id: &str, title: &str, prompt: &str, provider: &str) -> Value {
    let mut w = json!({
        "isolation": "worktree",
        "projectId": project_id,
        "mode": "branch-off",
        "newBranch": branch_name(title),
        "base": "main",
        "provider": provider,
    });
    let title = title.trim();
    if !title.is_empty() {
        w["title"] = title.into();
    }
    let prompt = prompt.trim();
    if !prompt.is_empty() {
        w["prompt"] = prompt.into();
    }
    w
}

/// `POST /v1/agents`: the agent in the worktree just created, with the CodeCor labels.
pub fn agent_request(
    ws: &Workspace,
    provider: &str,
    model: Option<String>,
    mode: Option<String>,
    title: &str,
    prompt: &str,
    repo: &str,
    role: Role,
    juno: bool,
) -> Value {
    let mut labels = json!({
        projects::PROJECT_LABEL: repo,
        projects::ROLE_LABEL: role.as_str(),
    });
    if juno {
        labels[JUNO_LABEL] = Value::from(JUNO_SHELL);
    }
    let mut b = json!({
        "provider": provider,
        "cwd": ws.cwd,
        "workspaceId": ws.workspace_id,
        "labels": labels,
    });
    let title = title.trim();
    if !title.is_empty() {
        b["title"] = title.into();
    }
    let prompt = prompt.trim();
    if !prompt.is_empty() {
        b["prompt"] = prompt.into();
    }
    if let Some(m) = model {
        b["model"] = m.into();
    }
    if let Some(m) = mode {
        b["modeId"] = m.into();
    }
    b
}

/// What to show under the form when the server refused a create: its message, as is (a `CODEX_NOT_ALLOWED` answer
/// included).
pub fn server_message(e: &ApiError) -> String {
    match e {
        ApiError::Http(_, _, m) if !m.is_empty() => m.clone(),
        other => other.to_string(),
    }
}

/// The path the agent create posts to.
pub fn agents_path() -> String {
    B.to_owned()
}

/// The path the worktree create posts to.
pub fn workspaces_path() -> String {
    format!("{B}/workspaces")
}
