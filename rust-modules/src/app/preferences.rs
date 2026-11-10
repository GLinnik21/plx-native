//! Live executor for Settings preference effects. The bridge's controlled-IO guard must admit
//! the effect before this entrypoint: profile capture, thread admission and persistence all live
//! here, never in a screen constructor or step.
mod marker_access;
pub(crate) use marker_access::MarkerAccessRunner;
use plx_media::route::{choose, Choice, How};
use plx_plex::plex::account::{PreferenceError, PreferenceRequest};
use plx_plex::plex::session::{IntroSkipPref, CreditsSkipPref, DolbyVisionModePref, InstantDpadSeekingPref, UriEnginePref, DeckPressPref, DirectPlayModePref, HdrOverlayBrightnessPref, NextEpisodeModePref, QualityPref,
    SkipIntervalPref, SubtitlePositionPref, SubtitleSizePref, TrailerAutoplayPref, UnderPostersPref};
use plx_screens::registry::{AccountPreferenceReply, PreferenceCmd};
use std::sync::mpsc::Sender;

/// The one place a Settings pick meets the write door: each row's own `SETTINGS` policy
/// (`How::settings`) decides whether the pick is confirmed first or published at once.
fn choose_setting(choice: Choice, reply: Sender<bool>) {
    match choice {
        Choice::IntroSkip(v) => choose::<IntroSkipPref>(v, How::settings::<IntroSkipPref>(reply)),
        Choice::CreditsSkip(v) => choose::<CreditsSkipPref>(v, How::settings::<CreditsSkipPref>(reply)),
        Choice::Quality(v) => choose::<QualityPref>(v, How::settings::<QualityPref>(reply)),
        Choice::DirectPlay(v) => choose::<DirectPlayModePref>(v, How::settings::<DirectPlayModePref>(reply)),
        Choice::NextEpisode(v) => choose::<NextEpisodeModePref>(v, How::settings::<NextEpisodeModePref>(reply)),
        Choice::InstantDpadSeeking(v) => choose::<InstantDpadSeekingPref>(v, How::settings::<InstantDpadSeekingPref>(reply)),
        Choice::UriEngine(v) => choose::<UriEnginePref>(v, How::settings::<UriEnginePref>(reply)),
        Choice::DolbyVision(v) => choose::<DolbyVisionModePref>(v, How::settings::<DolbyVisionModePref>(reply)),
        Choice::SkipInterval(v) => choose::<SkipIntervalPref>(v, How::settings::<SkipIntervalPref>(reply)),
        Choice::DeckPress(v) => choose::<DeckPressPref>(v, How::settings::<DeckPressPref>(reply)),
        Choice::UnderPosters(v) => choose::<UnderPostersPref>(v, How::settings::<UnderPostersPref>(reply)),
        Choice::TrailerAutoplay(v) => choose::<TrailerAutoplayPref>(v, How::settings::<TrailerAutoplayPref>(reply)),
        Choice::SubtitleSize(v) => choose::<SubtitleSizePref>(v, How::settings::<SubtitleSizePref>(reply)),
        Choice::SubtitlePosition(v) => choose::<SubtitlePositionPref>(v, How::settings::<SubtitlePositionPref>(reply)),
        Choice::HdrOverlayBrightness(v) => choose::<HdrOverlayBrightnessPref>(v, How::settings::<HdrOverlayBrightnessPref>(reply)),
    }
}

pub(super) fn execute(command: PreferenceCmd) {
    match command {
        // These commands need the Bridge's admitted, profile-scoped feature publication.
        PreferenceCmd::EnsureMarkerAccess => {}
        PreferenceCmd::ChooseMarker { reply, .. } => { let _ = reply.send(false); }
        PreferenceCmd::Choose(Choice::IntroSkip(_) | Choice::CreditsSkip(_), reply) => { let _ = reply.send(false); }
        PreferenceCmd::Load { reply } => {
            let Some(request) = PreferenceRequest::capture() else {
                let _ = reply.send(AccountPreferenceReply {
                    request: None, outcome: Err(PreferenceError::Unavailable),
                });
                plx_machine::idle::invalidate();
                return;
            };
            // A refused spawn drops the reply sender. The screen reports its disconnected
            // receipt as retryable without claiming that an account call ran.
            plx_base::task::spawn_small("acct-prefs", move || {
                let outcome = request.load();
                let _ = reply.send(AccountPreferenceReply { request: Some(request), outcome });
                plx_machine::idle::invalidate();
            });
        }
        PreferenceCmd::Save { request, base, update, reply } => {
            plx_base::task::spawn_small("acct-prefs", move || {
                let outcome = request.save(&base, update);
                let _ = reply.send(AccountPreferenceReply { request: Some(request), outcome });
                plx_machine::idle::invalidate();
            });
        }
        // Every live setting saves through the one write door, in `choose_setting`.
        PreferenceCmd::Choose(choice, reply) => choose_setting(choice, reply),
        PreferenceCmd::UiResolution { mode, reply } => {
            let _ = plx_base::storage_worker::submit_retained(move || {
                let _ = reply.send(plx_plex::plex::session::set_ui_resolution(mode));
                plx_machine::idle::invalidate();
            });
        }
        PreferenceCmd::Language { language, reply } => {
            let _ = plx_base::storage_worker::submit_retained(move || {
                let _ = reply.send(plx_plex::plex::session::set_language(language));
                plx_machine::idle::invalidate();
            });
        }
        PreferenceCmd::ValidateArr { service, url, token, reply } => {
            let _ = plx_base::storage_worker::submit_retained(move || {
                let outcome = validate_arr_service(service, &url, &token);
                let _ = reply.send(outcome);
                plx_machine::idle::invalidate();
            });
        }
        PreferenceCmd::SaveArrConfig { service, url, token } => {
            let _ = plx_base::storage_worker::submit_retained(move || {
                let mut config = plx_data::integrations::IntegrationsConfig::load();
                *config.service_mut(service) = plx_data::integrations::ArrServiceConfig {
                    url: url.trim().to_string(),
                    token: token.trim().to_string(),
                    ..Default::default()
                };
                let _ = config.save();
                plx_machine::idle::invalidate();
            });
        }
        PreferenceCmd::LookupArr { query, reply } => {
            let _ = plx_base::storage_worker::submit_retained(move || {
                let response = lookup_arr_all(&query);
                let _ = reply.send(response);
                plx_machine::idle::invalidate();
            });
        }
        PreferenceCmd::AddArrItem { request, reply } => {
            let _ = plx_base::storage_worker::submit_retained(move || {
                let outcome = add_arr_item(&request);
                let _ = reply.send(outcome);
                plx_machine::idle::invalidate();
            });
        }
        PreferenceCmd::FetchArrLibrary { service, reply } => {
            let _ = plx_base::storage_worker::submit_retained(move || {
                let outcome = fetch_arr_library(service);
                let _ = reply.send(outcome);
                plx_machine::idle::invalidate();
            });
        }
        PreferenceCmd::ToggleArrMonitored { service, id, monitored, reply } => {
            let _ = plx_base::storage_worker::submit_retained(move || {
                let outcome = toggle_arr_monitored(service, id, monitored);
                let _ = reply.send(outcome);
                plx_machine::idle::invalidate();
            });
        }
        PreferenceCmd::TriggerArrSearch { service, id, reply } => {
            let _ = plx_base::storage_worker::submit_retained(move || {
                let outcome = trigger_arr_search(service, id);
                let _ = reply.send(outcome);
                plx_machine::idle::invalidate();
            });
        }
    }
}

fn validate_arr_service(service: plx_data::integrations::ArrKind, url: &str, token: &str) -> Result<String, String> {
    let mut normalized = plx_data::integrations::normalize_service_url(url);
    let token = token.trim();
    if normalized.is_empty() {
        return Err("Server address cannot be empty".to_string());
    }
    if token.is_empty() {
        return Err("API key cannot be empty".to_string());
    }

    // Persist immediately so what the user typed is stored even if validation errors out:
    let mut config = plx_data::integrations::IntegrationsConfig::load();
    *config.service_mut(service) = plx_data::integrations::ArrServiceConfig {
        url: normalized.clone(),
        token: token.to_string(),
        ..Default::default()
    };
    let _ = config.save();

    let headers = vec![
        format!("X-Api-Key: {}", token),
        "Accept: application/json".to_string(),
    ];
    let timeouts = plx_net::net::Timeouts {
        connect_s: 5,
        total_s: 10,
        total_ms: 0,
        low_speed_bps: 0,
        low_speed_s: 0,
    };

    let query_sep = if normalized.contains('?') { '&' } else { '?' };
    let test_url = format!("{}/api/v3/system/status{}apikey={}", normalized, query_sep, token);

    plx_base::eventlog::log(&format!("integrations: validating {} at {}", service.name(), normalized));

    let mut resp = match plx_net::net::request_result(&test_url, &headers, "GET", None, timeouts, false, Some(65536), None) {
        Ok(r) => r,
        Err(e) => {
            plx_base::eventlog::log(&format!("integrations: request to {} failed: {:?}", test_url, e));
            return match e {
                plx_net::net::RequestError::TimedOut => Err("Connection timed out. Check server address and port".to_string()),
                plx_net::net::RequestError::Transport => Err("Connection failed. Check network, server address and port".to_string()),
            };
        }
    };

    // If HTTP returned a redirect (e.g. 301/302/307/308) and scheme was http://, try https://
    if normalized.starts_with("http://") && (301..=308).contains(&resp.status) {
        let https_normalized = format!("https://{}", &normalized["http://".len()..]);
        let https_url = format!("{}/api/v3/system/status{}apikey={}", https_normalized, query_sep, token);
        plx_base::eventlog::log(&format!("integrations: redirect ({}) detected, retrying with https: {}", resp.status, https_normalized));
        if let Ok(r) = plx_net::net::request_result(&https_url, &headers, "GET", None, timeouts, false, Some(65536), None) {
            if r.status == 200 {
                normalized = https_normalized;
                resp = r;
            }
        }
    }

    // If 404, check if the instance is behind a URL Base like /radarr or /sonarr
    if resp.status == 404 {
        let slug = match service {
            plx_data::integrations::ArrKind::Radarr => "radarr",
            plx_data::integrations::ArrKind::Sonarr => "sonarr",
        };
        if !normalized.ends_with(slug) {
            let base_normalized = format!("{}/{}", normalized, slug);
            let base_url = format!("{}/api/v3/system/status{}apikey={}", base_normalized, query_sep, token);
            if let Ok(r) = plx_net::net::request_result(&base_url, &headers, "GET", None, timeouts, false, Some(65536), None) {
                if r.status == 200 {
                    normalized = base_normalized;
                    resp = r;
                }
            }
        }
    }

    if resp.status == 200 {
        let mut app_info = service.name().to_string();
        if let Ok(v) = serde_json::from_slice::<serde_json::Value>(&resp.body) {
            if let Some(ver) = v.get("version").and_then(|v| v.as_str()) {
                app_info = format!("{} v{}", service.name(), ver);
            }
        }

        // Update config with the normalized/detected URL if it was redirected or has a URL base
        let mut config = plx_data::integrations::IntegrationsConfig::load();
        let details = fetch_arr_details(service, &normalized, token);
        config.service_mut(service).quality_profiles = details.quality_profiles;
        config.service_mut(service).root_folders = details.root_folders;
        config.service_mut(service).tags = details.tags;
        if config.service(service).url != normalized {
            config.service_mut(service).url = normalized;
        }
        if let Err(e) = config.save() {
            return Err(format!("Validated but failed to save config: {e}"));
        }

        Ok(app_info)
    } else if resp.status == 401 {
        plx_base::eventlog::log(&format!("integrations: validation failed for {}: HTTP 401 Unauthorized (invalid API key)", service.name()));
        Err("Authentication failed: Invalid API key (HTTP 401 Unauthorized)".to_string())
    } else if resp.status == 403 {
        plx_base::eventlog::log(&format!("integrations: validation failed for {}: HTTP 403 Forbidden", service.name()));
        Err("Access forbidden: HTTP 403 Forbidden".to_string())
    } else if resp.status == 404 {
        plx_base::eventlog::log(&format!("integrations: validation failed for {}: HTTP 404 Not Found at {}", service.name(), normalized));
        Err("Endpoint not found: HTTP 404. Check server URL, port, and URL Base".to_string())
    } else if (301..=308).contains(&resp.status) {
        plx_base::eventlog::log(&format!("integrations: validation failed for {}: unhandled redirect (HTTP {})", service.name(), resp.status));
        Err(format!("Server redirected (HTTP {}). Check if HTTPS or a URL base path is required", resp.status))
    } else {
        plx_base::eventlog::log(&format!("integrations: validation failed for {}: server returned HTTP {}", service.name(), resp.status));
        Err(format!("Server returned HTTP error status {}", resp.status))
    }
}

fn url_encode(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || b == b'-' || b == b'_' || b == b'.' || b == b'~' {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{:02X}", b));
        }
    }
    out
}

fn arr_request(
    url: &str,
    token: &str,
    verb: &str,
    body: Option<&[u8]>,
) -> Result<plx_net::net::Resp, String> {
    let headers = vec![
        format!("X-Api-Key: {}", token),
        "Accept: application/json".to_string(),
        "Content-Type: application/json".to_string(),
    ];
    let timeouts = plx_net::net::Timeouts {
        connect_s: 5,
        total_s: 15,
        total_ms: 0,
        low_speed_bps: 0,
        low_speed_s: 0,
    };
    let query_sep = if url.contains('?') { '&' } else { '?' };
    let full_url = format!("{}{}apikey={}", url, query_sep, token);
    plx_net::net::request_result(&full_url, &headers, verb, body, timeouts, false, Some(512 * 1024), None)
        .map_err(|e| match e {
            plx_net::net::RequestError::TimedOut => "Connection timed out".to_string(),
            plx_net::net::RequestError::Transport => "Network transport failed".to_string(),
        })
}

fn fetch_arr_details(
    service: plx_data::integrations::ArrKind,
    url: &str,
    token: &str,
) -> plx_data::integrations::ArrServiceDetails {
    use plx_data::integrations::{ArrQualityProfile, ArrRootFolder, ArrServiceDetails, ArrTag};
    let mut details = ArrServiceDetails {
        service,
        quality_profiles: Vec::new(),
        root_folders: Vec::new(),
        tags: Vec::new(),
    };

    if let Ok(resp) = arr_request(&format!("{}/api/v3/qualityprofile", url), token, "GET", None) {
        if resp.status == 200 {
            if let Ok(val) = serde_json::from_slice::<serde_json::Value>(&resp.body) {
                if let Some(arr) = val.as_array() {
                    for item in arr {
                        if let (Some(id), Some(name)) = (item.get("id").and_then(|v| v.as_u64()), item.get("name").and_then(|v| v.as_str())) {
                            details.quality_profiles.push(ArrQualityProfile { id: id as u32, name: name.to_string() });
                        }
                    }
                }
            }
        }
    }

    if let Ok(resp) = arr_request(&format!("{}/api/v3/rootfolder", url), token, "GET", None) {
        if resp.status == 200 {
            if let Ok(val) = serde_json::from_slice::<serde_json::Value>(&resp.body) {
                if let Some(arr) = val.as_array() {
                    for item in arr {
                        if let (Some(id), Some(path)) = (item.get("id").and_then(|v| v.as_u64()), item.get("path").and_then(|v| v.as_str())) {
                            let free = item.get("freeSpace").and_then(|v| v.as_u64()).unwrap_or(0);
                            details.root_folders.push(ArrRootFolder { id: id as u32, path: path.to_string(), free_space_bytes: free });
                        }
                    }
                }
            }
        }
    }

    if let Ok(resp) = arr_request(&format!("{}/api/v3/tag", url), token, "GET", None) {
        if resp.status == 200 {
            if let Ok(val) = serde_json::from_slice::<serde_json::Value>(&resp.body) {
                if let Some(arr) = val.as_array() {
                    for item in arr {
                        if let (Some(id), Some(label)) = (item.get("id").and_then(|v| v.as_u64()), item.get("label").and_then(|v| v.as_str())) {
                            details.tags.push(ArrTag { id: id as u32, label: label.to_string() });
                        }
                    }
                }
            }
        }
    }

    details
}

fn lookup_arr_all(query: &str) -> plx_data::integrations::ArrLookupResponse {
    use plx_data::integrations::{ArrKind, ArrLookupResponse, ArrMediaLookup, IntegrationsConfig};

    let mut response = ArrLookupResponse {
        query: query.to_string(),
        ..Default::default()
    };

    let term = query.trim();
    if term.is_empty() {
        return response;
    }
    let encoded_term = url_encode(term);

    let mut config = IntegrationsConfig::load();
    let mut config_dirty = false;

    // Radarr Movies:
    if config.radarr.is_configured() {
        let r_url = &config.radarr.url;
        let r_token = &config.radarr.token;

        if config.radarr.quality_profiles.is_empty() || config.radarr.root_folders.is_empty() {
            let details = fetch_arr_details(ArrKind::Radarr, r_url, r_token);
            config.radarr.quality_profiles = details.quality_profiles.clone();
            config.radarr.root_folders = details.root_folders.clone();
            config.radarr.tags = details.tags.clone();
            response.details.push(details);
            config_dirty = true;
        } else {
            response.details.push(plx_data::integrations::ArrServiceDetails {
                service: ArrKind::Radarr,
                quality_profiles: config.radarr.quality_profiles.clone(),
                root_folders: config.radarr.root_folders.clone(),
                tags: config.radarr.tags.clone(),
            });
        }

        let default_profile = config.radarr.default_quality_profile_id
            .or_else(|| config.radarr.quality_profiles.first().map(|p| p.id))
            .unwrap_or(1);
        let default_root = config.radarr.default_root_folder_path
            .clone()
            .or_else(|| config.radarr.root_folders.first().map(|f| f.path.clone()))
            .unwrap_or_default();

        let req_url = format!("{}/api/v3/movie/lookup?term={}", r_url, encoded_term);
        plx_base::eventlog::log(&format!("integrations: lookup Radarr movies for '{}'", term));
        match arr_request(&req_url, r_token, "GET", None) {
            Ok(resp) if resp.status == 200 => {
                if let Ok(val) = serde_json::from_slice::<serde_json::Value>(&resp.body) {
                    if let Some(arr) = val.as_array() {
                        for item in arr {
                            let title = item.get("title").and_then(|v| v.as_str()).unwrap_or("").to_string();
                            if title.is_empty() { continue; }
                            let year = item.get("year").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
                            let overview = item.get("overview").and_then(|v| v.as_str()).unwrap_or("").to_string();
                            let tmdb_id = item.get("tmdbId").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
                            let existing_id = item.get("id").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
                            let has_file = item.get("hasFile").and_then(|v| v.as_bool()).unwrap_or(false);
                            let monitored = item.get("monitored").and_then(|v| v.as_bool()).unwrap_or(false);
                            let in_library = existing_id > 0 || has_file;

                            let mut poster = item.get("remotePoster").and_then(|v| v.as_str()).unwrap_or("").to_string();
                            if poster.is_empty() {
                                if let Some(images) = item.get("images").and_then(|v| v.as_array()) {
                                    for img in images {
                                        if img.get("coverType").and_then(|v| v.as_str()) == Some("poster") {
                                            if let Some(u) = img.get("remoteUrl").and_then(|v| v.as_str()).filter(|s| !s.is_empty()) {
                                                poster = u.to_string();
                                                break;
                                            }
                                            if let Some(u) = img.get("url").and_then(|v| v.as_str()).filter(|s| !s.is_empty()) {
                                                let base = r_url.trim_end_matches('/');
                                                let sep = if u.contains('?') { "&" } else { "?" };
                                                poster = format!("{}{}{sep}apikey={}", base, u, r_token);
                                                break;
                                            }
                                        }
                                    }
                                }
                            }
                            if poster.contains("/t/p/original/") {
                                poster = poster.replace("/t/p/original/", "/t/p/w500/");
                            }
                            if poster.contains("/t/p/w1280/") {
                                poster = poster.replace("/t/p/w1280/", "/t/p/w500/");
                            }

                            response.radarr_results.push(ArrMediaLookup {
                                service: ArrKind::Radarr,
                                title,
                                year,
                                overview,
                                poster_url: poster,
                                external_id: tmdb_id,
                                in_library,
                                monitored,
                                has_file,
                                existing_id,
                                quality_profile_id: default_profile,
                                root_folder_path: default_root.clone(),
                            });
                        }
                    }
                }
            }
            Ok(resp) => {
                plx_base::eventlog::log(&format!("integrations: Radarr lookup returned status {}", resp.status));
            }
            Err(e) => {
                plx_base::eventlog::log(&format!("integrations: Radarr lookup failed: {}", e));
            }
        }
    }

    // Sonarr Series:
    if config.sonarr.is_configured() {
        let s_url = &config.sonarr.url;
        let s_token = &config.sonarr.token;

        if config.sonarr.quality_profiles.is_empty() || config.sonarr.root_folders.is_empty() {
            let details = fetch_arr_details(ArrKind::Sonarr, s_url, s_token);
            config.sonarr.quality_profiles = details.quality_profiles.clone();
            config.sonarr.root_folders = details.root_folders.clone();
            config.sonarr.tags = details.tags.clone();
            response.details.push(details);
            config_dirty = true;
        } else {
            response.details.push(plx_data::integrations::ArrServiceDetails {
                service: ArrKind::Sonarr,
                quality_profiles: config.sonarr.quality_profiles.clone(),
                root_folders: config.sonarr.root_folders.clone(),
                tags: config.sonarr.tags.clone(),
            });
        }

        let default_profile = config.sonarr.default_quality_profile_id
            .or_else(|| config.sonarr.quality_profiles.first().map(|p| p.id))
            .unwrap_or(1);
        let default_root = config.sonarr.default_root_folder_path
            .clone()
            .or_else(|| config.sonarr.root_folders.first().map(|f| f.path.clone()))
            .unwrap_or_default();

        let req_url = format!("{}/api/v3/series/lookup?term={}", s_url, encoded_term);
        plx_base::eventlog::log(&format!("integrations: lookup Sonarr series for '{}'", term));
        match arr_request(&req_url, s_token, "GET", None) {
            Ok(resp) if resp.status == 200 => {
                if let Ok(val) = serde_json::from_slice::<serde_json::Value>(&resp.body) {
                    if let Some(arr) = val.as_array() {
                        for item in arr {
                            let title = item.get("title").and_then(|v| v.as_str()).unwrap_or("").to_string();
                            if title.is_empty() { continue; }
                            let year = item.get("year").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
                            let overview = item.get("overview").and_then(|v| v.as_str()).unwrap_or("").to_string();
                            let tvdb_id = item.get("tvdbId").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
                            let existing_id = item.get("id").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
                            let monitored = item.get("monitored").and_then(|v| v.as_bool()).unwrap_or(false);
                            let has_file = item.get("statistics")
                                .and_then(|s| s.get("episodeFileCount"))
                                .and_then(|v| v.as_u64())
                                .unwrap_or(0) > 0;
                            let in_library = existing_id > 0;

                            let mut poster = item.get("remotePoster").and_then(|v| v.as_str()).unwrap_or("").to_string();
                            if poster.is_empty() {
                                if let Some(images) = item.get("images").and_then(|v| v.as_array()) {
                                    for img in images {
                                        if img.get("coverType").and_then(|v| v.as_str()) == Some("poster") {
                                            if let Some(u) = img.get("remoteUrl").and_then(|v| v.as_str()).filter(|s| !s.is_empty()) {
                                                poster = u.to_string();
                                                break;
                                            }
                                            if let Some(u) = img.get("url").and_then(|v| v.as_str()).filter(|s| !s.is_empty()) {
                                                let base = s_url.trim_end_matches('/');
                                                let sep = if u.contains('?') { "&" } else { "?" };
                                                poster = format!("{}{}{sep}apikey={}", base, u, s_token);
                                                break;
                                            }
                                        }
                                    }
                                }
                            }
                            if poster.contains("/t/p/original/") {
                                poster = poster.replace("/t/p/original/", "/t/p/w500/");
                            }
                            if poster.contains("/t/p/w1280/") {
                                poster = poster.replace("/t/p/w1280/", "/t/p/w500/");
                            }

                            response.sonarr_results.push(ArrMediaLookup {
                                service: ArrKind::Sonarr,
                                title,
                                year,
                                overview,
                                poster_url: poster,
                                external_id: tvdb_id,
                                in_library,
                                monitored,
                                has_file,
                                existing_id,
                                quality_profile_id: default_profile,
                                root_folder_path: default_root.clone(),
                            });
                        }
                    }
                }
            }
            Ok(resp) => {
                plx_base::eventlog::log(&format!("integrations: Sonarr lookup returned status {}", resp.status));
            }
            Err(e) => {
                plx_base::eventlog::log(&format!("integrations: Sonarr lookup failed: {}", e));
            }
        }
    }

    if config_dirty {
        let _ = config.save();
    }

    response
}

fn add_arr_item(req: &plx_data::integrations::ArrAddRequest) -> plx_data::integrations::ArrAddOutcome {
    use plx_data::integrations::{ArrAddOutcome, ArrKind, IntegrationsConfig};

    let config = IntegrationsConfig::load();
    let service_cfg = config.service(req.service);
    if !service_cfg.is_configured() {
        return ArrAddOutcome {
            success: false,
            message: format!("{} is not configured in Settings", req.service.name()),
        };
    }

    let url = service_cfg.url.clone();
    let token = service_cfg.token.clone();

    if req.existing_id > 0 {
        let (endpoint, cmd_body) = match req.service {
            ArrKind::Radarr => (
                format!("{}/api/v3/command", url),
                serde_json::json!({
                    "name": "MoviesSearch",
                    "movieIds": [req.existing_id]
                }),
            ),
            ArrKind::Sonarr => (
                format!("{}/api/v3/command", url),
                serde_json::json!({
                    "name": "SeriesSearch",
                    "seriesId": req.existing_id
                }),
            ),
        };
        let body_bytes = serde_json::to_vec(&cmd_body).unwrap_or_default();
        plx_base::eventlog::log(&format!("integrations: triggering search on {} for id {}", req.service.name(), req.existing_id));
        match arr_request(&endpoint, &token, "POST", Some(&body_bytes)) {
            Ok(resp) if resp.status == 200 || resp.status == 201 => {
                return ArrAddOutcome {
                    success: true,
                    message: format!("Search initiated on {}!", req.service.name()),
                };
            }
            Ok(resp) => {
                return ArrAddOutcome {
                    success: false,
                    message: format!("Search command failed with status {}", resp.status),
                };
            }
            Err(e) => {
                return ArrAddOutcome {
                    success: false,
                    message: format!("Failed to connect to {}: {}", req.service.name(), e),
                };
            }
        }
    }

    let mut quality_profile_id = req.quality_profile_id;
    let mut root_folder_path = req.root_folder_path.clone();

    if quality_profile_id == 0 || root_folder_path.is_empty() {
        let details = fetch_arr_details(req.service, &url, &token);
        if quality_profile_id == 0 {
            if let Some(first_prof) = details.quality_profiles.first() {
                quality_profile_id = first_prof.id;
            }
        }
        if root_folder_path.is_empty() {
            if let Some(first_folder) = details.root_folders.first() {
                root_folder_path = first_folder.path.clone();
            }
        }
    }

    if quality_profile_id == 0 {
        return ArrAddOutcome {
            success: false,
            message: format!("No quality profile found on {}. Please create one in {}", req.service.name(), req.service.name()),
        };
    }
    if root_folder_path.is_empty() {
        return ArrAddOutcome {
            success: false,
            message: format!("No root folder found on {}. Please set up a root folder in {}", req.service.name(), req.service.name()),
        };
    }

    let title_slug = plx_data::integrations::slug(&format!("{}-{}", req.title, req.external_id));

    let (endpoint, payload) = match req.service {
        ArrKind::Radarr => (
            format!("{}/api/v3/movie", url),
            serde_json::json!({
                "title": req.title,
                "qualityProfileId": quality_profile_id,
                "titleSlug": title_slug,
                "tmdbId": req.external_id,
                "year": req.year,
                "rootFolderPath": root_folder_path,
                "monitored": true,
                "tags": req.tags,
                "addOptions": {
                    "searchForMovie": req.search_on_add
                }
            }),
        ),
        ArrKind::Sonarr => (
            format!("{}/api/v3/series", url),
            serde_json::json!({
                "title": req.title,
                "qualityProfileId": quality_profile_id,
                "titleSlug": title_slug,
                "tvdbId": req.external_id,
                "year": req.year,
                "rootFolderPath": root_folder_path,
                "monitored": true,
                "tags": req.tags,
                "addOptions": {
                    "searchForMissingEpisodes": req.search_on_add
                }
            }),
        ),
    };

    let body_bytes = serde_json::to_vec(&payload).unwrap_or_default();
    plx_base::eventlog::log(&format!("integrations: adding '{}' to {}", req.title, req.service.name()));
    match arr_request(&endpoint, &token, "POST", Some(&body_bytes)) {
        Ok(resp) if resp.status == 200 || resp.status == 201 => {
            let search_note = if req.search_on_add { " and search triggered" } else { "" };
            ArrAddOutcome {
                success: true,
                message: format!("Added '{}' to {}{}!", req.title, req.service.name(), search_note),
            }
        }
        Ok(resp) => {
            let mut err_msg = format!("Failed to add to {} (HTTP {})", req.service.name(), resp.status);
            if let Ok(v) = serde_json::from_slice::<serde_json::Value>(&resp.body) {
                if let Some(msg) = v.get("message").and_then(|m| m.as_str()) {
                    err_msg = format!("{}: {}", req.service.name(), msg);
                } else if let Some(arr) = v.as_array() {
                    if let Some(first_err) = arr.first().and_then(|e| e.get("errorMessage")).and_then(|m| m.as_str()) {
                        err_msg = format!("{}: {}", req.service.name(), first_err);
                    }
                }
            }
            ArrAddOutcome {
                success: false,
                message: err_msg,
            }
        }
        Err(e) => ArrAddOutcome {
            success: false,
            message: format!("Failed to connect to {}: {}", req.service.name(), e),
        },
    }
}

fn fetch_arr_library(service: plx_data::integrations::ArrKind) -> Result<Vec<plx_data::integrations::ArrMediaItem>, String> {
    use plx_data::integrations::{ArrKind, ArrMediaItem, IntegrationsConfig};
    let config = IntegrationsConfig::load();
    let service_cfg = config.service(service);
    if !service_cfg.is_configured() {
        return Err(format!("{} is not configured in Settings", service.name()));
    }
    let url = &service_cfg.url;
    let token = &service_cfg.token;

    match service {
        ArrKind::Radarr => {
            let req_url = format!("{}/api/v3/movie", url);
            let resp = arr_request(&req_url, token, "GET", None)?;
            if resp.status != 200 {
                return Err(format!("Radarr returned HTTP status {}", resp.status));
            }
            let val = serde_json::from_slice::<serde_json::Value>(&resp.body)
                .map_err(|e| format!("Failed to parse JSON: {}", e))?;
            let arr = val.as_array().ok_or_else(|| "Response was not a JSON array".to_string())?;

            let mut items = Vec::with_capacity(arr.len());
            for item in arr {
                let id = item.get("id").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
                let title = item.get("title").and_then(|v| v.as_str()).unwrap_or("").to_string();
                if title.is_empty() { continue; }
                let year = item.get("year").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
                let overview = item.get("overview").and_then(|v| v.as_str()).unwrap_or("").to_string();
                let monitored = item.get("monitored").and_then(|v| v.as_bool()).unwrap_or(false);
                let has_file = item.get("hasFile").and_then(|v| v.as_bool()).unwrap_or(false);
                let status = item.get("status").and_then(|v| v.as_str()).unwrap_or("").to_string();
                let size_bytes = item.get("sizeOnDisk").and_then(|v| v.as_u64()).unwrap_or(0);
                let quality_profile_id = item.get("qualityProfileId").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
                let quality_profile_name = config.radarr.quality_profiles.iter().find(|p| p.id == quality_profile_id).map(|p| p.name.clone()).unwrap_or_default();
                let path = item.get("path").or_else(|| item.get("folderName")).and_then(|v| v.as_str()).unwrap_or("").to_string();

                let mut poster = String::new();
                let mut fanart = String::new();
                if let Some(images) = item.get("images").and_then(|v| v.as_array()) {
                    for img in images {
                        let ctype = img.get("coverType").and_then(|v| v.as_str());
                        let raw_url = img.get("remoteUrl").or_else(|| img.get("url")).and_then(|v| v.as_str()).filter(|s| !s.is_empty());
                        if let Some(u) = raw_url {
                            let resolved = if u.starts_with("http://") || u.starts_with("https://") {
                                u.to_string()
                            } else {
                                let base = url.trim_end_matches('/');
                                let sep = if u.contains('?') { "&" } else { "?" };
                                format!("{}{}{sep}apikey={}", base, u, token)
                            };
                            if ctype == Some("poster") && poster.is_empty() {
                                poster = resolved;
                            } else if (ctype == Some("fanart") || ctype == Some("background")) && fanart.is_empty() {
                                fanart = resolved;
                            }
                        }
                    }
                }
                if poster.contains("/t/p/original/") {
                    poster = poster.replace("/t/p/original/", "/t/p/w500/");
                }
                if poster.contains("/t/p/w1280/") {
                    poster = poster.replace("/t/p/w1280/", "/t/p/w500/");
                }
                if fanart.is_empty() {
                    fanart = poster.clone();
                }

                items.push(ArrMediaItem {
                    service,
                    id,
                    title,
                    year,
                    overview,
                    poster_url: poster,
                    fanart_url: fanart,
                    monitored,
                    has_file,
                    status,
                    size_bytes,
                    quality_profile_id,
                    quality_profile_name,
                    path,
                    episode_count: 0,
                    total_episode_count: 0,
                });
            }
            Ok(items)
        }
        ArrKind::Sonarr => {
            let req_url = format!("{}/api/v3/series", url);
            let resp = arr_request(&req_url, token, "GET", None)?;
            if resp.status != 200 {
                return Err(format!("Sonarr returned HTTP status {}", resp.status));
            }
            let val = serde_json::from_slice::<serde_json::Value>(&resp.body)
                .map_err(|e| format!("Failed to parse JSON: {}", e))?;
            let arr = val.as_array().ok_or_else(|| "Response was not a JSON array".to_string())?;

            let mut items = Vec::with_capacity(arr.len());
            for item in arr {
                let id = item.get("id").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
                let title = item.get("title").and_then(|v| v.as_str()).unwrap_or("").to_string();
                if title.is_empty() { continue; }
                let year = item.get("year").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
                let overview = item.get("overview").and_then(|v| v.as_str()).unwrap_or("").to_string();
                let monitored = item.get("monitored").and_then(|v| v.as_bool()).unwrap_or(false);
                let status = item.get("status").and_then(|v| v.as_str()).unwrap_or("").to_string();
                let quality_profile_id = item.get("qualityProfileId").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
                let quality_profile_name = config.sonarr.quality_profiles.iter().find(|p| p.id == quality_profile_id).map(|p| p.name.clone()).unwrap_or_default();
                let path = item.get("path").and_then(|v| v.as_str()).unwrap_or("").to_string();

                let (has_file, size_bytes, ep_file_count, ep_total_count) = if let Some(stats) = item.get("statistics") {
                    let ep_files = stats.get("episodeFileCount").and_then(|v| v.as_u64()).unwrap_or(0);
                    let ep_total = stats.get("totalEpisodeCount").and_then(|v| v.as_u64()).unwrap_or(0);
                    let size = stats.get("sizeOnDisk").and_then(|v| v.as_u64()).unwrap_or(0);
                    (ep_files > 0, size, ep_files as u32, ep_total as u32)
                } else {
                    (false, 0, 0, 0)
                };

                let mut poster = String::new();
                let mut fanart = String::new();
                if let Some(images) = item.get("images").and_then(|v| v.as_array()) {
                    for img in images {
                        let ctype = img.get("coverType").and_then(|v| v.as_str());
                        let raw_url = img.get("remoteUrl").or_else(|| img.get("url")).and_then(|v| v.as_str()).filter(|s| !s.is_empty());
                        if let Some(u) = raw_url {
                            let resolved = if u.starts_with("http://") || u.starts_with("https://") {
                                u.to_string()
                            } else {
                                let base = url.trim_end_matches('/');
                                let sep = if u.contains('?') { "&" } else { "?" };
                                format!("{}{}{sep}apikey={}", base, u, token)
                            };
                            if ctype == Some("poster") && poster.is_empty() {
                                poster = resolved;
                            } else if (ctype == Some("fanart") || ctype == Some("background")) && fanart.is_empty() {
                                fanart = resolved;
                            }
                        }
                    }
                }
                if poster.contains("/t/p/original/") {
                    poster = poster.replace("/t/p/original/", "/t/p/w500/");
                }
                if poster.contains("/t/p/w1280/") {
                    poster = poster.replace("/t/p/w1280/", "/t/p/w500/");
                }
                if fanart.is_empty() {
                    fanart = poster.clone();
                }

                items.push(ArrMediaItem {
                    service,
                    id,
                    title,
                    year,
                    overview,
                    poster_url: poster,
                    fanart_url: fanart,
                    monitored,
                    has_file,
                    status,
                    size_bytes,
                    quality_profile_id,
                    quality_profile_name,
                    path,
                    episode_count: ep_file_count,
                    total_episode_count: ep_total_count,
                });
            }
            Ok(items)
        }
    }
}

fn toggle_arr_monitored(service: plx_data::integrations::ArrKind, id: u32, monitored: bool) -> Result<bool, String> {
    use plx_data::integrations::{ArrKind, IntegrationsConfig};
    let config = IntegrationsConfig::load();
    let service_cfg = config.service(service);
    if !service_cfg.is_configured() {
        return Err(format!("{} is not configured in Settings", service.name()));
    }
    let url = &service_cfg.url;
    let token = &service_cfg.token;

    let endpoint = match service {
        ArrKind::Radarr => format!("{}/api/v3/movie/{}", url, id),
        ArrKind::Sonarr => format!("{}/api/v3/series/{}", url, id),
    };

    let resp = arr_request(&endpoint, token, "GET", None)?;
    if resp.status != 200 {
        return Err(format!("Failed to retrieve item (HTTP {})", resp.status));
    }

    let mut val = serde_json::from_slice::<serde_json::Value>(&resp.body)
        .map_err(|e| format!("JSON parse error: {}", e))?;

    if let Some(obj) = val.as_object_mut() {
        obj.insert("monitored".to_string(), serde_json::Value::Bool(monitored));
    }

    let body_bytes = serde_json::to_vec(&val).map_err(|e| format!("Serialization error: {}", e))?;
    let put_resp = arr_request(&endpoint, token, "PUT", Some(&body_bytes))?;
    if put_resp.status >= 200 && put_resp.status < 300 {
        Ok(monitored)
    } else {
        Err(format!("Update failed (HTTP {})", put_resp.status))
    }
}

fn trigger_arr_search(service: plx_data::integrations::ArrKind, id: u32) -> Result<String, String> {
    use plx_data::integrations::{ArrKind, IntegrationsConfig};
    let config = IntegrationsConfig::load();
    let service_cfg = config.service(service);
    if !service_cfg.is_configured() {
        return Err(format!("{} is not configured in Settings", service.name()));
    }
    let url = &service_cfg.url;
    let token = &service_cfg.token;

    let (body_val, cmd_name) = match service {
        ArrKind::Radarr => (
            serde_json::json!({ "name": "MoviesSearch", "movieIds": [id] }),
            "Movie Search",
        ),
        ArrKind::Sonarr => (
            serde_json::json!({ "name": "SeriesSearch", "seriesId": id }),
            "Series Search",
        ),
    };

    let endpoint = format!("{}/api/v3/command", url);
    let body_bytes = serde_json::to_vec(&body_val).map_err(|e| format!("Serialization error: {}", e))?;
    let resp = arr_request(&endpoint, token, "POST", Some(&body_bytes))?;
    if resp.status >= 200 && resp.status < 300 {
        Ok(format!("{} initiated", cmd_name))
    } else {
        Err(format!("Failed to initiate search (HTTP {})", resp.status))
    }
}

pub(super) fn execute_marker(command: PreferenceCmd, view: plx_appkit::skip_policy::MarkerPrefsView) {
    if let PreferenceCmd::ChooseMarker { kind, mode, scope, reply } = command {
        if scope == 0 || scope != view.scope || !view.granted(kind) {
            let _ = reply.send(false); return;
        }
        choose_setting(match kind { plx_data::metadata::MarkerKind::Intro => Choice::IntroSkip(mode),
            plx_data::metadata::MarkerKind::Credits => Choice::CreditsSkip(mode) }, reply);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validation_failure_logs_event() {
        let _serial = plx_base::testlock::serial();
        let (_outcome, log) = plx_base::eventlog::with_private_log(|| {
            let res = validate_arr_service(
                plx_data::integrations::ArrKind::Radarr,
                "http://127.0.0.1:1",
                "dummy_token",
            );
            (
                res,
                std::fs::read_to_string(plx_base::eventlog::events_log()).unwrap_or_default(),
            )
        });
        assert!(log.contains("integrations: validating Radarr"), "log was: {log}");
        assert!(log.contains("integrations: request to") && log.contains("failed"), "log was: {log}");
    }
}
