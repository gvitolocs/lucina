//! What Juno's home shows, as plain data: the shell's sources fill it (calendar, Lucina, the event hub, the player, uploads, the
//! machine), and the view draws only what is here. Every card is optional: a card with no source is left out, never filled with
//! sample text (docs/design/rules.md, rule 8). Words are the sources' own; the view adds none but its fixed labels.

use crate::icons::Icon;

/// The status bar: the clock, the network and the battery, as the shell reads them.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct StatusBar {
    /// "14:05".
    pub clock: String,
    /// Whether the machine is online (Wi-Fi or the cable).
    pub online: bool,
    /// Battery charge, 0 to 100, when there is a battery.
    pub battery: Option<u8>,
    pub charging: bool,
    /// Someone captures from the camera: a red mark by the clock (a privacy indicator, never hidden).
    pub camera: bool,
}

/// The Build card: something running in phases, such as a merge going through its gates or an update installing.
#[derive(Clone, Debug, PartialEq)]
pub struct Build {
    /// One or two words: "Build", "Update".
    pub title: String,
    /// 1-based, at most `phases`.
    pub phase: u32,
    pub phases: u32,
    /// The whole job's progress, 0 to 100.
    pub percent: u32,
    /// Running now: the red pill shows and breathes.
    pub live: bool,
    /// The one line under the title, the source's own count: "Job 2 of 3".
    pub line: String,
    /// What opens when it is chosen (an app id), if anything.
    pub app: Option<String>,
}

/// The machine card under the device name: one short line ("Generation 42") and whether something is loading.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Device {
    pub line: String,
    pub busy: bool,
}

/// A person chip: initials, a name and one line.
#[derive(Clone, Debug, PartialEq)]
pub struct Person {
    pub initials: String,
    pub name: String,
    pub line: String,
}

/// The next thing due: one fact, one short line, and whether it is close.
#[derive(Clone, Debug, PartialEq)]
pub struct Deadline {
    pub fact: String,
    pub line: String,
    /// Within the hour: the bar is the warning colour, else the accent.
    pub soon: bool,
}

/// A dot on a day: whose event it is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Dot {
    /// The owner's own (Lucina's calendar): the accent.
    Own,
    /// From someone else's calendar: the info colour.
    Other,
    /// Happening now: red.
    Now,
}

/// One day of the agenda grid.
#[derive(Clone, Debug, PartialEq)]
pub struct Day {
    /// Day of the month.
    pub n: u32,
    /// "Mon".."Sun".
    pub name: String,
    pub dots: Vec<Dot>,
    pub today: bool,
    /// Busy minutes in each working hour from [`Week::first_hour`], for the summary's bars.
    pub hours: Vec<u32>,
    /// The busy ranges in words, "9–10, 14–15", or empty.
    pub ranges: String,
}

/// The week the agenda shows: the day chips are its first five days, the grid its days in order.
#[derive(Clone, Debug, PartialEq)]
pub struct Week {
    pub days: Vec<Day>,
    /// The day chosen when it arrives (today, or the first day).
    pub selected: usize,
    /// The working day the summary measures, in hours: 8 to 18.
    pub first_hour: u32,
    pub last_hour: u32,
}

/// A stat card: an icon, a caption, one value.
#[derive(Clone, Debug, PartialEq)]
pub struct Stat {
    pub icon: Icon,
    pub caption: String,
    pub value: String,
    /// What opens when it is chosen.
    pub app: Option<String>,
    /// The count's detail, when the card has the room: up to three lines, each opening its own item.
    pub items: Vec<StatItem>,
}

/// One line of a stat card's detail: a title and what it opens.
#[derive(Clone, Debug, PartialEq)]
pub struct StatItem {
    pub title: String,
    pub target: Target,
}

/// A notification: one fact and one short line (rule 1).
#[derive(Clone, Debug, PartialEq)]
pub struct Note {
    /// Stable across refreshes, so a card that is already shown does not enter again.
    pub id: String,
    pub fact: String,
    pub line: String,
    pub urgent: bool,
    /// What "Open" opens.
    pub app: Option<String>,
    /// When it happened, wall-clock ms: the shell words `line` from it ("5 min ago") each time it composes, so the age stays true.
    pub at: Option<i64>,
}

/// A notification's age in words: "Just now" under a minute, "5 min ago" under an hour, "2 h ago" under twelve, then the
/// time of day (`clock`, as the source gives it).
pub fn age_line(age_ms: i64, clock: &str) -> String {
    let min = age_ms.max(0) / 60_000;
    if min < 1 {
        "Just now".into()
    } else if min < 60 {
        format!("{min} min ago")
    } else if min < 12 * 60 {
        format!("{} h ago", min / 60)
    } else {
        clock.to_string()
    }
}

/// Uploads going out (recordings, files).
#[derive(Clone, Debug, PartialEq)]
pub struct Upload {
    /// "Uploading 2".
    pub label: String,
    /// 0..1 when known.
    pub fraction: Option<f32>,
}

/// The countdown in the dock: until the next event starts, within the hour.
#[derive(Clone, Debug, PartialEq)]
pub struct Countdown {
    /// When it reaches zero, on the view's clock (seconds).
    pub at: f64,
    /// The window the ring measures, seconds (3600: the ring is full an hour before).
    pub window: u32,
}

/// What the player has, or last had.
#[derive(Clone, Debug, PartialEq)]
pub struct Media {
    pub title: String,
    pub playing: bool,
    /// The app that plays it, which Play opens.
    pub app: String,
}

/// An action button: an icon over one verb, opening an app (or the apps page).
#[derive(Clone, Debug, PartialEq)]
pub struct Action {
    pub icon: Icon,
    pub label: String,
    pub target: Target,
}

/// Where an action goes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Target {
    App(String),
    /// The page of every app.
    Apps,
    Settings,
    /// One task, opened in Tasks (by its id).
    Task(String),
}

/// Everything the home shows. Each `None` or empty list leaves its card out.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct HomeData {
    pub status: Option<StatusBar>,
    pub build: Option<Build>,
    /// The segmented control's labels and, per segment, its row of up to four actions.
    pub segments: Vec<(String, Vec<Action>)>,
    /// Up to two stat cards at the foot of the left column.
    pub stats_left: Vec<Stat>,
    /// The card titled with the device's name.
    pub device: Option<Device>,
    pub person: Option<Person>,
    pub deadline: Option<Deadline>,
    pub week: Option<Week>,
    /// Up to two stat cards at the top of the right column.
    pub stats_right: Vec<Stat>,
    /// `None`: no source, the panel is left out. `Some(vec![])`: nothing new.
    pub notes: Option<Vec<Note>>,
    pub upload: Option<Upload>,
    pub countdown: Option<Countdown>,
    pub media: Option<Media>,
    /// The system volume, 0..1, and whether it is muted.
    pub volume: Option<(f32, bool)>,
}
