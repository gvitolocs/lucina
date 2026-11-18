//! The capture of `services/lucina-files/tests/fixtures/server-keys.json`: the key SET (paths only, no values)
//! that the real `lucina-media` answers for each of [`SHAPE_ROUTES`], taken from a server of this run's own —
//! `Engine::start` with its own temporary HOME, ports and data, seeded with the same generated photos the
//! tests seed — so nothing that is deployed and running is contacted, read or disturbed. Kept so that the
//! fixture can be taken again from what is in the tree; `tests/fixtures/README.md` records the binary it came
//! from. The server is the one `LUCINA_MEDIA_BIN` names, and this names no other.
//!
//!     LUCINA_MEDIA_BIN=$HOME/.local/bin/lucina-media \
//!       cargo run -p lucina-files --example dump-server-keys -- <out.json>

use std::io::Write;

use lucina_files::engine::{Engine, SHAPE_ROUTES, key_paths};
use serde_json::{Map, Value};

fn main() {
    let out = std::env::args()
        .nth(1)
        .expect("the file to write the captured key set to");
    let engine = Engine::start("keys")
        .expect("LUCINA_MEDIA_BIN: the lucina-media to capture the shape from");
    let mut real = (engine.connect())().unwrap();
    let mut routes = Map::new();
    for route in SHAPE_ROUTES {
        let keys = key_paths(&real.get(route).unwrap());
        println!("{route}: {} keys", keys.len());
        routes.insert(
            route.to_string(),
            Value::Array(keys.into_iter().map(Value::String).collect()),
        );
    }
    // Filled in sorted order, and a `Map` is itself ordered: the same capture is the same text.
    let mut text = serde_json::to_string_pretty(&Value::Object(routes)).unwrap();
    text.push('\n');
    let mut f = std::fs::File::create(&out).expect("open the output");
    f.write_all(text.as_bytes()).expect("write the key set");
    f.flush().unwrap();
    println!("{out}");
}
