#!/usr/bin/env bash
# Shows that services/lucina-events/src/rules.rs is what keeps Verus on it: each line plants one bug in a copy of the shipped
# file, turned into what Verus reads exactly as scripts/verify-shipped.sh does, and Verus must refuse every mutant.
# Run on buildhost from the repo root, under verify-gate: verify-gate -- verification/juno-events/mutants.sh
set -uo pipefail
verus=${VERUS:-/home/user/tools/verus/verus-x86-linux/verus}
rules=services/lucina-events/src/rules.rs
work=$(mktemp -d "${TMPDIR:-/tmp}/juno-events-mutants.XXXXXX")
trap 'rm -rf "${work:?}"' EXIT

# What Verus reads from the shipped file, exactly the mechanical steps of scripts/verify-shipped.sh.
make_verus_input() {
    local body
    body=$(sed -E -e '/\/\/@omit$/d' -e 's/-> (.*[^ ]) \/\/@ret$/-> (ret: \1)/' -e 's/^( *)\/\/@ ?/\1\/*proof*\/ /' "$1")
    printf 'use vstd::prelude::*;\nverus! {\n%s\n}\nfn main() {}\n' "$body"
}

# The unchanged file must verify, or nothing below means anything.
make_verus_input "$rules" > "$work/base.rs"
if ! "$verus" "$work/base.rs" > "$work/base.out" 2>&1; then
    echo "BASE FAILS"
    cat "$work/base.out"
    exit 1
fi
echo "baseline: verifies"

fail=0
refused=0
weak=0
try() {
    local from=$1 to=$2
    local text; text=$(<"$rules")
    case "$text" in
        *"$from"*) ;;
        *) echo "NOT FOUND: $from"; fail=1; return ;;
    esac
    local mut; mut=${text/"$from"/"$to"}
    if [ "$mut" = "$text" ]; then echo "NOT CHANGED: $from"; fail=1; return; fi
    local in=$work/mutant-src.rs out=$work/mutant.rs
    printf "%s\n" "$mut" > "$in"
    make_verus_input "$in" > "$out"
    if "$verus" "$out" > "$work/verus.out" 2>&1; then
        echo "NOT REFUSED: $from -> $to"
        cat "$work/verus.out"
        fail=1
    else
        if grep -qE 'verification results|simplifies to|not satisfied|assertion failed|possible arithmetic|may not terminate|cannot show|recommendation not met' "$work/verus.out" && ! grep -q 'error\[E' "$work/verus.out"; then
            refused=$((refused + 1))
            echo "refused: $from -> $to ($(grep -c "^error" "$work/verus.out") errors)"
        else
            echo "refused (NO VERIFICATION RUN: compile or other error): $from -> $to ($(grep -c "^error" "$work/verus.out") errors)"
            weak=$((weak+1))
        fi
    fi
}
# The queue and its losses: a full queue drops the oldest, and a reader behind is told exactly how many it lost.
try "let head = if dropped { w.head + 1 } else { w.head };" "let head = w.head;"
try "Pushed { window: Window { head, next: w.next + 1 }, dropped }" "Pushed { window: Window { head, next: w.next }, dropped }"
try "let lost = if reset { 0 } else { start - since };" "let lost = 0;"
try "let count = if left < max { left } else { max };" "let count = left;"
try "let start = if reset || since < w.head { w.head } else { since };" "let start = if reset { w.head } else { since };"
# The order of each source: an event that is not the very next one is refused, and the gaps in between are counted.
try "if got <= l {" "if got < l {"
try "} else if got - l == 1 {" "} else if got - l <= 2 {"
# What may leave the program that made it: an app that opted out of introspection is not recorded, owner.input is content-free,
# and only the public fields of an event are exposed.
try "    src < SOURCES && !((src == SRC_APP || src == SRC_TOOLKIT) && introspect == INTROSPECT_NONE)" "    src < SOURCES && !(src == SRC_APP && introspect == INTROSPECT_NONE)"
try "kind[5] == b'.'" "kind[5] == b'_'"
try "if vis[i] == VIS_PUBLIC {" "if vis[i] != VIS_SECRET {"
try "if content_free {" "if false && content_free {"
# The filters: a kind pattern, a valid kind, a source mask, a subject prefix.
try "let n = if prefix { p - 1 } else { p };" "let n = if prefix { p - 2 } else { p };"
try "|| c == b'_' || c == b'.';" "|| c == b'_' || c == b'.' || c == b'*';"
try "(mask >> src) & 1 == 1" "mask & 1 == 1"
try "if subject.len() < n {" "if subject.len() + 1 < n {"
# The flood guard and the settle and battery rules: a per-app token bucket, the screen settling, and a battery reading reported
# per step of 5 %.
try "if tokens == 0 {" "if tokens == 0 && now_ms == 0 {"
try "tokens = if gained >= room { burst } else { tokens + gained as u32 };" "tokens = tokens + gained as u32;"
try "    present_seq > since_present &&" "    present_seq >= since_present &&"
try "(now as u16 >= l as u16 + 2" "(now as u16 >= l as u16 + 1"
[ "$fail" = 0 ] && echo "all refused"
echo "mutants: $refused refused by verification, $weak refused without a verification run"
if [ "${MUTANTS_STRICT:-0}" = "1" ] && [ "$weak" -gt 0 ]; then exit 1; fi
exit "$fail"
