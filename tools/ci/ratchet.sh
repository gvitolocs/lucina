#!/usr/bin/env bash
# fmt and clippy as a ratchet (docs/ci/VERUS.md): the files listed in tools/ci/{fmt,clippy}-baseline.txt had findings before CI existed.
# Any other file with a finding fails, and so does anything that does not compile. A file is taken off its list once it is clean.
set -uo pipefail
export LC_ALL=C
mode=${1:?usage: ratchet.sh fmt|clippy}
cd "$(dirname "$0")/../.."
now=$(mktemp)
case "$mode" in
fmt)
    out=$(cargo fmt --all -- --check -l 2>&1); rc=$?
    printf '%s\n' "$out" | grep -q '^error' && { printf '%s\n' "$out"; exit 1; }
    printf '%s\n' "$out" | sed "s#^$PWD/##" | grep '\.rs$' | sort -u > "$now"
    ;;
clippy)
    out=$(cargo clippy --workspace --all-targets --keep-going --quiet --message-format=short -- --cap-lints warn 2>&1); rc=$?
    [ "$rc" = 0 ] || { printf '%s\n' "$out" | tail -30; echo "ratchet: clippy could not finish (exit $rc)"; exit 1; }
    printf '%s\n' "$out" | grep -E '^[^ :]+\.rs:[0-9]+:[0-9]+: warning' | cut -d: -f1 | sort -u > "$now"
    ;;
*) echo "ratchet: unknown mode $mode" >&2; exit 2 ;;
esac
new=$(comm -23 "$now" <(sort -u "tools/ci/$mode-baseline.txt"))
echo "ratchet $mode: $(wc -l < "$now") files with findings, baseline $(wc -l < "tools/ci/$mode-baseline.txt")"
if [ -n "$new" ]; then echo "ratchet $mode: new files with findings:"; printf '%s\n' "$new"; exit 1; fi
