//! Diana as a tablet: 2560 x 1600 at 2 px/dp, touch first.
//!
//! - **Landscape** (1280 x 800 dp): Build on the left with the player card; the agenda, titled "Diana", on the right with a
//!   6-column calendar. Notification is a slide-over that peeks from the right edge, showing its count.
//! - **Portrait** (800 x 1280 dp): the agenda across the top. Under it, side by side: Build's header and player, and
//!   Notification with its stat cards.
//!
//! Touch targets are at least 44 dp; no hover.

use crate::anim::Secs;
use crate::canvas::{Canvas, Color, R};
use crate::imagery;
use crate::screens::{Backdrop, content as c};
use crate::text::{Font, Type};
use crate::tokens::{color, radius, space};
use crate::widgets::chrome;
use crate::Paint;

use super::blocks::{self, Parts};
use super::{Layout, pane};

pub struct DianaTablet {
    pub portrait: bool,
    bg: Option<Backdrop>,
    pub parts: Parts,
    panes: Vec<(R, R)>,
}

impl DianaTablet {
    pub fn new(now: Secs, portrait: bool) -> DianaTablet {
        DianaTablet { portrait, bg: None, parts: Parts::new(now, 6, 3), panes: Vec::new() }
    }

    /// The agenda block: header, profile and reminder side by side, days, the grid to the bottom of `r`.
    fn agenda(p: &mut Paint, r: R, parts: &mut Parts) {
        blocks::agenda_header(p, R::new(r.x, r.y, r.w, 100.0), parts, c::PHONE);
        let row = R::new(r.x, r.y + 112.0, r.w, 80.0);
        let chip = pane(row, 12, 1, space::M, 0, 0, 5, 1);
        let rem = pane(row, 12, 1, space::M, 5, 0, 7, 1);
        chrome::profile_chip(p, R::new(chip.x, chip.y + 14.0, chip.w, 52.0), c::PERSON.0, c::PERSON.1, c::PERSON.2);
        chrome::reminder_card(p, rem, c::REMINDER, color::WARNING);
        parts.days.draw(p, R::new(r.x, r.y + 206.0, r.w, 38.0));
        parts.grid.draw(p, R::new(r.x, r.y + 258.0, r.w, r.h - 258.0));
    }
}

impl Layout for DianaTablet {
    fn name(&self) -> &'static str {
        if self.portrait { "diana-tablet-portrait" } else { "diana-tablet-landscape" }
    }

    fn size(&self) -> (u32, u32, f32) {
        if self.portrait { (1600, 2560, 2.0) } else { (2560, 1600, 2.0) }
    }

    fn panes(&self) -> Vec<(R, R)> {
        self.panes.clone()
    }

    fn draw(&mut self, cv: &mut Canvas, ty: &mut Type, now: Secs) {
        Backdrop::ensure(&mut self.bg, cv, imagery::stairs_in_space);
        let bg = self.bg.as_ref().expect("backdrop");
        cv.px.copy_from_slice(&bg.image.px);
        let full = cv.bounds();
        let parts = &mut self.parts;
        let mut p = Paint { cv, ty, now, backdrop: Some(&bg.blurred) };
        chrome::status_bar(&mut p, R::new(0.0, 0.0, full.w, 44.0), color::INK);

        if !self.portrait {
            let area = R::new(24.0, 52.0, full.w - 24.0 - 64.0, full.h - 52.0 - 28.0);
            let left = pane(area, 12, 1, 24.0, 0, 0, 5, 1);
            let right = pane(area, 12, 1, 24.0, 5, 0, 7, 1);
            blocks::build_header(&mut p, R::new(left.x, left.y, left.w, 180.0), parts, 76.0);
            parts.seg.draw(&mut p, R::new(left.x, left.y + 194.0, left.w, space::TAP + 2.0));
            blocks::actions(&mut p, R::new(left.x, left.y + 258.0, left.w, 64.0), parts);
            let player = R::new(left.x, left.bottom() - 220.0, left.w, 220.0);
            blocks::player(&mut p, player, parts);
            // Between the actions and the player: the two stat cards.
            let tiles = R::new(left.x, left.y + 338.0, left.w, player.y - left.y - 338.0 - space::M);
            if tiles.h >= 110.0 {
                for i in 0..2u32 {
                    let t = pane(tiles, 2, 1, space::M, i, 0, 1, 1);
                    let icon = if i == 0 { crate::icons::Icon::Person } else { crate::icons::Icon::Heart };
                    chrome::stat_tile(&mut p, t, icon, c::TILES[i as usize].0, c::TILES[i as usize].1);
                }
            }
            DianaTablet::agenda(&mut p, right, parts);
            // The slide-over: Notification peeks from the right edge with its count.
            let peek = R::new(full.w - 40.0, 140.0, 300.0, 460.0);
            p.shadow(peek, radius::XL, 16.0);
            p.glass(peek, radius::XL, Color::hexa(0x1c1c1e, 220));
            p.cv.fill_rrect(R::new(peek.x + 7.0, peek.cy() - 28.0, 4.0, 56.0), 2.0, color::INK_3);
            p.cv.fill_circle(peek.x + 24.0, peek.y + 26.0, 9.0, color::LIVE);
            p.text_centered(Font::Semibold, (10.0, 0.0), peek.x + 24.0, peek.y + 30.0, color::INK, &c::PENDING.to_string());
            self.panes = vec![(left, area), (right, area), (player, left)];
        } else {
            let area = R::new(20.0, 56.0, full.w - 40.0, full.h - 56.0 - 28.0);
            let top = pane(area, 2, 12, 20.0, 0, 0, 2, 6);
            let bl = pane(area, 2, 12, 20.0, 0, 6, 1, 6);
            let br = pane(area, 2, 12, 20.0, 1, 6, 1, 6);
            DianaTablet::agenda(&mut p, top, parts);
            blocks::build_header(&mut p, R::new(bl.x, bl.y, bl.w, 166.0), parts, 60.0);
            blocks::actions(&mut p, R::new(bl.x, bl.y + 178.0, bl.w, 64.0), parts);
            blocks::player(&mut p, R::new(bl.x, bl.bottom() - 220.0, bl.w, 220.0), parts);
            p.glass(br, radius::XL, Color::hexa(0x1c1c1e, 214));
            let inner = br.inset(space::L);
            let end = blocks::notifications(&mut p, inner, parts, 48.0);
            let rest = R::new(inner.x, end + space::L, inner.w, inner.bottom() - end - space::L);
            if rest.h > 120.0 {
                blocks::stats(&mut p, rest);
            }
            self.panes = vec![(top, area), (bl, area), (br, area)];
        }
    }
}
