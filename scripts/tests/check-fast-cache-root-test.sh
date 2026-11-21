#!/usr/bin/env bash
# scripts/tests/check-fast-cache-root-test.sh: a cache root that cannot be created must not stop the gate. It runs the real launcher
# over a stub of `lucina-ci check-fast` (CHECK_FAST_BIN) with LUCINA_CHECK_CACHE under /proc, where nothing can be created, and no
# CARGO_TARGET_DIR, so the target dir defaults to a folder inside that cache root. The marker step has its own test
# (check-fast-marker-test.sh, which runs only the lines between marker-begin and marker-end); this one covers the target-dir lines
# after it. The last line is `check-fast-cache-root-test: pass`.
set -euo pipefail
root=$(cd "$(dirname "$0")/../.." && pwd)
base=${CHECK_FAST_CACHE_ROOT_TEST_TMP:-${TMPDIR:-/tmp}}
mkdir -p "$base"
tmp=$(mktemp -d "$base/check-fast-cache-root.XXXXXX")
trap 'rm -rf "$tmp"' EXIT
fail() { printf 'FAIL: %s\n' "$*" >&2; [ ! -f "${er:-}" ] || head -n 30 "$er" >&2; exit 1; }
home=$tmp/home
mkdir -p "$home" "$tmp/pl/$(basename "$root")/scripts"
launcher=$tmp/pl/$(basename "$root")/scripts/check-fast
# A copy whose one assignment of `qhome` (the passwd lookup of this uid's home) is the fake home, as in check-fast-oos-test.sh.
awk -v h="$home" '
    /^qhome=\$\($/ { print "qhome=\"" h "\""; skip = 1; next }
    skip && /^\) \|\| qhome=""$/ { skip = 0; next }
    skip { next }
    { print }' "$root/scripts/check-fast" > "$launcher"
chmod +x "$launcher"
stub=$tmp/stub
cat > "$stub" <<'STUB'
#!/usr/bin/env bash
# `sweep-target` and anything else succeeds silently; `check-fast` leaves a file saying it ran.
[ "${1:-}" = check-fast ] || exit 0
: > "$STUB_RAN"
echo "check-fast: 1s, 0 failed, 0 skipped"
exit 0
STUB
chmod +x "$stub"
er=$tmp/err
rc=0
env -u BQ_SLOT -u BQ_SLOT_PID -u CARGO_TARGET_DIR HOME="$home" PATH=/usr/bin:/bin TMPDIR="$tmp" LUCINA_CHECK_TMP="$tmp" \
    CHECK_FAST_BIN="$stub" CHECK_FAST_LOG="$tmp/raw" STUB_RAN="$tmp/ran" \
    LUCINA_CHECK_CACHE=/proc/no-such-folder/cache LUCINA_CHECK_FREE_FLOOR_GIB=0 \
    "$launcher" > "$tmp/out" 2> "$er" || rc=$?
[ "$rc" -eq 0 ] || fail "an uncreatable cache root stopped the launcher (exit $rc)"
[ -e "$tmp/ran" ] || fail "the check never ran"
grep -qx "check-fast: warning: cannot create /proc/no-such-folder/cache" "$er" || fail "no warning for the cache root"
grep -q 'check-fast: warning: cannot create /proc/no-such-folder/cache/wt-' "$er" || fail "no warning for the target dir"
echo "check-fast-cache-root-test: pass"
