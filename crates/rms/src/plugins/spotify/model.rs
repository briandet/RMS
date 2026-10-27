//! What browsing Spotify gives: tracks, albums, artists and playlists, with what RMS shows of
//! them.  [`super::browse`] and [`super::pathfinder`] build them, [`super::menu`] and the
//! ListenBrainz discoveries use them.

use super::{url_for, TrackMeta};

/// An artist as a track or album credits it.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ArtistRef {
    pub id: String,
    pub name: String,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Track {
    pub id: String,
    pub name: String,
    pub artists: Vec<ArtistRef>,
    pub album: String,
    pub album_id: String,
    /// The album's largest cover.
    pub cover: Option<String>,
    /// The album's year.
    pub year: Option<i32>,
    pub duration_ms: u32,
    pub isrc: Option<String>,
}

impl Track {
    pub fn url(&self) -> String {
        url_for("track", &self.id)
    }

    /// The artists' names, comma-separated.
    pub fn artist_names(&self) -> String {
        names(&self.artists)
    }

    pub fn meta(&self) -> TrackMeta {
        TrackMeta {
            title: self.name.clone(),
            artist: self.artist_names(),
            album: self.album.clone(),
            secs: f64::from(self.duration_ms) / 1000.0,
            image: self.cover.clone(),
            album_id: Some(self.album_id.clone()).filter(|a| !a.is_empty()),
            artist_id: self.artists.first().map(|a| a.id.clone()),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Album {
    pub id: String,
    pub name: String,
    pub artists: Vec<ArtistRef>,
    /// The largest cover.
    pub cover: Option<String>,
}

impl Album {
    pub fn artist_names(&self) -> String {
        names(&self.artists)
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Artist {
    pub id: String,
    pub name: String,
    pub picture: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Playlist {
    pub id: String,
    pub name: String,
    /// The owner's display name, or their username.
    pub owner: Option<String>,
    pub picture: Option<String>,
}

/// Something played from (Recently Played).
#[derive(Debug, Clone, PartialEq)]
pub enum Context {
    Album(Album),
    Playlist(Playlist),
    Artist(Artist),
}

/// What a search looks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchKind {
    Track,
    Album,
    Artist,
    Playlist,
}

impl SearchKind {
    pub const ALL: [SearchKind; 4] = [
        SearchKind::Track,
        SearchKind::Album,
        SearchKind::Artist,
        SearchKind::Playlist,
    ];

    /// Its name in menu references (`searchtrack:…`).
    pub fn name(self) -> &'static str {
        match self {
            SearchKind::Track => "track",
            SearchKind::Album => "album",
            SearchKind::Artist => "artist",
            SearchKind::Playlist => "playlist",
        }
    }

    pub fn from_name(name: &str) -> Option<SearchKind> {
        SearchKind::ALL.into_iter().find(|k| k.name() == name)
    }
}

/// What a search found (of one [`SearchKind`]).
#[derive(Debug, Clone, PartialEq)]
pub enum Found {
    Tracks(Vec<Track>),
    Albums(Vec<Album>),
    Artists(Vec<Artist>),
    Playlists(Vec<Playlist>),
}

fn names(artists: &[ArtistRef]) -> String {
    artists
        .iter()
        .map(|a| a.name.as_str())
        .collect::<Vec<_>>()
        .join(", ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_tracks_metadata() {
        let t = Track {
            id: "abc".into(),
            name: "Song".into(),
            artists: vec![
                ArtistRef {
                    id: "a".into(),
                    name: "A".into(),
                },
                ArtistRef {
                    id: "b".into(),
                    name: "B".into(),
                },
            ],
            album: "Album".into(),
            album_id: "al".into(),
            cover: Some("l".into()),
            duration_ms: 215000,
            ..Default::default()
        };
        let m = t.meta();
        assert_eq!(
            (m.title.as_str(), m.artist.as_str(), m.album.as_str()),
            ("Song", "A, B", "Album")
        );
        assert_eq!(m.secs, 215.0);
        assert_eq!(m.image.as_deref(), Some("l"));
        assert_eq!(
            (m.album_id.as_deref(), m.artist_id.as_deref()),
            (Some("al"), Some("a"))
        );
        assert_eq!(t.url(), "spotify://track:abc");
    }

    #[test]
    fn search_kinds_by_name() {
        for k in SearchKind::ALL {
            assert_eq!(SearchKind::from_name(k.name()), Some(k));
        }
        assert_eq!(SearchKind::from_name("episode"), None);
    }
}
