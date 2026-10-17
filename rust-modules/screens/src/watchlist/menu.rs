//! **The Watchlist's Sort & Filter menu** (plan §2.4 W45; boards WatchlistOptions, Motion): a
//! `Style::Compact` surface presented over the page, a glass `Ground::Menu` panel that MORPHS out
//! of the header pill (DS6, DS7). The surface owns the open/morph state ([`OptionsMenu`]) and the
//! engine owns the highlight: every row is a focus stop keyed by its position in
//! [`OptionsRow::ALL`].
//!
//! OK on a row applies it ([`OptionsRow::apply`]) and reports the new value to the application
//! (`AppFx::Watchlist(WatchlistOut::SetOptions)`), which hands it to the store; the menu stays up
//! with the check marks moved, the Library sort menu's idiom. BACK dismisses.
use std::borrow::Cow;

use plx_machine::machine::{
    Canon, Cx, Edge, Effects, EntryId, FocusKey, Fx, GroupId, Handled, InputKind, Key, LogicalState, Machine, NavOp,
};
use plx_ui::frame::Budget;
use plx_ui::screen::{
    Activate, At, AxisMask, Dir, DrawFrame, EdgeRule, ElemKind, Focusable, GroupKind, GroupSpec, Hover, Placed,
    RenderStrategy, Screen, ScreenEvent, Scrim, Seat, Step, Stop,
};
use plx_ui::Rect;

use super::options::{OptionsMenu, OptionsRow, WatchlistOptionsState};
use super::page::WatchlistOut;
use crate::registry::{AppFx, ContentLike, WatchlistMenuArg};

pub const SHAPE: &str = "WatchlistMenu{arg:WatchlistMenuArg,options:{type:u32,sort:u32,descending:bool,show:u32},sel:u32,table:TableViewMotion}";

const GROUP: GroupId = GroupId(0);

pub struct WatchlistMenuScreen {
    entry: EntryId,
    arg: WatchlistMenuArg,
    /// The value the rows show; the application's store is the owner, this follows the presses.
    options: WatchlistOptionsState,
    menu: OptionsMenu,
    /// The highlighted row (an index into [`OptionsRow::ALL`]).
    sel: usize,
    opened: bool,
}

/// The pill rect a menu argument carries.
fn anchor_rect(arg: &WatchlistMenuArg) -> Rect {
    let [x, y, w, h] = arg.anchor.map(f32::from_bits);
    Rect::new(x, y, w, h)
}

/// The row a value opens on: the first row it has chosen (the Type choice), so a stray OK on a
/// freshly opened menu re-asserts what is already set.
fn opening_row(options: &WatchlistOptionsState) -> usize {
    OptionsRow::ALL.iter().position(|row| row.is_chosen(options)).unwrap_or(0)
}

impl WatchlistMenuScreen {
    pub fn new(entry: EntryId, arg: WatchlistMenuArg) -> Self {
        Self {
            entry,
            arg,
            options: arg.options,
            menu: OptionsMenu::new(),
            sel: opening_row(&arg.options),
            opened: false,
        }
    }

    fn key(&self, row: usize) -> FocusKey<u32> {
        FocusKey { entry: self.entry, elem: row as u32 }
    }

    fn row_of(&self, elem: u32) -> Option<usize> {
        ((elem as usize) < OptionsRow::ALL.len()).then_some(elem as usize)
    }

    fn activate<H: ContentLike>(&mut self, elem: u32, fx: &mut Effects<'_, H>) {
        let Some(row) = self.row_of(elem) else { return };
        self.sel = row;
        self.menu.set_selected(row);
        if self.menu.press(&mut self.options) {
            fx.push(Fx::App(AppFx::Watchlist(WatchlistOut::SetOptions(self.options))));
        }
    }

    /// The settled panel, for the focus engine and the overscan audit.
    fn frame(&self, measure: &dyn plx_machine::machine::Measure) -> Rect {
        self.menu.panel_rect(measure)
    }
}

impl<H: ContentLike> Machine<H> for WatchlistMenuScreen {
    type Ev = ScreenEvent<H>;

    fn step(&mut self, ev: &Self::Ev, cx: &Cx<'_, H>, fx: &mut Effects<'_, H>) -> Handled {
        match ev {
            ScreenEvent::Mount => {
                self.menu.open(anchor_rect(&self.arg), &self.options);
                self.menu.set_selected(self.sel);
                self.opened = true;
            }
            ScreenEvent::Tick(tick) => {
                if let Some(row) = cx
                    .focus
                    .current
                    .filter(|key| key.entry == self.entry)
                    .and_then(|key| self.row_of(key.elem))
                {
                    self.sel = row;
                    self.menu.set_selected(row);
                }
                self.menu.update(tick.dt(), cx.measure);
            }
            ScreenEvent::FocusMoved { to, .. } => {
                if let Some(row) = self.row_of(to.elem) {
                    self.sel = row;
                    self.menu.set_selected(row);
                }
            }
            ScreenEvent::Activate(elem) => self.activate(*elem, fx),
            ScreenEvent::PressCommit(_) => {
                if let Some(key) = cx.focus.current {
                    self.activate(key.elem, fx);
                }
            }
            ScreenEvent::Input(input) => {
                if let InputKind::Key { key: Key::Back, edge: Edge::Down, .. } = input.kind {
                    self.menu.dismiss();
                    fx.push(Fx::Nav(NavOp::Dismiss(self.entry)));
                    return Handled::Yes;
                }
            }
            _ => {}
        }
        Handled::No
    }
}

impl<H: ContentLike> Focusable<H> for WatchlistMenuScreen {
    fn groups(&self, cx: &Cx<'_, H>, out: &mut Vec<GroupSpec>) {
        out.push(GroupSpec {
            id: GROUP,
            kind: GroupKind::Column,
            seat: Seat::Remembered,
            reachable: AxisMask::BOTH,
            edge: [EdgeRule::Stop; 4],
            extent: self.frame(cx.measure),
            len: OptionsRow::ALL.len(),
            elem: ElemKind::Bare,
        });
    }
    fn group_of(&self, elem: &u32, _: &Cx<'_, H>) -> Option<GroupId> {
        self.row_of(*elem).map(|_| GROUP)
    }
    fn neighbour(&self, key: FocusKey<u32>, dir: Dir, _: &Cx<'_, H>) -> Step<u32> {
        let Some(row) = self.row_of(key.elem) else { return Step::Edge };
        let next = match dir {
            Dir::Up => row.checked_sub(1),
            Dir::Down => (row + 1 < OptionsRow::ALL.len()).then_some(row + 1),
            _ => None,
        };
        next.map_or(Step::Edge, |next| Step::Move(self.key(next)))
    }
    fn place(&self, elem: &u32, cx: &Cx<'_, H>, _: At) -> Option<Placed> {
        let row = self.row_of(*elem)?;
        let rect = self.menu.row_frame(row, cx.measure)?;
        Some(Placed { rect, rest_rect: rect, clip: self.frame(cx.measure), index: Some(row as u32) })
    }
    fn reconcile(&self, want: FocusKey<u32>, cx: &Cx<'_, H>) -> FocusKey<u32> {
        if self.group_of(&want.elem, cx).is_some() {
            want
        } else {
            self.key(self.sel)
        }
    }
    fn seat(&self, _: GroupId, _: Placed, _: &Cx<'_, H>) -> FocusKey<u32> {
        self.key(self.sel)
    }
}

impl<H: ContentLike> Screen<H> for WatchlistMenuScreen {
    /// The pill the panel grew out of is THIS surface's while it is up (DS7: the pill's glass
    /// becomes the panel), so the page leaves it out of its pass (`DrawFrame::lifted`) and
    /// [`draw_pill`](Self::draw_pill) is its only copy. Left on the page, the pill sat under the
    /// scrim for the whole menu, and the settled panel — slid up over it, being taller than the
    /// room below — refracted its label into the glass rim.
    /// Sort & Filter shows the effect of each click: the page reflows under the open menu, and
    /// the glass stands on a host that follows it (`popover::host::set_reflow`).
    fn lets_host_reflow(&self) -> bool {
        true
    }
    fn opener(&self) -> Option<(EntryId, Option<FocusKey<u32>>)> {
        Some((self.arg.host, Some(FocusKey { entry: self.arg.host, elem: super::page::PILL_ELEM })))
    }
    fn as_any(&self) -> Option<&dyn std::any::Any> {
        Some(self)
    }
    fn name(&self) -> &'static str {
        "watchlist_menu"
    }
    fn state(&self) -> &dyn LogicalState {
        self
    }
    fn crumb(&self, _: &Cx<'_, H>) -> Option<Cow<'_, str>> {
        None
    }
    /// The COMPACT role (DS6's 30% scrim): a chip-shaped control on a live page opening a list.
    fn scrim(&self) -> Scrim {
        Scrim::dim(plx_ui::theme::underlay::DIM_COMPACT)
    }
    fn prepare(&mut self, _: &mut Budget, _: &Cx<'_, H>) {}
    /// The menu and the pill it grows out of, as one surface (`OptionsMenu::draw`). It is drawn
    /// on the frame painter, NOT faded by the container's appear (`f.page_alpha`, which still
    /// gates the hit stops): the surface begins and ends as the pill, so it has nothing to fade in
    /// or out. Faded by it, the landed pill dimmed away under the closing scrim and the page's own
    /// pill popped in at full strength when the surface left (frame dump at 572f46d07, n325–352).
    fn draw(&mut self, f: &mut DrawFrame<'_, '_, H>) {
        let p = f.painter.alpha(f.page_alpha);
        self.menu.draw(f.painter, f.underlay, f.measure, &self.options);
        for row in 0..OptionsRow::ALL.len() {
            let Some(rect) = self.menu.row_frame(row, f.measure) else { continue };
            f.stop(
                p,
                Stop {
                    key: self.key(row),
                    rect,
                    rest_rect: rect,
                    clip: self.frame(f.measure),
                    hover: Hover::Focus,
                    activate: Activate::Immediate,
                },
            );
        }
    }
    fn render(&self) -> RenderStrategy {
        RenderStrategy::Page
    }
    /// Held (its capture frame, its text warming), the surface is still the pill the page has left
    /// out: drawn here, it does not vanish on OK.
    fn draw_held(&mut self, f: &mut DrawFrame<'_, '_, H>) {
        self.menu.draw_face(f.painter, f.measure, &self.options);
    }
}

impl LogicalState for WatchlistMenuScreen {
    fn write(&self, c: &mut Canon) {
        self.arg.write(c);
        self.options.write_canon(c);
        c.u32(self.sel as u32);
        self.menu.write_motion(c);
    }
    fn probe(&self, out: &mut String) {
        out.push_str(&format!("watchlist_menu sel={} opened={}", self.sel, self.opened));
    }
}
