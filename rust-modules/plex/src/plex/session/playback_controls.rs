//! Opt-in transport policy. Kept separate so a fork can carry classic controls across updates.
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlaybackControls {
    #[default]
    Modern,
    #[serde(alias = "legacy")]
    Classic,
}

impl PlaybackControls {
    pub fn index(self) -> u8 {
        u8::from(self == Self::Classic)
    }
    pub fn from_index(index: u8) -> Self {
        if index == 1 {
            Self::Classic
        } else {
            Self::Modern
        }
    }
    pub fn is_modern(&self) -> bool {
        *self == Self::Modern
    }
}

pub(super) fn deserialize<'de, D: serde::Deserializer<'de>>(
    d: D,
) -> Result<PlaybackControls, D::Error> {
    let value = serde_json::Value::deserialize(d)?;
    Ok(serde_json::from_value(value).unwrap_or_default())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plex::session::{
        effective, own_prefs, put_own, PlaybackControlsPref, Preferences, ProfilePrefs,
    };

    #[test]
    fn classic_controls_default_off_and_round_trip_per_profile() {
        for saved in ["classic", "legacy"] {
            let json = format!(r#"{{"playback_controls":"{saved}"}}"#);
            let prefs: Preferences = serde_json::from_str(&json).unwrap();
            assert_eq!(prefs.playback_controls, PlaybackControls::Classic);
            let profile: ProfilePrefs = serde_json::from_str(&json).unwrap();
            assert_eq!(profile.playback_controls, Some(PlaybackControls::Classic));
            assert_eq!(serde_json::to_value(&profile).unwrap()["playback_controls"], "classic");
        }
        let prefs: Preferences =
            serde_json::from_str(r#"{"playback_controls":"unknown"}"#).unwrap();
        assert_eq!(prefs.playback_controls, PlaybackControls::Modern);
        assert!(!serde_json::to_value(&prefs)
            .unwrap()
            .as_object()
            .unwrap()
            .contains_key("playback_controls"));
        let mut records = Vec::new();
        assert!(put_own::<PlaybackControlsPref>(
            &mut records,
            "alice",
            "account",
            PlaybackControls::Classic
        ));
        assert_eq!(
            effective::<PlaybackControlsPref>(&prefs, own_prefs(&records, "alice", "account")),
            PlaybackControls::Classic
        );
        assert_eq!(
            effective::<PlaybackControlsPref>(&prefs, own_prefs(&records, "bob", "account")),
            PlaybackControls::Modern
        );
        assert_eq!(
            effective::<PlaybackControlsPref>(&prefs, own_prefs(&records, "alice", "other")),
            PlaybackControls::Modern
        );
        let encoded = serde_json::to_string(&records[0].prefs).unwrap();
        assert_eq!(
            serde_json::from_str::<ProfilePrefs>(&encoded).unwrap(),
            records[0].prefs
        );
    }
}
