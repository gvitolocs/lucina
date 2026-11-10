//! A filled-in [`HomeData`] for tests and the review renders only: the shell never draws it. Its words are plainly a fixture.

use super::*;

pub fn full(now: Secs) -> HomeData {
    let day = |n: u32, name: &str, dots: &[Dot], today: bool, hours: &[u32], ranges: &str| Day {
        n,
        name: name.into(),
        dots: dots.to_vec(),
        today,
        hours: hours.to_vec(),
        ranges: ranges.into(),
    };
    let act = |icon, label: &str, t: Target| Action {
        icon,
        label: label.into(),
        target: t,
    };
    HomeData {
        status: Some(StatusBar {
            clock: "14:05".into(),
            online: true,
            battery: Some(82),
            charging: false,
            camera: false,
        }),
        build: Some(Build {
            title: "Build".into(),
            phase: 2,
            phases: 3,
            percent: 64,
            live: true,
            line: "Phase 2 of 3".into(),
            app: Some("agents".into()),
        }),
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
            (
                "Lucina".into(),
                vec![
                    act(Icon::Send, "Ask", Target::App("chat".into())),
                    act(Icon::Bell, "Decide", Target::App("decisions".into())),
                ],
            ),
        ],
        stats_left: vec![
            Stat {
                icon: Icon::Doc,
                caption: "Due today".into(),
                value: "3".into(),
                app: Some("tasks".into()),
                items: ["Fixture task one", "Fixture task two", "Fixture task three"]
                    .iter()
                    .enumerate()
                    .map(|(i, t)| StatItem {
                        title: t.to_string(),
                        target: Target::Task(format!("t{i}")),
                    })
                    .collect(),
            },
            Stat {
                icon: Icon::Bolt,
                caption: "Overdue".into(),
                value: "1".into(),
                app: Some("tasks".into()),
                items: Vec::new(),
            },
        ],
        device: Some(Device {
            line: "Generation 42".into(),
            busy: true,
        }),
        person: None,
        deadline: Some(Deadline {
            fact: "Fixture event".into(),
            line: "Today 15:00".into(),
            soon: true,
        }),
        week: Some(Week {
            days: vec![
                day(
                    9,
                    "Mon",
                    &[Dot::Own],
                    false,
                    &[0, 30, 60, 0, 0, 0, 0, 0, 0, 0],
                    "9–10",
                ),
                day(
                    10,
                    "Tue",
                    &[Dot::Other, Dot::Now],
                    true,
                    &[0, 0, 60, 60, 0, 30, 0, 45, 0, 0],
                    "10–12, 15–16",
                ),
                day(11, "Wed", &[], false, &[0; 10], ""),
                day(
                    12,
                    "Thu",
                    &[Dot::Other],
                    false,
                    &[60, 60, 0, 0, 0, 0, 0, 0, 0, 0],
                    "8–10",
                ),
                day(13, "Fri", &[Dot::Own, Dot::Other], false, &[0; 10], ""),
                day(14, "Sat", &[], false, &[0; 10], ""),
                day(15, "Sun", &[], false, &[0; 10], ""),
            ],
            selected: 1,
            first_hour: 8,
            last_hour: 18,
        }),
        stats_right: vec![
            Stat {
                icon: Icon::Bell,
                caption: "Decisions".into(),
                value: "2".into(),
                app: Some("decisions".into()),
                items: Vec::new(),
            },
            Stat {
                icon: Icon::Code,
                caption: "Agents".into(),
                value: "5".into(),
                app: Some("agents".into()),
                items: Vec::new(),
            },
        ],
        notes: Some(vec![
            Note {
                id: "a".into(),
                fact: "Fixture notice".into(),
                line: "Two minutes ago".into(),
                urgent: true,
                app: None,
                at: None,
            },
            Note {
                id: "b".into(),
                fact: "Second fixture".into(),
                line: "Today".into(),
                urgent: false,
                app: None,
                at: None,
            },
        ]),
        upload: Some(Upload {
            label: "Uploading 2".into(),
            fraction: Some(0.4),
        }),
        countdown: Some(Countdown {
            at: now + 754.0,
            window: 3600,
        }),
        media: Some(Media {
            title: "Fixture track".into(),
            playing: true,
            app: "player".into(),
        }),
        volume: Some((0.6, false)),
    }
}
