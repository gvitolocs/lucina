use super::*;
use alloc::vec;
use alloc::vec::Vec;

/// One item comes back at the last CPU's quiescent point.
#[test]
fn retire_then_every_cpu_returns_it() {
    let mut monitor = Monitor::new();
    monitor.retire(1);
    assert_eq!(step(&mut monitor, 0, 0b11), vec![]);
    assert_eq!(step(&mut monitor, 1, 0b11), vec![1]);
    assert!(!monitor.is_monitoring());
}

/// An item retired while a grace period runs waits for the next one, a full round.
#[test]
fn retired_during_a_grace_period_needs_a_second_round() {
    let mut monitor = Monitor::new();
    monitor.retire(1);
    monitor.quiescent(0, 0b11);
    monitor.retire(2);
    assert_eq!(step(&mut monitor, 1, 0b11), vec![1]);
    assert!(monitor.is_monitoring());
    assert_eq!(step(&mut monitor, 1, 0b11), vec![]);
    assert_eq!(step(&mut monitor, 0, 0b11), vec![2]);
    assert!(!monitor.is_monitoring());
}

/// With nothing retired there is nothing to do.
#[test]
fn quiescent_when_not_monitoring_is_a_no_op() {
    let mut monitor = Monitor::new();
    assert_eq!(step(&mut monitor, 0, 1), vec![]);
    assert!(!monitor.is_monitoring());
    monitor.retire(7);
    assert_eq!(step(&mut monitor, 0, 1), vec![7]);
}

/// Eight CPUs: the first item starts a grace period alone, the second waits for the next one; each ends at its eighth CPU.
#[test]
fn eight_cpus() {
    let mut monitor = Monitor::new();
    monitor.retire(10);
    monitor.retire(11);
    for cpu in 0..7 {
        assert_eq!(step(&mut monitor, cpu, 0xff), vec![]);
    }
    assert_eq!(step(&mut monitor, 7, 0xff), vec![10]);
    assert!(monitor.is_monitoring());
    for cpu in (1..8).rev() {
        assert_eq!(step(&mut monitor, cpu, 0xff), vec![]);
    }
    assert_eq!(step(&mut monitor, 0, 0xff), vec![11]);
    assert!(!monitor.is_monitoring());
}

/// A CPU passing again in the same grace period does not stand in for another.
#[test]
fn a_cpu_passing_twice_counts_once() {
    let mut monitor = Monitor::new();
    monitor.retire(1);
    for _ in 0..2 {
        assert_eq!(step(&mut monitor, 0, 0b111), vec![]);
        assert_eq!(step(&mut monitor, 1, 0b111), vec![]);
    }
    assert_eq!(step(&mut monitor, 2, 0b111), vec![1]);
}

/// CPUs outside the mask, and numbers past 63, count for nothing.
#[test]
fn a_cpu_not_in_all_changes_nothing() {
    let mut monitor = Monitor::new();
    monitor.retire(1);
    assert_eq!(step(&mut monitor, 5, 0b11), vec![]);
    assert_eq!(step(&mut monitor, 64, 0b11), vec![]);
    assert_eq!(step(&mut monitor, 100, 0b11), vec![]);
    assert_eq!(step(&mut monitor, 0, 0b11), vec![]);
    assert_eq!(step(&mut monitor, 1, 0b11), vec![1]);
}

/// Everything retired comes back once, in order.
#[test]
fn items_come_back_in_order_once() {
    let mut monitor = Monitor::new();
    let mut collected = vec![];
    for i in 0..100 {
        monitor.retire(i);
        if i % 3 == 0 {
            collected.extend(step(&mut monitor, 0, 0b1));
        }
    }
    while monitor.is_monitoring() {
        collected.extend(step(&mut monitor, 0, 0b1));
    }
    assert_eq!(collected, (0..100).collect::<Vec<_>>());
}

/// One quiescent point, then everything it made ready, oldest first ([`Monitor::take_ready`] gives chunks newest first).
fn step<T>(monitor: &mut Monitor<T>, cpu: u32, all: u64) -> Vec<T> {
    monitor.quiescent(cpu, all);
    let mut chunks = Vec::new();
    loop {
        let chunk = monitor.take_ready();
        if chunk.is_empty() {
            break;
        }
        assert!(chunk.len() <= CHUNK);
        chunks.push(chunk);
    }
    chunks.into_iter().rev().flatten().collect()
}

/// A queue's items, oldest first: its chunks are taken newest first.
fn items<T>(mut q: Queue<T>) -> Vec<T> {
    let mut chunks = Vec::new();
    loop {
        let chunk = q.take_last();
        if chunk.is_empty() {
            break;
        }
        chunks.push(chunk);
    }
    chunks.into_iter().rev().flatten().collect()
}

/// Items pushed across many chunks come back in order.
#[test]
fn queue_keeps_order_across_chunks() {
    let mut q = Queue::new();
    for i in 0..1000u32 {
        q.push(i);
    }
    assert_eq!(items(q), (0..1000).collect::<Vec<_>>());
}

/// Taking never gives more than a chunk, and only an empty queue gives nothing.
#[test]
fn take_last_never_returns_more_than_a_chunk() {
    let mut q = Queue::new();
    assert!(q.is_empty());
    for i in 0..1000u32 {
        q.push(i);
    }
    assert!(!q.is_empty());
    let mut total = 0;
    loop {
        let chunk = q.take_last();
        assert!(chunk.len() <= CHUNK);
        if chunk.is_empty() {
            break;
        }
        total += chunk.len();
    }
    assert_eq!(total, 1000);
    assert!(q.is_empty());
}

/// With only full chunks, the newest comes first, then the oldest, then nothing.
#[test]
fn exact_multiple_of_chunk() {
    let mut q = Queue::new();
    for i in 0..2 * CHUNK {
        q.push(i);
    }
    assert_eq!(q.take_last(), (CHUNK..2 * CHUNK).collect::<Vec<_>>());
    assert_eq!(q.take_last(), (0..CHUNK).collect::<Vec<_>>());
    assert!(q.take_last().is_empty());
}

/// Dropping a long queue unlinks its chunks one by one: a recursive drop would overflow this small stack.
#[test]
fn a_long_queue_drops_without_recursing() {
    extern crate std;
    let run = std::thread::Builder::new()
        .stack_size(64 * 1024)
        .spawn(|| {
            let mut q = Queue::new();
            for i in 0..2_000_000u64 {
                q.push(alloc::boxed::Box::new(i));
            }
            drop(q);
        })
        .unwrap();
    assert!(run.join().is_ok());
}

/// Ten thousand items on one CPU all come back, in order.
#[test]
fn monitor_with_many_items_returns_all() {
    let mut monitor = Monitor::new();
    for i in 0..10_000 {
        monitor.retire(i);
    }
    let mut collected = vec![];
    while monitor.is_monitoring() {
        collected.extend(step(&mut monitor, 0, 0b1));
    }
    assert_eq!(collected, (0..10_000).collect::<Vec<_>>());
}

/// A finished grace period's items come out a chunk at a time, and the next grace period ends only once they are out:
/// what a quiescent point does stays bounded however much was retired.
#[test]
fn ready_items_come_out_a_chunk_at_a_time() {
    let mut monitor = Monitor::new();
    for i in 0..1000 {
        monitor.retire(i);
    }
    // The first item had a grace period of its own; the other 999 wait for the next.
    assert_eq!(step(&mut monitor, 0, 0b1), vec![0]);
    monitor.quiescent(0, 0b1);
    let first = monitor.take_ready();
    assert!(!first.is_empty() && first.len() <= CHUNK);
    // Retired now: its grace period runs, every CPU passes, but it waits while ready items are out.
    monitor.retire(5000);
    for _ in 0..3 {
        monitor.quiescent(0, 0b1);
    }
    let mut rest = vec![first];
    loop {
        assert!(monitor.is_monitoring());
        let chunk = monitor.take_ready();
        if chunk.is_empty() {
            break;
        }
        assert!(!chunk.contains(&5000), "5000 came out before the items ready ahead of it");
        rest.push(chunk);
    }
    let mut all: Vec<i32> = rest.into_iter().rev().flatten().collect();
    all.sort();
    assert_eq!(all, (1..1000).collect::<Vec<_>>());
    // Now its grace period can end.
    assert_eq!(step(&mut monitor, 0, 0b1), vec![5000]);
    assert!(!monitor.is_monitoring());
}

/// Nothing ready, nothing handed out; and a monitor with only ready items still says it has work.
#[test]
fn take_ready_when_nothing_is_ready() {
    let mut monitor: Monitor<u32> = Monitor::new();
    assert!(monitor.take_ready().is_empty());
    monitor.retire(1);
    monitor.retire(2);
    monitor.quiescent(0, 0b1);
    assert!(monitor.is_monitoring(), "1 is ready, 2 has a grace period running");
    assert_eq!(monitor.take_ready(), vec![1]);
    monitor.quiescent(0, 0b1);
    assert!(monitor.is_monitoring(), "2 is ready, no grace period runs");
    assert_eq!(monitor.take_ready(), vec![2]);
    assert!(!monitor.is_monitoring());
}

/// The budget: the first chunk always, then within the count and the time.
#[test]
fn the_budget_of_a_quiescent_state() {
    assert!(another_chunk(BUSY, 0, u64::MAX));
    assert!(another_chunk(BUSY, 3, 499));
    assert!(!another_chunk(BUSY, 4, 0));
    assert!(!another_chunk(BUSY, 1, 500));
    let none = Budget { chunks: 0, micros: 0 };
    assert!(another_chunk(none, 0, 0) && !another_chunk(none, 1, 0));
    assert!(IDLE.chunks > BUSY.chunks && IDLE.micros > BUSY.micros, "idle drains first");
    // At most `chunks` chunks of `CHUNK`, whatever the clock says.
    let mut taken = 0;
    while another_chunk(BUSY, taken, 0) {
        taken += 1;
    }
    assert_eq!(taken, BUSY.chunks);
}

#[test]
fn the_batch_grows_with_a_flood_only() {
    // A few waiting: the base.
    assert_eq!(budget_for(BUSY, 0), BUSY);
    assert_eq!(budget_for(BUSY, QHIMARK), BUSY);
    assert_eq!(budget_for(IDLE, 5_000), IDLE);
    // A flood (a process unmapping a gigabyte: 262144 frames): no count limit, 2 ms.
    let flood = budget_for(BUSY, 262_144);
    assert_eq!(flood.chunks, u32::MAX);
    assert_eq!(flood.micros, FLOOD_MICROS);
    // The idle budget keeps its longer time in a flood.
    assert_eq!(budget_for(IDLE, 262_144).micros, IDLE.micros);
    // Still bounded by time: another chunk stops at the flood time.
    assert!(another_chunk(flood, 1_000, FLOOD_MICROS - 1));
    assert!(!another_chunk(flood, 1_000, FLOOD_MICROS));
}
