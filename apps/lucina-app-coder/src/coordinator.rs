//! The coordinator's first prompt: a fixed preamble, constant in code, that names the CodeCor project and lists its
//! workers, followed by the owner's text. The preamble says what a coordinator may do (children in their own workspaces
//! through the lagent/agentd tools, message them, wait for them, merges with `lagent merge`) and what it never does
//! (answer permissions; go-aheads go to Lucina with `lucina_decide`).

/// The fixed preamble of a coordinator's first prompt. `{project}` is the repo's basename and `{workers}` the worker
/// list, filled in by [`preamble`].
pub const COORDINATOR_PREAMBLE: &str = "\
You coordinate the CodeCor project {project}.

The project's worker agents are:
{workers}

You may create child agents in their own workspaces through your lagent/agentd tools, message them, wait for them, and ask for merges with `lagent merge`. You never answer permissions; go-aheads go to Lucina with lucina_decide.";

/// One worker of the project, as the preamble lists it: by id and title.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorkerRef {
    pub id: String,
    pub title: String,
}

/// The preamble with the project's name and its workers' ids and titles in it.
pub fn preamble(project: &str, workers: &[WorkerRef]) -> String {
    let list = if workers.is_empty() {
        "- (none yet)".to_owned()
    } else {
        workers
            .iter()
            .map(|w| format!("- {} — {}", w.id, w.title))
            .collect::<Vec<_>>()
            .join("\n")
    };
    COORDINATOR_PREAMBLE
        .replace("{project}", project)
        .replace("{workers}", &list)
}

/// A coordinator's first prompt: the preamble, then the owner's text.
pub fn prompt(project: &str, workers: &[WorkerRef], owner_text: &str) -> String {
    let mut p = preamble(project, workers);
    let t = owner_text.trim();
    if !t.is_empty() {
        p.push_str("\n\n");
        p.push_str(t);
    }
    p
}
