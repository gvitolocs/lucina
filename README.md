# LucinaOS

An operating system in Rust, built on [Asterinas](https://github.com/asterinas/asterinas). This repository holds our Rust code for it, with its proofs and models. It is a filtered copy of a private tree, updated by a script.

| Folder | What is in it |
| --- | --- |
| `kernel/` | Core kernel and boot logic: memory, interrupts, power, file systems, networking |
| `drivers/` | Device drivers: GPU, Wi-Fi, USB, storage, sound, camera, video |
| `services/` | System services: accounts, events, intents, remote access |
| `apps/` | The desktop shell and native apps |
| `boards/` | Code for other boards: the DSi, and small ESP32 boards |
| `boot/` | Boot loaders |
| `verification/` | Verus proofs and the mutants each must refuse |
| `specs/` | TLA+ specifications and Stateright models |
| `third_party/` | Vendored Rust projects, each under its own licence |
| `tools/ci/`, `.github/` | The proof launcher and CI workflows |
| `scripts/` | The entry points for the checks |

`docs/ci/PROOF-MATRIX.md` lists every proof.

Licence: MIT for our own code. Code taken from elsewhere keeps its own licence: see `LICENSE`.
