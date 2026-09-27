//! **Which libraries feed Home — the rules, pure.**
//!
//! The store is [`browse`](crate::browse)'s section table (in memory, indexed by the app) and
//! [`Session::home_pins`](super::session::Session::home_pins) (on disk, keyed by profile). This
//! module is neither: it is the policy between them, kept free of both so every rule below is
//! graded by `cargo test --lib` rather than observed on a television.
//!
//! `browse.rs` referred to a `pin::resolve` for a week before one existed — the comment describing
//! this file was written when the pin default was hard-wired to `true` because deliverable F (the
//! first-run route) had nowhere to ask the question. It has one now, and these are its rules:
//!
//! * **Your HOUSEHOLD's libraries arrive On, a friend's arrive Off.** The point of asking is not
//!   to put a stranger's shelves on your front door unannounced (`Shared Sources.dc.html` §F).
//!   The household and not the account, because plex.tv answers a Plex Home managed profile with
//!   `owned:false` on the family's own server — so a rule written on raw ownership gave the
//!   household's own shelves a stranger's defaults for every profile but the admin's. The verdict
//!   is [`crate::plex::is_household`]'s and arrives here as a bool; see [`LibRef::household`].
//! * **An account with no server of its OWN HOUSEHOLD is not a special case.** Every source it has
//!   is borrowed, so "a friend's arrives Off" would open the app on nothing at all; with nothing of
//!   the household's to prefer, a borrowed library is simply a library.
//! * **A recorded answer beats the default**, in both directions — which is why
//!   [`HomePins`](super::session::HomePins) records the Offs too. A library nobody was asked about
//!   (a share that answered late, one the owner created since) lands on its default instead of
//!   silently Off, and that is what makes "a share arriving later does not reopen this screen"
//!   honest rather than merely quiet.
//! * **Only an answer somebody GAVE is written down.** [`record`] takes one `Option` per library
//!   and skips the `None`s, so a default nobody chose keeps re-deriving instead of being frozen as
//!   if it were a decision — see [`answers`], which is the rule for which is which. Which rows
//!   those are is REPORTED by the editor that holds the draft (`BrowseCmd::ApplyPins`), never
//!   inferred from the values: an answer is still an answer when the default it was given against
//!   has since moved onto the same value.
//! * **Home is never empty.** The one state the control genuinely has no answer for, and the reason
//!   the Home editor's draft refuses to unpin the last library (`screens::onboard`'s
//!   `OnboardScreen::toggle_row`; the
//!   test-only `browse::toggle_pin` keeps the same floor). The same floor is applied here,
//!   because a *recorded* selection can be emptied without any toggle at all: unpin your own
//!   libraries in favour of a friend's, then lose the friend from the roster.
use super::session::{HomePins, PinnedLib};

/// One library as the rules see it — the join key plus the one fact the default turns on.
///
/// Borrowed rather than owned strings: the caller (`browse::resolve_pins`) already holds the
/// section table and this is re-derived on every append, so a `String` per library per landing
/// would be an allocation for nothing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct LibRef<'a> {
    /// the server's `machineIdentifier` — empty while nobody has learned it, which is a library
    /// that cannot be *recorded* (see [`record`]) though it still resolves and still draws
    pub(crate) machine_id: &'a str,
    /// the section key, server-local: both servers in the measured pair have a section `1`
    pub(crate) key: i64,
    /// **This HOUSEHOLD's server rather than an outsider's** — half of the default's input, and
    /// [`crate::plex::is_household`]'s verdict rather than plex.tv's raw `owned`.
    ///
    /// It was `owned` until the managed-profile fix, and the rename is the fix: a Plex Home
    /// managed profile is told `owned:false` about the family server it watches every day, so the
    /// old field put a stranger's defaults on the household's own shelves — and, because
    /// *everything* was then unowned, put a genuine friend's share on Home beside them. A `bool`
    /// and not the evidence, so these rules stay a leaf: the grant, the `home` flag and the Home
    /// roster are all `plex::servers`' business, and `browse::lib_refs` hands down the answer.
    pub(crate) household: bool,
    /// **Does this HOUSEHOLD have any library of THIS library's type?** The other half, and the
    /// reason the default is per-type rather than per-roster since 2026-09-05.
    ///
    /// A `bool` rather than the type itself so these rules stay a leaf: `SecKind` lives in
    /// `browse`, which sits *above* this layer, and the rule never needs to know which type a
    /// library is — only whether the household already has one of its own. `browse::lib_refs`
    /// computes it, because the section table is the only thing that can.
    pub(crate) household_type: bool,
}

/// What the first-run screen SHOWS for one library before anybody touches it.
///
/// **Your household's, and anything of a type the household has none of.** With no server of the
/// household's there is nothing to prefer, so everything comes On — which this still says, because
/// a household that has nothing has nothing of every type.
///
/// The per-type half arrived with the favourite switch on 2026-09-05, and it is a correctness fix
/// rather than a nicety. The switch governs the tab strip now, so a library that is off draws no
/// pill — and under the old roster-wide rule, a friend sharing a type you do not own (they have TV,
/// you have only films) defaulted to Off and took the *entire type* off the strip with it. A
/// content type silently absent on first boot is not a preference anybody expressed; it is a
/// default nobody chose, and it is exactly the "tab that leads to nothing" argument running
/// backwards. `Library Screens.dc.html` B says the strip grows by missing types, and this is what
/// makes that true at the default rather than only after a visit to Settings.
///
/// Your household's libraries are unaffected, and so is the case the rule was written for: a
/// friend's films still default Off while the household has films of its own.
pub(crate) fn default_on(lib: LibRef<'_>) -> bool {
    lib.household || !lib.household_type
}

/// Resolve the whole table for one profile: the recorded answer where there is one, the default
/// where there is not, and the never-empty floor over the lot.
///
/// **A whole-table function on purpose.** Two of its callers arrive holding one new row, and the
/// floor still cannot be decided per row — "is anything on?" is a question about the table. The
/// third is the one that makes the shape load-bearing rather than merely convenient: a Plex Home
/// roster landing reclassifies SOURCES, and a source changing sides moves `household_type` for
/// every library of its kind on every other source at once, so there is no such thing as
/// re-resolving the row that changed.
///
/// The fourth is the editor's own commit (`browse::apply_pins`), and it is the floor's case rather
/// than the classification's: a commit that records only the rows somebody answered leaves every
/// other row living on a default — including one the floor had RAISED, whose reason to be raised
/// the new record may well have just removed. Re-running this is how what the table SHOWS after a
/// commit stays what the commit SAVED.
///
/// Re-running it over rows that already have an answer is free: a recorded row resolves to its
/// record, unconditionally and in both directions. Only the rows nobody answered about move —
/// which is exactly the property that lets a late roster correct a default it arrived too late to
/// inform, without being able to overrule a decision somebody made.
pub(crate) fn resolve(libs: &[LibRef<'_>], rec: Option<&HomePins>) -> Vec<bool> {
    let mut out: Vec<bool> = libs
        .iter()
        .map(|&l| {
            rec.and_then(|r| r.answer(l.machine_id, l.key))
                .unwrap_or_else(|| default_on(l))
        })
        .collect();
    if out.iter().any(|&on| on) {
        return out;
    }
    // Nothing feeds Home. Fall back to the FIRST source's libraries — the roster is registered
    // ours-first (`auth::registration_order`), so that is your own server whenever you have one.
    // Absence here is not a preference anybody expressed; it is a selection that outlived the
    // server it named, and a front door with nothing on it is not a state this app has.
    //
    // **Keyed on the machine id, but ONLY when there is one.** `record` refuses to write down an
    // empty `machineIdentifier`, so a source nobody has learned one for yet carries `""` — and
    // `l.machine_id == first` is then `"" == ""`, true for every nameless library on every
    // registered server at once. Home would come up fed by an arbitrary mixture of your server
    // and a friend's, which is not "the FIRST source's libraries" and is not a state anyone
    // chose. With no id to group by, the honest floor is the first library itself.
    match libs.first().map(|l| l.machine_id) {
        Some(first) if !first.is_empty() => {
            for (i, l) in libs.iter().enumerate() {
                out[i] = l.machine_id == first;
            }
        }
        Some(_) => out[0] = true,
        None => {}
    }
    out
}

/// Does the first-run route have a question for this profile?
///
/// **Two conditions, and both are the design's.** More than one source, because a single-server
/// install would meet a screen with one row and no decision in it — 90% of installs, and the
/// rejected alternative the canvas names. And never twice: a first-run screen that comes back is
/// not a first-run screen.
///
/// It counts SOURCES, not libraries: the question is whose shelves reach your Home, and a second
/// library on your own server is not a second answer to it.
pub(crate) fn asks(sources: usize, rec: Option<&HomePins>) -> bool {
    sources > 1 && !rec.is_some_and(|r| r.asked)
}

/// **Which rows of the table are this profile's ANSWER, and which are still only a default.**
///
/// One `Option` per library, for [`record`] to write down or leave alone:
///
/// * a row the viewer **touched** in this editing session records what they left it at;
/// * any other row records whatever this profile had **already** answered about it, if anything;
/// * and a row with neither — a default nobody has ever chosen — records **nothing**, so it keeps
///   re-deriving from [`default_on`] on every resolve.
///
/// **A default nobody chose must not outlive the reason it was chosen.** The editor commits once
/// per session rather than once per toggle, and until this function existed [`record`] froze every
/// row that commit could see — so a default computed *before* the Plex
/// Home roster landed became indistinguishable from a deliberate answer, permanently, with no
/// provenance anywhere to tell them apart. A managed profile's very first boot computes its
/// defaults from `owned:false` on every source it has; if the editor is committed in that window
/// the wrong defaults are the user's answer for good. Leaving them unrecorded is what lets the
/// roster's arrival correct them, and it costs nothing in the other direction, because an
/// unrecorded row resolves to exactly the value it was showing.
///
/// `touched` is indexed like `libs` and may be SHORT (or empty): a missing entry is "not touched",
/// which is what a commit with no edits in it means.
///
/// **`touched` is the editor's own account of which rows it was ANSWERED about, not a comparison
/// anybody made here or in the store.** A row can be answered to the value it already held — the
/// viewer switches a friend's films off while they read On, and a Plex Home roster landing moves
/// the live pin to Off before the commit — and the two are indistinguishable from the values
/// alone. Inferring `touched` from "differs from the live pin" is precisely what dropped such an
/// answer and let a later reclassification raise it again.
pub(crate) fn answers(
    libs: &[LibRef<'_>],
    on: &[bool],
    touched: &[bool],
    prev: Option<&HomePins>,
) -> Vec<Option<bool>> {
    libs.iter()
        .enumerate()
        .map(|(i, l)| {
            if touched.get(i).copied().unwrap_or(false) {
                Some(on.get(i).copied().unwrap_or_else(|| default_on(*l)))
            } else {
                prev.and_then(|r| r.answer(l.machine_id, l.key))
            }
        })
        .collect()
}

/// Write this profile's answers down — **the ones it actually gave**.
///
/// **Both sides**, per [`HomePins`]'s own doc — and only the libraries this build can actually
/// name. A source whose `machineIdentifier` nobody has learned is unaddressable across a restart
/// (the key is the machine, never the roster position, which reshuffles), so recording it would
/// write a row that can only ever match the *other* nameless ones. It keeps its RESOLVED value —
/// which is its default, not the answer, from the moment the commit's own reconcile runs
/// (`browse::apply_pins`) rather than only from the next landing — and is asked about again next
/// boot. Showing an answer that provably cannot be saved is the same disagreement between the
/// screen and the record that reconcile exists to end; it is simply one nothing can record away.
///
/// `answers` is [`answers`]'s output: `Some` is a decision, `None` is a row still living on its
/// default, and a `None` row is absent from BOTH lists — which [`HomePins::answer`] reads back as
/// "never answered for" and [`resolve`] therefore re-derives. A record written by a build before
/// this distinction existed has every row in one list or the other and keeps winning outright;
/// nothing here demotes a stored row back to a default, because this cannot tell one of those from
/// an answer somebody meant.
pub(crate) fn record(
    user: &str,
    asked: bool,
    libs: &[LibRef<'_>],
    answers: &[Option<bool>],
) -> HomePins {
    let mut out = HomePins {
        user: user.to_string(),
        asked,
        on: Vec::new(),
        off: Vec::new(),
        extensions: Default::default(),
    };
    for (i, l) in libs.iter().enumerate() {
        if l.machine_id.is_empty() {
            continue;
        }
        let Some(on) = answers.get(i).copied().flatten() else {
            continue;
        };
        let lib = PinnedLib {
            machine_id: l.machine_id.to_string(),
            key: l.key,
            extensions: Default::default(),
        };
        if on {
            out.on.push(lib);
        } else {
            out.off.push(lib);
        }
    }
    out
}

/// Carry forward the answers this profile gave about servers the table cannot currently SEE.
///
/// [`record`] writes down the answers about rows the section table HOLDS, and that table holds
/// only the sources that have answered — so a friend whose server was asleep when a switch was
/// flipped would have their
/// whole recorded answer replaced by silence, and the ownership default would come back for
/// libraries the user had already decided about. `HomePins` is a whole-profile record and
/// [`Session::set_pins_for`](super::session::Session::set_pins_for) replaces it wholesale, so the
/// merge belongs here rather than being the store's problem.
///
/// **The grain is the MACHINE, not the library.** A server whose libraries the table holds has
/// just been answered about in full — including one it has since lost, which is correctly dropped.
/// A server it does not hold has not been answered about at all, and silence is not an answer.
pub(crate) fn carry_forward(
    mut rec: HomePins,
    prev: Option<&HomePins>,
    libs: &[LibRef<'_>],
) -> HomePins {
    let Some(prev) = prev else { return rec };
    let absent = |p: &&PinnedLib| {
        // an entry with no machine can never be WRITTEN (see [`record`]), so it can never be
        // carried either — and matching it against a table row's empty id would adopt a neighbour's
        !p.machine_id.is_empty() && !libs.iter().any(|l| l.machine_id == p.machine_id)
    };
    rec.on.extend(prev.on.iter().filter(absent).cloned());
    rec.off.extend(prev.off.iter().filter(absent).cloned());
    rec
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A library of a type this HOUSEHOLD also has — the ordinary case, and what every test here
    /// asserted before the per-type rule existed. [`lib_of_new_type`] is its counterpart.
    fn lib(mid: &str, key: i64, household: bool) -> LibRef<'_> {
        LibRef {
            machine_id: mid,
            key,
            household,
            household_type: true,
        }
    }
    /// A borrowed library of a type this household has NONE of — a friend's TV shelf on a
    /// films-only install. It defaults On, and the strip would otherwise lose the whole type.
    fn lib_of_new_type(mid: &str, key: i64) -> LibRef<'_> {
        LibRef {
            machine_id: mid,
            key,
            household: false,
            household_type: false,
        }
    }
    /// The measured pair: your household's two libraries and a friend's one, in registration
    /// order.
    fn roster<'a>() -> Vec<LibRef<'a>> {
        vec![
            lib("mine", 1, true),
            lib("mine", 2, true),
            lib("theirs", 1, false),
        ]
    }
    /// **Every row is an answer somebody gave** — the shape [`record`] took before it could tell a
    /// decision from a default, and what the editor's own commit still produces for the rows the
    /// viewer actually moved. Tests about the DEFAULTS say so with [`answers`] instead.
    fn chosen(on: &[bool]) -> Vec<Option<bool>> {
        on.iter().copied().map(Some).collect()
    }

    /// **The per-type default, and the bug it closes.** The switch governs the tab strip since
    /// 2026-09-05, so a library that is Off draws no pill — and a whole TYPE whose only library is
    /// a friend's would have defaulted Off and vanished from the strip entirely, on first boot,
    /// with nothing on screen to say a decision had been made. That is a default nobody chose, not
    /// a preference anybody expressed.
    #[test]
    fn a_borrowed_library_of_a_type_you_do_not_own_defaults_on() {
        assert!(
            !default_on(lib("theirs", 1, false)),
            "a friend's FILMS still default off while the household has films of its own"
        );
        assert!(
            default_on(lib_of_new_type("theirs", 2)),
            "…but a friend's SHOWS come on when the household has no shows at all, or the type \
             has no pill"
        );
        assert!(
            default_on(lib("mine", 1, true)),
            "the household's own is unaffected, by either half of the rule"
        );
        // …and the roster-wide case the rule was originally written for still holds, because a
        // household with nothing has nothing of every type.
        assert!(
            default_on(lib_of_new_type("theirs", 3)),
            "with no server of the household's there is nothing to prefer"
        );
    }

    /// **The defaults rule, which is the whole first frame of the screen.**
    ///
    /// Unchanged by the managed-profile fix and deliberately so: for the account that owns its
    /// server, `is_household` is raw `owned` (its first clause), so this is the same assertion
    /// about the same install it always was.
    #[test]
    fn your_own_libraries_arrive_on_and_a_friends_arrives_off() {
        assert_eq!(resolve(&roster(), None), vec![true, true, false]);
    }

    /// An account with nothing of its HOUSEHOLD's has nothing to prefer, so the default has no
    /// question to answer. Without this the app would open on an empty Home for exactly the user
    /// who has no other library to fall back on.
    ///
    /// **"Household" and not "own" is the whole of the managed-profile fix seen from here.** The
    /// borrowed-only account this was written for is genuinely rare; the shape it describes is
    /// not, because plex.tv hands a Plex Home managed profile `owned:false` on the family server
    /// too — so under the old rule EVERY managed profile looked like this test, and every library
    /// it could see, a real stranger's included, came On. The mixed roster is
    /// [`a_mixed_household_roster_keeps_a_friends_share_off`] below; this one is now what it says
    /// on the tin, an account with no household server at all.
    #[test]
    fn an_account_with_no_server_of_its_own_gets_every_borrowed_library() {
        // A household with nothing has nothing OF EVERY TYPE, which is why the per-type default
        // subsumes the roster-wide one rather than replacing it.
        let libs = vec![lib_of_new_type("a", 1), lib_of_new_type("b", 1)];
        assert_eq!(resolve(&libs, None), vec![true, true]);
    }

    /// **THE managed-profile case: a household server and a genuine friend's share, both
    /// `owned:false`.**
    ///
    /// This is the roster a Plex Home managed or Guest profile actually gets, and the one the old
    /// rule could not express at all. Reading raw ownership, nothing here is ours, so the
    /// per-type bit was false for every type too and `default_on` answered On for every row — the
    /// friend's films landed on the family's Home beside the family's own, on first boot, with
    /// nothing on screen to say a choice had been made.
    ///
    /// Graded on the household, the four rows separate exactly as they do for the admin: the
    /// house's two On, the friend's FILMS Off because the house has films, and the friend's SHOWS
    /// On because the house has none and the strip would otherwise lose the whole type.
    #[test]
    fn a_mixed_household_roster_keeps_a_friends_share_off() {
        // The family server's Movies, seen by a profile plex.tv tells `owned:false`; the friend's
        // Movies, of a type the house already has; and the friend's Shows, of a type it has none
        // of. All three arrive `owned:false` on the wire — the house's included.
        let house_films = lib("house", 1, true);
        let friend_films = lib("friend", 1, false);
        let friend_shows = lib_of_new_type("friend", 2);
        let libs = vec![house_films, friend_films, friend_shows];
        assert_eq!(
            resolve(&libs, None),
            vec![true, false, true],
            "the household's own arrives On, the friend's FILMS Off, the friend's SHOWS On"
        );

        // …and the same roster as the OLD rule saw it, which is the bug: reading raw ownership,
        // nothing is ours, so no type is ours either and every row comes On — the friend's films
        // on the family's Home, unasked.
        let as_owned_read_it = vec![
            lib_of_new_type("house", 1),
            lib_of_new_type("friend", 1),
            lib_of_new_type("friend", 2),
        ];
        assert_eq!(
            resolve(&as_owned_read_it, None),
            vec![true, true, true],
            "what every managed profile got before the household verdict reached this rule"
        );
    }

    /// **Only the rows the viewer TOUCHED become answers; the rest stay derivable.**
    ///
    /// The editor commits once, for the whole session, so without this every default it could see
    /// was frozen as though it had been chosen — and a managed profile's first boot
    /// computes its defaults before `/api/v2/home/users` can possibly have landed. A default that
    /// outlives the reason it was chosen is not a preference; it is a guess nobody can revise.
    #[test]
    fn only_a_row_the_viewer_moved_is_written_down() {
        let libs = roster();
        // the viewer turned the friend's library on, and touched nothing else
        let touched = [false, false, true];
        let on = [true, true, true];
        let rec = record("u-7", true, &libs, &answers(&libs, &on, &touched, None));
        assert_eq!(
            (rec.answer("theirs", 1), rec.answer("mine", 1), rec.answer("mine", 2)),
            (Some(true), None, None),
            "one decision recorded; two defaults left where the resolve can still re-derive them"
        );
        // the table looks identical either way — an unrecorded row resolves to what it showed
        assert_eq!(resolve(&libs, Some(&rec)), vec![true, true, true]);

        // …and now the household verdict arrives and reclassifies the "mine" server as an
        // outsider's. The untouched rows follow it; the one the viewer answered does not.
        let regraded = vec![
            lib("mine", 1, false),
            lib("mine", 2, false),
            lib("theirs", 1, false),
        ];
        assert_eq!(
            resolve(&regraded, Some(&rec)),
            vec![false, false, true],
            "a late correction reaches the defaults and stops at the decision"
        );
    }

    /// The other half of the same rule: a row the viewer did NOT touch keeps whatever they had
    /// already said about it. Recording is per session, and a session that flips one switch must
    /// not withdraw the answers the previous one gave about the rows it left alone.
    #[test]
    fn an_untouched_row_keeps_the_answer_it_already_had() {
        let libs = roster();
        let first = record("u-7", true, &libs, &chosen(&[false, false, true]));
        // a second session moves only the first row
        let on = [true, false, true];
        let second = record(
            "u-7",
            true,
            &libs,
            &answers(&libs, &on, &[true], Some(&first)),
        );
        assert_eq!(
            (
                second.answer("mine", 1),
                second.answer("mine", 2),
                second.answer("theirs", 1)
            ),
            (Some(true), Some(false), Some(true)),
            "the new decision, and the two the previous session recorded"
        );

        // a commit with NOTHING touched at all — `Start watching` straight off the defaults —
        // records the question as put and invents no answers beyond the ones already there
        let idle = record("u-7", true, &libs, &answers(&libs, &on, &[], Some(&first)));
        assert!(idle.asked, "the profile has been asked, which is its own fact");
        assert_eq!(
            (idle.answer("mine", 1), idle.answer("theirs", 1)),
            (Some(false), Some(true)),
            "…and the earlier answers survive it untouched"
        );
        let fresh = record("u-7", true, &libs, &answers(&libs, &on, &[], None));
        assert!(
            fresh.on.is_empty() && fresh.off.is_empty(),
            "with nothing recorded before and nothing touched now, nothing is written down"
        );
        assert!(fresh.asked);
    }

    /// A recorded answer beats the default IN BOTH DIRECTIONS — and a library the answer never
    /// named falls on its default rather than on Off, which is the case a one-list record cannot
    /// express and the reason [`HomePins`] carries two.
    #[test]
    fn a_recorded_answer_beats_the_default_and_silence_does_not() {
        let rec = record("u-7", true, &roster(), &chosen(&[false, true, true]));
        // the same three libraries, resolved back
        assert_eq!(resolve(&roster(), Some(&rec)), vec![false, true, true]);

        // …now a share answers late, with a library nobody was asked about
        let mut later = roster();
        later.push(lib("late", 4, false));
        later.push(lib("mine", 9, true)); // and a library created on your own server since
        assert_eq!(
            resolve(&later, Some(&rec)),
            vec![false, true, true, false, true],
            "unasked libraries take their OWN default, not the absence of a record"
        );
    }

    /// The never-empty floor. A selection can be emptied without any toggle: pin only a friend's
    /// library, then lose the friend from the roster.
    #[test]
    fn a_selection_that_outlived_its_server_still_leaves_home_something_to_draw() {
        let rec = record("u-7", true, &roster(), &chosen(&[false, false, true]));
        // the share is gone; both of the remaining libraries are recorded Off
        let left = vec![lib("mine", 1, true), lib("mine", 2, true)];
        assert_eq!(
            resolve(&left, Some(&rec)),
            vec![true, true],
            "Home is never empty"
        );

        // and the floor is the FIRST SOURCE's libraries, not every library there is
        let two_srcs = vec![lib("mine", 1, true), lib("theirs", 1, false)];
        let all_off = record("u-7", true, &two_srcs, &chosen(&[false, false]));
        assert_eq!(resolve(&two_srcs, Some(&all_off)), vec![true, false]);
    }

    /// The floor never fires while anything is on — a user who turned their own libraries off in
    /// favour of a friend's gets what they asked for.
    #[test]
    fn the_floor_does_not_second_guess_a_selection_that_works() {
        let rec = record("u-7", true, &roster(), &chosen(&[false, false, true]));
        assert_eq!(resolve(&roster(), Some(&rec)), vec![false, false, true]);
    }

    /// **The gate.** One source is not a question, and the same profile is never asked twice.
    #[test]
    fn the_route_appears_only_for_a_roster_with_more_than_one_source_and_only_once() {
        let answered = HomePins {
            user: "u-7".into(),
            asked: true,
            ..Default::default()
        };
        assert!(
            !asks(1, None),
            "a single-server install goes straight to Home"
        );
        assert!(
            !asks(0, None),
            "…and so does one that has not discovered anything"
        );
        assert!(asks(2, None), "two sources and nobody has been asked");
        assert!(!asks(2, Some(&answered)), "asked once, never again");
        assert!(
            asks(3, None),
            "a third source is still the same one question"
        );
        // An entry that exists but records no answer is still an entry — only `asked` decides.
        let touched = HomePins {
            user: "u-7".into(),
            asked: false,
            ..Default::default()
        };
        assert!(asks(2, Some(&touched)));
    }

    /// A library whose server nobody has named cannot be recorded — the key is the machine, and a
    /// row with no machine would match every other nameless row on the next boot.
    #[test]
    fn a_library_on_an_unnamed_machine_is_not_written_down() {
        // one library on a machine nobody has named, beside two on machines we can name
        let libs = vec![
            lib("", 1, false),
            lib("mine", 1, true),
            lib("theirs", 1, false),
        ];
        let rec = record("u-7", true, &libs, &chosen(&[false, true, true]));
        assert_eq!(
            rec.on
                .iter()
                .map(|p| p.machine_id.as_str())
                .collect::<Vec<_>>(),
            ["mine", "theirs"]
        );
        assert!(
            rec.off.is_empty(),
            "the nameless one is absent from BOTH lists, not recorded Off"
        );
        assert_eq!(
            rec.answer("", 1),
            None,
            "and it can never be looked up either"
        );
        // …so next boot it takes its own default (a share: Off) rather than a neighbour's answer
        assert_eq!(resolve(&libs, Some(&rec)), vec![false, true, true]);
    }

    /// Two profiles, two answers, one file — the requirement the whole rework exists for. The
    /// resolve is a pure function of the record handed in, so switching profile is switching which
    /// record it gets.
    #[test]
    fn two_profiles_resolve_the_same_roster_differently() {
        let libs = roster();
        let dad = record("u-dad", true, &libs, &chosen(&[true, true, true]));
        let kid = record("u-kid", true, &libs, &chosen(&[true, false, false]));
        assert_eq!(resolve(&libs, Some(&dad)), vec![true, true, true]);
        assert_eq!(resolve(&libs, Some(&kid)), vec![true, false, false]);
        // and a third person who has never been asked gets the defaults, not either of theirs
        assert_eq!(resolve(&libs, None), vec![true, true, false]);
    }

    /// **An answer about a server that is not on screen is not withdrawn by writing a new one.**
    ///
    /// A record is written from the section TABLE, which holds only the sources that have answered
    /// — so flipping one switch while a friend's server is asleep would otherwise replace their
    /// whole recorded answer with silence, and silence resolves back to the ownership default.
    #[test]
    fn a_sleeping_servers_answer_survives_a_record_written_without_it() {
        let full = record("u-7", true, &roster(), &chosen(&[true, false, true]));

        // the next boot sees only our own libraries; the share has not answered
        let awake = vec![lib("mine", 1, true), lib("mine", 2, true)];
        let written = carry_forward(
            record("u-7", true, &awake, &chosen(&[true, true])),
            Some(&full),
            &awake,
        );
        assert_eq!(
            written.answer("theirs", 1),
            Some(true),
            "the absent server's answer is kept"
        );
        assert_eq!(
            written.answer("mine", 2),
            Some(true),
            "…and the present one's is the NEW answer"
        );

        // a library the present server has since LOST is dropped rather than carried: that server
        // was answered about in full, and this is not a machine we are keeping silence for
        let full2 = record("u-7", true, &roster(), &chosen(&[true, true, true]));
        let shrunk = vec![lib("mine", 1, true)];
        let written = carry_forward(record("u-7", true, &shrunk, &chosen(&[true])), Some(&full2), &shrunk);
        assert_eq!(written.answer("mine", 2), None);
        assert_eq!(written.answer("theirs", 1), Some(true));

        // and with nothing recorded before, there is nothing to carry
        let fresh = record("u-7", true, &awake, &chosen(&[true, true]));
        assert_eq!(carry_forward(fresh.clone(), None, &awake), fresh);
    }
}
