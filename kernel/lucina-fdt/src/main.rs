//! `lucina-fdt`: inspect a DTB and add `/reserved-memory` regions, a framebuffer or plain nodes to it.

use std::{path::Path, process::ExitCode};

use lucina_fdt::{FbFormat, FramebufferSpec, RegionSpec, Tree, cells32, files, report, string};

const USAGE: &str = "usage:
  lucina-fdt info <dtb>
  lucina-fdt add-reserved <in.dtb> -o <out.dtb> --region <name>@<hex addr>+<hex size>[,no-map] [--region ...]
  lucina-fdt add-framebuffer <in.dtb> -o <out.dtb> --addr <hex> --width <dec> --height <dec> --format x8r8g8b8|a8r8g8b8|r5g6b5 [--stride <bytes>] [--no-map]
  lucina-fdt delete-node <in.dtb> -o <out.dtb> <path>
  lucina-fdt add-node <in.dtb> -o <out.dtb> <path> [--u32 <name>=<v>[,<v>...]] [--str <name>=<s>[;<s>...]] [--empty <name>]...
";
fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match run(&args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(Failure::Usage(m)) => {
            eprintln!("lucina-fdt: {m}\n{USAGE}");
            ExitCode::from(2)
        }
        Err(Failure::Other(m)) => {
            eprintln!("lucina-fdt: {m}");
            ExitCode::FAILURE
        }
    }
}

enum Failure {
    Usage(String),
    Other(String),
}

fn other<E: std::fmt::Display>(what: &str) -> impl Fn(E) -> Failure + '_ {
    move |e| Failure::Other(format!("{what}: {e}"))
}

fn run(args: &[String]) -> Result<(), Failure> {
    match args.first().map(String::as_str) {
        Some("info") => match args {
            [_, input] => {
                let bytes = files::read_file(Path::new(input)).map_err(other(input))?;
                print!("{}", report::info(&bytes).map_err(other(input))?);
                Ok(())
            }
            _ => Err(Failure::Usage("info takes exactly one file".into())),
        },
        Some("add-reserved") => add_reserved(&args[1..]),
        Some("add-framebuffer") => add_framebuffer(&args[1..]),
        Some("add-node") => add_node(&args[1..]),
        Some("delete-node") => match args {
            [_, input, o, output, path] if o == "-o" || o == "--output" => {
                let bytes = files::read_file(Path::new(input)).map_err(other(input))?;
                let mut tree = Tree::parse(&bytes).map_err(other(input))?;
                if !tree.remove_node(path).map_err(other(path))? {
                    return Err(Failure::Other(format!("{path}: no such node")));
                }
                let out = tree.to_bytes().map_err(other("serialize"))?;
                files::write_file(Path::new(output), &out).map_err(other(output))?;
                Ok(())
            }
            _ => Err(Failure::Usage(
                "delete-node takes <in.dtb> -o <out.dtb> <path>".into(),
            )),
        },
        Some(c) => Err(Failure::Usage(format!("unknown command {c:?}"))),
        None => Err(Failure::Usage("missing command".into())),
    }
}

fn add_reserved(args: &[String]) -> Result<(), Failure> {
    let mut input = None;
    let mut output = None;
    let mut regions = Vec::new();
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "-o" | "--output" => {
                let v = it
                    .next()
                    .ok_or_else(|| Failure::Usage(format!("{a} needs a value")))?;
                if output.replace(v).is_some() {
                    return Err(Failure::Usage("-o given twice".into()));
                }
            }
            "--region" => {
                let v = it
                    .next()
                    .ok_or_else(|| Failure::Usage("--region needs a value".into()))?;
                regions.push(RegionSpec::parse(v).map_err(other(v))?);
            }
            s if s.starts_with('-') => return Err(Failure::Usage(format!("unknown option {s:?}"))),
            _ => {
                if input.replace(a).is_some() {
                    return Err(Failure::Usage("more than one input file".into()));
                }
            }
        }
    }
    let input = input.ok_or_else(|| Failure::Usage("missing input dtb".into()))?;
    let output = output.ok_or_else(|| Failure::Usage("missing -o <out.dtb>".into()))?;
    if regions.is_empty() {
        return Err(Failure::Usage("at least one --region is required".into()));
    }
    let bytes = files::read_file(Path::new(input)).map_err(other(input))?;
    let mut tree = Tree::parse(&bytes).map_err(other(input))?;
    tree.add_reserved_regions(&regions)
        .map_err(other("add-reserved"))?;
    let out = tree.to_bytes().map_err(other("serialize"))?;
    files::write_file(Path::new(output), &out).map_err(other(output))?;
    Ok(())
}

fn add_framebuffer(args: &[String]) -> Result<(), Failure> {
    let (mut input, mut output) = (None, None);
    let (mut addr, mut width, mut height, mut format) = (None, None, None, None);
    let mut no_map = false;
    let mut stride = None;
    let mut it = args.iter();
    while let Some(a) = it.next() {
        let mut value = || {
            it.next()
                .ok_or_else(|| Failure::Usage(format!("{a} needs a value")))
        };
        match a.as_str() {
            "-o" | "--output" => output = Some(value()?),
            "--addr" => {
                let v = value()?;
                let digits = v.strip_prefix("0x").unwrap_or(v);
                addr = Some(u64::from_str_radix(digits, 16).map_err(other("--addr"))?);
            }
            "--width" => width = Some(value()?.parse::<u32>().map_err(other("--width"))?),
            "--height" => height = Some(value()?.parse::<u32>().map_err(other("--height"))?),
            "--format" => format = Some(FbFormat::parse(value()?).map_err(other("--format"))?),
            "--stride" => stride = Some(value()?.parse::<u32>().map_err(other("--stride"))?),
            "--no-map" => no_map = true,
            s if s.starts_with('-') => return Err(Failure::Usage(format!("unknown option {s:?}"))),
            _ => {
                if input.replace(a).is_some() {
                    return Err(Failure::Usage("more than one input file".into()));
                }
            }
        }
    }
    let missing = |what: &str| Failure::Usage(format!("missing {what}"));
    let input = input.ok_or_else(|| missing("input dtb"))?;
    let output = output.ok_or_else(|| missing("-o <out.dtb>"))?;
    let spec = FramebufferSpec {
        addr: addr.ok_or_else(|| missing("--addr"))?,
        width: width.ok_or_else(|| missing("--width"))?,
        height: height.ok_or_else(|| missing("--height"))?,
        format: format.ok_or_else(|| missing("--format"))?,
        stride,
        no_map,
    };
    let bytes = files::read_file(Path::new(input)).map_err(other(input))?;
    let mut tree = Tree::parse(&bytes).map_err(other(input))?;
    tree.add_framebuffer(&spec)
        .map_err(other("add-framebuffer"))?;
    let out = tree.to_bytes().map_err(other("serialize"))?;
    files::write_file(Path::new(output), &out).map_err(other(output))?;
    Ok(())
}

/// A new node (or more properties on an existing one): `--u32` cells separated by `,` (hex with `0x`, else decimal),
/// `--str` string lists separated by `;` (compatible strings contain commas), `--empty` boolean properties.
fn add_node(args: &[String]) -> Result<(), Failure> {
    let (mut input, mut output, mut path) = (None, None, None);
    let mut props: Vec<(String, Vec<u8>)> = Vec::new();
    let mut it = args.iter();
    while let Some(a) = it.next() {
        let mut value = || {
            it.next()
                .ok_or_else(|| Failure::Usage(format!("{a} needs a value")))
        };
        match a.as_str() {
            "-o" | "--output" => output = Some(value()?),
            "--u32" | "--str" => {
                let v = value()?;
                let (name, list) = v
                    .split_once('=')
                    .ok_or_else(|| Failure::Usage(format!("{a} takes <name>=<values>")))?;
                let bytes = if a == "--u32" {
                    let words = list
                        .split(',')
                        .map(|w| match w.strip_prefix("0x") {
                            Some(hex) => u32::from_str_radix(hex, 16),
                            None => w.parse(),
                        })
                        .collect::<Result<Vec<u32>, _>>()
                        .map_err(other(name))?;
                    cells32(&words)
                } else {
                    list.split(';').flat_map(string).collect()
                };
                props.push((name.to_string(), bytes));
            }
            "--empty" => props.push((value()?.clone(), Vec::new())),
            s if s.starts_with('-') => return Err(Failure::Usage(format!("unknown option {s:?}"))),
            _ if input.is_none() => input = Some(a),
            _ if path.is_none() => path = Some(a),
            _ => {
                return Err(Failure::Usage(
                    "add-node takes one input and one path".into(),
                ));
            }
        }
    }
    let missing = |what: &str| Failure::Usage(format!("missing {what}"));
    let input = input.ok_or_else(|| missing("input dtb"))?;
    let output = output.ok_or_else(|| missing("-o <out.dtb>"))?;
    let path = path.ok_or_else(|| missing("node path"))?;
    let bytes = files::read_file(Path::new(input)).map_err(other(input))?;
    let mut tree = Tree::parse(&bytes).map_err(other(input))?;
    if tree.find(path).map_err(other(path))?.is_none() {
        tree.add_node(path).map_err(other(path))?;
    }
    for (name, value) in props {
        tree.set_prop(path, &name, value).map_err(other(&name))?;
    }
    let out = tree.to_bytes().map_err(other("serialize"))?;
    files::write_file(Path::new(output), &out).map_err(other(output))?;
    Ok(())
}
