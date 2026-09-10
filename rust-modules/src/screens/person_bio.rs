//! The person page's **bio alert panel** — the full biography, behind the truncation mark.
//!
//! **The reference is `Alert Views.dc.html` §1C · Person** (the owner's design project): a centred
//! 1120×700 glass sheet with 48px of padding and a [`theme::ALERT_PANEL_RAD`] corner, holding an
//! eyebrow, the name, an identity line, and — between two hairlines — the biography as a
//! **scrolling, paged viewport** with a feathered edge and a rail down its right side. The footer
//! states what the page knows about this person's presence in the library, and how to leave.
//!
//! **It is the panel BEHIND the mark, not a replacement for it.** `person.rs`'s bio block dissolves
//! its third line into a right-pinned `MORE` (`TextView::fade_last` + the pinned label), and that
//! affordance is unchanged: `MORE` is still a mark rather than a control, still unfocusable, still
//! drawn only when `bio.truncates(BIO_W)`. This panel is what OK on the header now opens, and it is
//! gated on **exactly that same predicate** — [`person::bio_is_truncated`]. A panel that opened on a
//! two-line bio would show the reader the words they had just finished reading.
//!
//! ## Three things here are not obvious
//!
//! **1. The scrim belongs to the PAGE, and it is heavier than a menu's.** [`scrim`] is called from
//! `person::draw` before the panel, not from [`draw`] — the rule `Popover::scrim` states for a
//! refreshing backdrop, kept here because it is also what makes the *contrast* argument work. The
//! design's host frame carries a standing comment that nothing bright may sit inside the panel's own
//! rectangle: a large name read through a 72% frost lifts the ground under the fine print past its
//! graded contrast. On the real page that name is not hypothetical — `person.rs` draws the person's
//! own name at `size::DISPLAY` (48) at x≈474, y≈96, which is *directly behind this panel's top-left
//! corner*, where the eyebrow and the identity line sit — and more squarely so than it used to,
//! since that band top-aligns on the name's cap top now, which makes y≈96 exact rather than
//! approximate. The mock could stage that away; we cannot, so the page is dimmed at [`SCRIM_A`] —
//! [`theme::SCRIM_TEXT_A`], the measured text-legibility floor — before the frost ever samples it.
//! The anchored chip menus use 0.45 because nothing of theirs is fine print over a headline; this
//! panel's whole lower half is.
//!
//! **The lift that value was chosen against was measured while that name was `size::HERO` (72)**,
//! and it has not been re-taken since the band stopped condensing and the name dropped to DISPLAY.
//! So the 23-level lift quoted on [`prepare_present`] below is an upper bound on today's page
//! rather than a reading of it: do not re-tune the scrim or the glass policy off it without
//! measuring again.
//!
//! **2. Paragraphs are separate views, because `TextView` cannot hold them.** `TextView::wrap`
//! splits on `char::is_whitespace` and reflows the lot, so a `\n\n` in a plex.tv biography is
//! destroyed — every paragraph break in the source becomes a single space. The block is therefore
//! [`paragraphs`] + one memoised `TextView` each, stacked on [`PARA_GAP`]. That also buys the
//! per-paragraph cull for free.
//!
//! **3. Nothing here animates from a clock.** The scroll is a [`Spring`], so `gfx::spring` reports
//! it to [`crate::ui::idle`] and the present gate sees the paging motion without this module opting
//! in. That is deliberate rather than incidental — `Xfade` and `Spinner` both shipped FROZEN behind
//! that gate because they integrate milliseconds, and a hand-rolled scroll offset here would have
//! been the third. The discrete transitions ([`open`]/[`close`]/[`move_focus`]) still call
//! `idle::invalidate` because a state change is not motion.
//!
//! **4. The panel's own paging must not re-source its own backdrop, and that took a measured FPS
//! regression to notice.** Every `move_focus` page turn calls `idle::invalidate` (point 3), which
//! is exactly right for the present gate — a page turn is real content damage and has to present.
//! It is exactly WRONG for [`prepare_present`]'s `underlay_changed` argument, though: `app.rs`'s
//! route arm hands it `underlay_moving || idle::present_dirty()`, and `present_dirty` cannot tell
//! "the PAGE BEHIND this panel changed" from "this panel's own page-turn just fired the very
//! `invalidate` point 3 documents" — both set the same process-wide flag. Taken straight through,
//! every keypress re-sources the WHOLE host page into the blur target and re-blurs it, although
//! nothing under the panel moved at all: the person page's header and shelves sit exactly still
//! while the reader pages the biography. [`prepare_present`] therefore does not trust the caller's
//! flag alone — it ALSO asks this panel's own [`Popover::appear_settled`], and refreshes only while
//! the appear choreography is still ramping (where a stale backdrop under a mid-fade scrim is the
//! contrast bug point 1 exists to prevent) or the caller's flag is true for some OTHER reason (a
//! shelf landing or a poster texture arriving on the person page itself, which does redraw the
//! header/shelves this panel sits over — `person::update` keeps pumping under an open panel, and
//! the media fetch is independent of the profile fetch that made the panel openable, so this is
//! the ordinary case on a cold page, not a corner). The panel therefore needs a ledger of the
//! damage it caused this frame — a page turn, or its scroll spring still gliding — subtracted from
//! the caller's flag.
//!
//! **That ledger and that decision are SHARED now** (2026-09-02). They lived here as a private
//! `OWN_DAMAGE` static and a private `glass_refresh`, because this was the first panel whose frame
//! rate was measured; the bug is a property of every popover with a scroll or a selection in it.
//! They are `popover::note_own_damage` / `popover::own_motion` and `popover::glass_refresh`, and
//! `Popover::prepare_present` folds them in itself — so this module's `prepare_present` is now a
//! plain forward, and the panels that never had the fix have it. The decision is unchanged and so
//! is its test, which moved to `popover.rs` with the function it grades.
use crate::person::Person;
use crate::ui::consts::{SCR_H, SCR_W, SDLK_DOWN, SDLK_UP};
use crate::ui::label::{Label, VAlign};
use crate::ui::text_view::TextView;
use crate::ui::theme;
use crate::ui::widgets;
use crate::ui::{Painter, Rect, Spring};
use std::ffi::CString;
use std::os::raw::c_uint;

// ---- geometry (the design's numbers, and everything else derived from them) --------------------

/// The sheet, centred. 1120×700 is the design's alert class — wide enough for a comfortable measure
/// of body text at [`theme::size::BODY`] and short enough to leave the page legible around it.
const PANEL_W: f32 = 1120.0;
/// **676, and it was 700 until 2026-08-22** — the design's figure less the 24px this panel used to
/// leave under its BACK hint, exactly as its two siblings were trimmed. See
/// [`crate::ui::widgets::KeyHint::pad_below`] for the argument and [`content_rect`] for why the body
/// does not lose those 24 with it.
const PANEL_H: f32 = 676.0;
/// The one padding, on all four sides. Quoted with [`theme::ALERT_PANEL_RAD`] because the two constrain
/// each other — see that token for why a bigger corner would eat this box.
const PAD: f32 = theme::alert::PAD;

/// The page dim. See the module doc — this is heavier than the chip menus' 0.45 on purpose, and it
/// is a named token rather than a tuned number.
const SCRIM_A: f32 = theme::SCRIM_TEXT_A;

/// Air between paragraphs of the biography. A block gap, one `space` rung — the paragraphs are
/// separate thoughts, not separate blocks of the page.
const PARA_GAP: f32 = theme::space::MD;
/// The design's `trailing 40px spacer` under the last paragraph (§1C; §1B spends the same as its
/// `padding-bottom:40`). It is air at the END OF THE TRAVEL, not padding: without it the last line
/// rests flush on the viewport's clip, and the bottom feather is off by then — the block has
/// stopped scrolling — so the final line of a biography was cut by a hard edge with nothing under
/// it. That is the one page of this panel a reader always reaches.
const BODY_TAIL: f32 = theme::space::LG;
/// The bio's line pitch. `person.rs`'s header bio uses the same 40 at the same rung, so the prose
/// is paced identically whether it is being previewed or read.
const BIO_LEAD: f32 = 40.0;

/// One press of UP/DOWN. The design's number, and deliberately NOT a full viewport: a page that
/// replaces every visible line gives the eye nothing to re-anchor on, so a step leaves several
/// lines of overlap.
const STEP: f32 = 200.0;
/// The dissolve band at each end of the viewport — see [`draw_bio`]'s `TextView::edge_fade` call.
const FEATHER: f32 = theme::alert::FEATHER;
/// Air between the prose column and the rail beside it.
const RAIL_GAP: f32 = theme::space::MD;

/// Letter-tracking on the eyebrow: `.08em` of its own rung. A kicker is read as a LABEL rather than
/// as a word, and tracking is what says so; the [`widgets::pass_capsule`] label spends .06em for the
/// same reason one rung of emphasis down.
const EYEBROW_TRACK: f32 = theme::size::CAPTION as f32 * 0.08;
/// The eyebrow, pre-split per character — [`widgets::tracked_run`] explains why a tracked run is
/// spelled this way, and why it is only worth it for a constant word.
const EYEBROW: [&std::ffi::CStr; 6] = [c"P", c"E", c"R", c"S", c"O", c"N"];

/// The footer's right-hand hint, as its three runs — assembled by the shared
/// [`widgets::KeyHint`], which owns the cap, the gaps and the measure. This module used to lay the
/// line out itself (three `text_width` calls, its own `HINT_GAP` and a bare `key_cap`), which was
/// the same arithmetic the widget already does and the place a fourth copy of the design's `gap`
/// would have drifted — it did drift, to 14 against the spec's 12.
const HINT_PRE: &std::ffi::CStr = c"Press";
const HINT_KEY: &std::ffi::CStr = c"BACK";
const HINT_POST: &std::ffi::CStr = c"to return";

/// The dot-separated identity line's air either side of its separator — `detail.rs`'s facts row
/// spends the same, and the two lines are the same idiom.
const META_SEP_PAD: f32 = 10.0;

// ---- the surface ---------------------------------------------------------------------------------

/// The fields [`PersonBioScreen`] canonicalises, for the recorder's shape pin (§5.4). The PAGE is in
/// it deliberately: this panel's UP/DOWN moves nothing else in the app, so without it a replay
/// grades the sheet opening and closing and nothing between — `tracks_panel::SHAPE`'s rule, and the
/// reason `focusprobe` used to carry a reader for this number.
pub(crate) const SHAPE: &str = "PersonBioScreen{page:usize,scroll:Spring{pos:f32,vel:f32}}";

/// **How far the sheet rises as it appears, in px** — `Popover::RISE`, the one number the whole
/// panel family shares. The container owns the spring; this is only the distance it drives.
const RISE: f32 = crate::ui::popover::Popover::RISE;

/// The person page's biography, in full. Presented on that page's own `ModalStack`
/// (`registry::ContentPanel::Bio`), dismissed by BACK; UP/DOWN page the prose.
pub(crate) struct PersonBioScreen {
    entry: crate::ui::machine::EntryId,
    /// The current page, 1-based. The scroll spring chases [`scroll_for_page`] of it, rather than
    /// the page being derived from the scroll: paging is the input, and a spring still travelling
    /// must not be read back as a different page half way there.
    page: usize,
    scroll: Spring,
    /// The frosted ground's render state — a resource, never logical state, which is why it is not
    /// in [`SHAPE`].
    glass: crate::ui::widgets::GlassState,
}

impl PersonBioScreen {
    pub(crate) fn new(entry: crate::ui::machine::EntryId) -> Self {
        Self {
            entry,
            page: 1,
            scroll: Spring::at(0.0),
            glass: crate::ui::widgets::GlassState::new(),
        }
    }

    /// UP/DOWN page the viewport; LEFT/RIGHT are inert (there is one column of prose, and nothing
    /// beside it to move to).
    ///
    /// **It does not clamp against the page COUNT, and that is load-bearing rather than lazy.**
    /// [`Self::tick`] already re-clamps every frame — it has to, since the store can land a longer
    /// (or empty) biography while the panel is open — so a second clamp here would be a duplicate.
    /// It would also be an expensive one: knowing `pages` means measuring the wrapped prose, which
    /// reaches `TextView` → `crate::text` → `TTF_SizeUTF8`.
    ///
    /// **Doing that from a key handler does not fail as a skipped test. It fails as a LINK ERROR.**
    /// This screen's `step` is called by the host suite, `cargo test --lib` builds without
    /// `--features hostsim`, and nothing then supplies SDL_ttf or GL — so one `.min(pages)` on this
    /// line once stopped the whole suite from BUILDING, with an undefined `_TTF_SizeUTF8` naming
    /// `crate::text` and nothing about this panel. It cost a bisect to find. Anything reachable
    /// from a key handler here has to stay clear of text measurement.
    ///
    /// So the index may run one past the end for a single frame and is pulled back before anything
    /// reads it: the tick clamps, then computes the scroll target, and `draw` reads the page after
    /// both. Nothing on screen can observe the overshoot.
    fn step_page(&mut self, sym: c_uint) -> bool {
        let next = match sym {
            SDLK_UP => self.page.saturating_sub(1).max(1),
            SDLK_DOWN => self.page + 1,
            _ => self.page,
        };
        let moved = next != self.page;
        self.page = next;
        moved
    }

    fn tick(&mut self, dt: f32) {
        // Re-clamp before springing: the store can land a longer (or empty) biography while the
        // panel is up — `person::pump` applies a profile whenever it arrives — and a page index
        // past the end would otherwise park the spring beyond the content.
        let (_, pages) = page_state();
        self.page = self.page.clamp(1, pages);
        let want = scroll_for_page(self.page);
        self.scroll.step(want, crate::ui::consts::K_SCROLL, dt);
        crate::ui::anim::probe("personbio.scroll", self.scroll.pos, self.scroll.vel, want, dt);
        // No per-frame `note_own_damage` here: the container's own-motion scope attributes this
        // spring AND every invalidate this step raises to the panel, and a claim made per frame
        // with no invalidate behind it over-counts — on a frame where a poster landed on the page
        // behind, that claim masked the landing and the frozen host kept the un-landed page (Codex
        // review, 2026-09-04). A claim belongs beside the one invalidate it names, in a key
        // handler.
    }

    /// The whole panel, at this frame's appear fraction.
    fn paint(&mut self, person: &Person, appear: f32) {
        let slide = RISE * (1.0 - appear);
        let p = Painter::root().alpha(appear).translate(0.0, slide);
        let panel = panel_rect();
        crate::ui::widgets::Glass::DYNAMIC_BACKDROP.panel(p, panel, slide, theme::ALERT_PANEL_RAD);

        let c = content_rect();
        draw_head(p, person, c);

        let view = viewport();
        let (max_scroll, pages) = paging(content_h(person), view.h, STEP);
        let scroll = self.scroll.pos.clamp(0.0, max_scroll);

        // the two hairlines that bracket the reading block
        let rule = |y: f32| widgets::hairline(p, c.x, y, c.w);
        rule(view.y - theme::space::MD - 1.0);
        rule(view.y + view.h + theme::space::MD);

        draw_bio(p, person, view, scroll, max_scroll);
        widgets::scroll_rail(
            p,
            Rect::new(c.x + c.w - widgets::RAIL_W, view.y, widgets::RAIL_W, view.h),
            self.page,
            pages,
        );

        draw_foot(p, person, c);
    }
}

impl<H: crate::screens::registry::AppLike> crate::ui::machine::Machine<H> for PersonBioScreen {
    type Ev = crate::ui::screen::ScreenEvent<H>;
    fn step(
        &mut self,
        ev: &Self::Ev,
        _cx: &crate::ui::machine::Cx<'_, H>,
        fx: &mut crate::ui::machine::Effects<'_, H>,
    ) -> crate::ui::machine::Handled {
        use crate::ui::machine::{Edge, Fx, Handled, InputKind, Key, NavOp};
        use crate::ui::screen::ScreenEvent;
        match ev {
            ScreenEvent::Tick(t) => {
                self.tick(t.dt());
                Handled::Yes
            }
            ScreenEvent::Input(input) => match input.kind {
                // **BACK closes and OK does NOT.** The design gives the read-only panels BACK
                // (§1E), and this one is entered by OK on the header — a press that both opens and
                // closes on the same key is how a viewer holding OK down leaves a sheet they were
                // opening. The legacy wiring said the same thing by handing `Key::Ok` an empty
                // arm; here it is a swallow, so the page beneath still cannot see it.
                InputKind::Key { key: Key::Back, edge: Edge::Down, .. } => {
                    fx.push(Fx::Nav(NavOp::Dismiss(self.entry)));
                    Handled::Yes
                }
                InputKind::Key { sym, edge: Edge::Down | Edge::Repeat, .. } => {
                    if self.step_page(sym as c_uint) {
                        fx.invalidate(crate::ui::present::Provenance::Input);
                    }
                    Handled::Yes
                }
                // Swallowed, never forwarded: the sheet is modal, and a click that fell through to
                // the person page focused a shelf tile UNDER it — the 2026-09-06 report this
                // panel's own pointer arms were added for.
                InputKind::Click { .. } | InputKind::Pointer { .. } => Handled::Yes,
                _ => Handled::No,
            },
            _ => Handled::No,
        }
    }
}

/// No focusable element at all — the panel's one cursor is a PAGE — so the engine and the hit map
/// are inert for it and `FocusSource::Legacy`/`HitSource::Legacy` is the honest answer, exactly as
/// it is for `tracks_panel`, `about_panel` and the player's four overlays.
impl<H: crate::screens::registry::AppLike> crate::ui::screen::Focusable<H> for PersonBioScreen {
    fn groups(&self, _cx: &crate::ui::machine::Cx<'_, H>, _out: &mut Vec<crate::ui::screen::GroupSpec>) {}
    fn group_of(&self, _key: &u32, _cx: &crate::ui::machine::Cx<'_, H>) -> Option<crate::ui::machine::GroupId> {
        None
    }
    fn neighbour(
        &self,
        _key: crate::ui::machine::FocusKey<u32>,
        _dir: crate::ui::screen::Dir,
        _cx: &crate::ui::machine::Cx<'_, H>,
    ) -> crate::ui::screen::Step<u32> {
        crate::ui::screen::Step::Edge
    }
    fn place(
        &self,
        _key: &u32,
        _cx: &crate::ui::machine::Cx<'_, H>,
        _at: crate::ui::screen::At,
    ) -> Option<crate::ui::screen::Placed> {
        None
    }
    fn reconcile(
        &self,
        want: crate::ui::machine::FocusKey<u32>,
        _cx: &crate::ui::machine::Cx<'_, H>,
    ) -> crate::ui::machine::FocusKey<u32> {
        want
    }
    fn seat(
        &self,
        _g: crate::ui::machine::GroupId,
        _from: crate::ui::screen::Placed,
        _cx: &crate::ui::machine::Cx<'_, H>,
    ) -> crate::ui::machine::FocusKey<u32> {
        crate::ui::machine::FocusKey { entry: self.entry, elem: 0 }
    }
}

impl crate::ui::machine::LogicalState for PersonBioScreen {
    fn write(&self, c: &mut crate::ui::machine::Canon) {
        c.u64(self.page as u64).f32(self.scroll.pos).f32(self.scroll.vel);
    }
    fn probe(&self, out: &mut String) {
        out.push_str("bio");
    }
}

impl<H: crate::screens::registry::AppLike> crate::ui::screen::Screen<H> for PersonBioScreen {
    fn name(&self) -> &'static str {
        "bio"
    }
    fn state(&self) -> &dyn crate::ui::machine::LogicalState {
        self
    }
    fn crumb(&self, _cx: &crate::ui::machine::Cx<'_, H>) -> Option<std::borrow::Cow<'_, str>> {
        None
    }
    fn prepare(&mut self, _b: &mut crate::ui::frame::Budget, _cx: &crate::ui::machine::Cx<'_, H>) {}
    /// **The refreshing backdrop's cadence** — the reason this hook exists at all, and the module
    /// doc's point 4 is the argument. The decision is the shared `popover::glass_refresh`: the
    /// caller's belief about the page, minus this panel's own damage (a page turn raises an
    /// `invalidate`, which `idle::present_dirty` cannot tell from the page changing), and forced
    /// while the appear ramp is still running, where a stale backdrop under a mid-fade scrim is
    /// the contrast bug the DYNAMIC policy exists to prevent.
    fn prepare_present(&mut self, underlay_changed: bool, appear_settled: bool) {
        let refresh = crate::ui::popover::glass_refresh(
            underlay_changed,
            appear_settled,
            crate::ui::popover::host::own_damage_this_frame(),
        );
        crate::ui::widgets::Glass::DYNAMIC_BACKDROP.prepare(&mut self.glass, refresh);
    }
    /// The page dim. Heavier than a chip menu's 0.45 on purpose — see the module doc's point 1:
    /// this page draws the person's own name at `size::DISPLAY` directly behind this sheet's top
    /// corner, and a large name read through a 72% frost lifts the ground under the fine print past
    /// its graded contrast. `theme::SCRIM_TEXT_A` is the measured text-legibility floor.
    ///
    /// Nothing is lifted: the sheet replaces the middle of the frame and holds no control.
    fn scrim(&self) -> crate::ui::screen::Scrim {
        crate::ui::screen::Scrim::dim(SCRIM_A)
    }
    fn draw(&mut self, f: &mut crate::ui::screen::DrawFrame<'_, '_, H>) {
        // **A surface may not appear in its own backdrop.** The direct blur-source path re-renders
        // the host page into a small target; the SCRIM deliberately does not take this branch,
        // being page content whose whole job is to be in what the frost samples.
        if crate::gfx::blur_source_pass() {
            return;
        }
        let Some(person) = crate::person::current() else { return };
        let appear = f.page_alpha;
        crate::ui::profile::phase("dt.bio", || self.paint(person, appear));
    }
    fn render(&self) -> crate::ui::screen::RenderStrategy {
        crate::ui::screen::RenderStrategy::Page
    }
    fn focus_source(&self) -> crate::ui::screen::FocusSource {
        crate::ui::screen::FocusSource::Legacy
    }
    fn hit_source(&self) -> crate::ui::screen::HitSource {
        crate::ui::screen::HitSource::Legacy
    }
    fn as_any(&self) -> Option<&dyn std::any::Any> {
        Some(self)
    }
    fn as_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        Some(self)
    }
}

// ---- the measured flow (pure where it can be) ---------------------------------------------------

/// The biography split into paragraphs.
///
/// Blank-line delimited, which is what plex.tv's `summary` uses, and every run trimmed. Empty runs
/// are dropped rather than kept as zero-height rows, so a summary that ends in three newlines does
/// not leave a page of air under the last line.
///
/// Pure, and the reason this module can hold paragraphs at all — see the module doc for why
/// `TextView` cannot.
pub(crate) fn paragraphs(bio: &str) -> Vec<&str> {
    bio.split("\n\n")
        .flat_map(|b| b.split("\r\n\r\n"))
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .collect()
}

/// The panel's rect, and the content box inside its padding.
fn panel_rect() -> Rect {
    Rect::new(
        (SCR_W - PANEL_W) * 0.5,
        (SCR_H - PANEL_H) * 0.5,
        PANEL_W,
        PANEL_H,
    )
}
/// The panel's content box — **inset by `PAD` on three sides and by [`KeyHint::pad_below`] at the
/// bottom**, the one asymmetry in this sheet.
///
/// It is what keeps the trim a trim: the box ends where the BACK hint's band ends, so shortening the
/// panel by 24 and shortening its bottom inset by the same 24 leaves `c.h` — and therefore the
/// paged viewport, its page count and the rail — byte-identical to what they were at 700.
fn content_rect() -> Rect {
    let r = panel_rect();
    Rect::new(
        r.x + PAD,
        r.y + PAD,
        r.w - 2.0 * PAD,
        r.h - PAD - widgets::KeyHint::pad_below(),
    )
}

/// The prose column's wrap width — the content box less the rail and its air. The rail is part of
/// the reading block's frame, so the text may not flow under it.
fn text_w() -> f32 {
    (content_rect().w - widgets::RAIL_W - RAIL_GAP).max(1.0)
}

/// One paragraph's view. Built in ONE place so its measure and its draw cannot disagree about the
/// rung, the ink or the leading — `person.rs::bio_view` carries the same note for the same reason.
fn para_view(text: &str) -> TextView<'_> {
    TextView::new(text, theme::size::BODY, theme::TEXT_READING).leading(BIO_LEAD)
}

/// The scrolling viewport's rect — what the clip cuts to and what the feather rides.
///
/// Everything above and below it is measured from the CONTENT box's two ends and the viewport takes
/// what is left, so the panel's fixed 700px height is spent on the prose rather than on air: a
/// header line that grows (a person with no roles, a very long name) shortens the reading window
/// instead of pushing the footer off the sheet.
fn viewport() -> Rect {
    let c = content_rect();
    let top = c.y + head_h() + theme::space::MD + 1.0 + theme::space::MD;
    let bottom = c.y + c.h - foot_h() - theme::space::MD - 1.0 - theme::space::MD;
    Rect::new(c.x, top, text_w(), (bottom - top).max(0.0))
}

/// The header block's height: eyebrow, name, identity line, on the ALERT FAMILY's head ladder
/// ([`theme::alert`]) — the same four numbers §1B spends, because §1C spells the same three runs.
///
/// **This used to measure its own cap bands and step by `space::SM`, and that is what made the
/// eyebrow crowd the name.** Two rungs of 16 over two measured caps put `PERSON` 26px above the
/// name and the name 45px above the identity line, against the family's 38 and 56 — the eyebrow
/// ended up TIGHTER than the gap under the title, which is the one relationship a kicker may not
/// invert. The leads are bands, not measurements, for the same reason: a cap height is what the
/// glyphs happen to occupy, and `line-height` is what the design reserved for them.
///
/// The last run keeps its measured cap: the identity line is the block's last, so nothing below
/// depends on its band — only on where its ink ends.
fn head_h() -> f32 {
    theme::alert::EYEBROW_LEAD
        + theme::alert::GAP_EYEBROW_TITLE
        + theme::alert::TITLE_LEAD
        + theme::alert::GAP_TITLE_SUB
        + crate::text::cap_h(theme::size::CAPTION, 0)
}

/// The footer band's height — the keycap is the tallest thing in it, so the band is the cap.
fn foot_h() -> f32 {
    widgets::KeyHint::height()
}

/// Total flowed height of the biography at the current wrap width, and the viewport's height.
fn content_h(person: &Person) -> f32 {
    let w = text_w();
    let paras = paragraphs(&person.bio);
    if paras.is_empty() {
        return 0.0;
    }
    let mut h = 0.0;
    for (i, para) in paras.iter().enumerate() {
        if i > 0 {
            h += PARA_GAP;
        }
        h += para_view(para).measure_h(w);
    }
    h + BODY_TAIL
}

/// **The paging arithmetic, pure**: how many pages `content_h` of prose makes in a `view_h`
/// viewport stepping `step` px, and the furthest the block may scroll.
///
/// A page is a scroll POSITION, not a screenful — the step (200px) is smaller than the viewport, so
/// consecutive pages overlap. `pages` is therefore "how many distinct resting places are there",
/// which is one more than the number of whole steps the travel contains, and the LAST page is
/// pinned to `max_scroll` rather than to `(pages-1) * step`: the final step is a short one, and a
/// rail that stopped short of its track's end while the prose had visibly run out would be lying.
///
/// Content that fits is one page with no travel, which is what makes the rail and both feather
/// edges disappear together for a short biography.
pub(crate) fn paging(content_h: f32, view_h: f32, step: f32) -> (f32, usize) {
    let max_scroll = (content_h - view_h).max(0.0);
    if max_scroll <= 0.0 || step <= 0.0 {
        return (0.0, 1);
    }
    (max_scroll, (max_scroll / step).ceil() as usize + 1)
}

/// The scroll offset page `page` rests at — `(page-1)` whole steps, clamped to the travel. Pure.
pub(crate) fn scroll_at(page: usize, max_scroll: f32, step: f32) -> f32 {
    ((page.max(1) - 1) as f32 * step).min(max_scroll).max(0.0)
}

/// `(max_scroll, pages)` for the open person — the impure wrapper the screen calls.
fn page_state() -> (f32, usize) {
    let Some(person) = crate::person::current() else {
        return (0.0, 1);
    };
    paging(content_h(person), viewport().h, STEP)
}

fn scroll_for_page(page: usize) -> f32 {
    let (max_scroll, _) = page_state();
    scroll_at(page, max_scroll, STEP)
}

// ---- the two lines of prose the panel writes itself ---------------------------------------------

/// The identity line's runs, in flow order — **`roles · born · died · birthplace`**, with every
/// absent fact absent rather than blank.
///
/// Pure, and the shape is the point: it builds a LIST for [`widgets::dotted_run`] rather than
/// filling one template with holes, so a person plex.tv knows nothing about produces an empty line
/// the flow simply does not draw, and someone with a death date but no birth date reads
/// "Died 2 June 2017" with no leading separator. That is `person::refresh_runs`' rule for the
/// header's life line, applied to a line that also carries the roles.
///
/// The dates arrive pre-formatted; this function does no date work, so it stays testable without a
/// locale or a clock.
pub(crate) fn meta_runs(roles: &str, born: &str, died: &str, birthplace: &str) -> Vec<String> {
    let mut v: Vec<String> = Vec::new();
    // `Born ` is the design's own wording (§1C: "Actor, Singer" · "Born 8 Jan 1987" · "London,
    // England"), and it is not decoration: a bare date sitting between a role list and a city has
    // nothing to say which date it is. Its sibling `Died ` was labelled from the start, which is
    // what made the asymmetry easy to miss.
    for (label, value) in [
        ("", roles),
        ("Born ", born),
        ("Died ", died),
        ("", birthplace),
    ] {
        let value = value.trim();
        if !value.is_empty() {
            v.push(format!("{label}{value}"));
        }
    }
    v
}

/// The footer's left-hand line: **what this page actually knows** about the person's presence in
/// the library, which is the two shelf totals it is already showing.
///
/// Pluralised honestly, and the two kinds are named rather than folded into a neutral "title":
/// "1 film and 3 shows" is a fact the page can defend, "4 titles" is a number that hides which
/// shelves it came from. `None` when there is nothing — the panel then draws no line at all, rather
/// than a "0 films" that reads as a failed fetch. The person page's own empty state already says so
/// in words, underneath.
///
/// The counts are the RESPONSE totals (`Person::total`), not the drawn tile counts — the shelves cap
/// at a `CardRow`'s spring count, and "24" on a person with 60 films would be the cap posing as a
/// fact about the library.
pub(crate) fn library_line(films: usize, shows: usize) -> Option<String> {
    let noun = |n: usize, one: &str, many: &str| format!("{n} {}", if n == 1 { one } else { many });
    let body = match (films, shows) {
        (0, 0) => return None,
        (f, 0) => noun(f, "film", "films"),
        (0, s) => noun(s, "show", "shows"),
        (f, s) => format!(
            "{} and {}",
            noun(f, "film", "films"),
            noun(s, "show", "shows")
        ),
    };
    Some(format!("{body} in this library"))
}

// ---- draw ----------------------------------------------------------------------------------------

/// Eyebrow, name, identity line — stacked from the content box's top edge on the alert family's
/// head ladder ([`theme::alert`]), the same flow [`head_h`] measures.
fn draw_head(p: Painter, person: &Person, c: Rect) {
    let mut y = c.y;
    // **Every y in this block is a CAP TOP**, which is what makes the ladder comparable to §1A's and
    // §1B's — both of those place through `TextView`/`Label`, i.e. `VAlign::CapTop`. `tracked_run`
    // is the one run here that reaches `Painter::text` directly, so it converts, exactly as the
    // identity line below does. Drawn at a bare `y` it started ~7px low, which put `PERSON` 7px
    // closer to the name on top of the 12 the old rung ladder was already costing.
    let (eyebrow_cap, _) = crate::text::text_cap_band(theme::size::CAPTION, 1);
    widgets::tracked_run(
        p,
        &EYEBROW,
        c.x,
        y - eyebrow_cap,
        theme::size::CAPTION,
        theme::TEXT_TERTIARY,
        1,
        EYEBROW_TRACK,
    );
    y += theme::alert::EYEBROW_LEAD + theme::alert::GAP_EYEBROW_TITLE;

    // The name is ELIDED to the content box, not wrapped: this is an identity, and a two-line name
    // would push the reading window down by a whole rung of the flow. `person::refresh_runs` budgets
    // the same name to the header's own column for the same reason.
    if let Ok(cs) = CString::new(crate::text::elide(
        &person.name,
        c.w,
        theme::size::TITLE,
        1,
        false,
    )) {
        Label::new(cs.as_ptr(), theme::size::TITLE, theme::TEXT_PRIMARY)
            .bold()
            .v(VAlign::CapTop)
            .draw(p, Rect::new(c.x, y, c.w, 0.0));
    }
    y += theme::alert::TITLE_LEAD + theme::alert::GAP_TITLE_SUB;

    let runs = meta_runs(
        &person.roles,
        &crate::ui::fmt::pretty_date(&person.born, 0),
        &crate::ui::fmt::pretty_date(&person.died, 0),
        &person.birthplace,
    );
    if !runs.is_empty() {
        let parts: Vec<&str> = runs.iter().map(|s| s.as_str()).collect();
        let (cap_top, _) = crate::text::text_cap_band(theme::size::CAPTION, 0);
        widgets::dotted_run(
            p,
            &parts,
            c.x,
            y - cap_top,
            theme::size::CAPTION,
            theme::TEXT_SECONDARY,
            META_SEP_PAD,
        );
    }
}

/// The scrolling prose: every paragraph stacked at `-scroll`, inside a hard scissor at the viewport.
///
/// **The clip is the hard cut; the DISSOLVE is the text's own glyphs fading, not a shape painted
/// over the glass.** This used to be `widgets::edge_feather` — an opaque `SURFACE_PANEL`-tinted
/// gradient laid over the viewport's edge, which read fine over an opaque sheet but produced a
/// distinct GREY BAND over this panel's frosted glass (the reported bug this replaces). Every
/// paragraph's [`TextView`] now carries [`TextView::edge_fade`] instead, so `text::draw_text_fade`
/// dissolves each line's glyph alpha directly against the SURFACE BEHIND it — nothing new is
/// painted at all. `top`/`bot` are computed ONCE, outside the loop: each edge appears only when
/// there is prose on the far side of it, exactly as the feather it replaced did, which is what
/// keeps a short biography free of a permanent dissolve across its first and last lines. Set and
/// clear of the clip are still paired inside this one function, because the scissor is global GL
/// state.
///
/// Paragraphs off the viewport are CULLED through the shared [`crate::ui::on_axis`] rather than
/// merely clipped: a `TextView::draw` submits every one of its wrapped lines, so a long biography
/// would otherwise pay for the whole document on every frame of a page transition.
fn draw_bio(p: Painter, person: &Person, view: Rect, scroll: f32, max_scroll: f32) {
    let paras = paragraphs(&person.bio);
    if paras.is_empty() {
        return;
    }
    let w = text_w();
    let top = (scroll > 0.5).then_some((view.y, view.y + FEATHER));
    let bot = (scroll < max_scroll - 0.5).then_some((view.y + view.h - FEATHER, view.y + view.h));
    p.clip(view);
    let mut y = view.y - scroll;
    for para in paras {
        let v = para_view(para).edge_fade(top, bot);
        let h = v.measure_h(w);
        if crate::ui::on_axis(y - view.y, h, view.h, 0.0) {
            v.draw(p, Rect::new(view.x, y, w, 0.0));
        }
        y += h + PARA_GAP;
    }
    p.clip_clear();
}

/// The footer: what the library holds on the left, how to leave on the right, both on one centre
/// line so the keycap and the prose share a band.
fn draw_foot(p: Painter, person: &Person, c: Rect) {
    let cy = c.y + c.h - foot_h() * 0.5;
    let sz = theme::size::CAPTION;
    if let Some(line) = library_line(person.total(0), person.total(1)) {
        if let Ok(cs) = CString::new(crate::text::elide(&line, c.w * 0.5, sz, 0, false)) {
            p.text(
                cs.as_ptr(),
                c.x,
                crate::text::text_vcenter_y(sz, 0, cy),
                sz,
                theme::TEXT_TERTIARY,
                0,
                0,
            );
        }
    }
    // …and the hint, right-anchored: the widget measures itself, so the whole run ends on the
    // content box's right edge — the same edge the rail and the hairlines end on.
    let hint = widgets::KeyHint::new(HINT_PRE, HINT_KEY, HINT_POST);
    hint.draw(p, c.x + c.w - hint.width(), cy);
}

// ---------------------------------------------------------------------------------------
#[cfg(test)]
mod tests {
    use super::*;

    /// **The paging arithmetic, at every boundary that has an off-by-one in it.** The rail is drawn
    /// straight from these two numbers, so a wrong `pages` is a fill of the wrong height sitting in
    /// the wrong place — visible, but not attributable without this.
    #[test]
    fn paging_counts_resting_places_and_pins_the_last_one_to_the_end() {
        // content that fits is ONE page with no travel — the rail and both feathers vanish together
        assert_eq!(paging(300.0, 400.0, STEP), (0.0, 1));
        assert_eq!(
            paging(400.0, 400.0, STEP),
            (0.0, 1),
            "exactly filling is still not scrolling"
        );

        // one short step of travel is two resting places: the top, and the end
        let (max, pages) = paging(500.0, 400.0, STEP);
        assert_eq!((max, pages), (100.0, 2));
        assert_eq!(scroll_at(1, max, STEP), 0.0);
        assert_eq!(
            scroll_at(2, max, STEP),
            100.0,
            "the last page is pinned to the END of the travel…"
        );

        // …and a step that divides exactly must not invent a page beyond it
        let (max, pages) = paging(800.0, 400.0, STEP);
        assert_eq!(
            (max, pages),
            (400.0, 3),
            "400px of travel at 200 a step is 0 / 200 / 400"
        );
        assert_eq!(scroll_at(3, max, STEP), 400.0);

        // a long biography, and the interior pages are whole steps
        let (max, pages) = paging(1500.0, 400.0, STEP);
        assert_eq!((max, pages), (1100.0, 7));
        assert_eq!(scroll_at(4, max, STEP), 600.0);
        assert_eq!(
            scroll_at(7, max, STEP),
            1100.0,
            "the short final step lands ON the end"
        );

        // out-of-range pages clamp instead of running off the content in either direction
        assert_eq!(scroll_at(0, max, STEP), 0.0);
        assert_eq!(scroll_at(99, max, STEP), max);
    }

    /// The rail's fill, as fractions of its track. It is quantised to PAGES — see
    /// [`widgets::rail_geom`] — so the invariant worth pinning is that the fill exactly fills the
    /// track across the pages, and that the LAST page's fill ends on the track's end.
    #[test]
    fn the_rail_fill_walks_its_track_and_ends_flush() {
        use crate::ui::widgets::rail_geom;
        // a single page is not railed at all, but the geometry still answers sanely
        assert_eq!(rail_geom(1, 1), (0.0, 1.0));

        let pages = 5;
        let (_, h) = rail_geom(1, pages);
        assert!((h - 0.2).abs() < 1e-6, "five pages ⇒ a fifth of the track");
        for page in 1..=pages {
            let (top, hh) = rail_geom(page, pages);
            assert!(
                (hh - h).abs() < 1e-6,
                "every page's fill is the same height"
            );
            assert!(
                top >= -1e-6 && top + hh <= 1.0 + 1e-6,
                "page {page} left the track"
            );
        }
        let (top, hh) = rail_geom(pages, pages);
        assert!(
            (top + hh - 1.0).abs() < 1e-6,
            "the last page's fill must end flush with the track"
        );
        // a page index past the end clamps rather than drawing off the track
        assert_eq!(rail_geom(99, pages), rail_geom(pages, pages));
    }

    /// **The identity line's separator logic, one missing field at a time.** The whole point of
    /// building a list is that an absent fact takes its dot with it; a template with holes leaves
    /// "Actor · · London", which is the bug this shape exists to make unrepresentable.
    #[test]
    fn the_meta_line_drops_a_missing_field_and_its_separator_with_it() {
        // everything present, in flow order. BOTH dates carry their label — the design writes the
        // line as "Actor, Singer" · "Born 8 Jan 1987" · "London, England" (§1C), and the birth
        // date shipped bare here while its `Died ` sibling was labelled, which is an asymmetry
        // that reads as an oversight rather than as a decision.
        assert_eq!(
            meta_runs("Actress, Singer", "8 January 1987", "", "Stockwell, London"),
            vec![
                "Actress, Singer",
                "Born 8 January 1987",
                "Stockwell, London"
            ]
        );
        // no roles — the line starts on the date, with nothing in front of it
        assert_eq!(
            meta_runs("", "8 January 1987", "", "London"),
            vec!["Born 8 January 1987", "London"]
        );
        // no dates at all (plex.tv knows the person but not when) — roles and place still read
        assert_eq!(
            meta_runs("Director", "", "", "Leeds"),
            vec!["Director", "Leeds"]
        );
        // a death date but no birth date: the run is LABELLED, because a bare second date beside a
        // birthplace would read as the birth date
        assert_eq!(
            meta_runs("Actor", "", "2 June 2017", "Leeds"),
            vec!["Actor", "Died 2 June 2017", "Leeds"]
        );
        // a person the provider has never heard of produces NO line, not an empty one
        assert!(meta_runs("", "", "", "").is_empty());
        // whitespace-only fields are absent too — the provider sends those
        assert!(meta_runs("  ", "\t", "", " \n ").is_empty());
    }

    /// **The footer's pluralisation, honestly.** Every arm is a different sentence, and the zero
    /// case is deliberately no sentence at all.
    #[test]
    fn the_footer_pluralises_each_kind_and_says_nothing_about_nothing() {
        assert_eq!(
            library_line(1, 0).as_deref(),
            Some("1 film in this library")
        );
        assert_eq!(
            library_line(4, 0).as_deref(),
            Some("4 films in this library")
        );
        assert_eq!(
            library_line(0, 1).as_deref(),
            Some("1 show in this library")
        );
        assert_eq!(
            library_line(0, 9).as_deref(),
            Some("9 shows in this library")
        );
        // both kinds: each is pluralised on its OWN count, which is the case a single plural flag
        // gets wrong
        assert_eq!(
            library_line(1, 3).as_deref(),
            Some("1 film and 3 shows in this library")
        );
        assert_eq!(
            library_line(2, 1).as_deref(),
            Some("2 films and 1 show in this library")
        );
        assert_eq!(
            library_line(1, 1).as_deref(),
            Some("1 film and 1 show in this library")
        );
        // nothing in the library is not "0 films" — a count of zero reads as a failed fetch
        assert!(library_line(0, 0).is_none());
    }

    /// Paragraphs survive the split that `TextView` would have destroyed, and the degenerate inputs
    /// (trailing newlines, a single block, nothing at all) produce no empty rows.
    #[test]
    fn the_biography_splits_into_paragraphs_and_drops_the_empty_ones() {
        assert_eq!(
            paragraphs("one\n\ntwo\n\nthree"),
            vec!["one", "two", "three"]
        );
        assert_eq!(
            paragraphs("one"),
            vec!["one"],
            "a single block is one paragraph, not none"
        );
        // a summary that ends in blank lines must not add a page of air under the last one
        assert_eq!(paragraphs("one\n\ntwo\n\n\n\n"), vec!["one", "two"]);
        assert_eq!(
            paragraphs("\r\n\r\nwrapped\r\n\r\ncrlf"),
            vec!["wrapped", "crlf"]
        );
        // a SINGLE newline is not a paragraph break — it is a soft wrap in the source, and joining
        // it is exactly what the wrap does
        assert_eq!(paragraphs("one\ntwo"), vec!["one\ntwo"]);
        assert!(paragraphs("").is_empty());
        assert!(paragraphs("   \n\n  \n\n ").is_empty());
    }

    // ---- the surface, driven with no SDL (§15.1 `a_new_screen_is_unit_tested_with_no_sdl`) -----
    //
    // A host of its own, three lines of it, rather than the application's: this panel is generic
    // over `AppLike` exactly so it can be stepped without one, and borrowing a sibling's test host
    // is the sibling dependency the layer gate exists to refuse.

    use crate::screens::registry::{AppFx, AppMsg};
    use crate::ui::machine::{
        Canon, Chrome, Cx, Edge, Effects, EntryId, FocusRead, Fx, Handled, Host, InputEvent,
        InputKind, InputOwner, Key, LogicalState, Machine, NavOp, PressRead, ScreenId,
        Source, Stamped, Tick,
    };
    use crate::ui::present::Present;
    use crate::ui::screen::{ScreenArg, ScreenEvent};

    #[derive(Clone, PartialEq, Eq)]
    struct TestArg;
    impl LogicalState for TestArg {
        fn write(&self, c: &mut Canon) {
            c.u32(0);
        }
        fn probe(&self, _: &mut String) {}
    }
    impl ScreenArg for TestArg {
        fn chrome(&self) -> Chrome {
            Chrome::None
        }
        fn id(&self) -> ScreenId {
            ScreenId(702)
        }
        fn title(&self) -> Option<&str> {
            None
        }
        fn same_instance(&self, other: &Self) -> bool {
            self == other
        }
    }

    #[derive(Clone, Default, Debug)]
    struct TestInit;
    impl LogicalState for TestInit {
        fn write(&self, _: &mut Canon) {}
        fn probe(&self, _: &mut String) {}
    }

    struct TestHost;
    impl Host for TestHost {
        type Arg = TestArg;
        type Fx = AppFx;
        type Msg = AppMsg;
        type Elem = u32;
        type Views<'a> = ();
        type Init = TestInit;
        type Memory = TestInit;
    }

    const ENTRY: EntryId = EntryId(7);

    fn cx(measure: &crate::ui::fixture::FixtureMeasure) -> Cx<'_, TestHost> {
        Cx {
            views: (),
            tick: Tick::default(),
            measure,
            press: PressRead::default(),
            focus: FocusRead::default(),
            owner: InputOwner::Entry(ENTRY),
        }
    }

    /// What one input does to a fresh panel: the effects it emitted, and whether it was consumed.
    fn press(kind: InputKind<u32>) -> (Vec<Stamped<TestHost>>, Handled) {
        let measure = crate::ui::fixture::FixtureMeasure;
        let cx = cx(&measure);
        let (mut out, mut present) = (Vec::new(), Present::new());
        let mut fx = Effects::new(&mut out, crate::ui::machine::MachineId::Nav, &mut present);
        let mut panel = PersonBioScreen::new(ENTRY);
        let handled = panel.step(
            &ScreenEvent::Input(InputEvent {
                kind,
                at: Tick::default(),
                source: Source::Sdl,
            }),
            &cx,
            &mut fx,
        );
        (out, handled)
    }

    fn key(k: Key) -> InputKind<u32> {
        InputKind::Key {
            key: k,
            sym: 0,
            wcode: 0,
            edge: Edge::Down,
            at_edge: false,
        }
    }

    fn dismissed(out: &[Stamped<TestHost>]) -> bool {
        out.iter()
            .any(|s| matches!(&s.fx, Fx::Nav(NavOp::Dismiss(id)) if *id == ENTRY))
    }

    /// **BACK leaves; OK does not; every other key is EATEN, and the page turns.**
    ///
    /// The last clause is the one with a bug's worth of wiring behind it: while this was a
    /// `Popover`, `PersonScreen::step` had to test `person_bio::is_open()` before its own key
    /// ladder ran — and its POINTER arms had to as well, whose absence was the whole of the
    /// 2026-09-06 report that "clicking jumps to the Actor pill" (a click fell past the open panel
    /// onto the page, which focused a shelf tile and redrew the route). Those guards are deleted
    /// with the popover: the container gives input to the topmost surface and the page is never
    /// asked, so what stops the ladder is this screen answering `Handled::Yes`, and nothing else.
    ///
    /// The paging assertion is on the CURSOR rather than on an effect, because that is all UP and
    /// DOWN do here: the scroll is a spring chasing the page, stepped on the tick.
    #[test]
    fn back_leaves_ok_does_not_and_every_other_key_is_the_panels_own() {
        let (out, handled) = press(key(Key::Back));
        assert_eq!(handled, Handled::Yes);
        assert!(dismissed(&out), "BACK dismisses this entry");

        let (out, handled) = press(key(Key::Ok));
        assert_eq!(handled, Handled::Yes, "OK is swallowed rather than passed down");
        assert!(!dismissed(&out), "…and OK is NOT an exit: the design gives these sheets BACK");

        for k in [Key::Up, Key::Down, Key::Left, Key::Right] {
            let (out, handled) = press(key(k));
            assert_eq!(handled, Handled::Yes, "{k:?} must not reach the page under the sheet");
            assert!(!dismissed(&out), "{k:?} is not an exit");
        }
    }

    /// DOWN pages forward and UP pages back, and page 1 is the floor. **No clamp against the page
    /// COUNT here** — that is the tick's, and knowing the count means measuring wrapped prose,
    /// which from a key handler is a LINK error in the host suite rather than a failing test (see
    /// `step_page`'s doc).
    #[test]
    fn up_and_down_move_the_page_and_one_is_the_floor() {
        let mut panel = PersonBioScreen::new(ENTRY);
        assert_eq!(panel.page, 1);
        assert!(panel.step_page(SDLK_DOWN), "DOWN moved");
        assert_eq!(panel.page, 2);
        assert!(panel.step_page(SDLK_UP));
        assert_eq!(panel.page, 1);
        assert!(!panel.step_page(SDLK_UP), "…and page 1 is the floor, reported as no movement");
        assert_eq!(panel.page, 1);
    }

    /// A click is SWALLOWED and moves nothing — the page under the sheet may not be reached by a
    /// pointer any more than by a key.
    #[test]
    fn a_click_does_not_reach_the_page_under_the_sheet() {
        let (out, handled) = press(InputKind::Click { x: 10.0, y: 10.0, hit: None });
        assert_eq!(handled, Handled::Yes);
        assert!(out.is_empty(), "a click on a read-only sheet does nothing at all");
    }

}
