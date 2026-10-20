//! Which MusicBrainz release each album is: album matching.  Identifying recordings one by
//! one ([`crate::musicbrainz`]) finds *a* recording of each song, but not the edition owned:
//! the album, its year, the track numbers and discs come from the release, so an album is
//! matched as a whole.  The server gathers candidate releases (`plugins::musicbrainz`); here
//! they are kept ([`Release`], table `mb_releases`), scored against the album's tracks
//! ([`score`]) and the best one remembered for each track (`track_release`, by audio
//! fingerprint, so a match survives wipes and follows moved or retagged files).
//!
//! A confident match fills in what the tags leave out ([`Library::apply_releases`]): the
//! album's MusicBrainz id and year, the tracks' MusicBrainz ids, numbers and discs.  Tags
//! always win; files are never written.

use crate::Library;
use rusqlite::{params, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;

/// A match scoring this (0–1), or chosen by the user or the tags, fills in the library.
pub const CONFIDENT: f64 = 0.8;
/// Below this, a release is not a match at all.
pub const MIN_SCORE: f64 = 0.5;
/// An album without a match is tried again after this long (MusicBrainz grows).
const RETRY_SECS: i64 = 90 * 86400;
/// The album's title and the release's must be this alike (0–1) for a confident match.
pub const TITLE_AGREES: f64 = 0.75;
/// A track and a release track below this are not paired.
const MIN_PAIR: f64 = 0.5;

/// A MusicBrainz release, as much of it as matching and tagging need.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Release {
    pub mbid: String,
    pub title: String,
    /// The artist credit as printed ("A feat. B").
    pub artist: String,
    pub artist_mbids: Vec<String>,
    /// `YYYY`, `YYYY-MM` or `YYYY-MM-DD`; empty when unknown.
    pub date: String,
    pub country: String,
    /// `Official`, `Promotion`, `Bootleg`, …
    pub status: String,
    pub barcode: String,
    pub label: String,
    pub catalog_number: String,
    pub release_group: String,
    /// `Album`, `Single`, `EP`, …
    pub primary_type: String,
    pub secondary_types: Vec<String>,
    /// The release group's first release date: the original year.
    pub original_date: String,
    pub media: Vec<Medium>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Medium {
    /// 1-based disc number.
    pub position: i64,
    /// `CD`, `Digital Media`, `12" Vinyl`, …
    pub format: String,
    pub title: String,
    pub tracks: Vec<ReleaseTrack>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ReleaseTrack {
    /// The MusicBrainz track id (the recording's appearance on this release).
    pub mbid: String,
    pub recording: String,
    /// 1-based position on the medium.
    pub position: i64,
    /// As printed: "A1", "3", …
    pub number: String,
    pub title: String,
    pub artist: String,
    #[serde(default)]
    pub artist_mbids: Vec<String>,
    /// Seconds.
    pub length: Option<f64>,
}

impl Release {
    /// From MusicBrainz's JSON for `/ws/2/release/<id>?inc=recordings+artist-credits+
    /// release-groups+labels`.
    pub fn from_json(v: &Value) -> Option<Release> {
        let s = |v: &Value| v.as_str().unwrap_or_default().to_string();
        let rg = &v["release-group"];
        let label = v["label-info"].as_array().and_then(|l| l.first());
        Some(Release {
            mbid: v["id"].as_str()?.to_string(),
            title: s(&v["title"]),
            artist: credit(&v["artist-credit"]),
            artist_mbids: credit_ids(&v["artist-credit"]),
            date: s(&v["date"]),
            country: s(&v["country"]),
            status: s(&v["status"]),
            barcode: s(&v["barcode"]),
            label: label.map(|l| s(&l["label"]["name"])).unwrap_or_default(),
            catalog_number: label.map(|l| s(&l["catalog-number"])).unwrap_or_default(),
            release_group: s(&rg["id"]),
            primary_type: s(&rg["primary-type"]),
            secondary_types: rg["secondary-types"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|t| t.as_str().map(str::to_string))
                .collect(),
            original_date: s(&rg["first-release-date"]),
            media: v["media"]
                .as_array()
                .into_iter()
                .flatten()
                .enumerate()
                .map(|(i, m)| Medium {
                    position: m["position"].as_i64().unwrap_or(i as i64 + 1),
                    format: s(&m["format"]),
                    title: s(&m["title"]),
                    tracks: m["tracks"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .enumerate()
                        .map(|(j, t)| ReleaseTrack {
                            mbid: s(&t["id"]),
                            recording: s(&t["recording"]["id"]),
                            position: t["position"].as_i64().unwrap_or(j as i64 + 1),
                            number: s(&t["number"]),
                            title: t["title"]
                                .as_str()
                                .or(t["recording"]["title"].as_str())
                                .unwrap_or_default()
                                .to_string(),
                            artist: {
                                let a = credit(&t["artist-credit"]);
                                if a.is_empty() {
                                    credit(&t["recording"]["artist-credit"])
                                } else {
                                    a
                                }
                            },
                            artist_mbids: {
                                let c =
                                    if t["artist-credit"].as_array().is_some_and(|a| !a.is_empty())
                                    {
                                        &t["artist-credit"]
                                    } else {
                                        &t["recording"]["artist-credit"]
                                    };
                                credit_ids(c)
                            },
                            length: t["length"]
                                .as_f64()
                                .or(t["recording"]["length"].as_f64())
                                .map(|ms| ms / 1000.0),
                        })
                        .collect(),
                })
                .collect(),
        })
    }

    pub fn track_count(&self) -> usize {
        self.media.iter().map(|m| m.tracks.len()).sum()
    }

    /// The year of release, or else the original year.
    pub fn year(&self) -> Option<i64> {
        [&self.date, &self.original_date]
            .iter()
            .find_map(|d| d.get(..4).and_then(|y| y.parse().ok()))
            .filter(|y| *y > 0)
    }

    /// The media's formats, "2×CD" style.
    pub fn format(&self) -> String {
        let mut out: Vec<(String, usize)> = Vec::new();
        for m in &self.media {
            let f = if m.format.is_empty() {
                "(unknown)".to_string()
            } else {
                m.format.clone()
            };
            match out.iter_mut().find(|(g, _)| *g == f) {
                Some((_, n)) => *n += 1,
                None => out.push((f, 1)),
            }
        }
        out.iter()
            .map(|(f, n)| {
                if *n > 1 {
                    format!("{n}×{f}")
                } else {
                    f.clone()
                }
            })
            .collect::<Vec<_>>()
            .join(" + ")
    }

    fn track(&self, medium: i64, position: i64) -> Option<&ReleaseTrack> {
        self.media
            .iter()
            .find(|m| m.position == medium)?
            .tracks
            .iter()
            .find(|t| t.position == position)
    }
}

/// The ids of the artists in an artist credit.
fn credit_ids(v: &Value) -> Vec<String> {
    v.as_array()
        .into_iter()
        .flatten()
        .filter_map(|c| c["artist"]["id"].as_str().map(str::to_string))
        .collect()
}

/// An artist credit as printed: names and join phrases.
fn credit(v: &Value) -> String {
    v.as_array()
        .into_iter()
        .flatten()
        .map(|c| {
            format!(
                "{}{}",
                c["name"]
                    .as_str()
                    .or(c["artist"]["name"].as_str())
                    .unwrap_or_default(),
                c["joinphrase"].as_str().unwrap_or_default()
            )
        })
        .collect()
}

/// An album to match: its tracks as the tags have them.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct AlbumJob {
    pub album_id: i64,
    pub title: String,
    /// The album artist ("Various Artists" for a compilation).
    pub artist: String,
    pub compilation: bool,
    /// The release id in the tags (`MUSICBRAINZ_ALBUMID`).
    pub release_tag: Option<String>,
    /// A release the user chose for the album: matched again only against it.
    pub chosen: Option<String>,
    pub tracks: Vec<MatchTrack>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct MatchTrack {
    pub fingerprint: String,
    pub title: String,
    pub artist: String,
    pub number: Option<i64>,
    pub disc: Option<i64>,
    /// Seconds.
    pub duration: Option<f64>,
    /// MusicBrainz recording id (tags, or identified).
    pub recording: Option<String>,
}

impl AlbumJob {
    /// Recording ids known for the album's tracks, most useful first (spread over the album).
    pub fn recordings(&self) -> Vec<&str> {
        let ids: Vec<&str> = self
            .tracks
            .iter()
            .filter_map(|t| t.recording.as_deref())
            .collect();
        // first, middle, last: a different song each, in case one is on many releases
        let mut out: Vec<&str> = Vec::new();
        for i in [0, ids.len() / 2, ids.len().saturating_sub(1)] {
            if let Some(id) = ids.get(i) {
                if !out.contains(id) {
                    out.push(id);
                }
            }
        }
        out
    }
}

/// How well a release fits an album, and which release track each of its tracks is.
#[derive(Debug, Clone, PartialEq)]
pub struct Scored {
    /// 0–1.
    pub score: f64,
    /// For each of the album's tracks, in order: (medium, position) on the release.
    pub tracks: Vec<Option<(i64, i64)>>,
}

/// Score `release` as `job`'s release.  Each track is paired with the release track it most
/// likely is (the same recording, else by title, length and position); the score weighs how
/// well the tracks pair up, how much of the release the album holds, and the album's title and
/// artist.
pub fn score(job: &AlbumJob, release: &Release) -> Scored {
    let flat: Vec<(i64, &ReleaseTrack)> = release
        .media
        .iter()
        .flat_map(|m| m.tracks.iter().map(move |t| (m.position, t)))
        .collect();
    let discs = release.media.len();
    let mut pairs: Vec<(f64, usize, usize)> = Vec::new();
    for (i, t) in job.tracks.iter().enumerate() {
        for (j, (medium, r)) in flat.iter().enumerate() {
            let s = pair_score(t, *medium, r, discs);
            if s >= MIN_PAIR {
                pairs.push((s, i, j));
            }
        }
    }
    // best pairs first; each track and release track used once
    pairs.sort_by(|a, b| b.0.total_cmp(&a.0).then(a.1.cmp(&b.1)).then(a.2.cmp(&b.2)));
    let mut taken = vec![false; flat.len()];
    let mut assigned: Vec<Option<(f64, usize)>> = vec![None; job.tracks.len()];
    for (s, i, j) in pairs {
        if assigned[i].is_none() && !taken[j] {
            assigned[i] = Some((s, j));
            taken[j] = true;
        }
    }
    let n = job.tracks.len().max(1) as f64;
    let paired = assigned.iter().flatten().count();
    let tracks_part = assigned.iter().flatten().map(|(s, _)| s).sum::<f64>() / n;
    let coverage = paired as f64 / flat.len().max(1) as f64;
    let title = title_similarity(&job.title, &release.title);
    let artist = if job.artist.is_empty() {
        0.5
    } else {
        similarity(&job.artist, &release.artist)
    };
    // How much of the release the album holds says little for an album of a track or two (a
    // one-track single holds all of one track): its weight goes to the title then.
    let small = ((job.tracks.len() as f64 - 1.0) / 3.0).clamp(0.0, 1.0);
    // (just under 1: the nudges below must still tell equal fits apart)
    let mut score = 0.99
        * (0.6 * tracks_part
            + 0.15 * small * coverage
            + (0.15 + 0.15 * (1.0 - small)) * title
            + 0.1 * artist);
    // the tags name the album: a release by another name is not surely it
    if title < TITLE_AGREES {
        score = score.min(CONFIDENT - 0.01);
    }
    // among releases with the same track list (reissues, other countries), a nudge to the
    // official one with a date; for a lone track, to the album over the single of the song
    if release.status.eq_ignore_ascii_case("official") {
        score += 0.004;
    }
    if !release.date.is_empty() {
        score += 0.001;
    }
    if release.primary_type == "Album" && release.secondary_types.is_empty() {
        score += 0.003;
    }
    Scored {
        score: score.min(1.0),
        tracks: assigned
            .iter()
            .map(|a| a.map(|(_, j)| (flat[j].0, flat[j].1.position)))
            .collect(),
    }
}

/// How likely `t` is the release track `r` (on disc `medium` of `discs`).
fn pair_score(t: &MatchTrack, medium: i64, r: &ReleaseTrack, discs: usize) -> f64 {
    let same_recording = match (t.recording.as_deref(), r.recording.as_str()) {
        (Some(a), b) if !b.is_empty() => Some(a == b),
        _ => None,
    };
    if same_recording == Some(true) {
        return 1.0;
    }
    let title = title_similarity(&t.title, &r.title);
    let length = match (t.duration, r.length) {
        (Some(a), Some(b)) => {
            let d = (a - b).abs();
            if d <= 3.0 {
                1.0
            } else {
                (1.0 - (d - 3.0) / 12.0).max(0.0)
            }
        }
        _ => 0.6,
    };
    let disc_ok = match t.disc {
        Some(d) => d == medium,
        None => discs == 1 || medium == 1,
    };
    let position = if t.number == Some(r.position) && disc_ok {
        1.0
    } else {
        0.0
    };
    let s = 0.55 * title + 0.3 * length + 0.15 * position;
    // another recording of the same song (a live take, a remix) is less likely this track
    if same_recording == Some(false) {
        s * 0.85
    } else {
        s
    }
}

/// Lower case, letters and digits only, words separated by single spaces, "&" as "and".
pub fn normalize(s: &str) -> String {
    let ascii = deunicode::deunicode(s).to_lowercase().replace('&', " and ");
    ascii
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|w| !w.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

/// 0–1: how alike two names are.
pub fn similarity(a: &str, b: &str) -> f64 {
    let (a, b) = (normalize(a), normalize(b));
    if a.is_empty() && b.is_empty() {
        return 1.0;
    }
    strsim::normalized_levenshtein(&a, &b)
}

/// [`similarity`] of two titles, also without a bracketed or dashed addition on either side
/// ("Song (Remastered 2011)", "Song - Live").
pub fn title_similarity(a: &str, b: &str) -> f64 {
    let full = similarity(a, b);
    let (sa, sb) = (strip_addition(a), strip_addition(b));
    if sa == a && sb == b {
        return full;
    }
    // a little less than a full match: the addition may matter ("Live", "Demo")
    full.max(0.95 * similarity(sa, sb))
}

fn strip_addition(s: &str) -> &str {
    let cut = [" (", " [", " - "]
        .iter()
        .filter_map(|p| s.find(p))
        .filter(|i| *i > 0)
        .min();
    match cut {
        Some(i) => s[..i].trim_end(),
        None => s,
    }
}

/// The album's match, as the API shows it.
#[derive(Debug, Clone, PartialEq)]
pub struct AlbumMatch {
    pub release: Option<Release>,
    pub release_mbid: Option<String>,
    pub score: Option<f64>,
    /// `tag` (the tags name the release), `auto`, `user` (chosen), `none` (nothing found).
    pub how: String,
    pub matched_at: i64,
    /// Library track id → (medium, position) on the release.
    pub tracks: Vec<(i64, Option<(i64, i64)>)>,
    /// Other releases considered: id, score and (when known) the release.
    pub candidates: Vec<(String, f64, Option<Release>)>,
}

impl AlbumMatch {
    pub fn confident(&self) -> bool {
        self.release_mbid.is_some()
            && (self.how == "user" || self.how == "tag" || self.score.unwrap_or(0.0) >= CONFIDENT)
    }
}

/// How far album matching got.
#[derive(Debug, Clone, Copy, Default, PartialEq, serde::Serialize)]
pub struct ReleaseTotals {
    /// Albums matched to a release confidently.
    pub matched: i64,
    /// …and to one only possibly: to check.
    pub uncertain: i64,
    /// Albums waiting to be matched.
    pub pending: i64,
}

/// The albums worth matching: with an album title, and some tracks of known audio.
const ALBUMS: &str = "SELECT al.id FROM albums al
     WHERE al.title_search != ?1
       AND EXISTS (SELECT 1 FROM tracks t WHERE t.album_id = al.id AND t.fingerprint IS NOT NULL)";

/// Of those, the ones to match now: a track never matched (new files), or nothing found a
/// while ago; and every track's recording looked up first, for its MusicBrainz id.
const PENDING: &str = "
       AND EXISTS (SELECT 1 FROM tracks t
                   LEFT JOIN track_release r ON r.fingerprint = t.fingerprint
                   WHERE t.album_id = al.id AND t.fingerprint IS NOT NULL
                     AND (r.fingerprint IS NULL OR (r.how = 'none' AND r.matched_at < ?2)))
       AND NOT EXISTS (SELECT 1 FROM tracks t
                       LEFT JOIN track_musicbrainz m ON m.fingerprint = t.fingerprint
                       WHERE t.album_id = al.id AND t.fingerprint IS NOT NULL
                         AND m.fingerprint IS NULL)";

impl Library {
    pub(crate) fn no_album_key(&self) -> String {
        let cfg = self.config();
        crate::text::TextRules::new(&cfg.ignored_articles, &cfg.split_list)
            .search_key(&cfg.no_album_string)
    }

    /// Up to `n` albums to match to a release.
    pub fn release_pending(&self, n: usize) -> Vec<AlbumJob> {
        let Ok(c) = self.conn() else {
            return Vec::new();
        };
        let cutoff = crate::scanner::now() - RETRY_SECS;
        let ids: Vec<i64> = c
            .prepare(&format!("{ALBUMS}{PENDING} ORDER BY al.id LIMIT {n}"))
            .and_then(|mut st| {
                st.query_map(params![self.no_album_key(), cutoff], |r| r.get(0))?
                    .collect()
            })
            .unwrap_or_default();
        ids.into_iter()
            .filter_map(|id| album_job(&c, id).ok().flatten())
            .collect()
    }

    /// The album as a job, whether or not it is pending (to match it against a release the
    /// user picks).
    pub fn album_job(&self, album_id: i64) -> Option<AlbumJob> {
        album_job(&*self.conn().ok()?, album_id).ok().flatten()
    }

    /// Keep a release fetched from MusicBrainz.
    pub fn store_release(&self, r: &Release) -> anyhow::Result<()> {
        self.conn()?.execute(
            "INSERT OR REPLACE INTO mb_releases (mbid, body, fetched_at) VALUES (?, ?, ?)",
            params![r.mbid, serde_json::to_string(r)?, crate::scanner::now()],
        )?;
        Ok(())
    }

    /// A release kept by [`Library::store_release`], if fetched within `max_age` seconds.
    pub fn cached_release(&self, mbid: &str, max_age: i64) -> Option<Release> {
        let c = self.conn().ok()?;
        cached_release(&c, mbid, crate::scanner::now() - max_age)
    }

    /// Remember the album's match: `best` (release and how it scored, `how` as in
    /// [`AlbumMatch::how`]), or that none was found; `candidates`: every release scored.
    pub fn store_album_match(
        &self,
        job: &AlbumJob,
        best: Option<(&Release, &Scored)>,
        how: &str,
        candidates: &[(String, f64)],
    ) -> anyhow::Result<()> {
        let mut c = self.conn()?;
        let tx = c.transaction()?;
        let now = crate::scanner::now();
        let cands = serde_json::to_string(
            &candidates
                .iter()
                .map(|(m, s)| serde_json::json!({"mbid": m, "score": round3(*s)}))
                .collect::<Vec<_>>(),
        )?;
        for (i, t) in job.tracks.iter().enumerate() {
            let (mbid, score, pos, how) = match best {
                Some((r, s)) => (
                    Some(r.mbid.as_str()),
                    Some(round3(s.score)),
                    s.tracks[i],
                    how,
                ),
                None => (None, None, None, "none"),
            };
            tx.execute(
                "INSERT OR REPLACE INTO track_release
                   (fingerprint, release_mbid, medium, position, score, how, candidates, matched_at)
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
                params![
                    t.fingerprint,
                    mbid,
                    pos.map(|p| p.0),
                    pos.map(|p| p.1),
                    score,
                    how,
                    cands,
                    now
                ],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    /// Forget the album's match, so it is matched again from scratch.
    pub fn forget_album_match(&self, album_id: i64) -> anyhow::Result<()> {
        self.conn()?.execute(
            "DELETE FROM track_release WHERE fingerprint IN
               (SELECT fingerprint FROM tracks WHERE album_id = ? AND fingerprint IS NOT NULL)",
            [album_id],
        )?;
        Ok(())
    }

    /// The album's release, as matched; `None` while it has not been matched.
    pub fn album_match(&self, album_id: i64) -> Option<AlbumMatch> {
        let c = self.conn().ok()?;
        album_match(&c, album_id, true).ok().flatten()
    }

    pub fn release_totals(&self) -> ReleaseTotals {
        let Ok(c) = self.conn() else {
            return ReleaseTotals::default();
        };
        let cutoff = crate::scanner::now() - RETRY_SECS;
        c.query_row(
            &format!(
                "WITH a AS (
                   SELECT MAX(r.release_mbid IS NOT NULL
                              AND (r.how IN ('user', 'tag') OR r.score >= {CONFIDENT})) AS confident,
                          MAX(r.release_mbid IS NOT NULL) AS matched,
                          MAX(r.fingerprint IS NULL
                              OR (r.how = 'none' AND r.matched_at < ?2)) AS pending
                   FROM albums al
                   JOIN tracks t ON t.album_id = al.id AND t.fingerprint IS NOT NULL
                   LEFT JOIN track_release r ON r.fingerprint = t.fingerprint
                   WHERE al.title_search != ?1
                   GROUP BY al.id)
                 SELECT COALESCE(SUM(confident), 0), COALESCE(SUM(matched AND NOT confident), 0),
                        COALESCE(SUM(pending), 0)
                 FROM a"
            ),
            params![self.no_album_key(), cutoff],
            |r| {
                Ok(ReleaseTotals {
                    matched: r.get(0)?,
                    uncertain: r.get(1)?,
                    pending: r.get(2)?,
                })
            },
        )
        .unwrap_or_default()
    }

    /// Fill in from confidently matched releases what the tags leave out: the albums'
    /// MusicBrainz ids and years, the tracks' MusicBrainz ids, numbers and discs.  Returns the
    /// tracks and albums changed.
    pub fn apply_releases(&self) -> usize {
        let Ok(c) = self.conn() else { return 0 };
        let n = apply_releases(&c).unwrap_or_else(|e| {
            tracing::warn!("filling in from MusicBrainz releases: {e}");
            0
        });
        drop(c);
        n + self.apply_release_art()
    }
}

fn round3(x: f64) -> f64 {
    (x * 1000.0).round() / 1000.0
}

pub(crate) fn cached_release(c: &rusqlite::Connection, mbid: &str, since: i64) -> Option<Release> {
    let body: String = c
        .query_row(
            "SELECT body FROM mb_releases WHERE mbid = ? AND fetched_at >= ?",
            params![mbid, since],
            |r| r.get(0),
        )
        .optional()
        .ok()??;
    serde_json::from_str(&body).ok()
}

fn album_job(c: &rusqlite::Connection, id: i64) -> rusqlite::Result<Option<AlbumJob>> {
    let head = c
        .query_row(
            "SELECT al.title, a.name, al.compilation, al.mbid FROM albums al
             LEFT JOIN artists a ON a.id = al.artist_id WHERE al.id = ?",
            [id],
            |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, Option<String>>(1)?,
                    r.get::<_, Option<i64>>(2)?,
                    r.get::<_, Option<String>>(3)?,
                ))
            },
        )
        .optional()?;
    let Some((title, artist, compilation, release_tag)) = head else {
        return Ok(None);
    };
    let mut st = c.prepare(
        "SELECT t.fingerprint, t.title, a.name, t.track_number, t.disc, t.duration,
                COALESCE(t.mbid, m.mbid), r.how, r.release_mbid
         FROM tracks t
         LEFT JOIN artists a ON a.id = t.artist_id
         LEFT JOIN track_musicbrainz m ON m.fingerprint = t.fingerprint
         LEFT JOIN track_release r ON r.fingerprint = t.fingerprint
         WHERE t.album_id = ? AND t.fingerprint IS NOT NULL
         GROUP BY t.fingerprint
         ORDER BY t.disc, t.track_number, t.title_sort",
    )?;
    let mut chosen = None;
    let tracks = st
        .query_map([id], |r| {
            if r.get::<_, Option<String>>(7)?.as_deref() == Some("user") {
                chosen = r.get::<_, Option<String>>(8)?;
            }
            Ok(MatchTrack {
                fingerprint: r.get(0)?,
                title: r.get(1)?,
                artist: r.get::<_, Option<String>>(2)?.unwrap_or_default(),
                number: r.get(3)?,
                disc: r.get(4)?,
                duration: r.get(5)?,
                recording: r.get(6)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    if tracks.is_empty() {
        return Ok(None);
    }
    Ok(Some(AlbumJob {
        album_id: id,
        title,
        artist: artist.unwrap_or_default(),
        compilation: compilation == Some(1),
        release_tag: release_tag.filter(|m| !m.trim().is_empty()),
        chosen,
        tracks,
    }))
}

/// The album's match; the other releases considered too with `candidates`.
fn album_match(
    c: &rusqlite::Connection,
    album_id: i64,
    candidates: bool,
) -> rusqlite::Result<Option<AlbumMatch>> {
    let mut st = c.prepare(
        "SELECT t.id, r.release_mbid, r.medium, r.position, r.score, r.how, r.candidates,
                r.matched_at
         FROM tracks t JOIN track_release r ON r.fingerprint = t.fingerprint
         WHERE t.album_id = ?
         ORDER BY t.disc, t.track_number, t.title_sort",
    )?;
    type Row = (
        i64,
        Option<String>,
        Option<i64>,
        Option<i64>,
        Option<f64>,
        String,
        Option<String>,
        i64,
    );
    let rows: Vec<Row> = st
        .query_map([album_id], |r| {
            Ok((
                r.get(0)?,
                r.get(1)?,
                r.get(2)?,
                r.get(3)?,
                r.get(4)?,
                r.get(5)?,
                r.get(6)?,
                r.get(7)?,
            ))
        })?
        .collect::<rusqlite::Result<_>>()?;
    // the release most of the album's tracks have (all of them, unless a track is shared
    // with another album)
    let mut votes: HashMap<Option<&str>, usize> = HashMap::new();
    for r in &rows {
        *votes.entry(r.1.as_deref()).or_default() += 1;
    }
    let Some((mbid, _)) = votes.into_iter().max_by_key(|(m, n)| (*n, m.is_some())) else {
        return Ok(None);
    };
    let mbid = mbid.map(str::to_string);
    let Some(first) = rows.iter().find(|r| r.1 == mbid) else {
        return Ok(None);
    };
    let release = mbid.as_deref().and_then(|m| cached_release(c, m, 0));
    let candidates = first
        .6
        .as_deref()
        .filter(|_| candidates)
        .and_then(|s| serde_json::from_str::<Vec<Value>>(s).ok())
        .unwrap_or_default()
        .into_iter()
        .filter_map(|v| {
            let m = v["mbid"].as_str()?.to_string();
            let r = cached_release(c, &m, 0);
            Some((m, v["score"].as_f64().unwrap_or(0.0), r))
        })
        .collect();
    Ok(Some(AlbumMatch {
        release,
        release_mbid: mbid.clone(),
        score: first.4,
        how: first.5.clone(),
        matched_at: first.7,
        tracks: rows
            .iter()
            .map(|r| {
                let pos = if r.1 == mbid { r.2.zip(r.3) } else { None };
                (r.0, pos)
            })
            .collect(),
        candidates,
    }))
}

fn apply_releases(c: &rusqlite::Connection) -> rusqlite::Result<usize> {
    // albums with a confident match and something to fill in
    let albums: Vec<i64> = c
        .prepare(&format!(
            "SELECT DISTINCT t.album_id FROM tracks t
             JOIN track_release r ON r.fingerprint = t.fingerprint
             JOIN albums al ON al.id = t.album_id
             WHERE r.release_mbid IS NOT NULL
               AND (r.how IN ('user', 'tag') OR r.score >= {CONFIDENT})
               AND (al.mbid IS NULL OR al.year = 0
                    OR (r.position IS NOT NULL
                        AND (t.mbid IS NULL OR t.track_number IS NULL
                             OR (t.disc IS NULL AND r.medium > 1))))"
        ))?
        .query_map([], |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    let mut n = 0;
    let mut releases: HashMap<String, Option<Release>> = HashMap::new();
    for album in albums {
        let Some(m) = album_match(c, album, false)? else {
            continue;
        };
        if !m.confident() {
            continue;
        }
        let Some(mbid) = m.release_mbid.clone() else {
            continue;
        };
        let release = releases
            .entry(mbid.clone())
            .or_insert_with(|| cached_release(c, &mbid, 0));
        let Some(release) = release else { continue };
        n += c.execute(
            "UPDATE albums SET mbid = ? WHERE id = ? AND mbid IS NULL",
            params![mbid, album],
        )?;
        if let Some(y) = release.year() {
            n += c.execute(
                "UPDATE albums SET year = ? WHERE id = ? AND year = 0",
                params![y, album],
            )?;
        }
        let several = release.media.len() > 1;
        for (track, pos) in &m.tracks {
            let Some((medium, position)) = pos else {
                continue;
            };
            let Some(rt) = release.track(*medium, *position) else {
                continue;
            };
            n += c.execute(
                "UPDATE tracks SET mbid = COALESCE(mbid, ?2),
                                   track_number = COALESCE(track_number, ?3),
                                   disc = COALESCE(disc, ?4)
                 WHERE id = ?1
                   AND (mbid IS NULL AND ?2 IS NOT NULL OR track_number IS NULL
                        OR disc IS NULL AND ?4 IS NOT NULL)",
                params![
                    track,
                    (!rt.recording.is_empty()).then_some(&rt.recording),
                    position,
                    several.then_some(*medium)
                ],
            )?;
        }
    }
    Ok(n)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn release(id: &str, title: &str, tracks: &[(&str, f64)]) -> Release {
        Release {
            mbid: id.into(),
            title: title.into(),
            artist: "Nirvana".into(),
            status: "Official".into(),
            date: "1991-09-24".into(),
            media: vec![Medium {
                position: 1,
                format: "CD".into(),
                tracks: tracks
                    .iter()
                    .enumerate()
                    .map(|(i, (t, len))| ReleaseTrack {
                        mbid: format!("{id}-t{i}"),
                        recording: format!("rec-{t}"),
                        position: i as i64 + 1,
                        number: (i + 1).to_string(),
                        title: t.to_string(),
                        length: Some(*len),
                        ..Default::default()
                    })
                    .collect(),
                ..Default::default()
            }],
            ..Default::default()
        }
    }

    fn job(tracks: &[(&str, f64, Option<i64>)]) -> AlbumJob {
        AlbumJob {
            album_id: 1,
            title: "Nevermind".into(),
            artist: "Nirvana".into(),
            tracks: tracks
                .iter()
                .enumerate()
                .map(|(i, (t, d, n))| MatchTrack {
                    fingerprint: format!("f{i}"),
                    title: t.to_string(),
                    duration: Some(*d),
                    number: *n,
                    ..Default::default()
                })
                .collect(),
            ..Default::default()
        }
    }

    const NEVERMIND: &[(&str, f64)] = &[
        ("Smells Like Teen Spirit", 301.0),
        ("In Bloom", 255.0),
        ("Come as You Are", 219.0),
        ("Breed", 184.0),
    ];

    #[test]
    fn the_whole_album_beats_a_compilation_with_some_of_its_songs() {
        let album = job(&[
            ("Smells Like Teen Spirit", 301.5, Some(1)),
            ("In Bloom", 254.0, Some(2)),
            ("Come As You Are", 219.0, Some(3)),
            ("Breed", 183.0, Some(4)),
        ]);
        let original = release("orig", "Nevermind", NEVERMIND);
        let hits = release(
            "hits",
            "Nirvana",
            &[
                ("You Know You're Right", 218.0),
                ("Smells Like Teen Spirit", 301.0),
                ("Come as You Are", 219.0),
                ("Lithium", 257.0),
                ("In Bloom", 255.0),
                ("Heart-Shaped Box", 281.0),
            ],
        );
        let a = score(&album, &original);
        let b = score(&album, &hits);
        assert!(a.score > 0.95, "{a:?}");
        assert!(b.score < a.score - 0.15, "{b:?}");
        assert_eq!(
            a.tracks,
            [Some((1, 1)), Some((1, 2)), Some((1, 3)), Some((1, 4))]
        );
        // the compilation still pairs each song with its own track
        assert_eq!(b.tracks[0], Some((1, 2)));
        assert_eq!(b.tracks[1], Some((1, 5)));
    }

    #[test]
    fn tracks_pair_up_by_title_and_length_without_numbers_or_ids() {
        // shuffled, untagged numbers, titles with additions
        let album = job(&[
            ("Breed", 184.0, None),
            ("Smells Like Teen Spirit (Remastered)", 301.0, None),
            ("In Bloom", 255.0, None),
            ("Come as you are", 220.0, None),
        ]);
        let s = score(&album, &release("orig", "Nevermind", NEVERMIND));
        assert_eq!(
            s.tracks,
            [Some((1, 4)), Some((1, 1)), Some((1, 2)), Some((1, 3))]
        );
        assert!(s.score > CONFIDENT, "{s:?}");
    }

    #[test]
    fn a_partial_album_matches_less_confidently() {
        let half = job(&[
            ("Smells Like Teen Spirit", 301.0, Some(1)),
            ("In Bloom", 255.0, Some(2)),
        ]);
        let whole = job(&[
            ("Smells Like Teen Spirit", 301.0, Some(1)),
            ("In Bloom", 255.0, Some(2)),
            ("Come as You Are", 219.0, Some(3)),
            ("Breed", 184.0, Some(4)),
        ]);
        let r = release("orig", "Nevermind", NEVERMIND);
        let (h, w) = (score(&half, &r), score(&whole, &r));
        assert!(h.score >= CONFIDENT && h.score < w.score, "{h:?} {w:?}");
        assert_eq!(h.tracks, [Some((1, 1)), Some((1, 2))]);
    }

    #[test]
    fn a_different_album_does_not_match() {
        let album = job(&[
            ("Smells Like Teen Spirit", 301.0, Some(1)),
            ("In Bloom", 255.0, Some(2)),
            ("Come as You Are", 219.0, Some(3)),
            ("Breed", 184.0, Some(4)),
        ]);
        let other = release(
            "utero",
            "In Utero",
            &[
                ("Serve the Servants", 216.0),
                ("Scentless Apprentice", 228.0),
                ("Heart-Shaped Box", 281.0),
                ("Rape Me", 170.0),
            ],
        );
        let s = score(&album, &other);
        assert!(s.score < MIN_SCORE, "{s:?}");
    }

    #[test]
    fn the_same_recording_pairs_whatever_the_title() {
        let mut album = job(&[("Track 01", 100.0, None)]);
        album.tracks[0].recording = Some("rec-In Bloom".into());
        let s = score(&album, &release("orig", "Nevermind", NEVERMIND));
        assert_eq!(s.tracks, [Some((1, 2))]);
    }

    #[test]
    fn discs_count_in_positions() {
        let mut r = release("two", "Double", &[("A", 100.0), ("B", 100.0)]);
        let mut second = r.media[0].clone();
        second.position = 2;
        for t in &mut second.tracks {
            t.title = format!("{} II", t.title);
            t.recording.clear();
        }
        r.media.push(second);
        assert_eq!(r.format(), "2×CD");
        let mut album = job(&[("A II", 100.0, Some(1))]);
        album.tracks[0].disc = Some(2);
        assert_eq!(score(&album, &r).tracks, [Some((2, 1))]);
    }

    /// A lone file of an album, tagged with the album's name.
    fn lone(album: &str) -> AlbumJob {
        AlbumJob {
            album_id: 1,
            title: album.into(),
            artist: "Nirvana".into(),
            tracks: vec![MatchTrack {
                fingerprint: "f".into(),
                title: "Smells Like Teen Spirit".into(),
                duration: Some(301.0),
                recording: Some("rec-Smells Like Teen Spirit".into()),
                ..Default::default()
            }],
            ..Default::default()
        }
    }

    #[test]
    fn a_lone_track_matches_its_album_not_a_single_of_it() {
        let mut album = release("album", "Nevermind", NEVERMIND);
        album.primary_type = "Album".into();
        let mut single = release("single", "Smells Like Teen Spirit", &NEVERMIND[..1]);
        single.primary_type = "Single".into();
        let job = lone("Nevermind");
        let (a, s) = (score(&job, &album), score(&job, &single));
        assert!(a.score >= CONFIDENT, "{a:?}");
        assert!(s.score < CONFIDENT && s.score < a.score, "{s:?}");
        assert_eq!(a.tracks, [Some((1, 1))]);
        // its album's title track released as a single of the same name: the album still
        let mut title_single = release("ts", "Nevermind", &NEVERMIND[..1]);
        title_single.primary_type = "Single".into();
        assert!(score(&job, &title_single).score < a.score);
    }

    #[test]
    fn a_track_from_elsewhere_matches_nothing_surely() {
        // a mixtape's name: neither the album nor the single is it
        let job = lone("Greatest Hits Mix");
        let album = release("album", "Nevermind", NEVERMIND);
        let single = release("single", "Smells Like Teen Spirit", &NEVERMIND[..1]);
        assert!(score(&job, &album).score < CONFIDENT);
        assert!(score(&job, &single).score < CONFIDENT);
    }

    #[test]
    fn names_are_compared_loosely() {
        assert_eq!(
            normalize("Guns N' Roses — Live & Loud!"),
            "guns n roses live and loud"
        );
        assert!(similarity("Sigur Rós", "Sigur Ros") > 0.99);
        assert!(title_similarity("Song (Remastered 2011)", "Song") > 0.9);
        assert!(title_similarity("Song", "Other Song Entirely") < 0.5);
    }

    #[test]
    fn a_release_is_read_from_musicbrainz_json() {
        let v = json!({
            "id": "r1", "title": "Nevermind", "status": "Official", "date": "1991-09-24",
            "country": "US", "barcode": "720642442524",
            "artist-credit": [{"name": "Nirvana", "joinphrase": "", "artist": {"id": "a1", "name": "Nirvana"}}],
            "release-group": {"id": "g1", "primary-type": "Album", "secondary-types": [],
                              "first-release-date": "1991-09-24"},
            "label-info": [{"catalog-number": "DGCD-24425", "label": {"name": "DGC"}}],
            "media": [{"position": 1, "format": "CD", "tracks": [
                {"id": "t1", "number": "1", "position": 1, "title": "Smells Like Teen Spirit",
                 "length": 301920, "recording": {"id": "rec1", "title": "Smells Like Teen Spirit"},
                 "artist-credit": [{"name": "Nirvana", "joinphrase": " feat. "}, {"name": "X"}]}
            ]}]
        });
        let r = Release::from_json(&v).unwrap();
        assert_eq!(r.label, "DGC");
        assert_eq!(r.catalog_number, "DGCD-24425");
        assert_eq!(r.year(), Some(1991));
        assert_eq!(r.media[0].tracks[0].recording, "rec1");
        assert_eq!(r.media[0].tracks[0].artist, "Nirvana feat. X");
        assert_eq!(r.media[0].tracks[0].length, Some(301.92));
        assert_eq!(r.track_count(), 1);
    }
}
