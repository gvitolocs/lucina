//! The browser chrome's icons and controls on one sheet (docs/design/browser-glass.md in LucinaNet), each drawn on the night sky and
//! checked to have drawn something inside its own rectangle: `cargo run --release -p lucina-glass --example browser_widgets -- <out dir>`
//! writes `browser-widgets.png` there and exits 1 if any control left its rectangle blank.

use lucina_glass::Paint;
use lucina_glass::canvas::{Canvas, Color, R};
use lucina_glass::icons::{self, Icon};
use lucina_glass::imagery;
use lucina_glass::text::{Font, Type};
use lucina_glass::tokens::{color, glass, ty};
use lucina_glass::widgets::adaptive::{adapt, adaptive_bar};
use lucina_glass::widgets::bar::bar;
use lucina_glass::widgets::dialog_card::HEIGHT as DIALOG_H;
use lucina_glass::widgets::ink::{Palette, text_left};
use lucina_glass::widgets::list_row::ListRow;
use lucina_glass::widgets::progress_bar::ProgressBar;
use lucina_glass::widgets::segmented_plain::SegmentedPlain;
use lucina_glass::widgets::stepper::Stepper;
use lucina_glass::widgets::switch::Switch;
use lucina_glass::widgets::text_field::{FieldMetrics, TextField};
use lucina_glass::widgets::{BarButton, BottomSheet, DialogCard, Popover, TabCard, TabChip};

const K: f32 = 1.5;
const W: f32 = 760.0;
const H: f32 = 780.0;

struct Sheet {
    cv: Canvas,
    blurred: Canvas,
    ty: Type,
    failed: Vec<String>,
}

impl Sheet {
    /// Run `f` and check that it changed pixels inside `r` (dp), a little outside for shadows and rings.
    fn check(&mut self, name: &str, r: R, f: impl FnOnce(&mut Paint)) {
        let before = self.cv.px.clone();
        {
            let mut p = Paint {
                cv: &mut self.cv,
                ty: &mut self.ty,
                now: 1.0,
                backdrop: Some(&self.blurred),
            };
            f(&mut p);
        }
        let (w, k) = (self.cv.w as usize, K);
        let (x0, y0) = (
            ((r.x - 6.0) * k).max(0.0) as usize,
            ((r.y - 6.0) * k).max(0.0) as usize,
        );
        let (x1, y1) = (
            ((r.right() + 6.0) * k) as usize,
            ((r.bottom() + 6.0) * k) as usize,
        );
        let changed = (y0..y1.min(self.cv.h as usize))
            .flat_map(|y| (x0..x1.min(w)).map(move |x| (x, y)))
            .filter(|&(x, y)| before[y * w + x] != self.cv.px[y * w + x])
            .count();
        println!("{name:<16} {changed:>7} px changed");
        if changed == 0 {
            self.failed.push(name.to_string());
        }
    }
}

fn main() {
    let dir = std::env::args()
        .nth(1)
        .expect("usage: browser_widgets <out dir>");
    std::fs::create_dir_all(&dir).expect("out dir");
    let bg = imagery::night_sky((W * K) as u32, (H * K) as u32, K, 7);
    let blurred = bg.blurred((glass::BLUR * K / 3.0).round().max(1.0) as u32);
    let mut s = Sheet {
        cv: bg.clone(),
        blurred,
        ty: Type::new(),
        failed: Vec::new(),
    };

    // Icons: the whole browser set at 28 dp, and the same at the smallest size, 16 dp.
    let set = [
        Icon::Back,
        Icon::Forward,
        Icon::Reload,
        Icon::Close,
        Icon::Plus,
        Icon::Minus,
        Icon::Lock,
        Icon::Search,
        Icon::Star,
        Icon::StarFill,
        Icon::Dots,
        Icon::Download,
        Icon::Gear,
        Icon::Private,
        Icon::Tabs,
        Icon::Check,
        Icon::Camera,
        Icon::Pin,
        Icon::Popup,
        Icon::Folder,
        Icon::Globe,
        Icon::ChevronRight,
    ];
    for (i, ic) in set.into_iter().enumerate() {
        for (row, size) in [(0.0, 28.0), (1.0, 16.0)] {
            let (cx, cy) = (30.0 + i as f32 * 33.0, 24.0 + row * 34.0);
            s.check(
                &format!("icon {ic:?} {size}"),
                R::new(cx - 14.0, cy - 14.0, 28.0, 28.0),
                |p| icons::draw(p.cv, ic, cx, cy, size, color::INK),
            );
        }
    }

    // Bars: a dark one over the sky, a light one over a light patch; the toolbar's buttons and a tab strip.
    let strip = R::new(12.0, 90.0, W - 24.0, 44.0);
    s.check("bar + tab chips", strip, |p| {
        bar(p, strip, 22.0);
        let pal = Palette::STANDARD;
        let tw = TabChip::width(strip.w - 60.0, 3, 4.0, 210.0);
        for (i, (t, h)) in [
            ("Example Domain", "example.org"),
            ("Delivery", "shop.example.net"),
            ("Release notes", "notes.example.org"),
        ]
        .into_iter()
        .enumerate()
        {
            let mut c = TabChip::new(t, h);
            c.active = i == 0;
            c.loading = i == 2;
            c.focused = i == 1;
            c.draw(
                p,
                R::new(
                    strip.x + 4.0 + i as f32 * (tw + 4.0),
                    strip.y + 4.0,
                    tw,
                    36.0,
                ),
                ty::BODY,
                &pal,
            );
        }
    });
    let toolbar = R::new(12.0, 144.0, W - 24.0, 48.0);
    s.check("bar buttons", toolbar, |p| {
        let a = adaptive_bar(p, toolbar, 24.0);
        for (i, ic) in [
            Icon::Back,
            Icon::Forward,
            Icon::Reload,
            Icon::Download,
            Icon::Dots,
        ]
        .into_iter()
        .enumerate()
        {
            let mut b = BarButton::new(ic);
            b.active = ic == Icon::Dots;
            b.focused = ic == Icon::Reload;
            b.draw(
                p,
                toolbar.x + 30.0 + i as f32 * 40.0,
                toolbar.cy(),
                36.0,
                a.ink,
            );
        }
        let mut f = TextField::new("lucina design");
        f.focus(p.now);
        f.draw(
            p,
            R::new(toolbar.x + 250.0, toolbar.y + 6.0, 400.0, 36.0),
            &FieldMetrics::bar(36.0, ty::LEAD),
            &a.palette(),
        );
    });
    // The light glass over a light patch.
    let patch = R::new(12.0, 200.0, 360.0, 60.0);
    s.cv.fill_rrect(patch, 0.0, Color::hex(0xf2f2f7));
    s.blurred =
        s.cv.blurred((glass::BLUR * K / 3.0).round().max(1.0) as u32);
    s.check("adaptive light bar", patch, |p| {
        let a = adaptive_bar(p, patch.inset(8.0), 20.0);
        assert_eq!(a.ink, adapt(1.0).ink, "a light patch gives light glass");
        text_left(
            p,
            Font::Regular,
            ty::LEAD,
            patch.x + 24.0,
            patch.cy(),
            a.ink,
            "Dark ink on light glass",
        );
    });

    // Controls on the thick material.
    let card = R::new(12.0, 272.0, 360.0, 276.0);
    s.check("list rows + stepper", card, |p| {
        p.glass(card, 18.0, color::MATERIAL);
        let pal = Palette::STANDARD;
        let mut y = card.y + 8.0;
        for (i, row) in [
            ListRow::new(Some(Icon::Private), "New private window"),
            ListRow::new(Some(Icon::Gear), "Settings")
                .value("On")
                .chevron(),
            ListRow::new(None, "Denmark").selected(true),
        ]
        .into_iter()
        .enumerate()
        {
            let mut row = row;
            row.focused = i == 0;
            row.draw(
                p,
                R::new(card.x + 6.0, y, card.w - 12.0, 40.0),
                ty::LEAD,
                &pal,
            );
            y += 44.0;
        }
        let mut st = Stepper::new("Zoom", 110, 50, 300, 10, "%");
        st.draw(
            p,
            R::new(card.x + 6.0, y, card.w - 12.0, 40.0),
            ty::LEAD,
            36.0,
        );
        y += 44.0;
        for (i, on) in [false, true].into_iter().enumerate() {
            let mut sw = Switch::new(on);
            sw.focused = i == 1;
            text_left(
                p,
                Font::Regular,
                ty::LEAD,
                card.x + 18.0,
                y + 20.0 + i as f32 * 44.0,
                color::INK,
                if on { "Location" } else { "Camera" },
            );
            sw.draw(p, card.right() - 14.0, y + 20.0 + i as f32 * 44.0);
        }
    });

    // Segmented, progress, popover, sheet, dialog, tab card on the right.
    let seg = R::new(392.0, 212.0, 240.0, 44.0);
    s.check("segmented", seg, |p| {
        SegmentedPlain::new(&["Tabs", "Private"], 0).draw(p, seg)
    });
    let bars = R::new(392.0, 268.0, 240.0, 40.0);
    s.check("progress bars", bars, |p| {
        ProgressBar::new(42, 120).draw(p, R::new(bars.x, bars.y + 8.0, bars.w, 4.0));
        ProgressBar::new(120, 120).draw(p, R::new(bars.x, bars.y + 28.0, bars.w, 4.0));
    });
    let pop = Popover::place(748.0, 330.0, 280.0, 90.0, 12.0, W);
    s.check("popover", pop, |p| {
        let mut po = Popover::new();
        po.open(0.0);
        let c = po.draw(p, pop).expect("open");
        ListRow::new(Some(Icon::Search), "Find").draw(
            p,
            R::new(c.x, c.y, c.w, 40.0),
            ty::LEAD,
            &Palette::STANDARD,
        );
    });
    let mut thumb = Canvas::new(200, 160, 1.0);
    thumb.clear(Color::hex(0xf2f2f7));
    let tcard = R::new(392.0, 470.0, 150.0, TabCard::height(70.0));
    s.check("tab card", tcard, |p| {
        let mut c = TabCard::new("Release notes", "notes.example.org");
        c.selected = true;
        c.draw(p, tcard, &thumb, false);
    });
    let dlg = DialogCard::place(R::new(552.0, 460.0, 200.0, DIALOG_H + 20.0), 196.0, 0.0);
    s.check("dialog", dlg, |p| {
        DialogCard::new("shop.example.net says", "Delete?", "Cancel", "OK").draw(p, dlg)
    });
    let sheet = BottomSheet::place(W, H, 60.0);
    s.check("bottom sheet", sheet, |p| {
        let mut sh = BottomSheet::new();
        sh.open(0.0);
        let c = sh.draw(p, sheet, H).expect("open");
        ListRow::new(Some(Icon::Folder), "Downloads")
            .chevron()
            .draw(p, R::new(c.x, c.y, c.w, 40.0), ty::LEAD, &Palette::STANDARD);
    });

    std::fs::write(format!("{dir}/browser-widgets.png"), s.cv.png()).expect("write png");
    println!("wrote {dir}/browser-widgets.png");
    if !s.failed.is_empty() {
        eprintln!("drew nothing: {:?}", s.failed);
        std::process::exit(1);
    }
}
