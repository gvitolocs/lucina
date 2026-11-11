//! lucina-app-decisions: Decisions and Approvals on Juno (`web/src/views/Decisions.tsx`, `DecisionDetail.tsx`, `Approvals.tsx`,
//! `ApprovalDetail.tsx`). What waits on the owner, left; the one he picked, right.
//!
//! A decision's answer is an ordinary chat message (`Decision <id>: <n>. <option>`), and a successful send is never repeated
//! because the read after it failed. An approval is decided on its own route, only for the `payloadHash` shown. Juno has no
//! biometric key, so it can deny a sensitive request but not approve one: no Approve button, and the server refuses it anyway.
// Juno with no C library: Eyra is the libc and the program start, in Rust (docs/research/juno-pure-rust.md).
#[cfg(all(target_os = "linux", target_env = "gnu", target_arch = "aarch64"))]
extern crate eyra;

use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use lucina_app::{App, Face, Harness, ListState, Rect, Rgb, Sender, TextEdit, Theme, Ui};
use lucina_account::client::ApiError;
use lucina_account::decisions::{self, Approval, Decision, Outcome};
use lucina_account::engine::{new_uuid, CHAT_MAX_CHARS};
use lucina_account::signin::DEVICE_NAME;
use lucina_account::timeline::parse_rfc3339_ms;
use lucina_account::ui::SignInView;
use lucina_account::{credential, system, Client};

const REFRESH: Duration = Duration::from_secs(15);
/// The last lists, in RAM, so that opening shows them before any network. A stale approval is harmless: deciding it answers
/// for what was shown, and the server says if it is gone.
const CACHE_FILE: &str = "decisions-cache.json";

pub enum Msg {
    Changed,
    SignedIn,
    Loaded(Box<Lists>),
    Failed(ApiError),
    Answered { id: String, result: Result<(), ApiError> },
    DecisionNow(Box<Decision>),
    Decided { id: String, outcome: Outcome },
}

#[derive(Default)]
pub struct Lists {
    approvals: Vec<Approval>,
    pending: Vec<Decision>,
    earlier: Vec<Decision>,
    approvals_failed: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Entry {
    Approval(String),
    Decision(String),
    Earlier(String),
    Heading(&'static str),
}

struct Signed {
    client: Client,
    lists: Lists,
    loading: bool,
    error: Option<String>,
    entries: Vec<Entry>,
    list: ListState,
    open: Option<Entry>,
    words: TextEdit,
    /// The answer went out for this decision: never again.
    sent: Vec<String>,
    busy: bool,
    note: Option<String>,
    closed_approvals: Vec<(Approval, Outcome)>,
    confirm_deny: bool,
    last_load: Option<std::time::Instant>,
    focus_list: bool,
}

enum Screen {
    Starting,
    SignIn(SignInView),
    Signed(Box<Signed>),
}

struct Decisions {
    screen: Screen,
    tx: Option<Sender<Msg>>,
    /// In tests and demos this answers synchronously.
    test_client: Option<Client>,
}

fn now_ms() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_millis() as i64)
}

/// `relativeTime`, from the phone's words.
fn relative(iso: &str) -> String {
    let Some(t) = parse_rfc3339_ms(iso) else { return String::new() };
    let s = (now_ms() - t) / 1000;
    match s {
        i64::MIN..=59 => "just now".to_owned(),
        60..=3599 => format!("{} min ago", s / 60),
        3600..=86_399 => format!("{} h ago", s / 3600),
        _ => format!("{} d ago", s / 86_400),
    }
}

fn clock(seconds: i64) -> String {
    format!("{}:{:02}", seconds / 60, seconds % 60)
}

fn restore_cache(s: &mut Signed) {
    let Some(v) = credential::read_private(CACHE_FILE).and_then(|b| serde_json::from_slice::<serde_json::Value>(&b).ok()) else { return };
    let decisions = |k: &str| -> Vec<Decision> { v[k].as_array().map(|a| a.iter().filter_map(Decision::parse).collect()).unwrap_or_default() };
    s.lists = Lists { approvals: Approval::parse_list(&v["approvals"]), pending: decisions("pending"), earlier: decisions("earlier"), approvals_failed: false };
    s.rebuild();
}

fn save_cache(s: &Signed) {
    let raw_d = |l: &[Decision]| l.iter().map(|d| d.raw.clone()).collect::<Vec<_>>();
    let v = serde_json::json!({
        "approvals": s.lists.approvals.iter().map(|a| a.raw.clone()).collect::<Vec<_>>(),
        "pending": raw_d(&s.lists.pending),
        "earlier": raw_d(&s.lists.earlier),
    });
    let _ = credential::write_private(CACHE_FILE, v.to_string().as_bytes());
}

fn load(client: &Client) -> Result<Lists, ApiError> {
    // One failing history list must not hide what is waiting.
    let pending = client.decisions(Some("pending"), 50)?;
    let (approvals, approvals_failed) = match client.approvals() {
        Ok(a) => (a, false),
        Err(ApiError::Unauthorized(m)) => return Err(ApiError::Unauthorized(m)),
        Err(_) => (Vec::new(), true),
    };
    let mut earlier = Vec::new();
    for status in ["decided", "expired", "withdrawn"] {
        if let Ok(mut d) = client.decisions(Some(status), 20) {
            earlier.append(&mut d);
        }
    }
    earlier.sort_by_key(|d| std::cmp::Reverse(parse_rfc3339_ms(&d.created_at).unwrap_or(0)));
    earlier.truncate(20);
    Ok(Lists { approvals, pending, earlier, approvals_failed })
}

impl Signed {
    fn new(client: Client) -> Signed {
        Signed {
            client,
            lists: Lists::default(),
            loading: true,
            error: None,
            entries: Vec::new(),
            list: ListState::new(),
            open: None,
            words: TextEdit::new().placeholder("Or answer in your own words"),
            sent: Vec::new(),
            busy: false,
            note: None,
            closed_approvals: Vec::new(),
            confirm_deny: false,
            last_load: None,
            focus_list: true,
        }
    }

    fn rebuild(&mut self) {
        let mut e = Vec::new();
        if !self.lists.approvals.is_empty() || !self.closed_approvals.is_empty() {
            e.push(Entry::Heading("Approvals"));
            for a in &self.lists.approvals {
                e.push(Entry::Approval(a.id.clone()));
            }
            for (a, _) in &self.closed_approvals {
                if !self.lists.approvals.iter().any(|x| x.id == a.id) {
                    e.push(Entry::Approval(a.id.clone()));
                }
            }
        }
        if !self.lists.pending.is_empty() {
            e.push(Entry::Heading("Waiting for you"));
            for d in &self.lists.pending {
                e.push(Entry::Decision(d.id.clone()));
            }
        }
        if !self.lists.earlier.is_empty() {
            e.push(Entry::Heading("Earlier"));
            for d in &self.lists.earlier {
                e.push(Entry::Earlier(d.id.clone()));
            }
        }
        if self.open.is_none() {
            self.open = e.iter().find(|x| !matches!(x, Entry::Heading(_))).cloned();
        }
        self.entries = e;
    }

    fn approval(&self, id: &str) -> Option<&Approval> {
        self.lists.approvals.iter().find(|a| a.id == id).or_else(|| self.closed_approvals.iter().find(|(a, _)| a.id == id).map(|(a, _)| a))
    }

    fn decision(&self, id: &str) -> Option<&Decision> {
        self.lists.pending.iter().chain(self.lists.earlier.iter()).find(|d| d.id == id)
    }
}

fn session_for(client: &Client) -> String {
    let device = client.session().ok().and_then(|s| s.device_id).unwrap_or_else(|| "laptop".to_owned());
    let clean: String = device.chars().filter(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-')).take(58).collect();
    format!("juno-{clean}")
}

impl Decisions {
    fn new(tx: Option<Sender<Msg>>, test_client: Option<Client>) -> Decisions {
        let mut d = Decisions { screen: Screen::Starting, tx, test_client };
        d.open();
        d
    }

    fn open(&mut self) {
        if let Some(c) = self.test_client.clone() {
            self.screen = Screen::Signed(Box::new(Signed::new(c)));
            self.reload();
            return;
        }
        match system::client() {
            Some(c) => {
                let mut signed = Box::new(Signed::new(c));
                restore_cache(&mut signed);
                self.screen = Screen::Signed(signed);
                self.reload();
            }
            None => self.show_sign_in(),
        }
    }

    fn show_sign_in(&mut self) {
        let tx = self.tx.clone();
        let tx2 = self.tx.clone();
        let changed: Arc<dyn Fn() + Send + Sync> = Arc::new(move || {
            if let Some(t) = &tx {
                t.send(Msg::Changed);
            }
        });
        let done: Arc<dyn Fn() + Send + Sync> = Arc::new(move || {
            if let Some(t) = &tx2 {
                t.send(Msg::SignedIn);
            }
        });
        self.screen = Screen::SignIn(SignInView::start(changed, done));
    }

    /// Runs `f` on a thread in the app; on the caller's thread in tests, where there is nothing to send to.
    fn run(&mut self, f: impl FnOnce() -> Msg + Send + 'static) {
        match &self.tx {
            Some(tx) => tx.spawn(f),
            None => {
                let m = f();
                self.message(m);
            }
        }
    }

    fn reload(&mut self) {
        let Screen::Signed(s) = &mut self.screen else { return };
        s.last_load = Some(std::time::Instant::now());
        let client = s.client.clone();
        self.run(move || match load(&client) {
            Ok(l) => Msg::Loaded(Box::new(l)),
            Err(e) => Msg::Failed(e),
        });
    }

    fn answer(&mut self, id: String, text: String) {
        let Screen::Signed(s) = &mut self.screen else { return };
        if s.sent.contains(&id) || s.busy {
            return;
        }
        s.busy = true;
        s.note = None;
        let client = s.client.clone();
        self.run(move || {
            let session = session_for(&client);
            let result = client.send_chat(&text, &session, &new_uuid(), None).map(|_| ());
            Msg::Answered { id, result }
        });
    }

    fn decide(&mut self, a: Approval, approve: bool) {
        let Screen::Signed(s) = &mut self.screen else { return };
        if s.busy {
            return;
        }
        s.busy = true;
        s.note = None;
        s.confirm_deny = false;
        let client = s.client.clone();
        self.run(move || {
            let outcome = Outcome::of(approve, client.approval_decide(&a.id, approve, &a.payload_hash));
            Msg::Decided { id: a.id, outcome }
        });
    }
}

impl App for Decisions {
    type Msg = Msg;

    fn message(&mut self, m: Msg) {
        match m {
            Msg::Changed => {}
            Msg::SignedIn => self.open(),
            Msg::Loaded(lists) => {
                if let Screen::Signed(s) = &mut self.screen {
                    s.lists = *lists;
                    s.loading = false;
                    s.error = None;
                    s.rebuild();
                }
            }
            Msg::Failed(e) => {
                if matches!(e, ApiError::Unauthorized(_)) && self.test_client.is_none() {
                    system::report_rejected();
                    self.show_sign_in();
                } else if let Screen::Signed(s) = &mut self.screen {
                    s.loading = false;
                    s.error = Some(e.to_string());
                }
            }
            Msg::Answered { id, result } => {
                let mut read_again = None;
                if let Screen::Signed(s) = &mut self.screen {
                    s.busy = false;
                    match result {
                        Ok(()) => {
                            s.sent.push(id.clone());
                            s.words.clear();
                            s.note = Some("Sent. Lucina has it.".to_owned());
                            read_again = Some((s.client.clone(), id));
                        }
                        Err(_) => s.note = Some("It did not go through. Try again.".to_owned()),
                    }
                }
                if let Some((client, id)) = read_again {
                    self.run(move || match client.decision(&id) {
                        Ok(d) => Msg::DecisionNow(Box::new(d)),
                        // The send worked; only the look afterwards did not.
                        Err(_) => Msg::Changed,
                    });
                }
            }
            Msg::DecisionNow(d) => {
                if let Screen::Signed(s) = &mut self.screen {
                    if d.status != "pending" {
                        s.lists.pending.retain(|x| x.id != d.id);
                        s.lists.earlier.retain(|x| x.id != d.id);
                        s.lists.earlier.insert(0, *d);
                        s.note = None;
                        s.rebuild();
                    }
                }
            }
            Msg::Decided { id, outcome } => {
                if let Screen::Signed(s) = &mut self.screen {
                    s.busy = false;
                    s.note = Some(outcome.text().to_owned());
                    if outcome.closes() {
                        if let Some(pos) = s.lists.approvals.iter().position(|a| a.id == id) {
                            let a = s.lists.approvals.remove(pos);
                            s.closed_approvals.push((a, outcome));
                        }
                        s.rebuild();
                    }
                }
            }
        }
    }

    fn on_quit(&mut self) {
        if let Screen::Signed(s) = &self.screen {
            save_cache(s);
        }
    }

    fn view(&mut self, ui: &mut Ui) {
        let mut todo: Option<Todo> = None;
        match &mut self.screen {
            Screen::SignIn(v) => {
                ui.header("Decisions");
                v.view(ui, &format!("Decisions and approvals wait in your account. This laptop asks to be added as “{DEVICE_NAME}”."));
            }
            Screen::Starting => {}
            Screen::Signed(s) => {
                ui.header("Decisions");
                view_signed(ui, s, &mut todo);
                if s.last_load.is_none_or(|t| t.elapsed() >= REFRESH) && !s.busy {
                    todo = todo.or(Some(Todo::Reload));
                }
            }
        }
        // A countdown on screen needs the second; otherwise only the next refresh does.
        let counting = matches!(&self.screen, Screen::Signed(s) if matches!(&s.open, Some(Entry::Approval(_))));
        ui.wake_after(if counting { Duration::from_secs(1) } else { REFRESH });
        match todo {
            Some(Todo::Reload) => self.reload(),
            Some(Todo::Answer(id, text)) => self.answer(id, text),
            Some(Todo::Decide(a, approve)) => self.decide(a, approve),
            None => {}
        }
    }
}

enum Todo {
    Reload,
    Answer(String, String),
    Decide(Approval, bool),
}

fn risk_colour(t: &Theme, risk: &str) -> Rgb {
    match risk {
        "high" => t.danger,
        "low" => t.ok,
        _ => t.accent,
    }
}

fn badge(ui: &mut Ui, x: &mut i32, y: i32, text: &str, c: Rgb) {
    let t = ui.theme();
    let w = ui.measure(Face::Sans, t.pt(12.0), text) + t.px(14.0);
    let r = Rect::new(*x, y, w, t.px(20.0));
    ui.fill_round(r, t.px(5.0), t.panel_hi);
    ui.text_in(Rect::new(r.x + t.px(7.0), r.y, w, r.h), Face::Sans, t.pt(12.0), c, text);
    *x += w + t.px(6.0);
}

fn view_signed(ui: &mut Ui, s: &mut Signed, todo: &mut Option<Todo>) {
    let t = ui.theme();
    let left = ui.cut_left(t.px(340.0));
    ui.fill(left, t.bar);
    let list_r = Rect::new(left.x + t.px(8.0), left.y + t.px(8.0), left.w - t.px(16.0), left.h - t.px(16.0));
    if s.entries.is_empty() {
        ui.within(list_r, |ui| {
            if s.loading {
                ui.dim("Loading…");
            } else if let Some(e) = &s.error {
                ui.wrapped(Face::Sans, t.pt(14.0), t.danger, e);
                if ui.button("Try again") {
                    *todo = Some(Todo::Reload);
                }
            } else {
                ui.dim("Nothing is waiting for you.");
            }
        });
    } else {
        let entries = &s.entries;
        let open = &s.open;
        let lists = &s.lists;
        let closed = &s.closed_approvals;
        // The arrows work from the first key.
        if std::mem::take(&mut s.focus_list) {
            ui.focus("entries");
        }
        let res = ui.list("entries", &mut s.list, list_r, entries.len(), 56.0, |ui, i, selected| {
            let row = ui.area();
            let e = &entries[i];
            if let Entry::Heading(h) = e {
                ui.text_in(Rect::new(row.x + t.px(4.0), row.bottom() - t.px(26.0), row.w, t.px(24.0)), Face::Bold, t.pt(13.0), t.accent, h);
                return;
            }
            if open.as_ref() == Some(e) || selected {
                ui.fill_round(Rect::new(row.x, row.y + 1, row.w, row.h - 2), t.px(6.0), t.selection);
            }
            let (title, sub) = match e {
                Entry::Approval(id) => match lists.approvals.iter().find(|a| &a.id == id) {
                    Some(a) => (a.summary.clone(), format!("{} · {}", a.requester(), relative(&a.created_at))),
                    None => {
                        let (a, o) = closed.iter().find(|(a, _)| &a.id == id).map_or((None, Outcome::Gone), |(a, o)| (Some(a), *o));
                        (a.map_or(String::new(), |a| a.summary.clone()), o.text().to_owned())
                    }
                },
                Entry::Decision(id) | Entry::Earlier(id) => match lists.pending.iter().chain(lists.earlier.iter()).find(|d| &d.id == id) {
                    Some(d) => (d.question.clone(), format!("{} · {} · {}", d.asker(), d.status, relative(&d.created_at))),
                    None => (String::new(), String::new()),
                },
                Entry::Heading(_) => unreachable!("handled above"),
            };
            ui.text_in(Rect::new(row.x + t.px(8.0), row.y + t.px(6.0), row.w - t.px(16.0), t.px(22.0)), Face::Sans, t.pt(14.0), t.text, &title);
            ui.text_in(Rect::new(row.x + t.px(8.0), row.y + t.px(29.0), row.w - t.px(16.0), t.px(20.0)), Face::Sans, t.pt(12.0), t.dim, &sub);
        });
        // Moving through the list shows each entry; there is nothing to load.
        let moved = if res.selection_changed { s.list.selected } else { None };
        if let Some(i) = res.activated.or(moved) {
            let e = s.entries[i].clone();
            if !matches!(e, Entry::Heading(_)) {
                s.open = Some(e);
                s.note = None;
                s.confirm_deny = false;
                s.words.clear();
            }
        }
    }

    let pane = ui.area();
    let pane = Rect::new(pane.x + t.px(22.0), pane.y + t.px(16.0), (pane.w - t.px(44.0)).min(t.px(680.0)), pane.h - t.px(24.0));
    ui.within(pane, |ui| {
        if s.lists.approvals_failed {
            ui.wrapped(Face::Sans, t.pt(13.0), t.danger, "Could not read the approvals.");
        }
        match s.open.clone() {
            Some(Entry::Approval(id)) => view_approval(ui, s, &id, todo),
            Some(Entry::Decision(id)) | Some(Entry::Earlier(id)) => view_decision(ui, s, &id, todo),
            _ => {
                ui.heading("Decisions");
                ui.paragraph("When Lucina or an agent needs you, it waits here.");
            }
        }
    });
}

fn view_decision(ui: &mut Ui, s: &mut Signed, id: &str, todo: &mut Option<Todo>) {
    let t = ui.theme();
    let Some(d) = s.decision(id).cloned() else {
        ui.dim("This decision is gone.");
        return;
    };
    ui.heading(&d.question);
    ui.dim(&format!("{} asks · {}", d.asker(), relative(&d.created_at)));
    let row = ui.cut_top(t.px(26.0));
    let mut x = row.x;
    let impact = format!("{} impact", d.impact);
    badge(ui, &mut x, row.y + t.px(3.0), &impact, risk_colour(&t, &d.impact));
    let urgency = format!("{} urgency", d.urgency);
    badge(ui, &mut x, row.y + t.px(3.0), &urgency, t.item);
    badge(ui, &mut x, row.y + t.px(3.0), if d.reversible { "reversible" } else { "irreversible" }, if d.reversible { t.item } else { t.danger });
    if let Some(r) = &d.reason {
        ui.space(6.0);
        ui.label("Reason");
        ui.paragraph(r);
    }
    ui.space(8.0);
    if d.status == "pending" {
        let sent = s.sent.iter().any(|x| x == &d.id);
        for (i, option) in d.options.iter().enumerate() {
            let rec = d.recommendation.as_deref() == Some(option.as_str());
            let label = if rec { format!("{option} (recommended)") } else { option.clone() };
            let id_btn = ("option", i);
            if sent || s.busy {
                ui.dim(&label);
            } else if ui.button_id(id_btn, &label) {
                *todo = Some(Todo::Answer(d.id.clone(), decisions::answer_text(&d.id, i + 1, option)));
            }
        }
        ui.space(8.0);
        if !sent {
            let max = CHAT_MAX_CHARS - format!("Decision {}: ", d.id).chars().count();
            let res = ui.text_field("words", &mut s.words);
            let words = s.words.text.trim().to_owned();
            let go = ui.button(if s.busy { "Sending…" } else { "Answer in my words" });
            if (go || res.submitted) && !words.is_empty() && words.chars().count() <= max && !s.busy {
                *todo = Some(Todo::Answer(d.id.clone(), decisions::free_text(&d.id, &words)));
            }
        }
    } else {
        let word = match d.status.as_str() {
            "decided" => "Decided",
            "expired" => "Expired",
            "withdrawn" => "Withdrawn",
            _ => "Closed",
        };
        ui.label(word);
        if let Some(c) = &d.chosen_option {
            ui.paragraph(c);
        }
        match d.decided_by.as_deref() {
            Some("owner") => ui.dim("By you"),
            Some(_) => ui.dim("By Lucina"),
            None => {}
        }
    }
    if let Some(n) = &s.note {
        ui.space(6.0);
        ui.wrapped(Face::Sans, t.pt(14.0), t.ok, n);
    }
    if !d.evidence.is_empty() {
        ui.space(10.0);
        ui.label("Evidence");
        for (kind, text, _) in &d.evidence {
            ui.dim(kind);
            ui.paragraph(text);
        }
    }
}

fn view_approval(ui: &mut Ui, s: &mut Signed, id: &str, todo: &mut Option<Todo>) {
    let t = ui.theme();
    let Some(a) = s.approval(id).cloned() else {
        ui.dim("This request is gone.");
        return;
    };
    let closed = s.closed_approvals.iter().find(|(x, _)| x.id == a.id).map(|(_, o)| *o);
    ui.heading(&a.summary);
    let who = match &a.requester_detail {
        Some(d) => format!("{} ({d}) asks", a.requester()),
        None => format!("{} asks", a.requester()),
    };
    ui.dim(&who);
    let row = ui.cut_top(t.px(26.0));
    let mut x = row.x;
    let risk = format!("{} risk", a.risk);
    badge(ui, &mut x, row.y + t.px(3.0), &risk, risk_colour(&t, &a.risk));
    if a.sensitive {
        badge(ui, &mut x, row.y + t.px(3.0), "sensitive", t.danger);
    }
    let left = parse_rfc3339_ms(&a.expires_at).map(|e| ((e - now_ms()) / 1000).max(0));
    if closed.is_none() {
        if let Some(l) = left {
            if l < 86_400 {
                badge(ui, &mut x, row.y + t.px(3.0), &format!("{} left", clock(l)), t.item);
            }
        }
    }
    ui.space(8.0);
    if !a.action.is_empty() {
        ui.dim(&a.action);
    }
    // The exact thing that will run or be revealed.
    ui.wrapped(Face::Mono, t.pt(13.0), t.text, &a.details);
    ui.space(12.0);
    if let Some(o) = closed {
        ui.label(o.text());
        return;
    }
    if left == Some(0) {
        ui.label(Outcome::Expired.text());
        return;
    }
    if s.confirm_deny {
        ui.label("Deny this request?");
        // Only the owner's hands decide: Lucina never approves or denies on screen.
        ui.owner_only(|ui| {
            ui.row(32.0, |ui| {
                if ui.primary_button("Deny") {
                    *todo = Some(Todo::Decide(a.clone(), false));
                }
                if ui.button("Back") {
                    s.confirm_deny = false;
                }
            });
        });
    } else if s.busy {
        ui.dim("Sending…");
    } else {
        ui.owner_only(|ui| {
            ui.row(32.0, |ui| {
                if a.can_approve() && ui.primary_button("Approve") {
                    *todo = Some(Todo::Decide(a.clone(), true));
                }
                if ui.button("Deny") {
                    s.confirm_deny = true;
                }
            });
        });
        if !a.can_approve() {
            ui.space(6.0);
            ui.wrapped(Face::Sans, t.pt(14.0), t.item, "This one is sensitive: approve it with Face ID on your iPhone. You can deny it here.");
        }
    }
    if let Some(n) = &s.note {
        ui.space(6.0);
        ui.wrapped(Face::Sans, t.pt(14.0), t.accent, n);
    }
}

fn main() {
    let mut args = std::env::args().skip(1);
    if args.next().as_deref() == Some("--render") {
        let (out, base) = (args.next().unwrap_or_else(|| "decisions.ppm".into()), args.next());
        let token = lucina_account::Token::parse(&std::env::var("LUCINA_SECRETARY_TEST_TOKEN").unwrap_or_default());
        let client = base.and_then(|b| Client::new(&b, token));
        let mut h = Harness::new(Decisions::new(None, client), 1920, 1080);
        h.core.paint();
        std::fs::write(out, h.ppm()).expect("write");
        return;
    }
    lucina_app::run("lucina-app-decisions", |tx| Decisions::new(Some(tx), None));
}

#[cfg(test)]
mod tests {
    use super::*;
    use lucina_account::fake::{Fake, TOKEN};

    fn app(f: &Fake) -> Harness<Decisions> {
        let c = Client::new(f.base(), lucina_account::Token::parse(TOKEN)).unwrap();
        let mut h = Harness::new(Decisions::new(None, Some(c)), 1920, 1080);
        h.core.paint();
        h
    }

    fn signed(d: &mut Decisions) -> &mut Signed {
        match &mut d.screen {
            Screen::Signed(s) => s,
            _ => panic!("not signed in"),
        }
    }

    #[test]
    fn everything_waiting_is_listed_with_approvals_first() {
        let f = Fake::start();
        f.fill_demo();
        let mut h = app(&f);
        let s = signed(h.app());
        assert_eq!(s.lists.approvals.len(), 2);
        assert_eq!(s.lists.pending.len(), 1);
        assert_eq!(s.lists.earlier.len(), 1);
        assert_eq!(s.entries[0], Entry::Heading("Approvals"));
        assert_eq!(s.open, Some(Entry::Approval("ap1".into())));
    }

    #[test]
    fn answering_is_a_chat_message_in_lucinas_words_and_goes_once() {
        let f = Fake::start();
        f.fill_demo();
        let mut h = app(&f);
        h.app().answer("dc-demo1".into(), decisions::answer_text("dc-demo1", 2, "Aspetta i test sul laptop"));
        assert_eq!(signed(h.app()).sent, ["dc-demo1"]);
        h.app().answer("dc-demo1".into(), "again".into());
        let bodies = f.bodies("/v1/chat");
        assert_eq!(bodies.len(), 1);
        assert!(bodies[0].contains("Decision dc-demo1: 2. Aspetta i test sul laptop"));
    }

    #[test]
    fn the_arrows_move_through_the_list_and_show_each_entry() {
        let f = Fake::start();
        f.fill_demo();
        let mut h = app(&f);
        h.core.message(Msg::Changed);
        h.core.paint();
        h.key(lucina_app::keys::KEY_DOWN);
        h.key(lucina_app::keys::KEY_DOWN);
        h.key(lucina_app::keys::KEY_DOWN);
        assert_eq!(signed(h.app()).open, Some(Entry::Approval("ap2".into())));
    }

    #[test]
    fn a_failed_answer_can_be_tried_again() {
        let f = Fake::start();
        f.fill_demo();
        let mut h = app(&f);
        f.fail_chats(1);
        h.app().answer("dc-demo1".into(), "x".into());
        assert!(signed(h.app()).sent.is_empty());
        assert!(signed(h.app()).note.as_deref().unwrap().contains("did not go through"));
        h.app().answer("dc-demo1".into(), "x".into());
        assert_eq!(signed(h.app()).sent, ["dc-demo1"]);
    }

    #[test]
    fn a_sensitive_request_has_no_approve_and_can_be_denied() {
        let f = Fake::start();
        f.fill_demo();
        let mut h = app(&f);
        signed(h.app()).open = Some(Entry::Approval("ap2".into()));
        h.core.input(lucina_app::Input::Wheel(0));
        h.core.paint();
        let a = signed(h.app()).approval("ap2").unwrap().clone();
        assert!(!a.can_approve());
        // Asked anyway, the server says no, and nothing closes.
        h.app().decide(a.clone(), true);
        assert_eq!(signed(h.app()).note.as_deref(), Some(Outcome::NeedsFaceId.text()));
        assert!(signed(h.app()).closed_approvals.is_empty());
        h.app().decide(a, false);
        assert_eq!(signed(h.app()).closed_approvals.len(), 1);
        assert_eq!(signed(h.app()).closed_approvals[0].1, Outcome::Denied);
        assert_eq!(Client::new(f.base(), lucina_account::Token::parse(TOKEN)).unwrap().approval("ap2").unwrap().status, "denied");
    }

    #[test]
    fn approving_uses_the_hash_on_screen() {
        let f = Fake::start();
        f.fill_demo();
        let mut h = app(&f);
        let mut a = signed(h.app()).approval("ap1").unwrap().clone();
        a.payload_hash = "something else".into();
        h.app().decide(a, true);
        assert_eq!(signed(h.app()).note.as_deref(), Some(Outcome::Changed.text()));
        let a = signed(h.app()).approval("ap1").unwrap().clone();
        h.app().decide(a, true);
        assert_eq!(signed(h.app()).closed_approvals[0].1, Outcome::Approved);
        // It keeps its place, saying what happened.
        assert!(signed(h.app()).entries.contains(&Entry::Approval("ap1".into())));
    }

    #[test]
    fn lucina_cannot_approve_or_deny_on_screen() {
        use lucina_ax::accesskit::Role;
        use lucina_ax::rules::Refusal;
        use lucina_ax::Refused;
        let f = Fake::start();
        f.fill_demo();
        let mut h = app(&f);
        assert_eq!(signed(h.app()).open, Some(Entry::Approval("ap1".into())));
        assert!(signed(h.app()).approval("ap1").unwrap().can_approve());
        let r = h.act_on(Role::Button, "Approve", "click", None);
        assert_eq!(r.unwrap_err(), Refused::Rule(Refusal::OwnerOnly));
        let r = h.act_on(Role::Button, "Deny", "click", None);
        assert_eq!(r.unwrap_err(), Refused::Rule(Refusal::OwnerOnly));
        assert!(!signed(h.app()).confirm_deny);
        assert!(signed(h.app()).closed_approvals.is_empty());
        assert_eq!(Client::new(f.base(), lucina_account::Token::parse(TOKEN)).unwrap().approval("ap1").unwrap().status, "pending");
    }

    #[test]
    fn a_revoked_token_in_a_test_shows_the_error_and_times_are_short() {
        let f = Fake::start();
        f.revoke();
        let mut h = app(&f);
        assert!(signed(h.app()).error.as_deref().unwrap().contains("Signed out"));
        assert_eq!(clock(125), "2:05");
        assert_eq!(relative("not a time"), "");
    }
}
