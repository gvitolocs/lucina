//! The tools that change the system: `generation_install`, `reload`, `reboot`, `poweroff`. The server decides which of them need
//! the owner's decision (lucina-rs files it and sends the call with `decision_id`); here they only run.
//!
//! * `generation_install {set}`: what `scripts/juno-install-main.sh` (the `juno-disk-root` path) does on the laptop. The kernel
//!   command line comes from BOOT.CFG on the ESP (mounted read-only, without ` maxcpus=1`), then
//!   `lucina-install-disk --root --base 0xa0000000 --cmdline "<line>" < <set>/disk-stream`. The new system goes to the free slot
//!   and BOOT.CFG changes last, as a trial boot; `lucina-install-disk rollback` undoes it.
//! * `reload {generation?}`: the ESP keeps the current generation (BOOT.CFG) and the previous one (PREV.CFG). Its kernel, initramfs
//!   and command line go through `lucina-reload pack … | lucina-reload`: checked, handed to kexec, entered a second later. Nothing
//!   is written to the disk.
//! * `reboot`, `poweroff`: the shell's power menu (`lucina-data clean`, then reboot(2)), a second after the answer is sent.
use std::path::Path;
use std::time::Duration;

use serde_json::{Value, json};

use crate::bash;
use crate::tools::Machine;

/// The ESP, which holds `\asterinas\BOOT.CFG` and `PREV.CFG`.
pub const ESP: &str = "/dev/ufs0lun0p1";
pub const INSTALL_SECS: u64 = 1800;
pub const RELOAD_SECS: u64 = 120;
const LOG_LINES: usize = 40;

/// A word for `/bin/sh`, whatever it holds.
pub fn quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

/// The ESP mounted read-only in a folder of its own, `$esp`, and unmounted when the script ends.
fn mount_esp(esp: &str) -> String {
    format!(
        "PATH=/bin:/usr/bin:/sbin:/usr/sbin\n\
         esp=$(mktemp -d /tmp/devd-esp.XXXXXX) || exit 1\n\
         mount -t vfat -o ro {} \"$esp\" || {{ rmdir \"$esp\"; echo 'the ESP did not mount'; exit 1; }}\n\
         trap 'umount \"$esp\"; rmdir \"$esp\"' EXIT\n",
        quote(esp)
    )
}

/// The script `generation_install` runs, for a set folder already checked.
pub fn install_script(esp: &str, set: &Path) -> String {
    let stream = quote(&set.join("disk-stream").display().to_string());
    format!(
        "{}line=$(sed -n 's/^cmdline *= *//p' \"$esp/asterinas/BOOT.CFG\" | head -1 | sed 's/ maxcpus=1//')\n\
         [ -n \"$line\" ] || {{ echo 'BOOT.CFG has no cmdline'; exit 1; }}\n\
         log=/tmp/devd-install.log\n\
         lucina-install-disk --root --base 0xa0000000 --cmdline \"$line\" < {stream} > \"$log\" 2>&1\n\
         rc=$?\n\
         tail -n {LOG_LINES} \"$log\"\n\
         echo \"install-exit $rc\"\n\
         exit $rc\n",
        mount_esp(esp)
    )
}

/// The script `reload` runs: BOOT.CFG's or PREV.CFG's kernel, initramfs and command line.
pub fn reload_script(esp: &str, previous: bool) -> String {
    let cfg = if previous { "PREV.CFG" } else { "BOOT.CFG" };
    format!(
        "{}cfg=\"$esp/asterinas/{cfg}\"\n\
         [ -f \"$cfg\" ] || {{ echo 'the ESP has no {cfg}'; exit 1; }}\n\
         kernel=$(sed -n 's/^kernel *= *\\([^ ]*\\).*/\\1/p' \"$cfg\" | head -1)\n\
         initrd=$(sed -n 's/^initrd *= *//p' \"$cfg\" | head -1)\n\
         line=$(sed -n 's/^cmdline *= *//p' \"$cfg\" | head -1)\n\
         [ -n \"$kernel\" ] && [ -n \"$initrd\" ] && [ -n \"$line\" ] || {{ echo '{cfg} names no kernel, initramfs or command line'; exit 1; }}\n\
         echo \"{cfg}: kernel $kernel, initramfs $initrd\"\n\
         lucina-reload pack --kernel \"$esp/asterinas/$kernel\" --initrd \"$esp/asterinas/$initrd\" --cmdline \"$line\" | lucina-reload 2>&1\n",
        mount_esp(esp)
    )
}

/// The set: a folder under `/data` holding `disk-stream`, and no refused place.
pub fn check_set(m: &Machine, set: &str) -> Result<std::path::PathBuf, String> {
    let real = m.places.existing(set)?;
    let data = std::fs::canonicalize(m.places.root().join("data"))
        .map_err(|_| "this boot has no /data".to_string())?;
    if !real.starts_with(&data) || real == data {
        return Err(format!("{set}: a generation set is a folder under /data"));
    }
    if !real.is_dir() {
        return Err(format!("{set}: not a folder"));
    }
    if !real.join("disk-stream").is_file() {
        return Err(format!(
            "{set}: no disk-stream in it (scripts/juno-disk-stream.sh makes one)"
        ));
    }
    Ok(real)
}

fn script(m: &Machine, text: &str, secs: u64) -> Result<bash::Output, String> {
    bash::run(&bash::Run {
        shell: &m.bash_shell,
        command: text,
        cwd: Path::new("/"),
        timeout: Duration::from_secs(secs),
    })
}

pub fn generation_install(m: &Machine, set: &str) -> Result<Value, String> {
    let real = check_set(m, set)?;
    let o = script(m, &install_script(&m.esp_device, &real), INSTALL_SECS)?;
    let log = o.text.trim_end().to_owned();
    if o.code != Some(0) {
        return Err(format!("the install failed: {log}"));
    }
    Ok(json!({
        "text": format!("Installed from {} as a trial boot; `lucina-install-disk rollback` undoes it. Log:\n{log}", real.display()),
        "log": log, "set": real.display().to_string(),
    }))
}

pub fn reload(m: &Machine, previous: bool) -> Result<Value, String> {
    let which = if previous { "previous" } else { "current" };
    let o = script(m, &reload_script(&m.esp_device, previous), RELOAD_SECS)?;
    let log = o.text.trim_end().to_owned();
    if o.code != Some(0) {
        return Err(format!(
            "the reload into the {which} generation failed, and the laptop runs on as it was: {log}"
        ));
    }
    Ok(json!({
        "text": format!("Reloading into the {which} generation; the laptop restarts in about a second. {log}"),
        "generation": which, "log": log,
    }))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Leave {
    Reboot,
    Poweroff,
}

/// As the shell's `leave`: /data marked clean, then reboot(2), a second from now so the answer goes first.
pub fn leave(m: &Machine, how: Leave) -> Result<Value, String> {
    let clean = if m.places.root().join("run/lucina/data-ready").exists() {
        match std::process::Command::new("/sbin/lucina-data")
            .arg("clean")
            .status()
        {
            Ok(s) if s.success() => "/data marked clean".to_owned(),
            Ok(s) => format!("/data not marked clean (lucina-data {s}); the next boot checks it"),
            Err(e) => format!("/data not marked clean ({e}); the next boot checks it"),
        }
    } else {
        "no /data on this boot".to_owned()
    };
    let cmd = match how {
        Leave::Reboot => libc::RB_AUTOBOOT,
        Leave::Poweroff => libc::RB_POWER_OFF,
    };
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_secs(1));
        // SAFETY: reboot takes one integer and returns only if it failed.
        let r = unsafe { libc::reboot(cmd) };
        eprintln!(
            "lucina-devd: reboot returned {r}: {}",
            std::io::Error::last_os_error()
        );
    });
    let what = match how {
        Leave::Reboot => "Restarting",
        Leave::Poweroff => "Powering off",
    };
    Ok(json!({"text": format!("{what} in a second; {clean}.")}))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::tests::test_machine;

    #[test]
    fn a_set_is_a_folder_under_data_with_its_stream() {
        let (root, m, _) = test_machine("set");
        let set = root.join("data/home/gen42");
        std::fs::create_dir_all(&set).unwrap();
        let s = set.display().to_string();
        assert!(check_set(&m, &s).unwrap_err().contains("disk-stream"));
        std::fs::write(set.join("disk-stream"), b"x").unwrap();
        assert_eq!(check_set(&m, &s).unwrap(), set);
        assert!(
            check_set(&m, &root.join("etc").display().to_string())
                .unwrap_err()
                .contains("under /data")
        );
        assert!(check_set(&m, &root.join("data").display().to_string()).is_err());
        let cred = root.join("data/lucina/secretary").display().to_string();
        assert!(check_set(&m, &cred).unwrap_err().contains("refused"));
    }

    #[test]
    fn the_install_runs_what_juno_install_main_runs() {
        let s = install_script(ESP, Path::new("/data/sets/it's"));
        assert!(
            s.contains("mount -t vfat -o ro '/dev/ufs0lun0p1' \"$esp\""),
            "{s}"
        );
        assert!(s.contains("sed -n 's/^cmdline *= *//p' \"$esp/asterinas/BOOT.CFG\" | head -1 | sed 's/ maxcpus=1//'"));
        assert!(s.contains("lucina-install-disk --root --base 0xa0000000 --cmdline \"$line\" < '/data/sets/it'\\''s/disk-stream'"), "{s}");
        assert!(s.contains("tail -n 40"));
    }

    #[test]
    fn the_reload_takes_the_files_boot_cfg_or_prev_cfg_names() {
        let s = reload_script(ESP, false);
        assert!(s.contains("$esp/asterinas/BOOT.CFG"));
        assert!(s.contains("lucina-reload pack --kernel \"$esp/asterinas/$kernel\" --initrd \"$esp/asterinas/$initrd\" --cmdline \"$line\" | lucina-reload"));
        assert!(reload_script(ESP, true).contains("$esp/asterinas/PREV.CFG"));
    }

    #[test]
    fn the_scripts_parse_boot_cfg_as_lucina_install_disk_writes_it() {
        // The ESP mount replaced by a folder: the rest of the script runs as written.
        let (root, m, _) = test_machine("cfg");
        let esp = root.join("esp/asterinas");
        std::fs::create_dir_all(&esp).unwrap();
        std::fs::write(
            esp.join("BOOT.CFG"),
            "# lucina-install-disk generation 7\nkernel = K0007.IMG 0xa0000000\ninitrd = I0007.GZ\ncmdline = console=ttyMSM0 maxcpus=1 lucina.x=1 -- /etc/lucina/remote.sh\n",
        )
        .unwrap();
        let fake = format!("esp={}\n", quote(&root.join("esp").display().to_string()));
        let body = |s: String| {
            let at = s.find("trap ").unwrap();
            let after = &s[at..];
            fake.clone() + &after[after.find('\n').unwrap() + 1..]
        };
        let install = body(install_script(ESP, Path::new("/nowhere")))
            .replace("lucina-install-disk", "echo")
            .replace("< '/nowhere/disk-stream'", "");
        let o = script(&m, &install, 10).unwrap();
        assert!(o.text.contains("--root --base 0xa0000000 --cmdline console=ttyMSM0 lucina.x=1 -- /etc/lucina/remote.sh"), "{}", o.text);
        assert!(o.text.contains("install-exit 0"));
        let reload = body(reload_script(ESP, false))
            .replace("| lucina-reload 2>&1", "")
            .replace("lucina-reload pack", "echo pack");
        let o = script(&m, &reload, 10).unwrap();
        assert!(
            o.text
                .contains("BOOT.CFG: kernel K0007.IMG, initramfs I0007.GZ"),
            "{}",
            o.text
        );
        assert!(
            o.text.contains("asterinas/K0007.IMG --initrd"),
            "{}",
            o.text
        );
        assert!(
            o.text
                .contains("--cmdline console=ttyMSM0 maxcpus=1 lucina.x=1"),
            "{}",
            o.text
        );
        assert_ne!(
            script(&m, &body(reload_script(ESP, true)), 10)
                .unwrap()
                .code,
            Some(0)
        );
    }
}
