//! Invented tasks for trying the app without an account and for its tests. Nothing here comes from
//! the owner's data, and the app says on screen that it is a sample.

use chrono::Duration;
use chrono_tz::Tz;
use secretary_tasks::{Engine, today_in};
use serde_json::{Value, json};

fn item(id: &str, project: &str, content: &str, priority: u8, due: Option<Value>) -> Value {
    json!({
        "id": id, "project_id": project, "content": content, "priority": priority,
        "due": due, "child_order": id[1..].parse::<i64>().unwrap_or(0), "labels": [],
    })
}

/// An engine holding the sample, as a Todoist full sync would have left it.
pub fn engine(tz: Tz, now: u64) -> Engine {
    let today = today_in(tz, now);
    let day = |n: i64| (today + Duration::days(n)).format("%Y-%m-%d").to_string();
    let due = |n: i64, s: &str| Some(json!({"date": day(n), "string": s, "lang": "en"}));
    let at = |n: i64, time: &str, s: &str| {
        Some(json!({"date": format!("{}T{time}:00", day(n)), "string": s, "lang": "en"}))
    };
    let every = |n: i64, s: &str| {
        Some(json!({"date": day(n), "string": s, "lang": "en", "is_recurring": true}))
    };
    let mut items = vec![
        item("t1", "p1", "Water the plants", 1, every(0, "every 3 days")),
        item("t2", "p1", "Sketch ideas for the garden shelf", 1, None),
        item(
            "t3",
            "p1",
            "Return the library books",
            3,
            due(-2, "2 days ago"),
        ),
        item(
            "t4",
            "p2",
            "Write the release notes",
            4,
            at(0, "09:30", "today 9:30"),
        ),
        item(
            "t5",
            "p2",
            "Review the design draft",
            2,
            at(0, "14:00", "today 2pm"),
        ),
        item("t6", "p2", "Plan the next sprint", 1, due(1, "tomorrow")),
        item("t7", "p2", "Send the invoice", 3, due(-1, "yesterday")),
        item("t8", "p3", "Oat milk and bread", 1, due(0, "today")),
        item("t9", "p3", "Book the dentist", 2, due(3, "in 3 days")),
        item(
            "t10",
            "p3",
            "Renew the bike insurance",
            1,
            due(5, "in 5 days"),
        ),
        item("t11", "p4", "Read chapter 4", 1, every(1, "every day")),
        item("t12", "p4", "Exercises for chapter 4", 1, None),
        item("t13", "p4", "Exercise 1", 1, None),
        item("t14", "p4", "Exercise 2", 1, None),
    ];
    items[0]["labels"] = json!(["home"]);
    items[3]["labels"] = json!(["work", "writing"]);
    items[4]["description"] =
        json!("The second draft: spacing on the home screen and the new icons.");
    items[7]["labels"] = json!(["errand"]);
    items[8]["labels"] = json!(["errand"]);
    items[11]["section_id"] = json!("s1");
    items[12]["parent_id"] = json!("t12");
    items[13]["parent_id"] = json!("t12");
    let sync = json!({
        "full_sync": true,
        "sync_token": "sample",
        "projects": [
            {"id": "p1", "name": "Inbox", "inbox_project": true, "color": "charcoal"},
            {"id": "p2", "name": "Work", "color": "blue", "child_order": 1},
            {"id": "p3", "name": "Home", "color": "green", "child_order": 2},
            {"id": "p4", "name": "Study", "color": "grape", "child_order": 3},
        ],
        "sections": [{"id": "s1", "project_id": "p4", "name": "This week", "section_order": 1}],
        "labels": [
            {"id": "l1", "name": "home", "color": "green"},
            {"id": "l2", "name": "work", "color": "blue"},
            {"id": "l3", "name": "errand", "color": "orange"},
            {"id": "l4", "name": "writing", "color": "grape"},
        ],
        "filters": [{"id": "f1", "name": "Urgent", "query": "p1 | p2", "color": "red"}],
        "items": items,
    });
    let mut e = Engine::in_memory();
    e.apply_sync(&sync, now).expect("the sample is valid");
    e.set_sync_status(secretary_tasks::SyncStatus::Disconnected, None, None)
        .expect("the sample is valid");
    e
}
