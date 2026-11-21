#!/usr/bin/env bash
# scripts/tests/check-fast-max-wait-test.sh: scripts/check-fast bounds its wait for a queue slot at 3600 s (`--max-wait 3600`,
# a run is one long job and may wait behind others), whatever CHECK_FAST_SLOT_WAIT_S says, and a queue whose usage does
# not offer `--max-wait` (the stub is <temp home>/.local/bin/bq, the passwd home of a copy of the launcher: make_launcher) fails the check, which never runs unqueued. A stub `bq` prints a usage and records the arguments of `bq run`; a stub of `lucina-ci
# check-fast` (CHECK_FAST_BIN) prints a summary. Last line: `check-fast-max-wait-test: pass`.
set -euo pipefail
root=$(cd "$(dirname "$0")/../.." && pwd)
base=${CHECK_FAST_MAXWAIT_TEST_TMP:-${TMPDIR:-/tmp}}
mkdir -p "$base"
tmp=$(mktemp -d "$base/check-fast-maxwait.XXXXXX")
trap 'rm -rf "$tmp"' EXIT
fail() { printf 'FAIL: %s\n' "$*" >&2; exit 1; }

# make_launcher <fake home> <dir>: a copy of scripts/check-fast at <dir>/scripts/check-fast whose one assignment of `qhome` (the
# passwd lookup of this uid's home, which a test cannot change) is a fixed directory. The folder is named like this repo, because
# --who is its basename. Nothing else differs: the copy is refused unless exactly that assignment (4 lines out, 1 in) changed.
make_launcher() {
    local fake=$1 d=$2
    mkdir -p "$d/scripts"
    awk -v h="$fake" '
        /^qhome=\$\($/ { print "qhome=\"" h "\""; skip = 1; next }
        skip && /^\) \|\| qhome=""$/ { skip = 0; next }
        skip { next }
        { print }' "$root/scripts/check-fast" > "$d/scripts/check-fast"
    chmod +x "$d/scripts/check-fast"
    [ "$(diff "$root/scripts/check-fast" "$d/scripts/check-fast" | grep -c '^[<>]')" -eq 5 ] ||
        fail "the launcher copy differs from the real one in more than its home assignment"
}

mkdir -p "$tmp/home/.local/bin"
cat > "$tmp/home/.local/bin/bq" <<'EOF2'
#!/usr/bin/env bash
# no arguments: the usage; `run ... -- cmd`: record the arguments, then run the command as a job of the queue
if [ "$#" -eq 0 ]; then
    echo "usage: bq run --class short|long|proof [--lock PATH] [--priority gate|normal] [--keep-env NAME] ${STUB_BQ_EXTRA:-}-- cmd args..."
    exit 2
fi
echo "$*" > "$STUB_BQ_LOG"
while [ "$1" != "--" ]; do shift; done
shift
h=$(cd "$(dirname "$0")/../.." && pwd)
mkdir -p "$h/.cache/bq"
printf 'pid=%s\nclass=long\n' "$$" > "$h/.cache/bq/long-0.meta"
export BQ_SLOT=long-0 BQ_SLOT_PID=$$

exec "$@"
EOF2
chmod +x "$tmp/home/.local/bin/bq"
cat > "$tmp/stub" <<'EOF2'
#!/usr/bin/env bash
[ "${1:-}" = check-fast ] || exit 0
echo "check-fast: 0s, 0 failed, 0 skipped"
EOF2
chmod +x "$tmp/stub"
mkdir -p "$tmp/reports"
chmod 755 "$tmp/reports"
who=$(basename "$root")
proot=$tmp/pl/$who
make_launcher "$tmp/home" "$proot"
launcher=$proot/scripts/check-fast

run_case() { # run_case <name> <extra usage text> [env assignments...]
    local name=$1 extra=$2
    shift 2
    rc=0
    env -u BQ_SLOT -u BQ_SLOT_PID "$@" HOME="$tmp/home" TMPDIR="$tmp/reports" LUCINA_CHECK_TMP="$tmp/reports" PATH=/usr/bin:/bin \
        STUB_BQ_EXTRA="$extra" STUB_BQ_LOG="$tmp/$name.log" \
        CHECK_FAST_BIN="$tmp/stub" CARGO_TARGET_DIR="$tmp/target" LUCINA_CHECK_FREE_FLOOR_GIB=0 LUCINA_CHECK_ALLOW_TARGET=1 \
        "$launcher" > "$tmp/$name.out" 2> "$tmp/$name.err" || rc=$?
    if [ "${EXPECT_REFUSED:-}" = 1 ]; then
        [ "$rc" -eq 2 ] || fail "($name) a queue without --max-wait must refuse with exit 2, got $rc"
    else
        [ "$rc" -eq 0 ] || fail "($name) check-fast failed: $(cat "$tmp/$name.err")"
    fi
}

run_case new '[--max-wait SECONDS] ' X=1
[ -f "$tmp/new.log" ] || fail "the queue was never asked to run the check"
[ "$(cat "$tmp/new.log")" = "run --class long --priority normal --max-wait 3600 --keep-env GIT_CONFIG_COUNT --who check-fast:$who -- $launcher --queued-by-bq=1" ] || fail "a queue that offers --max-wait gets 3600 seconds: $(cat "$tmp/new.log")"
run_case custom '[--max-wait SECONDS] ' CHECK_FAST_SLOT_WAIT_S=7
[ "$(cat "$tmp/custom.log")" = "$(cat "$tmp/new.log")" ] || fail "the bound is not an environment knob: $(cat "$tmp/custom.log")"
EXPECT_REFUSED=1 run_case old '' X=1
if [ -f "$tmp/old.log" ]; then fail "a queue that does not offer --max-wait was used: $(cat "$tmp/old.log")"; fi
if grep -qF "check-fast: 0s" "$tmp/old.out"; then fail "the check ran unqueued: $(cat "$tmp/old.out")"; fi
grep -qF "not our build queue" "$tmp/old.err" || fail "the refusal does not say why: $(cat "$tmp/old.err")"
echo "check-fast-max-wait-test: pass"
