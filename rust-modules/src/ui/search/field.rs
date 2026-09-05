//! The query LINE, its caret, and the scope block under it.
//!
//! ## The design, so it does not have to be re-derived from the mock
//!
//! - The field carries **no magnifier**. The pill above it is the mark, and repeating it here says
//!   nothing the screen has not already said.
//! - **There is no RIM, no rule, no capsule OUTLINE, and — since 2026-09-04 — no FILL either.** The
//!   query IS the type, at `size::HERO` on the flat ground; the design system's `SearchField`
//!   contract forbids a border on this control and that half never changed. It wore a full control
//!   face until 2026-08-30 (`ACCENT` focused, `CONTROL_IDLE_FILL` holding a query while focus was
//!   elsewhere), then briefly a flat `ACCENT` PLATE for one state only (issue 22, 2026-09-03) — and
//!   the owner rejected that plate on sight: "you made the search bar background white, while I
//!   wanted text to be white". The plate is gone for good. **Focus is carried by INK ALONE,
//!   permanently** — the design system's own `SearchField` contract, which the plate never actually
//!   had authority to override.
//! - Two focused states, distinguished by ink rather than by a face. `draw`'s `ink_target` picks
//!   the query's bright endpoint on `editing` alone — never a second spring, riding the SAME `hot`
//!   spring the ink always has either way. **Focused, not yet editing** — no caret, no keyboard, and
//!   now no plate to help — crosses to [`theme::FIELD_WAITING_INK`], a genuinely PURE white: this is
//!   what "I wanted text to be white" means once there is nothing else on screen to carry the
//!   signal. **Editing** drops back one stop, to [`theme::FIELD_EDITING_INK`] — the SAME near-white
//!   `TEXT_PRIMARY` stop this control's ink crossed to before issue 22 ever existed, and the caret's
//!   own colour, so glyph and bar read as one continuous line — because the blinking caret and the
//!   open keyboard already carry "you are editing this", and the brighter white is left for the
//!   state that has nothing else. Until 2026-09-02 the whole ink step was `TEXT_SECONDARY`→
//!   `TEXT_HEADING`, one stop, and from the couch it read as "this field has content" rather than
//!   "the remote is on it" (owner feedback) — which is the wide step both targets still ride today.
//!   The flip between the two targets is discrete, not a fade of its own, for the same reason the
//!   short-lived plate's `fill_t` never faded on `editing`: the keyboard raising is a discrete
//!   event, not a spring one. The placeholder stays one step behind the query ink at every `hot`,
//!   unconditionally on `editing` (`TEXT_TERTIARY`→`TEXT_SECONDARY`) — it is never shown once a real
//!   query needs the brighter ramp to itself.
//! - The placeholder is **unconditional**: an empty field is "Search your library" at the same
//!   rung in `TEXT_TERTIARY` — a step back from either query ink — never a bare caret on an empty
//!   row. **The caret sits at the field's own START while it is showing over a blank field**
//!   (issue 20): the placeholder is chrome nobody is editing, so it supplies no insertion point of
//!   its own to trail — see [`draw`]'s blank branch, which feeds `run_layout` a `caret_w` of zero
//!   rather than the placeholder's own width.
//! - The run scrolls so the **CARET** stays on screen — see [`run_layout`]. There is a real
//!   insertion point now (`super`'s `CARET`), because the television's own panel delegates its
//!   `◀`/`▶` keys to the app: the field is the thing that owns the text, so it is the thing that
//!   has to move the cursor. What there still is no route to is a POINTER cursor — no selection,
//!   no click-to-position.
//! - The caret is **5px wide and one CAP BAND tall**, sitting on the run's baseline, drawn only
//!   while `editing`, and **blinks**. This deliberately differs from the current `SearchField`
//!   component's solid bar: on the television a dead cursor read as a field that was not accepting
//!   input. `super::step_blink` reports only phase flips, so the open keyboard costs about two
//!   presents a second and a finished screen with the keyboard down stays idle.
//! - One character short of [`crate::search::MIN_QUERY`], the field adds **"one more character"**
//!   at `size::BODY` after the caret. It stays with the control whose state it explains and is
//!   consumed by the next keystroke; the recent terms below do not disappear for pressing a key.
//! - The **scope line** sits UNDER the query at `super::SCOPE_Y`, left-aligned on the app's own
//!   margin — a 72px line has no right-hand half to put a caption in. It is stated in EVERY state,
//!   one source included: this is the one screen where you cannot see what you are looking at. One
//!   plain fact, no warning tint and no Retry (the retry that re-kicks one source lives on that
//!   section's Library).
//!
//! ## The two pieces of arithmetic worth extracting
//!
//! [`run_layout`], [`run_and_head`], [`run_caret_w`], [`descent_pad`], [`ink_target`] and
//! [`scope_text`] are **pure**, and host-tested below, because
//! each is the kind of thing that is wrong by a few pixels, one word or one byte and invisible in a
//! screenshot — and [`run_and_head`]'s failure is not cosmetic at all, since a slice cut off a char
//! boundary panics inside the SDL event loop. They are not the whole risk, though — [`draw`] and
//! [`draw_scope`] carry the composition (the scissor pair and four placements),
//! and that is where a defect hides from both the tests and the eye. The registry projection the
//! scope line is written from is no longer part of that: it is [`with_scope`], built once per change
//! and shared with [`super::empty`] — see the section comment above it for what that fixed.
#![allow(dead_code)]

use crate::ui::label::Label;
use crate::ui::search::{View, FIELD};
use crate::ui::{theme, Painter, Rect};
use std::ffi::{CStr, CString};
use std::os::raw::c_int;

/// The caret's width, from the design. Its HEIGHT is not a constant: it is the run's own cap band
/// (`text::text_cap_band` at [`RUN_SZ`]), which is what "52 of 72" in the design means and what
/// keeps the bar the height of the letters beside it if the face ever changes.
const CARET_W: f32 = 5.0;
/// The design's air between a run and its caret: 8px at the 72px HERO rung.
const CARET_GAP: f32 = 8.0;
/// The one-character instruction, drawn inside the field after the caret.
const GHOST: &CStr = c"one more character";
/// One BODY-sized rung between the caret and the instruction beside it.
const GHOST_GAP: f32 = 28.0;
/// The QUERY's own rung. `HERO` is the app's largest, and this line is the largest thing on the
/// screen by design — what the user typed is the subject of the page.
const RUN_SZ: c_int = theme::size::HERO;
/// The scope block's own height: exactly one caption line. The minimum-query instruction belongs
/// to the field above, so reserving a second line here would move the scope away from its token.
/// [`super::HEAD_BOTTOM`] is measured off it, so this is the number that says where the app's
/// chrome stops and the result band begins.
pub(crate) const SCOPE_H: f32 = theme::size::CAPTION as f32 * 1.35;
/// …and how much room it gets: the rest of the row, out to the app's own right margin. It is the
/// LAST run on this row, so it is the one that gives way — elided to this rather than allowed to
/// run off the panel. A `const` because the elide (in [`build`]) and the frame it is drawn into (in
/// [`draw_scope`]) are now on opposite sides of the memo, and a budget measured in one place and
/// drawn in the other is how a run comes to be elided to a width nothing clips it at.
const SCOPE_W: f32 = FIELD.w;
/// Drawn whenever the query has nothing readable in it, focused or not. A `CStr` literal, so the
/// one string this module knows at compile time costs no per-frame allocation.
const PLACEHOLDER: &CStr = c"Search your library";

/// What a source is called before the roster has named it. The line still has to be a sentence —
/// an empty run in the middle of one reads as a rendering fault, not as a missing fact — and at
/// boot a name genuinely can be absent: `plex::describe_server` is fed by the plex.tv roster and by
/// a server naming itself, both of which land after the first frame this screen draws.
const UNNAMED_OWN: &str = "your server";
const UNNAMED_SHARE: &str = "a shared server";

pub(crate) fn draw(p: Painter, v: &View) {
    // **The field is IN the document** (2026-09-05). It was pinned — drawn at a fixed `FIELD.y`
    // over a scrim that dissolved the shelves as they rose behind it — and now it rides the one
    // scroll everything else on this screen rides, exactly as the Library's chip and control row
    // do. Everything below is authored in FLOW coordinates against `FIELD`/`super::SCOPE_Y` and
    // this one translate is what puts it on the panel, `p.clip` included (the cascade translate is
    // folded into the scissor). `super::hit` applies the same shift to the field's hit rect.
    //
    // At `shift == 0` the result is byte-identical to the pinned draw, which is the whole of the
    // screen the keyboard is up over: `super::scroll_frozen` holds the flow at zero while editing.
    let p = p.translate(0.0, -v.shift);
    // **Focus is carried by ink alone — permanently.** Issue 22's flat `ACCENT` plate lived here for
    // one day (2026-09-03) and the owner rejected it on sight: "you made the search bar background
    // white, while I wanted text to be white". It is gone, along with the discrete-flip machinery
    // that only existed to keep an ink ramp legible against a moving plate — there is no plate to
    // land within a couch-invisible luma of any more, so the ink below is a single continuous
    // `theme::cross` on `super::HOT`, exactly as it was before issue 22 ever existed.
    //
    // The ink is CROSS-FADED on `super`'s focus spring rather than swapped — see `super::HOT` for
    // why this control owes the rest of the app that motion, and `theme::cross` for why it is not
    // `theme::mix` (the pair differs in ALPHA as well as hue, and `mix` would land the destination
    // colour at the source's opacity).
    //
    // **Two bright endpoints, picked by `editing` alone — never a second spring.** [`ink_target`]
    // is the whole of it: focused-not-editing crosses toward [`theme::FIELD_WAITING_INK`], a genuinely
    // pure white, because that state has no caret and no keyboard to help it read as "the remote is
    // on this" — editing crosses toward [`theme::FIELD_EDITING_INK`] instead, one stop dimmer and the
    // caret's own colour, because the blinking bar and the open keyboard already carry that. The
    // flip is discrete rather than a fade of its own — the keyboard raising is a discrete event, not
    // a spring one, the same reason issue 22's now-deleted `fill_t` never faded on `editing` either.
    let hot = v.hot;
    let ink = theme::cross(theme::TEXT_SECONDARY, ink_target(v.editing), hot);
    // The placeholder rides the same spring one step BEHIND the query ink at every `hot`, so it
    // never reads as bright as a real query — that is what makes it a placeholder rather than a dim
    // query — but it brightens with focus too, so an EMPTY focused field also says so. Unconditional
    // on `editing`: the placeholder only ever draws while the field is blank (see `blank` below),
    // and a blank field's caret sits at its own start rather than trailing the hint, so the two
    // ink ramps never have to agree with each other the way the query's and the fill's once did.
    let hint_ink = theme::cross(theme::TEXT_TERTIARY, theme::TEXT_SECONDARY, hot);

    let inner = FIELD;
    // Verbatim — `search::query` keeps the trailing space precisely because this is what draws it.
    // The two cuts [`run_and_head`] makes (the NUL, and the caret back to a char boundary) are
    // argued there.
    let (q, head) = run_and_head(crate::search::query(), v.caret);
    let blank = q.trim().is_empty();
    let cq = CString::new(q).unwrap_or_default();
    let ch = CString::new(head).unwrap_or_default();
    // An empty field draws the placeholder as its run — so a short query does not jump when it
    // replaces the hint — but the CARET sits at the field's own START, not after it (issue 20): see
    // [`run_caret_w`].
    let (run_w, caret_w) = if blank {
        (
            crate::text::text_width(PLACEHOLDER.as_ptr(), RUN_SZ, 1),
            run_caret_w(true, 0.0),
        )
    } else {
        let head_w = crate::text::text_width(ch.as_ptr(), RUN_SZ, 1);
        (
            crate::text::text_width(cq.as_ptr(), RUN_SZ, 1),
            run_caret_w(false, head_w),
        )
    };
    let (run_dx, caret_dx) = run_layout(run_w, caret_w, inner.w, v.editing);

    // The box the run paints AND clips into. [`FIELD`]'s own 80px height is fit to the cap band
    // alone ("the box a 72px run sits in", not the glyphs' full extent). `Label`'s `VAlign::Middle`
    // centres only the CAP BAND on that 80px frame's own middle (`text::text_vcenter_y`'s doc) — it
    // does NOT centre the run's full glyph box, but because the cap band sits well above that box's
    // own centre (there is no ascender clearance to match the descent), centring the cap band alone
    // already pushes a good deal of the full box's lower half inside the 80px frame for free. Issue
    // 21's fix round 1 missed exactly that: padding by the WHOLE descent (`full_h - cap_base`)
    // double-counted this free clearance and overshot by ~13px. [`descent_pad`] now takes the
    // frame's own height too and returns exactly the shortfall past what the cap-band-centred draw
    // origin already clears — Codex's review round 1 derived the formula
    // (`full_h − box_h⁄2 − (cap_top+cap_base)⁄2`). It grows only the CLIP now — the short-lived
    // issue-22 plate that once shared this box is gone (2026-09-04).
    //
    // **This DOES leave a sub-4px sliver where the painted clip runs past `FIELD`'s own hit rect**
    // (`search::mod::hit`'s `FIELD.contains`, unchanged and deliberately not coupled to a live font
    // metric): a click landing in that sliver — past `FIELD`'s own bottom edge but still inside the
    // clipped glyph's own descent — reads as a miss rather than a field hit. Left as a known,
    // recorded gap rather than silently ignored: the pad is small (≈3.5px on the shipped face) and
    // growing `FIELD` itself was rejected, because `SCOPE_Y`/`HEAD_BOTTOM` are measured off it and
    // moving them for a font-metric sliver is a bigger change than this bug earns.
    let (cap_top, cap_base) = crate::text::text_cap_band(RUN_SZ, 1);
    let pad = descent_pad(
        inner.h,
        crate::text::text_height(RUN_SZ, 1),
        cap_top,
        cap_base,
    );
    let field_box = Rect::new(inner.x, inner.y, inner.w, inner.h + pad);

    // Scissor, so the head of an overlong run is CUT at the app's own margin rather than sliding
    // out across it. With the capsule gone this is the ONLY thing bounding the line, which is why
    // `FIELD` (grown by the descent pad above) survives as a rect at all. Global GL state — cleared
    // before the scope block below.
    p.clip(field_box);
    // Whitespace is not a readable query: `search::draw` gates the whole screen below on the
    // TRIMMED string, so a lone typed space must not leave this line blank and hintless while
    // the rest of the screen says nothing has been asked.
    if blank {
        Label::new(PLACEHOLDER.as_ptr(), RUN_SZ, hint_ink)
            .bold()
            .draw(p, Rect::new(inner.x + run_dx, inner.y, inner.w, inner.h));
    } else {
        Label::new(cq.as_ptr(), RUN_SZ, ink)
            .bold()
            .draw(p, Rect::new(inner.x + run_dx, inner.y, inner.w, inner.h));
    }
    if caret_shown(v.editing, v.caret_on) {
        // One CAP BAND tall, sitting on the run's own baseline — the design's "52 of 72", derived
        // from the face rather than transcribed, so it stays the height of the letters beside it.
        // Square, not rounded: at 5px a radius is a lozenge, and the design draws a bar. Always
        // `TEXT_PRIMARY`: the caret only ever shows while `editing`, which is exactly when
        // [`ink_target`] has the query ink riding toward that SAME stop — `theme::FIELD_EDITING_INK`
        // is `TEXT_PRIMARY` — so caret and glyph read as one continuous line rather than two colours.
        let ty = crate::text::text_vcenter_y(RUN_SZ, 1, inner.cy());
        let cr = Rect::new(inner.x + caret_dx, ty + cap_top, CARET_W, cap_base - cap_top);
        p.rect(cr, 0.0, theme::TEXT_PRIMARY, theme::TEXT_PRIMARY, 0.0);
    }
    // One character in, after the insertion point — the component's BODY-sized instruction. It is
    // placed off the caret's own slot whether or not the bar is drawn, so dismissing the panel does
    // not shuffle the line. Different sizes share the HERO run's baseline, never their top edges.
    if ghost_shown(q) {
        let run_y = crate::text::text_vcenter_y(RUN_SZ, 1, inner.cy());
        let ghost_y = crate::text::baseline_y(theme::size::BODY, 0, RUN_SZ, 1, run_y);
        p.text(
            GHOST.as_ptr(),
            inner.x + caret_dx + CARET_W + GHOST_GAP,
            ghost_y,
            theme::size::BODY,
            hint_ink,
            0,
            0,
        );
    }
    p.clip_clear();

    draw_scope(p);
}

/// The run this field DRAWS, and the slice of it that sits before the insertion point — the two
/// slices [`draw`] measures, CUT once so the two cuts can never drift apart. (They are still
/// MEASURED separately, which is a different thing and not fixed here: `head` is shaped on its own,
/// so a kern pair straddling the insertion point is applied to the run and not to the head, and the
/// bar can sit a pixel or so off where the next glyph lands. Inter's `kern` is deliberately kept —
/// `tools/cut-inter.py`, and `docs/agent-reference.md` calls it load-bearing — so this is real, but it
/// is a sub-pixel placement question that only a panel can judge, and `text_width` cannot be
/// reached from the host suite at all.)
///
/// Pure, and the third thing in this file that is (see the module doc), because **every expression
/// in it is a slice and every one of them is in a DRAW**: a bad index here does not mis-place a
/// pixel, it panics inside the SDL event loop and takes the app down — the class `remote.rs` lost
/// the app to once already, off a multi-byte separator.
///
/// Two cuts, and neither may be assumed away:
///
/// 1. **At the first NUL.** Keyboard input cannot contain one, and `super::mount` already filters
///    every control character out of the boot trigger's seed — so this is the BELT over that brace,
///    not the only guard, and neither may be deleted on the strength of the other. It is kept
///    because the failure it prevents is silent and total: `CString::new` refuses a string with an
///    interior NUL, so the capsule would blank while the Rust string stayed non-empty — a field
///    refusing to show its own text — and the caret would be placed against the wrong length.
/// 2. **The caret, back to a CHAR BOUNDARY of the run** — not merely `min(len)`, which is the shape
///    this was written as and the reason it is now a function. `min` bounds the LENGTH and says
///    nothing about the boundary, so it is only sound while the incoming offset is already a
///    boundary of *this* slice. `super::caret` does clamp against the live query, so nothing in the
///    tree reaches the bad case today — but the two strings are not the same string whenever the
///    query holds a NUL, the guarantee is one another module owns, and the comment that used to
///    stand here claimed the re-bound as though it had been done. A defence that is documented and
///    absent is worse than one that is neither.
fn run_and_head(q: &str, caret: usize) -> (&str, &str) {
    let run = &q[..q.find('\0').unwrap_or(q.len())];
    let mut c = caret.min(run.len());
    while c > 0 && !run.is_char_boundary(c) {
        c -= 1;
    }
    (run, &run[..c])
}

/// The width of the run BEFORE the insertion point that `draw` feeds [`run_layout`] as `caret_w` —
/// issue 20. A blank field's caret is always `0.0`, whatever `head_w` would otherwise measure: the
/// placeholder is chrome nobody is editing, so it supplies no insertion point of its own to trail. A
/// real query's caret is its own measured HEAD width, unchanged. Extracted into its own function
/// (rather than left as `draw`'s inline `if`) so a host test calls the exact decision `draw` makes;
/// Codex review round 1 noted the PREVIOUS test only re-asserted the geometry `run_layout` produces
/// from a hand-supplied `0.0`, which could not have caught a regression in which VALUE `draw` chose
/// to pass.
fn run_caret_w(blank: bool, head_w: f32) -> f32 {
    if blank {
        0.0
    } else {
        head_w
    }
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

/// The bar belongs to BOTH states: the keyboard must be up, and the blink must be in its ON phase.
/// Kept as a pure predicate so a future design pass cannot silently turn the intentional blink
/// back into an always-on bar by dropping `View::caret_on` from the draw condition.
fn caret_shown(editing: bool, phase_on: bool) -> bool {
    editing && phase_on
}

/// How far past [`FIELD`]'s own box the run's clip (and fill) must reach to hold the WHOLE glyph —
/// issue 21. `box_h` is that box's height (`FIELD.h`); `full_h` is the run's own
/// [`crate::text::text_height`] (the font's full line box, string-independent — ascent plus
/// descent, the SAME reading a digit or a capital gets); `cap_top`/`cap_base` are
/// [`crate::text::text_cap_band`]'s own offsets.
///
/// **Not simply `full_h - cap_base`** — that was this function's first shape, and it double-counts
/// clearance `Label`'s `VAlign::Middle` already gives away for free. `VAlign::Middle` centres only
/// the CAP BAND on `box_h`'s own middle (`text::text_vcenter_y`), never the run's full glyph box —
/// but the cap band sits well above that full box's own centre (nothing balances the descent on the
/// ascent side), so centring the cap band alone already pushes part of the box's lower half inside
/// `box_h` for free: `box_h/2 - (cap_top+cap_base)/2` of it, below the frame's own vertical centre.
/// What is left to reserve is `full_h`'s own reach PAST that centre, minus what is already covered.
/// Caught in Codex review round 1: the untrimmed formula padded FIELD's 80px box by the whole
/// ≈17px descent when the cap-band-centred draw origin already covered all but ≈3.5px of it,
/// overshooting enough to run the fill into the scope line's own row below.
/// `.max(0.0)` only matters for a hypothetical face whose line box does not reach past that centre
/// at all, which no shipped face here does — kept because a negative pad would SHRINK the clip
/// instead of leaving it alone.
///
/// Pure, so the arithmetic is pinned without a font: `draw` supplies the three live metric reads,
/// the test below pins the formula against the shipped face's own measured shape (Inter Bold at
/// `size::HERO`, off `tools/font-hint-audit.py`'s own face — cap band 17..70px of an 87px line box).
fn descent_pad(box_h: f32, full_h: f32, cap_top: f32, cap_base: f32) -> f32 {
    (full_h - box_h * 0.5 - (cap_top + cap_base) * 0.5).max(0.0)
}

/// The bright endpoint [`draw`]'s query ink crosses toward on `super::HOT` — issue 22's whole state
/// machine, now that the plate it once gated is gone (2026-09-04). A discrete pick on `editing`
/// alone, never a continuous function of it: the keyboard raising is a discrete event, not a spring
/// one, which is the same reason the deleted `fill_t` never faded on `editing` either.
///
/// **Focused, not editing** has no caret, no keyboard and no plate to carry "the remote is on this
/// control" instead — so it gets [`theme::FIELD_WAITING_INK`], a genuinely pure white, one stop past
/// what editing needs. **Editing** drops back to [`theme::FIELD_EDITING_INK`] — the SAME near-white
/// `TEXT_PRIMARY` stop this control's ink crossed to before issue 22 ever existed, and the caret's
/// own colour (see the caret's comment in [`draw`]) — because the blinking bar and the open keyboard
/// already carry "you are editing this", leaving the brighter white for the state that has nothing
/// else. Extracted so a host test can drive exactly what `draw` does, the same reason
/// [`run_caret_w`]'s own extraction exists — and the same reason the deleted `fill_t`/`field_ink`
/// pair (issue 22's plate machinery) were their own functions too.
fn ink_target(editing: bool) -> [f32; 4] {
    if editing {
        theme::FIELD_EDITING_INK
    } else {
        theme::FIELD_WAITING_INK
    }
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
/// it belongs to. The returned offset is also used by the one-character hint when the bar is not
/// drawn, so the gap is always present; `caret` controls only whether the box must reserve it.
///
/// The bar sits one [`CARET_GAP`] after the insertion point — the current SearchField component's
/// explicit air. The returned offset includes that gap, while the run shift is still derived from
/// the measured head and therefore follows the real insertion point.
fn run_layout(run_w: f32, caret_w: f32, box_w: f32, caret: bool) -> (f32, f32) {
    let tail = if caret { CARET_GAP + CARET_W } else { 0.0 };
    let avail = (box_w - tail).max(0.0);
    // Never scroll further than the run's own overflow: a short query must sit at the left edge
    // whatever the caret is doing, and the second clamp is what stops a caret at position 0 from
    // dragging an already-short run off the box.
    let over = (caret_w - avail).max(0.0).min((run_w - avail).max(0.0));
    let caret_x = (caret_w - over + CARET_GAP).min((box_w - CARET_W).max(0.0));
    (-over, caret_x)
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
    /// ("Gleb's Mac mini"); a share is experienced as the LIBRARY it gave you ("Film Club"), and
    /// its hostname is a string that means nothing to anyone watching. `SearchScreen.jsx`
    /// writes exactly that pair, and `browse::BrowseSource::name` states the rule outright: the
    /// Sources list is "the only place in the app a machine is named".
    ///
    /// A share with several libraries joins them; a share whose section table has not landed yet
    /// falls back to [`UNNAMED_SHARE`], never to its hostname.
    fn label(&self) -> String {
        if !self.owned {
            // Blank titles are dropped rather than counted: a section the table holds but has not
            // titled yet would otherwise read as a library this share has.
            let named: Vec<&str> = self
                .libs
                .iter()
                .copied()
                .filter(|t| !t.is_empty())
                .collect();
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
/// deleting. Two things overturned it. `SearchScreen.jsx` makes the point that a row whose
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
            // several libraries falls back to. "friend · friend" attributes a thing to itself.
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
    Some(format!(
        "{} unreachable · results from {} only",
        name_set(&down),
        name_set(&live)
    ))
}

/// Name a set of sources, collapsing to a COUNT once naming them all stops being a sentence.
///
/// `SearchScreen.jsx` designs one share ("Searching <your server> and <their library> ·
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
    let libs: usize = shares
        .iter()
        .map(|s| s.libs.iter().filter(|t| !t.is_empty()).count())
        .sum();
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
    /// exact roster identity: count alone aliases an equal-size `{A,B}` → `{A,C}` replacement
    roster: u32,
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
            roster: crate::plex::server_roster_gen(),
            sections: crate::browse::sections_gen(),
            live: 0,
            facts: [0; crate::plex::MAX_SERVERS],
        };
        // `i` is the position in the LIVE roster, not the slot number — a sign-out retires the
        // slots below the registry's floor without renumbering what registers after them, so the
        // two stopped agreeing. It does not have to be the slot: this is a fingerprint, and all it
        // owes is that it moves when the roster does. `server_ids` yields at most `MAX_SERVERS`
        // entries, so `i` is always both a bit of `live` and an index of `facts`; `take` says so to
        // the reader rather than leaving it to be re-derived from two other modules.
        for (i, sid) in crate::plex::server_ids()
            .enumerate()
            .take(crate::plex::MAX_SERVERS)
        {
            // The same reading [`build`] uses: a source `browse`'s table has never adopted has not
            // failed, so an absent one is live. The two must agree or the line changes without the
            // key moving.
            if srcs
                .iter()
                .find(|s| s.sid == sid)
                .map(|s| s.reachable())
                .unwrap_or(true)
            {
                k.live |= 1 << i;
            }
            k.facts[i] =
                crate::plex::server_facts(sid).map_or(0, |f| std::ptr::from_ref(f) as usize);
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
                live: srcs
                    .iter()
                    .find(|s| s.sid == sid)
                    .map(|s| s.reachable())
                    .unwrap_or(true),
            }
        })
        .collect();
    let line = scope_text(&src).map(|t| {
        CString::new(crate::text::elide(
            &t,
            SCOPE_W,
            theme::size::CAPTION,
            0,
            false,
        ))
        .unwrap_or_default()
    });
    // A name nothing has supplied yet is DROPPED rather than replaced — `super::empty::scope_hint`
    // then writes the sentence with no scope clause at all, which stays true whatever the roster
    // turns out to be. The fallbacks above ([`UNNAMED_OWN`]) belong to the LINE, which has to stay
    // a sentence with a hole in the middle of it; a hint with a whole clause to drop does not.
    let name = (src.len() == 1)
        .then(|| src[0].name)
        .filter(|s| !s.is_empty());
    ScopeState {
        key,
        line,
        n: src.len(),
        name,
    }
}

/// How many sources this screen searches, and — only when there is exactly one — its machine name.
/// [`super::empty`]'s hint is written from these two and nothing else, so it can no longer
/// contradict the line [`draw_scope`] draws one row above it.
///
/// `empty` used to count this itself, as `server_ids().filter(|id| client_for(id).is_some())`. That
/// filter was **inert** and is not carried over: `plex::servers::register_lazy` publishes the slot
/// pointer and only *then* stores the count ("after the pointer: a visible count implies a live
/// slot"), and nothing in a shipped build ever nulls a slot back — so every id `server_ids` yields
/// resolves to a `Client`. It still does now that a sign-out RETIRES slots, because the retirement
/// is reflected by that same walk; profile visibility may contain holes, but `server_ids` and
/// `client_for` turn away exactly the same inactive ids. Keeping the filter implied a
/// registered-but-undialable state the registry does not permit.
pub(super) fn sources() -> (usize, Option<&'static str>) {
    with_scope(|s| (s.n, s.name))
}

/// State the scope, if there is anything to state. Everything this reads was resolved by
/// [`with_scope`], so the draw is one `Label` on one already-elided run.
/// The field's own FACTS, stacked under the query and left-aligned on it.
///
/// One line: where the field is LOOKING. The minimum-query instruction sits in the field itself,
/// after the caret, so the scope never moves when the query grows from one character to two.
fn draw_scope(p: Painter) {
    let line_h = theme::size::CAPTION as f32 * 1.35;
    with_scope(|s| {
        let Some(line) = s.line.as_ref() else { return };
        Label::new(line.as_ptr(), theme::size::CAPTION, theme::TEXT_TERTIARY)
            .draw(p, Rect::new(FIELD.x, super::SCOPE_Y, SCOPE_W, line_h));
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every name here is a PLACEHOLDER. This repository is public and a real share's machine name
    /// and account handle are a third party's — see `docs/agent-reference.md`.
    fn own(name: &str) -> Scope<'_> {
        Scope {
            name,
            libs: Vec::new(),
            handle: "",
            owned: true,
            live: true,
        }
    }
    /// A share is named by its LIBRARY, so that is what the fixture takes — the machine name is
    /// carried only to prove the line never reaches for it.
    fn share<'a>(lib: &'a str, handle: &'a str) -> Scope<'a> {
        Scope {
            name: "a-hostname",
            libs: vec![lib],
            handle,
            owned: false,
            live: true,
        }
    }
    fn down(mut s: Scope) -> Scope {
        s.live = false;
        s
    }

    /// The current SearchField component keeps the one-character instruction in the query line,
    /// after the caret. The block below therefore owns exactly one caption line: scope. Reserving
    /// a second line here is the stale layout that put "Searching starts…" above it.
    #[test]
    fn the_scope_block_is_one_line_because_the_minimum_hint_lives_in_the_field() {
        assert_eq!(SCOPE_H, theme::size::CAPTION as f32 * 1.35);
        assert!(ghost_shown("a"));
        assert!(!ghost_shown(""));
        assert!(!ghost_shown("ab"));
    }

    /// [`ink_target`] in isolation — the whole of issue 22's post-plate state machine (2026-09-04):
    /// a discrete pick on `editing` alone, never a spring of its own.
    #[test]
    fn ink_target_picks_pure_white_off_editing_and_the_editing_stop_on() {
        assert_eq!(ink_target(false), theme::FIELD_WAITING_INK);
        assert_eq!(ink_target(true), theme::FIELD_EDITING_INK);
        assert_ne!(
            theme::FIELD_WAITING_INK,
            theme::FIELD_EDITING_INK,
            "the two focused states must read apart from each other by ink alone"
        );
    }

    /// Focus must be a couch-visible COLOUR step in EVERY state, and the two focused states must
    /// read apart from each other too — the property that replaced issue 22's plate (2026-09-04,
    /// owner: "you made the search bar background white, while I wanted text to be white"). This is
    /// `draw`'s whole ink computation (`theme::cross(TEXT_SECONDARY, ink_target(editing), hot)`) at
    /// both ends of `hot` and both values of `editing`.
    #[test]
    fn focus_is_carried_by_a_wide_ink_step_and_nothing_else() {
        let idle = theme::cross(theme::TEXT_SECONDARY, ink_target(false), 0.0);
        assert_eq!(idle, theme::TEXT_SECONDARY, "idle reads the same whatever `editing` claims");

        // Focused, not yet editing: the brightest thing on the page — no caret, no keyboard, no
        // plate, so ink alone has to say "the remote is on this control".
        let waiting = theme::cross(theme::TEXT_SECONDARY, ink_target(false), 1.0);
        assert_eq!(
            waiting,
            theme::FIELD_WAITING_INK,
            "focused-not-editing ink is the pure-white token"
        );

        // Editing: one stop dimmer, and the caret's own colour — the blinking bar and the open
        // keyboard carry the rest of "you are editing this".
        let editing = theme::cross(theme::TEXT_SECONDARY, ink_target(true), 1.0);
        assert_eq!(
            editing,
            theme::FIELD_EDITING_INK,
            "editing ink is the same near-white stop the caret itself is drawn in"
        );

        let luma = |c: [f32; 4]| (0.2126 * c[0] + 0.7152 * c[1] + 0.0722 * c[2]) * c[3];
        assert!(
            luma(waiting) - luma(idle) > 0.15,
            "the focus step must be far wider than a one-stop fade: {idle:?} -> {waiting:?}"
        );
        assert!(
            luma(editing) - luma(idle) > 0.15,
            "editing must be a couch-visible step off idle too: {idle:?} -> {editing:?}"
        );
        assert!(
            luma(waiting) > luma(editing),
            "focused-not-editing must read brighter than editing, or the two states collapse into \
             one: waiting={waiting:?} editing={editing:?}"
        );
    }

    /// **A narrow regression tripwire for issue 22's EXACT shape — not a general proof of absence.**
    /// Nothing on the host can call `draw` itself (it rasterizes; see the module doc's "two pieces
    /// of arithmetic" section), so there is no way to assert the property this control actually
    /// wants ("no draw call in `draw` ever fills anything") without a real GPU or a mocked
    /// `Painter`, neither of which exists here. What this CAN do, the same technique `diag::scrub`'s
    /// `no_log_call_site_interpolates_viewing_content` uses for its own absence property, is grep the
    /// file for the two literal fragments issue 22's fill was actually built from — the `Painter`
    /// call that filled `field_box`, and the `ACCENT` token it filled it with — rather than the
    /// deleted `fill_t`/`field_ink` function names alone, so simply renaming those two GATING
    /// helpers and pasting the same call back in still fails it. **It is still only a grep, and
    /// narrower than that reads**: a fill spelled across multiple lines, through a differently-named
    /// LOCAL holding `field_box` or the colour itself, via an aliased colour, a different fill token
    /// than `ACCENT`, or added inside a helper `draw` calls rather than inline, would all slip past
    /// this exact pattern undetected (Codex review, 2026-09-04). Treat it as "the one bug that
    /// already happened cannot happen the same way twice",
    /// not as a guarantee this file has no fill in any state — the module doc's plain-English "no
    /// FILL either" claim is the actual contract, upheld by review and by the simulator captures this
    /// change was verified with, not by this test alone. Each needle in the body below is assembled
    /// at runtime from two literals that do NOT sit adjacent anywhere in this file's own source
    /// (including this comment) — a whole literal would match this very assertion's own line and
    /// fail the test against itself.
    #[test]
    fn issue_22s_exact_fill_shape_does_not_reappear_in_the_source() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(file!());
        let src = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
        let fill_call: String = ["p", ".rect(field_box"].concat();
        assert!(
            !src.contains(&fill_call),
            "a fill rect behind the query line reintroduces the plate the owner rejected — 2026-09-03 \
             issue 22, reverted 2026-09-04. Focus is ink only."
        );
        let accent_use: String = ["theme::AC", "CENT,"].concat();
        assert!(
            !src.contains(&accent_use),
            "ACCENT has no job in this file any more — it was only ever the plate's fill"
        );
    }

    #[test]
    fn the_caret_is_visible_only_during_the_on_phase_of_an_editing_field() {
        assert!(caret_shown(true, true));
        assert!(
            !caret_shown(true, false),
            "the OFF phase must actually hide the bar"
        );
        assert!(
            !caret_shown(false, true),
            "a parked ON phase must not draw without the keyboard"
        );
        assert!(!caret_shown(false, false));
    }

    #[test]
    fn a_run_that_fits_starts_at_the_edge_and_the_caret_trails_it() {
        let (run, caret) = run_layout(100.0, 100.0, 756.0, true);
        assert_eq!(run, 0.0);
        assert_eq!(caret, 100.0 + CARET_GAP);
    }

    #[test]
    fn an_overlong_run_slides_left_so_the_caret_lands_on_the_boxs_right_edge() {
        let box_w = 756.0;
        let (run, caret) = run_layout(900.0, 900.0, box_w, true);
        assert!(run < 0.0, "the run must slide LEFT, not clip on the right");
        // the tail of the string is what stays on screen…
        assert_eq!(run + 900.0, box_w - CARET_GAP - CARET_W);
        // …and the caret's right edge is exactly the box's
        assert_eq!(caret + CARET_W, box_w);
    }

    #[test]
    fn with_no_caret_the_run_uses_the_whole_box() {
        let box_w = 756.0;
        let (run, _) = run_layout(900.0, 900.0, box_w, false);
        assert_eq!(run + 900.0, box_w);
    }

    /// Even at the start of a run the caret keeps the component's explicit air after the insertion
    /// point. A box too narrow to hold it is covered by the degenerate case below.
    #[test]
    fn an_empty_run_keeps_the_designs_caret_gap() {
        let (run, caret) = run_layout(0.0, 0.0, 756.0, true);
        assert_eq!(run, 0.0);
        assert_eq!(caret, CARET_GAP);
    }

    /// The caret follows the NEXT glyph position with the component's fixed visual gap, at every
    /// length — the property the whole left-clip exists to preserve. (With the caret at the run's
    /// END, which is where typing keeps it; the cases where it is not are the test below.)
    #[test]
    fn the_caret_follows_the_insertion_point_whether_or_not_the_run_has_slid() {
        for run_w in [0.0, 1.0, 400.0, 753.0, 900.0, 5000.0] {
            let (run, caret) = run_layout(run_w, run_w, 756.0, true);
            assert_eq!(
                caret,
                run + run_w + CARET_GAP,
                "caret must keep the design gap after the run for run_w={run_w}",
            );
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
        let avail = box_w - CARET_GAP - CARET_W;
        // the caret at the FRONT of a long run: no scroll at all, and the tail is what is cut
        let (run, caret) = run_layout(2000.0, 0.0, box_w, true);
        assert_eq!(
            (run, caret),
            (0.0, CARET_GAP),
            "a caret at the front pins the run to the left edge"
        );
        // …stepped just inside the box: still no scroll, the bar simply stands further along
        let (run, caret) = run_layout(2000.0, avail - 10.0, box_w, true);
        assert_eq!((run, caret), (0.0, avail - 10.0 + CARET_GAP));
        // …and past it: the run slides by exactly the overflow, parking the bar on the right edge
        let (run, caret) = run_layout(2000.0, 1000.0, box_w, true);
        assert_eq!(
            run,
            -(1000.0 - avail),
            "the run slides to bring the caret back inside"
        );
        assert_eq!(caret + CARET_W, box_w);
        // the caret is always drawn INSIDE the box, at every position in a very long query
        for c in [0.0, 1.0, 500.0, 1999.0, 2000.0] {
            let (_, caret) = run_layout(2000.0, c, box_w, true);
            assert!(
                (0.0..=box_w - CARET_W).contains(&caret),
                "caret at {c} drew at {caret}, outside the field",
            );
        }
    }

    // ---- the two slices the draw measures ------------------------------------------------------

    /// The ordinary case, and the one that carries the whole point of the caret: the head is the
    /// text BEFORE the insertion point, so its width is where the bar stands.
    #[test]
    fn the_head_is_the_text_before_the_insertion_point() {
        assert_eq!(run_and_head("wallace", 0), ("wallace", ""));
        assert_eq!(run_and_head("wallace", 3), ("wallace", "wal"));
        assert_eq!(run_and_head("wallace", 7), ("wallace", "wallace"));
        // A trailing space is part of the run — `search::query` keeps it because this draws it.
        assert_eq!(run_and_head("wallace ", 8), ("wallace ", "wallace "));
        assert_eq!(run_and_head("", 0), ("", ""));
    }

    /// **A caret is a BYTE offset, and half the characters a television's keyboard can commit are
    /// not one byte wide.** Splitting one panics — inside a DRAW, inside the SDL event loop, which
    /// is the app. Every offset into a multi-byte run is exercised here, including the ones that
    /// land mid-codepoint, because `super::caret` clamps against the LIVE query while this cuts the
    /// NUL-truncated copy, and the two are not the same string.
    #[test]
    fn a_caret_inside_a_multi_byte_character_never_splits_it() {
        for q in ["суббота", "千と千尋", "Amélie", "🎬🎬", "aé千🎬z"] {
            for c in 0..=q.len() + 4 {
                let (run, head) = run_and_head(q, c);
                assert_eq!(run, q);
                assert!(
                    q.starts_with(head),
                    "the head must be a prefix of the run: {head:?}"
                );
                // it lands on the boundary at or below the asked-for offset, never above it
                assert!(head.len() <= c.min(q.len()));
                assert!(
                    c.min(q.len()) - head.len() < 4,
                    "…and never backs up past one character"
                );
            }
        }
    }

    /// The run is cut at the first NUL — `super::enter`'s seed is read from a FILE, so a control
    /// byte is reachable — and a caret past that cut lands at its end rather than out of range.
    #[test]
    fn a_nul_cuts_the_run_and_the_caret_lands_inside_what_is_left() {
        assert_eq!(run_and_head("wal\0lace", 3), ("wal", "wal"));
        assert_eq!(
            run_and_head("wal\0lace", 8),
            ("wal", "wal"),
            "a caret past the cut clamps to it"
        );
        assert_eq!(run_and_head("\0wallace", 4), ("", ""));
        // …and the clamp still lands on a character boundary, not merely inside the length.
        assert_eq!(run_and_head("су\0ббота", 99), ("су", "су"));
    }

    /// The degenerate box (a field narrower than its own caret) must not produce a negative
    /// budget, which would slide a short run left for no reason.
    #[test]
    fn a_box_too_narrow_for_the_caret_does_not_go_negative() {
        let (run, caret) = run_layout(0.0, 0.0, 2.0, true);
        assert_eq!(run, 0.0);
        assert_eq!(caret, 0.0);
    }

    // ---- issue 20: the blank-field caret sits at the FRONT, not after the placeholder ---------

    /// Calls [`run_caret_w`] — the SAME function `draw` calls for both its blank and non-blank
    /// branches — rather than hand-supplying `0.0` to `run_layout`. That closes ONE gap Codex review
    /// round 1 found (the first version of this test supplied `0.0` directly and could not have
    /// caught a regression in `run_caret_w`'s OWN body) but not the other it named: a `draw` that
    /// stopped calling `run_caret_w` and went back to inlining the placeholder's width would still
    /// compile and still pass every test here, since nothing on the host can call `draw` itself (it
    /// rasterizes) — the simulator screenshots this file's commits carry are what close that half.
    #[test]
    fn a_blank_field_puts_the_caret_at_the_field_start_not_after_the_placeholder() {
        // The function itself, in isolation: blank is always 0, whatever `head_w` would have been;
        // non-blank passes the measured head straight through.
        assert_eq!(run_caret_w(true, 999.0), 0.0);
        assert_eq!(run_caret_w(false, 42.0), 42.0);

        let box_w = 756.0;
        let placeholder_w = 480.0; // stand-in for "Search your library"'s measured width
        // The FIX composed with `run_layout`: `caret_w` is 0 for a blank field, so the bar lands
        // `CARET_GAP` past the box's own left edge, exactly where a real query's caret sits before
        // its first character.
        let (run, caret) = run_layout(placeholder_w, run_caret_w(true, 0.0), box_w, true);
        assert_eq!(run, 0.0, "the placeholder itself is not scrolled");
        assert_eq!(
            caret, CARET_GAP,
            "the caret sits CARET_GAP past the field's own start"
        );
        // The HISTORICAL BUG, simulated (not reachable through `run_caret_w`, which never returns
        // the placeholder's own width for the blank case): feeding it back in as `caret_w` — the
        // shape `draw` had before this fix — parks the bar past every letter of "Search your
        // library" instead. Kept as a direct `run_layout` call, deliberately bypassing
        // `run_caret_w`, so this comparison survives even though the buggy VALUE is no longer
        // reachable through the helper.
        let (buggy_run, buggy_caret) = run_layout(placeholder_w, placeholder_w, box_w, true);
        assert_eq!(buggy_run, 0.0);
        assert_eq!(
            buggy_caret,
            placeholder_w + CARET_GAP,
            "the historical bug: the caret trailed the whole placeholder"
        );
        assert!(
            caret < buggy_caret,
            "the fixed caret must land well before the buggy one"
        );
    }

    // ---- issue 21: the field's clip (and fill) reserve the run's own DESCENT -------------------

    #[test]
    fn the_clip_reserves_only_the_descent_centering_does_not_already_clear() {
        // Measured off the shipped face (Inter Bold, `pkg/appfont-bold.ttf`) at `size::HERO` (72px)
        // via `tools/font-hint-audit.py`'s own FreeType read: cap band 17..70px of an 87px line box
        // (ascent ≈70px, full line height ≈87px). `Label`'s `VAlign::Middle` centres the CAP BAND —
        // not the full 87px box — on `FIELD`'s own 80px frame's middle; since the cap band sits well
        // above the box's own centre, that alone already gives away `(80 - (70-17))/2 ≈ 13.5px`
        // below the baseline for free — so the true shortfall is `87 - 80/2 - (17+70)/2 = 3.5px`,
        // not the naive `87 - 70 = 17px` this function's first shape returned (Codex review round 1:
        // that overshot enough to run the fill 5px into the scope line's own row below `FIELD`).
        assert_eq!(descent_pad(80.0, 87.0, 17.0, 70.0), 3.5);
        // A face whose line box does not reach past the frame's own centre needs no pad at all —
        // the clip stays exactly `FIELD`'s own box.
        assert_eq!(descent_pad(80.0, 40.0, 17.0, 20.0), 0.0);
        // A degenerate reading must not SHRINK the clip below FIELD's own box.
        assert_eq!(descent_pad(80.0, 10.0, 17.0, 70.0), 0.0);
    }

    /// **One source is still named** — the rule reversed on 2026-08-15, and worth a test rather
    /// than a deletion because the old one asserted the opposite for a stated reason. The line is
    /// the field's answer to "where am I looking", `empty` no longer carries that fact for the
    /// single-source case, and a right half that empties out when a server is removed reads as a
    /// missing element. Only "no sources at all" is silent, because there is nothing true to say.
    #[test]
    fn one_source_is_named_and_only_no_source_is_silent() {
        assert_eq!(
            scope_text(&[own("nas-home")]).unwrap(),
            "Searching nas-home"
        );
        // …by its MACHINE name, which is what your own server is recognised by (`Scope::label`).
        assert_eq!(
            scope_text(&[own("")]).unwrap(),
            "Searching your server",
            "never a blank in a sentence"
        );
        // A lone SHARE is named by its library, and carries no handle: there is no second name for
        // the `·` to separate it from, and "Film Club · friend" beside a one-source install states
        // an attribution nothing on the screen contrasts with.
        assert_eq!(
            scope_text(&[share("Film Club", "friend")]).unwrap(),
            "Searching Film Club"
        );
        // Down, alone: the bare fact, with nothing left to say results are coming from instead.
        assert_eq!(
            scope_text(&[down(own("nas-home"))]).unwrap(),
            "nas-home unreachable"
        );
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
    /// `SearchScreen.jsx` writes "Searching &lt;your server&gt; and &lt;their library&gt; · &lt;handle&gt;", and
    /// `browse::BrowseSource::name` states the invariant: the Sources list is the only place in the
    /// app a machine is named. A hostname is a string that means nothing to someone watching.
    ///
    /// The fixture hands every share a machine name precisely so this can assert it never appears.
    #[test]
    fn a_share_is_named_by_its_library_and_never_by_its_hostname() {
        let t = scope_text(&[own("nas-home"), share("Film Club", "friend")]).unwrap();
        assert!(t.contains("Film Club"), "the share is its library: {t}");
        assert!(
            !t.contains("a-hostname"),
            "the share's MACHINE must not reach this line: {t}"
        );
        // …while your own side is still the machine you recognise
        assert!(
            t.contains("nas-home"),
            "your own server keeps its name: {t}"
        );

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
        assert_eq!(
            t.matches("friend").count(),
            1,
            "the handle appears once, not as its own suffix: {t}"
        );
    }

    /// **Past a couple of shares the line stops naming and starts counting.** The registry holds
    /// sixteen slots; enumerating them is a list, not the "one plain fact" the design asks for, and
    /// the handle could attribute only one of them regardless. Libraries are what is counted — a
    /// machine is not what anyone is searching — and your own server keeps its name, because that
    /// is the part of the answer that does not change.
    #[test]
    fn many_shares_collapse_to_a_count_rather_than_becoming_a_list() {
        let mk = |lib: &'static str, h: &'static str| {
            let mut s = share(lib, h);
            s.libs = vec![lib];
            s
        };
        let many = vec![
            own("nas-home"),
            mk("A", "ann"),
            mk("B", "bob"),
            mk("C", "cat"),
        ];
        assert_eq!(
            scope_text(&many).unwrap(),
            "Searching nas-home and 3 shared libraries"
        );

        // …and with the section tables still landing there are no library names to count, so it
        // counts what it does know rather than inventing one.
        let mut blank = vec![
            own("nas-home"),
            share("", "ann"),
            share("", "bob"),
            share("", "cat"),
        ];
        for s in blank.iter_mut() {
            if !s.owned {
                s.libs = Vec::new();
            }
        }
        assert_eq!(
            scope_text(&blank).unwrap(),
            "Searching nas-home and 3 shared sources"
        );
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
            scope_text(&[
                own("nas-home"),
                share("Film Club", "friend"),
                share("Archive", "pal")
            ])
            .unwrap(),
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
        assert_eq!(
            scope_text(&[own("nas-home"), share("", "")]).unwrap(),
            "Searching nas-home and a shared server"
        );
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
    /// The remaining inputs are read straight out of the counters that define them (the exact
    /// registry generation/count and the section table generation).
    ///
    /// This grades [`Key::read`] and never [`with_scope`]: building the projection measures and
    /// elides text, and there is no font on the host tier.
    #[test]
    fn the_memo_key_moves_when_a_source_goes_quiet_or_is_described() {
        let _g = fresh_registry();
        // `register_for_test`, not the public `register`: the latter resolves the device id through
        // `session::load`, which mints and PERSISTS a uuid a host test has no business writing, and
        // it spawns a `serverinfo` worker against the fixture address.
        let own =
            crate::plex::register_for_test("mach-own", "10.0.0.1", 32400, "tok", "cid-field-test");
        crate::plex::register_for_test("mach-friend", "10.0.0.2", 32400, "tok", "cid-field-test");

        crate::browse::seed_sources_for_test(2, true);
        let live = Key::read();
        assert_eq!(live.servers, 2, "both registered slots are sources");
        assert_eq!(live.live, 0b11, "…and both last answered");

        // A share going quiet is `browse`'s flag alone. The seed also resets the section table, so
        // the whole key moves for two reasons — the BIT is what this pins.
        crate::browse::seed_sources_for_test(2, false);
        assert_eq!(
            Key::read().live,
            0,
            "a source that stopped answering must move the key"
        );

        // …and the roster naming a machine moves it through the facts address on its own: nothing
        // else about the registry or the table changed, and the line goes from "your server" to a
        // name the user recognises.
        //
        // **Through the facts ADDRESS and not through `roster`**, which is the distinction
        // `plex::servers` publishes two counters for: `ROSTER_GEN` says the set of servers moved
        // (this screen's stores discard work aimed at the old one), `FACTS_GEN` says what we SAY
        // about one moved. A describe is only ever the second. This screen needs neither counter
        // for it — every describe leaks a fresh `ServerFacts`, and the address of that record is
        // already in the key.
        crate::browse::seed_sources_for_test(2, true);
        let before = Key::read();
        crate::plex::describe_server(own, "nas-home", "", true);
        let after = Key::read();
        assert_ne!(
            before, after,
            "a description landing must rebuild the line that speaks it"
        );
        assert_eq!(
            (after.servers, after.roster, after.live, after.sections),
            (before.servers, before.roster, before.live, before.sections),
            "…through the facts address, which is the only input that changed"
        );
    }

    #[test]
    fn an_undescribed_equal_count_roster_replacement_moves_the_memo_key() {
        let _g = fresh_registry();
        let a = crate::plex::register_for_test("scope-a", "10.0.0.1", 32400, "a", "cid");
        crate::plex::register_for_test("scope-b", "10.0.0.2", 32400, "b", "cid");
        crate::browse::seed_sources_for_test(2, true);
        let before = Key::read();

        crate::plex::revoke_for_profile_switch();
        crate::plex::register_for_test("scope-c", "10.0.0.3", 32400, "c", "cid");
        let after = Key::read();
        assert_eq!((before.servers, after.servers), (2, 2));
        assert_ne!(
            before.roster, after.roster,
            "the active machine set changed at the same count"
        );
        assert_eq!(crate::plex::server_ids().next(), Some(a));
        assert_ne!(before, after);
    }
}
