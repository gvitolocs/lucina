//! Invented events for trying the app without an account and for its tests: mostly iPhone-sourced,
//! as the owner's real ones are. Nothing here comes from his data, and the app says it's a sample.

use chrono::{Datelike, Duration, NaiveDate};
use chrono_tz::Tz;
use secretary_calendar::{Item, Moment};

use crate::server::Fake;

fn at(day: NaiveDate, h: u32, m: u32, tz: Tz) -> Moment {
    Moment::Local {
        at: day.and_hms_opt(h, m, 0).expect("a time"),
        tz: tz.name().to_owned(),
    }
}

fn timed(uid: &str, title: &str, day: NaiveDate, from: (u32, u32), to: (u32, u32), tz: Tz) -> Item {
    Item {
        uid: uid.into(),
        title: title.into(),
        start: Some(at(day, from.0, from.1, tz)),
        end: Some(at(day, to.0, to.1, tz)),
        ..Item::default()
    }
}

/// A server with the sample around `today`.
pub fn server(tz: Tz, today: NaiveDate) -> Fake {
    let monday = today - Duration::days(today.weekday().num_days_from_monday() as i64);
    let d = |n: i64| monday + Duration::days(n);
    // The iPhone's snapshot has one item per instance (`iphone:<id>@<start>`), expanded on the phone.
    let mut iphone = Vec::new();
    for week in -6..10i64 {
        for wd in [0, 2, 4] {
            let n = week * 7 + wd;
            let mut standup = timed(
                &format!("a1@{n}"),
                "Team stand-up",
                d(n),
                (9, 30),
                (10, 0),
                tz,
            );
            standup.calendar = Some("Work".into());
            iphone.push(standup);
        }
        let n = week * 7 + 1;
        let mut lecture = timed(
            &format!("a2@{n}"),
            "Distributed systems lecture",
            d(n),
            (10, 15),
            (12, 0),
            tz,
        );
        lecture.location = Some("Building 1531, room 113".into());
        lecture.calendar = Some("University".into());
        iphone.push(lecture);
    }
    let mut review = timed("a3", "Design review", d(2), (14, 0), (15, 30), tz);
    review.calendar = Some("Work".into());
    review.description = Some("Second draft of the home screen.".into());
    review.attendees = vec!["Sam".into(), "Noor".into()];
    let mut overlap = timed("a4", "Call with the printer", d(2), (14, 30), (15, 0), tz);
    overlap.calendar = Some("Work".into());
    let mut dentist = timed("a5", "Dentist", d(3), (8, 0), (9, 0), tz);
    dentist.calendar = Some("Home".into());
    dentist.location = Some("Nørregade 12".into());
    let mut dinner = timed("a6", "Dinner with friends", d(4), (19, 0), (22, 0), tz);
    dinner.calendar = Some("Home".into());
    let mut trip = Item {
        uid: "a7".into(),
        title: "Weekend in the country".into(),
        start: Some(Moment::Date(d(5))),
        end: Some(Moment::Date(d(7))),
        calendar: Some("Home".into()),
        ..Item::default()
    };
    trip.free = true;
    let holiday = Item {
        uid: "a8".into(),
        title: "Grandma's birthday".into(),
        start: Some(Moment::Date(d(3))),
        end: Some(Moment::Date(d(4))),
        calendar: Some("Home".into()),
        ..Item::default()
    };
    iphone.extend([review, overlap, dentist, dinner, trip, holiday]);
    let own = timed(
        "ev_0",
        "Focus: write the report",
        d(1),
        (13, 0),
        (15, 0),
        tz,
    );
    Fake::new(tz, vec![own], iphone)
}
