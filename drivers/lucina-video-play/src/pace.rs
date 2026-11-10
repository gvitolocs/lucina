//! When to show a frame and when to give up on one. Pure, so that what happens on a CPU that cannot keep up is tested on one that can.
//!
//! Numbers from the laptop (`docs/research/juno-media.md`): a video this CPU cannot decode in time must get slower to look at, not stop.

use std::time::Duration;

/// What to do next.
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum Step {
    /// The first frame in the queue goes on the screen.
    Show,
    /// The first frame's time has passed and another is waiting: throw it away.
    Drop,
    /// Nothing to do until then, or until a key.
    Wait(Duration),
    /// Too few pictures to show one: wait for the decoding thread.
    Starving,
    /// Far behind: go straight to the next key frame.
    SkipToKey,
    /// The file is over and everything has been shown.
    Finished,
}

pub struct State {
    /// Pictures decoded and not shown.
    pub queued: usize,
    /// Time of the first of them.
    pub first_pts: Option<i64>,
    /// Playback time; `None` before the first frame.
    pub now: Option<i64>,
    pub eof: bool,
    /// How many pictures must wait before the first may go: decode order and display order differ this far.
    pub reorder: usize,
    /// One frame's time, from the file.
    pub frame_us: i64,
    pub paused: bool,
    /// Since the last frame went on the screen.
    pub since_shown_us: i64,
}

/// Show when up to this many frames late, and otherwise throw away.
const LATE_DROP_FRAMES: i64 = 2;
/// However late things are, the picture changes at least this often: about 8 frames a second.
const MAX_STALE_US: i64 = 125_000;
/// Behind by this, nothing short of a jump will catch up.
const JUMP_AFTER_US: i64 = 2_000_000;
/// Behind by this, the decoding thread leaves out pictures nothing refers to.
pub const SKIP_AFTER_US: i64 = 150_000;
/// And back to all of them under this.
pub const SKIP_UNTIL_US: i64 = 40_000;

pub fn next(s: &State) -> Step {
    if s.paused && s.now.is_some() {
        return Step::Wait(Duration::from_millis(100));
    }
    if let (Some(p), Some(n)) = (s.first_pts, s.now)
        && n - p > JUMP_AFTER_US
        && !s.eof
    {
        return Step::SkipToKey;
    }
    if s.queued < s.reorder + 1 && !s.eof {
        return Step::Starving;
    }
    let Some(pts) = s.first_pts else {
        return if s.eof { Step::Finished } else { Step::Starving };
    };
    let Some(now) = s.now else {
        return Step::Show;
    };
    let early = pts - now;
    if early > 0 {
        return Step::Wait(Duration::from_micros(early as u64));
    }
    if -early > LATE_DROP_FRAMES * s.frame_us && s.queued > 1 && s.since_shown_us < MAX_STALE_US {
        return Step::Drop;
    }
    Step::Show
}

/// How far playback is behind what has been decoded, for telling the decoding thread to skip.
pub fn behind_us(first_pts: Option<i64>, now: Option<i64>, last_shown: Option<i64>, frame_us: i64) -> i64 {
    let Some(now) = now else { return 0 };
    // With nothing decoded yet, the frame after the last one shown is what is late.
    let head = first_pts.or(last_shown.map(|l| l + frame_us));
    head.map_or(0, |h| now - h)
}

#[cfg(test)]
mod tests {
    use super::*;

    const F: i64 = 41_666;

    fn s() -> State {
        State { queued: 0, first_pts: None, now: None, eof: false, reorder: 0, frame_us: F, paused: false, since_shown_us: 0 }
    }

    #[test]
    fn waits_for_pictures_then_shows_the_first_at_once() {
        assert_eq!(next(&s()), Step::Starving);
        assert_eq!(next(&State { queued: 1, first_pts: Some(5_000_000), ..s() }), Step::Show);
    }

    #[test]
    fn waits_for_reordering_before_showing_anything() {
        let st = State { queued: 2, first_pts: Some(0), reorder: 4, ..s() };
        assert_eq!(next(&st), Step::Starving);
        assert_eq!(next(&State { queued: 5, ..st }), Step::Show);
        // At the end of the file there is nothing to wait for.
        assert_eq!(next(&State { queued: 1, eof: true, ..st }), Step::Show);
    }

    #[test]
    fn an_early_frame_waits_until_its_time() {
        let st = State { queued: 3, first_pts: Some(F), now: Some(0), ..s() };
        assert_eq!(next(&st), Step::Wait(Duration::from_micros(F as u64)));
    }

    #[test]
    fn a_due_frame_shows_and_a_long_late_one_is_thrown_away() {
        let st = State { queued: 2, first_pts: Some(0), now: Some(F), ..s() };
        assert_eq!(next(&st), Step::Show);
        assert_eq!(next(&State { now: Some(3 * F), ..st }), Step::Drop);
        // But never the only frame there is.
        assert_eq!(next(&State { queued: 1, now: Some(3 * F), ..st }), Step::Show);
    }

    /// The bug the laptop found: with H.264's reorder queue always full and a CPU that cannot keep up, every frame counted as too late,
    /// and 15 s of 1080p showed 2 frames.
    #[test]
    fn however_late_it_gets_the_picture_keeps_changing() {
        let st = State { queued: 6, first_pts: Some(0), now: Some(5 * F), reorder: 4, ..s() };
        assert_eq!(next(&State { since_shown_us: 40_000, ..st }), Step::Drop);
        assert_eq!(next(&State { since_shown_us: MAX_STALE_US, ..st }), Step::Show);
    }

    /// The other one: VP9 with one frame queued was never late enough to do anything, and drifted 8 s behind.
    #[test]
    fn far_behind_it_jumps_with_whatever_is_queued() {
        let one = State { queued: 1, first_pts: Some(0), now: Some(3_000_000), ..s() };
        assert_eq!(next(&one), Step::SkipToKey);
        let many = State { queued: 6, reorder: 4, ..one };
        assert_eq!(next(&many), Step::SkipToKey);
        let starving = State { queued: 1, reorder: 4, ..one };
        assert_eq!(next(&starving), Step::SkipToKey);
        // Nothing to jump to at the end.
        assert_eq!(next(&State { eof: true, ..one }), Step::Show);
    }

    #[test]
    fn behind_is_measured_from_what_is_next() {
        assert_eq!(behind_us(Some(100), Some(300), Some(50), F), 200);
        assert_eq!(behind_us(None, Some(F * 3), Some(F), F), F);
        assert_eq!(behind_us(Some(100), None, None, F), 0);
        assert_eq!(behind_us(None, Some(100), None, F), 0);
    }

    #[test]
    fn a_pause_holds_everything_once_a_frame_is_up() {
        let st = State { queued: 1, first_pts: Some(0), now: Some(0), paused: true, ..s() };
        assert_eq!(next(&st), Step::Wait(Duration::from_millis(100)));
        // Paused right after a seek: the frame there still has to show.
        assert_eq!(next(&State { now: None, ..st }), Step::Show);
        // A long pause is not lateness.
        assert_eq!(next(&State { now: Some(10_000_000), ..st }), Step::Wait(Duration::from_millis(100)));
    }

    #[test]
    fn the_end() {
        assert_eq!(next(&State { eof: true, ..s() }), Step::Finished);
        assert_eq!(next(&State { eof: true, queued: 1, first_pts: Some(0), now: Some(0), ..s() }), Step::Show);
    }
}
