# C oracles for lucina-ath10k

Nothing in `tests/fixtures/` is typed by hand. Each `*_oracle.c` here is compiled with gcc against the real Linux
ath10k sources, and what it prints is the expected value for a Rust test.

| program | what it compiles from Linux | fixture | Rust test |
| --- | --- | --- | --- |
| `snoc_oracle.c` | the table block of `snoc.c`, `ce.h`, `htc.h` service ids, `qmi.h` shadow struct | `snoc_oracle.txt` | `tests/fixtures.rs` |
| (in `gen.sh`) | `sc7180.dtsi` wifi node, `snoc.c` defines | `dt_snoc.txt` | `tests/fixtures.rs` |
| `hw_oracle.c` | `hw.c` WCN3990 register blocks, the `core.c` hw_params entry | `hw_oracle.txt` | `tests/fixtures.rs` |
| `ce_oracle.c` | **the unchanged `ce.c`**, run on a fake register block | `ce_oracle.txt` | `tests/ce_trace.rs` |
| `htc_oracle.c` | `htc.h` structs, the real trailer/credit functions of `htc.c` | `htc_oracle.txt` | `tests/fixtures.rs` |
| `wmi_oracle.c` | `wmi.h`, `wmi-tlv.h` structs, the real TLV iterator, event parsers and `gen_*` encoders | `wmi_oracle.txt` | `tests/wmi_tlv.rs` |
| `htt_oracle.c` | `htt.h` structs, `ath10k_rx_desc_wcn3990_get_offsets`, message dumps filled as `htt_tx.c` does | `htt_oracle.txt` | `tests/htt.rs` |
| `regd_oracle.c` | the `regDomainPairs` table of `ath/regd_common.h` | `regd_oracle.txt` | `tests/driver.rs` |

`shim/` holds the stand-ins for kernel headers (`linux/types.h`, `hif.h`, ...). They are not Linux code; they only
define the types and macros the extracted code needs (`__le32`, `__packed`, a `struct ath10k` with the few fields
`ce.c` reads, `kzalloc_flex`, ...). `gen.sh` cuts the Linux code out of the C files with `awk`; nothing is retyped.

## Regenerating

On the machine that has the kernel tree (buildhost):

    cd drivers/lucina-ath10k/oracle
    bash gen.sh                      # all oracles
    ORACLES="ce_oracle" bash gen.sh  # one

Environment: `ATH10K_SRC` (default `/home/user/Projects/Lucina/lucinaos-kernel/drivers/net/wireless/ath/ath10k`),
`SC7180_DTSI`, `ORACLE_BUILD` (scratch directory, default `~/scratch/juno-wifi/oracle-build`), `CC`.
Needs gcc and awk; the host must be little endian. The build directory is wiped on every run. Fixtures are
plain text, one fact per line, and are committed so that `cargo test` needs no C compiler.

When the kernel tree moves to a newer ath10k, regenerate and read the diff: a changed table or encoder shows up as a
failing Rust test, which is the point.

## What the oracles cannot tell you

The driver's commands (peer assoc, install key, channel list, station parameters) and events (management rx, tx
completions, kickout, roam) run the real functions too; `SVCMAP`'s two bit tests print which services a test bitmap
turns on, and the HTT frag bank, aggregation, peer map, rx indication and rx descriptor layouts come from the real
structs. One reading differs: Linux's `ath10k_wmi_event_mgmt_tx_bundle_compl` takes each status from `desc_ids[i]`;
`parse_mgmt_tx_bundle_compl` uses the status array the same parser found.

They run the Linux code on the host, so they prove the Rust port agrees with Linux, not that the firmware agrees with
Linux. The mgmt-tx encoder and the tx completion arithmetic are the C assignments repeated in the oracle (the real
functions need mac80211 and kfifo); the HTC handshake and the HTC receive path are checked by byte equality of
messages and by hand-derived expectations, not by running the C. Three places differ on purpose, because the C trusts
firmware memory; each is covered by a test that states the difference (`htc_trailers_match_the_c_function` vector 11,
the RRI masking in `tests/fuzz.rs`, the in-order indication bound in `tests/htt.rs`).
