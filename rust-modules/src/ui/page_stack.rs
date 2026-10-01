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
use crate::ui::form::FormTable;
use crate::ui::machine::Canon;
use crate::ui::table::TableView;

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
