//! Client-side sort and filter over stored rows, and the title comparator (plan §1.3, §1.10).
//! No request is made on a Sort & Filter change.
use super::model::{Kind, WatchRow};
use std::cmp::Ordering;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SortKey {
    /// `titleSort` else `title`, via [`compare_titles`].
    Title,
    /// Server order is newest-first; ascending is its exact reverse (never `watchlistedAt:asc`).
    DateAdded,
    /// `originallyAvailableAt`; a missing date sorts last in both directions.
    Release,
    /// A missing rating sorts last in both directions.
    Rating,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SortSpec {
    pub key: SortKey,
    pub desc: bool,
}

/// The "Show" filter: which rows by availability. `Available` is "On your servers".
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Show {
    #[default]
    All,
    Available,
    Unavailable,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct FilterSpec {
    pub kind: Option<Kind>,
    pub show: Show,
}

/// Title order: Unicode-lowercase, fold only marks that are not letters in any shipped language
/// (e-acute to e, a-grave to a, c-cedilla to c, oe-ligature to oe); n-tilde, short-u (Be) and
/// short-i (Be) stay distinct (after n, u, i); then the unfolded string, then the hex. No new
/// dependency. Operates on `titleSort` else `title`.
pub fn compare_titles(a: &WatchRow, b: &WatchRow) -> Ordering {
    let _ = (a, b);
    unimplemented!("W2a")
}

/// Indices into `rows`, filtered then sorted. `available` answers the Show filter from the
/// committed tile state (row index to "playable on a Reachable server"); it is computed at commit,
/// never per frame.
pub fn sorted_filtered(rows: &[WatchRow], sort: &SortSpec, filter: &FilterSpec, available: &dyn Fn(usize) -> bool) -> Vec<usize> {
    let _ = (rows, sort, filter, available);
    unimplemented!("W2a")
}
