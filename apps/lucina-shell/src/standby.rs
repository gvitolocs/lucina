//! Idle standby in the shell (`docs/research/juno-power.md`, "Idle standby"): the screen goes off after the idle minutes, and the
//! machine sleeps the same way as for the lid a minute later. The decisions are `lucina_standby`'s, proved by Verus; this module
//! only reads what holds the sleep off besides apps and inhibitors: audio playback.
//!
//! The original for audio is ChromeOS's power manager: "audio activity (currently defined as active output streams)" lets the
//! screen dim and go off "but the system will not suspend" (`power_manager/docs/inactivity_delays.md`). Windows' Modern Standby
//! likewise lets audio run on with the screen off. Here the audio driver's status file says whether a stream runs; the
//! microphone counts too (adapt: a recording with the screen off must not be cut by a sleep; ChromeOS names output only).

use std::fs;
use std::io::Write;
use std::process::{Command, Stdio};
use std::time::Duration;

/// Present while the standby has Wi-Fi down on purpose: lucina-watchd then neither retries Wi-Fi nor counts the network as dead.
pub const MARK: &str = "/run/lucina/standby";
/// The Wi-Fi driver's control file (`disconnect`), and the command lucina-watchd brings Wi-Fi back with.
const WIFI: &str = "/dev/lucina-wifi";
const WIFI_COMMAND: &str = "/sbin/lucina-wifi";

/// Into standby: what can go down at runtime and come back without a restart. Wi-Fi leaves its network; the modem it runs on
/// stays up (this kernel cannot stop and start it again), and so does the Ethernet adapter. The GPU, the keyboard and the
/// touchpad are down already from the sleep. Says what was done, for the log.
pub fn enter() -> String {
    if let Err(e) = fs::write(MARK, b"standby\n") {
        return format!("Wi-Fi left as it was: {MARK}: {e}");
    }
    match fs::OpenOptions::new()
        .write(true)
        .open(WIFI)
        .and_then(|mut f| f.write_all(b"disconnect\n"))
    {
        Ok(()) => "Wi-Fi disconnected".to_string(),
        Err(e) => format!("Wi-Fi left as it was: {WIFI}: {e}"),
    }
}

/// Out of standby: Wi-Fi to the saved network again, as lucina-watchd does it, then the mark goes. The command runs on its own
/// (a thread waits for it), so the wake does not wait for the association.
pub fn leave() -> String {
    let started = Command::new(WIFI_COMMAND)
        .args(["connect", "--saved"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn();
    let _ = fs::remove_file(MARK);
    match started {
        Ok(mut child) => {
            std::thread::spawn(move || child.wait());
            "Wi-Fi connecting to the saved network".to_string()
        }
        Err(e) => format!("Wi-Fi not brought back: {WIFI_COMMAND}: {e}"),
    }
}

/// A mark left behind (the shell ended while in standby): Wi-Fi comes back as at a wake.
pub fn leave_if_left_behind() -> Option<String> {
    fs::metadata(MARK).is_ok().then(leave)
}

/// The audio driver's status: one `key value` per line (kernel patch 0850, `/dev/lucina-audio-ctl`).
pub const AUDIO_CTL: &str = "/dev/lucina-audio-ctl";

/// While audio holds the sleep off, how often the status is read again to see whether it stopped. ChromeOS hears of streams from
/// its audio server at once; here the end of playback is noticed within this, and the sleep then counts its whole delay again
/// from that moment, so this only delays the sleep, never brings it sooner.
pub const AUDIO_RECHECK: Duration = Duration::from_secs(10);

/// Whether the status says a stream or the microphone runs: a `stream` or `mic` line whose state is not `idle`.
pub fn audio_busy(status: &str) -> bool {
    status.lines().any(|line| {
        let mut words = line.split_whitespace();
        matches!(words.next(), Some("stream" | "mic"))
            && words.next().is_some_and(|state| state != "idle")
    })
}

/// The status read now; a driver that is not there plays nothing.
pub fn read_audio_busy() -> bool {
    std::fs::read_to_string(AUDIO_CTL).is_ok_and(|s| audio_busy(&s))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn idle_audio_holds_nothing() {
        let laptop = "state ready\nwhy \nstream idle\nvolume -30\nceiling -12\nplayed_frames 0\nqueued_bytes 0\nmic idle\nmic_bytes 0\n";
        assert!(!audio_busy(laptop));
        assert!(!audio_busy(""));
        // Before patch 0894 there is no mic line.
        assert!(!audio_busy("state ready\nstream idle\n"));
    }

    /// The proved rule turns the screen off when the shell's earlier timer did, for every input time, hold end, hold and
    /// choice of minutes: moving to `lucina_standby` changed when the screen goes off in no case.
    #[test]
    fn the_screen_goes_off_as_it_did_before() {
        use lucina_standby::{Kind, Screen};
        for minutes in crate::backlight::SCREEN_OFF_CHOICES {
            for (input, ended) in [
                (0, 0),
                (5, 0),
                (0, 7),
                (90_000_000, 30_000_000),
                (30_000_000, 90_000_000),
            ] {
                for held in [false, true] {
                    let mut s = lucina_standby::start(0);
                    s = lucina_standby::input(s, input);
                    s = lucina_standby::give(
                        lucina_standby::take(s, Kind::Display),
                        Kind::Display,
                        ended,
                    );
                    if held {
                        s = lucina_standby::take(s, Kind::Display);
                    }
                    s.screen = Screen::Lit;
                    let before = crate::backlight::screen_off_at(input, ended, held, minutes);
                    // The screen-off alone (no sleep, no standby), as the earlier timer knew it.
                    let t = lucina_standby::Times {
                        off: minutes,
                        sleep: 0,
                        standby: 0,
                    };
                    assert_eq!(
                        lucina_standby::deadline(s, t),
                        before,
                        "{minutes} min, {input}, {ended}, {held}"
                    );
                }
            }
        }
    }

    #[test]
    fn a_stream_or_the_microphone_holds_the_sleep() {
        assert!(audio_busy("state ready\nstream running\nmic idle\n"));
        assert!(audio_busy("state ready\nstream idle\nmic capturing\n"));
        // Keys that only start with the words do not count.
        assert!(!audio_busy("streams running\nmic_bytes 10\n"));
    }
}
