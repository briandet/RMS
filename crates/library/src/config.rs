//! Library configuration (the subset of LMS server prefs that affects the library).

use std::path::PathBuf;

#[derive(Debug, Clone)]
pub struct LibraryConfig {
    /// pref `mediadirs`
    pub media_dirs: Vec<PathBuf>,
    /// pref `playlistdir`
    pub playlist_dir: Option<PathBuf>,
    /// pref `ignoredarticles`
    pub ignored_articles: String,
    /// pref `splitList` (whitespace separated list of separators)
    pub split_list: String,
    /// pref `variousArtistsString` (None → "Various Artists")
    pub various_artists_string: String,
    pub no_artist_string: String,
    pub no_album_string: String,
    pub no_genre_string: String,
    /// pref `groupdiscs`
    pub group_discs: bool,
    pub composer_in_artists: bool,
    pub conductor_in_artists: bool,
    pub band_in_artists: bool,
    pub trackartist_in_artists: bool,
    pub use_unified_artists_list: bool,
    pub various_artist_auto_identification: bool,
    pub use_tpe2_as_album_artist: bool,
    pub use_tit1_as_work: bool,
    /// pref `ignoreDirRE`
    pub ignore_dir_re: Option<String>,
    /// pref `skipsentinel` (e.g. ".nomedia")
    pub skip_sentinel: Option<String>,
    /// pref `searchSubString`
    pub search_substring: bool,
    /// pref `browseagelimit`
    pub browse_age_limit: usize,
    /// pref `browsePopularityMaxDays`
    pub browse_popularity_max_days: i64,
    /// pref `itemsPerPage`
    pub items_per_page: usize,
    /// pref `ignoreReleaseTypes`
    pub ignore_release_types: bool,
    /// pref `disabledextensionsaudio`
    pub disabled_extensions_audio: Vec<String>,
    /// pref `disabledextensionsplaylist`
    pub disabled_extensions_playlist: Vec<String>,
    /// pref `coverArt` (literal basename, `%FORMAT` patterns are not supported)
    pub cover_art: String,
    /// pref `artfolder`: folder with artist pictures named after the artist
    pub art_folder: Option<PathBuf>,
    /// `!noContributorPictures`: run the contributor portrait scan
    pub contributor_pictures: bool,
    /// pref `guessFileFormats`
    pub guess_file_formats: Vec<String>,
    /// pref `worksScan`: 0 never, 1 always, 2 classical genres only
    pub works_scan: u8,
    /// pref `myClassicalGenres`
    pub my_classical_genres: String,
    /// localized string `DISC` (for `addDiscNumberToAlbumTitle`)
    pub string_disc: String,
    /// localized string `OF`
    pub string_of: String,
    /// localized string `KBPS`
    pub string_kbps: String,
    /// pref `longdateFormat` / `timeFormat` (strftime with LMS `|` markers)
    pub long_date_format: String,
    pub time_format: String,
}

impl Default for LibraryConfig {
    fn default() -> Self {
        LibraryConfig {
            media_dirs: Vec::new(),
            playlist_dir: None,
            ignored_articles: "The El La Los Las Le Les".into(),
            split_list: ";".into(),
            various_artists_string: "Various Artists".into(),
            no_artist_string: "No Artist".into(),
            no_album_string: "No Album".into(),
            no_genre_string: "No Genre".into(),
            group_discs: true,
            composer_in_artists: false,
            conductor_in_artists: false,
            band_in_artists: false,
            trackartist_in_artists: false,
            use_unified_artists_list: false,
            various_artist_auto_identification: true,
            use_tpe2_as_album_artist: true,
            use_tit1_as_work: true,
            ignore_dir_re: None,
            skip_sentinel: None,
            search_substring: false,
            browse_age_limit: 100,
            browse_popularity_max_days: 180,
            items_per_page: 50,
            ignore_release_types: false,
            disabled_extensions_audio: Vec::new(),
            disabled_extensions_playlist: Vec::new(),
            cover_art: String::new(),
            art_folder: None,
            contributor_pictures: true,
            guess_file_formats: crate::tags::default_guess_formats(),
            works_scan: 2,
            my_classical_genres: "Classical, Klassik, Classique, Klassiek".into(),
            string_disc: "Disc".into(),
            string_of: "of".into(),
            string_kbps: "kbps".into(),
            long_date_format: "%A, %B |%d, %Y".into(),
            time_format: "|%I:%M %p".into(),
        }
    }
}

pub const ROLE_ARTIST: i64 = 1;
pub const ROLE_COMPOSER: i64 = 2;
pub const ROLE_CONDUCTOR: i64 = 3;
pub const ROLE_BAND: i64 = 4;
pub const ROLE_ALBUMARTIST: i64 = 5;
pub const ROLE_TRACKARTIST: i64 = 6;

/// `contributorRoles()` – sorted alphabetically.
pub const CONTRIBUTOR_ROLES: &[(&str, i64)] = &[
    ("ALBUMARTIST", ROLE_ALBUMARTIST),
    ("ARTIST", ROLE_ARTIST),
    ("BAND", ROLE_BAND),
    ("COMPOSER", ROLE_COMPOSER),
    ("CONDUCTOR", ROLE_CONDUCTOR),
    ("TRACKARTIST", ROLE_TRACKARTIST),
];

pub fn type_to_role(t: &str) -> Option<i64> {
    let up = t.trim().to_ascii_uppercase();
    if let Some((_, id)) = CONTRIBUTOR_ROLES.iter().find(|(n, _)| *n == up) {
        return Some(*id);
    }
    t.trim().parse::<i64>().ok()
}

pub fn role_to_type(id: i64) -> Option<&'static str> {
    CONTRIBUTOR_ROLES
        .iter()
        .find(|(_, i)| *i == id)
        .map(|(n, _)| *n)
}

impl LibraryConfig {
    fn in_artists(&self, role: &str) -> bool {
        match role {
            "COMPOSER" => self.composer_in_artists,
            "CONDUCTOR" => self.conductor_in_artists,
            "BAND" => self.band_in_artists,
            "TRACKARTIST" => self.trackartist_in_artists,
            _ => false,
        }
    }

    fn album_link(&self, role: &str) -> bool {
        match role {
            "ARTIST" | "ALBUMARTIST" => true,
            r => self.use_unified_artists_list && self.in_artists(r),
        }
    }

    /// `activeContributorRoles($includeTrackArtist)`
    pub fn active_contributor_roles(&self, include_track_artist: bool) -> Vec<&'static str> {
        let mut roles = vec!["ARTIST", "ALBUMARTIST"];
        if include_track_artist && !self.trackartist_in_artists {
            roles.push("TRACKARTIST");
        }
        for (r, _) in CONTRIBUTOR_ROLES {
            if self.in_artists(r) && !roles.contains(r) {
                roles.push(r);
            }
        }
        roles
    }

    /// `allAlbumLinkRoles()`
    pub fn all_album_link_roles(&self) -> Vec<&'static str> {
        CONTRIBUTOR_ROLES
            .iter()
            .filter(|(r, _)| self.album_link(r))
            .map(|(r, _)| *r)
            .collect()
    }

    pub fn is_classical_genre(&self, genres: &[String]) -> bool {
        let mine: Vec<String> = self
            .my_classical_genres
            .split(',')
            .map(|s| s.trim().to_uppercase())
            .filter(|s| !s.is_empty())
            .collect();
        genres
            .iter()
            .any(|g| mine.contains(&g.trim().to_uppercase()))
    }
}
