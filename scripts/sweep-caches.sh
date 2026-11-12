#!/usr/bin/env bash
# Caps the single Cargo target caches under ~/.cache/cargo-target (docs/ci/SHIFT-LEFT.md, "Other cargo targets"). These are the app
# and landing targets, one per checkout, which grew past the disk: a single app target reached 112 GiB in nine hours on 2026-11-12
# while nothing capped it. The fast-check cache has its own cap, run by scripts/check-fast.
#
# Each sweep drops the least recently used build artifacts of that one target over the cap (never its binaries). A target a build
# is using is left whole and logged as in use: not swept; the timer calls this every 15 minutes, so it is tried again at the next
# mark. Cost: a rebuild, never correctness.
#
# Which folders, and each one's cap, are not here but in a data file: LUCINA_SWEEP_CONF, else sweep-caches.conf next to this
# script. One entry per line, `<name-or-glob> <cap in GiB>`, names relative to the cache dir, `#` comments and blank lines
# ignored; a folder is swept once, under the cap of the last line that matches it. The table names the checkouts of one machine,
# so it is private and stays out of the public copy; the script is not.
#
# One line per folder on stdout and in ~/.local/state/lucina-cache-sweep.log: the dir, its size before, what happened, its size
# after. Run by the lucina-cache-sweep user timer; safe to run by hand, and --dry-run shows what a sweep would do.
#
# For a test: HOME moves the cache and the log, LUCINA_SWEEP_CONF names the folder table, LUCINA_CI_SWEEP names the binary, and
# LUCINA_SWEEP_LIMIT_KIB (with LUCINA_SWEEP_TO_KIB) caps every folder in KiB, so a few KiB of fake tree is enough to exercise it.
set -euo pipefail

here=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
sweep=${LUCINA_CI_SWEEP:-$HOME/.local/bin/lucina-ci-sweep}
cache=$HOME/.cache/cargo-target
log=${LUCINA_CACHE_SWEEP_LOG:-$HOME/.local/state/lucina-cache-sweep.log}
conf=${LUCINA_SWEEP_CONF:-$here/sweep-caches.conf}

dry=()
if [ $# -gt 0 ]; then
    case $1 in
        --dry-run) dry=(--dry-run) ;;
        -h|--help) echo "usage: sweep-caches.sh [--dry-run]"; exit 0 ;;
        *) echo "sweep-caches: unknown argument: $1 (only --dry-run)" >&2; exit 2 ;;
    esac
fi

if [ ! -x "$sweep" ]; then
    echo "sweep-caches: no $sweep: build tools/ci/lucina-ci and install it there, or set LUCINA_CI_SWEEP" >&2
    exit 0
fi
[ -d "$cache" ] || { echo "sweep-caches: no $cache: nothing to do"; exit 0; }

size_kib() { du -sk -- "$1" 2>/dev/null | cut -f1; }
gib() { awk -v k="${1:-0}" 'BEGIN { printf "%.1f", k / 1048576 }'; }

sweep_one() { # <folder> <cap in GiB>: a folder that is not there is skipped, the rest gets its line
    local dir=$1 gib=$2 before after rc action line
    [ -d "$dir" ] || return 0
    local limits=(--limit-gib "$gib")
    if [ -n "${LUCINA_SWEEP_LIMIT_KIB:-}" ]; then
        limits=(--limit-kib "$LUCINA_SWEEP_LIMIT_KIB")
        if [ -n "${LUCINA_SWEEP_TO_KIB:-}" ]; then limits+=(--to-kib "$LUCINA_SWEEP_TO_KIB"); fi
    fi
    before=$(size_kib "$dir")
    # The sweep's own verdict, not a second measure: du counts blocks and the sweep counts the bytes it removes, so the sizes
    # alone cannot tell a cap that was already met from a folder it chose to leave. 3 means a build is using it.
    rc=0
    "$sweep" sweep-target --dir "$dir" --single "${limits[@]}" ${dry[@]+"${dry[@]}"} || rc=$?
    after=$(size_kib "$dir")
    case $rc in
        0)
            if [ ${#dry[@]} -gt 0 ]; then
                action="dry run"
            elif [ "$after" -lt "$before" ]; then
                action="swept"
            else
                action="nothing removed"
            fi
            ;;
        3) action="in use: not swept" ;;
        *) action="FAILED (exit $rc), nothing removed" ;;
    esac
    line=$(printf '%s %s %s GiB -> %s GiB: %s (cap %s)' \
        "$(date -u +%Y-%m-%dT%H:%M:%SZ)" "$dir" "$(gib "$before")" "$(gib "$after")" "$action" "${limits[*]}")
    mkdir -p "${log%/*}"
    printf '%s\n' "$line" >> "$log"
    # One line per folder per run is about 400 a day; trim the file rather than let it grow without end.
    [ "$(wc -l < "$log")" -gt 20000 ] && tail -n 10000 "$log" > "$log.tmp" && mv "$log.tmp" "$log"
    echo "sweep-caches: $line"
}

# read_table: fills dirs with the cache folders the table names, in the order the table first names each, and cap with the
# GiB mark of the last line that matches it. A line that is not `<name-or-glob> <GiB>` is refused before anything is swept: a
# wrong cap here is a folder swept on the wrong number, and the timer would repeat the mistake every 15 minutes.
read_table() {
    local line n=0 d
    local -a words
    while IFS= read -r line || [ -n "$line" ]; do
        n=$((n + 1))
        read -r -a words <<< "$line"
        if [ ${#words[@]} -eq 0 ]; then
            continue
        fi
        if [ "${words[0]#'#'}" != "${words[0]}" ]; then
            continue
        fi
        if [ ${#words[@]} -ne 2 ] || ! [[ ${words[1]} =~ ^[0-9]+(\.[0-9]+)?$ ]]; then
            printf 'sweep-caches: %s:%s: not "<name> <GiB>"\n' "$conf" "$n" >&2
            return 1
        fi
        # The entry is a pattern, so it is deliberately unquoted here: the shell matches it against the folders that are there,
        # in order. Only the pattern half is left bare; $cache stays quoted and a name cannot hold a blank (the line has two words).
        for d in "$cache"/${words[0]}; do
            if [ -d "$d" ]; then
                if [ -z "${cap[$d]:-}" ]; then
                    dirs+=("$d")
                fi
                cap[$d]=${words[1]}
            fi
        done
    done < "$conf"
}

if [ ! -f "$conf" ]; then
    echo "sweep-caches: no folder table ($conf): nothing to sweep"
    exit 0
fi
dirs=()
declare -A cap=()
read_table || exit 2

for dir in ${dirs[@]+"${dirs[@]}"}; do
    sweep_one "$dir" "${cap[$dir]}"
done
