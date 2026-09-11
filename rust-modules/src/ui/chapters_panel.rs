//! In-player Chapters strip: a horizontal row of chapter cards (thumbnail + name + timestamp) over
//! the transport, opened from the HUD's Chapters tab. LEFT/RIGHT pick a chapter, OK seeks to its
//! start. Card layout mirrors the detail-page episode picker; modal wiring mirrors info_panel.
//!
//! Data comes from the PLAYING leaf (`metadata::playing_chapters`, loaded with `?includeChapters=1`
//! on the same fetch the track store already makes), never from `metadata::current()` — the same
//! identity rule `ui/track_menu.rs` and `screens/player/skip_pill.rs` state. Reading `current()` is what made
//! the Chapters tab vanish for every episode started from a show detail page: `current()` is then
//! the SHOW, and a show container carries no `Chapter[]`.
#![allow(dead_code)]
use crate::metadata;
use crate::ui::consts::{MARGIN_X, SCR_W};
use crate::ui::frame::Budget;
use crate::ui::geom::IndexElem;
use crate::ui::machine::{Cx, EntryId, FocusKey, GroupId, Host};
use crate::ui::screen::{
    Activate, At, AxisMask, Dir, DrawFrame, EdgeRule, ElemKind, Focusable, GroupKind, GroupSpec,
    Hover, Part, Placed, Seat, Step, Stop,
};
use crate::ui::theme;
use crate::ui::{Painter, Rect, Spring};
use std::ffi::CString;
use std::os::raw::c_int;

const CH_W: f32 = 288.0;
const CH_H: f32 = 162.0; // 16:9 still
const CH_GAP: f32 = 24.0;
const CH_TOP: f32 = 684.0; // thumbnail top — name/time fit above the tabs (SCR_H-128)
use crate::ui::widgets::CARD_FOCUS_SCALE;

/// The strip's whole state, owned by the container that mounts this panel — the modal PHASE and
/// the appear spring belong to `ui::containers::modal::ModalStack` now, not to this struct; `draw`
/// takes the appear fraction as a parameter instead of stepping its own `Popover`.
pub(crate) struct ChaptersState {
    sel: c_int,
    scroll: Spring, // horizontal scroll offset (px)
    scale: Spring,  // focused-card pop (springs 1.0 → FOCUS_SCALE on each move)
}

impl ChaptersState {
    /// focus the chapter that contains the current playhead
    pub(crate) fn new() -> Self {
        let pos_ms = crate::player::playpos_ns() / 1_000_000;
        let sel = chapters()
            .iter()
            .rposition(|c| c.start_ms <= pos_ms)
            .unwrap_or(0) as c_int;
        ChaptersState {
            sel,
            scroll: Spring::at(scroll_target(sel)),
            scale: Spring::at(1.0), // pop in
        }
    }

    /// The highlighted chapter, for the focus probe (`crate::focusprobe`). The strip's LEFT/RIGHT
    /// arm in `app.rs` moves this and nothing else, so the fingerprint is blind to it without a
    /// reader.
    pub(crate) fn sel(&self) -> c_int {
        self.sel
    }

    /// **Write back the engine's own focus cursor** (restructure phase 12): the Row group
    /// [`ChaptersPart`] answers is the source of geometry, but the ENGINE owns the current
    /// element (§7.3 step 5) — the owner's `step` is the only place that mutates in response to a
    /// `FocusMoved`, and this is `screens::player::overlay::PlayerOverlayScreen::step`'s write.
    /// Re-pops the card exactly as the old `move_focus` did on an actual change.
    pub(crate) fn set_sel(&mut self, i: c_int) {
        if i != self.sel {
            self.scale.jump(1.0); // re-pop the newly-focused card
        }
        self.sel = i;
    }

    /// seek target (nanoseconds) for the focused chapter, or -1 if none.
    pub(crate) fn on_ok(&self) -> i64 {
        let s = self.sel;
        chapters()
            .get(s.max(0) as usize)
            .map(|c| c.start_ms * 1_000_000)
            .unwrap_or(-1)
    }

    pub(crate) fn update(&mut self, dt: f32) {
        // The store this indexes belongs to the PLAYING item and a new play retires it, so re-clamp
        // rather than spring the scroll toward a slot that no longer exists (which culls every card
        // and leaves an empty panel). `on_ok`/`draw` are `.get()`-based, so this is about the strip
        // staying coherent, not about safety.
        let sel = self.sel.min((n() - 1).max(0));
        self.sel = sel;
        let sctgt = scroll_target(sel);
        self.scroll.step(sctgt, 220.0, dt);
        crate::ui::anim::probe("chapters.scroll", self.scroll.pos, self.scroll.vel, sctgt, dt);
        self.scale.step(CARD_FOCUS_SCALE, 300.0, dt);
        crate::ui::anim::probe(
            "chapters.scale",
            self.scale.pos,
            self.scale.vel,
            CARD_FOCUS_SCALE,
            dt,
        );
    }

    pub(crate) fn draw(
        &mut self,
        ps: &crate::route::PlaybackSession,
        appear: f32,
        measure: &dyn crate::ui::machine::Measure,
    ) {
        let chs = chapters();
        if chs.is_empty() {
            return;
        }
        let scroll = self.scroll.pos;
        let sel = self.sel;
        let scale = self.scale.pos;
        // reproduces exactly what `Popover::painter(0.0, 20.0)` (no scrim + `content_painter(20.0)`)
        // used to draw, translated further by the strip's own horizontal scroll.
        let p = crate::ui::Painter::root()
            .alpha(appear)
            .translate(0.0, crate::ui::popover::Popover::RISE * (1.0 - appear))
            .translate(-scroll, 0.0);

        // timecode uses SECONDARY (not the dim TERTIARY): it's drawn straight over the video, where the
        // dim grey washed out even up close. SECONDARY matches the (readable) chapter-name grey; the
        // name still leads by size (LABEL vs CAPTION) + bold.
        let dimc = theme::TEXT_SECONDARY;
        for (i, ch) in chs.iter().enumerate() {
            let x = MARGIN_X + i as f32 * (CH_W + CH_GAP);
            if !crate::ui::on_axis(x - scroll, CH_W, SCR_W, 0.0) {
                continue; // culled off-screen (the shared cull primitive)
            }
            let focused = i as c_int == sel;
            let card = Rect::new(x, CH_TOP, CH_W, CH_H);
            crate::ui::widgets::draw_card(
                p,
                card,
                crate::route::item_sid(crate::route::cur_sid(ps)),
                &ch.thumb,
                (480, 270),
                10.0,
                focused,
                scale,
            );
            // name + timestamp beneath the card
            let ty = CH_TOP + CH_H + 26.0;
            let titc = if focused {
                theme::TEXT_PRIMARY
            } else {
                theme::TEXT_SECONDARY
            };
            let name = if ch.title.trim().is_empty() {
                format!("Chapter {}", ch.index)
            } else {
                ch.title.clone()
            };
            if let Ok(tc) = CString::new(crate::text::elide_by(&name, CH_W, false, |t| {
                measure.width_str(t, theme::size::LABEL, true)
            })) {
                p.text(tc.as_ptr(), x, ty, theme::size::LABEL, titc, 0, 1);
            }
            if let Ok(sc) = CString::new(crate::ui::fmt::clock(ch.start_ms)) {
                p.text(sc.as_ptr(), x, ty + 34.0, theme::size::CAPTION, dimc, 0, 0);
            }
        }
    }
}

/// the playing leaf's chapters — the ONE read, so within a frame the count, the open, the seek and
/// the draw cannot end up describing different items. ACROSS frames the store can still be replaced
/// (a new play retires it, `route::request_play`), which is why `update` re-clamps the selection.
fn chapters() -> &'static [metadata::Chapter] {
    metadata::playing_chapters()
}
fn n() -> c_int {
    chapters().len() as c_int
}
/// whether the PLAYING item has chapters — drives showing/hiding the Chapters tab
pub(crate) fn has_chapters() -> bool {
    n() > 0
}

fn scroll_target(sel: c_int) -> f32 {
    // pin the focused card to the 2nd slot (like the episode picker)
    if sel > 1 {
        (sel as f32 - 1.0) * (CH_W + CH_GAP)
    } else {
        0.0
    }
}

/// A card's on-screen rect at `scroll` — the same formula [`ChaptersState::draw`] paints at
/// (`MARGIN_X + i*(CH_W+CH_GAP)`, translated by `-scroll`, exactly as the draw's own painter
/// cascade does), so a stop registered from it lands on the pixel the card was drawn at.
fn card_rect(i: usize, scroll: f32) -> Rect {
    Rect::new(MARGIN_X + i as f32 * (CH_W + CH_GAP) - scroll, CH_TOP, CH_W, CH_H)
}

/// The strip's own clip — every card's stop is bounded to it, matching the cull test
/// [`ChaptersState::draw`] runs per card (`crate::ui::on_axis`).
fn strip_extent() -> Rect {
    Rect::new(0.0, CH_TOP, SCR_W, CH_H)
}

fn step_index<K: IndexElem>(entry: EntryId, k: FocusKey<K>, dir: Dir, n: usize) -> Step<K> {
    let Some(i) = k.elem.index() else {
        return Step::Edge;
    };
    let i = i as usize;
    match dir {
        Dir::Left if i > 0 => Step::Move(FocusKey { entry, elem: K::of_index(i as u32 - 1) }),
        Dir::Right if i + 1 < n => Step::Move(FocusKey { entry, elem: K::of_index(i as u32 + 1) }),
        _ => Step::Edge,
    }
}

fn clamp_index<K: IndexElem>(entry: EntryId, want: FocusKey<K>, n: usize) -> FocusKey<K> {
    let i = want.elem.index().unwrap_or(0) as usize;
    FocusKey {
        entry,
        elem: K::of_index(i.min(n.saturating_sub(1)) as u32),
    }
}

/// **The Engine-shaped view of this strip** (restructure phase 12): one horizontal `Row` focus
/// group over the chapter cards, built fresh by `screens::player::overlay::PlayerOverlayScreen`
/// each frame from a `&ChaptersState` — the same borrowed-view shape `ui::table_screen::TablePart`
/// and `ui::geom::Shelf` use for every other list-shaped component, so the strip answers the same
/// [`Focusable`]/[`Part`] query protocol every other screen does.
///
/// **`state` is a SHARED reference** — every [`Focusable`] method here is a pure read (`&self`),
/// and the owning screen's own `Focusable` impl only ever has `&self` too (§7.1: "the engine never
/// mutates a screen"), so a mutable field would make this type unconstructable from there. The
/// actual paint (`ChaptersState::draw`) stays a direct call on the owned `Panel` from
/// `PlayerOverlayScreen::draw`'s `&mut self`; [`Part::draw`] below only registers stops.
pub(crate) struct ChaptersPart<'a> {
    pub(crate) state: &'a ChaptersState,
    pub(crate) entry: EntryId,
    pub(crate) group: GroupId,
}

impl<H: Host> Focusable<H> for ChaptersPart<'_>
where
    H::Elem: IndexElem,
{
    fn groups(&self, _cx: &Cx<'_, H>, out: &mut Vec<GroupSpec>) {
        let nn = n() as usize;
        if nn == 0 {
            return;
        }
        out.push(GroupSpec {
            id: self.group,
            kind: GroupKind::Row { wrap: false },
            seat: Seat::Nearest,
            reachable: AxisMask::HORIZONTAL,
            // UP is swallowed (no group above, as the old ladder's `_ => {}` left it); DOWN drops
            // focus back onto the HUD tabs, which only the owning screen can do —
            // `overlay.rs`'s `Key::Down => … FocusTabs`.
            edge: [EdgeRule::Stop, EdgeRule::Screen, EdgeRule::Stop, EdgeRule::Stop],
            extent: strip_extent(),
            len: nn,
            elem: ElemKind::Card,
        });
    }
    fn group_of(&self, key: &H::Elem, _cx: &Cx<'_, H>) -> Option<GroupId> {
        ((key.index()? as usize) < n() as usize).then_some(self.group)
    }
    fn neighbour(&self, key: FocusKey<H::Elem>, dir: Dir, _cx: &Cx<'_, H>) -> Step<H::Elem> {
        step_index(self.entry, key, dir, n() as usize)
    }
    fn place(&self, key: &H::Elem, _cx: &Cx<'_, H>, _at: At) -> Option<Placed> {
        let i = key.index()? as usize;
        if i >= n() as usize {
            return None;
        }
        let rect = card_rect(i, self.state.scroll.pos);
        Some(Placed {
            rect,
            rest_rect: rect,
            clip: strip_extent(),
            index: Some(i as u32),
        })
    }
    fn reconcile(&self, want: FocusKey<H::Elem>, _cx: &Cx<'_, H>) -> FocusKey<H::Elem> {
        clamp_index(self.entry, want, n() as usize)
    }
    fn seat(&self, _g: GroupId, _from: Placed, _cx: &Cx<'_, H>) -> FocusKey<H::Elem> {
        FocusKey {
            entry: self.entry,
            elem: H::Elem::of_index(self.state.sel.max(0) as u32),
        }
    }
}

impl<H: Host> Part<H> for ChaptersPart<'_>
where
    H::Elem: IndexElem,
{
    fn prepare(&mut self, _b: &mut Budget, _cx: &Cx<'_, H>) {}
    /// Registers every card's stop (§7.6); the strip's own paint happens directly on the owned
    /// `ChaptersState` from `PlayerOverlayScreen::draw` (see the struct doc above).
    fn draw(&mut self, f: &mut DrawFrame<'_, '_, H>, _rect: Rect) {
        // Every card is a stop (rule 11: hover parks, a click activates) — registered in ROOT
        // space because `card_rect` already resolves the strip's own scroll offset, exactly as
        // `TablePart::draw` registers a table's already-absolute row rects.
        let p = Painter::root();
        let nn = n() as usize;
        for i in 0..nn {
            f.stop(
                p,
                Stop {
                    key: FocusKey {
                        entry: self.entry,
                        elem: H::Elem::of_index(i as u32),
                    },
                    rect: card_rect(i, self.state.scroll.pos),
                    rest_rect: card_rect(i, self.state.scroll.pos),
                    clip: strip_extent(),
                    hover: Hover::Focus,
                    activate: Activate::Direct,
                },
            );
        }
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

    /// **The pure geometric core, independent of the live playing item**: LEFT/RIGHT step by one,
    /// clamped at both ends, matching [`ChaptersState::move_focus`]'s clamp exactly (no wrap).
    #[test]
    fn left_right_step_by_one_and_clamp_at_both_ends() {
        let e = EntryId(9);
        let step = |i: u32, dir: Dir| match step_index::<u32>(e, FocusKey { entry: e, elem: i }, dir, 5) {
            Step::Move(k) => Some(k.elem),
            Step::Edge => None,
        };
        assert_eq!(step(0, Dir::Right), Some(1));
        assert_eq!(step(4, Dir::Right), None, "the last card does not wrap");
        assert_eq!(step(0, Dir::Left), None, "the first card does not wrap");
        assert_eq!(step(2, Dir::Left), Some(1));
        // Up/Down never move within the strip — they are the screen's own edge handling.
        assert!(matches!(
            step_index::<u32>(e, FocusKey { entry: e, elem: 2 }, Dir::Up, 5),
            Step::Edge
        ));
    }

    /// A cursor past the end of a shorter (replaced) chapter list settles onto the last card,
    /// mirroring [`ChaptersState::update`]'s own re-clamp of `sel`.
    #[test]
    fn an_out_of_range_cursor_clamps_to_the_last_card() {
        let e = EntryId(9);
        let got = clamp_index::<u32>(e, FocusKey { entry: e, elem: 99 }, 5);
        assert_eq!(got.elem, 4);
        assert_eq!(clamp_index::<u32>(e, FocusKey { entry: e, elem: 0 }, 0).elem, 0);
    }

    /// A card's placed rect is exactly the strip's own draw formula, so a stop built from it
    /// lands on the pixel the card was painted at.
    #[test]
    fn a_cards_rect_matches_the_draw_formula() {
        let r = card_rect(2, 40.0);
        assert_eq!(r.x, MARGIN_X + 2.0 * (CH_W + CH_GAP) - 40.0);
        assert_eq!(r.y, CH_TOP);
        assert_eq!(r.w, CH_W);
        assert_eq!(r.h, CH_H);
    }

    /// `Focusable::place`/`group_of`/`reconcile` agree with the pure helpers above through the
    /// real trait dispatch, over `HostFixture` — the same shape the already-converted screens are
    /// host-tested with (`ItemMenuScreen`'s `HostFixture`/`with_cx`).
    #[test]
    fn the_focusable_impl_dispatches_to_the_same_pure_geometry() {
        let e = EntryId(3);
        let st = ChaptersState {
            sel: 1,
            scroll: Spring::at(0.0),
            scale: Spring::at(1.0),
        };
        let part = ChaptersPart { state: &st, entry: e, group: GroupId(0) };
        with_cx(e, |cx| {
            let placed = <ChaptersPart as Focusable<HostFixture>>::place(&part, &0u32, cx, At::Drawn);
            if let Some(placed) = placed {
                let want = card_rect(0, 0.0);
                assert_eq!((placed.rect.x, placed.rect.y), (want.x, want.y));
            }
            assert_eq!(
                <ChaptersPart as Focusable<HostFixture>>::group_of(&part, &0u32, cx),
                (n() > 0).then_some(GroupId(0))
            );
        });
    }
}
