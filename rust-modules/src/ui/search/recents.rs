//! The empty-query state: the user's own recent searches, and the control that clears them.
//!
//! **STUB** — signatures final, bodies to come.
//!
//! ## The design
//!
//! Rows, not a shelf: nothing here has artwork. They use TableView's own geometry on the app's
//! ground instead of inside a panel — `--row-h` tall, `--panel-side` margins, `--row-content-pad`
//! inside, the focus pill inset by `--row-pill-inset`, HEADLINE labels. Written as markup rather
//! than mounted as a `TableView` for one reason, and it is worth keeping: **these rows are the
//! user's own words and have to stay editable in place.**
//!
//! Above them sits a section HEADER, not a heading — it names the source of the rows, so it sits a
//! full step BELOW their labels: CAPTION, caps, tertiary, `--row-header-h`, exactly as TableView
//! draws one. At HEADLINE it read as another row.
//!
//! Clearing is a **control, not another term**: it leaves the list and becomes a `Button`, so a
//! verb never sits in the same column as the words you searched for.
//!
//! ## Persistence
//!
//! The terms live in the session file beside the roster, behind `#[serde(default,
//! deserialize_with = "de_soft_vec")]` — a corrupt entry costs that entry, never the session. They
//! are the account's, so `search::reset()`'s caller drops them with everything else.
#![allow(dead_code)]

use crate::ui::search::View;
use crate::ui::Painter;

/// How many terms are stored (capped at [`super::MAX_RECENTS`] for display by the drawer, not
/// here — the store may hold more than the screen has room for).
pub(crate) fn count() -> usize {
    0
}

/// The terms, most recent first.
pub(crate) fn terms() -> Vec<String> {
    Vec::new()
}

/// Record a term that was actually searched. Moves an existing one to the front rather than
/// duplicating it.
pub(crate) fn remember(_term: &str) {}

pub(crate) fn clear() {}

pub(crate) fn draw(_p: Painter, _v: &View) {}
