//! Queue transport policy for the optional legacy HUD. No I/O and no new playback path.
use super::{cur_rk, cur_sid, pq_item_id, with_queue, PlaybackSession};
use plx_plex::plex::{queue_index_of, QueueRow, ServerId};

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
    fn legacy_controls_previous_has_a_five_second_boundary() {
        assert!(!previous_restarts(0));
        assert!(!previous_restarts(4_999_999_999));
        assert!(previous_restarts(5_000_000_000));
        assert!(previous_restarts(90_000_000_000));
    }

    #[test]
    fn legacy_controls_neighbors_use_queue_identity_and_never_wrap() {
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
