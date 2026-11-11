#!/usr/bin/env bash
# Shows that verification/juno-files-keys/rules.rs can fail: each line plants one bug in a copy of the shipped file, and Verus must refuse it.
# Run on buildhost from the repo root, under verify-gate (or on buildhost2 with verus-remote).
set -uo pipefail
verus=${VERUS:-verus}
[ -x /home/user/tools/verus/verus-x86-linux/verus ] && verus=${VERUS:-/home/user/tools/verus/verus-x86-linux/verus}
work=$(mktemp -d "${TMPDIR:-/tmp}/juno-files-keys-mutants.XXXXXX")
trap 'rm -rf "${work:?}"' EXIT
fail=0 refused=0 total=0
weak=0
src=services/lucina-nav/src/lib.rs
try() {
    local from=$1 to=$2
    local shown="${from//$'\n'/\\n} -> ${to//$'\n'/\\n}"
    total=$((total + 1))
    mkdir -p "$work/services/lucina-nav/src" "$work/verification/juno-files-keys"
    cp $src "$work/services/lucina-nav/src/"
    cp verification/juno-files-keys/rules.rs "$work/verification/juno-files-keys/"
    local target=$work/$src
    local text; text=$(<"$target")
    case "$text" in
        *"$from"*) ;;
        *) echo "NOT FOUND: ${shown% -> *}"; fail=1; return ;;
    esac
    printf "%s\n" "${text/"$from"/"$to"}" > "$target"
    if cmp -s "$target" $src; then echo "NOT CHANGED: ${shown% -> *}"; fail=1; return; fi
    if "$verus" "$work/verification/juno-files-keys/rules.rs" > "$work/out" 2>&1; then
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
if "$verus" verification/juno-files-keys/rules.rs > "$work/baseline" 2>&1; then
    echo "baseline: verifies"
else
    cat "$work/baseline"; echo "mutants: the unchanged files do not verify"; exit 1
fi
# Next or Previous goes two rows, wraps at an end, or loses its start.
try 'Some(i) if i < len - 1 => Some(i + 1),' 'Some(i) if i < len - 1 => Some(i + 2),'
try '        Some(i) if i < len => Some(i),
        Some(_) => Some(len - 1),
    }
}

/// One row up' '        Some(i) if i < len => Some(0),
        Some(_) => Some(len - 1),
    }
}

/// One row up'
try 'match cur {
        None => Some(0),' 'match cur {
        None => Some(len - 1),'
try 'Some(i) if i == 0 => Some(0),' 'Some(i) if i == 0 => Some(len - 1),'
try 'Some(i) if i < len => Some(i - 1),' 'Some(i) if i < len => Some(i),'
# Home and End select something in an empty list, or the wrong row.
try 'if len == 0 { None } else { Some(0) }' 'Some(0)'
try 'if len == 0 { None } else { Some(len - 1) }' 'if len == 0 { None } else { Some(len) }'
# A stale index is left outside the list.
try 'Some(_) => Some(len - 1),
        None => None,' 'Some(i) => Some(i),
        None => None,'
# Pages run past an end or wrap.
try 'if page < len - from { Some(from + page) } else { Some(len - 1) }' 'Some(from + page)'
try 'if page <= from { Some(from - page) } else { Some(0) }' 'if page <= from { Some(from - page) } else { Some(len - 1) }'
# Grid rows: one past the end, the wrong tile in the short last row, a wrap at the top.
try 'if cols < len - i {' 'if cols <= len - i {'
try '        Some(len - 1)
    } else {
        Some(i)
    }' '        Some(0)
    } else {
        Some(i)
    }'
try 'if cols <= i { Some(i - cols) } else { Some(i) }' 'if cols <= i { Some(i - cols) } else { Some(len - 1) }'
# A viewer steps onto the file it is on, ignores where it is, or goes the wrong way at an end.
try 'let mut k = from + 1;' 'let mut k = from;'
try 'let mut k = if from < n { from } else { n };' 'let mut k = n;'
try 'Step::First => if j < at { j } else { at },' 'Step::First => j,'
try '        None => at,
    }
}' '        None => 0,
    }
}'
echo "juno-files-keys mutants: $refused of $total refused"
echo "mutants: $refused refused by verification, $weak refused without a verification run"
if [ "${MUTANTS_STRICT:-0}" = "1" ] && [ "$weak" -gt 0 ]; then exit 1; fi
exit $fail
