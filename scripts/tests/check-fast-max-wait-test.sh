#!/usr/bin/env bash
# scripts/tests/check-fast-max-wait-test.sh: scripts/check-fast bounds its wait for a queue slot (`--max-wait 900`, or
# CHECK_FAST_SLOT_WAIT_S) when the queue's own usage offers `--max-wait`, and does not pass it to an older queue that does not.
# A stub `bq` prints a usage and records the arguments of `bq run`; a stub of `lucina-ci check-fast` (CHECK_FAST_BIN) prints a
# summary. Last line: `check-fast-max-wait-test: pass`.
set -euo pipefail
root=$(cd "$(dirname "$0")/../.." && pwd)
base=${CHECK_FAST_MAXWAIT_TEST_TMP:-${TMPDIR:-/tmp}}
mkdir -p "$base"
tmp=$(mktemp -d "$base/check-fast-maxwait.XXXXXX")
trap 'rm -rf "$tmp"' EXIT
fail() { printf 'FAIL: %s\n' "$*" >&2; exit 1; }

cat > "$tmp/bq" <<'EOF'
#!/usr/bin/env bash
# no arguments: the usage; `run ... -- cmd`: record the arguments, then run the command
if [ "$#" -eq 0 ]; then
    echo "usage: bq run --class short|long|proof [--lock PATH] ${STUB_BQ_EXTRA:-}-- cmd args..."
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

run_case() { # run_case <name> <extra usage text> [env assignments...]
    local name=$1 extra=$2
    shift 2
    env "$@" HOME="$tmp/home" TMPDIR="$tmp/reports" LUCINA_CHECK_TMP="$tmp/reports" PATH=/usr/bin:/bin \
        LUCINA_BQ="$tmp/bq" STUB_BQ_EXTRA="$extra" STUB_BQ_LOG="$tmp/$name.log" \
        CHECK_FAST_BIN="$tmp/stub" CARGO_TARGET_DIR="$tmp/target" LUCINA_CHECK_FREE_FLOOR_GIB=0 LUCINA_CHECK_ALLOW_TARGET=1 \
        "$root/scripts/check-fast" > "$tmp/$name.out" 2> "$tmp/$name.err" ||
        fail "($name) check-fast failed: $(cat "$tmp/$name.err")"
    [ -f "$tmp/$name.log" ] || fail "($name) the queue was never asked to run the check"
}

run_case new '[--max-wait SECONDS] ' X=1
grep -qF -- "--max-wait 900 --" "$tmp/new.log" || fail "a queue that offers --max-wait gets 900 seconds: $(cat "$tmp/new.log")"
run_case custom '[--max-wait SECONDS] ' CHECK_FAST_SLOT_WAIT_S=7
grep -qF -- "--max-wait 7 --" "$tmp/custom.log" || fail "CHECK_FAST_SLOT_WAIT_S sets the bound: $(cat "$tmp/custom.log")"
run_case old '' X=1
if grep -qF -- "--max-wait" "$tmp/old.log"; then fail "an older queue is not given an option it does not know: $(cat "$tmp/old.log")"; fi
grep -qF -- "--class short" "$tmp/old.log" || fail "the older queue still runs the check in a short slot: $(cat "$tmp/old.log")"
echo "check-fast-max-wait-test: pass"
