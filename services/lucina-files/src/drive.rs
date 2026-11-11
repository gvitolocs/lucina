//! The Drive's model: `web/src/lib/drive.ts`, parsed as defensively, since any field may be missing on an older server.

use lucina_app::time::Zone;
use lucina_app_support::dates;
use lucina_app_support::net::enc;
use serde_json::Value;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum DriveType {
    Pdf,
    Document,
    Spreadsheet,
    Presentation,
    Text,
    Code,
    Archive,
    Image,
    Video,
    Audio,
    Other,
}

pub const DRIVE_TYPES: [DriveType; 11] = [
    DriveType::Pdf,
    DriveType::Document,
    DriveType::Spreadsheet,
    DriveType::Presentation,
    DriveType::Text,
    DriveType::Code,
    DriveType::Archive,
    DriveType::Image,
    DriveType::Video,
    DriveType::Audio,
    DriveType::Other,
];

impl DriveType {
    pub fn parse(s: &str) -> Option<DriveType> {
        DRIVE_TYPES.into_iter().find(|t| t.name() == s)
    }

    /// As the server spells it.
    pub fn name(self) -> &'static str {
        match self {
            DriveType::Pdf => "pdf",
            DriveType::Document => "document",
            DriveType::Spreadsheet => "spreadsheet",
            DriveType::Presentation => "presentation",
            DriveType::Text => "text",
            DriveType::Code => "code",
            DriveType::Archive => "archive",
            DriveType::Image => "image",
            DriveType::Video => "video",
            DriveType::Audio => "audio",
            DriveType::Other => "other",
        }
    }

    /// `typeLabelKey`, in English.
    pub fn label(self) -> &'static str {
        match self {
            DriveType::Pdf => "PDF",
            DriveType::Document => "Document",
            DriveType::Spreadsheet => "Spreadsheet",
            DriveType::Presentation => "Slides",
            DriveType::Text => "Text",
            DriveType::Code => "Code",
            DriveType::Archive => "Archive",
            DriveType::Image => "Image",
            DriveType::Video => "Video",
            DriveType::Audio => "Audio",
            DriveType::Other => "File",
        }
    }

    /// `filterLabelKey`, in English.
    pub fn filter_label(self) -> &'static str {
        match self {
            DriveType::Pdf => "PDF",
            DriveType::Document => "Documents",
            DriveType::Spreadsheet => "Spreadsheets",
            DriveType::Presentation => "Slides",
            DriveType::Text => "Text",
            DriveType::Code => "Code",
            DriveType::Archive => "Archives",
            DriveType::Image => "Images",
            DriveType::Video => "Video",
            DriveType::Audio => "Audio",
            DriveType::Other => "Other",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DrivePreview {
    Pdf,
    Office,
    Text,
    Code,
    Archive,
    Image,
    Video,
    Audio,
    None,
}

impl DrivePreview {
    fn parse(s: &str) -> Option<DrivePreview> {
        Some(match s {
            "pdf" => DrivePreview::Pdf,
            "office" => DrivePreview::Office,
            "text" => DrivePreview::Text,
            "code" => DrivePreview::Code,
            "archive" => DrivePreview::Archive,
            "image" => DrivePreview::Image,
            "video" => DrivePreview::Video,
            "audio" => DrivePreview::Audio,
            "none" => DrivePreview::None,
            _ => return None,
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DriveSort {
    Name,
    Modified,
    Size,
    Type,
}

pub const DRIVE_SORTS: [DriveSort; 4] = [DriveSort::Name, DriveSort::Modified, DriveSort::Size, DriveSort::Type];

impl DriveSort {
    pub fn name(self) -> &'static str {
        match self {
            DriveSort::Name => "name",
            DriveSort::Modified => "modified",
            DriveSort::Size => "size",
            DriveSort::Type => "type",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            DriveSort::Name => "Name",
            DriveSort::Modified => "Modified",
            DriveSort::Size => "Size",
            DriveSort::Type => "Type",
        }
    }

    /// `defaultOrder`: ascending for name and type, descending for modified and size. True is ascending.
    pub fn default_ascending(self) -> bool {
        matches!(self, DriveSort::Name | DriveSort::Type)
    }
}

/// `sameOriginPath`: a path on this server, never another's.
pub fn same_origin(v: &Value) -> Option<String> {
    let s = v.as_str()?;
    (s.starts_with('/') && !s.starts_with("//")).then(|| s.to_owned())
}

fn text(v: &Value, max: usize) -> Option<String> {
    let s = v.as_str()?;
    if s.is_empty() {
        return None;
    }
    Some(s.chars().take(max).collect())
}

/// `count`.
fn count(v: &Value) -> Option<u64> {
    v.as_u64().or_else(|| v.as_f64().filter(|f| f.is_finite() && *f >= 0.0).map(|f| f as u64))
}

/// `cleanPath`: a Drive path within a folder, `/`-separated, with no leading `/` and no `.`, `..` or empty segments.
pub fn clean_path(s: &str) -> String {
    s.split('/').filter(|p| !p.is_empty() && *p != "." && *p != ".." && !p.contains('\\') && !p.contains('\0')).collect::<Vec<_>>().join("/")
}

/// `extOf`.
pub fn ext_of(name: &str) -> String {
    let base = name.rsplit('/').next().unwrap_or("");
    match base.rfind('.') {
        Some(dot) if dot > 0 => base[dot + 1..].to_lowercase(),
        _ => String::new(),
    }
}

/// `guessType`: the server's type is missing, so from kind, MIME and extension.
pub fn guess_type(kind: &str, mime: &str, ext: &str) -> DriveType {
    if kind == "photo" || mime.starts_with("image/") {
        return DriveType::Image;
    }
    if kind == "video" || mime.starts_with("video/") {
        return DriveType::Video;
    }
    if kind == "audio" || mime.starts_with("audio/") {
        return DriveType::Audio;
    }
    if mime == "application/pdf" {
        return DriveType::Pdf;
    }
    match ext {
        "pdf" => DriveType::Pdf,
        "doc" | "docx" | "odt" | "rtf" | "pages" => DriveType::Document,
        "xls" | "xlsx" | "ods" | "csv" | "tsv" | "numbers" => DriveType::Spreadsheet,
        "ppt" | "pptx" | "odp" | "key" => DriveType::Presentation,
        "txt" | "md" | "log" | "rst" | "tex" => DriveType::Text,
        "rs" | "py" | "js" | "ts" | "tsx" | "jsx" | "c" | "h" | "cpp" | "hpp" | "java" | "kt" | "swift" | "go" | "rb" | "sh" | "json" | "toml"
        | "yaml" | "yml" | "xml" | "html" | "css" | "sql" | "m" | "r" | "ipynb" => DriveType::Code,
        "zip" | "tar" | "gz" | "tgz" | "7z" | "rar" | "bz2" | "xz" => DriveType::Archive,
        _ if mime.starts_with("text/") => DriveType::Text,
        _ => DriveType::Other,
    }
}

/// `previewForType`.
pub fn preview_for_type(t: DriveType) -> DrivePreview {
    match t {
        DriveType::Pdf => DrivePreview::Pdf,
        DriveType::Text => DrivePreview::Text,
        DriveType::Code => DrivePreview::Code,
        DriveType::Archive => DrivePreview::Archive,
        DriveType::Image => DrivePreview::Image,
        DriveType::Video => DrivePreview::Video,
        DriveType::Audio => DrivePreview::Audio,
        DriveType::Document | DriveType::Spreadsheet | DriveType::Presentation => DrivePreview::Office,
        DriveType::Other => DrivePreview::None,
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DriveInfo {
    pub folder: String,
    pub path: String,
    pub dir: String,
    pub name: String,
    pub ext: String,
    pub kind: DriveType,
    pub preview: DrivePreview,
    pub language: Option<String>,
    pub modified: Option<String>,
}

fn parse_info(d: &Value, title: &str) -> Option<DriveInfo> {
    let folder = text(&d["folder"], 512)?;
    let path = clean_path(d["path"].as_str().unwrap_or(""));
    let name = text(&d["name"], 1024)
        .or_else(|| path.rsplit('/').next().filter(|s| !s.is_empty()).map(str::to_owned))
        .unwrap_or_else(|| title.to_owned());
    let ext = d["ext"].as_str().map_or_else(|| ext_of(&name), |e| e.to_lowercase().chars().take(32).collect());
    let kind = d["type"].as_str().and_then(DriveType::parse).unwrap_or_else(|| guess_type("file", "", &ext));
    let dir = match d["dir"].as_str() {
        Some(dir) => clean_path(dir),
        None => path.rfind('/').map_or(String::new(), |i| path[..i].to_owned()),
    };
    Some(DriveInfo {
        folder,
        path,
        dir,
        name,
        ext,
        kind,
        preview: d["preview"].as_str().and_then(DrivePreview::parse).unwrap_or_else(|| preview_for_type(kind)),
        language: text(&d["language"], 64),
        modified: text(&d["modified"], 64),
    })
}

/// An item from `/items`, `/items/{id}` or `/drive/browse`. Only what Juno's screens read: a folder of thousands stays small.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DriveItem {
    pub id: String,
    pub kind: String,
    pub title: String,
    pub mime: String,
    pub size: Option<u64>,
    pub created_at: Option<String>,
    pub taken_at: Option<String>,
    pub thumb_state: Option<String>,
    pub duration_ms: Option<u64>,
    pub urls: DriveUrls,
    pub thumb_url: Option<String>,
    pub poster_url: Option<String>,
    pub album: Option<String>,
    pub drive: Option<DriveInfo>,
    pub snippet: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DriveUrls {
    pub original: Option<String>,
    pub download: Option<String>,
    pub thumb: Option<String>,
    pub large: Option<String>,
    pub poster: Option<String>,
    pub pdf: Option<String>,
    pub text: Option<String>,
    pub archive: Option<String>,
}

/// `parseDriveItem`: `None` when it has no usable id.
pub fn parse_item(b: &Value) -> Option<DriveItem> {
    let id = text(&b["id"], 128)?;
    let u = &b["urls"];
    let title = text(&b["title"], 1024).unwrap_or_else(|| id.clone());
    let drive = parse_info(&b["drive"], &title);
    Some(DriveItem {
        kind: text(&b["kind"], 32).unwrap_or_else(|| "file".into()),
        mime: text(&b["mime"], 255).unwrap_or_default(),
        size: count(&b["size"]),
        created_at: text(&b["created_at"], 64),
        taken_at: text(&b["taken_at"], 64),
        thumb_state: text(&b["thumb_state"], 32),
        duration_ms: count(&b["duration_ms"]),
        urls: DriveUrls {
            original: same_origin(&u["original"]),
            download: same_origin(&u["download"]),
            thumb: same_origin(&u["thumb"]),
            large: same_origin(&u["large"]),
            poster: same_origin(&u["poster"]),
            pdf: same_origin(&u["pdf"]),
            text: same_origin(&u["text"]),
            archive: same_origin(&u["archive"]),
        },
        thumb_url: same_origin(&b["thumb_url"]),
        poster_url: same_origin(&b["poster_url"]),
        album: text(&b["album"], 512),
        snippet: text(&b["snippet"], 2000),
        drive,
        title,
        id,
    })
}

impl DriveItem {
    /// `itemType`.
    pub fn file_type(&self) -> DriveType {
        self.drive.as_ref().map_or_else(|| guess_type(&self.kind, &self.mime, &ext_of(&self.title)), |d| d.kind)
    }

    /// `itemPreview`.
    pub fn preview(&self) -> DrivePreview {
        self.drive.as_ref().map_or_else(|| preview_for_type(self.file_type()), |d| d.preview)
    }

    /// `itemName`.
    pub fn name(&self) -> &str {
        self.drive.as_ref().map_or(&self.title, |d| &d.name)
    }

    /// `itemModified`.
    pub fn modified(&self) -> Option<&str> {
        self.drive.as_ref().and_then(|d| d.modified.as_deref()).or(self.taken_at.as_deref()).or(self.created_at.as_deref())
    }

    /// `itemLocation`: "Lezioni / Digital Forensics", where a file lives, for search results.
    pub fn location(&self) -> String {
        match &self.drive {
            None => self.album.clone().unwrap_or_default(),
            Some(d) => {
                let mut parts = vec![d.folder.as_str()];
                parts.extend(d.dir.split('/').filter(|s| !s.is_empty()));
                parts.join(" / ")
            }
        }
    }

    /// `thumbnailUrl` with no JPEG XL: the tokenised JPEG first, then the JPEG thumb.
    pub fn thumbnail(&self) -> Option<&str> {
        if self.thumb_state.as_deref().is_some_and(|s| s != "ready") {
            return None;
        }
        self.thumb_url.as_deref().or(self.urls.thumb.as_deref())
    }

    /// The picture for a preview. The web shows the original, tokenised; a 1600 px rendition fills this screen in a fraction of the memory.
    pub fn picture(&self) -> Option<&str> {
        self.urls.large.as_deref().or(self.urls.original.as_deref())
    }

    /// `downloadUrl`.
    pub fn download(&self) -> String {
        self.urls.download.clone().unwrap_or_else(|| format!("/v1/media/items/{}/original?download=1", enc(&self.id)))
    }

    /// `fileMeta` in the rows: "PDF · 1.2 MB · 4 Mar 2026".
    pub fn meta(&self, zone: &Zone) -> String {
        join_dots(&[self.file_type().label().to_owned(), format_bytes(self.size), format_modified(self.modified(), zone)])
    }
}

/// `typeBadge`: the short badge on a file's icon.
pub fn type_badge(t: DriveType, ext: &str) -> String {
    let short = || (!ext.is_empty() && ext.len() <= 4).then(|| ext.to_uppercase());
    match t {
        DriveType::Pdf => "PDF".into(),
        DriveType::Document => "DOC".into(),
        DriveType::Spreadsheet => "XLS".into(),
        DriveType::Presentation => "PPT".into(),
        DriveType::Text => if ext == "md" { "MD" } else { "TXT" }.into(),
        DriveType::Code => short().unwrap_or_else(|| "</>".into()),
        DriveType::Archive => "ZIP".into(),
        DriveType::Image => "IMG".into(),
        DriveType::Video => "VID".into(),
        DriveType::Audio => "AUD".into(),
        DriveType::Other => short().unwrap_or_default(),
    }
}

/// `typeBadge`'s colour family, as `0xRRGGBB`: red, blue, green, orange, gray, purple, yellow, teal, pink, indigo.
pub fn type_tone(t: DriveType) -> u32 {
    match t {
        DriveType::Pdf => 0xd9_5c_5c,
        DriveType::Document => 0x5c_8f_d9,
        DriveType::Spreadsheet => 0x5c_b0_7a,
        DriveType::Presentation => 0xd9_8f_4e,
        DriveType::Text | DriveType::Other => 0x8c_93_a8,
        DriveType::Code => 0xa2_7c_d9,
        DriveType::Archive => 0xc9_a2_4a,
        DriveType::Image => 0x4e_b3_b3,
        DriveType::Video => 0xd9_6c_a8,
        DriveType::Audio => 0x7c_82_d9,
    }
}

/// `opensAsSheet`: the workbooks the web's sheet editor takes.
pub fn is_workbook(name: &str) -> bool {
    matches!(ext_of(name).as_str(), "xlsx" | "xlsm" | "xltx" | "xltm")
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DriveRoot {
    pub folder: String,
    pub files: Option<u64>,
    pub bytes: Option<u64>,
    pub modified: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DriveAlbum {
    pub id: String,
    pub title: String,
    pub count: Option<u64>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DriveRoots {
    pub folders: Vec<DriveRoot>,
    pub albums: Vec<DriveAlbum>,
}

/// `parseAlbums`.
pub fn parse_albums(v: &Value) -> Vec<DriveAlbum> {
    v.as_array()
        .into_iter()
        .flatten()
        .filter_map(|a| {
            let id = text(&a["id"], 128)?;
            Some(DriveAlbum { title: text(&a["title"], 512).unwrap_or_else(|| id.clone()), count: count(&a["count"]), id })
        })
        .collect()
}

/// `parseRoots`.
pub fn parse_roots(b: &Value) -> DriveRoots {
    let folders = b["folders"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|f| {
            let folder = text(&f["folder"], 512).filter(|n| !n.contains('/'))?;
            Some(DriveRoot { folder, files: count(&f["files"]), bytes: count(&f["bytes"]), modified: text(&f["modified"], 64) })
        })
        .collect();
    DriveRoots { folders, albums: parse_albums(&b["albums"]) }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Crumb {
    pub name: String,
    pub path: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FolderEntry {
    pub name: String,
    pub path: String,
    pub files: Option<u64>,
    pub bytes: Option<u64>,
    pub modified: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BrowsePage {
    pub folder: String,
    pub path: String,
    pub breadcrumbs: Vec<Crumb>,
    pub folders: Vec<FolderEntry>,
    pub files: Vec<DriveItem>,
    pub total: Option<u64>,
    pub next_offset: Option<u64>,
}

/// `breadcrumbs`: from the folder root down to `path`, when the server sends none.
pub fn breadcrumbs(folder: &str, path: &str) -> Vec<Crumb> {
    let mut crumbs = vec![Crumb { name: folder.to_owned(), path: String::new() }];
    let clean = clean_path(path);
    let parts: Vec<&str> = clean.split('/').filter(|s| !s.is_empty()).collect();
    for (i, name) in parts.iter().enumerate() {
        crumbs.push(Crumb { name: (*name).to_owned(), path: parts[..=i].join("/") });
    }
    crumbs
}

/// `parseBrowse`.
pub fn parse_browse(b: &Value, folder: &str, path: &str) -> BrowsePage {
    let real_folder = text(&b["folder"], 512).unwrap_or_else(|| folder.to_owned());
    let real_path = b["path"].as_str().map_or_else(|| path.to_owned(), clean_path);
    let mut crumbs: Vec<Crumb> = b["breadcrumbs"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|c| Some(Crumb { name: text(&c["name"], 1024)?, path: clean_path(c["path"].as_str().unwrap_or("")) }))
        .collect();
    if crumbs.is_empty() {
        crumbs = breadcrumbs(&real_folder, &real_path);
    }
    let folders = b["folders"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|f| {
            let p = clean_path(f["path"].as_str().unwrap_or(""));
            let name = text(&f["name"], 1024).or_else(|| p.rsplit('/').next().map(str::to_owned)).unwrap_or_default();
            if p.is_empty() || name.is_empty() {
                return None;
            }
            Some(FolderEntry { name, path: p, files: count(&f["files"]), bytes: count(&f["bytes"]), modified: text(&f["modified"], 64) })
        })
        .collect();
    BrowsePage {
        folder: real_folder,
        path: real_path,
        breadcrumbs: crumbs,
        folders,
        files: parse_items(&b["files"]),
        total: count(&b["total"]),
        next_offset: count(&b["next_offset"]),
    }
}

/// `parseItems`.
pub fn parse_items(v: &Value) -> Vec<DriveItem> {
    v.as_array().into_iter().flatten().filter_map(parse_item).collect()
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ItemsPage {
    pub items: Vec<DriveItem>,
    pub next_cursor: Option<String>,
    pub total: Option<u64>,
}

/// `parseItemsPage`.
pub fn parse_items_page(b: &Value) -> ItemsPage {
    ItemsPage { items: parse_items(&b["items"]), next_cursor: text(&b["next_cursor"], 512), total: count(&b["total"]) }
}

/// A token of a highlighted line: its class and its text.
pub type Token = (String, String);

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TextPreview {
    /// 202: the server is still reading the file.
    Pending,
    Ready { language: Option<String>, truncated: bool, bytes: Option<u64>, text: String, lines: Option<Vec<Vec<Token>>> },
}

/// `parseTextPreview`.
pub fn parse_text_preview(b: &Value) -> TextPreview {
    if b["state"] == "pending" {
        return TextPreview::Pending;
    }
    let lines = b["lines"].as_array().map(|ls| {
        ls.iter()
            .map(|line| {
                line.as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|tok| match tok {
                        Value::String(s) => Some((String::new(), s.clone())),
                        Value::Array(pair) => Some((pair.first().and_then(Value::as_str).unwrap_or("").to_owned(), pair.get(1)?.as_str()?.to_owned())),
                        _ => None,
                    })
                    .collect()
            })
            .collect()
    });
    TextPreview::Ready {
        language: text(&b["language"], 64),
        truncated: b["truncated"] == true,
        bytes: count(&b["bytes"]),
        text: b["text"].as_str().unwrap_or("").to_owned(),
        lines,
    }
}

/// `previewLines`: the highlighted lines, then the rest of the text plain (highlighting stops at 5,000 lines).
pub fn preview_lines(text: &str, lines: Option<Vec<Vec<Token>>>) -> Vec<Vec<Token>> {
    let mut plain: Vec<&str> = text.split('\n').map(|l| l.strip_suffix('\r').unwrap_or(l)).collect();
    if plain.len() > 1 && plain.last() == Some(&"") {
        plain.pop();
    }
    let mut out = lines.unwrap_or_default();
    for line in plain.iter().skip(out.len()) {
        out.push(vec![(String::new(), (*line).to_owned())]);
    }
    out
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ArchiveEntry {
    pub path: String,
    pub size: Option<u64>,
    pub dir: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ArchiveListing {
    pub format: Option<String>,
    pub total: Option<u64>,
    pub truncated: bool,
    pub entries: Vec<ArchiveEntry>,
}

/// `parseArchive`.
pub fn parse_archive(b: &Value) -> ArchiveListing {
    let entries = b["entries"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|e| Some(ArchiveEntry { path: text(&e["path"], 4096)?, size: count(&e["size"]), dir: e["dir"] == true }))
        .collect();
    ArchiveListing { format: text(&b["format"], 32), total: count(&b["total"]), truncated: b["truncated"] == true, entries }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Filters {
    pub q: String,
    pub kind: Option<DriveType>,
    pub sort: Option<DriveSort>,
    pub ascending: Option<bool>,
}

impl Filters {
    pub const NONE: Filters = Filters { q: String::new(), kind: None, sort: None, ascending: None };
}

pub const BROWSE_LIMIT: u32 = 200;

/// `browseUrl`: a folder, and its subtree when searching.
pub fn browse_url(folder: &str, path: &str, f: &Filters, offset: u64) -> String {
    let mut q = format!("/v1/media/drive/browse?folder={}", enc(folder));
    let p = clean_path(path);
    if !p.is_empty() {
        q.push_str(&format!("&path={}", enc(&p)));
    }
    if let Some(sort) = f.sort {
        q.push_str(&format!("&sort={}", sort.name()));
        if let Some(asc) = f.ascending {
            q.push_str(if asc { "&order=asc" } else { "&order=desc" });
        }
    }
    if let Some(kind) = f.kind {
        q.push_str(&format!("&type={}", kind.name()));
    }
    let words = f.q.trim();
    if !words.is_empty() {
        q.push_str(&format!("&q={}", enc(words)));
    }
    if offset > 0 {
        q.push_str(&format!("&offset={offset}"));
    }
    q.push_str(&format!("&limit={BROWSE_LIMIT}"));
    q
}

/// `itemsUrl`: search everywhere, or an album's items.
pub fn items_url(q: &str, kind: Option<DriveType>, album: Option<&str>, cursor: Option<&str>) -> String {
    let mut u = String::from("/v1/media/items?scope=drive");
    if let Some(a) = album {
        u.push_str(&format!("&album={}", enc(a)));
    }
    let words = q.trim();
    if !words.is_empty() {
        u.push_str(&format!("&q={}", enc(words)));
    }
    if let Some(k) = kind {
        u.push_str(&format!("&type={}", k.name()));
    }
    if let Some(c) = cursor {
        u.push_str(&format!("&cursor={}", enc(c)));
    }
    u.push_str(&format!("&limit={BROWSE_LIMIT}"));
    u
}

/// `formatBytes`: decimal units.
pub fn format_bytes(n: Option<u64>) -> String {
    let Some(n) = n else { return String::new() };
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut v = n as f64;
    let mut u = 0;
    while v >= 1000.0 && u < UNITS.len() - 1 {
        v /= 1000.0;
        u += 1;
    }
    let shown = if u == 0 || v >= 100.0 { format!("{v:.0}") } else { format!("{:.1}", v) };
    format!("{} {}", shown.strip_suffix(".0").unwrap_or(&shown), UNITS[u])
}

const MONTHS: [&str; 12] = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];

/// Days since 1970 to a civil date (Howard Hinnant's `civil_from_days`).
pub fn civil(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (yoe + era * 400 + i64::from(m <= 2), m, d)
}

/// The local date of an ISO time.
pub fn local_date(iso: &str, zone: &Zone) -> Option<(i64, u32, u32)> {
    let ms = dates::parse_iso(iso)?;
    let (offset, _) = zone.at(ms.div_euclid(1000));
    let local = ms.div_euclid(1000) + offset;
    Some(civil(local.div_euclid(86_400)))
}

/// `formatModified`: "4 Mar 2026", or nothing when it is missing or invalid.
pub fn format_modified(iso: Option<&str>, zone: &Zone) -> String {
    let Some((y, m, d)) = iso.and_then(|s| local_date(s, zone)) else { return String::new() };
    format!("{d} {} {y}", MONTHS[m as usize - 1])
}

/// `[…].filter(Boolean).join(" · ")`.
pub fn join_dots(parts: &[String]) -> String {
    parts.iter().filter(|s| !s.is_empty()).cloned().collect::<Vec<_>>().join(" · ")
}

/// `folderDetail`.
pub fn folder_detail(files: Option<u64>, bytes: Option<u64>, modified: Option<&str>, zone: &Zone) -> String {
    let files = files.map_or(String::new(), |n| if n == 1 { "1 file".to_owned() } else { format!("{n} files") });
    join_dots(&[files, format_bytes(bytes), format_modified(modified, zone)])
}
