#!/usr/bin/env bash
# Shows that verification/net-installer/rules.rs can fail: each line plants one bug in a copy of the shipped file, and Verus must refuse it.
# Run on buildhost from the repo root, under verify-gate.
set -uo pipefail
verus=${VERUS:-/home/user/tools/verus/verus-x86-linux/verus}
work=$(mktemp -d "${TMPDIR:-/tmp}/net-installer-mutants.XXXXXX")
trap 'rm -rf "${work:?}"' EXIT
fail=0
refused=0
weak=0
try() {
    local file=$1 from=$2 to=$3
    mkdir -p "$work/services/lucina-release/src" "$work/verification/net-installer"
    mkdir -p "$work/kernel/lucina-boot/src"
    cp services/lucina-release/src/rules.rs "$work/services/lucina-release/src/"
    cp kernel/lucina-boot/src/board.rs "$work/kernel/lucina-boot/src/"
    cp verification/net-installer/rules.rs "$work/verification/net-installer/"
    local target=$work/$file
    local text; text=$(<"$target")
    case "$text" in
        *"$from"*) ;;
        *) echo "NOT FOUND: $from"; fail=1; return ;;
    esac
    printf "%s\n" "${text/"$from"/"$to"}" > "$target"
    if cmp -s "$target" "$file"; then echo "NOT CHANGED: $from"; fail=1; return; fi
    if "$verus" "$work/verification/net-installer/rules.rs" > "$work/out" 2>&1; then
        echo "NOT REFUSED: $from -> $to"; fail=1
    else
        if grep -qE 'verification results|simplifies to|not satisfied|assertion failed|possible arithmetic|may not terminate|cannot show|recommendation not met' "$work/out" && ! grep -q 'error\[E' "$work/out"; then
            refused=$((refused + 1))
            echo "refused: $from -> $to ($(grep -c '^error' "$work/out") errors)"
        else
            echo "refused (NO VERIFICATION RUN: compile or other error): $from -> $to ($(grep -c '^error' "$work/out") errors)"
            weak=$((weak+1))
        fi
    fi
}
# The unchanged files must verify, or every "refused" below would mean nothing.
if "$verus" verification/net-installer/rules.rs > "$work/baseline" 2>&1; then
    echo "baseline: verifies"
else
    cat "$work/baseline"; echo "mutants: the unchanged files do not verify"; exit 1
fi
R=services/lucina-release/src/rules.rs
try $R 'let n = (size - 1) / chunk + 1;' 'let n = size / chunk;'
try $R 'if size == 0 || chunk == 0 {' 'if chunk == 0 {'
try $R 'if index >= n {' 'if index > n {'
try $R 'let start = index * chunk;' 'let start = index * chunk + 1;'
try $R 'let len = if left < chunk { left } else { chunk };' 'let len = chunk;'
try $R 'let left = size - start;' 'let left = size - start + 1;'
try $R 'if verified_prefix < count {' 'if verified_prefix <= count {'
try $R $'bool {\n    chunk_hash_ok && index == verified_prefix' $'bool {\n    index == verified_prefix'
try $R $'bool {\n    chunk_hash_ok && index == verified_prefix' $'bool {\n    chunk_hash_ok && index >= verified_prefix'
try $R $'bool {\n    verified_prefix == count && whole_hash_ok' $'bool {\n    verified_prefix == count'
try $R $'bool {\n    verified_prefix == count && whole_hash_ok' $'bool {\n    verified_prefix >= count && whole_hash_ok'
try $R 'Some(i) => new >= i,' 'Some(i) => new + 1 >= i,'
try $R 'None => true,' 'None => false,'
try $R $'bool {\n    target_in_compiled_rule && named_in_manifest' $'bool {\n    target_in_compiled_rule || named_in_manifest'
try $R $'bool {\n    target_in_compiled_rule && named_in_manifest' $'bool {\n    named_in_manifest'
B=kernel/lucina-boot/src/board.rs
try $B 'if s.len() == 0 || s.len() > 40 {' 'if s.len() > 40 {'
try $B 'if s.len() == 0 || s.len() > 40 {' 'if s.len() == 0 || s.len() > 41 {'
try $B '(b >= b'"'"'a'"'"' && b <= b'"'"'z'"'"')' '(b >= b'"'"'a'"'"' && b <= b'"'"'{'"'"')'
try $B '|| b == b'"'"'-'"'"') {' '|| b == b'"'"'-'"'"' || b == b'"'"'_'"'"') {'
try $B 'if !((b >= b' 'if ((b >= b'
try $B $'bool {\n    smbios_present && manufacturer_equal && product_equal' $'bool {\n    manufacturer_equal && product_equal'
try $B $'bool {\n    smbios_present && manufacturer_equal && product_equal' $'bool {\n    smbios_present && manufacturer_equal'
try $B $'bool {\n    smbios_present && manufacturer_equal && product_equal' $'bool {\n    !smbios_present || (manufacturer_equal && product_equal)'
try $B 'Some(n) => Some((true, n)),' 'Some(n) => Some((false, n)),'
try $B $'None => match smbios_line {\n            Some(n) => Some((false, n)),' $'None => match smbios_line {\n            Some(n) => Some((true, n)),'
try $B $'    match compatible_line {\n        Some(n) => Some((true, n)),\n        None => match smbios_line {\n            Some(n) => Some((false, n)),\n            None => None,\n        },\n    }' $'    match smbios_line {\n        Some(n) => Some((false, n)),\n        None => match compatible_line {\n            Some(n) => Some((true, n)),\n            None => None,\n        },\n    }'
[ "$fail" = 0 ] && echo "mutants: every planted bug was refused" || echo "mutants: FAILED"
echo "mutants: $refused refused by verification, $weak refused without a verification run"
if [ "${MUTANTS_STRICT:-0}" = "1" ] && [ "$weak" -gt 0 ]; then exit 1; fi
exit $fail
