//! The client-side hub-title override rule (issue #12): PMS localizes `Hub.title` server-side by
//! `X-Plex-Language`, but its per-string translation coverage for a tag like `be` is partial
//! (verified live 2026-09-28, `docs/pms-api.md` §3), so a Home screen mixed Belarusian titles
//! with untranslated ones. The fix is an unconditional client-side override for every
//! **standard** hub — one whose `hubIdentifier` is a stable, documented PMS identifier this
//! catalog knows a message for. A hub this table does not recognize (a custom collection shelf,
//! a promoted rail PMS mints with a different id, anything future) keeps `pms_title` verbatim,
//! exactly as before: there is no substitute catalog for server-owned text this app cannot
//! enumerate.
//!
//! Two call sites share this ONE table so neither can drift from the other's id list:
//! `crate::pms::project` (Home's whole-catalog merge, one row per source) and
//! `crate::browse::section_hubs::parse_hubs` (one library's own `/hubs/sections/{id}` shelves).
//! They differ only in **scope** — see [`Scope`] — which is why this is a parameter rather than
//! two copies of the same match.

use crate::i18n::msg;

/// Which endpoint the hub came from. The id catalog below is the same either way; only the
/// "Recently Added" family's wording differs, because PMS itself titles that family differently
/// per endpoint (`docs/pms-api.md` §3 vs §3a):
///
/// - [`Scope::Home`] — `/hubs` (or `/hubs/promoted`), which mixes every library on the source. A
///   `movie.recentlyadded.<id>` / `show.recentlyadded.<id>` / `tv.recentlyadded.<id>` hub, or a
///   whole-server `home.<type>.recent` hub PMS mints for exactly one library of that type, needs
///   the library folded into the string ("Recently Added in {library}") or a household with two
///   TV libraries reads as one repeated "Recently Added" shelf.
/// - [`Scope::Section`] — `/hubs/sections/{id}`, already scoped to one library. PMS itself titles
///   the same `movie.recentlyadded.<id>` / `tv.recentlyadded.<id>` family plain "Recently Added"
///   here (measured live PMS 1.43.3 — the fixtures in `section_hubs.rs`'s `HUBS_JSON` and
///   `collection_metadata_is_dropped_but_collection_shelves_of_movies_survive`), so naming the
///   library again would repeat a heading the section page already carries.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Scope {
    Home,
    Section,
}

/// The "Recently Added" hubIdentifier family: the whole-server `home.*.recent` hubs AND the
/// per-library `movie.recentlyadded.<id>` / `show.recentlyadded.<id>` / `tv.recentlyadded.<id>`
/// ones (`docs/plex-openapi.json`: `movie.recentlyadded.1` → "Recently Added in Movies" on Home,
/// plain "Recently Added" on a section's own `/hubs/sections/{id}` — see [`Scope`]). They are the
/// same shape, not two: `home_keeps_recently_added_rows_for_two_same_type_libraries`
/// (`pms_multi_source_merge_tests.rs`) measured PMS minting a SEPARATE `home.television.recent`
/// hub per TV library, with the library's name folded into `title` ("Recently Added in TV" vs
/// "…in TV HDR") — a whole-server hub only when a household happens to own exactly one library
/// of that type.
pub(crate) fn is_recently_added_hub(hub_identifier: &str) -> bool {
    const WHOLE_SERVER: [&str; 5] = [
        "home.movies.recent",
        "home.television.recent",
        "home.music.recent",
        "home.videos.recent",
        "home.photos.recent",
    ];
    const PER_LIBRARY_PREFIXES: [&str; 3] =
        ["movie.recentlyadded.", "show.recentlyadded.", "tv.recentlyadded."];
    WHOLE_SERVER.contains(&hub_identifier)
        || PER_LIBRARY_PREFIXES.iter().any(|p| hub_identifier.starts_with(p))
}

/// A `home.<type>.recent` whole-server hub's own per-type "Recently Added {Type}" string, for the
/// case PMS mints it because a household owns exactly one library of that type (`home_hubs`'s
/// `home.movies.recent`/`home.television.recent`/`home.music.recent`/`home.videos.recent`/
/// `home.photos.recent`) — natural English ("Recently Added Movies") rather than the per-library
/// form ("Recently Added in Movies") that reads oddly when there is nothing to disambiguate.
/// `None` for anything else, including the per-library `*.recentlyadded.<id>` family, which never
/// gets this form even on Home (see [`localized_hub_title`]'s Home arm).
fn recently_added_whole_server_type(hub_identifier: &str) -> Option<fn() -> &'static str> {
    match hub_identifier {
        "home.movies.recent" => Some(msg::browse_home_hub_recently_added_movies),
        "home.television.recent" => Some(msg::browse_home_hub_recently_added_tv),
        "home.music.recent" => Some(msg::browse_home_hub_recently_added_music),
        "home.videos.recent" => Some(msg::browse_home_hub_recently_added_videos),
        "home.photos.recent" => Some(msg::browse_home_hub_recently_added_photos),
        _ => None,
    }
}

/// The one hub-title override, for both [`Scope`]s.
///
/// `library` is the hub's own `librarySectionTitle` (present on every item PMS returns, per
/// `docs/pms-api.md` §2) — Home's per-library form needs it to interpolate ("Recently Added in
/// {library}"), so an empty `library` there (an older/partial fixture, or a payload PMS sent
/// without it) falls back to `pms_title` rather than drawing "Recently Added in" with nothing
/// after it. Section scope's string carries no `{library}`, so it never consults this argument.
///
/// `identifier_is_unique` is Home-only: whether `hub_identifier` names exactly ONE hub in the
/// same `/hubs` response (see [`crate::pms::project`], which counts before it loops). It decides
/// a `home.<type>.recent` hub's wording — the per-type form ("Recently Added Movies") when a
/// household owns exactly one library of that type and PMS answers with one such hub, or the
/// per-library form ("Recently Added in {library}") the moment PMS mints more than one because it
/// is disambiguating between same-type libraries
/// (`home_keeps_recently_added_rows_for_two_same_type_libraries`,
/// `pms_multi_source_merge_tests.rs`). Section scope ignores it: every id it sees there is already
/// scoped to one library by construction.
pub(crate) fn localized_hub_title(
    scope: Scope,
    hub_identifier: &str,
    pms_title: &str,
    library: &str,
    identifier_is_unique: bool,
) -> String {
    if is_recently_added_hub(hub_identifier) {
        match scope {
            Scope::Section => {
                // Inside the library already — PMS itself drops the "in {library}" qualifier
                // here, and so does the override; no library name is needed to say it.
                return msg::browse_library_hub_recently_added().to_string();
            }
            Scope::Home => {
                // A whole-server hub PMS minted because there is exactly one library of that
                // type reads more naturally by type ("Recently Added Movies") than by library
                // name ("Recently Added in Movies") — the per-library form is reserved for when
                // PMS is actually disambiguating between two libraries of the same type, which it
                // signals by minting more than one hub under the SAME identifier.
                if identifier_is_unique {
                    if let Some(per_type) = recently_added_whole_server_type(hub_identifier) {
                        return per_type().to_string();
                    }
                }
                if library.is_empty() {
                    return pms_title.to_string();
                }
                return msg::browse_home_hub_recently_added_in(library);
            }
        }
    }
    match (scope, hub_identifier) {
        (Scope::Home, "home.ondeck" | "home.onDeck") => msg::browse_home_hub_on_deck().to_string(),
        (Scope::Home, "home.playlists") => msg::browse_home_hub_recent_playlists().to_string(),
        _ => pms_title.to_string(),
    }
}
