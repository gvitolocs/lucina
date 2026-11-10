//! Packet bytes against the C structs.
//!
//! `tests/fixtures/packets.txt` is written by `oracle/`, which fills Linux's own `struct qrtr_hdr_v1`,
//! `qrtr_hdr_v2` and `qrtr_ctrl_pkt` the way `qrtr_node_enqueue()` does. Every packet is decoded here field by
//! field, rebuilt byte for byte, and then cut, extended and mutated: nothing may panic, and a packet whose
//! length no longer fits its header is refused.

use lucina_qrtr::packet::{
    self, CTRL_PKT_LEN, CtrlPkt, EP_NID_AUTO, FLAGS_CONFIRM_RX, HDR_V1_LEN, HDR_V2_LEN,
    MAX_EPH_PORT, MIN_EPH_PORT, NODE_BCAST, PORT_CTRL, PORT_CTRL_LEGACY, PROTO_VER_1, PROTO_VER_2,
    Packet, PacketError, Server, TX_FLOW_HIGH, TX_FLOW_LOW, decode, encode_v1, encode_v2, pkt_type,
};
use std::collections::{BTreeMap, BTreeSet};

const PACKETS: &str = include_str!("fixtures/packets.txt");
const CONSTANTS: &str = include_str!("fixtures/constants.txt");

fn unhex(s: &str) -> Vec<u8> {
    assert!(s.len().is_multiple_of(2));
    (0..s.len() / 2)
        .map(|i| u8::from_str_radix(&s[2 * i..2 * i + 2], 16).unwrap())
        .collect()
}

fn fixtures() -> BTreeMap<String, Vec<u8>> {
    PACKETS
        .lines()
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .map(|l| {
            let (n, h) = l.split_once(' ').unwrap();
            (n.to_string(), unhex(h))
        })
        .collect()
}

#[test]
fn constants_and_layout_match_the_c() {
    let mut c = BTreeMap::new();
    for l in CONSTANTS.lines().filter(|l| !l.starts_with('#')) {
        let (n, v) = l.rsplit_once(' ').unwrap();
        c.insert(n.to_string(), v.parse::<u64>().unwrap());
    }
    let mut used = BTreeSet::new();
    let mut eq = |name: &str, ours: u64| {
        assert_eq!(
            ours,
            *c.get(name).unwrap_or_else(|| panic!("no {name}")),
            "{name}"
        );
        used.insert(name.to_string());
    };
    eq("QRTR_PROTO_VER_1", u64::from(PROTO_VER_1));
    eq("QRTR_PROTO_VER_2", u64::from(PROTO_VER_2));
    eq("QRTR_NODE_BCAST", u64::from(NODE_BCAST));
    eq("QRTR_PORT_CTRL", u64::from(PORT_CTRL));
    eq("QRTR_PORT_CTRL_LEGACY", u64::from(PORT_CTRL_LEGACY));
    eq("QRTR_EP_NID_AUTO", u64::from(EP_NID_AUTO));
    eq("QRTR_MIN_EPH_SOCKET", u64::from(MIN_EPH_PORT));
    eq("QRTR_MAX_EPH_SOCKET", u64::from(MAX_EPH_PORT));
    eq("QRTR_FLAGS_CONFIRM_RX", u64::from(FLAGS_CONFIRM_RX));
    eq("QRTR_TX_FLOW_HIGH", u64::from(TX_FLOW_HIGH));
    eq("QRTR_TX_FLOW_LOW", u64::from(TX_FLOW_LOW));
    eq(
        "QRTR_NS_MAX_NODES",
        lucina_qrtr::router::NS_MAX_NODES as u64,
    );
    eq(
        "QRTR_NS_MAX_SERVERS",
        lucina_qrtr::router::NS_MAX_SERVERS as u64,
    );
    eq(
        "QRTR_NS_MAX_LOOKUPS",
        lucina_qrtr::router::NS_MAX_LOOKUPS as u64,
    );
    eq("QRTR_TYPE_DATA", u64::from(pkt_type::DATA));
    eq("QRTR_TYPE_HELLO", u64::from(pkt_type::HELLO));
    eq("QRTR_TYPE_BYE", u64::from(pkt_type::BYE));
    eq("QRTR_TYPE_NEW_SERVER", u64::from(pkt_type::NEW_SERVER));
    eq("QRTR_TYPE_DEL_SERVER", u64::from(pkt_type::DEL_SERVER));
    eq("QRTR_TYPE_DEL_CLIENT", u64::from(pkt_type::DEL_CLIENT));
    eq("QRTR_TYPE_RESUME_TX", u64::from(pkt_type::RESUME_TX));
    eq("QRTR_TYPE_EXIT", u64::from(pkt_type::EXIT));
    eq("QRTR_TYPE_PING", u64::from(pkt_type::PING));
    eq("QRTR_TYPE_NEW_LOOKUP", u64::from(pkt_type::NEW_LOOKUP));
    eq("QRTR_TYPE_DEL_LOOKUP", u64::from(pkt_type::DEL_LOOKUP));
    eq("sizeof(struct qrtr_hdr_v1)", HDR_V1_LEN as u64);
    eq("sizeof(struct qrtr_hdr_v2)", HDR_V2_LEN as u64);
    eq("sizeof(struct qrtr_ctrl_pkt)", CTRL_PKT_LEN as u64);
    // the offsets decode() reads at, by field
    for (name, at) in [
        ("offsetof(struct qrtr_hdr_v1, version)", 0),
        ("offsetof(struct qrtr_hdr_v1, type)", 4),
        ("offsetof(struct qrtr_hdr_v1, src_node_id)", 8),
        ("offsetof(struct qrtr_hdr_v1, src_port_id)", 12),
        ("offsetof(struct qrtr_hdr_v1, confirm_rx)", 16),
        ("offsetof(struct qrtr_hdr_v1, size)", 20),
        ("offsetof(struct qrtr_hdr_v1, dst_node_id)", 24),
        ("offsetof(struct qrtr_hdr_v1, dst_port_id)", 28),
        ("offsetof(struct qrtr_hdr_v2, version)", 0),
        ("offsetof(struct qrtr_hdr_v2, type)", 1),
        ("offsetof(struct qrtr_hdr_v2, flags)", 2),
        ("offsetof(struct qrtr_hdr_v2, optlen)", 3),
        ("offsetof(struct qrtr_hdr_v2, size)", 4),
        ("offsetof(struct qrtr_hdr_v2, src_node_id)", 8),
        ("offsetof(struct qrtr_hdr_v2, src_port_id)", 10),
        ("offsetof(struct qrtr_hdr_v2, dst_node_id)", 12),
        ("offsetof(struct qrtr_hdr_v2, dst_port_id)", 14),
        ("offsetof(struct qrtr_ctrl_pkt, cmd)", 0),
        ("offsetof(struct qrtr_ctrl_pkt, server.service)", 4),
        ("offsetof(struct qrtr_ctrl_pkt, server.instance)", 8),
        ("offsetof(struct qrtr_ctrl_pkt, server.node)", 12),
        ("offsetof(struct qrtr_ctrl_pkt, server.port)", 16),
        ("offsetof(struct qrtr_ctrl_pkt, client.node)", 4),
        ("offsetof(struct qrtr_ctrl_pkt, client.port)", 8),
    ] {
        eq(name, at);
    }
    for name in c.keys() {
        assert!(used.contains(name), "C constant {name} is not checked");
    }
}

#[test]
fn control_payloads() {
    let f = fixtures();
    let new_server = CtrlPkt::with_server(
        pkt_type::NEW_SERVER,
        Server {
            service: 69,
            instance: 1,
            node: 0,
            port: 0x2a,
        },
    );
    assert_eq!(new_server.to_bytes().to_vec(), f["ctrl_new_server"]);
    let resume = CtrlPkt::with_client(pkt_type::RESUME_TX, 1, 0x4001);
    assert_eq!(resume.to_bytes().to_vec(), f["ctrl_resume_tx"]);
    assert_eq!(
        CtrlPkt::with_client(pkt_type::BYE, 0, 0)
            .to_bytes()
            .to_vec(),
        f["ctrl_bye"]
    );
    assert_eq!(
        CtrlPkt::new(pkt_type::HELLO).to_bytes().to_vec(),
        f["ctrl_hello"]
    );
    let all = Server {
        service: 0xdead_beef,
        instance: 0x0102_0304,
        node: 0x0a0b_0c0d,
        port: 0xf0e0_d0c0,
    };
    let p = CtrlPkt::with_server(pkt_type::NEW_SERVER, all);
    assert_eq!(p.to_bytes().to_vec(), f["ctrl_server_all_bits"]);

    // and back; the client fields lie over the first two server words
    for name in [
        "ctrl_new_server",
        "ctrl_resume_tx",
        "ctrl_bye",
        "ctrl_server_all_bits",
        "ctrl_hello",
    ] {
        let p = CtrlPkt::parse(&f[name]).unwrap();
        assert_eq!(p.to_bytes().to_vec(), f[name], "{name}");
    }
    let p = CtrlPkt::parse(&f["ctrl_resume_tx"]).unwrap();
    assert_eq!(p.client(), (1, 0x4001));
    assert_eq!(p.server().service, 1);
    assert_eq!(p.server().instance, 0x4001);
    assert_eq!(
        CtrlPkt::parse(&f["ctrl_new_server"]).unwrap().server().port,
        0x2a
    );

    // short payloads read as zero past their end; under four bytes there is no command
    assert_eq!(CtrlPkt::parse(&[]), None);
    assert_eq!(CtrlPkt::parse(&[1, 0, 0]), None);
    let p = CtrlPkt::parse(&f["ctrl_new_server"][..8]).unwrap();
    assert_eq!(p.cmd, pkt_type::NEW_SERVER);
    assert_eq!(p.server().service, 69);
    assert_eq!(p.server().instance, 0);
    // long payloads are read as far as the struct goes
    let mut long = f["ctrl_new_server"].clone();
    long.extend_from_slice(&[9; 8]);
    assert_eq!(CtrlPkt::parse(&long), CtrlPkt::parse(&f["ctrl_new_server"]));
}

struct Want<'a> {
    ptype: u32,
    src: (u32, u32),
    dst: (u32, u32),
    confirm_rx: bool,
    payload: &'a [u8],
}

fn check_decoded(name: &str, p: &Packet<'_>, version: u8, w: &Want<'_>) {
    assert_eq!(p.version, version, "{name}");
    assert_eq!(p.ptype, w.ptype, "{name}");
    assert_eq!((p.src_node, p.src_port), w.src, "{name}");
    assert_eq!((p.dst_node, p.dst_port), w.dst, "{name}");
    assert_eq!(p.confirm_rx, w.confirm_rx, "{name}");
    assert_eq!(p.payload, w.payload, "{name}");
}

#[test]
fn v1_packets() {
    let f = fixtures();
    let ctrl = |name: &str| f[name].clone();
    let hello = ctrl("ctrl_hello");
    let new_server = ctrl("ctrl_new_server");
    let mut used = BTreeSet::new();

    // (name, type, src, dst, confirm_rx, payload)
    let nid_auto = EP_NID_AUTO;
    let p_new_server_local = CtrlPkt::with_server(
        pkt_type::NEW_SERVER,
        Server {
            service: 69,
            instance: 1,
            node: 1,
            port: 0x4001,
        },
    )
    .to_bytes();
    let p_del_server_modem = CtrlPkt::with_server(
        pkt_type::DEL_SERVER,
        Server {
            service: 69,
            instance: 1,
            node: 0,
            port: 0x2a,
        },
    )
    .to_bytes();
    let p_del_server_local = CtrlPkt::with_server(
        pkt_type::DEL_SERVER,
        Server {
            service: 69,
            instance: 1,
            node: 1,
            port: 0x4001,
        },
    )
    .to_bytes();
    let p_lookup = |cmd| {
        CtrlPkt::with_server(
            cmd,
            Server {
                service: 69,
                instance: 1,
                node: 0,
                port: 0,
            },
        )
        .to_bytes()
    };
    let p_new_lookup = p_lookup(pkt_type::NEW_LOOKUP);
    let p_del_lookup = p_lookup(pkt_type::DEL_LOOKUP);
    let cases: Vec<(&str, Want<'_>)> = vec![
        (
            "hello_to_unknown_node",
            Want {
                ptype: 2,
                src: (1, PORT_CTRL),
                dst: (nid_auto, PORT_CTRL),
                confirm_rx: false,
                payload: &hello,
            },
        ),
        (
            "hello_from_node0_to_unknown_node",
            Want {
                ptype: 2,
                src: (0, PORT_CTRL),
                dst: (nid_auto, PORT_CTRL),
                confirm_rx: false,
                payload: &hello,
            },
        ),
        (
            "hello_to_node0",
            Want {
                ptype: 2,
                src: (1, PORT_CTRL),
                dst: (0, PORT_CTRL),
                confirm_rx: false,
                payload: &hello,
            },
        ),
        (
            "hello_from_node0",
            Want {
                ptype: 2,
                src: (0, PORT_CTRL),
                dst: (1, PORT_CTRL),
                confirm_rx: false,
                payload: &hello,
            },
        ),
        (
            "new_server_from_modem",
            Want {
                ptype: 4,
                src: (0, PORT_CTRL),
                dst: (1, PORT_CTRL),
                confirm_rx: false,
                payload: &new_server,
            },
        ),
        (
            "new_server_announce_local",
            Want {
                ptype: 4,
                src: (1, PORT_CTRL),
                dst: (0, PORT_CTRL),
                confirm_rx: false,
                payload: &p_new_server_local,
            },
        ),
        (
            "del_server_from_modem",
            Want {
                ptype: 5,
                src: (0, PORT_CTRL),
                dst: (1, PORT_CTRL),
                confirm_rx: false,
                payload: &p_del_server_modem,
            },
        ),
        (
            "del_server_announce_local",
            Want {
                ptype: 5,
                src: (1, PORT_CTRL),
                dst: (0, PORT_CTRL),
                confirm_rx: false,
                payload: &p_del_server_local,
            },
        ),
        (
            "new_lookup_from_modem",
            Want {
                ptype: 10,
                src: (0, 0x30),
                dst: (1, PORT_CTRL),
                confirm_rx: false,
                payload: &p_new_lookup,
            },
        ),
        (
            "del_lookup_from_modem",
            Want {
                ptype: 11,
                src: (0, 0x30),
                dst: (1, PORT_CTRL),
                confirm_rx: false,
                payload: &p_del_lookup,
            },
        ),
        (
            "data_len1",
            Want {
                ptype: 1,
                src: (1, 0x4001),
                dst: (0, 0x2a),
                confirm_rx: false,
                payload: b"a",
            },
        ),
        (
            "data_len4",
            Want {
                ptype: 1,
                src: (1, 0x4001),
                dst: (0, 0x2a),
                confirm_rx: false,
                payload: b"abcd",
            },
        ),
        (
            "data_len5_confirm_rx",
            Want {
                ptype: 1,
                src: (1, 0x4001),
                dst: (0, 0x2a),
                confirm_rx: true,
                payload: b"hello",
            },
        ),
        (
            "data_len7",
            Want {
                ptype: 1,
                src: (0, 0x2a),
                dst: (1, 0x4001),
                confirm_rx: false,
                payload: b"7 bytes",
            },
        ),
        (
            "data_len8_from_modem_confirm_rx",
            Want {
                ptype: 1,
                src: (0, 0x2a),
                dst: (1, 0x4001),
                confirm_rx: true,
                payload: b"8 bytes!",
            },
        ),
        (
            "data_qmi_sized",
            Want {
                ptype: 1,
                src: (1, 0x4001),
                dst: (0, 0x2a),
                confirm_rx: false,
                payload: b"\x00\x34\x12\x24\x00\x07\x00\x02\x04\x00\x00\x00\x00\x00",
            },
        ),
    ];
    for (name, w) in &cases {
        used.insert(name.to_string());
        let bytes = &f[*name];
        let p = decode(bytes).unwrap_or_else(|e| panic!("{name}: {e:?}"));
        check_decoded(name, &p, PROTO_VER_1, w);
        let again = encode_v1(w.ptype, w.src, w.dst, w.confirm_rx, w.payload);
        assert_eq!(&again, bytes, "{name}: bytes");
        assert_eq!(bytes.len() % 4, 0);
        assert!(
            bytes[0] == 1 && bytes[1..4] == [0, 0, 0],
            "{name}: version word"
        );
    }

    // packets with a client in the control payload
    let clients = [
        (
            "del_client_local_port_closed",
            pkt_type::DEL_CLIENT,
            (1, 0x4001),
            (0, PORT_CTRL),
            (1, 0x4001),
        ),
        (
            "del_client_from_modem",
            pkt_type::DEL_CLIENT,
            (0, 0x2a),
            (1, PORT_CTRL),
            (0, 0x2a),
        ),
        (
            "bye_from_modem",
            pkt_type::BYE,
            (0, PORT_CTRL),
            (1, PORT_CTRL),
            (0, 0),
        ),
        (
            "resume_tx_to_modem",
            pkt_type::RESUME_TX,
            (1, 0x4001),
            (0, 0x2a),
            (1, 0x4001),
        ),
        (
            "resume_tx_from_modem",
            pkt_type::RESUME_TX,
            (0, 0x2a),
            (1, 0x4001),
            (0, 0x2a),
        ),
    ];
    for (name, ty, src, dst, (node, port)) in clients {
        used.insert(name.to_string());
        let payload = CtrlPkt::with_client(ty, node, port).to_bytes();
        let bytes = &f[name];
        let p = decode(bytes).unwrap();
        check_decoded(
            name,
            &p,
            PROTO_VER_1,
            &Want {
                ptype: ty,
                src,
                dst,
                confirm_rx: false,
                payload: &payload,
            },
        );
        assert_eq!(&encode_v1(ty, src, dst, false, &payload), bytes, "{name}");
    }

    let ping = CtrlPkt::new(pkt_type::PING).to_bytes();
    used.insert("ping_from_modem".to_string());
    let p = decode(&f["ping_from_modem"]).unwrap();
    assert_eq!(
        (p.ptype, p.src_node, p.dst_port, p.payload),
        (9, 0, PORT_CTRL, &ping[..])
    );

    // normalisations the receiver applies
    used.insert("hello_dst_legacy_ctrl_port".to_string());
    let p = decode(&f["hello_dst_legacy_ctrl_port"]).unwrap();
    assert_eq!(p.dst_port, PORT_CTRL);
    used.insert("hello_type_word_0x102".to_string());
    let p = decode(&f["hello_type_word_0x102"]).unwrap();
    assert_eq!(p.ptype, pkt_type::HELLO, "the C keeps the type in a u8");

    // what it refuses
    for (name, want) in [
        ("data_len0", PacketError::BadSize),
        ("hello_to_data_port", PacketError::NotForNameService),
        ("new_server_short_payload", PacketError::ShortControl),
        ("resume_tx_short_payload", PacketError::ShortControl),
    ] {
        used.insert(name.to_string());
        assert_eq!(decode(&f[name]).err(), Some(want), "{name}");
    }
    // a RESUME_TX goes to a data port and is fine; a data packet to the control port is fine too
    let d = encode_v1(pkt_type::DATA, (0, 1), (1, PORT_CTRL), false, b"abcd");
    assert!(decode(&d).is_ok());

    let v2_names: BTreeSet<String> = f.keys().filter(|k| k.starts_with("v2_")).cloned().collect();
    let ctrl_names: BTreeSet<String> = f
        .keys()
        .filter(|k| k.starts_with("ctrl_"))
        .cloned()
        .collect();
    for name in f.keys() {
        assert!(
            used.contains(name) || v2_names.contains(name) || ctrl_names.contains(name),
            "fixture {name} is not tested"
        );
    }
}

#[test]
fn v2_packets() {
    let f = fixtures();
    let hello = f["ctrl_hello"].clone();
    type V2<'a> = (
        &'a str,
        u8,
        bool,
        (u32, u32),
        (u32, u32),
        &'a [u8],
        &'a [u8],
        &'a [u8],
    );
    // (name, type, confirm_rx, src, dst, options, payload, expected payload)
    let opts: &[u8] = &[0xaa, 0xbb, 0xcc, 0xdd];
    let cases: Vec<V2<'_>> = vec![
        (
            "v2_data",
            1,
            false,
            (0, 0x2a),
            (1, 0x4001),
            &[],
            b"six!!!",
            b"six!!!",
        ),
        (
            "v2_data_confirm_rx",
            1,
            true,
            (0x0102, 0x0304),
            (0x0506, 0x0708),
            &[],
            b"abcd",
            b"abcd",
        ),
        (
            "v2_data_options",
            1,
            false,
            (0, 0x2a),
            (1, 0x4001),
            opts,
            b"opts",
            b"opts",
        ),
    ];
    for (name, ty, confirm, src, dst, options, payload, want_payload) in cases {
        let bytes = &f[name];
        assert_eq!(bytes[0], 3);
        let p = decode(bytes).unwrap_or_else(|e| panic!("{name}: {e:?}"));
        check_decoded(
            name,
            &p,
            PROTO_VER_2,
            &Want {
                ptype: u32::from(ty),
                src,
                dst,
                confirm_rx: confirm,
                payload: want_payload,
            },
        );
        assert_eq!(
            &encode_v2(ty, confirm, src, dst, options, payload).unwrap(),
            bytes,
            "{name}"
        );
    }
    // the 16-bit control port is widened, source and destination
    let p = decode(&f["v2_hello_ctrl_ports"]).unwrap();
    assert_eq!(
        (p.src_port, p.dst_port, p.ptype, p.payload),
        (PORT_CTRL, PORT_CTRL, 2, &hello[..])
    );
    assert_eq!(
        &encode_v2(2, false, (0, PORT_CTRL), (1, PORT_CTRL), &[], &hello).unwrap(),
        &f["v2_hello_ctrl_ports"]
    );
    // and the legacy 0xffff as a destination
    let p = decode(&f["v2_hello_legacy_ctrl"]).unwrap();
    assert_eq!((p.src_port, p.dst_port), (0x10, PORT_CTRL));
    // options longer than a byte cannot be written
    assert!(encode_v2(1, false, (0, 1), (1, 2), &[0; 256], b"abcd").is_none());
}

/// Every packet is refused when its length is not what its header says, and nothing panics on any change.
#[test]
fn damaged_packets_are_refused_without_panics() {
    let f = fixtures();
    for (name, bytes) in &f {
        if name.starts_with("ctrl_") {
            continue;
        }
        let good = decode(bytes).is_ok();
        if good {
            // shorter or longer by any amount: the length no longer fits
            for k in 0..bytes.len() {
                assert!(decode(&bytes[..k]).is_err(), "{name} cut at {k}");
            }
            for extra in 1..12 {
                let mut b = bytes.clone();
                b.extend(std::iter::repeat_n(0u8, extra));
                assert!(decode(&b).is_err(), "{name} +{extra}");
            }
        }
        // any byte changed to anything: whatever it decodes to, no panic and no out-of-range payload
        for i in 0..bytes.len() {
            for v in 0..=255u8 {
                let mut b = bytes.clone();
                b[i] = v;
                if let Ok(p) = decode(&b) {
                    assert!(!p.payload.is_empty() && p.payload.len() <= b.len());
                }
            }
        }
    }
}

#[test]
fn hostile_headers() {
    let base = encode_v1(pkt_type::DATA, (0, 1), (1, 2), false, b"abcd");
    let set = |off: usize, v: u32| {
        let mut b = base.clone();
        b[off..off + 4].copy_from_slice(&v.to_le_bytes());
        b
    };
    // sizes that overflow when added to the header, or do not fit
    for size in [
        0u32,
        5,
        0xffff_ffff,
        0xffff_fffc,
        0xffff_fffd,
        0x8000_0000,
        1 << 24,
    ] {
        assert_eq!(
            decode(&set(20, size)).err(),
            Some(PacketError::BadSize),
            "{size:#x}"
        );
    }
    assert!(decode(&set(20, 4)).is_ok());
    // versions
    for v in [0u32, 2, 4, 5, 0xff, 0x103] {
        let e = decode(&set(0, v)).err();
        let low = v as u8;
        if low == 1 || low == 3 {
            continue;
        }
        assert_eq!(e, Some(PacketError::BadVersion(low)), "version {v:#x}");
    }
    // the high bytes of the version word are not looked at
    assert!(decode(&set(0, 0xabcd_ef01)).is_ok());
    // lengths
    assert_eq!(decode(&[]).err(), Some(PacketError::BadLength));
    assert_eq!(decode(&base[..31]).err(), Some(PacketError::BadLength));
    assert_eq!(decode(&base[..28]).err(), Some(PacketError::ShortHeader));
    assert_eq!(decode(&[3, 0, 0, 0]).err(), Some(PacketError::ShortHeader));
    // v2: an option length longer than the packet
    let mut v2 = encode_v2(1, false, (0, 1), (1, 2), &[], b"abcd").unwrap();
    for optlen in 0..=255u8 {
        v2[3] = optlen;
        let _ = decode(&v2);
    }
    v2[3] = 255;
    assert!(decode(&v2).is_err());
    // two packets in one message are one bad packet
    let mut two = base.clone();
    two.extend_from_slice(&base);
    assert_eq!(decode(&two).err(), Some(PacketError::BadSize));
}

#[test]
fn random_bytes_never_panic() {
    let mut s = 0x1234_5678_9abc_def1u64;
    let mut next = || {
        s ^= s >> 12;
        s ^= s << 25;
        s ^= s >> 27;
        s.wrapping_mul(0x2545_f491_4f6c_dd1d)
    };
    for _ in 0..50_000 {
        let n = (next() % 24) as usize * 4;
        let mut b: Vec<u8> = (0..n).map(|_| next() as u8).collect();
        // make a good start likely
        if n >= 8 && next() % 2 == 0 {
            b[0] = [1, 3][(next() % 2) as usize];
            b[1] = 0;
            b[2] = 0;
            b[3] = if b[0] == 3 { (next() % 8) as u8 } else { 0 };
        }
        let _ = decode(&b);
        let _ = CtrlPkt::parse(&b);
    }
    let _: Packet<'_> = decode(&packet::encode_v1(1, (0, 0), (0, 0), false, b"x")).unwrap();
}
