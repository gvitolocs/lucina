//! lucina-rootimg: runs on the build machine. Turns an ext2 image into the stream `lucina-install-disk` applies (crate `image`).
//!
//! ```text
//! lucina-rootimg pack <image.ext2> > out.stream
//! lucina-rootimg info <out.stream>
//! ```

// Juno with no C library: Eyra is the libc and the program start, in Rust (docs/research/juno-pure-rust.md).
#[cfg(all(target_os = "linux", target_env = "gnu", target_arch = "aarch64"))]
extern crate eyra;

use std::{
    fs::File,
    io::{self, BufReader, BufWriter, Read, Write},
    os::unix::fs::FileExt,
    process::ExitCode,
};

use lucina_remote::{
    image::{BLOCK, Extent, Header, read_extent, sum_extent_head, used_blocks, uuid_hex, write_extent},
    stream::hex,
};
use sha2::{Digest, Sha256};

fn pack(path: &str) -> Result<(), String> {
    let f = File::open(path).map_err(|e| format!("{path}: {e}"))?;
    let read = |b: u64| -> io::Result<Vec<u8>> {
        let mut buf = vec![0u8; BLOCK];
        f.read_exact_at(&mut buf, b * BLOCK as u64)?;
        Ok(buf)
    };
    let used = used_blocks(read)?;
    let info = lucina_remote::image::parse_superblock(&read(0).map_err(|e| e.to_string())?)
        .ok_or("not an ext2 image")?;
    let zero: Vec<bool> = used
        .iter()
        .enumerate()
        .map(|(i, u)| *u && read(i as u64).is_ok_and(|b| b.iter().all(|x| *x == 0)))
        .collect();
    let extents = lucina_remote::image::extents(&used, |b| zero[b as usize]);
    let mut sum = Sha256::new();
    let zeros = vec![0u8; BLOCK];
    let mut data_blocks = 0u64;
    for e in &extents {
        sum_extent_head(&mut sum, e);
        for b in e.first..e.first + e.count {
            if e.zero {
                sum.update(&zeros);
            } else {
                sum.update(read(b).map_err(|x| x.to_string())?);
                data_blocks += 1;
            }
        }
    }
    let header = Header {
        blocks: info.blocks,
        extents: extents.len() as u64,
        uuid: info.uuid,
        sha256: sum.finalize().into(),
    };
    let mut out = BufWriter::with_capacity(1 << 20, io::stdout().lock());
    header.write(&mut out).map_err(|e| e.to_string())?;
    for e in &extents {
        write_extent(&mut out, e).map_err(|x| x.to_string())?;
        if !e.zero {
            for b in e.first..e.first + e.count {
                out.write_all(&read(b).map_err(|x| x.to_string())?)
                    .map_err(|x| x.to_string())?;
            }
        }
    }
    out.flush().map_err(|e| e.to_string())?;
    eprintln!(
        "lucina-rootimg: uuid {}, {} blocks in the file system, {} used, {} with data, {} extents, sha256 {}",
        uuid_hex(&info.uuid),
        info.blocks,
        used.iter().filter(|u| **u).count(),
        data_blocks,
        extents.len(),
        hex(&header.sha256)
    );
    Ok(())
}

fn info(path: &str) -> Result<(), String> {
    let mut f = BufReader::new(File::open(path).map_err(|e| format!("{path}: {e}"))?);
    let h = Header::read(&mut f)?;
    println!("uuid {} blocks {} extents {} sha256 {}", uuid_hex(&h.uuid), h.blocks, h.extents, hex(&h.sha256));
    let mut sink = vec![0u8; BLOCK];
    for _ in 0..h.extents {
        let e: Extent = read_extent(&mut f)?;
        if !e.zero {
            for _ in 0..e.count {
                f.read_exact(&mut sink).map_err(|x| x.to_string())?;
            }
        }
    }
    Ok(())
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let result = match args.iter().map(String::as_str).collect::<Vec<_>>().as_slice() {
        ["pack", path] => pack(path),
        ["info", path] => info(path),
        _ => Err("usage: lucina-rootimg pack <image> | info <stream>".to_string()),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("lucina-rootimg: {e}");
            ExitCode::FAILURE
        }
    }
}
