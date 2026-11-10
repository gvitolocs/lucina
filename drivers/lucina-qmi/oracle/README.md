# QMI oracle

Golden data for `drivers/lucina-qmi`, produced by Linux's own code.

`build.sh` compiles, with gcc on the host, three files of the Linux tree plus `harness.c`:

* `drivers/soc/qcom/qmi_encdec.c`: the encoder and decoder (`qmi_encode_message()`, `qmi_decode_message()`),
  unmodified;
* `drivers/net/wireless/ath/ath10k/qmi_wlfw_v01.c`: the `struct qmi_elem_info` tables of every WLFW message;
* `include/linux/soc/qcom/qmi.h` and `ath10k/qmi_wlfw_v01.h` are used as they are.

`shim/linux/` replaces the other kernel headers those files include (`slab.h`, `module.h`, `errno.h`, ...)
with a few lines of type definitions and no-op macros. Nothing in the shim changes how an element is encoded.

`harness.c` fills the C structs with non-trivial values, encodes them, decodes the bytes back and aborts unless
the struct comes back identical. It writes four files into `../tests/fixtures/`:

| file            | content                                                                                       |
| --------------- | --------------------------------------------------------------------------------------------- |
| `encode.txt`    | `<case> <hex>`: header and TLVs of every message                                              |
| `errors.txt`    | `<case> <rc>`: encodes the C refuses (`-22` EINVAL, `-525` ETOOSMALL)                         |
| `decode.txt`    | `<case> <hex> rc=<rc> ...`: hand-made inputs, what the C decoder returns and the fields it set |
| `constants.txt` | `<NAME> <value>`: message ids, limits and `MAX_MSG_LEN`s as `qmi_wlfw_v01.h` defines them      |

The Rust tests (`tests/wlfw_oracle.rs`) type the same messages in separately and compare byte for byte.

## Regenerate

On buildhost (any host with gcc and the Linux tree):

```sh
cd drivers/lucina-qmi/oracle
./build.sh            # LINUX=/path/to/linux WORK=/scratch/dir OUT=/where/fixtures/go to override
```

Defaults: `LINUX=/home/user/Projects/Lucina/lucinaos-kernel`, `WORK=$HOME/scratch/juno-wifi/qmi-oracle`
(binary only), `OUT=../tests/fixtures`. The output does not depend on the host; commit the changed fixtures.
The oracle prints the C decoder's complaints (`qmi-oracle: ...`) on stderr; those are expected for the refused
cases.
