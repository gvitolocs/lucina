# Proof results

From GitHub run 42868326535 on gvitolocs/lucina commit f8ec1214b (2026-11-10): 125 of 125 units verified. Runners: GitHub-hosted, MemTotal 15.6 GiB, cap 12 GiB, each unit in its own memory-capped scope. No unit came near the cap, so none needed splitting and none is a resource blocker.

| Unit | Group | Outcome | Seconds | Peak MiB |
| --- | --- | --- | ---: | ---: |
| `full/verify-sh` | full | verified | 60.3 | 317 |
| `models/stateright` | models | verified | 18.3 | 772 |
| `mutants/fortuna-library` | fortuna | verified | 14.8 | 239 |
| `mutants/fortuna-look` | fortuna | verified | 11.5 | 249 |
| `mutants/fortuna-ring` | fortuna | verified | 8.8 | 225 |
| `mutants/fortuna-secretary` | fortuna | verified | 13.8 | 230 |
| `mutants/fortuna-tls` | fortuna | verified | 5.8 | 222 |
| `mutants/fortuna-voice` | fortuna | verified | 14.3 | 246 |
| `mutants/juno-audio` | juno-audio-voice | verified | 35.5 | 253 |
| `mutants/juno-audio-ledger` | juno-audio-voice | verified | 14.5 | 288 |
| `mutants/juno-audio-wake` | juno-audio-voice | verified | 14.5 | 276 |
| `mutants/juno-ax` | juno-input-apps | verified | 36.3 | 264 |
| `mutants/juno-camera` | juno-gpu-media | verified | 36 | 251 |
| `mutants/juno-charge` | juno-power | verified | 85.1 | 310 |
| `mutants/juno-cpufreq` | juno-power | verified | 4.8 | 270 |
| `mutants/juno-disk` | boot-storage | verified | 37.5 | 294 |
| `mutants/juno-ec` | juno-power | verified | 7 | 255 |
| `mutants/juno-events` | juno-input-apps | verified | 22.5 | 257 |
| `mutants/juno-evlog` | boot-storage | verified | 5.5 | 229 |
| `mutants/juno-files-keys` | juno-input-apps | verified | 18 | 270 |
| `mutants/juno-fnkeys` | juno-input-apps | verified | 13 | 250 |
| `mutants/juno-fnrow` | juno-input-apps | verified | 7.8 | 245 |
| `mutants/juno-glass-home` | juno-gpu-media | verified | 21.3 | 249 |
| `mutants/juno-gpu-faults` | juno-gpu-media | verified | 6.5 | 229 |
| `mutants/juno-gpu-power` | juno-gpu-media | verified | 9 | 236 |
| `mutants/juno-gpu-vm` | juno-gpu-media | verified | 19.8 | 252 |
| `mutants/juno-hostname` | boot-storage | verified | 10.3 | 227 |
| `mutants/juno-idle` | juno-power | verified | 25 | 296 |
| `mutants/juno-input` | juno-input-apps | verified | 8.8 | 267 |
| `mutants/juno-intents` | juno-input-apps | verified | 31.3 | 251 |
| `mutants/juno-ir3-delay` | juno-gpu-media | verified | 8.5 | 245 |
| `mutants/juno-ir3-pack` | juno-gpu-media | verified | 7.5 | 254 |
| `mutants/juno-irqgate` | kernel-memory | verified | 9.3 | 285 |
| `mutants/juno-kexec` | boot-storage | verified | 14.3 | 269 |
| `mutants/juno-modem-witness` | juno-net-security | verified | 4.5 | 262 |
| `mutants/juno-netlink` | juno-net-security | verified | 6.3 | 228 |
| `mutants/juno-oom` | kernel-memory | verified | 9.5 | 248 |
| `mutants/juno-parse-caps` | juno-input-apps | verified | 14.3 | 271 |
| `mutants/juno-pon` | boot-storage | verified | 4.5 | 225 |
| `mutants/juno-power` | juno-power | verified | 18 | 309 |
| `mutants/juno-random` | juno-net-security | verified | 18 | 289 |
| `mutants/juno-rcu` | kernel-memory | verified | 44.3 | 287 |
| `mutants/juno-resolv` | juno-net-security | verified | 13.5 | 250 |
| `mutants/juno-ring` | kernel-memory | verified | 31.8 | 317 |
| `mutants/juno-rpmh` | juno-power | verified | 7.3 | 232 |
| `mutants/juno-sleep` | juno-power | verified | 17.3 | 264 |
| `mutants/juno-standby` | juno-power | verified | 17.8 | 268 |
| `mutants/juno-structmap` | kernel-memory | verified | 17.3 | 243 |
| `mutants/juno-taskcpu` | kernel-memory | verified | 8 | 248 |
| `mutants/juno-tcpbuf` | juno-net-security | verified | 7.8 | 290 |
| `mutants/juno-touchpad` | juno-input-apps | verified | 13.3 | 237 |
| `mutants/juno-venus` | juno-gpu-media | verified | 36 | 260 |
| `mutants/juno-vsync` | juno-gpu-media | verified | 4.5 | 244 |
| `mutants/juno-wdt` | boot-storage | verified | 7.5 | 229 |
| `mutants/juno-wifi-wake` | juno-net-security | verified | 8.8 | 276 |
| `mutants/lucinanet-mse` | juno-gpu-media | verified | 12.8 | 227 |
| `mutants/phone-glass` | juno-gpu-media | verified | 40 | 263 |
| `mutants/voice-stream` | juno-audio-voice | verified | 27.3 | 257 |
| `mutations/global` | global-mutations | verified | 29.5 | 298 |
| `shipped/fortuna-library/rules` | fortuna | verified | 1 | 237 |
| `shipped/fortuna-look/rules` | fortuna | verified | 1 | 248 |
| `shipped/fortuna-ring/rules` | fortuna | verified | 0.8 | 225 |
| `shipped/fortuna-secretary/rules` | fortuna | verified | 0.8 | 228 |
| `shipped/fortuna-tls/rules` | fortuna | verified | 0.8 | 222 |
| `shipped/fortuna-voice/rules` | fortuna | verified | 1 | 246 |
| `shipped/lucina-adreno/yuv_rules` | juno-gpu-media | verified | 0.8 | 247 |
| `shipped/lucina-app-office/member_path` | juno-input-apps | verified | 0.8 | 228 |
| `shipped/lucina-ax/rules` | juno-input-apps | verified | 1 | 263 |
| `shipped/lucina-events/rules` | juno-input-apps | verified | 1 | 246 |
| `shipped/lucina-evlog/rules` | boot-storage | verified | 1 | 228 |
| `shipped/lucina-glass/home_rules` | juno-gpu-media | verified | 1 | 249 |
| `shipped/lucina-glass/rules` | juno-gpu-media | verified | 1.8 | 262 |
| `shipped/lucina-hostname/rules` | boot-storage | verified | 1 | 226 |
| `shipped/lucina-i2c-hid/tp_rules` | juno-input-apps | verified | 1 | 235 |
| `shipped/lucina-intents/rules` | juno-input-apps | verified | 1 | 249 |
| `shipped/lucina-modem-rpmh/vote_rules` | juno-power | verified | 1 | 232 |
| `shipped/lucina-oom/rules` | kernel-memory | verified | 0.8 | 248 |
| `shipped/lucina-pon/rules` | boot-storage | verified | 1 | 225 |
| `shipped/lucina-q6/rules` | juno-audio-voice | verified | 1.3 | 252 |
| `shipped/lucina-uvc/rules` | juno-gpu-media | verified | 1 | 250 |
| `shipped/lucina-venus/rules` | juno-gpu-media | verified | 1.3 | 257 |
| `shipped/lucina-video/mse_rules` | juno-gpu-media | verified | 0.8 | 226 |
| `shipped/lucina-voice-stream/rules` | juno-audio-voice | verified | 1 | 256 |
| `shipped/lucina-wdt/rules` | boot-storage | verified | 1 | 228 |
| `tla/juno` | tla | verified | 10.3 | 152 |
| `verus/aarch64-pt/descriptor` | kernel-memory | verified | 1.3 | 272 |
| `verus/aarch64-pt/walk` | kernel-memory | verified | 2 | 267 |
| `verus/armv7-pt/mirror` | kernel-memory | verified | 0.8 | 241 |
| `verus/bootimg/align` | boot-storage | verified | 1 | 222 |
| `verus/bootimg/layout` | boot-storage | verified | 1 | 227 |
| `verus/fdt/header` | boot-storage | verified | 1 | 232 |
| `verus/juno-audio-ledger/rules` | juno-audio-voice | verified | 1 | 286 |
| `verus/juno-audio-wake/rules` | juno-audio-voice | verified | 1 | 268 |
| `verus/juno-charge/rules` | juno-power | verified | 1.3 | 299 |
| `verus/juno-cpufreq/rules` | juno-power | verified | 1.3 | 267 |
| `verus/juno-disk/rules` | boot-storage | verified | 1.3 | 289 |
| `verus/juno-ec/rules` | juno-power | verified | 1 | 251 |
| `verus/juno-files-keys/rules` | juno-input-apps | verified | 1 | 268 |
| `verus/juno-fnkeys/rules` | juno-input-apps | verified | 0.8 | 248 |
| `verus/juno-fnrow/rules` | juno-input-apps | verified | 0.8 | 244 |
| `verus/juno-gpu-faults/rules` | juno-gpu-media | verified | 0.8 | 227 |
| `verus/juno-gpu-power/rules` | juno-gpu-media | verified | 0.8 | 234 |
| `verus/juno-gpu-vm/rules` | juno-gpu-media | verified | 1 | 250 |
| `verus/juno-idle/rules` | juno-power | verified | 1.3 | 287 |
| `verus/juno-input/rules` | juno-input-apps | verified | 0.8 | 265 |
| `verus/juno-ir3-delay/rules` | juno-gpu-media | verified | 0.8 | 243 |
| `verus/juno-ir3-pack/rules` | juno-gpu-media | verified | 1 | 252 |
| `verus/juno-irqgate/rules` | kernel-memory | verified | 0.8 | 274 |
| `verus/juno-kexec/rules` | boot-storage | verified | 1.8 | 266 |
| `verus/juno-modem-witness/rules` | juno-net-security | verified | 1 | 260 |
| `verus/juno-netlink/rules` | juno-net-security | verified | 0.8 | 226 |
| `verus/juno-parse-caps/rules` | juno-input-apps | verified | 1 | 269 |
| `verus/juno-power/rules` | juno-power | verified | 1 | 307 |
| `verus/juno-random/rules` | juno-net-security | verified | 1.3 | 287 |
| `verus/juno-rcu/rules` | kernel-memory | verified | 1.3 | 283 |
| `verus/juno-resolv/rules` | juno-net-security | verified | 1.3 | 247 |
| `verus/juno-ring/rules` | kernel-memory | verified | 2.3 | 314 |
| `verus/juno-sleep/rules` | juno-power | verified | 1 | 262 |
| `verus/juno-standby/rules` | juno-power | verified | 1.3 | 264 |
| `verus/juno-structmap/rules` | kernel-memory | verified | 1 | 241 |
| `verus/juno-taskcpu/rules` | kernel-memory | verified | 1 | 244 |
| `verus/juno-tcpbuf/rules` | juno-net-security | verified | 1 | 289 |
| `verus/juno-vsync/rules` | juno-gpu-media | verified | 0.8 | 243 |
| `verus/juno-wifi-wake/rules` | juno-net-security | verified | 1 | 275 |
| `verus/vostd-port/frame_linked_list` | kernel-memory | verified | 2 | 295 |

## Deep tier

From GitHub run 43011772527 on gvitolocs/lucina (2026-11-11), one runner per unit, cap 12 GiB. Both checkers reached the pinned count, `expect-states: 53613540`.

| Unit | Checker | Seconds | Peak GiB | Distinct states |
| --- | --- | ---: | ---: | ---: |
| `deep/tla-mc/idle-wake` | tla-mc (Rust) | 200 | 2.90 | 53,613,540 |
| `deep/tlc/idle-wake` | TLC (Java, 10 GiB heap) | 612 | 5.91 | 53,613,540 |

The heaviest proof uses less than half the cap, so nothing needs splitting yet.
