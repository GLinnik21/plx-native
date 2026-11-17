//! The accessibility-id coverage TABLE (a11y-ids plan, D2 and workstream A): which screens answer
//! `Focusable::a11y`, `a11y_elems` and `a11y_find` and run `plx_ui::a11y::conformance`.
//!
//! [`coverage`] is an exhaustive `match` over every `AppArg` arm, and over the arms of `ContentArg`
//! and `SettingsPage` it wraps, with NO wildcard: a new screen does not compile until it is tabled
//! here. Every arm starts `Pending`, because even the cards screens carry hand-written groups
//! (Home's hero, Detail's hero, the Library header) that only the grammar and the three hooks
//! answer. A screen leaves `Pending` when its conformance mount lands, so the table only shrinks;
//! precedent: `cards_conformance_tests.rs`'s `table()`.
use crate::registry::{AppArg, ContentArg, SettingsPage};

/// Where one screen stands.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum Coverage {
    /// Not yet run through `plx_ui::a11y::conformance`; the reason says what is missing.
    Pending(&'static str),
}

/// The one reason every arm starts with: no screen answers the hooks on this commit, and no shared
/// widget does either (their lanes land later). A screen's reason is made specific only when it has
/// something more to say than that.
const NOT_YET: &str = "answers none of a11y, a11y_elems or a11y_find yet; no conformance mount";

/// The table: every screen the application can mount, and where it stands.
pub(super) fn coverage(arg: &AppArg) -> Coverage {
    use Coverage::Pending;
    match arg {
        AppArg::LibraryMenu(_) => Pending(NOT_YET),
        AppArg::AccountMenu => Pending(NOT_YET),
        AppArg::ItemMenu(_) => Pending(NOT_YET),
        AppArg::WatchlistMenu(_) => Pending(NOT_YET),
        AppArg::PlayerOverlay(_) => Pending(NOT_YET),
        AppArg::AltSources(_) => Pending(NOT_YET),
        AppArg::VersionsPanel(_) => Pending(NOT_YET),
        AppArg::TracksPanel(_) => Pending(NOT_YET),
        AppArg::AboutPanel => Pending(NOT_YET),
        AppArg::PersonBio => Pending(NOT_YET),
        AppArg::CollectionAbout => Pending(NOT_YET),
        AppArg::Login => Pending(NOT_YET),
        AppArg::Profiles => Pending(NOT_YET),
        AppArg::Onboard => Pending(NOT_YET),
        AppArg::Home => Pending(NOT_YET),
        AppArg::Library => Pending(NOT_YET),
        AppArg::LibraryCollections { .. } => Pending(NOT_YET),
        AppArg::LibraryRecommended { .. } => Pending(NOT_YET),
        AppArg::Search => Pending(NOT_YET),
        AppArg::Watchlist => Pending(NOT_YET),
        AppArg::Player => Pending(NOT_YET),
        AppArg::Content(content) => match content {
            ContentArg::Detail { .. } => Pending(NOT_YET),
            ContentArg::Person { .. } => Pending(NOT_YET),
            ContentArg::Filmography { .. } => Pending(NOT_YET),
            ContentArg::Collection(_) => Pending(NOT_YET),
            ContentArg::Catalog { .. } => Pending(NOT_YET),
        },
        AppArg::Settings(page) => match page {
            SettingsPage::Root => Pending(NOT_YET),
            SettingsPage::Language => Pending(NOT_YET),
            SettingsPage::UnderPosters => Pending(NOT_YET),
            SettingsPage::UnderPostersDetails(_) => Pending(NOT_YET),
            SettingsPage::Contribute => Pending(NOT_YET),
            SettingsPage::Playback => Pending(NOT_YET),
            SettingsPage::AudioSubtitles => Pending(NOT_YET),
            SettingsPage::Favourites => Pending(NOT_YET),
            SettingsPage::LibraryFavourites => Pending(NOT_YET),
            SettingsPage::Privacy => Pending(NOT_YET),
            SettingsPage::Legal => Pending(NOT_YET),
            SettingsPage::About => Pending(NOT_YET),
            SettingsPage::Document(_) => Pending(NOT_YET),
            SettingsPage::Preview(_) => Pending(NOT_YET),
            SettingsPage::ConsentStage(_) => Pending(NOT_YET),
            SettingsPage::Picker(_) => Pending(NOT_YET),
            SettingsPage::Diagnostics => Pending(NOT_YET),
            SettingsPage::PlayerHud => Pending(NOT_YET),
            SettingsPage::HudGallery => Pending(NOT_YET),
            SettingsPage::HudDetail(_) => Pending(NOT_YET),
            SettingsPage::HudAbout(_) => Pending(NOT_YET),
            SettingsPage::HudPreview(_) => Pending(NOT_YET),
        },
        AppArg::FirstRunConsent(_) => Pending(NOT_YET),
    }
}

/// **Every page the application can mount is in the table with a reason.** The census is
/// `registry::every_page_arg`; the exhaustive `match` above is what makes a new `AppArg` arm fail
/// to compile until it is tabled, and this keeps the reasons from being empty.
#[test]
fn every_page_argument_is_tabled_with_a_reason() {
    let pages = crate::registry::every_page_arg();
    assert!(!pages.is_empty());
    for arg in &pages {
        let Coverage::Pending(why) = coverage(arg);
        assert!(!why.is_empty(), "{}: a pending screen names why", arg.route_word());
    }
}
