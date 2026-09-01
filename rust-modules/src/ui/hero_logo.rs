//! `HeroLogo` — the ONE way a clearLogo is sized, placed, and fallen back from.
//!
//! Three screens draw an item's clearLogo with a text title behind it: the home hero, the detail
//! hero, and the detail page's pinned compact title. They used to pass their own literal bounds to
//! `posters::logo_tex` (660×96 / 680×120 / ∞×54), which made the same mark three sizes in one app —
//! and, because every one of those boxes is far wider than it is tall, made all three a pure HEIGHT
//! clamp. Under a height clamp the drawn area is linear in aspect, so a 5:1 wordmark covered five
//! times the ink of a 1:1 emblem and square logos read as an afterthought.
//!
//! The rule is now CONSTANT AREA, bounded (see [`theme::logo`]): solve `h` from `w·h = AREA`, clamp
//! `h` into the rung's floor/ceiling, then contain into the caller's column. A square grows tall, a
//! wordmark grows wide, both carry the same weight.
//!
//! **Layout ≠ paint** (`label.rs`'s rule, applied to art): the band a caller reserves in its flow is
//! [`band_h`] — a CONSTANT, the rung's floor — never the drawn height. A taller logo spills UPWARD
//! out of the band as pure paint. That is what keeps a bottom-anchored hero column from lurching the
//! moment a texture lands, and what keeps two items with different logo aspects level through a hero
//! flip. It also means a hero logo now paints ABOVE the hero scrim's knee
//! (`widgets::HERO_SCRIM_KNEE`) — up to y≈260 on home — where the wedge is still feathering in; the
//! clearance that matters there is the top chrome's (`widgets::TOP_BAR_BOTTOM`), asserted in
//! `home.rs`'s `the_home_hero_logo_never_reaches_the_top_bar`.
//!
//! There is deliberately no fade-in behind the text→logo swap: `posters::logo_src` answers `None`
//! both while the fetch is pending AND when the item simply has no clearLogo, so a cross-fade would
//! need the store to distinguish `P_FAILED`/absent from `P_WANT`/`P_LOADING` first. The swap is a
//! cut, but nothing around it MOVES, which is the part that used to read as a glitch.
use crate::ui::label::{HAlign, Label, VAlign};
use crate::ui::{theme, Painter, Rect};
use std::os::raw::c_int;

/// Which presence rung a logo is drawn at. Both HEROES share one rung — the "mutual logic for logo
/// size for all hero" directive — and the pinned compact title is the only other one. A new site
/// picks a rung; it does not invent bounds.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum LogoRung {
    /// The full-bleed hero title band — home's sliding column and the detail page's title block.
    Hero,
    /// The detail page's pinned compact title, the strip the hero scrolls up into.
    Compact,
}

impl LogoRung {
    /// `(target area px², height floor, height ceiling, the text fallback's size rung)`.
    /// One match so the four numbers of a rung can never be read from four different places.
    fn bounds(self) -> (f32, f32, f32, c_int) {
        match self {
            LogoRung::Hero => (
                theme::logo::HERO_AREA,
                theme::logo::HERO_H_MIN,
                theme::logo::HERO_H_MAX,
                theme::size::HERO,
            ),
            LogoRung::Compact => (
                theme::logo::COMPACT_AREA,
                theme::logo::COMPACT_H_MIN,
                theme::logo::COMPACT_H_MAX,
                theme::size::TITLE,
            ),
        }
    }
}

/// The LAYOUT height of a logo's title band — the rung's floor, constant, independent of the source
/// (which is why it takes no source argument). A caller's flow reserves exactly this; a taller logo
/// overflows upward as paint. See the module docs for why that split is the whole point.
pub fn band_h(rung: LogoRung) -> f32 {
    rung.bounds().1
}

/// The DRAWN size of a `src_w`×`src_h` logo at `rung` inside a `col_w`-wide column.
/// Pure — no GL, no font, no globals; this is the host-testable core of the sizing rule.
///
/// The three clamps apply in a fixed ORDER and it is load-bearing: constant area first (that is the
/// rule), then the rung's floor/ceiling (a very wide mark must not thin to a pinstripe, a square must
/// not swallow the hero), then the caller's column LAST — the column is the one bound allowed to
/// break the floor, because drawing past the column is never acceptable and drawing short is.
pub fn fit(rung: LogoRung, src_w: f32, src_h: f32, col_w: f32) -> (f32, f32) {
    let (area, h_min, h_max, _) = rung.bounds();
    // A degenerate source has no aspect to solve for. `posters::logo_src` already rejects one, but
    // `fit` is public and a 0.0/0.0 would propagate a NaN into a `Rect` — which blanks the band
    // silently rather than crashing, i.e. is invisible until someone looks at a television.
    if !(src_w > 0.0) || !(src_h > 0.0) {
        return (h_min.min(col_w.max(0.0)), h_min);
    }
    let a = src_w / src_h;
    let mut h = (area / a).sqrt().clamp(h_min, h_max);
    let mut w = a * h;
    if col_w > 0.0 && w > col_w {
        h *= col_w / w;
        w = col_w;
    }
    (w, h)
}

/// Bottom-anchor a `w`×`h` logo in `band`: its BOTTOM EDGE lands on the band's bottom edge — the
/// same line the text fallback puts its BASELINE on, because a logo's bottom edge and a baseline are
/// the same optical line. Overflow is upward and is paint, never layout.
pub fn place(band: Rect, w: f32, h: f32, align: HAlign) -> Rect {
    let x = match align {
        HAlign::Left => band.x,
        HAlign::Center => band.x + (band.w - w) * 0.5,
        HAlign::Right => band.x + band.w - w,
    };
    Rect::new(x, band.y + band.h - h, w, h)
}

/// The title band itself: the item's clearLogo if it has resolved, else its title as text. Owning
/// both halves here is what makes the swap consistent across the three sites — and is why the two
/// fallbacks that used to paint an unbounded run now elide like home's always did.
pub struct HeroLogo<'a> {
    rk: &'a str,
    /// The server `rk` is a key on. A clearLogo is fetched through the image transcoder of the
    /// server that holds the item, and `rk` names an item on that machine alone — resolving it
    /// against whichever server is current drew no logo at all for a borrowed hero, which is how
    /// this was reported ("detail page does not show logo image").
    sid: crate::plex::ServerId,
    title: &'a str,
    rung: LogoRung,
    align: HAlign,
}

impl<'a> HeroLogo<'a> {
    /// `rk` is the ratingKey whose clearLogo to draw — for an episode hero that is the SHOW's
    /// (episodes have no clearLogo asset; keying to the episode 404s straight into a text fallback
    /// of the wrong title). `title` is the run drawn when there is no logo.
    pub fn new(sid: crate::plex::ServerId, rk: &'a str, title: &'a str, rung: LogoRung) -> Self {
        Self {
            rk,
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

    /// Draw into `band` — `band.w` is the column the logo is contained to, `band.y + band.h` the
    /// anchor line, and `band.h` should be [`band_h`] of the same rung. Ink is [`theme::TEXT_PRIMARY`]
    /// for both the logo tint and the fallback text (the value all three sites passed already).
    /// Draws only through `p`, so the caller's cascade alpha fades logo and fallback identically.
    pub fn draw(&self, p: Painter, band: Rect) {
        // The hero draws the item the shelf under it has focused, and that shelf is the server
        // being browsed — so its clearLogo is asked of the current server. An item that came from
        // somewhere else (a merged Continue Watching row) names its own, once items carry one.
        if let Some((tex, sw, sh)) = crate::posters::logo_src(self.sid, self.rk) {
            let (w, h) = fit(self.rung, sw, sh, band.w);
            // NOT pixel-snapped: a logo is SCALED content, and the crispness contract snaps
            // 1:1-texel content only (see the note above `theme.rs`'s size ladder).
            p.tex(tex, place(band, w, h, self.align), 0.0, theme::TEXT_PRIMARY);
            return;
        }
        let (_, _, _, sz) = self.rung.bounds();
        let line = crate::text::elide(self.title, band.w, sz, 1, false);
        // `cs` must outlive the draw — `Label` holds a non-owning pointer (ui/CLAUDE.md's first gotcha)
        let Ok(cs) = std::ffi::CString::new(line) else {
            return;
        };
        Label::new(cs.as_ptr(), sz, theme::TEXT_PRIMARY)
            .bold()
            .h(self.align)
            .v(VAlign::Baseline)
            .draw(p, band);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use LogoRung::{Compact, Hero};

    // `fit`/`place`/`band_h` are pure f32 arithmetic over their arguments — no GL, no SDL_ttf, no
    // crate globals — so unlike `ui/home.rs`'s tests these are ordinary parallel ones, needing
    // neither `testlock::serial()` nor a module mutex. Everything downstream (`HeroLogo::draw` →
    // `posters::logo_src` + `text::elide`) is deliberately NOT tested here: it reaches GL and the
    // font, and `text_cap_band`'s host fallback would measure the fallback rather than the device
    // font, which is worse than no test.

    /// The whole low-risk claim in one assertion: the commonest logo shape does not move. If someone
    /// retunes [`theme::logo::HERO_AREA`] without meaning to shift wordmarks, this fires.
    #[test]
    fn a_wide_wordmark_is_the_anchor_and_does_not_move() {
        let (w, h) = fit(Hero, 1500.0, 300.0, 900.0); // detail's column
        assert!(
            (w - 600.0).abs() < 1e-3 && (h - 120.0).abs() < 1e-3,
            "detail hero wordmark: {w}×{h}"
        );
        let (w, h) = fit(Hero, 1500.0, 300.0, 660.0); // home's, narrower — and still not binding
        assert!(
            (w - 600.0).abs() < 1e-3 && (h - 120.0).abs() < 1e-3,
            "home hero wordmark: {w}×{h}"
        );
        let (w, h) = fit(Compact, 1500.0, 300.0, 1740.0);
        assert!(
            (w - 270.0).abs() < 1e-3 && (h - 54.0).abs() < 1e-3,
            "compact wordmark: {w}×{h}"
        );
    }

    /// The nit itself, pinned: a square emblem and a wordmark are the same amount of brand.
    #[test]
    fn a_square_logo_carries_the_same_ink_as_a_wordmark() {
        let (w1, h1) = fit(Hero, 800.0, 800.0, 900.0);
        let (w5, h5) = fit(Hero, 1500.0, 300.0, 900.0);
        assert!(
            h1 > h5,
            "a square must grow TALL to earn its area ({h1} vs {h5})"
        );
        let (a1, a5) = (w1 * h1, w5 * h5);
        assert!(
            (a1 - a5).abs() / a5 < 0.02,
            "the two carry different ink: {a1} vs {a5} px²"
        );
        // …and the regression guard against the height clamp coming back: the detail hero used to
        // draw a 1:1 emblem at 120×120.
        assert!(
            a1 > 3.0 * 120.0 * 120.0,
            "a square is back to a height-clamped size ({a1} px²)"
        );
    }

    /// The ORDER of the three clamps — the one part of `fit` that is easy to get wrong, and whose
    /// failure (a logo wider than its column) is a painted-over layout, not a panic.
    #[test]
    fn the_floor_holds_until_the_column_binds() {
        // 6:1: the area rule wants h=109.5, the floor raises it to 120 and the column has room
        let (w, h) = fit(Hero, 600.0, 100.0, 900.0);
        assert!(
            (w - 720.0).abs() < 1e-3 && (h - 120.0).abs() < 1e-3,
            "{w}×{h}"
        );
        // the same source in home's narrower column: the column is allowed to break the floor
        let (w, h) = fit(Hero, 600.0, 100.0, 660.0);
        assert!(
            (w - 660.0).abs() < 1e-3 && (h - 110.0).abs() < 1e-3,
            "{w}×{h}"
        );
    }

    /// The property sweep: whatever the source, the fit stays inside its bounds and never distorts
    /// the mark — which no visual review would reliably catch on an unfamiliar logo.
    #[test]
    fn nothing_overflows_its_column_or_its_ceiling_and_aspect_is_preserved() {
        for rung in [Hero, Compact] {
            let (_, _, h_max, _) = rung.bounds();
            for a in [
                0.5f32, 0.75, 1.0, 1.5, 2.0, 3.0, 4.0, 5.0, 6.0, 8.0, 10.0, 12.0,
            ] {
                for col_w in [660.0f32, 900.0, 1740.0] {
                    let (w, h) = fit(rung, a * 300.0, 300.0, col_w);
                    assert!(
                        w <= col_w + 1e-3,
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
        let k = theme::logo::COMPACT_H_MIN / theme::logo::HERO_H_MIN;
        assert!(
            (theme::logo::COMPACT_AREA / theme::logo::HERO_AREA - k * k).abs() < 1e-3,
            "the compact area is no longer the hero area scaled by (54/120)²"
        );
        let hero_h = fit(Hero, 1500.0, 300.0, 900.0).1;
        let compact_h = fit(Compact, 1500.0, 300.0, 1740.0).1;
        assert!(
            (compact_h / hero_h - k).abs() < 1e-3,
            "a 5:1 mark must land on the floor at BOTH rungs"
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
        let band = Rect::new(90.0, 408.0, 660.0, band_h(Hero)); // home's worst-case stack
        let floor = place(band, 600.0, 120.0, HAlign::Left);
        let tall = place(band, 268.0, 268.0, HAlign::Left);
        assert_eq!(
            floor.y + floor.h,
            band.y + band.h,
            "the floor-height logo left the anchor line"
        );
        assert_eq!(
            tall.y + tall.h,
            band.y + band.h,
            "the tall logo left the anchor line"
        );
        assert_eq!(
            tall.y, 260.0,
            "the overflow must go UP out of the band, not down"
        );
        assert_eq!(
            floor.x, tall.x,
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
    fn band_h_is_the_floor_not_the_drawn_height() {
        assert_eq!(band_h(Hero), theme::logo::HERO_H_MIN);
        assert_eq!(band_h(Compact), theme::logo::COMPACT_H_MIN);
        assert!(
            band_h(Hero) < fit(Hero, 800.0, 800.0, 900.0).1,
            "a square draws taller than its band"
        );
    }
}
