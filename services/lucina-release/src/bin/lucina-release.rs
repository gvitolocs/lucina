//! `lucina-release`: make, sign and check the net installer's per-board manifests. See `docs/design/lucina-net-installer.md`.

use lucina_release::{
    DEFAULT_CHUNK, Manifest, b64_encode, parse_keys, signing_key_from_seed, verify,
};
use std::io::{Read, Write};
use std::process::ExitCode;

const USAGE: &str = "\
usage:
  lucina-release make --board B --commit C --date D --seq N [--chunk BYTES] [--write T]... <image> > manifest.unsigned
  lucina-release sign <manifest.unsigned> > manifest
        the private key (32-byte seed, base64) is read ONLY from the environment variable LUCINA_RELEASE_KEY
  lucina-release verify --keys <file of base64 public keys, one per line> <manifest>
  lucina-release serve <dir> --addr <ip:port>
        GET and HEAD of the files of <dir> over HTTP/1.1, with byte ranges: the LAN server a net installer stick
        downloads from (docs/design/lucina-net-installer.md section 4). One log line per request on stderr.
  lucina-release keygen-test
        prints a throwaway test seed and its public key. For tests and QEMU ONLY: never a release key.
";

fn fail(msg: impl std::fmt::Display) -> ExitCode {
    eprintln!("lucina-release: {msg}");
    ExitCode::from(1)
}

fn take(args: &mut Vec<String>, flag: &str) -> Result<Option<String>, String> {
    match args.iter().position(|a| a == flag) {
        None => Ok(None),
        Some(i) => {
            if i + 1 >= args.len() {
                return Err(format!("{flag} needs a value"));
            }
            let v = args.remove(i + 1);
            args.remove(i);
            Ok(Some(v))
        }
    }
}

fn need(args: &mut Vec<String>, flag: &str) -> Result<String, String> {
    take(args, flag)?.ok_or_else(|| format!("{flag} is required"))
}

fn make(mut args: Vec<String>) -> Result<(), String> {
    let board = need(&mut args, "--board")?;
    let commit = need(&mut args, "--commit")?;
    let date = need(&mut args, "--date")?;
    let seq: u64 = need(&mut args, "--seq")?
        .parse()
        .map_err(|_| "--seq is not a number".to_string())?;
    let chunk: u32 = match take(&mut args, "--chunk")? {
        Some(c) => c
            .parse()
            .map_err(|_| "--chunk is not a number".to_string())?,
        None => DEFAULT_CHUNK,
    };
    let mut writes = Vec::new();
    while let Some(w) = take(&mut args, "--write")? {
        writes.push(w);
    }
    let [image] = args.as_slice() else {
        return Err("exactly one image file is needed".into());
    };
    let f = std::fs::File::open(image).map_err(|e| format!("{image}: {e}"))?;
    let mut m = Manifest::make(
        &board,
        &commit,
        &date,
        seq,
        chunk,
        std::io::BufReader::new(f),
    )
    .map_err(|e| e.to_string())?;
    m.writes = writes;
    // Render through the same checks the reader applies.
    let text = m.render();
    std::io::stdout()
        .write_all(text.as_bytes())
        .map_err(|e| e.to_string())
}

fn sign(args: Vec<String>) -> Result<(), String> {
    let [path] = args.as_slice() else {
        return Err("exactly one unsigned manifest is needed".into());
    };
    let seed = std::env::var("LUCINA_RELEASE_KEY")
        .map_err(|_| "LUCINA_RELEASE_KEY is not set".to_string())?;
    let key = signing_key_from_seed(&seed)
        .map_err(|_| "LUCINA_RELEASE_KEY is not a base64 32-byte seed".to_string())?;
    let text = std::fs::read_to_string(path).map_err(|e| format!("{path}: {e}"))?;
    let m = Manifest::parse_unsigned(&text).map_err(|e| e.to_string())?;
    let signed = m.sign(&key);
    std::io::stdout()
        .write_all(signed.as_bytes())
        .map_err(|e| e.to_string())
}

fn check(mut args: Vec<String>) -> Result<(), String> {
    let keys_path = need(&mut args, "--keys")?;
    let [path] = args.as_slice() else {
        return Err("exactly one manifest is needed".into());
    };
    let keys =
        parse_keys(&std::fs::read_to_string(&keys_path).map_err(|e| format!("{keys_path}: {e}"))?)
            .map_err(|e| e.to_string())?;
    if keys.is_empty() {
        return Err(format!("{keys_path} holds no keys"));
    }
    let text = std::fs::read_to_string(path).map_err(|e| format!("{path}: {e}"))?;
    let m = verify(&text, &keys).map_err(|e| e.to_string())?;
    println!("board {}", m.board);
    println!("commit {}", m.commit);
    println!("date {}", m.date);
    println!("seq {}", m.seq);
    println!("size {}", m.size);
    println!("chunks {}", m.chunks.len());
    Ok(())
}

fn serve(mut args: Vec<String>) -> Result<(), String> {
    let addr: std::net::SocketAddr = need(&mut args, "--addr")?
        .parse()
        .map_err(|_| "--addr is not an <ip:port> to listen on".to_string())?;
    let [dir] = args.as_slice() else {
        return Err("exactly one folder to serve is needed".into());
    };
    let dir = std::path::Path::new(dir);
    if !dir.is_dir() {
        return Err(format!("{} is not a folder", dir.display()));
    }
    lucina_release::serve::serve(dir, addr).map_err(|e| format!("serve: {e}"))
}

fn keygen_test() -> Result<(), String> {
    let mut seed = [0u8; 32];
    std::fs::File::open("/dev/urandom")
        .and_then(|mut f| f.read_exact(&mut seed))
        .map_err(|e| format!("/dev/urandom: {e}"))?;
    let key = ed25519_dalek::SigningKey::from_bytes(&seed);
    println!("TEST KEY, not for releases");
    println!("seed {}", b64_encode(&seed));
    println!("public {}", b64_encode(key.verifying_key().as_bytes()));
    Ok(())
}

fn main() -> ExitCode {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    if args.is_empty() || args[0] == "--help" || args[0] == "-h" {
        print!("{USAGE}");
        return ExitCode::from(if args.is_empty() { 2 } else { 0 });
    }
    let cmd = args.remove(0);
    let r = match cmd.as_str() {
        "make" => make(args),
        "sign" => sign(args),
        "verify" => check(args),
        "serve" => serve(args),
        "keygen-test" => keygen_test(),
        _ => Err(format!("unknown command `{cmd}`\n{USAGE}")),
    };
    match r {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => fail(e),
    }
}
