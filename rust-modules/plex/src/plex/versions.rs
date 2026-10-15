//! Which version of an item to play, when the item has more than one.
//!
//! An item can carry several `Media[]` versions (a 4K and a 1080p file, a theatrical and a
//! director's cut), and everything downstream of the metadata fetch reads the FIRST of them
//! ([`super::Metadata::primary_media`]): the detail page's badges and track lists, the play path's
//! part key and the transcode plan. So a version is chosen by putting it first, in one place, the
//! backend's metadata read, and nothing downstream needs to know a choice was made.
//!
//! The choice is remembered per item for the rest of the app run, keyed by the item's id and
//! naming the version by its part key (the one string that says which file it is on either
//! backend). It is not persisted: the next launch plays the server's own first version again.
//!
//! Only a backend that can play any version by its part key offers the choice
//! ([`super::Client::versions_selectable`]). PMS cannot: its transcoder addresses a version by
//! `mediaIndex`, which this client always sends as 0, so on Plex the list is never offered and
//! nothing here is consulted.

use std::sync::Mutex;

/// How many items' choices are kept; the oldest is forgotten first.
const KEPT: usize = 64;

/// `(item id, chosen version's part key)`, oldest first.
static CHOSEN: Mutex<Vec<(String, String)>> = Mutex::new(Vec::new());

fn chosen() -> std::sync::MutexGuard<'static, Vec<(String, String)>> {
    CHOSEN.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Play `part_key`'s version of item `rating_key` from now on.
pub fn prefer(rating_key: &str, part_key: &str) {
    if rating_key.is_empty() || part_key.is_empty() {
        return;
    }
    let mut list = chosen();
    list.retain(|(rk, _)| rk != rating_key);
    if list.len() >= KEPT {
        list.remove(0);
    }
    list.push((rating_key.to_string(), part_key.to_string()));
}

/// The part key of the version chosen for `rating_key`, if one was.
pub fn preferred(rating_key: &str) -> Option<String> {
    chosen().iter().find(|(rk, _)| rk == rating_key).map(|(_, key)| key.clone())
}

/// Move the chosen version of `rating_key` to the front of `items`, where `is(item, part_key)`
/// says whether an item is the version that part key names. The rest keep their order; nothing
/// moves when no choice was made or the chosen version is no longer among them.
pub fn put_first<T>(rating_key: &str, items: &mut [T], is: impl Fn(&T, &str) -> bool) {
    let Some(want) = preferred(rating_key) else { return };
    if let Some(at) = items.iter().position(|it| is(it, &want)) {
        items[..=at].rotate_right(1);
    }
}

/// Forget every choice; the next play of each item is the server's own first version again.
pub fn forget_all() {
    chosen().clear();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_chosen_version_moves_first_and_the_rest_keep_their_order() {
        let _guard = plx_base::testlock::serial();
        forget_all();
        let mut v = vec!["a", "b", "c"];
        put_first("item", &mut v, |it, key| *it == key);
        assert_eq!(v, ["a", "b", "c"], "no choice, no change");
        prefer("item", "c");
        put_first("item", &mut v, |it, key| *it == key);
        assert_eq!(v, ["c", "a", "b"]);
        prefer("item", "gone");
        put_first("item", &mut v, |it, key| *it == key);
        assert_eq!(v, ["c", "a", "b"], "a version that is no longer there moves nothing");
        assert_eq!(preferred("other"), None);
    }

    #[test]
    fn only_the_latest_items_are_remembered() {
        let _guard = plx_base::testlock::serial();
        forget_all();
        for n in 0..=KEPT {
            prefer(&format!("item{n}"), "k");
        }
        assert_eq!(preferred("item0"), None);
        assert_eq!(preferred(&format!("item{KEPT}")).as_deref(), Some("k"));
        prefer("item1", "k2");
        assert_eq!(preferred("item1").as_deref(), Some("k2"));
        assert_eq!(chosen().len(), KEPT);
    }
}
