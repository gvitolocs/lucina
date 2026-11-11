//! Whether the device keeps writes in a volatile cache, for the boot log: the Caching mode page of one unit and the
//! descriptor fields that describe caching and reliability. Everything here is read only: the driver never sends MODE
//! SELECT, never writes an attribute or flag, and never turns WriteBooster on.

use alloc::{format, string::String, vec::Vec};

use crate::{
    Error,
    desc::{DeviceDesc, UnitDesc},
    scsi::{Caching, ModeSense},
};

/// What the driver learnt about one unit's write cache.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CacheReport {
    pub lun: u8,
    /// MODE SENSE (10), page 08, current values.
    pub current: Result<ModeSense, Error>,
    /// The same page, changeable values; `None` when not asked (the current values already failed).
    pub changeable: Option<Result<ModeSense, Error>>,
    pub unit: UnitDesc,
    pub device: Option<DeviceDesc>,
}

fn bit(b: bool) -> u8 {
    b as u8
}

fn caching(m: &Result<ModeSense, Error>) -> Option<Caching> {
    m.as_ref().ok().and_then(|m| m.caching)
}

impl CacheReport {
    /// The volatile write cache is on: a flush (or FUA) is needed for a write to be durable. `None` if unknown.
    pub fn write_cache(&self) -> Option<bool> {
        caching(&self.current).map(|c| c.wce)
    }

    /// The log lines, each short enough for the 100-column screen after the kernel's "ufs: ufs0: " prefix (69
    /// columns). The first line is the summary; the others say why something is unknown, and the details.
    pub fn lines(&self) -> Vec<String> {
        let lun = self.lun;
        let wb = match self.device.and_then(|d| d.write_booster_supported()) {
            Some(true) => "yes",
            Some(false) => "no",
            None => "n/a",
        };
        let mut out = Vec::new();
        let cache = match (&self.current, caching(&self.current)) {
            (Ok(m), Some(c)) => format!(
                "WCE={} RCD={} DPOFUA={} (page 08)",
                bit(c.wce),
                bit(c.rcd),
                bit(m.dpofua)
            ),
            (Ok(m), None) => format!("WCE=? DPOFUA={} (no page 08)", bit(m.dpofua)),
            (Err(_), _) => String::from("WCE=? (MODE SENSE failed)"),
        };
        out.push(format!(
            "lun{lun} cache {cache}, bDataReliability={}, WB={wb}",
            self.unit.data_reliability
        ));
        if let Err(e) = &self.current {
            out.push(format!("lun{lun} MODE SENSE: {e}"));
        }
        match &self.changeable {
            Some(Ok(m)) => match m.caching {
                Some(c) => out.push(format!(
                    "lun{lun} page 08 changeable: WCE={} RCD={}",
                    bit(c.wce),
                    bit(c.rcd)
                )),
                None => out.push(format!("lun{lun} page 08 changeable: not returned")),
            },
            Some(Err(e)) => out.push(format!("lun{lun} changeable: {e}")),
            None => {}
        }
        let ext = match self.device.and_then(|d| d.ext_features) {
            Some(f) => format!("{f:#010x}"),
            None => String::from("n/a"),
        };
        let wb_type = match self.device.and_then(|d| d.wb_buffer_type) {
            Some(t) => format!("{t}"),
            None => String::from("n/a"),
        };
        out.push(format!(
            "lun{lun} bMemoryType {} bLogicalBlockSize {:#04x} ext {ext} WB type {wb_type}",
            self.unit.memory_type, self.unit.log2_block_size
        ));
        out
    }
}

#[cfg(test)]
mod tests {
    use alloc::{vec, vec::Vec};

    use crate::{
        Host, ResetPolicy, WritePolicy,
        desc::DeviceDesc,
        model::{Lu, Model},
        policy::UnitRole,
        policy::WritePolicy as Policy,
        scsi::{self, Caching, ModeSense},
        write_rule,
    };

    /// MODE SENSE (10) page 08 as a UFS device answers it with DBD 0: header (MODE DATA LENGTH 0x22, DPOFUA),
    /// one short-LBA block descriptor (blocks, 4096-byte length), then the 20-byte Caching page with WCE set.
    const WITH_BD: [u8; 36] = [
        0x00, 0x22, 0x00, 0x10, 0x00, 0x00, 0x00, 0x08, // header
        0x01, 0xdb, 0xef, 0xff, 0x00, 0x00, 0x10, 0x00, // block descriptor
        0x08, 0x12, 0x04, 0x00, 0xff, 0xff, 0x00, 0x00, 0xff, 0xff, 0xff, 0xff, 0x80, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x00, 0x00, // page 08
    ];

    #[test]
    fn caching_page_after_a_block_descriptor() {
        let m = ModeSense::parse(&WITH_BD).unwrap();
        assert_eq!(
            m,
            ModeSense {
                wp: false,
                dpofua: true,
                caching: Some(Caching {
                    wce: true,
                    rcd: false
                }),
            }
        );
        assert_eq!(ModeSense::total_len(&WITH_BD).unwrap(), 36);
    }

    #[test]
    fn caching_page_without_block_descriptors() {
        let mut d = vec![0x00, 0x1a, 0x00, 0x80, 0x00, 0x00, 0x00, 0x00];
        d.extend_from_slice(&[0x08, 0x12, 0x01]);
        d.extend_from_slice(&[0; 17]);
        let m = ModeSense::parse(&d).unwrap();
        assert!(m.wp && !m.dpofua);
        assert_eq!(
            m.caching,
            Some(Caching {
                wce: false,
                rcd: true
            })
        );
        // Bytes past MODE DATA LENGTH are not read: a page 08 there is not taken.
        let mut d = vec![0x00, 0x06, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00];
        d.extend_from_slice(&[0x08, 0x12, 0x04, 0x00]);
        assert_eq!(ModeSense::parse(&d).unwrap().caching, None);
    }

    #[test]
    fn caching_page_absent_or_in_subpage_format() {
        // Only the Control page (0x0a).
        let mut d = vec![0x00, 0x12, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00];
        d.extend_from_slice(&[0x0a, 0x0a, 0x04, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
        assert_eq!(ModeSense::parse(&d).unwrap().caching, None);
        // Header only.
        assert_eq!(
            ModeSense::parse(&[0x00, 0x06, 0, 0x10, 0, 0, 0, 0])
                .unwrap()
                .caching,
            None
        );
        // Page 08 subpage 01 (SPF) is skipped; the plain page 08 after it is read.
        let mut d = vec![0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00];
        d.extend_from_slice(&[0x48, 0x01, 0x00, 0x02, 0x04, 0x04]);
        d.extend_from_slice(&[0x08, 0x12, 0x00]);
        d.extend_from_slice(&[0; 17]);
        let mdl = (d.len() - 2) as u16;
        d[0..2].copy_from_slice(&mdl.to_be_bytes());
        assert_eq!(
            ModeSense::parse(&d).unwrap().caching,
            Some(Caching {
                wce: false,
                rcd: false
            })
        );
    }

    #[test]
    fn short_mode_sense_answers_are_rejected() {
        assert!(ModeSense::parse(&[]).is_err());
        assert!(ModeSense::parse(&WITH_BD[..7]).is_err());
        assert!(ModeSense::total_len(&WITH_BD[..4]).is_err());
        // MODE DATA LENGTH says less than a header.
        assert!(ModeSense::parse(&[0x00, 0x02, 0, 0, 0, 0, 0, 0]).is_err());
        // The block descriptors run past the data.
        assert!(ModeSense::parse(&WITH_BD[..12]).is_err());
        // Page 08's header arrived but not its flags byte.
        assert!(ModeSense::parse(&WITH_BD[..18]).is_err());
        // A page 08 that says it is empty.
        let d = [0x00, 0x08, 0, 0, 0, 0, 0, 0, 0x08, 0x00];
        assert!(ModeSense::parse(&d).is_err());
        // Cut after a byte that is a page code only: no page, no error.
        assert_eq!(ModeSense::parse(&WITH_BD[..17]).unwrap().caching, None);
    }

    #[test]
    fn mode_sense_cdb() {
        assert_eq!(
            scsi::mode_sense_10(scsi::MODE_PAGE_CACHING, scsi::MODE_PC_CHANGEABLE, 0x100),
            [0x5a, 0, 0x48, 0, 0, 0, 0, 0x01, 0x00, 0]
        );
    }

    fn device_desc(len: usize, ext: u32, wb_type: u8) -> Vec<u8> {
        let mut d = vec![0u8; len];
        d[0] = len as u8;
        if len >= 0x53 {
            d[0x4f..0x53].copy_from_slice(&ext.to_be_bytes());
        }
        if len > 0x54 {
            d[0x54] = wb_type;
        }
        d
    }

    #[test]
    fn write_booster_fields_of_the_device_descriptor() {
        let d = DeviceDesc::parse(&device_desc(0x59, 0x0000_0100, 1)).unwrap();
        assert_eq!(d.ext_features, Some(0x100));
        assert_eq!(d.write_booster_supported(), Some(true));
        assert_eq!(d.wb_buffer_type, Some(1));
        let d = DeviceDesc::parse(&device_desc(0x59, 0x0000_0001, 0)).unwrap();
        assert_eq!(d.write_booster_supported(), Some(false));
        // A UFS 2.1 descriptor (0x40 bytes) has no such fields; bLength wins over a longer buffer.
        let d = DeviceDesc::parse(&device_desc(0x40, 0, 0)).unwrap();
        assert_eq!((d.ext_features, d.wb_buffer_type), (None, None));
        assert_eq!(d.write_booster_supported(), None);
        let mut long = device_desc(0x59, 0x100, 1);
        long[0] = 0x40;
        assert_eq!(DeviceDesc::parse(&long).unwrap().ext_features, None);
    }

    fn lus() -> Vec<Lu> {
        let mut boot = Lu::new(1, 16, 12);
        boot.boot_lun_id = 1;
        vec![Lu::new(0, 256, 12), boot, Lu::new(3, 8, 12)]
    }

    #[test]
    fn the_report_reads_page_08_of_lun_0_and_nothing_else() {
        for policy in [WritePolicy::default(), WritePolicy::READ_ONLY] {
            let m = Model::cold(lus());
            let mut h =
                Host::start(m.regs(), m.area(), m.clock(), ResetPolicy::IfNeeded, policy).unwrap();
            let before = m.0.borrow().n.scsi.len();
            let writes_before = m.lu(0).data.clone();
            let r = h.cache_report(0).unwrap();
            assert_eq!(r.write_cache(), Some(true));
            assert!(!r.current.unwrap().caching.unwrap().rcd);
            assert!(r.current.unwrap().dpofua);
            assert_eq!(
                r.changeable.unwrap().unwrap().caching,
                Some(Caching {
                    wce: true,
                    rcd: false
                })
            );
            let sent: Vec<_> = m.0.borrow().n.scsi[before..].to_vec();
            assert_eq!(
                sent,
                vec![(0, scsi::MODE_SENSE_10), (0, scsi::MODE_SENSE_10)]
            );
            assert_eq!(m.lu(0).data, writes_before);
            let lines = r.lines();
            assert_eq!(
                lines[0],
                "lun0 cache WCE=1 RCD=0 DPOFUA=1 (page 08), bDataReliability=0, WB=no"
            );
            assert_eq!(lines[1], "lun0 page 08 changeable: WCE=1 RCD=0");
            assert_eq!(
                lines[2],
                "lun0 bMemoryType 0 bLogicalBlockSize 0x0c ext 0x00000000 WB type 0"
            );
            assert!(h.cache_report(5).is_none());
        }
    }

    #[test]
    fn a_failed_mode_sense_is_reported_not_fatal() {
        let mut l = lus();
        l[0].caching = None;
        let m = Model::cold(l);
        let mut h = Host::start(
            m.regs(),
            m.area(),
            m.clock(),
            ResetPolicy::IfNeeded,
            Policy::default(),
        )
        .unwrap();
        let r = h.cache_report(0).unwrap();
        assert_eq!(r.write_cache(), None);
        assert!(r.changeable.is_none());
        let lines = r.lines();
        assert_eq!(
            lines[0],
            "lun0 cache WCE=? (MODE SENSE failed), bDataReliability=0, WB=no"
        );
        assert_eq!(
            lines[1],
            "lun0 MODE SENSE: SCSI status 0x02, sense key 0x5 asc 0x20 ascq 0x00"
        );
        // The host still works.
        let seg = h.bounce(4096);
        h.read(0, 0, 1, &[seg]).unwrap();
        assert_eq!(h.units()[0].role, UnitRole::Main);
    }

    #[test]
    fn report_without_block_descriptor_and_lines_fit() {
        let mut l = lus();
        l[0].mode_block_descriptor = false;
        l[0].caching = Some((false, true));
        let m = Model::cold(l);
        let mut h = Host::start(
            m.regs(),
            m.area(),
            m.clock(),
            ResetPolicy::IfNeeded,
            Policy::default(),
        )
        .unwrap();
        let r = h.cache_report(0).unwrap();
        assert_eq!(r.write_cache(), Some(false));
        for line in r.lines() {
            assert!(line.len() <= 69, "{} columns: {line}", line.len());
        }
        // Every variant of the lines fits too, with the longest error and the WriteBooster fields present.
        let mut r2 = r;
        r2.device = Some(DeviceDesc::parse(&device_desc(0x59, 0xffff_ffff, 1)).unwrap());
        r2.unit.data_reliability = 1;
        r2.current = Err(crate::Error::Scsi {
            status: 0x02,
            sense: Some(scsi::Sense {
                key: 0xf,
                asc: 0xff,
                ascq: 0xff,
            }),
        });
        r2.changeable = Some(Err(crate::Error::BadData("MODE SENSE (10) caching page")));
        for line in r2.lines() {
            assert!(line.len() <= 69, "{} columns: {line}", line.len());
        }
    }

    /// MODE SENSE (10) only reads, so every unit may get it; nothing that sets a mode page or writes reaches any
    /// unit, writable or not.
    #[test]
    fn mode_sense_is_a_read_and_mode_select_is_never_allowed() {
        for writable in [false, true] {
            for lun in [0, 1, 3] {
                assert!(write_rule::command_allowed(
                    writable,
                    lun,
                    scsi::MODE_SENSE_10,
                    Some(0)
                ));
            }
            assert!(!write_rule::command_allowed(
                writable,
                0xd0,
                scsi::MODE_SENSE_10,
                Some(0)
            ));
            // MODE SELECT (6/10), MODE SENSE (6) (not needed), FORMAT UNIT, WRITE BUFFER, UNMAP, WRITE SAME (10/16),
            // SECURITY PROTOCOL OUT, START STOP UNIT, WRITE AND VERIFY, WRITE LONG.
            for op in [
                0x15, 0x55, 0x1a, 0x04, 0x3b, 0x42, 0x41, 0x93, 0xb5, 0x1b, 0x2e, 0x3f,
            ] {
                assert!(
                    !write_rule::command_allowed(writable, 0, op, Some(0)),
                    "opcode {op:#04x}"
                );
            }
        }
        // Read-only units still refuse every write-type command.
        for op in [scsi::WRITE_10, scsi::WRITE_16, scsi::SYNCHRONIZE_CACHE_10] {
            assert!(!write_rule::command_allowed(false, 0, op, Some(0)));
        }
    }
}
