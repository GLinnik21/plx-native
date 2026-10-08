//! The **item context menu** — a registered `Style::Compact` surface on the shared `ModalStack`,
//! opened by a press-and-hold on a card (or on the detail page's episode still / season tab).
//!
//! The reference is **Apple TV's card menu, not Plex's**: a panel anchored BESIDE the focused card
//! (the card and the rest of the shelf stay where they are, visible behind it), rows of
//! `[leading icon] [label]`, the focused row a filled light pill spanning the panel, and a hairline
//! separator between the navigation actions and the state actions. Plex's own version is a
//! full-screen sheet; that is deliberately not what this is.
//!
//! It exists because OK on a Continue Watching tile can **resume immediately** (the Playback
//! setting [`DeckPress::Play`]; the amber play badge on the card is the affordance that says so).
//! The hold is the *other* half of that interaction, and until it existed Go-to-Show, per-item
//! Mark-as-Watched and Play-from-Start had nowhere to live (`docs/parity-gaps.md` §1.2/§1.3, §5a).
//! Under the default, [`DeckPress::Details`], OK opens the page instead and the deck card's menu
//! leads with the Play row that press gave up (and drops the Go to Movie / Episode row it now
//! duplicates).
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
//! Search's Cast & Crew rows (`search::Item::Tag` has no rating key). A collection has a rating
//! key but no watch state and no row here either ([`has_actions`] refuses it), so a hold on a
//! Collections tile opens the collection page as OK does.
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
use std::convert::Infallible;
use std::os::raw::c_int;

use plx_data::pms::PmsMovie;
use plx_media::route::DeckPress;
use crate::registry::{tile_facts, RepeatGate, PANEL_REPEAT_MS};
use crate::registry::{AppFx, AppLike, ItemMenuArg, ItemMenuKind, ItemMenuReq};
use plx_ui::consts::*;
use plx_ui::form::{Activation, Form, FormId, FormSection, FormTable, RowKey, RowKind};
use plx_ui::frame::Budget;
use plx_ui::icons::Icon;
use plx_machine::machine::{
    Canon, Cx, Edge, Effects, EntryId, FocusKey, Fx, GroupId, Handled, InputKind, Key,
    LogicalState, Machine, NavOp,
};
use plx_ui::screen::{
    Activate, At, AxisMask, Dir, DrawFrame, EdgeRule, ElemKind, Focusable, GroupKind, GroupSpec,
    Hover, Placed, RenderStrategy, Screen, ScreenEvent, Scrim, Seat, Step, Stop,
};
use plx_ui::table::{Row, MENU_MAX_W, MENU_MIN_W};
use plx_ui::widgets::PosterMark;
use plx_ui::{theme, Rect};

/// What the highlighted row does on OK. Every variant carries the identity it needs, captured when
/// the menu opened — a hub refetch can re-order the catalog underneath an open panel, so nothing
/// here is an index.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Action {
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
    /// play this item — a leaf resumes where it left off when it has a resume point; a show or
    /// season takes `activate_card`'s Play path (open its page, fire that page's Play) — the
    /// Continue Watching card's first row while OK on that card opens the page
    /// ([`DeckPress::Details`])
    Play(String),
    /// play this leaf ignoring its resume point
    PlayFromStart(String),
    /// play the item's already-loaded trailer extra — never the parent, never a PMS fetch
    PlayTrailer {
        rk: String,
        part: String,
        vcodec: String,
        acodec: String,
        title: String,
    },
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
    pub fn watch_write(&self) -> Option<plx_data::viewstate::Write> {
        match self {
            Action::MarkWatched(_) => Some(plx_data::viewstate::Write::Watched),
            Action::MarkUnwatched(_) => Some(plx_data::viewstate::Write::Unwatched),
            _ => None,
        }
    }

    /// The ratingKey this action acts on.
    pub fn rk(&self) -> &str {
        match self {
            Action::GoToItem(rk)
            | Action::GoToShow(rk, _)
            | Action::MarkWatched(rk)
            | Action::MarkUnwatched(rk)
            | Action::Play(rk)
            | Action::PlayFromStart(rk)
            | Action::RemoveFromDeck(rk) => rk,
            Action::PlayTrailer { rk, .. } => rk,
        }
    }

    fn write(&self, c: &mut Canon) {
        // The tags are the recorded shape's (`SHAPE`); 0 was the retired `None` sentinel.
        let tag = match self {
            Action::GoToItem(_) => 1,
            Action::GoToShow(..) => 2,
            Action::MarkWatched(_) => 3,
            Action::MarkUnwatched(_) => 4,
            Action::PlayFromStart(_) => 5,
            Action::RemoveFromDeck(_) => 6,
            Action::PlayTrailer { .. } => 7,
            Action::Play(_) => 8,
        };
        c.u32(tag).str(self.rk());
        if let Action::GoToShow(_, season) = self {
            c.u32(*season as u32);
        }
        if let Action::PlayTrailer {
            part,
            vcodec,
            acodec,
            title,
            ..
        } = self
        {
            c.str(part).str(vcodec).str(acodec).str(title);
        }
    }
}

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
const EDGE_X: f32 = plx_ui::consts::MARGIN_X;

pub const SHAPE: &str =
    "ItemMenu{arg:ItemMenuArg,acts:[Option<Action{tag:u32,rk:str,season:u32,part:str,vcodec:str,acodec:str,title:str}>],sel:i32,\
     table:TableViewMotion}";

/// Is `m` an item the menu has anything to offer? A leaf or a show/season — i.e. everything the
/// home shelves carry. Kept as a predicate so the caller can decline to present an empty panel.
pub fn has_actions(m: &PmsMovie) -> bool {
    m.kind != plx_data::pms::KIND_COLLECTION && !m.rk.is_empty()
}

/// A menu row's identity: which of the seven rows it is. Hand-assigned keys, never a position —
/// the row SET varies by item kind (three item kinds, an optional `Go to Show`, an optional deck
/// row, a state group shared with two other entry points), so a position would name a different
/// row in every menu, and reordering a menu would move focus keys. A row is declared once, with
/// its action, so a row cannot exist without one and a press cannot perform its neighbour's.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ItemRow {
    GoToItem,
    GoToShow,
    MarkWatched,
    MarkUnwatched,
    PlayFromStart,
    PlayTrailer,
    RemoveFromDeck,
    Play,
}

impl ItemRow {
    /// The focus element of this row (for a test, or a probe, that addresses it by identity).
    #[cfg(any(test, feature = "test-support"))]
    pub fn focus_key(self) -> u32 {
        self.key().0
    }
}

impl FormId for ItemRow {
    fn key(&self) -> RowKey {
        RowKey(match self {
            ItemRow::GoToItem => 1,
            ItemRow::GoToShow => 2,
            ItemRow::MarkWatched => 3,
            ItemRow::MarkUnwatched => 4,
            ItemRow::PlayFromStart => 5,
            ItemRow::PlayTrailer => 6,
            ItemRow::RemoveFromDeck => 7,
            ItemRow::Play => 8,
        })
    }
}

/// What every item-menu builder returns: a one-section form of button rows whose action is the
/// whole of what a press means (no row pushes a page — `Dest` is uninhabited).
type ItemForm = Form<ItemRow, Action, Infallible>;

/// The rows, and the action each one commits. Order is the pinned design's:
/// navigation (`Go to Episode` · `Go to Show`) — separator — state (the watch row or ROWS ·
/// `Play from Start`), adapted per item kind (`PmsMovie::kind`: 0 movie / 1 show / 2 season /
/// 3 episode; 4 collection is refused by [`has_actions`], which every caller asks first). The
/// state group is one row or two off [`state_rows`], so this list has no fixed length.
#[cfg(test)]
fn build(m: &PmsMovie, from_deck: bool) -> ItemForm {
    build_with(m, from_deck, DeckPress::Play, None)
}

/// `deck_press` is the Continue Watching setting. It changes this menu for a card FROM the deck
/// only, and only while OK there opens the page ([`DeckPress::Details`]): the menu then leads with
/// Play (the opening row — the press that used to play now lives here) and drops the row that
/// navigates to the card's own page (Go to Movie / Episode, and for a show or season its Go to
/// Show / Season), which is what OK on the card already does. Any other card, and a deck
/// card while OK plays, gets the menu it always had.
fn build_with(
    m: &PmsMovie,
    from_deck: bool,
    deck_press: DeckPress,
    trailer: Option<&plx_data::metadata::Extra>,
) -> ItemForm {
    let leaf = m.kind == 0 || m.kind == 3;
    let opens_details = from_deck && !deck_press.press_plays();
    let mut sec = FormSection::new(""); // no header: the card behind the panel IS the title

    // ---- the deck card's own press, when OK no longer plays ----
    // First, so it is where the menu opens. It resumes when there is a resume point and the label
    // says which, in the words the hero's pill uses.
    // A show or season has no resume point of its own, so it says "Play".
    let resumes = leaf && plx_data::metadata::resume_ns(m.resume_ms, m.dur_ns / 1_000_000) > 0;
    sec = sec.item_if(
        opens_details && (leaf || m.kind == 1 || m.kind == 2),
        ItemRow::Play,
        RowKind::Button,
        Action::Play(m.rk.clone()),
        Row::new(if resumes {
            plx_platform::i18n::msg::browse_home_continue()
        } else {
            plx_platform::i18n::msg::browse_detail_play()
        })
        .licon(Icon::Play),
    );

    // ---- navigation: this tile's own page, then the show it belongs to ----
    // A row whose TARGET is missing is simply absent: a hub row can arrive without a
    // grandparentRatingKey, and a "Go to Show" that resolves to an empty rk would fire a blocking
    // fetch for nothing and land the user on a blank page.
    let has_show = !m.show_rk.is_empty();
    let go_item = |label: &'static str| {
        (ItemRow::GoToItem, Action::GoToItem(m.rk.clone()), Row::new(label).licon(Icon::Episode))
    };
    let go_show = |label: &'static str, rk: &str, season: c_int| {
        (ItemRow::GoToShow, Action::GoToShow(rk.to_string(), season), Row::new(label).licon(Icon::Show))
    };
    let mut nav = Vec::new();
    match m.kind {
        3 => {
            if !opens_details {
                nav.push(go_item(plx_platform::i18n::msg::browse_menu_go_episode()));
            }
            if has_show {
                nav.push(go_show(plx_platform::i18n::msg::browse_menu_go_show(), &m.show_rk, m.season_index));
            }
        }
        // a show or season deck card whose OK opens the page: that page IS the "go to" row's
        // target, so the row would only repeat the press (Play leads instead)
        1 | 2 if opens_details => {}
        // a season has no page of its own — it IS the show page with that season selected, so one
        // row covers it; a show's own page is likewise the only navigation it has
        2 if has_show => nav.push(go_show(plx_platform::i18n::msg::browse_menu_go_season(), &m.show_rk, m.season_index)),
        2 => {}
        1 => nav.push(go_show(plx_platform::i18n::msg::browse_menu_go_show(), &m.rk, 0)),
        _ if opens_details => {}
        _ => nav.push(go_item(plx_platform::i18n::msg::browse_menu_go_movie())),
    }
    // the divider the design groups on — only when there IS a group above it
    let had_nav = !nav.is_empty();
    for (id, action, row) in nav {
        sec = sec.item(id, RowKind::Button, action, row);
    }
    if had_nav {
        sec = sec.separator();
    }

    // ---- state ----
    // The row set comes from the shared `widgets::row_watch_state`, over the catalog row this menu
    // captured when it was presented — so it is exact for a leaf (`viewCount`/`viewOffset` off the
    // same container the shelf was built from) AND for a container, whose three states are already
    // on the row as the `unwatched`/`watched` PAIR (`pms::parse_item`: a show is "neither" exactly
    // while some of its leaves are viewed and some are not).
    let mut sec = state_rows(
        sec,
        &m.rk,
        plx_ui::widgets::row_watch_state(&tile_facts::of(m)),
        leaf,
        trailer.filter(|_| m.kind == 0 || m.kind == 1),
        &m.title,
    );

    // ---- and, only on a Continue Watching card, the row that takes it off the deck ----
    // Gated on the SHELF, not on the item: the action is meaningless anywhere else (nothing to
    // remove it from), and offering it on a Recently Added card would be a row that silently did
    // nothing. `pms::hub_is_continue` is what the caller passes in.
    //
    // Last, after the watched toggle, because it is the only row here that removes something from
    // view — the destructive-ish end of the group, where a mis-hit is least likely.
    //
    // This hides the item from the deck and leaves its resume point intact. Panel sizing follows
    // the translated action, so the destination remains readable before confirming.
    sec = sec.item_if(
        from_deck,
        ItemRow::RemoveFromDeck,
        RowKind::Button,
        Action::RemoveFromDeck(m.rk.clone()),
        Row::new(plx_platform::i18n::msg::browse_menu_remove_deck()).licon(Icon::Close).destructive(true),
    );
    Form::new().section(sec)
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
    sec: FormSection<ItemRow, Action, Infallible>,
    rk: &str,
    mark: PosterMark,
    leaf: bool,
    trailer: Option<&plx_data::metadata::Extra>,
    parent_title: &str,
) -> FormSection<ItemRow, Action, Infallible> {
    let playable = trailer.filter(|e| e.playable());
    sec.item_if(
        mark != PosterMark::Watched,
        ItemRow::MarkWatched,
        RowKind::Button,
        Action::MarkWatched(rk.to_string()),
        Row::new(plx_ui::widgets::mark_watched_verb()).licon(Icon::CheckCircleFill),
    )
    // Destructive: it throws the watch record away, so a menu never OPENS on it
    // (`TableView::opening_row`) — a watched episode opens on Play from Start instead.
    .item_if(
        mark != PosterMark::None,
        ItemRow::MarkUnwatched,
        RowKind::Button,
        Action::MarkUnwatched(rk.to_string()),
        Row::new(plx_ui::widgets::mark_unwatched_verb())
            .licon(Icon::MinusCircleFill)
            .destructive(true),
    )
    .item_if(
        leaf,
        ItemRow::PlayFromStart,
        RowKind::Button,
        Action::PlayFromStart(rk.to_string()),
        Row::new(plx_ui::widgets::play_from_start_verb()).licon(Icon::PlayStart),
    )
    .item_if(
        playable.is_some(),
        ItemRow::PlayTrailer,
        RowKind::Button,
        playable.map_or_else(
            || Action::PlayTrailer {
                rk: String::new(), part: String::new(), vcodec: String::new(), acodec: String::new(), title: String::new(),
            },
            |extra| Action::PlayTrailer {
                rk: extra.rk.clone(),
                part: extra.part.clone(),
                vcodec: extra.vcodec.clone(),
                acodec: extra.acodec.clone(),
                title: extra.hud_title(parent_title).to_string(),
            },
        ),
        Row::new(plx_ui::widgets::play_trailer_verb()).licon(Icon::Trailer),
    )
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
/// divider only exists when there is a group above it). An episode is a LEAF, so all three states are
/// reachable and a part-watched one gets the pair, exactly as a shelf card does.
fn build_episode(rk: &str, mark: PosterMark) -> ItemForm {
    Form::new().section(state_rows(FormSection::new(""), rk, mark, true, None, ""))
}

/// The DETAIL page's season strip: [`build_episode`]'s row set with the one difference a season
/// makes — **no "Play from Start"**.
///
/// That row means "play THIS item from 00:00", and a season is not a thing you play: the press
/// would have to pick a leaf, which is a second decision the row does not state. Starting a season
/// from its beginning is what the first episode's own tile does, exactly and visibly. So
/// `leaf: false` — the same flag [`build`] passes for a show, for the same reason.
fn build_season(rk: &str, mark: PosterMark) -> ItemForm {
    Form::new().section(state_rows(FormSection::new(""), rk, mark, false, None, ""))
}

/// Beside the card, never over it: to its RIGHT by default, flipped to its LEFT when that would run
/// past the screen's keep-out. Vertically it hangs off the card's top edge, pulled back inside the
/// safe band so a bottom shelf still gets a whole panel. Pure (anchor + measured height in, rect
/// out), which is what makes the placement rules host-testable.
fn panel_at(a: Rect, content_w: f32, content_h: f32) -> Rect {
    let width = content_w.clamp(MENU_MIN_W, MENU_MAX_W);
    let h = content_h.clamp(120.0, SCR_H - 2.0 * EDGE); // same floor the profile popover uses
    let right = a.x + a.w + CARD_GAP;
    let x = if right + width <= SCR_W - EDGE_X {
        right
    } else {
        a.x - CARD_GAP - width
    };
    let x = x.clamp(EDGE_X, (SCR_W - EDGE_X - width).max(EDGE_X));
    let y = a.y.clamp(EDGE, (SCR_H - EDGE - h).max(EDGE));
    Rect::new(x, y, width, h)
}

/// The anchor a menu with no rect to hang off falls back to — a centred card. The headless trigger
/// and a host with nothing focused both land here.
///
/// Resolved by the PRESENTER, once, rather than every frame inside [`ItemMenuScreen::frame`], so
/// the panel cannot drift if a host's rect stops resolving while the menu is up — the same
/// argument the legacy `present` made for resolving it before it stored `OPENER`.
pub fn fallback_anchor() -> Rect {
    Rect::new(
        (SCR_W - CARD_W) * 0.5 - MENU_MIN_W * 0.5,
        (SCR_H - CARD_H) * 0.5,
        CARD_W,
        CARD_H,
    )
}

pub struct ItemMenuScreen {
    entry: EntryId,
    arg: ItemMenuArg,
    /// The rows and the action each one commits, declared together ([`ItemForm`]) — the row SET
    /// varies by item kind, so a row is found by its identity ([`ItemRow`]), never by a position.
    form: FormTable<ItemRow, Action, Infallible>,
    /// The cadence a HELD direction walks this panel's list at. `app/run.rs`'s client-side repeat
    /// timer (`App::held_key`) did this at 110 ms for exactly this menu and, before phase 9, for
    /// the player's four panels; the dispatcher delivers the hardware's ~50 ms `Edge::Repeat`
    /// instead, so the surface applies the cadence itself — the same [`PANEL_REPEAT_MS`] those
    /// four take, reused rather than copied.
    repeat: RepeatGate,
    built: bool,
}

/// Play Trailer only when the already-loaded Detail is this movie/show and already has a trailer.
/// The menu never talks to PMS.
fn cached_trailer(
    sid: plx_plex::plex::ServerId,
    m: &PmsMovie,
    meta: plx_data::metadata::MetadataView<'_>,
) -> Option<plx_data::metadata::Extra> {
    if m.kind != 0 && m.kind != 1 {
        return None;
    }
    let d = meta.current()?;
    if !plx_plex::plex::same_item((d.sid, d.rk.as_str()), (sid, m.rk.as_str())) {
        return None;
    }
    d.trailer().cloned().filter(|e| e.playable())
}

impl ItemMenuScreen {
    pub fn new(entry: EntryId, arg: ItemMenuArg) -> Self {
        Self {
            entry,
            arg,
            form: FormTable::new(crate::registry::BAND),
            repeat: RepeatGate::IDLE,
            built: false,
        }
    }

    /// The rows, built ONCE at `Mount`. A hub refetch can re-order the catalog underneath an open
    /// panel, so nothing here is rebuilt while the menu is up — which is also why every [`Action`]
    /// carries the identity it needs rather than an index.
    fn build_rows(&mut self, meta: plx_data::metadata::MetadataView<'_>) {
        if self.built {
            return;
        }
        self.built = true;
        let form = match &self.arg.kind {
            ItemMenuKind::Card { row, from_deck } => {
                let trailer = cached_trailer(self.arg.sid, row, meta);
                build_with(row, *from_deck, plx_media::route::deck_press(), trailer.as_ref())
            }
            ItemMenuKind::Episode { mark } => build_episode(&self.arg.rk, *mark),
            ItemMenuKind::Season { mark } => build_season(&self.arg.rk, *mark),
        };
        // a short list of one-line actions — BODY labels, not menu-size HEADLINE
        self.form.table.compact = true;
        self.form.set(form, None);
    }

    /// Where focus starts, and where it falls back to when the key it had is gone — the table's
    /// opening row, which skips the separator and never lands on a destructive row while any other
    /// is on offer.
    fn opening(&self) -> u32 {
        self.form.opening_key().map_or(0, |k| k.0)
    }

    fn frame(&self, measure: &dyn plx_machine::machine::Measure) -> Rect {
        let [x, y, w, h] = self.arg.anchor.map(f32::from_bits);
        panel_at(Rect::new(x, y, w, h), self.form.table.measured_width(measure), self.form.table.measured_height())
    }

    /// The rows a focus stop exists for — every bound row. The separator is unfocusable, exactly
    /// as `TableView::move_sel` skips it.
    fn focusable(&self) -> impl Iterator<Item = u32> + '_ {
        (0..self.form.table.n_rows() as usize).filter_map(|i| self.form.key_at(i).map(|k| k.0))
    }

    fn step_focus(&self, from: u32, delta: i32) -> Option<u32> {
        self.form.step_key(RowKey(from), delta).map(|k| k.0)
    }

    /// Commit the focused row: report the action, then dismiss. The legacy `on_ok` closed first and
    /// returned the action second; the two are one drain here, and the order is what keeps the
    /// loop's dispatch reading a REQUEST rather than a static a frame after the close.
    fn activate<H: AppLike>(&mut self, elem: u32, fx: &mut Effects<'_, H>) {
        let act = self
            .form
            .index_of_key(RowKey(elem))
            .and_then(|i| self.form.activate(i))
            .map(|a| match a {
                Activation::Action(act) => act,
                Activation::Push(never) => match never {},
            });
        // Every arm of the dispatch turns an rk into a blocking fetch, a scrobble or a play; an
        // empty one would fetch nothing and land on a blank page. `build` already refuses to offer
        // such a row — this is the belt to that braces, since the rows are data-driven off hub
        // rows. A key that names no row (the separator has none) only dismisses.
        if let Some(act) = act.filter(|act| !act.rk().is_empty()) {
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
    pub fn sel(&self) -> i32 {
        self.form.table.sel
    }

    /// The server every action from this menu names. Captured rather than looked up: on a Continue
    /// Watching shelf merged across servers, resolving a bare rk against the CURRENT server is the
    /// reported bug itself (hold a friend's episode → Play from Start → our film with the same key
    /// plays, under the friend's title).
    pub fn sid(&self) -> plx_plex::plex::ServerId {
        self.arg.sid
    }

    /// The page this menu hangs off, and the element on it the panel is anchored beside and lifted
    /// back out of the dim. ONE owner: the entry's own argument, captured when the menu was
    /// presented. It was `Bridge::menu_opener`, a second copy of the same pair on the side.
    pub fn opener(&self) -> (EntryId, Option<FocusKey<u32>>) {
        (self.arg.host, self.arg.focus)
    }
}

impl<H: AppLike + crate::registry::MetadataLike> Machine<H> for ItemMenuScreen {
    type Ev = ScreenEvent<H>;
    fn step(&mut self, ev: &Self::Ev, cx: &Cx<'_, H>, fx: &mut Effects<'_, H>) -> Handled {
        match ev {
            ScreenEvent::Mount => {
                self.build_rows(H::metadata(cx));
                // The menu is the thing the Home hold hint teaches; once ANY card screen has
                // opened one the lesson is learned and the hint retires (`ui::hold_hint::learned`).
                plx_ui::hold_hint::mark_learned();
            }
            ScreenEvent::Tick(tick) => {
                self.form.table.sel = cx
                    .focus
                    .current
                    .filter(|key| key.entry == self.entry)
                    .and_then(|key| self.form.index_of_key(RowKey(key.elem)))
                    .map_or(self.form.table.sel, |i| i as i32);
                self.form.table.update(tick.dt(), self.frame(cx.measure).h);
            }
            ScreenEvent::FocusMoved { to, .. } => {
                if let Some(i) = self.form.index_of_key(RowKey(to.elem)) {
                    self.form.table.sel = i as i32;
                }
            }
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
    fn groups(&self, cx: &Cx<'_, H>, out: &mut Vec<GroupSpec>) {
        out.push(GroupSpec {
            id: GroupId(0),
            kind: GroupKind::Column,
            seat: Seat::Remembered,
            reachable: AxisMask::BOTH,
            edge: [EdgeRule::Stop; 4],
            extent: self.frame(cx.measure),
            len: self.focusable().count(),
            elem: ElemKind::Bare,
        });
    }
    fn group_of(&self, elem: &u32, _: &Cx<'_, H>) -> Option<GroupId> {
        self.form.index_of_key(RowKey(*elem)).map(|_| GroupId(0))
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
        let index = self.form.index_of_key(RowKey(*elem))?;
        let rect = self.form.table.row_frame(self.frame(cx.measure), index as i32)?;
        Some(Placed {
            rect,
            rest_rect: rect,
            clip: self.frame(cx.measure),
            index: Some(index as u32),
        })
    }
    fn reconcile(&self, want: FocusKey<u32>, cx: &Cx<'_, H>) -> FocusKey<u32> {
        if <Self as Focusable<H>>::group_of(self, &want.elem, cx).is_some() {
            want
        } else {
            FocusKey {
                entry: self.entry,
                elem: self.opening(),
            }
        }
    }
    fn seat(&self, _: GroupId, _: Placed, _: &Cx<'_, H>) -> FocusKey<u32> {
        FocusKey {
            entry: self.entry,
            elem: self.form.selected_key().map_or_else(|| self.opening(), |k| k.0),
        }
    }
}

impl<H: AppLike + crate::registry::MetadataLike> Screen<H> for ItemMenuScreen {
    fn opener(&self) -> Option<(EntryId, Option<FocusKey<u32>>)> {
        Some(ItemMenuScreen::opener(self))
    }
    fn as_any(&self) -> Option<&dyn std::any::Any> {
        Some(self)
    }
    fn name(&self) -> &'static str {
        crate::registry::word::ITEM_MENU
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
    /// itself quietly contradicts. The un-dimmed copy is also what the host snapshot holds.
    fn scrim(&self) -> Scrim {
        // The COMPACT role — the lightest dim in `theme::underlay`: the design's whole point is that
        // the card and the shelf stay legible behind the popover, so this recesses them rather
        // than blanking them.
        Scrim::dim(theme::underlay::DIM_COMPACT)
    }
    fn prepare(&mut self, _: &mut Budget, _: &Cx<'_, H>) {}
    fn draw(&mut self, f: &mut DrawFrame<'_, '_, H>) {
        let p = f.painter.alpha(f.page_alpha);
        let r = self.frame(f.measure);
        plx_ui::widgets::panel_ground(p, r, PANEL_RAD, f.underlay);
        self.form.table.draw(p, r, f.measure);
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
        let n = self.form.table.n_rows() as usize;
        c.seq(n);
        for i in 0..n {
            c.option(self.form.binding_at(i).map(|b| &b.action), |c, a| a.write(c));
        }
        c.u32(self.form.table.sel as u32);
        self.form.table.write_motion(c);
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
            plx_ui::widgets::row_watch_state(&tile_facts::of(&m)),
            mark,
            "the fixture must build the state it names"
        );
        m
    }
    /// A built menu: the form run through a [`FormTable`], so a test reads rows back the way the
    /// screen does — by identity — instead of through a parallel vector.
    struct Built(FormTable<ItemRow, Action, Infallible>);
    fn built(form: ItemForm) -> Built {
        let mut table = FormTable::new(crate::registry::BAND);
        table.set(form, None);
        Built(table)
    }
    fn build(m: &PmsMovie, from_deck: bool) -> Built {
        built(super::build(m, from_deck))
    }
    fn build_with(m: &PmsMovie, from_deck: bool, trailer: Option<&plx_data::metadata::Extra>) -> Built {
        built(super::build_with(m, from_deck, DeckPress::Play, trailer))
    }
    /// A menu built under the Continue Watching setting `mode`.
    fn build_deck(m: &PmsMovie, from_deck: bool, mode: DeckPress) -> Built {
        built(super::build_with(m, from_deck, mode, None))
    }
    fn build_episode(rk: &str, mark: PosterMark) -> Built {
        built(super::build_episode(rk, mark))
    }
    fn build_season(rk: &str, mark: PosterMark) -> Built {
        built(super::build_season(rk, mark))
    }
    impl Built {
        /// The row ids in drawn order (the separator is not a row).
        fn ids(&self) -> Vec<ItemRow> {
            (0..self.0.table.n_rows() as usize).filter_map(|i| self.0.id_at(i).copied()).collect()
        }
        /// Every action the menu can commit, in drawn order.
        fn actions(&self) -> Vec<Action> {
            (0..self.0.table.n_rows() as usize)
                .filter_map(|i| self.0.binding_at(i).map(|b| b.action.clone()))
                .collect()
        }
        /// The action the row `id` commits.
        fn action(&self, id: ItemRow) -> Action {
            let i = self.0.index_of(&id).unwrap_or_else(|| panic!("{id:?} is not on offer"));
            self.0.binding_at(i).expect("bound").action.clone()
        }
        fn offers(&self, id: ItemRow) -> bool {
            self.0.index_of(&id).is_some()
        }
        fn is_destructive(&self, id: ItemRow) -> bool {
            self.0.index_of(&id).is_some_and(|i| self.0.table.sections[0].rows[i].destructive)
        }
    }
    fn labels(menu: &Built) -> Vec<String> {
        menu.0.table.sections[0]
            .rows
            .iter()
            .map(|r| if r.sep { "—".to_string() } else { r.label.clone() })
            .collect()
    }
    /// The write each row commits, paired with its label — the projection every row-set assertion
    /// below is really about, since a label and its verb living in two places is the bug this
    /// module's `Action` split closed.
    fn verbs(menu: &Built) -> Vec<(String, Option<plx_data::viewstate::Write>)> {
        let table = &menu.0.table;
        (0..table.n_rows() as usize)
            .map(|i| {
                let label = table.sections[0].rows[i].label.clone();
                (label, menu.0.binding_at(i).and_then(|b| b.action.watch_write()))
            })
            .collect()
    }

    /// **Under "Open details" a deck card's menu leads with Play and opens on it**, drops the Go
    /// to Movie / Episode row OK now does, and keeps every other row — and in Play mode and off
    /// the deck the menu is exactly today's.
    #[test]
    fn a_deck_card_leads_with_play_when_ok_opens_the_page() {
        // an episode: Play, Go to Show, separator, state, Remove last
        let on = build_deck(&item(3, PosterMark::None), true, DeckPress::Details);
        assert_eq!(
            labels(&on),
            ["Play", "Go to Show", "—", "Mark as Watched", "Play from Start", "Remove from Deck"]
        );
        assert_eq!(
            on.ids(),
            [ItemRow::Play, ItemRow::GoToShow, ItemRow::MarkWatched, ItemRow::PlayFromStart, ItemRow::RemoveFromDeck]
        );
        assert!(!on.offers(ItemRow::GoToItem), "OK on the card already opens that page");
        assert_eq!(on.action(ItemRow::Play), Action::Play("42".into()));
        assert_eq!(
            on.0.opening_key(),
            Some(ItemRow::Play.key()),
            "the menu opens on Play"
        );

        // a movie: no navigation group at all, so no separator — not at the top, not under Play
        let movie = build_deck(&item(0, PosterMark::None), true, DeckPress::Details);
        assert_eq!(labels(&movie), ["Play", "Mark as Watched", "Play from Start", "Remove from Deck"]);
        assert!(!movie.offers(ItemRow::GoToItem) && !movie.offers(ItemRow::GoToShow));
        assert!(!labels(&movie).iter().any(|l| l == "—"), "{:?}", labels(&movie));

        // an episode with no show row loses its whole group: still no stray separator
        let mut orphan = item(3, PosterMark::None);
        orphan.show_rk.clear();
        let orphan = build_deck(&orphan, true, DeckPress::Details);
        assert_eq!(labels(&orphan), ["Play", "Mark as Watched", "Play from Start", "Remove from Deck"]);

        // an item with a resume point says so, in the hero pill's word
        let resuming = build_deck(&item(0, PosterMark::InProgress), true, DeckPress::Details);
        assert_eq!(labels(&resuming)[0], "Continue");
    }

    /// A show or season card on the deck (the Library's `*.inprogress.*` shelf can hold one) in
    /// Open-details mode: Play leads and opens the menu, and the self-navigation row OK already
    /// does is dropped — exactly as for a leaf. Play mode keeps today's rows.
    #[test]
    fn a_show_or_season_deck_card_leads_with_play_when_ok_opens_the_page() {
        for (kind, nav) in [(1, "Go to Show"), (2, "Go to Season")] {
            let m = item(kind, PosterMark::InProgress);
            let on = build_deck(&m, true, DeckPress::Details);
            let l = labels(&on);
            assert_eq!(l[0], "Play", "{kind}: {l:?}");
            assert!(!l.iter().any(|x| x == nav), "{kind}: {l:?}");
            assert!(!on.offers(ItemRow::GoToItem) && !on.offers(ItemRow::GoToShow), "{kind}");
            assert_eq!(on.action(ItemRow::Play), Action::Play("42".into()));
            assert_eq!(on.0.opening_key(), Some(ItemRow::Play.key()), "{kind}: opens on Play");
            assert_ne!(l[1], "—", "{kind}: no separator under Play: {l:?}");
            assert_ne!(l.last().unwrap(), "—", "{kind}: {l:?}");
            assert!(!l.windows(2).any(|w| w[0] == "—" && w[1] == "—"), "{kind}: {l:?}");
            // the remaining rows are the Play-mode menu's, minus its navigation group
            let play = labels(&build_deck(&m, true, DeckPress::Play));
            assert_eq!(play[0], nav, "{kind}: {play:?}");
            assert_eq!(l[1..], play[2..], "{kind}: {l:?} vs {play:?}");
            assert!(!play.iter().any(|x| x == "Play" || x == "Continue"), "{play:?}");
            // off the deck the setting is not consulted
            assert_eq!(
                labels(&build_deck(&m, false, DeckPress::Details)),
                labels(&build_deck(&m, false, DeckPress::Play)),
                "{kind}"
            );
        }
    }

    #[test]
    fn the_play_mode_and_off_deck_menus_are_unchanged() {
        for kind in [0, 3] {
            let today = labels(&build(&item(kind, PosterMark::None), true));
            assert!(!today.iter().any(|l| l == "Play" || l == "Continue"), "{today:?}");
            assert_eq!(labels(&build_deck(&item(kind, PosterMark::None), true, DeckPress::Play)), today);
            // off the deck the setting is not consulted at all
            for mode in DeckPress::LADDER {
                assert_eq!(
                    labels(&build_deck(&item(kind, PosterMark::None), false, mode)),
                    labels(&build(&item(kind, PosterMark::None), false)),
                    "{kind} {mode:?}"
                );
            }
        }
        let play_mode = build_deck(&item(0, PosterMark::None), true, DeckPress::Play);
        assert!(play_mode.offers(ItemRow::GoToItem) && !play_mode.offers(ItemRow::Play));
        let off_deck = build_deck(&item(3, PosterMark::None), false, DeckPress::Details);
        assert!(off_deck.offers(ItemRow::GoToItem) && !off_deck.offers(ItemRow::Play));
    }

    #[test]
    fn an_episode_offers_the_pinned_row_set_in_order() {
        let menu = build(&item(3, PosterMark::None), false);
        assert_eq!(
            labels(&menu),
            [
                "Go to Episode",
                "Go to Show",
                "—",
                "Mark as Watched",
                "Play from Start"
            ]
        );
        // the separator is not a row: every BOUND row acts, and there are four of them
        assert_eq!(
            menu.ids(),
            [ItemRow::GoToItem, ItemRow::GoToShow, ItemRow::MarkWatched, ItemRow::PlayFromStart]
        );
        // "Go to Show" targets the SHOW rk + the episode's season, not the episode
        match menu.action(ItemRow::GoToShow) {
            Action::GoToShow(rk, season) => assert_eq!((rk.as_str(), season), ("7", 3)),
            _ => panic!("Go to Show must commit GoToShow"),
        }
    }

    #[test]
    fn a_watched_leaf_offers_only_the_way_back() {
        let menu = build(&item(0, PosterMark::Watched), false); // movie, viewCount >= 1
        assert_eq!(
            labels(&menu),
            ["Go to Movie", "—", "Mark as Unwatched", "Play from Start"]
        );
        assert_eq!(
            menu.action(ItemRow::MarkUnwatched).watch_write(),
            Some(plx_data::viewstate::Write::Unwatched),
            "a finished item has one end left to be sent to"
        );
        // a movie has no show, so no second navigation row
        assert!(!menu.offers(ItemRow::GoToShow));
    }

    #[test]
    fn a_collection_has_no_item_menu_actions() {
        let collection = PmsMovie { rk: "42".into(), kind: plx_data::pms::KIND_COLLECTION,
            ..Default::default() };
        assert!(!has_actions(&collection), "a collection menu must not be openable before its page exists");
    }

    /// A show or season never offers *Play from Start* — there is no single part to start — and
    /// its watch state is the flag PAIR, not one flag: `!unwatched` alone means "some leaf was
    /// watched", which is the middle of the range and not the end of it.
    #[test]
    fn a_show_has_no_play_from_start_and_gets_the_pair_when_it_is_mid_run() {
        let menu = build(&item(1, PosterMark::InProgress), false);
        assert_eq!(
            labels(&menu),
            ["Go to Show", "—", "Mark as Watched", "Mark as Unwatched"]
        );
        assert_eq!(
            menu.action(ItemRow::MarkWatched).watch_write(),
            Some(plx_data::viewstate::Write::Watched)
        );
        assert_eq!(
            menu.action(ItemRow::MarkUnwatched).watch_write(),
            Some(plx_data::viewstate::Write::Unwatched)
        );

        // a show whose every leaf is seen is DONE, and offering to mark it watched again was the
        // old row set's other wrong answer (it read `!unwatched`, which cannot tell the two apart)
        let menu = build(&item(1, PosterMark::Watched), false);
        assert_eq!(labels(&menu), ["Go to Show", "—", "Mark as Unwatched"]);
        // …and one nobody has opened offers only the way forward
        let menu = build(&item(1, PosterMark::None), false);
        assert_eq!(labels(&menu), ["Go to Show", "—", "Mark as Watched"]);
    }

    #[test]
    fn a_row_whose_target_is_missing_is_not_offered_at_all() {
        // an episode hub row that arrived without a grandparentRatingKey: no "Go to Show" — the row
        // would resolve to an empty rk, i.e. a blocking fetch for nothing and a blank page
        let mut m = item(3, PosterMark::None);
        m.show_rk.clear();
        let menu = build(&m, false);
        assert_eq!(
            labels(&menu),
            ["Go to Episode", "—", "Mark as Watched", "Play from Start"]
        );
        assert!(!menu.offers(ItemRow::GoToShow));
        // and a SEASON with no parent has no navigation at all — so no leading separator either,
        // which would otherwise open the menu with a rule above its first row
        let mut s = item(2, PosterMark::None);
        s.show_rk.clear();
        let menu = build(&s, false);
        assert_eq!(labels(&menu), ["Mark as Watched"]);
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
        let off_deck = build(&item(3, PosterMark::None), false);
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
            !off_deck.offers(ItemRow::RemoveFromDeck),
            "a card off the deck must not offer to remove it from one"
        );

        let on_deck = build(&item(3, PosterMark::None), true);
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
        assert_eq!(on_deck.ids().last(), Some(&ItemRow::RemoveFromDeck), "it is the last row");
        match on_deck.action(ItemRow::RemoveFromDeck) {
            Action::RemoveFromDeck(rk) => {
                assert_eq!(rk, "42", "…carrying the card's own ratingKey")
            }
            _ => panic!("the deck row must commit the deck removal"),
        }
        // it is an ADDITION to the group, not a replacement — the watch-state row is still there
        assert!(on_deck.offers(ItemRow::MarkWatched));
    }

    /// The detail page's filmstrip menu: the state rows, no navigation group, and therefore no
    /// leading separator. It shares `state_rows` with the shelf menu, so the row the owner asked
    /// for cannot say one thing on Home and another on the episode page — asserted by comparing the
    /// two builders' tails rather than by re-listing the labels here.
    #[test]
    fn the_filmstrip_menu_is_the_state_group_alone() {
        let menu = build_episode("42", PosterMark::None);
        assert_eq!(labels(&menu), ["Mark as Watched", "Play from Start"]);
        assert_eq!(menu.ids().len(), 2, "no separator, so every row acts");
        // the shelf menu's episode rows END with exactly these two, in this order
        let shelf = build(&item(3, PosterMark::None), false);
        let shelf_labels = labels(&shelf);
        assert_eq!(labels(&menu), shelf_labels[shelf_labels.len() - 2..]);

        // a watched episode gets the way back instead, carrying its OWN rk (the menu captured its
        // target when it was presented — nothing here may resolve through a live focus index)
        let menu = build_episode("77", PosterMark::Watched);
        assert_eq!(labels(&menu), ["Mark as Unwatched", "Play from Start"]);
        match menu.action(ItemRow::MarkUnwatched) {
            Action::MarkUnwatched(rk) => assert_eq!(rk, "77"),
            _ => panic!("the unwatched row must commit MarkUnwatched"),
        }
        match menu.action(ItemRow::PlayFromStart) {
            Action::PlayFromStart(rk) => assert_eq!(rk, "77"),
            _ => panic!("Play from Start must commit PlayFromStart"),
        }
        // and nothing navigates: both rows the shelf menu offers an episode are dead ends from the
        // page that episode already belongs to
        assert!(!menu.offers(ItemRow::GoToShow) && !menu.offers(ItemRow::GoToItem));
    }

    /// **A menu never opens with its focus on a destructive action.** *Mark as Unwatched* throws the
    /// watch record away (and propagates to the other copies), so a watched episode's menu opens on
    /// *Play from Start*; a watched season, whose one row it is, still opens on it. *Remove from
    /// Deck* is last in the shelf menu and is tagged too, so no reorder can make it the opening row.
    #[test]
    fn a_menu_never_opens_on_a_destructive_row() {
        let opening = |menu: Built| {
            let id = *menu.0.id_at(menu.0.table.sel as usize).expect("the opening row acts");
            menu.action(id)
        };
        assert!(matches!(opening(build_episode("77", PosterMark::Watched)),
            Action::PlayFromStart(_)), "a watched episode opened on Mark as Unwatched");
        assert!(matches!(opening(build_season("78", PosterMark::Watched)),
            Action::MarkUnwatched(_)), "the only row is the one on offer");
        assert!(matches!(opening(build_episode("79", PosterMark::None)),
            Action::MarkWatched(_)));
        let menu = build(&item(3, PosterMark::None), true);
        assert!(menu.offers(ItemRow::RemoveFromDeck), "rig: a deck card");
        assert!(menu.is_destructive(ItemRow::RemoveFromDeck));
        assert!(menu.is_destructive(ItemRow::MarkUnwatched) || !menu.offers(ItemRow::MarkUnwatched));
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
            labels(&build(&item(3, mark), false)).split_off(3) // past "Go to Episode", "Go to Show", the separator
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
            labels(&build_episode("42", mark))
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
            let mc = serde_json::from_str::<plx_plex::plex::Envelope>(&body)
                .expect("parses")
                .media_container;
            plx_data::pms::parse_item(&mc.hub[0].metadata[0], plx_plex::plex::ServerId::UNSET)
        };
        let set = |json: &str| labels(&build(&row(json), false));

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
        let menu = build(&row(&movie("")), false);
        assert!(!labels(&menu).iter().any(|l| l == "Remove from Deck"));
        assert!(!menu.offers(ItemRow::RemoveFromDeck));
    }

    /// **A row performs the verb its own label names**, in every state and at both entry points —
    /// which is the assertion the old shape could not make at all: the action carried what the item
    /// WAS and `app.rs` inverted it, so "what this row does" lived in another file and, on the pair,
    /// would have been the same bool twice — one row doing its neighbour's write.
    #[test]
    fn every_watch_row_performs_the_verb_its_label_names() {
        use plx_data::viewstate::Write;
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
                    let menu = build(&item(kind, mark), deck);
                    for (label, got) in verbs(&menu) {
                        assert_eq!(got, want(&label), "{label:?} (kind {kind}, {mark:?})");
                    }
                }
            }
            let menu = build_episode("42", mark);
            for (label, got) in verbs(&menu) {
                assert_eq!(got, want(&label), "{label:?} on the filmstrip ({mark:?})");
            }
        }
    }

    /// Every row a menu draws is a row it can act on: a row and its action are declared in ONE
    /// place ([`ItemForm`]), so the shifted-neighbour failure the old parallel vector allowed —
    /// a row added without its action, every action below it moving up one — has no spelling left.
    /// States the property over every kind, state and entry point the builders serve: each drawn,
    /// non-separator row is bound, and its action names the item the menu is about.
    #[test]
    fn every_drawn_row_is_bound_to_an_action() {
        for mark in [
            PosterMark::None,
            PosterMark::InProgress,
            PosterMark::Watched,
        ] {
            for kind in [0, 1, 2, 3] {
                for deck in [false, true] {
                    let menu = build(&item(kind, mark), deck);
                    let drawn = menu.0.table.sections[0].rows.iter().filter(|r| !r.sep).count();
                    assert_eq!(menu.actions().len(), drawn, "kind {kind}, {mark:?}, deck {deck}");
                }
            }
            let menu = build_episode("42", mark);
            let drawn = menu.0.table.sections[0].rows.len();
            assert_eq!(menu.actions().len(), drawn, "filmstrip, {mark:?}");
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
        let screen = screen_with(super::build(&item(3, PosterMark::None), false));
        let key = |id: ItemRow| id.key().0;
        let step = |from: ItemRow, dir: Dir| {
            let from = key(from);
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
                Step::Move(k) => Some(k.elem),
                _ => None,
            }
        };
        use ItemRow::*;
        assert_eq!(step(GoToItem, Dir::Down), Some(key(GoToShow)));
        assert_eq!(step(GoToShow, Dir::Down), Some(key(MarkWatched))); // the separator is skipped
        assert_eq!(step(MarkWatched, Dir::Down), Some(key(PlayFromStart)));
        assert_eq!(step(PlayFromStart, Dir::Down), None); // stops at the end
        assert_eq!(step(MarkWatched, Dir::Up), Some(key(GoToShow))); // skipped back over the separator
        assert_eq!(step(GoToShow, Dir::Up), Some(key(GoToItem)));
        assert_eq!(step(GoToItem, Dir::Up), None); // stops at the start
    }

    /// A focus key that names no row settles onto the opening row — the ENGINE asks `reconcile`,
    /// which answers the opening row for anything that is not one. The separator has no key at
    /// all, so there is no element a cursor could land on it with.
    #[test]
    fn a_key_naming_no_row_settles_onto_a_real_row() {
        let screen = screen_with(super::build(&item(3, PosterMark::None), false));
        let want = FocusKey {
            entry: EntryId(7),
            elem: 0xdead,
        };
        let got =
            with_cx(|cx| <ItemMenuScreen as Focusable<HostFixture>>::reconcile(&screen, want, cx));
        assert_eq!(got.elem, ItemRow::GoToItem.key().0, "…the first real row is where it lands");
        let live = FocusKey { entry: EntryId(7), elem: ItemRow::MarkWatched.key().0 };
        let kept =
            with_cx(|cx| <ItemMenuScreen as Focusable<HostFixture>>::reconcile(&screen, live, cx));
        assert_eq!(kept.elem, live.elem, "a live row is left where it is");
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
        m.sid = plx_plex::plex::ServerId::from_raw(3);
        m.part = "/library/parts/42/file.mkv".to_string();

        let mut screen = ItemMenuScreen::new(EntryId(7), card_arg(&m, false));
        screen.build_rows(test_store().view());
        let elem = first_action(&screen, |a| matches!(a, Action::PlayFromStart(_)));
        let req = commit(&mut screen, elem);
        assert_eq!(
            req.sid,
            plx_plex::plex::ServerId::from_raw(3),
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
                sid: plx_plex::plex::ServerId::from_raw(3),
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
        strip.build_rows(test_store().view());
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
    fn translated_action_menus_measure_complete_verbs_and_keep_safe_anchors() {
        use plx_platform::i18n::{LocaleContext, Preference};
        use plx_machine::machine::Measure;
        struct MenuMeasure;
        impl Measure for MenuMeasure {
            fn width(&self, text: &std::ffi::CStr, size: i32, bold: bool) -> f32 {
                assert!(!bold, "compact action labels keep BODY regular");
                text.to_string_lossy().chars().count() as f32 * size as f32 * 0.6
            }
            fn cap_h(&self, size: i32) -> f32 { size as f32 * 0.7 }
            fn line_h(&self, size: i32) -> f32 { size as f32 * 1.2 }
        }
        let measure = MenuMeasure;
        for preference in [Preference::En, Preference::Es, Preference::Be] {
            let locale = LocaleContext::resolve(preference, None, None, None, None);
            let mut form: FormTable<ItemRow, Action, Infallible> = FormTable::new(crate::registry::BAND);
            form.table.compact = true;
            form.set(Form::new().section(FormSection::new("")
                .item(ItemRow::MarkWatched, RowKind::Button, Action::MarkWatched("1".into()),
                    Row::new(plx_platform::i18n::msg::widgets_action_mark_watched_in(&locale)).licon(Icon::CheckCircleFill))
                .item(ItemRow::MarkUnwatched, RowKind::Button, Action::MarkUnwatched("1".into()),
                    Row::new(plx_platform::i18n::msg::widgets_action_mark_unwatched_in(&locale)).licon(Icon::MinusCircleFill))
                .item(ItemRow::RemoveFromDeck, RowKind::Button, Action::RemoveFromDeck("1".into()),
                    Row::new(plx_platform::i18n::msg::browse_menu_remove_deck_in(&locale)).licon(Icon::Close))), None);
            let table = &form.table;
            let width = table.measured_width(&measure);
            if preference == Preference::Be {
                assert!(width > MENU_MIN_W, "the failing Belarusian menu must exercise growth");
            }
            for x in [MARGIN_X, (SCR_W - CARD_W) * 0.5, SCR_W - MARGIN_X - CARD_W] {
                let anchor = Rect::new(x, SCR_H - 120.0, CARD_W, CARD_H);
                let frame = panel_at(anchor, width, table.measured_height());
                assert!(plx_ui::consts::inside_safe(frame));
                assert!(frame.x >= anchor.x + anchor.w || frame.x + frame.w <= anchor.x,
                    "the measured menu must remain beside a visible card");
                for row in &table.sections[0].rows {
                    let label_width = measure.width_str(&row.label, plx_ui::theme::size::BODY, false);
                    assert!(label_width <= table.label_width(row, frame.w, &measure),
                        "{preference:?} action was elided: {}", row.label);
                }
            }
        }
    }

    #[test]
    fn the_panel_sits_beside_the_card_and_never_leaves_the_screen() {
        let h = 5.0 * 60.0; // a five-row menu, roughly
                            // a card on the left of the shelf: the panel sits to its RIGHT, clear of the card
        let r = panel_at(Rect::new(MARGIN_X, 300.0, CARD_W, CARD_H), MENU_MIN_W, h);
        assert!(
            r.x >= MARGIN_X + CARD_W,
            "expected the panel right of the card, got x={}",
            r.x
        );
        // a card at the right edge: it flips LEFT rather than running off screen — and lands clear
        // of the card it belongs to, which is the whole point of anchoring beside it
        let a = Rect::new(SCR_W - 300.0, 300.0, CARD_W, CARD_H);
        let r = panel_at(a, MENU_MIN_W, h);
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
        let low = panel_at(Rect::new(MARGIN_X, SCR_H - 120.0, CARD_W, CARD_H), MENU_MIN_W, h);
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
        let tall = panel_at(Rect::new(MARGIN_X, 300.0, CARD_W, CARD_H), MENU_MIN_W, 4000.0);
        for (what, p) in [("flipped", r), ("low", low), ("tall", tall)] {
            assert!(
                plx_ui::consts::inside_safe(p),
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
        let mut screen = screen_with(super::build(&item(3, PosterMark::None), false));
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

    use crate::registry::{AppMsg, PageMemory};
    use plx_ui::fixture::FixtureMeasure;
    use plx_machine::machine::{FocusRead, Host, InputEvent, InputOwner, PressRead, Source, Tick};
    use plx_ui::screen::ScreenArg;

    #[derive(Clone)]
    struct Arg;
    impl LogicalState for Arg {
        fn write(&self, _: &mut Canon) {}
        fn probe(&self, _: &mut String) {}
    }
    impl ScreenArg for Arg {
        fn chrome(&self) -> plx_machine::machine::Chrome {
            plx_machine::machine::Chrome::None
        }
        fn id(&self) -> plx_machine::machine::ScreenId {
            plx_machine::machine::ScreenId(1)
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
    thread_local! {
        // TEST ONLY: see `screens::detail::tests`'s `TEST_METADATA` for why this lives here
        // rather than being threaded as a parameter.
        static TEST_METADATA: std::cell::UnsafeCell<plx_data::stores::metadata::MetadataStore> =
            std::cell::UnsafeCell::new(plx_data::stores::metadata::MetadataStore::default());
    }

    fn test_store() -> &'static mut plx_data::stores::metadata::MetadataStore {
        TEST_METADATA.with(|cell| unsafe { &mut *cell.get() })
    }

    impl crate::registry::MetadataLike for HostFixture {
        fn metadata<'a>(_cx: &Cx<'a, Self>) -> plx_data::metadata::MetadataView<'a> {
            test_store().view()
        }
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
    fn screen_with(form: ItemForm) -> ItemMenuScreen {
        let mut s = ItemMenuScreen::new(EntryId(7), card_arg(&item(3, PosterMark::None), false));
        s.built = true;
        s.form.table.compact = true;
        s.form.set(form, None);
        s
    }

    /// The focus key of the first row whose action satisfies `want`.
    fn first_action(s: &ItemMenuScreen, want: impl Fn(&Action) -> bool) -> u32 {
        (0..s.form.table.n_rows() as usize)
            .find(|i| s.form.binding_at(*i).is_some_and(|b| want(&b.action)))
            .and_then(|i| s.form.key_at(i))
            .expect("the row set offers this action")
            .0
    }

    /// Commit a row and return the ONE request it emitted.
    fn commit(s: &mut ItemMenuScreen, elem: u32) -> ItemMenuReq {
        let mut out = Vec::new();
        let mut present = plx_machine::present::Present::new();
        let mut fx = Effects::new(
            &mut out,
            plx_machine::machine::MachineId::Instance(plx_machine::machine::InstanceId(8)),
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

    /// Step the screen with one event through a throwaway `Effects`.
    fn step_ev(s: &mut ItemMenuScreen, ev: &ScreenEvent<HostFixture>) -> Handled {
        let mut out = Vec::new();
        let mut present = plx_machine::present::Present::new();
        let mut fx = Effects::new(
            &mut out,
            plx_machine::machine::MachineId::Instance(plx_machine::machine::InstanceId(8)),
            &mut present,
        );
        with_cx(|cx| <ItemMenuScreen as Machine<HostFixture>>::step(s, ev, cx, &mut fx))
    }

    fn feed(s: &mut ItemMenuScreen, key: Key, edge: Edge, ms: u32) -> Handled {
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
        step_ev(s, &ev)
    }

    /// **Opening the menu from ANY card screen retires Home's "Hold OK for options" hint**: the
    /// mount is the one door every presenter goes through, so the lesson is marked there.
    #[test]
    fn mounting_the_menu_retires_the_hold_hint() {
        plx_ui::hold_hint::reset_learned_for_test();
        let mut screen = ItemMenuScreen::new(EntryId(7), card_arg(&item(0, PosterMark::None), false));
        assert!(!plx_ui::hold_hint::learned());
        step_ev(&mut screen, &ScreenEvent::Mount);
        assert!(plx_ui::hold_hint::learned());
        plx_ui::hold_hint::reset_learned_for_test();
    }

    fn extra() -> plx_data::metadata::Extra {
        plx_data::metadata::Extra {
            rk: "99".into(),
            part: "/library/parts/trailer".into(),
            vcodec: "h264".into(),
            acodec: "aac".into(),
            title: "Official Trailer".into(),
            subtype: "trailer".into(),
            extra_type: 1,
            dur_ms: 120_000,
            bitrate: 2_500,
            thumb: String::new(),
        }
    }

    #[test]
    fn a_movie_menu_without_a_trailer_keeps_today_s_labels() {
        let menu = build(&item(0, PosterMark::None), false);
        assert_eq!(
            labels(&menu),
            ["Go to Movie", "—", "Mark as Watched", "Play from Start"]
        );
        assert!(!menu.offers(ItemRow::PlayTrailer));
    }

    #[test]
    fn a_movie_menu_with_a_trailer_offers_play_trailer_for_the_extra() {
        let extra = extra();
        let movie = item(0, PosterMark::None);
        let menu = build_with(&movie, false, Some(&extra));
        assert!(labels(&menu).contains(&"Play Trailer".to_string()));
        match menu.action(ItemRow::PlayTrailer) {
            Action::PlayTrailer { rk, part, .. } => {
                assert_eq!(rk, "99");
                assert_eq!(part, "/library/parts/trailer");
                assert_ne!(rk.as_str(), movie.rk.as_str());
            }
            other => panic!("expected PlayTrailer, got {other:?}"),
        }
    }

    #[test]
    fn a_show_menu_with_a_trailer_offers_play_trailer() {
        let menu = build_with(&item(1, PosterMark::None), false, Some(&extra()));
        assert!(labels(&menu).contains(&"Play Trailer".to_string()));
    }

    #[test]
    fn episode_and_season_menus_never_offer_play_trailer() {
        for kind in [2, 3] {
            let menu = build_with(&item(kind, PosterMark::None), false, Some(&extra()));
            assert!(
                !labels(&menu).contains(&"Play Trailer".to_string()),
                "kind {kind}"
            );
            assert!(!menu.offers(ItemRow::PlayTrailer));
        }
    }

    #[test]
    fn play_trailer_is_cache_only_on_the_loaded_detail() {
        let _g = plx_base::testlock::serial();
        plx_data::metadata::set_current_for_test(test_store().state_mut(), None);
        assert!(
            cached_trailer(plx_plex::plex::ServerId::UNSET, &item(0, PosterMark::None), test_store().view()).is_none(),
            "no loaded Detail → no row"
        );

        plx_data::metadata::set_current_for_test(test_store().state_mut(), Some(plx_data::metadata::Detail {
            sid: plx_plex::plex::ServerId::UNSET,
            rk: "42".into(),
            kind: "movie".into(),
            extras: vec![extra()],
            ..Default::default()
        }));
        let hit = cached_trailer(plx_plex::plex::ServerId::UNSET, &item(0, PosterMark::None), test_store().view()).unwrap();
        assert_eq!(hit.rk, "99");

        let mut other = item(0, PosterMark::None);
        other.rk = "other".into();
        assert!(
            cached_trailer(plx_plex::plex::ServerId::UNSET, &other, test_store().view()).is_none(),
            "a related tile of a different item must not steal the loaded trailer"
        );
        plx_data::metadata::set_current_for_test(test_store().state_mut(), None);
    }

    #[test]
    fn play_trailer_falls_back_to_the_card_title() {
        let mut extra = extra();
        extra.title.clear();
        let mut movie = item(0, PosterMark::None);
        movie.title = "The Movie".into();
        let menu = build_with(&movie, false, Some(&extra));
        match menu.action(ItemRow::PlayTrailer) {
            Action::PlayTrailer { title, rk, part, .. } => {
                assert_eq!(title, "The Movie");
                assert_eq!(rk, "99");
                assert_eq!(part, "/library/parts/trailer");
            }
            other => panic!("expected PlayTrailer, got {other:?}"),
        }
    }

    #[test]
    fn play_trailer_action_carries_the_extra_identity() {
        let act = Action::PlayTrailer {
            rk: "9".into(),
            part: "/p".into(),
            vcodec: "h264".into(),
            acodec: "aac".into(),
            title: "T".into(),
        };
        assert_eq!(act.rk(), "9");
        match act {
            Action::PlayTrailer { part, title, .. } => {
                assert_eq!(part, "/p");
                assert_eq!(title, "T");
            }
            _ => unreachable!(),
        }
    }

    /// **Every action row of every item menu fits its panel, in every shipped language.** The
    /// panel grows to its content ([`panel_at`] clamps `measured_width` between [`MENU_MIN_W`] and
    /// [`MENU_MAX_W`]), so what this guards is the CEILING: no verb may be longer than the widest
    /// panel the screen will ever draw. Built through the real [`build_with`], [`build_episode`]
    /// and [`build_season`] over every item kind, watch state, Continue Watching and trailer
    /// combination, in the same compact table `build_rows` seats, measured with the device's
    /// whole-pixel advances. No row carries server text; the parent title only rides an action.
    #[test]
    fn every_action_row_fits_the_widest_panel_in_every_language() {
        use plx_platform::i18n::{language_on_this_thread_for_test, SHIPPED};
        // The panel hugs its content, so it always fits itself; the widest it may ever grow to
        // ([`MENU_MAX_W`], the shared cap) is the width a verb can actually be held to.
        let widest = MENU_MAX_W;
        let measure = plx_base::fontcov::advances::ShippedMeasure;
        let marks = [PosterMark::None, PosterMark::InProgress, PosterMark::Watched];
        let trailer = plx_data::metadata::Extra { rk: "9".into(), part: "/p".into(), ..Default::default() };
        // A row captures its text when it is BUILT, so the menus are rebuilt inside each language.
        let all_menus = || {
            let mut menus: Vec<(String, Built)> = Vec::new();
            for kind in 0..=3 {
                for mark in marks {
                    for from_deck in [false, true] {
                        for with_trailer in [false, true] {
                            let t = with_trailer.then_some(&trailer);
                            menus.push((format!("card kind={kind} {mark:?} deck={from_deck} trailer={with_trailer}"),
                                build_with(&item(kind, mark), from_deck, t)));
                        }
                    }
                }
            }
            for mark in marks {
                menus.push((format!("episode {mark:?}"), build_episode("1", mark)));
                menus.push((format!("season {mark:?}"), build_season("1", mark)));
            }
            menus
        };
        let mut out = Vec::new();
        for language in SHIPPED {
            let _guard = language_on_this_thread_for_test(language);
            for (name, menu) in all_menus() {
                let mut menu = menu;
                menu.0.table.compact = true;
                let table = &menu.0.table;
                out.extend(table.menu_cap_failure(&measure, &format!("{} {name}", language.tag())));
                out.extend(table.app_fit_failures(widest, &format!("{} {name}", language.tag())));
                out.extend(table.app_fit_failures_hugged(&format!("{} {name}", language.tag())));
            }
        }
        plx_ui::table::assert_no_fit_failures(&out);
    }
}
