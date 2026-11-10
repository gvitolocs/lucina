extern crate std;

use std::{cell::RefCell, rc::Rc, vec, vec::Vec};

use crate::{
    Error,
    card::{self, Cid, ext_csd},
    host::{BOUNDARY, Config, DataSegment, Host, Probe, SECTOR, WriteWindow, allowed, fat_partition, next_chunk},
    model::{DMA_AREA, MEM_SIZE, Model, SECTORS, Shared, State, pattern},
    regs::*,
};

const BASE_HZ: u32 = 198_000_000;

type TestHost = Host<Shared, Shared, Shared>;

fn start_with(model: Model, max_hz: u32) -> (Result<TestHost, Error>, Shared) {
    let shared = Shared(Rc::new(RefCell::new(model)));
    let host = Host::start(
        shared.clone(),
        shared.clone(),
        shared.clone(),
        Config { base_hz: BASE_HZ, max_hz },
    );
    (host, shared)
}

fn start(model: Model) -> (TestHost, Shared) {
    let (host, shared) = start_with(model, 52_000_000);
    (host.unwrap(), shared)
}

fn no_violations(shared: &Shared) {
    assert_eq!(shared.0.borrow().violations, Vec::<&str>::new());
}

fn commands(shared: &Shared) -> Vec<u8> {
    shared.0.borrow().commands.iter().map(|c| c.0).collect()
}

#[test]
fn clock_fields_are_amazons() {
    // pre_div 32, div 16.
    assert_eq!(clock_fields(BASE_HZ, 400_000), ((16 << 8) | (15 << 4), 386_718));
    // pre_div 2, div 2.
    assert_eq!(clock_fields(BASE_HZ, 52_000_000), ((1 << 8) | (1 << 4), 49_500_000));
    assert_eq!(clock_fields(BASE_HZ, 26_000_000), ((1 << 8) | (3 << 4), 24_750_000));
    // Never above what was asked, down to the slowest it has.
    for target in [100_000, 400_000, 1_000_000, 20_000_000, 26_000_000, 52_000_000, 198_000_000] {
        assert!(clock_fields(BASE_HZ, target).1 <= target);
    }
}

#[test]
fn nothing_that_writes_is_allowed() {
    for cmd in [
        4, 5, 10, 11, 14, 15, 16, 19, 20, 21, 23, 24, 25, 26, 27, 28, 29, 30, 31, 35, 36, 38, 39, 40, 42, 44, 45, 46, 47,
        48, 49, 53, 54, 55, 56, 60, 61, 62, 63,
    ] {
        assert!(!allowed(cmd, 0), "CMD{cmd}");
        assert!(!allowed(cmd, 0x0300_0001), "CMD{cmd}");
    }
    // Pre-idle, and the card streaming its boot partition.
    assert!(allowed(0, 0));
    assert!(!allowed(0, 0xf0f0_f0f0));
    assert!(!allowed(0, 0xffff_fffa));
}

fn switch_arg(access: u32, index: u32, value: u32, set: u32) -> u32 {
    (access << 24) | (index << 16) | (value << 8) | set
}

#[test]
fn cmd6_can_only_set_the_two_bytes_it_needs() {
    assert!(allowed(6, switch_arg(3, 183, 2, 1)));
    assert!(allowed(6, switch_arg(3, 183, 0, 1)));
    assert!(allowed(6, switch_arg(3, 185, 1, 1)));
    // Double data rate, HS200 and HS400 are not used.
    for value in [5, 6, 3] {
        assert!(!allowed(6, switch_arg(3, 183, value, 1)));
    }
    assert!(!allowed(6, switch_arg(3, 185, 2, 1)));
    // Setting or clearing bits instead of writing the byte, and another command set.
    assert!(!allowed(6, switch_arg(1, 183, 2, 1)));
    assert!(!allowed(6, switch_arg(2, 183, 2, 1)));
    assert!(!allowed(6, switch_arg(0, 183, 2, 1)));
    assert!(!allowed(6, switch_arg(3, 183, 2, 0)));
    // Every other byte: PARTITION_CONFIG, the fuses, cache, all of them.
    for index in 0..=255u32 {
        if index != 183 && index != 185 {
            for value in 0..=255u32 {
                assert!(!allowed(6, switch_arg(3, index, value, 1)), "byte {index}");
            }
        }
    }
}

#[test]
fn every_command_is_allowed_only_as_it_is_used() {
    let (mut host, shared) = start(Model::new());
    let mut data = vec![DataSegment { daddr: 0x20_0000, len: 1024 }];
    host.read(5, &data).unwrap();
    data[0].len = 512;
    host.read(6, &data).unwrap();
    for (cmd, arg) in &shared.0.borrow().commands {
        assert!(allowed(*cmd, *arg), "CMD{cmd} {arg:#x}");
    }
    no_violations(&shared);
}

#[test]
fn brings_up_a_card_at_eight_bits_and_high_speed() {
    let (host, shared) = start(Model::new());
    let card = *host.card();
    assert_eq!(card.sectors, SECTORS as u64);
    assert_eq!(card.bus_bits, 8);
    assert!(card.high_speed);
    assert_eq!(card.clock_hz, 49_500_000);
    assert_eq!(card.ext_csd_rev, 6);
    assert_eq!(card.boot_kib, 2048);
    assert_eq!(card.rpmb_kib, 512);
    let model = shared.0.borrow();
    assert_eq!(model.state, State::Tran);
    assert_eq!((model.card_width, model.hs_timing), (2, 1));
    assert_eq!(model.host_width(), 2);
    assert_eq!(model.full_resets, 1);
    assert_eq!(model.regs[WTMK_LVL / 4], WTMK_VALUE);
    assert_eq!(model.regs[INT_SIGNAL_EN / 4], 0);
    assert_eq!(model.regs[INT_STATUS_EN / 4], INT_USED);
    assert_eq!(model.regs[SYS_CTRL / 4] & SYS_CLOCKS_ON, SYS_CLOCKS_ON);
    assert_eq!(model.regs[SYS_CTRL / 4] & SYS_DTOCV_MASK, 0xe << 16);
    assert_ne!(model.regs[VEND_SPEC / 4] & VEND_FORCE_CLOCK_ON, 0);
    drop(model);
    assert_eq!(
        commands(&shared)[..6],
        [0, 1, 0, 1, 1, 1],
        "CMD0, what the card takes, CMD0, then until it is ready"
    );
    no_violations(&shared);
}

#[test]
fn the_cid_is_decoded() {
    // Bits 127-8 of a response, as the registers hold them.
    let bytes: [u8; 15] = [
        0x11, 0x01, 0x00, b'0', b'1', b'6', b'G', b'9', b'2', 0x30, 0x12, 0x34, 0x56, 0x78, 0xab,
    ];
    let mut raw = [0u32; 4];
    for (i, byte) in bytes.iter().enumerate() {
        let at = 112 - i * 8;
        raw[at / 32] |= (*byte as u32) << (at % 32);
    }
    let cid = Cid::parse(&raw);
    assert_eq!(cid.manufacturer, 0x11);
    assert_eq!(cid.name_str(), "016G92");
    assert_eq!(cid.serial, 0x1234_5678);
}

#[test]
fn the_speed_limit_is_kept() {
    let (host, _) = start_with(Model::new(), 20_000_000);
    let host = host.unwrap();
    assert!(host.card().clock_hz <= 20_000_000);
    assert!(host.card().high_speed);
}

#[test]
fn a_card_without_high_speed_stays_slow() {
    let mut model = Model::new();
    model.ext[ext_csd::CARD_TYPE] = 0;
    let (host, shared) = start(model);
    assert!(!host.card().high_speed);
    assert!(host.card().clock_hz <= 26_000_000);
    assert_eq!(shared.0.borrow().hs_timing, 0);
}

#[test]
fn falls_back_to_four_bits_then_one() {
    let mut model = Model::new();
    model.deny_8bit = true;
    let (host, shared) = start(model);
    assert_eq!(host.card().bus_bits, 4);
    assert_eq!(shared.0.borrow().card_width, 1);
    no_violations(&shared);

    let mut model = Model::new();
    model.deny_8bit = true;
    model.deny_4bit = true;
    let (host, shared) = start(model);
    assert_eq!(host.card().bus_bits, 1);
    assert_eq!(shared.0.borrow().card_width, 0);
    assert_eq!(shared.0.borrow().host_width(), 0);
    no_violations(&shared);
}

#[test]
fn a_wide_read_with_other_bytes_is_not_believed() {
    // Here the bus "works" but gives other bytes: compare what must be the same.
    let (a, mut b) = ([1u8; card::EXT_CSD_SIZE], [1u8; card::EXT_CSD_SIZE]);
    assert!(card::same_read_only_bytes(&a, &b));
    b[ext_csd::PARTITION_CONFIG] = 9;
    assert!(card::same_read_only_bytes(&a, &b), "a byte that can change");
    b[ext_csd::SEC_COUNT + 2] = 9;
    assert!(!card::same_read_only_bytes(&a, &b));
}

#[test]
fn another_partition_is_refused_with_nothing_sent_to_change_it() {
    for partition in 1..=3 {
        let mut model = Model::new();
        model.ext[ext_csd::PARTITION_CONFIG] = 0x48 | partition;
        let (host, shared) = start_with(model, 52_000_000);
        assert_eq!(host.err(), Some(Error::OtherPartition(partition)));
        assert!(!commands(&shared).contains(&6), "no CMD6");
        no_violations(&shared);
    }
    // What it boots from is no reason to refuse.
    let mut model = Model::new();
    model.ext[ext_csd::PARTITION_CONFIG] = 0x48;
    let (host, _) = start(model);
    assert_eq!(host.card().partition_config, 0x48);
}

#[test]
fn a_card_with_no_answer_is_an_error_and_not_a_hang() {
    let mut model = Model::new();
    model.silent = Some(1);
    let (host, shared) = start_with(model, 52_000_000);
    assert!(matches!(host.err(), Some(Error::Command { cmd: 1, .. })));
    assert_eq!(commands(&shared), [0, 1]);
    assert!(shared.0.borrow().line_resets > 0);
}

#[test]
fn a_card_that_stays_busy_gives_up() {
    let mut model = Model::new();
    model.never_ready = true;
    let (host, _) = start_with(model, 52_000_000);
    assert!(matches!(host.err(), Some(Error::Unusable(_))));
}

#[test]
fn no_controller_clock_is_an_error() {
    let shared = Shared(Rc::new(RefCell::new(Model::new())));
    let host = Host::start(
        shared.clone(),
        shared.clone(),
        shared.clone(),
        Config { base_hz: 0, max_hz: 52_000_000 },
    );
    assert!(matches!(host.err(), Some(Error::Unusable(_))));
}

#[test]
fn the_probe_writes_nothing() {
    let shared = Shared(Rc::new(RefCell::new(Model::new())));
    let before = shared.0.borrow().regs;
    let probe = Probe::read(&shared);
    assert_eq!(shared.0.borrow().regs, before);
    assert_eq!(probe.sys, 0x0080_800f);
    assert!(shared.0.borrow().commands.is_empty());
}

fn sector_at(shared: &Shared, daddr: u32) -> [u8; SECTOR] {
    let mut sector = [0u8; SECTOR];
    let at = daddr as usize;
    sector.copy_from_slice(&shared.0.borrow().mem[at..at + SECTOR]);
    sector
}

fn expect_sector(shared: &Shared, daddr: u32, lba: u32) {
    let mut want = [0u8; SECTOR];
    pattern(lba, &mut want);
    assert_eq!(sector_at(shared, daddr), want, "at {daddr:#x}");
}

#[test]
fn reads_one_sector() {
    let (mut host, shared) = start(Model::new());
    host.read(1234, &[DataSegment { daddr: 0x20_0000, len: 512 }]).unwrap();
    expect_sector(&shared, 0x20_0000, 1234);
    assert_eq!(*shared.0.borrow().commands.last().unwrap(), (17, 1234));
    no_violations(&shared);
}

#[test]
fn reads_many_and_stops_the_card() {
    let (mut host, shared) = start(Model::new());
    host.read(100, &[DataSegment { daddr: 0x20_0000, len: 4096 }]).unwrap();
    for i in 0..8 {
        expect_sector(&shared, 0x20_0000 + i * 512, 100 + i);
    }
    let commands = commands(&shared);
    assert_eq!(commands[commands.len() - 2..], [18, 12]);
    assert_eq!(shared.0.borrow().state, State::Tran);
    no_violations(&shared);
}

#[test]
fn adjacent_segments_are_one_command_and_others_are_not() {
    let (mut host, shared) = start(Model::new());
    let before = commands(&shared).len();
    host.read(
        10,
        &[
            DataSegment { daddr: 0x20_0000, len: 4096 },
            DataSegment { daddr: 0x20_1000, len: 4096 },
            DataSegment { daddr: 0x20_4000, len: 4096 },
        ],
    )
    .unwrap();
    let commands = commands(&shared);
    assert_eq!(commands[before..], [18, 12, 18, 12]);
    expect_sector(&shared, 0x20_1000, 18);
    expect_sector(&shared, 0x20_4000, 26);
    no_violations(&shared);
}

#[test]
fn no_transfer_crosses_a_boundary() {
    let (mut host, shared) = start(Model::new());
    let daddr = (2 * BOUNDARY - 4096) as u32;
    host.read(10, &[DataSegment { daddr, len: 8192 }]).unwrap();
    expect_sector(&shared, daddr, 10);
    expect_sector(&shared, 2 * BOUNDARY as u32, 18);
    assert_eq!(
        shared.0.borrow().transfers[shared.0.borrow().transfers.len() - 2..],
        [(daddr, 4096), (2 * BOUNDARY as u32, 4096)]
    );
    no_violations(&shared);
}

#[test]
fn a_chunk_is_found_across_segments_and_boundaries() {
    let boundary = BOUNDARY as u32;
    // A boundary inside the second of two adjacent segments.
    let segments = [
        DataSegment { daddr: boundary - 8192, len: 4096 },
        DataSegment { daddr: boundary - 4096, len: 8192 },
    ];
    let (daddr, len, at) = next_chunk(&segments, (0, 0)).unwrap();
    assert_eq!((daddr, len, at), (boundary - 8192, 8192, (1, 4096)));
    let (daddr, len, at) = next_chunk(&segments, at).unwrap();
    assert_eq!((daddr, len, at), (boundary, 4096, (2, 0)));
}

#[test]
fn bad_buffers_and_ranges_are_refused_before_anything_is_sent() {
    let (mut host, shared) = start(Model::new());
    let before = commands(&shared).len();
    let at = |daddr, len| [DataSegment { daddr, len }];
    assert_eq!(host.read(0, &at(0x20_0000, 100)), Err(Error::BadBuffer));
    assert_eq!(host.read(0, &[]), Err(Error::BadBuffer));
    assert_eq!(host.read(0, &at(0x20_0002, 512)), Err(Error::BadBuffer));
    assert_eq!(host.read(SECTORS as u64, &at(0x20_0000, 512)), Err(Error::OutOfRange));
    assert_eq!(host.read(SECTORS as u64 - 1, &at(0x20_0000, 1024)), Err(Error::OutOfRange));
    assert_eq!(host.read(u64::MAX, &at(0x20_0000, 512)), Err(Error::OutOfRange));
    assert_eq!(commands(&shared).len(), before);
    host.read(SECTORS as u64 - 1, &at(0x20_0000, 512)).unwrap();
    expect_sector(&shared, 0x20_0000, SECTORS - 1);
    no_violations(&shared);
}

#[test]
fn a_failed_read_is_tried_again_after_stopping_the_card() {
    let (mut host, shared) = start(Model::new());
    shared.0.borrow_mut().data_errors = 1;
    let resets = shared.0.borrow().line_resets;
    host.read(100, &[DataSegment { daddr: 0x20_0000, len: 4096 }]).unwrap();
    expect_sector(&shared, 0x20_0000, 100);
    let commands = commands(&shared);
    assert_eq!(commands[commands.len() - 5..], [18, 12, 13, 18, 12]);
    assert_eq!(shared.0.borrow().line_resets, resets + 1);
    // The reset of the lines did not cost the bus width.
    assert_eq!(shared.0.borrow().host_width(), 2);
}

#[test]
fn a_read_that_keeps_failing_is_an_error_and_the_next_can_work() {
    let (mut host, shared) = start(Model::new());
    let width = shared.0.borrow().host_width();
    assert_eq!(width, 2);
    shared.0.borrow_mut().data_errors = 2;
    let result = host.read(100, &[DataSegment { daddr: 0x20_0000, len: 512 }]);
    assert!(matches!(result, Err(Error::Data { cmd: 17, .. })), "{result:?}");
    assert_eq!(shared.0.borrow().state, State::Tran);
    assert_eq!(shared.0.borrow().host_width(), 2, "the bus width came back after the reset");
    host.read(100, &[DataSegment { daddr: 0x20_0000, len: 512 }]).unwrap();
    expect_sector(&shared, 0x20_0000, 100);
    no_violations(&shared);
}

#[test]
fn the_ext_csd_goes_to_the_drivers_memory() {
    let (_, shared) = start(Model::new());
    let model = shared.0.borrow();
    assert!(model.transfers.iter().all(|(at, len)| *at == DMA_AREA && *len == 512));
    assert_eq!(model.mem[DMA_AREA as usize + ext_csd::REV], 6);
    assert!((DMA_AREA as usize) < MEM_SIZE);
}

#[test]
fn a_card_still_on_a_wide_bus_is_told_otherwise() {
    let mut model = Model::new();
    model.cmd0_keeps_bus = true;
    model.card_width = 2;
    model.hs_timing = 1;
    let (host, shared) = start(model);
    assert_eq!(host.card().bus_bits, 8);
    let commands = commands(&shared);
    let first_read = commands.iter().position(|c| *c == 8).unwrap();
    assert_eq!(commands[first_read + 1], 6, "told to go back to one bit");
    no_violations(&shared);
}

#[test]
fn the_reset_line_is_never_touched_even_on_errors() {
    let (mut host, shared) = start(Model::new());
    shared.0.borrow_mut().data_errors = 5;
    let _ = host.read(100, &[DataSegment { daddr: 0x20_0000, len: 4096 }]);
    assert_ne!(shared.0.borrow().regs[SYS_CTRL / 4] & SYS_IPP_RST_N, 0);
    no_violations(&shared);
}

const WINDOW: WriteWindow = WriteWindow { start: 1000, end: 2000 };
const WRITE_AT: u32 = 0x20_0000;

fn fill_write_buffer(shared: &Shared, bytes: usize) {
    let mut model = shared.0.borrow_mut();
    for (i, b) in model.mem[WRITE_AT as usize..][..bytes].iter_mut().enumerate() {
        *b = (i / 512) as u8 + 1;
    }
}

fn one_segment(len: usize) -> [DataSegment; 1] {
    [DataSegment { daddr: WRITE_AT, len }]
}

#[test]
fn nothing_writes_on_the_read_only_list() {
    assert!(!allowed(24, 1000));
    assert!(!allowed(25, 1000));
    // Cache, erase and the rest.
    for cmd in [23, 26, 27, 28, 29, 35, 36, 38, 42, 56] {
        assert!(!allowed(cmd, 0), "CMD{cmd}");
    }
}

#[test]
fn no_window_no_write() {
    let (mut host, shared) = start(Model::new());
    fill_write_buffer(&shared, 512);
    assert_eq!(host.write(1000, &one_segment(512)), Err(Error::Refused { cmd: 25, arg: 1000 }));
    let model = shared.0.borrow();
    assert!(model.written.is_empty());
    assert!(!model.commands.iter().any(|c| c.0 == 24 || c.0 == 25));
}

#[test]
fn a_write_inside_the_window_gets_there() {
    let (mut host, shared) = start(Model::new());
    host.allow_writes(WINDOW).unwrap();
    fill_write_buffer(&shared, 512 * 3);
    shared.0.borrow_mut().programming_polls = 2;
    host.write(1000, &one_segment(512)).unwrap();
    assert_eq!(*commands(&shared).last().unwrap(), 13);
    shared.0.borrow_mut().commands.clear();
    host.write(1997, &one_segment(512 * 3)).unwrap();
    assert_eq!(commands(&shared), vec![25, 12, 13]);
    let model = shared.0.borrow();
    assert_eq!(model.written.len(), 4);
    assert_eq!(model.written[0].0, 1000);
    assert_eq!(model.written[3].0, 1999);
    assert_eq!(model.written[3].1, vec![3u8; 512]);
    assert_eq!(model.programming_polls, 0);
    drop(model);
    no_violations(&shared);
}

#[test]
fn a_write_outside_the_window_never_reaches_the_card() {
    let (mut host, shared) = start(Model::new());
    host.allow_writes(WINDOW).unwrap();
    fill_write_buffer(&shared, 512 * 2);
    for lba in [0, 999, 1999, 2000, u32::MAX as u64] {
        let len = if lba == 1999 || lba == 999 { 1024 } else { 512 };
        assert!(matches!(host.write(lba, &one_segment(len)), Err(Error::Refused { .. })), "{lba}");
    }
    let model = shared.0.borrow();
    assert!(model.written.is_empty());
    assert!(!model.commands.iter().any(|c| c.0 == 24 || c.0 == 25));
}

#[test]
fn a_window_must_be_on_the_card() {
    let (mut host, _shared) = start(Model::new());
    let end = SECTORS as u64 + 1;
    assert_eq!(host.allow_writes(WriteWindow { start: 0, end }), Err(Error::OutOfRange));
    assert_eq!(host.allow_writes(WriteWindow { start: 5, end: 5 }), Err(Error::OutOfRange));
}

#[test]
fn the_cache_goes_off_before_any_write() {
    let mut model = Model::new();
    model.ext[ext_csd::CACHE_CTRL] = 1;
    let (mut host, shared) = start(model);
    assert!(host.card().cache_on);
    host.allow_writes(WINDOW).unwrap();
    assert!(!host.card().cache_on);
    let model = shared.0.borrow();
    assert_eq!(model.ext[ext_csd::CACHE_CTRL], 0);
    drop(model);
    no_violations(&shared);
}

#[test]
fn a_failed_write_is_tried_once_more() {
    let (mut host, shared) = start(Model::new());
    host.allow_writes(WINDOW).unwrap();
    fill_write_buffer(&shared, 1024);
    shared.0.borrow_mut().write_errors = 1;
    host.write(1000, &one_segment(1024)).unwrap();
    assert_eq!(shared.0.borrow().written.len(), 2);
    shared.0.borrow_mut().written.clear();
    shared.0.borrow_mut().write_errors = 2;
    assert!(host.write(1000, &one_segment(1024)).is_err());
    assert!(shared.0.borrow().written.is_empty());
}

#[test]
fn only_a_fat_partition_is_a_window() {
    let mut sector = [0u8; 512];
    sector[510] = 0x55;
    sector[511] = 0xaa;
    let mut entry = |n: usize, kind: u8, start: u32, count: u32| {
        let e = 446 + (n - 1) * 16;
        sector[e + 4] = kind;
        sector[e + 8..e + 12].copy_from_slice(&start.to_le_bytes());
        sector[e + 12..e + 16].copy_from_slice(&count.to_le_bytes());
    };
    entry(1, 0x83, 16384, 921600);
    entry(4, 0x0b, 0x13_1000, 0x61_7000);
    assert_eq!(fat_partition(&sector, 4), Some(WriteWindow { start: 0x13_1000, end: 0x13_1000 + 0x61_7000 }));
    assert_eq!(fat_partition(&sector, 1), None);
    assert_eq!(fat_partition(&sector, 2), None);
    assert_eq!(fat_partition(&sector, 0), None);
    assert_eq!(fat_partition(&sector, 5), None);
    sector[511] = 0;
    assert_eq!(fat_partition(&sector, 4), None);
}

#[test]
fn only_a_whole_update_to_main_may_be_queued() {
    use crate::host::{UPDATE_BYTES, update_goes_home};
    let mut block = vec![0u8; UPDATE_BYTES];
    block[0x1f0..0x200].copy_from_slice(b"abcdefghhgfedcba");
    block[0x1000..0x1004].copy_from_slice(b"main");
    assert!(update_goes_home(&block));
    assert!(!update_goes_home(&block[..UPDATE_BYTES - 1]));
    let mut other = block.clone();
    other[0x1000..0x1005].copy_from_slice(b"diags");
    assert!(!update_goes_home(&other));
    let mut longer = block.clone();
    longer[0x1004] = b'x';
    assert!(!update_goes_home(&longer));
    let mut no_magic = block;
    no_magic[0x1f0] = 0;
    assert!(!update_goes_home(&no_magic));
}

#[test]
fn a_pending_update_s_boot_mode_is_read_and_set_alone() {
    use crate::host::{UPDATE_BYTES, set_update_bootmode, update_bootmode};
    let mut block = vec![0x5au8; UPDATE_BYTES];
    block[0x1f0..0x200].copy_from_slice(b"abcdefghhgfedcba");
    block[0x1000..0x1010].fill(0);
    block[0x1000..0x1008].copy_from_slice(b"fastboot");
    assert_eq!(update_bootmode(&block), Some(&b"fastboot"[..]));
    let before = block.clone();
    assert!(set_update_bootmode(&mut block, b"main"));
    assert_eq!(update_bootmode(&block), Some(&b"main"[..]));
    let changed: Vec<usize> = (0..UPDATE_BYTES).filter(|&i| block[i] != before[i]).collect();
    assert!(changed.iter().all(|i| (0x1000..0x1010).contains(i)));
    assert!(!set_update_bootmode(&mut block, b"0123456789abcdef"));
    assert!(!set_update_bootmode(&mut block, b""));
    block[0x1f0] = 0;
    assert_eq!(update_bootmode(&block), None);
    assert!(!set_update_bootmode(&mut block, b"main"));
}
