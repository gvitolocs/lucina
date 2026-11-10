// Replays the teo oracle (oracle/teo) against our reimplementation: the same inputs, the same state, must give the same choice.

use lucina_idle::teo::{Clock, State, Teo};

const TICK_NS: i64 = 1_000_000; // HZ 1000, as the oracle's fake kernel

const SILVER: [State; 3] = [
    State { target_residency_ns: 1_000, exit_latency_ns: 1_000 },
    State { target_residency_ns: 1_774_000, exit_latency_ns: 1_450_000 },
    State { target_residency_ns: 4_001_000, exit_latency_ns: 1_617_000 },
];
const GOLD: [State; 3] = [
    State { target_residency_ns: 1_000, exit_latency_ns: 1_000 },
    State { target_residency_ns: 2_207_000, exit_latency_ns: 1_767_000 },
    State { target_residency_ns: 5_555_000, exit_latency_ns: 2_380_000 },
];

#[test]
fn replay() {
    let path = format!("{}/tests/fixtures/teo-oracle.txt", env!("CARGO_MANIFEST_DIR"));
    let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("missing oracle fixture {path}: {e}"));

    let mut teo = Teo::new();
    let mut last_residency: i64 = 0;
    let mut prev: Option<(&str, &str)> = None;

    for (n, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let mut f = line.split_whitespace();
        let table: &str = f.next().unwrap();
        let scenario: &str = f.next().unwrap();
        let step: i64 = f.next().unwrap().parse().unwrap();
        let sleep: i64 = f.next().unwrap().parse().unwrap();
        let delta: i64 = f.next().unwrap().parse().unwrap();
        let latency_req: i64 = f.next().unwrap().parse().unwrap();
        let disabled_mask: u32 = f.next().unwrap().parse().unwrap();
        let residency: i64 = f.next().unwrap().parse().unwrap();
        let got_tick: u32 = f.next().unwrap().parse().unwrap();
        let idx: usize = f.next().unwrap().parse().unwrap();
        let stop_tick: u32 = f.next().unwrap().parse().unwrap();

        let cur = (table, scenario);
        if prev != Some(cur) {
            teo = Teo::new();
            last_residency = 0;
        }
        prev = Some(cur);

        let states: &[State] = if table == "silver" { &SILVER } else { &GOLD };
        let disabled: [bool; 10] = (0..10)
            .map(|i| (disabled_mask >> i) & 1 != 0)
            .collect::<Vec<bool>>()
            .try_into()
            .unwrap();

        let clock = Clock { tick_ns: TICK_NS, tick_stopped: false, latency_req_ns: latency_req };
        let c = teo.select(states, &disabled, clock, last_residency, &mut || (sleep, delta));

        assert_eq!(
            c.state, idx,
            "table={table} scenario={scenario} step={step} (line {}): rust state {}, c state {idx}",
            n + 1, c.state
        );
        assert_eq!(
            c.stop_tick, stop_tick != 0,
            "table={table} scenario={scenario} step={step} (line {}): rust stop_tick {}, c stop_tick {stop_tick}",
            n + 1, c.stop_tick
        );

        teo.reflect(Some(c.state), got_tick != 0);
        last_residency = residency;
    }
}
