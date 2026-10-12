//! Reading and writing the tags RMS can change in music files: titles, artists, numbers,
//! dates and MusicBrainz ids (see [`FIELDS`]), by the names MusicBrainz Picard uses.
//!
//! Only the fields asked for change; everything else in the file stays as it was: Vorbis
//! comments (FLAC, Ogg, Opus) are edited directly, other tags (ID3v2, MP4, APE) through lofty's
//! split and merge, which keeps what a generic tag can't hold (pictures, unknown frames).  The
//! file is rewritten in place, so its owner, permissions and links are kept, and a copy of it
//! (in a folder of RMS's own) puts it back if writing fails; the audio itself is not touched.

use lofty::config::{ParseOptions, WriteOptions};
use lofty::file::{AudioFile, FileType};
use lofty::ogg::VorbisComments;
use lofty::tag::{ItemKey, ItemValue, MergeTag, SplitTag, Tag, TagItem};
use std::collections::BTreeMap;
use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

/// A tag RMS writes: its name (as the API has it), its Vorbis comment, and its generic key.
pub struct Field {
    pub name: &'static str,
    vorbis: &'static str,
    /// Other names a Vorbis comment may have for it, removed when it is written.
    vorbis_aliases: &'static [&'static str],
    key: ItemKey,
}

macro_rules! fields {
    ($($name:literal => $vorbis:literal $([$($alias:literal),*])?, $key:ident;)*) => {
        /// The tags RMS can change.
        pub const FIELDS: &[Field] = &[$(Field {
            name: $name,
            vorbis: $vorbis,
            vorbis_aliases: &[$($($alias),*)?],
            key: ItemKey::$key,
        }),*];
    };
}

fields! {
    "title" => "TITLE", TrackTitle;
    "artist" => "ARTIST", TrackArtist;
    "album" => "ALBUM", AlbumTitle;
    "album_artist" => "ALBUMARTIST" ["ALBUM ARTIST", "ALBUM_ARTIST"], AlbumArtist;
    "track_number" => "TRACKNUMBER", TrackNumber;
    "track_total" => "TRACKTOTAL" ["TOTALTRACKS"], TrackTotal;
    "disc_number" => "DISCNUMBER", DiscNumber;
    "disc_total" => "DISCTOTAL" ["TOTALDISCS"], DiscTotal;
    "date" => "DATE", RecordingDate;
    "original_date" => "ORIGINALDATE", OriginalReleaseDate;
    "label" => "LABEL", Label;
    "catalog_number" => "CATALOGNUMBER", CatalogNumber;
    "barcode" => "BARCODE", Barcode;
    "musicbrainz_recording_id" => "MUSICBRAINZ_TRACKID", MusicBrainzRecordingId;
    "musicbrainz_track_id" => "MUSICBRAINZ_RELEASETRACKID", MusicBrainzTrackId;
    "musicbrainz_release_id" => "MUSICBRAINZ_ALBUMID", MusicBrainzReleaseId;
    "musicbrainz_release_group_id" => "MUSICBRAINZ_RELEASEGROUPID", MusicBrainzReleaseGroupId;
    "musicbrainz_artist_id" => "MUSICBRAINZ_ARTISTID", MusicBrainzArtistId;
    "musicbrainz_album_artist_id" => "MUSICBRAINZ_ALBUMARTISTID", MusicBrainzReleaseArtistId;
}

impl Field {
    /// The generic key for the field in a tag of this type.
    fn key_for(&self, tt: lofty::tag::TagType) -> ItemKey {
        match (&self.key, tt) {
            // APE has no recording date; Picard writes the date as "Year"
            (ItemKey::RecordingDate, lofty::tag::TagType::Ape) => ItemKey::Year,
            (k, _) => k.clone(),
        }
    }
}

pub fn field(name: &str) -> Option<&'static Field> {
    FIELDS.iter().find(|f| f.name == name)
}

/// Values by field name; `None`: the tag is absent (or, written, removed).
pub type Values = BTreeMap<String, Option<String>>;

/// Why a file's tags can't be changed, if they can't.
pub fn check_writable(path: &Path) -> Result<(), String> {
    if file_type(path).is_none() {
        return Err("RMS can't write tags in this kind of file".into());
    }
    OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)
        .map(drop)
        .map_err(|e| format!("can't write {}: {e}", path.display()))
}

fn file_type(path: &Path) -> Option<FileType> {
    crate::tags::file_type_for(path).filter(|t| {
        matches!(
            t,
            FileType::Flac
                | FileType::Vorbis
                | FileType::Opus
                | FileType::Mpeg
                | FileType::Mp4
                | FileType::Ape
                | FileType::WavPack
                | FileType::Aiff
                | FileType::Wav
        )
    })
}

fn parse_options() -> ParseOptions {
    ParseOptions::new().read_properties(true)
}

/// Every field's value in the file (several values of a tag joined with "; ").
pub fn read(path: &Path) -> anyhow::Result<Values> {
    let ft = file_type(path).ok_or_else(|| anyhow::anyhow!("unsupported file type"))?;
    let mut f = File::open(path)?;
    let po = parse_options();
    let mut out = Values::new();
    let from_vorbis = |vc: Option<&VorbisComments>, out: &mut Values| {
        for fd in FIELDS {
            let v = vc.and_then(|vc| {
                let vals: Vec<&str> = std::iter::once(fd.vorbis)
                    .chain(fd.vorbis_aliases.iter().copied())
                    .find_map(|k| {
                        let v: Vec<&str> = vc.get_all(k).collect();
                        (!v.is_empty()).then_some(v)
                    })?;
                Some(vals.join("; "))
            });
            out.insert(fd.name.to_string(), v);
        }
    };
    let from_tag = |tag: Option<Tag>, out: &mut Values| {
        for fd in FIELDS {
            let v = tag.as_ref().and_then(|t| {
                let key = fd.key_for(t.tag_type());
                let vals: Vec<&str> = t.get_strings(&key).collect();
                (!vals.is_empty()).then(|| vals.join("; "))
            });
            out.insert(fd.name.to_string(), v);
        }
    };
    match ft {
        FileType::Flac => {
            let ff = lofty::flac::FlacFile::read_from(&mut f, po)?;
            from_vorbis(ff.vorbis_comments(), &mut out);
        }
        FileType::Vorbis => {
            let vf = lofty::ogg::VorbisFile::read_from(&mut f, po)?;
            from_vorbis(Some(vf.vorbis_comments()), &mut out);
        }
        FileType::Opus => {
            let of = lofty::ogg::OpusFile::read_from(&mut f, po)?;
            from_vorbis(Some(of.vorbis_comments()), &mut out);
        }
        FileType::Mpeg => {
            let mf = lofty::mpeg::MpegFile::read_from(&mut f, po)?;
            from_tag(mf.id3v2().map(|t| t.clone().split_tag().1), &mut out);
        }
        FileType::Aiff => {
            let af = lofty::iff::aiff::AiffFile::read_from(&mut f, po)?;
            from_tag(af.id3v2().map(|t| t.clone().split_tag().1), &mut out);
        }
        FileType::Wav => {
            let wf = lofty::iff::wav::WavFile::read_from(&mut f, po)?;
            from_tag(wf.id3v2().map(|t| t.clone().split_tag().1), &mut out);
        }
        FileType::Mp4 => {
            let mf = lofty::mp4::Mp4File::read_from(&mut f, po)?;
            from_tag(mf.ilst().map(|t| t.clone().split_tag().1), &mut out);
        }
        FileType::Ape => {
            let af = lofty::ape::ApeFile::read_from(&mut f, po)?;
            from_tag(af.ape().map(|t| t.clone().split_tag().1), &mut out);
        }
        FileType::WavPack => {
            let wf = lofty::wavpack::WavPackFile::read_from(&mut f, po)?;
            from_tag(wf.ape().map(|t| t.clone().split_tag().1), &mut out);
        }
        _ => anyhow::bail!("unsupported file type"),
    }
    Ok(out)
}

/// Change the given fields (`None`: remove the tag), leaving everything else as it is.
/// `scratch` is a folder of RMS's own for the copy kept while writing.
pub fn write(path: &Path, changes: &Values, scratch: &Path) -> anyhow::Result<()> {
    let ft = file_type(path).ok_or_else(|| anyhow::anyhow!("unsupported file type"))?;
    let changes: Vec<(&Field, Option<&str>)> = changes
        .iter()
        .map(|(k, v)| {
            let fd = field(k).ok_or_else(|| anyhow::anyhow!("unknown tag {k}"))?;
            Ok((fd, v.as_deref().map(str::trim).filter(|v| !v.is_empty())))
        })
        .collect::<anyhow::Result<_>>()?;
    if changes.is_empty() {
        return Ok(());
    }
    let before = audio_length(path, ft)?;
    let backup = Backup::new(path, scratch)?;
    let res = (|| -> anyhow::Result<()> {
        let mut f = OpenOptions::new().read(true).write(true).open(path)?;
        edit(&mut f, ft, &changes)?;
        f.sync_all()?;
        drop(f);
        // read back: the tags as asked, the audio as long as before
        let after = audio_length(path, ft)?;
        if (after - before).abs() > 0.05 {
            anyhow::bail!("the audio changed length ({before:.2}s → {after:.2}s)");
        }
        let now = read(path)?;
        for (fd, want) in &changes {
            let have = now.get(fd.name).cloned().flatten();
            if have.as_deref() != *want {
                anyhow::bail!(
                    "{} reads back as {:?}, not {:?}",
                    fd.name,
                    have.unwrap_or_default(),
                    want.unwrap_or_default()
                );
            }
        }
        Ok(())
    })();
    match res {
        Ok(()) => {
            backup.discard();
            Ok(())
        }
        Err(e) => match backup.restore() {
            Ok(()) => Err(e),
            Err(r) => Err(e.context(format!(
                "and putting the file back failed ({r}); a copy is at {}",
                backup.path.display()
            ))),
        },
    }
}

fn edit(f: &mut File, ft: FileType, changes: &[(&Field, Option<&str>)]) -> anyhow::Result<()> {
    let po = parse_options();
    let wo = WriteOptions::default();
    // (each file type's own tag, edited and written back; `save_to` writes from the start)
    macro_rules! vorbis {
        ($file:ty) => {{
            let mut file = <$file>::read_from(f, po)?;
            edit_vorbis(file.vorbis_comments_mut(), changes);
            f.seek(SeekFrom::Start(0))?;
            file.save_to(f, wo)?;
        }};
    }
    macro_rules! generic {
        ($file:ty, $take:ident, $set:ident, $tag:ty) => {{
            let mut file = <$file>::read_from(f, po)?;
            let tag: $tag = file.$take().unwrap_or_default();
            let (rest, mut generic) = tag.split_tag();
            edit_tag(&mut generic, changes);
            file.$set(rest.merge_tag(generic));
            f.seek(SeekFrom::Start(0))?;
            file.save_to(f, wo)?;
        }};
    }
    match ft {
        FileType::Flac => {
            let mut file = lofty::flac::FlacFile::read_from(f, po)?;
            let mut vc = file.remove_vorbis_comments().unwrap_or_default();
            edit_vorbis(&mut vc, changes);
            file.set_vorbis_comments(vc);
            f.seek(SeekFrom::Start(0))?;
            file.save_to(f, wo)?;
        }
        FileType::Vorbis => {
            vorbis!(lofty::ogg::VorbisFile)
        }
        FileType::Opus => {
            vorbis!(lofty::ogg::OpusFile)
        }
        FileType::Mpeg => generic!(
            lofty::mpeg::MpegFile,
            remove_id3v2,
            set_id3v2,
            lofty::id3::v2::Id3v2Tag
        ),
        FileType::Aiff => generic!(
            lofty::iff::aiff::AiffFile,
            remove_id3v2,
            set_id3v2,
            lofty::id3::v2::Id3v2Tag
        ),
        FileType::Wav => generic!(
            lofty::iff::wav::WavFile,
            remove_id3v2,
            set_id3v2,
            lofty::id3::v2::Id3v2Tag
        ),
        FileType::Mp4 => generic!(lofty::mp4::Mp4File, remove_ilst, set_ilst, lofty::mp4::Ilst),
        FileType::Ape => generic!(lofty::ape::ApeFile, remove_ape, set_ape, lofty::ape::ApeTag),
        FileType::WavPack => generic!(
            lofty::wavpack::WavPackFile,
            remove_ape,
            set_ape,
            lofty::ape::ApeTag
        ),
        _ => anyhow::bail!("unsupported file type"),
    }
    Ok(())
}

fn edit_vorbis(vc: &mut VorbisComments, changes: &[(&Field, Option<&str>)]) {
    for (fd, v) in changes {
        let _ = vc.remove(fd.vorbis).count();
        for a in fd.vorbis_aliases {
            let _ = vc.remove(a).count();
        }
        if let Some(v) = v {
            vc.push(fd.vorbis.to_string(), v.to_string());
        }
    }
}

fn edit_tag(tag: &mut Tag, changes: &[(&Field, Option<&str>)]) {
    let tt = tag.tag_type();
    for (fd, v) in changes {
        let key = fd.key_for(tt);
        tag.remove_key(&key);
        if let Some(v) = v {
            let item = TagItem::new(key, ItemValue::Text(v.to_string()));
            // keys the format stores specially (ID3v2's recording id is a UFID frame) have no
            // plain mapping, but merging writes them
            if !tag.insert(item.clone()) {
                tag.insert_unchecked(item);
            }
        }
    }
}

/// The audio's length in seconds, to tell that writing tags left it alone.
fn audio_length(path: &Path, ft: FileType) -> anyhow::Result<f64> {
    let mut f = File::open(path)?;
    let po = parse_options();
    let d = match ft {
        FileType::Flac => lofty::flac::FlacFile::read_from(&mut f, po)?
            .properties()
            .duration(),
        FileType::Vorbis => lofty::ogg::VorbisFile::read_from(&mut f, po)?
            .properties()
            .duration(),
        FileType::Opus => lofty::ogg::OpusFile::read_from(&mut f, po)?
            .properties()
            .duration(),
        FileType::Mpeg => lofty::mpeg::MpegFile::read_from(&mut f, po)?
            .properties()
            .duration(),
        FileType::Aiff => lofty::iff::aiff::AiffFile::read_from(&mut f, po)?
            .properties()
            .duration(),
        FileType::Wav => lofty::iff::wav::WavFile::read_from(&mut f, po)?
            .properties()
            .duration(),
        FileType::Mp4 => lofty::mp4::Mp4File::read_from(&mut f, po)?
            .properties()
            .duration(),
        FileType::Ape => lofty::ape::ApeFile::read_from(&mut f, po)?
            .properties()
            .duration(),
        FileType::WavPack => lofty::wavpack::WavPackFile::read_from(&mut f, po)?
            .properties()
            .duration(),
        _ => anyhow::bail!("unsupported file type"),
    };
    Ok(d.as_secs_f64())
}

/// A copy of a file being written, to put it back if writing fails.
struct Backup {
    original: PathBuf,
    path: PathBuf,
}

impl Backup {
    fn new(original: &Path, scratch: &Path) -> anyhow::Result<Backup> {
        std::fs::create_dir_all(scratch)?;
        static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let path = scratch.join(format!("{}-{n}.bak", std::process::id()));
        std::fs::copy(original, &path)?;
        Ok(Backup {
            original: original.to_path_buf(),
            path,
        })
    }

    /// Put the original contents back, in place (the same file, so its owner stays).
    fn restore(&self) -> anyhow::Result<()> {
        let mut src = File::open(&self.path)?;
        let mut dst = OpenOptions::new().write(true).open(&self.original)?;
        let mut buf = Vec::new();
        src.read_to_end(&mut buf)?;
        dst.set_len(0)?;
        dst.write_all(&buf)?;
        dst.sync_all()?;
        let _ = std::fs::remove_file(&self.path);
        Ok(())
    }

    fn discard(&self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

/// The fingerprint the scanner gives a whole file (see `scanner::fingerprint`).
pub fn file_fingerprint(path: &Path) -> Option<String> {
    let t = crate::tags::read_tags(path, &crate::tags::TagOptions::default()).ok()?;
    crate::scanner::fingerprint(path, &t.info, None)
}

impl crate::Library {
    /// The audio once fingerprinted `old` is now `new` (its tags were written: for FLAC and Ogg
    /// the fingerprint covers the whole file): what is kept by fingerprint follows it, as it
    /// follows a moved file.
    pub fn refingerprint(&self, old: &str, new: &str) -> anyhow::Result<()> {
        if old == new {
            return Ok(());
        }
        let mut c = self.conn()?;
        let tx = c.transaction()?;
        for table in [
            "track_analysis",
            "track_rhythm",
            "track_chromaprint",
            "track_musicbrainz",
            "track_release",
        ] {
            tx.execute(
                &format!("UPDATE OR REPLACE {table} SET fingerprint = ?2 WHERE fingerprint = ?1"),
                [old, new],
            )?;
        }
        tx.execute(
            "UPDATE track_stats SET fingerprint = ?2 WHERE fingerprint = ?1",
            [old, new],
        )?;
        tx.execute(
            "UPDATE tracks SET fingerprint = ?2 WHERE fingerprint = ?1",
            [old, new],
        )?;
        tx.execute(
            "UPDATE remote_tracks SET local_match = ?2 WHERE local_match = ?1",
            [old, new],
        )?;
        tx.commit()?;
        Ok(())
    }
}

/// A file's tags changed by RMS: as they were and as written.
#[derive(Debug, Clone, PartialEq)]
pub struct Edit {
    pub url: String,
    pub before: Values,
    pub after: Values,
}

/// Files changed together.
#[derive(Debug, Clone, PartialEq)]
pub struct EditBatch {
    pub batch: i64,
    /// `tags` or `move`.
    pub kind: String,
    pub album: Option<String>,
    pub files: i64,
    pub applied_at: i64,
    pub undone_at: Option<i64>,
}

/// A library track's file, for writing its tags.
#[derive(Debug, Clone, PartialEq)]
pub struct TrackFile {
    pub url: String,
    pub fingerprint: Option<String>,
    pub album: Option<String>,
    /// A segment of a file (cue sheet): its tags are not the file's.
    pub segment: bool,
}

impl crate::Library {
    /// The album's tracks: id and fingerprint.
    pub fn album_track_ids(&self, album_id: i64) -> Vec<(i64, Option<String>)> {
        let Ok(c) = self.conn() else {
            return Vec::new();
        };
        c.prepare(
            "SELECT id, fingerprint FROM tracks WHERE album_id = ? ORDER BY disc, track_number, id",
        )
        .and_then(|mut st| {
            st.query_map([album_id], |r| Ok((r.get(0)?, r.get(1)?)))?
                .collect()
        })
        .unwrap_or_default()
    }

    pub fn track_file(&self, id: i64) -> Option<TrackFile> {
        let c = self.conn().ok()?;
        c.query_row(
            "SELECT t.url, t.fingerprint, al.title, t.cue_start IS NOT NULL
             FROM tracks t LEFT JOIN albums al ON al.id = t.album_id WHERE t.id = ?",
            [id],
            |r| {
                Ok(TrackFile {
                    url: r.get(0)?,
                    fingerprint: r.get(1)?,
                    album: r.get(2)?,
                    segment: r.get(3)?,
                })
            },
        )
        .ok()
    }

    /// Remember files changed together (`kind`: `tags` or `move`); returns the batch's number.
    pub fn record_edits(
        &self,
        kind: &str,
        album: Option<&str>,
        edits: &[Edit],
    ) -> anyhow::Result<i64> {
        let mut c = self.conn()?;
        let tx = c.transaction()?;
        let batch: i64 = tx.query_row(
            "SELECT COALESCE(MAX(batch), 0) + 1 FROM tag_edits",
            [],
            |r| r.get(0),
        )?;
        let now = crate::scanner::now();
        for e in edits {
            tx.execute(
                "INSERT INTO tag_edits (batch, url, album, before, after, applied_at, kind)
                 VALUES (?, ?, ?, ?, ?, ?, ?)",
                rusqlite::params![
                    batch,
                    e.url,
                    album,
                    serde_json::to_string(&e.before)?,
                    serde_json::to_string(&e.after)?,
                    now,
                    kind
                ],
            )?;
        }
        tx.commit()?;
        Ok(batch)
    }

    /// The latest batches of changes, newest first.
    pub fn edit_batches(&self, limit: usize) -> Vec<EditBatch> {
        let Ok(c) = self.conn() else {
            return Vec::new();
        };
        c.prepare(
            "SELECT batch, MAX(album), COUNT(*), MAX(applied_at), MAX(undone_at), MAX(kind)
             FROM tag_edits GROUP BY batch ORDER BY batch DESC LIMIT ?",
        )
        .and_then(|mut st| {
            st.query_map([limit as i64], |r| {
                Ok(EditBatch {
                    batch: r.get(0)?,
                    album: r.get(1)?,
                    files: r.get(2)?,
                    applied_at: r.get(3)?,
                    undone_at: r.get(4)?,
                    kind: r.get(5)?,
                })
            })?
            .collect()
        })
        .unwrap_or_default()
    }

    /// A batch's changes; `None` when there is no such batch.
    pub fn edit_batch(&self, batch: i64) -> Option<(EditBatch, Vec<Edit>)> {
        let head = self
            .edit_batches(i64::MAX as usize)
            .into_iter()
            .find(|b| b.batch == batch)?;
        let c = self.conn().ok()?;
        let edits = c
            .prepare("SELECT url, before, after FROM tag_edits WHERE batch = ? ORDER BY id")
            .and_then(|mut st| {
                st.query_map([batch], |r| {
                    let (url, before, after): (String, String, String) =
                        (r.get(0)?, r.get(1)?, r.get(2)?);
                    Ok(Edit {
                        url,
                        before: serde_json::from_str(&before).unwrap_or_default(),
                        after: serde_json::from_str(&after).unwrap_or_default(),
                    })
                })?
                .collect()
            })
            .ok()?;
        Some((head, edits))
    }

    pub fn mark_undone(&self, batch: i64) -> anyhow::Result<()> {
        self.conn()?.execute(
            "UPDATE tag_edits SET undone_at = ? WHERE batch = ?",
            rusqlite::params![crate::scanner::now(), batch],
        )?;
        Ok(())
    }
}

impl crate::Library {
    /// Files moved from one URL to another: what the library keeps by URL follows them (play
    /// history, saved playlists and their files, the journal of tag changes).  The tracks
    /// themselves are the scanner's to move.
    pub fn rename_urls(&self, moves: &[(String, String)]) -> anyhow::Result<()> {
        if moves.is_empty() {
            return Ok(());
        }
        let mut c = self.conn()?;
        let tx = c.transaction()?;
        let mut playlists = std::collections::BTreeSet::new();
        for (old, new) in moves {
            tx.execute(
                "UPDATE OR REPLACE track_stats SET url = ?2 WHERE url = ?1",
                [old, new],
            )?;
            tx.execute(
                "UPDATE tag_edits SET url = ?2 WHERE url = ?1 AND kind = 'tags'",
                [old, new],
            )?;
            tx.execute(
                "UPDATE OR REPLACE discovered SET url = ?2 WHERE url = ?1",
                [old, new],
            )?;
            let mut st = tx.prepare(
                "SELECT DISTINCT p.id FROM playlist_items i JOIN playlists p ON p.id = i.playlist_id
                 WHERE i.url = ? AND p.kind = 'saved'",
            )?;
            for id in st.query_map([old], |r| r.get::<_, i64>(0))? {
                playlists.insert(id?);
            }
            drop(st);
            tx.execute(
                "UPDATE playlist_items SET url = ?2 WHERE url = ?1",
                [old, new],
            )?;
        }
        tx.commit()?;
        // saved playlists' files list the new places too
        for id in playlists {
            let Some((_, purl, urls)) = self.playlist_by_id(id) else {
                continue;
            };
            if let Some(path) = crate::url::path_from_file_url(&purl) {
                if let Err(e) = self.write_playlist_file(&c, &path, &urls) {
                    tracing::warn!("rewriting playlist {}: {e}", path.display());
                }
            }
        }
        Ok(())
    }

    /// The URLs of the scanner's audio files directly in a folder.
    pub fn audio_files_in(&self, dir: &Path) -> Vec<String> {
        let Ok(c) = self.conn() else {
            return Vec::new();
        };
        let prefix = crate::url::file_url_from_path(dir);
        let prefix = format!("{}/", prefix.trim_end_matches('/'));
        c.prepare("SELECT url FROM files WHERE kind = 'audio' AND substr(url, 1, length(?1)) = ?1")
            .and_then(|mut st| {
                st.query_map([&prefix], |r| r.get::<_, String>(0))?
                    .collect::<rusqlite::Result<Vec<String>>>()
            })
            .unwrap_or_default()
            .into_iter()
            .filter(|u: &String| !u[prefix.len()..].contains('/'))
            .collect()
    }
}
