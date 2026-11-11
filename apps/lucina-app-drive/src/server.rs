//! `web/src/api/drive.ts` on a worker thread: one request at a time, so a screen's answers arrive in the order they were asked, and a
//! download on a thread of its own, so a film never holds up a folder.

use std::path::PathBuf;
use std::time::Duration;

use lucina_files::drive::{self, ArchiveListing, Crumb, DriveAlbum, DriveItem, DriveRoots, Filters, FolderEntry, Token};
use lucina_files::media::{Connect, Media, MediaError};
use lucina_files::pictures::Done;

use crate::app::Context;

/// `Next`: what comes after this page.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Next {
    None,
    Offset(u64),
    Cursor(String),
}

pub struct Page {
    pub crumbs: Vec<Crumb>,
    pub folders: Vec<FolderEntry>,
    pub files: Vec<DriveItem>,
    pub total: Option<u64>,
    pub next: Next,
}

pub enum TextAnswer {
    Pending,
    Ready { language: Option<String>, truncated: bool, bytes: Option<u64>, lines: Vec<Vec<Token>> },
    /// 404, or nothing in it: "This file has no text to show."
    Empty,
    Failed,
}

pub enum Job {
    Roots { generation: u64 },
    List { generation: u64, context: Context, filters: Filters, after: Next, more: bool },
    Text { token: u64, url: String },
    /// The server was still reading it: after `after`, ask again.
    TextAgain { token: u64, url: String, after: Duration },
    Archive { token: u64, url: String },
    Download { url: String, dest: PathBuf },
}

pub enum Msg {
    /// A view on a thread of its own changed: the sign-in screen, the picker.
    Changed,
    SignedIn,
    SignedOut,
    Roots { generation: u64, result: Result<DriveRoots, String> },
    Page { generation: u64, more: bool, result: Result<Page, String> },
    Text { token: u64, answer: TextAnswer },
    /// `Err` holds the status, when there was one.
    Archive { token: u64, result: Result<ArchiveListing, Option<u16>> },
    Picture(Done),
    Download(Result<PathBuf, String>),
    DownloadProgress(u64),
}

pub struct Worker {
    media: Box<dyn Media>,
    connect: Connect,
    /// For what outlives one answer: asking again, downloads.
    send: Box<dyn Fn(Msg) + Send + Sync>,
    again: Option<Box<dyn Fn(Job) + Send + Sync>>,
}

/// `driveApi.roots`: an older server has no `/drive`, so the Drive albums.
fn roots(m: &mut dyn Media) -> Result<DriveRoots, MediaError> {
    match m.get("/v1/media/drive") {
        Ok(v) => Ok(drive::parse_roots(&v)),
        Err(MediaError::Refused { status: 404 | 405, .. }) => {
            let v = m.get("/v1/media/albums?scope=drive")?;
            let albums: Vec<DriveAlbum> = drive::parse_albums(&v["albums"]).into_iter().filter(|a| a.count != Some(0)).collect();
            Ok(DriveRoots { folders: Vec::new(), albums })
        }
        Err(e) => Err(e),
    }
}

/// `fetchPage`.
fn page(m: &mut dyn Media, context: &Context, filters: &Filters, after: &Next) -> Result<Page, MediaError> {
    if let Context::Folder { folder, path } = context {
        let offset = if let Next::Offset(o) = after { *o } else { 0 };
        let p = drive::parse_browse(&m.get(&drive::browse_url(folder, path, filters, offset))?, folder, path);
        return Ok(Page {
            crumbs: p.breadcrumbs,
            folders: p.folders,
            files: p.files,
            total: p.total,
            next: p.next_offset.map_or(Next::None, Next::Offset),
        });
    }
    let album = if let Context::Album { id, .. } = context { Some(id.as_str()) } else { None };
    let cursor = if let Next::Cursor(c) = after { Some(c.as_str()) } else { None };
    let p = drive::parse_items_page(&m.get(&drive::items_url(&filters.q, filters.kind, album, cursor))?);
    Ok(Page {
        crumbs: Vec::new(),
        folders: Vec::new(),
        files: p.items,
        total: p.total,
        next: p.next_cursor.map_or(Next::None, Next::Cursor),
    })
}

fn text_answer(r: Result<serde_json::Value, MediaError>) -> Result<TextAnswer, MediaError> {
    match r {
        Ok(v) => Ok(match drive::parse_text_preview(&v) {
            drive::TextPreview::Pending => TextAnswer::Pending,
            drive::TextPreview::Ready { language, truncated, bytes, text, lines } => {
                let had = lines.as_ref().is_some_and(|l| !l.is_empty());
                if text.is_empty() && !had {
                    TextAnswer::Empty
                } else {
                    TextAnswer::Ready { language, truncated, bytes, lines: drive::preview_lines(&text, lines) }
                }
            }
        }),
        Err(MediaError::Refused { status: 404, .. }) => Ok(TextAnswer::Empty),
        Err(MediaError::SignedOut) => Err(MediaError::SignedOut),
        Err(_) => Ok(TextAnswer::Failed),
    }
}

impl Worker {
    pub fn new(media: Box<dyn Media>, connect: Connect, send: Box<dyn Fn(Msg) + Send + Sync>) -> Worker {
        Worker { media, connect, send, again: None }
    }

    pub fn with_again(mut self, again: Box<dyn Fn(Job) + Send + Sync>) -> Worker {
        self.again = Some(again);
        self
    }

    pub fn handle(&mut self, job: Job) -> Vec<Msg> {
        let m = &mut *self.media;
        match job {
            Job::Roots { generation } => match roots(m) {
                Err(MediaError::SignedOut) => vec![Msg::SignedOut],
                r => vec![Msg::Roots { generation, result: r.map_err(|e| e.text()) }],
            },
            Job::List { generation, context, filters, after, more } => match page(m, &context, &filters, &after) {
                Err(MediaError::SignedOut) => vec![Msg::SignedOut],
                r => vec![Msg::Page { generation, more, result: r.map_err(|e| e.text()) }],
            },
            Job::Text { token, url } => {
                let sep = if url.contains('?') { '&' } else { '?' };
                match text_answer(m.get(&format!("{url}{sep}highlight=1"))) {
                    Ok(answer) => vec![Msg::Text { token, answer }],
                    Err(_) => vec![Msg::SignedOut],
                }
            }
            Job::TextAgain { token, url, after } => {
                if let Some(again) = self.again.take() {
                    let again: std::sync::Arc<dyn Fn(Job) + Send + Sync> = std::sync::Arc::from(again);
                    let a = again.clone();
                    std::thread::spawn(move || {
                        std::thread::sleep(after);
                        a(Job::Text { token, url });
                    });
                    self.again = Some(Box::new(move |j| again(j)));
                    Vec::new()
                } else {
                    // The tests, with no thread: a moment on this one.
                    std::thread::sleep(after.min(Duration::from_millis(100)));
                    self.handle(Job::Text { token, url })
                }
            }
            Job::Archive { token, url } => match m.get(&url) {
                Ok(v) => vec![Msg::Archive { token, result: Ok(drive::parse_archive(&v)) }],
                Err(MediaError::SignedOut) => vec![Msg::SignedOut],
                Err(e) => vec![Msg::Archive { token, result: Err(e.status()) }],
            },
            Job::Download { url, dest } => {
                let connect = self.connect.clone();
                let send = std::sync::Arc::new(std::mem::replace(&mut self.send, Box::new(|_| {})));
                let s2 = send.clone();
                std::thread::spawn(move || {
                    let r = download(&connect, &url, &dest, &*s2);
                    s2(Msg::Download(r.map(|()| dest)));
                });
                self.send = Box::new(move |m| send(m));
                Vec::new()
            }
        }
    }
}

/// Whole or not at all: the file appears under its name when every byte is there.
pub fn download(connect: &Connect, url: &str, dest: &std::path::Path, send: &(dyn Fn(Msg) + Send + Sync)) -> Result<(), String> {
    let mut m = connect().ok_or("not signed in")?;
    let tmp = dest.with_extension("part");
    let mut f = std::fs::File::create(&tmp).map_err(|e| format!("{}: {e}", tmp.display()))?;
    let mut last = 0u64;
    let r = m.download(url, &mut f, &mut |bytes| {
        if bytes - last >= 8 << 20 {
            last = bytes;
            send(Msg::DownloadProgress(bytes));
        }
        true
    });
    drop(f);
    match r {
        Ok(_) => std::fs::rename(&tmp, dest).map_err(|e| format!("{}: {e}", dest.display())),
        Err(e) => {
            let _ = std::fs::remove_file(&tmp);
            Err(e.text())
        }
    }
}
