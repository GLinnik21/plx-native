//! Shared fixtures and helpers for the `metadata` test modules split out below.

use super::*;
use std::sync::atomic::Ordering;

// ---- convert_streams: the Dolby Vision record's survival ------------------------------

pub(super) fn video_stream(dovi: Option<(i64, i64, i64)>) -> crate::plex::Stream {
    let (present, profile, compat, el) = match dovi {
        Some((profile, compat, el)) => (1, profile, compat, el),
        None => (0, 0, 0, 0),
    };
    crate::plex::Stream {
        stream_type: 1,
        codec: "hevc".into(),
        dovi_present: present,
        dovi_profile: profile,
        dovi_bl_compat_id: compat,
        dovi_el_present: el,
        ..Default::default()
    }
}

/// post through the REAL mailbox write, so the monotone guard is under test rather than
/// bypassed (an unconditional store here would make the "older lands late" case vacuous)
pub(super) fn landing(gen: u32, rk: &str) {
    land_detail(
        crate::plex::ServerId::UNSET,
        rk,
        gen,
        Some(Detail {
            rk: rk.to_string(),
            ..Default::default()
        }),
    );
}

pub(super) fn cur_rk() -> Option<String> {
    current().map(|d| d.rk.clone())
}

// ---- the season mailbox -----------------------------------------------------------------

/// A two-season show with a populated episode row, as a landed detail fetch leaves it. Written
/// straight into CURRENT rather than through `pump_detail` — that pump is the other test's
/// subject, and routing through it would couple the two.
/// Two registry slots — plain values, so the identity rules are gradeable without a registry.
/// `SRV_A` stands in for the signed-in user's own server, `SRV_B` for a share.
pub(super) const SRV_A: crate::plex::ServerId = crate::plex::ServerId::from_raw(0);

pub(super) const SRV_B: crate::plex::ServerId = crate::plex::ServerId::from_raw(1);

pub(super) fn install_show(rk: &str, cur: usize, eps: &[&str]) {
    install_show_on(SRV_A, rk, cur, eps);
}

pub(super) fn install_show_on(sid: crate::plex::ServerId, rk: &str, cur: usize, eps: &[&str]) {
    unsafe {
        *addr_of_mut!(CURRENT) = Some(Detail {
            sid,
            rk: rk.to_string(),
            is_show: true,
            seasons: vec![
                Season {
                    rk: "sk1".to_string(),
                    index: 1,
                    title: "Season 1".to_string(),
                    leaf_count: 0,
                    viewed_leaf_count: 0,
                },
                Season {
                    rk: "sk2".to_string(),
                    index: 2,
                    title: "Season 2".to_string(),
                    leaf_count: 0,
                    viewed_leaf_count: 0,
                },
            ],
            episodes: eps.iter().map(|e| episode(e)).collect(),
            cur_season: cur,
            ..Default::default()
        })
    };
}

pub(super) fn episode(rk: &str) -> Episode {
    Episode {
        rk: rk.to_string(),
        ..Default::default()
    }
}

pub(super) fn listed_eps() -> Vec<String> {
    current()
        .map(|d| d.episodes.iter().map(|e| e.rk.clone()).collect())
        .unwrap_or_default()
}

/// which season tab reads *selected* — the tabs pill `d.cur_season`; the focus ring is a
/// separate, view-local column
pub(super) fn selected_tab() -> usize {
    current().map(|d| d.cur_season).unwrap_or(usize::MAX)
}

/// arm a season switch exactly as `load_season` does — flip the tab optimistically, then take
/// the generation. Hands back what the worker carries to `land_season`.
pub(super) fn begin_switch(to: usize) -> (u32, usize) {
    let prev = selected_tab();
    unsafe {
        if let Some(d) = (*addr_of_mut!(CURRENT)).as_mut() {
            d.cur_season = to;
        }
    }
    (SEASON_GEN.fetch_add(1, Ordering::SeqCst) + 1, prev)
}
