// SPDX-License-Identifier: BSD-3-Clause
//
// From userspace/crates/lucina-qcom/src/servreg.rs, after linux-msm pd-mapper's pd-mapper.c and servreg_loc.qmi
// (Copyright (c) 2018, Linaro Ltd.).

//! The service-registry locator ("pd-mapper"): tells a client which protection domains provide a service, e.g.
//! `wlan/fw` -> `msm/modem/wlan_pd`, instance 180.
//!
//! The maps come from the `.jsn` files beside the firmware, through [`Files`]. Each file names one domain
//! (`sr_domain`: soc/domain/subdomain and the QMI instance) and the services in it (`sr_service`:
//! provider/service).
//!
//! Differences from the C original, on purpose:
//! * the request's optional `offset` is honoured as the kernel's locator does (the C one ignores it);
//! * a `.jsn` that is missing or does not parse is skipped with a message (the C one exits);
//! * [`Files`] cannot list a directory, so the names are [`JSN_FILES`], not a scan;
//! * a list stops short of the 65535 bytes a TLV holds (the C's buffer is far larger than any real map).

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec;
use alloc::vec::Vec;

use crate::json::{self, Value};
use crate::qmi::{self, Builder};
use crate::{Files, Log, Reply};

pub const SERVICE: u32 = 0x40;
pub const VERSION: u16 = 0x101;
pub const INSTANCE: u16 = 0;
pub const GET_DOMAIN_LIST: u16 = 0x21;
pub const PFR: u16 = 0x24;
/// `QMI_ERR_MALFORMED_MSG` in `servreg_loc.qmi`.
pub const ERR_MALFORMED: u16 = 2;
/// The C structs hold 256-byte names, NUL included.
pub const NAME_MAX: usize = 255;
/// `domain_list[255]`.
const LIST_MAX: usize = 255;
/// Room in the list TLV: one more entry never takes more than 265 bytes.
const LIST_BYTES_MAX: usize = 65000;

/// Samsung's maps beside the modem firmware on the Book Go. Missing ones are skipped.
pub const JSN_FILES: [&str; 5] = [
    "adspr.jsn",
    "adspua.jsn",
    "cdspr.jsn",
    "modemr.jsn",
    "modemuw.jsn",
];
/// The longest `.jsn` read; the real ones are under a kilobyte.
pub const JSN_MAX_LEN: u64 = 64 * 1024;

const TLV_NAME: u8 = 1;
const TLV_OFFSET: u8 = 0x10;
const TLV_TOTAL: u8 = 0x10;
const TLV_REVISION: u8 = 0x11;
const TLV_LIST: u8 = 0x12;

/// One service in one domain.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PdMap {
    /// `provider/service`, e.g. `wlan/fw`.
    pub service: String,
    /// `soc/domain/subdomain`, e.g. `msm/modem/wlan_pd`.
    pub domain: String,
    pub instance: u32,
}

/// The maps in one `.jsn` file (pd-mapper's `pd_load_map()`).
pub fn parse_jsn(text: &str) -> Result<Vec<PdMap>, String> {
    let root = json::parse(text)?;
    let d = root.get("sr_domain").ok_or("no sr_domain")?;
    let field = |k: &str| {
        d.get(k)
            .and_then(Value::as_str)
            .ok_or(format!("sr_domain has no {k}"))
    };
    let domain = format!(
        "{}/{}/{}",
        field("soc")?,
        field("domain")?,
        field("subdomain")?
    );
    let instance = d
        .get("qmi_instance_id")
        .and_then(Value::as_i64)
        .and_then(|i| u32::try_from(i).ok())
        .ok_or("sr_domain has no qmi_instance_id")?;
    let services = root
        .get("sr_service")
        .and_then(Value::as_array)
        .ok_or("no sr_service list")?;
    services
        .iter()
        .map(|s| {
            let provider = s.get("provider").and_then(Value::as_str);
            let service = s.get("service").and_then(Value::as_str);
            match (provider, service) {
                (Some(p), Some(n)) => Ok(PdMap {
                    service: format!("{p}/{n}"),
                    domain: domain.clone(),
                    instance,
                }),
                _ => Err("an sr_service entry has no provider or service".to_string()),
            }
        })
        .collect()
}

/// The GET_DOMAIN_LIST response: result, total, database revision 1, and the matching domains from `offset` on (the
/// list TLV only when it is not empty).
pub fn domain_list_response(txn: u16, maps: &[PdMap], name: &str, offset: u32) -> Vec<u8> {
    let matching: Vec<&PdMap> = maps.iter().filter(|m| m.service == name).collect();
    let mut b = Builder::new();
    b.result(qmi::RESULT_SUCCESS, 0)
        .u16(TLV_TOTAL, matching.len().min(usize::from(u16::MAX)) as u16)
        .u16(TLV_REVISION, 1);
    let mut v = vec![0u8];
    let mut count = 0u8;
    for m in matching
        .iter()
        .skip(usize::try_from(offset).unwrap_or(usize::MAX))
        .take(LIST_MAX)
    {
        if v.len() > LIST_BYTES_MAX {
            break;
        }
        qmi::put_inner_string(&mut v, &m.domain);
        v.extend_from_slice(&m.instance.to_le_bytes());
        // service_data_valid and service_data: pd-mapper leaves them zero
        v.push(0);
        v.extend_from_slice(&0u32.to_le_bytes());
        count += 1;
    }
    if count != 0 {
        v[0] = count;
        b.tlv(TLV_LIST, &v);
    }
    b.finish(qmi::RESPONSE, txn, GET_DOMAIN_LIST)
}

fn malformed_response(txn: u16, msg_id: u16) -> Vec<u8> {
    let mut b = Builder::new();
    b.result(qmi::RESULT_FAILURE, ERR_MALFORMED);
    b.finish(qmi::RESPONSE, txn, msg_id)
}

/// A client's GET_DOMAIN_LIST request (the C fixtures check it; tests are a client).
pub fn domain_list_request(txn: u16, name: &str, offset: Option<u32>) -> Vec<u8> {
    let mut b = Builder::new();
    b.tlv(TLV_NAME, name.as_bytes());
    if let Some(o) = offset {
        b.u32(TLV_OFFSET, o);
    }
    b.finish(qmi::REQUEST, txn, GET_DOMAIN_LIST)
}

/// A decoded GET_DOMAIN_LIST response.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DomainList {
    pub result: (u16, u16),
    pub total: Option<u16>,
    pub revision: Option<u16>,
    /// (domain, instance)
    pub domains: Vec<(String, u32)>,
}

pub fn parse_domain_list_response(buf: &[u8]) -> Result<DomainList, qmi::Error> {
    let m = qmi::parse(buf)?;
    let mut domains = Vec::new();
    if let Some(v) = m.get(TLV_LIST) {
        let bad = qmi::Error::BadValue(TLV_LIST);
        let (&n, mut p) = v.split_first().ok_or(bad.clone())?;
        for _ in 0..n {
            let (&len, rest) = p.split_first().ok_or(bad.clone())?;
            let len = usize::from(len);
            if rest.len() < len + 9 {
                return Err(bad);
            }
            let name = String::from_utf8_lossy(&rest[..len]).into_owned();
            let i = &rest[len..len + 4];
            domains.push((name, u32::from_le_bytes([i[0], i[1], i[2], i[3]])));
            p = &rest[len + 9..];
        }
        if !p.is_empty() {
            return Err(bad);
        }
    }
    Ok(DomainList {
        result: m.result()?,
        total: m.u16(TLV_TOTAL)?,
        revision: m.u16(TLV_REVISION)?,
        domains,
    })
}

/// pd-mapper: its maps, and the answers to one datagram at a time.
#[derive(Debug, Default)]
pub struct PdMapper {
    maps: Vec<PdMap>,
}

impl PdMapper {
    pub fn from_maps(maps: Vec<PdMap>) -> PdMapper {
        PdMapper { maps }
    }

    /// Read `names` through `files`, with one message per file that failed or is missing.
    pub fn load<F: Files>(files: &mut F, names: &[&str], log: &mut Log) -> PdMapper {
        let mut maps = Vec::new();
        for name in names {
            match read_all(files, name).and_then(|t| parse_jsn(&t)) {
                Ok(m) => maps.extend(m),
                Err(e) => log.push(&format!("pd-mapper: {name}: {e}")),
            }
        }
        PdMapper { maps }
    }

    pub fn maps(&self) -> &[PdMap] {
        &self.maps
    }

    /// Answer one request, as pd-mapper's main loop does.
    pub fn on_message(
        &mut self,
        src_node: u32,
        src_port: u32,
        payload: &[u8],
        log: &mut Log,
    ) -> Vec<Reply> {
        let Ok(m) = qmi::parse(payload) else {
            return Vec::new();
        };
        let h = m.header;
        let reply = |payload| {
            vec![Reply {
                dst_node: src_node,
                dst_port: src_port,
                payload,
            }]
        };
        match h.msg_id {
            GET_DOMAIN_LIST => {
                if h.kind != qmi::REQUEST {
                    return reply(malformed_response(h.txn, GET_DOMAIN_LIST));
                }
                let req = m
                    .check_known(&[TLV_NAME])
                    .and_then(|()| Ok((m.need_string(TLV_NAME, NAME_MAX)?, m.u32(TLV_OFFSET)?)));
                reply(match req {
                    Ok((name, offset)) => {
                        domain_list_response(h.txn, &self.maps, &name, offset.unwrap_or(0))
                    }
                    Err(_) => malformed_response(h.txn, GET_DOMAIN_LIST),
                })
            }
            PFR => {
                // a protection-domain restart request: pd-mapper only logs these
                let text = |t| m.string(t, NAME_MAX).ok().flatten().unwrap_or_default();
                log.push(&format!("pd-mapper: pfr: {} ({})", text(1), text(2)));
                Vec::new()
            }
            _ => Vec::new(),
        }
    }
}

fn read_all<F: Files>(files: &mut F, name: &str) -> Result<String, String> {
    let len = files.file_len(name).ok_or("no such file")?;
    if len > JSN_MAX_LEN {
        return Err("too long".to_string());
    }
    let mut buf = vec![0u8; len as usize];
    let mut n = 0;
    while n < buf.len() {
        let got = files
            .read_at(name, n as u64, &mut buf[n..])
            .map_err(|_| "read failed".to_string())?;
        if got == 0 {
            break;
        }
        n += got.min(buf.len() - n);
    }
    buf.truncate(n);
    String::from_utf8(buf).map_err(|_| "not UTF-8".to_string())
}
