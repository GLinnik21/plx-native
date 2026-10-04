//! The Continue Watching press persistence seam: durable-first, live only once the write lands.
use super::*;
use plx_plex::plex::session::DeckPress;

struct Restore(DeckPress);
impl Drop for Restore {
    fn drop(&mut self) {
        restore_deck_press(self.0);
    }
}

/// **A durable pick is live and read back by the next load**; the default is Details.
#[test]
fn a_durable_pick_is_live_and_persisted() {
    let _g = plx_base::testlock::serial();
    let _session = plx_plex::plex::session::TempSession::new("deck-press-set");
    let _restore = Restore(deck_press());
    restore_deck_press(DeckPress::Details);
    assert_eq!(deck_press(), DeckPress::Details, "the shipped default opens the page");

    assert!(set_deck_press(DeckPress::Play));
    assert_eq!(deck_press(), DeckPress::Play);
    assert_eq!(plx_plex::plex::session::load().deck_press(), DeckPress::Play);

    // choosing the default again removes the key from the file
    assert!(set_deck_press(DeckPress::Details));
    assert_eq!(plx_plex::plex::session::load().deck_press(), DeckPress::Details);
}

/// **A failed write claims nothing**: the live value stays.
#[test]
fn a_failed_write_changes_nothing() {
    let _g = plx_base::testlock::serial();
    let _session = plx_plex::plex::session::TempSession::new("deck-press-failed");
    let _restore = Restore(deck_press());
    restore_deck_press(DeckPress::Details);
    // the session file's parent is a regular file, so no write can land
    let dir = std::env::temp_dir().join(format!("plxnative-deck-press-blocked-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("blocker"), b"x").unwrap();
    plx_plex::plex::session::redirect_for_test(Some(dir.join("blocker").join("auth.json")));

    let saved = set_deck_press(DeckPress::Play);
    plx_plex::plex::session::redirect_for_test(None);
    let _ = std::fs::remove_dir_all(&dir);
    assert!(!saved);
    assert_eq!(deck_press(), DeckPress::Details);
}
