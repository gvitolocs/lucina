//! `multipart/form-data` for `POST /v1/share` (SecretaryIOS docs/share.md): text fields, then one file part whose
//! bytes the caller sends between `file_head` and `tail`, so the body length is known before the file is read.

use alloc::string::String;
use alloc::vec::Vec;

pub struct Form {
    pub boundary: String,
    pub head: Vec<u8>,
}

impl Form {
    /// `boundary` must not occur in any field or the file; `lucina-<share id>` as the Android client does (a UUID in
    /// letters and dashes) followed by `--fortuna` keeps it out of an ADPCM stream with overwhelming odds, and the
    /// caller checks the file anyway (`contains`).
    pub fn new(boundary: &str) -> Form {
        Form { boundary: boundary.into(), head: Vec::new() }
    }

    pub fn field(&mut self, name: &str, value: &str) {
        self.part_head(name, None, None);
        self.head.extend_from_slice(value.as_bytes());
        self.head.extend_from_slice(b"\r\n");
    }

    fn part_head(&mut self, name: &str, filename: Option<&str>, mime: Option<&str>) {
        let h = &mut self.head;
        h.extend_from_slice(b"--");
        h.extend_from_slice(self.boundary.as_bytes());
        h.extend_from_slice(b"\r\nContent-Disposition: form-data; name=\"");
        h.extend_from_slice(name.as_bytes());
        h.push(b'"');
        if let Some(f) = filename {
            h.extend_from_slice(b"; filename=\"");
            h.extend_from_slice(f.as_bytes());
            h.push(b'"');
        }
        h.extend_from_slice(b"\r\n");
        if let Some(m) = mime {
            h.extend_from_slice(b"Content-Type: ");
            h.extend_from_slice(m.as_bytes());
            h.extend_from_slice(b"\r\n");
        }
        h.extend_from_slice(b"\r\n");
    }

    /// The last thing in `head`: the file part's header. The file's bytes follow it.
    pub fn file_head(&mut self, name: &str, filename: &str, mime: &str) {
        self.part_head(name, Some(filename), Some(mime));
    }

    pub fn tail(&self) -> Vec<u8> {
        let mut t = Vec::new();
        t.extend_from_slice(b"\r\n--");
        t.extend_from_slice(self.boundary.as_bytes());
        t.extend_from_slice(b"--\r\n");
        t
    }

    pub fn content_type(&self) -> String {
        let mut s = String::from("multipart/form-data; boundary=");
        s.push_str(&self.boundary);
        s
    }

    /// Whether the boundary's line occurs in `data` (then another boundary is needed).
    pub fn collides(&self, data: &[u8]) -> bool {
        let b = self.boundary.as_bytes();
        data.windows(b.len()).any(|w| w == b)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_share_body() {
        let mut f = Form::new("lucina-x--fortuna");
        f.field("intent", "ask");
        f.file_head("file", "note.wav", "audio/wav");
        let mut body = f.head.clone();
        body.extend_from_slice(b"RIFF....");
        body.extend_from_slice(&f.tail());
        let s = String::from_utf8(body).unwrap();
        assert_eq!(
            s,
            "--lucina-x--fortuna\r\nContent-Disposition: form-data; name=\"intent\"\r\n\r\nask\r\n--lucina-x--fortuna\r\nContent-Disposition: form-data; name=\"file\"; filename=\"note.wav\"\r\nContent-Type: audio/wav\r\n\r\nRIFF....\r\n--lucina-x--fortuna--\r\n"
        );
        assert!(f.collides(b"aa lucina-x--fortuna bb"));
        assert!(!f.collides(b"RIFF"));
    }
}
