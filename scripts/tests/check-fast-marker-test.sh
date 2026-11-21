#!/usr/bin/env bash
# scripts/tests/check-fast-marker-test.sh: the cache-marker step of scripts/check-fast (the lines between `marker-begin` and
# `marker-end`), run alone on a fake HOME. The marker is a file of our own beside Cargo's CACHEDIR.TAG, which is never changed. The
# last line is `check-fast-marker-test: pass`.
set -euo pipefail
root=$(cd "$(dirname "$0")/../.." && pwd)
base=${CHECK_FAST_MARKER_TEST_TMP:-${TMPDIR:-/tmp}}
mkdir -p "$base"
tmp=$(mktemp -d "$base/check-fast-marker.XXXXXX")
trap 'rm -rf "$tmp"' EXIT
fail() { printf 'FAIL: %s\n' "$*" >&2; exit 1; }
sed -n '/^# marker-begin$/,/^# marker-end$/p' "$root/scripts/check-fast" > "$tmp/step.sh"
[ -s "$tmp/step.sh" ] || fail "the marker step is not in scripts/check-fast"
home=$tmp/home
mkdir -p "$home/.cache"
step() { # step <cache_root>: the step as check-fast runs it, with HOME and cache_root set
    HOME=$home cache_root=$1 bash -euo pipefail "$tmp/step.sh"
}
cargo_tag='Signature: 8a477f597d28d172789f06886806bc55
# This file is a cache directory tag created by cargo.
# For information about cache directory tags see https://bford.info/cachedir/
'
# 1. A cache root that does not exist yet gets the marker.
new=$tmp/new-cache
step "$new"
[ "$(cat "$new/.lucina-check-target")" = "lucina-check-target" ] || fail "a new root has no marker"
# 2. The default root, which already carries Cargo's tag: the marker is added beside it and the tag is not touched.
def=$home/.cache/lucina-check-target
mkdir -p "$def"
printf '%s' "$cargo_tag" > "$def/CACHEDIR.TAG"
step "$def"
[ "$(cat "$def/.lucina-check-target")" = "lucina-check-target" ] || fail "the default root has no marker"
[ "$(cat "$def/CACHEDIR.TAG")" = "$(printf '%s' "$cargo_tag")" ] || fail "Cargo's CACHEDIR.TAG was changed"
# 3. The same root spelled another way (double slash, trailing slash, through a link) is the default root too.
rm "$def/.lucina-check-target"
step "$home/.cache//lucina-check-target/"
[ -f "$def/.lucina-check-target" ] || fail "an equivalent spelling of the default root got no marker"
rm "$def/.lucina-check-target"
ln -s "$def" "$tmp/link-to-default"
step "$tmp/link-to-default"
[ -f "$def/.lucina-check-target" ] || fail "a link to the default root got no marker"
# 4. Another existing cache is never marked.
other=$home/.cache/cargo-target
mkdir -p "$other"
step "$other"
[ ! -e "$other/.lucina-check-target" ] || fail "another existing cache was marked"
# 5. A good marker is kept as it is.
printf 'lucina-check-target\n' > "$def/.lucina-check-target"
step "$def" 2> "$tmp/err5"
[ "$(cat "$def/.lucina-check-target")" = "lucina-check-target" ] || fail "a good marker was rewritten"
[ ! -s "$tmp/err5" ] || fail "a good marker drew a warning"
# 6. A marker that is wrong is moved aside, with a warning, and the real one is written: other content, a dangling link (never
# written through), a folder, and a FIFO (which must not hang the step).
bad_case() { # bad_case <label> <command that makes the bad marker>
    rm -rf "$def/.lucina-check-target" "$def"/.sweep-parked-*
    eval "$2"
    timeout 20 bash -c 'HOME=$1 cache_root=$2 bash -euo pipefail "$3"' _ "$home" "$def" "$tmp/step.sh" 2> "$tmp/err6" || fail "$1: the step failed or hung"
    grep -q 'check-fast: warning' "$tmp/err6" || fail "$1: no warning"
    [ -f "$def/.lucina-check-target" ] && [ ! -L "$def/.lucina-check-target" ] || fail "$1: no real marker was written"
    [ "$(cat "$def/.lucina-check-target")" = "lucina-check-target" ] || fail "$1: wrong marker content"
    compgen -G "$def/.sweep-parked-*-lucina-check-target-marker" > /dev/null || fail "$1: the bad one was not kept aside under the parked prefix"
    [ -z "$(compgen -G "$def/.lucina-check-target.bad-*" || true)" ] || fail "$1: a sweepable name was used for the bad one"
}
bad_case "other content" "printf 'another-cache\n' > \"$def/.lucina-check-target\""
bad_case "a dangling link" "ln -s \"$tmp/nowhere\" \"$def/.lucina-check-target\""
[ ! -e "$tmp/nowhere" ] || fail "the marker was written through a link"
bad_case "a folder" "mkdir \"$def/.lucina-check-target\""
bad_case "a FIFO" "mkfifo \"$def/.lucina-check-target\""
# A big file is moved aside by its size, never read: the right line followed by 1 MiB of newlines, which a read would strip and accept.
bad_case "an oversized file" "{ printf 'lucina-check-target'; head -c 1048576 /dev/zero | tr '\\0' '\\n'; } > \"$def/.lucina-check-target\""
big=$(compgen -G "$def/.sweep-parked-*-lucina-check-target-marker")
[ "$(stat -c %s "$big")" -gt 512 ] || fail "the oversized file was not the one kept aside"
# 7. An existing folder that is not the default root is not marked (here no default cache exists at all, so a comparison of two empty
# readlink results must not read as equal).
home2=$tmp/home2
mkdir -p "$home2/.cache/other"
HOME=$home2 cache_root=$home2/.cache/other bash -euo pipefail "$tmp/step.sh"
[ ! -e "$home2/.cache/other/.lucina-check-target" ] || fail "an existing folder was marked although no default cache exists"
# 8. A cache root that cannot be created warns and does not fail the step (the gate must not stop on it).
HOME=$home2 cache_root=/proc/no-such-folder/cache bash -euo pipefail "$tmp/step.sh" 2> "$tmp/err8" || fail "an uncreatable cache root failed the step"
grep -q "check-fast: warning: cannot create" "$tmp/err8" || fail "no warning for an uncreatable cache root"
echo "check-fast-marker-test: pass"
