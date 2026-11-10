//! The board file parser on a built file and, on the machine that has it, on Debian's `board-2.bin`.

use lucina_ath10k::board;

fn ie(id: u32, data: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&id.to_le_bytes());
    out.extend_from_slice(&(data.len() as u32).to_le_bytes());
    out.extend_from_slice(data);
    while !out.len().is_multiple_of(4) {
        out.push(0);
    }
    out
}

fn file() -> Vec<u8> {
    let mut f = board::MAGIC.to_vec();
    while !f.len().is_multiple_of(4) {
        f.push(0);
    }
    let one = [ie(board::IE_BOARD_NAME, b"bus=snoc,qmi-board-id=ff"), ie(board::IE_BOARD_DATA, b"first")].concat();
    let two = [
        ie(board::IE_BOARD_NAME, b"bus=snoc,qmi-board-id=a,qmi-chip-id=1"),
        ie(board::IE_BOARD_NAME, b"bus=snoc,qmi-board-id=a"),
        ie(board::IE_BOARD_DATA, b"second"),
    ]
    .concat();
    f.extend(ie(board::IE_BOARD, &one));
    f.extend(ie(board::IE_BOARD, &two));
    f
}

#[test]
fn finds_a_board_by_exact_name() {
    let f = file();
    assert_eq!(board::find(&f, "bus=snoc,qmi-board-id=ff"), Some(&b"first"[..]));
    assert_eq!(board::find(&f, "bus=snoc,qmi-board-id=a"), Some(&b"second"[..]));
    assert_eq!(board::find(&f, "bus=snoc,qmi-board-id=a,qmi-chip-id=1"), Some(&b"second"[..]));
    assert_eq!(board::find(&f, "bus=snoc,qmi-board-id=b"), None);
    assert_eq!(board::names(&f).count(), 3);
}

#[test]
fn bad_files_find_nothing() {
    let f = file();
    assert_eq!(board::find(&f[1..], "bus=snoc,qmi-board-id=ff"), None);
    for cut in 0..f.len() {
        let _ = board::find(&f[..cut], "bus=snoc,qmi-board-id=a");
        let _ = board::names(&f[..cut]).count();
    }
}

#[test]
fn the_real_board_file_parses() {
    let path = std::env::var("HOME").unwrap() + "/scratch/gbgo/fw/ath10k/board-2.bin";
    let Ok(f) = std::fs::read(&path) else {
        eprintln!("skipped: {path} is not here");
        return;
    };
    let names: Vec<_> = board::names(&f).map(|n| String::from_utf8_lossy(n).into_owned()).collect();
    assert!(!names.is_empty());
    for name in &names {
        assert!(name.starts_with("bus=snoc,qmi-board-id="), "{name}");
        assert!(!board::find(&f, name).unwrap().is_empty());
    }
    eprintln!("{} boards, first {}", names.len(), names[0]);
}
