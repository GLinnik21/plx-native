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
        // is named, and the reason every other surface can say only the handle. An unreachable
        // group also SAYS so there, and says it FIRST: the accessory is elided from the right, so
        // leading with the state means a long handle gives way rather than the fact that the server
        // is not answering. It is a state in the same register as the rows' own `On`/`Off`.
        let accessory = match (g.reachable(), g.handle.is_empty()) {
            (true, _) => g.handle.clone(),
            (false, true) => "Not reachable".to_string(),
            (false, false) => format!("Not reachable \u{b7} {}", g.handle),
        };
        let mut sec = Section::new(g.name.clone()).accessory(accessory).dim(!g.reachable());
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
    keep.unwrap_or_else(|| rows.iter().position(|r| r.current).map(|i| i as i32).unwrap_or(0))
}
