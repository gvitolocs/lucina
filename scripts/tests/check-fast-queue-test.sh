#!/usr/bin/env bash
# scripts/tests/check-fast-queue-test.sh: scripts/check-fast runs as one job of the build queue's `long` class, so several runs do not overload the host.
# The queue is looked up in the passwd home of this uid (never $HOME): <home>/.local/bin/bq first, which MUST be ours or the check
# fails with exit 2; a `bq` on PATH only when that file does not exist. The launcher always re-runs itself through `bq run
# --class long`, marking the job's call with --queued-by-bq; no environment variable decides whether it queues. A stub `bq` prints
# our usage, records its argv, runs a call of the class of the slot an ancestor really holds right there (as bq does) and any
# other call in a slot of its own, and execs what follows the `--` with BQ_SLOT and BQ_SLOT_PID set. The passwd home is replaced
# by a fixed directory in a copy of the launcher (make_launcher below); `lucina-ci check-fast` is a stub (CHECK_FAST_BIN): nothing is built.
#   (1) a plain run queues once with the byte-exact call: class long, priority normal, --max-wait 3600 (the merge gate: 300, tied to the Rust budget by budget.rs), the job's call marked;
#   (2) --merge-gate asks for priority gate and --max-wait 300;
#   (3) a call nested in a job of the SAME class runs in place once; of ANOTHER class it queues in a slot of its own; neither loops;
#   (4) a hand-set BQ_SLOT / BQ_SLOT_PID / BQ_PRIORITY decides nothing: the run still queues;
#   (5) no bq anywhere (neither in the passwd home nor on PATH): runs directly and the first line says so;
#   (6) a bq of the wrong program (the Google Cloud SDK's) on PATH, none in the home, is not taken;
#   (7) the home's bq exists but is not ours: the check fails with exit 2 and runs nothing;
#   (8) an ours-looking bq first on PATH does not displace the home's; it is used only when the home has none;
#   (9) LUCINA_BQ is not read;
#   (10) $HOME decides nothing: a HOME without a bq, or none, still queues through the passwd home's bq;
#   (11) a passwd home that cannot be resolved is a refusal with exit 2, not "no queue here";
#   (13) a typed --queued-by-bq is ignored unless bq really holds a slot for the run (a live ancestor holder, and a meta file in the
#        queue's state directory under the passwd home naming that pid): without one it queues; with a real one it runs in place;
#   (14) a queue that never holds a real slot sends the run round at most twice, then it stops with exit 2.
#   (12) LUCINA_CHECK_TMP (where the exec'd private copy lives) writable by group or others is a refusal with exit 2.
# Last line: `check-fast-queue-test: pass`.
set -euo pipefail
root=$(cd "$(dirname "$0")/../.." && pwd)
base=${CHECK_FAST_QUEUE_TEST_TMP:-${TMPDIR:-/tmp}}
mkdir -p "$base"
tmp=$(mktemp -d "$base/check-fast-queue.XXXXXX")
sleeper=""
cleanup() {
    [ -z "$sleeper" ] || kill "$sleeper" 2>/dev/null || true
    rm -rf "$tmp"
}
trap cleanup EXIT
fail() { printf 'FAIL: %s\n' "$*" >&2; exit 1; }
ncalls() { wc -l < "$1" | tr -d ' '; }
who=$(basename "$root")

BQ_USAGE='usage: bq run --class short|long|proof [--lock PATH] [--cores N] [--mem 8G|none] [--exclusive] [--who NAME] [--priority gate|normal] [--max-wait SECONDS] [--keep-env NAME] -- cmd args...'
mkdir -p "$tmp/base" "$tmp/state" "$tmp/gcloud" "$tmp/pathbq" "$tmp/other"
cat > "$tmp/stub.tpl" <<'STUB'
#!/usr/bin/env bash
# A queue in the shape of tools/bq. No arguments: the usage. `run`: record the call (first word: @TAG@), and then, as bq does, run
# a call of the SAME class as the slot an ancestor really holds (its meta names this pid) right there, and any other call in a
# slot of its own; exec what follows the `--` with BQ_SLOT and BQ_SLOT_PID set.
if [ "$#" -eq 0 ]; then
    echo '@USAGE@'
    exit 2
fi
class=""
args=("$@")
for ((i = 0; i < ${#args[@]}; i++)); do
    if [ "${args[$i]}" = --class ]; then class=${args[$((i + 1))]}; fi
done
nested=0
if [ -n "${BQ_SLOT:-}" ] && [ "$BQ_SLOT" = "$class-0" ] && [ -f "$STUB_BQ_STATE/$BQ_SLOT.meta" ] &&
    grep -qx "pid=${BQ_SLOT_PID:-}" "$STUB_BQ_STATE/$BQ_SLOT.meta" && grep -qx "class=$class" "$STUB_BQ_STATE/$BQ_SLOT.meta" && [ -d "/proc/${BQ_SLOT_PID:-0}" ]; then
    nested=1
fi
echo "@TAG@ $*" >> "$STUB_BQ_LOG"
if [ "$nested" = 1 ]; then echo "@TAG@ in-place $class" >> "$STUB_BQ_LOG"; fi
while [ "$1" != "--" ]; do shift; done
shift
if [ "$nested" = 0 ]; then
    if [ -z "${STUB_NO_META:-}" ]; then
        mkdir -p "$STUB_BQ_STATE"
        printf 'pid=%s\nclass=%s\n' "$$" "$class" > "$STUB_BQ_STATE/$class-0.meta"
    fi
    export BQ_SLOT="$class-0" BQ_SLOT_PID=$$
fi
exec "$@"
STUB
make_stub() { # make_stub <file> <tag>
    sed -e "s#@TAG@#$2#g" -e "s#@USAGE@#$BQ_USAGE#" "$tmp/stub.tpl" > "$1"
    chmod +x "$1"
}
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
# mkhome <dir> [bq <tag> | badbq]: a home directory for a launcher copy, with the stand-in tool the launcher installs and runs.
mkhome() {
    mkdir -p "$1/.local/bin" "$1/.cache/bq"
    case "${2:-}" in
        bq) make_stub "$1/.local/bin/bq" "$3" ;;
        badbq)
            printf '#!/usr/bin/env bash\necho "ERROR: (bq) You do not currently have an active account selected." >&2\nexit 1\n' > "$1/.local/bin/bq"
            chmod +x "$1/.local/bin/bq"
            ;;
    esac
}
make_stub "$tmp/pathbq/bq" path
mkdir -p "$tmp/gcloud"
printf '#!/usr/bin/env bash\necho "ERROR: (bq) You do not currently have an active account selected." >&2\nexit 1\n' > "$tmp/gcloud/bq"
chmod +x "$tmp/gcloud/bq"
cat > "$tmp/stub" <<'STUB'
#!/usr/bin/env bash
[ "${1:-}" = check-fast ] || exit 0
echo "stub-ran check-fast"
echo "check-fast: 0s, 0 failed, 0 skipped"
STUB
chmod +x "$tmp/stub"
mkdir -p "$tmp/reports"
chmod 755 "$tmp/reports"

mkhome "$tmp/home" bq home
mkhome "$tmp/nohome"
mkhome "$tmp/badhome" badbq
mkhome "$tmp/pathhome"
for h in home nohome badhome pathhome; do make_launcher "$tmp/$h" "$tmp/pl-$h/$who"; done
launcher_for() { echo "$tmp/pl-$1/$who/scripts/check-fast"; }

# launch <name> <home name> <dirs first on PATH> [env assignment...] -- <command...>; leaves rc, out, err, calls.
launch() {
    local name=$1 h=$2 first=$3
    shift 3
    local envs=()
    while [ "$#" -gt 0 ] && [ "$1" != -- ]; do envs+=("$1"); shift; done
    shift
    calls=$tmp/$name.calls
    : > "$calls"
    rc=0
    env -u BQ_SLOT -u BQ_SLOT_PID -u BQ_PRIORITY -u LUCINA_BQ -u CHECK_FAST_FILTER_ONLY ${envs[@]+"${envs[@]}"} \
        PATH="$first:/usr/bin:/bin" STUB_BQ_LOG="$calls" STUB_BQ_STATE="$tmp/$h/.cache/bq" \
        CHECK_FAST_BIN="$tmp/stub" TMPDIR="$tmp/reports" LUCINA_CHECK_TMP="$tmp/reports" CARGO_TARGET_DIR="$tmp/target" \
        LUCINA_CHECK_FREE_FLOOR_GIB=0 LUCINA_CHECK_ALLOW_TARGET=1 \
        "$@" > "$tmp/$name.out" 2> "$tmp/$name.err" || rc=$?
    out=$tmp/$name.out
    err=$tmp/$name.err
}
# ran_ok <case>: the last launch exited 0 and the check ran exactly once.
ran_ok() {
    [ "$rc" -eq 0 ] || fail "($1) exit $rc: $(cat "$err")"
    [ "$(grep -c "stub-ran check-fast" "$out")" = 1 ] || fail "($1) the check did not run exactly once: out=$(cat "$out") err=$(cat "$err")"
}
# refused <case>: the last launch exited 2 and ran nothing.
refused() {
    [ "$rc" -eq 2 ] || fail "($1) expected the refusal's exit code 2, got $rc: $(cat "$err")"
    if grep -q "stub-ran" "$out"; then fail "($1) the check ran: $(cat "$out")"; fi
}
home=$tmp/home
L=$(launcher_for home)

# (1) a plain run: one call, byte-exact.
launch plain home "$tmp/base" -- "$L" --base HEAD
ran_ok plain
want="home run --class long --priority normal --max-wait 3600 --keep-env GIT_CONFIG_COUNT --who check-fast:$who -- $L --queued-by-bq=1 --base HEAD"
[ "$(cat "$calls")" = "$want" ] || fail "(1) the queue call is not byte-exact: $(cat "$calls")"
echo "ok: (1) a plain run queues once with the byte-exact call, and the job's own run does not queue again"

# (2) the merge gate.
launch gate home "$tmp/base" -- "$L" --base HEAD^1 --merge-gate
ran_ok gate
want="home run --class long --priority gate --max-wait 300 --keep-env GIT_CONFIG_COUNT --who check-fast:$who -- $L --queued-by-bq=1 --base HEAD^1 --merge-gate"
[ "$(cat "$calls")" = "$want" ] || fail "(2) the gate call is not byte-exact: $(cat "$calls")"
echo "ok: (2) --merge-gate asks for priority gate and a 300 s wait, byte-exact"

# (3) nested. The outer stub call stands for a worker's `bq run -- scripts/check-fast`; `bash -c '...; exit'` keeps bash from
# exec'ing the launcher, so the slot's holder is an ancestor of it. Same class: the launcher's own call runs in place (once), and
# the run does not loop. Another class: the call queues in a slot of its own.
launch nested home "$tmp/base" -- "$home/.local/bin/bq" run --class long -- bash -c '"$@"; exit $?' sh "$L" --base HEAD
ran_ok nested
[ "$(grep -c '^home in-place long$' "$calls")" = 1 ] || fail "(3) a nested call of the same class did not run in place exactly once: $(cat "$calls")"
[ "$(grep -c -- "--queued-by-bq" "$calls")" = 1 ] || fail "(3) the launcher called the queue other than once: $(cat "$calls")"
launch nested2 home "$tmp/base" -- "$home/.local/bin/bq" run --class short -- bash -c '"$@"; exit $?' sh "$L" --base HEAD
ran_ok nested2
if grep -q 'in-place' "$calls"; then fail "(3) a nested call of another class ran in place: $(cat "$calls")"; fi
grep -qxF "home run --class long --priority normal --max-wait 3600 --keep-env GIT_CONFIG_COUNT --who check-fast:$who -- $L --queued-by-bq=1 --base HEAD" "$calls" ||
    fail "(3) the nested call of another class did not queue as class long: $(cat "$calls")"
echo "ok: (3) nested in the same class runs in place once, in another class queues in its own slot; neither loops"

# (4) hand-set BQ_SLOT / BQ_SLOT_PID / BQ_PRIORITY decide nothing: a dead holder, a live non-ancestor, no holder.
dead=$(sh -c 'echo $$')
launch dead home "$tmp/base" BQ_SLOT=long-0 "BQ_SLOT_PID=$dead" -- "$L" --base HEAD
ran_ok dead
[ "$(grep -c '^home run ' "$calls")" = 1 ] || fail "(4) BQ_SLOT with a dead holder: $(cat "$calls")"
if grep -q in-place "$calls"; then fail "(4) a dead holder ran in place: $(cat "$calls")"; fi
sleep 60 &
sleeper=$!
launch alive home "$tmp/base" BQ_SLOT=long-0 "BQ_SLOT_PID=$sleeper" -- "$L" --base HEAD
ran_ok alive
[ "$(grep -c '^home run ' "$calls")" = 1 ] || fail "(4) BQ_SLOT naming a live non-ancestor: $(cat "$calls")"
if grep -q in-place "$calls"; then fail "(4) a live non-ancestor ran in place: $(cat "$calls")"; fi
launch noslotpid home "$tmp/base" BQ_SLOT=long-0 -- "$L" --base HEAD
ran_ok noslotpid
[ "$(grep -c '^home run ' "$calls")" = 1 ] || fail "(4) BQ_SLOT alone: $(cat "$calls")"
launch prio home "$tmp/base" BQ_PRIORITY=gate -- "$L" --base HEAD
ran_ok prio
grep -qF -- "--priority normal --max-wait 3600 --keep-env" "$calls" || fail "(4) BQ_PRIORITY changed the call: $(cat "$calls")"
echo "ok: (4) a hand-set BQ_SLOT (dead holder, live non-ancestor, no holder) or BQ_PRIORITY changes nothing: still one queued call"

# (5) no bq anywhere: runs directly, and the first line of output says so.
L2=$(launcher_for nohome)
launch none nohome "$tmp/base" -- bash -c '"$@" 2>&1' sh "$L2" --base HEAD
ran_ok none
[ "$(ncalls "$calls")" = 0 ] || fail "(5) no queue exists, yet it was called: $(cat "$calls")"
first=$(head -n 1 "$out")
[ "$first" = "check-fast: no bq on this host; running without the queue" ] || fail "(5) the first line of output is: $first"
echo "ok: (5) without any bq the check runs directly and its first line says so"

# (6) the other program called bq, on PATH, with none in the home: not taken, and the same line.
launch gcloud nohome "$tmp/gcloud:$tmp/base" -- bash -c '"$@" 2>&1' sh "$L2" --base HEAD
ran_ok gcloud
head -n 1 "$out" | grep -qF "no bq on this host" || fail "(6) a bq that is not ours was taken: $(cat "$out")"
if grep -qF "active account" "$out"; then fail "(6) the other bq's error reached the reader: $(cat "$out")"; fi
echo "ok: (6) a bq on PATH that is not our queue is not taken"

# (7) the home's bq exists and is not ours: exit 2, a message, nothing runs, never a direct run.
launch badhome badhome "$tmp/pathbq:$tmp/base" -- "$(launcher_for badhome)" --base HEAD
refused badhome
[ "$(ncalls "$calls")" = 0 ] || fail "(7) a queue was called: $(cat "$calls")"
grep -qF "not our build queue" "$err" || fail "(7) the refusal does not say why: $(cat "$err")"
echo "ok: (7) a home bq that is not ours fails the check with exit 2 and runs nothing"

# (8) an ours-looking bq first on PATH does not displace the home's.
launch pathfirst home "$tmp/pathbq:$tmp/base" -- "$L" --base HEAD
ran_ok pathfirst
grep -q "^home run " "$calls" || fail "(8) the bq in the home was not used: $(cat "$calls")"
if grep -q "^path " "$calls"; then fail "(8) the bq on PATH was used as well: $(cat "$calls")"; fi
launch pathonly pathhome "$tmp/pathbq:$tmp/base" -- "$(launcher_for pathhome)" --base HEAD
ran_ok pathonly
grep -q "^path run " "$calls" || fail "(8) with no bq in the home the one on PATH is used: $(cat "$calls")"
echo "ok: (8) the home's bq comes first; a bq on PATH only when the home has none"

# (9) LUCINA_BQ is not read: it neither supplies a queue nor displaces the real one.
make_stub "$tmp/other/bq" other
launch envbq nohome "$tmp/base" LUCINA_BQ="$tmp/other/bq" -- bash -c '"$@" 2>&1' sh "$L2" --base HEAD
ran_ok envbq
[ "$(ncalls "$calls")" = 0 ] || fail "(9) LUCINA_BQ supplied a queue: $(cat "$calls")"
launch envbq2 home "$tmp/base" LUCINA_BQ="$tmp/other/bq" -- "$L" --base HEAD
ran_ok envbq2
grep -q "^home run " "$calls" || fail "(9) the queue in the home was not used: $(cat "$calls")"
if grep -q "^other " "$calls"; then fail "(9) LUCINA_BQ displaced the queue in the home: $(cat "$calls")"; fi
echo "ok: (9) LUCINA_BQ is not read"

# (10) $HOME decides nothing: a HOME with no bq, no HOME at all, and a HOME with a bq of its own that is not the passwd home's.
launch homeenv home "$tmp/base" "HOME=$tmp/nohome" -- "$L" --base HEAD
ran_ok homeenv
grep -q "^home run " "$calls" || fail "(10) HOME=<dir without bq> skipped the queue: $(cat "$calls")"
launch nohomeenv home "$tmp/base" -- env -u HOME "$L" --base HEAD
ran_ok nohomeenv
grep -q "^home run " "$calls" || fail "(10) an unset HOME skipped the queue: $(cat "$calls")"
launch homeother nohome "$tmp/base" "HOME=$tmp/home" -- bash -c '"$@" 2>&1' sh "$L2" --base HEAD
ran_ok homeother
[ "$(ncalls "$calls")" = 0 ] || fail "(10) HOME named a queue: $(cat "$calls")"
echo "ok: (10) \$HOME decides nothing: the passwd home's bq is the one looked up"

# (11) a passwd home that cannot be resolved (empty, or not a directory) is a refusal with exit 2.
make_launcher "" "$tmp/pl-empty/$who"
make_launcher "$tmp/does-not-exist" "$tmp/pl-gone/$who"
for h in empty gone; do
    launch "res-$h" home "$tmp/base" -- "$(launcher_for "$h")" --base HEAD
    refused "res-$h"
    [ "$(ncalls "$calls")" = 0 ] || fail "(11) a queue was called: $(cat "$calls")"
    grep -qF "cannot resolve the home directory" "$err" || fail "(11) the refusal does not say why: $(cat "$err")"
done
echo "ok: (11) an unresolvable passwd home is a refusal with exit 2"

# (12) the temp directory the private copy of the tool is exec'd from must not be open to group or others.
mkdir -p "$tmp/loose"
chmod 775 "$tmp/loose"
launch loose home "$tmp/base" -- env LUCINA_CHECK_TMP="$tmp/loose" "$L" --base HEAD
refused loose
[ "$(ncalls "$calls")" = 1 ] || fail "(12) the job was not queued exactly once (the refusal is the job's own): $(cat "$calls")"
grep -qF "writable by group or others" "$err" || fail "(12) the refusal does not say why: $(cat "$err")"
echo "ok: (12) a group-writable LUCINA_CHECK_TMP is a refusal with exit 2"

# (13) a typed --queued-by-bq decides nothing unless bq really holds a slot for the run. The state directory is the passwd home's
# .cache/bq. Each case must end queued exactly once, with the check run once.
queued_once() { # queued_once <case> [depth]: the one queue call this run made, byte for byte, and the check ran once
    ran_ok "$1"
    [ "$(grep -c '^home run ' "$calls")" = 1 ] || fail "($1) expected exactly one queue call: $(cat "$calls")"
    grep -qxF "home run --class long --priority normal --max-wait 3600 --keep-env GIT_CONFIG_COUNT --who check-fast:$who -- $L --queued-by-bq=${2:-1} --base HEAD" "$calls" ||
        fail "($1) the call is not the byte-exact one: $(cat "$calls")"
    if grep -q in-place "$calls"; then fail "($1) it ran in place: $(cat "$calls")"; fi
}
rm -f "$tmp/home/.cache/bq/"*.meta
launch typed home "$tmp/base" -- "$L" --queued-by-bq --base HEAD
queued_once typed
launch typed1 home "$tmp/base" -- "$L" --queued-by-bq=1 --base HEAD
queued_once typed1 2
launch typed2 home "$tmp/base" -- "$L" --queued-by-bq=7 --base HEAD
refused typed2 # a typed count past the second trip is stopped by the guard, never run
grep -qF "queued twice" "$err" || fail "(13) typed2: the refusal does not say why: $(cat "$err")"
# a slot named and a live ancestor, but no meta file
rm -f "$tmp/home/.cache/bq/"*.meta
launch nometa home "$tmp/base" BQ_SLOT=long-0 "BQ_SLOT_PID=$$" -- "$L" --queued-by-bq --base HEAD
queued_once nometa
# a meta file that names another pid
mkdir -p "$tmp/home/.cache/bq"
printf 'pid=%s\nclass=long\n' "$dead" > "$tmp/home/.cache/bq/long-0.meta"
launch otherpid home "$tmp/base" BQ_SLOT=long-0 "BQ_SLOT_PID=$$" -- "$L" --queued-by-bq --base HEAD
queued_once otherpid
# a meta file for this pid but no class bq knows
printf 'pid=%s\nclass=bogus\n' "$$" > "$tmp/home/.cache/bq/long-0.meta"
launch noclass home "$tmp/base" BQ_SLOT=long-0 "BQ_SLOT_PID=$$" -- "$L" --queued-by-bq --base HEAD
queued_once noclass
# a meta file in a state directory the launcher does not read (BQ_DIR, HOME) is not looked at
mkdir -p "$tmp/elsewhere"
printf 'pid=%s\nclass=long\n' "$$" > "$tmp/elsewhere/long-0.meta"
launch elsewhere home "$tmp/base" BQ_SLOT=long-0 "BQ_SLOT_PID=$$" BQ_DIR="$tmp/elsewhere" "HOME=$tmp" -- "$L" --queued-by-bq --base HEAD
queued_once elsewhere
rm -f "$tmp/home/.cache/bq/"*.meta
# a real slot, made by the stub-bq nested run that writes the meta: the call runs in place, once
launch real home "$tmp/base" -- "$home/.local/bin/bq" run --class long -- bash -c '"$@"; exit $?' sh "$L" --base HEAD
ran_ok real
[ "$(grep -c '^home in-place long$' "$calls")" = 1 ] || fail "(13) a real nested slot did not run in place exactly once: $(cat "$calls")"
echo "ok: (13) a typed --queued-by-bq is ignored unless bq really holds a slot for the run; a real slot runs in place"

# (14) a queue that never writes a slot record: the run goes round at most twice, then stops with exit 2 and runs nothing.
launch noslot home "$tmp/base" STUB_NO_META=1 -- "$L" --base HEAD
refused noslot
[ "$(grep -c '^home run ' "$calls")" = 2 ] || fail "(14) expected exactly two trips through the queue: $(cat "$calls")"
grep -qF "queued twice" "$err" || fail "(14) the refusal does not say why: $(cat "$err")"
echo "ok: (14) a queue that holds no real slot is gone through twice, then the run stops with exit 2"

echo "check-fast-queue-test: pass"
