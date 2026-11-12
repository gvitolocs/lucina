#!/usr/bin/env bash
# scripts/tests/check-fast-oos-test.sh: the out-of-space pass of scripts/check-fast (docs/ci/BQ.md), end to end.
#
# It runs the real launcher with CHECK_FAST_BIN, a stub of `lucina-ci check-fast` that prints what the binary prints and exits
# with the code it exits with, and CHECK_FAST_LOG, which keeps the raw unfiltered output so the test can read it. So the pass is
# judged the way a reader judges it: on the launcher's own exit code and its own stdout, with no build, no queue and no disk
# filled. The rule under test is the condition the coordinator set for the pass: it may only rename a failure that already fails,
# and may never turn a gate's failure into a pass, a skip or a warning. The last line is `check-fast-oos-test: pass`.
set -euo pipefail
root=$(cd "$(dirname "$0")/../.." && pwd)
base=${CHECK_FAST_OOS_TEST_TMP:-${TMPDIR:-/tmp}}
mkdir -p "$base"
tmp=$(mktemp -d "$base/check-fast-oos.XXXXXX")
trap 'rm -rf "$tmp"' EXIT
fail() { # fail <why>: what the case checked, plus what the launcher said on its own stderr and in its raw log. A case that dies
    # before the stub runs leaves nothing in stdout, and stdout is all the asserts quote: these two say what actually happened.
    printf 'FAIL: %s\n' "$*" >&2
    if [ -n "${er:-}" ] && [ -f "$er" ]; then
        printf -- '--- %s: stderr of the case, first 40 lines ---\n' "$er" >&2
        head -n 40 "$er" >&2
    fi
    if [ -n "${raw:-}" ] && [ -f "$raw" ]; then
        printf -- '--- %s: raw log of the case, first 40 lines ---\n' "$raw" >&2
        head -n 40 "$raw" >&2
    fi
    exit 1
}

home=$tmp/home
tdir=$tmp/reports
mkdir -p "$home" "$tdir" "$tmp/noawk"
cat > "$tmp/noawk/awk" <<'EOF'
#!/usr/bin/env bash
# The pass of scripts/check-fast runs in awk; this is a broken awk: it dies instead of filtering, so it prints nothing.
echo "gawk: fatal: cannot execute" >&2
exit 3
EOF
chmod +x "$tmp/noawk/awk"

stub=$tmp/stub
cat > "$stub" <<'EOF'
#!/usr/bin/env bash
# The stub of `lucina-ci check-fast`: `sweep-target` and anything else says nothing and succeeds; `check-fast` plays the case the
# test set, with $OOS_OUT on stdout, $OOS_ERR on stderr and $OOS_RC as its exit code. With $OOS_SEE it first records to that file
# the TMPDIR it was given and what that dir holds while it runs.
set -u
[ "${1:-}" = check-fast ] || exit 0
if [ -n "${OOS_SEE:-}" ]; then
    {
        printf 'tmpdir=%s\n' "${TMPDIR:-}"
        printf 'contents=%s\n' "$(ls -A "${TMPDIR:-/nonexistent}" 2>/dev/null | tr '\n' ' ')"
    } > "$OOS_SEE"
fi
[ -z "${OOS_OUT:-}" ] || cat "$OOS_OUT"
[ -z "${OOS_ERR:-}" ] || cat "$OOS_ERR" >&2
exit "${OOS_RC:-0}"
EOF
chmod +x "$stub"

has() { # has <file> <text> <what it proves>: the text is there
    grep -qF -- "$2" "$1" || fail "$3: '$2' is missing from:
$(cat "$1")"
}
hasnt() { # hasnt <file> <text> <what it proves>: the text is not there
    if grep -qF -- "$2" "$1"; then
        fail "$3: unexpected '$2' in:
$(cat "$1")"
    fi
}
nlines() { # nlines <file> <regex> <count> <what it proves>
    local n
    n=$(grep -c -- "$2" "$1" || true)
    [ "$n" -eq "$3" ] || fail "$4: expected $3 line(s) matching '$2', found $n in:
$(cat "$1")"
}
samebytes() { # samebytes <name>: what the launcher printed is the stub's own output, byte for byte
    diff_out=$(diff "$tmp/$1.direct" "$out" || true)
    [ -z "$diff_out" ] || fail "($1) the launcher did not print the binary's own bytes:
$diff_out"
}

# run_case <name> <stdout-file> <exit code> [stderr-file] [dir-first-on-PATH]: the real scripts/check-fast over the stub, with a
# free-space floor it cannot trip, a temp dir of its own and no `bq` on PATH (so nothing is built, queued or waited for).
# LUCINA_CHECK_TMP is the launcher's own knob for that dir: it sets TMPDIR from it and does not keep the caller's, and the folded
# class of a step that names no path is the dir the pass reads off TMPDIR, so the cases below that expect $tdir pin it here.
# It leaves rc, out (what the launcher printed), raw (the unfiltered log) and er (its stderr) for the asserts.
run_case() {
    local name=$1 of=$2 code=$3 err=${4:-} first=${5:-}
    export OOS_OUT=$of OOS_RC=$code
    if [ -n "$err" ]; then export OOS_ERR=$err; else unset OOS_ERR; fi
    rc=0
    HOME=$home PATH=${first:+$first:}/usr/bin:/bin TMPDIR=$tdir LUCINA_CHECK_TMP=$tdir \
        CHECK_FAST_BIN=$stub CHECK_FAST_LOG=$tmp/$name.raw \
        CARGO_TARGET_DIR=$tmp/target \
        LUCINA_CHECK_FREE_FLOOR_GIB=0 LUCINA_CHECK_ALLOW_TARGET_DIR=1 \
        "$root/scripts/check-fast" > "$tmp/$name.out" 2> "$tmp/$name.err" || rc=$?
    out=$tmp/$name.out
    raw=$tmp/$name.raw
    er=$tmp/$name.err
}

# One diagnostic line, on stderr and before the first case: the host the launcher's own pre-stub commands run on. Nothing asserts
# on it, so it cannot change a verdict; it is there for a runner where case (a) dies before the stub, where stdout is empty and
# says nothing. The programs are looked up on the same PATH the cases hand the launcher.
{
    bash_v=$(bash --version 2>&1 | head -1)
    awk_v=$( { awk -W version 2>&1 | head -1 || awk --version 2>&1 | head -1; } || true)
    progs=$(PATH=/usr/bin:/bin bash -c 'for c in sha1sum df timeout du tee mktemp; do
        command -v "$c" >/dev/null 2>&1 && printf "%s=yes " "$c" || printf "%s=no " "$c"
    done')
    printf 'diag: %s | awk: %s | %s | PATH=/usr/bin:/bin: %s\n' \
        "$bash_v" "$awk_v" "$(uname -sr)" "$progs" >&2
} || true

# The four gate steps that write their reports under TMPDIR, all of them dead on one full tmpfs: what 2026-11-12 actually looked
# like, and what must read as ONE class. Two name the path they were writing, two only the phrase.
f=$tmp/a.txt
{
    printf 'ok   %-28s %5.1fs\n' build 1.2
    printf 'FAIL %-28s %5.1fs\n' inventory 0.9
    printf '    Error: writing the report: Disk quota exceeded (os error 122)\n'
    printf 'FAIL %-28s %5.1fs\n' spec 1.1
    printf '    Error: writing %s/check-fast-4242/spec.json: Disk quota exceeded (os error 122)\n' "$tdir"
    printf 'FAIL %-28s %5.1fs\n' roles 0.7
    printf '    Error: writing %s/check-fast-4242/roles.json: Disk quota exceeded (os error 122)\n' "$tdir"
    printf 'FAIL %-28s %5.1fs\n' trust 0.8
    printf '    Error: writing %s/check-fast-5150/trust.json: No space left on device (os error 28)\n' "$tdir"
    printf 'check-fast: 30s, 4 failed, 0 skipped\n'
} > "$f"
# (a) all four on a full disk: one class, no FAIL line left in the report, and still a failure.
run_case a "$f" 1
[ "$rc" -ne 0 ] || fail "(a) a run of four quota'd steps passed: exit $rc"
nlines "$out" '^FAIL ' 0 "(a) four gate failures must not be reported as four FAIL lines"
nlines "$out" '^oos   ' 4 "(a) each folded step says which one it is"
nlines "$out" "^check-fast: out of space in $tdir:" 1 "(a) the report carries exactly one class line, of the one folder they share"
has "$out" "out of space in $tdir: inventory, spec, roles, trust failed on a full disk" "(a) the class names all four steps and the folder"
has "$out" "ONE failure class and not 4 gate failures" "(a) the class says what it replaced"
has "$out" "oos   trust" "(a) the folded lines name their step"
has "$out" "(No space left on device)" "(a) and the cause each one died on"
has "$out" "check-fast: 30s, 1 failed, 0 skipped: out of space in $tdir (inventory, spec, roles, trust)" \
    "(a) the summary counts the class as one failure, never as zero and never as a skip"
nlines "$raw" '^FAIL ' 4 "(a) the raw log keeps the four blocks"
has "$raw" "Error: writing the report: Disk quota exceeded (os error 122)" "(a) the tail of the step that named no path survives"
for j in spec roles trust; do
    has "$raw" "$j.json: " "(a) the tail of $j survives with the path it was writing"
done
has "$raw" "check-fast: 30s, 4 failed, 0 skipped" "(a) the raw log keeps the binary's own summary"
echo "ok: (a) four quota'd steps are one class, still a failure, and every tail survives in the raw log"

# (b) a real gate failure next to a quota'd one: the real one is reported as it was, and counted as its own failure.
f=$tmp/b.txt
{
    printf 'FAIL %-28s %5.1fs\n' fmt 0.4
    printf '    Diff in tools/bq/src/main.rs:4 at line 4:\n'
    printf '    unexpected `cfg` condition name: `nope`\n'
    printf 'ok   %-28s %5.1fs\n' clippy 2.0
    printf 'FAIL %-28s %5.1fs\n' inventory 0.9
    printf '    Error: writing %s/check-fast-4242/inventory.json: Disk quota exceeded (os error 122)\n' "$tdir"
    printf 'check-fast: 12s, 2 failed, 1 skipped\n'
} > "$f"
run_case b "$f" 1
[ "$rc" -ne 0 ] || fail "(b) a run with a real failure passed: exit $rc"
nlines "$out" '^FAIL ' 1 "(b) fmt is the only failure reported as one"
has "$out" "FAIL fmt" "(b) the fmt block is still reported as a failure"
has "$out" "    Diff in tools/bq/src/main.rs:4 at line 4:" "(b) its header keeps its tail, unchanged"
has "$out" '    unexpected `cfg` condition name: `nope`' "(b) every tail line of the real failure is printed as it was"
has "$out" "oos   inventory" "(b) the quota'd step is the one folded"
has "$out" "check-fast: 12s, 2 failed, 1 skipped: out of space in $tdir/check-fast-4242 (inventory)" \
    "(b) fmt is counted on its own and the class adds the second failure, of the folder it was writing"
nlines "$raw" '^FAIL ' 2 "(b) the raw log keeps both blocks"
echo "ok: (b) a failing fmt next to a quota'd step is printed unchanged and still counted"

# (c) the binary died on the quota without reporting a FAIL block: nothing to fold, and the run still fails.
f=$tmp/c.txt
printf 'check-fast: 8s, 0 failed, 0 skipped\n' > "$f"
printf 'Error: writing %s/check-fast-909/report.json: Disk quota exceeded (os error 122)\n' "$tdir" > "$f.err"
run_case c "$f" 1 "$f.err"
[ "$rc" -ne 0 ] || fail "(c) a run that died on the quota with no FAIL block passed: exit $rc"
hasnt "$out" "out of space in" "(c) a step that was never reported as failing is not folded either"
has "$er" "Disk quota exceeded (os error 122)" "(c) the reason still reaches the reader on stderr"
echo "ok: (c) no FAIL block and the quota only on stderr: still non-zero, nothing folded"

# (d) a clean run: the launcher prints the stub's bytes and nothing else, which is what it printed before the pass existed.
f=$tmp/d.txt
{
    printf 'ok   %-28s %5.1fs\n' fmt 0.4
    printf 'ok   %-28s %5.1fs\n' clippy 2.0
    printf 'check-fast: 9s, 0 failed, 0 skipped\n'
} > "$f"
run_case d "$f" 0
[ "$rc" -eq 0 ] || fail "(d) a clean run failed: exit $rc, $(cat "$er")"
OOS_OUT=$f OOS_RC=0 "$stub" check-fast > "$tmp/d.direct" 2> /dev/null
samebytes d
hasnt "$out" "out of space" "(d) a clean run says nothing about the disk"
echo "ok: (d) a clean run is byte-identical to the binary's own output"

# (d2) the other line a clean run ends with, `nothing changed against main`, is passed through as it is.
f=$tmp/d2.txt
printf 'check-fast: nothing changed against main\n' > "$f"
run_case d2 "$f" 0
[ "$rc" -eq 0 ] || fail "(d2) a nothing-changed run failed: exit $rc, $(cat "$er")"
OOS_OUT=$f OOS_RC=0 "$stub" check-fast > "$tmp/d2.direct" 2> /dev/null
samebytes d2
echo "ok: (d2) 'nothing changed against main' is passed through byte for byte"

# (e) a failing unit test whose message merely contains the phrase: the pass cannot tell the two apart, so its cause reads as out
# of space. It is still a failure, and the raw log keeps the test as it was -- which is where such a case is read.
f=$tmp/e.txt
{
    printf 'FAIL %-28s %5.1fs\n' unit-tests 3.1
    printf '    test out_of_space_reports_one_class ... FAILED\n'
    printf "    thread 'out_of_space_reports_one_class' panicked at scripts/tests/check-fast-oos-test.sh:57: Disk quota exceeded (os error 122) while writing the fixture\n"
    printf '    failures:\n'
    printf 'check-fast: 20s, 1 failed, 0 skipped\n'
} > "$f"
run_case e "$f" 1
[ "$rc" -ne 0 ] || fail "(e) a run with a failing test passed: exit $rc"
has "$out" "check-fast: out of space in $tdir:" "(e) its cause is shown as out of space, which is the price of the fold"
has "$out" "unit-tests failed on a full disk" "(e) the step that folded is named"
has "$out" "check-fast: 20s, 1 failed, 0 skipped: out of space in" "(e) and the summary still counts one failure"
nlines "$raw" '^FAIL ' 1 "(e) the raw log keeps the failing test block"
has "$raw" "test out_of_space_reports_one_class ... FAILED" "(e) the raw log keeps the test line itself"
has "$raw" "panicked at scripts/tests/check-fast-oos-test.sh" "(e) the raw log keeps where it panicked"
echo "ok: (e) a test that only mentions the phrase is folded (cause shown as out of space), raw log keeps it, exit non-zero"

# (f) the pass itself dies (a broken awk) on a clean run: the run must fail, never pass. Two things guard it, and the first is the
# pipeline status: `tee` is killed writing to the dead filter and its buffer is lost, so the raw log can well be empty and the
# summary line is not there to be checked. The exit code of the pipeline is the rightmost non-zero one, so awk dying fails the run.
run_case f "$tmp/d.txt" 0 "" "$tmp/noawk"
[ "$rc" -ne 0 ] || fail "(f) a run whose filter died passed: exit $rc"
has "$er" "cannot execute" "(f) the filter's own error is what the reader sees"
[ ! -s "$out" ] || fail "(f) a run whose filter died printed as if it had passed:
$(cat "$out")"
echo "ok: (f) a dying awk fails the run instead of passing it"

# (g) the same on a run that already failed for a real reason: still non-zero, whatever the filter does.
run_case g "$tmp/b.txt" 1 "" "$tmp/noawk"
[ "$rc" -ne 0 ] || fail "(g) a failing run whose filter died passed: exit $rc"
echo "ok: (g) a failing run with a broken filter still fails"

# (h) The hooks are a way to test the pass, not a way to skip the check: a binary that prints nothing and exits 0 has no summary
# line, which is a refusal of its own (docs/ci/SHIFT-LEFT.md), so CHECK_FAST_BIN cannot fake a pass.
: > "$tmp/h.txt"
run_case h "$tmp/h.txt" 0
[ "$rc" -ne 0 ] || fail "(h) a binary that printed nothing passed: exit $rc"
has "$er" "ended without its summary line" "(h) and the reason says why"
hasnt "$out" "check-fast:" "(h) nothing of a run is printed"
echo "ok: (h) a binary that prints nothing is refused, not passed"

# (i) LUCINA_CHECK_TMP: what a run leaves in a temp dir -- its private binary copy, its raw log, and every temp file of the binary and
# its children -- is under that dir and not under /tmp. The launcher gets a `cargo` of its own so the private binary really is made,
# no CHECK_FAST_BIN/CHECK_FAST_LOG so nothing is handed to it, and a TMPDIR of its own that it must NOT keep.
fakebin=$tmp/fakebin
mkdir -p "$fakebin"
cat > "$fakebin/cargo" <<'EOF'
#!/usr/bin/env bash
# A cargo that "builds" by placing the stub where the launcher copies its private binary from.
mkdir -p "$CARGO_TARGET_DIR/release"
cp "$FAKE_CARGO_STUB" "$CARGO_TARGET_DIR/release/lucina-ci"
EOF
chmod +x "$fakebin/cargo"
n_in_tmp() { # n_in_tmp: how many files a check-fast run leaves loose at the top of /tmp (its private bin and log templates)
    local n=0 f
    for f in /tmp/lucina-ci.* /tmp/check-fast-log.*; do [ -e "$f" ] && n=$((n + 1)); done
    printf '%s' "$n"
}
ltmp=$tmp/lchecktmp
f=$tmp/i.txt
{
    printf 'ok   %-28s %5.1fs\n' fmt 0.3
    printf 'check-fast: 6s, 0 failed, 0 skipped\n'
} > "$f"
see=$tmp/i.see
before=$(n_in_tmp)
rc=0
OOS_OUT=$f OOS_RC=0 OOS_SEE=$see FAKE_CARGO_STUB=$stub \
    HOME=$home PATH=$fakebin:/usr/bin:/bin TMPDIR=$tdir LUCINA_CHECK_TMP=$ltmp \
    CHECK_FAST_BIN='' CHECK_FAST_LOG='' CARGO_TARGET_DIR='' \
    LUCINA_CHECK_FREE_FLOOR_GIB=0 \
    "$root/scripts/check-fast" > "$tmp/i.out" 2> "$tmp/i.err" || rc=$?
[ "$rc" -eq 0 ] || fail "(i) a clean run under LUCINA_CHECK_TMP failed: exit $rc, $(cat "$tmp/i.err")"
has "$tmp/i.out" "check-fast: 6s, 0 failed, 0 skipped" "(i) the run is the private copy of the stub's own output"
has "$see" "tmpdir=$ltmp" "(i) TMPDIR is LUCINA_CHECK_TMP, not the caller's TMPDIR ($tdir), which the launcher does not keep"
has "$see" "lucina-ci." "(i) the private binary copy is under $ltmp while the run goes ($(cat "$see"))"
has "$see" "check-fast-log." "(i) the raw log is under $ltmp while the run goes ($(cat "$see"))"
left=$(ls -A "$ltmp" | tr '\n' ' ')
[ -z "${left// /}" ] || fail "(i) the launcher left its private files in $ltmp: $left"
after=$(n_in_tmp)
[ "$before" -eq "$after" ] || fail "(i) the run left something new in /tmp: $before before, $after after"
echo "ok: (i) LUCINA_CHECK_TMP holds the private bin and log while the run goes, nothing new lands in /tmp, and both are removed after"

# (j) The queue is never taken from PATH. On GitHub's ubuntu-24.04 runners, PATH's `bq` is the Google Cloud SDK's BigQuery CLI,
# a different program of the same name, and the old lookup ran the check through it: it died on "no active account" and the public
# proof CI printed nothing (run 43276124239). So a `bq` first on PATH is a decoy here: it must not be run at all, and case (d)'s
# clean run stays exactly as clean. LUCINA_BQ (j2) is the one way of naming a queue that lives elsewhere.
unset LUCINA_BQ
gcloud=$tmp/gcloud-bin
mkdir -p "$gcloud"
cat > "$gcloud/bq" <<'EOF'
#!/usr/bin/env bash
# The BigQuery CLI of GitHub's runners: what `bq run --class short ...` answers there with no account configured, its two error
# lines and its exit code. It is not a build queue.
printf 'ERROR: (bq) You do not currently have an active account selected.\n' >&2
printf 'Please run:\n\n  $ gcloud auth login\n\nto obtain new credentials.\n' >&2
exit 1
EOF
chmod +x "$gcloud/bq"
run_case j "$tmp/d.txt" 0 "" "$gcloud"
[ "$rc" -eq 0 ] || fail "(j) a bq on PATH that is not our queue was used: exit $rc, $(cat "$er")"
OOS_OUT=$tmp/d.txt OOS_RC=0 "$stub" check-fast > "$tmp/j.direct" 2> /dev/null
samebytes j
hasnt "$er" "active account" "(j) the decoy was never run, so not one line of its error reaches the reader"
hasnt "$out" "active account" "(j) and nothing of it is on stdout either"
echo "ok: (j) a bq on PATH that is not the LucinaOS queue is ignored: the clean run still passes, byte for byte"

# (j2) A queue that IS ours, named by LUCINA_BQ, is used: with no arguments it prints the usage line the probe looks for, and as
# `run` it records its arguments and execs what comes after the `--`. So the check goes through it as a short job, and the bytes
# the launcher prints are still the binary's own.
ownq=$tmp/own-bq
rec=$tmp/j2.calls
cat > "$ownq" <<'EOF'
#!/usr/bin/env bash
# A build queue in the shape of tools/bq: no arguments prints its usage, exit code not ours to judge; `run` records the whole
# command line and then execs the command after the `--`.
if [ "${1:-}" = run ]; then
    printf '%s\n' "$*" > "$OWN_BQ_CALLS"
    while [ "$#" -gt 0 ] && [ "$1" != -- ]; do shift; done
    shift
    exec "$@"
fi
printf 'usage: bq run --class short|long|proof [--lock PATH] [--cores N] [--exclusive] [--who NAME] -- cmd args...\n'
exit 2
EOF
chmod +x "$ownq"
export LUCINA_BQ=$ownq OWN_BQ_CALLS=$rec
run_case j2 "$tmp/d.txt" 0
unset LUCINA_BQ OWN_BQ_CALLS
[ "$rc" -eq 0 ] || fail "(j2) a run through the queue named by LUCINA_BQ failed: exit $rc, $(cat "$er")"
nlines "$rec" '^run --class short --who ' 1 "(j2) exactly one queue call, and the usage probe was not one"
has "$rec" "run --class short --who $(basename "$root") -- $stub check-fast" "(j2) a short job, this worktree as --who, the binary after the --"
OOS_OUT=$tmp/d.txt OOS_RC=0 "$stub" check-fast > "$tmp/j2.direct" 2> /dev/null
samebytes j2
echo "ok: (j2) LUCINA_BQ naming our own queue puts the run through it, as --class short, with the same bytes out"

echo "check-fast-oos-test: pass"
