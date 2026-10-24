//! **The title of a page, as the user sees it on that page** — the one table a page that names
//! where BACK goes reads (the Collection screen's "‹ Home").
//!
//! The container says which KIND of page sits beneath the one being built
//! ([`plx_machine::machine::Host::views_over`] folds the entry beneath it into
//! [`AppViews::beneath`]); this turns that kind into the words the page wears: Home, the library's
//! own name, Collections, Recommended, Watchlist, Search, an item's title for a Detail. A page the
//! table does not name answers `None`, and the screen draws no caption rather than a wrong one. The
//! strings are the ones those pages already show (one catalog entry each, never a second
//! spelling), borrowed from the catalog or from the views, so a frame allocates nothing for it.

use std::borrow::Cow;
use plx_machine::machine::ScreenId;
use plx_platform::i18n::msg;
use plx_screens::registry::page_id;
use super::bridge::AppViews;

/// The title of the `page` kind of page, read from `views` (the library's name and an item's title
/// are data, not catalog strings).
pub(crate) fn of<'a>(page: ScreenId, views: &AppViews<'a>) -> Option<Cow<'a, str>> {
    let name: &'a str = match page {
        page_id::HOME => msg::browse_chrome_home(),
        page_id::SEARCH => msg::browse_chrome_search(),
        page_id::WATCHLIST => msg::watchlist_page_title(),
        page_id::LIBRARY_COLLECTIONS => msg::browse_kind_collections(),
        page_id::LIBRARY_RECOMMENDED => msg::library_header_recommended(),
        page_id::LIBRARY => {
            let directory = views.directory;
            directory.current().and_then(|i| directory.sections().get(i)).map(|section| section.row.title.as_str())?
        }
        page_id::DETAIL => views.metadata.current().map(|detail| detail.title.as_str())?,
        _ => return None,
    };
    (!name.is_empty()).then_some(Cow::Borrowed(name))
}
