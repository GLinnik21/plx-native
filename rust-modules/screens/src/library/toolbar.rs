//! The header row's elements: the page's controls, the Collections control's state, and which
//! library the row names. The row's declaration is `header.rs`; its geometry, paint and motion are
//! `plx_ui::page_header`.
use super::*;

/// The header's controls. Their ids sit beside the other page controls (`SORT`, `FILTER`, ...).
pub(super) const COLLECTIONS: u32 = 8;
pub(super) const RECOMMENDED: u32 = 9;
pub(super) const SORTFILTER: u32 = 10;
/// The Collections page's crumb root ("Films"): OK returns to the page below.
pub(super) const CRUMB_ROOT: u32 = 11;

/// What the Collections control knows about its count (`LibraryLike::collections_count`).
///
/// **"Collections" is on the row only with its count** (the #843 spec: it always shows its count),
/// so only [`Self::Count`] puts it there. A bare "Collections" would be drawn for a library that has
/// none, for the frames until its probe answers, and then withdrawn: the flash this enum's two
/// other states prevent. The count the row holds is kept while the SAME library's probe reads
/// nothing again (`LibraryScreen::refresh_header`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub(super) enum CollectionsControl {
    /// Not probed yet (or the probe learned nothing): the control is NOT on the row, though its
    /// room in the cluster's budget is kept so the names do not repack when the count lands. A
    /// count that lands later fades in through the row's `HeaderSwap`.
    #[default]
    Unknown,
    Count(u32),
    /// The probe found none, or the server refused it (HTTP 403): the control is not on the row,
    /// though its room in the cluster's budget is kept, as for [`Self::Unknown`], so the names do
    /// not repack when one of the two turns into the other.
    Hidden,
}

impl CollectionsControl {
    pub(super) fn of(answer: Option<Result<u32, crate::registry::Forbidden>>) -> Self {
        match answer {
            None => Self::Unknown,
            Some(Ok(0) | Err(_)) => Self::Hidden,
            Some(Ok(n)) => Self::Count(n),
        }
    }
}

impl LibraryScreen {
    /// The controls on the row, left to right. The Collections control is on it only with a known
    /// count: an unknown or hidden one is not.
    pub(super) fn toolbar_elems(&self) -> &'static [u32] {
        // The Collections page's row: the crumb (its own element, `CRUMB_ROOT`) and, only when the
        // server declares two sorts, the one Sort control. No Collections, Recommended or menu.
        if self.listing == Listing::Collections { return if self.collections_sort { &[SORTFILTER] } else { &[] }; }
        if matches!(self.collections, CollectionsControl::Count(_)) { &[COLLECTIONS, RECOMMENDED, SORTFILTER] }
        else { &[RECOMMENDED, SORTFILTER] }
    }


    pub(super) fn view_section<H: LibraryLike>(&self, cx: &Cx<'_, H>) -> Option<usize> {
        let directory = H::directory(cx);
        self.pending.section().filter(|target| Some(target.epoch) == directory.epoch())
            .map(|target| target.index).or_else(|| match self.wanted_kind {
                Some(kind) => directory.preferred(kind), None => directory.current(),
            })
            .filter(|index| *index < directory.sections().len())
    }

    /// A library's name on the header row in its two pieces, borrowed from the directory: its title
    /// and its owner's handle when someone else's (empty when it is not). Everything
    /// [`Self::library_label`] reads, so a cache keyed on the pair cannot go stale.
    pub(super) fn library_name<'a, H: LibraryLike>(&self, section: usize, cx: &Cx<'a, H>) -> (&'a str, &'a str) {
        let directory = H::directory(cx);
        let Some(section) = directory.sections().get(section) else { return ("", "") };
        let owner = section.sid.and_then(|sid| directory.sources().iter().find(|(id, _)| *id == sid))
            .map(|(_, source)| source.handle.as_str()).unwrap_or("");
        (section.row.title.as_str(), owner)
    }

    /// A library's name on the header row: its title, and its owner's handle when someone else's.
    pub(super) fn library_label<H: LibraryLike>(&self, section: usize, cx: &Cx<'_, H>) -> String {
        let (title, owner) = self.library_name(section, cx);
        joined_name(title, owner)
    }
}

/// The two pieces of [`LibraryScreen::library_name`] as one name: the title, then the owner's handle
/// when there is one. The one place the join is spelled; the capsule builds it only when a piece changes.
pub(super) fn joined_name(title: &str, owner: &str) -> String {
    if owner.is_empty() { title.to_owned() } else { format!("{title} {owner}") }
}
