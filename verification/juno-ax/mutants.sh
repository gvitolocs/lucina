#!/usr/bin/env bash
# Shows that Verus can fail on services/lucina-ax/src/rules.rs: each line plants one bug in a copy of the shipped file, and Verus must refuse it.
# Run on buildhost from the repo root, under verify-gate: verify-gate -- verification/juno-ax/mutants.sh
set -uo pipefail
verus=${VERUS:-/home/user/tools/verus/verus-x86-linux/verus}
file=services/lucina-ax/src/rules.rs
work=$(mktemp -d "${TMPDIR:-/tmp}/juno-ax-mutants.XXXXXX")
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
# Secret values: a password's text, a private area's values or drawn text, a dropped child, or a missing secret mark leak.
try 'value: !secret,' 'value: true,'
try 'value: !secret,' 'value: !scope.private,'
try 'name: !(scope.private && content),' 'name: true,'
try 'name: !(scope.private && content),' 'name: !content,'
try 'let secret = scope.private || !matches!(kind, Kind::Plain);' 'let secret = !matches!(kind, Kind::Plain);'
try 'let secret = scope.private || !matches!(kind, Kind::Plain);' 'let secret = scope.private || matches!(kind, Kind::Password);'
try 'children: !matches!(kind, Kind::Password),' 'children: true,'
try 'return Shown { node: false, name: false, value: false, children: false, actions: false, secret: true };' 'return Shown { node: true, name: false, value: false, children: false, actions: false, secret: true };'
try 'dropped: parent.dropped || matches!(parent_kind, Kind::Password),' 'dropped: matches!(parent_kind, Kind::Password),'
try 'dropped: parent.dropped || matches!(parent_kind, Kind::Password),' 'dropped: parent.dropped,'
try 'private: parent.private || matches!(parent_kind, Kind::Private),' 'private: matches!(parent_kind, Kind::Private),'
# Node ids: an occurrence that counts every sibling (an index, so ids move) or misses one.
try $'        if earlier[i] == key {\n            n += 1;\n        }' $'        n += 1;'
try $'        if earlier[i] == key {\n            n += 1;\n        }' $'        if earlier[i] != key {\n            n += 1;\n        }'
try 'while i < earlier.len()' 'while i + 1 < earlier.len()'
# Size bounds.
try 'nodes < limit && depth < MAX_DEPTH && siblings < MAX_CHILDREN' 'nodes <= limit && depth < MAX_DEPTH && siblings < MAX_CHILDREN'
try 'nodes < limit && depth < MAX_DEPTH && siblings < MAX_CHILDREN' 'nodes < limit && siblings < MAX_CHILDREN'
try 'nodes < limit && depth < MAX_DEPTH && siblings < MAX_CHILDREN' 'nodes < MAX_WEB_NODES && depth < MAX_DEPTH && siblings < MAX_CHILDREN'
try 'pub const MAX_WEB_NODES: u32 = 8192;' 'pub const MAX_WEB_NODES: u32 = 20000;'
try 'if web { MAX_WEB_NODES } else { MAX_NODES }' 'if web { MAX_WEB_NODES } else { MAX_WEB_NODES }'
try $'    if keep > room {\n        keep = room;\n    }' ''
try 'let mut keep = if chars > MAX_TEXT_CHARS { MAX_TEXT_CHARS } else { chars };' 'let mut keep = chars;'
try 'pub const NODE_BYTES: u64 = 640;' 'pub const NODE_BYTES: u64 = 2048;'
# The action mapping.
try $'    if t.owner_only {\n        return Err(Refusal::OwnerOnly);\n    }' ''
try 'if matches!(t.kind, Kind::Password) && matches!(a, Act::SetValue) {' 'if matches!(t.kind, Kind::Password) && matches!(a, Act::Click) {'
try $'    if !t.current {\n        return Err(Refusal::Stale);\n    }' ''
try $'    if t.inert {\n        return Err(Refusal::Inert);\n    }' ''
try $'    if !t.supports {\n        return Err(Refusal::Unsupported);\n    }' ''
try $'    if t.owner_only {\n        return Err(Refusal::OwnerOnly);\n    }\n    if t.inert {' $'    if t.inert {\n        return Err(Refusal::Inert);\n    }\n    if t.owner_only {'
try 'Some(p) if p.node == node && allowed => {' 'Some(p) if p.node == node => {'
try 'Some(p) if p.node == node && allowed => {' 'Some(p) if allowed => {'
try $'            *pending = None;\n            Some(p.act)' $'            Some(p.act)'
# Owner-only buttons and Lucina's injected input.
try $'    !(owner_only && injected)\n}' $'    !owner_only\n}'
try $'    !(owner_only && injected)\n}' $'    !injected\n}'
[ "$fail" = 0 ] && echo "mutants: every planted bug was refused" || echo "mutants: FAILED"
exit $fail
