//! `lucina-accountd` and its link, against the fake server and real sockets and files in a folder of their own.
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use lucina_account::fake::{Fake, TOKEN};
use lucina_account::service::{start, Config, Running};
use lucina_account::system::{Link, Pairing, Status};
use lucina_account::Token;

fn folder(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("accountd-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn service(root: &Path, keep: bool, fake: &Fake) -> Running {
    let mut cfg = Config::under(root, keep);
    cfg.base = Some(fake.base().to_owned());
    cfg.confirm_retry = Duration::from_millis(200);
    start(cfg).unwrap()
}

fn wait(what: &str, mut f: impl FnMut() -> bool) {
    let until = Instant::now() + Duration::from_secs(10);
    while !f() {
        assert!(Instant::now() < until, "timed out: {what}");
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn signed_in(l: &Link) -> bool {
    matches!(l.status(), Ok(Status::SignedIn { .. }))
}

fn token_files(root: &Path) -> (PathBuf, PathBuf) {
    (root.join("run/lucina/secretary/credential"), root.join("data/lucina/secretary/credential"))
}

fn sign_in(l: &Link, fake: &Fake) {
    let _w = l.watch(true).unwrap();
    wait("a code", || fake.qr_requests() >= 1);
    fake.approve_all();
    wait("signed in", || signed_in(l));
}

/// Plants a token as an earlier boot would have.
fn plant(path: &Path) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, TOKEN).unwrap();
}

fn data_up(root: &Path) {
    let run = root.join("run/lucina");
    std::fs::create_dir_all(&run).unwrap();
    std::fs::write(run.join("data-expected"), "").unwrap();
    std::fs::write(run.join("data-ready"), "").unwrap();
}

#[test]
fn every_program_shares_one_code_and_one_sign_in() {
    let (root, fake) = (folder("one"), Fake::start());
    let s = service(&root, false, &fake);
    let (a, b) = (Link::at(s.socket()), Link::at(s.socket()));
    wait("settled", || a.status().unwrap() == Status::SignedOut);
    assert!(a.token().is_none());

    let (mut wa, mut wb) = (a.watch(true).unwrap(), b.watch(true).unwrap());
    let ready = |w: &mut lucina_account_link::Watch| loop {
        match w.next(Duration::from_secs(5)).unwrap() {
            Some(Status::SigningIn { pairing: Pairing::Ready { approve_url, short_code, .. } }) => return (approve_url, short_code),
            Some(_) => {}
            None => panic!("no code"),
        }
    };
    let (ca, cb) = (ready(&mut wa), ready(&mut wb));
    assert_eq!(ca, cb, "both screens show the same code");
    assert_eq!(fake.qr_requests(), 1, "one code for the machine");
    assert_eq!(fake.qr_device_names(), [Some("Juno (Galaxy Book Go)".to_owned())]);

    fake.approve_all();
    let done = |w: &mut lucina_account_link::Watch| loop {
        if let Some(Status::SignedIn { .. }) = w.next(Duration::from_secs(5)).unwrap() {
            return;
        }
    };
    done(&mut wa);
    done(&mut wb);
    assert_eq!(a.token(), Token::parse(TOKEN));
    assert_eq!(b.token(), Token::parse(TOKEN));
    assert_eq!(fake.qr_requests(), 1);
}

#[test]
fn the_token_is_only_in_the_answer_to_token() {
    let (root, fake) = (folder("quiet"), Fake::start());
    let s = service(&root, false, &fake);
    let l = Link::at(s.socket());
    let mut w = l.watch(true).unwrap();
    wait("a code", || fake.qr_requests() >= 1);
    fake.approve_all();
    wait("signed in", || signed_in(&l));
    let status = serde_json::to_string(&l.status().unwrap()).unwrap();
    assert!(!status.contains(TOKEN), "{status}");
    while let Some(st) = w.next(Duration::from_millis(50)).unwrap() {
        assert!(!serde_json::to_string(&st).unwrap().contains(TOKEN));
    }
    // The poll token stays in the service too.
    assert!(!status.contains("poll1"));
}

#[test]
fn nothing_goes_to_disk_unless_the_owner_said_so() {
    let (root, fake) = (folder("ram"), Fake::start());
    data_up(&root);
    let s = service(&root, false, &fake);
    let l = Link::at(s.socket());
    sign_in(&l, &fake);
    let (ram, disk) = token_files(&root);
    assert!(ram.exists());
    assert!(!disk.exists(), "RAM only without the switch");
    assert_eq!(std::fs::metadata(&ram).unwrap().permissions().mode() & 0o777, 0o600);
    assert!(matches!(l.status().unwrap(), Status::SignedIn { on_disk: false, .. }));
}

#[test]
fn a_token_left_on_disk_goes_away_when_the_switch_is_off() {
    let (root, fake) = (folder("leftover"), Fake::start());
    data_up(&root);
    let (_, disk) = token_files(&root);
    plant(&disk);
    let s = service(&root, false, &fake);
    let l = Link::at(s.socket());
    wait("settled", || l.status().unwrap() == Status::SignedOut);
    assert!(!disk.exists());
    assert!(l.token().is_none(), "and it does not sign the machine in");
}

#[test]
fn with_the_switch_on_the_token_is_there_after_a_restart() {
    let (root, fake) = (folder("disk"), Fake::start());
    data_up(&root);
    {
        let s = service(&root, true, &fake);
        let l = Link::at(s.socket());
        sign_in(&l, &fake);
        wait("on disk", || matches!(l.status(), Ok(Status::SignedIn { on_disk: true, .. })));
    }
    let (ram, disk) = token_files(&root);
    assert_eq!(std::fs::metadata(&disk).unwrap().permissions().mode() & 0o777, 0o600);
    assert_eq!(std::fs::metadata(disk.parent().unwrap()).unwrap().permissions().mode() & 0o777, 0o700);
    // A restart: RAM is empty, the socket's service is gone.
    std::fs::remove_file(&ram).unwrap();
    std::fs::remove_file(root.join("run/lucina/account.sock")).unwrap();
    let requests = fake.qr_requests();
    let s = service(&root, true, &fake);
    let l = Link::at(s.socket());
    wait("signed in from disk", || signed_in(&l));
    assert_eq!(l.token(), Token::parse(TOKEN));
    assert_eq!(fake.qr_requests(), requests, "no new device");
}

#[test]
fn it_waits_for_data_before_it_offers_a_code() {
    let (root, fake) = (folder("wait"), Fake::start());
    let run = root.join("run/lucina");
    std::fs::create_dir_all(&run).unwrap();
    std::fs::write(run.join("data-expected"), "").unwrap();
    let (_, disk) = token_files(&root);
    plant(&disk);
    let s = service(&root, true, &fake);
    let l = Link::at(s.socket());
    let _w = l.watch(true).unwrap();
    std::thread::sleep(Duration::from_millis(400));
    assert_eq!(l.status().unwrap(), Status::Starting);
    assert_eq!(fake.qr_requests(), 0, "a kept token may be about to appear");
    std::fs::write(run.join("data-ready"), "").unwrap();
    wait("signed in from disk", || signed_in(&l));
    assert_eq!(fake.qr_requests(), 0);
}

#[test]
fn a_failed_data_keeps_the_sign_in_in_ram() {
    let (root, fake) = (folder("failed"), Fake::start());
    let run = root.join("run/lucina");
    std::fs::create_dir_all(&run).unwrap();
    std::fs::write(run.join("data-expected"), "").unwrap();
    std::fs::write(run.join("data-failed"), "").unwrap();
    let s = service(&root, true, &fake);
    let l = Link::at(s.socket());
    sign_in(&l, &fake);
    let (ram, disk) = token_files(&root);
    assert!(ram.exists() && !disk.exists());
    assert!(matches!(l.status().unwrap(), Status::SignedIn { on_disk: false, .. }));
}

#[test]
fn a_restart_of_the_service_keeps_the_sign_in() {
    let (root, fake) = (folder("restart"), Fake::start());
    let (ram, _) = token_files(&root);
    plant(&ram);
    let s = service(&root, false, &fake);
    let l = Link::at(s.socket());
    wait("signed in from RAM", || signed_in(&l));
    assert_eq!(fake.qr_requests(), 0);
}

#[test]
fn a_program_with_a_stale_401_does_not_sign_the_machine_out() {
    let (root, fake) = (folder("stale"), Fake::start());
    let s = service(&root, false, &fake);
    let l = Link::at(s.socket());
    sign_in(&l, &fake);
    l.report_rejected().unwrap();
    assert_eq!(l.status().unwrap(), Status::Checking, "held back while it asks");
    wait("kept", || signed_in(&l));
    assert_eq!(l.token(), Token::parse(TOKEN));
}

#[test]
fn a_revoked_token_signs_the_machine_out_and_clears_what_apps_cached() {
    let (root, fake) = (folder("revoked"), Fake::start());
    let s = service(&root, false, &fake);
    let l = Link::at(s.socket());
    sign_in(&l, &fake);
    let (ram, _) = token_files(&root);
    let cache = ram.with_file_name("timeline-cache");
    std::fs::write(&cache, "messages").unwrap();
    fake.revoke();
    l.report_rejected().unwrap();
    wait("signed out", || l.status().unwrap() == Status::SignedOut);
    assert!(!ram.exists() && !cache.exists());
    assert!(l.token().is_none());
}

#[test]
fn sign_out_is_for_everyone_and_clears_the_disk() {
    let (root, fake) = (folder("out"), Fake::start());
    data_up(&root);
    let s = service(&root, true, &fake);
    let l = Link::at(s.socket());
    sign_in(&l, &fake);
    wait("on disk", || matches!(l.status(), Ok(Status::SignedIn { on_disk: true, .. })));
    let mut w = Link::at(s.socket()).watch(false).unwrap();
    l.sign_out().unwrap();
    let (ram, disk) = token_files(&root);
    assert!(!ram.exists() && !disk.exists());
    assert!(l.token().is_none());
    loop {
        if w.next(Duration::from_secs(5)).unwrap() == Some(Status::SignedOut) {
            break;
        }
    }
}

#[test]
fn no_screen_no_new_codes() {
    let (root, fake) = (folder("idle"), Fake::start());
    let s = service(&root, false, &fake);
    let l = Link::at(s.socket());
    wait("settled", || l.status().unwrap() == Status::SignedOut);
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(fake.qr_requests(), 0, "nobody asked");

    fake.set_qr_ttl(1);
    let w = l.watch(true).unwrap();
    wait("a code", || fake.qr_requests() == 1);
    drop(w);
    // The code on the screen runs out; then it stops.
    wait("the pairing ends", || l.status().unwrap() == Status::SignedOut);
    let n = fake.qr_requests();
    std::thread::sleep(Duration::from_millis(1500));
    assert_eq!(fake.qr_requests(), n);
}

#[test]
fn an_approval_after_the_screen_closed_is_still_collected() {
    let (root, fake) = (folder("late"), Fake::start());
    let s = service(&root, false, &fake);
    let l = Link::at(s.socket());
    let w = l.watch(true).unwrap();
    wait("a code", || fake.qr_requests() == 1);
    drop(w);
    std::thread::sleep(Duration::from_millis(100));
    fake.approve_all();
    wait("signed in", || signed_in(&l));
}

#[test]
fn a_second_service_refuses_to_start() {
    let (root, fake) = (folder("two"), Fake::start());
    let _s = service(&root, false, &fake);
    let mut cfg = Config::under(&root, false);
    cfg.base = Some(fake.base().to_owned());
    assert!(start(cfg).is_err());
}

#[test]
fn the_socket_is_for_root_only() {
    let (root, fake) = (folder("mode"), Fake::start());
    let s = service(&root, false, &fake);
    assert_eq!(std::fs::metadata(s.socket()).unwrap().permissions().mode() & 0o777, 0o600);
}

#[test]
fn without_the_service_a_program_is_just_signed_out() {
    let l = Link::at(folder("none").join("nothing.sock"));
    assert!(l.token().is_none());
    assert!(l.status().is_err());
}

#[test]
fn a_damaged_token_on_disk_means_signed_out_and_is_removed() {
    for (name, content) in [("garbage", "not a token at all".to_owned()), ("cut", TOKEN[..20].to_owned()), ("empty", String::new())] {
        let (root, fake) = (folder(&format!("damaged-{name}")), Fake::start());
        data_up(&root);
        let (_, disk) = token_files(&root);
        std::fs::create_dir_all(disk.parent().unwrap()).unwrap();
        std::fs::write(&disk, &content).unwrap();
        let s = service(&root, true, &fake);
        let l = Link::at(s.socket());
        wait("settled", || l.status().unwrap() == Status::SignedOut);
        assert!(!disk.exists(), "{name}: the damaged file goes");
        assert!(l.token().is_none());
        // And the next approval writes a whole one.
        sign_in(&l, &fake);
        wait("on disk", || matches!(l.status(), Ok(Status::SignedIn { on_disk: true, .. })));
        assert_eq!(std::fs::read_to_string(&disk).unwrap(), TOKEN);
    }
}

#[test]
fn a_damaged_token_in_ram_is_removed_too() {
    let (root, fake) = (folder("damaged-ram"), Fake::start());
    let (ram, _) = token_files(&root);
    std::fs::create_dir_all(ram.parent().unwrap()).unwrap();
    std::fs::write(&ram, "sec_short").unwrap();
    let s = service(&root, true, &fake);
    let l = Link::at(s.socket());
    wait("settled", || l.status().unwrap() == Status::SignedOut);
    assert!(!ram.exists());
}

#[test]
fn a_token_revoked_while_the_machine_was_off_signs_it_out_at_boot() {
    let (root, fake) = (folder("revoked-off"), Fake::start());
    data_up(&root);
    let (ram, disk) = token_files(&root);
    plant(&disk);
    fake.revoke();
    let s = service(&root, true, &fake);
    let l = Link::at(s.socket());
    wait("signed out", || l.status().unwrap() == Status::SignedOut);
    assert!(!disk.exists() && !ram.exists());
    assert_eq!(fake.qr_requests(), 0, "and no code until a screen asks");
}

#[test]
fn a_kept_token_stays_while_the_server_cannot_be_reached_and_is_checked_when_it_can() {
    let root = folder("offline");
    data_up(&root);
    let (_, disk) = token_files(&root);
    plant(&disk);
    // A port nobody listens on yet: the network is not up.
    let port = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
    let mut cfg = Config::under(&root, true);
    cfg.base = Some(format!("http://127.0.0.1:{port}"));
    cfg.confirm_retry = Duration::from_millis(200);
    let s = start(cfg).unwrap();
    let l = Link::at(s.socket());
    wait("signed in from disk", || signed_in(&l));
    std::thread::sleep(Duration::from_millis(700));
    assert!(signed_in(&l), "no answer is not a no");
    assert_eq!(l.token(), Token::parse(TOKEN));
    // The server comes up, and it has revoked the device meanwhile.
    let fake = Fake::start_on(port);
    fake.revoke();
    wait("signed out once the server answers", || l.status().unwrap() == Status::SignedOut);
    assert!(!disk.exists());
}

#[test]
fn a_kept_token_the_server_still_takes_stays() {
    let (root, fake) = (folder("kept-good"), Fake::start());
    data_up(&root);
    let (_, disk) = token_files(&root);
    plant(&disk);
    let s = service(&root, true, &fake);
    let l = Link::at(s.socket());
    wait("named by the server", || matches!(l.status(), Ok(Status::SignedIn { device_name: Some(_), on_disk: true, .. })));
    assert!(disk.exists());
    assert_eq!(fake.qr_requests(), 0);
}

fn now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs()
}

/// What Lucina reads to know which sign-in is active, with no token in it.
fn times(l: &Link) -> (Option<String>, Option<u64>, Option<u64>) {
    match l.status().unwrap() {
        Status::SignedIn { device_id, paired_at, confirmed_at, .. } => (device_id, paired_at, confirmed_at),
        other => panic!("not signed in: {other:?}"),
    }
}

#[test]
fn the_status_says_which_device_since_when_and_when_the_server_last_agreed() {
    let (root, fake) = (folder("times"), Fake::start());
    data_up(&root);
    let s = service(&root, true, &fake);
    let l = Link::at(s.socket());
    let before = now();
    sign_in(&l, &fake);
    wait("named by the server", || times(&l).0.is_some());
    let (device, paired, confirmed) = times(&l);
    assert_eq!(device.as_deref(), Some(lucina_account::fake::DEVICE_ID));
    assert!(paired.unwrap() >= before && paired.unwrap() <= now());
    assert!(confirmed.unwrap() >= paired.unwrap());
    let text = serde_json::to_string(&l.status().unwrap()).unwrap();
    assert!(!text.contains(TOKEN), "no token in the status");
    l.sign_out().unwrap();
    wait("signed out", || l.status().unwrap() == Status::SignedOut);
}

#[test]
fn a_kept_token_keeps_its_pairing_time_across_restarts() {
    let (root, fake) = (folder("times-kept"), Fake::start());
    data_up(&root);
    let (ram, disk) = token_files(&root);
    plant(&disk);
    let paired = now() - 86_400;
    let at = std::time::UNIX_EPOCH + Duration::from_secs(paired);
    std::fs::File::options().write(true).open(&disk).unwrap().set_modified(at).unwrap();
    {
        let s = service(&root, true, &fake);
        let l = Link::at(s.socket());
        wait("checked with the server", || signed_in(&l) && times(&l).2.is_some());
        let (device, p, confirmed) = times(&l);
        assert_eq!(p, Some(paired), "the file's time, not the boot's");
        assert!(confirmed.unwrap() >= now() - 5);
        assert_eq!(device.as_deref(), Some(lucina_account::fake::DEVICE_ID));
    }
    // The service restarts with the copy in RAM, which is newer than the one on /data.
    assert!(ram.exists());
    std::fs::remove_file(root.join("run/lucina/account.sock")).unwrap();
    let s = service(&root, true, &fake);
    let l = Link::at(s.socket());
    wait("signed in, on disk", || matches!(l.status(), Ok(Status::SignedIn { on_disk: true, .. })));
    assert_eq!(times(&l).1, Some(paired));
    let mtime = std::fs::metadata(&disk).unwrap().modified().unwrap();
    assert_eq!(mtime, at, "a file that already holds the token is not rewritten");
}
