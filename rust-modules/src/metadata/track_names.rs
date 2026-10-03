//! The names a media container gives its audio and subtitle tracks. The demuxer (`ff`) publishes
//! them into the player's shared state, the track menu reads them, and the data layer's own
//! `sub_layout` and `track_label` join them to the item's Plex streams, so the type lives with the
//! data module that does the joining: a layer below `player` cannot name a type `player` defines.
//! `player::TrackNames` re-exports it for the media layer and the widgets.

/// **The names the CONTAINER gives its audio and subtitle tracks**, each list in file order.
///
/// Published once by the demuxer when it opens a part (`ff`), read by the in-player track
/// menu. Both `Vec`s are dense — a track the file does not name contributes an EMPTY string rather
/// than being skipped — because position is the whole join: the N-th entry is the N-th stream of
/// that type, which is the ordinal `metadata::sub_render_ordinal` resolves a menu row to. Skipping
/// unnamed tracks would silently shift every name after the first untagged one onto its neighbour,
/// which is worse than showing none: a wrong name is indistinguishable from a right one.
#[derive(Default)]
pub(crate) struct TrackNames {
    pub audio: Vec<String>,
    pub subs: Vec<String>,
}

impl TrackNames {
    /// `Default`, but callable from `Shared::new`, which is a `const fn`.
    pub const fn new() -> Self {
        Self {
            audio: Vec::new(),
            subs: Vec::new(),
        }
    }
    /// The name of the `i`-th subtitle stream in file order, or `""` — `i` is what
    /// `metadata::sub_render_ordinal` answers, and its `-1` (an external sidecar, which is not in
    /// the container at all) can be passed straight in.
    pub fn sub(&self, i: i32) -> &str {
        usize::try_from(i)
            .ok()
            .and_then(|i| self.subs.get(i))
            .map(String::as_str)
            .unwrap_or("")
    }
    /// The same for audio — `metadata::audio_ordinal`'s answer.
    pub fn audio(&self, i: i32) -> &str {
        usize::try_from(i)
            .ok()
            .and_then(|i| self.audio.get(i))
            .map(String::as_str)
            .unwrap_or("")
    }
}
