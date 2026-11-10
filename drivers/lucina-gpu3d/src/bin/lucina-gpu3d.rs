//! `lucina-gpu3d`: proves stage 1 of Juno's 3D GPU stack on the laptop (`docs/research/juno-gpu-3d.md`). Every check prints
//! `ok <name> ...` or `FAIL <name> ...`, and the exit code is the number of failures.
//!
//! - `lucina-gpu3d params`: what the kernel says about the context.
//! - `lucina-gpu3d write [rounds]`: a command buffer in the program's memory writes words into another object; the CPU reads
//!   them back. Prints what a submit and its wait cost.
//! - `lucina-gpu3d fill`: the 2D engine, driven from the program's own command buffer, fills and copies inside its objects.
//! - `lucina-gpu3d isolate`: two contexts with objects at the same GPU address see only their own; writes to an unmapped
//!   address, to the screen's physical address, to the ring and to a read-only object fault without effect, and the GPU goes
//!   on.
//! - `lucina-gpu3d queue [n]`: n submits queued at once, then one wait.
//! - `lucina-gpu3d close-busy`: a context closed with its work still running; the next one works.
//! - `lucina-gpu3d clobber`: a command buffer changes registers the kernel's batches share; run `lucina-gpu tiles` after.
//! - `lucina-gpu3d hostile`: command buffers that hang the GPU, write a protected register, carry a packet that is no packet,
//!   and switch to another program's table: each is stopped, the GPU is reset (patch 1172), that program is banned and the
//!   others go on.
//! - `lucina-gpu3d blame [rounds]`: program A's command buffer faults and program B's runs right after it, so that the bank
//!   holds B's table by the time the work retires: the fault must still be A's (SPI 364, patch 1173).
//! - `lucina-gpu3d all`: everything above but `clobber` and `hostile`.

#[cfg(all(target_os = "linux", target_env = "gnu", target_arch = "aarch64"))]
extern crate eyra;

use std::time::{Duration, Instant};

use lucina_gpu3d::{Bo, Device, Pm4, Use, Va, a2d, pm4, uapi};

const SECOND: Duration = Duration::from_secs(1);

struct Report {
    failures: u32,
}

impl Report {
    fn check(&mut self, name: &str, ok: bool, detail: String) {
        if ok {
            println!("ok {} {}", name, detail);
        } else {
            println!("FAIL {} {}", name, detail);
            self.failures += 1;
        }
    }
}

fn status_value(key: &str) -> Option<u64> {
    let text = std::fs::read_to_string(lucina_gpu3d::DEVICE).ok()?;
    text.lines().find_map(|l| {
        let v = l.strip_prefix(key)?.strip_prefix(' ')?;
        let v = v.split_whitespace().next()?;
        v.strip_prefix("0x").map_or_else(|| v.parse().ok(), |h| u64::from_str_radix(h, 16).ok())
    })
}

fn params(r: &mut Report) {
    let d = Device::open().expect("open");
    let chip = d.param(uapi::PARAM_CHIP_ID).unwrap_or(0);
    let start = d.param(uapi::PARAM_VA_START).unwrap_or(0);
    let end = d.param(uapi::PARAM_VA_END).unwrap_or(0);
    let gmem = d.param(uapi::PARAM_GMEM_SIZE).unwrap_or(0);
    r.check("params", chip == 0x0601_0800 && start < end, format!("chip {:#x} window {:#x}..{:#x} gmem {}", chip, start, end, gmem));
}

/// A command buffer object and a data object, at fresh addresses.
fn pair(d: &Device, va: &mut Va, data_size: u64) -> (Bo, Bo) {
    let cmd = d.bo(va.take(0x1_0000), 0x1_0000, false).expect("command object");
    let data = d.bo(va.take(data_size), data_size, false).expect("data object");
    (cmd, data)
}

fn write(r: &mut Report, rounds: u32) {
    let d = Device::open().expect("open");
    let mut va = Va::default();
    let (mut cmd, mut data) = pair(&d, &mut va, 0x1000);
    let mut worst = Duration::ZERO;
    let mut total = Duration::ZERO;
    let mut wrong = 0;
    for round in 0..rounds {
        data.words()[..4].fill(0);
        let mut p = Pm4::default();
        let value = 0x5a00_0000 | round;
        p.mem_write(data.iova + 4, &[value, !value]);
        let c = p.into_bo(&mut cmd, 0);
        let started = Instant::now();
        let seq = d.submit(&[c], &[(&data, Use::Clean)]).expect("submit");
        d.wait(seq, SECOND).expect("wait");
        let took = started.elapsed();
        total += took;
        worst = worst.max(took);
        data.invalidate().unwrap();
        let w = data.words();
        if w[0] != 0 || w[1] != value || w[2] != !value || w[3] != 0 {
            wrong += 1;
            if wrong < 4 {
                println!("  round {}: {:#x} {:#x} {:#x} {:#x}", round, w[0], w[1], w[2], w[3]);
            }
        }
    }
    r.check(
        "write",
        wrong == 0,
        format!("{} rounds, {} wrong, submit+wait average {} us, worst {} us", rounds, wrong, total.as_micros() / rounds.max(1) as u128, worst.as_micros()),
    );
}

fn fill(r: &mut Report) {
    let d = Device::open().expect("open");
    let mut va = Va::default();
    let (w, h) = (256u32, 128u32);
    let pitch = w * 4;
    let size = (pitch * h) as u64;
    let mut cmd = d.bo(va.take(0x1_0000), 0x1_0000, false).unwrap();
    let mut a = d.bo(va.take(size), size, false).unwrap();
    let mut b = d.bo(va.take(size), size, false).unwrap();
    let scratch = d.bo(va.take(0x1000), 0x1000, false).unwrap();
    a.words().fill(0);
    b.words().fill(0);
    let sa = a2d::Surface { address: a.iova, pitch, width: w, height: h };
    let sb = a2d::Surface { address: b.iova, pitch, width: w, height: h };
    let mut words = Vec::new();
    a2d::setup(&mut words);
    a2d::fill(&mut words, &sa, &a2d::Rect { x: 10, y: 5, w: 100, h: 50 }, 0xff11_2233).unwrap();
    a2d::flush(&mut words, scratch.iova);
    a2d::blit(&mut words, &sa, &a2d::Rect { x: 10, y: 5, w: 100, h: 50 }, &sb, &a2d::Rect { x: 100, y: 60, w: 100, h: 50 }, false).unwrap();
    a2d::flush(&mut words, scratch.iova);
    let p = Pm4 { words };
    let c = p.into_bo(&mut cmd, 0);
    let started = Instant::now();
    let seq = d.submit(&[c], &[(&a, Use::Clean), (&b, Use::Clean), (&scratch, Use::Gpu)]).unwrap();
    d.wait(seq, SECOND).unwrap();
    let took = started.elapsed();
    a.invalidate().unwrap();
    b.invalidate().unwrap();
    let px = |bo: &mut Bo, x: u32, y: u32| bo.words()[(y * w + x) as usize];
    let ok = px(&mut a, 10, 5) == 0xff11_2233
        && px(&mut a, 109, 54) == 0xff11_2233
        && px(&mut a, 110, 54) == 0
        && px(&mut a, 9, 5) == 0
        && px(&mut b, 100, 60) == 0xff11_2233
        && px(&mut b, 199, 109) == 0xff11_2233
        && px(&mut b, 200, 109) == 0
        && px(&mut b, 10, 5) == 0;
    r.check(
        "fill",
        ok,
        format!(
            "{} us; a {:#x} {:#x} out {:#x}; b {:#x} {:#x} out {:#x}",
            took.as_micros(),
            px(&mut a, 10, 5),
            px(&mut a, 109, 54),
            px(&mut a, 110, 54),
            px(&mut b, 100, 60),
            px(&mut b, 199, 109),
            px(&mut b, 200, 109)
        ),
    );
    let faults = d.faults().unwrap_or(u64::MAX);
    r.check("fill-no-fault", faults == 0, format!("faults {}", faults));
}

/// One write by the GPU to `target`, then a check that the context still works.
fn bad_write(r: &mut Report, d: &Device, cmd: &mut Bo, probe: &mut Bo, name: &str, target: u64) {
    let before = d.faults().unwrap();
    let mut p = Pm4::default();
    p.mem_write(target, &[0xbad0_0bad, 0xbad0_0bad]);
    let c = p.into_bo(cmd, 0);
    let seq = d.submit(&[c], &[]).expect("submit");
    let fenced = d.wait(seq, SECOND).is_ok();
    let after = d.faults().unwrap();
    // The GPU goes on: a good write after it lands.
    probe.words()[0] = 0;
    let mut p = Pm4::default();
    p.mem_write(probe.iova, &[0x600d]);
    let c = p.into_bo(cmd, 64);
    let seq = d.submit(&[c], &[(probe, Use::Clean)]).expect("submit");
    let went_on = d.wait(seq, SECOND).is_ok() && {
        probe.invalidate().unwrap();
        probe.words()[0] == 0x600d
    };
    r.check(name, fenced && after > before && went_on, format!("fence {} faults {}->{} then {}", fenced, before, after, went_on));
}

fn isolate(r: &mut Report) {
    let a = Device::open().expect("open a");
    let b = Device::open().expect("open b");
    let at = lucina_gpu3d::gpu_rules::BO_BASE + 0x100_0000;
    let mut a_cmd = a.bo(at - 0x10_0000, 0x1_0000, false).unwrap();
    let mut b_cmd = b.bo(at - 0x10_0000, 0x1_0000, false).unwrap();
    let mut a_data = a.bo(at, 0x1000, false).unwrap();
    let mut b_data = b.bo(at, 0x1000, false).unwrap();
    a_data.words()[0] = 0x1111_1111;
    b_data.words()[0] = 0x2222_2222;
    let mut p = Pm4::default();
    p.mem_write(at, &[0xbbbb_bbbb]);
    let c = p.into_bo(&mut b_cmd, 0);
    let seq = b.submit(&[c], &[(&b_data, Use::Clean)]).unwrap();
    b.wait(seq, SECOND).unwrap();
    // `a` also runs, so that a's table was in the bank before and after.
    let mut p = Pm4::default();
    p.mem_write(at + 4, &[0xaaaa_aaaa]);
    let c = p.into_bo(&mut a_cmd, 0);
    let seq = a.submit(&[c], &[(&a_data, Use::Clean)]).unwrap();
    a.wait(seq, SECOND).unwrap();
    a_data.invalidate().unwrap();
    b_data.invalidate().unwrap();
    let (a0, a1, b0, b1) = (a_data.words()[0], a_data.words()[1], b_data.words()[0], b_data.words()[1]);
    r.check(
        "isolate-same-address",
        a0 == 0x1111_1111 && a1 == 0xaaaa_aaaa && b0 == 0xbbbb_bbbb && b1 == 0,
        format!("a {:#x} {:#x}, b {:#x} {:#x}", a0, a1, b0, b1),
    );

    let mut probe = b.bo(at + 0x10_0000, 0x1000, false).unwrap();
    bad_write(r, &b, &mut b_cmd, &mut probe, "fault-unmapped", at + 0x80_0000);
    // The screen, by its physical address: not in any program's table.
    let fb_before = std::fs::read("/dev/fb0").ok().map(|v| v[..64].to_vec());
    bad_write(r, &b, &mut b_cmd, &mut probe, "fault-screen", 0x9bc0_0000);
    let fb_after = std::fs::read("/dev/fb0").ok().map(|v| v[..64].to_vec());
    r.check("screen-untouched", fb_before == fb_after && fb_before.is_some(), String::new());
    // The ring: in every table, read-only.
    if let Some(ring) = status_value("ring") {
        bad_write(r, &b, &mut b_cmd, &mut probe, "fault-ring-read-only", ring + 0x7f00);
    } else {
        r.check("fault-ring-read-only", false, String::from("no ring line in the status"));
    }
    // An object the GPU may only read.
    let ro = b.bo(at + 0x20_0000, 0x1000, true).unwrap();
    bad_write(r, &b, &mut b_cmd, &mut probe, "fault-read-only-object", ro.iova);
    // The other context saw none of it.
    r.check("faults-stay-with-their-program", a.faults().unwrap() == 0, format!("a faults {}", a.faults().unwrap()));
    // Kernel-made addresses refuse.
    let refused = [
        a.bo(lucina_gpu3d::gpu_rules::FENCE_IOVA, 0x1000, false).is_err(),
        a.bo(0x9bc0_0000, 0x1000, false).is_err(),
        a.bo(at, 0x1000, false).is_err(),
        a.bo(lucina_gpu3d::gpu_rules::VA_END - 0x1000, 0x2000, false).is_err(),
    ];
    r.check("refused-ranges", refused.iter().all(|x| *x), format!("{:?}", refused));
}

fn queue(r: &mut Report, n: u32) {
    let d = Device::open().expect("open");
    let mut va = Va::default();
    let (mut cmd, mut data) = pair(&d, &mut va, 0x1000);
    data.words().fill(0);
    let mut cmds = Vec::new();
    for i in 0..n as usize {
        let mut p = Pm4::default();
        p.mem_write(data.iova + 4 * i as u64, &[i as u32 + 1]);
        cmds.push(p.into_bo(&mut cmd, i * 8));
    }
    let started = Instant::now();
    let mut last = 0;
    for (i, c) in cmds.iter().enumerate() {
        let named: &[(&Bo, Use)] = if i == 0 { &[(&data, Use::Clean)] } else { &[] };
        last = d.submit(std::slice::from_ref(c), named).expect("submit");
    }
    let queued = started.elapsed();
    d.wait(last, SECOND).unwrap();
    let all = started.elapsed();
    data.invalidate().unwrap();
    let ok = (0..n as usize).all(|i| data.words()[i] == i as u32 + 1);
    r.check("queue", ok, format!("{} submits queued in {} us, all done in {} us", n, queued.as_micros(), all.as_micros()));
}

fn close_busy(r: &mut Report) {
    {
        let d = Device::open().expect("open");
        let mut va = Va::default();
        let (w, h) = (1920u32, 1080u32);
        let size = (w * h * 4) as u64;
        let mut cmd = d.bo(va.take(0x1_0000), 0x1_0000, false).unwrap();
        let big = d.bo(va.take(size), size, false).unwrap();
        let scratch = d.bo(va.take(0x1000), 0x1000, false).unwrap();
        let s = a2d::Surface { address: big.iova, pitch: w * 4, width: w, height: h };
        let mut words = Vec::new();
        a2d::setup(&mut words);
        for i in 0..30 {
            a2d::fill(&mut words, &s, &a2d::Rect { x: 0, y: 0, w, h }, 0xff00_0000 | i).unwrap();
            a2d::flush(&mut words, scratch.iova);
        }
        let c = Pm4 { words }.into_bo(&mut cmd, 0);
        let _ = d.submit(&[c], &[(&big, Use::Gpu)]).unwrap();
        // Closed here, with about 20 ms of fills on the ring.
    }
    let deferred = status_value("deferred").unwrap_or(99);
    let mut w = Report { failures: 0 };
    write(&mut w, 10);
    std::thread::sleep(Duration::from_millis(100));
    let after = status_value("deferred").unwrap_or(99);
    r.check("close-busy", w.failures == 0 && after == 0, format!("deferred right after the close {}, 100 ms later {}", deferred, after));
}

fn clobber(r: &mut Report) {
    let d = Device::open().expect("open");
    let mut va = Va::default();
    let mut cmd = d.bo(va.take(0x1_0000), 0x1_0000, false).unwrap();
    let mut p = Pm4::default();
    // RB_CCU_CNTL, GRAS_SC_CNTL, RB_MODE_CNTL, SP_CHICKEN_BITS: registers the kernel's batches take as set.
    p.pkt4(0x8e07, &[0]);
    p.pkt4(0x80a0, &[0]);
    p.pkt4(0x8811, &[0]);
    p.pkt4(0xae03, &[0]);
    let c = p.into_bo(&mut cmd, 0);
    let seq = d.submit(&[c], &[]).unwrap();
    d.wait(seq, SECOND).unwrap();
    r.check("clobber", true, String::from("registers changed: run `lucina-gpu tiles 2` and `lucina-gpu yuv` now"));
    let _ = pm4::CP_NOP;
}

/// Runs `p` in a fresh context; expects its wait to fail (the GPU reset), the context to be refused after, and a new
/// context to work. Returns the time from the submit to the failed wait.
fn hostile_one(r: &mut Report, name: &str, expect_reset: bool, build: impl FnOnce(&mut Pm4, &Bo)) -> Duration {
    let recoveries = status_value("recoveries").unwrap_or(0);
    let d = Device::open().expect("open");
    let mut va = Va::default();
    let (mut cmd, data) = pair(&d, &mut va, 0x1000);
    let mut p = Pm4::default();
    build(&mut p, &data);
    let c = p.into_bo(&mut cmd, 0);
    let started = Instant::now();
    let seq = d.submit(&[c], &[(&data, Use::Clean)]).expect("submit");
    let waited = d.wait(seq, Duration::from_secs(8));
    let took = started.elapsed();
    let banned = d.submit(&[c], &[]).is_err();
    let after = status_value("recoveries").unwrap_or(0);
    let mut w = Report { failures: 0 };
    write(&mut w, 20);
    let stopped = if expect_reset { waited.is_err() && banned && after == recoveries + 1 } else { true };
    r.check(
        name,
        stopped && w.failures == 0,
        format!(
            "wait {:?} after {} ms, refused after {}, recoveries {}->{}, a new program works {}",
            waited.err().map(|e| e.raw_os_error()),
            took.as_millis(),
            banned,
            recoveries,
            after,
            w.failures == 0
        ),
    );
    took
}

fn hostile(r: &mut Report) {
    // CP_WAIT_REG_MEM on memory that never says 1: the CP waits for ever.
    hostile_one(r, "hang", true, |p, data| {
        p.pkt7(pm4::CP_WAIT_REG_MEM, &[3 | (1 << 4), data.iova as u32, (data.iova >> 32) as u32, 1, 1, 0]);
    });
    // CP_RB_BASE (0x800), which CP_PROTECT denies to a command buffer.
    hostile_one(r, "protected-register", true, |p, _| {
        p.pkt4(0x0800, &[0x1000, 0]);
    });
    // A word that is no packet header.
    hostile_one(r, "not-a-packet", true, |p, _| {
        p.words.extend_from_slice(&[0xdead_beef; 8]);
    });
    // Another program's table, by its physical address (the status shows the table of the last fault): if a command buffer could
    // switch tables, the write after it would land in the other program's object.
    let victim = Device::open().expect("open");
    let at = lucina_gpu3d::gpu_rules::BO_BASE + 0x100_0000;
    let mut vcmd = victim.bo(at - 0x10_0000, 0x1_0000, false).unwrap();
    let mut vdata = victim.bo(at, 0x1000, false).unwrap();
    vdata.words()[0] = 0x7777_7777;
    let mut p = Pm4::default();
    p.mem_write(at + 0x80_0000, &[1]);
    let c = p.into_bo(&mut vcmd, 0);
    let seq = victim.submit(&[c], &[(&vdata, Use::Clean)]).unwrap();
    let _ = victim.wait(seq, SECOND);
    let root = std::fs::read_to_string(lucina_gpu3d::DEVICE)
        .ok()
        .and_then(|t| t.lines().find_map(|l| l.strip_prefix("smmu_last_fault ").map(String::from)))
        .and_then(|l| l.split_whitespace().nth(3).and_then(|v| u64::from_str_radix(v.trim_start_matches("0x"), 16).ok()));
    let Some(root) = root else {
        r.check("table-switch", false, String::from("no table in the status"));
        return;
    };
    hostile_one(r, "table-switch", false, |p, _| {
        p.pkt7(pm4::CP_SMMU_TABLE_UPDATE, &[root as u32, (root >> 32) as u32 & 0xffff, 0, 0]);
        p.mem_write(at, &[0x6666_6666]);
    });
    vdata.invalidate().unwrap();
    let v = vdata.words()[0];
    r.check("table-switch-victim-untouched", v == 0x7777_7777, format!("victim word {:#x}", v));
}

fn blame(r: &mut Report, rounds: u32) {
    let a = Device::open().expect("open a");
    let b = Device::open().expect("open b");
    let mut va_a = Va::default();
    let mut va_b = Va::default();
    let (mut a_cmd, _a_data) = pair(&a, &mut va_a, 0x1000);
    let (mut b_cmd, b_data) = pair(&b, &mut va_b, 0x1000);
    let irq_before = status_value("smmu_irq_interrupts").unwrap_or(0);
    let mut p = Pm4::default();
    p.mem_write(lucina_gpu3d::gpu_rules::BO_BASE + 0x80_0000, &[0xbad]);
    let bad = p.into_bo(&mut a_cmd, 0);
    let mut p = Pm4::default();
    p.mem_write(b_data.iova, &[0x600d]);
    let good = p.into_bo(&mut b_cmd, 0);
    for _ in 0..rounds {
        let sa = a.submit(&[bad], &[]).expect("submit a");
        let sb = b.submit(&[good], &[(&b_data, Use::Clean)]).expect("submit b");
        a.wait(sa, SECOND).expect("wait a");
        b.wait(sb, SECOND).expect("wait b");
    }
    let (fa, fb) = (a.faults().unwrap(), b.faults().unwrap());
    let irq_after = status_value("smmu_irq_interrupts").unwrap_or(0);
    r.check(
        "blame",
        fa >= rounds as u64 && fb == 0,
        format!("{} rounds: faults A {} B {}; SMMU interrupts {} -> {}; dropped {}", rounds, fa, fb, irq_before, irq_after,
            status_value("smmu_faults_dropped").unwrap_or(99)),
    );
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut r = Report { failures: 0 };
    let n = |i: usize, d: u32| args.get(i).and_then(|v| v.parse().ok()).unwrap_or(d);
    match args.first().map(String::as_str) {
        Some("params") => params(&mut r),
        Some("write") => write(&mut r, n(1, 1000)),
        Some("fill") => fill(&mut r),
        Some("isolate") => isolate(&mut r),
        Some("queue") => queue(&mut r, n(1, 60)),
        Some("close-busy") => close_busy(&mut r),
        Some("clobber") => clobber(&mut r),
        Some("hostile") => hostile(&mut r),
        Some("blame") => blame(&mut r, n(1, 100)),
        Some("all") => {
            params(&mut r);
            write(&mut r, 1000);
            fill(&mut r);
            isolate(&mut r);
            queue(&mut r, 60);
            close_busy(&mut r);
        }
        _ => {
            eprintln!("usage: lucina-gpu3d params|write [n]|fill|isolate|queue [n]|close-busy|clobber|hostile|blame [n]|all");
            std::process::exit(2);
        }
    }
    println!("{} failures", r.failures);
    std::process::exit(r.failures as i32);
}
