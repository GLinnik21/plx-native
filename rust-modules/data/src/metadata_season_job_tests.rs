//! A season switch and an episode-page window are typed jobs the owner emits, not threads the data
//! layer starts (`docs/sans-io.md`): the step leaves its request on the outbox and starts nothing.

use super::*;
use super::test_support::episode;
use crate::stores::metadata::{MetadataCmd, MetadataStore};

fn show_on(store: &mut MetadataStore, sid: plx_plex::plex::ServerId) {
    let season = |index: i64| Season {
        rk: format!("sk{index}"), index, title: format!("Season {index}"), leaf_count: 0, viewed_leaf_count: 0,
    };
    let mut episodes = EpisodeList::with_total(200);
    episodes.set_page(0, (0..EPISODE_PAGE).map(|i| episode(&format!("e{i}"))).collect());
    set_current_for_test(store.state_mut(), Some(Detail {
        sid, rk: "show".into(), is_show: true, seasons: vec![season(1), season(2)], cur_season: 0,
        episodes, ..Default::default()
    }));
}

/// A season switch leaves one request on the outbox: it starts no worker of its own.
#[test]
fn a_season_switch_leaves_its_request_on_the_outbox() {
    let _serial = plx_base::testlock::serial();
    let mut store = MetadataStore::default();
    show_on(&mut store, plx_plex::plex::ServerId::UNSET);
    assert!(!store.has_jobs());
    store.run(MetadataCmd::LoadSeason(1));
    assert!(store.has_jobs(), "the switch emitted its request");
    assert_eq!(store.take_jobs().len(), 1);
}

/// An episode-page window leaves one request on the outbox: it starts no worker of its own.
#[test]
fn an_episode_window_leaves_its_request_on_the_outbox() {
    let _serial = plx_base::testlock::serial();
    let mut store = MetadataStore::default();
    show_on(&mut store, plx_plex::plex::ServerId::UNSET);
    assert!(!store.has_jobs());
    store.run(MetadataCmd::WantEpisodes { lo: EPISODE_PAGE, hi: EPISODE_PAGE + 10, focus: None, restore: None });
    assert!(store.has_jobs(), "the window emitted its request");
    assert_eq!(store.take_jobs().len(), 1);
}
