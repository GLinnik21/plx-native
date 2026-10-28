//! **Opus fed to the television's own decoder**, and the record of what a session feeds natively.
//!
//! The Starfish pipeline names `OPUS` beside AC3, E-AC3, AAC and DTS: libpf's
//! `CustomPipeline::parseOptionStringSpi` reads the audio name, and `getAudioCaps` builds
//! `audio/x-opus` from `contents.opusInfo` (`channels`, `sampleRate`, `streamHeader`;
//! `player::engine::with_opus_info` builds it). Read from libpf on a C1, webOS 6.5.3, 2026-10-21.
//!
//! The demuxer records what it is feeding natively ([`publish_fed`]) so the player can tell a native
//! audio refusal from any other failure ([`fed`]): that is what the learned-refusal path and the
//! surround stall watchdog (`player::native_stall`) hang on.
use std::sync::Mutex;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Kind {
    Opus,
}

impl Kind {
    /// From FFmpeg's codec name (`avcodec_get_name`), which is also how PMS spells them.
    pub(crate) fn from_ffmpeg_name(name: &str) -> Option<Kind> {
        match name {
            "opus" => Some(Kind::Opus),
            _ => None,
        }
    }

    /// The name the learned-refusal list and the exclusion rows spell the codec with.
    pub(crate) fn ffmpeg_name(self) -> &'static str {
        match self {
            Kind::Opus => "opus",
        }
    }
}

static FED: Mutex<Option<(Kind, u32)>> = Mutex::new(None);

/// A new session is starting: forget the last one's answer. Called on the main thread before the
/// demuxer is spawned, so no demuxer is alive to answer into the cell.
pub(crate) fn reset() {
    *FED.lock().unwrap_or_else(|e| e.into_inner()) = None;
}

/// The demuxer feeds this session's audio natively: the codec and the track's channel count.
pub(crate) fn publish_fed(kind: Kind, channels: u32) {
    *FED.lock().unwrap_or_else(|e| e.into_inner()) = Some((kind, channels));
}

/// What this session feeds the pipeline natively, once the demuxer has said so. `None` before that
/// and for any track the TV decodes by name alone (AC3, AAC, DTS) or the app decodes (FLAC as PCM).
/// The player reads it to tell a native audio refusal from any other.
pub(crate) fn fed() -> Option<(Kind, u32)> {
    *FED.lock().unwrap_or_else(|e| e.into_inner())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_kinds_are_the_names_ffmpeg_gives() {
        assert_eq!(Kind::from_ffmpeg_name("opus"), Some(Kind::Opus));
        // FLAC is decoded in the app and fed as PCM (`ff_pcm.rs`), never natively.
        for other in ["flac", "vorbis", "ac3", "", "OPUS"] {
            assert_eq!(Kind::from_ffmpeg_name(other), None, "{other:?}");
        }
        assert_eq!(Kind::Opus.ffmpeg_name(), "opus");
    }

    #[test]
    fn what_a_session_feeds_is_remembered_until_the_next_one_starts() {
        let _guard = plx_base::testlock::serial();
        reset();
        assert_eq!(fed(), None);
        publish_fed(Kind::Opus, 6);
        assert_eq!(fed(), Some((Kind::Opus, 6)));
        reset();
        assert_eq!(fed(), None);
    }
}
