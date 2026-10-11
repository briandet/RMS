//! Mixes for "Don't Stop The Music" and "Create Mix".
//!
//! * Random picks as Lyrion's Random Mix plugin makes them (`RandomPlay::Mixer`): random
//!   songs, optionally limited to genres or to one random year, a random album, a random
//!   artist.
//! * Similar songs: candidates that share genres or artists with the seed tracks, scored
//!   from their tags (genres, artists, year) and play history (artists played around the same
//!   time as the seeds, play count, rating, recently played), then picked with a little
//!   randomness and at most one track per album and two per artist.  Tracks that sound like
//!   the same recording as one already in the mix, the seeds or the queue are left out.  Once the library has
//!   been through sonic analysis ([`crate::sonic`]), songs that sound like the seeds join the
//!   candidates and how close they sound counts most in the score.

use crate::config::{ROLE_ALBUMARTIST, ROLE_ARTIST, ROLE_TRACKARTIST};
use crate::Library;
use rand::seq::SliceRandom;
use rand::Rng;
use rusqlite::{params_from_iter, Connection};
use std::collections::{HashMap, HashSet};
use tracing::debug;

/// Largest number of candidates scored for a similar-songs mix.
const POOL: usize = 3000;
/// Candidates added for sounding like the seeds.
const SOUND_POOL: usize = 300;

/// What a mix starts from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Seed {
    Track(i64),
    Album(i64),
    Artist(i64),
    Genre(i64),
    Year(i64),
}

fn ids_sql(n: usize) -> String {
    vec!["?"; n].join(",")
}

fn query_ids(c: &Connection, sql: &str, params: &[i64]) -> Vec<i64> {
    let Ok(mut st) = c.prepare(sql) else {
        return Vec::new();
    };
    st.query_map(params_from_iter(params.iter()), |r| r.get::<_, i64>(0))
        .map(|rows| rows.flatten().collect())
        .unwrap_or_default()
}

impl Library {
    /// `n` random songs, limited to `genres` (when not empty) and `year`, none of `exclude`.
    pub fn random_tracks(
        &self,
        n: usize,
        genres: &[i64],
        year: Option<i64>,
        exclude: &HashSet<i64>,
    ) -> Vec<i64> {
        let Ok(c) = self.conn() else {
            return Vec::new();
        };
        let mut sql = String::from("SELECT tracks.id FROM tracks WHERE 1");
        let mut params: Vec<i64> = Vec::new();
        if !genres.is_empty() {
            sql += &format!(
                " AND tracks.id IN (SELECT track_id FROM track_genres WHERE genre_id IN ({}))",
                ids_sql(genres.len())
            );
            params.extend(genres);
        }
        if let Some(y) = year {
            sql += " AND tracks.year = ?";
            params.push(y);
        }
        sql += " ORDER BY RANDOM() LIMIT ?";
        params.push((n + exclude.len()) as i64);
        query_ids(&c, &sql, &params)
            .into_iter()
            .filter(|id| !exclude.contains(id))
            .take(n)
            .collect()
    }

    /// A random album with playable tracks, in `genres` when not empty.
    pub fn random_album(&self, genres: &[i64]) -> Option<i64> {
        let c = self.conn().ok()?;
        let mut sql = String::from(
            "SELECT DISTINCT tracks.album_id FROM tracks WHERE tracks.album_id IS NOT NULL",
        );
        if !genres.is_empty() {
            sql += &format!(
                " AND tracks.id IN (SELECT track_id FROM track_genres WHERE genre_id IN ({}))",
                ids_sql(genres.len())
            );
        }
        sql += " ORDER BY RANDOM() LIMIT 1";
        query_ids(&c, &sql, genres).into_iter().next()
    }

    /// A random artist (artist, album artist or track artist role).
    pub fn random_artist(&self) -> Option<i64> {
        let c = self.conn().ok()?;
        let sql = format!(
            "SELECT DISTINCT track_artists.artist_id FROM track_artists \
             JOIN tracks ON tracks.id = track_artists.track_id \
             WHERE track_artists.role IN ({ROLE_ARTIST}, {ROLE_ALBUMARTIST}, {ROLE_TRACKARTIST}) \
             ORDER BY RANDOM() LIMIT 1"
        );
        query_ids(&c, &sql, &[]).into_iter().next()
    }

    /// A random year that has songs (`RandomPlay::Mixer::getRandomYear`).
    pub fn random_year(&self) -> Option<i64> {
        let c = self.conn().ok()?;
        let sql =
            "SELECT DISTINCT tracks.year FROM tracks WHERE tracks.year > 0 ORDER BY RANDOM() LIMIT 1";
        query_ids(&c, sql, &[]).into_iter().next()
    }

    /// Genres of the given tracks.
    pub fn genres_of(&self, tracks: &[i64]) -> Vec<i64> {
        let Ok(c) = self.conn() else {
            return Vec::new();
        };
        if tracks.is_empty() {
            return Vec::new();
        }
        let sql = format!(
            "SELECT DISTINCT genre_id FROM track_genres WHERE track_id IN ({})",
            ids_sql(tracks.len())
        );
        query_ids(&c, &sql, tracks)
    }

    /// The tracks a seed stands for, in album order.
    pub fn seed_tracks(&self, seed: Seed) -> Vec<i64> {
        let Ok(c) = self.conn() else {
            return Vec::new();
        };
        let order = "ORDER BY tracks.album_id, tracks.disc, tracks.track_number, tracks.id";
        let (sql, id) = match seed {
            Seed::Track(id) => ("SELECT id FROM tracks WHERE id = ?".to_string(), id),
            Seed::Album(id) => (
                format!("SELECT id FROM tracks WHERE album_id = ? {order}"),
                id,
            ),
            Seed::Artist(id) => (
                format!(
                    "SELECT DISTINCT tracks.id FROM tracks JOIN track_artists ON track_artists.track_id = tracks.id \
                     WHERE track_artists.artist_id = ? {order}"
                ),
                id,
            ),
            Seed::Genre(id) => (
                "SELECT tracks.id FROM tracks JOIN track_genres ON track_genres.track_id = tracks.id \
                 WHERE track_genres.genre_id = ? ORDER BY RANDOM() LIMIT 50"
                    .to_string(),
                id,
            ),
            Seed::Year(y) => (
                "SELECT id FROM tracks WHERE year = ? ORDER BY RANDOM() LIMIT 50".to_string(),
                y,
            ),
        };
        query_ids(&c, &sql, &[id])
    }

    /// Up to `n` songs similar to `seeds`, none of `exclude`.
    pub fn similar_tracks(&self, seeds: &[i64], n: usize, exclude: &HashSet<i64>) -> Vec<i64> {
        let Ok(c) = self.conn() else {
            return Vec::new();
        };
        let seed_info = load_candidates(&c, seeds);
        if seed_info.is_empty() {
            return Vec::new();
        }
        let mut profile = Profile::from_seeds(&seed_info);
        profile.related = related_artists(&c, &profile);

        // candidates: sharing a genre or an artist with the seeds, then random ones
        let genres: Vec<i64> = profile.genres.keys().copied().collect();
        let artists: Vec<i64> = profile
            .artists
            .keys()
            .chain(profile.related.keys())
            .copied()
            .collect();
        let mut pool: Vec<i64> = Vec::new();
        if !genres.is_empty() {
            let sql = format!(
                "SELECT DISTINCT track_id FROM track_genres JOIN tracks ON tracks.id = track_genres.track_id \
                 WHERE genre_id IN ({}) ORDER BY RANDOM() LIMIT {POOL}",
                ids_sql(genres.len())
            );
            pool.extend(query_ids(&c, &sql, &genres));
        }
        if !artists.is_empty() {
            let sql = format!(
                "SELECT DISTINCT track_id FROM track_artists JOIN tracks ON tracks.id = track_artists.track_id \
                 WHERE artist_id IN ({}) ORDER BY RANDOM() LIMIT {POOL}",
                ids_sql(artists.len())
            );
            pool.extend(query_ids(&c, &sql, &artists));
        }
        // songs that sound like the seeds, whatever their tags
        let index = crate::sonic::Index::load(&c);
        let sound = index.closeness(seeds);
        if let Some(sound) = &sound {
            pool.extend(sound.nearest.iter().take(SOUND_POOL.max(20 * n)).copied());
        }
        profile.sound = sound.map(|s| s.rank);
        if pool.len() < 4 * n {
            let sql = "SELECT id FROM tracks ORDER BY RANDOM() LIMIT ?";
            pool.extend(query_ids(&c, sql, &[(4 * n) as i64]));
        }
        let seed_set: HashSet<i64> = seeds.iter().copied().collect();
        let mut seen = HashSet::new();
        pool.retain(|id| !exclude.contains(id) && !seed_set.contains(id) && seen.insert(*id));

        let candidates = load_candidates(&c, &pool);
        let mut avoid: Vec<i64> = seeds.to_vec();
        avoid.extend(exclude.iter().filter(|id| !seed_set.contains(id)));
        let duplicate = |a: i64, b: i64| {
            let dup = index.near_duplicate(a, b);
            if dup {
                debug!("mix: track {a} sounds like track {b}, left out");
            }
            dup
        };
        let now = crate::scanner::now();
        pick(
            &profile,
            &candidates,
            &avoid,
            &duplicate,
            n,
            now,
            &mut rand::rng(),
        )
    }
}

/// What the scoring needs to know about a track.
#[derive(Debug, Clone, Default)]
pub struct Candidate {
    pub id: i64,
    pub album: Option<i64>,
    pub artists: Vec<i64>,
    pub genres: Vec<i64>,
    pub year: Option<i64>,
    pub play_count: i64,
    pub last_played: Option<i64>,
    /// 0..100
    pub rating: i64,
}

/// Tracks with their artists, genres and play statistics.
fn load_candidates(c: &Connection, ids: &[i64]) -> Vec<Candidate> {
    let mut out: Vec<Candidate> = Vec::with_capacity(ids.len());
    for chunk in ids.chunks(500) {
        let sql = format!(
            "SELECT tracks.id, tracks.album_id, tracks.year, \
               (SELECT GROUP_CONCAT(artist_id) FROM track_artists WHERE track_id = tracks.id \
                  AND role IN ({ROLE_ARTIST}, {ROLE_ALBUMARTIST}, {ROLE_TRACKARTIST})), \
               (SELECT GROUP_CONCAT(genre_id) FROM track_genres WHERE track_id = tracks.id), \
               track_stats.play_count, track_stats.last_played, track_stats.rating \
             FROM tracks LEFT JOIN track_stats ON track_stats.url = tracks.url \
             WHERE tracks.id IN ({})",
            ids_sql(chunk.len())
        );
        let Ok(mut st) = c.prepare(&sql) else {
            continue;
        };
        let split = |s: Option<String>| -> Vec<i64> {
            s.map(|s| s.split(',').filter_map(|x| x.parse().ok()).collect())
                .unwrap_or_default()
        };
        let rows = st.query_map(params_from_iter(chunk.iter()), |r| {
            Ok(Candidate {
                id: r.get(0)?,
                album: r.get(1)?,
                year: r.get::<_, Option<i64>>(2)?.filter(|y| *y > 0),
                artists: split(r.get(3)?),
                genres: split(r.get(4)?),
                play_count: r.get::<_, Option<i64>>(5)?.unwrap_or(0),
                last_played: r.get(6)?,
                rating: r.get::<_, Option<i64>>(7)?.unwrap_or(0),
            })
        });
        if let Ok(rows) = rows {
            out.extend(rows.flatten());
        }
    }
    // keep the order of `ids`
    let pos: HashMap<i64, usize> = ids.iter().enumerate().map(|(i, id)| (*id, i)).collect();
    out.sort_by_key(|t| pos.get(&t.id).copied().unwrap_or(usize::MAX));
    out
}

/// Artists played within an hour of the seed artists' songs (by last-played time), and
/// artists sharing albums with them (features, compilations): weight 0..1.
fn related_artists(c: &Connection, p: &Profile) -> HashMap<i64, f64> {
    let seeds: Vec<i64> = p.artists.keys().copied().collect();
    let mut related: HashMap<i64, f64> = HashMap::new();
    if seeds.is_empty() {
        return related;
    }
    let list = ids_sql(seeds.len());
    let mut add = |sql: String, weight: f64| {
        let Ok(mut st) = c.prepare(&sql) else { return };
        let rows = st.query_map(params_from_iter(seeds.iter().chain(seeds.iter())), |r| {
            Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?))
        });
        let rows: Vec<(i64, i64)> = rows.map(|r| r.flatten().collect()).unwrap_or_default();
        let max = rows.iter().map(|r| r.1).max().unwrap_or(1).max(1) as f64;
        for (artist, n) in rows {
            let w = related.entry(artist).or_insert(0.0);
            *w = w.max(weight * n as f64 / max);
        }
    };
    add(
        format!(
            "WITH seedplays AS ( \
               SELECT tp.last_played AS t FROM track_stats tp JOIN tracks ON tracks.url = tp.url \
               WHERE tracks.artist_id IN ({list}) AND tp.last_played > 0) \
             SELECT tracks.artist_id, COUNT(*) AS n FROM track_stats tp \
               JOIN tracks ON tracks.url = tp.url JOIN seedplays s \
               ON tp.last_played BETWEEN s.t - 3600 AND s.t + 3600 \
             WHERE tracks.artist_id IS NOT NULL AND tracks.artist_id NOT IN ({list}) \
             GROUP BY 1 ORDER BY n DESC LIMIT 25"
        ),
        1.0,
    );
    add(
        format!(
            "SELECT ct.artist_id, COUNT(*) AS n FROM track_artists ct JOIN tracks ON tracks.id = ct.track_id \
             WHERE tracks.album_id IN (SELECT DISTINCT tracks.album_id FROM tracks JOIN track_artists s ON s.track_id = tracks.id \
                                    WHERE s.artist_id IN ({list})) \
               AND ct.role IN ({ROLE_ARTIST}, {ROLE_ALBUMARTIST}, {ROLE_TRACKARTIST}) AND ct.artist_id NOT IN ({list}) \
             GROUP BY 1 ORDER BY n DESC LIMIT 25"
        ),
        0.6,
    );
    related
}

/// The seeds summed up.
#[derive(Debug, Default)]
pub struct Profile {
    /// genre → share of the seeds having it
    pub genres: HashMap<i64, f64>,
    /// artist → share of the seeds by them
    pub artists: HashMap<i64, f64>,
    /// related artist → 0..1
    pub related: HashMap<i64, f64>,
    /// median year of the seeds
    pub year: Option<f64>,
    /// analysed track → how close it sounds to the seeds, 1 (nearest) to 0 (farthest);
    /// `None` when no seed has been analysed
    pub sound: Option<HashMap<i64, f64>>,
}

impl Profile {
    pub fn from_seeds(seeds: &[Candidate]) -> Profile {
        let n = seeds.len().max(1) as f64;
        let mut p = Profile::default();
        for s in seeds {
            for g in &s.genres {
                *p.genres.entry(*g).or_default() += 1.0 / n;
            }
            for a in &s.artists {
                *p.artists.entry(*a).or_default() += 1.0 / n;
            }
        }
        let mut years: Vec<i64> = seeds.iter().filter_map(|s| s.year).collect();
        years.sort_unstable();
        if !years.is_empty() {
            p.year = Some(years[years.len() / 2] as f64);
        }
        p
    }

    /// How well `c` fits the seeds (higher is better).
    pub fn score(&self, c: &Candidate, now: i64) -> f64 {
        let mut s = 0.0;
        // genres: the share of the seeds' genre weight this track has
        if !self.genres.is_empty() {
            let total: f64 = self.genres.values().sum();
            let shared: f64 = c.genres.iter().filter_map(|g| self.genres.get(g)).sum();
            s += 3.0 * shared / total;
        }
        // same artist as a seed, or one played / appearing with them
        let same = c
            .artists
            .iter()
            .filter_map(|a| self.artists.get(a).copied())
            .fold(0.0, f64::max);
        let related = c
            .artists
            .iter()
            .filter_map(|a| self.related.get(a).copied())
            .fold(0.0, f64::max);
        s += 1.5 * same.min(1.0) + 2.0 * related;
        // sound: only the closest few percent of the library count much; tracks not analysed
        // yet get a middling score so they are neither favoured nor left out
        if let Some(sound) = &self.sound {
            let r = sound.get(&c.id).copied().unwrap_or(0.85);
            s += 4.0 * r.powi(8);
        }
        // era
        if let (Some(y), Some(cy)) = (self.year, c.year) {
            s += 1.5 * (-(y - cy as f64).abs() / 8.0).exp();
        }
        // taste: liked and played songs a little more, recently played ones less
        s += 1.0 * c.rating as f64 / 100.0;
        if c.play_count > 0 {
            s += 0.3;
        }
        if let Some(t) = c.last_played {
            if now - t < 24 * 3600 {
                s -= 2.0;
            }
        }
        s
    }
}

/// Choose `n` of the candidates: the best scores with some randomness, at most one track per
/// album and two per artist (relaxed when there are not enough candidates), and none that
/// `duplicate` takes for the same recording as a track in `avoid` or one already chosen
/// (never relaxed).
pub fn pick(
    profile: &Profile,
    candidates: &[Candidate],
    avoid: &[i64],
    duplicate: &dyn Fn(i64, i64) -> bool,
    n: usize,
    now: i64,
    rng: &mut impl Rng,
) -> Vec<i64> {
    let mut scored: Vec<(f64, &Candidate)> = candidates
        .iter()
        .map(|c| (profile.score(c, now) + rng.random_range(0.0..1.0), c))
        .collect();
    scored.shuffle(rng);
    scored.sort_by(|a, b| b.0.total_cmp(&a.0));

    let mut chosen: Vec<i64> = Vec::new();
    let mut taken: HashSet<i64> = HashSet::new();
    for (max_album, max_artist) in [(1, 2), (2, 3), (usize::MAX, usize::MAX)] {
        let mut albums: HashMap<i64, usize> = HashMap::new();
        let mut artists: HashMap<i64, usize> = HashMap::new();
        for id in &chosen {
            if let Some(c) = candidates.iter().find(|c| c.id == *id) {
                if let Some(a) = c.album {
                    *albums.entry(a).or_default() += 1;
                }
                for a in &c.artists {
                    *artists.entry(*a).or_default() += 1;
                }
            }
        }
        for (_, c) in &scored {
            if chosen.len() >= n {
                return chosen;
            }
            if taken.contains(&c.id)
                || avoid.iter().chain(&chosen).any(|o| duplicate(c.id, *o))
                || c.album
                    .is_some_and(|a| albums.get(&a).copied().unwrap_or(0) >= max_album)
                || c.artists
                    .iter()
                    .any(|a| artists.get(a).copied().unwrap_or(0) >= max_artist)
            {
                continue;
            }
            taken.insert(c.id);
            chosen.push(c.id);
            if let Some(a) = c.album {
                *albums.entry(a).or_default() += 1;
            }
            for a in &c.artists {
                *artists.entry(*a).or_default() += 1;
            }
        }
    }
    chosen
}

#[cfg(test)]
mod tests {
    use super::*;
    use rand::SeedableRng;

    fn track(id: i64, album: i64, artist: i64, genres: &[i64], year: i64) -> Candidate {
        Candidate {
            id,
            album: Some(album),
            artists: vec![artist],
            genres: genres.to_vec(),
            year: Some(year),
            ..Default::default()
        }
    }

    #[test]
    fn genre_artist_and_era_raise_the_score() {
        let seeds = [track(1, 10, 100, &[7], 1990), track(2, 11, 101, &[7], 1992)];
        let p = Profile::from_seeds(&seeds);
        let now = 2_000_000_000;
        let same_genre_era = track(3, 12, 102, &[7], 1991);
        let same_genre_other_era = track(4, 13, 103, &[7], 2020);
        let other_genre = track(5, 14, 104, &[8], 1991);
        let same_artist = track(6, 15, 100, &[7], 1990);
        assert!(p.score(&same_genre_era, now) > p.score(&same_genre_other_era, now));
        assert!(p.score(&same_genre_other_era, now) > p.score(&other_genre, now));
        assert!(p.score(&same_artist, now) > p.score(&same_genre_era, now));
        let mut recent = same_genre_era.clone();
        recent.last_played = Some(now - 600);
        assert!(p.score(&recent, now) < p.score(&same_genre_era, now));
    }

    #[test]
    fn leaves_out_duplicates() {
        let p = Profile::default();
        let cands: Vec<Candidate> = (2..=6).map(|i| track(i, i, 100 + i, &[], 2000)).collect();
        // 2 is the queued 1 again, 4 and 5 are one recording twice
        let same = |a: i64, b: i64| matches!((a.min(b), a.max(b)), (1, 2) | (4, 5));
        let mut rng = rand::rngs::StdRng::seed_from_u64(1);
        let got = pick(&p, &cands, &[1], &same, 5, 0, &mut rng);
        assert!(!got.contains(&2));
        assert_eq!(got.len(), 3);
        assert!(got.contains(&3) && got.contains(&6));
        assert!(got.contains(&4) != got.contains(&5));
    }

    #[test]
    fn sounding_alike_beats_sharing_a_genre() {
        let seeds = [track(1, 10, 100, &[7], 1990)];
        let mut p = Profile::from_seeds(&seeds);
        let now = 2_000_000_000;
        let same_genre = track(2, 11, 101, &[7], 1990);
        let sounds_alike = track(3, 12, 102, &[8], 1990);
        let not_analysed = track(4, 13, 103, &[8], 1990);
        p.sound = Some(HashMap::from([(1, 1.0), (3, 0.99), (2, 0.2)]));
        assert!(p.score(&sounds_alike, now) > p.score(&same_genre, now));
        // a track not analysed yet is not ranked below the ones that sound unlike the seeds
        let sounds_unlike = track(5, 14, 104, &[8], 1990);
        p.sound.as_mut().unwrap().insert(5, 0.1);
        assert!(p.score(&not_analysed, now) > p.score(&sounds_unlike, now));
    }

    #[test]
    fn related_artists_count() {
        let seeds = [track(1, 10, 100, &[], 0)];
        let mut p = Profile::from_seeds(&seeds);
        p.related.insert(200, 1.0);
        let related = track(2, 11, 200, &[], 0);
        let stranger = track(3, 12, 300, &[], 0);
        assert!(p.score(&related, 0) > p.score(&stranger, 0) + 1.0);
    }

    #[test]
    fn picks_spread_over_albums_and_artists() {
        let seeds = [track(1, 10, 100, &[7], 1990)];
        let p = Profile::from_seeds(&seeds);
        // one artist with a big album of perfect matches, and a few other fitting songs
        let mut cands: Vec<Candidate> = (0..10)
            .map(|i| track(100 + i, 20, 100, &[7], 1990))
            .collect();
        cands.extend((0..6).map(|i| track(200 + i, 30 + i, 300 + i, &[7], 1991)));
        let mut rng = rand::rngs::StdRng::seed_from_u64(1);
        let got = pick(&p, &cands, &[], &|_, _| false, 5, 0, &mut rng);
        assert_eq!(got.len(), 5);
        // only one from the big album
        assert_eq!(got.iter().filter(|id| (100..110).contains(*id)).count(), 1);
        // asking for more than the spread allows relaxes the limits rather than coming up short
        let got = pick(&p, &cands, &[], &|_, _| false, 14, 0, &mut rng);
        assert_eq!(got.len(), 14);
        let unique: HashSet<_> = got.iter().collect();
        assert_eq!(unique.len(), 14);
    }
}
