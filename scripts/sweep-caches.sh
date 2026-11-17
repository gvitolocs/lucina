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
# A SECOND, separate pass takes the cargo targets the table does not name, by IDLENESS instead of by size: the builds under
# ~/scratch, the `target/` of a worktree under ~/Projects/.worktrees, and each target folder of the two check caches under
# ~/.cache (`wt-*` in lucina-check-target, every cargo child of lucinanet-target). One goes whole when its newest file is older
# than LUCINA_SWEEP_IDLE_HOURS (24) and nothing is building into it: no running process names it or stands in it, and its own cargo
# lock is free. It reports unless --apply is given (--dry-run does not reach it), one line per candidate with the same prefix
# `idle-sweep:`, and with --apply a total of what it freed at the end. The table's own targets under ~/.cache/cargo-target are never
# candidates here: they are capped by size, and public-sync has to survive between syncs. What else stays out is said in the same
# table, by `exclude <path>` lines and one keyword that names the juno auto-install loop's build tree; see read_excludes.
#
# A THIRD pass takes what neither of those reaches: the `debug/` folder of the target of a MAIN checkout, `~/Projects/<group>/<repo>/target`
# (exactly two levels, never `.worktrees`, which the idle pass already covers). Those targets belong to no cache and to no worktree, so
# nothing capped them: one main checkout carried 93.7 GiB of debug build last written four days earlier. It removes ONLY that one
# directory named `debug`, and only when nothing under it was written in the last LUCINA_SWEEP_MAIN_DEBUG_DAYS (7) days, no running
# process names it (the idle pass' own checks), no systemd unit mentions the target and no link in a bin folder points into it. A
# `release/`, a `<triple>/` (musl, wasm32, ...) or any other profile beside it is never touched, since that is where an installed
# binary or a cross build lives and the idle pass does not reach it either. Dry unless --apply, one line per candidate prefixed
# `main-debug:`, the same total line as the idle pass. See main_debug_sweep.
#
# For a test: HOME moves the cache and the log, LUCINA_SWEEP_CONF names the folder table, LUCINA_CI_SWEEP names the binary,
# LUCINA_JUNO_AUTO_SCRIPT the juno loop script the exclude keyword reads, and LUCINA_SWEEP_LIMIT_KIB (with LUCINA_SWEEP_TO_KIB)
# caps every folder in KiB, so a few KiB of fake tree is enough to exercise it.
# The idle pass is tested the same way and asks the real /proc, so a test holds a target with a real process of its own.
# The third pass moves every root it reads: LUCINA_SWEEP_PROJECTS_ROOT the checkouts, LUCINA_SWEEP_MAIN_DEBUG_DAYS the age,
# LUCINA_SWEEP_SYSTEMD_DIRS the unit folders to grep and LUCINA_SWEEP_BIN_DIRS the folders to scan for links, so a test never
# asks the real /etc or the real ~/.config.
set -euo pipefail

here=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
sweep=${LUCINA_CI_SWEEP:-$HOME/.local/bin/lucina-ci-sweep}
cache=$HOME/.cache/cargo-target
log=${LUCINA_CACHE_SWEEP_LOG:-$HOME/.local/state/lucina-cache-sweep.log}
conf=${LUCINA_SWEEP_CONF:-$here/sweep-caches.conf}
check_target=$HOME/.cache/lucina-check-target
lucinanet_target=$HOME/.cache/lucinanet-target
auto_install=${LUCINA_JUNO_AUTO_SCRIPT:-$here/juno-auto-install.sh}
idle_hours=${LUCINA_SWEEP_IDLE_HOURS:-24}
projects_root=${LUCINA_SWEEP_PROJECTS_ROOT:-$HOME/Projects}
main_debug_days=${LUCINA_SWEEP_MAIN_DEBUG_DAYS:-7}

# unit_dirs and bin_dirs: the two lists the third pass asks besides the process table, each a whitespace-separated set of folders a
# test may replace wholesale so it never reads the real /etc or the real ~/.config. The defaults are where a user unit lives and
# where a binary gets installed by hand.
unit_dirs=("$HOME/.config/systemd/user" /etc/systemd/system /usr/lib/systemd/user)
bin_dirs=("$HOME/.local/bin" /usr/local/bin)
if [ -n "${LUCINA_SWEEP_SYSTEMD_DIRS:-}" ]; then
    read -r -a unit_dirs <<< "$LUCINA_SWEEP_SYSTEMD_DIRS"
fi
if [ -n "${LUCINA_SWEEP_BIN_DIRS:-}" ]; then
    read -r -a bin_dirs <<< "$LUCINA_SWEEP_BIN_DIRS"
fi

dry=()
apply=0
for arg in "$@"; do
    case $arg in
        --dry-run) dry=(--dry-run) ;;
        --apply) apply=1 ;;
        -h|--help) echo "usage: sweep-caches.sh [--dry-run] [--apply]"; exit 0 ;;
        *) echo "sweep-caches: unknown argument: $arg (only --dry-run and --apply)" >&2; exit 2 ;;
    esac
done

size_kib() { du -sk -- "$1" 2>/dev/null | cut -f1; }
gib() { awk -v k="${1:-0}" 'BEGIN { printf "%.1f", k / 1048576 }'; }

# log_line: one stamped line to the state log. Both passes write it here, so both trim it the same way.
log_line() { # <line>
    mkdir -p "${log%/*}"
    printf '%s\n' "$1" >> "$log"
    # One line per folder per run is about 400 a day; trim the file rather than let it grow without end.
    if [ "$(wc -l < "$log")" -gt 20000 ]; then
        tail -n 10000 "$log" > "$log.tmp" && mv "$log.tmp" "$log"
    fi
}

# real_path: the folder with every symlink followed, and nothing when it is not there. A candidate and a process path are only
# ever compared resolved, so a link that aims out of the roots below cannot move the sweep somewhere else.
real_path() { # <path>
    [ -d "$1" ] || return 1
    (cd -- "$1" 2>/dev/null && pwd -P) || return 1
}

# ---- the idle pass: a cargo target nobody has touched since the mark ----
#
# The table caps the few targets it names by SIZE; a target it does not name is never cleaned however long it sits. This pass
# takes those by IDLENESS: the builds under ~/scratch, the `target/` of a worktree under ~/Projects/.worktrees, the `wt-*` folders
# of the fast-check cache and every cargo target of the LucinaNet one. One goes WHOLE when its newest file is older than
# LUCINA_SWEEP_IDLE_HOURS (24) and no running process names it or stands in it. Cargo rebuilds what is missing, so the cost is
# time and never correctness, and as in the table what is in use is left for the next mark.
#
# Nothing else is ever a candidate: not ~/Projects itself, and not the table's own targets under ~/.cache/cargo-target, which are
# capped by size and where public-sync has to survive between syncs. Anything the owner writes as an `exclude` in the table is out
# too, and is still listed once so the run shows what it did not touch and why. In use is asked two ways: the process table (what a
# process stands in or names), and the cargo build lock of the target itself, which is what a build holds when its target directory
# came from a config file and so appears nowhere in the table. The tests run in the order of their cost: the exclude and the name
# and fingerprint check touch one folder, the process table is read once for the whole run, the walk of a target stops at the first
# young file, and a size is measured only for a target that is really going.

declare -a idle=()
declare -A idle_seen=()
declare -A cwd_resolved=()
declare -A hold_seen=()
declare -a excl_path=()
declare -a excl_where=()
declare -A excl_seen=()
proc_hold=()
proc_blind=()
proc_read=0
cache_real=$(real_path "$cache") || cache_real=""
our_uid=$(id -u)

# resolve_candidate: what one raw path has to satisfy before any pass takes it as a target: it is there, its resolved last component
# is what its source promised, its resolved path lies under the root that source may reach, and it is not one of the table's own
# targets. Echoes the resolved path, which is the ONLY form a candidate is ever used in: a link that aims out of the roots cannot
# move a sweep somewhere else, because what comes out here has every symlink followed. The two passes share it for exactly that
# reason, so neither can be steered by a folder that merely looks right where it was found.
resolve_candidate() { # <candidate> <root it must lie under> <rule>
    local raw=$1 root=$2 rule=$3 d base r
    d=$(real_path "$raw") || return 1
    base=${d##*/}
    case $rule/$base in
        target/target | wt/wt-* | any/*) ;;
        targetx/target*) ;;
        *) return 1 ;;
    esac
    r=$(real_path "$root") || return 1
    case $d in
        "$r"|"$r"/*) ;;
        *) return 1 ;;
    esac
    if [ -n "$cache_real" ]; then
        case $d in
            "$cache_real"|"$cache_real"/*) return 1 ;;
        esac
    fi
    printf '%s' "$d"
}

# add_idle: one candidate of the idle pass, judged by resolve_candidate and kept once. `rule` is `target` for a worktree's,
# `targetx` for a scratch folder named target..., `wt` for a folder of the fast-check cache named wt..., `any` for a folder of a cache
# whose name is nobody's business (the LucinaNet targets, and the fast-check cache taken as one unit).
# A folder gone by the time we look is simply not reported.
add_idle() { # <candidate> <root it must lie under> <rule>
    local d
    d=$(resolve_candidate "$1" "$2" "$3") || return 0
    if [ -n "${idle_seen[$d]:-}" ]; then
        return 0
    fi
    idle_seen[$d]=1
    idle+=("$d")
}

# expand_home: a leading `~` or `$HOME` becomes the home this run works from, which is the only two forms a path is written in
# here. Nothing is eval:ed: a path read from a file is data, and a `$( )` that wandered into one must not run as this user.
expand_home() { # <path>
    local p=$1
    case $p in
        '~') p=$HOME ;;
        \~/*) p=$HOME/${p:2} ;;
        '$HOME/'*) p=$HOME/${p#'$HOME/'} ;;
    esac
    printf '%s' "$p"
}

# add_exclude: one path the idle pass never touches, with the `<file>:<line>` that said so, so the report points at the reason.
# Resolved when it is there, so a candidate that reaches the same folder through a link is caught too; kept as written when it is
# not, which simply matches nothing yet. A path that is not absolute, or is the root itself, is refused: an exclude that mistyped
# its way into matching everything is the one mistake this list may not make.
add_exclude() { # <raw path> <where>
    local p=$1 where=$2 r
    p=${p%/}
    case $p in
        /*) ;;
        *)
            printf 'sweep-caches: %s: not an absolute path: %s\n' "$where" "$1" >&2
            return 1
            ;;
    esac
    if [ "$p" = "/" ]; then
        printf 'sweep-caches: %s: excludes the whole disk\n' "$where" >&2
        return 1
    fi
    r=$(real_path "$p") || r=$p
    if [ -z "${excl_seen[$r]:-}" ]; then
        excl_seen[$r]=1
        excl_path+=("$r")
        excl_where+=("$where")
    fi
    return 0
}

# excluded_by: the `<file>:<line>` of the exclude that covers this candidate, the path itself or anything under it, and nothing
# when no exclude does.
excluded_by() { # <resolved candidate>
    local i p
    for ((i = 0; i < ${#excl_path[@]}; i++)); do
        p=${excl_path[$i]}
        case $1 in
            "$p"|"$p"/*)
                printf '%s' "${excl_where[$i]}"
                return 0
                ;;
        esac
    done
    return 1
}

# auto_worktree_target: what `exclude-auto-install-worktree` stands for, the TARGET of the juno auto-install loop's build tree.
# Resolved when the pass runs and never copied into the table: the loop moves from one generation folder to the next, so a path
# written down here would keep the old one warm forever and let the one it builds with now go cold. First what the user unit was
# given, then the default the loop script itself falls back to.
auto_worktree_target() {
    local w="" v
    if command -v systemctl >/dev/null 2>&1; then
        v=$(systemctl --user show juno-auto-install.service -p Environment 2>/dev/null |
            awk '{ for (i = 1; i <= NF; i++)
                       if ($i ~ /^JUNO_AUTO_WORKTREE=/) { sub(/^JUNO_AUTO_WORKTREE=/, "", $i); gsub(/^"|"$/, "", $i); print $i; exit } }') || v=""
        if [ -n "$v" ]; then
            w=$(expand_home "$v")
        fi
    fi
    if [ -z "$w" ] && [ -n "${auto_install:-}" ]; then
        # `W=${JUNO_AUTO_WORKTREE:-<default>}`: the default the loop builds into when nothing set it, which is the answer here too.
        v=$(awk '/^W=\$\{JUNO_AUTO_WORKTREE:-/ {
                    s = $0
                    sub(/^W=\$\{JUNO_AUTO_WORKTREE:-/, "", s)
                    sub(/\}$/, "", s)
                    print s
                    exit
                }' "$auto_install" 2>/dev/null) || v=""
        if [ -n "$v" ]; then
            w=$(expand_home "$v")
        fi
    fi
    [ -n "$w" ] || return 1
    printf '%s/target' "$w"
}

# read_excludes: the exclude lines of the same conf, which the size pass skips over. A line is `exclude <absolute path>`, with `~`
# and `$HOME` expanded, or the one keyword `exclude-auto-install-worktree`; the reason for each is the comment line above it, in
# the table, where the owner keeps it. A line that is not one of those forms ends the run before anything is removed: an exclude
# silently dropped because of a typo is the difference between a warm build tree and a wiped one. A keyword that cannot be
# resolved is the other way round: it says so in one line and excludes nothing, since the loop need not be installed here at all.
read_excludes() {
    [ -f "$conf" ] || return 0
    local line n=0 where w
    local -a words
    while IFS= read -r line || [ -n "$line" ]; do
        n=$((n + 1))
        read -r -a words <<< "$line"
        if [ ${#words[@]} -eq 0 ] || [ "${words[0]#'#'}" != "${words[0]}" ]; then
            continue
        fi
        where="$conf:$n"
        case ${words[0]} in
            exclude)
                if [ ${#words[@]} -ne 2 ]; then
                    printf 'sweep-caches: %s: not "exclude <path>"\n' "$where" >&2
                    return 1
                fi
                add_exclude "$(expand_home "${words[1]}")" "$where" || return 1
                ;;
            exclude-auto-install-worktree)
                if [ ${#words[@]} -ne 1 ]; then
                    printf 'sweep-caches: %s: takes no argument\n' "$where" >&2
                    return 1
                fi
                if w=$(auto_worktree_target); then
                    add_exclude "$w" "$where" || return 1
                else
                    idle_line "exclude-auto-install-worktree ($where): not resolved, nothing excluded"
                fi
                ;;
        esac
    done < "$conf"
    return 0
}

# idle_candidates: fill idle with the target dirs the table does not reach, in the order they are reported.
idle_candidates() {
    local d shared=1
    # Every folder named target* under ~/scratch, to 4 levels. -prune: a target that is found is a candidate, not a folder to dig
    # a second one out of.
    while IFS= read -r d; do
        add_idle "$d" "$HOME/scratch" targetx
    done < <(find -H "$HOME/scratch" -maxdepth 4 -type d -name 'target*' -prune -print 2>/dev/null)
    # The worktrees, and nothing else of ~/Projects is a candidate HERE: the glob fixes how deep the name sits, add_idle re-checks
    # both the resolved root and the last component, so a stray folder there is not even a candidate. A main checkout reaches its
    # target by the third pass, and only for its `debug/`.
    for d in "$HOME"/Projects/.worktrees/*/*/target; do
        add_idle "$d" "$HOME/Projects/.worktrees" target
    done
    # One target folder per worktree of the fast check.
    for d in "$check_target"/wt-*; do
        add_idle "$d" "$check_target" wt
    done
    # Every cargo target of the LucinaNet check cache: a direct child folder of it, whatever the loop that made it named the
    # folder. The cargo-target test decides, as everywhere else, so a stray checkout or a loose file in there is listed and left
    # alone rather than taken. Its root is not a candidate on its own: no unit rule here, because nothing about that cache says the
    # profiles sit at its root the way the fast-check cache once did.
    for d in "$lucinanet_target"/*; do
        add_idle "$d" "$lucinanet_target" any
    done
    # The old shared layout of that cache, `debug` and `release` straight under its root: one unit, and only while no wt- folder
    # lives in the same cache, because taking the root would take those with it.
    if [ -d "$check_target/debug" ] || [ -d "$check_target/release" ]; then
        for d in "$check_target"/wt-*; do
            if [ -d "$d" ]; then
                shared=0
                break
            fi
        done
        if [ "$shared" = 1 ]; then
            add_idle "$check_target" "$check_target" any
        fi
    fi
}

# is_cargo_target: a cargo target says so itself, with CACHEDIR.TAG, or with a profile folder that holds a .fingerprint: the
# profile of a target, not a folder that only shares its name. The root and one level below (a `<triple>/`) are the only places
# either form is looked for.
is_cargo_target() { # <dir>
    local d=$1 sub
    if [ -f "$d/CACHEDIR.TAG" ]; then
        return 0
    fi
    if [ -d "$d/debug/.fingerprint" ] || [ -d "$d/release/.fingerprint" ]; then
        return 0
    fi
    for sub in "$d"/*/; do
        if [ -d "${sub}debug/.fingerprint" ] || [ -d "${sub}release/.fingerprint" ]; then
            return 0
        fi
    done
    return 1
}

# newest_side: one walk of the target, stopped at the FIRST file past the cutoff, so a busy target is never stat:ed through twice
# and an idle one is stat:ed once. Echoes `<epoch> <idle|recent|broken>`: the newest mtime of an idle target, for a busy one the
# first file that broke the rule (young enough to keep it, and the age worth printing), and broken when nothing could be read.
newest_side() { # <dir> <cutoff epoch>
    { find "$1" -printf '%T@\n' 2>/dev/null || true; } |
        awk -v cut="$2" '
            { t = int($1)
              if (t > cut) { hit = t; exit }
              if (t > max) max = t }
            END { if (hit) printf "%d recent\n", hit
                  else if (NR) printf "%d idle\n", max + 0
                  else print "0 broken" }'
}

# cwd_of: where a process stands, resolved. A box like this one has hundreds of shells in the same folder, so each distinct path
# is resolved once for the run.
cwd_of() { # <pid>
    local raw seen
    raw=$(readlink -- "/proc/$1/cwd" 2>/dev/null) || return 1
    [ -n "$raw" ] || return 1
    if [ -n "${cwd_resolved[$raw]:-}" ]; then
        printf '%s' "${cwd_resolved[$raw]}"
        return 0
    fi
    seen=$(readlink -f -- "$raw" 2>/dev/null) || seen=$raw
    cwd_resolved[$raw]=$seen
    printf '%s' "$seen"
}

# hold: remember a process string as a reason to keep, for the first pid that showed it. Two tests drop most of what the table
# would otherwise hold: the string has to contain `target`, since every candidate is named that way by the three roots and a
# string without it cannot name one, and a string already in the table adds nothing. Without this the pass would scan some tens
# of thousands of arguments for every candidate, which is minutes of bash for information we already threw away.
hold() { # <path-or-word> <pid>
    case $1 in
        *target*) ;;
        *) return 0 ;;
    esac
    if [ -z "${hold_seen[$1]:-}" ]; then
        hold_seen[$1]=$2
        proc_hold+=("$2:$1")
    fi
}

# read_proc_table: what a running process could be holding, read ONCE for the whole run: the cwd, the arguments and
# CARGO_TARGET_DIR / CARGO_BUILD_TARGET_DIR of every process of ours. The idle pass and the third pass share the answer, and
# `proc_read` says that it happened. The owner of a /proc folder is the process's own uid, so one find pass says whose these are, and
# an entry of another user is skipped: it is not ours to ask about. An entry of OURS that cannot be read at all is a reason to keep,
# because the unseen build is exactly the one a sweep would destroy; see the guard in the loop, which is what keeps the privileged
# programs of a session from freezing the whole pass.
read_proc_table() {
    local uid pid cwd a v line ok comm
    local -a w=()
    proc_hold=()
    proc_blind=()
    hold_seen=()
    while read -r uid pid; do
        [ "$uid" = "$our_uid" ] || continue
        ok=0
        cwd=$(cwd_of "$pid") || cwd=""
        if [ -n "$cwd" ]; then
            hold "$cwd" "$pid"
            ok=1
        fi
        # The arguments: one that is not absolute was read in the cwd of the process, so it is completed from there.
        if mapfile -d '' w 2>/dev/null < "/proc/$pid/cmdline"; then
            ok=1
            for a in ${w[@]+"${w[@]}"}; do
                case $a in
                    /*) ;;
                    *) a=${cwd:+$cwd/}$a ;;
                esac
                hold "$a" "$pid"
            done
        fi
        if mapfile -d '' w 2>/dev/null < "/proc/$pid/environ"; then
            ok=1
            for line in ${w[@]+"${w[@]}"}; do
                case $line in
                    CARGO_TARGET_DIR=* | CARGO_BUILD_TARGET_DIR=*)
                        v=${line#*=}
                        case $v in
                            /*) ;;
                            *) v=${cwd:+$cwd/}$v ;;
                        esac
                        hold "$v" "$pid"
                        ;;
                esac
            done
        fi
        if [ "$ok" = 0 ] && kill -0 "$pid" 2>/dev/null; then
            # Nothing of it could be read at all, which is what a program that dropped its dumpability looks like from here: the
            # price of missing a build is a wiped target under a running build, so a process we cannot see is a reason to keep,
            # but only when its own name says it builds. `comm` is the one thing such a process still tells us.
            comm=""
            read -r comm 2>/dev/null < "/proc/$pid/comm" || comm=""
            case $comm in
                cargo | rustc | rustdoc | build-script* | cargo-*) proc_blind+=("$pid") ;;
            esac
        fi
    done < <(find /proc -mindepth 1 -maxdepth 1 -type d -name '[0-9]*' -printf '%U %f\n' 2>/dev/null)
    proc_read=1
}

# names_target: does a string of the process table name this target, or a path inside it? A substring test, deliberately: an
# argument is usually a flag with the path in it (`--target-dir=...`), and the price of keeping a target by mistake is the next
# mark, against the price of removing the target a build is writing into.
names_target() { # <string> <resolved target dir>
    case $1 in
        *"$2"*) return 0 ;;
        *) return 1 ;;
    esac
}

# proc_holder: the pid of a process that uses this target, and nothing when none does.
proc_holder() { # <resolved target dir>
    local e
    for e in ${proc_hold[@]+"${proc_hold[@]}"}; do
        if names_target "${e#*:}" "$1"; then
            printf '%s' "${e%%:*}"
            return 0
        fi
    done
    return 1
}

# cargo_locks: fills `locks` with the `.cargo-lock` files of one target that are THERE, which is where cargo holds its build lock
# (the target root or a profile folder, the same two places the sweep binary looks). This never makes one: a target no build has
# ever run has none.
cargo_locks() { # <resolved target dir>
    locks=()
    local l
    for l in "$1/.cargo-lock" "$1"/*/.cargo-lock; do
        if [ -f "$l" ]; then
            locks+=("$l")
        fi
    done
    return 0
}

# lock_held: does a build hold one of these locks right now? `flock -n` on the same file the build locked: the lock dies with the
# process, so nothing stale answers here. This catches what the process table cannot, a build whose target directory came from a
# config file rather than from an argument or its cwd. A lock we cannot take for another reason keeps the target too: the price of
# that mistake is one more mark, the price of the other is a wiped target under a running build.
lock_held() { # <lock file...>
    local l
    for l in "$@"; do
        if ! flock -n "$l" -c : 2>/dev/null; then
            return 0
        fi
    done
    return 1
}

# safe_to_remove: the last guard before a removal. An empty path, the root, the home itself, or something that is not a folder is
# never removed: what reaches here came out of add_idle, which resolved it under one of the roots above.
safe_to_remove() { # <resolved dir>
    [ -n "$1" ] && [ "$1" != "/" ] && [ "$1" != "$HOME" ] && [ -d "$1" ]
}

# idle_line: one report of the idle pass, on stdout and stamped into the state log the table pass writes to.
idle_line() { # <what>
    echo "idle-sweep: $1"
    log_line "$(date -u +%Y-%m-%dT%H:%M:%SZ) idle-sweep: $1"
}

# idle_sweep: report every candidate, and remove an idle one only with --apply. Without it this is a list and nothing else; the
# final line counts what was removed, not what was found. A size or an age that was never measured is reported as `-`, which is
# why the tests run cheapest first: the excludes from the table first of all, then the name, then the process table, and only a
# target that is idle enough to go is ever walked for its size.
idle_sweep() {
    local now cut dir size age res epoch state holder verdict where freed=0 removed=0
    now=$(date +%s)
    cut=$((now - idle_hours * 3600))
    read_excludes || exit 2
    read_proc_table
    idle_candidates
    for dir in ${idle[@]+"${idle[@]}"}; do
        if where=$(excluded_by "$dir"); then
            idle_line "$dir - newest - h: kept: excluded ($where)"
            continue
        fi
        if ! is_cargo_target "$dir"; then
            idle_line "$dir - newest - h: skipped: not a cargo target"
            continue
        fi
        size=-
        age=-
        if [ ${#proc_blind[@]} -gt 0 ]; then
            verdict="kept: proc table unreadable (pid ${proc_blind[*]})"
        elif holder=$(proc_holder "$dir"); then
            verdict="kept: in use by pid $holder"
        elif cargo_locks "$dir" && [ ${#locks[@]} -gt 0 ] && lock_held "${locks[@]}"; then
            verdict="kept: cargo build lock held"
        else
            res=$(newest_side "$dir" "$cut")
            epoch=${res%% *}
            state=${res##* }
            if [ "$state" != broken ]; then
                age=$(( (now - epoch) / 3600 ))
            fi
            case $state in
                broken) verdict="kept: not walked" ;;
                recent) verdict="kept: recent" ;;
                *)
                    # The size of one that is going, in a dry run too: the list without the total it would free is the question
                    # the run is there to answer. This is the only place a `du` runs, and it is after every cheaper keep.
                    size=$(size_kib "$dir")
                    verdict="would remove"
                    if [ "$apply" = 1 ]; then
                        if safe_to_remove "$dir" && rm -rf -- "$dir"; then
                            verdict="removed"
                            freed=$((freed + ${size:-0}))
                            removed=$((removed + 1))
                        else
                            verdict="kept: removal failed"
                        fi
                    fi
                    ;;
            esac
        fi
        if [ "$size" != - ]; then
            size="$(gib "$size") GiB"
        fi
        idle_line "$dir $size newest $age h: $verdict"
    done
    if [ "$apply" = 1 ]; then
        idle_line "freed $(gib "$freed") GiB from $removed targets"
    fi
}

# ---- the third pass: the debug/ of a MAIN checkout's target ----
#
# The table caps the targets it names under ~/.cache/cargo-target by SIZE; the idle pass takes a target WHOLE by IDLENESS, but only of
# a worktree, a scratch build or one of the two check caches. The target of a main checkout, `~/Projects/<group>/<repo>/target`, is
# neither: nothing has ever cleaned it, and one carried 93.7 GiB of debug build last written four days earlier on a box whose disk has
# run short before. This pass takes exactly ONE directory of it, the `debug/` directly under the target, which is the profile a hand
# run of `cargo build` fills and nothing else. A `release/` beside it is where an installed or packaged binary came from, a `<triple>/`
# folder (musl for a static build, wasm32 for a landing) is a cross target somebody keeps on purpose, `tmp/` is other people's output:
# none of those is ever named here, whatever its age.
#
# Four things have to hold, asked cheapest first: nothing under `debug/` written in the last LUCINA_SWEEP_MAIN_DEBUG_DAYS (7) days;
# no process of the table the idle pass read naming the target, and no cargo build lock of it held; no systemd unit mentioning the
# target, in any of the three folders one can live in; no symlink in a bin folder pointing into it. The last two are asked against the
# TARGET and not against `debug/` alone, on purpose: a unit or an installed command that reaches into this checkout to RUN something
# is a reason to leave the folder to a human, where the age and the process table only ask whether someone is BUILDING here.

declare -a mdbg=()
declare -A mdbg_seen=()
declare -A mdbg_raw=()

# main_add: one checkout target, judged by the same resolve_candidate the idle pass uses (so a symlink cannot steer it), remembering
# the path as the glob found it: a unit says what whoever wrote it typed, which on a box with a linked checkout is not the resolved
# spelling.
main_add() { # <raw candidate>
    local d
    d=$(resolve_candidate "$1" "$projects_root" target) || return 0
    if [ -n "${mdbg_seen[$d]:-}" ]; then
        return 0
    fi
    mdbg_seen[$d]=1
    mdbg_raw[$d]=$1
    mdbg+=("$d")
}

# main_candidates: every checkout two levels under the projects root and nothing else. The glob fixes the depth, so a repo sitting
# directly in it, a target one folder deeper and a folder that is not named target are never looked at. `.worktrees` is said out loud
# as well: the glob already skips a dot folder, and the idle pass owns those targets anyway.
main_candidates() {
    local d r
    r=$(real_path "$projects_root") || return 0
    for d in "$r"/*/*/target; do
        case $d in
            "$r"/.worktrees/*) continue ;;
        esac
        main_add "$d"
    done
    return 0
}

# unit_holder: the first unit file that names this target, and nothing when no unit does. `grep -R` follows the symlinks those folders
# are full of, since an enabled unit is a link to a file that lives somewhere else; `-I` keeps a binary out of the answer. A unit is a
# reason to keep that no other check gives: what is removed here would be what the service points at, not a cache it rebuilds.
unit_holder() { # <resolved target> <path as found>
    local d f
    local -a pat=(-R -l -I -F -e "$1")
    if [ -n "${2:-}" ]; then
        pat+=(-e "$2")
    fi
    for d in ${unit_dirs[@]+"${unit_dirs[@]}"}; do
        [ -d "$d" ] || continue
        f=$(grep "${pat[@]}" -- "$d" 2>/dev/null | head -n 1) || f=""
        if [ -n "$f" ]; then
            printf '%s' "$f"
            return 0
        fi
    done
    return 1
}

# link_holder: a symlink of a bin folder that aims at this target or into it, and nothing when no link does. Both spellings are
# asked, what the link says and where it lands, because a link is often relative; only links are asked, a real file in a bin folder
# says nothing about where it came from.
link_holder() { # <resolved target>
    local d l raw r
    for d in ${bin_dirs[@]+"${bin_dirs[@]}"}; do
        [ -d "$d" ] || continue
        for l in "$d"/*; do
            [ -L "$l" ] || continue
            raw=$(readlink -- "$l" 2>/dev/null) || continue
            r=$(readlink -f -- "$l" 2>/dev/null) || r=$raw
            case $r in
                "$1" | "$1"/*)
                    printf '%s' "$l"
                    return 0
                    ;;
            esac
            case $raw in
                "$1" | "$1"/*)
                    printf '%s' "$l"
                    return 0
                    ;;
            esac
        done
    done
    return 1
}

# safe_to_remove_debug: the last guard before an rm, one notch stricter than the idle pass needs. What goes is the folder named debug
# directly under the resolved candidate and nothing else, so a bug upstream that hands this a target, a profile of another name or the
# home itself is refused here as well.
safe_to_remove_debug() { # <debug dir> <resolved target>
    [ -n "$1" ] && [ "$1" = "$2/debug" ] && [ "$1" != "/debug" ] && safe_to_remove "$1"
}

# main_line: one report of the third pass, on stdout and stamped into the state log like the other two.
main_line() { # <what>
    echo "main-debug: $1"
    log_line "$(date -u +%Y-%m-%dT%H:%M:%SZ) main-debug: $1"
}

# main_debug_sweep: report every checkout target, and drop its debug/ only with --apply. The size and the age of one that is kept by a
# cheaper reason are never measured, as in the idle pass: a `du` or a walk of a 90 GiB tree for a target that stays anyway is minutes
# for nothing. The process table is the one the idle pass read for the whole run; read_proc_table is asked again only should that ever
# stop running first, since an empty table would fail this pass OPEN.
main_debug_sweep() {
    local now cut dir dbg raw res epoch state age size unit link holder verdict freed=0 removed=0
    now=$(date +%s)
    cut=$((now - main_debug_days * 86400))
    main_candidates
    [ "$proc_read" = 1 ] || read_proc_table
    for dir in ${mdbg[@]+"${mdbg[@]}"}; do
        if ! is_cargo_target "$dir"; then
            continue
        fi
        dbg=$dir/debug
        size=-
        age=-
        raw=${mdbg_raw[$dir]}
        if [ ! -d "$dbg" ]; then
            verdict="kept: no debug/"
        elif [ ${#proc_blind[@]} -gt 0 ]; then
            verdict="kept: proc table unreadable (pid ${proc_blind[*]})"
        elif holder=$(proc_holder "$dir"); then
            verdict="kept: in use by pid $holder"
        elif cargo_locks "$dir" && [ ${#locks[@]} -gt 0 ] && lock_held "${locks[@]}"; then
            verdict="kept: cargo build lock held"
        elif unit=$(unit_holder "$dir" "$raw"); then
            verdict="kept: referenced by unit $unit"
        elif link=$(link_holder "$dir"); then
            verdict="kept: referenced by symlink $link"
        else
            res=$(newest_side "$dbg" "$cut")
            epoch=${res%% *}
            state=${res##* }
            if [ "$state" != broken ]; then
                age=$(( (now - epoch) / 86400 ))
            fi
            case $state in
                broken) verdict="kept: not walked" ;;
                recent) verdict="kept: written $age d ago" ;;
                *)
                    size=$(size_kib "$dbg")
                    verdict="would remove $(gib "$size") GiB"
                    if [ "$apply" = 1 ]; then
                        if safe_to_remove_debug "$dbg" "$dir" && rm -rf -- "$dbg"; then
                            verdict="removed $(gib "$size") GiB"
                            freed=$((freed + ${size:-0}))
                            removed=$((removed + 1))
                        else
                            verdict="kept: removal failed"
                        fi
                    fi
                    ;;
            esac
        fi
        if [ "$size" != - ]; then
            size="$(gib "$size") GiB"
        fi
        main_line "$dir $size newest $age d: $verdict"
    done
    if [ "$apply" = 1 ]; then
        main_line "freed $(gib "$freed") GiB from $removed debug folders"
    fi
}

idle_sweep
main_debug_sweep

if [ ! -x "$sweep" ]; then
    echo "sweep-caches: no $sweep: build tools/ci/lucina-ci and install it there, or set LUCINA_CI_SWEEP" >&2
    exit 0
fi
[ -d "$cache" ] || { echo "sweep-caches: no $cache: nothing to do"; exit 0; }

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
    log_line "$line"
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
        # The exclude lines belong to the idle pass, which reads them itself before it starts; the size pass ignores them, and a
        # path is not a `<name> <GiB>` pair, so the check below must not see one.
        case ${words[0]} in
            exclude | exclude-auto-install-worktree) continue ;;
        esac
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
