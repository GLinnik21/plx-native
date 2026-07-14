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
/// Secondary text: metadata lines, synopsis, idle row titles.
pub const TEXT_SECONDARY: [f32; 4] = [0.72, 0.75, 0.80, 1.0];
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

// ── Accent / control ─────────────────────────────────────────────────────────
/// The mockup's "Snow": warm off-white focus fill (#e9e6e0). Focused controls fill this.
pub const ACCENT: [f32; 4] = [0.914, 0.902, 0.878, 1.0];
/// Near-black ink/glyphs drawn over [`ACCENT`].
pub const ACCENT_INK: [f32; 4] = [0.03, 0.03, 0.04, 1.0];
/// Alias for [`ACCENT_INK`] read from the "ink over accent" intent.
pub const INK_ON_ACCENT: [f32; 4] = ACCENT_INK;
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
/// Warm amber Continue-Watching progress fill (Plex-specific; no player equivalent).
pub const RESUME_FILL: [f32; 4] = [0.98, 0.72, 0.18, 0.95];
/// Error/destructive signal ink — the wrong-PIN dot flash. Desaturated toward the palette's
/// warm neutrals so it reads as a state, not an alarm.
pub const DANGER: [f32; 4] = [0.92, 0.32, 0.29, 1.0];
/// Section divider hairline.
pub const HAIRLINE: [f32; 4] = [1.0, 1.0, 1.0, 0.10];
/// Faint focus pill (pre-`TabPill`-adoption tab highlight).
pub const OVERLAY_FOCUS_PILL: [f32; 4] = [1.0, 1.0, 1.0, 0.14];
/// Softer selection panel.
pub const OVERLAY_FOCUS_SOFT: [f32; 4] = [1.0, 1.0, 1.0, 0.07];
/// Outlined-badge / meta-badge border.
pub const OVERLAY_BORDER: [f32; 4] = [1.0, 1.0, 1.0, 0.55];
/// Filled metadata chip (the About column's CC/SDH/AD accessibility badges).
pub const BADGE_FILL: [f32; 4] = [0.86, 0.88, 0.92, 0.20];
/// No-op texture tint (structural: draw an RGBA texture unmodified).
pub const TINT_WHITE: [f32; 4] = [1.0, 1.0, 1.0, 1.0];

// ── Focus-ring geometry (the ring *color* is shader-baked in gfx.rs; only geometry is tunable) ──
// The hero-grid card's wide glow uses `consts::GLOW_PAD` (shared with off-screen culling, so it has
// one home there); the tile/strip cards (shelf / related / episode / chapters) use these:
/// Strip/tile card focus-ring inflation (tight ring around a shelf/related/episode tile).
pub const CARD_RING_PAD_STRIP: f32 = 6.0;
/// Focus-ring corner radius.
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
