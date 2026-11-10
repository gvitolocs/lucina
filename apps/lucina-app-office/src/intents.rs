//! The intents Office offers Lucina (docs/research/juno-intents.md): `office.open`, `office.create`. Each goes through the same
//! `Office::open` the picked row goes through; the answer to `office.open` is sent before the exec, since the editor takes the
//! process over.

use std::path::PathBuf;

use lucina_app::intent::{
    ArgSpec, Call, Effect, Entity, GONE, IntentSpec, Reply, Responder, Undo, Write, WriteKind,
    there,
};

use crate::{EXTENSIONS, Office, SAMPLES};

/// The document types a new document may have.
const CREATE_EXTENSIONS: &[&str] = &["docx", "xlsx", "pptx", "odt", "ods", "odp"];

/// What Office offers, listed by `<program> --intents` and named in `manifest.app`.
pub const INTENTS: &[IntentSpec] = &[
    IntentSpec {
        name: "office.open",
        description: "Open a document in the editor, and return the file it opened.",
        args: &[ArgSpec::path("path", "The document to open.", true)],
        effect: Effect::Read,
        shows: true,
        inverse: None,
    },
    IntentSpec {
        name: "office.create",
        description: "Create an empty document at the given path and open it in the editor; the undo is a file undo.",
        args: &[ArgSpec::path("path", "Where the new document goes.", true)],
        effect: Effect::Write,
        shows: true,
        inverse: None,
    },
];

/// One of [`INTENTS`], with its arguments already checked. The same door `dispatch` uses.
pub fn intent(app: &mut Office, call: Call, reply: Responder) {
    match call.name.as_str() {
        "office.open" => open(app, &call, reply),
        "office.create" => create(app, &call, reply),
        _ => reply.send(Reply::failed(format!("no intent {} here", call.name))),
    }
}

fn extension_of(path: &std::path::Path) -> String {
    path.extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase()
}

fn file_name(path: &std::path::Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// The same path as picking the document: `Office::open`. The answer goes first, because the exec replaces the process.
fn open(app: &mut Office, call: &Call, reply: Responder) {
    let path = PathBuf::from(call.args["path"].as_str().unwrap_or(""));
    if !path.is_file() {
        return reply.send(Reply::failed(format!("{} is not a file", path.display())));
    }
    if !EXTENSIONS.iter().any(|e| *e == extension_of(&path)) {
        return reply.send(Reply::failed(format!(
            "{} is not a document the editor opens",
            path.display()
        )));
    }
    let name = file_name(&path);
    let shown = path.to_string_lossy().into_owned();
    reply.send(
        Reply::done(format!("Opening {name} in Office"))
            .with_entity(Entity::new("file", &shown, &name)),
    );
    app.open(&path);
}

/// An empty document at `path`, then opened as `office.open` does. Refuses a path that is there and a type Office does not make.
fn create(app: &mut Office, call: &Call, reply: Responder) {
    let path = PathBuf::from(call.args["path"].as_str().unwrap_or(""));
    let ext = extension_of(&path);
    if !CREATE_EXTENSIONS.iter().any(|e| *e == ext) {
        return reply.send(Reply::failed(format!(
            "{} is not a document type Office creates ({})",
            path.display(),
            CREATE_EXTENSIONS.join(", ")
        )));
    }
    if path.exists() {
        return reply.send(Reply::failed(format!(
            "{} is there already",
            path.display()
        )));
    }
    let bytes = match blank_document(&ext) {
        Ok(b) => b,
        Err(e) => return reply.send(Reply::failed(e)),
    };
    if let Some(parent) = path.parent()
        && let Err(e) = std::fs::create_dir_all(parent)
    {
        return reply.send(Reply::failed(format!("{}: {e}", parent.display())));
    }
    if let Err(e) = std::fs::write(&path, &bytes) {
        return reply.send(Reply::failed(format!("{}: {e}", path.display())));
    }
    let name = file_name(&path);
    let shown = path.to_string_lossy().into_owned();
    let w = Write {
        kind: WriteKind::Create,
        target: format!("file:{shown}"),
        before: GONE.into(),
        after: there(&bytes).into(),
        undo: Undo::File {
            path: shown.clone(),
            before: None,
        },
    };
    reply.send(
        Reply::done(format!("Created {name}"))
            .with_entity(Entity::new("file", &shown, &name))
            .with_write(w),
    );
    app.open(&path);
}

/// The bytes of a new empty document of `ext`: a blank sample from the package when it ships one, else a minimal document written
/// here (every one of these types is a zip).
fn blank_document(ext: &str) -> Result<Vec<u8>, String> {
    if let Ok(entries) = std::fs::read_dir(SAMPLES) {
        let mut blanks: Vec<PathBuf> = entries
            .flatten()
            .map(|e| e.path())
            .filter(|p| {
                let name = p
                    .file_name()
                    .map(|n| n.to_string_lossy().to_lowercase())
                    .unwrap_or_default();
                let stem = name.split('.').next().unwrap_or("");
                (stem == "blank" || stem == "empty") && extension_of(p) == ext
            })
            .collect();
        blanks.sort();
        if let Some(p) = blanks.first() {
            return std::fs::read(p).map_err(|e| format!("{}: {e}", p.display()));
        }
    }
    minimal_document(ext).ok_or_else(|| format!("no empty {ext} document"))
}

/// A minimal valid empty document of `ext`, as a zip of stored entries.
fn minimal_document(ext: &str) -> Option<Vec<u8>> {
    let office_ns = "http://schemas.openxmlformats.org/officeDocument/2006/relationships";
    let (content_types, rels, main, main_part) = match ext {
        "docx" => (
            r#"<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/></Types>"#.to_owned(),
            format!(r#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="{office_ns}/officeDocument" Target="word/document.xml"/></Relationships>"#),
            r#"<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body><w:p/></w:body></w:document>"#.to_owned(),
            "word/document.xml",
        ),
        "xlsx" => (
            r#"<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/xl/workbook.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml"/></Types>"#.to_owned(),
            format!(r#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="{office_ns}/officeDocument" Target="xl/workbook.xml"/></Relationships>"#),
            format!(r#"<workbook xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" xmlns:r="{office_ns}"><sheets/></workbook>"#),
            "xl/workbook.xml",
        ),
        "pptx" => (
            r#"<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/ppt/presentation.xml" ContentType="application/vnd.openxmlformats-officedocument.presentationml.presentation.main+xml"/></Types>"#.to_owned(),
            format!(r#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="{office_ns}/officeDocument" Target="ppt/presentation.xml"/></Relationships>"#),
            format!(r#"<p:presentation xmlns:p="http://schemas.openxmlformats.org/presentationml/2006/main" xmlns:r="{office_ns}"><p:sldIdLst/></p:presentation>"#),
            "ppt/presentation.xml",
        ),
        "odt" | "ods" | "odp" => {
            let (mime, body) = match ext {
                "odt" => ("application/vnd.oasis.opendocument.text", "<office:text/>"),
                "ods" => ("application/vnd.oasis.opendocument.spreadsheet", "<office:spreadsheet/>"),
                _ => ("application/vnd.oasis.opendocument.presentation", "<office:presentation/>"),
            };
            let content = format!(
                r#"<office:document-content xmlns:office="urn:oasis:names:tc:opendocument:xmlns:office:1.0" office:version="1.2"><office:body>{body}</office:body></office:document-content>"#
            );
            let manifest = format!(
                r#"<manifest:manifest xmlns:manifest="urn:oasis:names:tc:opendocument:xmlns:manifest:1.0" manifest:version="1.2"><manifest:file-entry manifest:full-path="/" manifest:media-type="{mime}"/><manifest:file-entry manifest:full-path="content.xml" manifest:media-type="text/xml"/></manifest:manifest>"#
            );
            return Some(zip_stored(&[("mimetype", mime.as_bytes()), ("content.xml", content.as_bytes()), ("META-INF/manifest.xml", manifest.as_bytes())]));
        }
        _ => return None,
    };
    Some(zip_stored(&[
        ("[Content_Types].xml", content_types.as_bytes()),
        ("_rels/.rels", rels.as_bytes()),
        (main_part, main.as_bytes()),
    ]))
}

fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = 0xffff_ffffu32;
    for &b in bytes {
        crc ^= u32::from(b);
        for _ in 0..8 {
            crc = if crc & 1 != 0 {
                (crc >> 1) ^ 0xedb8_8320
            } else {
                crc >> 1
            };
        }
    }
    !crc
}

/// A zip of stored (uncompressed) entries: the smallest container these document types accept.
fn zip_stored(entries: &[(&str, &[u8])]) -> Vec<u8> {
    let mut out: Vec<u8> = Vec::new();
    let mut central: Vec<u8> = Vec::new();
    for (name, data) in entries {
        let name = name.as_bytes();
        let crc = crc32(data);
        let size = data.len() as u32;
        let offset = out.len() as u32;
        out.extend_from_slice(&0x0403_4b50u32.to_le_bytes());
        out.extend_from_slice(&20u16.to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes());
        out.extend_from_slice(&crc.to_le_bytes());
        out.extend_from_slice(&size.to_le_bytes());
        out.extend_from_slice(&size.to_le_bytes());
        out.extend_from_slice(&(name.len() as u16).to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes());
        out.extend_from_slice(name);
        out.extend_from_slice(data);
        central.extend_from_slice(&0x0201_4b50u32.to_le_bytes());
        central.extend_from_slice(&20u16.to_le_bytes());
        central.extend_from_slice(&20u16.to_le_bytes());
        central.extend_from_slice(&0u16.to_le_bytes());
        central.extend_from_slice(&0u16.to_le_bytes());
        central.extend_from_slice(&0u16.to_le_bytes());
        central.extend_from_slice(&0u16.to_le_bytes());
        central.extend_from_slice(&crc.to_le_bytes());
        central.extend_from_slice(&size.to_le_bytes());
        central.extend_from_slice(&size.to_le_bytes());
        central.extend_from_slice(&(name.len() as u16).to_le_bytes());
        central.extend_from_slice(&0u16.to_le_bytes());
        central.extend_from_slice(&0u16.to_le_bytes());
        central.extend_from_slice(&0u16.to_le_bytes());
        central.extend_from_slice(&0u16.to_le_bytes());
        central.extend_from_slice(&0u32.to_le_bytes());
        central.extend_from_slice(&offset.to_le_bytes());
        central.extend_from_slice(name);
    }
    let cd_offset = out.len() as u32;
    let cd_size = central.len() as u32;
    out.extend_from_slice(&central);
    out.extend_from_slice(&0x0605_4b50u32.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes());
    out.extend_from_slice(&(entries.len() as u16).to_le_bytes());
    out.extend_from_slice(&(entries.len() as u16).to_le_bytes());
    out.extend_from_slice(&cd_size.to_le_bytes());
    out.extend_from_slice(&cd_offset.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes());
    out
}

/// So the tests can see the container they wrote is a zip.
#[cfg(test)]
pub(crate) fn is_zip(bytes: &[u8]) -> bool {
    bytes.len() > 4 && bytes[..4] == [0x50, 0x4b, 0x03, 0x04]
}
