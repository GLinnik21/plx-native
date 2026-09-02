//! Shared full-screen route geometry used by Settings and its children.
//!
//! A route screen is a two-column composition, not a bag of coordinates: a narrative column on
//! the left, a content column on the right, and one bottom action row. Screens supply words and
//! state; this module owns every spatial relationship between them.
//!
//! # Where BACK goes is the CRUMB, never a hint in the action band
//!
//! A route in this family that was arrived at from somewhere says so on one caption line above
//! its title: a left-pointing chevron and the name of the place BACK returns to (see
//! [`RouteLayout::draw_narrative`]). That line replaced the `Press [BACK] to return` hint this
//! family used to draw in the bottom action row, and the trade is the point rather than a tidy-up
//! — the hint spent a whole 60px band restating a key the remote already has, and it could not
//! say where the key WENT, which on a three-deep push (Settings → Legal notices → a document) is
//! the only part anybody needs. Freeing the band is what lets first-run consent put its two
//! answers there ([`RouteLayout::action_pair`]) instead of hiding them among the readable rows of
//! its list.
//!
//! **The rule for `None` is "BACK does not go anywhere inside the app", not "the Settings root".**
//! Three routes qualify today and a hard count here would be the fourth transcription of a number
//! this project keeps letting rot — take the census with
//! `git grep -A2 'draw_narrative(' -- 'rust-modules/src/ui/*.rs'`. They are the Settings modal
//! (BACK leaves the family), the FIRST first-run consent question (sign-in is behind it and
//! cannot be undone) and the QR sign-in itself (there is no app behind it yet).

use crate::ui::consts::SAFE;
use crate::ui::icons::{self, Icon};
use crate::ui::text_view::TextView;
use crate::ui::widgets::{AmbientWash, ControlPalette};
use crate::ui::{theme, Painter, Rect, Spring};

/// The left column is the same editorial measure as the Home hero.  Reusing that named measure is
/// what makes first-run routes and Settings feel like one family instead of two similar layouts.
const NARRATIVE_W: f32 = crate::ui::home::HERO_COL_W;
/// Two visually separate columns need a region gap, not a row gap.  Expressed entirely on the
/// spacing ladder so a retune of that ladder moves every route together.
const COLUMN_GAP: f32 = theme::space::XL * 2.0 + theme::space::LG + theme::space::SM;
const TOP_INSET: f32 = theme::space::XL + theme::space::MD;
const ACTION_H: f32 = crate::ui::widgets::StatusOverlay::CTRL_H;
/// The crumb's mark, drawn at the smallest rung a mark is worn at anywhere in the product. It is
/// a return affordance rather than a heading, so it may not outweigh the caption beside it.
const CRUMB_MARK: f32 = theme::size::MICRO as f32;
/// The crumb LINE's height — the mark's, because the mark is the taller of the two things on it
/// (a `CAPTION` cap band is a few px shorter) and both are centred in it.
///
/// A constant rather than a measured cap height, and that is not a shortcut: `text::cap_h` opens
/// the font through SDL2_ttf, which the host suite cannot LINK, so a measured band would make
/// every route's vertical flow ungradeable off-device — the boundary `ui/CLAUDE.md` records as
/// stopping the whole suite linking rather than skipping one test.
const CRUMB_BAND: f32 = CRUMB_MARK;
const PUSH_K: f32 = 200.0;
const PARENT_TRAVEL: f32 = 0.35;
const CHILD_LEAD: f32 = 0.22;

/// Density of the Settings-family ambient ground.
///
/// The source is already an UltraBlur envelope (or four broad framebuffer means), so increasing
/// this number does not make it *more blurred*; it only lets more source light through.  Reusing
/// the shared ground weight keeps bright green/yellow artwork below the section-label contrast
/// floor while preserving its hue.
const GROUND_W: f32 = AmbientWash::GROUND_W;

/// The fixed ground under a Settings-family route.
///
/// It deliberately stores a four-corner colour envelope rather than a downsampled screenshot.
/// That is effectively a blur with a support wider than the screen: title glyphs, faces and poster
/// edges cannot survive it, but the host artwork's light still does.  Once latched it never samples
/// again; nested push/back transitions therefore move content over one stationary ground.
#[derive(Clone, Copy)]
pub(crate) struct RouteGround {
    wash: AmbientWash,
    key: [f32; 3],
    latched: bool,
}

impl RouteGround {
    pub(crate) const fn new() -> Self {
        Self {
            wash: AmbientWash::flat(theme::SURFACE_APP),
            key: [
                theme::SURFACE_APP[0],
                theme::SURFACE_APP[1],
                theme::SURFACE_APP[2],
            ],
            latched: false,
        }
    }

    pub(crate) fn reset(&mut self) {
        self.wash.jump([theme::SURFACE_APP; 4]);
        self.key = [
            theme::SURFACE_APP[0],
            theme::SURFACE_APP[1],
            theme::SURFACE_APP[2],
        ];
        self.latched = false;
    }

    fn latch(&mut self, corners: [[f32; 3]; 4], key: [f32; 3]) {
        if self.latched {
            return;
        }
        self.wash.jump(AmbientWash::keyed(corners, [GROUND_W; 4]));
        self.key = key;
        self.latched = true;
    }

    fn latch_target(&mut self, target: [[f32; 4]; 4]) {
        if self.latched {
            return;
        }
        self.wash.jump(target);
        self.key = mean_target_key(target);
        self.latched = true;
    }

    /// Freeze the page that was already drawn this frame. Its one caller is the Settings modal,
    /// which opens over Home; first-run consent takes [`Self::draw_home`] instead, because since
    /// the device question moved ahead of the profile picker it usually has no rendered host to
    /// sample at all.
    pub(crate) fn draw_host(&mut self, p: Painter) {
        if !self.latched {
            let sample = crate::gfx::sample_modal_ambient();
            self.latch(sample.corners, sample.key);
        }
        self.wash.draw(p, Rect::FULL);
    }

    /// Seed a pre-Home route from the same hero metadata Home will use when it appears. Shared
    /// Sources has no rendered host to sample yet, so this is the semantic equivalent of freezing
    /// Home after an infinitely broad blur. If artwork has not landed, the app surface is the
    /// honest fallback and remains frozen for this visit.
    pub(crate) fn draw_home(&mut self, p: Painter) {
        if !self.latched {
            if let Some(hero) = crate::ui::home::hero_item().filter(|m| m.has_blur) {
                self.latch(hero.blur, mean_key(hero.blur));
            } else {
                self.latch_target(theme::ROUTE_GROUND_FALLBACK);
            }
        }
        self.wash.draw(p, Rect::FULL);
    }

    /// Draw a pre-content route on the product's authored fallback atmosphere.
    ///
    /// Login and the profile ceremony have no Home frame and no media item to sample.  They still
    /// belong to the same route family, so they take the same broad graphite/amber envelope as an
    /// artwork-less first-run screen instead of inventing another flat background locally.
    pub(crate) fn draw_default(&mut self, p: Painter) {
        if !self.latched {
            self.latch_target(theme::ROUTE_GROUND_FALLBACK);
        }
        self.wash.draw(p, Rect::FULL);
    }

    pub(crate) fn palette(&self) -> ControlPalette {
        ControlPalette::ambient(self.key)
    }

    pub(crate) fn is_latched(&self) -> bool {
        self.latched
    }
}

fn mean_key(corners: [[f32; 3]; 4]) -> [f32; 3] {
    let mut key = [0.0; 3];
    for corner in corners {
        for channel in 0..3 {
            key[channel] += corner[channel] * 0.25;
        }
    }
    key
}

fn mean_target_key(corners: [[f32; 4]; 4]) -> [f32; 3] {
    mean_key(corners.map(|c| [c[0], c[1], c[2]]))
}

/// The one nested-route transition used by Settings documents.
///
/// Only content moves: the host ground is drawn outside these painters and therefore remains
/// fixed. The parent exits left while fading; the child leads from the right while appearing.
/// Reversing `open` produces the exact inverse spring, so BACK never invents a second motion.
pub(crate) struct RoutePush {
    progress: Spring,
}

impl RoutePush {
    pub(crate) const fn new() -> Self {
        Self {
            progress: Spring::at(0.0),
        }
    }

    pub(crate) fn jump(&mut self, open: bool) {
        self.progress.jump(if open { 1.0 } else { 0.0 });
    }

    pub(crate) fn update(&mut self, open: bool, dt: f32) {
        self.progress.step(if open { 1.0 } else { 0.0 }, PUSH_K, dt);
    }

    pub(crate) fn amount(&self) -> f32 {
        self.progress.pos.clamp(0.0, 1.0)
    }

    pub(crate) fn parent(&self, p: Painter) -> Painter {
        let t = self.amount();
        p.alpha(1.0 - t)
            .translate(-PARENT_TRAVEL * Rect::FULL.w * t, 0.0)
    }

    pub(crate) fn child(&self, p: Painter) -> Painter {
        let t = self.amount();
        p.alpha(t)
            .translate(CHILD_LEAD * Rect::FULL.w * (1.0 - t), 0.0)
    }
}

#[derive(Clone, Copy)]
pub(crate) struct RouteLayout {
    pub(crate) narrative: Rect,
    pub(crate) content: Rect,
    pub(crate) action: Rect,
}

impl RouteLayout {
    pub(crate) fn screen() -> Self {
        let top = SAFE.y + TOP_INSET;
        let bottom = SAFE.y + SAFE.h;
        let narrative = Rect::new(SAFE.x, top, NARRATIVE_W, bottom - top);
        let content_x = narrative.x + narrative.w + COLUMN_GAP;
        let content = Rect::new(content_x, top, SAFE.x + SAFE.w - content_x, bottom - top);
        let action = Rect::new(narrative.x, bottom - ACTION_H, narrative.w, ACTION_H);
        Self {
            narrative,
            content,
            action,
        }
    }

    /// Frame for a table whose first section has a label.
    ///
    /// [`TableView`](crate::ui::table::TableView) owns internal breathing room above that label.
    /// The route owns the external alignment contract: the label's cap-top sits on the same anchor
    /// as the narrative title.  Expanding the frame upward by the table-owned inset joins those
    /// two facts without either component copying the other's padding.
    pub(crate) fn sectioned_table(self) -> Rect {
        let inset = crate::ui::table::FIRST_HEADER_CAP_OFFSET;
        Rect::new(
            self.content.x,
            self.content.y - inset,
            self.content.w,
            self.content.h + inset,
        )
    }

    /// Place two controls on one action row, as ONE GROUP.
    ///
    /// Their relationship belongs here: the leading one starts on the shared margin, the trailing
    /// one follows by [`widgets::CONTROL_GAP`], and both inherit the action band's Y/height. A
    /// screen supplies measured widths, never a second pair of coordinates.
    ///
    /// **Its one caller is first-run consent's two answers, and they are EQUALS** — no primary,
    /// no secondary, no danger face. This doc used to describe a primary beside a BACK
    /// affordance (`Start watching` + `Press [BACK] to return`), which is the pairing the crumb
    /// deleted from this family; there is no BACK affordance in an action band any more. The gap
    /// followed that change: it was `space::LG` 40, the REGION rung for a primary standing beside
    /// a separate hint, and two peer answers at that distance read as two unrelated controls
    /// sharing a row rather than one question's two faces.
    pub(crate) fn action_pair(self, leading_w: f32, trailing_w: f32) -> (Rect, Rect) {
        let leading = Rect::new(self.action.x, self.action.y, leading_w, self.action.h);
        let trailing = Rect::new(
            leading.x + leading.w + crate::ui::widgets::CONTROL_GAP,
            self.action.y,
            trailing_w,
            self.action.h,
        );
        debug_assert!(trailing.x + trailing.w <= self.action.x + self.action.w);
        (leading, trailing)
    }

    /// Draw the return crumb — `‹ <where BACK goes>` — in the band whose top is at `top`.
    ///
    /// The gap between mark and word is measured to the chevron's INK, not to its box: the asset
    /// carries about a third of its width as bearing, so a box-to-text gap on a spacing rung comes
    /// out visibly loose and the pair stops reading as one object. Same rule the player HUD's
    /// transport slot uses, and [`icons::ink_x`] is where the numbers live.
    /// The y the TITLE starts at, given whether a crumb precedes it.
    ///
    /// Pure and separate from [`Self::draw_narrative`] because it is the whole of the crumb's
    /// layout cost, and a test that cannot call the draw (it measures through SDL2_ttf) can still
    /// grade it. The first version of this test compared `RouteLayout::screen().content.y` with
    /// its own `l.content.y` — a value against itself, which cannot fail.
    pub(crate) fn narrative_top(self, has_crumb: bool) -> f32 {
        if has_crumb {
            self.narrative.y + CRUMB_BAND + theme::space::SM
        } else {
            self.narrative.y
        }
    }

    fn draw_crumb(self, p: Painter, top: f32, back_to: &str) {
        let h = CRUMB_BAND;
        let cy = top + h * 0.5;
        icons::draw(
            p,
            Icon::ChevronLeft,
            Rect::new(
                self.narrative.x,
                cy - CRUMB_MARK * 0.5,
                CRUMB_MARK,
                CRUMB_MARK,
            ),
            theme::TEXT_TERTIARY,
        );
        let (_, ink_r) = icons::ink_x(Icon::ChevronLeft);
        let x = self.narrative.x + CRUMB_MARK * ink_r + theme::space::XS;
        TextView::new(back_to, theme::size::CAPTION, theme::TEXT_TERTIARY)
            .max_lines(1)
            .draw(p, Rect::new(x, top, self.narrative.x + self.narrative.w - x, h));
    }

    /// Draw a measured crumb→title→copy flow.  Each block begins after the previous one's actual
    /// wrapped height, never after a screen-specific y offset, and the copy stops before the
    /// shared action slot.
    ///
    /// `back_to` is `None` exactly when BACK does not go anywhere inside the app — see the module
    /// doc for which routes those are and how to census them.  Passing it here rather than letting
    /// screens draw their own line is what makes that a contract instead of a convention: a new
    /// route cannot forget to answer without deleting an argument.
    pub(crate) fn draw_narrative(
        self,
        p: Painter,
        back_to: Option<&str>,
        title: &str,
        copy: &str,
        copy_size: std::os::raw::c_int,
    ) {
        let top = self.narrative_top(back_to.is_some());
        if let Some(back_to) = back_to {
            self.draw_crumb(p, self.narrative.y, back_to);
        }

        let title = TextView::new(title, theme::size::HERO, theme::TEXT_HEADING)
            .bold()
            .max_lines(2);
        let title_h = title.measure_h(self.narrative.w);
        title.draw(p, Rect::new(self.narrative.x, top, self.narrative.w, title_h));

        let copy_top = top + title_h + theme::space::MD;
        let copy_bottom = self.action.y - theme::space::XL;
        TextView::new(copy, copy_size, theme::TEXT_READING)
            .leading(copy_size as f32 + theme::space::XS)
            .max_lines(12)
            .draw(
                p,
                Rect::new(
                    self.narrative.x,
                    copy_top,
                    self.narrative.w,
                    (copy_bottom - copy_top).max(0.0),
                ),
            );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::consts::inside_safe;

    #[test]
    fn every_required_route_region_is_inside_the_safe_area() {
        let l = RouteLayout::screen();
        assert!(inside_safe(l.narrative));
        assert!(inside_safe(l.content));
        assert!(inside_safe(l.action));
    }

    #[test]
    fn columns_are_related_objects_not_overlapping_coordinates() {
        let l = RouteLayout::screen();
        assert!(l.narrative.x + l.narrative.w < l.content.x);
        assert_eq!(l.narrative.y, l.content.y);
        assert_eq!(l.narrative.y + l.narrative.h, l.content.y + l.content.h);
        assert_eq!(l.action.x, l.narrative.x);
        assert_eq!(l.action.y + l.action.h, SAFE.y + SAFE.h);
    }

    #[test]
    fn first_table_section_and_narrative_share_one_top_anchor() {
        let l = RouteLayout::screen();
        let table = l.sectioned_table();
        assert_eq!(
            table.y + crate::ui::table::FIRST_HEADER_CAP_OFFSET,
            l.narrative.y
        );
        assert_eq!(table.y + table.h, l.content.y + l.content.h);
    }

    #[test]
    fn simultaneous_actions_flow_in_one_shared_bottom_row() {
        let l = RouteLayout::screen();
        let (leading, trailing) = l.action_pair(280.0, 240.0);
        assert_eq!(leading.x, l.action.x);
        assert_eq!(leading.y, trailing.y);
        assert_eq!(leading.h, trailing.h);
        assert_eq!(
            trailing.x - (leading.x + leading.w),
            crate::ui::widgets::CONTROL_GAP,
            "two peer answers are one control GROUP, not two blocks on a spacing rung"
        );
        assert!(
            crate::ui::widgets::CONTROL_GAP < theme::space::MD,
            "and a control-group gap is tighter than any rung of the block ladder"
        );
        assert!(inside_safe(trailing));
    }

    /// **The crumb costs the TITLE its own height and nothing else pays.** The content column is
    /// anchored at `--route-top` whether or not the narrative carries a crumb — a crumb that
    /// pushed nothing would overprint the title, and one that moved the CONTENT would break the
    /// alignment contract `sectioned_table`'s lift exists to hold.
    #[test]
    fn a_crumb_costs_the_title_its_own_height_and_moves_nothing_else() {
        let l = RouteLayout::screen();
        assert_eq!(l.narrative_top(false), l.narrative.y, "no crumb, no cost");
        assert_eq!(
            l.narrative_top(true) - l.narrative_top(false),
            CRUMB_BAND + theme::space::SM,
            "a crumb pushes the title down by exactly its own band plus one rung"
        );
        // Its neighbours are anchored independently of it, so both columns still hang from the one
        // top guide however the narrative begins.
        assert_eq!(l.content.y, l.narrative.y);
        assert_eq!(
            l.sectioned_table().y + crate::ui::table::FIRST_HEADER_CAP_OFFSET,
            l.narrative.y
        );
        assert_eq!(l.action.y + l.action.h, SAFE.y + SAFE.h);
        // And a crumbed narrative still has room for a two-line title and copy above the band.
        assert!(l.narrative_top(true) + theme::size::HERO as f32 * 2.0 + theme::space::MD
            < l.action.y - theme::space::XL);
    }

    /// The crumb's mark and its word are ONE object, so the gap between them is measured to the
    /// chevron's ink rather than to its box — the asset is a third bearing, and a box-to-text gap
    /// on a spacing rung reads as two separate things.
    #[test]
    fn the_crumb_measures_its_gap_to_the_marks_ink() {
        let (_, ink_r) = crate::ui::icons::ink_x(Icon::ChevronLeft);
        assert!(ink_r < 1.0, "the chevron carries a right bearing to absorb");
        let l = RouteLayout::screen();
        let label_x = l.narrative.x + CRUMB_MARK * ink_r + theme::space::XS;
        assert!(
            label_x < l.narrative.x + CRUMB_MARK + theme::space::XS,
            "measuring to ink puts the word CLOSER than a box-to-text gap would"
        );
        assert!(label_x > l.narrative.x + CRUMB_MARK * 0.5, "…but not over it");
    }

    #[test]
    fn nested_route_uses_one_reversible_spring() {
        let mut push = RoutePush::new();
        push.update(true, 1.0 / 60.0);
        assert!(push.amount() > 0.0 && push.amount() < 1.0);
        for _ in 0..600 {
            push.update(true, 1.0 / 60.0);
        }
        assert!((push.amount() - 1.0).abs() < 0.001);
        for _ in 0..600 {
            push.update(false, 1.0 / 60.0);
        }
        assert!(push.amount() < 0.001);
    }

    #[test]
    fn route_ground_latches_once_instead_of_following_child_screens() {
        let mut ground = RouteGround::new();
        let first = [[0.1, 0.2, 0.3]; 4];
        let second = [[0.8, 0.7, 0.6]; 4];
        ground.latch(first, mean_key(first));
        let palette = ground.palette();
        ground.latch(second, mean_key(second));
        assert_eq!(ground.palette(), palette);
        assert!(ground.is_latched());
    }

    #[test]
    fn route_ground_key_is_the_whole_envelope_not_one_loud_corner() {
        assert_eq!(
            mean_key([
                [0.0, 0.2, 0.4],
                [0.2, 0.4, 0.6],
                [0.4, 0.6, 0.8],
                [0.6, 0.8, 1.0],
            ]),
            [0.3, 0.5, 0.7]
        );
    }

    #[test]
    fn pre_home_fallback_is_an_envelope_not_a_flat_grey() {
        let c = theme::ROUTE_GROUND_FALLBACK;
        assert!(c.windows(2).any(|pair| pair[0] != pair[1]));
        assert!(c.iter().any(|corner| {
            (corner[0] - corner[1]).abs() > 0.001 || (corner[1] - corner[2]).abs() > 0.001
        }));
    }

    #[test]
    fn default_route_ground_latches_the_auth_fallback_once() {
        let mut ground = RouteGround::new();
        ground.latch_target(theme::ROUTE_GROUND_FALLBACK);
        let palette = ground.palette();
        ground.latch_target([[1.0, 0.0, 0.0, 1.0]; 4]);
        assert_eq!(ground.palette(), palette);
        assert!(ground.is_latched());
    }
}
