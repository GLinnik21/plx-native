//! The *Also available* panel's own suite: the pure row model, the addressed store it reads, and
//! the two rules the surface conversion moved (the rebuild under a correction, and the CUT that a
//! page teardown is as against the FADE a BACK is).

use super::*;
use crate::metadata::{alt_install, alt_restamp_owners, alt_source_count, alt_stand_in};

fn sid(n: u16) -> ServerId {
    ServerId::from_raw(n)
}
/// A copy on server `s`, in `library`, owned by `owner` (`""` = this account), at class `res`.
fn copy(s: u16, library: &str, owner: &str, rk: &str, res: &str) -> AltCopy {
    AltCopy {
        sid: sid(s),
        library: library.into(),
        owner: (!owner.is_empty()).then(|| owner.to_string()),
        rk: rk.into(),
        dur_ms: 7_020_000, // 1 hr 57 min, the design's own runtime
        res: res.into(),
        width: 0,
        height: 0,
    }
}
fn labels(rs: &[AltRow]) -> Vec<&str> {
    rs.iter().map(|r| r.label.as_str()).collect()
}

/// A mounted panel for `(sid, rk)`, anchored anywhere — the surface with no dispatcher around it,
/// which is all these tests need: the argument carries everything the panel reads.
fn panel(host_sid: ServerId, rk: &str) -> AltSourcesScreen {
    AltSourcesScreen::new(
        EntryId(7),
        AltSourcesArg {
            host: InstanceId(1),
            sid: host_sid,
            rk: rk.to_string(),
            anchor: [0.0f32, 0.0, 100.0, 40.0].map(f32::to_bits),
        },
    )
}

/// What the panel would DRAW — the materialised table, not `rows(alt_copies(..))`. The pure path
/// passes without the rebuild and proves nothing about what is on screen.
fn drawn(p: &AltSourcesScreen) -> Vec<String> {
    p.table.sections[0]
        .rows
        .iter()
        .map(|r| r.detail.clone())
        .collect()
}

/// **The gate.** The control is drawn only when a SECOND pinned source holds the item — so one
/// server, or two copies inside one server, draw nothing at all. The middle case is the one
/// worth pinning: two rows would look like a working feature while the second row led back to
/// the machine you are already on.
#[test]
fn the_button_appears_only_when_a_second_source_holds_the_item() {
    assert_eq!(alt_source_count(&[]), 0, "nothing resolved yet");
    assert_eq!(
        alt_source_count(&[copy(0, "Movies", "", "4", "1080")]),
        1,
        "one source is the 90% install"
    );
    assert_eq!(
        alt_source_count(&[
            copy(0, "Movies", "", "4", "1080"),
            copy(0, "4K Movies", "", "9", "4k")
        ]),
        1,
        "two copies on ONE server are still one source"
    );
    assert_eq!(
        alt_source_count(&[
            copy(0, "Movies", "", "4", "1080"),
            copy(1, "Film Club", "friend", "318", "4k")
        ]),
        2
    );
    // and a third source counts once however many copies it contributes
    assert_eq!(
        alt_source_count(&[
            copy(0, "Movies", "", "4", "1080"),
            copy(1, "Film Club", "friend", "318", "4k"),
            copy(1, "Films", "friend", "319", "1080"),
        ]),
        2
    );
}

/// **The ordering rule, exactly as the design states it**: the copy that plays first, then what
/// a viewer would prefer. The design's own example is the case that separates the two clauses —
/// a 1080p copy you are standing on sorts above a friend's 4K one — and it is the row order
/// the canvas draws.
#[test]
fn the_copy_that_plays_comes_first_and_the_rest_rank_by_preference() {
    let list = [
        copy(1, "Film Club", "friend", "318", "4k"),
        copy(0, "Movies", "", "4", "1080"),
    ];
    let rs = rows(&list, sid(0), "4");
    assert_eq!(
        labels(&rs),
        ["Movies", "Film Club"],
        "the copy that plays leads, whatever its class"
    );
    // the sub-line answers WHOSE and only that; the runtime is the trailing read-out, so it
    // lines up down the panel instead of sitting mid-string at a different x on every row
    assert_eq!(rs[0].detail, "This account");
    assert_eq!(rs[1].detail, "friend");
    assert_eq!(rs[0].value.as_deref(), Some("1 hr 57 min"));
    assert_eq!(rs[1].value.as_deref(), Some("1 hr 57 min"));
    assert_eq!(rs[0].badge.as_deref(), Some("1080p"));
    assert_eq!(
        rs[1].badge.as_deref(),
        Some("4K"),
        "the badge is the hero's own resolution vocabulary"
    );

    // …and standing on the OTHER copy inverts only the first key, not the rest
    assert_eq!(labels(&rows(&list, sid(1), "318")), ["Film Club", "Movies"]);
}

/// Below the copy that plays, quality decides — and only at EQUAL quality does "yours cannot go
/// offline mid-film" break the tie. Both halves are asserted against the same fixture, because
/// getting the keys in the other order would pass a test for either one alone.
#[test]
fn quality_outranks_ownership_and_ownership_breaks_the_tie() {
    let here = copy(9, "On now", "", "1", "720");
    let mine = copy(0, "Movies", "", "4", "1080");
    let theirs_4k = copy(1, "Film Club", "friend", "318", "4k");
    let theirs_hd = copy(2, "Kino", "carol", "77", "1080");

    let rs = rows(
        &[
            here.clone(),
            mine.clone(),
            theirs_4k.clone(),
            theirs_hd.clone(),
        ],
        sid(9),
        "1",
    );
    assert_eq!(
        labels(&rs),
        ["On now", "Film Club", "Movies", "Kino"],
        "playing copy, then 4K, then the two 1080p with mine in front"
    );

    // the input order must not decide anything — the same set shuffled sorts identically
    let shuffled = rows(&[theirs_hd, theirs_4k, mine, here], sid(9), "1");
    assert_eq!(labels(&shuffled), labels(&rs));
}

/// The badge and the sort key are read off the SAME fields in the same precedence, so a list
/// can never be ordered against a ladder the badges contradict. (The server's class wins over
/// the stored frame size: a 2.35:1 1080p film is 1918x802, which a height rule would sort as
/// 720p while its badge said 1080p.)
#[test]
fn the_sort_key_agrees_with_the_badge_it_is_drawn_beside() {
    let with = |res: &str, w: i64, h: i64| AltCopy {
        res: res.into(),
        width: w,
        height: h,
        ..Default::default()
    };
    let ladder = ["8k", "4k", "1080", "720", "576", "sd"];
    for pair in ladder.windows(2) {
        let (a, b) = (with(pair[0], 0, 0), with(pair[1], 0, 0));
        assert!(
            scan_lines(&a) > scan_lines(&b),
            "{} must outrank {}",
            pair[0],
            pair[1]
        );
    }
    // the class beats the frame size, exactly as `fmt::resolution` badges it
    let scope = with("1080", 1918, 802);
    assert_eq!(scan_lines(&scope), 1080);
    assert_eq!(
        crate::ui::fmt::resolution(&scope.res, scope.width, scope.height).as_deref(),
        Some("1080p")
    );
    // …and with no class at all both fall back to the frame, and still agree
    let noclass = with("", 3840, 2160);
    assert!(scan_lines(&noclass) > scan_lines(&with("", 1920, 1080)));
    assert_eq!(
        crate::ui::fmt::resolution(&noclass.res, noclass.width, noclass.height).as_deref(),
        Some("4K")
    );
    // a garbage height must not overflow into a top-of-list key
    assert!(scan_lines(&with("", i64::MAX, i64::MAX)) > 0);
}

/// **Exactly one copy is marked current** — the row model's whole claim about the tick. Marked
/// once even when a producer lists the same copy twice, and marked NOWHERE (never twice, never
/// on a guess) when the page's own copy is not in the list at all.
#[test]
fn exactly_one_row_is_ever_ticked() {
    let ticked = |rs: &[AltRow]| rs.iter().filter(|r| r.checked).count();

    let list = [
        copy(0, "Movies", "", "4", "1080"),
        copy(1, "Film Club", "friend", "318", "4k"),
    ];
    let rs = rows(&list, sid(0), "4");
    assert_eq!(ticked(&rs), 1);
    assert!(
        rs[0].checked,
        "the tick is on the copy the page is standing on"
    );

    // a duplicated copy: one identity, one tick
    let dup = [
        copy(0, "Movies", "", "4", "1080"),
        copy(0, "Movies", "", "4", "1080"),
        copy(1, "LDN", "b", "318", "4k"),
    ];
    assert_eq!(
        ticked(&rows(&dup, sid(0), "4")),
        1,
        "one identity cannot be two 'you are here's"
    );

    // the same ratingKey on the OTHER server is a different copy — rk alone must not tick it
    assert_eq!(
        ticked(&rows(
            &[
                copy(0, "Movies", "", "4", "1080"),
                copy(1, "LDN", "b", "4", "4k")
            ],
            sid(0),
            "4"
        )),
        1
    );
    // …and nothing is ticked when the page's copy is not in the list yet
    assert_eq!(
        ticked(&rows(&list, sid(7), "4")),
        0,
        "no guess, no second tick"
    );
}

/// A copy the server sent no runtime for leaves the read-out slot EMPTY and still says whose it
/// is — never "0 min", the dangling-clause rule the hero's facts row follows.
#[test]
fn a_copy_with_no_runtime_states_only_its_owner() {
    let mut c = copy(1, "Film Club", "friend", "318", "4k");
    c.dur_ms = 0;
    let rs = rows(&[c], sid(0), "4");
    assert_eq!(rs[0].detail, "friend");
    assert_eq!(rs[0].value, None, "an unknown runtime is absent, not zero");
    // and one with no video at all carries no badge rather than an empty chip
    let mut n = copy(0, "Movies", "", "4", "");
    n.dur_ms = 0;
    assert_eq!(rows(&[n], sid(0), "4")[0].badge, None);
}

/// OK NAVIGATES — and the row you are already on is not a destination: it reports nothing, so
/// the panel simply dismisses rather than re-mounting the page under itself. An out-of-range
/// selection is `None` too, never a neighbouring row's server.
///
/// It is graded on the ROW LIST the panel drew, which since phase 10 is also the destination map:
/// a parallel `DESTS` vector is one more thing that can be resolved against a differently-ordered
/// list, and deleting it is what makes "the row list IS the mapping" true rather than asserted.
#[test]
fn ok_navigates_to_another_copy_and_never_to_the_one_you_are_on() {
    let row = |s: u16, rk: &str| AltRow {
        label: String::new(),
        detail: String::new(),
        value: None,
        badge: None,
        checked: false,
        sid: sid(s),
        rk: rk.into(),
    };
    let list = [row(0, "4"), row(1, "318"), row(2, "")];
    assert_eq!(
        action_at(&list, 0, sid(0), "4"),
        Action::None,
        "the copy you are on goes nowhere"
    );
    assert_eq!(
        action_at(&list, 1, sid(0), "4"),
        Action::Open {
            sid: sid(1),
            rk: "318".into()
        }
    );
    assert_eq!(
        action_at(&list, 2, sid(0), "4"),
        Action::None,
        "a copy with no ratingKey is not a destination"
    );
    assert_eq!(action_at(&list, 3, sid(0), "4"), Action::None);
    assert_eq!(action_at(&list, -1, sid(0), "4"), Action::None);
    assert_eq!(action_at(&[], 0, sid(0), "4"), Action::None);
}

/// The headless stand-in describes what a device capture is looking at, so its SHAPE is graded
/// here: your real copy plus the same film on a second slot, one class better — which is the
/// design's own example, and the only arrangement in which a still can show that the ordering
/// rule put the copy that PLAYS above the better one.
#[test]
fn the_headless_stand_in_shows_the_case_the_ordering_rule_turns_on() {
    let v = alt_stand_in("friend", "Movies", "4", "1080", 7_020_000, sid(0), sid(1));
    assert_eq!(
        alt_source_count(&v),
        2,
        "…or the gate would refuse the very panel it exists to show"
    );
    assert_eq!(
        v[0].owner, None,
        "your copy is the real one, on the current server"
    );
    assert_eq!((v[0].rk.as_str(), v[0].dur_ms), ("4", 7_020_000));
    assert_eq!(v[1].owner.as_deref(), Some("friend"));
    assert_eq!(v[1].rk, v[0].rk, "the same film — the slot is what differs");

    let rs = rows(&v, sid(0), "4");
    assert!(rs[0].checked, "the tick is on yours…");
    assert_eq!(rs[0].badge.as_deref(), Some("1080p"));
    assert_eq!(
        rs[1].badge.as_deref(),
        Some("4K"),
        "…and the better copy is the one BELOW it"
    );

    // the one invention is bounded: the top of the ladder is not promoted past itself, and an
    // unrecognised class is left exactly as the server spelled it
    let better = |res: &str| {
        alt_stand_in("f", "L", "1", res, 0, sid(0), sid(1))[1]
            .res
            .clone()
    };
    assert_eq!(better("4k"), "4k");
    assert_eq!(better("sd"), "720");
    assert_eq!(better(""), "");
    assert_eq!(better("weird"), "weird");
}

/// **A corrected credit re-stamps rows already on an OPEN page**, which is the sixth and last
/// surface that draws the "Shared by …" decision (`plex::servers::owner_credit`,
/// `docs/shared-servers.md` §13).
///
/// `AltCopy::owner` is a COPY of the registry's credit, taken when the cross-source resolve
/// landed. When a roster refresh re-grades that credit — the household's own server ceasing to
/// be captioned with the account holder's handle — every other surface follows and this one
/// could not: `metadata::pump_alt_sources` answered the change by invalidating the resolve and
/// pruning, which retains the installed copies untouched and restarts nothing, so the row went
/// on naming the person watching until the page was remounted.
///
/// The row ORDER is asserted with it, because `owner` is the own-before-a-friend's tiebreak in
/// [`rows`] and a restamp that did not reach the ordering would put the household's copy below
/// a friend's on a page that had just decided they were equals.
///
/// **The last leg of the legacy version is now structural rather than tested.** It checked that a
/// panel already DISMISSED but still fading kept following corrections, because `rebuild_if_showing`
/// gated on `Popover::visible()` rather than `is_open()`. A surface has no such flag to get wrong:
/// the container owns the phase, a `Closing` surface still receives `Tick` and `StoreChanged`
/// (`ModalStack::tick`'s own rule), and this screen's refresh is ungated. What replaces that leg is
/// the assertion below that a REFRESH with no mount and no phase at all still rebuilds.
#[test]
fn a_re_described_source_restamps_the_credit_on_an_open_page() {
    struct Fresh(#[allow(dead_code)] crate::testlock::Serial);
    impl Drop for Fresh {
        fn drop(&mut self) {
            crate::stores::metadata::apply(crate::stores::metadata::MetadataCmd::Clear);
            crate::plex::reset_servers_for_test();
        }
    }
    let _g = Fresh(crate::testlock::serial());
    crate::plex::reset_servers_for_test();
    let house = crate::plex::register_for_test("alt-house", "127.0.0.1", 1, "t", "cid");
    let friend = crate::plex::register_for_test("alt-friend", "127.0.0.1", 2, "t", "cid");
    assert_eq!((house, friend), (sid(0), sid(1)), "slots 0 and 1");

    // what a build without the rule published: the household's own server wearing the account
    // holder's handle, and the panel's rows stamped from it
    crate::plex::describe_server(house, "Mac mini", "admin", false);
    crate::plex::describe_server(friend, "nas-home", "friend", false);
    alt_install(
        house,
        "4",
        vec![
            copy(0, "Movies", "admin", "4", "1080"),
            copy(1, "Film Club", "friend", "318", "1080"),
        ],
    );
    let copies = || crate::metadata::alt_copies(house, "4");
    assert_eq!(
        rows(copies(), house, "4")
            .iter()
            .map(|r| r.detail.clone())
            .collect::<Vec<_>>(),
        ["admin", "friend"]
    );

    // the roster refresh re-grades the household's own server, with NO new resolve
    crate::plex::describe_server(house, "Mac mini", "", false);
    assert!(alt_restamp_owners(), "the credit moved");

    let after = rows(copies(), house, "4");
    assert_eq!(
        after.iter().map(|r| r.detail.clone()).collect::<Vec<_>>(),
        [OWN_ACCOUNT, "friend"],
        "the row follows the registry off a credit without waiting for a re-resolve"
    );
    assert_eq!(
        copies()[0].owner,
        None,
        "and absence is spelled `None`, never `Some(\"\")`"
    );

    // the friend is untouched — a restamp is not a blanket clear
    assert_eq!(copies()[1].owner.as_deref(), Some("friend"));

    // **An OPEN panel is a materialised table, not a view of the store.** Without the rebuild it
    // keeps both its old text and its old ORDER — and the order is not cosmetic, `owner` is the
    // own-before-a-friend's tiebreak — until the user closes and reopens it.
    crate::plex::describe_server(house, "Mac mini", "admin", false);
    alt_restamp_owners();
    let mut p = panel(house, "4");
    // the page's own copy is `(house, "4")`, so its row wears the tick and leads
    assert_eq!(drawn(&p), ["admin", "friend"]);

    crate::plex::describe_server(house, "Mac mini", "", false);
    alt_restamp_owners();
    assert!(p.refresh(), "the correction reached the drawn table");
    assert_eq!(
        p.table.n_rows(),
        2,
        "the open panel is rebuilt, not emptied or duplicated"
    );
    assert_eq!(
        drawn(&p),
        [OWN_ACCOUNT, "friend"],
        "an OPEN panel follows the correction; it is a snapshot, not a view of the store"
    );
    assert!(
        !p.refresh(),
        "…and a refresh with nothing to say rebuilds nothing"
    );
}

/// **A resolve dispatched BEFORE the correction, landing AFTER it.** The worker reads the
/// credit off the registry on a background thread; by the time its list reaches the main
/// thread the roster refresh can have re-graded that credit AND `pump_alt_sources` can have
/// consumed the facts epoch it moved. Nothing downstream would ever look again, so the stale
/// stamp would have outlived every correction — which is why `alt_install` regrades rather than
/// trusting what the worker carried.
#[test]
fn a_resolve_that_landed_after_the_correction_is_regraded_on_the_way_in() {
    struct Fresh(#[allow(dead_code)] crate::testlock::Serial);
    impl Drop for Fresh {
        fn drop(&mut self) {
            crate::stores::metadata::apply(crate::stores::metadata::MetadataCmd::Clear);
            crate::plex::reset_servers_for_test();
        }
    }
    let _g = Fresh(crate::testlock::serial());
    crate::plex::reset_servers_for_test();
    let house = crate::plex::register_for_test("alt-late-house", "127.0.0.1", 1, "t", "cid");
    let friend = crate::plex::register_for_test("alt-late-friend", "127.0.0.1", 2, "t", "cid");
    crate::plex::describe_server(house, "Mac mini", "admin", false);
    crate::plex::describe_server(friend, "nas-home", "friend", false);

    // the worker's list, stamped while the old credit was still published
    let in_flight = vec![
        copy(0, "Movies", "admin", "4", "1080"),
        copy(1, "Film Club", "friend", "318", "1080"),
    ];

    // …then the correction lands, and the epoch that saw it is already spent
    crate::plex::describe_server(house, "Mac mini", "", false);
    alt_restamp_owners();

    // …and only now does the resolve arrive
    alt_install(house, "4", in_flight);

    let copies = crate::metadata::alt_copies(house, "4");
    assert_eq!(
        copies[0].owner, None,
        "the list is graded against the registry as it is NOW, not as the worker found it"
    );
    assert_eq!(copies[1].owner.as_deref(), Some("friend"));
}

/// **The store is ADDRESSED on the pair, and the pair is what a reader supplies.** A cross-source
/// resolve is one round trip PER SOURCE and a share that has gone away costs a whole `connect(2)`
/// timeout, so one asked for the page you just left routinely lands seconds after you have opened
/// another — and with two servers registered "another page with the same ratingKey" is the
/// ordinary case, since both number their items from 1.
///
/// Nothing upstream can catch it: `metadata::pump_alt_sources`' generation guard only moves
/// when a DETAIL lands, and the newly opened page's has not. So an rk-only test here put the
/// OTHER machine's copies on this hero, where the tick would be missing (no listed copy matches
/// the page) and OK would open a different film on a server you were not looking at.
///
/// Since phase 10 the refusal happens on the way OUT rather than on the way in, and that is what
/// removed the whole class of failure: a MAILBOX has to be STAMPED by the page before a landing can
/// be accepted, and the owned `DetailScreen` of phase 7 stopped stamping it — see `AltStore`'s doc.
#[test]
fn a_landing_for_another_servers_copy_with_the_same_key_is_refused() {
    struct Fresh(#[allow(dead_code)] crate::testlock::Serial);
    impl Drop for Fresh {
        fn drop(&mut self) {
            crate::stores::metadata::apply(crate::stores::metadata::MetadataCmd::Clear);
        }
    }
    let _g = Fresh(crate::testlock::serial());
    crate::stores::metadata::apply(crate::stores::metadata::MetadataCmd::Clear);
    let available = crate::metadata::alt_available;
    let two_sources = || {
        vec![
            copy(0, "Movies", "", "4", "1080"),
            copy(1, "Film Club", "friend", "318", "4k"),
        ]
    };

    assert!(!available(sid(0), "4"), "a fresh mount holds no copies");
    // OUR film 4's resolve lands while the user is on the SHARE's film 4 — the same key, another
    // machine
    alt_install(sid(0), "4", two_sources());
    assert!(
        !available(sid(1), "4"),
        "our copies are not news about the share's film"
    );

    // the control: the very same landing IS the answer for the page that asked, so the refusal
    // above is about the SERVER and not about the mechanism
    assert!(available(sid(0), "4"), "the awaited landing installs");

    // and the pre-existing rule is untouched: the same server, a different item
    assert!(
        !available(sid(0), "318"),
        "a landing for another item on this server is still refused"
    );

    // a closed page (UNSET, empty rk) can reach nothing
    assert!(
        !available(ServerId::UNSET, ""),
        "nothing lands on a page that is gone"
    );
}

/// The panel hangs off the control that opened it, and is never over it or off the screen.
#[test]
fn the_panel_hangs_off_its_button_and_stays_on_screen() {
    let btn = Rect::new(crate::ui::consts::MARGIN_X, 300.0, 300.0, 60.0);
    let r = panel_at(btn, 224.0);
    assert_eq!(r.w, PANEL_W);
    assert_eq!(
        r.y,
        btn.y + btn.h + BTN_GAP,
        "under the button when there is room"
    );
    assert_eq!(r.x, btn.x, "and aligned to its left edge");

    // a button low on the page flips the panel ABOVE it rather than off the bottom
    let low = Rect::new(crate::ui::consts::MARGIN_X, 900.0, 300.0, 60.0);
    let r = panel_at(low, 224.0);
    assert!(
        r.y + r.h <= low.y - BTN_GAP + 0.01,
        "flipped above the button"
    );
    assert!(r.y >= EDGE);

    // a button near the right edge pulls the panel back inside the keep-out
    let right = Rect::new(SCR_W - 200.0, 300.0, 180.0, 60.0);
    let r = panel_at(right, 224.0);
    assert!(
        r.x + r.w <= SCR_W - EDGE_X + 0.01,
        "a panel must not run off the panel"
    );
    // …and a list taller than the screen is clamped rather than drawn past both edges
    let tall = panel_at(btn, 4000.0);
    assert!(tall.y >= EDGE && tall.y + tall.h <= SCR_H - EDGE + 0.01);

    // Every one of those worst cases is inside the overscan frame — the keep-out is per AXIS
    // precisely so that the horizontal clamp is `MARGIN_X` and not the `space::XL` the vertical
    // one uses. `ui::consts::SAFE` is the frame; this is that predicate on this panel's own
    // extremes, since a panel placed against an ANCHOR has no fixed rect a table could carry.
    for (what, p) in [
        ("under", r),
        ("tall", tall),
        ("right-edge", panel_at(right, 224.0)),
    ] {
        assert!(
            crate::ui::consts::inside_safe(p),
            "the {what} panel leaves the safe area: ({}, {}) {}x{}",
            p.x,
            p.y,
            p.w,
            p.h
        );
    }

    // …and the ANCHOR that decides all of it travels on the argument, bit for bit, so a canonical
    // state can hold it without float equality and a `static mut ANCHOR: PanelAnchor` holding a
    // `Rect` is gone (§15.2: a `Rect` static is never an allowlistable render cache).
    let p = AltSourcesScreen::new(
        EntryId(3),
        AltSourcesArg {
            host: InstanceId(1),
            sid: ServerId::UNSET,
            rk: String::new(),
            anchor: [low.x, low.y, low.w, low.h].map(f32::to_bits),
        },
    );
    let want = panel_at(low, p.table.measured_height());
    let got = p.frame();
    assert_eq!((got.x, got.y, got.w, got.h), (want.x, want.y, want.w, want.h));
}

/// **A page teardown is a CUT; only an interactive exit fades.** Codex review, 2026-09-02: the
/// legacy `reset` called the fading `close`, so a mount that followed a navigation from this menu
/// found `dismiss` a no-op (already not open), left `visible()` true, and drew the previous
/// server's rows over the incoming detail page until the spring ran out.
///
/// Since phase 10 the two verbs are the CONTAINER's — `ModalStack::hide` (jump, retired by the very
/// next `prune`) against `ModalStack::dismiss` (the appear spring run backwards) — and the page
/// being removed is what triggers the first: `Navigation::commit` unmounts a covered stack's
/// surfaces the moment their host page leaves the stack, with no fade to run over a page that is
/// not there. This grades the pair the panel depends on, at the one place it is now decided.
#[test]
fn a_reset_hides_the_menu_at_once_while_back_fades_it() {
    use crate::ui::containers::modal::{ModalStack, Phase, Style};
    use crate::ui::containers::Minter;
    use crate::ui::fixture::{tick, FixtureArg, FixtureHost};
    use crate::ui::machine::PresentHandle;
    use crate::ui::present::Present;

    let opened = || {
        let mut ms: ModalStack<FixtureHost> = ModalStack::new();
        let mut ids = Minter::default();
        let (id, _) = ms.present(&mut ids, FixtureArg::Modal, Style::Compact);
        let mut present = Present::new();
        for i in 0..200u32 {
            let mut ph = PresentHandle::of(&mut present);
            ms.tick(tick(16 + i * 16), &mut ph);
            if ms.surface(id).unwrap().phase == Phase::Open {
                break;
            }
        }
        (ms, id)
    };

    // the teardown
    let (mut ms, id) = opened();
    assert!(ms.hide(id));
    assert_eq!(ms.surface(id).unwrap().motion.appear, 0.0, "JUMPED, not sprung");
    assert_eq!(ms.prune().len(), 2, "gone on the frame it is called");
    assert!(ms.is_empty());

    // BACK, for contrast: the same surface, the same first prune, and it is STILL up
    let (mut ms, id) = opened();
    assert!(ms.dismiss(id));
    assert_eq!(ms.surface(id).unwrap().phase, Phase::Closing);
    assert!(
        ms.surface(id).unwrap().motion.appear > 0.5,
        "…but the sheet is still fading"
    );
    assert!(ms.prune().is_empty());
}
