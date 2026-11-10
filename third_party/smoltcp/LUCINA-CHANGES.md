# LucinaOS changes to smoltcp

This is the Asterinas fork of smoltcp 0.11.0, tag `r_2026-08-11_1b7629c` of `github.com/asterinas/smoltcp`, commit 56ab83798451, which is what the kernel used until now. Its first commit in this repository has only the files below and no dev-dependencies, examples or benches, because a workspace member's dev-dependencies would have to be in the kernel's `Cargo.lock`. `kernel/inject.txt` copies it into the kernel tree and the kernel's workspace depends on that copy by path: `[patch]` is ignored by OSDK's base crate. Licence: 0BSD. Offering a change upstream is a draft only, without the owner's OK.

**The shortest retransmission timeout is a setting (`set_min_rto_ms`).** It was the constant `RTTE_MIN_RTO`, 10 ms. On the Galaxy Book Go, 16 USB transfers queued before the RTL8152 made a sending laptop retransmit about a fifth of what it sent, none of it lost: a delay in our own queue that outgrew the estimate set off a timeout, and a timeout resends everything unacknowledged. Linux's minimum is 200 ms, RFC 6298 asks for 1 s. Behaviour reference only; nothing copied.

**A zero window probe no longer panics the kernel.** `last_scaled_window` subtracted sequence numbers without looking, and `SeqNumber`'s subtraction panics below zero. With a window scale, a few free bytes are rounded down and advertised as a window of 0, and a peer's one-byte probe then lands in them, past the last window advertised. Linux's `tcp_receive_window` clamps the same sum to 0, and so does this now. The laptop's crash on 2026-11-02 (`attempt to subtract sequence numbers with underflow`, wire/tcp.rs:81, while a 31 MB stream came in over SSH) has this panic's message; its backtrace could not be named, so this path is the likeliest one, not a proven one. The other unchecked subtractions in `socket/tcp.rs` are guarded by a comparison just before them.

**TCP's initial sequence numbers are RFC 6528's (`Config::isn_key`).** They came from `rand_u32`, a seeded sPCG32 whose output gives away what follows. With a secret key the number depends on the connection and a clock, as `secure_tcp_seq` in Linux. A kernel that leaves the key zero gets a predictable sequence: Asterinas's `aster-bigtcp` passes one from the kernel's generator (patch 0891).

**A ring buffer can be kept in pages (`RingBuffer::new_paged`), for TCP's socket buffers (kernel patch 1053).** A TCP socket's
receive and send buffers were one `vec!` each, 256 KiB on Juno: 64 physically contiguous pages through the kernel heap, and on
the laptop, after an hour and a half of uptime, `connect()` found none and the kernel panicked (boot 11, "Heap allocation
error"). Linux keeps no such block (a socket's data is in page-sized `sk_buff`s). `new_paged(capacity, page)` keeps the storage
as pages of their own, each made with `try_reserve_exact`, so a failure is an error the caller turns into ENOMEM. The ring's
behaviour is unchanged except that a slice it hands out also ends at a page end ("the largest contiguous slice" already allowed
that), and the copy functions (`enqueue_slice`, `dequeue_slice`, `write_unallocated`, `read_allocated`) go round as many
pieces as they need instead of exactly two. The page arithmetic is `storage/paged_math.rs`, which Verus proves
(`verification/juno-tcpbuf` in the LucinaOS repository, 10 verified, 6 planted bugs refused); the ring's own tests plus a model
test against a `VecDeque` (several thousand random operations on a dozen capacity/page shapes) check the rest, and the TCP tests
pass with 4 KiB pages. A one-slice ring (`RingBuffer::new`) behaves as before. Behaviour reference: Linux's page-sized socket
memory; nothing copied.
