//! Queue transport policy and existing-queue window refresh for the optional classic HUD.
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
    fn from_queue(id: String, rows: &[QueueRow], target: &QueueRow) -> Option<Self> {
        if id.is_empty() || target.item_id == 0 {
            return None;
        }
        let selected = queue_index_of(rows, target.item_id, target.sid, &target.rk)?;
        let row = &rows[selected];
        if row.item_id != target.item_id || row.sid != target.sid || row.rk != target.rk {
            return None;
        }
        Some(Self {
            id,
            rows: Arc::new(rows.to_vec()),
            selected,
        })
    }

    pub(super) fn capture(ps: &PlaybackSession, target: &QueueRow) -> Option<Self> {
        with_queue(ps, |rows| Self::from_queue(pq_id(ps), rows, target))
    }

    pub(super) fn matches(&self, sid: ServerId, rk: &str) -> bool {
        self.rows[self.selected].sid == sid && self.rows[self.selected].rk == rk
    }

    fn with_window(&self, rows: &[QueueRow]) -> Self {
        Self::from_queue(self.id.clone(), rows, &self.rows[self.selected])
            .unwrap_or_else(|| self.clone())
    }

    pub(super) fn refreshed(&self, client: &plx_plex::plex::Client) -> Self {
        client
            .play_queue_window(&self.id, self.rows[self.selected].item_id)
            .map(|rows| self.with_window(&rows))
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

fn neighbor<'a>(
    rows: &'a [QueueRow],
    item_id: i64,
    sid: ServerId,
    rk: &str,
    step: QueueStep,
) -> Option<&'a QueueRow> {
    let current = queue_index_of(rows, item_id, sid, rk)?;
    let index = match step {
        QueueStep::Previous => current.checked_sub(1)?,
        QueueStep::Next => current.checked_add(1)?,
    };
    rows.get(index)
}

/// Clone before teardown/request_play can retire the outgoing session's queue.
pub fn queue_neighbor(ps: &PlaybackSession, step: QueueStep) -> Option<QueueRow> {
    with_queue(ps, |rows| {
        neighbor(
            rows,
            pq_item_id(ps).parse().unwrap_or(0),
            cur_sid(ps),
            &cur_rk(ps),
            step,
        )
        .cloned()
    })
}

/// Per-frame availability without copying either item's strings into the HUD.
pub fn queue_neighbors(ps: &PlaybackSession) -> (bool, bool) {
    with_queue(ps, |rows| {
        let Some(index) = queue_index_of(
            rows,
            pq_item_id(ps).parse().unwrap_or(0),
            cur_sid(ps),
            &cur_rk(ps),
        ) else {
            return (false, false);
        };
        (index > 0, index + 1 < rows.len())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classic_controls_previous_has_a_five_second_boundary() {
        assert!(!previous_restarts(0));
        assert!(!previous_restarts(4_999_999_999));
        assert!(previous_restarts(5_000_000_000));
        assert!(previous_restarts(90_000_000_000));
    }

    #[test]
    fn classic_controls_queue_handoff_retains_the_predecessor_and_original_queue_ids() {
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
        let handoff = QueueContinuation::from_queue("queue-7".into(), &rows, &rows[1]).unwrap();
        let landed = handoff.resolve();
        assert_eq!(landed.id, "queue-7");
        assert_eq!(landed.item_id, "12");
        assert_eq!(landed.rows.len(), 2);
        assert_eq!(
            neighbor(&landed.rows, 12, sid, "12", QueueStep::Previous)
                .unwrap()
                .rk,
            "11"
        );
        assert!(handoff.matches(sid, "12"));
        assert!(!handoff.matches(ServerId::from_raw(2), "12"));
        assert!(!handoff.matches(sid, "11"));
        let back = QueueContinuation::from_queue(landed.id, &landed.rows, &landed.rows[0])
            .unwrap()
            .resolve();
        assert_eq!(back.item_id, "11");
        assert_eq!(back.up_next.unwrap().rk, "12");
    }

    #[test]
    fn classic_controls_queue_window_moves_forward_without_losing_the_selected_item() {
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
            QueueContinuation::from_queue("queue-7".into(), &rows[..2], &rows[1]).unwrap();
        let landed = handoff.with_window(&rows).resolve();
        assert_eq!(landed.rows.len(), 3);
        assert_eq!(landed.item_id, "12");
        assert_eq!(landed.up_next.unwrap().rk, "13");
        // Empty/expired/wrong-item windows cannot discard the known predecessor or selection.
        assert_eq!(handoff.with_window(&[]).resolve().rows.len(), 2);
        assert_eq!(handoff.with_window(&rows[2..]).resolve().item_id, "12");
    }

    #[test]
    fn classic_controls_neighbors_use_queue_identity_and_never_wrap() {
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
            neighbor(&rows, 12, sid, "repeated-rating-key", QueueStep::Previous)
                .unwrap()
                .item_id,
            11
        );
        assert_eq!(
            neighbor(&rows, 12, sid, "repeated-rating-key", QueueStep::Next)
                .unwrap()
                .item_id,
            13
        );
        assert!(neighbor(&rows, 11, sid, "repeated-rating-key", QueueStep::Previous).is_none());
        assert!(neighbor(&rows, 13, sid, "repeated-rating-key", QueueStep::Next).is_none());
        assert!(neighbor(&rows, 0, sid, "absent", QueueStep::Next).is_none());
    }
}
