#!/usr/bin/env bash
# One branch per proof unit, from the current commit: proof/<unit>, each running only its own proof on its own runner (docs/ci/VERUS.md).
# Forced: each snapshot is a new root commit. Pushed one at a time, because GitHub starts no workflow for many refs in one push.
# Usage: tools/ci/create-proof-branches.sh <remote> [--dry-run]
set -euo pipefail
root=$(cd "$(dirname "$0")/../.." && pwd)
cd "$root"
remote=${1:?usage: create-proof-branches.sh <remote> [--dry-run]}
sha=$(git rev-parse HEAD)
for unit in $(grep -v '^#' tools/ci/proof-inventory.tsv | cut -f1); do
    if [ "${2:-}" = --dry-run ]; then
        echo "proof/$unit"
    else
        git push -q -f "$remote" "$sha:refs/heads/proof/$unit"
        echo "pushed proof/$unit"
        sleep 2
    fi
done
