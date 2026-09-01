//! A route-sized document reader: measured text flow, spring scrolling and a continuous rail.

use crate::ui::consts::K_SCROLL;
use crate::ui::text_view::TextView;
use crate::ui::widgets;
use crate::ui::{theme, Painter, Rect, Spring};

const BODY_LEAD: f32 = theme::size::CAPTION as f32 + theme::space::XS;
const STEP: f32 = BODY_LEAD * 6.0;
const INDENT: f32 = theme::space::XS;
const RAIL_GAP: f32 = theme::space::MD;

pub(crate) struct DocumentReader {
    scroll: Spring,
    target: f32,
    max_scroll: f32,
}

impl DocumentReader {
    pub(crate) const fn new() -> Self {
        Self {
            scroll: Spring::at(0.0),
            target: 0.0,
            max_scroll: 0.0,
        }
    }

    pub(crate) fn reset(&mut self) {
        self.target = 0.0;
        self.max_scroll = 0.0;
        self.scroll.jump(0.0);
    }

    pub(crate) fn move_by(&mut self, delta: i32) {
        self.target = (self.target + delta as f32 * STEP).clamp(0.0, self.max_scroll);
        crate::ui::idle::invalidate();
    }

    pub(crate) fn update(&mut self, dt: f32) {
        self.target = self.target.clamp(0.0, self.max_scroll);
        self.scroll.step(self.target, K_SCROLL, dt);
    }

    /// Draw `body` as a preserved line flow.  A line may wrap, but explicit blank lines and JSON
    /// indentation remain real layout objects instead of being collapsed by `TextView`.
    pub(crate) fn draw(&mut self, p: Painter, frame: Rect, title: Option<&str>, body: &str) {
        let mut body_frame = frame;
        if let Some(title) = title {
            let title = TextView::new(title, theme::size::HEADLINE, theme::TEXT_HEADING).bold();
            let h = title.measure_h(frame.w);
            title.draw(p, Rect::new(frame.x, frame.y, frame.w, h));
            body_frame.y += h + theme::space::MD;
            body_frame.h -= h + theme::space::MD;
        }

        let text_w = (body_frame.w - widgets::RAIL_W - RAIL_GAP).max(1.0);
        let mut lines = Vec::new();
        let mut y = 0.0;
        for line in body.lines() {
            let trimmed = line.trim_start();
            let indentation = (line.len() - trimmed.len()) as f32 * INDENT;
            let heading = !trimmed.is_empty()
                && trimmed.chars().any(char::is_alphabetic)
                && trimmed
                    .chars()
                    .all(|c| !c.is_alphabetic() || c.is_uppercase());
            let mut view = TextView::new(trimmed, theme::size::CAPTION, theme::TEXT_READING)
                .leading(BODY_LEAD);
            if heading {
                view = view.bold();
            }
            let h = if trimmed.is_empty() {
                BODY_LEAD
            } else {
                view.measure_h((text_w - indentation).max(1.0))
            };
            lines.push((y, h, indentation, view));
            y += h;
        }

        self.max_scroll = (y - body_frame.h).max(0.0);
        self.target = self.target.min(self.max_scroll);
        let scroll = self.scroll.pos.clamp(0.0, self.max_scroll);
        p.clip(body_frame);
        for (line_y, h, indentation, view) in &lines {
            let top = body_frame.y + line_y - scroll;
            if top + h < body_frame.y || top > body_frame.y + body_frame.h {
                continue;
            }
            view.draw(
                p,
                Rect::new(
                    body_frame.x + indentation,
                    top,
                    (text_w - indentation).max(1.0),
                    *h,
                ),
            );
        }
        p.clip_clear();

        widgets::continuous_scroll_rail(
            p,
            Rect::new(
                body_frame.x + body_frame.w - widgets::RAIL_W,
                body_frame.y,
                widgets::RAIL_W,
                body_frame.h,
            ),
            scroll,
            y,
            body_frame.h,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn document_scroll_moves_on_a_spring_and_reaches_rest() {
        let _guard = crate::testlock::serial();
        let mut reader = DocumentReader::new();
        reader.max_scroll = 1000.0;
        reader.move_by(1);
        let target = reader.target;
        reader.update(1.0 / 60.0);
        assert!(reader.scroll.pos > 0.0 && reader.scroll.pos < target);
        for _ in 0..600 {
            reader.update(1.0 / 60.0);
        }
        assert!((reader.scroll.pos - target).abs() < 0.01);
        assert!(reader.scroll.vel.abs() < 0.01);
    }

    #[test]
    fn reset_clears_position_velocity_and_document_bounds() {
        let _guard = crate::testlock::serial();
        let mut reader = DocumentReader::new();
        reader.max_scroll = 600.0;
        reader.move_by(1);
        reader.update(1.0 / 60.0);
        reader.reset();
        assert_eq!(
            (
                reader.scroll.pos,
                reader.scroll.vel,
                reader.target,
                reader.max_scroll
            ),
            (0.0, 0.0, 0.0, 0.0)
        );
    }
}
