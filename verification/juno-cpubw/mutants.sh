#!/usr/bin/env bash
# Shows that verification/juno-cpubw/rules.rs can fail: each `try` plants one bug in a copy of the shipped file (one or more
# replacements, applied together; a pattern given twice replaces its first and then its second occurrence), and Verus must refuse
# it. The `both` ones change the code and its own spec alike, so only the lemmas of rules.rs over the fields can catch them. Run from the repo root, on buildhost2:
#   ~/.local/bin/verus-remote . bash verification/juno-cpubw/mutants.sh
set -uo pipefail
verus=${VERUS:-verus}
[ -x /home/user/tools/verus/verus-x86-linux/verus ] && verus=${VERUS:-/home/user/tools/verus/verus-x86-linux/verus}
work=$(mktemp -d "${TMPDIR:-/tmp}/juno-cpubw-mutants.XXXXXX")
trap 'rm -rf "${work:?}"' EXIT
src=kernel/lucina-cpubw/src/lib.rs
fail=0 refused=0 weak=0 total=0
try() {
    local name=$1; shift
    total=$((total + 1))
    mkdir -p "$work/kernel/lucina-cpubw/src" "$work/verification/juno-cpubw"
    cp "$src" "$work/kernel/lucina-cpubw/src/"
    cp verification/juno-cpubw/rules.rs "$work/verification/juno-cpubw/"
    local target=$work/$src text
    text=$(<"$target")
    while [ $# -ge 2 ]; do
        case "$text" in
            *"$1"*) ;;
            *) echo "NOT FOUND ($name): $1"; fail=1; return ;;
        esac
        text=${text/"$1"/"$2"}
        shift 2
    done
    printf "%s\n" "$text" > "$target"
    if cmp -s "$target" "$src"; then echo "NOT CHANGED ($name)"; fail=1; return; fi
    if "$verus" "$work/verification/juno-cpubw/rules.rs" > "$work/out" 2>&1; then
        echo "NOT REFUSED: $name"; fail=1
    else
        if grep -qE 'verification results|simplifies to|not satisfied|assertion failed|possible arithmetic|may not terminate|cannot show|recommendation not met' "$work/out" && ! grep -q 'error\[E' "$work/out"; then
            refused=$((refused + 1))
            echo "refused: $name ($(grep -c "^error" "$work/out") errors)"
        else
            echo "refused (NO VERIFICATION RUN: compile or other error): $name ($(grep -c "^error" "$work/out") errors)"
            weak=$((weak+1))
        fi
    fi
}
# The unchanged files must verify, or every "refused" below would mean nothing.
if "$verus" verification/juno-cpubw/rules.rs > "$work/baseline" 2>&1; then
    echo "baseline: verifies"
else
    cat "$work/baseline"; echo "mutants: the unchanged files do not verify"; exit 1
fi
# Code only: the `ensures` of the function itself refuses these.
try "the clamp to 14 bits removed (code)" '    if v > VOTE_MAX as u128 {
        VOTE_MAX
    } else {
        v as u32
    }' '    v as u32'
try "the keepalive applied to a non-zero AMC vote (code)" '    if keepalive && r[0].0 == 0 && r[0].1 == 0 {' '    if keepalive && (r[0].0 == 0 || r[0].1 == 0) {'
try "the keepalive forces 2 (code)" '        r[0] = (1, 1);' '        r[0] = (2, 2);'
try "the keepalive forces SLEEP too (code)" '        r[1] = (1, 1);
    }' '        r[1] = (1, 1);
        r[2] = (1, 1);
    }'
try "the keepalive leaves WAKE alone (code)" '        r[1] = (1, 1);' '        r[1] = r[1];'
try "the keepalive looks at WAKE instead of AMC (code)" '    if keepalive && r[0].0 == 0 && r[0].1 == 0 {' '    if keepalive && r[1].0 == 0 && r[1].1 == 0 {'
try "the cap before the scale removed (code)" 'let a = if agg > AGG_CAP { AGG_CAP } else { agg };' 'let a = agg;'
try "the default scale 1 instead of 1000 (code)" '    let scale: u128 = if aux.vote_scale == 0 {
        1000' '    let scale: u128 = if aux.vote_scale == 0 {
        1'
try "the peak divided by buswidth * channels (code)" \
    'div128(mul_u64_u16(n.req.peak_kbps, aux.width), n.buswidth as u128)' \
    'div128(mul_u64_u16(n.req.peak_kbps, aux.width), mul_u16_u16(n.buswidth, n.channels) as u128)'
try "the BCM takes the smallest avg term (code)" 'if avg > agg_avg {' 'if avg < agg_avg {'
try "a small vote rounds to nothing (code)" \
    '    } else if num != 0 && num < base {
        1
    } else {
        num / base
    }
}

/// `bcm_div` (bcm-voter.c)' \
    '    } else {
        num / base
    }
}

/// `bcm_div` (bcm-voter.c)'
try "the first index chosen at > instead of >= (code)" 'if lut_hz[i] >= rate_hz {' 'if lut_hz[i] > rate_hz {'
try "the last of equal entries instead of the first (code)" 'if lut_hz[i] >= rate_hz {' 'if lut_hz[i] >= rate_hz && (i + 1 >= last || lut_hz[i + 1] != lut_hz[i]) {'
try "the search runs over the last entry too (code)" 'let last = lut_hz.len() - 1;' 'let last = lut_hz.len();'
try "the L3 rate in KiB (code)" '(peak_kbps as u128) * 1000 / (buswidth as u128)' '(peak_kbps as u128) * 1024 / (buswidth as u128)'
try "the L3 rate 0 above u64 (code)" '    if rate > u64::MAX as u128 {
        u64::MAX' '    if rate > u64::MAX as u128 {
        0'
try "the peak summed instead of the max (code)" \
    '    let peak_kbps = if a.peak_kbps >= b.peak_kbps {
        a.peak_kbps
    } else {
        b.peak_kbps
    };' \
    '    let peak_kbps = if a.peak_kbps > u64::MAX - b.peak_kbps { u64::MAX } else { a.peak_kbps + b.peak_kbps };'
try "the avg wraps instead of saturating (code)" \
    '    let avg_kbps = if a.avg_kbps > u64::MAX - b.avg_kbps {
        u64::MAX
    } else {
        a.avg_kbps + b.avg_kbps
    };' \
    '    let avg_kbps = a.avg_kbps.wrapping_add(b.avg_kbps);'
try "the avg takes the max (code)" \
    '    let avg_kbps = if a.avg_kbps > u64::MAX - b.avg_kbps {
        u64::MAX
    } else {
        a.avg_kbps + b.avg_kbps
    };' \
    '    let avg_kbps = if a.avg_kbps >= b.avg_kbps { a.avg_kbps } else { b.avg_kbps };'
try "the OPP matched at or above the frequency (code)" 'if table[i].khz == khz {' 'if table[i].khz >= khz {'
try "the OPP cells swapped (code)" 'return Some((table[i].ddr_kbps, table[i].l3_kbps));' 'return Some((table[i].l3_kbps, table[i].ddr_kbps));'
try "the test pair checked on its DDR cell only (code)" \
    'if table[i].ddr_kbps == ddr_kbps && table[i].l3_kbps == l3_kbps {' 'if table[i].ddr_kbps == ddr_kbps {'
try "the test pair accepts anything (code)" '    }
    false
}' '    }
    true
}'
try "the mask vote on an idle node (code)" 'if nodes[i].req.avg_kbps != 0 || nodes[i].req.peak_kbps != 0 {' 'if nodes[i].req.avg_kbps != 0 && nodes[i].req.peak_kbps != 0 {'
try "the mask vote x set (code)" 'return (0, enable_mask);' 'return (enable_mask, enable_mask);'
try "the keepalive mask forgotten (code)" '    if keepalive {
        return (enable_mask, enable_mask);
    }' '    if false {
        return (enable_mask, enable_mask);
    }'
# The same bugs in the code and its spec alike: only the lemmas over the fields can refuse them.
try "both: the clamp removed" \
    '    if v > VOTE_MAX as u128 {
        VOTE_MAX' '    if v > 0xffff_ffff {
        0xffff_ffff' \
    'min_spec(VOTE_MAX as int, div_spec(agg * scale_spec(aux), aux.unit as int))' 'min_spec(0xffff_ffff, div_spec(agg * scale_spec(aux), aux.unit as int))'
try "both: a small vote rounds to 2, which breaks monotonicity" \
    '    } else if num != 0 && num < base {
        1
    } else {' '    } else if num != 0 && num < base {
        2
    } else {' \
    '    } else if num != 0 && num < base {
        1
    } else {' '    } else if num != 0 && num < base {
        2
    } else {'
try "both: the BCM takes the smallest peak term" \
    'if peak > agg_peak {' 'if peak < agg_peak {' \
    'max_spec(agg_peak_spec(aux, nodes, n - 1), peak_term_spec(aux, nodes[n - 1]))' 'min_spec(agg_peak_spec(aux, nodes, n - 1), peak_term_spec(aux, nodes[n - 1]))'
try "both: the first index at > instead of >=" \
    'if lut_hz[i] >= rate_hz {' 'if lut_hz[i] > rate_hz {' \
    '} else if lut[n - 1] >= rate {' '} else if lut[n - 1] > rate {'
# Formulas changed in the code and its spec alike: only the worked Linux numbers of rules.rs (the_votes_are_linuxs,
# the_l3_rates_are_linuxs) refuse these.
try "both: the default scale 1 instead of 1000" \
    '    let scale: u128 = if aux.vote_scale == 0 {
        1000' '    let scale: u128 = if aux.vote_scale == 0 {
        1' \
    'if aux.vote_scale == 0 { 1000 }' 'if aux.vote_scale == 0 { 1 }'
try "both: the peak divided by buswidth * channels" \
    'div128(mul_u64_u16(n.req.peak_kbps, aux.width), n.buswidth as u128)' \
    'div128(mul_u64_u16(n.req.peak_kbps, aux.width), mul_u16_u16(n.buswidth, n.channels) as u128)' \
    'div_spec(n.req.peak_kbps as int * aux.width as int, n.buswidth as int)' \
    'div_spec(n.req.peak_kbps as int * aux.width as int, n.buswidth as int * n.channels as int)'
try "both: the avg divided by buswidth only" \
    'mul_u16_u16(n.buswidth, n.channels) as u128,' 'n.buswidth as u128,' \
    'div_spec(n.req.avg_kbps as int * aux.width as int, n.buswidth as int * n.channels as int)' \
    'div_spec(n.req.avg_kbps as int * aux.width as int, n.buswidth as int)'
try "both: the L3 rate in KiB" \
    '(peak_kbps as u128) * 1000 / (buswidth as u128)' '(peak_kbps as u128) * 1024 / (buswidth as u128)' \
    'peak_kbps as int * 1000 / buswidth as int' 'peak_kbps as int * 1024 / buswidth as int' \
    'peak_kbps as int * 1000 / buswidth as int' 'peak_kbps as int * 1024 / buswidth as int' \
    'peak_kbps as int * 1000 / buswidth as int' 'peak_kbps as int * 1024 / buswidth as int'
# Property 6: only the_l3_row_is_monotone refuses this one (a huge peak would get row 0).
try "both: the L3 rate 0 above u64, which breaks the row's monotonicity" \
    '    if rate > u64::MAX as u128 {
        u64::MAX' '    if rate > u64::MAX as u128 {
        0' \
    '> u64::MAX as int ==> r == u64::MAX,' '> u64::MAX as int ==> r == 0,'
# Property 4: no bandwidth must give no vote; here bcm_div rounds 0 up to 1 in the code and its spec alike.
try "both: nothing asked still votes 1" \
    'num != 0 && num < base' 'num < base' \
    'num != 0 && num < base' 'num < base'
echo "$refused of $total refused"
echo "mutants: $refused refused by verification, $weak refused without a verification run"
if [ "${MUTANTS_STRICT:-0}" = "1" ] && [ "$weak" -gt 0 ]; then exit 1; fi
exit $fail
