//! The sliding window behind a Recently Added row: at most [`MAX_SHELF_ITEMS`] cards, each kept with
//! the listing position it was read from, moved by a forward or backward ask that replaces half of
//! it. The window is generic over where its rows come from only through the `fetch` closure the
//! caller passes, so the same logic reads a live hub listing and a test's stand-in server alike.

use std::sync::Arc;

use plx_plex::plex::{MediaContainer, ServerId};

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
