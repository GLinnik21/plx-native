//! The `timeline` frames [`super::classify`] is tested against, in one place.
//!
//! **COMMUNITY-DOCUMENTED, NOT YET CAPTURED LIVE.** The shape is the one python-plexapi's
//! `AlertListener` consumers document (`NotificationContainer.type == "timeline"`, a
//! `TimelineEntry` array with `identifier`, `sectionID`, `itemID`, `type`, `state`,
//! `metadataState`, `mediaState`, `updatedAt`; `state` 5 = processed, 9 = deleted). No frame from a
//! real server has been recorded (`docs/pms-api.md` §9). When the owner's passive capture exists,
//! its scrubbed frames REPLACE the constants below; the tests in `super::tests` read only these
//! names, so swapping them is a one-file change. Titles and ids here are synthetic.

/// A movie the scanner finished (state 5) in section 1, id 123.
pub const MOVIE_CREATED: &str = r#"{"NotificationContainer":{"type":"timeline","size":1,"TimelineEntry":[
    {"identifier":"com.plexapp.plugins.library","sectionID":"1","itemID":"123","type":1,"title":"Synthetic Movie","state":5,"metadataState":"created","mediaState":"analyzing","updatedAt":1760000000}]}}"#;

/// A show removed from the library (state 9), ids as strings.
pub const SHOW_DELETED: &str = r#"{"NotificationContainer":{"type":"timeline","size":1,"TimelineEntry":[
    {"identifier":"com.plexapp.plugins.library","sectionID":"2","itemID":"456","type":"2","title":"Synthetic Show","state":"9","metadataState":"deleted","updatedAt":1760000001}]}}"#;

/// One scan: progress states (0 created, 1 scanning, 3 analysed), an episode and an artist (types
/// 4 and 8), another plugin's entry, then the one finished movie and one finished delete.
pub const SCAN_MIXED: &str = r#"{"NotificationContainer":{"type":"timeline","size":7,"TimelineEntry":[
    {"identifier":"com.plexapp.plugins.library","sectionID":"1","itemID":"10","type":1,"state":0,"metadataState":"created"},
    {"identifier":"com.plexapp.plugins.library","sectionID":"1","itemID":"10","type":1,"state":3},
    {"identifier":"com.plexapp.plugins.library","sectionID":"1","itemID":"11","type":4,"state":5},
    {"identifier":"com.plexapp.plugins.library","sectionID":"3","itemID":"12","type":8,"state":5},
    {"identifier":"tv.plex.provider.other","sectionID":"1","itemID":"13","type":1,"state":5},
    {"identifier":"com.plexapp.plugins.library","sectionID":"1","itemID":"14","type":1,"state":5,"unknown":{"a":[1]}},
    {"sectionID":"1","itemID":14,"type":1,"state":9}]}}"#;

/// Only scan progress and a non-movie, non-show type: nothing a Watchlist tile can follow.
pub const UNFINISHED: &str = r#"{"NotificationContainer":{"type":"timeline","size":2,"TimelineEntry":[
    {"identifier":"com.plexapp.plugins.library","sectionID":"1","itemID":"20","type":1,"state":2},
    {"identifier":"com.plexapp.plugins.library","sectionID":"1","itemID":"21","type":4,"state":5}]}}"#;

/// Entries missing a field or carrying one of the wrong shape: each is dropped, none panics.
pub const MALFORMED: &str = r#"{"NotificationContainer":{"type":"timeline","TimelineEntry":[
    {"type":1,"state":5},
    {"itemID":"30","state":5},
    {"itemID":"31","type":1},
    {"itemID":"","type":1,"state":5},
    {"itemID":{"a":1},"type":1,"state":5},
    {"itemID":"32","type":"movie","state":5},
    {"itemID":"33","type":1,"state":null},
    "not an object", 7]}}"#;
