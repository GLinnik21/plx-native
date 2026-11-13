//! Explicit queue navigation and Up Next share one stop → resolve → start ritual.
use super::*;
use plx_media::route::{queue_neighbor, QueueStep};
use plx_plex::plex::QueueRow;

/// Previous and Next both ask for the neighbouring queue row and play it through [`play`]; neither
/// is ever a seek. The outgoing episode is stopped by the replacement itself
/// (`stop_for_replacement` → `final_report`), which reports the playhead it really reached, so its
/// resume point survives the step. Restarting the current episode is `from_beginning` (the Info
/// tab's action, and a More-menu row in a package that folds it), not a Previous.
pub(super) fn step(
    ps: &mut plx_media::route::PlaybackSession,
    pa: &mut plx_media::player::adapter::PlayerAdapter,
    direction: QueueStep,
    pages: &mut plx_ui::dispatch::Dispatcher<super::super::bridge::AppHost>,
    bridge: &mut super::super::bridge::Bridge,
) {
    let Some(row) = queue_neighbor(ps, direction) else {
        return;
    };
    log(&format!("queue transport: {direction:?} rk={}", row.rk));
    play(ps, pa, row, HUD_LINGER_MS, pages, bridge);
}

pub(super) fn play(
    ps: &mut plx_media::route::PlaybackSession,
    pa: &mut plx_media::player::adapter::PlayerAdapter,
    row: QueueRow,
    hud_ms: u32,
    pages: &mut plx_ui::dispatch::Dispatcher<super::super::bridge::AppHost>,
    bridge: &mut super::super::bridge::Bridge,
) -> bool {
    // The row is owned before the outgoing queue or engine can be retired. Its server identity
    // follows it across the handoff, independent of the server being browsed behind the player.
    let resume = plx_data::metadata::resume_ns(row.resume_ms, row.dur_ms);
    let context = if row.kind == "episode" {
        plx_ui::fmt::episode_kicker(row.season, row.index, &row.title)
    } else {
        String::new()
    };
    close_player_overlays(pages);
    if !plx_media::player::stop_for_replacement(ps, pa) {
        return false;
    }
    let requested = plx_media::route::request_play_queue_item(ps, bridge.metadata_mut(), start_caps(), &row, &context);
    bridge.start_store_jobs();
    if !requested { return false; }
    bridge.metadata_run(plx_data::stores::metadata::MetadataCmd::RetirePlaying);
    bridge.metadata_run(plx_data::stores::metadata::MetadataCmd::RequestDetail {
            sid: row.sid,
            rk: row.rk,
        });
    start_playback(
        ps,
        pa,
        resume,
        Origin::Unchanged,
        hud_ms,
        None,
        pages,
        bridge,
    );
    true
}
