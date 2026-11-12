//! Remote access to LucinaOS.
//!
//! - `lucina-sshd`: an SSH server on sunset, key-only, for `root`, that runs commands and shells (with a pty).
//! - `juno-wifi-push`: runs on buildhost, not on LucinaOS: hands the Wi-Fi passphrase to `lucina-wifi` over SSH.
//! - `lucina-reload`: receives a kernel and an initramfs on stdin (usually through `ssh root@host lucina-reload`),
//!   checks their SHA-256, hands them to the kernel with `kexec_file_load` and reboots into them
//!   (`reboot(LINUX_REBOOT_CMD_KEXEC)`), all in RAM.
//!
//! This library holds what both need and what can be tested on the host: key files and the reload stream format.

pub mod appset;
pub mod conn;
pub mod dataclean;
pub mod disk;
pub mod disk_rules;
pub mod esp;
pub mod fsck;
pub mod gptw;
pub mod image;
pub mod install;
pub mod keys;
pub mod log;
pub mod netinst;
pub mod netinst_rules;
pub mod process;
pub mod repair;
pub mod seed;
pub mod stream;
pub mod testroot;
