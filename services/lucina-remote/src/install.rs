//! What `lucina-install-disk` decides, apart from the disk itself: is this the EFI system partition of the main user
//! LUN, what does the new BOOT.CFG say, which files belong to it.
//!
//! The commit is one write: BOOT.CFG is exactly one 4096-byte block of the disk, and it is overwritten in place. The
//! files it names are written first under new names, so before that write the old set boots and after it the new one.
//! No file the running set uses is renamed or changed, because a rename on FAT is two directory updates with a moment
//! between them when a name does not exist.

use lucina_boot::uefi::Config;
use lucina_ufs::gpt;

/// BOOT.CFG's size: one logical block of the laptop's disk, the unit the device writes whole.
pub const CONFIG_BLOCK: usize = 4096;

/// The Galaxy Book Go's EFI system partition, in 4096-byte LBAs.
pub const LAPTOP_ESP: (u64, u64) = (256, 66815);

const EFI_SYSTEM: &str = "C12A7328-F81F-11D2-BA4B-00A0C93EC93B";
const GENERATION: &str = "# lucina-install-disk generation ";
const PADDING: &str = "# padding: this file stays one 4096-byte block, see lucina-install-disk";

/// A partition found to be the install target.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Esp {
    /// Its number, 1 for the first entry: `/dev/ufs0lun0p1`.
    pub index: u32,
    pub first_lba: u64,
    pub last_lba: u64,
}

/// Finds the target in LUN 0's GPT (`lba1`: the header block; `array`: the partition entries). Refuses a damaged table,
/// anything but exactly one EFI system partition, and a partition anywhere but `expected`.
pub fn find_esp(lba1: &[u8], array: &[u8], expected: (u64, u64)) -> Result<Esp, String> {
    let header = gpt::parse_header(lba1).ok_or("LUN 0 has no GPT at LBA 1")?;
    if !header.crc_ok {
        return Err("the GPT header's CRC is wrong".into());
    }
    let bytes = header.entries as usize * header.entry_size as usize;
    let table = array
        .get(..bytes)
        .ok_or("the GPT's partition array was not read whole")?;
    if gpt::crc32(table) != header.entries_crc {
        return Err("the GPT partition array's CRC is wrong".into());
    }
    let esps: Vec<_> = gpt::parse_entries(&header, table)
        .into_iter()
        .filter(|e| gpt::guid(&e.type_guid) == EFI_SYSTEM)
        .collect();
    let [esp] = esps.as_slice() else {
        return Err(format!(
            "LUN 0 has {} EFI system partitions, expected one",
            esps.len()
        ));
    };
    if (esp.first_lba, esp.last_lba) != expected {
        return Err(format!(
            "the EFI system partition is at LBA {}-{}, expected {}-{}",
            esp.first_lba, esp.last_lba, expected.0, expected.1
        ));
    }
    Ok(Esp {
        index: esp.index,
        first_lba: esp.first_lba,
        last_lba: esp.last_lba,
    })
}

/// Checks the boot sector: FAT32 with 4096-byte sectors. Then a file's first block is one block of the disk, which
/// is what makes the commit one write.
pub fn check_fat(boot_sector: &[u8]) -> Result<(), String> {
    if boot_sector.len() < 512 || boot_sector[510..512] != [0x55, 0xaa] {
        return Err("the partition has no FAT boot sector".into());
    }
    let sector = u16::from_le_bytes([boot_sector[11], boot_sector[12]]);
    if sector != CONFIG_BLOCK as u16 {
        return Err(format!(
            "the file system has {sector}-byte sectors, expected {CONFIG_BLOCK}"
        ));
    }
    if &boot_sector[82..90] != b"FAT32   " {
        return Err("the file system is not FAT32".into());
    }
    Ok(())
}

/// The generation BOOT.CFG was written by; 0 for one this tool did not write.
pub fn generation(config: &str) -> u32 {
    config
        .lines()
        .find_map(|l| l.strip_prefix(GENERATION)?.trim().parse().ok())
        .unwrap_or(0)
}

pub fn kernel_name(generation: u32) -> String {
    format!("K{:04}.IMG", generation % 10000)
}

pub fn initrd_name(generation: u32) -> String {
    format!("I{:04}.GZ", generation % 10000)
}

/// Whether `name` is a file this tool made and may remove when nothing names it any more.
pub fn is_generated(name: &str) -> bool {
    let upper = name.to_ascii_uppercase();
    let digits = |s: &str| s.len() == 4 && s.bytes().all(|b| b.is_ascii_digit());
    upper
        .strip_prefix('K')
        .and_then(|r| r.strip_suffix(".IMG"))
        .is_some_and(digits)
        || upper
            .strip_prefix('I')
            .and_then(|r| r.strip_suffix(".GZ"))
            .is_some_and(digits)
}

/// Whether a loader understands `quiet`: the key is a string in the binary. A loader without it refuses the whole BOOT.CFG.
pub fn loader_knows_quiet(loader: &[u8]) -> bool {
    loader.windows(5).any(|w| w == b"quiet")
}

/// A configuration without its `quiet` line, for a loader that does not know the key.
pub fn drop_quiet(config: &str) -> String {
    let mut out = String::new();
    for line in config.lines() {
        let key = line.split_once('=').map(|(k, _)| k.trim());
        if key == Some("quiet") {
            continue;
        }
        out += line;
        out.push('\n');
    }
    out
}

/// What the kernel runs first when it starts from a root partition.
pub const ROOT_INIT: &str = "/etc/lucina/init.sh";

/// The laptop's host name (owner, 2026-11-08), as `LAPTOP_HOSTNAME` in lucina-init's board.rs: the kernel takes it from
/// `lucina.hostname=` (patch 1115) for `uname -n` and DHCP.
pub const LAPTOP_HOSTNAME: &str = "Juno001";

/// The arguments before the kernel's ` -- `, and the rest with it.
fn split_line(line: &str) -> (&str, &str) {
    match line.find(" -- ") {
        Some(at) => line.split_at(at),
        None => (line, ""),
    }
}

/// The line with `lucina.generation=<n>`, the generation BOOT.CFG names, in place of any earlier one (the home screen shows it).
pub fn with_generation(line: &str, n: u32) -> String {
    let (args, rest) = split_line(line);
    let mut out: Vec<String> = args
        .split_ascii_whitespace()
        .filter(|a| !a.starts_with("lucina.generation="))
        .map(str::to_string)
        .collect();
    out.push(format!("lucina.generation={n}"));
    format!("{}{rest}", out.join(" "))
}

/// A command line that boots from this root partition, holding this image; `ufs=on ufs.write=main` so that the kernel sees the disk and
/// /data can be written. Earlier root arguments go. Without a `lucina.hostname=` word the laptop's name is added; one given
/// stays, so the laptop can be renamed on its command line.
pub fn with_root(line: &str, device: &str, uuid: &str) -> String {
    let (args, rest) = split_line(line);
    let mut out: Vec<String> = args
        .split_ascii_whitespace()
        .filter(|a| {
            !matches!(*a, "ro" | "rw")
                && !["root=", "rootfstype=", "init=", "lucina.root.uuid=", "ufs=", "ufs.write="]
                    .iter()
                    .any(|k| a.starts_with(k))
        })
        .map(str::to_string)
        .collect();
    out.push("ufs=on".into());
    out.push("ufs.write=main".into());
    out.push(format!("root={device}"));
    out.push("rootfstype=ext2".into());
    out.push("ro".into());
    out.push(format!("init={ROOT_INIT}"));
    out.push(format!("lucina.root.uuid={uuid}"));
    if !out.iter().any(|a| a.starts_with("lucina.hostname=")) {
        out.push(format!("lucina.hostname={LAPTOP_HOSTNAME}"));
    }
    format!("{}{rest}", out.join(" "))
}

/// The root partition and image a configuration boots from, if any.
pub fn root_of(config: &str) -> Option<(String, String)> {
    let line = config.lines().find_map(|l| {
        l.trim()
            .strip_prefix("cmdline")?
            .trim_start()
            .strip_prefix('=')
    })?;
    let (args, _) = split_line(line.trim());
    let value = |key: &str| args.split_ascii_whitespace().find_map(|a| a.strip_prefix(key));
    Some((value("root=")?.to_string(), value("lucina.root.uuid=")?.to_string()))
}

/// The new configuration: the old one with the kernel, the initramfs and, if given, the command line and the pause
/// replaced. The rest stays (device tree, log, comments).
pub fn new_config(
    old: &str,
    generation: u32,
    dram_base: u64,
    cmdline: Option<&str>,
    pause_s: Option<u32>,
    quiet: Option<bool>,
) -> Result<String, String> {
    if cmdline.is_some_and(|c| c.contains(['\n', '\r'])) {
        return Err("the command line has a line break".into());
    }
    let key = |line: &str| {
        let t = line.trim();
        (!t.starts_with('#'))
            .then(|| t.split_once('=').map(|(k, _)| k.trim().to_string()))
            .flatten()
    };
    let mut out = format!("{GENERATION}{generation}\n");
    out += &format!("kernel = {} {dram_base:#x}\n", kernel_name(generation));
    out += &format!("initrd = {}\n", initrd_name(generation));
    if let Some(c) = cmdline {
        out += &format!("cmdline = {c}\n");
    }
    if let Some(s) = pause_s {
        out += &format!("pause = {s}\n");
    }
    if let Some(q) = quiet {
        out += &format!("quiet = {}\n", u8::from(q));
    }
    for line in old.lines() {
        if line.starts_with(GENERATION) || line.starts_with(PADDING) || is_fill(line) {
            continue;
        }
        match key(line).as_deref() {
            Some("kernel" | "initrd") => continue,
            Some("cmdline") if cmdline.is_some() => continue,
            Some("pause") if pause_s.is_some() => continue,
            Some("quiet") if quiet.is_some() => continue,
            _ => {
                out += line;
                out.push('\n');
            }
        }
    }
    let parsed = Config::parse(&out).map_err(|e| format!("the new BOOT.CFG is not valid: {e}"))?;
    if parsed.cmdline.is_empty() {
        return Err("the new BOOT.CFG has no command line: pass --cmdline".into());
    }
    Ok(out)
}

fn is_fill(line: &str) -> bool {
    !line.is_empty() && line.bytes().all(|b| b == b'#')
}

/// `config` as the 4096 bytes BOOT.CFG holds: its text, then comment lines that the loader skips.
pub fn config_block(config: &str) -> Result<Vec<u8>, String> {
    // A configuration read from the disk has its padding already: PREV.CFG from the second install on.
    let mut text = String::new();
    for line in config.lines() {
        if !line.starts_with(PADDING) && !is_fill(line) {
            text += line;
            text.push('\n');
        }
    }
    if !text.ends_with('\n') {
        text.push('\n');
    }
    let room = CONFIG_BLOCK
        .checked_sub(text.len() + PADDING.len() + 2)
        .ok_or_else(|| {
            format!(
                "BOOT.CFG would need {} bytes, it has {CONFIG_BLOCK}",
                text.len()
            )
        })?;
    text += PADDING;
    text.push('\n');
    text.extend(core::iter::repeat_n('#', room));
    text.push('\n');
    debug_assert_eq!(text.len(), CONFIG_BLOCK);
    Ok(text.into_bytes())
}

/// The files a configuration names, as paths from the root of the file system with `/`: relative paths are relative
/// to `dir`, the folder BOOT.CFG is in, as the loader takes them.
pub fn named_files(config: &str, dir: &str) -> Result<Vec<String>, String> {
    let parsed = Config::parse(config).map_err(|e| format!("BOOT.CFG is not valid: {e}"))?;
    let path = |p: &str| match p.strip_prefix('\\') {
        Some(absolute) => absolute.replace('\\', "/"),
        None => format!("{dir}/{}", p.replace('\\', "/")),
    };
    let mut files: Vec<String> = parsed.kernels().map(|k| path(k.path)).collect();
    files.extend(parsed.initrd.map(path));
    files.extend(parsed.dtb.map(path));
    files.dedup();
    Ok(files)
}

#[cfg(test)]
mod tests {
    use super::*;

    const OLD: &str = "# LucinaUEFI on the Samsung Galaxy Book Go\n\
        kernel = KERNEL-A0.IMG 0xa0000000\n\
        kernel = KERNEL-B0.IMG 0xb0000000\n\
        initrd = INITRAMFS.GZ\n\
        dtb = GBGO.DTB\n\
        cmdline = loglevel=info console=tty0 -- /etc/lucina/remote.sh sh -l\n\
        pause = 15\n\
        log = LOG.TXT\n";

    #[test]
    fn new_config_replaces_the_set_and_keeps_the_rest() {
        let new = new_config(OLD, 3, 0xa000_0000, Some("ufs=on -- sh -l"), None, None).unwrap();
        let c = Config::parse(&new).unwrap();
        let kernels: Vec<_> = c.kernels().map(|k| (k.path, k.dram_base)).collect();
        assert_eq!(kernels, [("K0003.IMG", 0xa000_0000)]);
        assert_eq!(c.initrd, Some("I0003.GZ"));
        assert_eq!(c.dtb, Some("GBGO.DTB"));
        assert_eq!(c.cmdline, "ufs=on -- sh -l");
        assert_eq!(c.pause_s, 15);
        assert_eq!(generation(&new), 3);
        assert!(new.contains("# LucinaUEFI on the Samsung Galaxy Book Go\n"));
        // Without --cmdline the old one stays.
        let kept = new_config(OLD, 4, 0xa000_0000, None, None, None).unwrap();
        assert_eq!(
            Config::parse(&kept).unwrap().cmdline,
            "loglevel=info console=tty0 -- /etc/lucina/remote.sh sh -l"
        );
        assert!(new_config(OLD, 4, 0, Some("a\nb"), None, None).is_err());
    }

    #[test]
    fn the_root_arguments_go_before_the_init_arguments_once() {
        let line = "PATH=/bin ufs=probe ro maxcpus=1 -- /etc/lucina/remote.sh /etc/lucina/session.sh";
        let a = with_root(line, "/dev/ufs0lun0p2", "aa");
        assert_eq!(
            a,
            "PATH=/bin maxcpus=1 ufs=on ufs.write=main root=/dev/ufs0lun0p2 rootfstype=ext2 ro init=/etc/lucina/init.sh \
             lucina.root.uuid=aa lucina.hostname=Juno001 -- /etc/lucina/remote.sh /etc/lucina/session.sh"
        );
        let b = with_root(&a, "/dev/ufs0lun0p3", "bb");
        assert_eq!(b.matches("root=").count(), 1);
        assert_eq!(b.matches(" ro ").count(), 1);
        assert_eq!(b.matches(" -- ").count(), 1);
        let config = format!("kernel = K.IMG 0x1\ncmdline = {b}\n");
        assert_eq!(root_of(&config), Some(("/dev/ufs0lun0p3".to_string(), "bb".to_string())));
        assert_eq!(root_of("cmdline = maxcpus=1 -- x root=/dev/no\n"), None);
    }

    #[test]
    fn the_laptop_gets_its_host_name_once() {
        let line = "PATH=/bin maxcpus=1 -- /etc/lucina/remote.sh lucina.hostname=init-only";
        let a = with_root(line, "/dev/ufs0lun0p2", "aa");
        let (kernel, rest) = a.split_once(" -- ").unwrap();
        assert_eq!(kernel.matches("lucina.hostname=Juno001").count(), 1);
        assert_eq!(rest, "/etc/lucina/remote.sh lucina.hostname=init-only");
        let b = with_root(&a, "/dev/ufs0lun0p3", "bb");
        assert_eq!(b.matches("lucina.hostname=").count(), 2, "{b}");
        assert_eq!(b.split_once(" -- ").unwrap().0.matches("lucina.hostname=Juno001").count(), 1);
    }

    #[test]
    fn a_given_host_name_stays() {
        let a = with_root("PATH=/bin lucina.hostname=Other maxcpus=1 -- sh", "/dev/ufs0lun0p2", "aa");
        assert!(a.starts_with("PATH=/bin lucina.hostname=Other maxcpus=1 "), "{a}");
        assert!(!a.contains("Juno001"), "{a}");
    }

    #[test]
    fn a_block_read_from_the_disk_makes_the_same_block() {
        let block = config_block(OLD).unwrap();
        let again = config_block(core::str::from_utf8(&block).unwrap()).unwrap();
        assert_eq!(block, again);
    }

    #[test]
    fn dropping_quiet_leaves_the_rest() {
        let c = "# quiet = 1 in a comment stays\nkernel = K 0x1\nquiet = 1\ncmdline = a lucina.quiet=1 -- b\n";
        assert_eq!(drop_quiet(c), "# quiet = 1 in a comment stays\nkernel = K 0x1\ncmdline = a lucina.quiet=1 -- b\n");
    }

    #[test]
    fn quiet_needs_a_loader_that_knows_it() {
        assert!(loader_knows_quiet(b"MZ..pause.verbose.quiet.pstore"));
        assert!(!loader_knows_quiet(b"MZ..pause.verbose.pstore"));
    }

    #[test]
    fn new_config_sets_quiet_once_and_keeps_it() {
        let on = new_config(OLD, 3, 0xa000_0000, None, None, Some(true)).unwrap();
        assert!(Config::parse(&on).unwrap().quiet);
        // Later installs keep the value, and a change leaves one line.
        let kept = new_config(&on, 4, 0xa000_0000, None, None, None).unwrap();
        assert!(Config::parse(&kept).unwrap().quiet);
        let off = new_config(&on, 5, 0xa000_0000, None, None, Some(false)).unwrap();
        assert!(!Config::parse(&off).unwrap().quiet);
    }

    #[test]
    fn new_config_replaces_the_pause_once() {
        let new = new_config(OLD, 3, 0xa000_0000, None, Some(0), None).unwrap();
        assert_eq!(Config::parse(&new).unwrap().pause_s, 0);
        assert_eq!(new.matches("pause").count(), 1);
        // Later installs keep it.
        let next = new_config(&new, 4, 0xa000_0000, None, None, None).unwrap();
        assert_eq!(Config::parse(&next).unwrap().pause_s, 0);
        // A value the loader refuses never reaches the disk.
        assert!(new_config(OLD, 3, 0xa000_0000, None, Some(9999), None).is_err());
    }

    #[test]
    fn the_block_is_one_block_the_loader_reads_as_the_same_config() {
        let new = new_config(OLD, 3, 0xa000_0000, Some("ufs=on -- sh -l"), None, None).unwrap();
        let block = config_block(&new).unwrap();
        assert_eq!(block.len(), CONFIG_BLOCK);
        let text = core::str::from_utf8(&block).unwrap();
        let (a, b) = (Config::parse(text).unwrap(), Config::parse(&new).unwrap());
        assert_eq!(a.cmdline, b.cmdline);
        assert_eq!(a.initrd, b.initrd);
        assert_eq!(a.kernels().count(), b.kernels().count());
        // Padding does not pile up from one install to the next.
        let next = new_config(text, 4, 0xa000_0000, None, None, None).unwrap();
        assert_eq!(next.lines().count(), new.lines().count());
        assert_eq!(config_block(&next).unwrap().len(), CONFIG_BLOCK);
        // Padding the old text changes nothing for the loader, whichever size the directory entry still says.
        let padded = config_block(OLD).unwrap();
        assert_eq!(&padded[..OLD.len()], OLD.as_bytes());
        assert!(Config::parse(core::str::from_utf8(&padded).unwrap()).is_ok());
        // Text that does not fit; a line of only # is padding and goes, so this one has a space.
        assert!(config_block(&format!("# {}", "#".repeat(CONFIG_BLOCK))).is_err());
    }

    #[test]
    fn named_files_follow_the_loader() {
        let files = named_files(OLD, "asterinas").unwrap();
        assert_eq!(
            files,
            [
                "asterinas/KERNEL-A0.IMG",
                "asterinas/KERNEL-B0.IMG",
                "asterinas/INITRAMFS.GZ",
                "asterinas/GBGO.DTB"
            ]
        );
        let abs = "kernel = \\LUCINA\\K.IMG 0x40000000\ncmdline = x\n";
        assert_eq!(named_files(abs, "asterinas").unwrap(), ["LUCINA/K.IMG"]);
    }

    #[test]
    fn only_generated_names_may_be_removed() {
        for yes in ["K0001.IMG", "k0001.img", "I9999.GZ"] {
            assert!(is_generated(yes), "{yes}");
        }
        for no in [
            "KERNEL-A0.IMG",
            "INITRAMFS.GZ",
            "BOOT.CFG",
            "BOOTAA64.EFI",
            "K1.IMG",
            "I0001.IMG",
            "GBGO.DTB",
        ] {
            assert!(!is_generated(no), "{no}");
        }
        assert_eq!(kernel_name(12), "K0012.IMG");
        assert_eq!(initrd_name(12), "I0012.GZ");
    }

    /// LUN 0 as the laptop has it, or with the entry's type or place changed.
    fn gpt_of(entries: &[([u8; 16], u64, u64)]) -> (Vec<u8>, Vec<u8>) {
        let mut array = vec![0u8; 128 * 128];
        for (i, (ty, first, last)) in entries.iter().enumerate() {
            let e = &mut array[i * 128..];
            e[0..16].copy_from_slice(ty);
            e[32..40].copy_from_slice(&first.to_le_bytes());
            e[40..48].copy_from_slice(&last.to_le_bytes());
        }
        let mut h = vec![0u8; 92];
        h[0..8].copy_from_slice(b"EFI PART");
        h[12..16].copy_from_slice(&92u32.to_le_bytes());
        h[72..80].copy_from_slice(&2u64.to_le_bytes());
        h[80..84].copy_from_slice(&128u32.to_le_bytes());
        h[84..88].copy_from_slice(&128u32.to_le_bytes());
        h[88..92].copy_from_slice(&gpt::crc32(&array).to_le_bytes());
        let crc = gpt::crc32(&h);
        h[16..20].copy_from_slice(&crc.to_le_bytes());
        h.resize(4096, 0);
        (h, array)
    }

    const ESP: [u8; 16] = [
        0x28, 0x73, 0x2a, 0xc1, 0x1f, 0xf8, 0xd2, 0x11, 0xba, 0x4b, 0x00, 0xa0, 0xc9, 0x3e, 0xc9,
        0x3b,
    ];

    #[test]
    fn only_the_expected_esp_of_a_sound_gpt_is_the_target() {
        let (h, a) = gpt_of(&[(ESP, 256, 66815)]);
        let esp = find_esp(&h, &a, LAPTOP_ESP).unwrap();
        assert_eq!((esp.index, esp.first_lba, esp.last_lba), (1, 256, 66815));
        // Elsewhere on the disk.
        let (h, a) = gpt_of(&[(ESP, 256, 66816)]);
        assert!(
            find_esp(&h, &a, LAPTOP_ESP)
                .unwrap_err()
                .contains("expected 256-66815")
        );
        // No EFI system partition: another type at the same place.
        let (h, a) = gpt_of(&[([7; 16], 256, 66815)]);
        assert!(
            find_esp(&h, &a, LAPTOP_ESP)
                .unwrap_err()
                .contains("0 EFI system")
        );
        // Two of them.
        let (h, a) = gpt_of(&[(ESP, 256, 66815), (ESP, 70000, 70100)]);
        assert!(
            find_esp(&h, &a, LAPTOP_ESP)
                .unwrap_err()
                .contains("2 EFI system")
        );
        // Damaged header and array.
        let (mut h, a) = gpt_of(&[(ESP, 256, 66815)]);
        h[40] ^= 1;
        assert!(find_esp(&h, &a, LAPTOP_ESP).unwrap_err().contains("header"));
        let (h, mut a) = gpt_of(&[(ESP, 256, 66815)]);
        a[32] ^= 1;
        assert!(find_esp(&h, &a, LAPTOP_ESP).unwrap_err().contains("array"));
        assert!(find_esp(&[0; 4096], &a, LAPTOP_ESP).is_err());
    }

    #[test]
    fn only_fat32_with_4096_byte_sectors_is_written() {
        let mut b = vec![0u8; 4096];
        b[11..13].copy_from_slice(&4096u16.to_le_bytes());
        b[82..90].copy_from_slice(b"FAT32   ");
        b[510] = 0x55;
        b[511] = 0xaa;
        assert_eq!(check_fat(&b), Ok(()));
        let mut c = b.clone();
        c[11..13].copy_from_slice(&512u16.to_le_bytes());
        assert!(check_fat(&c).unwrap_err().contains("512-byte"));
        let mut c = b.clone();
        c[82..90].copy_from_slice(b"FAT16   ");
        assert!(check_fat(&c).is_err());
        let mut c = b;
        c[510] = 0;
        assert!(check_fat(&c).is_err());
    }

    #[test]
    fn the_generation_word_is_set_once_before_the_init_arguments() {
        assert_eq!(with_generation("a lucina.generation=3 b -- /init x", 4), "a b lucina.generation=4 -- /init x");
        assert_eq!(with_generation("a", 12), "a lucina.generation=12");
        assert_eq!(with_generation("a -- lucina.generation=9", 1), "a lucina.generation=1 -- lucina.generation=9");
    }
}
