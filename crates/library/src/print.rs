//! Audio fingerprints with Chromaprint's algorithm (AcoustID's, in the pure-Rust port): to
//! recognise the same recording as a local file and as a stream, whatever their tags say.
//! A fingerprint covers the first two minutes; two are the same recording when most of the
//! shorter one lines up with the other with few differing bits.

use rusty_chromaprint::{match_fingerprints, Configuration, Fingerprinter};

/// Stored with each fingerprint; others are redone.
pub const VERSION: i64 = 1;
/// The audio fingerprinted (AcoustID uses the first two minutes too).
const SECS: usize = 120;
/// Matching segments must cover this share of the shorter fingerprint.
const COVERAGE: f64 = 0.7;
/// …with at most this many differing bits per 32-bit item on average (unrelated audio: ~16).
const MAX_SCORE: f64 = 7.0;

fn config() -> Configuration {
    Configuration::preset_test2()
}

/// The fingerprint of a track's audio (mono f32 at `rate` Hz), or `None` for too little audio.
pub fn fingerprint(samples: &[f32], rate: u32) -> Option<Vec<u32>> {
    let n = samples.len().min(SECS * rate as usize);
    if n < 5 * rate as usize {
        return None;
    }
    let mut p = Fingerprinter::new(&config());
    p.start(rate, 1).ok()?;
    let pcm: Vec<i16> = samples[..n]
        .iter()
        .map(|x| (x.clamp(-1.0, 1.0) * 32767.0) as i16)
        .collect();
    for chunk in pcm.chunks(rate as usize) {
        p.consume(chunk);
    }
    p.finish();
    let f = p.fingerprint().to_vec();
    (!f.is_empty()).then_some(f)
}

/// Whether two fingerprints are of the same recording.
pub fn same_recording(a: &[u32], b: &[u32]) -> bool {
    let shorter = a.len().min(b.len());
    if shorter < 16 {
        return false;
    }
    let Ok(segments) = match_fingerprints(a, b, &config()) else {
        return false;
    };
    let good: usize = segments
        .iter()
        .filter(|s| s.score <= MAX_SCORE)
        .map(|s| s.items_count)
        .sum();
    good as f64 >= COVERAGE * shorter as f64
}

/// Store a track's fingerprint (`None`: none could be made).
pub(crate) fn store(
    c: &rusqlite::Connection,
    fingerprint: &str,
    print: Option<&[u32]>,
    duration: Option<f64>,
) -> rusqlite::Result<()> {
    c.execute(
        "INSERT OR REPLACE INTO track_chromaprint (fingerprint, version, print, duration, analyzed_at)
         VALUES (?, ?, ?, ?, ?)",
        rusqlite::params![
            fingerprint,
            VERSION,
            print.map(to_blob),
            duration,
            crate::scanner::now()
        ],
    )?;
    Ok(())
}

/// A local track's fingerprint is new: mark the streamed tracks it is a copy of.
pub(crate) fn match_local(
    c: &rusqlite::Connection,
    fingerprint: &str,
    print: &[u32],
    duration: Option<f64>,
) -> rusqlite::Result<usize> {
    let Some(d) = duration else { return Ok(0) };
    let rows: Vec<(i64, Vec<u8>)> = c
        .prepare_cached(
            "SELECT r.id, p.print FROM remote_tracks r
             JOIN track_chromaprint p ON p.fingerprint = r.fingerprint AND p.version = ?3
             WHERE r.local_match IS NULL AND p.print IS NOT NULL
               AND r.duration BETWEEN ?1 - ?2 AND ?1 + ?2",
        )?
        .query_map(rusqlite::params![d, LENGTH_SLACK, VERSION], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })?
        .collect::<Result<_, _>>()?;
    let mut n = 0;
    for (id, blob) in rows {
        if same_recording(print, &from_blob(&blob)) {
            n += c.execute(
                "UPDATE remote_tracks SET local_match = ? WHERE id = ?",
                rusqlite::params![fingerprint, id],
            )?;
        }
    }
    Ok(n)
}

/// A streamed track's fingerprint: the audio fingerprint of the library's copy, if any.
pub(crate) fn find_local(
    c: &rusqlite::Connection,
    print: &[u32],
    duration: Option<f64>,
) -> rusqlite::Result<Option<String>> {
    let Some(d) = duration else { return Ok(None) };
    let rows: Vec<(String, Vec<u8>)> = c
        .prepare_cached(
            "SELECT t.fingerprint, p.print FROM tracks t
             JOIN track_chromaprint p ON p.fingerprint = t.fingerprint AND p.version = ?3
             WHERE p.print IS NOT NULL AND t.duration BETWEEN ?1 - ?2 AND ?1 + ?2
             GROUP BY t.fingerprint",
        )?
        .query_map(rusqlite::params![d, LENGTH_SLACK, VERSION], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })?
        .collect::<Result<_, _>>()?;
    Ok(rows
        .into_iter()
        .find(|(_, blob)| same_recording(print, &from_blob(blob)))
        .map(|(fp, _)| fp))
}

/// Copies of a recording differ this much in length at most (seconds): a few seconds of
/// silence or fade more or less.
const LENGTH_SLACK: f64 = 4.0;

pub fn to_blob(f: &[u32]) -> Vec<u8> {
    f.iter().flat_map(|x| x.to_le_bytes()).collect()
}

pub fn from_blob(b: &[u8]) -> Vec<u32> {
    b.chunks_exact(4)
        .map(|c| u32::from_le_bytes([c[0], c[1], c[2], c[3]]))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Something like music: a changing chord over noise, from `seed`.
    fn music(seed: u32, secs: usize) -> Vec<f32> {
        let rate = 22050;
        let mut x = seed.wrapping_mul(2654435761) | 1;
        (0..secs * rate)
            .map(|i| {
                let t = i as f32 / rate as f32;
                x ^= x << 13;
                x ^= x >> 17;
                x ^= x << 5;
                let noise = (x as f32 / u32::MAX as f32 - 0.5) * 0.05;
                // its own tune: tempo, key and steps depend on the seed
                let step = ((t * (1.5 + seed as f32)) as u32 * (2 + seed)) % 7;
                let f = 110.0 * (1.0 + 0.37 * seed as f32) * 2f32.powf(step as f32 / 12.0 * 3.0);
                0.3 * (2.0 * std::f32::consts::PI * f * t).sin()
                    + 0.2 * (2.0 * std::f32::consts::PI * f * 1.5 * t).sin()
                    + noise
            })
            .collect()
    }

    #[test]
    fn the_same_recording_matches_and_others_do_not() {
        let a = music(1, 60);
        // another copy: quieter, with a little more silence in front
        let mut copy = vec![0.0; 22050];
        copy.extend(a.iter().map(|x| x * 0.7));
        let other = music(2, 60);
        let fa = fingerprint(&a, 22050).unwrap();
        let fc = fingerprint(&copy, 22050).unwrap();
        let fo = fingerprint(&other, 22050).unwrap();
        assert!(same_recording(&fa, &fc));
        assert!(!same_recording(&fa, &fo));
        assert_eq!(from_blob(&to_blob(&fa)), fa);
        // too little audio
        assert!(fingerprint(&a[..22050], 22050).is_none());
    }
}
