//! **The drill-in page stack** the player's popovers share (`docs/player-submenus.md`): what a
//! pushed page remembers of the page beneath it, so a pop brings that page back exactly as it was
//! left, plus the two small pieces every such popover needs around it — the title band's
//! pointer-only key and handing the leaving page's table to the slide.
//!
//! It owns no UI and no table: the popover keeps its own [`FormTable`] and calls
//! [`PageStack::push`] / [`PageStack::pop`] around the form operations (`FormTable::open` on a push,
//! `FormTable::restore` on a pop). [`crate::ui::track_menu::TrackMenuState`] (Subtitles' Style and
//! language pages) and [`crate::ui::more_menu::MoreMenuState`] (Quality) both stand on it, so the
//! two cannot drift on what a push saves or what the replay canon says about it.
//!
//! The root is the EMPTY stack, not a value of `P`.
use crate::ui::form::{FormTable, RowKey};
use crate::ui::geom::IndexElem;
use crate::ui::machine::{Canon, EntryId, FocusKey, GroupId, Host, Measure};
use crate::ui::panel_motion::PanelMotion;
use crate::ui::screen::{
    Activate, AxisMask, Dir, DrawFrame, EdgeRule, ElemKind, GroupKind, GroupSpec, Hover, Placed,
    Seat, Step, Stop,
};
use crate::ui::table::TableView;
use crate::ui::{Painter, Rect};

/// The pointer-only key of the page title band ("< STYLE", "< QUALITY"): OUTSIDE the form's key
/// range (at the ceiling), so it is never a row and never in the D-pad column. A click on it pops.
pub(crate) const TITLE_KEY: u32 = crate::ui::table_screen::BAND_BASE;

/// Is `elem` the title band's pointer-only key ([`TITLE_KEY`])?
pub(crate) fn is_title_key(elem: u32) -> bool {
    elem == TITLE_KEY
}

/// **What a pushed page remembers of the page beneath it**: the opener's id and the scroll the
/// list was left at, so a pop ([`FormTable::restore`]) brings the page back exactly as it was.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Saved<P, R> {
    /// The page this entry opened.
    pub(crate) page: P,
    /// The row that opened it, on the page beneath.
    pub(crate) return_id: R,
    pub(crate) scroll: f32,
}

/// The pages pushed above a popover's root, outermost first (empty = the root).
#[derive(Debug)]
pub(crate) struct PageStack<P, R> {
    saved: Vec<Saved<P, R>>,
}

impl<P: Copy, R> PageStack<P, R> {
    pub(crate) const fn new() -> Self {
        Self { saved: Vec::new() }
    }

    /// Open `page` above the current one, remembering its opener and the scroll it was left at.
    pub(crate) fn push(&mut self, page: P, return_id: R, scroll: f32) {
        self.saved.push(Saved { page, return_id, scroll });
    }

    /// Take the top page off; `None` at the root.
    pub(crate) fn pop(&mut self) -> Option<Saved<P, R>> {
        self.saved.pop()
    }

    /// The page showing, `None` at the root.
    pub(crate) fn top(&self) -> Option<P> {
        self.saved.last().map(|s| s.page)
    }

    /// The first page pushed (what a pop-to-root restores), `None` at the root.
    pub(crate) fn first(&self) -> Option<&Saved<P, R>> {
        self.saved.first()
    }

    pub(crate) fn clear(&mut self) {
        self.saved.clear();
    }

    pub(crate) fn len(&self) -> usize {
        self.saved.len()
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.saved.is_empty()
    }

    pub(crate) fn iter(&self) -> impl Iterator<Item = &Saved<P, R>> {
        self.saved.iter()
    }

    /// The stack's part of a popover's replay canon: the depth, then each page's stable `code` and
    /// its opener's key.
    pub(crate) fn canon(&self, c: &mut Canon, code: impl Fn(P) -> u32, key: impl Fn(&R) -> u32) {
        c.u32(self.saved.len() as u32);
        for s in &self.saved {
            c.u32(code(s.page)).u32(key(&s.return_id));
        }
    }
}

/// Take the page that is showing OUT of `form`, whole, leaving a blank table of the same kind for
/// the next page to be built into: the page slide draws the old one once more
/// ([`crate::ui::panel_motion::PanelMotion::begin_slide`]), so it is moved, never cloned.
pub(crate) fn leave_page<Id, A, D>(form: &mut FormTable<Id, A, D>) -> TableView
where
    Id: PartialEq + Clone,
    A: Clone,
    D: Clone,
{
    let blank = form.table.blank_like();
    std::mem::replace(&mut form.table, blank)
}

/// **What a popover must expose for its focus stops to be shared**: the form it shows, its
/// [`PanelMotion`], and the two rects the card is laid out and drawn at. The
/// `Focusable`/`Part` impls of [`crate::ui::track_menu::TrackMenuPart`] and
/// [`crate::ui::more_menu::MoreMenuPart`] differ only in `reconcile`; everything else is the
/// `popover_*` functions below, so the two cannot drift on where a stop is, what it clips to, or
/// which way a key steps.
pub(crate) trait PopoverPanel {
    type Id: PartialEq + Clone;
    type Act: Clone;
    type Dest: Clone;
    fn form(&self) -> &FormTable<Self::Id, Self::Act, Self::Dest>;
    fn motion(&self) -> &PanelMotion;
    /// The panel's natural (layout) rect.
    fn panel_rect(&self, measure: &dyn Measure) -> Rect;
    /// The card as drawn this frame.
    fn shown_rect(&self, measure: &dyn Measure) -> Rect;
}

/// The popover's one `Column` group: UP/DOWN stop at the ends; LEFT (pop a page) and RIGHT (enter
/// a Nav row) are the owning screen's (`PlayerOverlayScreen::edge_key`), so both are
/// [`EdgeRule::Screen`].
pub(crate) fn popover_groups<S: PopoverPanel>(
    s: &S,
    group: GroupId,
    measure: &dyn Measure,
    out: &mut Vec<GroupSpec>,
) {
    out.push(GroupSpec {
        id: group,
        kind: GroupKind::Column,
        seat: Seat::Remembered,
        reachable: AxisMask::VERTICAL,
        edge: [EdgeRule::Stop, EdgeRule::Stop, EdgeRule::Screen, EdgeRule::Screen],
        extent: s.panel_rect(measure),
        len: s.form().focusable_len(),
        elem: ElemKind::Bare,
    });
}

/// The group a row key belongs to (none for the title band's pointer-only key).
pub(crate) fn popover_group_of<S: PopoverPanel, E: IndexElem>(
    s: &S,
    key: &E,
    group: GroupId,
) -> Option<GroupId> {
    s.form().index_of_key(RowKey(key.index()?)).map(|_| group)
}

/// UP/DOWN step through the form's focusable rows; LEFT/RIGHT are the screen's.
pub(crate) fn popover_neighbour<S: PopoverPanel, E: IndexElem>(
    s: &S,
    entry: EntryId,
    key: FocusKey<E>,
    dir: Dir,
) -> Step<E> {
    let Some(from) = key.elem.index() else {
        return Step::Edge;
    };
    let delta = match dir {
        Dir::Up => -1,
        Dir::Down => 1,
        _ => return Step::Edge,
    };
    match s.form().step_key(RowKey(from), delta) {
        Some(k) => Step::Move(FocusKey { entry, elem: E::of_index(k.0) }),
        None => Step::Edge,
    }
}

/// Where a stop is DRAWN: the page's slide offset on x and the animated card as the clip, the same
/// numbers [`popover_register_stops`] registers (replay compares the two). `rest_rect` is the
/// layout, where the row settles. The title band's pointer-only key is placed too, because replay
/// and hit validation must be able to.
pub(crate) fn popover_place<S: PopoverPanel, E: IndexElem>(
    s: &S,
    key: &E,
    measure: &dyn Measure,
) -> Option<Placed> {
    let natural = s.panel_rect(measure);
    let clip = s.shown_rect(measure);
    let dx = s.motion().live_dx();
    let placed = |rest: Rect, index: Option<u32>| Placed {
        rect: Rect::new(rest.x + dx, rest.y, rest.w, rest.h),
        rest_rect: rest,
        clip,
        index,
    };
    if key.index() == Some(TITLE_KEY) {
        return Some(placed(s.form().table.title_rect(natural)?, None));
    }
    let i = s.form().index_of_key(RowKey(key.index()?))? as u32;
    Some(placed(s.form().table.row_frame(natural, i as i32)?, Some(i)))
}

/// The key focus seats on when the group is entered: the cursor's row, else the opening row.
pub(crate) fn popover_seat<S: PopoverPanel, E: IndexElem>(s: &S, entry: EntryId) -> FocusKey<E> {
    let key = s.form().selected_key().or_else(|| s.form().opening_key());
    FocusKey { entry, elem: E::of_index(key.map_or(0, |k| k.0)) }
}

/// Register every selectable row's stop (a hover parks, a click activates) and the title band's
/// pointer-only stop (no hover focus, never in the D-pad column; a click pops). Only the ACTIVE
/// page registers, where it is drawn this frame: the slide's x offset and the animated card as the
/// clip; the pointer is held while either moves.
pub(crate) fn popover_register_stops<S: PopoverPanel, H: Host>(
    s: &S,
    entry: EntryId,
    f: &mut DrawFrame<'_, '_, H>,
) where
    H::Elem: IndexElem,
{
    let p = Painter::root();
    let r = s.panel_rect(f.measure);
    let dx = s.motion().live_dx();
    let clip = s.shown_rect(f.measure);
    let moved = |rect: Rect| Rect::new(rect.x + dx, rect.y, rect.w, rect.h);
    let form = s.form();
    for i in 0..form.table.n_rows() {
        let Some(key) = form.key_at(i as usize) else {
            continue;
        };
        if let Some(row) = form.table.row_frame(r, i) {
            f.stop(
                p,
                Stop {
                    key: FocusKey { entry, elem: H::Elem::of_index(key.0) },
                    rect: moved(row),
                    rest_rect: row,
                    clip,
                    hover: Hover::Focus,
                    activate: Activate::Direct,
                },
            );
        }
    }
    if let Some(band) = form.table.title_rect(r) {
        f.stop(
            p,
            Stop {
                key: FocusKey { entry, elem: H::Elem::of_index(TITLE_KEY) },
                rect: moved(band),
                rest_rect: band,
                clip,
                hover: Hover::Ignore,
                activate: Activate::Direct,
            },
        );
    }
}
