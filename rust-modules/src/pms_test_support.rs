//! Shared fixtures and helpers for the `pms` test modules split out below.
//!
//! `Owner` bundles one `PmsState`/`Arc<PmsAdapter>` pair — the exact shape `stores::hubs::HubsStore`
//! owns in production — so each test builds its own catalog with nothing shared across tests, the
//! way `search_test_support::Owner` does for Search. Every helper below is a plain function taking
//! explicit `state`/`adapter` (or `&Owner`'s fields) rather than an inherent `&mut self` method:
//! several tests hold a closure that borrows `owner.adapter` across an intervening mutation of
//! `owner.state` (`fetch_retry_tests`'s request-minting closures across a `reset`), and a whole-
//! struct `&mut self` method would force that borrow to cover all of `Owner` instead of just the
//! one field it touches. Free functions over precise field paths keep those borrows disjoint.

use super::*;

/// One state/adapter pair — this test's own Hubs owner, sharing nothing with any other test.
pub(super) struct Owner {
    pub(super) state: PmsState,
    pub(super) adapter: Arc<PmsAdapter>,
}

impl Default for Owner {
    fn default() -> Self {
        Self { state: PmsState::default(), adapter: Arc::new(PmsAdapter::default()) }
    }
}

/// Land whatever the workers left, then tick — the combined pass the legacy Home drove each
/// frame. Production splits it (the adapter drains with [`take_landings`] and applies through
/// `apply_landing`; the store's [`tick`] only counts down), so it survives here as the one
/// driver these landing/back-off contracts are phrased in.
pub(super) fn pump(state: &mut PmsState, adapter: &Arc<PmsAdapter>, dt: f32) {
    let a = Arc::clone(adapter);
    let _outcome = pump_with_landings(state, adapter, dt, move || take_landings(&a));
}

pub(super) fn pool(state: &PmsState) -> &Vec<HeroSlot> {
    &published_home(state).heroes
}

/// number of items in the rotating hero pool
pub(super) fn hero_pool_len(state: &PmsState) -> usize {
    pool(state).len()
}

/// hero-pool item `i`, or None
pub(super) fn hero_pool_item(state: &PmsState, i: usize) -> Option<&PmsMovie> {
    movie(state, pool(state).get(i)?.idx)
}

/// Handle of the server hero-pool page `i` came from ("friend"), or **empty** for the
/// signed-in user's own — see [`HeroSlot`]. The hero's meta line draws no run at all for the
/// empty case (`screens::home::meta_source_flow`), so a single-server library pays nothing for
/// this.
pub(super) fn hero_pool_source(state: &PmsState, i: usize) -> &str {
    pool(state).get(i).map(|s| s.source.as_str()).unwrap_or("")
}

/// title of hub `i` (e.g. "Continue Watching")
pub(super) fn hub_title(state: &PmsState, i: usize) -> &str {
    hubs(state).get(i).map(|h| h.title.as_str()).unwrap_or("")
}

/// Handle of the server hub `i` came from ("friend"), or **empty** for the signed-in user's
/// own server — see [`HubRow::source`].
pub(super) fn hub_source(state: &PmsState, i: usize) -> &str {
    hubs(state).get(i).map(|h| h.source.as_str()).unwrap_or("")
}

/// whether hub `i` is the merged Continue Watching shelf (its tiles play directly on OK, so
/// the home grid stamps the play-hint badge on them). Matched on the locale-independent
/// hubIdentifier, through the one identity function the publication itself uses.
pub(super) fn hub_is_continue(state: &PmsState, i: usize) -> bool {
    hubs(state)
        .get(i)
        .and_then(|h| stable_hub_identity(h, catalog(state)))
        .is_some_and(|id| matches!(id, HubIdentity::ContinueWatching))
}

pub(super) fn sid(slot: u16) -> ServerId {
    ServerId::from_raw(slot)
}

/// One source of the table, seeded directly. `handle` empty = a server of our own.
pub(super) fn src(slot: u16, handle: &str, state: HubState, last: Option<SourceBuild>) -> Src {
    let mut s = Src::new(sid(slot), handle.into());
    s.state = state;
    s.last = last;
    s
}

/// Install a source table and the merge of it — the state a run of landings would have reached.
/// Bypasses the roster, because a host test has no server registry to derive one from.
pub(super) fn seed(state: &mut PmsState, srcs: Vec<Src>) {
    let build = merge(&srcs);
    state.srcs = srcs;
    // leave `sync_roster` idle — both halves, see `seed_for_test`
    remember_roster(state, &BrowseScope::standalone());
    state.seen_facts = facts_key();
    commit(state, build);
}

/// A landing from the worker this source has out right now (current generation and seq) — what
/// a real fetch would post. `None` is a failure.
pub(super) fn land(state: &PmsState, adapter: &PmsAdapter, slot: u16, build: Option<SourceBuild>) {
    let s = sid(slot);
    let seq = state.srcs.iter().find(|x| x.sid == s).map(|x| x.seq).unwrap_or(0);
    adapter
        .results
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .push(Landing {
            gen: state.hub_gen,
            seq,
            sid: s,
            client: None,
            token_gen: 0,
            build,
        });
}

/// A drawable catalog row of one server. `thumb` is what `project` requires of a row, `art`
/// what the hero pool requires of one.
pub(super) fn row(slot: u16, rk: &str) -> PmsMovie {
    PmsMovie {
        sid: sid(slot),
        rk: rk.into(),
        title: rk.into(),
        thumb: "/t.jpg".into(),
        art: "/a.jpg".into(),
        ..Default::default()
    }
}

pub(super) fn shelf(slot: u16, title: &str, hub_id: &str, rks: &[&str]) -> Shelf {
    Shelf {
        title: title.into(),
        hub_id: hub_id.into(),
        key: String::new(),
        items: rks.iter().map(|r| row(slot, r)).collect(),
    }
}

/// A source's projection: `(lastViewedAt, rk)` deck entries plus whole shelves.
pub(super) fn built(slot: u16, cw: &[(i64, &str)], shelves: Vec<Shelf>) -> SourceBuild {
    SourceBuild {
        cw: cw
            .iter()
            .map(|&(t, r)| CwItem {
                last_viewed_at: t,
                m: row(slot, r),
            })
            .collect(),
        shelves,
    }
}

/// The ratingKeys of shelf `h`, in drawn order.
pub(super) fn rks(state: &PmsState, h: usize) -> Vec<String> {
    (0..hub_len(state, h))
        .filter_map(|c| hub_item(state, h, c))
        .map(|m| m.rk.clone())
        .collect()
}

/// A row that is part-way through, i.e. the shape a Continue Watching card really has.
pub(super) fn started(slot: u16, rk: &str) -> PmsMovie {
    PmsMovie {
        dur_ns: 90 * 60 * 1_000_000_000,
        resume_ms: 30 * 60_000,
        unwatched: false,
        ..row(slot, rk)
    }
}

pub(super) fn pool_of(sources: &[&str]) -> Vec<HeroSlot> {
    sources
        .iter()
        .enumerate()
        .map(|(i, s)| HeroSlot {
            idx: i,
            source: s.to_string(),
        })
        .collect()
}

pub(super) fn sources_of(pool: &[HeroSlot]) -> Vec<&str> {
    pool.iter().map(|s| s.source.as_str()).collect()
}

pub(super) fn two_library_directory(
    sid: ServerId,
    alpha_pinned: bool,
) -> crate::stores::browse::DirectorySnapshot {
    let section = |key, section, title: &str, pinned| {
        crate::stores::browse::SectionView {
            borrowed: false,
            sid: Some(sid),
            key,
            kind: crate::stores::browse::SecKind::Movie,
            row: crate::stores::browse::SrcRow {
                section,
                title: title.into(),
                pinned,
                current: section == 0,
                ..Default::default()
            },
        }
    };
    crate::stores::browse::DirectorySnapshot::fixture(23, 0, vec![
        section(1, 0, "Alpha", alpha_pinned),
        section(2, 1, "Beta", !alpha_pinned),
    ])
}
