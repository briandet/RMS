//! `rms-mix-eval`: how good are the similar-songs mixes?  For seed tracks from the library, it
//! makes mixes several ways (today's scoring, without sound, sound only, tags only, at random)
//! and checks each pick against ListenBrainz: a *hit* is a song by an artist ListenBrainz's
//! listeners play in the same sessions as the seed's artist.  See `rms_library::mix_eval`.
//!
//! It reads RMS's data folder (the library database, the sound analysis) and changes nothing
//! but its cache of ListenBrainz answers; RMS can keep running.  The seeds need artists with
//! MusicBrainz ids (from tags or identification).
//!
//! ```text
//! rms-mix-eval --data-dir /var/lib/rms --seeds 50
//! ```

use clap::Parser;
use rand::rngs::StdRng;
use rand::SeedableRng;
use rms_library::mix_eval::{self, Case, Mix, Variant};
use rms_library::Library;
use serde_json::{json, Value};
use std::path::PathBuf;
use std::time::Duration;

#[derive(Parser, Debug)]
#[command(
    name = "rms-mix-eval",
    version,
    about = "Judge RMS's similar-songs mixes against ListenBrainz"
)]
struct Args {
    /// RMS's data folder (with library.sqlite).
    #[arg(long, env = "RMS_DATA_DIR", default_value = "./data")]
    data_dir: PathBuf,
    /// Seed tracks to judge mixes for.
    #[arg(long, default_value_t = 50)]
    seeds: usize,
    /// Songs per mix.
    #[arg(long, default_value_t = 20)]
    size: usize,
    /// Mixes per seed and way of mixing (they differ by chance).
    #[arg(long, default_value_t = 3)]
    repeats: usize,
    /// Ways of mixing to compare, the first being the one the others are compared with.
    #[arg(
        long,
        value_delimiter = ',',
        default_value = "current,no-sound,sound-only,tags-only,random"
    )]
    variants: Vec<String>,
    /// A seed's artist needs this many songs in the library by artists ListenBrainz finds
    /// similar (with fewer, no mix can do well and the seed tells nothing).
    #[arg(long, default_value_t = 3)]
    min_similar: usize,
    /// For choosing seeds (the same number, the same seeds).
    #[arg(long, default_value_t = 1)]
    rng_seed: u64,
    /// Print every seed's mixes.
    #[arg(long)]
    details: bool,
    /// Write everything (seeds, mixes, summaries) as JSON to this file.
    #[arg(long)]
    json: Option<PathBuf>,
    /// Only use ListenBrainz answers already cached.
    #[arg(long)]
    offline: bool,
    #[arg(long, default_value = "https://labs.api.listenbrainz.org", hide = true)]
    listenbrainz_url: String,
}

/// ListenBrainz answers are kept this long (as RMS keeps them).
const CACHE_SECS: i64 = 30 * 86400;

#[tokio::main]
async fn main() {
    if let Err(e) = run(Args::parse()).await {
        eprintln!("rms-mix-eval: {e}");
        std::process::exit(1);
    }
}

async fn run(args: Args) -> anyhow::Result<()> {
    let variants: Vec<Variant> = args
        .variants
        .iter()
        .map(|v| Variant::parse(v).ok_or_else(|| anyhow::anyhow!("unknown variant {v}")))
        .collect::<Result<_, _>>()?;
    let db = args.data_dir.join("library.sqlite");
    if !db.exists() {
        anyhow::bail!("no library at {} (use --data-dir)", db.display());
    }
    let lib = Library::open(&db, &args.data_dir.join("cache"))?;
    let cat = lib.eval_catalog()?;
    let analysed = lib.sound_index().len();
    eprintln!(
        "library: {} tracks, {} analysed for sound",
        cat.track_artist.len(),
        analysed
    );
    if analysed == 0 {
        eprintln!("(no sound analysis yet: the sound variants mix like the others without it)");
    }

    // seeds: tracks whose artist ListenBrainz knows, with enough similar music in the library
    let http = reqwest::Client::builder()
        .user_agent(concat!(
            "RustyMusicServer/",
            env!("CARGO_PKG_VERSION"),
            " ( https://github.com/rustydc/RMS ) mix-eval"
        ))
        .timeout(Duration::from_secs(20))
        .build()?;
    let mut rng = StdRng::seed_from_u64(args.rng_seed);
    let candidates = cat.seed_candidates(&mut rng);
    let mut cases: Vec<Case> = Vec::new();
    let (mut tried, mut unknown, mut too_few) = (0, 0, 0);
    for (track, artist, mbid) in candidates {
        if cases.len() >= args.seeds {
            break;
        }
        tried += 1;
        let Some(similar) = similar_artists(&lib, &http, &args, &mbid).await else {
            unknown += 1;
            continue;
        };
        let case = lib.eval_case(&cat, track, artist, &similar);
        if similar.is_empty() {
            unknown += 1;
        } else if case.similar_tracks < args.min_similar {
            too_few += 1;
        } else {
            cases.push(case);
        }
    }
    eprintln!(
        "seeds: {} (of {tried} artists tried: {unknown} unknown to ListenBrainz, {too_few} with fewer than {} similar songs here)",
        cases.len(),
        args.min_similar
    );
    if cases.is_empty() {
        anyhow::bail!("no seeds to judge with");
    }

    // the mixes: every case, every variant, the repeats
    let mut mixes: Vec<Vec<Vec<Mix>>> = Vec::new();
    for (ci, case) in cases.iter().enumerate() {
        let mut per_variant = Vec::new();
        for (vi, v) in variants.iter().enumerate() {
            let mut ms = Vec::new();
            for r in 0..args.repeats {
                let mut rng = StdRng::seed_from_u64(
                    args.rng_seed ^ ((ci as u64) << 32 | (vi as u64) << 16 | r as u64),
                );
                ms.push(mix_eval::mix(&lib, &cat, case, *v, args.size, &mut rng));
            }
            per_variant.push(ms);
        }
        mixes.push(per_variant);
        eprint!("\rmixing: {}/{}", ci + 1, cases.len());
    }
    eprintln!();
    let summaries = mix_eval::summarize(&cases, &variants, &mixes);

    let artist_name = |track: i64| {
        cat.track_artist
            .get(&track)
            .and_then(|a| cat.names.get(a))
            .cloned()
            .unwrap_or_default()
    };
    if args.details {
        for (ci, case) in cases.iter().enumerate() {
            println!(
                "\n{} — {}  (chance {:.1}%: {} of {} songs here by similar artists)",
                lib.eval_title(case.track),
                artist_name(case.track),
                100.0 * case.chance(),
                case.similar_tracks,
                case.others
            );
            for (vi, v) in variants.iter().enumerate() {
                let m = &mixes[ci][vi][0];
                let marks: Vec<String> = m
                    .picks
                    .iter()
                    .map(|t| {
                        let mark = match cat.hit(case, *t) {
                            Some(true) => "✓",
                            Some(false) => "·",
                            None => "=",
                        };
                        format!("{mark}{}", artist_name(*t))
                    })
                    .collect();
                println!(
                    "  {:<11} {}/{}  {}",
                    v.name(),
                    m.hits,
                    m.others,
                    marks.join(", ")
                );
            }
        }
        println!(
            "\n(✓ similar artist, · other artist, = the seed's artist; the first mix of each)"
        );
    }

    let chance = cases.iter().map(Case::chance).sum::<f64>() / cases.len() as f64;
    println!(
        "\n{} seeds, {} songs per mix, {} mixes each way.  A random song from the library is a hit {:.1}% of the time (mean over seeds).\n",
        cases.len(),
        args.size,
        args.repeats,
        100.0 * chance
    );
    println!(
        "{:<11} {:>15} {:>7} {:>12}   compared with {}",
        "way",
        "hit rate",
        "lift",
        "same artist",
        variants[0].name()
    );
    for (i, s) in summaries.iter().enumerate() {
        let vs = if i == 0 {
            "—".to_string()
        } else {
            let seeds = |n: usize| {
                if n == 1 {
                    "1 seed".to_string()
                } else {
                    format!("{n} seeds")
                }
            };
            format!(
                "{} better, {} worse, {} same",
                seeds(s.wins),
                s.losses,
                s.ties
            )
        };
        println!(
            "{:<11} {:>7.1}% ± {:<4.1} {:>6.1}× {:>11.1}%   {vs}",
            s.variant,
            100.0 * s.hit_rate,
            100.0 * s.hit_rate_se,
            s.lift,
            100.0 * s.same_artist
        );
    }
    println!(
        "\nhit rate: picks by artists ListenBrainz finds similar to the seed's, of picks by other artists (± a standard error).\n\
         lift: hit rate over the chance rate (1× is no better than random).  same artist: picks by the seed's own artist."
    );

    if let Some(path) = &args.json {
        let out = json!({
            "seeds": cases.iter().enumerate().map(|(ci, c)| json!({
                "track": c.track,
                "title": lib.eval_title(c.track),
                "artist": artist_name(c.track),
                "chance": c.chance(),
                "similar_tracks": c.similar_tracks,
                "others": c.others,
                "mixes": variants.iter().enumerate().map(|(vi, v)| (v.name().to_string(),
                    Value::from(mixes[ci][vi].iter().map(|m| json!({
                        "hits": m.hits, "others": m.others, "same_artist": m.same_artist,
                        "picks": m.picks.iter().map(|t| json!({
                            "track": t, "title": lib.eval_title(*t), "artist": artist_name(*t),
                            "hit": cat.hit(c, *t),
                        })).collect::<Vec<_>>(),
                    })).collect::<Vec<_>>()))).collect::<serde_json::Map<_, _>>(),
            })).collect::<Vec<_>>(),
            "summaries": summaries,
            "args": {"seeds": args.seeds, "size": args.size, "repeats": args.repeats, "rng_seed": args.rng_seed},
        });
        std::fs::write(path, serde_json::to_string_pretty(&out)?)?;
        eprintln!("wrote {}", path.display());
    }
    Ok(())
}

/// ListenBrainz's similar artists for an artist: (MusicBrainz id, name), from RMS's cache when
/// fresh.  `None` when it can't be had.
async fn similar_artists(
    lib: &Library,
    http: &reqwest::Client,
    args: &Args,
    mbid: &str,
) -> Option<Vec<(String, String)>> {
    let url = format!(
        "{}/similar-artists/json?artist_mbids={mbid}&algorithm={}",
        args.listenbrainz_url.trim_end_matches('/'),
        rms_library::discover::LB_ARTISTS_ALGORITHM
    );
    let body = match lib.cached(&url, CACHE_SECS) {
        Some(b) => b,
        None if args.offline => return None,
        None => {
            // gently: one request at a time, a little apart
            tokio::time::sleep(Duration::from_millis(250)).await;
            let r = http.get(&url).send().await.ok()?;
            if !r.status().is_success() {
                return None;
            }
            let b = r.text().await.ok()?;
            lib.cache(&url, &b);
            b
        }
    };
    let v: Value = serde_json::from_str(&body).ok()?;
    Some(
        v.as_array()?
            .iter()
            .filter_map(|x| {
                Some((
                    x["artist_mbid"].as_str()?.to_string(),
                    x["name"].as_str()?.to_string(),
                ))
            })
            .collect(),
    )
}
