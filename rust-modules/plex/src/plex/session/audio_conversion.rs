//! The Convert-audio setting's value (FinNative's "Convert audio: on the TV"); the row is
//! `prefs::AudioConversionPref`.
use serde::{Deserialize, Serialize};

/// **Where audio this television cannot decode is converted** (Settings > Playback > Convert
/// audio) — one per profile, like `DirectPlayMode`. The TV's own decoder takes AC3, E-AC3, AAC
/// and DTS; TrueHD, FLAC, ALAC, Opus, Vorbis and PCM are none of them. `Server`, the default and
/// what every build before the setting did, leaves those out of the direct-play profile, so the
/// server converts the file. `Tv` offers them for direct play and the app decodes the sound itself
/// and re-encodes it to AC3 (`plx_media`'s `ff_audio.rs`), whenever the FFmpeg it ships can
/// (`devcaps::converted_on_tv`).
#[derive(Serialize, Deserialize, Default, Clone, Copy, Debug, PartialEq, Eq)]
pub enum AudioConversion {
    #[default]
    #[serde(rename = "server")]
    Server,
    #[serde(rename = "tv")]
    Tv,
}

impl AudioConversion {
    /// Both places, the default first — the order the picker lists them in.
    pub const LADDER: [AudioConversion; 2] = [AudioConversion::Server, AudioConversion::Tv];

    /// An in-memory index back to a value — out of range is the default, for the reason
    /// `SubtitleTone::from_index` gives.
    pub fn from_index(i: u8) -> AudioConversion {
        Self::LADDER.get(i as usize).copied().unwrap_or_default()
    }

    pub fn index(self) -> u8 {
        Self::LADDER.iter().position(|&c| c == self).unwrap_or(0) as u8
    }
}
