//! File type table from LMS `types.conf`.

/// (id, suffixes, first mime type, slimType)
const TYPES: &[(&str, &[&str], &str, &str)] = &[
    ("aif", &["aif", "aiff"], "audio/x-aiff", "audio"),
    ("alc", &[], "audio/x-m4a-lossless", "audio"),
    ("alcx", &[], "", "audio"),
    ("ape", &["ape"], "audio/monkeys-audio", "audio"),
    ("asx", &["asx", "wax"], "video/asx", "playlist"),
    ("cue", &["cue"], "audio/x-cue", "playlist"),
    ("cur", &[], "audio/x-cue-referenced", "-"),
    ("dff", &["dff"], "audio/dff", "audio"),
    ("dir", &[], "application/directory", "list"),
    ("dsf", &["dsf"], "audio/dsf", "audio"),
    ("flc", &["flac", "flc", "fla"], "audio/x-flac", "audio"),
    ("fec", &[], "audio/x-cue-flac", "playlist"),
    ("gif", &["gif"], "image/gif", "-"),
    ("lpcm", &["lpcm", "l16", "l24"], "audio/L16", "audio"),
    ("jpg", &["jpg", "jpeg"], "image/jpeg", "-"),
    ("lnk", &[], "application/windowsshortcut", "list"),
    ("m3u", &["m3u", "m3u8"], "audio/mpegurl", "playlist"),
    ("aac", &["aac"], "audio/aac", "audio"),
    ("mp4", &["m4a", "mp4", "m4b"], "audio/m4a", "audio"),
    ("mp4x", &[], "", "audio"),
    ("mp3", &["mp2", "mp3"], "audio/mpeg", "audio"),
    ("mpc", &["mpc", "mp+"], "audio/x-musepack", "audio"),
    ("ogg", &["ogg", "oga"], "audio/x-ogg", "audio"),
    ("ogf", &["ogf"], "audio/ogg;codecs=flac", "audio"),
    ("ops", &["opus"], "audio/opus", "audio"),
    ("pcm", &["pcm"], "", "audio"),
    ("pls", &["pls"], "audio/scpls", "playlist"),
    ("png", &["png"], "image/png", "-"),
    ("sls", &[], "audio/x-m4a-sls", "audio"),
    ("wav", &["wav", "wave"], "audio/x-wav", "audio"),
    ("wma", &["wma"], "audio/x-ms-wma", "audio"),
    ("wmal", &[], "audio/x-wma-lossless", "audio"),
    ("wmap", &[], "audio/x-wma-wmapro", "audio"),
    ("wpl", &["wpl"], "application/vnd.ms-wpl", "playlist"),
    ("wvp", &["wv"], "audio/x-wavpack", "audio"),
    ("wvpx", &[], "audio/x-wavpack", "audio"),
    ("xpf", &["xspf"], "application/xspf+xml", "playlist"),
    ("ssp", &[], "audio/x-slimserverplaylist", "playlist"),
    ("cpl", &[], "audio/x-slimserverplaylist", "playlist"),
];

/// Content type id for a file extension (lower-cased lookup).
pub fn type_for_suffix(ext: &str) -> Option<&'static str> {
    let ext = ext.to_ascii_lowercase();
    TYPES
        .iter()
        .find(|(_, sfx, _, _)| sfx.contains(&ext.as_str()))
        .map(|(id, _, _, _)| *id)
}

pub fn slim_type(ct: &str) -> &'static str {
    TYPES
        .iter()
        .find(|(id, _, _, _)| *id == ct)
        .map(|(_, _, _, t)| *t)
        .unwrap_or("-")
}

pub fn mime_type(ct: &str) -> &'static str {
    TYPES
        .iter()
        .find(|(id, _, _, _)| *id == ct)
        .map(|(_, _, m, _)| *m)
        .unwrap_or("application/octet-stream")
}

pub fn is_song_type(ct: &str) -> bool {
    slim_type(ct) == "audio"
}

pub fn is_playlist_type(ct: &str) -> bool {
    slim_type(ct) == "playlist"
}

/// `typeFromPath`: strip anchor, take the extension after the last `.`.
pub fn type_from_path(path_or_url: &str) -> &'static str {
    let s = crate::url::strip_anchor(path_or_url);
    let last = s.rsplit('/').next().unwrap_or(s);
    match last.rfind('.') {
        Some(i) => type_for_suffix(&last[i + 1..]).unwrap_or("unk"),
        None => "unk",
    }
}

/// Audio extensions accepted by the scanner (`validTypeExtensions('audio')` + cue).
pub fn is_audio_ext(ext: &str, disabled: &[String]) -> bool {
    let ext = ext.to_ascii_lowercase();
    if disabled.iter().any(|d| d.eq_ignore_ascii_case(&ext)) {
        return false;
    }
    if ext == "cue" {
        return true;
    }
    match type_for_suffix(&ext) {
        Some(ct) => is_song_type(ct),
        None => false,
    }
}

/// Playlist extensions (`validTypeExtensions('list')`, minus dirs).
pub fn is_playlist_ext(ext: &str, disabled: &[String]) -> bool {
    let ext = ext.to_ascii_lowercase();
    if disabled.iter().any(|d| d.eq_ignore_ascii_case(&ext)) {
        return false;
    }
    match type_for_suffix(&ext) {
        Some(ct) => is_playlist_type(ct),
        None => false,
    }
}

/// `Slim::Music::Info::isLossy`
pub fn is_lossy(ct: &str) -> bool {
    !["flc", "ogf", "wav", "alc", "alcx", "aif", "dff", "dsf", "wvp", "wvpx"]
        .iter()
        .any(|t| ct.contains(t))
}
