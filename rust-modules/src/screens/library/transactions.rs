//! Semantic deferred transactions for the page and grid fade scopes.

use crate::browse::SecKind;
use crate::screens::registry::LibrarySectionIdentity;
use crate::stores::browse::ListingView;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct SectionTarget {
    pub epoch: u32,
    pub index: usize,
    pub identity: LibrarySectionIdentity,
    pub kind: SecKind,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct GridTarget {
    pub epoch: u32,
    pub sid: crate::plex::ServerId,
    pub section: i64,
    pub query: u32,
}

impl GridTarget {
    pub(super) fn from_view(view: ListingView<'_>) -> Option<Self> {
        let id = view.id()?;
        Some(Self { epoch: id.epoch, sid: id.sid, section: id.section, query: id.query })
    }

    pub(super) fn matches(&self, view: ListingView<'_>) -> bool {
        Self::from_view(view).as_ref() == Some(self)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum GridAction {
    Sort { key: String, desc: bool },
    Unwatched { desired: bool },
    Genre { id: Option<String> },
}

#[derive(Clone, Debug, Default)]
pub(super) struct PendingTransactions {
    section: Option<SectionTarget>,
    grid: Option<(GridTarget, GridAction)>,
}

impl PendingTransactions {
    pub(super) fn section(&self) -> Option<&SectionTarget> { self.section.as_ref() }
    pub(super) fn grid(&self) -> Option<&(GridTarget, GridAction)> { self.grid.as_ref() }

    pub(super) fn request_section(&mut self, target: SectionTarget) {
        self.section = Some(target);
    }

    pub(super) fn request_grid(&mut self, target: GridTarget, action: GridAction) {
        self.grid = Some((target, action));
    }

    pub(super) fn take_section(&mut self, epoch: u32) -> Option<SectionTarget> {
        self.section.take().filter(|target| target.epoch == epoch)
    }

    pub(super) fn take_grid(&mut self, view: ListingView<'_>) -> Option<GridAction> {
        self.grid.take().and_then(|(target, action)| target.matches(view).then_some(action))
    }

    pub(super) fn cancel(&mut self) {
        self.section = None;
        self.grid = None;
    }

    /// Leaving the page commits both semantic halves in deterministic page-then-grid order.
    pub(super) fn flush(&mut self) -> (Option<SectionTarget>, Option<(GridTarget, GridAction)>) {
        (self.section.take(), self.grid.take())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn section(index: usize) -> SectionTarget {
        SectionTarget {
            epoch: 7,
            index,
            identity: LibrarySectionIdentity {
                sid: crate::plex::ServerId::from_raw(2),
                key: 40 + index as i64,
            },
            kind: SecKind::Movie,
        }
    }

    fn grid(query: u32) -> GridTarget {
        GridTarget {
            epoch: 7,
            sid: crate::plex::ServerId::from_raw(2),
            section: 42,
            query,
        }
    }

    #[test]
    fn section_and_grid_transactions_coexist_and_flush_in_order() {
        let mut pending = PendingTransactions::default();
        pending.request_section(section(2));
        pending.request_grid(grid(11), GridAction::Unwatched { desired: true });
        let (page, query) = pending.flush();
        assert_eq!(page, Some(section(2)));
        assert_eq!(query, Some((grid(11), GridAction::Unwatched { desired: true })));
    }

    #[test]
    fn newest_request_supersedes_only_its_own_half() {
        let mut pending = PendingTransactions::default();
        pending.request_section(section(1));
        pending.request_grid(grid(9), GridAction::Genre { id: Some("7".into()) });
        pending.request_section(section(2));
        pending.request_grid(grid(9), GridAction::Sort { key: "titleSort".into(), desc: true });
        assert_eq!(pending.section(), Some(&section(2)));
        assert_eq!(pending.grid(), Some(&(grid(9), GridAction::Sort { key: "titleSort".into(), desc: true })));
    }

    #[test]
    fn stale_epoch_and_wrong_listing_target_are_refused() {
        let mut pending = PendingTransactions::default();
        pending.request_section(section(2));
        assert_eq!(pending.take_section(8), None);
        assert!(pending.section().is_none());
    }

    #[test]
    fn unwatched_action_records_desired_value_not_a_toggle() {
        assert_ne!(
            GridAction::Unwatched { desired: true },
            GridAction::Unwatched { desired: false }
        );
    }
}
