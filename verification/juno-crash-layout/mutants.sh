#!/usr/bin/env bash
# Shows that kernel/lucina-crash/src/layout.rs and waits.rs are what keep Verus on them: each line plants one bug in a copy of the shipped
# file, turned into what Verus reads exactly as scripts/verify-shipped.sh does, and Verus must refuse every mutant.
# Run on buildhost from the repo root, under verify-gate: verify-gate -- verification/juno-crash-layout/mutants.sh
set -uo pipefail
verus=${VERUS:-/home/user/tools/verus/verus-x86-linux/verus}
rules=kernel/lucina-crash/src/layout.rs
work=$(mktemp -d "${TMPDIR:-/tmp}/juno-crash-layout-mutants.XXXXXX")
trap 'rm -rf "${work:?}"' EXIT

# What Verus reads from the shipped file, exactly the mechanical steps of scripts/verify-shipped.sh.
make_verus_input() {
    local body
    body=$(sed -E -e '/\/\/@omit$/d' -e 's/-> (.*[^ ]) \/\/@ret$/-> (ret: \1)/' -e 's/^( *)\/\/@ ?/\1\/*proof*\/ /' "$1")
    printf 'use vstd::prelude::*;\nverus! {\n%s\n}\nfn main() {}\n' "$body"
}

# The unchanged file must verify, or nothing below means anything.
base() {
    make_verus_input "$rules" > "$work/base.rs"
    if ! "$verus" "$work/base.rs" > "$work/base.out" 2>&1; then
        echo "BASE FAILS: $rules"
        cat "$work/base.out"
        exit 1
    fi
    echo "base verifies: $rules"
}
base

fail=0
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
        echo "refused: $from -> $to ($(grep -c "^error" "$work/verus.out") errors)"
    fi
}
# Records stay a sixteenth in whole pages, consoles leave room for the records and the reserve, format 4's tail is reserved.
try "    (size / 16) / 4096 * 4096
}" "    (size / 8) / 4096 * 4096
}"
try "    if version >= 4 && size >= RESERVE_FROM {" "    if version >= 3 && size >= RESERVE_FROM {"
try "        TAIL_RESERVE
    } else {" "        TAIL_RESERVE / 2
    } else {"
try "    let left = size - FIRST_ZONE - RECORDS * records - reserved;" "    let left = size - FIRST_ZONE - RECORDS * records;"
try "    (left / 2) / 4096 * 4096
}" "    left / 4096 * 4096
}"
try "    FIRST_ZONE + RECORDS * records + 2 * consoles" "    FIRST_ZONE + RECORDS * records + consoles"
try "    v4 <= v3" "    v4 < v3"

# The wait-chain reader: a holder neither waits nor idles, the search starts at `from` and names the CPU it checked, and one
# lock is reported only when every waiter waits on it.
rules=kernel/lucina-crash/src/waits.rs
base
try "        && waiting[cpu] == 0
        && !in_wfi[cpu]" \
    "        && waiting[cpu] == 0"
try "        && cpu < seen.len()
        && seen[cpu]" \
    "        && cpu < seen.len()"
try "    let mut k: usize = from;" "    let mut k: usize = from + 1;"
try "            return Some(k);" "            return Some(k + 1);"
try "            } else if lock != found {" "            } else if lock == found {"
try "                found = lock;
                first = k;" "                found = lock;"
try "        Some(waiting[first])" "        Some(waiting[0])"
exit "$fail"
