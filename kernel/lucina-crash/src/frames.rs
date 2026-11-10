//! The frames of a backtrace as the crash screen shows them: named from the kernel's own table (`lucina-ksyms`), and cut
//! to the ones that say where it went wrong. The saved record and the console keep every address.

use core::fmt;

/// One return address and the function it is in, if the kernel's table names it.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Frame<'a> {
    pub pc: u64,
    pub name: Option<&'a str>,
    /// How far into the function.
    pub offset: u64,
}

impl fmt::Display for Frame<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.name {
            Some(name) => write!(f, "{name}+{:#x}", self.offset),
            None => write!(f, "{:016x}", self.pc),
        }
    }
}

/// The panic and unwind machinery, which every panic's backtrace starts with: the crash path itself, the panic handlers,
/// `core::panicking`, the `unwrap` and `expect` failures and the allocation-failure handlers. What called them is the
/// first frame that matters.
const MACHINERY: &[&str] = &[
    "ostd::crash::",
    "ostd::panic::",
    "__ostd_panic_handler",
    "aster_kernel::panic",
    "aster_core::panic",
    "aster_core::thread::oops::panic_handler",
    "rust_begin_unwind",
    "__rustc::",
    "rust_panic",
    "core::panicking::",
    "core::panic::",
    "core::result::unwrap_failed",
    "core::option::unwrap_failed",
    "core::option::expect_failed",
    "core::option::Option::unwrap",
    "core::option::Option::expect",
    "core::result::Result::unwrap",
    "core::result::Result::expect",
    "core::cell::panic_already",
    "alloc::alloc::handle_alloc_error",
    "alloc::alloc::rust_oom",
    "alloc::raw_vec::handle_error",
    "__rust_",
    "__rg_oom",
    "__rdl_oom",
    "unwinding::",
];

/// Where a kernel thread or a trap entered, and the catch around a user task's body: nothing below says more.
const ENTRY: &[&str] = &[
    "unwinding::panic::catch_unwind",
    "aster_core::thread::oops::catch_panics_as_oops",
    "trap_handler",
    "trap_common",
    "_trap_from_kernel",
    "_trap_from_user",
    "__vectors",
    "early_trap",
    "ostd::arch::trap::",
    "kernel_task_entry",
    "ostd::task::kernel_task_entry",
];

/// What a thread's start goes through just above its entry.
const START: &[&str] = &["core::ops::function::FnOnce::call_once", "<alloc::boxed::Box"];

fn is_any(frame: &Frame<'_>, prefixes: &[&str]) -> bool {
    frame.name.is_some_and(|name| {
        // A qualified path (`<T as Trait>::f`) is matched on what is inside.
        let inner = name.strip_prefix('<').unwrap_or(name);
        prefixes.iter().any(|p| name.starts_with(p) || inner.starts_with(p))
    })
}

/// The frames that matter, innermost first: without the panic machinery at the top, and from the first trap or thread
/// entry down. Without names (no table) every frame stays; if nothing would be left, everything stays.
pub fn that_matter<'f, 'a>(frames: &'f [Frame<'a>]) -> &'f [Frame<'a>] {
    if frames.iter().all(|f| f.name.is_none()) {
        return frames;
    }
    let start = frames.iter().position(|f| !is_any(f, MACHINERY)).unwrap_or(frames.len());
    let mut end = frames[start..]
        .iter()
        .position(|f| is_any(f, ENTRY))
        .map_or(frames.len(), |i| start + i);
    // The closure-calling glue right above a thread's entry.
    if end < frames.len() {
        while end > start && is_any(&frames[end - 1], START) {
            end -= 1;
        }
    }
    if start >= end {
        return frames;
    }
    &frames[start..end]
}

#[cfg(test)]
mod tests {
    extern crate std;

    use super::*;

    fn named(names: &[&'static str]) -> [Frame<'static>; 12] {
        let mut out = [Frame::default(); 12];
        for (i, name) in names.iter().enumerate() {
            out[i] = Frame {
                pc: 0x1000 + i as u64 * 16,
                name: if name.is_empty() { None } else { Some(name) },
                offset: 4,
            };
        }
        out
    }

    fn names<'a>(frames: &'a [Frame<'a>]) -> impl Iterator<Item = Option<&'a str>> + 'a {
        frames.iter().map(|f| f.name)
    }

    #[test]
    fn a_heap_panic_starts_at_what_allocated() {
        let all = [
            "ostd::crash::crash",
            "__ostd_panic_handler",
            "rust_begin_unwind",
            "core::panicking::panic_fmt",
            "alloc::alloc::handle_alloc_error",
            "alloc::raw_vec::RawVec::grow_one",
            "smoltcp::storage::ring_buffer::RingBuffer::new",
            "aster_bigtcp::socket::unbound::new_tcp_socket",
            "aster_kernel::syscall::connect::sys_connect",
            "aster_kernel::syscall::handle_syscall",
            "core::ops::function::FnOnce::call_once",
            "kernel_task_entry",
        ];
        let frames = named(&all);
        let kept: std::vec::Vec<_> = names(that_matter(&frames)).collect();
        assert_eq!(
            kept,
            [
                Some("alloc::raw_vec::RawVec::grow_one"),
                Some("smoltcp::storage::ring_buffer::RingBuffer::new"),
                Some("aster_bigtcp::socket::unbound::new_tcp_socket"),
                Some("aster_kernel::syscall::connect::sys_connect"),
                Some("aster_kernel::syscall::handle_syscall"),
            ]
        );
    }

    #[test]
    fn a_fault_ends_at_the_trap_entry() {
        let frames = named(&[
            "ostd::crash::crash",
            "ostd::mm::fault::handle_kernel_page_fault",
            "aster_core::device::qcom_gpu::Gpu::irq",
            "ostd::irq::top_half::IrqLine::on_active",
            "trap_handler",
            "trap_common",
            "aster_kernel::sched::idle",
        ]);
        let kept: std::vec::Vec<_> = names(that_matter(&frames[..7])).collect();
        assert_eq!(
            kept,
            [
                Some("ostd::mm::fault::handle_kernel_page_fault"),
                Some("aster_core::device::qcom_gpu::Gpu::irq"),
                Some("ostd::irq::top_half::IrqLine::on_active"),
            ]
        );
    }

    /// The backtrace of QEMU's panic scenario, as the first trimmed screen showed it (2026-11-08).
    #[test]
    fn the_qemu_panic_keeps_the_syscall() {
        let frames = named(&[
            "aster_core::thread::oops::panic_handler",
            "__ostd_panic_handler",
            "__rustc::rust_begin_unwind",
            "core::panicking::panic_fmt",
            "<aster_core::fs::fs_impls::procfs::crash_test::CrashTestFileOps as aster_core::fs::FileOps>::write",
            "aster_core::syscall::write::sys_write",
            "aster_core::syscall::handle_syscall",
            "unwinding::panic::catch_unwind",
            "aster_core::thread::oops::catch_panics_as_oops",
            "<aster_core::thread::task::create_new_user_task::{closure#2} as core::ops::function::FnOnce>::call_once",
        ]);
        let kept: std::vec::Vec<_> = names(that_matter(&frames[..10])).collect();
        assert_eq!(
            kept,
            [
                Some("<aster_core::fs::fs_impls::procfs::crash_test::CrashTestFileOps as aster_core::fs::FileOps>::write"),
                Some("aster_core::syscall::write::sys_write"),
                Some("aster_core::syscall::handle_syscall"),
            ]
        );
    }

    #[test]
    fn qualified_paths_match_inside() {
        let frames = named(&["<core::panicking::Foo as core::fmt::Display>::fmt", "aster_kernel::f", "<alloc::boxed::Box<F> as core::ops::function::FnOnce>::call_once", "kernel_task_entry"]);
        let kept: std::vec::Vec<_> = names(that_matter(&frames[..4])).collect();
        assert_eq!(kept, [Some("aster_kernel::f")]);
    }

    #[test]
    fn without_names_or_with_nothing_left_everything_stays() {
        let bare = named(&["", "", ""]);
        assert_eq!(that_matter(&bare[..3]).len(), 3);
        let only_machinery = named(&["ostd::crash::crash", "rust_begin_unwind"]);
        assert_eq!(that_matter(&only_machinery[..2]).len(), 2);
        let only_entry = named(&["trap_handler", "trap_common"]);
        assert_eq!(that_matter(&only_entry[..2]).len(), 2);
        assert_eq!(that_matter(&[]).len(), 0);
        // An unnamed frame in the middle stays where it is.
        let mixed = named(&["core::panicking::panic", "", "aster_kernel::f"]);
        assert_eq!(that_matter(&mixed[..3]).len(), 2);
    }

    #[test]
    fn frames_print_with_their_offset() {
        let f = Frame { pc: 0xffff_ffff_a000_1234, name: Some("a::b"), offset: 0x1c };
        assert_eq!(std::format!("{f}"), "a::b+0x1c");
        let bare = Frame { name: None, ..f };
        assert_eq!(std::format!("{bare}"), "ffffffffa0001234");
    }
}
