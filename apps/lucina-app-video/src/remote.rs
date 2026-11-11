//! A video from the owner's library: `POST /v1/media/items/{id}/play` says where it is and how, as for every other Secretary player. The
//! URLs it answers carry their own token, so nothing is signed again while it plays.

use std::io;
use std::sync::Arc;

use lucina_account::Client;
use lucina_video::Demuxer;
use lucina_video::net::{Fetch, Hls, RangeReader};
use secretary_player::subtitles::{self, Cue};
use serde_json::{Value, json};

/// What this player tells the server it is: the server then sends only what this CPU decodes in time (`docs/research/juno-media.md`).
const CLIENT: &str = "juno";
/// A server from before that gets the nearest it knows: MP4 and WebM with H.264, VP9 or AV1 as they are, at any size.
const OLD_SERVER_CLIENT: &str = "web_chrome";

pub struct AccountFetch {
    pub client: Client,
}

impl Fetch for AccountFetch {
    fn range(&self, url: &str, start: u64, len: u64) -> io::Result<(Vec<u8>, u64)> {
        self.client.get_range(url, start, len).map_err(|e| io::Error::other(e.to_string()))
    }

    fn get(&self, url: &str) -> io::Result<Vec<u8>> {
        // A segment is seconds of video at the rate of a file this plays: far under this.
        self.client.get_bytes(url, 64 << 20).map_err(|e| io::Error::other(e.to_string()))
    }
}

/// What `/play` answered, for the player.
pub struct Plan {
    pub title: String,
    /// `direct`, `remux` or `transcode`, for the log.
    pub mode: String,
    pub duration_us: Option<i64>,
    pub resume_ms: u64,
    pub file_url: String,
    pub hls_url: Option<String>,
    pub subtitle_url: Option<String>,
}

pub fn parse_play(v: &Value) -> Result<Plan, String> {
    let file_url = v["file_url"].as_str().or(v["url"].as_str()).ok_or("the server gave no address for this video")?.to_owned();
    let subs = v["subtitles"].as_array();
    // The first text track: choosing another is for a later version of this screen.
    let subtitle_url = subs.and_then(|s| s.iter().find(|t| t["kind"] == "text")).and_then(|t| t["url"].as_str()).map(str::to_owned);
    Ok(Plan {
        title: v["title"].as_str().unwrap_or("Video").to_owned(),
        mode: v["mode"].as_str().unwrap_or("unknown").to_owned(),
        duration_us: v["duration_ms"].as_i64().map(|d| d * 1000),
        resume_ms: v["resume_ms"].as_u64().unwrap_or(0),
        file_url,
        hls_url: v["hls_url"].as_str().map(str::to_owned),
        subtitle_url,
    })
}

/// Asks the server. Blocking: this is before the first frame, with the screen saying so.
pub fn plan(client: &Client, id: &str) -> Result<Plan, String> {
    let path = format!("/v1/media/items/{}/play", enc(id));
    let v = match client.post_json(&path, &json!({"client": CLIENT, "device": "juno"})) {
        // 422 is what the server's JSON extractor answers for a client it has no name for.
        Err(lucina_account::ApiError::Http(422, ..)) => client.post_json(&path, &json!({"client": OLD_SERVER_CLIENT, "device": "juno"})),
        other => other,
    };
    parse_play(&v.map_err(|e| e.to_string())?)
}

/// The id of the shortest of the newest videos in the library, for `--item newest`: a test of the whole path that does not depend on knowing an id.
pub fn newest_video(client: &Client) -> Result<String, String> {
    let v = client.get_json("/v1/media/items?scope=gallery&origin=all&kind=video&limit=50").map_err(|e| e.to_string())?;
    let items = v["items"].as_array().ok_or("the library's answer has no items")?;
    shortest(items).ok_or_else(|| "the library has no videos".to_owned())
}

fn shortest(items: &[Value]) -> Option<String> {
    items.iter().min_by_key(|i| i["duration_ms"].as_u64().unwrap_or(u64::MAX))?["id"].as_str().map(str::to_owned)
}

pub fn demuxer(client: Client, plan: &Plan) -> Result<Box<dyn Demuxer>, String> {
    let fetch = Arc::new(AccountFetch { client });
    if let Some(hls) = &plan.hls_url {
        let d = Hls::open(fetch, hls, plan.duration_us).map_err(|e| e.to_string())?;
        return Ok(Box::new(d));
    }
    let r = RangeReader::open(fetch, &plan.file_url).map_err(|e| e.to_string())?;
    lucina_video::open(r).map_err(|e| e.to_string())
}

/// The sound of a file the server gives by ranges, on connections of its own. HLS packages sound in segments of its own format, which
/// nothing here reads yet.
pub fn sound(client: Client, plan: &Plan) -> Option<crate::sound::Open> {
    if plan.hls_url.is_some() {
        return None;
    }
    let url = plan.file_url.clone();
    Some(Arc::new(move || {
        let fetch = Arc::new(AccountFetch { client: client.clone() });
        let reader = RangeReader::open(fetch, &url).map_err(lucina_audio_decode::DecodeError::Open)?;
        lucina_audio_decode::Decoder::from_reader(reader)
    }))
}

pub fn cues(client: &Client, plan: &Plan) -> Vec<Cue> {
    let Some(url) = &plan.subtitle_url else { return Vec::new() };
    client.get_bytes(url, 8 << 20).ok().map_or(Vec::new(), |b| subtitles::parse(&String::from_utf8_lossy(&b)))
}

/// `PUT /v1/media/progress/{id}` with what the engine said to push. On a thread: the screen does not wait for this.
pub fn push_progress(client: Client, id: String, entry: Value) {
    std::thread::spawn(move || {
        if let Err(e) = client.send_json("PUT", &format!("/v1/media/progress/{}", enc(&id)), &entry) {
            eprintln!("lucina-app-video: the server did not take the position: {e}");
        }
    });
}

fn enc(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || b"-_.~".contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// As `secretary-media-stream`'s `PlayResponse` serializes.
    #[test]
    fn reads_the_servers_answer() {
        let v = json!({
            "ok": true, "id": "itm_1", "kind": "video", "title": "Film", "mime": "video/mp4", "duration_ms": 5000, "width": 1920, "height": 1080,
            "mode": "transcode", "url": "/v1/media/s/t/itm_1/hls/h264-aac/index.m3u8", "file_url": "/v1/media/s/t/itm_1/file",
            "hls_url": "/v1/media/s/t/itm_1/hls/h264-aac/index.m3u8", "poster_url": "/v1/media/s/t/itm_1/poster.jpg",
            "subtitles": [
                {"id": "a", "lang": "it", "label": "Pictures", "forced": false, "kind": "image", "format": "pgs", "source": "embedded", "url": "/s/a.vtt"},
                {"id": "b", "lang": "en", "label": "English", "forced": false, "kind": "text", "format": "srt", "source": "file", "url": "/s/b.vtt"}
            ],
            "bitmap_subtitles": 0, "expires_at": 1, "progress": null, "resume_ms": 1500
        });
        let p = parse_play(&v).unwrap();
        assert_eq!((p.title.as_str(), p.mode.as_str()), ("Film", "transcode"));
        assert_eq!(p.duration_us, Some(5_000_000));
        assert_eq!(p.resume_ms, 1500);
        assert_eq!(p.hls_url.as_deref(), Some("/v1/media/s/t/itm_1/hls/h264-aac/index.m3u8"));
        assert_eq!(p.subtitle_url.as_deref(), Some("/s/b.vtt"));
        assert!(parse_play(&json!({"ok": true})).is_err());
    }

    #[test]
    fn newest_takes_the_shortest_and_an_empty_library_has_none() {
        let items = [json!({"id": "a", "duration_ms": 90000}), json!({"id": "b", "duration_ms": 2000}), json!({"id": "c"})];
        assert_eq!(shortest(&items).as_deref(), Some("b"));
        assert_eq!(shortest(&[]), None);
    }

    #[test]
    fn ids_go_in_a_path_safely() {
        assert_eq!(enc("itm_a-1.b"), "itm_a-1.b");
        assert_eq!(enc("a/b c"), "a%2Fb%20c");
    }
}
