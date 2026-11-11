//! The Agent screen's pane, as the Agents app's detail view is (`apps/lucina-app-agents/src/app.rs`, `view_detail`,
//! `view_chat`, `view_diff`, `rebuild`): tabs Chat and Changes, the projected timeline with tool calls and thinking one
//! Enter away, permission cards with their diff above Allow/Deny, the composer with Send/Steer/Stop, and the worktree's
//! diff in three modes. The app owns the requests (one worker thread, the stream with its back-off) and calls
//! [`AgentPane::rebuild`] and [`AgentPane::view`]; what the pane wants done comes back as a [`PaneAct`].

use std::collections::{BTreeMap, HashSet};
use std::time::{Duration, Instant};

use lucina_app::{Face, ListState, Rect, Rgb, TextEdit, Ui};

use crate::model::{DiffResult, DiffRow, base_name, diff_rows};
use crate::pages::{self, Act, Line};
use crate::store::Store;
use crate::ui::{draw, height};

/// Streamed text arrives in many small pieces: the screen follows at this pace (the Agents app's `STREAM_PACE`).
pub const STREAM_PACE: Duration = Duration::from_millis(100);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tab {
    Chat,
    Changes,
}

/// The Changes tab's state: the three diff modes, the file list and the numbered rows of the chosen file.
#[derive(Default)]
pub struct DiffPane {
    pub mode: usize,
    pub ignore_ws: bool,
    pub result: Option<DiffResult>,
    pub error: Option<String>,
    pub loading: bool,
    pub selected: usize,
    pub rows: Vec<DiffRow>,
    pub files_list: ListState,
    pub rows_list: ListState,
}

/// What the pane wants the app to do: answer a permission, send or stop, read the diff again.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PaneAct {
    None,
    Send,
    Stop,
    Answer { key: String, allow: bool },
    LoadDiff,
}

/// The Agent screen: one agent's Chat and Changes.
pub struct AgentPane {
    pub tab: Tab,
    pub lines: Vec<Line>,
    pub main: ListState,
    pub expanded: HashSet<u64>,
    /// The permission keys already shown, so an answered one stays, saying how it ended.
    pub seen: Vec<String>,
    /// What ended a card elsewhere, if anything.
    pub closed: BTreeMap<String, String>,
    /// The permission keys being answered right now.
    pub busy: HashSet<String>,
    pub composer: TextEdit,
    pub composer_busy: bool,
    pub follow: bool,
    follow_selected: Option<usize>,
    last_scroll: i32,
    last_h: i32,
    pub stale: bool,
    pub soft: bool,
    pub built_at: Option<Instant>,
    pub pace: Duration,
    pub diff: DiffPane,
    pub focus: Option<&'static str>,
}

impl AgentPane {
    pub fn new() -> AgentPane {
        AgentPane {
            tab: Tab::Chat,
            lines: Vec::new(),
            main: ListState::new(),
            expanded: HashSet::new(),
            seen: Vec::new(),
            closed: BTreeMap::new(),
            busy: HashSet::new(),
            composer: TextEdit::new(),
            composer_busy: false,
            follow: true,
            follow_selected: None,
            last_scroll: 0,
            last_h: 0,
            stale: true,
            soft: false,
            built_at: None,
            pace: STREAM_PACE,
            diff: DiffPane::default(),
            focus: Some("main"),
        }
    }

    /// Opening an agent starts over: Chat, following, nothing expanded, an empty composer.
    pub fn open(&mut self) {
        self.tab = Tab::Chat;
        self.lines.clear();
        self.main = ListState::new();
        self.expanded.clear();
        self.seen.clear();
        self.closed.clear();
        self.busy.clear();
        self.composer.clear();
        self.composer_busy = false;
        self.follow = true;
        self.follow_selected = None;
        self.last_scroll = 0;
        self.last_h = 0;
        self.stale = true;
        self.soft = false;
        self.built_at = None;
        self.diff = DiffPane::default();
        self.focus = Some("main");
    }

    pub fn mark_stale(&mut self) {
        self.stale = true;
    }

    pub fn mark_soft(&mut self) {
        self.soft = true;
    }

    pub fn tabs(&self, has_workspace: bool) -> Vec<(Tab, &'static str)> {
        let mut out = vec![(Tab::Chat, "Chat")];
        if has_workspace {
            out.push((Tab::Changes, "Changes"));
        }
        out
    }

    /// Switch the tab; `true` when the Changes tab was chosen and its diff should be read.
    pub fn switch_tab(&mut self, tab: Tab) -> bool {
        self.tab = tab;
        self.main = ListState::new();
        self.stale = true;
        self.focus = Some(if matches!(tab, Tab::Changes) {
            "diff-files"
        } else {
            "main"
        });
        matches!(tab, Tab::Changes)
    }

    pub fn cycle_tab(&mut self, step: i32, has_workspace: bool) {
        let tabs = self.tabs(has_workspace);
        let at = tabs.iter().position(|(t, _)| *t == self.tab).unwrap_or(0) as i32;
        let next = (at + step).rem_euclid(tabs.len() as i32) as usize;
        self.switch_tab(tabs[next].0);
    }

    /// The chat's lines again from the store: the projected timeline (capped at `pages::CHAT_WINDOW`), the permission
    /// cards, the finished-run gate. Following keeps the newest line selected, and never an Allow or Deny button.
    pub fn rebuild(&mut self, store: &Store, id: &str, now: i64) {
        self.stale = false;
        self.soft = false;
        self.built_at = Some(Instant::now());
        let lines = pages::chat(
            store,
            id,
            &self.expanded,
            &self.seen,
            &self.closed,
            &self.busy,
            now,
        );
        let grew = lines.len() != self.lines.len() || lines != self.lines;
        self.lines = lines;
        if grew {
            self.main.invalidate();
            if self.follow && self.tab == Tab::Chat {
                self.main.scroll_to_end();
            }
        }
        if let Some(i) = self.main.selected {
            if i >= self.lines.len() {
                self.main.selected = self.lines.len().checked_sub(1);
            }
        }
        if self.tab == Tab::Chat && self.follow {
            // An Enter that was meant for the list must not answer anything.
            self.main.selected = self
                .lines
                .iter()
                .rposition(|l| !matches!(l, Line::Button { .. }));
            self.follow_selected = self.main.selected;
        } else if self.main.selected.is_none() && !self.lines.is_empty() {
            let first = self
                .lines
                .iter()
                .position(|l| matches!(l, Line::Row { act, .. } if *act != Act::None))
                .unwrap_or(0);
            self.main.select(first);
        }
    }

    /// Draw the tabs and the tab's content; what the pane wants done comes back.
    pub fn view(
        &mut self,
        ui: &mut Ui,
        store: &Store,
        id: &str,
        archived: bool,
        running: bool,
    ) -> PaneAct {
        let mut tab = None;
        let tabs = self.tabs(store.workspace_of(id).is_some());
        if tabs.len() > 1 {
            ui.row(30.0, |ui| {
                for (i, (t, label)) in tabs.iter().enumerate() {
                    let text = if self.tab == *t {
                        format!("› {label}")
                    } else {
                        (*label).to_owned()
                    };
                    if ui.button_id(("tab", i), &text) {
                        tab = Some(*t);
                    }
                }
            });
        }
        if let Some(t) = tab {
            if self.switch_tab(t) {
                return PaneAct::LoadDiff;
            }
        }
        match self.tab {
            Tab::Chat => self.view_chat(ui, archived, running),
            Tab::Changes => self.view_diff(ui),
        }
    }

    /// The page list every line-based page shares.
    fn view_lines(&mut self, ui: &mut Ui) -> Option<Act> {
        let t = ui.theme();
        let r = ui.area();
        if self.focus == Some("main") && !self.lines.is_empty() {
            ui.focus("main");
            self.focus = None;
        }
        let lines = &self.lines;
        let res = ui.list_measured(
            "main",
            &mut self.main,
            r,
            lines.len(),
            |text, i, w| height(text, &lines[i], w, &t),
            |ui, i, selected| draw(ui, &lines[i], selected),
        );
        res.activated.and_then(|i| lines[i].act().cloned())
    }

    fn view_chat(&mut self, ui: &mut Ui, archived: bool, running: bool) -> PaneAct {
        let t = ui.theme();
        let compose = ui.cut_bottom(t.px(42.0));
        let r = ui.area();
        let act = self.view_lines(ui);
        // Scrolling up stops following; a send, End or reaching the end starts it again.
        if self.main.scroll < self.last_scroll && r.h == self.last_h && self.main.scroll != 0 {
            self.follow = false;
        }
        self.last_scroll = self.main.scroll;
        self.last_h = r.h;
        if self.follow && self.main.selected != self.follow_selected {
            self.follow = false;
        }
        let mut out = PaneAct::None;
        if let Some(a) = act {
            match a {
                Act::Toggle(seq) => {
                    if !self.expanded.remove(&seq) {
                        self.expanded.insert(seq);
                    }
                    self.follow = false;
                    self.stale = true;
                }
                Act::Answer { key, allow } => out = PaneAct::Answer { key, allow },
                _ => {}
            }
        }
        let mut send = false;
        let mut stop = false;
        ui.within(compose, |ui| {
            let bw = t.px(74.0);
            let rx = compose.right() - if running { 2 * bw + t.px(6.0) } else { bw };
            self.composer.placeholder = if archived {
                "Archived — sending a message resumes it".to_owned()
            } else if running {
                "Steer the agent while it works".to_owned()
            } else {
                "Message the agent".to_owned()
            };
            let res = ui.text_field_at(
                Rect::new(
                    compose.x,
                    compose.y + t.px(6.0),
                    rx - compose.x - t.px(6.0),
                    t.px(32.0),
                ),
                "composer",
                &mut self.composer,
            );
            if self.focus == Some("composer") {
                ui.focus("composer");
                self.focus = None;
            }
            if res.submitted {
                send = true;
            }
            if ui.button_at(
                Rect::new(rx, compose.y + t.px(6.0), bw, t.px(32.0)),
                "send",
                if running { "Steer" } else { "Send" },
            ) {
                send = true;
            }
            if running
                && ui.button_at(
                    Rect::new(rx + bw + t.px(6.0), compose.y + t.px(6.0), bw, t.px(32.0)),
                    "stop",
                    "Stop",
                )
            {
                stop = true;
            }
        });
        if send {
            out = PaneAct::Send;
        }
        if stop {
            out = PaneAct::Stop;
        }
        out
    }

    pub fn pick_diff_file(&mut self, i: usize) {
        if let Some(f) = self.diff.result.as_ref().and_then(|d| d.files.get(i)) {
            self.diff.selected = i;
            self.diff.rows = diff_rows(f);
            self.diff.rows_list = ListState::new();
        }
    }

    fn view_diff(&mut self, ui: &mut Ui) -> PaneAct {
        const MODES: [&str; 3] = ["All changes", "Uncommitted", "Since base"];
        let t = ui.theme();
        let mut mode = None;
        let mut reload = false;
        let totals = self.diff.result.as_ref().map(|d| {
            (
                d.files.len(),
                d.files.iter().map(|f| f.additions).sum::<u64>(),
                d.files.iter().map(|f| f.deletions).sum::<u64>(),
            )
        });
        ui.row(30.0, |ui| {
            for (i, m) in MODES.iter().enumerate() {
                let text = if self.diff.mode == i {
                    format!("› {m}")
                } else {
                    (*m).to_owned()
                };
                if ui.button_id(("diff-mode", i), &text) {
                    mode = Some(i);
                }
            }
            if ui.checkbox("Ignore whitespace", &mut self.diff.ignore_ws) {
                reload = true;
            }
            if ui.button("Refresh") {
                reload = true;
            }
        });
        if let Some((n, add, del)) = totals.filter(|x| x.0 > 0) {
            ui.dim(&format!("{n} files +{add} −{del}"));
        }
        let mut pick = None;
        let (d, loading, error) = (
            self.diff.result.as_ref(),
            self.diff.loading,
            self.diff.error.as_deref(),
        );
        if let Some(e) = error {
            ui.wrapped(Face::Sans, t.pt(13.0), t.danger, e);
        }
        if let Some(e) = d.and_then(|d| d.error.as_deref()) {
            ui.wrapped(Face::Sans, t.pt(13.0), t.danger, e);
        }
        if d.is_some_and(|d| d.diff_too_large) {
            ui.wrapped(
                Face::Sans,
                t.pt(13.0),
                t.accent,
                "The diff is too large; some files are left out.",
            );
        }
        match d.filter(|d| !d.files.is_empty()) {
            None => {
                if loading {
                    ui.dim("Loading…");
                } else if d.is_some() {
                    ui.dim("No changes.");
                }
            }
            Some(d) => {
                let left = ui.cut_left(t.px(200.0));
                let files = &d.files;
                if self.focus == Some("diff-files") {
                    ui.focus("diff-files");
                    self.focus = None;
                }
                let selected = self.diff.selected;
                let res = ui.list(
                    "diff-files",
                    &mut self.diff.files_list,
                    left,
                    files.len(),
                    42.0,
                    |ui, i, sel| {
                        let row = ui.area();
                        if sel || i == selected {
                            ui.fill_round(
                                Rect::new(row.x, row.y + 1, row.w, row.h - 2),
                                t.px(5.0),
                                t.selection,
                            );
                        }
                        let f = &files[i];
                        ui.text_in(
                            Rect::new(
                                row.x + t.px(6.0),
                                row.y + t.px(2.0),
                                row.w - t.px(10.0),
                                t.px(20.0),
                            ),
                            Face::Sans,
                            t.pt(13.0),
                            t.text,
                            base_name(&f.path),
                        );
                        let state = if f.is_new {
                            " new"
                        } else if f.is_deleted {
                            " deleted"
                        } else {
                            ""
                        };
                        ui.text_in(
                            Rect::new(
                                row.x + t.px(6.0),
                                row.y + t.px(21.0),
                                row.w - t.px(10.0),
                                t.px(18.0),
                            ),
                            Face::Sans,
                            t.pt(11.0),
                            t.dim,
                            &format!("+{} −{}{state}", f.additions, f.deletions),
                        );
                    },
                );
                if res.selection_changed {
                    pick = self.diff.files_list.selected;
                }
                if let Some(i) = res.activated {
                    pick = Some(i);
                }
                let cur = files.get(selected);
                let head = ui.cut_top(t.px(22.0));
                if let Some(f) = cur {
                    let mut title = f.path.clone();
                    if let Some(o) = f.old_path.as_ref().filter(|o| **o != f.path) {
                        title += &format!(" ← {o}");
                    }
                    ui.text_in(
                        Rect::new(head.x + t.px(8.0), head.y, head.w - t.px(8.0), head.h),
                        Face::Mono,
                        t.pt(12.0),
                        t.item,
                        &title,
                    );
                }
                let r = ui.area();
                let body = Rect::new(r.x + t.px(6.0), r.y, r.w - t.px(6.0), r.h);
                if cur.is_some_and(|f| f.status != "ok" && !f.status.is_empty()) {
                    let s = cur.map_or("", |f| f.status.as_str());
                    ui.within(body, |ui| {
                        ui.dim(if s == "binary" {
                            "Binary file."
                        } else {
                            "This file's diff is too large to show."
                        })
                    });
                } else {
                    let rows = &self.diff.rows;
                    ui.list(
                        "diff-rows",
                        &mut self.diff.rows_list,
                        body,
                        rows.len(),
                        19.0,
                        |ui, i, sel| {
                            let row = ui.area();
                            let x = &rows[i];
                            let (bg, fg, sign) = match x.kind.as_str() {
                                "add" => (Some(Rgb::hex(0x103824)), t.text, "+"),
                                "remove" => (Some(Rgb::hex(0x461a1e)), t.text, "−"),
                                "hunk" => (None, t.accent, ""),
                                "header" => (None, t.dim, ""),
                                _ => (None, t.item, ""),
                            };
                            if let Some(c) = bg {
                                ui.fill(row, c);
                            }
                            if sel {
                                ui.fill(Rect::new(row.x, row.y, t.px(3.0), row.h), t.accent);
                            }
                            let nw = t.px(42.0);
                            ui.text_in(
                                Rect::new(row.x + t.px(4.0), row.y, nw, row.h),
                                Face::Mono,
                                t.pt(11.0),
                                t.dim,
                                &x.n,
                            );
                            ui.text_in(
                                Rect::new(row.x + nw, row.y, t.px(12.0), row.h),
                                Face::Mono,
                                t.pt(12.0),
                                fg,
                                sign,
                            );
                            ui.text_in(
                                Rect::new(
                                    row.x + nw + t.px(12.0),
                                    row.y,
                                    row.w - nw - t.px(12.0),
                                    row.h,
                                ),
                                Face::Mono,
                                t.pt(12.0),
                                fg,
                                &x.content,
                            );
                        },
                    );
                }
            }
        }
        if let Some(m) = mode {
            self.diff.mode = m;
            reload = true;
        }
        if let Some(i) = pick {
            self.pick_diff_file(i);
        }
        if reload {
            PaneAct::LoadDiff
        } else {
            PaneAct::None
        }
    }
}
