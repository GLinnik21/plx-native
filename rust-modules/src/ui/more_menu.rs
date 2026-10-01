//! The player transport's **overflow menu** — the popover behind the third control disc (`…`), on
//! the same animated [`TableView`] as the subtitle/audio and profile menus. It only REPORTS the
//! chosen [`Action`]; `app.rs` performs it, exactly as the profile menu did before it became
//! an owned surface ([`crate::screens::account_menu`], restructure phase 10).
//!
//! # Why an overflow menu exists at all
//!
//! **Stats for nerds**, the diagnostics overlay ([`crate::app::diagnostics`]), needs a home a stranger can
//! find, because it is how this app gets bug reports off televisions nobody here owns — every other
//! diagnostic surface in the codebase (the `/tmp/plxnative-*` triggers, the remote FIFO, the
//! capture stream) is compiled out of RELEASE builds by the `devtriggers` feature, which is what a
//! user installs. "Press `…`, turn Stats for nerds on, photograph the screen" is a sentence that
//! fits in a GitHub reply and needs no ssh, no root and no rebuild.
//!
//! It held that one row for a while, and a menu with one row is not a mistake either: the
//! alternative — hanging the toggle off a hidden key chord — is undiscoverable by exactly the
//! people who would report the bug, and the alternative to THAT is a fourth disc for a control most
//! users touch once. Overflow is what a `…` means.
//!
//! # Two sections, and the two row idioms they are each drawn in
//!
//! **Quality leads.** It is the primary playback control this popover exists to reach — a rung
//! picked here re-routes the picture that is on screen right now (see below). **Options** trails
//! it: the diagnostics switch is an overflow affordance, something a viewer reaches for once, to
//! photograph a bug, not a control anyone returns to.
//!
//! **Quality** is the [`crate::route::Quality`] ladder — Original, fixed rungs, and Auto once its
//! playback readiness gate opens — and its rows carry
//! [`Row::checked`]'s LEADING checkmark, which means "the active one of several". That is the same
//! design-system rule from the other side: **a mark says where you are and a word says what is set,
//! and no row says both**, which is why a rung's rate rides inside its own label rather than in a
//! trailing value beside the mark. (The Options row drew as a PAIR OF MARKS for one day, a ring
//! ticked when on; those assets were deleted the same evening — see [`crate::ui::icons`].)
//!
//! **Options** holds switches. Its row carries [`Row::toggle`], so it states itself as the WORD
//! `On`/`Off` at the row's trailing edge. It is a STATE, not a destination: a chevron would promise
//! a page behind the row and there is none.
//!
//! A flat popover with a header per section, deliberately, rather than a Quality row that drills
//! into a second page: `docs/parity-gaps.md`'s standing decision is that this app has **no
//! full-screen menu sheets** — the reference clients put playback quality in one and we do not.
//! Drilling in INSIDE a popover is a separate matter and the Subtitles tab of
//! [`crate::ui::track_menu`] now does it (its Style pages, where BACK means "up one page" and only
//! BACK on the root dismisses); this menu stays flat until Quality moves onto the same page stack
//! (`docs/player-submenus.md`, PR 5). Six rungs and a switch fit; when they stop fitting, the
//! [`TableView`] scrolls, which is what it is for.
//!
//! The menu closes on commit either way, so the read-out is never what confirms the press: the
//! overlay appearing behind the dismissed panel — or, for a rung, the next play routing differently
//! — is.
//!
//! # What a picked rung does, and what it deliberately does not
//!
//! It is a ROUTING policy, not a number handed to the transcoder: over-ceiling content loses direct
//! play *and* the container remux, which is the only way a cap can bind at all. Original preserves
//! the source unchanged. Auto is exposed only through `route::auto_quality_ready()`, the named
//! fail-closed gate owned by the integrated HLS prime/swap path. The whole argument is
//! [`crate::route::Quality`]'s doc.
//!
//! It binds every future play, **and it re-decides the one on screen** — because this menu is the
//! ladder's only entry point, so a rung that waited for the next play would be a control that
//! visibly does nothing everywhere it can be reached. `route::set_quality` re-asks the routing
//! question with the new rung and reloads only when the answer changed; picking a HIGHER rung than
//! the picture already satisfies does nothing at all. That is a user-initiated switch and not an
//! adaptive one — nothing measures a link or moves a rung on its own.
#![allow(non_upper_case_globals)]
use crate::ui::consts::*;
use crate::ui::frame::Budget;
use crate::ui::geom::IndexElem;
use crate::ui::machine::{Cx, EntryId, FocusKey, GroupId, Host};
use crate::ui::screen::{
    Activate, At, AxisMask, Dir, DrawFrame, EdgeRule, ElemKind, Focusable, GroupKind, GroupSpec,
    Hover, Part, Placed, Seat, Step, Stop,
};
use crate::ui::form::{Activation, Form, FormId, FormSection, FormTable, RowKey, RowKind};
use crate::ui::panel_motion::PanelMotion;
use crate::ui::table::Row;
use crate::ui::{theme, Painter, Rect};
use std::convert::Infallible;

/// What the highlighted row does on OK.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Action {
    None,
    /// flip [`crate::app::diagnostics`]'s overlay on/off
    ToggleStats,
    /// select a rung of the playback-quality ladder ([`crate::route::set_quality`])
    SetQuality(crate::route::Quality),
    /// **Lab builds only** — snapshot and upload the diagnostic ring (`crate::lab`). Here as well
    /// as in `account_menu` because the account menu is unreachable during playback, and playback
    /// is what a Cloud Test Lab session is usually reproducing.
    SendDiagnostics,
}

/// The menu's whole state, owned by the container that mounts this panel — the modal PHASE and the
/// appear spring belong to `ui::containers::modal::ModalStack` now, not to this struct; `draw`
/// takes the appear fraction as a parameter instead of stepping its own `Popover`.
/// The panel's width is the shared menu rule (`TableView::menu_panel_width`): it hugs the widest
/// row, capped at `MENU_MAX_W`, and every row must fit the cap in every language
/// (`every_row_fits_the_panel_in_every_language`).

pub(crate) struct MoreMenuState {
    /// The rows AND the action each one commits, declared together ([`more_form`]) — the row SET
    /// varies (the Quality ladder is built from `route::available_quality_ladder`, and Force
    /// Direct Play drops it), so a row is found by its identity ([`Action`]'s [`FormId`] key),
    /// never by a position. The table is main-thread-only, like every other panel's.
    form: FormTable<Action, Action, Infallible>,
    /// The row set the form was last built from (what [`rows_for`] answered), so a live change to
    /// it — Auto's readiness gate opening, Force Direct Play flipping — is noticed and the panel
    /// rebuilt and RESIZED ([`Self::refresh`]) rather than left showing a ladder that has moved.
    rows: Vec<Action>,
    forced: bool,
    /// The card's resize spring ([`crate::ui::panel_motion`]), the same one the track menu uses:
    /// a row set that changes height animates the top edge, bottom and right stay on the anchor.
    motion: PanelMotion,
}

/// The hand-assigned focus key of each row. `SetQuality` rungs are an exhaustive match, so a new
/// rung cannot compile without claiming a key; none of these is a discriminant, a position or a
/// hash, so reordering the menu moves no key.
impl FormId for Action {
    fn key(&self) -> RowKey {
        use crate::route::Quality;
        RowKey(match self {
            Action::None => 0,
            Action::ToggleStats => 1,
            Action::SendDiagnostics => 2,
            Action::SetQuality(Quality::Auto) => 10,
            Action::SetQuality(Quality::Original) => 11,
            Action::SetQuality(Quality::P1080High) => 12,
            Action::SetQuality(Quality::P1080) => 13,
            Action::SetQuality(Quality::P720) => 14,
            Action::SetQuality(Quality::P720Low) => 15,
            Action::SetQuality(Quality::P480) => 16,
        })
    }
}

/// The menu as a pure form: Quality (one checked-rung row per `rows` entry that is a rung), then
/// Options. An empty Quality section is not drawn at all — a heading over nothing would read as a
/// menu that failed to load — and Force Direct Play (`forced`) never offers one.
fn more_form(ps: &crate::route::PlaybackSession, rows: &[Action], forced: bool) -> Form<Action, Action, Infallible> {
    let mut quality = FormSection::new(crate::i18n::msg::widgets_menu_quality());
    let mut options = FormSection::new(crate::i18n::msg::widgets_menu_options());
    for a in rows.iter().copied() {
        match a {
            Action::SetQuality(_) => quality = quality.item(a, RowKind::Button, a, row_for(ps, a)),
            _ => options = options.item(a, RowKind::Button, a, row_for(ps, a)),
        }
    }
    Form::new().section(quality.visible(!forced)).section(options)
}

impl MoreMenuState {
    fn open_focused(ps: &crate::route::PlaybackSession, quality: Option<crate::route::Quality>) -> Self {
        let forced = crate::route::forced_direct_play(ps);
        let rows = rows_for(forced);
        let mut form = FormTable::new(crate::ui::table_screen::BAND_BASE);
        form.table.compact = true; // a short action list — BODY labels, like the profile menu
        form.table.min_panel_w = theme::layout::PLAYER_MENU_MIN_W;
        // A quality entry focuses the active rung; under Force Direct Play there is no ladder to
        // focus, so the rung has vanished and the menu opens on its first row like an ordinary open.
        let keep = quality.map(Action::SetQuality);
        form.set_or_open(more_form(ps, &rows, forced), keep.as_ref());
        MoreMenuState { form, rows, forced, motion: PanelMotion::new() }
    }

    /// **Follow a live change of the row set**: rebuild the form when [`rows_for`] no longer
    /// answers what it was built from, keeping the focused row by identity. The panel's height
    /// follows, and the card animates to it ([`Self::update`]). Returns whether it rebuilt.
    pub(crate) fn refresh(&mut self, ps: &crate::route::PlaybackSession) -> bool {
        let forced = crate::route::forced_direct_play(ps);
        let rows = rows_for(forced);
        if forced == self.forced && rows == self.rows {
            return false;
        }
        let keep = self.form.selected_id().copied();
        self.form.set_or_open(more_form(ps, &rows, forced), keep.as_ref());
        self.rows = rows;
        self.forced = forced;
        true
    }

    pub(crate) fn new(ps: &crate::route::PlaybackSession) -> Self {
        Self::open_focused(ps, None)
    }

    /// The existing overflow menu, focused directly on the ACTIVE quality rung.
    ///
    /// The terminal playback screen has no transport discs, so OK must enter the ladder on the rung
    /// that is actually playing — a viewer arriving here is fixing a bad decision, not browsing the
    /// list. An ordinary `…` open already lands on the ladder's head (Quality leads Options now, so
    /// row 0 is the first rung), but "head" and "active" agree only when the active rung happens to
    /// be first; this entry point cannot assume that. It is still the SAME TableView and action map
    /// as the ordinary `…` menu; only the initial cursor differs.
    ///
    /// Under Force Direct Play the menu has no Quality section (see [`rows_for`]), and this is the
    /// same menu as [`Self::new`]; `screens::player::overlay` does not route here then.
    pub(crate) fn new_quality(ps: &crate::route::PlaybackSession) -> Self {
        Self::open_focused(ps, Some(crate::route::quality()))
    }

    /// Every row's focus key, in drawn order (for a test that walks the rows).
    #[cfg(test)]
    pub(crate) fn keys(&self) -> Vec<u32> {
        (0..self.form.table.n_rows() as usize).filter_map(|i| self.form.key_at(i).map(|k| k.0)).collect()
    }

    /// The highlighted row, for the focus probe (`crate::focusprobe`) — a READ of the cursor the
    /// key ladder moves, and the reason it exists: `app.rs`'s UP/DOWN arm for this panel changes
    /// nothing else, so without this the fingerprint records the panel opening and closing and
    /// nothing between.
    pub(crate) fn sel(&self) -> i32 {
        self.form.table.sel
    }

    /// **Write back the engine's own focus cursor** (restructure phase 12): the Column group
    /// [`MoreMenuPart`] answers is the source of geometry, but the ENGINE owns the current element
    /// (§7.3 step 5) — the owner's `step` is the only place that mutates in response to a
    /// `FocusMoved`, and this is `screens::player::overlay::PlayerOverlayScreen::step`'s write.
    /// Both a D-pad move AND a pointer hover reach here now — hover parks focus THROUGH the engine
    /// (§7.5), replacing this menu's own `pointer_focus`.
    pub(crate) fn focus_key(&mut self, elem: u32) {
        if let Some(i) = self.form.index_of_key(RowKey(elem)) {
            self.form.table.sel = i as i32;
        }
    }

    /// Commit the highlighted row — dismissing the panel afterward is the container's job now, not
    /// this method's.
    pub(crate) fn on_ok(&self) -> Action {
        self.form
            .selected_id()
            .and_then(|id| self.form.index_of(id))
            .and_then(|i| self.form.activate(i))
            .map_or(Action::None, |a| match a {
                Activation::Action(act) => act,
                Activation::Push(never) => match never {},
            })
    }

    /// Bottom-right, above the control row — anchored to the `…` disc that opened it, the way the
    /// track menu is anchored to the pair beside it. Shares the track menu's right margin
    /// (`player_hud::CTRL_RIGHT`, the discs' own edge) and its bottom edge, so opening one after the
    /// other does not make the panel hop. This is the NATURAL (layout) rect, cached against the
    /// table's `layout_rev`; [`Self::shown_rect`] is what is on screen while the card resizes.
    fn panel_rect(&self, measure: &dyn crate::ui::machine::Measure) -> Rect {
        self.motion.natural(self.form.table.layout_rev(), || {
            let pw = self.form.table.menu_panel_width(measure);
            let px = crate::ui::player_hud::CTRL_RIGHT - pw;
            let bottom = SCR_H - 316.0; // ~28px above the discs, as track_menu
            let ph = self.panel_h();
            Rect::new(px, bottom - ph, pw, ph)
        })
    }

    /// The panel's height alone. The ceiling was 320 while this menu held one row, and it was
    /// invisible then. With the Quality ladder beside it `measured_height()` can reach 600 when
    /// Auto is enabled — two headers, seven rows, a divider, AND the table's own top/bottom
    /// padding — so a 320 cap put four of nine rows on screen and silently scrolled the rest,
    /// which is a picker whose options you cannot see.
    ///
    /// The cap is a FRACTION of the room the panel has rather than a subtraction from it: the panel
    /// is anchored at `bottom` and grows upward, so `bottom` IS the space, and 0.86 of it leaves a
    /// clear margin at the top of the frame while comfortably clearing 600. Reaching for a
    /// `bottom - <margin>` literal is what put the first version of this line 4px UNDER the content
    /// — the margin was derived from the 560 of content and forgot the 40 of padding, so the last
    /// rung was clipped until you scrolled: the same symptom, one row deep instead of five. Past
    /// the cap it scrolls, which is what `TableView` is for.
    fn panel_h(&self) -> f32 {
        let bottom = SCR_H - 316.0;
        self.form.table.measured_height().clamp(120.0, bottom * 0.86)
    }

    /// The card as drawn this frame: top and left on their springs toward [`Self::panel_rect`].
    fn shown_rect(&self, measure: &dyn crate::ui::machine::Measure) -> Rect {
        self.motion.shown(self.panel_rect(measure))
    }

    /// Is the card still resizing? The player overlay holds the pointer while it is
    /// (`Screen::pointer_held`).
    pub(crate) fn transitioning(&self) -> bool {
        self.motion.transitioning()
    }

    pub(crate) fn update(&mut self, dt: f32, measure: &dyn crate::ui::machine::Measure, ps: &crate::route::PlaybackSession) {
        self.refresh(ps);
        // `update` subtracts its own top/bottom padding now — pass the panel's raw height.
        let natural = self.panel_rect(measure);
        self.form.table.update(dt, natural.h);
        self.motion.step(dt, natural);
        self.motion.prewarm_text(natural, &self.form.table, measure);
    }

    pub(crate) fn draw(&mut self, appear: f32, measure: &dyn crate::ui::machine::Measure) {
        // rises INTO place from below, toward the disc that opened it. The dim under it is the
        // container's (`PlayerOverlayScreen::scrim`, `theme::underlay::DIM_SHEET`), painted at the
        // end of the player's page pass — not here.
        let p = crate::ui::Painter::root()
            .alpha(appear)
            .translate(0.0, 16.0 * (1.0 - appear));
        self.motion.draw(p, self.panel_rect(measure), 24.0, &self.form.table, measure);
    }
}

/// **The Engine-shaped view of this popover** (restructure phase 12): one `Column` focus group
/// over the flat Options+Quality row list, built fresh by
/// `screens::player::overlay::PlayerOverlayScreen` each frame from a `&MoreMenuState` — the same
/// borrowed-view shape `ui::table_screen::TablePart`/`ui::geom::Table` use for the other panels
/// that are already a bare `TableView` in a frame, so this popover answers the same
/// [`Focusable`]/[`Part`] query protocol they do. Every edge is `Stop`, exactly as
/// `screens::account_menu::AccountMenuScreen`'s one `Column` group answers — this menu is a
/// self-contained modal surface with nowhere else for focus to escape to.
///
/// **`state` is a SHARED reference** — every [`Focusable`] method here is a pure read (`&self`),
/// and the owning screen's own `Focusable` impl only ever has `&self` too (§7.1: "the engine never
/// mutates a screen"), so a mutable field would make this type unconstructable from there. The
/// actual paint (`MoreMenuState::draw`) stays a direct call on the owned `Panel` from
/// `PlayerOverlayScreen::draw`'s `&mut self`; [`Part::draw`] below only registers stops.
pub(crate) struct MoreMenuPart<'a> {
    pub(crate) state: &'a MoreMenuState,
    pub(crate) entry: EntryId,
    pub(crate) group: GroupId,
}

impl<H: Host> Focusable<H> for MoreMenuPart<'_>
where
    H::Elem: IndexElem,
{
    fn groups(&self, _cx: &Cx<'_, H>, out: &mut Vec<GroupSpec>) {
        out.push(GroupSpec {
            id: self.group,
            kind: GroupKind::Column,
            seat: Seat::Remembered,
            reachable: AxisMask::VERTICAL,
            edge: [EdgeRule::Stop; 4],
            extent: self.state.panel_rect(_cx.measure),
            len: self.state.form.focusable_len(),
            elem: ElemKind::Bare,
        });
    }
    fn group_of(&self, key: &H::Elem, _cx: &Cx<'_, H>) -> Option<GroupId> {
        self.state.form.index_of_key(RowKey(key.index()?)).map(|_| self.group)
    }
    fn neighbour(&self, key: FocusKey<H::Elem>, dir: Dir, _cx: &Cx<'_, H>) -> Step<H::Elem> {
        let Some(from) = key.elem.index() else {
            return Step::Edge;
        };
        let delta = match dir {
            Dir::Up => -1,
            Dir::Down => 1,
            _ => return Step::Edge,
        };
        match self.state.form.step_key(RowKey(from), delta) {
            Some(k) => Step::Move(FocusKey { entry: self.entry, elem: H::Elem::of_index(k.0) }),
            None => Step::Edge,
        }
    }
    fn place(&self, key: &H::Elem, cx: &Cx<'_, H>, _at: At) -> Option<Placed> {
        let i = self.state.form.index_of_key(RowKey(key.index()?))? as u32;
        let r = self.state.form.table.row_frame(self.state.panel_rect(cx.measure), i as i32)?;
        Some(Placed {
            rect: r,
            rest_rect: r,
            clip: self.state.shown_rect(cx.measure),
            index: Some(i),
        })
    }
    fn reconcile(&self, want: FocusKey<H::Elem>, _cx: &Cx<'_, H>) -> FocusKey<H::Elem> {
        let kept = want
            .elem
            .index()
            .filter(|k| self.state.form.index_of_key(RowKey(*k)).is_some());
        FocusKey {
            entry: self.entry,
            elem: H::Elem::of_index(kept.or_else(|| self.state.form.opening_key().map(|k| k.0)).unwrap_or(0)),
        }
    }
    fn seat(&self, _g: GroupId, _from: Placed, _cx: &Cx<'_, H>) -> FocusKey<H::Elem> {
        let key = self.state.form.selected_key().or_else(|| self.state.form.opening_key());
        FocusKey {
            entry: self.entry,
            elem: H::Elem::of_index(key.map_or(0, |k| k.0)),
        }
    }
}

impl<H: Host> Part<H> for MoreMenuPart<'_>
where
    H::Elem: IndexElem,
{
    fn prepare(&mut self, _b: &mut Budget, _cx: &Cx<'_, H>) {}
    /// Registers every selectable row's stop (rule 11: hover parks, a click activates — the same
    /// loop `TablePart::draw` runs over an ordinary page table); the popover's own paint happens
    /// directly on the owned `MoreMenuState` from `PlayerOverlayScreen::draw` (struct doc above).
    fn draw(&mut self, f: &mut DrawFrame<'_, '_, H>, _rect: Rect) {
        let p = Painter::root();
        let r = self.state.panel_rect(f.measure);
        let clip = self.state.shown_rect(f.measure);
        for i in 0..self.state.form.table.n_rows() {
            let Some(key) = self.state.form.key_at(i as usize) else {
                continue;
            };
            if let Some(row) = self.state.form.table.row_frame(r, i) {
                f.stop(
                    p,
                    Stop {
                        key: FocusKey {
                            entry: self.entry,
                            elem: H::Elem::of_index(key.0),
                        },
                        rect: row,
                        rest_rect: row,
                        clip,
                        hover: Hover::Focus,
                        activate: Activate::Direct,
                    },
                );
            }
        }
    }
}

/// Every row the menu can offer, in order and ACROSS SECTIONS. A free function (rather than a
/// literal inside [`MoreMenuState::open_focused`]) so the index mapping [`MoreMenuState::on_ok`]
/// relies on is one testable value.
///
/// **The order here is the whole contract**, because [`TableView`]'s `sel` is a single flat index
/// over every row of every section: this list must be built in exactly the order
/// [`MoreMenuState::open_focused`] pushes rows, or a press commits its neighbour. A separator would
/// be a row here too — there is none, and the debug assert in [`MoreMenuState::open_focused`] is
/// what would catch one being added on one side only.
///
/// **`forced` (Force Direct Play) drops the Quality section entirely.** Every rung is a request for
/// the server to convert, and Force forbids conversion, so under it no rung can change what plays:
/// a row that cannot change the outcome is not offered (the same rule as the failure read-out's
/// `player::failure_actions`). Pure over the flag so both shapes are testable without a session.
fn rows_for(forced: bool) -> Vec<Action> {
    let mut v: Vec<Action> = if forced {
        Vec::new()
    } else {
        crate::route::available_quality_ladder()
            .iter()
            .map(|q| Action::SetQuality(*q))
            .collect()
    };
    v.push(Action::ToggleStats);
    if crate::lab::menu_row_enabled() {
        v.push(Action::SendDiagnostics);
    }
    v
}

fn label(a: Action) -> std::borrow::Cow<'static, str> {
    match a {
        Action::ToggleStats => crate::i18n::msg::widgets_menu_stats().into(),
        // the rung names itself — rate and frame in one string, because the row already carries
        // the picker's leading mark (see this module's doc)
        Action::SetQuality(q) => q.label().into(),
        Action::SendDiagnostics => crate::i18n::msg::widgets_menu_diagnostics().into(),
        Action::None => "".into(),
    }
}

/// Whether the SWITCH a row names is currently on. It reaches the row as [`Row::toggle`] and so
/// draws as the WORD `On`/`Off` at the trailing edge — never as a picker's leading checkmark, which
/// means "the active one of several" and is what the Quality rung rows use instead. Two idioms, one
/// rule: a mark says where you are and a word says what is set, and no row says both. (Named
/// `checked` until 2026-08-21, from the row builder it does not call — a name that read as a
/// promise of the leading mark an Options row deliberately does not draw.)
fn is_on(a: Action) -> bool {
    match a {
        Action::ToggleStats => crate::app::diagnostics::enabled(),
        // a rung is not a switch — see `row_for`, which gives it the leading mark instead
        Action::SetQuality(_) | Action::SendDiagnostics | Action::None => false,
    }
}

/// **"Original" is a claim about the SOURCE, and for some sources it is false.**
///
/// Every other rung names a bound the viewer can reason about — "1080p · 20 Mbps". This one names a
/// provenance, and when the television cannot decode the source video at all (AV1, VP9, MPEG-2 —
/// `route::source_decodable`) the server must re-encode the pixels whatever is picked. The row
/// still does something: it is the only rung that sends no bitrate or resolution cap. But it cannot
/// deliver the original, and until this it said so nowhere, while the DETAIL page for the same item
/// already said "Converts on server" from the same predicate.
///
/// **The same words as the detail page, deliberately.** One vocabulary for one fact; a second
/// phrasing here would read as a second fact.
///
/// **A sub-line rather than a trailing value**, because `Row`'s rule is that a leading mark and a
/// trailing word may not both appear and every rung row carries the picker's mark. And no `dim`:
/// dimming is the ink of "unavailable", the row is fully selectable and still useful, and this is
/// the one rung that can ask for more than 1080p — an annotation that reads as "do not pick this"
/// would steer people off the only >1080p ask the app has.
/// **Pure, and it takes the fact rather than reading it.** The predicate lives on the session and
/// the row is drawn from it (`row_for`); passing it in is what lets the copy be tested without a
/// resolved playback, and what keeps this function a statement about the LADDER rather than about
/// global state.
fn quality_detail(q: crate::route::Quality, source_decodable: bool) -> &'static str {
    if q == crate::route::Quality::Original && !source_decodable {
        crate::ui::fmt::converts_on_server()
    } else {
        ""
    }
}

/// One row, drawn in the idiom its ACTION calls for. Free-standing (rather than inline in
/// [`MoreMenuState::open_focused`]) so the two idioms are decided in one place: a switch gets the
/// trailing word, a picker rung gets the leading mark, and nothing gets both.
fn row_for(ps: &crate::route::PlaybackSession, a: Action) -> Row {
    match a {
        Action::SetQuality(q) => Row::new(label(a))
            .checked(crate::route::quality() == q)
            .detail(quality_detail(q, crate::route::source_decodable(ps))),
        _ => Row::new(label(a)).toggle(is_on(a)),
    }
}

/// The panel at its TALLEST, for the overscan audit ([`crate::ui::consts::SAFE`]).
#[cfg(test)]
pub(crate) fn overscan_rects(out: &mut Vec<(&'static str, Rect)>) {
    let (pw, ph) = (crate::ui::table::MENU_MAX_W, 320.0f32);
    let bottom = SCR_H - 316.0;
    out.push((
        "… overflow menu panel",
        Rect::new(crate::ui::player_hud::CTRL_RIGHT - pw, bottom - ph, pw, ph),
    ));
}

/// The index→action mapping, which is the only part of a popover that is testable off the main
/// thread: a real `MoreMenuState` owns `TableView`/its row list, and both are main-thread-only,
/// like every other panel's state.
#[cfg(test)]
mod tests {
    use super::*;

    /// **The Original row says so when it cannot be Original.**
    ///
    /// For a source this television cannot decode — AV1, VP9, MPEG-2 — the server must re-encode
    /// the pixels whatever rung is picked, so the word "Original" is a promise the pipeline cannot
    /// keep. The DETAIL page for the same item already said "Converts on server" from the same
    /// predicate; the quality picker, which is where a viewer goes to do something about it, said
    /// nothing at all.
    ///
    /// Differential: unmodified code draws no sub-line on any quality row, in any state.
    ///
    /// The negative half is the important one. A fixed rung ALSO converts, and Auto on such an
    /// item runs an encoded ladder too — but neither of them is named after the source, so neither
    /// is making the claim this line corrects. Annotating them would turn one honest correction
    /// into four lines of noise.
    #[test]
    fn only_the_original_row_says_the_source_cannot_be_preserved() {
        use crate::route::Quality;
        assert_eq!(
            quality_detail(Quality::Original, false),
            "Converts on server"
        );
        assert_eq!(
            quality_detail(Quality::Original, true),
            "",
            "a source the panel decodes needs no correction — Original means Original",
        );
        for q in crate::route::QUALITY_LADDER {
            if q == Quality::Original {
                continue;
            }
            assert_eq!(
                quality_detail(q, false),
                "",
                "{q:?} is not named after the source, so it makes no claim to correct",
            );
        }
    }

    /// The copy is the DETAIL page's, verbatim. One vocabulary for one fact: a second phrasing
    /// would read as a second fact, and the two surfaces answer from the same predicate.
    #[test]
    fn the_conversion_notice_is_the_words_the_detail_page_already_uses() {
        assert_eq!(
            quality_detail(crate::route::Quality::Original, false),
            crate::ui::fmt::converts_on_server(),
        );
    }

    #[test]
    fn every_row_has_a_label() {
        for a in rows_for(false) {
            assert!(!label(a).is_empty(), "{a:?} would draw a blank row");
        }
        // The full persisted ladder must keep finished UI copy even if the readiness gate is
        // deliberately closed again for a future protocol regression.
        for q in crate::route::QUALITY_LADDER {
            assert!(
                !label(Action::SetQuality(q)).is_empty(),
                "{q:?} would draw a blank row when enabled"
            );
        }
    }

    /// The actions in table order, read back off a built menu by identity.
    fn order(st: &MoreMenuState) -> Vec<Action> {
        (0..st.form.table.n_rows() as usize).filter_map(|i| st.form.id_at(i).copied()).collect()
    }

    /// A menu built from a row list, without a `PlaybackSession` beyond the default one.
    fn menu(forced: bool, focus: Option<crate::route::Quality>) -> MoreMenuState {
        let ps = crate::route::PlaybackSession::default();
        let mut form = FormTable::new(crate::ui::table_screen::BAND_BASE);
        form.table.compact = true;
        let keep = focus.map(Action::SetQuality);
        form.set_or_open(more_form(&ps, &rows_for(forced), forced), keep.as_ref());
        MoreMenuState { form, rows: rows_for(forced), forced, motion: PanelMotion::new() }
    }

    /// Pressing each row commits ITS action, addressed by identity — never by a position.
    #[test]
    fn a_focused_row_commits_its_own_action() {
        let mut st = menu(false, None);
        for a in rows_for(false) {
            st.focus_key(a.key().0);
            assert_eq!(st.on_ok(), a);
        }
    }

    /// Quality leads Options — see this module's doc — so the ladder's head is the first row and
    /// the toggle follows the last rung. The two sections are declared as sections, so the join
    /// that a flat index could quietly break is one the form owns.
    #[test]
    fn the_quality_ladder_leads_and_the_toggle_follows_it() {
        let st = menu(false, None);
        let ladder: Vec<Action> =
            crate::route::available_quality_ladder().iter().map(|q| Action::SetQuality(*q)).collect();
        assert_eq!(&order(&st)[..ladder.len()], &ladder[..]);
        assert_eq!(order(&st)[ladder.len()], Action::ToggleStats);
    }

    #[test]
    fn failure_entry_can_focus_the_active_quality_in_the_shared_menu() {
        for q in crate::route::available_quality_ladder() {
            let st = menu(false, Some(*q));
            assert_eq!(st.on_ok(), Action::SetQuality(*q));
        }
        let st = menu(false, None);
        assert_eq!(
            st.on_ok(),
            order(&st)[0],
            "ordinary … starts at the top row — which is now the ladder's head, since Quality \
             leads Options"
        );
    }

    /// Reordering the rows moves no key: every row keeps the key (and so the action) it had, and
    /// the press still commits the row's own action.
    #[test]
    fn a_reordered_menu_resolves_every_key_to_the_same_action() {
        let ps = crate::route::PlaybackSession::default();
        let mut rows = rows_for(false);
        rows.reverse();
        let mut form = FormTable::new(crate::ui::table_screen::BAND_BASE);
        form.set(more_form(&ps, &rows, false), None);
        let st = MoreMenuState { form, rows: rows.clone(), forced: false, motion: PanelMotion::new() };
        for a in rows_for(false) {
            let i = st.form.index_of_key(a.key()).expect("every row keeps its key");
            assert_eq!(st.form.id_at(i), Some(&a));
            match st.form.activate(i) {
                Some(Activation::Action(got)) => assert_eq!(got, a),
                _ => panic!("{a:?} must commit its own action"),
            }
        }
    }

    /// **Quality must stay entirely ahead of Options in the flat row order.** The two `TableView`
    /// sections are built by one pass over [`rows_for`]'s list (see
    /// [`MoreMenuState::open_focused`]), so this is the one test that actually guards "Quality
    /// first, Options after" as a property of the list rather than of a couple of hand-picked
    /// indices — get a future row added on the wrong side of the split and nothing here fails
    /// loudly; `on_ok` just returns the wrong `Action` for the row a viewer pressed.
    #[test]
    fn every_quality_rung_sits_ahead_of_the_stats_toggle() {
        let rows = rows_for(false);
        let stats_i = rows
            .iter()
            .position(|a| *a == Action::ToggleStats)
            .expect("the toggle is always in the menu");
        for (i, a) in rows.iter().enumerate() {
            if matches!(a, Action::SetQuality(_)) {
                assert!(
                    i < stats_i,
                    "{a:?} at row {i} must come before Stats for nerds at row {stats_i}"
                );
            }
        }
    }

    /// **Force Direct Play leaves no Quality section**: no rung can change what plays under it,
    /// so none is offered, and a quality entry (`new_quality`) lands on the first Options row.
    #[test]
    fn forced_direct_play_offers_no_quality_rung_and_lands_on_the_first_option() {
        let forced = rows_for(true);
        assert!(
            !forced.iter().any(|a| matches!(a, Action::SetQuality(_))),
            "forced direct play must not offer a quality rung: {forced:?}"
        );
        assert_eq!(forced.first(), Some(&Action::ToggleStats));
        for q in crate::route::QUALITY_LADDER {
            let st = menu(true, Some(q));
            assert_eq!(st.on_ok(), Action::ToggleStats, "a vanished rung opens on the first option");
            assert_eq!(st.sel(), 0);
        }
        // not forced: the ladder is unchanged, in order, ahead of Options
        let open = rows_for(false);
        let ladder: Vec<Action> = open
            .iter()
            .copied()
            .filter(|a| matches!(a, Action::SetQuality(_)))
            .collect();
        let expected: Vec<Action> = crate::route::available_quality_ladder()
            .iter()
            .map(|q| Action::SetQuality(*q))
            .collect();
        assert_eq!(ladder, expected);
        assert_eq!(&open[ladder.len()..], &forced[..]);
    }

    /// A key that names no row commits nothing, never a neighbour's action.
    #[test]
    fn an_unknown_key_is_none_not_a_neighbour() {
        let mut st = menu(false, None);
        let before = st.sel();
        st.focus_key(0xdead);
        assert_eq!(st.sel(), before, "an unknown key moves nothing");
    }

    /// Every row's key is distinct and below the band.
    #[test]
    fn every_key_is_distinct_and_below_the_band() {
        let mut all: Vec<Action> = crate::route::QUALITY_LADDER.iter().map(|q| Action::SetQuality(*q)).collect();
        all.extend([Action::ToggleStats, Action::SendDiagnostics]);
        let mut keys: Vec<u32> = all.iter().map(|a| a.key().0).collect();
        keys.sort_unstable();
        keys.dedup();
        assert_eq!(keys.len(), all.len());
        assert!(keys.iter().all(|k| *k < crate::ui::table_screen::BAND_BASE));
    }

    /// **A row set that changes height animates the card** (`ui::panel_motion`): the panel opens
    /// at rest, a live change to the set (Force Direct Play dropping the Quality section, Auto's
    /// gate opening) rebuilds it and the top edge springs to the new layout while the bottom and
    /// right stay anchored, then it asks for no more frames.
    #[test]
    fn a_changed_row_set_resizes_the_card_with_a_spring_and_then_rests() {
        let _serial = crate::testlock::serial();
        const DT: f32 = 1.0 / 60.0;
        let ps = crate::route::PlaybackSession::default();
        let m = crate::fontcov::advances::ShippedMeasure;
        let step = |st: &mut MoreMenuState| {
            crate::ui::idle::frame_begin(DT);
            st.update(DT, &m, &ps);
            crate::ui::idle::present_moving()
        };
        // opened under Force Direct Play: no Quality section, a short panel
        let mut st = menu(true, None);
        let short = st.panel_rect(&m);
        st.motion.step(DT, short); // the open: the first step places the card AT its layout
        assert!(!st.transitioning(), "a freshly opened panel is at rest");
        assert_eq!(st.shown_rect(&m), short);

        // Force Direct Play is switched off under the open panel: the ladder appears, the panel grows
        assert!(st.refresh(&ps), "the row set moved");
        assert!(!st.refresh(&ps), "and only once");
        let tall = st.panel_rect(&m);
        assert!(tall.h > short.h, "the Quality ladder makes the panel taller: {} > {}", tall.h, short.h);
        assert_eq!((tall.x + tall.w, tall.y + tall.h), (short.x + short.w, short.y + short.h), "bottom and right are the anchor");
        assert!(st.transitioning());
        assert!(step(&mut st), "the resize is motion");
        let mid = st.shown_rect(&m);
        assert!(mid.h > short.h && mid.h < tall.h, "mid-resize the card is between the heights: {mid:?}");
        let mut n = 0;
        while step(&mut st) {
            n += 1;
            assert!(n < 240, "the resize never settles");
        }
        assert_eq!(st.shown_rect(&m), tall, "lands exactly on the new layout");
        assert!(!st.transitioning());
        assert!(!step(&mut st), "at rest: no frames requested");
    }
}

#[cfg(test)]
mod focus_tests {
    use super::*;
    use crate::screens::registry::{AppFx, AppMsg, PageMemory};
    use crate::ui::machine::{FocusRead, InputOwner, PressRead, Tick};

    struct HostFixture;
    impl Host for HostFixture {
        type Arg = crate::ui::fixture::FixtureArg;
        type Fx = AppFx;
        type Msg = AppMsg;
        type Elem = u32;
        type Views<'a> = ();
        type Init = crate::ui::fixture::FixtureInit;
        type Memory = PageMemory;
    }

    fn with_cx<R>(entry: EntryId, test: impl FnOnce(&Cx<'_, HostFixture>) -> R) -> R {
        let measure = crate::ui::fixture::FixtureMeasure;
        test(&Cx {
            views: (),
            tick: Tick::default(),
            measure: &measure,
            focus: FocusRead::default(),
            press: PressRead::default(),
            owner: InputOwner::Entry(entry),
        })
    }

    /// A three-row menu (two synthetic quality rungs, then `Stats for nerds`) — mirrors the real
    /// menu's order (Quality leads, Options trails; this module's doc) rather than contradicting
    /// it, built the same way [`MoreMenuState::open_focused`] does but without a `PlaybackSession`
    /// — no row here reads one.
    fn three_row_menu() -> MoreMenuState {
        let mut sec = FormSection::new("Quality");
        for (a, label) in [
            (Action::SetQuality(crate::route::Quality::Original), "Rung A"),
            (Action::SetQuality(crate::route::Quality::Auto), "Rung B"),
            (Action::ToggleStats, "Stats for nerds"),
        ] {
            sec = sec.item(a, RowKind::Button, a, Row::new(label));
        }
        let mut form = FormTable::new(crate::ui::table_screen::BAND_BASE);
        form.table.compact = true;
        form.set(Form::new().section(sec), None);
        MoreMenuState { form, rows: Vec::new(), forced: false, motion: PanelMotion::new() }
    }

    /// The focus element of the row at `index` — the keys are identities, not positions.
    fn elem(st: &MoreMenuState, index: usize) -> u32 {
        st.form.key_at(index).expect("a bound row").0
    }

    /// **UP/DOWN step by one row and clamp at both ends**, over `TableView::next_selectable`,
    /// exercised here through the real `Focusable` dispatch over `HostFixture`.
    #[test]
    fn up_down_step_by_one_and_clamp_at_both_ends() {
        let e = EntryId(4);
        let st = three_row_menu();
        let part = MoreMenuPart { state: &st, entry: e, group: GroupId(0) };
        with_cx(e, |cx| {
            let step = |i: u32, dir: Dir| {
                match <MoreMenuPart as Focusable<HostFixture>>::neighbour(
                    &part,
                    FocusKey { entry: e, elem: i },
                    dir,
                    cx,
                ) {
                    Step::Move(k) => Some(k.elem),
                    Step::Edge => None,
                }
            };
            let (r0, r1, r2) = (elem(&st, 0), elem(&st, 1), elem(&st, 2));
            assert_eq!(step(r0, Dir::Down), Some(r1));
            assert_eq!(step(r1, Dir::Down), Some(r2));
            assert_eq!(step(r2, Dir::Down), None, "the last row does not wrap");
            assert_eq!(step(r0, Dir::Up), None, "the first row does not wrap");
            assert_eq!(step(r1, Dir::Up), Some(r0));
            // LEFT/RIGHT are swallowed — this popover is ONE column, matching the old ladder's
            // `Key::Up | Key::Down => …` arm with no Left/Right case at all.
            assert!(matches!(step(elem(&st, 1), Dir::Left), None));
        });
    }

    /// `place` reports exactly the row rect `TableView::row_frame` (and so the old `draw`) would
    /// paint at.
    #[test]
    fn place_matches_the_tables_own_row_frame() {
        let e = EntryId(4);
        let st = three_row_menu();
        let r = st.panel_rect(&crate::ui::fixture::FixtureMeasure);
        let want = st.form.table.row_frame(r, 1);
        let part = MoreMenuPart { state: &st, entry: e, group: GroupId(0) };
        with_cx(e, |cx| {
            let placed = <MoreMenuPart as Focusable<HostFixture>>::place(&part, &elem(&st, 1), cx, At::Drawn);
            assert_eq!(placed.map(|p| (p.rect.x, p.rect.y, p.rect.w, p.rect.h)), want.map(|r| (r.x, r.y, r.w, r.h)));
        });
    }

    /// A cursor whose key names no row (a shorter previous row set) settles onto the menu's
    /// OPENING row — a key has no neighbour to slide to — reached through `Focusable::reconcile`.
    #[test]
    fn a_stale_cursor_settles_onto_the_opening_row() {
        let e = EntryId(4);
        let st = three_row_menu();
        let part = MoreMenuPart { state: &st, entry: e, group: GroupId(0) };
        with_cx(e, |cx| {
            let got = <MoreMenuPart as Focusable<HostFixture>>::reconcile(
                &part,
                FocusKey { entry: e, elem: 99 },
                cx,
            );
            assert_eq!(got.elem, elem(&st, 0));
            let kept = <MoreMenuPart as Focusable<HostFixture>>::reconcile(
                &part,
                FocusKey { entry: e, elem: elem(&st, 2) },
                cx,
            );
            assert_eq!(kept.elem, elem(&st, 2), "a live key is left where it is");
        });
    }

    /// **Every row fits the panel, in every shipped language.** The panel hugs its widest row up to
    /// [`MENU_MAX_W`](crate::ui::table::MENU_MAX_W), and a row elides its label to what the value beside it leaves — Spanish
    /// *Estadísticas avanzadas* and Belarusian *Падрабязная статыстыка* both ended in `…` beside
    /// their *Off*. Measured with the device's whole-pixel advances.
    #[test]
    fn every_row_fits_the_panel_in_every_language() {
        use crate::i18n::{language_on_this_thread_for_test, SHIPPED};
        let ps = crate::route::PlaybackSession::default();
        let mut out = Vec::new();
        for language in SHIPPED {
            let _guard = language_on_this_thread_for_test(language);
            let menu = MoreMenuState::new(&ps);
            out.extend(menu.form.table.menu_cap_failure(&crate::fontcov::advances::ShippedMeasure, language.tag()));
            out.extend(menu.form.table.app_fit_failures(crate::ui::table::MENU_MAX_W, language.tag()));
            out.extend(menu.form.table.app_fit_failures_hugged(language.tag()));
        }
        crate::ui::table::assert_no_fit_failures(&out);
    }
}
