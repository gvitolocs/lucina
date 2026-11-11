//! `web/src/state/agents.ts` without the network: the agent list, snapshots, timelines, the permission book, workspaces, terminals
//! and scripts, kept live by the events of `GET /v1/agents/stream`. Pure, so all of it is tested without a screen.

use std::collections::{BTreeMap, HashSet};

use lucina_agents::bucket::{BucketInput, state_bucket};
use lucina_agents::timeline::TimelineRow;
use lucina_agents::types::{AgentListItem, AgentSnapshot};
use serde_json::Value;

use crate::model::{PermissionView, Script, Terminal, Workspace, is_internal};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TlStatus {
    Loading,
    Ready,
    Error,
}

#[derive(Clone, Debug)]
pub struct Timeline {
    pub epoch: String,
    pub rows: Vec<TimelineRow>,
    pub status: TlStatus,
    pub error: Option<String>,
}

/// What an event asks for beyond the state it changed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Effect {
    None,
    /// `stream.reset`: everything again.
    RefreshAll,
    /// A gap or a new epoch in this agent's timeline.
    LoadTimeline(String),
}

#[derive(Default)]
pub struct Store {
    pub items: BTreeMap<String, AgentListItem>,
    pub snapshots: BTreeMap<String, AgentSnapshot>,
    pub timelines: BTreeMap<String, Timeline>,
    pub permissions: BTreeMap<String, PermissionView>,
    pub workspaces: BTreeMap<String, Workspace>,
    pub terminals: BTreeMap<String, Terminal>,
    pub scripts: BTreeMap<String, Vec<Script>>,
    /// Events that came while a timeline was loading: it reads again.
    pub dirty: HashSet<String>,
}

/// `itemFromSnapshot`.
pub fn item_from_snapshot(s: &AgentSnapshot) -> AgentListItem {
    AgentListItem {
        id: s.id.clone(),
        short_id: s.id.chars().take(7).collect(),
        title: s.title.clone(),
        provider: s.provider.clone(),
        model: s.model.clone(),
        thinking_option_id: s.thinking_option_id.clone(),
        status: s.status,
        cwd: s.cwd.clone(),
        created_at: s.created_at.clone(),
        updated_at: s.updated_at.clone(),
        last_user_message_at: s.last_user_message_at.clone(),
        archived_at: s.archived_at.clone(),
        requires_attention: s.requires_attention,
        attention_reason: s.attention_reason,
        attention_timestamp: s.attention_timestamp.clone(),
        labels: s.labels.clone(),
        provider_unavailable: s.provider_unavailable,
        bucket: state_bucket(BucketInput {
            status: s.status,
            pending_permission_count: s.pending_permissions.len(),
            requires_attention: s.requires_attention.unwrap_or(false),
            attention_reason: s.attention_reason,
        }),
        external: s.external,
        owner_device_id: s.owner_device_id.clone(),
        host: s.host.clone(),
    }
}

/// `mergeRows`: only rows past the last one, sorted.
pub fn merge_rows(have: &mut Vec<TimelineRow>, more: Vec<TimelineRow>) {
    let last = have.last().map_or(0, |r| r.seq);
    let mut fresh: Vec<TimelineRow> = more.into_iter().filter(|r| have.is_empty() || r.seq > last).collect();
    fresh.sort_by_key(|r| r.seq);
    have.append(&mut fresh);
}

impl Store {
    pub fn apply_snapshot(&mut self, s: AgentSnapshot) {
        self.items.insert(s.id.clone(), item_from_snapshot(&s));
        self.snapshots.insert(s.id.clone(), s);
    }

    /// `loadItems`: the active list; archived ones stay known.
    pub fn set_items(&mut self, list: Vec<AgentListItem>) {
        let seen: HashSet<&str> = list.iter().map(|a| a.id.as_str()).collect();
        self.items.retain(|id, a| a.archived_at.is_some() || seen.contains(id.as_str()));
        for a in list {
            self.items.insert(a.id.clone(), a);
        }
    }

    /// `applyEvent`. `agent_command` frames, `terminal_attention` and anything new are ignored.
    pub fn apply_event(&mut self, ev: &Value) -> Effect {
        let of = |k: &str| ev.get(k).cloned().unwrap_or(Value::Null);
        match ev["type"].as_str().unwrap_or("") {
            "stream.reset" => return Effect::RefreshAll,
            "agent_state" => {
                if let Ok(s) = serde_json::from_value::<AgentSnapshot>(of("agent")) {
                    self.apply_snapshot(s);
                }
            }
            "agent_deleted" => {
                let id = ev["agentId"].as_str().unwrap_or("");
                self.items.remove(id);
                self.snapshots.remove(id);
                self.timelines.remove(id);
            }
            "permission" => {
                if let Ok(p) = serde_json::from_value::<PermissionView>(of("permission")) {
                    self.permissions.insert(p.key(), p);
                }
            }
            "workspace" => {
                if let Ok(w) = serde_json::from_value::<Workspace>(of("workspace")) {
                    self.workspaces.insert(w.workspace_id.clone(), w);
                }
            }
            "terminal" => {
                if let Ok(t) = serde_json::from_value::<Terminal>(of("terminal")) {
                    self.terminals.insert(t.id.clone(), t);
                }
            }
            "terminal_removed" => {
                self.terminals.remove(ev["terminalId"].as_str().unwrap_or(""));
            }
            "script_status" => {
                if let (Some(w), Ok(list)) = (ev["workspaceId"].as_str(), serde_json::from_value::<Vec<Script>>(of("scripts"))) {
                    self.scripts.insert(w.to_owned(), list);
                }
            }
            "agent_stream" => return self.apply_stream(ev),
            _ => {}
        }
        Effect::None
    }

    /// `applyAgentStream`: a row in order joins the timeline; a gap or a new epoch reads it again.
    fn apply_stream(&mut self, ev: &Value) -> Effect {
        let id = ev["agentId"].as_str().unwrap_or("").to_owned();
        let Some(tl) = self.timelines.get_mut(&id) else { return Effect::None };
        let (Some(seq), "timeline") = (ev["seq"].as_u64(), ev["event"]["type"].as_str().unwrap_or("")) else { return Effect::None };
        let Ok(item) = serde_json::from_value(ev["event"]["item"].clone()) else { return Effect::None };
        if tl.status == TlStatus::Loading {
            self.dirty.insert(id);
            return Effect::None;
        }
        let epoch = ev["epoch"].as_str().unwrap_or("");
        if !tl.epoch.is_empty() && epoch != tl.epoch {
            return Effect::LoadTimeline(id);
        }
        let last = tl.rows.last().map_or(0, |r| r.seq);
        if seq <= last {
            return Effect::None;
        }
        if last > 0 && seq > last + 1 {
            return Effect::LoadTimeline(id);
        }
        tl.rows.push(TimelineRow { seq, timestamp: ev["timestamp"].as_str().unwrap_or("").to_owned(), item, turn_id: ev["event"]["turnId"].as_str().map(str::to_owned) });
        Effect::None
    }

    /// `agentList`: needs input first, then failed, running, attention, done; newest first inside a group.
    pub fn agent_list(&self, archived: bool) -> Vec<&AgentListItem> {
        let mut v: Vec<&AgentListItem> = self.items.values().filter(|a| a.archived_at.is_some() == archived && !is_internal(&a.labels)).collect();
        v.sort_by(|a, b| a.bucket.priority().cmp(&b.bucket.priority()).then_with(|| b.updated_at.cmp(&a.updated_at)));
        v
    }

    /// `pendingPermissions`.
    pub fn pending_permissions(&self) -> Vec<&PermissionView> {
        let mut v: Vec<&PermissionView> = self.permissions.values().filter(|p| p.status == "pending").collect();
        v.sort_by(|a, b| a.created_at.cmp(&b.created_at));
        v
    }

    /// `workspaceOf`.
    pub fn workspace_of(&self, agent: &str) -> Option<&Workspace> {
        let snap = self.snapshots.get(agent);
        if let Some(w) = snap.and_then(|s| s.workspace_id.as_ref()).and_then(|id| self.workspaces.get(id)) {
            return Some(w);
        }
        let cwd = snap.map(|s| s.cwd.as_str()).or_else(|| self.items.get(agent).map(|a| a.cwd.as_str()))?;
        self.workspaces.values().find(|w| w.cwd == cwd)
    }

    pub fn archived(&self, agent: &str) -> bool {
        self.snapshots.get(agent).map_or_else(|| self.items.get(agent).is_some_and(|a| a.archived_at.is_some()), |s| s.archived_at.is_some())
    }

    pub fn running(&self, agent: &str) -> bool {
        use lucina_agents::types::AgentStatus::Running;
        self.snapshots.get(agent).map(|s| s.status).or_else(|| self.items.get(agent).map(|a| a.status)) == Some(Running)
    }
}
