#!/usr/bin/env bash
# Shows that verification/juno-disk/rules.rs can fail: each line plants one bug in a copy of the shipped files, and Verus must refuse it.
# Run on buildhost from the repo root, under verify-gate.
set -uo pipefail
verus=${VERUS:-/home/user/tools/verus/verus-x86-linux/verus}
work=$(mktemp -d "${TMPDIR:-/tmp}/juno-disk-mutants.XXXXXX")
trap 'rm -rf "${work:?}"' EXIT
fail=0
refused=0
weak=0
try() {
    local file=$1 from=$2 to=$3
    mkdir -p "$work/services/lucina-remote/src" "$work/drivers/lucina-ufs/src" "$work/kernel/lucina-boot/src" "$work/verification/juno-disk"
    cp services/lucina-remote/src/disk_rules.rs "$work/services/lucina-remote/src/"
    cp drivers/lucina-ufs/src/write_rule.rs "$work/drivers/lucina-ufs/src/"
    cp kernel/lucina-boot/src/trial.rs "$work/kernel/lucina-boot/src/"
    cp verification/juno-disk/rules.rs "$work/verification/juno-disk/"
    local target=$work/$file
    local text; text=$(<"$target")
    case "$text" in
        *"$from"*) ;;
        *) echo "NOT FOUND: $from"; fail=1; return ;;
    esac
    printf "%s\n" "${text/"$from"/"$to"}" > "$target"
    if cmp -s "$target" "$file"; then echo "NOT CHANGED: $from"; fail=1; return; fi
    if "$verus" "$work/verification/juno-disk/rules.rs" > "$work/out" 2>&1; then
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
if "$verus" verification/juno-disk/rules.rs > "$work/baseline" 2>&1; then
    echo "baseline: verifies"
else
    cat "$work/baseline"; echo "mutants: the unchanged files do not verify"; exit 1
fi
R=services/lucina-remote/src/disk_rules.rs
U=drivers/lucina-ufs/src/write_rule.rs
B=kernel/lucina-boot/src/trial.rs
try $R 'f.header_size > 4096' 'f.header_size > 4097'
try $R 'f.header_size < 92' 'f.header_size < 91'
try $R 'f.my_lba != my || ' ''
try $R 'bytes > room' 'bytes > room + 1'
try $R 'last - blocks };' 'last - blocks - 1 };'
try $R 'f.first_usable < 2 + blocks || ' ''
try $R 'f.last_usable >= last - blocks' 'f.last_usable > last - blocks'
try $R 'start < first_usable' 'start + 1 < first_usable'
try $R 'start % 256 != 0 || ' ''
try $R 'free - roots < 256' 'free < roots'
try $R 'data_first: start + roots,' 'data_first: start + roots - 1,'
try $R 'lba >= 1 && ' ''
try $R 'lba < first_usable || lba > last_usable' 'lba <= first_usable || lba > last_usable'
try $R 'first <= esp_last || ' ''
try $R 'last > last_usable' 'last > last_usable + 1'
try $R 'count <= image_blocks - first' 'count <= image_blocks'
try $R 'Some(Slot::A) => Slot::B' 'Some(Slot::A) => Slot::A'
try $R 'if same_slot(b, slot) {' 'if !same_slot(b, slot) {'
try $R 'if same_slot(x, slot) {' 'if !same_slot(x, slot) {'
try $R 'same_slot(p, slot) && !replaces_prev' 'same_slot(p, slot) && replaces_prev'
try $R '(Slot::A, Slot::A) | (Slot::B, Slot::B) => true' '(Slot::A, Slot::A) => true'
try $R 'free as u128 + reclaimable as u128 >=' 'free as u128 + reclaimable as u128 + 1 >='
try $R 'initrd as u128 + margin as u128' 'initrd as u128'
try $R 'if !esp_has_room(free, reclaimable, kernel, initrd, margin) {' 'if !esp_has_room(free, reclaimable, kernel, 0, margin) {'
try $U 'lun != main' 'lun == main'
try $U 'boot_lun_id != 0' 'boot_lun_id > 1'
try $U 'c < largest' 'c + 1 < largest'
try $U 'return writable;' 'return true;'
try $U 'lun & WLUN_FLAG != 0' 'lun & 0x40 != 0'
try $U $'    op == TEST_UNIT_READY\n' $'    op == 0x1b || op == TEST_UNIT_READY\n'
try $B 'let roll_back = have_previous && last_bad && bad >= SETTLE_BAD;' 'let roll_back = last_bad && bad >= SETTLE_BAD;'
try $B 'if p.boots >= SETTLE_BOOTS {' 'if p.boots > SETTLE_BOOTS {'
try $B 'boots: p.boots + 1, bad' 'boots: p.boots, bad'
try $B 'let roll_back = have_previous && last_bad && bad >= SETTLE_BAD;' 'let roll_back = have_previous && bad >= SETTLE_BAD;'
# AB-3: the settle window in the record.
try $B $'state: State::Good,\n                        tries: r.tries - 1,' $'state: State::Good,\n                        tries: r.tries,'
try $B '} else if have_previous {' '} else if false {'
try $B 'tries: SETTLE_BAD, settle: r.settle - 1' 'tries: SETTLE_BAD, settle: r.settle'
try $B 'tries: SETTLE_BAD, settle: SETTLE_BOOTS }' 'tries: SETTLE_BAD, settle: 0 }'
[ "$fail" = 0 ] && echo "mutants: every planted bug was refused" || echo "mutants: FAILED"
echo "mutants: $refused refused by verification, $weak refused without a verification run"
if [ "${MUTANTS_STRICT:-0}" = "1" ] && [ "$weak" -gt 0 ]; then exit 1; fi
exit $fail
