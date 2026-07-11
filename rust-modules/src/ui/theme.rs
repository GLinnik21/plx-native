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
/// Flat shelf/app base (both gradient stops).
pub const SURFACE_APP: [f32; 4] = [0.10, 0.10, 0.115, 1.0];
/// GL clear color — 3-float (`frame_clear` takes r,g,b, no alpha). Overdrawn by [`SURFACE_APP`].
pub const CLEAR_RGB: (f32, f32, f32) = (0.03, 0.03, 0.045);
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

// ── Rails / overlays / accents ───────────────────────────────────────────────
/// Unfilled progress/scrubber track.
pub const RAIL_TRACK: [f32; 4] = [1.0, 1.0, 1.0, 0.20];
/// Buffered-ahead / resume-track band.
pub const RAIL_BUFFERED: [f32; 4] = [1.0, 1.0, 1.0, 0.28];
/// Played/filled portion of a rail.
pub const RAIL_FILL: [f32; 4] = [1.0, 1.0, 1.0, 0.95];
/// Warm amber Continue-Watching progress fill (Plex-specific; no player equivalent).
pub const RESUME_FILL: [f32; 4] = [0.98, 0.72, 0.18, 0.95];
/// Section divider hairline.
pub const HAIRLINE: [f32; 4] = [1.0, 1.0, 1.0, 0.10];
/// Faint focus pill (pre-`TabPill`-adoption tab highlight).
pub const OVERLAY_FOCUS_PILL: [f32; 4] = [1.0, 1.0, 1.0, 0.14];
/// Softer selection panel.
pub const OVERLAY_FOCUS_SOFT: [f32; 4] = [1.0, 1.0, 1.0, 0.07];
/// Outlined-badge / meta-badge border.
pub const OVERLAY_BORDER: [f32; 4] = [1.0, 1.0, 1.0, 0.55];
/// No-op texture tint (structural: draw an RGBA texture unmodified).
pub const TINT_WHITE: [f32; 4] = [1.0, 1.0, 1.0, 1.0];

// ── Focus-ring geometry (the ring *color* is shader-baked in gfx.rs; only geometry is tunable) ──
// The hero-grid card's wide glow uses `consts::GLOW_PAD` (shared with off-screen culling, so it has
// one home there); the tile/strip cards (shelf / related / episode / chapters) use these:
/// Strip/tile card focus-ring inflation (tight ring around a shelf/related/episode tile).
pub const CARD_RING_PAD_STRIP: f32 = 6.0;
/// Focus-ring corner radius.
pub const CARD_RING_RAD: f32 = 14.0;
