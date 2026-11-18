# lucina-files

The Drive client, the disk cache, the picture queue and the file picker. `cargo test -p lucina-files` is
hermetic: `src/fake.rs` answers the `/v1/media` routes in this process from a made-up library, so the model,
the cache, the queue and the picker all run with nothing deployed and nothing listening.

Three tests are not, and are `#[ignore]`d; each reason names the hermetic twin that runs by default against
that `Fake`:

| Test | Against the deployed server | Hermetic twin |
| --- | --- | --- |
| `the_sample_library_answers_in_the_real_servers_shape` | compares the key sets `Fake` sends with the ones the real server sends, and that the captured key set is still what it sends | `the_sample_library_answers_in_the_captured_servers_shape`, the same comparison over the same routes against `tests/fixtures/server-keys.json` |
| `the_model_reads_the_real_servers_drive` | roots, browse, file types, background text extraction, the archive listing, a chunked download, a 404 | `a_drive_listing_keeps_folders_and_documents_only`, which covers the roots, the browse page and the typing; the rest is asserted only here |
| `the_picker_fetches_from_the_real_servers_drive` | the picker fetching one Drive file into a local folder by keyboard | `a_file_from_the_drive_arrives_as_a_local_path` |

`tests/fixtures/server-keys.json` stands in for the server in that twin: the key paths each route answers
with, no values, captured once from the deployed binary by `examples/dump-server-keys.rs` over a server of its
own on temporary folders. `tests/fixtures/README.md` says which build it came from and how to take it again.

These three start the real `lucina-media`, the server the apps reach through p1. That binary is built outside
this repo and nothing here looks for it on its own: `LUCINA_MEDIA_BIN` names it, and `Engine::start`
(`src/engine.rs`) reads only that variable. The one command that runs the real-server variants of both this
crate and `lucina-app-gallery`, from the repository root:

    LUCINA_MEDIA_BIN=$HOME/.local/bin/lucina-media cargo test -p lucina-files -p lucina-app-gallery -- --ignored

Without the variable these five are counted ignored, which is the point: a machine without the server reports
them ignored instead of passing a body that never ran. Asking for `--ignored` without naming a binary is not
the same proof: the three here take the `None` branch of `Engine::start` and pass for nothing.
