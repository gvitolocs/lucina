//! Input devices: every `/dev/input/event*` node, opened non-blocking and read with `poll`.
//! Keyboards come and go (the Book Go's i2c-hid keyboard appears late, a USB one can be plugged in
//! any time), so the kernel posts an `input.added` or `input.removed` line on `/dev/lucina-events`
//! for each of them and the caller rescans on those. The timer in `rescan` remains only for the
//! first 15 s after the start and for kernels that post no events at all.
//!
//! Event-driven through [`crate::input_watch::rescan`] (a thread waiting on the hub): the shell,
//! the app loop, the video player, and LucinaOffice.

use std::fs::{File, OpenOptions};
use std::io::{self, Read};
use std::os::fd::{AsRawFd, RawFd};
use std::os::unix::fs::OpenOptionsExt;
use std::time::{Duration, Instant};

/// How long the timer keeps scanning after the start, in case a device appears before the kernel's
/// first event is polled.
const BOOT_WINDOW: Duration = Duration::from_secs(15);

/// How often the directory is looked at: often while the devices are still appearing at boot, then seldom, so that an idle
/// machine stays asleep. A device plugged in later waits at most the last of these.
fn rescan_every(since_start: Duration) -> Duration {
    if since_start < BOOT_WINDOW {
        Duration::from_millis(200)
    } else if since_start < Duration::from_secs(60) {
        Duration::from_secs(2)
    } else {
        Duration::from_secs(10)
    }
}

pub const EV_SYN: u16 = 0x00;
pub const SYN_DROPPED: u16 = 0x03;
pub const EV_KEY: u16 = 0x01;
pub const EV_REL: u16 = 0x02;
pub const EV_ABS: u16 = 0x03;
pub const REL_X: u16 = 0x00;
pub const REL_Y: u16 = 0x01;
pub const REL_WHEEL: u16 = 0x08;
pub const REL_HWHEEL: u16 = 0x06;
pub const REL_WHEEL_HI_RES: u16 = 0x0b;
pub const REL_HWHEEL_HI_RES: u16 = 0x0c;
pub const ABS_X: u16 = 0x00;
pub const ABS_Y: u16 = 0x01;
pub const ABS_MT_SLOT: u16 = 0x2f;

/// Linux `struct input_event` (64-bit): macOS has no evdev, but the toolkit still compiles.
#[cfg(target_os = "macos")]
#[repr(C)]
#[derive(Clone, Copy)]
struct MacInputEvent {
    time: MacTimeval,
    type_: u16,
    code: u16,
    value: i32,
}

#[cfg(target_os = "macos")]
#[repr(C)]
#[derive(Clone, Copy)]
struct MacTimeval {
    tv_sec: i64,
    tv_usec: i64,
}

/// One `struct input_event`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Event {
    pub type_: u16,
    pub code: u16,
    pub value: i32,
    /// When the kernel got it, in microseconds of `CLOCK_MONOTONIC` (or of the real time on a kernel
    /// without `EVIOCSCLOCKID`).
    pub at_us: i64,
}

/// The range of an absolute axis (`struct input_absinfo`'s minimum and maximum).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AbsRange {
    pub min: i32,
    pub max: i32,
}

/// QEMU's virtio and USB tablets report 0..32767; used when the kernel cannot say.
pub const DEFAULT_ABS: AbsRange = AbsRange { min: 0, max: 32767 };

pub struct Device {
    pub node: String,
    pub name: String,
    /// What EVIOCGNAME returned, for `--trace`.
    pub name_ioctl: i32,
    file: File,
    pub abs_x: AbsRange,
    pub abs_y: AbsRange,
    pub has_abs_ranges: bool,
    /// The EV_REL bitmap has REL_WHEEL_HI_RES or REL_HWHEEL_HI_RES: smooth scroll, in 1/120 notch.
    pub hires: bool,
}

#[derive(Default)]
pub struct Inputs {
    pub devices: Vec<Device>,
    /// Raw multi-touch nodes, skipped: their contacts are already pointer, scroll and button
    /// events on the pointer device, so reading them here would be wrong.
    skipped: Vec<String>,
    scanned: Option<Instant>,
    started: Option<Instant>,
    /// The kernel's `input.added` and `input.removed` events drive the scan; the timer then only
    /// covers the boot window, in case an event is lost.
    event_driven: bool,
}

/// `_IOC(dir, 'E', nr, size)` for the evdev ioctls.
const fn eviocg(nr: u32, size: u32) -> libc::c_ulong {
    const IOC_READ: u32 = 2;
    ((IOC_READ << 30) | (size << 16) | ((b'E' as u32) << 8) | nr) as libc::c_ulong
}

/// `_IOW('E', nr, size)`.
const fn eviocs(nr: u32, size: u32) -> libc::c_ulong {
    const IOC_WRITE: u32 = 1;
    ((IOC_WRITE << 30) | (size << 16) | ((b'E' as u32) << 8) | nr) as libc::c_ulong
}

/// The device's name and what EVIOCGNAME returned: the name's length with its NUL, on Linux.
fn device_name(fd: RawFd) -> Option<(String, i32)> {
    let mut buf = [0u8; 128];
    // SAFETY: EVIOCGNAME(len) writes at most `len` bytes into the buffer.
    let n = unsafe { libc::ioctl(fd, eviocg(0x06, buf.len() as u32) as _, buf.as_mut_ptr()) };
    if n < 0 {
        return None;
    }
    let end = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
    (end > 0).then(|| (String::from_utf8_lossy(&buf[..end]).into_owned(), n))
}

/// The device's capability bitmap for event type `ev`: `EVIOCGBIT(ev, len)` = `_IOC(READ, 'E', 0x20 + ev, len)`.
fn capability_bits(fd: RawFd, ev: u16, len: u32) -> Option<Vec<u8>> {
    let mut buf = vec![0u8; len as usize];
    // SAFETY: EVIOCGBIT(ev, len) writes at most `len` bytes into the buffer.
    let r = unsafe { libc::ioctl(fd, eviocg(0x20 + ev as u32, len) as _, buf.as_mut_ptr()) };
    (r >= 0).then_some(buf)
}

/// Whether a capability bitmap has `code` set.
fn has_bit(bits: &[u8], code: u16) -> bool {
    bits.get((code / 8) as usize)
        .is_some_and(|b| b & (1 << (code % 8)) != 0)
}

fn abs_range(fd: RawFd, axis: u16) -> Option<AbsRange> {
    // struct input_absinfo: value, minimum, maximum, fuzz, flat, resolution.
    let mut info = [0i32; 6];
    // SAFETY: EVIOCGABS(axis) fills a 24-byte struct input_absinfo.
    let r = unsafe { libc::ioctl(fd, eviocg(0x40 + axis as u32, 24) as _, info.as_mut_ptr()) };
    (r >= 0 && info[2] > info[1]).then_some(AbsRange {
        min: info[1],
        max: info[2],
    })
}

/// Whether a line from `/dev/lucina-events` changes the input devices: its kind, the third field,
/// is exactly `input.added` or `input.removed`.
pub fn input_event_changes_devices(line: &str) -> bool {
    matches!(
        line.split_whitespace().nth(2),
        Some("input.added" | "input.removed")
    )
}

impl Inputs {
    /// Open nodes that appeared since the last scan, on the timer's pace; returns whether the set
    /// changed. Event-driven, the timer only covers the boot window.
    pub fn rescan(&mut self, now: Instant) -> bool {
        let started = *self.started.get_or_insert(now);
        if self.event_driven && now.duration_since(started) >= BOOT_WINDOW {
            return false;
        }
        let every = rescan_every(now.duration_since(started));
        if self.scanned.is_some_and(|t| now.duration_since(t) < every) {
            return false;
        }
        self.scanned = Some(now);
        self.scan()
    }

    /// The same scan as `rescan`, ignoring the timer: a kernel input event just arrived.
    pub fn rescan_now(&mut self, now: Instant) -> bool {
        self.started.get_or_insert(now);
        self.scanned = Some(now);
        self.scan()
    }

    /// The kernel's `input.added` and `input.removed` events drive the scan: the timer stops
    /// covering it after the boot window.
    pub fn set_event_driven(&mut self, on: bool) {
        self.event_driven = on;
    }

    /// Open every `event*` node not open yet; returns whether the set changed.
    fn scan(&mut self) -> bool {
        let Ok(dir) = std::fs::read_dir("/dev/input") else {
            return false;
        };
        let mut changed = false;
        let mut nodes: Vec<String> = dir
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.starts_with("event"))
            .collect();
        nodes.sort();
        for node in nodes {
            if self.devices.iter().any(|d| d.node == node) || self.skipped.contains(&node) {
                continue;
            }
            let Ok(file) = OpenOptions::new()
                .read(true)
                .custom_flags(libc::O_NONBLOCK | libc::O_CLOEXEC)
                .open(format!("/dev/input/{node}"))
            else {
                continue;
            };
            let fd = file.as_raw_fd();
            // A raw multi-touch device (ABS_MT_SLOT in its EV_ABS bitmap): its contacts are already
            // turned into pointer, scroll and button events on the pointer device, and every EV_ABS
            // here would be read as an absolute pointer. Skip it, and remember, so the scan does not
            // open it again; `file` drops at the end of this round, closing the node.
            if capability_bits(fd, EV_ABS, 8)
                .as_deref()
                .is_some_and(|b| has_bit(b, ABS_MT_SLOT))
            {
                self.skipped.push(node);
                continue;
            }
            #[cfg(not(target_os = "macos"))]
            let clock = libc::CLOCK_MONOTONIC;
            #[cfg(target_os = "macos")]
            let clock = libc::CLOCK_MONOTONIC as libc::c_int;
            // SAFETY: EVIOCSCLOCKID reads an int. Failing only leaves the event times in real time.
            unsafe { libc::ioctl(fd, eviocs(0xa0, 4) as _, &clock as *const libc::c_int) };
            let (name, name_ioctl) = device_name(fd).unwrap_or_else(|| ("input device".into(), -1));
            let x = abs_range(fd, ABS_X);
            let y = abs_range(fd, ABS_Y);
            let has_abs_ranges = x.is_some() && y.is_some();
            let abs_x = x.unwrap_or(DEFAULT_ABS);
            let abs_y = y.unwrap_or(DEFAULT_ABS);
            let hires = capability_bits(fd, EV_REL, 2)
                .as_deref()
                .is_some_and(|b| has_bit(b, REL_WHEEL_HI_RES) || has_bit(b, REL_HWHEEL_HI_RES));
            self.devices.push(Device {
                node,
                name,
                name_ioctl,
                file,
                abs_x,
                abs_y,
                has_abs_ranges,
                hires,
            });
            changed = true;
        }
        changed
    }

    /// When `rescan` next has something to do: `None` when the timer does not cover it any more
    /// (event-driven, past the boot window).
    pub fn next_rescan(&self, now: Instant) -> Option<Instant> {
        let started = self.started.unwrap_or(now);
        if self.event_driven && now.duration_since(started) >= BOOT_WINDOW {
            return None;
        }
        Some(self.scanned.unwrap_or(now) + rescan_every(now.duration_since(started)))
    }

    pub fn fds(&self) -> Vec<RawFd> {
        self.devices.iter().map(|d| d.file.as_raw_fd()).collect()
    }

    /// Everything readable now, per device index. Devices that fail (unplugged) are dropped;
    /// returns whether one was.
    pub fn read(&mut self, out: &mut Vec<(usize, Event)>) -> bool {
        #[cfg(not(target_os = "macos"))]
        const SIZE: usize = std::mem::size_of::<libc::input_event>();
        #[cfg(target_os = "macos")]
        const SIZE: usize = std::mem::size_of::<MacInputEvent>();
        let mut buf = [0u8; SIZE * 32];
        let mut gone = Vec::new();
        for (i, d) in self.devices.iter_mut().enumerate() {
            loop {
                match d.file.read(&mut buf) {
                    Ok(0) => {
                        gone.push(i);
                        break;
                    }
                    Ok(n) => {
                        for chunk in buf[..n].as_chunks::<SIZE>().0 {
                            // SAFETY: input_event is plain integers; read_unaligned copes with the
                            // byte buffer's alignment.
                            #[cfg(not(target_os = "macos"))]
                            let ev: libc::input_event =
                                unsafe { std::ptr::read_unaligned(chunk.as_ptr().cast()) };
                            #[cfg(target_os = "macos")]
                            let ev: MacInputEvent =
                                unsafe { std::ptr::read_unaligned(chunk.as_ptr().cast()) };
                            out.push((
                                i,
                                Event {
                                    type_: ev.type_,
                                    code: ev.code,
                                    value: ev.value,
                                    at_us: ev.time.tv_sec * 1_000_000 + ev.time.tv_usec,
                                },
                            ));
                        }
                    }
                    Err(e) if e.kind() == io::ErrorKind::WouldBlock => break,
                    Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                    Err(_) => {
                        gone.push(i);
                        break;
                    }
                }
            }
        }
        for &i in gone.iter().rev() {
            self.devices.remove(i);
        }
        !gone.is_empty()
    }
}

/// The pointer: relative motion (mice, the touchpad in relative mode) and absolute positions
/// (tablets, touchscreens), kept inside the screen.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Pointer {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
    /// Shown once a pointing device has moved.
    pub visible: bool,
}

impl Pointer {
    pub fn new(w: i32, h: i32) -> Pointer {
        Pointer {
            x: w / 2,
            y: h / 2,
            w,
            h,
            visible: false,
        }
    }

    pub fn moved(&mut self, dx: i32, dy: i32) {
        self.x = (self.x + dx).clamp(0, self.w - 1);
        self.y = (self.y + dy).clamp(0, self.h - 1);
        self.visible = true;
    }

    /// An absolute position on `axis` (ABS_X or ABS_Y) within `range`.
    pub fn absolute(&mut self, axis: u16, value: i32, range: AbsRange) {
        let span = (range.max - range.min).max(1) as i64;
        let v = (value - range.min).clamp(0, range.max - range.min) as i64;
        match axis {
            ABS_X => self.x = ((v * (self.w - 1) as i64) / span) as i32,
            ABS_Y => self.y = ((v * (self.h - 1) as i64) / span) as i32,
            _ => return,
        }
        self.visible = true;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ioctl_numbers_match_linux() {
        // EVIOCGNAME(256) and EVIOCGABS(ABS_X) as the kernel headers spell them.
        assert_eq!(eviocg(0x06, 256), 0x8100_4506);
        assert_eq!(eviocg(0x40, 24), 0x8018_4540);
        // EVIOCGBIT(EV_REL, 2), as the kernel headers spell it.
        assert_eq!(eviocg(0x20 + EV_REL as u32, 2), 0x8002_4522);
        assert_eq!(eviocs(0xa0, 4), 0x4004_45a0);
        #[cfg(not(target_os = "macos"))]
        assert_eq!(std::mem::size_of::<libc::input_event>(), 24);
    }

    #[test]
    fn lucina_events_line_only_input_kinds() {
        assert!(input_event_changes_devices(
            "17 6821 input.added event2 name=i2c-hid%200cf2:9028"
        ));
        assert!(input_event_changes_devices("18 9001 input.removed event4"));
        assert!(!input_event_changes_devices("3 42 camera.on cam0"));
        assert!(!input_event_changes_devices("1 2"));
        assert!(!input_event_changes_devices(""));
        assert!(!input_event_changes_devices("17 6821 input.addedx event2"));
    }

    #[test]
    fn event_driven_stops_the_timer_after_the_boot_window() {
        let start = Instant::now();

        let mut inputs = Inputs::default();
        inputs.set_event_driven(true);
        inputs.started = Some(start);
        // Inside the boot window the timer still scans, at the 200 ms pace.
        assert_eq!(
            inputs.next_rescan(start + Duration::from_secs(3)),
            Some(start + Duration::from_secs(3) + Duration::from_millis(200))
        );
        // Past it, the kernel's events are the only driver, and the timer rescan does nothing.
        assert_eq!(inputs.next_rescan(start + Duration::from_secs(20)), None);
        assert!(!inputs.rescan(start + Duration::from_secs(20)));

        let inputs = Inputs {
            started: Some(start),
            ..Default::default()
        };
        assert_eq!(
            inputs.next_rescan(start + Duration::from_secs(20)),
            Some(start + Duration::from_secs(20) + Duration::from_secs(2))
        );
        assert_eq!(
            inputs.next_rescan(start + Duration::from_secs(120)),
            Some(start + Duration::from_secs(120) + Duration::from_secs(10))
        );
    }

    #[test]
    fn pointer_stays_on_screen() {
        let mut p = Pointer::new(100, 50);
        assert!(!p.visible);
        p.moved(-500, 10);
        assert_eq!((p.x, p.y), (0, 35));
        assert!(p.visible);
        p.absolute(ABS_X, 32767, DEFAULT_ABS);
        p.absolute(ABS_Y, 0, DEFAULT_ABS);
        assert_eq!((p.x, p.y), (99, 0));
        p.absolute(ABS_X, 16384, AbsRange { min: 0, max: 32767 });
        assert_eq!(p.x, 49);
    }
}
