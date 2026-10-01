//! **The resize and page-slide motion of an in-player table popover** (`docs/player-submenus.md`,
//! "Animation"): the Tracks panel's drill-in pages and tab switches, and the `…` popover's row set.
//!
//! The panel is a bottom/right-anchored card whose natural size is its table's measured size. When
//! that size changes (a page push or pop, a tab switch, a live row added) the card does not jump:
//! a [`Spring`] per moving edge carries the TOP and LEFT edges to the new layout while the bottom
//! and right edges stay on the anchor the panel family shares with the control row. Everything
//! here is built from the app's one spring integrator, so the present gate ([`ui::idle`]) knows
//! exactly when the panel is moving and an at-rest panel asks for no frame at all.
//!
//! A page transition additionally moves the OUTGOING and INCOMING pages sideways, both inside the
//! animated card's clip, on a STAGGERED fade: the outgoing page leaves quickly and the incoming one
//! only starts to appear once it is nearly gone, so the two never read on top of each other. Only the
//! incoming page draws a focus pill. There is exactly one panel background, no backdrop
//! capture and no transition texture: the outgoing page is the old [`TableView`] itself, swapped
//! out whole when the next page was built ([`TableView::blank_like`]) and drawn once more, at its
//! own natural size, until the slide lands.
//!
//! **Layout is computed once.** [`PanelMotion::natural`] caches a table's natural rect against the
//! table's own [`TableView::layout_rev`], so the per-frame callers (update, draw, every stop's
//! `place`) re-measure text only when the table actually changed.
//!
//! **Input follows the logical state, not the motion.** A push during a push, or a pop during a
//! push, retargets from where the layers are ([`PanelMotion::begin_slide`] hands each layer's alpha
//! and offset to its new role, so nothing steps). The pointer is the one exception: while [`PanelMotion::transitioning`] it is held
//! (`Screen::pointer_held`), because a hit map built from pages in motion would turn a click on a
//! row sliding past into a click on the row that happened to be under it.

use std::cell::Cell;

use crate::ui::machine::Measure;
use crate::ui::screen::ClipScope;
use crate::ui::table::TableView;
use crate::ui::{theme, Painter, Rect, Spring};

/// How far a page travels as it leaves and arrives, as a fraction of the card's width. Short on
/// purpose: the card's own resize carries the large motion, and a page crossing the whole card
/// would read as a route change rather than a drill-in.
const SHIFT_FRAC: f32 = 0.22;

/// A leaving page fades out in this many seconds, linearly: quick, so it is gone before the
/// arriving page is legible.
const OUT_S: f32 = 0.14;
/// The arriving page fades in over this many seconds once it is allowed to start.
const IN_S: f32 = 0.22;
/// The arriving page does not start to rise until every leaving page is at or below this alpha, so
/// the two never read on top of each other: at no point are both above it. (Two pages that are each
/// this faint still collide visibly, so it is low: the arriving page waits for the leaving one to
/// be all but gone.)
pub(crate) const GATE: f32 = 0.1;
/// A layer this faint is not drawn at all (the Mali fill-rate budget: no second table pass for a
/// page nobody can see).
const VISIBLE: f32 = 0.01;

/// How many strings [`PanelMotion::drain_queued_text`] rasterises in one presented frame. Counted, not
/// timed: the UI reads no wall clock (`ci/allow/wall.txt`), and on the TV one string is ~1 ms
/// (`textx8:7.4`), so eight sit inside the frame's ~15 ms back-buffer wait. The rest, if any, are
/// rasterised by the draw exactly as before.
const PREWARM_STRINGS: u64 = 8;

/// [`PREWARM_STRINGS`] for a queue that holds only a page nobody is looking at yet
/// ([`PanelMotion::prewarm_background_text`]). Nothing waits on it, so it costs one string, ~1 ms
/// on the TV, per presented frame: drained eight at a time it was `warmdrain:11.4`, a 21.3 ms
/// frame right after the track menu opened.
const BACKGROUND_STRINGS: u64 = 1;

thread_local! {
    /// Whether the text prewarm queue holds only background strings. Set by
    /// [`PanelMotion::prewarm_background_text`], cleared by a live walk and by an empty queue.
    static BACKGROUND_QUEUE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Stiffness of the card's top/left edges and of the pages' sideways travel (`gfx::spring`'s `k`;
/// the table's own scroll uses 300). Critically damped, so nothing overshoots.
const RECT_K: f32 = 300.0;
const SLIDE_K: f32 = 300.0;

/// A rect edge within this of its target (px) counts as arrived for [`PanelMotion::transitioning`];
/// half a pixel is below anything the panel can show.
const EDGE_REST_PX: f32 = 0.5;

/// While pages move the content clip is pulled in by this much on every side, so a page sliding
/// past the edge is cut inside the card, never at (or beyond) the card's rounded corners: the
/// scissor is a plain rectangle and cannot follow the radius.
const CLIP_INSET: f32 = 10.0;

/// A page that is leaving: the whole old table, drawn once more at the rect it was laid out at.
struct Layer {
    table: TableView,
    rect: Rect,
    alpha: f32,
    /// Sideways offset in px; springs toward the exit side.
    x: Spring,
}

/// A running page transition. The live page's own table is the caller's (it keeps taking input);
/// only its alpha and offset live here.
struct Slide {
    /// `+1.0` for a push (pages travel left: leaving exits left, arriving enters from the right),
    /// `-1.0` for a pop.
    dir: f32,
    /// Pages on their way out, oldest first. Almost always one; a push during a push adds one.
    leaving: Vec<Layer>,
    live_alpha: f32,
    live_x: Spring,
}

pub(crate) struct PanelMotion {
    left: Spring,
    top: Spring,
    /// The anchored edges: the last natural rect's right and bottom.
    right: f32,
    bottom: f32,
    /// `false` until the first [`Self::step`], so a panel opens AT its layout and only later moves.
    placed: bool,
    /// The layout cache: the table's [`TableView::layout_rev`] and the natural rect measured for it.
    cache: Cell<Option<(u32, Rect)>>,
    /// The last natural rect handed out — what the page that is about to leave was laid out at.
    last_natural: Cell<Rect>,
    slide: Option<Slide>,
    /// The [`TableView::layout_rev`] whose strings [`Self::prewarm_text`] last rasterised.
    warmed: Cell<Option<u32>>,
}

impl PanelMotion {
    pub(crate) const fn new() -> Self {
        Self {
            left: Spring::at(0.0),
            top: Spring::at(0.0),
            right: 0.0,
            bottom: 0.0,
            placed: false,
            cache: Cell::new(None),
            last_natural: Cell::new(Rect::new(0.0, 0.0, 0.0, 0.0)),
            slide: None,
            warmed: Cell::new(None),
        }
    }

    /// **Record a freshly built page's strings in the UPDATE phase; they are rasterised before its
    /// first draw.**
    ///
    /// On the player route the frame's first framebuffer command (`glClear`) is where this driver
    /// waits for a free back buffer, ~15 ms of every frame. A drill-in's new page used to meet its
    /// strings cold in the draw AFTER that wait: `FRAMEDROP … clear:12.8 … textx8:7.4,surf:10.9`,
    /// a 26.7 ms frame on the TV on the first visit to each Tracks sub-page. Recorded
    /// here and drained ahead of the draw ([`Self::drain_queued_text`]), the same work overlaps the
    /// wait instead of adding to it. Runs once per table layout ([`TableView::layout_rev`]); a
    /// string the budget does not reach is rasterised by the draw exactly as before.
    pub(crate) fn prewarm_text(&self, natural: Rect, live: &TableView, measure: &dyn Measure) {
        let rev = live.layout_rev();
        if self.warmed.get() == Some(rev) {
            return;
        }
        self.warmed.set(Some(rev));
        // This walk's strings are the whole queue: a held modal's or a finished transition's
        // leftovers must not eat the drain's budget (`ui::dispatch` clears the same way).
        crate::text::clear_prewarm();
        BACKGROUND_QUEUE.with(|b| b.set(false));
        self.prewarm_more_text(natural, live, measure);
    }

    /// **Add `table`'s strings to the queue without replacing it** — a page that is not live yet
    /// (`track_menu`'s other tab). The next [`Self::prewarm_text`] walk replaces them with the
    /// live page's, which is the right priority: what is on screen comes first.
    pub(crate) fn prewarm_more_text(&self, natural: Rect, table: &TableView, measure: &dyn Measure) {
        // The same walk `ui::dispatch` runs for a page's warm pass: speculative to the recorder,
        // and no raw clear may reach the framebuffer from it.
        crate::gfx::without_frame_clear(|| {
            crate::ui::rec::speculative(|| {
                crate::ui::record_walk(|| table.draw(Painter::recording(), natural, measure))
            })
        });
    }

    /// [`Self::prewarm_more_text`] for a page that is not on screen, into an EMPTY queue: the
    /// drain then takes it [`BACKGROUND_STRINGS`] at a time.
    pub(crate) fn prewarm_background_text(&self, natural: Rect, table: &TableView, measure: &dyn Measure) {
        debug_assert!(!crate::text::prewarm_pending(), "background strings must not slow a live page's drain");
        BACKGROUND_QUEUE.with(|b| b.set(true));
        self.prewarm_more_text(natural, table, measure);
    }

    /// **Rasterise what [`Self::prewarm_text`] queued** — at most [`PREWARM_STRINGS`] of it, or
    /// [`BACKGROUND_STRINGS`] of a background queue. Called
    /// from the PRESENTING side of the present decision (`app::run::prepare_window`, step 9's
    /// upload seam), never from `update`: this uploads GL textures, and §10 says a frame that does
    /// not present uploads nothing (nor may it reach EGL while the window is backgrounded). It
    /// still runs before the draw's first `glClear`, so the work overlaps the back-buffer wait.
    pub(crate) fn drain_queued_text() -> usize {
        if !crate::text::prewarm_pending() {
            BACKGROUND_QUEUE.with(|b| b.set(false));
            return 0;
        }
        let budget = if BACKGROUND_QUEUE.with(|b| b.get()) { BACKGROUND_STRINGS } else { PREWARM_STRINGS };
        // `drain_prewarm` reads its clock before each next string: a clock that advances one unit
        // per read admits exactly `PREWARM_STRINGS` of them.
        let mut reads = 0u64;
        crate::diag::spans::span("warmdrain", || {
            crate::text::drain_prewarm(budget, || {
                reads += 1;
                reads
            })
        })
    }

    /// **The table's natural rect**, `compute`d only when `rev` (the table's
    /// [`TableView::layout_rev`]) differs from the one the cached rect was measured for.
    pub(crate) fn natural(&self, rev: u32, compute: impl FnOnce() -> Rect) -> Rect {
        if let Some((cached, rect)) = self.cache.get() {
            if cached == rev {
                return rect;
            }
        }
        let rect = compute();
        self.cache.set(Some((rev, rect)));
        self.last_natural.set(rect);
        rect
    }

    /// Advance one frame toward `natural`. Springs on the TOP and LEFT edges only; the right and
    /// bottom edges are the anchor and move with the layout at once.
    pub(crate) fn step(&mut self, dt: f32, natural: Rect) {
        self.right = natural.x + natural.w;
        self.bottom = natural.y + natural.h;
        if !self.placed {
            self.placed = true;
            self.left = Spring::at(natural.x);
            self.top = Spring::at(natural.y);
        } else {
            self.left.step(natural.x, RECT_K, dt);
            self.top.step(natural.y, RECT_K, dt);
            // land exactly: the analytic spring only approaches its target, and a panel that rests
            // a hair off its anchor would never be bit-identical to its own layout
            for (spring, target) in [(&mut self.left, natural.x), (&mut self.top, natural.y)] {
                if crate::ui::idle::settled(spring.pos, target, spring.vel) {
                    *spring = Spring::at(target);
                }
            }
        }
        let shift = SHIFT_FRAC * self.last_natural.get().w;
        if let Some(slide) = &mut self.slide {
            for layer in &mut slide.leaving {
                layer.alpha = (layer.alpha - dt / OUT_S).max(0.0);
                layer.x.step(-slide.dir * shift, SLIDE_K, dt);
                crate::ui::idle::note_spring(layer.alpha, 0.0, 1.0);
            }
            slide.leaving.retain(|l| l.alpha > 0.0);
            let loudest = slide.leaving.iter().fold(0.0_f32, |m, l| m.max(l.alpha));
            if loudest <= GATE {
                slide.live_alpha = (slide.live_alpha + dt / IN_S).min(1.0);
            }
            slide.live_x.step(0.0, SLIDE_K, dt);
            if slide.live_alpha < 1.0 {
                crate::ui::idle::note_spring(slide.live_alpha, 1.0, 1.0);
            }
            let at_rest = crate::ui::idle::settled(slide.live_x.pos, 0.0, slide.live_x.vel);
            if slide.leaving.is_empty() && slide.live_alpha >= 1.0 && at_rest {
                self.slide = None;
            }
        }
    }

    /// The card as drawn right now: top and left from the springs, bottom and right on the anchor.
    /// Before the first [`Self::step`] it is the layout itself.
    pub(crate) fn shown(&self, natural: Rect) -> Rect {
        if !self.placed {
            return natural;
        }
        Rect::new(self.left.pos, self.top.pos, (self.right - self.left.pos).max(0.0), (self.bottom - self.top.pos).max(0.0))
    }

    /// Is the panel mid-motion — the card still moving to its layout, or a page slide running?
    /// What `Screen::pointer_held` answers.
    pub(crate) fn transitioning(&self) -> bool {
        let natural = self.last_natural.get();
        self.slide.is_some()
            || (self.placed
                && ((self.left.pos - natural.x).abs() > EDGE_REST_PX || (self.top.pos - natural.y).abs() > EDGE_REST_PX))
    }

    /// Is a page slide running (as distinct from only the card resizing)?
    #[cfg(test)]
    pub(crate) fn sliding(&self) -> bool {
        self.slide.is_some()
    }

    /// The alphas of the pages in play, `(leaving..., live)`, for tests: the live page is `1.0`
    /// when nothing slides.
    #[cfg(test)]
    pub(crate) fn alphas(&self) -> (Vec<f32>, f32) {
        self.slide.as_ref().map_or((Vec::new(), 1.0), |s| (s.leaving.iter().map(|l| l.alpha).collect(), s.live_alpha))
    }

    /// How many LEAVING pages would draw a focus pill (none may: only the arriving page does).
    #[cfg(test)]
    pub(crate) fn leaving_pills(&self) -> usize {
        self.slide.as_ref().map_or(0, |s| s.leaving.iter().filter(|l| l.table.list_focused).count())
    }

    /// **A page was just swapped for another**: `old` is the table that was showing and `dir` is
    /// `+1.0` for a push, `-1.0` for a pop. The incoming page is the live table the caller just
    /// built. `old` is stored WITHOUT its focus pill: only the incoming page draws one.
    ///
    /// Every layer carries its own alpha and offset, and a swap hands them over rather than
    /// restarting them, so no input can step a picture:
    /// - No slide running: `old` leaves from full, the live page arrives from transparent.
    /// - OPPOSITE direction (push then pop mid-slide, or the reverse): the page that was arriving
    ///   is now the one being returned to and continues from its own alpha and offset, and the
    ///   page that was live leaves from its own.
    /// - The SAME direction (a push during a push): the page that was arriving leaves from where it
    ///   is, the older leaving page keeps fading, and the new page arrives from transparent.
    pub(crate) fn begin_slide(&mut self, old: TableView, dir: f32) {
        let old = old.unfocused();
        let rect = self.last_natural.get();
        let entry = dir * SHIFT_FRAC * rect.w;
        let Some(slide) = &mut self.slide else {
            self.slide = Some(Slide {
                dir,
                leaving: vec![Layer { table: old, rect, alpha: 1.0, x: Spring::at(0.0) }],
                live_alpha: 0.0,
                live_x: Spring::at(entry),
            });
            return;
        };
        let leaving_now = Layer { table: old, rect, alpha: slide.live_alpha, x: slide.live_x };
        if slide.dir != dir && !slide.leaving.is_empty() {
            let revived = slide.leaving.pop().expect("checked non-empty");
            slide.live_alpha = revived.alpha;
            slide.live_x = revived.x;
        } else {
            slide.live_alpha = 0.0;
            slide.live_x = Spring::at(entry);
        }
        slide.dir = dir;
        slide.leaving.push(leaving_now);
        // a held-down key must not stack tables without bound: past three, the faintest goes
        while slide.leaving.len() > 3 {
            let faintest = (0..slide.leaving.len())
                .min_by(|&a, &b| slide.leaving[a].alpha.total_cmp(&slide.leaving[b].alpha))
                .unwrap_or(0);
            slide.leaving.remove(faintest);
        }
    }

    /// Drop a running slide (the page was replaced wholesale: a tab switch, or a poll that popped
    /// every page): the card keeps resizing, the content just stops sliding.
    pub(crate) fn cancel_slide(&mut self) {
        self.slide = None;
    }

    /// The x offset the LIVE page is drawn (and its stops registered) at.
    pub(crate) fn live_dx(&self) -> f32 {
        self.slide.as_ref().map_or(0.0, |s| s.live_x.pos)
    }

    /// **Paint the card and its page(s)**: one background at the animated rect, then the pages
    /// under that rect's clip. `live` is drawn at `natural`, each leaving page at the rect it was
    /// laid out at, each on its own alpha and offset; a page too faint to see is not drawn.
    pub(crate) fn draw(&self, p: Painter, natural: Rect, radius: f32, live: &TableView, measure: &dyn Measure) {
        let shown = self.shown(natural);
        p.rect(shown, radius, theme::PANEL_TOP, theme::PANEL_BOT, 0.0);
        match &self.slide {
            None => {
                let _clip = ClipScope::open_in(p, shown);
                live.draw(p, natural, measure);
            }
            Some(s) => {
                let inset = Rect::new(
                    shown.x + CLIP_INSET,
                    shown.y + CLIP_INSET,
                    (shown.w - 2.0 * CLIP_INSET).max(0.0),
                    (shown.h - 2.0 * CLIP_INSET).max(0.0),
                );
                let _clip = ClipScope::open_in(p, inset);
                for layer in s.leaving.iter().filter(|l| l.alpha > VISIBLE) {
                    layer.table.draw(p.alpha(layer.alpha).translate(layer.x.pos, 0.0), layer.rect, measure);
                }
                if s.live_alpha > VISIBLE {
                    live.draw(p.alpha(s.live_alpha).translate(s.live_x.pos, 0.0), natural, measure);
                }
            }
        }
    }
}
