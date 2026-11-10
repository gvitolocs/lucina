//! `lucina-initramfs overlay <in.cpio[.gz]> <out.cpio.gz> <dir>...`: the archive with each directory's tree added
//! on top (later directories win). `lucina-initramfs list <archive>` prints mode, size and path of each entry.

use std::{path::Path, process::ExitCode};

fn run(args: &[String]) -> Result<(), String> {
    match args {
        [cmd, input, output, dirs @ ..] if cmd == "overlay" && !dirs.is_empty() => {
            let bytes = std::fs::read(input).map_err(|e| format!("{input}: {e}"))?;
            let mut entries =
                lucina_initramfs::read_archive(&bytes).map_err(|e| format!("{input}: {e}"))?;
            for dir in dirs {
                let tree = lucina_initramfs::tree_entries(Path::new(dir))
                    .map_err(|e| format!("{dir}: {e}"))?;
                entries = lucina_initramfs::overlay(entries, tree);
            }
            let out = lucina_initramfs::write_gzip(&entries).map_err(|e| e.to_string())?;
            std::fs::write(output, out).map_err(|e| format!("{output}: {e}"))
        }
        [cmd, input] if cmd == "list" => {
            let bytes = std::fs::read(input).map_err(|e| format!("{input}: {e}"))?;
            for e in lucina_initramfs::read_archive(&bytes).map_err(|e| format!("{input}: {e}"))? {
                println!("{:06o} {:>9} {}", e.mode, e.data.len(), e.name);
            }
            Ok(())
        }
        _ => Err(
            "usage: lucina-initramfs overlay <in> <out.cpio.gz> <dir>... | list <archive>".into(),
        ),
    }
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match run(&args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("lucina-initramfs: {e}");
            ExitCode::FAILURE
        }
    }
}
