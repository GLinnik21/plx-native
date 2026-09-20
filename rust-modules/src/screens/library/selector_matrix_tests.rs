//! The library selector across connection tier x ownership x library/source count — see each
//! test's doc comment for the cell it pins.
use super::*;
use std::ffi::CString;
use super::tests::Fixture;

fn sid(n: u16) -> crate::plex::ServerId {
    crate::plex::ServerId::from_raw(n)
}

fn group(name: &str, handle: &str, state: crate::browse::SourceState,
    tier: Option<crate::plex::probe::Location>) -> crate::browse::SrcGroup {
    crate::browse::SrcGroup { name: name.into(), handle: handle.into(), state, tier }
}

fn section(source: crate::plex::ServerId, key: i64, index: usize, title: &str, current: bool)
    -> crate::browse::view::SectionView {
    crate::browse::view::SectionView { sid: Some(source), key, kind: SecKind::Movie,
        row: crate::browse::SrcRow { section: index, title: title.into(), pinned: true, current,
            ..Default::default() } }
}

/// **The headline case.** `DirectorySnapshot::capture_from` copies the section table with no
/// tier/state filter, and `LibraryScreen::sync` (mod.rs) filters favourites on `sid.is_some()`
/// alone — neither a source's connection tier nor its probe state reaches the selector at all.
/// So every `Location` (plus "no tier yet", `None`) crossed with every `SourceState` — 4x5 = 20
/// cells — must draw the SAME outcome: no selector at F=1, and at F=2 exactly two pills whose
/// labels never change with the cell. That invariance, not any one cell, is what this pins.
#[test]
fn every_connection_tier_and_source_state_draws_the_same_selector() {
    let _guard = crate::testlock::serial();
    use crate::browse::SourceState;
    use crate::plex::probe::Location;
    let tiers = [None, Some(Location::Local), Some(Location::Remote), Some(Location::Relay)];
    let states = [SourceState::NotProbed, SourceState::Reachable, SourceState::Unauthorized,
        SourceState::Unreachable, SourceState::InsecureOnly];
    let sid_a = sid(0);
    let sid_b = sid(1);
    let expected_a = CString::new("Movies A").unwrap();
    let expected_b = CString::new("Movies B friend").unwrap();

    for tier in tiers {
        for state in states {
            // F=1: a lone favourite must never draw a selector, whatever the tier/state.
            let mut fixture = Fixture::new();
            fixture.directory = crate::browse::view::DirectorySnapshot::fixture_with_sources(1, 0,
                vec![(sid_a, group("Cinema server", "", state, tier))],
                vec![section(sid_a, 1, 0, "Cinema", true)]);
            let page = fixture.screen();
            assert!(page.libraries.is_empty(),
                "tier={tier:?} state={state:?}: a single favourite must clear the selector");
            assert!(!page.layout.libraries,
                "tier={tier:?} state={state:?}: no row height reserved for a single favourite");

            // F=2: two favourites must draw exactly 2 pills, with labels that never move.
            let mut fixture = Fixture::new();
            fixture.directory = crate::browse::view::DirectorySnapshot::fixture_with_sources(1, 0,
                vec![(sid_a, group("Cinema server", "", state, tier)),
                    (sid_b, group("Home cinema", "friend", state, tier))],
                vec![section(sid_a, 1, 0, "Movies A", true), section(sid_b, 1, 1, "Movies B", false)]);
            let page = fixture.screen();
            let cx = fixture.cx(None);
            assert_eq!(page.libraries.len(), 2,
                "tier={tier:?} state={state:?}: two favourites must draw exactly 2 pills");
            assert_eq!(page.library_label(0, &cx), expected_a, "tier={tier:?} state={state:?}");
            assert_eq!(page.library_label(1, &cx), expected_b, "tier={tier:?} state={state:?}");
        }
    }
}

/// Regression guard for issue #100/#165: the 0.6.x server picker's singleton exception ("a
/// borrowed library is still ambiguous") was carried into the restructure by `3c2de7ad` and kept a
/// legacy `Library · <name> ⌄` chip up for any Guest/managed profile, whose own household server
/// always arrives `owned:false` (`plex/account.rs`). `SectionView` carries no ownership bit at
/// all any more — the only trace of "whose server" that reaches this layer is the handle
/// `owner_credit` attaches (`servers.rs:247-290`: empty for your own/household server, the
/// sharer's name otherwise) — so this table drives every shape ownership can take, at F=1 and
/// F=2, and asserts the pill COUNT tracks F alone.
#[test]
fn ownership_does_not_change_how_many_pills_are_drawn() {
    let _guard = crate::testlock::serial();
    use crate::browse::SourceState;
    let rosters: [(&str, &[(&str, &str)]); 5] = [
        ("one owned source", &[("Cinema server", "")]),
        ("one borrowed source", &[("Friend's cinema", "friend")]),
        ("two owned sources", &[("Cinema server", ""), ("Home cinema", "")]),
        ("own + borrowed", &[("Cinema server", ""), ("Friend's cinema", "friend")]),
        // A managed/Guest profile's own household server: plex.tv says `owned:false`, but the
        // household handle is still empty (`is_household`, servers.rs:247-250), so the shape at
        // this layer is indistinguishable from "two owned sources" above — which is the point.
        ("all-owned:false managed/Guest roster", &[("Family room", ""), ("Kids room", "")]),
    ];

    for (label, roster) in rosters {
        let sources: Vec<_> = roster.iter().enumerate()
            .map(|(i, (name, handle))| (sid(i as u16),
                group(name, handle, SourceState::Reachable, None)))
            .collect();

        // F=1: exactly one favourite, on the roster's first source.
        let mut fixture = Fixture::new();
        fixture.directory = crate::browse::view::DirectorySnapshot::fixture_with_sources(1, 0,
            sources.clone(), vec![section(sources[0].0, 1, 0, "Movies", true)]);
        let page = fixture.screen();
        assert!(page.libraries.is_empty(), "{label}: F=1 must clear the selector");

        // F=2: one favourite per source when the roster has two; two libraries on the one source
        // it has otherwise. Either way F becomes 2 and only the pill COUNT is asserted.
        let sections: Vec<_> = if sources.len() >= 2 {
            sources.iter().enumerate()
                .map(|(i, (source, _))| section(*source, 1, i, &format!("Movies {i}"), i == 0))
                .collect()
        } else {
            (0..2).map(|i| section(sources[0].0, i as i64 + 1, i, &format!("Movies {i}"), i == 0)).collect()
        };
        let mut fixture = Fixture::new();
        fixture.directory = crate::browse::view::DirectorySnapshot::fixture_with_sources(1, 0, sources, sections);
        let page = fixture.screen();
        assert_eq!(page.libraries.len(), 2, "{label}: F=2 must draw exactly 2 pills");
    }
}

/// The owner's handle branch in `library_label` (toolbar.rs:27-37): a nonempty `SrcGroup.handle`
/// suffixes the pill, an empty one leaves the title bare. A managed/Guest profile's own household
/// library arrives `owned:false` from plex.tv but still gets an EMPTY handle
/// (`owner_credit`/`is_household`, servers.rs:247-290) — so that case must render exactly like an
/// ordinary owned library, never like a share.
#[test]
fn a_pill_carries_its_owners_handle_and_a_household_library_carries_none() {
    let _guard = crate::testlock::serial();
    use crate::browse::SourceState;
    let friend = sid(0);
    let own = sid(1);
    let household_as_managed_guest = sid(2);
    let mut fixture = Fixture::new();
    fixture.directory = crate::browse::view::DirectorySnapshot::fixture_with_sources(1, 0,
        vec![
            (friend, group("Friend's cinema", "friend", SourceState::Reachable, None)),
            (own, group("Cinema server", "", SourceState::Reachable, None)),
            (household_as_managed_guest, group("Family room", "", SourceState::Reachable, None)),
        ],
        vec![
            section(friend, 1, 0, "Films", true),
            section(own, 1, 1, "Films", false),
            section(household_as_managed_guest, 1, 2, "Films", false),
        ]);
    let page = fixture.screen();
    let cx = fixture.cx(None);
    assert_eq!(page.library_label(0, &cx), CString::new("Films friend").unwrap(),
        "a share's handle suffixes the pill");
    assert_eq!(page.library_label(1, &cx), CString::new("Films").unwrap(),
        "your own server's empty handle leaves the title bare");
    assert_eq!(page.library_label(2, &cx), CString::new("Films").unwrap(),
        "a managed/Guest profile's own household library is `owned:false` yet still bare — \
         there is no handle for it to carry");
}

/// **Pinned as a known wart, not endorsed.** `toolbar.rs:29-32` builds the pill purely from the
/// section title plus the owner handle; it never consults the server NAME (`SrcGroup.name`) to
/// disambiguate two same-titled libraries on two different owned servers. If both are truly
/// "Movies" on two of your own servers, the pills read as identical to the viewer today — do not
/// "fix" this from a test; it is pinned so a future change to it is a deliberate, reviewed one.
#[test]
fn two_owned_servers_with_the_same_library_title_draw_two_identical_pills() {
    let _guard = crate::testlock::serial();
    use crate::browse::SourceState;
    let a = sid(0);
    let b = sid(1);
    let mut fixture = Fixture::new();
    fixture.directory = crate::browse::view::DirectorySnapshot::fixture_with_sources(1, 0,
        vec![(a, group("Cinema server", "", SourceState::Reachable, None)),
            (b, group("Home cinema", "", SourceState::Reachable, None))],
        vec![section(a, 1, 0, "Movies", true), section(b, 1, 1, "Movies", false)]);
    let page = fixture.screen();
    let cx = fixture.cx(None);
    assert_eq!(page.libraries.len(), 2);
    let label_a = page.library_label(0, &cx);
    let label_b = page.library_label(1, &cx);
    assert_eq!(label_a, CString::new("Movies").unwrap());
    assert_eq!(label_a, label_b, "two owned servers with the same library title draw indistinguishable pills");
}

/// A favourite whose source has gone `SourceState::Unreachable` stays counted and stays listed —
/// `sync` (mod.rs) filters favourites on `sid.is_some()` and `pinned` only, never on the source's
/// last dial. The pill itself carries no status cue for it (unlike the Sources overflow menu's
/// header accessory, which leads with the state word: `ui/source_list.rs:150-173`'s `accessory`),
/// so a dead favourite reads on the strip exactly like a live one.
#[test]
fn an_unreachable_favourite_stays_in_the_selector() {
    let _guard = crate::testlock::serial();
    use crate::browse::SourceState;
    let live = sid(0);
    let dead = sid(1);
    let mut fixture = Fixture::new();
    fixture.directory = crate::browse::view::DirectorySnapshot::fixture_with_sources(1, 0,
        vec![(live, group("Cinema server", "", SourceState::Reachable, None)),
            (dead, group("Friend's cinema", "friend", SourceState::Unreachable, None))],
        vec![section(live, 1, 0, "Movies", true), section(dead, 1, 1, "Movies", false)]);
    let page = fixture.screen();
    let cx = fixture.cx(None);
    assert_eq!(page.libraries.len(), 2, "the unreachable favourite still counts toward F");
    assert_eq!(page.library_label(1, &cx), CString::new("Movies friend").unwrap(),
        "the pill carries the owner handle only — no status word leaks into it");
}
