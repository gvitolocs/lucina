//! The model against `web/src/lib/drive.ts` and the real engine, the cache, the picture queue and the picker.

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use lucina_app::time::Zone;
use lucina_app::{App, Harness, Ui, keys};
use serde_json::{Value, json};

use crate::cache::{DiskCache, crc32};
use crate::drive::*;
use crate::engine::Engine;
use crate::fake::Fake;
use crate::media::{Media, MediaError};
use crate::picker::{FilePicker, Mode, Picked, Setup, bad_name, safe_name};
use crate::pictures::{Done, Fit, Pictures, Want, prepare, rotated, square_crop};
use crate::places::Place;

const NOW: i64 = 1_790_000_000_000;

fn temp(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("lucina-files-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

#[test]
fn clean_path_keeps_a_path_inside_its_folder() {
    assert_eq!(clean_path("/a//b/../c/./d"), "a/b/c/d");
    assert_eq!(clean_path(""), "");
    assert_eq!(clean_path("x\\y/z"), "z");
}

#[test]
fn same_origin_refuses_other_servers() {
    assert_eq!(same_origin(&json!("/v1/media/x")).as_deref(), Some("/v1/media/x"));
    assert_eq!(same_origin(&json!("//evil.example/x")), None);
    assert_eq!(same_origin(&json!("https://evil.example/x")), None);
    assert_eq!(same_origin(&json!(3)), None);
}

#[test]
fn an_older_server_with_missing_fields_still_parses() {
    let i = parse_item(&json!({"id": "a", "title": "report.pdf", "mime": "application/pdf"})).unwrap();
    assert_eq!(i.file_type(), DriveType::Pdf);
    assert_eq!(i.preview(), DrivePreview::Pdf);
    assert_eq!(i.name(), "report.pdf");
    assert_eq!(i.download(), "/v1/media/items/a/original?download=1");
    assert!(parse_item(&json!({"title": "no id"})).is_none());
    let i = parse_item(&json!({"id": "b", "title": "x.rs", "kind": "file"})).unwrap();
    assert_eq!(i.file_type(), DriveType::Code);
}

#[test]
fn urls_are_those_of_the_web() {
    let f = Filters { q: " two words ".into(), kind: Some(DriveType::Pdf), sort: Some(DriveSort::Size), ascending: Some(false) };
    assert_eq!(
        browse_url("Lezioni", "a b/c", &f, 200),
        "/v1/media/drive/browse?folder=Lezioni&path=a%20b%2Fc&sort=size&order=desc&type=pdf&q=two%20words&offset=200&limit=200"
    );
    assert_eq!(browse_url("L", "", &Filters::NONE, 0), "/v1/media/drive/browse?folder=L&limit=200");
    assert_eq!(items_url("x", None, Some("alb"), Some("c1")), "/v1/media/items?scope=drive&album=alb&q=x&cursor=c1&limit=200");
}

#[test]
fn bytes_and_dates_read_as_on_the_web() {
    assert_eq!(format_bytes(Some(999)), "999 B");
    assert_eq!(format_bytes(Some(1500)), "1.5 KB");
    assert_eq!(format_bytes(Some(1_000_000)), "1 MB");
    assert_eq!(format_bytes(Some(123_456_789)), "123 MB");
    assert_eq!(format_bytes(None), "");
    let zone = Zone::utc();
    assert_eq!(format_modified(Some("2026-03-04T14:03:00.000Z"), &zone), "4 Mar 2026");
    assert_eq!(format_modified(Some("nonsense"), &zone), "");
    assert_eq!(format_modified(None, &zone), "");
}

#[test]
fn preview_lines_end_with_the_rest_of_the_text_plain() {
    let highlighted = Some(vec![vec![("kw".to_owned(), "fn".to_owned())]]);
    let lines = preview_lines("fn\nsecond\nthird\n", highlighted);
    assert_eq!(lines.len(), 3);
    assert_eq!(lines[0][0].0, "kw");
    assert_eq!(lines[2], vec![(String::new(), "third".to_owned())]);
}

#[test]
fn the_cache_returns_what_it_was_given_and_refuses_damage() {
    let d = temp("cache");
    let c = DiskCache::new(d.clone(), 1 << 20);
    assert_eq!(c.read("k"), None);
    c.write("k", b"bytes");
    assert_eq!(c.read("k").as_deref(), Some(&b"bytes"[..]));
    // A power cut on ext2: the file is there with zeros in it.
    let shard = std::fs::read_dir(&d).unwrap().next().unwrap().unwrap().path();
    let file = std::fs::read_dir(&shard).unwrap().next().unwrap().unwrap().path();
    std::fs::write(&file, [0u8; 9]).unwrap();
    assert_eq!(c.read("k"), None);
    assert!(!file.exists(), "a damaged file is removed");
    // Too short to hold its check at all.
    c.write("k", b"bytes");
    std::fs::write(&file, [1u8]).unwrap();
    assert_eq!(c.read("k"), None);
    std::fs::remove_dir_all(&d).unwrap();
}

#[test]
fn the_cache_keeps_to_its_limit_oldest_first() {
    let d = temp("prune");
    let c = DiskCache::new(d.clone(), 1000);
    for i in 0..10 {
        c.write(&format!("k{i}"), &[i as u8; 200]);
    }
    c.prune();
    let total: u64 = std::fs::read_dir(&d)
        .unwrap()
        .flatten()
        .flat_map(|s| std::fs::read_dir(s.path()).unwrap().flatten().collect::<Vec<_>>())
        .map(|f| f.metadata().unwrap().len())
        .sum();
    assert!(total <= 1000, "{total}");
    assert!(c.read("k9").is_some(), "the newest stays");
    c.clear();
    assert_eq!(c.read("k9"), None);
    std::fs::remove_dir_all(&d).unwrap();
}

#[test]
fn crc_is_crc32() {
    assert_eq!(crc32(b"123456789"), 0xcbf4_3926);
}

#[test]
fn a_quarter_turn_swaps_the_sides_and_goes_clockwise() {
    let img = lucina_app::Image { w: 2, h: 1, px: vec![1, 2] };
    let r = rotated(img.clone(), 90);
    assert_eq!((r.w, r.h), (1, 2));
    assert_eq!(r.px, vec![1, 2]);
    let r = rotated(img.clone(), 270);
    assert_eq!(r.px, vec![2, 1]);
    assert_eq!(rotated(img.clone(), 180).px, vec![2, 1]);
    assert_eq!(rotated(img.clone(), 0), img);
    let tall = lucina_app::Image { w: 1, h: 3, px: vec![7, 8, 9] };
    assert_eq!(square_crop(&tall).px, vec![8]);
}

#[test]
fn a_picture_is_prepared_small_and_rubbish_is_an_error() {
    let png = crate::fake::painted(1, 400, 300);
    let t = prepare(&png, Fit::Square(64), 0).unwrap();
    assert_eq!((t.w, t.h), (64, 64));
    let t = prepare(&png, Fit::Within(100, 100), 90).unwrap();
    assert_eq!((t.w, t.h), (75, 100));
    assert!(prepare(b"rubbish", Fit::Square(64), 0).is_err());
}

fn queue(fake: &Fake, cache: Option<Arc<DiskCache>>) -> (Pictures, Arc<Mutex<Vec<Done>>>) {
    let got = Arc::new(Mutex::new(Vec::new()));
    let sink = got.clone();
    let p = Pictures::start(fake.connect(), cache, move |d| sink.lock().unwrap().push(d));
    (p, got)
}

fn want(i: u32) -> Want {
    Want { url: format!("/v1/media/items/itm_sample{i:05}/thumb?size=256&v=1"), fit: Fit::Square(48), rotation: 0 }
}

fn wait_for(what: &str, mut ok: impl FnMut() -> bool) {
    let until = Instant::now() + Duration::from_secs(20);
    while !ok() {
        assert!(Instant::now() < until, "never happened: {what}");
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn pictures_come_from_the_cache_the_second_time() {
    let d = temp("queue");
    let fake = Fake::new(NOW, 4);
    let cache = Arc::new(DiskCache::new(d.clone(), 1 << 24));
    let (p, got) = queue(&fake, Some(cache.clone()));
    p.want(vec![want(0), want(1)]);
    wait_for("two pictures", || got.lock().unwrap().len() == 2);
    assert_eq!(fake.fetches.load(Ordering::SeqCst), 2);
    got.lock().unwrap().clear();
    let (p2, got2) = queue(&fake, Some(cache));
    p2.want(vec![want(0), want(1)]);
    wait_for("the same two again", || got2.lock().unwrap().len() == 2);
    assert_eq!(fake.fetches.load(Ordering::SeqCst), 2, "no fetch for what is on the disk");
    std::fs::remove_dir_all(&d).unwrap();
}

#[test]
fn a_picture_the_server_lacks_is_said_once_and_not_asked_for_again() {
    let fake = Fake::new(NOW, 1);
    let (p, got) = queue(&fake, None);
    let missing = Want { url: "/v1/media/items/nope/thumb?size=256".into(), fit: Fit::Square(48), rotation: 0 };
    p.want(vec![missing.clone()]);
    wait_for("the failure", || got.lock().unwrap().len() == 1);
    assert!(matches!(got.lock().unwrap()[0], Done::Failed(..)));
    p.want(vec![missing]);
    std::thread::sleep(Duration::from_millis(200));
    assert_eq!(got.lock().unwrap().len(), 1);
}

#[test]
fn a_dead_token_reaches_the_app() {
    let fake = Fake::new(NOW, 1);
    fake.signed_out.store(true, Ordering::SeqCst);
    let (p, got) = queue(&fake, None);
    p.want(vec![want(0)]);
    wait_for("signed out", || got.lock().unwrap().iter().any(|d| matches!(d, Done::SignedOut)));
}

#[test]
fn a_rate_limit_waits_and_then_fetches() {
    let fake = Fake::new(NOW, 1);
    fake.rate_limited.store(true, Ordering::SeqCst);
    let (p, got) = queue(&fake, None);
    p.want(vec![want(0)]);
    std::thread::sleep(Duration::from_millis(300));
    assert!(got.lock().unwrap().is_empty());
    fake.rate_limited.store(false, Ordering::SeqCst);
    wait_for("the picture after the wait", || got.lock().unwrap().iter().any(|d| matches!(d, Done::Ready(..))));
}

/// What a caller of the picker has: an app with one open.
struct Host {
    picker: FilePicker,
    picked: Option<Picked>,
}

impl App for Host {
    type Msg = ();

    fn view(&mut self, ui: &mut Ui) {
        if self.picked.is_none() {
            self.picked = self.picker.view(ui);
        }
    }
}

fn host(mode: Mode, places: Vec<Place>, fake: Option<&Fake>, fetch_to: Option<PathBuf>) -> Harness<Host> {
    let setup = Setup { title: "Test".into(), mode, places, drive: fake.map(Fake::connect), fetch_to };
    let mut h = Harness::new(Host { picker: FilePicker::start(setup, Arc::new(|| {})), picked: None }, 800, 600);
    settle(&mut h);
    h
}

fn settle(h: &mut Harness<Host>) {
    let until = Instant::now() + Duration::from_secs(20);
    loop {
        h.core.paint();
        h.core.input(lucina_app::Input::Wheel(0));
        h.core.paint();
        if !h.app().picker.is_busy() && h.app().picked.is_none() {
            break;
        }
        if h.app().picked.is_some() || Instant::now() > until {
            break;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[test]
fn an_extension_is_matched_as_lucina_app_office_matched_it() {
    // Its `is_document`, whose rules came here with its list.
    assert_eq!(ext_of("a.DOCX"), "docx");
    assert_eq!(ext_of("b.c.xlsx"), "xlsx");
    assert_eq!(ext_of("photo.jpg"), "jpg");
    assert_eq!(ext_of("docx"), "", "a bare word has no extension");
    assert_eq!(ext_of(".docx"), "", "nor has a hidden file");
}

#[test]
fn a_drive_listing_keeps_folders_and_documents_only() {
    // `browsing_keeps_folders_and_documents` in lucina-app-office, on the picker's list.
    let d = temp("picker-browse");
    let fake = Fake::new(NOW, 1);
    let mut h = host(Mode::Open { extensions: vec!["pdf".into()] }, vec![], Some(&fake), Some(d.clone()));
    h.key(keys::KEY_DOWN); // Lezioni
    h.key(keys::KEY_ENTER);
    settle(&mut h);
    // Compilers, Digital Forensics, syllabus.pdf: no pictures, no film, no workbook.
    h.key(keys::KEY_DOWN);
    h.key(keys::KEY_DOWN);
    h.key(keys::KEY_ENTER);
    settle(&mut h);
    let Some(Picked::File(path)) = h.app().picked.clone() else { panic!("{:?}", h.app().picked) };
    assert!(path.to_string_lossy().ends_with("syllabus.pdf"), "{path:?}");
    std::fs::remove_dir_all(&d).unwrap();
}

#[test]
fn the_picker_opens_a_local_file_with_the_keyboard_alone() {
    let d = temp("picker-open");
    std::fs::write(d.join("b.docx"), "x").unwrap();
    std::fs::write(d.join("a.docx"), "x").unwrap();
    std::fs::write(d.join("photo.jpg"), "x").unwrap();
    std::fs::write(d.join(".hidden.docx"), "x").unwrap();
    std::fs::create_dir(d.join("Sub")).unwrap();
    std::fs::write(d.join("Sub/deep.docx"), "x").unwrap();
    let mode = Mode::Open { extensions: vec!["docx".into()] };
    let mut h = host(mode, vec![Place::new("Here", &d)], None, None);
    // Sub, a.docx, b.docx: the folder first, the jpg and the hidden file not at all.
    h.key(keys::KEY_DOWN);
    h.key(keys::KEY_ENTER);
    settle(&mut h);
    assert_eq!(h.app().picked, Some(Picked::File(d.join("a.docx"))));
    std::fs::remove_dir_all(&d).unwrap();
}

#[test]
fn the_picker_goes_into_folders_and_back_but_not_above_its_place() {
    let d = temp("picker-folders");
    std::fs::create_dir(d.join("Sub")).unwrap();
    std::fs::write(d.join("Sub/deep.txt"), "x").unwrap();
    let mut h = host(Mode::Open { extensions: vec![] }, vec![Place::new("Here", &d)], None, None);
    h.key(keys::KEY_ENTER);
    settle(&mut h);
    h.key(keys::KEY_BACKSPACE);
    settle(&mut h);
    h.key(keys::KEY_BACKSPACE);
    settle(&mut h);
    // Still here: Enter goes back into Sub, and Enter on its only file picks it.
    h.key(keys::KEY_ENTER);
    settle(&mut h);
    h.key(keys::KEY_ENTER);
    settle(&mut h);
    assert_eq!(h.app().picked, Some(Picked::File(d.join("Sub/deep.txt"))));
    std::fs::remove_dir_all(&d).unwrap();
}

#[test]
fn saving_asks_before_replacing_and_refuses_a_bad_name() {
    let d = temp("picker-save");
    std::fs::write(d.join("export.json"), "old").unwrap();
    let mut h = host(Mode::Save { name: "new.json".into() }, vec![Place::new("Here", &d)], None, None);
    assert_eq!(bad_name("a/b"), Some("A name can't have a / in it."));
    assert_eq!(bad_name(""), Some("Type a name for the file."));
    assert_eq!(bad_name(".x"), Some("A name can't start with a dot."));
    assert_eq!(bad_name("ok.json"), None);
    h.app().picker.set_name_for_test("export.json");
    h.app().picker.save_for_test();
    settle(&mut h);
    assert_eq!(h.app().picked, None, "it asks first");
    assert!(h.app().picker.asks_to_replace());
    h.key(keys::KEY_ESC);
    settle(&mut h);
    assert!(!h.app().picker.asks_to_replace());
    assert_eq!(h.app().picked, None, "Escape keeps the file and stays");
    h.app().picker.set_name_for_test("new.json");
    h.app().picker.save_for_test();
    settle(&mut h);
    std::fs::remove_dir_all(&d).unwrap();
}

#[test]
fn saving_into_a_folder_not_made_yet_makes_it() {
    let d = temp("picker-mkdir");
    let dest = d.join("Downloads");
    let mut h = host(Mode::Save { name: "x.txt".into() }, vec![Place::new("Downloads", &dest)], None, None);
    let got = h.app().picker.save_for_test();
    assert_eq!(got, Some(Picked::File(dest.join("x.txt"))));
    assert!(dest.is_dir());
    std::fs::remove_dir_all(&d).unwrap();
}

#[test]
fn a_file_from_the_drive_arrives_as_a_local_path() {
    let d = temp("picker-drive");
    let fake = Fake::new(NOW, 1);
    let mut h = host(Mode::Open { extensions: vec!["xlsx".into()] }, vec![], Some(&fake), Some(d.clone()));
    // The roots: CraftIT, Lezioni. In CraftIT only the workbook is listed.
    h.key(keys::KEY_ENTER);
    settle(&mut h);
    h.key(keys::KEY_ENTER);
    settle(&mut h);
    let Some(Picked::File(path)) = h.app().picked.clone() else { panic!("{:?}", h.app().picked) };
    assert!(path.starts_with(&d));
    assert!(path.to_string_lossy().ends_with("invoices-2026.xlsx"));
    assert!(path.is_file());
    assert!(std::fs::read_dir(&d).unwrap().flatten().all(|e| !e.file_name().to_string_lossy().ends_with(".part")));
    std::fs::remove_dir_all(&d).unwrap();
}

#[test]
fn a_name_from_the_server_stays_in_its_folder() {
    assert_eq!(safe_name("../../etc/passwd"), "passwd");
    assert_eq!(safe_name("..\\..\\x"), "x");
    assert_eq!(safe_name(".."), "file");
    assert_eq!(safe_name(".bashrc"), "bashrc");
    assert_eq!(safe_name("ok name.pdf"), "ok name.pdf");
}

#[test]
fn the_drive_signed_out_says_so_and_does_not_crash() {
    let fake = Fake::new(NOW, 1);
    fake.signed_out.store(true, Ordering::SeqCst);
    let mut h = host(Mode::Open { extensions: vec![] }, vec![], Some(&fake), None);
    settle(&mut h);
    assert_eq!(h.app().picker.error_for_test().as_deref(), Some("Sign in to Secretary to see the Drive."));
}

// ---- The real engine.

fn keys_of(v: &Value) -> BTreeSet<String> {
    v.as_object().map(|o| o.keys().cloned().collect()).unwrap_or_default()
}

#[test]
fn the_sample_library_answers_in_the_real_servers_shape() {
    let Some(engine) = Engine::start("shape") else {
        eprintln!("skipped: no lucina-media here");
        return;
    };
    let mut real = (engine.connect())().unwrap();
    let mut fake = Fake::new(NOW, 4);
    for path in [
        "/v1/media/drive",
        "/v1/media/drive/browse?folder=Lezioni&limit=200",
        "/v1/media/items?scope=gallery&origin=all&kind=photo,video&limit=2",
        "/v1/media/sources",
        "/v1/media/albums?scope=gallery",
        "/v1/media/items/months?origin=all&kind=photo,video",
    ] {
        let (r, f) = (real.get(path).unwrap(), fake.get(path).unwrap());
        let (rk, fk) = (keys_of(&r), keys_of(&f));
        // The fake may answer null where the real one left a key out, but nothing the real one sends may be missing from it.
        assert!(rk.is_subset(&fk) || fk.is_subset(&rk), "{path}: real {rk:?}, fake {fk:?}");
    }
    let real_page = real.get("/v1/media/items?scope=gallery&origin=all&kind=photo,video&limit=1").unwrap();
    let fake_page = fake.get("/v1/media/items?scope=gallery&origin=all&kind=photo,video&limit=1").unwrap();
    let (r, f) = (keys_of(&real_page["items"][0]), keys_of(&fake_page["items"][0]));
    assert_eq!(r.difference(&f).collect::<Vec<_>>(), Vec::<&String>::new(), "keys the fake lacks");
    let real_browse = real.get("/v1/media/drive/browse?folder=Lezioni&limit=200").unwrap();
    let fake_browse = fake.get("/v1/media/drive/browse?folder=Lezioni&limit=200").unwrap();
    let (r, f) = (keys_of(&real_browse["files"][0]), keys_of(&fake_browse["files"][0]));
    assert_eq!(r.difference(&f).collect::<Vec<_>>(), Vec::<&String>::new(), "keys the fake lacks");
    assert_eq!(
        keys_of(&real_browse["files"][0]["drive"]).difference(&keys_of(&fake_browse["files"][0]["drive"])).collect::<Vec<_>>(),
        Vec::<&String>::new()
    );
}

#[test]
fn the_model_reads_the_real_servers_drive() {
    let Some(engine) = Engine::start("drive") else {
        eprintln!("skipped: no lucina-media here");
        return;
    };
    let mut m = (engine.connect())().unwrap();
    let roots = parse_roots(&m.get("/v1/media/drive").unwrap());
    assert_eq!(roots.folders.iter().map(|f| f.folder.as_str()).collect::<Vec<_>>(), ["Lezioni"]);
    assert_eq!(roots.folders[0].files, Some(5));
    let page = parse_browse(&m.get(&browse_url("Lezioni", "", &Filters::NONE, 0)).unwrap(), "Lezioni", "");
    assert_eq!(page.breadcrumbs, vec![Crumb { name: "Lezioni".into(), path: String::new() }]);
    assert_eq!(page.folders.iter().map(|f| f.path.as_str()).collect::<Vec<_>>(), ["Sub"]);
    let name = |n: &str| page.files.iter().find(|f| f.name() == n).unwrap_or_else(|| panic!("{n}")).clone();
    let notes = name("notes.txt");
    assert_eq!(notes.file_type(), DriveType::Text);
    assert_eq!(notes.preview(), DrivePreview::Text);
    assert_eq!(name("a.zip").preview(), DrivePreview::Archive);
    assert_eq!(name("board.png").preview(), DrivePreview::Image);
    assert_eq!(name("data.bin").file_type(), DriveType::Other);
    // Sub.
    let sub = parse_browse(&m.get(&browse_url("Lezioni", "Sub", &Filters::NONE, 0)).unwrap(), "Lezioni", "Sub");
    assert_eq!(sub.breadcrumbs.len(), 2);
    let code = &sub.files[0];
    assert_eq!(code.file_type(), DriveType::Code);
    // Text is read in the background: 202 while it is not ready.
    let url = format!("{}?highlight=1", code.urls.text.clone().unwrap());
    let mut text = TextPreview::Pending;
    engine.wait_for("text extraction", || {
        text = parse_text_preview(&m.get(&url).unwrap());
        text != TextPreview::Pending
    });
    let TextPreview::Ready { text: body, lines, language, .. } = text else { unreachable!() };
    assert!(body.contains("println!"));
    assert_eq!(language.as_deref(), Some("Rust"));
    assert!(preview_lines(&body, lines).iter().flatten().any(|t| !t.0.is_empty()), "highlighted");
    // An archive's listing.
    let zip = name("a.zip");
    let listing = parse_archive(&m.get(&zip.urls.archive.clone().unwrap()).unwrap());
    assert_eq!(listing.entries.iter().map(|e| e.path.as_str()).collect::<Vec<_>>(), ["inside.txt"]);
    // A download, a piece at a time.
    let mut out = Vec::new();
    let n = m.download(&name("data.bin").download(), &mut out, &mut |_| true).unwrap();
    assert_eq!((n, out), (5, vec![1, 2, 3, 4, 5]));
    // A file that is not there.
    assert_eq!(m.get("/v1/media/items/itm_nope").unwrap_err().status(), Some(404));
    assert!(matches!(m.get("/v1/media/items/itm_nope"), Err(MediaError::Refused { status: 404, .. })));
}

#[test]
fn the_picker_fetches_from_the_real_servers_drive() {
    let Some(engine) = Engine::start("picker") else {
        eprintln!("skipped: no lucina-media here");
        return;
    };
    let d = temp("picker-engine");
    let setup = Setup {
        title: "Open".into(),
        mode: Mode::Open { extensions: vec!["bin".into()] },
        places: vec![],
        drive: Some(engine.connect()),
        fetch_to: Some(d.clone()),
    };
    let mut h = Harness::new(Host { picker: FilePicker::start(setup, Arc::new(|| {})), picked: None }, 800, 600);
    settle(&mut h);
    h.key(keys::KEY_ENTER); // Lezioni
    settle(&mut h);
    // Sub, then data.bin: the only .bin.
    h.key(keys::KEY_DOWN);
    h.key(keys::KEY_ENTER);
    settle(&mut h);
    let Some(Picked::File(path)) = h.app().picked.clone() else { panic!("{:?}", h.app().picked) };
    assert_eq!(std::fs::read(&path).unwrap(), vec![1, 2, 3, 4, 5]);
    std::fs::remove_dir_all(&d).unwrap();
}

fn alt_left(h: &mut Harness<Host>) {
    h.press(keys::KEY_LEFT, keys::Mods { alt: true, ..keys::Mods::default() });
    settle(h);
}

#[test]
fn in_the_picker_the_arrows_stop_at_the_ends_and_alt_left_goes_up() {
    let d = temp("picker-arrows");
    std::fs::create_dir(d.join("Sub")).unwrap();
    std::fs::write(d.join("Sub/deep.txt"), "x").unwrap();
    std::fs::write(d.join("a.txt"), "x").unwrap();
    std::fs::write(d.join("b.txt"), "x").unwrap();
    let mut h = host(Mode::Open { extensions: vec![] }, vec![Place::new("Here", &d)], None, None);
    // Sub, a.txt, b.txt. Down past the last and Up above the first stay on them: no wrap, and the focus stays on the list.
    h.key(keys::KEY_UP);
    h.key(keys::KEY_UP);
    h.key(keys::KEY_END);
    h.key(keys::KEY_DOWN);
    h.key(keys::KEY_DOWN);
    // Left goes to the places and Right comes back: the list still has the keyboard.
    h.key(keys::KEY_RIGHT);
    h.key(keys::KEY_LEFT);
    h.key(keys::KEY_RIGHT);
    h.key(keys::KEY_ENTER);
    settle(&mut h);
    assert_eq!(h.app().picked, Some(Picked::File(d.join("b.txt"))), "the last file");
    std::fs::remove_dir_all(&d).unwrap();

    let d = temp("picker-arrows-up");
    std::fs::create_dir(d.join("Sub")).unwrap();
    std::fs::write(d.join("Sub/deep.txt"), "x").unwrap();
    std::fs::write(d.join("z.txt"), "x").unwrap();
    let mut h = host(Mode::Open { extensions: vec![] }, vec![Place::new("Here", &d)], None, None);
    h.key(keys::KEY_HOME);
    h.key(keys::KEY_ENTER); // Sub
    settle(&mut h);
    alt_left(&mut h);
    h.key(keys::KEY_END);
    h.key(keys::KEY_ENTER);
    settle(&mut h);
    assert_eq!(h.app().picked, Some(Picked::File(d.join("z.txt"))), "Alt+Left came back up to the folder");
    std::fs::remove_dir_all(&d).unwrap();
}
