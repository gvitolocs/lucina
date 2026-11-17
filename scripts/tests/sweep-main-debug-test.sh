#!/usr/bin/env bash
# scripts/tests/sweep-main-debug-test.sh: the third pass of scripts/sweep-caches.sh, the `debug/` of a MAIN checkout's target
# (docs/ci/SHIFT-LEFT.md, "Other cargo targets"). The pass is there because a main checkout belongs to no cache and no worktree, so
# neither of the other two passes ever reached its 93.7 GiB of debug build. What is proved here is the whole of the rule: the one
# directory it may remove, the four reasons it must not, and that nothing else of the target is so much as touched.
#
# Everything is a fixture: a fake HOME, a projects root of its own, a unit folder and a bin folder of its own, an empty folder table,
# and a sweep binary that is deliberately not there so the size pass says so and stops. Nothing of the real ~/Projects, /etc/systemd
# or ~/.config is read. The one thing the script asks for real is /proc, which is why the in-use case holds a target with a `sleep`
# this test starts and kills again by the PID it got back, and by no other.
#
# The fixture is run twice: dry, where every verdict is reported and nothing is touched, and --apply, where the one debug/ that has
# earned it goes and each of the others stays. The siblings of the debug that goes are fingerprinted by name and content before the
# run and compared after it, because "only debug/" is the one promise a size total cannot show. Two more fixtures hold what the rule
# must not reach at all: the two wrong path shapes, and the symlinks — one folder under two names, and a target that aims out of the
# projects root onto a disk this run was never given. The last line is `sweep-main-debug-test: pass`.
set -euo pipefail

root=$(cd "$(dirname "$0")/../.." && pwd)
sweep=$root/scripts/sweep-caches.sh
base=${SWEEP_MAIN_DEBUG_TEST_TMP:-${TMPDIR:-$HOME/scratch/tmp}}
mkdir -p "$base"
tmp=$(mktemp -d "$base/sweep-main-debug.XXXXXX")
holder_pid=""

# cleanup: the sleep this test started, by its PID and no other, and the fixture. On the exit path, so a failure leaves nothing behind.
cleanup() {
    if [ -n "$holder_pid" ]; then
        kill "$holder_pid" 2> /dev/null || true
        wait "$holder_pid" 2> /dev/null || true
    fi
    rm -rf "$tmp"
}
trap cleanup EXIT

fail() { echo "FAIL: $*" >&2; exit 1; }

# prep <fixture>: a fake HOME with a projects root, a unit folder and a bin folder of its own, and the table the size pass reads empty.
prep() {
    fx=$tmp/$1
    fhome=$fx/fakehome
    fproj=$fhome/Projects
    funit=$fhome/.config/systemd/user
    fbin=$fhome/.local/bin
    mkdir -p "$fproj" "$funit" "$fbin"
    : > "$fx/conf"
}

# run_sweep <fixture> [args]: the real script over that fixture and nothing else, everything it may read named out of the fixture path
# rather than remembered from a prep. The log lands in the fake HOME like the cache does, and LUCINA_CI_SWEEP points at a file that is
# not there, which is how the size pass is kept out of the run.
run_sweep() {
    local fx=$tmp/$1
    shift
    env HOME="$fx/fakehome" \
        LUCINA_SWEEP_CONF="$fx/conf" \
        LUCINA_SWEEP_PROJECTS_ROOT="$fx/fakehome/Projects" \
        LUCINA_SWEEP_SYSTEMD_DIRS="$fx/fakehome/.config/systemd/user" \
        LUCINA_SWEEP_BIN_DIRS="$fx/fakehome/.local/bin" \
        LUCINA_CACHE_SWEEP_LOG="$fx/fakehome/.local/state/lucina-cache-sweep.log" \
        LUCINA_CI_SWEEP="$fx/no-such-sweep-binary" \
        bash "$sweep" "$@"
}

# mk_target <target>: the shapes a main checkout's target carries, a few bytes each. `debug/` with the four folders of a real debug
# build, and beside it a release, two triples and a tmp, each with content worth naming.
mk_target() {
    local t=$1
    mkdir -p "$t/debug/deps" "$t/debug/build" "$t/debug/incremental" "$t/debug/.fingerprint" \
        "$t/release/deps" "$t/release/.fingerprint" \
        "$t/x86_64-unknown-linux-musl/debug/deps" "$t/wasm32-unknown-unknown/release/deps" "$t/tmp"
    printf 'Signature: 8a477f597d28d1786c96eae039bdb42b\n' > "$t/CACHEDIR.TAG"
    printf 'debug artifact\n' > "$t/debug/deps/libpkg.rlib"
    printf 'debug build script output\n' > "$t/debug/build/out.rs"
    printf 'debug incremental cache\n' > "$t/debug/incremental/sess-1.bin"
    : > "$t/debug/.fingerprint/pkg-1"
    printf '#!/bin/sh\necho debug binary\n' > "$t/debug/app"
    printf 'release artifact\n' > "$t/release/deps/libpkg.rlib"
    : > "$t/release/.fingerprint/pkg-2"
    printf 'musl artifact\n' > "$t/x86_64-unknown-linux-musl/debug/deps/libpkg.rlib"
    printf 'wasm artifact\n' > "$t/wasm32-unknown-unknown/release/deps/libpkg.a"
    printf 'output somebody left here\n' > "$t/tmp/keepme.bin"
}

# age <days> <path...>: the mtime of these paths and of everything under them, folders included: the pass asks the NEWEST entry of the
# debug tree, and a directory created today is today's whatever the files inside it say.
age() {
    local d=$1
    shift
    find "$@" -exec touch -d "$d days ago" {} +
}

# fingerprint <target>: every file of the target EXCEPT its debug/, by name and content. What the removal must not change.
fingerprint() {
    local t=$1 f
    (
        cd "$t" || exit 1
        find . -path ./debug -prune -o -type f -print | LC_ALL=C sort | while IFS= read -r f; do
            printf '%s %s\n' "$(sha256sum < "$f" | cut -d' ' -f1)" "$f"
        done
    )
}

# line_of <out> <target>: the one main-debug line of that run for that target, and a failure when the run said nothing about it.
line_of() {
    local l
    l=$(grep -F "main-debug: $2 " "$1" | head -n 1) || true
    if [ -z "$l" ]; then
        echo "FAIL: no main-debug line for $2 in $1:" >&2
        sed 's/^/| /' "$1" >&2
        exit 1
    fi
    printf '%s\n' "$l"
}

# want <out> <target> <text>: that the line of this target carries this verdict, and no other candidate's line will do for it.
want() {
    local l
    l=$(line_of "$1" "$2")
    case $l in
        *"$3"*) return 0 ;;
    esac
    echo "FAIL: line for $2 says \"$l\", expected it to carry \"$3\"" >&2
    exit 1
}

# stays <out> <target>: a folder this pass is not to name at all, since one line per candidate is the report and a folder that is no
# candidate is not in it.
stays() {
    if grep -F "main-debug: $2" "$1" > /dev/null; then
        echo "FAIL: $2 is not a candidate and yet a main-debug line names it:" >&2
        sed 's/^/| /' "$1" >&2
        exit 1
    fi
}

# ------------------------------------------------------------------- the fixture
#
# Six checkout targets of three groups, one per reason to keep, and one whose debug has earned its removal. The folders that are no
# candidate at all, by their depth or by where a link actually puts them, are in fixtures of their own below.
prep one
gone=$fproj/Lucina/Gone/target
recent=$fproj/Lucina/Recent/target
unitref=$fproj/Commissioned/Unitref/target
held=$fproj/Commissioned/Held/target
linked=$fproj/AI/Linked/target
nodebug=$fproj/AI/NoDebug/target
for t in "$gone" "$recent" "$unitref" "$held" "$linked" "$nodebug"; do
    mk_target "$t"
done
rm -rf "$nodebug/debug"
age 8 "$fproj"

# One file of one debug tree written yesterday: the age rule, and on the newest write of the folder and not on the folder itself.
printf 'rebuilt yesterday\n' > "$recent/debug/deps/fresh.rlib"

# A unit that runs out of a target: the reference a rebuild does not heal, since the service points at this exact path.
printf '[Unit]\nDescription=holds a main checkout target open\n\n[Service]\nExecStart=%s/release/app\n' "$unitref" \
    > "$funit/lucina-fake-thing.service"

# A hand-installed command that is a link into a debug build.
ln -s "$linked/debug/app" "$fbin/linked-tool"

# The build this test starts and owns: a sleep standing in a target with its cwd. Killed by this PID, and by no other.
(
    cd "$held" && exec sleep 300
) &
holder_pid=$!
sleep 1

# -------------------------------------------------------------------- the dry run
#
# Every verdict of the rule, and nothing touched: a dry run reports the debug it would free and keeps the one it must.
rc=0
run_sweep one > "$tmp/dry.out" 2>&1 || rc=$?
[ "$rc" = 0 ] || fail "dry run exited $rc: $(cat "$tmp/dry.out")"

want "$tmp/dry.out" "$gone" "would remove"
want "$tmp/dry.out" "$recent" "kept: written"
want "$tmp/dry.out" "$unitref" "kept: referenced by unit $funit/lucina-fake-thing.service"
want "$tmp/dry.out" "$held" "kept: in use by pid $holder_pid"
want "$tmp/dry.out" "$linked" "kept: referenced by symlink $fbin/linked-tool"
want "$tmp/dry.out" "$nodebug" "kept: no debug/"
[ "$(grep -c '^main-debug: ' "$tmp/dry.out")" = 6 ] ||
    fail "one line per candidate and nothing else: $(grep -n '^main-debug: ' "$tmp/dry.out")"
if grep -q '^main-debug: freed ' "$tmp/dry.out"; then
    fail "a dry run totals a removal it did not make: $(grep '^main-debug: freed ' "$tmp/dry.out")"
fi
for t in "$gone" "$recent" "$unitref" "$held" "$linked"; do
    [ -d "$t/debug" ] || fail "$t/debug is gone after a dry run"
done
echo "ok: the dry run reports every verdict of the rule and touches nothing"

# --------------------------------------------------------------------- the apply
#
# The siblings are fingerprinted here, after the dry run and before the removal, so what is compared is the very state the rm runs on.
fingerprint "$gone" > "$tmp/gone.before"
[ -s "$tmp/gone.before" ] || fail "the fingerprint of $gone came out empty: there was nothing to compare"
rc=0
run_sweep one --apply > "$tmp/apply.out" 2>&1 || rc=$?
[ "$rc" = 0 ] || fail "--apply exited $rc: $(cat "$tmp/apply.out")"

want "$tmp/apply.out" "$gone" "removed "
[ -d "$gone" ] || fail "the whole target went and not only its debug/: $gone"
if [ -e "$gone/debug" ]; then
    echo "FAIL: $gone/debug survived the removal it was given" >&2
    line_of "$tmp/apply.out" "$gone" >&2
    exit 1
fi
fingerprint "$gone" > "$tmp/gone.after"
if ! diff -u "$tmp/gone.before" "$tmp/gone.after" > "$tmp/gone.diff" 2>&1; then
    cat "$tmp/gone.diff" >&2
    fail "the profiles beside the debug that went changed with it"
fi
for d in "$gone/release" "$gone/x86_64-unknown-linux-musl" "$gone/wasm32-unknown-unknown" "$gone/tmp" "$gone/CACHEDIR.TAG"; do
    [ -e "$d" ] || fail "$d is not there after --apply"
done
for t in "$recent" "$unitref" "$held" "$linked"; do
    [ -d "$t/debug" ] || fail "$t is kept by the rule and yet its debug/ is gone: $(cat "$tmp/apply.out")"
done
grep -Eq '^main-debug: freed [0-9.]+ GiB from 1 debug folders$' "$tmp/apply.out" ||
    fail "no total of what --apply freed, or not exactly one folder: $(grep '^main-debug: freed ' "$tmp/apply.out")"
grep -q ' main-debug: ' "$fhome/.local/state/lucina-cache-sweep.log" ||
    fail "the pass wrote nothing to the state log"
echo "ok: --apply removes the one debug/ and leaves its release, its two triples and its tmp byte for byte"

# -------------------------------------------------------- what is no candidate
#
# Two aged cargo targets of the shapes the rule must not reach: the target of a worktree, which is the idle pass' and is no main
# checkout, and a target three folders deep instead of two. Their own fixture, run dry so the idle pass is asked about the first
# without removing it: the claim here is only the one this pass is about, that it names neither.
prep two
wt=$fproj/.worktrees/LucinaOS/br/target
deep=$fproj/Deep/repo/inner/target
for t in "$wt" "$deep"; do
    mk_target "$t"
done
age 8 "$fproj"
rc=0
run_sweep two > "$tmp/roots.out" 2>&1 || rc=$?
[ "$rc" = 0 ] || fail "the roots run exited $rc: $(cat "$tmp/roots.out")"
stays "$tmp/roots.out" "$wt"
stays "$tmp/roots.out" "$deep"
for t in "$wt" "$deep"; do
    [ -d "$t/debug" ] || fail "$t is no candidate and yet its debug/ went: $(cat "$tmp/roots.out")"
done
echo "ok: a .worktrees target and a target one level too deep are never named, let alone swept"

# ------------------------------------------------------ a link, and the folder it aims at
#
# Two shapes a real ~/Projects carries. A checkout whose `target` is a link to a folder on another disk: resolved, it is not under
# the projects root at all, and since every later check and the removal itself work on the path resolve_candidate gives back, a link
# cannot move the sweep to a disk this run was never given. And a folder with two names inside the root, which is ONE candidate: the pass
# reports and removes the folder it IS, once, and never under the name that is a link.
prep three
out=$tmp/elsewhere/target
steer=$fproj/Linked/Repo/target
twice=$fproj/Linked/Mirror/target
dup=$fproj/Real/Repo/target
mirror=$fproj/Real/Twin/target
mk_target "$out"
mk_target "$dup"
age 8 "$out" "$dup"
mkdir -p "$(dirname "$steer")" "$(dirname "$twice")" "$(dirname "$mirror")"
ln -s "$out" "$steer"
ln -s "$out" "$twice"
ln -s "$dup" "$mirror"
out_real=$(realpath -- "$out")
dup_real=$(realpath -- "$dup")
rc=0
run_sweep three --apply > "$tmp/link.out" 2>&1 || rc=$?
[ "$rc" = 0 ] || fail "the link run exited $rc: $(cat "$tmp/link.out")"
stays "$tmp/link.out" "$out_real"
stays "$tmp/link.out" "$steer"
stays "$tmp/link.out" "$mirror"
[ -d "$out_real/debug" ] || fail "a symlink aimed the removal at a folder outside the projects root: $out_real"
want "$tmp/link.out" "$dup_real" "removed "
[ "$(grep -c "^main-debug: $dup_real " "$tmp/link.out")" = 1 ] ||
    fail "one folder with two names came out as two candidates: $(grep -F "main-debug: $dup_real " "$tmp/link.out")"
[ ! -e "$dup_real/debug" ] || fail "$dup_real was named removed and its debug is still there"
echo "ok: a target resolving out of the projects root is no candidate, and one folder with two names is one"

echo "sweep-main-debug-test: pass"
