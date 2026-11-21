#!/usr/bin/env bash
# scripts/tests/check-fast-priority-test.sh: scripts/check-fast, run as a merge gate, queues itself with `--priority gate` and every
# other run with `--priority normal`, always as one `long` job with the 3600 s bounded wait (scripts/tests/check-fast-queue-test.sh
# holds the rest of the queue contract). A stub `bq` prints the usage it is given (STUB_BQ_USAGE) and records the arguments of
# `bq run`, then execs the command with BQ_SLOT and BQ_SLOT_PID set as the real one does; a stub of `lucina-ci check-fast`
# (CHECK_FAST_BIN) prints a summary, so nothing is built or queued. The stub is <temp home>/.local/bin/bq, and the temp home is the passwd home of a
# copy of the launcher (make_launcher; the real one looks it up from the passwd entry, never $HOME). A queue there whose usage does not offer an option the launcher passes (`--priority`, `--max-wait`) is not
# one it can call as written: the check FAILS and never runs unqueued.
# Last line: `check-fast-priority-test: pass`.
set -euo pipefail
root=$(cd "$(dirname "$0")/../.." && pwd)
base=${CHECK_FAST_PRIORITY_TEST_TMP:-${TMPDIR:-/tmp}}
mkdir -p "$base"
tmp=$(mktemp -d "$base/check-fast-priority.XXXXXX")
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
    echo "$STUB_BQ_USAGE"
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

# The queues a run can meet: today's (every option), one that knows the priority but not the bounded wait, and the one a
# merge-gate timed out behind 40 ordinary jobs of (no priority at all).
FULL='usage: bq run --class short|long|proof [--lock PATH] [--cores N] [--exclusive] [--who NAME] [--priority gate|normal] [--max-wait SECONDS] [--keep-env NAME] -- cmd args...'
NO_WAIT='usage: bq run --class short|long|proof [--exclusive] [--who NAME] [--priority gate|normal] [--keep-env NAME] -- cmd args...'
NO_PRIORITY='usage: bq run --class short|long|proof [--exclusive] [--who NAME] [--max-wait SECONDS] [--keep-env NAME] -- cmd args...'

run_case() { # run_case <name> <usage> <merge-gate 0|1> [refused]
    local name=$1 usage=$2 gate=$3 want=${4:-ok}
    shift 3
    rc=0
    local cfargs=(--base HEAD)
    if [ "$gate" = 1 ]; then cfargs+=(--merge-gate); fi
    env -u BQ_SLOT -u BQ_SLOT_PID HOME="$tmp/home" TMPDIR="$tmp/reports" LUCINA_CHECK_TMP="$tmp/reports" PATH=/usr/bin:/bin \
        STUB_BQ_USAGE="$usage" STUB_BQ_LOG="$tmp/$name.log" \
        CHECK_FAST_BIN="$tmp/stub" CARGO_TARGET_DIR="$tmp/target" LUCINA_CHECK_FREE_FLOOR_GIB=0 LUCINA_CHECK_ALLOW_TARGET=1 \
        "$launcher" "${cfargs[@]}" >"$tmp/$name.out" 2>"$tmp/$name.err" || rc=$?
    if [ "$want" = refused ]; then
        [ "$rc" -eq 2 ] || fail "($name) a queue that cannot take the call as written must refuse with exit 2, got $rc"
    else
        [ "$rc" -eq 0 ] || fail "($name) check-fast failed: $(cat "$tmp/$name.err")"
    fi
}
queued() { [ -f "$tmp/$1.log" ] || fail "($1) the queue was never asked to run the check"; }
refused() {
    [ ! -f "$tmp/$1.log" ] || fail "($1) a queue that cannot take the call as written was used: $(cat "$tmp/$1.log")"
    if grep -qF "check-fast: 0s" "$tmp/$1.out"; then fail "($1) the check ran unqueued: $(cat "$tmp/$1.out")"; fi
    grep -qF "not our build queue" "$tmp/$1.err" || fail "($1) the refusal does not say why: $(cat "$tmp/$1.err")"
}

run_case gate_full "$FULL" 1
queued gate_full
[ "$(cat "$tmp/gate_full.log")" = "run --class long --priority gate --max-wait 300 --keep-env GIT_CONFIG_COUNT --who check-fast:$who -- $launcher --queued-by-bq=1 --base HEAD --merge-gate" ] ||
    fail "a merge gate goes first in its class and keeps the bounded slot wait: $(cat "$tmp/gate_full.log")"

run_case check_full "$FULL" 0
queued check_full
[ "$(cat "$tmp/check_full.log")" = "run --class long --priority normal --max-wait 3600 --keep-env GIT_CONFIG_COUNT --who check-fast:$who -- $launcher --queued-by-bq=1 --base HEAD" ] ||
    fail "an ordinary check-fast run does not jump the queue: $(cat "$tmp/check_full.log")"
if grep -qF -- "--priority gate" "$tmp/check_full.log"; then
    fail "an ordinary check-fast run asked for the gate priority: $(cat "$tmp/check_full.log")"
fi

run_case gate_no_wait "$NO_WAIT" 1 refused
refused gate_no_wait
run_case gate_old_bq "$NO_PRIORITY" 1 refused
refused gate_old_bq

echo "check-fast-priority-test: pass"
