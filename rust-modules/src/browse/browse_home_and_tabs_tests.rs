//! Home library-selection defaults/persistence per profile, and the type tab strip's
//! pill/position bookkeeping.

use super::*;
#[allow(unused_imports)]
use super::test_support::*;

/// **The first-run defaults, and the one rule the control has.**
///
/// This asserted `(true, true, true)` — every granted library on — for as long as deliverable F
/// had nowhere to ask the question: defaulting a share OFF with no screen to say so means it is
/// granted, discovered, browsable and silently absent from Home with no control anywhere to
/// turn it on. The screen exists now, so the design's own default is back, and it is the state
/// that screen SHOWS before anybody touches it.
#[test]
fn your_own_libraries_start_on_home_and_a_friends_does_not() {
    let _g = crate::testlock::serial();
    let t = TempPins::new("defaults");
    t.watching("u-owner");
    let mut browse = TestBrowse::default();
    seed_two_servers(&mut browse);
    assert_eq!(
        (browse.pinned(0), browse.pinned(1), browse.pinned(2)),
        (true, true, false),
        "yours On, a friend's Off"
    );
    assert_eq!(browse.state.pinned_count(), 2);

    assert!(
        browse.state.toggle_pin(2),
        "…and a friend's can be turned on, which is what makes it a decision"
    );
    assert_eq!(browse.state.pinned_count(), 3);
    assert!(
        browse.state.toggle_pin(0) && browse.state.toggle_pin(1),
        "your own can be unpinned — a preference, not a mistake"
    );
    assert_eq!(browse.state.pinned_count(), 1);

    assert!(browse.pinned(2) && browse.state.pinned_count() == 1);
    assert!(!browse.state.toggle_pin(2), "the last pinned library is refused");
    assert!(browse.pinned(2), "…and refused means UNCHANGED, not toggled twice");
    assert_eq!(browse.state.pinned_count(), 1);
}
/// **The SHARED fixture's shape is its own, not the disk's.**
///
/// [`seed_two_source_table_for_test`] is used by three dozen tests in a dozen modules and its
/// doc promises one thing — four libraries projecting to two library-type pills. That promise
/// is about the pins as much as about the table, because [`append_sections`] ends in
/// [`resolve_pins`]. This plants exactly the record that broke it: an answer for the CURRENT
/// profile naming this fixture's own machines, with Movies switched off.
///
/// It is the regression artifact for a failure that was NOT a race. A record of this shape,
/// for the empty profile key, was sitting in one checkout's `target/debug/deps/auth.json` —
/// which is where `paths::in_app_dir` resolves for a TEST BINARY — and it made
/// `app::bridge::library_publishes_the_actual_container_strip` and
/// `app::chrome::four_libraries_on_two_servers_publish_two_type_destinations` fail alone,
/// single-threaded, in that checkout only. Run against the fixture as it was, this test is red
/// with the Movies pill missing, in exactly the way those two were.
#[test]
fn the_shared_fixture_resolves_the_defaults_over_a_recorded_answer() {
    let _g = crate::testlock::serial();
    let t = TempPins::new("fixture-owns-its-pins");
    t.watching("u-fixture-owns-its-pins");
    let user = crate::plex::session::current_profile_key();
    let lib = |machine: &str, key| crate::plex::session::PinnedLib {
        machine_id: machine.into(),
        key,
        extensions: Default::default(),
    };
    assert!(
        crate::plex::session::update(|s| {
            let mut next = s.clone();
            next.set_pins_for(
                &user,
                crate::plex::session::HomePins {
                    user: user.clone(),
                    asked: true,
                    on: vec![lib("mac-mini", 2)],
                    off: vec![lib("mac-mini", 1), lib("nas-home", 1), lib("nas-home", 2)],
                    extensions: Default::default(),
                },
            );
            Some(next)
        }),
        "the answer really is on disk, or this test grades nothing"
    );

    let mut browse = TestBrowse::default();
    seed_two_source_table_for_owner_test(&mut browse.state);

    assert_eq!(
        (browse.state.tab_kind(0), browse.state.tab_kind(1), browse.tab_count()),
        (Some(SecKind::Movie), Some(SecKind::Show), 2),
        "the fixture's own two pills, whatever anybody recorded for this profile"
    );
    assert_eq!(
        (browse.pinned(0), browse.pinned(1), browse.pinned(2), browse.pinned(3)),
        (true, true, false, false),
        "…and they are the OWNERSHIP defaults: yours On, a friend's Off"
    );
    assert!(
        crate::plex::session::peek().pins_for(&user).is_none(),
        "the record was forgotten rather than worked around, so a later resolve agrees"
    );
}
/// **The editor's commit is one write for the whole batch, not one per toggle.** [`toggle_pin`]
/// above records on every call because it has no draft standing between the press and the
/// store; [`apply_pins`] is what a caller with one (`screens::onboard`) reaches for instead —
/// every
/// edit lands in the SAME `record_pins` call, so an editing session that flips three rows costs
/// this app one write and one generation bump, exactly as it costs one press of Done.
#[test]
fn apply_pins_writes_the_whole_batch_in_one_record() {
    let _g = crate::testlock::serial();
    let t = TempPins::new("apply-pins");
    t.watching("u-owner");
    let mut browse = TestBrowse::default();
    seed_two_servers(&mut browse);
    assert_eq!((browse.pinned(0), browse.pinned(1), browse.pinned(2)), (true, true, false));

    browse.state.apply_pins(&[(2, true), (1, false)]);
    assert_eq!(
        (browse.pinned(0), browse.pinned(1), browse.pinned(2)),
        (true, false, true),
        "every edit in the batch landed"
    );

    let sess = crate::plex::session::peek();
    let rec = sess.pins_for(&crate::plex::session::current_profile_key());
    assert!(
        rec.is_some_and(|r| r.asked),
        "one commit is still a recorded answer"
    );
}
/// **An explicit answer that a DEFAULT has caught up with is still an answer.**
///
/// Codex review finding 1 (2026-09-20) against the household-grading work, and the sequence is
/// the whole of it: before the Plex Home roster lands, a friend's films read On; the viewer
/// switches them Off; the roster arrives, the household turns out to have films of its own and
/// the LIVE pin moves to Off as well. `apply_pins` used to INFER "the viewer touched this row"
/// from "the row disagrees with the live pin", so the commit saw `Off == Off`, read the row as
/// untouched and wrote nothing — and an unrecorded row goes on re-deriving, which is how an
/// explicit Off comes back On the day the household stops having films of its own.
///
/// The command carries the rows ANSWERED now (`screens::onboard`'s `touched`, the set its own
/// presses filled in), so a row whose value the world agreed with is recorded exactly like one it
/// did not.
#[test]
fn an_answer_the_live_pin_already_agrees_with_is_recorded_anyway() {
    let _g = crate::testlock::serial();
    let t = TempPins::new("answer-agrees");
    t.watching("u-owner");
    let mut browse = TestBrowse::default();
    seed_two_servers(&mut browse);
    assert!(
        !browse.pinned(2),
        "the friend's films are Off by DEFAULT here — nobody's answer yet"
    );

    // The one commit, carrying the one row the viewer answered: Off, which is what the live pin
    // already reads because a roster correction got there first.
    browse.state.apply_pins(&[(2, false)]);

    let user = crate::plex::session::current_profile_key();
    assert_eq!(
        crate::plex::session::peek().pins_for(&user).and_then(|r| r.answer("nas-home", 1)),
        Some(false),
        "the answer was written down rather than mistaken for the default it agreed with"
    );

    // …and why it matters: the household loses its only Movies library, so a friend's films are
    // now a type the household has none of and `default_on` would raise them. The never-empty
    // floor is not in play — the household's TV library is still on.
    browse.seed_sources(vec![
        a_source("mac-mini", "", true),
        a_source("nas-home", "friend", true),
    ]);
    browse.append_sections(0, vec![(2, "TV Shows".into(), SecKind::Show)]);
    browse.append_sections(1, vec![(1, "Film Club".into(), SecKind::Movie)]);
    assert_eq!(
        (browse.pinned(0), browse.pinned(1)),
        (true, false),
        "the explicit Off outlived the default that would otherwise have brought it back"
    );
}
/// **After a commit, what the table SHOWS is what the commit SAVED.**
///
/// Codex review finding 2 (2026-09-20), and it is the provenance change's own doing: once
/// `record_pins` writes only the rows somebody answered, the untouched rows the never-empty floor
/// had RAISED keep their raised value on screen while the record keeps the older answer — so the
/// next resolve, or the next boot, silently puts one back down with no user action behind it.
/// A plain account owner, three of their own machines, and no share anywhere near it.
#[test]
fn a_commit_leaves_the_table_showing_what_it_saved() {
    let _g = crate::testlock::serial();
    let t = TempPins::new("commit-reconcile");
    t.watching("u-owner");
    let user = crate::plex::session::current_profile_key();
    let lib = |machine: &str, key| crate::plex::session::PinnedLib {
        machine_id: machine.into(),
        key,
        extensions: Default::default(),
    };
    assert!(
        crate::plex::session::update(|s| {
            let mut next = s.clone();
            next.set_pins_for(
                &user,
                crate::plex::session::HomePins {
                    user: user.clone(),
                    asked: true,
                    on: vec![lib("laptop", 1)],
                    off: vec![lib("mac-mini", 1), lib("study-nas", 1)],
                    extensions: Default::default(),
                },
            );
            Some(next)
        }),
        "the three recorded answers really are on disk, or this test grades nothing"
    );

    // The only machine that was On leaves the roster. Nothing is left on, so the never-empty
    // floor raises the FIRST source's libraries: row 0 reads On without anybody having said so.
    let mut browse = TestBrowse::default();
    browse.seed_sources(vec![
        a_source("mac-mini", "", true),
        a_source("study-nas", "", true),
    ]);
    browse.append_sections(0, vec![(1, "Movies".into(), SecKind::Movie)]);
    browse.append_sections(1, vec![(1, "Cinema".into(), SecKind::Movie)]);
    assert_eq!(
        (browse.pinned(0), browse.pinned(1)),
        (true, false),
        "the floor raised the first source's library over two recorded Offs"
    );

    // The viewer switches the other one on and leaves the raised row alone.
    browse.state.apply_pins(&[(1, true)]);

    let saved = crate::plex::session::peek();
    let recorded = saved.pins_for(&user).and_then(|r| r.answer("mac-mini", 1));
    assert_eq!(
        (browse.pinned(0), recorded),
        (false, Some(false)),
        "the raised row went back to what the record says, instead of disagreeing with it until \
         the next resolve"
    );
    assert!(
        browse.pinned(1),
        "…and the answer that was just given is not undone by the same reconcile"
    );
}
/// **A commit the SESSION REFUSED leaves the viewer's answer on screen.**
///
/// The other half of the reconcile above, and the regression it arrived with. `session::update`
/// refuses the whole read-modify-write when the live read is unusable — a Locked/Blocked record
/// whose device key the keymanager will not hand over, or no file at all — so its closure never
/// runs and this commit produces NO record. An unconditional reconcile then ran against the
/// record standing from the last resolve, which is the answer this commit was replacing: the
/// viewer switched a library off, pressed Done, and watched the row come back on by itself.
///
/// `session::update`'s own contract for that refusal is explicit: the caller keeps its change in
/// memory for the run. So the table keeps the answer, nothing is recorded, and the next resolve
/// is the first thing entitled to move the row again.
#[test]
fn a_commit_the_session_refuses_leaves_the_answer_on_screen() {
    let _g = crate::testlock::serial();
    let t = TempPins::new("refused-commit");
    t.watching("u-owner");
    let mut browse = TestBrowse::default();
    seed_two_servers(&mut browse);
    assert_eq!(
        (browse.pinned(0), browse.pinned(1)),
        (true, true),
        "two libraries of this account's own, both on Home"
    );

    // The device key is unavailable: what is on disk identifies itself as a secure envelope this
    // build cannot open, which is the `ReadState::Locked` every keymanager failure lands on. The
    // file is written directly rather than through `save`, exactly as a keymanager going away
    // under a record this process already wrote would leave it.
    std::fs::write(
        t.path(),
        br#"{"format":"plxnative-secure-session","version":99,"sealed":{}}"#,
    )
    .expect("the locked fixture");
    crate::plex::session::invalidate_for_test();

    // One of them switched off, and Done pressed.
    browse.state.apply_pins(&[(1, false)]);

    assert_eq!(
        (browse.pinned(0), browse.pinned(1), browse.pinned(2)),
        (true, false, false),
        "the answer stands: a commit that produced no record must not be reconciled against the \
         record it was replacing, which restores exactly what the viewer just changed"
    );
    assert!(
        crate::plex::session::peek()
            .pins_for(&crate::plex::session::current_profile_key())
            .is_none(),
        "…and nothing was recorded — the answer is this RUN's, and the record is untouched"
    );
}
/// **A selection outlives the run.** Every flip was in-memory until 2026-08-21, so the answer
/// was gone by the next boot and the ownership default came back — which reads as the switch
/// not working rather than as nothing having been written down.
#[test]
fn a_selection_survives_the_table_being_rebuilt() {
    let _g = crate::testlock::serial();
    let t = TempPins::new("persist");
    t.watching("u-owner");
    let mut browse = TestBrowse::default();
    seed_two_servers(&mut browse);
    assert!(browse.state.toggle_pin(2) && browse.state.toggle_pin(1)); // the share On, one of ours Off
    assert_eq!((browse.pinned(0), browse.pinned(1), browse.pinned(2)), (true, false, true));

    // …and now the table is wiped and re-discovered, which is what a profile switch, a
    // sign-in and a `reset` all do
    seed_two_servers(&mut browse);
    assert_eq!(
        (browse.pinned(0), browse.pinned(1), browse.pinned(2)),
        (true, false, true),
        "the answer came back"
    );
}
/// **The answer reaches Home before that server's libraries have been ENUMERATED.**
///
/// Every catalog screen drives discovery, but the share's Home hubs may land before its section
/// worker. In that interval the share is in the roster with no row in the section table — and
/// `pms::feeds_home`'s "a library nobody has discovered is undecided, not unpinned" rule then
/// put a friend's shelves back on the front door of somebody who had turned them off the night
/// before. `library_pins` is the join, and the recorded answer is the other half of it.
#[test]
fn a_recorded_answer_reaches_home_before_that_servers_sections_do() {
    let _g = crate::testlock::serial();
    let t = TempPins::new("unenumerated");
    t.watching("u-owner");
    let mut browse = TestBrowse::default();
    seed_two_servers(&mut browse);
    // The friend's library, turned on and then off again — an ANSWER about it rather than the
    // default it happens to agree with. Only a row somebody moved is written down now
    // (`plex::pins::answers`), so a fixture that wants a record has to make the decision.
    assert!(browse.state.toggle_pin(2) && browse.state.toggle_pin(2));
    assert!(!browse.pinned(2), "back where it started, but now on the record");

    // the next boot, before the share's section worker has landed
    let boot = |browse: &mut TestBrowse| {
        browse.seed_sources(vec![
            a_source("mac-mini", "", true),
            a_source("nas-home", "friend", true),
        ]);
        browse.append_sections(
            0,
            vec![
                (1, "Movies".into(), SecKind::Movie),
                (2, "TV Shows".into(), SecKind::Show),
            ],
        );
    };
    boot(&mut browse);
    assert_eq!(
        browse.state.sections().len(),
        2,
        "the share has not answered — it contributes no rows"
    );
    let pins = browse.state.library_pins();
    assert!(
        pins.contains(&(1, 1, false)),
        "the friend's recorded Off is reported anyway, or Home reads it as undecided: {pins:?}"
    );
    assert_eq!(
        pins.len(),
        3,
        "…and nothing else is invented: two enumerated rows plus the one record"
    );

    // the other direction, so this is a JOIN and not a blanket "a share is off"
    t.watching("u-owner");
    seed_two_servers(&mut browse);
    assert!(browse.state.toggle_pin(2)); // the toggle IS the write; nothing else is needed
    boot(&mut browse);
    assert!(
        browse.state.library_pins().contains(&(1, 1, true)),
        "a recorded On reaches Home the same way"
    );

    // and a source the record cannot NAME is left undecided rather than joined by accident
    browse.seed_sources(vec![a_source("mac-mini", "", true), {
        let mut s = a_source("nas-home", "friend", true);
        s.machine_id = String::new();
        s
    }]);
    browse.append_sections(0, vec![(1, "Movies".into(), SecKind::Movie)]);
    assert!(
        browse.state.library_pins().iter().all(|&(si, _, _)| si == 0),
        "a nameless machine joins nothing"
    );
}
/// **A flip made while a friend's server is asleep does not withdraw the answer about it.**
///
/// `record_pins` writes the section TABLE, which holds only what has answered — and
/// `set_pins_for` replaces a profile's record wholesale. So without the merge, one switch
/// flipped on a boot the share missed erased the share's recorded answer, and the ownership
/// default came back for a library the user had already decided about.
#[test]
fn a_flip_made_while_a_share_is_absent_does_not_erase_its_answer() {
    let _g = crate::testlock::serial();
    let t = TempPins::new("merge");
    t.watching("u-owner");
    let mut browse = TestBrowse::default();
    seed_two_servers(&mut browse);
    assert!(browse.state.toggle_pin(2), "the friend's library goes on Home");
    assert_eq!((browse.pinned(0), browse.pinned(1), browse.pinned(2)), (true, true, true));

    // a boot the share missed entirely, on which one of our own is turned off
    browse.seed_sources(vec![
        a_source("mac-mini", "", true),
        a_source("nas-home", "friend", true),
    ]);
    browse.append_sections(
        0,
        vec![
            (1, "Movies".into(), SecKind::Movie),
            (2, "TV Shows".into(), SecKind::Show),
        ],
    );
    assert!(browse.state.toggle_pin(1));
    assert!(
        browse.state.library_pins().contains(&(1, 1, true)),
        "the absent share is still recorded On"
    );

    // …and the next boot on which it DOES answer finds both answers intact
    seed_two_servers(&mut browse);
    assert_eq!((browse.pinned(0), browse.pinned(1), browse.pinned(2)), (true, false, true));
}
/// **THE requirement: the selection is per PROFILE.** It hung off the `Session` — one per
/// install — so a household could hold exactly one opinion about a friend's films, and
/// switching profile left the previous person's shelves on the front door.
///
/// **Seeded as a MANAGED profile sees the house**, which is what the two people switching here
/// actually are. `seed_two_servers` is the admin's view — `mac-mini` `owned:true` — and that made
/// this test quietly unable to reach the case it is about: two profiles in one Plex Home, both of
/// whom plex.tv answers `owned:false` on the family server, indistinguishable from the friend's
/// share beside it on the raw flag alone. Every expectation below is unchanged, and that IS the
/// assertion: a managed profile's per-profile selection behaves exactly as the admin's does.
#[test]
fn two_profiles_keep_their_own_home_selections_across_a_switch() {
    let _g = crate::testlock::serial();
    let t = TempPins::new("profiles");
    let mut browse = TestBrowse::default();

    // Dad wants the friend's films on Home and does not want the household's TV shows there.
    t.watching("u-dad");
    seed_two_servers_managed(&mut browse);
    assert!(browse.state.toggle_pin(2) && browse.state.toggle_pin(1));
    assert_eq!((browse.pinned(0), browse.pinned(1), browse.pinned(2)), (true, false, true));

    // The kid switches in. Never asked, so the defaults — NOT dad's answer.
    t.watching("u-kid");
    seed_two_servers_managed(&mut browse);
    assert_eq!(
        (browse.pinned(0), browse.pinned(1), browse.pinned(2)),
        (true, true, false),
        "a switch switches the shelves"
    );
    assert!(browse.state.toggle_pin(0), "…and the kid answers for themselves");
    assert_eq!((browse.pinned(0), browse.pinned(1), browse.pinned(2)), (false, true, false));

    // …and back, with dad's answer intact rather than overwritten by the kid's.
    t.watching("u-dad");
    seed_two_servers_managed(&mut browse);
    assert_eq!(
        (browse.pinned(0), browse.pinned(1), browse.pinned(2)),
        (true, false, true),
        "one file, two answers"
    );
}
/// The route's own gate, end to end: two sources and an unanswered profile, then never again
/// for that profile — while the person beside them is still owed the question.
#[test]
fn the_first_run_question_is_asked_once_per_profile() {
    let _g = crate::testlock::serial();
    let t = TempPins::new("gate");
    t.watching("u-dad");
    let mut browse = TestBrowse::default();
    seed_two_servers(&mut browse);
    assert!(
        browse.first_run_asks(),
        "two sources, and nobody has asked this profile"
    );

    // What `Start watching` — and BACK, which commits the same thing — does with nothing touched:
    // it records that the question was PUT, and no answers, because the viewer gave none.
    browse.state.record_pins(true, &[]);
    assert!(!browse.first_run_asks(), "asked once, never again");
    t.watching("u-kid");
    assert!(
        browse.first_run_asks(),
        "…and the answer belongs to the person who gave it"
    );

    // A single-server install is not a question at all, whoever is watching.
    browse.seed_sources(vec![a_source("mac-mini", "", true)]);
    browse.append_sections(0, vec![(1, "Movies".into(), SecKind::Movie)]);
    assert!(!browse.first_run_asks());
}
/// **The switch governs the strip, and a strip POSITION is not a name.**
///
/// Both halves in one run, because they are the same fact seen twice. Switching off the last
/// favourite of a type removes its pill — the design's "a type left with no ON library draws
/// no pill" — and the moment that can happen, *TV Shows* stops being pill 1 and becomes pill
/// 0. Anything that stored the integer is now pointing at the wrong destination, which is why
/// `ui::widgets::Pill::Section` carries a `SecKind`.
///
/// The pin state is SET rather than resolved, deliberately: `resolve_pins` consults this
/// machine's own signed-in session (`session::peek`), so a test that let it decide would be
/// asserting against whatever `home_pins` the developer happens to have recorded — green here
/// and red on a clean checkout, the shape `[[make-check-hides-host-assumptions]]` describes.
/// That hazard is older than this test and is not this landing's to fix; stating the intent is.
/// **Issue #68, at the layer that can answer it.** Two TV libraries on one server sit behind
/// ONE *TV Shows* pill — `tab_section` opens exactly one of them — so the head of the Library
/// is the only place that can say the other exists. This is the fact it says it from.
///
/// The three assertions are the three ways the head can be wrong: silence where there is a
/// choice, a `1 of 1` where there is not, and a position that disagrees with the panel the
/// same head opens.
#[test]
fn two_libraries_behind_one_pill_have_a_position_and_a_lone_one_has_none() {
    let _g = crate::testlock::serial();
    let _t = TempPins::new("kind-position");
    let mut browse = TestBrowse::default();
    browse.seed_sources(vec![a_source("mac-mini", "", true)]);
    browse.append_sections(
        0,
        vec![
            (1, "Movies".into(), SecKind::Movie),
            (2, "TV Shows".into(), SecKind::Show),
            (3, "Animes".into(), SecKind::Show),
        ],
    );
    assert_eq!(
        browse.tab_count(),
        2,
        "still two pills — the second TV library folds onto the one it shares a type with, \
         which is the whole shape of the report"
    );
    assert_eq!(
        browse.kind_position(1),
        Some((1, 2)),
        "the TV library the pill opens on is the FIRST of two"
    );
    assert_eq!(
        browse.kind_position(2),
        Some((2, 2)),
        "…and the reported one the second"
    );
    assert_eq!(
        browse.kind_position(0),
        None,
        "the lone film library has no position: `1 of 1` would advertise a choice that does \
         not exist"
    );

    // …and the count is a promise about the list the head OPENS, so it follows the same
    // favourite filter that list does rather than the grant.
    set_pinned_for_owner_test(&mut browse.state, 2, false);
    assert_eq!(
        browse.kind_position(1),
        None,
        "with its sibling switched off there is nowhere else to go, and nothing to count"
    );
    assert_eq!(
        browse.source_rows().len(),
        1,
        "the panel agrees — one row, so a `1 of 2` beside it would have been a lie"
    );
}
/// **The scope both the head's row and the panel it opens now share.** Two surfaces read this:
/// the owned Library row and its Sources menu behind `+N`. Neither may use
/// [`source_rows`], which is scoped through `cur_kind()` and therefore lags a tab press by the
/// length of the page fade — the row drew the MOVIE libraries under a *TV Shows* tab and kept
/// them (its cache key is the viewed section, which does not move again at the commit), and the
/// popover listed the other type's libraries under a row naming this one.
///
/// It is graded here rather than at either call site because both of those go through text
/// measurement — `crate::text` is SDL2_ttf, which the host test build does not link — so this
/// is the layer at which the shared decision is reachable at all.
#[test]
fn the_rows_a_head_offers_follow_the_viewed_librarys_type_not_the_current_one() {
    let _g = crate::testlock::serial();
    let _t = TempPins::new("rows-for-section");
    let mut browse = TestBrowse::default();
    browse.seed_sources(vec![a_source("mac-mini", "", true)]);
    browse.append_sections(
        0,
        vec![
            (1, "Movies".into(), SecKind::Movie),
            (2, "Films".into(), SecKind::Movie),
            (3, "TV Shows".into(), SecKind::Show),
            (4, "Animes".into(), SecKind::Show),
        ],
    );
    // browsing a FILM library, asking about a SHOW one — the mid-fade shape exactly
    browse.state.set_cur(0);
    let rows = browse.state.source_rows_for(3);
    let shows: Vec<&str> = rows.iter().map(|r| r.title.as_str()).collect();
    assert_eq!(
        shows,
        vec!["TV Shows", "Animes"],
        "the rows must follow the section asked about, not `cur()`"
    );
    assert_eq!(
        browse.source_rows().len(),
        2,
        "…while `source_rows` answers for the films, which is what made it the wrong call"
    );

    // …and it is the FAVOURITE filter too, so the count on a head can never promise a row the
    // panel behind it will not draw.
    set_pinned_for_owner_test(&mut browse.state, 3, false); // "Animes"
    assert_eq!(
        browse.state.source_rows_for(3).len(),
        1,
        "a switched-off library draws no row here either"
    );
    assert_eq!(
        browse.kind_position(3),
        None,
        "…and so there is nothing left to count"
    );
}
/// The position is read of the VIEWED library, which during a page fade is not `cur()` — so it
/// may not be derived from the current section's kind the way [`source_rows`] is.
#[test]
fn a_position_is_scoped_to_the_type_of_the_library_it_is_asked_about() {
    let _g = crate::testlock::serial();
    let _t = TempPins::new("kind-position-scope");
    let mut browse = TestBrowse::default();
    browse.seed_sources(vec![a_source("mac-mini", "", true)]);
    browse.append_sections(
        0,
        vec![
            (1, "Movies".into(), SecKind::Movie),
            (2, "Films".into(), SecKind::Movie),
            (3, "TV Shows".into(), SecKind::Show),
        ],
    );
    browse.state.set_cur(2); // browsing the SHOW library…
    assert_eq!(
        browse.kind_position(0),
        Some((1, 2)),
        "…and a film library still counts against the films, not against what is on screen"
    );
    assert_eq!(
        browse.kind_position(2),
        None,
        "the lone show library, from the same call"
    );
}
/// **Issue #68's second half, reported by the person who hit it.** Two TV libraries on one
/// server, the pill opening the one they did not want — so they did the obviously right thing
/// and switched the other OFF in *Favorite libraries*. Nothing changed: "even if I disable my
/// Anime library in the settings, only my Animes library is populated under TV Shows".
///
/// They were not wrong about the control. In the shipped build the favourite switch governed
/// HOME alone and `tab_section` filtered on `s.kind` and nothing else, so a pill resolved to
/// the first library of its type whether or not the user had just told the app to stop showing
/// it. That is worse than the missing switcher beside it: the one workaround the UI offered was
/// correct, and the app ignored it.
#[test]
fn switching_a_librarys_favourite_off_repoints_its_tab_at_the_one_that_is_left() {
    let _g = crate::testlock::serial();
    let _t = TempPins::new("tab-follows-favourite");
    let mut browse = TestBrowse::default();
    browse.seed_sources(vec![a_source("mac-mini", "", true)]);
    // The reporter's shape, in their order: the pill lands on the library they were trying to
    // get away from, because table order is the server's and nothing else.
    browse.append_sections(
        0,
        vec![
            (1, "Animes".into(), SecKind::Show),
            (2, "TV Shows".into(), SecKind::Show),
        ],
    );
    let shows = browse.state.tab_of_kind(SecKind::Show).expect("the type has a pill");
    assert_eq!(
        browse.state.tab_section(shows),
        Some(0),
        "the pill opens the first of the two — which is the complaint, not the bug"
    );

    // …and now the switch they actually reached for.
    set_pinned_for_owner_test(&mut browse.state, 0, false);

    assert_eq!(
        browse.state.tab_section(shows),
        Some(1),
        "with Animes switched off the TV Shows pill must open the library that is left"
    );
    assert!(
        browse.state.tab_has_favorite(SecKind::Show),
        "…and it keeps its pill: one of the two is still on"
    );
}
#[test]
fn switching_off_a_types_last_favourite_takes_its_pill_and_renumbers_the_rest() {
    let _g = crate::testlock::serial();
    // The strip reads the favourite set, and the favourite set is resolved against the
    // RECORDED per-profile answer — so this test needs a session of its own, or it
    // grades whatever the host machine happens to have on disk.
    let _t = TempPins::new("strip-reshape");
    let mut browse = TestBrowse::default();
    browse.seed_sources(vec![a_source("mac-mini", "", true)]);
    browse.append_sections(
        0,
        vec![
            (1, "Movies".into(), SecKind::Movie),
            (2, "TV Shows".into(), SecKind::Show),
        ],
    );
    assert_eq!(browse.tab_count(), 2, "both types have a favourite to start with");
    assert_eq!(browse.state.tab_of_kind(SecKind::Show), Some(1));

    // …and this profile has since switched its film library off.
    set_pinned_for_owner_test(&mut browse.state, 0, false);

    assert_eq!(
        browse.tab_count(),
        1,
        "the type with no favourite left draws no pill"
    );
    assert_eq!(browse.tab_title(0), "TV Shows");
    assert_eq!(
        browse.state.tab_of_kind(SecKind::Movie),
        None,
        "a switched-off type has no position at all — not position 0"
    );
    assert_eq!(
        browse.state.tab_of_kind(SecKind::Show),
        Some(0),
        "…and the type that remains has MOVED, which is the whole hazard"
    );
    assert_eq!(
        browse.state.tab_section(0),
        Some(1),
        "the surviving pill opens the surviving library"
    );
}
/// **A pill is a TYPE, never a person.** The strip names your own libraries; a friend's film
/// library gets no pill of its own (the pill's own label says whose), but a type only they
/// have does — otherwise that content is unreachable from the strip at all. And the selection
/// capsule for a borrowed library rests on its TYPE's pill, so nothing is ever homeless.
#[test]
fn the_tab_strip_grows_by_types_and_never_by_people() {
    let _g = crate::testlock::serial();
    // The strip reads the favourite set, and the favourite set is resolved against the
    // RECORDED per-profile answer — so this test needs a session of its own, or it
    // grades whatever the host machine happens to have on disk.
    let _t = TempPins::new("strip-types");
    let mut browse = TestBrowse::default();
    browse.seed_sources(vec![
        a_source("mac-mini", "", true),
        a_source("nas-home", "friend", true),
    ]);
    browse.append_sections(0, vec![(1, "Movies".into(), SecKind::Movie)]);
    browse.append_sections(
        1,
        vec![
            (1, "Film Club".into(), SecKind::Movie),
            (2, "Their Shows".into(), SecKind::Show),
        ],
    );

    assert_eq!(
        browse.tab_count(),
        2,
        "your Movies, plus the shows nobody of yours provides"
    );
    assert_eq!((browse.tab_title(0), browse.tab_title(1)), ("Movies", "TV Shows"));
    assert_eq!(browse.state.tab_section(1), Some(2));
    assert_eq!(
        browse.tab_of_section(1),
        Some(0),
        "their films ride YOUR Movies pill — same type, one level"
    );
    assert_eq!(
        browse.tab_of_section(2),
        Some(1),
        "their shows have a pill of their own"
    );
}
/// The case a BOOLEAN type could not express, and the reason [`SecKind`] exists: "does an owned
/// library have this kind" has to be asked of a real type, or a friend's library of a type you
/// do not own rides one of your pills and nothing in it is reachable from the strip.
///
/// Stated here with an owner who has ONLY films and a friend who also shares shows. It used to
/// be stated with a friend's MUSIC library, which read better — the two servers differed by a
/// type neither could be confused for — but music is no longer a type this product has a level
/// for, and a test may not be the last place a deleted feature survives.
#[test]
fn a_friends_library_of_a_type_you_do_not_own_gets_its_own_pill() {
    let _g = crate::testlock::serial();
    // The strip reads the favourite set, and the favourite set is resolved against the
    // RECORDED per-profile answer — so this test needs a session of its own, or it
    // grades whatever the host machine happens to have on disk.
    let _t = TempPins::new("strip-missing-type");
    let mut browse = TestBrowse::default();
    browse.seed_sources(vec![
        a_source("mac-mini", "", true),
        a_source("nas-home", "friend", true),
    ]);
    browse.append_sections(0, vec![(1, "Movies".into(), SecKind::Movie)]); // we own films and nothing else
    browse.append_sections(
        1,
        vec![
            (1, "Film Club".into(), SecKind::Movie),
            (2, "Their Shows".into(), SecKind::Show),
        ],
    );

    assert_eq!(
        browse.tab_count(),
        2,
        "your films, plus the shows nobody of yours provides"
    );
    assert_eq!(browse.tab_title(1), "TV Shows");
    assert_eq!(
        browse.tab_of_section(2),
        Some(1),
        "their shows are their own pill, NOT your Movies one"
    );
    assert_eq!(
        browse.tab_of_section(1),
        Some(0),
        "…while their films still ride yours"
    );
    // the wire types this product has a level for — and the ones it deliberately does not, which
    // is what keeps an unplayable library out of the strip, the Sources panel and the grid at once
    assert_eq!(SecKind::from_wire("movie"), Some(SecKind::Movie));
    assert_eq!(SecKind::from_wire("show"), Some(SecKind::Show));
    assert_eq!(
        SecKind::from_wire("artist"),
        None,
        "music has no level below the grid: no pill"
    );
    assert_eq!(SecKind::from_wire("photo"), None);
    assert_eq!(
        SecKind::from_wire("mixed"),
        None,
        "a type with no level is still refused"
    );
}
/// **The deliverable, as an assertion**: the strip does not grow by PEOPLE. Its pill list —
/// and therefore its width, which is a pure function of the labels — does not move as the
/// roster grows from one server to three, because every borrowed library folds onto the pill of
/// a type you already have. Only a type gaining its FIRST favourite library may widen it, which
/// is the second half below.
///
/// The shape the design rejected is the control: a pill per section reaches eleven pills here,
/// which is what measured 2133px against a 1540px track at three friends.
#[test]
fn the_strip_is_the_same_row_at_one_friend_and_at_three() {
    let _g = crate::testlock::serial();
    // The strip reads the favourite set, and the favourite set is resolved against the
    // RECORDED per-profile answer — so this test needs a session of its own, or it
    // grades whatever the host machine happens to have on disk.
    let _t = TempPins::new("strip-width");
    let mut browse = TestBrowse::default();
    browse.seed_sources(vec![
        a_source("mac-mini", "", true),
        a_source("nas-home", "friend", true),
        a_source("nas-home", "friend", true),
        a_source("nas-home", "friend", true),
    ]);
    // OWNED, deliberately: `tab_title` hands back a `&'static str` borrowed out of the section
    // table's own `String`s, and `append_sections` can reallocate that Vec — so a row captured
    // as borrows and compared after the next source lands is reading freed memory. Every
    // caller in the app consumes these inside one frame with no append in between, which is
    // what makes the signature sound in the product and unsound in a test that spans landings.
    let row = |browse: &TestBrowse| {
        (0..browse.tab_count())
            .map(|tab| browse.tab_title(tab).to_string())
            .collect::<Vec<_>>()
    };
    // We own FILMS and nothing else. The owner used to hold both types here, which made the
    // second half of this test need a third type (music) to have anything left over; with the
    // product's list down to two, the un-owned type has to be one of them.
    browse.append_sections(0, vec![(1, "Movies".into(), SecKind::Movie)]);
    let alone = row(&browse);
    assert_eq!(
        alone,
        vec!["Movies"],
        "a type with no favourite library draws no pill: we hold no shows yet"
    );

    for src in 1..=3 {
        browse.append_sections(
            src,
            vec![
                (1, "Film Club".into(), SecKind::Movie),
                (3, "Film Club".into(), SecKind::Movie),
            ],
        );
        assert_eq!(
            row(&browse),
            alone,
            "source {src} added a pill — the strip must not grow by people"
        );
    }
    assert_eq!(browse.section_count(), 7, "seven libraries…");
    assert_eq!(browse.tab_count(), 1, "…and still the one pill they all fold onto");

    // …and a type NOBODY owns grows the row by exactly one however many people share it. Every
    // fixture above is a type we own, which is why this half needs saying separately: it is the
    // only branch of the projection that can admit a borrowed library at all — a shared library
    // of a type you have none of defaults ON (`pins::default_on`), so it really does arrive as
    // a new pill rather than as a switched-off one nobody can reach.
    for src in 1..=3 {
        browse.append_sections(src, vec![(9, "Their Shows".into(), SecKind::Show)]);
    }
    assert_eq!(
        browse.tab_count(),
        2,
        "three friends sharing shows are ONE TV Shows pill"
    );
    assert_eq!(row(&browse).len(), 2);
}
/// A profile switch must not leave the previous account's pills on screen — and now that the
/// strip is a projection of the FAVOURITE set rather than a permanent two, that is a statement
/// about the pills themselves and not only about a cache. `reset()` empties the table, so the
/// row it projects is EMPTY until the new account's own libraries are discovered; the pills
/// then come back one type at a time as they land.
///
/// The generation is what the tab row's label cache keys on, so it has to move across the
/// reset too — otherwise `draw_tab_row` (which iterates the cache, not the live table) would go
/// on drawing and hit-testing libraries the new user cannot open until some later landing
/// happened to change the row.
#[test]
fn a_profile_switch_re_measures_the_strip_instead_of_keeping_the_last_accounts_pills() {
    let _g = crate::testlock::serial();
    // The strip reads the favourite set, and the favourite set is resolved against the
    // RECORDED per-profile answer — so this test needs a session of its own, or it
    // grades whatever the host machine happens to have on disk.
    let _t = TempPins::new("strip-profile");
    let mut browse = TestBrowse::default();
    browse.seed_sources(vec![a_source("mac-mini", "", true)]);
    browse.append_sections(
        0,
        vec![
            (1, "Movies".into(), SecKind::Movie),
            (2, "TV Shows".into(), SecKind::Show),
        ],
    );
    assert_eq!(browse.tab_count(), 2);
    let before = browse.state.tabs_gen();

    browse.reset(); // install_pms: a different account signs in
    assert_eq!(
        browse.tab_count(),
        0,
        "the previous account's pills are gone, not inherited"
    );
    assert_ne!(
        browse.state.tabs_gen(),
        before,
        "…and the row's generation moved, so the label cache cannot serve them"
    );

    // the new account's own libraries land and the row is rebuilt from THEM
    browse.seed_sources(vec![a_source("nas-home", "", true)]);
    browse.append_sections(0, vec![(4, "Films".into(), SecKind::Movie)]);
    assert_eq!((browse.tab_count(), browse.tab_title(0)), (1, "Movies"));
}
/// The strip's own generation moves when the ROW changes and not when the TABLE does — which,
/// once a table is appended to one source at a time, are different questions. Every borrowed
/// library that folds onto a pill you already have bumps the table's generation and changes
/// nothing in the row, so keying the label + width cache on the table re-measured every pill in
/// the strip once per source, on Home's hot path, for a strip that had not moved.
#[test]
fn only_a_changed_row_costs_the_tab_cache_a_re_measure() {
    let _g = crate::testlock::serial();
    // The strip reads the favourite set, and the favourite set is resolved against the
    // RECORDED per-profile answer — so this test needs a session of its own, or it
    // grades whatever the host machine happens to have on disk.
    let _t = TempPins::new("strip-cache");
    let mut browse = TestBrowse::default();
    browse.seed_sources(vec![
        a_source("mac-mini", "", true),
        a_source("nas-home", "friend", true),
        a_source("nas-home", "friend", true),
    ]);
    browse.append_sections(0, vec![(1, "Movies".into(), SecKind::Movie)]);
    let (g0, table0) = (browse.state.tabs_gen(), browse.state.sections_gen());

    // two friends' film libraries land: both fold onto your Movies pill
    browse.append_sections(1, vec![(1, "Film Club".into(), SecKind::Movie)]);
    browse.append_sections(2, vec![(1, "Film Club".into(), SecKind::Movie)]);
    assert_ne!(
        browse.state.sections_gen(),
        table0,
        "the TABLE's generation moved, twice"
    );
    assert_eq!(
        browse.state.tabs_gen(),
        g0,
        "…and the row did not, so it must not re-measure"
    );

    // …and the other direction, which is the half that makes the generation worth having: a
    // type gaining its FIRST favourite library really does reshape the row — a new pill — so
    // this landing MUST cost a re-measure where the three above must not.
    browse.append_sections(2, vec![(9, "Their Shows".into(), SecKind::Show)]);
    assert_ne!(
        browse.state.tabs_gen(),
        g0,
        "a new pill appeared: the cached labels and widths are stale"
    );
    let g1 = browse.state.tabs_gen();
    browse.append_sections(2, vec![(10, "More Shows".into(), SecKind::Show)]);
    assert_eq!(
        browse.state.tabs_gen(),
        g1,
        "…and the next one folds onto it again, costing nothing"
    );
}
/// **The Sources panel cannot switch tabs, because it is scoped to the tab's own TYPE.**
///
/// Owner-reported on the device build: picking a library in the Sources panel could land on one
/// of a different type, which moves the selected section — and the tab is derived from the
/// section's kind, so a toolbar control silently navigated the row above it.
///
/// The scope is the fix, not a guard on the press: every row the panel offers is of the browsed
/// type, so no reachable press can change the tab. Both servers' films appear together; neither
/// server's shows do.
#[test]
fn the_sources_panel_offers_only_libraries_of_the_tab_being_browsed() {
    let _g = crate::testlock::serial();
    // The strip reads the favourite set, and the favourite set is resolved against the
    // RECORDED per-profile answer — so this test needs a session of its own, or it
    // grades whatever the host machine happens to have on disk.
    let _t = TempPins::new("picker-scope");
    let mut browse = TestBrowse::default();
    browse.seed_sources(vec![
        a_source("mac-mini", "", true),
        a_source("nas-home", "friend", true),
    ]);
    browse.append_sections(
        0,
        vec![
            (1, "Movies".into(), SecKind::Movie),
            (2, "TV Shows".into(), SecKind::Show),
        ],
    );
    browse.append_sections(
        1,
        vec![
            (1, "Film Club".into(), SecKind::Movie),
            (2, "Their Shows".into(), SecKind::Show),
        ],
    );
    // The friend's two libraries are types we own, so `pins::default_on` starts them OFF and
    // the picker — which is favourite-scoped now — would not offer them at all. Favourite them
    // explicitly: what is under test here is the TYPE scope, and it has to be graded on a
    // roster where both servers have something to contribute to each tab.
    set_pinned_for_owner_test(&mut browse.state, 2, true);
    set_pinned_for_owner_test(&mut browse.state, 3, true);

    // browsing a FILM library: both servers' film libraries, and no show library from either
    browse.state.set_cur(0);
    let films: Vec<String> = browse.source_rows().iter().map(|r| r.title.clone()).collect();
    assert_eq!(
        films,
        vec!["Movies", "Film Club"],
        "both servers' films, nothing else: {films:?}"
    );

    // …and the same panel on the shows tab is the other list entirely
    browse.state.set_cur(1);
    let shows: Vec<String> = browse.source_rows().iter().map(|r| r.title.clone()).collect();
    assert_eq!(
        shows,
        vec!["TV Shows", "Their Shows"],
        "both servers' shows: {shows:?}"
    );

    // …and the picker is FAVOURITE-scoped as well as type-scoped: switching one off takes it
    // out of the list, which is how a non-favourite library stops being reachable from here.
    // Settings' own editor (`all_source_rows`) is the unscoped list that brings it back.
    set_pinned_for_owner_test(&mut browse.state, 3, false);
    let shows: Vec<String> = browse.source_rows().iter().map(|r| r.title.clone()).collect();
    assert_eq!(shows, vec!["TV Shows"], "a non-favourite is not offered");
    assert!(
        browse.state.all_source_rows().iter().any(|r| r.title == "Their Shows"),
        "…but Settings still lists it, or it could never come back"
    );
    set_pinned_for_owner_test(&mut browse.state, 3, true);

    // the decisive property: every row the panel can activate keeps the browsed TYPE, so the
    // tab derived from it cannot move. Stated over the section each row opens, not its title.
    for r in browse.source_rows() {
        assert_eq!(
            browse.state.sections()[r.section].kind,
            SecKind::Show,
            "a row of another type is reachable"
        );
    }
}
/// **The tab destination, on the roster a managed profile actually gets.**
///
/// Two Movies libraries, one on the household's own server and one on a friend's share, both
/// pinned, neither remembered — the tie [`BrowseState::section_of_kind`] breaks. The friend's is
/// seeded FIRST on purpose: under the old `!owned` tiebreak every row of this roster scored the
/// same (plex.tv says `owned:false` about the family server too), so `min_by_key` kept the first
/// it met and the Movies tab opened on a stranger's shelf, permanently, with no control anywhere
/// that would move it. That is GitHub #68's mechanism — *"I have two TV Shows libraries … only my
/// Animes are being displayed"* — still open for every profile but the admin's.
#[test]
fn the_movies_tab_prefers_the_households_library_over_a_friends() {
    let _g = crate::testlock::serial();
    let t = TempPins::new("tab-household");
    t.watching("u-managed");
    let mut browse = TestBrowse::default();
    // the friend's server first in the table, so arrival order and the right answer disagree
    browse.seed_sources(vec![
        a_source("nas-home", "friend", true),
        a_household_source("mac-mini"),
    ]);
    browse.append_sections(0, vec![(1, "Film Club".into(), SecKind::Movie)]);
    browse.append_sections(1, vec![(1, "Movies".into(), SecKind::Movie)]);

    // both are favourites, or the tiebreak is not what is being graded. The friend's is turned on
    // deliberately — a recorded answer, so the re-resolve below cannot take it back off.
    assert!(browse.pinned(1), "the household's films default On");
    assert!(browse.state.toggle_pin(0), "…and the friend's is switched on");

    let tab = browse.state.tab_of_kind(SecKind::Movie).expect("a Movies pill");
    assert_eq!(
        browse.state.tab_section(tab).map(|s| browse.section_title(s)),
        Some("Movies"),
        "the tab lands on the household's library, not on whichever answered first"
    );

    // …and a REMEMBERED choice still wins outright: the tiebreak is only ever consulted when the
    // profile has not already said. Nothing about the household may second-guess that.
    crate::plex::session::update(|session| {
        let mut next = session.clone();
        let user = crate::plex::session::current_profile_key();
        next.last_library.retain(|l| l.user != user);
        let mut libs = crate::plex::session::LastLibrary { user, ..Default::default() };
        libs.libs.push(crate::plex::session::TypedLib {
            kind: SecKind::Movie.wire().to_string(),
            machine_id: "nas-home".into(),
            key: 1,
            extensions: Default::default(),
        });
        next.last_library.push(libs);
        Some(next)
    });
    browse.state.resolve_pins();
    assert_eq!(
        browse.state.tab_section(tab).map(|s| browse.section_title(s)),
        Some("Film Club"),
        "the library this profile chose, household or not"
    );
}

/// **`/api/v2/home/users` landing on its own re-resolves the whole pin table.**
///
/// The roster is the one input to the household verdict that is not in the server registry, and it
/// arrives on its own schedule — a managed profile's first boot routinely computes its defaults
/// before it lands. Nothing used to notice: `discovery_needs_pump` read the registry and never the
/// session, so a roster that changed no `ServerFacts` never reached the sync those defaults are
/// derived in, and the misgrading stood until something unrelated happened to move a fact.
///
/// The test drives the real gate, not just the sync: the other pump reasons are quieted first, so
/// `discovery_needs_pump` answering `true` can only be the roster.
#[test]
fn a_home_roster_arriving_late_re_resolves_the_pin_table() {
    struct Fresh(#[allow(dead_code)] crate::testlock::Serial);
    impl Drop for Fresh {
        fn drop(&mut self) {
            crate::plex::reset_servers_for_test();
        }
    }
    let _g = Fresh(crate::testlock::serial());
    let t = TempPins::new("late-roster");
    t.watching("u-managed");
    crate::plex::reset_servers_for_test();

    // Both grants arrive `owned:false` — the family server included, which is what plex.tv tells
    // a managed profile. `home:false` on both, because this account is a Plex Home admin's and
    // that flag was measured `false` on every grant it has; `ownerId` is the whole signal.
    let house = crate::plex::register_for_test("mac-mini", "127.0.0.1", 41001, "tok", "cid");
    let friend = crate::plex::register_for_test("nas-home", "127.0.0.1", 41002, "tok", "cid");
    let grant = |owner_id| crate::plex::GrantEvidence { owned: false, home: false, owner_id };
    crate::plex::describe_server(house, "Mac mini", "", grant(ADMIN_ID));
    crate::plex::describe_server(friend, "nas-home", "friend", grant(ADMIN_ID + 1));

    let mut browse = TestBrowse::default();
    browse.sync_roster();
    browse.append_sections(
        0,
        vec![
            (1, "Movies".into(), SecKind::Movie),
            (2, "TV Shows".into(), SecKind::Show),
        ],
    );
    browse.append_sections(1, vec![(1, "Film Club".into(), SecKind::Movie)]);

    // Before the roster: the house cannot be told from the share, so nothing is the household's,
    // no type is the household's either, and EVERY library defaults On — the friend's films on
    // the family's front door, unasked.
    assert_eq!(
        (browse.pinned(0), browse.pinned(1), browse.pinned(2)),
        (true, true, true),
        "an un-enumerable house grades every grant as an outsider's"
    );

    // The viewer answers ONE row: the household's TV shows, off. Everything else is a default.
    assert!(browse.state.toggle_pin(1));

    // Quiet every other reason the discovery pump has to run, so the gate below can only be
    // answering the roster.
    for index in 0..2 {
        let source = browse.state.source_mut(index).expect("a synced source");
        source.sections_done = true;
        source.counts_done = true;
    }
    assert!(
        !browse.state.discovery_needs_pump(&browse.adapter),
        "nothing is owed before the roster lands, or this proves nothing"
    );

    // `/api/v2/home/users` lands: the admin and the managed user, and no zeroes.
    let member = |id| crate::plex::session::HomeUserRef { id, ..Default::default() };
    assert!(crate::plex::session::update(|session| {
        let mut next = session.clone();
        next.home_users = vec![member(ADMIN_ID), member(ADMIN_ID + 7)];
        Some(next)
    }));
    assert_eq!(
        crate::plex::session::peek().household_ids(),
        vec![ADMIN_ID, ADMIN_ID + 7],
        "the roster enumerates the house, and carries no zero and no watching-user id"
    );

    assert!(
        browse.state.discovery_needs_pump(&browse.adapter),
        "the roster arrival is itself a reason to sync — the trigger this lane added"
    );
    browse.sync_roster();

    assert_eq!(
        (browse.pinned(0), browse.pinned(1), browse.pinned(2)),
        (true, false, false),
        "the house's films stay On, the friend's films fall to Off — and the one ANSWER stands"
    );
    assert!(
        browse.state.sources()[0].household && !browse.state.sources()[1].household,
        "the sources themselves were reclassified, which is what moved the pins"
    );

    // A recorded answer is not a default and is not corrected: the household's TV shows would
    // default On now, and they are Off because somebody said so.
    let session = crate::plex::session::peek();
    let record = session
        .pins_for(&crate::plex::session::current_profile_key())
        .expect("the answer reached the disk");
    assert_eq!(record.answer("mac-mini", 2), Some(false), "the decision");
    assert_eq!(
        (record.answer("mac-mini", 1), record.answer("nas-home", 1)),
        (None, None),
        "…and the rows nobody moved were never written down, which is what let them be corrected"
    );

    // Idempotent: a second sync against the same roster is not a change.
    assert!(
        !browse.state.discovery_needs_pump(&browse.adapter),
        "the correction settles rather than re-firing every frame"
    );
}
