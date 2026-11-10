//! Byte goldens: every layout and number below was read off the C, and the line cited is where. All lines are in
//! `drivers/rpmsg/qcom_glink_native.c` of the 7.1 tree unless a file is named. Multi-byte fields are little endian.

use std::vec;
use std::vec::Vec;

use super::model::{Msg, Rig, bring_up, m_close, m_data, m_intent, m_open};
use crate::Event;
use crate::wire::{self, Header};

struct Case {
    what: &'static str,
    src: &'static str,
    got: Vec<u8>,
    want: Vec<u8>,
}

fn name_bytes(s: &str, total: usize) -> Vec<u8> {
    let mut v = s.as_bytes().to_vec();
    v.resize(total, 0);
    v
}

#[test]
fn frame_layouts() {
    let sigs = wire::SIG_DSR | wire::SIG_CTS;
    let mut open_want = vec![0x02, 0, 0x02, 0, 0x07, 0, 0, 0];
    open_want.extend(name_bytes("IPCRTR", 8));
    let cases = [
        Case {
            what: "VERSION(1, INTENT_REUSE)",
            src: "398-409: cmd=GLINK_CMD_VERSION(0), param1=GLINK_VERSION_1, param2=features",
            got: wire::simple(
                wire::CMD_VERSION,
                wire::VERSION_1,
                wire::FEATURE_INTENT_REUSE,
            ),
            want: vec![0, 0, 1, 0, 1, 0, 0, 0],
        },
        Case {
            what: "VERSION_ACK(1, 0)",
            src: "411-422: cmd=GLINK_CMD_VERSION_ACK(1)",
            got: wire::simple(wire::CMD_VERSION_ACK, 1, 0),
            want: vec![1, 0, 1, 0, 0, 0, 0, 0],
        },
        Case {
            what: "OPEN lcid=2 \"IPCRTR\"",
            src: "480-: param1=lcid, param2=name_len incl NUL (7), name follows, ALIGN(8 + 7, 8) = 16 bytes",
            got: wire::open(2, "IPCRTR"),
            want: open_want,
        },
        Case {
            what: "OPEN_ACK rcid=7",
            src: "424-437: cmd=GLINK_CMD_OPEN_ACK(4), param1=channel->rcid, param2=0",
            got: wire::simple(wire::CMD_OPEN_ACK, 7, 0),
            want: vec![4, 0, 7, 0, 0, 0, 0, 0],
        },
        Case {
            what: "CLOSE lcid=3",
            src: "524-537: cmd=GLINK_CMD_CLOSE(3), param1=channel->lcid",
            got: wire::simple(wire::CMD_CLOSE, 3, 0),
            want: vec![3, 0, 3, 0, 0, 0, 0, 0],
        },
        Case {
            what: "CLOSE_ACK rcid=9",
            src: "539-552: cmd=GLINK_CMD_CLOSE_ACK(11), param1=channel->rcid",
            got: wire::simple(wire::CMD_CLOSE_ACK, 9, 0),
            want: vec![11, 0, 9, 0, 0, 0, 0, 0],
        },
        Case {
            what: "INTENT lcid=2 size=1024 iid=1",
            src: "719-744: struct command {le16 id; le16 lcid; le32 count; le32 size; le32 liid} __packed, count=1",
            got: wire::intent(2, 1024, 1),
            want: vec![5, 0, 2, 0, 1, 0, 0, 0, 0x00, 0x04, 0, 0, 1, 0, 0, 0],
        },
        Case {
            what: "RX_DONE lcid=2 iid=3",
            src: "554-: struct {u16 id; u16 lcid; u32 liid} __packed, id = GLINK_CMD_RX_DONE(6)",
            got: wire::simple(wire::CMD_RX_DONE, 2, 3),
            want: vec![6, 0, 2, 0, 3, 0, 0, 0],
        },
        Case {
            what: "RX_DONE_W_REUSE lcid=2 iid=3",
            src: "554-: same struct, id = GLINK_CMD_RX_DONE_W_REUSE(14) when intent->reuse",
            got: wire::simple(wire::CMD_RX_DONE_W_REUSE, 2, 3),
            want: vec![14, 0, 2, 0, 3, 0, 0, 0],
        },
        Case {
            what: "RX_INTENT_REQ cid=2 size=300",
            src: "1427-: struct {u16 id; u16 cid; u32 size} __packed, id = GLINK_CMD_RX_INTENT_REQ(7)",
            got: wire::simple(wire::CMD_RX_INTENT_REQ, 2, 300),
            want: vec![7, 0, 2, 0, 0x2c, 1, 0, 0],
        },
        Case {
            what: "RX_INTENT_REQ_ACK lcid=2 granted",
            src: "691-704: cmd=GLINK_CMD_RX_INTENT_REQ_ACK(8), param1=channel->lcid, param2=granted",
            got: wire::simple(wire::CMD_RX_INTENT_REQ_ACK, 2, 1),
            want: vec![8, 0, 2, 0, 1, 0, 0, 0],
        },
        Case {
            what: "TX_DATA header lcid=2 iid=5 chunk=100 left=50",
            src: "1476-: struct {glink_msg_hdr; le32 chunk_size; le32 left_size} __packed, cmd TX_DATA(9) at offset 0",
            got: wire::data_header(wire::CMD_TX_DATA, 2, 5, 100, 50).to_vec(),
            want: vec![9, 0, 2, 0, 5, 0, 0, 0, 100, 0, 0, 0, 50, 0, 0, 0],
        },
        Case {
            what: "TX_DATA_CONT header",
            src: "1476-: GLINK_CMD_TX_DATA_CONT(12) for every chunk after the first",
            got: wire::data_header(wire::CMD_TX_DATA_CONT, 2, 5, 100, 0).to_vec(),
            want: vec![12, 0, 2, 0, 5, 0, 0, 0, 100, 0, 0, 0, 0, 0, 0, 0],
        },
        Case {
            what: "SIGNALS lcid=2, DTR|RTS",
            src: "1126-1144: cmd=GLINK_CMD_SIGNALS(15), param1=lcid, param2 = NATIVE_DTR_SIG|NATIVE_RTS_SIG = BIT(31)|BIT(30)",
            got: wire::simple(wire::CMD_SIGNALS, 2, sigs),
            want: vec![15, 0, 2, 0, 0, 0, 0, 0xc0],
        },
        Case {
            what: "READ_NOTIF",
            src: "329-340: cmd=GLINK_CMD_READ_NOTIF(13), params 0",
            got: wire::simple(wire::CMD_READ_NOTIF, 0, 0),
            want: vec![13, 0, 0, 0, 0, 0, 0, 0],
        },
    ];
    for c in cases {
        assert_eq!(c.got, c.want, "{} (C: {})", c.what, c.src);
        assert_eq!(c.got.len() % 8, 0, "{}: frames are padded to 8", c.what);
    }
}

#[test]
fn command_numbers_and_constants() {
    // `#define GLINK_CMD_*`, lines 200-214 (10 is unused); GLINK_FEATURE_*: qcom_glink_native.h:11-13 and
    // line 216; NATIVE_*_SIG: 218-221; GLINK_NAME_SIZE 29, GLINK_VERSION_1 30, RPM_GLINK_CID_MIN/MAX 32-33.
    let table: [(u16, u16); 15] = [
        (wire::CMD_VERSION, 0),
        (wire::CMD_VERSION_ACK, 1),
        (wire::CMD_OPEN, 2),
        (wire::CMD_CLOSE, 3),
        (wire::CMD_OPEN_ACK, 4),
        (wire::CMD_INTENT, 5),
        (wire::CMD_RX_DONE, 6),
        (wire::CMD_RX_INTENT_REQ, 7),
        (wire::CMD_RX_INTENT_REQ_ACK, 8),
        (wire::CMD_TX_DATA, 9),
        (wire::CMD_CLOSE_ACK, 11),
        (wire::CMD_TX_DATA_CONT, 12),
        (wire::CMD_READ_NOTIF, 13),
        (wire::CMD_RX_DONE_W_REUSE, 14),
        (wire::CMD_SIGNALS, 15),
    ];
    for (got, want) in table {
        assert_eq!(got, want);
    }
    assert_eq!(wire::FEATURE_INTENT_REUSE, 1);
    assert_eq!(wire::FEATURE_MIGRATION, 2);
    assert_eq!(wire::FEATURE_TRACER_PKT, 4);
    assert_eq!(wire::SIG_DSR, 0x8000_0000);
    assert_eq!(wire::SIG_CTS, 0x4000_0000);
    assert_eq!(
        (
            wire::NAME_SIZE,
            wire::VERSION_1,
            wire::CID_MIN,
            wire::CID_MAX
        ),
        (32, 1, 1, 65536)
    );
    assert_eq!((wire::HDR_LEN, wire::DATA_HDR_LEN), (8, 16));
    // qcom_glink_smem.c:34-36 and :267, :292: the SMEM items and sizes.
    assert_eq!(
        (
            crate::SMEM_ITEM_DESCRIPTOR,
            crate::SMEM_ITEM_FIFO_0,
            crate::SMEM_ITEM_FIFO_1
        ),
        (478, 479, 480)
    );
    assert_eq!((crate::DESCRIPTOR_SIZE, crate::TX_FIFO_SIZE), (32, 16384));
    assert_eq!(
        (
            crate::fifo::FIFO_FULL_RESERVE,
            crate::fifo::TX_BLOCKED_CMD_RESERVE
        ),
        (8, 8)
    );
}

#[test]
fn header_decode_is_the_inverse() {
    let h = Header::decode(&[0x0e, 0, 0x34, 0x12, 0x78, 0x56, 0x34, 0x12]);
    assert_eq!(
        h,
        Header {
            cmd: 14,
            param1: 0x1234,
            param2: 0x1234_5678
        }
    );
    assert_eq!(h.encode(), [0x0e, 0, 0x34, 0x12, 0x78, 0x56, 0x34, 0x12]);
}

/// What the AP actually puts in the ring, byte for byte, for a bring-up and a few operations.
#[test]
fn ap_frames_on_the_wire() {
    let mut rig = Rig::new(4096, 4096);
    let up = bring_up(&mut rig);
    let mut want: Vec<Vec<u8>> = vec![
        vec![0, 0, 1, 0, 1, 0, 0, 0], // VERSION(1, 1)
        vec![1, 0, 1, 0, 1, 0, 0, 0], // VERSION_ACK(1, 1)
        vec![4, 0, 5, 0, 0, 0, 0, 0], // OPEN_ACK(modem's id 5)
        [vec![2, 0, 1, 0, 7, 0, 0, 0], name_bytes("IPCRTR", 8)].concat(), // OPEN lcid=1, name_len 7
    ];
    for iid in 1..=5u8 {
        // INTENT lcid=1 count=1 size=0x400 iid
        want.push(vec![5, 0, 1, 0, 1, 0, 0, 0, 0, 4, 0, 0, iid, 0, 0, 0]);
    }
    assert_eq!(rig.modem.raw, want);

    // A 20-byte message into the modem's intent 7, an unmet request, a close.
    rig.modem_says(vec![m_intent(up.m_cid, 64, 7)]);
    rig.modem.raw.clear();
    rig.ap.send(up.ch, &[0xaa; 20]).unwrap();
    rig.ap.send(up.ch, &[0xbb; 100]).unwrap();
    rig.ap.set_flow_control(up.ch, true).unwrap();
    rig.ap.close(up.ch).unwrap();
    rig.settle();
    let mut data = vec![9, 0, 1, 0, 7, 0, 0, 0, 20, 0, 0, 0, 0, 0, 0, 0];
    data.extend([0xaa; 20]);
    data.extend([0, 0, 0, 0]); // padding to 40 bytes
    assert_eq!(
        rig.modem.raw,
        vec![
            data,
            vec![7, 0, 1, 0, 100, 0, 0, 0], // RX_INTENT_REQ(lcid 1, 100)
            vec![15, 0, 1, 0, 0, 0, 0, 0xc0], // SIGNALS
            vec![3, 0, 1, 0, 0, 0, 0, 0],   // CLOSE
        ]
    );
}

/// `glink_defer_cmd`/`qcom_glink_rx_defer`: an OPEN carries `ALIGN(param2 & 0xffff, 8)` bytes after the header, and
/// the upper 16 bits of param2 are a priority the receiver ignores (line 1207). The AP must consume exactly the
/// padded size.
#[test]
fn open_frame_size_and_prio_bits() {
    let mut rig = Rig::new(1024, 1024);
    rig.settle();
    rig.modem.take();
    for (name, prio) in [
        ("A", 0u32),
        ("EIGHT_NM", 0),
        ("NINE_NAME", 0),
        ("IPCRTR", 5),
    ] {
        let mut f = m_open(3, name);
        let n = name.len() as u32 + 1;
        f[4..8].copy_from_slice(&(n | (prio << 16)).to_le_bytes());
        // Place by hand to see how far the AP's tail moves.
        let before = rig.ram.borrow().desc(2);
        rig.modem.send(f.clone());
        rig.settle();
        let after = rig.ram.borrow().desc(2);
        let consumed = (after + 1024 - before) % 1024;
        assert_eq!(consumed as usize, (8 + n as usize + 7) & !7, "{name}");
        rig.modem.take();
        // Close it again so the next name can use the same remote id.
        rig.modem.send(m_close(3));
        rig.settle();
        rig.modem.take();
    }
}

/// `qcom_glink_handle_intent`: `msglen = struct_size(msg, intents, count)` = 8 + 8 * count, advanced as
/// `ALIGN(msglen, 8)`; the pairs are (size, iid).
#[test]
fn intent_pairs_layout_and_consumption() {
    let mut rig = Rig::new(2048, 2048);
    let up = bring_up(&mut rig);
    let mut f = vec![5, 0, up.m_cid as u8, 0, 2, 0, 0, 0];
    f.extend([100, 0, 0, 0, 40, 0, 0, 0]); // size 100, iid 40
    f.extend([200, 0, 0, 0, 41, 0, 0, 0]); // size 200, iid 41
    let before = rig.ram.borrow().desc(2);
    rig.modem_says(vec![f]);
    assert_eq!((rig.ram.borrow().desc(2) + 2048 - before) % 2048, 24);
    rig.ap.send(up.ch, &[1; 150]).unwrap();
    rig.settle();
    let seen = rig.modem.take();
    assert!(matches!(&seen[..], [Msg::Data { iid: 41, .. }]), "{seen:?}");
}

/// The TX_DATA payload is copied from `sizeof(hdr) = 16` bytes after the tail and the frame is advanced by
/// `ALIGN(16 + chunk_size, 8)` (lines 1011 and the peek before it).
#[test]
fn data_frame_consumption() {
    let mut rig = Rig::new(2048, 2048);
    let up = bring_up(&mut rig);
    let (_, iid) = up.intents[0];
    for len in [1usize, 7, 8, 9, 100] {
        let before = rig.ram.borrow().desc(2);
        let ev = rig.modem_says(vec![m_data(false, up.m_cid, iid, &vec![3; len], 0)]);
        assert!(matches!(&ev[..], [Event::Data { data, .. }] if data.len() == len));
        assert_eq!(
            (rig.ram.borrow().desc(2) + 2048 - before) as usize % 2048,
            (16 + len + 7) & !7
        );
        rig.modem.take();
    }
}

#[test]
fn modem_helper_builders_match_wire() {
    // The model builds its frames on its own; they must agree with wire.rs for the shapes both make.
    assert_eq!(super::model::m_intent(2, 1024, 1), wire::intent(2, 1024, 1));
    let mut o = super::model::m_open(2, "IPCRTR");
    o.resize(16, 0);
    assert_eq!(o, wire::open(2, "IPCRTR"));
}
