//! `web/src/api/gallery.ts` and the parts of `web/src/lib/galleryLayout.ts` and `galleryInfo.ts` this app uses. A listing keeps only what the
//! grid draws and the dialogs ask: the rest of an item comes from `/details` when the info panel opens, so a long scroll stays small.

use lucina_app::time::Zone;
use lucina_app_support::dates;
use lucina_app_support::net::enc;
use lucina_files::drive::{civil, join_dots, same_origin};
use serde_json::Value;

pub const MONTHS: [&str; 12] = ["January", "February", "March", "April", "May", "June", "July", "August", "September", "October", "November", "December"];
const WEEKDAYS: [&str; 7] = ["Thursday", "Friday", "Saturday", "Sunday", "Monday", "Tuesday", "Wednesday"];

fn text(v: &Value) -> Option<String> {
    v.as_str().filter(|s| !s.is_empty()).map(str::to_owned)
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Item {
    pub id: String,
    pub video: bool,
    pub title: String,
    pub caption: Option<String>,
    pub mime: String,
    pub size: u64,
    /// Taken, else added, as milliseconds: what the grid sorts into days.
    pub at_ms: Option<i64>,
    pub taken_at: Option<String>,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub duration_ms: Option<u64>,
    pub source: String,
    pub device: Option<String>,
    pub device_name: Option<String>,
    pub favorite: bool,
    pub rotation: u16,
    pub albums: Vec<String>,
    pub trash_expires_at: Option<String>,
    pub thumbhash: Option<String>,
    pub thumb: Option<String>,
    pub large: Option<String>,
    pub poster: Option<String>,
    pub original: Option<String>,
    pub download: Option<String>,
}

/// `parseItem`: `None` with no id, and for anything but a photo or a video.
pub fn parse_item(r: &Value) -> Option<Item> {
    let id = text(&r["id"])?;
    let video = match r["kind"].as_str()? {
        "photo" => false,
        "video" => true,
        _ => return None,
    };
    let taken_at = text(&r["taken_at"]);
    let added = text(&r["created_at"]).or_else(|| text(&r["added_at"]));
    let at_ms = taken_at.as_deref().or(added.as_deref()).and_then(dates::parse_iso);
    let u = &r["urls"];
    let rotation = r["rotation"].as_u64().filter(|x| matches!(x, 0 | 90 | 180 | 270)).unwrap_or(0) as u16;
    Some(Item {
        video,
        title: text(&r["title"]).unwrap_or_default(),
        caption: text(&r["caption"]),
        mime: text(&r["mime"]).unwrap_or_default(),
        size: r["size"].as_u64().unwrap_or(0),
        at_ms,
        taken_at,
        width: r["width"].as_u64().map(|x| x as u32),
        height: r["height"].as_u64().map(|x| x as u32),
        duration_ms: r["duration_ms"].as_u64(),
        source: text(&r["source"]).unwrap_or_default(),
        device: text(&r["device"]),
        device_name: text(&r["device_name"]),
        favorite: r["favorite"] == true,
        rotation,
        albums: r["albums"].as_array().into_iter().flatten().filter_map(|a| a.as_str().map(str::to_owned)).collect(),
        trash_expires_at: text(&r["trash_expires_at"]),
        thumbhash: text(&r["thumbhash"]),
        thumb: same_origin(&u["thumb"]),
        large: same_origin(&u["large"]),
        poster: same_origin(&u["poster"]),
        original: same_origin(&u["original"]),
        download: same_origin(&u["download"]),
        id,
    })
}

impl Item {
    /// `tileUrl` with no JPEG XL.
    pub fn tile_url(&self) -> Option<&str> {
        self.thumb.as_deref()
    }

    /// `previewUrl` with no JPEG XL: the 1600 px JPEG. A video's is its poster.
    pub fn preview_url(&self) -> Option<&str> {
        if self.video {
            return self.poster.as_deref().or(self.large.as_deref()).or(self.thumb.as_deref());
        }
        self.large.as_deref().or(self.thumb.as_deref())
    }

    /// The original, when this machine can decode it: JPEG, PNG and WebP, within what a picture may cost here. HEIC is not.
    pub fn decodable_original(&self) -> Option<&str> {
        let ok = matches!(self.mime.as_str(), "image/jpeg" | "image/png" | "image/webp") && self.size <= 30 << 20;
        self.original.as_deref().filter(|_| ok && !self.video)
    }

    pub fn download_url(&self) -> String {
        self.download.clone().unwrap_or_else(|| format!("/v1/media/items/{}/original?download=1", enc(&self.id)))
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct Query {
    pub origin: Option<String>,
    /// `None` is photos and videos.
    pub kind: Option<&'static str>,
    pub q: String,
    pub favorite: bool,
    pub trashed: bool,
    pub album: Option<String>,
    pub smart: bool,
}

/// `galleryQueryString`.
pub fn query_string(q: &Query, cursor: Option<&str>, limit: u32) -> String {
    let mut s = format!("scope=gallery&origin={}&kind={}", enc(q.origin.as_deref().unwrap_or("all")), q.kind.unwrap_or("photo%2Cvideo"));
    let words = q.q.trim();
    if !words.is_empty() {
        s.push_str(&format!("&q={}", enc(words)));
    }
    if q.favorite {
        s.push_str("&favorite=true");
    }
    if q.trashed {
        s.push_str("&trashed=true");
    }
    if let Some(a) = &q.album {
        s.push_str(&format!("&album={}", enc(a)));
    }
    if let Some(c) = cursor {
        s.push_str(&format!("&cursor={}", enc(c)));
    }
    s.push_str(&format!("&limit={}", limit.clamp(1, 500)));
    s
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Source {
    pub id: String,
    pub label: String,
    pub photos: u64,
    pub videos: u64,
}

/// `listSources`.
pub fn parse_sources(b: &Value) -> Vec<Source> {
    b["sources"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|s| {
            Some(Source {
                id: text(&s["id"])?,
                label: text(&s["label"]).unwrap_or_default(),
                photos: s["photos"].as_u64().unwrap_or(0),
                videos: s["videos"].as_u64().unwrap_or(0),
            })
        })
        .collect()
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Album {
    pub id: String,
    pub title: String,
    pub count: u64,
    pub system: bool,
    pub cover: Option<String>,
}

/// `parseAlbumCard`, and `listAlbums`' rule: no share albums.
pub fn parse_albums(b: &Value) -> Vec<Album> {
    b["albums"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|a| {
            let key = text(&a["system_key"]);
            if key.as_deref().is_some_and(|k| k.starts_with("share:")) {
                return None;
            }
            Some(Album {
                id: text(&a["id"])?,
                title: text(&a["title"]).unwrap_or_else(|| "Untitled album".to_owned()),
                count: a["count"].as_u64().unwrap_or(0),
                system: key.is_some(),
                cover: text(&a["cover_item"]),
            })
        })
        .collect()
}

/// `listMonths`.
pub fn parse_months(b: &Value) -> Vec<(String, u64)> {
    b["months"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|m| {
            let month = text(&m["month"])?;
            let good = month.len() == 7 && month.as_bytes()[4] == b'-';
            good.then(|| (month, m["count"].as_u64().unwrap_or(0)))
        })
        .collect()
}

/// `sectionKey`, by the local day: `2026-03-04`. `0000-00` when there is no date.
pub fn section_key(at_ms: Option<i64>, zone: &Zone) -> String {
    let Some(ms) = at_ms else { return "0000-00".to_owned() };
    let (offset, _) = zone.at(ms.div_euclid(1000));
    let (y, m, d) = civil((ms.div_euclid(1000) + offset).div_euclid(86_400));
    format!("{y:04}-{m:02}-{d:02}")
}

fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y.rem_euclid(400);
    let doy = (153 * (if m > 2 { m as i64 - 3 } else { m as i64 + 9 }) + 2) / 5 + d as i64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// `formatSectionLabel`, for a day: "Wednesday, 4 March 2026". "No date" for an item with none.
pub fn section_label(key: &str) -> String {
    let parts: Vec<&str> = key.split('-').collect();
    if parts.len() != 3 {
        return "No date".to_owned();
    }
    let (Ok(y), Ok(m), Ok(d)) = (parts[0].parse::<i64>(), parts[1].parse::<u32>(), parts[2].parse::<u32>()) else {
        return "No date".to_owned();
    };
    if !(1..=12).contains(&m) {
        return "No date".to_owned();
    }
    let weekday = WEEKDAYS[days_from_civil(y, m, d).rem_euclid(7) as usize];
    format!("{weekday}, {d} {} {y}", MONTHS[m as usize - 1])
}

/// "March 2026", from `2026-03`.
pub fn month_label(key: &str) -> String {
    let (y, m) = key.split_once('-').unwrap_or((key, ""));
    match m.parse::<usize>() {
        Ok(n @ 1..=12) => format!("{} {y}", MONTHS[n - 1]),
        _ => key.to_owned(),
    }
}

/// `formatDuration`: `0:07`, `12:30`, `1:02:03`.
pub fn format_duration(ms: u64) -> String {
    let total = (ms + 500) / 1000;
    let (h, m, s) = (total / 3600, total / 60 % 60, total % 60);
    if h > 0 { format!("{h}:{m:02}:{s:02}") } else { format!("{m}:{s:02}") }
}

/// `formatExposure`: 0.004 is "1/250", 0.5 "1/2", 2 "2", 2.5 "2.5".
pub fn format_exposure(s: f64) -> String {
    if s <= 0.0 {
        return String::new();
    }
    if s >= 1.0 {
        let t = format!("{s:.1}");
        return t.strip_suffix(".0").unwrap_or(&t).to_owned();
    }
    format!("1/{}", (1.0 / s).round() as u64)
}

/// `deviceNames`: the distinct devices behind these items, in first-seen order.
pub fn device_names(items: &[&Item]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for i in items {
        if let Some(d) = &i.device {
            let name = i.device_name.clone().unwrap_or_else(|| d.clone());
            if !out.contains(&name) {
                out.push(name);
            }
        }
    }
    out
}

/// `joinNames`: "a", "a and b", "a, b and c".
pub fn join_names(names: &[String]) -> String {
    match names {
        [] => String::new(),
        [a] => a.clone(),
        [rest @ .., last] => format!("{} and {last}", rest.join(", ")),
    }
}

/// What the info panel shows: `PhotoInfo`, one line per row that has anything to say.
pub fn details_rows(body: &Value, item: &Item, zone: &Zone, album_titles: &[(String, String)]) -> Vec<(String, String)> {
    let full = if body["item"].is_object() { &body["item"] } else { &Value::Null };
    let exif = &body["exif"];
    let mut rows: Vec<(String, String)> = Vec::new();
    let mut add = |k: &str, v: String| {
        if !v.is_empty() {
            rows.push((k.to_owned(), v));
        }
    };
    add("Taken", item.taken_at.as_deref().map_or_else(|| "Date unknown".to_owned(), |t| taken_label(t, exif["offset"].as_str(), zone)));
    add("Device", item.device_name.clone().or_else(|| item.device.clone()).unwrap_or_default());
    let make = exif["make"].as_str().unwrap_or("");
    let model = exif["model"].as_str().unwrap_or("");
    let camera = if !make.is_empty() && model.to_lowercase().starts_with(&make.to_lowercase()) {
        model.to_owned()
    } else {
        format!("{make} {model}").trim().to_owned()
    };
    add("Camera", if camera.is_empty() { full["camera"].as_str().unwrap_or("").to_owned() } else { camera });
    let mut settings = Vec::new();
    if let Some(iso) = exif["iso"].as_f64() {
        settings.push(format!("ISO {}", iso.round() as u64));
    }
    if let Some(e) = exif["exposure_s"].as_f64() {
        settings.push(format!("{} s", format_exposure(e)));
    }
    if let Some(f) = exif["f_number"].as_f64() {
        settings.push(format!("f/{f}"));
    }
    if let Some(mm) = exif["focal_mm"].as_f64() {
        settings.push(format!("{} mm", mm.round() as u64));
    }
    add("Settings", join_dots(&settings));
    add("Lens", exif["lens"].as_str().unwrap_or("").to_owned());
    add("Size", lucina_files::drive::format_bytes(Some(item.size)));
    if let (Some(w), Some(h)) = (item.width, item.height) {
        let mp = (w as f64 * h as f64 / 1e6 * 10.0).round() / 10.0;
        add("Dimensions", format!("{w} × {h} · {mp} MP"));
    }
    if item.video {
        add("Duration", item.duration_ms.map_or(String::new(), format_duration));
    }
    add("Original file", full["origin"]["filename"].as_str().unwrap_or("").to_owned());
    add("Folder", full["origin"]["folder"].as_str().unwrap_or("").to_owned());
    add("Imported from", full["origin"]["imported_from"].as_str().unwrap_or("").to_owned());
    let place = &full["place"];
    if place.is_object() {
        let mut parts: Vec<String> = Vec::new();
        for key in ["name", "admin1", "country"] {
            if let Some(p) = place[key].as_str() {
                if !p.is_empty() && !parts.iter().any(|x| x == p) {
                    parts.push(p.to_owned());
                }
            }
        }
        let line = if parts.is_empty() {
            format!("{:.4}, {:.4}", place["lat"].as_f64().unwrap_or(0.0), place["lon"].as_f64().unwrap_or(0.0))
        } else {
            parts.join(", ")
        };
        add("Location", line);
    }
    let names: Vec<String> = item.albums.iter().filter_map(|id| album_titles.iter().find(|(i, _)| i == id).map(|(_, t)| t.clone())).collect();
    add("Albums", if names.is_empty() { "Not in any album".to_owned() } else { names.join(", ") });
    rows
}

/// `formatTaken`: as the wall clock showed it where it was taken when the offset is known, else here.
pub fn taken_label(iso: &str, offset: Option<&str>, zone: &Zone) -> String {
    let Some(ms) = dates::parse_iso(iso) else { return "Date unknown".to_owned() };
    let secs = ms.div_euclid(1000);
    let off = offset.and_then(offset_seconds).unwrap_or_else(|| zone.at(secs).0);
    let local = secs + off;
    let days = local.div_euclid(86_400);
    let (y, m, d) = civil(days);
    let day = local.rem_euclid(86_400);
    let weekday = &WEEKDAYS[days.rem_euclid(7) as usize][..3];
    format!("{weekday}, {d} {} {y}, {:02}:{:02}", &MONTHS[m as usize - 1][..3], day / 3600, day % 3600 / 60)
}

/// `offsetMinutes`, in seconds.
fn offset_seconds(s: &str) -> Option<i64> {
    let s = s.trim();
    let sign = match s.chars().next()? {
        '+' => 1,
        '-' => -1,
        _ => return None,
    };
    let digits: String = s[1..].chars().filter(|c| *c != ':').collect();
    if digits.len() != 4 {
        return None;
    }
    let h: i64 = digits[..2].parse().ok()?;
    let m: i64 = digits[2..].parse().ok()?;
    Some(sign * (h * 3600 + m * 60))
}

/// `daysLeft`, rounded up, at least 0.
pub fn days_left(expires: &str, now_ms: i64) -> Option<i64> {
    let at = dates::parse_iso(expires)?;
    Some(((at - now_ms) as f64 / 86_400_000.0).ceil().max(0.0) as i64)
}

/// Decode the base64 of a ThumbHash in the item JSON.
pub fn base64(s: &str) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(s.len() * 3 / 4);
    let (mut bits, mut acc) = (0u32, 0u32);
    for b in s.bytes() {
        let v = match b {
            b'A'..=b'Z' => b - b'A',
            b'a'..=b'z' => b - b'a' + 26,
            b'0'..=b'9' => b - b'0' + 52,
            b'+' | b'-' => 62,
            b'/' | b'_' => 63,
            b'=' => break,
            _ => return None,
        } as u32;
        acc = acc << 6 | v;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
            acc &= (1 << bits) - 1;
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn an_item_with_only_an_id_and_a_kind_parses() {
        let i = parse_item(&json!({"id": "a", "kind": "photo"})).unwrap();
        assert_eq!((i.rotation, i.favorite, i.video), (0, false, false));
        assert!(parse_item(&json!({"id": "a", "kind": "file"})).is_none());
        assert!(parse_item(&json!({"kind": "photo"})).is_none());
    }

    #[test]
    fn a_rotation_it_cannot_draw_is_none() {
        let i = parse_item(&json!({"id": "a", "kind": "photo", "rotation": 45})).unwrap();
        assert_eq!(i.rotation, 0);
    }

    #[test]
    fn a_url_to_elsewhere_is_dropped() {
        let i = parse_item(&json!({"id": "a", "kind": "photo", "urls": {"thumb": "https://evil.example/x", "large": "//evil/x", "original": "/v1/ok"}})).unwrap();
        assert_eq!((i.thumb, i.large, i.original.as_deref()), (None, None, Some("/v1/ok")));
    }

    #[test]
    fn query_is_the_webs() {
        let q = Query { q: " a b ".into(), favorite: true, album: Some("alb".into()), ..Query::default() };
        assert_eq!(query_string(&q, Some("c"), 200), "scope=gallery&origin=all&kind=photo%2Cvideo&q=a%20b&favorite=true&album=alb&cursor=c&limit=200");
        assert!(query_string(&Query::default(), None, 900).ends_with("limit=500"));
    }

    #[test]
    fn days_and_months_are_named() {
        assert_eq!(section_label("2026-03-04"), "Wednesday, 4 March 2026");
        assert_eq!(section_label("0000-00"), "No date");
        assert_eq!(month_label("2026-03"), "March 2026");
        assert_eq!(section_key(Some(1_772_582_400_000), &Zone::utc()), "2026-03-04");
    }

    #[test]
    fn durations_and_exposures_read_as_on_the_web() {
        assert_eq!(format_duration(7_000), "0:07");
        assert_eq!(format_duration(750_000), "12:30");
        assert_eq!(format_duration(3_723_000), "1:02:03");
        assert_eq!(format_exposure(0.004), "1/250");
        assert_eq!(format_exposure(0.5), "1/2");
        assert_eq!(format_exposure(2.0), "2");
        assert_eq!(format_exposure(2.5), "2.5");
    }

    #[test]
    fn the_wall_clock_where_it_was_taken() {
        assert_eq!(taken_label("2026-03-04T12:03:00.000Z", Some("+02:00"), &Zone::utc()), "Wed, 4 Mar 2026, 14:03");
        assert_eq!(taken_label("2026-03-04T12:03:00.000Z", None, &Zone::utc()), "Wed, 4 Mar 2026, 12:03");
        assert_eq!(taken_label("x", None, &Zone::utc()), "Date unknown");
    }

    #[test]
    fn names_join_as_on_the_web() {
        assert_eq!(join_names(&["a".into()]), "a");
        assert_eq!(join_names(&["a".into(), "b".into()]), "a and b");
        assert_eq!(join_names(&["a".into(), "b".into(), "c".into()]), "a, b and c");
    }

    #[test]
    fn base64_decodes_a_real_thumbhash() {
        let b = base64("k+cFHYr5eJdrY3aeeph2ezQK8teJ").unwrap();
        assert_eq!(b.len(), 21);
        assert!(secretary_image::placeholder(&b).is_ok());
    }
}
