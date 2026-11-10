//! Chat, Agents and the sign-in (talk.rs) drawn by the real crate into PNG pairs, as `mockup` does for the shell.
//!
//! cargo run -p fortuna-look --example talk -- <out dir>

use std::path::{Path, PathBuf};

use fortuna_look::canvas::Canvas;
use fortuna_look::layout::{SCREEN_H, SCREEN_W};
use fortuna_look::screens::{Bar, Wifi};
use fortuna_look::skin;
use fortuna_look::talk::*;
use fortuna_look::tokens::rgb888;

const W: usize = SCREEN_W as usize;
const H: usize = SCREEN_H as usize;

const HINGE: usize = 16;

fn write_pair(dir: &Path, name: &str, top: &[u16], bottom: &[u16]) {
    let total_h = H * 2 + HINGE;
    let mut rgb = vec![0u8; W * total_h * 3];
    let put = |rgb: &mut Vec<u8>, y0: usize, f: &[u16]| {
        for y in 0..H {
            for x in 0..W {
                let v = rgb888(f[y * W + x]);
                let i = ((y0 + y) * W + x) * 3;
                rgb[i] = (v >> 16) as u8;
                rgb[i + 1] = (v >> 8) as u8;
                rgb[i + 2] = v as u8;
            }
        }
    };
    put(&mut rgb, 0, top);
    // the hinge, as in lucina-glass layouts/fortuna.rs: a grey band with a small handle in the middle
    let (h, k) = (skin::HINGE, [0x55u8, 0x56, 0x5c]);
    let band = [h.r, h.g, h.b];
    for y in H..H + HINGE {
        for x in 0..W {
            let i = (y * W + x) * 3;
            let handle = (118..138).contains(&x) && (y == H + HINGE / 2 - 1 || y == H + HINGE / 2);
            rgb[i..i + 3].copy_from_slice(if handle { &k } else { &band });
        }
    }
    put(&mut rgb, H + HINGE, bottom);
    save(&dir.join(format!("{name}.png")), W, total_h, &rgb);
    let s = 3;
    let mut big = vec![0u8; W * s * total_h * s * 3];
    for y in 0..total_h * s {
        for x in 0..W * s {
            let (i, j) = ((y * W * s + x) * 3, ((y / s) * W + x / s) * 3);
            big[i..i + 3].copy_from_slice(&rgb[j..j + 3]);
        }
    }
    save(&dir.join(format!("{name}@3x.png")), W * s, total_h * s, &big);
}

fn save(path: &Path, w: usize, h: usize, rgb: &[u8]) {
    let f = std::fs::File::create(path).expect("create png");
    let mut e = png::Encoder::new(std::io::BufWriter::new(f), w as u32, h as u32);
    e.set_color(png::ColorType::Rgb);
    e.set_depth(png::BitDepth::Eight);
    e.write_header().expect("png header").write_image_data(rgb).expect("png data");
    println!("{}", path.display());
}

fn pair(dir: &Path, name: &str, top: impl FnOnce(&mut Canvas), bottom: impl FnOnce(&mut Canvas)) {
    let (mut t, mut b) = (vec![0x8000u16; W * H], vec![0x8000u16; W * H]);
    top(&mut Canvas::new(&mut t, SCREEN_W, SCREEN_H, SCREEN_W));
    bottom(&mut Canvas::new(&mut b, SCREEN_W, SCREEN_H, SCREEN_W));
    write_pair(dir, name, &t, &b);
}

fn main() {
    let dir = PathBuf::from(std::env::args().nth(1).expect("out dir"));
    std::fs::create_dir_all(&dir).unwrap();
    let bar = Bar { hh: 16, mm: 20, wifi: Wifi::Connected, battery: 11, charging: false };
    let msgs = [
        Msg { lucina: false, text: "What is on my list today?", voice_ms: None, wave: &[], state: MsgState::Sent, hidden: false, others: 0 },
        Msg { lucina: true, text: "Three things: the laundry, the parcel at the post office, and the shopping for Teresa.", voice_ms: None, wave: &[], state: MsgState::Sent, hidden: false, others: 0 },
        Msg { lucina: false, text: "remind me to buy milk tomorrow", voice_ms: Some(4200), wave: &[30, 120, 230, 255, 180, 90, 40, 140, 200, 60], state: MsgState::Sent, hidden: false, others: 0 },
        Msg { lucina: true, text: "Done: milk is on the list for tomorrow morning.", voice_ms: None, wave: &[], state: MsgState::Sent, hidden: false, others: 0 },
    ];
    pair(&dir, "chat", |cv| chat_top(cv, &bar, &msgs, false), |cv| chat_bottom(cv, "", Rec::Off, None, None));
    pair(&dir, "chat-recording", |cv| chat_top(cv, &bar, &msgs[..2], false), |cv| chat_bottom(cv, "", Rec::On { ms: 7400, bars: 8 }, None, Some(ChatFocus::Mic)));
    pair(&dir, "chat-typing", |cv| chat_top(cv, &bar, &msgs, true), |cv| chat_typing(cv, "Hello Lucina", false, None));
    pair(&dir, "chat-empty", |cv| chat_top(cv, &bar, &[], false), |cv| chat_bottom(cv, "hi", Rec::Off, Some("Not sent: no Wi-Fi"), Some(ChatFocus::Tabs)));
    let agents = [
        Agent { title: "Fix the DSi build", provider: "claude", state: AgentState::NeedsYou },
        Agent { title: "Juno Wi-Fi speed", provider: "codex", state: AgentState::Running },
        Agent { title: "Port the reader", provider: "vibe", state: AgentState::Done },
        Agent { title: "A failing one", provider: "claude", state: AgentState::Failed },
    ];
    pair(&dir, "agents", |cv| agents_top(cv, &bar, &agents, true), |cv| agents_bottom(cv, &agents, 0, None, Some(ListFocus::Row(0))));
    let lines = [
        Line { kind: LineKind::Asked, text: "Can you fix the build on the DSi?" },
        Line { kind: LineKind::Tool, text: "Bash · completed" },
        Line { kind: LineKind::Said, text: "The build fails at the link step: a missing symbol in the ARM7 core. Which rollout should we use?" },
    ];
    let opts = ["Now", "Behind a flag"];
    pair(&dir, "agent-question", |cv| agent_top(cv, &bar, &agents[0], &lines, Some("Which rollout should we use?"), true), |cv| {
        agent_bottom(cv, Some(&Waiting { question: "Which rollout should we use?", options: &opts }), None, Some(AgentFocus::Answer(0)));
    });
    let qr: Vec<bool> = (0..29 * 29).map(|i| ((i * 7919) % 13) < 6).collect();
    pair(&dir, "pair", |cv| pair_top(cv, &bar), |cv| pair_bottom(cv, &Pair::Ready { modules: &qr, width: 29, code: "482913", left_s: 97 }, false, false));
    println!("wrote {}", dir.display());
}
