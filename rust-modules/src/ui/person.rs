//! The person / actor page — reached by pressing OK on a detail page's cast headshot.
//!
//! **The reference is `Person Screen.dc.html`** (the owner's design project), whose shape is Apple
//! TV's, not Plex's field list. ONE scroll flow, in three parts:
//!
//! * an **asymmetric editorial band** across the top — a circular portrait on the left with a text
//!   column beside it (name at DISPLAY, a roles kicker, a Born/Died line, a 3-line bio that
//!   dissolves into a right-pinned `MORE`), **top-aligned on the name's cap top**;
//! * the plain **`Movies` / `Shows` shelves** of ordinary poster cards on the shared [`CardRow`]
//!   strip, each heading carrying its count;
//! * and the **Filmography entry row** at the end of them — the one control on the page that
//!   REPLACES the page ([`crate::ui::filmography`]).
//!
//! ## What changed, and why each half of it was a fix rather than a restyle
//!
//! **1. The band no longer CONDENSES.** It used to shrink the portrait 320→160 and step the name
//! HERO→TITLE the moment focus left it. That was a second, competing motion: the page already
//! scrolls, so the first keypress moved every element on screen at once. The band is a fixed block
//! at the top of the flow now and the page simply scrolls past it. Everything the condense needed —
//! the `cond` spring, the two-rung name crossfade, a second portrait diameter — is gone with it.
//!
//! **2. The page opens on the FILMOGRAPHY ENTRY, not on the header or the first shelf — owner
//! directive, 2026-09-06.** It briefly opened on the first shelf's first card instead ("the page's
//! subject is what you can play"), which read fine on a page that had already landed, but the
//! credits shelves arrive ASYNCHRONOUSLY and keep reshaping for a beat after the first items land —
//! so a mount that immediately picked a card could have that card's own SHELF re-pointed under it
//! mid-load, which reads as focus jumping partway down a list nobody asked to move through. The
//! entry row is the one landing spot that plex.tv's answer cannot move once it has been given: it
//! either turns out to exist or it does not, and [`clamp_focus`] holds the mount there until credits
//! have genuinely SETTLED before resolving it either way — to the entry row for real, or (rare) to
//! a shelf or the header if the person turns out to have no filmography at all. UP from the first
//! card still reaches the band, which is still a focus row — it is the page's top scroll position,
//! and it is what OK opens the biography from — but it is no longer where a mount lands, and neither
//! is the entry row a permanent one: it is a HOLD during the load, not a destination on its own.
//! [`on_header`] is the derived predicate that keeps the header side of this safe: a page with no
//! shelves *yet* (or a person with nothing in your libraries) has nowhere else for focus to be, so
//! the header holds it whatever the raw flag says, and a shelf landing then hands focus to its first
//! card without anything having to remember to move it back off.
//!
//! **3. The shelves are paced by the SHARED system instead of by three numbers of their own.**
//! This screen set a 46px heading band, no air at all under it and a 64px gap, which pitched its
//! shelves at 597 and ran the heading straight into the top of a popped card. A shelf here is now
//! `consts::TITLE_DY + CARD_DY` of heading, the poster, and `consts::UNDER_LABEL_AIR` to the next
//! one — i.e. exactly `consts::ROW_PITCH` at focus, the same rhythm Home, the Library and Search
//! have. The 64 rung survives ONCE, as [`BAND_GAP_TO_SHELF`]: the gap from the band down to the
//! first shelf.
//!
//! **The band top-aligns its two halves** (over the earlier owner call to centre them, which was
//! made against the header as it then was — a HERO name over a wider bio, where a top-aligned
//! circle really did float beside a ragged column). At DISPLAY, against a text stack the same
//! order of height as the portrait, the alignment is what reads as a decision. The mock's
//! `padding-top: 14px` and its `margin-top:-7px` are both CSS artifacts and are NOT ported: a 72px
//! line box carries leading above its caps, which our cap-band placement does not have (`ui::label`'s
//! layout ≠ paint rule), so the name's cap top IS the band's top edge. **A header that is nothing
//! but a name centres instead**, since there is no block to align to.
//!
//! The whole page sits on an **ambient wash** ([`Painter::ambient`]) keyed to the focused poster's
//! `UltraBlurColors` — the same corner data home's backdrop and detail's below-hero wash draw —
//! fading to a faint warm `ACCENT` tint while the header holds focus; twelve small springs chase
//! the corner channels so a focus change is a dissolve, not a cut.
//!
//! **Every header line below the name is optional, and the block reads as finished without them.**
//! The name + portrait arrive with the page (the cast row already held them); the roles/dates/bio
//! come from plex.tv (`crate::person`, `plex/discover.rs`) and may never come at all. Each line is
//! drawn only when it has content and the measured flow centres what is present, so the degenerate
//! page — portrait, name, shelves — is composed, not broken. No placeholder, no header spinner.
//!
//! Structure mirrors `detail.rs`: the page is a [`ScrollColumn`] whose children are **the header
//! band** and the PRESENT shelves — and nothing else. The Filmography entry is NOT a child of that
//! column: it moved INSIDE the band on 2026-09-06, at the end of the identity text stack, so
//! `Scene::len()` is `1 + nshelves(p)` and asking the flow for a child at `1 + nshelves` walks off
//! the end of it. (This sentence described the old frame-wide row after the shelves for one commit
//! after it stopped being true — long enough to matter, because every derived y on this page, the
//! scroll target, the hit test and the reveal, comes out of `child_top`.) The header is still a
//! **focus row** (child 0): reachability no longer forces it (see above), but it is what UP from
//! the first shelf lands on and it is the page's top scroll position. Perf
//! discipline for the A53 + Mali:
//! shelves draw through [`card_row::strip`] (`on_axis`-culled, so only VISIBLE tiles touch
//! `resolve_tex`), the header flow is measured once per store change (never per frame), and
//! `crate::person` caps each shelf at a `CardRow`'s spring count.
#![allow(non_upper_case_globals)]
use crate::person::Person;
use crate::pms::PmsMovie;
use crate::ui::card_row::{self, CardRow, RowStyle};
use crate::ui::consts::*;
use crate::ui::label::{HAlign, Label, VAlign};
use crate::ui::text_view::TextView;
use crate::ui::theme;
use crate::ui::widgets::{AmbientWash, Art, PageGround, StatusKind, StatusOverlay};
use crate::ui::{card_row::reveal, Column, Env, Painter, Rect, ScrollColumn, View};
use std::ffi::CString;
use std::os::raw::{c_int, c_uint};
use std::ptr::{addr_of, addr_of_mut};

// ---- geometry (a measured flow; every Y below is derived, none is authored) -------------------

/// Shelves, in flow order. Kind 0 = Movies, 1 = Shows — re-exported from the store, so an index
/// means one thing across the store, this focus model, the headings and the hit-test.
use crate::person::NSHELF;
/// Headings as C-string LITERALS, not `&str` — they are drawn every frame, and `CString::new`
/// per shelf per frame is a heap allocation the draw path does not need (the same reason
/// `detail.rs` writes `c"Related"` / `c"Cast & Crew"`).
const SHELF_TITLE: [&std::ffi::CStr; NSHELF] = [c"Movies", c"Shows"];

/// Portrait diameter. **One value, because the band no longer condenses** (module doc).
///
/// **It is 320, and this doc claimed 250 while the constant never moved.** The owner's note ("it
/// currently dominates the header") and the argument for reaching at [`CARD_W`] — the circle
/// exactly one poster card wide, so the band reads as the head of the page's own grid rather than a
/// plate that happens to be big — were both written down here and the reduction was never applied.
/// Recorded rather than quietly performed: the 320 band is what has been photographed and signed
/// off on the television since, so shrinking it now is a fresh design decision, not a correction.
/// If it should be `CARD_W`, that is one line here plus a re-measure of [`BIO_W`] and `HEADER_H`,
/// which both derive from this number. The rest of this note describes what the smaller circle
/// would buy, and is kept for whoever makes that call:
/// makes the top alignment ([`header_flow`]) read as a decision rather than as a coincidence.
/// **320** — measured against the canvas 2026-09-06 ("yours is about 250 px across; the reference
/// is roughly 320").
///
/// It was `consts::CARD_W` (250) from an owner review on 2026-09-05 ("it currently dominates the
/// header"); the same owner measured it back up against the reference the next day, so the newer
/// call stands. **Moving it also moves the whole text column**, which is the other half of that
/// report ("yours starts around x=410; the reference starts around x=478"): `col_x` is
/// `MARGIN_X + d + BAND_GAP`, so 320 puts the column at 480 with no second constant to keep in
/// step — and `BIO_W`, derived from the same expression, follows to the reference's ~1325.
const PORTRAIT_EXP: f32 = 320.0;
/// ...and beside a header that is ONLY a name — the degenerate page (no internet, or a person
/// plex.tv has never heard of). Smaller on purpose; see [`header_flow`].
const PORTRAIT_BARE: f32 = 220.0;
/// Requested from the image transcoder at 300×300 — the SAME size the detail page's cast circles
/// ask for, so arriving here from a headshot is a poster-cache HIT, not a fresh round trip through
/// `/photo/:/transcode` (a 320px draw of a 300px photo is invisible; a second cache slot is not).
const PORTRAIT_RES: (c_int, c_int) = (300, 300);
/// First child's pre-scroll top — the page's top air above the band (`Person Screen.dc.html`'s
/// `margin-top: 96px`). It is also [`TOP_MARGIN`], as it is in the mock: the same number the page
/// rests its top content at is the highest a focused shelf may lift to.
const HEADER_TOP: f32 = 96.0;
/// portrait → the text column: the band's one internal major gap (the mock's `gap: 64px`).
const BAND_GAP: f32 = theme::space::XL;
/// **The band's gap ladder, and it is three rungs rather than three numbers.** Below the name the
/// band is two GROUPS — the identity pair (roles + dates), then the bio block — and what separates
/// them is which rung each gap takes.
///
/// name → the identity pair: the break out of the name into the first group.
/// Name → the identity pair. **`LG`, up one rung from `SM`** — "the occupation and birth lines are
/// crowded together, directly beneath the name" (owner measurement, 2026-09-06).
const META_GAP: f32 = theme::space::LG;
/// the kicker → the Born/Died line. Those two lines are ONE fact in two lines and share one ink —
/// the dates are not a footnote to the roles, and dimming them a second step is what made the band
/// read as four separate strays.
///
/// **The rung is `SM`, not the tighter `XS` it was for a day** (owner: "enough line spacing to
/// avoid the cramped appearance"). Sharing an ink is what makes two lines one fact; setting them
/// eight pixels apart at `size::LABEL` and `size::CAPTION` just glues them, and the pair read as a
/// single wrapped line rather than as two facts about one person.
/// Between the two lines that read as one fact. The canvas's comment says 8; measured on the panel
/// the pair came out "crowded together" at that rung, so it is `SM` — still tighter than the break
/// above it, which is what keeps the two reading as a group rather than as two strays.
const LIFE_GAP: f32 = theme::space::SM;
/// the identity pair → the biography, **measured to the PROSE** rather than to the selectable
/// block's plate edge (which is how the design's mock spends it, as `MD` + the block's own top
/// padding). What a reader sees a gap before is the words, so that is what the rung is on.
///
/// The band's ladder is therefore `SM` · `SM` · `LG`: tight inside the name→identity break, tighter
/// still between the two lines that read as one fact, and a whole region rung down to the bio.
/// **`LG` after two owner passes** — `MD` first ("a medium gap before the biography"), then one
/// step up ("increase the gap between the metadata and biography by one spacing-token step"). The
/// step also buys back the clearance the plate needs: it hangs [`HL_PAD_Y`] up into this gap, and
/// at `MD` that reached the dates line's own descenders.
/// The identity pair → the bio. **`LG`**, one step back down from `XL`: at `XL` the gap read as too
/// much separation between the birth details and the biography, and pushed the Movies shelf lower
/// than it needed to be (owner correction, 2026-09-06, against the render at `XL`).
const BIO_GAP: f32 = theme::space::LG;
/// a shelf heading → its item count, inline.
const SHELF_COUNT_GAP: f32 = theme::space::SM;
/// **The bio PROSE's measure: the identity column's OWN width** — the same guide the name and both
/// meta lines run to, not a narrower reading measure of its own.
///
/// It was a flat `860`, and that number came from an OLD version of the canvas ("the design draws
/// the block 912 wide with 26 of padding either side"). The current one draws the block
/// `align-self: stretch` in the identity column with `width: calc(100% + 26px)` and the prose
/// running its full width — visibly most of the frame, where 860 wrapped after about sixty
/// characters and left a third of the band empty (owner comparison against the canvas, 2026-09-06).
///
/// Derived rather than authored, so a change to the portrait or to [`BAND_GAP`] moves the prose
/// with the two lines above it instead of leaving one run on a guide of its own. A header carrying
/// a bio is never the bare kind, so [`PORTRAIT_EXP`] is the only diameter this can be measured
/// against.
const BIO_W: f32 = text_w_const(PORTRAIT_EXP);

/// [`text_w`] as a `const fn`, because [`BIO_W`] is a constant and the two must not be two
/// expressions that can drift.
const fn text_w_const(d: f32) -> f32 {
    SCR_W - MARGIN_X - (MARGIN_X + d + BAND_GAP)
}
const BIO_LINES: usize = 3;
const BIO_LEAD: f32 = 40.0;
/// The selectable-block mark's inset around the bio prose — `detail.rs`'s About columns spend the
/// same 26 on x, so a block that can be opened is padded identically on both pages. The vertical
/// pad is even (the About columns' 36/24 pair is asymmetric because their 36 clears a HEADING's cap
/// band; this block has no heading, so an even box is what hugs it).
const HL_PAD_X: f32 = 26.0;
const HL_PAD_Y: f32 = 24.0;
/// The bio's truncation mark, as a C literal — it is drawn every frame the bio is cut off, and the
/// SAME pointer the reserved zone is measured from, so the gap and the mark can never end up being
/// two different strings (`detail.rs`'s About card writes `c"MORE"` twice for want of one).
const MORE: &std::ffi::CStr = c"MORE";
/// Air between the point the bio's dissolving last line has vanished and the left edge of [`MORE`].
/// **Re-derived for this rung, not copied.** detail's About card reserves 36 px beside a
/// `size::CAPTION` synopsis — 1.5× its own type size — and the bio is a rung larger
/// (`size::BODY`), where the same proportion is 42; `theme::space::LG` is the ladder rung nearest
/// that. An inline gap comes off the space ladder here exactly as [`SHELF_COUNT_GAP`] does, never
/// off a literal tuned against another screen's type size.
const BIO_MORE_GAP: f32 = theme::space::LG;
/// **The band → the FIRST shelf.** The one place this screen's old 64px section gap survives, and
/// it is a real region break: the editorial header ends, and the content begins.
const BAND_GAP_TO_SHELF: f32 = theme::space::XL;
/// **Shelf → shelf, and the last shelf → the Filmography entry.** [`crate::ui::consts::UNDER_LABEL_AIR`],
/// i.e. the air the shared row pitch already puts between a focused label block and the next thing
/// down — so a shelf on this page pitches at `consts::ROW_PITCH` exactly like one on Home. This was
/// [`theme::space::XL`] for both gaps, which pitched these shelves at 597 against everybody else's
/// 549; see the module doc.
const SHELF_GAP: f32 = UNDER_LABEL_AIR;
/// Shelf heading → poster row — `consts::TITLE_DY + CARD_DY`, the shared shelf's own heading band.
/// It was a hand-authored 46 here, which is the heading's cap band and NO air at all under it, so a
/// focused card popped straight up into the words above it.
const SHELF_LABEL_H: f32 = TITLE_DY + CARD_DY;
/// The shelves' own row style: [`RowStyle::HOME`]'s motion and geometry verbatim — one source, so a
/// poster here animates exactly as it does on Home. A long Plex title ("Wallace & Gromit: The Curse
/// of the Were-Rabbit") no longer wraps to a second line — every focused tile in the app shares the
/// one single-line title that marquees when it overflows [`card_row`]'s widened budget, so this
/// shelf reads exactly like Home's and Library's instead of growing its own label band.
const SHELF_STYLE: RowStyle = RowStyle::HOME;
/// A focused shelf lifts no higher than this from the screen top — [`HEADER_TOP`]'s twin.
const TOP_MARGIN: f32 = HEADER_TOP;
/// Air kept under the lowest visible content when a shelf is revealed by scrolling.
///
/// The OVERSCAN keep-out rather than a `space` rung (it was `LG` 40): what this bounds is a focused
/// tile's own caption against the bottom edge of the panel, so the number that belongs here is the
/// safe area's, not a gap from the spacing ladder.
const BOTTOM_PAD: f32 = crate::ui::consts::MARGIN_Y;

/// **The Filmography entry row** — height, and the pill's inset from the frame.
///
/// It is a ROW and not a pill, because it goes SOMEWHERE: so it is built like the rows inside the
/// route it opens (same inset, same radius, same accent fill) and carries a trailing chevron, the
/// mirror of the crumb that route lands on. A control that only changes something on THIS screen
/// has no chevron.
/// **The route entry is a COMPACT PILL in the band now, not a frame-wide row after the shelves.**
/// `Person Screen.dc.html`, 2026-09-06: "at ten feet that row was the heaviest object on the page —
/// a 1728px capsule under the posters pulls the eye off the thing the page is about. Everything
/// this person was ever in belongs to the person, so it belongs to the band."
///
/// Being compact is also what lets it take a CONTROL's focus motion, which the frame-wide row
/// could not: `crate::ui::widgets::CTRL_FOCUS_SCALE` 1.07, grown from the LEFT so the identity column's guide
/// holds. It keeps a row's construction — a trailing chevron, the mirror of the crumb the route
/// lands on — because it is the one control here that replaces the page.
const ENTRY_H: f32 = 60.0;
/// Outer inset inside the entry pill, on both sides — owner spec, 2026-09-06, stated as a ratio of
/// the pill's own height H rather than a bare pixel guess, so it stays correct if [`ENTRY_H`] ever
/// retunes: **horizontal padding ≈ 0.45H** (0.45×60 = 27), snapped to the nearest spacing token,
/// `MD` (24). Went through 30 (borrowed from `filmography.rs`'s row padding) and then 88 (a linear
/// 80-96 guess with no anchor to the control's own geometry) before landing here; this is the value
/// that stuck; keep it a token, not the bare 27, if `ENTRY_H` moves.
///
/// **It is a VISIBLE distance, not a box inset** — see [`ENTRY_MARK_BEARING`]: on the trailing side
/// the pill's edge is set from the chevron's INK, so both ends of the capsule show the same 24.
const ENTRY_OUTER_PAD: f32 = theme::space::MD;
/// The entry's trailing accessory box.
const ENTRY_MARK: f32 = 24.0;
/// **The chevron's own empty margin inside [`ENTRY_MARK`], each side** — `chevron.svg` inks only
/// x=7.5..16.5 of its 24-unit viewBox ([`crate::ui::icons::ink_x`]), so a box-measured gap beside it
/// is 7.5px wider on screen than the same number beside a letter. Every distance the chevron takes
/// part in is therefore corrected by this, which is what makes [`ENTRY_OUTER_PAD`] and
/// [`ENTRY_CHEVRON_GAP`] mean the same thing to the eye at both ends of the pill: uncorrected, the
/// space after the mark measured 31.5 against 24 before the label, and the mark's own gap measured
/// 23.5 — indistinguishable from the outer pad it is supposed to sit inside of.
///
/// Taken from the asset's table rather than transcribed, for the reason [`crate::ui::icons::ink_x`]
/// exists at all: a re-drawn `chevron.svg` moves this with no compile error and no test. The two
/// sides are read separately even though this asset is symmetric — a re-draw need not be.
const ENTRY_MARK_INK: (f32, f32) = crate::ui::icons::ink_x(crate::ui::icons::Icon::Chevron);
const ENTRY_MARK_BEARING_L: f32 = ENTRY_MARK * ENTRY_MARK_INK.0;
const ENTRY_MARK_BEARING_R: f32 = ENTRY_MARK * (1.0 - ENTRY_MARK_INK.1);
/// Air between the count and the chevron's INK — owner spec, 2026-09-06: **≈0.25H** (0.25×60 = 15),
/// nearest token `SM` (16) — deliberately wider than [`ENTRY_RUN_GAP`] so the chevron reads as a
/// disclosure mark trailing the "Filmography · N" fact rather than a fourth run in the same list,
/// and deliberately narrower than [`ENTRY_OUTER_PAD`] so the whole run still reads as one group
/// inside the capsule rather than two things sharing it. That ordering — `XS` inside `SM` inside
/// `MD` — only holds once [`ENTRY_MARK_BEARING_L`] is taken off it; measured to the box it was
/// 23.5 against the pad's 24, i.e. no ordering at all.
const ENTRY_CHEVRON_GAP: f32 = theme::space::SM;
/// Air between the bio block and the pill — the same `space::MD` rung the block's own break takes.
/// Bio → the route pill. `LG`, which is the ~14px the button needed BEYOND what it inherits from
/// the bio's own move (measured y≈375 against y≈441).
const ENTRY_GAP: f32 = theme::space::LG;
/// The gap EACH SIDE of the separator dot — label↔dot, dot↔count. Owner spec, 2026-09-06:
/// **≈0.12H** (0.12×60 = 7.2), nearest token `XS` (8) — proportioned off [`ENTRY_H`] rather than a
/// flat guess, and it lands back on the value this constant opened with (a coincidence worth
/// recording rather than re-deriving: `MD` was tried in between and read as too loose once the
/// chevron gap grew a separate, wider rung of its own to compare against). The chevron does NOT
/// share this rung — see [`ENTRY_CHEVRON_GAP`].
const ENTRY_RUN_GAP: f32 = theme::space::XS;
/// Ambient corner weights (tl, tr, br, bl — [`Painter::ambient`]'s order), i.e. how far each
/// corner leans from the app surface toward the source colour. Header state: a faint warm
/// `ACCENT` wash, strongest top-left (the mock's `radial at 18% 0%`). Card state: the focused
/// poster's UltraBlur corners, strong across the top and nearly gone by the bottom (the mock's
/// `.30 → .07` stops, trimmed a step on-device because our wash is opaque, not additive) — the
/// card arrangement is [`PageGround::CARD_W`] now, the one every item-keyed page ground in the app
/// leans by, named rather than spelt out so this page and the browsing screens cannot drift apart.
const AMB_HEADER_W: [f32; 4] = [0.10, 0.06, 0.02, 0.03];
const AMB_CARD_W: [f32; 4] = PageGround::CARD_W;

// ---- screen state ----------------------------------------------------------------------------

struct Scene {
    /// **The header holds focus** (flow child 0) rather than a shelf or the entry row. It is the
    /// page's top position and what UP from the first shelf returns to.
    ///
    /// **Read it through [`on_header`], never directly**, and mount it FALSE: a fresh mount starts
    /// [`Scene::on_entry`] instead (see there), and this only becomes true once that has been given
    /// up for a page that genuinely has no entry to hold.
    on_header: bool,
    /// **The Filmography entry row holds focus** — the last stop on the page, below every shelf.
    /// Mutually exclusive with [`Scene::on_header`]; both false means a shelf has it.
    ///
    /// **Mount it TRUE — owner directive, 2026-09-06.** The page used to mount on the first
    /// shelf's first card instead ("the subject is what you can play"), which read fine once, but
    /// the credits shelves land ASYNCHRONOUSLY and keep changing shape for a beat after the first
    /// items arrive: [`clamp_focus`]'s `kinds[0]` re-derivation, meant only to rescue a focus whose
    /// shelf emptied out from under it, was ALSO the mount-time default, so the very first frames
    /// of a real load could re-point `focus_kind` (and so the on-screen focus) partway down a
    /// shelf that had just gone from short to long. The entry row is comparatively stable — it
    /// either exists once credits have SETTLED or it never will — so mounting there and holding it
    /// through the load (see the credits-pending clause in [`clamp_focus`]) gives the page a
    /// landing spot that data arriving underneath it cannot move. It still gives way to a shelf or
    /// the header once the answer is actually in and says there is nothing to hold here.
    on_entry: bool,
    /// **Whether an explicit D-pad navigation has landed on the header.** A fresh mount starts
    /// `false` — the page opens on the header as a SCROLL POSITION, not a selection, so nothing
    /// should read as focused before the user has pressed a key toward it. Set the moment UP walks
    /// back onto the header from the first shelf, or a D-pad press while already on the header
    /// keeps it there (UP/LEFT/RIGHT, or OK opening the bio panel) — never by a data landing that
    /// happens to leave the header as the only row. See [`bio_mark_visible`].
    header_marked: bool,
    /// which shelf holds focus (a KIND, not a position — a shelf that empties must not silently
    /// hand its focus to the other one's contents). Meaningless while [`Scene::on_header`].
    focus_kind: usize,
    /// per-shelf focus memory: leaving a shelf and coming back restores the tile you were on
    col: [c_int; NSHELF],
    shelves: [CardRow; NSHELF],
    column: ScrollColumn,
    /// The ambient wash, dissolving toward [`amb_target`] — what makes a focus change wash over the
    /// page instead of cutting. The springs and the "a wash cannot be alpha-faded" reasoning live in
    /// the shared [`AmbientWash`], not here.
    amb: PageGround,
    spin_ms: f32,
    /// a cast headshot was just activated; app.rs takes this and routes (see [`take_request`])
    requested: bool,
    /// The header's text runs, NUL-terminated ONCE per store change (see [`refresh_header`]) —
    /// never per frame. `Label` holds a non-owning pointer, so these must outlive the draw: a
    /// field does, a temporary would not. This keeps the HEADER's draw allocation-free; the strip
    /// beneath it allocates exactly the focused tile's two lines per frame, which is the shared
    /// `card_row::strip` contract every shelf in the app draws under.
    name_c: CString,
    roles_c: CString,
    life_c: CString,
    /// the per-shelf heading counts — digits change only when a landing does, so they are baked
    /// with the rest of the runs rather than formatted per frame
    shelf_count_c: [CString; NSHELF],
    /// the Filmography entry's own count, baked for the same reason
    entry_count_c: CString,
    /// The measured header layout, and the flag that says it needs remeasuring. Rebuilt in
    /// [`update`] on the frame a store change lands, never in the draw — see [`HeaderFlow`].
    header: HeaderFlow,
    header_dirty: bool,
}

impl Scene {
    fn new() -> Self {
        Scene {
            on_header: false,
            on_entry: true,
            header_marked: false,
            focus_kind: 0,
            col: [0; NSHELF],
            shelves: [CardRow::new(); NSHELF],
            column: ScrollColumn::new(HEADER_TOP, TOP_MARGIN),
            amb: PageGround::new(),
            spin_ms: 0.0,
            requested: false,
            name_c: CString::default(),
            roles_c: CString::default(),
            life_c: CString::default(),
            shelf_count_c: [CString::default(), CString::default()],
            entry_count_c: CString::default(),
            header: HeaderFlow::default(),
            header_dirty: true,
        }
    }
    /// The band's height — a plain field read, since the band no longer condenses. Kept as a method
    /// because it is `Column::height(0)` and every scroll target, child top and hit-test goes
    /// through it.
    fn band_h(&self) -> f32 {
        self.header.exp_h
    }
}

static mut SCENE: Option<Scene> = None;
fn scene() -> &'static mut Scene {
    unsafe { (*addr_of_mut!(SCENE)).get_or_insert_with(Scene::new) }
}

/// What an OK / click on this page means for app.rs (everything else is consumed internally).
pub(crate) enum Action {
    None,
    /// A shelf card was activated — open its detail page (app.rs owns routing).
    Card,
}

// ---- present shelves -------------------------------------------------------------------------

/// The shelf kinds that have content, in flow order, and how many. A person with no shows has no
/// "Shows" heading at all — an empty labelled shelf reads as a broken fetch.
fn present(p: &Person) -> ([usize; NSHELF], usize) {
    let mut v = [0usize; NSHELF];
    let mut n = 0;
    for k in 0..NSHELF {
        if !p.shelf(k).is_empty() {
            v[n] = k;
            n += 1;
        }
    }
    (v, n)
}

/// **Does the HEADER hold focus this frame?**
///
/// [`Scene::on_header`] is the user's own answer; a page with NO shelves — one still fetching, or a
/// person with nothing in these libraries — has nowhere else for focus to be, so the header holds
/// it whatever the flag says. Deriving that rather than writing it back into the flag is what lets
/// a page that resolves off the entry row onto a shelf (module doc, and [`clamp_focus`]) land
/// there cleanly: the flag stays false through the whole wait, and the frame a shelf lands the
/// focus is already on its first card with nothing having moved it.
///
/// The entry row wins over both when it holds focus, because a page can be all header and entry —
/// a person with credits but nothing of theirs in your libraries, which is the case the entry row
/// matters most on.
fn on_header(p: &Person, sc: &Scene) -> bool {
    !sc.on_entry && (sc.on_header || present(p).1 == 0)
}

/// **Is the Filmography entry row on the page at all?** — the credits count has answered.
///
/// Drawn only once the COUNT is known, and the count is the credits response's OWN: `CreditType`'s
/// number is a different one (1745 actor credits where the list returns 222), so an entry filled
/// from the profile would name a total the route cannot honour.
fn has_entry(p: &Person) -> bool {
    p.credited && crate::person::filmography_total(p) > 0
}

/// Flow position of the focused shelf among the present ones (0-based), or None when the page has
/// no shelves yet.
fn focus_pos(p: &Person, sc: &Scene) -> Option<usize> {
    let (kinds, n) = present(p);
    kinds[..n].iter().position(|&k| k == sc.focus_kind)
}

/// The tile holding focus, WITHOUT touching the scene singleton — for callers that already hold
/// `&Scene` (minting a second `&'static mut` through [`scene`] while one is live is aliasing UB;
/// `update` carries the same note).
fn focused_of<'a>(p: &'a Person, sc: &Scene) -> Option<&'a PmsMovie> {
    if on_header(p, sc) || sc.on_entry {
        return None;
    }
    p.shelf(sc.focus_kind)
        .get(sc.col[sc.focus_kind].max(0) as usize)
}

// ---- the scroll flow (ScrollColumn children: 0 = header, 1.. = present shelves) ---------------

/// The flow, in one place: **0 = the band, 1..=n = the present shelves.** Every `Column` method
/// below indexes through this rather than re-deriving the split, so a page whose shelves have just
/// landed cannot have four functions disagreeing about what child 3 is.
///
/// **The Filmography entry is NOT a child of this flow any more** — it moved into the band (child
/// 0) on 2026-09-06, where the design puts it; `HeaderFlow::entry_y` places it and `Scene::band_h`
/// carries its height. It was `n + 1` here, a frame-wide row after the last shelf.
fn nshelves(p: &Person) -> usize {
    present(p).1
}

impl Column for Scene {
    fn len(&self) -> usize {
        crate::person::current()
            .map(|p| 1 + nshelves(p))
            .unwrap_or(1)
    }
    fn height(&self, i: usize) -> f32 {
        match i {
            0 => self.band_h(),
            // …at this shelf's LIVE label band: the block is only there while the shelf holds
            // focus, so an unfocused one gives the space back (`card_row::under_band`).
            _ => shelf_block_h_at(self.shelves[i - 1].under_band()),
        }
        .max(0.0)
    }
    /// The band takes its own region break down to the first shelf; everything below it is paced by
    /// the shared shelf rhythm. See [`BAND_GAP_TO_SHELF`] and [`SHELF_GAP`].
    fn gap_before(&self, i: usize) -> f32 {
        if i == 1 {
            BAND_GAP_TO_SHELF
        } else {
            SHELF_GAP
        }
    }
    fn focus_child(&self) -> Option<usize> {
        let p = crate::person::current()?;
        if on_header(p, self) {
            return Some(0);
        }
        if self.on_entry {
            return Some(0); // the pill lives in the band now
        }
        focus_pos(p, self).map(|pos| pos + 1)
    }
    fn draw_child(&self, i: usize, _env: &Env, p: Painter) {
        let Some(person) = crate::person::current() else {
            return;
        };
        if i == 0 {
            // …and the band draws the route-entry pill itself, at the end of its identity column
            draw_header(p, person, self);
            return;
        }
        let (kinds, n) = present(person);
        if let Some(&kind) = kinds[..n].get(i - 1) {
            draw_shelf(p, person, kind, self);
        }
    }
}

/// Where each header line sits inside the EXPANDED band, and how tall the band is — the ONE place
/// the header flow is computed, so the measure and [`draw_header`] cannot disagree about where a
/// line goes. Every text entry is `None` unless its content exists: a person with no kicker has no
/// gap where one would have been, which is what makes a sparse header read as composed rather than
/// as fields that failed to load. The portrait and the text stack are EACH vertically centred in
/// the band (`exp_h` = whichever is taller), which is also what centres the degenerate
/// portrait-plus-name page with no special case.
///
/// **Computed once per store change, not per frame** ([`Scene::header`]). It is the only thing on
/// this page that measures text, and it is reached from `Column::height(0)` — which `child_top`,
/// `content_h`, `scroll_target` and the pointer hit-test all call, several times a frame each.
#[derive(Clone, Copy, Default)]
struct HeaderFlow {
    /// the band height — `max(exp_d, text stack)`
    exp_h: f32,
    /// the portrait diameter: [`PORTRAIT_EXP`] normally, the smaller [`PORTRAIT_BARE`] when the
    /// header is nothing but a name. Recorded here rather than re-derived in the draw, so the
    /// measured band height and the drawn circle can never disagree about which one it is.
    exp_d: f32,
    /// **The portrait's own y offset from the band top.** 0 for a full header, which is the whole
    /// of the top-alignment rule: the circle's top edge sits on the name's cap top. A header that
    /// is nothing but a name centres both halves instead, and then this is the centring offset.
    portrait_y: f32,
    /// y offsets FROM THE BAND TOP of each present line's cap-top
    name_y: f32,
    meta_y: Option<f32>,
    life_y: Option<f32>,
    bio_y: Option<f32>,
    /// **The route-entry pill's own TOP** (not a cap band — it is a control, and its box is what is
    /// placed), from the band top. `None` when there is no filmography to enter.
    entry_y: Option<f32>,
}

fn header_flow(sc: &Scene) -> HeaderFlow {
    let mut f = HeaderFlow::default();
    let mut y = crate::text::cap_h(theme::size::DISPLAY, 1); // the name; everything below stacks from its bottom
    // **While the plex.tv profile has not answered, ASSUME it will have a full header** — a role
    // line, a life line, a bio — and reserve exactly the space the real content would take once it
    // lands, at the SAME per-line heights `cap_h` gives the real runs. This is the fix for the
    // reflow the design's `loadState` machinery exists to prevent: without it a header opens BARE
    // (no roles/life/bio yet, so `header_flow` sees three empty strings) at the smaller centred
    // portrait, and then jumps to the full top-aligned 320px layout the instant the profile lands
    // — on ordinary hardware, well inside a second, which read as the page hiccuping rather than
    // loading.
    //
    // It is a GUESS, not a certainty — a person whose real answer turns out bare (no roles, no
    // life, no bio: rare, but real) still shrinks once `facts_pending` clears, and that shrink is
    // not solved here. What is solved is the common case, which is that a person HAS these lines,
    // and reserving the smaller bare geometry for all of them was actively wrong for the common
    // case to save the rare one.
    let pending = crate::person::current().is_some_and(crate::person::facts_pending);
    if pending || !sc.roles_c.as_bytes().is_empty() {
        y += META_GAP;
        f.meta_y = Some(y);
        y += crate::text::cap_h(theme::size::LABEL, 0);
    }
    if pending || !sc.life_c.as_bytes().is_empty() {
        // the life line hugs the kicker above it, but takes the kicker's own gap when it is the
        // first meta line — the gap belongs to the stack's top edge, not to a particular line
        y += if f.meta_y.is_some() {
            LIFE_GAP
        } else {
            META_GAP
        };
        f.life_y = Some(y);
        y += crate::text::cap_h(theme::size::LABEL, 0);
    }
    let bio = crate::person::current()
        .map(|p| p.bio.as_str())
        .unwrap_or("");
    if pending || !bio.is_empty() {
        y += BIO_GAP;
        f.bio_y = Some(y);
        // No text to wrap yet, so the placeholder reserves the FULL [`BIO_LINES`] the real prose is
        // capped at — never less, or a short real bio would grow the block on arrival; never more,
        // or every arrival would shrink it.
        y += if pending && bio.is_empty() {
            BIO_LEAD * BIO_LINES as f32
        } else {
            bio_view(bio, 1.0).measure_h(BIO_W)
        };
    }
    // …and the route entry, at the END of the identity column rather than after the shelves — see
    // [`ENTRY_H`]. It is the last thing in the band, so the band's height carries it and every
    // scroll target below it moves with it for free.
    if crate::person::current().is_some_and(has_entry) {
        y += ENTRY_GAP;
        f.entry_y = Some(y);
        y += ENTRY_H;
    }
    // A header that is nothing BUT a name wears the smaller portrait: 320px of headshot beside one
    // line reads as a portrait with a caption.
    let bare = !pending && f.meta_y.is_none() && f.life_y.is_none() && f.bio_y.is_none();
    f.exp_d = if bare { PORTRAIT_BARE } else { PORTRAIT_EXP };
    f.exp_h = y.max(f.exp_d);
    // **TOP-ALIGNED on the name's CAP TOP** — the design's `align-items: flex-start`, and the band
    // is as tall as whichever half is taller. Both offsets are 0 for a full header, so the circle's
    // top edge and the name's caps start on the same line; the module doc has the argument, and
    // why the mock's `padding-top`/`margin-top` compensations are CSS artifacts we must not port.
    //
    // **A SPARSE header centres instead**, since there is no block to align to: one line beside a
    // circle top-aligned reads as a caption that slipped, and the degenerate page has to look
    // composed rather than broken. Centring BOTH in the band makes their centres coincide whichever
    // is taller, with no special case for which.
    if bare {
        f.portrait_y = (f.exp_h - f.exp_d) * 0.5;
        let ty = (f.exp_h - y) * 0.5;
        f.name_y = ty;
        for v in [&mut f.meta_y, &mut f.life_y, &mut f.bio_y]
            .into_iter()
            .flatten()
        {
            *v += ty;
        }
    }
    f
}

/// The bio block, in ONE place so its measure and its draw cannot disagree: 3 lines of body text
/// that **dissolve into a right-pinned [`MORE`]** when the cap actually hid words. The affordance
/// is a truncation MARK, not a control — nothing on this page expands text in place. `a` fades it
/// with the rest of the expanded header.
///
/// **The reserve is all this view owns of the affordance; the mark itself is drawn by
/// [`draw_header`].** That is `fade_last`'s shape, and it is the About card's — the last line is
/// painted through the fade shader (`shaders/fs_text_fade.frag`) so it vanishes *before* the
/// column's right edge, and the word is then pinned to that edge on the same cap band. It replaced
/// an inline `.trailing("MORE", …)`, which hard-elided the last line with an ellipsis and floated
/// the word at the text's own ragged end: one page of this app cut its prose off with `…`, the
/// other let it fade, for the same reason and one rung apart.
fn bio_view(bio: &str, a: f32) -> TextView<'_> {
    // `TEXT_READING`, not `TEXT_SECONDARY`: this is the longest run of prose on the page, and the
    // reading ink is the token for exactly that role — the same one `person_bio`'s full biography
    // and the route family's copy columns take, so the preview and the panel behind it are one
    // voice at two lengths.
    TextView::new(
        bio,
        theme::size::BODY,
        theme::with_a(theme::TEXT_READING, a),
    )
    .leading(BIO_LEAD)
    .max_lines(BIO_LINES)
    .fade_last(more_w() + BIO_MORE_GAP)
}

/// **The TEXT COLUMN's width** — from the column's left edge to the page's right margin, i.e. what
/// `flex: 1 1 auto` resolves to in the mock. The name and the two meta lines are elided against
/// this; only the bio narrows, to [`BIO_W`]'s reading measure.
///
/// A function rather than a constant because the column's x depends on the portrait diameter, which
/// depends on whether the header is a bare one. It is called from [`refresh_runs`] (once per store
/// change) and from the draw, where it is three additions.
fn text_w(d: f32) -> f32 {
    SCR_W - MARGIN_X - col_x(d)
}

/// The text column's left edge, beside a portrait of diameter `d`.
fn col_x(d: f32) -> f32 {
    MARGIN_X + d + BAND_GAP
}

/// [`MORE`]'s drawn width at the bio's own rung and weight — `size::BODY` bold, NOT the About
/// card's `size::CAPTION`. Only the reserve reads it: the mark is drawn right-ALIGNED on the
/// column edge, so its own draw needs no width at all.
fn more_w() -> f32 {
    crate::text::text_width(MORE.as_ptr(), theme::size::BODY, 1)
}

/// Rebuild the header's cached text runs from the store — from [`update`], on the frame a store
/// change lands. They are `format!`s over store fields, and doing that 60 times a second for
/// strings that change twice in a page's life is exactly the per-frame allocation the shelf tiles
/// avoid.
///
/// **The life line is a stack of independent facts, joined only where both exist**: a living person
/// simply has no Died clause, a person with only a death date reads "Died 2 Jun 2017", and someone
/// plex.tv knows nothing about produces empty strings, which the flow then gives no room to at all.
/// That is why this builds a LIST and joins it rather than filling one template with holes.
fn remeasure_header(sc: &mut Scene) {
    refresh_runs(sc);
    // the flow MEASURES the runs above, so it can only be computed after them — which is why the
    // two live behind one entry point instead of two calls a caller must order correctly
    sc.header = header_flow(sc);
    sc.header_dirty = false;
}

fn refresh_runs(sc: &mut Scene) {
    let Some(p) = crate::person::current() else {
        sc.roles_c = CString::default();
        sc.life_c = CString::default();
        sc.shelf_count_c = [CString::default(), CString::default()];
        return;
    };
    // The name is rebuilt (and budgeted) HERE, not at `open` — it is the one header run every page
    // has, and the only one that would otherwise draw unmeasured: a HERO-size "Daniel Michael Blake
    // Day-Lewis" runs past 1300px. Same column budget as everything under it.
    // The column budget every single-line run in the band shares. The portrait is not measured yet
    // on the first pass (the flow that decides it is computed AFTER these runs — that is the whole
    // reason `remeasure_header` orders the two), so the runs are budgeted against the WIDER of the
    // two diameters, i.e. the narrower column. A bare header's name then has room to spare rather
    // than being elided against a column it does not have.
    let w = text_w(PORTRAIT_EXP);
    sc.name_c = cstr_elide(&p.name, w, theme::size::DISPLAY, 1);
    sc.roles_c = cstr_elide(&p.roles, w, theme::size::LABEL, 0);

    let mut life: Vec<String> = Vec::new();
    let born = crate::ui::fmt::pretty_date(&p.born, 0);
    if !born.is_empty() {
        life.push(match p.birthplace.is_empty() {
            true => format!("Born {born}"),
            false => format!("Born {born}, {}", p.birthplace),
        });
    }
    let died = crate::ui::fmt::pretty_date(&p.died, 0);
    if !died.is_empty() {
        life.push(format!("Died {died}"));
    }
    sc.life_c = cstr_elide(&life.join(" \u{b7} "), w, theme::size::CAPTION, 0);

    // Each heading's count prints the RESPONSE total, not the tile list's length: the shelves cap at
    // a `CardRow`'s spring count, and "24" on a person with 60 movies would be the cap posing as a
    // fact about the person.
    for k in 0..NSHELF {
        sc.shelf_count_c[k] = match p.total(k) {
            0 => CString::default(),
            n => CString::new(n.to_string()).unwrap_or_default(),
        };
    }
    // …and the Filmography entry's count, on the same rule and for the same reason: it is the
    // credits response's own total, and it changes exactly when a landing does.
    sc.entry_count_c = match crate::person::filmography_total(p) {
        0 => CString::default(),
        n => CString::new(n.to_string()).unwrap_or_default(),
    };
}

/// A header meta line, NUL-terminated and clipped to `w`. `Painter` has no clip, so an over-long
/// line (a birthplace with four commas in it) would otherwise paint out past the bio beneath it;
/// `text::elide` is the shared measurer that cuts it at the same width. `cont` is FALSE — that
/// flag is `TextView`'s continued-line mode and marks the run with an ellipsis even when nothing
/// was cut, which put a stray `…` on every kicker and every Born/Died line that simply fit.
fn cstr_elide(s: &str, w: f32, sz: c_int, bold: c_int) -> CString {
    if s.is_empty() {
        return CString::default();
    }
    CString::new(crate::text::elide(s, w, sz, bold, false)).unwrap_or_default()
}

/// Total flowed content height (last child's bottom + the bottom air).
fn content_h(sc: &Scene, c: &impl Column) -> f32 {
    let col = sc.column;
    let last = c.len().saturating_sub(1);
    col.child_top(c, last) + c.height(last) + BOTTOM_PAD
}

/// A shelf block's flowed height: heading + poster row + the focused tile's label band.
///
/// The band is ASKED FOR, not restated — `card_row` lays that block out and
/// [`card_row::TileLabel::height`] is its only authority on how tall it is. Getting it wrong is not
/// cosmetic: [`reveal_block`] guarantees [`BOTTOM_PAD`] under the block a screen DECLARES, so a
/// short declaration runs the character caption off the bottom of the panel — which is exactly what
/// a hand-authored 112 did here, because it assumed the mock's 24px poster→title drop where the
/// shared code uses 30.
fn shelf_block_h() -> f32 {
    shelf_block_h_at(card_row::UNDER_LABEL_H)
}

/// …the same, at an arbitrary label band — the collapsing half of the rule above. A shelf that does
/// not hold focus draws no label block at all, so it reserves `card_row::LABEL_BAND_COLLAPSED`
/// instead of the whole of it and the shelf below moves up by the difference.
///
/// The declaration contract is unchanged and still load-bearing: [`reveal_block`] guarantees
/// [`BOTTOM_PAD`] under the block a screen DECLARES, and the shelf whose scroll is being revealed
/// is by construction the FOCUSED one — whose band is open — so the caption still clears the panel.
fn shelf_block_h_at(band: f32) -> f32 {
    SHELF_LABEL_H + CARD_H + band
}

/// The MINIMAL scroll that reveals the block `[top, top+h)` inside `content` px of flow — the
/// shared [`reveal`] rule the shelves and the Library grid use, NOT a lift-to-margin pin: a block
/// scrolls only as far as its own extent needs, and not at all when it already fits.
///
/// Pure, and kept separate from [`scroll_target`] deliberately — the flow that feeds it is measured
/// through the text stack, which the host suite cannot link, so this is the layer the geometry
/// tests can actually reach.
fn reveal_block(cur: f32, top: f32, h: f32, content: f32) -> f32 {
    let lo = top + h - (SCR_H - BOTTOM_PAD);
    let hi = top - TOP_MARGIN;
    reveal(cur, lo, hi, (content - SCR_H).max(0.0))
}

fn scroll_target(sc: &Scene) -> f32 {
    let col = sc.column;
    let Some(fi) = sc.focus_child() else {
        return 0.0;
    };
    // …measured against the SETTLED column, never the live one. See [`Settled`].
    let st = Settled(sc, fi);
    reveal_block(
        col.scroll.pos,
        col.child_top(&st, fi),
        st.height(fi),
        content_h(sc, &st),
    )
}

/// **[`Scene`] with every shelf's label band at its DESTINATION** — open on the focused shelf,
/// closed on all the others — rather than wherever the springs have it this frame.
///
/// A scroll target has to be measured against this. The band and the scroll are two springs
/// travelling at one rate to two destinations, so a target derived from the live column moves every
/// frame while the column chases it and the block arrives and then drifts.
/// `card_row::settled_top` carries the argument in full.
///
/// A `Column` view rather than a second copy of [`ScrollColumn::child_top`]'s summation: the
/// running sum stays in one place, and the only thing this overrides is where each child's height
/// comes from. `draw_child` is unreachable through it — nothing draws a settled column — and says
/// so rather than silently drawing the live one.
struct Settled<'a>(&'a Scene, usize);
impl Column for Settled<'_> {
    fn len(&self) -> usize {
        self.0.len()
    }
    fn height(&self, i: usize) -> f32 {
        let n = crate::person::current().map(nshelves).unwrap_or(0);
        match i {
            0 => self.0.band_h(),
            i if i <= n => shelf_block_h_at(card_row::under_band((self.1 == i) as i32 as f32)),
            _ => ENTRY_H,
        }
    }
    fn gap_before(&self, i: usize) -> f32 {
        self.0.gap_before(i)
    }
    fn focus_child(&self) -> Option<usize> {
        Some(self.1)
    }
    fn draw_child(&self, _i: usize, _env: &Env, _p: Painter) {
        debug_assert!(false, "the settled column is a measurement, never a draw");
    }
}

// ---- the ambient wash ------------------------------------------------------------------------

/// The wash's target corners (tl, tr, br, bl), each mixed from the app surface toward a source
/// colour: the focused poster's UltraBlur corners while a card holds focus, else the faint warm
/// header tint. A tile whose art carried no blur envelope keeps the header tint too — a wash
/// invented from nothing would flash grey on one poster in a row of colour.
///
/// Mixing FROM [`theme::SURFACE_APP`] is what makes "no artwork" the app's own flat ground with no
/// special case, and it is why the corner weights below can be read as "how much of this poster
/// shows through". That mix is [`AmbientWash::target`]'s contract now, not a loop written here.
///
/// The card corners go through [`AmbientWash::keyed`], so a white poster cannot outshine the role
/// captions sitting on the ground under it — see `widgets::GROUND_LUMA`. The header tint is a
/// palette token we chose, so it needs no cap.
fn amb_target(sc: &Scene) -> [[f32; 4]; 4] {
    match crate::person::current()
        .and_then(|p| focused_of(p, sc))
        .filter(|m| m.has_blur)
    {
        Some(m) => AmbientWash::keyed(m.blur, AMB_CARD_W),
        None => AmbientWash::target([theme::WASH_WARM; 4], AMB_HEADER_W),
    }
}

// ---- entry / exit ----------------------------------------------------------------------------

/// Mount the page for a cast member — everything `Role[]` already carries (`key` = the local
/// personId, `guid` = the `tagKey` plex.tv answers to). Loads the store's header immediately and
/// resets this screen's focus / scroll / band state, WITHOUT raising the routing latch.
///
/// The shared half of [`open`] and of the BACK trail's re-entry (`app.rs`'s `enter_node`), which
/// must not raise the latch: it is already doing the routing, and a latch left set there would be
/// drained on the same frame and push back the very node the pop just took off.
///
/// **The page opens on the Filmography ENTRY row** — at scroll 0, header expanded — owner
/// directive 2026-09-06 (module doc, and [`Scene::on_entry`]): shelves land a moment later and keep
/// reshaping while they do, so the mount parks on the one landing spot that load cannot move, and
/// [`clamp_focus`] resolves it once credits have genuinely settled (or are never going to — see its
/// `p.guid.is_empty()` clause). The header itself is still reachable — it is a scroll POSITION, not
/// a selection (see [`move_focus`]) — but it is not where a fresh mount lands either.
pub(crate) fn reopen(sid: crate::plex::ServerId, key: &str, guid: &str, name: &str, thumb: &str) {
    // **Every overlay this screen owns dies with the person it was opened for**, and mounting a new
    // one is exactly as much a reason as leaving the page. This was `leave`'s alone, and the gap it
    // left is reachable in three presses: open A's Filmography, press a credit that IS in your
    // library (which navigates FORWARD, so `leave` is never called — Detail and Person both stay on
    // the trail), then open person B from that page's cast row. B mounts through here, the route is
    // Person again, and the route overlay is still up holding A's model — so B's page draws A's
    // filmography over it, and a press there acts on A's credit while the trail says B.
    hide_overlays();
    crate::person::open(sid, key, guid, name, thumb);
    let sc = scene();
    // A WHOLESALE reset, not a field-by-field one: every default this page mounts with — focus on
    // the header, scroll and condense at 0, shelves at their left edge, EMPTY text runs — is already
    // spelled in `Scene::new`, and a field added there must not need remembering here too. The runs
    // in particular must start empty so the previous person's dates cannot survive into this one for
    // the length of a fetch; `header_dirty` then has `remeasure_header` build them on the next
    // `update`, which keeps the page's only text measure off `detail::on_ok`'s input path.
    *sc = Scene::new();
    // the wash starts AT the header tint — the previous person's poster colours must not dissolve
    // across the new page's mount
    let k = amb_target(sc);
    sc.amb.jump_target(k);
}

/// [`reopen`] plus the flag app.rs routes on — the INTERACTIVE entry, called from `detail.rs`'s
/// cast-row OK arm.
pub(crate) fn open(sid: crate::plex::ServerId, key: &str, guid: &str, name: &str, thumb: &str) {
    reopen(sid, key, guid, name, thumb);
    scene().requested = true;
}

/// app.rs polls this right after a detail-page OK: true exactly once per [`open`]. Keeping the
/// request here (rather than a second return channel out of `detail::on_ok`) is what lets the
/// cast arm stay four lines in a file eight other changes are landing in.
pub(crate) fn take_request() -> bool {
    let sc = scene();
    std::mem::replace(&mut sc.requested, false)
}

/// **Tear down every overlay this screen owns, with no animation** — [`leave`]'s half and
/// [`reopen`]'s, in one place so a THIRD overlay cannot be added to one and forgotten in the other.
///
/// No animation, deliberately, in both callers: the page these belong to is going or being
/// replaced, so a sheet fading out over whatever arrives next is a frame of the previous person's
/// biography or credits on somebody else's page.
fn hide_overlays() {
    crate::ui::person_bio::hide();
    crate::ui::filmography::hide();
}

/// BACK: leave the page. The page UNDERNEATH is put back by `app.rs`'s BACK trail (`ui::trail`),
/// which knows the whole history — this used to re-open a single remembered `from_rk`, which was
/// right for exactly one level and lost person → detail → person entirely (the second BACK went to
/// Home rather than to the first actor).
pub(crate) fn leave() {
    // drop any un-consumed request: `requested` is a LATCH, and one left set here would fire on
    // some unrelated OK several screens later
    scene().requested = false;
    hide_overlays();
    crate::person::close();
}

/// A panel this SCREEN has open takes the BACK press, and the page stays — `detail::back()`'s shape
/// one screen over, and for the reason that one records: a panel is part of the screen, so leaving
/// the screen must not be how you close it. `false` means there was nothing of ours to close and
/// `app.rs` should pop the BACK trail.
pub(crate) fn back() -> bool {
    // The route first: it is drawn over the panel's own layer and is the thing on screen, so a BACK
    // while both are somehow up must close what the user is looking at.
    if crate::ui::filmography::is_open() {
        crate::ui::filmography::close();
        return true;
    }
    if crate::ui::person_bio::is_open() {
        crate::ui::person_bio::close();
        return true;
    }
    false
}

/// **Is there more biography than the header shows?** — the ONE gate on the bio panel, and it is
/// deliberately the same call the truncation MARK is drawn from ([`draw_header`]), sharing the same
/// memoised wrap.
///
/// Written as one predicate rather than repeated at the two call sites because the mark and the
/// panel must never disagree: a panel reachable on a bio that fits would open on the words the user
/// has just finished reading, and a `MORE` with nothing behind it is worse still.
pub(crate) fn bio_is_truncated() -> bool {
    crate::person::current()
        .map(|p| !p.bio.is_empty() && bio_view(&p.bio, 1.0).truncates(BIO_W))
        .unwrap_or(false)
}

/// **The bio block's focus-mark rect, or None when there is no mark to draw.**
///
/// **The block that OK opens wears the page's selectable-block mark** — the shared
/// [`crate::ui::widgets::text_block_highlight`], i.e. the same lift the detail page's About card
/// and its three columns take. It is a GROUND, not a frame around the prose.
///
/// Owner call, and it narrows a decision [`header_ok`] records rather than reversing it: what was
/// rejected there was making the bio a SECOND FOCUS STOP inside the band, and it still is not one —
/// the whole band is one stop, and this draws no ring, no scale and no dip. What it fixes is that
/// the band was the only focusable thing in the app that showed nothing at all for holding focus,
/// so the one block OK acts on looked like prose that happened to end in a word. `MORE` stays a
/// mark; the lift is what says the mark can be pressed.
///
/// Gated on [`header_ok`]'s condition in its two halves — focus is here, and there is more to read —
/// PLUS [`Scene::header_marked`], so a page nobody has navigated to the top of draws none. The
/// truncation half is asked of THIS view rather than through [`bio_is_truncated`], so the lift, the
/// `MORE` beside it and the panel behind them are one expression on one memoised wrap and cannot
/// disagree by a frame.
///
/// Split out of [`draw_header`] because the plate has to be painted BEFORE the band's text (see
/// there) while the number it needs comes from the bio view at the BOTTOM of the flow. One
/// function, so the rect and the `MORE` ink that keys off its existence cannot disagree.
///
/// `last_line_cap_y` is where the view put the last line's cap band, so the height is measured to
/// the text rather than to the flow, which ends in a line of leading nothing is drawn in.
fn bio_mark_rect(person: &Person, sc: &Scene, flow: HeaderFlow, col_x: f32) -> Option<Rect> {
    let by = flow.bio_y?;
    if person.bio.is_empty() {
        return None;
    }
    let bio = bio_view(&person.bio, 1.0);
    if !bio_mark_visible(person, sc, bio.truncates(BIO_W)) {
        return None;
    }
    let bh = bio.measure_h(BIO_W);
    let ink = bio.last_line_cap_y(by, bh) - by + crate::text::cap_h(theme::size::BODY, 0);
    Some(Rect::new(
        col_x - HL_PAD_X,
        by - HL_PAD_Y,
        // …and it hangs [`HL_PAD_X`] outside the guide on BOTH sides, exactly as the detail page's
        // own `text_block_highlight` plates do (`EP_TEXT_HL_PAD_X`, padded left and right alike).
        // This one used to pad the left only, on the reasoning that the right side would then reach
        // past the frame's own right margin — but `col_x + BIO_W` is already `SCR_W - MARGIN_X`
        // (the prose runs to the SAME gutter the left margin mirrors), and `HL_PAD_X` (26) is well
        // inside `MARGIN_X` (96), so the right overhang lands exactly as safely into that gutter as
        // the left one already does into its own. The asymmetry was what put `MORE` — right-pinned
        // to this same column edge — flush against the plate's border instead of clear of it.
        BIO_W + 2.0 * HL_PAD_X,
        ink + 2.0 * HL_PAD_Y,
    ))
}

/// **Whether [`draw_header`] should draw the block's focus mark.** Pure so the fresh-mount /
/// after-navigation split can be asserted without a draw — see [`Scene::header_marked`] for what
/// sets the flag. `truncated` is threaded in rather than re-asked of the store, because
/// `draw_header` already holds the one memoised wrap this frame's `MORE` and lift agree on.
fn bio_mark_visible(p: &Person, sc: &Scene, truncated: bool) -> bool {
    on_header(p, sc) && sc.header_marked && truncated
}

/// OK while the HEADER holds focus. Returns whether the press was spent.
///
/// **This is the bio panel's entry point, and it is why the header's OK is no longer inert.** The
/// header is already a focus row (flow child 0) but contains no CONTROL — nothing in it draws a
/// focus ring, `focus_is_card` is false there, and `app.rs` therefore arms no tvOS press on it. So
/// this acts on the key DOWN rather than on a press spring-back: there is no card to dip, and
/// waiting ~150ms to animate a dip nobody can see would only add latency.
///
/// The alternative — making the bio block a real focusable inside the band — was rejected against
/// the page as built, and still is. It would put a second focus stop inside a row this module
/// documents four times over as a scroll POSITION rather than a selection, and give the condense a
/// state it has no mock for. Here the whole header band is the target: focus is already at the top
/// of the page, and OK there reads the rest.
///
/// **What that rejection used to include, and no longer does, is the MARK.** The argument ran that
/// a focus mark on the prose would turn `MORE` into a control — but the band was then the only
/// focusable thing in the app that drew nothing at all for holding focus, so the block OK acts on
/// was indistinguishable from prose that ends in a word. [`draw_header`] now draws the shared
/// `widgets::text_block_highlight` under the bio on this function's predicate NARROWED by
/// [`bio_mark_visible`] — the two agree on focus-is-here and there-is-more-to-read, but the mark
/// also needs [`Scene::header_marked`], so a fresh mount (or a landing that leaves the header as
/// the only row) does not draw it before the user has pressed a key toward the header at all.
/// `MORE` is unchanged and is still a mark; the lift is the page saying which block the press will
/// reach.
/// **Is either overlay this page owns already up?** Split out of [`header_ok`] so a host test can
/// call it: `header_ok` itself reaches `bio_is_truncated`'s real SDL2_ttf text measurement on the
/// branch below, which is exactly the trap `ui/CLAUDE.md` records for `person::move_focus` —
/// something reachable from a key handler measuring text does not fail one test, it stops the
/// whole suite LINKING. A host test calling `header_ok()` directly proved that the hard way this
/// session: `bio_is_truncated` had never been linker-reachable from any test before, and one call
/// site was enough to pull it (and `TTF_SizeUTF8`) into the default host build. This predicate has
/// no such reach, so it is the one half of the guard a test can hold.
fn overlay_owns_the_press() -> bool {
    crate::ui::filmography::is_open() || crate::ui::person_bio::is_open()
}

pub(crate) fn header_ok() -> bool {
    // …and the ENTRY row's OK, which is the other press this page spends itself rather than handing
    // to app.rs: it opens the Filmography route, which is an overlay this screen owns exactly as
    // the bio panel is. Both live here because `app.rs`'s person arm asks one question — did the
    // page consume the press — and the answer for both is yes.
    //
    // **Bail out first when either overlay is already open.** `app.rs` reaches this function
    // whenever `focus_is_card()` is false for `Route::Person` — and while an overlay is up, that is
    // true not only for the base page's own header/entry rows but for the overlay's OWN focused-
    // but-not-a-card rows too (a filmography credit nobody holds). `sc.on_entry`/`on_header` are
    // PARKED at whatever they were when the overlay opened, not live navigation state, so without
    // this guard a press on an unavailable filmography credit fell through to `sc.on_entry` still
    // reading true from the entry press that opened the route, and unconditionally called
    // `filmography::open()` AGAIN — resetting the overlay's own focus back to its tab strip on
    // every such press. `app.rs` never arms a tvOS press for that row (it is not a card) and so
    // never reaches [`on_ok`] either, which would have answered `Action::None` for it; the correct
    // outcome for a credit nobody holds is that OK does nothing at all, which returning `false` here
    // gives it directly.
    if overlay_owns_the_press() {
        return false;
    }
    let sc = scene();
    if sc.on_entry {
        if crate::person::current().is_some_and(has_entry) {
            crate::ui::filmography::open();
            return true;
        }
        return false;
    }
    let Some(p) = crate::person::current() else {
        return false;
    };
    if !on_header(p, sc) || !bio_is_truncated() {
        return false;
    }
    // A press that opens the panel is exactly as much "landing on the header by the D-pad" as an
    // UP that keeps it there — see [`Scene::header_marked`]. Whether or not the mark was showing
    // a frame ago, this press proves the user found the block.
    scene().header_marked = true;
    crate::ui::person_bio::open();
    true
}

/// The focused shelf card (None while the shelves are still out, and None while the HEADER holds
/// focus — it contains no control) — app.rs opens its detail page.
pub(crate) fn focused_item() -> Option<&'static PmsMovie> {
    let sc = scene();
    let p = crate::person::current()?;
    focused_of(p, sc)
}

/// **What OK NAVIGATES TO — the `(server, ratingKey)` pair, from the shelves OR from an open
/// Filmography row.** `app.rs`'s `open_person_card` asks this; nothing else does.
///
/// **It is deliberately NOT [`focused_item`], and conflating the two was a real defect.** The first
/// version of this route answered the Filmography's rows through `focused_item` — minting a
/// `PmsMovie` carrying only `sid` and `rk` — because that made navigation work with no new arm in
/// `app.rs`. It also made every OTHER consumer of that function wrong: the press-and-hold context
/// menu builds its rows from the catalog item it is handed, so a credit row offered *Play from
/// start* (guaranteed to do nothing — `route::request_play_movie` refuses an empty `part`), called
/// every show a movie, and drew watch-state rows from a default that describes nothing.
///
/// A Discover credit **is not a catalog row and cannot be made into one** without fetching it, so
/// the fix is not a fuller stand-in: it is that these are two questions. `focused_item` means "the
/// catalog item under focus" and answers `None` while the route is up, which is exactly how the
/// three other card surfaces decline a hold (`app.rs`'s menu arm says so in those words) — the hold
/// falls through to an ordinary spring-back and no menu opens. This one means "where does OK go",
/// and a pair is all it ever needed.
pub(crate) fn focused_target() -> Option<(crate::plex::ServerId, String)> {
    if crate::ui::filmography::is_open() {
        return crate::ui::filmography::focused_item();
    }
    focused_item().map(|m| (m.sid, m.rk.clone()))
}

/// The only focusABLE thing on this page is a poster card, so OK always takes the tvOS press (dip
/// on key-down, activate on the spring-back). Named to match the other screens' predicate so
/// app.rs's press arm reads the same for all of them.
///
/// False on the header row: it carries no card to dip. OK there is not inert any more — it opens
/// the bio panel ([`header_ok`]) — but that is an overlay rather than an activation, so it takes no
/// press. And false while the bio panel is UP, for `detail::focus_is_card`'s reason: without it an
/// OK held over the panel would begin a tvOS press on a tile nobody can see and commit it on
/// release, opening a detail page from behind a modal.
pub(crate) fn focus_is_card() -> bool {
    if crate::ui::filmography::is_open() {
        // the route's own rule: only a credit with a server behind it arms a press
        return crate::ui::filmography::focus_is_card();
    }
    !crate::ui::person_bio::is_open() && focused_item().is_some()
}

pub(crate) fn on_ok() -> Action {
    if crate::ui::filmography::is_open() {
        // The route answers with the pair a Discover row can actually give — `app.rs` reads it
        // back through `focused_target`, so nothing here has to invent a catalog row.
        return match crate::ui::filmography::on_ok() {
            crate::ui::filmography::Action::Open(..) => Action::Card,
            crate::ui::filmography::Action::None => Action::None,
        };
    }
    // nothing behind an open panel is activatable — the panel's own OK is inert (it is a reader,
    // not a chooser), so the press is simply swallowed here
    if crate::ui::person_bio::is_open() {
        return Action::None;
    }
    if focused_item().is_some() {
        Action::Card
    } else {
        Action::None
    }
}

// ---- update ----------------------------------------------------------------------------------

pub(crate) fn update(dt: f32) {
    // The focused card's IDENTITY, taken BEFORE the pump. The shelves are a merge across every
    // source now (`person::merge_shelves`), and a landing re-divides the row's budget between them
    // — so a bare column index silently comes to mean a different film the moment a slow share
    // answers. Before this, a share landing two seconds in slid the card out from under the user's
    // focus and the next OK opened something they were not looking at.
    let was = focused_id();
    // `clamp_focus` and the dirty flag below both reach `scene()` themselves, so they run BEFORE
    // this function takes its own `&'static mut` — holding one across a call that mints a second is
    // aliasing UB, not a lint. (`detail.rs::update` carries the same note for the same reason.)
    if crate::person::pump() {
        reseat(was);
        clamp_focus();
        scene().header_dirty = true; // a landing changes the header runs AND the flow
        // …and the filmography's availability join, whose whole point is that a share landing five
        // seconds in turns "no server behind this" into a real annotation. See that module's
        // `mark_dirty`.
        crate::ui::filmography::mark_dirty();
    }
    let sc = scene();
    // Remeasure BEFORE anything reads the flow this frame: `scroll_target` below, and every draw
    // after it, go through `Column::height(0)`, which is now a plain field read.
    if sc.header_dirty {
        remeasure_header(sc);
    }
    sc.spin_ms += dt * 1000.0;
    // the wash: dissolve toward whatever the page is about now
    let k = amb_target(sc);
    sc.amb.key_target(k, dt);
    let n_items = |k: usize| {
        crate::person::current()
            .map(|p| p.shelf(k).len())
            .unwrap_or(0)
    };
    let shelf_has_focus = crate::person::current()
        .map(|p| !on_header(p, sc) && !sc.on_entry)
        .unwrap_or(false);
    for k in 0..NSHELF {
        // a shelf that does not hold focus freezes its scroll (CardRow's `None` contract) — the
        // same behaviour a non-focused home shelf has. The header or the entry row holding focus
        // freezes them ALL, which is what a page whose subject is elsewhere should do.
        let n = n_items(k);
        let focus =
            (shelf_has_focus && k == sc.focus_kind && n > 0).then(|| sc.col[k].max(0) as usize);
        sc.shelves[k].update(n, focus, &SHELF_STYLE, dt);
    }
    let want = scroll_target(sc);
    sc.column.scroll.step(want, K_SCROLL, dt);
    // the bio panel's and the Filmography route's own springs. Unconditional, like every other
    // popover's `update` — a closed one steps nothing.
    crate::ui::person_bio::update(dt);
    crate::ui::filmography::update(dt);
}

/// The focused card's IDENTITY — the `(server, ratingKey)` PAIR, never the bare key. The shelves
/// merge two sources into one row and both servers number their items from 1, so a bare key names a
/// card on neither of them in particular (`plex::same_item`). None while the header holds focus,
/// which is a state [`reseat`] then correctly leaves alone.
fn focused_id() -> Option<(crate::plex::ServerId, String)> {
    focused_item().map(|m| (m.sid, m.rk.clone()))
}

/// Put focus back on the card it was on, wherever a landing has since moved it.
///
/// [`clamp_focus`] cannot do this: it range-clamps an INDEX, and the index is exactly what stops
/// meaning the same thing when `person::merge_shelves` re-divides the row between the sources. A
/// no-op when the card is gone (its source dropped out, or the cap pushed it off the row) — the
/// clamp that follows then does what it always did.
fn reseat(was: Option<(crate::plex::ServerId, String)>) {
    let Some((sid, rk)) = was else { return };
    let Some(p) = crate::person::current() else {
        return;
    };
    for kind in 0..NSHELF {
        let found = p
            .shelf(kind)
            .iter()
            .position(|m| crate::plex::same_item((m.sid, &m.rk), (sid, &rk)));
        if let Some(i) = found {
            let sc = scene();
            sc.focus_kind = kind;
            sc.col[kind] = i as c_int;
            return;
        }
    }
}

/// Keep the focus inside whatever the store currently holds — after a landing, and after every
/// navigation. A shelf can appear (the fetch lands) or be absent entirely, so the focused KIND is
/// re-seated onto the first present shelf rather than left pointing at nothing.
fn clamp_focus() {
    let sc = scene();
    let Some(p) = crate::person::current() else {
        sc.on_header = false;
        sc.on_entry = false;
        sc.focus_kind = 0;
        sc.col = [0; NSHELF];
        return;
    };
    let (kinds, n) = present(p);
    // **A page with no shelves does NOT write the header back into the flag** — [`on_header`]
    // derives that, and writing it would be the bug the derivation exists to prevent: the flag
    // would then still say "header" the frame the shelves land, so a page that opened on the first
    // card would need a second rule to move focus back off a header nobody chose.
    if n > 0 && !kinds[..n].contains(&sc.focus_kind) {
        sc.focus_kind = kinds[0];
    }
    // …but an ENTRY row that has gone away — genuinely, not merely because credits have not
    // ANSWERED yet — really must give its focus back, because that is a position on the page and
    // not a fallback: it lands on the last shelf, or on the header when there is none.
    //
    // **`p.credited` gates it, and the gate is load-bearing.** [`has_entry`] is `p.credited &&
    // filmography_total(p) > 0`, so before the credits fetch has answered it already reads false —
    // indistinguishable, from this clause alone, from a person who truly has no filmography. Firing
    // on that false would undo the mount default the moment the page's first `update` runs, for
    // EVERY person, landing back on `kinds[0]` and reintroducing the jump [`Scene::on_entry`]'s doc
    // describes. Held here until `p.credited` is actually true, a fresh mount stays parked on the
    // entry row through the whole load — visible or not — and only resolves once, to wherever the
    // real answer says: the entry row if there is one, a shelf or the header if there truly is not.
    // **`p.guid.is_empty()` releases it too, immediately, never waiting.** A credit whose PMS row
    // carried a numeric id but no tagKey (`detail.rs`'s cast-row OK arm keys the page on whichever
    // of the two the row actually has, and either can be the one missing) opens this page with an
    // empty guid — and `address()` gates BOTH the profile and the credits fetch on a non-empty one,
    // so `p.credited` is not merely slow here, it is NEVER GOING TO become true. Waiting on it would
    // maroon the mount on the entry row for the rest of the page's life, with no key that escapes it
    // apart from an explicit UP/DOWN — for a person whose shelves (keyed on name/local id, not guid)
    // can perfectly well have landed. A person with a guid holds until `credited` genuinely answers;
    // a person with no guid has already had its answer, and it is "never asked".
    if sc.on_entry && (p.credited || p.guid.is_empty()) && !has_entry(p) {
        sc.on_entry = false;
        // …and NOT `sc.on_header = n == 0`, the write this line carried while [`Scene::on_entry`]
        // mounted false and this branch was reachable only by real navigation off a since-vanished
        // entry. Now that a fresh mount reaches it too whenever there is no filmography to hold,
        // latching `on_header` here would break the same unlatched contract [`on_header`]'s
        // derivation exists for: `!sc.on_entry && (sc.on_header || present(p).1 == 0)` already
        // answers `true` for an empty page without the raw flag saying so, exactly as a fresh mount
        // with shelves needs the flag to stay clear for the frame they land.
    }
    for k in 0..NSHELF {
        let last = p.shelf(k).len() as c_int - 1;
        sc.col[k] = sc.col[k].clamp(0, last.max(0));
    }
}

// ---- input -----------------------------------------------------------------------------------

/// D-pad. Rows are **the header, the present shelves, then the Filmography entry** — the same
/// "hero is a row" model `detail.rs` uses (its section 0), with the entry as the floor.
///
/// The header row predates this band: it was what made the portrait reachable at all when the first
/// header outgrew the fold. It is not what a mount lands on any more (module doc), but it is still
/// the page's top scroll position and it is what OK opens the biography from. UP into it is a
/// scroll, not a selection — nothing draws a focus ring — and DOWN returns to the shelf that had it.
///
/// **Each row of the page owns exactly one meaning for OK**: the header opens the biography, a
/// shelf opens the focused title, the entry replaces the page with the Filmography route.
pub(crate) fn move_focus(sym: c_uint) {
    // The bio panel and the Filmography route take the nav keys while they are up — focus is
    // TRAPPED in each, as it is in every other overlay this app opens (`detail::move_focus` carries
    // the same lines for the "Also available" sheet). Without this the shelves would walk under
    // them and the page would scroll behind.
    if crate::ui::filmography::is_open() {
        crate::ui::filmography::move_focus(sym);
        return;
    }
    if crate::ui::person_bio::is_open() {
        crate::ui::person_bio::move_focus(sym);
        return;
    }
    let sc = scene();
    let Some(p) = crate::person::current() else {
        return;
    };
    let (kinds, n) = present(p);
    let entry = has_entry(p);
    if on_header(p, sc) {
        // LEFT/RIGHT do nothing on a row with one item, and UP is already at the top — but any of
        // those presses (and a DOWN with nothing below to reach) is still an explicit press that
        // LANDS here, which is what [`Scene::header_marked`] gates on. Only a DOWN that actually
        // leaves the header takes no mark, since the header is no longer what is drawn.
        // **DOWN off the band reaches the entry PILL first**, because the pill is IN the band now
        // (see [`ENTRY_H`]) and sits below the bio it follows. Walking past it straight to the
        // shelves would make the one control in this column unreachable by the key that points at
        // it. With no filmography, DOWN goes to the shelves exactly as before.
        match (sym, entry, n > 0) {
            (SDLK_DOWN, true, _) => {
                sc.on_header = false;
                sc.on_entry = true;
            }
            (SDLK_DOWN, false, true) => sc.on_header = false,
            _ => sc.header_marked = true,
        }
        clamp_focus();
        return;
    }
    if sc.on_entry {
        // The pill is the band's last stop: UP returns to the prose above it, DOWN leaves the band
        // for the first shelf. LEFT/RIGHT have nowhere to go — it is one control on its own line.
        match sym {
            SDLK_UP => {
                sc.on_entry = false;
                sc.on_header = true;
                sc.header_marked = true;
            }
            SDLK_DOWN if n > 0 => sc.on_entry = false,
            _ => {}
        }
        clamp_focus();
        return;
    }
    if n == 0 {
        return;
    }
    let pos = focus_pos(p, sc).unwrap_or(0);
    let k = sc.focus_kind;
    match sym {
        SDLK_LEFT => sc.col[k] = (sc.col[k] - 1).max(0),
        SDLK_RIGHT => {
            let last = p.shelf(k).len() as c_int - 1;
            sc.col[k] = (sc.col[k] + 1).min(last.max(0));
        }
        SDLK_UP if pos > 0 => sc.focus_kind = kinds[pos - 1],
        SDLK_UP if entry => sc.on_entry = true,
        SDLK_UP => {
            // ...and from the FIRST shelf, back to the portrait — an explicit arrival, so the
            // focus mark is what [`Scene::header_marked`] exists to show for the first time.
            sc.on_header = true;
            sc.header_marked = true;
        }
        SDLK_DOWN if pos + 1 < n => sc.focus_kind = kinds[pos + 1],
        _ => {}
    }
    clamp_focus();
}

/// Screen-space y of shelf `kind`'s poster row, given its flow position among the present
/// shelves. The ONE vertical geometry the draw and the pointer hit-test share.
fn shelf_row_y(sc: &Scene, pos: usize) -> f32 {
    sc.column.child_top(sc, pos + 1) - sc.column.scroll.pos + SHELF_LABEL_H
}

/// The shelf tile under the pointer, or None in the gaps.
///
/// **None while the bio panel is up**, which gates hover AND click in one place rather than in the
/// two callers: a modal owns the frame, so a cursor wandering over the page behind it must neither
/// light a card up nor activate one. (Its own hit test is the panel's; today it has none — the
/// panel is read with the D-pad and dismissed with BACK.)
///
/// O(VISIBLE), deliberately — the same discipline `library.rs::cell_at` documents. Two things
/// make it that: the per-shelf `row_y` is computed ONCE outside the column loop (calling it per
/// tile walks the whole `child_top` flow ~48 times per pointer motion), and the column is derived
/// arithmetically from x instead of being searched for. The flow walk is cheap in itself —
/// `Column::height(0)` reads the cached [`HeaderFlow`] through [`Scene::band_h`] rather than
/// measuring text — but the hoist stays: it is the part that keeps this O(VISIBLE).
fn tile_at(mx: f32, my: f32) -> Option<(usize, usize)> {
    if crate::ui::person_bio::is_open() || crate::ui::filmography::is_open() {
        return None;
    }
    let sc = scene();
    let p = crate::person::current()?;
    let (kinds, n) = present(p);
    for (pos, &kind) in kinds[..n].iter().enumerate() {
        let row_y = shelf_row_y(sc, pos);
        if my < row_y || my > row_y + CARD_H {
            continue; // not in this shelf's band
        }
        let slot = CARD_W + GAP;
        let x = mx - (MARGIN_X - sc.shelves[kind].scroll_x());
        if x < 0.0 {
            return None;
        }
        let i = (x / slot) as usize;
        // reject the inter-card gap: only the card's own width is a hit
        return (i < p.shelf(kind).len() && x - i as f32 * slot <= CARD_W).then_some((kind, i));
    }
    None
}

/// Pointer hover: focus follows the cursor onto a shelf card. Landing on a tile also LEAVES the
/// header — otherwise the hovered card would light up while the page still held itself at the top,
/// and the next OK would act on a card the scroll had already moved.
///
/// **An open overlay takes it first, and takes it WHOLE**, which is the same trap `move_focus` and
/// `on_ok` set for the keys — a modal that covers the page owns the pointer, or the page under it
/// lights up cards nobody can see and the next press acts on one of them. The route was missing
/// from both pointer arms when it shipped, which is the whole of the owner's "table view is not
/// interactable with the mouse" and "clicking jumps to Actor pill": every click fell through to the
/// person page beneath, which focused a shelf tile, which redrew the route from the top.
pub(crate) fn pointer_focus(mx: f32, my: f32) {
    if crate::ui::filmography::is_open() {
        crate::ui::filmography::pointer_focus(mx, my);
        return;
    }
    if crate::ui::person_bio::is_open() {
        return; // a reader has nothing to hover
    }
    // The entry pill first: it sits in the identity BAND, which `tile_at` does not walk at all.
    if entry_at(mx, my) {
        let sc = scene();
        if !sc.on_entry {
            sc.on_entry = true;
            sc.on_header = false;
            crate::ui::idle::invalidate();
        }
        return;
    }
    if let Some((kind, i)) = tile_at(mx, my) {
        focus_tile(kind, i);
    }
}

/// Is the pointer over the Filmography entry pill? From the box [`draw_entry`] recorded, so the
/// answer can never disagree with what was drawn — see [`ENTRY_PILL`].
fn entry_at(mx: f32, my: f32) -> bool {
    let Some(r) = (unsafe { *addr_of!(ENTRY_PILL) }) else {
        return false;
    };
    mx >= r.x && mx < r.x + r.w && my >= r.y && my < r.y + r.h
}

/// Pointer click: same activation as OK on the card under the cursor — and, behind an open
/// overlay, that overlay's own. See [`pointer_focus`] for why the forward is unconditional.
pub(crate) fn click(mx: f32, my: f32) -> Action {
    if crate::ui::filmography::is_open() {
        return match crate::ui::filmography::click(mx, my) {
            crate::ui::filmography::Action::Open(..) => Action::Card,
            crate::ui::filmography::Action::None => Action::None,
        };
    }
    if crate::ui::person_bio::is_open() {
        return Action::None;
    }
    // A click on the pill both focuses it and OPENS the route — the same thing OK does, which is
    // what `pointer_focus`/`click` mean by "same activation as OK on the thing under the cursor".
    // Routed through `header_ok` rather than calling `filmography::open` here, so the pill has ONE
    // activation path and its guards (has_entry, and the overlay-already-open bail) cannot drift.
    if entry_at(mx, my) {
        let sc = scene();
        sc.on_entry = true;
        sc.on_header = false;
        return if header_ok() { Action::Card } else { Action::None };
    }
    match tile_at(mx, my) {
        Some((kind, i)) => {
            focus_tile(kind, i);
            Action::Card
        }
        None => Action::None,
    }
}

/// Move focus onto one shelf tile — the ONE place hover and click agree on what "focused" means.
fn focus_tile(kind: usize, i: usize) {
    let sc = scene();
    sc.on_header = false;
    sc.on_entry = false;
    sc.focus_kind = kind;
    sc.col[kind] = i as c_int;
}

// ---- draw ------------------------------------------------------------------------------------

pub(crate) fn draw() {
    // The clear IS the app surface (see `profiles::draw` for why no SURFACE_APP rect follows it).
    crate::gfx::frame_clear(theme::CLEAR_RGB.0, theme::CLEAR_RGB.1, theme::CLEAR_RGB.2);
    // THE page transition, as ONE cascade-alpha push at the root — the same line `detail::draw` and
    // `home_draw` carry, and for the same reasons (see `ui::nav`). Like the detail page, this screen
    // has no continuous chrome, so the whole tree rides the page alpha; the wash included, since
    // `Painter::ambient` mixes toward `theme::SURFACE_APP` under the cascade and that is the colour
    // the clear above just laid down.
    let p = Painter::root().alpha(crate::ui::nav::page_alpha());
    let sc = scene();
    let env = Env::inert();
    // Forget last frame's focused tile before anything draws, and before the early return below:
    // focus on the header, or a page with no shelves yet, must leave [`FOCUS_TILE`] empty rather
    // than naming a tile this frame does not draw.
    unsafe { *addr_of_mut!(FOCUS_TILE) = None };
    // …and the entry pill's box with it, for the same reason: a frame that does not draw the pill
    // (no credits yet, or a person with none) must leave no rect for the pointer to hit.
    unsafe { *addr_of_mut!(ENTRY_PILL) = None };
    if crate::person::current().is_none() {
        return;
    }
    // the ambient wash under everything — opaque, so it stands in for the clear
    sc.amb.draw(p, Rect::FULL);
    let col = sc.column;
    col.draw(sc, &env, p);
    draw_shelf_state(p, &env, sc);
    // ---- the bio panel, over everything the page just drew ------------------------------------
    // The SCRIM is the page's and is drawn HERE, before the panel — `Popover::scrim`'s rule, and on
    // this page it is also what protects the panel's fine print: the person's own `size::DISPLAY`
    // name sits directly behind the sheet's top-left corner — and squarely on it, since the band
    // top-aligns on that name's cap top — and a headline read through a 72% frost lifts the ground
    // under a `size::CAPTION` line past its graded contrast. Dimming the page before the frost
    // samples it is the fix; see `person_bio`'s module doc, which also records that the measurement
    // behind the scrim's value was taken while this name was `size::HERO`.
    crate::ui::person_bio::scrim();
    crate::ui::person_bio::draw();
    // ---- and the Filmography route, over the lot ---------------------------------------------
    // Last, and with no scrim of its own: it REPLACES the page rather than standing on it, so its
    // ground is opaque and there is nothing behind it to dim. See `ui::filmography`.
    crate::ui::filmography::draw();
}

/// The header band: the circular portrait and, top-aligned on its cap line, the text column (name,
/// the roles kicker, Born/Died, the 3-line bio). Drawn from the child's local origin (the
/// `ScrollColumn` has already translated to the block top) and from the SAME [`header_flow`] that
/// measured it, so a line can never draw where the flow reserved no room.
///
/// **Nothing here animates.** The band used to condense — two portrait diameters, a HERO↔TITLE name
/// crossfade, every other line fading out — and the module doc records why that is gone: a header
/// that shrinks while the page scrolls means the first keypress moves every element on screen at
/// once. What is left is a fixed block, which is also why the alpha argument that threaded through
/// every line below it is gone.
fn draw_header(p: Painter, person: &Person, sc: &Scene) {
    let flow = sc.header;
    let d = flow.exp_d;
    // `Art::Person`, not `Thumb` — the SAME art case the credits shelf this page was opened from
    // draws, so a person with no headshot wears the person glyph here exactly as their circle did
    // there; and it draws that glyph only for an EMPTY key, never for a texture still resolving.
    // NEVER pixel-snapped: this is scaled art, and snapping it would fight the transcoder's
    // resampling exactly the way snapping a poster does.
    // TOP-ALIGNED on the name's cap top for a full header, centred for a bare one — the flow made
    // that decision once, in `header_flow`, and this reads its answer.
    let portrait = Rect::new(MARGIN_X, flow.portrait_y, d, d);
    crate::ui::widgets::card(
        p,
        portrait,
        Art::Person {
            sid: person.sid,
            key: &person.thumb,
            res: PORTRAIT_RES,
        },
        d * 0.5,
        false,
        1.0,
        0.0,
    );

    let col_x = col_x(d);
    // **The selectable block's mark goes down FIRST — before any of the band's text.**
    //
    // It is a GROUND, so it belongs under the prose; what puts it all the way at the top of the
    // draw rather than just under the bio is the gap above it. `BIO_GAP` measures to the PROSE (the
    // owner's ladder, see that constant) while the plate hangs its own `HL_PAD_Y` UP into that gap,
    // so how close its top edge comes to the Born/Died line's descenders is a function of two
    // constants that are tuned independently. At today's `LG` it clears them; at the `MD` this
    // ladder briefly used it did not, and the plate painted over them. Ordering the ground first
    // makes that a non-question for any future rung — every run in the band lands on top of the
    // plate and stays whole. It costs nothing: the same rect, from the same flow.
    let mark = bio_mark_rect(person, sc, flow, col_x);
    if let Some(r) = mark {
        crate::ui::widgets::text_block_highlight(p, r);
    }
    // The NAME, at ONE rung — the band no longer condenses, so there is no second run and no
    // crossfade. `DISPLAY` rather than `HERO`: at 72 the name was the largest type on the screen by
    // a long way and the column under it read as its caption, where at 48 the band reads as one
    // block with a heading.
    Label::new(
        sc.name_c.as_ptr(),
        theme::size::DISPLAY,
        theme::TEXT_PRIMARY,
    )
    .bold()
    .v(VAlign::CapTop)
    .draw(p, Rect::new(col_x, flow.name_y, 0.0, 0.0));
    // **The identity pair — roles over dates — is ONE fact in two lines, so they share one ink.**
    // The dates used to step back to TERTIARY, a second de-emphasis under a line that is already
    // secondary, and what that produced was a band reading as four separate strays rather than as a
    // name with a group under it. They differ by SIZE (the ladder's own rung) and by nothing else.
    // Both are pre-built + pre-elided runs — see `refresh_runs`.
    // While the profile has not answered, these two lines and the bio below draw the shared
    // pending SWEEP instead of their real runs — see [`header_flow`] for why the space is already
    // reserved at the size the real content will take.
    let pending = crate::person::current().is_some_and(crate::person::facts_pending);
    let phase = crate::ui::widgets::skeleton_phase(sc.spin_ms as u32);
    for (y, run, sz, w) in [
        (flow.meta_y, &sc.roles_c, theme::size::LABEL, 0.42),
        (flow.life_y, &sc.life_c, theme::size::CAPTION, 0.68),
    ] {
        let Some(y) = y else { continue };
        if pending {
            let h = crate::text::cap_h(sz, 0);
            crate::ui::widgets::skeleton_bar(p, Rect::new(col_x, y, BIO_W * w, h), phase);
        } else {
            Label::new(run.as_ptr(), sz, theme::TEXT_SECONDARY)
                .v(VAlign::CapTop)
                .draw(p, Rect::new(col_x, y, 0.0, 0.0));
        }
    }
    if pending {
        if let Some(by) = flow.bio_y {
            let h = crate::text::cap_h(theme::size::BODY, 0);
            // three lines, the widest first — a placeholder that TAPERS reads as text rather than
            // as three identical bars, which is what a body of real prose actually looks like
            for (i, w) in [1.0, 1.0, 0.58].into_iter().enumerate() {
                let ly = by + i as f32 * BIO_LEAD;
                crate::ui::widgets::skeleton_bar(p, Rect::new(col_x, ly, BIO_W * w, h), phase);
            }
        }
    } else if let Some(by) = flow.bio_y {
        // `BIO_W` flat, NOT a width derived from the live column x: the wrap must be the one
        // `header_flow` measured (same cache entry). It is the READING measure rather than the
        // column's, which is what makes it narrower than the runs above it — see `BIO_W`.
        let bio = bio_view(&person.bio, 1.0);
        let bh = bio.measure_h(BIO_W);
        let truncated = bio.truncates(BIO_W);
        bio.draw(p, Rect::new(col_x, by, BIO_W, 0.0));
        // MORE — quiet grey (the About card's ink; this is a mark, not a control, and the loudest
        // thing in the band must stay the name), pinned to the bio COLUMN's right edge and sitting
        // on the last line's own cap band, which is where the line just dissolved to meet it. Both
        // halves of that placement are ASKED OF THE VIEW THAT DREW THE TEXT (`last_line_cap_y`, and
        // `Label`'s own cap-band conversion) rather than restated here — the leading and the cap
        // band are `bio`'s, not this block's.
        if truncated {
            // …and it steps UP a rung while the block is lifted: a mark that stays tertiary under
            // an active highlight reads as disabled, which is the one thing it is not.
            Label::new(
                MORE.as_ptr(),
                theme::size::BODY,
                match mark.is_some() {
                    true => theme::TEXT_SECONDARY,
                    false => theme::TEXT_TERTIARY,
                },
            )
            .bold()
            .h(HAlign::Right)
            .v(VAlign::CapTop)
            .draw(p, Rect::new(col_x, bio.last_line_cap_y(by, bh), BIO_W, 0.0));
        }
    }
    // …and the route entry, last in the identity column.
    if let Some(ey) = flow.entry_y {
        draw_entry(p, sc, col_x, ey);
    }
}

/// One `Movies` / `Shows` shelf: the heading with its item count, then the shared strip of poster
/// cards. Identical composition to detail's Related row — same `RowStyle::HOME` geometry, same
/// springs, same progress language on the tiles — plus the focused tile's SECOND line: the
/// character this person played in it ([`Person::role`]), the caption `card_row::strip` now
/// threads through.
fn draw_shelf(p: Painter, person: &Person, kind: usize, sc: &Scene) {
    let items = person.shelf(kind);
    let row = &sc.shelves[kind];
    // `-1` while the HEADER holds focus, not just while another shelf does: a shelf whose kind
    // happens to match still owns no focus then, and `strip` captions whatever column it is given.
    // Without the `on_header` term the page mounted with tile 0 already wearing its title and
    // character line — a card that reads as selected while the portrait is what the user is on.
    // (`update` already freezes these rows' springs in that state, so the pop was correctly absent,
    // which is exactly what made the stray labels look deliberate.)
    let focus_col = if !on_header(person, sc) && !sc.on_entry && sc.focus_kind == kind {
        sc.col[kind]
    } else {
        -1
    };
    // Where this shelf's tile row lands on the PANEL — the `ScrollColumn` has already translated `p`
    // to the block top, so the number is not otherwise recoverable from inside the strip. Only
    // needed for the focused shelf, and only to record the focused tile's frame (see [`FOCUS_TILE`]).
    let screen_top = (focus_col >= 0)
        .then(|| {
            focus_pos(person, sc).map(|pos| sc.column.child_top(sc, pos + 1) - sc.column.scroll.pos)
        })
        .flatten();
    let hy = -row.lift();
    #[allow(unused_variables)] // `tw` is used only when the count run exists
    let tw = Label::new(
        SHELF_TITLE[kind].as_ptr(),
        theme::size::HEADLINE,
        theme::TEXT_HEADING,
    )
    .bold()
    .v(VAlign::CapTop)
    .draw(p, Rect::new(MARGIN_X, hy, SCR_W, 0.0));
    if !sc.shelf_count_c[kind].as_bytes().is_empty() {
        // the count sits ON THE HEADING'S BASELINE — two sizes cap-top-aligned would leave the
        // smaller run floating above the line the eye reads
        let tw = crate::text::text_width(SHELF_TITLE[kind].as_ptr(), theme::size::HEADLINE, 1);
        Label::new(
            sc.shelf_count_c[kind].as_ptr(),
            theme::size::CAPTION,
            theme::TEXT_TERTIARY,
        )
        .v(VAlign::Baseline)
        .draw(
            p,
            Rect::new(
                MARGIN_X + tw + SHELF_COUNT_GAP,
                hy,
                0.0,
                crate::text::cap_h(theme::size::HEADLINE, 1),
            ),
        );
    }
    card_row::strip(
        p,
        row,
        items.len(),
        focus_col,
        SHELF_LABEL_H,
        (CARD_W, CARD_H),
        CARD_W + GAP,
        &SHELF_STYLE,
        SCR_W,
        |i| Art::Poster(items.get(i)),
        |i| items.get(i).and_then(|m| m.resume_frac()),
        // the focused tile's two lines: the single-line title (marqueeing if it overflows) over
        // the character this person played in it
        |i| match items.get(i) {
            Some(m) => card_row::TileLabel::titled(&m.title, person.role(kind, i)),
            None => card_row::TileLabel::default(),
        },
        // The focused tile's frame, in screen space and UNSCALED — what the press-and-hold context
        // menu is anchored beside and re-drawn into over its scrim. Recorded rather than derived,
        // for `search::HIT_R`'s reason: the horizontal offset is the scroll spring inside this
        // shelf's own `CardRow`, which the caller cannot see.
        |_, i, x, focused| {
            if let (true, Some(top)) = (focused, screen_top) {
                let r = Rect::new(x - row.scroll_x(), top + SHELF_LABEL_H, CARD_W, CARD_H);
                unsafe { *addr_of_mut!(FOCUS_TILE) = Some((kind, i, r)) };
            }
        },
    );
}

/// **The Filmography entry row** — the end of the page, and the one control on it that replaces the
/// page rather than changing something on it.
///
/// It is built like the rows INSIDE the route it opens (`ui::filmography`'s credit rows): the same
/// pill inset, the same radius, the same accent fill, and a trailing chevron which is the mirror of
/// the crumb that route lands on. That is the whole rule the mark states — a control that only
/// changes something on this screen has none.
///
/// **At rest it keeps a sheen edge; focused, it additionally casts** — see [`crate::ui::widgets::
/// draw_control_face`], which fixed a defect this doc used to state as the rule ("there is no cast
/// and no POP"): a control face that never lifts off the page reads as a sticker rather than a
/// button. There is still no POP proper (`CtlPop`) — `--focus-scale-control` is a CAPSULE's motion
/// about its own centre, and a row growing 7% about its centre would step off the identity column's
/// left guide; [`draw_entry`] applies the same scale BY HAND instead, from the left edge, for
/// exactly that reason. There is no press dip either, because `app.rs` arms no tvOS press here:
/// `focus_is_card` is false on this row (it holds no card), so the OK goes down the immediate path
/// through [`header_ok`], exactly as the header's does.
/// **The route-entry pill's width, sized to its own runs** — the ONE expression the draw, the focus
/// scale and the pointer hit test all read, so a click can never land beside the capsule it saw.
fn entry_w(sc: &Scene) -> f32 {
    let label = crate::text::text_width(c"Filmography".as_ptr(), theme::size::LABEL, 1);
    let count = if sc.entry_count_c.as_bytes().is_empty() {
        0.0
    } else {
        ENTRY_RUN_GAP
            + crate::text::text_width(c"·".as_ptr(), theme::size::LABEL, 0)
            + ENTRY_RUN_GAP
            + crate::text::text_width(sc.entry_count_c.as_ptr(), theme::size::LABEL, 0)
    };
    // …and the trailing half measured to the chevron's INK, not to its box: one
    // `ENTRY_CHEVRON_GAP` of visible air, the mark's own drawn width, then one `ENTRY_OUTER_PAD` of
    // visible air. The two bearings come off because the box carries them and the eye does not.
    2.0 * ENTRY_OUTER_PAD + label + count + ENTRY_CHEVRON_GAP + ENTRY_MARK
        - ENTRY_MARK_BEARING_L
        - ENTRY_MARK_BEARING_R
}

/// **Where the run starts inside the DRAWN pill, measured from its left edge.**
///
/// The group is CENTRED in the box the focus pop actually draws, not pinned to
/// [`ENTRY_OUTER_PAD`]. At rest the two are the same number by construction — [`entry_w`] is
/// `2 * ENTRY_OUTER_PAD` plus this group — so this changes nothing about a resting pill; under the
/// pop it is the only thing that keeps the two ends equal.
///
/// **Pinning to the pad put the pop's whole 7% into the trailing margin alone**, because the box
/// grows by `entry_w * (e - 1)` while a left-pinned run does not move at all. Measured on the
/// television at this pill's real width: **25px of air before the F against 45px after the
/// chevron** — the owner reported the same imbalance twice, and both times the arithmetic that
/// looked right was the RESTING one.
///
/// Pure, and split out for that reason: [`draw_entry`] measures text, which is the SDL2_ttf
/// host-link trap `ui/CLAUDE.md` records, and this half is the half a test can hold.
fn entry_run_x(w: f32, e: f32) -> f32 {
    (w * e - (w - 2.0 * ENTRY_OUTER_PAD)) * 0.5
}

/// The route entry — a COMPACT capsule at the end of the identity column. See [`ENTRY_H`] for why
/// it is here rather than after the shelves, and why being compact is what earns it a control's
/// focus motion.
///
/// **It grows from the LEFT** (`transform-origin: left center`), which is the whole reason the
/// scale is applied by hand here rather than through a `CtlPop`: the identity column's left guide
/// carries the name, both meta lines and the bio, and a capsule that grew about its centre would
/// step 3px off that guide every time focus arrived. There is no press dip, because `app.rs` arms
/// no tvOS press on this row — `focus_is_card` is false here (it holds no card), so OK goes down
/// the immediate path through [`header_ok`], exactly as the bio's does.
fn draw_entry(p: Painter, sc: &Scene, x: f32, y: f32) {
    let focused = sc.on_entry;
    let w = entry_w(sc);
    // **Recorded for the pointer, at the RESTING scale and in SCREEN space** — see [`ENTRY_PILL`].
    // The resting box is the right one because the focus pop strictly contains it, so every pixel
    // that was clickable before focus arrived still is and a hover cannot be lost by the control
    // growing under the cursor (the trade `home::focused_card_rect` documents for a card).
    //
    // **The cascade offset is not optional.** This runs inside a `ScrollColumn` child, so `p` has
    // already been translated to the block top minus the scroll and `x`/`y` are the block's LOCAL
    // coordinates — which is what `draw_*` is supposed to work in. A rect recorded without adding
    // the translate back is a hit test in a different coordinate system from the pointer, and it
    // fails silently: the first device pass after this was written clicked the drawn pill and
    // nothing happened at all, exactly as before the fix. `shelf_row_y` and `FOCUS_TILE` solve the
    // same problem the long way, through `column.child_top(..) - scroll.pos`; `p.dy()` IS that
    // number, and taking it from the painter that just drew the thing is the version that cannot
    // drift from what was drawn.
    unsafe {
        *addr_of_mut!(ENTRY_PILL) = Some(Rect::new(x + p.dx(), y + p.dy(), w, ENTRY_H));
    }
    // the control family's focus pop, from the left edge: the box grows, the guide does not move
    let e = if focused { crate::ui::widgets::CTRL_FOCUS_SCALE } else { 1.0 };
    let (pw, ph) = (w * e, ENTRY_H * e);
    let pill = Rect::new(x, y - (ph - ENTRY_H) * 0.5, pw, ph);
    // The shared control-face construction every other pill-shaped control in the app wears — the
    // blended capsule outline, the 1px card-sheen edge and (focused) the soft cast — rather than a
    // bare flat-fill `rrect`. See `widgets::draw_control_face`'s own doc for what that was missing.
    crate::ui::widgets::draw_control_face(
        p,
        pill,
        if focused {
            crate::ui::ACCENT
        } else {
            theme::CONTROL_IDLE_FILL
        },
        focused,
        crate::ui::widgets::ControlGround::Keyed,
    );
    let (ink, count_ink) = if focused {
        (crate::ui::ACCENT_INK, theme::ROW_VALUE_INK_ON)
    } else {
        (theme::TEXT_PRIMARY, theme::TEXT_TERTIARY)
    };
    let cy = pill.y + pill.h * 0.5;
    // **Every gap below is its NOMINAL (e=1) size — none of these adds is multiplied by `e`, and
    // the type does not scale either** (`CtlPop`'s rule: rule 2 has no rung between 26 and 28). The
    // pop grows the BOX, and the only thing that answers it is where the whole group STARTS, which
    // is [`entry_run_x`]. Two bugs are closed by that split and they are opposite. Walking the
    // accumulation by `e` dumped `entry_w * (e-1)` minus one scaled pad into the single gap before
    // the chevron; pinning it to `ENTRY_OUTER_PAD` instead dumped all of `entry_w * (e-1)` into the
    // trailing MARGIN — 25px of air before the F against 45px after the chevron, on the panel.
    // Centring the group in the drawn box is what makes the pop widen both ends equally and no gap
    // between two runs at all.
    let mut rx = x + entry_run_x(w, e);
    rx += Label::new(c"Filmography".as_ptr(), theme::size::LABEL, ink)
        .bold()
        .v(VAlign::Middle)
        .draw(p, Rect::new(rx, cy, 0.0, 0.0));
    if !sc.entry_count_c.as_bytes().is_empty() {
        rx += ENTRY_RUN_GAP;
        rx += Label::new(c"·".as_ptr(), theme::size::LABEL, theme::TEXT_SEPARATOR)
            .v(VAlign::Middle)
            .draw(p, Rect::new(rx, cy, 0.0, 0.0));
        rx += ENTRY_RUN_GAP;
        rx += Label::new(sc.entry_count_c.as_ptr(), theme::size::LABEL, count_ink)
            .v(VAlign::Middle)
            .draw(p, Rect::new(rx, cy, 0.0, 0.0));
    }
    // The chevron is a disclosure mark trailing the fact, not a fourth run in the same list — its
    // own, wider [`ENTRY_CHEVRON_GAP`] rather than [`ENTRY_RUN_GAP`] is what says so (owner
    // correction, 2026-09-06). Measured to the mark's INK: the box's own leading bearing comes off
    // the gap, so the air the eye sees here is the token, not the token plus 7.5px of empty svg.
    rx += ENTRY_CHEVRON_GAP - ENTRY_MARK_BEARING_L;
    crate::ui::icons::draw(
        p,
        crate::ui::icons::Icon::Chevron,
        Rect::new(rx, cy - ENTRY_MARK * 0.5, ENTRY_MARK, ENTRY_MARK),
        ink,
    );
}

/// **The Filmography entry pill's screen box, recorded by [`draw_entry`] — the pointer's half of
/// that control.**
///
/// It exists because the pill shipped DRAWN but not clickable, and the two halves of that are worth
/// keeping apart. The draw was right: the pill mounts focused (`Scene::new`'s `on_entry: true`), so
/// it renders ACCENT-filled and popped, visibly the focused button of the page. The pointer paths
/// consulted only [`tile_at`], which walks the shelf rows and never the identity band — so hovering
/// it lit nothing and clicking it did nothing, on a page where the posters below it did both. A
/// control whose draw and whose hit test are not one predicate is the failure `ui/CLAUDE.md` names
/// over and over; recording the rect AT DRAW is how every other control here answers it, and it
/// gets absence, the scroll offset and the focus pop right for free rather than by re-deriving
/// geometry on the pointer path — which would also drag `entry_w`'s `text_width` onto a path a host
/// test can link, the SDL2_ttf trap this module records above [`overlay_owns_the_press`].
static mut ENTRY_PILL: Option<Rect> = None;

/// The focused shelf tile's kind, column and UNSCALED screen frame, recorded by [`draw_shelf`].
///
/// Cleared at the top of every [`draw`], so a frame drawn with focus on the HEADER — or with no
/// shelves at all — leaves `None` rather than the last tile that held it.
static mut FOCUS_TILE: Option<(usize, usize, Rect)> = None;

/// The focused tile's rect at its focus magnification — what the item context menu anchors beside.
/// `press::scale()` is left out for `home::focused_card_rect`'s reason.
pub(crate) fn focused_tile_rect() -> Option<Rect> {
    let (kind, i, base) = unsafe { *addr_of!(FOCUS_TILE) }?;
    Some(base.scaled(scene().shelves[kind].scale(i)))
}

/// Re-draw the focused tile ON TOP of a modal scrim — this screen's half of
/// [`crate::ui::popover::Opener`].
///
/// The page has no clip of its own (the shelves are `on_axis`-culled, not scissored) and no chrome
/// over them, so unlike Search this needs no bound: whatever the strip drew, this draws again in
/// the same place.
pub(crate) fn redraw_focused_tile() {
    crate::ui::guard(|| {
        let Some((kind, i, base)) = (unsafe { *addr_of!(FOCUS_TILE) }) else {
            return;
        };
        let Some(person) = crate::person::current() else {
            return;
        };
        let sc = scene();
        let Some(m) = person.shelf(kind).get(i) else {
            return;
        };
        let s = sc.shelves[kind].scale(i) * crate::ui::press::scale();
        let label = card_row::TileLabel::titled(&m.title, person.role(kind, i));
        let p = Painter::root().alpha(crate::ui::nav::page_alpha());
        card_row::draw_focused(
            p,
            Art::Poster(Some(m)),
            base.scaled(s),
            s,
            &SHELF_STYLE,
            m.resume_frac(),
            &label,
        );
    });
}

/// What sits where the shelves go while there are none: a spinner until the one `/media` request
/// lands, then — only if the person really has nothing in this library — a plain line saying so.
/// Anchored on the SAME flow the shelves would occupy, so nothing jumps when they arrive.
fn draw_shelf_state(p: Painter, env: &Env, sc: &Scene) {
    if crate::person::current()
        .map(|pp| present(pp).1 > 0)
        .unwrap_or(true)
    {
        return;
    }
    // the SAME flow the first shelf would occupy — child_top(1), not a restated sum, so nothing
    // jumps when the shelves arrive and a flow change cannot leave this anchor behind. With no
    // shelves that child is the Filmography entry row when there is one, which is the right anchor
    // either way: this read-out and that row occupy the same place in the flow.
    let y = sc.column.child_top(sc, 1) - sc.column.scroll.pos;
    let band = Rect::new(0.0, y, SCR_W, CARD_H);
    if crate::person::loading() {
        // **One PENDING SHELF, in the exact geometry a real one draws** — a heading placeholder at
        // `TITLE_DY`, then a row of poster-shaped [`skeleton_sheet`]s at `CARD_DY` (`Person
        // Screen.dc.html`: "the same poster geometry, the same resting card shadow"). It replaced a
        // bare `Spinner` here: the spinner was this screen's own invention, predating the design's
        // pending-shelf treatment, and a row of grey cards where the posters will land is a more
        // specific promise than a dot chasing itself in the middle of empty space.
        let phase = crate::ui::widgets::skeleton_phase(sc.spin_ms as u32);
        crate::ui::widgets::skeleton_bar(
            p,
            Rect::new(MARGIN_X, y + TITLE_DY - 32.0, 214.0, 32.0),
            phase,
        );
        let cy = y + TITLE_DY + CARD_DY;
        for i in 0..5 {
            let cx = MARGIN_X + i as f32 * (CARD_W + GAP);
            crate::ui::widgets::skeleton_sheet(
                p,
                Rect::new(cx, cy, CARD_W, CARD_H),
                theme::CARD_RING_RAD,
                phase,
            );
        }
        return;
    }
    // ...but the settled answer IS the shared Empty read-out — a library with nothing in it is an
    // answer, not a fault, which is the distinction `StatusKind::Empty` exists to keep.
    StatusOverlay::new(
        band,
        c"Nothing from this person is in your libraries",
        StatusKind::Empty,
    )
    .draw(env, p);
}

// ---------------------------------------------------------------------------------------
#[cfg(test)]
mod tests {
    use super::*;

    fn item(rk: &str) -> PmsMovie {
        PmsMovie {
            rk: rk.to_string(),
            ..Default::default()
        }
    }

    /// Seed the store the way a landing does, without a server. **No opening DOWN any more**: the
    /// page mounts on the first shelf now (module doc), so a seeded page with shelves is already
    /// standing on one.
    ///
    /// Mounts through [`super::reopen`], not `crate::person::open` directly, because `reopen` is
    /// where the SCENE singleton is reset. The data-layer call alone left the previous test's
    /// `col[]`/focus in place — `clamp_focus` clamps but never zeroes — so which column a fresh
    /// page "opened" on depended on which test the serial lock ran before this one.
    fn seed(movies: usize, shows: usize) {
        super::reopen(
            crate::plex::ServerId::UNSET,
            "161",
            "5d77682aeb5d26001f1de4b0",
            "Idina Menzel",
            "",
        );
        crate::person::install_for_test(
            (0..movies).map(|i| item(&format!("m{i}"))).collect(),
            (0..shows).map(|i| item(&format!("s{i}"))).collect(),
        );
        clamp_focus();
    }

    /// A person with only movies has ONE shelf, and DOWN must not walk focus off the end of it —
    /// the focus model indexes the PRESENT shelves, not the two possible ones.
    #[test]
    fn focus_walks_only_the_shelves_that_exist() {
        let _serial = crate::testlock::serial();
        seed(3, 0);
        assert_eq!(scene().focus_kind, 0);
        move_focus(SDLK_DOWN);
        assert_eq!(scene().focus_kind, 0, "there is no Shows shelf to move to");
        move_focus(SDLK_RIGHT);
        move_focus(SDLK_RIGHT);
        move_focus(SDLK_RIGHT); // past the end
        assert_eq!(scene().col[0], 2, "column focus clamps to the last card");
        assert_eq!(focused_item().map(|m| m.rk.clone()), Some("m2".to_string()));

        seed(2, 2);
        assert!(
            !scene().on_header,
            "the page MOUNTS on the first shelf, not on the header"
        );
        assert_eq!(focused_item().map(|m| m.rk.clone()), Some("m0".to_string()));
        move_focus(SDLK_DOWN);
        assert_eq!(
            scene().focus_kind,
            1,
            "with both shelves present DOWN reaches Shows"
        );
        assert_eq!(focused_item().map(|m| m.rk.clone()), Some("s0".to_string()));
        crate::person::close();
    }

    /// UP from the FIRST shelf must reach the header — the page's top row — while UP from a LOWER
    /// shelf still walks one shelf at a time and never teleports past them.
    #[test]
    fn up_from_the_first_shelf_returns_to_the_header_and_scrolls_the_portrait_back() {
        let _serial = crate::testlock::serial();
        seed(2, 2);
        move_focus(SDLK_DOWN); // → Shows, the second shelf
        assert_eq!(scene().focus_kind, 1);

        move_focus(SDLK_UP);
        assert!(
            !scene().on_header,
            "UP from the SECOND shelf must land on the first, not the header"
        );
        assert_eq!(scene().focus_kind, 0);

        move_focus(SDLK_UP);
        assert!(
            scene().on_header,
            "UP from the first shelf must reach the header"
        );
        assert!(
            focused_item().is_none(),
            "the header holds no card — OK there must do nothing"
        );
        assert_eq!(
            Column::focus_child(scene()),
            Some(0),
            "the header is flow child 0"
        );

        move_focus(SDLK_DOWN);
        assert!(!scene().on_header, "DOWN must go back into the shelves");
        assert_eq!(scene().focus_kind, 0);
        crate::person::close();
    }

    /// **The bio block's focus mark must not show without an explicit arrival at the header.** The
    /// mark says which block a press will land on, so a page nobody has navigated to the top of
    /// must not draw one — and a landing that happens to leave the header as the only row is not an
    /// arrival either.
    #[test]
    fn the_bio_mark_only_shows_after_an_explicit_arrival_at_the_header() {
        let _serial = crate::testlock::serial();
        seed(2, 2);
        let p = crate::person::current().unwrap();
        assert!(
            !bio_mark_visible(p, scene(), true),
            "a fresh mount is on the first shelf — there is no mark to draw"
        );

        move_focus(SDLK_UP); // → the header, explicitly
        let p = crate::person::current().unwrap();
        assert!(scene().on_header);
        assert!(
            bio_mark_visible(p, scene(), true),
            "UP back onto the header is an explicit arrival — the mark must show"
        );
        assert!(
            !bio_mark_visible(p, scene(), false),
            "the mark still needs a truncated bio; landing alone is not enough"
        );
        crate::person::close();
    }

    /// **A page with no shelves holds focus on the header WITHOUT writing that into the flag** —
    /// the whole of [`on_header`]'s reason to exist. UP/DOWN there must not focus a shelf that does
    /// not exist, the page must not scroll away from a header that is all there is, and — the half
    /// a fresh mount depends on — the flag must stay false so the frame a shelf lands, focus is
    /// already on its first card.
    #[test]
    fn a_person_with_no_shelves_holds_the_header_without_latching_it() {
        let _serial = crate::testlock::serial();
        seed(0, 0);
        let p = crate::person::current().unwrap();
        assert!(on_header(p, scene()), "there is nowhere else for focus to be");
        assert!(
            !scene().on_header,
            "…but nothing latched it: a landing must not have to move focus back off"
        );
        move_focus(SDLK_DOWN);
        let p = crate::person::current().unwrap();
        assert!(on_header(p, scene()), "DOWN found a shelf that is not there");
        move_focus(SDLK_UP);
        assert_eq!(Column::focus_child(scene()), Some(0));

        // …and now the shelves land. Nothing moves focus, and it is on the first card.
        crate::person::install_for_test(vec![item("m0")], Vec::new());
        clamp_focus();
        let p = crate::person::current().unwrap();
        assert!(!on_header(p, scene()), "a landing must hand focus to the shelf");
        assert_eq!(focused_item().map(|m| m.rk.clone()), Some("m0".to_string()));
        crate::person::close();
    }

    /// A shelf that disappears under the focus (a re-open landing with a different split) must
    /// re-seat it, not leave `focused_item` reading an empty list — and the per-shelf column
    /// memory must clamp with it.
    #[test]
    fn a_landing_that_drops_a_shelf_re_seats_the_focus() {
        let _serial = crate::testlock::serial();
        seed(2, 3);
        move_focus(SDLK_DOWN);
        move_focus(SDLK_RIGHT);
        move_focus(SDLK_RIGHT);
        assert_eq!(scene().focus_kind, 1);
        assert_eq!(scene().col[1], 2);

        crate::person::install_for_test(vec![item("m0")], Vec::new()); // shows vanished
        clamp_focus();
        assert_eq!(
            scene().focus_kind,
            0,
            "focus must fall back to the one present shelf"
        );
        assert_eq!(
            scene().col[1],
            0,
            "the vanished shelf's remembered column clamps too"
        );
        assert_eq!(focused_item().map(|m| m.rk.clone()), Some("m0".to_string()));
        crate::person::close();
    }

    /// **The card under focus must stay the same CARD when a source lands**, not the same index.
    /// The shelves are a merge, and `person::merge_shelves` re-divides the row's budget every time a
    /// server answers — so the film at column 3 a moment ago can be a different film now. Before
    /// [`reseat`], a share landing two seconds into the page slid the selection out from under the
    /// user and the next OK opened something they were not looking at.
    #[test]
    fn a_landing_that_re_divides_the_shelf_keeps_focus_on_the_same_card() {
        let _serial = crate::testlock::serial();
        seed(4, 0);
        move_focus(SDLK_RIGHT);
        move_focus(SDLK_RIGHT); // on "m2"
        let was = focused_id();
        assert_eq!(was.as_ref().map(|(_, rk)| rk.as_str()), Some("m2"));

        // the row is rebuilt with two rows inserted ahead of it — what a second source landing does
        let rebuilt: Vec<PmsMovie> = ["x0", "x1", "m0", "m1", "m2", "m3"]
            .iter()
            .map(|rk| item(rk))
            .collect();
        crate::person::install_for_test(rebuilt, Vec::new());
        reseat(was);
        clamp_focus();
        assert_eq!(
            scene().col[0],
            4,
            "the index followed the card, instead of the card following the index"
        );
        assert_eq!(focused_item().map(|m| m.rk.clone()), Some("m2".to_string()));

        // a card that is gone entirely leaves the clamp to do what it always did — never a panic
        let was = focused_id();
        crate::person::install_for_test(vec![item("z0")], Vec::new());
        reseat(was);
        clamp_focus();
        assert_eq!(focused_item().map(|m| m.rk.clone()), Some("z0".to_string()));
        crate::person::close();
    }

    /// `leave()` must drop the un-consumed request. `requested` is a LATCH — `detail::on_ok`'s cast
    /// arm raises it and only app.rs's per-frame drain lowers it — so one left set here fires on an
    /// unrelated OK several screens later and teleports the user onto an actor page they never
    /// asked for.
    #[test]
    fn leaving_drops_the_un_consumed_request() {
        let _serial = crate::testlock::serial();
        seed(2, 0);
        scene().requested = true;

        leave();

        assert!(
            !take_request(),
            "the request latched past the page it belonged to"
        );
        assert!(crate::person::current().is_none());
    }

    /// The invariant `app.rs`'s `enter_node` depends on: putting a person page back through a BACK
    /// pop must NOT raise the routing latch. `open` raises it (it is the interactive entry, and
    /// app.rs's drain is what routes); `reopen` must not, because the drain would then push the very
    /// node the pop just took off — and the next BACK would appear to do nothing.
    #[test]
    fn reopen_mounts_the_page_without_asking_to_be_routed_to() {
        let _serial = crate::testlock::serial();
        reopen(
            crate::plex::ServerId::UNSET,
            "77",
            "plex://person/77",
            "Peter Sallis",
            "/t.jpg",
        );
        assert!(
            !take_request(),
            "a trail re-entry must not ask app.rs to route again"
        );
        assert_eq!(
            crate::person::current().map(|p| p.key.clone()),
            Some("77".to_string())
        );
        // …and it is still a full mount. The page opens on neither its header nor a shelf, so what
        // a mount has to leave behind is the state a fresh page starts from: focus parked on the
        // entry row (`Scene::on_entry`'s own doc says why — it is the one landing spot data arriving
        // underneath cannot move), not latched to the header, no explicit arrival recorded against
        // it, and the first shelf's column at 0. A trail re-entry that inherited the previous page's
        // `header_marked` would draw the bio's focus mark on a page nobody has navigated to the top
        // of, and one that inherited its `on_entry` from a DIFFERENT person's settled answer would
        // start this fresh mount already resolved off the entry row before its own credits had even
        // been asked for.
        assert!(!scene().on_header);
        assert!(scene().on_entry);
        assert!(!scene().header_marked);
        assert_eq!(scene().col, [0; NSHELF]);

        open(
            crate::plex::ServerId::UNSET,
            "78",
            "plex://person/78",
            "Nick Park",
            "/n.jpg",
        );
        assert!(
            take_request(),
            "the interactive entry is the one that raises the latch"
        );
        crate::person::close();
    }

    // The band's real heights need the text stack (the name's cap band, the bio's pixel wrap),
    // which the host suite cannot link — so the geometry tests below feed `reveal_block`, which is
    // pure, with BOUNDS on those heights instead of measuring them.
    //
    /// The FULL text stack, bounded above by giving every line its whole point size (the real flow
    /// measures CAP BANDS, which are ~28% shorter): 48 + 16 + 26 + 16 + 24 + 24 + 120 = 274 against
    /// a measured stack nearer 247.
    ///
    /// **The band is the TEXT half's height now, not the portrait's**, and both halves of that
    /// moved: the stack shrank when the band stopped condensing (the name dropped HERO→DISPLAY and
    /// the gap ladder came down a rung) and the portrait came down 320 → [`CARD_W`]. The two are
    /// within a few pixels of each other, which is what makes the top alignment read as a decision
    /// — see [`header_flow`]. Do not re-assert which one wins: it is a near-tie by design, and a
    /// test that pinned the winner would fail on the next honest retune of either.
    const STACK_BOUND: f32 = theme::size::DISPLAY as f32
        + META_GAP
        + theme::size::LABEL as f32
        + LIFE_GAP
        + theme::size::CAPTION as f32
        + BIO_GAP
        + BIO_LINES as f32 * BIO_LEAD;
    /// The band, bounded above: whichever of the portrait and that stack is taller. `f32::max` is
    /// not const, hence the comparison.
    const HEADER_H: f32 = if STACK_BOUND > PORTRAIT_EXP {
        STACK_BOUND
    } else {
        PORTRAIT_EXP
    };

    /// **The packing dividend, as geometry.** The first shelf fits on screen with NO scroll at
    /// rest, even against the pessimistic point-size bound above. (Under the first version's
    /// stacked header a full biography pushed the first shelf below the fold, which is what made
    /// the header a focus row at all; the row stays, for navigation and for the biography, not for
    /// reachability.) So this is the test that fails if a future band, gap or under-title band
    /// grows past the room the page actually has — and since the page now MOUNTS on that shelf, it
    /// is also what guarantees a mount does not open part-scrolled.
    #[test]
    fn the_first_shelf_fits_at_rest() {
        let h = shelf_block_h();
        let scroll = |band: f32| {
            let top = HEADER_TOP + band + BAND_GAP_TO_SHELF;
            reveal_block(0.0, top, h, top + h + BOTTOM_PAD)
        };
        // Every band the page can actually BE in, and the pessimistic bound above all of them.
        // [`HEADER_H`] substitutes point sizes for cap heights, so it stands ~27px above anything
        // the real text stack can produce — asserting the packing dividend at THAT height is what
        // makes this test survive an honest retune of the ladder rather than pinning today's
        // numbers.
        for band in [PORTRAIT_BARE, PORTRAIT_EXP, HEADER_H] {
            assert_eq!(
                scroll(band),
                0.0,
                "band {band}: focusing the first shelf scrolled the page (block bottom {}, floor {})",
                HEADER_TOP + band + BAND_GAP_TO_SHELF + h,
                SCR_H - BOTTOM_PAD
            );
        }
    }

    /// ...and a SECOND shelf, which cannot fit alongside the first, does scroll — by exactly
    /// enough to put its own block bottom on screen, never further.
    #[test]
    fn reaching_the_second_shelf_scrolls_it_fully_into_view_and_no_further() {
        let h = shelf_block_h();
        let top = HEADER_TOP + PORTRAIT_EXP + BAND_GAP_TO_SHELF + h + SHELF_GAP;
        let content = top + h + BOTTOM_PAD;
        let want = reveal_block(0.0, top, h, content);
        assert!(want > 0.0, "the second shelf must scroll into view");
        assert!(
            top + h - want <= SCR_H,
            "its block bottom is still off screen"
        );
        assert!(
            top - want >= TOP_MARGIN,
            "it scrolled past the minimum — the shelf overshot upward"
        );
    }

    /// Focusing the header — flow child 0, at [`HEADER_TOP`] — rests the page at 0 for ANY band
    /// height the page can produce, from wherever the scroll happens to be: returning from deep in
    /// the shelves lands at the top, never part-way.
    #[test]
    fn focusing_the_header_rests_the_page_at_the_top_from_any_scroll() {
        for h in [PORTRAIT_BARE, HEADER_H, HEADER_H * 1.5] {
            let content = HEADER_TOP
                + h
                + BAND_GAP_TO_SHELF
                + 2.0 * (shelf_block_h() + SHELF_GAP)
                + BOTTOM_PAD;
            for cur in [0.0, 200.0, content] {
                assert_eq!(
                    reveal_block(cur, HEADER_TOP, h, content),
                    0.0,
                    "band h={h} did not rest the page at the top from scroll {cur}"
                );
            }
        }
    }

    /// **The entry pill answers the pointer over the box it was DRAWN in.**
    ///
    /// It shipped drawn-but-unclickable: the pill mounts focused, so it renders ACCENT-filled and
    /// popped — visibly the focused button of the page — while `pointer_focus`/`click` consulted
    /// only `tile_at`, which walks the shelf rows and never the identity band. Hovering it lit
    /// nothing and clicking it did nothing, on a page whose posters did both.
    ///
    /// Graded through [`entry_at`] against a directly-seeded [`ENTRY_PILL`], deliberately: the
    /// recording happens in `draw_entry`, which measures text, and `click` routes through
    /// `header_ok` → `bio_is_truncated`, which measures text too — the SDL2_ttf trap this module
    /// records, where reaching `TTF_SizeUTF8` from a test stops the whole suite LINKING.
    #[test]
    fn the_entry_pill_answers_the_pointer_over_its_drawn_box() {
        let _serial = crate::testlock::serial();
        let r = Rect::new(480.0, 700.0, 300.0, ENTRY_H);
        unsafe { *addr_of_mut!(ENTRY_PILL) = Some(r) };
        assert!(entry_at(r.x + 1.0, r.y + 1.0), "the pill's own top-left corner");
        assert!(entry_at(r.x + r.w * 0.5, r.y + r.h * 0.5), "its centre");
        assert!(!entry_at(r.x - 1.0, r.y + 5.0), "just left of it");
        assert!(!entry_at(r.x + r.w, r.y + 5.0), "the exclusive right edge");
        assert!(!entry_at(r.x + 5.0, r.y - 1.0), "just above it");
        assert!(!entry_at(r.x + 5.0, r.y + r.h), "the exclusive bottom edge");

        // …and a frame that drew NO pill leaves nothing to hit: `draw` clears the box beside
        // `FOCUS_TILE`, so a person with no credits cannot be clicked into a route that is not on
        // screen. This is the half a recorded-at-draw rect gets right for free and a re-derived
        // rect gets wrong.
        unsafe { *addr_of_mut!(ENTRY_PILL) = None };
        assert!(!entry_at(r.x + 5.0, r.y + 5.0), "no pill drawn, nothing to hit");
    }

    /// **A shelf on this page pitches exactly as one on Home does.** The screen used to set its own
    /// heading band, its own gap and no air under the heading at all, which pitched its shelves at
    /// 597 against everybody else's 549 AND ran a popped card into the words above it. This is that
    /// claim as arithmetic: the fixed part plus the open label band IS `consts::ROW_PITCH`.
    #[test]
    fn a_shelf_here_pitches_like_a_shelf_on_home() {
        assert_eq!(SHELF_GAP + shelf_block_h(), crate::ui::consts::ROW_PITCH);
        assert_eq!(SHELF_LABEL_H, TITLE_DY + CARD_DY);
    }

    /// **Both ends of the entry pill show the SAME air, measured to INK and from the CAPSULE'S OWN
    /// EDGES** — the owner's rule for this control, reported twice, and the second report is the one
    /// this grades. Two separate things conspire and each is invisible to the other:
    ///
    /// * the chevron's box carries 7.5px of empty svg on each side ([`crate::ui::icons::ink_x`]), so
    ///   a pill balanced by its BOXES is visibly 7.5px looser after the mark than before the label;
    /// * and the focus pop grows the drawn box by `entry_w * (e - 1)`, which a run pinned to
    ///   [`ENTRY_OUTER_PAD`] does not answer at all — every pixel of it lands on the trailing side.
    ///
    /// Corrected for the first alone, the pill measured **25px before the F against 45px after the
    /// chevron** on the television, which is what makes the resting arithmetic a trap: at `e = 1`
    /// the pinned walk and the centred one are the SAME number.
    ///
    /// Graded through [`entry_run_x`] rather than [`draw_entry`], deliberately: that function
    /// measures text, and `ui/CLAUDE.md` records what reaching `TTF_SizeUTF8` from anything a host
    /// test calls does to this suite — it does not fail one test, it stops the whole thing LINKING.
    /// The content run's width cancels out of both ends anyway, which is why a stand-in proves the
    /// same statement.
    #[test]
    fn the_entry_pill_shows_the_same_air_before_the_label_and_after_the_chevron() {
        // `w` here is a whole pill; 307 is the one the owner photographed ("Filmography · 17").
        for w in [2.0 * ENTRY_OUTER_PAD + 60.0, 307.0, 420.0] {
            // The photographed pill is the one that has to be visibly wrong when pinned.
            if w == 307.0 {
                let err = w * (crate::ui::widgets::CTRL_FOCUS_SCALE - 1.0);
                assert!(err > 15.0, "the reported imbalance was ~20px, not a hairline: {err}");
            }
            let group = w - 2.0 * ENTRY_OUTER_PAD; // label · count, the mark's gap, the mark's INK

            // At rest the centred start IS the pad, so nothing about a resting pill moved.
            assert!(
                (entry_run_x(w, 1.0) - ENTRY_OUTER_PAD).abs() < 0.01,
                "w {w}: a resting pill must still start on its own outer pad"
            );

            let e = crate::ui::widgets::CTRL_FOCUS_SCALE;
            let pw = w * e;
            let leading = entry_run_x(w, e);
            let trailing = pw - leading - group; // the mark's INK is what the far end answers to
            assert!(
                (leading - trailing).abs() < 0.01,
                "w {w}: {leading} of air before the label against {trailing} after the chevron"
            );
            assert!(
                leading > ENTRY_OUTER_PAD,
                "w {w}: the pop must widen BOTH ends, not only the far one"
            );

            // …and the defect itself, exactly: pinned to the pad, ALL of the pop's growth is
            // trailing air, so the two ends differ by the whole of `w * (e - 1)` — 20px at the
            // width the owner photographed, which is why they could see it and the resting
            // arithmetic could not.
            let pinned = pw - ENTRY_OUTER_PAD - group;
            assert!(
                (pinned - ENTRY_OUTER_PAD - w * (e - 1.0)).abs() < 0.01,
                "w {w}: the pinned walk's error is the pop's whole growth, by construction"
            );
        }
    }

    /// **The group reads as one thing inside the capsule: `XS` inside `SM` inside `MD`, optically.**
    /// The ordering is the whole design of this control — label/dot/count tight, the disclosure mark
    /// a step off them, and both still further inside the pill's own end. It only holds once the
    /// mark's bearing is discounted; measured to the box, the chevron's gap is 23.5 against a 24 pad
    /// and the middle rung disappears.
    #[test]
    fn the_entry_pill_groups_its_runs_tighter_than_it_pads_its_ends() {
        let chevron_air = ENTRY_CHEVRON_GAP; // already ink-measured — see `ENTRY_MARK_BEARING_L`
        assert!(
            ENTRY_RUN_GAP < chevron_air,
            "the dot's gaps must be tighter than the mark's"
        );
        assert!(
            chevron_air < ENTRY_OUTER_PAD,
            "the mark's gap must stay inside the pill's own end padding"
        );
        // …and the point of the correction, as the number that motivated it: measured to the BOX,
        // this gap lands within a pixel of the outer pad (23.5 against 24), so the middle rung is
        // not merely tight, it is GONE — two nominally different spacings the eye cannot separate.
        assert!(
            (chevron_air + ENTRY_MARK_BEARING_L - ENTRY_OUTER_PAD).abs() < 1.0,
            "the box-measured gap should be the indistinguishable-from-the-pad case this fixes"
        );
    }

    /// **The Filmography entry is the FLOOR of the page, and the focus order is header → shelves →
    /// entry.** DOWN off the last shelf reaches it, UP returns to that shelf, and DOWN on it does
    /// nothing at all — there is nothing under it.
    #[test]
    fn the_filmography_entry_is_the_bands_last_stop() {
        let _serial = crate::testlock::serial();
        seed(2, 2);
        crate::person::install_credits_for_test(&[("Actor", 7)]);
        clamp_focus();
        assert!(has_entry(crate::person::current().unwrap()));

        // **It is IN the band now** (2026-09-06), not a frame-wide row after the shelves — so it is
        // reached going UP off the first shelf, and it belongs to flow child 0. The page mounts on
        // the entry row now, not the first shelf card — but `seed()`'s OWN `clamp_focus()` runs
        // before `install_credits_for_test` below has given it anything to hold, so that first
        // resolution genuinely has no entry yet and lands on the shelf exactly as it would for a
        // real person turning out to have none — which is what leaves this walking UP to find it,
        // just not for the reason a page opened just now would.
        move_focus(SDLK_UP);
        assert!(scene().on_entry, "UP off the first shelf must reach the entry pill");
        assert_eq!(
            Column::focus_child(scene()),
            Some(0),
            "the pill lives in the BAND — the flow's last child is the last shelf"
        );
        assert!(
            focused_item().is_none(),
            "the pill holds no card — a shelf OK must not fire on it"
        );

        // UP again leaves it for the prose above; DOWN comes back and then out to the shelves.
        move_focus(SDLK_UP);
        assert!(!scene().on_entry);
        assert!(on_header(crate::person::current().unwrap(), scene()));
        move_focus(SDLK_DOWN);
        assert!(scene().on_entry, "DOWN off the band reaches the pill before the shelves");
        move_focus(SDLK_DOWN);
        assert!(!scene().on_entry, "…and DOWN again leaves the band for the first shelf");
        assert_eq!(scene().focus_kind, 0);
        crate::person::close();
    }

    /// **Regression pin, host-test-safe half.** `header_ok`'s overlay guard used to be reachable
    /// only by eye — see [`overlay_owns_the_press`]'s doc for why the function it guards cannot be
    /// called from here at all. This pins the guard's own condition: an unavailable filmography
    /// credit's OK press reaches `header_ok` exactly when an overlay is open, and this is the
    /// predicate that must say so, over the transitions the real bug hit (open → focus moved onto
    /// an unheld row → close).
    #[test]
    fn overlay_owns_the_press_tracks_both_overlays_this_page_owns() {
        let _serial = crate::testlock::serial();
        assert!(!overlay_owns_the_press());
        crate::ui::filmography::open();
        assert!(
            overlay_owns_the_press(),
            "a filmography credit nobody holds is exactly the case this guard exists for"
        );
        crate::ui::filmography::hide();
        assert!(!overlay_owns_the_press());
        crate::ui::person_bio::open();
        assert!(overlay_owns_the_press());
        crate::ui::person_bio::close();
        assert!(!overlay_owns_the_press());
    }

    /// **A person with NOTHING in your libraries still reaches the entry row**, which is the case
    /// it matters most on: it is then the only thing on the page worth pressing, and the page walks
    /// header ↔ entry with no shelves in between.
    #[test]
    fn a_page_with_no_shelves_still_walks_to_the_entry() {
        let _serial = crate::testlock::serial();
        seed(0, 0);
        crate::person::install_credits_for_test(&[("Actor", 3), ("Appeared", 1)]);
        clamp_focus();

        let p = crate::person::current().unwrap();
        assert!(on_header(p, scene()));
        move_focus(SDLK_DOWN);
        assert!(scene().on_entry, "DOWN from the header must reach the entry");
        let p = crate::person::current().unwrap();
        assert!(!on_header(p, scene()));
        move_focus(SDLK_UP);
        let p = crate::person::current().unwrap();
        assert!(on_header(p, scene()), "UP must return to the header");
        assert!(
            scene().header_marked,
            "…and that arrival is explicit, so the bio mark may show"
        );
        crate::person::close();
    }

    /// An entry row that GOES AWAY under the focus must hand it back rather than leaving the page
    /// focused on a child that no longer exists — the one thing `clamp_focus` still has to write,
    /// as against the header, which it deliberately does not.
    #[test]
    fn a_vanishing_entry_row_hands_its_focus_back() {
        let _serial = crate::testlock::serial();
        seed(1, 0);
        crate::person::install_credits_for_test(&[("Actor", 9)]);
        clamp_focus();
        move_focus(SDLK_UP); // the pill sits ABOVE the shelves now — see the test above
        assert!(scene().on_entry);

        crate::person::install_credits_for_test(&[]); // the credits answer is now empty
        clamp_focus();
        assert!(!scene().on_entry, "the entry is gone — focus cannot stay on it");
        assert_eq!(focused_item().map(|m| m.rk.clone()), Some("m0".to_string()));
        crate::person::close();
    }

    /// **Mounting a second person takes the first one's overlays down** — the invariant
    /// [`hide_overlays`] exists for, graded from the SIDE THE BUG ARRIVES FROM: `reopen`, not
    /// `leave`.
    ///
    /// The three-press repro is on `reopen`'s own comment and the shape of it is what makes this
    /// worth a test rather than a read-through: the path that strands an overlay never touches
    /// `leave` at all. Open A's Filmography, press a credit that IS in your library — that
    /// navigates FORWARD, so both pages stay on the trail and nothing is torn down — then open
    /// person B from the resulting page's cast row. B mounts through `reopen`, the route is
    /// `Person` either way, and an overlay left standing is holding A's model over B's page: it
    /// draws A's credits, and a press in it acts on A's item while the trail says B.
    ///
    /// Both overlays are asserted, because the fix is the SHARED call and a regression that
    /// re-inlines one screen's teardown would otherwise pass on the other's.
    #[test]
    fn mounting_a_second_person_takes_the_first_ones_overlays_down() {
        let _serial = crate::testlock::serial();
        seed(1, 0);
        crate::person::install_credits_for_test(&[("Actor", 9)]);
        crate::ui::filmography::open();
        crate::ui::person_bio::open();
        assert!(crate::ui::filmography::is_open());
        assert!(crate::ui::person_bio::is_open());

        // person B arrives — through `reopen`, exactly as a cast-row OK on another page does
        reopen(
            crate::plex::ServerId::UNSET,
            "88",
            "plex://person/88",
            "Somebody Else",
            "/b.jpg",
        );
        assert!(
            !crate::ui::filmography::is_open(),
            "B's page mounted under A's open Filmography route"
        );
        assert!(
            !crate::ui::person_bio::is_open(),
            "B's page mounted under A's open biography panel"
        );
        crate::person::close();
    }
}
