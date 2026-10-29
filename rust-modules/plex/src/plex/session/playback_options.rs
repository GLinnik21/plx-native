//! The per-profile choice for instant D-pad seeking.
use serde::{Deserialize, Serialize};

/// Whether Left/Right on a hidden transport jumps at once instead of first revealing the scrubber —
/// one per profile, like [`super::DeckPress`]. Absence is `Off`, the reveal-first behaviour every
/// build before the preference had.
#[derive(Serialize, Deserialize, Default, Clone, Copy, Debug, PartialEq, Eq)]
pub enum InstantDpadSeeking {
    #[default]
    #[serde(rename = "off")]
    Off,
    #[serde(rename = "on")]
    On,
}

impl InstantDpadSeeking {
    /// Every option, the order the picker lists them in.
    pub const LADDER: [InstantDpadSeeking; 2] = [InstantDpadSeeking::Off, InstantDpadSeeking::On];

    /// An in-memory index back to an option — out of range is the default, for the reason
    /// [`super::SubtitleTone::from_index`] gives: the ladder can grow or shrink.
    pub fn from_index(i: u8) -> InstantDpadSeeking {
        Self::LADDER.get(i as usize).copied().unwrap_or_default()
    }

    pub fn index(self) -> u8 {
        Self::LADDER.iter().position(|&m| m == self).unwrap_or(0) as u8
    }

    /// `skip_serializing_if` needs a function, not just `PartialEq` with `Default::default()`.
    pub fn is_default(&self) -> bool {
        *self == Self::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plex::session::{effective, own_prefs, put_own, InstantDpadSeekingPref,
        Preferences, ProfilePrefs};

    #[test]
    fn instant_dpad_seeking_is_defaulted_and_scoped_per_profile() {
        for json in ["{}", r#"{"instant_dpad_seeking":"future"}"#, r#"{"instant_dpad_seeking":null}"#] {
            let prefs: Preferences = serde_json::from_str(json).unwrap();
            assert_eq!(prefs.instant_dpad_seeking, InstantDpadSeeking::Off);
            let encoded = serde_json::to_value(&prefs).unwrap();
            assert!(encoded.get("instant_dpad_seeking").is_none());
        }
        let prefs = Preferences::default();
        let mut records = Vec::new();
        assert!(put_own::<InstantDpadSeekingPref>(&mut records, "alice", "account", InstantDpadSeeking::On));
        assert_eq!(effective::<InstantDpadSeekingPref>(&prefs, own_prefs(&records, "alice", "account")), InstantDpadSeeking::On);
        for (user, account) in [("bob", "account"), ("alice", "other")] {
            assert_eq!(effective::<InstantDpadSeekingPref>(&prefs, own_prefs(&records, user, account)), InstantDpadSeeking::Off);
        }
        let encoded = serde_json::to_string(&records[0].prefs).unwrap();
        assert_eq!(serde_json::from_str::<ProfilePrefs>(&encoded).unwrap(), records[0].prefs);
    }
}
