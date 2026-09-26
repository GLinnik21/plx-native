//! Embedded ASS source ownership. The demuxer preserves headers, fonts and timed packets;
//! the renderer receives immutable snapshots, never a pointer into FFmpeg's storage.
//! Selection does not discard read-ahead. A seek replaces each source identity so a completed
//! render from the old position cannot land in the new playback epoch.
use super::ass::{self, Content, Event, Font, Source};
use std::collections::BTreeMap;
use std::sync::{Arc, MutexGuard};

pub(crate) const MAX_HEADER_BYTES: usize = 1024 * 1024;
pub(crate) const MAX_HEADERS_BYTES: usize = 4 * MAX_HEADER_BYTES;
pub(crate) const MAX_TRACKS: usize = 64;
pub(crate) const MAX_FONTS: usize = 128;
pub(crate) const MAX_FONT_BYTES: usize = 32 * 1024 * 1024;
const MAX_PACKET_BYTES: usize = 256 * 1024;
const MAX_EVENTS: usize = 8192;
const MAX_EVENT_BYTES: usize = 8 * 1024 * 1024;
const MAX_MEDIA_KEY_BYTES: usize = 64 * 1024;

#[derive(Default)]
pub(crate) struct Store {
    generation: u64,
    // Exact delivery identity, never logged: it may include an authentication token.
    media_key: Option<String>,
    tracks: BTreeMap<i32, Arc<Source>>,
    fonts: Option<Arc<[Font]>>,
}
fn store() -> MutexGuard<'static, Store> {
    super::SHARED
        .ass_sources
        .lock()
        .unwrap_or_else(|e| e.into_inner())
}

impl Store {
    pub(crate) const fn new() -> Self {
        Self {
            generation: 0,
            media_key: None,
            tracks: BTreeMap::new(),
            fonts: None,
        }
    }

    pub(super) fn reset(&mut self) {
        self.generation = self.generation.wrapping_add(1);
        self.media_key = None;
        self.tracks.clear();
        self.fonts = None;
    }

    /// Retire producer/raster identities while retaining immutable facts about the
    /// current file. The reopened demuxer must prove its identity and header inventory
    /// again in `begin` before it can inherit any packets.
    pub(super) fn retain_for_reload(&mut self) {
        self.generation = self.generation.wrapping_add(1);
        self.seek(self.generation);
    }

    fn begin(&mut self, media_key: &str, headers: Vec<(i32, Vec<u8>)>, fonts: Vec<Font>) -> u64 {
        let same_file = !media_key.is_empty()
            && self.media_key.as_deref() == Some(media_key)
            && headers.len() == self.tracks.len()
            && headers.iter().all(|(track, bytes)| {
                self.tracks.get(track).is_some_and(|source| {
                    matches!(&source.content, Content::Embedded { header, .. }
                        if header.as_ref() == bytes.as_slice())
                })
            });
        let previous = if same_file {
            std::mem::take(&mut self.tracks)
        } else {
            BTreeMap::new()
        };
        self.reset();
        if !media_key.is_empty() && media_key.len() <= MAX_MEDIA_KEY_BYTES {
            self.media_key = Some(media_key.to_owned());
        }
        let mut bytes = 0;
        let fonts: Arc<[Font]> = fonts
            .into_iter()
            .take(MAX_FONTS)
            .filter(|f| {
                bytes += f.data.len();
                bytes <= MAX_FONT_BYTES
            })
            .collect::<Vec<_>>()
            .into();
        self.fonts = Some(fonts.clone());
        let mut header_bytes = 0;
        for (track, header) in headers.into_iter().take(MAX_TRACKS) {
            if header.len() > MAX_HEADER_BYTES || header_bytes + header.len() > MAX_HEADERS_BYTES {
                continue;
            }
            header_bytes += header.len();
            // An unchanged inventory also preserves the per-track/global byte budgets.
            // A demux reopen seeks to a video cue and may never resend a long sign.
            let events = previous.get(&track).and_then(|source| match &source.content {
                Content::Embedded { events, .. } => Some(events.clone()),
                _ => None,
            }).unwrap_or_else(|| Arc::from([]));
            self.tracks.insert(
                track,
                Arc::new(Source {
                    id: ass::next_source_id(),
                    revision: 0,
                    content: Content::Embedded {
                        header: header.into(),
                        events,
                        fonts: fonts.clone(),
                    },
                }),
            );
        }
        self.generation
    }

    fn push(&mut self, generation: u64, track: i32, event: Event, floor_ms: i64) {
        if generation != self.generation
            || event.payload.len() > MAX_PACKET_BYTES
            || event.duration_ms <= 0
        {
            return;
        }
        let Some(old) = self.tracks.get(&track) else {
            return;
        };
        let Content::Embedded {
            header,
            events,
            fonts,
        } = &old.content
        else {
            return;
        };
        // Copies only small event descriptors and Arc handles. Font/script/payload bytes stay
        // shared, and the render worker never holds this store lock during native work.
        if events.iter().any(|known| known == &event) {
            return; // reread after a backward seek: preserve one source event and its budget
        }
        let mut events: Vec<_> = events
            .iter()
            .filter(|e| e.start_ms.saturating_add(e.duration_ms) >= floor_ms)
            .cloned()
            .collect();
        events.push(event);
        let mut bytes: usize = events.iter().map(|e| e.payload.len()).sum();
        // Dense karaoke can carry many events at one timestamp. Bound BYTES as well as count;
        // retire the earliest-ending events first, retaining the useful forward window.
        // A file with dozens of subtitle languages shares one 16 MiB packet allowance.
        // Each individual native source also fits the renderer's 8 MiB script bound.
        let budget =
            (MAX_EVENT_BYTES - header.len()).min(16 * 1024 * 1024 / self.tracks.len().max(1));
        if events.len() > MAX_EVENTS || bytes > budget {
            events.sort_by_key(|e| e.start_ms.saturating_add(e.duration_ms));
            let mut n = 0;
            while events.len() - n > MAX_EVENTS || bytes > budget {
                bytes -= events[n].payload.len();
                n += 1;
            }
            events.drain(..n);
        }
        let source = Source {
            id: old.id,
            revision: old.revision.wrapping_add(1),
            content: Content::Embedded {
                header: header.clone(),
                events: events.into(),
                fonts: fonts.clone(),
            },
        };
        self.tracks.insert(track, Arc::new(source));
    }

    fn seek(&mut self, generation: u64) {
        if generation != self.generation {
            return;
        }
        for source in self.tracks.values_mut() {
            let Content::Embedded {
                header,
                events,
                fonts,
            } = &source.content
            else {
                continue;
            };
            *source = Arc::new(Source {
                id: ass::next_source_id(),
                revision: 0,
                content: Content::Embedded {
                    header: header.clone(),
                    fonts: fonts.clone(),
                    // These are immutable facts about this media, not images from the old
                    // clock. Matroska seeks do not resend an earlier cue spanning the target.
                    events: events.clone(),
                },
            });
        }
    }
}

pub(crate) fn begin(media_key: &str, headers: Vec<(i32, Vec<u8>)>, fonts: Vec<Font>) -> u64 {
    store().begin(media_key, headers, fonts)
}
pub(crate) fn push(generation: u64, track: i32, start_ns: i64, end_ns: i64, payload: &[u8]) {
    if payload.len() > MAX_PACKET_BYTES {
        return;
    }
    store().push(
        generation,
        track,
        Event {
            start_ms: start_ns / 1_000_000,
            duration_ms: end_ns.saturating_sub(start_ns) / 1_000_000,
            payload: payload.into(),
        },
        super::subtitle_floor_ns() / 1_000_000,
    );
}
pub(crate) fn seek(generation: u64) {
    store().seek(generation);
}
#[cfg(test)]
pub(crate) fn reset() {
    store().reset();
}
pub(crate) fn font_snapshot() -> (u64, Arc<[Font]>) {
    let mut store = store();
    let fonts = store.fonts.get_or_insert_with(|| Arc::from([])).clone();
    (store.generation, fonts)
}
pub(crate) fn selected(track: i32) -> Option<Arc<Source>> {
    store().tracks.get(&track).cloned()
}

/// Interpolate the sparse native clock only while it is advancing. Limit extrapolation to
/// one normal callback interval plus jitter; a stalled decoder must not animate indefinitely.
#[derive(Default)]
pub(crate) struct Clock {
    anchor: Option<(i64, u32)>,
    last_ms: i64,
    running: bool,
}
impl Clock {
    pub(crate) fn sample(&mut self, position_ns: i64, tick_ms: u32, running: bool) -> i64 {
        let position_ms = position_ns / 1_000_000;
        let continuous = self.running
            && running
            && self
                .anchor
                .is_some_and(|(p, _)| position_ms >= p && position_ms.saturating_sub(p) <= 500);
        let changed = self.anchor.is_none_or(|(p, _)| p != position_ms);
        if changed || running != self.running {
            self.anchor = Some((position_ms, tick_ms));
        }
        self.running = running;
        let (_, at) = self.anchor.unwrap();
        let projected = position_ms.saturating_add(if running {
            tick_ms.wrapping_sub(at).min(250) as i64
        } else {
            0
        });
        // Never extrapolate across a discontinuity or a pause. Ordinary callback jitter may
        // put the fresh anchor just behind the prior estimate; hold rather than rewind a fade.
        self.last_ms = if continuous {
            projected.max(self.last_ms)
        } else {
            projected
        };
        self.last_ms
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resetting_another_playback_owner_cannot_retire_the_live_subtitles() {
        let _guard = crate::testlock::serial();
        begin("fixture", vec![(0, b"header".to_vec())], vec![]);
        let id = selected(0).unwrap().id;
        let other = crate::player::Shared::new();
        other.reset_session();
        assert_eq!(selected(0).unwrap().id, id);
        reset();
    }

    fn event(text: &str, start_ms: i64) -> Event {
        Event {
            start_ms,
            duration_ms: 2000,
            payload: text.as_bytes().into(),
        }
    }

    #[test]
    fn an_in_place_seek_preserves_a_known_sign_spanning_the_target() {
        let mut s = Store::default();
        let generation = s.begin("fixture", vec![(0, b"header".to_vec())], vec![]);
        let sign = Event {
            start_ms: 0,
            duration_ms: 120_000,
            payload: b"0,1,Sign,,0,0,0,,long sign".as_slice().into(),
        };
        s.push(generation, 0, sign.clone(), 0);
        let old_id = s.tracks[&0].id;
        s.seek(generation);
        let after = &s.tracks[&0];
        assert_ne!(after.id, old_id, "the old raster must be fenced");
        let Content::Embedded { events, .. } = &after.content else {
            panic!()
        };
        assert_eq!(
            events.as_ref(),
            &[sign.clone()],
            "av_seek_frame resumes at a video cue and does not resend an earlier long sign"
        );
        // A backward seek can reread a packet we already retained. It is one event, not
        // another cache entry consuming the source budget on every rewind.
        s.push(generation, 0, sign, 0);
        let Content::Embedded { events, .. } = &s.tracks[&0].content else {
            panic!()
        };
        assert_eq!(events.len(), 1);
    }

    #[test]
    fn backward_seek_packets_survive_until_the_presentation_clock_rebases() {
        let mut s = Store::default();
        let generation = s.begin("fixture", vec![(0, b"header".to_vec())], vec![]);
        // The demuxer is reading the requested ten-second position, while the native
        // presentation callback still reports ninety seconds until the first new picture.
        let floor_ms =
            super::super::subtitle_floor_for(90_000_000_000, true, 10_000_000_000) / 1_000_000;
        s.push(generation, 0, event("first new cue", 10_000), floor_ms);
        s.push(generation, 0, event("next new cue", 12_000), floor_ms);
        let Content::Embedded { events, .. } = &s.tracks[&0].content else {
            panic!()
        };
        assert_eq!(
            events.len(),
            2,
            "the old clock must not evict freshly read seek cues"
        );
    }
    #[test]
    fn reload_seek_keeps_a_known_sign_that_matroska_will_not_resend() {
        let shared = crate::player::Shared::new();
        let sign = Event {
            start_ms: 0,
            duration_ms: 120_000,
            payload: b"0,1,Sign,,0,0,0,,long sign".as_slice().into(),
        };
        let before = {
            let mut s = shared.ass_sources.lock().unwrap();
            let generation = s.begin("fixture", vec![(0, b"header".to_vec())], vec![]);
            s.push(generation, 0, sign.clone(), 0);
            s.tracks[&0].id
        };
        shared.reset_session_for_reload();
        let mut s = shared.ass_sources.lock().unwrap();
        s.begin("fixture", vec![(0, b"header".to_vec())], vec![]);
        let source = &s.tracks[&0];
        assert_ne!(source.id, before, "retire the old raster across a fresh native Load");
        let Content::Embedded { events, .. } = &source.content else { panic!() };
        assert_eq!(events.as_ref(), &[sign], "a reload-based seek must preserve already-read media facts");
    }
    #[test]
    fn demux_reopen_keeps_known_events_and_rejects_the_old_producer() {
        let mut s = Store::new();
        let old_generation = s.begin("part-A", vec![(0, b"header".to_vec())], vec![]);
        let cue = event("known", 1000);
        s.push(old_generation, 0, cue.clone(), 0);
        let old_id = s.tracks[&0].id;
        let generation = s.begin("part-A", vec![(0, b"header".to_vec())], vec![]);
        assert_ne!(old_generation, generation);
        assert_ne!(old_id, s.tracks[&0].id);
        s.push(old_generation, 0, event("stale producer", 1000), 0);
        s.push(generation, 0, cue.clone(), 0); // a reread still deduplicates
        let Content::Embedded { events, .. } = &s.tracks[&0].content else { panic!() };
        assert_eq!(events.as_ref(), &[cue]);
    }

    #[test]
    fn new_playback_or_changed_file_inventory_cannot_inherit_subtitle_events() {
        for (fresh, key, headers) in [
            (true, "part-A", vec![(0, b"header".to_vec())]),
            (false, "part-B", vec![(0, b"header".to_vec())]),
            (false, "part-A", vec![(0, b"different style sheet".to_vec())]),
            (false, "part-A", vec![(0, b"header".to_vec()), (1, b"new track".to_vec())]),
        ] {
            let shared = crate::player::Shared::new();
            {
                let mut s = shared.ass_sources.lock().unwrap();
                let generation = s.begin("part-A", vec![(0, b"header".to_vec())], vec![]);
                s.push(generation, 0, event("belongs to previous source", 1000), 0);
            }
            if fresh { shared.reset_session(); } else { shared.reset_session_for_reload(); }
            let mut s = shared.ass_sources.lock().unwrap();
            s.begin(key, headers, vec![]);
            let Content::Embedded { events, .. } = &s.tracks[&0].content else { panic!() };
            assert!(events.is_empty(), "only the same active playback and file inventory may reuse events");
        }
    }
    #[test]
    fn overlapping_ass_events_and_headers_survive_without_flattening() {
        let mut s = Store::default();
        let g = s.begin("fixture", vec![(0, b"[V4+ Styles]\nStyle: Sign".to_vec())], vec![]);
        let sign = r"0,1,Sign,,0,0,0,,{\pos(120,80)\c&H0000FF&}sign";
        let dialogue = r"1,0,Default,,0,0,0,,{\k20}dialogue";
        s.push(g, 0, event(sign, 1000), 0);
        s.push(g, 0, event(dialogue, 1000), 0);
        let Content::Embedded { header, events, .. } = &s.tracks[&0].content else {
            panic!()
        };
        assert_eq!(header.as_ref(), b"[V4+ Styles]\nStyle: Sign");
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].payload.as_ref(), sign.as_bytes());
        assert_eq!(events[1].payload.as_ref(), dialogue.as_bytes());
    }
    #[test]
    fn seek_retires_old_frames_but_keeps_headers_and_fonts() {
        let mut s = Store::default();
        let g = s.begin(
            "fixture",
            vec![(0, b"header".to_vec())],
            vec![Font {
                name: "font.ttf".into(),
                data: Arc::from(&b"font"[..]),
            }],
        );
        s.push(g, 0, event("a", 1000), 0);
        let before = s.tracks[&0].clone();
        s.seek(g);
        let after = &s.tracks[&0];
        assert_ne!(before.id, after.id);
        let Content::Embedded {
            header,
            events,
            fonts,
        } = &after.content
        else {
            panic!()
        };
        assert_eq!(
            events.len(),
            1,
            "a seek retires the raster, not known media events"
        );
        assert_eq!(header.as_ref(), b"header");
        assert_eq!(fonts.len(), 1);
        s.reset();
        s.push(g, 0, event("stale", 2000), 0);
        assert!(s.tracks.is_empty());
    }
    #[test]
    fn callback_jitter_never_rewinds_on_the_following_tick() {
        let mut c = Clock::default();
        assert_eq!(c.sample(1_000_000_000, 0, true), 1000);
        assert_eq!(c.sample(1_000_000_000, 250, true), 1250);
        assert_eq!(c.sample(1_200_000_000, 250, true), 1250);
        assert_eq!(c.sample(1_200_000_000, 266, true), 1250);
        assert_eq!(c.sample(1_200_000_000, 316, true), 1266);
    }
    #[test]
    fn interpolation_advances_between_callbacks_and_stops_when_paused_or_stalled() {
        let mut c = Clock::default();
        assert_eq!(c.sample(1_000_000_000, 0, true), 1000);
        assert_eq!(c.sample(1_000_000_000, 80, true), 1080);
        assert_eq!(c.sample(1_000_000_000, 900, true), 1250);
        assert_eq!(c.sample(1_000_000_000, 1000, false), 1000);
        assert_eq!(c.sample(1_000_000_000, 2000, false), 1000);
        assert_eq!(c.sample(20_000_000_000, 2100, true), 20000);
        assert_eq!(c.sample(2_000_000_000, 2200, true), 2000);
    }
}
