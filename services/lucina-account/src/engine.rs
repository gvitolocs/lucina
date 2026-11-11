//! The chat's network side: three threads around one [`State`], which the app reads under its lock when drawing.
//!
//! * the **stream**: `GET /v1/timeline/stream`, resuming with `Last-Event-ID`, backing off 1, 2, 5, 10, 30 s. Only the main
//!   chat opens one (the server allows 4 per device); a conversation being read is fetched every few seconds instead;
//! * **sending**: one `POST /v1/chat` at a time, retrying with the same `clientMessageId` is safe;
//! * **fetching**: pages (the `secretary_chat::sync` cursor loop), jobs, the secrets queue and saves.
//!
//! `wake` is called after every change, from any thread; the app redraws on it.
use std::sync::mpsc::{channel, Receiver, RecvTimeoutError, Sender};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use secretary_chat::sync::{Action as SyncAction, Outcome as SyncOutcome, SyncPlan};

use crate::client::{ApiError, Client, TimelineQuery};
use crate::secrets::{self, Project, SaveSecret, SecretQueue, SecretReceipt};
use crate::timeline::{Conversation, Timeline, PAGE};

/// Server limit for a chat message.
pub const CHAT_MAX_CHARS: usize = 12_000;
/// The main chat's last messages, in RAM, so that opening the app shows them before any network: [`Engine::save_cache`].
const CACHE_FILE: &str = "chat-cache.json";
const CACHE_ITEMS: usize = 200;
const BACKOFF: [u64; 5] = [1, 2, 5, 10, 30];
const JOB_POLL: Duration = Duration::from_secs(3);

/// What the header's dot shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum StreamStatus {
    #[default]
    Connecting,
    Live,
    Reconnecting,
    Offline,
}

/// How a save came out, for the card that asked.
#[derive(Debug)]
pub enum SaveResult {
    Saved(SecretReceipt),
    /// The broker says it exists. Replacing takes one more tap, with this same value: [`SaveSecret::overwrite`].
    Exists(SaveSecret),
    Failed(String),
}

#[derive(Default)]
pub struct State {
    pub timeline: Timeline,
    pub status: StreamStatus,
    /// A 401: the token is dead. The app forgets it and shows sign-in.
    pub signed_out: bool,
    pub device_id: Option<String>,
    pub device_name: Option<String>,
    pub session_id: Option<String>,
    /// The first load has come back.
    pub loaded: bool,
    pub load_error: Option<String>,
    pub older_loading: bool,
    pub sending: bool,
    pub secret_queue: Option<SecretQueue>,
    pub secret_queue_failed: bool,
    pub catalog: Option<Vec<Project>>,
    pub conversations: Vec<Conversation>,
    /// `(request id, result)`, taken by the card.
    pub save_result: Option<(Option<String>, SaveResult)>,
    /// What the server says of decisions the chat's buttons answer: pending ids, and the closed ones with their choice. `None`
    /// until read: a button stays as it is rather than say something unknown.
    pub decisions: Option<DecisionStates>,
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct DecisionStates {
    pub pending: Vec<String>,
    /// `(id, chosen option)`.
    pub closed: Vec<(String, Option<String>)>,
}

impl DecisionStates {
    /// `Some(choice)` when this decision is over: not pending. One too old for the lists counts as over too.
    pub fn closed(&self, id: &str) -> Option<Option<&str>> {
        if self.pending.iter().any(|p| p == id) {
            return None;
        }
        Some(self.closed.iter().find(|(c, _)| c == id).and_then(|(_, o)| o.as_deref()))
    }
}

enum Fetch {
    Decisions,
    Sync,
    Older,
    Secrets,
    Catalog,
    Conversations,
    Save(SaveSecret),
    Dismiss(String),
    Stop,
}

struct Outgoing {
    local_id: String,
    text: String,
    cmid: String,
    reply_to: Option<String>,
}

/// One conversation's connection: "mine" to talk in, or another one to read.
pub struct Engine {
    state: Arc<Mutex<State>>,
    fetch: Sender<Fetch>,
    send: Sender<Option<Outgoing>>,
    conversation: Option<String>,
    stopped: Arc<AtomicBool>,
    wake: Arc<dyn Fn() + Send + Sync>,
}

pub(crate) fn now_ms() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_millis() as i64)
}

/// A random UUID, from the kernel's getrandom through /dev/urandom.
pub fn new_uuid() -> String {
    use std::io::Read;
    let mut b = [0u8; 16];
    if std::fs::File::open("/dev/urandom").and_then(|mut f| f.read_exact(&mut b)).is_err() {
        let t = now_ms() as u128 ^ (std::process::id() as u128) << 64;
        b = t.to_le_bytes();
    }
    b[6] = (b[6] & 0x0f) | 0x40;
    b[8] = (b[8] & 0x3f) | 0x80;
    let h: String = b.iter().map(|x| format!("{x:02x}")).collect();
    format!("{}-{}-{}-{}-{}", &h[..8], &h[8..12], &h[12..16], &h[16..20], &h[20..])
}

/// The server's rule: no control characters but newline and tab; `state/chat.ts` `cleanMessage`.
pub fn clean_message(text: &str) -> String {
    let t = text.replace("\r\n", "\n").replace('\r', "\n");
    t.chars().filter(|c| !c.is_control() || matches!(c, '\n' | '\t')).collect::<String>().trim().to_owned()
}

/// `[A-Za-z0-9_-]{1,64}`, stable for the device, so Lucina keeps one session for it across restarts.
fn session_id_for(device_id: &str) -> String {
    let clean: String = device_id.chars().filter(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-')).take(58).collect();
    format!("juno-{clean}")
}

impl Engine {
    /// `conversation`: `None` is this device's main chat; `Some(key)` one conversation of All chats, read-only.
    pub fn start(client: Client, conversation: Option<String>, wake: Arc<dyn Fn() + Send + Sync>) -> Engine {
        let mut first = State::default();
        if conversation.is_none() {
            restore_cache(&mut first);
        }
        let state = Arc::new(Mutex::new(first));
        let (fetch, fetch_rx) = channel();
        let (send, send_rx) = channel();
        let stopped = Arc::new(AtomicBool::new(false));
        let e = Engine { state: state.clone(), fetch, send, conversation: conversation.clone(), stopped: stopped.clone(), wake: wake.clone() };
        if conversation.is_none() {
            let (client, state, wake, fetch) = (client.clone(), state.clone(), wake.clone(), e.fetch.clone());
            std::thread::spawn(move || stream_thread(client, state, fetch, stopped, wake));
        }
        {
            let (client, state, wake) = (client.clone(), state.clone(), wake.clone());
            std::thread::spawn(move || fetch_thread(client, conversation, state, fetch_rx, wake));
        }
        std::thread::spawn(move || send_thread(client, state, send_rx, wake));
        let _ = e.fetch.send(Fetch::Sync);
        let _ = e.fetch.send(Fetch::Secrets);
        let _ = e.fetch.send(Fetch::Decisions);
        e
    }

    /// Called when the app is leaving. It takes no network and little time.
    pub fn save_cache(&self) {
        if self.read_only() {
            return;
        }
        let st = self.state();
        if st.signed_out {
            drop(st);
            forget_cache();
            return;
        }
        let v = serde_json::json!({
            "timeline": st.timeline.snapshot(CACHE_ITEMS),
            "device": { "id": st.device_id, "name": st.device_name },
            "conversations": st.conversations.iter().map(Conversation::to_json).collect::<Vec<_>>(),
        });
        drop(st);
        let _ = crate::credential::write_private(CACHE_FILE, v.to_string().as_bytes());
    }

    pub fn state(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// Read-only: not the main chat.
    pub fn read_only(&self) -> bool {
        self.conversation.is_some()
    }

    /// Adds the bubble and queues it. `false` if nothing was sent: empty, too long, a `/secret` command, read-only. The caller
    /// has already run the guard ([`secrets::detect`]) on `text`.
    pub fn send(&self, text: &str, reply_to: Option<String>) -> bool {
        let message = clean_message(text);
        if self.read_only() || message.is_empty() || message.chars().count() > CHAT_MAX_CHARS || secrets::is_command(&message) {
            return false;
        }
        let cmid = new_uuid();
        let local_id = self.state().timeline.add_local(&message, &cmid, reply_to.clone(), now_ms());
        (self.wake)();
        self.send.send(Some(Outgoing { local_id, text: message, cmid, reply_to })).is_ok()
    }

    /// A failed bubble again, under its own `clientMessageId`, so the server cannot act on it twice.
    pub fn retry(&self, local_id: &str) -> bool {
        let out = {
            let mut st = self.state();
            let Some(m) = st.timeline.get(local_id) else { return false };
            let Some(cmid) = m.item.client_message_id.clone() else { return false };
            let out = Outgoing { local_id: local_id.to_owned(), text: m.item.text.clone(), cmid, reply_to: m.item.reply_to.clone() };
            st.timeline.mark_sending(local_id);
            out
        };
        (self.wake)();
        self.send.send(Some(out)).is_ok()
    }

    pub fn load_older(&self) {
        let _ = self.fetch.send(Fetch::Older);
    }

    pub fn resync(&self) {
        let _ = self.fetch.send(Fetch::Sync);
    }

    pub fn refresh_secrets(&self) {
        let _ = self.fetch.send(Fetch::Secrets);
    }

    pub fn load_catalog(&self) {
        let _ = self.fetch.send(Fetch::Catalog);
    }

    pub fn load_conversations(&self) {
        let _ = self.fetch.send(Fetch::Conversations);
    }

    /// For a second engine on the same app.
    pub fn wake_handle(&self) -> Arc<dyn Fn() + Send + Sync> {
        self.wake.clone()
    }

    pub fn save_secret(&self, save: SaveSecret) {
        let _ = self.fetch.send(Fetch::Save(save));
    }

    pub fn dismiss_secret(&self, id: &str) {
        let _ = self.fetch.send(Fetch::Dismiss(id.to_owned()));
    }
}

impl Drop for Engine {
    fn drop(&mut self) {
        self.stopped.store(true, Ordering::Relaxed);
        let _ = self.fetch.send(Fetch::Stop);
        let _ = self.send.send(None);
    }
}

/// The cached chat goes with the token.
pub fn forget_cache() {
    crate::credential::remove_private(CACHE_FILE);
}

fn restore_cache(st: &mut State) {
    let Some(bytes) = crate::credential::read_private(CACHE_FILE) else { return };
    let Ok(v) = serde_json::from_slice::<serde_json::Value>(&bytes) else { return };
    st.timeline.restore(&v["timeline"]);
    st.device_id = v["device"]["id"].as_str().map(str::to_owned);
    st.device_name = v["device"]["name"].as_str().map(str::to_owned);
    st.session_id = st.device_id.as_deref().map(session_id_for);
    st.conversations = v["conversations"].as_array().map(|a| a.iter().filter_map(Conversation::parse).collect()).unwrap_or_default();
    // Something to show: the first sync comes in behind it.
    st.loaded = !st.timeline.is_empty();
}

fn note_auth(state: &Arc<Mutex<State>>, wake: &Arc<dyn Fn() + Send + Sync>, e: &ApiError) {
    if matches!(e, ApiError::Unauthorized(_)) {
        state.lock().unwrap_or_else(|p| p.into_inner()).signed_out = true;
        wake();
    }
}

fn stream_thread(client: Client, state: Arc<Mutex<State>>, fetch: Sender<Fetch>, stopped: Arc<AtomicBool>, wake: Arc<dyn Fn() + Send + Sync>) {
    let mut last_id: Option<String> = None;
    let mut failures = 0usize;
    let mut connected_before = false;
    loop {
        if stopped.load(Ordering::Relaxed) {
            return;
        }
        match client.stream(last_id.as_deref()) {
            Ok(mut stream) => {
                if connected_before {
                    // Fill the gap the connection left: without an id seen yet, `Last-Event-ID` cannot.
                    let _ = fetch.send(Fetch::Sync);
                    let _ = fetch.send(Fetch::Secrets);
                    let _ = fetch.send(Fetch::Decisions);
                }
                connected_before = true;
                failures = 0;
                {
                    let mut st = state.lock().unwrap_or_else(|p| p.into_inner());
                    st.status = StreamStatus::Live;
                }
                wake();
                loop {
                    match stream.next_item() {
                        Ok(Some(item)) => {
                            last_id = Some(item.id.clone());
                            let secret = item.kind.as_deref().is_some_and(|k| k.starts_with("secret_"));
                            let decision = item.kind.as_deref() == Some("decision") || item.text.starts_with("Decision ");
                            {
                                let mut st = state.lock().unwrap_or_else(|p| p.into_inner());
                                if item.is_mine(st.device_id.as_deref()) {
                                    st.timeline.merge(vec![item]);
                                }
                            }
                            if secret {
                                let _ = fetch.send(Fetch::Secrets);
                            }
                            if decision {
                                let _ = fetch.send(Fetch::Decisions);
                            }
                            wake();
                        }
                        // The server's own end, or the connection's time limit: resume at once.
                        Ok(None) => break,
                        Err(_) => break,
                    }
                }
            }
            Err(e) => {
                note_auth(&state, &wake, &e);
                if matches!(e, ApiError::Unauthorized(_)) {
                    return;
                }
                let wait = match e {
                    ApiError::RateLimited(s) => s.max(1),
                    _ => BACKOFF[failures.min(BACKOFF.len() - 1)],
                };
                failures += 1;
                {
                    let mut st = state.lock().unwrap_or_else(|p| p.into_inner());
                    st.status = if failures >= BACKOFF.len() { StreamStatus::Offline } else { StreamStatus::Reconnecting };
                }
                wake();
                std::thread::sleep(Duration::from_secs(wait));
            }
        }
    }
}

fn send_thread(client: Client, state: Arc<Mutex<State>>, rx: Receiver<Option<Outgoing>>, wake: Arc<dyn Fn() + Send + Sync>) {
    while let Ok(Some(out)) = rx.recv() {
        // The device id, and with it the session, comes from the first sync. A message typed before that waits for it.
        let session = {
            let existing = state.lock().unwrap_or_else(|p| p.into_inner()).session_id.clone();
            match existing {
                Some(s) => Ok(s),
                None => client.session().map(|s| session_id_for(s.device_id.as_deref().unwrap_or("laptop"))),
            }
        };
        state.lock().unwrap_or_else(|p| p.into_inner()).sending = true;
        wake();
        let result = session.and_then(|s| client.send_chat(&out.text, &s, &out.cmid, out.reply_to.as_deref()));
        {
            let mut st = state.lock().unwrap_or_else(|p| p.into_inner());
            st.sending = false;
            match result {
                Ok(reply) => {
                    st.timeline.apply_reply(&out.local_id, &out.cmid, reply, now_ms());
                }
                Err(e) => {
                    st.timeline.mark_failed(&out.local_id, e.chat_text());
                    drop(st);
                    note_auth(&state, &wake, &e);
                }
            }
        }
        wake();
    }
}

fn fetch_thread(client: Client, conversation: Option<String>, state: Arc<Mutex<State>>, rx: Receiver<Fetch>, wake: Arc<dyn Fn() + Send + Sync>) {
    let idle = |s: &Arc<Mutex<State>>| s.lock().unwrap_or_else(|p| p.into_inner()).timeline.pending_jobs().is_empty();
    if conversation.is_none() {
        match client.session() {
            Ok(s) => {
                let mut st = state.lock().unwrap_or_else(|p| p.into_inner());
                st.session_id = Some(session_id_for(s.device_id.as_deref().unwrap_or("laptop")));
                st.device_id = s.device_id;
                st.device_name = s.device_name;
            }
            Err(e) => note_auth(&state, &wake, &e),
        }
    }
    loop {
        // Unfinished jobs and an OTP's expiry need a beat; otherwise wait for work.
        let timeout = if idle(&state) { Duration::from_secs(5) } else { JOB_POLL };
        let cmd = match rx.recv_timeout(timeout) {
            Ok(c) => Some(c),
            Err(RecvTimeoutError::Timeout) => None,
            Err(RecvTimeoutError::Disconnected) => return,
        };
        match cmd {
            None => {
                if conversation.is_some() {
                    sync(&client, conversation.as_deref(), &state, &wake);
                    continue;
                }
                let jobs = state.lock().unwrap_or_else(|p| p.into_inner()).timeline.pending_jobs();
                let mut changed = false;
                for id in jobs {
                    if let Ok(job) = client.job(&id) {
                        changed |= state.lock().unwrap_or_else(|p| p.into_inner()).timeline.apply_job(job);
                    }
                }
                changed |= state.lock().unwrap_or_else(|p| p.into_inner()).timeline.expire_sensitive(now_ms());
                if changed {
                    wake();
                }
            }
            Some(Fetch::Stop) => return,
            Some(Fetch::Sync) => {
                sync(&client, conversation.as_deref(), &state, &wake);
            }
            Some(Fetch::Decisions) => {
                if conversation.is_some() {
                    continue;
                }
                // Lists, not one request per button: the rate limit is shared by every app.
                if let Ok(pending) = client.decisions(Some("pending"), 50) {
                    let closed = client.decisions(Some("decided"), 50).unwrap_or_default();
                    state.lock().unwrap_or_else(|p| p.into_inner()).decisions = Some(DecisionStates {
                        pending: pending.into_iter().map(|d| d.id).collect(),
                        closed: closed.into_iter().map(|d| (d.id, d.chosen_option)).collect(),
                    });
                    wake();
                }
            }
            Some(Fetch::Older) => older(&client, conversation.as_deref(), &state, &wake),
            Some(Fetch::Secrets) => {
                if conversation.is_some() {
                    continue;
                }
                let r = client.secret_requests();
                let mut st = state.lock().unwrap_or_else(|p| p.into_inner());
                match r {
                    Ok(q) => {
                        st.secret_queue = Some(q);
                        st.secret_queue_failed = false;
                    }
                    Err(_) => st.secret_queue_failed = true,
                }
                drop(st);
                wake();
            }
            Some(Fetch::Catalog) => {
                if let Ok(c) = client.secrets_catalog() {
                    state.lock().unwrap_or_else(|p| p.into_inner()).catalog = Some(c);
                    wake();
                }
            }
            Some(Fetch::Conversations) => {
                if let Ok(c) = client.conversations() {
                    state.lock().unwrap_or_else(|p| p.into_inner()).conversations = c;
                    wake();
                }
            }
            Some(Fetch::Save(save)) => {
                let request = save.request.clone();
                let result = match client.secret_save(&save) {
                    Ok(r) => SaveResult::Saved(r),
                    Err(e) if e.code() == Some("exists") => SaveResult::Exists(save),
                    Err(e) if e.code() == Some("secrets_unconfigured") => SaveResult::Failed("The secrets store is not set up on the server.".into()),
                    Err(e) => SaveResult::Failed(e.to_string()),
                };
                let next = matches!(&result, SaveResult::Saved(r) if r.next_item_id.is_some());
                let saved = matches!(result, SaveResult::Saved(_));
                state.lock().unwrap_or_else(|p| p.into_inner()).save_result = Some((request, result));
                wake();
                if saved {
                    if let Ok(q) = client.secret_requests() {
                        state.lock().unwrap_or_else(|p| p.into_inner()).secret_queue = Some(q);
                    }
                }
                if next {
                    sync(&client, conversation.as_deref(), &state, &wake);
                }
                wake();
            }
            Some(Fetch::Dismiss(id)) => {
                let _ = client.secret_dismiss(&id);
                if let Ok(q) = client.secret_requests() {
                    state.lock().unwrap_or_else(|p| p.into_inner()).secret_queue = Some(q);
                }
                sync(&client, conversation.as_deref(), &state, &wake);
                wake();
            }
        }
    }
}

/// `ChatSync.run`: the steps come from `secretary_chat::sync`, and this only fetches and applies.
fn sync(client: &Client, conversation: Option<&str>, state: &Arc<Mutex<State>>, wake: &Arc<dyn Fn() + Send + Sync>) {
    let cursor = state.lock().unwrap_or_else(|p| p.into_inner()).timeline.cursor.clone();
    let mut plan = SyncPlan::new(cursor);
    let mut outcome = SyncOutcome::Started;
    let mut page = None;
    let mut asked_after = false;
    loop {
        match plan.next(outcome) {
            SyncAction::Fetch { after, limit } => {
                asked_after = after.is_some();
                let q = TimelineQuery { after, conversation: conversation.map(str::to_owned), limit, before: None };
                match client.timeline(&q) {
                    Ok(p) => {
                        outcome = SyncOutcome::Page { cursor: p.cursor.clone(), has_more: p.has_more };
                        page = Some(p);
                    }
                    Err(e) => {
                        let gone = e.status() == Some(410);
                        outcome = if gone { SyncOutcome::Gone410 } else { SyncOutcome::Failed };
                        if !gone {
                            note_auth(state, wake, &e);
                            let mut st = state.lock().unwrap_or_else(|p| p.into_inner());
                            if !st.loaded {
                                st.load_error = Some(e.to_string());
                            }
                            drop(st);
                            wake();
                        }
                    }
                }
            }
            SyncAction::Apply { reset } => {
                let Some(p) = page.take() else { return };
                {
                    let mut st = state.lock().unwrap_or_else(|p| p.into_inner());
                    if reset {
                        st.timeline.reset_server_items();
                    }
                    st.timeline.merge(p.items);
                    if p.cursor.is_some() {
                        st.timeline.cursor = p.cursor;
                    }
                    if !asked_after {
                        st.timeline.note_first_page(p.next_before);
                    }
                    st.loaded = true;
                    st.load_error = None;
                }
                wake();
                outcome = SyncOutcome::Applied;
            }
            SyncAction::Done | SyncAction::Abort => return,
        }
    }
}

fn older(client: &Client, conversation: Option<&str>, state: &Arc<Mutex<State>>, wake: &Arc<dyn Fn() + Send + Sync>) {
    let before = {
        let mut st = state.lock().unwrap_or_else(|p| p.into_inner());
        if st.older_loading || !st.timeline.has_older() {
            return;
        }
        st.older_loading = true;
        st.timeline.older_cursor()
    };
    wake();
    let q = TimelineQuery { before, conversation: conversation.map(str::to_owned), limit: PAGE, after: None };
    let r = client.timeline(&q);
    {
        let mut st = state.lock().unwrap_or_else(|p| p.into_inner());
        st.older_loading = false;
        if let Ok(p) = r {
            st.timeline.merge(p.items);
            st.timeline.note_older_page(p.next_before);
        }
    }
    wake();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clean_message_keeps_what_the_server_takes() {
        assert_eq!(clean_message("  a\r\nb\u{7}\tc \n"), "a\nb\tc");
        assert_eq!(clean_message(" \n "), "");
    }

    #[test]
    fn a_uuid_has_the_servers_shape() {
        let u = new_uuid();
        assert_eq!(u.len(), 36);
        assert_eq!(u.as_bytes()[14], b'4');
        assert!(u.bytes().all(|b| b.is_ascii_hexdigit() || b == b'-'));
        assert_ne!(u, new_uuid());
    }

    #[test]
    fn the_session_id_fits_the_rule() {
        assert_eq!(session_id_for("dev_1"), "juno-dev_1");
        let long = session_id_for(&"x".repeat(100));
        assert!(long.len() <= 64);
        assert_eq!(session_id_for("a b/c"), "juno-abc");
    }
}
