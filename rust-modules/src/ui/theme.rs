//! `theme` — the single palette for the whole UI, in TWO LAYERS.
//!
//! **Primitives** (private, at the top of this file) are the palette itself: the only place a colour
//! CODE is written down. **Roles** (`pub`, everything after them) are the JOBS — [`TEXT_PRIMARY`],
//! [`ACCENT`], [`HAIRLINE`] — and every role resolves to a primitive, never to a fresh literal. A
//! screen may only ever name a role; that is the other end of `ui/CLAUDE.md`'s "never write a raw
//! colour literal" rule. The design project mirrors this split exactly (`tokens/primitives.css` +
//! `tokens/colors.css`), so a palette decision is one edit here and one there.
//!
//! Two roles may resolve to the SAME primitive and still stay two roles — [`ACCENT`] and
//! [`TEXT_PRIMARY`] are both `COOL_0`, [`TEXT_TERTIARY`] and [`RATING_MUTED`] both `COOL_400` —
//! because sharing a value is not sharing a job: retuning the focus fill must not restyle every
//! title on the screen. They are values on one stop for that reason, not `pub use` aliases of each
//! other.
//!
//! The values began as the player screen's literals promoted to canonical (it was the one screen
//! with a coherent design language); the two *lossy* collapses (several near-whites → `COOL_0`;
//! several dim greys → [`TEXT_SECONDARY`]/[`TEXT_TERTIARY`]) are deliberate and only visible
//! on-device — see `docs/ui-system-migration.md` §A. Colors are `[f32; 4]` (r,g,b,a); scrims supply
//! their alpha per call via [`scrim`]/[`scrim_black`].
//!
//! `ACCENT`/`ACCENT_INK` live here and are re-exported from `ui` (`mod.rs`) so the existing
//! `crate::ui::ACCENT` call sites keep compiling unchanged.
#![allow(dead_code)]

// ── PRIMITIVES — the palette. PRIVATE: nothing outside this module may name a stop ───────────
// Families are by hue, stops by lightness (0 = lightest). Every stop is an EXACT 8-bit code, which
// is load-bearing rather than tidy: the panel is plain 888 with no sRGB framebuffer anywhere in the
// tree, so a token value IS an sRGB code — and a FRACTIONAL one hands `GL_DITHER` (on by default in
// GLES2) a half-code to alternate on across a large flat fill, which banded the app ground visibly
// before it was snapped. So write the code and let `rgb8` do the division; never a decimal guess.
/// An 8-bit sRGB code as an opaque token value. `rgb8(0x2c, 0x2c, 0x2e)` is `#2c2c2e`.
const fn rgb8(r: u8, g: u8, b: u8) -> [f32; 4] {
    [r as f32 / 255.0, g as f32 / 255.0, b as f32 / 255.0, 1.0]
}

// Cool — blue-leaning: everything that is text, and artwork that has not loaded.
const COOL_0: [f32; 4] = rgb8(0xf7, 0xfa, 0xfc);
const COOL_50: [f32; 4] = rgb8(0xeb, 0xf0, 0xf7);
const COOL_150: [f32; 4] = rgb8(0xdb, 0xe0, 0xeb);
const COOL_200: [f32; 4] = rgb8(0xcd, 0xd3, 0xdd);
const COOL_300: [f32; 4] = rgb8(0xb8, 0xbf, 0xcc);
const COOL_400: [f32; 4] = rgb8(0x94, 0x99, 0xa3);
// The dark half of the same cool ramp. `COOL_600` is the rung `COOL_400` is on the other side of
// mid: the LIGHT track's idle label, muted against its own material by exactly as much as
// `TEXT_TERTIARY` is against the dark one.
const COOL_600: [f32; 4] = rgb8(0x5a, 0x60, 0x6c);
const COOL_850: [f32; 4] = rgb8(0x1f, 0x21, 0x29);
const COOL_900: [f32; 4] = rgb8(0x14, 0x17, 0x1c);

// Neutral — achromatic: the shelf, panels, control plates, inks.
const NEUTRAL_500: [f32; 4] = rgb8(0x2c, 0x2c, 0x2e);
const NEUTRAL_600: [f32; 4] = rgb8(0x25, 0x25, 0x27);
const NEUTRAL_650: [f32; 4] = rgb8(0x22, 0x22, 0x24);
const NEUTRAL_750: [f32; 4] = rgb8(0x1b, 0x1b, 0x1d);
const NEUTRAL_850: [f32; 4] = rgb8(0x14, 0x14, 0x16);
const NEUTRAL_950: [f32; 4] = rgb8(0x08, 0x08, 0x0a);
const NEUTRAL_1000: [f32; 4] = rgb8(0x05, 0x05, 0x08);
const WHITE: [f32; 4] = rgb8(0xff, 0xff, 0xff);
const BLACK: [f32; 4] = rgb8(0x00, 0x00, 0x00);

// Sand — one warm off-white, and it has exactly one job: the ambient wash's resting cast.
const SAND_100: [f32; 4] = rgb8(0xe9, 0xe6, 0xe0);

// Semantic — amber / red / green: state and verdict, never decoration.
const AMBER_300: [f32; 4] = rgb8(0xfa, 0xb8, 0x2e);
const AMBER_400: [f32; 4] = rgb8(0xf0, 0xb4, 0x29);
const AMBER_500: [f32; 4] = rgb8(0xe5, 0xa0, 0x0d);
const AMBER_950: [f32; 4] = rgb8(0x1a, 0x12, 0x04);
const RED_400: [f32; 4] = rgb8(0xeb, 0x52, 0x4a);
const RED_500: [f32; 4] = rgb8(0xf5, 0x34, 0x1a);
const GREEN_400: [f32; 4] = rgb8(0x3e, 0xc9, 0x6b);
const GREEN_500: [f32; 4] = rgb8(0x2f, 0xae, 0x5b);

// The two ALPHA ramps are `WHITE`/`BLACK` at a measured weight, spelled `with_a(WHITE, .20)` at the
// role: a new overlay is a weight on that ramp, never a new hue. `NEUTRAL_900` (#0c0c0d) is not a
// primitive here because its one role — the tab track — is a translucent GRADIENT in this renderer;
// see `TAB_TRACK_TOP`.

// ── Text ────────────────────────────────────────────────────────────────────
/// Primary reading text / high-emphasis title. Collapses the 3-4 near-whites.
pub const TEXT_PRIMARY: [f32; 4] = COOL_0;
/// Section headings ("Related", "Cast & Crew", About headings) — a touch below primary.
pub const TEXT_HEADING: [f32; 4] = COOL_50;
/// Secondary text: metadata lines, idle row titles.
pub const TEXT_SECONDARY: [f32; 4] = COOL_300;
/// **Reading copy** — a multi-line paragraph someone actually reads through, one step brighter than
/// the label grey above it (`#cdd3dd`, `Details Screen.dc.html`). Its one job today is the detail
/// hero's synopsis, which is the longest run of text on the page and sits over the backdrop scrim;
/// at [`TEXT_SECONDARY`] it read as fine print rather than as the blurb. A one-line metadata VALUE
/// stays secondary — the distinction is paragraph-vs-label, not importance.
pub const TEXT_READING: [f32; 4] = COOL_200;
/// Tertiary text: runtime, kickers, inactive tabs, About labels, dim/empty states.
pub const TEXT_TERTIARY: [f32; 4] = COOL_400;
/// The `·` between fine-print facts, and **only** that — a step below the words it separates.
/// Both mocks set every separator dot `opacity:.45` against the run around it, and the reason is
/// worth keeping: a dot at the full ink of its neighbours joins the list instead of punctuating
/// it, so a three-fact row reads as five things. Derived from [`TEXT_TERTIARY`] rather than
/// spelled out, because it is that ink quietened, not a colour of its own.
///
/// Only reachable where the separator is its OWN draw run. A dot baked into a joined string
/// (`"a   ·   b"`) is one run at one colour by construction; those are unchanged, and converting
/// them is a per-site decision about whether the extra draw call is worth it.
pub const TEXT_SEPARATOR: [f32; 4] = with_a(TEXT_TERTIARY, 0.45);

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
/// **The focus fill — and the only fill a control can light up with.** `COOL_0`: the same near-white
/// [`TEXT_PRIMARY`] is, and the same the tab bar inks a selected label in, because a control lights
/// up for exactly one reason — the remote is on it. Nothing is filled by RANK, so there is no
/// always-filled primary CTA: the hero Play pill sits idle like everything else until focus reaches
/// it.
///
/// It was the mockup's warm "Snow" `#e9e6e0` until the 2026-08-13 palette sync, alongside a second,
/// cooler control white for the "primary" CTA (`FILL_PRIMARY`/`INK_ON_PRIMARY`, and
/// `ControlStyle::Primary` with them). At ten feet nobody could tell the two whites apart, so the
/// pair collapsed onto this one. Both survivors of that collapse are elsewhere and neither is a
/// control: the warm off-white is [`WASH_WARM`], a page ground; the cool plate is
/// [`SURFACE_QR_PLATE`], a scan surface.
pub const ACCENT: [f32; 4] = COOL_0;
/// Near-black ink/glyphs drawn over [`ACCENT`].
pub const ACCENT_INK: [f32; 4] = NEUTRAL_950;
/// Alias for [`ACCENT_INK`] read from the "ink over accent" intent. The one legitimate alias in this
/// file: it is the same ROLE under a second name, not a second role that happens to share a stop.
pub const INK_ON_ACCENT: [f32; 4] = ACCENT_INK;
/// The *spent* portion of a control that is counting down ([`Button::progress`](crate::ui::widgets::Button::progress)).
/// [`ACCENT`] mixed a quarter of the way to the shelf ([`SURFACE_APP`]), NOT a different hue: the
/// control stays focused-looking end to end, so the sweep reads as time passing rather than as the
/// focus state changing. Deliberately close enough to `ACCENT` that [`ACCENT_INK`] stays legible on
/// BOTH sides — the first version flipped the ink at the sweep line and cut the label in half
/// mid-word.
pub const CONTROL_SPENT_FILL: [f32; 4] = mix(COOL_0, NEUTRAL_500, 0.24);

/// Idle (unfocused) control disc/pill fill — solid dark, faintly translucent.
pub const CONTROL_IDLE_FILL: [f32; 4] = with_a(NEUTRAL_600, 0.92);
/// White glyph/label over an idle control.
pub const CONTROL_IDLE_INK: [f32; 4] = WHITE;

// ── Surfaces / backgrounds ───────────────────────────────────────────────────
/// Flat shelf/app base (both gradient stops) — Apple TV's shelf gray **#2C2C2E (44,44,46)**. A
/// MEDIUM gray, deliberately not near-black: the focused-card drop-shadow only reads against a base
/// this light (on the old near-black 25,25,29 a black shadow had almost nothing to darken).
pub const SURFACE_APP: [f32; 4] = NEUTRAL_500;
/// GL clear color — 3-float (`frame_clear` takes r,g,b, no alpha). The app's DEFAULT base, and
/// [`SURFACE_APP`] itself rather than a second copy of its code: every non-player screen clears to
/// the Apple-TV gray (the player clears transparent separately), which is what makes a route change
/// read as a seamless dip. Home overdraws it with `SURFACE_APP` (the identical gray).
pub const CLEAR_RGB: (f32, f32, f32) = (SURFACE_APP[0], SURFACE_APP[1], SURFACE_APP[2]);
/// Opaque menu panel / fade mask / badge knockout interior.
pub const SURFACE_PANEL: [f32; 4] = NEUTRAL_650;
/// Near-opaque sheet/card gradient — top stop. [`SURFACE_PANEL`]'s own stop at .985, so the sheet
/// and its opaque twin are one material rather than two greys a hair apart.
pub const PANEL_TOP: [f32; 4] = with_a(NEUTRAL_650, 0.985);
/// Near-opaque sheet gradient — bottom stop (kept distinct; the gradient is deliberate).
pub const PANEL_BOT: [f32; 4] = with_a(NEUTRAL_750, 0.985);
/// The FROSTED sheet's gradient — top stop: the same two greys as [`PANEL_TOP`]/[`PANEL_BOT`] at
/// the alpha a real backdrop blur allows, and the same material as far as the palette is concerned.
///
/// It is a separate pair rather than a lower alpha passed at the call site because the two are not
/// interchangeable: `.985` over an unknown background is the panel *being* its own ground, and this
/// one is only legal ON TOP of [`Painter::backdrop_blur`](crate::ui::Painter::backdrop_blur) — a
/// panel that draws it without one is a translucent hole onto whatever the page had there.
/// [`Popover::panel`](crate::ui::popover::Popover::panel) is what keeps the two paired.
///
/// The value is the legibility floor, not a taste knob: at .72 a `TEXT_TERTIARY` sub-line still
/// clears 4.5:1 over the brightest ground a blurred poster shelf produces (the blur removes detail,
/// it does not cap luminance), and every rung below that was measured against a white poster.
pub const PANEL_FROST_TOP: [f32; 4] = with_a(NEUTRAL_650, 0.72);
/// The frosted sheet's bottom stop — see [`PANEL_FROST_TOP`].
pub const PANEL_FROST_BOT: [f32; 4] = with_a(NEUTRAL_750, 0.72);
/// The sign-in **QR card's** quiet-zone plate (`login.rs`) — a bright cool-white whose job is SCAN
/// CONTRAST for a phone camera, not focus. It is a SURFACE, not a control fill, which is the whole
/// reason it outlived the second control white it used to be (`FILL_PRIMARY`): a plate under a
/// black-tinted QR texture, and the only place in the app where the app paints something this
/// bright on purpose.
pub const SURFACE_QR_PLATE: [f32; 4] = COOL_0;
/// Poster/thumb skeleton flat placeholder.
pub const CARD_PLACEHOLDER: [f32; 4] = COOL_850;
/// Loading-skeleton gradient — top. The same `COOL_850` stop as [`CARD_PLACEHOLDER`]: a card whose
/// artwork is on the way and one with none to load are the same object at rest. Both lean BLUE,
/// away from the neutral panel greys, so a missing poster never reads as a panel.
pub const SKELETON_TOP: [f32; 4] = COOL_850;
/// Loading-skeleton gradient — bottom.
pub const SKELETON_BOT: [f32; 4] = COOL_900;

// ── Scrims (near-black; alpha supplied per call) ─────────────────────────────
/// Hero/scroll scrim ink; use via [`scrim`].
pub const SCRIM_INK: [f32; 3] = [NEUTRAL_1000[0], NEUTRAL_1000[1], NEUTRAL_1000[2]];
/// Pure-black scrim ink (HUD bottom, subtitle outline, modal); use via [`scrim_black`].
pub const SCRIM_BLACK_INK: [f32; 3] = [BLACK[0], BLACK[1], BLACK[2]];

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
/// The same weight the tab bar's own track capsule already uses for the same job over the same
/// artwork ([`TAB_TRACK_A_TOP`]) — one value per role, arrived at independently twice. An alpha
/// rather than a colour, like [`CARD_SHADOW_REST_A`]: the ink is [`SCRIM_INK`], only the weight is
/// the decision. **This is the one knob** if the treatment reads heavy — 0.60 still clears 3:1
/// everywhere.
pub const SCRIM_TEXT_A: f32 = 0.72;

/// Near-black scrim at alpha `a` — hero/scroll dimming.
pub const fn scrim(a: f32) -> [f32; 4] {
    [SCRIM_INK[0], SCRIM_INK[1], SCRIM_INK[2], a]
}/// Pure-black scrim at alpha `a` — HUD/modal dimming.
pub const fn scrim_black(a: f32) -> [f32; 4] {
    [SCRIM_BLACK_INK[0], SCRIM_BLACK_INK[1], SCRIM_BLACK_INK[2], a]
}
/// Splat a token's rgb with an overridden alpha (e.g. the `env.sp`-baked hub title). Also how a role
/// spells a stop on the white/black **alpha ramps**: `with_a(WHITE, 0.20)`.
pub const fn with_a(c: [f32; 4], a: f32) -> [f32; 4] {
    [c[0], c[1], c[2], a]
}
/// Blend `a` toward `b` by `t` (rgb only; keeps `a`'s alpha) — for a token that is a *mix* of two
/// roles rather than one of them, e.g. an ambient wash sitting `t` of the way from [`SURFACE_APP`]
/// to an item's artwork colour. A screen that lerps channels in a loop wants this instead. `const`
/// so a ROLE can be spelled as a mix of two primitives ([`CONTROL_SPENT_FILL`]) — the design
/// project's `color-mix(in srgb, …)`.
pub const fn mix(a: [f32; 4], b: [f32; 4], t: f32) -> [f32; 4] {
    [a[0] + (b[0] - a[0]) * t, a[1] + (b[1] - a[1]) * t, a[2] + (b[2] - a[2]) * t, a[3]]
}
/// [`mix`], **alpha included** — for CROSS-FADING one role into another over time, as opposed to
/// spelling a token that sits between two of them.
///
/// The distinction is not pedantry, it is the reason both exist. `mix` keeps `a`'s alpha because a
/// tint has one opacity and two hues; a cross-fade has two of each, and the pairs this app actually
/// animates between differ in both — `CONTROL_IDLE_FILL` is `.92` where `ACCENT` is opaque, and
/// `TEXT_TERTIARY` is opaque where `ROW_VALUE_INK_ON_DIM` is `.38`. Reaching for `mix` there lands
/// the destination hue at the SOURCE's opacity, which on the search field's own hint run is a
/// near-black at full strength over white.
pub const fn cross(a: [f32; 4], b: [f32; 4], t: f32) -> [f32; 4] {
    [
        a[0] + (b[0] - a[0]) * t,
        a[1] + (b[1] - a[1]) * t,
        a[2] + (b[2] - a[2]) * t,
        a[3] + (b[3] - a[3]) * t,
    ]
}
/// Scale a token's rgb by `k` toward black, keeping its alpha — e.g. folding a scroll-darken into a
/// layer tint. Pair with [`with_a`] when the alpha also changes.
pub const fn dim(c: [f32; 4], k: f32) -> [f32; 4] {
    [c[0] * k, c[1] * k, c[2] * k, c[3]]
}

// ── Rails / overlays / accents ───────────────────────────────────────────────
/// Unfilled progress/scrubber track.
pub const RAIL_TRACK: [f32; 4] = with_a(WHITE, 0.20);
/// Buffered-ahead / resume-track band.
pub const RAIL_BUFFERED: [f32; 4] = with_a(WHITE, 0.28);
/// Played/filled portion of a rail.
pub const RAIL_FILL: [f32; 4] = with_a(WHITE, 0.95);
/// An intro/credits SEGMENT on the player scrubber — a band lit a step above the track, drawn
/// between the track and the fill so the played part of a segment reads as played. Stays inside the
/// rail's monochrome language on purpose: a hue here would compete with the amber resume fill for
/// "this bit is special", and the rail sits straight over moving video.
// (RAIL_MARKER, white @ 0.42, was the intro/credits band on the scrubber. Removed 2026-08-04 with
// the band itself — at twice the track's opacity it read as a rendering artifact rather than as
// information. See the "the rail carries NO marks" note in ui/player_hud.rs.)
/// The **ambient wash's** resting tint — the faint warm cast a page carries when no artwork is
/// keying it (the person page's header state, `Person Screen v2.dc.html`'s
/// `rgba(233,230,224,.10)`). The palette's one warm stop, and the only survivor of the warm "Snow"
/// [`ACCENT`] used to be: a page GROUND, never a control. Its own role rather than a borrowed one,
/// so retuning a focus colour can never silently restyle a page background.
pub const WASH_WARM: [f32; 4] = SAND_100;
/// Warm amber Continue-Watching progress fill (Plex-specific; no player equivalent). `#fab82e`.
pub const RESUME_FILL: [f32; 4] = with_a(AMBER_300, 0.95);
/// Plex's own "Plex Pass" gold, `#e5a00d` — a BRAND REFERENCE, not a palette member
/// (`Plex Pass Awareness.dc.html`, "the amber decision"). Two ambers exist ON PURPOSE and stay
/// two tokens: [`RESUME_FILL`] is ours and tunable; this one names somebody else's colour and
/// must not drift when the first is retuned. They never meet: progress fill lives on card art;
/// pass-gold appears on exactly TWO surfaces, both derived from Plex's own Pass docs (the owner's
/// directive — design as visual baseline, logic from the docs): the failure read-out's filled
/// capsule, and the detail facts row's two Pass-gated states (`detail::play_note`, the docs-derived
/// truth table — hardware conversion and HDR tone mapping are both Pass-gated server features, and
/// the warning outranks the soft note because a wrong picture outranks a slower one). Every use of
/// the name is load-bearing,
/// which is the trademark position. [`crate::ui::widgets::pass_capsule`] is its only consumer
/// besides ink; a line carries at most one gold thing, and warning severity is never amber — it
/// is carried by glyph, stroke and contrast.
pub const PASS_GOLD: [f32; 4] = AMBER_500;
/// Near-black ink over a [`PASS_GOLD`] fill — the error read-out's FILLED capsule, the one place
/// the capsule fills (on pure black an outline has no ground and reads as a hole). `#1a1204`.
pub const PASS_GOLD_INK: [f32; 4] = AMBER_950;
/// Unfilled track behind the Continue-Watching resume bar — the full-bleed card-bottom rail. A
/// hair lighter than the player scrubber's so the amber reads against a bright poster.
pub const RESUME_TRACK: [f32; 4] = with_a(WHITE, 0.22);
/// Ink over [`RESUME_FILL`] when the amber is a SURFACE rather than a mark. Distinct from
/// [`ACCENT_INK`]: that one is tuned against the near-white [`ACCENT`], and at that value it
/// disappeared into the amber rather than reading as a knockout.
///
/// **No consumer today** — its one user was the amber watched DISC, which the 2026-08-13 evening
/// sync replaced with a bare tick over a veil ([`TILE_MARK_INK`]). Kept because the design system
/// still carries the role, and because the next amber surface needs exactly this ink.
pub const INK_ON_RESUME: [f32; 4] = NEUTRAL_850;

// ── A list row's trailing VALUE read-out ─────────────────────────────────────
/// The word at a row's trailing edge that says what it is SET to (`On`/`Off`, "English"), over the
/// FOCUSED row's near-white pill. One step behind the label so the label is what you read and the
/// value is what you check — and an ALPHA of the pill's own ink rather than a grey, because a grey
/// over near-white goes muddy where black at a weight stays clean. Unfocused rows use
/// [`TEXT_SECONDARY`] (and [`TEXT_TERTIARY`] when quietened), which need no token of their own.
pub const ROW_VALUE_INK_ON: [f32; 4] = with_a(BLACK, 0.60);
/// The same read-out a further step back — see [`ROW_VALUE_INK_ON`]. Derived from [`ACCENT_INK`]
/// rather than black: at .38 the ink is thin enough that its HUE starts to show, and the pill's own
/// near-black is the one this must not look tinted against.
pub const ROW_VALUE_INK_ON_DIM: [f32; 4] = with_a(ACCENT_INK, 0.38);

// ── The watched TICK on artwork (the tile's one state mark) ──────────────────
/// The tick's ink: `COOL_0`, the same near-white a title is set in. **No disc, no plate** — the
/// artwork stays visible and only the veil below touches it. (The mark was an amber disc with a
/// knocked-out check for one day, 2026-08-13; the amber is now the resume bar's alone, so the two
/// states cannot be mistaken for each other at a glance across a shelf.)
pub const TILE_MARK_INK: [f32; 4] = COOL_0;
/// The **veil**: a corner falloff under the tick, and the only thing that touches the picture. A
/// white tick has no contrast of its own over a light frame — a snowfield, a white title card, the
/// office's white backdrop — so this guarantees it reads without putting a plate on the artwork.
/// Peak strength at the corner itself, gone by ~half the veil's box (`widgets::tile_mark_veil`).
pub const TILE_MARK_VEIL: [f32; 4] = with_a(BLACK, 0.40);
/// The tick's own soft drop shadow, INSIDE the veil — belt and braces on a bright frame, and what
/// keeps the mark from dissolving into a busy one (the design's `drop-shadow(0 2px 7px …)`).
pub const TILE_MARK_SHADOW: [f32; 4] = with_a(BLACK, 0.60);
/// Error/destructive signal ink — the wrong-PIN dot flash. Desaturated toward the palette's
/// warm neutrals so it reads as a state, not an alarm.
pub const DANGER: [f32; 4] = RED_400;
/// Dev counter's two human-visible buffer phases. The renderer holds each for 30 returned swaps,
/// so motion is visible instead of red/green frames blending into yellow at panel refresh rate.
pub const DIAG_FLIP_A: [f32; 4] = GREEN_400;
pub const DIAG_FLIP_B: [f32; 4] = RED_400;
/// Section divider hairline.
pub const HAIRLINE: [f32; 4] = with_a(WHITE, 0.10);
/// A glass container's **perimeter line** — the design system's `--glass-rim`, one notch under the
/// tiles' own [`CARD_SHEEN`] (.22) because a sheet's edge is quieter than a card's.
///
/// It goes ON TOP of the material, not under it, and that ordering is the whole reason it is a
/// token here rather than a shader term. The design states the track as two inset shadows —
/// `inset 0 0 0 1px var(--glass-rim), inset 0 1px 0 var(--glass-rim-light)` — and an inset shadow
/// composites over the background it is declared with. `fs_glass.frag`'s own specular hairline is
/// part of the BACKDROP, so the darkening lands on top of it and its weight is whatever the scrim
/// leaves; drawing the line here puts it where the design puts it and makes .14 mean .14.
pub const GLASS_RIM: [f32; 4] = with_a(WHITE, 0.14);
/// The same line on the TOP edge only, at double weight — `--glass-rim-light`. One light, from
/// above, the direction every card shadow in this system already falls in.
pub const GLASS_RIM_LIGHT: [f32; 4] = with_a(WHITE, 0.28);

/// The lit edge's weight where the surface has the BRIGHTEST ground it will ever sit on.
///
/// [`GLASS_RIM`] and [`GLASS_RIM_LIGHT`] are the design system's constants and they are right for
/// the ground the design was drawn over — a dark one. They are constants, though, and the edge they
/// draw is not a colour but a RELATIONSHIP: a lit edge has to be brighter than the light it is
/// catching. Measured on the panel against bright artwork, the .28 top line lands **34.9 L\* below
/// its own ground**, which stops it being a local maximum across the edge at all — the profile runs
/// 220 → 196 → 150 → 73 straight down, and an edge that is only a step on a monotone ramp reads as
/// blur, not as a bevel. Reported as "the rim is harder to see than the buttons'", which is exactly
/// right and is a comparison worth keeping: a control sits in the darkest quarter of the frame by
/// construction, so its rim is the brightest thing at its boundary and never had this problem.
///
/// So the two constants become the FLOOR of a ramp that ends here, travelled with the ground — the
/// same ground, and the same eased signal, the scrim density already follows. A dark ground still
/// gets exactly `.14`/`.28` and nothing about that case moves.
///
/// **One, and one is safe rather than reckless, because the rim is a LERP.** `fs_glass.frag` mixes
/// the surface TOWARD this colour, so the ceiling means "at the brightest ground this bar will ever
/// sit on, the line is the line's own colour" — a 1px white hairline, which is what a lit edge over a
/// bright sky looks like. It could not have been 1.0 while the rim was an ADDITION: there the value
/// was an amount of light poured onto whatever was underneath, and 1.0 was a blown-out smear.
pub const GLASS_RIM_MAX: f32 = 1.0;

// ---- The standing track has ONE material -----------------------------------------------------
//
// It had two for a while: a black pair for dark artwork and a white pair for bright, with a
// crossover between them. That is Apple's answer and it is a good one — over bright content their
// chrome goes light and flips its ink — but it is a DISCRETE second state, and a discrete state has
// to be decided every time the ground moves. Deciding it produced four reported bugs in one week: a
// bar flipping back and forth on one unchanging hero, the same hero settling dark in one run and
// light in the next, 1.6 s between the press and the material changing on a route change, and a
// visible wrong-direction excursion while the decision was pending. Guarding it took a hysteresis
// band, a commit window, a two-independent-readings rule, an override that woke the whole screen so
// those readings could happen at all, a page-swap fast path and a frozen draw weight.
//
// A judging panel priced that against what the second polarity actually bought, on synthetic grounds
// (`ui::testpat`) rather than on whatever poster happened to be up, and three of four lenses said
// delete it. The measurements that decided it:
//
//   * with a brighter idle ink (`TEXT_READING` rather than `TEXT_TERTIARY`) the dark material sits
//     at or near its floor for every ground up to L* 73, so the light one is not rescuing the common
//     case — the two are BIT-IDENTICAL on 8 of 14 graded grounds;
//   * on the real heroes it differs on, the light material wins only where the hero is uniformly
//     bright, and wins there BY DISSOLVING: L* 97.4 against a L* 95.7 ground is 1.7 L* of
//     separation, i.e. four words and a rim with no container under them;
//   * on a hero whose ground is MIXED — half a dark building, half bright sky, which a census of a
//     real library's rotation found is the median case, span 26.8 L* — the light material lands
//     34.9 L* off the local ground on the dark half where the dark material lands 3.4 off. The
//     Apple property it was imitating (sit close to your backdrop) is honoured by the material that
//     replaced it.
//
// What remains is one black pair, solved per frame, and an ink bright enough that it rarely has to
// ask for much. `docs/glass-hardware-budget.md` prices the surface; the deleted polarity is in the
// history if it is ever wanted back.

/// …and the focus pill's own RIM there: a specular hairline along the top edge — the same lamp the
/// track's hairline comes from — over a perimeter kept two steps below anything that could read as
/// an outline.
pub const PILL_RIM_ON_LIGHT: [f32; 4] = with_a(BLACK, 0.08);
pub const PILL_RIM_LIT_ON_LIGHT: [f32; 4] = with_a(WHITE, 1.0);
/// Faint focus pill (pre-`TabPill`-adoption tab highlight).
pub const OVERLAY_FOCUS_PILL: [f32; 4] = with_a(WHITE, 0.14);
/// The shared top tab bar's **TRACK** — the recessed near-black capsule the tab pills sit in, and
/// the one thing that owns their legibility over bright hero art (inside it a plain segment can stay
/// bare [`TEXT_TERTIARY`] text, and the wedge above deliberately stops short of it: see
/// `widgets::HERO_BASE_SCRIM_Y0`). Dark-material weight — light enough to keep a hint of the
/// artwork, dark enough that tertiary ink holds over near-white art — and a GRADIENT: this top stop
/// plus [`TAB_TRACK_BOT`].
///
/// The design project flattens the pair to one opaque code (`--tab-track-fill`, Neutral 900
/// `#0c0c0d`) because CSS cannot say "black at .72 over whatever happens to be behind it". That code
/// IS this stop composited over [`SURFACE_APP`] (44 × 0.28 ≈ 12), which is what makes the two
/// descriptions one material rather than two decisions.
pub const TAB_TRACK_TOP: [f32; 4] = scrim_black(TAB_TRACK_A_TOP);
/// The track's bottom stop — see [`TAB_TRACK_TOP`].
pub const TAB_TRACK_BOT: [f32; 4] = scrim_black(TAB_TRACK_A_BOT);
/// The tab track's stops when it is drawn as GLASS — the same near-black ink at roughly half the
/// weight, since a real backdrop behind it is doing the work the flat material had to do alone.
/// Paired with `Painter::backdrop_blur` exactly as [`PANEL_FROST_TOP`] is: without one they are a
/// translucent hole onto the page.
///
/// **The track DARKENS where a panel frosts, and that is the difference between a container you
/// read THROUGH and one you read ON.** A sheet is a neutral frost because it holds a page's worth
/// of copy; this band is 76px tall over moving artwork and its idle labels are [`TEXT_TERTIARY`],
/// which a neutral frost leaves swimming. So it takes a black scrim pair rather than the panel
/// family's greys — the design system's `--glass-track-top`/`-bot`, the two stops on the existing
/// ramp nearest the .38/.46 this was first built with.
///
/// **These are the FLOOR of a range, not a fixed pair** — [`crate::ui::widgets::track_alpha_for`]
/// solves the weight per frame from what is actually behind the bar, and this is where it starts.
///
/// They were a fixed pair for one afternoon and could not be: both the .38/.46 they were derived
/// from and the .34/.50 recorded here were tuned against a render that darkened TWICE — the direct
/// source pass drew the flat track into the glass track's own backdrop (`widgets::draw_tab_row`
/// holds the account of it), so a nominal .42 was landing at an effective ~.87 and every legibility
/// judgement was made on the wrong picture. With that fixed the material became honest and the
/// numbers stopped being enough. Against the worst artwork a hero can be — white — the ground is
/// `1 - a`, and [`TEXT_TERTIARY`] over it:
///
/// | a | contrast | | a | contrast |
/// |---|---|---|---|---|
/// | .34 | 1.21 | | .62 | 2.17 |
/// | .50 | 1.39 | | .67 | 2.64 |
/// | .56 | 1.73 | | **.72** | **3.23** |
///
/// The bar is **3:1** — the same one `widgets`' ground test holds an ambient wash to, and the same
/// arithmetic that put [`SCRIM_TEXT_A`] and [`TAB_TRACK_A_TOP`] at .72 in the first place. Read as a
/// CONSTANT that table says the first legal density is the flat track's own, because **a blur
/// removes DETAIL, not brightness**: a quarter-res Kawase of a white poster is still white. That was
/// the verdict for a day, and it is the right verdict for a constant.
///
/// **The premise was the mistake.** A hero is one picture at a time, not every picture at once, so
/// the density does not have to survive the worst one — it has to survive THIS one. Solved per
/// frame it sits here on a dark backdrop and walks up the table only as far as the ground makes it,
/// which on a bright hero measured .562 and on most heroes is this floor. The ink never moves,
/// which is what the row's hierarchy is made of.
pub const TAB_GLASS_TOP: [f32; 4] = scrim_black(0.20);
/// **The material's LIGHT FLOOR — the amount of itself the glass shows where the page shows
/// nothing.** Two numbers, both measured against the real thing rather than chosen.
///
/// A black scrim can only ever subtract, and at the bottom of the range there is nothing left to
/// subtract from: over a page at L\*0 the bar's face measures exactly the page — 0% Weber — and the
/// whole container is carried by one pixel of rim. Over L\*20 it is 28% and darker. The system
/// container this material is answering does the opposite: measured on the real macOS 26 tab bar
/// over a near-black page, .071 → .098, **+38% Weber and LIGHTER**. Its edge, meanwhile, is one
/// antialiased pixel with no rim at all on any ground — it does not need one, because the material
/// separates itself.
///
/// So the material gets a diffuse component, and this is the one number behind it: **the darkest
/// the glass itself may be, in absolute sRGB, however dark the page gets.** It is a floor, not a
/// target — the scrim keeps doing all the separating it can, and this only catches the bottom. A
/// page brighter than the floor takes no lift at all, so every ordinary hero is untouched bit for
/// bit; `crate::ui::widgets::track_lift` is the rule and records the two shapes that were tried
/// first and why they were worse.
///
/// **This is not the light polarity coming back.** That was a second MATERIAL — a white scrim with
/// dark ink and a crossover to hunt — and it was deleted with the argument that a bar is one
/// surface. This is one surface still: same ink, same solve, same scrim, with a floor under how
/// dark the glass itself is allowed to get.
pub const TAB_GLASS_LIFT_FLOOR: f32 = 0.045;
/// The glass track's bottom stop — see [`TAB_GLASS_TOP`].
pub const TAB_GLASS_BOT: [f32; 4] = scrim_black(0.36);
/// The track material's two weights, exposed as alphas because the focused **profile chip** wears
/// the same material and FADES it in with its unfurl (`scrim_black(A * e)`) — one material and one
/// pair of weights whether it is painted at full strength or on the way in, which is what keeps the
/// chip's capsule and the tab track reading as one band on the top chrome line.
pub const TAB_TRACK_A_TOP: f32 = 0.72;
/// See [`TAB_TRACK_A_TOP`].
pub const TAB_TRACK_A_BOT: f32 = 0.82;
/// A PLATED tab segment's fill — the detail page's season tabs, which sit bare on the backdrop rather
/// than inside the top bar's track and so provide their own ground
/// ([`crate::ui::widgets::TabPill::plated`], `Details Screen.dc.html`). The selected one is a step up
/// from [`OVERLAY_FOCUS_PILL`]: at .14 against a plated neighbour at .08 the two read as the same pill.
pub const TAB_PLATE_SELECTED: [f32; 4] = with_a(WHITE, 0.20);
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
pub const TAB_PLATE_SELECTED_OVER: [f32; 4] = with_a(WHITE, 0.13);
/// An unselected plated segment — present, but only just: it says "this is a control" and nothing more.
pub const TAB_PLATE_IDLE: [f32; 4] = with_a(WHITE, 0.08);
/// Softer selection panel — the lightest weight on the overlay ramp, and deliberately NOT
/// [`TAB_PLATE_IDLE`]'s .08 one step up. The design project briefly resolved this role to that .08
/// stop while its own specimen card and readme both said .07; holding the product at .07 was the
/// right call, and the design has since added a .07 stop of its own and points here again.
pub const OVERLAY_FOCUS_SOFT: [f32; 4] = with_a(WHITE, 0.07);
/// Outlined-badge / meta-badge border.
pub const OVERLAY_BORDER: [f32; 4] = with_a(WHITE, 0.55);
/// The keyline PILL's stroke — a secondary action outlined over scrimmed video (the post-play
/// card's "Watch credits"; `Plex Pass Awareness.dc.html` deliverable D: stroke 1.5, white .38).
/// Deliberately a step quieter than [`OVERLAY_BORDER`]: that value is tuned for a 2px chip border
/// on a panel, and at .55 a 60px capsule outline over a ~.85 black scrim becomes the row's
/// brightest object, outshining the filled primary beside it.
pub const PILL_KEYLINE: [f32; 4] = with_a(WHITE, 0.38);
/// The keyline pill's knockout interior — and here the knockout is the DESIGN, not a limitation.
/// (`Painter::rring` draws a genuinely hollow outline now, which is what `keyline_chip` and the
/// PLEX PASS capsule use.) This pill sits over LIVE VIDEO, where a hollow ring would leave the
/// label on whatever frame happens to be under it; the interior is a translucent near-black that
/// keeps the scrimmed credits part of the surface instead of punching an opaque hole in them (and
/// doubles as the quiet plate [`TEXT_HEADING`] ink needs).
pub const PILL_KEYLINE_BG: [f32; 4] = scrim_black(0.55);
/// Filled metadata chip (the About column's CC/SDH/AD accessibility badges). A COOL tint rather than
/// plain white at a weight, so a chip reads as a plate rather than as a gap in the panel.
pub const BADGE_FILL: [f32; 4] = with_a(COOL_150, 0.20);
/// No-op texture tint (structural: draw an RGBA texture unmodified).
pub const TINT_WHITE: [f32; 4] = WHITE;

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
pub const RATING_FRESH: [f32; 4] = RED_500;
/// The Certified body — `#f0b429`. The SAME fruit struck in gold rather than a separate seal:
/// a rarer bar reads as a richer version of the thing, and a seal was the one shape here that was
/// unmistakably somebody's award rather than a piece of fruit.
pub const RATING_CERTIFIED: [f32; 4] = AMBER_400;
/// The audience crowd when the verdict is good — `#3ec96b`. Note the polarity across the row is
/// deliberately NOT uniform: critics go red-for-good (a ripe tomato), audience goes green-for-good
/// (a healthy crowd). Each mark is read against itself, not against its neighbour.
pub const RATING_AUDIENCE: [f32; 4] = GREEN_400;
/// The **calyx** green — the tomato's leaf-and-stem, on a fresh or certified body. `#2fae5b`, and
/// deliberately not [`RATING_AUDIENCE`]: they are two marks' greens that land close, and folding
/// them would let an audience tweak silently restyle a leaf.
pub const RATING_LEAF: [f32; 4] = GREEN_500;
/// The drained state, for both a hollow tomato and a negated crowd — `COOL_400`, the same stop
/// [`TEXT_TERTIARY`] resolves to, because the negation is literally "this has the weight of a
/// caption now". Its own role ON that stop rather than an alias OF the text token: a verdict mark
/// and a runtime label are two jobs, and retuning one must not restyle the other.
pub const RATING_MUTED: [f32; 4] = COOL_400;

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
pub const CARD_SHEEN: [f32; 4] = with_a(WHITE, 0.22);
/// Stroke width (px) of the perimeter edge-highlight.
pub const CARD_SHEEN_W: f32 = 1.0;
/// Drop-shadow ink under a raised card — pure black (the design's `rgba(0,0,0,…)`), alpha supplied per
/// call (× the resting→lifted focus ramp). Only the alpha/rgb matter for the folded card shadow (the
/// rgb is used by the chip's standalone shadow); its own token (not `scrim_black`) so it can be tuned alone.
pub const CARD_SHADOW: [f32; 4] = with_a(BLACK, 0.40);
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
