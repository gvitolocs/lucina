#!/usr/bin/env bash
# scripts/tests/in-lane-worker-test.sh: tests scripts/lib/in-lane-worker.sh and the entry points that source it. Everything
# is a fixture: fake /proc trees whose pid chain starts at this script's own $$, stub `lane`, `timeout` and `getent`
# programs at paths of the fixture, and entry points started with LANE_WORKER=1 so that they stop at the guard and never
# reach a prover. The paths the helper reads are ARGUMENTS of `in_lane_worker` (never the environment), and the cases below
# prove that no variable turns a worker answer off. Entry points that are not in this tree are skipped. The last line is
# `in-lane-worker-test: pass`.
set -euo pipefail

root=$(cd "$(dirname "$0")/../.." && pwd)
base=${IN_LANE_WORKER_TEST_TMP:-${TMPDIR:-$HOME/scratch/tmp}}
mkdir -p "$base"
tmp=$(mktemp -d "$base/in-lane-worker.XXXXXX")
trap 'rm -rf "$tmp"' EXIT

# shellcheck source=../lib/in-lane-worker.sh
. "$root/scripts/lib/in-lane-worker.sh"

me=$$
fails=0
fail() {
    echo "FAIL: $*" >&2
    fails=$((fails + 1))
}

# expect <worker|not> <label> <lane_path> <proc_root>: the answer of the helper, with every LANE_* variable unset.
expect() {
    local want=$1 label=$2 got
    if (
        unset LANE_WORKER LANE_PROOFS_REFUSED
        in_lane_worker "$3" "$4" "${5:-}"
    ); then got=worker; else got=not; fi
    [ "$got" = "$want" ] || fail "$label: wanted $want, got $got"
}

# fake_proc <dir> <cgroup of $$> <cgroup of parent 1> <cgroup of parent 2>: $$ -> 91001 -> 91002 -> 1. A cgroup of "-" is an
# unreadable step (the file is not written). The command names hold spaces and a ")" to prove the parse is after the last ") ".
fake_proc() { # <dir> <cgroups...>; the chain starts at $me (default this script, $$)
    local d=$1 p
    rm -rf "$d"
    mkdir -p "$d/$me" "$d/91001" "$d/91002" "$d/1"
    printf '%s (bash) S 91001 1 1 0 -1\n' "$me" > "$d/$me/stat"
    printf '91001 (bq run (x) y) S 91002 1 1 0 -1\n' > "$d/91001/stat"
    printf '91002 (a b) S 1 1 1 0 -1\n' > "$d/91002/stat"
    printf '1 (init) S 0 1 1 0 -1\n' > "$d/1/stat"
    local i=2
    for p in "$me" 91001 91002; do
        [ "${!i}" = "-" ] || printf '0::%s\n' "${!i}" > "$d/$p/cgroup"
        i=$((i + 1))
    done
    printf '0::/init.scope\n' > "$d/1/cgroup"
}

clean=/user.slice/user-1000.slice/session-3.scope

# --- 1. the walk (no lane at all: the path does not exist)
nolane=$tmp/no-such-lane
fake_proc "$tmp/p1" "$clean" "$clean" "$clean"
expect not "clean chain" "$nolane" "$tmp/p1"

# the runner's scope on a GRANDPARENT, the script's own cgroup a bq scope: the case /proc/self/cgroup alone misses
fake_proc "$tmp/p2" /user.slice/bq-4242.scope /user.slice/bq-wrap-1.scope /user.slice/lane-runner-ab12cd.scope
expect worker "runner scope on a grandparent, own cgroup a bq scope" "$nolane" "$tmp/p2"

fake_proc "$tmp/p3" /user.slice/lane-runner-ab12cd.scope "$clean" "$clean"
expect worker "runner scope on the script itself" "$nolane" "$tmp/p3"

fake_proc "$tmp/p4" "$clean" /user.slice/lane-attempt-4521.scope "$clean"
expect worker "attempt scope lane-<name>-<n>.scope" "$nolane" "$tmp/p4"

fake_proc "$tmp/p5" "$clean" /user.slice/lane-notes.scope "$clean"
expect not "a lane-*.scope without a trailing number is not an attempt scope" "$nolane" "$tmp/p5"

# unreadable steps count as workers
fake_proc "$tmp/p6" "$clean" "-" "$clean"
expect worker "unreadable cgroup of a parent" "$nolane" "$tmp/p6"
fake_proc "$tmp/p7" "$clean" "$clean" "$clean"
rm "$tmp/p7/91001/stat"
expect worker "unreadable stat of a parent" "$nolane" "$tmp/p7"
fake_proc "$tmp/p8" "$clean" "$clean" "$clean"
printf 'garbage\n' > "$tmp/p8/91002/stat"
expect worker "stat without a parent field" "$nolane" "$tmp/p8"
fake_proc "$tmp/p9" "$clean" "$clean" "$clean"
printf '91002 (a b) S notapid 1 1 0 -1\n' > "$tmp/p9/91002/stat"
expect worker "parent that is not a number" "$nolane" "$tmp/p9"
fake_proc "$tmp/p10" "$clean" "$clean" "$clean"
printf '91002 (a b) S 91002 1 1 0 -1\n' > "$tmp/p10/91002/stat"
expect worker "a parent chain that never reaches pid 1" "$nolane" "$tmp/p10"

# --- 2. stub lane at an absolute path of the fixture
stub_lane() { # <path> <exit code of `in-worker`> <help works: 1|0>
    cat > "$1" << EOS
#!/bin/sh
if [ "\$1" = in-worker ] && [ "\$2" = --help ]; then exit $([ "$3" = 1 ] && echo 0 || echo 2); fi
if [ "\$1" = in-worker ]; then exit $2; fi
exit 9
EOS
    chmod 0755 "$1"
}
for code in 0 1 2 7; do
    stub_lane "$tmp/lane$code" "$code" 1
done
expect worker "lane in-worker exits 0 (clean tree)" "$tmp/lane0" "$tmp/p1"
expect not "lane in-worker exits 1 (clean tree)" "$tmp/lane1" "$tmp/p1"
expect worker "lane in-worker exits 2: cannot tell" "$tmp/lane2" "$tmp/p1"
expect worker "lane in-worker exits 7: anything but 1" "$tmp/lane7" "$tmp/p1"
expect worker "lane says 1 but an ancestor holds the runner's scope" "$tmp/lane1" "$tmp/p2"

# `--help` fails (older lane): the call itself decides when it answers 0, 1 or 2
stub_lane "$tmp/lane-nh0" 0 0
stub_lane "$tmp/lane-nh1" 1 0
stub_lane "$tmp/lane-nh2" 2 0
expect worker "help fails, in-worker exits 0" "$tmp/lane-nh0" "$tmp/p1"
expect not "help fails, in-worker exits 1 (clean tree)" "$tmp/lane-nh1" "$tmp/p1"
expect worker "help fails, in-worker exits 1, runner scope on a grandparent" "$tmp/lane-nh1" "$tmp/p2"
expect worker "help fails, in-worker exits 2" "$tmp/lane-nh2" "$tmp/p1"
# any other status without a working --help: no such subcommand, the walk decides
stub_lane "$tmp/lane-none" 9 0
expect not "no in-worker subcommand, clean tree: the walk says not" "$tmp/lane-none" "$tmp/p1"
expect worker "no in-worker subcommand, runner scope on a grandparent: the walk says worker" "$tmp/lane-none" "$tmp/p2"
# a lane that is not there at all
expect worker "no lane, runner scope on a grandparent" "$nolane" "$tmp/p2"
# a lane that hangs: `timeout 10` counts as a worker
printf '#!/bin/sh\nsleep 30\n' > "$tmp/lane-hang"
chmod 0755 "$tmp/lane-hang"
expect worker "a lane that hangs counts as a worker" "$tmp/lane-hang" "$tmp/p1"
# a fake `timeout` first on PATH that exits 1 changes nothing: the helper calls /usr/bin/timeout by absolute path
mkdir "$tmp/fakebin"
printf '#!/bin/sh\nexit 1\n' > "$tmp/fakebin/timeout"
chmod 0755 "$tmp/fakebin/timeout"
if ! (
    PATH="$tmp/fakebin:$PATH"
    unset LANE_WORKER LANE_PROOFS_REFUSED
    in_lane_worker "$tmp/lane0" "$tmp/p1"
); then fail "a fake timeout first on PATH (exit 1) skipped the lane trigger"; fi
# a timeout path that is missing or not executable: worker (argument for the tests only)
if ! (
    unset LANE_WORKER LANE_PROOFS_REFUSED
    in_lane_worker "$tmp/lane1" "$tmp/p1" "" "$tmp/no-such-timeout"
); then fail "a missing timeout binary must count as worker"; fi
# st=124: `--help` answers at once, `in-worker` sleeps past the timeout: worker
printf '#!/bin/sh\n[ "$2" = --help ] && exit 0\nsleep 30\n' > "$tmp/lane-slow"
chmod 0755 "$tmp/lane-slow"
expect worker "in-worker sleeping past the timeout counts as a worker" "$tmp/lane-slow" "$tmp/p1"

# --- 2b. no /proc and no lane on Darwin: not a worker; every other unreadable case stays a worker
expect not "Darwin, proc root that does not exist" "$nolane" "$tmp/no-proc-here" Darwin
expect worker "Linux, proc root that does not exist" "$nolane" "$tmp/no-proc-here" Linux
mkdir "$tmp/empty-proc"
expect worker "Darwin, proc root exists but is empty (no self): a Linux view with procfs hidden stays a worker" "$nolane" "$tmp/empty-proc" Darwin
expect worker "Darwin, proc root exists, runner scope on a grandparent: the walk decides" "$nolane" "$tmp/p2" Darwin
expect not "Darwin, proc root exists, clean chain: the walk decides" "$nolane" "$tmp/p1" Darwin
if ! (
    export LANE_WORKER=1
    in_lane_worker "$nolane" "$tmp/no-proc-here" Darwin
); then fail "LANE_WORKER=1 on Darwin must still say worker"; fi

# --- 2c. the same answers from a POSIX shell (dash)
posix_answer() { # <cgroup of $$> <cgroup of parent 1> <cgroup of parent 2>: the answer of `sh` over a tree built for its pid
    local out=$tmp/sh-out go=$tmp/sh-go pidf=$tmp/sh-pid i
    rm -f "$out" "$go" "$pidf"
    mkfifo "$go"
    sh -c 'echo $$ > "$1"; read -r _ < "$2"; . "$3"; if in_lane_worker "$4" "$5"; then echo worker; else echo not; fi' \
        sh "$pidf" "$go" "$root/scripts/lib/in-lane-worker.sh" "$nolane" "$tmp/psh" > "$out" 2>&1 &
    for i in $(seq 1 100); do [ -s "$pidf" ] && break; sleep 0.1; done
    me=$(cat "$pidf")
    fake_proc "$tmp/psh" "$1" "$2" "$3"
    echo go > "$go"
    wait $! || true
    me=$$
    cat "$out"
}
[ "$(posix_answer "$clean" "$clean" "$clean")" = not ] || fail "sh: clean chain must say not"
[ "$(posix_answer /user.slice/bq-4242.scope /user.slice/bq-wrap-1.scope /user.slice/lane-runner-ab12cd.scope)" = worker ] \
    || fail "sh: runner scope on a grandparent must say worker (the [[ =~ ]] fail-open)"
[ "$(posix_answer "$clean" /user.slice/lane-attempt-4521.scope "$clean")" = worker ] || fail "sh: attempt scope must say worker"

# --- 2d. the lane binary is found from the passwd entry of the real uid (a stub getent is the fifth argument)
mkdir -p "$tmp/h/.local/bin"
stub_getent() { printf '#!/bin/sh\n%s\n' "$2" > "$1"; chmod 0755 "$1"; }
stub_getent "$tmp/getent-ok" "echo 'user:x:1000:1000:U:$tmp/h:/bin/sh'"
stub_getent "$tmp/getent-fail" "exit 2"
stub_getent "$tmp/getent-nohome" "echo 'user:x:1000:1000:U::/bin/sh'"
stub_getent "$tmp/getent-relhome" "echo 'user:x:1000:1000:U:relative/home:/bin/sh'"
stub_getent "$tmp/getent-nolane" "echo 'user:x:1000:1000:U:$tmp/empty-home:/bin/sh'"
mkdir -p "$tmp/empty-home"
lookup() { # <expected> <label> <getent> <proc> [lane stub to put at <home>/.local/bin/lane]
    local want=$1 label=$2 got
    if [ -n "${5:-}" ]; then cp "$5" "$tmp/h/.local/bin/lane"; else rm -f "$tmp/h/.local/bin/lane"; fi
    if (
        unset LANE_WORKER LANE_PROOFS_REFUSED
        in_lane_worker "" "$4" "" "" "$3"
    ); then got=worker; else got=not; fi
    [ "$got" = "$want" ] || fail "$label: wanted $want, got $got"
}
lookup worker "passwd home holds a lane that says 0" "$tmp/getent-ok" "$tmp/p1" "$tmp/lane0"
lookup not "passwd home holds a lane that says 1, clean tree" "$tmp/getent-ok" "$tmp/p1" "$tmp/lane1"
lookup worker "passwd home holds a lane that says 1, runner scope on a grandparent" "$tmp/getent-ok" "$tmp/p2" "$tmp/lane1"
lookup worker "getent fails: the home cannot be resolved" "$tmp/getent-fail" "$tmp/p1" "$tmp/lane1"
lookup worker "passwd entry without a home" "$tmp/getent-nohome" "$tmp/p1" "$tmp/lane1"
lookup worker "passwd entry with a relative home" "$tmp/getent-relhome" "$tmp/p1" "$tmp/lane1"
lookup worker "no getent binary" "$tmp/no-such-getent" "$tmp/p1" "$tmp/lane1"
lookup not "home resolved, no lane binary there, clean tree: the walk decides" "$tmp/getent-nolane" "$tmp/p1"
lookup worker "home resolved, no lane binary there, runner scope: the walk decides" "$tmp/getent-nolane" "$tmp/p2"
# $HOME and PATH do not move the lookup: a lane that says 0 in a fake $HOME is not consulted
mkdir -p "$tmp/fakehome/.local/bin"
cp "$tmp/lane0" "$tmp/fakehome/.local/bin/lane"
if ! (
    unset LANE_WORKER LANE_PROOFS_REFUSED
    HOME="$tmp/fakehome" PATH="$tmp/fakehome/.local/bin:$PATH"
    in_lane_worker "" "$tmp/p1" "" "" "$tmp/getent-nolane"
); then :; else fail "\$HOME or PATH steered the lane lookup"; fi
# Darwin with no proc root answers before the lookup, even when getent is missing
if (
    unset LANE_WORKER LANE_PROOFS_REFUSED
    in_lane_worker "" "$tmp/no-proc-here" Darwin "" "$tmp/no-such-getent"
); then fail "Darwin with no proc root must not need getent"; fi

# --- 3. variables are a second trigger, and cannot clear anything
if ! (
    export LANE_WORKER=1
    in_lane_worker "$nolane" "$tmp/p1"
); then fail "LANE_WORKER=1 on a clean tree"; fi
if ! (
    export LANE_PROOFS_REFUSED=1
    in_lane_worker "$nolane" "$tmp/p1"
); then fail "LANE_PROOFS_REFUSED set on a clean tree"; fi
for v in "LANE_WORKER=0" "LANE_WORKER=" "LANE_PROOFS_REFUSED=" "LANE_ANYTHING=0"; do
    if ! (
        unset LANE_WORKER LANE_PROOFS_REFUSED
        export "${v?}"
        in_lane_worker "$nolane" "$tmp/p2"
    ); then fail "variable $v turned a worker (runner scope on a grandparent) into not-a-worker"; fi
    if ! (
        unset LANE_WORKER LANE_PROOFS_REFUSED
        export "${v?}"
        in_lane_worker "$tmp/lane0" "$tmp/p1"
    ); then fail "variable $v turned lane's exit 0 into not-a-worker"; fi
done
if ! (
    unset LANE_WORKER LANE_PROOFS_REFUSED
    in_lane_worker "$nolane" "$tmp/p2"
); then fail "with the variables unset the walk must still say worker"; fi
# the paths are arguments, never the environment: a variable that names them changes nothing
if ! (
    export LANE_PATH="$tmp/lane1" PROC_ROOT="$tmp/p1" PROCROOT="$tmp/p1" LANE="$tmp/lane1"
    in_lane_worker "$nolane" "$tmp/p2"
); then fail "an environment variable replaced a path argument"; fi

# --- 4. the guarded entry points that exist in this tree stop at the guard, exit 75, with the message, and never reach a prover
entry() { # <script path under root>
    local f=$root/$1 name out code
    name=$(basename "$1")
    [ -f "$f" ] || return 0
    grep -q 'lib/in-lane-worker.sh' "$f" || fail "$1 does not source the helper"
    code=0
    out=$(cd "$tmp" && LANE_WORKER=1 VERUS=/nonexistent/verus bash "$f" 2>&1 > /dev/null) || code=$?
    [ "$code" = 75 ] || fail "$1: exit $code, wanted 75: $out"
    [ "$out" = "$name: refused inside a lane worker (proofs run on GitHub)" ] || fail "$1: message was: $out"
    for shell in sh dash; do
        command -v "$shell" > /dev/null 2>&1 || continue
        code=0
        out=$(cd "$tmp" && LANE_WORKER=1 VERUS=/nonexistent/verus "$shell" "$f" 2>&1 > /dev/null) || code=$?
        [ "$code" = 75 ] || fail "$1 under $shell: exit $code, wanted 75: $out"
        [ "$out" = "$name: refused inside a lane worker (proofs run on GitHub)" ] || fail "$1 under $shell: message was: $out"
    done
}
entries="scripts/verify.sh scripts/verify-shipped.sh scripts/verify-verus.sh scripts/verify-mutations.sh scripts/verus-one.sh
    scripts/mac-model-check.sh userspace/formal/verify-mutations.sh specs/juno/run.sh userspace/formal/tla-parity.sh"
for e in $entries; do
    entry "$e"
done

# --- 4c. the juno entry point finds the helper at either depth (private tree, published tree) and refuses when it finds none
juno_entry=
for e in specs/juno/run.sh specs/juno/run.sh; do
    if [ -f "$root/$e" ]; then
        juno_entry=$e
        break
    fi
done
if [ -n "$juno_entry" ]; then
    for layout in "two:a/b" "three:a/b/c"; do
        d=$tmp/depth-${layout%%:*}
        sub=${layout#*:}
        rm -rf "$d"
        mkdir -p "$d/$sub"
        cp "$root/$juno_entry" "$d/$sub/run.sh"
        up=$(printf '%s' "$sub" | sed 's|[^/]*|..|g')
        mkdir -p "$d/$sub/$up/scripts/lib"
        cp "$root/scripts/lib/in-lane-worker.sh" "$d/$sub/$up/scripts/lib/in-lane-worker.sh"
        code=0
        out=$(cd "$tmp" && LANE_WORKER=1 bash "$d/$sub/run.sh" 2>&1 > /dev/null) || code=$?
        [ "$code" = 75 ] || fail "juno entry at depth ${layout%%:*}: exit $code, wanted 75: $out"
        [ "$out" = "run.sh: refused inside a lane worker (proofs run on GitHub)" ] || fail "juno entry at depth ${layout%%:*}: message was: $out"
    done
    d=$tmp/depth-none
    rm -rf "$d"
    mkdir -p "$d/a/b"
    cp "$root/$juno_entry" "$d/a/b/run.sh"
    code=0
    out=$(cd "$tmp" && bash "$d/a/b/run.sh" 2>&1 > /dev/null) || code=$?
    [ "$code" = 75 ] || fail "juno entry with no helper: exit $code, wanted 75: $out"
    case $out in *"in-lane-worker.sh not found"*) ;; *) fail "juno entry with no helper: message was: $out" ;; esac
fi

# --- 4b. entry-point level, with the helper's paths fixed by a wrapper (a test file in the fixture, not the environment):
# the entry point is started with no LANE_* variable, its pid is read, a fake /proc tree is built for it, and it is released.
# an entry whose body stops at once on its own (no prover, no input), so that the guard is all that decides; none: skip 4b
pass_entry=
for e in scripts/verus-one.sh scripts/verify-verus.sh; do
    if [ -f "$root/$e" ]; then
        pass_entry=$e
        break
    fi
done
run_entry() { # <uname> <proc: tree|none|empty> [cgroups of the script, its parent, its grandparent]
    local d=$tmp/ep uname_s=$1 mode=$2 i
    shift 2
    rm -rf "$d"
    mkdir -p "$d/scripts/lib" "$d/proc-fix"
    mkdir -p "$d/$(dirname "$pass_entry")"
    cp "$root/$pass_entry" "$d/$pass_entry"
    sed 's/^    in_lane_worker() {/    _real_ilw() {/' "$root/scripts/lib/in-lane-worker.sh" > "$d/scripts/lib/in-lane-worker.sh"
    local procdir=$d/proc-fix
    [ "$mode" != none ] || procdir=$d/no-such-proc
    cat >> "$d/scripts/lib/in-lane-worker.sh" << EOW
in_lane_worker() {
    echo \$\$ > "$d/pid"
    read -r _ < "$d/go"
    _real_ilw "$d/no-such-lane" "$procdir" "$uname_s"
}
EOW
    mkfifo "$d/go"
    (cd "$d" && env -u LANE_WORKER -u LANE_PROOFS_REFUSED VERUS=/nonexistent/verus VSTD_PATH=/nonexistent bash "$d/$pass_entry" > "$d/out" 2>&1) &
    local bg=$!
    for i in $(seq 1 100); do [ -s "$d/pid" ] && break; sleep 0.1; done
    me=$(cat "$d/pid")
    [ "$mode" != tree ] || fake_proc "$d/proc-fix" "$@"
    me=$$
    echo go > "$d/go"
    entry_code=0
    wait "$bg" || entry_code=$?
    entry_out=$(cat "$d/out")
}
if [ -n "$pass_entry" ]; then
run_entry Linux tree "$clean" "$clean" "$clean"
[ "$entry_code" != 75 ] || fail "entry point, clean tree, no worker anywhere: refused although nothing says worker: $entry_out"
case $entry_out in *"refused inside a lane worker"*) fail "entry point, clean tree: refusal message printed: $entry_out" ;; esac
run_entry Linux tree /user.slice/bq-4242.scope /user.slice/bq-wrap-1.scope /user.slice/lane-runner-ab12cd.scope
[ "$entry_code" = 75 ] || fail "entry point, runner scope on a grandparent: exit $entry_code, wanted 75: $entry_out"
run_entry Darwin none
[ "$entry_code" != 75 ] || fail "entry point on Darwin with no proc root (the Mac): refused: $entry_out"
run_entry Darwin empty
[ "$entry_code" = 75 ] || fail "entry point, Darwin but a proc root that exists and is empty: exit $entry_code, wanted 75: $entry_out"
run_entry Linux none
[ "$entry_code" = 75 ] || fail "entry point on Linux with no proc root: exit $entry_code, wanted 75: $entry_out"
fi

# the mutation scripts carry no guard
while IFS= read -r m; do
    if grep -q 'in-lane-worker' "$m"; then fail "$m carries the guard"; fi
done < <(find "$root/verification" "$root/crates" "$root/scripts" \( -name mutants.sh -o -name "verus-mutants-*.sh" \) 2> /dev/null)

if [ "$fails" -ne 0 ]; then
    echo "in-lane-worker-test: $fails failure(s)" >&2
    exit 1
fi
echo "in-lane-worker-test: pass"
