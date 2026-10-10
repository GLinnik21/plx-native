//! A result row's hits past its first twelve: one [`Lane`] per (source, kind), read in windows.
//!
//! The first paint is `/hubs/search?limit=12`, the *preview*. A lane is that preview plus what
//! `/library/search?searchTypes=<kind>` ([`plx_plex::plex::Client::search_typed`]) holds past it.
//! A hit's *depth* is its index in its own server's list for that kind; the merged row is the
//! round robin by depth (`search::merge_refs`), so a window of depths `[lo, lo + span)` over every
//! lane is a window of the merged row, and a lane only ever holds the depths of that window (plus
//! the ones a read has just fetched). Depths below the preview's length are served from the preview,
//! never read again.
//!
//! | kind | listing | depth `d` is at offset |
//! |---|---|---|
//! | movie | `movies` | `origin + d` |
//! | person | `people`, always a SAMPLE of the preview (the preview may hold director hits): the listing minus the preview's keys | `origin + d - P` |
//! | show | `tv`, up to the first episode row | `origin + d` |
//! | episode | `tv`, past the shows | `S + d` (`S` = [`TvMap::first_episode`]) |
//! | collection | none: LIMIT GROWTH, `/hubs/search?limit=<depth reached>` and the kind's rows sliced | n/a |
//!
//! Any other lane whose first typed request answers nothing while its preview is not empty is
//! switched to growth ("unsupported is not end"). Growth costs a request for depth `d` that returns
//! `d` rows of every hub kind: the endpoint has no offset. It ends when a response holds fewer rows
//! than the limit.
//!
//! **A read overlaps the lane's edge row by one** and expects that row's key at the offset it was
//! read from. Found elsewhere in the page, `origin` (or `S`) moves by the difference and the read is
//! made again: a show added while the query is open moves every offset by one and nothing is read
//! twice or skipped. Not found at all is a listing that will not hold still: the lane lets go of what
//! it read past the preview and is read by LIMIT GROWTH from then on, which every hit is reachable by
//! whatever the typed listing does.
//!
//! **`S`** (the first episode's offset in `tv`) is learned in this order: a preview show hub shorter
//! than the limit; the Show lane reaching the first episode row; else [`learn_first_episode`] gallops
//! single-row requests at 12, 36, 84, ... until one is an episode (or past the end) and bisects.

use std::collections::BTreeMap;
use std::ops::Range;

use plx_plex::plex::{MediaContainer, PageReq, SearchKind, SearchResult, ServerId};

use super::{same_tag, tag_hit, Item, Kind, Projection};

/// Rows per typed request, the overlap row excluded.
pub(super) const PAGE: usize = 24;
/// Requests one read may spend before it publishes what it has.
pub(super) const REQUESTS: usize = 8;
const FAV: &[(ServerId, i64, bool)] = &[];

/// What a worker reads a lane with: the typed listing and the grown hub list.
pub(super) trait Io {
    fn listing(&mut self, kind: SearchKind, req: PageReq) -> Option<MediaContainer>;
    /// `/hubs/search?limit=`, projected.
    fn hubs(&mut self, limit: usize) -> Option<Projection>;
    /// A test's fake serves every source; this says which one is about to ask.
    #[cfg(test)]
    fn serving(&mut self, _sid: ServerId) {}
}

/// What the Show and Episode lanes of one source share about its `tv` listing.
#[derive(Clone, Copy, Default, Debug, PartialEq, Eq)]
pub(super) struct TvMap {
    /// The offset of the first episode row, once known.
    pub first_episode: Option<usize>,
}

#[derive(Clone)]
pub(super) struct Lane {
    pub inited: bool,
    growth: bool,
    sample: bool,
    /// A typed request has answered with rows.
    probed: bool,
    preview: Vec<Item>,
    rows: BTreeMap<usize, Item>,
    /// The depths at or past the preview that are held, `[lo, hi)`.
    cov: (usize, usize),
    /// Depths from here on hold nothing.
    pub end: Option<usize>,
    origin: usize,
}

impl Lane {
    pub const EMPTY: Lane = Lane { inited: false, growth: false, sample: false, probed: false, preview: Vec::new(),
        rows: BTreeMap::new(), cov: (0, 0), end: None, origin: 0 };

    pub fn from_preview(kind: Kind, preview: &[Item]) -> Lane {
        let p = preview.len();
        let people = kind == Kind::Person;
        Lane { inited: true, growth: kind == Kind::Collection, sample: people, preview: preview.to_vec(),
            cov: (p, p), end: (p == 0 || (!people && p < super::LIMIT as usize)).then_some(p), ..Lane::EMPTY }
    }

    /// Every card the lane holds, for an edit that must reach the copy a rebuild draws from.
    pub fn items_mut(&mut self) -> impl Iterator<Item = &mut Item> {
        self.preview.iter_mut().chain(self.rows.values_mut())
    }

    pub fn is_growth(&self) -> bool { self.growth }
    pub fn preview_len(&self) -> usize { self.preview.len() }
    pub fn held(&self) -> usize { self.rows.len() }

    pub fn get(&self, d: usize) -> Option<&Item> {
        self.preview.get(d).or_else(|| self.rows.get(&d))
    }

    /// Does the lane have a depth at or past `d`? (`false` once its end is known to be at `d` or
    /// before.)
    pub fn has_past(&self, d: usize) -> bool {
        self.end.is_none_or(|e| e > d)
    }

    /// The highest depth read so far, plus one.
    pub fn covered(&self) -> usize { self.cov.1.max(self.preview.len()) }

    /// Does reading `range` need a request?
    pub fn needs(&self, range: &Range<usize>) -> bool {
        let hi = self.end.map_or(range.end, |e| range.end.min(e));
        let lo = range.start.max(self.preview.len());
        (hi > self.cov.1 && hi > self.preview.len()) || (lo < self.cov.0 && lo < hi)
    }

    /// Keep only the depths of `window`.
    pub fn trim(&mut self, window: Range<usize>) {
        let p = self.preview.len();
        self.rows.retain(|d, _| window.contains(d));
        let lo = self.cov.0.max(window.start).max(p);
        let hi = self.cov.1.min(window.end).max(lo);
        self.cov = (lo, hi);
    }

    /// Bring depths `range` into the lane. `None` is a failed request; the lane may be partly moved,
    /// so the caller reads a copy.
    pub fn read(&mut self, kind: Kind, tv: &mut TvMap, range: Range<usize>, io: &mut dyn Io, sid: ServerId)
        -> Option<()> {
        for _ in 0..REQUESTS {
            if !self.needs(&range) { return Some(()); }
            if self.growth {
                return self.read_growth(kind, range, io);
            }
            let forward = self.end.map_or(range.end, |e| range.end.min(e)) > self.cov.1;
            self.step(kind, tv, forward, &range, io, sid)?;
        }
        Some(())
    }

    fn read_growth(&mut self, kind: Kind, range: Range<usize>, io: &mut dyn Io) -> Option<()> {
        let limit = range.end.max(super::LIMIT as usize);
        let proj = io.hubs(limit)?;
        let k = super::KINDS.iter().position(|x| *x == kind)?;
        let got = &proj[k];
        for d in range.start.max(self.preview.len())..range.end {
            let Some(item) = got.get(d) else { continue };
            // a list that shifted between two asks may repeat a card it already gave
            let k = key_of(item);
            if self.preview.iter().chain(self.rows.values()).any(|x| key_of(x) == k) { continue; }
            self.rows.insert(d, item.clone());
        }
        let p = self.preview.len();
        self.cov = (self.cov.0.min(range.start.max(p)), self.cov.1.max(range.end.min(got.len())));
        if got.len() < limit { self.end = Some(got.len()); }
        Some(())
    }

    /// Read this lane by limit growth from now on, starting again from its preview: the typed
    /// listing's offsets no longer mean anything, and the hub list is one list in one order.
    fn into_growth(&mut self) {
        plx_base::eventlog::log("search: a typed listing moved under its lane, growing the limit");
        let p = self.preview.len();
        self.growth = true;
        self.rows.clear();
        self.cov = (p, p);
        self.end = (!self.sample && p < super::LIMIT as usize).then_some(p);
    }

    /// The offset of depth `d` (`d` at or past the preview), or `None` while `S` is unknown.
    fn offset(&self, kind: Kind, tv: &TvMap, d: usize) -> Option<usize> {
        let base = match kind {
            Kind::Episode => tv.first_episode?,
            _ => self.origin,
        };
        Some(base + if self.sample { d - self.preview.len() } else { d })
    }

    fn shift(&mut self, kind: Kind, tv: &mut TvMap, by: isize) -> Option<()> {
        let move_by = |n: usize| n.checked_add_signed(by);
        match kind {
            Kind::Episode => tv.first_episode = Some(move_by(tv.first_episode?)?),
            Kind::Show => {
                self.origin = move_by(self.origin)?;
                if let Some(s) = tv.first_episode { tv.first_episode = Some(move_by(s)?); }
            }
            _ => self.origin = move_by(self.origin)?,
        }
        Some(())
    }

    /// One typed request, forward from the covered edge or backward from it.
    fn step(&mut self, kind: Kind, tv: &mut TvMap, forward: bool, range: &Range<usize>, io: &mut dyn Io,
        sid: ServerId) -> Option<()> {
        let p = self.preview.len();
        let sk = match kind { Kind::Movie => SearchKind::Movies, Kind::Person => SearchKind::People, _ => SearchKind::Tv };
        if kind == Kind::Episode && tv.first_episode.is_none() { learn_first_episode(tv, io)?; }
        if kind == Kind::Show {
            if let Some(s) = tv.first_episode { self.end = Some(s.saturating_sub(self.origin)); if !self.needs(range) { return Some(()); } }
        }
        let (a, b) = if forward {
            let a = self.cov.1.max(p);
            (a, range.end.min(a + PAGE).min(self.end.unwrap_or(usize::MAX)))
        } else {
            let b = self.cov.0;
            (range.start.max(p).max(b.saturating_sub(PAGE)), b)
        };
        // the row read beside the edge, and the key it must carry
        let edge_depth = if forward { a.checked_sub(1) } else { Some(b) };
        let edge = edge_depth.and_then(|d| {
            if d < p { (!self.sample).then(|| self.preview.get(d)).flatten() } else { self.rows.get(&d) }
        }).map(key_of);
        let first = self.offset(kind, tv, a)?;
        let (start, size) = match (&edge, forward) {
            (Some(_), true) => (first - 1, b - a + 1),
            (Some(_), false) => (first, b - a + 1),
            (None, _) => (first, b - a),
        };
        let mc = io.listing(sk, PageReq { start, size })?;
        if mc.offset != start as i64 { return None; }
        let rows = &mc.search_result;
        if rows.is_empty() && !self.probed && p > 0 {
            // a kind the server does not list answers empty, which is not the end of the row
            self.growth = true;
            plx_base::eventlog::log("search: typed listing answered nothing for a non-empty preview, growing the limit");
            return Some(());
        }
        let mut at = 0;
        if let Some(edge) = &edge {
            let expected = if forward { 0 } else { size - 1 };
            if rows.get(expected).and_then(row_key).as_ref() != Some(edge) {
                let found = rows.iter().position(|r| row_key(r).as_ref() == Some(edge));
                match found {
                    Some(j) => {
                        let by = j as isize - expected as isize;
                        return self.shift(kind, tv, by);
                    }
                    None if !self.probed && forward && self.cov.0 == self.cov.1 && matches!(kind, Kind::Movie) => {
                        // the listing does not start with the preview: it is a sample of it
                        self.sample = true;
                        return Some(());
                    }
                    None if forward && self.relocate_back(kind, tv, start, edge, sk, io)? => return Some(()),
                    None => { self.into_growth(); return Some(()); }
                }
            }
            if forward { at = 1; }
        }
        let rows_end = if forward { rows.len() } else { rows.len() - usize::from(edge.is_some()) };
        let body = &rows[at..rows_end];
        if kind == Kind::Episode {
            let lead = body.iter().take_while(|r| !is_type(r, "episode")).count();
            if lead > 0 {
                tv.first_episode = Some(tv.first_episode? + lead);
                return Some(());
            }
        }
        let mut ended = None;
        let mut taken = 0;
        for (i, row) in body.iter().enumerate() {
            let depth = a + i;
            let offset = first + i;
            if kind == Kind::Show && is_type(row, "episode") {
                tv.first_episode = Some(offset);
                ended = Some(depth);
                break;
            }
            taken += 1;
            let Some(item) = row_item(row, sid) else { continue };
            if self.sample && self.preview.iter().any(|x| key_of(x) == key_of(&item)) { continue; }
            if let Item::Tag(t) = &item {
                // The same person listed again for another library section: the preview fold SUMS
                // their counts (`search::project`), so the card still held takes this row's count
                // rather than the row being dropped. A card that has left the window cannot be
                // reached from here, and skipping a second sighting of a card already drawn is
                // right; the preview's own cards are never edited (a sample lane's listing repeats
                // them by design, and their counts were folded when the preview was).
                if self.preview.iter().any(|x| matches!(x, Item::Tag(o) if same_tag(o, t))) { continue; }
                let held = self.rows.iter_mut().find_map(|(d, x)| match x {
                    Item::Tag(o) if same_tag(o, t) => Some((*d, o)),
                    _ => None,
                });
                if let Some((at, o)) = held {
                    if at != depth {
                        o.count += t.count;
                        o.fav |= t.fav;
                        if o.thumb.is_empty() { o.thumb = t.thumb.clone(); }
                    }
                    continue;
                }
            }
            self.rows.insert(depth, item);
        }
        self.probed = true;
        let full = taken == b - a && ended.is_none();
        if forward {
            if self.cov.0 >= self.cov.1 { self.cov.0 = a; }
            self.cov.1 = if full { b } else { a + taken };
            if !full { self.end = Some(ended.unwrap_or(a + taken)); }
        } else {
            self.cov.0 = a;
        }
        Some(())
    }

    /// A forward read's edge row was not in the page read where it was left: look one page back
    /// (rows removed ahead of it move it to a lower offset). `true` when it was found and the
    /// offsets were corrected, so the caller reads again.
    fn relocate_back(&mut self, kind: Kind, tv: &mut TvMap, start: usize, edge: &str, sk: SearchKind,
        io: &mut dyn Io) -> Option<bool> {
        let from = start.saturating_sub(PAGE);
        if from == start { return Some(false); }
        let mc = io.listing(sk, PageReq { start: from, size: start - from })?;
        if mc.offset != from as i64 { return None; }
        match mc.search_result.iter().position(|r| row_key(r).as_deref() == Some(edge)) {
            Some(j) => { self.shift(kind, tv, (from + j) as isize - start as isize)?; Some(true) }
            None => Some(false),
        }
    }
}

/// Find where the episodes start in `tv`, by single-row requests: at 12, 36, 84, ... until a row is
/// an episode or the listing has ended, then by bisection. O(log S) requests of one row each. Runs
/// only for a source whose preview show hub filled its 12 rows, so the first 12 are shows.
pub(super) fn learn_first_episode(tv: &mut TvMap, io: &mut dyn Io) -> Option<()> {
    let past = |io: &mut dyn Io, at: usize| -> Option<bool> {
        let mc = io.listing(SearchKind::Tv, PageReq { start: at, size: 1 })?;
        if mc.offset != at as i64 { return None; }
        Some(mc.search_result.first().is_none_or(|r| is_type(r, "episode")))
    };
    let mut shows = super::LIMIT as usize - 1; // the last row known to be a show
    let mut probe = super::LIMIT as usize;
    let mut upper = loop {
        if past(io, probe)? { break probe; }
        shows = probe;
        probe = probe * 2 + super::LIMIT as usize;
    };
    while upper - shows > 1 {
        let mid = shows + (upper - shows) / 2;
        if past(io, mid)? { upper = mid; } else { shows = mid; }
    }
    tv.first_episode = Some(upper);
    Some(())
}

impl TvMap {
    /// `S` from the preview alone: a show hub that came back short holds every show there is.
    pub fn from_previews(shows_in_preview: usize) -> TvMap {
        TvMap { first_episode: (shows_in_preview < super::LIMIT as usize).then_some(shows_in_preview) }
    }
}

fn is_type(r: &SearchResult, ty: &str) -> bool {
    r.metadata.as_ref().is_some_and(|m| m.kind == ty)
}

fn row_item(r: &SearchResult, sid: ServerId) -> Option<Item> {
    match (&r.metadata, &r.directory) {
        (Some(m), _) => Some(Item::Media(crate::pms::parse_item(m, sid))),
        (None, Some(t)) => Some(Item::Tag(tag_hit(t, sid, FAV))),
        _ => None,
    }
}

fn row_key(r: &SearchResult) -> Option<String> {
    match (&r.metadata, &r.directory) {
        (Some(m), _) => Some(crate::pms::clean(&m.rating_key)),
        (None, Some(t)) => Some(if t.tag_key.is_empty() { format!("{}:{}", t.id, t.tag) } else { t.tag_key.clone() }),
        _ => None,
    }
}

pub(super) fn key_of(it: &Item) -> String {
    match it {
        Item::Media(m) => m.rk.clone(),
        Item::Collection(c) => c.item.rk.clone(),
        Item::Tag(t) => if t.tag_key.is_empty() { format!("{}:{}", t.id, t.name) } else { t.tag_key.clone() },
    }
}
