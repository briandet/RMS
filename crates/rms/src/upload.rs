//! Adding music files to the library over the API (not part of LMS): an uploaded file is
//! written into `Uploads/` of the first music folder (optionally a subfolder of it), scanned,
//! and answered with the tracks it became.  Like changing tags, only with file changes on and
//! a login (`tagedit::blocked`), since anyone who can reach the server could otherwise fill
//! its disk.  MusicBrainz identification and the file name template then apply to it like to
//! any other file.

use crate::server::Server;
use crate::tagedit::Error;
use std::collections::BTreeSet;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
use tracing::info;

/// Largest file accepted.
pub const MAX_BYTES: usize = 1024 * 1024 * 1024;
/// Where uploads go, inside the first music folder.
pub const FOLDER: &str = "Uploads";

/// A file name that can only name a file in the upload folder: the last path component,
/// without control characters or characters Windows file systems refuse, not hidden.
pub fn safe_name(name: &str) -> Result<String, String> {
    let base = name.rsplit(['/', '\\']).next().unwrap_or("");
    let clean: String = base
        .chars()
        .filter(|c| !c.is_control())
        .map(|c| if "<>:\"|?*".contains(c) { '_' } else { c })
        .collect();
    let clean = clean.trim().trim_start_matches('.').trim().to_string();
    let ext = Path::new(&clean)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("");
    if clean.is_empty() || ext.is_empty() || clean.len() > 200 {
        return Err("name: a file name with its extension, like \"01 Song.flac\"".into());
    }
    if ext.eq_ignore_ascii_case("cue") || !rms_library::types::is_audio_ext(ext, &[]) {
        return Err(format!("not an audio file type: .{ext}"));
    }
    Ok(clean)
}

/// A relative folder inside the upload folder: plain names only, no `..`, no root.
pub fn safe_folder(folder: &str) -> Result<PathBuf, String> {
    let mut out = PathBuf::new();
    for c in Path::new(folder.trim()).components() {
        match c {
            Component::Normal(n) => {
                let n = n.to_string_lossy();
                let n = n.trim().trim_start_matches('.').trim();
                if n.is_empty()
                    || n.chars()
                        .any(|c| c.is_control() || "<>:\"|?*\\".contains(c))
                {
                    return Err(format!("folder: {folder:?} is not a plain folder name"));
                }
                out.push(n);
            }
            Component::CurDir => {}
            _ => return Err(format!("folder: {folder:?} must stay inside {FOLDER}/")),
        }
    }
    Ok(out)
}

/// `name`, or `name (2)`, `name (3)`, … whichever is free in `dir`.
fn free_path(dir: &Path, name: &str) -> PathBuf {
    let p = dir.join(name);
    if !p.exists() {
        return p;
    }
    let stem = Path::new(name)
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let ext = Path::new(name)
        .extension()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    (2..)
        .map(|n| dir.join(format!("{stem} ({n}).{ext}")))
        .find(|p| !p.exists())
        .unwrap()
}

/// Write `data` as `name` into the upload folder (or `folder` inside it), scan it, and return
/// the ids of the tracks it became (several for a file with an embedded cue sheet).
pub async fn upload(
    server: &Arc<Server>,
    name: &str,
    folder: Option<&str>,
    data: &[u8],
) -> Result<Vec<i64>, Error> {
    if let Some(why) = crate::tagedit::blocked(server) {
        return Err(Error::Forbidden(why));
    }
    let name = safe_name(name).map_err(Error::Invalid)?;
    let sub = safe_folder(folder.unwrap_or("")).map_err(Error::Invalid)?;
    if data.is_empty() {
        return Err(Error::Invalid("the file is empty".into()));
    }
    let Some(media) = server
        .prefs
        .server
        .get_string_list("mediadirs")
        .into_iter()
        .find(|d| !d.is_empty())
    else {
        return Err(Error::Invalid("the server has no music folder".into()));
    };
    let dir = Path::new(&media).join(FOLDER).join(&sub);
    // written under a hidden name first, so a scan never sees half a file
    let (path, data) = (dir.clone(), data.to_vec());
    let path = crate::util::spawn_blocking(move || -> std::io::Result<PathBuf> {
        std::fs::create_dir_all(&path)?;
        let tmp = path.join(format!(".upload-{:016x}.part", rand::random::<u64>()));
        std::fs::write(&tmp, &data)?;
        let dest = free_path(&path, &name);
        std::fs::rename(&tmp, &dest)?;
        Ok(dest)
    })
    .await
    .map_err(|e| Error::Failed(e.to_string()))?
    .map_err(|e| Error::Failed(format!("cannot write into {}: {e}", dir.display())))?;
    info!("upload: {}", path.display());

    crate::tagedit::rescan_now(server, BTreeSet::from([dir.clone()])).await;
    let url = rms_library::url::file_url_from_path(&path);
    let lib = server.library.clone();
    let u = url.clone();
    // a file is scanned on its name alone; one whose audio couldn't be read has no length
    let ids = crate::util::spawn_blocking(move || {
        let ids = lib.tracks_of_file(&u);
        let readable = !ids.is_empty()
            && ids
                .iter()
                .all(|id| lib.track(*id).and_then(|t| t.secs).unwrap_or(0.0) > 0.0);
        if readable {
            ids
        } else {
            Vec::new()
        }
    })
    .await
    .map_err(|e| Error::Failed(e.to_string()))?;
    if ids.is_empty() {
        // not something the scanner could read: take it away again
        let _ = std::fs::remove_file(&path);
        crate::tagedit::rescan_now(server, BTreeSet::from([dir])).await;
        return Err(Error::Invalid(
            "the file could not be read as music (damaged, or not what its name says)".into(),
        ));
    }
    Ok(ids)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_names() {
        assert_eq!(safe_name("01 Song.flac").unwrap(), "01 Song.flac");
        assert_eq!(safe_name("../../etc/x.mp3").unwrap(), "x.mp3");
        assert_eq!(safe_name("C:\\music\\a?b.mp3").unwrap(), "a_b.mp3");
        assert_eq!(safe_name("..hidden.ogg").unwrap(), "hidden.ogg");
        assert!(safe_name("noext").is_err());
        assert!(safe_name("photo.jpg").is_err());
        assert!(safe_name("album.cue").is_err());
        assert!(safe_name("").is_err());
        assert!(safe_name("/").is_err());
    }

    #[test]
    fn folders() {
        assert_eq!(safe_folder("").unwrap(), PathBuf::new());
        assert_eq!(
            safe_folder("Artist/Album").unwrap(),
            PathBuf::from("Artist/Album")
        );
        assert_eq!(safe_folder("./A").unwrap(), PathBuf::from("A"));
        assert!(safe_folder("../x").is_err());
        assert!(safe_folder("/etc").is_err());
        assert!(safe_folder("A/../../x").is_err());
        assert!(safe_folder("a:b").is_err());
    }

    #[test]
    fn free_names() {
        let d = tempfile::tempdir().unwrap();
        assert_eq!(free_path(d.path(), "a.mp3"), d.path().join("a.mp3"));
        std::fs::write(d.path().join("a.mp3"), b"x").unwrap();
        assert_eq!(free_path(d.path(), "a.mp3"), d.path().join("a (2).mp3"));
        std::fs::write(d.path().join("a (2).mp3"), b"x").unwrap();
        assert_eq!(free_path(d.path(), "a.mp3"), d.path().join("a (3).mp3"));
    }
}
