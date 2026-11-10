//! The system volume the volume keys move: a level of `fnkeys` (0..=VOLUME_MAX) and mute, kept by the kernel for the boot
//! (`/dev/lucina-audio-ctl`: `system <dB>`, `mute 0|1`, patch 1168) and added to every program's own volume there, under
//! the kernel's ceiling. Without that device (no sound, or an older kernel) the keys only show the level.

use std::fs::{self, OpenOptions};
use std::io::Write;

use crate::fnkeys::{self, VOLUME_MAX, VOLUME_STEP_DB};

const CONTROL: &str = "/dev/lucina-audio-ctl";

pub struct Volume {
    pub level: u8,
    pub muted: bool,
}

impl Volume {
    /// What the kernel holds now: a shell that restarts shows the level that is in force.
    pub fn read() -> Volume {
        let status = fs::read_to_string(CONTROL).unwrap_or_default();
        let value = |key: &str| {
            status.lines().find_map(|l| {
                l.strip_prefix(key)?
                    .strip_prefix(' ')?
                    .trim()
                    .parse::<i32>()
                    .ok()
            })
        };
        let db = value("system_volume").unwrap_or(0);
        Volume {
            level: level_of(db),
            muted: value("muted") == Some(1),
        }
    }

    /// One volume key. Unmutes, as desktops do on a volume key.
    pub fn step(&mut self, up: bool) {
        self.level = fnkeys::volume_step(self.level, up);
        self.muted = false;
        self.send();
    }

    /// A level chosen on the screen (the dashboard's volume line). Unmutes.
    pub fn set_level(&mut self, level: u8) {
        self.level = level.min(VOLUME_MAX);
        self.muted = false;
        self.send();
    }

    pub fn toggle_mute(&mut self) {
        self.muted = !self.muted;
        self.send();
    }

    /// The level shown: 0 while muted.
    pub fn shown(&self) -> u8 {
        if self.muted { 0 } else { self.level }
    }

    fn send(&self) {
        let silent = self.muted || self.level == 0;
        let lines = [
            format!("system {}\n", fnkeys::volume_db(self.level)),
            format!("mute {}\n", u8::from(silent)),
        ];
        for line in lines {
            let sent = OpenOptions::new()
                .write(true)
                .open(CONTROL)
                .and_then(|mut f| f.write_all(line.as_bytes()));
            if let Err(e) = sent {
                eprintln!("lucina-shell: volume: {}: {}", line.trim(), e);
                return;
            }
        }
    }
}

/// The level for a system volume in dB, as `fnkeys::volume_db` gives them: the nearest level at or below.
fn level_of(db: i32) -> u8 {
    let below = (-db).max(0) / VOLUME_STEP_DB;
    VOLUME_MAX.saturating_sub(below.min(i32::from(VOLUME_MAX)) as u8)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn levels_and_decibels_agree() {
        for level in 0..=VOLUME_MAX {
            assert_eq!(level_of(fnkeys::volume_db(level)), level);
        }
        assert_eq!(level_of(5), VOLUME_MAX);
        assert_eq!(level_of(-1000), 0);
    }
}
