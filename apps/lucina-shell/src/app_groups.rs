//! The apps page's groups, and where their tiles go: the ordering of `Home.tiles` and the one
//! placement function the drawing and the input share.

use lucina_app::gfx::Rect;

/// The four groups of the apps page, in the order they are shown.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Group {
    System,
    Create,
    Work,
    Media,
}

impl Group {
    pub const ALL: [Group; 4] = [Group::System, Group::Create, Group::Work, Group::Media];

    pub fn name(self) -> &'static str {
        match self {
            Group::System => "System",
            Group::Create => "Create",
            Group::Work => "Work",
            Group::Media => "Media",
        }
    }
}

/// The group an app belongs to, by its manifest id.
pub fn group_of(id: &str) -> Group {
    match id {
        "terminal" | "settings" | "agents" | "server-health" | "secrets" | "reliability"
        | "vault" | "desk" | "codecor" => Group::System,
        "artcraft" | "camera" | "recorder" => Group::Create,
        id if id.ends_with("craft") => Group::Create,
        "gallery" | "viewer" | "video" | "player" | "library" | "gameboy" => Group::Media,
        _ => Group::Work,
    }
}

/// Where a page's tiles and group headers are.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Placement {
    /// The height of every tile.
    pub height: i32,
    /// Each tile's rectangle, in tile order.
    pub tiles: Vec<Rect>,
    /// One header per non-empty group, in group order.
    pub headers: Vec<Rect>,
    /// The group index of each header.
    pub header_groups: Vec<usize>,
    /// The row each tile is on.
    pub tile_rows: Vec<usize>,
    /// The top of each row, in row order.
    pub row_tops: Vec<i32>,
}

/// Lay the groups out in `area`: a header per non-empty group, then its tiles flowing left to
/// right. The tile height comes from the space left, kept between `min_h` and `max_h`; when even
/// `min_h` does not fit, the tiles keep that height and run on below the area for scrolling.
#[allow(clippy::too_many_arguments)]
pub fn place(
    counts: &[usize; 4],
    area: Rect,
    gap: i32,
    header: i32,
    min_h: i32,
    max_h: i32,
    cols: usize,
) -> Placement {
    let cols = cols.max(1) as i32;
    let rows: Vec<i32> = counts
        .iter()
        .map(|&n| {
            if n == 0 {
                0
            } else {
                (n as i32 + cols - 1) / cols
            }
        })
        .collect();
    let total_rows: i32 = rows.iter().sum();
    if total_rows == 0 || area.w <= 0 || area.h <= 0 {
        return Placement::default();
    }
    let nonempty = counts.iter().filter(|&&n| n > 0).count() as i32;
    let tile_w = ((area.w - (cols - 1) * gap) / cols).max(1);

    // Headers, the gaps between rows, and the gaps between groups take their space first; the
    // rest is shared by the rows of tiles.
    let fixed = nonempty * header + (total_rows - 1).max(0) * gap;
    let height = ((area.h - fixed) / total_rows).clamp(min_h, max_h);

    let mut out = Placement {
        height,
        ..Placement::default()
    };
    let mut y = area.y;
    let mut row = 0;
    let mut group = 0;
    for (g, &count) in counts.iter().enumerate() {
        if count == 0 {
            continue;
        }
        out.headers.push(Rect::new(area.x, y, area.w, header));
        out.header_groups.push(g);
        y += header;
        for r in 0..rows[g] {
            out.row_tops.push(y);
            let in_row = (count as i32 - r * cols).min(cols);
            for c in 0..in_row {
                out.tiles
                    .push(Rect::new(area.x + c * (tile_w + gap), y, tile_w, height));
                out.tile_rows.push(row as usize);
            }
            y += height;
            row += 1;
            if r + 1 < rows[g] {
                y += gap;
            }
        }
        group += 1;
        if group < nonempty {
            y += gap;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn inside(r: &Rect, area: Rect) -> bool {
        r.x >= area.x && r.y >= area.y && r.right() <= area.right() && r.bottom() <= area.bottom()
    }

    #[test]
    fn groups_are_named_and_found() {
        for (id, group) in [
            ("terminal", Group::System),
            ("settings", Group::System),
            ("agents", Group::System),
            ("woodcraft", Group::Create),
            ("artcraft", Group::Create),
            ("camera", Group::Create),
            ("recorder", Group::Create),
            ("gallery", Group::Media),
            ("gameboy", Group::Media),
            ("chat", Group::Work),
            ("calendar", Group::Work),
            ("tasks", Group::Work),
            ("notes", Group::Work),
            ("unknown-app", Group::Work),
        ] {
            assert_eq!(group_of(id), group, "{id}");
        }
        assert_eq!(
            Group::ALL.map(Group::name),
            ["System", "Create", "Work", "Media"]
        );
    }

    #[test]
    fn everything_fits_when_it_can() {
        let area = Rect::new(30, 20, 1000, 600);
        let counts = [2, 3, 4, 1];
        let p = place(&counts, area, 10, 20, 40, 120, 10);
        assert!(p.height >= 40 && p.height <= 120);
        assert_eq!(p.tiles.len(), counts.iter().sum::<usize>());
        assert_eq!(p.headers.len(), 4);
        for r in p.tiles.iter().chain(&p.headers) {
            assert!(inside(r, area), "{r:?} outside {area:?}");
        }
        for (i, a) in p.tiles.iter().chain(&p.headers).enumerate() {
            for b in p.tiles.iter().chain(&p.headers).skip(i + 1) {
                assert!(a.intersect(b).is_empty(), "{a:?} overlaps {b:?}");
            }
        }
    }

    #[test]
    fn headers_come_before_their_tiles_and_empty_groups_have_none() {
        let area = Rect::new(0, 0, 900, 500);
        let counts = [0, 3, 0, 2];
        let p = place(&counts, area, 8, 18, 40, 100, 10);
        assert_eq!(p.headers.len(), 2);
        assert_eq!(p.header_groups, vec![1, 3]);
        let first_create = &p.tiles[0];
        assert!(p.headers[0].bottom() <= first_create.y);
        let first_media = &p.tiles[3];
        assert!(p.headers[1].bottom() <= first_media.y);
        let empty = place(&[0, 0, 0, 0], area, 8, 18, 40, 100, 10);
        assert!(empty.headers.is_empty());
        assert!(empty.tiles.is_empty());
        assert_eq!(empty.height, 0);
    }

    #[test]
    fn overflow_keeps_the_smallest_height_for_scrolling() {
        let area = Rect::new(10, 10, 500, 300);
        let counts = [50, 50, 50, 50];
        let p = place(&counts, area, 8, 20, 40, 120, 5);
        assert_eq!(p.height, 40);
        assert_eq!(p.tiles.len(), 200);
        for (i, a) in p.tiles.iter().enumerate() {
            for b in p.tiles.iter().skip(i + 1) {
                assert!(a.intersect(b).is_empty(), "{a:?} overlaps {b:?}");
            }
        }
    }
}
