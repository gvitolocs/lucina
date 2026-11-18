# The captured server key set

`server-keys.json` is the shape of the real media server's answers with their contents thrown away: for each
route of `SHAPE_ROUTES` (`../../src/engine.rs`), the sorted JSON key paths it answers with — `a.b` under a
nested object, `a[].b` under every element of a list, no value, no size, nothing that came from a picture or
a document. `../../src/tests.rs` reads it as the server's half of
`the_sample_library_answers_in_the_captured_servers_shape`, the hermetic twin of
`the_sample_library_answers_in_the_real_servers_shape`.

## How it was captured

Once, on 2026-11-18, from the deployed build — the binary the user unit `lucina-media.service` runs, not a
build of this branch, which is why the daily real-server job re-asks the same question live:

    LUCINA_MEDIA_BIN=$HOME/.local/bin/lucina-media \
      cargo run -p lucina-files --example dump-server-keys -- services/lucina-files/tests/fixtures/server-keys.json

| | |
| --- | --- |
| binary | `$HOME/.local/bin/lucina-media` |
| size, mtime | 39570480 bytes, 2026-11-03 03:39:52 +0000 |
| its sha256 | `ef02f66e017cf481815b69aa1ad631a00ac4ec3ce65a3e5d7fb9142109d089a6` |
| this file | 5398 bytes, sha256 `4bd72c3f3a3d505718d7e963a143bf87ae8d89a63d561d58eed3a5a54f3ec6f4` |

`examples/dump-server-keys.rs` asks `Engine::start`, which takes the server from `LUCINA_MEDIA_BIN` and nothing
else and runs one of its own: a temporary HOME, data and store, two free ports, the same six generated photos
and the same `Lezioni` folder the tests seed, `LUCINA_MEDIA_PREPARE=0`. No library but that temporary one is
read and no port the deployed service listens on is opened. Taken twice that way the two files were
byte-identical, which is the determinism the twin leans on; per route it is 8, 10, 76, 57, 57, 3 and 4 keys.

One thing the capture does not freeze into an assertion: the two `/v1/media/items` routes send a top-level `ok`
that `Fake` does not, so the twin keeps the containment tolerance the real-server test states at the top level
and is strict about every key nested below it.
