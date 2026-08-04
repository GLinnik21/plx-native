//! `theme` — the single palette for the whole UI.
//!
//! Every color the UI paints comes from a named token here. The values are the player screen's
//! literals promoted to canonical (it is the one screen with a coherent design language); the
//! two *lossy* collapses (several near-white primaries → [`TEXT_PRIMARY`]; several dim greys →
//! [`TEXT_SECONDARY`]/[`TEXT_TERTIARY`]) are deliberate and only visible on-device — see
//! `docs/ui-system-migration.md` §A. Colors are `[f32; 4]` (r,g,b,a); scrims supply their alpha
//! per call via [`scrim`]/[`scrim_black`].
//!
//! `ACCENT`/`ACCENT_INK` live here and are re-exported from `ui` (`mod.rs`) so the existing
//! `crate::ui::ACCENT` call sites keep compiling unchanged.
#![allow(dead_code)]

// ── Text ────────────────────────────────────────────────────────────────────
/// Primary reading text / high-emphasis title. Collapses the 3-4 near-whites.
pub const TEXT_PRIMARY: [f32; 4] = [0.97, 0.98, 0.99, 1.0];
/// Section headings ("Related", "Cast & Crew", About headings) — a touch below primary.
pub const TEXT_HEADING: [f32; 4] = [0.92, 0.94, 0.97, 1.0];
/// Secondary text: metadata lines, idle row titles.
pub const TEXT_SECONDARY: [f32; 4] = [0.72, 0.75, 0.80, 1.0];
/// **Reading copy** — a multi-line paragraph someone actually reads through, one step brighter than
/// the label grey above it (`#cdd3dd`, `Details Screen.dc.html`). Its one job today is the detail
/// hero's synopsis, which is the longest run of text on the page and sits over the backdrop scrim;
/// at [`TEXT_SECONDARY`] it read as fine print rather than as the blurb. A one-line metadata VALUE
/// stays secondary — the distinction is paragraph-vs-label, not importance.
pub const TEXT_READING: [f32; 4] = [0.804, 0.827, 0.867, 1.0];
/// Tertiary text: runtime, kickers, inactive tabs, About labels, dim/empty states.
pub const TEXT_TERTIARY: [f32; 4] = [0.58, 0.60, 0.64, 1.0];

// ── Type scale ───────────────────────────────────────────────────────────────
/// The one legibility-tuned ladder of text sizes for the whole UI — the *size* axis of the design
/// system (colours above, focus geometry below). Authored for a 1920×1080 panel viewed from a
/// couch, so [`size::CAPTION`] (24) is a **hard floor**: nothing in the product chrome renders
/// smaller, because sub-24 text is unreadable at that distance (the old raw 17/18/19/20/21 sizes
/// were exactly what read badly). Pass these to `Painter::text` / `Label` / `TextView` /
/// `text::elide` in place of a raw integer — a size is a *role*, not a magic number (mirrors the
/// "never a raw colour literal" rule). Rungs step ~1.15–1.25×; a role picks the nearest rung.
///
/// Two deliberate carve-outs sit outside the ladder (documented at their call site, not raw
/// literals): the player-HUD now-playing **display title** (larger than [`size::TITLE`]) and the
/// client-rendered **subtitle** caption — both media chrome with their own legibility contract, and
/// both already well above the floor. The boot splash (`anim.rs`) is likewise its own one-off.
///
/// **Rasterization contract:** the render path keeps every rung's stroke weights design-true
/// (light hinting in `text.rs::font_at`, pixel-snapped 1:1 quads via `gfx::snap`), so rung values
/// are chosen for hierarchy and legibility ONLY — no rung needs to dodge px sizes that hint badly.
/// After swapping fonts or touching hinting, re-verify with `tools/font-hint-audit.py`.
///
/// **A size cannot be animated — CROSSFADE two rungs instead.** Glyphs are cached per (size, bold)
/// as rasterized textures, so tweening a point size would rasterize a fresh run every frame and
/// churn that cache. A title that has to change size does it as two `Label` draws whose alphas run
/// opposite on one 0..1 progress: `detail.rs`'s hero → compact title (HERO → TITLE, on the scroll)
/// and `person.rs`'s band condense (the same pair, on focus) are both spelled that way.
pub mod size {
    use std::os::raw::c_int;
    /// Full-bleed hero title — the home + detail hero headline.
    pub const HERO: c_int = 72;
    /// Screen / panel title — the in-player Info card title, the scrolled compact title, About column heads.
    pub const TITLE: c_int = 40;
    /// Section headers ("Related", "Cast & Crew") + list-row titles.
    pub const HEADLINE: c_int = 32;
    /// Default reading text (synopsis, hero meta, About paragraphs, empty states) + control labels (Play / action buttons, tabs, pills — one rung under a header).
    pub const BODY: c_int = 28;
    /// Secondary labels — card / episode / chapter titles, cast names, list detail, meta chips.
    pub const LABEL: c_int = 26;
    /// Couch legibility **FLOOR** for reading text — kickers, timecodes, cast roles, badges, field
    /// labels. Nothing that must be *read* renders smaller.
    pub const CAPTION: c_int = 24;
    /// Fine print — deliberately below the couch floor (explicit design direction, 2026-07-12,
    /// re-tuned on-device 16 → 20 → 22: "bigger, but smaller than the meta line"): the small
    /// info/synopsis text under the home/detail hero's title block, and the episode row's air
    /// date. De-emphasis is the point — atmosphere copy beside an outsized title, not content
    /// someone must read; labels the eye needs to catch (the SxEy kicker, meta lines) stay on
    /// the regular rungs.
    pub const MICRO: c_int = 22;
}

/// The **spacing scale** — the vertical/horizontal *gap* axis of the design system, the sibling of
/// [`size`]. Gaps between stacked elements come from a named rung, never a hand-tuned pixel offset,
/// so vertical rhythm stays consistent instead of drifting per-screen (the same reason colours and
/// sizes are tokenised). Rungs step ~1.6×; a gap picks the nearest rung by role. Positions still
/// flow off *measured* element heights — a rung is the gap *between* blocks, not an absolute Y.
pub mod space {
    /// Hairline gap — an icon and its label, chips in a row.
    pub const XS: f32 = 8.0;
    /// Tight — closely related lines (a meta line under its title).
    pub const SM: f32 = 16.0;
    /// Default — a heading and its body text, label→value pairs.
    pub const MD: f32 = 24.0;
    /// Block gap — one content block to the next (synopsis → action row).
    pub const LG: f32 = 40.0;
    /// Major gap — separates whole regions (a title band from the metadata beneath it).
    pub const XL: f32 = 64.0;
}

/// The **logo presence ladder** — how big a clearLogo is DRAWN. The third size axis of the design
/// system, beside [`size`] (type) and [`space`] (gaps), and it exists for the same reason: a logo
/// used to be sized by a per-screen literal (96 on the home hero, 120 on the detail hero, 54 on the
/// scrolled compact title), so the SAME mark was three sizes in one app.
///
/// **The rule is constant AREA, not constant height** ([`crate::ui::hero_logo::fit`]). A clearLogo's
/// aspect runs from ~1:1 (an emblem) to ~10:1 (a long wordmark); under a height clamp the drawn area
/// is LINEAR in aspect, so a 5:1 wordmark covered five times the ink of a 1:1 emblem — which is
/// exactly why square logos read as an afterthought. Solving for a target AREA instead makes a
/// square grow TALL and a wordmark grow WIDE, so both carry the same weight. The floor then stops a
/// very wide mark thinning to a pinstripe, the ceiling stops a taller-than-wide asset swallowing the
/// hero, and the caller's column contains the result last.
pub mod logo {
    /// HERO rung — the home hero's title band AND the detail hero's title band (one mark on two
    /// screens, one value; the user directive is "mutual logic for logo size for all hero"). The
    /// target ink in px², anchored on the shape this rule must NOT move: a 5:1 wordmark at the
    /// height floor, i.e. 600×120 — the detail hero's on-device-tuned size today. Every other
    /// aspect is solved from it, so the commonest logo shape is unchanged and only squarer ones grow.
    pub const HERO_AREA: f32 = 72_000.0;
    /// The shortest a hero logo is ever drawn, and the height everything ~5:1 and wider lands on.
    /// The detail hero's tuned 120; home's old 96 is retired, because two heroes drawing the same
    /// clearLogo must draw it the same size. Deliberately a step ABOVE one line of [`super::size::HERO`]
    /// text (72 × 1.32 ≈ 95): the logo is the brand mark and outranks the text title it stands in
    /// for. ALSO the LAYOUT height of the title band ([`crate::ui::hero_logo::band_h`]) — a taller
    /// logo spills upward as paint, never as layout.
    pub const HERO_H_MIN: f32 = 120.0;
    /// The tallest. = √[`HERO_AREA`], i.e. precisely where the area rule lands a 1:1 logo — so the
    /// ceiling never touches a real wordmark and bites only on a taller-than-wide asset. This is the
    /// ONE knob to pull down (to ~200) if a device capture says a square emblem shouts or crowds the
    /// detail page's compact title mid-scroll; do not reach for the area, which is what keeps
    /// wordmarks where they are.
    pub const HERO_H_MAX: f32 = 268.0;
    /// COMPACT rung — the pinned title the detail page scrolls up to. The hero rung scaled by
    /// ([`COMPACT_H_MIN`]/[`HERO_H_MIN`])² = 0.45², so the two rungs cannot drift apart: a 5:1
    /// wordmark lands on the floor at BOTH rungs, by construction.
    pub const COMPACT_AREA: f32 = 14_580.0;
    /// One line of [`super::size::TITLE`] text (40 × 1.32 ≈ 53) — the compact title's own text
    /// fallback, which is what this rung stands in for. Unchanged from the literal 54 it replaces,
    /// so a wordmark in the pinned strip is pixel-identical to today.
    pub const COMPACT_H_MIN: f32 = 54.0;
    /// Set by the BAND, not by the area rule (which would want 121 here): the pinned strip has to
    /// clear the screen top and stop short of `detail::TOP_MARGIN` (120), where the first scrolled
    /// section lifts to. 72 leaves 22px of head room above the logo and 26px below it.
    pub const COMPACT_H_MAX: f32 = 72.0;
}

// ── Accent / control ─────────────────────────────────────────────────────────
/// The mockup's "Snow": warm off-white focus fill (#e9e6e0). Focused controls fill this.
pub const ACCENT: [f32; 4] = [0.914, 0.902, 0.878, 1.0];
/// Near-black ink/glyphs drawn over [`ACCENT`].
pub const ACCENT_INK: [f32; 4] = [0.03, 0.03, 0.04, 1.0];
/// Alias for [`ACCENT_INK`] read from the "ink over accent" intent.
pub const INK_ON_ACCENT: [f32; 4] = ACCENT_INK;
/// The *spent* portion of a control that is counting down ([`Button::progress`](crate::ui::widgets::Button::progress)).
/// A dimmed [`ACCENT`], NOT a different hue: the control stays focused-looking end to end, so the
/// sweep reads as time passing rather than as the focus state changing. Deliberately close enough
/// to `ACCENT` that [`ACCENT_INK`] stays legible on BOTH sides — the first version flipped the ink
/// at the sweep line and cut the label in half mid-word.
pub const CONTROL_SPENT_FILL: [f32; 4] = [0.749, 0.739, 0.720, 1.0];

/// Idle (unfocused) control disc/pill fill — solid dark, faintly translucent.
pub const CONTROL_IDLE_FILL: [f32; 4] = [0.145, 0.145, 0.153, 0.92];
/// White glyph/label over an idle control.
pub const CONTROL_IDLE_INK: [f32; 4] = [1.0, 1.0, 1.0, 1.0];
/// Primary-CTA cool-white *fill* (a control surface, not text) — e.g. the hero Play pill.
pub const FILL_PRIMARY: [f32; 4] = [0.97, 0.98, 0.99, 1.0];
/// Near-black ink over [`FILL_PRIMARY`].
pub const INK_ON_PRIMARY: [f32; 4] = [0.05, 0.06, 0.08, 1.0];

// ── Surfaces / backgrounds ───────────────────────────────────────────────────
/// Flat shelf/app base (both gradient stops) — Apple TV's shelf gray **#2C2C2E (44,44,46)**. A
/// MEDIUM gray, deliberately not near-black: the focused-card drop-shadow only reads against a base
/// this light (on the old near-black 25,25,29 a black shadow had almost nothing to darken). Snapped
/// to exact 8-bit codes (44/44/46 are all integers) so `GL_DITHER` — on by default in GLES2 — has no
/// half-code to alternate on across this large flat fill (a half-code banded the old value visibly).
pub const SURFACE_APP: [f32; 4] = [44.0 / 255.0, 44.0 / 255.0, 46.0 / 255.0, 1.0];
/// GL clear color — 3-float (`frame_clear` takes r,g,b, no alpha). The app's DEFAULT base: the same
/// `#2C2C2E` shelf gray as [`SURFACE_APP`], so every non-player screen clears to the Apple-TV gray
/// (the player clears transparent separately). Home overdraws it with `SURFACE_APP` (identical gray).
pub const CLEAR_RGB: (f32, f32, f32) = (44.0 / 255.0, 44.0 / 255.0, 46.0 / 255.0);
/// Opaque menu panel / fade mask / badge knockout interior.
pub const SURFACE_PANEL: [f32; 4] = [0.133, 0.133, 0.141, 1.0];
/// Near-opaque sheet/card gradient — top stop.
pub const PANEL_TOP: [f32; 4] = [0.129, 0.129, 0.137, 0.985];
/// Near-opaque sheet gradient — bottom stop (kept distinct; the gradient is deliberate).
pub const PANEL_BOT: [f32; 4] = [0.106, 0.106, 0.114, 0.985];
/// Poster/thumb skeleton flat placeholder.
pub const CARD_PLACEHOLDER: [f32; 4] = [0.12, 0.13, 0.16, 1.0];
/// Loading-skeleton gradient — top.
pub const SKELETON_TOP: [f32; 4] = [0.13, 0.14, 0.17, 1.0];
/// Loading-skeleton gradient — bottom.
pub const SKELETON_BOT: [f32; 4] = [0.08, 0.09, 0.11, 1.0];

// ── Scrims (near-black; alpha supplied per call) ─────────────────────────────
/// Hero/scroll scrim ink; use via [`scrim`].
pub const SCRIM_INK: [f32; 3] = [0.02, 0.02, 0.03];
/// Pure-black scrim ink (HUD bottom, subtitle outline, modal); use via [`scrim_black`].
pub const SCRIM_BLACK_INK: [f32; 3] = [0.0, 0.0, 0.0];

/// The **text-legibility floor** for copy drawn straight onto ARTWORK: the scrim alpha at which
/// [`TEXT_PRIMARY`]/[`TEXT_SECONDARY`] hold ≥4.5:1 over a bright backdrop (an encoded 0.85
/// highlight, brighter than essentially any fanart pixel) and ≥3:1 over a blown-white one.
///
/// MEASURED, not chosen. The panel is plain 888 with no sRGB framebuffer anywhere in the tree, so a
/// token value IS an sRGB code and GL's blend is a straight lerp in that space — which makes the
/// whole derivation arithmetic. It lives on [`crate::ui::widgets::hero_scrim`] and is asserted by
/// that component's anchor table, so brightening a text token and re-deriving this stay one edit.
/// The binding case is a `TEXT_SECONDARY` BODY line where the hero's bottom-up ramp supplies only
/// ~0.29: it needs 0.655 total, so the wedge must carry ~0.51 there and rather more at the title.
///
/// The same weight `draw_tab_row`'s track capsule already uses for the same job over the same
/// artwork — one value per role, arrived at independently twice. An alpha rather than a colour,
/// like [`CARD_SHADOW_REST_A`]: the ink is [`SCRIM_INK`], only the weight is the decision. **This is
/// the one knob** if the treatment reads heavy — 0.60 still clears 3:1 everywhere.
pub const SCRIM_TEXT_A: f32 = 0.72;

/// Near-black scrim at alpha `a` — hero/scroll dimming.
pub const fn scrim(a: f32) -> [f32; 4] {
    [SCRIM_INK[0], SCRIM_INK[1], SCRIM_INK[2], a]
}
/// Pure-black scrim at alpha `a` — HUD/modal dimming.
pub const fn scrim_black(a: f32) -> [f32; 4] {
    [SCRIM_BLACK_INK[0], SCRIM_BLACK_INK[1], SCRIM_BLACK_INK[2], a]
}
/// Splat a token's rgb with an overridden alpha (e.g. the `env.sp`-baked hub title).
pub const fn with_a(c: [f32; 4], a: f32) -> [f32; 4] {
    [c[0], c[1], c[2], a]
}
/// Blend `a` toward `b` by `t` (rgb only; keeps `a`'s alpha) — for a token that is a *mix* of two
/// roles rather than one of them, e.g. an ambient wash sitting `t` of the way from [`SURFACE_APP`]
/// to an item's artwork colour. A screen that lerps channels in a loop wants this instead.
pub fn mix(a: [f32; 4], b: [f32; 4], t: f32) -> [f32; 4] {
    [a[0] + (b[0] - a[0]) * t, a[1] + (b[1] - a[1]) * t, a[2] + (b[2] - a[2]) * t, a[3]]
}
/// Scale a token's rgb by `k` toward black, keeping its alpha — e.g. folding a scroll-darken into a
/// layer tint. Pair with [`with_a`] when the alpha also changes.
pub const fn dim(c: [f32; 4], k: f32) -> [f32; 4] {
    [c[0] * k, c[1] * k, c[2] * k, c[3]]
}

// ── Rails / overlays / accents ───────────────────────────────────────────────
/// Unfilled progress/scrubber track.
pub const RAIL_TRACK: [f32; 4] = [1.0, 1.0, 1.0, 0.20];
/// Buffered-ahead / resume-track band.
pub const RAIL_BUFFERED: [f32; 4] = [1.0, 1.0, 1.0, 0.28];
/// Played/filled portion of a rail.
pub const RAIL_FILL: [f32; 4] = [1.0, 1.0, 1.0, 0.95];
/// An intro/credits SEGMENT on the player scrubber — a band lit a step above the track, drawn
/// between the track and the fill so the played part of a segment reads as played. Stays inside the
/// rail's monochrome language on purpose: a hue here would compete with the amber resume fill for
/// "this bit is special", and the rail sits straight over moving video.
// (RAIL_MARKER, white @ 0.42, was the intro/credits band on the scrubber. Removed 2026-08-04 with
// the band itself — at twice the track's opacity it read as a rendering artifact rather than as
// information. See the "the rail carries NO marks" note in ui/player_hud.rs.)
/// The **ambient wash's** resting tint — the faint warm cast a page carries when no artwork is
/// keying it (the person page's header state, `Person Screen v2.dc.html`'s
/// `rgba(233,230,224,.10)`). Its own token rather than [`ACCENT`], which it is a hair from: ACCENT
/// is a *focus fill* ("focused controls fill this"), and borrowing it here would mean retuning the
/// focus colour silently restyles a page background — one value per role.
pub const WASH_WARM: [f32; 4] = [0.914, 0.902, 0.878, 1.0];
/// Warm amber Continue-Watching progress fill (Plex-specific; no player equivalent). `#fab82e`.
pub const RESUME_FILL: [f32; 4] = [0.98, 0.72, 0.18, 0.95];
/// Unfilled track behind the Continue-Watching resume bar — the full-bleed card-bottom rail. A
/// hair lighter than the player scrubber's so the amber reads against a bright poster.
pub const RESUME_TRACK: [f32; 4] = [1.0, 1.0, 1.0, 0.22];
/// Ink over [`RESUME_FILL`] when the amber is a SURFACE rather than a mark — the dark check knocked
/// out of the episode still's watched disc (`Details Screen.dc.html`'s `#141416`). Distinct from
/// [`ACCENT_INK`]: that one is tuned against the near-white [`ACCENT`], and at that value it
/// disappeared into the amber rather than reading as a knockout.
pub const INK_ON_RESUME: [f32; 4] = [0.078, 0.078, 0.086, 1.0];
/// Error/destructive signal ink — the wrong-PIN dot flash. Desaturated toward the palette's
/// warm neutrals so it reads as a state, not an alarm.
pub const DANGER: [f32; 4] = [0.92, 0.32, 0.29, 1.0];
/// Section divider hairline.
pub const HAIRLINE: [f32; 4] = [1.0, 1.0, 1.0, 0.10];
/// Faint focus pill (pre-`TabPill`-adoption tab highlight).
pub const OVERLAY_FOCUS_PILL: [f32; 4] = [1.0, 1.0, 1.0, 0.14];
/// A PLATED tab segment's fill — the detail page's season tabs, which sit bare on the backdrop rather
/// than inside the top bar's track and so provide their own ground
/// ([`crate::ui::widgets::TabPill::plated`], `Details Screen.dc.html`). The selected one is a step up
/// from [`OVERLAY_FOCUS_PILL`]: at .14 against a plated neighbour at .08 the two read as the same pill.
pub const TAB_PLATE_SELECTED: [f32; 4] = [1.0, 1.0, 1.0, 0.20];
/// The selected-segment plate as a **travelling capsule** over a plated strip
/// ([`crate::ui::widgets::TabStrip`]). It is deliberately not [`TAB_PLATE_SELECTED`]: that value
/// assumed the selected plate REPLACED the idle one, whereas a capsule *slides over* the idle plates
/// that stay put underneath it — and .20 over .08 composites to .26, a selected season visibly
/// heavier than it was. 0.13 over [`TAB_PLATE_IDLE`] lands back on .20 (`a + b − ab` = .1996), and
/// because both layers are the same white that arithmetic is order-independent, so it holds whether
/// the plate is painted under or over the capsule.
///
/// The top tab row needs no such value: its pills sit bare inside the tab-bar track (which already
/// is their ground) and its selection capsule uses [`OVERLAY_FOCUS_PILL`] unchanged.
pub const TAB_PLATE_SELECTED_OVER: [f32; 4] = [1.0, 1.0, 1.0, 0.13];
/// An unselected plated segment — present, but only just: it says "this is a control" and nothing more.
pub const TAB_PLATE_IDLE: [f32; 4] = [1.0, 1.0, 1.0, 0.08];
/// Softer selection panel.
pub const OVERLAY_FOCUS_SOFT: [f32; 4] = [1.0, 1.0, 1.0, 0.07];
/// Outlined-badge / meta-badge border.
pub const OVERLAY_BORDER: [f32; 4] = [1.0, 1.0, 1.0, 0.55];
/// Filled metadata chip (the About column's CC/SDH/AD accessibility badges).
pub const BADGE_FILL: [f32; 4] = [0.86, 0.88, 0.92, 0.20];
/// No-op texture tint (structural: draw an RGBA texture unmodified).
pub const TINT_WHITE: [f32; 4] = [1.0, 1.0, 1.0, 1.0];

// ── Review-score marks (detail hero ratings row) ─────────────────────────────
// These used to be the ONE place the palette borrowed someone else's colour, justified by the
// badge being a BRAND MARK. That justification is gone with the marks: Rotten Tomatoes' fruit,
// seal and popcorn tub were removed 2026-08-02 (no licensing route exists, and a redraw is the
// infringement pattern, not a defence), and every provider is NAMED in text instead. So these are
// now our own semantic colours for a VERDICT, and only two still sit near a brand's hue because a
// ripe tomato that isn't red has stopped being a tomato.
//
// They tint an icon MASK only — never text, never a surface — so they cannot leak into the rest of
// the UI. The row's captions and scores use the ordinary TEXT_* tokens, which is the point: the
// only colour left in the row is the verdict.
/// The ripe tomato's body — `#f5341a`.
pub const RATING_FRESH: [f32; 4] = [0.961, 0.204, 0.102, 1.0];
/// The Certified body — `#f0b429`. The SAME fruit struck in gold rather than a separate seal:
/// a rarer bar reads as a richer version of the thing, and a seal was the one shape here that was
/// unmistakably somebody's award rather than a piece of fruit.
pub const RATING_CERTIFIED: [f32; 4] = [0.941, 0.706, 0.161, 1.0];
/// The audience crowd when the verdict is good — `#3ec96b`. Note the polarity across the row is
/// deliberately NOT uniform: critics go red-for-good (a ripe tomato), audience goes green-for-good
/// (a healthy crowd). Each mark is read against itself, not against its neighbour.
pub const RATING_AUDIENCE: [f32; 4] = [0.243, 0.788, 0.420, 1.0];
/// The **calyx** green — the tomato's leaf-and-stem, on a fresh or certified body. `#2fae5b`, and
/// deliberately not [`RATING_AUDIENCE`]: they are two marks' greens that land close, and folding
/// them would let an audience tweak silently restyle a leaf.
pub const RATING_LEAF: [f32; 4] = [0.184, 0.682, 0.357, 1.0];
/// The drained state, for both a hollow tomato and a negated crowd — an ALIAS of
/// [`TEXT_TERTIARY`], not a copy, because the negation is literally "this has the weight of a
/// caption now". An alias so the two can never drift apart.
pub const RATING_MUTED: [f32; 4] = TEXT_TERTIARY;

// ── Card-glow geometry (the glow *color* is shader-baked in gfx.rs's FS_SRC/FS_IMG; only geometry
// is tunable). The hero-grid card's wide glow pad is `consts::GLOW_PAD` (shared with off-screen
// culling, so it has one home there).
/// Card corner radius (tiles/shelves — `CardRow`'s rounded rect + its baked focus glow follow it).
pub const CARD_RING_RAD: f32 = 14.0;

// ── Card treatment (Home Screen.dc.html): every tile = a soft drop shadow that GROWS with the focus
// pop + a 1px perimeter edge-sheen, both FOLDED into the tile's own draw pass (FS_IMG for textured
// tiles, FS_SRC for skeleton/chip fills), replacing the old glow ring. Applies to circles too. ──
// The edge-sheen is a single thin rounded-rect stroke flush around the whole perimeter (following the
// corner radius), CONSTANT strength on every tile — NOT a gloss/wash over the card face.
/// The 1px perimeter edge-highlight on a tile (white, faintly translucent).
pub const CARD_SHEEN: [f32; 4] = [1.0, 1.0, 1.0, 0.22];
/// Stroke width (px) of the perimeter edge-highlight.
pub const CARD_SHEEN_W: f32 = 1.0;
/// Drop-shadow ink under a raised card — pure black (the design's `rgba(0,0,0,…)`), alpha supplied per
/// call (× the resting→lifted focus ramp). Only the alpha/rgb matter for the folded card shadow (the
/// rgb is used by the chip's standalone shadow); its own token (not `scrim_black`) so it can be tuned alone.
pub const CARD_SHADOW: [f32; 4] = [0.0, 0.0, 0.0, 0.40];
/// Focused (LIFTED) card drop-shadow penumbra (px) and downward offset (px) — the CAPS on the
/// tile-scaled values. Kept subtle: on the `SURFACE_APP` gray shelf a tight, close shadow already
/// reads, so this is a gentle lift, not the design's oversized `0 30px 70px` pool.
pub const CARD_SHADOW_BLUR: f32 = 30.0;
pub const CARD_SHADOW_DY: f32 = 12.0;
/// RESTING (unfocused) drop-shadow caps — every tile carries a small, tight shadow so it sits CLOSE
/// to the shelf; on focus the blur/offset/alpha lerp UP to the lifted values above, so the tile reads
/// as rising off the background. Caps on the tile-scaled resting values.
pub const CARD_SHADOW_REST_BLUR: f32 = 11.0;
pub const CARD_SHADOW_REST_DY: f32 = 4.0;
/// Resting shadow ink alpha (unfocused); lerps up to [`CARD_SHADOW`]'s alpha on focus.
pub const CARD_SHADOW_REST_A: f32 = 0.34;
