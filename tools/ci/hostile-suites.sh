#!/usr/bin/env bash
# The tests that feed malformed input from outside (firmware, network, files) to the parsers. A crate with no such test any more
# is a failure, not a pass: a suite cannot disappear by being renamed. Run on a checkout whose workspace can build here.
set -euo pipefail
crates=(lucina-ath10k lucina-gpu3d lucina-modem-rpmh lucina-parse-caps lucina-qcom-services lucina-qcom-soc lucina-qmi lucina-qrtr lucina-venus lucina-video lucina-wifi-mac)
for c in "${crates[@]}"; do
    n=$(cargo test --quiet --locked -p "$c" hostile -- --list 2>/dev/null | grep -c ': test$' || true)
    [ "$n" -gt 0 ] || { echo "hostile-suites: $c has no test named hostile" >&2; exit 1; }
    cargo test --quiet --locked -p "$c" hostile
    echo "ok $c ($n)"
done
