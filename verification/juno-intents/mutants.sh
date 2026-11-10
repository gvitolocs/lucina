#!/usr/bin/env bash
# Shows that Verus can fail on services/lucina-intents/src/rules.rs: each line plants one bug in a copy of the shipped file, and Verus must refuse it.
# Run on buildhost from the repo root, under verify-gate.
set -uo pipefail
verus=${VERUS:-/home/user/tools/verus/verus-x86-linux/verus}
file=services/lucina-intents/src/rules.rs
work=$(mktemp -d "${TMPDIR:-/tmp}/juno-intents-mutants.XXXXXX")
trap 'rm -rf "${work:?}"' EXIT
fail=0
try() {
    local from=$1 to=$2
    local text; text=$(<"$file")
    case "$text" in
        *"$from"*) ;;
        *) echo "NOT FOUND: $from"; fail=1; return ;;
    esac
    printf '%s\n' "${text/"$from"/"$to"}" > "$work/mutant.rs"
    local body; body=$(sed -E -e '/\/\/@omit$/d' -e 's/-> (.*[^ ]) \/\/@ret$/-> (ret: \1)/' -e 's/^( *)\/\/@ ?/\1\/*proof*\/ /' "$work/mutant.rs")
    printf 'use vstd::prelude::*;\nverus! {\n%s\n}\nfn main() {}\n' "$body" > "$work/rules.rs"
    if "$verus" "$work/rules.rs" > "$work/out" 2>&1; then
        echo "NOT REFUSED: $from -> $to"; fail=1
    elif grep -qE 'verification results:: [0-9]+ verified, [1-9][0-9]* errors' "$work/out"; then
        echo "refused: $from -> $to"
    else
        # Refused before any proof ran (it does not compile): that shows nothing about the proof.
        echo "DID NOT COMPILE: $from -> $to"; sed -n 1,6p "$work/out"; fail=1
    fi
}
try 'Effect::External => 3,' 'Effect::External => 1,'
try 'Effect::OwnerOnly => 4,' 'Effect::OwnerOnly => 2,'
try 'if effect_rank(a) >= effect_rank(b) { a } else { b }' 'if effect_rank(a) >= effect_rank(b) { b } else { a }'
try $'        Effect::Read | Effect::Write => Gate::Run,\n        Effect::Destructive | Effect::External => {' $'        Effect::Read | Effect::Write | Effect::Destructive => Gate::Run,\n        Effect::External => {'
try 'if decided { Gate::Run } else { Gate::NeedsDecision }' 'Gate::Run'
try 'Effect::OwnerOnly => Gate::Refuse,' 'Effect::OwnerOnly => Gate::NeedsDecision,'
try 'match stricter(declared, server) {' 'match declared {'
try 'if s[i] == 0 {' 'if s[i] == 0 && i > 0 {'
try $'if n < 2 || n > MAX_PATH || s[0] != b\'/\' {' $'if n < 2 || n > MAX_PATH {'
try $'let two = i + 1 < n && s[i + 1] == b\'.\' && (i + 2 == n || s[i + 2] == b\'/\');' 'let two = false;'
try $'if c == b\'/\' && (i + 1 == n || s[i + 1] == b\'/\') {' $'if c == b\'/\' && s[i + 1] == b\'/\' && i + 1 < n {'
try 'if n == 0 || n > MAX_ID {' 'if n > MAX_ID {'
try $'|| c == b\'.\' || c == b\':\';' $'|| c == b\'.\' || c == b\':\' || c == b\'/\';'
try $'        Given::Missing | Given::Null => !r.required,\n        Given::Bool => matches!(r.kind, Kind::Bool),' $'        Given::Missing | Given::Null => true,\n        Given::Bool => matches!(r.kind, Kind::Bool),'
try $'        Given::Text { len, clean, path, id } => match r.kind {\n            Kind::Text => clean && len <= r.max_len,\n            Kind::Path => path && clean && len <= r.max_len,' $'        Given::Text { len, clean, path, id } => match r.kind {\n            Kind::Text => clean && len <= r.max_len,\n            Kind::Path => clean && len <= r.max_len,'
try 'Given::Int(v) => matches!(r.kind, Kind::Int) && r.min <= v && v <= r.max,' 'Given::Int(v) => matches!(r.kind, Kind::Int) && r.min <= v,'
try 'if unknown != 0 || rules.len() != given.len() {' 'if rules.len() != given.len() {'
try 'if before.exists || !after.exists {' 'if !after.exists {'
try 'if !before.exists || !after.exists || before.stamp == after.stamp {' 'if !before.exists || !after.exists {'
try $'        UndoOp::BringBack\n        }' $'        UndoOp::TakeAway\n        }'
try 'if now.exists != e.after.exists || now.stamp != e.after.stamp {' 'if now.exists != e.after.exists {'
try 'UndoOp::PutBack | UndoOp::BringBack => Some(e.before),' 'UndoOp::PutBack | UndoOp::BringBack => Some(now),'
try $'    if r[j].seq != t || r[j].undoes != 0 || !r[j].undoable {' $'    if r[j].seq != t || !r[j].undoable {'
try $'        if r[k].undoes == t {' $'        if r[k].undoes == t && k > 0 {'
try $'    } else if r[n - 1].seq == u64::MAX || rec.seq != r[n - 1].seq + 1 {' $'    } else if r[n - 1].seq == u64::MAX || rec.seq <= r[n - 1].seq {'
try 'if t < first || rec.undoable {' 'if t < first {'
try $'        let j = j as usize;\n        if r[j].seq != t || r[j].undoes != 0 || !r[j].undoable {' $'        let j = j as usize;\n        if r[j].seq != t {'
try 'if rec.seq < 1 || rec.undoes != 0 {' 'if rec.undoes != 0 {'
[ "$fail" = 0 ] && echo "mutants: every planted bug was refused" || echo "mutants: FAILED"
exit $fail
