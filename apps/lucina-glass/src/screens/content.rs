//! Every word on the screens, in one place: placeholder data until the concept is wired in. Per docs/design/rules.md,
//! each card says one fact and at most one short secondary line, and actions are icons with one-word labels.

/// The device names (rule 6): the phone and tablet are Diana, the laptop and desktop Juno, the DSi Fortuna.
pub const PHONE: &str = "Diana";
pub const LAPTOP: &str = "Juno";
pub const DSI: &str = "Fortuna";

pub const BUILD_TITLE: &str = "Build";
pub const LIVE: &str = "Live";
/// The header's one secondary line.
pub const PHASE: &str = "Phase 2 of 3";
pub const PHASE_PERCENT: u32 = 76;
pub const SEGMENTS: [&str; 2] = ["Work", "Talks"];
/// The action row: an icon over one verb each (HIG Writing: label buttons with verbs).
pub const ACTIONS: [&str; 4] = ["Open", "Email", "Book", "Share"];
pub const UPLOADING: &str = "Uploading";
/// The timer's whole length and what is left when the screen opens: 02:14.
pub const TIMER_TOTAL: u32 = 300;
pub const TIMER_LEFT: f64 = 134.0;
/// Stat tiles: a caption with its icon, over one value.
pub const TILES: [(&str, &str); 2] = [("Views", "12"), ("Saves", "07")];

pub const QUEUE: &str = "3 pending";
pub const PERSON: (&str, &str, &str) = ("MS", "Mira S.", "Designer");
/// The reminder card: one fact, one secondary line.
pub const REMINDER: (&str, &str) = ("Portfolio due Friday", "4 teams reviewing");
pub const DAYS: [&str; 5] = ["Mon", "Tue", "Wed", "Thu", "Fri"];
pub const DAY_SELECTED: usize = 1;
pub const RANGES: &str = "3–4, 8–9";
pub const LOAD_PERCENT: u32 = 51;
/// The summary tile's load per hour, for the concept's sample day.
pub const LOAD_BARS: [f32; 7] = [0.35, 0.6, 0.45, 0.9, 0.7, 0.3, 0.55];

pub const NOW: &str = "Now";
/// The rail's count (the pill says what it counts).
pub const PENDING: u32 = 3;
/// The activity card: a caption with its icon, over one value.
pub const ACTIVITY: (&str, &str) = ("Profile views today", "2");
/// The second stat card.
pub const TEAMS: (&str, &str) = ("Teams reviewing", "4");
pub const NOTIFY_TITLE: &str = "Notification";
pub const CARDS: [(&str, &str); 3] = [("Shortlisted by 3 teams", "Closes Friday"), ("New case notes", "2-minute read"), ("Intro call moved", "Thu 10:30")];
pub const PAGES: usize = 5;
pub const PAGE: usize = 1;
pub const NOTIFY_ACTIONS: [&str; 2] = ["Upload", "Remind"];
