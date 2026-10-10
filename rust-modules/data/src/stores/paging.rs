//! The sliding window behind a Recently Added row: at most [`MAX_SHELF_ITEMS`] cards, each kept with
//! the listing position it was read from, moved by a forward or backward ask that replaces half of
//! it. The window is generic over where its rows come from only through the `fetch` closure the
//! caller passes, so the same logic reads a live hub listing and a test's stand-in server alike.
//!
//! A listing whose order does not hold between requests cannot be addressed by position. The
//! [`Ledger`] below is the one fallback: the keys a row has shown, in the order it showed them, so
//! the row still holds only its window yet reaches every item once and can move back over it.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use plx_plex::plex::{MediaContainer, Metadata, ServerId};

use crate::pms::{clean, listable, parse_item, PmsMovie, HUB_FETCH_COUNT, MAX_SHELF_ITEMS};

/// A card with the listing position it was read from.
pub type Row = (usize, Arc<PmsMovie>);

/// Where a window sits in its listing, as the server last reported it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PageInfo {
    /// The first listing position the window covers.
    pub offset: usize,
    /// One past the last listing position read.
    pub end: usize,
    /// The listing's `totalSize`, 0 when the server sent none.
    pub total: usize,
    /// Whether another page remains in the direction the window last moved.
    pub more: bool,
    /// Set when the edge row could not be found where the listing now holds it. The window is
    /// returned unchanged; a listing that will not hold still needs a ledger to be read further.
    pub unstable: bool,
}

/// One ask against a window: where it starts, which way it moves, and the sections hidden from it.
pub struct Ask<'a> {
    pub start: usize,
    pub before: bool,
    pub hidden: &'a [i64],
}

/// The retained row a read continues from: the last one a forward ask reads past, or the first one a
/// backward ask reads before. Its key must still be where the listing holds it, or every retained
/// position is stale by the same difference.
struct Edge {
    position: usize,
    key: String,
}

/// Where the edge is now, against where the window last read it.
enum Relocation {
    /// Found in the page that was read where the edge was, so that page's rows are still usable.
    InPage(isize),
    /// Found one page on the far side of its old position. The page read there is stale, so the
    /// rows beyond the edge are read again from the edge's new position.
    Beyond(isize),
    /// Not on either side.
    Lost,
}

/// Moves a window by one ask. `current` is the window as it stands, or `None` for a first read;
/// `minimum_end` is the position a forward read must reach before it stops (0 for an ordinary ask).
/// `fetch(start, size)` is one server page. `None` means a page failed or echoed another offset,
/// and the caller keeps what it has.
///
/// Each ask re-anchors on the retained edge. The first read starts at the edge and overlaps it by
/// one row, so a full page of new rows still arrives; when hidden rows trail the edge, the edge is
/// checked on a page of its own and the reads continue where the window ends. If the edge key is not
/// where the listing puts it, it is searched for in the page that was read and then in one page on
/// the far side. If it is still not found, the window is returned unchanged and flagged `unstable`.
pub fn fetch_window(sid: ServerId, ask: &Ask, current: Option<(&[Row], PageInfo)>, minimum_end: usize,
    mut fetch: impl FnMut(usize, usize) -> Option<MediaContainer>) -> Option<(Vec<Row>, PageInfo)> {
    let current_info = current.map(|(_, info)| info);
    let mut rows: Vec<Row> = current.map_or_else(Vec::new, |(rows, _)| {
        rows.iter().filter(|(_, item)| !ask.hidden.contains(&item.sec)).cloned().collect()
    });
    let retained_all = rows.len() <= HUB_FETCH_COUNT as usize;
    if ask.before { rows.truncate(HUB_FETCH_COUNT as usize); }
    else if !retained_all { rows.drain(..rows.len() - HUB_FETCH_COUNT as usize); }
    let mut offset = if ask.before { ask.start } else {
        current_info.filter(|_| retained_all).map_or_else(|| rows.first().map_or(ask.start, |row| row.0), |info| info.offset)
    };
    let mut end = if ask.before {
        current_info.filter(|_| retained_all).map_or_else(|| rows.last().map_or(ask.start, |row| row.0 + 1), |info| info.end)
    } else { ask.start };
    let mut total = current_info.map_or(0, |info| info.total);
    let mut more = ask.before && current_info.is_some_and(|info| info.more || !retained_all);
    let edge = if ask.before { rows.first() } else { rows.last() }
        .map(|(position, item)| Edge { position: *position, key: item.rk.clone() });
    let mut cursor = ask.start;
    let mut carried = false;
    if let Some(edge) = &edge {
        if (ask.before && edge.position == ask.start) || (!ask.before && edge.position + 1 == ask.start) {
            // The edge is the row the ask reads beside, so the first read carries it as its overlap.
            cursor = edge.position;
            carried = true;
        } else {
            // Hidden rows trail the edge, so the overlap cannot ride in the read. The edge is checked
            // on a page of its own, and the reads continue where the window ends.
            let (start, count) = if ask.before {
                let start = edge.position.saturating_sub(MAX_SHELF_ITEMS);
                (start, edge.position - start + 1)
            } else { (edge.position, MAX_SHELF_ITEMS + 1) };
            let page = fetch(start, count)?;
            if page.offset != start as i64 { return None; }
            let by = match relocate(&mut fetch, edge, ask.before, &page, start)? {
                Relocation::InPage(by) | Relocation::Beyond(by) => by,
                Relocation::Lost => return Some(unchanged(current)),
            };
            if shift(&mut rows, &mut offset, &mut end, by).is_none() { return Some(unchanged(current)); }
            let Some(next) = ask.start.checked_add_signed(by) else { return Some(unchanged(current)) };
            cursor = next;
        }
    }
    // Keep each worker bounded even when several consecutive server pages are hidden.
    // A partial window retains the overlap and publishes its advanced server cursor.
    let mut requests = 0;
    while requests < 8 || (!ask.before && cursor < minimum_end) {
        requests += 1;
        if rows.len() >= MAX_SHELF_ITEMS || (ask.before && cursor == 0) { break; }
        // Backward pages ask only for the room left beside the retained overlap, so every row
        // fetched is kept and `offset` stays the first server position the window covers.
        let size = if ask.before { cursor.min(MAX_SHELF_ITEMS - rows.len()) } else { MAX_SHELF_ITEMS };
        let start = if ask.before { cursor - size } else { cursor };
        let count = size + usize::from(carried);
        let mc = fetch(start, count)?;
        if mc.offset != start as i64 { return None; }
        let carry = if std::mem::take(&mut carried) { edge.as_ref() } else { None };
        let mut edge_now = None;
        if let Some(edge) = carry {
            match relocate(&mut fetch, edge, ask.before, &mc, start)? {
                Relocation::Lost => return Some(unchanged(current)),
                Relocation::Beyond(by) => {
                    let Some(corrected) = edge.position.checked_add_signed(by) else { return Some(unchanged(current)) };
                    if shift(&mut rows, &mut offset, &mut end, by).is_none() { return Some(unchanged(current)); }
                    cursor = if ask.before { corrected } else { corrected + 1 };
                    continue;
                }
                Relocation::InPage(by) => {
                    let Some(corrected) = edge.position.checked_add_signed(by) else { return Some(unchanged(current)) };
                    if shift(&mut rows, &mut offset, &mut end, by).is_none() { return Some(unchanged(current)); }
                    edge_now = Some(corrected);
                }
            }
        }
        let got = mc.metadata.len().min(count);
        total = mc.total_size.max(0) as usize;
        let raw_end = start.saturating_add(got);
        // The positions this read may add. Forward, those past the edge; backward, those between the
        // edge and the read's end, so a read never repeats the window or passes its edge.
        let (keep_from, keep_to) = match (ask.before, edge_now) {
            (false, Some(corrected)) => (corrected + 1, usize::MAX),
            (false, None) => (start, usize::MAX),
            (true, Some(corrected)) => (corrected.saturating_sub(size).max(start.min(corrected)), corrected),
            (true, None) => (start, start + size),
        };
        let mut fresh = Vec::new();
        for (i, item) in mc.metadata.iter().take(got).enumerate() {
            let position = start + i;
            if !ask.before { end = position + 1; }
            if !(keep_from..keep_to).contains(&position) { continue; }
            if !listable(&item.kind) { continue; }
            let item = parse_item(item, sid);
            if item.title.is_empty() || item.thumb.is_empty() || ask.hidden.contains(&item.sec)
                || rows.iter().any(|(_, row)| row.rk == item.rk) { continue; }
            if ask.before { fresh.push((position, Arc::new(item))); }
            else {
                rows.push((position, Arc::new(item)));
                if rows.len() == MAX_SHELF_ITEMS { break; }
            }
        }
        if ask.before {
            fresh.extend(rows);
            rows = fresh;
            cursor = keep_from;
            offset = keep_from;
            if total > 0 { more = end < total; }
        } else {
            if got == 0 { end = start; }
            more = got > 0 && if total > 0 { end < total } else { end < raw_end || got == count };
            cursor = end;
            if !more { break; }
        }
    }
    Some((rows, PageInfo { offset, end, total, more, unstable: false }))
}

/// Where the edge is now. `page` is the read that started at `from`, covering the edge's old position.
/// The edge is searched for there first, then in one page on the far side of that position: a forward
/// read covers the edge and what follows it, so the far side is the page before it, and a backward
/// read covers the edge and what precedes it, so the far side is the page after it.
fn relocate(fetch: &mut impl FnMut(usize, usize) -> Option<MediaContainer>, edge: &Edge, before: bool,
    page: &MediaContainer, from: usize) -> Option<Relocation> {
    if let Some(index) = find_key(page, &edge.key) {
        return Some(Relocation::InPage(delta(from + index, edge.position)));
    }
    let (start, count) = if before { (edge.position + 1, MAX_SHELF_ITEMS) } else {
        let start = edge.position.saturating_sub(MAX_SHELF_ITEMS);
        (start, edge.position - start)
    };
    if count == 0 { return Some(Relocation::Lost); }
    let far = fetch(start, count)?;
    if far.offset != start as i64 { return None; }
    Some(match find_key(&far, &edge.key) {
        Some(index) => Relocation::Beyond(delta(start + index, edge.position)),
        None => Relocation::Lost,
    })
}

fn find_key(page: &MediaContainer, key: &str) -> Option<usize> {
    page.metadata.iter().position(|item| clean(&item.rating_key) == key)
}

fn delta(now: usize, was: usize) -> isize {
    now as isize - was as isize
}

/// Moves the retained rows and the window's bounds by the edge's change in position.
fn shift(rows: &mut [Row], offset: &mut usize, end: &mut usize, by: isize) -> Option<()> {
    for (position, _) in rows.iter_mut() { *position = position.checked_add_signed(by)?; }
    *offset = offset.checked_add_signed(by)?;
    *end = end.checked_add_signed(by)?;
    Some(())
}

/// The window as it stood, flagged: its edge is not where the listing holds it, so nothing is read on.
fn unchanged(current: Option<(&[Row], PageInfo)>) -> (Vec<Row>, PageInfo) {
    match current {
        Some((rows, info)) => (rows.to_vec(), PageInfo { unstable: true, ..info }),
        None => (Vec::new(), PageInfo { unstable: true, ..PageInfo::default() }),
    }
}

/// A rating key as the ledger keeps it: PMS keys are integers, so they cost eight bytes; a key that
/// is not one stays as the text it came as.
#[derive(Clone, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum LedgerKey {
    Num(u64),
    Text(Box<str>),
}

impl LedgerKey {
    pub fn new(key: &str) -> Self {
        match key.parse::<u64>() {
            Ok(number) if number.to_string() == key => Self::Num(number),
            _ => Self::Text(key.into()),
        }
    }

    pub fn text(&self) -> String {
        match self {
            Self::Num(number) => number.to_string(),
            Self::Text(text) => text.to_string(),
        }
    }
}

/// Consecutive passes over a listing that find nothing new before the row stops looking.
const IDLE_PASSES: u8 = 2;

/// The keys a row has shown, in the order it showed them. While `active` the row's positions are
/// indices into `keys`, and the window moves over the ledger rather than over the listing.
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Ledger {
    pub keys: Vec<LedgerKey>,
    /// Whether the window is read from the ledger (the listing could not be addressed) or the
    /// ledger only records what a stable listing showed, in case it stops being stable.
    #[serde(default)]
    pub active: bool,
    /// The listing offset the next forward read starts at.
    #[serde(default)]
    pub next: usize,
    /// Listing rows the first pass saw and did not show (hidden section, no poster, a type the
    /// shelf does not list). Counted once, so `keys + filtered` reaching `totalSize` ends the walk.
    #[serde(default)]
    pub filtered: usize,
    /// Keys the pass in progress has added.
    #[serde(default)]
    pub added: usize,
    /// Completed passes in a row that added nothing.
    #[serde(default)]
    pub idle: u8,
    /// Passes begun again from offset 0 after the first.
    #[serde(default)]
    pub rescans: u8,
    /// The listing holds nothing the ledger has not seen.
    #[serde(default)]
    pub done: bool,
}

impl Ledger {
    /// A ledger that starts from a window: its rows are what the row has shown so far.
    pub fn from_rows(rows: &[Row], next: usize) -> Self {
        Self { keys: rows.iter().map(|(_, item)| LedgerKey::new(&item.rk)).collect(), next, ..Self::default() }
    }

    pub fn position(&self, key: &str) -> Option<usize> {
        let key = LedgerKey::new(key);
        self.keys.iter().position(|known| *known == key)
    }

    /// Whether the window holds keys the ledger knows in an order other than its own: the listing
    /// the window was read from does not agree with what the row has already shown.
    pub fn contradicts(&self, rows: &[Row]) -> bool {
        let mut last = None;
        rows.iter().filter_map(|(_, item)| self.position(&item.rk)).any(|index| {
            let backwards = last.is_some_and(|last| index <= last);
            last = Some(index);
            backwards
        })
    }

    /// Records the window's keys in the order the window has them. A key already known keeps its
    /// place; one that is new goes beside the known row it sits next to (before the first known
    /// row, after the rest), and a window with nothing known goes at the end.
    pub fn note(&mut self, rows: &[Row]) {
        let keys: Vec<LedgerKey> = rows.iter().map(|(_, item)| LedgerKey::new(&item.rk)).collect();
        let Some(anchor) = keys.iter().position(|key| self.keys.contains(key)) else {
            self.keys.extend(keys);
            return;
        };
        let at = self.keys.iter().position(|k| *k == keys[anchor]).unwrap_or(0);
        for key in keys[..anchor].iter().rev() { self.keys.insert(at, key.clone()); }
        let mut cursor = at + anchor;
        for key in &keys[anchor..] {
            match self.keys.iter().position(|k| k == key) {
                Some(index) => cursor = index + 1,
                None => { self.keys.insert(cursor, key.clone()); cursor += 1; }
            }
        }
    }
}

/// What the server made of one key the ledger asked it for.
enum Fetched {
    /// The server no longer returns it.
    Gone,
    /// It returns it, but not as a card this shelf shows.
    Skip,
    Card(Arc<PmsMovie>),
}

/// One card of a listing row, or `None` when the shelf does not show it.
fn card(sid: ServerId, item: &Metadata, hidden: &[i64]) -> Option<Arc<PmsMovie>> {
    if !listable(&item.kind) { return None; }
    let item = parse_item(item, sid);
    (!item.title.is_empty() && !item.thumb.is_empty() && !hidden.contains(&item.sec)).then(|| Arc::new(item))
}

/// The cards for `keys`, in the order of `keys`: from a response already in hand, else one batch
/// read. A key the batch does not return is `Gone`; the batch's own order is not trusted.
fn materialise(sid: ServerId, keys: &[String], hidden: &[i64], cache: &HashMap<String, Arc<PmsMovie>>,
    many: &mut impl FnMut(&[String]) -> Option<MediaContainer>) -> Option<Vec<Fetched>> {
    let missing: Vec<String> = keys.iter().filter(|key| !cache.contains_key(*key)).cloned().collect();
    let read = if missing.is_empty() { MediaContainer::default() } else { many(&missing)? };
    Some(keys.iter().map(|key| {
        if let Some(item) = cache.get(key) { return Fetched::Card(Arc::clone(item)); }
        match read.metadata.iter().find(|item| clean(&item.rating_key) == *key) {
            None => Fetched::Gone,
            Some(item) => card(sid, item, hidden).map_or(Fetched::Skip, Fetched::Card),
        }
    }).collect())
}

/// The cards of a released row, read again by the rating keys it showed, in the order it showed
/// them. A key the server no longer returns is left out.
pub fn kept_cards(sid: ServerId, keys: &[String], hidden: &[i64],
    many: &mut impl FnMut(&[String]) -> Option<MediaContainer>) -> Option<Vec<Arc<PmsMovie>>> {
    let cards = materialise(sid, keys, hidden, &HashMap::new(), many)?;
    Some(cards.into_iter().filter_map(|fetched| match fetched { Fetched::Card(card) => Some(card), _ => None }).collect())
}

/// Moves a window over a ledger by one ask. `current` is the window as it stands, its positions
/// indices into the ledger. A forward ask keeps the last twelve rows and adds up to twelve: from
/// the ledger where it already holds the keys (a window that moved back), else from the listing,
/// skipping keys the ledger has seen. At the listing's end, if the ledger is shorter than the
/// listing, the listing is read again from 0 for the keys not yet seen. A backward ask keeps the
/// first twelve and re-reads the keys before them through `many`.
///
/// A key `many` no longer returns is dropped from the ledger and the window closes over it, so a
/// card the user is on keeps its place in the window by identity. A server that ignores paging
/// (its response does not start where the read asked, or holds more rows than asked for) sends the
/// whole listing: its keys become the ledger, and the cards are kept only for the window.
///
/// At most eight requests per ask. On `None` the ledger may be partly updated: pass a copy.
pub fn ledger_window(sid: ServerId, ask: &Ask, current: (&[Row], PageInfo), ledger: &mut Ledger,
    mut list: impl FnMut(usize, usize) -> Option<MediaContainer>,
    mut many: impl FnMut(&[String]) -> Option<MediaContainer>) -> Option<(Vec<Row>, PageInfo)> {
    let (retained, info) = current;
    let half = HUB_FETCH_COUNT as usize;
    let mut rows: Vec<Row> = retained.iter().filter(|(_, item)| !ask.hidden.contains(&item.sec)).cloned().collect();
    if ask.before { rows.truncate(half); }
    else if rows.len() > half { rows.drain(..rows.len() - half); }
    let mut cache: HashMap<String, Arc<PmsMovie>> = HashMap::new();
    let mut known: HashSet<LedgerKey> = ledger.keys.iter().cloned().collect();
    let mut total = info.total;
    let mut requests = 0;
    let mut cursor = rows.last().and_then(|(_, item)| ledger.position(&item.rk))
        .map_or(info.end.min(ledger.keys.len()), |i| i + 1);
    let mut first = rows.first().and_then(|(_, item)| ledger.position(&item.rk))
        .unwrap_or(info.offset.min(ledger.keys.len()));
    while rows.len() < MAX_SHELF_ITEMS && requests < 8 {
        if ask.before {
            if first == 0 { break; }
            requests += 1;
            let lo = first.saturating_sub(MAX_SHELF_ITEMS - rows.len());
            let keys: Vec<String> = ledger.keys[lo..first].iter().map(LedgerKey::text).collect();
            let mut fresh = Vec::new();
            for (key, fetched) in keys.iter().zip(materialise(sid, &keys, ask.hidden, &cache, &mut many)?) {
                match fetched {
                    Fetched::Card(item) => fresh.push((0, item)),
                    Fetched::Skip => {}
                    Fetched::Gone => {
                        let gone = LedgerKey::new(key);
                        ledger.keys.retain(|k| *k != gone);
                        known.remove(&gone);
                    }
                }
            }
            fresh.extend(rows);
            rows = fresh;
            first = lo;
            continue;
        }
        if cursor < ledger.keys.len() {
            requests += 1;
            let upto = (cursor + MAX_SHELF_ITEMS - rows.len()).min(ledger.keys.len());
            let keys: Vec<String> = ledger.keys[cursor..upto].iter().map(LedgerKey::text).collect();
            let mut kept = 0;
            for (key, fetched) in keys.iter().zip(materialise(sid, &keys, ask.hidden, &cache, &mut many)?) {
                match fetched {
                    Fetched::Card(item) => { kept += 1; rows.push((0, item)); }
                    Fetched::Skip => kept += 1,
                    Fetched::Gone => {
                        let gone = LedgerKey::new(key);
                        ledger.keys.retain(|k| *k != gone);
                        known.remove(&gone);
                    }
                }
            }
            cursor += kept;
            continue;
        }
        if ledger.done { break; }
        requests += 1;
        let start = ledger.next;
        let mc = list(start, MAX_SHELF_ITEMS)?;
        total = if mc.total_size > 0 { mc.total_size as usize } else { total };
        if mc.offset != start as i64 || mc.metadata.len() > MAX_SHELF_ITEMS {
            // The server ignored the paging: this one response is the whole listing.
            let mut keys = Vec::new();
            known.clear();
            for item in &mc.metadata {
                let key = clean(&item.rating_key);
                let Some(shown) = card(sid, item, ask.hidden) else { continue };
                if known.insert(LedgerKey::new(&key)) {
                    keys.push(LedgerKey::new(&key));
                    cache.insert(key, shown);
                }
            }
            total = mc.metadata.len();
            ledger.keys = keys;
            ledger.next = ledger.keys.len();
            ledger.done = true;
            rows.retain(|(_, item)| known.contains(&LedgerKey::new(&item.rk)));
            cursor = rows.last().and_then(|(_, item)| ledger.position(&item.rk)).map_or(0, |i| i + 1);
            first = rows.first().and_then(|(_, item)| ledger.position(&item.rk)).unwrap_or(cursor);
            continue;
        }
        let mut processed = 0;
        for item in &mc.metadata {
            if rows.len() >= MAX_SHELF_ITEMS { break; }
            processed += 1;
            let key = LedgerKey::new(&clean(&item.rating_key));
            if known.contains(&key) { continue; }
            let Some(shown) = card(sid, item, ask.hidden) else {
                if ledger.rescans == 0 { ledger.filtered += 1; }
                continue;
            };
            known.insert(key.clone());
            ledger.keys.push(key);
            ledger.added += 1;
            rows.push((0, shown));
            cursor = ledger.keys.len();
        }
        ledger.next = start + processed;
        let page_end = processed == mc.metadata.len()
            && (mc.metadata.len() < MAX_SHELF_ITEMS || (total > 0 && ledger.next >= total));
        if page_end {
            ledger.idle = if ledger.added == 0 { ledger.idle + 1 } else { 0 };
            if ledger.keys.len() + ledger.filtered >= total.max(1) || ledger.idle >= IDLE_PASSES {
                ledger.done = true;
            } else {
                ledger.next = 0;
                ledger.added = 0;
                ledger.rescans = ledger.rescans.saturating_add(1);
            }
        }
    }
    // Positions are ledger indices, recomputed because keys may have been dropped from it.
    let mut rows: Vec<Row> = rows.into_iter()
        .filter_map(|(_, item)| ledger.position(&item.rk).map(|position| (position, item))).collect();
    rows.sort_by_key(|(position, _)| *position);
    rows.dedup_by_key(|(position, _)| *position);
    let offset = rows.first().map_or(first.min(ledger.keys.len()), |(position, _)| *position);
    let end = if ask.before { rows.last().map_or(offset, |(position, _)| position + 1) }
        else { cursor.min(ledger.keys.len()) };
    let more = if ask.before { info.more || end < ledger.keys.len() || !ledger.done }
        else { end < ledger.keys.len() || !ledger.done };
    Some((rows, PageInfo { offset, end, total, more, unstable: false }))
}

/// How a row's positions relate to its listing.
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum RowMode {
    /// Positions are listing offsets: the preview is the head of the listing, or the row is a
    /// Recently Added row, which the listing's order defines.
    #[default]
    Head,
    /// A preview not yet compared with its listing; the first forward ask does that.
    Unprobed,
    /// The preview is a sample of the listing (a `random` hub). The row is the preview, kept here
    /// as rating keys in position order, followed by the listing minus those keys; a listing
    /// offset `n` is the row's position `n + preview.len()`.
    Sample(Vec<String>),
}

/// What a row remembers beyond its window.
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct RowState {
    #[serde(default)]
    pub mode: RowMode,
    #[serde(default)]
    pub ledger: Option<Ledger>,
    /// The rating keys of the cards a released row showed, in order, for a row whose listing cannot
    /// say them again (it has no key the pager reads, or it has not been compared with its listing
    /// and may be a random sample). Empty while the row holds cards.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub kept: Vec<String>,
}

impl RowState {
    pub fn is_default(&self) -> bool {
        *self == Self::default()
    }
}
