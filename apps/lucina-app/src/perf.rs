//! Per-frame performance recorder for Juno's shell and apps.
//!
//! Juno is a 1920x1080 laptop with a 60 Hz panel, so a frame has 16.667 ms.
//! The shell and the apps push one [`FrameRecord`] for every frame that reaches
//! the screen. The recorder keeps the last `capacity` of them in a ring and
//! turns them into one [`Summary`] and one trace line.
//!
//! The scenario suite in `docs/research/juno-perf.md` runs the same scenes
//! before and after a change and compares these lines. The numbers that matter
//! are the p95 and the maximum, the missed vsyncs and the frames over budget.
//!
//! The recorder is plain std. `push` never allocates. `summary` sorts copies
//! in scratch buffers made at construction, so it does not allocate either.
//! That is why `summary` takes `&mut self`.

/// The panel's frame time in microseconds.
pub const PERIOD_US: u32 = 16_667;

/// One frame that reached the screen.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FrameRecord {
    /// Monotonic time the frame reached the screen.
    pub at_us: i64,
    /// Input-to-photon latency, 0 when no input waited for this frame.
    pub input_us: u32,
    /// Drawing/compose time.
    pub compose_us: u32,
    /// Copy/blit to screen time.
    pub present_us: u32,
    /// Pixels presented.
    pub pixels: u32,
    /// CPU the frame ran on, -1 unknown.
    pub cpu: i16,
}

/// Median, 95th percentile and maximum of one series.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Pct {
    pub p50: u32,
    pub p95: u32,
    pub max: u32,
}

/// What the recorder found in the frames it holds.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Summary {
    pub frames: u32,
    /// Over `compose_us`.
    pub compose: Pct,
    /// Over `present_us`.
    pub present: Pct,
    /// Over `compose_us + present_us` (saturating).
    pub total: Pct,
    /// Over `input_us` of frames with `input_us > 0`; None if there are none.
    pub input: Option<Pct>,
    /// Missed vsyncs, see [`missed_vsyncs`].
    pub missed: u32,
    /// Frames whose compose + present is longer than [`PERIOD_US`].
    pub over_budget: u32,
    /// Number of distinct CPU values >= 0 seen (0 if none).
    pub cpus: u8,
    /// CPU of the newest frame, -1 unknown.
    pub last_cpu: i16,
}

/// Fixed-capacity ring of frame records plus the scratch the summary needs.
pub struct Recorder {
    ring: Vec<FrameRecord>,
    cap: usize,
    head: usize, // next slot to write
    len: usize,
    s_compose: Vec<u32>,
    s_present: Vec<u32>,
    s_total: Vec<u32>,
    s_input: Vec<u32>,
    s_at: Vec<i64>,
    s_cpu: Vec<i16>,
}

impl Recorder {
    /// A recorder that keeps the last `capacity` frames (at least 1).
    pub fn new(capacity: usize) -> Recorder {
        let cap = capacity.max(1);
        Recorder {
            ring: vec![FrameRecord::default(); cap],
            cap,
            head: 0,
            len: 0,
            s_compose: Vec::with_capacity(cap),
            s_present: Vec::with_capacity(cap),
            s_total: Vec::with_capacity(cap),
            s_input: Vec::with_capacity(cap),
            s_at: Vec::with_capacity(cap),
            s_cpu: Vec::with_capacity(cap),
        }
    }

    /// Record one frame. The oldest one is dropped when the ring is full.
    pub fn push(&mut self, r: FrameRecord) {
        self.ring[self.head] = r;
        self.head = (self.head + 1) % self.cap;
        if self.len < self.cap {
            self.len += 1;
        }
    }

    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    pub fn clear(&mut self) {
        self.head = 0;
        self.len = 0;
    }

    /// Summary of the frames recorded so far (the last `capacity` at most), or
    /// None when empty. Takes `&mut self` because it uses the scratch buffers.
    pub fn summary(&mut self) -> Option<Summary> {
        if self.len == 0 {
            return None;
        }
        self.s_compose.clear();
        self.s_present.clear();
        self.s_total.clear();
        self.s_input.clear();
        self.s_at.clear();
        self.s_cpu.clear();
        let start = (self.head + self.cap - self.len) % self.cap;
        let mut over_budget = 0u32;
        let mut last_cpu = -1i16;
        for k in 0..self.len {
            let r = self.ring[(start + k) % self.cap];
            let total = r.compose_us.saturating_add(r.present_us);
            self.s_compose.push(r.compose_us);
            self.s_present.push(r.present_us);
            self.s_total.push(total);
            if r.input_us > 0 {
                self.s_input.push(r.input_us);
            }
            self.s_at.push(r.at_us);
            if total > PERIOD_US {
                over_budget += 1;
            }
            if r.cpu >= 0 {
                self.s_cpu.push(r.cpu);
            }
            last_cpu = r.cpu;
        }
        let missed = missed_vsyncs(&self.s_at, PERIOD_US);
        self.s_cpu.sort_unstable();
        self.s_cpu.dedup();
        let cpus = self.s_cpu.len().min(255) as u8;
        let input = if self.s_input.is_empty() {
            None
        } else {
            Some(pct(&mut self.s_input))
        };
        Some(Summary {
            frames: self.len as u32,
            compose: pct(&mut self.s_compose),
            present: pct(&mut self.s_present),
            total: pct(&mut self.s_total),
            input,
            missed,
            over_budget,
            cpus,
            last_cpu,
        })
    }
}

fn pct(v: &mut [u32]) -> Pct {
    v.sort_unstable();
    Pct {
        p50: percentile(v, 50),
        p95: percentile(v, 95),
        max: v[v.len() - 1],
    }
}

/// Nearest-rank percentile of an already sorted, non-empty slice:
/// index = ceil(p/100 * n) - 1, clamped to 0..n-1. `p` is 0..=100.
pub fn percentile(sorted: &[u32], p: u32) -> u32 {
    let n = sorted.len();
    let rank = (p.min(100) as usize * n).div_ceil(100);
    sorted[rank.saturating_sub(1).min(n - 1)]
}

/// Missed vsyncs between consecutive frames of one burst. For each gap
/// `g = at_us[i] - at_us[i-1]` with `0 < g < 4 * period` we add
/// `(g + period/2) / period - 1` when that is positive: the gap rounded to the
/// nearest whole number of periods, minus the one that was due. Gaps of four
/// periods or more are idle time between bursts and count nothing.
pub fn missed_vsyncs(at_us: &[i64], period_us: u32) -> u32 {
    let period = period_us as i64;
    if period <= 0 {
        return 0;
    }
    let mut missed = 0u32;
    for w in at_us.windows(2) {
        let g = w[1] - w[0];
        if g <= 0 || g >= 4 * period {
            continue;
        }
        let m = (g + period / 2) / period - 1;
        if m > 0 {
            missed += m as u32;
        }
    }
    missed
}

fn ms(us: u32) -> String {
    format!("{:.1}", us as f64 / 1000.0)
}

fn triple(p: &Pct) -> String {
    format!("{}/{}/{}", ms(p.p50), ms(p.p95), ms(p.max))
}

impl Summary {
    /// One trace line, times in ms with one decimal:
    /// `perf <label> frames N compose p50/p95/max present ... total ... input ... missed M over K cpu C`.
    /// `input -` when there was no input. `C` is the CPU of the newest frame.
    pub fn line(&self, label: &str) -> String {
        let input = match &self.input {
            Some(p) => triple(p),
            None => "-".to_string(),
        };
        format!(
            "perf {} frames {} compose {} present {} total {} input {} missed {} over {} cpu {}",
            label,
            self.frames,
            triple(&self.compose),
            triple(&self.present),
            triple(&self.total),
            input,
            self.missed,
            self.over_budget,
            self.last_cpu
        )
    }
}

/// The CPU this thread runs on now, -1 if unknown.
pub fn current_cpu() -> i16 {
    // SAFETY: sched_getcpu takes no arguments and only reads the calling
    // thread's state; it returns -1 on error.
    let c = unsafe { libc::sched_getcpu() };
    if (0..=i16::MAX as i32).contains(&c) {
        c as i16
    } else {
        -1
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rec(at: i64, input: u32, c: u32, p: u32, cpu: i16) -> FrameRecord {
        FrameRecord {
            at_us: at,
            input_us: input,
            compose_us: c,
            present_us: p,
            pixels: 100,
            cpu,
        }
    }

    #[test]
    fn percentile_sizes() {
        assert_eq!(percentile(&[7], 0), 7);
        assert_eq!(percentile(&[7], 50), 7);
        assert_eq!(percentile(&[7], 100), 7);
        let two = [1, 2];
        assert_eq!(percentile(&two, 50), 1);
        assert_eq!(percentile(&two, 95), 2);
        assert_eq!(percentile(&two, 100), 2);
        assert_eq!(percentile(&two, 0), 1);
        let ten: Vec<u32> = (1..=10).collect();
        assert_eq!(percentile(&ten, 50), 5);
        assert_eq!(percentile(&ten, 95), 10);
        assert_eq!(percentile(&ten, 100), 10);
        assert_eq!(percentile(&ten, 0), 1);
        let hundred: Vec<u32> = (1..=100).collect();
        assert_eq!(percentile(&hundred, 50), 50);
        assert_eq!(percentile(&hundred, 95), 95);
        assert_eq!(percentile(&hundred, 100), 100);
        assert_eq!(percentile(&hundred, 0), 1);
    }

    fn times(gaps: &[i64]) -> Vec<i64> {
        let mut t = vec![1_000_000i64];
        for g in gaps {
            let last = *t.last().unwrap();
            t.push(last + g);
        }
        t
    }

    #[test]
    fn missed_steady_is_zero() {
        let t = times(&[16_667; 60]);
        assert_eq!(missed_vsyncs(&t, PERIOD_US), 0);
    }

    #[test]
    fn missed_doubled_and_triple() {
        assert_eq!(
            missed_vsyncs(&times(&[16_667, 33_334, 16_667]), PERIOD_US),
            1
        );
        assert_eq!(missed_vsyncs(&times(&[16_667, 50_001]), PERIOD_US), 2);
    }

    #[test]
    fn missed_idle_gap_counts_nothing() {
        assert_eq!(
            missed_vsyncs(&times(&[16_667, 10_000_000, 16_667]), PERIOD_US),
            0
        );
        assert_eq!(missed_vsyncs(&times(&[4 * 16_667]), PERIOD_US), 0);
    }

    #[test]
    fn missed_jitter_and_bad_gaps() {
        let g = [13_667, 19_667, 14_000, 19_000, 16_667];
        assert_eq!(missed_vsyncs(&times(&g), PERIOD_US), 0);
        assert_eq!(missed_vsyncs(&[100, 100, 50], PERIOD_US), 0);
        assert_eq!(missed_vsyncs(&[], PERIOD_US), 0);
        assert_eq!(missed_vsyncs(&[5], PERIOD_US), 0);
    }

    #[test]
    fn ring_wraps_and_keeps_last() {
        let mut r = Recorder::new(4);
        assert!(r.is_empty());
        assert!(r.summary().is_none());
        for i in 0..10u32 {
            r.push(rec(i as i64 * 16_667, 0, i * 1000, 0, 0));
        }
        assert_eq!(r.len(), 4);
        let s = r.summary().unwrap();
        assert_eq!(s.frames, 4);
        // kept compose times 6000, 7000, 8000, 9000
        assert_eq!(s.compose.max, 9000);
        assert_eq!(s.compose.p50, 7000);
        r.clear();
        assert!(r.is_empty());
        assert!(r.summary().is_none());
        r.push(rec(0, 0, 1000, 0, 1));
        assert_eq!(r.summary().unwrap().frames, 1);
    }

    #[test]
    fn summary_known_records() {
        let mut r = Recorder::new(16);
        r.push(rec(0, 0, 4_000, 1_000, 2));
        r.push(rec(16_667, 30_000, 5_000, 1_000, 2));
        r.push(rec(50_001, 0, 14_000, 4_000, 5)); // 2-period gap: 1 missed; 18 ms over budget
        r.push(rec(66_668, 20_000, 3_000, 2_000, -1));
        let s = r.summary().unwrap();
        assert_eq!(s.frames, 4);
        assert_eq!(
            s.compose,
            Pct {
                p50: 4_000,
                p95: 14_000,
                max: 14_000
            }
        );
        assert_eq!(
            s.present,
            Pct {
                p50: 1_000,
                p95: 4_000,
                max: 4_000
            }
        );
        assert_eq!(
            s.total,
            Pct {
                p50: 5_000,
                p95: 18_000,
                max: 18_000
            }
        );
        assert_eq!(
            s.input,
            Some(Pct {
                p50: 20_000,
                p95: 30_000,
                max: 30_000
            })
        );
        assert_eq!(s.missed, 1);
        assert_eq!(s.over_budget, 1);
        assert_eq!(s.cpus, 2);
        assert_eq!(s.last_cpu, -1);
    }

    #[test]
    fn line_format() {
        let s = Summary {
            frames: 120,
            compose: Pct {
                p50: 4_200,
                p95: 9_800,
                max: 12_000,
            },
            present: Pct {
                p50: 1_000,
                p95: 1_500,
                max: 2_000,
            },
            total: Pct {
                p50: 5_200,
                p95: 11_300,
                max: 14_000,
            },
            input: None,
            missed: 3,
            over_budget: 1,
            cpus: 2,
            last_cpu: 6,
        };
        assert_eq!(
            s.line("scroll"),
            "perf scroll frames 120 compose 4.2/9.8/12.0 present 1.0/1.5/2.0 total 5.2/11.3/14.0 input - missed 3 over 1 cpu 6"
        );
        let s2 = Summary {
            input: Some(Pct {
                p50: 20_000,
                p95: 30_000,
                max: 31_300,
            }),
            ..s
        };
        assert!(s2.line("x").contains("input 20.0/30.0/31.3 missed"));
    }

    #[test]
    fn cpu_is_sane() {
        assert!(current_cpu() >= -1);
    }
}
