//! What `lucina-install-disk netinstall` decides, as pure functions: which targets a board may write, whether a manifest is
//! acceptable here, the installed sequence file, and the typed yes. Nothing here reads a disk or the network.

use lucina_release::{Manifest, rules};

/// Where the installed manifest's sequence number is kept on the ESP.
pub const SEQ_FILE: &str = "lucinaos/netinst.seq";

/// The targets this installer may write for a board, compiled in. A manifest can only narrow this. Every other board has none:
/// it is refused. Nothing ever names another LUN (the SC7180 boards keep the Qualcomm firmware on LUNs 1-5).
pub fn compiled_targets(board: &str) -> &'static [&'static str] {
    match board {
        "galaxy-book-go" => &["lun0:esp", "lun0:root-a", "lun0:root-b"],
        // The QEMU proof's board (scripts/qemu-net-install.sh): the same three targets, and only in a build made for it.
        #[cfg(feature = "qemu-test-board")]
        "qemu-virt-test" => &["lun0:esp", "lun0:root-a", "lun0:root-b"],
        _ => &[],
    }
}

/// Whether a verified manifest may be installed on this board here. `installed` is the sequence on the target ESP, if any.
pub fn check_manifest(
    m: &Manifest,
    board: &str,
    installed: Option<u64>,
    go_back: bool,
) -> Result<(), String> {
    let compiled = compiled_targets(board);
    if compiled.is_empty() {
        return Err(format!(
            "board {board}: this installer has no write rule for it"
        ));
    }
    if m.board != board {
        return Err(format!(
            "the manifest is for board {}, not {board}",
            m.board
        ));
    }
    for target in &m.writes {
        if !rules::target_allowed(compiled.contains(&target.as_str()), true) {
            return Err(format!(
                "the manifest asks to write {target}, which {board} may not"
            ));
        }
    }
    if !rules::seq_allowed(installed, m.seq, go_back) {
        return Err(format!(
            "sequence {} is lower than the installed {} (--go-back allows it)",
            m.seq,
            installed.unwrap_or(0)
        ));
    }
    Ok(())
}

pub fn seq_text(seq: u64) -> String {
    format!("{seq}\n")
}

/// The sequence file's content: one decimal number and a newline.
pub fn parse_seq(text: &str) -> Option<u64> {
    let n = text.strip_suffix('\n')?;
    if n.is_empty() || n.len() > 20 || !n.bytes().all(|c| c.is_ascii_digit()) {
        return None;
    }
    n.parse().ok()
}

/// A disk without the Lucina layout lacks a root-a or a root-b partition.
pub fn needs_layout(has_root_a: bool, has_root_b: bool) -> bool {
    !(has_root_a && has_root_b)
}

/// The typed answer: the word `yes` and nothing else but the line's end.
pub fn yes_typed(line: &str) -> bool {
    line.strip_suffix('\n')
        .unwrap_or(line)
        .trim_end_matches('\r')
        == "yes"
}

#[cfg(test)]
mod tests {
    use super::*;

    fn manifest(board: &str, seq: u64, writes: &[&str]) -> Manifest {
        let mut m = Manifest::make(
            board,
            &"a".repeat(40),
            "2026-11-11T11:39:59Z",
            seq,
            lucina_release::MIN_CHUNK,
            &[7u8; 10][..],
        )
        .unwrap();
        m.writes = writes.iter().map(|s| s.to_string()).collect();
        m
    }

    #[test]
    fn compiled_set() {
        assert_eq!(
            compiled_targets("galaxy-book-go"),
            ["lun0:esp", "lun0:root-a", "lun0:root-b"]
        );
        assert!(compiled_targets("lenovo-duet").is_empty());
        assert!(compiled_targets("").is_empty());
        for t in compiled_targets("galaxy-book-go") {
            assert!(t.starts_with("lun0:"));
        }
    }

    /// The default build knows no QEMU board: a manifest for it is refused like any other unknown board.
    #[test]
    #[cfg(not(feature = "qemu-test-board"))]
    fn qemu_test_board_refused_without_its_feature() {
        assert!(compiled_targets("qemu-virt-test").is_empty());
        assert!(
            check_manifest(
                &manifest("qemu-virt-test", 1, &[]),
                "qemu-virt-test",
                None,
                false
            )
            .is_err()
        );
    }

    #[test]
    #[cfg(feature = "qemu-test-board")]
    fn qemu_test_board_with_its_feature() {
        assert_eq!(
            compiled_targets("qemu-virt-test"),
            ["lun0:esp", "lun0:root-a", "lun0:root-b"]
        );
        assert!(compiled_targets("galaxy-book-go").len() == 3);
        assert!(compiled_targets("qemu-virt-test-2").is_empty());
        assert!(
            check_manifest(
                &manifest("qemu-virt-test", 1, &["lun1:x"]),
                "qemu-virt-test",
                None,
                false
            )
            .is_err()
        );
    }

    #[test]
    fn manifests() {
        let b = "galaxy-book-go";
        assert!(
            check_manifest(
                &manifest(b, 5, &["lun0:esp", "lun0:root-a"]),
                b,
                None,
                false
            )
            .is_ok()
        );
        assert!(check_manifest(&manifest(b, 5, &[]), b, Some(5), false).is_ok());
        assert!(check_manifest(&manifest(b, 5, &["lun1:x"]), b, None, false).is_err());
        assert!(
            check_manifest(&manifest(b, 5, &["lun0:esp", "lun0:data"]), b, None, true).is_err()
        );
        assert!(check_manifest(&manifest("other", 5, &[]), b, None, false).is_err());
        assert!(check_manifest(&manifest("other", 5, &[]), "other", None, false).is_err());
        assert!(check_manifest(&manifest(b, 4, &[]), b, Some(5), false).is_err());
        assert!(check_manifest(&manifest(b, 4, &[]), b, Some(5), true).is_ok());
    }

    #[test]
    fn seq_file() {
        assert_eq!(parse_seq(&seq_text(0)), Some(0));
        assert_eq!(parse_seq(&seq_text(u64::MAX)), Some(u64::MAX));
        for bad in [
            "",
            "\n",
            "5",
            "5\n\n",
            " 5\n",
            "-1\n",
            "5x\n",
            "99999999999999999999\n",
            "+5\n",
        ] {
            assert_eq!(parse_seq(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn layout_and_yes() {
        assert!(!needs_layout(true, true));
        assert!(needs_layout(true, false));
        assert!(needs_layout(false, true));
        assert!(needs_layout(false, false));
        assert!(yes_typed("yes\n"));
        assert!(yes_typed("yes\r\n"));
        assert!(yes_typed("yes"));
        for no in [
            "",
            "\n",
            "y\n",
            "Yes\n",
            "YES\n",
            "yes \n",
            " yes\n",
            "yes please\n",
            "yess\n",
            "no\n",
        ] {
            assert!(!yes_typed(no), "{no:?}");
        }
    }
}
