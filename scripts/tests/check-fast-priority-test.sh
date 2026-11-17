#!/usr/bin/env bash
# scripts/tests/check-fast-priority-test.sh: scripts/check-fast asks the queue for the gate priority (`--priority gate`) when it is
# run as a merge gate AND the queue's own usage offers the option, and calls every other case exactly as it did before. A stub `bq`
# prints the usage it is given (STUB_BQ_USAGE) and records the arguments of `bq run`; a stub of `lucina-ci check-fast`
# (CHECK_FAST_BIN) prints a summary, so nothing is built or queued. Last line: `check-fast-priority-test: pass`.
set -euo pipefail
root=$(cd "$(dirname "$0")/../.." && pwd)
base=${CHECK_FAST_PRIORITY_TEST_TMP:-${TMPDIR:-/tmp}}
mkdir -p "$base"
tmp=$(mktemp -d "$base/check-fast-priority.XXXXXX")
trap 'rm -rf "$tmp"' EXIT
fail() { printf 'FAIL: %s\n' "$*" >&2; exit 1; }

cat > "$tmp/bq" <<'EOF'
#!/usr/bin/env bash
# no arguments: the usage; `run ... -- cmd`: record the arguments, then run the command
if [ "$#" -eq 0 ]; then
    echo "$STUB_BQ_USAGE"
    exit 2
fi
echo "$*" > "$STUB_BQ_LOG"
while [ "$1" != "--" ]; do shift; done
shift
exec "$@"
EOF
chmod +x "$tmp/bq"
cat > "$tmp/stub" <<'EOF'
#!/usr/bin/env bash
[ "${1:-}" = check-fast ] || exit 0
echo "check-fast: 0s, 0 failed, 0 skipped"
EOF
chmod +x "$tmp/stub"
mkdir -p "$tmp/home" "$tmp/reports"

# The three queues a run can meet: today's (both options), one that knows the priority but not the bounded wait, and the one a
# merge-gate timed out behind 40 ordinary jobs of (no priority at all).
WITH_WAIT='usage: bq run --class short|long|proof [--lock PATH] [--cores N] [--exclusive] [--who NAME] [--priority gate|normal] [--max-wait SECONDS] -- cmd args...'
NO_WAIT='usage: bq run --class short|long|proof [--exclusive] [--who NAME] [--priority gate|normal] -- cmd args...'
NO_PRIORITY='usage: bq run --class short|long|proof [--exclusive] [--who NAME] [--max-wait SECONDS] -- cmd args...'

run_case() { # run_case <name> <usage> <merge-gate 0|1>
    local name=$1 usage=$2 gate=$3
    shift 3
    local cfargs=(--base HEAD)
    if [ "$gate" = 1 ]; then cfargs+=(--merge-gate); fi
    env HOME="$tmp/home" TMPDIR="$tmp/reports" LUCINA_CHECK_TMP="$tmp/reports" PATH=/usr/bin:/bin \
        LUCINA_BQ="$tmp/bq" STUB_BQ_USAGE="$usage" STUB_BQ_LOG="$tmp/$name.log" \
        CHECK_FAST_BIN="$tmp/stub" CARGO_TARGET_DIR="$tmp/target" LUCINA_CHECK_FREE_FLOOR_GIB=0 LUCINA_CHECK_ALLOW_TARGET=1 \
        "$root/scripts/check-fast" "${cfargs[@]}" >"$tmp/$name.out" 2>"$tmp/$name.err" ||
        fail "($name) check-fast failed: $(cat "$tmp/$name.err")"
    [ -f "$tmp/$name.log" ] || fail "($name) the queue was never asked to run the check"
}

run_case gate_with_wait "$WITH_WAIT" 1
grep -qF -- "--priority gate --max-wait 900 --" "$tmp/gate_with_wait.log" ||
    fail "a merge gate goes first in its class and keeps the bounded slot wait: $(cat "$tmp/gate_with_wait.log")"

run_case check_with_wait "$WITH_WAIT" 0
if grep -qF -- "--priority" "$tmp/check_with_wait.log"; then
    fail "an ordinary check-fast run does not jump the queue: $(cat "$tmp/check_with_wait.log")"
fi
grep -qF -- "--max-wait 900 --" "$tmp/check_with_wait.log" ||
    fail "the ordinary run is called as it was: $(cat "$tmp/check_with_wait.log")"

run_case gate_no_wait "$NO_WAIT" 1
grep -qF -- "--priority gate --" "$tmp/gate_no_wait.log" ||
    fail "the queue that knows the priority but not --max-wait gets it too: $(cat "$tmp/gate_no_wait.log")"
if grep -qF -- "--max-wait" "$tmp/gate_no_wait.log"; then
    fail "an option the queue does not offer is not passed: $(cat "$tmp/gate_no_wait.log")"
fi

run_case gate_old_bq "$NO_PRIORITY" 1
if grep -qF -- "--priority" "$tmp/gate_old_bq.log"; then
    fail "a queue that does not know --priority is run exactly as before: $(cat "$tmp/gate_old_bq.log")"
fi
grep -qF -- "--max-wait 900 --" "$tmp/gate_old_bq.log" ||
    fail "the old queue still runs the check in a short slot with a bounded wait: $(cat "$tmp/gate_old_bq.log")"

# The checked command itself is the same in every case: the priority only sits among the queue options, before the `--`.
for n in gate_with_wait check_with_wait gate_no_wait gate_old_bq; do
    grep -qF -- "run --class short --who $(basename "$root")" "$tmp/$n.log" ||
        fail "($n) the queue lost the class and the session name: $(cat "$tmp/$n.log")"
    grep -qF -- " -- $tmp/stub check-fast --base HEAD" "$tmp/$n.log" ||
        fail "($n) the check command moved: $(cat "$tmp/$n.log")"
done

echo "check-fast-priority-test: pass"
