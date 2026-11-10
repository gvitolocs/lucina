// SPDX-License-Identifier: GPL-2.0
//
// Derived from Linux net/qrtr/af_qrtr.c (GPL-2.0-only, Copyright (c) 2015 Sony Mobile Communications Inc.,
// Copyright (c) 2013 The Linux Foundation), net/qrtr/ns.c (GPL-2.0 OR BSD-3-Clause, Copyright (c) 2015 Sony
// Mobile Communications Inc., Copyright (c) 2013 The Linux Foundation, Copyright (c) 2020 Linaro Ltd.) and
// net/qrtr/smd.c (GPL-2.0-only): sockets and the kernel's networking are replaced by plain queues, the logic
// is the C's.

//! A small QRTR router with its name service: one remote endpoint (the modem over the GLINK channel `IPCRTR`),
//! local ports, and the name service that Linux runs as a kernel socket on port [`PORT_CTRL`].
//!
//! The owner calls in with what happened and takes out what to do:
//!
//! * [`Router::endpoint_up`] / [`Router::endpoint_down`]: the channel opened or closed
//!   (`qrtr_endpoint_register()` / `qrtr_endpoint_unregister()` from `smd.c`).
//! * [`Router::handle_rx`]: one received message, which is one packet (`qrtr_endpoint_post()`); anything wrong
//!   with it is an error and the message is dropped, as `qcom_smd_qrtr_callback()` does.
//! * [`Router::poll_tx`]: the next message to put on the channel.
//! * [`Router::poll_event`]: the next thing a local port would see: data, a service appearing or going away.
//! * [`Router::bind`], [`Router::close`], [`Router::send`], and for the name service
//!   [`Router::publish`], [`Router::unpublish`], [`Router::lookup`], [`Router::del_lookup`]: what the sockets
//!   of local programs do.
//!
//! What it models, and from where:
//!
//! * `qrtr_endpoint_post()`: the header checks ([`crate::packet::decode`]), node id assignment from the source
//!   of every packet and from the server record of a NEW_SERVER, delivery of data to a bound port, delivery of
//!   everything addressed to [`PORT_CTRL`] to the name service, RESUME_TX to the flow control.
//! * `qrtr_tx_wait()`, `qrtr_tx_resume()`, `qrtr_tx_flow_failed()`: a counter per destination. Data to a
//!   destination counts up; the [`TX_FLOW_LOW`]th packet in a row carries `confirm_rx`; at [`TX_FLOW_HIGH`]
//!   unanswered packets sending stops ([`SendError::WouldBlock`], where Linux sleeps) until the remote's
//!   RESUME_TX arrives ([`Event::TxResumed`]). The receiver sends RESUME_TX when its port is read
//!   ([`Router::poll_event`] returning the data), like `qrtr_recvmsg()`.
//! * `ns.c`: servers per node, lookups from local ports, HELLO, BYE, NEW_SERVER, DEL_SERVER, DEL_CLIENT,
//!   NEW_LOOKUP, DEL_LOOKUP, with the same limits ([`NS_MAX_NODES`] and friends).
//! * `qrtr_port_remove()`: closing a port broadcasts DEL_CLIENT and the name service drops the port's servers
//!   and lookups.
//!
//! Differences from Linux, on purpose:
//!
//! * Linux sends no HELLO when an endpoint registers (the modem speaks first, and the name service's own
//!   broadcast happened at boot); here [`Router::endpoint_up`] does, as the task of bring-up wants, and the
//!   name service answers only the first HELLO it receives per endpoint session. Linux answers every one,
//!   which makes two Linux-style name services answer each other without end. It still announces its servers
//!   on every HELLO.
//! * Control packets for local ports are not built as bytes and parsed again; they arrive as typed [`Event`]s.
//! * A control payload shorter than 20 bytes is read with zeros after its end (Linux reads stale bytes of its
//!   receive buffer).
//! * Receive queues are bounded ([`RX_QUEUE_LIMIT`] events per port) and so is the set of node ids an
//!   endpoint can claim, so a peer cannot grow memory without limit.
//! * Packets with type 0x100 and up in a v1 header are cut to eight bits, because `struct qrtr_cb` keeps the
//!   type in a `u8`.

use crate::packet::{
    self, CtrlPkt, EP_NID_AUTO, MAX_EPH_PORT, MAX_PAYLOAD, MIN_EPH_PORT, NODE_BCAST, PORT_CTRL,
    PacketError, Server, TX_FLOW_HIGH, TX_FLOW_LOW, pkt_type,
};
use alloc::collections::{BTreeMap, BTreeSet, VecDeque};
use alloc::vec::Vec;

/// `QRTR_NS_MAX_NODES`, `QRTR_NS_MAX_SERVERS` (per node) and `QRTR_NS_MAX_LOOKUPS`.
pub const NS_MAX_NODES: usize = 512;
pub const NS_MAX_SERVERS: usize = 256;
pub const NS_MAX_LOOKUPS: usize = 128;
/// Events a port can have waiting (a stand-in for the socket's receive buffer; Linux counts bytes).
pub const RX_QUEUE_LIMIT: usize = 256;
/// Node ids one endpoint may claim (Linux's radix tree has no limit).
pub const MAX_ENDPOINT_NODES: usize = 1024;

/// Something a local port receives.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    /// A data packet for `port` (from the remote, or from another local port).
    Data {
        port: u32,
        src_node: u32,
        src_port: u32,
        data: Vec<u8>,
    },
    /// An answer to a lookup of `port`, or a new service that matches one: NEW_SERVER.
    ServiceNew { port: u32, server: Server },
    /// A service that matches a lookup of `port` is gone: DEL_SERVER.
    ServiceDel { port: u32, server: Server },
    /// The empty NEW_SERVER that ends the list of answers to a lookup.
    LookupDone { port: u32 },
    /// Every client of `node` is gone (BYE, delivered to the ports that serve something).
    Bye { port: u32, node: u32 },
    /// One client is gone (DEL_CLIENT, delivered to the ports that serve something).
    DelClient {
        port: u32,
        node: u32,
        client_port: u32,
    },
    /// The remote sent RESUME_TX: sending to `(node, port)` may continue.
    TxResumed { node: u32, port: u32 },
}

/// Why [`Router::handle_rx`] dropped a message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RxError {
    /// No endpoint is up.
    EndpointDown,
    /// The message failed the header checks.
    Packet(PacketError),
    /// The destination port is not bound.
    NoPort,
    /// The destination port's queue is full.
    QueueFull,
}

/// Why [`Router::send`] and the name-service calls refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SendError {
    /// The sending port is not bound.
    NotBound,
    /// Payload over 65535 bytes (`-EMSGSIZE`).
    TooLong,
    /// A broadcast to a port other than the name service's (`-ENOTCONN`).
    NotConnected,
    /// The destination node is unknown (`-ECONNRESET`).
    NoRoute,
    /// The local destination port is not bound, or is the sender (`-ENODEV`).
    NoPort,
    /// The destination's queue is full (`-ENOSPC`).
    QueueFull,
    /// The flow to this destination has [`TX_FLOW_HIGH`] packets unanswered; retry after
    /// [`Event::TxResumed`]. Linux sleeps here.
    WouldBlock,
}

/// Why [`Router::bind`] refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BindError {
    /// The port is taken, or is the name service's.
    InUse,
    /// An automatic port was asked for and none is free.
    NoFreePort,
}

#[derive(Debug)]
struct Flow {
    pending: u32,
    tx_failed: bool,
}

#[derive(Debug)]
struct Endpoint {
    /// `node->nid`: [`EP_NID_AUTO`] until a packet tells.
    nid: u32,
    /// Every node id that routes here (the keys of `qrtr_nodes` pointing at this endpoint).
    nodes: BTreeSet<u32>,
    flows: BTreeMap<(u32, u32), Flow>,
    hello_received: bool,
}

#[derive(Debug)]
struct Lookup {
    service: u32,
    instance: u32,
    node: u32,
    port: u32,
}

#[derive(Debug, Default)]
struct NsNode {
    servers: BTreeMap<u32, Server>,
}

#[derive(Debug)]
struct Queued {
    event: Event,
    /// For data that asked for confirmation: what `qrtr_send_resume_tx()` needs once it is read.
    resume: Option<Resume>,
}

#[derive(Debug)]
struct Resume {
    remote: (u32, u32),
    local: (u32, u32),
}

/// See the [module documentation](self).
#[derive(Debug)]
pub struct Router {
    local_node: u32,
    ep: Option<Endpoint>,
    /// Bound ports and how many events each has queued.
    ports: BTreeMap<u32, usize>,
    queue: VecDeque<Queued>,
    /// Flows the remote reopened, not yet reported.
    resumed: BTreeSet<(u32, u32)>,
    tx: VecDeque<Vec<u8>>,
    ns_nodes: BTreeMap<u32, NsNode>,
    ns_lookups: Vec<Lookup>,
}

/// `server_match()` with a filter whose `ifilter` is zero, which is the only way `ns.c` uses it.
fn server_match(srv: &Server, service: u32, instance: u32) -> bool {
    if service != 0 && srv.service != service {
        return false;
    }
    let ifilter = if instance != 0 { u32::MAX } else { 0 };
    (srv.instance & ifilter) == instance
}

impl Router {
    /// A router for the node `local_node` (Linux's `qrtr_local_nid`, 1; the modem is 0).
    pub fn new(local_node: u32) -> Router {
        Router {
            local_node,
            ep: None,
            ports: BTreeMap::new(),
            queue: VecDeque::new(),
            resumed: BTreeSet::new(),
            tx: VecDeque::new(),
            ns_nodes: BTreeMap::new(),
            ns_lookups: Vec::new(),
        }
    }

    pub fn local_node(&self) -> u32 {
        self.local_node
    }

    pub fn is_up(&self) -> bool {
        self.ep.is_some()
    }

    /// The remote's node id once a packet has told it.
    pub fn remote_node(&self) -> Option<u32> {
        self.ep
            .as_ref()
            .map(|e| e.nid)
            .filter(|&n| n != EP_NID_AUTO)
    }

    /// Every server the name service knows, by node and port.
    pub fn servers(&self) -> Vec<Server> {
        self.ns_nodes
            .values()
            .flat_map(|n| n.servers.values().copied())
            .collect()
    }

    // ---- the endpoint ----

    /// The channel is open. Registers the endpoint with an unknown node id and says HELLO to it
    /// (`say_hello(&qrtr_ns.bcast_sq)`).
    pub fn endpoint_up(&mut self) {
        if self.ep.is_some() {
            self.endpoint_down();
        }
        self.ep = Some(Endpoint {
            nid: EP_NID_AUTO,
            nodes: BTreeSet::new(),
            flows: BTreeMap::new(),
            hello_received: false,
        });
        self.ns_send_bcast(CtrlPkt::new(pkt_type::HELLO));
    }

    /// The channel closed (`qrtr_endpoint_unregister()`): the name service hears BYE for every node that
    /// routed here, so the servers behind them go away and their lookups hear it.
    pub fn endpoint_down(&mut self) {
        let Some(ep) = self.ep.take() else { return };
        for node in ep.nodes {
            let bye = CtrlPkt::new(pkt_type::BYE);
            self.ns_input((node, PORT_CTRL), &bye.to_bytes());
        }
        self.tx.clear();
    }

    /// A message was lost on its way out. The next data packet to `(node, port)` then carries `confirm_rx`
    /// so the flow cannot stall waiting for a RESUME_TX that will never come (`qrtr_tx_flow_failed()`).
    pub fn tx_failed(&mut self, node: u32, port: u32) {
        if let Some(flow) = self
            .ep
            .as_mut()
            .and_then(|ep| ep.flows.get_mut(&(node, port)))
        {
            flow.tx_failed = true;
        }
    }

    /// The next message for the channel.
    pub fn poll_tx(&mut self) -> Option<Vec<u8>> {
        self.tx.pop_front()
    }

    /// The next thing a port would receive. Reading data that asked for confirmation queues the RESUME_TX
    /// (`qrtr_recvmsg()` -> `qrtr_send_resume_tx()`).
    pub fn poll_event(&mut self) -> Option<Event> {
        if let Some(&(node, port)) = self.resumed.iter().next() {
            self.resumed.remove(&(node, port));
            return Some(Event::TxResumed { node, port });
        }
        let q = self.queue.pop_front()?;
        let port = match &q.event {
            Event::Data { port, .. }
            | Event::ServiceNew { port, .. }
            | Event::ServiceDel { port, .. }
            | Event::LookupDone { port }
            | Event::Bye { port, .. }
            | Event::DelClient { port, .. } => Some(*port),
            Event::TxResumed { .. } => None,
        };
        if let Some(c) = port.and_then(|p| self.ports.get_mut(&p)) {
            *c = c.saturating_sub(1);
        }
        if let Some(r) = q.resume {
            // Linux looks the remote node up; it was assigned when the packet came in.
            let known = self
                .ep
                .as_ref()
                .is_some_and(|ep| ep.nodes.contains(&r.remote.0));
            if known {
                let pkt = CtrlPkt::with_client(pkt_type::RESUME_TX, r.local.0, r.local.1);
                let _ = self.node_enqueue(pkt_type::RESUME_TX, r.local, r.remote, &pkt.to_bytes());
            }
        }
        Some(q.event)
    }

    /// One message from the channel (`qrtr_endpoint_post()`).
    pub fn handle_rx(&mut self, data: &[u8]) -> Result<(), RxError> {
        if self.ep.is_none() {
            return Err(RxError::EndpointDown);
        }
        let p = packet::decode(data).map_err(RxError::Packet)?;

        let ep = self.ep.as_mut().ok_or(RxError::EndpointDown)?;
        ep.assign(p.src_node);
        if p.ptype == pkt_type::NEW_SERVER {
            // "Remote node endpoint can bridge other distant nodes". decode() guarantees 20 bytes.
            if let Some(pkt) = CtrlPkt::parse(p.payload) {
                ep.assign(pkt.server().node);
            }
        }

        if p.ptype == pkt_type::RESUME_TX {
            let pkt =
                CtrlPkt::parse(p.payload).ok_or(RxError::Packet(PacketError::ShortControl))?;
            let (node, port) = pkt.client();
            // qrtr_tx_resume(): only a flow that exists is reset and woken.
            if let Some(flow) = ep.flows.get_mut(&(node, port)) {
                flow.pending = 0;
                self.resumed.insert((node, port));
            }
            return Ok(());
        }

        // qrtr_port_lookup(): the control port is stored as 0
        if p.dst_port == PORT_CTRL || p.dst_port == 0 {
            self.ns_input((p.src_node, p.src_port), p.payload);
            return Ok(());
        }
        let Some(count) = self.ports.get_mut(&p.dst_port) else {
            return Err(RxError::NoPort);
        };
        if *count >= RX_QUEUE_LIMIT {
            return Err(RxError::QueueFull);
        }
        *count += 1;
        self.queue.push_back(Queued {
            event: Event::Data {
                port: p.dst_port,
                src_node: p.src_node,
                src_port: p.src_port,
                data: p.payload.to_vec(),
            },
            resume: p.confirm_rx.then_some(Resume {
                remote: (p.src_node, p.src_port),
                local: (p.dst_node, p.dst_port),
            }),
        });
        Ok(())
    }

    // ---- ports ----

    /// `qrtr_port_assign()`: port 0 asks for the lowest free automatic port; any other number is taken as
    /// it is. [`PORT_CTRL`] belongs to the name service.
    pub fn bind(&mut self, port: u32) -> Result<u32, BindError> {
        if port == 0 {
            // The first gap in the run of bound ports that starts at the bottom of the range.
            let mut port = MIN_EPH_PORT;
            for &p in self
                .ports
                .range(MIN_EPH_PORT..=MAX_EPH_PORT)
                .map(|(p, _)| p)
            {
                if p != port {
                    break;
                }
                port += 1;
            }
            if port > MAX_EPH_PORT {
                return Err(BindError::NoFreePort);
            }
            self.ports.insert(port, 0);
            return Ok(port);
        }
        if port == PORT_CTRL || self.ports.contains_key(&port) {
            return Err(BindError::InUse);
        }
        self.ports.insert(port, 0);
        Ok(port)
    }

    /// `qrtr_port_remove()` for a closing socket: DEL_CLIENT goes to the remote and to the name service,
    /// then the port and what was queued for it are gone. False when the port was not bound.
    pub fn close(&mut self, port: u32) -> bool {
        if !self.ports.contains_key(&port) {
            return false;
        }
        let pkt = CtrlPkt::with_client(pkt_type::DEL_CLIENT, self.local_node, port).to_bytes();
        let from = (self.local_node, port);
        if self.ep.is_some() {
            let _ = self.node_enqueue(pkt_type::DEL_CLIENT, from, (NODE_BCAST, PORT_CTRL), &pkt);
        }
        self.ns_input(from, &pkt);
        self.ports.remove(&port);
        self.queue.retain(|q| match &q.event {
            Event::Data { port: p, .. }
            | Event::ServiceNew { port: p, .. }
            | Event::ServiceDel { port: p, .. }
            | Event::LookupDone { port: p }
            | Event::Bye { port: p, .. }
            | Event::DelClient { port: p, .. } => *p != port,
            Event::TxResumed { .. } => true,
        });
        true
    }

    /// `qrtr_sendmsg()`: data from the bound port `from_port` to `(to_node, to_port)`. To the local node
    /// it lands in the destination's queue; to [`NODE_BCAST`] (only for [`PORT_CTRL`]) it goes out the
    /// endpoint and to the local name service; to another node it goes out the endpoint with flow control.
    pub fn send(
        &mut self,
        from_port: u32,
        to_node: u32,
        to_port: u32,
        data: &[u8],
    ) -> Result<(), SendError> {
        if !self.ports.contains_key(&from_port) {
            return Err(SendError::NotBound);
        }
        if data.len() > MAX_PAYLOAD {
            return Err(SendError::TooLong);
        }
        let from = (self.local_node, from_port);
        let to = (to_node, to_port);
        if to_node == NODE_BCAST {
            if to_port != PORT_CTRL {
                return Err(SendError::NotConnected);
            }
            // qrtr_bcast_enqueue(): every endpoint, then this node's own name service
            if self.ep.is_some() {
                let _ = self.node_enqueue(pkt_type::DATA, from, to, data);
            }
            return self.local_enqueue(from, to, data);
        }
        if to_node == self.local_node {
            return self.local_enqueue(from, to, data);
        }
        if !self
            .ep
            .as_ref()
            .is_some_and(|ep| ep.nodes.contains(&to_node))
        {
            return Err(SendError::NoRoute);
        }
        self.node_enqueue(pkt_type::DATA, from, to, data)
    }

    /// Register a service of this node with the name service (what a QMI server's `qmi_add_server()` sends).
    /// Linux only logs what the name service thinks of it; here the answer is returned.
    pub fn publish(&mut self, port: u32, service: u32, instance: u32) -> Result<(), SendError> {
        let s = Server {
            service,
            instance,
            node: self.local_node,
            port,
        };
        self.ctrl_to_ns(port, CtrlPkt::with_server(pkt_type::NEW_SERVER, s))
    }

    /// Withdraw a service (DEL_SERVER).
    pub fn unpublish(&mut self, port: u32, service: u32, instance: u32) -> Result<(), SendError> {
        let s = Server {
            service,
            instance,
            node: self.local_node,
            port,
        };
        self.ctrl_to_ns(port, CtrlPkt::with_server(pkt_type::DEL_SERVER, s))
    }

    /// Ask to hear about a service, now and later (NEW_LOOKUP; `qmi_add_lookup()`). A `service` of 0 matches
    /// all; an `instance` of 0 matches all instances, else it must be equal. Answers are
    /// [`Event::ServiceNew`] and then [`Event::LookupDone`]; later arrivals and departures follow.
    pub fn lookup(&mut self, port: u32, service: u32, instance: u32) -> Result<(), SendError> {
        let s = Server {
            service,
            instance,
            node: 0,
            port: 0,
        };
        self.ctrl_to_ns(port, CtrlPkt::with_server(pkt_type::NEW_LOOKUP, s))
    }

    pub fn del_lookup(&mut self, port: u32, service: u32, instance: u32) -> Result<(), SendError> {
        let s = Server {
            service,
            instance,
            node: 0,
            port: 0,
        };
        self.ctrl_to_ns(port, CtrlPkt::with_server(pkt_type::DEL_LOOKUP, s))
    }

    fn ctrl_to_ns(&mut self, port: u32, pkt: CtrlPkt) -> Result<(), SendError> {
        self.send(port, self.local_node, PORT_CTRL, &pkt.to_bytes())
    }

    // ---- transmit side ----

    /// `qrtr_local_enqueue()`.
    fn local_enqueue(
        &mut self,
        from: (u32, u32),
        to: (u32, u32),
        data: &[u8],
    ) -> Result<(), SendError> {
        if to.1 == PORT_CTRL || to.1 == 0 {
            // The name service is a socket too, and "do not send to self".
            if from.1 == PORT_CTRL {
                return Err(SendError::NoPort);
            }
            self.ns_input(from, data);
            return Ok(());
        }
        if to.1 == from.1 {
            return Err(SendError::NoPort);
        }
        let Some(count) = self.ports.get_mut(&to.1) else {
            return Err(SendError::NoPort);
        };
        if *count >= RX_QUEUE_LIMIT {
            return Err(SendError::QueueFull);
        }
        *count += 1;
        self.queue.push_back(Queued {
            event: Event::Data {
                port: to.1,
                src_node: from.0,
                src_port: from.1,
                data: data.to_vec(),
            },
            resume: None,
        });
        Ok(())
    }

    /// `qrtr_tx_wait()`: whether the packet carries `confirm_rx`, or why it cannot go yet.
    fn tx_wait(&mut self, dest: (u32, u32), ptype: u32) -> Result<bool, SendError> {
        // "Never set confirm_rx on non-data packets"
        if ptype != pkt_type::DATA {
            return Ok(false);
        }
        let Some(ep) = self.ep.as_mut() else {
            return Err(SendError::NoRoute);
        };
        let flow = ep.flows.entry(dest).or_insert(Flow {
            pending: 0,
            tx_failed: false,
        });
        if flow.pending >= TX_FLOW_HIGH && !flow.tx_failed {
            return Err(SendError::WouldBlock);
        }
        if flow.tx_failed {
            flow.tx_failed = false;
            return Ok(true);
        }
        flow.pending += 1;
        Ok(flow.pending == TX_FLOW_LOW)
    }

    /// `qrtr_node_enqueue()`: flow control, the v1 header, padding.
    fn node_enqueue(
        &mut self,
        ptype: u32,
        from: (u32, u32),
        to: (u32, u32),
        payload: &[u8],
    ) -> Result<(), SendError> {
        let confirm_rx = self.tx_wait(to, ptype)?;
        let Some(ep) = self.ep.as_ref() else {
            return Err(SendError::NoRoute);
        };
        // A packet for the name service of a node is addressed to the node as the endpoint knows it.
        let dst = if to.1 == PORT_CTRL {
            (ep.nid, PORT_CTRL)
        } else {
            to
        };
        self.tx
            .push_back(packet::encode_v1(ptype, from, dst, confirm_rx, payload));
        Ok(())
    }

    // ---- the name service (ns.c) ----

    /// A control packet from `from` reaches the name service (`qrtr_ns_worker()`).
    fn ns_input(&mut self, from: (u32, u32), payload: &[u8]) {
        let Some(pkt) = CtrlPkt::parse(payload) else {
            return;
        };
        let s = pkt.server();
        let (cnode, cport) = pkt.client();
        match pkt.cmd {
            pkt_type::HELLO => self.ns_hello(from),
            pkt_type::BYE => self.ns_bye(from),
            pkt_type::DEL_CLIENT => self.ns_del_client(from, cnode, cport),
            pkt_type::NEW_SERVER => self.ns_new_server(from, s),
            pkt_type::DEL_SERVER => self.ns_del_server(from, s),
            pkt_type::NEW_LOOKUP => self.ns_new_lookup(from, s.service, s.instance),
            pkt_type::DEL_LOOKUP => self.ns_del_lookup(from, s.service, s.instance),
            // EXIT, PING and RESUME_TX are read and ignored; anything else is not a command.
            _ => {}
        }
    }

    /// Send a control packet from the name service to `to`: a local port gets an event, the remote a packet,
    /// [`NODE_BCAST`] every endpoint (and not the name service itself).
    fn ns_send(&mut self, to: (u32, u32), pkt: CtrlPkt) {
        let from = (self.local_node, PORT_CTRL);
        if to.0 == NODE_BCAST {
            self.ns_send_bcast(pkt);
        } else if to.0 == self.local_node {
            self.ns_deliver_local(to.1, pkt);
        } else if self.ep.as_ref().is_some_and(|ep| ep.nodes.contains(&to.0)) {
            let _ = self.node_enqueue(pkt.cmd, from, to, &pkt.to_bytes());
        }
    }

    fn ns_send_bcast(&mut self, pkt: CtrlPkt) {
        if self.ep.is_some() {
            let from = (self.local_node, PORT_CTRL);
            let _ = self.node_enqueue(pkt.cmd, from, (NODE_BCAST, PORT_CTRL), &pkt.to_bytes());
        }
    }

    fn ns_deliver_local(&mut self, port: u32, pkt: CtrlPkt) {
        let event = match pkt.cmd {
            pkt_type::NEW_SERVER => {
                let s = pkt.server();
                if s == Server::default() {
                    Event::LookupDone { port }
                } else {
                    Event::ServiceNew { port, server: s }
                }
            }
            pkt_type::DEL_SERVER => Event::ServiceDel {
                port,
                server: pkt.server(),
            },
            pkt_type::BYE => Event::Bye {
                port,
                node: pkt.client().0,
            },
            pkt_type::DEL_CLIENT => Event::DelClient {
                port,
                node: pkt.client().0,
                client_port: pkt.client().1,
            },
            _ => return,
        };
        // qrtr_local_enqueue(): no such port, or its buffer is full, and the packet is dropped.
        let Some(count) = self.ports.get_mut(&port) else {
            return;
        };
        if *count >= RX_QUEUE_LIMIT {
            return;
        }
        *count += 1;
        self.queue.push_back(Queued {
            event,
            resume: None,
        });
    }

    /// `node_get()`: false when the node table is full.
    fn ns_node_get(&mut self, id: u32) -> bool {
        if self.ns_nodes.contains_key(&id) {
            return true;
        }
        if self.ns_nodes.len() >= NS_MAX_NODES {
            return false;
        }
        self.ns_nodes.insert(id, NsNode::default());
        true
    }

    /// `ctrl_cmd_hello()`: say HELLO back (once per endpoint session, see the module documentation) and
    /// announce the servers of this node to the sender.
    fn ns_hello(&mut self, from: (u32, u32)) {
        if from.0 != self.local_node {
            let first = self.ep.as_mut().is_some_and(|ep| {
                let first = !ep.hello_received;
                ep.hello_received = true;
                first
            });
            if first {
                self.ns_send(from, CtrlPkt::new(pkt_type::HELLO));
            }
        }
        // announce_servers()
        if !self.ns_node_get(self.local_node) {
            return;
        }
        let local: Vec<Server> = self.ns_nodes[&self.local_node]
            .servers
            .values()
            .copied()
            .collect();
        for srv in local {
            self.ns_send(from, CtrlPkt::with_server(pkt_type::NEW_SERVER, srv));
        }
    }

    /// `ctrl_cmd_bye()`: every server of the node is dropped, the local servers are told, the node is gone.
    fn ns_bye(&mut self, from: (u32, u32)) {
        if !self.ns_node_get(from.0) {
            return;
        }
        let ports: Vec<u32> = self.ns_nodes[&from.0].servers.keys().copied().collect();
        for port in ports {
            self.ns_server_del(from.0, port, true);
        }
        if self.ns_node_get(self.local_node) {
            let local: Vec<Server> = self.ns_nodes[&self.local_node]
                .servers
                .values()
                .copied()
                .collect();
            let bye = CtrlPkt::with_client(pkt_type::BYE, from.0, 0);
            for srv in local {
                self.ns_send((srv.node, srv.port), bye);
            }
        }
        self.ns_nodes.remove(&from.0);
    }

    /// `ctrl_cmd_del_client()`.
    fn ns_del_client(&mut self, from: (u32, u32), node_id: u32, port: u32) {
        // "Don't accept spoofed messages"
        if from.0 != node_id {
            return;
        }
        // "Local DEL_CLIENT messages comes from the port being closed"
        if from.0 == self.local_node && from.1 != port {
            return;
        }
        self.ns_lookups
            .retain(|l| !(l.node == node_id && l.port == port));
        // The neighbours dropped the server on the DEL_CLIENT broadcast already: no DEL_SERVER.
        if self.ns_node_get(node_id) {
            self.ns_server_del(node_id, port, false);
        }
        if !self.ns_node_get(self.local_node) {
            return;
        }
        let local: Vec<Server> = self.ns_nodes[&self.local_node]
            .servers
            .values()
            .copied()
            .collect();
        let pkt = CtrlPkt::with_client(pkt_type::DEL_CLIENT, node_id, port);
        for srv in local {
            self.ns_send((srv.node, srv.port), pkt);
        }
    }

    /// `server_add()` and the rest of `ctrl_cmd_new_server()`.
    fn ns_new_server(&mut self, from: (u32, u32), s: Server) {
        // "Ignore specified node and port for local servers"
        let (node_id, port) = if from.0 == self.local_node {
            from
        } else {
            (s.node, s.port)
        };
        if s.service == 0 || port == 0 || !self.ns_node_get(node_id) {
            return;
        }
        let Some(node) = self.ns_nodes.get_mut(&node_id) else {
            return;
        };
        if !node.servers.contains_key(&port) && node.servers.len() >= NS_MAX_SERVERS {
            return;
        }
        // "Delete the old server on the same port"
        let srv = Server {
            service: s.service,
            instance: s.instance,
            node: node_id,
            port,
        };
        node.servers.insert(port, srv);

        if srv.node == self.local_node {
            self.ns_send_bcast(CtrlPkt::with_server(pkt_type::NEW_SERVER, srv));
        }
        for (node, port) in self.matching_lookups(&srv) {
            self.ns_send(
                (node, port),
                CtrlPkt::with_server(pkt_type::NEW_SERVER, srv),
            );
        }
    }

    /// `ctrl_cmd_del_server()`.
    fn ns_del_server(&mut self, from: (u32, u32), s: Server) {
        let (node_id, port) = if from.0 == self.local_node {
            from
        } else {
            (s.node, s.port)
        };
        if self.ns_node_get(node_id) {
            self.ns_server_del(node_id, port, true);
        }
    }

    /// `server_del()`.
    fn ns_server_del(&mut self, node_id: u32, port: u32, bcast: bool) {
        let Some(srv) = self
            .ns_nodes
            .get_mut(&node_id)
            .and_then(|n| n.servers.remove(&port))
        else {
            return;
        };
        let del = CtrlPkt::with_server(pkt_type::DEL_SERVER, srv);
        // "Broadcast the removal of local servers"
        if srv.node == self.local_node && bcast {
            self.ns_send_bcast(del);
        }
        // "Announce the service's disappearance to observers"
        for (node, port) in self.matching_lookups(&srv) {
            self.ns_send((node, port), del);
        }
    }

    fn matching_lookups(&self, srv: &Server) -> Vec<(u32, u32)> {
        self.ns_lookups
            .iter()
            .filter(|l| {
                !(l.service != 0 && l.service != srv.service)
                    && !(l.instance != 0 && l.instance != srv.instance)
            })
            .map(|l| (l.node, l.port))
            .collect()
    }

    /// `ctrl_cmd_new_lookup()`.
    fn ns_new_lookup(&mut self, from: (u32, u32), service: u32, instance: u32) {
        // "Accept only local observers"
        if from.0 != self.local_node || self.ns_lookups.len() >= NS_MAX_LOOKUPS {
            return;
        }
        self.ns_lookups.push(Lookup {
            service,
            instance,
            node: from.0,
            port: from.1,
        });
        let found: Vec<Server> = self
            .ns_nodes
            .values()
            .flat_map(|n| n.servers.values())
            .filter(|s| server_match(s, service, instance))
            .copied()
            .collect();
        for srv in found {
            self.ns_send(from, CtrlPkt::with_server(pkt_type::NEW_SERVER, srv));
        }
        // "Empty notification, to indicate end of listing"
        self.ns_send(
            from,
            CtrlPkt::with_server(pkt_type::NEW_SERVER, Server::default()),
        );
    }

    /// `ctrl_cmd_del_lookup()`.
    fn ns_del_lookup(&mut self, from: (u32, u32), service: u32, instance: u32) {
        self.ns_lookups.retain(|l| {
            l.node != from.0
                || l.port != from.1
                || l.service != service
                || (l.instance != 0 && l.instance != instance)
        });
    }
}

impl Endpoint {
    /// `qrtr_node_assign()`.
    fn assign(&mut self, nid: u32) {
        if nid == EP_NID_AUTO {
            return;
        }
        if self.nodes.len() < MAX_ENDPOINT_NODES || self.nodes.contains(&nid) {
            self.nodes.insert(nid);
        }
        if self.nid == EP_NID_AUTO {
            self.nid = nid;
        }
    }
}
