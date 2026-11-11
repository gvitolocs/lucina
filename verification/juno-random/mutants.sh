#!/usr/bin/env bash
# Shows that verification/juno-random/rules.rs can fail: each line plants one bug in a copy of the shipped files, and Verus has to
# refuse it. Run on buildhost from the repo root, under verify-gate.
set -uo pipefail
verus=${VERUS:-/home/user/tools/verus/verus-x86-linux/verus}
work=$(mktemp -d "${TMPDIR:-/tmp}/juno-random-mutants.XXXXXX")
trap 'rm -rf "${work:?}"' EXIT
fail=0
refused=0
weak=0
try() {
    local file=$1 from=$2 to=$3
    mkdir -p "$work/kernel/lucina-random/src" "$work/kernel/lucina-boot/src" "$work/verification/juno-random"
    cp kernel/lucina-boot/src/rng_marker.rs kernel/lucina-boot/src/trial.rs "$work/kernel/lucina-boot/src/"
    cp kernel/lucina-random/src/rules.rs kernel/lucina-random/src/seed_rules.rs "$work/kernel/lucina-random/src/"
    cp verification/juno-random/rules.rs "$work/verification/juno-random/"
    local target=$work/$file
    local text; text=$(<"$target")
    case "$text" in
        *"$from"*) ;;
        *) echo "NOT FOUND: $from"; fail=1; return ;;
    esac
    printf "%s\n" "${text/"$from"/"$to"}" > "$target"
    if cmp -s "$target" "$file"; then echo "NOT CHANGED: $from"; fail=1; return; fi
    if "$verus" "$work/verification/juno-random/rules.rs" > "$work/out" 2>&1; then
        echo "NOT REFUSED: $from -> $to"; fail=1
    else
        if grep -qE 'verification results|simplifies to|not satisfied|assertion failed|possible arithmetic|may not terminate|cannot show|recommendation not met' "$work/out" && ! grep -q 'error\[E' "$work/out"; then
            refused=$((refused + 1))
            echo "refused: $from -> $to ($(grep -c '^error' "$work/out") errors)"
        else
            echo "refused (NO VERIFICATION RUN: compile or other error): $from -> $to ($(grep -c '^error' "$work/out") errors)"
            weak=$((weak+1))
        fi
    fi
}
# The unchanged files must verify, or every "refused" below would mean nothing.
if "$verus" verification/juno-random/rules.rs > "$work/baseline" 2>&1; then
    echo "baseline: verifies"
else
    cat "$work/baseline"; echo "mutants: the unchanged files do not verify"; exit 1
fi

# Output before the generator is seeded.
try kernel/lucina-random/src/rules.rs '(_, Request::Waits) => Gate::Wait,' '(_, Request::Waits) => Gate::Hand,'
try kernel/lucina-random/src/rules.rs '(_, Request::DoesNotWait) => Gate::Again,' '(_, Request::DoesNotWait) => Gate::Hand,'
# Ready with too few bits, or the cap on one source gone.
try kernel/lucina-random/src/rules.rs 'pub const POOL_READY_BITS: u32 = 256;' 'pub const POOL_READY_BITS: u32 = 128;'
try kernel/lucina-random/src/rules.rs 'let add = if bits > POOL_BITS { POOL_BITS } else { bits };' 'let add = POOL_BITS;'
try kernel/lucina-random/src/rules.rs 'Credited { seeding: s, then: Then::Nothing }' 'Credited { seeding: s, then: Then::Reseed }'
# A wiped or untrusted seed that counts.
try kernel/lucina-random/src/rules.rs 'if !trusted || all_zero {' 'if !trusted {'
try kernel/lucina-random/src/rules.rs 'if !trusted || all_zero {' 'if all_zero {'
# Flags.
try kernel/lucina-random/src/rules.rs 'if flags & GRND_INSECURE != 0 && flags & GRND_RANDOM != 0 {' 'if false {'
# The seed used before it is replaced, after a failed replacement, or twice.
try kernel/lucina-random/src/seed_rules.rs 'Step::Mix
            } else if l.replace_failed {' 'Step::Done
            } else if l.replace_failed {'
try kernel/lucina-random/src/seed_rules.rs '} else if l.replaced {' '} else if !l.replaced {'
try kernel/lucina-random/src/seed_rules.rs 'if l.mixed {
                Step::Done' 'if l.mixed {
                Step::Mix'
try kernel/lucina-random/src/seed_rules.rs 'pub fn may_save(kernel_seeded: bool) -> bool {
    kernel_seeded' 'pub fn may_save(kernel_seeded: bool) -> bool {
    true'
# The loader's marker: a call after one that did not come back, from no record, or with a marker that is not what the next boot reads.
M=kernel/lucina-boot/src/rng_marker.rs
try $M 'return Plan::NoCall(Why::NoRecord, None);' 'return Plan::Call(Record { seq: 1, state: State::Calling });'
try $M 'Plan::NoCall(Why::DidNotReturn, Some(Record { seq: r.seq + 1, state: State::Skip }))' 'Plan::Call(Record { seq: r.seq + 1, state: State::Calling })'
try $M 'State::Skip => Plan::NoCall(Why::SkippedBefore, None),' 'State::Skip => Plan::Call(Record { seq: r.seq, state: State::Calling }),'
try $M 'Plan::Call(Record { seq: r.seq + 1, state: State::Calling })' 'Plan::Call(Record { seq: r.seq, state: State::Calling })'
try $M 'Plan::Call(Record { seq: r.seq + 1, state: State::Calling })' 'Plan::Call(Record { seq: r.seq + 1, state: State::Clear })'
try $M 'Record { seq: marker.seq + 1, state: State::Clear }' 'Record { seq: marker.seq, state: State::Clear }'
try $M 'if x.seq >= y.seq {' 'if x.seq < y.seq {'
echo "mutants: $refused refused by verification, $weak refused without a verification run"
if [ "${MUTANTS_STRICT:-0}" = "1" ] && [ "$weak" -gt 0 ]; then exit 1; fi
exit $fail
