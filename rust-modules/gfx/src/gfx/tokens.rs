//! The design tokens the renderer itself draws with: the 8-bit colour constructor and the two palette
//! stops `gfx` paints with, the card shading constants its image shader mirrors, and the type-size
//! ladder `text` warms its faces from.
//!
//! They lived in `ui/theme.rs` and moved here (module-layers step L5) because `gfx` and `text` read
//! them and the `gfx` layer may not name `ui`. `ui::theme` imports `rgb8` and the two stops for its
//! private palette and re-exports everything else (`theme::CLEAR_RGB`, `theme::size::BODY`,
//! `theme::with_a`, `theme::CARD_GLOW_A`, …), so every screen still names the token in the place
//! `ui/CLAUDE.md` tells it to look: `theme.rs` stays the one palette, and a token's VALUE is written
//! down once, here.
//!
//! `tools/font-hint-audit.py` parses `pub mod size` out of THIS file.
// The same blanket `ui/theme.rs` carries: a token is added with its role and some rungs/constants
// are read only through the `ui::theme` re-export (or only by a test), which is not dead code.
#![allow(dead_code)]

/// An 8-bit sRGB code as an opaque token value. `rgb8(0x2c, 0x2c, 0x2e)` is `#2c2c2e`.
///
/// Every palette stop is an EXACT 8-bit code, which is load-bearing rather than tidy: the panel is
/// plain 888 with no sRGB framebuffer anywhere in the tree, so a token value IS an sRGB code — and a
/// FRACTIONAL one hands `GL_DITHER` (on by default in GLES2) a half-code to alternate on across a
/// large flat fill, which banded the app ground visibly before it was snapped. So write the code and
/// let this do the division; never a decimal guess.
pub const fn rgb8(r: u8, g: u8, b: u8) -> [f32; 4] {
    [r as f32 / 255.0, g as f32 / 255.0, b as f32 / 255.0, 1.0]
}

// Neutral — achromatic. These two stops are the ones the renderer paints with directly; the rest of
// the palette is `ui::theme`'s private primitive layer, which imports these so a code is still
// written down in exactly one place.
/// The app ground, Apple TV's shelf gray `#2c2c2e` (`theme::SURFACE_APP`'s stop).
pub const NEUTRAL_500: [f32; 4] = rgb8(0x2c, 0x2c, 0x2e);
/// The scrim ink, near-black `#050508` (`theme::scrim`'s stop).
pub const NEUTRAL_1000: [f32; 4] = rgb8(0x05, 0x05, 0x08);

/// GL clear color — 3-float (`frame_clear` takes r,g,b, no alpha). The app's DEFAULT base, and
/// `theme::SURFACE_APP` itself rather than a second copy of its code (both are [`NEUTRAL_500`]):
/// browsing screens clear to the Apple-TV gray, which is what makes a route change read as a
/// seamless dip. Home overdraws it with `SURFACE_APP` (the identical gray). Two punches through to
/// the hardware video plane use transparent black instead: the player route, and the detail page
/// while a trailer preview has presented a frame (`gfx::frame_clear_through`).
pub const CLEAR_RGB: (f32, f32, f32) = (NEUTRAL_500[0], NEUTRAL_500[1], NEUTRAL_500[2]);

/// Hero/scroll scrim ink; use via `theme::scrim`.
pub const SCRIM_INK: [f32; 3] = [NEUTRAL_1000[0], NEUTRAL_1000[1], NEUTRAL_1000[2]];

/// Splat a token's rgb with an overridden alpha (e.g. the `env.sp`-baked hub title). Also how a role
/// spells a stop on the white/black **alpha ramps**: `with_a(WHITE, 0.20)`.
pub const fn with_a(c: [f32; 4], a: f32) -> [f32; 4] {
    [c[0], c[1], c[2], a]
}

// ── THE FOCUSED TILE'S LIT-GLASS EDGE (ArtTile component, Claude Design) ────────────────────────
// A FOCUSED art tile only — fades in with the focus pop `f` (0 at rest, 1 fully focused), folded
// into the same `fs_img.frag` pass as the resting sheen above rather than a second draw. Three
// layers, white light only, never a coloured ring or an outset border. The shader carries the
// pixel geometry as literals (it cannot read a Rust `const`); the numbers below are that shader's
// documentation copy, and `gfx.rs`'s `image_focus_geometry_matches_the_shader_literals` test pins
// the two together so one cannot drift from the other.
/// RIM inner glow (`--card-glass-rim-focus`), base layer: `inset 0 0 12px 0 white/.08`, carried
/// inward from the whole perimeter over this many px.
pub const CARD_GLOW_A: f32 = 0.08;
pub const CARD_GLOW_BAND_PX: f32 = 12.0;
/// RIM inner glow, TOP layer: `inset 0 4px 8px -4px white/.12` — a brighter, tighter band hugging
/// the top edge only (one light, from above — the same direction every card shadow falls in).
pub const CARD_GLOW_TOP_A: f32 = 0.12;
pub const CARD_GLOW_TOP_PX: f32 = 6.0;
/// RIM inner glow, BOTTOM layer: `inset 0 -4px 8px -5px white/.06` — fainter, tighter, on the
/// bottom edge (the light's own falloff reaching the far side of the tile).
pub const CARD_GLOW_BOT_A: f32 = 0.06;
pub const CARD_GLOW_BOT_PX: f32 = 4.0;
/// GLARE (`--card-glass-glare-focus`): the top tab bar's own glass crown. The resting 1px
/// perimeter sheen (`theme::CARD_SHEEN`, .22) is lifted to [`CARD_GLARE_A`] for the top `CARD_GLARE_PX`
/// of the tile height, easing linearly back to the plain sheen by `CARD_GLARE_EASE` of the height —
/// `linear-gradient(180deg, card_glare_a 0, card_glare_a 12px, transparent 16%)` masked to the 1px
/// ring. `CARD_GLARE_EASE` keeps the glare on the crown only, rather than running down the sides.
pub const CARD_GLARE_PX: f32 = 12.0;
pub const CARD_GLARE_EASE: f32 = 0.16;
/// The crown's own target alpha — brighter than `theme::GLASS_RIM_LIGHT` (.28) because a 1px hairline
/// over bright artwork needs more contrast than the same hairline over the dark glass track that
/// `theme::GLASS_RIM_LIGHT` was tuned for.
pub const CARD_GLARE_A: f32 = 0.45;
/// GLOSS (`--card-glass-gloss-focus`): `linear-gradient(160deg, white/.14 0%, transparent 34%)`
/// over the artwork — a soft top-left sheen on the face, composited under the RIM/GLARE above it.
pub const CARD_GLOSS_A: f32 = 0.14;
pub const CARD_GLOSS_FADE: f32 = 0.34;
/// The 160deg CSS gradient direction as a unit vector in card-local (x-right, y-down) space:
/// `(sin 160°, -cos 160°)` — mostly down, slightly left-to-right. `sin160 = sin20`, `-cos160 = cos20`.
pub const CARD_GLOSS_DIR: [f32; 2] = [0.342_020_14, 0.939_692_6];

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
/// is how that is spelled. **`person.rs`'s band condense was the second worked example and is
/// gone** — that band stopped condensing (its module doc says why), so its name is one run at
/// `size::DISPLAY` now and a reader sent there for the pattern would conclude the rule is
/// unimplemented.
pub mod size {
    use std::os::raw::c_int;
    /// Full-bleed hero title — the home + detail hero headline.
    pub const HERO: c_int = 72;
    /// Full-card display title — the post-play (Up Next) card's episode name
    /// (`Plex Pass Awareness.dc.html` deliverable D): a one-line headline for a card that owns
    /// the FRAME but not the page, one ~1.2× ladder step past [`TITLE`] without [`HERO`]'s
    /// billboard weight (a 72px line over live credits would read as a new screen, not a prompt).
    pub const DISPLAY: c_int = 48;
    /// Screen / panel title — the in-player Info card title, the scrolled compact title, About column heads.
    pub const TITLE: c_int = 40;
    /// Section headers ("Related", "Cast & Crew") + list-row titles.
    pub const HEADLINE: c_int = 32;
    /// Default reading text (synopsis, hero meta, About paragraphs, empty states) + control labels (Play / action buttons, tabs, pills — one rung under a header).
    pub const BODY: c_int = 28;
    /// Secondary labels — card / episode / chapter titles, cast names, list detail, meta chips.
    pub const LABEL: c_int = 26;
    /// Couch legibility **FLOOR** for ordinary product text — kickers, timecodes, cast roles,
    /// badges and field labels. The deliberately opened diagnostics instrument is the sole
    /// exception; it has its own dense [`DIAGNOSTIC`] rung rather than weakening this product rule.
    pub const CAPTION: c_int = 24;
    /// Fine print — deliberately below the couch floor (explicit design direction, 2026-07-12,
    /// re-tuned on-device 16 → 20 → 22: "bigger, but smaller than the meta line"). De-emphasis is
    /// the point — atmosphere copy beside an outsized title, not content someone must read; labels
    /// the eye needs to catch (the SxEy kicker, meta lines) stay on the regular rungs.
    ///
    /// **NEITHER HERO'S SYNOPSIS IS ON THIS RUNG ANY MORE**, and this doc went on saying "the small
    /// info/synopsis text under the home/detail hero's title block" for both of the releases in
    /// which that was false of one of them and then of neither. Detail's blurb moved to `LABEL`/36
    /// in `aa598bf2`, home's followed when the two were made one block
    /// (`ui::hero_synopsis`, which holds the argument): a hero blurb is the longest run of
    /// prose on its page, so it is READING copy and the recorded "smaller than the meta line"
    /// directive is satisfied at `LABEL` 26 under a `BODY` 28 meta line. What is left here is the
    /// episode row's air date, the rating-row provider captions, the Library's note line, the
    /// player HUD's key capsule and the Source chip's handle — one-line labels, every one of them.
    pub const MICRO: c_int = 22;
    /// Dense engineering read-outs whose primary job is comparing many bounded numeric fields in
    /// one photograph.  This is deliberately below [`MICRO`]: diagnostics are opened on purpose
    /// and read as an instrument, not as ordinary couch copy.  It must never migrate into product
    /// chrome or prose; `app::diagnostics` is its sole owner.
    pub const DIAGNOSTIC: c_int = 20;
}
