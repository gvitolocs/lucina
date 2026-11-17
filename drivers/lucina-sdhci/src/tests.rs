use alloc::vec;

use crate::{
    Error,
    card::{self, Csd, Long, Scr},
    host::{Clock, Config, DataSegment, DmaArea, Host, Probe, Regs, Variant},
    model::{DMA_BASE, Model, SD_SECTOR, TestClock, TestDma},
    msm,
    regs::{self, *},
    soc,
};

const GENERIC_RW: Config = Config {
    variant: Variant::Generic,
    writable: true,
};
const QCOM_RO: Config = Config {
    variant: Variant::QcomV5 {
        core_hz: soc::CORE_CLOCK_HZ,
    },
    writable: false,
};

fn start(model: &Model, config: Config) -> Result<Host<Model, TestDma, TestClock>, Error> {
    Host::start(
        model.clone(),
        TestDma::new(model, DMA_BASE),
        TestClock::new(model),
        config,
    )
}

const SDHC_SECTORS: usize = 4 * 1024 * 1024;

#[test]
fn clock_dividers_match_the_two_register_formats() {
    // 3.00: base / 2N, 10 bits.
    assert_eq!(
        regs::clock_divider(100_000_000, 400_000, 2),
        (125 << 8, 400_000)
    );
    assert_eq!(
        regs::clock_divider(100_000_000, 50_000_000, 2),
        (1 << 8, 50_000_000)
    );
    assert_eq!(
        regs::clock_divider(100_000_000, 25_000_000, 2),
        (2 << 8, 25_000_000)
    );
    let (field, hz) = regs::clock_divider(400_000_000, 200_000, 2);
    assert_eq!(hz, 200_000);
    assert_eq!(field, (232 << 8) | (3 << 6));
    // Never above the target, and no divider when the base clock is slow enough.
    assert_eq!(
        regs::clock_divider(25_000_000, 50_000_000, 2),
        (0, 25_000_000)
    );
    // 2.00 (QEMU's default): powers of two.
    assert_eq!(
        regs::clock_divider(52_000_000, 400_000, 1),
        (128 << 8, 203_125)
    );
    assert_eq!(
        regs::clock_divider(52_000_000, 25_000_000, 1),
        (2 << 8, 13_000_000)
    );
}

#[test]
fn csd_scr_and_cid_decode_like_the_specifications() {
    // A real SDHC CSD: version 1, C_SIZE 0x1d9f (about 3.8 GiB)... here built field by field.
    let c_size = 0x1d9fu32;
    let raw: Long = [
        0x400e_0032,
        0x5b59_0000 | (c_size >> 16),
        (c_size << 16) | 0x7f80,
        0x0a40_0000,
    ];
    let csd = Csd::parse(&raw).unwrap();
    assert_eq!(csd.sectors, (c_size as u64 + 1) * 1024);
    assert!(csd.class10);
    assert!(!csd.write_protected);

    let mut wp = raw;
    wp[3] |= 1 << 12;
    assert!(Csd::parse(&wp).unwrap().write_protected);

    // SDSC: 1 GiB in 512-byte blocks with READ_BL_LEN 9.
    let (mult, c_size) = (7u32, 4095u32);
    let raw: Long = [
        0,
        (9 << 16) | (c_size >> 2),
        (c_size << 30) | (mult << 15),
        0,
    ];
    assert_eq!(Csd::parse(&raw).unwrap().sectors, 4096 << 9);

    // Version 3 (SDUC) is refused.
    assert_eq!(Csd::parse(&[2 << 30, 0, 0, 0]), None);

    assert_eq!(
        Scr::parse(&[0x02, 0x35, 0, 0, 0, 0, 0, 0]),
        Scr {
            spec: 2,
            bus_4bit: true
        }
    );
    assert_eq!(
        Scr::parse(&[0x00, 0x01, 0, 0, 0, 0, 0, 0]),
        Scr {
            spec: 0,
            bus_4bit: false
        }
    );
}

#[test]
fn sdhc_card_comes_up_with_4bit_and_high_speed() {
    let m = Model::new(false, SDHC_SECTORS, true);
    let host = start(&m, GENERIC_RW).unwrap();
    let c = *host.card();
    assert!(c.high_capacity);
    assert_eq!(c.sectors, SDHC_SECTORS as u64);
    assert_eq!(c.rca, 0xb368);
    assert!(c.bus_4bit && c.high_speed);
    assert_eq!(c.clock_hz, 50_000_000);
    assert!(!c.read_only);
    // The order `mmc_sd_init_card` uses, with no CMD16 on a block-addressed card.
    let cmds = m.commands();
    assert_eq!(&cmds[..2], &[0, 8]);
    let end = cmds.iter().position(|c| *c == 2).unwrap();
    assert!(cmds[2..end].chunks(2).all(|p| p == [55, 41]));
    assert_eq!(&cmds[end..end + 4], &[2, 3, 9, 7]);
    assert!(!cmds.contains(&16));
    let s = m.0.borrow();
    assert!(s.bus_4bit && s.high_speed);
    assert_eq!(
        s.regs[&HOST_CONTROL] & (CTRL_4BIT | CTRL_HISPD),
        CTRL_4BIT | CTRL_HISPD
    );
}

#[test]
fn old_card_without_cmd8_is_byte_addressed_and_gets_cmd16() {
    let m = Model::new(false, 4096 << 9, false);
    m.0.borrow_mut().v2 = false;
    let mut host = start(&m, GENERIC_RW).unwrap();
    assert!(!host.card().high_capacity);
    assert!(m.commands().contains(&16));
    // ACMD41 without HCS.
    let acmd41 = m.0.borrow().commands.iter().find(|c| c.0 == 41).unwrap().1;
    assert_eq!(acmd41 & card::OCR_HCS, 0);

    m.0.borrow_mut().store[3 * SD_SECTOR] = 0xa5;
    let seg = DataSegment {
        daddr: 0x9000_0000,
        len: SD_SECTOR,
    };
    host.read(3, 1, &[seg]).unwrap();
    let last = *m.0.borrow().commands.last().unwrap();
    assert_eq!(last, (17, 3 * SD_SECTOR as u32));
    assert_eq!(m.0.borrow().mem[&0x9000_0000], 0xa5);
}

#[test]
fn reads_and_writes_go_through_the_table_across_segments() {
    let m = Model::new(false, SDHC_SECTORS, true);
    let mut host = start(&m, GENERIC_RW).unwrap();
    {
        let mut s = m.0.borrow_mut();
        for i in 0..4 * SD_SECTOR {
            s.store[100 * SD_SECTOR + i] = i as u8 ^ 0x5a;
        }
    }
    // Two segments, the second above 4 GiB, as the laptop's memory is.
    let high = 0x1_2000_0000u64;
    let segs = [
        DataSegment {
            daddr: 0x9000_0000,
            len: 1024,
        },
        DataSegment {
            daddr: high,
            len: 1024,
        },
    ];
    host.read(100, 4, &segs).unwrap();
    assert_eq!(m.0.borrow().commands.last().unwrap().0, 18);
    assert_eq!(m.0.borrow().mem[&0x9000_0000], 0x5a);
    assert_eq!(m.0.borrow().mem[&(high + 1)], (1025usize as u8) ^ 0x5a);

    m.0.borrow_mut().mem.insert(0x9100_0000, 0x77);
    host.write(
        7,
        1,
        &[DataSegment {
            daddr: 0x9100_0000,
            len: SD_SECTOR,
        }],
    )
    .unwrap();
    assert_eq!(m.0.borrow().store[7 * SD_SECTOR], 0x77);
    assert_eq!(m.commands().last(), Some(&13));
}

#[test]
fn long_segment_splits_into_lines() {
    let m = Model::new(false, SDHC_SECTORS, true);
    let mut host = start(&m, GENERIC_RW).unwrap();
    let len = 128 * 1024;
    host.read(
        0,
        (len / SD_SECTOR) as u32,
        &[DataSegment {
            daddr: 0x9000_0000,
            len,
        }],
    )
    .unwrap();
    let table_at = DMA_BASE;
    let s = m.0.borrow();
    let attr = |line: u64| s.mem[&(table_at + line * 12)];
    // Four 32 KiB lines, the end flag on the last alone.
    assert_eq!(attr(0) & ADMA_ATTR_END as u8, 0);
    assert_eq!(attr(2) & ADMA_ATTR_END as u8, 0);
    assert_eq!(attr(3) & ADMA_ATTR_END as u8, ADMA_ATTR_END as u8);
}

#[test]
fn thirty_two_bit_controller_refuses_memory_above_4gib() {
    let m = Model::new(false, SDHC_SECTORS, true);
    m.0.borrow_mut().caps &= !CAN_64BIT;
    let mut host = start(&m, GENERIC_RW).unwrap();
    let before = m.commands().len();
    assert_eq!(
        host.read(
            0,
            1,
            &[DataSegment {
                daddr: 1 << 32,
                len: SD_SECTOR
            }]
        ),
        Err(Error::BadBuffer)
    );
    assert_eq!(m.commands().len(), before);
    // Below 4 GiB it works, on eight-byte lines.
    host.read(
        0,
        1,
        &[DataSegment {
            daddr: 0x9000_0000,
            len: SD_SECTOR,
        }],
    )
    .unwrap();
    assert_eq!(
        m.0.borrow().regs[&HOST_CONTROL] & CTRL_DMA_MASK,
        CTRL_ADMA32
    );
}

#[test]
fn a_read_only_host_never_sends_a_write_command() {
    let m = Model::new(false, SDHC_SECTORS, true);
    let mut host = start(
        &m,
        Config {
            writable: false,
            ..GENERIC_RW
        },
    )
    .unwrap();
    assert!(host.card().read_only);
    let seg = DataSegment {
        daddr: 0x9000_0000,
        len: SD_SECTOR,
    };
    assert_eq!(host.write(0, 1, &[seg]), Err(Error::ReadOnly));
    assert_eq!(
        host.write(0, 2, &[DataSegment { len: 1024, ..seg }]),
        Err(Error::ReadOnly)
    );
    host.read(0, 1, &[seg]).unwrap();
    assert!(!m.commands().iter().any(|c| matches!(c, 24 | 25)));
}

#[test]
fn the_write_switch_and_the_csd_also_stop_writes() {
    let m = Model::new(false, SDHC_SECTORS, true);
    m.0.borrow_mut().write_switch_on = false;
    assert!(start(&m, GENERIC_RW).unwrap().card().read_only);

    let m = Model::new(false, SDHC_SECTORS, true);
    m.0.borrow_mut().csd[3] |= 1 << 12;
    assert!(start(&m, GENERIC_RW).unwrap().card().read_only);
}

#[test]
fn requests_that_do_not_add_up_are_refused_before_the_card_sees_them() {
    let m = Model::new(false, SDHC_SECTORS, true);
    let mut host = start(&m, GENERIC_RW).unwrap();
    let before = m.commands().len();
    let seg = DataSegment {
        daddr: 0x9000_0000,
        len: SD_SECTOR,
    };
    assert_eq!(host.read(0, 2, &[seg]), Err(Error::BadBuffer));
    assert_eq!(host.read(0, 0, &[]), Err(Error::BadBuffer));
    assert_eq!(
        host.read(SDHC_SECTORS as u64, 1, &[seg]),
        Err(Error::OutOfRange)
    );
    assert_eq!(host.read(u64::MAX, 1, &[seg]), Err(Error::OutOfRange));
    assert_eq!(
        host.read(
            0,
            1,
            &[DataSegment {
                daddr: 0x9000_0001,
                ..seg
            }]
        ),
        Err(Error::BadBuffer)
    );
    assert_eq!(m.commands().len(), before);
}

#[test]
fn a_failed_transfer_resets_the_lines_and_the_next_one_works() {
    let m = Model::new(false, SDHC_SECTORS, true);
    let mut host = start(&m, GENERIC_RW).unwrap();
    m.0.borrow_mut().fail_data_of = Some(17);
    let seg = DataSegment {
        daddr: 0x9000_0000,
        len: SD_SECTOR,
    };
    assert!(matches!(
        host.read(0, 1, &[seg]),
        Err(Error::Data { cmd: 17, .. })
    ));
    let resets =
        m.0.borrow()
            .writes
            .iter()
            .filter(|w| *w == &(SOFTWARE_RESET, (RESET_CMD | RESET_DATA) as u64))
            .count();
    assert!(resets >= 1);
    host.read(0, 1, &[seg]).unwrap();
}

#[test]
fn write_waits_until_the_card_has_programmed() {
    let m = Model::new(false, SDHC_SECTORS, true);
    let mut host = start(&m, GENERIC_RW).unwrap();
    m.0.borrow_mut().card_busy_polls = 3;
    host.write(
        0,
        1,
        &[DataSegment {
            daddr: 0x9000_0000,
            len: SD_SECTOR,
        }],
    )
    .unwrap();
    assert_eq!(
        m.commands().iter().rev().take_while(|c| **c == 13).count(),
        4
    );
}

#[test]
fn no_card_and_no_controller_are_reported() {
    let m = Model::new(false, SDHC_SECTORS, true);
    m.0.borrow_mut().card_inserted = false;
    assert_eq!(start(&m, GENERIC_RW).err(), Some(Error::NoCard));
    assert!(m.commands().is_empty());

    let m = Model::new(false, SDHC_SECTORS, true);
    m.0.borrow_mut().version = u16::MAX;
    m.0.borrow_mut().caps = u32::MAX;
    assert_eq!(start(&m, GENERIC_RW).err(), Some(Error::NoController));
    assert!(m.0.borrow().writes.is_empty());
}

#[test]
fn a_bad_scr_costs_speed_not_the_card() {
    let m = Model::new(false, SDHC_SECTORS, true);
    m.0.borrow_mut().fail_scr = true;
    let c = *start(&m, GENERIC_RW).unwrap().card();
    assert!(!c.bus_4bit && !c.high_speed);
    assert_eq!(c.clock_hz, 25_000_000);
}

#[test]
fn probe_writes_nothing() {
    for (qcom, variant) in [
        (false, Variant::Generic),
        (
            true,
            Variant::QcomV5 {
                core_hz: soc::CORE_CLOCK_HZ,
            },
        ),
    ] {
        let m = Model::new(qcom, SDHC_SECTORS, true);
        let p = Probe::read(&m, variant);
        assert_eq!(p.spec(), 2);
        assert_eq!(p.mci_version != 0, qcom);
        assert!(m.0.borrow().writes.is_empty());
    }
}

#[test]
fn qcom_answers_power_requests_while_resetting() {
    let m = Model::new(true, SDHC_SECTORS, true);
    m.0.borrow_mut().reset_waits_for_pwr = true;
    m.0.borrow_mut().pwr_status = msm::PWRCTL_BUS_ON; // left by the firmware
    let host = start(&m, QCOM_RO).unwrap();
    assert!(!host.notes().reset_all_stuck);
    assert_eq!(
        host.notes().pwr_requests & msm::PWRCTL_BUS_OFF,
        msm::PWRCTL_BUS_OFF
    );
    let s = m.0.borrow();
    assert_eq!(s.reset_all_count, 1);
    assert_eq!(s.pwr_status, 0);
    // BUS_ON or BUS_OFF: both successes, as `sdhci_msm_handle_pwr_irq` writes them.
    assert!(
        s.pwr_ctl_writes
            .iter()
            .all(|a| *a == (msm::PWRCTL_ACK_BUS_SUCCESS | msm::PWRCTL_ACK_IO_SUCCESS))
    );
    // The interrupt stays masked, the vendor register is back at its power-on value.
    assert_eq!(s.r32(msm::CORE_PWRCTL_MASK), 0);
    assert_eq!(s.r32(msm::CORE_VENDOR_SPEC), msm::CORE_VENDOR_SPEC_POR_VAL);
    // The DLL is off at this speed.
    let dll = s.r32(msm::CORE_DLL_CONFIG);
    assert_eq!(
        dll & (msm::CORE_DLL_RST | msm::CORE_DLL_PDN),
        msm::CORE_DLL_RST | msm::CORE_DLL_PDN
    );
}

#[test]
fn qcom_powers_the_bus_at_3v_in_one_write_and_answers_bus_on() {
    let m = Model::new(true, SDHC_SECTORS, true);
    let host = start(&m, QCOM_RO).unwrap();
    assert_eq!(m.0.borrow().power_written, vec![POWER_300 | POWER_ON]);
    assert_eq!(
        host.notes().pwr_requests & msm::PWRCTL_BUS_ON,
        msm::PWRCTL_BUS_ON
    );
    // The card clock comes from the fixed core clock.
    assert_eq!(host.card().clock_hz, 50_000_000);
    let clock = u16::from_le_bytes([
        m.0.borrow().regs[&CLOCK_CONTROL],
        m.0.borrow().regs[&(CLOCK_CONTROL + 1)],
    ]);
    assert_eq!(clock & 0xff00, 1 << 8);
}

#[test]
fn qcom_falls_back_to_a_line_reset_when_reset_all_never_finishes() {
    let m = Model::new(true, SDHC_SECTORS, true);
    m.0.borrow_mut().reset_all_stuck = true;
    // RESET_ALL stays set for good; the line-reset bits are what the fallback waits on, and they clear.
    let host = start(&m, QCOM_RO).unwrap();
    assert!(host.notes().reset_all_stuck);
    assert!(
        m.0.borrow()
            .writes
            .contains(&(SOFTWARE_RESET, (RESET_CMD | RESET_DATA) as u64))
    );
    assert!(host.card().sectors > 0);
}

#[test]
fn table_and_small_buffers_are_synced() {
    let m = Model::new(false, SDHC_SECTORS, true);
    let dma = TestDma::new(&m, DMA_BASE);
    let (to, from) = (dma.to_device.clone(), dma.from_device.clone());
    let mut host = Host::start(m.clone(), dma, TestClock::new(&m), GENERIC_RW).unwrap();
    // SCR and two CMD6: one table sync plus one buffer sync each way in, one from-device out.
    assert_eq!(from.get(), 3);
    assert_eq!(to.get(), 6);
    host.read(
        0,
        1,
        &[DataSegment {
            daddr: 0x9000_0000,
            len: SD_SECTOR,
        }],
    )
    .unwrap();
    assert_eq!(to.get(), 7);
    let _ = host.notes();
}

#[test]
fn soc_clocks_reads_change_nothing_and_enable_sets_100_mhz() {
    let gcc = Model::new(false, 1024, true);
    gcc.write32(0x008, 1 << 31);
    gcc.write32(0x004, 1 << 31);
    let writes = gcc.0.borrow().writes.len();
    let c = soc::Clocks::read(&gcc);
    assert!(!c.running());
    assert_eq!(gcc.0.borrow().writes.len(), writes);

    // Running once CLK_OFF has cleared.
    gcc.write32(0x008, 1);
    gcc.write32(0x004, 1);
    assert!(soc::Clocks::read(&gcc).running());

    let gpll0 = Model::new(false, 1024, true);
    let clock = TestClock::new(&gcc);
    assert_eq!(
        soc::enable_clocks(&gcc, &gpll0, &clock),
        Err(Error::Unusable("GPLL0 is not running"))
    );
    gpll0.write32(0, 1 << 31);
    // The model does not clear the update bit, so this times out; the writes before that are what counts.
    assert_eq!(
        soc::enable_clocks(&gcc, &gpll0, &clock),
        Err(Error::Timeout("the SDCC2 clocks"))
    );
    assert_eq!(gcc.read32(0x010), (6 << 8) | 5);
    assert_eq!(gcc.read32(0x00c) & 1, 1);
    assert_eq!(gcc.read32(0x008) & 1, 1);
    assert_eq!(gcc.read32(0x004) & 1, 1);
}

#[test]
fn soc_pins_follow_the_device_tree() {
    let tlmm = Model::new(false, 1024, true);
    tlmm.write32(0, 0xffff_ffff);
    soc::configure_pins(&tlmm);
    let v = tlmm.read32(0);
    assert_eq!((v >> 14) & 3, 0, "clock: no pull");
    assert_eq!((v >> 6) & 7, 7, "clock: 16 mA");
    assert_eq!((v >> 11) & 3, 3, "command: pull-up");
    assert_eq!((v >> 3) & 7, 4, "command: 10 mA");
    assert_eq!((v >> 9) & 3, 3, "data: pull-up");
    assert_eq!(v & 7, 4, "data: 10 mA");
    assert_eq!(v >> 16, 0xffff, "others left alone");
}

#[test]
fn soc_card_detect_honours_the_polarity() {
    let gpio = Model::new(false, 1024, true);
    gpio.write32(0, 0x3ff);
    soc::configure_card_detect(&gpio);
    assert_eq!(gpio.read32(0), 3, "input, function 0, 2 mA, pull-up");

    gpio.write32(4, 0);
    assert!(soc::CardDetect::read(&gpio, true).present);
    assert!(!soc::CardDetect::read(&gpio, false).present);
    gpio.write32(4, 1);
    assert!(!soc::CardDetect::read(&gpio, true).present);
    assert!(soc::CardDetect::read(&gpio, false).present);
}

#[test]
fn clock_helper_does_not_stall() {
    let c = TestClock::new(&Model::new(false, 1024, true));
    let a = c.now_us();
    c.delay_us(10);
    assert!(c.now_us() > a + 10);
    let d = TestDma::new(&Model::new(false, 1024, true), 0x1000);
    d.write(0, &[1, 2]);
    let mut b = [0; 2];
    d.read(0, &mut b);
    assert_eq!(b, [1, 2]);
}

/// Unsafe audit round 2, S-1: a data-line reset that never finishes leaves no evidence the ADMA engine stopped, so the
/// host sends nothing more and tells the caller to keep the buffers.
#[test]
fn a_data_reset_that_never_finishes_wedges_the_host() {
    let m = Model::new(false, SDHC_SECTORS, true);
    let mut host = start(&m, GENERIC_RW).unwrap();
    {
        let mut s = m.0.borrow_mut();
        s.fail_data_of = Some(17);
        s.lines_reset_stuck = true;
    }
    let seg = DataSegment {
        daddr: 0x9000_0000,
        len: SD_SECTOR,
    };
    assert!(matches!(
        host.read(0, 1, &[seg]),
        Err(Error::Data { cmd: 17, .. })
    ));
    assert!(host.wedged());
    let before = m.commands().len();
    assert_eq!(host.read(0, 1, &[seg]), Err(Error::Wedged));
    assert_eq!(m.commands().len(), before, "nothing reaches the controller");
}

#[test]
fn a_data_reset_that_finishes_leaves_the_host_usable() {
    let m = Model::new(false, SDHC_SECTORS, true);
    let mut host = start(&m, GENERIC_RW).unwrap();
    m.0.borrow_mut().fail_data_of = Some(17);
    let seg = DataSegment {
        daddr: 0x9000_0000,
        len: SD_SECTOR,
    };
    assert!(host.read(0, 1, &[seg]).is_err());
    assert!(!host.wedged());
    assert_eq!(host.read(0, 1, &[seg]), Ok(()));
}

#[test]
fn a_hung_transfer_with_a_stuck_reset_wedges_the_host() {
    let m = Model::new(false, SDHC_SECTORS, true);
    let mut host = start(&m, GENERIC_RW).unwrap();
    {
        let mut s = m.0.borrow_mut();
        s.hang_data_of = Some(18);
        s.lines_reset_stuck = true;
    }
    let seg = DataSegment {
        daddr: 0x9000_0000,
        len: 2 * SD_SECTOR,
    };
    assert_eq!(
        host.read(0, 2, &[seg]),
        Err(Error::Timeout("a data transfer"))
    );
    assert!(host.wedged());
}

/// S-3: the kexec hook's reset, with and without a controller that finishes it.
#[test]
fn quiesce_resets_the_lines_and_reports_a_stuck_reset() {
    let m = Model::new(true, SDHC_SECTORS, true);
    let t = core::cell::Cell::new(0u64);
    let clock = || {
        t.set(t.get() + 10);
        t.get()
    };
    assert!(crate::quiesce(&m, clock));
    let writes = m.0.borrow().writes.clone();
    assert!(writes.contains(&(SOFTWARE_RESET, (RESET_CMD | RESET_DATA) as u64)));
    assert!(writes.contains(&(SIGNAL_ENABLE, 0)));
    m.0.borrow_mut().lines_reset_stuck = true;
    assert!(!crate::quiesce(&m, clock));
}

/// A response the controller latches late: with its interrupt, the host sleeps for it instead of backing off, and the
/// data is the same.
#[test]
fn a_slow_response_is_waited_on_the_interrupt() {
    let m = Model::new(false, SDHC_SECTORS, true);
    let mut host = start(&m, GENERIC_RW).unwrap();
    {
        let mut s = m.0.borrow_mut();
        // The response comes 40 `INT_STATUS` reads late: past the first 20 quick looks.
        s.rsp_lag = 40;
        s.irq = true;
        s.store[0] = 0xa5;
    }
    host.read(
        0,
        1,
        &[DataSegment {
            daddr: 0x9000_0000,
            len: SD_SECTOR,
        }],
    )
    .unwrap();
    assert!(
        m.0.borrow().irq_waits > 0,
        "the host never waited for the interrupt"
    );
    assert_eq!(m.0.borrow().mem[&0x9000_0000], 0xa5);
}

// ---------------------------------------------------------- patch 1356: the slot's way down and the card's way back

/// One page of the GCC. The two branch registers answer a cleared enable bit with `CLK_OFF`, which is how
/// `clk_branch2_check_halt` reads them (`drivers/clk/qcom/clk-branch.c:44-56`).
#[derive(Clone)]
struct GccPage {
    regs: alloc::rc::Rc<core::cell::RefCell<[u32; 8]>>,
}

impl GccPage {
    /// GPLL0's page: only `GPLL0_MODE` at offset 0, with the lock bit telling whether the PLL runs.
    fn gpll0(locked: bool) -> Self {
        Self {
            regs: alloc::rc::Rc::new(core::cell::RefCell::new([
                if locked { 1 << 31 } else { 0 },
                0,
                0,
                0,
                0,
                0,
                0,
                0,
            ])),
        }
    }

    fn with(apps: u32, ahb: u32) -> Self {
        Self {
            regs: alloc::rc::Rc::new(core::cell::RefCell::new([0, apps, ahb, 0, 0, 0, 0, 0])),
        }
    }

    fn raw(&self, off: usize) -> u32 {
        self.regs.borrow()[off / 4]
    }
}

impl Regs for GccPage {
    fn read32(&self, offset: usize) -> u32 {
        let v = self.regs.borrow()[offset / 4];
        match offset / 4 {
            // `APPS_CBCR` at index 1, `AHB_CBCR` at index 2: enabled reads running, gated reads off.
            1 | 2 => {
                if v & 1 == 0 {
                    v | (1 << 31)
                } else {
                    v & !(1 << 31)
                }
            }
            // `RCG_CMD` (index 3): hardware takes the update and clears `CMD_UPDATE` itself.
            3 => 0,
            _ => v,
        }
    }

    fn write32(&self, offset: usize, value: u32) {
        self.regs.borrow_mut()[offset / 4] = value;
    }

    fn read16(&self, offset: usize) -> u16 {
        (self.read32(offset) & 0xffff) as u16
    }

    fn read8(&self, offset: usize) -> u8 {
        (self.read32(offset) & 0xff) as u8
    }

    fn write16(&self, offset: usize, value: u16) {
        self.write32(offset, value as u32);
    }

    fn write8(&self, offset: usize, value: u8) {
        self.write32(offset, value as u32);
    }
}

/// A clock that does not pass: the two helpers count their own looks, and this one says the update is done at once.
struct NoWait;

impl Clock for NoWait {
    fn now_us(&self) -> u64 {
        0
    }

    fn delay_us(&self, _us: u64) {}
}

#[test]
fn a_sleep_gates_both_sdcc2_branches_and_leaves_the_rcg_alone() {
    let gcc = GccPage::with(1, 1);
    let after = soc::disable_clocks(&gcc);
    assert!(!after.running(), "both branches read gated");
    assert_eq!(gcc.raw(0x004) & 1, 0, "core (apps) gated");
    assert_eq!(gcc.raw(0x008) & 1, 0, "iface (ahb) gated");
    // The RCG is not touched: Linux's suspend gates the branches, and the rate comes back with the OPP on resume.
    assert_eq!(
        gcc.raw(0x010),
        0,
        "the RCG configuration is left as bring-up put it"
    );
}

/// A page of the GCC whose two branches keep answering as enabled and never show `CLK_OFF`, however often they are
/// written: hardware that does not stop the clock. Counts its own reads, which is how the test below sees the poll
/// run out.
#[derive(Default)]
struct NeverHalts {
    reads: core::cell::Cell<usize>,
}

impl Regs for NeverHalts {
    fn read32(&self, offset: usize) -> u32 {
        match offset {
            // `APPS_CBCR` 0x004 and `AHB_CBCR` 0x008: bit 0 set, so `CLK_OFF` (bit 31) never appears.
            0x004 | 0x008 => {
                self.reads.set(self.reads.get() + 1);
                1
            }
            _ => 0,
        }
    }

    fn write32(&self, _offset: usize, _value: u32) {}

    fn read16(&self, offset: usize) -> u16 {
        (self.read32(offset) & 0xffff) as u16
    }

    fn read8(&self, offset: usize) -> u8 {
        (self.read32(offset) & 0xff) as u8
    }

    fn write16(&self, offset: usize, value: u16) {
        self.write32(offset, value as u32);
    }

    fn write8(&self, offset: usize, value: u8) {
        self.write32(offset, value as u32);
    }
}

#[test]
fn a_disable_waits_for_clk_off_and_a_branch_that_never_shows_it_only_costs_the_poll() {
    // Both SDCC2 branches declare `BRANCH_HALT` (`gcc-sc7180.c:1767`, `:1780`), the type `clk_branch_wait` polls in
    // both directions (`drivers/clk/qcom/clk-branch.c:77-79`), so a disable looks for `CLK_OFF` up to the
    // `count = 200` of `:80`. A branch that never answers is the `-EBUSY` of `:87-89`, which
    // `clk_branch2_disable` ignores (`:133-136`): the loop bounds itself, the write is not retried, and the caller
    // gets the CBCR as it reads.
    let gcc = NeverHalts::default();
    let after = soc::disable_clocks(&gcc);
    assert!(
        after.running(),
        "the read-back is what the branch says, so the hook can say it"
    );
    assert_eq!(
        gcc.reads.get(),
        2 * 2 + 2 * soc::HALT_CHECKS,
        "one read per write, then the bounded poll of each branch, and nothing after that"
    );
}

#[test]
fn the_wake_votes_the_branches_back_before_it_reports_running() {
    let gcc = GccPage::with(1, 1);
    soc::disable_clocks(&gcc);
    let gpll0 = GccPage::gpll0(true);
    soc::restore_clocks(&gcc, &gpll0, &NoWait).expect("the branches come back");
    assert!(soc::Clocks::read(&gcc).running());
    assert_eq!(
        gcc.raw(0x010),
        (6 << 8) | 5,
        "the 100 MHz configuration is set again"
    );
    // A GPLL0 that is not locked is refused without touching the branches, as bring-up's helper does.
    let gcc = GccPage::with(0, 0);
    assert!(soc::restore_clocks(&gcc, &GccPage::gpll0(false), &NoWait).is_err());
    assert_eq!(gcc.raw(0x004), 0);
}

#[test]
fn deselecting_the_card_is_cmd7_with_no_argument_and_no_response() {
    let m = Model::new(true, SDHC_SECTORS, true);
    let mut host = start(&m, QCOM_RO).unwrap();
    let before = m.commands().len();
    host.deselect_card().unwrap();
    let cmds = m.commands();
    assert_eq!(cmds.len(), before + 1, "one command, nothing else");
    assert_eq!(
        cmds.last(),
        Some(&7),
        "MMC_SELECT_CARD with a zero argument"
    );
    assert!(!m.0.borrow().selected, "and the card is no longer selected");
}

#[test]
fn the_card_is_initialised_again_after_the_sleep() {
    // Linux's `_mmc_sd_resume` is `mmc_power_up` and `mmc_sd_init_card` (`sd.c:1737-1753`); the rail write of the
    // power-up is not ours, so the re-init is what this driver can do, and it is the whole sequence again.
    let m = Model::new(true, SDHC_SECTORS, true);
    let mut host = start(&m, QCOM_RO).unwrap();
    let before = m.commands();
    host.resume_card().unwrap();
    let after = m.commands();
    let sectors = host.card().sectors;
    let fresh = &after[before.len()..];
    assert_eq!(
        fresh.first(),
        Some(&0),
        "the fresh sequence starts at idle state"
    );
    assert!(fresh.contains(&7), "and selects the card again on its way");
    assert_eq!(
        host.card().sectors,
        sectors,
        "the same card as before the sleep"
    );
    assert_eq!(
        host.card().sectors,
        sectors,
        "the same card as before the sleep"
    );
}
