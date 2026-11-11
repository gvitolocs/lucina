//! lucina-trial: the trial's other half (`lucina_boot::trial`, patch-free: the loader says what it did on the kernel command line).
//!
//! `lucina.trial=on:<generation>`: this generation is on trial. Once the shell has drawn and the same shell has lived for [`STABLE`], the
//! record says good and the counting stops. A shell that died in that time leaves it uncounted: this boot used up a try.
//!
//! `lucina.trial=rolledback:<generation>`: the loader gave up on that generation and started PREV.CFG. BOOT.CFG is made to name what is
//! running, by the same one-block commit as an install, so that the next install takes the failed slot and never the working one. A
//! line says so on the home screen and in `lucina-crash`.
//!
//! After the good mark the generation is watched for [`trial::SETTLE_BOOTS`] boots, kept in `/data/lucina/trial-settle`: if
//! [`trial::SETTLE_BAD`] of them end in a crash or a reset that left no record, it goes back to PREV.CFG by itself, as the loader would
//! have done inside the 60 s (`trial::settle_step`, proved with the rest). Generation 32 did that every 100 s on 2026-11-05.
//!
//! Every boot: an app the shell had open when the machine crashed gets its line too. It is never reopened.
//!
//! `lucina-trial show <file>...` prints records, for tests.

// Juno with no C library: Eyra is the libc and the program start, in Rust (docs/research/juno-pure-rust.md).
#[cfg(all(target_os = "linux", target_env = "gnu", target_arch = "aarch64"))]
extern crate eyra;

use std::{
    fs,
    process::{Command, ExitCode},
    thread::sleep,
    time::Duration,
};

use lucina_boot::trial::{self, Record, Settle, State};
use lucina_remote::{
    disk::{self, FOLDER, Mounted, in_folder, overwrite_block, sync_all},
    esp::release_apps_mount,
    install::{LAPTOP_ESP, config_block, generation},
};

/// How long the same shell must live after its first frame. A crash or a hang that comes later than this no longer rolls back.
const STABLE: Duration = Duration::from_secs(60);
const FIRST_FRAME: &str = "/run/lucina/first-frame";
const NOTICE: &str = "/run/lucina/notice";
const DATA_READY: &str = "/run/lucina/data-ready";
const OPEN_APP: &str = "/data/lucina/open-app";
const SETTLE: &str = "/data/lucina/trial-settle";
/// The line for the home screen after a settle rollback, carried over the restart.
const WENT_BACK: &str = "/data/lucina/trial-went-back";

fn kmsg(line: &str) {
    let _ = fs::write("/dev/kmsg", format!("[trial] {line}\n"));
}

fn notice(line: &str) {
    let mut text = fs::read_to_string(NOTICE).unwrap_or_default();
    text += line;
    text.push('\n');
    let _ = fs::write(NOTICE, text);
    kmsg(line);
}

/// This boot's `lucina.trial=` and the generation it names.
fn asked() -> Option<(String, u32)> {
    let line = fs::read_to_string("/proc/cmdline").ok()?;
    let (kind, generation) = line
        .split_ascii_whitespace()
        .take_while(|a| *a != "--")
        .find_map(|a| a.strip_prefix("lucina.trial="))?
        .split_once(':')?;
    Some((kind.to_string(), generation.parse().ok()?))
}

/// The PID of the running shell.
fn shell() -> Option<u32> {
    fs::read_dir("/proc").ok()?.flatten().find_map(|e| {
        let pid: u32 = e.file_name().to_str()?.parse().ok()?;
        let cmd = fs::read(e.path().join("cmdline")).ok()?;
        (cmd.split(|b| *b == 0).next()? == b"/usr/bin/lucina-shell").then_some(pid)
    })
}

fn wait_for(path: &str, limit: Duration) -> bool {
    let step = Duration::from_millis(100);
    let mut waited = Duration::ZERO;
    while !fs::exists(path).unwrap_or(false) {
        if waited >= limit {
            return false;
        }
        sleep(step);
        waited += step;
    }
    true
}

/// The ESP read-only, for a read. Mounting read-write when nothing is written left the volume to the vfat driver at every boot,
/// and resets followed some of those mounts (crash-recovery, 2026-11-11). Every mount says why in the kernel log.
fn esp_for_reading(why: &str) -> Result<Mounted, String> {
    release_apps_mount();
    let (device, _) = disk::check_target(LAPTOP_ESP)?;
    kmsg(&format!("ESP read-only: {why}"));
    Mounted::new(&device, true)
}

/// The ESP read-write, only around a write; the caller syncs and drops it (unmount) at once.
fn esp_for_writing(why: &str) -> Result<Mounted, String> {
    release_apps_mount();
    let (device, _) = disk::check_target(LAPTOP_ESP)?;
    kmsg(&format!("ESP read-write: {why}"));
    Mounted::new(&device, false)
}

/// This boot's generation, from `lucina.generation=` on the kernel command line (the installer writes it since 2026-11-09).
fn generation_on_cmdline() -> Option<u32> {
    let line = fs::read_to_string("/proc/cmdline").ok()?;
    line.split_ascii_whitespace()
        .take_while(|w| *w != "--")
        .find_map(|w| w.strip_prefix("lucina.generation="))
        .and_then(|g| g.parse().ok())
}

/// The generation whose loader settle window is known to be closed (written here when it closes): later boots of it need no ESP.
const SETTLED: &str = "/data/lucina/trial-settled";

fn settled_generation() -> Option<u32> {
    fs::read_to_string(SETTLED).ok()?.trim().parse().ok()
}

fn note_settled(generation: u32) {
    let _ = write_whole(SETTLED, &format!("{generation}\n"));
}

fn mark_good(generation: u32) -> Result<(), String> {
    let newest = {
        let _esp = esp_for_reading("the trial record, before the good mark")?;
        disk::read_trial()
    };
    let record = newest.record.ok_or("no record")?;
    if record.generation != generation {
        return Err(format!(
            "the record is about generation {}",
            record.generation
        ));
    }
    if record.seq == u32::MAX {
        return Err("the record's count is spent".into());
    }
    // AB-3: the good mark opens the settle window the loader counts.
    let good = trial::mark_good(record);
    let _esp = esp_for_writing("the good mark")?;
    disk::write_trial(newest.write_to, &good)?;
    sync_all();
    Ok(())
}

/// A good generation in the loader's settle window (AB-3): once the same shell has lived [`STABLE`], this start lasted, so the
/// record gets its starts back and one boot of the window is used. Nothing happens for a settled generation.
fn settle_lasted() {
    if !wait_for(FIRST_FRAME, Duration::from_secs(120)) {
        return;
    }
    let on_cmdline = generation_on_cmdline();
    // A generation whose window closed on an earlier boot: nothing to read, no mount at all.
    if on_cmdline.is_some() && on_cmdline == settled_generation() {
        return;
    }
    let Some(before) = shell() else { return };
    sleep(STABLE);
    if shell() != Some(before) {
        return;
    }
    let result = (|| -> Result<Option<Record>, String> {
        let (current, newest) = {
            let _esp = esp_for_reading("the settle window")?;
            let current = match on_cmdline {
                Some(g) => g,
                None => generation(
                    &fs::read_to_string(in_folder("BOOT.CFG"))
                        .map_err(|e| format!("BOOT.CFG: {e}"))?,
                ),
            };
            (current, disk::read_trial())
        };
        let Some(next) = newest.record.and_then(|r| trial::lasted(r, current)) else {
            // Settled (or not this generation's record): later boots of this generation skip the ESP.
            if newest
                .record
                .is_some_and(|r| r.generation == current && r.settle == 0)
            {
                note_settled(current);
            }
            return Ok(None);
        };
        {
            let _esp = esp_for_writing("one boot of the settle window used")?;
            disk::write_trial(newest.write_to, &next)?;
            sync_all();
        }
        if next.settle == 0 {
            note_settled(current);
        }
        Ok(Some(next))
    })();
    match result {
        Ok(Some(r)) => kmsg(&format!(
            "generation {} lasted: {} settle boots left",
            r.generation, r.settle
        )),
        Ok(None) => {}
        Err(e) => kmsg(&format!("settle window: {e}")),
    }
}

fn settle_on_previous(failed: u32) -> Result<u32, String> {
    let _esp = esp_for_writing("back to PREV.CFG")?;
    let prev = fs::read_to_string(in_folder("PREV.CFG")).map_err(|e| format!("PREV.CFG: {e}"))?;
    overwrite_block(&in_folder("BOOT.CFG"), &config_block(&prev)?)?;
    sync_all();
    let now = fs::read_to_string(in_folder("BOOT.CFG")).map_err(|e| format!("BOOT.CFG: {e}"))?;
    if generation(&now) == failed {
        return Err(format!("{FOLDER}/BOOT.CFG still names generation {failed}"));
    }
    Ok(generation(&now))
}

/// A crash with an app open: the line for the home screen. Whatever it was, the record goes.
fn after_crash() {
    if !fs::exists("/run/lucina/data-expected").unwrap_or(false) {
        return;
    }
    if !wait_for(DATA_READY, Duration::from_secs(120)) {
        return;
    }
    let Ok(app) = fs::read_to_string(OPEN_APP) else {
        return;
    };
    let _ = fs::remove_file(OPEN_APP);
    let crashed = Command::new("/sbin/lucina-crash")
        .output()
        .map(|o| {
            String::from_utf8_lossy(&o.stdout)
                .lines()
                .next()
                .unwrap_or("")
                .contains("last boot crashed")
        })
        .unwrap_or(false);
    if crashed {
        notice(&format!(
            "Lucina Juno restarted after a crash while {} was open",
            app.trim()
        ));
    }
}

/// The count after the good mark, from `/data`: "generation boots bad". Anything else reads as none.
fn read_settle() -> Option<Settle> {
    let text = fs::read_to_string(SETTLE).ok()?;
    let mut f = text.split_ascii_whitespace().map(|w| w.parse::<u32>().ok());
    let (generation, boots, bad) = (f.next()??, f.next()??, f.next()??);
    Some(Settle {
        generation,
        boots,
        bad,
    })
}

/// Written whole or not at all: a new file, synced, renamed over the old, and the folder synced.
/// A cut in the middle leaves `<path>.new` behind; the next call truncates it, and readers never look at it.
fn write_whole(path: &str, text: &str) -> Result<(), String> {
    use std::io::Write;
    let tmp = format!("{path}.new");
    let mut f = fs::File::create(&tmp).map_err(|e| format!("{tmp}: {e}"))?;
    f.write_all(text.as_bytes())
        .map_err(|e| format!("{tmp}: {e}"))?;
    f.sync_all().map_err(|e| format!("{tmp}: {e}"))?;
    fs::rename(&tmp, path).map_err(|e| format!("{path}: {e}"))?;
    if let Some(dir) = path
        .rsplit_once('/')
        .and_then(|(parent, _)| fs::File::open(parent).ok())
    {
        let _ = dir.sync_all();
    }
    Ok(())
}

/// The settle count, written whole or not at all.
fn write_settle(s: &Settle) -> Result<(), String> {
    write_whole(SETTLE, &format!("{} {} {}\n", s.generation, s.boots, s.bad))
}

/// Whether the boot before this one ended in a crash record or vanished, from `lucina-crash`'s first line.
fn last_boot_bad() -> bool {
    Command::new("/sbin/lucina-crash")
        .output()
        .map(|o| {
            let line = String::from_utf8_lossy(&o.stdout)
                .lines()
                .next()
                .unwrap_or("")
                .to_string();
            line.contains("last boot crashed") || line.contains("last boot vanished")
        })
        .unwrap_or(false)
}

/// A good generation in its first boots: one more boot counted, and back to PREV.CFG if it keeps ending badly.
fn settle_check() -> Result<(), String> {
    if let Ok(line) = fs::read_to_string(WENT_BACK) {
        notice(line.trim());
        let _ = fs::remove_file(WENT_BACK);
    }
    let Some(prev) = read_settle() else {
        return Ok(());
    };
    if prev.boots >= trial::SETTLE_BOOTS {
        return Ok(());
    }
    let last_bad = last_boot_bad();
    let esp = esp_for_reading("BOOT.CFG and PREV.CFG for the settle count")?;
    let current = generation(
        &fs::read_to_string(in_folder("BOOT.CFG")).map_err(|e| format!("BOOT.CFG: {e}"))?,
    );
    let previous = fs::read_to_string(in_folder("PREV.CFG"))
        .ok()
        .map(|t| generation(&t));
    let have_previous = previous.is_some_and(|p| p != current && p != 0);
    drop(esp);
    let step = trial::settle_step(Some(prev), current, last_bad, have_previous);
    match step.next {
        Some(next) => write_settle(&next)?,
        None => {
            let _ = fs::remove_file(SETTLE);
        }
    }
    if step.roll_back {
        let bad = step.next.map(|n| n.bad).unwrap_or(0);
        let now = settle_on_previous(current)?;
        let _ = fs::remove_file(SETTLE);
        let line = format!(
            "Lucina Juno went back to generation {now}: generation {current} crashed or reset {bad} times after it was marked good"
        );
        kmsg(&line);
        let _ = write_whole(WENT_BACK, &line);
        sync_all();
        let _ = Command::new("reboot").arg("-f").status();
    }
    Ok(())
}

fn run() -> Result<(), String> {
    let asked = asked();
    let crash_thread = std::thread::spawn(after_crash);
    match asked {
        Some((kind, failed)) if kind == "rolledback" => {
            let now = settle_on_previous(failed)?;
            notice(&format!(
                "Lucina Juno went back to generation {now}: generation {failed} did not start"
            ));
        }
        Some((kind, on_trial)) if kind == "on" => {
            kmsg(&format!("generation {on_trial} is on trial"));
            if !wait_for(FIRST_FRAME, Duration::from_secs(120)) {
                return Err("no first frame in 2 minutes: this try is used up".into());
            }
            let before = shell().ok_or("no shell")?;
            sleep(STABLE);
            if shell() != Some(before) {
                return Err("the shell did not last: this try is used up".into());
            }
            mark_good(on_trial)?;
            if wait_for(DATA_READY, Duration::from_secs(120)) {
                write_settle(&trial::settle_start(on_trial))?;
            }
            kmsg(&format!("generation {on_trial} is good"));
        }
        _ => {
            if wait_for(DATA_READY, Duration::from_secs(120)) {
                settle_check()?;
            }
            settle_lasted();
        }
    }
    let _ = crash_thread.join();
    Ok(())
}

fn show(paths: &[String]) -> ExitCode {
    for path in paths {
        let record = fs::read(path).ok().and_then(|b| Record::decode(&b));
        println!("{path}: {record:?}");
    }
    let read = |i: usize| {
        paths
            .get(i)
            .and_then(|p| fs::read(p).ok())
            .and_then(|b| Record::decode(&b))
    };
    println!("newest: {:?}", trial::newest(read(0), read(1)).record);
    ExitCode::SUCCESS
}

/// `write <file> <seq> <generation> <trial|good|failed> <tries>`, for scripts/qemu-juno-trial.sh, on the host.
fn write_for_a_test(args: &[String]) -> ExitCode {
    let number = |i: usize| args.get(i).and_then(|a| a.parse::<u32>().ok());
    let state = match args.get(3).map(String::as_str) {
        Some("trial") => Some(State::Trial),
        Some("good") => Some(State::Good),
        Some("failed") => Some(State::Failed),
        _ => None,
    };
    let (Some(path), Some(seq), Some(generation), Some(state), Some(tries)) =
        (args.first(), number(1), number(2), state, number(4))
    else {
        eprintln!(
            "usage: lucina-trial write <file> <seq> <generation> <trial|good|failed> <tries> [settle]"
        );
        return ExitCode::FAILURE;
    };
    let settle = number(5).unwrap_or(0);
    match fs::write(
        path,
        Record {
            seq,
            generation,
            state,
            tries,
            settle,
        }
        .encode(),
    ) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("{path}: {e}");
            ExitCode::FAILURE
        }
    }
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().map(String::as_str) == Some("show") {
        return show(&args[1..]);
    }
    if args.first().map(String::as_str) == Some("write") {
        return write_for_a_test(&args[1..]);
    }
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            kmsg(&e);
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::write_whole;
    use std::{fs, path::Path};

    /// `f` runs in a fresh temp directory, removed afterwards.
    fn with_dir(name: &str, f: impl FnOnce(&str)) {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap();
        let dir = std::env::temp_dir().join(format!(
            "lucina-trial-test-{}-{name}-{now:?}",
            std::process::id()
        ));
        fs::create_dir_all(&dir).unwrap();
        f(dir.to_str().unwrap());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn write_whole_creates_the_file_with_exact_contents() {
        with_dir("creates", |dir| {
            let path = format!("{dir}/trial-settle");
            write_whole(&path, "3 1 0\n").unwrap();
            assert_eq!(fs::read_to_string(&path).unwrap(), "3 1 0\n");
        });
    }

    #[test]
    fn write_whole_replaces_an_existing_file() {
        with_dir("replaces", |dir| {
            let path = format!("{dir}/trial-settle");
            fs::write(&path, "a much older and longer content").unwrap();
            write_whole(&path, "3 1 0\n").unwrap();
            assert_eq!(fs::read_to_string(&path).unwrap(), "3 1 0\n");
        });
    }

    #[test]
    fn write_whole_leaves_no_new_file_behind() {
        with_dir("no-new-file", |dir| {
            let path = format!("{dir}/trial-settle");
            write_whole(&path, "3 1 0\n").unwrap();
            assert!(!Path::new(&format!("{path}.new")).exists());
        });
    }

    #[test]
    fn a_leftover_new_file_does_not_break_the_next_write() {
        with_dir("leftover", |dir| {
            let path = format!("{dir}/trial-settle");
            fs::write(format!("{path}.new"), "half a line from a cut boot").unwrap();
            write_whole(&path, "3 1 0\n").unwrap();
            assert_eq!(fs::read_to_string(&path).unwrap(), "3 1 0\n");
            assert!(!Path::new(&format!("{path}.new")).exists());
        });
    }
}
