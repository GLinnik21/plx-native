//! The alignment classes a Text-origin frame's cues sit in at the rendered `now` (plan
//! `libass-text-subs` §2.4), read from the Style field of each active event.
//!
//! The screen lifts only the bottom-anchored regions while the transport HUD is up, and it cannot
//! tell a bottom cue from a top or middle one by looking at pixels alone. The worker knows which
//! cues are active, so it says so beside the frame it publishes.
use super::{Content, Event, Source};
use crate::player::ass_script::ScriptIndex;
use crate::player::text_ass::AlignClass;

/// Which of the three vertical classes have a cue active.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct AlignSet {
    pub bottom: bool,
    pub middle: bool,
    pub top: bool,
}

impl AlignSet {
    pub fn contains(self, class: AlignClass) -> bool {
        match class {
            AlignClass::Bottom => self.bottom,
            AlignClass::Middle => self.middle,
            AlignClass::Top => self.top,
        }
    }

    pub fn is_empty(self) -> bool {
        !(self.bottom || self.middle || self.top)
    }

    fn insert(&mut self, class: AlignClass) {
        match class {
            AlignClass::Bottom => self.bottom = true,
            AlignClass::Middle => self.middle = true,
            AlignClass::Top => self.top = true,
        }
    }

    /// A Style name the synthesized header does not define renders in `Default`, which is bottom.
    fn insert_style(&mut self, style: &[u8]) {
        let class = std::str::from_utf8(style)
            .ok()
            .and_then(|name| AlignClass::of_style(name.trim()))
            .unwrap_or(AlignClass::Bottom);
        self.insert(class);
    }
}

/// The `field`th comma-separated field of `bytes`.
fn field(bytes: &[u8], field: usize) -> Option<&[u8]> {
    bytes.split(|&b| b == b',').nth(field)
}

/// The Style of a Matroska ASS packet: `ReadOrder,Layer,Style,Name,...`.
fn packet_style(payload: &[u8]) -> Option<&[u8]> {
    field(payload, 2)
}

/// The Style of a script line: `Dialogue: Layer,Start,End,Style,...` in the synthesized Format.
fn dialogue_style(line: &[u8]) -> Option<&[u8]> {
    let text = line.strip_prefix(b"\xef\xbb\xbf").unwrap_or(line);
    let start = text.iter().position(|b| !b.is_ascii_whitespace())?;
    let body = text[start..].strip_prefix(b"Dialogue:")?;
    field(body, 3)
}

/// The classes of the events of an embedded track that are active at `now_ms`
/// (`start <= now < start + duration`, libass's own test).
pub fn of_events(events: &[Event], now_ms: i64) -> AlignSet {
    let mut set = AlignSet::default();
    for event in events {
        if event.start_ms <= now_ms && now_ms < event.start_ms.saturating_add(event.duration_ms) {
            if let Some(style) = packet_style(&event.payload) {
                set.insert_style(style);
            }
        }
    }
    set
}

/// The classes of the script lines alive at `now_ms`.
pub fn of_script(index: &ScriptIndex, now_ms: i64) -> AlignSet {
    let mut set = AlignSet::default();
    for line in index.alive_at(now_ms) {
        if let Some(style) = dialogue_style(line) {
            set.insert_style(style);
        }
    }
    set
}

/// What `Engine` caches to answer for a Text-origin script that is not windowed (a windowed one
/// already holds its index): the parse of the script that `(id, revision)` names.
pub struct ScriptAligns {
    key: (u64, u64),
    index: Option<ScriptIndex>,
}

impl ScriptAligns {
    pub fn at(slot: &mut Option<ScriptAligns>, source: &Source, now_ms: i64) -> AlignSet {
        let Content::Script { bytes, .. } = &source.content else {
            return AlignSet::default();
        };
        let key = (source.id, source.revision);
        if slot.as_ref().is_none_or(|held| held.key != key) {
            *slot = Some(ScriptAligns { key, index: ScriptIndex::parse(bytes) });
        }
        slot.as_ref()
            .and_then(|held| held.index.as_ref())
            .map(|index| of_script(index, now_ms))
            .unwrap_or_default()
    }
}
