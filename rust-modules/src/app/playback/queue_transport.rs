//! Explicit queue navigation and Up Next share one stop → resolve → start ritual.
use super::*;
use plx_media::route::{previous_restarts, queue_neighbor, QueueStep};
use plx_plex::plex::QueueRow;

pub(super) fn step(
    ps: &mut plx_media::route::PlaybackSession,
    pa: &mut plx_media::player::adapter::PlayerAdapter,
    direction: QueueStep,
    pages: &mut plx_ui::dispatch::Dispatcher<super::super::bridge::AppHost>,
    bridge: &mut super::super::bridge::Bridge,
) {
    if direction == QueueStep::Previous && previous_restarts(plx_media::player::intended_pos_ns(ps))
    {
        request_seek(0);
        resume_if_paused(pa);
        return;
    }
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
    plx_media::player::stop_bufferfeed(ps, pa);
    let requested = plx_media::route::request_play_queue_item(ps, bridge.metadata_mut(), &row, &context);
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
