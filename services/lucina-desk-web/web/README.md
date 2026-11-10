# voxl web client

These files are voxl's browser client (https://github.com/tembo/voxl, MIT license; see VOXL-LICENSE), used unchanged. lucina-desk-web serves them with include_bytes! and implements voxl's server protocol. The Inter font is licensed under the SIL Open Font License; see inter-LICENSE.txt. index.html contains a `{{title}}` placeholder that the server replaces with "Juno" (HTML-escaped).

Pin: 47bb94ec4d253638c1470eb81906676e1bef4b00

| File | Source (inside voxl) | SHA256 |
|------|----------------------|--------|
| audio.js | bundle of web/src/worklets/audio.ts | f091cdd3b2b9e85563b5640921ae733593721e02fc169568e9194f9174161e5a |
| client.js | bundle of web/src/client/main.ts | 29c59c0d83b05c9f1f973f00a0a7c0308e489e51cbd0051374aa101b5de48c73 |
| favicon.ico | web/favicon.ico | 3734f66b9f97b7ff239d7f64767e36e507aad0616fc64d1ae5abae85f6115eba |
| index.html | web/index.html | 01f8e5912c2399b195c0b0224ecc716d6cdb5d2d9bae8a6333e1716ad99880e3 |
| inter-LICENSE.txt | web/inter-LICENSE.txt | 3b0a5fca3d17942cde889069889dedbbbd075e9b599968c82a95f4d944e9b345 |
| inter.woff2 | web/inter.woff2 | 3bc1101fd3c6f3ec0e8c2f57e6c12790b92adce03b70d0e504160a517c2cfc5a |
| mic.js | bundle of web/src/worklets/mic.ts | 2edcf8cbafe4723f9cd704181f18a6ad4997815a9d80b32e26eb33a75a78b4fa |
| VOXL-LICENSE | LICENSE | ac25984d13ede5f650f229c7ed94255454dbdd20d15200a96274335c2caef647 |
| worker.js | bundle of web/src/worker.ts | 5177221039e44439388e5cd856326289e2795a5955438b4432dd888b696e54f2 |

Rebuilding the bundles
----------------------
From a checkout of voxl at the pin above, with Ubuntu's esbuild package (`apt install esbuild`), run exactly (this is the argument list in voxl's build.rs `bundle_web`):

```bash
esbuild client=web/src/client/main.ts worker=web/src/worker.ts audio=web/src/worklets/audio.ts mic=web/src/worklets/mic.ts \
  --bundle --format=iife --target=es2022 --minify --log-level=warning --outdir=.
```

Compare the resulting SHA256 values above. Note that a different esbuild version may produce different bytes.

Moving the pin
--------------
Update the files, this table, and docs/upstreams.toml together.

Plain HTTP
----------
The page decodes with WebCodecs, which browsers offer only on secure pages (HTTPS or localhost). Opened as
`http://<laptop address>:6901/` from another machine it shows "voxl needs HTTPS or localhost". Either forward the port
(`ssh -L 6901:<laptop address>:6901 <a machine on that network>`, then open `http://localhost:6901/?token=...`), or mark
the address as secure in the browser (Chromium: `chrome://flags/#unsafely-treat-insecure-origin-as-secure`).
