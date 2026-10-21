//! One tile per show in Continue Watching, as the official client draws it.
//!
//! Plex Web merges the row in the browser from each source's `continuewatching` hub: episodes group
//! by `grandparentGuid`, a movie never groups, and the FIRST copy in source order wins (a copy
//! without Media yields to one that has it). `lastViewedAt` only sorts the merged row; it never
//! picks the copy. This is that rule, run over [`deck::merge_deck`]'s output (it does not change
//! the lanes, the ledger, `place`, `reload` or `fit`), and `docs/shared-servers.md` ("One tile per
//! show in Continue Watching") records why.
//!
//! **The pick is pure.** The winner is a function of roster order, the Media flag and eligibility.
//! Nothing is remembered about a decision, so the merge stays a function of its inputs and
//! `publish_staged`'s trial merge leaves nothing behind.
//!
//! **What is remembered is facts**: [`Held`], per server, the copy of each show that server has
//! listed. A lane trims the rows it let go of, but the roster-first copy can be one its lane is not
//! showing (deep in the listing, or behind the window); a pass that saw only the window would show
//! the other server's copy now and the winner pages later, which is a duplicate. So the facts
//! outlive the trim, and are dropped by ORDER (a ledger lane's positions are an order, not listing
//! offsets): a held copy that falls inside the span of the rows the lane now holds, and is not among
//! them, is gone.

use std::cmp::Ordering;

use plx_plex::plex::ServerId;

use super::deck::{order, Lane};
use crate::pms::{item_pinned, CwItem, PmsMovie};

/// The show an episode card groups under; `None` for anything else, which never groups (a movie, a
/// season, an episode whose server named no decodable show guid).
pub(crate) fn key_of(m: &PmsMovie) -> Option<[u8; 12]> {
    if m.kind == 3 { m.show_guid } else { None }
}

/// One server's copy of a show: the deck row, and the group it belongs to.
#[derive(Clone)]
pub(crate) struct Entry {
    pub(crate) group: [u8; 12],
    pub(crate) item: CwItem,
}

impl Entry {
    /// Whether the row carries a playable part (`Media[0].Part[0].key`).
    fn has_media(&self) -> bool { !self.item.m.part.is_empty() }

    fn is(&self, card: &CwItem) -> bool {
        self.item.m.sid == card.m.sid && self.item.m.rk == card.m.rk
    }
}

/// The copies one server has listed, at most one per show (a server's deck lists a show once).
#[derive(Clone, Default)]
pub(crate) struct Held {
    copies: Vec<Entry>,
}

impl Held {
    /// Forgets everything: a fresh answer from the server, a reset, a profile switch.
    pub(crate) fn clear(&mut self) { self.copies.clear(); }

    /// Lets go of the copy `gone` picks out: the card was removed from the deck. The span test in
    /// [`Held::feed`] cannot see this one, because removing the lane's newest or oldest row leaves
    /// its copy outside the span of the rows that remain.
    pub(crate) fn forget(&mut self, gone: impl Fn(&PmsMovie) -> bool) { self.copies.retain(|c| !gone(&c.item.m)); }

    #[cfg(test)]
    pub(crate) fn rks(&self) -> Vec<String> { self.copies.iter().map(|c| c.item.m.rk.clone()).collect() }

    /// Takes the rows a lane holds now. A held copy a row replaces is replaced FIRST (a copy
    /// restamped to a new `lastViewedAt` moves, it is not dropped); then a copy absent from the rows
    /// that falls inside their span is let go, because the lane would have listed it.
    pub(crate) fn feed(&mut self, lane: &Lane, preview: &[CwItem]) {
        let rows: &[CwItem] = if lane.placed { &lane.rows } else { preview };
        let mut seen: Vec<Entry> = Vec::new();
        for row in rows {
            if let Some(group) = key_of(&row.m) {
                if !seen.iter().any(|c| c.group == group) {
                    seen.push(Entry { group, item: row.clone() });
                }
            }
        }
        let span = rows.iter().min_by(|a, b| order(a, b)).zip(rows.iter().max_by(|a, b| order(a, b)));
        match span {
            // nothing held and the listing is over, with no window ever moved off its head: the
            // server lists no show at all. A lane the deck has paged past holds nothing too (it
            // trimmed every row behind the window), and its copies are still the server's.
            None if !lane.placed || (lane.done && lane.lo == 0 && lane.seen.is_empty()) => self.copies.clear(),
            None => {}
            Some((first, last)) => self.copies.retain(|held| {
                seen.iter().all(|c| c.group != held.group)
                    && (order(&held.item, first) == Ordering::Less || order(&held.item, last) == Ordering::Greater)
            }),
        }
        self.copies.extend(seen);
    }

    fn of(&self, group: &[u8; 12]) -> Option<&Entry> { self.copies.iter().find(|c| c.group == *group) }
}

/// A server as the pass sees it: its held copies, in roster order, and whether it may win.
pub(crate) struct Source<'a> {
    pub(crate) held: &'a Held,
    /// The server answered its deck in this process and the latest deck read did not fail (a
    /// refresh in flight does not count against it). A source restored
    /// from earlier, one that is asleep or one whose client is gone must not win with a copy that
    /// cannot play (Plex Web merges only the servers that answered this load).
    pub(crate) eligible: bool,
}

/// The first ELIGIBLE copy of `group` in roster order, among copies that would be shown (their
/// library is pinned); copies with Media come before copies without.
pub(crate) fn winner<'a>(group: &[u8; 12], roster: &[Source<'a>], pins: &[(ServerId, i64, bool)]) -> Option<&'a Entry> {
    let mut candidates = roster.iter().filter(|s| s.eligible).filter_map(|s| s.held.of(group))
        .filter(|c| item_pinned(pins, &c.item.m));
    let first = candidates.next()?;
    if first.has_media() { return Some(first); }
    candidates.find(|c| c.has_media()).or(Some(first))
}

/// The merged deck with each show once. A card is kept if and only if it is its group's winner;
/// everything else passes through unchanged, in order. A group whose copies no eligible server
/// holds hides nothing.
pub(crate) fn present<'a, T>(cards: Vec<(T, &'a CwItem)>, roster: &[Source<'_>], pins: &[(ServerId, i64, bool)])
    -> Vec<(T, &'a CwItem)> {
    if roster.len() < 2 { return cards; }
    cards.into_iter().filter(|(_, card)| {
        let Some(group) = key_of(&card.m) else { return true };
        match winner(&group, roster, pins) {
            Some(win) => win.is(card),
            None => true,
        }
    }).collect()
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;

    const SHOW: [u8; 12] = [7; 12];

    fn card(slot: u16, rk: &str, kind: i32, guid: Option<[u8; 12]>, media: bool, viewed: i64) -> CwItem {
        CwItem {
            last_viewed_at: viewed,
            position: 0,
            m: Arc::new(PmsMovie {
                sid: ServerId::from_raw(slot), rk: rk.into(), kind, show_guid: guid,
                part: if media { "/library/parts/1/file.mkv".into() } else { String::new() },
                ..PmsMovie::default()
            }),
        }
    }

    fn held(rows: Vec<CwItem>) -> Held {
        let mut h = Held::default();
        let mut lane = Lane::default();
        lane.rows = rows;
        lane.placed = true;
        h.feed(&lane, &[]);
        h
    }

    fn episode(slot: u16, rk: &str, media: bool) -> CwItem { card(slot, rk, 3, Some(SHOW), media, 100) }

    fn pick(heads: &[(&Held, bool)]) -> Option<String> {
        let roster: Vec<Source> = heads.iter().map(|&(held, eligible)| Source { held, eligible }).collect();
        winner(&SHOW, &roster, &[]).map(|c| format!("{}:{}", c.item.m.sid.raw(), c.item.m.rk))
    }

    #[test]
    fn roster_first_copy_wins() {
        let (a, b) = (held(vec![episode(0, "own", true)]), held(vec![episode(1, "theirs", true)]));
        assert_eq!(pick(&[(&a, true), (&b, true)]).as_deref(), Some("0:own"));
        assert_eq!(pick(&[(&b, true), (&a, true)]).as_deref(), Some("1:theirs"), "roster order, not the server");
    }

    #[test]
    fn a_copy_without_media_yields_to_one_with_media() {
        let (a, b) = (held(vec![episode(0, "own", false)]), held(vec![episode(1, "theirs", true)]));
        assert_eq!(pick(&[(&a, true), (&b, true)]).as_deref(), Some("1:theirs"));
        let c = held(vec![episode(1, "theirs", false)]);
        assert_eq!(pick(&[(&a, true), (&c, true)]).as_deref(), Some("0:own"), "none has Media: the first");
    }

    #[test]
    fn an_offline_servers_copy_never_wins() {
        let (a, b) = (held(vec![episode(0, "own", true)]), held(vec![episode(1, "theirs", true)]));
        assert_eq!(pick(&[(&a, false), (&b, true)]).as_deref(), Some("1:theirs"));
        assert_eq!(pick(&[(&a, false), (&b, false)]), None);
    }

    #[test]
    fn movies_never_group() {
        let (a, b) = (held(vec![card(0, "m", 0, Some(SHOW), true, 5)]), held(vec![card(1, "m", 0, Some(SHOW), true, 5)]));
        assert!(a.copies.is_empty() && b.copies.is_empty(), "a movie has no group");
        assert_eq!(key_of(&card(0, "m", 0, Some(SHOW), true, 5).m), None);
    }

    #[test]
    fn an_episode_without_a_show_guid_never_groups() {
        assert_eq!(key_of(&episode(0, "e", true).m), Some(SHOW));
        assert_eq!(key_of(&card(0, "e", 3, None, true, 5).m), None);
    }

    #[test]
    fn seasons_never_group() {
        assert_eq!(key_of(&card(0, "s", 2, Some(SHOW), true, 5).m), None);
    }

    /// A ledger lane's positions are an order, not listing offsets: a copy the lane restamped to a
    /// newer `lastViewedAt` is replaced where it moved, not dropped by the span test.
    #[test]
    fn a_restamped_copy_on_a_ledger_lane_is_replaced_not_dropped() {
        let ledger = |rows: Vec<CwItem>| {
            let mut lane = Lane::default();
            lane.rows = rows;
            (lane.placed, lane.scan, lane.seen) = (true, Some(0), vec!["x".into()]);
            lane
        };
        let at = |viewed: i64, position: usize, rk: &str| {
            let mut c = card(0, rk, 3, Some(SHOW), true, viewed);
            c.position = position;
            c
        };
        let other = |viewed: i64, position: usize| {
            let mut c = card(0, &format!("m{position}"), 0, None, true, viewed);
            c.position = position;
            c
        };
        let mut h = Held::default();
        h.feed(&ledger(vec![other(300, 1), at(200, 2, "e1"), other(100, 3)]), &[]);
        assert_eq!(h.rks(), ["e1"]);
        // the server restamped the show: it now sorts first, under the same key
        h.feed(&ledger(vec![at(900, 0, "e1"), other(300, 1), other(100, 3)]), &[]);
        assert_eq!(h.rks(), ["e1"], "replaced, not dropped");
        assert_eq!(h.copies[0].item.last_viewed_at, 900);
    }

    /// A held copy inside the span of the rows the lane holds, and absent from them, is gone; one
    /// outside the span (the lane trimmed it) is remembered.
    #[test]
    fn a_copy_the_lane_no_longer_lists_is_dropped_by_order_not_by_absence() {
        let mut lane = Lane::default();
        lane.placed = true;
        let row = |viewed: i64, position: usize, rk: &str, show: bool| {
            let mut c = if show { card(0, rk, 3, Some(SHOW), true, viewed) } else { card(0, rk, 0, None, true, viewed) };
            c.position = position;
            c
        };
        lane.rows = vec![row(900, 0, "e-old", true), row(800, 1, "m1", false), row(700, 2, "m2", false)];
        let mut h = Held::default();
        h.feed(&lane, &[]);
        // the window moved on: the lane trimmed the row behind it
        lane.rows = vec![row(800, 1, "m1", false), row(700, 2, "m2", false)];
        h.feed(&lane, &[]);
        assert_eq!(h.rks(), ["e-old"], "outside the span: remembered");
        // the server removed it from a place the lane now covers
        lane.rows = vec![row(950, 0, "m0", false), row(800, 1, "m1", false)];
        h.feed(&lane, &[]);
        assert!(h.rks().is_empty(), "inside the span, not listed: gone");
    }
}
