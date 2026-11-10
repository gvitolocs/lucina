//! Juno, the laptop: 1920 x 1080 at 1.5 px/dp (1280 x 720 dp). The three phone screens merge into one dashboard of
//! three columns (Build, the agenda titled "Juno", Notification), and the sheet becomes a dock. Pointer hover and the
//! keyboard's focus ring are shown on the actions; shortcuts live in help, not on the screen (rule 3).

use crate::anim::Secs;
use crate::canvas::{Canvas, Color, R};
use crate::imagery;
use crate::screens::{Backdrop, content as c};
use crate::text::{Font, Type};
use crate::tokens::{color, radius, space, ty};
use crate::widgets::chrome;
use crate::Paint;

use super::blocks::{self, Parts};
use super::{Layout, pane};

pub struct Juno {
    bg: Option<Backdrop>,
    pub parts: Parts,
    panes: Vec<(R, R)>,
}

impl Juno {
    pub fn new(now: Secs) -> Juno {
        let mut parts = Parts::new(now, 4, 2);
        parts.actions[1].set_hover(now, true);
        Juno { bg: None, parts, panes: Vec::new() }
    }
}

impl Layout for Juno {
    fn name(&self) -> &'static str {
        "juno"
    }

    fn size(&self) -> (u32, u32, f32) {
        (1920, 1080, 1.5)
    }

    fn panes(&self) -> Vec<(R, R)> {
        self.panes.clone()
    }

    fn draw(&mut self, cv: &mut Canvas, ty: &mut Type, now: Secs) {
        Backdrop::ensure(&mut self.bg, cv, imagery::night_sky);
        let bg = self.bg.as_ref().expect("backdrop");
        cv.px.copy_from_slice(&bg.image.px);
        let full = cv.bounds();
        let parts = &mut self.parts;
        let mut p = Paint { cv, ty, now, backdrop: Some(&bg.blurred) };
        p.cv.scrim(R::new(0.0, 0.0, full.w, 60.0), Color::hexa(0x000000, 120), Color::hexa(0x000000, 0));
        chrome::status_bar(&mut p, R::new(0.0, 0.0, full.w, 40.0), color::INK);

        let area = R::new(20.0, 48.0, full.w - 40.0, 576.0);
        let dock = R::new(20.0, 638.0, full.w - 40.0, 64.0);
        let left = pane(area, 12, 1, space::L, 0, 0, 4, 1);
        let mid = pane(area, 12, 1, space::L, 4, 0, 4, 1);
        let right = pane(area, 12, 1, space::L, 8, 0, 4, 1);
        self.panes = vec![(left, area), (mid, area), (right, area), (dock, full)];

        // Build: header, segments, actions (one hovered, one with the keyboard's focus), two stat cards.
        blocks::build_header(&mut p, R::new(left.x, left.y, left.w, 166.0), parts, 64.0);
        parts.seg.draw(&mut p, R::new(left.x, left.y + 178.0, left.w, space::TAP));
        let row = R::new(left.x, left.y + 240.0, left.w, 64.0);
        blocks::actions(&mut p, row, parts);
        let (cx, cw) = crate::widgets::cell_x(row, 4, space::M, 2, 1);
        blocks::focus_ring(p.cv, R::new(cx + cw / 2.0 - space::TAP / 2.0, row.y, space::TAP, space::TAP), space::TAP / 2.0);
        let tiles = R::new(left.x, left.bottom() - 150.0, left.w, 150.0);
        for i in 0..2u32 {
            let t = pane(tiles, 2, 1, space::M, i, 0, 1, 1);
            let icon = if i == 0 { crate::icons::Icon::Person } else { crate::icons::Icon::Heart };
            chrome::stat_tile(&mut p, t, icon, c::TILES[i as usize].0, c::TILES[i as usize].1);
        }

        // The agenda, titled Juno.
        blocks::agenda_header(&mut p, R::new(mid.x, mid.y, mid.w, 100.0), parts, c::LAPTOP);
        chrome::profile_chip(&mut p, R::new(mid.x, mid.y + 112.0, mid.w * 0.62, 52.0), c::PERSON.0, c::PERSON.1, c::PERSON.2);
        chrome::reminder_card(&mut p, R::new(mid.x, mid.y + 176.0, mid.w, 80.0), c::REMINDER, color::WARNING);
        parts.days.draw(&mut p, R::new(mid.x, mid.y + 270.0, mid.w, 36.0));
        parts.grid.draw(&mut p, R::new(mid.x, mid.y + 320.0, mid.w, mid.h - 320.0));

        // Notification: two stat cards, then the title, stack, pager and actions.
        p.glass(right, radius::XL, Color::hexa(0x1c1c1e, 200));
        let inner = right.inset(space::L);
        blocks::stats(&mut p, R::new(inner.x, inner.y, inner.w, 200.0));
        blocks::notifications(&mut p, R::new(inner.x, inner.y + 230.0, inner.w, inner.h - 230.0), parts, 48.0);

        // The dock: upload, ring and timer, controls, volume, toggles.
        p.glass(dock, radius::PILL, color::SMOKE);
        chrome::spinner(&mut p, dock.x + 30.0, dock.cy(), 6.0, color::INK);
        p.text(Font::Regular, ty::CAPTION, dock.x + 46.0, dock.cy() + 4.0, color::INK_2, c::UPLOADING);
        parts.timer.draw_ring(&mut p, dock.x + 200.0, dock.cy(), 21.0);
        parts.timer.draw_digits(&mut p, dock.x + 230.0, dock.cy() + 12.0, 32.0, color::INK);
        parts.media.draw(&mut p, R::new(dock.cx() - 190.0, dock.y + 8.0, 380.0, 48.0));
        parts.volume.draw(&mut p, R::new(dock.right() - 330.0, dock.cy() - 12.0, 200.0, 24.0));
        parts.round[0].draw(&mut p, dock.right() - 80.0, dock.cy(), 18.0);
        parts.round[1].draw(&mut p, dock.right() - 38.0, dock.cy(), 18.0);
    }
}
