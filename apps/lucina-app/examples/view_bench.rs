//! One app's `view` pass, timed on the host: a Notes-like list screen driven through the same [`Core`] the
//! runtime drives, for N frames. `lucina-glass`'s `frame_bench` times the glass canvas; this times the
//! toolkit, which is what every app's frame is made of. CPU time, not wall time, so a loaded host compares.
//!
//! `cargo run --release -p lucina-app --features prof --example view_bench -- [frames] [--cpu N] [--scroll] [--parts]`
//!
//! With `LUCINA_APP_FULL_REDRAW=1` every frame draws the whole screen instead of only the tiles the last frame
//! differs in: the cost of a frame where everything moves. Being *set* is what counts, empty included, so a
//! script that exports it unconditionally measures a whole-frame redraw while it believes it measures a dirty
//! one — which is how one round of the step-3d comparison went wrong before anyone noticed. `--scroll` moves the list one notch a frame, which is
//! what a person on the wheel actually gets: the damaged tiles are redrawn and the rest is a compare.
//! `--type` puts a character in the field every fourth frame, `--hover` moves the pointer over a control every
//! frame: the two other ways an app's screen is redrawn.
//!
//! `--parts` needs the `prof` feature and prints where the frame's CPU time went: the clocked regions and the
//! counts of the work they did. See `src/prof.rs`.

use std::time::Instant;

use lucina_app::app::Core;
use lucina_app::keys;
use lucina_app::prof;
use lucina_app::{App, Face, Input, ListState, Rect, TextEdit, Ui};

/// Milliseconds of CPU time (all threads) so far.
fn cpu_ms() -> f64 {
    let mut ts = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: writes one timespec the caller owns.
    unsafe { libc::clock_gettime(libc::CLOCK_PROCESS_CPUTIME_ID, &mut ts) };
    ts.tv_sec as f64 * 1000.0 + ts.tv_nsec as f64 / 1e6
}

fn pin(cpu: usize) -> bool {
    // SAFETY: a zeroed cpu_set_t is the empty set; CPU_SET writes inside it; sched_setaffinity reads `size_of` bytes of it.
    unsafe {
        let mut set: libc::cpu_set_t = std::mem::zeroed();
        libc::CPU_SET(cpu, &mut set);
        libc::sched_setaffinity(0, std::mem::size_of::<libc::cpu_set_t>(), &set) == 0
    }
}

struct Row {
    title: String,
    snippet: String,
    date: String,
}

/// The screen: a rail, a search field, two buttons, a heading, a paragraph and a list of forty notes.
/// Text in every role the toolkit has, in Latin, Cyrillic and the marks `★ ✓ · —` the UI face does not carry.
struct Scene {
    search: TextEdit,
    rail: ListState,
    list: ListState,
    rows: Vec<Row>,
    labels: Vec<String>,
}

impl Scene {
    fn new() -> Scene {
        let titles = [
            "Pasta night",
            "Rust ideas",
            "Meeting briefing",
            "Лекция по distributed systems",
            "Dentist — Nørregarde 12",
            "★ Favourite: Grandma's birthday",
            "Unfinished Letters · notes",
        ];
        let snippets = [
            "Everything every app needs: a list, a field, a picture, a dialog and the house type.",
            "Alt det en note har: en overskrift, brødtekst, en dato og et par mærker. ★ ✓",
            "The queue is only the shortest way to somewhere you did not mean to go.",
        ];
        Scene {
            search: TextEdit::with_text("ideas"),
            rail: ListState::default(),
            list: ListState::default(),
            rows: (0..40)
                .map(|i| Row {
                    title: format!("{} {}", titles[i % titles.len()], i),
                    snippet: snippets[i % snippets.len()].to_owned(),
                    date: format!("2026-11-{:02} 14:{:02}", 2 + (i % 26), i % 60),
                })
                .collect(),
            labels: [
                "Notes",
                "Reminders",
                "Archive",
                "Trash",
                "Labels",
                "Work",
                "Home",
                "Soon",
            ]
            .iter()
            .map(|s| s.to_string())
            .collect(),
        }
    }
}

impl App for Scene {
    type Msg = ();

    fn view(&mut self, ui: &mut Ui) {
        let t = ui.theme();
        ui.header("Notes");
        let rail = ui.cut_left(t.px(230.0));
        ui.fill(rail, t.bar);
        let labels = &self.labels;
        let rail_r = Rect::new(rail.x + 1, rail.y, rail.w - 1, rail.h);
        ui.list(
            "rail",
            &mut self.rail,
            rail_r,
            labels.len(),
            32.0,
            |ui, i, _| {
                let a = ui.area();
                ui.text_in(
                    Rect::new(a.x + t.px(14.0), a.y, a.w - t.px(20.0), a.h),
                    Face::Sans,
                    t.pt(15.0),
                    t.item,
                    &labels[i],
                );
            },
        );
        let search = ui.cut_top(t.px(44.0));
        ui.text_field_at(search, "search", &mut self.search);
        ui.space(8.0);
        ui.row(34.0, |ui| {
            ui.button("New note");
            ui.primary_button("Save");
        });
        ui.space(8.0);
        ui.heading("Ideas");
        ui.paragraph(
            "A note on several lines, wrapped to the width of the column it is in: the toolkit measures every \
             word, cuts the line where it no longer fits, and draws what is left with an ellipsis. ★ Cyrillic: \
             «Идея для следующего раза», and a tick ✓ for the ones that are done.",
        );
        ui.space(8.0);
        let rows = &self.rows;
        let area = ui.area();
        ui.list(
            "rows",
            &mut self.list,
            area,
            rows.len(),
            60.0,
            |ui, i, sel| {
                let a = ui.area();
                if sel {
                    ui.fill(a, t.selection);
                }
                let inner = a.inset(t.px(8.0));
                let right = inner.right();
                ui.label(&rows[i].title);
                ui.dim(&rows[i].snippet);
                let d = &rows[i].date;
                let w = ui.measure(Face::Mono, t.pt(13.0), d);
                ui.text_in(
                    Rect::new(right - w - t.px(8.0), inner.y, w, inner.h),
                    Face::Mono,
                    t.pt(13.0),
                    t.dim,
                    d,
                );
            },
        );
    }
}

fn main() {
    let mut args = std::env::args().skip(1);
    let (mut frames, mut cpu) = (300usize, None);
    let (mut scroll, mut typing, mut hover, mut parts) = (false, false, false, false);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--cpu" => cpu = args.next().and_then(|v| v.parse().ok()),
            "--scroll" => scroll = true,
            "--type" => typing = true,
            "--hover" => hover = true,
            "--parts" => parts = true,
            f if f.parse::<usize>().is_ok() => frames = f.parse().unwrap(),
            _ => {
                eprintln!(
                    "usage: view_bench [frames] [--cpu N] [--scroll] [--type] [--hover] [--parts]"
                );
                std::process::exit(2);
            }
        }
    }
    let full = std::env::var_os("LUCINA_APP_FULL_REDRAW").is_some();
    if let Some(c) = cpu
        && !pin(c)
    {
        eprintln!(
            "view_bench: cannot pin to CPU {c}: {}",
            std::io::Error::last_os_error()
        );
        std::process::exit(1);
    }
    // What a region costs before it times anything, so a part smaller than that is a rounding.
    let clock = prof::calibrate();
    if parts && clock == 0 {
        eprintln!("view_bench --parts: built without the `prof` feature, so nothing is measured");
        std::process::exit(2);
    }
    let boot = Instant::now();
    let mut core = Core::new(Scene::new(), 1920, 1080);
    let made = boot.elapsed().as_secs_f64() * 1000.0;
    let c0 = cpu_ms();
    let dirty_first = core.paint().len();
    let cold = cpu_ms() - c0;
    let cold_parts = prof::read();
    prof::reset();
    if typing {
        // The field has the caret: everything typed after this lands in it.
        core.input(Input::Down(700, 106));
        core.input(Input::Up(700, 106));
        core.paint();
    }
    let mut v: Vec<f64> = Vec::with_capacity(frames);
    let mut rects = 0usize;
    for i in 0..frames {
        if scroll {
            // Down for twenty frames, then back up, so the list keeps moving inside its range.
            // The pointer sits over the rows list, or the wheel would land on the header.
            core.input(Input::Move(960, 600));
            core.input(Input::Scroll {
                dx: 0,
                dy: if (i / 20) % 2 == 0 { 12 } else { -12 },
            });
        } else if typing {
            if i % 4 == 0 {
                let c = "abcdefghijklmnopqrstuvwxyz"[i / 4 % 26..][..1].to_owned();
                type_str(&mut core, &c);
            }
        } else if hover {
            // Over a row, then over the button: the hover state is what has to be redrawn.
            core.input(if i % 2 == 0 {
                Input::Move(960, 600)
            } else {
                Input::Move(160, 166)
            });
        } else {
            core.input(Input::Wheel(0));
        }
        let a = cpu_ms();
        rects += core.paint().len();
        v.push(cpu_ms() - a);
    }
    let spent = prof::read();
    v.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let mean = v.iter().sum::<f64>() / v.len() as f64;
    let where_ = cpu.map_or(String::new(), |c| format!(" @cpu{c}"));
    println!(
        "{frames} frames ({}{where_}): mean {mean:.2} ms  p50 {:.2}  p95 {:.2}  max {:.2} | cold first frame {cold:.2} ms ({dirty_first} dirty rects) | list frames: {} rects on average | core built in {made:.1} ms",
        if full {
            "whole frame"
        } else if scroll {
            "scrolling"
        } else if typing {
            "typing"
        } else if hover {
            "hovering"
        } else {
            "idle"
        },
        v[v.len() / 2],
        v[v.len() * 95 / 100],
        v[v.len() - 1],
        rects / frames,
    );
    if parts {
        report("cold first frame", 1, &cold_parts, cold, clock, full);
        report(
            if full {
                "whole frame"
            } else if scroll {
                "scrolling"
            } else if typing {
                "typing"
            } else if hover {
                "hovering"
            } else {
                "idle"
            },
            frames as u64,
            &spent,
            mean,
            clock,
            full,
        );
    }
}

/// One key press per character, the US layout, as `app::Harness` types.
fn type_str(core: &mut Core<Scene>, s: &str) {
    for c in s.chars() {
        let (code, shift) = keys::code_for(c).unwrap_or_else(|| panic!("cannot type {c:?}"));
        core.input(Input::Key(keys::Press {
            code,
            mods: keys::Mods {
                shift,
                ..keys::Mods::default()
            },
        }));
    }
}

/// The regions and the counts of one run, per frame, with the clock's own cost taken off.
fn report(
    name: &str,
    frames: u64,
    spent: &(Vec<u64>, Vec<u64>, Vec<u64>),
    total: f64,
    clock: u64,
    full: bool,
) {
    let (ns, calls, count) = spent;
    println!(
        "  parts of a {name} frame, {frames} frames, ms per frame, one region costs {clock} ns to time:"
    );
    let mut top = 0.0;
    for (i, n) in prof::REGIONS.iter().enumerate() {
        let calls = calls[i] / frames;
        let work = ns[i].saturating_sub(calls * clock) as f64 / 1e6 / frames as f64;
        if i > prof::REPLAY {
            // Nested: FILL, SHAPE, the walks and RASTER are work the passes above already hold.
            println!("      of which {n:<22} {work:>7.3}   {calls:>8} calls");
            continue;
        }
        top += work;
        println!("    {n:<26} {work:>7.3}   {calls:>8} calls");
    }
    println!(
        "    {:<26} {:>7.3}   (the three passes above)",
        "sum of what is timed", top
    );
    println!(
        "    {:<26} {:>7.3}   (the frame cost {total:.3} ms)",
        "not covered by a region",
        total - top
    );
    println!("  work counted per frame:");
    for (i, n) in prof::COUNTERS.iter().enumerate() {
        println!("    {n:<26} {:>9}", count[i] / frames);
    }
    if full {
        println!(
            "    (a whole-frame redraw records nothing: the drawing happens inside the view pass)"
        );
    }
}
