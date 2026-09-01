//! **The Sources ROW MODEL** — one list of libraries grouped by server, drawn on two surfaces.
//!
//! The Library toolbar's Source panel (`ui::library`, deliverable A) and the first-run route that
//! asks the same question before Home (`ui::onboard`, deliverable F) show the SAME list: your
//! servers, each server's libraries, and whether each one feeds Home. The canvas says so in as
//! many words — F "is the same list as A's On Home level, in the flow's own frame rather than a
//! panel" — so it is one builder, and the two screens differ only in the frame around it and in
//! whether the roster-refresh row rides along.
//!
//! Building it twice would have been the ordinary thing to do and the wrong one: the two would
//! have drifted on exactly the details that make the list readable — which column carries a mark,
//! which carries a word, what an unreachable group says and where it says it — and each drift
//! would read as a bug on whichever screen you saw second.
//!
//! Pure over its inputs, so it is host-testable without a live section table (which is also why
//! `browse` hands out owned [`SrcGroup`](crate::browse::SrcGroup)/[`SrcRow`](crate::browse::SrcRow)
//! projections rather than borrows of its statics).
use crate::browse::{SourceState, SrcGroup};
use crate::plex::probe::Location;
use crate::ui::table::{Row, Section};

/// The two levels of the Sources panel, swapped by the pills at its top — the same swap the
/// player's track menu makes between Audio and Subtitles.
///
/// They differ in MEDIUM as well as in position, and that is the design's rule: a **mark** says
/// where you are, a **word** says what is set, and no row ever says both. So Browse draws one tick
/// and no words, On Home draws every row's word and no ticks — neither level mirrors the other's
/// marks.
///
/// The first-run route is [`Level::OnHome`] and nothing else: it has no current library to point
/// at, because it runs before there is a Library screen to have been on.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Level {
    /// a picker: one tick, on the library you are looking at; OK closes the panel
    Browse,
    /// toggles: `On`/`Off` at the trailing edge; OK flips and the panel stays open
    OnHome,
}

/// What a row of the Sources list does. Built beside the rows, indexed by the same global row
/// index the `TableView` reports — including the separator, which does nothing and can never be
/// focused, but still occupies an index.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum SrcAction {
    None,
    /// browse (Browse level) or pin (On Home level) this section
    Library(usize),
    Recheck,
}

/// Does the list end with the roster-refresh row?
///
/// The panel's does. **The first-run route's does not**, and that is a design call rather than an
/// omission: a share that arrives later must not reopen a first-run screen, so there is nothing
/// for a refresh to be FOR there — it appears unpinned in the Library chip's list, "which is where
/// 'Check for new shares' already lives" (`Shared Sources.dc.html` §F).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Tail {
    Recheck,
    None,
}

/// The word a group's state is said in — `None` for the two states that say nothing.
///
/// [`SourceState::NotProbed`] says nothing because nothing has gone wrong: a group only reaches
/// this list once its libraries have landed, so "nobody has dialled" here means the app has talked
/// to the server and simply not GRADED the connection. That is a fact about our own instrumentation
/// and stating it would put a warning on a working server.
///
/// [`SourceState::Reachable`] says nothing because it is the unremarkable case. What a working
/// group may still have to say is its TIER — see [`tier_word`].
fn state_word(s: SourceState) -> Option<&'static str> {
    match s {
        SourceState::NotProbed | SourceState::Reachable => None,
        SourceState::Unauthorized => Some(UNAUTHORIZED),
        SourceState::Unreachable => Some(UNREACHABLE),
    }
}

/// The server answered and **refused our token**: a sharing-grant problem, never a network one.
///
/// It has to read as a different KIND of fault from [`UNREACHABLE`], because it has a different
/// remedy and the wrong word costs the user an evening — "not reachable" sends somebody to look at
/// a router for something no router was ever part of. The remedy is in this same panel, one row
/// down: *Check for new shares* is the `/api/v2/resources` refetch that reissues the per-(user,
/// server) `accessToken`, which is why this run does not have to carry an instruction as well.
const UNAUTHORIZED: &str = "Not authorized";
/// Did not answer at all — refused, timed out, or unresolvable.
const UNREACHABLE: &str = "Not reachable";

/// The word a WORKING group's connection tier is said in — `None` when there is nothing worth
/// saying.
///
/// [`Location::Local`] is silent by the same rule that keeps a healthy state silent: it is what a
/// server on your own network is expected to be, and annotating every group with it would be noise
/// on the common panel. The other two each buy the user an explanation they cannot get anywhere
/// else:
///
/// - **Relay** is the one this list exists to say. A relay connection is a ~2 Mbit/s tunnel the
///   server transcodes down to fit (`plex::probe`'s rule 3), so a library that plays at DVD quality
///   over a 200 Mbit link reads as a broken server unless something on screen says why.
/// - **Remote** is quieter but the same shape: it explains a slower first frame and a transcode on
///   a file that direct-plays at home.
fn tier_word(t: Location) -> Option<&'static str> {
    match t {
        Location::Local => None,
        Location::Remote => Some("Remote"),
        Location::Relay => Some("Relay"),
    }
}

/// May this group's libraries be OPENED? The question [`Section::dim`] actually asks.
///
/// **[`SrcGroup::reachable`] is still the read for three of the four states, and wrong for the
/// fourth** — which is the whole reason the state widened. That method answers the old two-state
/// question, so `NotProbed` comes back `true` (correctly: a group nobody has dialled must not open
/// dimmed) and so does `Unauthorized`, not because it is browsable but because a `bool` had no way
/// to say otherwise. A server that answered and refused our token has nothing behind it, so it dims
/// exactly as an unreachable one does; only the WORD differs, because only the remedy differs.
///
/// Written as an exhaustive match rather than `reachable() && state != Unauthorized`, so that a
/// fifth state has to be given an answer here instead of quietly inheriting one.
fn usable(g: &SrcGroup) -> bool {
    match g.state {
        SourceState::NotProbed | SourceState::Reachable | SourceState::Unreachable => g.reachable(),
        SourceState::Unauthorized => false,
    }
}

/// The group header's ACCESSORY: `[state] · [tier] · handle`, in that order.
///
/// **The state leads**, and that is load-bearing rather than cosmetic. The accessory is the last
/// run on the header line and is elided from the right ([`Section::accessory`]), so whatever is
/// last is what gives way — leading with the state means a 34-character plex.tv handle truncates
/// instead of the fact that the server is not answering.
///
/// The TIER is only spoken for a group that is working. On a dead one it is at best stale and at
/// worst misleading: "Not reachable · Relay · friend" reads as three problems where there is one,
/// and the tier that failed is not a fact about the server the user is being asked to look at.
fn accessory(g: &SrcGroup) -> String {
    let tier = (g.state == SourceState::Reachable)
        .then(|| g.tier.and_then(tier_word))
        .flatten();
    let runs = [
        state_word(g.state),
        tier,
        (!g.handle.is_empty()).then_some(g.handle.as_str()),
    ];
    runs.iter()
        .flatten()
        .copied()
        .collect::<Vec<_>>()
        .join(" \u{b7} ")
}

/// Build the list.
///
/// The levels never mirror each other's marks. **Browse** is a picker: one tick, on the library you
/// are looking at, and no words. **On Home** is a set of switches: every row states its value as
/// the word `On`/`Off` at the trailing edge, and no ticks. A mark says where you are, a word says
/// what is set, and no row is allowed to say both.
///
/// Returns the sections beside one [`SrcAction`] per global row index — the separator included,
/// which does nothing but still occupies an index.
pub(crate) fn sections(
    level: Level,
    groups: &[crate::browse::SrcGroup],
    rows: &[crate::browse::SrcRow],
    tail: Tail,
) -> (Vec<Section>, Vec<SrcAction>) {
    let mut out: Vec<Section> = Vec::new();
    let mut acts: Vec<SrcAction> = Vec::new();
    for (gi, g) in groups.iter().enumerate() {
        let mine = rows.iter().filter(|r| r.src == gi);
        // a server whose libraries we have never learned contributes no group at all — a header
        // over nothing reads as broken, and D's answer for a source that cannot be reached on a
        // FIRST run is absence
        if mine.clone().next().is_none() {
            continue;
        }
        // the header is the MACHINE, its accessory the PERSON — the one place in the app a machine
        // is named, and the reason every other surface can say only the handle. A group that is not
        // working also SAYS so there, and says it FIRST — see [`accessory`]. It is a state in the
        // same register as the rows' own `On`/`Off`.
        let mut sec = Section::new(g.name.clone())
            .accessory(accessory(g))
            .dim(!usable(g));
        for r in mine {
            let mut row = Row::new(r.title.clone());
            row = match level {
                Level::Browse => row.checked(r.current).detail(r.count_line.clone()),
                Level::OnHome => row
                    .toggle(r.pinned)
                    // the LAST pinned library: the value dims, the label keeps live ink, and the
                    // sub-line states the rule. Not the whole row — dim means unavailable, and this
                    // is the library that works.
                    .value_dim(r.last_pinned)
                    .detail(if r.last_pinned {
                        "Home needs one library".to_string()
                    } else {
                        r.count_line.clone()
                    }),
            };
            acts.push(SrcAction::Library(r.section));
            sec = sec.row(row);
        }
        out.push(sec);
    }
    // …and the one row that is not a library, last, under a separator. It rides BOTH levels, which
    // is what keeps their row counts — and therefore the panel's height — identical.
    if tail == Tail::None {
        return (out, acts);
    }
    if let Some(last) = out.last_mut() {
        last.rows.push(Row::separator());
        acts.push(SrcAction::None);
        // no leading glyph, deliberately: on the Browse level that column carries the picker's
        // tick, and an action mark in it would be a second grammar for one column
        last.rows.push(Row::new("Check for new shares"));
        acts.push(SrcAction::Recheck);
    }
    (out, acts)
}

/// Where the cursor sits. On OPEN it lands on the library you are browsing; on a rebuild — a level
/// swap, a pin flip, a source's libraries landing — it HOLDS, because the two levels list the same
/// rows in the same order and the design's whole claim about the swap is that nothing moves.
pub(crate) fn sel(rows: &[crate::browse::SrcRow], keep: Option<i32>) -> i32 {
    keep.unwrap_or_else(|| {
        rows.iter()
            .position(|r| r.current)
            .map(|i| i as i32)
            .unwrap_or(0)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::browse::{SourceState, SrcGroup};

    fn group(state: SourceState, tier: Option<Location>, handle: &str) -> SrcGroup {
        SrcGroup {
            name: "nas-home".into(),
            handle: handle.into(),
            state,
            tier,
        }
    }

    /// **The four states, as four different sentences.** The two that say nothing say nothing for
    /// two different reasons (see [`state_word`]), and the two that speak must not be confusable:
    /// a 401 is a sharing-grant problem whose remedy is in this panel, and calling it "not
    /// reachable" sends the user to look at a router.
    #[test]
    fn every_source_state_gets_its_own_word() {
        let words: Vec<String> = [
            SourceState::NotProbed,
            SourceState::Reachable,
            SourceState::Unauthorized,
            SourceState::Unreachable,
        ]
        .iter()
        .map(|s| accessory(&group(*s, None, "friend")))
        .collect();
        assert_eq!(
            words[0], "friend",
            "nobody has dialled: nothing is wrong, so nothing is said"
        );
        assert_eq!(words[1], "friend", "a working source states only its owner");
        assert_eq!(words[2], "Not authorized \u{b7} friend");
        assert_eq!(words[3], "Not reachable \u{b7} friend");
        assert_ne!(
            words[2], words[3],
            "the two faults are told apart, or the remedy is a guess"
        );
    }

    /// The state LEADS, so the run that gives way under the accessory's elision is the handle — and
    /// a source with no handle (your own server) says the state alone rather than a stray middot.
    #[test]
    fn the_state_leads_and_an_absent_handle_leaves_no_separator() {
        assert_eq!(
            accessory(&group(SourceState::Unreachable, None, "")),
            "Not reachable"
        );
        assert_eq!(accessory(&group(SourceState::Reachable, None, "")), "");
    }

    /// **Relay is the run this list exists to draw.** Local says nothing (it is what a server is
    /// expected to be), Remote and Relay each explain something the user would otherwise read as a
    /// broken server.
    #[test]
    fn the_winning_tier_is_stated_for_relay_and_remote_and_never_for_local() {
        let a = |t| accessory(&group(SourceState::Reachable, Some(t), "friend"));
        assert_eq!(a(Location::Local), "friend");
        assert_eq!(a(Location::Remote), "Remote \u{b7} friend");
        assert_eq!(a(Location::Relay), "Relay \u{b7} friend");
    }

    /// A tier is a fact about a connection that WORKS. On a dead source it is stale at best, and
    /// stacking it behind the fault reads as two problems where there is one.
    #[test]
    fn a_broken_source_states_its_fault_and_not_its_last_known_tier() {
        assert_eq!(
            accessory(&group(
                SourceState::Unreachable,
                Some(Location::Relay),
                "friend"
            )),
            "Not reachable \u{b7} friend"
        );
    }

    /// **The dim is `usable`, never `reachable()`.** That method answers the OLD question, in which
    /// an answered-and-refused server comes back `true` — so a panel that dimmed on it would draw a
    /// group with nothing browsable behind it as though it were live.
    #[test]
    fn only_the_states_with_something_behind_them_stay_lit() {
        let u = |s| usable(&group(s, None, "friend"));
        assert!(
            u(SourceState::NotProbed),
            "a group nobody has dialled must not open dimmed"
        );
        assert!(u(SourceState::Reachable));
        assert!(
            !u(SourceState::Unauthorized),
            "there is nothing browsable behind a 401"
        );
        assert!(!u(SourceState::Unreachable));
        assert!(
            group(SourceState::Unauthorized, None, "friend").reachable(),
            "…and this is exactly the answer that made the fourth state worth its own arm"
        );
    }
}
