//! Vector icon assets (SVG) rasterized at runtime into tinted GL textures — the iOS-style
//! "ship the vector, render at runtime" approach. Each icon lives as an SVG file under
//! assets/icons/ (authored as a white #ffffff mask), embedded via include_str!. On first use
//! at a given pixel size we rasterize it (plx_gfx::svg → nanosvg), upload it once as a GL texture
//! (cached), and draw it through the Painter with a per-state tint. Main/GL-thread only.
//!
//! ## Authoring contract (what an asset may contain)
//!
//! The result is a **mask**: only alpha survives, so gradients and multi-colour fills are wasted
//! and the tint is the whole colour story. Beyond that, two rules that are not obvious until a
//! mark looks wrong on the panel — both verified by rasterizing through `src/svg.c` itself:
//!
//! 1. **A mark is ONE `<path>`; a composite mark is that path's overlapping SUBPATHS.** Subpaths
//!    of one path are winding-unioned by the rasterizer, so the joins carry no seam. Separate
//!    `<circle>`/`<path>` ELEMENTS are alpha-composited instead — `a1 + a2(1-a1)` — so wherever
//!    two antialiased edges run together the union lands at ~0.75 alpha and the mark wears a
//!    visible crease. The pre-redraw `popcorn-spilled.svg` did exactly that (140/255 at 34px,
//!    16/255 at 136px — a composite seam gets WORSE with resolution, which is how it tells itself
//!    apart from a real notch).
//! 2. **Every subpath winds the same way** (these are all clockwise). Nonzero fill turns a
//!    counter-clockwise subpath into a HOLE punched through whatever it overlaps, which looks
//!    like a rasterizer bug and is not one.
//!
//! Grade a new mark by rasterizing it at its real draw size and at 4×: full opacity reached, no
//! sub-255 pixel more than 2px inside the ink except where the geometry really is notched (it
//! resolves to a clean gap at 4×), and no ink on the border.
#![allow(dead_code)]
use crate::{Painter, Rect};
use std::cell::RefCell;
use std::os::raw::c_uint;

// `Debug` so a host test can name the glyph it expected: `widgets`' play-indicator table grades
// an Option<Icon> per cell and an assertion that cannot print the two sides is one nobody reads.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Icon {
    /// Two people joined by one agreement line. A one-colour stroked mask that NanoSVG can
    /// rasterize at icon and artwork sizes without unsupported filters.
    Agreement,
    Cc,
    Audio,
    Check,
    Chevron, // points RIGHT; the directional variants below are separate masks (the
    // rasterizer draws untransformed, so direction is per-asset, not a rotation)
    ChevronDown,
    ChevronUp,
    /// Points LEFT — the route crumb's mark, and the mirror of [`Icon::Chevron`] rather than a
    /// second drawing: same viewBox, same stroke, the path reflected about x=12, so the two share
    /// one [`ink_x`] entry and a row's drill-in chevron and a crumb's return chevron are visibly
    /// the same object pointing two ways.
    ChevronLeft,
    /// The watch-state ACTIONS, as **filled** discs: a check knocked out of one for "Mark as
    /// Watched", a minus knocked out of one for "Mark as Unwatched" (`screens::item_menu::state_rows`).
    /// Filled is the rule, not a preference: it is what stops an ACTION being read as a STATE. The
    /// leading column carries a picker's bare tick or an action's glyph and nothing else — a switch
    /// states itself as a word at the row's trailing edge (`Row::toggle`), so a hollow circle here
    /// would be a third grammar for the same column. (There was one, briefly, on 2026-08-13: a
    /// ring/ticked-ring pair. The design system deleted both assets the same evening.)
    ///
    /// Both are one `<path>` with **`fill-rule="evenodd"`**, which is how the mark is knocked out of
    /// the disc — the one place this set departs from the all-subpaths-wind-the-same-way rule above,
    /// and it is load-bearing: under nonzero the knockout fills solid and the mark disappears.
    /// Verified through `src/svg.c` itself at 26px and 4×, where the gap resolves clean.
    CheckCircleFill,
    /// See [`Icon::CheckCircleFill`].
    MinusCircleFill,
    /// A bare horizontal stroke — the "remove" half of the bare [`Icon::Check`], for a control that
    /// is ALREADY a circle (a hero or detail disc button), where a filled disc inside a disc would
    /// be two circles saying one thing. Its user is the detail hero's *mark unwatched* disc, beside
    /// a `Check` on the same ground: the pair `detail::hero_ctls` draws for a part-watched item.
    Minus,
    /// The player row's Previous/Next episode discs.
    Previous,
    Next,
    Play,
    Pause,
    /// The transport pair — two filled triangles, pointing back and forward. Worn by ONE surface:
    /// the player HUD's transport state read-out beside the elapsed clock (`player_hud::draw_hud`,
    /// via `plx_appkit::player_hud::transport_mark`), where `Rewind` marks the playhead travelling
    /// BACKWARDS and `FastForward` marks it travelling forwards — a scrub, a chapter/marker hop and
    /// a rapid-seek burst alike. They are a read-out, never a control: there is no rewind BUTTON in
    /// this app and the design system forbids adding one.
    ///
    /// **Both live in [`Icon::Pause`]'s own 14-unit band** (y 5..19 of a 24 viewBox), which is the
    /// design system's transport rule rather than a coincidence — the read-out is one small slot
    /// that swaps glyphs under a running clock, and a family drawn to a common band does not shift
    /// optical weight when the state flips. Do not rescale either mark to "fill the box".
    ///
    /// Two `<path>` ELEMENTS rather than one path's subpaths, which the module doc's rule 1 would
    /// normally forbid — permitted for exactly [`Icon::Crowd`]'s reason: the triangles do not touch
    /// (0.8 units apart), so there is no join for the alpha composite to crease along.
    Rewind,
    /// See [`Icon::Rewind`].
    FastForward,
    /// Counter-clockwise circular arrow (↺) — **the detail hero's restart disc, and only that**.
    ///
    /// It is the same ACTION as [`Icon::PlayStart`], which is normally how a second mark gets
    /// deleted, and this one was — on 2026-08-21, on exactly that reasoning. It came back the next
    /// day because the argument had the wrong scope. Cross-surface consistency is worth having,
    /// but it loses to DISCRIMINABILITY WITHIN A ROW, and the hero is the only place these two
    /// facts collide: the disc sits ~20px from the Play/Resume pill, it is icon-ONLY at rest (the
    /// verb is behind the unfurl, so it exists for a fraction of a second on focus and never for
    /// the control beside it), and `play-start` is a play triangle with a bar — at three metres,
    /// the pill's ▶ with a tick of extra ink. Two controls that start playback, distinguished by
    /// a 3px stem.
    ///
    /// The menu ROW has neither problem: its glyph is 20px from the words *Play from Start*, and
    /// nothing beside it is a play triangle. So the row keeps `PlayStart` and the disc takes this,
    /// and the divergence is the point rather than drift — see [`Icon::PlayStart`].
    Restart,
    /// A play triangle knocked out of a landscape frame — the detail hero's Trailer disc and
    /// the item menu's Play Trailer row. One path, evenodd knockout, same construction as
    /// [`Icon::CheckCircleFill`].
    Trailer,
    Info,
    /// Warning triangle — `info.svg`'s sibling (same 24 viewBox, 2.2 stroke, round caps/joins,
    /// dot-and-bar inverted). From `Plex Pass Awareness.dc.html`: the facts row's HDR chip at
    /// ~24px and the playback-failed read-out at 96px. Outline, not filled — a solid triangle
    /// reads as an error state where this marks a warning or a verdict already worded in text.
    Alert,
    User,
    Backspace,
    /// A screen with a play triangle — the item menu's "Go to Episode" leading glyph.
    Episode,
    /// A stack of layers — the item menu's "Go to Show" leading glyph (a series of episodes).
    Show,
    /// A portrait card with a second one behind it — a COLLECTION, drawn by the neutral tile of a
    /// collection that has no artwork of its own (`ui::collection_tile`). Two stroked elements
    /// that never touch (a unit of air between them), so rule 1's crease cannot form.
    Collection,
    /// A play triangle behind a leading bar — **"Play from Start"** (restart, not resume), worn by
    /// the card menu's row of that name and by nothing else.
    ///
    /// It is the better mark of the two, and that is why it has the surface where a mark is only
    /// reinforcement: it says *from the beginning*, where [`Icon::Restart`]'s `↺` says *again*,
    /// which is a different promise from the one the press keeps. What it cannot do is be the sole
    /// carrier of "this is not the Play button" while standing next to the Play button — a
    /// triangle-plus-bar against a triangle, both icon-only, at three metres. That is the hero
    /// disc's whole job, so the hero takes `Restart` instead.
    ///
    /// **The two are one action deliberately drawn twice, and the split is by SURFACE, not by
    /// accident**: wherever the verb is written out beside the glyph, this is the mark. Read
    /// [`Icon::Restart`] before reconciling them — they were reconciled once, on 2026-08-21, and it
    /// was wrong.
    PlayStart,
    /// An X — "remove this" (the item menu's Remove from Continue Watching row).
    Close,
    /// A horizontal ellipsis — "more options". The player transport's third control disc, which
    /// opens the overflow popover (`appkit/more_menu.rs`). Overflow, so it sits at the END of the row.
    More,
    /// The magnifier — the only pill in the shared top strip that is a MARK instead of a word
    /// (`ui_kits/tv-app/SearchScreen.jsx`). Drawn at 1.15× the strip's own type rung, inked exactly as a
    /// label would be, so it reads as one of the row rather than as an ornament on it.
    ///
    /// ONE `<path>`, two subpaths (the ring as a pair of half-arcs, then the handle), both STROKED
    /// — `info.svg`'s construction, and the reason it is one element rather than a `<circle>` plus
    /// a `<line>`: separate elements alpha-composite, and where the handle meets the ring their two
    /// antialiased edges would land at ~0.75 and wear a visible crease (see the module doc). The
    /// handle also starts just outside the ring, so the round caps close the joint without the two
    /// strokes overlapping at all.
    Search,
    // ---- review-score marks (the detail hero's ratings row) ----
    //
    // These are OUR OWN drawings, not reproductions. Rotten Tomatoes' marks — the fruit, the
    // Certified Fresh seal, the popcorn tub — were shipped here until 2026-08-02 and removed:
    // there is no licensing route for them (RT's developer programme is closed to unofficial
    // projects and `developer.fandango.com` does not resolve), and redrawing a mark is the
    // standard infringement pattern rather than a defence. The provider is now NAMED in text
    // instead, which is referential use and needs no licence — so the glyph no longer has to say
    // *whose* score this is. It only has to carry the VERDICT, which is what these four do.
    //
    // Two layers per mark for the same reason as before: the rasterizer renders a MASK and the
    // colour is the tint (`theme::RATING_*`), so a two-tone mark needs two masks at one rect.
    // Both critic layers share one 26×26 viewBox and register exactly.
    /// The tomato's body. Tinted [`theme::RATING_FRESH`] for a ripe score and
    /// [`theme::RATING_CERTIFIED`] for the rarer Certified bar — the SAME fruit struck in gold,
    /// not a seal, which is the one substantive simplification against the retired art.
    Tomato,
    /// The stem-and-sepals over [`Icon::Tomato`]. Painted [`theme::RATING_LEAF`] on a fresh or
    /// certified body and [`theme::RATING_MUTED`] on a hollow one. Its base sits INSIDE the body's
    /// silhouette, so the two masks overlap solidly instead of meeting at a seam.
    TomatoCalyx,
    /// A rotten score: the same fruit drained to an outline. A stroked ring rather than a splat —
    /// the negation is "the colour has gone out of it", which needs no second device.
    TomatoHollow,
    /// The audience mark: a CROWD — two figures under one shoulder line, so it reads as "many
    /// people" rather than "a person". One layer, and the only mark here with four elements in it:
    /// permitted because none of them touch (heads clear their bodies by ~2.5 units and the two
    /// figures do not overlap in x), so there is no antialiased seam for them to composite across.
    /// It negates by DRAINING to [`theme::RATING_MUTED`] rather than going hollow: a single fruit
    /// can carry an outline, but outlining every shape in a crowd is a tangle of strokes at 30px.
    Crowd,
    // ---- read-out glyphs (the 112px mark `StatusOverlay::page` draws above a page-filling
    // `Failed` verdict — spec "1A") ----
    //
    // Twelve marks, one family: a BASE says what is involved (a server, plex.tv, an account, a
    // sign-in's stored key, a profile roster, a wait's clock), a BADGE knocked out of it says what
    // went wrong (a plus/minus/x/question/alert), and `telemetry::incident::IncidentContext::
    // readout_glyph` is the ONE place that reads an `IncidentKind` and picks which. `WifiSlash`
    // stands alone — there is no server to blame when the TV itself has no link. Drawn by
    // `tools/readout-glyphs.py` (`shapely` polygon booleans, not hand-authored paths) because the
    // badge's knockout is `fill-rule="evenodd"` — see [`Icon::CheckCircleFill`]'s doc for why that
    // is the one place this module's "every subpath winds the same way" rule does not hold — and a
    // hand-drawn badge-on-base composite would recreate exactly the seam that doc warns about.
    /// A clock, alert badge — a wait that ran out (`IncidentKind::PinExpired`/`LinkStalled`).
    ClockBadgeAlert,
    /// A cloud, alert badge — plex.tv answered but with an error, or the app's own machinery
    /// failed (an `Internal` cause, the closest honest read among these twelve).
    CloudBadgeAlert,
    /// A globe, minus badge — plex.tv did not answer and the failure is not DNS or TLS.
    GlobeBadgeMinus,
    /// A globe, question badge — plex.tv could not be found (DNS).
    GlobeBadgeQuestion,
    /// A key, alert badge — the sign-in could not be saved or read back
    /// (`IncidentKind::SaveFailed`/`StoredLocked`).
    KeyBadgeAlert,
    /// A lock, alert badge — plex.tv could not be reached securely (TLS).
    LockBadgeAlert,
    /// Two people, alert badge — a profile switch failed (`IncidentKind::ProfileSwitch`).
    PeopleBadgeAlert,
    /// One person, X badge — plex.tv refused the account token (`IncidentKind::Authorization`).
    PersonBadgeXmark,
    /// A server, minus badge — a known server did not answer (Home's and the Library's own
    /// "can't reach" verdict, and `Discovery(Silent)` targeting `Servers`).
    ServerBadgeMinus,
    /// A server, plus badge — the account has no server at all (`DiscoveryClass::NoServers`).
    ServerBadgePlus,
    /// A server, X badge — a server answered and refused (`DiscoveryClass::Refused`).
    ServerBadgeXmark,
    /// A crossed-out wifi arc — no internet to even reach plex.tv (`IncidentKind::PinCreate`).
    WifiSlash,
    // ---- Watchlist marks (docs/watchlist-plan.md, DS1) ----
    //
    // Baked by `tools/readout-glyphs.py`'s INLINE table at the inline 2.2 weight (search, restart,
    // info), one even-odd path each, so they sit beside the stroked family without a weight step.
    /// A bookmark ribbon, outline — the Watchlist's not-on-the-list mark.
    Bookmark,
    /// The same silhouette filled, so the outline-to-filled state flip never moves an edge.
    BookmarkFill,
    /// A plain clock face — the Watchlist's coming-soon mark (the read-out's clock base, inline weight).
    Clock,
    /// A server with a slash through it — the Watchlist's server-offline mark (the read-out's server
    /// base, slashed the way [`Icon::WifiSlash`] is).
    ServerSlash,
    /// A ring with a bar through it — the Watchlist's on-no-server mark, the universal "unavailable"
    /// sign at the inline weight.
    CircleSlash,
}

/// **Where a mark's INK sits inside its 24-unit viewBox**, as `(left, right)` fractions — and
/// [`band`] below, the vertical half of the same question.
///
/// Here rather than in the screens because it is a property of the ASSET. Two screens had already
/// transcribed it independently and in two different shapes — `player_hud`'s transport slot carried
/// an `(l, r)` tuple per glyph so the gap after the elapsed clock is measured to ink rather than to
/// box, and `home`'s hero pager carried a single leading bearing for `chevron.svg` — with nothing in
/// either file pointing back at the `.svg` whose numbers they are. `assets/icons/*.svg` is exactly
/// the kind of file that gets re-drawn (this module's own doc invites it), and a re-draw moved both
/// of those silently: no compile error, no test.
///
/// Default `(0.0, 1.0)` is "ink fills the box", which is what an unmeasured mark is assumed to do
/// and what every caller wanted before any of this existed.
pub const fn ink_x(id: Icon) -> (f32, f32) {
    match id {
        Icon::Pause => (7.0 / 24.0, 17.0 / 24.0),
        Icon::Play => (6.0 / 24.0, 20.0 / 24.0),
        Icon::Rewind | Icon::FastForward => (2.6 / 24.0, 21.4 / 24.0),
        // One entry for the mirrored pair: the reflected path has the same ink bounds.
        Icon::Chevron | Icon::ChevronLeft => (7.5 / 24.0, 16.5 / 24.0),
        // The fold pair: path x 6..18 plus the 3-unit stroke's 1.5 each side. One entry, the pair
        // reflected about y=12 (`the_fold_chevrons_ink_matches_their_assets`).
        Icon::ChevronDown | Icon::ChevronUp => (4.5 / 24.0, 19.5 / 24.0),
        // The bookmark pair share one silhouette, so one entry (the fill is the outline's own shape).
        Icon::Bookmark | Icon::BookmarkFill => (4.4 / 24.0, 19.6 / 24.0),
        Icon::Clock | Icon::ServerSlash | Icon::CircleSlash => (1.9 / 24.0, 22.1 / 24.0),
        _ => (0.0, 1.0),
    }
}

/// A mark's ink HEIGHT as a fraction of its viewBox. The transport family — rewind, pause,
/// fast-forward — is authored to one 14-unit band precisely so a small player HUD never shifts
/// weight when the state flips, and `play.svg` is the one member that is NOT: it spans y=4..20, so
/// 16 units, because it predates that slot and is worn elsewhere at its own size. A caller drawing
/// the family in one slot scales by `band(Pause) / band(id)` and the odd one out stops being the
/// player HUD's private problem.
pub const fn band(id: Icon) -> f32 {
    match id {
        Icon::Pause | Icon::Rewind | Icon::FastForward => 14.0 / 24.0,
        Icon::Play => 16.0 / 24.0,
        // Measured ink heights of the Watchlist marks (y 1.9..21.4, 1.9..22.1, 1.5..22.5).
        Icon::Bookmark | Icon::BookmarkFill => 19.5 / 24.0,
        Icon::Clock | Icon::CircleSlash => 20.2 / 24.0,
        Icon::ServerSlash => 21.0 / 24.0,
        _ => 1.0,
    }
}

fn src(id: Icon) -> &'static str {
    match id {
        Icon::Agreement => include_str!("../../../assets/icons/agreement.svg"),
        Icon::Cc => include_str!("../../../assets/icons/cc.svg"),
        Icon::Audio => include_str!("../../../assets/icons/audio.svg"),
        Icon::Check => include_str!("../../../assets/icons/check.svg"),
        Icon::Chevron => include_str!("../../../assets/icons/chevron.svg"),
        Icon::ChevronLeft => include_str!("../../../assets/icons/chevron-left.svg"),
        Icon::ChevronDown => include_str!("../../../assets/icons/chevron-down.svg"),
        Icon::ChevronUp => include_str!("../../../assets/icons/chevron-up.svg"),
        Icon::CheckCircleFill => include_str!("../../../assets/icons/check-circle-fill.svg"),
        Icon::MinusCircleFill => include_str!("../../../assets/icons/minus-circle-fill.svg"),
        Icon::Minus => include_str!("../../../assets/icons/minus.svg"),
        Icon::Play => include_str!("../../../assets/icons/play.svg"),
        Icon::Pause => include_str!("../../../assets/icons/pause.svg"),
        Icon::Rewind => include_str!("../../../assets/icons/rewind.svg"),
        Icon::FastForward => include_str!("../../../assets/icons/fast-forward.svg"),
        Icon::Restart => include_str!("../../../assets/icons/restart.svg"),
        Icon::Trailer => include_str!("../../../assets/icons/trailer.svg"),
        Icon::Info => include_str!("../../../assets/icons/info.svg"),
        Icon::Alert => include_str!("../../../assets/icons/alert.svg"),
        Icon::User => include_str!("../../../assets/icons/user.svg"),
        Icon::Backspace => include_str!("../../../assets/icons/backspace.svg"),
        Icon::Episode => include_str!("../../../assets/icons/episode.svg"),
        Icon::Show => include_str!("../../../assets/icons/show.svg"),
        Icon::Collection => include_str!("../../../assets/icons/collection.svg"),
        Icon::Previous => include_str!("../../../assets/icons/previous.svg"),
        Icon::Next => include_str!("../../../assets/icons/next.svg"),
        Icon::PlayStart => include_str!("../../../assets/icons/play-start.svg"),
        Icon::Close => include_str!("../../../assets/icons/close.svg"),
        Icon::More => include_str!("../../../assets/icons/more.svg"),
        Icon::Search => include_str!("../../../assets/icons/search.svg"),
        Icon::Tomato => include_str!("../../../assets/icons/tomato.svg"),
        Icon::TomatoCalyx => include_str!("../../../assets/icons/tomato-calyx.svg"),
        Icon::TomatoHollow => include_str!("../../../assets/icons/tomato-hollow.svg"),
        Icon::Crowd => include_str!("../../../assets/icons/crowd.svg"),
        Icon::ClockBadgeAlert => include_str!("../../../assets/icons/clock-badge-alert.svg"),
        Icon::CloudBadgeAlert => include_str!("../../../assets/icons/cloud-badge-alert.svg"),
        Icon::GlobeBadgeMinus => include_str!("../../../assets/icons/globe-badge-minus.svg"),
        Icon::GlobeBadgeQuestion => include_str!("../../../assets/icons/globe-badge-question.svg"),
        Icon::KeyBadgeAlert => include_str!("../../../assets/icons/key-badge-alert.svg"),
        Icon::LockBadgeAlert => include_str!("../../../assets/icons/lock-badge-alert.svg"),
        Icon::PeopleBadgeAlert => include_str!("../../../assets/icons/people-badge-alert.svg"),
        Icon::PersonBadgeXmark => include_str!("../../../assets/icons/person-badge-xmark.svg"),
        Icon::ServerBadgeMinus => include_str!("../../../assets/icons/server-badge-minus.svg"),
        Icon::ServerBadgePlus => include_str!("../../../assets/icons/server-badge-plus.svg"),
        Icon::ServerBadgeXmark => include_str!("../../../assets/icons/server-badge-xmark.svg"),
        Icon::WifiSlash => include_str!("../../../assets/icons/wifi-slash.svg"),
        Icon::Bookmark => include_str!("../../../assets/icons/bookmark.svg"),
        Icon::BookmarkFill => include_str!("../../../assets/icons/bookmark-fill.svg"),
        Icon::Clock => include_str!("../../../assets/icons/clock.svg"),
        Icon::ServerSlash => include_str!("../../../assets/icons/server-slash.svg"),
        Icon::CircleSlash => include_str!("../../../assets/icons/circle-slash.svg"),
    }
}

// (icon, px) → GL texture. The UI is fixed 1080p so only a handful of (icon,size) pairs ever
// appear; a flat Vec is plenty. Rasterize+upload once, then reuse.
//
// A `thread_local!`, because the texture names it holds belong to the GL context of the thread that
// uploaded them: the render thread, the only one that draws. (The entries have no destructor, so
// nothing runs GL at thread exit; the textures live for the process, as `CACHE` never evicts.)
struct Entry {
    id: Icon,
    px: i32,
    tex: c_uint,
}
thread_local! {
    static CACHE: RefCell<Vec<Entry>> = const { RefCell::new(Vec::new()) };
}

// Antialias the way text does — rasterise the vector at the *exact* draw size and keep nanosvg's own
// coverage-AA edge (SS = 1: no supersample). The old path rasterised SS× larger and let GL minify it
// down, but GL_LINEAR only samples 2×2 texels for an SS×SS footprint, so it under-filtered and
// re-aliased the edge (and GLES2 can't mipmap these NPOT masks). Drawing 1:1 keeps the edge crisp.
// `downsample_alpha` then just normalises rgb → white so straight-alpha edges never fringe dark.
// (Bump SS to supersample + box-downsample here if a size ever looks jaggy.)
const SS: i32 = 1;

/// The largest square any icon is ever rasterized at — the page read-out's 112px glyph
/// (`ui::widgets::StatusOverlay::GLYPH_SIZE`), the largest consumer today. `icon_raster_px`
/// clamps to this so a runaway caller cannot blow the texture cache open-ended — `tex_for` keys
/// `CACHE` on the CLAMPED size, not the caller's raw `px` (each distinct `(Icon, clamped px)`
/// pair keeps its own GL texture for the process lifetime — `CACHE` never evicts), so every
/// runaway `px` above this cap collapses onto the same one entry rather than minting a new
/// texture per distinct oversized value — while staying wide enough that every size this
/// codebase actually draws — including the page glyph's shrink toward
/// `StatusOverlay::GLYPH_MIN_SIZE` when `glyph_ceiling` is tight — rasterizes 1:1 rather than
/// being upscaled from a smaller raster and going soft. Raise this, not the clamp's magic number,
/// if a future icon needs to draw larger still.
const MAX_ICON_PX: i32 = 112;
/// The pixel size `tex_for` actually rasterizes a draw of `px` at, before `render_scale`'s
/// multiply. Split out so a host test can assert a real draw size survives the clamp instead of
/// a bypassed `rasterize()` call at a hand-picked size — which is how the clamp sitting at 96
/// silently downscaled the 112px page glyph (soft on a real panel) with every existing icon test
/// still green, since none of them rasterized through this function at all.
pub fn icon_raster_px(px: i32) -> i32 {
    px.clamp(8, MAX_ICON_PX)
}

fn tex_for(id: Icon, px: i32) -> c_uint {
    // Keyed on the CLAMPED size, not the caller's raw `px` — two different `px` values that
    // land on the same `icon_raster_px` result rasterize identically, so they share one
    // texture instead of minting a duplicate. This is also what makes `MAX_ICON_PX`'s own
    // doc claim ("a runaway caller cannot blow the cache open-ended") actually true: keyed on
    // the raw `px`, a caller sweeping through distinct oversized values still minted one
    // entry per value, unbounded, the clamp having capped only the RASTER, not the cache.
    let raster_px = icon_raster_px(px);
    // The cell is borrowed only for the lookup and, below, for the push: the rasteriser and the
    // upload run with no borrow held, so nothing they call can meet one.
    if let Some(tex) = CACHE.with(|cache| {
        cache.borrow().iter().find(|e| e.id == id && e.px == raster_px).map(|e| e.tex)
    }) {
        return tex;
    }
    // `render_scale` is the simulator's supersampling (1 on a television): the mask is
    // rasterised at physical size and still drawn into the same logical rect.
    let target = raster_px * plx_base::surface::render_scale();
    let hi = target * SS;
    let tex = match plx_gfx::svg::rasterize(src(id), hi, hi) {
        Some(rgba) => {
            let small = downsample_alpha(&rgba, hi, SS);
            upload(target, &small)
        }
        None => 0,
    };
    CACHE.with(|cache| cache.borrow_mut().push(Entry { id, px: raster_px, tex }));
    tex
}

/// Upload a rasterized mask, or — in a HOST TEST — do not.
///
/// The same seam, and the same reason, as `underlay::upload`: the test binary LINKS OpenGL but
/// never creates a context, so `glGenTextures` dereferences a null dispatch table and takes the
/// whole binary down. A host test that draws a whole page descriptively (`frame::backdrop::discover`)
/// reaches this through the page header's icons; it is handed a stand-in name per cache entry,
/// specced in `gfx::tex_ledger` as the real upload would be, so the paint the icon declares still
/// carries a texture and the cache keeps its one-entry-per-size shape.
fn upload(side: i32, px: &[u8]) -> c_uint {
    #[cfg(not(any(test, feature = "test-support")))]
    {
        plx_gfx::gfx::upload_rgba(0, side, side, px.as_ptr())
    }
    #[cfg(any(test, feature = "test-support"))]
    {
        use std::sync::atomic::{AtomicU32, Ordering};
        let _ = px;
        /// Far above any name a test hand-picks for its own stand-in fields.
        const STAND_IN_BASE: u32 = 0x4943_0000;
        static NEXT: AtomicU32 = AtomicU32::new(STAND_IN_BASE);
        let tex = NEXT.fetch_add(1, Ordering::Relaxed);
        plx_gfx::gfx::tex_ledger::specified(tex, side, side);
        tex
    }
}

/// Box-average each `ss`×`ss` block of the supersampled mask into one output texel. The alpha is the
/// mean coverage (the clean AA edge); rgb is forced white so bilinear/compositing never darkens the
/// edge — the icon's colour comes entirely from the draw tint (`FS_IMG`: `c.rgb*tint.rgb`, coverage
/// `c.a`), so straight-alpha edge pixels would otherwise fringe dark.
fn downsample_alpha(src: &[u8], sw: i32, ss: i32) -> Vec<u8> {
    let dw = sw / ss;
    let mut out = vec![255u8; (dw * dw * 4) as usize];
    let n = (ss * ss) as u32;
    for y in 0..dw {
        for x in 0..dw {
            let mut a = 0u32;
            for jy in 0..ss {
                for jx in 0..ss {
                    a += src[(((y * ss + jy) * sw + (x * ss + jx)) * 4 + 3) as usize] as u32;
                }
            }
            out[((y * dw + x) * 4 + 3) as usize] = (a / n) as u8;
        }
    }
    out
}

/// Draw icon `id` filling `r` (rasterized+cached at r's pixel size), tinted `tint` (a white mask
/// times the tint = a solid-colour icon; tint alpha fades it). No-op if rasterization failed.
pub fn draw(p: Painter, id: Icon, r: Rect, tint: [f32; 4]) {
    draw_raster(p, id, r, r.w.max(r.h).round() as i32, tint);
}

/// [`draw`] with the RASTER's size chosen by the caller instead of read off `r`: the mask cached at
/// `raster_px` is stretched over `r`. For a glyph whose box moves continuously (a control popping
/// on its lift spring) so the mask it is drawn from stays ONE texture for the whole motion: sized
/// from `r`, the cache key would change at each whole pixel the box crosses, and the glyph would be
/// re-rasterized (a different edge, with nothing moving) on the frame the spring settles. At
/// `r.w == raster_px` this is exactly [`draw`], 1:1.
pub fn draw_raster(p: Painter, id: Icon, r: Rect, raster_px: i32, tint: [f32; 4]) {
    let px = raster_px;
    // A text-recording painter submits no primitive; rasterising the mask for it would only
    // spend the transition's prewarm budget on something it then does not draw.
    if px <= 0 || p.is_recording() {
        #[cfg(any(test, feature = "test-support"))]
        if p.is_recording() {
            crate::draw_census::note_icon(id, p.place(r));
        }
        return;
    }
    let tex = tex_for(id, px);
    if tex != 0 {
        // 1:1 mask — snap the COMPOSITED origin (fold the painter translate, snap, unfold),
        // same contract as text; see gfx::snap.
        let r = Rect::new(
            plx_gfx::gfx::snap(r.x + p.dx) - p.dx,
            plx_gfx::gfx::snap(r.y + p.dy) - p.dy,
            r.w,
            r.h,
        );
        p.tex(tex, r, 0.0, tint);
    }
}

#[cfg(test)]
mod ink_tests {
    use super::*;

    /// **The transport family shares one band, and `Play` is the documented exception.** This is
    /// the invariant `player_hud`'s slot depends on — it scales every mark by
    /// `band(Pause) / band(id)` so the read-out never shifts weight as the state flips — and the
    /// way it breaks is a FIFTH transport glyph added later, authored to its own box, with nothing
    /// to notice. A `Skip +10` chevron pair is the obvious next one.
    #[test]
    fn the_transport_family_is_one_band() {
        let b = band(Icon::Pause);
        for id in [Icon::Rewind, Icon::FastForward] {
            assert_eq!(band(id), b, "every travel mark must share pause's band");
        }
        assert_ne!(
            band(Icon::Play),
            b,
            "play.svg is the off-band member; the scale exists for it"
        );
    }

    /// Ink bounds are ordered and inside the viewBox — the shape every caller assumes.
    #[test]
    fn ink_bounds_are_sane() {
        for id in [
            Icon::Pause,
            Icon::Play,
            Icon::Rewind,
            Icon::FastForward,
            Icon::Chevron,
            Icon::ChevronLeft,
            Icon::ChevronDown,
            Icon::ChevronUp,
            Icon::Check,
        ] {
            let (l, r) = ink_x(id);
            assert!(
                (0.0..1.0).contains(&l) && r > l && r <= 1.0,
                "ink_x {l}..{r} out of shape"
            );
        }
    }

    /// **The crumb's chevron is the row chevron reflected, not a second drawing.** They share an
    /// [`ink_x`] entry, so the day somebody re-draws either asset to different bounds the shared
    /// entry becomes a lie for one of them with nothing to notice — the exact failure `ink_x`'s
    /// own doc was written about. Reflection about x=12 is checkable from the sources themselves.
    #[test]
    fn the_crumb_chevron_is_the_row_chevron_mirrored() {
        assert_eq!(ink_x(Icon::ChevronLeft), ink_x(Icon::Chevron));
        assert!(src(Icon::Chevron).contains(r#"d="M9 6l6 6-6 6""#));
        assert!(src(Icon::ChevronLeft).contains(r#"d="M15 6l-6 6 6 6""#));
    }

    /// The fold chevrons' `ink_x` is their asset's own bounds (path x 6..18, stroke 3), and the up
    /// mark is the down mark reflected about y=12, so one entry serves the pair.
    #[test]
    fn the_fold_chevrons_ink_matches_their_assets() {
        assert_eq!(ink_x(Icon::ChevronDown), ink_x(Icon::ChevronUp));
        assert!(src(Icon::ChevronDown).contains(r#"d="M6 9l6 6 6-6""#));
        assert!(src(Icon::ChevronUp).contains(r#"d="M6 15l6-6 6 6""#));
        let (l, r) = ink_x(Icon::ChevronDown);
        assert!((l - (6.0 - 1.5) / 24.0).abs() < 1e-6 && (r - (18.0 + 1.5) / 24.0).abs() < 1e-6);
    }

    #[test]
    fn agreement_art_stays_inside_the_nanosvg_subset() {
        let svg = src(Icon::Agreement);
        assert!(svg.contains("<path"));
        for unsupported in ["<filter", "feDropShadow", "<image", "<foreignObject"] {
            assert!(
                !svg.contains(unsupported),
                "agreement asset uses unsupported {unsupported}"
            );
        }
    }

    /// Every one of the twelve read-out marks (spec "1A") rasterizes at the size
    /// `StatusOverlay::page`'s glyph actually draws them at (112px, this family's only draw
    /// size) — `plx_gfx::svg::rasterize` returns `Some`, reaches full opacity somewhere inside the
    /// mask (nanosvg did not silently fail to fill the shape), and leaves the outermost ring of
    /// pixels untouched (no ink on the border), the same two checks the module doc's own
    /// authoring contract asks a human to grade by eye.
    #[test]
    fn every_readout_glyph_rasterizes_clean_at_its_draw_size() {
        // Routed through `icon_raster_px`, the SAME clamp `tex_for` applies to a real draw —
        // not a bypassed `rasterize()` call at a hand-picked size. This is what catches a clamp
        // sitting below the page glyph's natural size again: `icon_raster_px(112)` would come
        // back 96 under the old cap, and every assertion below would then be grading a 96px
        // raster while believing it was 112.
        let px = icon_raster_px(112);
        assert_eq!(px, 112, "the page glyph's 112px draw size must survive the production clamp");
        for id in [
            Icon::ClockBadgeAlert,
            Icon::CloudBadgeAlert,
            Icon::GlobeBadgeMinus,
            Icon::GlobeBadgeQuestion,
            Icon::KeyBadgeAlert,
            Icon::LockBadgeAlert,
            Icon::PeopleBadgeAlert,
            Icon::PersonBadgeXmark,
            Icon::ServerBadgeMinus,
            Icon::ServerBadgePlus,
            Icon::ServerBadgeXmark,
            Icon::WifiSlash,
        ] {
            let rgba = plx_gfx::svg::rasterize(src(id), px, px)
                .unwrap_or_else(|| panic!("{id:?} failed to rasterize at {px}px"));
            assert_eq!(rgba.len(), (px * px * 4) as usize, "{id:?} wrong buffer size");
            let alpha = |x: i32, y: i32| rgba[((y * px + x) * 4 + 3) as usize];
            let max_alpha = (0..px).flat_map(|y| (0..px).map(move |x| alpha(x, y))).max().unwrap();
            assert_eq!(max_alpha, 255, "{id:?} never reaches full opacity at {px}px");
            for x in 0..px {
                assert_eq!(alpha(x, 0), 0, "{id:?} has ink on the top border");
                assert_eq!(alpha(x, px - 1), 0, "{id:?} has ink on the bottom border");
            }
            for y in 0..px {
                assert_eq!(alpha(0, y), 0, "{id:?} has ink on the left border");
                assert_eq!(alpha(px - 1, y), 0, "{id:?} has ink on the right border");
            }
        }
    }

    /// The collection mark at the sizes its tiles draw it (`collection_tile::glyph_px` of each
    /// page's RESTING tile width; a focus pop does not change the size, the card's zoom grows the
    /// cached mask): full opacity inside, nothing on the border.
    #[test]
    fn the_collection_mark_rasterizes_clean_at_its_tile_sizes() {
        for px in [64, 72, 76, 80, 84] {
            let rgba = plx_gfx::svg::rasterize(src(Icon::Collection), px, px)
                .unwrap_or_else(|| panic!("Collection failed to rasterize at {px}px"));
            let alpha = |x: i32, y: i32| rgba[((y * px + x) * 4 + 3) as usize];
            let max_alpha = (0..px).flat_map(|y| (0..px).map(move |x| alpha(x, y))).max().unwrap();
            assert_eq!(max_alpha, 255, "Collection never reaches full opacity at {px}px");
            for i in 0..px {
                for (x, y) in [(i, 0), (i, px - 1), (0, i), (px - 1, i)] {
                    assert_eq!(alpha(x, y), 0, "Collection has ink on the border at {px}px ({x},{y})");
                }
            }
        }
    }

    /// **Every size a `glyph_ceiling` shrink (the Collection header's) can actually land the page glyph at
    /// rasterizes 1:1 too**, not just the natural 112px — `StatusOverlay::glyph_rect` scales
    /// continuously between `GLYPH_MIN_SIZE` (56, below which it draws nothing) and `GLYPH_SIZE`
    /// (112), and every value in that range must clear `MAX_ICON_PX` unclamped or a shrunk glyph
    /// goes soft exactly where the ceiling put it. One representative mark (rasterizing all
    /// twelve at every size would be redundant with the test above, which already grades all
    /// twelve at 112) is enough to catch a clamp regression at the sizes that matter.
    #[test]
    fn the_glyph_shrink_range_rasterizes_1to1_through_the_production_clamp() {
        for size in [56, 64, 76, 88, 96, 104, 112] {
            assert_eq!(
                icon_raster_px(size),
                size,
                "size {size}px (inside the glyph's shrink range) was clamped — MAX_ICON_PX must cover it"
            );
            let rgba = plx_gfx::svg::rasterize(src(Icon::WifiSlash), size, size)
                .unwrap_or_else(|| panic!("WifiSlash failed to rasterize at {size}px"));
            let alpha = |x: i32, y: i32| rgba[((y * size + x) * 4 + 3) as usize];
            let max_alpha = (0..size).flat_map(|y| (0..size).map(move |x| alpha(x, y))).max().unwrap();
            assert_eq!(max_alpha, 255, "WifiSlash never reaches full opacity at {size}px");
        }
    }

    /// The `d` attribute's coordinates as `(x, y)` pairs. The Watchlist assets are baked absolute
    /// `M`/`z` paths of bare coordinate pairs, so splitting on letters and whitespace is exact.
    fn path_points(svg: &str) -> Vec<(f32, f32)> {
        let start = svg.find(" d=\"").expect("asset has a path") + 4;
        let d = &svg[start..start + svg[start..].find('"').unwrap()];
        let nums: Vec<f32> = d
            .split(|c: char| c.is_ascii_alphabetic() || c.is_whitespace())
            .filter(|t| !t.is_empty())
            .map(|t| t.parse().unwrap())
            .collect();
        assert_eq!(nums.len() % 2, 0, "path is not coordinate pairs");
        nums.chunks(2).map(|p| (p[0], p[1])).collect()
    }

    /// The five Watchlist marks rasterize at the two sizes the Watchlist draws them at (the 28px
    /// control-size mark and the 112px read-out) through the production clamp: full opacity reached,
    /// no ink on the border. Same two checks the read-out family is graded by.
    #[test]
    fn the_watchlist_marks_rasterize_clean_at_28_and_112() {
        for id in [Icon::Bookmark, Icon::BookmarkFill, Icon::Clock, Icon::ServerSlash, Icon::CircleSlash] {
            for px in [28, 112] {
                let px = icon_raster_px(px);
                let rgba = plx_gfx::svg::rasterize(src(id), px, px)
                    .unwrap_or_else(|| panic!("{id:?} failed to rasterize at {px}px"));
                assert_eq!(rgba.len(), (px * px * 4) as usize, "{id:?} wrong buffer size");
                let alpha = |x: i32, y: i32| rgba[((y * px + x) * 4 + 3) as usize];
                let max_alpha =
                    (0..px).flat_map(|y| (0..px).map(move |x| alpha(x, y))).max().unwrap();
                assert_eq!(max_alpha, 255, "{id:?} never reaches full opacity at {px}px");
                for i in 0..px {
                    for (x, y) in [(i, 0), (i, px - 1), (0, i), (px - 1, i)] {
                        assert_eq!(alpha(x, y), 0, "{id:?} has ink on the border at {px}px ({x},{y})");
                    }
                }
            }
        }
    }

    /// The Watchlist marks' `ink_x` and `band` entries are the bounds of their own assets, not
    /// transcriptions of them: the same drift guard `the_crumb_chevron_is_the_row_chevron_mirrored`
    /// gives the chevron, measured from the path data this time.
    #[test]
    fn the_watchlist_marks_ink_entries_match_their_assets() {
        for id in [Icon::Bookmark, Icon::BookmarkFill, Icon::Clock, Icon::ServerSlash, Icon::CircleSlash] {
            let pts = path_points(src(id));
            let xs = pts.iter().map(|p| p.0);
            let ys = pts.iter().map(|p| p.1);
            let (min_x, max_x) = (xs.clone().fold(f32::MAX, f32::min), xs.fold(f32::MIN, f32::max));
            let (min_y, max_y) = (ys.clone().fold(f32::MAX, f32::min), ys.fold(f32::MIN, f32::max));
            let (l, r) = ink_x(id);
            assert!((l - min_x / 24.0).abs() < 0.005, "{id:?} ink left {l} vs asset {}", min_x / 24.0);
            assert!((r - max_x / 24.0).abs() < 0.005, "{id:?} ink right {r} vs asset {}", max_x / 24.0);
            let height = (max_y - min_y) / 24.0;
            assert!((band(id) - height).abs() < 0.005, "{id:?} band {} vs asset {height}", band(id));
        }
    }
}
