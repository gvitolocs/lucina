#!/usr/bin/env bash
# Shows that Verus can fail on drivers/lucina-q6/src/rules.rs: each line plants one bug in a copy of the shipped file, and Verus must refuse it.
# Run on buildhost from the repo root, under verify-gate.
set -uo pipefail
verus=${VERUS:-/home/user/tools/verus/verus-x86-linux/verus}
file=drivers/lucina-q6/src/rules.rs
work=$(mktemp -d "${TMPDIR:-/tmp}/juno-audio-mutants.XXXXXX")
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
    else
        echo "refused: $from -> $to"
    fi
}
try 'if phys >= DSP_MEMORY_LIMIT' 'if phys > DSP_MEMORY_LIMIT'
try 'iommu_stream % STREAMS' 'iommu_stream % 32'
try 'Some(stream * DSP_MEMORY_LIMIT + phys)' 'Some(phys)'
try 'bytes <= DSP_MEMORY_LIMIT - phys' 'bytes <= DSP_MEMORY_LIMIT'
try '(bytes + 4095) / 4096 * 4096' 'bytes'
try 'if slot + 1 >= periods' 'if slot + 1 > periods'
try $'        || slot >= periods\n' ''
try 'Step::MapMemory => Step::OpenWrite' 'Step::MapMemory => Step::AdmOpen'
try 'Step::MediaFormat => Step::Prepared' 'Step::MediaFormat => Step::Run'
try 'Step::ClockOff => Step::Close' 'Step::ClockOff => Step::Stopped'
try 'matches!(step, Step::Prepared | Step::Run | Step::Running)' 'matches!(step, Step::MediaFormat | Step::Prepared | Step::Run | Step::Running)'
try 'if !genuine || !g.stream_running {' 'if !g.stream_running {'
try 'if !genuine || !g.stream_running {' 'if !genuine {'
try $'on: false,\n        stream_running: true,' $'on: true,\n        stream_running: true,'
try $'on: false,\n        stream_running: false,' $'on: true,\n        stream_running: false,'
try 'let db = if db > CEILING_DB { CEILING_DB } else { db };' 'let db = if db > -11 { -11 } else { db };'
try 'if raw > CEILING_Q15 { CEILING_Q15 } else { raw }' 'if raw > 9000 { 9000 } else { raw }'
try $'if magnitude > TONE_PEAK {\n        TONE_PEAK' $'if magnitude > 2000 {\n        2000'
try $'if ramp > TONE_RAMP_FRAMES as u32 {\n        TONE_RAMP_FRAMES as u32' $'if ramp > 480 {\n        480'
try 'CaptureStep::BusClock => CaptureStep::Mclk' 'CaptureStep::BusClock => CaptureStep::PortConfig'
try 'CaptureStep::Mclk => CaptureStep::PortConfig' 'CaptureStep::Mclk => CaptureStep::PortStart'
try 'CaptureStep::OpenRead => CaptureStep::AdmOpen' 'CaptureStep::OpenRead => CaptureStep::Matrix'
try 'CaptureStep::EncoderConfig => CaptureStep::Prepared' 'CaptureStep::EncoderConfig => CaptureStep::Run'
try 'CaptureStep::PortStop => CaptureStep::Close' 'CaptureStep::PortStop => CaptureStep::Unmap'
try $'        CaptureStep::Prepared | CaptureStep::Run | CaptureStep::Running\n' $'        CaptureStep::EncoderConfig | CaptureStep::Prepared | CaptureStep::Run | CaptureStep::Running\n'
try 'CaptureStep::PortConfig => CodecPhase::Open' 'CaptureStep::PortStart => CodecPhase::Open'
try 'CaptureStep::MapMemory => CodecPhase::Start' 'CaptureStep::OpenRead => CodecPhase::Start'
try '(pin == 6 || pin == 7 || pin == 8 || pin == 9 || pin == 12 || pin == 13)' '(pin == 6 || pin == 7 || pin == 8 || pin == 9 || pin == 12 || pin == 14)'
try '(reg == 0 || reg == 4) &&' '(reg == 0 || reg == 8) &&'
try $'        offset <= 0x7A8 && offset % 4 == 0\n' $'        offset <= 0x7AC && offset % 4 == 0\n'
try $'        offset <= 0x7A8 && offset % 4 == 0\n' $'        offset <= 0x7A8\n'
# Windows asks for node 10 (0x30C) for a recording, not the bus clock 0x30D, and node 9 only for property 0x11.
try 'let node: u32 = if property_0x11_is_1 { 9 } else { 10 };' 'let node: u32 = if property_0x11_is_1 { 9 } else { 11 };'
try 'let node: u32 = if property_0x11_is_1 { 9 } else { 10 };' 'let node: u32 = if property_0x11_is_1 { 10 } else { 9 };'
# With mclk_only the bus clock is skipped, and to the mclk, not further.
try '            CaptureStep::BusClock => CaptureStep::Mclk,
            _ => step,' '            CaptureStep::BusClock => CaptureStep::BusClock,
            _ => step,'
try '            CaptureStep::BusClock => CaptureStep::Mclk,
            _ => step,' '            CaptureStep::BusClock => CaptureStep::PortConfig,
            _ => step,'
try '    if mclk_only {
        match step {' '    if true {
        match step {'
# The gain register: Linux's range, held, in two's complement.
try 'pub const MIC_GAIN_MAX_DB: i32 = 40;' 'pub const MIC_GAIN_MAX_DB: i32 = 127;'
try '    if held >= 0 { held as u32 } else { (held + 256) as u32 }' '    if held >= 0 { held as u32 } else { (held + 255) as u32 }'
try '    } else if db < MIC_GAIN_MIN_DB {
        MIC_GAIN_MIN_DB' '    } else if db < MIC_GAIN_MIN_DB {
        0'
[ "$fail" = 0 ] && echo "mutants: every planted bug was refused" || echo "mutants: FAILED"
exit $fail
