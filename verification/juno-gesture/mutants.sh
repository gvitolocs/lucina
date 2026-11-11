#!/usr/bin/env bash
# Shows that Verus can fail on drivers/lucina-gesture/src/rules.rs: each line plants one bug in a copy of the shipped file, and Verus must refuse it.
# Run on buildhost from the repo root, under verify-gate.
set -uo pipefail
verus=${VERUS:-/home/user/tools/verus/verus-x86-linux/verus}
file=drivers/lucina-gesture/src/rules.rs
work=$(mktemp -d "${TMPDIR:-/tmp}/juno-gesture-mutants.XXXXXX")
trap 'rm -rf "${work:?}"' EXIT
fail=0 refused=0 weak=0
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
    else
        if grep -qE 'verification results|simplifies to|not satisfied|assertion failed|possible arithmetic|may not terminate|cannot show|recommendation not met' "$work/out" && ! grep -q 'error\[E' "$work/out"; then
            refused=$((refused + 1))
            echo "refused: $from -> $to"
        else
            echo "refused (NO VERIFICATION RUN: compile or other error): $from -> $to"
            weak=$((weak+1))
        fi
    fi
}
# The unchanged files must verify, or every "refused" below would mean nothing.
base_body=$(sed -E -e '/\/\/@omit$/d' -e 's/-> (.*[^ ]) \/\/@ret$/-> (ret: \1)/' -e 's/^( *)\/\/@ ?/\1\/*proof*\/ /' "$file")
printf 'use vstd::prelude::*;\nverus! {\n%s\n}\nfn main() {}\n' "$base_body" > "$work/base.rs"
if "$verus" "$work/base.rs" > "$work/baseline" 2>&1; then
    echo "baseline: verifies"
else
    cat "$work/baseline"; echo "mutants: the unchanged files do not verify"; exit 1
fi
# Sequence numbers: a begin that does not count, a wrap that lands on 1, a gesture opened under another number.
try "r.next_seq = if next_seq == 4294967295 {
                    0
                } else {
                    next_seq + 1
                };" "r.next_seq = next_seq;"
try "r.next_seq = if next_seq == 4294967295 {
                    0
                } else {" "r.next_seq = if next_seq == 4294967295 {
                    1
                } else {"
try "r.open = Some(Open {
                    kind: ev.kind,
                    fingers: ev.fingers,
                    seq: next_seq," "r.open = Some(Open {
                    kind: ev.kind,
                    fingers: ev.fingers,
                    seq: 0,"
try "seq: next_seq,
                    cancelled: false," "seq: next_seq + 1,
                    cancelled: false,"
# Updates: one let through with nothing open, one of another kind, one with other fingers, one that closes the gesture.
try "                        cancelled: false,
                    });
                }
            }
        }
        Op::End => {" "                        cancelled: false,
                    });
                }
            } else {
                r.first = Some(Emit {
                    phase: RulePhase::Update,
                    kind: ev.kind,
                    fingers: 1,
                    seq: 0,
                    cancelled: false,
                });
            }
        }
        Op::End => {"
try "if same_kind(o.kind, ev.kind) {" "if true {"
try "phase: RulePhase::Update,
                        kind: o.kind,
                        fingers: o.fingers," "phase: RulePhase::Update,
                        kind: o.kind,
                        fingers: 1,"
try "seq: o.seq,
                        cancelled: false,
                    });" "seq: o.seq,
                        cancelled: false,
                    });
                    r.open = None;"
# Ends: one with nothing open, the open gesture left open, the cancelled flag lost, another gesture's number.
try "                        cancelled: ev.cancelled,
                    });
                    r.open = None;
                }
            }" "                        cancelled: ev.cancelled,
                    });
                    r.open = None;
                }
            } else {
                r.first = Some(Emit {
                    phase: RulePhase::End,
                    kind: ev.kind,
                    fingers: 1,
                    seq: 0,
                    cancelled: ev.cancelled,
                });
            }"
try "cancelled: ev.cancelled,
                    });
                    r.open = None;" "cancelled: ev.cancelled,
                    });"
try "seq: o.seq,
                        cancelled: ev.cancelled," "seq: o.seq,
                        cancelled: false,"
try "seq: o.seq,
                        cancelled: ev.cancelled," "seq: o.seq + 1,
                        cancelled: ev.cancelled,"
# Resets: an open gesture left open, one ended without the cancelled flag, a reset that writes an end with nothing open.
try "            }
            r.open = None;
        }
    }
    r" "            }
        }
    }
    r"
try "                    cancelled: true,
                });
            }
            r.open = None;" "                    cancelled: false,
                });
            }
            r.open = None;"
try "                    cancelled: true,
                });
            }
            r.open = None;" "                    cancelled: true,
                });
            } else {
                r.first = Some(Emit {
                    phase: RulePhase::End,
                    kind: RuleKind::Swipe,
                    fingers: 1,
                    seq: 0,
                    cancelled: true,
                });
            }
            r.open = None;"
# Begins: two in a row without the end between them, the displacing end not cancelled, a finger count of 5 or of 0.
try "                    r.first = Some(Emit {
                        phase: RulePhase::End,
                        kind: o.kind,
                        fingers: o.fingers,
                        seq: o.seq,
                        cancelled: true,
                    });
                }
                r.second" "                    r.first = None;
                }
                r.second"
try "                        seq: o.seq,
                        cancelled: true,
                    });
                }
                r.second" "                        seq: o.seq,
                        cancelled: false,
                    });
                }
                r.second"
try "if ev.fingers >= 1 && ev.fingers <= 4 {" "if ev.fingers >= 1 && ev.fingers <= 5 {"
try "if ev.fingers >= 1 && ev.fingers <= 4 {" "if ev.fingers <= 4 {"
echo "mutants: $refused refused by verification, $weak refused without a verification run"
if [ "${MUTANTS_STRICT:-0}" = "1" ] && [ "$weak" -gt 0 ]; then exit 1; fi
exit "$fail"
