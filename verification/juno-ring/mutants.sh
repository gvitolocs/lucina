#!/usr/bin/env bash
# Shows that verification/juno-ring/rules.rs can fail: each line plants one bug in a copy of the shipped file, and Verus must refuse it.
# Run on buildhost from the repo root, under verify-gate.
set -uo pipefail
verus=${VERUS:-/home/user/tools/verus/verus-x86-linux/verus}
work=$(mktemp -d "${TMPDIR:-/tmp}/juno-ring-mutants.XXXXXX")
trap 'rm -rf "${work:?}"' EXIT
fail=0
refused=0
weak=0
try() {
    local from=$1 to=$2
    # Shown on one line: a planted bug may span lines.
    local shown="${from//$'\n'/\\n} -> ${to//$'\n'/\\n}"
    mkdir -p "$work/kernel/lucina-ring/src" "$work/verification/juno-ring"
    cp kernel/lucina-ring/src/lib.rs "$work/kernel/lucina-ring/src/"
    cp verification/juno-ring/rules.rs "$work/verification/juno-ring/"
    local target=$work/kernel/lucina-ring/src/lib.rs
    local text; text=$(<"$target")
    case "$text" in
        *"$from"*) ;;
        *) echo "NOT FOUND: ${shown% -> *}"; fail=1; return ;;
    esac
    printf "%s\n" "${text/"$from"/"$to"}" > "$target"
    if cmp -s "$target" kernel/lucina-ring/src/lib.rs; then echo "NOT CHANGED: ${shown% -> *}"; fail=1; return; fi
    if "$verus" "$work/verification/juno-ring/rules.rs" > "$work/out" 2>&1; then
        echo "NOT REFUSED: $shown"; fail=1
    else
        if grep -qE 'verification results|simplifies to|not satisfied|assertion failed|possible arithmetic|may not terminate|cannot show|recommendation not met' "$work/out" && ! grep -q 'error\[E' "$work/out"; then
            refused=$((refused + 1))
            echo "refused: $shown ($(grep -c "^error" "$work/out") errors)"
        else
            echo "refused (NO VERIFICATION RUN: compile or other error): $shown ($(grep -c "^error" "$work/out") errors)"
            weak=$((weak+1))
        fi
    fi
}
# The unchanged files must verify, or every "refused" below would mean nothing.
if "$verus" verification/juno-ring/rules.rs > "$work/baseline" 2>&1; then
    echo "baseline: verifies"
else
    cat "$work/baseline"; echo "mutants: the unchanged files do not verify"; exit 1
fi
# The offset is within a page.
try 'pos % PAGE)' 'pos % (PAGE - 1))'
# The page is the slot of the position in the ring, not how many times it wrapped.
try '((pos % cap) / PAGE' '((pos / cap) / PAGE'
# A piece of a copy stops at the end of its page, and at the end of the copy.
try 'let room = PAGE - offset;' 'let room = PAGE - offset + 1;'
try 'if left < room { left } else { room }' 'if left < room { room } else { left }'
# The length wraps with the counters.
try 'tail.wrapping_sub(head)' 'if tail >= head { tail - head } else { 0 }'
# Room is the capacity less what is held.
try 'cap - len(head, tail)' 'cap + 1 - len(head, tail)'
# A page is in use when head is inside it, or its start is within the data, around the ring.
try '(ahead < held || behind < PAGE)' '(ahead < held)'
try 'let ahead = (start + cap - h) % cap;' 'let ahead = (start + cap - h) % (2 * cap);'
# The page head stopped in is finished only when the ring is empty, and then it is.
try $'    } else {\n        head_page\n    };' $'    } else {\n        head_page.wrapping_add(PAGE)\n    };'
try 'let end = if head == tail &&' 'let end = if head != tail &&'
# The first page stays when the data wrapped round into it.
try 'tail.wrapping_sub(start) > cap {' 'tail.wrapping_sub(start) > cap + PAGE {'
# Every page the read took is reported, not one fewer.
try 'let mut count = end.wrapping_sub(start) / PAGE;' 'let mut count = end.wrapping_sub(start) / PAGE / 2;'
echo "mutants: $refused refused by verification, $weak refused without a verification run"
if [ "${MUTANTS_STRICT:-0}" = "1" ] && [ "$weak" -gt 0 ]; then exit 1; fi
exit $fail
