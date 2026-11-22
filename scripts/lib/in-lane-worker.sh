# shellcheck shell=sh
# scripts/lib/in-lane-worker.sh: sourced, never run. It defines one function, `in_lane_worker`, which answers whether this
# process runs inside a lane worker: it returns 0 for a worker and 1 for not a worker.
#
# It is plain POSIX shell (no `[[ ]]`, no arrays, no `local`, no `$(< file)`), so bash and `sh` (dash) give the same answer.
# A shell that cannot run it answers "worker".
#
# How it decides. The answer is "worker" when any of these holds:
#   1. `lane in-worker` says so. The lane binary is `<home>/.local/bin/lane`, where <home> is the home directory of the
#      real uid taken from the passwd database (`/usr/bin/getent passwd "$(/usr/bin/id -ru)"`, 6th field), never $HOME
#      and never PATH. If the home cannot be resolved the answer is "worker". It runs as `/usr/bin/timeout 10`, by
#      absolute path; a timeout, or a `timeout` that is missing or not executable, answers "worker". Exit 0 means worker,
#      1 means not a worker, 2 means cannot tell; only an exact 1 lets the later steps decide. If `lane in-worker --help`
#      fails but `lane in-worker` itself returns 0, 1 or 2, that result is used; any other status without a working
#      `--help` means this lane has no such subcommand and the step is skipped. A lane binary that is not there is
#      skipped too.
#   2. Walking up the parent chain from $$ to pid 1, some process has a /proc/<pid>/cgroup naming a `lane-runner-*.scope`
#      or a `lane-*-<n>.scope`. A step that cannot be read (no stat, no cgroup, a parent that is not a number, a chain that
#      does not end) answers "worker". Exception, asked before step 1: when the proc root itself is absent and
#      `/usr/bin/uname -s` is Darwin the answer is "not a worker" (macOS has no /proc and no lane worker). A proc root that
#      exists without a `self` entry is not this case. macOS has no /usr/bin/timeout, so a missing `timeout` answers
#      "worker" on any other host, while on macOS the Darwin exception answers first.
#   3. LANE_WORKER=1 or LANE_PROOFS_REFUSED is set. No variable turns any step off, and `lane` saying 1 does not skip
#      steps 2 and 3.
#
# The lane path, proc root, uname answer, timeout path and passwd lookup are arguments, for tests only:
#   in_lane_worker [lane_path [proc_root [uname_s [timeout_path [getent_path]]]]]
# They default to the values above and are never read from the environment.

if [ -z "${BASH_VERSION:-}" ] && ! (eval 'x=ab; case ${x##a} in b) exit 0 ;; esac; exit 1') 2> /dev/null; then
    in_lane_worker() { return 0; }
else
    in_lane_worker() {
        _ilw_lane=${1:-}
        _ilw_proc=${2:-/proc}
        _ilw_uname=${3:-}
        _ilw_to=${4:-/usr/bin/timeout}
        _ilw_getent=${5:-/usr/bin/getent}

        if [ "${LANE_WORKER:-}" = 1 ] || [ -n "${LANE_PROOFS_REFUSED:-}" ]; then
            return 0
        fi

        if [ ! -d "$_ilw_proc" ]; then
            [ -n "$_ilw_uname" ] || _ilw_uname=$(/usr/bin/uname -s 2> /dev/null) || _ilw_uname=
            if [ "$_ilw_uname" = Darwin ]; then
                return 1
            fi
        fi

        if [ -z "$_ilw_lane" ]; then
            _ilw_uid=$(/usr/bin/id -ru 2> /dev/null) || return 0
            [ -n "$_ilw_uid" ] || return 0
            _ilw_pw=$("$_ilw_getent" passwd "$_ilw_uid" 2> /dev/null) || return 0
            _ilw_home=${_ilw_pw#*:*:*:*:*:}
            _ilw_home=${_ilw_home%%:*}
            case $_ilw_home in /?*) ;; *) return 0 ;; esac
            _ilw_lane=$_ilw_home/.local/bin/lane
        fi

        if [ -x "$_ilw_lane" ]; then
            [ -x "$_ilw_to" ] || return 0
            _ilw_h=2
            "$_ilw_to" 10 "$_ilw_lane" in-worker --help > /dev/null 2>&1 && _ilw_h=0 || _ilw_h=$?
            [ "$_ilw_h" -ne 124 ] || return 0
            _ilw_st=2
            "$_ilw_to" 10 "$_ilw_lane" in-worker > /dev/null 2>&1 && _ilw_st=0 || _ilw_st=$?
            case $_ilw_st in
                1) ;;
                0 | 2 | 124) return 0 ;;
                *) [ "$_ilw_h" -ne 0 ] || return 0 ;;
            esac
        fi

        _ilw_p=$$
        _ilw_n=0
        while :; do
            _ilw_n=$((_ilw_n + 1))
            [ "$_ilw_n" -le 128 ] || return 0
            [ -r "$_ilw_proc/$_ilw_p/cgroup" ] || return 0
            while IFS= read -r _ilw_line || [ -n "$_ilw_line" ]; do
                _ilw_b=${_ilw_line##*/}
                case $_ilw_b in
                    lane-runner-*.scope) return 0 ;;
                    lane-*-*.scope)
                        _ilw_t=${_ilw_b%.scope}
                        _ilw_t=${_ilw_t##*-}
                        case $_ilw_t in '' | *[!0-9]*) ;; *) return 0 ;; esac
                        ;;
                esac
            done < "$_ilw_proc/$_ilw_p/cgroup" || return 0
            [ "$_ilw_p" = 1 ] && return 1
            [ -r "$_ilw_proc/$_ilw_p/stat" ] || return 0
            _ilw_stat=
            IFS= read -r _ilw_stat < "$_ilw_proc/$_ilw_p/stat" || [ -n "$_ilw_stat" ] || return 0
            case $_ilw_stat in *') '*) ;; *) return 0 ;; esac
            _ilw_rest=${_ilw_stat##*) }
            _ilw_rest=${_ilw_rest#* }
            _ilw_ppid=${_ilw_rest%% *}
            case $_ilw_ppid in '' | *[!0-9]*) return 0 ;; esac
            [ "$_ilw_ppid" -ge 1 ] || return 0
            _ilw_p=$_ilw_ppid
        done
    }
fi
