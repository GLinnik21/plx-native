//! Read the moving window of an existing PlayQueue; never replace or take ownership of it.
use super::client::{Client, QueryBuilder};
use super::{models::MediaContainer, QueueRow, ServerId};

fn window_query(id: &str, center: i64) -> Option<String> {
    let id = id.parse::<i64>().ok().filter(|id| *id > 0)?;
    if center <= 0 {
        return None;
    }
    // docs/plex-openapi.json: GET /playQueues/{playQueueId}. `center` is an item ID,
    // independent of the server's selected item. `window` is a page size, not a total cap.
    Some(
        QueryBuilder::new(&format!("/playQueues/{id}"))
            .int("own", 0)
            .int("center", center)
            .int("window", 50)
            .int("includeBefore", 1)
            .int("includeAfter", 1)
            .build(),
    )
}

fn window_rows(mc: MediaContainer, id: i64, sid: ServerId) -> Option<Vec<QueueRow>> {
    if mc.play_queue_id != id {
        return None;
    }
    Some(
        mc.metadata
            .into_iter()
            .map(|row| QueueRow::of(row, sid))
            .collect(),
    )
}

impl Client {
    /// GET the existing queue around an item, without transferring ownership or changing the
    /// selected item. The caller validates that the requested identity exists in the response.
    pub fn play_queue_window(&self, id: &str, center: i64) -> Option<Vec<QueueRow>> {
        let query = window_query(id, center)?;
        window_rows(self.get_json(&query)?, id.parse().ok()?, self.id())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn queue_window_queue_window_uses_item_identity_and_does_not_take_ownership() {
        let query = window_query("7", 12).unwrap();
        assert_eq!(
            query,
            "/playQueues/7?own=0&center=12&window=50&includeBefore=1&includeAfter=1"
        );
        assert!(window_query("7/other", 12).is_none());
        assert!(window_query("7", 0).is_none());
        let mc: super::super::models::Envelope = serde_json::from_str(
            r#"{"MediaContainer":{
            "playQueueID":7,"playQueueSelectedItemID":11,"Metadata":[
            {"playQueueItemID":11,"ratingKey":"same","type":"episode"},
            {"playQueueItemID":12,"ratingKey":"same","type":"episode"},
            {"playQueueItemID":13,"ratingKey":"third","type":"episode"}]}}"#,
        )
        .unwrap();
        let sid = ServerId::from_raw(2);
        let rows = window_rows(mc.media_container, 7, sid).unwrap();
        assert_eq!(
            rows.iter().map(|r| r.item_id).collect::<Vec<_>>(),
            vec![11, 12, 13]
        );
        assert!(rows.iter().all(|r| r.sid == sid));
        assert!(window_rows(MediaContainer::default(), 7, sid).is_none());
    }
}
