use lucina_events::wire::{Event, Filter, Source};
use serde_json::Map;

fn ev(kind: &str, app: Option<&str>, subject: Option<&str>, src: Source) -> Event {
    Event {
        seq: 0,
        src,
        src_seq: 0,
        t_ms: 0,
        kind: kind.to_string(),
        app: app.map(|a| a.to_string()),
        subject: subject.map(|s| s.to_string()),
        fields: Map::new(),
    }
}

fn kind_filter(patterns: &[&str]) -> Filter {
    Filter {
        kind: patterns.iter().map(|p| p.to_string()).collect(),
        ..Default::default()
    }
}

#[test]
fn kind_exact() {
    let f = kind_filter(&["media.ended"]);
    assert!(f.matches(&ev("media.ended", None, None, Source::App)));
    assert!(!f.matches(&ev("media.started", None, None, Source::App)));
    assert!(!f.matches(&ev("media", None, None, Source::App)));
}

#[test]
fn kind_prefix() {
    let f = kind_filter(&["media.*"]);
    assert!(f.matches(&ev("media.ended", None, None, Source::App)));
    assert!(f.matches(&ev("media.a.b", None, None, Source::App)));
    assert!(!f.matches(&ev("media", None, None, Source::App)));
    assert!(!f.matches(&ev("mediaplayer.x", None, None, Source::App)));
}

#[test]
fn kind_patterns_or() {
    let f = kind_filter(&["media.ended", "power.*"]);
    assert!(f.matches(&ev("media.ended", None, None, Source::App)));
    assert!(f.matches(&ev("power.low", None, None, Source::App)));
    assert!(!f.matches(&ev("media.started", None, None, Source::App)));
}

#[test]
fn app_src_subject_and() {
    let mut f = kind_filter(&["media.*"]);
    f.app = Some("player".to_string());
    f.src = vec![Source::App, Source::Kernel];
    f.subject_prefix = Some("track".to_string());
    assert!(f.matches(&ev("media.ended", Some("player"), Some("track1"), Source::App)));
    assert!(f.matches(&ev("media.ended", Some("player"), Some("track1"), Source::Kernel)));
    assert!(!f.matches(&ev("media.ended", Some("other"), Some("track1"), Source::App)));
    assert!(!f.matches(&ev("media.ended", None, Some("track1"), Source::App)));
    assert!(!f.matches(&ev("media.ended", Some("player"), Some("track1"), Source::Shell)));
    assert!(!f.matches(&ev("media.ended", Some("player"), Some("album1"), Source::App)));
    assert!(!f.matches(&ev("media.ended", Some("player"), None, Source::App)));
    assert!(!f.matches(&ev("power.low", Some("player"), Some("track1"), Source::App)));
}

#[test]
fn empty_filter_takes_all() {
    let f = Filter::default();
    assert!(f.matches(&ev("anything.at_all", None, None, Source::Kernel)));
    assert_eq!(f.mask(), 0);
}

#[test]
fn mask_bits() {
    let mut f = Filter {
        src: vec![Source::Shell],
        ..Default::default()
    };
    assert_eq!(f.mask(), 1 << 2);
    f.src.push(Source::Kernel);
    assert_eq!(f.mask(), (1 << 2) | (1 << 3));
}

#[test]
fn check_refuses_bad_patterns() {
    for bad in ["media*", "*", "Media.x", "a..b", ".media", "media.", ""] {
        let f = kind_filter(&[bad]);
        assert!(f.check().is_err(), "{bad} should be refused");
    }
    for good in ["media.ended", "media.*", "a", "a_b.c9.*"] {
        let f = kind_filter(&[good]);
        assert!(f.check().is_ok(), "{good} should pass");
    }
}

#[test]
fn filter_serde_defaults() {
    let f: Filter = serde_json::from_str("{}").unwrap();
    assert_eq!(f, Filter::default());
    let v = serde_json::to_value(&f).unwrap();
    assert_eq!(v, serde_json::json!({"kind": [], "app": null, "src": [], "subject_prefix": null}));
}

#[test]
fn only_tree_kinds_make_apps_build_trees() {
    let f = |k: &[&str]| lucina_events::wire::Filter { kind: k.iter().map(|s| s.to_string()).collect(), ..Default::default() };
    assert!(lucina_events::hub::wants_tree(&f(&["node.changed"])));
    assert!(lucina_events::hub::wants_tree(&f(&["media.ended", "view.*"])));
    // A reader of everything (lucina-devd's forwarder) does not: a tree after every paint costs a view pass.
    assert!(!lucina_events::hub::wants_tree(&f(&[])));
    assert!(!lucina_events::hub::wants_tree(&f(&["media.*", "nodes.x"])));
}
