//! The `timeline` frames [`super::classify`] is tested against, in one place.
//!
//! **CAPTURED SHAPE, SCRUBBED.** These are built from passive captures of a real server's feed
//! (`docs/pms-api.md` §9): `event: timeline`, `data: {"TimelineEntry":{...}}`, ONE object per frame, no
//! `NotificationContainer`; `sectionID` and `itemID` strings, `type` and `state` integers. Titles, ids
//! and section ids are synthetic ("Movie A"), `updatedAt` is rebased to 1760000000, and the order of
//! the frames inside each sequence is the order they arrived in. Each constant is the `data:` text of
//! the frames of one item's sequence, in arrival order.
//!
//! What the captures show: a movie or show runs state 5 (`metadataState` "processing"), 4 ("loading"),
//! then 5 with NO `metadataState` (the finished frame); a season runs 2 ("matching"), 5, 3 ("queued"),
//! 3 ("processing"), 4 ("loading"), 5 and its episodes interleave; a music track (type 10) is two
//! state-5 frames, the first with `mediaState` "analyzing". No delete was observed: [`MOVIE_DELETED`]
//! is the community-documented state 9 and is NOT a recording.

/// A movie add, as captured: processing, loading, then the finished frame.
pub const MOVIE_ADD: [&str; 3] = [
    r#"{"TimelineEntry":{"identifier":"com.plexapp.plugins.library","sectionID":"1","itemID":"1001","type":1,"title":"Movie A","state":5,"metadataState":"processing","updatedAt":1760000001}}"#,
    r#"{"TimelineEntry":{"identifier":"com.plexapp.plugins.library","sectionID":"1","itemID":"1001","type":1,"title":"Movie A","state":4,"metadataState":"loading","updatedAt":1760000002}}"#,
    r#"{"TimelineEntry":{"identifier":"com.plexapp.plugins.library","sectionID":"1","itemID":"1001","type":1,"title":"Movie A","state":5,"updatedAt":1760000002}}"#,
];

/// A show add: the same three frames as a movie with type 2.
pub const SHOW_ADD: [&str; 3] = [
    r#"{"TimelineEntry":{"identifier":"com.plexapp.plugins.library","sectionID":"2","itemID":"2001","type":2,"title":"Show A","state":5,"metadataState":"processing","updatedAt":1760000101}}"#,
    r#"{"TimelineEntry":{"identifier":"com.plexapp.plugins.library","sectionID":"2","itemID":"2001","type":2,"title":"Show A","state":4,"metadataState":"loading","updatedAt":1760000102}}"#,
    r#"{"TimelineEntry":{"identifier":"com.plexapp.plugins.library","sectionID":"2","itemID":"2001","type":2,"title":"Show A","state":5,"updatedAt":1760000102}}"#,
];

/// A season with two episodes, in arrival order: season matching, season 5, season queued and
/// processing and loading, the episodes (each 5, 4, 4, 5), the season's finished frame last. Nothing
/// here is a movie or a show.
pub const SEASON_AND_EPISODES: [&str; 20] = [
    r#"{"TimelineEntry":{"identifier":"com.plexapp.plugins.library","sectionID":"3","itemID":"3001","type":3,"title":"Season 1","state":2,"metadataState":"matching","updatedAt":1760000201}}"#,
    r#"{"TimelineEntry":{"identifier":"com.plexapp.plugins.library","sectionID":"3","itemID":"3001","type":3,"title":"Season 1","state":5,"updatedAt":1760000201}}"#,
    r#"{"TimelineEntry":{"identifier":"com.plexapp.plugins.library","sectionID":"3","itemID":"3001","type":3,"title":"Season 1","state":3,"metadataState":"queued","updatedAt":1760000201}}"#,
    r#"{"TimelineEntry":{"identifier":"com.plexapp.plugins.library","sectionID":"3","itemID":"3001","type":3,"title":"Season 1","state":3,"metadataState":"processing","updatedAt":1760000201}}"#,
    r#"{"TimelineEntry":{"identifier":"com.plexapp.plugins.library","sectionID":"3","itemID":"3001","type":3,"title":"Season 1","state":4,"metadataState":"loading","updatedAt":1760000202}}"#,
    r#"{"TimelineEntry":{"identifier":"com.plexapp.plugins.library","sectionID":"3","itemID":"3002","type":4,"title":"Episode A","state":5,"metadataState":"processing","updatedAt":1760000202}}"#,
    r#"{"TimelineEntry":{"identifier":"com.plexapp.plugins.library","sectionID":"3","itemID":"3002","type":4,"title":"Episode A","state":4,"metadataState":"loading","updatedAt":1760000202}}"#,
    r#"{"TimelineEntry":{"identifier":"com.plexapp.plugins.library","sectionID":"3","itemID":"3002","type":4,"title":"Episode A","state":4,"metadataState":"loading","updatedAt":1760000202}}"#,
    r#"{"TimelineEntry":{"identifier":"com.plexapp.plugins.library","sectionID":"3","itemID":"3002","type":4,"title":"Episode A","state":5,"updatedAt":1760000202}}"#,
    r#"{"TimelineEntry":{"identifier":"com.plexapp.plugins.library","sectionID":"3","itemID":"3003","type":4,"title":"Episode B","state":5,"metadataState":"processing","updatedAt":1760000202}}"#,
    r#"{"TimelineEntry":{"identifier":"com.plexapp.plugins.library","sectionID":"3","itemID":"3003","type":4,"title":"Episode B","state":4,"metadataState":"loading","updatedAt":1760000202}}"#,
    r#"{"TimelineEntry":{"identifier":"com.plexapp.plugins.library","sectionID":"3","itemID":"3003","type":4,"title":"Episode B","state":4,"metadataState":"loading","updatedAt":1760000202}}"#,
    r#"{"TimelineEntry":{"identifier":"com.plexapp.plugins.library","sectionID":"3","itemID":"3003","type":4,"title":"Episode B","state":5,"updatedAt":1760000202}}"#,
    r#"{"TimelineEntry":{"identifier":"com.plexapp.plugins.library","sectionID":"3","itemID":"3004","type":4,"title":"Episode C","state":5,"metadataState":"processing","updatedAt":1760000203}}"#,
    r#"{"TimelineEntry":{"identifier":"com.plexapp.plugins.library","sectionID":"3","itemID":"3004","type":4,"title":"Episode C","state":4,"metadataState":"loading","updatedAt":1760000203}}"#,
    r#"{"TimelineEntry":{"identifier":"com.plexapp.plugins.library","sectionID":"3","itemID":"3004","type":4,"title":"Episode C","state":4,"metadataState":"loading","updatedAt":1760000203}}"#,
    r#"{"TimelineEntry":{"identifier":"com.plexapp.plugins.library","sectionID":"3","itemID":"3004","type":4,"title":"Episode C","state":5,"updatedAt":1760000203}}"#,
    r#"{"TimelineEntry":{"identifier":"com.plexapp.plugins.library","sectionID":"3","itemID":"3001","type":3,"title":"Season 1","state":5,"updatedAt":1760000203}}"#,
    r#"{"TimelineEntry":{"identifier":"com.plexapp.plugins.library","sectionID":"3","itemID":"3005","type":4,"title":"Episode D","state":4,"metadataState":"loading","updatedAt":1760000203}}"#,
    r#"{"TimelineEntry":{"identifier":"com.plexapp.plugins.library","sectionID":"3","itemID":"3005","type":4,"title":"Episode D","state":5,"updatedAt":1760000203}}"#,
];

/// A music track (type 10), as captured: an `analyzing` frame, then the bare state-5 frame. The
/// identifier is the library plugin's; the TYPE is what makes it not ours.
pub const MUSIC_TRACK: [&str; 2] = [
    r#"{"TimelineEntry":{"identifier":"com.plexapp.plugins.library","sectionID":"4","itemID":"4001","type":10,"title":"Track A","state":5,"mediaState":"analyzing","updatedAt":1760000301}}"#,
    r#"{"TimelineEntry":{"identifier":"com.plexapp.plugins.library","sectionID":"4","itemID":"4001","type":10,"title":"Track A","state":5,"updatedAt":1760000301}}"#,
];

/// A delete as the community docs show it (state 9). NOT a recording: no delete has been observed.
pub const MOVIE_DELETED: &str = r#"{"TimelineEntry":{"identifier":"com.plexapp.plugins.library","sectionID":"1","itemID":"1002","type":1,"title":"Movie B","state":9,"updatedAt":1760000401}}"#;

/// The envelope the community docs show (`NotificationContainer` around a `TimelineEntry` ARRAY, ids
/// and numbers as strings), which the parser still reads although the captured shape differs.
pub const COMMUNITY_ENVELOPE: &str = r#"{"NotificationContainer":{"type":"timeline","size":2,"TimelineEntry":[
    {"identifier":"com.plexapp.plugins.library","sectionID":"1","itemID":"1003","type":"1","state":"5"},
    {"identifier":"com.plexapp.plugins.library","sectionID":"2","itemID":"2003","type":2,"state":5}]}}"#;

/// Another plugin's finished movie: not the library's.
pub const OTHER_PLUGIN: &str = r#"{"TimelineEntry":{"identifier":"tv.plex.provider.other","sectionID":"1","itemID":"1004","type":1,"title":"Movie C","state":5,"updatedAt":1760000501}}"#;

/// Entries missing a field or carrying one of the wrong shape: each is dropped, none panics.
pub const MALFORMED: [&str; 9] = [
    r#"{"TimelineEntry":{"type":1,"state":5}}"#,
    r#"{"TimelineEntry":{"itemID":"30","state":5}}"#,
    r#"{"TimelineEntry":{"itemID":"31","type":1}}"#,
    r#"{"TimelineEntry":{"itemID":"","type":1,"state":5}}"#,
    r#"{"TimelineEntry":{"itemID":{"a":1},"type":1,"state":5}}"#,
    r#"{"TimelineEntry":{"itemID":"32","type":"movie","state":5}}"#,
    r#"{"TimelineEntry":{"itemID":"33","type":1,"state":null}}"#,
    r#"{"TimelineEntry":"not an object"}"#,
    r#"{"TimelineEntry":[]}"#,
];
