// SPDX-License-Identifier: MPL-2.0

//! Every golden stream in `tests/golden/` (written by the replay tests of `lucina-i2c-hid`) read with [`StreamReader`]: what
//! a consumer, the window manager's rules for one, takes from the device. Each must decode whole, pair its begins with its
//! ends, and match what `rules::encode_step` writes for it.

use lucina_gesture::rules::{self, RuleKind, RulePhase};
use lucina_gesture::{GestureRecord, Kind, Phase, StreamReader};
use std::fs;
use std::path::PathBuf;

fn golden() -> Vec<(String, Vec<u8>)> {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/golden");
    let mut v: Vec<_> = fs::read_dir(&dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|x| x == "lgst"))
        .map(|p| {
            (
                p.file_stem().unwrap().to_string_lossy().into_owned(),
                fs::read(&p).unwrap(),
            )
        })
        .collect();
    v.sort();
    v
}

/// The records of a golden stream, every one decoded.
fn records(bytes: &[u8]) -> Vec<GestureRecord> {
    StreamReader::new(bytes)
        .expect("a header this crate reads")
        .map(|r| r.expect("a record"))
        .collect()
}

#[test]
fn there_are_fifteen_golden_streams() {
    let names: Vec<_> = golden().into_iter().map(|g| g.0).collect();
    for want in [
        "swipe3-left",
        "swipe3-right",
        "swipe3-up",
        "swipe3-down",
        "swipe4-up",
        "swipe4-down",
        "pinch2-out",
        "pinch2-in",
        "pinch4-in",
        "pinch4-out",
        "pinch2-rotate",
        "hold3-lift",
        "swipe3-to-4",
        "scroll2-down",
        "pointer1",
    ] {
        assert!(
            names.iter().any(|n| n == want),
            "{want} is missing from {names:?}"
        );
    }
}

#[test]
fn every_golden_stream_decodes_and_pairs() {
    for (name, bytes) in golden() {
        let rs = records(&bytes);
        assert_eq!((bytes.len() - 32) / 32, rs.len(), "{name}");
        let mut open = None;
        let mut next_seq = rs.first().map_or(0, |r| r.seq);
        let mut last_time = 0;
        for (i, r) in rs.iter().enumerate() {
            assert!(r.time_us >= last_time, "{name} #{i}");
            last_time = r.time_us;
            let kind = match r.kind {
                Kind::Swipe => RuleKind::Swipe,
                Kind::Pinch => RuleKind::Pinch,
                Kind::Hold => RuleKind::Hold,
            };
            let ev = match r.phase {
                Phase::Begin => rules::begin(kind, r.fingers),
                Phase::Update => rules::update(kind),
                Phase::End => rules::end(kind, r.cancelled()),
            };
            let s = rules::encode_step(open, next_seq, ev);
            let e = if r.phase == Phase::Begin {
                s.second
            } else {
                s.first
            };
            let e = e.unwrap_or_else(|| panic!("{name} #{i}: {r:?} against {open:?}"));
            assert!(
                r.phase != Phase::Begin || s.first.is_none(),
                "{name} #{i}: a begin over an open gesture"
            );
            assert_eq!((e.fingers, e.seq), (r.fingers, r.seq), "{name} #{i}");
            assert_eq!(
                e.phase == RulePhase::End,
                r.phase == Phase::End,
                "{name} #{i}"
            );
            open = s.open;
            next_seq = s.next_seq;
        }
        assert!(open.is_none(), "{name}: ends with a gesture open");
    }
}
