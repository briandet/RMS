mod common;

use std::process::Command;

use rms_library::{LibQuery, LibQueryName, Library, LibraryConfig, ScanMode};

fn gen(path: &std::path::Path, codec_args: &[&str], meta: &[(&str, &str)]) -> bool {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let mut cmd = Command::new("ffmpeg");
    cmd.args([
        "-y",
        "-hide_banner",
        "-loglevel",
        "error",
        "-f",
        "lavfi",
        "-i",
        "sine=frequency=440:duration=1.5",
    ]);
    cmd.args(codec_args);
    for (k, v) in meta {
        cmd.arg("-metadata").arg(format!("{}={}", k, v));
    }
    cmd.arg(path);
    cmd.output().map(|o| o.status.success()).unwrap_or(false)
}

#[test]
fn many_formats() {
    if !common::ffmpeg_available() {
        eprintln!("ffmpeg not available - skipping");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let media = dir.path().join("media");
    let cases: Vec<(&str, Vec<&str>, &str, bool)> = vec![
        ("a.ogg", vec!["-c:a", "libvorbis"], "ogg", false),
        ("b.opus", vec!["-c:a", "libopus"], "ops", false),
        ("c.wav", vec!["-c:a", "pcm_s16le"], "wav", false),
        (
            "d.aiff",
            vec!["-c:a", "pcm_s16be", "-write_id3v2", "1"],
            "aif",
            false,
        ),
        ("e.m4a", vec!["-c:a", "alac"], "alc", true),
        ("f.wv", vec!["-c:a", "wavpack"], "wvp", false),
        ("g.m4a", vec!["-c:a", "aac"], "mp4", false),
    ];
    let mut made = Vec::new();
    for (name, args, ct, lossless) in &cases {
        let title = format!("Title {}", name);
        if gen(
            &media.join("Fmt").join(name),
            args,
            &[
                ("title", &title),
                ("artist", "Format Artist"),
                ("album", "Formats"),
            ],
        ) {
            made.push((name.to_string(), ct.to_string(), *lossless, title));
        }
    }
    eprintln!(
        "generated: {:?}",
        made.iter().map(|m| m.0.as_str()).collect::<Vec<_>>()
    );
    // a corrupt file must not abort the scan
    std::fs::write(media.join("Fmt/broken.flac"), b"not a flac file at all").unwrap();
    std::fs::write(media.join("Fmt/broken.mp3"), b"\xff\xfbgarbage").unwrap();

    let lib = Library::open(&dir.path().join("db.sqlite"), &dir.path().join("cache")).unwrap();
    lib.set_config(LibraryConfig {
        media_dirs: vec![media.clone()],
        ..Default::default()
    });
    let s = lib.scan(ScanMode::Full).unwrap();
    assert!(!s.aborted);

    let r = lib
        .query(&LibQuery::new(LibQueryName::Titles, 0, 100).param("tags", "uoaldQITr"))
        .unwrap();
    let rows = r["titles_loop"].as_array().cloned().unwrap_or_default();
    for (name, ct, lossless, title) in &made {
        let row = rows
            .iter()
            .find(|x| x["url"].as_str().unwrap().ends_with(name.as_str()))
            .unwrap_or_else(|| panic!("{} not scanned: {:?}", name, rows));
        assert_eq!(
            row["type"].as_str().unwrap(),
            ct,
            "content type of {}",
            name
        );
        assert_eq!(row["title"].as_str().unwrap(), title, "title of {}", name);
        assert_eq!(
            row["artist"].as_str().unwrap(),
            "Format Artist",
            "artist of {}",
            name
        );
        assert_eq!(row["album"].as_str().unwrap(), "Formats");
        let d = row["duration"].as_f64().unwrap();
        assert!((d - 1.5).abs() < 0.2, "duration of {}: {}", name, d);
        assert!(row["samplerate"].as_u64().unwrap() > 0);
        if *lossless {
            assert_eq!(row["lossless"], 1);
        }
    }
    // broken flac is still listed (title from the file name), broken mp3 is skipped like LMS
    assert!(rows
        .iter()
        .any(|x| x["url"].as_str().unwrap().ends_with("broken.flac")));
    assert!(!rows
        .iter()
        .any(|x| x["url"].as_str().unwrap().ends_with("broken.mp3")));
    // all in one album
    let r = lib
        .query(&LibQuery::new(LibQueryName::Albums, 0, 10).param("tags", "l"))
        .unwrap();
    assert_eq!(r["count"].as_u64().unwrap(), 2, "{:?}", r); // "Formats" + "No Album" (broken.flac)
}

/// `cargo test -p rms-library --release --test formats bench_scan -- --ignored --nocapture`
#[test]
#[ignore]
fn bench_scan() {
    if !common::ffmpeg_available() {
        return;
    }
    let n: usize = std::env::var("BENCH_FILES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(2000);
    let dir = tempfile::tempdir().unwrap();
    let media = dir.path().join("media");
    let src = dir.path().join("src.flac");
    common::make_audio(
        &src,
        1.0,
        &[("title", "T"), ("artist", "A"), ("album", "B")],
        None,
    );
    let src_mp3 = dir.path().join("src.mp3");
    common::make_audio(
        &src_mp3,
        1.0,
        &[("title", "T"), ("artist", "A"), ("album", "B")],
        None,
    );
    for i in 0..n {
        let d = media.join(format!("Artist {}/Album {}", i / 100, i / 10));
        std::fs::create_dir_all(&d).unwrap();
        let (s, ext) = if i % 2 == 0 {
            (&src, "flac")
        } else {
            (&src_mp3, "mp3")
        };
        std::fs::copy(s, d.join(format!("{:02} Track {}.{}", i % 10, i, ext))).unwrap();
    }
    let lib = Library::open(&dir.path().join("db.sqlite"), &dir.path().join("cache")).unwrap();
    lib.set_config(LibraryConfig {
        media_dirs: vec![media.clone()],
        ..Default::default()
    });
    let t = std::time::Instant::now();
    let s = lib.scan(ScanMode::Full).unwrap();
    eprintln!("full scan of {} files: {:?} ({:?})", n, t.elapsed(), s);
    let t = std::time::Instant::now();
    let s = lib.scan(ScanMode::Full).unwrap();
    eprintln!("no-op rescan: {:?} ({:?})", t.elapsed(), s);
    let t = std::time::Instant::now();
    let r = lib
        .query(&LibQuery::new(LibQueryName::Albums, 0, 50).param("tags", "lyjaS"))
        .unwrap();
    eprintln!("albums query: {:?} count {}", t.elapsed(), r["count"]);
    let t = std::time::Instant::now();
    let r = lib
        .query(
            &LibQuery::new(LibQueryName::Titles, 0, 50)
                .param("tags", "galdAc")
                .param("search", "track"),
        )
        .unwrap();
    eprintln!("titles search: {:?} count {}", t.elapsed(), r["count"]);
    let t = std::time::Instant::now();
    let _ = lib.totals();
    eprintln!("totals: {:?}", t.elapsed());
}
