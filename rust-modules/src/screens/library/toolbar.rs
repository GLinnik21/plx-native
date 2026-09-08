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
    pub fn width(&self, measure: &dyn crate::ui::machine::Measure) -> f32 {
        ValueChip::width(measure, self.name, &self.value, self.note.as_deref())
    }
}

impl LibraryScreen {
    pub(super) fn source_chip<H: LibraryLike>(&self, cx: &Cx<'_, H>) -> Option<Chip> {
        if self.libraries.len() != 1 { return None; }
        let directory = H::directory(cx);
        let section = self.pending.section().map(|target| target.index)
            .or_else(|| self.libraries.first().map(|(_, section)| *section))?;
        let section = directory.sections().get(section)?;
        let owner = section.sid.and_then(|sid| directory.sources().iter().find(|(id, _)| *id == sid))
            .map(|(_, source)| source.handle.as_str()).unwrap_or("");
        Some(Chip { name: c"Library",
            value: CString::new(format!(" · {}", section.row.title)).unwrap_or_default(),
            note: (!owner.is_empty()).then(|| CString::new(format!("  {owner}")).unwrap_or_default()) })
    }

    pub(super) fn toolbar_chip<H: LibraryLike>(&self, elem: u32, cx: &Cx<'_, H>) -> Chip {
        let listing = H::listing(cx);
        let queued = self.pending.grid().filter(|(target, _)| target.matches(listing)).map(|(_, action)| action);
        let (name, value) = if elem == SORT {
            let sort = match queued {
                Some(GridAction::Sort { key, .. }) => listing.sorts().iter().find(|sort| &sort.key == key),
                _ => listing.sorts().get(listing.sort_index()),
            };
            (c"Sort", sort.map_or("Title", |sort| sort.title.as_str()).to_owned())
        } else {
            let genre = match queued {
                Some(GridAction::Genre { id }) => id.as_ref().and_then(|id| listing.genres().iter().find(|genre| &genre.id == id)),
                _ => listing.genre(),
            }.map_or("All", |genre| genre.title.as_str());
            let unwatched = match queued {
                Some(GridAction::Unwatched { desired }) => *desired,
                _ => listing.unwatched(),
            };
            (c"Filter", match (genre, unwatched) {
                ("All", false) => "All".into(), ("All", true) => "Unwatched".into(),
                (genre, false) => genre.into(), (genre, true) => format!("{genre} · Unwatched"),
            })
        };
        Chip { name, value: CString::new(format!(" · {value}")).unwrap_or_default(), note: None }
    }

    pub(super) fn toolbar_chip_rect<H: LibraryLike>(&self, elem: u32, cx: &Cx<'_, H>, at: At) -> Rect {
        let x = MARGIN_X + if elem == FILTER { self.toolbar_chip(SORT, cx).width(cx.measure) + 16.0 } else { 0.0 };
        let (layout, scroll) = match at {
            At::Drawn => (&self.layout, self.scroll.pos),
            At::SpringTarget => (&self.target_layout, self.scroll_target),
        };
        Rect::new(x, CONTENT_TOP + layout.grid_block_top() + crate::ui::consts::TITLE_DY + CARD_DY - scroll,
            self.toolbar_chip(elem, cx).width(cx.measure), 52.0)
    }
}
