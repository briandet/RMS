//! Test fixture generator: tiny audio files produced by the `ffmpeg` binary.

#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::process::Command;

use rms_library::{Library, LibraryConfig};

pub fn ffmpeg_available() -> bool {
    Command::new("ffmpeg")
        .arg("-version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Generate an audio file with metadata. `cover`: optional image to embed as attached picture.
pub fn make_audio(path: &Path, secs: f64, meta: &[(&str, &str)], cover: Option<&Path>) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let ext = path.extension().unwrap().to_string_lossy().to_string();
    let mut cmd = Command::new("ffmpeg");
    cmd.args([
        "-y",
        "-hide_banner",
        "-loglevel",
        "error",
        "-f",
        "lavfi",
        "-i",
    ]);
    cmd.arg(format!("sine=frequency=440:duration={}", secs));
    if let Some(c) = cover {
        cmd.arg("-i").arg(c);
        cmd.args(["-map", "0", "-map", "1"]);
    }
    match ext.as_str() {
        "flac" => {
            cmd.args(["-c:a", "flac"]);
        }
        "mp3" => {
            cmd.args(["-c:a", "libmp3lame", "-b:a", "128k", "-id3v2_version", "3"]);
        }
        "m4a" => {
            cmd.args(["-c:a", "aac", "-b:a", "96k"]);
        }
        "ogg" => {
            cmd.args(["-c:a", "libvorbis"]);
        }
        "wav" => {
            cmd.args(["-c:a", "pcm_s16le"]);
        }
        _ => {}
    }
    if cover.is_some() {
        let vcodec = if ext == "m4a" { "mjpeg" } else { "png" };
        cmd.args(["-c:v", vcodec, "-disposition:v", "attached_pic"]);
    }
    for (k, v) in meta {
        cmd.arg("-metadata").arg(format!("{}={}", k, v));
    }
    cmd.arg(path);
    let out = cmd.output().expect("ffmpeg");
    assert!(
        out.status.success(),
        "ffmpeg failed for {}: {}",
        path.display(),
        String::from_utf8_lossy(&out.stderr)
    );
}

pub fn make_image(path: &Path, color: &str, w: u32, h: u32) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let out = Command::new("ffmpeg")
        .args([
            "-y",
            "-hide_banner",
            "-loglevel",
            "error",
            "-f",
            "lavfi",
            "-i",
        ])
        .arg(format!("color=c={}:s={}x{}", color, w, h))
        .args(["-frames:v", "1"])
        .arg(path)
        .output()
        .expect("ffmpeg");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

pub struct Fixture {
    pub dir: tempfile::TempDir,
    pub media: PathBuf,
    pub playlists: PathBuf,
    pub lib: Library,
}

/// Build the standard test library:
///
/// ```text
/// media/Artist A/Album One/01 - Song One.flac   (embedded cover)
/// media/Artist A/Album One/02 - Song Two.flac   (embedded cover)
/// media/Artist B/Album Two/01 - The Mp3 Song.mp3 (+ folder.jpg)
/// media/Comp/Hits/0{1,2,3}.m4a                  (three different artists → compilation)
/// media/Cue/album.flac + album.cue              (2 cue tracks)
/// media/EmbeddedCue/live.flac                   (CUESHEET vorbis comment)
/// media/.hidden/secret.mp3                      (ignored)
/// playlists/Favourites.m3u
/// ```
pub fn build() -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let media = dir.path().join("media");
    let playlists = dir.path().join("playlists");
    std::fs::create_dir_all(&playlists).unwrap();
    let cover = dir.path().join("cover_src.png");
    make_image(&cover, "red", 64, 48);

    let a1 = media.join("Artist A/Album One");
    make_audio(
        &a1.join("01 - Song One.flac"),
        2.0,
        &[
            ("title", "Song One"),
            ("artist", "Artist A"),
            ("album", "Album One"),
            ("TRACKNUMBER", "1"),
            ("genre", "Rock"),
            ("date", "2001-05-01"),
            ("comment", "first comment"),
        ],
        Some(&cover),
    );
    make_audio(
        &a1.join("02 - Song Two.flac"),
        2.0,
        &[
            ("title", "Song Two"),
            ("artist", "Artist A"),
            ("album", "Album One"),
            ("TRACKNUMBER", "2"),
            ("genre", "Rock"),
            ("date", "2001"),
        ],
        Some(&cover),
    );

    let b = media.join("Artist B/Album Two");
    make_audio(
        &b.join("01 - The Mp3 Song.mp3"),
        2.0,
        &[
            ("title", "The Mp3 Song"),
            ("artist", "The Artist B"),
            ("album", "Album Two"),
            ("track", "1/10"),
            ("genre", "Jazz; Blues"),
            ("date", "1999"),
            ("composer", "Some Composer"),
        ],
        None,
    );
    make_image(&b.join("folder.jpg"), "blue", 80, 80);

    let c = media.join("Comp/Hits");
    for (i, artist) in ["Xavier", "Yolanda", "Zed"].iter().enumerate() {
        make_audio(
            &c.join(format!("0{}.m4a", i + 1)),
            1.0,
            &[
                ("title", &format!("Hit {}", i + 1)),
                ("artist", artist),
                ("album", "Hits"),
                ("track", &format!("{}", i + 1)),
                ("genre", "Pop"),
            ],
            None,
        );
    }

    let cue_dir = media.join("Cue");
    make_audio(&cue_dir.join("album.flac"), 6.0, &[], None);
    std::fs::write(
        cue_dir.join("album.cue"),
        "REM GENRE Classical\nREM DATE 1985\nPERFORMER \"Cue Band\"\nTITLE \"Cue Album\"\nFILE \"album.flac\" WAVE\n  TRACK 01 AUDIO\n    TITLE \"Cue One\"\n    INDEX 01 00:00:00\n  TRACK 02 AUDIO\n    TITLE \"Cue Two\"\n    INDEX 01 00:03:00\n",
    )
    .unwrap();

    let emb = media.join("EmbeddedCue");
    make_audio(
        &emb.join("live.flac"),
        4.0,
        &[
            ("artist", "Live Artist"),
            ("album", "Live Album"),
            ("CUESHEET", "FILE \"live.flac\" FLAC\n  TRACK 01 AUDIO\n    TITLE \"Live One\"\n    INDEX 01 00:00:00\n  TRACK 02 AUDIO\n    TITLE \"Live Two\"\n    INDEX 01 00:02:00\n"),
        ],
        None,
    );

    make_audio(
        &media.join(".hidden/secret.mp3"),
        1.0,
        &[("title", "Secret")],
        None,
    );

    std::fs::write(
        playlists.join("Favourites.m3u"),
        format!(
            "#EXTM3U\n#EXTINF:2,Song Two\n{}\n{}\nhttp://example.com/stream.mp3\n",
            a1.join("02 - Song Two.flac").display(),
            b.join("01 - The Mp3 Song.mp3").display()
        ),
    )
    .unwrap();

    let lib = Library::open(&dir.path().join("library.db"), &dir.path().join("cache")).unwrap();
    let cfg = LibraryConfig {
        media_dirs: vec![media.clone()],
        playlist_dir: Some(playlists.clone()),
        ..Default::default()
    };
    lib.set_config(cfg);
    Fixture {
        dir,
        media,
        playlists,
        lib,
    }
}
