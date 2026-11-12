// SPDX-License-Identifier: GPL-2.0-only
#![allow(clippy::manual_is_multiple_of)]

use super::*;
use Op::*;

const MIB: u64 = 1 << 20;
const SIZE: u64 = 8 * MIB;

fn pool() -> Budget {
    Budget {
        used: 0,
        limit: 64 * MIB,
    }
}

fn fresh() -> (Buf, Budget) {
    new(SIZE, pool()).unwrap()
}

fn run(mut s: (Buf, Budget), ops: &[Op]) -> (Buf, Budget) {
    for &op in ops {
        s = step(s.0, s.1, op).unwrap_or_else(|e| panic!("{op:?} refused: {e:?}"));
    }
    s
}

#[test]
fn a_new_buffer_is_charged_once() {
    let (b, bud) = fresh();
    assert_eq!((b.phase, b.refs, b.fds), (Phase::Live, 1, 1));
    assert_eq!(bud.used, SIZE);
    assert_eq!(new(0, pool()), Err(Refusal::BadSize));
    assert_eq!(new(65 * MIB, pool()), Err(Refusal::NoRoom));
}

#[test]
fn the_owner_dies_while_the_compositor_holds_it() {
    // The app opens, hands a duplicate to the compositor, and dies: its descriptor closes, the compositor's stays.
    let s = run(fresh(), &[Open, Close]);
    assert_eq!(s.0.phase, Phase::Live);
    assert_eq!(s.0.refs, 1);
    assert_eq!(s.1.used, SIZE);
    let s = run(s, &[Close]);
    assert_eq!(s.0.phase, Phase::Freed);
    assert_eq!(s.1.used, 0);
}

#[test]
fn a_mapping_keeps_the_buffer_after_the_last_descriptor() {
    let s = run(fresh(), &[Map, Close]);
    assert_eq!((s.0.phase, s.0.refs, s.0.fds), (Phase::Live, 1, 0));
    assert_eq!(step(s.0, s.1, Close), Err(Refusal::NoDescriptor));
    let s = run(s, &[Unmap]);
    assert_eq!(s.0.phase, Phase::Freed);
    assert_eq!(s.1.used, 0);
}

#[test]
fn close_with_no_descriptor_is_refused() {
    let s = run(fresh(), &[Map, Close]);
    assert_eq!(step(s.0, s.1, Close), Err(Refusal::NoDescriptor));
    let s = run(fresh(), &[Close]);
    assert_eq!(step(s.0, s.1, Close), Err(Refusal::Dead));
}

#[test]
fn a_running_batch_keeps_the_buffer_alive() {
    let s = run(fresh(), &[GpuBind, BatchStart(1), Close, GpuUnbind]);
    assert_eq!(s.0.refs, 0);
    assert_eq!(s.0.gpu_pending, 1);
    assert_eq!(
        s.0.phase,
        Phase::Live,
        "freed only when the fence has passed"
    );
    assert_eq!(s.1.used, SIZE);
    let s = run(s, &[BatchRetire(1)]);
    assert_eq!(s.0.phase, Phase::Freed);
    assert_eq!(s.1.used, 0);
}

#[test]
fn a_retire_must_match_its_batches() {
    let s = run(fresh(), &[GpuBind, BatchStart(1)]);
    assert_eq!(step(s.0, s.1, BatchRetire(2)), Err(Refusal::BadBatch));
    // The only batch retiring while a slot use stays named by none is refused.
    assert_eq!(step(s.0, s.1, BatchRetire(0)), Err(Refusal::BadBatch));
    let s2 = run(s, &[BatchStart(2)]);
    let s3 = run(s2, &[BatchRetire(2)]);
    assert_eq!((s3.0.gpu_pending, s3.0.gpu_slots_in_batch), (1, 1));
    assert_eq!(
        step(fresh().0, fresh().1, BatchRetire(0)),
        Err(Refusal::NoBatch)
    );
}

#[test]
fn purge_returns_the_bytes_and_free_does_not_return_them_again() {
    let s = run(fresh(), &[SetPurgeable(true), Purge]);
    assert_eq!(s.0.phase, Phase::Purged);
    assert_eq!(s.1.used, 0);
    let s = run(s, &[Close]);
    assert_eq!(s.0.phase, Phase::Freed);
    assert_eq!(s.1.used, 0, "reduced once");
}

#[test]
fn the_pool_outside_the_buffer_is_not_touched_by_purge_and_free() {
    // Another buffer holds 20 MiB; after this one's purge and free the pool still holds exactly that.
    let mut bud = pool();
    bud.used = 20 * MIB;
    let s = run(new(SIZE, bud).unwrap(), &[SetPurgeable(true), Purge, Close]);
    assert_eq!(s.1.used, 20 * MIB);
}

#[test]
fn purge_is_refused_while_mapped() {
    let s = run(fresh(), &[SetPurgeable(true), Map]);
    assert_eq!(step(s.0, s.1, Purge), Err(Refusal::PurgeDenied));
    let s = run(s, &[Unmap, Purge]);
    assert_eq!(s.0.phase, Phase::Purged);
}

#[test]
fn purge_needs_purgeable_and_an_idle_gpu() {
    let s = fresh();
    assert_eq!(
        step(s.0, s.1, Purge),
        Err(Refusal::PurgeDenied),
        "not purgeable"
    );
    let s = run(s, &[SetPurgeable(true), GpuBind, BatchStart(1)]);
    assert_eq!(
        step(s.0, s.1, Purge),
        Err(Refusal::PurgeDenied),
        "in a running batch"
    );
    let s = run(s, &[BatchRetire(1), BatchStart(0)]);
    assert_eq!(
        step(s.0, s.1, Purge),
        Err(Refusal::PurgeDenied),
        "a batch with no slot still names it"
    );
    // A bound slot stops it even when no batch runs: the GPU still names these bytes.
    let s = run(s, &[BatchRetire(0)]);
    assert_eq!(
        step(s.0, s.1, Purge),
        Err(Refusal::PurgeDenied),
        "one slot bound, no batch running"
    );
    let s = run(s, &[GpuUnbind, Purge]);
    assert_eq!(
        (s.0.phase, s.0.gpu_slots),
        (Phase::Purged, 0),
        "unbound, then purged"
    );
    assert_eq!(s.1.used, 0);
}

#[test]
fn purge_is_refused_after_a_gpu_failure() {
    // Purge would hand back the bytes that quarantine keeps for good.
    let s = run(fresh(), &[SetPurgeable(true), GpuFail]);
    assert_eq!(
        step(s.0, s.1, Purge),
        Err(Refusal::PurgeDenied),
        "the GPU is marked failed"
    );
    let s = run(s, &[Close]);
    assert_eq!(s.0.phase, Phase::Quarantined);
    assert_eq!(s.1.used, SIZE, "still charged, never returned");
}

#[test]
fn purge_before_a_gpu_failure_leaves_nothing_to_quarantine() {
    let s = run(fresh(), &[SetPurgeable(true), Purge]);
    assert_eq!(s.1.used, 0);
    let s = run(s, &[GpuFail]);
    assert_eq!(
        (s.0.phase, s.0.gpu_failed, s.1.used),
        (Phase::Purged, true, 0),
        "the failure is recorded on the purged buffer"
    );
    let s = run(s, &[Close]);
    assert_eq!(
        (s.0.phase, s.1.used),
        (Phase::Freed, 0),
        "no bytes to quarantine"
    );
}

#[test]
fn a_purged_buffer_cannot_be_mapped_bound_or_named() {
    let s = run(fresh(), &[SetPurgeable(true), Purge]);
    assert_eq!(step(s.0, s.1, Map), Err(Refusal::Purged));
    assert_eq!(step(s.0, s.1, GpuBind), Err(Refusal::Purged));
    assert_eq!(step(s.0, s.1, BatchStart(1)), Err(Refusal::Purged));
    assert_eq!(step(s.0, s.1, Purge), Err(Refusal::PurgeDenied));
    // Descriptors still work.
    let s = run(s, &[Open, Close]);
    assert_eq!(s.0.phase, Phase::Purged);
}

#[test]
fn restore_reports_contents_lost_and_charges_again() {
    let s = run(fresh(), &[SetPurgeable(true), Purge]);
    assert!(!s.0.contents_lost);
    let s = run(s, &[SetPurgeable(false)]);
    assert_eq!(s.0.phase, Phase::Live);
    assert!(s.0.contents_lost);
    assert!(!s.0.purgeable);
    assert_eq!(s.1.used, SIZE);
    // And it can be mapped again.
    let s = run(s, &[Map]);
    assert_eq!(s.0.mappings, 1);
}

#[test]
fn restore_without_room_is_refused_and_changes_nothing() {
    let s = run(fresh(), &[SetPurgeable(true), Purge]);
    // The pool filled up meanwhile.
    let full = Budget {
        used: s.1.limit - SIZE + 1,
        limit: s.1.limit,
    };
    assert_eq!(step(s.0, full, SetPurgeable(false)), Err(Refusal::NoRoom));
    let ok = Budget {
        used: s.1.limit - SIZE,
        limit: s.1.limit,
    };
    let r = step(s.0, ok, SetPurgeable(false)).unwrap();
    assert_eq!((r.0.phase, r.1.used), (Phase::Live, s.1.limit));
}

#[test]
fn a_purged_buffer_freed_by_the_last_close_returns_nothing() {
    let s = run(fresh(), &[Open, SetPurgeable(true), Purge, Close]);
    assert_eq!(s.0.phase, Phase::Purged);
    let s = run(s, &[Close]);
    assert_eq!(s.0.phase, Phase::Freed);
    assert_eq!(s.1.used, 0);
}

#[test]
fn after_a_gpu_failure_the_last_reference_quarantines() {
    let s = run(fresh(), &[GpuFail, Close]);
    assert_eq!(s.0.phase, Phase::Quarantined);
    assert_eq!(s.1.used, SIZE, "never returned to the pool");
    assert_eq!(step(s.0, s.1, Open), Err(Refusal::Dead));
    assert_eq!(step(s.0, s.1, Purge), Err(Refusal::Dead));
}

#[test]
fn quarantine_waits_for_the_batch_too() {
    let s = run(fresh(), &[GpuFail, BatchStart(0), Close]);
    assert_eq!(s.0.phase, Phase::Live);
    let s = run(s, &[BatchRetire(0)]);
    assert_eq!(s.0.phase, Phase::Quarantined);
    assert_eq!(s.1.used, SIZE);
}

#[test]
fn a_failed_gpu_does_not_quarantine_a_purged_buffer() {
    // Nothing is mapped by the GPU's memory any more: no bytes to keep.
    let s = run(fresh(), &[SetPurgeable(true), Purge, GpuFail, Close]);
    assert_eq!(s.0.phase, Phase::Freed);
    assert_eq!(s.1.used, 0);
}

#[test]
fn counters_do_not_overflow() {
    let (mut b, bud) = fresh();
    b.fds = u32::MAX;
    b.refs = u32::MAX;
    assert_eq!(step(b, bud, Open), Err(Refusal::Overflow));
    assert_eq!(step(b, bud, Map), Err(Refusal::Overflow));
    assert_eq!(step(b, bud, GpuBind), Err(Refusal::Overflow));
    let (mut b, bud) = fresh();
    b.gpu_pending = u32::MAX;
    b.gpu_slots_in_batch = u32::MAX;
    assert_eq!(step(b, bud, BatchStart(0)), Err(Refusal::Overflow));
    let (mut b, bud) = fresh();
    b.gpu_pending = 1;
    b.gpu_slots_in_batch = u32::MAX;
    assert_eq!(step(b, bud, BatchStart(1)), Err(Refusal::Overflow));
}

#[test]
fn unmap_and_unbind_need_something_to_undo() {
    let s = fresh();
    assert_eq!(step(s.0, s.1, Unmap), Err(Refusal::NotMapped));
    assert_eq!(step(s.0, s.1, GpuUnbind), Err(Refusal::NotBound));
}

#[test]
fn set_purgeable_off_on_a_live_buffer_just_clears_the_flag() {
    let s = run(fresh(), &[SetPurgeable(true)]);
    assert!(s.0.purgeable);
    let s = run(s, &[SetPurgeable(false)]);
    assert!(!s.0.purgeable);
    assert!(!s.0.contents_lost);
    assert_eq!(s.1.used, SIZE);
}

#[test]
fn the_budget_never_overflows_or_underflows() {
    let b = Budget {
        used: u64::MAX - 5,
        limit: u64::MAX,
    };
    assert_eq!(charge(b, 6), Err(NoRoom));
    assert_eq!(
        charge(b, 5),
        Ok(Budget {
            used: u64::MAX,
            limit: u64::MAX
        })
    );
    assert_eq!(
        charge(
            Budget {
                used: 10,
                limit: 10
            },
            1
        ),
        Err(NoRoom)
    );
    assert_eq!(
        charge(
            Budget {
                used: 10,
                limit: 10
            },
            0
        ),
        Ok(Budget {
            used: 10,
            limit: 10
        })
    );
    assert_eq!(release(Budget { used: 3, limit: 10 }, 4), Err(Underflow));
    assert_eq!(
        release(Budget { used: 3, limit: 10 }, 3),
        Ok(Budget { used: 0, limit: 10 })
    );
}

#[test]
fn pitch_rounds_up_to_64() {
    assert_eq!(pitch(1), Some(64));
    assert_eq!(pitch(16), Some(64));
    assert_eq!(pitch(17), Some(128));
    assert_eq!(pitch(1920), Some(7680));
    assert_eq!(pitch(1366), Some(5504));
    assert_eq!(pitch(0x3fff), Some(65536));
    assert_eq!(pitch(0), None);
    assert_eq!(pitch(0x4000), None);
    assert_eq!(pitch(u32::MAX), None);
    for w in 1..0x4000u32 {
        let p = pitch(w).unwrap();
        assert!(p % 64 == 0 && p >= w * 4 && p < w * 4 + 64);
        assert_eq!(p, (w * 4).next_multiple_of(64));
    }
}

#[test]
fn buffer_bytes_is_page_aligned() {
    assert_eq!(buffer_bytes(7680, 1080), Some(8_294_400)); // 1080p XRGB, already a multiple of 4096
    assert_eq!(buffer_bytes(64, 1), Some(4096));
    assert_eq!(buffer_bytes(64, 65), Some(8192));
    assert_eq!(buffer_bytes(0, 10), None);
    assert_eq!(buffer_bytes(64, 0), None);
    assert_eq!(buffer_bytes(64, 0x4000), None);
    assert_eq!(buffer_bytes(MAX_PITCH + 1, 10), None);
    assert_eq!(buffer_bytes(u32::MAX, 0x3fff), None);
    let big = buffer_bytes(MAX_PITCH, 0x3fff).unwrap();
    assert!(big % 4096 == 0 && big >= MAX_PITCH as u64 * 0x3fff);
}

#[test]
fn surface_size_works_for_every_width_and_height_below_the_limit() {
    assert_eq!(surface_size(1920, 1080), Some((7680, 8_294_400)));
    assert_eq!(surface_size(0, 10), None);
    assert_eq!(surface_size(10, 0), None);
    assert_eq!(surface_size(0x4000, 10), None);
    assert_eq!(surface_size(10, 0x4000), None);
    for w in [1, 2, 63, 64, 65, 1366, 1920, 3840, 0x3ffe, 0x3fff] {
        for h in [1, 2, 7, 1080, 2160, 0x3ffe, 0x3fff] {
            let (p, b) = surface_size(w, h).unwrap();
            assert!(p % 64 == 0 && b % 4096 == 0 && b >= p as u64 * h as u64);
        }
    }
}

#[test]
fn sync_rows_stay_inside_the_buffer() {
    assert_eq!(rows_ok(&[], 100), Some(0));
    assert_eq!(rows_ok(&[(0, 100)], 100), Some(100));
    assert_eq!(rows_ok(&[(0, 10), (50, 5), (99, 1)], 100), Some(16));
    assert_eq!(rows_ok(&[(0, 101)], 100), None);
    assert_eq!(rows_ok(&[(100, 1)], 100), None);
    assert_eq!(rows_ok(&[(5, 0)], 100), None, "empty rectangle");
    assert_eq!(
        rows_ok(&[(u32::MAX, 2)], 100),
        None,
        "y + h would overflow u32"
    );
    assert_eq!(rows_ok(&[(1, u32::MAX)], u32::MAX), None);
    assert_eq!(
        rows_ok(&[(0, 10), (95, 6)], 100),
        None,
        "the second one is out"
    );
    assert_eq!(
        rows_ok(&[(0, u32::MAX), (0, u32::MAX)], u32::MAX),
        Some(2 * u32::MAX as u64)
    );
}
