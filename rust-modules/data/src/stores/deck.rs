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
//! same re-anchor rule as the other rows' (`stores::paging`). A lane whose edge row is nowhere near
//! where it was moves onto its ledger: the keys it has emitted, in order. From then on it reads the
//! listing from 0 and skips what the ledger holds, and a card behind the window is read back by key,
//! so every item is still reached and none appears twice.
//!
//! A refresh does not snap a moved deck back to its head: [`reload`] reads a lane again at its own
//! offsets and seats the window on the cards it showed, by rating key.

use std::cmp::Ordering;
use std::collections::HashSet;
use std::sync::Arc;

use plx_plex::plex::{MediaContainer, Metadata, ServerId};

use crate::pms::{clean, listable, parse_item, CwItem, HUB_FETCH_COUNT, MAX_SHELF_ITEMS};

/// Rows per server request, and the lookahead each lane keeps.
pub(crate) const PAGE: usize = HUB_FETCH_COUNT as usize;

/// Requests one read may spend on one lane before it publishes what it has.
const REQUESTS: usize = 8;

fn is_zero(n: &usize) -> bool { *n == 0 }

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
    /// The ledger: the rating keys the deck has emitted forward from this lane, in order, and one
    /// past the highest position among them.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub seen: Vec<String>,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub reach: usize,
    /// On the ledger: the listing offset the next read starts at. Positions are no longer listing
    /// offsets then, only an order.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scan: Option<usize>,
}

impl Lane {
    pub fn is_default(&self) -> bool {
        !self.placed && self.rows.is_empty() && self.tail.key.is_empty() && !self.done && self.total == 0
            && self.seen.is_empty() && self.scan.is_none()
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

    /// Whether the lane has anything before its earliest held row.
    fn has_before(&self) -> bool {
        if self.scan.is_none() { return self.head.position > 0; }
        self.rows.first().and_then(|first| self.seen.iter().position(|key| *key == first.m.rk))
            .is_some_and(|at| at > 0)
    }

    /// Every position the lane holds moves by `by`; `None` if one would leave the listing.
    fn shift(&mut self, by: isize) -> Option<()> {
        for row in &mut self.rows { row.position = row.position.checked_add_signed(by)?; }
        self.lo = self.lo.checked_add_signed(by)?;
        self.hi = self.hi.checked_add_signed(by)?;
        self.reach = self.reach.checked_add_signed(by)?;
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
                if self.scan.is_none() { self.done = false; }
            }
        }
    }

    /// Takes the row `gone` picks out of the lane (a card the user removed from the deck): the
    /// positions after it, the window and the anchors close up by one. True if the lane held it.
    pub fn remove(&mut self, gone: impl Fn(&CwItem) -> bool) -> bool {
        let Some(at) = self.rows.iter().position(|row| gone(row)) else { return false };
        let (position, key) = (self.rows[at].position, self.rows[at].m.rk.clone());
        self.rows.remove(at);
        for row in self.rows.iter_mut().filter(|row| row.position > position) { row.position -= 1; }
        if position < self.lo { self.lo -= 1; }
        if position < self.hi { self.hi -= 1; }
        if position < self.reach { self.reach -= 1; }
        self.total = self.total.saturating_sub(1);
        let close = |anchor: &mut Anchor, rows: &[CwItem], last: bool| {
            if anchor.key == key {
                let near = if last { rows.iter().rev().find(|row| row.position <= position) } else { rows.iter().find(|row| row.position >= position) };
                *anchor = near.map_or(Anchor { position: position.saturating_sub(1), key: String::new() },
                    |row| Anchor { position: row.position, key: row.m.rk.clone() });
            } else if anchor.position > position {
                anchor.position -= 1;
            }
        };
        close(&mut self.head, &self.rows, false);
        close(&mut self.tail, &self.rows, true);
        true
    }

    /// Moves the lane onto its ledger: the lookahead is let go (the listing no longer holds it where
    /// it was), positions leave room for the cards the ledger can read back, and the listing is
    /// scanned again from 0 for what the ledger does not hold.
    fn to_ledger(&mut self) -> Result<(), Fail> {
        self.rows.retain(|row| row.position < self.hi);
        self.shift(self.seen.len() as isize).ok_or(Fail::Server)?;
        let last = self.rows.last().map_or(0, |row| row.position);
        self.tail = Anchor { position: last.max(self.hi.saturating_sub(1)), key: String::new() };
        self.scan = Some(0);
        self.done = false;
        Ok(())
    }
}

/// The deck's order: last viewed descending, then the lane's roster index, then position.
pub(crate) fn order(a: (&CwItem, usize), b: (&CwItem, usize)) -> Ordering {
    b.0.last_viewed_at.cmp(&a.0.last_viewed_at).then(a.1.cmp(&b.1)).then(a.0.position.cmp(&b.0.position))
}

/// Whether a move in `forward`'s direction would have to read `lane` first to be sure of the next
/// `n` cards in merged order.
fn short(lane: &Lane, forward: bool, n: usize) -> bool {
    if forward { !lane.done && lane.ahead() < n } else { lane.has_before() && lane.behind() < n }
}

/// The lanes a move of `n` cards must read before it can run.
pub(crate) fn needs(lanes: &[Lane], forward: bool, n: usize) -> Vec<usize> {
    (0..lanes.len()).filter(|&i| short(&lanes[i], forward, n)).collect()
}

/// The deck as it stands: the shown rows of every lane in merged order, as `(lane, row)`, with
/// whether anything lies before the window and whether anything lies after it.
pub(crate) fn merge_deck<'a>(lanes: &[&'a Lane]) -> (Vec<(usize, &'a CwItem)>, bool, bool) {
    let mut cards: Vec<(usize, &CwItem)> = lanes.iter().enumerate()
        .flat_map(|(i, lane)| lane.shown().map(move |row| (i, row))).collect();
    cards.sort_by(|a, b| order((a.1, a.0), (b.1, b.0)));
    let before = lanes.iter().any(|lane| lane.lo > 0 || lane.has_before());
    let after = lanes.iter().any(|lane| !lane.done || lane.ahead() > 0);
    (cards, before, after)
}

fn shown_count(lanes: &[Lane]) -> usize { lanes.iter().map(|lane| lane.shown().count()).sum() }

/// Puts the deck on every lane that has not got it yet. The preview is the lane's rows. With no lane
/// placed yet the deck is the first [`MAX_SHELF_ITEMS`] cards in merged order (what the merge of the
/// previews showed before any ask); a lane that joins a deck already placed shows the rows that fall
/// inside the window's span and holds the rest as lookahead or as rows behind.
pub(crate) fn place(lanes: &mut [Lane], previews: &[&[CwItem]]) {
    let mut fresh = Vec::new();
    for (i, (lane, preview)) in lanes.iter_mut().zip(previews).enumerate() {
        if lane.placed { continue; }
        if lane.rows.is_empty() {
            lane.rows = preview.iter().enumerate().map(|(at, row)| {
                let mut row = row.clone();
                if row.position == 0 { row.position = at; }
                row
            }).collect();
        }
        if lane.tail.key.is_empty() {
            // A source that named no listing: its preview is everything it has.
            if let Some(last) = lane.rows.last() {
                lane.tail = Anchor { position: last.position, key: last.m.rk.clone() };
            }
            if let Some(first) = lane.rows.first() {
                lane.head = Anchor { position: first.position, key: first.m.rk.clone() };
            }
            lane.done = true;
        }
        (lane.lo, lane.hi, lane.placed) = (0, 0, true);
        fresh.push(i);
    }
    if fresh.is_empty() { return; }
    if fresh.len() == lanes.len() {
        take(lanes, true, MAX_SHELF_ITEMS, &vec![true; lanes.len()]);
        return;
    }
    let bounds = {
        let held: Vec<&Lane> = lanes.iter().enumerate().filter(|(i, _)| !fresh.contains(i)).map(|(_, l)| l).collect();
        let index: Vec<usize> = (0..lanes.len()).filter(|i| !fresh.contains(i)).collect();
        let (cards, _, _) = merge_deck(&held);
        match (cards.first(), cards.last()) {
            (Some(&(fi, f)), Some(&(li, l))) => Some(((f.clone(), index[fi]), (l.clone(), index[li]))),
            _ => None,
        }
    };
    let Some((first, last)) = bounds else {
        take(lanes, true, MAX_SHELF_ITEMS, &vec![true; lanes.len()]);
        return;
    };
    for &i in &fresh {
        let lane = &mut lanes[i];
        let before = lane.rows.iter().filter(|row| order((row, i), (&first.0, first.1)) == Ordering::Less).count();
        let upto = lane.rows.iter().filter(|row| order((row, i), (&last.0, last.1)) != Ordering::Greater).count();
        let end = lane.tail.position + 1;
        lane.lo = lane.rows.get(before).map_or(end, |row| row.position);
        lane.hi = lane.rows.get(upto.max(before)).map_or(end, |row| row.position);
    }
    drop_excess(lanes, true);
}

/// Emits up to `n` cards in merged order from the lanes marked usable; returns how many. It stops
/// where a lane that still has rows has none read, since the next card cannot be known.
fn take(lanes: &mut [Lane], forward: bool, n: usize, usable: &[bool]) -> usize {
    let mut taken = 0;
    while taken < n {
        let mut pick: Option<(usize, usize)> = None; // (lane, index into rows)
        let mut blind = false;
        for (i, lane) in lanes.iter().enumerate().filter(|(i, _)| usable[*i]) {
            let at = if forward { lane.rows.iter().position(|row| row.position >= lane.hi) }
                else { lane.rows.iter().rposition(|row| row.position < lane.lo) };
            let Some(at) = at else {
                blind |= if forward { !lane.done } else { lane.has_before() };
                continue;
            };
            let better = pick.is_none_or(|(j, k)| {
                let ordering = order((&lane.rows[at], i), (&lanes[j].rows[k], j));
                if forward { ordering == Ordering::Less } else { ordering == Ordering::Greater }
            });
            if better { pick = Some((i, at)); }
        }
        let Some((i, at)) = pick.filter(|_| !blind) else { break };
        let lane = &mut lanes[i];
        let position = lane.rows[at].position;
        if forward {
            if position >= lane.reach {
                lane.seen.push(lane.rows[at].m.rk.clone());
                lane.reach = position + 1;
            }
            lane.hi = position + 1;
        } else {
            lane.lo = position;
        }
        taken += 1;
    }
    taken
}

/// Lets go of the cards past the bound: the oldest ones on a forward move, the newest on a backward.
fn drop_excess(lanes: &mut [Lane], forward: bool) {
    while shown_count(lanes) > MAX_SHELF_ITEMS {
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

/// Brings the window back to the bound after lanes were rebuilt: cards past it are let go from the
/// old end, and a window that lost cards (finished or removed ones) is filled from the lookahead.
pub(crate) fn fit(lanes: &mut [Lane]) {
    // A deck at its head keeps its newest cards; one that has moved keeps the cards it was moving to.
    let at_head = lanes.iter().all(|lane| lane.lo == 0 && !lane.has_before());
    drop_excess(lanes, !at_head);
    let shown = shown_count(lanes);
    if shown < MAX_SHELF_ITEMS {
        let usable: Vec<bool> = lanes.iter().map(|lane| !short(lane, true, MAX_SHELF_ITEMS - shown)).collect();
        take(lanes, true, MAX_SHELF_ITEMS - shown, &usable);
    }
}

/// Why a read of one lane stopped.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Fail {
    /// The request failed, or the server answered from another offset.
    Server,
    /// The window cannot be seated: a position would leave the listing, or the cards it showed are
    /// nowhere in what the server answers.
    Lost,
}

fn find(page: &MediaContainer, key: &str) -> Option<usize> {
    page.metadata.iter().position(|item| clean(&item.rating_key) == key)
}

/// One card of a listing row, or `None` when the deck does not show it.
fn row(sid: ServerId, item: &Metadata, position: usize, hidden: &[i64]) -> Option<CwItem> {
    if !listable(&item.kind) { return None; }
    let m = parse_item(item, sid);
    (!m.title.is_empty() && !m.thumb.is_empty() && !hidden.contains(&m.sec))
        .then(|| CwItem { last_viewed_at: item.last_viewed_at, m: Arc::new(m), position })
}

/// Reads `lane` forward until it holds `n` rows past its shown ones, or its listing ends. A lane
/// whose edge is lost goes onto its ledger and the read goes on there.
pub(crate) fn read_ahead(sid: ServerId, lane: &mut Lane, n: usize, hidden: &[i64],
    mut list: impl FnMut(usize, usize) -> Option<MediaContainer>) -> Result<(), Fail> {
    let mut requests = 0;
    while lane.ahead() < n && !lane.done && requests < REQUESTS {
        requests += 1;
        if let Some(scan) = lane.scan {
            scan_ledger(sid, lane, scan, hidden, &mut list)?;
            continue;
        }
        let overlap = !lane.tail.key.is_empty();
        let (start, count) = if overlap { (lane.tail.position, PAGE + 1) } else { (lane.tail.position, PAGE) };
        let mc = list(start, count).filter(|mc| mc.offset == start as i64).ok_or(Fail::Server)?;
        if overlap {
            match find(&mc, &lane.tail.key) {
                Some(0) => {}
                Some(j) => lane.shift(j as isize).ok_or(Fail::Lost)?,
                None => {
                    // The edge is behind where it was read: one page on that side.
                    let far = lane.tail.position.saturating_sub(PAGE);
                    let found = if far < lane.tail.position {
                        let page = list(far, lane.tail.position - far).filter(|mc| mc.offset == far as i64)
                            .ok_or(Fail::Server)?;
                        find(&page, &lane.tail.key)
                    } else { None };
                    match found {
                        Some(j) => lane.shift((far + j) as isize - lane.tail.position as isize).ok_or(Fail::Lost)?,
                        None => lane.to_ledger()?,
                    }
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

/// One page of the ledger scan: listing rows the ledger does not hold and the lane does not already
/// hold, appended in listing order.
fn scan_ledger(sid: ServerId, lane: &mut Lane, scan: usize, hidden: &[i64],
    list: &mut impl FnMut(usize, usize) -> Option<MediaContainer>) -> Result<(), Fail> {
    let mc = list(scan, PAGE).filter(|mc| mc.offset == scan as i64).ok_or(Fail::Server)?;
    let known: HashSet<String> = lane.seen.iter().cloned().chain(lane.rows.iter().map(|r| r.m.rk.clone())).collect();
    let mut next = lane.tail.position + 1;
    for item in mc.metadata.iter().take(PAGE) {
        if known.contains(&clean(&item.rating_key)) { continue; }
        if let Some(fresh) = row(sid, item, next, hidden) {
            lane.rows.push(fresh);
            lane.tail.position = next;
            next += 1;
        }
    }
    let end = scan + mc.metadata.len().min(PAGE);
    lane.total = mc.total_size.max(0) as usize;
    lane.scan = Some(end);
    lane.done = if lane.total > 0 { end >= lane.total } else { mc.metadata.len() < PAGE };
    Ok(())
}

/// Reads `lane` backward until it holds `n` rows before its shown ones, or reaches its first row.
/// On the ledger the cards come back by rating key through `many` (one row per key).
pub(crate) fn read_behind(sid: ServerId, lane: &mut Lane, n: usize, hidden: &[i64],
    mut list: impl FnMut(usize, usize) -> Option<MediaContainer>,
    mut many: impl FnMut(&[String]) -> Option<MediaContainer>) -> Result<(), Fail> {
    let mut requests = 0;
    while lane.has_before() && lane.behind() < n && requests < REQUESTS {
        requests += 1;
        if lane.scan.is_some() {
            read_back(sid, lane, hidden, &mut many)?;
            continue;
        }
        let start = lane.head.position.saturating_sub(PAGE);
        let count = lane.head.position - start + 1;
        let mc = list(start, count).filter(|mc| mc.offset == start as i64).ok_or(Fail::Server)?;
        match find(&mc, &lane.head.key) {
            Some(j) if j + 1 == mc.metadata.len().min(count) => {}
            Some(j) => lane.shift((start + j) as isize - lane.head.position as isize).ok_or(Fail::Lost)?,
            None => {
                // The edge is ahead of where it was read: one page on that side.
                let far = lane.head.position + 1;
                let page = list(far, PAGE).filter(|mc| mc.offset == far as i64).ok_or(Fail::Server)?;
                match find(&page, &lane.head.key) {
                    Some(j) => lane.shift((far + j) as isize - lane.head.position as isize).ok_or(Fail::Lost)?,
                    None => lane.to_ledger()?,
                }
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

/// A ledger lane reads back the keys just before its earliest held row.
fn read_back(sid: ServerId, lane: &mut Lane, hidden: &[i64],
    many: &mut impl FnMut(&[String]) -> Option<MediaContainer>) -> Result<(), Fail> {
    let Some(first) = lane.rows.first() else { return Err(Fail::Lost) };
    let at = lane.seen.iter().position(|key| *key == first.m.rk).ok_or(Fail::Lost)?;
    let from = at.saturating_sub(PAGE);
    let keys = lane.seen[from..at].to_vec();
    let mc = many(&keys).ok_or(Fail::Server)?;
    let base = first.position;
    let mut back = Vec::new();
    for (i, key) in keys.iter().enumerate() {
        let Some(item) = mc.metadata.iter().find(|item| clean(&item.rating_key) == *key) else { continue };
        let position = base.checked_sub(keys.len() - i).ok_or(Fail::Lost)?;
        if let Some(card) = row(sid, item, position, hidden) { back.push(card); }
    }
    back.append(&mut lane.rows);
    lane.rows = back;
    // A key the server no longer has is not asked for again.
    lane.seen.drain(from..at);
    Ok(())
}

/// Reads `old` again at its own offsets and seats the window on the cards it showed, by rating key.
/// The progress of every card is the server's present answer; a card that left the listing is gone;
/// a card that moved is found again. `Fail::Lost` (none of the shown cards is where it can be found)
/// leaves the caller to keep the lane it had.
pub(crate) fn reload(sid: ServerId, old: &Lane, hidden: &[i64],
    mut list: impl FnMut(usize, usize) -> Option<MediaContainer>) -> Result<Lane, Fail> {
    if old.scan.is_some() || old.rows.is_empty() { return Ok(old.clone()); }
    let start = if old.lo == 0 { 0 } else { old.head.position };
    let want = (old.tail.position + 1).saturating_sub(start).max(1);
    let mut read: Vec<(usize, Metadata)> = Vec::new();
    let mut total = old.total;
    let mut requests = 0;
    let shown_keys: Vec<&str> = old.shown().map(|row| row.m.rk.as_str()).collect();
    // Read until the span the lane held is covered AND the last card it showed is found: a card
    // that moved down the listing may now lie past that span.
    let found = |read: &[(usize, Metadata)]| shown_keys.last()
        .is_none_or(|key| read.iter().any(|(_, item)| clean(&item.rating_key) == *key));
    while (read.len() < want || !found(&read)) && requests < REQUESTS {
        requests += 1;
        let at = start + read.len();
        let size = want.saturating_sub(read.len()).clamp(1, PAGE);
        let mc = list(at, size).filter(|mc| mc.offset == at as i64).ok_or(Fail::Server)?;
        if mc.total_size > 0 { total = mc.total_size as usize; }
        let page = mc.metadata.len();
        read.extend(mc.metadata.into_iter().enumerate().map(|(i, item)| (at + i, item)));
        if page < size { break; }
    }
    let Some((last_position, last_item)) = read.last() else { return Err(Fail::Server) };
    let mut lane = Lane { placed: true, seen: old.seen.clone(), ..Lane::default() };
    for (position, item) in &read {
        if let Some(card) = row(sid, item, *position, hidden) {
            if lane.rows.iter().all(|held| held.m.rk != card.m.rk) { lane.rows.push(card); }
        }
    }
    lane.head = Anchor { position: start, key: clean(&read[0].1.rating_key) };
    lane.tail = Anchor { position: *last_position, key: clean(&last_item.rating_key) };
    lane.total = total;
    lane.done = if total > 0 { last_position + 1 >= total } else { read.len() < want };
    let at = |key: &str| lane.rows.iter().find(|row| row.m.rk == key).map(|row| row.position);
    let (first, last) = (shown_keys.iter().find_map(|key| at(key)), shown_keys.iter().rev().find_map(|key| at(key)));
    if !shown_keys.is_empty() && first.is_none() { return Err(Fail::Lost); }
    let lo = if old.lo == 0 { 0 } else { first.unwrap_or(0) };
    let hi = last.map_or(lo, |p| p + 1);
    (lane.lo, lane.hi, lane.reach) = (lo, hi, hi);
    Ok(lane)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    fn sid(slot: u32) -> ServerId { ServerId::from_raw(slot as u16) }

    /// One server's listing, newest first: `(rating key, last viewed, view offset)`.
    struct Server {
        slot: u32,
        items: Vec<(String, i64, i64)>,
        /// Requests served, and the number after which every request fails.
        calls: Cell<usize>,
        fail_after: Cell<usize>,
    }

    impl Server {
        fn new(slot: u32, viewed: &[i64]) -> Server {
            let mut items: Vec<(String, i64, i64)> = viewed.iter().enumerate()
                .map(|(i, v)| (format!("s{slot}-{i}"), *v, 0)).collect();
            items.sort_by_key(|item| std::cmp::Reverse(item.1));
            Server { slot, items, calls: Cell::new(0), fail_after: Cell::new(usize::MAX) }
        }

        fn card(&self, item: &(String, i64, i64)) -> serde_json::Value {
            serde_json::json!({"ratingKey": item.0, "type": "movie", "title": "T", "thumb": "/p",
                "lastViewedAt": item.1, "viewOffset": item.2})
        }

        fn gate(&self) -> bool {
            self.calls.set(self.calls.get() + 1);
            self.calls.get() <= self.fail_after.get()
        }

        fn list(&self, start: usize, size: usize) -> Option<MediaContainer> {
            if !self.gate() { return None; }
            let metadata: Vec<_> = self.items.iter().skip(start).take(size).map(|item| self.card(item)).collect();
            serde_json::from_value(serde_json::json!({"offset": start, "totalSize": self.items.len(),
                "Metadata": metadata})).ok()
        }

        fn many(&self, keys: &[String]) -> Option<MediaContainer> {
            if !self.gate() { return None; }
            let metadata: Vec<_> = keys.iter().filter_map(|key| self.items.iter().find(|item| item.0 == *key))
                .map(|item| self.card(item)).collect();
            serde_json::from_value(serde_json::json!({"Metadata": metadata})).ok()
        }

        fn remove(&mut self, key: &str) { self.items.retain(|item| item.0 != key); }

        fn lane(&self) -> Lane {
            let mc = self.list(0, PAGE).unwrap();
            let rows: Vec<CwItem> = mc.metadata.iter().enumerate()
                .filter_map(|(i, item)| row(sid(self.slot), item, i, &[])).collect();
            let key = |item: Option<&Metadata>| item.map_or_else(String::new, |item| clean(&item.rating_key));
            let more = mc.metadata.len() >= PAGE || mc.total_size as usize > mc.metadata.len();
            let mut lane = Lane::preview(&key(mc.metadata.first()), &key(mc.metadata.last()),
                mc.metadata.len(), mc.total_size as usize, !more);
            lane.rows = rows;
            if !lane.done {
                read_ahead(sid(self.slot), &mut lane, MAX_SHELF_ITEMS, &[], |s, n| self.list(s, n)).unwrap();
            }
            lane
        }
    }

    fn deck(servers: &[&Server]) -> Vec<Lane> {
        let mut lanes: Vec<Lane> = servers.iter().map(|s| s.lane()).collect();
        let none = vec![&[][..]; lanes.len()];
        place(&mut lanes, &none);
        lanes
    }

    fn keys(lanes: &[Lane]) -> Vec<String> {
        let refs: Vec<&Lane> = lanes.iter().collect();
        merge_deck(&refs).0.iter().map(|(_, row)| row.m.rk.clone()).collect()
    }

    /// One ask, as the store runs it: read the lanes that must be read, then move.
    fn ask(lanes: &mut [Lane], servers: &[&Server], forward: bool) -> usize {
        for i in needs(lanes, forward, PAGE) {
            let server = servers[i];
            let _ = if forward {
                read_ahead(sid(server.slot), &mut lanes[i], PAGE, &[], |s, n| server.list(s, n))
            } else {
                read_behind(sid(server.slot), &mut lanes[i], PAGE, &[], |s, n| server.list(s, n),
                    |k| server.many(k))
            };
        }
        advance(lanes, forward, PAGE)
    }

    /// The order a full sort of every server's listing gives.
    fn sorted(servers: &[&Server]) -> Vec<String> {
        let mut all: Vec<(i64, usize, usize, String)> = Vec::new();
        for (i, server) in servers.iter().enumerate() {
            for (at, item) in server.items.iter().enumerate() { all.push((-item.1, i, at, item.0.clone())); }
        }
        all.sort();
        all.into_iter().map(|item| item.3).collect()
    }

    /// Walks the deck forward to its end; the cards in the order they first appeared.
    fn walk_forward(lanes: &mut [Lane], servers: &[&Server]) -> Vec<String> {
        let mut seen: Vec<String> = Vec::new();
        for _ in 0..200 {
            for key in keys(lanes) { if !seen.contains(&key) { seen.push(key); } }
            assert!(keys(lanes).len() <= MAX_SHELF_ITEMS);
            if ask(lanes, servers, true) == 0 { break; }
        }
        seen
    }

    fn viewed(n: usize, step: i64, base: i64) -> Vec<i64> { (0..n as i64).map(|i| base - i * step).collect() }

    #[test]
    fn the_merged_order_equals_a_full_sort_for_one_two_and_five_servers() {
        for count in [1usize, 2, 5] {
            // Few distinct timestamps, so most cards tie and only the roster index and position order them.
            let servers: Vec<Server> = (0..count).map(|i| Server::new(i as u32, &(0..37 + i * 9)
                .map(|j| 1000 - (j as i64 / 3) * 5 - (i as i64 % 2)).collect::<Vec<_>>())).collect();
            let refs: Vec<&Server> = servers.iter().collect();
            let mut lanes = deck(&refs);
            let walked = walk_forward(&mut lanes, &refs);
            assert_eq!(walked, sorted(&refs), "{count} servers");
        }
    }

    #[test]
    fn a_sixty_item_deck_walks_to_the_end_and_back_showing_every_item_once() {
        let a = Server::new(1, &viewed(35, 10, 5000));
        let b = Server::new(2, &viewed(25, 14, 4995));
        let refs = [&a, &b];
        let mut lanes = deck(&refs);
        let first = keys(&lanes);
        let walked = walk_forward(&mut lanes, &refs);
        assert_eq!(walked.len(), 60);
        assert_eq!(walked, sorted(&refs));
        let mut back: Vec<String> = keys(&lanes);
        for _ in 0..200 {
            if ask(&mut lanes, &refs, false) == 0 { break; }
            assert!(keys(&lanes).len() <= MAX_SHELF_ITEMS);
            for key in keys(&lanes) { if !back.contains(&key) { back.push(key); } }
        }
        assert_eq!(keys(&lanes), first, "back at the head");
        assert_eq!(back.len(), 60, "every item reached on the way back");
    }

    #[test]
    fn a_removal_before_the_window_between_asks_skips_nothing_and_repeats_nothing() {
        let mut a = Server::new(1, &viewed(40, 10, 5000));
        let b = Server::new(2, &viewed(30, 13, 4990));
        let mut lanes = {
            let refs = [&a, &b];
            let mut lanes = deck(&refs);
            ask(&mut lanes, &refs, true);
            ask(&mut lanes, &refs, true);
            lanes
        };
        let focused = keys(&lanes).last().cloned().unwrap();
        let mut shown = keys(&lanes);
        // Two cards left the listing while the user sat on the window.
        let gone = [a.items[1].0.clone(), a.items[3].0.clone()];
        for key in &gone { a.remove(key); }
        let refs = [&a, &b];
        for _ in 0..200 {
            if ask(&mut lanes, &refs, true) == 0 { break; }
            for key in keys(&lanes) { if !shown.contains(&key) { shown.push(key); } }
            assert!(keys(&lanes).contains(&focused) || shown.len() > 40, "the focused card keeps its place");
        }
        let after: Vec<String> = sorted(&refs);
        let window_start = after.iter().position(|key| *key == shown[0]).unwrap();
        // Everything after the first window's start appears, in order, exactly once.
        assert_eq!(shown.iter().filter(|key| !gone.contains(key)).cloned().collect::<Vec<_>>(),
            after[window_start..].to_vec());
    }

    #[test]
    fn an_item_that_reaches_the_head_between_asks_repeats_nothing_and_skips_nothing() {
        let mut a = Server::new(1, &viewed(60, 10, 5000));
        let mut lanes = {
            let refs = [&a];
            let mut lanes = deck(&refs);
            ask(&mut lanes, &refs, true);
            lanes
        };
        let mut shown = keys(&lanes);
        // Something was played elsewhere: it is the newest item now and every other one is one place down.
        a.items.insert(0, ("s1-played".into(), 9999, 0));
        let refs = [&a];
        for _ in 0..30 {
            if ask(&mut lanes, &refs, true) == 0 { break; }
            for key in keys(&lanes) { if !shown.contains(&key) { shown.push(key); } }
            for held in &lanes[0].rows {
                assert_eq!(a.items.iter().position(|item| item.0 == held.m.rk), Some(held.position),
                    "{} keeps its listing offset", held.m.rk);
            }
        }
        let names: Vec<String> = a.items.iter().skip(1).map(|item| item.0.clone()).collect();
        assert_eq!(shown, names[12..].to_vec());
    }

    #[test]
    fn one_server_failing_leaves_the_other_servers_items_all_reachable() {
        let a = Server::new(1, &viewed(50, 10, 5000));
        let b = Server::new(2, &viewed(50, 10, 4995));
        let mut lanes = deck(&[&a, &b]);
        b.fail_after.set(0);
        let refs = [&a, &b];
        let walked = walk_forward(&mut lanes, &refs);
        for item in &a.items { assert!(walked.contains(&item.0), "{} unreachable", item.0); }
        assert!(walked.len() == walked.iter().collect::<std::collections::HashSet<_>>().len());
    }

    #[test]
    fn a_single_lane_behaves_exactly_as_an_offset_listing() {
        let a = Server::new(1, &viewed(70, 3, 9000));
        let refs = [&a];
        let mut lanes = deck(&refs);
        let names: Vec<String> = a.items.iter().map(|item| item.0.clone()).collect();
        assert_eq!(keys(&lanes), names[..24]);
        ask(&mut lanes, &refs, true);
        assert_eq!(keys(&lanes), names[12..36]);
        ask(&mut lanes, &refs, true);
        assert_eq!(keys(&lanes), names[24..48]);
        ask(&mut lanes, &refs, false);
        assert_eq!(keys(&lanes), names[12..36]);
        for _ in 0..10 { ask(&mut lanes, &refs, true); }
        assert_eq!(keys(&lanes).last(), names.last());
        let refs: Vec<&Lane> = lanes.iter().collect();
        assert!(!merge_deck(&refs).2, "nothing after the last page");
    }

    #[test]
    fn a_lane_with_thirty_newer_items_fills_the_first_window_before_the_other_lane_shows() {
        let a = Server::new(1, &viewed(30, 1, 5000));
        let b = Server::new(2, &viewed(30, 1, 100));
        let lanes = deck(&[&a, &b]);
        let names: Vec<String> = a.items.iter().take(24).map(|item| item.0.clone()).collect();
        assert_eq!(keys(&lanes), names);
    }

    #[test]
    fn a_lane_that_loses_its_edge_by_more_than_a_page_goes_to_the_ledger_and_loses_nothing() {
        let mut a = Server::new(1, &viewed(80, 10, 9000));
        let refs = [&a];
        let mut lanes = deck(&refs);
        ask(&mut lanes, &refs, true);
        ask(&mut lanes, &refs, true);
        let mut shown: Vec<String> = a.items.iter().take(48).map(|item| item.0.clone()).collect();
        // Thirteen cards that were already seen leave at once: the lane's edge is a whole page away.
        let gone: Vec<String> = a.items.iter().take(13).map(|item| item.0.clone()).collect();
        for key in &gone { a.remove(key); }
        let refs = [&a];
        for _ in 0..30 {
            let held = keys(&lanes);
            if ask(&mut lanes, &refs, true) == 0 { break; }
            for key in keys(&lanes) {
                if !held.contains(&key) {
                    assert!(!shown.contains(&key), "{key} shown twice");
                    shown.push(key);
                }
            }
            assert!(keys(&lanes).len() <= MAX_SHELF_ITEMS);
        }
        assert!(lanes[0].scan.is_some(), "the lane is on its ledger");
        let mut reached = shown.clone();
        reached.sort();
        let mut want: Vec<String> = a.items.iter().map(|item| item.0.clone())
            .chain(gone.iter().cloned()).collect();
        want.sort();
        assert_eq!(reached, want, "every item reached, none twice");
        // …and the way back is open, through the ledger.
        let mut back = keys(&lanes);
        for _ in 0..30 {
            if ask(&mut lanes, &refs, false) == 0 { break; }
            for key in keys(&lanes) { if !back.contains(&key) { back.push(key); } }
        }
        assert!(back.len() > 24, "moved back through the ledger");
        let unique: std::collections::HashSet<_> = back.iter().collect();
        assert_eq!(unique.len(), back.len());
    }

    #[test]
    fn a_refresh_of_a_moved_deck_keeps_its_cards_and_brings_their_progress_up_to_date() {
        let mut a = Server::new(1, &viewed(40, 10, 5000));
        let b = Server::new(2, &viewed(40, 10, 4995));
        let lanes = {
            let refs = [&a, &b];
            let mut lanes = deck(&refs);
            ask(&mut lanes, &refs, true);
            ask(&mut lanes, &refs, true);
            ask(&mut lanes, &refs, false);
            lanes
        };
        let before = keys(&lanes);
        // The server moved on: a new item at the head, and progress on a shown card.
        let shown = a.items.iter().position(|item| before.contains(&item.0)).unwrap();
        a.items[shown].2 = 4242;
        let progressed = a.items[shown].0.clone();
        a.items.insert(0, ("s1-new".into(), 9999, 0));
        let mut fresh = Vec::new();
        for (i, lane) in lanes.iter().enumerate() {
            let server = if i == 0 { &a } else { &b };
            fresh.push(reload(sid(server.slot), lane, &[], |s, n| server.list(s, n)).unwrap());
        }
        fit(&mut fresh);
        assert_eq!(keys(&fresh), before, "the window did not snap back to the head");
        let moved = fresh[0].shown().find(|row| row.m.rk == progressed).unwrap();
        assert_eq!(moved.m.resume_ms, 4242);
    }

    #[test]
    fn a_lane_that_joins_a_placed_deck_shows_only_what_falls_inside_the_window() {
        let a = Server::new(1, &viewed(60, 10, 5000));
        let b = Server::new(2, &viewed(60, 10, 4995));
        let mut lanes = vec![a.lane()];
        let none: Vec<&[CwItem]> = vec![&[]];
        place(&mut lanes, &none);
        ask(&mut lanes, &[&a], true);
        let window = keys(&lanes);
        lanes.push(b.lane());
        place(&mut lanes, &vec![&[][..]; 2]);
        let merged = keys(&lanes);
        assert!(merged.len() <= MAX_SHELF_ITEMS);
        let (low, high) = (a.items.iter().position(|i| i.0 == window[0]).unwrap(),
            a.items.iter().position(|i| i.0 == *window.last().unwrap()).unwrap());
        for key in &merged {
            if key.starts_with("s2-") {
                let at = b.items.iter().position(|i| i.0 == *key).unwrap();
                assert!(at >= low.saturating_sub(1) && at <= high, "{key} outside the window");
            }
        }
        assert!(merged.iter().any(|key| key.starts_with("s2-")));
    }

    #[test]
    fn a_card_is_never_emitted_past_a_lane_that_may_hold_a_newer_one_unread() {
        let a = Server::new(1, &viewed(40, 1, 5000));
        let b = Server::new(2, &viewed(40, 1, 100));
        let mut lanes = vec![a.lane(), b.lane()];
        for lane in &mut lanes { (lane.lo, lane.hi, lane.placed) = (0, 0, true); }
        // `a` holds 24 rows of 40; the 25th card is not knowable without reading it.
        assert_eq!(take(&mut lanes, true, 30, &[true, true]), 24);
    }

    #[test]
    fn a_lane_nobody_has_touched_is_the_default_and_costs_a_recording_nothing() {
        assert!(Lane::default().is_default());
        assert_eq!(serde_json::to_string(&Lane::default()).unwrap().contains("seen"), false);
    }
}
