//! Judging similar-songs mixes against an outside opinion: ListenBrainz's similar artists
//! (artists its listeners play in the same sessions), fetched by `rms-mix-eval`.  For each seed
//! track, mixes are made with several ways of scoring ([`Variant`]); a pick is a *hit* when
//! its artist is one ListenBrainz finds similar to the seed's.  Hit rates are compared with
//! the *chance* rate, the share of the library's tracks (by other artists than the seed's)
//! that are hits, which no mix can help: lift 1 is no better than chance.  Picks by the seed's
//! own artist count apart (`same_artist`): they tell nothing about similarity.

use crate::mix::Weights;
use crate::Library;
use rand::seq::IndexedRandom;
use rand::Rng;
use rusqlite::OptionalExtension;
use serde::Serialize;
use std::collections::{HashMap, HashSet};

/// A way of scoring mixes to judge.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub enum Variant {
    /// Today's scoring.
    Current,
    /// Without how the songs sound.
    NoSound,
    /// Only how the songs sound.
    SoundOnly,
    /// Only tags: genres, artists, years (no sound, no listening history).
    TagsOnly,
    /// Songs at random: the floor.
    Random,
}

impl Variant {
    pub const ALL: [Variant; 5] = [
        Variant::Current,
        Variant::NoSound,
        Variant::SoundOnly,
        Variant::TagsOnly,
        Variant::Random,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Variant::Current => "current",
            Variant::NoSound => "no-sound",
            Variant::SoundOnly => "sound-only",
            Variant::TagsOnly => "tags-only",
            Variant::Random => "random",
        }
    }

    pub fn parse(s: &str) -> Option<Variant> {
        Variant::ALL.into_iter().find(|v| v.name() == s)
    }

    /// The scoring weights (`None`: no scoring, songs at random).
    pub fn weights(self) -> Option<Weights> {
        let d = Weights::default();
        let none = Weights {
            genres: 0.0,
            same_artist: 0.0,
            related_artist: 0.0,
            sound: 0.0,
            era: 0.0,
            taste: 0.0,
        };
        match self {
            Variant::Current => Some(d),
            Variant::NoSound => Some(Weights { sound: 0.0, ..d }),
            Variant::SoundOnly => Some(Weights {
                sound: d.sound,
                ..none
            }),
            Variant::TagsOnly => Some(Weights {
                sound: 0.0,
                taste: 0.0,
                ..d
            }),
            Variant::Random => None,
        }
    }
}

/// A library artist, as ListenBrainz may name it.
#[derive(Debug, Clone, Default)]
pub struct ArtistKey {
    pub mbid: Option<String>,
    /// search key of the name
    pub name: String,
}

/// What the evaluation knows of the library: each track's primary artist, and each artist's
/// MusicBrainz id and name.
pub struct Catalog {
    pub track_artist: HashMap<i64, i64>,
    pub artists: HashMap<i64, ArtistKey>,
    pub names: HashMap<i64, String>,
}

/// A seed track and what ListenBrainz finds similar to its artist.
#[derive(Debug, Clone, Serialize)]
pub struct Case {
    pub track: i64,
    pub artist: i64,
    #[serde(skip)]
    pub similar_mbids: HashSet<String>,
    #[serde(skip)]
    pub similar_names: HashSet<String>,
    /// tracks by other artists, and of those by similar artists: the chance a song at random
    /// is a hit
    pub others: usize,
    pub similar_tracks: usize,
}

impl Case {
    pub fn chance(&self) -> f64 {
        if self.others == 0 {
            0.0
        } else {
            self.similar_tracks as f64 / self.others as f64
        }
    }
}

impl Library {
    /// The library's tracks and artists, for judging mixes.
    pub fn eval_catalog(&self) -> anyhow::Result<Catalog> {
        let c = self.conn()?;
        let rules = self.rules();
        let mut track_artist = HashMap::new();
        let mut st = c.prepare("SELECT id, artist_id FROM tracks WHERE artist_id IS NOT NULL")?;
        for r in st.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?)))? {
            let (t, a) = r?;
            track_artist.insert(t, a);
        }
        let mut artists = HashMap::new();
        let mut names = HashMap::new();
        let mut st = c.prepare("SELECT id, name, mbid FROM artists")?;
        for r in st.query_map([], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, Option<String>>(2)?,
            ))
        })? {
            let (id, name, mbid) = r?;
            artists.insert(
                id,
                ArtistKey {
                    mbid,
                    name: rules.search_key(&name),
                },
            );
            names.insert(id, name);
        }
        // artists without a tagged id: as their tracks were identified
        let mut st = c.prepare(
            "SELECT t.artist_id, m.artist_mbid, COUNT(*) AS n FROM tracks t
             JOIN track_musicbrainz m ON m.fingerprint = t.fingerprint
             WHERE m.artist_mbid IS NOT NULL AND t.artist_id IS NOT NULL
             GROUP BY t.artist_id, m.artist_mbid ORDER BY n",
        )?;
        for r in st.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)))? {
            let (a, m) = r?;
            if let Some(k) = artists.get_mut(&a) {
                if k.mbid.is_none() || k.mbid.as_deref() == Some("") {
                    k.mbid = Some(m);
                }
            }
        }
        Ok(Catalog {
            track_artist,
            artists,
            names,
        })
    }

    /// A case for a seed track by `artist`, from ListenBrainz's similar artists (MusicBrainz
    /// id, name).
    pub fn eval_case(
        &self,
        cat: &Catalog,
        track: i64,
        artist: i64,
        similar: &[(String, String)],
    ) -> Case {
        cat.case(track, artist, similar, &self.rules())
    }

    /// A track's title (for reports).
    pub fn eval_title(&self, track: i64) -> String {
        let Ok(c) = self.conn() else {
            return String::new();
        };
        c.query_row("SELECT title FROM tracks WHERE id = ?", [track], |r| {
            r.get(0)
        })
        .optional()
        .ok()
        .flatten()
        .unwrap_or_default()
    }
}

impl Catalog {
    /// Seed candidates: tracks whose artist has a MusicBrainz id, in random order, one per
    /// artist.
    pub fn seed_candidates(&self, rng: &mut impl Rng) -> Vec<(i64, i64, String)> {
        let mut by_artist: HashMap<i64, Vec<i64>> = HashMap::new();
        for (t, a) in &self.track_artist {
            by_artist.entry(*a).or_default().push(*t);
        }
        let mut out: Vec<(i64, i64, String)> = by_artist
            .into_iter()
            .filter_map(|(a, mut tracks)| {
                let mbid = self
                    .artists
                    .get(&a)?
                    .mbid
                    .clone()
                    .filter(|m| !m.is_empty())?;
                tracks.sort_unstable();
                Some((*tracks.choose(rng)?, a, mbid))
            })
            .collect();
        out.sort_unstable_by_key(|x| x.1);
        use rand::seq::SliceRandom;
        out.shuffle(rng);
        out
    }

    /// A case for a seed, from ListenBrainz's similar artists (MusicBrainz id, name).
    pub fn case(
        &self,
        track: i64,
        artist: i64,
        similar: &[(String, String)],
        rules: &crate::text::TextRules,
    ) -> Case {
        let similar_mbids: HashSet<String> = similar.iter().map(|(m, _)| m.clone()).collect();
        let similar_names: HashSet<String> =
            similar.iter().map(|(_, n)| rules.search_key(n)).collect();
        let mut case = Case {
            track,
            artist,
            similar_mbids,
            similar_names,
            others: 0,
            similar_tracks: 0,
        };
        for a in self.track_artist.values() {
            if *a == artist {
                continue;
            }
            case.others += 1;
            if self.is_similar(&case, *a) {
                case.similar_tracks += 1;
            }
        }
        case
    }

    fn is_similar(&self, case: &Case, artist: i64) -> bool {
        let Some(k) = self.artists.get(&artist) else {
            return false;
        };
        k.mbid
            .as_ref()
            .is_some_and(|m| case.similar_mbids.contains(m))
            || (!k.name.is_empty() && case.similar_names.contains(&k.name))
    }

    /// How a pick fares: `None` when it is by the seed's artist.
    pub fn hit(&self, case: &Case, track: i64) -> Option<bool> {
        let a = *self.track_artist.get(&track)?;
        if a == case.artist {
            return None;
        }
        Some(self.is_similar(case, a))
    }
}

/// One mix of one variant for one case.
#[derive(Debug, Clone, Serialize)]
pub struct Mix {
    pub picks: Vec<i64>,
    pub hits: usize,
    /// picks by other artists than the seed's
    pub others: usize,
    pub same_artist: usize,
}

/// Make a mix of `n` for a case.
pub fn mix(
    lib: &Library,
    cat: &Catalog,
    case: &Case,
    variant: Variant,
    n: usize,
    rng: &mut impl Rng,
) -> Mix {
    let picks = match variant.weights() {
        Some(w) => lib.similar_tracks_weighted(&[case.track], n, &HashSet::new(), false, w, rng),
        None => {
            let all: Vec<i64> = cat
                .track_artist
                .keys()
                .copied()
                .filter(|t| *t != case.track)
                .collect();
            let mut all = all;
            all.sort_unstable();
            all.choose_multiple(rng, n).copied().collect()
        }
    };
    let mut m = Mix {
        picks,
        hits: 0,
        others: 0,
        same_artist: 0,
    };
    for t in &m.picks {
        match cat.hit(case, *t) {
            None => m.same_artist += 1,
            Some(hit) => {
                m.others += 1;
                m.hits += hit as usize;
            }
        }
    }
    m
}

/// How a variant did over all cases.
#[derive(Debug, Clone, Default, Serialize)]
pub struct Summary {
    pub variant: String,
    pub mixes: usize,
    pub picks: usize,
    /// picks by other artists than the seed's, and of those hits
    pub others: usize,
    pub hits: usize,
    /// hits / others over all picks
    pub hit_rate: f64,
    /// the mean over cases of hit rate / chance rate
    pub lift: f64,
    /// standard error of the per-case hit rates' mean
    pub hit_rate_se: f64,
    /// share of picks by the seed's artist
    pub same_artist: f64,
    /// cases where this variant's hit rate beats, ties and trails the first variant's
    pub wins: usize,
    pub ties: usize,
    pub losses: usize,
}

/// Summaries of the variants over the cases (`mixes[case][variant]` holds that case's
/// repeated mixes); wins, ties and losses are against the first variant.
pub fn summarize(cases: &[Case], variants: &[Variant], mixes: &[Vec<Vec<Mix>>]) -> Vec<Summary> {
    // per case and variant: hit rate over the repeats
    let rate = |ms: &[Mix]| -> Option<f64> {
        let others: usize = ms.iter().map(|m| m.others).sum();
        (others > 0).then(|| ms.iter().map(|m| m.hits).sum::<usize>() as f64 / others as f64)
    };
    variants
        .iter()
        .enumerate()
        .map(|(vi, v)| {
            let mut s = Summary {
                variant: v.name().into(),
                ..Default::default()
            };
            let mut rates = Vec::new();
            let mut lifts = Vec::new();
            for (ci, case) in cases.iter().enumerate() {
                let ms = &mixes[ci][vi];
                s.mixes += ms.len();
                for m in ms {
                    s.picks += m.picks.len();
                    s.others += m.others;
                    s.hits += m.hits;
                    s.same_artist += m.same_artist as f64;
                }
                let Some(r) = rate(ms) else { continue };
                rates.push(r);
                if case.chance() > 0.0 {
                    lifts.push(r / case.chance());
                }
                if vi > 0 {
                    if let Some(base) = rate(&mixes[ci][0]) {
                        if (r - base).abs() < 1e-9 {
                            s.ties += 1;
                        } else if r > base {
                            s.wins += 1;
                        } else {
                            s.losses += 1;
                        }
                    }
                }
            }
            s.same_artist /= s.picks.max(1) as f64;
            s.hit_rate = s.hits as f64 / s.others.max(1) as f64;
            s.lift = mean(&lifts);
            if rates.len() > 1 {
                let m = mean(&rates);
                let var =
                    rates.iter().map(|r| (r - m).powi(2)).sum::<f64>() / (rates.len() - 1) as f64;
                s.hit_rate_se = (var / rates.len() as f64).sqrt();
            }
            s
        })
        .collect()
}

fn mean(xs: &[f64]) -> f64 {
    if xs.is_empty() {
        0.0
    } else {
        xs.iter().sum::<f64>() / xs.len() as f64
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn catalog() -> Catalog {
        // artists 1 (seed), 2 (similar by id), 3 (similar by name), 4 (not similar)
        let rules = crate::text::TextRules::new("", "");
        let key = |mbid: Option<&str>, name: &str| ArtistKey {
            mbid: mbid.map(str::to_string),
            name: rules.search_key(name),
        };
        let mut artists = HashMap::new();
        artists.insert(1, key(Some("m1"), "One"));
        artists.insert(2, key(Some("m2"), "Two"));
        artists.insert(3, key(None, "Three"));
        artists.insert(4, key(Some("m4"), "Four"));
        let track_artist: HashMap<i64, i64> =
            [(10, 1), (11, 1), (20, 2), (30, 3), (40, 4), (41, 4)].into();
        Catalog {
            track_artist,
            artists,
            names: HashMap::new(),
        }
    }

    #[test]
    fn hits_are_picks_by_similar_artists() {
        let cat = catalog();
        let rules = crate::text::TextRules::new("", "");
        let case = cat.case(
            10,
            1,
            &[("m2".into(), "Two".into()), ("mx".into(), "Three".into())],
            &rules,
        );
        // 4 tracks by others, 2 by similar artists (by id, by name)
        assert_eq!((case.others, case.similar_tracks), (4, 2));
        assert_eq!(case.chance(), 0.5);
        assert_eq!(cat.hit(&case, 11), None, "the seed's artist");
        assert_eq!(cat.hit(&case, 20), Some(true));
        assert_eq!(cat.hit(&case, 30), Some(true));
        assert_eq!(cat.hit(&case, 40), Some(false));
    }

    #[test]
    fn summaries_compare_with_chance_and_the_first_variant() {
        let cat = catalog();
        let rules = crate::text::TextRules::new("", "");
        let case = cat.case(10, 1, &[("m2".into(), "Two".into())], &rules);
        assert_eq!(case.chance(), 0.25);
        let m = |picks: Vec<i64>| {
            let mut out = Mix {
                picks: picks.clone(),
                hits: 0,
                others: 0,
                same_artist: 0,
            };
            for t in picks {
                match cat.hit(&case, t) {
                    None => out.same_artist += 1,
                    Some(h) => {
                        out.others += 1;
                        out.hits += h as usize;
                    }
                }
            }
            out
        };
        let mixes = vec![vec![
            vec![m(vec![40, 41])],
            vec![m(vec![20, 40])],
            vec![m(vec![11, 20])],
        ]];
        let v = [Variant::Current, Variant::NoSound, Variant::SoundOnly];
        let s = summarize(&[case], &v, &mixes);
        assert_eq!(s[0].hit_rate, 0.0);
        assert_eq!(s[1].hit_rate, 0.5);
        assert_eq!(s[1].lift, 2.0, "twice chance");
        assert_eq!((s[1].wins, s[1].losses), (1, 0));
        assert_eq!(s[2].hit_rate, 1.0, "the seed's own artist doesn't count");
        assert_eq!(s[2].same_artist, 0.5);
    }

    #[test]
    fn variants_by_name() {
        for v in Variant::ALL {
            assert_eq!(Variant::parse(v.name()), Some(v));
        }
        assert_eq!(Variant::Current.weights(), Some(Weights::default()));
        assert_eq!(Variant::NoSound.weights().unwrap().sound, 0.0);
        assert_eq!(Variant::SoundOnly.weights().unwrap().genres, 0.0);
        assert!(Variant::Random.weights().is_none());
    }
}
