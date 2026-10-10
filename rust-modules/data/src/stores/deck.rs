//! The Continue Watching deck across servers: a MERGE of per-server offset listings, each sorted by
//! last viewed (descending), held as a bounded window.
//!
//! Each server is a [`Lane`]: the rows it has read of `/hubs/continueWatching/items`, with the
//! positions `lo..hi` marking the ones the deck shows. The deck is the union of every lane's shown
//! rows in one order, [`order`]: last viewed descending, then the lane's roster index, then the
//! row's position, which is total, so a tie cannot change places between two asks.
//!
//! A forward ask adds the next [`PAGE`] cards in that order and lets go of the oldest until the
//! window is back to [`MAX_SHELF_ITEMS`]; a backward ask is the mirror. The merge may only emit a
//! card while every lane that has more rows still holds [`PAGE`] unconsumed rows past its shown
//! ones, which is what makes the merged order equal to sorting everything; a lane that ran out is
//! read first ([`needs`]) and a lane that could not be read is left out of that one move
//! ([`advance`]), so one server failing never stops the others.
//!
//! A lane reads a page that overlaps its edge row by one and expects the same rating key there. If
//! the key is somewhere else (a card was removed from the deck, or a finished one went to the head),
//! every position the lane holds is corrected by the difference and the read goes on; that is the
//! same re-anchor rule as the other rows' (`stores::paging`).

use std::cmp::Ordering;
use std::sync::Arc;

use plx_plex::plex::{MediaContainer, ServerId};

use crate::pms::{clean, listable, parse_item, CwItem, HUB_FETCH_COUNT, MAX_SHELF_ITEMS};

/// Rows per server request, and the lookahead each lane keeps.
pub(crate) const PAGE: usize = HUB_FETCH_COUNT as usize;

/// Requests one read may spend on one lane before it publishes what it has.
const REQUESTS: usize = 8;

/// A raw listing row a lane read: its position and rating key.
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) struct Anchor {
    pub position: usize,
    pub key: String,
}

/// One server's part of the deck.
#[derive(Clone, Default, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Lane {
    /// The rows read and kept, ascending by position. Empty until the deck is placed, when the
    /// source's `cw` (its preview) is the lane's rows.
    #[serde(default)]
    pub(crate) rows: Vec<CwItem>,
    /// The deck shows the rows at positions `lo..hi`.
    #[serde(default)]
    pub lo: usize,
    #[serde(default)]
    pub hi: usize,
    /// The first and the last listing row read (kept or not): where the next read overlaps.
    #[serde(default)]
    pub head: Anchor,
    #[serde(default)]
    pub tail: Anchor,
    /// The listing's `totalSize`, 0 when the server named none.
    #[serde(default)]
    pub total: usize,
    /// The listing's end has been read.
    #[serde(default)]
    pub done: bool,
    /// The deck has been placed on this lane (see [`place`]); until then the preview is the lane.
    #[serde(default)]
    pub placed: bool,
}

impl Lane {
    pub fn is_default(&self) -> bool {
        !self.placed && self.rows.is_empty() && self.tail.key.is_empty() && !self.done && self.total == 0
    }

    /// The lane of a source's first answer: its preview, `raw` rows long, from the listing's head.
    pub fn preview(first_key: &str, last_key: &str, raw: usize, total: usize, done: bool) -> Self {
        Lane {
            head: Anchor { position: 0, key: first_key.into() },
            tail: Anchor { position: raw.saturating_sub(1), key: last_key.into() },
            total, done, ..Lane::default()
        }
    }

    fn ahead(&self) -> usize {
        self.rows.iter().filter(|row| row.position >= self.hi).count()
    }

    fn behind(&self) -> usize {
        self.rows.iter().filter(|row| row.position < self.lo).count()
    }

    fn shown(&self) -> impl Iterator<Item = &CwItem> {
        self.rows.iter().filter(|row| (self.lo..self.hi).contains(&row.position))
    }

    /// Every position the lane holds moves by `by`; `None` if one would leave the listing.
    fn shift(&mut self, by: isize) -> Option<()> {
        for row in &mut self.rows { row.position = row.position.checked_add_signed(by)?; }
        self.lo = self.lo.checked_add_signed(by)?;
        self.hi = self.hi.checked_add_signed(by)?;
        self.head.position = self.head.position.checked_add_signed(by)?;
        self.tail.position = self.tail.position.checked_add_signed(by)?;
        Some(())
    }

    /// Lets go of every row the deck has moved past on the side it came from, so a lane holds only
    /// its shown rows and the lookahead in the direction of travel.
    fn trim(&mut self, forward: bool) {
        if forward {
            self.rows.retain(|row| row.position >= self.lo);
            if let Some(first) = self.rows.first() {
                self.head = Anchor { position: first.position, key: first.m.rk.clone() };
            }
        } else {
            self.rows.retain(|row| row.position < self.hi);
            if let Some(last) = self.rows.last() {
                self.tail = Anchor { position: last.position, key: last.m.rk.clone() };
                self.done = false;
            }
        }
    }
}

/// The deck's order: last viewed descending, then the lane's roster index, then position.
pub(crate) fn order(a: (&CwItem, usize), b: (&CwItem, usize)) -> Ordering {
    b.0.last_viewed_at.cmp(&a.0.last_viewed_at).then(a.1.cmp(&b.1)).then(a.0.position.cmp(&b.0.position))
}

/// Whether a move in `forward`'s direction would have to read `lane` first to be sure of the next
/// `n` cards in merged order.
fn short(lane: &Lane, forward: bool, n: usize) -> bool {
    if forward { !lane.done && lane.ahead() < n } else { lane.head.position > 0 && lane.behind() < n }
}

/// The lanes a move of `n` cards must read before it can run.
pub(crate) fn needs(lanes: &[Lane], forward: bool, n: usize) -> Vec<usize> {
    (0..lanes.len()).filter(|&i| short(&lanes[i], forward, n)).collect()
}

/// The deck as it stands: the shown rows of every lane in merged order, as `(lane, row)`, with
/// whether anything lies before the window and whether anything lies after it.
pub(crate) fn merge_deck(lanes: &[Lane]) -> (Vec<(usize, &CwItem)>, bool, bool) {
    let mut cards: Vec<(usize, &CwItem)> = lanes.iter().enumerate()
        .flat_map(|(i, lane)| lane.shown().map(move |row| (i, row))).collect();
    cards.sort_by(|a, b| order((a.1, a.0), (b.1, b.0)));
    let before = lanes.iter().any(|lane| lane.lo > 0);
    let after = lanes.iter().any(|lane| !lane.done || lane.ahead() > 0);
    (cards, before, after)
}

/// Puts the deck on every lane that has not got it yet: the preview is the lane's rows, nothing
/// shown. With `window` set the deck is then the first [`MAX_SHELF_ITEMS`] cards in merged order
/// (what the merge of the previews showed before any ask).
pub(crate) fn place(lanes: &mut [Lane], previews: &[&[CwItem]]) {
    let mut fresh = false;
    for (lane, preview) in lanes.iter_mut().zip(previews) {
        if lane.placed { continue; }
        fresh = true;
        lane.rows = preview.iter().enumerate().map(|(i, row)| {
            let mut row = row.clone();
            if row.position == 0 { row.position = i; }
            row
        }).collect();
        if lane.tail.key.is_empty() {
            if let Some(last) = lane.rows.last() {
                lane.tail = Anchor { position: last.position, key: last.m.rk.clone() };
            }
            if let Some(first) = lane.rows.first() {
                lane.head = Anchor { position: first.position, key: first.m.rk.clone() };
            }
            lane.done = true;
        }
        (lane.lo, lane.hi, lane.placed) = (0, 0, true);
    }
    if fresh && lanes.iter().all(|lane| lane.hi == 0) { take(lanes, true, MAX_SHELF_ITEMS, &vec![true; lanes.len()]); }
}

/// Emits up to `n` cards in merged order from the lanes marked usable; returns how many.
fn take(lanes: &mut [Lane], forward: bool, n: usize, usable: &[bool]) -> usize {
    let mut taken = 0;
    while taken < n {
        let mut pick: Option<(usize, usize)> = None; // (lane, index into rows)
        for (i, lane) in lanes.iter().enumerate().filter(|(i, _)| usable[*i]) {
            let at = if forward { lane.rows.iter().position(|row| row.position >= lane.hi) }
                else { lane.rows.iter().rposition(|row| row.position < lane.lo) };
            let Some(at) = at else { continue };
            let better = pick.is_none_or(|(j, k)| {
                let ordering = order((&lane.rows[at], i), (&lanes[j].rows[k], j));
                if forward { ordering == Ordering::Less } else { ordering == Ordering::Greater }
            });
            if better { pick = Some((i, at)); }
        }
        let Some((i, at)) = pick else { break };
        let lane = &mut lanes[i];
        let position = lane.rows[at].position;
        if forward { lane.hi = position + 1; } else { lane.lo = position; }
        taken += 1;
    }
    taken
}

/// Lets go of the cards past the bound: the oldest ones on a forward move, the newest on a backward.
fn drop_excess(lanes: &mut [Lane], forward: bool) {
    while lanes.iter().map(|lane| lane.shown().count()).sum::<usize>() > MAX_SHELF_ITEMS {
        let mut pick: Option<(usize, usize)> = None;
        for (i, lane) in lanes.iter().enumerate() {
            let at = if forward { lane.rows.iter().position(|row| (lane.lo..lane.hi).contains(&row.position)) }
                else { lane.rows.iter().rposition(|row| (lane.lo..lane.hi).contains(&row.position)) };
            let Some(at) = at else { continue };
            let better = pick.is_none_or(|(j, k)| {
                let ordering = order((&lane.rows[at], i), (&lanes[j].rows[k], j));
                if forward { ordering == Ordering::Less } else { ordering == Ordering::Greater }
            });
            if better { pick = Some((i, at)); }
        }
        let Some((i, at)) = pick else { break };
        let lane = &mut lanes[i];
        let position = lane.rows[at].position;
        if forward { lane.lo = position + 1; } else { lane.hi = position; }
    }
}

/// Moves the deck by up to `n` cards. A lane that is still [`short`] of rows is left out of this
/// move (the server failed to answer, or has nothing more), so the others go on. Returns how many
/// cards the window gained.
pub(crate) fn advance(lanes: &mut [Lane], forward: bool, n: usize) -> usize {
    let usable: Vec<bool> = lanes.iter().map(|lane| !short(lane, forward, n)).collect();
    let taken = take(lanes, forward, n, &usable);
    drop_excess(lanes, forward);
    for lane in lanes.iter_mut() { lane.trim(forward); }
    taken
}

/// Why a read of one lane stopped.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Fail {
    /// The request failed, or the server answered from another offset.
    Server,
    /// The edge row is nowhere near where it was.
    Lost,
}

fn find(page: &MediaContainer, key: &str) -> Option<usize> {
    page.metadata.iter().position(|item| clean(&item.rating_key) == key)
}

/// One card of a listing row, or `None` when the deck does not show it.
fn row(sid: ServerId, item: &plx_plex::plex::Metadata, position: usize, hidden: &[i64]) -> Option<CwItem> {
    if !listable(&item.kind) { return None; }
    let m = parse_item(item, sid);
    (!m.title.is_empty() && !m.thumb.is_empty() && !hidden.contains(&m.sec))
        .then(|| CwItem { last_viewed_at: item.last_viewed_at, m: Arc::new(m), position })
}

/// Reads `lane` forward until it holds `n` rows past its shown ones, or its listing ends.
pub(crate) fn read_ahead(sid: ServerId, lane: &mut Lane, n: usize, hidden: &[i64],
    mut list: impl FnMut(usize, usize) -> Option<MediaContainer>) -> Result<(), Fail> {
    let mut requests = 0;
    while lane.ahead() < n && !lane.done && requests < REQUESTS {
        requests += 1;
        let overlap = !lane.tail.key.is_empty();
        let (start, count) = if overlap { (lane.tail.position, PAGE + 1) } else { (lane.tail.position, PAGE) };
        let mc = list(start, count).filter(|mc| mc.offset == start as i64).ok_or(Fail::Server)?;
        if overlap {
            match find(&mc, &lane.tail.key) {
                Some(0) => {}
                Some(j) => { lane.shift((start + j) as isize - start as isize).ok_or(Fail::Lost)?; }
                None => {
                    // The edge is behind where it was read: one page on that side.
                    let far = lane.tail.position.saturating_sub(PAGE);
                    if lane.tail.position == far { return Err(Fail::Lost); }
                    let page = list(far, lane.tail.position - far).filter(|mc| mc.offset == far as i64)
                        .ok_or(Fail::Server)?;
                    let j = find(&page, &lane.tail.key).ok_or(Fail::Lost)?;
                    lane.shift((far + j) as isize - lane.tail.position as isize).ok_or(Fail::Lost)?;
                    continue;
                }
            }
        }
        let after = if overlap { lane.tail.position } else { usize::MAX };
        let mut last = None;
        for (i, item) in mc.metadata.iter().take(count).enumerate() {
            let position = start + i;
            if overlap && position <= after { continue; }
            last = Some(Anchor { position, key: clean(&item.rating_key) });
            let Some(fresh) = row(sid, item, position, hidden) else { continue };
            if lane.rows.iter().all(|held| held.m.rk != fresh.m.rk) { lane.rows.push(fresh); }
        }
        lane.total = mc.total_size.max(0) as usize;
        match last {
            Some(last) => {
                lane.done = if lane.total > 0 { last.position + 1 >= lane.total } else { mc.metadata.len() < count };
                lane.tail = last;
            }
            None => lane.done = true,
        }
    }
    Ok(())
}

/// Reads `lane` backward until it holds `n` rows before its shown ones, or reaches its first row.
pub(crate) fn read_behind(sid: ServerId, lane: &mut Lane, n: usize, hidden: &[i64],
    mut list: impl FnMut(usize, usize) -> Option<MediaContainer>) -> Result<(), Fail> {
    let mut requests = 0;
    while lane.head.position > 0 && lane.behind() < n && requests < REQUESTS {
        requests += 1;
        let start = lane.head.position.saturating_sub(PAGE);
        let count = lane.head.position - start + 1;
        let mc = list(start, count).filter(|mc| mc.offset == start as i64).ok_or(Fail::Server)?;
        match find(&mc, &lane.head.key) {
            Some(j) if j + 1 == mc.metadata.len().min(count) => {}
            Some(j) => { lane.shift((start + j) as isize - lane.head.position as isize).ok_or(Fail::Lost)?; }
            None => {
                // The edge is ahead of where it was read: one page on that side.
                let far = lane.head.position + 1;
                let page = list(far, PAGE).filter(|mc| mc.offset == far as i64).ok_or(Fail::Server)?;
                let j = find(&page, &lane.head.key).ok_or(Fail::Lost)?;
                lane.shift((far + j) as isize - lane.head.position as isize).ok_or(Fail::Lost)?;
                continue;
            }
        }
        let anchor = lane.head.position;
        let mut fresh = Vec::new();
        for (i, item) in mc.metadata.iter().enumerate() {
            let position = start + i;
            if position >= anchor { break; }
            if let Some(card) = row(sid, item, position, hidden) {
                if lane.rows.iter().all(|held| held.m.rk != card.m.rk) { fresh.push(card); }
            }
        }
        fresh.append(&mut lane.rows);
        lane.rows = fresh;
        lane.head = mc.metadata.first().map_or(Anchor { position: start, key: String::new() },
            |item| Anchor { position: start, key: clean(&item.rating_key) });
        lane.total = if mc.total_size > 0 { mc.total_size as usize } else { lane.total };
    }
    Ok(())
}
