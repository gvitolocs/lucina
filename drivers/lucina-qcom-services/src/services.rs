// SPDX-License-Identifier: BSD-3-Clause

//! The three services on one router, for the kernel thread: see the [crate documentation](crate).

use alloc::collections::VecDeque;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use lucina_qrtr::packet::PORT_CTRL;
use lucina_qrtr::router::{BindError, Event, Router, SendError};

use crate::rmtfs::{self, Rmtfs};
use crate::servreg::{self, PdMapper};
use crate::tqftp::Tftp;
use crate::{
    Files, Log, PUBLISH_PD_MAPPER, PUBLISH_RMTFS, PUBLISH_TQFTP, Partitions, Publish, Reply,
    RmtfsMem,
};

/// Answers of pd-mapper and rmtfs that may wait for a closed flow. A modem that asks without reading its answers
/// loses the newest.
pub const OUTBOX_LIMIT: usize = 64;

/// The published ports. Transfers have more: [`Tftp::transfer_ports`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Ports {
    /// `None` when no `.jsn` gave a map: like the C daemon that exits, pd-mapper is then not there at all.
    pub pd_mapper: Option<u32>,
    pub tftp: u32,
    pub rmtfs: u32,
}

/// Why [`Services::start`] could not register a service. The router is left with what was done so far.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StartError {
    Bind(BindError),
    Publish(SendError),
}

#[derive(Debug)]
struct Out {
    from: u32,
    to: (u32, u32),
    data: Vec<u8>,
}

/// pd-mapper, tqftpserv and rmtfs, as one thing to call from the thread that owns the router.
#[derive(Debug)]
pub struct Services<F, P, M> {
    pub files: F,
    pub pd_mapper: PdMapper,
    pub tftp: Tftp,
    pub rmtfs: Rmtfs<P, M>,
    ports: Option<Ports>,
    outbox: VecDeque<Out>,
    log: Log,
}

impl<F: Files, P: Partitions, M: RmtfsMem> Services<F, P, M> {
    /// Reads the `.jsn` maps through `files` at once ([`servreg::JSN_FILES`]); nothing touches a router yet.
    pub fn new(mut files: F, partitions: P, mem: M) -> Self {
        let mut log = Log::new();
        let pd_mapper = PdMapper::load(&mut files, &servreg::JSN_FILES, &mut log);
        Services {
            files,
            pd_mapper,
            tftp: Tftp::new(),
            rmtfs: Rmtfs::new(partitions, mem),
            ports: None,
            outbox: VecDeque::new(),
            log,
        }
    }

    /// Bind and publish, before the modem starts. Publishing lasts over an endpoint going down and coming up: the
    /// router announces them again on the modem's next HELLO.
    pub fn start(&mut self, router: &mut Router) -> Result<Ports, StartError> {
        if let Some(p) = self.ports {
            return Ok(p);
        }
        let publish = |router: &mut Router, port: u32, p: Publish| {
            router
                .publish(port, p.service, p.instance_word())
                .map_err(StartError::Publish)
        };
        let pd_mapper = if self.pd_mapper.maps().is_empty() {
            self.log.push("pd-mapper: no pd maps available");
            None
        } else {
            let port = router.bind(0).map_err(StartError::Bind)?;
            publish(router, port, PUBLISH_PD_MAPPER)?;
            Some(port)
        };
        let tftp = router.bind(0).map_err(StartError::Bind)?;
        publish(router, tftp, PUBLISH_TQFTP)?;
        let rmtfs_port = router.bind(rmtfs::PORT).map_err(StartError::Bind)?;
        publish(router, rmtfs_port, PUBLISH_RMTFS)?;
        let ports = Ports {
            pd_mapper,
            tftp,
            rmtfs: rmtfs_port,
        };
        self.ports = Some(ports);
        Ok(ports)
    }

    pub fn ports(&self) -> Option<Ports> {
        self.ports
    }

    /// One line of what the C daemons would print, oldest first.
    pub fn pop_log(&mut self) -> Option<String> {
        self.log.pop()
    }

    /// Give every event from [`Router::poll_event`] here. `None`: it was for these services. `Some`: it was not,
    /// and it comes back for the thread's other users of the router. Run this until the router has no more events,
    /// then drain [`Router::poll_tx`]. There is nothing else to call: sending goes on from here when a flow reopens.
    pub fn on_event(&mut self, router: &mut Router, event: Event) -> Option<Event> {
        let rest = self.dispatch(router, event);
        self.pump(router);
        rest
    }

    fn is_ours(&self, port: u32) -> bool {
        self.ports
            .is_some_and(|p| p.pd_mapper == Some(port) || p.tftp == port || p.rmtfs == port)
            || self.tftp.owns_port(port)
    }

    fn dispatch(&mut self, router: &mut Router, event: Event) -> Option<Event> {
        match event {
            Event::Data {
                port,
                src_node,
                src_port,
                data,
            } => {
                let Some(ports) = self.ports.filter(|_| self.is_ours(port)) else {
                    return Some(Event::Data {
                        port,
                        src_node,
                        src_port,
                        data,
                    });
                };
                if src_port == PORT_CTRL {
                    return None;
                }
                let from = (src_node, src_port);
                if ports.pd_mapper == Some(port) {
                    let replies =
                        self.pd_mapper
                            .on_message(src_node, src_port, &data, &mut self.log);
                    self.queue(port, replies);
                } else if ports.rmtfs == port {
                    let replies = self
                        .rmtfs
                        .on_message(src_node, src_port, &data, &mut self.log);
                    self.queue(port, replies);
                } else if ports.tftp == port {
                    self.tftp.on_service_message(
                        router,
                        &mut self.files,
                        &mut self.log,
                        from,
                        &data,
                    );
                } else {
                    self.tftp.on_transfer_message(
                        router,
                        &mut self.files,
                        &mut self.log,
                        port,
                        from,
                        &data,
                    );
                }
                None
            }
            // rmtfs and pd-mapper keep nothing per client: the C only logs these. Each published port hears them.
            Event::DelClient {
                port,
                node,
                client_port,
            } if self.is_ours(port) => {
                self.tftp.on_client_gone(router, node, client_port);
                self.outbox.retain(|o| o.to != (node, client_port));
                None
            }
            Event::Bye { port, node } if self.is_ours(port) => {
                self.tftp.on_bye(router, node);
                self.outbox.retain(|o| o.to.0 != node);
                None
            }
            Event::TxResumed { node, port }
                if self.tftp.has_peer(node, port)
                    || self.outbox.iter().any(|o| o.to == (node, port)) =>
            {
                None
            }
            other => Some(other),
        }
    }

    fn queue(&mut self, from: u32, replies: Vec<Reply>) {
        for r in replies {
            if self.outbox.len() >= OUTBOX_LIMIT {
                self.log
                    .push("qcom-services: too many answers waiting, dropped one");
                continue;
            }
            self.outbox.push_back(Out {
                from,
                to: (r.dst_node, r.dst_port),
                data: r.payload,
            });
        }
    }

    fn pump(&mut self, router: &mut Router) {
        let mut blocked: Vec<(u32, u32)> = Vec::new();
        let mut i = 0;
        while let Some(o) = self.outbox.get(i) {
            if blocked.contains(&o.to) {
                i += 1;
                continue;
            }
            match router.send(o.from, o.to.0, o.to.1, &o.data) {
                Ok(()) => {
                    self.outbox.remove(i);
                }
                Err(SendError::WouldBlock) => {
                    blocked.push(o.to);
                    i += 1;
                }
                Err(e) => {
                    self.log.push(&format!(
                        "qcom-services: answer to {}:{} failed: {e:?}",
                        o.to.0, o.to.1
                    ));
                    self.outbox.remove(i);
                }
            }
        }
        self.tftp.pump(router, &mut self.files, &mut self.log);
    }
}
