//! The timeline's chart: one column per day, the stability line through the days' indices on a 1 to 10 scale, and under it the
//! four category rows of Reliability Monitor with the count of each day.

use lucina_app::{Face, Rect, Ui};
use lucina_evlog::event::Category;
use lucina_evlog::timeline::Day;

use crate::look::{BLUE, category_colour, centred, disc, index_colour, line};
use crate::model::{index_text, parts_of_date};

pub const ROWS: [Category; 4] = [
    Category::JunoFailure,
    Category::AppFailure,
    Category::Warning,
    Category::Info,
];

/// Reference pixels. The chart is as high as `height` says, so the page can reserve it.
const PLOT_H: f32 = 96.0;
const DATE_H: f32 = 36.0;
const ROW_H: f32 = 23.0;
const PAD: f32 = 8.0;
pub const GUTTER: f32 = 150.0;

pub fn height(ui: &Ui) -> i32 {
    let t = ui.theme();
    t.px(PAD * 2.0 + PLOT_H + DATE_H + ROW_H * 4.0)
}

fn count(d: &Day, c: Category) -> u32 {
    match c {
        Category::JunoFailure => d.juno_failures,
        Category::AppFailure => d.app_failures,
        Category::Warning => d.warnings,
        Category::Info => d.info,
    }
}

/// Draws the chart in `r` for `days` (the last is today) with column `sel` highlighted. Returns the column a click chose.
pub fn draw(ui: &mut Ui, r: Rect, days: &[Day], sel: usize) -> Option<usize> {
    let t = ui.theme();
    let n = days.len().max(1);
    let gutter = t.px(GUTTER);
    let cw = ((r.w - gutter - t.px(PAD)) / n as i32).max(1);
    let x0 = r.x + gutter;
    let plot = Rect::new(x0, r.y + t.px(PAD), cw * n as i32, t.px(PLOT_H));
    let dates_y = plot.bottom();
    let rows_y = dates_y + t.px(DATE_H);

    let mut clicked = None;
    let mut hovered = None;
    for i in 0..n {
        let col = Rect::new(x0 + cw * i as i32, r.y, cw, r.h);
        let hit = ui.hit(("col", i), col);
        if hit.clicked {
            clicked = Some(i);
        }
        if hit.hovered {
            hovered = Some(i);
        }
    }

    ui.fill_round(r, t.px(8.0), t.panel);
    for (i, band) in [(sel, t.panel_hi), (hovered.unwrap_or(usize::MAX), t.bar)] {
        if i < n && (i == sel || hovered != Some(sel)) {
            ui.fill_round(
                Rect::new(x0 + cw * i as i32 + 1, r.y + 3, cw - 2, r.h - 6),
                t.px(6.0),
                band,
            );
        }
    }

    // The scale: 10 at the top, 1 at the bottom, a dashed guide at 5.
    let inner = plot.h - t.px(2.0 * PAD);
    let y_of =
        |v: f64| plot.y + t.px(PAD) + ((10.0 - v.clamp(1.0, 10.0)) / 9.0 * f64::from(inner)) as i32;
    for (v, label) in [(10.0, "10"), (5.0, "5"), (1.0, "1")] {
        let y = y_of(v);
        let mut x = plot.x;
        while x < plot.right() {
            ui.fill(Rect::new(x, y, t.px(4.0), 1), t.panel_hi);
            x += t.px(8.0);
        }
        let w = ui.measure(Face::Sans, t.pt(11.0), label);
        ui.text_in(
            Rect::new(x0 - w - t.px(10.0), y - t.px(8.0), w, t.px(16.0)),
            Face::Sans,
            t.pt(11.0),
            t.dim,
            label,
        );
    }
    ui.text_in(
        Rect::new(r.x + t.px(12.0), plot.y, gutter - t.px(40.0), t.px(18.0)),
        Face::Bold,
        t.pt(12.0),
        t.item,
        "Stability",
    );
    ui.text_in(
        Rect::new(
            r.x + t.px(12.0),
            plot.y + t.px(18.0),
            gutter - t.px(40.0),
            t.px(16.0),
        ),
        Face::Sans,
        t.pt(11.0),
        t.dim,
        "1 worst, 10 best",
    );

    // The line, with a gap where a day has no index.
    let point = |i: usize| {
        days[i]
            .index
            .map(|v| (x0 + cw * i as i32 + cw / 2, y_of(v)))
    };
    for i in 1..days.len() {
        if let (Some((ax, ay)), Some((bx, by))) = (point(i - 1), point(i)) {
            line(ui, ax, ay, bx, by, t.px(2.5).max(2), BLUE);
        }
    }
    for (i, d) in days.iter().enumerate() {
        let Some((x, y)) = point(i) else { continue };
        let c = index_colour(d.index, &t);
        let side = t.px(if i == sel { 13.0 } else { 9.0 });
        if i == sel {
            disc(ui, x, y, side + t.px(5.0), t.text);
        }
        disc(ui, x, y, side, c);
        // The value above the point, or under it near the top.
        let px = t.pt(if i == sel { 13.0 } else { 11.0 });
        let text = index_text(d.index);
        let above = y - t.px(24.0) >= plot.y;
        let ty = if above { y - t.px(26.0) } else { y + t.px(9.0) };
        centred(
            ui,
            x,
            ty,
            t.px(18.0),
            if i == sel { Face::Bold } else { Face::Sans },
            px,
            if i == sel { t.text } else { t.item },
            &text,
        );
    }

    // Dates: weekday, then the day number (with the month on the first column and on the 1st).
    for (i, d) in days.iter().enumerate() {
        let (wd, day, mon) = parts_of_date(&d.date);
        let today = i + 1 == days.len();
        let cx = x0 + cw * i as i32;
        let c = if i == sel {
            t.accent
        } else if today {
            t.text
        } else {
            t.dim
        };
        let face = if i == sel || today {
            Face::Bold
        } else {
            Face::Sans
        };
        centred(
            ui,
            cx + cw / 2,
            dates_y + t.px(3.0),
            t.px(16.0),
            face,
            t.pt(11.0),
            c,
            wd,
        );
        let num = if i == 0 || day == 1 {
            format!("{day} {mon}")
        } else {
            day.to_string()
        };
        centred(
            ui,
            cx + cw / 2,
            dates_y + t.px(18.0),
            t.px(18.0),
            face,
            t.pt(12.0),
            c,
            &num,
        );
    }

    // The category rows.
    for (k, cat) in ROWS.iter().enumerate() {
        let y = rows_y + t.px(ROW_H) * k as i32;
        ui.fill(
            Rect::new(r.x + t.px(8.0), y, r.w - t.px(16.0), 1),
            t.panel_hi,
        );
        let c = category_colour(*cat, &t);
        disc(ui, r.x + t.px(20.0), y + t.px(ROW_H) / 2, t.px(11.0), c);
        ui.text_in(
            Rect::new(r.x + t.px(34.0), y, gutter - t.px(38.0), t.px(ROW_H)),
            Face::Sans,
            t.pt(12.0),
            t.item,
            cat.name(),
        );
        for (i, d) in days.iter().enumerate() {
            let cx = x0 + cw * i as i32 + cw / 2;
            let cy = y + t.px(ROW_H) / 2;
            let n = count(d, *cat);
            if n == 0 {
                disc(ui, cx, cy, t.px(4.0), t.panel_hi);
                continue;
            }
            let side = t.px(20.0);
            disc(ui, cx, cy, side, c);
            let s = n.to_string();
            centred(
                ui,
                cx,
                cy - side / 2,
                side,
                Face::Bold,
                t.pt(12.0),
                t.bg,
                &s,
            );
        }
    }
    clicked
}
