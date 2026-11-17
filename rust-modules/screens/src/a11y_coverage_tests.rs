//! The accessibility-id coverage TABLE (a11y-ids plan, D2 and workstream A): which screens answer
//! `Focusable::a11y`, `a11y_elems` and `a11y_find` and run `plx_ui::a11y::conformance`.
//!
//! [`coverage`] is an exhaustive `match` over every `AppArg` arm, and over the arms of `ContentArg`
//! and `SettingsPage` it wraps, with NO wildcard: a new screen does not compile until it is tabled
//! here. An arm is [`Coverage::Covered`] by a function that MOUNTS that screen and runs
//! `plx_ui::a11y::conformance` over it; the function is the screen's own test body, exposed from its
//! test module (`<screen>_conforms`) and called by that module's test too, so the table cannot say
//! "covered" about a mount the screen's tests no longer run. An arm that cannot be mounted on the
//! host is [`Coverage::Pending`] with the true reason, and the table's tests print it. The table only
//! shrinks; precedent: `cards_conformance_tests.rs`'s `table()`.
//!
//! A covered function returns the namespace of every mounting it exercised (`Screen::a11y_ns`), and
//! [`conform`] is the one way a test module produces one: it runs conformance and reads the
//! namespace together, so a mount cannot be run through one without the other.
use crate::family::{PickerKind, PosterKind};
use crate::registry::{AppArg, ContentArg, SettingsPage};
use plx_machine::machine::{Cx, Host};
use plx_ui::screen::Screen;

/// **Run `plx_ui::a11y::conformance` over one mounted screen and answer its namespace.** The one
/// call every `<screen>_conforms` makes per mounting.
pub(crate) fn conform<H: Host, S: Screen<H> + ?Sized>(view: &S, cx: &Cx<'_, H>) -> &'static str {
    plx_ui::a11y::conformance::<H, _>(view, cx);
    let ns = Screen::<H>::a11y_ns(view);
    assert!(plx_ui::a11y::is_ns(ns), "{ns:?} is not a namespace");
    ns
}

/// What a covered arm's function answers: the namespace of each mounting it ran.
pub(crate) type Namespaces = Vec<&'static str>;

/// Where one screen stands.
#[derive(Debug)]
pub(super) enum Coverage {
    /// Mounted and run through `plx_ui::a11y::conformance` by this function; the name is the arm's.
    Covered(&'static str, fn() -> Namespaces),
    /// Cannot be run through it yet: the arm's name, then the reason, which says what is missing and
    /// is TRUE of this commit.
    Pending(&'static str, &'static str),
}

/// A Settings-family page through the surface (`RouteSurface`), the way `AppArg::Settings` mounts it.
fn surface(page: SettingsPage) -> Namespaces {
    crate::settings::root_navigation_tests::settings_page_conforms(page)
}

/// The table: every screen the application can mount, and where it stands. A [`Coverage::Covered`]
/// arm names its mount function; a new arm is a compile error until it is tabled here.
pub(super) fn coverage(arg: &AppArg) -> Coverage {
    use Coverage::{Covered, Pending};
    match arg {
        AppArg::LibraryMenu(_) => Pending("LibraryMenu", LIBRARYMENU_PENDING),
        AppArg::AccountMenu => Covered("AccountMenu", crate::account_menu::tests::account_menu_conforms),
        AppArg::ItemMenu(_) => Covered("ItemMenu", crate::item_menu::tests::item_menu_conforms),
        AppArg::WatchlistMenu(_) => Covered("WatchlistMenu", crate::watchlist::page_tests::watchlist_menu_conforms),
        AppArg::PlayerOverlay(_) => Covered("PlayerOverlay", crate::player::overlay_tests::player_overlay_conforms),
        AppArg::AltSources(_) => Covered("AltSources", crate::alt_sources::tests::focus_and_hit::alt_sources_conforms),
        AppArg::VersionsPanel(_) => Covered("VersionsPanel", crate::versions_panel::tests::versions_panel_conforms),
        AppArg::TracksPanel(_) => Covered("TracksPanel", crate::tracks_panel::tests::tracks_panel_conforms),
        AppArg::AboutPanel => Covered("AboutPanel", crate::about_panel::tests::about_panel_conforms),
        AppArg::PersonBio => Covered("PersonBio", crate::person_bio::tests::person_bio_conforms),
        AppArg::CollectionAbout => Covered("CollectionAbout", crate::about_panel::tests::collection_about_conforms),
        AppArg::Login => Covered("Login", crate::login::tests::login_conforms),
        AppArg::Profiles => Covered("Profiles", crate::profiles::selection_tests::profiles_conforms),
        AppArg::Onboard => Covered("Onboard", crate::onboard::tests::onboard_first_run_conforms),
        AppArg::Home => Covered("Home", crate::home::cards_harness::home_conforms),
        AppArg::Library => Covered("Library", crate::library::cards_harness::library_conforms),
        AppArg::LibraryCollections { .. } => Covered("LibraryCollections", crate::library::tests::collections_listing_tests::library_collections_conforms),
        AppArg::LibraryRecommended { .. } => Covered("LibraryRecommended", crate::library::cards_harness::recommended_conforms),
        AppArg::Search => Covered("Search", crate::search::cards_harness::search_conforms),
        AppArg::Watchlist => Covered("Watchlist", crate::watchlist::page_tests::watchlist_conforms),
        AppArg::Player => Covered("Player", crate::player::step_ladder_tests::player_conforms),
        AppArg::Content(content) => match content {
            ContentArg::Detail { .. } => Covered("Content::Detail", crate::detail::cards_harness::detail_conforms),
            ContentArg::Person { .. } => Covered("Content::Person", crate::person::cards_harness::person_conforms),
            ContentArg::Filmography { .. } => Covered("Content::Filmography", crate::filmography::tests::filmography_conforms),
            ContentArg::Collection(_) => Covered("Content::Collection", crate::collection::cards_harness::collection_conforms),
            ContentArg::Catalog { .. } => Covered("Content::Catalog", crate::catalog::tests::catalog_conforms),
        },
        AppArg::Settings(page) => match page {
            SettingsPage::Root => Covered("Settings::Root", || surface(SettingsPage::Root)),
            SettingsPage::Language => Covered("Settings::Language", || surface(SettingsPage::Language)),
            SettingsPage::UnderPosters => Covered("Settings::UnderPosters", || surface(SettingsPage::UnderPosters)),
            SettingsPage::UnderPostersDetails(_) => Covered("Settings::UnderPostersDetails", || {
                let mut namespaces = surface(SettingsPage::UnderPostersDetails(PosterKind::Movie));
                namespaces.extend(surface(SettingsPage::UnderPostersDetails(PosterKind::Show)));
                namespaces
            }),
            SettingsPage::Contribute => Covered("Settings::Contribute", || surface(SettingsPage::Contribute)),
            SettingsPage::Playback => Covered("Settings::Playback", || surface(SettingsPage::Playback)),
            SettingsPage::AudioSubtitles => Covered("Settings::AudioSubtitles", || surface(SettingsPage::AudioSubtitles)),
            SettingsPage::Favourites => Covered("Settings::Favourites", || surface(SettingsPage::Favourites)),
            SettingsPage::LibraryFavourites => Covered("Settings::LibraryFavourites", || surface(SettingsPage::LibraryFavourites)),
            SettingsPage::Privacy => Covered("Settings::Privacy", || surface(SettingsPage::Privacy)),
            SettingsPage::Legal => Covered("Settings::Legal", || surface(SettingsPage::Legal)),
            SettingsPage::About => Covered("Settings::About", || surface(SettingsPage::About)),
            SettingsPage::Document(_) => Covered("Settings::Document", || surface(SettingsPage::Document(0))),
            SettingsPage::Preview(_) => Covered("Settings::Preview", || surface(SettingsPage::Preview(0))),
            SettingsPage::ConsentStage(_) => Covered("Settings::ConsentStage", || surface(SettingsPage::ConsentStage(0))),
            SettingsPage::Picker(_) => Covered("Settings::Picker", || surface(SettingsPage::Picker(PickerKind::Quality))),
            SettingsPage::Diagnostics => Covered("Settings::Diagnostics", || surface(SettingsPage::Diagnostics)),
            SettingsPage::PlayerHud => Covered("Settings::PlayerHud", || surface(SettingsPage::PlayerHud)),
            SettingsPage::HudGallery => Covered("Settings::HudGallery", || surface(SettingsPage::HudGallery)),
            SettingsPage::HudDetail(_) => Covered("Settings::HudDetail", crate::hud_settings::tests::hud_detail_conforms),
            SettingsPage::HudAbout(_) => Covered("Settings::HudAbout", crate::hud_settings::tests::hud_about_conforms),
            SettingsPage::HudPreview(_) => Covered("Settings::HudPreview", crate::hud_settings::tests::hud_preview_conforms),
        },
        AppArg::FirstRunConsent(_) => Covered("FirstRunConsent", crate::settings::root_navigation_tests::first_run_consent_conforms),
    }
}

/// `LibraryMenu` (`library/menu.rs`) implements `Focusable::groups` (one `Column` group over its form's
/// rows) and none of `a11y`, `a11y_elems` or `a11y_find`, so `plx_ui::a11y::conformance` fails it on
/// "a11y_elems lists no element although GroupSpec.len > 0"; making it answer them is product code (its
/// `FormId` impl names no row, as `account_menu.rs`'s does), which this table's lane does not change.
const LIBRARYMENU_PENDING: &str = "answers none of a11y, a11y_elems or a11y_find (library/menu.rs's \
     Focusable impl declares its row group and no ids); conformance needs product code: row names on its \
     FormId and the three hooks";

/// How many leaf arms the table has: 21 flat `AppArg` arms, 5 `ContentArg`, 22 `SettingsPage` and
/// `FirstRunConsent`. A new arm fails [`coverage`] to compile, and then this to pass until the census
/// below holds it too.
const ARMS: usize = 49;

/// **A value of every leaf arm** the table names: the registry's own page and surface censuses, plus
/// the arms those leave out (Filmography and every Settings page).
fn every_arm() -> Vec<AppArg> {
    let sha = plx_data::hudpacks::Sha::parse(&"0".repeat(64)).expect("a sha256");
    let mut arms = crate::registry::every_page_arg();
    arms.extend(crate::registry::every_surface_arg());
    arms.push(AppArg::Content(ContentArg::Filmography {
        sid: plx_plex::plex::ServerId::UNSET,
        key: String::new(),
    }));
    arms.extend(
        [
            SettingsPage::Root,
            SettingsPage::Language,
            SettingsPage::UnderPosters,
            SettingsPage::UnderPostersDetails(PosterKind::Movie),
            SettingsPage::Contribute,
            SettingsPage::Playback,
            SettingsPage::AudioSubtitles,
            SettingsPage::Favourites,
            SettingsPage::LibraryFavourites,
            SettingsPage::Privacy,
            SettingsPage::Legal,
            SettingsPage::About,
            SettingsPage::Document(0),
            SettingsPage::Preview(0),
            SettingsPage::ConsentStage(0),
            SettingsPage::Picker(PickerKind::Quality),
            SettingsPage::Diagnostics,
            SettingsPage::PlayerHud,
            SettingsPage::HudGallery,
            SettingsPage::HudDetail(sha),
            SettingsPage::HudAbout(sha),
            SettingsPage::HudPreview(sha),
        ]
        .map(AppArg::Settings),
    );
    arms.push(AppArg::FirstRunConsent(0));
    arms
}

/// The census with each arm's coverage, one per arm name (the registry's censuses list a
/// surface's kinds more than once).
fn tabled() -> Vec<Coverage> {
    let mut seen = std::collections::BTreeSet::new();
    every_arm()
        .iter()
        .map(coverage)
        .filter(|c| {
            let (Coverage::Covered(name, _) | Coverage::Pending(name, _)) = c;
            seen.insert(*name)
        })
        .collect()
}

/// **Every arm is tabled, and the table is real.** Each [`Coverage::Covered`] arm's function runs: it
/// mounts the screen and runs `plx_ui::a11y::conformance` over it, and answers at least one namespace.
/// A [`Coverage::Pending`] arm names a reason. `Pending` arms are printed, so a gap is read, not found.
#[test]
fn every_arm_is_covered_by_a_mount_that_conforms_or_pending_with_a_reason() {
    let table = tabled();
    assert_eq!(table.len(), ARMS, "the census lists every leaf arm of the table, once");
    let (mut covered, mut pending) = (0, 0);
    for entry in &table {
        match entry {
            Coverage::Covered(name, run) => {
                let namespaces = run();
                assert!(!namespaces.is_empty(), "{name}: a covered arm mounts something and answers its namespace");
                covered += 1;
            }
            Coverage::Pending(name, why) => {
                assert!(!why.trim().is_empty(), "{name}: a pending screen names why");
                eprintln!("a11y coverage PENDING {name}: {why}");
                pending += 1;
            }
        }
    }
    eprintln!("a11y coverage: {covered} covered, {pending} pending of {ARMS}");
}

/// Two arms that answer one namespace because they mount ONE screen type: the namespace, the arms, and
/// why they are the same screen. A pair that is not in this list and shares a namespace is two screens
/// whose ids would meet.
const ONE_SCREEN_TWO_MOUNTINGS: &[(&str, &[&str], &str)] = &[(
    "onboard",
    &["Onboard", "Settings::Favourites", "Settings::LibraryFavourites"],
    "one `OnboardScreen`, mounted by the first run, by the Settings editor and by the Library's editor \
     (`settings::mount_page` and the registry both build it); its ids are the same elements",
), (
    "library",
    &["Library", "LibraryCollections"],
    "one `LibraryScreen`: `AppArg::LibraryCollections` is the same screen in `Listing::Collections` mode, \
     pushed over `AppArg::Library` (`is_library_family`)",
), (
    "consent",
    &["FirstRunConsent", "Settings::ConsentStage"],
    "one `ConsentPage::first_run`: `AppArg::FirstRunConsent` roots a `RouteSurface` in `Family::FirstRunConsent` \
     at `SettingsPage::ConsentStage`, and the Settings family pushes the same page for the same stage",
), (
    "about",
    &["AboutPanel", "CollectionAbout"],
    "one `AboutPanelScreen`: the registry mounts it over the item (`AboutPanelScreen::new`) and over the \
     collection (`AboutPanelScreen::collection`); it declares no group, so it answers no id at all",
)];

/// **No two DIFFERENT screens answer one namespace** (plan D2): the namespace is the `<ns>` of every id
/// the screen answers, so two screens sharing one would give two elements one id. The namespaces of
/// every covered mounting are collected (every `SettingsPage` and `ContentArg` arm included); an arm may
/// share only as [`ONE_SCREEN_TWO_MOUNTINGS`] allows.
#[test]
fn no_two_screens_answer_one_namespace() {
    use std::collections::BTreeMap;
    let mut by_namespace: BTreeMap<&'static str, Vec<&'static str>> = BTreeMap::new();
    for entry in tabled() {
        if let Coverage::Covered(name, run) = entry {
            let mut own = run();
            own.sort_unstable();
            own.dedup();
            for ns in own {
                by_namespace.entry(ns).or_default().push(name);
            }
        }
    }
    assert!(by_namespace.len() > 30, "the covered arms answer their own namespaces: {by_namespace:?}");
    let mut complaints = Vec::new();
    for (ns, arms) in &by_namespace {
        if arms.len() == 1 {
            continue;
        }
        let allowed = ONE_SCREEN_TWO_MOUNTINGS
            .iter()
            .any(|(allowed_ns, allowed_arms, _)| allowed_ns == ns && allowed_arms.len() == arms.len() && arms.iter().all(|a| allowed_arms.contains(a)));
        if !allowed {
            complaints.push(format!("namespace {ns:?} is answered by {arms:?}: two screens would share an id"));
        }
    }
    assert!(complaints.is_empty(), "{}", complaints.join("\n"));
    // an allow-list entry that no longer matches (its arms stopped sharing) is stale and fails too
    for (ns, arms, _) in ONE_SCREEN_TWO_MOUNTINGS {
        assert_eq!(by_namespace.get(ns).map(|a| a.len()), Some(arms.len()), "{ns:?} no longer shares: drop its allow-list entry");
    }
}
