//! Library browsing and search.  The lists come from the same library queries the LMS
//! interfaces use (so the web UI shows what players and Jive clients show: the same
//! artists for the role settings, the same Various Artists handling), turned into typed
//! resources here.

use super::model::{
    Album, AlbumDetail, AlbumRelease, AlbumTags, Artist, FolderEntry, FolderEntryType, Genre, Page,
    Playlist, ReleaseCandidate, ReleaseInfo, ReleaseStatus, ReleaseTrackMatch, SearchResults,
    SimilarArtist, TagEdit, TagEditFailure, TagEditResult, TagValue, Track, TrackTags, Year,
};
use super::state::{int, string, track_from_row};
use crate::server::Server;
use rms_library::{LibQuery, LibQueryName};
use serde::Deserialize;
use serde_json::{Map, Value};
use std::sync::Arc;

/// Default page size.
pub const LIMIT: usize = 100;
/// Largest page size.
pub const MAX_LIMIT: usize = 10_000;

const ARTIST_TAGS: &str = "s4";
const ALBUM_TAGS: &str = "ljyaSwKW";
const TRACK_TAGS: &str = "uasleKcdtiyo";

#[derive(Debug)]
pub enum LibraryError {
    Invalid(String),
    NotFound,
    Failed(String),
}

#[derive(Debug, Deserialize, Default)]
pub struct ListParams {
    #[serde(default)]
    pub offset: usize,
    pub limit: Option<usize>,
    /// Search words.
    pub q: Option<String>,
}

impl ListParams {
    fn limit(&self) -> usize {
        self.limit.unwrap_or(LIMIT).min(MAX_LIMIT)
    }
}

// (No `#[serde(flatten)]` of `ListParams`: query strings only deserialize numbers into
// fields of the top-level struct.)

#[derive(Debug, Deserialize, Default)]
pub struct AlbumParams {
    #[serde(default)]
    pub offset: usize,
    pub limit: Option<usize>,
    pub q: Option<String>,
    /// `title` (default), `artist`, `year`, `added`.
    pub sort: Option<String>,
    pub artist: Option<i64>,
    pub genre: Option<i64>,
    pub year: Option<i64>,
}

#[derive(Debug, Deserialize, Default)]
pub struct FolderParams {
    #[serde(default)]
    pub offset: usize,
    pub limit: Option<usize>,
    /// Folder to list; the top of the music folders without.
    pub id: Option<i64>,
}

impl AlbumParams {
    fn list(&self) -> ListParams {
        ListParams {
            offset: self.offset,
            limit: self.limit,
            q: self.q.clone(),
        }
    }
}

impl FolderParams {
    fn list(&self) -> ListParams {
        ListParams {
            offset: self.offset,
            limit: self.limit,
            q: None,
        }
    }
}

type Row = Map<String, Value>;

async fn query(
    server: &Arc<Server>,
    name: LibQueryName,
    offset: usize,
    limit: usize,
    params: &[(&str, String)],
) -> Result<Row, LibraryError> {
    let mut q = LibQuery::new(name, offset, limit);
    for (k, v) in params {
        q = q.param(k, v);
    }
    let lib = server.library.clone();
    match tokio::task::spawn_blocking(move || lib.query(&q)).await {
        Ok(Ok(row)) => Ok(row),
        Ok(Err(e)) => Err(LibraryError::Invalid(e.to_string())),
        Err(e) => Err(LibraryError::Failed(e.to_string())),
    }
}

/// The `<name>_loop` entries and `count` of a query result.
fn page<T>(row: &Row, loop_name: &str, offset: usize, f: impl Fn(&Row) -> Option<T>) -> Page<T> {
    let items: Vec<T> = row
        .get(loop_name)
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_object())
                .filter_map(f)
                .collect()
        })
        .unwrap_or_default();
    let total = int(row.get("count"))
        .map(|n| n.max(0) as usize)
        .unwrap_or(offset + items.len());
    Page {
        total,
        offset,
        items,
    }
}

fn search_param(q: &Option<String>) -> Option<(&'static str, String)> {
    q.as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| ("search", s.to_string()))
}

fn artist(row: &Row) -> Option<Artist> {
    Some(Artist {
        id: int(row.get("id"))?,
        name: string(row.get("artist")).unwrap_or_default(),
        portrait: string(row.get("portraitid")).map(|p| format!("contributor/{}/image", p)),
        letter: string(row.get("textkey")),
    })
}

fn album(row: &Row) -> Option<Album> {
    let artwork = string(row.get("artwork_url")).or_else(|| {
        string(row.get("artwork_track_id"))
            .filter(|c| c != "0")
            .map(|c| format!("music/{}/cover", c))
    });
    Some(Album {
        id: int(row.get("id"))?,
        title: string(row.get("album")).unwrap_or_default(),
        artist: string(row.get("artist")),
        artist_id: int(row.get("artist_id")),
        year: int(row.get("year")).filter(|&y| y > 0),
        compilation: int(row.get("compilation")).unwrap_or(0) != 0,
        release_type: string(row.get("release_type")).map(|t| t.to_lowercase()),
        artwork,
        letter: string(row.get("textkey")),
    })
}

fn genre(row: &Row) -> Option<Genre> {
    Some(Genre {
        id: int(row.get("id"))?,
        name: string(row.get("genre")).unwrap_or_default(),
    })
}

fn track(row: &Row) -> Option<Track> {
    let url = string(row.get("url"))?;
    Some(track_from_row(row, &url))
}

pub async fn artists(server: &Arc<Server>, p: ListParams) -> Result<Page<Artist>, LibraryError> {
    let mut params = vec![("tags", ARTIST_TAGS.to_string())];
    params.extend(search_param(&p.q));
    let row = query(server, LibQueryName::Artists, p.offset, p.limit(), &params).await?;
    Ok(page(&row, "artists_loop", p.offset, artist))
}

pub async fn artist_by_id(server: &Arc<Server>, id: i64) -> Result<Artist, LibraryError> {
    let params = [
        ("tags", ARTIST_TAGS.to_string()),
        ("artist_id", id.to_string()),
    ];
    let row = query(server, LibQueryName::Artists, 0, 1, &params).await?;
    page(&row, "artists_loop", 0, artist)
        .items
        .into_iter()
        .next()
        .ok_or(LibraryError::NotFound)
}

pub async fn albums(server: &Arc<Server>, p: AlbumParams) -> Result<Page<Album>, LibraryError> {
    let sort = match p.sort.as_deref().unwrap_or("title") {
        "title" => "album",
        "artist" => "artflow",
        "year" => "yearalbum",
        "added" => "new",
        other => {
            return Err(LibraryError::Invalid(format!(
                "unknown sort {:?} (title, artist, year or added)",
                other
            )))
        }
    };
    let mut params = vec![("tags", ALBUM_TAGS.to_string()), ("sort", sort.to_string())];
    params.extend(search_param(&p.q));
    if let Some(a) = p.artist {
        params.push(("artist_id", a.to_string()));
    }
    if let Some(g) = p.genre {
        params.push(("genre_id", g.to_string()));
    }
    if let Some(y) = p.year {
        params.push(("year", y.to_string()));
    }
    let (offset, limit) = (p.offset, p.list().limit());
    let row = query(server, LibQueryName::Albums, offset, limit, &params).await?;
    Ok(page(&row, "albums_loop", offset, album))
}

pub async fn album_by_id(server: &Arc<Server>, id: i64) -> Result<AlbumDetail, LibraryError> {
    let params = [
        ("tags", ALBUM_TAGS.to_string()),
        ("album_id", id.to_string()),
    ];
    let row = query(server, LibQueryName::Albums, 0, 1, &params).await?;
    let album = page(&row, "albums_loop", 0, album)
        .items
        .into_iter()
        .next()
        .ok_or(LibraryError::NotFound)?;
    let params = [
        ("tags", TRACK_TAGS.to_string()),
        ("album_id", id.to_string()),
        ("sort", "tracknum".to_string()),
    ];
    let row = query(server, LibQueryName::Titles, 0, MAX_LIMIT, &params).await?;
    let tracks = page(&row, "titles_loop", 0, track).items;
    let duration = tracks.iter().filter_map(|t| t.duration).sum::<f64>();
    Ok(AlbumDetail {
        album,
        duration: (duration * 1000.0).round() / 1000.0,
        tracks,
    })
}

/// The MusicBrainz release the album was matched to.
pub async fn album_release(server: &Arc<Server>, id: i64) -> Result<AlbumRelease, LibraryError> {
    let lib = server.library.clone();
    tokio::task::spawn_blocking(move || {
        if lib.album_job(id).is_none() {
            return Err(LibraryError::NotFound);
        }
        let Some(m) = lib.album_match(id) else {
            return Ok(AlbumRelease {
                status: ReleaseStatus::Pending,
                how: None,
                score: None,
                release: None,
                tracks: Vec::new(),
                candidates: Vec::new(),
            });
        };
        let status = if m.release_mbid.is_none() {
            ReleaseStatus::None
        } else if m.confident() {
            ReleaseStatus::Matched
        } else {
            ReleaseStatus::Uncertain
        };
        let tracks = m
            .tracks
            .iter()
            .map(|(track_id, pos)| {
                let rt = pos.and_then(|(d, p)| {
                    m.release
                        .as_ref()?
                        .media
                        .iter()
                        .find(|x| x.position == d)?
                        .tracks
                        .iter()
                        .find(|t| t.position == p)
                });
                let some = |s: &str| (!s.is_empty()).then(|| s.to_string());
                ReleaseTrackMatch {
                    track_id: *track_id,
                    disc: pos.map(|p| p.0),
                    position: pos.map(|p| p.1),
                    number: rt.and_then(|t| some(&t.number)),
                    title: rt.and_then(|t| some(&t.title)),
                    artist: rt.and_then(|t| some(&t.artist)),
                    length: rt.and_then(|t| t.length),
                    recording: rt.and_then(|t| some(&t.recording)),
                }
            })
            .collect();
        let mut candidates: Vec<ReleaseCandidate> = m
            .candidates
            .iter()
            .map(|(mbid, score, r)| ReleaseCandidate {
                mbid: mbid.clone(),
                score: *score,
                release: r.as_ref().map(release_info),
            })
            .collect();
        candidates.sort_by(|a, b| b.score.total_cmp(&a.score));
        Ok(AlbumRelease {
            status,
            how: (m.how != "none").then(|| m.how.clone()),
            score: m.score,
            release: m.release.as_ref().map(release_info),
            tracks,
            candidates,
        })
    })
    .await
    .map_err(|e| LibraryError::Failed(e.to_string()))?
}

/// An album's tags and proposals ([`crate::tagedit::proposal`]) as the API has them.
pub fn album_tags(
    server: &Server,
    release: Option<rms_library::releases::Release>,
    tracks: Vec<crate::tagedit::TrackTags>,
) -> AlbumTags {
    let norm = |field: &str, v: &Option<String>| -> Option<String> {
        let v = v.as_deref().map(str::trim).filter(|v| !v.is_empty())?;
        // "03" is track 3
        if field.ends_with("_number") || field.ends_with("_total") {
            if let Ok(n) = v.parse::<i64>() {
                return Some(n.to_string());
            }
        }
        Some(v.to_string())
    };
    AlbumTags {
        release: release.as_ref().map(release_info),
        blocked: crate::tagedit::blocked(server),
        tracks: tracks
            .into_iter()
            .map(|t| TrackTags {
                track_id: t.track_id,
                path: t.path,
                error: t.error,
                tags: t
                    .tags
                    .into_iter()
                    .map(|(field, current, proposed)| TagValue {
                        changed: proposed.is_some()
                            && norm(&field, &proposed) != norm(&field, &current),
                        field,
                        current,
                        proposed,
                    })
                    .collect(),
            })
            .collect(),
    }
}

pub fn tag_edit_result(o: crate::tagedit::Outcome) -> TagEditResult {
    TagEditResult {
        batch: o.batch,
        changed: o.changed as i64,
        failed: o
            .failed
            .into_iter()
            .map(|(track_id, error)| TagEditFailure { track_id, error })
            .collect(),
    }
}

pub fn tag_edits(server: &Server, limit: usize) -> Vec<TagEdit> {
    server
        .library
        .edit_batches(limit.min(1000))
        .into_iter()
        .map(|b| TagEdit {
            batch: b.batch,
            album: b.album,
            files: b.files,
            applied_at: b.applied_at,
            undone_at: b.undone_at,
            kind: b.kind,
        })
        .collect()
}

/// An album's files and where `template` puts them, as the API has them.
pub fn album_files(
    server: &Server,
    template: String,
    plan: Vec<crate::tagedit::Planned>,
) -> super::model::AlbumFiles {
    let rel = |root: &std::path::Path, p: &std::path::Path| {
        p.strip_prefix(root).unwrap_or(p).display().to_string()
    };
    super::model::AlbumFiles {
        blocked: crate::tagedit::blocked(server),
        template,
        files: plan
            .into_iter()
            .map(|p| super::model::FileMove {
                track_id: p.track_id,
                changed: p.from != p.to,
                from: rel(&p.root, &p.from),
                to: rel(&p.root, &p.to),
                error: p.error,
            })
            .collect(),
    }
}

fn release_info(r: &rms_library::releases::Release) -> ReleaseInfo {
    ReleaseInfo {
        mbid: r.mbid.clone(),
        title: r.title.clone(),
        artist: r.artist.clone(),
        date: r.date.clone(),
        original_date: r.original_date.clone(),
        country: r.country.clone(),
        status: r.status.clone(),
        label: r.label.clone(),
        catalog_number: r.catalog_number.clone(),
        barcode: r.barcode.clone(),
        format: r.format(),
        release_type: std::iter::once(&r.primary_type)
            .chain(&r.secondary_types)
            .filter(|t| !t.is_empty())
            .cloned()
            .collect(),
        track_count: r.track_count() as i64,
        disc_count: r.media.len() as i64,
        url: format!("https://musicbrainz.org/release/{}", r.mbid),
    }
}

pub async fn genres(server: &Arc<Server>, p: ListParams) -> Result<Page<Genre>, LibraryError> {
    let params: Vec<_> = search_param(&p.q).into_iter().collect();
    let row = query(server, LibQueryName::Genres, p.offset, p.limit(), &params).await?;
    Ok(page(&row, "genres_loop", p.offset, genre))
}

pub async fn years(server: &Arc<Server>, p: ListParams) -> Result<Page<Year>, LibraryError> {
    let row = query(server, LibQueryName::Years, p.offset, p.limit(), &[]).await?;
    Ok(page(&row, "years_loop", p.offset, |r| {
        Some(Year {
            year: int(r.get("year")).unwrap_or(0).max(0),
        })
    }))
}

pub async fn tracks(server: &Arc<Server>, p: ListParams) -> Result<Page<Track>, LibraryError> {
    let mut params = vec![("tags", TRACK_TAGS.to_string())];
    params.extend(search_param(&p.q));
    let row = query(server, LibQueryName::Titles, p.offset, p.limit(), &params).await?;
    Ok(page(&row, "titles_loop", p.offset, track))
}

pub async fn playlists(
    server: &Arc<Server>,
    p: ListParams,
) -> Result<Page<Playlist>, LibraryError> {
    let params: Vec<_> = search_param(&p.q).into_iter().collect();
    let row = query(
        server,
        LibQueryName::Playlists,
        p.offset,
        p.limit(),
        &params,
    )
    .await?;
    let mut page = page(&row, "playlists_loop", p.offset, |r| {
        Some(Playlist {
            id: int(r.get("id"))?,
            name: string(r.get("playlist")).unwrap_or_default(),
            editable: false,
        })
    });
    let lib = server.library.clone();
    let ids: Vec<i64> = page.items.iter().map(|p| p.id).collect();
    let editable = tokio::task::spawn_blocking(move || {
        ids.iter()
            .map(|id| lib.playlist_editable(*id).unwrap_or(false))
            .collect::<Vec<_>>()
    })
    .await
    .map_err(|e| LibraryError::Failed(e.to_string()))?;
    for (p, e) in page.items.iter_mut().zip(editable) {
        p.editable = e;
    }
    Ok(page)
}

pub async fn playlist_by_id(server: &Arc<Server>, id: i64) -> Result<Playlist, LibraryError> {
    let lib = server.library.clone();
    let found = tokio::task::spawn_blocking(move || {
        lib.playlist_by_id(id)
            .map(|(name, _, _)| (name, lib.playlist_editable(id).unwrap_or(false)))
    })
    .await
    .map_err(|e| LibraryError::Failed(e.to_string()))?;
    let (name, editable) = found.ok_or(LibraryError::NotFound)?;
    Ok(Playlist { id, name, editable })
}

/// A library track by id.
async fn track_by_id(server: &Arc<Server>, id: i64) -> Option<Track> {
    let params = [
        ("tags", TRACK_TAGS.to_string()),
        ("track_id", id.to_string()),
    ];
    let row = query(server, LibQueryName::Titles, 0, 1, &params)
        .await
        .ok()?;
    page(&row, "titles_loop", 0, track).items.into_iter().next()
}

/// The artist's most played tracks.
pub async fn top_tracks(server: &Arc<Server>, id: i64) -> Result<Vec<Track>, LibraryError> {
    artist_by_id(server, id).await?;
    let lib = server.library.clone();
    let ids = tokio::task::spawn_blocking(move || lib.most_played_by_artist(id, 10))
        .await
        .map_err(|e| LibraryError::Failed(e.to_string()))?;
    let mut out = Vec::new();
    for t in ids {
        out.extend(track_by_id(server, t).await);
    }
    Ok(out)
}

/// Artists ListenBrainz finds similar (with discovery or a ListenBrainz account on; none
/// without), the library's first.
pub async fn similar_artists(
    server: &Arc<Server>,
    id: i64,
) -> Result<Vec<SimilarArtist>, LibraryError> {
    artist_by_id(server, id).await?;
    let lib = server.library.clone();
    let Some(mbid) = tokio::task::spawn_blocking(move || lib.artist_mbid(id))
        .await
        .map_err(|e| LibraryError::Failed(e.to_string()))?
    else {
        return Ok(Vec::new());
    };
    let found = crate::plugins::listenbrainz::similar_artists(server, &mbid).await;
    let mut out = Vec::new();
    for (name, m) in found {
        let lib = server.library.clone();
        let n = name.clone();
        let local = tokio::task::spawn_blocking(move || lib.artist_in_library(Some(&m), &n))
            .await
            .ok()
            .flatten()
            .filter(|a| *a != id);
        let artist = match local {
            Some(a) => artist_by_id(server, a).await.ok(),
            None => None,
        };
        out.push(SimilarArtist { name, artist });
    }
    // what is in the library first, keeping ListenBrainz's order otherwise
    out.sort_by_key(|s| s.artist.is_none());
    Ok(out)
}

pub async fn playlist_tracks(
    server: &Arc<Server>,
    id: i64,
    p: ListParams,
) -> Result<Page<Track>, LibraryError> {
    playlist_by_id(server, id).await?;
    let params = [
        ("tags", TRACK_TAGS.to_string()),
        ("playlist_id", id.to_string()),
    ];
    let row = query(
        server,
        LibQueryName::PlaylistTracks,
        p.offset,
        p.limit(),
        &params,
    )
    .await?;
    Ok(page(&row, "playlisttracks_loop", p.offset, track))
}

pub async fn folder(
    server: &Arc<Server>,
    p: FolderParams,
) -> Result<Page<FolderEntry>, LibraryError> {
    let mut params = vec![];
    if let Some(id) = p.id {
        params.push(("folder_id", id.to_string()));
    }
    let (offset, limit) = (p.offset, p.list().limit());
    let row = query(server, LibQueryName::MusicFolder, offset, limit, &params).await?;
    let mut entries = page(&row, "folder_loop", offset, |r| {
        let kind = match r.get("type").and_then(Value::as_str) {
            Some("folder") => FolderEntryType::Folder,
            Some("track") => FolderEntryType::Track,
            Some("playlist") => FolderEntryType::Playlist,
            _ => FolderEntryType::Other,
        };
        Some(FolderEntry {
            kind,
            id: int(r.get("id"))?,
            name: string(r.get("filename")).unwrap_or_default(),
            track: None,
        })
    });
    // tracks with their tags, as everywhere else
    let ids: Vec<i64> = entries
        .items
        .iter()
        .filter(|e| e.kind == FolderEntryType::Track)
        .map(|e| e.id)
        .collect();
    let lib = server.library.clone();
    let rows = tokio::task::spawn_blocking(move || lib.track_rows(&ids, TRACK_TAGS))
        .await
        .map_err(|e| LibraryError::Failed(e.to_string()))?;
    let mut tracks: std::collections::HashMap<i64, Track> = rows
        .iter()
        .filter_map(|r| Some((int(r.get("id"))?, track(r)?)))
        .collect();
    for e in &mut entries.items {
        if e.kind == FolderEntryType::Track {
            e.track = tracks.remove(&e.id);
        }
    }
    Ok(entries)
}

/// Artists, albums, tracks and genres matching all the words of `q`, `limit` of each.
pub async fn search(server: &Arc<Server>, p: ListParams) -> Result<SearchResults, LibraryError> {
    if search_param(&p.q).is_none() {
        return Err(LibraryError::Invalid("q is missing".into()));
    }
    let list = || ListParams {
        offset: p.offset,
        limit: Some(p.limit.unwrap_or(20)),
        q: p.q.clone(),
    };
    let albums_params = AlbumParams {
        offset: p.offset,
        limit: Some(p.limit.unwrap_or(20)),
        q: p.q.clone(),
        ..Default::default()
    };
    let (artists, albums, tracks, genres) = tokio::try_join!(
        artists(server, list()),
        albums(server, albums_params),
        tracks(server, list()),
        genres(server, list()),
    )?;
    Ok(SearchResults {
        artists,
        albums,
        tracks,
        genres,
    })
}
