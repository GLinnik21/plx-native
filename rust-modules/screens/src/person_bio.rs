//! The person page's **bio alert panel** — the full biography, behind the truncation mark.
//!
//! **The reference is `Alert Views.dc.html` §1C · Person** (the owner's design project): a centred
//! 1120×700 sheet with 48px of padding and a [`theme::ALERT_PANEL_RAD`] corner, holding an
//! eyebrow, the name, an identity line, and — between two hairlines — the biography as a
//! **scrolling, paged viewport** with a feathered edge and a rail down its right side. The footer
//! states what the page knows about this person's presence in the library, and how to leave.
//!
//! **It is the panel BEHIND the mark, not a replacement for it.** `person.rs`'s bio block dissolves
//! its third line into a right-pinned `MORE` (`TextView::fade_last` + the pinned label), and that
//! affordance is unchanged: `MORE` is still a mark rather than a control, still unfocusable, still
//! drawn only when `bio.truncates(BIO_W)`. This panel is what OK on the header now opens, and it is
//! gated on **exactly that same predicate** — `PersonScreen::bio_more` (the header's measured `bio.truncates(BIO_W)`). A panel that opened on a
//! two-line bio would show the reader the words they had just finished reading.
//!
//! ## Three things here are not obvious
//!
//! **1. Its ground is the page's own light, latched once, and its dim is heavier than a menu's.**
//! The sheet stands on `widgets::panel_ground`: the container's underlay field, sampled from the
//! UNDIMMED page at the head of the dim (`containers::modal::ModalUnderlay`), drawn through the
//! panel's own window and frosted. It used to be the app's one `Glass::DYNAMIC_BACKDROP`, re-blurring
//! the host on every changed present, because a CACHED snapshot taken while its own scrim was still
//! ramping through zero frosted an undimmed page — and on this page that is not hypothetical:
//! `person.rs` draws the person's own name at `size::DISPLAY` (48) at x≈474, y≈96, *directly behind
//! this panel's top-left corner*, where the eyebrow and the identity line sit. A 15x8 field cannot
//! carry a name — a box 128px wide reduces lettering to a faint lift of its cell — and the panel's
//! luma ceiling (`theme::underlay::PANEL_LUMA_MAX`) caps even that, so the refreshing backdrop, its
//! cadence hook and the FPS work it needed went with it. The page around the panel is still dimmed
//! at [`theme::underlay::DIM_PROSE`] — the measured text-legibility floor [`theme::SCRIM_TEXT_A`],
//! as the PROSE role's weight — because a panel of fine print over a headline should stand further
//! forward than a menu does.
//!
//! **2. Paragraphs are separate views, because `TextView` cannot hold them.** `TextView::wrap`
//! splits on `char::is_whitespace` and reflows the lot, so a `\n\n` in a plex.tv biography is
//! destroyed — every paragraph break in the source becomes a single space. The block is therefore
//! [`paragraphs`] + one memoised `TextView` each, stacked on [`PARA_GAP`]. That also buys the
//! per-paragraph cull for free.
//!
//! **3. Nothing here animates from a clock.** The scroll is a [`Spring`], so `gfx::spring` reports
//! it to [`plx_machine::idle`] and the present gate sees the paging motion without this module opting
//! in. That is deliberate rather than incidental — `Xfade` and `Spinner` both shipped FROZEN behind
//! that gate because they integrate milliseconds, and a hand-rolled scroll offset here would have
//! been the third. The discrete transitions ([`open`]/[`close`]/[`move_focus`]) still call
//! `idle::invalidate` because a state change is not motion.
use plx_data::person::Person;
use plx_ui::consts::{SCR_H, SCR_W};
use plx_ui::label::{Label, VAlign};
use plx_ui::theme;
use plx_ui::widgets;
use plx_ui::prose_scroll::{self, paging, ProseScroll};
use plx_ui::{Painter, Rect};
use std::ffi::CString;
use std::os::raw::c_uint;

// ---- geometry (the design's numbers, and everything else derived from them) --------------------

/// The sheet, centred. 1120×700 is the design's alert class — wide enough for a comfortable measure
/// of body text at [`theme::size::BODY`] and short enough to leave the page legible around it.
const PANEL_W: f32 = 1120.0;
/// **676, and it was 700 until 2026-08-22** — the design's figure less the 24px this panel used to
/// leave under its BACK hint, exactly as its two siblings were trimmed. See
/// [`plx_ui::widgets::KeyHint::pad_below`] for the argument and [`content_rect`] for why the body
/// does not lose those 24 with it.
const PANEL_H: f32 = 676.0;
/// The one padding, on all four sides. Quoted with [`theme::ALERT_PANEL_RAD`] because the two constrain
/// each other — see that token for why a bigger corner would eat this box.
const PAD: f32 = theme::alert::PAD;

/// The bio's line pitch. `person.rs`'s header bio uses the same 40 at the same rung, so the prose
/// is paced identically whether it is being previewed or read.
const BIO_LEAD: f32 = 40.0;
/// One press of UP/DOWN: five lines of the bio's pitch (200), the design's number and deliberately
/// NOT a full viewport (see [`prose_scroll`]).
const STEP: f32 = 5.0 * BIO_LEAD;

/// The footer's right-hand hint, as its three runs — assembled by the shared
/// [`widgets::KeyHint`], which owns the cap, the gaps and the measure. This module used to lay the
/// line out itself (three `text_width` calls, its own `HINT_GAP` and a bare `key_cap`), which was
/// the same arithmetic the widget already does and the place a fourth copy of the design's `gap`
/// would have drifted — it did drift, to 14 against the spec's 12.
const HINT_KEY: &std::ffi::CStr = c"BACK";

/// The dot-separated identity line's air either side of its separator — `detail.rs`'s facts row
/// spends the same, and the two lines are the same idiom.
const META_SEP_PAD: f32 = 10.0;

// ---- the surface ---------------------------------------------------------------------------------

/// The fields [`PersonBioScreen`] canonicalises, for the recorder's shape pin (§5.4). The PAGE is in
/// it deliberately: this panel's UP/DOWN moves nothing else in the app, so without it a replay
/// grades the sheet opening and closing and nothing between — `tracks_panel::SHAPE`'s rule, and the
/// reason `focusprobe` used to carry a reader for this number.
pub const SHAPE: &str = "PersonBioScreen{page:usize,scroll:Spring{pos:f32,vel:f32}}";

/// **How far the sheet rises as it appears, in px** — `Popover::RISE`, the one number the whole
/// panel family shares. The container owns the spring; this is only the distance it drives.
const RISE: f32 = plx_ui::popover::Popover::RISE;

/// The person page's biography, in full. Presented on that page's own `ModalStack`
/// (`registry::ContentPanel::Bio`), dismissed by BACK; UP/DOWN page the prose.
pub struct PersonBioScreen {
    entry: plx_machine::machine::EntryId,
    /// The page cursor and the spring that chases it — the viewport both read-only prose panels
    /// share ([`prose_scroll`]).
    scroll: ProseScroll,
}

impl PersonBioScreen {
    pub fn new(entry: plx_machine::machine::EntryId) -> Self {
        Self {
            entry,
            scroll: ProseScroll::new(),
        }
    }

    /// UP/DOWN page the viewport; LEFT/RIGHT are inert (there is one column of prose, and nothing
    /// beside it to move to).
    ///
    /// **It does not clamp against the page COUNT, and that is load-bearing rather than lazy.**
    /// [`Self::tick`] already re-clamps every frame — it has to, since the store can land a longer
    /// (or empty) biography while the panel is open — so a second clamp here would be a duplicate.
    /// It would also be an expensive one: knowing `pages` means measuring the wrapped prose, which
    /// reaches `TextView` → `plx_gfx::text` → `TTF_SizeUTF8`.
    ///
    /// **Doing that from a key handler does not fail as a skipped test. It fails as a LINK ERROR.**
    /// This screen's `step` is called by the host suite, `cargo test --lib` builds without
    /// `--features hostsim`, and nothing then supplies SDL_ttf or GL — so one `.min(pages)` on this
    /// line once stopped the whole suite from BUILDING, with an undefined `_TTF_SizeUTF8` naming
    /// `plx_gfx::text` and nothing about this panel. It cost a bisect to find. Anything reachable
    /// from a key handler here has to stay clear of text measurement.
    ///
    /// So the index may run one past the end for a single frame and is pulled back before anything
    /// reads it: the tick clamps, then computes the scroll target, and `draw` reads the page after
    /// both. Nothing on screen can observe the overshoot.
    fn step_page(&mut self, sym: c_uint) -> bool {
        self.scroll.step_page(sym)
    }

    fn tick(&mut self, dt: f32, person: Option<&Person>) {
        // Re-clamp before springing: the store can land a longer (or empty) biography while the
        // panel is up — `person::pump` applies a profile whenever it arrives — and a page index
        // past the end would otherwise park the spring beyond the content.
        let (max_scroll, pages) = person.map(page_state).unwrap_or((0.0, 1));
        self.scroll.follow(dt, max_scroll, pages, STEP, "personbio.scroll");
        // No per-frame `note_own_damage` here: the container's own-motion scope attributes this
        // spring AND every invalidate this step raises to the panel, and a claim made per frame
        // with no invalidate behind it over-counts — on a frame where a poster landed on the page
        // behind, that claim masked the landing and the frozen host kept the un-landed page (Codex
        // review, 2026-09-04). A claim belongs beside the one invalidate it names, in a key
        // handler.
    }

    /// The whole panel, at this frame's appear fraction.
    fn paint(
        &mut self,
        person: &Person,
        appear: f32,
        measure: &dyn plx_machine::machine::Measure,
        field: Option<&plx_ui::underlay::UnderlayField>,
    ) {
        let slide = RISE * (1.0 - appear);
        let p = Painter::root().alpha(appear).translate(0.0, slide);
        let panel = panel_rect();
        plx_ui::widgets::panel_ground(p, panel, theme::ALERT_PANEL_RAD, field);

        let c = content_rect();
        draw_head(p, person, c, measure);

        let view = viewport();
        let (max_scroll, pages) = paging(content_h(person), view.h, STEP);
        let scroll = self.scroll.offset(max_scroll);

        // the two hairlines that bracket the reading block
        let rule = |y: f32| widgets::hairline(p, c.x, y, c.w);
        rule(view.y - theme::space::MD - 1.0);
        rule(view.y + view.h + theme::space::MD);

        draw_bio(p, person, view, scroll, max_scroll);
        prose_scroll::draw_rail(p, view, c.x + c.w, self.scroll.page, pages);

        draw_foot(p, person, c, measure);
    }
}

impl<H: crate::registry::AppLike + crate::registry::PersonLike> plx_machine::machine::Machine<H> for PersonBioScreen {
    type Ev = plx_ui::screen::ScreenEvent<H>;
    fn step(
        &mut self,
        ev: &Self::Ev,
        cx: &plx_machine::machine::Cx<'_, H>,
        fx: &mut plx_machine::machine::Effects<'_, H>,
    ) -> plx_machine::machine::Handled {
        use plx_machine::machine::{Edge, Fx, Handled, InputKind, Key, NavOp};
        use plx_ui::screen::ScreenEvent;
        match ev {
            ScreenEvent::Tick(t) => {
                self.tick(t.dt(), H::person(cx).current());
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
                        fx.invalidate(plx_machine::present::Provenance::Input);
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

/// **No focusable element at all — the panel's one cursor is a PAGE, not a `GroupSpec`.** That
/// used to be the reason this screen answered `FocusSource::Legacy`/`HitSource::Legacy`
/// (restructure phase 12's D2 converts every remaining Legacy answerer to the uniform `Engine`
/// contract, `tracks_panel`/`about_panel`/the player among them). It is still the honest
/// description of what this `Focusable` impl declares — zero groups — and an empty declaration is
/// a legitimate one: the engine and the hit map ask exactly what a populated screen's `Focusable`
/// would be asked, get nothing back, and fall through to `Outcome::Nothing` at every step
/// (`enter`, `reconcile`) rather than doing anything — see
/// `engine_paths_are_inert_on_a_panel_with_no_focusable_element` below, which proves it against
/// the same `FocusEngine` entry points `ui/dispatch.rs` calls. The page cursor stays exactly what
/// it always was: `Self::page`, moved by `step_page` from the screen's own `step`, sprung to by
/// `tick` — the engine has no opinion about it, under either source.
impl<H: crate::registry::AppLike + crate::registry::PersonLike> plx_ui::screen::Focusable<H> for PersonBioScreen {
    fn groups(&self, _cx: &plx_machine::machine::Cx<'_, H>, _out: &mut Vec<plx_ui::screen::GroupSpec>) {}
    fn group_of(&self, _key: &u32, _cx: &plx_machine::machine::Cx<'_, H>) -> Option<plx_machine::machine::GroupId> {
        None
    }
    fn neighbour(
        &self,
        _key: plx_machine::machine::FocusKey<u32>,
        _dir: plx_ui::screen::Dir,
        _cx: &plx_machine::machine::Cx<'_, H>,
    ) -> plx_ui::screen::Step<u32> {
        plx_ui::screen::Step::Edge
    }
    fn place(
        &self,
        _key: &u32,
        _cx: &plx_machine::machine::Cx<'_, H>,
        _at: plx_ui::screen::At,
    ) -> Option<plx_ui::screen::Placed> {
        None
    }
    fn reconcile(
        &self,
        want: plx_machine::machine::FocusKey<u32>,
        _cx: &plx_machine::machine::Cx<'_, H>,
    ) -> plx_machine::machine::FocusKey<u32> {
        want
    }
    fn seat(
        &self,
        _g: plx_machine::machine::GroupId,
        _from: plx_ui::screen::Placed,
        _cx: &plx_machine::machine::Cx<'_, H>,
    ) -> plx_machine::machine::FocusKey<u32> {
        plx_machine::machine::FocusKey { entry: self.entry, elem: 0 }
    }
}

impl plx_machine::machine::LogicalState for PersonBioScreen {
    fn write(&self, c: &mut plx_machine::machine::Canon) {
        self.scroll.write(c);
    }
    fn probe(&self, out: &mut String) {
        out.push_str("bio");
    }
}

impl<H: crate::registry::AppLike + crate::registry::PersonLike> plx_ui::screen::Screen<H> for PersonBioScreen {
    fn name(&self) -> &'static str {
        "bio"
    }
    fn state(&self) -> &dyn plx_machine::machine::LogicalState {
        self
    }
    fn crumb(&self, _cx: &plx_machine::machine::Cx<'_, H>) -> Option<std::borrow::Cow<'_, str>> {
        None
    }
    fn prepare(&mut self, _b: &mut plx_ui::frame::Budget, _cx: &plx_machine::machine::Cx<'_, H>) {}
    /// The page dim, at the PROSE role. Heavier than a menu's on purpose — see the module doc's
    /// point 1: this page draws the person's own name at `size::DISPLAY` directly behind this
    /// sheet's top corner, and the page around a panel of fine print should recede further than
    /// around a menu. `theme::underlay::DIM_PROSE` restates the measured text-legibility floor
    /// `theme::SCRIM_TEXT_A` as this role's weight.
    ///
    /// Nothing is lifted: the sheet replaces the middle of the frame and holds no control.
    fn scrim(&self) -> plx_ui::screen::Scrim {
        plx_ui::screen::Scrim::dim(theme::underlay::DIM_PROSE)
    }
    fn draw(&mut self, f: &mut plx_ui::screen::DrawFrame<'_, '_, H>) {
        // **A surface is never part of a blur source.** The direct blur-source path (the chrome's
        // glass — the only glass there is) re-renders the host page into a small target; a
        // panel drawn into it would be blurred into the bar under its own frost.
        if plx_gfx::gfx::blur_source_pass() {
            return;
        }
        let Some(person) = H::person(f.cx).current() else { return };
        let appear = f.page_alpha;
        let measure = f.measure;
        let field = f.underlay;
        plx_ui::profile::phase("dt.bio", || self.paint(person, appear, measure, field));
    }
    fn render(&self) -> plx_ui::screen::RenderStrategy {
        plx_ui::screen::RenderStrategy::Page
    }
    fn focus_source(&self) -> plx_ui::screen::FocusSource {
        plx_ui::screen::FocusSource::Engine
    }
    fn hit_source(&self) -> plx_ui::screen::HitSource {
        plx_ui::screen::HitSource::Engine
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
pub fn paragraphs(bio: &str) -> Vec<&str> {
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
    prose_scroll::text_w(content_rect().w)
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
    // `viewport()`/`page_state()` call this from BOTH the draw path and pagination arithmetic
    // reached from key handling, so no single `&dyn Measure` from a `DrawFrame` covers every call
    // site — going through `TtfMeasure` (spec §4.3's device/simulator impl) directly is exactly
    // the capability a draw-time caller would have handed in, and `cap_h` is a pure `f(sz)` font
    // metric no string or replay state can move.
    use plx_machine::machine::Measure;
    theme::alert::EYEBROW_LEAD
        + theme::alert::GAP_EYEBROW_TITLE
        + theme::alert::TITLE_LEAD
        + theme::alert::GAP_TITLE_SUB
        + plx_gfx::text::TtfMeasure.cap_h(theme::size::CAPTION)
}

/// The footer band's height — the keycap is the tallest thing in it, so the band is the cap.
fn foot_h() -> f32 {
    widgets::KeyHint::height()
}

/// Total flowed height of the biography at the current wrap width, and the viewport's height.
fn content_h(person: &Person) -> f32 {
    prose_scroll::content_h(&paragraphs(&person.bio), text_w(), BIO_LEAD)
}

/// `(max_scroll, pages)` for the open person — the impure wrapper the screen calls.
fn page_state(person: &Person) -> (f32, usize) {
    paging(content_h(person), viewport().h, STEP)
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
pub fn meta_runs(roles: &str, born: &str, died: &str, birthplace: &str) -> Vec<String> {
    let mut runs = Vec::new();
    if !roles.trim().is_empty() { runs.push(roles.trim().to_owned()); }
    if !born.trim().is_empty() { runs.push(plx_platform::i18n::msg::browse_person_born(born.trim())); }
    if !died.trim().is_empty() { runs.push(plx_platform::i18n::msg::browse_person_died(died.trim())); }
    if !birthplace.trim().is_empty() { runs.push(birthplace.trim().to_owned()); }
    runs
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
pub fn library_line(films: usize, shows: usize) -> Option<String> {
    match (films, shows) {
        (0, 0) => None,
        (films, 0) => Some(plx_platform::i18n::msg::browse_person_library_one(&plx_platform::i18n::msg::browse_person_films(films as i64))),
        (0, shows) => Some(plx_platform::i18n::msg::browse_person_library_one(&plx_platform::i18n::msg::browse_person_shows(shows as i64))),
        (films, shows) => Some(plx_platform::i18n::msg::browse_person_library_both(
            &plx_platform::i18n::msg::browse_person_films(films as i64), &plx_platform::i18n::msg::browse_person_shows(shows as i64))),
    }
}

// ---- draw ----------------------------------------------------------------------------------------

/// Eyebrow, name, identity line — stacked from the content box's top edge on the alert family's
/// head ladder ([`theme::alert`]), the same flow [`head_h`] measures.
fn draw_head(p: Painter, person: &Person, c: Rect, measure: &dyn plx_machine::machine::Measure) {
    let mut y = c.y;
    Label::new(plx_platform::i18n::msg::browse_person_eyebrow_c().as_ptr(), theme::size::CAPTION, theme::TEXT_TERTIARY)
        .bold().h(theme::alert::TEXT_ALIGN).v(VAlign::CapTop).draw(p, Rect::new(c.x, y, c.w, 0.0));
    y += theme::alert::EYEBROW_LEAD + theme::alert::GAP_EYEBROW_TITLE;

    // The name is ELIDED to the content box, not wrapped: this is an identity, and a two-line name
    // would push the reading window down by a whole rung of the flow. `person::refresh_runs` budgets
    // the same name to the header's own column for the same reason.
    if let Ok(cs) = CString::new(plx_gfx::text::elide_by(&person.name, c.w, false, |t| {
        measure.width_str(t, theme::size::TITLE, true)
    })) {
        Label::new(cs.as_ptr(), theme::size::TITLE, theme::TEXT_PRIMARY)
            .h(theme::alert::TEXT_ALIGN)
            .bold()
            .v(VAlign::CapTop)
            .draw(p, Rect::new(c.x, y, c.w, 0.0));
    }
    y += theme::alert::TITLE_LEAD + theme::alert::GAP_TITLE_SUB;

    let runs = meta_runs(
        &person.roles.join(", "),
        &plx_ui::fmt::pretty_date(&person.born, 0),
        &plx_ui::fmt::pretty_date(&person.died, 0),
        &person.birthplace,
    );
    if !runs.is_empty() {
        let parts: Vec<&str> = runs.iter().map(|s| s.as_str()).collect();
        let (cap_top, _) = plx_gfx::text::text_cap_band(theme::size::CAPTION, 0);
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

/// The scrolling prose — [`prose_scroll::draw`], which owns the clip, the text-glyph dissolve (not
/// a shape painted over the glass: that was the grey band `widgets::edge_feather` made over this
/// panel's frost) and the per-paragraph cull. A paragraph is its own view because `TextView` cannot
/// hold a `\n\n` — see the module doc.
fn draw_bio(p: Painter, person: &Person, view: Rect, scroll: f32, max_scroll: f32) {
    prose_scroll::draw(p, &paragraphs(&person.bio), BIO_LEAD, view, scroll, max_scroll);
}

/// The footer: what the library holds on the left, how to leave on the right, both on one centre
/// line so the keycap and the prose share a band.
fn draw_foot(p: Painter, person: &Person, c: Rect, measure: &dyn plx_machine::machine::Measure) {
    let cy = c.y + c.h - foot_h() * 0.5;
    let sz = theme::size::CAPTION;
    if let Some(line) = library_line(person.total(0), person.total(1)) {
        if let Ok(cs) = CString::new(plx_gfx::text::elide_by(&line, c.w * 0.5, false, |t| {
            measure.width_str(t, sz, false)
        })) {
            p.text(
                cs.as_ptr(),
                c.x,
                plx_gfx::text::text_vcenter_y(sz, 0, cy),
                sz,
                theme::TEXT_TERTIARY,
                0,
                0,
            );
        }
    }
    // …and the hint, right-anchored: the widget measures itself, so the whole run ends on the
    // content box's right edge — the same edge the rail and the hairlines end on.
    let hint = widgets::KeyHint::translated(plx_platform::i18n::msg::widgets_hint_return("\u{fffc}"), HINT_KEY);
    hint.draw(p, c.x + c.w - hint.width(measure), cy, measure);
}

// ---------------------------------------------------------------------------------------
#[cfg(test)]
mod tests {
    use super::*;
    use plx_ui::prose_scroll::scroll_at;
    use plx_ui::consts::{SDLK_DOWN, SDLK_UP};

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
        use plx_ui::widgets::rail_geom;
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

    use crate::registry::{AppFx, AppMsg};
    use plx_machine::machine::{
        Canon, Chrome, Cx, Edge, Effects, EntryId, FocusRead, Fx, Handled, Host, InputEvent,
        InputKind, InputOwner, Key, LogicalState, Machine, NavOp, PressRead, ScreenId,
        Source, Stamped, Tick,
    };
    use plx_machine::present::Present;
    use plx_ui::screen::{ScreenArg, ScreenEvent};

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
        type Views<'a> = plx_data::person::PersonView<'a>;
        type Init = TestInit;
        type Memory = TestInit;
    }
    impl crate::registry::PersonLike for TestHost {
        fn person<'a>(cx: &Cx<'a, Self>) -> plx_data::person::PersonView<'a> { cx.views }
    }

    const ENTRY: EntryId = EntryId(7);

    fn cx(measure: &plx_ui::fixture::FixtureMeasure) -> Cx<'_, TestHost> {
        Cx {
            views: plx_data::person::PersonView::default(),
            tick: Tick::default(),
            measure,
            press: PressRead::default(),
            focus: FocusRead::default(),
            owner: InputOwner::Entry(ENTRY),
        }
    }

    /// What one input does to a fresh panel: the effects it emitted, and whether it was consumed.
    fn press(kind: InputKind<u32>) -> (Vec<Stamped<TestHost>>, Handled) {
        let measure = plx_ui::fixture::FixtureMeasure;
        let cx = cx(&measure);
        let (mut out, mut present) = (Vec::new(), Present::new());
        let mut fx = Effects::new(&mut out, plx_machine::machine::MachineId::Nav, &mut present);
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
        assert_eq!(panel.scroll.page, 1);
        assert!(panel.step_page(SDLK_DOWN), "DOWN moved");
        assert_eq!(panel.scroll.page, 2);
        assert!(panel.step_page(SDLK_UP));
        assert_eq!(panel.scroll.page, 1);
        assert!(!panel.step_page(SDLK_UP), "…and page 1 is the floor, reported as no movement");
        assert_eq!(panel.scroll.page, 1);
    }

    /// A click is SWALLOWED and moves nothing — the page under the sheet may not be reached by a
    /// pointer any more than by a key.
    #[test]
    fn a_click_does_not_reach_the_page_under_the_sheet() {
        let (out, handled) = press(InputKind::Click { x: 10.0, y: 10.0, hit: None });
        assert_eq!(handled, Handled::Yes);
        assert!(out.is_empty(), "a click on a read-only sheet does nothing at all");
    }

    /// **The `FocusSource::Engine`/`HitSource::Engine` conversion (restructure phase 12, D2)
    /// changes nothing observable — this is the proof.** Before the conversion, `ui/dispatch.rs`
    /// never asked the engine about this screen at all (`engine_page()`/`hit_page()` read
    /// `Legacy` and short-circuited). After it, the dispatcher calls exactly the entry points
    /// exercised here — `FocusEngine::enter` on mount (`Enter::Fresh`, `restored: None`, the
    /// container's actual call shape in `ui/dispatch.rs`'s `after_step`) and
    /// `FocusEngine::reconcile` before every draw — and both must still do nothing, because the
    /// `Focusable` impl above declares zero groups. The page cursor (`Self::page`) is untouched by
    /// either: it is moved only from `step`/`tick`, never by the focus engine. Graded directly
    /// against `FocusEngine`, the same type the dispatcher holds, rather than against the
    /// dispatcher itself (a sibling dependency the layer gate forbids this module from taking).
    #[test]
    fn engine_paths_are_inert_on_a_panel_with_no_focusable_element() {
        use plx_ui::focus::{FocusEngine, Outcome};
        use plx_machine::machine::GroupId;
        use plx_ui::screen::FocusTarget;

        let measure = plx_ui::fixture::FixtureMeasure;
        let cx = cx(&measure);
        let panel = PersonBioScreen::new(ENTRY);
        let owner = InputOwner::Entry(ENTRY);
        let mut engine: FocusEngine<u32> = FocusEngine::new();

        // The dispatcher's mount-time call: a fresh Enter, target the container group, nothing
        // restored (`ScreenEvent::Enter(Enter::Fresh { .. })` never carries a restore).
        let outcome = engine.enter(owner, &panel, FocusTarget::ContainerGroup(GroupId(0)), None, &cx);
        assert!(
            matches!(outcome, Outcome::Nothing),
            "no groups means nothing to enter, exactly as under Legacy nothing was ever asked"
        );

        // With nothing entered, the dispatcher's own pre-draw reconcile step also has nothing to
        // do — `FocusEngine::current` answers `None` for this owner.
        let outcome = engine.reconcile(owner, &panel, &cx);
        assert!(matches!(outcome, Outcome::Nothing));

        // The page cursor itself is unaffected — it only ever moves from `step_page`/`tick`.
        assert_eq!(panel.scroll.page, 1);

        use plx_ui::screen::Screen;
        assert_eq!(
            (
                Screen::<TestHost>::focus_source(&panel),
                Screen::<TestHost>::hit_source(&panel),
            ),
            (plx_ui::screen::FocusSource::Engine, plx_ui::screen::HitSource::Engine),
            "the conversion this test guards"
        );
    }

}
