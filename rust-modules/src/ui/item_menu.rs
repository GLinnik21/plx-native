//! The **item context menu** — the popover a press-and-hold opens on a home shelf tile, on the same
//! [`Popover`] + [`TableView`] pair as the track menu and the profile menu.
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
//! **Every card surface opens it**, through two builders and one presenter. [`open`] serves the
//! card rows — a home shelf, the Library browse grid, a Search result shelf, a person's filmography
//! and the detail page's Related shelf — and [`open_episode`] the detail page's episode filmstrip
//! (the owner-reported gap: a long press on an episode still did nothing, so there was nowhere to
//! mark an episode watched). The row SET differs only because a navigation row that leads to the
//! page you are standing on is not an action; everything else — the panel, the placement, the
//! choreography, the state rows — is shared.
//!
//! **The Related shelf was the last card surface to join, and it is worth knowing why it was late.**
//! It was excluded on the grounds that its tiles carried no `(ratingKey, watched)` pair to build
//! rows from — which was true of the struct behind them and never of the data: `/related` returns
//! the same wire DTO as every other listing, and `fetch_related` copied three fields out of it and
//! dropped the rest. So the fix was upstream (`metadata::Related` is a real `pms::PmsMovie` now),
//! and this module needed nothing: a Related tile takes [`open`], because it is a card row like the
//! others and NOT a leaf of the season the page beneath it has loaded.
//!
//! **What remains excluded is excluded for a reason that does not dissolve.** A tile that is a
//! PERSON or a TAG has no rating key and no watch state at all, so every row this module can build
//! would be absent and the hold would open an empty panel: the detail page's cast headshots, and
//! Search's Cast & Crew / Collections rows (`search::Item::Tag` has no rating key). A hold there
//! keeps doing nothing, deliberately.
//!
//! Like [`crate::ui::account_menu`], this module only **reports** the chosen [`Action`]; `app.rs`
//! performs the routing, the server call and the hub refresh. It never mutates playback or PMS
//! state itself.
#![allow(non_upper_case_globals)]
use crate::pms::PmsMovie;
use crate::ui::consts::*;
use crate::ui::icons::Icon;
use crate::ui::popover::{Opener, Popover};
use crate::ui::table::{Row, Section, TableView};
use crate::ui::widgets::PosterMark;
use crate::ui::{theme, Rect};
use std::os::raw::c_int;
use std::ptr::{addr_of, addr_of_mut};

/// What the highlighted row does on OK. Every variant carries the identity it needs, captured when
/// the menu opened — a hub refetch can re-order the catalog underneath an open popover, so nothing
/// here is an index.
#[derive(Clone)]
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
    /// hide this item from the Continue Watching deck — the server-side `removeFromContinueWatching`.
    /// **Keeps the resume point**: it is a hide, not a reset, so playing the item again picks up
    /// where it left off (see `plex::Client::remove_from_continue_watching`).
    RemoveFromDeck(String),
}

impl Action {
    /// The view-state write this action performs, if it is one of the two that do — the item-menu
    /// twin of `detail::HeroAction::watch_write`, and the reason this enum carries two variants
    /// rather than one flag.
    ///
    /// The old shape was `MarkWatched(rk, watched)` where the bool was **what the item is NOW**, and
    /// `apply_item_action` inverted it at the press. That works for exactly as long as the menu emits
    /// one row: with this menu's part-watched PAIR both rows would carry the same bool, so one would
    /// invert to its neighbour's write and silently do the opposite of its own label. Now the glyph
    /// the user aimed at IS the verb, and nothing downstream re-reads the item.
    pub(crate) fn watch_write(&self) -> Option<crate::viewstate::Write> {
        match self {
            Action::MarkWatched(_) => Some(crate::viewstate::Write::Watched),
            Action::MarkUnwatched(_) => Some(crate::viewstate::Write::Unwatched),
            _ => None,
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

static mut POP: Popover = Popover::new(); // shared open/appear choreography
static mut TABLE: TableView = TableView::new(); // main-thread only
/// The chosen action per global row index (`None` for the separator, which is unfocusable anyway).
/// Parallel to the table's rows because the row SET varies by item kind — a movie has no
/// "Go to Show", a show has no "Play from Start" — so a positional `match sel` would be a lie.
static mut ACTS: Vec<Option<Action>> = Vec::new();
/// The element the menu was opened ON — its drawn rect (what the panel anchors beside) and how its
/// own screen re-draws it above the modal scrim. Supplied by the host at [`open`]; see
/// [`Opener`] for why the two halves are one value.
static mut OPENER: Opener = Opener::NONE;
/// **The item the menu is about**, captured at [`open`].
///
/// Every [`Action`] carries a `ratingKey` and [`SID`] says which server it means, which is enough to
/// FETCH — but not enough to PLAY: `route::request_play_movie` needs the whole row (its part id,
/// duration, resume offset, media flags), and the only way to get one back from a bare key used to
/// be `pms::index_of_rk`, which walks the HOME hub catalog alone. A library, search or person tile
/// is usually in no hub, so that lookup silently found nothing and "Play from Start" did nothing at
/// all. Carrying the row is both the fix and the smaller claim: this popover is about ONE item, and
/// it is the item that was on screen when the hold began, not whatever a key resolves to later.
///
/// Deliberately NOT cleared by [`close`], for [`SID`]'s reason — `on_ok` closes and then returns the
/// action, so the drain reads this one frame later.
static mut ITEM: Option<PmsMovie> = None;
/// WHICH SERVER every [`Action`] above is about, captured when the menu opened.
///
/// One popover is about ONE item, so the server belongs to the menu rather than to each variant —
/// and it has to be captured, not looked up when the action is performed: on a Continue Watching
/// shelf merged across servers, `apply_item_action` resolving a bare rk against the current server
/// is precisely the reported failure (long-press a friend's episode → Play from Start → our film
/// with the same number plays, under the friend's title). Deliberately NOT cleared by [`close`]:
/// `on_ok` returns the action after closing, so the drain reads this one frame later.
static mut SID: crate::plex::ServerId = crate::plex::ServerId::UNSET;

fn table() -> &'static mut TableView {
    unsafe { &mut *addr_of_mut!(TABLE) }
}

/// The highlighted row, for the focus probe (`crate::focusprobe`) — a READ of the cursor the key
/// ladder moves, and the reason it exists: `app.rs`'s UP/DOWN arm for this panel changes nothing
/// else, so without this the fingerprint records the panel opening and closing and nothing between.
/// Through `addr_of!` rather than the module's own `table()`, which hands out a `&'static mut`.
pub(crate) fn sel() -> i32 {
    unsafe { (*addr_of!(TABLE)).sel }
}
fn pop() -> &'static mut Popover {
    unsafe { &mut *addr_of_mut!(POP) }
}
fn acts() -> &'static mut Vec<Option<Action>> {
    unsafe { &mut *addr_of_mut!(ACTS) }
}

pub(crate) fn is_open() -> bool {
    unsafe { (*addr_of!(POP)).is_open() }
}

/// Is `m` an item the menu has anything to offer? A leaf or a show/season — i.e. everything the
/// home shelves carry. Kept as a predicate so the caller can decline to open an empty popover.
pub(crate) fn has_actions(m: &PmsMovie) -> bool {
    !m.rk.is_empty()
}

/// Open the menu for `m`, anchored beside (and lifting) `opener` — the focused tile on whichever
/// screen the hold happened on. An [`Opener`] with no rect centres the panel, which is what the
/// headless trigger and a host with nothing focused get.
pub(crate) fn open(m: &PmsMovie, from_deck: bool, opener: Opener) {
    unsafe {
        *addr_of_mut!(SID) = m.sid; // the ROW's server, not the current one
        *addr_of_mut!(ITEM) = Some(m.clone()); // …and the row itself — see [`ITEM`]
    }
    present(build(m, from_deck), opener);
}

/// Open the menu for the DETAIL page's focused episode still — the owner-reported gap (a long press
/// on an episode tile had no menu at all, so there was nowhere to mark one watched). Same panel,
/// same choreography, a shorter row set: see [`build_episode`].
///
/// No [`ITEM`] here: the detail page plays an episode through its own loaded season
/// (`detail::play_episode_rk_from_start`), which is the one path that never needed a catalog row.
pub(crate) fn open_episode(sid: crate::plex::ServerId, rk: &str, mark: PosterMark, opener: Opener) {
    unsafe {
        *addr_of_mut!(SID) = sid; // the loaded show's server — the episode is one of its own
        *addr_of_mut!(ITEM) = None;
    }
    present(build_episode(rk, mark), opener);
}

/// Open the menu for the DETAIL page's focused SEASON tab — the middle of the three grains this
/// page can mark, and the one that had no surface at all.
///
/// An episode is marked from its still's long press, a show from the hero's toggle (and from its
/// poster's menu on every card screen). "I have seen season 3" was expressible only by opening
/// eleven episodes one at a time, which is the shape of a missing control rather than a workflow.
///
/// Same panel, same rows, and — like [`open_episode`] — **no navigation group**: the season's tab
/// is selected by the very press that opened this, so there is nowhere for a nav row to go.
///
/// No [`ITEM`], for `open_episode`'s reason: a season is not a catalog row here, it is one of the
/// loaded show's own children.
pub(crate) fn open_season(sid: crate::plex::ServerId, rk: &str, mark: PosterMark, opener: Opener) {
    unsafe {
        *addr_of_mut!(SID) = sid; // the loaded show's server — the season is one of its own
        *addr_of_mut!(ITEM) = None;
    }
    present(build_season(rk, mark), opener);
}

/// The server every [`Action`] from the open (or just-closed) menu names — see [`SID`]. Read by
/// `app.rs`'s dispatch, which pairs it with the action's rk.
pub(crate) fn item_sid() -> crate::plex::ServerId {
    unsafe { *addr_of!(SID) }
}

/// The catalog row the open (or just-closed) menu is about — see [`ITEM`]. `None` on the detail
/// page's episode menu, which plays through the loaded season instead.
pub(crate) fn item() -> Option<&'static PmsMovie> {
    unsafe { (*addr_of!(ITEM)).as_ref() }
}

/// Put a built row set on screen beside `opener`. Shared by both entry points so the panel's
/// choreography — anchor fallback, compact rows, selection reset, the appear spring — is written
/// once and cannot drift between the screens the menu serves.
fn present((rows, a): (Section, Vec<Option<Action>>), opener: Opener) {
    let fallback =
        Rect::new((SCR_W - CARD_W) * 0.5 - PANEL_W * 0.5, (SCR_H - CARD_H) * 0.5, CARD_W, CARD_H);
    // The fallback is resolved HERE, once, so `panel_rect` (asked three times a frame) is a pure
    // read and the panel cannot drift if a host's rect stops resolving while the menu is up.
    unsafe { *addr_of_mut!(OPENER) = Opener { rect: Some(opener.rect.unwrap_or(fallback)), ..opener } };
    *acts() = a;
    table().compact = true; // a short list of one-line actions — BODY labels, not menu-size HEADLINE
    table().set_sections(vec![rows], 0, false);
    pop().open();
}

pub(crate) fn close() {
    pop().close();
}

/// The rows, and the action each one commits. Order is the pinned design's:
/// navigation (`Go to Episode` · `Go to Show`) — separator — state (the watch row or ROWS ·
/// `Play from Start`), adapted per item kind (`PmsMovie::kind`: 0 movie / 1 show / 2 season /
/// 3 episode). The state group is one row or two off [`state_rows`], so this list has no fixed
/// length and every index into it is resolved through `ACTS` — see [`ACTS_PARALLEL`].
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
            nav.push(("Go to Episode", Icon::Episode, Action::GoToItem(m.rk.clone())));
            if !m.show_rk.is_empty() {
                nav.push(("Go to Show", Icon::Show, Action::GoToShow(m.show_rk.clone(), m.season_index)));
            }
        }
        // a season has no page of its own — it IS the show page with that season selected, so one
        // row covers it; a show's own page is likewise the only navigation it has
        2 if !m.show_rk.is_empty() => {
            nav.push(("Go to Season", Icon::Show, Action::GoToShow(m.show_rk.clone(), m.season_index)));
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
    // captured at `open` — so it is exact for a leaf (`viewCount`/`viewOffset` off the same container
    // the shelf was built from) AND for a container, whose three states are already on the row as the
    // `unwatched`/`watched` PAIR (`pms::parse_item`: a show is "neither" exactly while some of its
    // leaves are viewed and some are not). This is the follow-up the comment here used to ask for —
    // it said a show/season "cannot distinguish part-watched from fully-watched" and so kept a
    // one-way row, which stopped being true when `PmsMovie::watched` gained its strict
    // `viewedLeafCount >= leafCount` rule.
    let mut sec = state_rows(sec, &mut acts, &m.rk, crate::ui::widgets::row_watch_state(m), leaf);

    // ---- and, only on a Continue Watching card, the row that takes it off the deck ----
    // Gated on the SHELF, not on the item: the action is meaningless anywhere else (nothing to
    // remove it from), and offering it on a Recently Added card would be a row that silently did
    // nothing. `pms::hub_is_continue` is what the caller passes in.
    //
    // Last, after the watched toggle, because it is the only row here that removes something from
    // view — the destructive-ish end of the group, where a mis-hit is least likely.
    if from_deck {
        // "Remove from Deck", not "Remove from Continue Watching": the longer label elides inside
        // `PANEL_W` at the table's compact BODY size, and widening the panel would push it across the
        // neighbouring card it is anchored beside. It is also the more accurate of the two — the server
        // action hides the item from the DECK and leaves its resume point intact, so "remove from
        // continue watching" over-promises a reset it does not perform.
        sec = sec.row(Row::new("Remove from Deck").licon(Icon::Close));
        acts.push(Some(Action::RemoveFromDeck(m.rk.clone())));
    }
    debug_assert_eq!(acts.len(), sec.rows.len(), "{ACTS_PARALLEL}");
    (sec, acts)
}

/// Why [`build`] and [`build_episode`] both end in a length assertion, and why it is HERE rather
/// than at the `present` that installs the pair.
///
/// `ACTS` is the index→action map — `on_ok` resolves the pressed row by its index into it — so a
/// `sec.row` added without its `acts.push` cannot panic. It shifts every action below it by one,
/// and the press performs its neighbour's. This is the menu where that is easiest to do, because
/// it is the only one of the four whose row set is CONDITIONAL: three item kinds, an optional
/// `Go to Show`, an optional deck row, and a state group shared with a second entry point.
///
/// The three sibling menus (`account_menu`, `more_menu`, `alt_sources`) assert at their equivalent
/// of `present`. This one cannot usefully: `present` is reached only from `open`/`open_episode`,
/// which no host test calls, and `debug_assert!` is compiled out of the `--release` build the
/// Makefile ships — so an assertion there would be unreachable in the only configuration that can
/// still run it. At the two builders it is exercised by all nine of this module's tests for free.
const ACTS_PARALLEL: &str = "ACTS must stay one-to-one with the rows: a row without its action \
                             shifts every action below it, and the press performs its neighbour's";

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
/// It was one row whose label and glyph FLIPPED on a `watched` bool. That is an exact toggle only
/// while an item is at one end or the other; on the third state it had to pick an end and picked
/// "not watched", so the way back from a half-watched item was unreachable from the tile that
/// offered every other action on it — the owner's report.
///
/// **Both glyphs are FILLED discs**: [`Icon::CheckCircleFill`] for the row that marks watched,
/// [`Icon::MinusCircleFill`] for the one that takes it away. A ticked circle beside "Mark as
/// Unwatched" states the outcome backwards, which is the one thing a destructive-ish row must not do
/// — and filled is what separates an ACTION from a STATE: this column carries a picker's bare tick or
/// an action's glyph, while a switch says what it is set to in words at the far edge. The hero's
/// discs take the BARE glyph for the other half of that same rule (a control that IS a circle needs
/// no drawn disc), and the two are deliberately not unified.
fn state_rows(sec: Section, acts: &mut Vec<Option<Action>>, rk: &str, mark: PosterMark, leaf: bool) -> Section {
    let mut sec = sec;
    if mark != PosterMark::Watched {
        sec = sec.row(Row::new(crate::ui::widgets::MARK_WATCHED_VERB).licon(Icon::CheckCircleFill));
        acts.push(Some(Action::MarkWatched(rk.to_string())));
    }
    if mark != PosterMark::None {
        sec = sec.row(Row::new(crate::ui::widgets::MARK_UNWATCHED_VERB).licon(Icon::MinusCircleFill));
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
/// It is worth saying what is NO LONGER a reason, because it used to be half of this argument:
/// reaching that page cost you your place. It does not any more — the filmstrip's own metadata row
/// is the way there, it raises `detail::take_open_request` instead of re-mounting in place, and
/// `ui::trail` brings BACK to the season being browsed, on the episode it was opened from.
///
/// `mark` is exact here — `detail::focused_episode` resolves it through the same `ep_state` that
/// draws the still's own state line, so the tile and the menu opened on it cannot describe one
/// episode two ways — and with no nav group there is no separator either (`build`'s rule: the divider
/// only exists when there is a group above it). An episode is a LEAF, so all three states are
/// reachable and a part-watched one gets the pair, exactly as a shelf card does.
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
///
/// A season IS a container, so all three states are reachable and a part-watched one gets the PAIR
/// ([`state_rows`]) — the menu's rule, deliberately not the hero toggle's; see `state_rows` for
/// why the two surfaces answer one question differently.
fn build_season(rk: &str, mark: PosterMark) -> (Section, Vec<Option<Action>>) {
    let mut acts: Vec<Option<Action>> = Vec::new();
    let sec = state_rows(Section::new(""), &mut acts, rk, mark, false);
    debug_assert_eq!(acts.len(), sec.rows.len(), "{ACTS_PARALLEL}");
    (sec, acts)
}

pub(crate) fn move_focus(sym: c_int) {
    let s = sym as u32;
    if s == SDLK_UP {
        table().move_sel(-1);
    } else if s == SDLK_DOWN {
        table().move_sel(1);
    }
}

/// Commit the highlighted row and close.
pub(crate) fn on_ok() -> Action {
    let sel = table().sel;
    close();
    acts().get(sel.max(0) as usize).cloned().flatten().unwrap_or(Action::None)
}

/// Pointer hover: focus follows the cursor over the popover rows.
pub(crate) fn pointer_focus(mx: f32, my: f32) {
    if !is_open() {
        return;
    }
    if let Some(gi) = table().hit_row(panel_rect(), mx, my) {
        table().sel = gi;
    }
}

/// Pointer click: commit the row under the cursor (same as OK); a click elsewhere reports
/// `Action::None` and the caller dismisses like BACK.
pub(crate) fn click(mx: f32, my: f32) -> Action {
    if !is_open() {
        return Action::None;
    }
    if let Some(gi) = table().hit_row(panel_rect(), mx, my) {
        table().sel = gi;
        return on_ok();
    }
    close();
    Action::None
}

/// Beside the card, never over it: to its RIGHT by default, flipped to its LEFT when that would run
/// past the screen's keep-out. Vertically it hangs off the card's top edge, pulled back inside the
/// safe band so a bottom shelf still gets a whole panel. Pure (anchor + measured height in, rect
/// out) so the placement rules are host-testable without the module's statics.
fn panel_at(a: Rect, content_h: f32) -> Rect {
    let h = content_h.clamp(120.0, SCR_H - 2.0 * EDGE); // same floor the profile popover uses
    let right = a.x + a.w + CARD_GAP;
    let x = if right + PANEL_W <= SCR_W - EDGE_X { right } else { a.x - CARD_GAP - PANEL_W };
    let x = x.clamp(EDGE_X, (SCR_W - EDGE_X - PANEL_W).max(EDGE_X));
    let y = a.y.clamp(EDGE, (SCR_H - EDGE - h).max(EDGE));
    Rect::new(x, y, PANEL_W, h)
}

fn panel_rect() -> Rect {
    let a = unsafe { (*addr_of!(OPENER)).rect }.unwrap_or(Rect::new(0.0, 0.0, CARD_W, CARD_H));
    panel_at(a, table().measured_height())
}

pub(crate) fn update(dt: f32) {
    if !is_open() {
        return;
    }
    pop().update(dt);
    let ph = panel_rect().h;
    table().update(dt, ph - crate::ui::table::PAD_V);
}

/// The modal dim, drawn as part of the HOST PAGE rather than with the panel — see
/// [`crate::ui::popover::Popover::scrim`] for why, and `app.rs`'s page closure for where.
///
/// **This menu is drawn AFTER the page closure, which is the whole test.** It is served by the
/// capture path today, which grabs framebuffer 0 and so picks the scrim up for free — but only
/// because no dynamic glass owner is live while a popover is open (`tab_glass_wanted` excludes
/// them), so nothing invalidates and the direct path never runs. That is three modules' behaviour
/// holding one invariant up; arm `/tmp/plxnative-glassboth` and it is already false. Owning the
/// scrim here costs one call and does not depend on any of it.
///
/// It also LIFTS the tile the menu was opened on back out of the dim ([`Opener`]). That tile is the
/// panel's whole subject — the design's stated point is that "the card and the rest of the shelf
/// stay where they are, visible behind it", which a scrim over the card itself quietly contradicts
/// — and being inside the page closure is what puts the un-dimmed copy into the direct-blur
/// snapshot too, so the panel's own glass never frosts a dimmed picture of its own card.
pub(crate) fn draw_scrim() {
    if is_open() {
        pop().scrim_lifting(SCRIM_A, unsafe { &*addr_of!(OPENER) });
    }
}

pub(crate) fn draw() {
    if !is_open() {
        return;
    }
    // the shared appear fade, rising a short beat into place off the card it belongs to. The scrim
    // (light — the shelf must stay readable behind it) is the PAGE's now: `content_painter`, not
    // `painter`, or the dim is drawn twice.
    let p = pop().content_painter(14.0);
    let r = panel_rect();
    pop().panel(p, r, PANEL_RAD);
    table().draw(p, r);
}

// ---------------------------------------------------------------------------------------
#[cfg(test)]
mod tests {
    use super::*;

    /// A catalog row of `kind` in a given watch state, with the flags a real `pms::parse_item`
    /// would set — which is the part that matters here, since the menu's row set is now derived
    /// from `unwatched`/`watched`/`resume_ms` together rather than from one of them.
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
        assert_eq!(crate::ui::widgets::row_watch_state(&m), mark, "the fixture must build the state it names");
        m
    }
    fn labels(sec: &Section) -> Vec<String> {
        sec.rows.iter().map(|r| if r.sep { "—".to_string() } else { r.label.clone() }).collect()
    }
    /// The write each row commits, paired with its label — the projection every row-set assertion
    /// below is really about, since a label and its verb living in two places is the bug this
    /// module's `Action` split closed.
    fn verbs(sec: &Section, acts: &[Option<Action>]) -> Vec<(String, Option<crate::viewstate::Write>)> {
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
            ["Go to Episode", "Go to Show", "—", "Mark as Watched", "Play from Start"]
        );
        // the separator carries no action, every other row does
        assert!(acts[2].is_none());
        assert!(acts.iter().enumerate().filter(|(i, _)| *i != 2).all(|(_, a)| a.is_some()));
        // "Go to Show" targets the SHOW rk + the episode's season, not the episode
        match acts[1].as_ref().unwrap() {
            Action::GoToShow(rk, season) => assert_eq!((rk.as_str(), *season), ("7", 3)),
            _ => panic!("row 1 must be Go to Show"),
        }
    }

    #[test]
    fn a_watched_leaf_offers_only_the_way_back() {
        let (sec, acts) = build(&item(0, PosterMark::Watched), false); // movie, viewCount >= 1
        assert_eq!(labels(&sec), ["Go to Movie", "—", "Mark as Unwatched", "Play from Start"]);
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
        assert_eq!(labels(&sec), ["Go to Show", "—", "Mark as Watched", "Mark as Unwatched"]);
        assert_eq!(acts[2].as_ref().unwrap().watch_write(), Some(crate::viewstate::Write::Watched));
        assert_eq!(acts[3].as_ref().unwrap().watch_write(), Some(crate::viewstate::Write::Unwatched));

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
        assert_eq!(labels(&sec), ["Go to Episode", "—", "Mark as Watched", "Play from Start"]);
        assert!(acts.iter().flatten().all(|a| !matches!(a, Action::GoToShow(..))));
        // and a SEASON with no parent has no navigation at all — so no leading separator either,
        // which would otherwise open the menu with a rule above its first row
        let mut s = item(2, PosterMark::None);
        s.show_rk.clear();
        let (sec, _) = build(&s, false);
        assert_eq!(labels(&sec), ["Mark as Watched"]);
    }

    /// **Remove from Continue Watching** is gated on the SHELF, not on the item: only a card that came
    /// from the deck has a deck to leave. Offered anywhere else it would be a row that appeared to work
    /// and silently changed nothing, since the server-side action only affects that hub.
    ///
    /// Pinned last in the group on purpose — it is the one row here that takes something out of view,
    /// so it sits where a mis-press is least likely, and the assertion below is what keeps a later row
    /// from being appended after it.
    #[test]
    fn the_remove_from_deck_row_exists_only_on_a_continue_watching_card() {
        // same card, both shelves — the ONLY difference is where it was focused
        let (off_deck, acts_off) = build(&item(3, PosterMark::None), false);
        assert_eq!(labels(&off_deck), ["Go to Episode", "Go to Show", "—", "Mark as Watched", "Play from Start"]);
        assert!(
            !acts_off.iter().flatten().any(|a| matches!(a, Action::RemoveFromDeck(_))),
            "a card off the deck must not offer to remove it from one"
        );

        let (on_deck, acts_on) = build(&item(3, PosterMark::None), true);
        assert_eq!(
            labels(&on_deck),
            ["Go to Episode", "Go to Show", "—", "Mark as Watched", "Play from Start", "Remove from Deck"],
            "…and on the deck it is the LAST row, after the watched toggle"
        );
        match acts_on.last().expect("a trailing action").as_ref().expect("not a separator") {
            Action::RemoveFromDeck(rk) => assert_eq!(rk, "42", "…carrying the card's own ratingKey"),
            _ => panic!("the last row must be the deck removal"),
        }
        // it is an ADDITION to the group, not a replacement — the watch-state row is still there
        assert!(acts_on.iter().flatten().any(|a| matches!(a, Action::MarkWatched(_))));
    }

    /// The detail page's filmstrip menu: the state rows, no navigation group, and therefore no
    /// leading separator. It shares `state_rows` with the shelf menu, so the row the owner asked
    /// for cannot say one thing on Home and another on the episode page — asserted by comparing the
    /// two builders' tails rather than by re-listing the labels here.
    #[test]
    fn the_filmstrip_menu_is_the_state_group_alone() {
        let (sec, acts) = build_episode("42", PosterMark::None);
        assert_eq!(labels(&sec), ["Mark as Watched", "Play from Start"]);
        assert!(acts.iter().all(|a| a.is_some()), "no separator, so every row acts");
        // the shelf menu's episode rows END with exactly these two, in this order
        let (shelf, _) = build(&item(3, PosterMark::None), false);
        assert_eq!(labels(&sec), labels(&shelf)[shelf.rows.len() - 2..]);

        // a watched episode gets the way back instead, carrying its OWN rk (the menu captured its
        // target at open time — nothing here may resolve through a live focus index)
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
        assert!(!acts.iter().flatten().any(|a| matches!(a, Action::GoToShow(..) | Action::GoToItem(_))));
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
        assert_eq!(tail(PosterMark::None), ["Mark as Watched", "Play from Start"]);
        assert_eq!(tail(PosterMark::InProgress), ["Mark as Watched", "Mark as Unwatched", "Play from Start"]);
        assert_eq!(tail(PosterMark::Watched), ["Mark as Unwatched", "Play from Start"]);

        // …and the detail page's filmstrip, off the same builder, so the two cannot drift
        let strip = |mark| {
            let (sec, _) = build_episode("42", mark);
            labels(&sec)
        };
        assert_eq!(strip(PosterMark::None), ["Mark as Watched", "Play from Start"]);
        assert_eq!(strip(PosterMark::InProgress), ["Mark as Watched", "Mark as Unwatched", "Play from Start"]);
        assert_eq!(strip(PosterMark::Watched), ["Mark as Unwatched", "Play from Start"]);
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
    /// `metadata::tests::related_rows_carry_the_watch_state_the_wire_already_had`, which drives the
    /// real `related_rows`; the two are a pair and neither is sufficient alone.
    ///
    /// It also pins the half a `viewCount > 0` shortcut gets wrong. A related **movie** is a leaf
    /// and offers Play from Start; a related **show** is a container, offers none, and when it is
    /// part-watched it is at NEITHER end of the range and gets both write verbs — the same rule the
    /// detail hero's discs follow.
    #[test]
    fn a_related_tile_off_the_wire_gets_the_row_set_its_state_earns() {
        let row = |json: &str| {
            let body = format!(r#"{{"MediaContainer":{{"Hub":[{{"Metadata":[{json}]}}]}}}}"#);
            let mc = serde_json::from_str::<crate::plex::Envelope>(&body).expect("parses").media_container;
            crate::pms::parse_item(&mc.hub[0].metadata[0], crate::plex::ServerId::UNSET)
        };
        let set = |json: &str| labels(&build(&row(json), false).0);

        // a MOVIE, in each of the three states — one navigation row, then the state group
        let movie = |extra: &str| format!(r#"{{"ratingKey":"11","type":"movie","duration":"7020000"{extra}}}"#);
        assert_eq!(
            set(&movie("")),
            ["Go to Movie", "—", "Mark as Watched", "Play from Start"],
            "never started: only the way forward"
        );
        assert_eq!(
            set(&movie(r#","viewOffset":"3510000""#)),
            ["Go to Movie", "—", "Mark as Watched", "Mark as Unwatched", "Play from Start"],
            "part-watched: both ends are reachable and both are true"
        );
        assert_eq!(
            set(&movie(r#","viewCount":2"#)),
            ["Go to Movie", "—", "Mark as Unwatched", "Play from Start"],
            "finished: only the way back"
        );

        // a SHOW — a container, so no Play from Start, and its middle state is the leaf-count one
        let show = |extra: &str| format!(r#"{{"ratingKey":"14","type":"show","leafCount":10{extra}}}"#);
        assert_eq!(set(&show("")), ["Go to Show", "—", "Mark as Watched"], "no leaf viewed");
        assert_eq!(
            set(&show(r#","viewedLeafCount":3"#)),
            ["Go to Show", "—", "Mark as Watched", "Mark as Unwatched"],
            "3 of 10: neither watched nor unwatched, so BOTH verbs — the case `viewCount > 0` misses"
        );
        assert_eq!(set(&show(r#","viewedLeafCount":10"#)), ["Go to Show", "—", "Mark as Unwatched"], "all 10");

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
        for mark in [PosterMark::None, PosterMark::InProgress, PosterMark::Watched] {
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

    /// The pair is TWO ROWS and therefore two actions, and `ACTS` has to grow with it —
    /// `ACTS_PARALLEL`'s failure is silent by construction (the press performs its neighbour's
    /// action), and the conditional tail is exactly where a row gets added without one. The builders'
    /// own `debug_assert` covers this for every case a test builds; this states it as the property.
    #[test]
    fn the_action_vector_grows_with_the_conditional_rows() {
        for mark in [PosterMark::None, PosterMark::InProgress, PosterMark::Watched] {
            for kind in [0, 1, 2, 3] {
                for deck in [false, true] {
                    let (sec, acts) = build(&item(kind, mark), deck);
                    assert_eq!(acts.len(), sec.rows.len(), "kind {kind}, {mark:?}, deck {deck}");
                }
            }
            let (sec, acts) = build_episode("42", mark);
            assert_eq!(acts.len(), sec.rows.len(), "filmstrip, {mark:?}");
        }
    }

    /// This test and the next drive a live `TableView`, whose selection moves `Spring::jump` —
    /// which reports to `ui::idle`'s process-global dirty flag. Serial by obligation, not
    /// precaution (`xfade.rs`'s rule): run parallel, they intermittently failed OTHER modules'
    /// "a settled screen asks for nothing" assertions.
    #[test]
    fn the_focus_walk_steps_over_the_separator_and_stops_at_the_ends() {
        let _g = crate::testlock::serial();
        let mut t = TableView::new();
        let (sec, _) = build(&item(3, PosterMark::None), false);
        t.set_sections(vec![sec], 0, false);
        assert_eq!(t.sel, 0); // Go to Episode
        t.move_sel(1);
        assert_eq!(t.sel, 1); // Go to Show
        t.move_sel(1);
        assert_eq!(t.sel, 3); // NOT 2 — the separator is skipped
        t.move_sel(1);
        assert_eq!(t.sel, 4); // Play from Start
        t.move_sel(1);
        assert_eq!(t.sel, 4); // clamped at the end
        t.move_sel(-1);
        assert_eq!(t.sel, 3);
        t.move_sel(-1);
        assert_eq!(t.sel, 1); // skipped back over the separator
        t.move_sel(-1);
        t.move_sel(-1);
        assert_eq!(t.sel, 0); // clamped at the start
    }

    #[test]
    fn a_selection_landed_on_the_separator_settles_onto_a_real_row() {
        let _g = crate::testlock::serial();
        let mut t = TableView::new();
        let (sec, _) = build(&item(3, PosterMark::None), false);
        t.set_sections(vec![sec], 2, false); // index 2 IS the separator
        assert_eq!(t.sel, 3);
    }

    /// **The menu carries the row it was opened on** — the fix for a Play-from-Start that resolved
    /// its target through the HOME hub catalog and so silently did nothing on every other card
    /// surface. Three properties, because they are the three ways the capture can be wrong: the row
    /// is there after `open`, it SURVIVES the close (`on_ok` closes and then returns the action, so
    /// the drain reads this a frame later, exactly as `SID` does), and the episode menu leaves it
    /// empty rather than holding the last card's — the detail page plays through its loaded season
    /// and must never fall back onto a stale row.
    ///
    /// Serial: `present` drives a live `TableView`, whose `Spring::jump` reports to `ui::idle`'s
    /// process-global flag, and `open`/`close` move the popover's process-wide open count.
    #[test]
    fn the_menu_carries_the_row_it_was_opened_on_and_the_episode_menu_carries_none() {
        let _g = crate::testlock::serial();
        let mut m = item(0, PosterMark::None);
        m.sid = crate::plex::ServerId::from_raw(3);
        m.part = "/library/parts/42/file.mkv".to_string();

        open(&m, false, crate::ui::popover::Opener::NONE);
        // `super::item`, spelled out: this module's tests already have a local `item(kind, …)`
        // fixture builder, which shadows the glob import
        let carried = super::item().expect("the row the popover is about");
        assert_eq!(carried.rk, "42");
        assert_eq!(
            carried.part,
            "/library/parts/42/file.mkv",
            "the WHOLE row — a key alone cannot start playback"
        );
        assert_eq!(item_sid(), crate::plex::ServerId::from_raw(3), "…on the row's own server");

        close();
        assert!(super::item().is_some(), "the drain reads it a frame after the close, like `SID`");

        open_episode(crate::plex::ServerId::from_raw(3), "77", PosterMark::None, crate::ui::popover::Opener::NONE);
        assert!(super::item().is_none(), "an episode menu plays through the loaded season, never a stale row");
        close();
    }

    #[test]
    fn the_panel_sits_beside_the_card_and_never_leaves_the_screen() {
        let h = 5.0 * 60.0; // a five-row menu, roughly
        // a card on the left of the shelf: the panel sits to its RIGHT, clear of the card
        let r = panel_at(Rect::new(MARGIN_X, 300.0, CARD_W, CARD_H), h);
        assert!(r.x >= MARGIN_X + CARD_W, "expected the panel right of the card, got x={}", r.x);
        // a card at the right edge: it flips LEFT rather than running off screen — and lands clear
        // of the card it belongs to, which is the whole point of anchoring beside it
        let a = Rect::new(SCR_W - 300.0, 300.0, CARD_W, CARD_H);
        let r = panel_at(a, h);
        assert!(r.x + r.w <= SCR_W - EDGE + 0.5, "panel ran off the right edge: x={} w={}", r.x, r.w);
        assert!(r.x + r.w <= a.x, "flipped panel overlaps its card: x={} w={} card.x={}", r.x, r.w, a.x);
        assert!(r.x >= EDGE - 0.5);
        // a card near the bottom keeps the whole panel on screen
        let low = panel_at(Rect::new(MARGIN_X, SCR_H - 120.0, CARD_W, CARD_H), h);
        assert!(low.y + low.h <= SCR_H - EDGE + 0.5, "panel ran off the bottom: y={} h={}", low.y, low.h);
        assert!(low.y >= EDGE - 0.5);

        // …and "on screen" means inside the OVERSCAN frame, which is why the keep-out is per axis:
        // `space::XL` 64 clears `MARGIN_Y` and misses `MARGIN_X` by 32. A panel placed against an
        // anchor has no fixed rect a table could carry, so the frame is graded on its extremes here.
        let tall = panel_at(Rect::new(MARGIN_X, 300.0, CARD_W, CARD_H), 4000.0);
        for (what, p) in [("flipped", r), ("low", low), ("tall", tall)] {
            assert!(crate::ui::consts::inside_safe(p), "the {what} panel leaves the safe area: ({}, {}) {}x{}", p.x, p.y, p.w, p.h);
        }
    }
}
