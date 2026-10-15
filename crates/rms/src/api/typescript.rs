//! `web/api-types.d.ts`: the native API's types for the web UI, generated from `model`.
//! A test, so a stale file fails CI; `UPDATE_TYPES=1 cargo test -p rms typescript` rewrites it.

use super::model::*;
use crate::bridge::{FromRenderer, Pcm, ToRenderer};
use ts_rs::{Config, TS};

fn declarations() -> String {
    let cfg = Config::new().with_large_int("number");
    let mut out = String::from(
        "// Generated from crates/rms/src/api/model.rs by `UPDATE_TYPES=1 cargo test -p rms \\\n\
         // typescript`.  Do not edit.  The API is described in docs/api.md.\n",
    );
    macro_rules! decl {
        ($($t:ty),* $(,)?) => {$(
            out.push_str("\nexport ");
            out.push_str(&<$t>::decl(&cfg));
            out.push('\n');
        )*};
    }
    decl!(
        ServerInfo,
        ScanProgress,
        LibraryTotals,
        Player,
        PlayMode,
        Shuffle,
        Repeat,
        QueueInfo,
        Spectrum,
        SpectrumKind,
        TableRequest,
        TableSort,
        TablePage,
        TableRow,
        TableField,
        TableView,
        Track,
        TrackDetail,
        TrackKind,
        Label,
        TrackAnalysis,
        AnalysisStatus,
        AnalysisStage<()>,
        AnalysisFile,
        Feature,
        Neighbour,
        BlissAnalysis,
        RhythmAnalysis,
        ChromaprintAnalysis,
        Duplicate,
        EffnetAnalysis,
        MusicBrainzAnalysis,
        AnalysisProgress,
        AnalysisCount,
        Queue,
        Item,
        EnqueueMode,
        Command,
        CommandResult,
        Event,
        ApiError,
        Page<()>,
        Artist,
        Album,
        AlbumDetail,
        AlbumRelease,
        ReleaseStatus,
        ReleaseInfo,
        ReleaseTrackMatch,
        ReleaseCandidate,
        ReleaseChoice,
        AlbumTags,
        TrackTags,
        TagValue,
        TagChanges,
        TrackTagChange,
        TagEditResult,
        TagEditFailure,
        TagEdit,
        AlbumFiles,
        FileMove,
        MoveRequest,
        MoveResult,
        CoverArt,
        CoverImage,
        CoverChoice,
        Genre,
        Year,
        Playlist,
        FolderEntryType,
        FolderEntry,
        SearchResults,
        SimilarRequest,
        BrowseItemType,
        BrowseItem,
        NewFavorite,
        SpotifyStatus,
        SpotifyLogin,
        SpotifyLoginResult,
        SpotifyPatch,
        ServerSettings,
        ServerSettingsPatch,
        ScanMode,
        ScanRequest,
        Choice,
        ChoiceOption,
        Range,
        PlayerSettings,
        DisplaySettings,
        PlayerSettingsPatch,
        ToRenderer,
        FromRenderer,
        Pcm,
    );
    out
}

#[test]
fn web_types_are_up_to_date() {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("web/api-types.d.ts");
    let want = declarations();
    if std::env::var_os("UPDATE_TYPES").is_some() {
        std::fs::write(&path, &want).unwrap();
        return;
    }
    let have = std::fs::read_to_string(&path).unwrap_or_default();
    assert!(
        have == want,
        "{} is out of date: run UPDATE_TYPES=1 cargo test -p rms typescript",
        path.display()
    );
}
