//! One value-chip model supplies both focus geometry and paint runs.
use std::ffi::{CStr, CString};
use super::*;
use crate::ui::value_chip::ValueChip;
/// One deliberate per-process leak for a translated fixed label (the language is decided once
/// at boot, so the answer never changes afterwards): the bytes behind a `t()` answer, terminated.
fn leak_cstr(s: &'static str) -> &'static std::ffi::CStr {
    let mut v = s.as_bytes().to_vec();
    v.push(0);
    std::ffi::CStr::from_bytes_with_nul(Box::leak(v.into_boxed_slice())).unwrap_or_default()
}


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

    // Chip titles translate (i18n), so they cannot stay c"" literals. The language is decided
    // once at boot (see `i18n`), so each label is built once per process: a deliberate one-off
    // leak, the same shape `screens::profiles` uses for its footer pill.
    fn sort_chip_name() -> &'static std::ffi::CStr {
        static S: std::sync::OnceLock<&'static std::ffi::CStr> = std::sync::OnceLock::new();
        *S.get_or_init(|| leak_cstr(crate::i18n::t("Sort")))
    }
    fn filter_chip_name() -> &'static std::ffi::CStr {
        static S: std::sync::OnceLock<&'static std::ffi::CStr> = std::sync::OnceLock::new();
        *S.get_or_init(|| leak_cstr(crate::i18n::t("Filter")))
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
        let (name, value) = if elem == SORT {
            let sort = match queued {
                Some(GridAction::Sort { key, .. }) => listing.sorts().iter().find(|sort| &sort.key == key),
                _ => listing.sorts().get(listing.sort_index()),
            };
            (Self::sort_chip_name(), sort.map_or(crate::i18n::t("Title"), |sort| sort.title.as_str()).to_owned())
        } else {
            let genre = match queued {
                Some(GridAction::Genre { id }) => id.as_ref().and_then(|id| listing.genres().iter().find(|genre| &genre.id == id)),
                _ => listing.genre(),
            }.map_or("All", |genre| genre.title.as_str());
            let unwatched = match queued {
                Some(GridAction::Unwatched { desired }) => *desired,
                _ => listing.unwatched(),
            };
            (Self::filter_chip_name(), match (genre, unwatched) {
                ("All", false) => crate::i18n::t("All").into(), ("All", true) => crate::i18n::t("Unwatched").into(),
                (genre, false) => genre.into(), (genre, true) => format!("{genre} · {}", crate::i18n::t("Unwatched")),
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
