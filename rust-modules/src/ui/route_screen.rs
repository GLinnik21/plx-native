//! Shared full-screen route geometry used by Settings and its children.
//!
//! A route screen is a two-column composition, not a bag of coordinates: a narrative column on
//! the left, a content column on the right, and one bottom action slot that is shared by the BACK
//! hint and a context action such as Done.  Screens supply words and state; this module owns every
//! spatial relationship between them.

use crate::ui::consts::SAFE;
use crate::ui::text_view::TextView;
use crate::ui::{theme, Painter, Rect};

/// The left column is the same editorial measure as the Home hero.  Reusing that named measure is
/// what makes first-run routes and Settings feel like one family instead of two similar layouts.
const NARRATIVE_W: f32 = crate::ui::home::HERO_COL_W;
/// Two visually separate columns need a region gap, not a row gap.  Expressed entirely on the
/// spacing ladder so a retune of that ladder moves every route together.
const COLUMN_GAP: f32 = theme::space::XL * 2.0 + theme::space::LG + theme::space::SM;
const TOP_INSET: f32 = theme::space::XL + theme::space::MD;
const ACTION_H: f32 = crate::ui::widgets::StatusOverlay::CTRL_H;

#[derive(Clone, Copy, Debug)]
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
        Self { narrative, content, action }
    }

    /// Draw a measured title→copy flow.  The copy begins after the title's actual wrapped height,
    /// never after a screen-specific y offset, and it stops before the shared action slot.
    pub(crate) fn draw_narrative(
        self,
        p: Painter,
        title: &str,
        copy: &str,
        copy_size: std::os::raw::c_int,
    ) {
        let title = TextView::new(title, theme::size::HERO, theme::TEXT_HEADING)
            .bold()
            .max_lines(2);
        let title_h = title.measure_h(self.narrative.w);
        title.draw(p, Rect::new(self.narrative.x, self.narrative.y, self.narrative.w, title_h));

        let copy_top = self.narrative.y + title_h + theme::space::MD;
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
}
