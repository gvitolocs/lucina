//! The ring arithmetic against memory images worked out by hand, so that a mistake shared by both ends of the
//! two-sided model cannot hide.

use std::cell::RefCell;
use std::rc::Rc;
use std::vec;
use std::vec::Vec;

use super::support::{Port, Ram, ap_layout};
use crate::fifo::{Fault, Fifo, Layout, LayoutError, Region, Side};

fn ap(tx: usize, rx: usize) -> (Rc<RefCell<Ram>>, Port, Fifo) {
    let ram = Ram::new(tx, rx);
    let mut port = Port::new(&ram, Side::Ap);
    let fifo = Fifo::new(&mut port, ap_layout(&ram, true)).unwrap();
    (ram, port, fifo)
}

fn bytes(ram: &Ram, r: Region) -> Vec<u8> {
    ram.words[r.off / 4..(r.off + r.len) / 4]
        .iter()
        .flat_map(|w| w.to_le_bytes())
        .collect()
}

/// Pretends the remote has consumed everything the AP wrote.
fn remote_reads_all(ram: &Rc<RefCell<Ram>>) {
    let head = ram.borrow().desc(1);
    ram.borrow_mut().set_desc(0, head);
}

#[test]
fn layout_validation() {
    let ok = Layout {
        side: Side::Ap,
        desc: 0,
        tx: Region { off: 32, len: 64 },
        rx: Some(Region { off: 96, len: 64 }),
    };
    assert_eq!(ok.validate(), Ok(()));
    let bad = |l: Layout| l.validate().unwrap_err();
    assert_eq!(bad(Layout { desc: 2, ..ok }), LayoutError::Misaligned);
    assert_eq!(
        bad(Layout {
            tx: Region { off: 34, len: 64 },
            ..ok
        }),
        LayoutError::Misaligned
    );
    assert_eq!(
        bad(Layout {
            tx: Region { off: 32, len: 60 },
            ..ok
        }),
        LayoutError::BadLength
    );
    assert_eq!(
        bad(Layout {
            tx: Region { off: 32, len: 32 },
            ..ok
        }),
        LayoutError::BadLength
    );
    assert_eq!(
        bad(Layout {
            tx: Region { off: 16, len: 64 },
            ..ok
        }),
        LayoutError::Overlap
    );
    assert_eq!(
        bad(Layout {
            rx: Some(Region { off: 64, len: 64 }),
            ..ok
        }),
        LayoutError::Overlap
    );
    // Not known yet is fine.
    assert_eq!(Layout { rx: None, ..ok }.validate(), Ok(()));
}

#[test]
fn tx_avail_follows_the_c_formula_with_both_reserves() {
    let (ram, mut io, mut f) = ap(64, 64);
    // Empty ring: `tail <= head` with both 0 -> avail = len - head + tail = 64, minus 16.
    assert_eq!(f.tx_avail(&mut io), Ok(48));
    assert_eq!(f.tx_capacity(), 48);
    assert_eq!(f.tx_write(&mut io, &[&[1; 8]], false), Ok(true));
    // head = 8, tail = 0: tail <= head -> 64 - 8 + 0 = 56, minus 16.
    assert_eq!(f.tx_avail(&mut io), Ok(40));
    // The remote reads the 8 bytes: tail = head = 8 -> empty again.
    remote_reads_all(&ram);
    assert_eq!(f.tx_avail(&mut io), Ok(48));
    // A frame of exactly the available size fits; one byte more (padded: 8 more) does not.
    assert_eq!(f.tx_write(&mut io, &[&[2; 49]], false), Ok(false));
    assert_eq!(f.tx_write(&mut io, &[&[2; 48]], false), Ok(true));
    assert_eq!(f.tx_avail(&mut io), Ok(0));
    assert_eq!(ram.borrow().desc(1), 56);
}

#[test]
fn tx_write_wraps_pads_with_zeros_and_concatenates_parts() {
    let (ram, mut io, mut f) = ap(64, 64);
    // Walk the head to 56 with the remote keeping up.
    for _ in 0..7 {
        assert_eq!(f.tx_write(&mut io, &[&[0xee; 8]], false), Ok(true));
        remote_reads_all(&ram);
    }
    assert_eq!(ram.borrow().desc(1), 56);
    let frame_a: Vec<u8> = (1..=10).collect();
    let frame_b: Vec<u8> = (11..=21).collect();
    assert_eq!(f.tx_write(&mut io, &[&frame_a, &frame_b], false), Ok(true));
    // 21 bytes -> 24 padded: bytes 1..=8 at 56..64, bytes 9..=21 and three zero pad bytes at 0..16.
    let tx = ram.borrow().tx_region();
    let img = bytes(&ram.borrow(), tx);
    let mut want = vec![0u8; 64];
    want[56..64].copy_from_slice(&[1, 2, 3, 4, 5, 6, 7, 8]);
    want[0..16].copy_from_slice(&[9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21, 0, 0, 0]);
    // Bytes 16..56 still hold the 0xee frames written earlier, except the first 16 which were overwritten.
    for b in &mut want[16..56] {
        *b = 0xee;
    }
    assert_eq!(img, want);
    // `head = ALIGN(56 + 21, 8) - 64` = 80 - 64 = 16.
    assert_eq!(ram.borrow().desc(1), 16);
}

#[test]
fn tx_reserve_for_the_blocked_notice() {
    let (ram, mut io, mut f) = ap(64, 64);
    assert_eq!(f.tx_write(&mut io, &[&[1; 48]], false), Ok(true));
    // head = 48, tail = 0: free 16. A normal frame needs 8 + 16 reserved: no. The notice may use 8 of them.
    assert_eq!(f.tx_avail(&mut io), Ok(0));
    assert_eq!(f.tx_write(&mut io, &[&[2; 8]], false), Ok(false));
    assert_eq!(f.tx_write(&mut io, &[&[3; 8]], true), Ok(true));
    // head = 56: free 8 = FIFO_FULL_RESERVE; nothing more, notice or not.
    assert_eq!(f.tx_write(&mut io, &[&[4; 8]], true), Ok(false));
    assert_eq!(ram.borrow().desc(1), 56);
    // And a full ring still never looks empty: head 56 != tail 0.
}

#[test]
fn rx_reads_across_the_wrap_and_unaligned_starts() {
    let (ram, mut io, mut f) = ap(64, 64);
    // The remote's ring is full of a counting pattern; our tail is at 56 and its head at 16, 24 bytes ahead.
    let rx = ram.borrow().rx_region();
    let pat: Vec<u8> = (0..64).collect();
    ram.borrow_mut().poke_rx(0, &pat);
    f.rx_advance(&mut io, 56);
    ram.borrow_mut().set_desc(3, 16);
    assert_eq!(f.rx_avail(&mut io), Ok(24));
    let mut out = [0u8; 24];
    f.rx_peek(&mut io, 0, &mut out);
    let want: Vec<u8> = (56..64).chain(0..16).collect();
    assert_eq!(out.to_vec(), want);
    // Starting inside a word and ending inside another, across the wrap.
    let mut out = [0u8; 9];
    f.rx_peek(&mut io, 5, &mut out);
    assert_eq!(out.to_vec(), vec![61, 62, 63, 0, 1, 2, 3, 4, 5]);
    // Advancing wraps the tail and writes it where the descriptor says (word 2 for the AP).
    f.rx_advance(&mut io, 24);
    assert_eq!(ram.borrow().desc(2), 16);
    assert_eq!(f.rx_avail(&mut io), Ok(0));
    let _ = rx;
}

#[test]
fn rx_avail_when_head_is_behind_tail() {
    let (ram, mut io, mut f) = ap(64, 64);
    f.rx_advance(&mut io, 48);
    ram.borrow_mut().set_desc(3, 8);
    // head < tail: len - tail + head = 64 - 48 + 8 = 24 (the C's other branch).
    assert_eq!(f.rx_avail(&mut io), Ok(24));
    assert_eq!(f.rx_capacity(), 56);
}

#[test]
fn junk_and_overwritten_indexes_are_faults_not_panics() {
    let (ram, mut io, mut f) = ap(64, 64);
    ram.borrow_mut().set_desc(3, 64);
    assert_eq!(f.rx_avail(&mut io), Err(Fault::Index));
    ram.borrow_mut().set_desc(3, 0xffff_ffff);
    assert_eq!(f.rx_avail(&mut io), Err(Fault::Index));
    ram.borrow_mut().set_desc(3, 0);
    assert_eq!(f.rx_avail(&mut io), Ok(0));
    // Our own rx_tail changed under us.
    ram.borrow_mut().set_desc(2, 8);
    assert_eq!(f.rx_avail(&mut io), Err(Fault::OwnIndexChanged));
    ram.borrow_mut().set_desc(2, 0);
    // The remote's tail out of range, then moving past our head.
    ram.borrow_mut().set_desc(0, 64);
    assert_eq!(f.tx_avail(&mut io), Err(Fault::Index));
    ram.borrow_mut().set_desc(0, 8);
    assert_eq!(f.tx_avail(&mut io), Err(Fault::TailBeyondHead));
    ram.borrow_mut().set_desc(0, 0);
    assert_eq!(f.tx_avail(&mut io), Ok(48));
    // Our own tx head overwritten.
    ram.borrow_mut().set_desc(1, 8);
    assert_eq!(f.tx_avail(&mut io), Err(Fault::OwnIndexChanged));
    assert_eq!(
        f.tx_write(&mut io, &[&[1; 8]], false),
        Err(Fault::OwnIndexChanged)
    );
}

#[test]
fn resync_empties_both_rings() {
    let (ram, mut io, mut f) = ap(64, 64);
    f.tx_write(&mut io, &[&[1; 16]], false).unwrap();
    remote_reads_all(&ram);
    f.tx_write(&mut io, &[&[1; 16]], false).unwrap();
    ram.borrow_mut().set_desc(3, 40);
    assert_eq!(f.rx_avail(&mut io), Ok(40));
    assert_eq!(f.resync(&mut io, false), Ok(()));
    // Unread data of ours is abandoned (head := the remote's tail), and so is the remote's (tail := its head).
    assert_eq!(ram.borrow().desc(1), 16);
    assert_eq!(ram.borrow().desc(2), 40);
    assert_eq!(f.rx_avail(&mut io), Ok(0));
    assert_eq!(f.tx_avail(&mut io), Ok(48));
    // Keeping the remote's unread data: the tail stays where the descriptor has it, if that is a sane index.
    ram.borrow_mut().set_desc(3, 56);
    ram.borrow_mut().set_desc(2, 24);
    assert_eq!(f.resync(&mut io, true), Ok(()));
    assert_eq!(ram.borrow().desc(2), 24);
    assert_eq!(f.rx_avail(&mut io), Ok(32));
    // ... and falls back to dropping it when the tail is junk.
    ram.borrow_mut().set_desc(2, 4000);
    assert_eq!(f.resync(&mut io, true), Ok(()));
    assert_eq!(ram.borrow().desc(2), 56);
    // With a junk index resync refuses and changes nothing.
    ram.borrow_mut().set_desc(3, 99);
    assert_eq!(f.resync(&mut io, false), Err(Fault::Index));
    assert_eq!(ram.borrow().desc(2), 56);
}

#[test]
fn late_and_bad_receive_rings() {
    let ram = Ram::new(64, 64);
    ram.borrow_mut().rx_visible = false;
    let mut io = Port::new(&ram, Side::Ap);
    let mut f = Fifo::new(&mut io, ap_layout(&ram, false)).unwrap();
    assert_eq!(f.rx_avail(&mut io), Ok(0));
    assert_eq!(f.rx_capacity(), 0);
    ram.borrow_mut().rx_visible = true;
    ram.borrow_mut().set_desc(3, 24);
    assert_eq!(f.rx_avail(&mut io), Ok(24));
    assert_eq!(f.rx_capacity(), 56);
}

#[test]
fn words_only_no_byte_access_leaks_out() {
    // `Port` panics on anything but aligned words inside the three areas, and on writes to the other side's
    // words; a long mixed run of every operation is the check that none of them does.
    let (ram, mut io, mut f) = ap(128, 128);
    for round in 0..200u32 {
        let n = (round as usize * 7) % 40 + 1;
        let _ = f
            .tx_write(&mut io, &[&vec![round as u8; n]], round % 5 == 0)
            .unwrap();
        remote_reads_all(&ram);
        ram.borrow_mut()
            .poke_rx(round as usize % 128, &[round as u8; 9]);
        ram.borrow_mut().set_desc(3, (round * 8) % 128);
        let avail = f.rx_avail(&mut io).unwrap();
        let mut out = vec![0u8; avail];
        f.rx_peek(&mut io, 0, &mut out);
        f.rx_advance(&mut io, avail);
        assert_eq!(f.rx_avail(&mut io), Ok(0));
    }
}
