//! JioSaavn's public web API: search and stream URLs, no account.
//!
//! `search.getResults` lists songs; `song.getDetails` carries an `encrypted_media_url`
//! (DES-ECB under the web player's public key) that decrypts to a CDN URL. Requests go out
//! plainly: no forwarded-for address is set, so a song the catalogue doesn't license where
//! the listener is simply isn't found, and YouTube plays it.

use std::time::Duration;

use base64::Engine;
use serde_json::Value;

use super::{SourceKind, SourceStream, SourceTrack};

const BASE_URL: &str = "https://www.jiosaavn.com/api.php";
/// JioSaavn answers in about 0.4 s when it answers.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(6);
/// The public key the web player decrypts `encrypted_media_url` with.
const URL_KEY: &[u8; 8] = b"38346591";
/// JioSaavn files a track at 48, 96, 160 or 320 kbps. The first two are below YouTube's
/// own AAC, so they are never worth swapping to.
const MIN_USABLE_KBPS: u32 = 96;

/// The API's own row, before it is a `SourceTrack`.
#[derive(Clone, Debug, Default, PartialEq)]
struct Song {
    id: String,
    title: String,
    image: String,
    explicit: bool,
    album: String,
    encrypted_url: String,
    duration: String,
    has_320: bool,
    artists: Vec<String>,
}

#[derive(Default)]
pub struct JioSaavn;

impl JioSaavn {
    /// Up to `limit` songs for `query`, uncensored rows first; empty on any failure.
    pub async fn search(&self, http: &reqwest::Client, query: &str, limit: usize) -> Vec<SourceTrack> {
        let body = match get(http, &[("__call", "search.getResults"), ("q", query), ("p", "1"), ("n", "10")]).await {
            Ok(body) => body,
            Err(e) => {
                log::info!("jiosaavn search: {e:#}");
                return Vec::new();
            }
        };
        let mut songs = parse_search(&body);
        // Uncensored rows ahead of their clean duplicates, otherwise in the catalogue's order.
        songs.sort_by_key(|s| !s.explicit);
        songs.into_iter().take(limit).map(track_of).collect()
    }

    /// The best stream this song really has, or `None` (also when it is too thin to play).
    pub async fn stream(&self, http: &reqwest::Client, track_id: &str) -> Option<SourceStream> {
        let body = match get(http, &[("__call", "song.getDetails"), ("pids", track_id)]).await {
            Ok(body) => body,
            Err(e) => {
                log::info!("jiosaavn details: {e:#}");
                return None;
            }
        };
        let Some(song) = parse_details(&body) else {
            log::info!("jiosaavn: no song in the details for {track_id}");
            return None;
        };
        let (url, kbps) = best_stream(&decrypt(&song.encrypted_url)?, song.has_320)?;
        if kbps.is_some_and(|k| k <= MIN_USABLE_KBPS) {
            log::info!("jiosaavn: only {} kbps for {track_id}; not worth playing", kbps.unwrap_or(0));
            return None;
        }
        Some(SourceStream { url, kbps, codec: "AAC", kind: SourceKind::JioSaavn })
    }
}

fn track_of(song: Song) -> SourceTrack {
    SourceTrack {
        id: song.id,
        title: song.title,
        artist: if song.artists.is_empty() { "Unknown Artist".to_owned() } else { song.artists.join(", ") },
        album: Some(song.album).filter(|a| !a.is_empty()),
        duration: song.duration.trim().parse().ok(),
        explicit: song.explicit,
        artwork: large_artwork(&song.image),
    }
}

/// The 500×500 artwork instead of the 150×150 the API names, over https.
fn large_artwork(image: &str) -> Option<String> {
    if image.trim().is_empty() {
        return None;
    }
    let url = image.replace("150x150", "500x500").replace("50x50", "500x500");
    Some(match url.strip_prefix("http://") {
        Some(rest) => format!("https://{rest}"),
        None => url,
    })
}

async fn get(http: &reqwest::Client, call: &[(&str, &str)]) -> anyhow::Result<String> {
    let common = [("_format", "json"), ("_marker", "0"), ("api_version", "4"), ("ctx", "android")];
    let response = http
        .get(BASE_URL)
        .query(&call[..1])
        .query(&common)
        .query(&call[1..])
        .timeout(REQUEST_TIMEOUT)
        .header("Accept", "application/json")
        .header("User-Agent", super::USER_AGENT)
        // Without this the catalogue censors rows that have an uncensored twin.
        .header("Cookie", "explicit_content=1")
        .send()
        .await?;
    anyhow::ensure!(response.status().is_success(), "HTTP {}", response.status());
    Ok(response.text().await?)
}

// ── Parsing ──────────────────────────────────────────────────────────────

/// A string from a value that may be a string or a number.
fn text(v: Option<&Value>) -> String {
    match v {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Number(n)) => n.to_string(),
        Some(Value::Bool(b)) => b.to_string(),
        _ => String::new(),
    }
}

/// JioSaavn HTML-escapes its text (`Tum Hi Ho (From &quot;Aashiqui 2&quot;)`); `&amp;` must
/// not reach the matcher as the word "amp".
fn unescape_html(s: &str) -> String {
    if !s.contains('&') {
        return s.to_owned();
    }
    let mut out = s.to_owned();
    for (entity, c) in [("&quot;", "\""), ("&#039;", "'"), ("&#39;", "'"), ("&apos;", "'"), ("&lt;", "<"), ("&gt;", ">"), ("&amp;", "&")] {
        out = out.replace(entity, c);
    }
    out
}

fn song_of(v: &Value) -> Option<Song> {
    let more = v.get("more_info");
    let m = |key: &str| text(more.and_then(|m| m.get(key)));
    let mut artists = Vec::new();
    if let Some(list) = more.and_then(|m| m.get("artistMap")).and_then(|a| a.get("primary_artists")).and_then(Value::as_array) {
        for artist in list {
            let name = unescape_html(&text(artist.get("name")));
            if !name.trim().is_empty() {
                artists.push(name);
            }
        }
    }
    let id = text(v.get("id"));
    if id.is_empty() {
        return None;
    }
    let explicit = text(v.get("explicit_content"));
    Some(Song {
        id,
        title: unescape_html(&text(v.get("title"))),
        image: text(v.get("image")),
        explicit: explicit == "1" || explicit.eq_ignore_ascii_case("true"),
        album: unescape_html(&m("album")),
        encrypted_url: m("encrypted_media_url"),
        duration: m("duration"),
        has_320: m("320kbps").eq_ignore_ascii_case("true"),
        artists,
    })
}

/// `{"results":[…]}`.
fn parse_search(body: &str) -> Vec<Song> {
    let Ok(v) = serde_json::from_str::<Value>(body) else { return Vec::new() };
    v.get("results").and_then(Value::as_array).map(|rows| rows.iter().filter_map(song_of).collect()).unwrap_or_default()
}

/// `song.getDetails` answers with a map keyed by the id asked for (`{"5xceLLNe":{…}}`),
/// not the list search uses. Both shapes are read.
fn parse_details(body: &str) -> Option<Song> {
    let v = serde_json::from_str::<Value>(body).ok()?;
    let object = v.as_object()?;
    if let Some(first) = object.get("songs").and_then(Value::as_array).and_then(|a| a.first()) {
        return song_of(first);
    }
    object.values().filter(|v| v.is_object()).find_map(song_of)
}

// ── Streams ──────────────────────────────────────────────────────────────

/// DES-ECB (PKCS#5) under the public key, base64-encoded.
fn decrypt(encrypted: &str) -> Option<String> {
    use ecb::cipher::{BlockDecryptMut, KeyInit, block_padding::Pkcs7};
    let compact: String = encrypted.chars().filter(|c| !c.is_whitespace()).collect();
    if compact.is_empty() {
        return None;
    }
    let bytes = base64::engine::general_purpose::STANDARD.decode(compact).ok()?;
    let plain = ecb::Decryptor::<des::Des>::new_from_slice(URL_KEY).ok()?.decrypt_padded_vec_mut::<Pkcs7>(&bytes).ok()?;
    let url = String::from_utf8(plain).ok()?.trim().to_owned();
    (!url.is_empty()).then_some(url)
}

/// The best CDN URL the track really has, and the rate it serves. The rewrite to `_320` is
/// made only when the catalogue says one exists: asking the CDN for `_320` anyway fetches
/// nothing better. A URL that isn't in the usual `_<rate>.<ext>` shape is played as it is,
/// its rate unknown.
fn best_stream(url: &str, supports_320: bool) -> Option<(String, Option<u32>)> {
    if url.trim().is_empty() {
        return None;
    }
    let Some((start, end, rate)) = rendition(url) else { return Some((url.to_owned(), None)) };
    if !supports_320 {
        return Some((url.to_owned(), Some(rate)));
    }
    let ext = &url[start + 1 + rate.to_string().len() + 1..end];
    Some((format!("{}_320.{}{}", &url[..start], ext, &url[end..]), Some(320)))
}

/// `_<48|96|160|320>.<mp4|aac|mp3>` right before the end or a `?`/`#`: where it starts, where
/// it ends, and the rate.
fn rendition(url: &str) -> Option<(usize, usize, u32)> {
    let lower = url.to_ascii_lowercase();
    for rate in [48u32, 96, 160, 320] {
        for ext in ["mp4", "aac", "mp3"] {
            let pattern = format!("_{rate}.{ext}");
            let mut from = 0;
            while let Some(at) = lower[from..].find(&pattern) {
                let start = from + at;
                let end = start + pattern.len();
                if lower[end..].is_empty() || lower[end..].starts_with(['?', '#']) {
                    return Some((start, end, rate));
                }
                from = start + 1;
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    const SEARCH: &str = r#"{"results":[
        {"id":"aRZbUYD7","title":"Tum Hi Ho (From &quot;Aashiqui 2&quot;)","image":"http://c.saavncdn.com/430/x-150x150.jpg","explicit_content":"0",
         "more_info":{"album":"Aashiqui 2 &amp; More","duration":"262","320kbps":"true","encrypted_media_url":"",
                      "artistMap":{"primary_artists":[{"id":"1","name":"Arijit Singh"}]}}},
        {"id":"x2","title":"Tum Hi Ho","explicit_content":"1","more_info":{"duration":"262"}}]}"#;

    #[test]
    fn search_rows_are_read_and_unescaped() {
        let songs = parse_search(SEARCH);
        assert_eq!(songs.len(), 2);
        assert_eq!(songs[0].title, "Tum Hi Ho (From \"Aashiqui 2\")");
        assert_eq!(songs[0].album, "Aashiqui 2 & More");
        assert_eq!(songs[0].artists, ["Arijit Singh"]);
        assert!(songs[0].has_320 && !songs[0].explicit);
        assert!(songs[1].explicit);
    }

    #[test]
    fn a_row_becomes_a_track() {
        let track = track_of(parse_search(SEARCH).remove(0));
        assert_eq!(track.duration, Some(262));
        assert_eq!(track.artist, "Arijit Singh");
        assert_eq!(track.artwork.as_deref(), Some("https://c.saavncdn.com/430/x-500x500.jpg"));
    }

    #[test]
    fn details_are_read_from_either_shape() {
        let keyed = r#"{"aRZbUYD7":{"id":"aRZbUYD7","title":"A","more_info":{"encrypted_media_url":"abc","320kbps":"false"}}}"#;
        assert_eq!(parse_details(keyed).map(|s| s.id), Some("aRZbUYD7".into()));
        let listed = r#"{"songs":[{"id":"z","title":"B","more_info":{}}]}"#;
        assert_eq!(parse_details(listed).map(|s| s.id), Some("z".into()));
        assert!(parse_details("not json").is_none());
    }

    #[test]
    fn the_url_decrypts() {
        use ecb::cipher::{BlockEncryptMut, KeyInit, block_padding::Pkcs7};
        let url = "https://aac.saavncdn.com/430/abc_96.mp4";
        let sealed = ecb::Encryptor::<des::Des>::new_from_slice(URL_KEY).unwrap().encrypt_padded_vec_mut::<Pkcs7>(url.as_bytes());
        let encoded = base64::engine::general_purpose::STANDARD.encode(sealed);
        assert_eq!(decrypt(&encoded).as_deref(), Some(url));
        assert_eq!(decrypt(""), None);
        assert_eq!(decrypt("%%%"), None);
    }

    #[test]
    fn the_rate_is_upgraded_only_when_one_exists() {
        let url = "https://aac.saavncdn.com/430/abc_96.mp4";
        assert_eq!(best_stream(url, false), Some((url.to_owned(), Some(96))));
        assert_eq!(best_stream(url, true), Some(("https://aac.saavncdn.com/430/abc_320.mp4".to_owned(), Some(320))));
    }

    #[test]
    fn parameters_after_the_extension_are_kept() {
        let url = "https://cdn/x_160.mp4?token=1#frag";
        assert_eq!(best_stream(url, true), Some(("https://cdn/x_320.mp4?token=1#frag".to_owned(), Some(320))));
        assert_eq!(best_stream(url, false), Some((url.to_owned(), Some(160))));
    }

    #[test]
    fn an_unrecognised_url_is_never_labelled_320() {
        let url = "https://cdn/stream/abc";
        assert_eq!(best_stream(url, true), Some((url.to_owned(), None)));
        assert_eq!(best_stream("  ", true), None);
    }

    /// Against the real service: `cargo test live_jiosaavn -- --ignored --nocapture`.
    #[tokio::test]
    #[ignore]
    async fn live_jiosaavn_finds_and_streams_a_song() {
        let http = reqwest::Client::new();
        let source = JioSaavn;
        let rows = source.search(&http, "Tum Hi Ho Arijit Singh", 10).await;
        for r in &rows {
            println!("{} | {} | {} | {:?}s | {:?}", r.id, r.title, r.artist, r.duration, r.album);
        }
        assert!(!rows.is_empty(), "search found nothing");
        let mut streamed = None;
        for r in &rows {
            if let Some(s) = source.stream(&http, &r.id).await {
                println!("stream for {}: {} kbps {:?} {}", r.id, s.kbps.unwrap_or(0), s.codec, s.url);
                streamed = Some(s);
                break;
            }
        }
        let s = streamed.expect("no row had a usable stream");
        let head = http.head(&s.url).header("User-Agent", crate::sources::USER_AGENT).send().await.unwrap();
        println!("HEAD {} len={:?} type={:?}", head.status(), head.content_length(), head.headers().get("content-type"));
        assert!(head.status().is_success());
    }

    #[test]
    fn a_rate_inside_the_path_is_not_a_rendition() {
        // `_96.mp4` mid-URL, not at the end or before a query.
        assert_eq!(rendition("https://cdn/a_96.mp4/seg"), None);
    }
}
