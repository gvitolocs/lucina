//! The ordered row of spaces (`docs/design/juno-window-manager.md`, section 7): Home first, then one space per app, with the
//! split views where a few apps share one space. Giving each app a space of its own and adding it to the right of the one you
//! are on follows niri (a new column opens next to the focused one) and GNOME Shell (a new window shows where you are looking);
//! closing a space and falling back to the neighbour on its left is what niri does when a column closes; drag-reordering the
//! spaces is GNOME Shell's overview, and a split view of side-by-side apps in one space is GNOME Shell's tiled pair living in
//! one workspace, like niri's columns inside a workspace. Integer only, fixed size, no allocation.

use crate::switcher::AppId;

/// How many spaces the row holds at most, Home included.
pub const MAX_SPACES: usize = 16;

/// How many apps one split view holds side by side.
pub const MAX_PER_SPACE: usize = 4;

/// The apps that share one space side by side, in the order they joined it. A split view always holds at least two apps: with
/// one app left it is an ordinary one-app space again.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Tiles {
    ids: [AppId; MAX_PER_SPACE],
    count: usize,
}

impl Tiles {
    /// A split view with no app in it, the starting point before any app joins.
    pub const fn empty() -> Tiles {
        Tiles {
            ids: [AppId(0); MAX_PER_SPACE],
            count: 0,
        }
    }

    /// How many apps are side by side.
    pub fn len(&self) -> usize {
        self.count
    }

    /// Whether the split view holds no app; a live space never is one.
    pub fn is_empty(&self) -> bool {
        self.count == 0
    }

    /// The app at position `i`, counted from the left, None past the last one.
    pub fn get(&self, i: usize) -> Option<AppId> {
        if i < self.count {
            Some(self.ids[i])
        } else {
            None
        }
    }

    /// The position of `id` in the split view, None when it is not shown here.
    pub fn index_of(&self, id: AppId) -> Option<usize> {
        let mut i = 0;
        while i < self.count {
            if self.ids[i] == id {
                return Some(i);
            }
            i += 1;
        }
        None
    }

    /// Whether `id` is shown side by side here.
    pub fn contains(&self, id: AppId) -> bool {
        self.index_of(id).is_some()
    }

    /// `id` joins at the right end. False, with nothing moved, when the split view is already full.
    fn push(&mut self, id: AppId) -> bool {
        if self.count == MAX_PER_SPACE {
            return false;
        }
        self.ids[self.count] = id;
        self.count += 1;
        true
    }

    /// The app at position `at` leaves the split view, the others close the gap and keep their order. Needs `at < count`.
    fn remove_at(&mut self, at: usize) {
        let mut i = at;
        while i + 1 < self.count {
            self.ids[i] = self.ids[i + 1];
            i += 1;
        }
        self.count -= 1;
        self.ids[self.count] = AppId(0);
    }
}

/// One place in the row: Home, one app on its own, or a split view of a few apps.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Space {
    /// The space at index 0: no app lives in it, it opens the row and never closes or moves.
    Home,
    /// One app filling the space.
    App(AppId),
    /// Two to four apps sharing the space side by side.
    Tiles(Tiles),
}

impl Space {
    /// Whether this is Home, the space that cannot move, cannot be filled and cannot close.
    pub fn is_home(&self) -> bool {
        matches!(self, Space::Home)
    }

    /// Whether `id` is shown in this space.
    pub fn contains(&self, id: AppId) -> bool {
        match self {
            Space::Home => false,
            Space::App(a) => *a == id,
            Space::Tiles(t) => t.contains(id),
        }
    }
}

/// The row of spaces and which one you are looking at.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Spaces {
    spaces: [Space; MAX_SPACES],
    len: usize,
    active: usize,
}

impl Default for Spaces {
    fn default() -> Spaces {
        Spaces::new()
    }
}

impl Spaces {
    /// The row of a device with nothing running: Home alone, and active.
    pub const fn new() -> Spaces {
        Spaces {
            spaces: [Space::Home; MAX_SPACES],
            len: 1,
            active: 0,
        }
    }

    /// How many spaces there are, Home included.
    pub fn len(&self) -> usize {
        self.len
    }

    /// Whether the row is empty; it never is, since Home is always there.
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// The index of the space you are looking at.
    pub fn active(&self) -> usize {
        self.active
    }

    /// The space you are looking at.
    pub fn active_space(&self) -> Space {
        self.spaces[self.active]
    }

    /// The space at index `i`, None past the end of the row.
    pub fn get(&self, i: usize) -> Option<Space> {
        if i < self.len {
            Some(self.spaces[i])
        } else {
            None
        }
    }

    /// A new app gets its own space, inserted right after the active one like niri's new column, and becomes the space you are
    /// looking at. Returns the new index, None when the row is full. An app that is live already takes no second space: the view
    /// just moves to the space it has.
    pub fn open_app(&mut self, id: AppId) -> Option<usize> {
        let at = match self.space_of(id) {
            Some(i) => {
                self.active = i;
                return Some(i);
            }
            None => self.active + 1,
        };
        if self.len == MAX_SPACES {
            return None;
        }
        let mut i = self.len;
        while i > at {
            self.spaces[i] = self.spaces[i - 1];
            i -= 1;
        }
        self.spaces[at] = Space::App(id);
        self.len += 1;
        self.active = at;
        Some(at)
    }

    /// The app ended. Its own space closes; an app of a split view only leaves it, and a split view with one app left becomes an
    /// ordinary one-app space again. Home never closes. The view stays on the same space, or moves to the neighbour on its left
    /// when that space was the one that closed (niri).
    pub fn close_app(&mut self, id: AppId) {
        let from = match self.space_of(id) {
            Some(i) => i,
            None => return,
        };
        match self.spaces[from] {
            Space::Home => {}
            Space::App(_) => self.remove_space(from),
            Space::Tiles(t) => {
                let at = match t.index_of(id) {
                    Some(p) => p,
                    None => return,
                };
                let mut rest = t;
                rest.remove_at(at);
                self.spaces[from] = if rest.len() == 1 {
                    Space::App(rest.ids[0])
                } else {
                    Space::Tiles(rest)
                };
            }
        }
    }

    /// Which space shows `id`, None when the app does not run.
    pub fn space_of(&self, id: AppId) -> Option<usize> {
        let mut i = 0;
        while i < self.len {
            if self.spaces[i].contains(id) {
                return Some(i);
            }
            i += 1;
        }
        None
    }

    /// Move the view to space `i`, clamped to the row and without wrapping: this is what a committed swipe asks for.
    pub fn go_to(&mut self, i: usize) {
        self.active = if i >= self.len { self.len - 1 } else { i };
    }

    /// Drag a space to another place in the row, the way GNOME Shell's overview reorders workspaces. Home cannot be dragged and
    /// nothing may be dropped in front of it, so index 0 stays Home. False when either index is outside the row or one of them is
    /// Home; the view follows its own space.
    pub fn move_space(&mut self, from: usize, to: usize) -> bool {
        if from == 0 || to == 0 || from >= self.len || to >= self.len {
            return false;
        }
        if from == to {
            return true;
        }
        let moved = self.spaces[from];
        if from < to {
            let mut i = from;
            while i < to {
                self.spaces[i] = self.spaces[i + 1];
                i += 1;
            }
            self.spaces[to] = moved;
            if self.active == from {
                self.active = to;
            } else if self.active > from && self.active <= to {
                self.active -= 1;
            }
        } else {
            let mut i = from;
            while i > to {
                self.spaces[i] = self.spaces[i - 1];
                i -= 1;
            }
            self.spaces[to] = moved;
            if self.active == from {
                self.active = to;
            } else if self.active >= to && self.active < from {
                self.active += 1;
            }
        }
        true
    }

    /// Split view: `id` moves into the space of `with`, both apps side by side, and the space `id` leaves closes. Both spaces
    /// have to be plain one-app spaces and the two apps different, otherwise nothing happens and the answer is false, as does a
    /// split view that would grow past `MAX_PER_SPACE`. When the view sat on `id`'s space it follows the app into the joined one.
    pub fn tile_together(&mut self, id: AppId, with: AppId) -> bool {
        if id == with {
            return false;
        }
        let i = match self.space_of(id) {
            Some(x) => x,
            None => return false,
        };
        let j = match self.space_of(with) {
            Some(x) => x,
            None => return false,
        };
        if i == j {
            return false;
        }
        if self.spaces[i] != Space::App(id) || self.spaces[j] != Space::App(with) {
            return false;
        }
        let mut joined = Tiles::empty();
        if !joined.push(with) {
            return false;
        }
        if !joined.push(id) {
            return false;
        }
        self.spaces[j] = Space::Tiles(joined);
        let followed = self.active == i;
        self.remove_space(i);
        if followed {
            self.active = if i < j { j - 1 } else { j };
        }
        true
    }

    /// `id` leaves its split view and gets a space of its own again, right after it (GNOME Shell breaking a tiled pair apart).
    /// False when the app runs alone or does not run, or when the row is full and the new space would not fit; then nothing moves.
    pub fn untile(&mut self, id: AppId) -> bool {
        let i = match self.space_of(id) {
            Some(x) => x,
            None => return false,
        };
        let t = match self.spaces[i] {
            Space::Tiles(t) => t,
            _ => return false,
        };
        if self.len == MAX_SPACES {
            return false;
        }
        let at = match t.index_of(id) {
            Some(p) => p,
            None => return false,
        };
        let mut rest = t;
        rest.remove_at(at);
        self.spaces[i] = if rest.len() == 1 {
            Space::App(rest.ids[0])
        } else {
            Space::Tiles(rest)
        };
        self.insert_space(i + 1, Space::App(id));
        true
    }

    /// Takes space `i` out of the row, keeping the view on its own space: it moves to the neighbour on the left when `i` was the
    /// space you looked at, and shifts by one when a space in front of it went. Needs `i > 0` and `i < len`, so Home and the
    /// bounds are never touched, and `active` stays inside the row.
    fn remove_space(&mut self, i: usize) {
        let mut k = i;
        while k + 1 < self.len {
            self.spaces[k] = self.spaces[k + 1];
            k += 1;
        }
        self.len -= 1;
        if self.active == i {
            self.active = i - 1;
        } else if self.active > i {
            self.active -= 1;
        }
    }

    /// Puts `s` in front of index `i`, pushing the rest right and moving the view along with the space it was on. Needs
    /// `i <= len` and room in the row.
    fn insert_space(&mut self, i: usize, s: Space) {
        let mut k = self.len;
        while k > i {
            self.spaces[k] = self.spaces[k - 1];
            k -= 1;
        }
        self.spaces[i] = s;
        self.len += 1;
        if self.active >= i {
            self.active += 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const A: AppId = AppId(0);
    const B: AppId = AppId(1);
    const C: AppId = AppId(2);
    const D: AppId = AppId(3);

    /// The space holding `list` side by side.
    fn tiles_of(list: &[AppId]) -> Space {
        let mut t = Tiles::empty();
        let mut i = 0;
        while i < list.len() {
            assert!(t.push(list[i]));
            i += 1;
        }
        Space::Tiles(t)
    }

    /// Home plus one space per id of `list`, the view on the last one opened.
    fn row(list: &[AppId]) -> Spaces {
        let mut s = Spaces::new();
        let mut i = 0;
        while i < list.len() {
            s.open_app(list[i]);
            i += 1;
        }
        s
    }

    /// Every app the row shows, as a bit set; the ids used here stay below 64.
    fn id_bits(s: &Spaces) -> u64 {
        let mut bits = 0u64;
        let mut i = 0;
        while i < s.len() {
            match s.get(i) {
                Some(Space::App(a)) => {
                    assert!(a.0 < 64);
                    bits |= 1u64 << a.0;
                }
                Some(Space::Tiles(t)) => {
                    let mut k = 0;
                    while k < t.len() {
                        let a = t.get(k).unwrap();
                        assert!(a.0 < 64);
                        bits |= 1u64 << a.0;
                        k += 1;
                    }
                }
                _ => {}
            }
            i += 1;
        }
        bits
    }

    /// The invariants that have to hold after every single operation.
    fn check_invariants(s: &Spaces) {
        assert!(!s.is_empty(), "Home is always in the row");
        assert!(
            s.len() <= MAX_SPACES,
            "the row holds at most MAX_SPACES spaces"
        );
        assert!(s.active() < s.len(), "the view points inside the row");
        assert_eq!(s.get(0), Some(Space::Home), "index 0 is Home");
        assert_eq!(s.get(s.len()), None, "nothing beyond the row");
        let mut seen = 0u64;
        let mut i = 0;
        while i < s.len() {
            match s.get(i) {
                Some(Space::Home) => assert_eq!(i, 0, "Home only ever sits at index 0"),
                Some(Space::App(a)) => {
                    assert!(a.0 < 64);
                    assert_eq!(
                        seen & (1u64 << a.0),
                        0,
                        "an app lives in two spaces at once"
                    );
                    seen |= 1u64 << a.0;
                }
                Some(Space::Tiles(t)) => {
                    assert!(
                        t.len() >= 2 && t.len() <= MAX_PER_SPACE,
                        "a split view holds 2 to 4 apps, not {}",
                        t.len()
                    );
                    let mut k = 0;
                    while k < t.len() {
                        let a = t.get(k).unwrap();
                        assert!(a.0 < 64);
                        assert_eq!(
                            seen & (1u64 << a.0),
                            0,
                            "an app lives in two spaces at once"
                        );
                        seen |= 1u64 << a.0;
                        k += 1;
                    }
                }
                None => panic!("a slot inside the row holds no space"),
            }
            i += 1;
        }
    }

    /// Closes every app with an id below `up_to`, which is all of them for the sequences below, and asks for the empty row back.
    fn close_all(s: &mut Spaces, up_to: u16) {
        let mut k: u16 = 0;
        while k < up_to {
            s.close_app(AppId(k));
            check_invariants(s);
            k += 1;
        }
        assert_eq!(s.len(), 1, "with every app gone only Home is left");
        assert_eq!(s.active(), 0, "and the view is back on Home");
        assert_eq!(s.get(0), Some(Space::Home));
        assert_eq!(id_bits(s), 0);
    }

    /// Runs one operation and checks what it owes the caller: the row invariants are checked by the caller after every op.
    fn apply(s: &mut Spaces, op: (u8, u16, u16)) {
        let (kind, a, b) = op;
        let active_before = s.active();
        let active_content = s.get(active_before);
        match kind {
            0 => {
                let had = s.space_of(AppId(a));
                let len_before = s.len();
                match had {
                    Some(i) => {
                        assert_eq!(
                            s.open_app(AppId(a)),
                            Some(i),
                            "a live app keeps the space it has"
                        );
                        assert_eq!(s.active(), i);
                        assert_eq!(s.len(), len_before);
                    }
                    None => {
                        if len_before == MAX_SPACES {
                            assert_eq!(s.open_app(AppId(a)), None, "a full row takes no new app");
                            assert_eq!(s.len(), len_before);
                        } else {
                            assert_eq!(
                                s.open_app(AppId(a)),
                                Some(active_before + 1),
                                "a new app lands right after the active space"
                            );
                            assert_eq!(
                                s.active(),
                                active_before + 1,
                                "and becomes the space you look at"
                            );
                            assert_eq!(s.get(s.active()), Some(Space::App(AppId(a))));
                            assert_eq!(s.len(), len_before + 1);
                        }
                    }
                }
            }
            1 => {
                let at = s.space_of(AppId(a));
                match at {
                    None => {
                        let len_before = s.len();
                        s.close_app(AppId(a));
                        assert_eq!(
                            s.len(),
                            len_before,
                            "closing an app that does not run changes nothing"
                        );
                        assert_eq!(s.active(), active_before);
                        assert_eq!(s.get(s.active()), active_content);
                    }
                    Some(i) => {
                        let alone = s.get(i) == Some(Space::App(AppId(a)));
                        let left = s.get(i - 1);
                        let was_active = i == active_before;
                        let len_before = s.len();
                        s.close_app(AppId(a));
                        assert_eq!(
                            s.space_of(AppId(a)),
                            None,
                            "the app that ended lives nowhere any more"
                        );
                        if alone && was_active {
                            assert_eq!(s.len(), len_before - 1, "its space closed with it");
                            assert_eq!(
                                s.active(),
                                i - 1,
                                "the view fell back to the neighbour on the left"
                            );
                            assert_eq!(s.get(s.active()), left);
                        } else if alone {
                            assert_eq!(s.len(), len_before - 1, "its space closed with it");
                            assert_eq!(
                                s.active(),
                                if active_before > i {
                                    active_before - 1
                                } else {
                                    active_before
                                }
                            );
                            assert_eq!(
                                s.get(s.active()),
                                active_content,
                                "closing a space you do not look at keeps the view"
                            );
                        } else {
                            assert_eq!(s.len(), len_before, "a split view survives losing one app");
                            assert_eq!(s.active(), active_before, "and the view does not move");
                            assert!(
                                !s.get(i).unwrap().contains(AppId(a)),
                                "the app left that split view"
                            );
                        }
                    }
                }
            }
            2 => {
                s.go_to(a as usize);
                assert_eq!(
                    s.active(),
                    if (a as usize) >= s.len() {
                        s.len() - 1
                    } else {
                        a as usize
                    },
                    "go_to clamps and never wraps"
                );
                assert!(s.active() < s.len());
            }
            3 => {
                let len_before = s.len();
                let bits = id_bits(s);
                let ok = s.move_space(a as usize, b as usize);
                let expected =
                    a != 0 && b != 0 && (a as usize) < len_before && (b as usize) < len_before;
                assert_eq!(
                    ok, expected,
                    "only a space inside the row, and never Home, may be dragged"
                );
                assert_eq!(s.len(), len_before);
                assert_eq!(id_bits(s), bits, "reordering changes no app");
                assert_eq!(s.get(0), Some(Space::Home), "Home stays at index 0");
                assert_eq!(
                    s.get(s.active()),
                    active_content,
                    "the view follows its own space"
                );
            }
            4 => {
                let ia = s.space_of(AppId(a));
                let ib = s.space_of(AppId(b));
                let both_plain = a != b
                    && match (ia, ib) {
                        (Some(i), Some(j)) => {
                            i != j
                                && s.get(i) == Some(Space::App(AppId(a)))
                                && s.get(j) == Some(Space::App(AppId(b)))
                        }
                        _ => false,
                    };
                let len_before = s.len();
                let ok = s.tile_together(AppId(a), AppId(b));
                assert_eq!(ok, both_plain, "only two different plain app spaces join");
                if ok {
                    let emptied = ia.unwrap();
                    let target = ib.unwrap();
                    let joined = s.space_of(AppId(a)).unwrap();
                    assert_eq!(
                        s.space_of(AppId(b)),
                        Some(joined),
                        "both apps share one space now"
                    );
                    assert_eq!(
                        s.get(joined),
                        Some(tiles_of(&[AppId(b), AppId(a)])),
                        "the app that was joined stays on the left"
                    );
                    assert_eq!(s.len(), len_before - 1, "the emptied space closed");
                    if active_before == emptied {
                        assert_eq!(
                            s.active(),
                            joined,
                            "the view followed the app into the joined space"
                        );
                    } else {
                        assert_eq!(
                            s.active(),
                            if active_before > emptied {
                                active_before - 1
                            } else {
                                active_before
                            }
                        );
                        if active_before != target {
                            assert_eq!(
                                s.get(s.active()),
                                active_content,
                                "joining two other spaces leaves the view alone"
                            );
                        }
                    }
                } else {
                    assert_eq!(s.len(), len_before);
                    assert_eq!(s.get(s.active()), active_content);
                }
            }
            5 => {
                let i = s.space_of(AppId(a));
                let in_tiles = match i {
                    Some(x) => match s.get(x) {
                        Some(Space::Tiles(t)) => t.contains(AppId(a)),
                        _ => false,
                    },
                    None => false,
                };
                let len_before = s.len();
                let ok = s.untile(AppId(a));
                assert_eq!(
                    ok,
                    in_tiles && len_before < MAX_SPACES,
                    "only an app of a split view leaves it, and only with room"
                );
                if ok {
                    let at = i.unwrap();
                    assert_eq!(s.len(), len_before + 1);
                    assert_eq!(
                        s.get(at + 1),
                        Some(Space::App(AppId(a))),
                        "its own space opens right after the split view"
                    );
                    assert!(
                        !s.get(at).unwrap().contains(AppId(a)),
                        "the split view it left holds it no more"
                    );
                    assert_eq!(
                        s.active(),
                        if active_before > at {
                            active_before + 1
                        } else {
                            active_before
                        }
                    );
                    if active_before != at {
                        assert_eq!(
                            s.get(s.active()),
                            active_content,
                            "the view stays on its own space"
                        );
                    }
                } else {
                    assert_eq!(s.len(), len_before);
                    assert_eq!(s.get(s.active()), active_content);
                }
            }
            _ => panic!("unknown operation kind"),
        }
        check_invariants(s);
    }

    #[test]
    fn a_fresh_row_is_home_alone() {
        let s = Spaces::new();
        assert_eq!(s.len(), 1);
        assert_eq!(s.active(), 0);
        assert_eq!(s.get(0), Some(Space::Home));
        assert_eq!(s.get(1), None);
        assert_eq!(s.active_space(), Space::Home);
        assert!(!s.is_empty());
        assert_eq!(s.space_of(A), None);
        check_invariants(&s);
    }

    #[test]
    fn open_app_inserts_right_after_the_active_space_and_looks_at_it() {
        let mut s = Spaces::new();
        assert_eq!(s.open_app(A), Some(1));
        assert_eq!(s.open_app(B), Some(2));
        assert_eq!(s.open_app(C), Some(3));
        assert_eq!(s.active(), 3);
        s.go_to(0);
        assert_eq!(
            s.open_app(D),
            Some(1),
            "opened after Home, pushing the rest right"
        );
        assert_eq!(s.get(0), Some(Space::Home));
        assert_eq!(s.get(1), Some(Space::App(D)));
        assert_eq!(s.get(2), Some(Space::App(A)));
        assert_eq!(s.get(3), Some(Space::App(B)));
        assert_eq!(s.get(4), Some(Space::App(C)));
        assert_eq!(s.active(), 1);
        assert_eq!(s.space_of(B), Some(3));
    }

    #[test]
    fn open_app_of_a_live_app_only_moves_the_view() {
        let mut s = row(&[A, B, C]);
        s.go_to(0);
        assert_eq!(s.open_app(B), Some(2));
        assert_eq!(s.active(), 2);
        assert_eq!(s.len(), 4);
        assert_eq!(s.get(2), Some(Space::App(B)));
    }

    #[test]
    fn a_full_row_refuses_a_new_space_and_changes_nothing() {
        let mut s = Spaces::new();
        let mut k: u16 = 0;
        while k < (MAX_SPACES - 1) as u16 {
            assert!(s.open_app(AppId(k)).is_some());
            k += 1;
        }
        assert_eq!(s.len(), MAX_SPACES);
        let before = s.clone();
        assert_eq!(s.open_app(AppId(100)), None);
        assert_eq!(s, before);
        check_invariants(&s);
    }

    #[test]
    fn close_app_closes_its_space_and_falls_back_to_the_left() {
        let mut s = row(&[A, B, C]);
        assert_eq!(s.active(), 3);
        s.close_app(B);
        assert_eq!(s.len(), 3);
        assert_eq!(s.get(1), Some(Space::App(A)));
        assert_eq!(s.get(2), Some(Space::App(C)));
        assert_eq!(
            s.active(),
            2,
            "the view shifted with the spaces behind the closed one"
        );
        s.close_app(C);
        assert_eq!(
            s.active(),
            1,
            "the space you looked at closed, so the left neighbour took over"
        );
        assert_eq!(s.active_space(), Space::App(A));
        s.close_app(D);
        assert_eq!(s.len(), 2, "an app that does not run changes nothing");
        s.close_app(A);
        assert_eq!(s.len(), 1);
        assert_eq!(s.active(), 0);
        assert_eq!(s.get(0), Some(Space::Home), "Home is still there");
    }

    #[test]
    fn close_app_in_the_middle_of_the_row_keeps_the_view_where_it_was() {
        let mut s = row(&[A, B, C]);
        s.go_to(2);
        let looked_at = s.active_space();
        s.close_app(A);
        assert_eq!(s.active(), 1);
        assert_eq!(s.active_space(), looked_at);
    }

    #[test]
    fn tiles_and_untile_share_one_space_then_split_again() {
        let mut s = row(&[A, B, C]);
        s.go_to(0);
        assert!(s.tile_together(B, A));
        assert_eq!(s.len(), 3);
        assert_eq!(
            s.get(1),
            Some(tiles_of(&[A, B])),
            "the joined app lands to the right of the one it joined"
        );
        assert_eq!(s.space_of(B), Some(1));
        assert_eq!(s.space_of(A), Some(1));
        assert_eq!(
            s.active(),
            0,
            "tiling two other spaces does not move the view"
        );
        assert_eq!(s.get(2), Some(Space::App(C)));
        assert!(s.untile(B));
        assert_eq!(s.len(), 4);
        assert_eq!(s.get(1), Some(Space::App(A)));
        assert_eq!(
            s.get(2),
            Some(Space::App(B)),
            "its own space, right after the split view"
        );
        assert_eq!(s.get(3), Some(Space::App(C)));
        assert_eq!(s.active(), 0);
    }

    #[test]
    fn tiling_the_space_you_look_at_carries_the_view_along() {
        let mut s = row(&[A, B, C]);
        assert_eq!(s.active(), 3);
        assert!(s.tile_together(C, A));
        assert_eq!(s.len(), 3);
        assert_eq!(s.get(1), Some(tiles_of(&[A, C])));
        assert_eq!(s.space_of(C), Some(1));
        assert_eq!(
            s.active(),
            1,
            "the emptied space is gone, the view followed the app"
        );
        assert_eq!(s.active_space(), tiles_of(&[A, C]));
    }

    #[test]
    fn close_app_leaves_a_split_view_and_shrinks_it() {
        let mut s = row(&[A, B]);
        s.go_to(0);
        assert!(s.tile_together(B, A));
        assert_eq!(s.len(), 2);
        s.close_app(A);
        assert_eq!(
            s.get(1),
            Some(Space::App(B)),
            "one app left means a plain space again"
        );
        assert_eq!(s.len(), 2);
        s.close_app(B);
        assert_eq!(s.len(), 1);
        assert_eq!(s.active(), 0);
    }

    #[test]
    fn tile_together_refuses_what_it_cannot_do() {
        let mut s = row(&[A, B, C]);
        s.go_to(0);
        assert!(!s.tile_together(A, A), "an app does not join itself");
        assert!(!s.tile_together(A, AppId(77)), "a missing app");
        assert!(!s.tile_together(D, A), "a missing app on the other side");
        assert!(s.tile_together(B, A));
        assert!(!s.tile_together(B, A), "the pair is already one space");
        assert!(
            !s.tile_together(B, C),
            "B lives in a split view, not a plain space"
        );
        assert!(
            !s.tile_together(C, B),
            "and so would the joined app be the target"
        );
        assert_eq!(s.len(), 3);
        assert_eq!(s.get(1), Some(tiles_of(&[A, B])));
        assert_eq!(s.get(2), Some(Space::App(C)));
    }

    #[test]
    fn untile_refuses_a_solo_app_and_needs_room() {
        let mut s = row(&[A, B]);
        assert!(!s.untile(A), "a plain space has nothing to split");
        assert!(
            !s.untile(AppId(9)),
            "and an app that does not run cannot leave anything"
        );
        s.go_to(0);
        assert!(s.tile_together(B, A));
        assert_eq!(s.len(), 2);
        let mut k: u16 = 10;
        while s.len() < MAX_SPACES {
            assert!(s.open_app(AppId(k)).is_some());
            k += 1;
        }
        assert_eq!(s.len(), MAX_SPACES);
        let before = s.clone();
        assert!(!s.untile(A), "no room for a new space, so nothing moves");
        assert_eq!(s, before);
        let pair = s.space_of(A).unwrap();
        assert_eq!(s.space_of(B), Some(pair));
        assert_eq!(
            s.get(pair),
            Some(tiles_of(&[A, B])),
            "the split view is intact, wherever the new apps pushed it"
        );
    }

    #[test]
    fn move_space_reorders_and_keeps_home_first_and_the_view_in_place() {
        let mut s = row(&[A, B, C]);
        assert!(!s.move_space(0, 2), "Home cannot be dragged");
        assert!(!s.move_space(2, 0), "nothing may go in front of Home");
        assert!(!s.move_space(4, 1), "past the end");
        assert!(!s.move_space(1, 9));
        assert!(s.move_space(1, 3));
        assert_eq!(s.len(), 4);
        assert_eq!(s.get(0), Some(Space::Home));
        assert_eq!(s.get(1), Some(Space::App(B)));
        assert_eq!(s.get(2), Some(Space::App(C)));
        assert_eq!(s.get(3), Some(Space::App(A)));
        assert_eq!(
            s.active(),
            2,
            "the view stayed on C, which slid left as A passed it"
        );
        assert_eq!(s.active_space(), Space::App(C));
        assert!(s.move_space(3, 1));
        assert_eq!(s.get(1), Some(Space::App(A)));
        assert_eq!(s.get(2), Some(Space::App(B)));
        assert_eq!(s.get(3), Some(Space::App(C)));
        assert_eq!(
            s.active(),
            3,
            "C moved to the end and the view went with it"
        );
        assert_eq!(s.active_space(), Space::App(C));
    }

    #[test]
    fn move_space_of_the_viewed_space_carries_the_view() {
        let mut s = row(&[A, B, C]);
        s.go_to(1);
        assert!(s.move_space(1, 2));
        assert_eq!(s.active(), 2);
        assert_eq!(s.active_space(), Space::App(A));
        assert!(s.move_space(2, 1));
        assert_eq!(s.active(), 1);
        assert_eq!(s.active_space(), Space::App(A));
        assert!(
            s.move_space(2, 2),
            "dropping a space where it already is changes nothing"
        );
        assert_eq!(s.active(), 1);
        assert_eq!(s.active_space(), Space::App(A));
    }

    #[test]
    fn go_to_clamps_to_the_row_without_wrapping() {
        let mut s = row(&[A, B, C]);
        s.go_to(99);
        assert_eq!(s.active(), 3);
        s.go_to(0);
        assert_eq!(s.active(), 0);
        assert_eq!(s.active_space(), Space::Home);
        s.go_to(2);
        assert_eq!(s.active(), 2);
        s.close_app(C);
        assert_eq!(s.len(), 3);
        s.go_to(5);
        assert_eq!(s.active(), 2, "the last space after the row shrank");
    }

    #[test]
    fn space_of_reports_the_one_space_of_every_app() {
        let mut s = row(&[A, B]);
        assert!(s.tile_together(A, B), "A joins the space B is looked at");
        assert_eq!(s.get(1), Some(tiles_of(&[B, A])));
        assert_eq!(s.space_of(A), Some(1));
        assert_eq!(
            s.space_of(B),
            Some(1),
            "both apps of a split view report the same space"
        );
        s.open_app(C);
        assert_eq!(
            s.space_of(C),
            Some(2),
            "a new app opens next to the pair you are on"
        );
        assert_eq!(s.space_of(A), Some(1));
        assert_eq!(s.space_of(D), None);
        assert_eq!(s.space_of(AppId(500)), None);
        check_invariants(&s);
    }

    #[test]
    fn closing_every_app_leaves_home_alone_again() {
        let mut s = row(&[A, B, C, D]);
        s.go_to(1);
        assert!(s.tile_together(C, B));
        assert!(s.untile(B));
        close_all(&mut s, 16);
    }

    #[test]
    fn a_split_view_of_three_apps_shrinks_but_stays_a_split_view() {
        let mut s = row(&[A, B]);
        s.go_to(0);
        assert!(s.tile_together(B, A));
        s.spaces[1] = tiles_of(&[A, B, C]);
        check_invariants(&s);
        assert_eq!(s.space_of(C), Some(1), "the third app shares the space");
        assert!(s.untile(C));
        assert_eq!(s.len(), 3);
        assert_eq!(
            s.get(1),
            Some(tiles_of(&[A, B])),
            "two apps left, still side by side"
        );
        assert_eq!(s.get(2), Some(Space::App(C)));
        s.close_app(A);
        assert_eq!(
            s.get(1),
            Some(Space::App(B)),
            "one app left, so a plain space"
        );
        assert_eq!(s.len(), 3, "the split view did not close with the app");
        close_all(&mut s, 16);
    }

    #[test]
    fn a_split_view_holds_apps_in_the_order_they_joined_and_forgets_them() {
        let mut t = Tiles::empty();
        assert!(t.is_empty());
        assert_eq!(t.len(), 0);
        assert_eq!(t.get(0), None);
        assert_eq!(t.index_of(A), None);
        assert!(!t.contains(A));
        assert!(t.push(A));
        assert!(t.push(B));
        assert_eq!(t.get(1), Some(B));
        assert_eq!(t.index_of(B), Some(1));
        assert!(t.contains(B));
        t.remove_at(0);
        assert_eq!(t.get(0), Some(B));
        assert_eq!(t.len(), 1);
        assert!(!t.contains(A));
        assert!(t.push(C));
        assert!(t.push(D));
        assert!(t.push(AppId(7)));
        assert!(!t.push(AppId(8)), "MAX_PER_SPACE apps and no more");
        assert_eq!(t.len(), MAX_PER_SPACE);
        assert!(Space::App(A).contains(A));
        assert!(!Space::Home.contains(A));
        assert!(Space::Home.is_home());
        assert!(!Space::App(A).is_home());
    }

    /// Every sequence of up to 4 operations of the row: each operation keeps the invariants and owes what the design says.
    #[test]
    fn exhaustive_space_operation_sequences() {
        let ops: [(u8, u16, u16); 24] = [
            (0, 0, 0),
            (0, 1, 0),
            (0, 2, 0),
            (0, 3, 0),
            (1, 0, 0),
            (1, 1, 0),
            (1, 2, 0),
            (1, 3, 0),
            (2, 0, 0),
            (2, 1, 0),
            (2, 3, 0),
            (2, 9, 0),
            (3, 1, 2),
            (3, 2, 1),
            (3, 3, 1),
            (3, 2, 9),
            (3, 1, 1),
            (4, 0, 1),
            (4, 1, 0),
            (4, 1, 2),
            (4, 2, 0),
            (4, 3, 1),
            (5, 1, 0),
            (5, 2, 0),
        ];
        let mut depth = 1;
        let mut total = 1usize;
        while depth <= 4 {
            total *= ops.len();
            let mut n = 0;
            while n < total {
                let mut s = Spaces::new();
                let mut rest = n;
                let mut d = 0;
                while d < depth {
                    let idx = rest % ops.len();
                    rest /= ops.len();
                    apply(&mut s, ops[idx]);
                    d += 1;
                }
                close_all(&mut s, 4);
                n += 1;
            }
            depth += 1;
        }
    }

    /// Long sequences of operations, driven by a fixed integer generator so the run is repeatable: these reach a full row, where
    /// a new space does not fit and a split view cannot be broken apart.
    #[test]
    fn long_generated_operation_sequences() {
        let mut round = 0;
        while round < 120 {
            let mut state: u64 = 0x2545F491 + round as u64 * 0x9E3779B1;
            let mut s = Spaces::new();
            let mut step = 0;
            while step < 120 {
                state = state
                    .wrapping_mul(6364136223846793005)
                    .wrapping_add(1442695040888963407);
                let kind = (state >> 33) % 6;
                let a = ((state >> 17) % 16) as u16;
                let b = ((state >> 8) % 16) as u16;
                apply(&mut s, (kind as u8, a, b));
                step += 1;
            }
            close_all(&mut s, 16);
            round += 1;
        }
    }
}
