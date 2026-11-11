# lucina-errseq

Linux's `errseq_t`: record a writeback error once in one place; every file holder sees it exactly once, a late
opener does not see an old, already-reported error.

* Source of truth: `lib/errseq.c` and `include/linux/errseq.h`, Linux **v6.12**, commit
  `adc218676eef25575469234709c2d87185ca223a` (tree `ac4266ccaf1cf79e8fb22ad3e0d86deac358ffb9`).
  Read from a sparse checkout in `/home/user/mnt/mybook/scratch/linux-ref/linux`.
* Original licence GPL-2.0 (SPDX line of `lib/errseq.c`). This crate is a port of Linux's errseq behaviour and is
  therefore under **GPL-2.0-only** (see Licence below). Porting record: **adapt** (same semantics, same bit
  layout, same refusal rules; Rust types and an `ErrSeq` struct around an `AtomicU32` instead of a bare pointer).
* `no_std`, no alloc, no dependencies, `forbid(unsafe_code)`.

## Semantics kept

* Layout: errno bits 0..=11 (`MAX_ERRNO` 4095), SEEN at bit 12, counter from bit 13.
* `set`: refuses 0 and anything outside -1..=-4095 and returns the current value; otherwise replaces the errno,
  clears SEEN, bumps the counter only if the old value was SEEN. Compare-exchange loop; success if another writer
  stored the same value.
* `sample`: 0 if the current value is not SEEN, else the value.
* `check`: 0 if equal to `since`, else the negative errno; changes nothing.
* `check_and_advance`: on a difference, one compare-exchange attempt to set SEEN (the outcome is ignored), advance
  `since` to the value with SEEN, return the errno.
* The counter wraps (19 bits); after a full wrap a stale sample can equal the current value and hide an error.

## Differences

* Linux warns (`WARN`) on a refused `set`; here it is silent.
* Orderings: Linux's `READ_ONCE` is a relaxed load, its `cmpxchg` is fully ordered: `SeqCst` on success here.

## Licence

This crate re-implements the behaviour of Linux's `lib/errseq.c` (v6.12, commit
`adc218676eef25575469234709c2d87185ca223a`), whose licence is GPL-2.0 (its SPDX line, `// SPDX-License-Identifier: GPL-2.0`;
in SPDX, `GPL-2.0` is the deprecated id for `GPL-2.0-only`). It is a port, not a clean-room work, so it is under
**GPL-2.0-only**. `Cargo.toml` and `src/lib.rs` carry that licence.

## Check

`cargo test -p lucina-errseq`
