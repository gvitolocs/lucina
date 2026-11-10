//! Every screen pair with sample data: what the mockup renders (`examples/mockup.rs`) and what the contrast test holds
//! to 4.5:1 (3:1 for large or bold text), measured on the dithered pixels each text run lands on.

use fortuna_library::{cmp_names, hit, kind, order, Day, Hit, Kind, SortKey};

use crate::browse::{self, Entry, Game, GamesFocus, Line, System};
use crate::canvas::Canvas;
use crate::icons as ic;
use crate::layout::{self as l, Key};
use crate::screens::{self, Activity, Bar, Button, Dash, HomeFocus, Pin, Row, Service, Stat, Wifi};
use crate::skin;

pub const W: usize = l::SCREEN_W as usize;
pub const H: usize = l::SCREEN_H as usize;
const YEAR: u16 = 2026;
const VERSION: &str = "1.51";

/// Every screen pair, by name: top above bottom.
pub const SCREENS: &[&str] = &[
    "1-home",
    "1-home-transfer",
    "1-home-trouble",
    "1-home-pin",
    "2-games",
    "2-games-next",
    "2-games-filter",
    "3-search",
    "4-files-root",
    "4-files-games",
    "5-system",
    "6-launch",
    "6-cant-start",
    "6-cant-open",
    "6-restart",
    "7-boot",
    "8-lucina",
];

fn bar(wifi: Wifi) -> Bar {
    Bar { hh: 14, mm: 32, wifi, battery: 11, charging: false }
}

fn dash(wifi: Wifi, activity: Activity<'static>) -> Dash<'static> {
    Dash {
        bar: bar(wifi),
        date: "Sun 8 Nov",
        ip: [192, 168, 1, 73],
        up_s: 2 * 3600 + 14 * 60 + 9,
        activity,
        wd: [Service::Ok; 3],
        wd_paused_s: 0,
        recovered: false,
    }
}

const fn game(name: &'static str, folder: &'static str, system: System, size: u64, played_ago: Option<u32>) -> Game<'static> {
    Game { name, folder, system, size, played_ago, art: None }
}

/// A stand-in banner icon for the mockups (a DS game's own icon goes here; no game's art is copied): two nested
/// rounded squares in three colours, in the banner's tile layout.
const fn sample_tiles() -> [u8; 512] {
    let mut t = [0u8; 512];
    let mut py = 0;
    while py < 32 {
        let mut px = 0;
        while px < 32 {
            let (dx, dy) = (if px < 16 { 15 - px } else { px - 16 }, if py < 16 { 15 - py } else { py - 16 });
            let d = if dx > dy { dx } else { dy };
            let n: u8 = if d >= 15 { 0 } else if d >= 11 { 1 } else if d >= 6 { 2 } else { 3 };
            let tile = (py / 8) * 4 + px / 8;
            let i = tile * 64 + (py % 8) * 8 + px % 8;
            t[i / 2] |= if i % 2 == 0 { n } else { n << 4 };
            px += 1;
        }
        py += 1;
    }
    t
}
static SAMPLE_TILES: [u8; 512] = sample_tiles();
static SAMPLE_PALETTE: [u16; 16] = [0, 0x5d4a, 0x7fff, 0x2d6b, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];

/// The card's games (sample data; the real list comes from the index).
pub const LIBRARY: [Game<'static>; 11] = [
    game("Example Game.gbc", "sd:/Games", System::Gb, 2_097_152, Some(2 * 3600 + 300)),
    game("Advance Wars.gba", "sd:/Games", System::Gba, 8_388_608, Some(26 * 3600)),
    Game { art: Some(crate::browse::Art { tiles: &SAMPLE_TILES, palette: &SAMPLE_PALETTE }), ..game("Kirby Mass Attack.nds", "sd:/Games", System::Ds, 134_217_728, None) },
    game("Tetris.gb", "sd:/Games", System::Gb, 32_768, None),
    game("Zelda Minish Cap.gba", "sd:/Games", System::Gba, 16_777_216, None),
    game("example.gba", "sd:/Games", System::Gba, 16_777_216, None),
    game("Pokémon Platinum.nds", "sd:/Games/DS", System::Ds, 134_217_728, None),
    game("Example Dungeon.gba", "sd:/Games", System::Gba, 33_554_432, None),
    game("Pokémon X", "", System::Fortuna, 0, None),
    game("fireda-engine.gba", "sd:", System::Gba, 16_777_216, None),
    game("Mario Kart DS.nds", "sd:/Games/DS", System::Ds, 33_554_432, None),
];

fn played_key(gm: &Game) -> u32 {
    gm.played_ago.map_or(0, |s| u32::MAX - s)
}

/// The library matching `q` under filter `f` (0 = all), ranked: recently played first, then rank, then name.
fn ranked(q: &str, f: usize) -> ([(Game<'static>, Hit); 12], usize) {
    let mut out = [(LIBRARY[0], Hit { rank: 0, start: 0, end: 0 }); 12];
    let mut n = 0;
    for gm in LIBRARY.iter() {
        if f != 0 && gm.system.filter() != f {
            continue;
        }
        if let Some(h) = hit(q, gm.name_shown()) {
            out[n] = (*gm, h);
            n += 1;
        }
    }
    out[..n].sort_unstable_by(|a, b| {
        let k = |x: &(Game<'static>, Hit)| -> SortKey<'static> { SortKey { played: played_key(&x.0), rank: x.1.rank, name: x.0.name_shown() } };
        order(&k(a), &k(b))
    });
    (out, n)
}

/// The grid's lines: recently played, then every game A–Z, four to a line.
fn grid<'a>(f: usize, store: &'a mut [Game<'static>; 16]) -> ([Line<'a>; 6], usize, usize) {
    let (all, n) = ranked("", f);
    let recent = all[..n].iter().filter(|x| x.0.played_ago.is_some()).count().min(4);
    for (i, x) in all[..recent].iter().enumerate() {
        store[i] = x.0;
    }
    let mut rest: [Game<'static>; 16] = [LIBRARY[0]; 16];
    let mut m = 0;
    for x in all[..n].iter() {
        rest[m] = x.0;
        m += 1;
    }
    rest[..m].sort_unstable_by(|a, b| cmp_names(a.name_shown(), b.name_shown()));
    store[recent..recent + m].copy_from_slice(&rest[..m]);
    let (recent_s, all_s) = store.split_at(recent);
    let all_s = &all_s[..m];
    let mut lines = [Line { head: None, games: &[] }; 6];
    let mut k = 0;
    if recent > 0 {
        lines[0] = Line { head: Some("Recently played"), games: recent_s };
        k = 1;
    }
    for (i, chunk) in all_s.chunks(l::GRID_COLS).enumerate() {
        lines[k] = Line { head: if i == 0 { Some("All games") } else { None }, games: chunk };
        k += 1;
    }
    (lines, k, recent)
}

fn pins() -> [Pin<'static>; 3] {
    [
        Pin { name: "Pokémon X", icon: &ic::DS_24, tint: skin::SIGNAL },
        Pin { name: "FireRed", icon: &ic::GBA_24, tint: skin::WARNING },
        Pin { name: "Lucina", icon: &ic::LUCINA_24, tint: skin::ACCENT_FILL },
    ]
}

fn entry(name: &'static str, dir: bool, size: u64, d: (u16, u16, u16)) -> Entry<'static> {
    Entry { name, kind: kind(name, dir), size, date: Day::fat(d.0, d.1, d.2) }
}

fn root() -> [Entry<'static>; 10] {
    let mut v = [
        entry("_gba", true, 2, (2026, 10, 30)),
        entry("_nds", true, 14, (2026, 11, 2)),
        entry("Games", true, 23, (2026, 11, 7)),
        entry("Pictures", true, 48, (2026, 11, 1)),
        entry("Pokémon", true, 6, (2026, 11, 3)),
        entry("FORTUNA.NDS", false, 4_300_000, (2026, 11, 8)),
        entry("FORTUNA.TXT", false, 3_100, (2026, 11, 8)),
        entry("INTRO.FMV", false, 39_800_000, (2026, 10, 28)),
        entry("fireda-engine.gba", false, 16_777_216, (2026, 11, 5)),
        entry("screen-0001.png", false, 74_000, (2025, 12, 24)),
    ];
    v.sort_unstable_by(|a, b| (a.kind != Kind::Folder).cmp(&(b.kind != Kind::Folder)).then(cmp_names(a.name, b.name)));
    v
}

fn games_dir() -> [Entry<'static>; 7] {
    [
        entry("Saves", true, 3, (2026, 11, 6)),
        entry("Advance Wars.gba", false, 8_388_608, (2026, 9, 14)),
        entry("Kirby Mass Attack.nds", false, 134_217_728, (2026, 10, 2)),
        entry("Example Game.gbc", false, 2_097_152, (2026, 8, 30)),
        entry("Tetris.gb", false, 32_768, (2026, 8, 30)),
        entry("Zelda Minish Cap.gba", false, 16_777_216, (2026, 9, 14)),
        entry("cover.png", false, 51_000, (2026, 9, 14)),
    ]
}

fn system_rows() -> [Row<'static>; 4] {
    [
        Row { icon: &ic::WIFI_16, label: "Wi-Fi", ok: Some(true), value: "192.168.1.73", opens: false },
        Row { icon: &ic::UPLOAD_16, label: "FTP", ok: Some(true), value: "Port 21 · 12 connections", opens: false },
        Row { icon: &ic::INFO_16, label: "Watchdog", ok: Some(true), value: "3 running", opens: false },
        Row { icon: &ic::BATTERY_16, label: "Battery", ok: None, value: "73 %", opens: false },
    ]
}

/// Draws screen pair `name`.
pub fn render(name: &str, top: &mut Canvas, bottom: &mut Canvas) {
    let b = bar(Wifi::Connected);
    let mut store = [LIBRARY[0]; 16];
    match name {
        "1-home" | "1-home-transfer" | "1-home-trouble" | "1-home-pin" => {
            let d = match name {
                "1-home" | "1-home-pin" => dash(Wifi::Connected, Activity::Indexing { found: 142, done: 23, seen: 40 }),
                "1-home-transfer" => dash(
                    Wifi::Connected,
                    Activity::Transfer { name: "FORTUNA.NDS", receiving: true, kib: 1240, kib_s: 412, tick: 3 },
                ),
                _ => {
                    let mut d = dash(Wifi::NoAp, Activity::Idle);
                    d.wd = [Service::Ok, Service::Silent(14), Service::Ok];
                    d.recovered = true;
                    d
                }
            };
            screens::dashboard(top, &d);
            let focus = match name {
                "1-home" => Some(HomeFocus::Play),
                "1-home-pin" => Some(HomeFocus::Pin(1)),
                _ => None,
            };
            screens::home(bottom, Some(&LIBRARY[0]), &pins(), focus);
        }
        "2-games" | "2-games-next" | "2-games-filter" | "6-launch" | "6-cant-start" => {
            let f = if name == "2-games-filter" { 2 } else { 0 };
            let (lines, n, _) = grid(f, &mut store);
            // the selection on the grid's second line: All games, second cover
            // 2-games-next: the d-pad one cover to the right of 2-games
            let (sel_line, sel_col) = match (f, name) {
                (0, "2-games-next") => (1, 2),
                (0, _) => (1, 1),
                _ => (1, 0),
            };
            let first = l::first_line(sel_line, n);
            let sel = lines[sel_line].games[sel_col];
            // a dialog is about Advance Wars: the top shows that game
            let shown = if name == "6-launch" || name == "6-cant-start" { LIBRARY[1] } else { sel };
            browse::game_card(top, &b, &shown);
            let indexing = if name == "2-games-filter" { Some(142) } else { None };
            // under a dialog the focus is on the dialog, not on a cover
            let focus = if name.starts_with("6-") { None } else { Some(GamesFocus::Cell(sel_line, sel_col)) };
            browse::games(bottom, f, &lines[..n], first, focus, indexing);
            if name == "6-launch" || name == "6-cant-start" {
                if name == "6-launch" {
                    screens::alert(
                        bottom,
                        "Play Advance Wars?",
                        "Wi-Fi and FTP stop while you play.",
                        &[("Cancel", Button::Plain), ("Play", Button::Primary)],
                        Some(1),
                    );
                } else {
                    screens::alert(
                        bottom,
                        "Can't start Advance Wars",
                        "The GBA BIOS is missing: sd:/_gba/bios.bin",
                        &[("Close", Button::Primary)],
                        Some(0),
                    );
                }
            }
        }
        "3-search" => {
            let (found, n) = ranked("poke", 0);
            browse::results_top(top, &b, "poke", &found[..n], n as u32);
            browse::search(bottom, "poke", &found[..n], n as u32, false, Some(Key::Char(b'e')));
        }
        "4-files-root" | "6-cant-open" => {
            let r = root();
            let first = if name == "6-cant-open" { 4 } else { 0 };
            let focus = if name == "6-cant-open" { 3 } else { 2 };
            let sel = r[first + focus as usize];
            browse::folder_top(top, &b, &[], "SD card", "10 items · 1.9 GB free", Some(&sel), YEAR);
            browse::files(bottom, None, &r[first..], first, r.len(), YEAR, Some(focus));
            if name == "6-cant-open" {
                screens::alert(
                    bottom,
                    "Can't open INTRO.FMV",
                    "Fortuna has no viewer for .fmv files.",
                    &[("Close", Button::Primary)],
                    Some(0),
                );
            }
        }
        "4-files-games" => {
            let e = games_dir();
            browse::folder_top(top, &b, &["SD card"], "Games", "7 items · 186 MB", Some(&e[1]), YEAR);
            browse::files(bottom, Some("SD card"), &e, 0, e.len(), YEAR, Some(2));
        }
        "5-system" | "6-restart" => {
            screens::system_top(top, &b, VERSION, 231, &Stat { icon: &ic::GAMEPAD_12, label: "Games", value: "214", line: "in the index" });
            screens::system(bottom, &system_rows(), false, if name == "5-system" { Some(l::SYSTEM_ROWS) } else { None });
            if name == "6-restart" {
                screens::alert(
                    bottom,
                    "Restart Fortuna?",
                    "Wi-Fi and FTP pause for a moment.",
                    &[("Cancel", Button::Plain), ("Restart", Button::Primary)],
                    Some(1),
                );
            }
        }
        "7-boot" => {
            screens::boot(top, 2, 4, "Starting Wi-Fi");
            skin::bottom_backdrop(bottom);
        }
        "8-lucina" => {
            screens::lucina_top(top, &b, "Chat, agents and voice notes");
            screens::lucina(bottom, false);
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::canvas::Audit;

    #[test]
    fn every_text_meets_its_contrast_floor() {
        let mut fails = std::vec::Vec::new();
        let mut bottom_controls = 0;
        for name in SCREENS {
            let (mut tp, mut bp) = (std::vec![0x8000u16; W * H], std::vec![0x8000u16; W * H]);
            let (mut ta, mut ba) = (Audit::default(), Audit::default());
            {
                let mut t = Canvas::new(&mut tp, l::SCREEN_W, l::SCREEN_H, l::SCREEN_W);
                let mut b = Canvas::new(&mut bp, l::SCREEN_W, l::SCREEN_H, l::SCREEN_W);
                t.audit = Some(&mut ta);
                b.audit = Some(&mut ba);
                render(name, &mut t, &mut b);
            }
            // the top screen takes no touch: it holds content and status, never a control
            if ta.controls > 0 {
                fails.push(std::format!("{name} top: {} control(s) drawn on the top screen", ta.controls));
            }
            bottom_controls += ba.controls;
            for (side, a) in [("top", &ta), ("bottom", &ba)] {
                if a.splits > 0 {
                    fails.push(std::format!("{name} {side}: {} wrapped line(s) split a word", a.splits));
                }
                // a name label never covers another run of text (its own text lies inside it)
                for &(lx, ly, lw, lh) in &a.labels[..a.n_labels] {
                    for r in &a.recs[..a.n] {
                        let (x, y, w, h) = r.rect;
                        let inside = x >= lx && y >= ly && x + w <= lx + lw && y + h <= ly + lh;
                        let apart = x >= lx + lw || lx >= x + w || y >= ly + lh || ly >= y + h;
                        if !inside && !apart {
                            fails.push(std::format!("{name} {side}: the name label covers {:?}", r.text()));
                        }
                    }
                }
                assert!(a.n > 0 || (*name == "7-boot" && side == "bottom"), "{name} {side} drew no text");
                for r in &a.recs[..a.n] {
                    if r.ratio < r.floor() {
                        fails.push(std::format!(
                            "{name} {side}: {:?} {}.{:02}:1 < {}.{}:1 ({} px{})",
                            r.text(),
                            r.ratio / 100,
                            r.ratio % 100,
                            r.floor() / 100,
                            r.floor() % 100 / 10,
                            r.px,
                            if r.bold { " bold" } else { "" }
                        ));
                    }
                }
            }
        }
        assert!(bottom_controls > 0, "no control noted on any bottom screen: the control check is not hooked up");
        let cuts = crate::screens::CUTS.load(std::sync::atomic::Ordering::Relaxed);
        if cuts > 0 {
            fails.push(std::format!("{cuts} text(s) cut because a buffer was full"));
        }
        assert!(fails.is_empty(), "layout checks failed:\n{}", fails.join("\n"));
    }

    #[test]
    fn grid_lines_and_ranking() {
        let mut store = [LIBRARY[0]; 16];
        let (lines, n, recent) = grid(0, &mut store);
        assert_eq!(recent, 2);
        assert_eq!(lines[0].head, Some("Recently played"));
        assert_eq!(lines[0].games[0].name, "Example Game.gbc", "the most recent first");
        assert_eq!(lines[1].head, Some("All games"));
        assert_eq!(n, 1 + 3);
        let (found, k) = ranked("poke", 0);
        assert_eq!(found[0].0.name, "Example Game.gbc", "recently played stay first");
        assert_eq!(k, 5);
    }
}
