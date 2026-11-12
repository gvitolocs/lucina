// Verus proof of the SHIPPED code, not a model (project rule D-011): this file only names the source file, and Verus checks it as the
// kernel compiles it. What is proved is in each function's `ensures` in services/lucina-surface-rules/src/rules.rs, and in `wf`,
// the state every accepted operation keeps: bytes are charged only while a buffer is live or quarantined; a freed or quarantined
// buffer has no reference and no pending GPU batch; a purged buffer is unmapped, bound to no slot and named by no batch; used <= limit. On top of that
// `step` returns the pool outside the buffer untouched (bytes come back once, a quarantined buffer's never), and a buffer is purged
// only when purgeable, unmapped and in no running batch. The pitch and size arithmetic cannot overflow and rounds as promised;
// a sync rectangle that `rows_ok` accepts lies inside the buffer.
// Since 2026-11-11 (prover's statements after review R-1..R-6): `step` and `new` are exact (`post`, `fresh`: only the named fields
// move) and total (`r is Ok <==> accepts(..)`, so Close/Unmap/GpuUnbind with their counter above zero are never refused); a GPU failure
// is never cleared and a live buffer of a failed GPU is never freed; `pool_ok` and the `lemma_pool_*` lemmas carry the budget over a
// pool of buffers (used == the sum of charged bytes <= limit, kept by every accepted step, by `new`, and by dropping a freed buffer);
// `rows_ok` is `Some` exactly when every rectangle is inside and the count fits, and the count is the sum of the rows.
// R-5 (2026-11-11/12): Purge is accepted only with no GPU slot bound and the GPU not failed (`accepts`, exact by `r is Ok <==> accepts`;
// and stated for every accepted step that purges), and a purged buffer is bound to no slot (in `wf`).
#![feature(proc_macro_hygiene)]
#[path = "../../services/lucina-surface-rules/src/rules.rs"]
pub mod rules;

fn main() {}
