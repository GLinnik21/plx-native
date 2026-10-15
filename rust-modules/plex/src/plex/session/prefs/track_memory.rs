//! **Per-show track memory — the data layer.** What a profile picked for a show's audio and
//! subtitles, remembered so the next episode can start on the same pick. This file is only the
//! DATA: the row, the entry, the bounds and the budget. Read at resolve time by `route::recall_all`
//! and `route/track_memory.rs`, and written by the track menu's explicit pick on an episode
//! (`route::commit_audio_pick` / `commit_subtitle_pick`, through `route::remember`), so the key is
//! not serialized until a viewer has picked a track of some show.
//!
//! The list is `Preferences::track_memory`, a [`ProfileList`] per profile held through the memory
//! door (`ProfileMemory::upsert_with`), keyed by `(machine_id, show_rk)`, at most [`CAP`] entries a
//! profile, oldest first. It is **account-bound**: the record carries the account stamp
//! (`grant::account_key`), a record stamped for any other account reads as nothing, a write without
//! a stamp is refused, and a write prunes every record that is not its own. It lives in the public
//! half of the DB8 document, so it is also **bounded in bytes** — see [`BUDGET_BYTES`] and the
//! bounds below. Those bounds are what make "64 entries for one profile" and "the whole household
//! under 32 KiB" arithmetic instead of hope: every string is capped, and the cap is applied when a
//! file is READ as well as when an entry is built, by the one function [`TrackMemoryEntry::normalized`].
//!
//! The wire shape (all keys always written except the halves a profile never picked):
//!
//! ```json
//! { "machine_id": "…", "show_rk": "…",
//!   "audio":    { "lang": "eng", "tag": "en-US", "commentary": false, "ad": false,
//!                 "codec": "eac3", "channels": 6, "title": "" },
//!   "subtitle": { "mode": "track", "lang": "eng", "tag": "", "forced": false, "sdh": false,
//!                 "kind": "text", "external": false, "title": "" } }
//! ```
//!
//! The halves carry no `extensions` and no `deny_unknown_fields`: an unknown key inside a half is
//! ignored on read and not written back, and a half this build cannot read at all (an unknown
//! `mode` from a newer build, a field of the wrong type) is dropped alone, reading as "never
//! picked" while the other half and the rest of the list survive.
use super::{MemoryEntry, Parse, ProfileList, Row, Scope, SettingsWrite};
use crate::plex::session::Session;
use serde::{Deserialize, Serialize};

/// The most entries one profile keeps (the oldest are dropped first).
pub const CAP: usize = 64;
/// The household budget for the whole list, in logical bytes (platform's counting rule). 64
/// worst-case entries for one profile fit in it (`one_profile_keeps_64_worst_case_entries` builds
/// them from the bounds below), so one viewer always keeps the full [`CAP`]; several heavy viewers
/// share what is left.
pub const BUDGET_BYTES: usize = 32 * 1024;

/// Bounds, in bytes. A string longer than its bound is DROPPED (it reads as absent) except a
/// title, which is cut at a character boundary. A key string (`machine_id`, `show_rk`) that is
/// dropped leaves the entry with no key, and an entry with no key is forgotten.
pub const MACHINE_ID_MAX: usize = 64;
pub const SHOW_RK_MAX: usize = 16;
/// A BCP 47 language tag.
pub const TAG_MAX: usize = 35;
pub const CODEC_MAX: usize = 16;
pub const TITLE_MAX: usize = 48;
/// An ISO 639-2 language code (`eng`): what Plex's `languageCode` is.
pub const LANG_MAX: usize = 3;
/// A subtitle's kind (`text`, `image`).
pub const KIND_MAX: usize = 5;
/// Audio channels: Plex reports the layout's count (8 for 7.1), and one digit is the bound, so the
/// 64-worst-case-entries arithmetic holds (a count above it reads as unknown, 0).
pub const CHANNELS_MAX: u8 = 9;

/// **What a profile picked for a show's audio.**
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct AudioMemo {
    /// ISO 639-2 language code of the picked track.
    pub lang: String,
    /// BCP 47 tag (`en-US`), when the track had one.
    pub tag: String,
    /// A commentary track.
    pub commentary: bool,
    /// An audio-description track.
    pub ad: bool,
    pub codec: String,
    /// The channel count (6 for 5.1); 0 when unknown.
    pub channels: u8,
    pub title: String,
}

/// **A specific subtitle track**, matched later by its descriptor rather than its stream id.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct SubtitleTrackMemo {
    pub lang: String,
    pub tag: String,
    pub forced: bool,
    /// Subtitles for the deaf and hard of hearing.
    pub sdh: bool,
    /// `text` or `image`.
    pub kind: String,
    /// A sidecar file rather than an embedded stream. The file differs per episode, so a memo of
    /// one means the next episode's own same-language sidecar (`route::track_memory::memory_subtitle`).
    pub external: bool,
    pub title: String,
}

/// **What a profile picked for a show's subtitles**: a specific track, Off, or Forced only. The
/// mode is the wire tag, so a build that does not know a mode reads that half as absent (see
/// `super::de_soft_opt`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "snake_case")]
pub enum SubtitleMemo {
    /// A specific track, matched by its descriptor (hard: language, `forced`).
    Track(SubtitleTrackMemo),
    /// Off. Like a track, it outranks the account's rule and the server's embedded selection, but
    /// unlike one it does not fall through to them; a server-selected sidecar the app can draw
    /// still shows first (see `rust-modules/plex/src/plex/CLAUDE.md`).
    Off,
    /// Forced only. `lang` is the language of the forced track on screen when the viewer chose it
    /// (empty if none was), used only when the playing audio has no language tag.
    ForcedOnly {
        #[serde(default)]
        lang: String,
    },
}

/// **One show's remembered picks for one profile**, keyed by `(machine_id, show_rk)`. Either half
/// may be absent: it was never picked, and a pick of one half merges into the other through
/// `upsert_with` (the closure sees the entry the list holds NOW).
///
/// Deserialization goes through [`TrackMemoryEntryWire`] and [`normalized`](Self::normalized), and
/// refuses an entry that [`is_blank`](Self::is_blank), so `de_soft_vec` drops it: a hand-edited
/// file cannot hold an over-long string, a keyless entry, or an entry that remembers nothing.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "TrackMemoryEntryWire")]
pub struct TrackMemoryEntry {
    pub machine_id: String,
    /// The show's `ratingKey` (the grandparent of an episode).
    pub show_rk: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub audio: Option<AudioMemo>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub subtitle: Option<SubtitleMemo>,
}

/// The tolerant read shape of a [`TrackMemoryEntry`]: the key strings are required strings (a
/// wrong type fails the element), the halves are soft (`super::de_soft_opt`).
#[derive(Deserialize)]
struct TrackMemoryEntryWire {
    machine_id: String,
    show_rk: String,
    #[serde(default, deserialize_with = "super::de_soft_opt")]
    audio: Option<AudioMemo>,
    #[serde(default, deserialize_with = "super::de_soft_opt")]
    subtitle: Option<SubtitleMemo>,
}

impl TryFrom<TrackMemoryEntryWire> for TrackMemoryEntry {
    type Error = &'static str;
    fn try_from(wire: TrackMemoryEntryWire) -> Result<Self, Self::Error> {
        let entry = TrackMemoryEntry {
            machine_id: wire.machine_id,
            show_rk: wire.show_rk,
            audio: wire.audio,
            subtitle: wire.subtitle,
        }
        .normalized();
        if entry.is_blank() {
            Err("a track-memory entry needs a server, a show and at least one remembered pick")
        } else {
            Ok(entry)
        }
    }
}

/// Cut `s` to at most `max` bytes at a character boundary.
fn truncate_at_boundary(s: &mut String, max: usize) {
    if s.len() > max {
        let mut end = max;
        while !s.is_char_boundary(end) {
            end -= 1;
        }
        s.truncate(end);
    }
}

/// An over-long string is not stored at all: it reads as absent.
fn drop_if_longer(s: &mut String, max: usize) {
    if s.len() > max {
        s.clear();
    }
}

impl AudioMemo {
    fn normalized(mut self) -> Self {
        drop_if_longer(&mut self.lang, LANG_MAX);
        drop_if_longer(&mut self.tag, TAG_MAX);
        drop_if_longer(&mut self.codec, CODEC_MAX);
        truncate_at_boundary(&mut self.title, TITLE_MAX);
        if self.channels > CHANNELS_MAX {
            self.channels = 0;
        }
        self
    }
}

impl SubtitleTrackMemo {
    fn normalized(mut self) -> Self {
        drop_if_longer(&mut self.lang, LANG_MAX);
        drop_if_longer(&mut self.tag, TAG_MAX);
        drop_if_longer(&mut self.kind, KIND_MAX);
        truncate_at_boundary(&mut self.title, TITLE_MAX);
        self
    }
}

impl SubtitleMemo {
    fn normalized(self) -> Self {
        match self {
            SubtitleMemo::Track(track) => SubtitleMemo::Track(track.normalized()),
            SubtitleMemo::Off => SubtitleMemo::Off,
            SubtitleMemo::ForcedOnly { mut lang } => {
                drop_if_longer(&mut lang, LANG_MAX);
                SubtitleMemo::ForcedOnly { lang }
            }
        }
    }
}

impl TrackMemoryEntry {
    /// An entry as the list may hold it: every string within its bound.
    pub fn new(machine_id: &str, show_rk: &str, audio: Option<AudioMemo>, subtitle: Option<SubtitleMemo>) -> Self {
        TrackMemoryEntry { machine_id: machine_id.to_string(), show_rk: show_rk.to_string(), audio, subtitle }.normalized()
    }

    /// The largest entry the list can hold, the `n`th of its profile: EVERY string at its bound, the
    /// widest digits, a sixteen-digit show key. Sixty-four of them for one profile are the household
    /// budget's size (`one_profile_keeps_64_worst_case_entries`), which is what the budget tests build
    /// and what the harness's `plxnative-trackmemory=fill` writes to read the DB8 write cost at the
    /// budget. Built through [`TrackMemoryEntry::new`], like any other.
    pub fn worst_case(n: u32) -> Self {
        let audio = AudioMemo {
            lang: "l".repeat(LANG_MAX),
            tag: "t".repeat(TAG_MAX),
            commentary: false,
            ad: false,
            codec: "c".repeat(CODEC_MAX),
            channels: CHANNELS_MAX,
            title: "t".repeat(TITLE_MAX),
        };
        let subtitle = SubtitleMemo::Track(SubtitleTrackMemo {
            lang: "l".repeat(LANG_MAX),
            tag: "t".repeat(TAG_MAX),
            forced: false,
            sdh: false,
            kind: "k".repeat(KIND_MAX),
            external: false,
            title: "t".repeat(TITLE_MAX),
        });
        TrackMemoryEntry::new(&"m".repeat(MACHINE_ID_MAX), &format!("{n:0>SHOW_RK_MAX$}"), Some(audio), Some(subtitle))
    }

    /// **The one normaliser**: applied when an entry is built, when it is written through
    /// `upsert_with`, and when a file is read. The key strings are dropped when over their bound
    /// (leaving the entry blank), the others as [`AudioMemo`]/[`SubtitleTrackMemo`] state.
    pub fn normalized(mut self) -> Self {
        drop_if_longer(&mut self.machine_id, MACHINE_ID_MAX);
        drop_if_longer(&mut self.show_rk, SHOW_RK_MAX);
        self.audio = self.audio.map(AudioMemo::normalized);
        self.subtitle = self.subtitle.map(SubtitleMemo::normalized);
        self
    }

    /// No key, or nothing remembered: not worth a record.
    pub fn is_blank(&self) -> bool {
        self.machine_id.is_empty() || self.show_rk.is_empty() || (self.audio.is_none() && self.subtitle.is_none())
    }
}

impl MemoryEntry for TrackMemoryEntry {
    type Key = (String, String);
    const CAP: usize = CAP;
    const ACCOUNT_BOUND: bool = true;
    const BUDGET: Option<usize> = Some(BUDGET_BYTES);
    fn key(&self) -> Self::Key {
        (self.machine_id.clone(), self.show_rk.clone())
    }
    fn matches(&self, key: &Self::Key) -> bool {
        self.machine_id == key.0 && self.show_rk == key.1
    }
    fn forgets(&self) -> bool {
        self.is_blank()
    }
    fn normalized(self) -> Self {
        TrackMemoryEntry::normalized(self)
    }
    fn slot(session: &Session) -> &Vec<ProfileList<Self>> {
        &session.prefs.track_memory
    }
    fn slot_mut(session: &mut Session) -> &mut Vec<ProfileList<Self>> {
        &mut session.prefs.track_memory
    }
}

/// The `track_memory` row: one show, one profile, bound to the account, applied by the player's
/// track selection (not a live cell), with no Settings row. A [`Row`] and deliberately not a
/// `ListRow`: it has no whole-list accessor, so it is written only through the memory door.
pub struct TrackMemoryPref;
impl Row for TrackMemoryPref {
    const KEY: &'static str = "track_memory";
    const SCOPE: Scope = Scope::PerShow;
    const ACCOUNT_BOUND: bool = true;
    const LIVE: bool = false;
    const SETTINGS: Option<SettingsWrite> = None;
    const PARSE: Parse = Parse::SoftVec;
}
