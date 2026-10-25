//! Independent per-profile choices for D-pad seeking and episode navigation buttons.
use serde::{de::DeserializeOwned, Deserialize, Serialize};

macro_rules! switch {
    ($name:ident, $default:ident) => {
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
        #[serde(rename_all = "snake_case")]
        pub enum $name { Off, On }
        impl Default for $name { fn default() -> Self { Self::$default } }
        impl $name {
            pub fn index(self) -> u8 { u8::from(self == Self::On) }
            pub fn from_index(index: u8) -> Self { if index == 1 { Self::On } else { Self::Off } }
            pub fn is_default(&self) -> bool { *self == Self::default() }
        }
    };
}
switch!(InstantDpadSeeking, Off);
switch!(EpisodeNavigationButtons, On);

pub(super) fn deserialize<'de, D: serde::Deserializer<'de>, T: DeserializeOwned + Default>(d: D) -> Result<T, D::Error> {
    let value = serde_json::Value::deserialize(d)?;
    Ok(serde_json::from_value(value).unwrap_or_default())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plex::session::{effective, own_prefs, put_own, InstantDpadSeekingPref,
        EpisodeNavigationButtonsPref, Preferences, ProfilePrefs};

    #[test]
    fn playback_options_are_independent_defaulted_and_scoped_per_profile() {
        for json in ["{}", r#"{"instant_dpad_seeking":"future","episode_navigation_buttons":null}"#] {
            let prefs: Preferences = serde_json::from_str(json).unwrap();
            assert_eq!(prefs.instant_dpad_seeking, InstantDpadSeeking::Off);
            assert_eq!(prefs.episode_navigation_buttons, EpisodeNavigationButtons::On);
            let encoded = serde_json::to_value(&prefs).unwrap();
            assert!(encoded.get("instant_dpad_seeking").is_none());
            assert!(encoded.get("episode_navigation_buttons").is_none());
        }
        let prefs = Preferences::default();
        let mut records = Vec::new();
        assert!(put_own::<InstantDpadSeekingPref>(&mut records, "alice", "account", InstantDpadSeeking::On));
        assert!(put_own::<EpisodeNavigationButtonsPref>(&mut records, "alice", "account", EpisodeNavigationButtons::Off));
        assert_eq!(effective::<InstantDpadSeekingPref>(&prefs, own_prefs(&records, "alice", "account")), InstantDpadSeeking::On);
        assert_eq!(effective::<EpisodeNavigationButtonsPref>(&prefs, own_prefs(&records, "alice", "account")), EpisodeNavigationButtons::Off);
        for (user, account) in [("bob", "account"), ("alice", "other")] {
            assert_eq!(effective::<InstantDpadSeekingPref>(&prefs, own_prefs(&records, user, account)), InstantDpadSeeking::Off);
            assert_eq!(effective::<EpisodeNavigationButtonsPref>(&prefs, own_prefs(&records, user, account)), EpisodeNavigationButtons::On);
        }
        let encoded = serde_json::to_string(&records[0].prefs).unwrap();
        assert_eq!(serde_json::from_str::<ProfilePrefs>(&encoded).unwrap(), records[0].prefs);
    }
}
