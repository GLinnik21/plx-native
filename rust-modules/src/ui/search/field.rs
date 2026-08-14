//! The query capsule, its caret, and the scope line beside it.
//!
//! ## The design, so it does not have to be re-derived from the mock
//!
//! - The field carries **no magnifier**. The pill above it is the mark, and repeating it here says
//!   nothing the screen has not already said.
//! - Focused (or editing) it wears the one control fill, `theme::ACCENT`, with `ACCENT_INK` on it.
//!   Holding a query while focus is elsewhere it drops to `CONTROL_IDLE_FILL`/`CONTROL_IDLE_INK`,
//!   so the query stays legible without claiming focus it does not have.
//! - The placeholder is **unconditional**: an empty focused field is the word "Search" in the
//!   on-accent hint ink (`ROW_VALUE_INK_ON_DIM`), never a blank capsule with a lone caret.
//! - The run clips from the **LEFT** — a field shows the TAIL of the string, so the caret end
//!   stays on screen. There is no text cursor: no pointer, no selection, no click-to-position.
//! - The caret is 3px × 34px and **solid**. Nothing pulses in this app, and a blinking caret would
//!   hold the GL loop awake forever — `ui::idle` cannot see a clock-driven animation, so a blink
//!   would either freeze or defeat the whole idle gate. Drawn only while `editing`.
//! - The **scope line** sits at x=950 on the field's own row, CAPTION/tertiary, and is stated only
//!   when the scope is smaller than the user's whole account: one plain fact, no warning tint and
//!   no Retry (the retry that re-kicks one source lives on that section's Library).
//!
//! ## The two pieces of arithmetic worth extracting
//!
//! [`run_layout`] and [`scope_text`] are **pure**, and host-tested below, because both are the kind
//! of thing that is wrong by a few pixels or one word and invisible in a screenshot. They are not
//! the whole risk, though — [`draw`] and [`draw_scope`] carry the composition (the fill branch, the
//! scissor pair, three placements and the two-store projection), and that is where a defect hides
//! from both the tests and the eye.
#![allow(dead_code)]

use crate::ui::label::Label;
use crate::ui::search::{View, Zone, FIELD};
use crate::ui::{theme, Painter, Rect};
use std::ffi::{CStr, CString};

/// Inner padding, both ends — the air between the capsule's arc and the run inside it. From
/// `Search Screen.dc.html` directly, which is why it is not a `theme::space` rung: this is the
/// design's stated inset for THIS control, not a gap between stacked blocks.
const PAD_X: f32 = 32.0;
/// The caret, from the design. Solid, never animated; see the module doc.
const CARET_W: f32 = 3.0;
const CARET_H: f32 = 34.0;
/// Clearance between the caret and the PLACEHOLDER behind it — the only gap the caret needs, since
/// against a real run it stands at the insertion point with no lead at all (see [`run_layout`]).
const HINT_GAP: f32 = 4.0;
/// Where the scope line starts — its own column on the field's row, clear of the 820-wide capsule.
const SCOPE_X: f32 = 950.0;
/// Drawn whenever the query has nothing readable in it, focused or not. A `CStr` literal, so the
/// one string this module knows at compile time costs no per-frame allocation.
const PLACEHOLDER: &CStr = c"Search";

/// What a source is called before the roster has named it. The line still has to be a sentence —
/// an empty run in the middle of one reads as a rendering fault, not as a missing fact — and at
/// boot a name genuinely can be absent: `plex::describe_server` is fed by the plex.tv roster and by
/// a server naming itself, both of which land after the first frame this screen draws.
const UNNAMED_OWN: &str = "your server";
const UNNAMED_SHARE: &str = "a shared server";

pub(crate) fn draw(p: Painter, v: &View) {
    // Focused and editing are one FILL: the keyboard being up is the strongest possible statement
    // that this control has the remote, and a field that dimmed while you typed into it would be
    // saying the opposite.
    let hot = v.zone == Zone::Field || v.editing;
    let (fill, ink) =
        if hot { (theme::ACCENT, theme::ACCENT_INK) } else { (theme::CONTROL_IDLE_FILL, theme::CONTROL_IDLE_INK) };
    let rad = FIELD.h * 0.5;
    // The card system's resting shadow, the same one every pill in `widgets::Button` carries: an
    // ACCENT capsule with no shadow measures barely above its surround and the shape disappears,
    // leaving the dark label floating.
    p.shadow(FIELD, rad, theme::CARD_SHADOW_REST_BLUR, theme::CARD_SHADOW_REST_DY,
        theme::with_a(theme::CARD_SHADOW, theme::CARD_SHADOW_REST_A));
    p.rrect(FIELD, rad, rad, fill);

    let inner = Rect::new(FIELD.x + PAD_X, FIELD.y, FIELD.w - PAD_X * 2.0, FIELD.h);
    // Verbatim — `search::query` keeps the trailing space precisely because this is what draws it.
    // Truncated at a NUL first: keyboard input cannot contain one, but `enter` is also fed by a
    // boot trigger that reads a FILE, and `CString::new` failing there would blank the capsule
    // while leaving the Rust string non-empty — a field that refuses to show its own text.
    let q = crate::search::query();
    let q = &q[..q.find('\0').unwrap_or(q.len())];
    let cq = CString::new(q).unwrap_or_default();
    let run_w = crate::text::text_width(cq.as_ptr(), theme::size::BODY, 0);
    let (run_dx, caret_dx) = run_layout(run_w, inner.w, v.editing);

    // Scissor, so the head of an overlong run is CUT at the padding rather than sliding out over
    // the capsule's left arc. Global GL state — cleared before the scope line, which is outside it.
    p.clip(inner);
    // Whitespace is not a readable query: `search::draw` gates the whole screen below on the
    // TRIMMED string, so a lone typed space must not leave this capsule blank and hintless while
    // the rest of the screen says nothing has been asked.
    if q.trim().is_empty() {
        // The caret is the insertion point, so it precedes the hint rather than sitting under it.
        let px = if v.editing { inner.x + caret_dx + CARET_W + HINT_GAP } else { inner.x };
        let col = if hot { theme::ROW_VALUE_INK_ON_DIM } else { theme::TEXT_TERTIARY };
        Label::new(PLACEHOLDER.as_ptr(), theme::size::BODY, col).draw(p, Rect::new(px, inner.y, inner.w, inner.h));
    } else {
        Label::new(cq.as_ptr(), theme::size::BODY, ink)
            .draw(p, Rect::new(inner.x + run_dx, inner.y, inner.w, inner.h));
    }
    if v.editing {
        let cr = Rect::new(inner.x + caret_dx, FIELD.cy() - CARET_H * 0.5, CARET_W, CARET_H);
        p.rrect(cr, CARET_W * 0.5, CARET_W * 0.5, ink);
    }
    p.clip_clear();

    draw_scope(p);
}

/// Where the run and the caret sit, as offsets from the padded box's left edge.
///
/// The field shows the **tail**: while the run fits, it starts at the left edge and the caret
/// trails it; once it outgrows the box the whole run slides LEFT by the overflow, which parks the
/// caret's right edge exactly on the box's. `caret` reserves the caret's own block in that budget,
/// so the bar never hangs off the end of the field it belongs to.
///
/// The caret sits at the run's END with **no lead**, because that is the insertion point: the next
/// glyph is painted exactly where the bar is standing. A gap here would be visible as the caret
/// jumping backwards on the first keystroke of every search — the bar waiting 4px right of where
/// the letter then lands.
///
/// The returned caret offset is meaningful only when `caret` is set; the caller draws no bar
/// otherwise.
fn run_layout(run_w: f32, box_w: f32, caret: bool) -> (f32, f32) {
    let tail = if caret { CARET_W } else { 0.0 };
    let avail = (box_w - tail).max(0.0);
    let over = (run_w - avail).max(0.0);
    (-over, run_w - over)
}

/// One search SOURCE, as the scope line sees it. A borrowed projection of the server registry
/// (plus `browse`'s reachability), so the wording below can be tested without one.
struct Scope<'a> {
    /// the MACHINE name, `""` until the roster or the server itself has said one. Used for YOUR
    /// OWN server and nowhere else — see [`Scope::label`].
    name: &'a str,
    /// The LIBRARY titles this source contributed, which is what a SHARE is called here. Empty
    /// while the section table is still landing.
    libs: Vec<&'a str>,
    /// the owner's plex.tv handle; empty on your own server
    handle: &'a str,
    owned: bool,
    /// did it last answer? Optimistic — a source nobody has dialled has not failed.
    live: bool,
}

impl Scope<'_> {
    /// What to call it in a sentence. Never empty — see [`UNNAMED_OWN`].
    ///
    /// **The two sides are named at different LEVELS, and that is the design rather than an
    /// inconsistency.** Your own server is one thing you own and recognise by its machine name
    /// ("Gleb's Mac mini"); a share is experienced as the LIBRARY it gave you ("LDN Films"), and
    /// its hostname is a string that means nothing to anyone watching. `Search Screen.dc.html`
    /// writes exactly that pair, and `browse::BrowseSource::name` states the rule outright: the
    /// Sources list is "the only place in the app a machine is named".
    ///
    /// A share with several libraries joins them; a share whose section table has not landed yet
    /// falls back to [`UNNAMED_SHARE`], never to its hostname.
    fn label(&self) -> String {
        if !self.owned {
            // Blank titles are dropped rather than counted: a section the table holds but has not
            // titled yet would otherwise read as a library this share has.
            let named: Vec<&str> = self.libs.iter().copied().filter(|t| !t.is_empty()).collect();
            return match (named.len(), self.handle.is_empty()) {
                // One library IS the share, and is what the viewer recognises.
                (1, _) => named[0].to_string(),
                // Several, or none landed yet — say the PERSON. `Shared Sources.dc.html` states
                // the rule: the Sources list is "the one place a machine name is spoken, and the
                // reason the rest of the app can say only '{{ handle }}'". Listing someone's
                // libraries here would be a rail of rooms in a sentence about scope; the person is
                // the fact, and the Sources list is where their rooms are enumerated.
                (_, false) => self.handle.to_string(),
                (_, true) => UNNAMED_SHARE.to_string(),
            };
        }
        if self.name.is_empty() {
            UNNAMED_OWN.to_string()
        } else {
            self.name.to_string()
        }
    }
}

/// The scope line, or `None` when there is nothing to state.
///
/// Nothing is stated for a single source: the scope then IS the user's whole account, and a line
/// saying so on every search would be chrome that never changes. With two, the design's own two
/// shapes — all live, or one that is not, in which case the fact is which machine did not answer
/// and what you are therefore looking at. There is no warning tint and no Retry: re-kicking one
/// source is a control, and it lives on that section's Library screen.
///
/// The owner's handle is attributed only when there are two sources and **exactly one** of them is
/// a share with a handle — the design's own shape, where the `·` can only bind to the name in front
/// of it. Three sources, or two shares, and `"A, B · friend and C"` cannot be read unambiguously
/// (the `·` and the `and` compete, and a first-match pick would silently drop the second owner), so
/// the names stand alone and the Sources list stays where a share is attributed.
///
/// Deliberately not `fmt::shared_by`, which is a different phrase for a different place: that one
/// is the standalone `"Shared by friend"` sentence the hero meta line and the Library read-out
/// draw, and this is a bare handle hung off a `·` inside a longer statement.
fn scope_text(src: &[Scope]) -> Option<String> {
    if src.len() < 2 {
        return None;
    }
    let down: Vec<&Scope> = src.iter().filter(|s| !s.live).collect();
    let live: Vec<&Scope> = src.iter().filter(|s| s.live).collect();
    if down.is_empty() {
        let mut t = format!("Searching {}", name_set(&live));
        // The handle attributes ONE share. With two it would have to pick a winner, and with the
        // registry's sixteen slots the line stops being a sentence anyway — `name_set` has already
        // collapsed to a count by then, so there is nothing left for a handle to qualify.
        // `live.len() == 2` as well as "exactly one share with a handle": with three sources
        // the `·` lands at the END of a list and reads as attributing whichever name it follows.
        // "nas-home, A and B · ann" says B belongs to ann, and B may be someone else entirely.
        let mut shares = live.iter().filter(|s| !s.owned && !s.handle.is_empty());
        if let (2, Some(s), None) = (live.len(), shares.next(), shares.next()) {
            // …unless the share is ALREADY being called by their name, which is what a share with
            // several libraries falls back to. "bamx23 · bamx23" attributes a thing to itself.
            if s.label() != s.handle {
                t.push_str(" · ");
                t.push_str(s.handle);
            }
        }
        return Some(t);
    }
    if live.is_empty() {
        // Every source is down. "results from … only" has nothing to name, and the bare fact is
        // the honest line — the empty state below says the rest.
        return Some(format!("{} unreachable", name_set(&down)));
    }
    Some(format!("{} unreachable · results from {} only", name_set(&down), name_set(&live)))
}

/// Name a set of sources, collapsing to a COUNT once naming them all stops being a sentence.
///
/// `Search Screen.dc.html` designs one share ("Searching <your server> and <their library> ·
/// <handle>"), and enumerating is right at that size. It does not survive N: the registry holds
/// sixteen slots, each share can carry several libraries, and "A and B and C and D and E" is a
/// list rather than the "one plain fact" the design asks for — while the handle can only ever
/// attribute one of them.
///
/// So: up to [`NAME_LIMIT`] shares are named; past it the shares become a count and only your own
/// server keeps its name, because that is the part of the answer that does not change. Counting
/// LIBRARIES rather than servers is deliberate — a machine is not what anyone is searching, and it
/// is the unit the rest of this line already speaks in. Falls back to counting sources while the
/// section table is still landing and no library is named yet.
fn name_set(v: &[&Scope]) -> String {
    let (own, shares): (Vec<&&Scope>, Vec<&&Scope>) = v.iter().partition(|s| s.owned);
    if shares.len() <= NAME_LIMIT {
        let all: Vec<String> = v.iter().map(|s| s.label()).collect();
        return join(&all);
    }
    let libs: usize = shares.iter().map(|s| s.libs.iter().filter(|t| !t.is_empty()).count()).sum();
    let tail = if libs > 0 {
        format!("{libs} shared libraries")
    } else {
        format!("{} shared sources", shares.len())
    };
    let mut names: Vec<String> = own.iter().map(|s| s.label()).collect();
    names.push(tail);
    join(&names)
}

/// How many shares are named individually before the line collapses to a count.
///
/// **Two**, which is a compromise between two correct instincts. Naming them is more useful than
/// counting them and stays a readable sentence at this size — "nas-home, Film Club and Archive" —
/// so the enumerate-and-attribute-none behaviour is kept exactly where it works. Past it the line
/// stops being a fact and becomes a list: the registry holds sixteen slots, each share can carry
/// several libraries, and no handle can attribute more than one of them anyway.
///
/// It is a threshold and not a truth, so it is one constant with the reasoning on it rather than a
/// number spelled into `name_set`.
const NAME_LIMIT: usize = 2;

/// "A", "A and B", "A, B and C" — the one list idiom this line uses. Generic over anything
/// string-shaped so it serves both the SOURCE list and one share's library list.
fn join<S: AsRef<str>>(v: &[S]) -> String {
    let v: Vec<&str> = v.iter().map(AsRef::as_ref).collect();
    match v.len() {
        0 => String::new(),
        1 => v[0].to_string(),
        n => format!("{} and {}", v[..n - 1].join(", "), v[n - 1]),
    }
}

/// Read the registry, state the scope. The gate is [`crate::plex::server_count`], checked before
/// anything is collected so a one-server install pays nothing at all for this.
///
/// **Known staleness, bounded and deliberate.** This is the first per-frame DRAW to read
/// [`crate::plex::server_facts`], and neither `plex::describe_server` nor the auth worker that
/// feeds it calls [`crate::ui::idle::invalidate`] — so on a settled Search screen the roster
/// landing that turns "your server" into a real machine name is not presented until `idle`'s 2 s
/// keepalive fires. The right fix is an `invalidate` at `describe`, which is a shared-module edit
/// this unit deliberately does not make on its own; the fallback wording above is what keeps the
/// intervening frames a sentence rather than a hole.
fn draw_scope(p: Painter) {
    if crate::plex::server_count() < 2 {
        return;
    }
    let srcs = crate::browse::sources();
    // Registration order is the fallback for OWNERSHIP, and it is a documented invariant rather
    // than a guess: `app.rs` registers every share AFTER `plex::install`, "so slot 0 is always the
    // session's own server". It matters because a described server is the LATE state — until the
    // roster lands, defaulting `owned` to true would call a friend's machine "your server", and
    // would make the share fallback unreachable on the one path it was written for.
    let first = crate::plex::server_ids().next();
    let src: Vec<Scope> = crate::plex::server_ids()
        .map(|sid| {
            let f = crate::plex::server_facts(sid);
            Scope {
                name: f.map(|f| f.name.as_str()).unwrap_or(""),
                libs: crate::browse::library_titles(sid),
                handle: f.map(|f| f.handle.as_str()).unwrap_or(""),
                owned: f.map(|f| f.owned).unwrap_or(Some(sid) == first),
                // `browse`'s is the app's ONE notion of a source that has stopped answering. A
                // source its table has never adopted has not failed, so it reads as live.
                live: srcs.iter().find(|s| s.sid == sid).map(|s| s.reachable).unwrap_or(true),
            }
        })
        .collect();
    let Some(t) = scope_text(&src) else { return };
    // The last run on this row, so it is the one that gives way: elided to the app's own right
    // margin rather than allowed to run off the panel.
    let budget = crate::ui::consts::SCR_W - crate::ui::consts::MARGIN_X - SCOPE_X;
    let t = crate::text::elide(&t, budget, theme::size::CAPTION, 0, false);
    let ct = CString::new(t).unwrap_or_default();
    Label::new(ct.as_ptr(), theme::size::CAPTION, theme::TEXT_TERTIARY)
        .draw(p, Rect::new(SCOPE_X, FIELD.y, budget, FIELD.h));
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every name here is a PLACEHOLDER. This repository is public and a real share's machine name
    /// and account handle are a third party's — see the root `CLAUDE.md`.
    fn own(name: &str) -> Scope<'_> {
        Scope { name, libs: Vec::new(), handle: "", owned: true, live: true }
    }
    /// A share is named by its LIBRARY, so that is what the fixture takes — the machine name is
    /// carried only to prove the line never reaches for it.
    fn share<'a>(lib: &'a str, handle: &'a str) -> Scope<'a> {
        Scope { name: "a-hostname", libs: vec![lib], handle, owned: false, live: true }
    }
    fn down(mut s: Scope) -> Scope {
        s.live = false;
        s
    }

    #[test]
    fn a_run_that_fits_starts_at_the_edge_and_the_caret_trails_it() {
        let (run, caret) = run_layout(100.0, 756.0, true);
        assert_eq!(run, 0.0);
        assert_eq!(caret, 100.0);
    }

    #[test]
    fn an_overlong_run_slides_left_so_the_caret_lands_on_the_boxs_right_edge() {
        let box_w = 756.0;
        let (run, caret) = run_layout(900.0, box_w, true);
        assert!(run < 0.0, "the run must slide LEFT, not clip on the right");
        // the tail of the string is what stays on screen…
        assert_eq!(run + 900.0, box_w - CARET_W);
        // …and the caret's right edge is exactly the box's
        assert_eq!(caret + CARET_W, box_w);
    }

    #[test]
    fn with_no_caret_the_run_uses_the_whole_box() {
        let box_w = 756.0;
        let (run, _) = run_layout(900.0, box_w, false);
        assert_eq!(run + 900.0, box_w);
    }

    /// The insertion point, exactly: the bar stands where the first glyph will be painted, so it
    /// cannot appear to jump backwards on the first keystroke.
    #[test]
    fn an_empty_query_puts_the_caret_at_the_start_of_the_run() {
        let (run, caret) = run_layout(0.0, 756.0, true);
        assert_eq!(run, 0.0);
        assert_eq!(caret, 0.0);
    }

    /// The caret marks where the NEXT glyph lands, at every length — the property the whole
    /// left-clip exists to preserve.
    #[test]
    fn the_caret_is_the_insertion_point_whether_or_not_the_run_has_slid() {
        for run_w in [0.0, 1.0, 400.0, 753.0, 900.0, 5000.0] {
            let (run, caret) = run_layout(run_w, 756.0, true);
            assert_eq!(caret, run + run_w, "caret must sit at the run's end for run_w={run_w}");
        }
    }

    /// The degenerate box (a field narrower than its own caret) must not produce a negative
    /// budget, which would slide a short run left for no reason.
    #[test]
    fn a_box_too_narrow_for_the_caret_does_not_go_negative() {
        let (run, caret) = run_layout(0.0, 2.0, true);
        assert_eq!(run, 0.0);
        assert_eq!(caret, 0.0);
    }

    #[test]
    fn one_source_is_the_whole_account_so_nothing_is_stated() {
        assert_eq!(scope_text(&[own("nas-home")]), None);
        assert_eq!(scope_text(&[]), None);
    }

    #[test]
    fn two_live_sources_name_both_and_attribute_the_share() {
        assert_eq!(
            scope_text(&[own("nas-home"), share("Film Club", "friend")]).unwrap(),
            "Searching nas-home and Film Club · friend"
        );
    }

    /// **The two sides are named at different levels, and a share is never named by its machine.**
    /// `Search Screen.dc.html` writes "Searching &lt;your server&gt; and &lt;their library&gt; · &lt;handle&gt;", and
    /// `browse::BrowseSource::name` states the invariant: the Sources list is the only place in the
    /// app a machine is named. A hostname is a string that means nothing to someone watching.
    ///
    /// The fixture hands every share a machine name precisely so this can assert it never appears.
    #[test]
    fn a_share_is_named_by_its_library_and_never_by_its_hostname() {
        let t = scope_text(&[own("nas-home"), share("Film Club", "friend")]).unwrap();
        assert!(t.contains("Film Club"), "the share is its library: {t}");
        assert!(!t.contains("a-hostname"), "the share's MACHINE must not reach this line: {t}");
        // …while your own side is still the machine you recognise
        assert!(t.contains("nas-home"), "your own server keeps its name: {t}");

        // A share with SEVERAL libraries is called by its PERSON, not by a list of their rooms —
        // `Shared Sources.dc.html`: the Sources list is "the one place a machine name is spoken,
        // and the reason the rest of the app can say only '{{ handle }}'". Enumerating them here
        // would put a rail inside a sentence about scope.
        let mut two = share("Film Club", "friend");
        two.libs = vec!["Film Club", "Archive"];
        let t = scope_text(&[own("nas-home"), two]).unwrap();
        assert_eq!(t, "Searching nas-home and friend");
        // …and the attribution is not repeated onto itself: "friend · friend" attributes a thing
        // to the thing it already is.
        assert_eq!(t.matches("friend").count(), 1, "the handle appears once, not as its own suffix: {t}");
    }

    /// **Past a couple of shares the line stops naming and starts counting.** The registry holds
    /// sixteen slots; enumerating them is a list, not the "one plain fact" the design asks for, and
    /// the handle could attribute only one of them regardless. Libraries are what is counted — a
    /// machine is not what anyone is searching — and your own server keeps its name, because that
    /// is the part of the answer that does not change.
    #[test]
    fn many_shares_collapse_to_a_count_rather_than_becoming_a_list() {
        let mut mk = |lib: &'static str, h: &'static str| {
            let mut s = share(lib, h);
            s.libs = vec![lib];
            s
        };
        let many = vec![own("nas-home"), mk("A", "ann"), mk("B", "bob"), mk("C", "cat")];
        assert_eq!(scope_text(&many).unwrap(), "Searching nas-home and 3 shared libraries");

        // …and with the section tables still landing there are no library names to count, so it
        // counts what it does know rather than inventing one.
        let mut blank = vec![own("nas-home"), share("", "ann"), share("", "bob"), share("", "cat")];
        for s in blank.iter_mut() {
            if !s.owned {
                s.libs = Vec::new();
            }
        }
        assert_eq!(scope_text(&blank).unwrap(), "Searching nas-home and 3 shared sources");
    }

    #[test]
    fn a_share_with_no_handle_is_named_but_not_attributed() {
        assert_eq!(
            scope_text(&[own("nas-home"), share("Film Club", "")]).unwrap(),
            "Searching nas-home and Film Club"
        );
    }

    #[test]
    fn an_unreachable_source_names_itself_and_what_is_left() {
        assert_eq!(
            scope_text(&[own("nas-home"), down(share("Film Club", "friend"))]).unwrap(),
            "Film Club unreachable · results from nas-home only"
        );
    }

    /// The failure is stated about the SOURCE, whichever one it is — the own server is not exempt.
    #[test]
    fn your_own_server_going_quiet_reads_the_same_way() {
        assert_eq!(
            scope_text(&[down(own("nas-home")), share("Film Club", "friend")]).unwrap(),
            "nas-home unreachable · results from Film Club only"
        );
    }

    #[test]
    fn with_nothing_answering_there_is_no_results_from_clause_to_write() {
        assert_eq!(
            scope_text(&[down(own("nas-home")), down(share("Film Club", "friend"))]).unwrap(),
            "nas-home and Film Club unreachable"
        );
    }

    /// Three sources drop the attribution rather than write a `·` the reader cannot bind.
    #[test]
    fn three_sources_list_plainly_and_attribute_none_of_them() {
        assert_eq!(
            scope_text(&[own("nas-home"), share("Film Club", "friend"), share("Archive", "pal")]).unwrap(),
            "Searching nas-home, Film Club and Archive"
        );
    }

    /// Two SHARES and no server of your own: the `·` would have nothing it could bind to, and a
    /// first-match pick would attribute one owner and silently drop the other.
    #[test]
    fn two_shares_are_named_but_neither_is_attributed() {
        assert_eq!(
            scope_text(&[share("Film Club", "friend"), share("Archive", "pal")]).unwrap(),
            "Searching Film Club and Archive"
        );
    }

    /// Both halves of this are reachable states, not hypotheticals: before the roster lands,
    /// `draw_scope` builds every source with an empty name, taking `owned` from registration order
    /// — so slot 0 is `your server` and every later slot is `a shared server`.
    #[test]
    fn an_unnamed_source_still_produces_a_sentence() {
        assert_eq!(
            scope_text(&[own(""), share("Film Club", "friend")]).unwrap(),
            "Searching your server and Film Club · friend"
        );
        assert_eq!(scope_text(&[own("nas-home"), share("", "")]).unwrap(), "Searching nas-home and a shared server");
    }

    /// The whole-roster-unnamed boot frame: two sources, no facts for either. It must not call a
    /// friend's machine "your server" twice — the defect the registration-order default fixes.
    #[test]
    fn an_undescribed_roster_does_not_call_every_machine_yours() {
        let t = scope_text(&[own(""), share("", "")]).unwrap();
        assert_eq!(t, "Searching your server and a shared server");
    }
}
