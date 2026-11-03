//! The **Collections page** ("{library} > Collections", redesign spec "Collections page"): the library
//! grid in [`Listing::Collections`] mode. The page asks the store for the section's collections
//! (`LibraryType::Collections`, PMS `type=18`) when it mounts and puts the section back on its own
//! type when it is left (`LibraryScreen::collections_listing`).
//!
//! The grid, the A-Z rail and the paging come from the ordinary library page; what this module owns
//! is what is *different* about the page, as plain values the screen reads:
//!
//! * the header row: a [`Crumb`] (the library's own name as the link, a centred chevron, "Collections" on the selected
//!   plate) and, only when the server declares two or more sorts, one "Sort . value" control at
//!   the right edge ([`header_spec`], built into the shared `PageHeader` by `header.rs`);
//! * which of the library page's behaviours do not run here ([`syncs_section`], read by
//!   `LibraryScreen::sync`);
//! * the tile: no count badge, and the smart mark; its caption is the ONE rule every collection tile
//!   reads on every surface (`registry::captions::label`: the Under posters policy, "N films" as the
//!   details line under every kind but Nothing, a name the face cuts riding that line), in the
//!   policy's own band like every poster;
//! * the empty page's sentence ([`empty_text`], read by the status read-out).
//!
//! OK on a tile already opens the collection: `app::input::activate_card` routes a collection row
//! to `ContentArg::Collection` through `collection_content_arg`, whatever page the row is on.
use super::Listing;
use plx_data::browse::LibraryType;
use plx_data::stores::browse::SecKind;
use plx_platform::i18n::msg;
use plx_ui::breadcrumb::Crumb;
use plx_ui::page_header::PageHeaderSpec;

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

/// Is the page showing a listing that is not its own? The Collections page BORROWS the section's one
/// store listing (`LibraryScreen::follow_listing_type`), so when it mounts the store still holds the
/// page beneath it, the library's films (`listed` is that listing's type), until the answer to the
/// page's own type-18 request is committed and lands. Until then the page has no content: it holds
/// its grid empty under the loading read-out, the way a library switch does, instead of wearing the
/// parent's tiles under its own crumb for as long as the round trip takes.
pub fn awaits_own_listing(listing: Listing, listed: LibraryType) -> bool {
    listing == Listing::Collections && listed != LibraryType::Collections
}

/// The crumb's root word: the library's own name, the one the header strip and the scroll capsule
/// show ("Movies", "Anime Films"). `library` is empty only when the directory has no section to
/// name (nothing to be on); the page then falls back to what the library holds.
pub fn root_label(library: &str, kind: SecKind) -> String {
    if !library.is_empty() { return library.to_owned(); }
    match kind {
        SecKind::Movie => msg::browse_kind_films(),
        SecKind::Show => msg::browse_kind_shows(),
    }.to_owned()
}

/// The header row of the page, as the declaration `plx_ui::page_header::PageHeader` lays out and
/// paints: the crumb (the library's name as the link, the centred chevron, "Collections" on the
/// selected plate) and the Sort control at the right edge when the server declares two or more
/// sorts ([`sort_visible`]). `library` is the root word's source ([`root_label`]); `sort_value`
/// the sort in force (its title). The row itself, its motion and its focus group are the ui's:
/// this is the page's declaration of what is on it, and nothing else.
pub fn header_spec(library: &str, kind: SecKind, declared_sorts: usize, sort_value: &str) -> PageHeaderSpec {
    let right = if sort_visible(declared_sorts) {
        vec![plx_ui::page_header::HeaderItem::sort_control(msg::browse_library_sort(), sort_value)]
    } else { Vec::new() };
    PageHeaderSpec { crumb: Some(Crumb::new(root_label(library, kind), msg::browse_kind_collections())), right, ..Default::default() }
}

/// The empty page's sentence, "No collections in {library}".
pub fn empty_text(library: &str) -> String {
    msg::collections_page_empty(library)
}

#[cfg(test)]
#[path = "collections_page_tests.rs"]
mod tests;
