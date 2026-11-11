//! `--capture-check <secs> <dir>`: is what the host captures what the screen holds? It scans the way a session does, and once a second
//! compares what the scan kept, the mapping read now, and `read(2)` of the device, which is what `dd` sees. Then it writes all three.

use std::os::unix::fs::FileExt;
use std::time::{Duration, Instant};

use super::capture::{Capture, FbSource, Source};

const DEVICE: &str = "/dev/fb0";

/// Rows where two screens differ, and the first of them.
fn differing(a: &[u32], b: &[u32], w: usize) -> (usize, Option<usize>) {
    let mut n = 0;
    let mut first = None;
    for (y, (ra, rb)) in a.chunks(w).zip(b.chunks(w)).enumerate() {
        if ra != rb {
            n += 1;
            first.get_or_insert(y);
        }
    }
    (n, first)
}

fn bytes(px: &[u32]) -> Vec<u8> {
    px.iter().flat_map(|p| p.to_le_bytes()).collect()
}

pub fn run(secs: u64, dir: &str) -> Result<(), String> {
    let mut source = FbSource::open(DEVICE)?;
    let (w, h) = source.size();
    let device = std::fs::File::open(DEVICE).map_err(|e| format!("{DEVICE}: {e}"))?;
    let mut cap = Capture::new(w, h);
    cap.scan(&mut source, true);
    let start = Instant::now();
    let mut compared = Instant::now();
    let mut map = vec![0u32; w * h];
    let mut read = vec![0u8; w * h * 4];
    let mut changed = 0;
    while start.elapsed() < Duration::from_secs(secs) {
        changed += cap.scan(&mut source, false);
        if compared.elapsed() >= Duration::from_secs(1) {
            compared = Instant::now();
            for (y, row) in map.chunks_mut(w).enumerate() {
                source.read_row(y, row);
            }
            device.read_exact_at(&mut read, 0).map_err(|e| format!("{DEVICE}: {e}"))?;
            let by_read: Vec<u32> = read.chunks_exact(4).map(|c| u32::from_le_bytes([c[0], c[1], c[2], c[3]])).collect();
            let (map_rows, map_first) = differing(&map, &by_read, w);
            let (kept_rows, kept_first) = differing(cap.screen(), &by_read, w);
            println!(
                "t={:.1} changed blocks {changed}; mapping against read: {map_rows} rows, first {map_first:?}; kept against read: {kept_rows} rows, first {kept_first:?}",
                start.elapsed().as_secs_f32()
            );
            changed = 0;
        }
        std::thread::sleep(Duration::from_millis(66));
    }
    for (y, row) in map.chunks_mut(w).enumerate() {
        source.read_row(y, row);
    }
    device.read_exact_at(&mut read, 0).map_err(|e| format!("{DEVICE}: {e}"))?;
    let write = |name: &str, b: &[u8]| std::fs::write(format!("{dir}/{name}.raw"), b).map_err(|e| format!("{dir}/{name}.raw: {e}"));
    write("kept", &bytes(cap.screen()))?;
    write("mapping", &bytes(&map))?;
    write("read", &read)
}
