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
//! **Options** holds switches. Its row carries [`Row::toggle`], so it states itself as the WORD
//! `On`/`Off` at the row's trailing edge. It is a STATE, not a destination: a chevron would promise
//! a page behind the row and there is none.
//!
//! **Quality** is the [`crate::route::Quality`] ladder — Original, fixed rungs, and Auto once its
//! playback readiness gate opens — and its rows carry
//! [`Row::checked`]'s LEADING checkmark, which means "the active one of several". That is the same
//! design-system rule from the other side: **a mark says where you are and a word says what is set,
//! and no row says both**, which is why a rung's rate rides inside its own label rather than in a
//! trailing value beside the mark. (The Options row drew as a PAIR OF MARKS for one day, a ring
//! ticked when on; those assets were deleted the same evening — see [`crate::ui::icons`].)
//!
//! A flat popover with a header per section, deliberately, rather than a Quality row that drills
//! into a second page: `docs/parity-gaps.md`'s standing decision is that this app has **no
//! full-screen menu sheets** — the reference clients put playback quality in one and we do not —
//! and a drill-in inside a popover would need a BACK that means "up one page" where every other
//! panel's BACK means "dismiss". Six rungs and a switch fit; when they stop fitting, the
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
use crate::ui::table::{Row, Section, TableView};
use crate::ui::{theme, Rect};
use std::os::raw::c_int;

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
pub(crate) struct MoreMenuState {
    table: TableView, // main-thread only
    /// The ordered rows captured at construction — the ONE place row order lives, so [`on_ok`]'s
    /// index mapping cannot drift from what was drawn. (`account_menu`'s rationale, and its bug.)
    ///
    /// An owned `Vec` rather than a `&'static [Action]`, because the Quality section's rows are
    /// BUILT from `route::available_quality_ladder` rather than written out here.
    rows: Vec<Action>,
}

impl MoreMenuState {
    fn open_focused(ps: &crate::route::PlaybackSession, quality: Option<crate::route::Quality>) -> Self {
        let rows = rows_for();
        let initial = initial_selection(&rows, quality);
        // TWO sections, built in ROWS order — see `rows_for`: `TableView::sel` is one flat index over
        // both, so the split here is presentational and the ORDER is the contract.
        let mut options = Section::new("Options");
        let mut quality_sec = Section::new("Quality");
        for a in &rows {
            match a {
                Action::SetQuality(_) => quality_sec = quality_sec.row(row_for(ps, *a)),
                _ => options = options.row(row_for(ps, *a)),
            }
        }
        let mut table = TableView::new();
        table.compact = true; // a short action list — BODY labels, like the profile menu
        table.set_sections(vec![options, quality_sec], initial, false);
        // `rows` *is* the index→action map, so it must stay one-to-one with what was built above.
        debug_assert_eq!(rows.len() as i32, table.n_rows());
        MoreMenuState { table, rows }
    }

    pub(crate) fn new(ps: &crate::route::PlaybackSession) -> Self {
        Self::open_focused(ps, None)
    }

    /// The existing overflow menu, focused directly on the active quality row.
    ///
    /// The terminal playback screen has no transport discs, so OK enters the one useful recovery
    /// section explicitly rather than parking on "Stats for nerds". It is still the SAME TableView
    /// and action map as the ordinary `…` menu; only the initial cursor differs.
    pub(crate) fn new_quality(ps: &crate::route::PlaybackSession) -> Self {
        Self::open_focused(ps, Some(crate::route::quality()))
    }

    /// The highlighted row, for the focus probe (`crate::focusprobe`) — a READ of the cursor the
    /// key ladder moves, and the reason it exists: `app.rs`'s UP/DOWN arm for this panel changes
    /// nothing else, so without this the fingerprint records the panel opening and closing and
    /// nothing between.
    pub(crate) fn sel(&self) -> i32 {
        self.table.sel
    }

    pub(crate) fn move_focus(&mut self, sym: c_int) {
        let s = sym as u32;
        if s == SDLK_UP {
            self.table.move_sel(-1);
        } else if s == SDLK_DOWN {
            self.table.move_sel(1);
        }
    }

    /// Pointer hover: focus follows the cursor over the popover rows.
    pub(crate) fn pointer_focus(&mut self, mx: f32, my: f32) {
        if let Some(gi) = self.table.hit_row(self.panel_rect(), mx, my) {
            self.table.sel = gi;
        }
    }

    /// Pointer click: commit the row under the cursor (same as OK); a click elsewhere reports
    /// `Action::None` and the caller dismisses like BACK.
    pub(crate) fn click(&mut self, mx: f32, my: f32) -> Action {
        if let Some(gi) = self.table.hit_row(self.panel_rect(), mx, my) {
            self.table.sel = gi;
            return self.on_ok();
        }
        Action::None
    }

    /// Commit the highlighted row — dismissing the panel afterward is the container's job now, not
    /// this method's.
    pub(crate) fn on_ok(&self) -> Action {
        let sel = self.table.sel;
        action_at(&self.rows, sel)
    }

    /// Bottom-right, above the control row — anchored to the `…` disc that opened it, the way the
    /// track menu is anchored to the pair beside it. Shares the track menu's right margin
    /// (`player_hud::CTRL_RIGHT`, the discs' own edge) and its bottom edge, so opening one after the
    /// other does not make the panel hop.
    fn panel_rect(&self) -> Rect {
        let pw = 448.0f32;
        let px = crate::ui::player_hud::CTRL_RIGHT - pw;
        let bottom = SCR_H - 316.0; // ~28px above the discs, as track_menu
                                    // The ceiling was 320 while this menu held one row, and it was invisible then. With the
                                    // Quality ladder beside it `measured_height()` can reach 600 when Auto is enabled — two
                                    // headers, seven rows, a divider, AND the table's own top/bottom padding — so a 320 cap put
                                    // four of nine rows on screen and
                                    // silently scrolled the rest, which is a picker whose options you cannot see.
                                    //
                                    // The cap is a FRACTION of the room the panel has rather than a subtraction from it: the panel
                                    // is anchored at `bottom` and grows upward, so `bottom` IS the space, and 0.86 of it leaves a
                                    // clear margin at the top of the frame while comfortably clearing 600. Reaching for a
                                    // `bottom - <margin>` literal is what put the first version of this line 4px UNDER the content
                                    // — the margin was derived from the 560 of content and forgot the 40 of padding, so the last
                                    // rung was clipped until you scrolled: the same symptom, one row deep instead of five. Past
                                    // the cap it scrolls, which is what `TableView` is for.
        let ph = self.table.measured_height().clamp(120.0, bottom * 0.86);
        Rect::new(px, bottom - ph, pw, ph)
    }

    pub(crate) fn update(&mut self, dt: f32) {
        // `update` subtracts its own top/bottom padding now — pass the panel's raw height.
        let h = self.panel_rect().h;
        self.table.update(dt, h);
    }

    pub(crate) fn draw(&mut self, appear: f32) {
        // rises INTO place from below, toward the disc that opened it — reproduces exactly what
        // `Popover::painter(0.5, 16.0)` used to draw (scrim + content painter).
        let dim = theme::scrim_black(0.5 * appear);
        crate::ui::Painter::root().rect(Rect::FULL, 0.0, dim, dim, 0.0);
        let p = crate::ui::Painter::root()
            .alpha(appear)
            .translate(0.0, 16.0 * (1.0 - appear));
        let r = self.panel_rect();
        p.rect(r, 24.0, theme::PANEL_TOP, theme::PANEL_BOT, 0.0);
        self.table.draw(p, r);
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
fn rows_for() -> Vec<Action> {
    let mut v = vec![Action::ToggleStats];
    if crate::lab::menu_row_enabled() {
        v.push(Action::SendDiagnostics);
    }
    v.extend(
        crate::route::available_quality_ladder()
            .iter()
            .map(|q| Action::SetQuality(*q)),
    );
    v
}

fn label(a: Action) -> &'static str {
    match a {
        Action::ToggleStats => "Stats for nerds",
        // the rung names itself — rate and frame in one string, because the row already carries
        // the picker's leading mark (see this module's doc)
        Action::SetQuality(q) => q.label(),
        Action::SendDiagnostics => "Send diagnostics",
        Action::None => "",
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
        "Converts on server"
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

fn initial_selection(rows: &[Action], quality: Option<crate::route::Quality>) -> i32 {
    quality
        .and_then(|q| {
            rows.iter()
                .position(|a| *a == Action::SetQuality(q))
                .and_then(|i| i32::try_from(i).ok())
        })
        .unwrap_or(0)
}

/// The row list IS the mapping — a selection outside it is `None` rather than whatever action
/// happens to sit at that index in some other row set.
fn action_at(rows: &[Action], sel: i32) -> Action {
    usize::try_from(sel)
        .ok()
        .and_then(|i| rows.get(i))
        .copied()
        .unwrap_or(Action::None)
}

/// The panel at its TALLEST, for the overscan audit ([`crate::ui::consts::SAFE`]).
#[cfg(test)]
pub(crate) fn overscan_rects(out: &mut Vec<(&'static str, Rect)>) {
    let (pw, ph) = (448.0f32, 320.0f32);
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
            crate::ui::fmt::CONVERTS_ON_SERVER,
        );
    }

    #[test]
    fn every_row_has_a_label() {
        for a in rows_for() {
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

    #[test]
    fn a_selection_maps_to_its_row() {
        let rows = rows_for();
        assert_eq!(action_at(&rows, 0), Action::ToggleStats);
        // …and the Quality section follows the Options one, in the available ladder order.
        // `sel` is ONE flat index over both sections, so this is the join that a section split
        // could quietly break: row 1 must be the ladder's head, not its second rung.
        assert_eq!(
            action_at(&rows, 1),
            Action::SetQuality(crate::route::Quality::Auto)
        );
        for (i, q) in crate::route::available_quality_ladder().iter().enumerate() {
            assert_eq!(action_at(&rows, 1 + i as i32), Action::SetQuality(*q));
        }
    }

    #[test]
    fn failure_entry_can_focus_the_active_quality_in_the_shared_menu() {
        let rows = rows_for();
        for q in crate::route::available_quality_ladder() {
            let i = rows
                .iter()
                .position(|a| *a == Action::SetQuality(*q))
                .expect("every available quality has a row");
            assert_eq!(initial_selection(&rows, Some(*q)), i as i32);
            assert!(i > 0, "quality recovery must skip the Options section");
        }
        assert_eq!(
            initial_selection(&rows, None),
            0,
            "ordinary … starts at Options"
        );
    }

    /// Out-of-range must be `None`, never a neighbouring action: `sel` survives a rebuild, so a
    /// shorter row set can be asked for an index the previous one had.
    #[test]
    fn an_out_of_range_selection_is_none_not_a_neighbour() {
        let rows = rows_for();
        assert_eq!(action_at(&rows, rows.len() as i32), Action::None);
        assert_eq!(action_at(&rows, -1), Action::None);
        assert_eq!(action_at(&[], 0), Action::None);
    }
}
