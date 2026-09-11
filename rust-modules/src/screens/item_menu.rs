//! The **item context menu** — a registered `Style::Compact` surface on the shared `ModalStack`,
//! opened by a press-and-hold on a card (or on the detail page's episode still / season tab).
//!
//! The reference is **Apple TV's card menu, not Plex's**: a panel anchored BESIDE the focused card
//! (the card and the rest of the shelf stay where they are, visible behind it), rows of
//! `[leading icon] [label]`, the focused row a filled light pill spanning the panel, and a hairline
//! separator between the navigation actions and the state actions. Plex's own version is a
//! full-screen sheet; that is deliberately not what this is.
//!
//! It exists because OK on a Continue Watching tile **resumes immediately, by design** — the amber
//! play badge on the card is the affordance that says so. The hold is the *other* half of that
//! interaction, and until it existed Go-to-Show, per-item Mark-as-Watched and Play-from-Start had
//! nowhere to live (`docs/parity-gaps.md` §1.2/§1.3, §5a).
//!
//! **Every card surface opens it** — a home shelf, the Library browse grid, a Search result shelf,
//! a person's filmography and the detail page's Related shelf present [`ItemMenuKind::Card`]; the
//! detail page's episode filmstrip and season tabs present [`ItemMenuKind::Episode`] /
//! [`ItemMenuKind::Season`]. The row SET differs only because a navigation row that leads to the
//! page you are standing on is not an action; everything else — the panel, the placement, the
//! choreography, the state rows — is shared.
//!
//! **What remains excluded is excluded for a reason that does not dissolve.** A tile that is a
//! PERSON or a TAG has no rating key and no watch state at all, so every row this module can build
//! would be absent and the hold would open an empty panel: the detail page's cast headshots, and
//! Search's Cast & Crew / Collections rows (`search::Item::Tag` has no rating key). A hold there
//! keeps doing nothing, deliberately.
//!
//! **Navigation owns its lifetime, phase and input scope** (restructure phase 10). It was
//! `ui/item_menu.rs` — a `Popover` plus six `static mut`s (`POP`, `TABLE`, `ACTS`, `OPENER`,
//! `ITEM`, `SID`) driven by `Route::ItemMenu { over: MenuHost }` — and every one of those statics
//! is a field of the entry's argument or of the instance now: the ROW the menu is about and the
//! anchor it hangs off travel on [`ItemMenuArg`], the built rows and their actions are the
//! screen's own. The six-variant `MenuHost` is down to the two BITS an action actually reads
//! (`loaded_episode`, `from_home`), which is all it was still deciding once the route it named
//! stopped existing.
//!
//! It still only **reports** the chosen [`Action`]: `AppFx::ItemMenu` carries it to
//! `app::input::apply_item_action`, which does the routing, the server call and the hub refresh.
//! It never mutates playback or PMS state itself.

use std::borrow::Cow;
use std::os::raw::c_int;

use crate::pms::PmsMovie;
use crate::screens::registry::{RepeatGate, PANEL_REPEAT_MS};
use crate::screens::registry::{AppFx, AppLike, ItemMenuArg, ItemMenuKind, ItemMenuReq};
use crate::ui::consts::*;
use crate::ui::frame::Budget;
use crate::ui::icons::Icon;
use crate::ui::machine::{
    Canon, Cx, Edge, Effects, EntryId, FocusKey, Fx, GroupId, Handled, InputKind, Key,
    LogicalState, Machine, NavOp,
};
use crate::ui::screen::{
    Activate, At, AxisMask, Dir, DrawFrame, EdgeRule, ElemKind, Focusable, GroupKind, GroupSpec,
    Hover, Placed, RenderStrategy, Screen, ScreenEvent, Scrim, Seat, Step, Stop,
};
use crate::ui::table::{Row, Section, TableView};
use crate::ui::widgets::{Glass, GlassState, PosterMark};
use crate::ui::{theme, Rect};

/// What the highlighted row does on OK. Every variant carries the identity it needs, captured when
/// the menu opened — a hub refetch can re-order the catalog underneath an open panel, so nothing
/// here is an index.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Action {
    None,
    /// open this leaf's own detail page (an episode's page, a movie's page)
    GoToItem(String),
    /// open the SHOW page with that season selected (`season <= 0` = no season to select)
    GoToShow(String, c_int),
    /// `/:/scrobble` — the ✓ row. **The verb is the ROW's, never the item's state read back**:
    /// there is no bool here to invert, because a part-watched item offers BOTH rows and has no
    /// single "other end" to derive. See [`Action::watch_write`].
    MarkWatched(String),
    /// `/:/unscrobble` — the − row. Twin of [`Action::MarkWatched`].
    MarkUnwatched(String),
    /// play this leaf ignoring its resume point
    PlayFromStart(String),
    /// hide this item from the Continue Watching deck — the server-side
    /// `removeFromContinueWatching`. **Keeps the resume point**: it is a hide, not a reset, so
    /// playing the item again picks up where it left off (see
    /// `plex::Client::remove_from_continue_watching`).
    RemoveFromDeck(String),
}

impl Action {
    /// The view-state write this action performs, if it is one of the two that do — the item-menu
    /// twin of `detail::HeroAction::watch_write`, and the reason this enum carries two variants
    /// rather than one flag.
    ///
    /// The old shape was `MarkWatched(rk, watched)` where the bool was **what the item is NOW**, and
    /// `apply_item_action` inverted it at the press. That works for exactly as long as the menu
    /// emits one row: with this menu's part-watched PAIR both rows would carry the same bool, so one
    /// would invert to its neighbour's write and silently do the opposite of its own label. Now the
    /// glyph the user aimed at IS the verb, and nothing downstream re-reads the item.
    pub(crate) fn watch_write(&self) -> Option<crate::viewstate::Write> {
        match self {
            Action::MarkWatched(_) => Some(crate::viewstate::Write::Watched),
            Action::MarkUnwatched(_) => Some(crate::viewstate::Write::Unwatched),
            _ => None,
        }
    }

    /// The ratingKey this action acts on — empty for [`Action::None`].
    pub(crate) fn rk(&self) -> &str {
        match self {
            Action::GoToItem(rk)
            | Action::GoToShow(rk, _)
            | Action::MarkWatched(rk)
            | Action::MarkUnwatched(rk)
            | Action::PlayFromStart(rk)
            | Action::RemoveFromDeck(rk) => rk,
            Action::None => "",
        }
    }

    fn write(&self, c: &mut Canon) {
        let tag = match self {
            Action::None => 0,
            Action::GoToItem(_) => 1,
            Action::GoToShow(..) => 2,
            Action::MarkWatched(_) => 3,
            Action::MarkUnwatched(_) => 4,
            Action::PlayFromStart(_) => 5,
            Action::RemoveFromDeck(_) => 6,
        };
        c.u32(tag).str(self.rk());
        if let Action::GoToShow(_, season) = self {
            c.u32(*season as u32);
        }
    }
}

/// The panel's fixed width. Wide enough for "Mark as Unwatched" at the table's compact BODY size
/// without eliding, narrow enough that it never covers the neighbouring card it is anchored off.
const PANEL_W: f32 = 460.0;
/// The pinned ~20px corner radius.
const PANEL_RAD: f32 = 20.0;
/// Air between the focused card's drawn edge and the panel — one `space` rung, like every other gap.
const CARD_GAP: f32 = theme::space::MD;
/// Keep-out from the screen edges, so a shelf near the bottom still gets a fully-visible panel.
///
/// Per AXIS, because the overscan frame is: `space::XL` 64 already cleared `MARGIN_Y` 54 vertically,
/// but the horizontal clamp is what a panel opened over the LAST column lands on, and 64 is 32px
/// outside `MARGIN_X`.
const EDGE: f32 = theme::space::XL;
const EDGE_X: f32 = crate::ui::consts::MARGIN_X;
/// Scrim peak alpha. Deliberately LIGHTER than the in-player panels' 0.58: the design's whole point
/// is that the card and the shelf stay legible behind the popover, so this recesses them rather than
/// blanking them.
const SCRIM_A: f32 = 0.34;

pub(crate) const SHAPE: &str =
    "ItemMenu{arg:ItemMenuArg,acts:[Option<Action{tag:u32,rk:str,season:u32}>],sel:i32,\
     table:TableViewMotion}";

/// Is `m` an item the menu has anything to offer? A leaf or a show/season — i.e. everything the
/// home shelves carry. Kept as a predicate so the caller can decline to present an empty panel.
pub(crate) fn has_actions(m: &PmsMovie) -> bool {
    !m.rk.is_empty()
}

/// Why [`build`], [`build_episode`] and [`build_season`] all end in a length assertion.
///
/// `acts` is the index→action map — the pressed row is resolved by its index into it — so a
/// `sec.row` added without its `acts.push` cannot panic. It shifts every action below it by one,
/// and the press performs its neighbour's. This is the menu where that is easiest to do, because
/// it is the only one of the four whose row set is CONDITIONAL: three item kinds, an optional
/// `Go to Show`, an optional deck row, and a state group shared with two other entry points.
const ACTS_PARALLEL: &str = "acts must stay one-to-one with the rows: a row without its action \
                             shifts every action below it, and the press performs its neighbour's";

/// The rows, and the action each one commits. Order is the pinned design's:
/// navigation (`Go to Episode` · `Go to Show`) — separator — state (the watch row or ROWS ·
/// `Play from Start`), adapted per item kind (`PmsMovie::kind`: 0 movie / 1 show / 2 season /
/// 3 episode). The state group is one row or two off [`state_rows`], so this list has no fixed
/// length and every index into it is resolved through `acts` — see [`ACTS_PARALLEL`].
fn build(m: &PmsMovie, from_deck: bool) -> (Section, Vec<Option<Action>>) {
    let mut sec = Section::new(""); // no header: the card behind the panel IS the title
    let mut acts: Vec<Option<Action>> = Vec::new();
    let leaf = m.kind == 0 || m.kind == 3;

    // ---- navigation: this tile's own page, then the show it belongs to ----
    // Built as a list first so a row whose TARGET is missing is simply absent: a hub row can arrive
    // without a grandparentRatingKey, and a "Go to Show" that resolves to an empty rk would fire a
    // blocking fetch for nothing and land the user on a blank page.
    let mut nav: Vec<(&str, Icon, Action)> = Vec::new();
    match m.kind {
        3 => {
            nav.push((
                "Go to Episode",
                Icon::Episode,
                Action::GoToItem(m.rk.clone()),
            ));
            if !m.show_rk.is_empty() {
                nav.push((
                    "Go to Show",
                    Icon::Show,
                    Action::GoToShow(m.show_rk.clone(), m.season_index),
                ));
            }
        }
        // a season has no page of its own — it IS the show page with that season selected, so one
        // row covers it; a show's own page is likewise the only navigation it has
        2 if !m.show_rk.is_empty() => {
            nav.push((
                "Go to Season",
                Icon::Show,
                Action::GoToShow(m.show_rk.clone(), m.season_index),
            ));
        }
        2 => {}
        1 => nav.push(("Go to Show", Icon::Show, Action::GoToShow(m.rk.clone(), 0))),
        _ => nav.push(("Go to Movie", Icon::Episode, Action::GoToItem(m.rk.clone()))),
    }
    let had_nav = !nav.is_empty();
    for (label, icon, act) in nav {
        sec = sec.row(Row::new(label).licon(icon));
        acts.push(Some(act));
    }

    // ---- the divider the design groups on (only when there IS a group above it) ----
    if had_nav {
        sec = sec.row(Row::separator());
        acts.push(None);
    }

    // ---- state ----
    // The row set comes from the shared `widgets::row_watch_state`, over the catalog row this menu
    // captured when it was presented — so it is exact for a leaf (`viewCount`/`viewOffset` off the
    // same container the shelf was built from) AND for a container, whose three states are already
    // on the row as the `unwatched`/`watched` PAIR (`pms::parse_item`: a show is "neither" exactly
    // while some of its leaves are viewed and some are not).
    let mut sec = state_rows(
        sec,
        &mut acts,
        &m.rk,
        crate::ui::widgets::row_watch_state(m),
        leaf,
    );

    // ---- and, only on a Continue Watching card, the row that takes it off the deck ----
    // Gated on the SHELF, not on the item: the action is meaningless anywhere else (nothing to
    // remove it from), and offering it on a Recently Added card would be a row that silently did
    // nothing. `pms::hub_is_continue` is what the caller passes in.
    //
    // Last, after the watched toggle, because it is the only row here that removes something from
    // view — the destructive-ish end of the group, where a mis-hit is least likely.
    if from_deck {
        // "Remove from Deck", not "Remove from Continue Watching": the longer label elides inside
        // `PANEL_W` at the table's compact BODY size, and widening the panel would push it across
        // the neighbouring card it is anchored beside. It is also the more accurate of the two —
        // the server action hides the item from the DECK and leaves its resume point intact, so
        // "remove from continue watching" over-promises a reset it does not perform.
        sec = sec.row(Row::new("Remove from Deck").licon(Icon::Close));
        acts.push(Some(Action::RemoveFromDeck(m.rk.clone())));
    }
    debug_assert_eq!(acts.len(), sec.rows.len(), "{ACTS_PARALLEL}");
    (sec, acts)
}

/// The state group every menu ends with: the watch-state row (or ROWS), then (for a leaf) Play from
/// Start. ONE builder, because these are the rows the menu exists for and they must read identically
/// wherever it is opened.
///
/// **The tail is one row or TWO**, off the same three-state vocabulary ([`PosterMark`]) the detail
/// hero resolves (`detail::hero_watch_state`) so the two surfaces cannot describe one item
/// differently: each row states the OUTCOME its press produces, so a finished item offers only *Mark
/// as Unwatched* and one never started offers only *Mark as Watched*. A PART-WATCHED item is in
/// neither state, and a LIST can name both destinations at once, so it gets both rows — the ✓ then
/// the −, the same order the two ends of the range read in.
///
/// **The detail hero answers the same question with ONE control, and the difference is deliberate.**
/// Its watched control is a TOGGLE wearing the face of the write it would perform
/// (`detail::hero_ctls`), so a part-watched item reads ✓ there — it is not watched, and the other
/// end is one press away rather than one control away. A menu has no state of its own and is free
/// to list both; a toggle is one thing and must read as the thing it currently is. Same vocabulary,
/// two presentations of it — so do NOT "fix" this row set to match the hero's.
///
/// **Both glyphs are FILLED discs**: [`Icon::CheckCircleFill`] for the row that marks watched,
/// [`Icon::MinusCircleFill`] for the one that takes it away. A ticked circle beside "Mark as
/// Unwatched" states the outcome backwards, which is the one thing a destructive-ish row must not do
/// — and filled is what separates an ACTION from a STATE.
fn state_rows(
    sec: Section,
    acts: &mut Vec<Option<Action>>,
    rk: &str,
    mark: PosterMark,
    leaf: bool,
) -> Section {
    let mut sec = sec;
    if mark != PosterMark::Watched {
        sec = sec.row(Row::new(crate::ui::widgets::MARK_WATCHED_VERB).licon(Icon::CheckCircleFill));
        acts.push(Some(Action::MarkWatched(rk.to_string())));
    }
    if mark != PosterMark::None {
        sec =
            sec.row(Row::new(crate::ui::widgets::MARK_UNWATCHED_VERB).licon(Icon::MinusCircleFill));
        acts.push(Some(Action::MarkUnwatched(rk.to_string())));
    }
    if leaf {
        sec = sec.row(Row::new(crate::ui::widgets::PLAY_FROM_START_VERB).licon(Icon::PlayStart));
        acts.push(Some(Action::PlayFromStart(rk.to_string())));
    }
    sec
}

/// The DETAIL page's episode filmstrip: the same panel and the same state rows, with **no
/// navigation group**.
///
/// Both navigation rows the shelf menu offers an episode are dead ends from here. "Go to Show" is
/// the page you are standing on. "Go to Episode" is the judgement call, and it goes the same way:
/// the episode's own page carries nothing the tile the popover is anchored to is not already
/// showing — its still, title, full summary and air date are all right there. A row that navigates
/// away from a page to show less of what that page already shows is not an action.
///
/// `mark` is exact here — `detail::focused_episode` resolves it through the same `ep_state` that
/// draws the still's own state line, so the tile and the menu opened on it cannot describe one
/// episode two ways — and with no nav group there is no separator either ([`build`]'s rule: the
/// divider only exists when there is a group above it). An episode is a LEAF, so all three states
/// are reachable and a part-watched one gets the pair, exactly as a shelf card does.
fn build_episode(rk: &str, mark: PosterMark) -> (Section, Vec<Option<Action>>) {
    let mut acts: Vec<Option<Action>> = Vec::new();
    let sec = state_rows(Section::new(""), &mut acts, rk, mark, true);
    debug_assert_eq!(acts.len(), sec.rows.len(), "{ACTS_PARALLEL}");
    (sec, acts)
}

/// The DETAIL page's season strip: [`build_episode`]'s row set with the one difference a season
/// makes — **no "Play from Start"**.
///
/// That row means "play THIS item from 00:00", and a season is not a thing you play: the press
/// would have to pick a leaf, which is a second decision the row does not state. Starting a season
/// from its beginning is what the first episode's own tile does, exactly and visibly. So
/// `leaf: false` — the same flag [`build`] passes for a show, for the same reason.
fn build_season(rk: &str, mark: PosterMark) -> (Section, Vec<Option<Action>>) {
    let mut acts: Vec<Option<Action>> = Vec::new();
    let sec = state_rows(Section::new(""), &mut acts, rk, mark, false);
    debug_assert_eq!(acts.len(), sec.rows.len(), "{ACTS_PARALLEL}");
    (sec, acts)
}

/// Beside the card, never over it: to its RIGHT by default, flipped to its LEFT when that would run
/// past the screen's keep-out. Vertically it hangs off the card's top edge, pulled back inside the
/// safe band so a bottom shelf still gets a whole panel. Pure (anchor + measured height in, rect
/// out), which is what makes the placement rules host-testable.
fn panel_at(a: Rect, content_h: f32) -> Rect {
    let h = content_h.clamp(120.0, SCR_H - 2.0 * EDGE); // same floor the profile popover uses
    let right = a.x + a.w + CARD_GAP;
    let x = if right + PANEL_W <= SCR_W - EDGE_X {
        right
    } else {
        a.x - CARD_GAP - PANEL_W
    };
    let x = x.clamp(EDGE_X, (SCR_W - EDGE_X - PANEL_W).max(EDGE_X));
    let y = a.y.clamp(EDGE, (SCR_H - EDGE - h).max(EDGE));
    Rect::new(x, y, PANEL_W, h)
}

/// The anchor a menu with no rect to hang off falls back to — a centred card. The headless trigger
/// and a host with nothing focused both land here.
///
/// Resolved by the PRESENTER, once, rather than every frame inside [`ItemMenuScreen::frame`], so
/// the panel cannot drift if a host's rect stops resolving while the menu is up — the same
/// argument the legacy `present` made for resolving it before it stored `OPENER`.
pub(crate) fn fallback_anchor() -> Rect {
    Rect::new(
        (SCR_W - CARD_W) * 0.5 - PANEL_W * 0.5,
        (SCR_H - CARD_H) * 0.5,
        CARD_W,
        CARD_H,
    )
}

pub(crate) struct ItemMenuScreen {
    entry: EntryId,
    arg: ItemMenuArg,
    /// The chosen action per global row index (`None` for the separator, which is unfocusable
    /// anyway). Parallel to the table's rows because the row SET varies by item kind — see
    /// [`ACTS_PARALLEL`].
    acts: Vec<Option<Action>>,
    table: TableView,
    glass: GlassState,
    /// The cadence a HELD direction walks this panel's list at. `app/run.rs`'s client-side repeat
    /// timer (`App::held_key`) did this at 110 ms for exactly this menu and, before phase 9, for
    /// the player's four panels; the dispatcher delivers the hardware's ~50 ms `Edge::Repeat`
    /// instead, so the surface applies the cadence itself — the same [`PANEL_REPEAT_MS`] those
    /// four take, reused rather than copied.
    repeat: RepeatGate,
    built: bool,
}

impl ItemMenuScreen {
    pub(crate) fn new(entry: EntryId, arg: ItemMenuArg) -> Self {
        Self {
            entry,
            arg,
            acts: Vec::new(),
            table: TableView::new(),
            glass: GlassState::new(),
            repeat: RepeatGate::IDLE,
            built: false,
        }
    }

    /// The rows, built ONCE at `Mount`. A hub refetch can re-order the catalog underneath an open
    /// panel, so nothing here is rebuilt while the menu is up — which is also why every [`Action`]
    /// carries the identity it needs rather than an index.
    fn build_rows(&mut self) {
        if self.built {
            return;
        }
        self.built = true;
        let (sec, acts) = match &self.arg.kind {
            ItemMenuKind::Card { row, from_deck } => build(row, *from_deck),
            ItemMenuKind::Episode { mark } => build_episode(&self.arg.rk, *mark),
            ItemMenuKind::Season { mark } => build_season(&self.arg.rk, *mark),
        };
        self.acts = acts;
        // a short list of one-line actions — BODY labels, not menu-size HEADLINE
        self.table.compact = true;
        self.table.set_sections(vec![sec], 0, false);
    }

    fn frame(&self) -> Rect {
        let [x, y, w, h] = self.arg.anchor.map(f32::from_bits);
        panel_at(Rect::new(x, y, w, h), self.table.measured_height())
    }

    /// The rows a focus stop exists for — every index whose action is `Some`. The separator is
    /// unfocusable, exactly as `TableView::move_sel` used to skip it.
    fn focusable(&self) -> impl Iterator<Item = u32> + '_ {
        self.acts
            .iter()
            .enumerate()
            .filter(|(_, a)| a.is_some())
            .map(|(i, _)| i as u32)
    }

    fn step_focus(&self, from: u32, delta: i32) -> Option<u32> {
        let rows: Vec<u32> = self.focusable().collect();
        let at = rows.iter().position(|r| *r == from)?;
        let next = if delta < 0 { at.checked_sub(1)? } else { at + 1 };
        rows.get(next).copied()
    }

    /// Commit the focused row: report the action, then dismiss. The legacy `on_ok` closed first and
    /// returned the action second; the two are one drain here, and the order is what keeps the
    /// loop's dispatch reading a REQUEST rather than a static a frame after the close.
    fn activate<H: AppLike>(&mut self, elem: u32, fx: &mut Effects<'_, H>) {
        let act = self
            .acts
            .get(elem as usize)
            .cloned()
            .flatten()
            .unwrap_or(Action::None);
        // Every arm of the dispatch turns an rk into a blocking fetch, a scrobble or a play; an
        // empty one would fetch nothing and land on a blank page. `build` already refuses to offer
        // such a row — this is the belt to that braces, since the rows are data-driven off hub
        // rows.
        if !matches!(act, Action::None) && !act.rk().is_empty() {
            fx.push(Fx::App(AppFx::ItemMenu(ItemMenuReq {
                act,
                sid: self.arg.sid,
                item: match &self.arg.kind {
                    ItemMenuKind::Card { row, .. } => Some((**row).clone()),
                    _ => None,
                },
                loaded_episode: self.arg.loaded_episode,
                from_home: self.arg.from_home,
            })));
        }
        fx.push(Fx::Nav(NavOp::Dismiss(self.entry)));
    }

    /// The highlighted row, for the focus probe — a READ of the cursor the engine moves.
    pub(crate) fn sel(&self) -> i32 {
        self.table.sel
    }

    /// The server every action from this menu names. Captured rather than looked up: on a Continue
    /// Watching shelf merged across servers, resolving a bare rk against the CURRENT server is the
    /// reported bug itself (hold a friend's episode → Play from Start → our film with the same key
    /// plays, under the friend's title).
    pub(crate) fn sid(&self) -> crate::plex::ServerId {
        self.arg.sid
    }

    /// The page this menu hangs off, and the element on it the panel is anchored beside and lifted
    /// back out of the dim. ONE owner: the entry's own argument, captured when the menu was
    /// presented. It was `Bridge::menu_opener`, a second copy of the same pair on the side.
    pub(crate) fn opener(&self) -> (EntryId, Option<FocusKey<u32>>) {
        (self.arg.host, self.arg.focus)
    }
}

impl<H: AppLike> Machine<H> for ItemMenuScreen {
    type Ev = ScreenEvent<H>;
    fn step(&mut self, ev: &Self::Ev, cx: &Cx<'_, H>, fx: &mut Effects<'_, H>) -> Handled {
        match ev {
            ScreenEvent::Mount => self.build_rows(),
            ScreenEvent::Tick(tick) => {
                self.table.sel = cx
                    .focus
                    .current
                    .filter(|key| key.entry == self.entry)
                    .map(|key| key.elem as i32)
                    .unwrap_or(self.table.sel);
                self.table.update(tick.dt(), self.frame().h);
            }
            ScreenEvent::FocusMoved { to, .. } => self.table.sel = to.elem as i32,
            ScreenEvent::Activate(elem) => self.activate(*elem, fx),
            ScreenEvent::PressCommit(_) => {
                if let Some(key) = cx.focus.current {
                    self.activate(key.elem, fx);
                }
            }
            ScreenEvent::Input(input) => {
                let InputKind::Key { key, edge, .. } = input.kind else {
                    return Handled::No;
                };
                if key == Key::Back && edge == Edge::Down {
                    fx.push(Fx::Nav(NavOp::Dismiss(self.entry)));
                    return Handled::Yes;
                }
                // **The hold-to-move cadence, applied HERE and nowhere else.** It was
                // `app/run.rs`'s per-frame `App::held_key` timer, whose LAST consumer this menu
                // was; the dispatcher hands the surface the hardware's own `Edge::Repeat` at
                // ~50 ms, which walks a five-row list faster than anybody can read it. A FRESH
                // press is never swallowed by the cadence of the press before it — it is acted on
                // unconditionally by falling through to `Handled::No`, and `rearm` records it as
                // the step it is.
                if matches!(key, Key::Up | Key::Down) {
                    match edge {
                        Edge::Down => self.repeat.rearm(input.at.ms),
                        Edge::Repeat if !self.repeat.ready_every(input.at.ms, PANEL_REPEAT_MS) => {
                            return Handled::Yes
                        }
                        _ => {}
                    }
                }
            }
            _ => {}
        }
        Handled::No
    }
}

impl<H: AppLike> Focusable<H> for ItemMenuScreen {
    fn groups(&self, _: &Cx<'_, H>, out: &mut Vec<GroupSpec>) {
        out.push(GroupSpec {
            id: GroupId(0),
            kind: GroupKind::Column,
            seat: Seat::Remembered,
            reachable: AxisMask::BOTH,
            edge: [EdgeRule::Stop; 4],
            extent: self.frame(),
            len: self.focusable().count(),
            elem: ElemKind::Bare,
        });
    }
    fn group_of(&self, elem: &u32, _: &Cx<'_, H>) -> Option<GroupId> {
        self.acts
            .get(*elem as usize)
            .is_some_and(|a| a.is_some())
            .then_some(GroupId(0))
    }
    fn neighbour(&self, key: FocusKey<u32>, dir: Dir, _: &Cx<'_, H>) -> Step<u32> {
        let delta = match dir {
            Dir::Up => -1,
            Dir::Down => 1,
            _ => return Step::Edge,
        };
        match self.step_focus(key.elem, delta) {
            Some(elem) => Step::Move(FocusKey {
                entry: self.entry,
                elem,
            }),
            None => Step::Edge,
        }
    }
    fn place(&self, elem: &u32, cx: &Cx<'_, H>, _: At) -> Option<Placed> {
        <Self as Focusable<H>>::group_of(self, elem, cx)?;
        let rect = self.table.row_frame(self.frame(), *elem as i32)?;
        Some(Placed {
            rect,
            rest_rect: rect,
            clip: self.frame(),
            index: Some(*elem),
        })
    }
    fn reconcile(&self, want: FocusKey<u32>, cx: &Cx<'_, H>) -> FocusKey<u32> {
        if <Self as Focusable<H>>::group_of(self, &want.elem, cx).is_some() {
            want
        } else {
            FocusKey {
                entry: self.entry,
                elem: self.focusable().next().unwrap_or(0),
            }
        }
    }
    fn seat(&self, _: GroupId, _: Placed, _: &Cx<'_, H>) -> FocusKey<u32> {
        let sel = u32::try_from(self.table.sel).unwrap_or(0);
        FocusKey {
            entry: self.entry,
            elem: if self.acts.get(sel as usize).is_some_and(|a| a.is_some()) {
                sel
            } else {
                self.focusable().next().unwrap_or(0)
            },
        }
    }
}

impl<H: AppLike> Screen<H> for ItemMenuScreen {
    fn as_any(&self) -> Option<&dyn std::any::Any> {
        Some(self)
    }
    fn name(&self) -> &'static str {
        crate::screens::registry::word::ITEM_MENU
    }
    fn state(&self) -> &dyn LogicalState {
        self
    }
    fn crumb(&self, _: &Cx<'_, H>) -> Option<Cow<'_, str>> {
        None
    }
    /// The modal dim. **The TILE is lifted back out of it, and the lift is the HOST PAGE's to
    /// draw** — only the screen that drew an element knows where it landed, and a `Scrim::lift` is
    /// a bare `fn()` with nothing to borrow a page through. So this asks for the dim alone, and
    /// `app/bridge.rs`'s `redraw_opener` — the ONE owner, reading this surface's own
    /// [`ItemMenuScreen::opener`] rather than a copy kept on the side — repaints the focused
    /// element immediately after the container's page pass, above the dim it just laid down.
    ///
    /// That tile is the panel's whole subject: the design's stated point is that "the card and the
    /// rest of the shelf stay where they are, visible behind it", which a scrim over the card
    /// itself quietly contradicts. The un-dimmed copy is also what the host snapshot holds, so the
    /// panel's own glass never frosts a dimmed picture of the very card it is about.
    fn scrim(&self) -> Scrim {
        Scrim::dim(SCRIM_A)
    }
    fn prepare(&mut self, _: &mut Budget, _: &Cx<'_, H>) {
        Glass::CACHED.prepare(&mut self.glass, false);
    }
    fn draw(&mut self, f: &mut DrawFrame<'_, '_, H>) {
        let p = f.painter.alpha(f.page_alpha);
        let r = self.frame();
        Glass::CACHED.panel(p, r, 0.0, PANEL_RAD);
        self.table.draw(p, r, f.measure);
        for elem in self.focusable().collect::<Vec<_>>() {
            if let Some(placed) = <Self as Focusable<H>>::place(self, &elem, f.cx, At::Drawn) {
                f.stop(
                    p,
                    Stop {
                        key: FocusKey {
                            entry: self.entry,
                            elem,
                        },
                        rect: placed.rect,
                        rest_rect: placed.rest_rect,
                        clip: placed.clip,
                        hover: Hover::Focus,
                        activate: Activate::Immediate,
                    },
                );
            }
        }
    }
    fn render(&self) -> RenderStrategy {
        RenderStrategy::Page
    }
}

impl LogicalState for ItemMenuScreen {
    fn write(&self, c: &mut Canon) {
        self.arg.write(c);
        c.seq(self.acts.len());
        for a in &self.acts {
            c.option(a.as_ref(), |c, a| a.write(c));
        }
        c.u32(self.table.sel as u32);
        self.table.write_motion(c);
    }
    fn probe(&self, out: &mut String) {
        out.push_str("item_menu");
    }
}

// ---------------------------------------------------------------------------------------
/// The row sets, the focus walk, the placement and the capture — all fourteen moved by NAME from
/// `ui/item_menu.rs` (restructure phase 10), plus the one this phase adds
/// (`the_item_menu_hold_repeats_through_the_surface_not_the_loop`).
///
/// **Two of them changed SUBJECT without changing name, and both are the phase working.** The
/// focus walk and the separator settle were stated on a live `TableView` (`move_sel`, and
/// `set_sections`' settle) because that is where the cursor lived; the walk is the focus ENGINE's
/// now, so they are stated on the screen's own `Focusable` answers — which is where the
/// separator's unfocusability actually lives (`acts[i].is_none()`) and what the engine asks. They
/// no longer take `testlock::serial()` either: nothing here drives a live `Spring`, so there is no
/// process-global dirty flag to contend for.
#[cfg(test)]
mod tests {
    use super::*;

    /// A catalog row of `kind` in a given watch state, with the flags a real `pms::parse_item`
    /// would set — which is the part that matters here, since the menu's row set is derived from
    /// `unwatched`/`watched`/`resume_ms` together rather than from one of them.
    ///
    /// A LEAF in the middle is `viewCount == 0` with a live `viewOffset`; a CONTAINER in the middle
    /// is "some leaves viewed, not all", i.e. NEITHER flag — the state a bool could not hold and the
    /// reason this fixture takes a [`PosterMark`].
    fn item(kind: c_int, mark: PosterMark) -> PmsMovie {
        let leaf = kind == 0 || kind == 3;
        let mut m = PmsMovie::default();
        m.rk = "42".to_string();
        m.kind = kind;
        m.show_rk = "7".to_string();
        m.season_index = 3;
        m.dur_ns = 100 * 60 * 1000 * 1_000_000;
        match mark {
            PosterMark::None => m.unwatched = true,
            PosterMark::Watched => m.watched = true,
            PosterMark::InProgress if leaf => {
                m.unwatched = true;
                m.resume_ms = 30 * 60 * 1000;
            }
            PosterMark::InProgress => {} // a container: neither end
        }
        assert_eq!(
            crate::ui::widgets::row_watch_state(&m),
            mark,
            "the fixture must build the state it names"
        );
        m
    }
    fn labels(sec: &Section) -> Vec<String> {
        sec.rows
            .iter()
            .map(|r| {
                if r.sep {
                    "—".to_string()
                } else {
                    r.label.clone()
                }
            })
            .collect()
    }
    /// The write each row commits, paired with its label — the projection every row-set assertion
    /// below is really about, since a label and its verb living in two places is the bug this
    /// module's `Action` split closed.
    fn verbs(
        sec: &Section,
        acts: &[Option<Action>],
    ) -> Vec<(String, Option<crate::viewstate::Write>)> {
        labels(sec)
            .into_iter()
            .zip(acts.iter())
            .map(|(l, a)| (l, a.as_ref().and_then(|a| a.watch_write())))
            .collect()
    }

    #[test]
    fn an_episode_offers_the_pinned_row_set_in_order() {
        let (sec, acts) = build(&item(3, PosterMark::None), false);
        assert_eq!(
            labels(&sec),
            [
                "Go to Episode",
                "Go to Show",
                "—",
                "Mark as Watched",
                "Play from Start"
            ]
        );
        // the separator carries no action, every other row does
        assert!(acts[2].is_none());
        assert!(acts
            .iter()
            .enumerate()
            .filter(|(i, _)| *i != 2)
            .all(|(_, a)| a.is_some()));
        // "Go to Show" targets the SHOW rk + the episode's season, not the episode
        match acts[1].as_ref().unwrap() {
            Action::GoToShow(rk, season) => assert_eq!((rk.as_str(), *season), ("7", 3)),
            _ => panic!("row 1 must be Go to Show"),
        }
    }

    #[test]
    fn a_watched_leaf_offers_only_the_way_back() {
        let (sec, acts) = build(&item(0, PosterMark::Watched), false); // movie, viewCount >= 1
        assert_eq!(
            labels(&sec),
            ["Go to Movie", "—", "Mark as Unwatched", "Play from Start"]
        );
        assert_eq!(
            acts[2].as_ref().unwrap().watch_write(),
            Some(crate::viewstate::Write::Unwatched),
            "a finished item has one end left to be sent to"
        );
        // a movie has no show, so no second navigation row
        assert!(!labels(&sec).contains(&"Go to Show".to_string()));
    }

    /// A show or season never offers *Play from Start* — there is no single part to start — and
    /// its watch state is the flag PAIR, not one flag: `!unwatched` alone means "some leaf was
    /// watched", which is the middle of the range and not the end of it.
    #[test]
    fn a_show_has_no_play_from_start_and_gets_the_pair_when_it_is_mid_run() {
        let (sec, acts) = build(&item(1, PosterMark::InProgress), false);
        assert_eq!(
            labels(&sec),
            ["Go to Show", "—", "Mark as Watched", "Mark as Unwatched"]
        );
        assert_eq!(
            acts[2].as_ref().unwrap().watch_write(),
            Some(crate::viewstate::Write::Watched)
        );
        assert_eq!(
            acts[3].as_ref().unwrap().watch_write(),
            Some(crate::viewstate::Write::Unwatched)
        );

        // a show whose every leaf is seen is DONE, and offering to mark it watched again was the
        // old row set's other wrong answer (it read `!unwatched`, which cannot tell the two apart)
        let (sec, _) = build(&item(1, PosterMark::Watched), false);
        assert_eq!(labels(&sec), ["Go to Show", "—", "Mark as Unwatched"]);
        // …and one nobody has opened offers only the way forward
        let (sec, _) = build(&item(1, PosterMark::None), false);
        assert_eq!(labels(&sec), ["Go to Show", "—", "Mark as Watched"]);
    }

    #[test]
    fn a_row_whose_target_is_missing_is_not_offered_at_all() {
        // an episode hub row that arrived without a grandparentRatingKey: no "Go to Show" — the row
        // would resolve to an empty rk, i.e. a blocking fetch for nothing and a blank page
        let mut m = item(3, PosterMark::None);
        m.show_rk.clear();
        let (sec, acts) = build(&m, false);
        assert_eq!(
            labels(&sec),
            ["Go to Episode", "—", "Mark as Watched", "Play from Start"]
        );
        assert!(acts
            .iter()
            .flatten()
            .all(|a| !matches!(a, Action::GoToShow(..))));
        // and a SEASON with no parent has no navigation at all — so no leading separator either,
        // which would otherwise open the menu with a rule above its first row
        let mut s = item(2, PosterMark::None);
        s.show_rk.clear();
        let (sec, _) = build(&s, false);
        assert_eq!(labels(&sec), ["Mark as Watched"]);
    }

    /// **Remove from Continue Watching** is gated on the SHELF, not on the item: only a card that
    /// came from the deck has a deck to leave. Offered anywhere else it would be a row that appeared
    /// to work and silently changed nothing, since the server-side action only affects that hub.
    ///
    /// Pinned last in the group on purpose — it is the one row here that takes something out of
    /// view, so it sits where a mis-press is least likely, and the assertion below is what keeps a
    /// later row from being appended after it.
    #[test]
    fn the_remove_from_deck_row_exists_only_on_a_continue_watching_card() {
        // same card, both shelves — the ONLY difference is where it was focused
        let (off_deck, acts_off) = build(&item(3, PosterMark::None), false);
        assert_eq!(
            labels(&off_deck),
            [
                "Go to Episode",
                "Go to Show",
                "—",
                "Mark as Watched",
                "Play from Start"
            ]
        );
        assert!(
            !acts_off
                .iter()
                .flatten()
                .any(|a| matches!(a, Action::RemoveFromDeck(_))),
            "a card off the deck must not offer to remove it from one"
        );

        let (on_deck, acts_on) = build(&item(3, PosterMark::None), true);
        assert_eq!(
            labels(&on_deck),
            [
                "Go to Episode",
                "Go to Show",
                "—",
                "Mark as Watched",
                "Play from Start",
                "Remove from Deck"
            ],
            "…and on the deck it is the LAST row, after the watched toggle"
        );
        match acts_on
            .last()
            .expect("a trailing action")
            .as_ref()
            .expect("not a separator")
        {
            Action::RemoveFromDeck(rk) => {
                assert_eq!(rk, "42", "…carrying the card's own ratingKey")
            }
            _ => panic!("the last row must be the deck removal"),
        }
        // it is an ADDITION to the group, not a replacement — the watch-state row is still there
        assert!(acts_on
            .iter()
            .flatten()
            .any(|a| matches!(a, Action::MarkWatched(_))));
    }

    /// The detail page's filmstrip menu: the state rows, no navigation group, and therefore no
    /// leading separator. It shares `state_rows` with the shelf menu, so the row the owner asked
    /// for cannot say one thing on Home and another on the episode page — asserted by comparing the
    /// two builders' tails rather than by re-listing the labels here.
    #[test]
    fn the_filmstrip_menu_is_the_state_group_alone() {
        let (sec, acts) = build_episode("42", PosterMark::None);
        assert_eq!(labels(&sec), ["Mark as Watched", "Play from Start"]);
        assert!(
            acts.iter().all(|a| a.is_some()),
            "no separator, so every row acts"
        );
        // the shelf menu's episode rows END with exactly these two, in this order
        let (shelf, _) = build(&item(3, PosterMark::None), false);
        assert_eq!(labels(&sec), labels(&shelf)[shelf.rows.len() - 2..]);

        // a watched episode gets the way back instead, carrying its OWN rk (the menu captured its
        // target when it was presented — nothing here may resolve through a live focus index)
        let (sec, acts) = build_episode("77", PosterMark::Watched);
        assert_eq!(labels(&sec), ["Mark as Unwatched", "Play from Start"]);
        match acts[0].as_ref().unwrap() {
            Action::MarkUnwatched(rk) => assert_eq!(rk, "77"),
            _ => panic!("row 0 must be the unwatched row"),
        }
        match acts[1].as_ref().unwrap() {
            Action::PlayFromStart(rk) => assert_eq!(rk, "77"),
            _ => panic!("row 1 must be Play from Start"),
        }
        // and nothing navigates: both rows the shelf menu offers an episode are dead ends from the
        // page that episode already belongs to
        assert!(!acts
            .iter()
            .flatten()
            .any(|a| matches!(a, Action::GoToShow(..) | Action::GoToItem(_))));
    }

    /// **The owner-reported gap, at both entry points.** An item in the MIDDLE is at neither end of
    /// the watch range, so no single toggle can express both destinations — it gets both rows, ✓
    /// then −, the order the two ends of the range read in.
    ///
    /// The pair is asserted alongside its two neighbours in the same test on purpose: the property
    /// is not "there are two rows", it is that the row SET tracks the state, and a fixture that only
    /// ever built the middle would pass with an unconditional pair.
    #[test]
    fn a_part_watched_item_offers_both_ends_of_the_range() {
        // the shelf card (an episode with a live resume point)
        let tail = |mark| {
            let (sec, _) = build(&item(3, mark), false);
            labels(&sec).split_off(3) // past "Go to Episode", "Go to Show", the separator
        };
        assert_eq!(
            tail(PosterMark::None),
            ["Mark as Watched", "Play from Start"]
        );
        assert_eq!(
            tail(PosterMark::InProgress),
            ["Mark as Watched", "Mark as Unwatched", "Play from Start"]
        );
        assert_eq!(
            tail(PosterMark::Watched),
            ["Mark as Unwatched", "Play from Start"]
        );

        // …and the detail page's filmstrip, off the same builder, so the two cannot drift
        let strip = |mark| {
            let (sec, _) = build_episode("42", mark);
            labels(&sec)
        };
        assert_eq!(
            strip(PosterMark::None),
            ["Mark as Watched", "Play from Start"]
        );
        assert_eq!(
            strip(PosterMark::InProgress),
            ["Mark as Watched", "Mark as Unwatched", "Play from Start"]
        );
        assert_eq!(
            strip(PosterMark::Watched),
            ["Mark as Unwatched", "Play from Start"]
        );
    }

    /// **A RELATED tile gets the same three-state row set as every other card**, built from a row
    /// that came off the wire rather than from this module's hand-made fixture.
    ///
    /// The Related shelf was the last card surface with no context menu, excluded on the stated
    /// grounds that its tiles carried no `(ratingKey, watched)` pair — true of the old struct, never
    /// of the response. So this half runs `/related`'s real JSON through the same `pms::parse_item`
    /// the shelf now uses, into `build`, and out as labels, rather than through this module's
    /// hand-made [`item`] fixture: the fixture sets the three flags directly, so it can only prove
    /// that `build` reads them, never that the WIRE fills them in.
    ///
    /// It is deliberately only that half. The other coupling — that `fetch_related` still hands the
    /// shelf a fully parsed row instead of going back to copying three fields — cannot be seen from
    /// here, because this test calls `parse_item` itself. That one is
    /// `metadata::tests::related_rows_carry_the_watch_state_the_wire_already_had`; the two are a
    /// pair and neither is sufficient alone.
    #[test]
    fn a_related_tile_off_the_wire_gets_the_row_set_its_state_earns() {
        let row = |json: &str| {
            let body = format!(r#"{{"MediaContainer":{{"Hub":[{{"Metadata":[{json}]}}]}}}}"#);
            let mc = serde_json::from_str::<crate::plex::Envelope>(&body)
                .expect("parses")
                .media_container;
            crate::pms::parse_item(&mc.hub[0].metadata[0], crate::plex::ServerId::UNSET)
        };
        let set = |json: &str| labels(&build(&row(json), false).0);

        // a MOVIE, in each of the three states — one navigation row, then the state group
        let movie = |extra: &str| {
            format!(r#"{{"ratingKey":"11","type":"movie","duration":"7020000"{extra}}}"#)
        };
        assert_eq!(
            set(&movie("")),
            ["Go to Movie", "—", "Mark as Watched", "Play from Start"],
            "never started: only the way forward"
        );
        assert_eq!(
            set(&movie(r#","viewOffset":"3510000""#)),
            [
                "Go to Movie",
                "—",
                "Mark as Watched",
                "Mark as Unwatched",
                "Play from Start"
            ],
            "part-watched: both ends are reachable and both are true"
        );
        assert_eq!(
            set(&movie(r#","viewCount":2"#)),
            ["Go to Movie", "—", "Mark as Unwatched", "Play from Start"],
            "finished: only the way back"
        );

        // a SHOW — a container, so no Play from Start, and its middle state is the leaf-count one
        let show =
            |extra: &str| format!(r#"{{"ratingKey":"14","type":"show","leafCount":10{extra}}}"#);
        assert_eq!(
            set(&show("")),
            ["Go to Show", "—", "Mark as Watched"],
            "no leaf viewed"
        );
        assert_eq!(
            set(&show(r#","viewedLeafCount":3"#)),
            ["Go to Show", "—", "Mark as Watched", "Mark as Unwatched"],
            "3 of 10: neither watched nor unwatched, so BOTH verbs — the case `viewCount > 0` misses"
        );
        assert_eq!(
            set(&show(r#","viewedLeafCount":10"#)),
            ["Go to Show", "—", "Mark as Unwatched"],
            "all 10"
        );

        // and no Related row offers the deck action: that shelf is not the Continue Watching deck
        let (sec, acts) = build(&row(&movie("")), false);
        assert!(!labels(&sec).iter().any(|l| l == "Remove from Deck"));
        assert_eq!(acts.len(), sec.rows.len(), "{ACTS_PARALLEL}");
    }

    /// **A row performs the verb its own label names**, in every state and at both entry points —
    /// which is the assertion the old shape could not make at all: the action carried what the item
    /// WAS and `app.rs` inverted it, so "what this row does" lived in another file and, on the pair,
    /// would have been the same bool twice — one row doing its neighbour's write.
    #[test]
    fn every_watch_row_performs_the_verb_its_label_names() {
        use crate::viewstate::Write;
        let want = |l: &str| match l {
            "Mark as Watched" => Some(Write::Watched),
            "Mark as Unwatched" => Some(Write::Unwatched),
            _ => None, // navigation, the separator, Play from Start, Remove from Deck
        };
        for mark in [
            PosterMark::None,
            PosterMark::InProgress,
            PosterMark::Watched,
        ] {
            for kind in [0, 1, 2, 3] {
                for deck in [false, true] {
                    let (sec, acts) = build(&item(kind, mark), deck);
                    for (label, got) in verbs(&sec, &acts) {
                        assert_eq!(got, want(&label), "{label:?} (kind {kind}, {mark:?})");
                    }
                }
            }
            let (sec, acts) = build_episode("42", mark);
            for (label, got) in verbs(&sec, &acts) {
                assert_eq!(got, want(&label), "{label:?} on the filmstrip ({mark:?})");
            }
        }
    }

    /// The pair is TWO ROWS and therefore two actions, and `acts` has to grow with it —
    /// [`ACTS_PARALLEL`]'s failure is silent by construction (the press performs its neighbour's
    /// action), and the conditional tail is exactly where a row gets added without one. The
    /// builders' own `debug_assert` covers this for every case a test builds; this states it as the
    /// property.
    #[test]
    fn the_action_vector_grows_with_the_conditional_rows() {
        for mark in [
            PosterMark::None,
            PosterMark::InProgress,
            PosterMark::Watched,
        ] {
            for kind in [0, 1, 2, 3] {
                for deck in [false, true] {
                    let (sec, acts) = build(&item(kind, mark), deck);
                    assert_eq!(
                        acts.len(),
                        sec.rows.len(),
                        "kind {kind}, {mark:?}, deck {deck}"
                    );
                }
            }
            let (sec, acts) = build_episode("42", mark);
            assert_eq!(acts.len(), sec.rows.len(), "filmstrip, {mark:?}");
        }
    }

    /// **The focus walk steps over the separator and stops at the ends.**
    ///
    /// It was `TableView::move_sel`'s own skip-and-clamp, exercised on a live `TableView`; the walk
    /// is the focus ENGINE's now, so the property is stated on the screen's own
    /// `Focusable::neighbour` — which is where the separator's unfocusability actually lives
    /// (`acts[i].is_none()`), and which is what the engine asks.
    #[test]
    fn the_focus_walk_steps_over_the_separator_and_stops_at_the_ends() {
        let screen = screen_with(build(&item(3, PosterMark::None), false).1);
        let step = |from: u32, dir: Dir| {
            match with_cx(|cx| {
                <ItemMenuScreen as Focusable<HostFixture>>::neighbour(
                    &screen,
                    FocusKey {
                        entry: EntryId(7),
                        elem: from,
                    },
                    dir,
                    cx,
                )
            }) {
                Step::Move(key) => Some(key.elem),
                _ => None,
            }
        };
        assert_eq!(step(0, Dir::Down), Some(1)); // Go to Episode → Go to Show
        assert_eq!(step(1, Dir::Down), Some(3)); // NOT 2 — the separator is skipped
        assert_eq!(step(3, Dir::Down), Some(4)); // Play from Start
        assert_eq!(step(4, Dir::Down), None); // stops at the end
        assert_eq!(step(3, Dir::Up), Some(1)); // skipped back over the separator
        assert_eq!(step(1, Dir::Up), Some(0));
        assert_eq!(step(0, Dir::Up), None); // stops at the start
    }

    /// A cursor landed on the separator settles onto a real row — `TableView::set_sections`'
    /// `settle` used to do this; the ENGINE asks `reconcile`, which answers the first focusable
    /// row for anything that is not one.
    #[test]
    fn a_selection_landed_on_the_separator_settles_onto_a_real_row() {
        let screen = screen_with(build(&item(3, PosterMark::None), false).1);
        let want = FocusKey {
            entry: EntryId(7),
            elem: 2,
        }; // index 2 IS the separator
        let got =
            with_cx(|cx| <ItemMenuScreen as Focusable<HostFixture>>::reconcile(&screen, want, cx));
        assert_ne!(got.elem, 2, "the separator is not a focus stop");
        assert_eq!(got.elem, 0, "…and the first real row is where it lands");
    }

    /// **The menu carries the row it was opened on**, and it is the ENTRY's argument now rather
    /// than a `static mut ITEM`. That capture is the fix for a Play-from-Start that resolved its
    /// target through the HOME hub catalog (`pms::index_of_rk`) and so silently did nothing on
    /// every other card surface — and it is a stronger claim as an argument than as a global: the
    /// row cannot be re-pointed by a hub refetch rebuilding the catalog under an open panel, and
    /// the episode / season entry points carry NONE rather than the last card's.
    ///
    /// The third property the legacy test asserted — that the capture SURVIVES the close, because
    /// `on_ok` closed and the drain read the static a frame later — is gone with the static that
    /// needed it: the request carries the row, so nothing is read after the dismissal at all.
    #[test]
    fn the_menu_carries_the_row_it_was_opened_on_and_the_episode_menu_carries_none() {
        let mut m = item(0, PosterMark::None);
        m.sid = crate::plex::ServerId::from_raw(3);
        m.part = "/library/parts/42/file.mkv".to_string();

        let mut screen = ItemMenuScreen::new(EntryId(7), card_arg(&m, false));
        screen.build_rows();
        let elem = first_action(&screen, |a| matches!(a, Action::PlayFromStart(_)));
        let req = commit(&mut screen, elem);
        assert_eq!(
            req.sid,
            crate::plex::ServerId::from_raw(3),
            "the ROW's server, not the current one"
        );
        let carried = req.item.expect("the row the panel is about");
        assert_eq!(carried.rk, "42");
        assert_eq!(
            carried.part, "/library/parts/42/file.mkv",
            "the WHOLE row — a key alone cannot start playback"
        );
        assert!(
            !req.loaded_episode,
            "a card row is not a leaf of a loaded season"
        );

        let mut strip = ItemMenuScreen::new(
            EntryId(8),
            ItemMenuArg {
                sid: crate::plex::ServerId::from_raw(3),
                rk: "77".into(),
                kind: ItemMenuKind::Episode {
                    mark: PosterMark::None,
                },
                host: EntryId(1),
                focus: None,
                anchor: [0; 4],
                loaded_episode: true,
                from_home: false,
            },
        );
        strip.build_rows();
        let elem = first_action(&strip, |a| matches!(a, Action::PlayFromStart(_)));
        let req = commit(&mut strip, elem);
        assert!(
            req.item.is_none(),
            "an episode menu plays through the loaded season, never a stale row"
        );
        assert!(
            req.loaded_episode,
            "…and says so, which is what routes it there"
        );
    }

    #[test]
    fn the_panel_sits_beside_the_card_and_never_leaves_the_screen() {
        let h = 5.0 * 60.0; // a five-row menu, roughly
                            // a card on the left of the shelf: the panel sits to its RIGHT, clear of the card
        let r = panel_at(Rect::new(MARGIN_X, 300.0, CARD_W, CARD_H), h);
        assert!(
            r.x >= MARGIN_X + CARD_W,
            "expected the panel right of the card, got x={}",
            r.x
        );
        // a card at the right edge: it flips LEFT rather than running off screen — and lands clear
        // of the card it belongs to, which is the whole point of anchoring beside it
        let a = Rect::new(SCR_W - 300.0, 300.0, CARD_W, CARD_H);
        let r = panel_at(a, h);
        assert!(
            r.x + r.w <= SCR_W - EDGE + 0.5,
            "panel ran off the right edge: x={} w={}",
            r.x,
            r.w
        );
        assert!(
            r.x + r.w <= a.x,
            "flipped panel overlaps its card: x={} w={} card.x={}",
            r.x,
            r.w,
            a.x
        );
        assert!(r.x >= EDGE - 0.5);
        // a card near the bottom keeps the whole panel on screen
        let low = panel_at(Rect::new(MARGIN_X, SCR_H - 120.0, CARD_W, CARD_H), h);
        assert!(
            low.y + low.h <= SCR_H - EDGE + 0.5,
            "panel ran off the bottom: y={} h={}",
            low.y,
            low.h
        );
        assert!(low.y >= EDGE - 0.5);

        // …and "on screen" means inside the OVERSCAN frame, which is why the keep-out is per axis:
        // `space::XL` 64 clears `MARGIN_Y` and misses `MARGIN_X` by 32. A panel placed against an
        // anchor has no fixed rect a table could carry, so the frame is graded on its extremes here.
        let tall = panel_at(Rect::new(MARGIN_X, 300.0, CARD_W, CARD_H), 4000.0);
        for (what, p) in [("flipped", r), ("low", low), ("tall", tall)] {
            assert!(
                crate::ui::consts::inside_safe(p),
                "the {what} panel leaves the safe area: ({}, {}) {}x{}",
                p.x,
                p.y,
                p.w,
                p.h
            );
        }
    }

    /// **A held direction walks the list at the panel's own cadence, not the hardware's.**
    ///
    /// `app/run.rs` did this from a per-frame `App::held_key` timer at 110 ms, and this menu was
    /// its LAST consumer — the block, the field and `HeldKey::arm` all go with it. The dispatcher
    /// hands the surface `Edge::Repeat` at the remote's own ~50 ms, so the cadence has to be
    /// applied by the surface that receives it: the same `RepeatGate` / [`PANEL_REPEAT_MS`] the
    /// player's four panels take, reused rather than copied.
    ///
    /// A swallowed repeat is `Handled::Yes` (the engine never sees it); an admitted one is
    /// `Handled::No`, which is what hands the direction to the focus engine. Red first, observed:
    /// with the `RepeatGate` arm deleted from `step`, every repeat falls through and the engine
    /// walks the list at the hardware rate — 12 admitted steps in the window below against 4.
    #[test]
    fn the_item_menu_hold_repeats_through_the_surface_not_the_loop() {
        let mut screen = screen_with(build(&item(3, PosterMark::None), false).1);
        // the fresh press: acted on unconditionally, and it re-arms the cadence from itself
        assert_eq!(feed(&mut screen, Key::Down, Edge::Down, 1000), Handled::No);
        // the hardware's own repeats, 50 ms apart
        let mut admitted = 0;
        for i in 1..=12u32 {
            if feed(&mut screen, Key::Down, Edge::Repeat, 1000 + i * 50) == Handled::No {
                admitted += 1;
            }
        }
        // Four, not the 5.5 the ratio suggests: the gate admits a step at the first repeat AT OR
        // PAST the cadence, and the hardware's 50 ms lattice means that is every third one — the
        // same rounding the loop's own 110 ms timer had against a 16 ms frame.
        assert_eq!(
            admitted, 4,
            "600 ms of hardware repeat is 4 steps at {PANEL_REPEAT_MS} ms, not 12 at the remote's 50"
        );
        // …and a FRESH press is never swallowed by the cadence of the press before it
        assert_eq!(feed(&mut screen, Key::Up, Edge::Down, 1620), Handled::No);
    }

    // ---- fixtures ---------------------------------------------------------------------------

    use crate::screens::registry::{AppMsg, PageMemory};
    use crate::ui::fixture::FixtureMeasure;
    use crate::ui::machine::{FocusRead, Host, InputEvent, InputOwner, PressRead, Source, Tick};
    use crate::ui::screen::ScreenArg;

    #[derive(Clone)]
    struct Arg;
    impl LogicalState for Arg {
        fn write(&self, _: &mut Canon) {}
        fn probe(&self, _: &mut String) {}
    }
    impl ScreenArg for Arg {
        fn chrome(&self) -> crate::ui::machine::Chrome {
            crate::ui::machine::Chrome::None
        }
        fn id(&self) -> crate::ui::machine::ScreenId {
            crate::ui::machine::ScreenId(1)
        }
        fn title(&self) -> Option<&str> {
            None
        }
        fn same_instance(&self, _: &Self) -> bool {
            true
        }
    }
    struct HostFixture;
    impl Host for HostFixture {
        type Arg = Arg;
        type Fx = AppFx;
        type Msg = AppMsg;
        type Elem = u32;
        type Views<'a> = ();
        type Init = Arg;
        type Memory = PageMemory;
    }

    fn with_cx<R>(test: impl FnOnce(&Cx<'_, HostFixture>) -> R) -> R {
        let measure = FixtureMeasure;
        test(&Cx {
            views: (),
            tick: Tick::default(),
            measure: &measure,
            focus: FocusRead {
                current: None,
                ..Default::default()
            },
            press: PressRead::default(),
            owner: InputOwner::Entry(EntryId(7)),
        })
    }

    fn card_arg(m: &PmsMovie, from_deck: bool) -> ItemMenuArg {
        ItemMenuArg {
            sid: m.sid,
            rk: m.rk.clone(),
            kind: ItemMenuKind::Card {
                row: Box::new(m.clone()),
                from_deck,
            },
            host: EntryId(1),
            focus: None,
            anchor: [0; 4],
            loaded_episode: false,
            from_home: true,
        }
    }

    /// A mounted screen whose row set is already built, so a test can state a property about the
    /// walk or the cadence without going through the container.
    fn screen_with(acts: Vec<Option<Action>>) -> ItemMenuScreen {
        let mut s = ItemMenuScreen::new(EntryId(7), card_arg(&item(3, PosterMark::None), false));
        s.built = true;
        s.acts = acts;
        s
    }

    fn first_action(s: &ItemMenuScreen, want: impl Fn(&Action) -> bool) -> u32 {
        s.acts
            .iter()
            .position(|a| a.as_ref().is_some_and(&want))
            .expect("the row set offers this action") as u32
    }

    /// Commit a row and return the ONE request it emitted.
    fn commit(s: &mut ItemMenuScreen, elem: u32) -> ItemMenuReq {
        let mut out = Vec::new();
        let mut present = crate::ui::present::Present::new();
        let mut fx = Effects::new(
            &mut out,
            crate::ui::machine::MachineId::Instance(crate::ui::machine::InstanceId(8)),
            &mut present,
        );
        s.activate::<HostFixture>(elem, &mut fx);
        let mut req = None;
        let mut dismissed = false;
        for e in out {
            match e.fx {
                Fx::App(AppFx::ItemMenu(r)) => req = Some(r),
                Fx::Nav(NavOp::Dismiss(_)) => dismissed = true,
                _ => {}
            }
        }
        assert!(
            dismissed,
            "every commit dismisses, exactly as the legacy `on_ok` closed first"
        );
        req.expect("a row that acts reports its action")
    }

    fn feed(s: &mut ItemMenuScreen, key: Key, edge: Edge, ms: u32) -> Handled {
        let mut out = Vec::new();
        let mut present = crate::ui::present::Present::new();
        let mut fx = Effects::new(
            &mut out,
            crate::ui::machine::MachineId::Instance(crate::ui::machine::InstanceId(8)),
            &mut present,
        );
        let ev = ScreenEvent::<HostFixture>::Input(InputEvent {
            at: Tick { ms, dt_us: 0 },
            source: Source::Sdl,
            kind: InputKind::Key {
                key,
                sym: 0,
                wcode: 0,
                edge,
                at_edge: false,
            },
        });
        with_cx(|cx| <ItemMenuScreen as Machine<HostFixture>>::step(s, &ev, cx, &mut fx))
    }
}
