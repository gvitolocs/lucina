//! lucina-app-chat: the main Lucina chat on Juno (`web/src/views/Chat.tsx`, `ios/Secretary/Features/Chat`): history, sending,
//! replies arriving live, rich replies with their buttons, and the paste-and-OK secret card. The other conversations (the
//! iPhone's, Telegram's) can be read from the rail, as in All chats; only this device's own chat sends.
//!
//! The chat is on `lucina_account::engine`. Secret values live in one password field and leave once, through
//! `POST /v1/secrets`; the composer's guard catches a pasted key before it can become a message.
// Juno with no C library: Eyra is the libc and the program start, in Rust (docs/research/juno-pure-rust.md).
#[cfg(all(target_os = "linux", target_env = "gnu", target_arch = "aarch64"))]
extern crate eyra;

mod secret;
mod attach;
mod voice;

use lucina_app::time as tz;
use std::sync::Arc;
use std::time::Duration;

use lucina_account::decisions;
use lucina_account::engine::{Engine, StreamStatus};
use lucina_account::rich::{self, ActionKind, Block, Tone};
use lucina_account::secrets;
use lucina_account::signin::DEVICE_NAME;
use lucina_account::timeline::{Conversation, Delivery, Message};
use lucina_account::ui::SignInView;
use lucina_account::{system, Client};
use lucina_app::{App, Face, Harness, ListState, Rect, Rgb, Sender, TextEdit, Theme, Ui};

use lucina_voice_stream::msg::Purpose;
use secret::SecretPanel;
use voice::VoiceUi;

pub enum Msg {
    /// The engine or the sign-in moved.
    Changed,
    SignedIn,
}

/// The evdev code; the toolkit names only the keys it uses itself.
const KEY_R: u16 = 19;
/// Ctrl+M: a voice note; Ctrl+T: talk to Lucina (a voice command); Esc: cancel the one being recorded.
const KEY_M: u16 = 50;
const KEY_T: u16 = 20;
const KEY_ESC: u16 = 1;

type Wake = Arc<dyn Fn() + Send + Sync>;

/// What a row of the message list draws. Rebuilt from the timeline when it changes.
#[derive(Debug, Clone, PartialEq)]
enum Row {
    Older {
        loading: bool,
    },
    Day(String),
    User {
        id: String,
        text: String,
        time: String,
        quote: Option<String>,
        status: Status,
    },
    Lucina {
        text: String,
        time: Option<String>,
        quote: Option<String>,
    },
    Block(Box<Block>),
    Buttons {
        message: String,
        chosen: Option<String>,
        buttons: Vec<(String, String, ActionKind)>,
    },
    /// A line for what has no form here: attachments, a job, a secret card's state.
    Note {
        text: String,
        tone: NoteTone,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum NoteTone {
    Accent,
    Dim,
    Ok,
    Bad,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Status {
    Sent,
    Sending,
    Failed(String),
}

/// DejaVu has no emoji, so they would be boxes: they go, with the joiners and selectors that hold them together.
fn shown(s: &str) -> String {
    let kept: String = s
        .chars()
        .filter_map(|c| match c as u32 {
            0x2705 => Some('✓'),
            0x274C | 0x274E => Some('✗'),
            0x1F000..=0x1FAFF | 0xFE00..=0xFE0F | 0x200D | 0x20E3 | 0x2B50 | 0x2728 | 0x23F0..=0x23F3 => None,
            _ => Some(c),
        })
        .collect();
    // What went may leave a line starting with a space, or two in a row.
    kept.lines()
        .map(|l| l.split(' ').filter(|w| !w.is_empty()).collect::<Vec<_>>().join(" "))
        .collect::<Vec<_>>()
        .join("\n")
        .trim()
        .to_owned()
}

/// A line of its own for a list item, as Markdown means it: `- x` and `* x` become bullets. Marks go after this, so a leading
/// `*` is not taken for one.
fn bullets(s: &str) -> String {
    s.lines()
        .map(|l| {
            let t = l.trim_start();
            match t.strip_prefix("- ").or_else(|| t.strip_prefix("* ")) {
                Some(rest) => format!("• {rest}"),
                None => l.to_owned(),
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// What a reply says when it has no widgets: its whole text. Bubbles are capped at four, so they may have been cut ("The…"),
/// and older ones lost their line ends; the text never does. The hints for channels without buttons go.
fn whole_text(text: &str) -> String {
    text.lines()
        .filter(|l| !l.trim_start().starts_with('↪'))
        .collect::<Vec<_>>()
        .join("\n")
        .trim_end()
        .to_owned()
}

/// Marks gone, and a link's address kept after its label: nothing opens links here.
fn prose(s: &str) -> String {
    let mut out = String::new();
    for span in rich::inline(&bullets(s)) {
        out.push_str(&span.text);
        if let Some(url) = span.link {
            if url != span.text {
                out.push_str(&format!(" ({url})"));
            }
        }
    }
    shown(&out)
}

fn one_line(s: &str, max: usize) -> String {
    let flat: String = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() <= max {
        return flat;
    }
    let cut: String = flat.chars().take(max).collect();
    format!("{cut}…")
}

const MONTHS: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];

fn civil(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (yoe + era * 400 + i64::from(m <= 2), m, d)
}

fn rows_of(engine: &Engine, zone: &tz::Zone) -> Vec<Row> {
    let st = engine.state();
    let mut rows = Vec::new();
    if st.timeline.has_older() {
        rows.push(Row::Older {
            loading: st.older_loading,
        });
    }
    let quote_of = |id: &Option<String>| -> Option<String> {
        let m = st.timeline.get(id.as_deref()?)?;
        let who = if m.item.lucina { "Lucina" } else { "You" };
        Some(format!("{who}: {}", one_line(&prose(&m.item.text), 90)))
    };
    let mut day = None;
    let messages: Vec<&Message> = st.timeline.messages().collect();
    for m in messages {
        let i = &m.item;
        let secs = i.ts_ms.div_euclid(1000);
        let (local, _) = zone.at(secs);
        let d = local.div_euclid(86_400);
        if day != Some(d) && i.ts_ms > 0 {
            day = Some(d);
            let (y, mo, da) = civil(d);
            rows.push(Row::Day(format!(
                "{da} {} {y}",
                MONTHS[(mo as usize - 1).min(11)]
            )));
        }
        let time = {
            let s = local.rem_euclid(86_400);
            format!("{:02}:{:02}", s / 3600, s % 3600 / 60)
        };
        if !i.lucina {
            let status = match &m.delivery {
                Delivery::Sent => Status::Sent,
                Delivery::Sending => Status::Sending,
                Delivery::Failed(e) => Status::Failed(e.clone()),
            };
            // A voice note shows only "Voice message · 0:05": its legacy "📎 file" text is dropped, its name never shown.
            let user_text = if attach::hides_voice_caption(&i.text, &i.attachments) {
                attach::voice_labels(i).join(", ")
            } else {
                shown(&i.text)
            };
            rows.push(Row::User {
                id: i.id.clone(),
                text: user_text,
                time,
                quote: quote_of(&i.reply_to),
                status,
            });
        } else {
            let quote = None;
            // A decision's question: its heading over the question, without how to answer by text.
            let tidied = decisions::tidy_question(&i.text);
            if let Some((head, _)) = &tidied {
                rows.push(Row::Note { text: head.clone(), tone: NoteTone::Accent });
            }
            let plain = tidied.as_ref().map_or_else(|| whole_text(&i.text), |(_, body)| whole_text(body));
            match &i.rich {
                Some(r) => {
                    if r.blocks.is_empty() && !plain.is_empty() {
                        rows.push(Row::Lucina {
                            text: prose(&plain),
                            time: Some(time.clone()),
                            quote: quote.clone(),
                        });
                    } else {
                        let mut first = true;
                        for b in &r.bubbles {
                            let t = if first { Some(time.clone()) } else { None };
                            first = false;
                            rows.push(Row::Lucina {
                                text: prose(b),
                                time: t,
                                quote: quote.clone(),
                            });
                        }
                    }
                    for b in &r.blocks {
                        rows.push(Row::Block(Box::new(b.clone())));
                    }
                    if let Some(request) = r.secret_request() {
                        rows.push(secret_note(
                            st.secret_queue.as_ref(),
                            st.secret_queue_failed,
                            request,
                        ));
                    }
                    let buttons: Vec<_> = r
                        .actions
                        .iter()
                        .filter(|a| {
                            !matches!(
                                &a.kind,
                                ActionKind::Secret {
                                    request: Some(_),
                                    ..
                                }
                            )
                        })
                        .map(|a| (a.id.clone(), shown(&a.label), a.kind.clone()))
                        .collect();
                    // A decision answered anywhere, on the phone say, has nothing left to press. Nor has a secret card:
                    // the form above the composer is where it is answered, and its row says how it ended.
                    let over = r.decision().and_then(|id| st.decisions.as_ref()?.closed(id));
                    if let Some(choice) = over {
                        let text = match choice {
                            Some(c) => format!("Decided: {}", shown(c)),
                            None => "This decision is closed.".to_owned(),
                        };
                        rows.push(Row::Note { text, tone: NoteTone::Ok });
                    } else if !buttons.is_empty() && r.secret_request().is_none() {
                        rows.push(Row::Buttons {
                            message: i.id.clone(),
                            chosen: st.timeline.choice_of(&i.id),
                            buttons,
                        });
                    }
                }
                None if !plain.is_empty() => rows.push(Row::Lucina {
                    text: prose(&plain),
                    time: Some(time),
                    quote,
                }),
                None => {}
            }
        }
        let caption_hidden = !i.lucina && attach::hides_voice_caption(&i.text, &i.attachments);
        for (n, (kind, name)) in i.attachments.iter().enumerate() {
            if kind == "voice" {
                // Already the user bubble when the caption is hidden; otherwise a line without the file name.
                if !caption_hidden {
                    rows.push(Row::Note {
                        text: attach::voice_label(i.attachment_ms.get(n).copied().flatten()),
                        tone: NoteTone::Dim,
                    });
                }
                continue;
            }
            rows.push(Row::Note { text: attach::file_note(kind, name.as_deref()), tone: NoteTone::Dim });
        }
        if let Some(job) = &m.job {
            let (text, tone) = match (job.status.as_str(), &job.error) {
                ("done", _) => (
                    format!("{} finished.", job.action.as_deref().unwrap_or("The job")),
                    NoteTone::Ok,
                ),
                ("error", Some(e)) if e == "interrupted" => (
                    "The job stopped: Lucina restarted.".to_owned(),
                    NoteTone::Bad,
                ),
                ("error", e) => (
                    format!(
                        "The job failed: {}",
                        e.as_deref().unwrap_or("no reason given")
                    ),
                    NoteTone::Bad,
                ),
                (s, _) => (
                    format!("{}: {s}…", job.action.as_deref().unwrap_or("Job")),
                    NoteTone::Dim,
                ),
            };
            rows.push(Row::Note { text, tone });
        }
    }
    rows
}

/// `SecretCard`'s closed states, as a line where the card was. The open card sits above the composer.
fn secret_note(queue: Option<&secrets::SecretQueue>, failed: bool, request: &str) -> Row {
    let (text, tone) = match (queue, queue.and_then(|q| q.find(request))) {
        (None, _) if failed => (
            "Could not read the secret requests.".to_owned(),
            NoteTone::Bad,
        ),
        (None, _) => ("Loading…".to_owned(), NoteTone::Dim),
        (Some(_), None) => ("This secret request is closed.".to_owned(), NoteTone::Dim),
        (Some(_), Some(e)) => match e.status.as_str() {
            "active" => (
                format!("Enter the secret for {} below.", e.name),
                NoteTone::Dim,
            ),
            "queued" => (format!("{} is waiting its turn.", e.name), NoteTone::Dim),
            "saved" => {
                let verb = if e.action.as_deref() == Some("updated") {
                    "Updated"
                } else {
                    "Saved"
                };
                (
                    format!(
                        "{verb} {} (fingerprint {}).",
                        e.name,
                        e.fingerprint.as_deref().unwrap_or("?")
                    ),
                    NoteTone::Ok,
                )
            }
            "dismissed" => (
                format!(
                    "Skipped {}.",
                    if e.name.is_empty() {
                        "a secret"
                    } else {
                        &e.name
                    }
                ),
                NoteTone::Dim,
            ),
            _ => (
                format!("The request for {} expired.", e.name),
                NoteTone::Dim,
            ),
        },
    };
    Row::Note { text, tone }
}

const BODY: f32 = 15.0;
const META: f32 = 12.0;

struct Sizes {
    body: u16,
    meta: u16,
    gap: i32,
    pad: i32,
}

fn sizes(t: &Theme) -> Sizes {
    Sizes {
        body: t.pt(BODY),
        meta: t.pt(META),
        gap: t.px(8.0),
        pad: t.px(10.0),
    }
}

/// The widest a bubble grows in a list row `w` wide.
fn bubble_width(w: i32, t: &Theme) -> i32 {
    (w * 3 / 4).min(t.px(640.0)).max(t.px(120.0))
}

fn block_lines(b: &Block) -> Vec<(Face, String)> {
    let mut out = Vec::new();
    let title = |out: &mut Vec<(Face, String)>, t: &Option<String>| {
        if let Some(t) = t {
            out.push((Face::Bold, prose(t)));
        }
    };
    match b {
        Block::Checklist { title: t, items } => {
            title(&mut out, t);
            for (text, done) in items {
                out.push((
                    Face::Sans,
                    format!("{} {}", if *done { "☑" } else { "☐" }, prose(text)),
                ));
            }
        }
        Block::Card {
            emoji: _,
            title: t,
            subtitle,
            body,
            tone: _,
        } => {
            out.push((Face::Bold, prose(t)));
            if let Some(s) = subtitle {
                out.push((Face::Sans, shown(s)));
            }
            if let Some(s) = body {
                out.push((Face::Sans, prose(s)));
            }
        }
        Block::Stat { items } => {
            for (label, value, delta) in items {
                let delta = delta
                    .as_ref()
                    .map(|d| format!("  ({d})"))
                    .unwrap_or_default();
                out.push((Face::Sans, format!("{}: {value}{delta}", shown(label))));
            }
        }
        Block::List { title: t, items } => {
            title(&mut out, t);
            for (_, text, meta) in items.iter().take(rich::LIST_VISIBLE) {
                let meta = meta.as_ref().map(|m| format!(" · {m}")).unwrap_or_default();
                out.push((Face::Sans, format!("• {}{meta}", prose(text))));
            }
            if items.len() > rich::LIST_VISIBLE {
                out.push((
                    Face::Sans,
                    format!("and {} more", items.len() - rich::LIST_VISIBLE),
                ));
            }
        }
        Block::Table {
            title: t,
            columns,
            rows,
        } => {
            title(&mut out, t);
            out.push((Face::Bold, columns.join(" | ")));
            for r in rows {
                out.push((Face::Sans, r.join(" | ")));
            }
        }
        Block::Steps { title: t, items } => {
            title(&mut out, t);
            for (i, s) in items.iter().enumerate() {
                out.push((Face::Sans, format!("{}. {}", i + 1, prose(s))));
            }
        }
        Block::Quote { text, cite } => {
            out.push((Face::Sans, format!("“{}”", prose(text))));
            if let Some(c) = cite {
                out.push((Face::Sans, format!("— {c}")));
            }
        }
        Block::Link { url, title: t } => {
            out.push((Face::Bold, shown(t)));
            out.push((Face::Sans, url.clone()));
        }
        Block::Place {
            title: t,
            address,
            when,
        } => {
            out.push((Face::Bold, shown(t)));
            if let Some(w) = when {
                out.push((Face::Sans, w.clone()));
            }
            if let Some(a) = address {
                out.push((Face::Sans, a.clone()));
            }
            out.push((Face::Sans, "The map opens on your phone.".to_owned()));
        }
        Block::Image { alt, caption } => {
            out.push((
                Face::Sans,
                format!("Picture: {}", caption.as_ref().unwrap_or(alt)),
            ));
        }
    }
    out
}

fn row_height(text: &mut lucina_app::gfx::Text, row: &Row, w: i32, t: &Theme) -> i32 {
    let s = sizes(t);
    let wrap =
        |text: &mut lucina_app::gfx::Text, f, px, c: &str, w| Ui::wrapped_height(text, f, px, c, w);
    let bw = bubble_width(w, t) - 2 * s.pad;
    match row {
        Row::Older { .. } => t.px(34.0),
        Row::Day(_) => t.px(30.0),
        Row::User {
            text: c,
            quote,
            status,
            ..
        } => {
            let mut h = wrap(text, Face::Sans, s.body, c, bw) + 2 * s.pad + s.gap;
            if let Some(q) = quote {
                h += wrap(text, Face::Sans, s.meta, q, bw) + s.gap / 2;
            }
            if matches!(status, Status::Failed(_)) {
                h += t.px(36.0);
            }
            h + t.px(16.0)
        }
        Row::Lucina { text: c, .. } => {
            wrap(text, Face::Sans, s.body, c, bw) + 2 * s.pad + s.gap + t.px(16.0)
        }
        Row::Block(b) => {
            let mut h = 2 * s.pad + s.gap;
            for (f, l) in block_lines(b) {
                h += wrap(text, f, s.body, &l, bw);
            }
            h
        }
        Row::Buttons { .. } => t.px(44.0),
        Row::Note { text: c, .. } => wrap(text, Face::Sans, s.meta, c, bw) + s.gap,
    }
}

/// A bubble shows it is selected itself, so there is no band across the screen.
fn messages_list() -> ListState {
    let mut list = ListState::new();
    list.plain = true;
    list
}

/// A signed-in chat, or the way there.
enum Screen {
    Starting,
    SignIn(SignInView),
    Chat(Box<ChatScreen>),
}

struct ChatScreen {
    client: Client,
    main: Engine,
    /// A conversation being read instead of the main one.
    reading: Option<(String, String, Engine)>,
    rows: Vec<Row>,
    list: ListState,
    follow: bool,
    last_scroll: i32,
    last_height: i32,
    layout_changed: bool,
    composer: TextEdit,
    reply_to: Option<(String, String)>,
    /// The composer's text looks like it holds a secret: `(byte spans)`.
    guard: Option<Vec<(usize, usize)>>,
    secret: SecretPanel,
    note: Option<String>,
    rail: ListState,
    focus_composer: bool,
    /// Voice notes and commands; `None` in tests and demos, which have no voice server.
    voice: Option<VoiceUi>,
}

struct Chat {
    screen: Screen,
    tx: Option<Sender<Msg>>,
    zone: tz::Zone,
    /// Lets a test or a demo run without the shell's account.
    client_for_tests: Option<Client>,
}

fn wake_of(tx: &Option<Sender<Msg>>) -> Wake {
    match tx {
        Some(tx) => {
            let tx = tx.clone();
            Arc::new(move || tx.send(Msg::Changed))
        }
        None => Arc::new(|| {}),
    }
}

impl Chat {
    fn new(tx: Option<Sender<Msg>>, client_for_tests: Option<Client>) -> Chat {
        let (_, zone) = tz::load();
        let mut chat = Chat {
            screen: Screen::Starting,
            tx,
            zone,
            client_for_tests,
        };
        chat.open();
        // What the RAM cache held is on the first frame.
        chat.refresh();
        chat
    }

    /// With a token: the chat. Without: sign-in.
    fn open(&mut self) {
        if let Some(c) = &self.client_for_tests {
            self.screen = Screen::Chat(Box::new(ChatScreen::new(c.clone(), &self.tx, false)));
            return;
        }
        match system::client() {
            Some(c) => self.screen = Screen::Chat(Box::new(ChatScreen::new(c, &self.tx, true))),
            None => self.show_sign_in(),
        }
    }

    fn show_sign_in(&mut self) {
        let changed = wake_of(&self.tx);
        let done: Wake = match &self.tx {
            Some(tx) => {
                let tx = tx.clone();
                Arc::new(move || tx.send(Msg::SignedIn))
            }
            None => Arc::new(|| {}),
        };
        self.screen = Screen::SignIn(SignInView::start(changed, done));
    }

    fn refresh(&mut self) {
        let signed_out = match &self.screen {
            Screen::Chat(c) => c.main.state().signed_out && self.client_for_tests.is_none(),
            _ => false,
        };
        if signed_out {
            // The service asks the server. If the token was good after all, the screen says so at once and the chat comes back.
            system::report_rejected();
            self.show_sign_in();
            return;
        }
        if let Screen::Chat(c) = &mut self.screen {
            c.refresh(&self.zone);
        }
    }
}

impl ChatScreen {
    fn new(client: Client, tx: &Option<Sender<Msg>>, with_voice: bool) -> ChatScreen {
        let main = Engine::start(client.clone(), None, wake_of(tx));
        // The voice socket opens with the chat window and stays while it shows.
        let voice = if with_voice { VoiceUi::start(&client, wake_of(tx)) } else { None };
        main.load_conversations();
        ChatScreen {
            client,
            main,
            reading: None,
            rows: Vec::new(),
            list: messages_list(),
            follow: true,
            last_scroll: 0,
            last_height: 0,
            layout_changed: false,
            composer: TextEdit::new().placeholder("Message Lucina"),
            reply_to: None,
            guard: None,
            secret: SecretPanel::new(),
            note: None,
            rail: ListState::new(),
            // The field to type in, from the first key on.
            focus_composer: true,
            voice,
        }
    }

    fn engine(&self) -> &Engine {
        self.reading.as_ref().map_or(&self.main, |(_, _, e)| e)
    }

    fn refresh(&mut self, zone: &tz::Zone) {
        if let Some(v) = &mut self.voice {
            v.pump();
        }
        let mut rows = rows_of(self.engine(), zone);
        if self.reading.is_none() {
            self.add_voice_rows(&mut rows);
        }
        if rows != self.rows {
            self.layout_changed = true;
            let prepended = !self.rows.is_empty()
                && rows.len() > self.rows.len()
                && rows.last() == self.rows.last();
            let appended = self.rows.len() < rows.len() && rows.starts_with(&self.rows);
            // A row in the middle changed height, or older ones came in above.
            if !appended {
                self.list.invalidate();
            }
            if prepended {
                let added = rows.len() - self.rows.len();
                self.list.select(added);
            }
            self.rows = rows;
            if self.follow {
                self.list.scroll_to_end();
            }
        }
        let (queue, no_catalog) = {
            let st = self.main.state();
            (st.secret_queue.clone(), st.catalog.is_none())
        };
        self.secret.sync(queue.as_ref());
        if no_catalog && self.secret.needs_catalog() {
            self.main.load_catalog();
        }
    }

    /// The owner's transcript and Lucina's pending reply, after the real rows, until the timeline shows the real ones.
    fn add_voice_rows(&self, rows: &mut Vec<Row>) {
        let Some(v) = &self.voice else { return };
        let recent: Vec<&Row> = rows.iter().rev().take(8).collect();
        let users: Vec<&str> = recent.iter().filter_map(|r| if let Row::User { text, .. } = r { Some(text.as_str()) } else { None }).collect();
        let lucina: Vec<&str> = recent.iter().filter_map(|r| if let Row::Lucina { text, .. } = r { Some(text.as_str()) } else { None }).collect();
        let (user, reply) = v.transient(&users, &lucina);
        if let Some(text) = user {
            rows.push(Row::User { id: "voice-transcript".into(), text, time: String::new(), quote: None, status: Status::Sent });
        }
        if let Some(text) = reply {
            rows.push(Row::Lucina { text, time: None, quote: None });
        }
    }

    fn voice_key(&mut self, p: lucina_app::keys::Press) -> bool {
        let Some(v) = &mut self.voice else { return false };
        if p.mods.ctrl && p.code == KEY_M {
            v.toggle(Purpose::Note);
            true
        } else if p.mods.ctrl && p.code == KEY_T {
            v.toggle(Purpose::Command);
            true
        } else if p.code == KEY_ESC {
            v.cancel()
        } else {
            false
        }
    }

    fn try_send(&mut self) {
        let text = self.composer.text.clone();
        if text.trim().is_empty() {
            return;
        }
        // `/secret NAME VALUE`: the panel, never a message, never a bubble.
        if secrets::is_command(&text) {
            let mut words = text.trim_start()[7..].split_whitespace();
            self.secret.open_manual(words.next(), words.next());
            self.composer.clear();
            return;
        }
        let spans = secrets::detect(&text);
        if !spans.is_empty() {
            self.guard = Some(spans);
            return;
        }
        self.send_now(&text);
    }

    fn send_now(&mut self, text: &str) {
        let reply = self.reply_to.take().map(|r| r.0);
        if self.main.send(text, reply) {
            self.composer.clear();
            self.follow = true;
            self.note = None;
        } else {
            self.note = Some(format!(
                "Too long: at most {} characters.",
                lucina_account::engine::CHAT_MAX_CHARS
            ));
        }
    }

    fn press_button(&mut self, message: &str, id: &str, kind: &ActionKind) {
        match kind {
            ActionKind::Reply {
                text,
                prefill: true,
            } => {
                self.composer.set(&format!("{text} "));
            }
            ActionKind::Reply {
                text,
                prefill: false,
            } => {
                let t = text.clone();
                self.send_now(&t);
            }
            ActionKind::Confirm { answer, text } => match (answer.as_str(), text) {
                ("edit", Some(t)) => self.composer.set(&format!("{t} ")),
                (_, Some(t)) => {
                    let t = t.clone();
                    self.send_now(&t);
                }
                _ => {}
            },
            ActionKind::Dismiss => {}
            ActionKind::Route { path } => {
                self.note = Some(format!(
                    "{path} is not on this laptop yet: open it on your phone."
                ));
                return;
            }
            ActionKind::Secret { name, .. } => {
                self.secret.open_manual(name.as_deref(), None);
                return;
            }
        }
        if let Some(m) = self.main.state().timeline.get_mut(message) {
            m.chosen = Some(id.to_owned());
        }
    }
}

fn draw_row(ui: &mut Ui, row: &Row, selected: bool) -> Option<RowAction> {
    let t = ui.theme();
    let s = sizes(&t);
    let a = ui.area();
    let bw = bubble_width(a.w, &t);
    let line = |ui: &mut Ui, r: Rect, px: u16, c: Rgb, f: Face, text: &str| {
        ui.within(r, |ui| ui.wrapped(f, px, c, text));
    };
    let mut action = None;
    match row {
        Row::Older { loading } => {
            let label = if *loading {
                "Loading…"
            } else {
                "Earlier messages"
            };
            let r = Rect::new(
                a.x + (a.w - t.px(220.0)) / 2,
                a.y + t.px(3.0),
                t.px(220.0),
                t.px(28.0),
            );
            if ui.button_at(r, "older", label) {
                action = Some(RowAction::Older);
            }
        }
        Row::Day(d) => {
            let w = ui.measure(Face::Sans, s.meta, d);
            ui.text_in(
                Rect::new(a.x + (a.w - w) / 2, a.y, w + 2, a.h),
                Face::Sans,
                s.meta,
                t.dim,
                d,
            );
        }
        Row::User {
            id,
            text,
            time,
            quote,
            status,
        } => {
            let inner = bw - 2 * s.pad;
            let qh = quote.as_ref().map_or(0, |q| {
                ui_wrapped(ui, Face::Sans, s.meta, q, inner) + s.gap / 2
            });
            let th = ui_wrapped(ui, Face::Sans, s.body, text, inner);
            let bubble_w = bubble_for(ui, text, inner, bw);
            let r = Rect::new(
                a.right() - bubble_w - t.px(4.0),
                a.y + s.gap / 2,
                bubble_w,
                th + qh + 2 * s.pad,
            );
            ui.fill_round(
                r,
                t.px(10.0),
                if selected { t.selection } else { t.panel_hi },
            );
            let mut y = r.y + s.pad;
            if let Some(q) = quote {
                line(
                    ui,
                    Rect::new(r.x + s.pad, y, inner, qh),
                    s.meta,
                    t.dim,
                    Face::Sans,
                    q,
                );
                y += qh;
            }
            line(
                ui,
                Rect::new(r.x + s.pad, y, inner, th),
                s.body,
                t.text,
                Face::Sans,
                text,
            );
            let meta_y = r.bottom() + t.px(1.0);
            let meta = match status {
                Status::Sending => "Sending…".to_owned(),
                Status::Sent => time.clone(),
                Status::Failed(e) => e.clone(),
            };
            let colour = if matches!(status, Status::Failed(_)) {
                t.danger
            } else {
                t.dim
            };
            let w = ui.measure(Face::Sans, s.meta, &meta).min(a.w);
            ui.text_in(
                Rect::new(a.right() - w - t.px(4.0), meta_y, w, t.px(16.0)),
                Face::Sans,
                s.meta,
                colour,
                &meta,
            );
            if matches!(status, Status::Failed(_)) {
                let by = meta_y + t.px(17.0);
                let bh = t.px(26.0);
                if ui.button_at(
                    Rect::new(a.right() - t.px(180.0), by, t.px(88.0), bh),
                    ("retry", id_hash(id)),
                    "Try again",
                ) {
                    action = Some(RowAction::Retry(id.clone()));
                }
                if ui.button_at(
                    Rect::new(a.right() - t.px(86.0), by, t.px(82.0), bh),
                    ("discard", id_hash(id)),
                    "Discard",
                ) {
                    action = Some(RowAction::Discard(id.clone()));
                }
            }
        }
        Row::Lucina { text, time, .. } => {
            let inner = bw - 2 * s.pad;
            let th = ui_wrapped(ui, Face::Sans, s.body, text, inner);
            let bubble_w = bubble_for(ui, text, inner, bw);
            let r = Rect::new(a.x + t.px(4.0), a.y + s.gap / 2, bubble_w, th + 2 * s.pad);
            ui.fill_round(r, t.px(10.0), if selected { t.selection } else { t.panel });
            line(
                ui,
                Rect::new(r.x + s.pad, r.y + s.pad, inner, th),
                s.body,
                t.text,
                Face::Sans,
                text,
            );
            if let Some(time) = time {
                let w = ui.measure(Face::Sans, s.meta, time);
                ui.text_in(
                    Rect::new(r.x + t.px(2.0), r.bottom() + t.px(1.0), w + 2, t.px(16.0)),
                    Face::Sans,
                    s.meta,
                    t.dim,
                    time,
                );
            }
        }
        Row::Block(b) => {
            let inner = bw - 2 * s.pad;
            let lines = block_lines(b);
            let mut h = 0;
            let heights: Vec<i32> = lines
                .iter()
                .map(|(f, l)| {
                    let x = ui_wrapped(ui, *f, s.body, l, inner);
                    h += x;
                    x
                })
                .collect();
            let r = Rect::new(a.x + t.px(4.0), a.y + s.gap / 2, bw, h + 2 * s.pad);
            let edge = match &**b {
                Block::Card {
                    tone: Tone::Warning,
                    ..
                } => t.danger,
                Block::Card {
                    tone: Tone::Success,
                    ..
                } => t.ok,
                _ => t.accent,
            };
            ui.fill_round(r, t.px(8.0), t.panel);
            ui.fill(
                Rect::new(r.x, r.y + t.px(4.0), t.px(3.0), r.h - t.px(8.0)),
                edge,
            );
            let mut y = r.y + s.pad;
            for ((f, l), x) in lines.iter().zip(heights) {
                line(
                    ui,
                    Rect::new(r.x + s.pad, y, inner, x),
                    s.body,
                    if *f == Face::Bold { t.text } else { t.item },
                    *f,
                    l,
                );
                y += x;
            }
        }
        Row::Buttons {
            message,
            chosen,
            buttons,
        } => {
            let mut x = a.x + t.px(4.0);
            let by = a.y + t.px(6.0);
            let bh = t.px(30.0);
            for (id, label, kind) in buttons {
                let w = ui.measure(Face::Sans, t.pt(14.0), label) + t.px(26.0);
                let r = Rect::new(x, by, w, bh);
                match chosen {
                    // The row is answered: the choice is marked and nothing can be pressed.
                    Some(c) => {
                        let on = c == id;
                        ui.fill_round(r, t.px(6.0), if on { t.selection } else { t.bg });
                        ui.text_in(
                            Rect::new(r.x + t.px(13.0), r.y, w, bh),
                            Face::Sans,
                            t.pt(14.0),
                            if on { t.text } else { t.dim },
                            label,
                        );
                    }
                    None => {
                        if ui.button_at(r, ("action", id_hash(&format!("{message}/{id}"))), label) {
                            action =
                                Some(RowAction::Button(message.clone(), id.clone(), kind.clone()));
                        }
                    }
                }
                x += w + t.px(8.0);
            }
        }
        Row::Note { text, tone } => {
            let c = match tone {
                NoteTone::Accent => t.accent,
                NoteTone::Dim => t.dim,
                NoteTone::Ok => t.ok,
                NoteTone::Bad => t.danger,
            };
            line(
                ui,
                Rect::new(a.x + t.px(8.0), a.y + s.gap / 2, bw, a.h),
                s.meta,
                c,
                Face::Sans,
                text,
            );
        }
    }
    action
}

fn ui_wrapped(ui: &mut Ui, f: Face, px: u16, s: &str, w: i32) -> i32 {
    let lh = ui.line_height(f, px);
    lh * ui.wrap_count(f, px, s, w)
}

/// A bubble as wide as its text, up to the limit when it wraps.
fn bubble_for(ui: &mut Ui, text: &str, inner: i32, bw: i32) -> i32 {
    let t = ui.theme();
    let s = sizes(&t);
    if ui.wrap_count(Face::Sans, s.body, text, inner) > 1 {
        return bw;
    }
    (ui.measure(Face::Sans, s.body, text).min(inner) + 2 * s.pad)
        .max(t.px(90.0))
        .min(bw)
}

fn id_hash(s: &str) -> usize {
    s.bytes().fold(0xcbf29ce484222325u64, |h, b| {
        (h ^ u64::from(b)).wrapping_mul(0x100000001b3)
    }) as usize
}

enum RowAction {
    Older,
    Retry(String),
    Discard(String),
    Button(String, String, ActionKind),
}

impl App for Chat {
    type Msg = Msg;

    fn message(&mut self, m: Msg) {
        match m {
            Msg::Changed => self.refresh(),
            Msg::SignedIn => {
                self.open();
                self.refresh();
            }
        }
    }

    fn on_quit(&mut self) {
        // A value in the secret field goes with the process; the last messages stay in RAM for the next opening.
        if let Screen::Chat(c) = &mut self.screen {
            c.secret.wipe();
            c.main.save_cache();
        }
    }

    fn view(&mut self, ui: &mut Ui) {
        match &mut self.screen {
            Screen::SignIn(view) => {
                ui.header("Lucina");
                view.view(ui, &format!("Lucina's chat here is the one on your phone. This laptop asks to be added as “{DEVICE_NAME}”."));
            }
            Screen::Chat(c) => view_chat(ui, c),
            Screen::Starting => {}
        }
    }
}

fn status_text(s: StreamStatus) -> (&'static str, fn(&Theme) -> Rgb) {
    match s {
        StreamStatus::Connecting => ("Connecting", |t| t.dim),
        StreamStatus::Live => ("Live", |t| t.ok),
        StreamStatus::Reconnecting => ("Reconnecting", |t| t.accent),
        StreamStatus::Offline => ("Offline", |t| t.danger),
    }
}

fn view_chat(ui: &mut Ui, c: &mut ChatScreen) {
    let t = ui.theme();
    ui.header("Lucina");
    let rail = ui.cut_left(t.px(230.0));
    view_rail(ui, c, rail);

    let reading = c.reading.as_ref().map(|(_, title, _)| title.clone());
    // The strip above the messages: which conversation, and the connection.
    let top = ui.cut_top(t.px(30.0));
    let (loaded, load_error) = {
        let st = c.engine().state();
        (st.loaded, st.load_error.clone())
    };
    let sending = c.main.state().sending;
    {
        let title = reading.clone().unwrap_or_else(|| "This laptop".to_owned());
        ui.fill(top, t.panel);
        ui.text_in(
            Rect::new(top.x + t.px(10.0), top.y, top.w / 2, top.h),
            Face::Bold,
            t.pt(13.0),
            t.text,
            &title,
        );
        let (word, colour) = status_text(c.main.state().status);
        let word = if sending {
            "Lucina is answering…"
        } else {
            word
        };
        let w = ui.measure(Face::Sans, t.pt(12.0), word);
        ui.text_in(
            Rect::new(top.right() - w - t.px(12.0), top.y, w + 2, top.h),
            Face::Sans,
            t.pt(12.0),
            colour(&t),
            word,
        );
    }

    let composing = reading.is_none();
    if composing {
        let bottom_h = composer_height(c, &t);
        let bottom = ui.cut_bottom(bottom_h);
        view_bottom(ui, c, bottom);
        // Ctrl+M a voice note, Ctrl+T talk, Esc cancels the one being recorded.
        if let Some(p) = ui.press() {
            if c.voice_key(p) {
                ui.take_press();
            }
        }
    }

    let area = ui.area();
    let area = Rect::new(
        area.x + t.px(10.0),
        area.y + t.px(6.0),
        area.w - t.px(20.0),
        area.h - t.px(6.0),
    );
    if !loaded && c.rows.is_empty() {
        ui.within(area, |ui| match load_error {
            Some(e) => {
                ui.wrapped(Face::Sans, t.pt(15.0), t.danger, &e);
                if ui.button("Try again") {
                    c.main.resync();
                }
            }
            None => ui.dim("Loading the chat…"),
        });
        return;
    }
    if loaded && c.rows.is_empty() {
        ui.within(area, |ui| ui.dim("No messages yet."));
        return;
    }
    let mut action = None;
    let rows = &c.rows;
    let theme = t;
    ui.list_measured(
        "messages",
        &mut c.list,
        area,
        rows.len(),
        |text, i, w| row_height(text, &rows[i], w, &theme),
        |ui, i, selected| {
            if let Some(a) = draw_row(ui, &rows[i], selected) {
                action = Some(a);
            }
        },
    );
    // Scrolling up stops following; a send, or the button, starts it again.
    // Only the owner's own scrolling counts: rows or room changing also moves the position.
    let moved_by_owner = !std::mem::take(&mut c.layout_changed) && area.h == c.last_height;
    if moved_by_owner && c.list.scroll < c.last_scroll && c.list.scroll != 0 {
        c.follow = false;
    }
    c.last_scroll = c.list.scroll;
    c.last_height = area.h;
    if !c.follow {
        let r = Rect::new(
            area.right() - t.px(120.0),
            area.bottom() - t.px(34.0),
            t.px(110.0),
            t.px(28.0),
        );
        if ui.button_at(r, "latest", "Latest") {
            c.follow = true;
            c.list.scroll_to_end();
            c.list.selected = None;
        }
    }
    if c.list.scroll == 0 && c.engine().state().timeline.has_older() && !c.rows.is_empty() {
        c.engine().load_older();
    }
    if let Some(i) = c.list.selected {
        // Ctrl+R on a message replies to it.
        if let Some(p) = ui.press() {
            if p.mods.ctrl && p.code == KEY_R && composing {
                if let Some((id, quote)) = reply_target(c, i) {
                    c.reply_to = Some((id, quote));
                }
                ui.take_press();
            }
        }
    }
    match action {
        Some(RowAction::Older) => c.engine().load_older(),
        Some(RowAction::Retry(id)) => {
            c.main.retry(&id);
        }
        Some(RowAction::Discard(id)) => c.main.state().timeline.discard_local(&id),
        Some(RowAction::Button(m, id, kind)) => c.press_button(&m, &id, &kind),
        None => {}
    }
}

fn reply_target(c: &ChatScreen, i: usize) -> Option<(String, String)> {
    let st = c.main.state();
    let msg_of_row = |row: &Row| -> Option<(String, String)> {
        match row {
            Row::User { id, text, .. } => Some((id.clone(), one_line(text, 80))),
            _ => None,
        }
    };
    if let Some(found) = c.rows.get(i).and_then(msg_of_row) {
        return Some(found);
    }
    // A Lucina row: the newest of hers at or before this one.
    let rows_up_to = c.rows.get(..=i)?;
    let lucina = rows_up_to.iter().rev().find_map(|r| match r {
        Row::Lucina { text, .. } => Some(text.clone()),
        _ => None,
    })?;
    st.timeline
        .messages()
        .filter(|m| m.item.lucina)
        .find(|m| {
            prose(&m.item.text).contains(&lucina)
                || m.item
                    .rich
                    .as_ref()
                    .is_some_and(|r| r.bubbles.iter().any(|b| prose(b) == lucina))
        })
        .map(|m| (m.item.id.clone(), one_line(&lucina, 80)))
}

fn composer_height(c: &ChatScreen, t: &Theme) -> i32 {
    let mut h = t.px(52.0);
    if c.reply_to.is_some() {
        h += t.px(24.0);
    }
    if c.note.is_some() {
        h += t.px(22.0);
    }
    if c.voice.as_ref().is_some_and(|v| v.line.is_some()) {
        h += t.px(22.0);
    }
    if c.guard.is_some() {
        h += t.px(70.0);
    }
    h + c.secret.height(t)
}

fn view_bottom(ui: &mut Ui, c: &mut ChatScreen, r: Rect) {
    let t = ui.theme();
    ui.fill(r, t.panel);
    ui.within(r, |ui| {
        let panel_h = c.secret.height(&t);
        if panel_h > 0 {
            let pr = ui.cut_top(panel_h);
            ui.within(pr, |ui| {
                c.secret.view(ui, &c.main);
            });
        }
        if let Some(spans) = c.guard.clone() {
            let g = ui.cut_top(t.px(70.0));
            ui.fill(g, t.bar);
            let inner = Rect::new(
                g.x + t.px(10.0),
                g.y + t.px(4.0),
                g.w - t.px(20.0),
                g.h - t.px(8.0),
            );
            ui.within(inner, |ui| {
                ui.label("That looks like a secret. It has not been sent.");
                ui.row(30.0, |ui| {
                    if ui.primary_button("Save it as a secret") {
                        let text = c.composer.text.clone();
                        let (a, b) = spans[0];
                        c.secret.open_with_value(&text[a..b]);
                        let masked = secrets::mask(&text, &spans);
                        c.composer.set(&masked);
                        c.guard = None;
                    }
                    if ui.button("Send it masked") {
                        let masked = secrets::mask(&c.composer.text, &spans);
                        c.guard = None;
                        c.send_now(&masked);
                    }
                    if ui.button("Edit") {
                        c.guard = None;
                    }
                });
            });
        }
        if let Some(n) = c.note.clone() {
            let nr = ui.cut_top(t.px(22.0));
            ui.text_in(
                Rect::new(nr.x + t.px(12.0), nr.y, nr.w - t.px(24.0), nr.h),
                Face::Sans,
                t.pt(12.0),
                t.accent,
                &n,
            );
        }
        if let Some(n) = c.voice.as_ref().and_then(|v| v.line.clone()) {
            let nr = ui.cut_top(t.px(22.0));
            ui.text_in(
                Rect::new(nr.x + t.px(12.0), nr.y, nr.w - t.px(24.0), nr.h),
                Face::Sans,
                t.pt(12.0),
                t.accent,
                &n,
            );
        }
        if let Some((_, q)) = c.reply_to.clone() {
            let qr = ui.cut_top(t.px(24.0));
            ui.text_in(
                Rect::new(qr.x + t.px(12.0), qr.y, qr.w - t.px(120.0), qr.h),
                Face::Sans,
                t.pt(12.0),
                t.dim,
                &format!("Replying to: {q}"),
            );
            if ui.button_at(
                Rect::new(qr.right() - t.px(84.0), qr.y + 1, t.px(72.0), qr.h - 2),
                "unreply",
                "Cancel",
            ) {
                c.reply_to = None;
            }
        }
        let row = ui.area();
        let pad = t.px(9.0);
        let send_w = t.px(76.0);
        let fh = t.px(34.0);
        // The voice controls sit between the field and Send; without a voice socket (tests, demos) the field takes their room.
        let voice_w = if c.voice.is_some() { t.px(64.0) } else { 0 };
        let voice_gap = if c.voice.is_some() { 2 * (voice_w + pad) } else { 0 };
        let field = Rect::new(row.x + pad, row.y + pad, row.w - 3 * pad - send_w - voice_gap, fh);
        if std::mem::take(&mut c.focus_composer) {
            ui.focus("composer");
        }
        let res = ui.text_field_at(field, "composer", &mut c.composer);
        if res.changed {
            c.guard = None;
        }
        if let Some(v) = &mut c.voice {
            let recording = v.recording();
            let mic = Rect::new(field.right() + pad, row.y + pad, voice_w, fh);
            let talk = Rect::new(mic.right() + pad, row.y + pad, voice_w, fh);
            let mic_label = if recording == Some(Purpose::Note) { "Stop" } else { "Mic" };
            let talk_label = if recording == Some(Purpose::Command) { "Stop" } else { "Talk" };
            if ui.button_at(mic, "mic", mic_label) {
                v.toggle(Purpose::Note);
            }
            if ui.button_at(talk, "talk", talk_label) {
                v.toggle(Purpose::Command);
            }
        }
        let send_x = field.right() + pad + voice_gap;
        let send = ui.button_at(Rect::new(send_x, row.y + pad, send_w, fh), "send", "Send");
        if (res.submitted || send) && c.guard.is_none() {
            c.try_send();
        }
    });
}

fn view_rail(ui: &mut Ui, c: &mut ChatScreen, r: Rect) {
    let t = ui.theme();
    ui.fill(r, t.bar);
    let conversations: Vec<Conversation> = c.main.state().conversations.clone();
    let device = c.main.state().device_id.clone();
    let main_key = device.map(|d| format!("device:{d}"));
    let others: Vec<&Conversation> = conversations
        .iter()
        .filter(|x| Some(&x.key) != main_key.as_ref())
        .collect();
    let mut open: Option<Option<(String, String)>> = None;
    ui.within(
        Rect::new(
            r.x + t.px(8.0),
            r.y + t.px(8.0),
            r.w - t.px(16.0),
            r.h - t.px(16.0),
        ),
        |ui| {
            if ui.button_id("rail-main", "This laptop") {
                open = Some(None);
            }
            ui.space(6.0);
            ui.dim("Other chats (read only)");
            let a = ui.area();
            let res = ui.list(
                "rail",
                &mut c.rail,
                a,
                others.len(),
                44.0,
                |ui, i, selected| {
                    let row = ui.area();
                    if selected {
                        ui.fill_round(row, t.px(6.0), t.selection);
                    }
                    let x = others[i];
                    ui.text_in(
                        Rect::new(
                            row.x + t.px(6.0),
                            row.y + t.px(3.0),
                            row.w - t.px(12.0),
                            t.px(20.0),
                        ),
                        Face::Bold,
                        t.pt(13.0),
                        t.text,
                        &x.title,
                    );
                    ui.text_in(
                        Rect::new(
                            row.x + t.px(6.0),
                            row.y + t.px(22.0),
                            row.w - t.px(12.0),
                            t.px(18.0),
                        ),
                        Face::Sans,
                        t.pt(12.0),
                        t.dim,
                        &one_line(&shown(&x.last_text), 40),
                    );
                },
            );
            if let Some(i) = res.activated {
                let x = others[i];
                open = Some(Some((x.key.clone(), x.title.clone())));
            }
        },
    );
    match open {
        Some(None) => {
            c.reading = None;
            c.list = messages_list();
            c.rows.clear();
            c.follow = true;
            c.last_scroll = 0;
        }
        Some(Some((key, title))) => {
            let wake = c.main.wake_handle();
            c.reading = Some((
                key.clone(),
                title,
                Engine::start(c.client.clone(), Some(key), wake),
            ));
            c.list = messages_list();
            c.rows.clear();
            c.follow = true;
            c.last_scroll = 0;
        }
        None => {}
    }
}

fn main() {
    let mut args = std::env::args().skip(1);
    if args.next().as_deref() == Some("--render") {
        let (out, base) = (
            args.next().unwrap_or_else(|| "chat.ppm".into()),
            args.next(),
        );
        let token = lucina_account::Token::parse(
            &std::env::var("LUCINA_SECRETARY_TEST_TOKEN").unwrap_or_default(),
        );
        let client = base.and_then(|b| Client::new(&b, token));
        let mut h = Harness::new(Chat::new(None, client), 1920, 1080);
        for _ in 0..20 {
            std::thread::sleep(Duration::from_millis(100));
            h.core.message(Msg::Changed);
            h.core.paint();
        }
        std::fs::write(out, h.ppm()).expect("write");
        return;
    }
    lucina_app::run("lucina-app-chat", |tx| Chat::new(Some(tx), None));
}

#[cfg(test)]
mod tests {
    use super::*;
    use lucina_account::fake::{Fake, TOKEN};
    use lucina_app::keys;
    use std::time::Instant;

    fn client(f: &Fake) -> Client {
        Client::new(f.base(), lucina_account::Token::parse(TOKEN)).unwrap()
    }

    fn settle(h: &mut Harness<Chat>, what: &str, mut ok: impl FnMut(&mut Chat) -> bool) {
        let start = Instant::now();
        loop {
            h.app().refresh();
            h.core.paint();
            if ok(h.app()) {
                return;
            }
            assert!(
                start.elapsed() < Duration::from_secs(10),
                "timed out waiting for {what}"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    fn screen(chat: &mut Chat) -> &mut ChatScreen {
        match &mut chat.screen {
            Screen::Chat(c) => c,
            _ => panic!("not the chat"),
        }
    }

    fn has_row(chat: &mut Chat, f: impl Fn(&Row) -> bool) -> bool {
        screen(chat).rows.iter().any(f)
    }

    fn user_text(r: &Row, want: &str) -> bool {
        matches!(r, Row::User { text, .. } if text == want)
    }

    fn lucina_text(r: &Row, want: &str) -> bool {
        matches!(r, Row::Lucina { text, .. } if text == want)
    }

    fn app(f: &Fake) -> Harness<Chat> {
        Harness::new(Chat::new(None, Some(client(f))), 1920, 1080)
    }

    #[test]
    fn history_rich_rows_and_a_send_show_up() {
        let f = Fake::start();
        f.fill_demo();
        let mut h = app(&f);
        settle(&mut h, "history", |c| {
            has_row(c, |r| matches!(r, Row::Buttons { .. }))
        });
        assert!(has_row(h.app(), |r| lucina_text(r, "Domani hai 2 impegni")));
        assert!(has_row(h.app(), |r| matches!(r, Row::Block(_))));
        assert!(has_row(h.app(), |r| matches!(r, Row::Day(_))));
        // The panel of the active request is open above the composer.
        settle(&mut h, "the card", |c| screen(c).secret.needs_catalog());

        h.app().screen_focus_composer();
        h.core.message(Msg::Changed);
        h.core.paint();
        h.type_str("ciao");
        h.key(keys::KEY_ENTER);
        settle(&mut h, "the reply", |c| {
            has_row(c, |r| lucina_text(r, "Hai detto: ciao"))
        });
        assert!(has_row(h.app(), |r| user_text(r, "ciao")));
        assert_eq!(screen(h.app()).composer.text, "");
    }

    #[test]
    fn a_pasted_key_never_leaves_through_the_composer() {
        let f = Fake::start();
        let key = format!("sk-{}", "Zq7".repeat(12));
        f.set_projects(serde_like_projects());
        let mut h = app(&f);
        settle(&mut h, "loaded", |c| screen(c).main.state().loaded);
        h.app().screen_focus_composer();
        screen(h.app()).composer.set(&format!("usa {key}"));
        screen(h.app()).try_send();
        assert!(screen(h.app()).guard.is_some());
        assert!(f.bodies("/v1/chat").is_empty());
        // Nor is there a bubble.
        h.app().refresh();
        assert!(!has_row(h.app(), |r| matches!(r, Row::User { .. })));

        // "Save it as a secret": the value moves to the form, and the composer keeps the mask.
        let spans = screen(h.app()).guard.take().unwrap();
        let text = screen(h.app()).composer.text.clone();
        screen(h.app())
            .secret
            .open_with_value(&text[spans[0].0..spans[0].1]);
        let masked = secrets::mask(&text, &spans);
        screen(h.app()).composer.set(&masked);
        assert_eq!(screen(h.app()).composer.text, "usa [secret:token]");
        // Sent masked, the message holds no value.
        let masked = screen(h.app()).composer.text.clone();
        screen(h.app()).send_now(&masked);
        settle(&mut h, "the masked message", |_| {
            !f.bodies("/v1/chat").is_empty()
        });
        assert!(f.all_bodies().iter().all(|(_, b)| !b.contains(&key)));
    }

    #[test]
    fn the_secret_command_opens_the_form_and_is_never_sent() {
        let f = Fake::start();
        let mut h = app(&f);
        settle(&mut h, "loaded", |c| screen(c).main.state().loaded);
        screen(h.app()).composer.set("/secret MY_KEY abc123");
        screen(h.app()).try_send();
        assert_eq!(screen(h.app()).composer.text, "");
        assert!(screen(h.app()).secret.needs_catalog());
        assert!(f.bodies("/v1/chat").is_empty());
    }

    #[test]
    fn a_failed_message_offers_try_again() {
        let f = Fake::start();
        f.fail_chats(1);
        let mut h = app(&f);
        settle(&mut h, "loaded", |c| screen(c).main.state().loaded);
        screen(h.app()).send_now("riprova");
        settle(&mut h, "the failure", |c| {
            has_row(c, |r| {
                matches!(
                    r,
                    Row::User {
                        status: Status::Failed(_),
                        ..
                    }
                )
            })
        });
        let id = screen(h.app())
            .rows
            .iter()
            .find_map(|r| match r {
                Row::User { id, .. } => Some(id.clone()),
                _ => None,
            })
            .unwrap();
        assert!(screen(h.app()).main.retry(&id));
        settle(&mut h, "the reply", |c| {
            has_row(c, |r| lucina_text(r, "Hai detto: riprova"))
        });
    }

    #[test]
    fn a_button_sends_its_words_once_and_the_row_is_answered() {
        let f = Fake::start();
        f.fill_demo();
        let mut h = app(&f);
        settle(&mut h, "buttons", |c| {
            has_row(c, |r| matches!(r, Row::Buttons { .. }))
        });
        let (message, id, kind) = screen(h.app())
            .rows
            .iter()
            .find_map(|r| match r {
                Row::Buttons {
                    message, buttons, ..
                } => Some((message.clone(), buttons[0].0.clone(), buttons[0].2.clone())),
                _ => None,
            })
            .unwrap();
        screen(h.app()).press_button(&message, &id, &kind);
        settle(&mut h, "the reply", |c| {
            has_row(c, |r| lucina_text(r, "Hai detto: Dammi i dettagli"))
        });
        settle(&mut h, "the mark", |c| {
            has_row(c, |r| {
                matches!(
                    r,
                    Row::Buttons {
                        chosen: Some(_),
                        ..
                    }
                )
            })
        });
        assert_eq!(f.bodies("/v1/chat").len(), 1);
    }

    #[test]
    fn an_empty_screen_says_so_and_prose_drops_marks_and_emoji() {
        assert_eq!(
            prose("Ecco **ora** ✅ 📅 [sito](https://a.b)"),
            "Ecco ora ✓ sito (https://a.b)"
        );
        assert_eq!(one_line("a\n b  c", 3), "a b…");
        let (y, m, d) = civil(20_758);
        assert_eq!((y, m, d), (2026, 11, 1));
    }

    #[test]
    fn a_reply_with_no_widgets_shows_its_whole_text_with_its_lines() {
        let f = Fake::start();
        // As the server stores it: bubbles cut and run together, the text whole.
        f.add_item("lucina", "other", "The end.\nWhy: this.\n\n- one\n- two\n\n↪︎ rispondi: «sì»", serde_json::json!({"meta": {"format": "v1", "bubbles": ["The end.Why: this.The…"], "actions": []}}));
        let mut h = app(&f);
        settle(&mut h, "the row", |c| has_row(c, |r| lucina_text(r, "The end.\nWhy: this.\n\n• one\n• two")));
        h.core.message(Msg::Changed);
        h.core.paint();
    }

    #[test]
    fn a_decision_answered_elsewhere_has_no_buttons_and_a_pending_one_has() {
        let f = Fake::start();
        let meta = |id: &str| serde_json::json!({"meta": {"kind": "decision", "format": "v1", "bubbles": ["x"], "actions": [
            {"id": "o1", "label": "1. Yes", "kind": "reply", "payload": {"text": format!("Decision {id}: 1. Yes")}},
            {"id": "o2", "label": "2. No", "kind": "reply", "payload": {"text": format!("Decision {id}: 2. No")}}]}});
        f.add_item("lucina", "other", "claude (s (local_1)) needs a decision: Done?", meta("dc-aaa"));
        f.add_item("lucina", "other", "claude (s (local_1)) needs a decision: Open?", meta("dc-bbb"));
        f.add_decision(serde_json::json!({"decision_id": "dc-aaa", "status": "decided", "chosen_option": "Yes"}));
        f.add_decision(serde_json::json!({"decision_id": "dc-bbb", "status": "pending"}));
        let mut h = app(&f);
        settle(&mut h, "decided", |c| has_row(c, |r| matches!(r, Row::Note { text, .. } if text == "Decided: Yes")));
        let buttons = screen(h.app()).rows.iter().filter(|r| matches!(r, Row::Buttons { .. })).count();
        assert_eq!(buttons, 1);
        assert!(has_row(h.app(), |r| matches!(r, Row::Note { text, .. } if text == "Decision for you · claude · s")));
        assert!(has_row(h.app(), |r| lucina_text(r, "Done?")));
    }

    #[test]
    fn the_voice_controls_draw_and_a_pending_reply_follows_the_real_rows() {
        let f = Fake::start();
        f.fill_demo();
        let mut h = app(&f);
        settle(&mut h, "history", |c| has_row(c, |r| matches!(r, Row::Buttons { .. })));
        // A voice socket nobody listens on: it never comes up, which is all the drawing needs.
        let offline = Client::new("http://127.0.0.1:9", lucina_account::Token::parse(TOKEN)).unwrap();
        let voice = VoiceUi::start(&offline, Arc::new(|| {})).unwrap();
        screen(h.app()).voice = Some(voice);
        let before = screen(h.app()).rows.len();
        screen(h.app()).voice.as_mut().unwrap().inject(vec![
            lucina_voice_stream::session::Event::Transcript("che ore sono".into()),
            lucina_voice_stream::session::Event::Delta("Sono le tre".into()),
        ]);
        screen(h.app()).voice.as_mut().unwrap().line = Some("Listening… press again to send, Esc cancels.".into());
        h.core.message(Msg::Changed);
        h.core.paint();
        let rows = &screen(h.app()).rows;
        assert_eq!(rows.len(), before + 2);
        assert!(matches!(&rows[before], Row::User { text, .. } if text == "che ore sono"));
        assert!(matches!(&rows[before + 1], Row::Lucina { text, .. } if text == "Sono le tre"));
        if let Some(path) = std::env::var_os("LUCINA_CHAT_PPM") {
            std::fs::write(path, h.ppm()).unwrap();
        }
        // Esc with nothing recording is not the voice's.
        assert!(!screen(h.app()).voice_key(lucina_app::keys::Press { code: KEY_ESC, mods: Default::default() }));
    }

    #[test]
    fn emoji_go_neatly_and_checks_stay() {
        assert_eq!(shown("✅ Saved 🔐 `x`"), "✓ Saved `x`");
        assert_eq!(shown("🔐 Line\n\n📅 Two"), "Line\n\nTwo");
    }

    fn serde_like_projects() -> serde_json::Value {
        serde_json::json!({"projects": [{"id": "p1", "name": "Lucina", "alias": "lucina", "environments": [{"slug": "prod"}]}]})
    }

    impl Chat {
        fn screen_focus_composer(&mut self) {
            // The composer takes keys once focused; a click there in a test is a matter of coordinates.
            screen(self).focus_composer = true;
            screen(self).note = None;
        }
    }
}
