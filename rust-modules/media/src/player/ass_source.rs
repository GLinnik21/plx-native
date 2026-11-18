//! Embedded ASS source ownership. The demuxer hands over headers, fonts and timed packets;
//! headers and packets lose their trailing NUL terminators on the way in, and the renderer
//! receives immutable snapshots, never a pointer into FFmpeg's storage.
//! Selection does not discard read-ahead. A seek replaces each source identity so a completed
//! render from the old position cannot land in the new playback epoch.
//! An authored track keeps the file's own bytes as its identity and is handed to the renderer with
//! the viewer's colour pick applied to its pure-white-primary styles (`ass_ink`); a track with no
//! such style is never restyled by the pick.
use super::ass::{self, Content, Event, Font, Origin, Source};
use super::text_ass::{self, Align, AlignClass, ClipCue, TextDialect};
use super::text_style::TextStyle;
use plx_plex::plex::session::SubtitleTone;
use std::collections::BTreeMap;
use std::sync::{Arc, MutexGuard};

// What can be shown is never cut by a count. Every ASS track of the file keeps its header (a
// copy of bytes FFmpeg already holds, so memory is bounded by the file's own stream headers) and
// every attached font is kept whole (the same, bounded by the file's own attachments). No byte
// budget decides which font a track can use (issue #1328): the renderer hands libass the fonts the
// track names (`ass::font_names`). What is bounded is per-track EVENT memory and the per-push work:
// a forward window of `MAX_EVENTS` events and `MAX_EVENT_BYTES` bytes, of which the selected track
// may use a whole script's worth and the others share one pool of the same size.
pub const MAX_HEADER_BYTES: usize = 1024 * 1024;
pub(crate) const MAX_PACKET_BYTES: usize = 256 * 1024;
const MAX_EVENTS: usize = 8192;
pub(crate) const MAX_EVENT_BYTES: usize = 8 * 1024 * 1024;
/// Byte pool the unselected tracks' events share between them (the selected track has its own).
const OTHER_TRACKS_EVENT_BYTES: usize = 8 * 1024 * 1024;
/// An event this far ahead of the newest packet read can only be one a backward seek left
/// behind; the demuxer reads it again on its way forward, so it is retired first.
const STALE_AHEAD_MS: i64 = 5_000;
const MAX_MEDIA_KEY_BYTES: usize = 64 * 1024;

/// The font list of a reopen of the same file is the one already held: same count, and the same
/// name and size at each place. A stronger test would read every byte of fonts that are, by the
/// file's identity, the same bytes.
fn same_font_list(held: &[Font], fresh: &[Font]) -> bool {
    held.len() == fresh.len()
        && held.iter().zip(fresh).all(|(a, b)| a.name == b.name && a.data.len() == b.data.len())
}

fn trim_terminators(mut bytes: &[u8]) -> &[u8] {
    while bytes.last() == Some(&0) {
        bytes = &bytes[..bytes.len() - 1];
    }
    bytes
}

/// What the demuxer registers for one subtitle track in [`begin`].
#[derive(Clone)]
pub enum TrackHeader {
    /// An ASS/SSA track: the codec-private header, as the file wrote it.
    Authored(Vec<u8>),
    /// A text track (SubRip, WebVTT, mov_text, ...): there is no header in the file. The store
    /// synthesizes one from the viewer's size, position and colour picks (a colour change restyles
    /// the track, a change among white and the grays does not: a gray is a draw-time tint) and
    /// remembers the dialect its cues are written in.
    Text(TextDialect),
}

/// An authored track as the FILE wrote it, beside the header the renderer is given for the viewer's
/// colour pick ([`super::ass_ink`]): the file's bytes are the reopen identity and the source the pick
/// is applied to afresh each time, never to an earlier result.
#[derive(Clone)]
struct AuthoredTrack {
    file: Arc<[u8]>,
    /// The pick the held header was built for.
    tone: SubtitleTone,
    /// Whether the colour pick can reach this track at all: the FILE's header has a style whose primary
    /// is pure white ([`super::ass_ink::takes_colour`]). Read once here, from the file's bytes (the held
    /// header reads false under a colour that already took them), so a per-frame poll never parses.
    takes_colour: bool,
}

/// A cue as the text track received it, converted: the ASS packet it became and the ReadOrder in it.
/// The cue's own start and end are the key it is held under ([`RawKey`]); the view's end may be
/// clipped shorter.
#[derive(Clone)]
struct RawCue {
    readorder: u32,
    align: Align,
    /// The complete Matroska ASS packet (`text_ass::event_payload`).
    payload: Arc<[u8]>,
}

/// (start_ms, end_ms, [`packet_identity`] of the packet's bytes as the demuxer delivered them,
/// BEFORE the charset step). The key is the dedup identity of a packet: a re-read after a seek or a
/// demux reopen delivers the same one again. It is a hash, not the text: the charset detector
/// learns as it reads, so the same packet can decode to different text the second time, and the
/// raw map must not hold a second copy of every cue's text beside its payload.
type RawKey = (i64, i64, u64);

/// A 64-bit FNV-1a of a packet's bytes: the third part of a [`RawKey`].
pub fn packet_identity(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325, |h, b| (h ^ u64::from(*b)).wrapping_mul(0x0100_0000_01b3))
}

/// A text track the store holds: its cues are converted by `dialect`.
///
/// `raw` is the truth about what the track received and `Source.events` is a view derived from it
/// ([`TextTrack::view`]): each cue's end clipped to the start of the next cue of its alignment
/// class (D10, newest wins). The view cannot answer "was this packet seen" (a clipped cue differs
/// from the packet), which is why a text track has no entry in `Store::seen`.
#[derive(Clone)]
struct TextTrack {
    dialect: TextDialect,
    raw: BTreeMap<RawKey, RawCue>,
    /// The ReadOrder the next new cue gets. It only increases, and a pruned cue's value is never
    /// handed out again: libass runs with `ass_set_check_readorder`, so two events with one
    /// ReadOrder would silently drop the second.
    next_readorder: u32,
    /// `TextStyle::generation()` of the picks the track's header was synthesized for.
    style_generation: u32,
}

impl TextTrack {
    fn new(dialect: TextDialect, style_generation: u32) -> Self {
        Self { dialect, raw: BTreeMap::new(), next_readorder: 0, style_generation }
    }

    /// The newest-wins view of the raw cues, in start order. A cue clipped to nothing is not in it.
    fn view(&self) -> Arc<[Event]> {
        let cues: Vec<ClipCue> = self
            .raw
            .iter()
            .map(|((start, end, _), cue)| ClipCue {
                start: *start,
                end: *end,
                class: AlignClass::of_align(cue.align),
                seq: cue.readorder,
            })
            .collect();
        let clipped = text_ass::clip_newest_wins(&cues);
        self.raw
            .values()
            .zip(clipped)
            .filter_map(|(cue, span)| {
                span.map(|(start, end)| Event {
                    start_ms: start,
                    duration_ms: end - start,
                    payload: cue.payload.clone(),
                })
            })
            .collect()
    }
}

/// Which of `items` (start_ms, end_ms, bytes) to retire so that at most `MAX_EVENTS` and `budget`
/// bytes remain. In order: items far AHEAD of `frontier`, the packet just read (left by a backward
/// seek; the demuxer reads them again, farthest first), then the earliest-ending, which keeps the
/// useful forward window. `true` = retire.
fn retire_to_fit(items: &[(i64, i64, usize)], frontier: i64, budget: usize) -> Vec<bool> {
    let mut bytes: usize = items.iter().map(|i| i.2).sum();
    let mut left = items.len();
    let mut retire = vec![false; items.len()];
    if left <= MAX_EVENTS && bytes <= budget {
        return retire;
    }
    let mut order: Vec<usize> = (0..items.len()).collect();
    order.sort_by_key(|&i| {
        let (start, end, _) = items[i];
        if start > frontier.saturating_add(STALE_AHEAD_MS) {
            (0u8, -start)
        } else {
            (1u8, end)
        }
    });
    for &i in &order {
        if left <= MAX_EVENTS && bytes <= budget {
            break;
        }
        retire[i] = true;
        bytes -= items[i].2;
        left -= 1;
    }
    retire
}

#[derive(Clone, Default)]
pub struct Store {
    generation: u64,
    /// Changes on every producer or presentation mutation. An off-lock source
    /// preparation may publish only against the snapshot it read.
    revision: u64,
    // Exact delivery identity, never logged: it may include an authentication token.
    media_key: Option<String>,
    awaiting_reopen: bool,
    tracks: BTreeMap<i32, Arc<Source>>,
    // The tracks of `tracks` that are Text origin, by track.
    text: BTreeMap<i32, TextTrack>,
    // The tracks of `tracks` the file styled, by track.
    authored: BTreeMap<i32, AuthoredTrack>,
    fonts: Option<Arc<[Font]>>,
    // Dedup index: the events held per track, so a reread after a seek is an O(1) lookup.
    seen: BTreeMap<i32, std::collections::HashSet<Event>>,
    // The track the viewer is watching (mirrors `desired_sub_idx`), which gets the full budget.
    selected_track: Option<i32>,
}
fn store() -> MutexGuard<'static, Store> {
    super::SHARED
        .ass_sources
        .lock()
        .unwrap_or_else(|e| e.into_inner())
}

impl Store {
    pub const fn new() -> Self {
        Self {
            generation: 0,
            revision: 0,
            media_key: None,
            awaiting_reopen: false,
            tracks: BTreeMap::new(),
            text: BTreeMap::new(),
            authored: BTreeMap::new(),
            fonts: None,
            seen: BTreeMap::new(),
            selected_track: None,
        }
    }

    pub(super) fn reset(&mut self) {
        self.generation = self.generation.wrapping_add(1);
        self.revision = self.revision.wrapping_add(1);
        self.media_key = None;
        self.awaiting_reopen = false;
        self.tracks.clear();
        self.text.clear();
        self.authored.clear();
        self.seen.clear();
        self.fonts = None;
    }

    /// Retire producer/raster identities while retaining immutable facts about the
    /// current file. The reopened demuxer must prove its identity and header inventory
    /// again in `begin` before it can inherit any packets.
    pub(super) fn retain_for_reload(&mut self) {
        self.generation = self.generation.wrapping_add(1);
        self.revision = self.revision.wrapping_add(1);
        self.seek(self.generation);
        self.awaiting_reopen = true;
        self.fonts = None;
    }

    /// The source of `track` for the viewer's picks `style`. A text track whose header was built
    /// for other picks is restyled here, lazily and once: the header is re-synthesized and the
    /// revision bumped (the id stays, so the screen's clock and the held frame are not reset); the
    /// engine swaps the track in place (plan §2.5). A track nobody selects is never restyled.
    fn selected_styled(&mut self, track: i32, style: TextStyle) -> Option<Arc<Source>> {
        if self.awaiting_reopen {
            return None;
        }
        if let (Some(text), Some(source)) = (self.text.get_mut(&track), self.tracks.get_mut(&track)) {
            if text.style_generation != style.generation() {
                self.revision = self.revision.wrapping_add(1);
                text.style_generation = style.generation();
                if let Content::Embedded { events, fonts, .. } = &source.content {
                    *source = Arc::new(Source {
                        id: source.id,
                        revision: source.revision.wrapping_add(1),
                        origin: Origin::Text(style),
                        content: Content::Embedded {
                            header: text_ass::header(&style).into_bytes().into(),
                            events: events.clone(),
                            fonts: fonts.clone(),
                        },
                    });
                }
            }
        }
        // An authored track takes the colour pick in its white-primary styles (`ass_ink`): the header the
        // renderer holds is rebuilt from the FILE's bytes when the pick moves, the track keeps its id and
        // its events, and the engine swaps it in place (`ass::restyles`). A pick that changes nothing in
        // this header (a grey, a size, a file with no white style) bumps nothing. This runs inline, under the
        // store lock, ONCE per pick change that moves the colour, and costs a scan plus a copy of the
        // header: at most `MAX_HEADER_BYTES` (1 MiB; a header over it is never admitted), and a real
        // Aegisub header is hundreds of KB at most. That is far more than the few hundred bytes a text track
        // synthesizes just above, but a bounded, rare cost, so it stays here instead of taking a worker
        // for the header alone. A sidecar script, whose whole file is up to `SIDECAR_MAX_BYTES` (4 MiB),
        // does the same on a worker (`sidecar::start_recolour`).
        if let (Some(ink), Some(source)) = (self.authored.get_mut(&track), self.tracks.get_mut(&track)) {
            if ink.tone != style.tone {
                ink.tone = style.tone;
                if let Content::Embedded { header, events, fonts } = &source.content {
                    let wanted: Arc<[u8]> = super::ass_ink::recolour(&ink.file, style.tone).map_or_else(|| ink.file.clone(), Arc::from);
                    if *header != wanted {
                        self.revision = self.revision.wrapping_add(1);
                        *source = Arc::new(Source {
                            id: source.id,
                            revision: source.revision.wrapping_add(1),
                            origin: Origin::Authored,
                            content: Content::Embedded { header: wanted, events: events.clone(), fonts: fonts.clone() },
                        });
                    }
                }
            }
        }
        self.tracks.get(&track).cloned()
    }

    /// Can the viewer's colour pick reach `track`? A text track always takes it (its header is built
    /// from the pick); an authored one when the file has a style the pick recolours; `None` for a track
    /// the store does not hold (nothing is known, so the caller must not dim anything).
    fn takes_colour(&self, track: i32) -> Option<bool> {
        match self.authored.get(&track) {
            Some(ink) => Some(ink.takes_colour),
            None => self.text.contains_key(&track).then_some(true),
        }
    }

    #[cfg(test)]
    fn selected(&mut self, track: i32) -> Option<Arc<Source>> {
        self.selected_styled(track, TextStyle::current())
    }

    #[cfg(test)]
    fn begin(&mut self, media_key: &str, headers: Vec<(i32, TrackHeader)>, fonts: Vec<Font>) -> u64 {
        self.begin_styled(media_key, headers, fonts, TextStyle::current())
    }

    fn begin_styled(
        &mut self,
        media_key: &str,
        headers: Vec<(i32, TrackHeader)>,
        fonts: Vec<Font>,
        style: TextStyle,
    ) -> u64 {
        let headers: Vec<_> = headers
            .into_iter()
            .filter_map(|(track, header)| match header {
                TrackHeader::Authored(mut header) => {
                    if header.len() > MAX_HEADER_BYTES {
                        return None;
                    }
                    header.truncate(trim_terminators(&header).len());
                    Some((track, TrackHeader::Authored(header)))
                }
                // The synthesized header is ours, so the bound on a file's own header is not.
                text @ TrackHeader::Text(_) => Some((track, text)),
            })
            .collect();
        // The same file: its tracks match one for one. An authored track matches by header bytes;
        // a text track by dialect, since its header follows the viewer's settings, not the file.
        let same_file = !media_key.is_empty()
            && self.media_key.as_deref() == Some(media_key)
            && headers.len() == self.tracks.len()
            && headers.iter().all(|(track, registered)| {
                self.tracks.get(track).is_some_and(|source| match registered {
                    TrackHeader::Authored(bytes) => {
                        source.origin == Origin::Authored
                            && self.authored.get(track).is_some_and(|ink| ink.file.as_ref() == bytes.as_slice())
                    }
                    TrackHeader::Text(dialect) => {
                        source.origin.is_text()
                            && self.text.get(track).is_some_and(|t| t.dialect == *dialect)
                    }
                })
            });
        // A same-file reopen keeps what the demuxer will not resend: an authored track its events,
        // a text track its raw cues (the view is derived from them, so the view alone cannot
        // rebuild the dedup index and its ReadOrders).
        let (previous, mut previous_text) = if same_file {
            (std::mem::take(&mut self.tracks), std::mem::take(&mut self.text))
        } else {
            (BTreeMap::new(), BTreeMap::new())
        };
        self.reset();
        if !media_key.is_empty() && media_key.len() <= MAX_MEDIA_KEY_BYTES {
            self.media_key = Some(media_key.to_owned());
        }
        // Every attached font is kept (issue #1328): which of them libass is handed is the
        // renderer's choice, by the names the track uses (`ass::font_names`). A reopen of the same
        // file keeps the font list the tracks already hold, so the renderer's same-fonts check is a
        // pointer compare and not a read of every font's bytes.
        let held = previous.values().find_map(|source| match &source.content {
            Content::Embedded { fonts, .. } | Content::Script { fonts, .. } => Some(fonts.clone()),
        });
        let fonts: Arc<[Font]> = match held {
            Some(held) if same_font_list(&held, &fonts) => held,
            _ => fonts.into(),
        };
        self.fonts = Some(fonts.clone());
        for (track, registered) in headers {
            let (origin, header, events) = match registered {
                TrackHeader::Authored(header) => {
                    // An unchanged inventory also preserves the per-track/global byte budgets.
                    // A demux reopen seeks to a video cue and may never resend a long sign.
                    let events = previous.get(&track).and_then(|source| match &source.content {
                        Content::Embedded { events, .. } => Some(events.clone()),
                        _ => None,
                    }).unwrap_or_else(|| Arc::from([]));
                    self.seen.insert(track, events.iter().cloned().collect());
                    // the renderer's header is the file's with the viewer's colour taken into its white styles
                    let file: Arc<[u8]> = header.into();
                    let held = super::ass_ink::recolour(&file, style.tone).map_or_else(|| file.clone(), Arc::from);
                    let takes_colour = super::ass_ink::takes_colour(&file);
                    self.authored.insert(track, AuthoredTrack { file, tone: style.tone, takes_colour });
                    (Origin::Authored, held, events)
                }
                TrackHeader::Text(dialect) => {
                    // The header is built for the picks of now, so the generation is the current one
                    // too: the old value beside a new header would restyle once more for nothing.
                    let mut text = match previous_text.remove(&track) {
                        Some(held) if held.dialect == dialect => held,
                        _ => TextTrack::new(dialect, style.generation()),
                    };
                    text.style_generation = style.generation();
                    let events = text.view();
                    self.text.insert(track, text);
                    (Origin::Text(style), text_ass::header(&style).into_bytes().into(), events)
                }
            };
            self.tracks.insert(
                track,
                Arc::new(Source {
                    id: ass::next_source_id(),
                    revision: 0,
                    origin,
                    content: Content::Embedded {
                        header,
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
        // A text track's events are the view of its raw cues; only `push_text` writes them.
        if self.text.contains_key(&track) {
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
        // The copy is bounded by the window (`MAX_EVENTS`); the dedup lookup is O(1).
        let seen = self.seen.entry(track).or_default();
        if seen.contains(&event) {
            return; // reread after a backward seek: preserve one source event and its budget
        }
        self.revision = self.revision.wrapping_add(1);
        let mut kept: Vec<Event> = Vec::with_capacity(events.len() + 1);
        for e in events.iter() {
            if e.start_ms.saturating_add(e.duration_ms) >= floor_ms {
                kept.push(e.clone());
            } else {
                seen.remove(e);
            }
        }
        let frontier = event.start_ms;
        seen.insert(event.clone());
        kept.push(event);
        // Dense karaoke can carry many events at one timestamp. Bound BYTES as well as count.
        // Each individual native source also fits the renderer's 8 MiB script bound.
        let budget = self.event_budget(track, header.len());
        let items: Vec<(i64, i64, usize)> = kept
            .iter()
            .map(|e| (e.start_ms, e.start_ms.saturating_add(e.duration_ms), e.payload.len()))
            .collect();
        let retire = retire_to_fit(&items, frontier, budget);
        if retire.contains(&true) {
            // Surviving events keep their arrival order.
            let seen = self.seen.entry(track).or_default();
            let mut i = 0;
            kept.retain(|e| {
                let drop = retire[i];
                i += 1;
                if drop {
                    seen.remove(e);
                }
                !drop
            });
        }
        let events = kept;
        let source = Source {
            id: old.id,
            revision: old.revision.wrapping_add(1),
            origin: old.origin,
            content: Content::Embedded {
                header: header.clone(),
                events: events.into(),
                fonts: fonts.clone(),
            },
        };
        self.tracks.insert(track, Arc::new(source));
    }

    /// The bytes of events a track may hold: the selected track gets the whole script's worth, the
    /// others share one pool.
    fn event_budget(&self, track: i32, header_len: usize) -> usize {
        let cap = MAX_EVENT_BYTES.saturating_sub(header_len);
        if self.selected_track == Some(track) {
            cap
        } else {
            let others = self.tracks.len() - usize::from(self.selected_track.is_some_and(|t| self.tracks.contains_key(&t)));
            cap.min(OTHER_TRACKS_EVENT_BYTES / others.max(1))
        }
    }

    /// One cue of a text track, in the track's own clock (ms). Returns the number of characters it
    /// shows when it is new, `None` when it was dropped (stale generation, an empty or
    /// non-positive span, nothing visible) or already held.
    ///
    /// The cue is kept as received in `raw`; the track's `Source.events` become the newest-wins
    /// view of all raw cues (plan §1.5). A re-delivered packet finds its raw key (`identity`, see
    /// [`packet_identity`]) and returns before anything is touched, so a seek re-read or a demux
    /// reopen never bumps the revision. A cue the budget retires on arrival is not new either.
    #[cfg(test)]
    fn push_text(
        &mut self,
        generation: u64,
        track: i32,
        start_ms: i64,
        end_ms: i64,
        text: &str,
        floor_ms: i64,
    ) -> Option<usize> {
        self.push_text_keyed(generation, track, start_ms, end_ms, text, packet_identity(text.as_bytes()), floor_ms)
    }

    #[allow(clippy::too_many_arguments)]
    fn push_text_keyed(
        &mut self,
        generation: u64,
        track: i32,
        start_ms: i64,
        end_ms: i64,
        text: &str,
        identity: u64,
        floor_ms: i64,
    ) -> Option<usize> {
        if generation != self.generation || end_ms <= start_ms || text.len() > MAX_PACKET_BYTES {
            return None;
        }
        let old = self.tracks.get(&track)?;
        let Content::Embedded { header, events: _, fonts } = &old.content else {
            return None;
        };
        let held = self.text.get_mut(&track)?;
        let key: RawKey = (start_ms, end_ms, identity);
        if held.raw.contains_key(&key) {
            return None; // reread after a seek or a demux reopen: one event, the same ReadOrder
        }
        let mut converted = text_ass::convert(text, held.dialect)?;
        // A zero inside an event makes the whole track invalid (`ass::validate`); a cue is not
        // worth the track.
        if converted.body.contains('\0') {
            converted.body.retain(|c| c != '\0');
            if converted.body.is_empty() {
                return None;
            }
        }
        let readorder = held.next_readorder;
        let payload: Arc<[u8]> = text_ass::event_payload(readorder, converted.align, &converted.body)
            .into_bytes()
            .into();
        if payload.len() > MAX_PACKET_BYTES {
            return None;
        }
        held.next_readorder = readorder.checked_add(1)?;
        self.revision = self.revision.wrapping_add(1);
        let shown = text_ass::visible_chars(&converted.body);
        let before_prune = held.raw.len();
        held.raw.retain(|(_, end, _), _| *end >= floor_ms);
        let pruned = held.raw.len() != before_prune;
        let held_before = held.raw.len();
        held.raw.insert(key, RawCue { readorder, align: converted.align, payload });
        let budget = self.event_budget(track, header.len());
        let held = self.text.get_mut(&track)?;
        let items: Vec<(i64, i64, usize)> =
            held.raw.iter().map(|((start, end, _), cue)| (*start, *end, cue.payload.len())).collect();
        let retire = retire_to_fit(&items, start_ms, budget);
        if retire.contains(&true) {
            let mut i = 0;
            held.raw.retain(|_, _| {
                i += 1;
                !retire[i - 1]
            });
        }
        // Retired on arrival (a backward seek's re-read can be the earliest-ending cue): the
        // track is as it was, so nothing changed and nothing is new to report.
        if !held.raw.contains_key(&key) && held.raw.len() == held_before && !pruned {
            return None;
        }
        let source = Source {
            id: old.id,
            revision: old.revision.wrapping_add(1),
            origin: old.origin,
            content: Content::Embedded { header: header.clone(), events: held.view(), fonts: fonts.clone() },
        };
        self.tracks.insert(track, Arc::new(source));
        Some(shown)
    }

    fn seek(&mut self, generation: u64) {
        if generation != self.generation {
            return;
        }
        self.revision = self.revision.wrapping_add(1);
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
                origin: source.origin,
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

pub fn begin(media_key: &str, headers: Vec<(i32, TrackHeader)>, fonts: Vec<Font>) -> u64 {
    // The picks are read before the store lock is taken, so no preference lock nests inside it.
    let style = TextStyle::current();
    store().begin_styled(media_key, headers, fonts, style)
}

/// The owned source inventory. Repreparing after a concurrent store change
/// reuses these facts without rereading FFmpeg's released metadata.
pub(crate) struct SourceInventory {
    media_key: String,
    headers: Vec<(i32, TrackHeader)>,
    fonts: Vec<Font>,
}

impl SourceInventory {
    pub(crate) fn new(media_key: &str, headers: Vec<(i32, TrackHeader)>, fonts: Vec<Font>) -> Self {
        Self { media_key: media_key.to_owned(), headers, fonts }
    }
}

pub(crate) fn begin_inventory(inventory: SourceInventory) -> u64 {
    begin(&inventory.media_key, inventory.headers, inventory.fonts)
}

pub(crate) struct PreparedBegin {
    expected_revision: u64,
    next: Store,
    generation: u64,
}

/// Clone the bounded cue index and merge the next ASS source off the native
/// retirement barrier. The source's payloads and fonts remain shared Arcs.
pub(crate) fn prepare_begin(inventory: &SourceInventory) -> PreparedBegin {
    let style = TextStyle::current();
    let mut next = store().clone();
    let expected_revision = next.revision;
    let generation = next.begin_styled(
        &inventory.media_key, inventory.headers.clone(), inventory.fonts.clone(), style,
    );
    PreparedBegin { expected_revision, next, generation }
}

/// Only the pointer/map swap runs under the native epoch barrier. If cues,
/// style or a reset changed during preparation, the caller must reprepare.
pub(crate) fn commit_prepared(prepared: PreparedBegin) -> Result<(u64, Store), PreparedBegin> {
    let mut active = store();
    if active.revision != prepared.expected_revision { return Err(prepared); }
    let old = std::mem::replace(&mut *active, prepared.next);
    Ok((prepared.generation, old))
}

/// One cue of a text track (`text` is UTF-8, in the track's dialect), `start_ns`/`end_ns` in the
/// clock of the subtitle stream; `identity` is the [`packet_identity`] of the packet before its
/// charset step. The cue is converted to ASS and joins the track's source; see
/// [`Store::push_text`]. Logs `sub cue [a..bms] len=N` for the selected track when the cue is new:
/// the cue's length, never its text (subtitle text is viewing content and the event log is shared
/// in issue threads), which `tests/run.py` reads to see that a text cue arrived.
pub fn push_text(generation: u64, track: i32, start_ns: i64, end_ns: i64, text: &str, identity: u64, retention: plx_playback_contract::subtitle::CaptionRetention) {
    let floor_ms = super::subtitle_floor_ns(retention) / 1_000_000;
    let (start_ms, end_ms) = (start_ns / 1_000_000, end_ns / 1_000_000);
    let selected = Some(super::desired_sub_idx()).filter(|&t| t >= 0);
    let shown = {
        let mut store = store();
        if store.selected_track != selected {
            store.selected_track = selected;
            store.revision = store.revision.wrapping_add(1);
        }
        store.push_text_keyed(generation, track, start_ms, end_ms, text, identity, floor_ms)
    };
    if let Some(len) = shown.filter(|_| selected == Some(track)) {
        super::log(&format!("sub cue [{start_ms}..{end_ms}ms] len={len}"));
    }
}
pub fn push(generation: u64, track: i32, start_ns: i64, end_ns: i64, payload: &[u8], retention: plx_playback_contract::subtitle::CaptionRetention) {
    if payload.len() > MAX_PACKET_BYTES {
        return;
    }
    let payload = trim_terminators(payload);
    if payload.is_empty() || payload.contains(&0) {
        return;
    }
    let mut store = store();
    let selected = Some(super::desired_sub_idx()).filter(|&t| t >= 0);
    if store.selected_track != selected {
        store.selected_track = selected;
        store.revision = store.revision.wrapping_add(1);
    }
    store.push(
        generation,
        track,
        Event {
            start_ms: start_ns / 1_000_000,
            duration_ms: end_ns.saturating_sub(start_ns) / 1_000_000,
            payload: payload.into(),
        },
        super::subtitle_floor_ns(retention) / 1_000_000,
    );
}
pub fn seek(generation: u64) {
    store().seek(generation);
}
#[cfg(any(test, feature = "test-support"))]
pub fn reset() {
    store().reset();
}
pub fn font_snapshot() -> (u64, Arc<[Font]>) {
    let mut store = store();
    let fonts = store.fonts.get_or_insert_with(|| Arc::from([])).clone();
    (store.generation, fonts)
}
pub fn selected(track: i32) -> Option<Arc<Source>> {
    let style = TextStyle::current();
    store().selected_styled(track, style)
}

/// Can the viewer's colour pick reach embedded track `track` ([`Store::takes_colour`])? `None` while the
/// store holds no such track.
pub fn takes_colour(track: i32) -> Option<bool> {
    store().takes_colour(track)
}

/// Interpolate the sparse native clock only while it is advancing. Limit extrapolation to
/// one normal callback interval plus jitter; a stalled decoder must not animate indefinitely.
#[derive(Default)]
pub struct Clock {
    anchor: Option<(i64, u32)>,
    last_ms: i64,
    running: bool,
}
impl Clock {
    pub fn sample(&mut self, position_ns: i64, tick_ms: u32, running: bool) -> i64 {
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
    use plx_play_feed::shared::FEED_SHARED;
    use super::*;

    fn fixture_retention() -> plx_playback_contract::subtitle::CaptionRetention {
        plx_playback_contract::subtitle::CaptionRetention { playhead_ns: None, seek_target_ns: None }
    }

    #[test]
    fn prepared_reopen_rebases_after_a_concurrent_cue() {
        let _serial = plx_base::testlock::serial();
        reset();
        let header = b"[Script Info]\n".to_vec();
        let generation = begin("fixture", vec![(0, TrackHeader::Authored(header.clone()))], Vec::new());
        let inventory = SourceInventory::new(
            "fixture", vec![(0, TrackHeader::Authored(header))], Vec::new(),
        );
        let stale = prepare_begin(&inventory);
        push(generation, 0, 1_000_000_000, 3_000_000_000,
            b"0,0,Default,,0,0,0,,SIGN", fixture_retention());
        let stale = match commit_prepared(stale) {
            Ok(_) => panic!("the prepared reopen erased a cue arriving after its snapshot"),
            Err(stale) => stale,
        };
        drop(stale);
        let fresh = prepare_begin(&inventory);
        let (next_generation, old) = match commit_prepared(fresh) {
            Ok(committed) => committed,
            Err(_) => panic!("no concurrent mutation occurred during the retry"),
        };
        drop(old);
        assert_ne!(generation, next_generation);
        let selected = selected(0).expect("the authored track survives its reopen");
        let Content::Embedded { events, .. } = &selected.content else { panic!("embedded source") };
        assert_eq!(events.len(), 1, "the cue received during preparation is retained");
        reset();
    }

    #[test]
    fn begin_keeps_empty_headers_but_bounds_raw_bytes_before_trimming() {
        let mut s = Store::default();
        s.begin(
            "fixture",
            vec![(0, TrackHeader::Authored(vec![])), (1, TrackHeader::Authored(vec![0; 3])), (2, TrackHeader::Authored(vec![0; MAX_HEADER_BYTES + 1]))],
            vec![],
        );
        for track in [0, 1] {
            let source = s.selected(track).unwrap();
            let Content::Embedded { header, .. } = &source.content else {
                panic!()
            };
            assert!(header.is_empty(), "validation must receive the empty header");
        }
        assert!(s.selected(2).is_none());
    }

    #[test]
    fn terminators_are_trimmed_from_headers_and_packets_but_a_zero_inside_is_not() {
        let _guard = plx_base::testlock::serial();
        FEED_SHARED.playpos_ns.store(0, std::sync::atomic::Ordering::Relaxed);
        let generation = begin(
            "fixture",
            vec![
                (0, TrackHeader::Authored(b"[Script Info]\n\0\0".to_vec())),
                (1, TrackHeader::Authored(b"[Script Info]\0\n".to_vec())),
                (2, TrackHeader::Authored(vec![])),
                (3, TrackHeader::Authored(vec![0; 4])),
            ],
            vec![],
        );
        let caption = b"0,0,Default,,0,0,0,,hello";
        let second = b"1,0,Default,,0,0,0,,again";
        let terminated = [caption.as_slice(), b"\0\0"].concat();
        let hidden_zero = b"2,0,Default,,0,0,0,,he\0llo";
        for payload in [
            terminated.as_slice(),
            b"".as_slice(),
            b"\0\0\0".as_slice(),
            hidden_zero.as_slice(),
            second.as_slice(),
        ] {
            push(generation, 0, 1_000_000_000, 3_000_000_000, payload, fixture_retention());
        }

        let track = |n| selected(n).unwrap();
        let source = track(0);
        let Content::Embedded { header, events, .. } = &source.content else {
            panic!()
        };
        assert_eq!(header.as_ref(), b"[Script Info]\n", "trailing zeros leave the header");
        let payloads: Vec<&[u8]> = events.iter().map(|e| &*e.payload).collect();
        assert_eq!(
            payloads,
            [caption.as_slice(), second.as_slice()],
            "a terminated packet is kept without its zeros; empty, all-zero and \
             inner-zero packets are dropped without costing the valid ones"
        );
        assert_eq!(ass::validate(&source), Ok(()));

        let source = track(1);
        let Content::Embedded { header, .. } = &source.content else {
            panic!()
        };
        assert_eq!(header.as_ref(), b"[Script Info]\0\n", "a zero inside is not trimmed");
        assert_eq!(ass::validate(&source), Err(ass::Fault::InvalidTrack));
        for empty in [2, 3] {
            let source = track(empty);
            let Content::Embedded { header, .. } = &source.content else {
                panic!()
            };
            assert!(header.is_empty());
            assert_eq!(ass::validate(&source), Err(ass::Fault::InvalidTrack));
        }

        // `push` never lets an inner zero in, but a track built any other way still fails it.
        let forged = Source {
            id: ass::next_source_id(),
            revision: 0,
            origin: Origin::Authored,
            content: Content::Embedded {
                header: Arc::from(&b"[Script Info]\n"[..]),
                events: Arc::from([Event {
                    start_ms: 0,
                    duration_ms: 1000,
                    payload: hidden_zero.as_slice().into(),
                }]),
                fonts: Arc::from([]),
            },
        };
        assert_eq!(ass::validate(&forged), Err(ass::Fault::InvalidEvents));
        reset();
    }

    #[test]
    fn begin_normalizes_headers_before_reopen_identity_checks() {
        let mut s = Store::default();
        let generation = s.begin("fixture", vec![(0, TrackHeader::Authored(b"header\0\0".to_vec()))], vec![]);
        s.push(generation, 0, event("caption", 1000), 0);
        s.retain_for_reload();
        s.begin("fixture", vec![(0, TrackHeader::Authored(b"header".to_vec()))], vec![]);
        let source = s.selected(0).unwrap();
        let Content::Embedded { header, events, .. } = &source.content else {
            panic!()
        };
        assert_eq!(header.as_ref(), b"header");
        assert_eq!(events.as_ref(), &[event("caption", 1000)]);
    }

    #[test]
    fn resetting_another_playback_owner_cannot_retire_the_live_subtitles() {
        let _guard = plx_base::testlock::serial();
        begin("fixture", vec![(0, TrackHeader::Authored(b"header".to_vec()))], vec![]);
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
        let generation = s.begin("fixture", vec![(0, TrackHeader::Authored(b"header".to_vec()))], vec![]);
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
        let generation = s.begin("fixture", vec![(0, TrackHeader::Authored(b"header".to_vec()))], vec![]);
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
            let generation = s.begin("fixture", vec![(0, TrackHeader::Authored(b"header".to_vec()))], vec![]);
            s.push(generation, 0, sign.clone(), 0);
            s.tracks[&0].id
        };
        shared.reset_session_for_reload();
        let mut s = shared.ass_sources.lock().unwrap();
        s.begin("fixture", vec![(0, TrackHeader::Authored(b"header".to_vec()))], vec![]);
        let source = &s.tracks[&0];
        assert_ne!(source.id, before, "retire the old raster across a fresh native Load");
        let Content::Embedded { events, .. } = &source.content else { panic!() };
        assert_eq!(events.as_ref(), &[sign], "a reload-based seek must preserve already-read media facts");
    }
    #[test]
    fn demux_reopen_keeps_known_events_and_rejects_the_old_producer() {
        let mut s = Store::new();
        let old_generation = s.begin("part-A", vec![(0, TrackHeader::Authored(b"header".to_vec()))], vec![]);
        let cue = event("known", 1000);
        s.push(old_generation, 0, cue.clone(), 0);
        let old_id = s.tracks[&0].id;
        let generation = s.begin("part-A", vec![(0, TrackHeader::Authored(b"header".to_vec()))], vec![]);
        assert_ne!(old_generation, generation);
        assert_ne!(old_id, s.tracks[&0].id);
        s.push(old_generation, 0, event("stale producer", 1000), 0);
        s.push(generation, 0, cue.clone(), 0); // a reread still deduplicates
        let Content::Embedded { events, .. } = &s.tracks[&0].content else { panic!() };
        assert_eq!(events.as_ref(), &[cue]);
    }

    #[test]
    fn cached_reload_sources_stay_hidden_until_the_demuxer_revalidates_them() {
        let mut s = Store::new();
        let generation = s.begin("part-A", vec![(0, TrackHeader::Authored(b"header".to_vec()))], vec![]);
        s.push(generation, 0, event("known", 1000), 0);
        assert!(s.selected(0).is_some());
        s.retain_for_reload();
        assert!(s.selected(0).is_none());
        assert!(s.fonts.is_none());
        s.push(generation, 0, event("late", 1000), 0);
        s.begin("part-A", vec![(0, TrackHeader::Authored(b"header".to_vec()))], vec![]);
        let source = s.selected(0).unwrap();
        let Content::Embedded { events, .. } = &source.content else { panic!() };
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].payload.as_ref(), b"known");
    }

    #[test]
    fn new_playback_or_changed_file_inventory_cannot_inherit_subtitle_events() {
        for (fresh, key, headers) in [
            (true, "part-A", vec![(0, TrackHeader::Authored(b"header".to_vec()))]),
            (false, "part-B", vec![(0, TrackHeader::Authored(b"header".to_vec()))]),
            (false, "part-A", vec![(0, TrackHeader::Authored(b"different style sheet".to_vec()))]),
            (false, "part-A", vec![(0, TrackHeader::Authored(b"header".to_vec())), (1, TrackHeader::Authored(b"new track".to_vec()))]),
        ] {
            let shared = crate::player::Shared::new();
            {
                let mut s = shared.ass_sources.lock().unwrap();
                let generation = s.begin("part-A", vec![(0, TrackHeader::Authored(b"header".to_vec()))], vec![]);
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
        let g = s.begin("fixture", vec![(0, TrackHeader::Authored(b"[V4+ Styles]\nStyle: Sign".to_vec()))], vec![]);
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
            vec![(0, TrackHeader::Authored(b"header".to_vec()))],
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
    fn embedded(s: &Store, track: i32) -> Vec<Event> {
        let Content::Embedded { events, .. } = &s.tracks[&track].content else { panic!() };
        events.to_vec()
    }

    #[test]
    fn the_seventieth_track_renders_styled_when_selected() {
        let mut s = Store::new();
        let headers = (0..70).map(|t| (t, TrackHeader::Authored(format!("header {t}").into_bytes()))).collect();
        let g = s.begin("fixture", headers, vec![]);
        for t in [0, 63, 64, 69] {
            s.push(g, t, event(&format!("track {t}"), 1000), 0);
        }
        let source = s.selected(69).expect("track 70 has a source");
        let Content::Embedded { header, events, .. } = &source.content else {
            panic!()
        };
        assert_eq!(header.as_ref(), b"header 69");
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].payload.as_ref(), b"track 69");
        assert_eq!(s.tracks.len(), 70, "headers of every track are kept");
    }

    #[test]
    fn two_hundred_fonts_are_all_available_to_the_renderer() {
        let mut s = Store::new();
        let fonts = (0..200)
            .map(|i| Font { name: format!("f{i}.ttf"), data: Arc::from(vec![i as u8; 2048]) })
            .collect();
        s.begin("fixture", vec![(0, TrackHeader::Authored(b"header".to_vec()))], fonts);
        let held = s.fonts.clone().unwrap();
        assert_eq!(held.len(), 200);
        assert!(held.iter().any(|f| f.name == "f199.ttf"), "a font late in the list is found");
    }

    /// A demux reopen of the same file hands `begin` a fresh copy of every font. The tracks keep the
    /// list they already hold, so the renderer's same-fonts check (`Arc` equality, a pointer
    /// compare when it is the same `Arc`) neither reads every byte nor opens a new renderer; a
    /// different file, or a list that differs in a name or a size, is taken as it comes.
    #[test]
    fn a_reopen_of_the_same_file_keeps_the_font_list_it_already_held() {
        fn held(s: &mut Store) -> Arc<[Font]> {
            match &s.selected(0).unwrap().content {
                Content::Embedded { fonts, .. } | Content::Script { fonts, .. } => fonts.clone(),
            }
        }
        let font = |name: &str, len: usize| Font { name: name.into(), data: Arc::from(vec![1u8; len]) };
        let headers = || vec![(0, TrackHeader::Authored(b"header".to_vec()))];
        let mut s = Store::new();
        s.begin("fixture", headers(), vec![font("a.ttf", 4096), font("b.ttf", 8192)]);
        let first = held(&mut s);
        s.retain_for_reload();
        s.begin("fixture", headers(), vec![font("a.ttf", 4096), font("b.ttf", 8192)]);
        assert!(Arc::ptr_eq(&first, &held(&mut s)), "same file, same fonts: the held list is kept");
        s.begin("fixture", headers(), vec![font("a.ttf", 4096), font("b.ttf", 9000)]);
        let changed = held(&mut s);
        assert!(!Arc::ptr_eq(&first, &changed), "a font of another size is another list");
        assert_eq!(changed[1].data.len(), 9000);
        s.begin("other file", headers(), vec![font("b.ttf", 9000)]);
        assert_eq!(held(&mut s).len(), 1, "another file starts from its own fonts");
    }

    /// Issue #1328: an anime MKV attaches 17 fonts (83 MB). The five the opening's karaoke styles
    /// name sat after the first seven had spent a 32 MiB cap (since removed; twelve were held in
    /// all), so libass never saw them and those lines fell back to the default face. A cap on
    /// content the file legitimately carries is a bug, so the store must hold every attachment, in
    /// order; which of them libass is given is the renderer's choice (`ass::font_names`).
    /// Sizes and names are the report's (`tests/fixtures/make_ass_stress.py`, `fonts-over-budget`).
    #[test]
    fn every_attached_font_reaches_the_renderer_issue_1328() {
        const ATTACHED: [(&str, usize); 17] = [
            ("AP-OTF-KizaKinryouStd-Bol.otf", 6_220_620),
            ("Comiccity.ttf", 32_904),
            ("DFMaruGothicPro6N-W9.otf", 11_229_304),
            ("FOT-Accordion.otf", 37_300),
            ("FOT-DotGothic12 Std M.ttf", 43_840),
            ("FOT-MatissePro-B.otf", 9_883_924),
            ("FOT-PopFury Std B.otf", 4_808_652),
            ("FOT-TelopMinPro-D.otf", 7_477_668),
            ("FOT-TsukuAOldMinPr6N-M.otf", 13_188_132),
            ("HOT-KarakusaStd-R.otf", 6_102_048),
            ("KizaJJK-B.ttf", 9_705_124),
            ("KizaKinryouStdJJK-B.ttf", 13_715_688),
            ("LEHN084.TTF", 47_816),
            ("MastroSans-SemiBold.ttf", 245_796),
            ("MastroSans-SemiBoldItalic.ttf", 262_448),
            ("Roboto-Regular.ttf", 146_004),
            ("stylus.ttf", 32_972),
        ];
        let total: usize = ATTACHED.iter().map(|&(_, len)| len).sum();
        assert!(total > 32 * 1024 * 1024, "the report's file is over the old 32 MiB cap: {total} bytes");
        let mut s = Store::new();
        let fonts = ATTACHED
            .iter()
            .map(|&(name, len)| Font { name: name.into(), data: Arc::from(vec![1u8; len]) })
            .collect();
        s.begin("fixture", vec![(0, TrackHeader::Authored(b"header".to_vec()))], fonts);
        let held = s.fonts.clone().unwrap();
        let names: Vec<_> = held.iter().map(|f| f.name.as_str()).collect();
        let wanted: Vec<_> = ATTACHED.iter().map(|&(name, _)| name).collect();
        assert_eq!(names, wanted, "every attached font is held, in container order");
    }

    fn timed(n: i64) -> Event {
        Event {
            start_ms: n * 1000,
            duration_ms: 2000,
            payload: format!("{n},0,Default,,0,0,0,,line {n}").into_bytes().into(),
        }
    }

    #[test]
    fn a_backward_seek_after_a_deep_read_keeps_the_events_at_the_target() {
        let mut s = Store::new();
        let g = s.begin("fixture", vec![(0, TrackHeader::Authored(b"header".to_vec()))], vec![]);
        // A typeset-heavy file read past the window: the window holds the latest 8 192, and every
        // event the file has to give is behind its first one. (A push copies and ranks the whole
        // window, so reading further than this only repeats the same eviction at a quadratic cost;
        // the 30 000 events this once read held the identical 8 192 latest at the seek.)
        let read = MAX_EVENTS as i64 + 300;
        for n in 0..read {
            s.push(g, 0, timed(n), 0);
        }
        assert_eq!(embedded(&s, 0).len(), MAX_EVENTS, "the window is full");
        // The viewer rewinds to event 100, more than `STALE_AHEAD_MS` behind everything the window
        // holds (the floor follows the rebased clock).
        s.seek(g);
        let floor_ms = 100_000 - 62_000;
        for n in 100..200 {
            s.push(g, 0, timed(n), floor_ms);
        }
        let held = embedded(&s, 0);
        for n in 100..200 {
            assert!(held.contains(&timed(n)), "event {n} at the seek target was lost");
        }
        assert!(held.len() <= 8192);
    }

    #[test]
    fn duplicates_after_a_seek_are_not_doubled_and_the_index_tracks_evictions() {
        let mut s = Store::new();
        let g = s.begin("fixture", vec![(0, TrackHeader::Authored(b"header".to_vec()))], vec![]);
        for n in 0..20_000 {
            s.push(g, 0, timed(n), 0);
        }
        s.seek(g);
        for n in 19_000..20_000 {
            s.push(g, 0, timed(n), 0);
        }
        let held = embedded(&s, 0);
        let mut unique = held.clone();
        unique.sort_by_key(|e| e.start_ms);
        unique.dedup();
        assert_eq!(unique.len(), held.len(), "a reread must not double an event");
        assert_eq!(s.seen[&0].len(), held.len(), "the dedup index mirrors the held events");
    }

    #[test]
    fn held_event_bytes_stay_within_the_budgets() {
        let mut s = Store::new();
        let headers = (0..4).map(|t| (t, TrackHeader::Authored(b"header".to_vec()))).collect();
        let g = s.begin("fixture", headers, vec![]);
        let heavy = |n: i64| Event {
            start_ms: n,
            duration_ms: 100_000_000,
            payload: format!("{n},{}", "x".repeat(200_000)).into_bytes().into(),
        };
        s.selected_track = Some(2);
        for n in 0..120 {
            for t in 0..4 {
                s.push(g, t, heavy(n), 0);
            }
        }
        let bytes = |t| embedded(&s, t).iter().map(|e| e.payload.len()).sum::<usize>();
        assert!(bytes(2) <= MAX_EVENT_BYTES - 6, "selected track within the script bound");
        assert!(bytes(2) > 7 * 1024 * 1024, "the selected track gets the full budget");
        let others: usize = [0, 1, 3].iter().map(|&t| bytes(t)).sum();
        assert!(others <= 8 * 1024 * 1024, "unselected tracks share one pool");
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

    // ---- Source.origin and TrackHeader::{Authored, Text(dialect)} (plan §1.5) ----

    #[test]
    fn a_text_track_is_a_text_origin_source_with_the_synthesized_header() {
        let mut s = Store::default();
        s.begin(
            "fixture",
            vec![(0, TrackHeader::Authored(b"[Script Info]".to_vec())), (1, TrackHeader::Text(TextDialect::Srt))],
            vec![],
        );
        let authored = s.selected(0).unwrap();
        assert_eq!(authored.origin, Origin::Authored);
        let text = s.selected(1).unwrap();
        assert!(text.origin.is_text());
        let Content::Embedded { header, events, .. } = &text.content else { panic!() };
        let header = std::str::from_utf8(header).unwrap();
        assert!(header.starts_with("[Script Info]") && header.contains("PlayResX: 1920"));
        assert!(header.contains("Style: An8,"), "one style per numpad alignment");
        assert!(events.is_empty());
    }

    #[test]
    fn origin_survives_a_push_and_a_seek() {
        let mut s = Store::default();
        let generation = s.begin(
            "fixture",
            vec![(0, TrackHeader::Authored(b"header".to_vec())), (1, TrackHeader::Text(TextDialect::WebVtt))],
            vec![],
        );
        s.push(generation, 0, event("authored", 1000), 0);
        s.push_text(generation, 1, 1000, 2000, "text", 0);
        s.seek(generation);
        assert_eq!(s.selected(0).unwrap().origin, Origin::Authored);
        let text = s.selected(1).unwrap();
        assert!(text.origin.is_text());
        let Content::Embedded { events, .. } = &text.content else { panic!() };
        assert_eq!(events.len(), 1);
    }

    #[test]
    fn a_cue_redelivered_after_a_same_file_reopen_is_not_appended_twice() {
        let mut s = Store::default();
        let generation = s.begin("fixture", vec![(0, TrackHeader::Text(TextDialect::Srt))], vec![]);
        s.push_text(generation, 0, 1000, 2000, "cue", 0);
        s.retain_for_reload();
        let generation = s.begin("fixture", vec![(0, TrackHeader::Text(TextDialect::Srt))], vec![]);
        s.push_text(generation, 0, 1000, 2000, "cue", 0);
        let source = s.selected(0).unwrap();
        let Content::Embedded { events, .. } = &source.content else { panic!() };
        assert_eq!(events.len(), 1);
    }

    #[test]
    fn a_text_origin_carries_the_picks_its_header_was_built_for() {
        let mut s = Store::default();
        s.begin("fixture", vec![(0, TrackHeader::Text(TextDialect::Srt))], vec![]);
        assert_eq!(s.selected(0).unwrap().origin, Origin::Text(TextStyle::current()));
    }

    #[test]
    fn the_same_file_check_compares_a_text_track_by_dialect_not_by_header_bytes() {
        let held = |s: &mut Store| {
            let source = s.selected(0).unwrap();
            let Content::Embedded { events, .. } = &source.content else { panic!() };
            events.len()
        };
        let mut s = Store::default();
        let generation = s.begin("fixture", vec![(0, TrackHeader::Text(TextDialect::Srt))], vec![]);
        s.push_text(generation, 0, 1000, 2000, "cue", 0);
        assert_eq!(held(&mut s), 1);

        // The viewer changed the size meanwhile: the stored header is for the old picks. The
        // reopened demuxer registers the same dialect, so the file is the same and the cue stays.
        s.tracks.insert(
            0,
            Arc::new(Source {
                id: s.tracks[&0].id,
                revision: 1,
                origin: Origin::Text(TextStyle::current()),
                content: Content::Embedded {
                    header: Arc::from(&b"an older header"[..]),
                    events: Arc::from([event("cue", 1000)]),
                    fonts: Arc::from([]),
                },
            }),
        );
        s.retain_for_reload();
        s.begin("fixture", vec![(0, TrackHeader::Text(TextDialect::Srt))], vec![]);
        assert_eq!(held(&mut s), 1, "same dialect: the held cue is kept across the reopen");

        // Another dialect is another track, and so is an authored one.
        s.begin("fixture", vec![(0, TrackHeader::Text(TextDialect::WebVtt))], vec![]);
        assert_eq!(held(&mut s), 0, "a changed dialect is not the same file");
        let generation = s.generation;
        s.push_text(generation, 0, 1000, 2000, "cue", 0);
        s.begin("fixture", vec![(0, TrackHeader::Authored(b"header".to_vec()))], vec![]);
        assert_eq!(held(&mut s), 0, "a text track registered as authored is not the same file");
        assert_eq!(s.selected(0).unwrap().origin, Origin::Authored);
        assert!(s.text.is_empty(), "and the dialect is forgotten with it");
        let generation = s.generation;
        s.push(generation, 0, event("cue", 1000), 0);
        s.begin("fixture", vec![(0, TrackHeader::Text(TextDialect::Srt))], vec![]);
        assert_eq!(held(&mut s), 0, "an authored track registered as text is not the same file");
        assert!(s.selected(0).unwrap().origin.is_text());
    }

    #[test]
    fn a_different_file_forgets_its_text_tracks() {
        let mut s = Store::default();
        s.begin("a", vec![(0, TrackHeader::Text(TextDialect::Srt))], vec![]);
        assert_eq!(s.text.len(), 1);
        s.begin("b", Vec::new(), vec![]);
        assert!(s.text.is_empty());
    }

    #[test]
    fn the_header_size_bound_is_for_a_files_own_header_not_the_synthesized_one() {
        let mut s = Store::default();
        s.begin(
            "fixture",
            vec![
                (0, TrackHeader::Authored(vec![b'x'; MAX_HEADER_BYTES + 1])),
                (1, TrackHeader::Text(TextDialect::Plain)),
            ],
            vec![],
        );
        assert!(s.selected(0).is_none(), "an oversized file header is refused");
        assert!(s.selected(1).is_some());
    }

    // ---- push_text: raw cues, ReadOrder, the newest-wins view (plan §1.5) ----

    /// `(start_ms, duration_ms, ReadOrder, Style, body)` of every event of the track, in order.
    fn view(s: &mut Store, track: i32) -> Vec<(i64, i64, u32, String, String)> {
        let source = s.selected(track).unwrap();
        let Content::Embedded { events, .. } = &source.content else { panic!() };
        events
            .iter()
            .map(|e| {
                let packet = std::str::from_utf8(&e.payload).unwrap();
                let f: Vec<&str> = packet.splitn(9, ',').collect();
                (e.start_ms, e.duration_ms, f[0].parse().unwrap(), f[2].to_owned(), f[8].to_owned())
            })
            .collect()
    }

    fn revision(s: &mut Store, track: i32) -> u64 {
        s.selected(track).unwrap().revision
    }

    fn text_store(media_key: &str) -> (Store, u64) {
        let mut s = Store::default();
        let generation = s.begin(media_key, vec![(0, TrackHeader::Text(TextDialect::Srt))], vec![]);
        (s, generation)
    }

    #[test]
    fn a_pushed_cue_becomes_an_ass_event_in_its_alignment_style() {
        let (mut s, g) = text_store("f");
        assert_eq!(s.push_text(g, 0, 1000, 3000, "Hello <i>there</i>\nbye", 0), Some(15));
        assert_eq!(s.push_text(g, 0, 5000, 6000, "{\\an8}Sign", 0), Some(4));
        assert_eq!(
            view(&mut s, 0),
            [
                (1000, 2000, 0, "Default".into(), "Hello {\\i1}there{\\i0}\\Nbye".into()),
                (5000, 1000, 1, "An8".into(), "Sign".into()),
            ]
        );
        assert_eq!(ass::validate(&s.selected(0).unwrap()), Ok(()));
    }

    #[test]
    fn a_cue_that_shows_nothing_or_spans_nothing_is_dropped_and_costs_no_readorder() {
        let (mut s, g) = text_store("f");
        let before = revision(&mut s, 0);
        for (start, end, text) in [(1000, 1000, "zero"), (2000, 1000, "negative"), (1000, 2000, "  \n "), (1000, 2000, "<i></i>")] {
            assert_eq!(s.push_text(g, 0, start, end, text, 0), None, "{text:?}");
        }
        assert_eq!(s.push_text(g + 1, 0, 1000, 2000, "stale generation", 0), None);
        assert_eq!(s.push_text(g, 7, 1000, 2000, "no such track", 0), None);
        assert_eq!(revision(&mut s, 0), before, "a dropped cue is not a change");
        s.push_text(g, 0, 1000, 2000, "real", 0);
        assert_eq!(view(&mut s, 0)[0].2, 0, "the first kept cue is ReadOrder 0");
    }

    #[test]
    fn a_reread_after_a_seek_is_found_by_its_raw_key_and_changes_nothing() {
        let (mut s, g) = text_store("f");
        s.push_text(g, 0, 1000, 2000, "one", 0);
        s.push_text(g, 0, 3000, 4000, "two", 0);
        let held = view(&mut s, 0);
        s.seek(g);
        let revision_after_seek = revision(&mut s, 0);
        for (start, end, text) in [(1000, 2000, "one"), (3000, 4000, "two")] {
            assert_eq!(s.push_text(g, 0, start, end, text, 0), None, "the demuxer read {text:?} again");
        }
        assert_eq!(view(&mut s, 0), held, "one event per cue, the same ReadOrders");
        assert_eq!(revision(&mut s, 0), revision_after_seek, "a re-read does not bump the revision");
        s.push_text(g, 0, 5000, 6000, "three", 0);
        assert_eq!(view(&mut s, 0)[2].2, 2, "the next new cue continues the count");
    }

    #[test]
    fn an_exact_duplicate_cue_collapses_into_one() {
        let (mut s, g) = text_store("f");
        assert!(s.push_text(g, 0, 1000, 2000, "twice", 0).is_some());
        assert!(s.push_text(g, 0, 1000, 2000, "twice", 0).is_none());
        assert_eq!(view(&mut s, 0).len(), 1);
        // Same span, other words: a different cue (one is dropped by the clip, see below).
        assert!(s.push_text(g, 0, 1000, 2000, "other", 0).is_some());
    }

    /// The reopen case (plan §1.5): the reopened demuxer seeks to a video cue, so it resends B and
    /// never A. A "naive" implementation keeps the raw cues beside `seen` and `reset()` clears
    /// both, so A would lose its raw entry and its ReadOrder, and B would be appended again.
    #[test]
    fn raw_cues_and_readorders_are_carried_across_a_same_file_reopen() {
        let (mut s, g) = text_store("file");
        s.push_text(g, 0, 0, 6000, "A", 0); // bottom, 0-6 s
        s.push_text(g, 0, 4000, 8000, "B", 0); // bottom, 4-8 s: clips A to 4 s
        let before = view(&mut s, 0);
        assert_eq!(before.iter().map(|v| (v.0, v.1, v.2)).collect::<Vec<_>>(), [(0, 4000, 0), (4000, 4000, 1)]);

        s.retain_for_reload();
        let g = s.begin("file", vec![(0, TrackHeader::Text(TextDialect::Srt))], vec![]);
        let revision_after_begin = revision(&mut s, 0);
        assert_eq!(s.push_text(g, 0, 4000, 8000, "B", 0), None, "B is found by its raw key");
        assert_eq!(revision(&mut s, 0), revision_after_begin, "the B re-delivery changes nothing");

        let after = view(&mut s, 0);
        assert_eq!(after, before, "one event per cue, the same ReadOrders, A held without being resent");
        let active = |at: i64| -> Vec<&str> {
            after.iter().filter(|v| v.0 <= at && at < v.0 + v.1).map(|v| v.4.as_str()).collect()
        };
        assert_eq!(active(3500), ["A"], "A is active at 3.5 s though the reopen never resent it");
        assert_eq!(active(4500), ["B"], "at 4.5 s only B, A clipped at 4 s");
        let next = s.push_text(g, 0, 9000, 10000, "C", 0);
        assert!(next.is_some());
        assert_eq!(view(&mut s, 0)[2].2, 2, "and the counter went on");
    }

    #[test]
    fn a_different_file_begin_clears_the_raw_cues_and_restarts_the_count() {
        let (mut s, g) = text_store("one");
        s.push_text(g, 0, 1000, 2000, "cue", 0);
        s.push_text(g, 0, 3000, 4000, "cue2", 0);
        s.retain_for_reload();
        let g = s.begin("two", vec![(0, TrackHeader::Text(TextDialect::Srt))], vec![]);
        assert!(view(&mut s, 0).is_empty(), "the other file's cues are gone");
        assert!(s.text[&0].raw.is_empty());
        s.push_text(g, 0, 1000, 2000, "cue", 0);
        assert_eq!(view(&mut s, 0)[0].2, 0, "ReadOrders are per track, per file");
    }

    #[test]
    fn a_dialect_that_changed_is_not_the_same_file_and_keeps_no_raw_cues() {
        let (mut s, g) = text_store("one");
        s.push_text(g, 0, 1000, 2000, "cue", 0);
        s.retain_for_reload();
        s.begin("one", vec![(0, TrackHeader::Text(TextDialect::WebVtt))], vec![]);
        assert!(view(&mut s, 0).is_empty());
        assert!(s.text[&0].raw.is_empty());
    }

    #[test]
    fn a_readorder_is_never_handed_out_twice_even_after_its_cue_is_pruned() {
        let (mut s, g) = text_store("f");
        s.push_text(g, 0, 1000, 2000, "old", 0); // ReadOrder 0
        // The playhead moved on: the floor is past the first cue, and pushing prunes it.
        s.push_text(g, 0, 90_000, 91_000, "new", 50_000); // ReadOrder 1
        assert_eq!(view(&mut s, 0).iter().map(|v| v.2).collect::<Vec<_>>(), [1], "the old cue is pruned");
        // A backward seek: the demuxer reads the old cue again. It is a new cue to the store.
        s.push_text(g, 0, 1000, 2000, "old", 50_000);
        let readorders: Vec<u32> = view(&mut s, 0).iter().map(|v| v.2).collect();
        assert_eq!(readorders, [2, 1], "the re-read cue got a fresh ReadOrder, not the pruned 0");
    }

    #[test]
    fn newest_wins_clips_a_cue_to_the_start_of_the_next_one_of_its_class() {
        let (mut s, g) = text_store("f");
        s.push_text(g, 0, 0, 6000, "first", 0);
        s.push_text(g, 0, 4000, 8000, "second", 0);
        s.push_text(g, 0, 5000, 7000, "{\\an8}sign", 0); // another class: not clipped, clips no one
        let v = view(&mut s, 0);
        assert_eq!(
            v.iter().map(|v| (v.0, v.1, v.4.as_str())).collect::<Vec<_>>(),
            [(0, 4000, "first"), (4000, 4000, "second"), (5000, 2000, "sign")]
        );
        // The raw keys are not clipped: the re-read of the first cue still dedups.
        assert_eq!(s.push_text(g, 0, 0, 6000, "first", 0), None);
    }

    #[test]
    fn a_cue_clipped_to_nothing_is_dropped_from_the_view_and_the_track_stays_valid() {
        let (mut s, g) = text_store("f");
        s.push_text(g, 0, 1000, 3000, "older", 0);
        s.push_text(g, 0, 1000, 4000, "newer", 0); // one start: the newer wins, `older` has no time left
        let v = view(&mut s, 0);
        assert_eq!(v.iter().map(|v| (v.0, v.1, v.4.as_str())).collect::<Vec<_>>(), [(1000, 3000, "newer")]);
        assert_eq!(ass::validate(&s.selected(0).unwrap()), Ok(()), "a zero duration would fault the whole track");
        assert_eq!(s.text[&0].raw.len(), 2, "but the raw cue is kept, so its re-read dedups");
        assert_eq!(s.push_text(g, 0, 1000, 3000, "older", 0), None);
    }

    #[test]
    fn a_text_cue_keeps_62_seconds_of_history_so_a_raised_delay_finds_it() {
        // The same floor `subtitle_floor_ns` applies to the other stores (mod.rs): the latest
        // delay (60 s) plus 2 s behind the playhead.
        let playhead_ns = 100 * 1_000_000_000;
        let floor_ms = super::super::subtitle_floor_for(playhead_ns, false, 0) / 1_000_000;
        assert!(floor_ms <= (100 - 62) * 1000, "the floor leaves at least 62 s of history: {floor_ms}");
        let (mut s, g) = text_store("f");
        let kept_end = floor_ms + 500;
        s.push_text(g, 0, kept_end - 1000, kept_end, "62 s back", 0);
        s.push_text(g, 0, 99_000, 100_000, "now", floor_ms);
        let texts: Vec<String> = view(&mut s, 0).into_iter().map(|v| v.4).collect();
        assert_eq!(texts, ["62 s back", "now"], "a cue still inside the floor is kept");
        // Past the floor it goes at the next push.
        s.push_text(g, 0, 101_000, 102_000, "later", floor_ms + 10_000);
        let texts: Vec<String> = view(&mut s, 0).into_iter().map(|v| v.4).collect();
        assert_eq!(texts, ["now", "later"]);
    }

    #[test]
    fn raw_cues_are_bounded_by_bytes_and_the_earliest_ending_go_first() {
        let (mut s, g) = text_store("f");
        // Each cue is the largest one a packet may be, so the selected track's budget is spent
        // after a few dozen of them.
        let big = "x".repeat(MAX_PACKET_BYTES - 64);
        let pushed = 40;
        for i in 0..pushed {
            s.push_text(g, 0, i * 3000, i * 3000 + 2000, &format!("{i} {big}"), 0);
        }
        let raw = &s.text[&0].raw;
        let bytes: usize = raw.values().map(|c| c.payload.len()).sum();
        assert!(bytes <= MAX_EVENT_BYTES, "{bytes} bytes held");
        assert!(raw.len() < pushed as usize, "the window is bounded");
        let first = raw.keys().next().unwrap().0;
        assert!(first > 0 && first % 3000 == 0, "the earliest-ending went first: {first}");
        assert_eq!(raw.keys().last().unwrap().0, (pushed - 1) * 3000, "and the newest cue is held");
        assert_eq!(s.text[&0].next_readorder as i64, pushed, "the count only went up");
        assert_eq!(view(&mut s, 0).len(), s.text[&0].raw.len());
    }

    /// A backward seek's re-read can be the earliest-ending cue the budget holds, so it is retired
    /// the moment it is inserted. It is not held, so it is not new: no revision, no `sub cue` line.
    #[test]
    fn a_cue_the_budget_retires_on_arrival_is_not_reported_as_new() {
        let (mut s, g) = text_store("f");
        let big = "x".repeat(MAX_PACKET_BYTES - 64);
        for i in 0..40 {
            s.push_text(g, 0, 1000 + i, 60_000 + i, &format!("{i} {big}"), 0);
        }
        let (held, revision_before) = (s.text[&0].raw.len(), revision(&mut s, 0));
        assert_eq!(s.push_text(g, 0, 500, 1500, &format!("early {big}"), 0), None);
        assert_eq!(s.text[&0].raw.len(), held, "it was not kept");
        assert_eq!(revision(&mut s, 0), revision_before, "and nothing changed");
    }

    /// The charset detector learns as it reads, so the same packet can decode to other text after
    /// a reopen. The dedup key is the packet, not the text, so the re-read is still found.
    #[test]
    fn a_reread_that_decodes_differently_is_still_the_same_cue() {
        let (mut s, g) = text_store("f");
        let packet = packet_identity(&[0xC4, 0xE3, 0xBA, 0xC3]);
        assert!(s.push_text_keyed(g, 0, 1000, 2000, "mis-detected", packet, 0).is_some());
        let held = view(&mut s, 0);
        assert_eq!(s.push_text_keyed(g, 0, 1000, 2000, "\u{4f60}\u{597d}", packet, 0), None);
        assert_eq!(view(&mut s, 0), held, "one event, the first reading");
        assert_ne!(packet_identity(b"a"), packet_identity(b"b"));
    }

    #[test]
    fn the_event_count_bound_retires_the_earliest_ending_then_the_far_ahead() {
        let items: Vec<(i64, i64, usize)> = (0..MAX_EVENTS as i64 + 3).map(|i| (i * 3000, i * 3000 + 2000, 10)).collect();
        let retire = retire_to_fit(&items, items.len() as i64 * 3000, usize::MAX);
        assert_eq!(retire.iter().filter(|r| **r).count(), 3);
        assert!(retire[..3].iter().all(|r| *r), "the earliest-ending went");
        // A cue far ahead of the packet just read is what a backward seek left behind: it goes first.
        let ahead = [(0, 1000, 10), (100_000, 101_000, 10), (1000, 2000, 10)];
        let retire = retire_to_fit(&ahead, 2000, 20);
        assert_eq!(retire, [false, true, false]);
    }

    #[test]
    fn a_zero_byte_in_a_cue_cannot_make_the_track_invalid() {
        let (mut s, g) = text_store("f");
        s.push_text(g, 0, 1000, 2000, "he\0llo", 0);
        s.push_text(g, 0, 3000, 4000, "\0\0", 0);
        assert_eq!(view(&mut s, 0).iter().map(|v| v.4.as_str()).collect::<Vec<_>>(), ["hello"]);
        assert_eq!(ass::validate(&s.selected(0).unwrap()), Ok(()));
    }

    #[test]
    fn the_authored_push_cannot_write_into_a_text_track() {
        let (mut s, g) = text_store("f");
        s.push(g, 0, event("0,0,Default,,0,0,0,,sneaked in", 1000), 0);
        assert!(view(&mut s, 0).is_empty(), "only push_text writes a text track");
        assert_eq!(s.push_text(g, 0, 1000, 2000, "cue", 0), Some(3));
        assert!(s.seen.get(&0).is_none_or(|seen| seen.is_empty()), "a text track has no `seen` entries");
    }

    #[test]
    fn an_authored_track_is_not_a_text_track_for_push_text() {
        let mut s = Store::default();
        let g = s.begin("f", vec![(0, TrackHeader::Authored(b"header".to_vec()))], vec![]);
        assert_eq!(s.push_text(g, 0, 1000, 2000, "cue", 0), None);
    }

    // ---- the colour pick on an authored track (`ass_ink`) ----

    /// An authored header with a white-primary `Default` and a blue `Sign` style.
    fn authored_header() -> Vec<u8> {
        let style = |name: &str, primary: &str| format!(
            "Style: {name},Arial,48,{primary},&H000000FF,&H00000000,&H80000000,0,0,0,0,100,100,0,0,1,2,1,2,10,10,10,1\n");
        format!(
            "[Script Info]\nScriptType: v4.00+\n\n[V4+ Styles]\nFormat: Name, Fontname, Fontsize, PrimaryColour, SecondaryColour, OutlineColour, BackColour, Bold, Italic, Underline, StrikeOut, ScaleX, ScaleY, Spacing, Angle, BorderStyle, Outline, Shadow, Alignment, MarginL, MarginR, MarginV, Encoding\n{}{}\n[Events]\n",
            style("Default", "&H00FFFFFF"), style("Sign", "&H00FF0000")
        ).into_bytes()
    }

    /// **An embedded ASS track takes the viewer's colour in its white styles** (an SRT converted to ASS is
    /// one): the renderer's header is the file's with `ass_ink::recolour` applied, rebuilt in place when the
    /// pick moves, while the store keeps the file's own bytes (the reopen identity) and the events.
    #[test]
    fn an_authored_track_takes_the_colour_pick_in_its_white_styles_and_keeps_the_files_own_bytes() {
        use plx_plex::plex::session::{SubtitlePosition, SubtitleSize, SubtitleTone};
        let file = authored_header();
        let white = TextStyle::default();
        let yellow = TextStyle { tone: SubtitleTone::Yellow, ..white };
        let header_of = |source: &Arc<Source>| match &source.content {
            Content::Embedded { header, .. } => header.to_vec(),
            Content::Script { .. } => panic!(),
        };
        let mut s = Store::default();
        let g = s.begin_styled("f", vec![(0, TrackHeader::Authored(file.clone()))], vec![], white);
        s.push(g, 0, event("hello", 1000), 0);
        let before = s.selected_styled(0, white).unwrap();
        assert_eq!((before.origin, header_of(&before)), (Origin::Authored, file.clone()), "white is the file's own ink: nothing is rewritten");
        let after = s.selected_styled(0, yellow).unwrap();
        assert_eq!((after.id, after.origin), (before.id, Origin::Authored), "authored geometry, same identity");
        assert_eq!(after.revision, before.revision.wrapping_add(1), "one bump for the new pick");
        assert_eq!(header_of(&after), super::super::ass_ink::recolour(&file, SubtitleTone::Yellow).unwrap());
        assert!(Arc::ptr_eq(&after, &s.selected_styled(0, yellow).unwrap()), "once only");
        let Content::Embedded { events, .. } = &after.content else { panic!() };
        assert_eq!(events.len(), 1, "the held events are untouched");
        // a size or position pick is not this track's business, and a grey is a draw-time tint
        let large = TextStyle { size: SubtitleSize::Large, position: SubtitlePosition::High, ..yellow };
        assert!(Arc::ptr_eq(&after, &s.selected_styled(0, large).unwrap()));
        // a cue arriving later is held under the recoloured header, and white puts the file's own back
        s.push(g, 0, event("again", 3000), 0);
        assert_eq!(header_of(&s.selected_styled(0, yellow).unwrap()), header_of(&after));
        let back = s.selected_styled(0, white).unwrap();
        assert_eq!(header_of(&back), file);
        assert_ne!(back.revision, after.revision, "white is a change of pick too");
        // the same file reopened under a pick still matches by the FILE's bytes and keeps its events
        s.selected_styled(0, yellow);
        s.begin_styled("f", vec![(0, TrackHeader::Authored(file.clone()))], vec![], yellow);
        let Content::Embedded { events, .. } = &s.selected_styled(0, yellow).unwrap().content else { panic!() };
        assert_eq!(events.len(), 2, "a reopen under a coloured pick is the same file");
    }

    #[test]
    fn an_authored_track_with_no_white_style_is_never_restyled_by_the_pick() {
        use plx_plex::plex::session::SubtitleTone;
        let coloured = String::from_utf8(authored_header()).unwrap().replace("&H00FFFFFF", "&H0000FFFF").into_bytes();
        let yellow = TextStyle { tone: SubtitleTone::Yellow, ..TextStyle::default() };
        let mut s = Store::default();
        s.begin_styled("f", vec![(0, TrackHeader::Authored(coloured))], vec![], TextStyle::default());
        let before = s.selected_styled(0, TextStyle::default()).unwrap();
        assert!(Arc::ptr_eq(&before, &s.selected_styled(0, yellow).unwrap()), "nothing for the pick to take");
    }

    /// **Whether the pick reaches a track is a fact of the track, read from the FILE's bytes**: a white
    /// primary takes it, a coloured-only script does not, a text track always does, a track the store does
    /// not hold is unknown — and none of that moves with the pick (the held header of a track that took
    /// yellow has no white style left to find).
    #[test]
    fn a_track_knows_whether_the_colour_pick_reaches_it_whatever_the_pick_is() {
        use plx_plex::plex::session::SubtitleTone;
        let coloured = String::from_utf8(authored_header()).unwrap().replace("&H00FFFFFF", "&H0000FFFF").into_bytes();
        let yellow = TextStyle { tone: SubtitleTone::Yellow, ..TextStyle::default() };
        let mut s = Store::default();
        s.begin_styled(
            "f",
            vec![(0, TrackHeader::Authored(authored_header())), (1, TrackHeader::Authored(coloured)), (2, TrackHeader::Text(TextDialect::Srt))],
            vec![],
            yellow,
        );
        let answers = |s: &Store| [s.takes_colour(0), s.takes_colour(1), s.takes_colour(2), s.takes_colour(3)];
        assert_eq!(answers(&s), [Some(true), Some(false), Some(true), None], "opened under yellow");
        s.selected_styled(0, TextStyle::default());
        assert_eq!(answers(&s), [Some(true), Some(false), Some(true), None], "the pick moved off the colour");
        s.selected_styled(0, yellow);
        assert_eq!(answers(&s), [Some(true), Some(false), Some(true), None], "and back");
    }

    /// **`player::subtitle_takes_colour` is the one answer the menu reads**: pictures never, text always,
    /// ASS by the header of the selected sidecar, else of the embedded track being fed, and yes while that
    /// header is unknown. The header's half reaches it through the session's readout, delivered once a
    /// tick by `player::deliver_subtitle_ink`: asked before a delivery, the answer is still the last one.
    #[test]
    fn the_subtitle_takes_colour_fact_follows_the_codec_and_the_selected_track() {
        let _guard = plx_base::testlock::serial();
        reset();
        super::super::sidecar::reset();
        let coloured = String::from_utf8(authored_header()).unwrap().replace("&H00FFFFFF", "&H0000FFFF").into_bytes();
        let mut ps = crate::route::PlaybackSession::IDLE;
        let mut takes = |codec: &str| {
            super::super::deliver_subtitle_ink(&mut ps);
            super::super::subtitle_takes_colour(&ps, codec)
        };
        assert!(!takes("pgs") && !takes("hdmv_pgs_subtitle") && !takes("dvd_subtitle"), "a picture never takes a colour");
        assert!(takes("srt") && takes("subrip") && takes("webvtt") && takes("mov_text"), "text always does");
        assert!(takes("ass"), "nothing known yet: nothing is dimmed");
        begin("fixture", vec![(0, TrackHeader::Authored(authored_header())), (1, TrackHeader::Authored(coloured.clone()))], vec![]);
        super::super::request_subtitle(0);
        assert!(takes("ass") && takes("ssa"), "a white-primary style");
        super::super::request_subtitle(1);
        assert!(!takes("ass") && !takes("ssa"), "no white-primary style");
        // a selected sidecar answers instead of the embedded track
        let with_event = |mut file: Vec<u8>| {
            file.extend_from_slice(b"Dialogue: 0,0:00:01.00,0:00:03.00,Default,,0,0,0,,Hello\n");
            file
        };
        super::super::sidecar::select_loaded_for_test(7, &with_event(authored_header()));
        assert!(takes("ass"), "the sidecar has a white style, the embedded track does not");
        super::super::sidecar::select_loaded_for_test(7, &with_event(coloured));
        super::super::request_subtitle(0);
        assert!(!takes("ass"), "the sidecar has none, the embedded track does");
        super::super::sidecar::reset();
        reset();
    }

    // ---- lazy restyle by style generation (plan §2.5) ----

    #[test]
    fn a_restyle_bumps_the_revision_once_and_keeps_the_events_and_the_identity() {
        use plx_plex::plex::session::{SubtitlePosition, SubtitleSize};
        let small = TextStyle { size: SubtitleSize::Small, position: SubtitlePosition::Low, ..TextStyle::default() };
        let large = TextStyle { size: SubtitleSize::Large, position: SubtitlePosition::Low, ..TextStyle::default() };
        let mut s = Store::default();
        let g = s.begin_styled("f", vec![(0, TrackHeader::Text(TextDialect::Srt))], vec![], small);
        s.push_text(g, 0, 1000, 2000, "cue", 0);
        let before = s.selected_styled(0, small).unwrap();
        assert_eq!(before.origin, Origin::Text(small));
        let again = s.selected_styled(0, small).unwrap();
        assert_eq!(again.revision, before.revision, "the same picks: no change");
        assert!(Arc::ptr_eq(&before, &again));

        let after = s.selected_styled(0, large).unwrap();
        assert_eq!(after.id, before.id, "the identity stays: the screen's clock and frame are not reset");
        assert_eq!(after.revision, before.revision.wrapping_add(1), "one bump for the new picks");
        assert_eq!(after.origin, Origin::Text(large));
        let (Content::Embedded { header: h0, events: e0, .. }, Content::Embedded { header: h1, events: e1, .. }) =
            (&before.content, &after.content)
        else {
            panic!()
        };
        assert_eq!(h1.as_ref(), text_ass::header(&large).as_bytes());
        assert_ne!(h0, h1, "the header follows the picks");
        assert_eq!(e0, e1, "the held events are not touched");
        let twice = s.selected_styled(0, large).unwrap();
        assert_eq!(twice.revision, after.revision, "and once only");
    }

    #[test]
    fn a_reopen_after_a_pick_change_builds_the_header_for_the_new_picks_without_a_second_bump() {
        use plx_plex::plex::session::{SubtitlePosition, SubtitleSize};
        let small = TextStyle { size: SubtitleSize::Small, position: SubtitlePosition::Low, ..TextStyle::default() };
        let large = TextStyle { size: SubtitleSize::Large, position: SubtitlePosition::High, ..TextStyle::default() };
        let mut s = Store::default();
        let g = s.begin_styled("f", vec![(0, TrackHeader::Text(TextDialect::Srt))], vec![], small);
        s.push_text(g, 0, 1000, 2000, "cue", 0);
        s.retain_for_reload();
        s.begin_styled("f", vec![(0, TrackHeader::Text(TextDialect::Srt))], vec![], large);
        let first = s.selected_styled(0, large).unwrap();
        assert_eq!(first.revision, 0, "the header already is for the new picks");
        assert_eq!(first.origin, Origin::Text(large));
        let Content::Embedded { events, .. } = &first.content else { panic!() };
        assert_eq!(events.len(), 1, "and the held cue is kept");
    }

    #[test]
    fn a_text_track_is_restyled_only_when_it_is_asked_for() {
        use plx_plex::plex::session::{SubtitlePosition, SubtitleSize};
        let small = TextStyle { size: SubtitleSize::Small, position: SubtitlePosition::Low, ..TextStyle::default() };
        let large = TextStyle { size: SubtitleSize::Large, position: SubtitlePosition::Low, ..TextStyle::default() };
        let mut s = Store::default();
        s.begin_styled(
            "f",
            vec![(0, TrackHeader::Text(TextDialect::Srt)), (1, TrackHeader::Text(TextDialect::WebVtt))],
            vec![],
            small,
        );
        s.selected_styled(0, large);
        assert_eq!(s.tracks[&1].origin, Origin::Text(small), "track 1 is still for the old picks");
        assert_eq!(s.selected_styled(1, large).unwrap().origin, Origin::Text(large));
    }

    // ---- the `sub cue` log line ----

    #[test]
    fn the_sub_cue_line_logs_a_length_for_the_selected_track_only_and_never_the_text() {
        let _guard = plx_base::testlock::serial();
        let generation = begin(
            "log-fixture",
            vec![(0, TrackHeader::Text(TextDialect::Srt)), (1, TrackHeader::Text(TextDialect::Srt))],
            vec![],
        );
        super::super::request_subtitle(1);
        let log = plx_base::eventlog::with_private_log(|| {
            let retention = plx_playback_contract::subtitle::CaptionRetention {
                playhead_ns: Some(0), seek_target_ns: None,
            };
            push_text(generation, 0, 1_000_000_000, 2_500_000_000, "not the selected track", packet_identity("not the selected track".as_bytes()), retention);
            push_text(generation, 1, 1_000_000_000, 2_500_000_000, "Secret <i>dialogue</i>", packet_identity("Secret <i>dialogue</i>".as_bytes()), retention);
            push_text(generation, 1, 1_000_000_000, 2_500_000_000, "Secret <i>dialogue</i>", packet_identity("Secret <i>dialogue</i>".as_bytes()), retention);
            std::fs::read_to_string(plx_base::eventlog::events_log()).unwrap_or_default()
        });
        super::super::request_subtitle(-1);
        reset();
        let lines: Vec<&str> = log.lines().filter(|l| l.contains("sub cue")).collect();
        assert_eq!(lines.len(), 1, "one new cue of the selected track: {log}");
        assert!(lines[0].ends_with("sub cue [1000..2500ms] len=15"), "{}", lines[0]);
        assert!(!log.contains("Secret") && !log.contains("dialogue"), "the log carries a length, never the text");
    }
}
