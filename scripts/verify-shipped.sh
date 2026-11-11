#!/usr/bin/env bash
# Verus on the functions that ship, not on a model of them (D-011). Such a file is plain Rust with its proof in `//@` lines. This
# script makes what Verus reads by mechanical steps only, and checks that undoing them gives the shipped text back:
#   `-> T //@ret`   becomes `-> (ret: T)`, the one thing Rust's syntax has no comment form for
#   `... //@omit`   is dropped (a derive Verus does not check, and that holds no logic)
#   `//@ proof`     becomes `proof`
# Run through verify-gate: verify-gate -- scripts/verify-shipped.sh
set -euo pipefail

root=$(cd "$(dirname "$0")/.." && pwd)
verus=${VERUS:-/home/user/tools/verus/verus-x86-linux/verus}
files=(drivers/lucina-adreno/src/yuv_rules.rs apps/lucina-glass/src/rules.rs apps/lucina-glass/src/home_rules.rs apps/lucina-app-office/src/member_path.rs drivers/lucina-q6/src/rules.rs drivers/lucina-venus/src/rules.rs services/lucina-ax/src/rules.rs services/lucina-events/src/rules.rs services/lucina-intents/src/rules.rs drivers/lucina-i2c-hid/src/tp_rules.rs drivers/lucina-modem-rpmh/src/vote_rules.rs drivers/lucina-video/src/mse_rules.rs drivers/lucina-uvc/src/rules.rs kernel/lucina-evlog/src/rules.rs kernel/lucina-pon/src/rules.rs kernel/lucina-hostname/src/rules.rs kernel/lucina-oom/src/rules.rs boards/fortuna-library/src/rules.rs boards/fortuna-ring/src/rules.rs boards/fortuna-look/src/rules.rs boards/fortuna-tls/src/rules.rs boards/fortuna-secretary/src/rules.rs boards/fortuna-voice/src/rules.rs drivers/lucina-voice-stream/src/rules.rs kernel/lucina-wdt/src/rules.rs kernel/lucina-crash/src/layout.rs kernel/lucina-crash/src/waits.rs services/lucina-wm-rules/src/snap_rules.rs services/lucina-wm-rules/src/mru_rules.rs services/lucina-wm-rules/src/tile_rules.rs)
# VERIFY_SHIPPED_ONLY=<file>: only that file (one unit of the CI matrix, tools/ci). It must be in the list.
if [ -n "${VERIFY_SHIPPED_ONLY:-}" ]; then
    found=0; for f in "${files[@]}"; do [ "$f" = "$VERIFY_SHIPPED_ONLY" ] && found=1; done
    [ "$found" = 1 ] || { echo "verify-shipped: $VERIFY_SHIPPED_ONLY is not in the list" >&2; exit 2; }
    files=("$VERIFY_SHIPPED_ONLY")
fi
work=$(mktemp -d "${TMPDIR:-/tmp}/verify-shipped.XXXXXX")
trap 'rm -rf "$work"' EXIT
status=0
for f in "${files[@]}"; do
    out=$work/$(basename "$f")
    # The proof lines stay tagged, so that taking them out again can be compared with the shipped text.
    body=$(sed -E -e '/\/\/@omit$/d' -e 's/-> (.*[^ ]) \/\/@ret$/-> (ret: \1)/' -e 's/^( *)\/\/@ ?/\1\/*proof*\/ /' "$root/$f")
    printf 'use vstd::prelude::*;\nverus! {\n%s\n}\nfn main() {}\n' "$body" > "$out"
    a=$(sed -E -e '/^ *\/\/@/d' -e '/\/\/@omit$/d' -e 's/ \/\/@ret$//' "$root/$f")
    b=$(printf '%s\n' "$body" | grep -vF '/*proof*/' | sed -E 's/-> \(ret: (.*)\)$/-> \1/')
    if [ "$a" != "$b" ]; then
        echo "FAIL $f: what Verus reads is not the shipped code"
        diff <(echo "$a") <(echo "$b") | head -20
        status=1
        continue
    fi
    if "$verus" "$out"; then echo "ok $f"; else echo "FAIL $f"; status=1; fi
done
exit "$status"
