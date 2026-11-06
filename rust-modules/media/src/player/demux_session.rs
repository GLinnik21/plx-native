//! The caption half of the physical demux worker's session. The clock, the adaptive actuator, the
//! callback observer and the effect sink are the Feed's own (`plx_play_feed::demux_clock`); what
//! stays here is what touches media's common stores: the cue and ASS inventory behind
//! `DemuxSubtitle` ([`PlayerDemuxSubtitle`]) and the two functions the Feed's session takes as its
//! media hooks, the telemetry class a demux fault is latched as ([`demux_exit_latch`]) and the
//! common `SHARED.track_names` write ([`replace_track_names`]). The application composes them into
//! the session the adapter captures per attempt ([`super::adapter::DemuxSessionProvider`]).
//! Construction captures no globals.

use super::SHARED;
use plx_playback_contract::demux::{DemuxFault, DemuxObserver, DemuxSubtitle};
use plx_playback_contract::subtitle::{DecodeKind, Manifest, RawCue, TrackInfo};
use plx_playback_contract::tracks::TrackNames;
use plx_telemetry::telemetry::classes::DemuxExitClass;
use std::sync::Arc;
use std::sync::atomic::Ordering;

/// The session of one demux attempt over the real Feed, as the host tests capture it: what the
/// application composes (`plx_media::player::adapter::DemuxSessionProvider`), with this module's
/// own hooks. Safe on the main thread: pause cursors and the native clock are sampled only after the
/// worker explicitly begins a lane.
#[cfg(test)]
pub fn capture(attempt: plx_playback_contract::AttemptId, native_epoch: u32)
    -> Box<dyn plx_playback_contract::demux::DemuxSession> {
    use plx_play_feed::demux_clock::{DemuxMediaHooks, FeedDemuxSession};
    FeedDemuxSession::capture(
        attempt, native_epoch,
        DemuxMediaHooks { exit_latch: demux_exit_latch, replace_track_names },
        PlayerDemuxSubtitle::new)
}

/// The caption half of a demux attempt's session: the cue and ASS inventory the demuxer's subtitle
/// tracks feed, addressed through the session's observer.
pub struct PlayerDemuxSubtitle {
    observer: Arc<dyn DemuxObserver>,
    subtitle_tracks: Option<super::subtitle_cue::SideTracks>,
    subtitle_indices: Vec<i32>,
    bitmap_generation: Option<u64>,
    font_budget: super::ass::FontBudget,
}

impl PlayerDemuxSubtitle {
    pub fn new(observer: Arc<dyn DemuxObserver>) -> Self {
        Self {
            observer,
            subtitle_tracks: None, subtitle_indices: Vec::new(),
            bitmap_generation: None,
            font_budget: super::ass::FontBudget::default(),
        }
    }
}

impl DemuxSubtitle for PlayerDemuxSubtitle {
    fn plan(&mut self, tracks: &[TrackInfo]) -> Vec<DecodeKind> {
        let presentation = super::subtitle_cue::SideTracks::new(tracks);
        let plan = presentation.decode_plan();
        self.subtitle_indices = tracks.iter().map(|track| track.stream_index).collect();
        self.subtitle_tracks = Some(presentation);
        let mut generation = None;
        self.observer.with_active(&mut || generation = Some(SHARED.bitmap_generation.load(Ordering::Acquire)));
        self.bitmap_generation = generation;
        self.font_budget = super::ass::FontBudget::default();
        plan
    }

    fn max_header_bytes(&self) -> usize { super::ass_source::MAX_HEADER_BYTES }

    fn admit_font(&mut self, bytes: usize) -> bool { self.font_budget.admit(bytes) }

    fn begin_sources(&mut self, media_key: &str, manifest: Manifest) -> Option<u64> {
        let tracks = self.subtitle_tracks.as_ref()?;
        let inventory = super::subtitle_cue::source_inventory(media_key, tracks.kinds(), manifest);
        loop {
            let mut prepared = Some(super::ass_source::prepare_begin(&inventory));
            let mut committed = None;
            // `commit_prepared` consumes the candidate, so the `FnMut` closure takes it out once.
            if !self.observer.with_active(&mut || {
                committed = prepared.take().map(super::ass_source::commit_prepared);
            }) {
                return None;
            }
            match committed {
                Some(Ok((generation, old))) => {
                    drop(old); // retired fonts and event maps are released outside NativeGate
                    return Some(generation);
                }
                Some(Err(stale)) => {
                    drop(stale); // release candidate events outside NativeGate
                    continue; // a concurrent cue or style change: rebase the inventory
                }
                None => return None,
            }
        }
    }

    fn record_tracks(&self) {
        let Some(tracks) = self.subtitle_tracks.as_ref() else { return; };
        if self.subtitle_indices.is_empty() { return; }
        let desc: Vec<String> = self.subtitle_indices.iter().zip(tracks.kinds()).map(|(index, kind)| {
            let name = match kind {
                super::subtitle_cue::PresentationTrack::Ass => "ass",
                super::subtitle_cue::PresentationTrack::Text(super::text_ass::TextDialect::MovText) => "mov_text",
                super::subtitle_cue::PresentationTrack::Text(_) => "text",
                super::subtitle_cue::PresentationTrack::Bitmap => "image",
            };
            format!("#{index}:{name}")
        }).collect();
        super::log(&format!("ff: sub tracks=[{}] selected={}", desc.join(","), super::desired_sub_idx()));
    }

    fn selected_track(&self) -> i32 { super::desired_sub_idx() }
    fn bitmap_budget(&self) -> usize { super::SUB_BITMAP_BUDGET }

    fn emit(&mut self, generation: u64, raw: RawCue) {
        let Some(retention) = self.observer.caption_retention() else { return; };
        let Some(tracks) = self.subtitle_tracks.as_ref() else { return; };
        let Some(bitmap_generation) = self.bitmap_generation else { return; };
        let cue = tracks.decode(raw);
        cue.push_addressed(generation, bitmap_generation, retention);
    }

    fn seek(&mut self, generation: u64) {
        super::ass_source::seek(generation);
    }
}

/// Publish container-reported track names into the common store and hand back what it held (the
/// caller drops it after the native barrier and this lock are released: old container strings may
/// own large allocations). The one common write a demux effect makes.
pub fn replace_track_names(names: TrackNames) -> TrackNames {
    let mut slot = SHARED.track_names.lock().unwrap();
    let previous = std::mem::replace(&mut *slot, names);
    SHARED.track_names_stamp.fetch_add(1, Ordering::Release);
    previous
}

/// The byte the demuxer's exit latch stores for a fault.
pub fn demux_exit_latch(fault: DemuxFault) -> u8 { demux_exit_class(fault).latch() }

fn demux_exit_class(fault: DemuxFault) -> DemuxExitClass {
    match fault {
        DemuxFault::HttpsReserveFailed => DemuxExitClass::HttpsReserveFailed,
        DemuxFault::HttpsOpenFailed => DemuxExitClass::HttpsOpenFailed,
        DemuxFault::HttpOpenFailed => DemuxExitClass::HttpOpenFailed,
        DemuxFault::OpenInputFailed => DemuxExitClass::OpenInputFailed,
        DemuxFault::FindStreamInfoFailed => DemuxExitClass::FindStreamInfoFailed,
        DemuxFault::NoVideoStream => DemuxExitClass::NoVideoStream,
        DemuxFault::NoFraming => DemuxExitClass::NoFraming,
        DemuxFault::LoadMismatch => DemuxExitClass::LoadMismatch,
        DemuxFault::NoAudioAu => DemuxExitClass::NoAudioAu,
        DemuxFault::NoUsableVideo => DemuxExitClass::NoUsableVideo,
        DemuxFault::DtsCoreMissing => DemuxExitClass::DtsCoreMissing,
        DemuxFault::HlsFailed => DemuxExitClass::HlsFailed,
        DemuxFault::HlsNotReady => DemuxExitClass::HlsNotReady,
        DemuxFault::DemuxPanicked => DemuxExitClass::DemuxPanicked,
        DemuxFault::NoAccessUnits => DemuxExitClass::NoAccessUnits,
        DemuxFault::Other => DemuxExitClass::Other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use plx_play_feed::shared::FEED_SHARED;
    use plx_playback_contract::AttemptId;
    use plx_playback_contract::subtitle::{BitmapDisplay, IndexedRect};

    struct ResetSession;
    impl Drop for ResetSession {
        fn drop(&mut self) { crate::player::reset_session_for_test(); }
    }

    #[test]
    fn late_bitmap_cue_cannot_populate_successor_store() {
        let _serial = plx_base::testlock::serial();
        let _reset = ResetSession;
        crate::player::reset_session_for_test();
        let first_epoch = FEED_SHARED.begin_native_session().expect("first native epoch");
        let mut first = capture(AttemptId::new(7, 1), first_epoch);
        let track = || TrackInfo {
            stream_index: 4, codec_name: "hdmv_pgs_subtitle".into(), language: None,
        };
        first.subtitle().plan(&[track()]);
        let first_generation = first.subtitle().begin_sources("first", Manifest {
            headers: vec![None], fonts: Vec::new(), font_bytes: 0,
        }).expect("first subtitle source");
        crate::player::reset_session_for_test();
        let next_epoch = FEED_SHARED.begin_native_session().expect("successor native epoch");
        let mut next = capture(AttemptId::new(7, 2), next_epoch);
        next.subtitle().plan(&[track()]);
        let next_generation = next.subtitle().begin_sources("next", Manifest {
            headers: vec![None], fonts: Vec::new(), font_bytes: 0,
        }).expect("next subtitle source");
        let cue = |pts| RawCue::Bitmap {
            track: 0, pts,
            display: BitmapDisplay::Set {
                cw: 1920, ch: 1080, total_rects: 1,
                rects: vec![IndexedRect {
                    x: 0, y: 0, w: 1, h: 1, index: vec![1],
                    palette: Box::new([[0; 4]; 256]),
                }],
            },
        };
        first.subtitle().emit(first_generation, cue(100));
        next.subtitle().emit(next_generation, cue(200));
        let bitmaps = SHARED.sub_bitmaps.lock().unwrap();
        assert_eq!(bitmaps.len(), 1, "retired pixels must not enter the successor store");
        assert_eq!(bitmaps[0].start_ns, 200);
    }

}
