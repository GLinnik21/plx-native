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
//! - The run scrolls so the **CARET** stays on screen — see [`run_layout`]. There is a real
//!   insertion point now (`super`'s `CARET`), because the television's own panel delegates its
//!   `◀`/`▶` keys to the app: the field is the thing that owns the text, so it is the thing that
//!   has to move the cursor. What there still is no route to is a POINTER cursor — no selection,
//!   no click-to-position.
//! - The caret is 3px × 34px and **blinks** (`super::BLINK_MS`), drawn only while `editing`.
//!   This doc argued the opposite for one release — solid, on the grounds that `ui::idle` cannot
//!   see a clock — and the argument was right about the mechanism and wrong about the conclusion:
//!   an animator that reports to the gate on its PHASE FLIP alone costs two presents a second, and
//!   a text field with a dead cursor reads on a television as a field that is not taking input.
//!   That was the first thing reported about this screen from the couch.
//! - The **scope line** sits at x=950 on the field's own row, CAPTION/tertiary, and is stated only
//!   when the scope is smaller than the user's whole account: one plain fact, no warning tint and
//!   no Retry (the retry that re-kicks one source lives on that section's Library).
//!
//! ## The two pieces of arithmetic worth extracting
//!
//! [`run_layout`] and [`scope_text`] are **pure**, and host-tested below, because both are the kind
//! of thing that is wrong by a few pixels or one word and invisible in a screenshot. They are not
//! the whole risk, though — [`draw`] and [`draw_scope`] carry the composition (the fill branch, the
//! scissor pair and three placements), and that is where a defect hides from both the tests and the
//! eye. The registry projection the scope line is written from is no longer part of that: it is
//! [`with_scope`], built once per change and shared with [`super::empty`] — see the section comment
//! above it for what that fixed.
#![allow(dead_code)]

use crate::ui::label::Label;
use crate::ui::search::{View, FIELD};
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
/// The one-character hint, and the air before it.
///
/// **It lives in the FIELD, which is the whole design decision.** One character in, the query has
/// not started ([`crate::search::MIN_QUERY`]) and the screen owes the user that fact — but the
/// region below still holds their recent terms, and replacing those with an instruction because a
/// key was pressed takes content away for pressing a key. So the hint sits where the eye already
/// is, right after the caret, in the placeholder's own dim ink, and is CONSUMED by the next
/// keystroke rather than dismissed.
///
/// (`Search Screen.dc.html` carries the alternative as a `minHint` prop — a `Searching starts at
/// two characters` line above the scope caption — and ships with this one as the default. The other
/// is not built: it puts a second sentence in a column that already has one, and the fact belongs
/// to the field.)
const GHOST: &CStr = c"one more character";
const GHOST_GAP: f32 = 14.0;
/// Where the scope line starts — its own column on the field's row, clear of the 820-wide capsule.
const SCOPE_X: f32 = 950.0;
/// …and how much room it gets: the rest of the row, out to the app's own right margin. It is the
/// LAST run on this row, so it is the one that gives way — elided to this rather than allowed to
/// run off the panel. A `const` because the elide (in [`build`]) and the frame it is drawn into (in
/// [`draw_scope`]) are now on opposite sides of the memo, and a budget measured in one place and
/// drawn in the other is how a run comes to be elided to a width nothing clips it at.
const SCOPE_W: f32 = crate::ui::consts::SCR_W - crate::ui::consts::MARGIN_X - SCOPE_X;
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
    // The two faces, CROSS-FADED on `super`'s focus spring rather than swapped — see `super::HOT`
    // for why this control owes the rest of the app that motion, and `theme::cross` for why it is
    // not `theme::mix` (both pairs differ in ALPHA as well as hue, and `mix` would land the
    // destination colour at the source's opacity).
    let hot = v.hot;
    let fill = theme::cross(theme::CONTROL_IDLE_FILL, theme::ACCENT, hot);
    let ink = theme::cross(theme::CONTROL_IDLE_INK, theme::ACCENT_INK, hot);
    // The placeholder and the one-character hint share one quiet role, on whichever face is under
    // them. They ride the same scalar, so nothing on this control can be half-swapped.
    let hint_ink = theme::cross(theme::TEXT_TERTIARY, theme::ROW_VALUE_INK_ON_DIM, hot);
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
    // How far into the run the insertion point sits: the width of the text BEFORE it. `v.caret` is
    // already clamped to a char boundary of the live query by `super::caret`, but this slice is
    // taken from the NUL-truncated copy above, so it is re-bounded here rather than trusted —
    // `enter` is fed by a file, and a panic in a draw is the app.
    let head = &q[..v.caret.min(q.len())];
    let ch = CString::new(head).unwrap_or_default();
    let caret_w = crate::text::text_width(ch.as_ptr(), theme::size::BODY, 0);
    let (run_dx, caret_dx) = run_layout(run_w, caret_w, inner.w, v.editing);

    // Scissor, so the head of an overlong run is CUT at the padding rather than sliding out over
    // the capsule's left arc. Global GL state — cleared before the scope line, which is outside it.
    p.clip(inner);
    // Whitespace is not a readable query: `search::draw` gates the whole screen below on the
    // TRIMMED string, so a lone typed space must not leave this capsule blank and hintless while
    // the rest of the screen says nothing has been asked.
    if q.trim().is_empty() {
        // The caret is the insertion point, so it precedes the hint rather than sitting under it.
        let px = if v.editing { inner.x + caret_dx + CARET_W + HINT_GAP } else { inner.x };
        Label::new(PLACEHOLDER.as_ptr(), theme::size::BODY, hint_ink).draw(p, Rect::new(px, inner.y, inner.w, inner.h));
    } else {
        Label::new(cq.as_ptr(), theme::size::BODY, ink)
            .draw(p, Rect::new(inner.x + run_dx, inner.y, inner.w, inner.h));
    }
    if v.editing && v.caret_on {
        let cr = Rect::new(inner.x + caret_dx, FIELD.cy() - CARET_H * 0.5, CARET_W, CARET_H);
        p.rrect(cr, CARET_W * 0.5, CARET_W * 0.5, ink);
    }
    // One character in, after the insertion point — see [`GHOST`]. Placed off the caret's own slot
    // whether or not a bar is drawn, so the run does not shuffle sideways when the panel closes.
    if ghost_shown(q) {
        Label::new(GHOST.as_ptr(), theme::size::BODY, hint_ink).draw(
            p,
            Rect::new(inner.x + caret_dx + CARET_W + GHOST_GAP, inner.y, inner.w, inner.h),
        );
    }
    p.clip_clear();

    draw_scope(p);
}

/// Is the one-character hint on? Exactly one character SHORT of the minimum, never at zero (an
/// empty field already says "Search", and two hints on one control is a control arguing with
/// itself) and never at the minimum (the query has started; the answer is the hint).
///
/// Written against `MIN_QUERY` rather than as `== 1` so it stays true if the minimum ever moves —
/// at three, "one more character" is right at two and wrong at one, which is a copy question this
/// predicate would then be the place to answer.
fn ghost_shown(q: &str) -> bool {
    let n = q.trim().chars().count();
    n > 0 && n + 1 == crate::search::MIN_QUERY
}

/// Where the run and the caret sit, as offsets from the padded box's left edge. `caret_w` is the
/// width of the text BEFORE the insertion point — 0 at the front of the run, `run_w` at its end.
///
/// **The scroll follows the CARET, not the tail.** While everything fits, the run starts at the
/// left edge and the bar stands wherever in it the insertion point is. Once it outgrows the box the
/// run slides left by just enough to bring the caret back inside — which for a caret at the end is
/// exactly the old "show the tail" behaviour, and for one stepped into the middle of a long term is
/// the only rule that keeps the thing you are editing visible. The old expression scrolled to the
/// tail unconditionally, which was correct while text could only ever be appended and put the
/// insertion point off the left edge the moment `◀` worked.
///
/// It is deliberately stateless — the shift is a function of this frame alone, so it cannot drift
/// out of step with the string — which costs one thing worth naming: stepping the caret left out of
/// view jumps it to the box's right edge rather than nudging the run by one character. A minimal
/// scroll needs the PREVIOUS shift, i.e. state, and the jump only happens past the first box-width
/// of a query nobody types on a remote.
///
/// `caret` reserves the bar's own block in the budget, so it never hangs off the end of the field
/// it belongs to. The returned caret offset is meaningful only when `caret` is set; the caller
/// draws no bar otherwise.
///
/// The bar sits AT the insertion point with no lead, because that is where the next glyph lands. A
/// gap would show as the caret jumping backwards on the first keystroke of every search.
fn run_layout(run_w: f32, caret_w: f32, box_w: f32, caret: bool) -> (f32, f32) {
    let tail = if caret { CARET_W } else { 0.0 };
    let avail = (box_w - tail).max(0.0);
    // Never scroll further than the run's own overflow: a short query must sit at the left edge
    // whatever the caret is doing, and the second clamp is what stops a caret at position 0 from
    // dragging an already-short run off the box.
    let over = (caret_w - avail).max(0.0).min((run_w - avail).max(0.0));
    (-over, caret_w - over)
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

/// The scope line, or `None` when there is no source to name at all.
///
/// **Stated for a SINGLE source too**, which this file argued against for one release: the scope is
/// then the user's whole account, so a line saying so never changes, and unchanging chrome is worth
/// deleting. Two things overturned it. `Search Screen.dc.html` makes the point that a row whose
/// right half empties out when a server is removed reads as a missing element rather than as a
/// simplification — the line is not chrome, it is the field's answer to the one question this
/// screen cannot show you, which is where you are looking. And the empty states no longer carry
/// the fact: `empty`'s "Titles, people and collections in <server>." was the single-source case's
/// only statement of scope anywhere, and the design deleted it as a second voice on one screen.
/// Suppressing this as well would leave a one-server install with no answer at all.
///
/// `None` survives for zero sources, where there is nothing true to say.
///
/// With two, the design's own two shapes — all live, or one that is not, in which case the fact is
/// which machine did not answer and what you are therefore looking at. There is no warning tint and
/// no Retry: re-kicking one source is a control, and it lives on that section's Library screen.
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
    if src.is_empty() {
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

// ---- the projection, read ONCE ---------------------------------------------------------------
//
// **Both regions of this screen now speak from one registry read.** The scope line here and
// `empty`'s "…in <server>." hint are two sentences about the same fact, and each used to build its
// own projection inside its own draw: this file joined `plex::server_ids` × `server_facts` ×
// `browse::sources()`'s reachability × `browse::library_titles` behind a `server_count() >= 2`
// gate, while `empty` counted `server_ids()` and never looked at reachability at all. Two joins
// over one registry is two answers to "what is a source", printed one line apart on the screen.
//
// It is MEMOISED for the second half of the same problem. Rebuilt inside a draw, that join cost a
// `Vec<Scope>`, a `Vec<&str>` per source (`library_titles` collects over the whole section table),
// two more from `name_set`'s partition, a `String` per label, `join`'s vec, a `format!`, `elide`'s
// String and a `CString` — ~16 allocations per PRESENTED frame, live on exactly the multi-source
// setup the line exists for, on the one screen that presents continuously while the user types,
// for a sentence that changes about twice a session. Both neighbouring regions already refused it
// — `recents` borrows where it could clone, and `empty`'s own count walked the roster rather than
// collecting it — which made this the outlier. `widgets::with_tab_metrics` is the in-tree pattern
// it follows: hold the result against a generation, rebuild only when the generation moves.

/// Everything the projection reads, in a form that can be compared every frame without touching
/// the heap. A key that misses an input is a STALE SENTENCE — the one failure the per-frame rebuild
/// could not have — so each field names what it stands in for.
#[derive(Clone, Copy, PartialEq, Debug)]
struct Key {
    /// registered slots: a share being added, and the count [`scope_text`] gates on
    servers: usize,
    /// `browse`'s section-table shape — the LIBRARY titles a share is NAMED by ([`Scope::label`])
    sections: u32,
    /// one bit per slot: did it last answer. `browse`'s flag flips mid-session, and it is the
    /// difference between "Searching …" and "… unreachable".
    live: u32,
    /// the published [`crate::plex::ServerFacts`] address per slot — name, handle and ownership.
    /// `describe_server` publishes a FRESH leaked record on every call and frees none of them
    /// (`plex::servers`' module doc: that leak is what makes its `&'static` sound), so the address
    /// IS that slot's description generation — it moves exactly when the description does, and two
    /// descriptions can never share one. The registry publishes no counter to read instead.
    facts: [usize; crate::plex::MAX_SERVERS],
}

impl Key {
    /// Allocation-free, and that is the whole point: this runs on every presented frame while the
    /// projection behind it does not.
    fn read() -> Key {
        let srcs = crate::browse::sources();
        let mut k = Key {
            servers: crate::plex::server_count(),
            sections: crate::browse::sections_gen(),
            live: 0,
            facts: [0; crate::plex::MAX_SERVERS],
        };
        // `server_ids` walks `0..server_count()` and registration is refused past `MAX_SERVERS`, so
        // `i` is always both a bit of `live` and an index of `facts`; `take` says so to the reader
        // rather than leaving it to be re-derived from two other modules.
        for (i, sid) in crate::plex::server_ids().enumerate().take(crate::plex::MAX_SERVERS) {
            // The same reading [`build`] uses: a source `browse`'s table has never adopted has not
            // failed, so an absent one is live. The two must agree or the line changes without the
            // key moving.
            if srcs.iter().find(|s| s.sid == sid).map(|s| s.reachable).unwrap_or(true) {
                k.live |= 1 << i;
            }
            k.facts[i] = crate::plex::server_facts(sid).map_or(0, |f| std::ptr::from_ref(f) as usize);
        }
        k
    }
}

/// What this screen says about SCOPE, projected from the registry once per change: the field's
/// line, and the two facts `empty`'s hint is written from.
struct ScopeState {
    key: Key,
    /// The line, elided to [`SCOPE_W`] and NUL-terminated — the exact bytes [`draw_scope`] hands to
    /// `Label`. `None` when there is nothing to state (see [`scope_text`]).
    line: Option<CString>,
    /// How many sources this screen SEARCHES. `crate::search` fans out one query per
    /// `plex::server_ids` slot and consults nothing else, so this is the whole registered roster
    /// and `browse`'s reachability is deliberately NOT folded into it: that flag is what section
    /// DISCOVERY last proved, a share that failed to list its libraries can still answer a search,
    /// and narrowing the hint on it would state a scope the fetch does not have.
    n: usize,
    /// The one source's MACHINE name, only when there is exactly one and something has named it.
    /// `&'static` because `server_facts` hands out a reference into a leaked record — there is
    /// nothing to clone.
    name: Option<&'static str>,
}

static mut SCOPE: Option<ScopeState> = None;

/// Run `f` with this screen's scope projection, rebuilding it only when [`Key`] moves.
///
/// Deliberately a closure and not a `&'static` getter, for `widgets::with_tab_metrics`' reason: the
/// borrow points into `SCOPE`, and a nested rebuild would free the `CString` a caller is still
/// handing to `Label` as a raw `*const c_char`. So do NOT call this again from inside `f`.
///
/// **A memo can only rebuild on a frame that PRESENTS**, so each input above owes an
/// [`crate::ui::idle::invalidate`] when it lands or the new sentence waits for `idle`'s 2 s
/// keepalive. All three that move mid-session do: `plex::servers::describe` (added for this line
/// exactly — the screen read "your server and your server" for two seconds after it knew better),
/// `browse::adopt_sections`, and `browse::mark_source_reachable`. That replaces a "known staleness,
/// bounded and deliberate" note this file carried saying the `describe` invalidate was still
/// missing; it landed, and a stale caveat is worse than none.
fn with_scope<R>(f: impl FnOnce(&ScopeState) -> R) -> R {
    let key = Key::read();
    // SAFETY: main thread only — every caller is a draw, and the UI draws on the SDL loop's thread.
    // Same terms as `widgets::TAB_CACHE`.
    let slot = unsafe { &mut *std::ptr::addr_of_mut!(SCOPE) };
    if slot.as_ref().map(|s| s.key != key).unwrap_or(true) {
        *slot = Some(build(key));
    }
    f(slot.as_ref().expect("just built"))
}

/// The join itself — everything that allocates, run only when [`Key`] says it must.
fn build(key: Key) -> ScopeState {
    let srcs = crate::browse::sources();
    // Registration order is the fallback for OWNERSHIP, and it is a documented invariant rather
    // than a guess: `app.rs` registers every share AFTER `plex::install`, "so slot 0 is always the
    // session's own server". It matters because a described server is the LATE state — until the
    // roster lands, defaulting `owned` to true would call a friend's machine "your server", and
    // would make the share fallback unreachable on the one path it was written for.
    let first = crate::plex::server_ids().next();
    let src: Vec<Scope<'static>> = crate::plex::server_ids()
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
    let line = scope_text(&src)
        .map(|t| CString::new(crate::text::elide(&t, SCOPE_W, theme::size::CAPTION, 0, false)).unwrap_or_default());
    // A name nothing has supplied yet is DROPPED rather than replaced — `super::empty::scope_hint`
    // then writes the sentence with no scope clause at all, which stays true whatever the roster
    // turns out to be. The fallbacks above ([`UNNAMED_OWN`]) belong to the LINE, which has to stay
    // a sentence with a hole in the middle of it; a hint with a whole clause to drop does not.
    let name = (src.len() == 1).then(|| src[0].name).filter(|s| !s.is_empty());
    ScopeState { key, line, n: src.len(), name }
}

/// How many sources this screen searches, and — only when there is exactly one — its machine name.
/// [`super::empty`]'s hint is written from these two and nothing else, so it can no longer
/// contradict the line [`draw_scope`] draws one row above it.
///
/// `empty` used to count this itself, as `server_ids().filter(|id| client_for(id).is_some())`. That
/// filter was **inert** and is not carried over: `plex::servers::register_lazy` publishes the slot
/// pointer and only *then* stores the count ("after the pointer: a visible count implies a live
/// slot"), `server_ids` walks `0..count`, and nothing in a shipped build ever nulls a slot back —
/// so every id it yields resolves to a `Client`. Keeping it implied a registered-but-undialable
/// state the registry does not permit, which is a worse thing to leave inside a projection than one
/// fewer branch.
pub(super) fn sources() -> (usize, Option<&'static str>) {
    with_scope(|s| (s.n, s.name))
}

/// State the scope, if there is anything to state. Everything this reads was resolved by
/// [`with_scope`], so the draw is one `Label` on one already-elided run.
fn draw_scope(p: Painter) {
    with_scope(|s| {
        let Some(line) = s.line.as_ref() else { return };
        Label::new(line.as_ptr(), theme::size::CAPTION, theme::TEXT_TERTIARY)
            .draw(p, Rect::new(SCOPE_X, FIELD.y, SCOPE_W, FIELD.h));
    });
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
        let (run, caret) = run_layout(100.0, 100.0, 756.0, true);
        assert_eq!(run, 0.0);
        assert_eq!(caret, 100.0);
    }

    #[test]
    fn an_overlong_run_slides_left_so_the_caret_lands_on_the_boxs_right_edge() {
        let box_w = 756.0;
        let (run, caret) = run_layout(900.0, 900.0, box_w, true);
        assert!(run < 0.0, "the run must slide LEFT, not clip on the right");
        // the tail of the string is what stays on screen…
        assert_eq!(run + 900.0, box_w - CARET_W);
        // …and the caret's right edge is exactly the box's
        assert_eq!(caret + CARET_W, box_w);
    }

    #[test]
    fn with_no_caret_the_run_uses_the_whole_box() {
        let box_w = 756.0;
        let (run, _) = run_layout(900.0, 900.0, box_w, false);
        assert_eq!(run + 900.0, box_w);
    }

    /// The insertion point, exactly: the bar stands where the first glyph will be painted, so it
    /// cannot appear to jump backwards on the first keystroke.
    #[test]
    fn an_empty_query_puts_the_caret_at_the_start_of_the_run() {
        let (run, caret) = run_layout(0.0, 0.0, 756.0, true);
        assert_eq!(run, 0.0);
        assert_eq!(caret, 0.0);
    }

    /// The caret marks where the NEXT glyph lands, at every length — the property the whole
    /// left-clip exists to preserve. (With the caret at the run's END, which is where typing keeps
    /// it; the cases where it is not are the test below.)
    #[test]
    fn the_caret_is_the_insertion_point_whether_or_not_the_run_has_slid() {
        for run_w in [0.0, 1.0, 400.0, 753.0, 900.0, 5000.0] {
            let (run, caret) = run_layout(run_w, run_w, 756.0, true);
            assert_eq!(caret, run + run_w, "caret must sit at the run's end for run_w={run_w}");
        }
    }

    /// **The `◀`/`▶` case, which is what the second argument exists for.** The panel moves the
    /// insertion point into the middle of a run that is wider than the box, and the field has to
    /// scroll to it — the whole point of editing a term you have already typed. Before the caret
    /// was a real position this scrolled to the TAIL unconditionally, so stepping left walked the
    /// bar straight off the left edge of the capsule.
    #[test]
    fn the_run_scrolls_to_the_caret_rather_than_to_the_tail() {
        let box_w = 756.0;
        let avail = box_w - CARET_W;
        // the caret at the FRONT of a long run: no scroll at all, and the tail is what is cut
        let (run, caret) = run_layout(2000.0, 0.0, box_w, true);
        assert_eq!((run, caret), (0.0, 0.0), "a caret at the front pins the run to the left edge");
        // …stepped just inside the box: still no scroll, the bar simply stands further along
        let (run, caret) = run_layout(2000.0, avail - 10.0, box_w, true);
        assert_eq!((run, caret), (0.0, avail - 10.0));
        // …and past it: the run slides by exactly the overflow, parking the bar on the right edge
        let (run, caret) = run_layout(2000.0, 1000.0, box_w, true);
        assert_eq!(run, -(1000.0 - avail), "the run slides to bring the caret back inside");
        assert_eq!(caret + CARET_W, box_w);
        // the caret is always drawn INSIDE the box, at every position in a very long query
        for c in [0.0, 1.0, 500.0, 1999.0, 2000.0] {
            let (_, caret) = run_layout(2000.0, c, box_w, true);
            assert!((0.0..=avail).contains(&caret), "caret at {c} drew at {caret}, outside the field");
        }
    }

    /// The degenerate box (a field narrower than its own caret) must not produce a negative
    /// budget, which would slide a short run left for no reason.
    #[test]
    fn a_box_too_narrow_for_the_caret_does_not_go_negative() {
        let (run, caret) = run_layout(0.0, 0.0, 2.0, true);
        assert_eq!(run, 0.0);
        assert_eq!(caret, 0.0);
    }

    /// **One source is still named** — the rule reversed on 2026-08-15, and worth a test rather
    /// than a deletion because the old one asserted the opposite for a stated reason. The line is
    /// the field's answer to "where am I looking", `empty` no longer carries that fact for the
    /// single-source case, and a right half that empties out when a server is removed reads as a
    /// missing element. Only "no sources at all" is silent, because there is nothing true to say.
    #[test]
    fn one_source_is_named_and_only_no_source_is_silent() {
        assert_eq!(scope_text(&[own("nas-home")]).unwrap(), "Searching nas-home");
        // …by its MACHINE name, which is what your own server is recognised by (`Scope::label`).
        assert_eq!(scope_text(&[own("")]).unwrap(), "Searching your server", "never a blank in a sentence");
        // A lone SHARE is named by its library, and carries no handle: there is no second name for
        // the `·` to separate it from, and "Film Club · friend" beside a one-source install states
        // an attribution nothing on the screen contrasts with.
        assert_eq!(scope_text(&[share("Film Club", "friend")]).unwrap(), "Searching Film Club");
        // Down, alone: the bare fact, with nothing left to say results are coming from instead.
        assert_eq!(scope_text(&[down(own("nas-home"))]).unwrap(), "nas-home unreachable");
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

    // ---- the memo ------------------------------------------------------------------------------

    /// Empty the registry AND `browse`'s source table around a test that needs real entries in
    /// both, and hand both back empty — the discipline `plex::servers`' own tests document: a
    /// client left registered at a port that closed is one another module's pump will dial on a
    /// background thread, and a seeded source table is a fixture the Library screen's own tests
    /// would otherwise inherit.
    struct FreshRegistry(#[allow(dead_code)] std::sync::MutexGuard<'static, ()>);
    impl Drop for FreshRegistry {
        fn drop(&mut self) {
            crate::browse::reset();
            crate::plex::reset_servers_for_test();
        }
    }
    fn fresh_registry() -> FreshRegistry {
        let g = crate::testlock::serial();
        crate::plex::reset_servers_for_test();
        FreshRegistry(g)
    }

    /// **The key must move for every input the projection reads**, because one it misses is a
    /// sentence that stays on screen after it stopped being true — the single failure mode the
    /// per-frame rebuild this replaced could not have. Two of the four are graded here, and they
    /// are the two no counter states outright: `browse`'s reachability flag (which flips
    /// mid-session, and is the difference between "Searching …" and "… unreachable"), and the FACTS
    /// address, which is a generation only because `plex::servers` never frees a description.
    ///
    /// The other two are read straight out of the counters that define them (`server_count` and the
    /// section table's own generation) and cannot drift from what they stand for.
    ///
    /// This grades [`Key::read`] and never [`with_scope`]: building the projection measures and
    /// elides text, and there is no font on the host tier.
    #[test]
    fn the_memo_key_moves_when_a_source_goes_quiet_or_is_described() {
        let _g = fresh_registry();
        // `register_for_test`, not the public `register`: the latter resolves the device id through
        // `session::load`, which mints and PERSISTS a uuid a host test has no business writing, and
        // it spawns a `serverinfo` worker against the fixture address.
        let own = crate::plex::register_for_test("mach-own", "10.0.0.1", 32400, "tok", "cid-field-test");
        crate::plex::register_for_test("mach-friend", "10.0.0.2", 32400, "tok", "cid-field-test");

        crate::browse::seed_sources_for_test(2, true);
        let live = Key::read();
        assert_eq!(live.servers, 2, "both registered slots are sources");
        assert_eq!(live.live, 0b11, "…and both last answered");

        // A share going quiet is `browse`'s flag alone. The seed also resets the section table, so
        // the whole key moves for two reasons — the BIT is what this pins.
        crate::browse::seed_sources_for_test(2, false);
        assert_eq!(Key::read().live, 0, "a source that stopped answering must move the key");

        // …and the roster naming a machine moves it through the facts address on its own: nothing
        // else about the registry or the table changed, and the line goes from "your server" to a
        // name the user recognises.
        crate::browse::seed_sources_for_test(2, true);
        let before = Key::read();
        crate::plex::describe_server(own, "nas-home", "", true);
        let after = Key::read();
        assert_ne!(before, after, "a description landing must rebuild the line that speaks it");
        assert_eq!(
            (after.servers, after.live, after.sections),
            (before.servers, before.live, before.sections),
            "…through the facts address, which is the only input that changed"
        );
    }
}
