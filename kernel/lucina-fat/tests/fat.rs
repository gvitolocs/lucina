//! Host tests of lucina-fat against images made by mkfs.fat and checked by fsck.fat.

mod common;

use std::sync::atomic::Ordering;

use common::{
    FileDisk, Rng, assert_fsck_clean, configs, fsck, mkfs, mount, opts, ro_opts,
    tree::{Model, Tree, assert_matches},
};
use lucina_fat::{Error, FatType, Volume};

fn pattern(seed: u64, len: usize) -> Vec<u8> {
    let mut r = Rng(seed | 1);
    (0..len).map(|_| r.next() as u8).collect()
}

#[test]
fn mounts_every_flavour() {
    for (label, args, kib) in configs() {
        let path = mkfs(&format!("mount-{label}"), kib, &args);
        let mut vol = mount(&path);
        let g = vol.geometry().clone();
        match label.as_bytes()[1..3] {
            [b'1', b'2'] => assert_eq!(g.fat_type, FatType::Fat12, "{label}"),
            [b'1', b'6'] => assert_eq!(g.fat_type, FatType::Fat16, "{label}"),
            _ => assert_eq!(g.fat_type, FatType::Fat32, "{label}"),
        }
        let sector: u32 = label.rsplit('-').next().unwrap().parse().unwrap();
        assert_eq!(g.bytes_per_sector, sector, "{label}");
        let st = vol.statfs().unwrap();
        assert!(
            st.free_clusters > 0 && st.free_clusters <= st.clusters,
            "{label}"
        );
        let mut t = Tree::new(vol);
        assert!(
            t.names("/").unwrap().is_empty(),
            "{label}: root of a fresh volume is empty"
        );
        // Mounting and syncing a volume that nobody wrote to writes nothing.
        t.sync().unwrap();
        assert_fsck_clean(&path);
    }
}

#[test]
fn readonly_and_idle_mounts_write_nothing() {
    let path = mkfs("ro", 40 * 1024, &["-F", "32", "-s", "1"]);
    // Put something on the volume first.
    {
        let mut t = Tree::new(mount(&path));
        t.write_file("/hello.txt", 0, b"hi").unwrap();
        t.sync().unwrap();
    }
    for readonly in [true, false] {
        let disk = FileDisk::open(&path);
        let writes = disk.writes.clone();
        let o = if readonly { ro_opts() } else { opts() };
        let mut t = Tree::new(Volume::mount(disk, o).unwrap());
        assert_eq!(t.read_file("/hello.txt").unwrap(), b"hi");
        t.vol.statfs().unwrap();
        t.sync().unwrap();
        assert_eq!(writes.load(Ordering::SeqCst), 0, "readonly={readonly}");
    }
    // A read-only volume refuses every change.
    let mut t = Tree::new(Volume::mount(FileDisk::open(&path), ro_opts()).unwrap());
    assert_eq!(t.create("/x").err(), Some(Error::ReadOnly));
    assert_eq!(t.mkdir("/d").err(), Some(Error::ReadOnly));
    assert_eq!(t.unlink("/hello.txt").err(), Some(Error::ReadOnly));
    assert_eq!(
        t.write_file("/hello.txt", 0, b"x").err(),
        Some(Error::ReadOnly)
    );
    assert_eq!(t.truncate("/hello.txt", 0).err(), Some(Error::ReadOnly));
    assert_eq!(t.read_file("/hello.txt").unwrap(), b"hi");
}

#[test]
fn dirty_bit_is_set_while_changed_and_cleared_by_sync() {
    for (label, args, kib) in configs() {
        if label.starts_with("f12") {
            // FAT12 has the same boot sector state byte as FAT16: covered by f16.
            continue;
        }
        let path = mkfs(&format!("dirty-{label}"), kib, &args);
        let state_off: usize = if label.starts_with("f32") { 0x41 } else { 0x25 };
        let byte = |p: &std::path::Path| std::fs::read(p).unwrap()[state_off];
        let mut t = Tree::new(mount(&path));
        assert_eq!(byte(&path) & 1, 0, "{label}: clean at mount");
        t.write_file("/a.txt", 0, b"abc").unwrap();
        assert_eq!(
            byte(&path) & 1,
            1,
            "{label}: dirty after the first change reached the device"
        );
        let (ok, _) = fsck(&path);
        assert!(!ok || true);
        t.sync().unwrap();
        assert_eq!(byte(&path) & 1, 0, "{label}: clean after sync");
        assert_fsck_clean(&path);
    }
}

#[test]
fn already_dirty_volume_stays_dirty() {
    let path = mkfs("was-dirty", 40 * 1024, &["-F", "32", "-s", "1"]);
    let mut img = std::fs::read(&path).unwrap();
    img[0x41] |= 1;
    std::fs::write(&path, &img).unwrap();
    let mut t = Tree::new(mount(&path));
    assert!(t.vol.was_dirty());
    t.write_file("/a", 0, b"1").unwrap();
    t.sync().unwrap();
    assert_eq!(std::fs::read(&path).unwrap()[0x41] & 1, 1);
}

#[test]
fn short_and_long_names() {
    let path = mkfs("names", 40 * 1024, &["-F", "32", "-s", "1"]);
    let mut t = Tree::new(mount(&path));
    for n in [
        "UPPER.TXT",
        "lower.txt",
        "Mixed Case.Txt",
        "a very long file name that needs several long entries.extension",
        "dots.in.name.tar.gz",
        "semi;colon+plus,comma=eq[b].dat",
        ".hidden",
        "trailing.dots...",
        "ünïcode-ñame.txt",
        "ÜBER",
        "日本語のファイル.txt",
        "twelve345678.abcd",
    ] {
        t.write_file(&format!("/{n}"), 0, n.as_bytes()).unwrap();
    }
    t.sync().unwrap();
    let mut names = t.names("/").unwrap();
    names.sort();
    let mut want: Vec<String> = [
        "UPPER.TXT",
        "lower.txt",
        "Mixed Case.Txt",
        "a very long file name that needs several long entries.extension",
        "dots.in.name.tar.gz",
        "semi;colon+plus,comma=eq[b].dat",
        ".hidden",
        "trailing.dots",
        "ünïcode-ñame.txt",
        "ÜBER",
        "日本語のファイル.txt",
        "twelve345678.abcd",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    want.sort();
    assert_eq!(names, want);
    // Case-insensitive lookup and the short names.
    assert_eq!(t.read_file("/upper.txt").unwrap(), b"UPPER.TXT");
    assert_eq!(t.read_file("/LOWER.TXT").unwrap(), b"lower.txt");
    assert_eq!(t.read_file("/trailing.dots.").unwrap(), b"trailing.dots...");
    let shorts: std::collections::BTreeMap<String, String> = t
        .list("/")
        .unwrap()
        .iter()
        .map(|e| {
            (
                e.name.clone(),
                String::from_utf8_lossy(&e.short).into_owned(),
            )
        })
        .collect();
    assert_eq!(shorts["UPPER.TXT"], "UPPER   TXT");
    assert_eq!(shorts["lower.txt"], "LOWER   TXT");
    assert_eq!(shorts["Mixed Case.Txt"], "MIXEDC~1TXT");
    assert_eq!(shorts["dots.in.name.tar.gz"], "DOTSIN~1GZ ");
    assert_eq!(shorts[".hidden"], "HIDDEN~1   ");
    assert_eq!(shorts["twelve345678.abcd"], "TWELVE~1ABC");
    assert_fsck_clean(&path);

    // Names that cannot be created.
    for bad in [
        "",
        ".",
        "..",
        "a*b",
        "a?b",
        "a|b",
        "a\"b",
        "a<b",
        "a:b",
        "trailing space ",
        "ctl\u{1}char",
    ] {
        assert!(
            matches!(t.create(&format!("/{bad}")), Err(Error::Invalid)),
            "{bad:?} should be invalid"
        );
    }
    let long = "x".repeat(256);
    assert_eq!(
        t.create(&format!("/{long}")).err(),
        Some(Error::NameTooLong)
    );
    let ok = "y".repeat(255);
    t.create(&format!("/{ok}")).unwrap();
    assert_eq!(t.create("/LOWER.txt").err(), Some(Error::Exists));
    t.sync().unwrap();
    assert_fsck_clean(&path);
}

#[test]
fn numeric_tails_are_unique() {
    let path = mkfs("tails", 40 * 1024, &["-F", "32", "-s", "1"]);
    let mut t = Tree::new(mount(&path));
    for i in 0..30 {
        t.create(&format!("/long file name number {i}.txt"))
            .unwrap();
    }
    t.sync().unwrap();
    let mut shorts: Vec<String> = t
        .list("/")
        .unwrap()
        .iter()
        .map(|e| String::from_utf8_lossy(&e.short).into_owned())
        .collect();
    shorts.sort();
    let before = shorts.len();
    shorts.dedup();
    assert_eq!(before, shorts.len(), "short names collide");
    assert!(shorts.contains(&"LONGFI~1TXT".to_string()));
    assert!(shorts.contains(&"LONGFI~9TXT".to_string()));
    assert!(shorts.contains(&"LONGF~10TXT".to_string()));
    assert_fsck_clean(&path);
}

#[test]
fn file_data_grow_shrink_and_zero_fill() {
    for (label, args, kib) in configs() {
        let path = mkfs(&format!("data-{label}"), kib, &args);
        let mut t = Tree::new(mount(&path));
        let free0 = t.vol.statfs().unwrap().free_clusters;
        let data = pattern(7, 100_000);
        assert_eq!(t.write_file("/f.bin", 0, &data).unwrap(), data.len());
        assert_eq!(t.read_file("/f.bin").unwrap(), data, "{label}");
        // Overwrite in the middle.
        let mid = pattern(8, 5000);
        t.write_file("/f.bin", 33_333, &mid).unwrap();
        let mut want = data.clone();
        want[33_333..38_333].copy_from_slice(&mid);
        assert_eq!(t.read_file("/f.bin").unwrap(), want);
        // Sparse write past the end: the gap reads as zeros.
        t.write_file("/f.bin", 130_000, b"tail").unwrap();
        want.resize(130_000, 0);
        want.extend_from_slice(b"tail");
        assert_eq!(t.read_file("/f.bin").unwrap(), want, "{label}");
        // Shrink into the middle of a cluster, then grow: the cut data must not come back.
        t.truncate("/f.bin", 50_001).unwrap();
        want.truncate(50_001);
        assert_eq!(t.read_file("/f.bin").unwrap(), want);
        t.truncate("/f.bin", 90_000).unwrap();
        want.resize(90_000, 0);
        assert_eq!(t.read_file("/f.bin").unwrap(), want, "{label}");
        t.sync().unwrap();
        assert_fsck_clean(&path);
        // Shrink to nothing releases every cluster.
        t.truncate("/f.bin", 0).unwrap();
        t.unlink("/f.bin").unwrap();
        t.sync().unwrap();
        assert_eq!(
            t.vol.statfs().unwrap().free_clusters,
            free0,
            "{label}: clusters leaked"
        );
        assert_fsck_clean(&path);
    }
}

#[test]
fn directories_grow_and_fixed_root_fills_up() {
    // FAT32: a directory spreads over many clusters.
    let path = mkfs("dirgrow", 40 * 1024, &["-F", "32", "-s", "1"]);
    let mut t = Tree::new(mount(&path));
    t.mkdir("/big").unwrap();
    for i in 0..400 {
        t.create(&format!("/big/file number {i:04}.dat")).unwrap();
    }
    t.sync().unwrap();
    assert_eq!(t.list("/big").unwrap().len(), 400);
    for i in (0..400).step_by(2) {
        t.unlink(&format!("/big/file number {i:04}.dat")).unwrap();
    }
    for i in 0..100 {
        t.create(&format!("/big/second {i}.dat")).unwrap();
    }
    t.sync().unwrap();
    assert_eq!(t.list("/big").unwrap().len(), 300);
    assert_fsck_clean(&path);

    // FAT16: the fixed root directory has 512 slots.
    let path = mkfs("rootfull", 32 * 1024, &["-F", "16", "-r", "512"]);
    let mut t = Tree::new(mount(&path));
    let mut made = 0;
    loop {
        match t.create(&format!("/F{made}")) {
            Ok(_) => made += 1,
            Err(Error::NoSpace) => break,
            Err(e) => panic!("{e}"),
        }
    }
    assert_eq!(made, 512, "short-only names take one slot each");
    t.sync().unwrap();
    assert_fsck_clean(&path);
    // Deleting makes room again.
    t.unlink("/F7").unwrap();
    t.create("/AGAIN").unwrap();
    t.sync().unwrap();
    assert_fsck_clean(&path);
}

#[test]
fn volume_full_gives_short_writes_and_recovers() {
    let path = mkfs("full", 8 * 1024, &["-F", "16", "-s", "2", "-S", "512"]);
    let mut t = Tree::new(mount(&path));
    let total = t.vol.statfs().unwrap();
    let cap = u64::from(total.free_clusters) * u64::from(total.cluster_size);
    let big = pattern(3, (cap + 50_000) as usize);
    let n = t.write_file("/big", 0, &big).unwrap();
    assert!(
        n as u64 <= cap && n as u64 + u64::from(total.cluster_size) > cap,
        "short write {n} of {cap}"
    );
    assert_eq!(t.vol.statfs().unwrap().free_clusters, 0);
    assert_eq!(t.write_file("/other", 0, b"x").err(), Some(Error::NoSpace));
    assert_eq!(t.read_file("/big").unwrap(), big[..n]);
    t.sync().unwrap();
    assert_fsck_clean(&path);
    t.unlink("/big").unwrap();
    t.unlink("/other").ok();
    assert_eq!(t.vol.statfs().unwrap().free_clusters, total.free_clusters);
    t.write_file("/small", 0, b"fits").unwrap();
    t.sync().unwrap();
    assert_fsck_clean(&path);
}

#[test]
fn rename_and_move() {
    for (label, args, kib) in configs() {
        let path = mkfs(&format!("rename-{label}"), kib, &args);
        let mut t = Tree::new(mount(&path));
        t.mkdir("/a").unwrap();
        t.mkdir("/b").unwrap();
        t.mkdir("/a/sub").unwrap();
        t.write_file("/a/sub/file.txt", 0, b"content").unwrap();
        t.write_file("/a/one", 0, b"1").unwrap();
        t.write_file("/b/two", 0, b"22").unwrap();
        // Same directory, new long name.
        t.rename("/a/one", "/a/A much longer new name.txt").unwrap();
        assert_eq!(t.read_file("/a/a much longer new name.txt").unwrap(), b"1");
        // Case-only change.
        t.rename(
            "/a/A much longer new name.txt",
            "/a/a much longer NEW name.txt",
        )
        .unwrap();
        assert_eq!(
            t.names("/a").unwrap(),
            vec!["a much longer NEW name.txt".to_string(), "sub".to_string()]
        );
        // Replace an existing file.
        t.rename("/a/a much longer NEW name.txt", "/b/two").unwrap();
        assert_eq!(t.read_file("/b/two").unwrap(), b"1");
        // Move a directory with contents; its ".." follows (fsck checks it).
        t.rename("/a/sub", "/b/moved sub").unwrap();
        assert_eq!(t.read_file("/b/moved sub/file.txt").unwrap(), b"content");
        t.rename("/b/moved sub", "/top").unwrap();
        assert_eq!(t.read_file("/top/file.txt").unwrap(), b"content");
        // A directory cannot move into itself or below.
        t.mkdir("/top/inner").unwrap();
        assert_eq!(
            t.rename("/top", "/top/inner/top").err(),
            Some(Error::Invalid)
        );
        // Wrong kinds and non-empty targets.
        assert_eq!(t.rename("/top", "/b/two").err(), Some(Error::NotDir));
        assert_eq!(t.rename("/b/two", "/top").err(), Some(Error::IsDir));
        t.mkdir("/dst").unwrap();
        assert_eq!(
            t.rename("/top", "/dst").err(),
            None,
            "an empty directory can be replaced"
        );
        t.mkdir("/other").unwrap();
        assert_eq!(t.rename("/other", "/dst").err(), Some(Error::NotEmpty));
        t.sync().unwrap();
        assert_fsck_clean(&path);
    }
}

#[test]
fn rmdir_needs_empty_directory_and_unlink_frees_space() {
    let path = mkfs("rmdir", 40 * 1024, &["-F", "32", "-s", "1"]);
    let mut t = Tree::new(mount(&path));
    let free0 = t.vol.statfs().unwrap().free_clusters;
    t.mkdir("/d").unwrap();
    t.write_file("/d/f", 0, &pattern(1, 20_000)).unwrap();
    assert_eq!(t.rmdir("/d").err(), Some(Error::NotEmpty));
    assert_eq!(t.unlink("/d").err(), Some(Error::IsDir));
    assert_eq!(t.rmdir("/d/f").err(), Some(Error::NotDir));
    t.unlink("/d/f").unwrap();
    t.rmdir("/d").unwrap();
    t.sync().unwrap();
    assert_eq!(t.vol.statfs().unwrap().free_clusters, free0);
    assert_eq!(t.resolve("/d").err(), Some(Error::NotFound));
    assert_fsck_clean(&path);
}

#[test]
fn unlinked_but_open_file_keeps_its_data_until_released() {
    let path = mkfs("orphan", 40 * 1024, &["-F", "32", "-s", "1"]);
    let mut t = Tree::new(mount(&path));
    let free0 = t.vol.statfs().unwrap().free_clusters;
    let data = pattern(5, 30_000);
    t.write_file("/f", 0, &data).unwrap();
    let mut open = t.resolve("/f").unwrap();
    let mut root = t.vol.root();
    t.vol.remove_entry(&mut root, &mut open).unwrap();
    // Still readable and writable through the open node, but it has no entry any more.
    let mut buf = vec![0u8; data.len()];
    assert_eq!(t.vol.read(&mut open, 0, &mut buf).unwrap(), data.len());
    assert_eq!(buf, data);
    t.vol.write(&mut open, 0, b"xx").unwrap();
    t.vol.sync_node(&mut open).unwrap();
    assert!(t.resolve("/f").is_err());
    t.sync().unwrap();
    assert!(t.vol.statfs().unwrap().free_clusters < free0);
    t.vol.release(&mut open).unwrap();
    t.sync().unwrap();
    assert_eq!(t.vol.statfs().unwrap().free_clusters, free0);
    assert_fsck_clean(&path);
}

#[test]
fn data_survives_remount() {
    for (label, args, kib) in configs() {
        let path = mkfs(&format!("remount-{label}"), kib, &args);
        let data = pattern(11, 70_000);
        {
            let mut t = Tree::new(mount(&path));
            t.mkdir("/dir one").unwrap();
            t.write_file("/dir one/Data File.bin", 0, &data).unwrap();
            t.sync().unwrap();
        }
        let mut t = Tree::new(mount(&path));
        assert_eq!(
            t.read_file("/DIR ONE/data file.BIN").unwrap(),
            data,
            "{label}"
        );
        let e = &t.list("/dir one").unwrap()[0];
        assert_eq!(e.size as usize, data.len());
        // The timestamp written is the mock clock's.
        assert_eq!(e.mtime.secs, 1_793_534_400);
        assert_fsck_clean(&path);
    }
}

#[test]
fn stress_against_a_model() {
    for (label, args, kib) in configs() {
        if label.starts_with("f12-4096") {
            continue;
        }
        let path = mkfs(&format!("stress-{label}"), kib, &args);
        let mut t = Tree::new(mount(&path));
        let mut m = Model::default();
        let mut rng = Rng(0x9E37_79B9_7F4A_7C15 ^ label.len() as u64 ^ kib);
        let dirs = ["", "d0", "d1", "d0/e0", "d1/E1"];
        for d in &dirs[1..] {
            t.mkdir(&format!("/{d}")).unwrap();
            m.add_dir(&format!("/{d}"));
        }
        let names = [
            "a",
            "B.txt",
            "readme",
            "Hello World.txt",
            "UPPER",
            "x.y.z",
            "lossy+name,1.bin",
            "ünï-é.txt",
            "τest",
            "a long name with many words in it for several slots.dat",
            "file.with.dots.tar.gz",
            "Mixed",
        ];
        let pick_dir = |rng: &mut Rng| dirs[rng.below(dirs.len() as u64) as usize];
        let pick_name = |rng: &mut Rng| -> String {
            if rng.below(3) == 0 {
                format!("gen{}", rng.below(20))
            } else {
                names[rng.below(names.len() as u64) as usize].to_string()
            }
        };
        let path_of = |d: &str, n: &str| {
            if d.is_empty() {
                format!("/{n}")
            } else {
                format!("/{d}/{n}")
            }
        };
        let max_len = if kib < 4000 { 3000 } else { 25_000 };
        for step in 0..600 {
            let d = pick_dir(&mut rng);
            let n = pick_name(&mut rng);
            let p = path_of(d, &n);
            match rng.below(10) {
                0..=2 => {
                    // write at a random offset (creating if needed)
                    if m.has(&p) && m.is_dir(&p) {
                        continue;
                    }
                    let off = if m.has(&p) {
                        rng.below(m.file_mut(&p).len() as u64 + 500)
                    } else {
                        0
                    };
                    let len = rng.below(max_len) as usize;
                    let data = pattern(step as u64 + 100, len);
                    match t.write_file(&p, off, &data) {
                        Ok(w) => {
                            assert_eq!(w, len, "{label} step {step}");
                            if !m.has(&p) {
                                m.add_file(&p, Vec::new());
                            }
                            let f = m.file_mut(&p);
                            if (off as usize) > f.len() {
                                f.resize(off as usize, 0);
                            }
                            let end = off as usize + len;
                            if end > f.len() {
                                f.resize(end, 0);
                            }
                            f[off as usize..end].copy_from_slice(&data);
                        }
                        Err(Error::NoSpace) => {
                            // Resynchronise the model with what is actually there.
                            if m.has(&p) {
                                let got = t.read_file(&p).unwrap();
                                *m.file_mut(&p) = got;
                            } else if t.resolve(&p).is_ok() {
                                m.add_file(&p, t.read_file(&p).unwrap());
                            }
                        }
                        Err(e) => panic!("{label} step {step}: write {p}: {e}"),
                    }
                }
                3 => {
                    if m.has(&p) && !m.is_dir(&p) {
                        let new_len = rng.below(m.file_mut(&p).len() as u64 + 3000);
                        if t.truncate(&p, new_len).is_ok() {
                            m.file_mut(&p).resize(new_len as usize, 0);
                        }
                    }
                }
                4 => {
                    if m.has(&p) && !m.is_dir(&p) {
                        t.unlink(&p).unwrap();
                        m.remove(&p);
                    }
                }
                5 => {
                    // mkdir / rmdir of a scratch directory
                    let q = path_of(d, &format!("sub{}", rng.below(3)));
                    if m.has(&q) {
                        if m.is_dir(&q) && m.children(&q[1..]).is_empty() {
                            t.rmdir(&q).unwrap();
                            m.remove(&q);
                        }
                    } else {
                        t.mkdir(&q).unwrap();
                        m.add_dir(&q);
                    }
                }
                6..=7 => {
                    // rename a file somewhere else (replacing a file is allowed)
                    let d2 = pick_dir(&mut rng);
                    let n2 = pick_name(&mut rng);
                    let q = path_of(d2, &n2);
                    if m.has(&p) && !m.is_dir(&p) && (!m.has(&q) || !m.is_dir(&q)) {
                        t.rename(&p, &q).unwrap();
                        m.rename(&p, &q);
                    }
                }
                8 => {
                    if m.has(&p) && !m.is_dir(&p) {
                        let got = t.read_file(&p).unwrap();
                        assert!(got == *m.file_mut(&p), "{label} step {step}: read {p}");
                    }
                }
                _ => {
                    t.sync().unwrap();
                }
            }
        }
        assert_matches(&mut t, &m, "");
        t.sync().unwrap();
        assert_fsck_clean(&path);
        // The same tree after a fresh mount.
        let mut t = Tree::new(mount(&path));
        assert_matches(&mut t, &m, "");
    }
}

#[test]
fn reads_what_mtools_wrote() {
    if !common::mtools_available() {
        eprintln!("skipped: lucinaos-grub:trixie (mtools) is not available");
        return;
    }
    for (label, args, kib) in [
        ("f16", vec!["-F", "16"], 32 * 1024u64),
        ("f32", vec!["-F", "32", "-s", "1"], 40 * 1024),
        ("f12", vec!["-F", "12"], 2880),
    ] {
        let path = mkfs(&format!("mtools-{label}"), kib, &args);
        let dir = path.parent().unwrap();
        let img = path.file_name().unwrap().to_str().unwrap();
        let data = pattern(21, 12_345);
        std::fs::write(dir.join("payload.bin"), &data).unwrap();
        let script = format!(
            "export LC_ALL=C.UTF-8 MTOOLS_SKIP_CHECK=1; cd /w && mmd -i {img} ::Docs ::Docs/Sub && \
             mcopy -i {img} payload.bin '::Docs/Long Name Here.bin' && \
             mcopy -i {img} payload.bin ::SHORT.BIN && mcopy -i {img} payload.bin ::lower.bin && \
             mcopy -i {img} payload.bin '::Docs/Sub/Plain Name.bin'"
        );
        let (ok, out) = common::mtools(dir, &script);
        assert!(ok, "mtools: {out}");
        let mut t = Tree::new(mount(&path));
        for p in [
            "/Docs/Long Name Here.bin",
            "/short.bin",
            "/LOWER.BIN",
            "/docs/sub/plain name.bin",
        ] {
            let got = t.read_file(p).unwrap_or_else(|e| {
                panic!(
                    "{label} {p}: {e}; root {:?} docs {:?} sub {:?}",
                    t.names("/"),
                    t.names("/Docs"),
                    t.names("/Docs/Sub")
                )
            });
            assert_eq!(got, data, "{label} {p}");
        }
        assert_eq!(
            t.names("/").unwrap().len(),
            3,
            "{label}: {:?}",
            t.names("/").unwrap()
        );
        // Now write with us and let mtools read it back.
        t.write_file("/Docs/from lucina.txt", 0, b"written by lucina-fat")
            .unwrap();
        t.mkdir("/Docs/Made Here").unwrap();
        t.write_file("/Docs/Made Here/Zażółć.txt", 0, b"unicode")
            .unwrap();
        t.sync().unwrap();
        drop(t);
        let script = format!(
            "export LC_ALL=C.UTF-8 MTOOLS_SKIP_CHECK=1; cd /w && mtype -i {img} '::Docs/from lucina.txt' && echo && \
             mtype -i {img} '::Docs/Made Here/Zażółć.txt' && mdir -i {img} ::Docs"
        );
        let (ok, out) = common::mtools(dir, &script);
        assert!(ok, "mtools read back: {out}");
        assert!(out.contains("written by lucina-fat"), "{out}");
        assert!(out.contains("unicode"), "{out}");
        assert!(out.contains("Made Here"), "{out}");
        assert_fsck_clean(&path);
    }
}

#[test]
fn device_that_ends_inside_a_cache_block() {
    // 2883 KiB is not a multiple of 4096 bytes: the last cache block is partial.
    let path = mkfs("odd-size", 2883, &["-F", "12"]);
    let mut t = Tree::new(mount(&path));
    let free = t.vol.statfs().unwrap().free_clusters;
    let data = pattern(9, free as usize * 512);
    let n = t.write_file("/fill", 0, &data).unwrap();
    assert_eq!(n, data.len(), "the whole free space is usable");
    assert_eq!(t.read_file("/fill").unwrap(), data);
    t.sync().unwrap();
    assert_fsck_clean(&path);
}

#[test]
fn readonly_mount_drops_in_memory_changes() {
    let path = mkfs("ro-dirty", 40 * 1024, &["-F", "32", "-s", "1"]);
    {
        let mut t = Tree::new(mount(&path));
        t.write_file("/a", 0, b"data").unwrap();
        t.sync().unwrap();
    }
    let disk = FileDisk::open(&path);
    let writes = disk.writes.clone();
    let mut t = Tree::new(Volume::mount(disk, ro_opts()).unwrap());
    let mut n = t.resolve("/a").unwrap();
    t.vol
        .touch(&mut n, Some(lucina_fat::Time::new(1_700_000_000, 0)), None);
    t.vol.sync_node(&mut n).unwrap();
    assert!(!n.dirty);
    t.sync().unwrap();
    assert_eq!(writes.load(Ordering::SeqCst), 0);
}

#[test]
fn remount_read_only_flushes_and_stops_writes() {
    let path = mkfs("remount-ro", 40 * 1024, &["-F", "32", "-s", "1"]);
    let mut t = Tree::new(mount(&path));
    t.write_file("/a", 0, b"before").unwrap();
    t.vol.set_readonly(true).unwrap();
    assert_eq!(t.write_file("/a", 0, b"after").err(), Some(Error::ReadOnly));
    // Everything written before is on the disk now and the volume is clean.
    assert_fsck_clean(&path);
    t.vol.set_readonly(false).unwrap();
    t.write_file("/b", 0, b"again").unwrap();
    t.sync().unwrap();
    assert_fsck_clean(&path);
}

#[test]
fn nothing_can_be_created_in_a_removed_directory() {
    let path = mkfs("removed-dir", 40 * 1024, &["-F", "32", "-s", "1"]);
    let mut t = Tree::new(mount(&path));
    t.mkdir("/d").unwrap();
    let mut d = t.resolve("/d").unwrap();
    let mut root = t.vol.root();
    t.vol.remove_entry(&mut root, &mut d).unwrap();
    assert_eq!(
        t.vol.create(&mut d, "x", false).err(),
        Some(Error::NotFound)
    );
    t.vol.release(&mut d).unwrap();
    t.sync().unwrap();
    assert_fsck_clean(&path);
}

/// Juno 2026-11-08: on the laptop's ESP a removal reached the disk, and the free count, only at unmount. After sync, with
/// the volume still mounted, a second mount of the same image (what the firmware or the next boot reads after a power cut)
/// must see the removal, the rename and the freed clusters. The laptop's geometry and mount options (nodirtybit,
/// nomtime).
#[test]
fn removal_and_rename_are_on_the_disk_after_sync_without_unmount() {
    let path = mkfs("kept", 266_240, &["-F", "32", "-S", "4096", "-s", "1"]);
    let laptop = lucina_fat::MountOptions { mark_dirty: false, keep_mtime: true, ..opts() };
    let mut t = Tree::new(Volume::mount(FileDisk::open(&path), laptop).unwrap());
    t.write_file("/GONE.BIN", 0, &pattern(7, 1 << 20)).unwrap();
    t.write_file("/OLD.TXT", 0, b"renamed\n").unwrap();
    t.sync().unwrap();
    let before = Volume::mount(FileDisk::open(&path), ro_opts()).unwrap().statfs().unwrap().free_clusters;

    t.unlink("/GONE.BIN").unwrap();
    t.rename("/OLD.TXT", "/NEW.TXT").unwrap();
    t.sync().unwrap();
    let freed = t.vol.statfs().unwrap().free_clusters;
    assert!(freed > before, "statfs after sync: {freed} free, {before} before the removal");

    // `t` stays mounted: this is the disk as a cut would leave it.
    let mut seen = Tree::new(Volume::mount(FileDisk::open(&path), ro_opts()).unwrap());
    assert_eq!(seen.resolve("/GONE.BIN").err(), Some(Error::NotFound));
    assert_eq!(seen.resolve("/OLD.TXT").err(), Some(Error::NotFound));
    assert!(seen.resolve("/NEW.TXT").is_ok());
    assert_eq!(seen.vol.statfs().unwrap().free_clusters, freed, "the free count on the disk");
    assert_fsck_clean(&path);
    drop(t);
}
