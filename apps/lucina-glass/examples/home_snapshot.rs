//! Renders the Juno home over the night sky with the values the laptop showed on 2026-11-09 at 00:25 (run4's fb0 capture: the
//! owner's calendar week, tasks, decisions, agents and the reset notice), for the review set while the laptop is busy. The
//! words are the sources' own, copied from that capture; the shell itself never draws this.
//!
//! `cargo run --release -p lucina-glass --example home_snapshot -- <out.png>`
use lucina_glass::home::{self, Action, Day, Deadline, Device, Dot, Home, HomeData, Note, Stat, StatusBar, Target, Week};
use lucina_glass::icons::Icon;
use lucina_glass::{Canvas, Type, imagery};

fn main() {
    let out = std::env::args().nth(1).unwrap_or_else(|| "home-snapshot.png".into());
    let (w, h) = (1920u32, 1080u32);
    let bg = imagery::night_sky(w, h, home::K, 7);
    let blurred = home::quick_blur(&bg);
    let act = |icon, l: &str, t: Target| Action { icon, label: l.into(), target: t };
    let day = |n: u32, name: &str, dots: &[Dot], today: bool, hours: &[u32], ranges: &str| Day {
        n,
        name: name.into(),
        dots: dots.to_vec(),
        today,
        hours: hours.to_vec(),
        ranges: ranges.into(),
    };
    let data = HomeData {
        status: Some(StatusBar { clock: "00:25".into(), online: true, battery: Some(100), charging: false, camera: false }),
        segments: vec![
            (
                "Work".into(),
                vec![
                    act(Icon::Doc, "Open", Target::Apps),
                    act(Icon::Clock, "Plan", Target::App("calendar".into())),
                    act(Icon::Pen, "Write", Target::App("notes".into())),
                    act(Icon::Play, "Play", Target::App("player".into())),
                ],
            ),
            ("Lucina".into(), vec![act(Icon::Send, "Ask", Target::App("chat".into()))]),
        ],
        stats_left: vec![
            Stat { icon: Icon::Doc, caption: "Due today".into(), value: "0".into(), app: Some("tasks".into()), items: Vec::new() },
            Stat { icon: Icon::Bolt, caption: "Overdue".into(), value: "9".into(), app: Some("tasks".into()), items: Vec::new() },
        ],
        device: Some(Device { line: "eth0 192.168.1.47".into(), busy: false }),
        deadline: Some(Deadline { fact: "Security and Privacy".into(), line: "Today 08:00".into(), soon: false }),
        week: Some(Week {
            days: vec![
                day(9, "Mon", &[Dot::Other, Dot::Other], true, &[60, 60, 60, 60, 60, 60, 60, 60, 0, 0], "8–16"),
                day(10, "Tue", &[Dot::Other, Dot::Other, Dot::Other], false, &[0; 10], ""),
                day(11, "Wed", &[], false, &[0; 10], ""),
                day(12, "Thu", &[Dot::Other], false, &[0; 10], ""),
                day(13, "Fri", &[Dot::Other, Dot::Other], false, &[0; 10], ""),
                day(14, "Sat", &[], false, &[0; 10], ""),
                day(15, "Sun", &[], false, &[0; 10], ""),
            ],
            selected: 0,
            first_hour: 8,
            last_hour: 18,
        }),
        stats_right: vec![
            Stat { icon: Icon::Bell, caption: "Decisions".into(), value: "0".into(), app: Some("decisions".into()), items: Vec::new() },
            Stat { icon: Icon::Code, caption: "Agents".into(), value: "3".into(), app: Some("agents".into()), items: Vec::new() },
        ],
        notes: Some(vec![Note { id: "event:1".into(), fact: "Juno reset itself without a crash report".into(), line: "Just now".into(), urgent: false, app: None, at: None }]),
        volume: Some((1.0, false)),
        ..HomeData::default()
    };
    let mut hm = Home::new(0.0);
    hm.set_data(0.0, data);
    let mut cv = Canvas::new(w, h, home::K);
    let mut ty = Type::new();
    let _ = hm.damage(10.0);
    hm.draw(&mut cv, &mut ty, 10.0, &bg, &blurred);
    std::fs::write(&out, cv.png()).expect("png written");
    println!("{out}");
}
