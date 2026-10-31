//! The library page header (redesign spec, "Header"): the library names, the "+N" overflow and the
//! page's controls (Collections › and Recommended › as places, a divider, then the sort control:
//! icon, the chosen criterion, a down chevron, with "Sort & Filter" as its unshown name
//! ([`HeaderItem::sort_control`], which the Collections page's Sort and the Watchlist's wear too); on the Collections page, the
//! crumb and its Sort) on the ui
//! [`PageHeader`], one focus row at y 148.
//!
//! This file DECLARES the row: which names, which controls, which element each focusable item is.
//! The row is built here, once per sync, as a ui [`HeaderRow`]; its layout, paint, motion and focus
//! group are the ui's (`plx_ui::page_header`), the same ones the Collections, Recommended and
//! Watchlist pages wear. The names are packed against the room the right cluster leaves
//! (`PageHeader::layout`'s `left_budget`, sized from the cluster's widest case), so the names never
//! shift when a value changes, the Collections answer (unknown, none, a count of up to three digits)
//! included; a library whose count needs a fourth digit widens its own budget once; the
//! controls inside the cluster stand by what they draw, one gap apart. The row's MOTION (`header_capsules`, `header_pop`) lives on the screen, not in the
//! row: a library switch replaces the whole row, and the pill must travel across it.
use super::*;
use plx_ui::hstack::{self, Align, HStack};
use plx_ui::page_header::{self, HeaderFocus, HeaderItem, HeaderRow, PageHeader, PageHeaderSpec, Slot};

/// The width the names `picks` (then, with `more_w`, the "+N" after them) stand in, one
/// [`page_header::GAP`] apart: the ui's own [`HStack::width`]. `PageHeader::layout` places the names
/// with the same stack and the same gap, so what is packed here and where the names stand share the
/// gap rule (not one construction: this takes measured widths, the layout takes spec items).
fn names_w(widths: &[f32], picks: &[usize], more_w: Option<f32>) -> f32 {
    let names: Vec<hstack::Item> = picks.iter().map(|&i| widths[i]).chain(more_w).map(hstack::Item::new).collect();
    HStack::new(&names, page_header::GAP, Align::Leading(0.0)).width()
}

/// Which of `n` names show when the row cannot hold them all (`budget` is text edge to text edge
/// for names plus "+N"). The picked library takes the LAST visible slot: names keep their order
/// from the front, and a selected one past the cut replaces the final name. Always at least the
/// selected name; every name when they all fit under `cap`.
pub(super) fn pack_names(widths: &[f32], selected: usize, budget: f32, more_w: f32, cap: usize) -> Vec<usize> {
    let n = widths.len();
    if n == 0 || cap == 0 { return Vec::new(); }
    let selected = selected.min(n - 1);
    let all: Vec<usize> = (0..n).collect();
    if n <= cap && names_w(widths, &all, None) <= budget { return all; }
    // "+N" takes the end of the row, one gap after the last name.
    let with_more = |picks: &[usize]| names_w(widths, picks, Some(more_w));
    for k in (1..n.min(cap)).rev() {
        let mut picks: Vec<usize> = (0..k).collect();
        if selected >= k { picks[k - 1] = selected; }
        if with_more(&picks) <= budget { return picks; }
    }
    vec![selected]
}

/// What the read-out's filtered count was taken under: the library listing (the key `unfiltered`
/// carries) and the filter terms in force. A count is only ever shown for the key it was counted under.
#[derive(Clone, PartialEq)]
pub(super) struct FilterKey {
    library: (LibrarySectionIdentity, u32, plx_data::browse::LibraryType),
    genre: Option<String>,
    unwatched: bool,
}

fn item(label: impl Into<String>) -> HeaderItem {
    HeaderItem { label: label.into(), focusable: true, ..Default::default() }
}

/// A control that opens a page (Collections, Recommended): the row draws a chevron after it.
fn place(label: impl Into<String>) -> HeaderItem {
    HeaderItem { place: true, ..item(label) }
}

/// The hairline between the places and the sort control. Not a stop.
fn divider() -> HeaderItem {
    HeaderItem { divider: true, ..Default::default() }
}

impl LibraryScreen {
    /// Every focusable element of the header row in focus order: the library names, "+N", then
    /// the controls (on the Collections page, the crumb's root, then Sort).
    pub(super) fn header_elems(&self) -> Vec<u32> {
        self.header.row().map_or_else(Vec::new, |row| row.stops.iter().map(|(elem, _)| *elem).collect())
    }

    pub(super) fn is_header_elem(&self, elem: u32) -> bool {
        self.header_focus().is_some_and(|focus| focus.contains(elem))
    }

    /// The Sort & Filter control's value: the sort in force, a queued edit included. The scroll
    /// capsule carries the same string. Borrowed from the listing (or the catalogue), so the capsule,
    /// which asks every frame it is up, allocates nothing for it.
    pub(super) fn sort_value<'a, H: LibraryLike>(&self, cx: &Cx<'a, H>) -> &'a str {
        let listing = H::listing(cx);
        let queued = self.pending.grid().filter(|(target, _)| target.matches(listing)).map(|(_, action)| action);
        let sort = match queued {
            Some(GridAction::Sort { key, .. }) => listing.sorts().iter().find(|sort| &sort.key == key),
            _ => listing.sorts().get(listing.sort_index()),
        };
        sort.map_or(plx_platform::i18n::msg::browse_library_title(), |sort| sort.label())
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

    /// What a filtered total belongs to: this listing's library and the filter terms it was asked
    /// with. `None` while the listing has no identity or the page no section.
    fn filter_key<H: LibraryLike>(&self, cx: &Cx<'_, H>) -> Option<FilterKey> {
        let listing = H::listing(cx);
        let (id, section) = (listing.id()?, self.section.clone()?);
        Some(FilterKey {
            library: (section, id.epoch, listing.library_type()),
            genre: listing.genre().map(|genre| genre.id.clone()),
            unwatched: listing.unwatched(),
        })
    }

    /// Remember the totals the read-out is made of: the data layer publishes only the filtered
    /// total, and the read-out needs the unfiltered one beside it.
    ///
    /// **The filtered total is held across a reload that keeps its terms.** A sort pick forgets the
    /// listing (`reset_listing`: loading, no total) under the same genre and Unwatched terms, and
    /// the count of what those terms match has not changed; so the read-out keeps saying it, and
    /// stays on the row to glide with its neighbours instead of leaving and coming back when the
    /// listing lands. The held count is dropped the first time the page sees any other library or
    /// other terms, and when the listing lands unfiltered, so it is never shown for a filter it
    /// was not counted under.
    fn note_totals<H: LibraryLike>(&mut self, cx: &Cx<'_, H>) {
        let listing = H::listing(cx);
        let Some(key) = self.filter_key(cx) else { return };
        if listing.total() < 0 || listing.fetch() != SecFetch::Ready {
            if self.filtered.as_ref().is_some_and(|(held, _)| *held != key) { self.filtered = None; }
            return;
        }
        if key.genre.is_none() && !key.unwatched {
            self.unfiltered = Some((key.library, listing.total()));
            self.filtered = None;
        } else {
            if self.unfiltered.as_ref().is_some_and(|(seen, _)| *seen != key.library) { self.unfiltered = None; }
            self.filtered = Some((key, listing.total()));
        }
    }

    /// "312 of 1,284 films", once a filter is on and the unfiltered total is known. The noun is the
    /// listing's own (`LibraryType::count_label`, the words the Sources list's counts wear), so the
    /// read-out cannot be taken for a count of the Collections beside it.
    fn filtered_readout<H: LibraryLike>(&self, cx: &Cx<'_, H>) -> Option<String> {
        if !self.filter_active(cx) { return None; }
        let listing = H::listing(cx);
        let (_, total) = self.unfiltered.as_ref()?;
        // The listing's own count once it has one; while it reloads under the same terms, the count
        // held for them (see [`Self::note_totals`]).
        let matching = if listing.total() >= 0 && listing.fetch() == SecFetch::Ready {
            listing.total()
        } else {
            let (held, matching) = self.filtered.as_ref()?;
            if self.filter_key(cx).as_ref() != Some(held) { return None; }
            *matching
        };
        let locale = plx_platform::i18n::current();
        let of_total = listing.library_type().count_label(self.kind, *total);
        Some(plx_platform::i18n::msg::library_header_filtered_of(&locale.number(matching), &of_total))
    }

    /// The right cluster as it is now, and in its widest case (every value at its longest), which
    /// fixes the cluster's BUDGET so the room the library names may use does not change when a
    /// value does.
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
        // The control is on the row only with its count: an unknown answer lays out as none (see
        // [`CollectionsControl`]), so "Collections" is never drawn bare. The slot is the widest
        // count the cluster's BUDGET allows for (THREE digits; a library whose count needs more
        // widens only its own slot, here and in the widest case). It is no air inside the row: the
        // control is placed by the text it draws, so the gap to "Recommended" is the row's one gap
        // whatever the count is. The budget reserves it for EVERY answer, a library with none
        // included: a cold cache reads Unknown for a round trip and then Hidden, and the names
        // must not repack between the two (the room they pack into is measured once and never
        // moves).
        let slot_of = |count: u32| Some(locale.number(i64::from(count.max(999))));
        let control = |value, slot| HeaderItem { value, value_slot: slot, ..place(msg::library_header_collections()) };
        match self.collections {
            CollectionsControl::Count(count) => {
                let slot = slot_of(count);
                right.push(HeaderItem { selected_plate: self.listing == Listing::Collections,
                    ..control(Some(locale.number(i64::from(count))), slot.clone()) });
                widest.push(control(slot.clone(), slot));
            }
            CollectionsControl::Unknown | CollectionsControl::Hidden => {
                let slot = slot_of(0);
                widest.push(control(slot.clone(), slot));
            }
        }
        right.push(HeaderItem { selected_plate: self.listing == Listing::Recommended,
            ..place(msg::library_header_recommended()) });
        widest.push(place(msg::library_header_recommended()));
        // The divider and the sort control draw what the criterion says, with no slot for it: the
        // control ends at the row's edge and the places slide as the criterion's width changes. The
        // widest criterion only sizes the room the library names may use.
        right.push(divider());
        widest.push(divider());
        let sort = self.sort_value(cx);
        let longest = H::listing(cx).sorts().iter().map(|sort| sort.label()).chain([sort])
            .map(|label| (page_header::cluster_width(&[HeaderItem::sort_control("", label)], cx.measure), label))
            .max_by(|a, b| a.0.total_cmp(&b.0))
            .map_or("", |(_, label)| label).to_owned();
        right.push(HeaderItem::sort_control(msg::library_header_sort_filter(), sort));
        widest.push(HeaderItem::sort_control(msg::library_header_sort_filter(), longest));
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
            listing.fetch() == SecFetch::Ready, listing.sorts().len(), self.unfiltered.as_ref().map(|(_, total)| *total),
            self.filtered.as_ref().map(|(_, total)| *total)).hash(&mut h);
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
    ///
    /// **The control is the answer for THIS library, and nothing else.** [`Self::view_section`] is
    /// the library the row names (a switch's target from the moment it is chosen), so the answer
    /// read is that library's and a switch never carries the one it left: while the new library's
    /// count is not known the control is not on the row, and a count that lands later fades in
    /// through the row's [`HeaderSwap`] (the probe is warmed for every library when the directory
    /// lands, so it is usually known before the switch is drawn, and the control is then in the
    /// new row's first frame). The control the library left is not drawn on the new one at all,
    /// not even fading out ([`HeaderSwap::begin_switch`]). The one thing carried is this SAME
    /// library's own last count across a read of nothing (see below), never another's.
    pub(super) fn refresh_header<H: LibraryLike>(&mut self, cx: &Cx<'_, H>) {
        let view = self.view_section(cx);
        let of = view.and_then(|index| H::directory(cx).sections().get(index)).map(|section| (section.sid, section.key));
        let read = view.map_or(CollectionsControl::Unknown, |index| CollectionsControl::of(H::collections_count(cx, index)));
        // The probe reading nothing AGAIN for the library the row already names (its server was
        // re-pointed at a new client, so the cache missed for a round trip) is not news: the count
        // the row has stays, with its focus stop, until an answer replaces it. Across a switch it
        // never does (`of` is another library's), so a count is only ever one this library gave.
        self.collections = match (read, self.collections) {
            (CollectionsControl::Unknown, held @ CollectionsControl::Count(_)) if of == self.header_of => held,
            (read, _) => read,
        };
        self.note_totals(cx);
        // `sync` runs every frame; the row (text measurement, packing) is rebuilt only when one
        // of the things it is made of changed.
        let key = self.header_key(cx);
        if self.header.is_current(key) { return; }
        let row = if self.listing == Listing::Collections { self.collections_header(cx) } else { self.names_header(cx) };
        if let Some(old) = self.header.row() {
            // The same library's row changing fades what comes and goes; a row for ANOTHER library
            // is the new page's own, and shows nothing of the one left (see `HeaderSwap::begin_switch`).
            if of == self.header_of { self.header_swap.begin(old, &row); } else { self.header_swap.begin_switch(old, &row); }
        }
        self.header_of = of;
        self.header.install(key, row);
    }

    /// The row of the Library page itself: the library names that fit, then the controls.
    fn names_header<H: LibraryLike>(&mut self, cx: &Cx<'_, H>) -> HeaderRow {
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

        let page = PageHeader::new(PageHeaderSpec { left, overflow, right, ..Default::default() }, &widest, measure);
        let mut layout = page.layout(SCR_W, measure);
        if self.libraries.is_empty() {
            // One library: no names, and the three controls sit centred between the nav and the
            // grid (the board's row top is y 158, ten under the multi-library row's 148).
            for it in &mut layout.items {
                it.text.y += layout::SINGLE_LIBRARY_DY;
                it.plate.y += layout::SINGLE_LIBRARY_DY;
            }
        }
        self.header_row(page, layout)
    }

    /// The built row: which element each focusable item of `layout` is. The names and "+N" are
    /// `self.libraries`, the controls are [`Self::toolbar_elems`] (the first focusable right item is
    /// the first control, a read-out before it is no stop), the crumb's root is [`CRUMB_ROOT`] and
    /// its page name, the title, is none.
    fn header_row(&self, page: PageHeader, layout: page_header::HeaderLayout) -> HeaderRow {
        let controls = self.toolbar_elems();
        let stops = layout.items.iter().enumerate().filter(|(_, rect)| rect.focusable).filter_map(|(index, rect)| {
            let elem = match rect.slot {
                Slot::Left(i) => self.libraries.get(i).map(|(elem, _)| *elem),
                Slot::Overflow => Some(MORE),
                // The n-th FOCUSABLE right item is the n-th control: a read-out before them and the
                // divider among them are no stops.
                Slot::Right(i) => {
                    let rank = page.spec().right[..i].iter().filter(|it| it.focusable).count();
                    controls.get(rank).copied()
                }
                Slot::CrumbRoot => Some(CRUMB_ROOT),
                Slot::CrumbPage => None,
            };
            elem.map(|elem| (elem, index))
        }).collect();
        HeaderRow::new(page, layout, stops)
    }

    /// The Collections page's row: the crumb (its root is a stop, its page name the title) in
    /// place of the library names, and Sort when the server declares more than one.
    fn collections_header<H: LibraryLike>(&mut self, cx: &Cx<'_, H>) -> HeaderRow {
        self.libraries.clear();
        // Only the collections' own declared sorts count: until the store has switched the section
        // to the collections listing, `sorts()` is still the films' list.
        let declared = if self.listed == plx_data::browse::LibraryType::Collections { H::listing(cx).sorts().len() } else { 0 };
        let library = self.view_section(cx).map(|index| self.library_label(index, cx)).unwrap_or_default();
        let spec = super::collections_page::header_spec(&library, self.kind, declared, self.sort_value(cx));
        self.collections_sort = super::collections_page::sort_visible(declared);
        let page = PageHeader::new(spec, &[], cx.measure);
        let layout = page.layout(SCR_W, cx.measure);
        self.header_row(page, layout)
    }

    /// The row as one focus group of this page, at its scroll: what the engine's `Focusable`
    /// questions about the header are answered from (`None` before the row is built).
    pub(super) fn header_focus(&self) -> Option<HeaderFocus<'_>> {
        self.header.row().map(|row| row.focus(self.entry, LIBRARY_GROUP, (self.scroll.pos, self.scroll_target), content_clip()))
    }

    /// Where the header control or name `elem` is, in screen space at the drawn or target scroll.
    pub(super) fn header_rect(&self, elem: u32, at: At) -> Option<Rect> {
        self.header_focus()?.place(elem, at).map(|placed| placed.rect)
    }

    /// One frame of the row's motion: the focus capsule travels to the focused control, the
    /// selection plate lands under it. Focus off the row (or a menu holding it) closes the pop.
    pub(super) fn step_header<H: LibraryLike>(&mut self, cx: &Cx<'_, H>, dt: f32) {
        self.header_swap.step(dt);
        let Some(row) = self.header.row() else { return };
        row.step(self.entry, cx.focus.current, &mut self.header_capsules, &mut self.header_pop, dt);
    }

    /// Paint the row at the drawn scroll. The row scrolls away with the page: only its pill and
    /// the selected plate are chrome of its own.
    pub(super) fn draw_header<H: LibraryLike>(&self, f: &DrawFrame<'_, '_, H>) {
        let Some(row) = self.header.row() else { return };
        let top = layout::HEADER_Y - self.scroll.pos;
        // The row rides the scroll under the top fade, and keeps out of the band the returning
        // chrome is drawn in while the scroll is still bringing it home (`Handoff::clearance`).
        let p = f.painter.alpha(f.page_alpha * self.page_fade.alpha() * self.handoff.clearance(top));
        // The pill's outset (2 px) is far inside the cast's padding the band is grown by, so the band needs no pop.
        if !super::draw::document_band_visible(p, top, page_header::HEIGHT, 1.0) { return; }
        // A control a menu has lifted out of the page (`Screen::opener`) is not drawn here at all:
        // the opener lift is its only copy ([`Self::draw_opener`]), lit above the scrim. One rule
        // for every header opener (design review S12). The dispatcher decides when it stands down
        // (`lifted_opener`: a closing menu whose focus moved off the opener), so a lift is always
        // the control's whole menu-open stretch, during which the engine's focus is on the menu's
        // rows and the row's motion HOLDS.
        let lifted = f.lifted.filter(|key| key.entry == self.entry).map(|key| key.elem);
        row.draw_with(p, self.scroll.pos, &self.header_capsules, self.header_pop.scale_with(0, f.press.scale), lifted, &self.header_swap);
    }

    /// **The opener lift for a header control** (`Screen::redraw_focused`): `elem`, the control a
    /// menu opened from, painted as the focused one above the menu's scrim, where the page pass
    /// left it out. Nothing is drawn for an element that is no stop of this row (a card).
    ///
    /// **It paints through the row's swap**, as the page's pass does: a sort picked from the menu
    /// rebuilds the row on the frame the menu is dismissed, and until the menu is gone this lift is
    /// the only thing that draws the sort control, so a lift that ignored the swap drew the new
    /// criterion whole from the first frame and popped when it stood down mid-slide.
    pub(super) fn draw_header_opener<H: LibraryLike>(&self, f: &DrawFrame<'_, '_, H>, elem: u32) {
        let Some(row) = self.header.row() else { return };
        let p = f.painter.alpha(f.page_alpha * self.page_fade.alpha() * self.handoff.clearance(layout::HEADER_Y - self.scroll.pos));
        row.draw_opener_with(p, self.scroll.pos, self.header_pop.scale_with(0, f.press.scale), elem, &self.header_swap);
    }
}
