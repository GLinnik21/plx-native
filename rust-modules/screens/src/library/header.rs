//! The library page header (redesign spec, "Header"): the library names, the "+N" overflow and the
//! page's three controls on the ui [`PageHeader`], one focus row at y 148.
//!
//! The row is built here, once per sync, and cached with its geometry (`Header`): placement, hit
//! rects, the focus order and the paint all read that one value, so they cannot disagree. The
//! names are packed against the room the right cluster leaves (`PageHeader::layout`'s
//! `left_budget`), so the cluster never moves when a name or a value changes.
use super::*;
use plx_ui::page_header::{self, HeaderItem, HeaderLayout, PageHeader, PageHeaderSpec};

/// The row as last built: the ui widget, its geometry (document space: y is the row's own top,
/// 148, before the scroll), and which element each focusable item is.
pub(super) struct Header {
    page: PageHeader,
    layout: HeaderLayout,
    /// `(element, index into layout.items)` of every focusable item, in focus order.
    stops: Vec<(u32, usize)>,
    /// The Collections page's row: the crumb in place of the library names. `None` on every
    /// other page.
    pub(super) crumb: Option<(plx_ui::breadcrumb::Crumb, plx_ui::breadcrumb::CrumbLayout)>,
}

impl Header {
    /// The plate (focus and hit rect) of `elem` in document space.
    pub(super) fn plate(&self, elem: u32) -> Option<Rect> {
        if elem == CRUMB_ROOT { return self.crumb.as_ref().map(|(_, layout)| layout.root_plate); }
        let (_, index) = self.stops.iter().find(|(key, _)| *key == elem)?;
        Some(self.layout.items[*index].plate)
    }

}

/// Which of `n` names show when the row cannot hold them all (`budget` is text edge to text edge
/// for names plus "+N"). The picked library takes the LAST visible slot: names keep their order
/// from the front, and a selected one past the cut replaces the final name. Always at least the
/// selected name; every name when they all fit under `cap`.
pub(super) fn pack_names(widths: &[f32], selected: usize, budget: f32, more_w: f32, cap: usize) -> Vec<usize> {
    let n = widths.len();
    if n == 0 || cap == 0 { return Vec::new(); }
    let selected = selected.min(n - 1);
    let gap = page_header::GAP;
    let span = |picks: &[usize]| picks.iter().map(|&i| widths[i]).sum::<f32>()
        + gap * picks.len().saturating_sub(1) as f32;
    let all: Vec<usize> = (0..n).collect();
    if n <= cap && span(&all) <= budget { return all; }
    // "+N" takes the end of the row: names get what is left of the budget after it and one gap.
    let room = budget - more_w - gap;
    for k in (1..n.min(cap)).rev() {
        let mut picks: Vec<usize> = (0..k).collect();
        if selected >= k { picks[k - 1] = selected; }
        if span(&picks) <= room { return picks; }
    }
    vec![selected]
}

fn item(label: impl Into<String>) -> HeaderItem {
    HeaderItem { label: label.into(), focusable: true, ..Default::default() }
}

impl LibraryScreen {
    /// Every focusable element of the header row in focus order: the library names, "+N", then
    /// the controls.
    pub(super) fn header_elems(&self) -> Vec<u32> {
        if self.listing == Listing::Collections {
            return [CRUMB_ROOT].into_iter().chain(self.toolbar_elems().iter().copied()).collect();
        }
        self.libraries.iter().map(|(elem, _)| *elem).chain(self.toolbar_elems().iter().copied()).collect()
    }

    pub(super) fn is_header_elem(&self, elem: u32) -> bool {
        if self.listing == Listing::Collections { return elem == CRUMB_ROOT || self.toolbar_elems().contains(&elem); }
        self.libraries.iter().any(|(key, _)| *key == elem) || self.toolbar_elems().contains(&elem)
    }

    /// The Sort & Filter control's value: the sort in force, a queued edit included.
    fn sort_value<H: LibraryLike>(&self, cx: &Cx<'_, H>) -> String {
        let listing = H::listing(cx);
        let queued = self.pending.grid().filter(|(target, _)| target.matches(listing)).map(|(_, action)| action);
        let sort = match queued {
            Some(GridAction::Sort { key, .. }) => listing.sorts().iter().find(|sort| &sort.key == key),
            _ => listing.sorts().get(listing.sort_index()),
        };
        sort.map_or(plx_platform::i18n::msg::browse_library_title(), |sort| sort.title.as_str()).to_owned()
    }

    /// Is a genre or Unwatched filter in force (a queued edit included)? Only then does the row
    /// carry the "312 of 1,284" read-out.
    pub(super) fn filter_active<H: LibraryLike>(&self, cx: &Cx<'_, H>) -> bool {
        let listing = H::listing(cx);
        let queued = self.pending.grid().filter(|(target, _)| target.matches(listing)).map(|(_, action)| action);
        let genre = match queued {
            Some(GridAction::Genre { id }) => id.is_some(),
            _ => listing.genre().is_some(),
        };
        let unwatched = match queued {
            Some(GridAction::Unwatched { desired }) => *desired,
            _ => listing.unwatched(),
        };
        genre || unwatched
    }

    /// Remember the total of the unfiltered listing the page last saw: the data layer publishes
    /// only the filtered total, and the read-out needs both.
    fn note_unfiltered<H: LibraryLike>(&mut self, cx: &Cx<'_, H>) {
        let listing = H::listing(cx);
        let (Some(id), Some(section)) = (listing.id(), self.section.clone()) else { return };
        if listing.total() < 0 || listing.fetch() != SecFetch::Ready { return; }
        let unfiltered = !listing.unwatched() && listing.genre().is_none();
        let key = (section, id.epoch, listing.library_type());
        if unfiltered {
            self.unfiltered = Some((key, listing.total()));
        } else if self.unfiltered.as_ref().is_some_and(|(seen, _)| *seen != key) {
            self.unfiltered = None;
        }
    }

    /// "312 of 1,284", once a filter is on and the unfiltered total is known.
    fn filtered_readout<H: LibraryLike>(&self, cx: &Cx<'_, H>) -> Option<String> {
        if !self.filter_active(cx) { return None; }
        let listing = H::listing(cx);
        let (_, total) = self.unfiltered.as_ref()?;
        if listing.total() < 0 || listing.fetch() != SecFetch::Ready { return None; }
        let locale = plx_platform::i18n::current();
        Some(plx_platform::i18n::msg::library_header_filtered_of(&locale.number(listing.total()), &locale.number(*total)))
    }

    /// The right cluster as it is now, and in its widest case (every value at its longest), which
    /// fixes the cluster's width so nothing on the row moves when a value changes.
    pub(super) fn header_right<H: LibraryLike>(&self, cx: &Cx<'_, H>) -> (Vec<HeaderItem>, Vec<HeaderItem>) {
        use plx_platform::i18n::msg;
        let locale = plx_platform::i18n::current();
        let mut right = Vec::new();
        let mut widest = Vec::new();
        if let Some(readout) = self.filtered_readout(cx) {
            let item = HeaderItem { label: readout, readout: true, ..Default::default() };
            widest.push(item.clone());
            right.push(item);
        }
        if self.collections != CollectionsControl::Hidden {
            let value = match self.collections { CollectionsControl::Count(n) => Some(locale.number(i64::from(n))), _ => None };
            right.push(HeaderItem { value, selected_plate: self.listing == Listing::Collections,
                ..item(msg::library_header_collections()) });
            widest.push(HeaderItem { value: Some(locale.number(9_999)), ..item(msg::library_header_collections()) });
        }
        right.push(HeaderItem { selected_plate: self.listing == Listing::Recommended,
            ..item(msg::library_header_recommended()) });
        widest.push(item(msg::library_header_recommended()));
        let sort = self.sort_value(cx);
        let longest = H::listing(cx).sorts().iter().map(|sort| sort.title.as_str()).chain([sort.as_str()])
            .max_by(|a, b| page_header::cluster_width(&[item(*a)], cx.measure)
                .total_cmp(&page_header::cluster_width(&[item(*b)], cx.measure)))
            .unwrap_or_default().to_owned();
        right.push(HeaderItem { value: Some(sort), fold_glyph: true, ..item(msg::library_header_sort_filter()) });
        widest.push(HeaderItem { value: Some(longest), fold_glyph: true, ..item(msg::library_header_sort_filter()) });
        (right, widest)
    }

    /// A fingerprint of everything [`Self::refresh_header`] reads, cheap enough for every frame.
    fn header_key<H: LibraryLike>(&self, cx: &Cx<'_, H>) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        let listing = H::listing(cx);
        let directory = H::directory(cx);
        (std::ptr::from_ref(plx_platform::i18n::current()) as usize).hash(&mut h);
        self.view_section(cx).hash(&mut h);
        self.collections.hash(&mut h);
        self.listing.hash(&mut h);
        self.listed.code().hash(&mut h);
        matches!(self.kind, SecKind::Movie).hash(&mut h);
        self.switched.hash(&mut h);
        for section in directory.sections() {
            (section.sid.map(|sid| sid.raw()), section.key, section.row.title.as_str(), section.row.pinned).hash(&mut h);
        }
        for (id, source) in directory.sources() { (id.raw(), source.handle.as_str()).hash(&mut h); }
        (listing.id().map(|id| (id.epoch, id.section, id.query)), listing.sort_index(), listing.sort_desc(),
            listing.unwatched(), listing.genre().map(|genre| genre.id.as_str()), listing.total(),
            listing.fetch() == SecFetch::Ready, listing.sorts().len(), self.unfiltered.as_ref().map(|(_, total)| *total)).hash(&mut h);
        match self.pending.grid().filter(|(target, _)| target.matches(listing)).map(|(_, action)| action) {
            Some(GridAction::Sort { key, desc }) => (1u8, key.as_str(), *desc).hash(&mut h),
            Some(GridAction::Unwatched { desired }) => (2u8, *desired).hash(&mut h),
            Some(GridAction::Genre { id }) => (3u8, id.as_deref()).hash(&mut h),
            Some(GridAction::LibraryType(kind)) => (4u8, kind.code()).hash(&mut h),
            None => 0u8.hash(&mut h),
        }
        h.finish()
    }

    /// Rebuild everything the header row is made of: the Collections control's state, the names
    /// that fit (`self.libraries`), and the cached geometry.
    pub(super) fn refresh_header<H: LibraryLike>(&mut self, cx: &Cx<'_, H>) {
        self.collections = self.view_section(cx)
            .and_then(|index| H::directory(cx).sections().get(index).map(|section| section.key))
            .and_then(|key| u32::try_from(key).ok())
            .map_or(CollectionsControl::Unknown, |key| CollectionsControl::of(H::collections_count(cx, key)));
        self.note_unfiltered(cx);
        // `sync` runs every frame; the row (text measurement, packing) is rebuilt only when one
        // of the things it is made of changed.
        let key = self.header_key(cx);
        if self.header.is_some() && self.header_key == Some(key) { return; }
        self.header_key = Some(key);
        if self.listing == Listing::Collections { return self.refresh_collections_header(cx); }
        let (right, widest) = self.header_right(cx);
        let measure = cx.measure;
        let budget = PageHeader::new(PageHeaderSpec { right: right.clone(), ..Default::default() }, &widest, measure)
            .layout(SCR_W, measure).left_budget;

        self.libraries.clear();
        self.folded.clear();
        let mut overflow = 0;
        let current = self.view_section(cx);
        let mut left = Vec::new();
        if let Some(current) = current {
            let directory = H::directory(cx);
            let mut all: Vec<(u32, usize)> = Vec::new();
            for (index, section) in directory.favorite_sections_for(current) {
                let Some(sid) = section.sid else { continue };
                let elem = self.keys.register(
                    LibraryIdentity::Library(LibrarySectionIdentity { sid, key: section.key }),
                    LIBRARY_GROUP, all.len());
                all.push((elem, index));
            }
            // A SINGLE candidate never draws a name, whatever owns the section it names: a lone
            // library carries nothing to disambiguate for ANY profile (issue #100/#165), so only
            // the candidate COUNT decides, and only 2+ ever reaches the packing below.
            if all.len() > 1 {
                let labels: Vec<String> = all.iter()
                    .map(|(_, section)| self.library_label(*section, cx)).collect();
                let widths: Vec<f32> = labels.iter().map(|label| page_header::cluster_width(&[item(label.as_str())], measure)).collect();
                let selected = all.iter().position(|(_, index)| *index == current).unwrap_or(0);
                let more_w = page_header::cluster_width(&[item(format!("+{}", all.len() - 1))], measure);
                let picks = pack_names(&widths, selected, budget, more_w, layout::MAX_LIBRARY_PILLS);
                overflow = all.len() - picks.len();
                self.folded = all.iter().enumerate().filter(|(i, _)| !picks.contains(i))
                    .filter_map(|(_, (_, section))| u32::try_from(*section).ok()).collect();
                for &pick in &picks {
                    left.push(HeaderItem { selected_plate: all[pick].1 == current, ..item(labels[pick].as_str()) });
                    self.libraries.push(all[pick]);
                }
                if overflow > 0 { self.libraries.push((MORE, usize::MAX)); }
            }
        }

        let page = PageHeader::new(PageHeaderSpec { left, overflow, right }, &widest, measure);
        let mut layout = page.layout(SCR_W, measure);
        if self.libraries.is_empty() {
            // One library: no names, and the three controls sit centred between the nav and the
            // grid (the board's row top is y 158, ten under the multi-library row's 148).
            for it in &mut layout.items {
                it.text.y += layout::SINGLE_LIBRARY_DY;
                it.plate.y += layout::SINGLE_LIBRARY_DY;
            }
        }
        let controls = self.toolbar_elems();
        let mut stops = Vec::new();
        for (index, rect) in layout.items.iter().enumerate() {
            let elem = match rect.slot {
                page_header::Slot::Left(i) => self.libraries.get(i).map(|(elem, _)| *elem),
                page_header::Slot::Overflow => Some(MORE),
                page_header::Slot::Right(i) => {
                    let first_control = page.spec().right.iter().position(|it| it.focusable).unwrap_or(0);
                    i.checked_sub(first_control).and_then(|i| controls.get(i).copied())
                }
            };
            if let Some(elem) = elem.filter(|_| rect.focusable) { stops.push((elem, index)); }
        }
        self.header = Some(Header { page, layout, stops, crumb: None });
    }

    /// The Collections page's row: the crumb (its root is the one stop besides Sort) in place of
    /// the library names and the page's three controls.
    fn refresh_collections_header<H: LibraryLike>(&mut self, cx: &Cx<'_, H>) {
        self.libraries.clear();
        // Only the collections' own declared sorts count: until the store has switched the section
        // to the collections listing, `sorts()` is still the films' list.
        let declared = if self.listed == plx_data::browse::LibraryType::Collections { H::listing(cx).sorts().len() } else { 0 };
        let library = self.view_section(cx).map(|index| self.library_label(index, cx)).unwrap_or_default();
        let row = super::collections_page::header(&library, self.kind, declared, &self.sort_value(cx), SCR_W, cx.measure);
        let stops = if row.sort.is_some() { vec![(SORTFILTER, 0)] } else { Vec::new() };
        self.collections_sort = row.sort.is_some();
        self.header = Some(Header { page: row.page, layout: row.layout, stops, crumb: Some((row.crumb, row.crumb_layout)) });
    }

    /// Where the header control or name `elem` is, in screen space at the drawn or target scroll.
    pub(super) fn header_rect(&self, elem: u32, at: At) -> Option<Rect> {
        let plate = self.header.as_ref()?.plate(elem)?;
        let scroll = match at { At::Drawn => self.scroll.pos, At::SpringTarget => self.scroll_target };
        Some(Rect::new(plate.x, plate.y - scroll, plate.w, plate.h))
    }

    /// Paint the row at the drawn scroll. The row scrolls away with the page: only its focused
    /// pill and the selected plate are chrome of its own.
    pub(super) fn draw_header<H: LibraryLike>(&self, f: &DrawFrame<'_, '_, H>) {
        let Some(header) = &self.header else { return };
        let p = f.painter.alpha(f.page_alpha * self.page_fade.alpha());
        let mut layout = header.layout.clone();
        for it in &mut layout.items {
            it.text.y -= self.scroll.pos;
            it.plate.y -= self.scroll.pos;
        }
        let top = layout::HEADER_Y - self.scroll.pos;
        if !super::draw::document_band_visible(p, top, page_header::HEIGHT, plx_ui::widgets::CTRL_FOCUS_SCALE) { return; }
        let focused = f.focus.current.filter(|key| key.entry == self.entry).map(|key| key.elem);
        let focus = focused.and_then(|elem| header.stops.iter().find(|(stop, _)| *stop == elem)).map(|(_, index)| *index);
        header.page.draw(p, &layout, focus);
        if let Some((crumb, crumb_layout)) = &header.crumb {
            let mut crumb_layout = *crumb_layout;
            for rect in [&mut crumb_layout.root_text, &mut crumb_layout.root_plate, &mut crumb_layout.glyph,
                &mut crumb_layout.page_text, &mut crumb_layout.page_plate] { rect.y -= self.scroll.pos; }
            crumb.draw(p, &crumb_layout, focused == Some(CRUMB_ROOT));
        }
    }
}
