//! The sliding window behind a Recently Added row: at most [`MAX_SHELF_ITEMS`] cards, each kept with
//! the listing position it was read from, moved by a forward or backward ask that replaces half of
//! it. The window is generic over where its rows come from only through the `fetch` closure the
//! caller passes, so the same logic reads a live hub listing and a test's stand-in server alike.

use std::sync::Arc;

use plx_plex::plex::{MediaContainer, ServerId};

use crate::pms::{listable, parse_item, PmsMovie, HUB_FETCH_COUNT, MAX_SHELF_ITEMS};

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
}

/// One ask against a window: where it starts, which way it moves, and the sections hidden from it.
pub struct Ask<'a> {
    pub start: usize,
    pub before: bool,
    pub hidden: &'a [i64],
}

/// Moves a window by one ask. `current` is the window as it stands, or `None` for a first read;
/// `minimum_end` is the position a forward read must reach before it stops (0 for an ordinary ask).
/// `fetch(start, size)` is one server page. `None` means a page failed or echoed another offset,
/// and the caller keeps what it has.
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
    let mut cursor = ask.start;
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
        let mc = fetch(start, size)?;
        if mc.offset != start as i64 { return None; }
        let got = mc.metadata.len().min(size);
        total = mc.total_size.max(0) as usize;
        let raw_end = start.saturating_add(got);
        let mut fresh = Vec::new();
        for (i, item) in mc.metadata.iter().take(got).enumerate() {
            if !ask.before { end = start + i + 1; }
            if !listable(&item.kind) { continue; }
            let item = parse_item(item, sid);
            if item.title.is_empty() || item.thumb.is_empty() || ask.hidden.contains(&item.sec)
                || rows.iter().any(|(_, row)| row.rk == item.rk) { continue; }
            if ask.before { fresh.push((start + i, Arc::new(item))); }
            else {
                rows.push((start + i, Arc::new(item)));
                if rows.len() == MAX_SHELF_ITEMS { break; }
            }
        }
        if ask.before {
            fresh.extend(rows);
            rows = fresh;
            cursor = start;
            offset = start;
            if total > 0 { more = end < total; }
        } else {
            if got == 0 { end = start; }
            more = got > 0 && if total > 0 { end < total } else { end < raw_end || got == size };
            cursor = end;
            if !more { break; }
        }
    }
    Some((rows, PageInfo { offset, end, total, more }))
}
