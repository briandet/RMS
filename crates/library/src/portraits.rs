//! Contributor portraits (`Slim::Music::ContributorPictureScan`): artist pictures found in the
//! `artfolder` pref folder or next to the artist's albums, stored as `artists.portrait`
//! (file URL) + `portraitid` and served as `/contributor/<portraitid>/image`.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use rusqlite::{params, Connection};

use crate::config::*;
use crate::scanner::{file_stat, image_id, Scanner};
use crate::text;
use crate::url::{file_url_from_path, path_from_file_url};

const STEP: &str = "contributor_picture";
const EXTENSIONS: &[&str] = &["jpg", "png", "jpeg", "JPG", "PNG", "JPEG"];

/// `sanitizedNameVariants`: file names an artist's picture may be stored under.
pub fn sanitized_name_variants(name: &str) -> Vec<String> {
    // wildcards and other characters conflicting with file system limitations
    let name: String = name
        .chars()
        .filter(|c| !matches!(c, ':' | '?' | '*'))
        .collect();
    let mut candidates = Vec::new();
    for v in [crate::playlist_files::cleanup_filename(&name), name] {
        // (utf8encode is a no-op on Rust strings)
        let punct = text::ignore_punct(&v);
        candidates.push(v);
        candidates.push(punct);
    }
    if let Some(last) = candidates.last() {
        candidates.push(text::transliterate_latin1(last));
    }
    let mut seen = HashSet::new();
    candidates
        .into_iter()
        .filter(|c| !c.is_empty() && seen.insert(c.clone()))
        .collect()
}

/// `imageInFolder`: first `<name>.<ext>` that exists in `folder`.
pub fn image_in_folder<S: AsRef<str>>(folder: &Path, names: &[S]) -> Option<PathBuf> {
    for name in names {
        let name = name.as_ref();
        // a raw name like "AC/DC" would point into a sub folder (or outside `folder`)
        if name.is_empty() || name.contains(['/', '\\']) {
            continue;
        }
        for ext in EXTENSIONS {
            let candidate = folder.join(format!("{}.{}", name, ext));
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    None
}

/// `generateImageId` for a picture file (None when it does not exist).
pub fn picture_id(path: &Path) -> Option<String> {
    let (mtime, size, is_dir) = file_stat(path)?;
    if is_dir {
        return None;
    }
    image_id(&file_url_from_path(path), mtime, size)
}

/// Store `path` as the portrait of contributor `id`; returns the new `portraitid`.
pub fn set_portrait(c: &Connection, id: i64, path: &Path) -> rusqlite::Result<Option<String>> {
    let Some(pid) = picture_id(path) else {
        return Ok(None);
    };
    c.execute(
        "UPDATE artists SET portrait = ?, portrait_id = ? WHERE id = ?",
        params![file_url_from_path(path), pid, id],
    )?;
    Ok(Some(pid))
}

/// `(id, name, portrait, portraitid)` of a contributor.
pub type PortraitRow = (i64, String, Option<String>, Option<String>);

/// Contributors the portrait scan looks at (album artists in the active roles, plus those
/// without albums).
pub fn candidates(c: &Connection, cfg: &LibraryConfig) -> rusqlite::Result<Vec<PortraitRow>> {
    let roles: Vec<String> = cfg
        .active_contributor_roles(false)
        .iter()
        .filter_map(|r| type_to_role(r))
        .map(|r| r.to_string())
        .collect();
    let sql = format!(
        "SELECT id, name, portrait, portrait_id
         FROM artists
           LEFT JOIN album_artists ON album_artists.artist_id = artists.id
         WHERE album_artists.role IS NULL OR album_artists.role IN ({})
         GROUP BY artists.id",
        roles.join(",")
    );
    let mut st = c.prepare(&sql)?;
    let rows = st.query_map([], |r| {
        Ok((
            r.get::<_, i64>(0)?,
            crate::songdata::get_text(r, 1)?.unwrap_or_default(),
            crate::songdata::get_text(r, 2)?.filter(|s| !s.is_empty()),
            crate::songdata::get_text(r, 3)?.filter(|s| !s.is_empty()),
        ))
    })?;
    rows.collect()
}

/// Folders holding the contributor's tracks, best candidates first: paths containing the
/// artist's name, then by role (ALBUMARTIST, ARTIST, BAND, others), then by path.
fn album_folders(c: &Connection, id: i64, names: &[String]) -> rusqlite::Result<Vec<PathBuf>> {
    let mut st = c.prepare_cached(
        "SELECT url, track_artists.role
         FROM tracks
         JOIN track_artists ON track_artists.track_id = tracks.id
         WHERE track_artists.artist_id = ? AND tracks.url LIKE 'file://%'
         GROUP BY album_id, track_artists.role",
    )?;
    let rows = st.query_map([id], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))?;
    let matcher = regex::Regex::new(&format!(
        "(?i){}",
        names
            .iter()
            .map(|n| regex::escape(n))
            .collect::<Vec<_>>()
            .join("|")
    ))
    .ok();
    let mut folders: Vec<(bool, u8, String, PathBuf)> = Vec::new();
    for (u, role) in rows.flatten() {
        let Some(dir) = path_from_file_url(&u).and_then(|p| p.parent().map(Path::to_path_buf))
        else {
            continue;
        };
        let s = dir.to_string_lossy().to_string();
        let matched = matcher.as_ref().is_some_and(|m| m.is_match(&s));
        let rank = match role {
            ROLE_ALBUMARTIST => 0,
            ROLE_ARTIST => 1,
            ROLE_BAND => 2,
            _ => 3,
        };
        folders.push((!matched, rank, s.to_lowercase(), dir));
    }
    folders.sort_by(|a, b| (a.0, a.1, &a.2).cmp(&(b.0, b.1, &b.2)));
    let mut seen = HashSet::new();
    Ok(folders
        .into_iter()
        .map(|f| f.3)
        .filter(|d| seen.insert(d.clone()))
        .collect())
}

/// Look for a picture of the artist: parent, own and grandparent folder of each album
/// folder (assuming a music/artist/album/(CDx) hierarchy), never a media root.
fn find_in_album_folders(
    folders: &[PathBuf],
    audio_dirs: &HashSet<PathBuf>,
    names: &[String],
) -> Option<PathBuf> {
    let mut names: Vec<&str> = names.iter().map(String::as_str).collect();
    names.extend(["artist", "contributor"]);
    for path in folders {
        if !path.is_dir() {
            continue;
        }
        let parent = path.parent().filter(|_| !audio_dirs.contains(path));
        let grandparent = parent
            .filter(|p| !audio_dirs.contains(*p))
            .and_then(Path::parent);
        for dir in [parent, Some(path.as_path()), grandparent]
            .into_iter()
            .flatten()
        {
            if audio_dirs.contains(dir) {
                continue;
            }
            if let Some(img) = image_in_folder(dir, &names) {
                return Some(img);
            }
        }
    }
    None
}

impl Scanner<'_> {
    /// `ContributorPictureScan::startArtworkScan`. Pictures below `keep_dir` (downloaded ones)
    /// are kept unless a local picture turns up.
    pub fn scan_contributor_pictures(
        &mut self,
        wipe: bool,
        keep_dir: Option<&Path>,
    ) -> anyhow::Result<()> {
        let art_folder = self.cfg.art_folder.clone().filter(|d| d.is_dir());
        let audio_dirs: HashSet<PathBuf> = self.cfg.media_dirs.iter().cloned().collect();
        let artists = candidates(self.conn, self.cfg)?;
        self.set_step(STEP, artists.len());
        self.begin()?;
        for (i, (id, name, portrait, portraitid)) in artists.into_iter().enumerate() {
            if self.aborted() {
                break;
            }
            self.step_done(i + 1, Some(&name));
            let names = sanitized_name_variants(&name);
            let mut img: Option<PathBuf> = None;
            let mut keep_existing = false;
            let existing = portrait
                .as_deref()
                .filter(|_| !wipe)
                .and_then(path_from_file_url);
            let existing_id = existing.as_deref().and_then(picture_id);
            // don't re-evaluate if we already have an unchanged portrait
            if let (Some(path), Some(pid)) = (&existing, &existing_id) {
                if Some(pid) == portraitid.as_ref() {
                    if !keep_dir.is_some_and(|d| path.starts_with(d)) {
                        continue;
                    }
                    keep_existing = true;
                } else {
                    img = Some(path.clone());
                }
            }
            if img.is_none() {
                if let Some(f) = &art_folder {
                    img = image_in_folder(f, &names);
                }
            }
            if img.is_none() {
                let folders = album_folders(self.conn, id, &names)?;
                img = find_in_album_folders(&folders, &audio_dirs, &names);
            }
            match img {
                Some(path) => {
                    // updated or new portrait
                    if picture_id(&path).is_some_and(|p| Some(p) != portraitid) {
                        set_portrait(self.conn, id, &path)?;
                        self.tick()?;
                    }
                }
                None if keep_existing => {}
                None => {
                    tracing::debug!("no portrait found for {}", name);
                    // the old picture vanished
                    if existing.is_some() && existing_id.is_none() {
                        self.conn.execute(
                            "UPDATE artists SET portrait = NULL, portrait_id = NULL WHERE id = ?",
                            [id],
                        )?;
                        self.tick()?;
                    }
                }
            }
        }
        self.commit()?;
        self.finish_step(STEP);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::OptionalExtension;
    use std::sync::atomic::AtomicBool;
    use std::sync::Arc;

    #[test]
    fn name_variants() {
        assert_eq!(sanitized_name_variants("Beatles"), vec!["Beatles"]);
        assert_eq!(sanitized_name_variants("AC/DC"), vec!["AC DC", "AC/DC"]);
        assert_eq!(
            sanitized_name_variants("Sigur Rós: Live?"),
            vec!["Sigur Rós Live", "Sigur Ros Live"]
        );
        assert_eq!(sanitized_name_variants("R.E.M."), vec!["R.E.M.", "R E M"]);
    }

    fn jpg(path: &Path) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let img = image::RgbImage::from_pixel(8, 8, image::Rgb([10, 200, 10]));
        img.save_with_format(path, image::ImageFormat::Jpeg)
            .unwrap();
    }

    struct Fixture {
        _tmp: tempfile::TempDir,
        root: PathBuf,
        conn: Connection,
    }

    /// A database with artists and tracks below `<tmp>/music` (no audio files needed).
    fn fixture() -> Fixture {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("music");
        std::fs::create_dir_all(&root).unwrap();
        let mut conn = Connection::open(tmp.path().join("db.sqlite")).unwrap();
        crate::db::init_connection(&mut conn).unwrap();
        crate::db::migrate(&mut conn).unwrap();
        Fixture {
            _tmp: tmp,
            root,
            conn,
        }
    }

    fn add_artist(c: &Connection, name: &str, tracks: &[(&Path, i64)]) -> i64 {
        c.execute(
            "INSERT INTO artists (name, name_sort, name_search) VALUES (?, ?, ?)",
            params![name, name.to_uppercase(), name.to_uppercase()],
        )
        .unwrap();
        let id = c.last_insert_rowid();
        for (path, role) in tracks {
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            let dir = path.parent().unwrap().to_string_lossy().to_string();
            let album: Option<i64> = c
                .query_row("SELECT id FROM albums WHERE title = ?", [&dir], |r| {
                    r.get(0)
                })
                .optional()
                .unwrap();
            let album = album.unwrap_or_else(|| {
                c.execute(
                    "INSERT INTO albums (title, title_sort, title_search) VALUES (?1, ?1, ?1)",
                    [&dir],
                )
                .unwrap();
                c.last_insert_rowid()
            });
            let u = file_url_from_path(path);
            let file = crate::scanner::upsert_file(c, &u, "audio", None, 1, 1).unwrap();
            c.execute(
                "INSERT INTO tracks (file_id, url, title, title_sort, title_search, album_id, format, added_at, updated_at)
                 VALUES (?1, ?2, ?2, ?2, ?2, ?3, 'mp3', 0, 0)",
                params![file, u, album],
            )
            .unwrap();
            let t = c.last_insert_rowid();
            c.execute(
                "INSERT INTO track_artists (role, artist_id, track_id, position) VALUES (?, ?, ?, 0)",
                params![role, id, t],
            )
            .unwrap();
            c.execute(
                "INSERT OR IGNORE INTO album_artists (role, artist_id, album_id) VALUES (?, ?, ?)",
                params![role, id, album],
            )
            .unwrap();
        }
        id
    }

    fn portrait(c: &Connection, id: i64) -> (Option<String>, Option<String>) {
        c.query_row(
            "SELECT portrait, portrait_id FROM artists WHERE id = ?",
            [id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap()
    }

    fn scan(f: &Fixture, cfg: &LibraryConfig, keep_dir: Option<&Path>) {
        let abort = AtomicBool::new(false);
        let mut sc = Scanner::new(&f.conn, cfg, &abort, Arc::new(Default::default()));
        sc.scan_contributor_pictures(false, keep_dir).unwrap();
    }

    #[test]
    fn finds_pictures_in_art_folder_and_album_folders() {
        let f = fixture();
        let art = f.root.parent().unwrap().join("artists");
        // artfolder: "<Artist Name>.jpg"
        jpg(&art.join("Pink Floyd.jpg"));
        let pf = add_artist(
            &f.conn,
            "Pink Floyd",
            &[(&f.root.join("Pink Floyd/Animals/01.flac"), ROLE_ARTIST)],
        );
        // artist.jpg in the album's parent folder; the CD sub folder leads up to it
        jpg(&f.root.join("Miles Davis/artist.jpg"));
        let md = add_artist(
            &f.conn,
            "Miles Davis",
            &[(
                &f.root.join("Miles Davis/Kind of Blue/CD1/01.flac"),
                ROLE_ALBUMARTIST,
            )],
        );
        // never look into the media root itself
        jpg(&f.root.join("artist.jpg"));
        let loose = add_artist(
            &f.conn,
            "Loose",
            &[(&f.root.join("Single/01.flac"), ROLE_ARTIST)],
        );
        // png named after the artist (punctuation stripped) in the album folder
        jpg(&f.root.join("Compilation/R E M.png"));
        let rem = add_artist(
            &f.conn,
            "R.E.M.",
            &[(&f.root.join("Compilation/01.flac"), ROLE_ARTIST)],
        );

        let cfg = LibraryConfig {
            media_dirs: vec![f.root.clone()],
            art_folder: Some(art.clone()),
            ..Default::default()
        };
        scan(&f, &cfg, None);

        let (u, pid) = portrait(&f.conn, pf);
        assert_eq!(u, Some(file_url_from_path(&art.join("Pink Floyd.jpg"))));
        assert_eq!(pid, picture_id(&art.join("Pink Floyd.jpg")));
        assert_eq!(pid.as_ref().map(String::len), Some(8));
        let (u, _) = portrait(&f.conn, md);
        assert_eq!(
            u,
            Some(file_url_from_path(&f.root.join("Miles Davis/artist.jpg")))
        );
        assert_eq!(portrait(&f.conn, loose), (None, None));
        let (u, _) = portrait(&f.conn, rem);
        assert_eq!(
            u,
            Some(file_url_from_path(&f.root.join("Compilation/R E M.png")))
        );

        // unchanged pictures are kept; a changed file gets a new id; a deleted one is dropped
        let old = portrait(&f.conn, pf).1;
        std::fs::write(art.join("Pink Floyd.jpg"), b"not really a jpeg, but bigger").unwrap();
        std::fs::remove_file(f.root.join("Miles Davis/artist.jpg")).unwrap();
        scan(&f, &cfg, None);
        let new = portrait(&f.conn, pf).1;
        assert!(new.is_some() && new != old);
        assert_eq!(portrait(&f.conn, md), (None, None));
    }

    #[test]
    fn album_folder_order_and_downloaded_pictures() {
        let f = fixture();
        // folders matching the artist's name win over role order
        jpg(&f.root.join("Various/artist.jpg"));
        jpg(&f.root.join("Nick Drake/artist.jpg"));
        let nd = add_artist(
            &f.conn,
            "Nick Drake",
            &[
                (&f.root.join("Various/Hits/01.flac"), ROLE_ALBUMARTIST),
                (&f.root.join("Nick Drake/Pink Moon/01.flac"), ROLE_ARTIST),
            ],
        );
        // a downloaded picture is replaced by a local one, but kept when there is none
        let keep = f.root.parent().unwrap().join("cache/artists");
        jpg(&keep.join("a.jpg"));
        jpg(&keep.join("b.jpg"));
        jpg(&f.root.join("Low/artist.jpg"));
        let low = add_artist(
            &f.conn,
            "Low",
            &[(&f.root.join("Low/Things/01.flac"), ROLE_ARTIST)],
        );
        let solo = add_artist(
            &f.conn,
            "Solo",
            &[(&f.root.join("Solo/X/01.flac"), ROLE_ARTIST)],
        );
        set_portrait(&f.conn, low, &keep.join("a.jpg")).unwrap();
        set_portrait(&f.conn, solo, &keep.join("b.jpg")).unwrap();

        let cfg = LibraryConfig {
            media_dirs: vec![f.root.clone()],
            ..Default::default()
        };
        scan(&f, &cfg, Some(&keep));
        let (u, _) = portrait(&f.conn, nd);
        assert_eq!(
            u,
            Some(file_url_from_path(&f.root.join("Nick Drake/artist.jpg")))
        );
        let (u, _) = portrait(&f.conn, low);
        assert_eq!(u, Some(file_url_from_path(&f.root.join("Low/artist.jpg"))));
        let (u, _) = portrait(&f.conn, solo);
        assert_eq!(u, Some(file_url_from_path(&keep.join("b.jpg"))));
    }
}
