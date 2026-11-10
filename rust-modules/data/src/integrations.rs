//! **External media integrations configuration** (Radarr, Sonarr).
//!
//! Stores server connection details and API tokens for external *arr services.
//! Persists to `persistent_state_root()/integrations.json`.

use std::path::PathBuf;
use serde::{Deserialize, Serialize};

/// Normalize a user-supplied service URL by:
/// 1. Trimming leading/trailing whitespace.
/// 2. Prepending `http://` if no scheme is specified.
/// 3. Stripping common API endpoint paths (`/api/v3/system/status`, `/api/v3`, `/api`).
/// 4. Stripping trailing slashes.
pub fn normalize_service_url(raw: &str) -> String {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return String::new();
    }

    let mut url = trimmed.to_string();
    if !url.starts_with("http://") && !url.starts_with("https://") {
        url = format!("http://{}", url);
    }
    while url.ends_with('/') {
        url.pop();
    }

    if let Some(stripped) = url.strip_suffix("/api/v3/system/status") {
        url = stripped.to_string();
    } else if let Some(stripped) = url.strip_suffix("/api/v3") {
        url = stripped.to_string();
    } else if let Some(stripped) = url.strip_suffix("/api") {
        url = stripped.to_string();
    }
    while url.ends_with('/') {
        url.pop();
    }

    url
}

/// Helper to turn a title into a URL slug (e.g. "Breaking Bad" -> "breaking-bad").
pub fn slug(title: &str) -> String {
    let mut out = String::new();
    let mut last_dash = false;
    for c in title.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
            last_dash = false;
        } else if !last_dash && !out.is_empty() {
            out.push('-');
            last_dash = true;
        }
    }
    while out.ends_with('-') {
        out.pop();
    }
    if out.is_empty() {
        "media".to_string()
    } else {
        out
    }
}

/// Supported external *arr services.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
pub enum ArrKind {
    #[default]
    Radarr,
    Sonarr,
}

/// Filter for displaying media in Arr libraries.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
pub enum ArrFilter {
    #[default]
    All,
    Monitored,
    Unmonitored,
    Missing,
}

impl ArrKind {
    pub const ALL: [Self; 2] = [Self::Radarr, Self::Sonarr];

    pub fn name(self) -> &'static str {
        match self {
            Self::Radarr => "Radarr",
            Self::Sonarr => "Sonarr",
        }
    }

    pub fn default_port(self) -> u16 {
        match self {
            Self::Radarr => 7878,
            Self::Sonarr => 8989,
        }
    }
}

/// Quality profile returned by Radarr/Sonarr.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArrQualityProfile {
    pub id: u32,
    pub name: String,
}

/// Root folder returned by Radarr/Sonarr.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArrRootFolder {
    pub id: u32,
    pub path: String,
    #[serde(default)]
    pub free_space_bytes: u64,
}

/// Tag returned by Radarr/Sonarr.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArrTag {
    pub id: u32,
    pub label: String,
}

/// Details (quality profiles, root folders, tags) fetched for an Arr service.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArrServiceDetails {
    pub service: ArrKind,
    pub quality_profiles: Vec<ArrQualityProfile>,
    pub root_folders: Vec<ArrRootFolder>,
    pub tags: Vec<ArrTag>,
}

/// Configuration for a single *arr service.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArrServiceConfig {
    pub url: String,
    pub token: String,
    #[serde(default)]
    pub quality_profiles: Vec<ArrQualityProfile>,
    #[serde(default)]
    pub root_folders: Vec<ArrRootFolder>,
    #[serde(default)]
    pub tags: Vec<ArrTag>,
    #[serde(default)]
    pub default_quality_profile_id: Option<u32>,
    #[serde(default)]
    pub default_root_folder_path: Option<String>,
}

impl ArrServiceConfig {
    pub fn is_configured(&self) -> bool {
        !self.url.trim().is_empty() && !self.token.trim().is_empty()
    }
}

/// A media item found in Radarr or Sonarr lookup.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArrMediaLookup {
    pub service: ArrKind,
    pub title: String,
    pub year: u32,
    pub overview: String,
    pub poster_url: String,
    pub external_id: u32, // tmdbId for Radarr, tvdbId for Sonarr
    pub in_library: bool,
    pub monitored: bool,
    pub has_file: bool,
    pub existing_id: u32,
    pub quality_profile_id: u32,
    pub root_folder_path: String,
}

/// A media item in the user's Radarr or Sonarr library.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArrMediaItem {
    pub service: ArrKind,
    pub id: u32,
    pub title: String,
    pub year: u32,
    pub overview: String,
    pub poster_url: String,
    #[serde(default)]
    pub fanart_url: String,
    pub monitored: bool,
    pub has_file: bool,
    pub status: String,
    pub size_bytes: u64,
    pub quality_profile_id: u32,
    #[serde(default)]
    pub quality_profile_name: String,
    #[serde(default)]
    pub path: String,
    #[serde(default)]
    pub episode_count: u32,
    #[serde(default)]
    pub total_episode_count: u32,
}

/// Combined lookup response across configured services.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArrLookupResponse {
    pub query: String,
    pub radarr_results: Vec<ArrMediaLookup>,
    pub sonarr_results: Vec<ArrMediaLookup>,
    pub details: Vec<ArrServiceDetails>,
    pub error: Option<String>,
}

impl ArrLookupResponse {
    pub fn is_empty(&self) -> bool {
        self.radarr_results.is_empty() && self.sonarr_results.is_empty()
    }

    pub fn total_count(&self) -> usize {
        self.radarr_results.len() + self.sonarr_results.len()
    }
}

/// Request to add or search for an item in Radarr or Sonarr.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArrAddRequest {
    pub service: ArrKind,
    pub title: String,
    pub year: u32,
    pub external_id: u32,
    pub quality_profile_id: u32,
    pub root_folder_path: String,
    pub tags: Vec<u32>,
    pub search_on_add: bool,
    pub existing_id: u32,
}

/// Result of an Add or Search action.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArrAddOutcome {
    pub success: bool,
    pub message: String,
}

static GENERATION: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

/// The current configuration generation. Bumped whenever [`IntegrationsConfig::save`] succeeds.
pub fn generation() -> u64 {
    GENERATION.load(std::sync::atomic::Ordering::Relaxed)
}

/// Invalidate any cached navigation or chrome state that depends on integrations.
pub fn bump_generation() {
    GENERATION.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
}

/// Install-wide configuration for external integrations.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct IntegrationsConfig {
    pub radarr: ArrServiceConfig,
    pub sonarr: ArrServiceConfig,
}

impl IntegrationsConfig {
    pub fn candidates() -> Vec<PathBuf> {
        #[cfg(any(test, feature = "test-support"))]
        {
            vec![plx_base::paths::persistent_state_root().join("integrations.json")]
        }
        #[cfg(not(any(test, feature = "test-support")))]
        {
            plx_base::paths::integrations_candidates()
        }
    }

    pub fn file_path() -> PathBuf {
        for candidate in Self::candidates() {
            if candidate.exists() {
                return candidate;
            }
        }
        Self::candidates().into_iter().next().unwrap_or_else(|| {
            plx_base::paths::persistent_state_root().join("integrations.json")
        })
    }

    pub fn service(&self, kind: ArrKind) -> &ArrServiceConfig {
        match kind {
            ArrKind::Radarr => &self.radarr,
            ArrKind::Sonarr => &self.sonarr,
        }
    }

    pub fn service_mut(&mut self, kind: ArrKind) -> &mut ArrServiceConfig {
        match kind {
            ArrKind::Radarr => &mut self.radarr,
            ArrKind::Sonarr => &mut self.sonarr,
        }
    }

    pub fn load() -> Self {
        for path in Self::candidates() {
            if let Ok(bytes) = std::fs::read(&path) {
                if let Ok(cfg) = serde_json::from_slice::<Self>(&bytes) {
                    return cfg;
                }
            }
        }
        Self::default()
    }

    pub fn save(&self) -> Result<(), std::io::Error> {
        let candidates = Self::candidates();
        let data = serde_json::to_vec_pretty(self)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))?;

        let mut last_err = None;
        for path in &candidates {
            if let Some(parent) = path.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            match std::fs::write(path, &data) {
                Ok(()) => {
                    for other in &candidates {
                        if other != path && other.exists() {
                            let _ = std::fs::remove_file(other);
                        }
                    }
                    bump_generation();
                    return Ok(());
                }
                Err(e) => {
                    last_err = Some(e);
                }
            }
        }
        Err(last_err.unwrap_or_else(|| {
            std::io::Error::new(std::io::ErrorKind::NotFound, "no writable candidate path found")
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_normalize_service_url() {
        assert_eq!(normalize_service_url(""), "");
        assert_eq!(normalize_service_url("   "), "");
        assert_eq!(normalize_service_url("192.168.1.50:7878"), "http://192.168.1.50:7878");
        assert_eq!(normalize_service_url("http://192.168.1.50:7878/"), "http://192.168.1.50:7878");
        assert_eq!(normalize_service_url("https://radarr.local:7878"), "https://radarr.local:7878");
        assert_eq!(normalize_service_url("http://192.168.1.50:7878/api/v3/system/status"), "http://192.168.1.50:7878");
        assert_eq!(normalize_service_url("http://192.168.1.50:7878/api/v3/"), "http://192.168.1.50:7878");
        assert_eq!(normalize_service_url("http://192.168.1.50:7878/radarr/"), "http://192.168.1.50:7878/radarr");
        assert_eq!(normalize_service_url("http://192.168.1.50:7878/radarr/api/v3"), "http://192.168.1.50:7878/radarr");
    }

    #[test]
    fn test_slug() {
        assert_eq!(slug("Breaking Bad"), "breaking-bad");
        assert_eq!(slug("Dune: Part Two (2024)"), "dune-part-two-2024");
        assert_eq!(slug("   "), "media");
    }

    #[test]
    fn serialization_roundtrip() {
        let mut cfg = IntegrationsConfig::default();
        cfg.radarr.url = "http://localhost:7878".to_string();
        cfg.radarr.token = "abc123token".to_string();
        assert!(cfg.radarr.is_configured());
        assert!(!cfg.sonarr.is_configured());

        let bytes = serde_json::to_vec(&cfg).unwrap();
        let loaded: IntegrationsConfig = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(cfg, loaded);
    }

    #[test]
    fn save_and_load_roundtrip() {
        let mut cfg = IntegrationsConfig::default();
        cfg.radarr.url = "http://192.168.1.10:7878".to_string();
        cfg.radarr.token = "testkey123".to_string();
        cfg.save().expect("save should succeed");

        let loaded = IntegrationsConfig::load();
        assert_eq!(cfg, loaded);

        let _ = std::fs::remove_file(IntegrationsConfig::file_path());
    }
}
