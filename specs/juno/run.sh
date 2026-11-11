#!/usr/bin/env bash
# Juno's concurrency models (TLA+). docs/research/juno-concurrency.md says what each one models and proves;
# docs/design/lucinaos-correctness.md places them in the plan; docs/research/tla-checkers.md compares the checkers.
#
# Checkers, as userspace/formal/run-lucina-checks: tla-mc first (Rust, crate tla-mc 0.3.0: TLC's exact state counts, far
# faster). Where tla-mc refuses a formula as unsupported, tla-checker (Rust, crate tla-checker 0.24.11, binary `tla`)
# checks that configuration, and the line says so. CHECKER=tla-mc|tla|tlc forces one checker. TLC is the parity
# cross-check (TLC_CROSSCHECK=1: both verdicts must agree), not the gate.
#
# Each model directory holds one module and its configurations, named by what the checker must find:
#   <Module>.cfg, <Module>Fixed*.cfg, <Module>Benign_*.cfg   must pass, every state explored
#   <Module>Witness_*.cfg                                    must fail with a property violated (a parse, config or
#                                                            limit error does not count): the model can see that bug
#   *.heavy.cfg                                              only with VERIFY_HEAVY=1; run them on the Mac
#                                                            (scripts/mac-model-check.sh)
#
# Run by scripts/verify.sh with VERIFY_TLA=1; alone on buildhost:  verify-gate -- specs/juno/run.sh
# Env: ONLY (one model directory), CHECKER (auto), TLAMC, TLA, TLA_MAX_STATES (50000000), WORKERS (4); for TLC: JAVA,
# TLA_JAR, TLC_HEAP (4g).
#
# tla-checker 0.24.11 note: an IF ... THEN ... ELSE used as a value in a /\ list nested in an outer IF's branch takes
# the next aligned conjunct into its ELSE (TLC and tla-mc do not). These specs parenthesize such IFs; keep it so.
set -uo pipefail

here=$(cd "$(dirname "$0")" && pwd)
checker=${CHECKER:-auto}
tlamc=${TLAMC:-$HOME/.cargo/bin/tla-mc}
tla=${TLA:-$HOME/.cargo/bin/tla}
max_states=${TLA_MAX_STATES:-50000000}
workers=${WORKERS:-4}
java=${JAVA:-$HOME/tools/jdk-21.0.12.1+1/bin/java}
# A bare name (JAVA=java on a CI runner) is looked up on PATH; [ -x ] below needs a path.
case "$java" in */*) ;; *) java=$(command -v "$java" || echo "$java") ;; esac
jar=${TLA_JAR:-$HOME/tools/tla/tla2tools.jar}
case "$checker" in
    auto | tla-mc) [ -x "$tlamc" ] || { echo "juno-tla: no tla-mc at $tlamc (cargo install tla-mc --version 0.3.0 --locked)" >&2; exit 2; } ;;
esac
case "$checker" in
    auto | tla) [ -x "$tla" ] || { echo "juno-tla: no tla-checker at $tla (cargo install tla-checker --version 0.24.11 --locked)" >&2; exit 2; } ;;
esac

# Each prints pass | fail | unsupported | error; the log is $log.
run_tlamc() {
    (cd "$1" && "$tlamc" -workers "$workers" -config "$3" "$2.tla") > "$log" 2>&1
    local rc=$?
    if [ $rc -eq 0 ] && grep -q 'No error has been found' "$log"; then echo pass
    elif [ $rc -ne 0 ] && grep -qE '(is|was|were) violated|Deadlock reached' "$log"; then echo fail
    elif [ $rc -eq 2 ] && grep -qi 'unsupported' "$log"; then echo unsupported
    else echo error; fi
}
run_tla() {
    local opts=()
    grep -q 'CHECK_DEADLOCK FALSE' "$1/$3" && opts+=(--allow-deadlock)
    grep -q '^PROPERTIES\|^PROPERTY' "$1/$3" && opts+=(--check-liveness)
    (cd "$1" && "$tla" "$2.tla" --config "$3" "${opts[@]}" --max-states "$max_states" --max-depth 1000) > "$log" 2>&1
    local rc=$?
    if [ $rc -eq 0 ] && grep -q 'No errors found' "$log"; then echo pass
    elif [ $rc -ne 0 ] && grep -qE 'Invariant [0-9]+ \([A-Za-z0-9_]+\) violated|Liveness property violated|Deadlock' "$log"; then echo fail
    else echo error; fi
}
run_tlc() {
    [ -x "$java" ] && [ -f "$jar" ] || { echo error; return; }
    (cd "$1" && "$java" -XX:+UseParallelGC -Xmx${TLC_HEAP:-4g} -cp "$jar" tlc2.TLC -noGenerateSpecTE \
        -workers "$workers" -metadir "$meta/states" -config "$3" "$2.tla") > "$log" 2>&1
    local rc=$?
    if [ $rc -eq 0 ] && grep -q 'No error has been found' "$log"; then echo pass
    elif [ $rc -ne 0 ] && grep -qE '(is|was|were) violated|Deadlock reached' "$log"; then echo fail
    else echo error; fi
}

status=0
matched=0
checked=0
for dir in "$here"/*/; do
    [ -z "${ONLY:-}" ] || [ "$(basename "$dir")" = "$ONLY" ] || continue
    matched=1
    for tla_file in "$dir"*.tla; do
        [ -e "$tla_file" ] || continue
        module=$(basename "$tla_file" .tla)
        case "$module" in *_TTrace_*) continue ;; esac
        for cfg in "$dir$module"*.cfg; do
            [ -e "$cfg" ] || continue
            name=$(basename "$cfg" .cfg)
            case "$name" in *.heavy) [ "${VERIFY_HEAVY:-0}" = 1 ] || { echo "skip  $name (heavy: the Mac)"; continue; } ;; esac
            expect=pass
            case "$name" in *Witness_*) expect=fail ;; esac
            meta=$(mktemp -d "${TMPDIR:-/tmp}/juno-tla-$name.XXXXXX")
            log="$meta/check.log"
            c=$(basename "$cfg")
            case "$checker" in
                tla-mc) used=tla-mc; got=$(run_tlamc "$dir" "$module" "$c") ;;
                tla) used=tla-checker; got=$(run_tla "$dir" "$module" "$c") ;;
                tlc) used=tlc; got=$(run_tlc "$dir" "$module" "$c") ;;
                *)
                    used=tla-mc; got=$(run_tlamc "$dir" "$module" "$c")
                    if [ "$got" = unsupported ]; then
                        used="tla-checker (tla-mc: unsupported formula)"; got=$(run_tla "$dir" "$module" "$c")
                    fi ;;
            esac
            what=$(grep -m1 -oE '[0-9,]+ distinct states found|Reachable states: [0-9]+|(Invariant|Temporal propert(y|ies)) [A-Za-z0-9_ ]+ (is|was|were) violated|Invariant [0-9]+ \([A-Za-z0-9_]+\) violated|Liveness property violated: [A-Za-z0-9_]+' "$log" || true)
            if [ "$got" != "$expect" ]; then
                echo "FAIL  $name ($used: expected $expect, got $got); log $log"
                tail -n 12 "$log" | sed 's/^/      /'
                status=1
                continue
            fi
            want=$(sed -n 's/.*expect-states: *\([0-9][0-9]*\).*/\1/p' "$cfg" | head -n 1)
            if [ -n "$want" ] && [ "$expect" = pass ]; then
                # The last count in the log: TLC prints running totals in its progress lines, its final total last.
                found=$(grep -oE '[0-9,]+ distinct states found|Reachable states: [0-9]+' "$log" | tail -n 1 | tr -d , | grep -oE '[0-9]+' | head -n 1)
                if [ "$found" != "$want" ]; then
                    echo "FAIL  $name ($used: expected $want distinct states, found ${found:-none}); log $log"
                    status=1
                    continue
                fi
            fi
            line="ok    $name ($used: $got${what:+, $what})"
            if [ "${TLC_CROSSCHECK:-0}" = 1 ] && [ "$checker" != tlc ]; then
                log="$meta/tlc.log"
                cross=$(run_tlc "$dir" "$module" "$c")
                if [ "$cross" != "$expect" ]; then
                    echo "FAIL  $name (TLC cross-check: expected $expect, got $cross); log $log"
                    status=1
                    continue
                fi
                line="$line; TLC agrees"
            fi
            echo "$line"
            checked=$((checked + 1))
            rm -rf "$meta"
        done
    done
done
if [ -n "${ONLY:-}" ] && [ "$matched" = 0 ]; then echo "FAIL  ONLY=$ONLY names no model directory"; status=1; fi
if [ "$checked" = 0 ]; then echo "FAIL  no configuration was checked"; status=1; fi
exit $status
