//! Writing tags: the fields asked for change, everything else in the file (other tags, the
//! audio) stays as it was, in every format RMS writes.

mod common;

use rms_library::tags::{first, read_tags, TagOptions};
use rms_library::tagwrite::{self, Values};
use std::path::Path;
use std::process::Command;

fn gen(path: &Path, codec_args: &[&str], meta: &[(&str, &str)]) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let mut cmd = Command::new("ffmpeg");
    cmd.args(["-y", "-loglevel", "error", "-f", "lavfi", "-i"])
        .arg("sine=frequency=440:duration=1.5")
        .args(codec_args);
    for (k, v) in meta {
        cmd.arg("-metadata").arg(format!("{k}={v}"));
    }
    assert!(cmd.arg(path).status().unwrap().success(), "{path:?}");
}

fn values(pairs: &[(&str, Option<&str>)]) -> Values {
    pairs
        .iter()
        .map(|(k, v)| (k.to_string(), v.map(str::to_string)))
        .collect()
}

/// The decoded audio's checksum.
fn audio_fingerprint(path: &Path) -> String {
    let out = Command::new("ffmpeg")
        .args(["-loglevel", "error", "-i"])
        .arg(path)
        .args(["-map", "0:a", "-f", "md5", "-"])
        .output()
        .unwrap();
    assert!(out.status.success());
    String::from_utf8(out.stdout).unwrap()
}

#[test]
fn tags_change_and_nothing_else_does() {
    if !common::ffmpeg_available() {
        eprintln!("ffmpeg not available - skipping");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let scratch = dir.path().join("scratch");
    let meta = [
        ("title", "Old Title"),
        ("artist", "Old Artist"),
        ("album", "Old Album"),
        ("comment", "keep me"),
        ("composer", "Keep Composer"),
    ];
    let cases: &[(&str, &[&str])] = &[
        ("a.flac", &["-c:a", "flac"]),
        ("b.ogg", &["-c:a", "libvorbis"]),
        ("c.opus", &["-c:a", "libopus"]),
        ("d.mp3", &["-c:a", "libmp3lame", "-id3v2_version", "4"]),
        ("e.m4a", &["-c:a", "aac"]),
        ("f.aiff", &["-c:a", "pcm_s16be", "-write_id3v2", "1"]),
        ("g.wv", &["-c:a", "wavpack"]),
    ];
    for (name, args) in cases {
        let path = dir.path().join(name);
        gen(&path, args, &meta);
        tagwrite::check_writable(&path).unwrap();
        let audio = audio_fingerprint(&path);
        let before = tagwrite::read(&path).unwrap();
        assert_eq!(
            before["title"].as_deref(),
            Some("Old Title"),
            "{name}: {before:?}"
        );

        let changes = values(&[
            ("title", Some("New Title")),
            ("album_artist", Some("The Band")),
            ("track_number", Some("3")),
            ("track_total", Some("12")),
            ("date", Some("1991-09-24")),
            (
                "musicbrainz_recording_id",
                Some("11111111-1111-1111-1111-111111111111"),
            ),
            (
                "musicbrainz_release_id",
                Some("22222222-2222-2222-2222-222222222222"),
            ),
            ("album", None),
        ]);
        tagwrite::write(&path, &changes, &scratch).unwrap_or_else(|e| panic!("{name}: {e:#}"));
        let after = tagwrite::read(&path).unwrap();
        for (k, v) in &changes {
            assert_eq!(&after[k], v, "{name}: {k}: {after:?}");
        }
        assert_eq!(after["artist"].as_deref(), Some("Old Artist"), "{name}");

        // the rest of the file as it was, as RMS's scanner reads it
        let t = read_tags(&path, &TagOptions::default()).unwrap();
        assert_eq!(first(&t.attrs, "TITLE"), Some("New Title"), "{name}: {t:?}");
        assert_eq!(
            first(&t.attrs, "COMPOSER"),
            Some("Keep Composer"),
            "{name}: {t:?}"
        );
        assert!(
            t.attrs.keys().any(|k| k.starts_with("COMMENT")),
            "{name}: comment lost: {t:?}"
        );
        assert_eq!(first(&t.attrs, "ALBUM"), None, "{name}");
        assert_eq!(audio_fingerprint(&path), audio, "{name}: the audio changed");

        // and back
        let undo: Values = changes
            .keys()
            .map(|k| (k.clone(), before[k].clone()))
            .collect();
        tagwrite::write(&path, &undo, &scratch).unwrap();
        let restored = tagwrite::read(&path).unwrap();
        assert_eq!(restored, before, "{name}");
    }
    // nothing left behind
    assert_eq!(std::fs::read_dir(&scratch).unwrap().count(), 0);
}

#[test]
fn unknown_tags_and_unwritable_files_are_refused() {
    if !common::ffmpeg_available() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("a.flac");
    gen(&path, &["-c:a", "flac"], &[("title", "T")]);
    let bad = values(&[("mood", Some("happy"))]);
    assert!(tagwrite::write(&path, &bad, &dir.path().join("s")).is_err());
    assert!(tagwrite::check_writable(&dir.path().join("x.wma")).is_err());
}
