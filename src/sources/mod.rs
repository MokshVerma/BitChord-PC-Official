//! Streaming sources other than YouTube: a catalogue that can be searched for the same
//! recording as a YouTube track, and that serves a stream for it.
//!
//! A source is asked in rank order (`SourceRegistry`); whether its row is the same song is
//! decided once, here (`best_match`), so every source is held to the same rules.
//!
//! `backend.rs` races them against YouTube (`resolve_ranked`).
#![allow(dead_code)]

pub mod jiosaavn;

/// What the catalogues' CDNs are asked with: an ordinary desktop browser's agent.
pub const USER_AGENT: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/134.0.0.0 Safari/537.36";

use crate::model::Track;

/// Which kind of source a configured entry is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SourceKind {
    JioSaavn,
}

impl SourceKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::JioSaavn => "JioSaavn",
        }
    }

    /// Whether the source can serve lossless audio when that is what's asked for.
    pub fn can_serve_lossless(self) -> bool {
        match self {
            Self::JioSaavn => false,
        }
    }

    /// Quick enough (search and stream in about half a second) to ask before a song plays.
    pub fn worth_prefetching(self) -> bool {
        match self {
            Self::JioSaavn => true,
        }
    }
}

/// One row of a source's catalogue.
#[derive(Clone, Debug, PartialEq)]
pub struct SourceTrack {
    pub id: String,
    pub title: String,
    pub artist: String,
    pub album: Option<String>,
    /// Seconds.
    pub duration: Option<u32>,
    pub explicit: bool,
    pub artwork: Option<String>,
}

/// A playable answer from a source.
#[derive(Clone, Debug, PartialEq)]
pub struct SourceStream {
    pub url: String,
    pub kbps: Option<u32>,
    /// What the player shows for it.
    pub codec: &'static str,
    pub kind: SourceKind,
}

/// A configured source. A closed set, so no trait object is needed.
pub enum AnySource {
    JioSaavn(jiosaavn::JioSaavn),
}

impl AnySource {
    pub fn kind(&self) -> SourceKind {
        match self {
            Self::JioSaavn(_) => SourceKind::JioSaavn,
        }
    }

    pub async fn search(&self, http: &reqwest::Client, query: &str, limit: usize) -> Vec<SourceTrack> {
        match self {
            Self::JioSaavn(s) => s.search(http, query, limit).await,
        }
    }

    pub async fn stream(&self, http: &reqwest::Client, track_id: &str) -> Option<SourceStream> {
        match self {
            Self::JioSaavn(s) => s.stream(http, track_id).await,
        }
    }

    /// The row of this source that is `track`, and its stream.
    pub async fn find(&self, http: &reqwest::Client, track: &Track) -> Option<(SourceTrack, SourceStream)> {
        let rows = self.search(http, &search_query(track), 10).await;
        let row = best_match(track, rows)?;
        let stream = self.stream(http, &row.id).await?;
        Some((row, stream))
    }
}

/// The enabled sources, in the order the listener ranked them.
#[derive(Default)]
pub struct SourceRegistry {
    sources: Vec<std::sync::Arc<AnySource>>,
}

impl SourceRegistry {
    pub fn new(sources: Vec<AnySource>) -> Self {
        Self { sources: sources.into_iter().map(std::sync::Arc::new).collect() }
    }

    pub fn is_empty(&self) -> bool {
        self.sources.is_empty()
    }

    /// Ranked best first.
    pub fn active(&self) -> &[std::sync::Arc<AnySource>] {
        &self.sources
    }

    /// The sources ranked above `kind`'s first entry (all of them when it isn't enabled).
    pub fn ranked_above(&self, kind: SourceKind) -> Vec<std::sync::Arc<AnySource>> {
        self.sources.iter().take_while(|s| s.kind() != kind).cloned().collect()
    }
}

/// The text a source is searched with for `track`.
pub fn search_query(track: &Track) -> String {
    let artist = track.artists.first().map_or_else(|| track.artist_line(), |a| a.name.clone());
    format!("{} {}", track.title, artist).trim().to_owned()
}

/// The row that is the same recording as `track`, if any: title, artist and length all
/// have to agree (`hires::match_score`). With equal scores the earlier row, and so an
/// uncensored one where the source lists those first, wins.
pub fn best_match(track: &Track, rows: Vec<SourceTrack>) -> Option<SourceTrack> {
    let artist = track.artist_line();
    let mut best: Option<(i32, SourceTrack)> = None;
    for row in rows {
        let Some(mut score) = crate::hires::match_score(&track.title, &artist, track.duration, &row.title, &row.artist, row.duration) else { continue };
        // The same title and artist can be two releases of the song: the album settles it.
        if let (Some(wanted), Some(found)) = (track.album.as_ref().map(|a| a.name.to_ascii_lowercase()), row.album.as_ref().map(|a| a.to_ascii_lowercase()))
            && !wanted.is_empty()
            && wanted == found
        {
            score += 10;
        }
        if best.as_ref().is_none_or(|(s, _)| score > *s) {
            best = Some((score, row));
        }
    }
    best.map(|(_, row)| row)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Link;

    fn track(title: &str, artist: &str, album: Option<&str>, duration: Option<u32>) -> Track {
        Track {
            id: "vid".into(),
            title: title.into(),
            artists: vec![Link { name: artist.into(), id: None }],
            album: album.map(|a| Link { name: a.into(), id: None }),
            duration,
            thumbs: Vec::new(),
            track_nr: None,
            plays: None,
            music_video: false,
        }
    }

    fn row(id: &str, title: &str, artist: &str, album: Option<&str>, duration: Option<u32>) -> SourceTrack {
        SourceTrack { id: id.into(), title: title.into(), artist: artist.into(), album: album.map(str::to_owned), duration, explicit: false, artwork: None }
    }

    #[test]
    fn the_same_recording_is_picked() {
        let wanted = track("Tum Hi Ho", "Arijit Singh", None, Some(262));
        let rows = vec![row("a", "Tum Hi Ho (Remix)", "Arijit Singh", None, Some(262)), row("b", "Tum Hi Ho", "Arijit Singh", None, Some(262))];
        assert_eq!(best_match(&wanted, rows).map(|r| r.id), Some("b".into()));
    }

    #[test]
    fn another_artist_or_length_is_not_the_song() {
        let wanted = track("Tum Hi Ho", "Arijit Singh", None, Some(262));
        assert!(best_match(&wanted, vec![row("a", "Tum Hi Ho", "Someone Else", None, Some(262))]).is_none());
        assert!(best_match(&wanted, vec![row("a", "Tum Hi Ho", "Arijit Singh", None, Some(200))]).is_none());
    }

    #[test]
    fn the_album_settles_two_releases() {
        let wanted = track("Tum Hi Ho", "Arijit Singh", Some("Aashiqui 2"), Some(262));
        let rows = vec![row("a", "Tum Hi Ho", "Arijit Singh", Some("Other"), Some(262)), row("b", "Tum Hi Ho", "Arijit Singh", Some("Aashiqui 2"), Some(262))];
        assert_eq!(best_match(&wanted, rows).map(|r| r.id), Some("b".into()));
    }

    #[test]
    fn a_catalogue_streams_rate_is_said_once() {
        let info = crate::hires::Info {
            source: "JioSaavn".into(),
            host: "aac.saavncdn.com".into(),
            codec: "AAC".into(),
            quality: String::new(),
            bitrate_kbps: Some(320),
            sample_rate: None,
            bit_depth: None,
            channels: None,
            atmos: false,
            size: 1,
            duration_ms: 1,
        };
        assert_eq!(info.badge(), "320 kbps");
        assert_eq!(info.details_line(), "320 kbps · AAC");
        // Discord never names the source: it always presents as YouTube Music.
        assert_eq!(info.rpc_line(), "320 kbps · AAC");
        // A rate the badge doesn't say is still listed, 
        let high = crate::hires::Info { bitrate_kbps: Some(160), source: "Addon".into(), ..info };
        assert_eq!(high.rpc_line(), "High · AAC · 160 kbps");
    }

    #[test]
    fn ranked_above_stops_at_the_kind() {
        let registry = SourceRegistry::new(vec![AnySource::JioSaavn(jiosaavn::JioSaavn)]);
        assert!(registry.ranked_above(SourceKind::JioSaavn).is_empty());
        assert_eq!(registry.active().len(), 1);
    }
}
