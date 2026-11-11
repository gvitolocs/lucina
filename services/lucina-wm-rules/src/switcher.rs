//! The most-recently-used order of the live apps and the Meta+Tab session (`docs/design/juno-window-manager.md`, section 7).
//! The order is the one GNOME Shell's app switcher keeps: the focused app at the front, and focusing an app that is already
//! listed moves it to the front instead of duplicating it. While the Meta key is held the switcher shows a snapshot of that
//! order taken when the key went down, exactly like GNOME Shell: the list stays put and only the selection moves.
//! Integer only, fixed size, no allocation.

use crate::mru_rules;

/// How many live apps the switcher order can hold at most.
pub const MAX_APPS: usize = mru_rules::MAX;

/// A running application.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AppId(pub u16);

/// The most-recently-used order of the live apps: `ids[0]` is the focused app, the rest are ordered by how recently they were
/// focused. GNOME Shell's `Alt+Tab` list.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Mru {
    ids: [AppId; MAX_APPS],
    len: usize,
}

impl Default for Mru {
    fn default() -> Mru {
        Mru::new()
    }
}

impl Mru {
    /// The order of a device with no app running.
    pub const fn new() -> Mru {
        Mru {
            ids: [AppId(0); MAX_APPS],
            len: 0,
        }
    }

    /// How many apps are live.
    pub fn len(&self) -> usize {
        self.len
    }

    /// True when no app runs at all.
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// The live apps, the focused one first.
    pub fn as_slice(&self) -> &[AppId] {
        &self.ids[0..self.len]
    }

    /// The focused app, None when nothing runs.
    pub fn front(&self) -> Option<AppId> {
        if self.len == 0 {
            None
        } else {
            Some(self.ids[0])
        }
    }

    /// The place of `id` in the order, None when the app does not run.
    pub fn index_of(&self, id: AppId) -> Option<usize> {
        let mut i = 0;
        while i < self.len {
            if self.ids[i] == id {
                return Some(i);
            }
            i += 1;
        }
        None
    }

    /// Whether `id` runs.
    pub fn contains(&self, id: AppId) -> bool {
        self.index_of(id).is_some()
    }

    /// App started or was focused: it moves to the front, an app that was not listed is added at the front, and the relative
    /// order of the others is kept (GNOME Shell). Returns false, leaving the order untouched, only when the app is absent and
    /// the list is already full.
    pub fn focus(&mut self, id: AppId) -> bool {
        match self.index_of(id) {
            Some(i) => mru_rules::move_to_front(&mut self.ids, self.len, i),
            None => {
                if self.len == MAX_APPS {
                    return false;
                }
                mru_rules::push_front(&mut self.ids, self.len, id);
                self.len += 1;
            }
        }
        true
    }

    /// App ended: it leaves the order, the others keep their places. Returns whether the app was live.
    pub fn remove(&mut self, id: AppId) -> bool {
        let from = match self.index_of(id) {
            Some(i) => i,
            None => return false,
        };
        mru_rules::remove_at(&mut self.ids, self.len, from);
        self.len -= 1;
        true
    }
}

/// One Meta+Tab session (GNOME Shell): the shown list is a snapshot of the MRU taken when the modifier went down and does not
/// change while it is held, only the selection moves; an app that ends meanwhile is taken out of the snapshot.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Switcher {
    items: [AppId; MAX_APPS],
    len: usize,
    sel: usize,
}

impl Switcher {
    /// Starts from a snapshot of `mru`. The selection lands on index 1, the previous app, or on index 0 when only one app runs;
    /// holding Shift first (backward) starts the walk from the other end, on the last app.
    pub fn begin(mru: &Mru, backward: bool) -> Switcher {
        let mut s = Switcher {
            items: [AppId(0); MAX_APPS],
            len: mru.len(),
            sel: 0,
        };
        let mut i = 0;
        while i < s.len {
            s.items[i] = mru.ids[i];
            i += 1;
        }
        if s.len > 1 {
            s.sel = if backward { s.len - 1 } else { 1 };
        }
        s
    }

    /// Tab moves the selection forward, Shift+Tab backward, both wrapping around the snapshot. Does nothing when the snapshot is
    /// empty.
    pub fn step(&mut self, backward: bool) {
        if self.len == 0 {
            return;
        }
        if backward {
            self.sel = (self.sel + self.len - 1) % self.len;
        } else {
            self.sel = (self.sel + 1) % self.len;
        }
    }

    /// The app the selection is on, None when the snapshot is empty.
    pub fn selected(&self) -> Option<AppId> {
        if self.len == 0 {
            None
        } else {
            Some(self.items[self.sel])
        }
    }

    /// The snapshot the switcher walks.
    pub fn items(&self) -> &[AppId] {
        &self.items[0..self.len]
    }

    /// An app ended while the switcher was open: it leaves the snapshot and the selection stays on the same app when that is
    /// still possible, otherwise it lands on the next app, wrapping around at the end.
    pub fn remove(&mut self, id: AppId) {
        let from = match self.index_of(id) {
            Some(i) => i,
            None => return,
        };
        let mut i = from;
        while i + 1 < self.len {
            self.items[i] = self.items[i + 1];
            i += 1;
        }
        self.len -= 1;
        if from < self.sel {
            self.sel -= 1;
        } else if from == self.sel && self.sel >= self.len {
            self.sel = 0;
        }
    }

    /// Index of `id` in the snapshot, None when it is not there.
    fn index_of(&self, id: AppId) -> Option<usize> {
        let mut i = 0;
        while i < self.len {
            if self.items[i] == id {
                return Some(i);
            }
            i += 1;
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const A: AppId = AppId(10);
    const B: AppId = AppId(20);
    const C: AppId = AppId(30);
    const D: AppId = AppId(40);

    /// The MRU whose front is the last element of `list`: focusing in reverse order builds it from the back.
    fn mru_back(list: &[AppId]) -> Mru {
        let mut m = Mru::new();
        let mut i = list.len();
        while i > 0 {
            i -= 1;
            assert!(m.focus(list[i]));
        }
        m
    }

    /// `list` has to be `old` with `gone` taken out, in the same relative order, optionally with `head` pushed in front.
    fn assert_order_kept(list: &[AppId], old: &[AppId], gone: AppId, head: Option<AppId>) {
        let mut k = 0;
        if let Some(h) = head {
            assert!(!list.is_empty());
            assert_eq!(list[0], h);
            k = 1;
        }
        let mut j = 0;
        while j < old.len() {
            if old[j] != gone {
                assert!(k < list.len(), "the list lost an app");
                assert_eq!(list[k], old[j]);
                k += 1;
            }
            j += 1;
        }
        assert_eq!(k, list.len(), "the list holds an app it should not");
    }

    #[test]
    fn focus_puts_the_app_at_the_front_and_keeps_the_relative_order_of_the_others() {
        let mut m = Mru::new();
        assert!(m.focus(A));
        assert!(m.focus(B));
        assert!(m.focus(C));
        assert_eq!(m.as_slice(), &[C, B, A]);
        assert!(m.focus(A));
        assert_eq!(m.as_slice(), &[A, C, B]);
        assert_eq!(m.len(), 3);
        assert_eq!(m.front(), Some(A));
        assert_eq!(m.index_of(B), Some(2));
        assert!(m.contains(C));
        assert!(!m.contains(D));
    }

    #[test]
    fn focus_of_the_app_that_is_already_at_the_front_changes_nothing() {
        let mut m = mru_back(&[B, A]);
        assert!(m.focus(A));
        assert_eq!(m.as_slice(), &[A, B]);
    }

    #[test]
    fn remove_drops_the_app_and_keeps_the_order_of_the_rest() {
        let mut m = mru_back(&[A, B, C]);
        assert!(m.remove(B));
        assert_eq!(m.as_slice(), &[A, C]);
        assert!(!m.remove(B));
        assert_eq!(m.as_slice(), &[A, C]);
        assert!(m.remove(A));
        assert!(m.remove(C));
        assert!(m.is_empty());
        assert_eq!(m.front(), None);
        assert_eq!(m.index_of(A), None);
    }

    #[test]
    fn focus_of_a_new_app_is_refused_only_when_the_list_is_full() {
        let mut m = Mru::new();
        let mut k: u16 = 0;
        while k < MAX_APPS as u16 {
            assert!(m.focus(AppId(k)));
            k += 1;
        }
        assert_eq!(m.len(), MAX_APPS);
        let before = m;
        assert!(!m.focus(AppId(999)));
        assert_eq!(m, before);
        assert!(m.remove(AppId(15)));
        assert!(m.focus(AppId(999)));
        assert_eq!(m.front(), Some(AppId(999)));
        assert_eq!(m.len(), MAX_APPS);
    }

    #[test]
    fn begin_selects_the_previous_app_or_the_last_one_when_started_backward() {
        let m = mru_back(&[A, B, C]);
        let s = Switcher::begin(&m, false);
        assert_eq!(s.items(), &[A, B, C]);
        assert_eq!(s.selected(), Some(B));
        assert_eq!(Switcher::begin(&m, true).selected(), Some(C));
        let one = mru_back(&[A]);
        assert_eq!(Switcher::begin(&one, false).selected(), Some(A));
        assert_eq!(Switcher::begin(&one, true).selected(), Some(A));
        let empty = Mru::new();
        let s0 = Switcher::begin(&empty, false);
        assert!(s0.items().is_empty());
        assert_eq!(s0.selected(), None);
    }

    #[test]
    fn step_walks_the_snapshot_and_wraps_both_ways() {
        let m = mru_back(&[A, B, C]);
        let mut s = Switcher::begin(&m, false);
        assert_eq!(s.selected(), Some(B));
        s.step(false);
        assert_eq!(s.selected(), Some(C));
        s.step(false);
        assert_eq!(s.selected(), Some(A));
        s.step(true);
        assert_eq!(s.selected(), Some(C));
        s.step(true);
        s.step(true);
        assert_eq!(s.selected(), Some(A));
        assert_eq!(
            s.items(),
            &[A, B, C],
            "the list does not change while the modifier is held"
        );
    }

    #[test]
    fn step_on_an_empty_snapshot_does_nothing() {
        let mut s = Switcher::begin(&Mru::new(), false);
        s.step(false);
        s.step(true);
        assert_eq!(s.selected(), None);
        assert!(s.items().is_empty());
    }

    #[test]
    fn stepping_n_times_over_n_items_returns_to_the_start() {
        let m = mru_back(&[A, B, C, D]);
        let mut s = Switcher::begin(&m, false);
        let start = s.selected();
        let n = s.items().len();
        let mut i = 0;
        while i < n {
            s.step(false);
            i += 1;
        }
        assert_eq!(s.selected(), start);
        let mut b = Switcher::begin(&m, true);
        let bstart = b.selected();
        let mut i = 0;
        while i < n {
            b.step(true);
            i += 1;
        }
        assert_eq!(b.selected(), bstart);
    }

    #[test]
    fn meta_tab_alone_toggles_between_the_last_two_apps() {
        let mut m = Mru::new();
        assert!(m.focus(B));
        assert!(m.focus(A));
        let mut i = 0;
        while i < 6 {
            let s = Switcher::begin(&m, false);
            let left = m.front().unwrap();
            let chosen = s.selected().unwrap();
            assert_eq!(chosen, if i % 2 == 0 { B } else { A });
            m.focus(chosen);
            assert_eq!(m.as_slice(), &[chosen, left]);
            i += 1;
        }
    }

    #[test]
    fn selected_is_some_exactly_when_the_snapshot_is_not_empty() {
        let mut m = Mru::new();
        assert_eq!(Switcher::begin(&m, false).selected(), None);
        assert!(m.focus(A));
        assert_eq!(Switcher::begin(&m, false).selected(), Some(A));
        assert!(m.focus(B));
        assert!(Switcher::begin(&m, false).selected().is_some());
        assert!(m.remove(B));
        assert_eq!(Switcher::begin(&m, false).selected(), Some(A));
        assert!(m.remove(A));
        assert_eq!(Switcher::begin(&m, false).selected(), None);
    }

    #[test]
    fn remove_keeps_the_selection_on_the_same_app_when_it_can() {
        let m = mru_back(&[A, B, C]);
        let mut s = Switcher::begin(&m, false);
        assert_eq!(s.selected(), Some(B));
        s.remove(C);
        assert_eq!(s.items(), &[A, B]);
        assert_eq!(
            s.selected(),
            Some(B),
            "an app behind the selection does not move it"
        );
        s.remove(A);
        assert_eq!(s.items(), &[B]);
        assert_eq!(
            s.selected(),
            Some(B),
            "an app in front of the selection shifts the index, not the app"
        );
        s.remove(B);
        assert!(s.items().is_empty());
        assert_eq!(s.selected(), None);
        s.remove(B);
        assert_eq!(s.selected(), None);
    }

    #[test]
    fn remove_of_the_selected_app_moves_to_the_next_one_wrapping() {
        let m = mru_back(&[A, B, C, D]);
        let mut s = Switcher::begin(&m, false);
        s.step(false);
        assert_eq!(s.selected(), Some(C));
        s.remove(C);
        assert_eq!(s.items(), &[A, B, D]);
        assert_eq!(
            s.selected(),
            Some(D),
            "the selection follows the apps that stayed"
        );
        s.remove(A);
        assert_eq!(s.selected(), Some(D));
        s.remove(D);
        assert_eq!(s.items(), &[B]);
        assert_eq!(
            s.selected(),
            Some(B),
            "the selected app left, the walk restarts at the front"
        );
    }

    #[test]
    fn remove_of_an_absent_app_changes_nothing() {
        let m = mru_back(&[A, B]);
        let mut s = Switcher::begin(&m, false);
        s.remove(D);
        assert_eq!(s.items(), &[A, B]);
        assert_eq!(s.selected(), Some(B));
    }

    /// Every sequence of up to 6 focus/remove operations on 4 app ids: the order stays duplicate-free, holds exactly the live
    /// apps, and each operation keeps the relative order of the apps it does not touch.
    #[test]
    fn exhaustive_mru_operation_sequences() {
        let ids = [A, B, C, D];
        let mut seq = [0usize; 6];
        let mut live = [false; 4];
        let mut depth = 1;
        let mut total = 1usize;
        while depth <= 6 {
            total *= 2 * ids.len();
            let mut n = 0;
            while n < total {
                let mut rest = n;
                let mut d = 0;
                while d < depth {
                    seq[d] = rest % (2 * ids.len());
                    rest /= 2 * ids.len();
                    d += 1;
                }
                let mut m = Mru::new();
                let mut i = 0;
                while i < 4 {
                    live[i] = false;
                    i += 1;
                }
                let mut old = [AppId(0); MAX_APPS];
                d = 0;
                while d < depth {
                    let op = seq[d];
                    let k = op / 2;
                    let is_remove = op % 2 == 1;
                    let old_len = m.len();
                    let mut j = 0;
                    while j < old_len {
                        old[j] = m.as_slice()[j];
                        j += 1;
                    }
                    if is_remove {
                        let was_live = live[k];
                        assert_eq!(m.remove(ids[k]), was_live);
                        live[k] = false;
                        assert_order_kept(m.as_slice(), &old[0..old_len], ids[k], None);
                    } else {
                        assert!(m.focus(ids[k]));
                        live[k] = true;
                        assert_order_kept(m.as_slice(), &old[0..old_len], ids[k], Some(ids[k]));
                    }
                    check_mru(&m, &ids, &live);
                    d += 1;
                }
                n += 1;
            }
            depth += 1;
        }
    }

    /// The MRU invariants: no duplicates, and one slot for every live app and none for a dead one.
    fn check_mru(m: &Mru, ids: &[AppId; 4], live: &[bool; 4]) {
        let mut expected = 0;
        let mut k = 0;
        while k < ids.len() {
            if live[k] {
                expected += 1;
                assert!(m.contains(ids[k]), "a live app is missing");
            } else {
                assert!(!m.contains(ids[k]), "a dead app is still listed");
            }
            k += 1;
        }
        assert_eq!(m.len(), expected);
        assert_eq!(m.is_empty(), expected == 0);
        let s = m.as_slice();
        let mut i = 0;
        while i < s.len() {
            let mut j = i + 1;
            while j < s.len() {
                assert_ne!(s[i], s[j], "the order holds the same app twice");
                j += 1;
            }
            i += 1;
        }
    }

    /// Every sequence of up to 6 step/remove operations on an open switcher over the snapshot [A, B, C, D]: the list only ever
    /// loses the app that ended, the selection stays on the same app whenever that app is still there, and it is Some exactly
    /// while the snapshot is not empty.
    #[test]
    fn exhaustive_switcher_operation_sequences() {
        let ids = [A, B, C, D];
        let mut seq = [0usize; 6];
        let mut depth = 1;
        let mut total = 1usize;
        while depth <= 6 {
            total *= 2 + ids.len();
            let mut n = 0;
            while n < total {
                let mut rest = n;
                let mut d = 0;
                while d < depth {
                    seq[d] = rest % (2 + ids.len());
                    rest /= 2 + ids.len();
                    d += 1;
                }
                let m = mru_back(&ids);
                let mut s = Switcher::begin(&m, false);
                let mut model = [AppId(0); MAX_APPS];
                let mut mlen = 0;
                while mlen < ids.len() {
                    model[mlen] = ids[mlen];
                    mlen += 1;
                }
                assert_eq!(s.selected(), Some(B));
                let mut i = 0;
                while i < depth {
                    let op = seq[i];
                    let before = s.selected();
                    let mut old = [AppId(0); MAX_APPS];
                    let mut k = 0;
                    while k < mlen {
                        old[k] = model[k];
                        k += 1;
                    }
                    let old_len = mlen;
                    if op == 0 {
                        s.step(false);
                    } else if op == 1 {
                        s.step(true);
                    } else {
                        let id = ids[op - 2];
                        s.remove(id);
                        k = 0;
                        let mut w = 0;
                        while k < mlen {
                            if model[k] != id {
                                model[w] = model[k];
                                w += 1;
                            }
                            k += 1;
                        }
                        mlen = w;
                    }
                    assert_eq!(
                        s.items(),
                        &model[0..mlen],
                        "the snapshot drifted away from the model"
                    );
                    assert_eq!(s.selected().is_some(), mlen > 0);
                    if mlen == 0 {
                        assert_eq!(s.selected(), None);
                    } else {
                        let sel = s.selected().unwrap();
                        assert!(
                            index_in(&model[0..mlen], sel) < mlen,
                            "the selection is not an app of the snapshot"
                        );
                        if op >= 2 {
                            let gone = ids[op - 2];
                            let still_there = index_in(&old[0..old_len], gone) >= old_len;
                            let selected_the_gone = before == Some(gone);
                            if still_there || !selected_the_gone {
                                assert_eq!(
                                    Some(sel),
                                    before,
                                    "the selection jumped without a reason"
                                );
                            } else {
                                assert_eq!(
                                    Some(sel),
                                    next_after(&old[0..old_len], gone),
                                    "the selection must land on the next app, wrapping"
                                );
                            }
                        }
                    }
                    i += 1;
                }
                if mlen > 0 {
                    let start = s.selected();
                    let mut k = 0;
                    while k < mlen {
                        s.step(false);
                        k += 1;
                    }
                    assert_eq!(s.selected(), start);
                }
                n += 1;
            }
            depth += 1;
        }
    }

    fn index_in(list: &[AppId], id: AppId) -> usize {
        let mut i = 0;
        while i < list.len() {
            if list[i] == id {
                return i;
            }
            i += 1;
        }
        list.len()
    }

    /// The app that comes right after `id` in `list`, wrapping around; None when `id` is the only app.
    fn next_after(list: &[AppId], id: AppId) -> Option<AppId> {
        let at = index_in(list, id);
        if at >= list.len() || list.len() < 2 {
            return None;
        }
        Some(list[(at + 1) % list.len()])
    }
}
