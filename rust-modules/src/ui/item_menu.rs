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
//! **Two screens open it**, through two builders and one presenter: a home shelf card ([`open`]) and
//! the detail page's episode filmstrip ([`open_episode`], the owner-reported gap — a long press on
//! an episode still did nothing, so there was nowhere to mark an episode watched). The row SET
//! differs because a navigation row that leads to the page you are standing on is not an action;
//! everything else — the panel, the placement, the choreography, the state rows — is shared.
//!
//! Like [`crate::ui::account_menu`], this module only **reports** the chosen [`Action`]; `app.rs`
//! performs the routing, the server call and the hub refresh. It never mutates playback or PMS
//! state itself.
#![allow(non_upper_case_globals)]
use crate::pms::PmsMovie;
use crate::ui::consts::*;
use crate::ui::icons::Icon;
use crate::ui::popover::Popover;
use crate::ui::table::{Row, Section, TableView};
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
    /// toggle the server-side view state of `rk`; the bool is what it is NOW (true = watched)
    MarkWatched(String, bool),
    /// play this leaf ignoring its resume point
    PlayFromStart(String),
}

/// The panel's fixed width. Wide enough for "Mark as Unwatched" at the table's compact BODY size
/// without eliding, narrow enough that it never covers the neighbouring card it is anchored off.
const PANEL_W: f32 = 460.0;
/// The pinned ~20px corner radius.
const PANEL_RAD: f32 = 20.0;
/// Air between the focused card's drawn edge and the panel — one `space` rung, like every other gap.
const CARD_GAP: f32 = theme::space::MD;
/// Keep-out from the screen edges, so a shelf near the bottom still gets a fully-visible panel.
const EDGE: f32 = theme::space::XL;
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
/// The focused card's drawn rect at open time, in screen coords — what the panel anchors beside.
static mut ANCHOR: Rect = Rect::new(0.0, 0.0, 0.0, 0.0);

fn table() -> &'static mut TableView {
    unsafe { &mut *addr_of_mut!(TABLE) }
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

/// Open the menu for `m`, anchored beside `anchor` (the focused card's drawn rect; `None` centres
/// it, which is only reachable from the headless trigger).
pub(crate) fn open(m: &PmsMovie, anchor: Option<Rect>) {
    present(build(m), anchor);
}

/// Open the menu for the DETAIL page's focused episode still — the owner-reported gap (a long press
/// on an episode tile had no menu at all, so there was nowhere to mark one watched). Same panel,
/// same choreography, a shorter row set: see [`build_episode`].
pub(crate) fn open_episode(rk: &str, watched: bool, anchor: Option<Rect>) {
    present(build_episode(rk, watched), anchor);
}

/// Put a built row set on screen beside `anchor`. Shared by both entry points so the panel's
/// choreography — anchor fallback, compact rows, selection reset, the appear spring — is written
/// once and cannot drift between the screens the menu serves.
fn present((rows, a): (Section, Vec<Option<Action>>), anchor: Option<Rect>) {
    let fallback =
        Rect::new((SCR_W - CARD_W) * 0.5 - PANEL_W * 0.5, (SCR_H - CARD_H) * 0.5, CARD_W, CARD_H);
    unsafe { addr_of_mut!(ANCHOR).write(anchor.unwrap_or(fallback)) };
    *acts() = a;
    table().compact = true; // a short list of one-line actions — BODY labels, not menu-size HEADLINE
    table().set_sections(vec![rows], 0, false);
    pop().open();
}

pub(crate) fn close() {
    pop().close();
}

/// The rows, and the action each one commits. Order is the pinned design's:
/// navigation (`Go to Episode` · `Go to Show`) — separator — state (`Mark as Watched` ·
/// `Play from Start`), adapted per item kind (`PmsMovie::kind`: 0 movie / 1 show / 2 season /
/// 3 episode).
fn build(m: &PmsMovie) -> (Section, Vec<Option<Action>>) {
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
    // Watched state is EXACT for a leaf: `PmsMovie::unwatched` is `viewCount == 0` off the same
    // container the shelf was built from. For a show/season it is only "no leaf watched yet", which
    // cannot distinguish part-watched from fully-watched — so those keep the one-way "Mark as
    // Watched" rather than claim a toggle they can't compute. (Follow-up: carry viewedLeafCount /
    // leafCount on the catalog row and this becomes a toggle everywhere.)
    let sec = state_rows(sec, &mut acts, &m.rk, leaf && !m.unwatched, leaf);
    (sec, acts)
}

/// The state group every menu ends with: the watched toggle, then (for a leaf) Play from Start.
/// ONE builder, because these are the rows the menu exists for and they must read identically
/// wherever it is opened — the label flips on `watched`, so the row is an exact toggle rather than
/// two rows the user has to pick between.
fn state_rows(sec: Section, acts: &mut Vec<Option<Action>>, rk: &str, watched: bool, leaf: bool) -> Section {
    let mut sec = sec
        .row(Row::new(if watched { "Mark as Unwatched" } else { "Mark as Watched" }).licon(Icon::CheckCircle));
    acts.push(Some(Action::MarkWatched(rk.to_string(), watched)));
    if leaf {
        sec = sec.row(Row::new("Play from Start").licon(Icon::PlayStart));
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
/// showing — its still, title, full summary and air date are all right there — and reaching it
/// would REPLACE this page rather than stack on it (`detail::open_rk` re-mounts in place, as the
/// Related row does), so BACK would then land on Home instead of the season being browsed. A row
/// that navigates away from a page to show less of what that page already shows, and loses your
/// place doing it, is not an action.
///
/// `watched` is exact here — it comes off the episode's own `viewCount` in the loaded season — so
/// the toggle is a true toggle, and with no nav group there is no separator either (`build`'s rule:
/// the divider only exists when there is a group above it).
fn build_episode(rk: &str, watched: bool) -> (Section, Vec<Option<Action>>) {
    let mut acts: Vec<Option<Action>> = Vec::new();
    let sec = state_rows(Section::new(""), &mut acts, rk, watched, true);
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
    let x = if right + PANEL_W <= SCR_W - EDGE { right } else { a.x - CARD_GAP - PANEL_W };
    let x = x.clamp(EDGE, (SCR_W - EDGE - PANEL_W).max(EDGE));
    let y = a.y.clamp(EDGE, (SCR_H - EDGE - h).max(EDGE));
    Rect::new(x, y, PANEL_W, h)
}

fn panel_rect() -> Rect {
    panel_at(unsafe { addr_of!(ANCHOR).read() }, table().measured_height())
}

pub(crate) fn update(dt: f32) {
    if !is_open() {
        return;
    }
    pop().update(dt);
    let ph = panel_rect().h;
    table().update(dt, ph - crate::ui::table::PAD_V);
}

pub(crate) fn draw() {
    if !is_open() {
        return;
    }
    // light scrim (the shelf must stay readable behind it) + the shared appear fade, rising a
    // short beat into place off the card it belongs to
    let p = pop().painter(SCRIM_A, 14.0);
    let r = panel_rect();
    p.rect(r, PANEL_RAD, theme::PANEL_TOP, theme::PANEL_BOT, 0.0);
    table().draw(p, r);
}

// ---------------------------------------------------------------------------------------
#[cfg(test)]
mod tests {
    use super::*;

    fn item(kind: c_int, unwatched: bool) -> PmsMovie {
        let mut m = PmsMovie::default();
        m.rk = "42".to_string();
        m.kind = kind;
        m.unwatched = unwatched;
        m.show_rk = "7".to_string();
        m.season_index = 3;
        m
    }
    fn labels(sec: &Section) -> Vec<String> {
        sec.rows.iter().map(|r| if r.sep { "—".to_string() } else { r.label.clone() }).collect()
    }

    #[test]
    fn an_episode_offers_the_pinned_row_set_in_order() {
        let (sec, acts) = build(&item(3, true));
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
    fn a_watched_leaf_offers_the_reverse_toggle() {
        let (sec, acts) = build(&item(0, false)); // movie, viewCount >= 1
        assert_eq!(labels(&sec), ["Go to Movie", "—", "Mark as Unwatched", "Play from Start"]);
        match acts[2].as_ref().unwrap() {
            Action::MarkWatched(_, watched) => assert!(*watched),
            _ => panic!("row 2 must be the watched toggle"),
        }
        // a movie has no show, so no second navigation row
        assert!(!labels(&sec).contains(&"Go to Show".to_string()));
    }

    #[test]
    fn a_show_has_no_play_from_start_and_never_claims_a_toggle() {
        // `unwatched == false` on a show only means "some leaf was watched" — not fully watched,
        // so the row must stay the one-way "Mark as Watched".
        let (sec, acts) = build(&item(1, false));
        assert_eq!(labels(&sec), ["Go to Show", "—", "Mark as Watched"]);
        match acts[2].as_ref().unwrap() {
            Action::MarkWatched(_, watched) => assert!(!*watched),
            _ => panic!("row 2 must be the watched action"),
        }
    }

    #[test]
    fn a_row_whose_target_is_missing_is_not_offered_at_all() {
        // an episode hub row that arrived without a grandparentRatingKey: no "Go to Show" — the row
        // would resolve to an empty rk, i.e. a blocking fetch for nothing and a blank page
        let mut m = item(3, true);
        m.show_rk.clear();
        let (sec, acts) = build(&m);
        assert_eq!(labels(&sec), ["Go to Episode", "—", "Mark as Watched", "Play from Start"]);
        assert!(acts.iter().flatten().all(|a| !matches!(a, Action::GoToShow(..))));
        // and a SEASON with no parent has no navigation at all — so no leading separator either,
        // which would otherwise open the menu with a rule above its first row
        let mut s = item(2, true);
        s.show_rk.clear();
        let (sec, _) = build(&s);
        assert_eq!(labels(&sec), ["Mark as Watched"]);
    }

    /// The detail page's filmstrip menu: the state rows, no navigation group, and therefore no
    /// leading separator. It shares `state_rows` with the shelf menu, so the row the owner asked
    /// for cannot say one thing on Home and another on the episode page — asserted by comparing the
    /// two builders' tails rather than by re-listing the labels here.
    #[test]
    fn the_filmstrip_menu_is_the_state_group_alone() {
        let (sec, acts) = build_episode("42", false);
        assert_eq!(labels(&sec), ["Mark as Watched", "Play from Start"]);
        assert!(acts.iter().all(|a| a.is_some()), "no separator, so every row acts");
        // the shelf menu's episode rows END with exactly these two, in this order
        let (shelf, _) = build(&item(3, true));
        assert_eq!(labels(&sec), labels(&shelf)[shelf.rows.len() - 2..]);

        // a watched episode gets the reverse toggle, carrying its OWN rk (the menu captured its
        // target at open time — nothing here may resolve through a live focus index)
        let (sec, acts) = build_episode("77", true);
        assert_eq!(labels(&sec), ["Mark as Unwatched", "Play from Start"]);
        match acts[0].as_ref().unwrap() {
            Action::MarkWatched(rk, watched) => assert_eq!((rk.as_str(), *watched), ("77", true)),
            _ => panic!("row 0 must be the watched toggle"),
        }
        match acts[1].as_ref().unwrap() {
            Action::PlayFromStart(rk) => assert_eq!(rk, "77"),
            _ => panic!("row 1 must be Play from Start"),
        }
        // and nothing navigates: both rows the shelf menu offers an episode are dead ends from the
        // page that episode already belongs to
        assert!(!acts.iter().flatten().any(|a| matches!(a, Action::GoToShow(..) | Action::GoToItem(_))));
    }

    #[test]
    fn the_focus_walk_steps_over_the_separator_and_stops_at_the_ends() {
        let mut t = TableView::new();
        let (sec, _) = build(&item(3, true));
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
        let mut t = TableView::new();
        let (sec, _) = build(&item(3, true));
        t.set_sections(vec![sec], 2, false); // index 2 IS the separator
        assert_eq!(t.sel, 3);
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
        let r = panel_at(Rect::new(MARGIN_X, SCR_H - 120.0, CARD_W, CARD_H), h);
        assert!(r.y + r.h <= SCR_H - EDGE + 0.5, "panel ran off the bottom: y={} h={}", r.y, r.h);
        assert!(r.y >= EDGE - 0.5);
    }
}
