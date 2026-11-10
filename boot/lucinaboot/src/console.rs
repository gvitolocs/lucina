//! Output on the PL011 UART named by `/chosen/stdout-path`, when there is one (QEMU virt). surya's GENI debug
//! UART and the ABL framebuffer are not drawn on yet.

use core::{
    fmt::{self, Write},
    sync::atomic::{AtomicUsize, Ordering},
};

use lucina_fdt::Fdt;

/// PL011 base address, 0 when there is none. Relaxed atomics are plain LDR/STR (see `arch::psci_init`).
static PL011: AtomicUsize = AtomicUsize::new(0);

const UARTDR: usize = 0x00;
const UARTFR: usize = 0x18;
const FR_TXFF: u32 = 1 << 5;

/// Finds the console: `stdout-path` (options after `:` dropped, an alias resolved through `/aliases`) naming a
/// node compatible with `arm,pl011`, at the first address of its `reg`.
pub fn init(fdt: &Fdt<'_>) {
    if let Some(base) = find_pl011(fdt) {
        PL011.store(base as usize, Ordering::Relaxed);
    }
}

fn find_pl011(fdt: &Fdt<'_>) -> Option<u64> {
    let chosen = fdt.chosen().ok()??;
    let spec = chosen.stdout_path?;
    let name = spec.split(':').next()?;
    let path = if name.starts_with('/') {
        name
    } else {
        fdt.property("/aliases", name).ok()??.as_str().ok()?
    };
    let node = fdt.node(path).ok()??;
    let pl011 = node
        .property("compatible")
        .ok()??
        .strings()
        .any(|s| s == Ok("arm,pl011"));
    if !pl011 {
        return None;
    }
    // `reg` is in the parent's address cells; LucinaBoot assumes identity `ranges` above it, as on QEMU virt.
    let parent = match path.trim_end_matches('/').rsplit_once('/') {
        Some(("", _)) | None => fdt.root().ok()?,
        Some((p, _)) => fdt.node(p).ok()??,
    };
    let (ac, sc) = (parent.address_cells().ok()?, parent.size_cells().ok()?);
    Some(node.reg(ac, sc).ok()?.next()?.base)
}

pub struct Console;

impl Write for Console {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        let base = PL011.load(Ordering::Relaxed);
        if base == 0 {
            return Ok(());
        }
        for b in s.bytes() {
            if b == b'\n' {
                put(base, b'\r');
            }
            put(base, b);
        }
        Ok(())
    }
}

fn put(base: usize, b: u8) {
    // SAFETY: `base` is the PL011 the device tree names; with the MMU off these are Device accesses to it.
    unsafe {
        while core::ptr::read_volatile((base + UARTFR) as *const u32) & FR_TXFF != 0 {}
        core::ptr::write_volatile((base + UARTDR) as *mut u32, u32::from(b));
    }
}

#[macro_export]
macro_rules! println {
    ($($arg:tt)*) => {{
        use core::fmt::Write as _;
        let _ = writeln!($crate::console::Console, $($arg)*);
    }};
}
