//! Library paint consumes the same placement queries as keyboard and pointer navigation.
use super::*;
use plx_ui::screen::{Activate, Hover, Stop};
use plx_ui::theme;
use plx_ui::{Env, View, on_axis};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Layer { Grid, Document, Rail }

/// Paint and stop registration share one back-to-front order.
fn layers(mut visit: impl FnMut(Layer)) {
    for layer in [Layer::Grid, Layer::Document, Layer::Rail] { visit(layer); }
}

/// Reject a whole control band before creating labels or measuring individual pills. The
/// full-width envelope preserves travelling capsules; shared cast tokens cover their shadows.
/// It is deliberately wider than the controls, so no text metrics are needed to reject it.
pub(super) fn document_band_visible(p: plx_ui::Painter, y: f32, height: f32, pop: f32) -> bool {
    let pad = theme::CONTROL_CAST_FOCUS.iter()
        .map(|(dy, blur, _)| dy.abs() + blur).fold(0.0f32, f32::max) + 1.0;
    let mut bounds = Rect::new(0.0, y, SCR_W, height)
        .scaled(pop.max(1.0) * p.scale().max(1.0)).inset(-pad);
    bounds.x += p.dx();
    bounds.y += p.dy();
    let visible = bounds.intersect(Rect::FULL);
    visible.w > 0.0 && visible.h > 0.0
        && (plx_ui::frame::backdrop::discovering()
            || !plx_gfx::gfx::culled(bounds.x, bounds.y, bounds.w, bounds.h))
}

#[cfg(test)]
mod layer_tests {
    #[test]
    fn document_band_culling_preserves_partial_labels_focus_and_shadow_edges() {
        let _guard = plx_base::testlock::serial();
        let p = plx_ui::Painter::root();
        let height = plx_ui::widgets::StatusOverlay::CTRL_H;
        let pad = plx_ui::theme::CONTROL_CAST_FOCUS.iter()
            .map(|(dy, blur, _)| dy.abs() + blur).fold(0.0f32, f32::max) + 1.0;
        assert!(super::document_band_visible(p, -height + 1.0, height, 1.0));
        assert!(super::document_band_visible(p, -height - pad + 1.0, height, 1.0),
            "a focus shadow can remain on the panel after the control itself left");
        let above = -height - pad - 1.0;
        assert!(!super::document_band_visible(p, above, height, 1.0));
        assert!(super::document_band_visible(p, above, height, plx_ui::widgets::CTRL_FOCUS_SCALE),
            "the focused capsule's growth belongs in the paint envelope");
        assert!(super::document_band_visible(p.translate(0.0, 300.0), above, height, 1.0));
        assert!(!super::document_band_visible(p, super::SCR_H + pad + 1.0, height, 1.0));
        assert!(!super::document_band_visible(p, -2000.0, plx_ui::page_header::HEIGHT, 1.0));
    }

    #[test]
    fn document_band_culling_keeps_visible_controls_during_backdrop_discovery() {
        let _guard = plx_base::testlock::serial();
        let _discovery = plx_ui::frame::backdrop::discover(
            std::rc::Rc::new(std::cell::RefCell::new(Default::default())));
        assert!(plx_gfx::gfx::culled(0.0, 0.0, 100.0, 100.0),
            "discovery suppresses GL draws without suppressing paint declarations");
        assert!(super::document_band_visible(plx_ui::Painter::root(),
            super::CONTENT_TOP, plx_ui::page_header::HEIGHT, 1.0));
    }

    #[test]
    fn library_paint_and_stop_order_keeps_controls_above_grid_and_rail_above_document() {
        use super::{layers, Layer};
        let mut seen = Vec::new();
        layers(|layer| seen.push(layer));
        assert_eq!(seen, [Layer::Grid, Layer::Document, Layer::Rail]);
        for layer in [Layer::Grid, Layer::Document, Layer::Rail] {
            assert_eq!(seen.iter().filter(|&&found| found == layer).count(), 1);
        }
    }
}

impl LibraryScreen {
    pub(super) fn draw_page<H: LibraryLike>(&mut self, f: &mut DrawFrame<'_, '_, H>) {
        // **The Library's own phase names**, in the `hm.*`/`st.*` family — `/tmp/plxnative-cpuprof`
        // times each on the render thread (inclusive wall, no `glFinish`, every phase at once) and
        // `--graphics-profile --profile-phase <name>` samples one. They exist because the owned
        // screen shipped with NONE: the 2026-09-09 `fps:library-switch` profile read `frame.ui`
        // 24.5 ms and `main.ui` 22.4 with nothing named beneath, so the whole page was one opaque
        // block and the only attribution left was the draw-MASK census, which can subtract a class
        // but cannot say which part of the screen spent it.
        //
        // `lb.ground` is the full-screen wash the mask census implicated (masking `ambient` moved
        // `loop=` 43-45 → 57); `lb.document` brackets the header and the status
        // read-out; `lb.grid` is the poster
        // wall's windowed rows and `lb.rail` the letter rail.
        plx_ui::profile::phase("lb.clear", || {
            plx_gfx::gfx::frame_clear(theme::CLEAR_RGB.0, theme::CLEAR_RGB.1, theme::CLEAR_RGB.2);
        });
        plx_ui::profile::phase("lb.ground", || {
            self.ground.draw(f.painter.alpha(f.page_alpha), Rect::FULL);
        });
        let layout = self.pair.layout();
        let alpha = self.page_fade.alpha() * self.grid_fade.alpha();
        layers(|layer| match layer {
            Layer::Grid => plx_ui::profile::phase("lb.grid", || {
                draw_faded_part_at(&mut self.pair.detail, f, layout.detail, alpha)
            }),
            Layer::Document => plx_ui::profile::phase("lb.document", || self.draw_document(f)),
            Layer::Rail => plx_ui::profile::phase("lb.rail", || {
                // Decoration derives from the CURRENT engine key; no remembered cursor is copied.
                let index = f.focus.current.filter(|key| key.entry == self.entry)
                    .and_then(|key| self.pair.detail.index_of(key.elem));
                let parent = f.page_alpha;
                f.page_alpha = parent * alpha;
                self.pair.master.draw_with_current(f, layout.master, index);
                f.page_alpha = parent;
            }),
        });
        // The page's own top edge once the chrome has tucked away, and then the capsule. They follow
        // the page's one handoff (`scroll_chrome::Handoff`): the capsule is absent until the chrome
        // has gone, so the two live glasses never share a frame. The capsule's rect is measured
        // first because the fade reaches through it (`scroll_capsule::fade_reach`).
        let tuck = self.handoff.tuck();
        let capsule = self.handoff.capsule();
        // The capsule carries the sort: the criterion the header's control shows, a queued edit
        // included. Its strings are kept while they stand (`CapsuleText`), and it is measured once
        // here (`Capsule::new`): the fade reads its rectangle and the capsule draws from the same
        // measured runs.
        let name = (capsule > 0.0 && self.capsule_wanted())
            .then(|| self.view_section(f.cx).map(|index| self.library_name(index, f.cx)))
            .flatten();
        let sort = name.map(|_| self.sort_value(f.cx));
        let total = H::listing(f.cx).total();
        // The name is made of the two borrowed pieces and keyed on them: no string is put together
        // on a frame that changes none of them.
        let strings = name.map(|(title, owner)| self.capsule_text.get_keyed(&[title, owner], || super::toolbar::joined_name(title, owner), total, sort));
        let worn = strings.map(|(name, count, criterion)| plx_ui::scroll_capsule::Capsule::new(name, count, criterion, f.measure));
        if tuck > 0.0 {
            let ground = self.ground.sample(Rect::FULL, SCR_W * 0.5, 0.0);
            let reach = worn.as_ref().map(|worn| (worn.rect(), capsule));
            plx_ui::scroll_capsule::draw_top_fade(f.painter.alpha(f.page_alpha), SCR_W, ground, tuck, reach);
        }
        if let Some(worn) = &worn {
            worn.draw(f.painter.alpha(f.page_alpha), capsule);
        }
        // A standing note, never a target: over the page, records no stop and no hit rect.
        self.hold_hint.draw(f.painter.alpha(f.page_alpha), f.measure);
        self.plaintext_alert.draw(f, self.entry);
    }

    fn draw_document<H: LibraryLike>(&self, f: &mut DrawFrame<'_, '_, H>) {
        let env = Env::inert();
        self.draw_header(f);
        if self.readout == Readout::Loading {
            // Preserve the Library's standalone loading spinner, outside either content fade.
            plx_ui::placeholder::note(f.painter, plx_ui::placeholder::Reason::LibrarySpinner, "");
            plx_ui::widgets::Spinner::new(SCR_W * 0.5, SCR_H * 0.52, 26.0)
                .phase(f.cx.tick.ms).draw(&env, f.painter.alpha(f.page_alpha));
        } else if self.readout != Readout::Grid {
            let (text, reason) = self.status_text(f.cx);
            let status = self.status_overlay(f.cx, &text, reason.as_deref());
            let alpha = if self.readout == Readout::Empty { self.page_fade.alpha() * self.grid_fade.alpha() } else { 1.0 };
            status.draw_measured(&env, f.painter.alpha(f.page_alpha * alpha), f.cx.measure);
        }
        if !self.plaintext_alert.visible() {
            // the question owns the pointer while it is up; nothing under it is a target
            self.record_document_stops(f);
        }
    }

    fn record_document_stops<H: LibraryLike>(&self, f: &mut DrawFrame<'_, '_, H>) {
        if !f.records_stops() { return; }
        for elem in self.header_elems() {
            if self.header_rect(elem, At::Drawn).is_some_and(|rect| on_axis(rect.y, rect.h, SCR_H, 0.0)) {
                self.stop(elem, f);
            }
        }
        if self.readout == Readout::Failed { self.stop(RETRY, f); }
    }

    /// The production stop producers without rasterization, for the no-SDL dispatcher fixture.
    #[cfg(test)]
    pub fn record_stops<H: LibraryLike>(&self, f: &mut DrawFrame<'_, '_, H>) {
        layers(|layer| match layer {
            Layer::Grid => self.pair.detail.record_stops(f),
            Layer::Document => self.record_document_stops(f),
            Layer::Rail => self.pair.master.record_stops(f),
        });
    }

    fn stop<H: LibraryLike>(&self, elem: u32, f: &mut DrawFrame<'_, '_, H>) {
        let Some(placed) = <Self as Focusable<H>>::place(self, &elem, f.cx, At::Drawn) else { return };
        f.stop(f.painter, Stop {
            key: self.key(elem), rect: placed.rect, rest_rect: placed.rest_rect, clip: placed.clip,
            hover: Hover::Focus, activate: Activate::Press,
        });
    }

    pub fn redraw_focused<H: LibraryLike>(&self, f: &mut DrawFrame<'_, '_, H>, focus: Option<FocusKey<u32>>) {
        let Some(key) = focus.filter(|key| key.entry == self.entry) else { return };
        // A header control a menu opened from (Sort & Filter, `+N`) is lit by the row itself, with
        // no card to place: the row knows its own stops.
        self.draw_header_opener(f, key.elem);
        let Some(placed) = <Self as Focusable<H>>::place(self, &key.elem, f.cx, At::Drawn) else { return };
        let _clip = f.clip(f.painter, placed.clip);
        if self.pair.detail.index_of(key.elem).is_some() {
            let parent = f.page_alpha;
            f.page_alpha *= self.page_fade.alpha() * self.grid_fade.alpha();
            self.pair.detail.draw_focused(f, Some(key));
            f.page_alpha = parent;
        }
    }
}

fn draw_faded_part_at<H: LibraryLike>(part: &mut impl Part<H>, f: &mut DrawFrame<'_, '_, H>, rect: Rect, alpha: f32) {
    let parent = f.page_alpha;
    f.page_alpha = parent * alpha;
    part.draw(f, rect);
    f.page_alpha = parent;
}
