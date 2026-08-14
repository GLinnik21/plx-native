//! The two states with no shelves: nothing searched yet, and nothing found.
//!
//! Both are **statements, not failures**: no alert mark, no danger tint, no Retry. Nothing found
//! is an answer the server gave, and dressing it as an error tells the user something untrue about
//! their library. A HEADLINE line over a CAPTION/tertiary hint, at [`super::CONTENT_TOP`].
//!
//! The nothing-searched-yet copy exists for a reason worth keeping: an empty screen with no words
//! on it reads as a screen that failed to draw.
//!
//! The hint names the scope — "across both sources" or "in <server>" — and so is the one place
//! this screen states what it is searching when the field's scope line is not shown.
//!
//! ## Plain copy, deliberately — not a [`StatusOverlay`]
//!
//! [`crate::ui::widgets::StatusKind::Empty`] is the right *reading* of "nothing found" and the
//! wrong *anatomy* for it here, so this draws two [`Label`]s instead of mounting the component.
//! `StatusOverlay` is the app's CENTRED read-out: it centres its block on the frame it is given,
//! aligns every run `HAlign::Center`, and fixes its own rungs (`BODY` verdict over a `CAPTION`
//! reason, neither bold). This screen's copy is left-aligned at the app's own side margin, on the
//! `HEADLINE`-bold rung, cap-top-anchored to [`super::CONTENT_TOP`] so it sits under the field
//! exactly where a first shelf's heading would. Bending `StatusOverlay` to that would mean adding
//! an alignment axis, an anchor mode and two size overrides to a component whose whole job is the
//! centred read-out — a fork of its anatomy dressed as a style variant, and every existing caller
//! would then be one builder call away from drifting off the centre it exists to hold.
//!
//! The division that remains is worth stating, because it is the design's own point made in
//! GEOMETRY rather than only in tint: a **statement** is set as page copy where the content would
//! have been; a **fault** is the app's centred read-out. See [`draw`] for the one fault case that
//! reaches this file.
#![allow(dead_code)]

use crate::ui::label::{Label, VAlign};
use crate::ui::search::View;
use crate::ui::widgets::{StatusKind, StatusOverlay};
use crate::ui::{theme, Env, Painter, Rect};
// The retui trait, imported anonymously: `search::View` (the per-frame snapshot) already owns the
// name in this module, and `StatusOverlay::draw` needs the trait in scope, not its name.
use crate::ui::View as _;
use std::ffi::CString;

/// The copy column. Wide enough for the longest hint at `CAPTION` and for a real query in the
/// headline, and deliberately far short of the 1740 the margins allow: a line that runs the full
/// panel reads as a banner, and this is a sentence. It is also the elide budget, so a pasted-long
/// query truncates inside the quotes instead of running off the frame.
const COPY_W: f32 = 1100.0;

/// What this file has to say for the state the screen is in. `None` is a real answer: while a
/// query is in flight there is nothing honest to state yet, and a spinner belongs to whoever owns
/// the fetch — one drawn here would be a second, unowned clock on the same screen.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Say {
    /// `State::Idle` with an empty field — nothing has been asked, and nothing is being asked.
    NotYet,
    /// `State::Idle` with something in the field. Also Idle, because a query below
    /// [`crate::search::MIN_QUERY`] costs no round trip — but the user is looking at their own
    /// typing, and [`Say::NotYet`]'s "you haven't searched yet" would be contradicted by the field
    /// right above it.
    KeepTyping,
    /// `State::Ready` with no shelves — the server answered, and its answer was nothing.
    NoResults,
    /// `State::Failed` — see [`draw`].
    Fault,
}

/// Which of the four (or none), from the store's state, whether anything landed, and how much is
/// in the field.
///
/// **Emptiness alone cannot decide this**, which is the distinction `browse.rs` had to learn: a
/// `Ready` store with nothing in it and a `Failed` one with nothing in it look identical from the
/// shelves and mean opposite things. `Ready` WITH shelves is the results screen's frame, not ours
/// — `mod.rs` does not route it here, and answering `None` means a stray call cannot paint "No
/// results" over a screen full of them.
///
/// `typed` is the trimmed length of the query, and it splits `Idle` in two. `set_query` maps
/// anything under [`crate::search::MIN_QUERY`] to `Idle` and clears the shelves, so backspacing a
/// finished search down to one character lands here with a visibly non-empty field — the one place
/// this file could state something the screen itself disproves.
fn say(state: crate::search::State, has_shelves: bool, typed: usize) -> Option<Say> {
    use crate::search::State;
    match state {
        State::Idle if typed > 0 => Some(Say::KeepTyping),
        State::Idle => Some(Say::NotYet),
        State::Searching => None,
        State::Ready => (!has_shelves).then_some(Say::NoResults),
        State::Failed => Some(Say::Fault),
    }
}

/// `No results for “wallace”` — typographic quotes, because the query is being QUOTED back to the
/// user and a straight `"` is a programmer's mark. `q` arrives already elided, so the closing quote
/// always survives: eliding the whole line would eat it and leave a sentence that never closes.
fn no_results_line(q: &str) -> String {
    format!("No results for \u{201C}{q}\u{201D}")
}

/// The hint under either headline: what this screen searches, stated as a plain fact.
///
/// `n` is how many sources are registered AND dialable; `name` is the one source's machine name
/// when there is exactly one and the roster has described it.
///
/// The design gives two strings, "across both sources" and "in &lt;server&gt;". A third rung is
/// added here rather than stretching the first: the registry holds up to
/// `plex::servers::MAX_SERVERS` slots, and "both" is simply false at three. The nameless case is
/// the fourth — a boot that never reached plex.tv has a client it can dial and no name for it, and
/// the honest answer there is the sentence with no scope clause at all, which stays true whatever
/// the roster turns out to be. Never a placeholder name: inventing one is worse than saying less.
fn scope_hint(n: usize, name: Option<&str>) -> String {
    const WHAT: &str = "Titles, people and collections";
    match (n, name) {
        (1, Some(s)) if !s.is_empty() => format!("{WHAT} in {s}."),
        (2, _) => format!("{WHAT} across both sources."),
        (n, _) if n > 2 => format!("{WHAT} across all sources."),
        _ => format!("{WHAT}."),
    }
}

/// The registry's answer to "what is this screen searching": how many sources can actually be
/// dialled, and — only when there is exactly one — what it is called.
///
/// Registered is not the same as reachable: `server_ids` walks every slot the account was granted,
/// and a slot with no `Client` is one nothing can query. Counting those in would make the hint
/// promise a breadth the fetch does not have.
fn sources() -> (usize, Option<&'static str>) {
    // Counted by walking, never collected: this runs on every presented frame, and a `Vec` built
    // only to be measured is an allocation a still screen pays forever.
    let mut n = 0usize;
    let mut first = None;
    for id in crate::plex::server_ids().filter(|&id| crate::plex::client_for(id).is_some()) {
        first.get_or_insert(id);
        n += 1;
    }
    // `&'static str` rather than an owned copy: `server_facts` hands out a reference into a leaked
    // record that is never freed, so there is nothing to clone.
    let name = (n == 1)
        .then(|| first.and_then(crate::plex::server_facts))
        .flatten()
        .map(|f| f.name.as_str())
        .filter(|s| !s.is_empty());
    (n, name)
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
    let typed = crate::search::query().trim().chars().count();
    let Some(say) = say(crate::search::state(), !crate::search::shelves().is_empty(), typed) else { return };

    if say == Say::Fault {
        let bottom = if v.editing { crate::ui::consts::SCR_H - super::KEYBOARD_H } else { crate::ui::consts::SCR_H };
        let frame = Rect::new(0.0, super::CONTENT_TOP, crate::ui::consts::SCR_W, bottom - super::CONTENT_TOP);
        StatusOverlay::new(frame, c"Search didn\u{2019}t reach the server", StatusKind::Failed)
            .reason(c"Your libraries are fine \u{2014} try again in a moment.")
            .draw(&Env::inert(), p);
        return;
    }

    let head = match say {
        Say::NoResults => {
            // Elide the QUERY, not the line: the shell is measured once and the remainder is the
            // budget, so the closing quote is never what gets cut.
            let q = crate::search::query().trim();
            let shell = CString::new(no_results_line("")).unwrap_or_default();
            let shell_w = crate::text::text_width(shell.as_ptr(), theme::size::HEADLINE, 1);
            no_results_line(&crate::text::elide(q, COPY_W - shell_w, theme::size::HEADLINE, 1, false))
        }
        Say::KeepTyping => format!("Keep typing \u{2014} {} letters or more", crate::search::MIN_QUERY),
        // U+2019, not an ASCII `'`, for the same reason the query is quoted with U+201C/U+201D:
        // these two headlines share one type role, and a straight mark beside a curly pair is the
        // kind of mixed typography that reads as an accident.
        _ => "You haven\u{2019}t searched yet".to_string(),
    };
    let (n, name) = sources();
    // The hint is elided on the same column as the headline. A Plex friendly name is free text and
    // routinely long, and `Label` paints past its frame rather than clipping to it (layout ≠ paint)
    // — so an unmeasured "…in <name>." runs off the copy column toward the frame edge.
    let hint = crate::text::elide(&scope_hint(n, name), COPY_W, theme::size::CAPTION, 0, false);
    // Both runs must outlive the draws below — `Label` borrows the pointer (`ui/CLAUDE.md`).
    let (Ok(head_c), Ok(hint_c)) = (CString::new(head), CString::new(hint)) else { return };

    // Cap-top anchored at CONTENT_TOP and stacked on one `space` rung, so the pair sits where a
    // first shelf's heading would and a descender in the headline cannot shove the hint down.
    let hint_y = super::CONTENT_TOP + crate::text::cap_h(theme::size::HEADLINE, 1) + theme::space::SM;
    Label::new(head_c.as_ptr(), theme::size::HEADLINE, theme::TEXT_HEADING).bold().v(VAlign::CapTop).draw(
        p,
        Rect::new(crate::ui::consts::MARGIN_X, super::CONTENT_TOP, COPY_W, 0.0),
    );
    Label::new(hint_c.as_ptr(), theme::size::CAPTION, theme::TEXT_TERTIARY)
        .v(VAlign::CapTop)
        .draw(p, Rect::new(crate::ui::consts::MARGIN_X, hint_y, COPY_W, 0.0));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::search::State;

    #[test]
    fn the_state_decides_what_is_said_not_the_emptiness() {
        // `Ready` with nothing and `Failed` with nothing are the same store and opposite meanings.
        assert_eq!(say(State::Ready, false, 7), Some(Say::NoResults));
        assert_eq!(say(State::Failed, false, 7), Some(Say::Fault));
        assert_eq!(say(State::Idle, false, 0), Some(Say::NotYet));
        // A query in flight has nothing honest to state, and owns no spinner here.
        assert_eq!(say(State::Searching, false, 7), None);
        // A landed answer is the results screen's, never ours — even if something calls us.
        assert_eq!(say(State::Ready, true, 7), None);
        // ...but a fault still reads as a fault with a stale set behind it, and Idle cannot have
        // shelves at all (`set_query` clears them), so neither is silenced by the flag.
        assert_eq!(say(State::Failed, true, 7), Some(Say::Fault));
        assert_eq!(say(State::Idle, true, 0), Some(Say::NotYet));
    }

    #[test]
    fn a_field_holding_letters_is_never_told_it_has_not_searched() {
        // Backspacing a finished search down under MIN_QUERY: `set_query` says Idle and clears the
        // shelves, but the letters are still on screen, so "you haven't searched yet" would be
        // contradicted by the field directly above the line.
        for typed in 1..crate::search::MIN_QUERY {
            assert_eq!(say(State::Idle, false, typed), Some(Say::KeepTyping));
        }
        // An empty field is the only thing that has genuinely not been searched.
        assert_eq!(say(State::Idle, false, 0), Some(Say::NotYet));
    }

    #[test]
    fn the_query_is_quoted_back_typographically_and_the_closing_quote_survives() {
        assert_eq!(no_results_line("wallace"), "No results for \u{201C}wallace\u{201D}");
        // The elide happens to the query, so whatever arrives is what sits inside the quotes.
        assert_eq!(no_results_line("wal\u{2026}"), "No results for \u{201C}wal\u{2026}\u{201D}");
        assert!(!no_results_line("q").contains('"'), "a straight quote is a programmer's mark");
    }

    #[test]
    fn the_hint_names_the_scope_and_never_says_both_of_three() {
        assert_eq!(scope_hint(1, Some("nas-home")), "Titles, people and collections in nas-home.");
        assert_eq!(scope_hint(2, None), "Titles, people and collections across both sources.");
        assert_eq!(scope_hint(3, None), "Titles, people and collections across all sources.");
        assert_eq!(scope_hint(16, None), "Titles, people and collections across all sources.");
        // A named second source does not make it one source: the count decides the clause.
        assert_eq!(scope_hint(2, Some("nas-home")), "Titles, people and collections across both sources.");
    }

    #[test]
    fn a_source_with_no_name_yet_drops_the_clause_rather_than_inventing_one() {
        // A boot that never reached plex.tv can dial a server and knows nothing about it.
        assert_eq!(scope_hint(1, None), "Titles, people and collections.");
        assert_eq!(scope_hint(1, Some("")), "Titles, people and collections.");
        // Nothing registered at all: still a true sentence, no scope claimed.
        assert_eq!(scope_hint(0, None), "Titles, people and collections.");
    }
}
