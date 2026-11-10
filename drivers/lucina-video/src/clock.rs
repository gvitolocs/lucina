//! What frames are shown against. With sound, the position the sound device has reached; without it, the wall clock.
//!
//! What the player needs from sound is [`AudioOutput`], nothing else: it says where to play from and asks where playback has got to, so
//! video follows sound the way every player does. The implementation reads the file's sound for itself (`lucina-app-video/src/sound.rs`).

use std::time::Instant;

/// Sound for a player. The implementation decodes on a thread of its own and owns the device.
pub trait AudioOutput {
    /// The presentation time of the sample at the speaker now, in microseconds. `None` until the first one has played, and when there
    /// is no sound: the clock then runs from the first frame.
    fn position_us(&self) -> Option<i64>;
    /// Play from `pts_us`: the first frame from there is on screen.
    fn start(&mut self, pts_us: i64);
    fn set_paused(&mut self, paused: bool);
    /// Stop: a seek has begun, and [`Self::start`] follows when a frame from the new place is up.
    fn seek(&mut self, pts_us: i64);
}

/// No sound: a video with none, or a test.
#[derive(Default)]
pub struct Silent;

impl AudioOutput for Silent {
    fn position_us(&self) -> Option<i64> {
        None
    }
    fn start(&mut self, _pts_us: i64) {}
    fn set_paused(&mut self, _paused: bool) {}
    fn seek(&mut self, _pts_us: i64) {}
}

/// Where playback is. Follows the sound when there is sound, and the wall clock from the last anchor otherwise.
pub struct Clock<A> {
    pub audio: A,
    anchor: Option<(Instant, i64)>,
    paused_at: Option<i64>,
}

impl<A: AudioOutput> Clock<A> {
    pub fn new(audio: A) -> Clock<A> {
        Clock { audio, anchor: None, paused_at: None }
    }

    /// Playback time now, in microseconds.
    pub fn now_us(&self, now: Instant) -> Option<i64> {
        if let Some(p) = self.paused_at {
            return Some(p);
        }
        if let Some(p) = self.audio.position_us() {
            return Some(p);
        }
        self.anchor.map(|(t, pts)| pts + now.saturating_duration_since(t).as_micros() as i64)
    }

    /// Whether time comes from the sound now, rather than the wall clock.
    pub fn follows_sound(&self) -> bool {
        self.audio.position_us().is_some()
    }

    /// The first frame after a start or a seek is on screen: time runs from it.
    pub fn start_at(&mut self, pts_us: i64, now: Instant) {
        self.anchor = Some((now, pts_us));
        if self.paused_at.is_some() {
            self.paused_at = Some(pts_us);
        }
        self.audio.start(pts_us);
    }

    pub fn is_started(&self) -> bool {
        self.anchor.is_some()
    }

    /// Before a seek: nothing runs until the next [`Clock::start_at`].
    pub fn stop(&mut self, pts_us: i64) {
        self.anchor = None;
        self.audio.seek(pts_us);
    }

    pub fn set_paused(&mut self, paused: bool, now: Instant) {
        match (paused, self.paused_at) {
            (true, None) => self.paused_at = Some(self.now_us(now).unwrap_or(0)),
            (false, Some(p)) => {
                self.paused_at = None;
                if self.anchor.is_some() {
                    self.anchor = Some((now, p));
                }
            }
            _ => return,
        }
        self.audio.set_paused(paused);
    }

    pub fn is_paused(&self) -> bool {
        self.paused_at.is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn silent_time_runs_from_the_first_frame_and_stops_for_a_pause() {
        let mut c = Clock::new(Silent);
        let t0 = Instant::now();
        assert_eq!(c.now_us(t0), None);
        c.start_at(5_000_000, t0);
        assert_eq!(c.now_us(t0 + Duration::from_secs(1)), Some(6_000_000));
        c.set_paused(true, t0 + Duration::from_secs(1));
        assert_eq!(c.now_us(t0 + Duration::from_secs(9)), Some(6_000_000));
        c.set_paused(false, t0 + Duration::from_secs(9));
        assert_eq!(c.now_us(t0 + Duration::from_secs(10)), Some(7_000_000));
    }

    #[test]
    fn a_seek_stops_time_until_a_frame_is_up() {
        let mut c = Clock::new(Silent);
        let t0 = Instant::now();
        c.start_at(0, t0);
        c.stop(60_000_000);
        assert!(!c.is_started());
        assert_eq!(c.now_us(t0), None);
        c.start_at(60_000_000, t0);
        assert_eq!(c.now_us(t0), Some(60_000_000));
    }

    struct Fake(Option<i64>);
    impl AudioOutput for Fake {
        fn position_us(&self) -> Option<i64> {
            self.0
        }
        fn start(&mut self, _: i64) {}
        fn set_paused(&mut self, _: bool) {}
        fn seek(&mut self, _: i64) {}
    }

    #[test]
    fn sound_leads_as_soon_as_it_plays() {
        let t0 = Instant::now();
        let mut c = Clock::new(Fake(None));
        c.start_at(0, t0);
        assert_eq!(c.now_us(t0 + Duration::from_secs(2)), Some(2_000_000));
        c.audio.0 = Some(1_500_000);
        assert_eq!(c.now_us(t0 + Duration::from_secs(2)), Some(1_500_000));
    }
}
