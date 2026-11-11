#!/usr/bin/env bash
# Shows that boards/fortuna-library/src/rules.rs is what keeps Verus on it: each line plants one bug in a copy of the shipped
# file, turned into what Verus reads exactly as scripts/verify-shipped.sh does, and Verus must refuse every mutant.
# Run on buildhost from the repo root, under verify-gate: verify-gate -- verification/fortuna-library/mutants.sh
# (or on buildhost2: verus-remote <a copy holding boards/fortuna-library/src/rules.rs and this script> env VERUS=verus verification/fortuna-library/mutants.sh)
set -uo pipefail
verus=${VERUS:-/home/user/tools/verus/verus-x86-linux/verus}
rules=boards/fortuna-library/src/rules.rs
work=$(mktemp -d "${TMPDIR:-/tmp}/fortuna-library-mutants.XXXXXX")
trap 'rm -rf "${work:?}"' EXIT

make_verus_input() {
    local body
    body=$(sed -E -e '/\/\/@omit$/d' -e 's/-> (.*[^ ]) \/\/@ret$/-> (ret: \1)/' -e 's/^( *)\/\/@ ?/\1\/*proof*\/ /' "$1")
    printf 'use vstd::prelude::*;\nverus! {\n%s\n}\nfn main() {}\n' "$body"
}

make_verus_input "$rules" > "$work/base.rs"
if ! "$verus" "$work/base.rs" > "$work/base.out" 2>&1; then
    echo "BASE FAILS"
    cat "$work/base.out"
    exit 1
fi
echo "baseline: verifies"

fail=0
weak=0
killed=0
try() {
    local from=$1 to=$2
    local text; text=$(<"$rules")
    case "$text" in
        *"$from"*) ;;
        *) echo "NOT FOUND: $from"; fail=1; return ;;
    esac
    local mut; mut=${text/"$from"/"$to"}
    if [ "$mut" = "$text" ]; then echo "NOT CHANGED: $from"; fail=1; return; fi
    printf "%s\n" "$mut" > "$work/mutant-src.rs"
    make_verus_input "$work/mutant-src.rs" > "$work/mutant.rs"
    if "$verus" "$work/mutant.rs" > "$work/verus.out" 2>&1; then
        echo "NOT REFUSED: $from -> $to"
        fail=1
    else
        if grep -qE 'verification results|simplifies to|not satisfied|assertion failed|possible arithmetic|may not terminate|cannot show|recommendation not met' "$work/verus.out" && ! grep -q 'error\[E' "$work/verus.out"; then
            killed=$((killed + 1))
            echo "refused: $from -> $to"
        else
            echo "refused (NO VERIFICATION RUN: compile or other error): $from -> $to"
            weak=$((weak+1))
        fi
    fi
}

# fold: the wrong range, the wrong distance
try 'if b >= 65 && b <= 90 {' 'if b >= 65 && b <= 91 {'
try '        b + 32' '        b + 31'
# eq_at: the length check off by one, the byte test inverted, a skipped byte
try 'if at > h.len() || n.len() > h.len() - at {' 'if at > h.len() || n.len() > h.len() - at + 1 {'
try 'if h[at + k] != n[k] {' 'if h[at + k] == n[k] {'
try '        k += 1;
    }
    true' '        k += 2;
    }
    true'
# find: skips the last place, starts one too late, returns the wrong index
try 'while i <= last' 'while i < last'
try 'let mut i = from;' 'let mut i = from + 1;'
try 'return Some(i);' 'return Some(i + 1);'
# rank: start rank given to an inside match, a miss reported as inside
try '            RANK_INSIDE
        }' '            RANK_START
        }'
try '            RANK_NONE
        }' '            RANK_INSIDE
        }'
# banner_span: a banner allowed in the header, one allowed past the end, the wrong length
try 'if off < HEADER_END {' 'if off < 0x100 {'
try 'if s + BANNER_V1_LEN > file_len {' 'if s + BANNER_V1_LEN > file_len + 1 {'
try 'Some((s, s + BANNER_V1_LEN))' 'Some((s, s + 0x800))'

# lemma_narrow_words: without the prefix condition, a longer query says nothing about a shorter one
try '//@     requires q.len() <= q2.len(), q2.subrange(0, q.len() as int) == q, spec_matches_all(h, q2),' '//@     requires q.len() <= q2.len(), spec_matches_all(h, q2),'
# spec_word: a word may run over a 0, so the folded query's words would not be what index.rs splits
try '//@     && forall|j: int| i <= j < k ==> #[trigger] q[j] != 0' '//@     && forall|j: int| i < j < k ==> #[trigger] q[j] != 0'
echo "mutants refused: $killed"
echo "mutants: $killed refused by verification, $weak refused without a verification run"
if [ "${MUTANTS_STRICT:-0}" = "1" ] && [ "$weak" -gt 0 ]; then exit 1; fi
exit "$fail"
