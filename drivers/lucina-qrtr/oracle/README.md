# QRTR oracle

Golden data for `drivers/lucina-qrtr`, from Linux's own struct definitions.

`net/qrtr/af_qrtr.c` cannot be linked outside the kernel (it is the socket layer), so this does less than the QMI
oracle: `build.sh` cuts the text of `struct qrtr_hdr_v1`, `struct qrtr_hdr_v2` and the `QRTR_*` constants out of
`af_qrtr.c`, `ns.c` and `net/qrtr/qrtr.h` with `sed`/`grep`, takes `struct qrtr_ctrl_pkt` and the packet type enum
from `include/uapi/linux/qrtr.h` as it is, and compiles `harness.c` against them. The harness builds packets the
way `qrtr_node_enqueue()` does (header pushed in front, `size` = payload length, zero padding to a multiple of four
bytes). That building code is about ten lines typed into `harness.c`; the layouts, offsets and numbers are
Linux's.

Output, into `../tests/fixtures/`:

| file            | content                                                                                          |
| --------------- | ------------------------------------------------------------------------------------------------ |
| `packets.txt`   | `<case> <hex>`: v1 and v2 packets, and the bare 20-byte control payloads (`ctrl_*`)              |
| `constants.txt` | `<NAME> <value>`: numbers, `sizeof` and `offsetof` of the three structs, name service limits    |

Cases are the traffic of the Galaxy Book Go: node 1 (the host) and node 0 (the modem), the WLFW service 69 at
instance 1 on port 0x2a, a QMI client on port 0x4001. Some cases are packets the receiver must refuse (a zero
`size`, a control packet for a data port, a short NEW_SERVER).

The Rust tests (`tests/packets_oracle.rs`, `tests/router.rs`) decode them field by field, rebuild them byte for
byte, and compare the router's output against them. What is not covered by the C: the router and name service
logic, which is read from `af_qrtr.c` and `ns.c`, not run.

## Regenerate

On buildhost (any host with gcc and the Linux tree):

```sh
cd drivers/lucina-qrtr/oracle
./build.sh            # LINUX=/path/to/linux WORK=/scratch/dir OUT=/where/fixtures/go to override
```

Defaults: `LINUX=/home/user/Projects/Lucina/lucinaos-kernel`, `WORK=$HOME/scratch/juno-wifi/qrtr-oracle`
(generated header and binary only), `OUT=../tests/fixtures`.
