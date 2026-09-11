//! **The heartbeat's `route=` and ` overlay=` alphabet** — the words `tests/run.py` selects fps
//! samples by (`LOOP_RE`/`FPS_RE` against `tests/manifest.json`'s `route`/`overlay` fields), and
//! the same words the focus fingerprint, the diag `RouteEntered` event and the lab envelope print.
//!
//! It is its own module since D1's item 8, out of `app/mod.rs` — which is `plex_run`'s ten-line
//! skeleton and the `App` struct, and had grown four test modules that graded four other files'
//! subjects (`ci/check-deps.sh`'s `testmod` gate). Everything the alphabet is made of came with
//! the tests that grade it, because the point of [`heartbeat_word_tests`] is that the two tables
//! are DERIVED rather than transcribed: the routes from [`route_word`] over [`every_route`], the
//! overlays from the real `Mounter` through `bridge::every_surface_word`.
//!
//! A word the app cannot print makes a scene fail on the television as "only 0 post-warmup samples
//! — scene never entered this screen", which is indistinguishable from a real regression. That is
//! the single failure every rule in here exists to prevent.

use super::bridge;
use crate::screens::registry::AppArg;

/// The heartbeat's `route=` WORD for a route — the string `tests/run.py` selects samples by
/// (`LOOP_RE`/`FPS_RE`) against `manifest.json`'s `route` field, and the one the focus
/// fingerprint, the diag `RouteEntered` event and the lab envelope all print. ONE function so the
/// four cannot disagree, and so `heartbeat_word_tests` can grade the table against the manifest:
/// a route renamed here without its scenes following would otherwise fail on the device as
/// "never entered this screen", which reads exactly like a total regression.
pub(crate) fn route_word(route: &AppArg) -> &'static str {
    use crate::screens::registry::ContentArg;
    match route {
        AppArg::Login => "login",
        AppArg::Profiles => "profiles",
        AppArg::Onboard => "onboard",
        AppArg::Library => "library",
        AppArg::Content(ContentArg::Detail { .. }) => "detail",
        // The filmography sheet answers `word::PERSON` too (`FilmographyScreen::name`): the test
        // manifest intentionally records that opaque modal as `route=person`.
        AppArg::Content(ContentArg::Person { .. } | ContentArg::Filmography { .. }) => "person",
        AppArg::Search => "search",
        AppArg::Player => "player",
        AppArg::Home => "home",
        // Not pages: a surface's word is ` overlay=`, and none of these reaches the page stack's
        // top. The arm is written out rather than swept into a `_` so a new PAGE variant is a
        // compile error here — the failure this table exists to prevent is a word the app cannot
        // print, which reads on the television as "0 post-warmup samples".
        AppArg::Settings(_)
        | AppArg::FirstRunConsent(_)
        | AppArg::LibraryMenu(_)
        | AppArg::AccountMenu
        | AppArg::ItemMenu(_)
        | AppArg::PlayerOverlay(_)
        | AppArg::AltSources(_)
        | AppArg::TracksPanel(_)
        | AppArg::AboutPanel
        | AppArg::PersonBio => "",
    }
}

/// **Every PAGE argument, one per heartbeat word** — the domain [`route_word`] is applied over to DERIVE the
/// heartbeat's `route=` alphabet, and the list `app::bridge`'s own argument tests walk.
///
/// The compiler is what keeps it complete: the array's length is written out, so an added variant
/// with no entry here fails the exhaustiveness `match` in `heartbeat_word_tests` rather than
/// quietly missing from the table the fps tier selects on. That is the failure this exists for —
/// a word the app cannot print makes a scene fail on the television as "only 0 post-warmup
/// samples", which is indistinguishable from a real regression.
///
/// `#[cfg(test)]`, because nothing in the SHIPPED app ever wants every route at once: the loop
/// holds one and asks about it. Its two readers are this module's word derivation and
/// `app::bridge`'s argument tests, which used to keep a second copy of the same list.
#[cfg(test)]
pub(crate) fn every_route() -> [AppArg; 9] {
    use crate::screens::registry::ContentArg;
    let sid = crate::plex::ServerId::UNSET;
    [
        AppArg::Login,
        AppArg::Profiles,
        AppArg::Onboard,
        AppArg::Home,
        AppArg::Library,
        AppArg::Content(ContentArg::Detail { sid, rk: String::new() }),
        AppArg::Content(ContentArg::Person {
            sid, key: String::new(), guid: String::new(), name: String::new(), thumb: String::new(),
        }),
        AppArg::Search,
        AppArg::Player,
    ]
}

/// The heartbeat's ` overlay=` WORD, or `None` when nothing is over the page.
///
/// **It is the topmost surface's own `Screen::name`, asked FIRST**, and since phase 10's item 4
/// that is the whole of it — `bridge::overlay_word` is a one-line read of the container with no
/// mapping table under it, so the alphabet the fps tier selects on IS the set of names the mounted
/// screens answer. See that function for the two failures the eleven-arm `match` it replaced had
/// already produced.
///
/// [`NO_OVERLAY`] is the one word here that no screen owns, and the reason this function exists at
/// all beside the bridge's: the player with nothing over it prints ` overlay=none`, which is a
/// statement about the ROUTE that the container cannot make. (Five arms stood here, one per
/// `Route::Player { overlay }` value, until phase 9 made the panels surfaces.)
pub(crate) fn overlay_word(pages: &crate::ui::dispatch::Dispatcher<bridge::AppHost>, route: &AppArg) -> Option<&'static str> {
    bridge::overlay_word(pages).or(matches!(route, AppArg::Player).then_some(NO_OVERLAY))
}

/// The bare player, whose ` overlay=` word belongs to no screen — see [`overlay_word`].
pub(crate) const NO_OVERLAY: &str = "none";

/// The heartbeat's ` overlay=<word>` suffix, prefix and all, empty when there is none. The prefix
/// is built HERE rather than baked into every word because the words are the SCREENS' own and a
/// screen has no business knowing what the heartbeat's grammar looks like.
pub(crate) fn overlay_suffix(pages: &crate::ui::dispatch::Dispatcher<bridge::AppHost>, route: &AppArg) -> String {
    overlay_word(pages, route).map_or(String::new(), |w| format!(" overlay={w}"))
}

/// **Every `route=` word [`route_word`] can print, and every ` overlay=` word [`overlay_word`]
/// can — DERIVED, not transcribed** (restructure phase 10, item 4).
///
/// Both were hand-written arrays, and both had already rotted in the one direction nothing fails
/// on: a word in the table that no function prints keeps a manifest scene looking armed while it
/// selects on a string the app never emits, which on the television reads as "only 0 post-warmup
/// samples — scene never entered this screen", i.e. exactly like a total regression. Phase 10
/// moved TWO words between the tables (`account`, `itemmenu`), which is the transition where a
/// transcription is least likely to survive.
///
/// So the routes come from [`route_word`] applied over [`every_route`], and the overlays from the
/// MOUNTER: `bridge::every_surface_word()` mounts one instance of every surface `AppArg` variant
/// through the real `Mounter` and reads its `Screen::name`, so the alphabet is what the screens
/// say it is. Neither list can carry a word its source does not produce, and neither can miss one
/// — the exhaustiveness of `every_route` and of the mounter's own `match` is the compiler's.
#[cfg(test)]
fn route_words() -> Vec<&'static str> {
    every_route().iter().map(route_word).collect()
}

#[cfg(test)]
fn overlay_words() -> Vec<&'static str> {
    let mut words = bridge::every_surface_word();
    words.push(NO_OVERLAY);
    words
}

/// The heartbeat word table versus `tests/manifest.json`. Every fps scene selects its samples by
/// a `route` word and an optional `overlay` word; a word the app cannot print makes that scene
/// fail on the television as "only 0 post-warmup samples — scene never entered this screen",
/// which is indistinguishable from a real regression. This is the host-side half of that gate,
/// and it is what lets the route-name source move (from this `match` to `Screen::name` later)
/// without the fps tier silently disarming.
#[cfg(test)]
mod heartbeat_word_tests {
    use super::{every_route, overlay_word, overlay_words, route_words, AppArg, NO_OVERLAY};

    const MANIFEST: &str = include_str!("../../../tests/manifest.json");

    fn scenes() -> Vec<serde_json::Value> {
        let v: serde_json::Value = serde_json::from_str(MANIFEST).expect("manifest.json parses");
        v["fps_scenes"]
            .as_array()
            .expect("fps_scenes is an array")
            .clone()
    }

    /// The Settings family's INNER pages, which are not `AppArg` variants and so cannot come off
    /// the mounter: `RouteSurface::top_word` answers with whichever page of the family's own stack
    /// is on top, and the family is presented rooted at `Root` (or, for the dev boot targets, at
    /// one of these). They are the registry's own constants rather than string literals, so a
    /// screen renamed there renames the alphabet entry with it.
    ///
    /// **`ONBOARD` is one screen wearing two hats** (§6.2 "Onboard ×2"): the SAME `Screen` impl is
    /// mounted once as a page of the app's outer stack (first run, so `route=onboard`) and once as
    /// a page of the family's INNER stack (`SettingsPage::Favourites`, so ` overlay=onboard`).
    /// Before it had its own word, the settings-mounted instance answered `word::SETTINGS` and the
    /// `fps:settings-home` scene printed a heartbeat BYTE-IDENTICAL to `settings-root` — the
    /// harness could not tell "opened the Home-sources editor" from "opened Settings and did
    /// nothing", so a trigger that silently failed to reach Favourites still produced a scene that
    /// passed, measuring the wrong screen.
    const FAMILY_INNER: [&str; 3] = [
        crate::screens::registry::word::PRIVACY,
        crate::screens::registry::word::LEGAL,
        crate::screens::registry::word::ONBOARD,
    ];

    /// **Every caller holds `crate::testlock::serial()` for its whole body**, because deriving
    /// this alphabet is not a read: [`overlay_words`] goes through
    /// `bridge::every_surface_word`, which mounts each surface by running real
    /// `bridge::frame`s — and a frame pumps every store. `browse`'s pump ends in `sync_roster`,
    /// which calls `browse::reset()` whenever the section table holds a source the live registry
    /// does not; every `browse` fixture in the suite (`seed_two_source_table_for_test` and its
    /// kin) seeds exactly such a table, with `ServerId::UNSET` sources. Unguarded, these three
    /// tests therefore EMPTIED another module's seeded table from a second thread, and the
    /// failure surfaced over there — `app::chrome`'s
    /// `four_libraries_on_two_servers_publish_two_type_destinations` losing both library
    /// destinations, `app::bridge`'s shelf-hold case seeing zero shelves — at a rate low enough
    /// to read as flakiness. The frame trunk (`bridge::frame_with_results`) asserts the lock now
    /// rather than merely documenting it.
    fn overlay_alphabet() -> Vec<&'static str> {
        let mut words = overlay_words();
        words.extend(FAMILY_INNER);
        words
    }

    #[test]
    fn every_manifest_route_word_is_one_the_heartbeat_prints() {
        let _guard = crate::testlock::serial();
        let routes = route_words();
        let overlays = overlay_alphabet();
        for s in scenes() {
            let name = s["name"].as_str().unwrap_or("?");
            let route = s["route"].as_str().expect("scene has a route word");
            assert!(
                routes.contains(&route),
                "scene {name}: route word {route:?} is not in the heartbeat table {routes:?}"
            );
            if let Some(ov) = s.get("overlay").and_then(|o| o.as_str()) {
                assert!(
                    overlays.contains(&ov),
                    "scene {name}: overlay word {ov:?} is not in the table {overlays:?}"
                );
            }
        }
    }

    /// **The two tables are DERIVED from the two sources, and this is what says the derivation is
    /// the whole of them** (restructure phase 10 item 4).
    ///
    /// It used to compare two hand-written arrays against two hand-written lists of routes and
    /// screen kinds — four transcriptions of two alphabets, each able to rot in the direction
    /// nothing fails on. What is left to assert is what a derivation cannot state about itself:
    /// that the two alphabets are DISJOINT bar the one word that is deliberately in both, that
    /// every word is a plausible heartbeat token, and that `overlay_word`'s one non-screen answer
    /// is the player's.
    #[test]
    fn the_tables_are_derived_and_the_two_alphabets_stay_apart() {
        let _guard = crate::testlock::serial();
        let routes = route_words();
        let overlays = overlay_alphabet();
        assert_eq!(
            routes.len(),
            every_route().len(),
            "one word per route, derived: {routes:?}"
        );
        // Nothing empty, nothing with a space in it: every one of these is a `\w+` token the
        // harness's `LOOP_RE`/`FPS_RE` capture groups have to match, and a word carrying the
        // heartbeat's own ` overlay=` prefix (which is how the mapping table this replaced spelled
        // them) would match nothing at all.
        for w in routes.iter().chain(overlays.iter()) {
            assert!(
                !w.is_empty() && w.chars().all(|c| c.is_ascii_alphanumeric() || c == '_'),
                "{w:?} is not a heartbeat token"
            );
        }
        // ONE word is in both alphabets, and it names one screen mounted on two stacks. Any other
        // overlap is a route and a surface that would be indistinguishable in a `route=` field.
        let both: Vec<&str> = overlays.iter().copied().filter(|w| routes.contains(w)).collect();
        assert_eq!(both, [crate::screens::registry::word::ONBOARD]);
        // …and the two menus that became surfaces in phase 10 are on the overlay side ONLY. Both
        // MOVED between the tables in the commits that deleted their routes, and each moved with
        // its `manifest.json` scene's re-key (`home-acct-glass`, `item-menu`); a word left behind
        // in the route alphabet would have let a scene keep selecting on a `route=` the app can no
        // longer print, which fails on the television as "never entered this screen".
        for w in [
            crate::screens::registry::word::ACCOUNT,
            crate::screens::registry::word::ITEM_MENU,
        ] {
            assert!(overlays.contains(&w), "{w:?} is a surface's own name");
            assert!(!routes.contains(&w), "{w:?} has no `route_word` arm any more");
        }

        // An EMPTY tree, so `overlay_word`'s second half is what answers: with no surface up the
        // container says nothing and the ROUTE decides, which is exactly the state every BARE
        // playback frame is in. It is the one word no screen owns, which is why it is added by
        // `overlay_words` rather than derived.
        let empty = crate::ui::dispatch::Dispatcher::<super::bridge::AppHost>::new();
        assert_eq!(overlay_word(&empty, &AppArg::Player), Some(NO_OVERLAY));
        assert_eq!(overlay_word(&empty, &AppArg::Home), None);
        assert_eq!(super::overlay_suffix(&empty, &AppArg::Player), " overlay=none");
        assert_eq!(super::overlay_suffix(&empty, &AppArg::Home), "");
        assert!(overlays.contains(&NO_OVERLAY));
        assert!(!routes.contains(&NO_OVERLAY));

        // The player's four panels are `OverlayKind::word` through `Screen::name`, so they arrive
        // in the derived alphabet with everything else — asserted here because it is the one place
        // a reader can see that the panel words and the family words come from ONE source now.
        use crate::screens::player::overlay::OverlayKind;
        for kind in [
            OverlayKind::Tracks { tab: 0 },
            OverlayKind::Info,
            OverlayKind::Chapters,
            OverlayKind::More { quality: false },
        ] {
            assert!(
                overlays.contains(&kind.word()),
                "{kind:?} prints {:?}, which the mounter's own alphabet must carry",
                kind.word()
            );
        }
    }

    /// **A word the app can print but no scene uses is fine; the reverse is not** — and the
    /// derivation is what makes the reverse impossible to write by accident.
    ///
    /// The one thing a derived table cannot catch on its own is a manifest scene keyed on a word
    /// that IS in the alphabet but names a surface the scene's triggers never open. That is a
    /// device question, not a host one. What this pins instead is the direction a host CAN see:
    /// every scene naming an overlay names one the mounter produces, and every scene's route is a
    /// route that exists.
    #[test]
    fn the_manifest_uses_a_subset_of_the_derived_alphabets() {
        let _guard = crate::testlock::serial();
        let routes = route_words();
        let overlays = overlay_alphabet();
        let mut used_overlays = 0;
        for s in scenes() {
            assert!(routes.contains(&s["route"].as_str().expect("a route word")));
            if let Some(ov) = s.get("overlay").and_then(|o| o.as_str()) {
                assert!(overlays.contains(&ov));
                used_overlays += 1;
            }
        }
        assert!(
            used_overlays >= 2,
            "the two menus' scenes select by `overlay=` since phase 10 — if this reaches zero, \
             the re-keys were reverted and every surface scene is measuring its host page"
        );
    }

    /// **The pollution these three tests used to cause, stated as a fact instead of a comment.**
    ///
    /// Deriving the alphabet is a DESTRUCTIVE operation on `browse`: `every_surface_word` mounts
    /// each surface by running real `bridge::frame`s, a frame pumps every store, and `browse`'s
    /// pump reaches `sync_roster`, which treats a source the live registry does not hold as an
    /// identity boundary and calls `browse::reset()`. Every `browse` fixture in the suite seeds
    /// exactly such a table (`ServerId::UNSET` sources), so this wipes it.
    ///
    /// That is correct behaviour for `sync_roster` and it is why the derivation may only run
    /// under `testlock::serial()`. Watched red before the fix in the only way this class can be:
    /// the wipe landed on ANOTHER thread's test — `app::chrome`'s
    /// `four_libraries_on_two_servers_publish_two_type_destinations` came back with only Home and
    /// Search in the strip, about one full-suite run in six. Here the same chain is on one
    /// thread, under the guard, and is therefore deterministic.
    #[test]
    fn deriving_the_surface_alphabet_empties_a_seeded_browse_table() {
        let _guard = crate::testlock::serial();
        crate::stores::browse::apply(crate::stores::browse::BrowseCmd::Reset);
        crate::browse::seed_two_source_table_for_test();
        assert_eq!(
            crate::browse::section_count(),
            4,
            "the fixture the browse-backed tests across the suite seed"
        );
        let _ = overlay_alphabet();
        assert_eq!(
            crate::browse::section_count(),
            0,
            "deriving the alphabet runs real frames and empties the section table — so it must \
             never run on a thread that does not hold testlock::serial()"
        );
        crate::stores::browse::apply(crate::stores::browse::BrowseCmd::Reset);
    }

    /// **Pins the focusprobe's player-overlay word to THIS module's `overlay_word`, not a second
    /// hand-written copy.** `app/run.rs`'s `probe_screen` closure (the focus fingerprint's
    /// `Route::Player` arm) used to carry its OWN `match overlay { Overlay::None => "none", … }`
    /// table, with a comment claiming it printed "the same words the heartbeat's `overlay=`
    /// uses" — a claim nothing checked, and exactly the shape that goes stale silently: a word
    /// edited on one side (a rename, a typo, a new `Overlay` variant) would make the focus
    /// fingerprint and the heartbeat disagree about the SAME frame's overlay, and nothing here
    /// would fail. `probe_screen` is a closure local to `run()`, not a free function this test can
    /// call, so — the same idiom `search_owned_tests.rs`'s chrome-guard pin uses for the same
    /// reason — this reads `run.rs`'s own source and asserts the arm DELEGATES to `overlay_word`
    /// rather than re-deriving the mapping inline.
    ///
    /// Observed RED before the unification: the arm read
    /// `overlay: match overlay { Overlay::None => "none", Overlay::Menu => "menu", … }`, which
    /// contains no `overlay_word(` call at all — this test failed as designed. A second manual
    /// check confirmed the pin actually discriminates rather than merely checking for a
    /// substring: with the delegating call in place, temporarily reintroducing a stray
    /// `Overlay::None =>` arm beside it (simulating a partial revert to a hand-rolled table) also
    /// turned this test red; reverted after observing it.
    #[test]
    fn focusprobe_player_overlay_delegates_to_the_shared_overlay_word_function() {
        let src = std::fs::read_to_string(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/app/run.rs"),
        )
        .expect("read run.rs");
        let start = src
            .find("crate::focusprobe::Screen::Player {")
            .expect("probe_screen must build a focusprobe::Screen::Player");
        let end = src[start..]
            .find("},")
            .map(|i| start + i)
            .expect("the Player arm must close with `},`");
        let arm = &src[start..end];
        assert!(
            arm.contains("overlay_word("),
            "the focusprobe's AppArg::Player arm must call the shared overlay_word(...) \
             function (the same one the heartbeat uses) rather than re-deriving the mapping; \
             found:\n{arm}"
        );
        assert!(
            !arm.contains("Overlay::None =>") && !arm.contains("Overlay::Menu =>"),
            "a hand-written Overlay match here means a SECOND overlay-word table exists \
             alongside overlay_word; found:\n{arm}"
        );
    }
}

