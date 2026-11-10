//! Malformed input must only ever produce errors: truncated and randomly mutated blobs, never a panic or a hang.

use lucina_fdt::{Edit, Fdt, Tree, patch, report};

const QEMU: &[u8] = include_bytes!("data/qemu-virt-gicv3-8g.dtb");

/// Touches every reader API; results are ignored, only panics and hangs matter.
fn exercise(bytes: &[u8]) {
    let Ok(fdt) = Fdt::new(bytes) else { return };
    for t in fdt.tokens() {
        let _ = t;
    }
    let _ = fdt.mem_reservations().count();
    if let Ok(root) = fdt.root() {
        let _ = root.address_cells();
        for p in root.properties().flatten() {
            let _ = (
                p.as_u32(),
                p.as_u64(),
                p.as_uint(),
                p.as_str(),
                p.strings().count(),
            );
            if let Ok(r) = p.reg(2, 2) {
                let _ = r.count();
            }
        }
        let mut stack = vec![root];
        while let Some(n) = stack.pop() {
            for c in n.children().flatten() {
                let _ = c.property("reg");
                stack.push(c);
            }
        }
    }
    if let Ok(m) = fdt.memory() {
        let _ = m.count();
    }
    if let Ok(Some(r)) = fdt.reserved_memory() {
        for c in r.flatten() {
            let _ = c.regs().map(Iterator::count);
        }
    }
    let _ = fdt.chosen();
    let _ = fdt.node("/soc/nope@1");
    // Whatever the reader accepts, the patcher must rewrite into something the reader accepts again.
    let edits = [
        Edit::Set {
            node: "/chosen",
            name: "bootargs",
            value: b"x\0",
        },
        Edit::Set {
            node: "/chosen",
            name: "lucina,new",
            value: &[0; 8],
        },
        Edit::Remove {
            node: "/memory",
            name: "reg",
        },
    ];
    let mut out = vec![0u8; bytes.len() + 256];
    if let Ok(n) = patch(&fdt, &edits, &mut out) {
        assert!(Fdt::new(&out[..n]).is_ok(), "patched blob does not parse");
    }
    let _ = report::info(bytes);
    if let Ok(t) = Tree::parse(bytes) {
        let _ = t.to_bytes();
    }
}

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        // xorshift64*
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_f491_4f6c_dd1d)
    }

    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
}

/// The meaningful part of the fixture (QEMU pads `totalsize` to 1 MiB with zeros).
fn content_len() -> usize {
    let off_strings = u32::from_be_bytes(QEMU[12..16].try_into().unwrap()) as usize;
    let size_strings = u32::from_be_bytes(QEMU[32..36].try_into().unwrap()) as usize;
    off_strings + size_strings
}

#[test]
fn truncated_blobs_are_errors() {
    let n = content_len();
    let packed = Tree::parse(QEMU).unwrap().to_bytes().unwrap();
    for blob in [&QEMU[..n], &packed[..]] {
        for len in 0..blob.len() {
            assert!(
                Fdt::new(&blob[..len]).is_err(),
                "prefix of {len} bytes accepted"
            );
        }
    }
}

#[test]
fn truncated_with_forged_totalsize_is_handled() {
    // totalsize is made to match the shortened buffer, so only the block bounds can catch it.
    let packed = Tree::parse(QEMU).unwrap().to_bytes().unwrap();
    for len in (40..packed.len()).step_by(4) {
        let mut b = packed[..len].to_vec();
        b[4..8].copy_from_slice(&(len as u32).to_be_bytes());
        exercise(&b);
        assert!(
            Fdt::new(&b).is_err(),
            "forged prefix of {len} bytes accepted"
        );
    }
}

#[test]
fn random_mutations_never_panic() {
    let packed = Tree::parse(QEMU).unwrap().to_bytes().unwrap();
    let mut rng = Rng(0x9e37_79b9_7f4a_7c15);
    for i in 0..30_000 {
        let mut b = packed.clone();
        let flips = 1 + rng.below(8);
        for _ in 0..flips {
            // Half the time leave the header alone so mutations reach the structure and strings blocks.
            let lo = if i % 2 == 0 { 0 } else { 40 };
            let at = lo + rng.below(b.len() - lo);
            match rng.below(3) {
                0 => b[at] ^= 1 << rng.below(8),
                1 => b[at] = rng.next() as u8,
                _ => b[at] = [0x00, 0xff, 0x01, 0x03, 0x09][rng.below(5)],
            }
        }
        if i % 7 == 0 {
            let cut = rng.below(b.len());
            b.truncate(cut);
        }
        exercise(&b);
    }
}

#[test]
fn random_garbage_never_panics() {
    let mut rng = Rng(42);
    for _ in 0..2_000 {
        let len = rng.below(300);
        let mut b: Vec<u8> = (0..len).map(|_| rng.next() as u8).collect();
        if len >= 8 {
            // Valid magic and a plausible totalsize so the parser gets past the first check.
            b[..4].copy_from_slice(&0xd00d_feedu32.to_be_bytes());
            b[4..8].copy_from_slice(&(len as u32).to_be_bytes());
        }
        exercise(&b);
    }
}
