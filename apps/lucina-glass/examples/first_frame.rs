//! Times the dashboard's first frame as the shell has it at start (only what needs no network), cold and warm.
use lucina_glass::home::{self, Action, Home, HomeData, StatusBar, Target};
use lucina_glass::icons::Icon;
use lucina_glass::{Canvas, Type};
use std::time::Instant;
fn main() {
    let (w, h) = (1920u32, 1080u32);
    let bg = Canvas::new(w, h, home::K);
    let blurred = Canvas::new(w / 2, h / 2, home::K / 2.0);
    let act = |icon, l: &str, id: &str| Action { icon, label: l.into(), target: Target::App(id.into()) };
    let data = HomeData {
        status: Some(StatusBar { clock: "22:47".into(), online: true, battery: Some(100), charging: false, camera: false }),
        segments: vec![
            ("Work".into(), vec![Action { icon: Icon::Doc, label: "Open".into(), target: Target::Apps }, act(Icon::Clock, "Plan", "calendar"), act(Icon::Pen, "Write", "notes"), act(Icon::Play, "Play", "player")]),
            ("Lucina".into(), vec![act(Icon::Send, "Ask", "chat")]),
        ],
        volume: Some((1.0, false)),
        ..HomeData::default()
    };
    for round in 0..3 {
        let mut ty = Type::new();
        let mut cv = Canvas::new(w, h, home::K);
        let mut hm = Home::new(0.0);
        hm.set_data(0.0, data.clone());
        let t = Instant::now();
        let _ = hm.damage(0.0);
        hm.draw(&mut cv, &mut ty, 0.0, &bg, &blurred);
        let cold = t.elapsed();
        hm.invalidate();
        let _ = hm.damage(0.1);
        let t = Instant::now();
        hm.draw(&mut cv, &mut ty, 0.1, &bg, &blurred);
        println!("round {round}: cold {cold:?}, warm {:?}", t.elapsed());
    }
}
