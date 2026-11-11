//! The app on the document of `web/src/test/serverFixtures.ts`, through the same JSON as the wire. The expectations follow
//! `web/src/views/ServerHealth.test.tsx` and the helpers of `web/src/api/systemOverview.ts`.

use std::sync::{Arc, Mutex};

use lucina_app::time::Zone;
use lucina_app::{Harness, keys};
use serde_json::json;

use crate::app::{Link, Msg, Section, ServerApp, Sheet};
use crate::model::{self, Overview, State};
use crate::pages::{self, Act, Ctx, Line};
use crate::server::{Fake, Server, ServerError, sample_json};

/// 2026-11-02 12:00 UTC.
const NOW: i64 = 1_793_620_800_000;

fn doc() -> Overview {
    serde_json::from_value(sample_json(NOW)).expect("the fixture parses")
}

/// The app with a link that answers on the same thread when the test says so.
struct T {
    h: Harness<ServerApp>,
    fake: Arc<Mutex<Fake>>,
}

fn start(doc: serde_json::Value) -> T {
    let fake = Arc::new(Mutex::new(Fake::new(doc)));
    let (tx, rx) = std::sync::mpsc::channel::<bool>();
    let link: Link = Box::new(move |fresh| {
        let _ = tx.send(fresh);
    });
    let mut a = ServerApp::new(Zone::utc(), Some(link));
    a.fixed_now = Some(NOW);
    let mut t = T { h: Harness::new(a, 1280, 800), fake: fake.clone() };
    PENDING.with(|p| *p.borrow_mut() = Some(rx));
    t.settle();
    t
}

thread_local! {
    static PENDING: std::cell::RefCell<Option<std::sync::mpsc::Receiver<bool>>> = const { std::cell::RefCell::new(None) };
}

impl T {
    fn settle(&mut self) {
        loop {
            let job = PENDING.with(|p| p.borrow().as_ref().and_then(|r| r.try_recv().ok()));
            let Some(fresh) = job else { break };
            let r = self.fake.lock().unwrap().overview(fresh);
            self.h.core.message(match r {
                Ok(o) => Msg::Loaded(Box::new(o)),
                Err(e) => Msg::Failed(e),
            });
            self.h.core.paint();
        }
        self.h.core.input(lucina_app::Input::Wheel(0));
        self.h.core.paint();
    }

    fn app(&mut self) -> &mut ServerApp {
        self.h.app()
    }

    fn titles(&mut self) -> Vec<String> {
        self.app().lines.iter().filter_map(|l| match l {
            Line::Row { title, .. } => Some(title.clone()),
            Line::Host { name, .. } => Some(name.clone()),
            Line::Heading(s) => Some(s.clone()),
            _ => None,
        }).collect()
    }

    /// Walks down to the line whose text holds `what` and presses Enter.
    fn enter_on(&mut self, what: &str) {
        let at = self.app().lines.iter().position(|l| match l {
            Line::Row { title, .. } => title.contains(what),
            Line::Host { name, .. } => name.contains(what),
            Line::Button { label, .. } => label.contains(what),
            _ => false,
        }).unwrap_or_else(|| panic!("no line with {what}"));
        let from = self.app().list.selected.expect("a list keeps a selected row");
        for _ in from..at {
            self.h.key(keys::KEY_DOWN);
        }
        for _ in at..from {
            self.h.key(keys::KEY_UP);
        }
        self.h.key(keys::KEY_ENTER);
        self.settle();
    }

    fn text(&mut self) -> String {
        format!("{:?}", self.app().lines)
    }
}

#[test]
fn the_overview_says_what_needs_attention_first() {
    let mut t = start(sample_json(NOW));
    let titles = t.titles();
    assert_eq!(titles[0], "Needs attention");
    assert_eq!(titles[1], "gmaps-server");
    assert_eq!(titles[2], "backup");
    assert_eq!(titles[3], "Hosts");
    assert!(titles.contains(&"buildhost".to_owned()));
    assert!(titles.iter().any(|x| x.contains("Restarted recently · 1")));
    assert!(titles.iter().any(|x| x.contains("All good · 122 checks")));
    let (state, text) = t.app().status();
    assert_eq!(state, State::Crit);
    assert_eq!(text, "2 need attention · updated 1 min ago");
}

#[test]
fn the_flapping_item_stays_out_of_needs_attention() {
    let mut t = start(sample_json(NOW));
    let at = t.titles().iter().position(|x| x == "Pokoin API");
    assert_eq!(at, None, "folded under Restarted recently");
    t.enter_on("Restarted recently");
    assert!(t.titles().contains(&"Pokoin API".to_owned()));
}

#[test]
fn calm_when_nothing_needs_attention() {
    let mut d = sample_json(NOW);
    d["attention"] = json!([]);
    d["attention_count"] = json!(0);
    let mut t = start(d);
    assert!(t.text().contains("Nothing needs attention"));
    assert_eq!(t.app().status().1, "All systems normal · updated 1 min ago");
}

#[test]
fn enter_opens_an_item_and_esc_comes_back() {
    let mut t = start(sample_json(NOW));
    t.enter_on("gmaps-server");
    assert_eq!(t.app().sheets, [Sheet::Item("catalog:gmaps-server".into())]);
    let text = t.text();
    assert!(text.contains("Not running; port 18210 not listening"));
    assert!(text.contains("Since 09:50 · 2 hr · 142 failures in 24 h"));
    assert!(text.contains("journalctl --user -u gmaps-server"));
    assert!(text.contains("Breaking"));
    assert!(text.contains("maps.example.com"));
    assert!(text.contains("Also down because of this"));
    t.h.key(keys::KEY_ESC);
    t.settle();
    assert!(t.app().sheets.is_empty());
}

#[test]
fn a_command_only_reaches_the_clipboard() {
    let c = doc();
    let zone = Zone::utc();
    let ctx = Ctx { o: &c, zone: &zone, now: NOW };
    let lines = pages::item(&ctx, &c.attention[0]);
    let restart = lines.iter().find_map(|l| match l {
        Line::Button { label, act } if label.contains("restart") => Some(act.clone()),
        _ => None,
    });
    assert_eq!(restart, Some(Act::Copy { what: "The restart command".into(), text: "systemctl --user restart gmaps-server".into() }));
}

#[test]
fn ask_lucina_has_the_webs_words() {
    let c = doc();
    let zone = Zone::utc();
    let ctx = Ctx { o: &c, zone: &zone, now: NOW };
    assert_eq!(
        pages::ask_text(&c.attention[0], &ctx),
        "Something on my server needs attention.\nProblem: gmaps-server\nHost: buildhost\nWhat's wrong: Not running; port 18210 not listening\nSince: 09:50 (2 hr)\nIt breaks: Lucina Maps\nLogs: journalctl --user -u gmaps-server -n 100 --no-pager\nWhat should I check first?"
    );
}

#[test]
fn a_host_opens_with_its_charts_and_an_unreachable_one_says_since_when() {
    let mut t = start(sample_json(NOW));
    t.enter_on("buildhost");
    assert_eq!(t.app().sheets, [Sheet::Host("buildhost".into())]);
    assert_eq!(t.app().list.selected, Some(0), "it opens on its charts");
    let charts = t.app().lines.iter().filter(|l| matches!(l, Line::Chart { .. })).count();
    assert_eq!(charts, 3);
    let text = t.text();
    assert!(text.contains("Tctl 95 °C"));
    assert!(text.contains("CPU temperature: now 95 °C, low 61 °C, high 96 °C"));
    assert!(text.contains("Needs attention on this host"));
    t.h.key(keys::KEY_ESC);
    t.settle();
    let c = doc();
    let zone = Zone::utc();
    let ctx = Ctx { o: &c, zone: &zone, now: NOW };
    let pi = pages::host(&ctx, &c.hosts[2]);
    assert!(format!("{pi:?}").contains("Unreachable since 07:00"));
    assert!(!pi.iter().any(|l| matches!(l, Line::Chart { .. })));
}

#[test]
fn services_open_what_is_unhealthy_and_search_by_keyboard() {
    let mut t = start(sample_json(NOW));
    t.h.key(keys::KEY_2);
    t.settle();
    assert_eq!(t.app().section, Section::Services);
    let titles = t.titles();
    assert!(titles.contains(&"▾ Lucina".to_owned()), "{titles:?}");
    assert!(titles.contains(&"▸ Pokoin".to_owned()));
    assert!(titles.contains(&"gmaps-server".to_owned()));
    assert!(!titles.contains(&"Pokoin API".to_owned()), "a healthy group starts closed");
    assert!(!titles.contains(&"SecretaryIOS".to_owned()), "repos are behind a switch");

    t.h.key(keys::KEY_SLASH);
    t.settle();
    t.h.type_str("pokoin");
    t.settle();
    let titles = t.titles();
    assert!(titles.contains(&"Pokoin API".to_owned()), "{titles:?}");
    assert!(!titles.contains(&"gmaps-server".to_owned()));
    t.h.type_str("zzz");
    t.settle();
    assert!(t.text().contains("No service matches “pokoinzzz”."));
    t.h.key(keys::KEY_ESC);
    t.settle();
    assert!(t.titles().contains(&"gmaps-server".to_owned()));
}

#[test]
fn a_component_opens_its_item_when_it_has_one() {
    let c = doc();
    assert!(c.attention_for("gmaps-server").is_some());
    assert_eq!(c.attention_for("secretary-server").map(|a| a.key.clone()), None);
    let mut t = start(sample_json(NOW));
    t.h.key(keys::KEY_2);
    t.settle();
    t.enter_on("Secretary server");
    assert_eq!(t.app().sheets, [Sheet::Component("secretary-server".into())]);
    assert!(t.text().contains("The API"));
}

#[test]
fn incidents_and_deploys_split_the_timeline_by_day() {
    let mut t = start(sample_json(NOW));
    t.h.key(keys::KEY_3);
    t.settle();
    let titles = t.titles();
    assert_eq!(titles, ["Today", "gmaps-server went down", "buildhost overheating"]);
    assert!(t.text().contains("(Enter: more)"));
    t.enter_on("buildhost overheating");
    assert!(t.text().contains("came back under ninety degrees"));
    t.h.key(keys::KEY_4);
    t.settle();
    assert_eq!(t.titles(), ["Today", "secretary-server deployed", "Fri, 30 Oct", "Move backups to p1"]);
}

#[test]
fn a_failed_refresh_keeps_the_page_and_says_so() {
    let mut t = start(sample_json(NOW));
    t.fake.lock().unwrap().offline.store(true, std::sync::atomic::Ordering::SeqCst);
    t.h.key(keys::KEY_R);
    t.settle();
    assert_eq!(t.app().error.as_deref(), Some("no connection"));
    assert_eq!(t.app().status().1, "Can't reach the server");
    assert!(t.titles().contains(&"gmaps-server".to_owned()));
    t.fake.lock().unwrap().offline.store(false, std::sync::atomic::Ordering::SeqCst);
    t.h.key(keys::KEY_R);
    t.settle();
    assert_eq!(t.app().error, None);
}

#[test]
fn r_asks_past_the_servers_cache() {
    let mut t = start(sample_json(NOW));
    t.h.key(keys::KEY_R);
    t.settle();
    let calls = t.fake.lock().unwrap().calls.lock().unwrap().clone();
    assert_eq!(calls, [false, true]);
}

#[test]
fn a_broken_source_says_which_and_the_rest_renders() {
    let mut d = sample_json(NOW);
    d["sources"]["ops"] = json!({"ok": false, "at": null, "error": "x"});
    let o: Overview = serde_json::from_value(d).unwrap();
    assert_eq!(o.broken_sources(), ["Ops records unavailable — the timeline may be incomplete"]);
}

#[test]
fn a_server_with_more_or_fewer_fields_still_parses() {
    let o: Overview = serde_json::from_value(json!({"overall": "new-state", "hosts": [{"id": "a", "state": "melted", "future": 1}], "attention": [{"key": "k", "severity": "warn"}]})).unwrap();
    assert_eq!(o.overall, State::Unknown);
    assert_eq!(o.hosts[0].state, State::Unknown);
    assert_eq!(o.attention[0].title, "");
}

#[test]
fn helpers_match_the_web() {
    assert_eq!(model::disk_level(79.9), State::Ok);
    assert_eq!(model::disk_level(80.0), State::Warn);
    assert_eq!(model::disk_level(90.0), State::Crit);
    assert_eq!(model::gb(422.0), "422 GB");
    assert_eq!(model::gb(1800.0), "1.8 TB");
    assert_eq!(model::num(33.3, 0), "33");
    assert_eq!(model::num(9.5, 2), "9.5");
    assert_eq!(model::num(9.58, 2), "9.58");
    let iso = |min: i64| {
        let d = sample_json(NOW - min * 60_000);
        d["generated_at"].as_str().unwrap().to_owned()
    };
    // `generated_at` is one minute before the time given.
    assert_eq!(model::duration(&iso(11), NOW), "12 min");
    assert_eq!(model::duration(&iso(179), NOW), "3 hr");
    assert_eq!(model::duration(&iso(2 * 1440 - 1), NOW), "2 days");
    assert_eq!(model::series_stats(&[Some(1.0), None, Some(3.0), Some(2.0)]), Some((2.0, 1.0, 3.0)));
    assert_eq!(model::series_stats(&[None]), None);
    assert_eq!(model::chart_scale(&[Some(1.0), Some(3.0)], Some((0.0, 100.0)), None), (0.0, 100.0));
    assert_eq!(model::chart_scale(&[Some(2.0), Some(2.0)], None, None), (1.5, 2.5));
    assert_eq!(model::chart_scale(&[Some(0.1), Some(5.1)], None, Some(0.0)), (0.0, 5.6));
    assert_eq!(model::chart_scale(&[None], None, None), (0.0, 1.0));
}

#[test]
fn a_project_whose_id_matches_keeps_all_its_components() {
    let c = doc();
    let by_project = model::filter_services(&c.services, "secretary");
    assert_eq!(by_project[0].projects[0].components.len(), 4);
    let by_host = model::filter_services(&c.services, "P1");
    assert_eq!(by_host.len(), 1);
    assert_eq!(by_host[0].name, "Pokoin");
    let healthy = model::healthy_by_group(&c.services);
    assert_eq!(healthy[0].1.iter().map(|x| x.name.as_str()).collect::<Vec<_>>(), ["Secretary server", "SecretaryIOS"]);
}

#[test]
fn the_keyboard_alone_goes_everywhere() {
    let mut t = start(sample_json(NOW));
    for (code, s) in [(keys::KEY_2, Section::Services), (keys::KEY_3, Section::Incidents), (keys::KEY_4, Section::Deploys), (keys::KEY_1, Section::Overview)] {
        t.h.key(code);
        t.settle();
        assert_eq!(t.app().section, s);
        assert!(!t.app().lines.is_empty());
    }
}

#[test]
fn a_dead_token_reports_signed_out() {
    assert_eq!(format!("{:?}", ServerError::SignedOut), "SignedOut");
    let f = Fake::new(sample_json(NOW));
    f.signed_out.store(true, std::sync::atomic::Ordering::SeqCst);
    let mut f = f;
    assert_eq!(f.overview(false).unwrap_err(), ServerError::SignedOut);
}

#[test]
fn paint_does_not_panic_at_the_small_or_large_screen() {
    for (w, h) in [(800, 600), (1920, 1080)] {
        let mut a = ServerApp::new(Zone::utc(), None);
        a.fixed_now = Some(NOW);
        a.restore(doc());
        let mut h = Harness::new(a, w, h);
        for act in [Act::OpenHost("buildhost".into()), Act::OpenHost("pi-home".into()), Act::OpenItem("catalog:gmaps-server".into()), Act::OpenItem("gone".into()), Act::OpenComponent("lucina-maps".into())] {
            h.app().act(act, None);
            h.core.input(lucina_app::Input::Wheel(0));
            h.core.paint();
        }
        let _ = h.ppm();
    }
}

/// Everything in `want` is in `got` with the same value, numbers as numbers. Keys this app does not read are listed.
fn holds(want: &serde_json::Value, got: &serde_json::Value, at: &str) {
    use serde_json::Value::*;
    match (want, got) {
        (Object(w), Object(g)) => {
            for (k, v) in w {
                if matches!(k.as_str(), "sources" if at.contains(".attention")) || k == "ops_id" {
                    continue;
                }
                let g = g.get(k).unwrap_or_else(|| panic!("{at}.{k} was dropped"));
                holds(v, g, &format!("{at}.{k}"));
            }
        }
        (Array(w), Array(g)) => {
            assert_eq!(w.len(), g.len(), "{at}");
            for (i, (a, b)) in w.iter().zip(g).enumerate() {
                holds(a, b, &format!("{at}[{i}]"));
            }
        }
        (Number(a), Number(b)) => assert_eq!(a.as_f64(), b.as_f64(), "{at}"),
        (Null, Bool(false)) | (Bool(false), Null) | (Null, Array(_)) => {}
        (a, b) => assert_eq!(a, b, "{at}"),
    }
}

/// The contract: `overview-server.json` is what secretary-server's own `build` produced from its own test fixture, at the revision in
/// `docs/research/juno-server-health.md` (`scripts/server-health-capture.sh <rev>`). Every field reads back, and every page draws it.
#[test]
fn what_the_servers_own_builder_produces_reads_back_field_by_field() {
    let want: serde_json::Value = serde_json::from_str(include_str!("overview-server.json")).unwrap();
    let o: Overview = serde_json::from_value(want.clone()).unwrap();
    assert!(!o.hosts.is_empty() && !o.attention.is_empty() && !o.services.is_empty() && !o.timeline.is_empty());
    holds(&want, &serde_json::to_value(&o).unwrap(), "overview");
    let now = lucina_account::timeline::parse_rfc3339_ms(&o.generated_at).unwrap();
    let zone = Zone::utc();
    let c = Ctx { o: &o, zone: &zone, now };
    let opened = pages::Opened::default();
    assert!(format!("{:?}", pages::overview(&c, &opened)).contains("gmaps-server"));
    for a in &o.attention {
        assert!(!pages::item(&c, a).is_empty());
    }
    for h in &o.hosts {
        assert!(!pages::host(&c, h).is_empty());
    }
    assert!(!pages::services(&c, &opened).is_empty());
    assert!(!pages::timeline(&c, &opened, &[model::Kind::Deploy, model::Kind::Decision], "none").is_empty());
}
