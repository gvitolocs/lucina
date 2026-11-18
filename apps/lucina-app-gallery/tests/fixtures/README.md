# The sample tiles

Twelve small JPEGs: the two renditions the media server keeps of each of the six photos that
`Engine::start` seeds (`services/lucina-files/src/engine.rs`) — `<n>-s.jpg` the grid tile, `<n>-l.jpg` the
bigger one, `n` the same seed as `lucina_files::fake::painted`. `Fake::imported` serves them in place of
the pictures it would otherwise paint, so that `apps/lucina-app-gallery`'s server-backed test runs the
real decoder over real JPEG bytes with no server, no port and nothing to wait for
(`services/lucina-files/src/fake.rs`, `apps/lucina-app-gallery/src/tests.rs`).

The app asks for them by the server's own paths: `/r/preview` for a grid tile, `/thumb?size=` for the
viewer's, `/poster` for a video's still; the small tile answers up to 512 and the large one above it,
which is how `Fake` has always chosen between the two. The folder is named
in `tests.rs` as `TILES`, not in the environment, because a test that needed `LUCINA_SAMPLE_PHOTOS` set
would quietly skip when it is not.

## How they were made

Every byte comes from `painted(n, w, h)` alone: no camera, no stock photo, no secret and nothing
personal. Anyone with the workspace and ImageMagick 7 gets the same files, and the `sha256sum` below
says when they did.

    # 1. the twelve pictures as PNG, from the seed of the photo they stand for
    #    (services/lucina-files/examples/dump-painted.rs, kept for this)
    cargo run -p lucina-files --example dump-painted -- ~/scratch/tmp/tiles-png
    # 2. JPEG, stripped of every marker that is not the picture
    here=$(git rev-parse --show-toplevel)/apps/lucina-app-gallery/tests/fixtures
    for f in ~/scratch/tmp/tiles-png/*.png; do
      magick "$f" -strip -sampling-factor 4:2:0 -quality 82 "$here/$(basename "${f%.png}").jpg"
    done

`painted` is deterministic, and so is the encoder for a fixed quality, sampling factor and `-strip`;
the sizes are what libjpeg gives, not something chosen. A different quality, or an encoder of another
version, gives different bytes of the same picture — the test reads the pixels through the decoder and
does not hash them, so that is harmless. What keeps the set honest is that the only input is
`painted(n, w, h)`, and that `Fake::imported` refuses the folder unless it holds exactly six pairs.

Sizes are those of the server's two shapes: 320x240 (240x320 for the portrait `4`) and 960x720
(720x960). Each file is well under 20 KiB, 18 KiB the largest, 128 KiB the twelve together.

| file | bytes | sha256 |
| --- | --- | --- |
| `0-s.jpg` | 4646 | `b2576321c3a26bc864a9155f61b6f5156f3d98cbfc51b80e4dfefa95c272b9b7` |
| `0-l.jpg` | 18497 | `05e79fca39df92f54f92aae385ea5a2cbb6c9d60335863f9fe42bdbd33d72b39` |
| `1-s.jpg` | 4505 | `fce0a6ef26ddad7044a259204f5e949e2cb5e8e1504d3c0467c1995e97fe3d07` |
| `1-l.jpg` | 16476 | `11efb2209dd743e7232d61e885483d286ede5c963782adea7ca653193db8eebd` |
| `2-s.jpg` | 4429 | `7be8e7ea31dd91ecdc748d41f9f987e260223d3308a26b290fb04aa9ea3f66ac` |
| `2-l.jpg` | 16200 | `bc8ea55084e37f99bd0514a768571f736dc45aabb555b65b3dc8fa25447d2364` |
| `3-s.jpg` | 4730 | `b53746bc949df7a241224c2201be0a3bd10c8828b01218b23d395b96e9755eae` |
| `3-l.jpg` | 17495 | `56886b2862246c44477dcd98b4c4b1143f42c711f3a4c40a596edae1f87b9497` |
| `4-s.jpg` | 4181 | `6e840d652870b7fbb6e3b3fa708c586545a3f1b43965cd3c2afa5813328d55a8` |
| `4-l.jpg` | 15055 | `0ee0e25e4a50d8bda0ab30bc4ce9f0fc79fbc83a256e5f6370976e2b7bb85a8a` |
| `5-s.jpg` | 4298 | `3b30dd5f6383e9f2111896c3e6e21facbfabdc9ea9ca9bf8e1138993d89f97de` |
| `5-l.jpg` | 17278 | `d6e922729f5f11d4fe09a7bfb48adff9be250d0bf4f00d66a66861eafb078638` |
