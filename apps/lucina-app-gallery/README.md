# lucina-app-gallery

The photo grid, the viewer and the batches. `cargo test -p lucina-app-gallery` is hermetic: `src/tests.rs`
drives the app through `lucina_files::fake::Fake`, which answers the real `/v1/media` routes in this process
from the real JPEG tiles of `tests/fixtures`, so the whole suite runs with nothing deployed and nothing
listening.

Two tests are not, and are `#[ignore]`d; each reason names the hermetic twin that runs by default against
that `Fake`, or says none:

| Test | Against the deployed server | Hermetic twin |
| --- | --- | --- |
| `the_real_servers_photos_reach_the_screen` | the same `photos_reach_the_screen` script: pages, tiles, ThumbHash, albums, months, details, favourite, rotation, Trash, a word, a new album | `the_servers_photos_reach_the_screen`, which runs that very script on `Fake::imported` |
| `the_real_servers_download_writes_the_original` | `server::download` writing an original, and the file it writes being the size the item reports | none: no test here fetches an original through `server::download` |

These two start the real `lucina-media`, the server the apps reach through p1. That binary is built outside
this repo and nothing here looks for it on its own: `LUCINA_MEDIA_BIN` names it, and `Engine::start`
(`../../services/lucina-files/src/engine.rs`) reads only that variable. The one command that runs the real-server variants
of this crate and of `lucina-files`, from the repository root:

    LUCINA_MEDIA_BIN=$HOME/.local/bin/lucina-media cargo test -p lucina-files -p lucina-app-gallery -- --ignored

Without the variable these two are counted ignored, which is the point: a machine without the server reports
them ignored instead of passing a body that never ran, and so these two `expect` rather than skip.
