//! Names what `/proc/lucina-prof` sampled: one report per run, on the Linux host, from the kernel's symbol table.
//!
//! The file is a header of [`HEADER_WORDS`] little-endian 64-bit words (magic, samples, first tick, last tick) and then
//! [`SAMPLE_WORDS`] words per sample: the interrupted address, `x30`, and three return addresses up the frame pointers.

use std::collections::HashMap;

const MAGIC: u64 = 0x4c55_4350_524f_4631; // "LUCPROF1"
const HEADER_WORDS: usize = 4;
const SAMPLE_WORDS: usize = 5;
const KERNEL_SPACE: u64 = 0xffff_0000_0000_0000;

/// A category is the first whose word appears in a function's name, anywhere up the stack as far as the sample reaches.
/// Order matters: a copy made inside the checksum's caller is still a copy.
const CATEGORIES: &[(&str, &[&str])] = &[
    ("idle", &["cpu_idle", "wait_for_interrupt", "idle_loop", "enter_idle", "stop_tick_for_idle"]),
    ("checksum", &["checksum"]),
    (
        "copy",
        &["memcpy", "memmove", "memset", "copy_fallible", "__memcpy", "rep_movs", "copy_nonoverlapping", "read_fallible", "write_fallible"],
    ),
    ("usb", &["crab_usb", "aster_usb", "usb_net", "lucina_rtl8152", "lucina_usbnet", "dwc3", "xhci"]),
    ("wifi", &["lucina_ath10k", "lucina_wifi", "qcom_wifi"]),
    ("tcp/ip", &["smoltcp", "aster_bigtcp", "net::socket", "net::iface", "aster_network"]),
    ("alloc", &["slab", "alloc", "frame_allocator", "dealloc", "heap"]),
    ("sched", &["sched::", "switch_to", "task::", "wait_queue", "Waiter", "timer::", "softirq"]),
    ("syscall", &["syscall", "sys_"]),
];

pub fn report(args: &[String]) {
    let [samples, symbols] = args else {
        eprintln!("usage: lucina-netperf report <samples> <nm -n output>");
        std::process::exit(2);
    };
    let samples = std::fs::read(samples).expect("samples");
    let symbols = std::fs::read_to_string(symbols).expect("symbols");
    print!("{}", render(&samples, &parse_symbols(&symbols)));
}

/// `nm -n --demangle`, sorted by address: text symbols only.
fn parse_symbols(text: &str) -> Vec<(u64, String)> {
    let mut symbols: Vec<(u64, String)> = text
        .lines()
        .filter_map(|line| {
            let mut fields = line.splitn(3, ' ');
            let address = u64::from_str_radix(fields.next()?, 16).ok()?;
            let kind = fields.next()?;
            let name = fields.next()?;
            matches!(kind, "t" | "T" | "w" | "W").then(|| (address, name.to_string()))
        })
        .collect();
    symbols.sort_by_key(|(address, _)| *address);
    symbols
}

fn name_of(symbols: &[(u64, String)], address: u64) -> Option<&str> {
    let index = symbols.partition_point(|(start, _)| *start <= address);
    index.checked_sub(1).map(|i| symbols[i].1.as_str())
}

fn render(bytes: &[u8], symbols: &[(u64, String)]) -> String {
    let words: Vec<u64> = bytes
        .chunks_exact(8)
        .map(|word| {
            let mut value = [0u8; 8];
            value.copy_from_slice(word);
            u64::from_le_bytes(value)
        })
        .collect();
    if words.len() < HEADER_WORDS || words[0] != MAGIC {
        return "not a /proc/lucina-prof file\n".to_string();
    }
    let (first_tick, last_tick) = (words[2], words[3]);
    let elapsed = last_tick.saturating_sub(first_tick);
    let samples: Vec<&[u64]> = words[HEADER_WORDS..].chunks_exact(SAMPLE_WORDS).collect();

    let mut functions: HashMap<&str, u64> = HashMap::new();
    let mut categories: HashMap<&str, u64> = HashMap::new();
    for sample in &samples {
        let category = if sample[0] < KERNEL_SPACE {
            "user"
        } else {
            let names: Vec<&str> = sample.iter().filter_map(|pc| name_of(symbols, *pc)).collect();
            *functions.entry(names.first().copied().unwrap_or("?")).or_default() += 1;
            category_of(&names)
        };
        *categories.entry(category).or_default() += 1;
    }

    let total = samples.len() as u64;
    let idle = categories.get("idle").copied().unwrap_or(0);
    // An idle CPU stops its tick, so a tick that never came was idle too.
    let busy = total - idle;
    let mut out = format!(
        "{total} samples over {elapsed} ticks of 1 ms; CPU busy {:.1}% of the time\n\n",
        percent(busy, elapsed.max(total))
    );
    let mut sorted: Vec<(&str, u64)> = categories.into_iter().collect();
    sorted.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(b.0)));
    for (name, count) in sorted {
        out += &format!("{:>6.1}% of samples  {name}\n", percent(count, total));
    }
    out += "\nmost sampled functions (of kernel samples):\n";
    let mut sorted: Vec<(&str, u64)> = functions.into_iter().collect();
    sorted.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(b.0)));
    for (name, count) in sorted.iter().take(40) {
        out += &format!("{:>6.1}%  {name}\n", percent(*count, total));
    }
    out
}

fn category_of(names: &[&str]) -> &'static str {
    // The interrupted function decides first: a copy under the USB thread is a copy.
    for name in names {
        for (category, words) in CATEGORIES {
            if words.iter().any(|word| name.contains(word)) {
                return category;
            }
        }
    }
    "other kernel"
}

fn percent(part: u64, whole: u64) -> f64 {
    100.0 * part as f64 / whole.max(1) as f64
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(samples: &[[u64; SAMPLE_WORDS]], first: u64, last: u64) -> Vec<u8> {
        let mut words = vec![MAGIC, samples.len() as u64, first, last];
        words.extend(samples.iter().flatten());
        words.iter().flat_map(|word| word.to_le_bytes()).collect()
    }

    const BASE: u64 = 0xffff_8000_0000_0000;

    fn symbols() -> Vec<(u64, String)> {
        parse_symbols(&format!(
            "{:x} T memcpy\n{:x} t smoltcp::wire::ip::checksum::data\n{:x} t aster_bigtcp::iface::poll\n{:x} t ostd::task::idle::wait_for_interrupt\n",
            BASE,
            BASE + 0x100,
            BASE + 0x200,
            BASE + 0x300
        ))
    }

    #[test]
    fn the_interrupted_function_decides_the_category() {
        let copy_under_poll = [BASE + 8, BASE + 0x208, 0, 0, 0];
        let checksum_under_poll = [BASE + 0x108, BASE + 0x208, 0, 0, 0];
        let poll = [BASE + 0x208, 0, 0, 0, 0];
        let text = render(
            &file(&[copy_under_poll, checksum_under_poll, poll, poll], 100, 104),
            &symbols(),
        );
        assert!(text.contains("25.0% of samples  copy"), "{text}");
        assert!(text.contains("25.0% of samples  checksum"), "{text}");
        assert!(text.contains("50.0% of samples  tcp/ip"), "{text}");
    }

    #[test]
    fn ticks_that_never_came_count_as_idle() {
        // Two busy samples in 100 ms: the tick stopped for the rest.
        let poll = [BASE + 0x208, 0, 0, 0, 0];
        let idle = [BASE + 0x308, 0, 0, 0, 0];
        let text = render(&file(&[poll, poll, idle], 0, 100), &symbols());
        assert!(text.contains("CPU busy 2.0%"), "{text}");
    }

    #[test]
    fn user_addresses_need_no_symbol() {
        let text = render(&file(&[[0x40_0000, 0, 0, 0, 0]], 0, 1), &symbols());
        assert!(text.contains("100.0% of samples  user"), "{text}");
    }

    #[test]
    fn a_foreign_file_is_refused() {
        assert!(render(&[0u8; 64], &symbols()).starts_with("not a"));
    }
}
