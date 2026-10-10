//! The player transport's **overflow menu** — the popover behind the third control disc (`…`), on
//! the same animated [`TableView`] as the subtitle/audio and profile menus. It only REPORTS the
//! chosen [`Action`]; `app.rs` performs it, exactly as the profile menu did before it became
//! an owned surface (`crate::screens::account_menu`, restructure phase 10).
//!
//! # Why an overflow menu exists at all
//!
//! **Stats for nerds**, the diagnostics overlay (`crate::app::diagnostics`), needs a home a stranger can
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
//! # A root with a Quality row, a Quality page, and the two row idioms they are drawn in
//!
//! **Quality leads.** It is the primary playback control this popover exists to reach — a rung
//! picked here re-routes the picture that is on screen right now (see below). On the ROOT it is one
//! headerless drill-in row ([`MoreRow::OpenQuality`]) whose value is the current rung's label;
//! RIGHT or OK pushes the **Quality page** (a title band, then the rungs). **Options** trails it on
//! the root: the diagnostics switch is an overflow affordance, something a viewer reaches for once,
//! to photograph a bug, not a control anyone returns to.
//!
//! **Quality** (the page) is the [`plx_media::route::Quality`] ladder — Original, fixed rungs, and Auto once its
//! playback readiness gate opens — and its rows carry
//! [`Row::checked`]'s LEADING checkmark, which means "the active one of several". That is the same
//! design-system rule from the other side: **a mark says where you are and a word says what is set,
//! and no row says both**, which is why a rung's rate rides inside its own label rather than in a
//! trailing value beside the mark. (The Options row drew as a PAIR OF MARKS for one day, a ring
//! ticked when on; those assets were deleted the same evening — see [`plx_ui::icons`].)
//!
//! **Options** holds switches. Its row carries [`Row::toggle`], so it states itself as the WORD
//! `On`/`Off` at the row's trailing edge. It is a STATE, not a destination: a chevron would promise
//! a page behind the row and there is none.
//!
//! `docs/parity-gaps.md`'s standing decision is that this app has **no full-screen menu sheets** —
//! the reference clients put playback quality in one and we do not. Quality drills in INSIDE the
//! popover instead, on the same [`plx_ui::page_stack::PageStack`] the Subtitles tab of
//! [`crate::track_menu`] uses: BACK, LEFT and a click on the title band mean "up one page", and
//! only BACK on the root dismisses. Six rungs fit; when they stop fitting, the [`TableView`]
//! scrolls, which is what it is for.
//!
//! A rung closes the menu on commit (opening the page does not), so the read-out is never what confirms the press: the
//! overlay appearing behind the dismissed panel — or, for a rung, the next play routing differently
//! — is.
//!
//! # What a picked rung does, and what it deliberately does not
//!
//! It is a ROUTING policy, not a number handed to the transcoder: over-ceiling content loses direct
//! play *and* the container remux, which is the only way a cap can bind at all. Original preserves
//! the source unchanged. Auto is exposed only through `route::auto_quality_ready()`, the named
//! fail-closed gate owned by the integrated HLS prime/swap path. The whole argument is
//! [`plx_media::route::Quality`]'s doc.
//!
//! It binds every future play, **and it re-decides the one on screen** — because this menu is the
//! ladder's only entry point, so a rung that waited for the next play would be a control that
//! visibly does nothing everywhere it can be reached. `route::set_quality` re-asks the routing
//! question with the new rung and reloads only when the answer changed; picking a HIGHER rung than
//! the picture already satisfies does nothing at all. That is a user-initiated switch and not an
//! adaptive one — nothing measures a link or moves a rung on its own.
#![allow(non_upper_case_globals)]
use plx_ui::frame::Budget;
use plx_ui::geom::IndexElem;
use plx_machine::machine::{Cx, EntryId, FocusKey, GroupId, Host, Measure};
use plx_ui::screen::{At, Dir, DrawFrame, Focusable, GroupSpec, Part, Placed, Step};
use plx_ui::form::{Activation, Form, FormId, FormSection, FormTable, RowKey, RowKind};
use plx_ui::page_stack::{
    self, popover_group_of, popover_groups, popover_neighbour, popover_place,
    popover_register_stops, popover_seat, PageStack, PopoverPanel,
};
use plx_ui::panel_motion::PanelMotion;
use plx_ui::table::Row;
use plx_ui::{theme, Rect};

/// What the highlighted row does on OK.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Action {
    None,
    /// flip `crate::app::diagnostics`'s overlay on/off
    ToggleStats,
    /// select a rung of the playback-quality ladder ([`plx_media::route::set_quality`])
    SetQuality(plx_media::route::Quality),
    /// **Lab builds only** — snapshot and upload the diagnostic ring (`crate::lab`). Here as well
    /// as in `account_menu` because the account menu is unreachable during playback, and playback
    /// is what a Cloud Test Lab session is usually reproducing.
    SendDiagnostics,
}

/// **A drill-in page of the More menu** — the form's `Dest`. The root is the empty page stack, not
/// a value of this type (`docs/player-submenus.md`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MorePage {
    /// The playback-quality ladder: one checked-rung row per rung ([`MoreRow::Act`] of a
    /// [`Action::SetQuality`]).
    Quality,
}

impl MorePage {
    /// The title band's text.
    fn title(self) -> String {
        match self {
            Self::Quality => plx_platform::i18n::msg::widgets_menu_quality().to_string(),
        }
    }

    /// A stable small number for the replay canon. Never reordered: recordings hash it.
    fn code(self) -> u32 {
        match self {
            Self::Quality => 1,
        }
    }
}

/// A row's identity on either page: the Quality drill-in on the root, or a row that commits an
/// [`Action`] (a rung on the Quality page, an Options switch on the root).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MoreRow {
    /// The root's Quality row ([`MorePage::Quality`]): reads out the current rung.
    OpenQuality,
    Act(Action),
}

/// What OK on the highlighted row did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MoreOk {
    /// Perform this action; the container dismisses the panel (a rung pick and the Options rows
    /// close it, exactly as before the Quality page existed).
    Action(Action),
    /// A Nav row opened a page: the panel stays, with its rows replaced.
    Navigated,
}

type MoreForm = Form<MoreRow, Action, MorePage>;
type MoreTable = FormTable<MoreRow, Action, MorePage>;

/// The menu's whole state, owned by the container that mounts this panel — the modal PHASE and the
/// appear spring belong to `ui::containers::modal::ModalStack` now, not to this struct; `draw`
/// takes the appear fraction as a parameter instead of stepping its own `Popover`.
/// The panel's width is the shared menu rule (`TableView::menu_panel_width`): it hugs the widest
/// row, capped at `MENU_MAX_W`, and every row must fit the cap in every language
/// (`every_row_fits_the_panel_in_every_language`).

pub struct MoreMenuState {
    /// The rows AND the action each one commits, declared together ([`root_form`],
    /// [`quality_form`]) — the row SET varies (the Quality ladder is built from
    /// `route::available_quality_ladder`, and Force Direct Play drops it), so a row is found by its
    /// identity ([`MoreRow`]'s [`FormId`] key), never by a position. One table serves the page that
    /// is showing. The table is main-thread-only, like every other panel's.
    form: MoreTable,
    /// The row set the form was last built from (what [`rows_for`] answered), so a live change to
    /// it — Auto's readiness gate opening, Force Direct Play flipping — is noticed and the panel
    /// rebuilt and RESIZED ([`Self::refresh`]) rather than left showing a ladder that has moved.
    rows: Vec<Action>,
    forced: bool,
    /// The rung the root's Quality row reads out and the Quality page checks, as of the last build.
    current: plx_media::route::Quality,
    /// The pages pushed above the root, outermost first (empty = the root): each remembers the row
    /// that opened it and the scroll it was left at ([`page_stack`]).
    pages: PageStack<MorePage, MoreRow>,
    /// The card's resize and the page slide ([`plx_ui::panel_motion`]), the same one the track
    /// menu uses: a row set that changes height animates the top edge, bottom and right stay on the
    /// anchor, and a push or pop slides the two pages.
    motion: PanelMotion,
}

/// The hand-assigned focus key of each row. `SetQuality` rungs are an exhaustive match, so a new
/// rung cannot compile without claiming a key; none of these is a discriminant, a position or a
/// hash, so reordering the menu moves no key.
impl FormId for Action {
    fn key(&self) -> RowKey {
        use plx_media::route::Quality;
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

impl FormId for MoreRow {
    fn key(&self) -> RowKey {
        match self {
            // free in `Action`'s key space (0..=2 and 10..=16)
            MoreRow::OpenQuality => RowKey(3),
            MoreRow::Act(a) => a.key(),
        }
    }
}

/// Is the Quality row offered? Force Direct Play never offers one (no rung can change what plays),
/// and an empty ladder has nothing to open onto — a row leading to an empty page would read as a
/// menu that failed to load.
fn quality_offered(rows: &[Action], forced: bool) -> bool {
    !forced && rows.iter().any(|a| matches!(a, Action::SetQuality(_)))
}

/// The ROOT as a pure form: the Quality drill-in (headerless, like the Subtitles root's Style —
/// it reads out the current rung as its value), then Options.
fn root_form(
    ps: &plx_media::route::PlaybackSession,
    rows: &[Action],
    forced: bool,
    current: plx_media::route::Quality,
) -> MoreForm {
    let mut options = FormSection::new(plx_platform::i18n::msg::widgets_menu_options());
    for a in rows.iter().copied().filter(|a| !matches!(a, Action::SetQuality(_))) {
        options = options.item(MoreRow::Act(a), RowKind::Button, a, row_for(ps, a));
    }
    let quality = FormSection::new("").item_if(
        quality_offered(rows, forced),
        MoreRow::OpenQuality,
        RowKind::Nav(MorePage::Quality),
        Action::None,
        Row::new(plx_platform::i18n::msg::widgets_menu_quality()).value(current.label()),
    );
    Form::new().section(quality).section(options)
}

/// The Quality PAGE: one checked-rung row per rung of `rows`, the current one checked.
fn quality_form(
    ps: &plx_media::route::PlaybackSession,
    rows: &[Action],
    current: plx_media::route::Quality,
) -> MoreForm {
    let sec = rows.iter().copied().filter(|a| matches!(a, Action::SetQuality(_))).fold(
        FormSection::new(""),
        |sec, a| {
            sec.choice(MoreRow::Act(a), a, row_for(ps, a), |id| {
                *id == MoreRow::Act(Action::SetQuality(current))
            })
        },
    );
    Form::new().section(sec)
}

impl MoreMenuState {
    fn open_focused(ps: &plx_media::route::PlaybackSession, quality: Option<plx_media::route::Quality>) -> Self {
        let forced = plx_media::route::forced_direct_play(ps);
        let rows = rows_for(forced);
        let current = plx_media::route::quality();
        let mut form = FormTable::new(plx_ui::table_screen::BAND_BASE);
        form.table.compact = true; // a short action list — BODY labels, like the profile menu
        form.table.min_panel_w = theme::layout::PLAYER_MENU_MIN_W;
        form.set_or_open(root_form(ps, &rows, forced, current), None);
        let mut st = MoreMenuState { form, rows, forced, current, pages: PageStack::new(), motion: PanelMotion::new() };
        // A quality entry (the failure screen's OK) opens straight on the Quality page with the
        // active rung focused, no slide: the viewer is fixing a bad decision, not browsing. Under
        // Force Direct Play there is no ladder, so it is the ordinary root.
        if quality.is_some() && quality_offered(&st.rows, st.forced) {
            st.push(ps, MorePage::Quality);
            st.motion.cancel_slide();
        }
        st
    }

    /// The page showing, `None` at the root.
    #[cfg(any(test, feature = "test-support"))]
    pub fn page(&self) -> Option<MorePage> {
        self.pages.top()
    }

    /// The highlighted row's identity, for tests.
    #[cfg(any(test, feature = "test-support"))]
    pub fn sel_id(&self) -> Option<MoreRow> {
        self.form.selected_id().copied()
    }

    /// The form of a pushed page, from the panel's own read-outs.
    fn page_form(&self, ps: &plx_media::route::PlaybackSession, page: MorePage) -> MoreForm {
        match page {
            MorePage::Quality => quality_form(ps, &self.rows, self.current),
        }
    }

    /// The explicit initial focus of a pushed page: the Quality page opens on its checked rung
    /// (else the first row, [`FormTable::open`]'s fallback).
    fn page_initial(&self, page: MorePage) -> Option<MoreRow> {
        match page {
            MorePage::Quality => Some(MoreRow::Act(Action::SetQuality(self.current))),
        }
    }

    /// Open `page` above the current one: remember the opener and its scroll, install the page's
    /// rows (the pill snaps, the scroll returns to the top, focus lands on [`Self::page_initial`])
    /// and its title band, and start the slide.
    fn push(&mut self, ps: &plx_media::route::PlaybackSession, page: MorePage) {
        let Some(return_id) = self.form.selected_id().copied() else { return };
        self.pages.push(page, return_id, self.form.table.scroll_pos());
        let leaving = page_stack::leave_page(&mut self.form);
        let form = self.page_form(ps, page);
        self.form.open(form, self.page_initial(page).as_ref());
        // after the sections: the band moves every row, so the pill is re-jumped onto the focused one
        self.form.table.set_title(Some(page.title()));
        self.motion.begin_slide(leaving, 1.0);
    }

    /// Pop the top page: the root comes back exactly as it was left — the opener focused by id, the
    /// scroll reinstated ([`FormTable::restore`]). `false` at the root (nothing popped).
    pub fn pop(&mut self, ps: &plx_media::route::PlaybackSession) -> bool {
        let Some(saved) = self.pages.pop() else { return false };
        let leaving = page_stack::leave_page(&mut self.form);
        self.motion.begin_slide(leaving, -1.0);
        let form = root_form(ps, &self.rows, self.forced, self.current);
        self.form.restore(form, Some(&saved.return_id), saved.scroll);
        self.form.table.set_title(None);
        true
    }

    /// **RIGHT**: on a Nav row it enters — the same as OK; anywhere else nothing happens (this
    /// popover has no tabs to switch).
    pub fn on_right(&mut self, ps: &plx_media::route::PlaybackSession) {
        let sel = self.form.table.sel.max(0) as usize;
        if matches!(self.form.binding_at(sel).map(|b| &b.kind), Some(RowKind::Nav(_))) {
            if let Some(Activation::Push(dest)) = self.form.activate(sel) {
                self.push(ps, dest);
            }
        }
    }

    /// **The replay canon**: the page path (each pushed page and the row that opened it) and the
    /// selected row's [`RowKey`] — not its index, which two pages share.
    pub fn canon(&self, c: &mut plx_machine::machine::Canon) {
        self.pages.canon(c, MorePage::code, |r| r.key().0);
        c.u32(self.form.key_at(self.form.table.sel.max(0) as usize).map_or(u32::MAX, |k| k.0));
    }

    /// What the `moreosc` trigger needs to choose its next key: how many pages are pushed.
    pub fn osc_depth(&self) -> usize {
        self.pages.len()
    }

    /// **Follow a live change of the row set or the current rung**: rebuild the page that is
    /// showing when [`rows_for`], Force Direct Play or the current rung no longer answer what it
    /// was built from. On the root the form is rebuilt keeping the focused row by identity; on the
    /// Quality page it is refreshed IN PLACE (focus kept by id, the checked rung following the
    /// pick), and the page pops to the root when Quality stops being offered. The panel's height
    /// follows, and the card animates to it ([`Self::update`]). Returns whether it rebuilt.
    pub fn refresh(&mut self, ps: &plx_media::route::PlaybackSession) -> bool {
        let forced = plx_media::route::forced_direct_play(ps);
        self.refresh_to(ps, forced, rows_for(forced), plx_media::route::quality())
    }

    /// [`Self::refresh`] against explicit answers (what a test or a probe supplies in place of the
    /// live route).
    fn refresh_to(
        &mut self,
        ps: &plx_media::route::PlaybackSession,
        forced: bool,
        rows: Vec<Action>,
        current: plx_media::route::Quality,
    ) -> bool {
        if forced == self.forced && rows == self.rows && current == self.current {
            return false;
        }
        self.rows = rows;
        self.forced = forced;
        self.current = current;
        if let Some(first) = self.pages.first().copied() {
            if quality_offered(&self.rows, self.forced) {
                let page = self.pages.top().unwrap_or(MorePage::Quality);
                let fallback = self.page_initial(page);
                let form = self.page_form(ps, page);
                self.form.refresh_with(form, None, fallback.as_ref());
            } else {
                // the ladder is gone under the viewer: back to the root, restoring the opener and
                // scroll the stack saved at its first push
                self.pages.clear();
                let form = root_form(ps, &self.rows, self.forced, self.current);
                self.form.restore(form, Some(&first.return_id), first.scroll);
                self.form.table.set_title(None);
                self.motion.cancel_slide();
            }
        } else {
            let keep = self.form.selected_id().copied();
            self.form.set_or_open(root_form(ps, &self.rows, self.forced, self.current), keep.as_ref());
        }
        true
    }

    pub fn new(ps: &plx_media::route::PlaybackSession) -> Self {
        Self::open_focused(ps, None)
    }

    /// The existing overflow menu, opened on the Quality page with the ACTIVE rung focused.
    ///
    /// The terminal playback screen has no transport discs, so OK must enter the ladder on the rung
    /// that is actually playing — a viewer arriving here is fixing a bad decision, not browsing the
    /// list. It is still the SAME menu, form and action map as the ordinary `…` menu; only the page
    /// it opens on differs (BACK pops to the root, as from any pushed page).
    ///
    /// Under Force Direct Play the menu has no Quality section (see [`rows_for`]), and this is the
    /// same menu as [`Self::new`]; `screens::player::overlay` does not route here then.
    pub fn new_quality(ps: &plx_media::route::PlaybackSession) -> Self {
        Self::open_focused(ps, Some(plx_media::route::quality()))
    }

    /// Every row's focus key, in drawn order (for a test that walks the rows).
    #[cfg(any(test, feature = "test-support"))]
    pub fn keys(&self) -> Vec<u32> {
        (0..self.form.table.n_rows() as usize).filter_map(|i| self.form.key_at(i).map(|k| k.0)).collect()
    }

    /// The highlighted row, for the focus probe (`crate::focusprobe`) — a READ of the cursor the
    /// key ladder moves, and the reason it exists: `app.rs`'s UP/DOWN arm for this panel changes
    /// nothing else, so without this the fingerprint records the panel opening and closing and
    /// nothing between.
    pub fn sel(&self) -> i32 {
        self.form.table.sel
    }

    /// **Write back the engine's own focus cursor** (restructure phase 12): the Column group
    /// [`MoreMenuPart`] answers is the source of geometry, but the ENGINE owns the current element
    /// (§7.3 step 5) — the owner's `step` is the only place that mutates in response to a
    /// `FocusMoved`, and this is `screens::player::overlay::PlayerOverlayScreen::step`'s write.
    /// Both a D-pad move AND a pointer hover reach here now — hover parks focus THROUGH the engine
    /// (§7.5), replacing this menu's own `pointer_focus`.
    /// Returns whether a row with that key is on the page showing.
    pub fn focus_key(&mut self, elem: u32) -> bool {
        let Some(i) = self.form.index_of_key(RowKey(elem)) else { return false };
        self.form.table.sel = i as i32;
        true
    }

    /// OK on the highlighted row: an action to perform (the container dismisses the panel
    /// afterward), or a Nav row that opened its page.
    pub fn on_ok(&mut self, ps: &plx_media::route::PlaybackSession) -> MoreOk {
        match self.form.selected_id().and_then(|id| self.form.index_of(id)).and_then(|i| self.form.activate(i)) {
            Some(Activation::Action(act)) => MoreOk::Action(act),
            Some(Activation::Push(page)) => {
                self.push(ps, page);
                MoreOk::Navigated
            }
            None => MoreOk::Action(Action::None),
        }
    }

    /// Bottom-right, above the control row — anchored to the `…` disc that opened it, the way the
    /// track menu is anchored to the pair beside it. Shares the track menu's right margin
    /// (`player_hud::CTRL_RIGHT`, the discs' own edge) and its bottom edge, so opening one after the
    /// other does not make the panel hop. This is the NATURAL (layout) rect, cached against the
    /// table's `layout_rev`; [`Self::shown_rect`] is what is on screen while the card resizes.
    fn panel_rect(&self, measure: &dyn plx_machine::machine::Measure) -> Rect {
        self.motion.natural(self.form.table.layout_rev(), || {
            let pw = self.form.table.menu_panel_width(measure);
            let px = crate::player_hud::CTRL_RIGHT - pw;
            let bottom = theme::layout::PLAYER_MENU_BOTTOM; // ~28px above the discs, as track_menu
            let ph = self.panel_h();
            Rect::new(px, bottom - ph, pw, ph)
        })
    }

    /// The panel's height alone. The ceiling was 320 while this menu held one row, and it was
    /// invisible then. With the Quality ladder beside it `measured_height()` can reach 600 when
    /// Auto is enabled — the tallest page is the Quality page: its title band plus seven
    /// rungs, AND the table's own top/bottom padding — so a 320 cap put four of nine rows on screen and silently scrolled the rest,
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
        let bottom = theme::layout::PLAYER_MENU_BOTTOM;
        self.form.table.measured_height().clamp(120.0, bottom * 0.86)
    }

    /// The card as drawn this frame: top and left on their springs toward [`Self::panel_rect`].
    fn shown_rect(&self, measure: &dyn plx_machine::machine::Measure) -> Rect {
        self.motion.shown(self.panel_rect(measure))
    }

    /// Is the card still resizing? The player overlay holds the pointer while it is
    /// (`Screen::pointer_held`).
    pub fn transitioning(&self) -> bool {
        self.motion.transitioning()
    }

    /// The opening page's strings (the root, or the Quality page for a quality entry), queued on
    /// the frame the panel mounts — see `TrackMenuState::warm_open`, which this mirrors.
    pub fn warm_open(&self, measure: &dyn plx_machine::machine::Measure) {
        self.motion.warm_open(&self.form.table, || self.panel_rect(measure), measure);
    }

    pub fn update(&mut self, dt: f32, measure: &dyn plx_machine::machine::Measure, ps: &plx_media::route::PlaybackSession) {
        self.refresh(ps);
        // `update` subtracts its own top/bottom padding now — pass the panel's raw height.
        let natural = self.panel_rect(measure);
        self.form.table.update(dt, natural.h);
        self.motion.step(dt, natural);
        self.motion.prewarm_text(natural, &self.form.table, measure);
    }

    pub fn draw(&mut self, appear: f32, measure: &dyn plx_machine::machine::Measure) {
        // rises INTO place from below, toward the disc that opened it. The dim under it is the
        // container's (`PlayerOverlayScreen::scrim`, `theme::underlay::DIM_SHEET`), painted at the
        // end of the player's page pass — not here.
        let p = plx_ui::Painter::root()
            .alpha(appear)
            .translate(0.0, 16.0 * (1.0 - appear));
        self.motion.draw(p, self.panel_rect(measure), 24.0, &self.form.table, measure);
    }
}

/// **The Engine-shaped view of this popover** (restructure phase 12): one `Column` focus group
/// over the rows of the page showing (the root's Quality drill-in plus Options, or the Quality
/// page's rungs), built fresh by
/// `screens::player::overlay::PlayerOverlayScreen` each frame from a `&MoreMenuState` — the same
/// borrowed-view shape `ui::table_screen::TablePart`/`ui::geom::Table` use for the other panels
/// that are already a bare `TableView` in a frame, so this popover answers the same
/// [`Focusable`]/[`Part`] query protocol they do. UP and DOWN `Stop` at the ends; LEFT and RIGHT
/// are `EdgeRule::Screen`, re-delivered to `PlayerOverlayScreen::edge_key`, which pops a pushed
/// page on LEFT and enters a Nav row (Quality) on RIGHT, and otherwise swallows the key so focus
/// never leaves the modal surface.
///
/// **`state` is a SHARED reference** — every [`Focusable`] method here is a pure read (`&self`),
/// and the owning screen's own `Focusable` impl only ever has `&self` too (§7.1: "the engine never
/// mutates a screen"), so a mutable field would make this type unconstructable from there. The
/// actual paint (`MoreMenuState::draw`) stays a direct call on the owned `Panel` from
/// `PlayerOverlayScreen::draw`'s `&mut self`; [`Part::draw`] below only registers stops.
pub struct MoreMenuPart<'a> {
    pub state: &'a MoreMenuState,
    pub entry: EntryId,
    pub group: GroupId,
}

impl PopoverPanel for MoreMenuState {
    type Id = MoreRow;
    type Act = Action;
    type Dest = MorePage;
    fn form(&self) -> &FormTable<Self::Id, Self::Act, Self::Dest> {
        &self.form
    }
    fn motion(&self) -> &PanelMotion {
        &self.motion
    }
    fn panel_rect(&self, measure: &dyn Measure) -> Rect {
        MoreMenuState::panel_rect(self, measure)
    }
    fn shown_rect(&self, measure: &dyn Measure) -> Rect {
        MoreMenuState::shown_rect(self, measure)
    }
}

impl<H: Host> Focusable<H> for MoreMenuPart<'_>
where
    H::Elem: IndexElem,
{
    fn groups(&self, cx: &Cx<'_, H>, out: &mut Vec<GroupSpec>) {
        popover_groups(self.state, self.group, cx.measure, out);
    }
    fn group_of(&self, key: &H::Elem, _cx: &Cx<'_, H>) -> Option<GroupId> {
        popover_group_of(self.state, key, self.group)
    }
    fn neighbour(&self, key: FocusKey<H::Elem>, dir: Dir, _cx: &Cx<'_, H>) -> Step<H::Elem> {
        popover_neighbour(self.state, self.entry, key, dir)
    }
    fn place(&self, key: &H::Elem, cx: &Cx<'_, H>, _at: At) -> Option<Placed> {
        popover_place(self.state, key, cx.measure)
    }
    fn reconcile(&self, want: FocusKey<H::Elem>, _cx: &Cx<'_, H>) -> FocusKey<H::Elem> {
        // A live key stays where it is. One that names no row of the page now showing — the opener
        // after a push, the rung after a pop, a row of a shorter set — settles on the panel's OWN
        // cursor (where the push/pop/refresh already decided focus belongs), else the opening row.
        let kept = want
            .elem
            .index()
            .filter(|k| self.state.form.index_of_key(RowKey(*k)).is_some());
        let key = kept
            .or_else(|| self.state.form.selected_key().map(|k| k.0))
            .or_else(|| self.state.form.opening_key().map(|k| k.0));
        FocusKey { entry: self.entry, elem: H::Elem::of_index(key.unwrap_or(0)) }
    }
    fn seat(&self, _g: GroupId, _from: Placed, _cx: &Cx<'_, H>) -> FocusKey<H::Elem> {
        popover_seat(self.state, self.entry)
    }
}

impl<H: Host> Part<H> for MoreMenuPart<'_>
where
    H::Elem: IndexElem,
{
    fn prepare(&mut self, _b: &mut Budget, _cx: &Cx<'_, H>) {}
    /// Registers every selectable row's stop and the title band's ([`popover_register_stops`]); the
    /// popover's own paint happens directly on the owned state from `PlayerOverlayScreen::draw`
    /// (struct doc above).
    fn draw(&mut self, f: &mut DrawFrame<'_, '_, H>, _rect: Rect) {
        popover_register_stops(self.state, self.entry, f);
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
        plx_media::route::available_quality_ladder()
            .iter()
            .map(|q| Action::SetQuality(*q))
            .collect()
    };
    v.push(Action::ToggleStats);
    if plx_platform::labcfg::menu_row_enabled() {
        v.push(Action::SendDiagnostics);
    }
    v
}

fn label(a: Action) -> std::borrow::Cow<'static, str> {
    match a {
        Action::ToggleStats => plx_platform::i18n::msg::widgets_menu_stats().into(),
        // the rung names itself — rate and frame in one string, because the row already carries
        // the picker's leading mark (see this module's doc)
        Action::SetQuality(q) => q.label().into(),
        Action::SendDiagnostics => plx_platform::i18n::msg::widgets_menu_diagnostics().into(),
        Action::None => "".into(),
    }
}

/// Where the Stats for nerds switch reads its state. The read-out's flag is owned by
/// `app::diagnostics`, whose caller names this menu's [`Action`]s, so the application registers a
/// reader at boot ([`install_stats_reader`], from `app::enter_application`) instead of this
/// module naming `app`. Unset it reads OFF, which is what the flag itself reads before anything has
/// toggled it.
static STATS_READER: std::sync::OnceLock<fn() -> bool> = std::sync::OnceLock::new();

/// Register the reader for the Stats for nerds switch. The first registration wins; the loop makes
/// exactly one, before any screen exists.
pub fn install_stats_reader(read: fn() -> bool) {
    let _ = STATS_READER.set(read);
}

fn stats_on() -> bool {
    STATS_READER.get().is_some_and(|read| read())
}

/// Whether the SWITCH a row names is currently on. It reaches the row as [`Row::toggle`] and so
/// draws as the WORD `On`/`Off` at the trailing edge — never as a picker's leading checkmark, which
/// means "the active one of several" and is what the Quality rung rows use instead. Two idioms, one
/// rule: a mark says where you are and a word says what is set, and no row says both. (Named
/// `checked` until 2026-08-21, from the row builder it does not call — a name that read as a
/// promise of the leading mark an Options row deliberately does not draw.)
fn is_on(a: Action) -> bool {
    match a {
        Action::ToggleStats => stats_on(),
        // a rung is not a switch — see `row_for`, which gives it the leading mark instead
        Action::SetQuality(_) | Action::SendDiagnostics | Action::None => false,
    }
}

/// **"Original" is a claim about the SOURCE, and for some sources it is false.**
///
/// Every other rung names a bound the viewer can reason about — "1080p · 20 Mbps". This one names a
/// provenance, and when the television cannot decode the source video at all (MPEG-2, and AV1 / VP9
/// on a set whose codec table lists no such decoder — `route::source_decodable`) the server must
/// re-encode the pixels whatever is picked. The row
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
fn quality_detail(q: plx_media::route::Quality, source_decodable: bool) -> &'static str {
    if q == plx_media::route::Quality::Original && !source_decodable {
        plx_ui::fmt::converts_on_server()
    } else {
        ""
    }
}

/// One row, drawn in the idiom its ACTION calls for. Free-standing (rather than inline in the form
/// builders) so the two idioms are decided in one place: a switch gets the trailing word, a picker
/// rung gets the leading mark ([`FormSection::choice`] derives it from the current rung), and nothing gets
/// both.
fn row_for(ps: &plx_media::route::PlaybackSession, a: Action) -> Row {
    match a {
        Action::SetQuality(q) => Row::new(label(a)).detail(quality_detail(q, plx_media::route::source_decodable(ps))),
        _ => Row::new(label(a)).toggle(is_on(a)),
    }
}

/// The panel at its TALLEST, for the overscan audit ([`plx_ui::consts::SAFE`]).
#[cfg(any(test, feature = "test-support"))]
pub fn overscan_rects(out: &mut Vec<(&'static str, Rect)>) {
    let (pw, ph) = (plx_ui::table::MENU_MAX_W, 320.0f32);
    let bottom = theme::layout::PLAYER_MENU_BOTTOM;
    out.push((
        "… overflow menu panel",
        Rect::new(crate::player_hud::CTRL_RIGHT - pw, bottom - ph, pw, ph),
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
    /// For a source this television cannot decode — MPEG-2, and AV1 / VP9 on a set whose codec table
    /// lists no such decoder — the server must re-encode
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
        use plx_media::route::Quality;
        assert_eq!(
            quality_detail(Quality::Original, false),
            "Converts on server"
        );
        assert_eq!(
            quality_detail(Quality::Original, true),
            "",
            "a source the panel decodes needs no correction — Original means Original",
        );
        for q in plx_media::route::QUALITY_LADDER {
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
            quality_detail(plx_media::route::Quality::Original, false),
            plx_ui::fmt::converts_on_server(),
        );
    }

    #[test]
    fn every_row_has_a_label() {
        for a in rows_for(false) {
            assert!(!label(a).is_empty(), "{a:?} would draw a blank row");
        }
        // The full persisted ladder must keep finished UI copy even if the readiness gate is
        // deliberately closed again for a future protocol regression.
        for q in plx_media::route::QUALITY_LADDER {
            assert!(
                !label(Action::SetQuality(q)).is_empty(),
                "{q:?} would draw a blank row when enabled"
            );
        }
    }

    /// The actions of the rows showing, in table order, read back off a built menu by identity (the
    /// root's Quality drill-in commits nothing and is not one).
    fn order(st: &MoreMenuState) -> Vec<Action> {
        (0..st.form.table.n_rows() as usize)
            .filter_map(|i| match st.form.id_at(i) {
                Some(MoreRow::Act(a)) => Some(*a),
                _ => None,
            })
            .collect()
    }

    /// A menu ROOT built from the row list, without a `PlaybackSession` beyond the default one.
    fn menu(forced: bool, current: plx_media::route::Quality) -> MoreMenuState {
        let ps = plx_media::route::PlaybackSession::default();
        let rows = rows_for(forced);
        let mut form = FormTable::new(plx_ui::table_screen::BAND_BASE);
        form.table.compact = true;
        form.set_or_open(root_form(&ps, &rows, forced, current), None);
        MoreMenuState { form, rows, forced, current, pages: PageStack::new(), motion: PanelMotion::new() }
    }

    /// The same menu with the Quality page pushed (and its slide skipped).
    fn quality_page(current: plx_media::route::Quality) -> MoreMenuState {
        let ps = plx_media::route::PlaybackSession::default();
        let mut st = menu(false, current);
        st.focus_key(MoreRow::OpenQuality.key().0);
        assert_eq!(st.on_ok(&ps), MoreOk::Navigated);
        st.motion.cancel_slide();
        st
    }

    /// The text a row reads out as its value (a Nav row's current setting).
    fn value_of(st: &mut MoreMenuState, id: MoreRow) -> Option<String> {
        let i = st.form.index_of(&id)?;
        st.form.table.row_mut(i as i32).and_then(|r| r.value.clone())
    }

    /// Is the row `id` drawn with the picker's leading check?
    fn checked(st: &mut MoreMenuState, id: MoreRow) -> bool {
        let i = st.form.index_of(&id).expect("a row");
        st.form.table.row_mut(i as i32).is_some_and(|r| r.checked)
    }

    use plx_media::route::Quality;

    /// Pressing each row commits ITS action, addressed by identity — never by a position: an
    /// Options row on the root and a rung on the Quality page.
    #[test]
    fn a_focused_row_commits_its_own_action() {
        let ps = plx_media::route::PlaybackSession::default();
        let mut st = menu(false, Quality::P1080);
        for a in rows_for(false).into_iter().filter(|a| !matches!(a, Action::SetQuality(_))) {
            st.focus_key(a.key().0);
            assert_eq!(st.on_ok(&ps), MoreOk::Action(a));
        }
        let mut st = quality_page(Quality::P1080);
        for a in rows_for(false).into_iter().filter(|a| matches!(a, Action::SetQuality(_))) {
            st.focus_key(a.key().0);
            // exactly the action the flat ladder reported: `app::playback::apply_more_action` is
            // the one place a `SetQuality` is performed (`route::set_quality`)
            assert_eq!(st.on_ok(&ps), MoreOk::Action(a), "{a:?}");
        }
    }

    /// **The root is the Quality drill-in then Options; the rungs live on the page.** The root no
    /// longer lists a rung, the page lists the ladder in order and nothing else.
    #[test]
    fn the_root_leads_with_the_quality_row_and_the_page_holds_the_ladder() {
        let st = menu(false, Quality::Original);
        let ids: Vec<Option<MoreRow>> =
            (0..st.form.table.n_rows() as usize).map(|i| st.form.id_at(i).copied()).collect();
        assert_eq!(ids[0], Some(MoreRow::OpenQuality), "Quality leads");
        assert!(ids.contains(&Some(MoreRow::Act(Action::ToggleStats))));
        assert!(
            !ids.iter().any(|i| matches!(i, Some(MoreRow::Act(Action::SetQuality(_))))),
            "no rung on the root: {ids:?}"
        );
        let ladder: Vec<Action> =
            plx_media::route::available_quality_ladder().iter().map(|q| Action::SetQuality(*q)).collect();
        assert_eq!(order(&quality_page(Quality::Original)), ladder);
    }

    /// **The root's Quality row reads out the CURRENT rung** — "Auto", "Original", "1080p · 8 Mbps"
    /// — as its value, and the page checks that rung and no other.
    #[test]
    fn the_quality_row_reads_the_current_rung_and_the_page_checks_it() {
        for q in plx_media::route::QUALITY_LADDER {
            let mut st = menu(false, q);
            assert_eq!(value_of(&mut st, MoreRow::OpenQuality), Some(q.label()), "{q:?}");
            assert!(
                !checked(&mut st, MoreRow::OpenQuality),
                "a drill-in row carries no check — a mark says where you are, a word says what is set"
            );
            let mut st = quality_page(q);
            for rung in plx_media::route::QUALITY_LADDER {
                let id = MoreRow::Act(Action::SetQuality(rung));
                if st.form.index_of(&id).is_some() {
                    assert_eq!(checked(&mut st, id), rung == q, "{rung:?} while {q:?} is current");
                }
            }
        }
    }

    /// **Push focuses the checked rung; pop restores focus on the Quality row** (by identity), the
    /// title band comes and goes with the page, and BACK at the root pops nothing.
    #[test]
    fn push_lands_on_the_checked_rung_and_pop_restores_the_quality_row() {
        let ps = plx_media::route::PlaybackSession::default();
        for q in plx_media::route::available_quality_ladder() {
            let mut st = menu(false, *q);
            assert!(!st.pop(&ps), "the root has nothing to pop");
            st.focus_key(MoreRow::OpenQuality.key().0);
            assert_eq!(st.on_ok(&ps), MoreOk::Navigated);
            assert_eq!(st.page(), Some(MorePage::Quality));
            assert_eq!(st.form.selected_id(), Some(&MoreRow::Act(Action::SetQuality(*q))), "initial focus = the checked rung");
            assert_eq!(st.form.table.title(), Some("Quality"));
            assert!(st.transitioning(), "a push slides");
            assert!(st.pop(&ps));
            assert_eq!(st.page(), None);
            assert_eq!(st.form.selected_id(), Some(&MoreRow::OpenQuality), "pop restores focus on the Quality row");
            assert_eq!(st.form.table.title(), None);
        }
    }

    /// RIGHT on the Quality row enters it, on an Options row does nothing; the page's own RIGHT
    /// does nothing either (there is no deeper page and no tab to switch to).
    #[test]
    fn right_enters_the_quality_row_and_nothing_else() {
        let ps = plx_media::route::PlaybackSession::default();
        let mut st = menu(false, Quality::P720);
        st.focus_key(Action::ToggleStats.key().0);
        st.on_right(&ps);
        assert_eq!(st.page(), None, "RIGHT on an Options row stays put");
        st.focus_key(MoreRow::OpenQuality.key().0);
        st.on_right(&ps);
        assert_eq!(st.page(), Some(MorePage::Quality));
        st.on_right(&ps);
        assert_eq!(st.page(), Some(MorePage::Quality), "RIGHT on a rung does nothing");
    }

    /// **`warm_open` queues the OPENING page's strings with no `update`, once** — the root for the
    /// ordinary entry, the Quality page for a quality entry.
    #[test]
    fn warm_open_queues_the_opening_page_once_without_an_update() {
        use plx_ui::fixture::FixtureMeasure as M;
        let _serial = plx_base::testlock::serial();
        let ps = plx_media::route::PlaybackSession::default();
        for st in [MoreMenuState::new(&ps), MoreMenuState::new_quality(&ps)] {
            plx_gfx::text::reset_prewarm_for_test();
            st.warm_open(&M);
            assert!(plx_gfx::text::prewarm_pending(), "warm_open queued the opening page");
            plx_gfx::text::clear_prewarm();
            st.warm_open(&M);
            assert!(!plx_gfx::text::prewarm_pending(), "an unchanged layout was walked again");
        }
    }

    /// The failure screen's entry opens ON the Quality page, the active rung focused, no slide; a
    /// Force Direct Play entry (no ladder) and the ordinary entry open the root.
    #[test]
    fn the_quality_entry_opens_on_the_page_and_the_ordinary_entry_on_the_root() {
        let _serial = plx_base::testlock::serial();
        let ps = plx_media::route::PlaybackSession::default();
        let st = MoreMenuState::new(&ps);
        assert_eq!(st.page(), None);
        let st = MoreMenuState::new_quality(&ps);
        assert_eq!(st.page(), Some(MorePage::Quality));
        assert!(!st.transitioning(), "the entry is not a slide");
        assert_eq!(st.form.table.title(), Some("Quality"));
        let active = MoreRow::Act(Action::SetQuality(plx_media::route::quality()));
        if st.form.index_of(&active).is_some() {
            assert_eq!(st.form.selected_id(), Some(&active), "the active rung is focused");
        }
    }

    /// Reordering the rows moves no key: every row keeps the key (and so the action) it had, and
    /// the press still commits the row's own action.
    #[test]
    fn a_reordered_menu_resolves_every_key_to_the_same_action() {
        let ps = plx_media::route::PlaybackSession::default();
        let mut rows = rows_for(false);
        rows.reverse();
        let mut form = FormTable::new(plx_ui::table_screen::BAND_BASE);
        form.set(quality_form(&ps, &rows, Quality::Auto), None);
        let st = MoreMenuState {
            form,
            rows: rows.clone(),
            forced: false,
            current: Quality::Auto,
            pages: PageStack::new(),
            motion: PanelMotion::new(),
        };
        for a in rows_for(false).into_iter().filter(|a| matches!(a, Action::SetQuality(_))) {
            let i = st.form.index_of_key(a.key()).expect("every row keeps its key");
            assert_eq!(st.form.id_at(i), Some(&MoreRow::Act(a)));
            match st.form.activate(i) {
                Some(Activation::Action(got)) => assert_eq!(got, a),
                _ => panic!("{a:?} must commit its own action"),
            }
        }
    }

    /// **Quality must stay entirely ahead of Options in the row list** ([`rows_for`]): the root
    /// builds the Quality drill-in before the Options section, and the list is what both forms are
    /// built from, so a rung added on the wrong side of the split would reach the Options section.
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

    /// **Force Direct Play leaves no Quality row and no page**: no rung can change what plays under
    /// it, so none is offered and a quality entry lands on the first Options row.
    #[test]
    fn forced_direct_play_offers_no_quality_row_and_lands_on_the_first_option() {
        let ps = plx_media::route::PlaybackSession::default();
        let forced = rows_for(true);
        assert!(
            !forced.iter().any(|a| matches!(a, Action::SetQuality(_))),
            "forced direct play must not offer a quality rung: {forced:?}"
        );
        assert_eq!(forced.first(), Some(&Action::ToggleStats));
        assert!(!quality_offered(&forced, true));
        assert!(!quality_offered(&[Action::ToggleStats], false), "an empty ladder offers no page to open");
        assert!(quality_offered(&rows_for(false), false));
        for q in plx_media::route::QUALITY_LADDER {
            let mut st = menu(true, q);
            assert!(st.form.index_of(&MoreRow::OpenQuality).is_none());
            assert_eq!(st.on_ok(&ps), MoreOk::Action(Action::ToggleStats), "a vanished rung opens on the first option");
            assert_eq!(st.sel(), 0);
        }
        // not forced: the ladder is unchanged, in order, ahead of Options
        let open = rows_for(false);
        let ladder: Vec<Action> = open
            .iter()
            .copied()
            .filter(|a| matches!(a, Action::SetQuality(_)))
            .collect();
        let expected: Vec<Action> = plx_media::route::available_quality_ladder()
            .iter()
            .map(|q| Action::SetQuality(*q))
            .collect();
        assert_eq!(ladder, expected);
        assert_eq!(&open[ladder.len()..], &forced[..]);
    }

    /// **A live change refreshes the open Quality page IN PLACE**: a rung leaving (Auto's gate
    /// closing) or the current rung moving rebuilds the page, keeps the page open, the title band
    /// and the focused row by id (the checked rung when the focused one left).
    #[test]
    fn a_live_rung_change_refreshes_the_open_page_in_place() {
        let ps = plx_media::route::PlaybackSession::default();
        let mut st = quality_page(Quality::P1080);
        st.focus_key(Action::SetQuality(Quality::P720).key().0);
        let before = st.form.table.sel;
        // the current rung moves: the check follows, focus stays on the row the viewer was on
        let rows = st.rows.clone();
        assert!(st.refresh_to(&ps, false, rows.clone(), Quality::P480));
        assert!(!st.refresh_to(&ps, false, rows.clone(), Quality::P480), "and only once");
        assert_eq!(st.page(), Some(MorePage::Quality), "still on the page");
        assert_eq!(st.form.table.title(), Some("Quality"), "the title band stays");
        assert_eq!(st.form.selected_id(), Some(&MoreRow::Act(Action::SetQuality(Quality::P720))));
        assert_eq!(st.form.table.sel, before);
        assert!(checked(&mut st, MoreRow::Act(Action::SetQuality(Quality::P480))));
        assert!(!checked(&mut st, MoreRow::Act(Action::SetQuality(Quality::P1080))));
        // the focused rung leaves the ladder: focus falls back to the checked rung, page stays
        let without: Vec<Action> = rows.iter().copied().filter(|a| *a != Action::SetQuality(Quality::P720)).collect();
        assert!(st.refresh_to(&ps, false, without, Quality::P480));
        assert!(st.form.index_of(&MoreRow::Act(Action::SetQuality(Quality::P720))).is_none());
        assert_eq!(st.page(), Some(MorePage::Quality));
        assert_eq!(st.form.selected_id(), Some(&MoreRow::Act(Action::SetQuality(Quality::P480))));
    }

    /// **Quality becoming unavailable pops to the root**: Force Direct Play arriving under the open
    /// page drops the ladder, so the page goes, the band goes, the slide is cancelled and the root
    /// comes back without the Quality row.
    #[test]
    fn quality_becoming_unavailable_pops_the_page_to_the_root() {
        let ps = plx_media::route::PlaybackSession::default();
        let mut st = quality_page(Quality::P1080);
        assert!(st.refresh_to(&ps, true, rows_for(true), Quality::P1080));
        assert_eq!(st.page(), None);
        assert_eq!(st.form.table.title(), None);
        assert!(!st.transitioning(), "a page that vanished does not slide away");
        assert!(st.form.index_of(&MoreRow::OpenQuality).is_none());
        assert_eq!(st.form.selected_id(), Some(&MoreRow::Act(Action::ToggleStats)));
    }

    /// A key that names no row commits nothing, never a neighbour's action.
    #[test]
    fn an_unknown_key_is_none_not_a_neighbour() {
        let mut st = menu(false, Quality::Auto);
        let before = st.sel();
        assert!(!st.focus_key(0xdead), "an unknown key reports it seated nothing");
        assert_eq!(st.sel(), before, "an unknown key moves nothing");
        assert!(st.focus_key(MoreRow::OpenQuality.key().0), "the Quality row is seatable on the root");
    }

    /// Every row's key is distinct and below the band, and the title band's pointer key is none of
    /// them.
    #[test]
    fn every_key_is_distinct_and_below_the_band() {
        let mut all: Vec<MoreRow> = plx_media::route::QUALITY_LADDER.iter().map(|q| MoreRow::Act(Action::SetQuality(*q))).collect();
        all.extend([MoreRow::OpenQuality, MoreRow::Act(Action::ToggleStats), MoreRow::Act(Action::SendDiagnostics)]);
        let mut keys: Vec<u32> = all.iter().map(|a| a.key().0).collect();
        keys.sort_unstable();
        keys.dedup();
        assert_eq!(keys.len(), all.len());
        assert!(keys.iter().all(|k| *k < plx_ui::table_screen::BAND_BASE));
        assert!(page_stack::is_title_key(page_stack::TITLE_KEY));
        assert!(keys.iter().all(|k| !page_stack::is_title_key(*k)));
    }

    /// **The replay canon tells the root from the Quality page**, and one selected row from
    /// another: the same index on the two pages must not hash alike.
    #[test]
    fn the_canon_tells_the_root_and_the_page_apart() {
        use plx_machine::machine::Canon;
        let fp = |st: &MoreMenuState| {
            let mut c = Canon::new();
            st.canon(&mut c);
            c.finish()
        };
        let root = menu(false, Quality::P1080);
        let page = quality_page(Quality::P1080);
        assert_ne!(fp(&root), fp(&page), "depth and opener are in the canon");
        assert_eq!(fp(&page), fp(&quality_page(Quality::P1080)), "the same state hashes the same");
        let mut moved = quality_page(Quality::P1080);
        moved.focus_key(Action::SetQuality(Quality::P480).key().0);
        assert_ne!(fp(&page), fp(&moved), "the selected KEY is in the canon");
    }

    /// **A push slides the page and a pop slides it back; both rest.** The panel reports motion each
    /// frame it animates and asks for none once settled — the same bar as the track menu.
    #[test]
    fn a_push_and_a_pop_slide_and_then_rest() {
        let _serial = plx_base::testlock::serial();
        const DT: f32 = 1.0 / 60.0;
        let ps = plx_media::route::PlaybackSession::default();
        let m = plx_base::fontcov::advances::ShippedMeasure;
        let step = |st: &mut MoreMenuState| {
            plx_machine::idle::frame_begin(DT);
            st.update(DT, &m, &ps);
            plx_machine::idle::present_moving()
        };
        let mut st = menu(false, plx_media::route::quality());
        st.motion.step(DT, st.panel_rect(&m));
        assert!(!step(&mut st), "an opened menu is at rest");
        st.focus_key(MoreRow::OpenQuality.key().0);
        assert_eq!(st.on_ok(&ps), MoreOk::Navigated);
        assert!(st.motion.sliding());
        assert!(step(&mut st), "the slide is motion");
        assert!(st.motion.live_dx() > 0.0, "the arriving page comes in from the right");
        let mut n = 0;
        while step(&mut st) {
            n += 1;
            assert!(n < 240, "the push never settles");
        }
        assert!(!st.motion.sliding() && !st.transitioning());
        assert_eq!(st.motion.live_dx(), 0.0);
        assert!(!step(&mut st), "at rest: no frames requested");
        assert!(st.pop(&ps));
        assert!(st.motion.sliding());
        assert!(step(&mut st), "the pop is motion too");
        n = 0;
        while step(&mut st) {
            n += 1;
            assert!(n < 240, "the pop never settles");
        }
        assert!(!st.motion.sliding() && !st.transitioning());
        assert!(!step(&mut st));
    }

    /// **A row set that changes height animates the card** (`ui::panel_motion`): the panel opens
    /// at rest, a live change to the set (Force Direct Play dropping the Quality row, Auto's
    /// gate opening) rebuilds it and the top edge springs to the new layout while the bottom and
    /// right stay anchored, then it asks for no more frames.
    #[test]
    fn a_changed_row_set_resizes_the_card_with_a_spring_and_then_rests() {
        let _serial = plx_base::testlock::serial();
        const DT: f32 = 1.0 / 60.0;
        let ps = plx_media::route::PlaybackSession::default();
        let m = plx_base::fontcov::advances::ShippedMeasure;
        let step = |st: &mut MoreMenuState| {
            plx_machine::idle::frame_begin(DT);
            st.update(DT, &m, &ps);
            plx_machine::idle::present_moving()
        };
        // opened under Force Direct Play: no Quality row, a short panel
        let mut st = menu(true, plx_media::route::quality());
        let short = st.panel_rect(&m);
        st.motion.step(DT, short); // the open: the first step places the card AT its layout
        assert!(!st.transitioning(), "a freshly opened panel is at rest");
        assert_eq!(st.shown_rect(&m), short);

        // Force Direct Play is switched off under the open panel: the Quality row appears, the panel grows
        assert!(st.refresh(&ps), "the row set moved");
        assert!(!st.refresh(&ps), "and only once");
        let tall = st.panel_rect(&m);
        assert!(tall.h > short.h, "the Quality row makes the panel taller: {} > {}", tall.h, short.h);
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
    use plx_machine::machine::{FocusRead, InputOwner, PressRead, Tick};

    struct HostFixture;
    impl Host for HostFixture {
        type Arg = plx_ui::fixture::FixtureArg;
        type Fx = plx_ui::fixture::FixtureFx;
        type Msg = plx_ui::fixture::FixtureMsg;
        type Elem = u32;
        type Views<'a> = ();
        type Init = plx_ui::fixture::FixtureInit;
        type Memory = ();
    }

    fn with_cx<R>(entry: EntryId, test: impl FnOnce(&Cx<'_, HostFixture>) -> R) -> R {
        let measure = plx_ui::fixture::FixtureMeasure;
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
            (Action::SetQuality(plx_media::route::Quality::Original), "Rung A"),
            (Action::SetQuality(plx_media::route::Quality::Auto), "Rung B"),
            (Action::ToggleStats, "Stats for nerds"),
        ] {
            sec = sec.item(MoreRow::Act(a), RowKind::Button, a, Row::new(label));
        }
        let mut form = FormTable::new(plx_ui::table_screen::BAND_BASE);
        form.table.compact = true;
        form.set(Form::new().section(sec), None);
        MoreMenuState {
            form,
            rows: Vec::new(),
            forced: false,
            current: plx_media::route::Quality::Auto,
            pages: PageStack::new(),
            motion: PanelMotion::new(),
        }
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
        let r = st.panel_rect(&plx_ui::fixture::FixtureMeasure);
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

    /// **Every row fits the panel, in every shipped language** — the root with each possible
    /// current rung read out beside its Quality row, and the Quality page (title band included).
    /// The panel hugs its widest row up to [`MENU_MAX_W`](plx_ui::table::MENU_MAX_W), and a row
    /// elides its label to what the value beside it leaves — Spanish *Estadísticas avanzadas* and
    /// Belarusian *Падрабязная статыстыка* both ended in `…` beside their *Off*, and a rung's value
    /// ("1080p · 20 Mbps", "Original", "Автаматычна") shares its row with the label *Quality*.
    /// Measured with the device's whole-pixel advances.
    #[test]
    fn every_row_fits_the_panel_in_every_language() {
        use plx_platform::i18n::{language_on_this_thread_for_test, SHIPPED};
        let ps = plx_media::route::PlaybackSession::default();
        let mut out = Vec::new();
        let rows = rows_for(false);
        let mut check = |tag: &str, form: &MoreTable| {
            out.extend(form.table.menu_cap_failure(&plx_base::fontcov::advances::ShippedMeasure, tag));
            out.extend(form.table.app_fit_failures(plx_ui::table::MENU_MAX_W, tag));
            out.extend(form.table.app_fit_failures_hugged(tag));
        };
        for language in SHIPPED {
            let _guard = language_on_this_thread_for_test(language);
            for q in plx_media::route::QUALITY_LADDER {
                let mut root = MoreMenuState::new(&ps);
                root.form.set(root_form(&ps, &rows, false, q), None);
                check(language.tag(), &root.form);
                let mut page = MoreMenuState::new(&ps);
                page.current = q;
                page.rows = rows.clone();
                page.focus_key(MoreRow::OpenQuality.key().0);
                page.push(&ps, MorePage::Quality);
                check(language.tag(), &page.form);
            }
            // the forced root (Options only)
            let forced = MoreMenuState::new(&ps);
            check(language.tag(), &forced.form);
        }
        plx_ui::table::assert_no_fit_failures(&out);
    }
}
