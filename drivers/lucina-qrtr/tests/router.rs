//! The router: two of them back to back, the byte goldens from the C structs, the limits of `ns.c`, flow
//! control, and hostile input.
//!
//! Node 1 ("apps", router `a`) is the host, node 0 ("modem", router `b`) the remote. The channel is a message
//! pipe, as GLINK is: each `poll_tx` result is one message and arrives as one `handle_rx` call.

use lucina_qrtr::packet::{
    self, CtrlPkt, EP_NID_AUTO, NODE_BCAST, PORT_CTRL, Server, TX_FLOW_HIGH, decode, encode_v1,
    pkt_type,
};
use lucina_qrtr::router::{
    BindError, Event, NS_MAX_LOOKUPS, NS_MAX_NODES, NS_MAX_SERVERS, RX_QUEUE_LIMIT, Router,
    RxError, SendError,
};
use std::collections::BTreeMap;

const PACKETS: &str = include_str!("fixtures/packets.txt");

const WLFW: u32 = 69;
/// QMI version 1, instance 0: `version | instance << 8`.
const WLFW_INSTANCE: u32 = 1;

fn fx(name: &str) -> Vec<u8> {
    let f: BTreeMap<&str, &str> = PACKETS
        .lines()
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .map(|l| l.split_once(' ').unwrap())
        .collect();
    let h = f.get(name).unwrap_or_else(|| panic!("no fixture {name}"));
    (0..h.len() / 2)
        .map(|i| u8::from_str_radix(&h[2 * i..2 * i + 2], 16).unwrap())
        .collect()
}

fn drain_tx(r: &mut Router) -> Vec<Vec<u8>> {
    std::iter::from_fn(|| r.poll_tx()).collect()
}

fn drain_events(r: &mut Router) -> Vec<Event> {
    std::iter::from_fn(|| r.poll_event()).collect()
}

fn server(service: u32, instance: u32, node: u32, port: u32) -> Server {
    Server {
        service,
        instance,
        node,
        port,
    }
}

struct Link {
    a: Router,
    b: Router,
}

impl Link {
    fn new() -> Link {
        Link {
            a: Router::new(1),
            b: Router::new(0),
        }
    }

    /// Carry messages both ways until neither side has more. Returns (a to b, b to a).
    fn pump(&mut self) -> (Vec<Vec<u8>>, Vec<Vec<u8>>) {
        let (mut ab, mut ba) = (Vec::new(), Vec::new());
        for _ in 0..50 {
            let pa = drain_tx(&mut self.a);
            let pb = drain_tx(&mut self.b);
            if pa.is_empty() && pb.is_empty() {
                return (ab, ba);
            }
            for p in pa {
                self.b.handle_rx(&p).expect("b accepts what a sends");
                ab.push(p);
            }
            for p in pb {
                self.a.handle_rx(&p).expect("a accepts what b sends");
                ba.push(p);
            }
        }
        panic!("the two routers never go quiet");
    }

    /// Both ends up, HELLOs exchanged, nothing pending.
    fn up() -> Link {
        let mut l = Link::new();
        l.a.endpoint_up();
        l.b.endpoint_up();
        l.pump();
        drain_events(&mut l.a);
        drain_events(&mut l.b);
        l
    }
}

#[test]
fn hello_and_service_exchange_matches_the_c_bytes() {
    let mut l = Link::new();
    // the modem has WLFW; the host's QMI client wants it
    assert_eq!(l.b.bind(0x2a), Ok(0x2a));
    l.b.publish(0x2a, WLFW, WLFW_INSTANCE).unwrap();
    assert_eq!(l.a.bind(0x4001), Ok(0x4001));
    l.a.lookup(0x4001, WLFW, WLFW_INSTANCE).unwrap();
    // nothing known yet: the list is empty
    assert_eq!(drain_events(&mut l.a), [Event::LookupDone { port: 0x4001 }]);

    l.a.endpoint_up();
    l.b.endpoint_up();
    // each side opens with HELLO, to a node whose number it does not know yet
    let from_a = drain_tx(&mut l.a);
    let from_b = drain_tx(&mut l.b);
    assert_eq!(from_a, [fx("hello_to_unknown_node")]);
    assert_eq!(from_b, [fx("hello_from_node0_to_unknown_node")]);
    assert_eq!(l.a.remote_node(), None);

    l.b.handle_rx(&from_a[0]).unwrap();
    l.a.handle_rx(&from_b[0]).unwrap();
    assert_eq!(l.a.remote_node(), Some(0));
    assert_eq!(l.b.remote_node(), Some(1));

    // the answers: HELLO once, then the servers of the node (one on the modem, none on the host)
    let (ab, ba) = l.pump();
    assert_eq!(ab, [fx("hello_to_node0")]);
    // the modem announces its servers on each of the two HELLOs it hears
    assert_eq!(
        ba,
        [
            fx("hello_from_node0"),
            fx("new_server_from_modem"),
            fx("new_server_from_modem")
        ]
    );

    // the host's lookup hears about it
    let ev = drain_events(&mut l.a);
    let want = Event::ServiceNew {
        port: 0x4001,
        server: server(WLFW, WLFW_INSTANCE, 0, 0x2a),
    };
    assert_eq!(ev, [want.clone(), want]);
    assert_eq!(l.a.servers(), [server(WLFW, WLFW_INSTANCE, 0, 0x2a)]);

    // QMI-sized data, host to modem; the bytes are the C's, padded
    let qmi = b"\x00\x34\x12\x24\x00\x07\x00\x02\x04\x00\x00\x00\x00\x00";
    l.a.send(0x4001, 0, 0x2a, qmi).unwrap();
    let pkt = drain_tx(&mut l.a);
    assert_eq!(pkt, [fx("data_qmi_sized")]);
    l.b.handle_rx(&pkt[0]).unwrap();
    assert_eq!(
        drain_events(&mut l.b),
        [Event::Data {
            port: 0x2a,
            src_node: 1,
            src_port: 0x4001,
            data: qmi.to_vec()
        }]
    );

    // and modem to host
    l.b.send(0x2a, 1, 0x4001, b"7 bytes").unwrap();
    let pkt = drain_tx(&mut l.b);
    assert_eq!(pkt, [fx("data_len7")]);
    l.a.handle_rx(&pkt[0]).unwrap();
    assert_eq!(
        drain_events(&mut l.a),
        [Event::Data {
            port: 0x4001,
            src_node: 0,
            src_port: 0x2a,
            data: b"7 bytes".to_vec()
        }]
    );
}

#[test]
fn a_late_lookup_gets_the_list_then_the_end_marker() {
    let mut l = Link::up();
    l.b.bind(0x2a).unwrap();
    l.b.publish(0x2a, WLFW, WLFW_INSTANCE).unwrap();
    l.b.bind(0x2b).unwrap();
    l.b.publish(0x2b, 64, 1).unwrap();
    let (_, ba) = l.pump();
    // the modem's new local servers are announced at once
    assert_eq!(ba.len(), 2);
    assert!(
        ba.iter()
            .all(|p| decode(p).unwrap().ptype == pkt_type::NEW_SERVER)
    );
    assert_eq!(l.a.servers().len(), 2);

    l.a.bind(0x4001).unwrap();
    l.a.lookup(0x4001, WLFW, 0).unwrap();
    assert_eq!(
        drain_events(&mut l.a),
        [
            Event::ServiceNew {
                port: 0x4001,
                server: server(WLFW, WLFW_INSTANCE, 0, 0x2a)
            },
            Event::LookupDone { port: 0x4001 }
        ]
    );
    // every service: node and port order
    l.a.bind(0x4002).unwrap();
    l.a.lookup(0x4002, 0, 0).unwrap();
    assert_eq!(
        drain_events(&mut l.a),
        [
            Event::ServiceNew {
                port: 0x4002,
                server: server(WLFW, WLFW_INSTANCE, 0, 0x2a)
            },
            Event::ServiceNew {
                port: 0x4002,
                server: server(64, 1, 0, 0x2b)
            },
            Event::LookupDone { port: 0x4002 }
        ]
    );
}

#[test]
fn local_servers_are_announced_to_the_modem() {
    let mut r = Router::new(1);
    r.bind(0x4001).unwrap();
    // before any endpoint: only the name service of this node knows
    r.publish(0x4001, WLFW, WLFW_INSTANCE).unwrap();
    assert_eq!(r.poll_tx(), None);

    r.endpoint_up();
    assert_eq!(drain_tx(&mut r), [fx("hello_to_unknown_node")]);
    // the modem says HELLO: ours answers it, then announces the server
    r.handle_rx(&fx("hello_from_node0")).unwrap();
    assert_eq!(
        drain_tx(&mut r),
        [fx("hello_to_node0"), fx("new_server_announce_local")]
    );

    // withdrawn: DEL_SERVER goes out
    r.unpublish(0x4001, WLFW, WLFW_INSTANCE).unwrap();
    assert_eq!(drain_tx(&mut r), [fx("del_server_announce_local")]);
    assert!(r.servers().is_empty());
    // withdrawing again is nothing
    r.unpublish(0x4001, WLFW, WLFW_INSTANCE).unwrap();
    assert_eq!(r.poll_tx(), None);

    // a new server before the node id is known goes to "node -1", which the modem reads as itself
    let mut r = Router::new(1);
    r.bind(0x4001).unwrap();
    r.endpoint_up();
    drain_tx(&mut r);
    r.publish(0x4001, WLFW, WLFW_INSTANCE).unwrap();
    let p = drain_tx(&mut r);
    assert_eq!(p.len(), 1);
    let d = decode(&p[0]).unwrap();
    assert_eq!(
        (d.ptype, d.dst_node, d.dst_port),
        (pkt_type::NEW_SERVER, EP_NID_AUTO, PORT_CTRL)
    );
}

#[test]
fn closing_a_port_says_del_client() {
    let mut l = Link::up();
    l.b.bind(0x2a).unwrap();
    l.b.publish(0x2a, WLFW, WLFW_INSTANCE).unwrap();
    l.a.bind(0x4001).unwrap();
    l.pump();
    l.a.lookup(0x4001, WLFW, WLFW_INSTANCE).unwrap();
    assert_eq!(drain_events(&mut l.a).len(), 2); // the server and the end marker

    // the client closes: DEL_CLIENT out, byte for byte
    assert!(l.a.close(0x4001));
    assert_eq!(drain_tx(&mut l.a), [fx("del_client_local_port_closed")]);
    assert!(!l.a.close(0x4001));
    // its lookup is gone: a new server is not reported to a port that is not there
    l.b.bind(0x2b).unwrap();
    l.b.publish(0x2b, WLFW, WLFW_INSTANCE).unwrap();
    l.pump();
    assert_eq!(drain_events(&mut l.a), []);
    assert_eq!(l.a.servers().len(), 2);

    // the server closes: DEL_CLIENT, no DEL_SERVER, and the host's name service drops the server
    l.a.bind(0x4003).unwrap();
    l.a.lookup(0x4003, WLFW, WLFW_INSTANCE).unwrap();
    drain_events(&mut l.a);
    assert!(l.b.close(0x2a));
    let out = drain_tx(&mut l.b);
    assert_eq!(out, [fx("del_client_from_modem")]);
    l.a.handle_rx(&out[0]).unwrap();
    assert_eq!(
        drain_events(&mut l.a),
        [Event::ServiceDel {
            port: 0x4003,
            server: server(WLFW, WLFW_INSTANCE, 0, 0x2a)
        }]
    );
    assert_eq!(l.a.servers(), [server(WLFW, WLFW_INSTANCE, 0, 0x2b)]);
}

#[test]
fn a_withdrawn_server_reaches_lookups() {
    let mut l = Link::up();
    l.b.bind(0x2a).unwrap();
    l.b.publish(0x2a, WLFW, WLFW_INSTANCE).unwrap();
    l.a.bind(0x4001).unwrap();
    l.pump();
    l.a.lookup(0x4001, WLFW, WLFW_INSTANCE).unwrap();
    drain_events(&mut l.a);
    l.b.unpublish(0x2a, WLFW, WLFW_INSTANCE).unwrap();
    let (_, ba) = l.pump();
    assert_eq!(ba, [fx("del_server_from_modem")]);
    assert_eq!(
        drain_events(&mut l.a),
        [Event::ServiceDel {
            port: 0x4001,
            server: server(WLFW, WLFW_INSTANCE, 0, 0x2a)
        }]
    );
    assert!(l.a.servers().is_empty());
    // a lookup that was withdrawn hears nothing more
    l.a.del_lookup(0x4001, WLFW, WLFW_INSTANCE).unwrap();
    l.b.publish(0x2a, WLFW, WLFW_INSTANCE).unwrap();
    l.pump();
    assert_eq!(drain_events(&mut l.a), []);
    assert_eq!(l.a.servers().len(), 1);
}

#[test]
fn losing_the_endpoint_is_a_bye_for_every_node() {
    let mut l = Link::up();
    l.b.bind(0x2a).unwrap();
    l.b.publish(0x2a, WLFW, WLFW_INSTANCE).unwrap();
    l.a.bind(0x4001).unwrap();
    l.a.bind(0x4002).unwrap();
    l.pump();
    l.a.lookup(0x4001, WLFW, 0).unwrap();
    // a local server hears BYE as well
    l.a.publish(0x4002, 7, 1).unwrap();
    drain_events(&mut l.a);
    drain_tx(&mut l.a);

    l.a.endpoint_down();
    assert!(!l.a.is_up());
    assert_eq!(
        drain_events(&mut l.a),
        [
            Event::ServiceDel {
                port: 0x4001,
                server: server(WLFW, WLFW_INSTANCE, 0, 0x2a)
            },
            Event::Bye {
                port: 0x4002,
                node: 0
            }
        ]
    );
    assert_eq!(l.a.servers(), [server(7, 1, 1, 0x4002)]);
    // nothing to send to, nothing to receive from
    assert_eq!(l.a.poll_tx(), None);
    assert_eq!(l.a.send(0x4001, 0, 0x2a, b"x"), Err(SendError::NoRoute));
    assert_eq!(
        l.a.handle_rx(&fx("hello_from_node0")),
        Err(RxError::EndpointDown)
    );
    // and back up: a fresh session says HELLO again and answers a HELLO again
    l.a.endpoint_up();
    assert_eq!(drain_tx(&mut l.a), [fx("hello_to_unknown_node")]);
    l.a.handle_rx(&fx("hello_from_node0")).unwrap();
    assert_eq!(drain_tx(&mut l.a)[0], fx("hello_to_node0"));
}

#[test]
fn remote_control_packets_reach_local_ports() {
    let mut r = Router::new(1);
    r.bind(0x4001).unwrap();
    r.bind(0x4002).unwrap();
    r.endpoint_up();
    drain_tx(&mut r);
    r.lookup(0x4001, WLFW, WLFW_INSTANCE).unwrap();
    r.publish(0x4002, 7, 1).unwrap();
    assert_eq!(drain_events(&mut r), [Event::LookupDone { port: 0x4001 }]);
    drain_tx(&mut r);

    // the modem announces a server, then takes it away
    r.handle_rx(&fx("new_server_from_modem")).unwrap();
    assert_eq!(
        drain_events(&mut r),
        [Event::ServiceNew {
            port: 0x4001,
            server: server(WLFW, WLFW_INSTANCE, 0, 0x2a)
        }]
    );
    r.handle_rx(&fx("del_server_from_modem")).unwrap();
    assert_eq!(
        drain_events(&mut r),
        [Event::ServiceDel {
            port: 0x4001,
            server: server(WLFW, WLFW_INSTANCE, 0, 0x2a)
        }]
    );
    // its client went away: the local server is told
    r.handle_rx(&fx("del_client_from_modem")).unwrap();
    assert_eq!(
        drain_events(&mut r),
        [Event::DelClient {
            port: 0x4002,
            node: 0,
            client_port: 0x2a
        }]
    );
    r.handle_rx(&fx("bye_from_modem")).unwrap();
    assert_eq!(
        drain_events(&mut r),
        [Event::Bye {
            port: 0x4002,
            node: 0
        }]
    );

    // lookups from the other side are not served; pings and unknown commands do nothing
    r.handle_rx(&fx("new_lookup_from_modem")).unwrap();
    r.handle_rx(&fx("del_lookup_from_modem")).unwrap();
    r.handle_rx(&fx("ping_from_modem")).unwrap();
    assert_eq!(drain_events(&mut r), []);
    assert_eq!(drain_tx(&mut r), Vec::<Vec<u8>>::new());

    // the dangerous ones are refused up front
    assert!(matches!(
        r.handle_rx(&fx("new_server_short_payload")),
        Err(RxError::Packet(_))
    ));
    assert!(matches!(
        r.handle_rx(&fx("data_len0")),
        Err(RxError::Packet(_))
    ));
    assert!(matches!(
        r.handle_rx(&fx("hello_to_data_port")),
        Err(RxError::Packet(_))
    ));
    // the old control port number and a wide type word are normalised, then it is a HELLO
    r.handle_rx(&fx("hello_dst_legacy_ctrl_port")).unwrap();
    assert_eq!(
        drain_tx(&mut r),
        [
            fx("hello_to_node0"),
            packet::encode_v1(
                pkt_type::NEW_SERVER,
                (1, PORT_CTRL),
                (0, PORT_CTRL),
                false,
                &CtrlPkt::with_server(pkt_type::NEW_SERVER, server(7, 1, 1, 0x4002)).to_bytes()
            )
        ]
    );
}

#[test]
fn a_server_on_a_new_node_makes_the_node_routable() {
    let mut r = Router::new(1);
    r.bind(0x4001).unwrap();
    r.endpoint_up();
    drain_tx(&mut r);
    assert_eq!(r.send(0x4001, 5, 9, b"x"), Err(SendError::NoRoute));
    // a bridge: the modem announces a service on node 5
    let p = packet::encode_v1(
        pkt_type::NEW_SERVER,
        (0, PORT_CTRL),
        (1, PORT_CTRL),
        false,
        &CtrlPkt::with_server(pkt_type::NEW_SERVER, server(40, 1, 5, 9)).to_bytes(),
    );
    r.handle_rx(&p).unwrap();
    assert_eq!(r.send(0x4001, 5, 9, b"x"), Ok(()));
    // it goes out the one endpoint
    let out = drain_tx(&mut r);
    let d = decode(&out[0]).unwrap();
    assert_eq!((d.dst_node, d.dst_port), (5, 9));
}

#[test]
fn flow_control_follows_qrtr_tx_wait() {
    let mut l = Link::up();
    l.b.bind(0x2a).unwrap();
    l.a.bind(0x4001).unwrap();
    // first four: no confirmation; the fifth asks for one
    for _ in 0..4 {
        l.a.send(0x4001, 0, 0x2a, b"abcd").unwrap();
        assert_eq!(drain_tx(&mut l.a), [fx("data_len4")]);
    }
    l.a.send(0x4001, 0, 0x2a, b"hello").unwrap();
    assert_eq!(drain_tx(&mut l.a), [fx("data_len5_confirm_rx")]);
    // up to ten may be outstanding
    for _ in 5..TX_FLOW_HIGH {
        l.a.send(0x4001, 0, 0x2a, b"abcd").unwrap();
    }
    let sent = drain_tx(&mut l.a);
    assert_eq!(sent.len() as u32, TX_FLOW_HIGH - 5);
    // the next one has to wait
    assert_eq!(
        l.a.send(0x4001, 0, 0x2a, b"abcd"),
        Err(SendError::WouldBlock)
    );
    assert_eq!(l.a.poll_tx(), None);
    // another destination is a flow of its own
    assert!(l.a.send(0x4001, 0, 0x2b, b"abcd").is_ok());
    drain_tx(&mut l.a);

    // the receiver gets all ten (the first five are the ones above, re-sent in order)
    let mut all = vec![fx("data_len4"); 4];
    all.push(fx("data_len5_confirm_rx"));
    all.extend(sent);
    for p in &all {
        l.b.handle_rx(p).unwrap();
    }
    // nothing is confirmed until the port is read
    assert_eq!(l.b.poll_tx(), None);
    for i in 0..10 {
        let ev = l.b.poll_event().unwrap();
        assert!(matches!(ev, Event::Data { port: 0x2a, .. }), "{i}");
        if i == 4 {
            // reading the packet that asked: RESUME_TX, byte for byte
            assert_eq!(drain_tx(&mut l.b), [fx("resume_tx_from_modem")]);
        } else {
            assert_eq!(l.b.poll_tx(), None, "{i}");
        }
    }
    // the sender resumes
    l.a.handle_rx(&fx("resume_tx_from_modem")).unwrap();
    assert_eq!(
        drain_events(&mut l.a),
        [Event::TxResumed {
            node: 0,
            port: 0x2a
        }]
    );
    l.a.send(0x4001, 0, 0x2a, b"abcd").unwrap();
    // and counts from zero: the fifth after that asks again
    for _ in 0..3 {
        l.a.send(0x4001, 0, 0x2a, b"abcd").unwrap();
    }
    drain_tx(&mut l.a);
    l.a.send(0x4001, 0, 0x2a, b"hello").unwrap();
    assert_eq!(drain_tx(&mut l.a), [fx("data_len5_confirm_rx")]);

    // a resume for a flow that does not exist is nothing
    l.a.handle_rx(&fx("resume_tx_to_modem")).unwrap();
    assert_eq!(drain_events(&mut l.a), []);
}

#[test]
fn a_lost_confirmation_is_replaced() {
    let mut l = Link::up();
    l.a.bind(0x4001).unwrap();
    for _ in 0..2 {
        l.a.send(0x4001, 0, 0x2a, b"abcd").unwrap();
    }
    drain_tx(&mut l.a);
    // the endpoint dropped one: the next carries confirm_rx and does not count
    l.a.tx_failed(0, 0x2a);
    l.a.send(0x4001, 0, 0x2a, b"abcd").unwrap();
    let confirm = |p: &[u8]| decode(p).unwrap().confirm_rx;
    let p = drain_tx(&mut l.a);
    assert!(confirm(&p[0]));
    // pending is still 2: the 3rd, 4th, 5th more reach 5 on the third
    let mut flags = Vec::new();
    for _ in 0..4 {
        l.a.send(0x4001, 0, 0x2a, b"abcd").unwrap();
        flags.push(confirm(&drain_tx(&mut l.a)[0]));
    }
    assert_eq!(flags, [false, false, true, false]);

    // even a full flow is let through by it
    for _ in 0..(TX_FLOW_HIGH - 6) {
        l.a.send(0x4001, 0, 0x2a, b"abcd").unwrap();
    }
    assert_eq!(
        l.a.send(0x4001, 0, 0x2a, b"abcd"),
        Err(SendError::WouldBlock)
    );
    l.a.tx_failed(0, 0x2a);
    assert_eq!(l.a.send(0x4001, 0, 0x2a, b"abcd"), Ok(()));
    // unknown flows are not made up
    l.a.tx_failed(9, 9);
}

#[test]
fn only_data_is_flow_controlled() {
    let mut l = Link::up();
    l.b.bind(0x2a).unwrap();
    // many name-service packets in a row never block and never ask for confirmation
    for i in 0..50u32 {
        l.b.publish(0x2a, 100 + i, 1).unwrap();
    }
    for p in drain_tx(&mut l.b) {
        let d = decode(&p).unwrap();
        assert!(!d.confirm_rx);
    }
}

#[test]
fn the_hello_reply_is_sent_once_per_session() {
    let mut r = Router::new(1);
    r.endpoint_up();
    assert_eq!(drain_tx(&mut r).len(), 1);
    r.handle_rx(&fx("hello_from_node0")).unwrap();
    assert_eq!(drain_tx(&mut r), [fx("hello_to_node0")]);
    r.handle_rx(&fx("hello_from_node0")).unwrap();
    assert_eq!(drain_tx(&mut r), Vec::<Vec<u8>>::new());
}

#[test]
fn filters_of_the_name_service() {
    let mut r = Router::new(1);
    for p in 0x4000..0x4003 {
        r.bind(p).unwrap();
    }
    r.publish(0x4000, 69, 1).unwrap();
    r.publish(0x4001, 69, 2).unwrap();
    r.publish(0x4002, 70, 1).unwrap();
    for (service, instance, want) in [
        (0, 0, 3),
        (69, 0, 2),
        (69, 1, 1),
        (69, 2, 1),
        (0, 1, 2),
        (70, 0, 1),
        (69, 3, 0),
        (71, 0, 0),
    ] {
        let port = r.bind(0).unwrap();
        r.lookup(port, service, instance).unwrap();
        let ev = drain_events(&mut r);
        assert_eq!(ev.len(), want + 1, "lookup {service}/{instance}: {ev:?}");
        assert_eq!(ev.last(), Some(&Event::LookupDone { port }));
        r.close(port);
        // every local server hears that a client went away
        assert_eq!(drain_events(&mut r).len(), 3);
    }

    // later arrivals and departures follow a lookup of the same filter, and only that one
    let a = r.bind(0).unwrap();
    let b = r.bind(0).unwrap();
    r.lookup(a, 80, 0).unwrap();
    r.lookup(b, 80, 5).unwrap();
    drain_events(&mut r);
    r.publish(0x4000, 80, 6).unwrap();
    assert_eq!(
        drain_events(&mut r),
        [Event::ServiceNew {
            port: a,
            server: server(80, 6, 1, 0x4000)
        }]
    );
    // the same port publishing again replaces its server
    r.publish(0x4000, 80, 5).unwrap();
    let ev = drain_events(&mut r);
    assert!(ev.contains(&Event::ServiceNew {
        port: b,
        server: server(80, 5, 1, 0x4000)
    }));
    assert_eq!(r.servers().iter().filter(|s| s.service == 80).count(), 1);
    // a service of 0 or from port 0 is not accepted
    assert!(r.publish(0x4000, 0, 1).is_ok());
    assert_eq!(r.servers().iter().filter(|s| s.service == 0).count(), 0);
    // del_lookup with another service name leaves the lookup alone
    r.del_lookup(a, 81, 0).unwrap();
    r.unpublish(0x4000, 80, 5).unwrap();
    assert_eq!(drain_events(&mut r).len(), 2);
}

#[test]
fn ports() {
    let mut r = Router::new(1);
    assert_eq!(r.bind(0), Ok(0x4000));
    assert_eq!(r.bind(0), Ok(0x4001));
    assert_eq!(r.bind(0x4001), Err(BindError::InUse));
    assert_eq!(r.bind(PORT_CTRL), Err(BindError::InUse));
    assert_eq!(r.bind(0x10), Ok(0x10));
    assert_eq!(r.bind(0xa000), Ok(0xa000));
    assert!(r.close(0x4000));
    assert_eq!(r.bind(0), Ok(0x4000), "the lowest free one");
    // all of them
    let mut r = Router::new(1);
    for p in 0x4000..=0x7fff {
        assert_eq!(r.bind(p), Ok(p));
    }
    assert_eq!(r.bind(0), Err(BindError::NoFreePort));
    assert_eq!(r.bind(0x8000), Ok(0x8000));
    r.close(0x5000);
    assert_eq!(r.bind(0), Ok(0x5000));
}

#[test]
fn sending_errors_and_local_delivery() {
    let mut r = Router::new(1);
    let a = r.bind(0).unwrap();
    let b = r.bind(0).unwrap();
    assert_eq!(r.send(0x999, 1, b, b"x"), Err(SendError::NotBound));
    assert_eq!(r.send(a, 1, b, &vec![0; 65536]), Err(SendError::TooLong));
    assert_eq!(r.send(a, NODE_BCAST, b, b"x"), Err(SendError::NotConnected));
    assert_eq!(r.send(a, 7, b, b"x"), Err(SendError::NoRoute));
    assert_eq!(r.send(a, 1, 0x5555, b"x"), Err(SendError::NoPort));
    assert_eq!(
        r.send(a, 1, a, b"x"),
        Err(SendError::NoPort),
        "not to itself"
    );
    // the longest message
    assert_eq!(r.send(a, 1, b, &vec![7; 65535]), Ok(()));
    assert_eq!(
        r.send(a, 1, b, b""),
        Ok(()),
        "an empty local message is a message"
    );
    let ev = drain_events(&mut r);
    assert_eq!(ev.len(), 2);
    assert!(
        matches!(&ev[0], Event::Data { port, src_node: 1, src_port, data } if *port == b && *src_port == a && data.len() == 65535)
    );

    // a full queue refuses, and closing the port empties it
    for _ in 0..RX_QUEUE_LIMIT {
        r.send(a, 1, b, b"x").unwrap();
    }
    assert_eq!(r.send(a, 1, b, b"x"), Err(SendError::QueueFull));
    assert!(r.close(b));
    assert_eq!(r.poll_event(), None);
    assert_eq!(r.send(a, 1, b, b"x"), Err(SendError::NoPort));
}

#[test]
fn the_remote_cannot_fill_a_port_or_the_tables() {
    let mut r = Router::new(1);
    r.bind(0x4001).unwrap();
    r.endpoint_up();
    drain_tx(&mut r);
    let data = encode_v1(pkt_type::DATA, (0, 0x2a), (1, 0x4001), false, b"abcd");
    for _ in 0..RX_QUEUE_LIMIT {
        r.handle_rx(&data).unwrap();
    }
    assert_eq!(r.handle_rx(&data), Err(RxError::QueueFull));
    let unbound = encode_v1(pkt_type::DATA, (0, 0x2a), (1, 0x4002), false, b"abcd");
    assert_eq!(r.handle_rx(&unbound), Err(RxError::NoPort));
    assert_eq!(drain_events(&mut r).len(), RX_QUEUE_LIMIT);

    // servers: 256 per node, then a new port is refused but an old one can be replaced
    let new_server = |node: u32, port: u32, service: u32| {
        encode_v1(
            pkt_type::NEW_SERVER,
            (0, PORT_CTRL),
            (1, PORT_CTRL),
            false,
            &CtrlPkt::with_server(pkt_type::NEW_SERVER, server(service, 1, node, port)).to_bytes(),
        )
    };
    for port in 1..=(NS_MAX_SERVERS as u32 + 10) {
        r.handle_rx(&new_server(0, port, 10)).unwrap();
    }
    assert_eq!(r.servers().len(), NS_MAX_SERVERS);
    r.handle_rx(&new_server(0, 1, 11)).unwrap();
    assert_eq!(r.servers()[0].service, 11);
    assert_eq!(r.servers().len(), NS_MAX_SERVERS);

    // nodes: 512 in the name service. Node 0 above took one.
    let mut r = Router::new(1);
    r.endpoint_up();
    for node in 1..=(NS_MAX_NODES as u32 + 100) {
        r.handle_rx(&new_server(node, 1, 10)).unwrap();
    }
    assert_eq!(r.servers().len(), NS_MAX_NODES);

    // lookups: 128
    let mut r = Router::new(1);
    let ports: Vec<u32> = (0..NS_MAX_LOOKUPS + 2)
        .map(|_| r.bind(0).unwrap())
        .collect();
    for &p in &ports {
        r.lookup(p, 5, 0).unwrap();
    }
    let ends = drain_events(&mut r);
    // the last two lookups were not taken, so they heard nothing, not even the end of the list
    assert_eq!(ends.len(), NS_MAX_LOOKUPS);
    r.bind(0x10).unwrap();
    r.publish(0x10, 5, 1).unwrap();
    assert_eq!(drain_events(&mut r).len(), NS_MAX_LOOKUPS);

    // node ids an endpoint can claim: bounded
    let mut r = Router::new(1);
    r.bind(0x4001).unwrap();
    r.endpoint_up();
    for node in 100..3000u32 {
        let p = encode_v1(pkt_type::DATA, (node, 1), (1, 0x4001), false, b"abcd");
        r.handle_rx(&p).unwrap();
        drain_events(&mut r);
    }
    assert_eq!(r.send(0x4001, 100, 1, b"x"), Ok(()));
    assert_eq!(r.send(0x4001, 2999, 1, b"x"), Err(SendError::NoRoute));
}

#[test]
fn a_remote_cannot_pose_as_a_local_client() {
    let mut r = Router::new(1);
    r.bind(0x4001).unwrap();
    r.bind(0x4002).unwrap();
    r.endpoint_up();
    r.publish(0x4002, 9, 1).unwrap();
    drain_tx(&mut r);
    // DEL_CLIENT for a local port, from the remote: refused as spoofed
    let spoof = encode_v1(
        pkt_type::DEL_CLIENT,
        (0, 0x30),
        (1, PORT_CTRL),
        false,
        &CtrlPkt::with_client(pkt_type::DEL_CLIENT, 1, 0x4002).to_bytes(),
    );
    r.handle_rx(&spoof).unwrap();
    assert_eq!(r.servers(), [server(9, 1, 1, 0x4002)]);
    // a local lookup is only taken from local ports: a NEW_LOOKUP naming local node 1 as its source node is
    // from the remote's own node 0 here, so it is dropped
    r.handle_rx(&fx("new_lookup_from_modem")).unwrap();
    assert_eq!(drain_tx(&mut r), Vec::<Vec<u8>>::new());
}

#[test]
fn nothing_hostile_panics_or_breaks_the_invariants() {
    let mut s = 0xfeed_beef_1234_5678u64;
    let mut next = move || {
        s ^= s >> 12;
        s ^= s << 25;
        s ^= s >> 27;
        s.wrapping_mul(0x2545_f491_4f6c_dd1d)
    };
    let mut l = Link::up();
    for p in [0x2a, 0x2b] {
        l.b.bind(p).unwrap();
    }
    for p in [0x4001, 0x4002] {
        l.a.bind(p).unwrap();
    }
    let ports_a = [0x4001u32, 0x4002, 0x4003, 0x10];
    let nodes = [0u32, 1, 2, 5, NODE_BCAST];
    let ports = [0x2au32, 0x2b, 0x4001, 0x4002, 0x30, PORT_CTRL, 0, 1];
    for round in 0..12_000 {
        // random packets from the remote, shaped so that the deep paths run
        let cmd = [1u32, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 0][(next() % 13) as usize];
        let words: Vec<u8> = (0..(next() % 6) * 4)
            .map(|_| {
                if next() % 2 == 0 {
                    [0, 1, 0x2a, 0x45, 0xff][(next() % 5) as usize]
                } else {
                    next() as u8
                }
            })
            .collect();
        let mut payload = cmd.to_le_bytes().to_vec();
        payload.extend_from_slice(&words);
        let ptype = if next() % 3 == 0 { pkt_type::DATA } else { cmd };
        let pick = |n: u64, from: &[u32]| from[(n % from.len() as u64) as usize];
        let pkt = encode_v1(
            ptype,
            (pick(next(), &nodes), pick(next(), &ports)),
            (pick(next(), &nodes), pick(next(), &ports)),
            next() % 4 == 0,
            &payload,
        );
        let _ = l.a.handle_rx(&pkt);
        if next() % 8 == 0 {
            // some raw damage too
            let mut bad = pkt;
            let i = (next() % bad.len() as u64) as usize;
            bad[i] = next() as u8;
            let _ = l.a.handle_rx(&bad);
        }
        // random local calls
        let port = pick(next(), &ports_a);
        match next() % 12 {
            0 => {
                let _ = l.a.bind(port);
            }
            1 => {
                l.a.close(port);
            }
            2 => {
                let _ =
                    l.a.send(port, pick(next(), &nodes), pick(next(), &ports), &payload);
            }
            3 => {
                let _ = l.a.publish(port, (next() % 4) as u32, (next() % 3) as u32);
            }
            4 => {
                let _ =
                    l.a.unpublish(port, (next() % 4) as u32, (next() % 3) as u32);
            }
            5 => {
                let _ = l.a.lookup(port, (next() % 4) as u32, (next() % 3) as u32);
            }
            6 => {
                let _ =
                    l.a.del_lookup(port, (next() % 4) as u32, (next() % 3) as u32);
            }
            7 => l.a.tx_failed(pick(next(), &nodes), pick(next(), &ports)),
            8 if round % 500 == 0 => {
                l.a.endpoint_down();
                l.a.endpoint_up();
            }
            _ => {}
        }
        for _ in 0..(next() % 3) {
            let _ = l.a.poll_event();
        }
        // what the router sends is always a packet that a router accepts
        for p in drain_tx(&mut l.a) {
            // A reply to a control packet goes back to the port it came from, as in ns.c; if a hostile peer
            // sent that from a port that is not the control port, the reply is a control packet for a data
            // port, which the receiver refuses. Everything else must be well formed.
            let d = match decode(&p) {
                Ok(d) => d,
                Err(packet::PacketError::NotForNameService) => continue,
                Err(e) => panic!("sent an invalid packet {p:02x?}: {e:?}"),
            };
            assert!(d.payload.len() <= 65535);
            // the other router takes it or drops it with an error, never panics
            let _ = l.b.handle_rx(&p);
        }
        let _ = drain_tx(&mut l.b).iter().map(|p| l.a.handle_rx(p)).count();
        assert!(l.a.servers().len() <= NS_MAX_NODES * NS_MAX_SERVERS);
    }
}
