//! Queue transport policy and existing-queue window refresh for the episode controls.
use super::{cur_rk, cur_sid, pq_id, pq_item_id, with_queue, PlaybackSession};
use plx_plex::plex::{queue_index_of, QueueRow, ServerId};
use std::sync::Arc;

/// An owned continuation of the server's existing queue, captured before request_play retires
/// the outgoing state. Shared across resolve snapshots without copying a show's rows per frame.
#[derive(Clone)]
pub struct QueueContinuation {
    id: String,
    rows: Arc<Vec<QueueRow>>,
    selected: usize,
}

impl QueueContinuation {
    /// Where `target` sits in queue `id`'s `rows`, if it is exactly that queue item.
    fn select(id: &str, rows: &[QueueRow], target: &QueueRow) -> Option<usize> {
        if id.is_empty() || target.item_id == 0 {
            return None;
        }
        let selected = queue_index_of(rows, target.item_id, target.sid, &target.rk)?;
        let row = &rows[selected];
        if row.item_id != target.item_id || row.sid != target.sid || row.rk != target.rk {
            return None;
        }
        Some(selected)
    }

    fn from_queue(id: String, rows: Vec<QueueRow>, target: &QueueRow) -> Option<Self> {
        let selected = Self::select(&id, &rows, target)?;
        Some(Self { id, rows: Arc::new(rows), selected })
    }

    /// The live queue is lent, not owned, so its rows are copied only once `target` is found.
    pub(super) fn capture(ps: &PlaybackSession, target: &QueueRow) -> Option<Self> {
        with_queue(ps, |rows| {
            let id = pq_id(ps);
            let selected = Self::select(&id, rows, target)?;
            Some(Self { id, rows: Arc::new(rows.to_vec()), selected })
        })
    }

    pub(super) fn matches(&self, sid: ServerId, rk: &str) -> bool {
        self.rows[self.selected].sid == sid && self.rows[self.selected].rk == rk
    }

    fn with_window(&self, rows: Vec<QueueRow>) -> Self {
        Self::from_queue(self.id.clone(), rows, &self.rows[self.selected])
            .unwrap_or_else(|| self.clone())
    }

    pub(super) fn refreshed(&self, client: &plx_plex::plex::Client) -> Self {
        client
            .play_queue_window(&self.id, self.rows[self.selected].item_id)
            .map(|rows| self.with_window(rows))
            .unwrap_or_else(|| self.clone())
    }

    pub(super) fn resolve(&self) -> super::decision::QueueInfo {
        super::decision::QueueInfo {
            id: self.id.clone(),
            item_id: self.rows[self.selected].item_id.to_string(),
            up_next: self
                .rows
                .get(self.selected + 1)
                .and_then(super::plan::up_next_of),
            rows: self.rows.as_ref().clone(),
            machine_id: String::new(),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum QueueStep {
    Previous,
    Next,
}

/// At five seconds the Previous button becomes Restart. Before it, it asks for the prior item.
pub fn previous_restarts(position_ns: i64) -> bool {
    position_ns >= 5_000_000_000
}

/// The row now playing among `rows`, by the queue's identity rule (item id, rating key as the
/// fallback).
fn current_index(ps: &PlaybackSession, rows: &[QueueRow]) -> Option<usize> {
    queue_index_of(rows, pq_item_id(ps).parse().unwrap_or(0), cur_sid(ps), &cur_rk(ps))
}

fn neighbor(rows: &[QueueRow], current: usize, step: QueueStep) -> Option<&QueueRow> {
    let index = match step {
        QueueStep::Previous => current.checked_sub(1)?,
        QueueStep::Next => current.checked_add(1)?,
    };
    rows.get(index)
}

/// Clone before teardown/request_play can retire the outgoing session's queue.
pub fn queue_neighbor(ps: &PlaybackSession, step: QueueStep) -> Option<QueueRow> {
    with_queue(ps, |rows| neighbor(rows, current_index(ps, rows)?, step).cloned())
}

/// Per-frame availability without copying either item's strings into the HUD.
pub fn queue_neighbors(ps: &PlaybackSession) -> (bool, bool) {
    with_queue(ps, |rows| {
        let Some(index) = current_index(ps, rows) else {
            return (false, false);
        };
        (index > 0, index + 1 < rows.len())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// [`neighbor`] of the row `item_id`/`rk` names, located as [`current_index`] locates it.
    fn neighbor_of<'a>(rows: &'a [QueueRow], item_id: i64, sid: ServerId, rk: &str, step: QueueStep)
        -> Option<&'a QueueRow> {
        neighbor(rows, queue_index_of(rows, item_id, sid, rk)?, step)
    }

    #[test]
    fn queue_navigation_previous_has_a_five_second_boundary() {
        assert!(!previous_restarts(0));
        assert!(!previous_restarts(4_999_999_999));
        assert!(previous_restarts(5_000_000_000));
        assert!(previous_restarts(90_000_000_000));
    }

    #[test]
    fn queue_navigation_queue_handoff_retains_the_predecessor_and_original_queue_ids() {
        let sid = ServerId::from_raw(1);
        let rows: Vec<_> = [11, 12]
            .into_iter()
            .map(|item_id| QueueRow {
                item_id,
                sid,
                rk: item_id.to_string(),
                kind: "episode".into(),
                ..Default::default()
            })
            .collect();
        // A fresh PlayQueue POST for item 12 can return only [12]. The continuation must carry
        // [11, 12] instead, or Previous on the newly started item has nowhere to go.
        let handoff = QueueContinuation::from_queue("queue-7".into(), rows.clone(), &rows[1]).unwrap();
        let landed = handoff.resolve();
        assert_eq!(landed.id, "queue-7");
        assert_eq!(landed.item_id, "12");
        assert_eq!(landed.rows.len(), 2);
        assert_eq!(
            neighbor_of(&landed.rows, 12, sid, "12", QueueStep::Previous)
                .unwrap()
                .rk,
            "11"
        );
        assert!(handoff.matches(sid, "12"));
        assert!(!handoff.matches(ServerId::from_raw(2), "12"));
        assert!(!handoff.matches(sid, "11"));
        let back = QueueContinuation::from_queue(landed.id, landed.rows.clone(), &landed.rows[0])
            .unwrap()
            .resolve();
        assert_eq!(back.item_id, "11");
        assert_eq!(back.up_next.unwrap().rk, "12");
    }

    #[test]
    fn queue_navigation_queue_window_moves_forward_without_losing_the_selected_item() {
        let sid = ServerId::from_raw(1);
        let rows: Vec<_> = [11, 12, 13]
            .into_iter()
            .map(|item_id| QueueRow {
                item_id,
                sid,
                rk: item_id.to_string(),
                kind: "episode".into(),
                ..Default::default()
            })
            .collect();
        let handoff =
            QueueContinuation::from_queue("queue-7".into(), rows[..2].to_vec(), &rows[1]).unwrap();
        let landed = handoff.with_window(rows.clone()).resolve();
        assert_eq!(landed.rows.len(), 3);
        assert_eq!(landed.item_id, "12");
        assert_eq!(landed.up_next.unwrap().rk, "13");
        // Empty/expired/wrong-item windows cannot discard the known predecessor or selection.
        assert_eq!(handoff.with_window(Vec::new()).resolve().rows.len(), 2);
        assert_eq!(handoff.with_window(rows[2..].to_vec()).resolve().item_id, "12");
    }

    #[test]
    fn queue_navigation_neighbors_use_queue_identity_and_never_wrap() {
        let sid = ServerId::from_raw(1);
        let rows: Vec<_> = [11, 12, 13]
            .into_iter()
            .map(|item_id| QueueRow {
                item_id,
                sid,
                rk: "repeated-rating-key".into(),
                ..Default::default()
            })
            .collect();
        assert_eq!(
            neighbor_of(&rows, 12, sid, "repeated-rating-key", QueueStep::Previous)
                .unwrap()
                .item_id,
            11
        );
        assert_eq!(
            neighbor_of(&rows, 12, sid, "repeated-rating-key", QueueStep::Next)
                .unwrap()
                .item_id,
            13
        );
        assert!(neighbor_of(&rows, 11, sid, "repeated-rating-key", QueueStep::Previous).is_none());
        assert!(neighbor_of(&rows, 13, sid, "repeated-rating-key", QueueStep::Next).is_none());
        assert!(neighbor_of(&rows, 0, sid, "absent", QueueStep::Next).is_none());
    }
}
