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
//! * **Your own libraries arrive On, a friend's arrive Off.** The point of asking is not to put a
//!   stranger's shelves on your front door unannounced (`Shared Sources.dc.html` §F).
//! * **An account with no server of its own is not a special case.** Every source it has is
//!   borrowed, so "a friend's arrives Off" would open the app on nothing at all; with nothing of
//!   your own to prefer, a borrowed library is simply a library.
//! * **A recorded answer beats the default**, in both directions — which is why
//!   [`HomePins`](super::session::HomePins) records the Offs too. A library nobody was asked about
//!   (a share that answered late, one the owner created since) lands on its default instead of
//!   silently Off, and that is what makes "a share arriving later does not reopen this screen"
//!   honest rather than merely quiet.
//! * **Home is never empty.** The one state the control genuinely has no answer for, and the reason
//!   `browse::toggle_pin` refuses to unpin the last library. The same floor is applied here,
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
    /// this account's own server rather than a friend's — the ownership default's whole input
    pub(crate) owned: bool,
}

/// What the first-run screen SHOWS for one library before anybody touches it.
///
/// `any_owned` is the roster's fact, not the library's: with no server of your own there is
/// nothing to prefer, so the ownership default has no question to answer and everything comes On.
pub(crate) fn default_on(lib: LibRef<'_>, any_owned: bool) -> bool {
    lib.owned || !any_owned
}

/// Resolve the whole table for one profile: the recorded answer where there is one, the default
/// where there is not, and the never-empty floor over the lot.
///
/// **A whole-table function on purpose**, even though its two callers both arrive holding one new
/// row. The floor cannot be decided per row — "is anything on?" is a question about the table —
/// and re-running it over rows that already have an answer is free, because every toggle is
/// recorded, so a resolved row and its record always agree.
pub(crate) fn resolve(libs: &[LibRef<'_>], rec: Option<&HomePins>) -> Vec<bool> {
    let any_owned = libs.iter().any(|l| l.owned);
    let mut out: Vec<bool> = libs
        .iter()
        .map(|&l| rec.and_then(|r| r.answer(l.machine_id, l.key)).unwrap_or_else(|| default_on(l, any_owned)))
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

/// Freeze the table's current state as this profile's answer.
///
/// **Both sides**, per [`HomePins`]'s own doc — and only the libraries this build can actually
/// name. A source whose `machineIdentifier` nobody has learned is unaddressable across a restart
/// (the key is the machine, never the roster position, which reshuffles), so recording it would
/// write a row that can only ever match the *other* nameless ones. It keeps its resolved value for
/// this run and is asked about again next boot.
pub(crate) fn record(user: &str, asked: bool, libs: &[LibRef<'_>], on: &[bool]) -> HomePins {
    let mut out = HomePins { user: user.to_string(), asked, on: Vec::new(), off: Vec::new() };
    for (i, l) in libs.iter().enumerate() {
        if l.machine_id.is_empty() {
            continue;
        }
        let lib = PinnedLib { machine_id: l.machine_id.to_string(), key: l.key };
        if on.get(i).copied().unwrap_or(false) {
            out.on.push(lib);
        } else {
            out.off.push(lib);
        }
    }
    out
}

/// Carry forward the answers this profile gave about servers the table cannot currently SEE.
///
/// [`record`] writes down what is on screen, and the section table holds only the sources that
/// have ANSWERED — so a friend whose server was asleep when a switch was flipped would have their
/// whole recorded answer replaced by silence, and the ownership default would come back for
/// libraries the user had already decided about. `HomePins` is a whole-profile record and
/// [`Session::set_pins_for`](super::session::Session::set_pins_for) replaces it wholesale, so the
/// merge belongs here rather than being the store's problem.
///
/// **The grain is the MACHINE, not the library.** A server whose libraries the table holds has
/// just been answered about in full — including one it has since lost, which is correctly dropped.
/// A server it does not hold has not been answered about at all, and silence is not an answer.
pub(crate) fn carry_forward(mut rec: HomePins, prev: Option<&HomePins>, libs: &[LibRef<'_>]) -> HomePins {
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

    fn lib(mid: &str, key: i64, owned: bool) -> LibRef<'_> {
        LibRef { machine_id: mid, key, owned }
    }
    /// The measured pair: your own two libraries and a friend's one, in registration order.
    fn roster<'a>() -> Vec<LibRef<'a>> {
        vec![lib("mine", 1, true), lib("mine", 2, true), lib("theirs", 1, false)]
    }

    /// **The defaults rule, which is the whole first frame of the screen.**
    #[test]
    fn your_own_libraries_arrive_on_and_a_friends_arrives_off() {
        assert_eq!(resolve(&roster(), None), vec![true, true, false]);
    }

    /// A borrowed-only account has nothing of its own to prefer, so the ownership default has no
    /// question to answer. Without this the app would open on an empty Home for exactly the user
    /// who has no other library to fall back on.
    #[test]
    fn an_account_with_no_server_of_its_own_gets_every_borrowed_library() {
        let libs = vec![lib("a", 1, false), lib("b", 1, false)];
        assert_eq!(resolve(&libs, None), vec![true, true]);
    }

    /// A recorded answer beats the default IN BOTH DIRECTIONS — and a library the answer never
    /// named falls on its default rather than on Off, which is the case a one-list record cannot
    /// express and the reason [`HomePins`] carries two.
    #[test]
    fn a_recorded_answer_beats_the_default_and_silence_does_not() {
        let rec = record("u-7", true, &roster(), &[false, true, true]);
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
        let rec = record("u-7", true, &roster(), &[false, false, true]);
        // the share is gone; both of the remaining libraries are recorded Off
        let left = vec![lib("mine", 1, true), lib("mine", 2, true)];
        assert_eq!(resolve(&left, Some(&rec)), vec![true, true], "Home is never empty");

        // and the floor is the FIRST SOURCE's libraries, not every library there is
        let two_srcs = vec![lib("mine", 1, true), lib("theirs", 1, false)];
        let all_off = record("u-7", true, &two_srcs, &[false, false]);
        assert_eq!(resolve(&two_srcs, Some(&all_off)), vec![true, false]);
    }

    /// The floor never fires while anything is on — a user who turned their own libraries off in
    /// favour of a friend's gets what they asked for.
    #[test]
    fn the_floor_does_not_second_guess_a_selection_that_works() {
        let rec = record("u-7", true, &roster(), &[false, false, true]);
        assert_eq!(resolve(&roster(), Some(&rec)), vec![false, false, true]);
    }

    /// **The gate.** One source is not a question, and the same profile is never asked twice.
    #[test]
    fn the_route_appears_only_for_a_roster_with_more_than_one_source_and_only_once() {
        let answered = HomePins { user: "u-7".into(), asked: true, ..Default::default() };
        assert!(!asks(1, None), "a single-server install goes straight to Home");
        assert!(!asks(0, None), "…and so does one that has not discovered anything");
        assert!(asks(2, None), "two sources and nobody has been asked");
        assert!(!asks(2, Some(&answered)), "asked once, never again");
        assert!(asks(3, None), "a third source is still the same one question");
        // An entry that exists but records no answer is still an entry — only `asked` decides.
        let touched = HomePins { user: "u-7".into(), asked: false, ..Default::default() };
        assert!(asks(2, Some(&touched)));
    }

    /// A library whose server nobody has named cannot be recorded — the key is the machine, and a
    /// row with no machine would match every other nameless row on the next boot.
    #[test]
    fn a_library_on_an_unnamed_machine_is_not_written_down() {
        // one library on a machine nobody has named, beside two on machines we can name
        let libs = vec![lib("", 1, false), lib("mine", 1, true), lib("theirs", 1, false)];
        let rec = record("u-7", true, &libs, &[false, true, true]);
        assert_eq!(rec.on.iter().map(|p| p.machine_id.as_str()).collect::<Vec<_>>(), ["mine", "theirs"]);
        assert!(rec.off.is_empty(), "the nameless one is absent from BOTH lists, not recorded Off");
        assert_eq!(rec.answer("", 1), None, "and it can never be looked up either");
        // …so next boot it takes its own default (a share: Off) rather than a neighbour's answer
        assert_eq!(resolve(&libs, Some(&rec)), vec![false, true, true]);
    }

    /// Two profiles, two answers, one file — the requirement the whole rework exists for. The
    /// resolve is a pure function of the record handed in, so switching profile is switching which
    /// record it gets.
    #[test]
    fn two_profiles_resolve_the_same_roster_differently() {
        let libs = roster();
        let dad = record("u-dad", true, &libs, &[true, true, true]);
        let kid = record("u-kid", true, &libs, &[true, false, false]);
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
        let full = record("u-7", true, &roster(), &[true, false, true]);

        // the next boot sees only our own libraries; the share has not answered
        let awake = vec![lib("mine", 1, true), lib("mine", 2, true)];
        let written = carry_forward(record("u-7", true, &awake, &[true, true]), Some(&full), &awake);
        assert_eq!(written.answer("theirs", 1), Some(true), "the absent server's answer is kept");
        assert_eq!(written.answer("mine", 2), Some(true), "…and the present one's is the NEW answer");

        // a library the present server has since LOST is dropped rather than carried: that server
        // was answered about in full, and this is not a machine we are keeping silence for
        let full2 = record("u-7", true, &roster(), &[true, true, true]);
        let shrunk = vec![lib("mine", 1, true)];
        let written = carry_forward(record("u-7", true, &shrunk, &[true]), Some(&full2), &shrunk);
        assert_eq!(written.answer("mine", 2), None);
        assert_eq!(written.answer("theirs", 1), Some(true));

        // and with nothing recorded before, there is nothing to carry
        let fresh = record("u-7", true, &awake, &[true, true]);
        assert_eq!(carry_forward(fresh.clone(), None, &awake), fresh);
    }
}
