//! The services on a real `Router`, joined back to back with a second one that plays the modem (node 0). This modem
//! is a model written from reading the C and the design notes: it proves the host side keeps to the protocol as the
//! tests understand it, not that a real modem agrees.

use std::collections::HashMap;

use lucina_qcom_services::rmtfs::{self, IovecEntry, Request};
use lucina_qcom_services::servreg;
use lucina_qcom_services::tftp::{self, RRQ, WRQ};
use lucina_qcom_services::tqftp::{self, MAX_TRANSFERS, RW_MAX_BYTES};
use lucina_qcom_services::{
    Files, IoError, PUBLISH_PD_MAPPER, PUBLISH_RMTFS, PUBLISH_TQFTP, Partitions, RmtfsMem,
    Services, qmi,
};
use lucina_qrtr::packet::TX_FLOW_HIGH;
use lucina_qrtr::router::{Event, Router};

const MODEMUW: &str = r#"{
    "sr_version": { "major": 1, "minor": 1, "patch": 1 },
    "sr_domain": { "soc": "msm", "domain": "modem", "subdomain": "wlan_pd", "qmi_instance_id": 180 },
    "sr_service": [
        { "provider": "kernel", "service": "elf_loader", "service_data_valid": 0, "service_data": 0 },
        { "provider": "tms", "service": "servreg", "service_data_valid": 0, "service_data": 0 },
        { "provider": "wlan", "service": "fw", "service_data_valid": 0, "service_data": 0 }
    ]
}"#;
const MODEMR: &str = r#"{ "sr_domain": { "soc": "msm", "domain": "modem", "subdomain": "root_pd",
    "qmi_instance_id": 180 }, "sr_service": [ { "provider": "tms", "service": "servreg" } ] }"#;

#[derive(Default)]
struct MemFiles {
    files: HashMap<String, Vec<u8>>,
    reads: usize,
    largest_read: usize,
    fail: bool,
}

impl Files for MemFiles {
    fn file_len(&mut self, name: &str) -> Option<u64> {
        self.files.get(name).map(|f| f.len() as u64)
    }
    fn read_at(&mut self, name: &str, offset: u64, buf: &mut [u8]) -> Result<usize, IoError> {
        if self.fail {
            return Err(IoError);
        }
        self.reads += 1;
        self.largest_read = self.largest_read.max(buf.len());
        let f = self.files.get(name).ok_or(IoError)?;
        let Some(rest) = f.get(offset as usize..) else {
            return Ok(0);
        };
        let n = buf.len().min(rest.len());
        buf[..n].copy_from_slice(&rest[..n]);
        Ok(n)
    }
}

#[derive(Default)]
struct Disk {
    parts: HashMap<String, Vec<u8>>,
    reads: usize,
    fail: bool,
}

impl Partitions for Disk {
    fn partition_len(&mut self, name: &str) -> Option<u64> {
        self.parts.get(name).map(|p| p.len() as u64)
    }
    fn read_at(&mut self, name: &str, offset: u64, buf: &mut [u8]) -> Result<(), IoError> {
        if self.fail {
            return Err(IoError);
        }
        self.reads += 1;
        let o = offset as usize;
        let p = self
            .parts
            .get(name)
            .expect("rmtfs read an unopened partition");
        // rmtfs promised never to read past the end
        buf.copy_from_slice(&p[o..o + buf.len()]);
        Ok(())
    }
}

struct Window(Vec<u8>);

impl RmtfsMem for Window {
    fn read(&mut self, offset: u64, buf: &mut [u8]) -> Result<(), IoError> {
        let o = offset as usize;
        buf.copy_from_slice(&self.0[o..o + buf.len()]);
        Ok(())
    }
    fn write(&mut self, offset: u64, buf: &[u8]) -> Result<(), IoError> {
        let o = offset as usize;
        self.0[o..o + buf.len()].copy_from_slice(buf);
        Ok(())
    }
}

fn file(n: usize) -> Vec<u8> {
    (0..n).map(|i| (i * 7 + i / 251) as u8).collect()
}

fn efs() -> Vec<u8> {
    (0..8192u32).map(|i| (i % 253) as u8).collect()
}

const WLANMDSP: &str = "/readonly/firmware/image/wlanmdsp.mbn";

struct World {
    host: Router,
    modem: Router,
    s: Services<MemFiles, Disk, Window>,
    /// Events the services gave back.
    stray: Vec<Event>,
    /// The model's one client port.
    client: u32,
}

impl World {
    fn new(firmware: Vec<u8>) -> World {
        let mut files = MemFiles::default();
        files.files.insert("wlanmdsp.mbn".into(), firmware);
        files.files.insert("modemuw.jsn".into(), MODEMUW.into());
        files.files.insert("modemr.jsn".into(), MODEMR.into());
        let mut disk = Disk::default();
        disk.parts.insert("modemst1".into(), efs());
        let mem = Window(vec![0; rmtfs::RMTFS_MEM_SIZE as usize]);
        let mut host = Router::new(1);
        let mut modem = Router::new(0);
        let mut s = Services::new(files, disk, mem);
        s.start(&mut host).unwrap();
        host.endpoint_up();
        modem.endpoint_up();
        let client = modem.bind(0).unwrap();
        let mut w = World {
            host,
            modem,
            s,
            stray: Vec::new(),
            client,
        };
        w.shuttle();
        w
    }

    /// Move messages and let the services act until nothing changes. The model's own events stay queued, so a modem
    /// that does not read holds back its RESUME_TX.
    fn shuttle(&mut self) {
        loop {
            let mut moved = false;
            while let Some(ev) = self.host.poll_event() {
                moved = true;
                if let Some(ev) = self.s.on_event(&mut self.host, ev) {
                    self.stray.push(ev);
                }
            }
            while let Some(m) = self.host.poll_tx() {
                moved = true;
                let _ = self.modem.handle_rx(&m);
            }
            while let Some(m) = self.modem.poll_tx() {
                moved = true;
                let _ = self.host.handle_rx(&m);
            }
            if !moved {
                return;
            }
        }
    }

    /// What the model's client port received: (from port, bytes).
    fn received(&mut self) -> Vec<(u32, Vec<u8>)> {
        let mut out = Vec::new();
        while let Some(ev) = self.modem.poll_event() {
            if let Event::Data {
                port,
                src_port,
                data,
                ..
            } = ev
                && port == self.client
            {
                out.push((src_port, data));
            }
        }
        out
    }

    /// A lookup, as the modem's clients do: the port of `service` on the host.
    fn find(&mut self, service: u32) -> u32 {
        let probe = self.modem.bind(0).unwrap();
        self.modem.lookup(probe, service, 0).unwrap();
        self.shuttle();
        let mut found = None;
        while let Some(ev) = self.modem.poll_event() {
            if let Event::ServiceNew { server, .. } = ev {
                assert_eq!(server.node, 1);
                found = Some(server.port);
            }
        }
        self.modem.close(probe);
        self.shuttle();
        found.unwrap_or_else(|| panic!("service {service} not found"))
    }

    fn send(&mut self, port: u32, data: &[u8]) {
        self.modem.send(self.client, 1, port, data).unwrap();
        self.shuttle();
    }

    /// A request and its one answer.
    fn ask(&mut self, port: u32, data: &[u8]) -> Vec<u8> {
        self.send(port, data);
        let mut r = self.received();
        assert_eq!(r.len(), 1, "expected one answer");
        r.remove(0).1
    }

    fn transfers(&self) -> usize {
        self.s.tftp.transfer_ports().count()
    }
}

fn request(op: u16, name: &str, opts: &[(&str, &str)]) -> Vec<u8> {
    let mut v = op.to_be_bytes().to_vec();
    for s in [name, "octet"] {
        v.extend_from_slice(s.as_bytes());
        v.push(0);
    }
    for (k, val) in opts {
        v.extend_from_slice(k.as_bytes());
        v.push(0);
        v.extend_from_slice(val.as_bytes());
        v.push(0);
    }
    v
}

#[test]
fn services_are_published_with_their_parameters() {
    let mut w = World::new(file(10));
    assert_eq!(PUBLISH_PD_MAPPER.service, 64);
    assert_eq!(PUBLISH_PD_MAPPER.instance_word(), 0x101);
    assert_eq!(PUBLISH_TQFTP.service, 4096);
    assert_eq!(PUBLISH_TQFTP.instance_word(), 1);
    assert_eq!(PUBLISH_RMTFS.service, 14);
    assert_eq!(PUBLISH_RMTFS.instance_word(), 1);
    let ports = w.s.ports().unwrap();
    assert_eq!(w.find(64), ports.pd_mapper.unwrap());
    assert_eq!(w.find(4096), ports.tftp);
    assert_eq!(w.find(14), ports.rmtfs);
    assert_eq!(ports.rmtfs, 14);
    // starting twice changes nothing
    assert_eq!(w.s.start(&mut w.host), Ok(ports));
}

/// The modem's whole fetch of a firmware-sized file: blksize, tsize and a window of 10, as `wlanmdsp.mbn` goes.
#[test]
fn tftp_round_trip_of_a_whole_file() {
    let content = file(3_832_348);
    let mut w = World::new(content.clone());
    let port = w.find(tqftp::SERVICE);

    let opts = [
        ("blksize", "7680"),
        ("timeoutms", "1000"),
        ("tsize", "0"),
        ("wsize", "10"),
    ];
    let oack = w.ask(port, &request(RRQ, WLANMDSP, &opts));
    let want = tftp::Options {
        blksize: 7680,
        tsize: Some(3_832_348),
        wsize: 10,
        ..tftp::Options::default()
    };
    assert_eq!(oack, tftp::oack(&want));
    assert_eq!(w.transfers(), 1);
    let tid = w.s.tftp.transfer_ports().next().unwrap();
    assert_ne!(tid, port, "a transfer runs on its own port");

    let mut got: Vec<u8> = Vec::new();
    let mut last = 0u16;
    w.send(tid, &tftp::ack(0));
    loop {
        let pkts = w.received();
        assert!(!pkts.is_empty(), "stalled after block {last}");
        assert!(pkts.len() <= 10);
        let mut end = false;
        for (from, p) in pkts {
            assert_eq!(from, tid);
            assert_eq!(tftp::opcode(&p), Some(tftp::DATA));
            assert_eq!(tftp::block_of(&p), Some(last + 1));
            last += 1;
            got.extend_from_slice(&p[4..]);
            end = p.len() - 4 < 7680;
        }
        // The model acknowledges whole windows, as the option says. The OACK counted in the flow, so a window can
        // arrive in two parts, with the RESUME_TX for reading the first part in between.
        if end || last.is_multiple_of(10) {
            w.send(tid, &tftp::ack(last));
        } else {
            w.shuttle();
        }
        if end {
            break;
        }
    }
    assert!(got == content, "the file came through changed");
    assert_eq!(w.transfers(), 0, "the last ACK ends the transfer");
    assert!(
        w.s.files.largest_read <= 7680,
        "read {} bytes at once",
        w.s.files.largest_read
    );
}

/// A request without options gets block 1 at once, and the rest one ACK at a time (RFC 1350).
#[test]
fn tftp_without_options() {
    let content = file(1300);
    let mut w = World::new(content.clone());
    let port = w.find(tqftp::SERVICE);
    let first = w.ask(port, &request(RRQ, WLANMDSP, &[]));
    assert_eq!(first, tftp::data(1, &content[..512]));
    let tid = w.s.tftp.transfer_ports().next().unwrap();
    assert_eq!(
        w.ask(tid, &tftp::ack(1)),
        tftp::data(2, &content[512..1024])
    );
    assert_eq!(w.ask(tid, &tftp::ack(2)), tftp::data(3, &content[1024..]));
    w.send(tid, &tftp::ack(3));
    assert_eq!(w.transfers(), 0);
}

/// Qualcomm's stat-like request: rsize and seek, ended by the client's ERROR 9.
#[test]
fn tftp_rsize_seek_and_end_of_transfer() {
    let content = file(10_000);
    let mut w = World::new(content.clone());
    let port = w.find(tqftp::SERVICE);
    let req = request(RRQ, WLANMDSP, &[("rsize", "52"), ("seek", "2000")]);
    w.send(port, &req);
    let oack = w.received();
    let tid = oack[0].0;
    let data = w.ask(tid, &tftp::ack(0));
    assert_eq!(data, tftp::data(1, &content[2000..2052]));
    w.send(tid, &tftp::error(tftp::ERR_END_OF_TRANSFER, ""));
    assert_eq!(w.transfers(), 0);
}

/// A window larger than the flow control allows: the router stops it, nothing past that is read, and the modem
/// reading its queue (which sends RESUME_TX) brings the rest, in order.
#[test]
fn tftp_window_over_the_flow_limit() {
    let content = file(512 * 60);
    let mut w = World::new(content.clone());
    let port = w.find(tqftp::SERVICE);
    w.send(port, &request(RRQ, WLANMDSP, &[("wsize", "40")]));
    let tid = w.received()[0].0;
    let before = w.s.files.reads;
    w.send(tid, &tftp::ack(0));
    // the model has not read: its RESUME_TX has not gone
    let reads = w.s.files.reads - before;
    assert!(
        reads <= TX_FLOW_HIGH as usize + 1,
        "{reads} block reads with the flow closed"
    );
    let mut blocks = Vec::new();
    for _ in 0..20 {
        for (_, p) in w.received() {
            blocks.push(p);
        }
        w.shuttle();
    }
    assert_eq!(blocks.len(), 40);
    for (i, p) in blocks.iter().enumerate() {
        assert_eq!(
            p,
            &tftp::data(i as u16 + 1, &content[i * 512..(i + 1) * 512])
        );
    }
}

/// wsize and blksize at their maximum from a hostile client: held back by the flow control, one block at a time.
#[test]
fn hostile_window_stays_bounded() {
    let mut w = World::new(file(64 * 1024 * 1024));
    let port = w.find(tqftp::SERVICE);
    let opts = [("blksize", "65464"), ("wsize", "65535")];
    w.send(port, &request(RRQ, WLANMDSP, &opts));
    let tid = w.received()[0].0;
    let before = w.s.files.reads;
    w.send(tid, &tftp::ack(0));
    assert!(w.s.files.reads - before <= TX_FLOW_HIGH as usize + 1);
    assert!(w.s.files.largest_read <= 65464);
}

#[test]
fn tftp_refusals() {
    let mut w = World::new(file(100));
    let port = w.find(tqftp::SERVICE);
    let enoent = tftp::error(tftp::ERR_ENOENT, "file not found");
    let eacces = tftp::error(tftp::ERR_EACCESS, "Access violation");
    for (req, want) in [
        (
            request(RRQ, "/readonly/firmware/image/nope.mbn", &[]),
            &enoent,
        ),
        (request(RRQ, "/etc/passwd", &[]), &enoent),
        (request(RRQ, "/readwrite/missing", &[]), &enoent),
        (request(RRQ, "/readwrite/../etc/passwd", &[]), &eacces),
        (request(WRQ, WLANMDSP, &[]), &eacces),
        (request(WRQ, "/etc/passwd", &[]), &eacces),
        (
            request(WRQ, "/readwrite/ota_firewall/ruleset", &[]),
            &eacces,
        ),
    ] {
        assert_eq!(&w.ask(port, &req), want);
    }
    assert_eq!(
        w.ask(port, &request(RRQ, WLANMDSP, &[("blksize", "4")])),
        tftp::error(tftp::ERR_EOPTNEG, "Option negotiation failed")
    );
    // truncated: no answer at all
    w.send(port, b"\0\x01/readwrite/x");
    assert!(w.received().is_empty());
    assert_eq!(w.transfers(), 0, "error ports are closed again");
}

#[test]
fn a_failed_read_ends_the_transfer_with_an_error() {
    let mut w = World::new(file(5000));
    let port = w.find(tqftp::SERVICE);
    w.send(port, &request(RRQ, WLANMDSP, &[("blksize", "512")]));
    let tid = w.received()[0].0;
    w.s.files.fail = true;
    assert_eq!(
        w.ask(tid, &tftp::ack(0)),
        tftp::error(tftp::ERR_UNDEF, "Read error")
    );
    assert_eq!(w.transfers(), 0);
}

/// `/readwrite` keeps what the modem wrote in RAM; `Files` has no way to write and sees nothing of it.
#[test]
fn writes_stay_in_ram_and_read_back() {
    let mut w = World::new(file(100));
    let port = w.find(tqftp::SERVICE);
    let reads = w.s.files.reads;
    let name = "/readwrite/server_check.txt";
    w.send(port, &request(WRQ, name, &[]));
    let r = w.received();
    assert_eq!(r[0].1, tftp::ack(0).to_vec());
    let tid = r[0].0;
    assert_eq!(w.ask(tid, &tftp::data(1, b"hello")), tftp::ack(1).to_vec());
    assert_eq!(w.transfers(), 0, "a short block ends the write");
    assert_eq!(w.s.files.reads, reads);
    assert_eq!(w.s.files.files.len(), 3, "no file appeared");

    assert_eq!(
        w.ask(port, &request(RRQ, name, &[])),
        tftp::data(1, b"hello")
    );
    let tid = w.s.tftp.transfer_ports().next().unwrap();
    w.send(tid, &tftp::ack(1));
    assert_eq!(w.transfers(), 0);

    // a second write replaces the file
    w.send(
        port,
        &request(WRQ, name, &[("blksize", "8"), ("wsize", "2")]),
    );
    let r = w.received();
    assert_eq!(tftp::opcode(&r[0].1), Some(tftp::OACK));
    let tid = r[0].0;
    w.send(tid, &tftp::data(1, b"12345678"));
    assert!(w.received().is_empty(), "the window is not full");
    assert_eq!(w.ask(tid, &tftp::data(2, b"9")), tftp::ack(2).to_vec());
    assert_eq!(w.s.tftp.ram().len_of("server_check.txt"), Some(9));

    // out of sequence
    w.send(port, &request(WRQ, name, &[]));
    let tid = w.received()[0].0;
    assert_eq!(
        w.ask(tid, &tftp::data(5, b"x")),
        tftp::error(tftp::ERR_EBADOP, "Block number out of sequence")
    );
    assert_eq!(w.transfers(), 0);
}

/// A modem filling the RAM store gets the C's "disk full" and the kernel keeps its memory.
#[test]
fn the_ram_store_is_bounded() {
    let mut w = World::new(file(100));
    let port = w.find(tqftp::SERVICE);
    w.send(
        port,
        &request(WRQ, "/readwrite/big", &[("blksize", "60000")]),
    );
    let tid = w.received()[0].0;
    let block = vec![0u8; 60000];
    let mut n = 1u16;
    let answer = loop {
        let a = w.ask(tid, &tftp::data(n, &block));
        if tftp::opcode(&a) != Some(tftp::ACK) {
            break a;
        }
        n += 1;
        assert!((n as usize) < 100, "never refused");
    };
    assert_eq!(
        answer,
        tftp::error(tftp::ERR_ENOSPACE, "Disk full or write error")
    );
    assert!(w.s.tftp.ram().len_of("big").unwrap() as usize <= RW_MAX_BYTES);
    assert_eq!(w.transfers(), 0);
    // out of file slots
    for i in 0..tqftp::RW_MAX_FILES {
        w.send(port, &request(WRQ, &format!("/readwrite/f{i}"), &[]));
        let tid = w.received()[0].0;
        w.send(tid, &tftp::data(1, b""));
        w.received();
    }
    assert_eq!(
        w.ask(port, &request(WRQ, "/readwrite/last", &[])),
        tftp::error(tftp::ERR_EACCESS, "Access violation")
    );
}

#[test]
fn del_client_and_bye_end_transfers() {
    let mut w = World::new(file(5000));
    let port = w.find(tqftp::SERVICE);
    let other = w.modem.bind(0).unwrap();
    w.modem
        .send(
            other,
            1,
            port,
            &request(RRQ, WLANMDSP, &[("blksize", "512")]),
        )
        .unwrap();
    w.send(port, &request(RRQ, WLANMDSP, &[("blksize", "512")]));
    assert_eq!(w.transfers(), 2);

    // the modem closes one client: DEL_CLIENT
    w.modem.close(other);
    w.shuttle();
    assert_eq!(w.transfers(), 1);

    // the channel goes down: BYE for the node, the rest go
    w.host.endpoint_down();
    w.shuttle();
    assert_eq!(w.transfers(), 0);

    // the modem comes back: the services are announced again and answer
    w.modem.endpoint_down();
    while w.modem.poll_event().is_some() {}
    w.modem.endpoint_up();
    w.host.endpoint_up();
    w.shuttle();
    let port = w.find(servreg::SERVICE);
    let r = w.ask(port, &servreg::domain_list_request(1, "wlan/fw", None));
    let d = servreg::parse_domain_list_response(&r).unwrap();
    assert_eq!(d.domains, vec![("msm/modem/wlan_pd".to_string(), 180)]);
}

#[test]
fn transfers_are_limited() {
    let mut w = World::new(file(100));
    let port = w.find(tqftp::SERVICE);
    for _ in 0..MAX_TRANSFERS + 20 {
        w.send(port, &request(RRQ, WLANMDSP, &[("blksize", "8")]));
        w.received();
    }
    assert_eq!(w.transfers(), MAX_TRANSFERS);
}

#[test]
fn pd_mapper_answers_wlan_fw() {
    let mut w = World::new(file(1));
    let port = w.find(servreg::SERVICE);
    let r = w.ask(port, &servreg::domain_list_request(7, "wlan/fw", None));
    let d = servreg::parse_domain_list_response(&r).unwrap();
    assert_eq!(d.result, (0, 0));
    assert_eq!(d.total, Some(1));
    assert_eq!(d.revision, Some(1));
    assert_eq!(d.domains, vec![("msm/modem/wlan_pd".to_string(), 180)]);
    assert_eq!(qmi::parse(&r).unwrap().header.txn, 7);

    let r = w.ask(port, &servreg::domain_list_request(8, "tms/servreg", None));
    let d = servreg::parse_domain_list_response(&r).unwrap();
    assert_eq!(d.total, Some(2));
    let r = w.ask(
        port,
        &servreg::domain_list_request(9, "tms/servreg", Some(1)),
    );
    let d = servreg::parse_domain_list_response(&r).unwrap();
    assert_eq!(d.domains, vec![("msm/modem/wlan_pd".to_string(), 180)]);

    let r = w.ask(port, &servreg::domain_list_request(9, "gps/none", None));
    assert!(qmi::parse(&r).unwrap().get(0x12).is_none());

    // a restart request is logged and gets nothing
    let mut b = qmi::Builder::new();
    b.tlv(1, b"wlan/fw").tlv(2, b"test");
    w.send(port, &b.finish(qmi::REQUEST, 1, servreg::PFR));
    assert!(w.received().is_empty());
    let mut lines = Vec::new();
    while let Some(l) = w.s.pop_log() {
        lines.push(l);
    }
    assert!(
        lines.iter().any(|l| l.contains("pfr: wlan/fw (test)")),
        "{lines:?}"
    );
    // the three maps that are not there were said to be missing
    assert!(lines.iter().any(|l| l.contains("adspr.jsn")), "{lines:?}");
}

#[test]
fn no_maps_no_pd_mapper() {
    let mut host = Router::new(1);
    let mut s = Services::new(
        MemFiles::default(),
        Disk::default(),
        Window(vec![0; rmtfs::RMTFS_MEM_SIZE as usize]),
    );
    let ports = s.start(&mut host).unwrap();
    assert_eq!(ports.pd_mapper, None);
    assert!(s.pop_log().is_some());
    assert!(host.servers().iter().all(|srv| srv.service != 64));
    assert_eq!(host.servers().len(), 2);
}

fn result(r: &[u8]) -> (u16, u16) {
    qmi::parse(r).unwrap().result().unwrap()
}

#[test]
fn rmtfs_open_alloc_read_and_a_discarded_write() {
    let mut w = World::new(file(1));
    let port = w.find(rmtfs::SERVICE);
    let img = efs();
    let base = rmtfs::RMTFS_MEM_PHYS as u32;

    let open = |path: &str| Request::Open { path: path.into() };
    let r = w.ask(port, &rmtfs::encode_request(1, &open("/boot/modem_fs1")));
    assert_eq!(result(&r), (0, 0));
    assert_eq!(qmi::parse(&r).unwrap().u32(0x10), Ok(Some(0)));
    // again: the same id
    let r = w.ask(port, &rmtfs::encode_request(2, &open("/boot/modem_fs1")));
    assert_eq!(qmi::parse(&r).unwrap().u32(0x10), Ok(Some(0)));
    // an absent partition and an unknown path: errors, no id
    for p in ["/boot/modem_fsg", "/boot/nope"] {
        let r = w.ask(port, &rmtfs::encode_request(3, &open(p)));
        assert_eq!(result(&r), (1, rmtfs::ERR_INTERNAL), "{p}");
        assert_eq!(qmi::parse(&r).unwrap().u32(0x10), Ok(None));
    }

    // alloc-buf: the window's physical address, if it fits
    let alloc = |size| Request::AllocBuf { caller_id: 0, size };
    let r = w.ask(port, &rmtfs::encode_request(4, &alloc(0x10_0000)));
    assert_eq!(qmi::parse(&r).unwrap().u64(0x10), Ok(Some(0x9460_0000)));
    let r = w.ask(port, &rmtfs::encode_request(5, &alloc(0x20_0001)));
    assert_eq!(result(&r), (1, rmtfs::ERR_INTERNAL));

    let iovec = |write, entries: &[IovecEntry]| Request::Iovec {
        caller_id: 0,
        write,
        entries: entries.to_vec(),
        force_sync: write,
    };
    let entry = |sector_addr, at: u32, num_sector| IovecEntry {
        sector_addr,
        phys_offset: base + at,
        num_sector,
    };

    // read two runs into the window
    let req = iovec(false, &[entry(2, 0x200, 2), entry(6, 0x1000, 1)]);
    let r = w.ask(port, &rmtfs::encode_request(6, &req));
    assert_eq!(result(&r), (0, 0));
    assert_eq!(&w.s.rmtfs.mem.0[0x200..0x600], &img[1024..2048]);
    assert_eq!(&w.s.rmtfs.mem.0[0x1000..0x1200], &img[3072..3584]);

    // a read that runs off the end of the partition: zeros there
    w.s.rmtfs.mem.0[0x4000..0x4400].fill(0xff);
    let req = iovec(false, &[entry(15, 0x4000, 2)]);
    assert_eq!(
        result(&w.ask(port, &rmtfs::encode_request(7, &req))),
        (0, 0)
    );
    assert_eq!(&w.s.rmtfs.mem.0[0x4000..0x4200], &img[7680..8192]);
    assert!(w.s.rmtfs.mem.0[0x4200..0x4400].iter().all(|&b| b == 0));

    // a write: the modem fills the window and asks. No read or anything else happens on the partition.
    let reads = w.s.rmtfs.partitions.reads;
    w.s.rmtfs.mem.0[0x8000..0x8200].fill(0xab);
    let req = iovec(true, &[entry(1, 0x8000, 1)]);
    assert_eq!(
        result(&w.ask(port, &rmtfs::encode_request(8, &req))),
        (0, 0)
    );
    assert_eq!(
        w.s.rmtfs.partitions.reads, reads,
        "a write touched the partition"
    );
    assert_eq!(
        w.s.rmtfs.partitions.parts["modemst1"], img,
        "the disk changed"
    );
    assert_eq!(w.s.rmtfs.shadow_bytes(), 512);

    // and reads back from memory, in the middle of a run from the partition
    let req = iovec(false, &[entry(0, 0x10000, 3)]);
    assert_eq!(
        result(&w.ask(port, &rmtfs::encode_request(9, &req))),
        (0, 0)
    );
    let win = &w.s.rmtfs.mem.0;
    assert_eq!(&win[0x10000..0x10200], &img[..512]);
    assert!(win[0x10200..0x10400].iter().all(|&b| b == 0xab));
    assert_eq!(&win[0x10400..0x10600], &img[1024..1536]);

    // a write past the end of the partition is kept too, as the C's shadow grows
    w.s.rmtfs.mem.0[0x8000..0x8200].fill(0xcd);
    let req = iovec(true, &[entry(100, 0x8000, 1)]);
    assert_eq!(
        result(&w.ask(port, &rmtfs::encode_request(10, &req))),
        (0, 0)
    );
    let req = iovec(false, &[entry(100, 0x9000, 1)]);
    assert_eq!(
        result(&w.ask(port, &rmtfs::encode_request(11, &req))),
        (0, 0)
    );
    assert!(w.s.rmtfs.mem.0[0x9000..0x9200].iter().all(|&b| b == 0xcd));

    // outside the window, in both directions and over its end
    for bad in [
        IovecEntry {
            sector_addr: 0,
            phys_offset: base - 512,
            num_sector: 1,
        },
        IovecEntry {
            sector_addr: 0,
            phys_offset: 0,
            num_sector: 1,
        },
        IovecEntry {
            sector_addr: 0,
            phys_offset: base + 0x1f_ff00,
            num_sector: 1,
        },
        IovecEntry {
            sector_addr: 0,
            phys_offset: base,
            num_sector: u32::MAX,
        },
        IovecEntry {
            sector_addr: u32::MAX,
            phys_offset: u32::MAX,
            num_sector: u32::MAX,
        },
    ] {
        for write in [false, true] {
            let r = w.ask(port, &rmtfs::encode_request(12, &iovec(write, &[bad])));
            assert_eq!(result(&r), (1, rmtfs::ERR_INTERNAL), "{bad:?}");
        }
    }

    // a read error of the disk is an error, not a short read
    w.s.rmtfs.partitions.fail = true;
    let req = iovec(false, &[entry(0, 0, 1)]);
    let r = w.ask(port, &rmtfs::encode_request(13, &req));
    assert_eq!(result(&r), (1, rmtfs::ERR_INTERNAL));
    w.s.rmtfs.partitions.fail = false;

    // an unknown caller
    let req = Request::Iovec {
        caller_id: 7,
        write: false,
        entries: vec![],
        force_sync: false,
    };
    let r = w.ask(port, &rmtfs::encode_request(14, &req));
    assert_eq!(result(&r), (1, rmtfs::ERR_INTERNAL));

    // dev error, close, and what close frees
    let dev = Request::DevError { caller_id: 0 };
    let r = w.ask(port, &rmtfs::encode_request(15, &dev));
    assert_eq!(qmi::parse(&r).unwrap().u8(0x10), Ok(Some(0)));
    assert!(w.s.rmtfs.shadow_bytes() > 0);
    let close = Request::Close { caller_id: 0 };
    assert_eq!(
        result(&w.ask(port, &rmtfs::encode_request(16, &close))),
        (0, 0)
    );
    assert_eq!(w.s.rmtfs.shadow_bytes(), 0);
    assert_eq!(
        result(&w.ask(port, &rmtfs::encode_request(17, &close))),
        (1, rmtfs::ERR_INTERNAL)
    );
}

/// As in the C, a modem that goes away keeps its callers and its writes: it opens again and finds them.
#[test]
fn rmtfs_survives_the_client_going_away() {
    let mut w = World::new(file(1));
    let port = w.find(rmtfs::SERVICE);
    let open = Request::Open {
        path: "/boot/modem_fs1".into(),
    };
    w.ask(port, &rmtfs::encode_request(1, &open));
    w.s.rmtfs.mem.0[..512].fill(0x5a);
    let wr = Request::Iovec {
        caller_id: 0,
        write: true,
        entries: vec![IovecEntry {
            sector_addr: 0,
            phys_offset: rmtfs::RMTFS_MEM_PHYS as u32,
            num_sector: 1,
        }],
        force_sync: true,
    };
    assert_eq!(result(&w.ask(port, &rmtfs::encode_request(2, &wr))), (0, 0));

    w.host.endpoint_down();
    w.modem.endpoint_down();
    w.shuttle();
    while w.modem.poll_event().is_some() {}
    w.host.endpoint_up();
    w.modem.endpoint_up();
    w.shuttle();

    let port = w.find(rmtfs::SERVICE);
    let r = w.ask(port, &rmtfs::encode_request(3, &open));
    assert_eq!(qmi::parse(&r).unwrap().u32(0x10), Ok(Some(0)));
    assert_eq!(w.s.rmtfs.shadow_bytes(), 512);
}

#[test]
fn rmtfs_malformed_and_foreign_nodes() {
    let mut w = World::new(file(1));
    let port = w.find(rmtfs::SERVICE);
    let mut b = qmi::Builder::new();
    b.u8(1, 3);
    let r = w.ask(port, &b.finish(qmi::REQUEST, 5, rmtfs::CLOSE));
    assert_eq!(result(&r), (1, rmtfs::ERR_MALFORMED));
    // an unknown message and something that is not QMI: no answer
    w.send(port, &b.finish(qmi::REQUEST, 5, 42));
    w.send(port, &[0, 1]);
    assert!(w.received().is_empty());
    // an iovec array whose count does not match its length
    let mut b = qmi::Builder::new();
    b.u32(1, 0).u8(2, 0).tlv(3, &[2, 0, 0, 0]).u8(4, 0);
    let r = w.ask(port, &b.finish(qmi::REQUEST, 6, rmtfs::RW_IOVEC));
    assert_eq!(result(&r), (1, rmtfs::ERR_MALFORMED));

    // callers belong to their node
    let mut log = lucina_qcom_services::Log::new();
    let open = rmtfs::encode_request(
        1,
        &Request::Open {
            path: "/boot/modem_fs1".into(),
        },
    );
    assert_eq!(
        result(&w.s.rmtfs.handle(5, &open, &mut log).unwrap()),
        (0, 0)
    );
    let close = rmtfs::encode_request(2, &Request::Close { caller_id: 0 });
    assert_eq!(
        result(&w.s.rmtfs.handle(6, &close, &mut log).unwrap()),
        (1, rmtfs::ERR_INTERNAL)
    );
}

/// Written sectors stop at the limit: one node, every slot, and the C's per-file limit.
#[test]
fn rmtfs_shadow_is_bounded() {
    let mut w = World::new(file(1));
    let mut log = lucina_qcom_services::Log::new();
    let open = rmtfs::encode_request(
        1,
        &Request::Open {
            path: "/boot/modem_fs1".into(),
        },
    );
    w.s.rmtfs.handle(0, &open, &mut log).unwrap();
    let base = rmtfs::RMTFS_MEM_PHYS as u32;
    let write_at = |sector_addr, num_sector| {
        rmtfs::encode_request(
            2,
            &Request::Iovec {
                caller_id: 0,
                write: true,
                entries: vec![IovecEntry {
                    sector_addr,
                    phys_offset: base,
                    num_sector,
                }],
                force_sync: false,
            },
        )
    };
    // the C refuses a write that reaches 16 MiB
    let r = w.s.rmtfs.handle(0, &write_at(32767, 1), &mut log).unwrap();
    assert_eq!(result(&r), (1, rmtfs::ERR_INTERNAL));
    let r = w.s.rmtfs.handle(0, &write_at(32766, 1), &mut log).unwrap();
    assert_eq!(result(&r), (0, 0));
    assert_eq!(w.s.rmtfs.shadow_bytes(), 512);
    // the same sector again takes no more
    w.s.rmtfs.handle(0, &write_at(32766, 1), &mut log).unwrap();
    assert_eq!(w.s.rmtfs.shadow_bytes(), 512);
}

/// What is not for the services comes back to the caller.
#[test]
fn other_events_come_back() {
    let mut w = World::new(file(1));
    let wlfw = w.host.bind(0).unwrap();
    w.host.lookup(wlfw, 69, 0).unwrap();
    w.modem.publish(w.client, 69, 1).unwrap();
    w.shuttle();
    assert!(
        w.stray
            .iter()
            .any(|e| matches!(e, Event::ServiceNew { port, .. } if *port == wlfw))
    );
    w.modem.send(w.client, 1, wlfw, b"wlfw data").unwrap();
    w.shuttle();
    assert!(w.stray.iter().any(
        |e| matches!(e, Event::Data { port, data, .. } if *port == wlfw && data == b"wlfw data")
    ));
    let gone = Event::TxResumed { node: 0, port: 999 };
    assert_eq!(w.s.on_event(&mut w.host, gone.clone()), Some(gone));
}

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
}

/// Random bytes and damaged good messages at every port, from the channel's side and straight at `on_event`: nothing
/// panics, nothing grows past its limits, and the services still answer afterwards.
#[test]
fn hostile_input_never_panics() {
    let mut w = World::new(file(70_000));
    let ports = w.s.ports().unwrap();
    let good: Vec<Vec<u8>> = vec![
        request(
            RRQ,
            WLANMDSP,
            &[("blksize", "512"), ("tsize", "0"), ("wsize", "65535")],
        ),
        request(RRQ, WLANMDSP, &[]),
        request(
            RRQ,
            WLANMDSP,
            &[("seek", "9223372036854775807"), ("rsize", "104857600")],
        ),
        request(
            WRQ,
            "/readwrite/a",
            &[("blksize", "65464"), ("wsize", "65535")],
        ),
        tftp::ack(0).to_vec(),
        tftp::ack(65535).to_vec(),
        tftp::data(1, &[0; 100]),
        tftp::error(9, "x"),
        servreg::domain_list_request(1, "wlan/fw", Some(u32::MAX)),
        rmtfs::encode_request(
            1,
            &Request::Open {
                path: "/boot/modem_fs1".into(),
            },
        ),
        rmtfs::encode_request(
            1,
            &Request::Iovec {
                caller_id: 0,
                write: true,
                entries: vec![IovecEntry {
                    sector_addr: u32::MAX,
                    phys_offset: u32::MAX,
                    num_sector: u32::MAX,
                }],
                force_sync: true,
            },
        ),
        rmtfs::encode_request(
            1,
            &Request::AllocBuf {
                caller_id: u32::MAX,
                size: u32::MAX,
            },
        ),
        rmtfs::encode_request(
            1,
            &Request::DevError {
                caller_id: u32::MAX,
            },
        ),
    ];
    let mut rng = Rng(0x1234_5678_9abc_def1);
    for round in 0..60_000 {
        let mut msg = match rng.below(3) {
            0 => {
                let n = rng.below(64);
                (0..n).map(|_| rng.next() as u8).collect()
            }
            _ => good[rng.below(good.len())].clone(),
        };
        for _ in 0..rng.below(4) {
            if !msg.is_empty() {
                let i = rng.below(msg.len());
                match rng.below(3) {
                    0 => msg[i] = rng.next() as u8,
                    1 => msg.truncate(i),
                    _ => msg[i] ^= 1 << rng.below(8),
                }
            }
        }
        let mut targets = vec![ports.tftp, ports.rmtfs, ports.pd_mapper.unwrap()];
        targets.extend(w.s.tftp.transfer_ports());
        let port = targets[rng.below(targets.len())];
        // from the modem's client, a stranger on its node, or a node nobody knows
        let (src_node, src_port) = match rng.below(8) {
            0 => (77, rng.next() as u32),
            1 => (0, 0xffff_fffe),
            _ => (0, w.client),
        };
        let ev = Event::Data {
            port,
            src_node,
            src_port,
            data: msg,
        };
        assert_eq!(w.s.on_event(&mut w.host, ev), None);
        match rng.below(16) {
            0 => {
                w.s.on_event(
                    &mut w.host,
                    Event::DelClient {
                        port: ports.tftp,
                        node: 0,
                        client_port: w.client,
                    },
                );
            }
            1 => {
                w.s.on_event(
                    &mut w.host,
                    Event::Bye {
                        port: ports.tftp,
                        node: 77,
                    },
                );
            }
            _ => {}
        }
        assert!(w.transfers() <= MAX_TRANSFERS);
        if round % 16 == 0 {
            w.shuttle();
            w.received();
        }
    }
    w.shuttle();
    w.received();
    // the channel is still good, and so are the services
    let port = ports.pd_mapper.unwrap();
    let r = w.ask(port, &servreg::domain_list_request(1, "wlan/fw", None));
    let d = servreg::parse_domain_list_response(&r).unwrap();
    assert_eq!(d.domains.len(), 1);
}
