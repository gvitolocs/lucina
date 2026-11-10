//! What the shell tells Lucina happened (stage D of docs/research/juno-ai-native.md): which app is open and in front, which
//! ended and how, and the notices on Home. The shell is the one program the hub takes `shell` events from (it checks the peer).
//! The event's `app` is the app it is about, so `wait {for: "app.closed", app: "video"}` works as it does for the app's own events.
//! Nothing is sent while nobody listens, and nothing here waits.
use lucina_app::events::{self, wire::Source};

pub fn init() {
    events::client::set_source(Source::Shell);
}

fn about(kind: &str, id: &str) -> events::Emit {
    events::emit(kind)
        .subject(&format!("app:{id}"))
        .field("app", id)
}

/// An app has the screen: opened, and in front.
pub fn opened(id: &str, title: &str, terminal: bool) {
    about("app.opened", id)
        .field("title", title)
        .field("terminal", terminal)
        .send();
    about("app.focused", id).send();
}

/// It could not start at all.
pub fn failed_to_start(id: &str, what: &str) {
    about("app.failed", id).field("what", what).send();
}

/// It ended: by itself or asked (`app.closed`), or by a crash or a kill (`app.crashed`). Home is in front again unless the crash
/// screen is.
pub fn ended(id: &str, what: &str, failed: bool) {
    about(if failed { "app.crashed" } else { "app.closed" }, id)
        .field("what", what)
        .send();
    if !failed {
        about("app.focused", "home").send();
    }
}

/// A notice shown on Home (battery low, a failed generation): its text is the shell's own words.
pub fn notice(text: &str) {
    events::emit("notice.shown").field("text", text).send();
}
