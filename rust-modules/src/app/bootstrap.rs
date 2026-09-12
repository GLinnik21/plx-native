//! Controlled, pre-effect boot inputs. Resource handles never enter this private wire format.
//! Home, Settings and the typed filmography-detail-return scenario have controlled inputs.
//! Other initial domains fail closed at preflight.

use crate::ui::machine::{Canon, LogicalState};
use crate::ui::rec::Recording;
use serde::{Deserialize, Serialize};
pub(crate) const CONTENT_SHAPE: &str = "ContentInitialV1{detail:str,detailsec:u32,detailok:bool,filmography:bool,personcredits:u32,nowan:bool};ContentResourcesV2{admission:Metadata(sid,rk,gen,client)|MetadataCancel(boundary,retired:DetailBatch)|Person(slot,gen,arg,guid,local?,client?,sid?),admitted:bool;result:DetailBatch(seq,req,terminal,Data(key,Option<Detail>)|Dropped(req)|Refused(req))|Person(slot,Mail(gen,Resolve|Media|Profile|Credits|Roles));PersonTerminal:slot+gen+kind-bound;DetailFloats:bits;ContentEffectsV1:complete_nav_store_request_return_memory}";
pub(crate) mod effects;
pub(crate) mod stores;
#[cfg(test)]
mod tests;
#[cfg(test)]
mod attachment_tests;

/// Resource execution is instance-owned. Replay consumes the exact correlated synchronous
/// admission answer (including refusal); only recorded ingress completes admitted work.
pub(crate) struct HomeIo {
    pub replay: bool,
    pub preferences: crate::plex::session::Session,
    pub requests: Vec<serde_json::Value>,
    pub admissions: std::collections::VecDeque<serde_json::Value>,
    pub failure: Option<&'static str>,
    pub profile: std::sync::Arc<crate::plex::session::CurrentProfile>,
}

impl HomeIo {
    fn admit(&mut self, request: serde_json::Value, launch: impl FnOnce() -> bool) -> bool {
        let admitted = if self.replay {
            let Some(mut recorded) = self.admissions.pop_front() else {
                self.failure = Some("missing synchronous admission"); return false;
            };
            let answer = recorded.as_object_mut().and_then(|v| v.remove("admitted")).and_then(|v| v.as_bool());
            if recorded != request || answer.is_none() {
                self.failure = Some("mismatched synchronous admission"); return false;
            }
            answer.unwrap()
        } else { launch() };
        let mut recorded = request;
        recorded["admitted"] = serde_json::json!(admitted);
        self.requests.push(recorded);
        admitted
    }
    pub fn hubs(&mut self, cmd: Option<crate::stores::hubs::HubsCmd>, dt: f32)
        -> crate::stores::StoreOutcome {
        self.hubs_with(cmd, dt, &mut crate::pms::spawn_fetch)
    }
    fn hubs_with(&mut self, cmd: Option<crate::stores::hubs::HubsCmd>, dt: f32,
        launch: &mut dyn FnMut(crate::pms::HubRequest) -> bool) -> crate::stores::StoreOutcome {
        crate::stores::hubs::controlled(cmd, dt, &mut |request| {
            let (epoch, req, sid, client, token_gen) = request.descriptor();
            self.admit(serde_json::json!({"kind":"hubs", "epoch":epoch,
                "req":req, "sid":sid, "client":client, "token_gen":token_gen}), || launch(request))
        })
    }
    pub fn discovery(&mut self) {
        self.discovery_with(&mut crate::browse::execute_discovery);
    }
    pub(crate) fn discovery_with(&mut self, launch: &mut dyn FnMut(crate::browse::DiscoveryRequest) -> bool) {
        crate::browse::controlled_discover(&mut |request| {
            self.admit(request.descriptor(), || launch(request))
        });
    }
}

pub(crate) const ADMISSION_SHAPE: &str = "HomeAdmissionV1{frame:u64,ordered_request:Home|Discovery,epoch:u32,sid:u16,client:u32,token_gen:u32,admitted:bool}";
pub(crate) fn validate_admission(value: &serde_json::Value, client: u32) -> Result<(), &'static str> {
    let object = value.as_object().ok_or("invalid synchronous admission")?;
    let hubs = value["kind"] == "hubs";
    let keys: &[&str] = if hubs { &["kind","epoch","req","sid","client","token_gen","admitted"] }
        else { &["epoch","source","sid","client","token_gen","name","sections","counts","admitted"] };
    if object.len() != keys.len() || keys.iter().any(|key| !object.contains_key(*key))
        || value["admitted"].as_bool().is_none() || value["client"] != client
        || value["token_gen"] != client || value["sid"] != 0 {
        return Err("invalid synchronous admission");
    }
    for key in ["epoch","client","token_gen"] {
        value[key].as_u64().and_then(|n| u32::try_from(n).ok()).ok_or("invalid admission identity")?;
    }
    if hubs { value["req"].as_u64().and_then(|n| u32::try_from(n).ok()).ok_or("invalid admission request")?; }
    else {
        if value["source"] != 0 { return Err("invalid admission source"); }
        value["name"].as_bool().ok_or("invalid admission naming")?;
        let sections = value["sections"].as_bool().ok_or("invalid admission kind")?;
        let counts = value["counts"].as_array().ok_or("invalid admission counts")?;
        if (sections && !counts.is_empty()) || counts.iter().any(|key| key.as_i64().is_none()) {
            return Err("invalid admission counts");
        }
    }
    Ok(())
}

pub(crate) const SHAPE: &str = "ControlledHomeInitV2{version:u32,session:SessionInit,consent:Consent,home:HubsInitialV1,clock_start:u32,entropy:Captured(Option<[u8;16]>)|Seeded(u32),primary_client:u32,automated:bool,settings:Option<root|privacy|legal>,triggers:[str]};HomeEffectsV1{from:MachineId,kind:Fx,payload:complete_supported_payload};OwnedInputV1{ms:u32,dt_us:u32,source:Source,body:InputKind};DiscoveryResultV1{epoch:u32,source:u32,sid:u16,client:u32,token_gen:u32,name:str,what:Sections|Counts}";
#[cfg(test)]
pub(crate) const PRE_SETTINGS_SHAPE: &str = "ControlledHomeInitV1{version:u32,session:SessionInit,consent:Consent,home:HubsInitialV1,clock_start:u32,entropy:Captured(Option<[u8;16]>)|Seeded(u32),primary_client:u32,automated:bool,triggers:[str]};HomeEffectsV1{from:MachineId,kind:Fx,payload:complete_supported_payload};OwnedInputV1{ms:u32,dt_us:u32,source:Source,body:InputKind};DiscoveryResultV1{epoch:u32,source:u32,sid:u16,client:u32,token_gen:u32,name:str,what:Sections|Counts}";

#[derive(Clone, Serialize, Deserialize)]
pub(crate) enum Entropy {
    /// Actual bytes used by normal first-boot minting, or no draw for an existing identity.
    Captured(Option<[u8; 16]>),
    /// Explicit synthetic construction. No OS entropy or household file is consulted.
    Seeded(u32),
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Initial {
    pub version: u32,
    pub session: crate::auth::SessionInit,
    pub consent: crate::telemetry::consent::Consent,
    pub home: crate::pms::initial::Initial,
    pub clock_start: u32,
    pub entropy: Entropy,
    pub primary_client: u32,
    pub automated: bool,
    #[serde(default)]
    pub settings: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content: Option<ContentInitial>,
    pub triggers: Vec<String>,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ContentInitial {
    pub detail: String,
    pub detailsec: u32,
    pub detailok: bool,
    pub filmography: bool,
    pub personcredits: u32,
    pub nowan: bool,
}

impl Initial {
    #[cfg(any(test, feature = "hostsim"))]
    pub(crate) fn synthetic_home(seed: u32, port: u16, settings: Option<String>)
        -> Result<Self, &'static str> {
        if port == 0 { return Err("invalid synthetic port"); }
        let saved = crate::plex::session::Session { client_id:format!("s{seed:08x}"), ..Default::default() };
        let origin = crate::plex::Origin::http("127.0.0.1", i32::from(port));
        let primary = crate::plex::session::ServerRef { address:"127.0.0.1".into(), port:i64::from(port),
            origin_url:origin.base(),token:format!("s{:08x}",seed.wrapping_add(1)),
            tier:Some(crate::plex::probe::Location::Local), ..Default::default() };
        let initial = Self { version:1, session:crate::auth::SessionInit::captured_boot(saved,Some(primary),Vec::new()),
            consent:Default::default(),home:crate::pms::initial::Initial::fresh(),clock_start:0,
            entropy:Entropy::Seeded(seed),primary_client:1,automated:true,settings:settings.clone(),
            content:None, triggers:vec!["plxnative-app-init".into(),"plxnative-rec".into(),
                "plxnative-focus".into(),"plxnative-noidle".into()] };
        let mut initial = initial;
        if settings.is_some() { initial.triggers.push("plxnative-settings".into()); }
        initial.validate()?;
        Ok(initial)
    }
    pub(crate) fn capture_home(host: &str, port: i32) -> Result<(Self, Option<crate::plex::session::DeferredLoad>), &'static str> {
        if crate::dev::flag("app-init") {
            let value = crate::ui::rec::initial_value(&crate::paths::in_runtime_dir("plxnative-app-init"))
                .map_err(|_| "invalid explicit initial input")?;
            return Self::from_value(value).map(|initial| (initial, None));
        }
        let token = crate::dev::scenarios::dev_token();
        let (saved, entropy, deferred) = crate::plex::session::load_capturing_entropy();
        let primary = (!token.is_empty()).then(|| crate::plex::session::ServerRef {
            address: host.into(), port: i64::from(port),
            origin_url: crate::plex::Origin::http(host, port).base(), token,
            tier: Some(crate::plex::probe::configured_tier(host)), ..Default::default()
        });
        let initial = Self { version: 1,
            session: crate::auth::SessionInit::captured_boot(saved, primary, Vec::new()),
            consent: crate::telemetry::capture_initial(), clock_start: 0, entropy: Entropy::Captured(entropy),
            primary_client: crate::plex::Client::capture_generation_seed(),
            automated: crate::dev::any_trigger_present(),
            settings: crate::dev::scenarios::settings_boot_value(),
            content: None,
            home: crate::pms::initial::Initial::capture(),
            triggers: crate::dev::armed_triggers(),
        };
        initial.validate()?;
        Ok((initial, Some(deferred)))
    }
    pub(crate) fn validate(&self) -> Result<(), &'static str> {
        use crate::auth::owner::BootstrapAuthority;
        let s = &self.session;
        if self.version != 1 { return Err("unsupported initial version"); }
        if self.primary_client == 0 { return Err("invalid primary client binding"); }
        if crate::screens::consent::should_show(&self.consent, self.automated) {
            return Err("unsupported initial consent route");
        }
        if !self.home.validate_boot() { return Err("unsupported populated Home initial state"); }
        if s.phase != crate::auth::Phase::Idle || s.epoch != 1 || s.next_req != 0
            || !s.pending.is_empty() || s.pending_commit.is_some() || s.pending_erase.is_some()
            || !s.inbox.is_empty() || s.pump_pending || s.signin_active || s.apply_pending
            || s.active_profile.is_some() || s.profile_scope.0 != 0 {
            return Err("initial state is not a pre-effect boot");
        }
        let BootstrapAuthority::DevPms { primary, extras } = &s.authority else {
            return Err("unsupported replay bootstrap authority");
        };
        let reconstructed = crate::auth::SessionInit::captured_boot(s.persisted.clone(), Some(primary.clone()), extras.clone());
        if serde_json::to_value(&reconstructed).map_err(|_| "invalid initial Session")?
            != serde_json::to_value(s).map_err(|_| "invalid initial Session")? {
            return Err("incoherent initial Session");
        }
        if crate::plex::Origin::parse(&primary.origin_url).is_none() || primary.token.is_empty() || !extras.is_empty() {
            return Err("unsupported Home server binding");
        }
        if s.persisted.client_id.is_empty() { return Err("missing initial identity"); }
        if self.settings.as_deref().is_some_and(|value|
            !matches!(value, "root" | "privacy" | "legal")) {
            return Err("unsupported initial Settings input");
        }
        if self.settings.is_some()
            != self.triggers.iter().any(|trigger| trigger == "plxnative-settings") {
            return Err("incoherent initial Settings input");
        }
        let content_triggers = ["detail", "detailsec", "detailok", "filmography", "personcredits", "nowan"];
        if let Some(content) = &self.content {
            if content.detail != "1001" || content.detailsec != 1 || !content.detailok
                || !content.filmography || content.personcredits != 9 || !content.nowan
                || self.settings.is_some() {
                return Err("unsupported initial content input");
            }
        }
        for name in content_triggers {
            if self.content.is_some() != self.triggers.iter().any(|t| t == &format!("plxnative-{name}")) {
                return Err("incoherent initial content input");
            }
        }
        match self.entropy {
            Entropy::Captured(Some(bytes)) if crate::plex::session::client_id_from_entropy(bytes) != s.persisted.client_id =>
                return Err("initial entropy mismatch"),
            Entropy::Seeded(seed) if format!("s{seed:08x}") != s.persisted.client_id =>
                return Err("initial seed mismatch"),
            _ => {}
        }
        for trigger in &self.triggers {
            if !matches!(trigger.as_str(), "plxnative-rec" | "plxnative-recplay" |
                "plxnative-focus" | "plxnative-noidle" | "plxnative-token" |
                "plxnative-app-init" | "plxnative-settings" | "plxnative-detail" |
                "plxnative-detailsec" | "plxnative-detailok" | "plxnative-filmography" |
                "plxnative-personcredits" | "plxnative-nowan") {
                return Err("unsupported initial developer input");
            }
        }
        Ok(())
    }

    pub(crate) fn from_value(value: serde_json::Value) -> Result<Self, &'static str> {
        let initial: Self = serde_json::from_value(value.clone()).map_err(|_| "invalid controlled initial data")?;
        // Nested legacy DTOs accept unknown fields/defaults; the controlled envelope must not.
        if serde_json::to_value(&initial).map_err(|_| "invalid initial encoding")? != value {
            return Err("noncanonical initial fields");
        }
        initial.validate()?;
        Ok(initial)
    }

    pub(crate) fn decode(value: serde_json::Value, expected_hash: u64) -> Result<Self, &'static str> {
        let initial = Self::from_value(value)?;
        if initial.hash() != expected_hash { return Err("initial hash mismatch"); }
        Ok(initial)
    }
}

impl LogicalState for Initial {
    fn write(&self, c: &mut Canon) {
        c.u32(self.version);
        self.session.write(c);
        self.home.write(c);
        let v = &self.consent;
        c.u32(v.asked_version).bool(v.errors).bool(v.usage);
        c.option(v.install_id.as_ref(), |c, id| { c.str(id); });
        c.option(v.errors_id.as_ref(), |c, id| { c.str(id); });
        c.u32(self.clock_start);
        match self.entropy {
            Entropy::Captured(bytes) => { c.u32(0); c.option(bytes.as_ref(), |c, bytes| { for byte in bytes { c.u32(u32::from(*byte)); } }); }
            Entropy::Seeded(seed) => { c.u32(1).u32(seed); }
        }
        c.u32(self.primary_client).bool(self.automated);
        c.option(self.settings.as_ref(), |c, value| { c.str(value); });
        if let Some(v) = &self.content {
            c.str("ContentInitialV1").str(&v.detail).u32(v.detailsec).bool(v.detailok)
                .bool(v.filmography).u32(v.personcredits).bool(v.nowan);
        }
        c.seq(self.triggers.len());
        for trigger in &self.triggers { c.str(trigger); }
    }
    fn probe(&self, out: &mut String) { out.push_str("controlled=home version=1"); }
}

pub(crate) enum Preflight {
    Live,
    Record,
    Replay { initial: Initial, recording: Recording },
}

impl Preflight {
    /// Must precede Session load/identity mint, telemetry boot and every bootstrap worker.
    pub(crate) fn detect() -> Result<Self, &'static str> {
        let rec = crate::dev::scenarios::rec_trigger()?;
        let replay = crate::dev::scenarios::recplay_trigger()?;
        match (rec, replay) {
            (Some(_), Some(_)) => Err("conflicting recorder modes"),
            (None, Some(path)) => {
                if path.is_empty() { return Err("missing replay directory"); }
                let recording = Recording::load(std::path::Path::new(&path), super::recorder::state_fp())
                    .map_err(|_| "invalid or incompatible recording")?;
                let initial = Initial::decode(recording.header.init_data.clone(), recording.header.init_hash)?;
                super::recorder::validate_controlled(&recording, &initial)?;
                Ok(Self::Replay { initial, recording })
            }
            (Some(option), None) if option.is_empty() => Ok(Self::Record),
            (Some(_), None) => Err("unsupported recorder option"),
            (None, None) => Ok(Self::Live),
        }
    }
    pub(crate) fn replay(&self) -> bool { matches!(self, Self::Replay { .. }) }
    pub(crate) fn controlled(&self) -> bool { !matches!(self, Self::Live) }
}
