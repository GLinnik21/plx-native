//! The **Collections page** ("Films > Collections", redesign spec "Collections page"): the library
//! grid in [`Listing::Collections`] mode (PMS `type=18`).
//!
//! The grid, the A-Z rail and the paging come from the ordinary library page; what this module owns
//! is what is *different* about the page, as plain values the screen reads at its hook sites:
//!
//! * the header row: a [`Crumb`] ("Films" link, a centred chevron, "Collections" on the selected
//!   plate) and, only when the server declares two or more sorts, one "Sort . value" control at
//!   the right edge ([`header`]);
//! * which of the library page's behaviours do not run here ([`syncs_section`]);
//! * the tile: no count badge, a caption of "N films" / "N shows" under the focused tile (the
//!   collection's name above it when Under posters > Title is on), and the smart mark
//!   ([`caption`], [`TileFacts`]);
//! * the empty page's sentence ([`empty_text`]).
//!
//! OK on a tile already opens the collection: `app::input::activate_card` routes a collection row
//! to `ContentArg::Collection` through `collection_content_arg`, whatever page the row is on.
use super::Listing;
use plx_data::stores::browse::SecKind;
use plx_platform::i18n::{msg, LocaleContext};
use plx_ui::breadcrumb::{Crumb, CrumbLayout};
use plx_ui::caption_policy::{caption_lines, CaptionFacts, CaptionPolicy, DetailsKind};
use plx_ui::page_header::{self, HeaderItem, PageHeader, PageHeaderSpec};
use plx_machine::machine::Measure;

/// The PMS library type the page lists (`/library/sections/{id}/all?type=18`).
pub const COLLECTIONS_TYPE: u32 = 18;

/// The grid geometry the page shares with the Library: tiles 250 x 375 on a 285 pitch, the first
/// row's top at y 244 and the second at 659 (375 + 40 of air under an unfocused row).
pub const TILE_W: f32 = 250.0;
pub const TILE_H: f32 = 375.0;
pub const TILE_PITCH: f32 = 285.0;
pub const GRID_TOP: f32 = 244.0;
pub const ROW2_TOP: f32 = 659.0;

/// A server that declares at least this many sorts for the listing gets the Sort control. PMS
/// declares only `titleSort` for `type=18` (and no descending key), so the control is absent today
/// and appears by itself if a future PMS adds more.
const SORTS_FOR_CONTROL: usize = 2;

/// Is the Sort control shown, given how many sorts the server declared for the collections listing?
pub fn sort_visible(declared_sorts: usize) -> bool {
    declared_sorts >= SORTS_FOR_CONTROL
}

/// Does the library page's section-switch sync (adopt the directory's current section, fade the
/// grid, re-key the viewport; `LibraryScreen::sync`) run for this listing? A pushed page is fixed
/// to the section that opened it: the directory's current section moving under it (a library
/// picked on the page below, a source change) must not re-seat it.
pub fn syncs_section(listing: Listing) -> bool {
    listing != Listing::Collections
}

/// The crumb's root word: the page below, by what the library holds.
pub fn root_label(kind: SecKind) -> &'static str {
    match kind {
        SecKind::Movie => msg::browse_kind_films(),
        SecKind::Show => msg::browse_kind_shows(),
    }
}

/// The header row of the page.
pub struct Header {
    pub crumb: Crumb,
    pub crumb_layout: CrumbLayout,
    /// The Sort control's laid-out rects, only when [`sort_visible`].
    pub sort: Option<page_header::ItemRect>,
}

/// What the focus row holds, in order: the crumb's root link, then the Sort control when present.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stop {
    Root,
    Sort,
}

impl Header {
    pub fn stops(&self) -> Vec<Stop> {
        let mut v = vec![Stop::Root];
        if self.sort.is_some() { v.push(Stop::Sort); }
        v
    }
}

/// Build the header for `kind` at `width`. `declared_sorts` is how many sorts the server declared
/// for `type=18`; `sort_value` the one in force (its title).
pub fn header(kind: SecKind, declared_sorts: usize, sort_value: &str, width: f32, measure: &dyn Measure) -> Header {
    let crumb = Crumb::new(root_label(kind), msg::browse_kind_collections());
    let crumb_layout = crumb.layout(measure);
    let sort = sort_visible(declared_sorts).then(|| {
        let item = HeaderItem {
            label: msg::browse_library_sort().to_owned(),
            value: Some(sort_value.to_owned()),
            fold_glyph: true,
            focusable: true,
            ..Default::default()
        };
        let spec = PageHeaderSpec { left: Vec::new(), overflow: 0, right: vec![item] };
        let page = PageHeader::new(spec, &[], measure);
        let layout = page.layout(width, measure);
        layout.items[0]
    });
    Header { crumb, crumb_layout, sort }
}

/// The facts of one collection row the tile and its caption read.
#[derive(Clone, Copy, Debug, Default)]
pub struct TileFacts<'a> {
    pub title: &'a str,
    /// `movie`, `show`, ... (PMS `subtype`); `None` = the server sent none.
    pub subtype: Option<&'a str>,
    /// The member count; 0 = unknown (an empty collection and an unread one look alike on the
    /// wire, so neither gets a count line).
    pub child_count: i64,
    pub smart: bool,
}

/// The policy a collection's caption follows: the stored Title switch is honoured; the details
/// line is always the member count unless the reader chose None (nothing opens). Genre, Video and
/// Age rating describe a film, not a collection, so they fall back to the count.
pub fn policy(stored: CaptionPolicy) -> CaptionPolicy {
    CaptionPolicy {
        title: stored.title,
        details: if stored.details == DetailsKind::None { DetailsKind::None } else { DetailsKind::YearLength },
    }
}

/// The focused tile's caption lines `[name, "N films"]` under `stored`. No count badge exists on
/// the page: this caption is the only place the size is told.
pub fn caption(stored: CaptionPolicy, facts: &TileFacts<'_>, locale: &LocaleContext) -> [Option<String>; 2] {
    let f = CaptionFacts {
        title: Some(facts.title.to_owned()),
        // An unknown subtype still takes the collection arm (a generic "N items").
        subtype: Some(facts.subtype.unwrap_or("").to_owned()),
        child_count: u32::try_from(facts.child_count).ok().filter(|n| *n > 0),
        ..Default::default()
    };
    caption_lines(&policy(stored), &f, locale)
}

/// The empty page's sentence, "No collections in {library}".
pub fn empty_text(library: &str) -> String {
    msg::collections_page_empty(library)
}

/// The smart mark's name, for the focus probe and any spoken description.
pub fn smart_label() -> &'static str {
    msg::collections_page_smart()
}

#[cfg(test)]
#[path = "collections_page_tests.rs"]
mod tests;
