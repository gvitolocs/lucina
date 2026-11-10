#!/usr/bin/env bash
# Runs the model-checked protocols (specs/lucina-models, Stateright; specs in docs/spec/, plan in
# docs/design/lucinaos-correctness.md): every configuration that must hold, every witness that must fail, and the conformance
# tests against the shipped state machines. The last step of scripts/verify.sh; on its own, on buildhost:
#   verify-gate -- scripts/verify-models.sh
set -euo pipefail

root=$(cd "$(dirname "$0")/.." && pwd)
cd "$root"
cargo test --release --locked -p lucina-models -- --test-threads=2
