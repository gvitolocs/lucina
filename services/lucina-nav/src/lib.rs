// SPDX-License-Identifier: MPL-2.0

//! Which row, tile or file a key moves to (owner, 2026-11-08: "in the file manager the arrows don't navigate within files,
//! especially images and videos"). This file is pure: a selection and a length in, the new selection out. The apps (Drive,
//! the file picker, Pictures, Video, Gallery) call it, and Verus proves this very file (`verification/juno-files-keys/rules.rs`,
//! D-011).
//!
//! **No wrapping.** A list stops at its ends: Next on the last item stays on it, Previous on the first stays on it. That is what
//! Windows Photos and Explorer do (a folder of pictures ends at the last one) and what a list selection does in Explorer, Finder
//! and every toolkit list; macOS Quick Look and Finder icon view stop at the ends too. Wrapping would hide where the folder ends.
//!
//! A selection is `Option<usize>`: `None` is nothing selected, which is the only possible state of an empty list. Every
//! function returns a selection inside `0..len`, whatever it was given (a stale index from before the list shrank comes back
//! to the last item).
#![allow(unexpected_cfgs)]

#[cfg(verus_keep_ghost)]
use vstd::prelude::*;

#[cfg(verus_keep_ghost)]
verus! {

/// A selection that points at a real row of a list of `len` rows, or at nothing.
pub open spec fn sel_ok(s: Option<usize>, len: usize) -> bool {
    match s {
        Some(i) => i < len,
        None => true,
    }
}

/// The selected row as a number, or -1 for nothing: to say which way a move went.
pub open spec fn at(s: Option<usize>) -> int {
    match s {
        Some(i) => i as int,
        None => -1,
    }
}

/// The first row of the last grid row: the rows of `cols` items each end at `len - 1`.
pub open spec fn last_row_start(len: usize, cols: usize) -> int
    recommends
        len > 0,
        cols > 0,
{
    (len as int - 1) - (len as int - 1) % (cols as int)
}

} // verus!

/// The selection, brought inside the list: a row past the end becomes the last one, and an empty list has none.
#[cfg_attr(verus_keep_ghost, verus_spec(r =>
    ensures
        sel_ok(r, len),
        len == 0 ==> r == None::<usize>,
        len > 0 && cur is Some && cur.unwrap() < len ==> r == cur,
))]
pub fn clamp(cur: Option<usize>, len: usize) -> Option<usize> {
    if len == 0 {
        return None;
    }
    match cur {
        Some(i) if i < len => Some(i),
        Some(_) => Some(len - 1),
        None => None,
    }
}

/// First row, or nothing in an empty list. (Home.)
#[cfg_attr(verus_keep_ghost, verus_spec(r =>
    ensures
        sel_ok(r, len),
        len == 0 ==> r == None::<usize>,
        len > 0 ==> r == Some(0usize),
))]
pub fn first(len: usize) -> Option<usize> {
    if len == 0 { None } else { Some(0) }
}

/// Last row, or nothing in an empty list. (End.)
#[cfg_attr(verus_keep_ghost, verus_spec(r =>
    ensures
        sel_ok(r, len),
        len == 0 ==> r == None::<usize>,
        len > 0 ==> r == Some((len - 1) as usize),
))]
pub fn last(len: usize) -> Option<usize> {
    if len == 0 { None } else { Some(len - 1) }
}

/// One row down (Down, Right, Next). With nothing selected it selects the first row. On the last row it stays: no wrap.
#[cfg_attr(verus_keep_ghost, verus_spec(r =>
    ensures
        sel_ok(r, len),
        len == 0 ==> r == None::<usize>,
        len > 0 ==> r is Some,
        len > 0 && cur is None ==> r == Some(0usize),
        len > 0 && cur is Some && cur.unwrap() < len ==> at(r) >= at(cur) && at(r) <= at(cur) + 1,
        len > 0 && cur is Some && cur.unwrap() + 1 < len ==> at(r) == at(cur) + 1,
        len > 0 && cur is Some && cur.unwrap() + 1 == len ==> r == cur,
))]
pub fn next(cur: Option<usize>, len: usize) -> Option<usize> {
    if len == 0 {
        return None;
    }
    match cur {
        None => Some(0),
        Some(i) if i < len - 1 => Some(i + 1),
        Some(i) if i < len => Some(i),
        Some(_) => Some(len - 1),
    }
}

/// One row up (Up, Left, Previous). With nothing selected it selects the last row, as Finder does. On the first row it stays:
/// no wrap.
#[cfg_attr(verus_keep_ghost, verus_spec(r =>
    ensures
        sel_ok(r, len),
        len == 0 ==> r == None::<usize>,
        len > 0 ==> r is Some,
        len > 0 && cur is None ==> r == Some((len - 1) as usize),
        len > 0 && cur is Some && cur.unwrap() < len ==> at(r) <= at(cur) && at(r) >= at(cur) - 1,
        len > 0 && cur is Some && cur.unwrap() > 0 && cur.unwrap() < len ==> at(r) == at(cur) - 1,
        len > 0 && cur is Some && cur.unwrap() == 0 ==> r == cur,
))]
pub fn prev(cur: Option<usize>, len: usize) -> Option<usize> {
    if len == 0 {
        return None;
    }
    match cur {
        None => Some(len - 1),
        Some(i) if i == 0 => Some(0),
        Some(i) if i < len => Some(i - 1),
        Some(_) => Some(len - 1),
    }
}

/// A screenful down (Page Down): `page` rows, at least one, and no further than the last row. With nothing selected it goes
/// `page` rows from the top.
#[cfg_attr(verus_keep_ghost, verus_spec(r =>
    ensures
        sel_ok(r, len),
        len == 0 ==> r == None::<usize>,
        len > 0 ==> r is Some,
        len > 0 && cur is Some && cur.unwrap() < len ==> at(r) >= at(cur),
        len > 0 && cur is Some && cur.unwrap() < len ==> at(r) <= at(cur) + (if page == 0 { 1int } else { page as int }),
        len > 0 && cur is Some && cur.unwrap() < len && at(cur) + (if page == 0 { 1int } else { page as int }) < len as int
            ==> at(r) == at(cur) + (if page == 0 { 1int } else { page as int }),
        len > 0 && cur is Some && cur.unwrap() < len && at(cur) + (if page == 0 { 1int } else { page as int }) >= len as int
            ==> at(r) == len as int - 1,
))]
pub fn page_down(cur: Option<usize>, len: usize, page: usize) -> Option<usize> {
    if len == 0 {
        return None;
    }
    let page = if page == 0 { 1 } else { page };
    let from = match cur {
        Some(i) if i < len => i,
        Some(_) => len - 1,
        None => 0,
    };
    if page < len - from { Some(from + page) } else { Some(len - 1) }
}

/// A screenful up (Page Up): `page` rows, at least one, and no further than the first row.
#[cfg_attr(verus_keep_ghost, verus_spec(r =>
    ensures
        sel_ok(r, len),
        len == 0 ==> r == None::<usize>,
        len > 0 ==> r is Some,
        len > 0 && cur is Some && cur.unwrap() < len ==> at(r) <= at(cur),
        len > 0 && cur is Some && cur.unwrap() < len ==> at(r) >= at(cur) - (if page == 0 { 1int } else { page as int }),
        len > 0 && cur is Some && cur.unwrap() < len && at(cur) >= (if page == 0 { 1int } else { page as int })
            ==> at(r) == at(cur) - (if page == 0 { 1int } else { page as int }),
        len > 0 && cur is Some && cur.unwrap() < len && at(cur) < (if page == 0 { 1int } else { page as int })
            ==> at(r) == 0,
))]
pub fn page_up(cur: Option<usize>, len: usize, page: usize) -> Option<usize> {
    if len == 0 {
        return None;
    }
    let page = if page == 0 { 1 } else { page };
    let from = match cur {
        Some(i) if i < len => i,
        Some(_) => len - 1,
        None => 0,
    };
    if page <= from { Some(from - page) } else { Some(0) }
}

/// One grid row down (Down in a grid of `cols` tiles per row). A whole row down is `cols` tiles on. From a row above a short
/// last row it goes to the last tile, as Explorer does; on the last row it stays. With nothing selected it selects the first.
#[cfg_attr(verus_keep_ghost, verus_spec(r =>
    ensures
        sel_ok(r, len),
        len == 0 ==> r == None::<usize>,
        len > 0 ==> r is Some,
        len > 0 && cur is None ==> r == Some(0usize),
        len > 0 && cur is Some && cur.unwrap() < len ==> at(r) >= at(cur),
        len > 0 && cur is Some && cur.unwrap() < len ==> at(r) <= at(cur) + (if cols == 0 { 1int } else { cols as int }),
        len > 0 && cur is Some && cur.unwrap() < len && at(cur) + (if cols == 0 { 1int } else { cols as int }) < len as int
            ==> at(r) == at(cur) + (if cols == 0 { 1int } else { cols as int }),
        len > 0 && cur is Some && cur.unwrap() < len && at(cur) < last_row_start(len, if cols == 0 { 1usize } else { cols })
            ==> at(r) > at(cur),
        len > 0 && cur is Some && cur.unwrap() < len && at(cur) >= last_row_start(len, if cols == 0 { 1usize } else { cols })
            ==> r == cur,
))]
pub fn row_down(cur: Option<usize>, len: usize, cols: usize) -> Option<usize> {
    if len == 0 {
        return None;
    }
    let cols = if cols == 0 { 1 } else { cols };
    let i = match cur {
        Some(i) if i < len => i,
        Some(_) => len - 1,
        None => return Some(0),
    };
    if cols < len - i {
        Some(i + cols)
    } else if (len - 1) % cols < (len - 1) - i {
        Some(len - 1)
    } else {
        Some(i)
    }
}

/// One grid row up. On the first row it stays. With nothing selected it selects the last tile.
#[cfg_attr(verus_keep_ghost, verus_spec(r =>
    ensures
        sel_ok(r, len),
        len == 0 ==> r == None::<usize>,
        len > 0 ==> r is Some,
        len > 0 && cur is None ==> r == Some((len - 1) as usize),
        len > 0 && cur is Some && cur.unwrap() < len ==> at(r) <= at(cur),
        len > 0 && cur is Some && cur.unwrap() < len ==> at(r) >= at(cur) - (if cols == 0 { 1int } else { cols as int }),
        len > 0 && cur is Some && cur.unwrap() < len && at(cur) >= (if cols == 0 { 1int } else { cols as int })
            ==> at(r) == at(cur) - (if cols == 0 { 1int } else { cols as int }),
        len > 0 && cur is Some && cur.unwrap() < len && at(cur) < (if cols == 0 { 1int } else { cols as int })
            ==> r == cur,
))]
pub fn row_up(cur: Option<usize>, len: usize, cols: usize) -> Option<usize> {
    if len == 0 {
        return None;
    }
    let cols = if cols == 0 { 1 } else { cols };
    let i = match cur {
        Some(i) if i < len => i,
        Some(_) => len - 1,
        None => return Some(len - 1),
    };
    if cols <= i { Some(i - cols) } else { Some(i) }
}

/// The next file of the kind the viewer shows: `ok[k]` says whether file `k` is one. The first `j` after `from` with `ok[j]`,
/// or nothing when there is none (the viewer stays where it is: no wrap).
#[cfg_attr(verus_keep_ghost, verus_spec(r =>
    ensures
        match r {
            Some(j) => from < j && j < ok.len() && ok[j as int] && forall|k: int| from < k && k < j ==> !ok[k],
            None => forall|k: int| from < k && k < ok.len() ==> !ok[k],
        },
))]
pub fn next_match(ok: &[bool], from: usize) -> Option<usize> {
    let n = ok.len();
    if from >= n {
        return None;
    }
    let mut k = from + 1;
    #[cfg_attr(verus_keep_ghost, verus_spec(
        invariant
            from < k && k <= n,
            n == ok.len(),
            forall|m: int| from < m && m < k ==> !ok[m],
        decreases n - k,
    ))]
    while k < n {
        if ok[k] {
            return Some(k);
        }
        k += 1;
    }
    None
}

/// The previous file of the kind: the last `j` before `from` with `ok[j]`, or nothing.
#[cfg_attr(verus_keep_ghost, verus_spec(r =>
    ensures
        match r {
            Some(j) => j < from && j < ok.len() && ok[j as int] && forall|k: int| j < k && k < from && k < ok.len() ==> !ok[k],
            None => forall|k: int| 0 <= k && k < from && k < ok.len() ==> !ok[k],
        },
))]
pub fn prev_match(ok: &[bool], from: usize) -> Option<usize> {
    let n = ok.len();
    let mut k = if from < n { from } else { n };
    #[cfg_attr(verus_keep_ghost, verus_spec(
        invariant
            k <= n,
            k <= from,
            n == ok.len(),
            forall|m: int| k <= m && m < from && m < n ==> !ok[m],
        decreases k,
    ))]
    while k > 0 {
        k -= 1;
        if ok[k] {
            return Some(k);
        }
    }
    None
}

/// The first file of the kind. (Home in a viewer.)
#[cfg_attr(verus_keep_ghost, verus_spec(r =>
    ensures
        match r {
            Some(j) => j < ok.len() && ok[j as int] && forall|k: int| 0 <= k && k < j ==> !ok[k],
            None => forall|k: int| 0 <= k && k < ok.len() ==> !ok[k],
        },
))]
pub fn first_match(ok: &[bool]) -> Option<usize> {
    let n = ok.len();
    let mut k = 0;
    #[cfg_attr(verus_keep_ghost, verus_spec(
        invariant
            k <= n,
            n == ok.len(),
            forall|m: int| 0 <= m && m < k ==> !ok[m],
        decreases n - k,
    ))]
    while k < n {
        if ok[k] {
            return Some(k);
        }
        k += 1;
    }
    None
}

/// The last file of the kind. (End in a viewer.)
#[cfg_attr(verus_keep_ghost, verus_spec(r =>
    ensures
        match r {
            Some(j) => j < ok.len() && ok[j as int] && forall|k: int| j < k && k < ok.len() ==> !ok[k],
            None => forall|k: int| 0 <= k && k < ok.len() ==> !ok[k],
        },
))]
pub fn last_match(ok: &[bool]) -> Option<usize> {
    let n = ok.len();
    let mut k = n;
    #[cfg_attr(verus_keep_ghost, verus_spec(
        invariant
            k <= n,
            n == ok.len(),
            forall|m: int| k <= m && m < n ==> !ok[m],
        decreases k,
    ))]
    while k > 0 {
        k -= 1;
        if ok[k] {
            return Some(k);
        }
    }
    None
}

/// Where a viewer on file `at` goes for a key: a step, or the first or last file of its kinds. It stays on `at` when there is
/// nowhere to go. The kinds of file are the viewer's own: `ok[k]` for those it can show.
#[cfg_attr(verus_keep_ghost, verus_verify)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Step {
    Next,
    Prev,
    First,
    Last,
}

/// The file a viewer shows after `step`, from file `at` of `ok.len()`. Never leaves `0..ok.len()` when it starts inside, and
/// never goes against the step's direction.
#[cfg_attr(verus_keep_ghost, verus_spec(r =>
    requires
        at < ok.len(),
    ensures
        r < ok.len(),
        step == Step::Next ==> r >= at,
        step == Step::Prev ==> r <= at,
        step == Step::Next && r != at ==> ok[r as int],
        step == Step::Prev && r != at ==> ok[r as int],
        step == Step::First && r != at ==> ok[r as int] && r < at,
        step == Step::Last && r != at ==> ok[r as int] && r > at,
        step == Step::First ==> r <= at,
        step == Step::Last ==> r >= at,
))]
pub fn walk(ok: &[bool], at: usize, step: Step) -> usize {
    let to = match step {
        Step::Next => next_match(ok, at),
        Step::Prev => prev_match(ok, at),
        Step::First => first_match(ok),
        Step::Last => last_match(ok),
    };
    match to {
        Some(j) => match step {
            Step::First => if j < at { j } else { at },
            Step::Last => if j > at { j } else { at },
            _ => j,
        },
        None => at,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_empty_list_has_no_selection() {
        for cur in [None, Some(0), Some(7)] {
            assert_eq!(next(cur, 0), None);
            assert_eq!(prev(cur, 0), None);
            assert_eq!(clamp(cur, 0), None);
            assert_eq!(page_down(cur, 0, 5), None);
            assert_eq!(page_up(cur, 0, 5), None);
            assert_eq!(row_down(cur, 0, 4), None);
            assert_eq!(row_up(cur, 0, 4), None);
        }
        assert_eq!((first(0), last(0)), (None, None));
    }

    #[test]
    fn the_ends_stop_and_do_not_wrap() {
        assert_eq!(next(Some(2), 3), Some(2));
        assert_eq!(prev(Some(0), 3), Some(0));
        assert_eq!(next(Some(0), 3), Some(1));
        assert_eq!(prev(Some(2), 3), Some(1));
        assert_eq!(next(None, 3), Some(0));
        assert_eq!(prev(None, 3), Some(2));
        assert_eq!((first(3), last(3)), (Some(0), Some(2)));
        // A stale index comes back inside.
        assert_eq!(next(Some(9), 3), Some(2));
        assert_eq!(prev(Some(9), 3), Some(2));
    }

    #[test]
    fn pages_stop_at_the_ends() {
        assert_eq!(page_down(Some(2), 100, 10), Some(12));
        assert_eq!(page_down(Some(95), 100, 10), Some(99));
        assert_eq!(page_down(Some(99), 100, 10), Some(99));
        assert_eq!(page_up(Some(12), 100, 10), Some(2));
        assert_eq!(page_up(Some(3), 100, 10), Some(0));
        assert_eq!(page_up(Some(0), 100, 10), Some(0));
        assert_eq!(page_down(None, 100, 10), Some(10));
        assert_eq!(page_down(Some(0), 100, 0), Some(1));
    }

    #[test]
    fn grid_rows_move_by_the_columns_and_the_last_row_may_be_short() {
        // 10 tiles, 4 per row: rows 0-3, 4-7, 8-9.
        assert_eq!(row_down(Some(1), 10, 4), Some(5));
        assert_eq!(row_down(Some(5), 10, 4), Some(9));
        assert_eq!(row_down(Some(6), 10, 4), Some(10 - 1));
        assert_eq!(row_down(Some(8), 10, 4), Some(8));
        assert_eq!(row_down(Some(9), 10, 4), Some(9));
        assert_eq!(row_up(Some(9), 10, 4), Some(5));
        assert_eq!(row_up(Some(2), 10, 4), Some(2));
        assert_eq!(row_up(Some(4), 10, 4), Some(0));
        // One column is a list; zero columns count as one.
        assert_eq!(row_down(Some(1), 5, 0), Some(2));
        assert_eq!(row_up(Some(1), 5, 1), Some(0));
        // A full last row stays.
        assert_eq!(row_down(Some(4), 8, 4), Some(4));
    }

    #[test]
    fn a_viewer_steps_over_its_own_kinds_only() {
        // pic, text, pic, video, text, pic
        let pic = [true, false, true, false, false, true];
        assert_eq!(walk(&pic, 0, Step::Next), 2);
        assert_eq!(walk(&pic, 2, Step::Next), 5);
        assert_eq!(walk(&pic, 5, Step::Next), 5);
        assert_eq!(walk(&pic, 5, Step::Prev), 2);
        assert_eq!(walk(&pic, 0, Step::Prev), 0);
        assert_eq!(walk(&pic, 2, Step::First), 0);
        assert_eq!(walk(&pic, 2, Step::Last), 5);
        assert_eq!(walk(&pic, 0, Step::First), 0);
        assert_eq!(walk(&pic, 5, Step::Last), 5);
        // On a file that is not of the kind (a video in a picture viewer): steps still work from there.
        assert_eq!(walk(&pic, 3, Step::Next), 5);
        assert_eq!(walk(&pic, 3, Step::Prev), 2);
        // Nothing of the kind: stay.
        assert_eq!(walk(&[false, false], 1, Step::Next), 1);
        assert_eq!(walk(&[false, false], 1, Step::First), 1);
    }

    #[test]
    fn matches_agree_with_a_plain_scan() {
        let ok = [false, true, false, true, true, false];
        assert_eq!(next_match(&ok, 0), Some(1));
        assert_eq!(next_match(&ok, 4), None);
        assert_eq!(next_match(&ok, 99), None);
        assert_eq!(prev_match(&ok, 4), Some(3));
        assert_eq!(prev_match(&ok, 1), None);
        assert_eq!(prev_match(&ok, 99), Some(4));
        assert_eq!((first_match(&ok), last_match(&ok)), (Some(1), Some(4)));
        assert_eq!((first_match(&[]), last_match(&[])), (None, None));
    }
}
