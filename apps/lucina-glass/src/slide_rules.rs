// The page slide's geometry (screen-forward and screen-back): which columns of a screen row come from the leaving page, which are
// black and which come from the arriving page, for a slide offset and a drift. The CPU compose (`home::slide_compose`) and the GPU
// compose (lucina-app `compose`) both take their spans from here, so the two backends show the same picture. Verus checks this very
// file, as it does src/home_rules.rs: `//@` lines hold the proof (scripts/verify-shipped.sh, D-011);
// verification/juno-perf-slide/mutants.sh plants bugs that Verus must refuse. The rule is total: it takes any width, offset and
// drift (an offset or drift beyond the width counts as the width), so a caller outside the proof cannot break a precondition.

/// The widest screen the rule takes, in pixels.
pub const MAX_W: u32 = 65_536;

/// One row of a slide frame, left to right in screen columns: `back_len` columns of the leaving page from its column `back_src`
/// starting at `back_dst`, `gap_len` black columns at `gap_dst`, and `front_len` columns of the arriving page from its column
/// `front_src` starting at `front_dst`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)] //@omit
pub struct Spans {
    pub back_src: u32,
    pub back_dst: u32,
    pub back_len: u32,
    pub gap_dst: u32,
    pub gap_len: u32,
    pub front_src: u32,
    pub front_dst: u32,
    pub front_len: u32,
}

//@ /// `x`, or the width when it is beyond it: what the rule makes of an offset or a drift.
//@ pub open spec fn capped(x: u32, w: u32) -> u32 { if x < w { x } else { w } }
//@
//@ /// What a row of a slide frame must be. Each line is one thing a caller relies on:
//@ /// - the three spans are `w` columns together, and none reads its page or writes the row beyond column `w`;
//@ /// - they lie in screen order with nothing between and nothing over another: back, gap, front (forward), front, gap, back (back);
//@ /// - the arriving page is `off` columns from its place, and is whole on that side: forward its column 0 is at screen column
//@ ///   `off`, back its last column is at screen column `w - off - 1` (it shows its columns from `off` on);
//@ /// - the leaving page is `drift` columns from its place: every column shown of it is `drift` to the left (forward) or to the
//@ ///   right (back) of where it was;
//@ /// - black is shown only where the leaving page has no column left to show: the gap, if any, starts at its last column
//@ ///   (forward) or ends at its first (back).
//@ pub open spec fn slide_ok(w: u32, off: u32, drift: u32, forward: bool, s: Spans) -> bool {
//@     &&& s.back_len + s.gap_len + s.front_len == w
//@     &&& s.back_src + s.back_len <= w
//@     &&& s.front_src + s.front_len <= w
//@     &&& s.back_dst + s.back_len <= w
//@     &&& s.gap_dst + s.gap_len <= w
//@     &&& s.front_dst + s.front_len <= w
//@     &&& forward ==> s.back_dst == 0 && s.gap_dst == s.back_len && s.front_dst == s.back_len + s.gap_len
//@     &&& !forward ==> s.front_dst == 0 && s.gap_dst == s.front_len && s.back_dst == s.front_len + s.gap_len
//@     &&& forward ==> s.front_src == 0 && s.front_dst == capped(off, w)
//@     &&& !forward ==> s.front_src == capped(off, w) && s.front_src + s.front_len == w
//@     &&& forward ==> s.back_src == s.back_dst + capped(drift, w)
//@     &&& !forward ==> s.back_dst == s.back_src + capped(drift, w)
//@     &&& (forward && s.gap_len > 0) ==> s.back_src + s.back_len == w
//@     &&& (!forward && s.gap_len > 0) ==> s.back_src == 0
//@ }
//@
//@ /// The statement leaves no freedom: two rows that both satisfy it for the same arguments are the same row. So it says all
//@ /// there is to say about the result, and any change of the code that changes a result breaks it.
//@ pub proof fn slide_ok_is_one_row(w: u32, off: u32, drift: u32, forward: bool, a: Spans, b: Spans)
//@     requires slide_ok(w, off, drift, forward, a), slide_ok(w, off, drift, forward, b),
//@     ensures a == b,
//@ {
//@ }
//@
/// The spans of a `w` pixels wide row when the arriving page's edge has moved `off` columns in and the leaving page has drifted
/// `drift` columns. `forward` (screen-forward): the new page comes from the right over the old one, which drifts left. Otherwise
/// (screen-back) the new page is on top and slides in from the left, its right edge at `w - off`, read from its column `off`;
/// the old one drifts right by `drift`, dimmed, to the right of it, and black fills between them once the old page has run out.
/// The three spans cover the row exactly once, each in its order, and no span reads outside its page: the spans tile the screen.
pub fn slide_spans(w: u32, off: u32, drift: u32, forward: bool) -> Spans //@ret
//@ ensures slide_ok(w, off, drift, forward, ret),
{
    let off = if off < w { off } else { w };
    let drift = if drift < w { drift } else { w };
    if forward {
        // The front page's left edge is at `off`; the back page has moved `drift` to the left.
        let edge = off;
        let shown = if edge < w - drift { edge } else { w - drift };
        Spans {
            back_src: drift,
            back_dst: 0,
            back_len: shown,
            gap_dst: shown,
            gap_len: edge - shown,
            front_src: 0,
            front_dst: edge,
            front_len: w - edge,
        }
    } else {
        let edge = w - off;
        let start = if edge > drift { edge } else { drift };
        Spans {
            back_src: start - drift,
            back_dst: start,
            back_len: w - start,
            gap_dst: edge,
            gap_len: start - edge,
            front_src: off,
            front_dst: 0,
            front_len: edge,
        }
    }
}
