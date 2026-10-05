//! saved_view — what a library's remembered VIEW says after the viewer changes something (#441).
//!
//! A pure function and its two inputs, split out of `browse` so the merge rules — which are the
//! whole of what "remember everything the toolbar changed, per library" means — are read and
//! tested without a section table, a worker or a session file.
//!
//! The record ([`plx_plex::plex::session::LibraryView`]) is rebuilt from the section's LIVE state
//! after every landed edit, with one complication that gives this module its reason to exist: the
//! first page of a library after a restart is requested before its menu is known, so for a moment
//! the live state does not know the sort or the genre the record holds (the sort index needs the
//! menu, and a saved genre is only believed once the server's genre list has confirmed it). An
//! Unwatched toggle in that window must not read the unknown as "default" and erase them, so the
//! stored record is consulted for whatever the live state cannot yet say. The same holds for good
//! when the restore itself failed transiently — the menu landed but the sorted re-ask or the genre
//! list did not — and the library shows the default: that is an unresolved restore, not a choice.

use super::LibraryType;
use plx_plex::plex::session::LibraryView;

/// What the viewer just changed. Decides which parts of the live state are authoritative when the
/// saved view has not been resolved (the menu is not yet known, or a restore failed).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum ViewField {
    Sort,
    Unwatched,
    Genre,
    Listing,
}

/// The section's live state, as far as the record cares.
#[derive(Clone, Debug)]
pub(super) struct ViewSnapshot {
    /// The listing shown.
    pub(super) listing: LibraryType,
    /// Is `sort` the viewer's choice rather than the absence of one? False until the listing's
    /// menu has landed AND the saved sort was settled — applied, or its key proved gone by the
    /// menu (or there was none to restore). A restore that failed leaves it false for good, and
    /// the saved sort is kept as stored.
    pub(super) sort_resolved: bool,
    /// The same for `genre`: settled when the saved genre was applied, or the library's genre list
    /// was read and proved it gone. A genre check that failed leaves it false.
    pub(super) genre_resolved: bool,
    /// The sort in force as `(key, descending)`, `None` for the listing's own default order.
    pub(super) sort: Option<(String, bool)>,
    pub(super) unwatched: bool,
    /// The genre filter's tag id.
    pub(super) genre: Option<String>,
}

/// The record `edit` leaves for library (`machine`, `key`), given what is `stored` now and the
/// live state `snap`.
///
/// * The listing and Unwatched are always the live state's — they are applied synchronously on a
///   restart, never waiting for a menu.
/// * The library's OWN listing's sort belongs to that listing: it is read from the live state
///   only while that listing is shown (and its sort resolved, or the edit was the sort itself) and
///   otherwise kept as stored, so it survives the pre-menu window, a failed restore and a visit to
///   Collections.
/// * The other listing's sort is read from the live state when it is resolved, dropped when
///   the listing was only just chosen (its menu starts empty and it has no remembered order yet),
///   and kept as stored only while the same listing is still waiting on its menu or restore.
/// * A genre belongs to the own listing alone: it is dropped while another listing is shown,
///   mirroring `set_library_type` clearing it. An unresolved genre is kept as stored unless the
///   edit was the genre itself.
pub(super) fn merge(
    stored: Option<&LibraryView>,
    machine: &str,
    key: i64,
    snap: &ViewSnapshot,
    edit: ViewField,
) -> LibraryView {
    let blank = LibraryView::default();
    let stored = stored.unwrap_or(&blank);
    let main = snap.listing == LibraryType::Primary;
    let live_sort = snap.sort_resolved || edit == ViewField::Sort;
    let (sort, desc) = if main && live_sort {
        pair(&snap.sort)
    } else {
        (stored.sort.clone(), stored.desc)
    };
    let (listing_sort, listing_desc) = if main {
        Default::default()
    } else if live_sort {
        pair(&snap.sort)
    } else if edit == ViewField::Listing || stored.listing != snap.listing.wire() {
        Default::default()
    } else {
        (stored.listing_sort.clone(), stored.listing_desc)
    };
    let genre = if !main {
        String::new()
    } else if snap.genre_resolved || edit == ViewField::Genre {
        snap.genre.clone().unwrap_or_default()
    } else {
        stored.genre.clone()
    };
    LibraryView {
        machine_id: machine.to_string(),
        key,
        sort,
        desc,
        unwatched: snap.unwatched,
        genre,
        listing: snap.listing.wire().to_string(),
        listing_sort,
        listing_desc,
        extensions: stored.extensions.clone(),
    }
}

fn pair(sort: &Option<(String, bool)>) -> (String, bool) {
    sort.clone().unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stored() -> LibraryView {
        LibraryView {
            machine_id: "m".into(), key: 1, sort: "viewCount".into(), desc: true,
            genre: "28".into(), ..Default::default()
        }
    }

    /// The live state of a library just reopened from its record: Unwatched applied, the menu
    /// (and so the sort and the genre) not landed yet.
    fn before_the_menu(unwatched: bool) -> ViewSnapshot {
        ViewSnapshot {
            listing: LibraryType::Primary, sort_resolved: false, genre_resolved: false, sort: None,
            unwatched, genre: None,
        }
    }

    #[test]
    fn an_unwatched_toggle_before_the_menu_lands_keeps_the_stored_sort_and_genre() {
        let view = merge(Some(&stored()), "m", 1, &before_the_menu(true), ViewField::Unwatched);
        assert!(view.unwatched);
        assert_eq!(view.primary_sort(), Some(("viewCount", true)));
        assert_eq!(view.genre, "28");
    }

    #[test]
    fn a_genre_edit_with_the_menu_unknown_still_wins() {
        let snap = ViewSnapshot { genre: Some("12".into()), ..before_the_menu(false) };
        let view = merge(Some(&stored()), "m", 1, &snap, ViewField::Genre);
        assert_eq!(view.genre, "12");
        assert_eq!(view.primary_sort(), Some(("viewCount", true)), "the sort is not the edit");
        let cleared = merge(Some(&stored()), "m", 1, &before_the_menu(false), ViewField::Genre);
        assert!(cleared.genre.is_empty(), "choosing no genre clears it");
    }

    #[test]
    fn a_type_change_keeps_the_own_sort_and_clears_the_genre_and_the_listing_sort() {
        let mut before = stored();
        before.listing = "episodes".into();
        before.listing_sort = "addedAt".into();
        let snap = ViewSnapshot {
            listing: LibraryType::Collections, sort_resolved: false, genre_resolved: false,
            sort: None, unwatched: false, genre: None,
        };
        let view = merge(Some(&before), "m", 1, &snap, ViewField::Listing);
        assert_eq!(view.listing, "collections");
        assert_eq!(view.primary_sort(), Some(("viewCount", true)), "the own sort survives Collections");
        assert!(view.genre.is_empty(), "a genre never survives a type change");
        assert_eq!(view.listing_sort(), None, "the new listing has no remembered order yet");
    }

    #[test]
    fn the_listings_sort_is_kept_while_its_menu_is_still_unknown() {
        let mut before = stored();
        before.listing = "episodes".into();
        before.listing_sort = "addedAt".into();
        before.listing_desc = true;
        let snap = ViewSnapshot {
            listing: LibraryType::Episodes, sort_resolved: false, genre_resolved: false,
            sort: None, unwatched: true, genre: None,
        };
        let view = merge(Some(&before), "m", 1, &snap, ViewField::Unwatched);
        assert_eq!(view.listing_sort(), Some(("addedAt", true)));
        assert_eq!(view.primary_sort(), Some(("viewCount", true)));
    }

    #[test]
    fn a_sort_edit_records_the_listing_that_was_sorted() {
        let snap = ViewSnapshot {
            listing: LibraryType::Collections, sort_resolved: true, genre_resolved: true,
            sort: Some(("titleSort".into(), true)), unwatched: false, genre: None,
        };
        let view = merge(None, "m", 1, &snap, ViewField::Sort);
        assert_eq!(view.listing_sort(), Some(("titleSort", true)));
        assert_eq!(view.primary_sort(), None);
    }

    #[test]
    fn a_default_main_view_is_the_forgotten_view() {
        let snap = ViewSnapshot {
            listing: LibraryType::Primary, sort_resolved: true, genre_resolved: true, sort: None,
            unwatched: false, genre: None,
        };
        assert!(merge(Some(&stored()), "m", 1, &snap, ViewField::Sort).is_default());
        assert!(merge(None, "m", 1, &snap, ViewField::Unwatched).is_default());
    }

    /// The menu landed but the saved sort and genre did not resolve (the sorted re-ask or the
    /// genre list failed): the library shows the defaults, the record still holds the saved view.
    fn after_a_failed_restore(unwatched: bool) -> ViewSnapshot {
        ViewSnapshot { sort_resolved: false, genre_resolved: false, ..before_the_menu(unwatched) }
    }

    #[test]
    fn an_unresolved_sort_is_kept_on_an_unwatched_edit() {
        let snap = ViewSnapshot { genre_resolved: true, ..after_a_failed_restore(true) };
        let view = merge(Some(&stored()), "m", 1, &snap, ViewField::Unwatched);
        assert!(view.unwatched);
        assert_eq!(view.primary_sort(), Some(("viewCount", true)));
        assert!(view.genre.is_empty(), "the resolved genre (none applied) is the live one");
    }

    #[test]
    fn an_unresolved_genre_is_kept_on_a_sort_edit() {
        let snap = ViewSnapshot {
            sort_resolved: true, sort: Some(("addedAt".into(), false)),
            ..after_a_failed_restore(false)
        };
        let view = merge(Some(&stored()), "m", 1, &snap, ViewField::Sort);
        assert_eq!(view.primary_sort(), Some(("addedAt", false)), "the sort edit wins");
        assert_eq!(view.genre, "28", "the genre that could not be checked is not cleared");
    }

    #[test]
    fn an_explicit_edit_wins_over_an_unresolved_restore() {
        let sorted = ViewSnapshot { sort: Some(("addedAt".into(), true)), ..after_a_failed_restore(false) };
        let view = merge(Some(&stored()), "m", 1, &sorted, ViewField::Sort);
        assert_eq!(view.primary_sort(), Some(("addedAt", true)));
        let default_sort = after_a_failed_restore(false);
        assert_eq!(merge(Some(&stored()), "m", 1, &default_sort, ViewField::Sort).primary_sort(), None,
            "choosing the default order back is a choice");
        let genre = ViewSnapshot { genre: Some("12".into()), ..after_a_failed_restore(false) };
        assert_eq!(merge(Some(&stored()), "m", 1, &genre, ViewField::Genre).genre, "12");
        assert!(merge(Some(&stored()), "m", 1, &default_sort, ViewField::Genre).genre.is_empty(),
            "choosing no genre is a choice");
    }

    #[test]
    fn a_genre_the_list_proved_gone_is_dropped_on_the_next_write() {
        let snap = ViewSnapshot { genre_resolved: true, ..after_a_failed_restore(true) };
        let view = merge(Some(&stored()), "m", 1, &snap, ViewField::Unwatched);
        assert!(view.genre.is_empty());
    }
}
