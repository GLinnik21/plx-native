//! `HeroLogo` — the ONE way a clearLogo is sized, placed, and fallen back from.
//!
//! Three screens draw an item's clearLogo with a text title behind it: the home hero, the detail
//! hero, and the detail page's pinned compact title. They used to pass their own literal bounds to
//! `the old poster store's `logo_tex`` (660×96 / 680×120 / ∞×54), which made the same mark three sizes in one app —
//! and, because every one of those boxes is far wider than it is tall, made all three a pure HEIGHT
//! clamp. Under a height clamp the drawn area is linear in aspect, so a 5:1 wordmark covered five
//! times the ink of a 1:1 emblem and square logos read as an afterthought.
//!
//! The rule targets a bounding-box AREA, then uniformly contains it within the rung's width and
//! height limits and the caller's column. There is no painted-height floor: one would inflate the
//! area of wide wordmarks. Tall emblems get more height while wide wordmarks get more width.
//!
//! **Layout ≠ paint**: the flow reserves a CONSTANT [`band_h`]. A taller logo spills upward;
//! a shorter logo sits at the band's foot. The bottom anchor keeps metadata and actions stable
//! across shapes, texture arrivals and the hero-to-compact transition.
//!
//! There is deliberately no fade-in behind the text→logo swap: `ui::tex::logo_src` answers `None`
//! both while the fetch is pending AND when the item simply has no clearLogo. The store can now
//! say which (`ui::tex::logo_failed`, a settled miss), and the placeholder accounting uses it —
//! the text of a pending logo is a counted wait, the text of an absent one is not, and the peek
//! is only made while that accounting can be seen (`placeholder::accounting_visible`) — but no
//! cross-fade has been built on it. The swap is a cut, but nothing around it MOVES, which is the
//! part that used to read as a glitch.
use crate::label::HAlign;
use crate::text_view::TextView;
use plx_machine::machine::Measure;
use crate::{theme, Painter, Rect};
use std::os::raw::c_int;

/// Which presence rung a logo is drawn at. Both HEROES share one rung — the "mutual logic for logo
/// size for all hero" directive — and the pinned compact title is the only other one. A new site
/// picks a rung; it does not invent bounds.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum LogoRung {
    /// The full-bleed hero title band — home's sliding column and the detail page's title block.
    Hero,
    /// The detail page's pinned compact title, the strip the hero scrolls up into.
    Compact,
    /// A Continue Watching deck card's logo (420x236): baked into the card's composite
    /// (`card_art::compose`) and, when there is no logo, its title in type (`title_text`).
    Card,
    /// A continuous point between two named rungs, at `t` (0 = the first, 1 = the second) — never
    /// authored as a constant, always the live output of [`LogoRung::lerp`] driven by a spring or
    /// other per-frame progress value (the detail hero's trailer-preview logo, shrinking from
    /// `Hero` to `Compact` while a trailer plays in the background). `PartialEq` compares floats
    /// here, so treat two `Custom` values as equal only by coincidence — match on `Hero`/`Compact`
    /// as before, never on this variant's fields.
    Custom {
        area: f32,
        band_h: f32,
        w_max: f32,
        h_max: f32,
        fallback_sz: c_int,
    },
}

impl LogoRung {
    /// `(target area px², layout band height, height ceiling, width ceiling, fallback size)`.
    /// One match keeps every rung's sizing and layout policy together.
    fn bounds(self) -> (f32, f32, f32, f32, c_int) {
        match self {
            LogoRung::Hero => (
                theme::logo::HERO_AREA,
                theme::logo::HERO_BAND_H,
                theme::logo::HERO_H_MAX,
                theme::logo::HERO_W_MAX,
                theme::size::DISPLAY,
            ),
            LogoRung::Compact => (
                theme::logo::COMPACT_AREA,
                theme::logo::COMPACT_BAND_H,
                theme::logo::COMPACT_H_MAX,
                theme::logo::COMPACT_W_MAX,
                theme::size::TITLE,
            ),
            LogoRung::Card => (
                theme::logo::CARD_AREA,
                theme::logo::CARD_BAND_H,
                theme::logo::CARD_H_MAX,
                theme::logo::CARD_W_MAX,
                theme::size::HEADLINE,
            ),
            LogoRung::Custom { area, band_h, h_max, w_max, fallback_sz } => (area, band_h, h_max, w_max, fallback_sz),
        }
    }

    /// A continuous point between rungs `a` and `b` at `t` (clamped to `[0, 1]`) — every one of
    /// the area/band/axis-limit/fallback-size numbers interpolated independently, so [`fit`]'s
    /// constant-area solve produces a continuously changing size across a live transform instead
    /// of snapping between two fixed rungs partway through it. `t = 0.0`/`1.0` reproduce `a`/`b`'s
    /// own `bounds()` exactly (pinned by a host test), so this is safe to call every frame of a
    /// spring/ease that starts or ends at either endpoint.
    pub fn lerp(a: LogoRung, b: LogoRung, t: f32) -> LogoRung {
        let (a_area, a_band, a_max, a_wmax, a_sz) = a.bounds();
        let (b_area, b_band, b_max, b_wmax, b_sz) = b.bounds();
        let t = t.clamp(0.0, 1.0);
        let lerp = |x: f32, y: f32| x + (y - x) * t;
        LogoRung::Custom {
            area: lerp(a_area, b_area),
            band_h: lerp(a_band, b_band),
            w_max: lerp(a_wmax, b_wmax),
            h_max: lerp(a_max, b_max),
            fallback_sz: lerp(a_sz as f32, b_sz as f32).round() as c_int,
        }
    }
}

/// The LAYOUT height of a logo's title band — constant, independent of the source
/// (which is why it takes no source argument). A caller's flow reserves exactly this; a taller logo
/// overflows upward as paint. See the module docs for why that split is the whole point.
pub fn band_h(rung: LogoRung) -> f32 {
    rung.bounds().1
}

/// The DRAWN size of a `src_w`×`src_h` logo at `rung` inside a `col_w`-wide column.
/// Pure — no GL, no font, no globals; this is the host-testable core of the sizing rule.
///
/// Solve `w*h = area` at the source aspect, then uniformly shrink to both axis limits and the
/// caller's column. A narrow column may reduce area further; it never distorts the mark.
pub fn fit(rung: LogoRung, src_w: f32, src_h: f32, col_w: f32) -> (f32, f32) {
    let (area, _, h_max, w_max, _) = rung.bounds();
    if !src_w.is_finite() || !src_h.is_finite() || !col_w.is_finite()
        || src_w <= 0.0 || src_h <= 0.0 || col_w <= 0.0 {
        return (0.0, 0.0);
    }
    let aspect = src_w / src_h;
    if !aspect.is_finite() || aspect <= 0.0 { return (0.0, 0.0); }
    let h = (area / aspect).sqrt().min(h_max);
    let w = aspect * h;
    let scale = (w_max.min(col_w) / w).min(1.0);
    (w * scale, h * scale)
}

/// Bottom-anchor a `w`×`h` logo in `band`: its BOTTOM EDGE lands on the band's bottom edge — the
/// same line the text fallback puts its BASELINE on, because a logo's bottom edge and a baseline are
/// the same optical line. Overflow is upward and is paint, never layout.
pub fn place(band: Rect, w: f32, h: f32, align: HAlign) -> Rect {
    let x = match align {
        HAlign::Left | HAlign::Start => band.x,
        HAlign::Center => band.x + (band.w - w) * 0.5,
        HAlign::Right => band.x + band.w - w,
    };
    Rect::new(x, band.y + band.h - h, w, h)
}

/// The title in type at `rung`: the run a band draws when its logo is absent, ink
/// [`theme::TEXT_PRIMARY`]. THE one place the text fallback's size, weight, leading and line count
/// are decided, so [`HeroLogo::draw`] and a deck card that never draws a live logo (`Card`'s Type
/// rung, `card_art::needs_type_title`) cannot set the same title two ways. The caller adds the
/// alignment and the measure (`.h(..)`, `.with_measure(..)`) and places it, bottom-anchored.
pub fn title_text(rung: LogoRung, title: &str) -> TextView<'_> {
    title_text_inked(rung, title, theme::TEXT_PRIMARY)
}

/// [`title_text`] in another ink — the placeholder accounting's sentinel ink in [`HeroLogo::draw`].
fn title_text_inked(rung: LogoRung, title: &str, ink: [f32; 4]) -> TextView<'_> {
    let (_, _, _, _, sz) = rung.bounds();
    let lines = if sz > theme::size::TITLE { theme::hero::TITLE_LINES } else { 1 };
    TextView::new(title, sz, ink)
        .bold().leading(sz as f32 * theme::hero::TITLE_LEAD / theme::size::DISPLAY as f32).max_lines(lines)
}

/// The height the title run `tv` paints at in a column `band_w` wide: its measured height less the
/// leading under its last line, plus the cap band, so the run's LAST BASELINE is what a bottom
/// anchor lands on. Shared by [`HeroLogo::draw`]'s fallback and [`draw_title`].
fn title_paint_h(tv: &TextView<'_>, rung: LogoRung, band_w: f32, measure: &dyn Measure) -> f32 {
    let (_, _, _, _, sz) = rung.bounds();
    tv.measure_h(band_w) - (sz as f32 * theme::hero::TITLE_LEAD / theme::size::DISPLAY as f32) + measure.cap_h(sz)
}

/// **Draw a title in type in the slot a logo of `rung` would stand in** (`band`: its column, with
/// `band.y + band.h` the anchor line the run's last baseline lands on), at `align`. It is the
/// fallback of [`HeroLogo::draw`] and NOTHING else: no logo texture is looked up, so it is what a
/// Continue Watching card draws when neither Plex's cover nor our composite exists (`Card`'s Type
/// rung, `card_art::needs_type_title`), where a live logo must never go up.
pub fn draw_title(p: Painter, rung: LogoRung, title: &str, band: Rect, align: HAlign, measure: &dyn Measure) {
    let tv = title_text(rung, title).h(align.resolve(title)).with_measure(measure);
    let h = title_paint_h(&tv, rung, band.w, measure);
    tv.draw(p, Rect::new(band.x, band.y + band.h - h, band.w, h));
}

/// The title band itself: the item's clearLogo if it has resolved, else its title as text. Owning
/// both halves here is what makes the swap consistent across the three sites — and is why the two
/// fallbacks that used to paint an unbounded run now elide like home's always did.
pub struct HeroLogo<'a> {
    logo: &'a str,
    /// The raw id of the server that holds the logo. The same source path can name different
    /// images on different servers.
    sid: u16,
    title: &'a str,
    rung: LogoRung,
    align: HAlign,
}

impl<'a> HeroLogo<'a> {
    /// `logo` is a stamped clearLogo path or a legacy rating key on `sid`. An empty source
    /// draws `title` directly. An episode hero can name its show's logo.
    pub fn new(sid: u16, logo: &'a str, title: &'a str, rung: LogoRung) -> Self {
        Self {
            logo,
            sid,
            title,
            rung,
            align: HAlign::Left,
        }
    }
    /// Horizontal placement inside the band — the logo art and the text fallback share it, so the
    /// two cannot land on different edges.
    pub fn align(mut self, h: HAlign) -> Self {
        self.align = h;
        self
    }

    /// **The rect [`draw`](Self::draw) paints in `band`**: the logo art's own box where it has
    /// resolved, else the title's text run bottom-anchored on the band and as wide as its widest
    /// line ([`TextView::ink_span`]), not the column the run was wrapped into. The hero shade stands
    /// behind exactly this ([`crate::hero_shade`]), so a screen asks the logo what it is painting
    /// rather than guessing from the band, and it moves with the logo.
    pub fn painted(&self, band: Rect, measure: &dyn Measure) -> Rect {
        let align = self.align.resolve(self.title);
        if let Some((_, sw, sh)) = crate::tex::logo_src(self.sid, self.logo) {
            let (w, h) = fit(self.rung, sw, sh, band.w);
            return place(band, w, h, align);
        }
        let fallback = title_text(self.rung, self.title).h(align).with_measure(measure);
        let h = title_paint_h(&fallback, self.rung, band.w, measure);
        let (left, right) = fallback.ink_span(band);
        Rect::new(left, band.y + band.h - h, right - left, h)
    }

    /// Draw into `band` — `band.w` is the column the logo is contained to, `band.y + band.h` the
    /// anchor line, and `band.h` should be [`band_h`] of the same rung. Ink is [`theme::TEXT_PRIMARY`]
    /// for both the logo tint and the fallback text (the value all three sites passed already).
    /// Draws only through `p`, so the caller's cascade alpha fades logo and fallback identically.
    pub fn draw(&self, p: Painter, band: Rect, measure: &dyn Measure) {
        // Resolve the fallback before consulting the asynchronous texture cache. A recording and
        // its replay need the same measurement census even when the logo happened to be resident
        // in only one run; texture residency is paint state, not logical replay state.
        let align = self.align.resolve(self.title);
        let fallback = title_text(self.rung, self.title).h(align).with_measure(measure);
        let fallback_h = title_paint_h(&fallback, self.rung, band.w, measure);
        if let Some((tex, sw, sh)) = crate::tex::logo_src(self.sid, self.logo) {
            let (w, h) = fit(self.rung, sw, sh, band.w);
            // NOT pixel-snapped: a logo is SCALED content, and the crispness contract snaps
            // 1:1-texel content only (see the note above `theme.rs`'s size ladder).
            p.tex(tex, place(band, w, h, align), 0.0, theme::TEXT_PRIMARY);
            return;
        }
        // An item with a logo source that has not resolved draws its title as text, and that is
        // a WAIT only while the logo can still arrive: counted (and sentinel-inked) then. A logo the
        // store has settled as a miss (the 404 of an item with no clearLogo) will never come, so
        // the text is the answer: not counted, but reported as an absence so a driver whose
        // storyboard promises a logo can fail fast on it. A hero with no key at all has nothing to
        // wait for — its title is the whole answer.
        //
        // The settled-miss peek is a formatted key, the poster store's lock and a slot scan, and on
        // a real library most items have no clearLogo, so this branch runs every frame. Its answer
        // only picks between the two accounting calls below (and, in the sentinel build, the ink):
        // when nothing can see them, neither call records and both draw TEXT_PRIMARY, so skip it.
        let (p, ink) = if self.logo.is_empty() || !crate::placeholder::accounting_visible() {
            (p, theme::TEXT_PRIMARY)
        } else if crate::tex::logo_failed(self.sid, self.logo) {
            crate::placeholder::note_absent(p, crate::placeholder::Reason::HeroLogoAbsent, self.logo);
            (p, theme::TEXT_PRIMARY)
        } else {
            crate::placeholder::ink(p, crate::placeholder::Reason::HeroLogoText, self.logo, theme::TEXT_PRIMARY)
        };
        title_text_inked(self.rung, self.title, ink).h(align).with_measure(measure)
            .draw(p, Rect::new(band.x, band.y + band.h - fallback_h, band.w, fallback_h));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wide_logos_do_not_grow_to_the_old_height_floor() {
        let (w, h) = fit(LogoRung::Hero, 640.0, 100.0, 943.0);
        assert!(w <= 640.0, "wide wordmark is oversized: {w} x {h}");
        assert!(h < band_h(LogoRung::Hero), "paint must be free to be shorter than the title band");
        assert!(w * h <= 60_000.0 + 0.01, "the height floor must not inflate the area");
    }
    use LogoRung::{Card, Compact, Hero};

    // `fit`/`place`/`band_h` are pure f32 arithmetic over their arguments — no GL, no SDL_ttf, no
    // crate globals — so unlike the catalog-backed Home tests these are ordinary parallel ones, needing
    // neither `testlock::serial()` nor a module mutex. The texture arm of `HeroLogo::draw`
    // (`ui::tex::logo_src`) is deliberately NOT tested here: it reaches GL. The TEXT fallback is,
    // under `placeholder::capture_declared` (text only declares itself there and never reaches the
    // font): `the_text_fallback_*` below pin what the placeholder counter says about it.

    #[test]
    fn ordinary_wordmarks_share_one_area_across_columns() {
        for col in [660.0, 900.0, 1740.0] {
            let (w, h) = fit(Hero, 1500.0, 300.0, col);
            assert!((w * h - theme::logo::HERO_AREA).abs() < 0.02);
            assert!((w / h - 5.0).abs() < 1e-3);
            assert!(h < band_h(Hero));
        }
    }

    /// Comparable bounding-box area, with a small reduction where the square hits its height cap.
    #[test]
    fn a_square_logo_and_a_wordmark_have_comparable_bounded_area() {
        let (w1, h1) = fit(Hero, 800.0, 800.0, 900.0);
        let (w5, h5) = fit(Hero, 1500.0, 300.0, 900.0);
        assert!(
            h1 > h5,
            "a square must grow TALL to earn its area ({h1} vs {h5})"
        );
        let (a1, a5) = (w1 * h1, w5 * h5);
        assert!(
            (a1 - a5).abs() / a5 < 0.05,
            "the two occupy very different areas: {a1} vs {a5} px²"
        );
        // …and the regression guard against the height clamp coming back: the detail hero used to
        // draw a 1:1 emblem at 120×120.
        assert!(
            a1 > 3.0 * 120.0 * 120.0,
            "a square is back to a height-clamped size ({a1} px²)"
        );
    }

    #[test]
    fn either_axis_can_limit_area_without_distorting_the_logo() {
        let (w, h) = fit(Hero, 800.0, 100.0, 943.0);
        assert_eq!((w, h), (640.0, 80.0));
        let (w, h) = fit(Hero, 800.0, 100.0, 500.0);
        assert_eq!((w, h), (500.0, 62.5));
        let (w, h) = fit(Hero, 100.0, 200.0, 943.0);
        assert_eq!((w, h), (120.0, 240.0));
    }

    /// The property sweep: whatever the source, the fit stays inside its bounds and never distorts
    /// the mark — which no visual review would reliably catch on an unfamiliar logo.
    #[test]
    fn nothing_overflows_its_column_or_its_ceiling_and_aspect_is_preserved() {
        for rung in [Hero, Compact, Card] {
            let (_, _, h_max, w_max, _) = rung.bounds();
            for a in [
                0.25f32, 0.5, 0.75, 1.0, 1.5, 2.0, 3.0, 4.0, 5.0, 6.0, 8.0, 10.0, 12.0, 20.0,
            ] {
                for col_w in [660.0f32, 900.0, 1740.0] {
                    let (w, h) = fit(rung, a * 300.0, 300.0, col_w);
                    assert!(
                        w <= col_w.min(w_max) + 1e-3,
                        "{rung:?} a={a} overflowed its {col_w} column at {w}"
                    );
                    assert!(
                        h <= h_max + 1e-3,
                        "{rung:?} a={a} broke its {h_max} ceiling at {h}"
                    );
                    assert!(
                        h > 0.0 && h.is_finite() && w.is_finite(),
                        "{rung:?} a={a} → {w}×{h}"
                    );
                    assert!(
                        (w / h - a).abs() < 1e-3,
                        "{rung:?} a={a} was distorted to {}",
                        w / h
                    );
                }
            }
        }
    }

    /// The two rungs are one derivation, so they cannot silently desync into per-site tuning again.
    #[test]
    fn the_compact_rung_is_the_hero_rung_scaled() {
        let k = theme::logo::COMPACT_BAND_H / theme::logo::HERO_BAND_H;
        assert!(
            (theme::logo::COMPACT_AREA / theme::logo::HERO_AREA - k * k).abs() < 1e-3,
            "the compact area is no longer the hero area scaled by (54/120)²"
        );
        let hero_h = fit(Hero, 1500.0, 300.0, 900.0).1;
        let compact_h = fit(Compact, 1500.0, 300.0, 1740.0).1;
        assert!(
            (compact_h / hero_h - k).abs() < 1e-3,
            "an unconstrained wordmark must retain the shared compact scale"
        );
    }

    /// `fit` is public and a NaN would propagate into a `Rect` and blank the band without crashing —
    /// invisible on device, so it is pinned here instead.
    #[test]
    fn a_degenerate_source_is_a_size_not_a_panic() {
        for (sw, sh, col) in [
            (0.0f32, 0.0f32, 900.0f32),
            (10.0, 0.0, 900.0),
            (-5.0, 20.0, 900.0),
            (1500.0, 300.0, 0.0),
            (f32::NAN, 300.0, 943.0),
            (1500.0, f32::INFINITY, 943.0),
            (1500.0, 300.0, f32::NAN),
        ] {
            let (w, h) = fit(Hero, sw, sh, col);
            assert!(
                w.is_finite() && h.is_finite(),
                "{sw}×{sh} in {col} → {w}×{h}"
            );
            assert!(
                w >= 0.0 && h >= 0.0,
                "{sw}×{sh} in {col} → negative {w}×{h}"
            );
            assert!(
                h <= theme::logo::HERO_H_MAX + 1e-3,
                "{sw}×{sh} in {col} broke the ceiling at {h}"
            );
        }
    }

    /// The layout ≠ paint contract, which is the whole reason a landing texture does not reflow the
    /// hero: the band's BOTTOM is the anchor and a taller logo grows out of its top.
    #[test]
    fn a_taller_logo_spills_upward_only() {
        let band = Rect::new(96.0, 326.0, theme::hero::TEXT_W, band_h(Hero)); // home's three-line synopsis
        let (w, h) = fit(Hero, 1500.0, 300.0, band.w);
        let wide = place(band, w, h, HAlign::Left);
        let (w, h) = fit(Hero, 800.0, 800.0, band.w);
        let tall = place(band, w, h, HAlign::Left);
        assert_eq!(
            wide.y + wide.h,
            band.y + band.h,
            "the wide logo left the anchor line"
        );
        assert_eq!(
            tall.y + tall.h,
            band.y + band.h,
            "the tall logo left the anchor line"
        );
        assert_eq!(
            tall.y, 206.0,
            "the overflow must go UP out of the band, not down"
        );
        assert_eq!(
            wide.x, tall.x,
            "…and the left edge does not move with the height"
        );
    }

    /// The compact title is centred on the panel — identical to the `SCR_W * 0.5` anchor the old
    /// hand-placed draw used, now derived from the band instead.
    #[test]
    fn place_centers_the_compact_title_on_the_panel() {
        let band = Rect::new(90.0, 22.0, 1740.0, band_h(Compact));
        assert_eq!(place(band, 144.0, 72.0, HAlign::Center).cx(), 960.0);
        assert_eq!(place(band, 270.0, 54.0, HAlign::Center).cx(), 960.0);
    }

    /// Trivial, but it documents at test level that the band deliberately ignores the source — the
    /// signature already enforces it, and this is the assertion that fires if someone "helpfully"
    /// makes the band track the drawn height again.
    #[test]
    fn band_h_is_independent_of_the_drawn_height() {
        assert_eq!(band_h(Hero), theme::logo::HERO_BAND_H);
        assert_eq!(band_h(Compact), theme::logo::COMPACT_BAND_H);
        assert!(
            band_h(Hero) < fit(Hero, 800.0, 800.0, 900.0).1,
            "a square draws taller than its band"
        );
    }

    /// `LogoRung::lerp`'s two endpoints must reproduce the named rungs' own `bounds()` exactly —
    /// this is what makes it safe to call every frame of a transform that starts or ends AT a
    /// named rung (the detail hero's trailer-preview logo does both), not just safe mid-travel.
    #[test]
    fn lerp_at_its_endpoints_is_bit_identical_to_the_named_rungs() {
        for (src_w, src_h, col_w) in [
            (1500.0f32, 300.0f32, 900.0f32),
            (800.0, 800.0, 660.0),
            (600.0, 100.0, 1740.0),
        ] {
            let hero = fit(Hero, src_w, src_h, col_w);
            let at_0 = fit(LogoRung::lerp(Hero, Compact, 0.0), src_w, src_h, col_w);
            assert_eq!(at_0, hero, "t=0 must match LogoRung::Hero exactly");
            let compact = fit(Compact, src_w, src_h, col_w);
            let at_1 = fit(LogoRung::lerp(Hero, Compact, 1.0), src_w, src_h, col_w);
            assert_eq!(at_1, compact, "t=1 must match LogoRung::Compact exactly");
        }
    }

    /// Mid-travel: no NaN, no overflow past the wider of the two endpoints' ceilings, and the size
    /// moves monotonically as `t` sweeps 0..1 (the property a snap-at-0.5 implementation would
    /// break: this asserts a continuous shrink, not a jump).
    #[test]
    fn lerp_is_monotonic_and_bounded_across_the_whole_sweep() {
        let (src_w, src_h, col_w) = (1500.0f32, 300.0f32, 900.0f32);
        let mut prev_h = fit(LogoRung::lerp(Hero, Compact, 0.0), src_w, src_h, col_w).1;
        for i in 1..=10 {
            let t = i as f32 / 10.0;
            let (w, h) = fit(LogoRung::lerp(Hero, Compact, t), src_w, src_h, col_w);
            assert!(w.is_finite() && h.is_finite(), "t={t} produced {w}x{h}");
            assert!(h <= theme::logo::HERO_H_MAX + 1e-3, "t={t} broke the wider ceiling at {h}");
            assert!(h <= prev_h + 1e-3, "t={t} grew from {prev_h} to {h}, not monotonic shrinking");
            prev_h = h;
        }
        assert!(
            (prev_h - theme::logo::COMPACT_H_MAX.min(prev_h + 1e-3)).abs() < 5.0
                || prev_h <= theme::logo::COMPACT_H_MAX + 1e-3,
            "t=1 must land at/under the Compact ceiling, got {prev_h}"
        );
    }

    /// `lerp` clamps `t` outside `[0, 1]` rather than extrapolating past either endpoint — a
    /// spring can overshoot its target for a frame (this app's springs are critically damped and
    /// should not, but a caller passing a raw un-clamped progress value is an easy mistake).
    #[test]
    fn lerp_clamps_t_outside_zero_one() {
        let (src_w, src_h, col_w) = (1500.0f32, 300.0f32, 900.0f32);
        let below = fit(LogoRung::lerp(Hero, Compact, -0.5), src_w, src_h, col_w);
        let at_0 = fit(LogoRung::lerp(Hero, Compact, 0.0), src_w, src_h, col_w);
        assert_eq!(below, at_0, "t<0 must clamp to t=0");
        let above = fit(LogoRung::lerp(Hero, Compact, 1.5), src_w, src_h, col_w);
        let at_1 = fit(LogoRung::lerp(Hero, Compact, 1.0), src_w, src_h, col_w);
        assert_eq!(above, at_1, "t>1 must clamp to t=1");
    }

    /// The card rung's slot is its own height ceiling, and its logo is the constant-area rule's.
    #[test]
    fn the_card_rung_is_a_constant_area_inside_its_slot() {
        assert_eq!(band_h(Card), theme::logo::CARD_H_MAX);
        let (w, h) = fit(Card, 1500.0, 300.0, 420.0);
        assert!((w * h - theme::logo::CARD_AREA).abs() < 0.02, "{w}x{h}");
        assert!(h <= band_h(Card), "an ordinary wordmark sits inside the slot");
        assert_eq!(fit(Card, 100.0, 100.0, 420.0), (theme::logo::CARD_H_MAX, theme::logo::CARD_H_MAX), "a square clamps at the ceiling");
    }

    /// The deck card's Type rung sets its title with `title_text`; it must be, run for run, what
    /// `HeroLogo::draw` falls back to, at every rung that has a fallback.
    #[test]
    fn title_text_is_what_the_hero_logo_falls_back_to() {
        install_store(true);
        let measure = crate::fixture::FixtureMeasure;
        for rung in [Hero, Compact, Card] {
            for title in ["A Title", "The Unbearably Long Name of the Last House at the Edge of the World"] {
                let band = Rect::new(96.0, 446.0, 400.0, band_h(rung));
                let drawn = crate::draw_census::capture_runs(|| {
                    HeroLogo::new(0, "", title, rung).draw(Painter::recording(), band, &measure);
                });
                let tv = title_text(rung, title).h(HAlign::Left.resolve(title)).with_measure(&measure);
                let (_, _, _, _, sz) = rung.bounds();
                let h = tv.measure_h(band.w) - tv.line_h() + measure.cap_h(sz);
                let direct = crate::draw_census::capture_runs(|| {
                    tv.draw(Painter::recording(), Rect::new(band.x, band.y + band.h - h, band.w, h));
                });
                assert!(!drawn.is_empty(), "{rung:?}: nothing drawn");
                assert_eq!(drawn, direct, "{rung:?} {title}");
            }
        }
    }

    /// `draw_title` is the hero logo's text fallback, run for run, and it looks no logo up.
    #[test]
    fn draw_title_is_the_fallback_and_never_a_logo() {
        install_store(false);
        let measure = crate::fixture::FixtureMeasure;
        for rung in [Hero, Compact, Card] {
            let band = Rect::new(96.0, 446.0, 400.0, band_h(rung));
            let drawn = |f: &dyn Fn()| crate::draw_census::capture_runs(f);
            let fallback = drawn(&|| HeroLogo::new(0, "", "A Title", rung).draw(Painter::recording(), band, &measure));
            let title = drawn(&|| draw_title(Painter::recording(), rung, "A Title", band, HAlign::Left, &measure));
            assert!(!title.is_empty(), "{rung:?}: nothing drawn");
            assert_eq!(title, fallback, "{rung:?}");
        }
        let band = Rect::new(0.0, 0.0, 400.0, band_h(Card));
        let runs = crate::draw_census::capture_runs(|| draw_title(Painter::recording(), Card, "A Title", band, HAlign::Center, &measure));
        assert_eq!(runs.len(), 1, "one title");
        assert_eq!(runs[0].text, "A Title");
    }

    // ---- the placeholder count of the text fallback -------------------------------------------

    /// A clearLogo source with one scripted answer: never resident (`logo` is `None`), and
    /// `failed` says whether the store has SETTLED the fetch as a miss. `peeks` counts how often
    /// the settled-miss peek was asked: on the TV it is a formatted key, the poster store's lock and
    /// a scan of every slot.
    struct LogoStore {
        failed: bool,
        peeks: &'static std::cell::Cell<u32>,
    }
    impl crate::tex::Source for LogoStore {
        fn probe(&self, _: u16, _: &str, _: i32, _: i32, _: bool) -> Option<plx_machine::machine::PosterKey> {
            None
        }
        fn warm(&self, _: u16, _: &str, _: i32, _: i32, _: bool) -> crate::tex::Warm {
            crate::tex::Warm::Known
        }
        fn logo(&self, _: u16, _: &str) -> Option<plx_machine::machine::PosterKey> {
            None
        }
        fn logo_warm(&self, _: u16, _: &str) -> crate::tex::Warm {
            crate::tex::Warm::Known
        }
        fn logo_failed(&self, _: u16, _: &str) -> bool {
            self.peeks.set(self.peeks.get() + 1);
            self.failed
        }
        fn unresident(&self, _: plx_machine::machine::PosterKey, _: bool) {}
        fn idle(&self) -> bool {
            true
        }
    }

    /// Install a store in the given state and return its peek counter. The source is thread-local
    /// and a libtest test owns its thread, so installing one here cannot reach another test.
    fn install_store(failed: bool) -> &'static std::cell::Cell<u32> {
        let peeks: &'static std::cell::Cell<u32> = Box::leak(Box::new(std::cell::Cell::new(0)));
        crate::tex::install(Box::leak(Box::new(LogoStore { failed, peeks })));
        peeks
    }

    /// Draw the fallback with the store in the given state, ARMED.
    fn drawn(rk: &str, failed: bool) -> crate::placeholder::Frame {
        install_store(failed);
        let band = Rect::new(0.0, 0.0, 900.0, 120.0);
        crate::placeholder::capture_declared(|| {
            HeroLogo::new(0, rk, "A Title", Hero).draw(Painter::root(), band, &crate::fixture::FixtureMeasure)
        })
        .1
    }

    /// The settled-miss peek costs a lock and a slot scan per frame, and its answer only chooses
    /// between two placeholder-accounting calls (and, in the sentinel build, the ink). With nobody
    /// counting, which is every shipping frame, it is not asked; with a driver armed it is, once.
    #[test]
    fn the_settled_miss_peek_is_asked_only_when_placeholder_accounting_can_see_it() {
        let band = Rect::new(0.0, 0.0, 900.0, 120.0);
        for failed in [false, true] {
            let peeks = install_store(failed);
            crate::placeholder::declared(|| {
                HeroLogo::new(0, "42", "A Title", Hero).draw(Painter::root(), band, &crate::fixture::FixtureMeasure)
            });
            let unarmed = if crate::placeholder::SENTINEL { 1 } else { 0 };
            assert_eq!(peeks.get(), unarmed, "unarmed, failed={failed}");

            let peeks = install_store(failed);
            crate::placeholder::capture_declared(|| {
                HeroLogo::new(0, "42", "A Title", Hero).draw(Painter::root(), band, &crate::fixture::FixtureMeasure)
            });
            assert_eq!(peeks.get(), 1, "armed, failed={failed}");
        }
    }

    #[test]
    fn pdf_proposal_long_title_wraps_to_two_display_lines_within_the_column() {
        install_store(true);
        let title = "The Unbearably Long Name of the Last House at the Edge of the World";
        let band = Rect::new(96.0, 446.0, theme::hero::TEXT_W, band_h(Hero));
        let measure = crate::fixture::FixtureMeasure;
        let runs = crate::draw_census::capture_runs(|| {
            HeroLogo::new(0, "", title, Hero).draw(Painter::recording(), band, &measure);
        });
        assert_eq!(runs.len(), 2, "the complete fallback has two lines: {runs:?}");
        for run in &runs {
            assert_eq!(run.size, theme::size::DISPLAY);
            assert!(run.at.x + run.at.w <= band.x + band.w + 0.01);
        }
        assert!(runs[1].at.y > runs[0].at.y);
    }

    /// **A typed title is painted as wide as its widest line**, not as wide as the column it wraps
    /// into: the shade stands behind this rect, and a column-wide one would be a slab. It starts where
    /// the title's alignment starts it, and a title that wraps still fits its column.
    #[test]
    fn a_typed_title_is_painted_as_wide_as_its_widest_line() {
        use plx_machine::machine::Measure;
        install_store(true);
        let m = crate::fixture::FixtureMeasure;
        let band = Rect::new(96.0, 446.0, theme::hero::TEXT_W, band_h(Hero));
        let (_, _, _, _, sz) = Hero.bounds();
        let width = m.width_str("Hollow Coast", sz, true);
        assert!(width < band.w * 0.8, "the premise: this title is well inside its column ({width})");
        let left = HeroLogo::new(0, "", "Hollow Coast", Hero).painted(band, &m);
        assert_eq!((left.x, left.w), (band.x, width), "left-aligned: from the column edge, its own width");
        let right = HeroLogo::new(0, "", "Hollow Coast", Hero).align(HAlign::Right).painted(band, &m);
        assert_eq!((right.w, right.x + right.w), (width, band.x + band.w), "right-aligned: ending at the column's edge");
        assert_eq!((left.y, left.h), (right.y, right.h), "the alignment moves it sideways only");
        let long = HeroLogo::new(0, "", "The Unbearably Long Name of the Last House at the Edge of the World", Hero)
            .painted(band, &m);
        assert!(long.x >= band.x && long.x + long.w <= band.x + band.w + 0.01, "a wrapped title stays in its column: {long:?}");
        assert!(long.h > left.h, "…and is taller than the one-line title");
    }

    /// The logo is on its way: the hero shows its title as text in the meantime, and that is a wait.
    #[test]
    fn the_text_fallback_counts_while_the_clearlogo_is_still_loading() {
        let f = drawn("42", false);
        assert_eq!((f.count, f.of(crate::placeholder::Reason::HeroLogoText)), (1, 1), "{f:?}");
        assert_eq!(f.entries[0].key, "42");
        assert!(f.absent.is_empty(), "{f:?}");
    }

    /// A 404 parks the slot as failed and nothing will ever arrive: the title as text is the answer,
    /// not a wait. It is not silent either: it is reported as an absence, for a driver whose
    /// storyboard promises a logo to fail fast on.
    #[test]
    fn the_text_fallback_of_a_logo_that_will_never_come_is_an_absence_not_a_wait() {
        let f = drawn("42", true);
        assert_eq!(f.count, 0, "an absent logo must not hold the run for ever: {f:?}");
        assert_eq!(f.absent.len(), 1, "{f:?}");
        assert_eq!((f.absent[0].reason, f.absent[0].key.as_str()), (crate::placeholder::Reason::HeroLogoAbsent, "42"));
    }

    /// A hero with no ratingKey has nothing to wait for and nothing to report.
    #[test]
    fn the_text_fallback_of_a_hero_with_no_key_is_neither() {
        let f = drawn("", false);
        assert_eq!(f, crate::placeholder::Frame::default());
    }

    /// A text-prewarm pass draws nothing, so it counts and reports nothing.
    #[test]
    fn a_recording_pass_over_the_hero_logo_counts_nothing() {
        install_store(false);
        let band = Rect::new(0.0, 0.0, 900.0, 120.0);
        let f = crate::placeholder::capture_declared(|| {
            HeroLogo::new(0, "42", "A Title", Hero).draw(Painter::recording(), band, &crate::fixture::FixtureMeasure)
        })
        .1;
        assert_eq!(f, crate::placeholder::Frame::default());
    }
}
