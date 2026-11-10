//! The Rust encoders and decoders against the C originals' bytes (`fixtures/c-oracle.txt`, a copy of the userspace
//! crate's, printed by `userspace/crates/lucina-qcom/oracle/oracle.c` from linux-msm's libqrtr, pd-mapper, rmtfs and
//! tqftpserv; pins in that directory's README).

use std::collections::HashMap;

use lucina_qcom_services::qmi::{self, Builder};
use lucina_qcom_services::rmtfs::{self, IovecEntry, Request, Rmtfs, Value};
use lucina_qcom_services::servreg::{self, PdMap, PdMapper};
use lucina_qcom_services::tftp::{self, AckStep, Options, Reader};
use lucina_qcom_services::{
    Files, IoError, Log, PUBLISH_PD_MAPPER, PUBLISH_RMTFS, PUBLISH_TQFTP, Partitions, RmtfsMem,
    translate,
};
use lucina_qrtr::packet::{CtrlPkt, decode, pkt_type};
use lucina_qrtr::router::Router;

struct Fixture {
    packets: Vec<Vec<u8>>,
    fields: Vec<String>,
}

fn unhex(s: &str) -> Vec<u8> {
    if s == "-" {
        return Vec::new();
    }
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
        .collect()
}

/// name -> fixture; `sanitize` lines (one name, many inputs) under `sanitize#<n>`.
fn fixtures() -> HashMap<String, Fixture> {
    let text = include_str!("fixtures/c-oracle.txt");
    let mut out = HashMap::new();
    let mut n = 0;
    for line in text.lines().filter(|l| !l.is_empty()) {
        let mut parts = line.split(' ');
        let mut name = parts.next().unwrap().to_string();
        if name == "sanitize" {
            name = format!("sanitize#{n}");
            n += 1;
        }
        let packets = parts.next().unwrap_or("-").split(',').map(unhex).collect();
        let fields = parts.filter(|f| !f.is_empty()).map(String::from).collect();
        out.insert(name, Fixture { packets, fields });
    }
    out
}

fn one(f: &HashMap<String, Fixture>, name: &str) -> Vec<u8> {
    let fx = f
        .get(name)
        .unwrap_or_else(|| panic!("fixture {name} missing"));
    assert_eq!(fx.packets.len(), 1, "{name}");
    fx.packets[0].clone()
}

fn fields<'a>(f: &'a HashMap<String, Fixture>, name: &str) -> &'a [String] {
    &f[name].fields
}

/// `Router::publish` with our constants must say what libqrtr's `qrtr_publish()` says. The router fills in node and
/// port for a local server, so only those differ from the fixture, which has zeros there.
#[test]
fn publishing_says_what_libqrtr_says() {
    let f = fixtures();
    for (name, p) in [
        ("ctrl_publish_tqftpserv", PUBLISH_TQFTP),
        ("ctrl_publish_pd_mapper", PUBLISH_PD_MAPPER),
        ("ctrl_publish_rmtfs", PUBLISH_RMTFS),
    ] {
        let want = one(&f, name);
        let want = CtrlPkt::parse(&want).unwrap();
        let mut r = Router::new(1);
        r.endpoint_up();
        while r.poll_tx().is_some() {}
        let port = r.bind(0).unwrap();
        r.publish(port, p.service, p.instance_word()).unwrap();
        let msg = r.poll_tx().unwrap();
        let pkt = decode(&msg).unwrap();
        assert_eq!(pkt.ptype, pkt_type::NEW_SERVER, "{name}");
        let got = CtrlPkt::parse(pkt.payload).unwrap();
        assert_eq!(got.cmd, want.cmd, "{name}");
        assert_eq!(got.server().service, want.server().service, "{name}");
        assert_eq!(got.server().instance, want.server().instance, "{name}");
        assert_eq!((got.server().node, got.server().port), (1, port), "{name}");
    }
}

/// Samsung's maps (adspr, adspua, cdspr, modemr, modemuw.jsn), in file-name order, as pd-mapper builds them: each
/// file's domain with its services.
fn samsung_maps() -> Vec<PdMap> {
    let files: [(&str, u32, &[&str]); 5] = [
        ("msm/adsp/root_pd", 74, &["tms/servreg", "tms/pdr_enabled"]),
        ("msm/adsp/audio_pd", 74, &["tms/servreg", "avs/audio"]),
        ("msm/cdsp/root_pd", 76, &["tms/servreg"]),
        (
            "msm/modem/root_pd",
            180,
            &["tms/servreg", "gps/gps_service", "tms/pdr_enabled"],
        ),
        (
            "msm/modem/wlan_pd",
            180,
            &["kernel/elf_loader", "tms/servreg", "wlan/fw"],
        ),
    ];
    files
        .iter()
        .flat_map(|(d, i, s)| {
            s.iter().map(move |svc| PdMap {
                service: svc.to_string(),
                domain: d.to_string(),
                instance: *i,
            })
        })
        .collect()
}

/// Samsung's real `.jsn` files and `wlanmdsp.mbn`, where this machine has them (buildhost; a clean checkout skips and
/// says so): the maps through `Files` give pd-mapper's fixture bytes, and the real blob goes through a whole
/// transfer unchanged.
#[test]
fn real_firmware_files_when_present() {
    let dir = std::env::var_os("HOME").map(|h| std::path::Path::new(&h).join("scratch/gbgo/fw"));
    let Some(dir) = dir.filter(|d| d.join("modemuw.jsn").exists()) else {
        eprintln!("SKIPPED: no ~/scratch/gbgo/fw here");
        return;
    };
    struct Dir(std::path::PathBuf);
    impl Files for Dir {
        fn file_len(&mut self, name: &str) -> Option<u64> {
            std::fs::metadata(self.0.join(name)).ok().map(|m| m.len())
        }
        fn read_at(&mut self, name: &str, offset: u64, buf: &mut [u8]) -> Result<usize, IoError> {
            use std::os::unix::fs::FileExt;
            let f = std::fs::File::open(self.0.join(name)).map_err(|_| IoError)?;
            f.read_at(buf, offset).map_err(|_| IoError)
        }
    }
    let f = fixtures();
    let mut files = Dir(dir.clone());
    let mut log = Log::new();
    let pd = PdMapper::load(&mut files, &servreg::JSN_FILES, &mut log);
    assert_eq!(log.pop(), None);
    assert_eq!(pd.maps(), samsung_maps());
    for (name, txn, service) in [
        ("servreg_resp_wlan_fw", 7, "wlan/fw"),
        ("servreg_resp_tms_servreg", 8, "tms/servreg"),
    ] {
        assert_eq!(
            servreg::domain_list_response(txn, pd.maps(), service, 0),
            one(&f, name)
        );
    }

    let real = std::fs::read(dir.join("wlanmdsp.mbn")).unwrap();
    assert_eq!(real.len(), 3_832_348);
    let mut r = Reader::new(Options {
        blksize: 7680,
        wsize: 10,
        ..Options::default()
    });
    let mut got = Vec::new();
    let mut last = 0;
    while r.on_ack(last) == AckStep::Window {
        while let Some(p) = r
            .next_data(|off, b| files.read_at("wlanmdsp.mbn", off, b))
            .unwrap()
        {
            got.extend_from_slice(&p[4..]);
            last += 1;
        }
    }
    assert!(got == real);
    eprintln!("USED the real firmware files in {}", dir.display());
}

#[test]
fn pd_mapper_messages() {
    let f = fixtures();
    let maps = samsung_maps();
    let mut pd = PdMapper::from_maps(maps.clone());
    let mut log = Log::new();
    let req = one(&f, "servreg_req_wlan_fw");
    assert_eq!(
        fields(&f, "servreg_req_wlan_fw"),
        ["0", "1", "wlan/fw", "0", "0"]
    );
    assert_eq!(req, servreg::domain_list_request(1, "wlan/fw", None));
    assert_eq!(
        fields(&f, "servreg_req_offset"),
        ["0", "2", "tms/servreg", "1", "1"]
    );
    assert_eq!(
        one(&f, "servreg_req_offset"),
        servreg::domain_list_request(2, "tms/servreg", Some(1))
    );

    // responses: pd-mapper's, byte for byte
    let replies = pd.on_message(0, 77, &req, &mut log);
    assert_eq!((replies[0].dst_node, replies[0].dst_port), (0, 77));
    let mut want = one(&f, "servreg_resp_wlan_fw");
    want[1] = 1; // the fixture used txn 7; this request's is 1
    assert_eq!(replies[0].payload, want);
    assert_eq!(
        servreg::domain_list_response(7, &maps, "wlan/fw", 0),
        one(&f, "servreg_resp_wlan_fw")
    );
    assert_eq!(
        servreg::domain_list_response(8, &maps, "tms/servreg", 0),
        one(&f, "servreg_resp_tms_servreg")
    );
    assert_eq!(
        servreg::domain_list_response(9, &maps, "nobody/none", 0),
        one(&f, "servreg_resp_none")
    );
    let mut bad = Builder::new();
    bad.u32(5, 1);
    let replies = pd.on_message(
        0,
        77,
        &bad.finish(qmi::REQUEST, 10, servreg::GET_DOMAIN_LIST),
        &mut log,
    );
    assert_eq!(replies[0].payload, one(&f, "servreg_resp_malformed"));

    let d = servreg::parse_domain_list_response(&one(&f, "servreg_resp_tms_servreg")).unwrap();
    assert_eq!(d.total, Some(5));
    assert_eq!(d.domains[2], ("msm/cdsp/root_pd".to_string(), 76));
}

#[test]
fn rmtfs_messages() {
    let f = fixtures();
    let open = Request::Open {
        path: "/boot/modem_fs1".into(),
    };
    assert_eq!(fields(&f, "rmtfs_open_req"), ["0", "3", "/boot/modem_fs1"]);
    let iov = Request::Iovec {
        caller_id: 0,
        write: true,
        entries: vec![
            IovecEntry {
                sector_addr: 0,
                phys_offset: 0x9460_0000,
                num_sector: 8,
            },
            IovecEntry {
                sector_addr: 16,
                phys_offset: 0x9460_1000,
                num_sector: 4,
            },
        ],
        force_sync: true,
    };
    let reqs = [
        ("rmtfs_close_req", 5, Request::Close { caller_id: 0 }),
        ("rmtfs_iovec_req", 6, iov),
        (
            "rmtfs_alloc_req",
            8,
            Request::AllocBuf {
                caller_id: 0,
                size: 0x10_0000,
            },
        ),
        ("rmtfs_dev_error_req", 9, Request::DevError { caller_id: 0 }),
        ("rmtfs_open_req", 3, open),
    ];
    for (name, txn, r) in &reqs {
        let bytes = one(&f, name);
        assert_eq!(&rmtfs::encode_request(*txn, r), &bytes, "{name}");
        let m = qmi::parse(&bytes).unwrap();
        assert_eq!(&rmtfs::decode_request(&m).unwrap(), r, "{name}");
    }
    let resps = [
        (
            "rmtfs_open_resp_ok",
            3,
            rmtfs::OPEN,
            None,
            Value::CallerId(0),
        ),
        (
            "rmtfs_open_resp_fail",
            4,
            rmtfs::OPEN,
            Some(rmtfs::ERR_INTERNAL),
            Value::None,
        ),
        ("rmtfs_close_resp_ok", 5, rmtfs::CLOSE, None, Value::None),
        ("rmtfs_iovec_resp_ok", 6, rmtfs::RW_IOVEC, None, Value::None),
        (
            "rmtfs_iovec_resp_fail",
            7,
            rmtfs::RW_IOVEC,
            Some(rmtfs::ERR_INTERNAL),
            Value::None,
        ),
        (
            "rmtfs_alloc_resp_ok",
            8,
            rmtfs::ALLOC_BUFF,
            None,
            Value::Address(0x9460_0000),
        ),
        (
            "rmtfs_dev_error_resp_ok",
            9,
            rmtfs::GET_DEV_ERROR,
            None,
            Value::Status(0),
        ),
        (
            "rmtfs_close_resp_malformed",
            10,
            rmtfs::CLOSE,
            Some(rmtfs::ERR_MALFORMED),
            Value::None,
        ),
    ];
    for (name, txn, id, err, v) in resps {
        assert_eq!(
            rmtfs::encode_response(txn, id, err, v),
            one(&f, name),
            "{name}"
        );
    }
}

struct Mem(Vec<u8>);

impl RmtfsMem for Mem {
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

struct Sevens;

impl Partitions for Sevens {
    fn partition_len(&mut self, name: &str) -> Option<u64> {
        (name == "modemst1").then_some(16 * 512)
    }
    fn read_at(&mut self, _: &str, _: u64, buf: &mut [u8]) -> Result<(), IoError> {
        buf.fill(7);
        Ok(())
    }
}

#[test]
fn rmtfs_server_answers_c_requests_with_c_responses() {
    let f = fixtures();
    let mut s = Rmtfs::new(Sevens, Mem(vec![0; 0x20_0000]));
    let mut log = Log::new();
    let mut ask = |req: &str| s.handle(1, &one(&f, req), &mut log).unwrap();
    assert_eq!(ask("rmtfs_open_req"), one(&f, "rmtfs_open_resp_ok"));
    assert_eq!(ask("rmtfs_iovec_req"), one(&f, "rmtfs_iovec_resp_ok"));
    assert_eq!(ask("rmtfs_alloc_req"), one(&f, "rmtfs_alloc_resp_ok"));
    assert_eq!(
        ask("rmtfs_dev_error_req"),
        one(&f, "rmtfs_dev_error_resp_ok")
    );
    assert_eq!(ask("rmtfs_close_req"), one(&f, "rmtfs_close_resp_ok"));
}

#[test]
fn tftp_packets() {
    let f = fixtures();
    let modem = Options {
        blksize: 7680,
        timeoutms: 1000,
        tsize: Some(3_832_348),
        wsize: 10,
        rsize: 0,
        seek: None,
    };
    assert_eq!(one(&f, "tftp_oack_modem"), tftp::oack(&modem));
    let stat = Options {
        rsize: 52,
        seek: Some(0),
        ..Options::default()
    };
    assert_eq!(one(&f, "tftp_oack_rsize_seek"), tftp::oack(&stat));
    assert_eq!(
        one(&f, "tftp_error_enoent"),
        tftp::error(tftp::ERR_ENOENT, "file not found")
    );
    assert_eq!(one(&f, "tftp_ack5"), tftp::ack(5));
}

#[test]
fn tftp_option_parsing() {
    let f = fixtures();
    let mut names: Vec<&String> = f.keys().filter(|k| k.starts_with("opts_")).collect();
    names.sort();
    assert!(names.len() >= 20, "{names:?}");
    for name in names {
        let fx = &f[name.as_str()];
        // ret blksize tsize wsize timeoutms rsize seek
        let c: Vec<i64> = fx.fields.iter().map(|v| v.parse().unwrap()).collect();
        let got = tftp::parse_options(&fx.packets[0]);
        if c[0] < 0 {
            assert_eq!(got, None, "{name}");
            continue;
        }
        let o = got.unwrap_or_else(|| panic!("{name}: refused, C accepts"));
        let rust = [
            0,
            o.blksize as i64,
            o.tsize.map_or(-1, |t| t as i64),
            o.wsize as i64,
            i64::from(o.timeoutms),
            o.rsize as i64,
            o.seek.map_or(-1, |s| s as i64),
        ];
        assert_eq!(rust.to_vec(), c, "{name}");
    }
}

/// The reader cases in oracle.c: (name, file length, blksize, wsize, rsize, seek, acks).
type ReaderCase = (
    &'static str,
    usize,
    usize,
    usize,
    usize,
    i64,
    &'static [u16],
);
const READERS: [ReaderCase; 6] = [
    ("reader_rsize200", 1000, 64, 3, 200, 0, &[0, 3, 4]),
    ("reader_rsize_multiple", 1000, 64, 4, 128, -1, &[0, 3]),
    ("reader_rsize_seek", 1000, 64, 2, 100, 300, &[0, 2]),
    ("reader_rsize_past_eof", 150, 64, 10, 500, 0, &[0]),
    ("reader_no_rsize", 200, 64, 1, 0, -1, &[0, 1]),
    ("reader_window_resend", 1000, 64, 2, 300, 0, &[0, 2, 4, 5]),
];

/// `on_ack` then every `next_data`, which is one window of the userspace crate's `Reader::on_ack`.
fn window(
    r: &mut Reader,
    ack: u16,
    read_at: impl Fn(u64, &mut [u8]) -> Result<usize, IoError>,
) -> Option<Vec<Vec<u8>>> {
    if r.on_ack(ack) == AckStep::Done {
        return None;
    }
    let mut out = Vec::new();
    while let Some(p) = r.next_data(&read_at).unwrap() {
        out.push(p);
    }
    Some(out)
}

#[test]
fn tftp_reads_send_what_tqftpserv_sends() {
    let f = fixtures();
    for (name, len, blksize, wsize, rsize, seek, acks) in READERS {
        let file: Vec<u8> = (0..len).map(|i| (i * 7 + i / 251) as u8).collect();
        let read_at = |off: u64, buf: &mut [u8]| -> Result<usize, IoError> {
            let off = off as usize;
            if off >= file.len() {
                return Ok(0);
            }
            let n = buf.len().min(file.len() - off);
            buf[..n].copy_from_slice(&file[off..off + n]);
            Ok(n)
        };
        let opts = Options {
            blksize,
            wsize,
            rsize,
            seek: (seek >= 0).then_some(seek as u64),
            ..Options::default()
        };
        let mut r = Reader::new(opts);
        for &a in acks {
            let key = format!("{name}_ack{a}");
            let fx = &f[key.as_str()];
            let c_done = fx.fields[0] == "0";
            let step = window(&mut r, a, read_at);
            if name == "reader_no_rsize" && a == 1 {
                // the one documented difference: without rsize the C server stops after block 1, this one sends
                // the rest of the file
                assert!(c_done);
                let p = step.expect(&key);
                assert_eq!(p.len(), 1);
                assert_eq!(&p[0][4..], &file[64..128]);
                continue;
            }
            match step {
                None => assert!(c_done, "{key}: Rust done, C sent {:?}", fx.packets.len()),
                Some(p) => {
                    assert!(!c_done, "{key}: C done, Rust sent {} packets", p.len());
                    let want: Vec<Vec<u8>> = fx
                        .packets
                        .iter()
                        .filter(|x| !x.is_empty())
                        .cloned()
                        .collect();
                    assert_eq!(p, want, "{key}");
                }
            }
        }
    }
}

#[test]
fn sanitize_matches_tqftpserv() {
    let f = fixtures();
    let mut n = 0;
    while let Some(fx) = f.get(&format!("sanitize#{n}")) {
        let path = String::from_utf8(fx.packets[0].clone()).unwrap();
        assert_eq!(translate::sanitize(&path), fx.fields[0] == "0", "{path:?}");
        n += 1;
    }
    assert!(n >= 15);
}
