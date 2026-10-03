//! One value-chip model supplies both focus geometry and paint runs.
use std::ffi::{CStr, CString};
use super::*;
use crate::ui::value_chip::ValueChip;

pub(super) struct Chip {
    pub name: &'static CStr,
    pub value: CString,
    pub note: Option<CString>,
}
impl Chip {
    pub fn width(&self, measure: &dyn plx_machine::machine::Measure) -> f32 {
        ValueChip::width(measure, self.name, &self.value, self.note.as_deref())
    }
}

impl LibraryScreen {
    /// Every library lists more than one type ([`crate::browse::LibraryType::offered`]), so TYPE
    /// always leads; FILTER goes while the listed type takes no filters (collections).
    pub(super) fn toolbar_elems(&self) -> &'static [u32] {
        if self.listed.filters() { &[TYPE, SORT, FILTER] } else { &[TYPE, SORT] }
    }

    pub(super) fn view_section<H: LibraryLike>(&self, cx: &Cx<'_, H>) -> Option<usize> {
        let directory = H::directory(cx);
        self.pending.section().filter(|target| Some(target.epoch) == directory.epoch())
            .map(|target| target.index).or_else(|| match self.wanted_kind {
                Some(kind) => directory.preferred(kind), None => directory.current(),
            })
            .filter(|index| *index < directory.sections().len())
    }

    pub(super) fn library_label<H: LibraryLike>(&self, section: usize, cx: &Cx<'_, H>) -> CString {
        let directory = H::directory(cx);
        let value = if let Some(section) = directory.sections().get(section) {
            let owner = section.sid.and_then(|sid| directory.sources().iter().find(|(id, _)| *id == sid))
                .map(|(_, source)| source.handle.as_str()).unwrap_or("");
            if owner.is_empty() { section.row.title.clone() } else { format!("{} {owner}", section.row.title) }
        } else {
            let total = self.view_section(cx).map(|section| directory.favorite_sections_for(section).count()).unwrap_or(0);
            format!("+{}", total.saturating_sub(self.libraries.len().saturating_sub(1)))
        };
        CString::new(value).unwrap_or_default()
    }

    pub(super) fn library_lays<H: LibraryLike>(&self, cx: &Cx<'_, H>) -> Vec<crate::ui::widgets::StripLay> {
        crate::ui::widgets::strip_layout_measured(
            self.libraries.iter().map(|(_, section)| self.library_label(*section, cx).to_string_lossy().into_owned()),
            MARGIN_X + crate::ui::widgets::STRIP_PAD, crate::ui::theme::size::BODY,
            crate::ui::widgets::STRIP_GAP_WIDE, cx.measure)
    }

    pub(super) fn toolbar_chip<H: LibraryLike>(&self, elem: u32, cx: &Cx<'_, H>) -> Chip {
        let listing = H::listing(cx);
        let queued = self.pending.grid().filter(|(target, _)| target.matches(listing)).map(|(_, action)| action);
        let (name, value) = if elem == TYPE {
            let kind = match queued {
                Some(GridAction::LibraryType(kind)) => *kind,
                _ => listing.library_type(),
            };
            (plx_platform::i18n::msg::browse_library_type_c(), kind.title(self.kind).to_owned())
        } else if elem == SORT {
            let sort = match queued {
                Some(GridAction::Sort { key, .. }) => listing.sorts().iter().find(|sort| &sort.key == key),
                _ => listing.sorts().get(listing.sort_index()),
            };
            (plx_platform::i18n::msg::browse_library_sort_c(), sort.map_or(plx_platform::i18n::msg::browse_library_title(), |sort| sort.title.as_str()).to_owned())
        } else {
            let genre = match queued {
                Some(GridAction::Genre { id }) => id.as_ref().and_then(|id| listing.genres().iter().find(|genre| &genre.id == id)),
                _ => listing.genre(),
            }.map(|genre| genre.title.as_str());
            let unwatched = match queued {
                Some(GridAction::Unwatched { desired }) => *desired,
                _ => listing.unwatched(),
            };
            (plx_platform::i18n::msg::browse_library_filter_c(), match (genre, unwatched) {
                (None, false) => plx_platform::i18n::msg::browse_library_all().into(),
                (None, true) => plx_platform::i18n::msg::browse_library_unwatched().into(),
                (Some(genre), false) => genre.into(),
                (Some(genre), true) => plx_platform::i18n::msg::browse_library_genre_unwatched(genre),
            })
        };
        Chip { name, value: CString::new(format!(" · {value}")).unwrap_or_default(), note: None }
    }

    pub(super) fn toolbar_chip_rect<H: LibraryLike>(&self, elem: u32, cx: &Cx<'_, H>, at: At) -> Rect {
        let x = MARGIN_X + self.toolbar_elems().iter().take_while(|&&key| key != elem)
            .map(|&key| self.toolbar_chip(key, cx).width(cx.measure) + 16.0).sum::<f32>();
        let (layout, scroll) = match at {
            At::Drawn => (&self.layout, self.scroll.pos),
            At::SpringTarget => (&self.target_layout, self.scroll_target),
        };
        Rect::new(x, CONTENT_TOP + layout.grid_block_top() + crate::ui::consts::TITLE_DY + CARD_DY - scroll,
            self.toolbar_chip(elem, cx).width(cx.measure), 52.0)
    }
}
