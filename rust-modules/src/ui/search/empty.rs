//! The two states with no shelves: nothing searched yet, and nothing found.
//!
//! Both are **statements, not failures**: no alert mark, no danger tint, no Retry. Nothing found
//! is an answer the server gave, and dressing it as an error tells the user something untrue about
//! their library.
//!
//! ## The anatomy, which changed on 2026-08-15 and is the reason this doc is long
//!
//! Each state is **two parts, CENTRED in the band the app owns**: the region's own name in the caps
//! header face ([`crate::ui::table::HDR_H`]'s type role — CAPTION, uppercase, tertiary), then one
//! statement at `size::TITLE`. Same two parts, same order, same place for both, which is what makes
//! them read as one screen answering in one voice rather than as two unrelated read-outs.
//!
//! | state | header | statement |
//! |---|---|---|
//! | nothing searched | `RECENT SEARCHES` | Nothing searched yet |
//! | nothing found | `SEARCH RESULTS` | No results for “…” |
//!
//! The header is load-bearing and not decoration: an empty recents list is still **the recents
//! list**, so it keeps the name of the thing it is empty of — the header is the only part of the
//! populated layout that survives, and dropping it would make the empty state a different screen
//! rather than the same one with nothing in it.
//!
//! **Centred, and only because the column is gone.** The populated list is a left column under the
//! field, and the terms line up with the control that produced them; with no terms there is no
//! column to align to, and a lone sentence pinned to the left margin of an otherwise empty 1920
//! panel reads as a fragment. The band is [`band`] — below the field, above the raised keyboard —
//! so the statement sits in the middle of the space the app actually has, and MOVES when the panel
//! comes up. It was a left-aligned `HEADLINE` over a `CAPTION` scope hint at `CONTENT_TOP` before.
//!
//! **The scope hint is gone with it.** It said "Titles, people and collections in <server>." under
//! the headline, and it was the second place on one screen to state what is being searched — the
//! field's scope line below the query is the first. The design's note is that repeating it
//! "makes the screen look like it is arguing"; the consequence is that [`field::scope_text`] now
//! states the scope for a SINGLE source too, since this line is no longer there to carry that case.
//!
//! ## Plain copy, deliberately — not a [`StatusOverlay`]
//!
//! [`crate::ui::widgets::StatusKind::Empty`] is the right *reading* of "nothing found" and the
//! wrong *anatomy* for it, so this draws two [`Label`]s instead of mounting the component — and it
//! stays true now that this file centres, because centring was never the whole difference.
//! `StatusOverlay` fixes its own rungs (a `BODY` verdict over a `CAPTION` reason, neither bold) and
//! has no caps header at all; this is a caps kicker over a **bold TITLE**. Bending it to that means
//! adding a header slot and two size overrides to a component whose whole job is the one centred
//! read-out, after which every existing caller is one builder call away from drifting off it.
//!
//! The division that remains is the design's own point made in TYPE rather than only in tint: a
//! **statement** is page copy in the page's own voice; a **fault** is the app's read-out. See
//! [`draw`] for the one fault case that reaches this file.
#![allow(dead_code)]

use crate::ui::label::{HAlign, Label, VAlign};
use crate::ui::search::View;
use crate::ui::widgets::{StatusKind, StatusOverlay};
use crate::ui::{theme, Env, Painter, Rect};
// The retui trait, imported anonymously: `search::View` (the per-frame snapshot) already owns the
// name in this module, and `StatusOverlay::draw` needs the trait in scope, not its name.
use crate::ui::View as _;
use std::ffi::{CStr, CString};

/// The copy column — the statement's elide budget. Wide enough for a real query at `TITLE` and
/// deliberately far short of the 1920 the panel allows: a line that runs the full width reads as a
/// banner, and this is a sentence.
const COPY_W: f32 = 1200.0;

/// Header to statement. The mock's 20 is a gap between two CSS **line boxes**, which carry leading
/// this renderer does not: text here is placed by its cap band, so the same number would draw
/// tighter than the design. `space::MD` is the rung that lands where the mock looks.
const PART_GAP: f32 = theme::space::MD;

/// What this file has to say for the state the screen is in. `None` is a real answer: while a
/// query is in flight there is nothing honest to state yet, and a spinner belongs to whoever owns
/// the fetch — one drawn here would be a second, unowned clock on the same screen.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Say {
    /// `State::Idle` — nothing has been searched. **Including with letters in the field**: a query
    /// under [`crate::search::MIN_QUERY`] costs no round trip, so nothing has been asked, and that
    /// is exactly what this says.
    ///
    /// It used to split here, into a second `KeepTyping` state whose headline read "Keep typing —
    /// 2 letters or more", on the reasoning that "nothing searched yet" would be contradicted by
    /// the field right above it. The design answers that where the contradiction is — **in the
    /// field**, as a ghost run after the caret ([`super::field`]'s `one more character`) — and
    /// keeps this region on the pre-search state it was already showing. The region belongs to the
    /// recent terms, and replacing them with an instruction because a key was pressed takes content
    /// away for pressing a key.
    NotYet,
    /// `State::Ready` with no shelves — the server answered, and its answer was nothing.
    NoResults,
    /// `State::Failed` — see [`draw`].
    Fault,
}

/// Which of the three (or none), from the store's state and whether anything landed.
///
/// **Emptiness alone cannot decide this**, which is the distinction `browse.rs` had to learn: a
/// `Ready` store with nothing in it and a `Failed` one with nothing in it look identical from the
/// shelves and mean opposite things. `Ready` WITH shelves is the results screen's frame, not ours
/// — `mod.rs` does not route it here, and answering `None` means a stray call cannot paint "No
/// results" over a screen full of them.
fn say(state: crate::search::State, has_shelves: bool) -> Option<Say> {
    use crate::search::State;
    match state {
        State::Idle => Some(Say::NotYet),
        State::Searching => None,
        State::Ready => (!has_shelves).then_some(Say::NoResults),
        State::Failed => Some(Say::Fault),
    }
}

/// The band a statement centres in: from [`super::CONTENT_TOP`] down to the panel edge, or to the
/// TV keyboard's top edge while it is up.
///
/// The screen's standing rule is that nothing the app owns hides behind that panel; here it is also
/// what keeps the statement optically centred in the space the user can actually see, rather than
/// centred in a 1080 frame whose bottom third is covered.
fn band(editing: bool) -> Rect {
    let bottom = if editing { crate::ui::consts::SCR_H - super::KEYBOARD_H } else { crate::ui::consts::SCR_H };
    Rect::new(0.0, super::CONTENT_TOP, crate::ui::consts::SCR_W, bottom - super::CONTENT_TOP)
}

/// `No results for “wallace”` — typographic quotes, because the query is being QUOTED back to the
/// user and a straight `"` is a programmer's mark. `q` arrives already elided, so the closing quote
/// always survives: eliding the whole line would eat it and leave a sentence that never closes.
fn no_results_line(q: &str) -> String {
    format!("No results for \u{201C}{q}\u{201D}")
}

/// The region's own name, in the caps header face — the part that survives from the populated
/// layout, so an empty list still says what it is a list of. See the module doc's table.
fn header_of(say: Say) -> &'static CStr {
    match say {
        Say::NoResults => c"SEARCH RESULTS",
        _ => c"RECENT SEARCHES",
    }
}

/// Draw whichever of the three states applies, or nothing.
///
/// **The fault case is here only because the routing above sends it here.** `mod.rs` hands this
/// file every query whose shelf list is empty, and a failed request has an empty shelf list — so
/// returning early for `State::Failed` would leave the panel blank under a field holding a query,
/// which is precisely the "a screen that failed to draw" reading the not-yet copy exists to
/// prevent. It gets the app's own failure anatomy and none of this screen's: the centred
/// [`StatusOverlay`] at [`StatusKind::Failed`], danger-tinted, with a reason line that says what is
/// still fine. **No action pill** — `StatusOverlay`'s control has to be focusable to be a control,
/// and [`super::Zone`] has no zone that can hold it; a pill the remote cannot reach is worse than
/// no pill. When the fault gets a real owner (a Retry, a zone, a per-source verdict), this arm is
/// the thing to delete, and the two statements above it are untouched by that.
///
/// The read-out's frame is the region whose content is missing, clamped above the TV's keyboard
/// while it is up ([`super::KEYBOARD_H`]) — the screen's standing rule is that nothing the app owns
/// hides behind that panel, and a read-out centred into it would be the first thing to break it.
pub(crate) fn draw(p: Painter, v: &View) {
    let Some(say) = say(crate::search::state(), !crate::search::shelves().is_empty()) else { return };
    let frame = band(v.editing);

    if say == Say::Fault {
        StatusOverlay::new(frame, c"Search didn\u{2019}t reach the server", StatusKind::Failed)
            .reason(c"Your libraries are fine \u{2014} try again in a moment.")
            .draw(&Env::inert(), p);
        return;
    }

    let statement = match say {
        Say::NoResults => {
            // Elide the QUERY, not the line: the shell is measured once and the remainder is the
            // budget, so the closing quote is never what gets cut.
            let q = crate::search::query().trim();
            let shell = CString::new(no_results_line("")).unwrap_or_default();
            let shell_w = crate::text::text_width(shell.as_ptr(), theme::size::TITLE, 1);
            no_results_line(&crate::text::elide(q, COPY_W - shell_w, theme::size::TITLE, 1, false))
        }
        // U+2019, not an ASCII `'`, for the same reason the query is quoted with U+201C/U+201D:
        // these two statements share one type role, and a straight mark beside a curly pair is the
        // kind of mixed typography that reads as an accident.
        _ => "Nothing searched yet".to_string(),
    };
    // The run must outlive the draw below — `Label` borrows the pointer (`ui/CLAUDE.md`).
    let Ok(statement_c) = CString::new(statement) else { return };
    let header = header_of(say);

    // Both parts by cap band, so the block's height is the ink's and a descender in the statement
    // cannot push the pair off centre. Centred on the FRAME, horizontally and vertically.
    let (hdr_h, stm_h) = (crate::text::cap_h(theme::size::CAPTION, 0), crate::text::cap_h(theme::size::TITLE, 1));
    let top = frame.y + (frame.h - (hdr_h + PART_GAP + stm_h)) * 0.5;
    Label::new(header.as_ptr(), theme::size::CAPTION, theme::TEXT_TERTIARY)
        .h(HAlign::Center)
        .v(VAlign::CapTop)
        .draw(p, Rect::new(frame.x, top, frame.w, 0.0));
    Label::new(statement_c.as_ptr(), theme::size::TITLE, theme::TEXT_HEADING)
        .bold()
        .h(HAlign::Center)
        .v(VAlign::CapTop)
        .draw(p, Rect::new(frame.x, top + hdr_h + PART_GAP, frame.w, 0.0));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::search::State;

    #[test]
    fn the_state_decides_what_is_said_not_the_emptiness() {
        // `Ready` with nothing and `Failed` with nothing are the same store and opposite meanings.
        assert_eq!(say(State::Ready, false), Some(Say::NoResults));
        assert_eq!(say(State::Failed, false), Some(Say::Fault));
        assert_eq!(say(State::Idle, false), Some(Say::NotYet));
        // A query in flight has nothing honest to state, and owns no spinner here.
        assert_eq!(say(State::Searching, false), None);
        // A landed answer is the results screen's, never ours — even if something calls us.
        assert_eq!(say(State::Ready, true), None);
        // ...but a fault still reads as a fault with a stale set behind it, and Idle cannot have
        // shelves at all (`set_query` clears them), so neither is silenced by the flag.
        assert_eq!(say(State::Failed, true), Some(Say::Fault));
        assert_eq!(say(State::Idle, true), Some(Say::NotYet));
    }

    /// An empty list is still THE LIST, so it keeps the name of what it is empty of — the one part
    /// of the populated layout that survives. Without it the empty state is a different screen
    /// rather than the same one with nothing in it.
    #[test]
    fn each_statement_keeps_the_name_of_the_region_it_replaces() {
        assert_eq!(header_of(Say::NotYet), c"RECENT SEARCHES");
        assert_eq!(header_of(Say::NoResults), c"SEARCH RESULTS");
        // …and it is the recents list's own header, verbatim, or the two spellings drift apart.
        assert_eq!(header_of(Say::NotYet), super::super::recents::HDR);
    }

    /// The band the pair centres in RISES with the keyboard: nothing the app owns hides behind that
    /// panel, and a statement centred in a 1080 frame whose bottom third is covered is not centred.
    #[test]
    fn the_statement_centres_in_the_space_the_user_can_actually_see() {
        let (up, down) = (band(true), band(false));
        assert_eq!(up.y, super::super::CONTENT_TOP, "both start under the field");
        assert_eq!(down.y, super::super::CONTENT_TOP);
        assert_eq!(down.h - up.h, super::super::KEYBOARD_H, "the raised panel is the whole difference");
        assert!(up.y + up.h <= crate::ui::consts::SCR_H - super::super::KEYBOARD_H);
        assert_eq!(down.y + down.h, crate::ui::consts::SCR_H);
        assert_eq!((up.x, up.w), (0.0, crate::ui::consts::SCR_W), "centred on the PANEL, not the field's column");
    }

    #[test]
    fn the_query_is_quoted_back_typographically_and_the_closing_quote_survives() {
        assert_eq!(no_results_line("wallace"), "No results for \u{201C}wallace\u{201D}");
        // The elide happens to the query, so whatever arrives is what sits inside the quotes.
        assert_eq!(no_results_line("wal\u{2026}"), "No results for \u{201C}wal\u{2026}\u{201D}");
        assert!(!no_results_line("q").contains('"'), "a straight quote is a programmer's mark");
    }
}
