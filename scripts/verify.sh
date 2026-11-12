#!/usr/bin/env bash
# Runs every LucinaOS Verus proof (every verification/**/*.rs file) with the pinned Verus, then the model-checked protocols
# (scripts/verify-models.sh). The single entry point of docs/design/lucinaos-correctness.md.
# On buildhost, run it through the gate:  verify-gate -- scripts/verify.sh
# Env: VERUS overrides the binary; it must still match the version and sha256 pinned in references/upstream.lock.
set -euo pipefail

root=$(cd "$(dirname "$0")/.." && pwd)
lock="$root/references/upstream.lock"
verus=${VERUS:-/home/user/tools/verus/verus-x86-linux/verus}
pinned=$(sed -n '/^\[verus\]/,/^\[/s/^release = "\([^"]*\)".*/\1/p' "$lock")
pinned_sha=$(sed -n '/^\[verus\]/,/^\[/s/^binary_sha256 = "\([0-9a-f]*\)".*/\1/p' "$lock")
# The launcher is the same file across releases, so the verifier beside it (rust_verify) is pinned too (2026-11-08: a
# release installed on 11-04 passed the launcher's sum, and only the version string told it apart).
pinned_verifier=$(sed -n '/^\[verus\]/,/^\[/s/^verifier_sha256 = "\([0-9a-f]*\)".*/\1/p' "$lock")
actual=$("$verus" --version 2>&1 | sed -n 's/^ *Version: //p')
actual_sha=$(sha256sum "$verus" | cut -d' ' -f1)
actual_verifier=$(sha256sum "$(dirname "$verus")/rust_verify" 2>/dev/null | cut -d' ' -f1)
if [ "$actual" != "$pinned" ] || [ "$actual_sha" != "$pinned_sha" ] || [ "$actual_verifier" != "$pinned_verifier" ]; then
    echo "verify: $verus is $actual ($actual_sha, rust_verify $actual_verifier)," \
        "upstream.lock pins $pinned ($pinned_sha, rust_verify $pinned_verifier)" >&2
    exit 2
fi

# Every proof file is checked; a new file cannot be forgotten.
mapfile -t proofs < <(cd "$root" && find verification -name '*.rs' | sort)
[ ${#proofs[@]} -gt 0 ] || { echo "verify: no proofs found" >&2; exit 2; }

status=0
for p in "${proofs[@]}"; do
    echo "== $p"
    # One proof needs more solver resources than the Verus default, by file name only; the reason and the ceiling
    # are in tools/ci/trust-allow.tsv.
    case "$p" in
        verification/juno-surface/rules.rs)
            if "$verus" --rlimit 20 "$root/$p"; then echo "ok $p"; else echo "FAIL $p"; status=1; fi
            ;;
        *)
            if "$verus" "$root/$p"; then echo "ok $p"; else echo "FAIL $p"; status=1; fi
            ;;
    esac
done

# The proofs on shipped files with `//@` lines (D-011), unless VERIFY_SHIPPED=0.
if [ "${VERIFY_SHIPPED:-1}" != 0 ]; then
    echo "== shipped (scripts/verify-shipped.sh)"
    if VERUS="$verus" bash "$root/scripts/verify-shipped.sh"; then echo "ok shipped"; else echo "FAIL shipped"; status=1; fi
fi

# Every planted bug must be refused: the global list and each proof's own mutants.sh. Long (one Verus run per mutant), so
# only with VERIFY_MUTANTS=1: the full gate before a release, and after a change to a proof's spec.
if [ "${VERIFY_MUTANTS:-0}" = 1 ]; then
    echo "== mutants (scripts/verify-mutations.sh)"
    if VERUS="$verus" bash "$root/scripts/verify-mutations.sh"; then echo "ok mutants"; else echo "FAIL mutants"; status=1; fi
    for m in $(cd "$root" && find verification -name mutants.sh | sort); do
        echo "== mutants ($m)"
        if (cd "$root" && VERUS="$verus" bash "$m"); then echo "ok $m"; else echo "FAIL $m"; status=1; fi
    done
fi

# The TLA+ specs and their witnesses (TLC), only with VERIFY_TLA=1: Minerva's, then Juno's concurrency models.
if [ "${VERIFY_TLA:-0}" = 1 ]; then
    echo "== tla (userspace/formal/run-lucina-checks)"
    if VERUS="$verus" "$root/userspace/formal/run-lucina-checks"; then echo "ok tla"; else echo "FAIL tla"; status=1; fi
    echo "== tla juno (specs/juno/run.sh)"
    if "$root/specs/juno/run.sh"; then echo "ok tla juno"; else echo "FAIL tla juno"; status=1; fi
fi

# The model-checked protocols (layers c and d of docs/spec/), unless VERIFY_MODELS=0.
if [ "${VERIFY_MODELS:-1}" != 0 ]; then
    echo "== models (scripts/verify-models.sh)"
    if bash "$root/scripts/verify-models.sh"; then echo "ok models"; else echo "FAIL models"; status=1; fi
fi
exit $status
