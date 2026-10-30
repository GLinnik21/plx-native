//! The `plx_plex` [`Backend`] that answers the app's `Client` calls from a Jellyfin server.
//!
//! The FinNative build installs [`JfBackend`] at boot ([`install`]); from then on every hooked
//! `Client` method asks it first. Until a server is signed in ([`JfBackend::set_client`]) it holds
//! no client and answers every read with `None`, the same "nothing here" a Plex `Client` with no
//! server gives.
//!
//! Shelf keys are this layer's own (`/jf/...`, see [`hub_key`]), so a "see all" on a shelf comes
//! back to [`Backend::hub_items_paged`] here rather than to a PMS path.

use super::auth::ClientIdentity;
use super::favorites;
use super::client::{both, CurlTransport, JfClient, LibraryPage, PlayState, PlaybackReport, Session as SignedIn, HOME_SHELF_SIZE};
use super::live;
use super::paths::{self, SortBy};
use super::playback_info;
use super::transcode;
use super::translate::{self, hubs};
use plx_plex::plex::account::{AudioPreferences, PreferenceError};
use plx_platform::i18n::msg;
use plx_plex::plex::backend::Backend;
use plx_plex::plex::grant::{self, MintRefusal, PlaintextAsk};
use plx_plex::plex::notify::{Notification, NotifyEnd};
use plx_plex::plex::Origin;
use plx_plex::plex::{
    Hub, MediaContainer, Metadata, PageReq, SearchKind, SearchResult, SectionQuery, StreamUrl, TimelineReport,
    TimelineState, TranscodeSpec, Trickplay,
};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex, OnceLock, RwLock};

/// The backend, holding the signed-in server (if any).
#[derive(Default)]
pub struct JfBackend {
    client: RwLock<Option<Arc<JfClient>>>,
    /// Moves each time a client is installed or removed (`Backend::signed_in_generation`).
    generation: AtomicU32,
    /// The server's verdict for the item about to play (`Backend::mde_decision`), read again when
    /// the player asks for its transcode. One playback at a time, so one item.
    verdict: Mutex<Option<(String, playback_info::Verdict)>>,
    /// Held while the saved server's plain-http grant is proved ([`JfBackend::prove_saved_server`]),
    /// so the calls a launch makes at once prove it once.
    proving: Mutex<()>,
    /// Moves after each proof, so a call that waited on [`Self::proving`] knows one just ran.
    proofs: AtomicU32,
    /// Held while a saved sign-in is resumed ([`JfBackend::client`]), so the calls a launch makes
    /// at once resume it once.
    resuming: Mutex<()>,
    /// Moves after each resume, so a call that waited on [`Self::resuming`] knows one just ran.
    resumes: AtomicU32,
    /// Set once the saved user has been checked against the server ([`JfBackend::refresh_user`]).
    user_checked: AtomicBool,
    /// The server's name and the base it answered at, from the launch's proof
    /// ([`JfBackend::prove_saved_server`]) or the last [`Backend::friendly_name`], so the name
    /// costs no second description of the server.
    server_name: Mutex<Option<(String, String)>>,
}

/// The deck: in-progress items, then the next episode of each show being watched that is not
/// already on it (by item or by show).
fn deck_of(mut resume: MediaContainer, next: Option<MediaContainer>) -> MediaContainer {
    for m in next.map(|n| n.metadata).unwrap_or_default() {
        let same_show = |d: &Metadata| !m.grandparent_rating_key.is_empty() && d.grandparent_rating_key == m.grandparent_rating_key;
        if !resume.metadata.iter().any(|d| d.rating_key == m.rating_key || same_show(d)) {
            resume.metadata.push(m);
        }
    }
    resume
}

/// A launch parameter's item id as this backend's rating key: a Jellyfin GUID, dashed or not, in
/// any case, read as its 32 lowercase hex digits ([`super::ids::normalize`]); anything else is no
/// Jellyfin item. A Plex-style numeric key is refused rather than looked up.
fn launch_item_key(raw: &str) -> Option<String> {
    let key = super::ids::normalize(raw);
    (key.len() == 32 && key.bytes().all(|b| b.is_ascii_hexdigit())).then_some(key)
}

/// A handle that stays valid after the backend is installed, for the sign-in flow to hand the
/// signed-in client to.
static INSTANCE: OnceLock<Arc<JfBackend>> = OnceLock::new();

/// Install the Jellyfin backend for this run. Returns the handle sign-in uses; calling it again
/// returns the same one.
pub fn install() -> Arc<JfBackend> {
    INSTANCE
        .get_or_init(|| {
            let b = Arc::new(JfBackend::default());
            plx_plex::plex::backend::install(Box::new(Shared(b.clone())));
            b
        })
        .clone()
}

/// The installed backend, if [`install`] ran.
pub fn instance() -> Option<Arc<JfBackend>> {
    INSTANCE.get().cloned()
}

/// **The server edited the signed-in user** (its `UserUpdated` message, [`live`](super::live)):
/// read the user again off the feed's thread, so a name or avatar changed while the app is open
/// shows without a relaunch ([`JfBackend::refresh_user`]).
pub fn refresh_signed_in_user() {
    let saved = plx_plex::plex::session::peek();
    if saved.pms_token().is_empty() {
        return;
    }
    if let Some(c) = instance().and_then(|b| b.installed()) {
        c.forget_user();
    }
    refresh_user_off_thread(saved);
}

/// [`JfBackend::refresh_user`] for `saved`'s sign-in, on a thread of its own.
fn refresh_user_off_thread(saved: Arc<plx_plex::plex::session::Session>) {
    let spawned = plx_base::task::spawn_small("jellyfin-user-refresh", move || {
        if let Some(b) = instance() {
            b.refresh_user(&saved);
        }
    });
    if !spawned {
        plx_base::eventlog::log("jellyfin: the user was not checked (no thread)");
    }
}

/// What `plx_plex` holds: the shared backend behind an `Arc`, so sign-in can still reach it.
struct Shared(Arc<JfBackend>);

impl JfBackend {
    /// Use `client` (signed in) for every call from now on, or `None` after sign-out.
    pub fn set_client(&self, client: Option<JfClient>) {
        *self.client.write().unwrap_or_else(|p| p.into_inner()) = client.map(Arc::new);
        self.generation.fetch_add(1, Ordering::Release);
    }

    /// The installed client, if any, without making one: for the frame thread, which must not
    /// wait on the network ([`Self::client`] may resume a saved sign-in).
    fn installed(&self) -> Option<Arc<JfClient>> {
        self.client.read().unwrap_or_else(|p| p.into_inner()).clone()
    }

    /// The current client. Cloned out of the lock so a slow request never holds it.
    ///
    /// With none set yet (a boot that resumed a saved sign-in), one is made from the saved
    /// session: the server it names, the token it holds and the user saved beside it
    /// ([`USER_KEY`]), with no request. A sign-in saved before the user was ([`saved_user`] finds
    /// none) asks the server whose the token is, gives `None` if it is no longer accepted, and
    /// saves the answer for the next launch. Calls that arrive while a resume runs wait for it
    /// rather than resume the same sign-in again; if it failed, they fail with it.
    pub fn client(&self) -> Option<Arc<JfClient>> {
        let saved = plx_plex::plex::session::peek();
        let token = saved.pms_token();
        if !token.is_empty() {
            self.prove_saved_server(&saved);
        }
        let current = || {
            self.installed().filter(|c| token.is_empty() || c.session().is_some_and(|s| s.token == token))
        };
        if let Some(c) = current() {
            return Some(c);
        }
        if token.is_empty() {
            return None;
        }
        let seen = self.resumes.load(Ordering::Acquire);
        let _one = self.resuming.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(c) = current() {
            return Some(c);
        }
        if self.resumes.load(Ordering::Acquire) != seen {
            return None;
        }
        let from_saved = saved_user(&saved.server, token).is_some();
        let resumed = resume(&saved, token);
        self.resumes.fetch_add(1, Ordering::Release);
        let c = Arc::new(resumed?);
        *self.client.write().unwrap_or_else(|p| p.into_inner()) = Some(c.clone());
        self.generation.fetch_add(1, Ordering::Release);
        if from_saved && !self.user_checked.swap(true, Ordering::AcqRel) {
            refresh_user_off_thread(saved.clone());
        }
        Some(c)
    }

    /// **Bring the saved user up to date** after a launch resumed from the record saved beside the
    /// sign-in, which names the user as of the sign-in: a name or avatar changed on the server
    /// since would show stale for good. Asks the server whose the token is, off the launch's
    /// path, through the installed client's read of the user, which Home's settings share
    /// ([`JfClient::user_value`]); when the name or avatar tag differ, saves the new record and
    /// swaps in a client that carries them (the generation moves, so what shows the user reads it
    /// again). A server that does not answer leaves the saved record; the next launch asks again.
    fn refresh_user(&self, saved: &plx_plex::plex::session::Session) {
        let token = saved.pms_token();
        let Some(current) = self.installed().filter(|c| c.session().is_some_and(|s| s.token == token)) else { return };
        let Ok(value) = current.user_value(super::client::FRESH) else { return };
        let Some(user) = SignedIn::of_value(&value, token) else { return };
        let machine_id = saved.server.machine_id.clone();
        let record = user_record(&user);
        if with_user(saved, &machine_id, &record).is_none() {
            return;
        }
        let _ = plx_plex::plex::session::queue_update(move |current| with_user(current, &machine_id, &record));
        let mut slot = self.client.write().unwrap_or_else(|p| p.into_inner());
        if slot.as_ref().is_some_and(|s| s.session().is_some_and(|s| s.token == token && s.user_id == user.user_id)) {
            let mut c = saved_client(saved);
            c.resume_saved(user);
            c.remember_user(value);
            *slot = Some(Arc::new(c));
            self.generation.fetch_add(1, Ordering::Release);
        }
    }

    /// **Grant the saved server's plain-http origin before a request needs it**, in a build that
    /// sends a credential over plain http only on a live grant (`plx_plex::plex::grant`).
    ///
    /// A grant is never stored, and every one ends when the app comes to the foreground, which a
    /// launch does too. Until the session's endpoint re-proof mints it again (a round trip to the
    /// server), every request carrying the token is refused at once ([`CurlTransport`]), so Home
    /// read a launch's first fetch as "can't reach your server" and recovered on its retry. The
    /// request now asks first, on the same evidence the re-proof uses ([`prove_grant`]). Calls
    /// that arrive while a proof runs wait for it and do not prove again; a server the person
    /// turned plain http off for is not asked at all.
    fn prove_saved_server(&self, saved: &plx_plex::plex::session::Session) {
        let base = saved_base(&saved.server);
        let Some(origin) = Origin::parse(&base) else { return };
        let machine_id = super::ids::normalize(&saved.server.machine_id);
        let allowed = || grant::credential_allowed_for(&machine_id, &origin);
        if allowed() || PlaintextAsk::capture(&saved.account_token).refused(&machine_id) {
            return;
        }
        let seen = self.proofs.load(Ordering::Acquire);
        let _one = self.proving.lock().unwrap_or_else(|p| p.into_inner());
        if allowed() || self.proofs.load(Ordering::Acquire) != seen {
            return;
        }
        let proved = prove_grant(
            &machine_id,
            || {
                let info = curl_client(&base, &saved.client_id).probe().ok()?;
                self.remember_name(&base, &info.server_name);
                Some(super::ids::normalize(&info.id))
            },
            || PlaintextAsk::capture(&saved.account_token).settle_vouched(&machine_id, &origin),
        );
        self.proofs.fetch_add(1, Ordering::Release);
        if let Err(why) = proved {
            plx_base::eventlog::log(&format!("jellyfin: the saved server was not granted on plain http ({why:?})"));
        }
    }

    /// The signed-in client and the plan for a transcode of `spec`'s item on this device. When the server already gave its verdict for this item and the
    /// viewer chose no quality ceiling, its encode choices are the plan's, and its own stream is
    /// used whenever the plan describes exactly what that stream carries.
    fn transcode_plan(&self, spec: &TranscodeSpec<'_>) -> Option<(Arc<JfClient>, transcode::Plan)> {
        let c = self.client()?;
        let m = c.metadata_recent(spec.rating_key)?;
        let held = self.verdict_for(spec.rating_key);
        let encoders = held.as_ref().map_or(playback_info::Encoders::ALL, |v| v.encoders);
        let server = held.filter(|v| !v.direct_play && spec.contract.ceiling.is_none());
        let video_copy = plx_plex::plex::direct_play_video_codecs(spec.caps);
        let dev = transcode::Device {
            video_copy: &video_copy,
            audio_target: audio_encode_target(spec.caps),
            audio_ok: &|codec: &str, channels: i64, rate: i64| plx_plex::plex::is_dp_audio_track_with(spec.caps, codec, channels, rate),
            force_video_encode: server.as_ref().is_some_and(|v| v.video_encode),
            force_audio_encode: server.as_ref().is_some_and(|v| v.audio_encode),
            encoders,
        };
        let mut p = transcode::plan(m, spec, &dev)?;
        if let Some(v) = server {
            if p.copy_video == !v.video_encode && p.copy_audio == !v.audio_encode {
                p.server_url = v.url;
            }
        }
        Some((c, p))
    }

    fn remember_name(&self, base: &str, name: &str) {
        *self.server_name.lock().unwrap_or_else(|p| p.into_inner()) = Some((base.to_owned(), name.to_owned()));
    }

    /// The server's name as it answered at `base`, if it has.
    fn known_name(&self, base: &str) -> Option<String> {
        let held = self.server_name.lock().unwrap_or_else(|p| p.into_inner());
        held.as_ref().filter(|(b, _)| b == base).map(|(_, n)| n.clone())
    }

    fn remember_verdict(&self, rating_key: &str, v: Option<playback_info::Verdict>) {
        *self.verdict.lock().unwrap_or_else(|p| p.into_inner()) = v.map(|v| (rating_key.to_string(), v));
    }

    fn verdict_for(&self, rating_key: &str) -> Option<playback_info::Verdict> {
        let held = self.verdict.lock().unwrap_or_else(|p| p.into_inner());
        held.as_ref().filter(|(rk, _)| rk == rating_key).map(|(_, v)| v.clone())
    }
}

/// A client for the saved sign-in (see [`JfBackend::client`]). Section keys are not listed here:
/// Home's first requests list them anyway, and a page that needs one first lists them itself.
fn resume(saved: &plx_plex::plex::session::Session, token: &str) -> Option<JfClient> {
    let mut c = saved_client(saved);
    if let Some(user) = saved_user(&saved.server, token) {
        c.resume_saved(user);
        return Some(c);
    }
    let user = c.who_is(token).ok()?;
    let machine_id = saved.server.machine_id.clone();
    let record = user_record(&user);
    if with_user(saved, &machine_id, &record).is_some() {
        let _ = plx_plex::plex::session::queue_update(move |current| with_user(current, &machine_id, &record));
    }
    Some(c)
}

/// The key the signed-in user is saved under, beside the session's server record: its id, name
/// and avatar tag ([`user_record`]), so a launch resumes without asking the server who it is.
pub const USER_KEY: &str = "jellyfinUser";

/// What [`USER_KEY`] holds for `user`.
pub fn user_record(user: &SignedIn) -> serde_json::Value {
    serde_json::json!({ "id": user.user_id, "name": user.user_name, "avatar": user.avatar_tag })
}

/// The user saved beside `server`, signed in with `token`; `None` for a sign-in saved before the
/// user was, or a record without an id.
fn saved_user(server: &plx_plex::plex::session::ServerRef, token: &str) -> Option<SignedIn> {
    let user = SavedUser::read(&server.extensions)?;
    Some(SignedIn {
        user_id: user.id,
        user_name: user.name,
        avatar_tag: Some(user.avatar).filter(|a| !a.is_empty()),
        token: token.to_owned(),
        server_id: super::ids::normalize(&server.machine_id),
    })
}

/// The user a [`USER_KEY`] record names: its id (normalized, never empty), name and avatar tag
/// (empty for none).
pub struct SavedUser {
    pub id: String,
    pub name: String,
    pub avatar: String,
}

impl SavedUser {
    /// The user saved under [`USER_KEY`] in `ext`.
    pub fn read(ext: &plx_plex::plex::session::OpaqueExtensions) -> Option<Self> {
        Self::of(ext.0.get(USER_KEY)?)
    }

    /// The user `record` ([`user_record`]) names; `None` for a record without an id.
    pub fn of(record: &serde_json::Value) -> Option<Self> {
        let text = |key: &str| record.get(key).and_then(|v| v.as_str()).unwrap_or_default();
        let id = super::ids::normalize(text("id"));
        (!id.is_empty()).then(|| Self { id, name: text("name").to_owned(), avatar: text("avatar").to_owned() })
    }

    /// The avatar's image path, empty for a user without one.
    pub fn thumb(&self) -> String {
        if self.avatar.is_empty() { String::new() } else { paths::user_image(&self.id, &self.avatar) }
    }
}

/// Save `record` ([`user_record`]) as the user in `ext`.
pub fn save_user(ext: &mut plx_plex::plex::session::OpaqueExtensions, record: &serde_json::Value) {
    ext.0.insert(USER_KEY.into(), record.clone());
}

/// `session` with `record` saved as the user of `machine_id`: on the server record and on that
/// server's source, and in the copies the screens read (the seated user, the who's-watching
/// tiles, the saved profiles), so a changed name or avatar shows everywhere it is drawn. `None`
/// when `machine_id` is no longer the signed-in server, or nothing would change.
fn with_user(session: &plx_plex::plex::session::Session, machine_id: &str, record: &serde_json::Value)
    -> Option<plx_plex::plex::session::Session> {
    if machine_id.is_empty() || session.server.machine_id != machine_id {
        return None;
    }
    let user = SavedUser::of(record)?;
    let (id, name, thumb) = (user.id.as_str(), user.name.as_str(), user.thumb());
    let mut next = session.clone();
    save_user(&mut next.server.extensions, record);
    for source in next.sources.iter_mut().filter(|s| s.machine_id == machine_id) {
        save_user(&mut source.extensions, record);
    }
    if next.user.uuid == id {
        next.user.title = name.to_owned();
        next.user.thumb.clone_from(&thumb);
    }
    for tile in next.home_users.iter_mut().filter(|u| u.uuid == id) {
        tile.title = name.to_owned();
        if !tile.thumb.is_empty() || next.user.uuid == id {
            tile.thumb.clone_from(&thumb);
        }
    }
    for profile in next.profiles.iter_mut().filter(|p| p.uuid == id) {
        profile.user.title = name.to_owned();
        profile.user.thumb.clone_from(&thumb);
        if profile.server.machine_id == machine_id {
            save_user(&mut profile.server.extensions, record);
        }
        for source in profile.sources.iter_mut().filter(|s| s.machine_id == machine_id) {
            save_user(&mut source.extensions, record);
        }
    }
    (serde_json::to_value(&next).ok()? != serde_json::to_value(session).ok()?).then_some(next)
}

/// Why [`prove_grant`] granted nothing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Unproved {
    /// Nothing answered the tokenless description.
    NoAnswer,
    /// Another server answers at that address.
    OtherServer,
    /// The grant's own refusal.
    Refused(MintRefusal),
}

/// Grant `machine_id`'s plain-http origin on the server's own word: `probe` asks it, without a
/// credential, who it is, and `mint` vouches for it (`PlaintextAsk::settle_vouched`, under a
/// capture taken as it is called) when it answers as `machine_id`. A mint refused as stale (the
/// network generation moved between the capture and the mint, as a launch's foreground does) is
/// proved once more from the probe, so the answer it rests on comes from the network as it is now.
fn prove_grant(
    machine_id: &str,
    mut probe: impl FnMut() -> Option<String>,
    mut mint: impl FnMut() -> Result<(), MintRefusal>,
) -> Result<(), Unproved> {
    let mut again = true;
    loop {
        match probe() {
            None => return Err(Unproved::NoAnswer),
            Some(id) if id != machine_id => return Err(Unproved::OtherServer),
            Some(_) => {}
        }
        match mint() {
            Err(MintRefusal::Stale) if std::mem::take(&mut again) => {}
            other => return other.map_err(Unproved::Refused),
        }
    }
}

/// The device as the server's profile describes it: the capability table the player's own
/// direct-play gate reads, every video codec it lists (h264, hevc, vp9, av1) with their rows.
fn this_device(caps: &plx_platform::devcaps::Caps) -> playback_info::Device<'_> {
    playback_info::Device {
        video: plx_plex::plex::direct_play_video_codecs(caps),
        max_w: caps.hevc_max.0,
        max_h: caps.hevc_max.1,
        rows: vec![("vp9", caps.vp9_row.0, caps.vp9_row.1), ("av1", caps.av1_row.0, caps.av1_row.1)],
        vp9_bit_depth: caps.vp9_bit_depth,
        audio: &caps.audio,
        converted_audio: if plx_platform::devcaps::tv_audio_conversion() { playback_info::CONVERTED_AUDIO } else { "" },
        audio_target: audio_encode_target(caps),
    }
}

/// The audio codec a server encodes to when a track cannot be copied: the head of the Plex
/// profile's `audioCodec` chain (ac3 first, then the rest of what the TV decodes).
fn audio_encode_target(caps: &plx_platform::devcaps::Caps) -> &'static str {
    ["ac3", "eac3", "aac", "dts"].into_iter().find(|c| caps.audio_has(c)).unwrap_or("aac")
}

/// How FinNative names itself to the server (the Dashboard's device list).
pub fn identity(device_id: &str) -> ClientIdentity {
    ClientIdentity {
        client: "FinNative".into(),
        device: "LG TV".into(),
        device_id: if device_id.is_empty() { "finnative".into() } else { device_id.into() },
        version: plx_plex::plex::identity::version().into(),
    }
}

/// A client for the server at `base`, over curl, named as this install (`client_id`).
pub fn curl_client(base: &str, client_id: &str) -> JfClient {
    JfClient::new(base, identity(client_id), Box::new(CurlTransport))
}

/// What the sign-in screen shows as the place to enter the Quick Connect code (the server's
/// address), set by the sign-in flow while its code is up.
static LINK: Mutex<String> = Mutex::new(String::new());

pub fn set_link_address(address: &str) {
    *LINK.lock().unwrap_or_else(|p| p.into_inner()) = address.to_string();
}

pub fn link_address() -> String {
    LINK.lock().unwrap_or_else(|p| p.into_inner()).clone()
}

/// The key the server's whole base URL is saved under, beside the session's server record.
///
/// The record's own origin is scheme, host and port only, so a server behind a reverse-proxy path
/// (`https://media.example/jellyfin`) would come back from a restart without its `/jellyfin`.
pub const BASE_KEY: &str = "jellyfinBase";

/// The server's base URL as signed in: the saved [`BASE_KEY`], else the record's origin (a sign-in
/// saved before the key existed, which was always at the root).
pub fn saved_base(server: &plx_plex::plex::session::ServerRef) -> String {
    base_of(&server.extensions, &server.origin())
}

/// [`saved_base`] for any saved record: its [`BASE_KEY`] in `ext`, else `origin`'s base.
pub fn base_of(ext: &plx_plex::plex::session::OpaqueExtensions, origin: &Origin) -> String {
    match ext.0.get(BASE_KEY).and_then(|v| v.as_str()) {
        Some(base) if !base.is_empty() => base.to_string(),
        _ => origin.base(),
    }
}

/// A client for the saved session's server, with no user yet.
fn saved_client(saved: &plx_plex::plex::session::Session) -> JfClient {
    curl_client(&saved_base(&saved.server), &saved.client_id)
}

/// How the next sign-in reaches its user: Quick Connect, or a username and password.
#[derive(Clone, PartialEq, Eq)]
pub enum SignInMethod {
    QuickConnect,
    Password { username: String, password: String },
}

impl std::fmt::Debug for SignInMethod {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Never the password, and not the username either: both are the person's.
        match self {
            Self::QuickConnect => f.write_str("QuickConnect"),
            Self::Password { .. } => f.write_str("Password"),
        }
    }
}

/// What the sign-in form asked for: the server it typed (empty means find one on the network) and
/// how to sign in to it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SignInRequest {
    pub server: String,
    pub method: SignInMethod,
}

/// The form's request, waiting for the sign-in it starts to take it. Held here rather than in the
/// session's work description because that description is serialized, and a password must never be.
static REQUEST: Mutex<Option<SignInRequest>> = Mutex::new(None);

/// Hand the next sign-in what the form asked for. It is taken once ([`take_sign_in_request`]).
pub fn request_sign_in(request: SignInRequest) {
    *REQUEST.lock().unwrap_or_else(|p| p.into_inner()) = Some(request);
}

/// The form's request, if one is waiting; the sign-in that takes it is the only one that sees it.
pub fn take_sign_in_request() -> Option<SignInRequest> {
    REQUEST.lock().unwrap_or_else(|p| p.into_inner()).take()
}

/// The server the last sign-in found or was given, as a person would type it, for the form to
/// start from.
static SERVER_HINT: Mutex<String> = Mutex::new(String::new());

pub fn set_server_hint(address: &str) {
    *SERVER_HINT.lock().unwrap_or_else(|p| p.into_inner()) = address.to_string();
}

pub fn server_hint() -> String {
    SERVER_HINT.lock().unwrap_or_else(|p| p.into_inner()).clone()
}

/// A base URL as a person types it, and as [`super::client::normalize_base`] reads it back:
/// `http://192.168.1.20:8096` is `192.168.1.20:8096`, and `https://media.example:443/jellyfin` is
/// `https://media.example/jellyfin`. An `https` address keeps its scheme, since without one an
/// address is read as plain `http`; a plain `http` address keeps its port, since without one it is
/// read as Jellyfin's 8096.
pub fn display_address(base: &str) -> String {
    match base.split_once("://") {
        Some(("http", rest)) => rest.to_string(),
        Some(("https", rest)) => {
            let (authority, path) = rest.find('/').map_or((rest, ""), |i| (&rest[..i], &rest[i..]));
            format!("https://{}{path}", authority.strip_suffix(":443").unwrap_or(authority))
        }
        _ => base.to_string(),
    }
}

/// Shelf keys: `/jf/deck`, `/jf/resume`, `/jf/nextup`, `/jf/latest/{section}`, `/jf/resume/{section}`, or an
/// item's own children key (`/jf/items/{id}/children`).
#[derive(Debug, PartialEq, Eq)]
pub enum HubKey<'a> {
    /// Continue Watching: Resume, then Next Up.
    Deck,
    Resume,
    NextUp,
    Favorites,
    ResumeIn(i64),
    Latest(i64),
    Children(&'a str),
}

pub fn hub_key(key: &str) -> Option<HubKey<'_>> {
    let key = key.split('?').next().unwrap_or(key);
    if let Some(rest) = key.strip_prefix(translate::ITEM_KEY_PREFIX) {
        return rest.strip_suffix("/children").map(HubKey::Children);
    }
    match key.trim_end_matches('/').split('/').collect::<Vec<_>>()[..] {
        ["", "jf", "deck"] => Some(HubKey::Deck),
        ["", "jf", "resume"] => Some(HubKey::Resume),
        ["", "jf", "nextup"] => Some(HubKey::NextUp),
        ["", "jf", "favorites"] => Some(HubKey::Favorites),
        ["", "jf", "resume", k] => k.parse().ok().map(HubKey::ResumeIn),
        ["", "jf", "latest", k] => k.parse().ok().map(HubKey::Latest),
        _ => None,
    }
}

/// A Plex sort value (`titleSort`, `addedAt:desc`, ...) as a Jellyfin sort and direction. An
/// empty or unknown sort is by name, ascending.
pub fn sort_of(sort: &str) -> (SortBy, bool) {
    let (field, desc) = match sort.split_once(':') {
        Some((f, d)) => (f, d == "desc"),
        None => (sort, false),
    };
    let by = match field {
        "addedAt" => SortBy::DateCreated,
        "originallyAvailableAt" | "year" => SortBy::PremiereDate,
        "rating" | "audienceRating" | "userRating" => SortBy::CommunityRating,
        "duration" => SortBy::Runtime,
        "lastViewedAt" => SortBy::DatePlayed,
        "random" => SortBy::Random,
        _ => SortBy::Title,
    };
    (by, desc)
}

/// The Jellyfin item types a library's grid lists: the library's own (`Movie`, `Series`), or the
/// listing its TYPE menu chose by Plex `type=` number (3 seasons, 4 episodes, 18 collections).
fn grid_types(kind: &str, plex_type: Option<&str>) -> &'static str {
    match (plex_type, kind) {
        (Some("3"), _) => "Season",
        (Some("4"), _) => "Episode",
        (Some("18"), _) => "BoxSet",
        (_, "show") => "Series",
        _ => "Movie",
    }
}

/// Keep `[start, start + size)` of an answer the server could not page itself.
fn window(mut c: MediaContainer, start: i64, size: i64) -> MediaContainer {
    let start = start.max(0) as usize;
    let total = c.metadata.len();
    c.metadata = c.metadata.into_iter().skip(start).take(if size > 0 { size as usize } else { usize::MAX }).collect();
    c.size = c.metadata.len() as i64;
    c.total_size = total as i64;
    c.offset = start as i64;
    c
}

impl Backend for Shared {
    /// What the server calls itself (Dashboard → General → Server name).
    fn friendly_name(&self) -> Option<String> {
        let c = self.0.client()?;
        let name = self.0.known_name(c.base()).or_else(|| {
            let name = c.probe().ok()?.server_name;
            self.0.remember_name(c.base(), &name);
            Some(name)
        })?;
        Some(name).filter(|n| !n.is_empty())
    }
    fn signed_in_user(&self) -> Option<(String, String)> {
        let c = self.0.installed()?;
        let s = c.session()?;
        let avatar = s.avatar_tag.as_deref().map(|t| paths::user_image(&s.user_id, t)).unwrap_or_default();
        Some((s.user_name.clone(), avatar)).filter(|(name, _)| !name.is_empty())
    }
    fn signed_in_generation(&self) -> u32 {
        self.0.generation.load(Ordering::Acquire)
    }
    fn machine_identity(&self) -> Option<String> {
        Some(self.0.client()?.session()?.server_id.clone())
    }

    // ---- libraries and shelves ----
    fn sections(&self) -> Option<MediaContainer> {
        // Home lists them too, at the same moment: one listing answers both.
        self.0.client()?.libraries_recent(super::client::FRESH)
    }
    fn home_hubs(&self, _count: i64) -> Option<MediaContainer> {
        self.0.client()?.home_hubs()
    }
    fn home_hubs_page(&self, count: i64, req: PageReq) -> Option<MediaContainer> {
        // Every home shelf comes in the first page; there is no second.
        if req.start > 0 {
            return Some(MediaContainer::default());
        }
        self.home_hubs(count)
    }
    fn library_hubs(&self, section_key: i64, count: i64) -> Option<MediaContainer> {
        let c = self.0.client()?;
        let kind = c.library_kind(section_key).unwrap_or_default();
        let size = if count > 0 { count } else { HOME_SHELF_SIZE };
        // Both shelves asked for at once.
        let (resume, latest) = both(|| c.continue_watching_in(section_key, 0, size), || c.latest(section_key, size));
        let latest = latest.flatten();
        let mut out = Vec::new();
        if let Some(items) = resume {
            out.push(translate::hub(
                &format!("{kind}.inprogress.{section_key}"),
                msg::browse_home_continue_watching(),
                &format!("/jf/resume/{section_key}"),
                items,
            ));
        }
        if let Some(items) = latest {
            out.push(translate::hub(
                &format!("{kind}.recentlyadded.{section_key}"),
                msg::browse_library_hub_recently_added(),
                &format!("/jf/latest/{section_key}"),
                items,
            ));
        }
        Some(hubs(out))
    }
    fn library_hubs_page(&self, section_key: i64, count: i64, req: PageReq) -> Option<MediaContainer> {
        if req.start > 0 {
            return Some(MediaContainer::default());
        }
        self.library_hubs(section_key, count)
    }
    fn hub_items_paged(&self, key: &str, start: i64, size: i64) -> Option<MediaContainer> {
        let c = self.0.client()?;
        match hub_key(key)? {
            HubKey::Resume => c.continue_watching(start, size),
            HubKey::Deck => self.continue_watching_page(start, size),
            HubKey::NextUp => c.next_up(start, size),
            HubKey::Favorites => c.favorites(start, size),
            HubKey::ResumeIn(k) => c.continue_watching_in(k, start, size),
            // `/Items/Latest` has no offset: ask for the whole window's end and cut it here.
            HubKey::Latest(k) => c.latest(k, start + size).map(|m| window(m, start, size)),
            HubKey::Children(id) => c.children_of(id, start, size),
        }
    }
    fn continue_watching(&self, count: i64) -> Option<MediaContainer> {
        let deck = self.continue_watching_page(0, count)?;
        Some(hubs(vec![translate::hub("home.continue", msg::browse_home_continue_watching(), "/jf/deck", deck)]))
    }
    fn continue_watching_page(&self, start: i64, size: i64) -> Option<MediaContainer> {
        // Plex's deck is in-progress items followed by the next episode of each show being
        // watched; Jellyfin keeps those as Resume and Next Up. Both are short lists, so the deck is
        // their union, cut to the window here.
        let c = self.0.client()?;
        let (resume, next) = both(|| c.continue_watching(0, start + size), || c.next_up(0, start + size));
        let deck = deck_of(resume?, next.flatten());
        Some(window(deck, start, size))
    }
    /// The live-update feed: the server's WebSocket ([`live`]).
    fn notifications(&self, _off: &plx_base::task::OffFrame, cancel: &AtomicBool, on: &mut dyn FnMut(Notification))
        -> NotifyEnd {
        match self.0.client().and_then(|c| c.socket_target()) {
            Some(target) => live::open(&target, cancel, on),
            None => NotifyEnd::Refused { status: 0 },
        }
    }
    fn search_subtitles(&self, rating_key: &str, language: &str) -> Option<MediaContainer> {
        self.0.client()?.subtitle_candidates(rating_key, language)
    }
    fn add_subtitle(&self, rating_key: &str, candidate_key: &str) -> bool {
        self.0.client().is_some_and(|c| c.download_subtitle(rating_key, candidate_key))
    }
    fn remove_from_continue_watching(&self, rating_key: &str) -> bool {
        let Some(c) = self.0.client() else { return false };
        // One light read for the one thing it needs: whether the item is an episode, which Next
        // Up would bring back.
        let is_episode = c.items_by_id(&[rating_key]).is_some_and(|m| m.metadata.first().is_some_and(|m| m.kind == "episode"));
        c.leave_continue_watching(rating_key, is_episode)
    }
    fn section_items_query(&self, q: &SectionQuery<'_>) -> Option<MediaContainer> {
        let c = self.0.client()?;
        let kind = c.library_kind(q.section_key).unwrap_or_default();
        let (sort_by, descending) = sort_of(q.sort);
        let filter = |key: &str| q.filters.iter().find(|(k, _)| k == key).map(|(_, v)| v.as_str());
        let unwatched_only = [filter("unwatched"), filter("unwatchedLeaves")].contains(&Some("1"));
        let starts_with = filter("firstCharacter");
        c.library_items(
            q.section_key,
            &LibraryPage {
                item_types: grid_types(&kind, filter("type")),
                sort_by,
                descending,
                unwatched_only,
                starts_with,
                genre_id: filter("genre"),
                start: q.start,
                size: q.size,
            },
        )
    }
    fn section_directory(&self, section_key: i64, directory: &str, filters: &[(String, String)]) -> Option<MediaContainer> {
        if directory != "genre" {
            return None;
        }
        let c = self.0.client()?;
        let kind = c.library_kind(section_key).unwrap_or_default();
        let wire = filters.iter().find(|(k, _)| k == "type").map(|(_, v)| v.as_str());
        c.library_genres(section_key, grid_types(&kind, wire))
    }
    fn search(&self, query: &str, limit: i64, _section_id: i64) -> Option<MediaContainer> {
        let c = self.0.client()?;
        let kinds = [
            ("movie", msg::browse_kind_movies(), "Movie"),
            ("show", msg::browse_kind_shows(), "Series"),
            ("episode", msg::browse_kind_episodes(), "Episode"),
        ];
        // Every kind, and the people, asked for at once.
        let (found, people) = std::thread::scope(|s| {
            let asked: Vec<_> = kinds.iter().map(|&(_, _, types)| s.spawn(|| c.search(query, types, 0, limit))).collect();
            let people = c.people_hub(query, limit);
            (asked.into_iter().map(|h| h.join().ok().flatten()).collect::<Vec<_>>(), people)
        });
        let mut out: Vec<Hub> = kinds.iter().zip(found)
            .filter_map(|(&(id, title, _), items)| Some(translate::hub(id, title, "", items?)))
            .collect();
        out.extend(people);
        Some(hubs(out))
    }
    fn search_typed(&self, query: &str, kind: SearchKind, req: PageReq) -> Option<MediaContainer> {
        let types = match kind {
            SearchKind::Movies => "Movie",
            SearchKind::Tv => "Series,Episode",
            SearchKind::People => return self.0.client()?.search_people(query, req.start as i64, req.size as i64),
        };
        let mut c = self.0.client()?.search(query, types, req.start as i64, req.size as i64)?;
        c.search_result = std::mem::take(&mut c.metadata)
            .into_iter()
            .map(|m| SearchResult { metadata: Some(m), directory: None })
            .collect();
        Some(c)
    }

    // ---- items ----
    /// A Jellyfin item id is a GUID: the rating keys this backend hands out are its 32 hex
    /// digits, lowercase, without dashes, and a launcher may spell it either way.
    fn item_key(&self, raw: &str) -> Option<String> {
        launch_item_key(raw)
    }
    fn metadata(&self, rating_key: &str) -> Option<Metadata> {
        self.0.client()?.metadata(rating_key)
    }
    fn metadata_many(&self, rating_keys: &[&str]) -> Option<MediaContainer> {
        self.0.client()?.items_by_id(rating_keys)
    }
    fn children_paged(&self, rating_key: &str, start: i64, size: i64) -> Option<MediaContainer> {
        self.0.client()?.children_of(rating_key, start, size)
    }
    fn extras(&self, rating_key: &str) -> Option<MediaContainer> {
        self.0.client()?.extras(rating_key)
    }
    fn related(&self, rating_key: &str) -> Option<MediaContainer> {
        let items = self.0.client()?.similar(rating_key, HOME_SHELF_SIZE)?;
        Some(hubs(vec![translate::hub("similar", msg::browse_jf_more_like_this(), "", items)]))
    }
    fn favorite_mark(&self, rating_key: &str) -> Option<bool> {
        Some(favorites::state(rating_key))
    }
    fn set_favorite_mark(&self, rating_key: &str, favourite: bool) {
        let shared = self.0.clone();
        favorites::set(rating_key, favourite, move |rk, fav| shared.client().is_some_and(|c| c.set_favorite(rk, fav)));
    }
    fn take_favorite_landed(&self) -> bool {
        favorites::take_landed()
    }
    fn scrobble(&self, rating_key: &str) -> bool {
        self.0.client().is_some_and(|c| c.mark_played(rating_key))
    }
    fn unscrobble(&self, rating_key: &str) -> bool {
        self.0.client().is_some_and(|c| c.mark_unplayed(rating_key))
    }

    // ---- artwork, subtitles, playback ----
    fn image_transcode_path(&self, src_path: &str, w: i64, h: i64, _png: bool) -> String {
        // Asked by the poster pipeline while it draws. An image path is the server's address and
        // the image's, so before a client is made it comes from the saved sign-in alone: making
        // the client resumes the sign-in, a request that, with the server not answering, held the
        // frame until the frame guard ended the app.
        if let Some(c) = self.0.installed() {
            return c.image_path(src_path, w, h);
        }
        let saved = plx_plex::plex::session::peek();
        if saved.pms_token().is_empty() {
            return String::new();
        }
        saved_client(&saved).image_path(src_path, w, h)
    }
    fn art_outlives_grant(&self, path: &str) -> bool {
        paths::is_user_image(path)
    }
    fn sidecar_subtitle_detailed(&self, key: &str, _codec: &str) -> Result<Vec<u8>, String> {
        self.0.client().ok_or_else(|| "not signed in".to_string())?.get_bytes(key)
    }
    fn mde_decision(&self, rating_key: &str, media_index: usize, _session: &str, audio: i64, subtitle: i64, forced: bool)
        -> Option<MediaContainer> {
        // The server decides, from a profile of this device (`playback_info`): it knows the
        // Dashboard's bitrate limits and which subtitles it must burn in. Forced direct play (the
        // viewer's setting) and a server that cannot be asked keep the original file.
        let c = self.0.client()?;
        let mut m = c.metadata_recent(rating_key)?;
        // The version asked for, by its position in the item's versions (the server's order).
        let chosen = m.media.get_mut(media_index)?;
        chosen.selected = true;
        let source = chosen.part.first().and_then(|p| transcode::media_source_of(&p.key)).map(str::to_string);
        let profile = playback_info::device_profile(&this_device(&plx_platform::devcaps::caps()));
        let verdict = if forced {
            None
        } else {
            c.playback_info(rating_key, source.as_deref(), (audio > 0).then_some(audio), (subtitle > 0).then_some(subtitle), &profile)
        };
        if let Some(v) = &verdict {
            plx_base::eventlog::log(&format!(
                "jellyfin: verdict direct_play={} video_encode={} audio_encode={} reasons={}",
                v.direct_play, v.video_encode, v.audio_encode, v.reasons.join(",")
            ));
        }
        self.0.remember_verdict(rating_key, verdict.clone());
        let direct = verdict.as_ref().is_none_or(|v| v.direct_play);
        for part in m.media.iter_mut().flat_map(|md| md.part.iter_mut()) {
            part.decision = if direct { "directplay" } else { "transcode" }.into();
            for st in part.stream.iter_mut().filter(|st| st.stream_type == 1 || st.stream_type == 2) {
                let encode = match (&verdict, st.stream_type) {
                    (Some(v), 1) => v.video_encode,
                    (Some(v), _) => v.audio_encode,
                    _ => false,
                };
                st.decision = if encode { "transcode" } else { "copy" }.into();
            }
        }
        Some(MediaContainer { size: 1, total_size: 1, metadata: vec![m], ..Default::default() })
    }
    fn direct_play_url(&self, part_key: &str, session: &str) -> Option<StreamUrl> {
        let c = self.0.client()?;
        let session = (!session.is_empty()).then_some(session);
        Some(StreamUrl::parse(&c.direct_play_url(part_key, session)))
    }
    fn transcode_decision(&self, spec: &TranscodeSpec<'_>) -> Option<MediaContainer> {
        if transcode::is_hls(spec) {
            return Some(transcode::hls_refusal());
        }
        self.0.transcode_plan(spec).map(|(_, p)| p.decision)
    }
    fn transcode_start_url(&self, spec: &TranscodeSpec<'_>) -> Option<StreamUrl> {
        let (c, p) = self.0.transcode_plan(spec)?;
        let url = match &p.server_url {
            Some(server) => c.server_transcode_url(server, spec.session, p.start_ticks),
            None => c.transcode_url(&p.request(spec.rating_key, spec.session)),
        };
        Some(StreamUrl::parse(&url))
    }
    fn transcode_stop(&self, session: &str) -> bool {
        self.0.client().is_some_and(|c| c.stop_transcode(session))
    }
    fn transcode_session_present(&self, _session: &str) -> Option<bool> {
        // Jellyfin ends an encoder when its stop request is answered.
        Some(false)
    }
    fn transcode_resource_reconciled(&self, _session: &str) -> Option<bool> {
        Some(true)
    }
    fn person_profile(&self, person_id: &str) -> Option<plx_plex::plex::discover::PersonProfile> {
        self.0.client()?.person_profile(person_id)
    }
    fn person_credits(&self, _person_id: &str) -> Option<Vec<plx_plex::plex::discover::CreditGroup>> {
        // Jellyfin knows a person's work only as far as the library holds it, and the page's own
        // shelf already lists that (`person_media`). Nothing beyond it: settle on an empty
        // filmography rather than retry a plex.tv request that cannot go anywhere.
        Some(Vec::new())
    }
    fn person_media(&self, person_id: &str) -> Option<MediaContainer> {
        self.0.client()?.person_media(person_id)
    }
    fn trickplay(&self, rating_key: &str, part_id: i64) -> Option<Trickplay> {
        self.0.client()?.trickplay(rating_key, part_id)
    }
    fn trickplay_sheet(&self, rating_key: &str, part_id: i64, info: &Trickplay, sheet: u32) -> Option<Vec<u8>> {
        self.0.client()?.trickplay_sheet(rating_key, part_id, info, sheet)
    }
    fn play_queue(&self, rating_key: &str, continuous: bool) -> Option<MediaContainer> {
        self.0.client()?.play_queue(rating_key, continuous)
    }
    fn timeline(&self, r: &TimelineReport<'_>) -> bool {
        let Some(c) = self.0.client() else { return false };
        c.report(&PlaybackReport {
            item_id: r.rating_key,
            play_session_id: r.session,
            state: match r.state {
                TimelineState::Playing => PlayState::Playing,
                TimelineState::Paused => PlayState::Paused,
                TimelineState::Stopped => PlayState::Stopped,
            },
            position_ms: r.time_ms,
            audio_index: r.audio_stream_id,
            subtitle_index: r.subtitle_stream_id,
        })
    }

    // The signed-in Jellyfin user's own settings stand in for the plex.tv profile's.
    fn audio_preferences(&self) -> Result<AudioPreferences, PreferenceError> {
        let c = self.0.client().ok_or(PreferenceError::Unavailable)?;
        c.language_preferences().map(|cfg| translate::audio_preferences(&cfg))
    }
    fn save_audio_preferences(&self, previous: &AudioPreferences, next: &AudioPreferences)
        -> Result<AudioPreferences, PreferenceError> {
        let c = self.0.client().ok_or(PreferenceError::Unavailable)?;
        let update = translate::language_update(previous, next);
        let saved = if update.is_empty() { c.language_preferences()? } else { c.save_language_preferences(&update)? };
        Ok(translate::audio_preferences(&saved))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A shelf of people has rows in `directory`, not `metadata`: it is not an empty shelf.
    #[test]
    fn a_people_shelf_survives_the_empty_shelf_filter() {
        let people = Hub { directory: vec![plx_plex::plex::Tag::default()], ..Default::default() };
        assert_eq!(hubs(vec![people, Hub::default()]).hub.len(), 1);
    }

    #[test]
    fn shelf_keys_round_trip() {
        assert_eq!(hub_key("/jf/resume"), Some(HubKey::Resume));
        assert_eq!(hub_key("/jf/nextup"), Some(HubKey::NextUp));
        assert_eq!(hub_key("/jf/resume/3"), Some(HubKey::ResumeIn(3)));
        assert_eq!(hub_key("/jf/latest/12?x=1"), Some(HubKey::Latest(12)));
        assert_eq!(hub_key("/jf/items/abc/children"), Some(HubKey::Children("abc")));
        assert_eq!(hub_key("/jf/items/abc"), None);
        assert_eq!(hub_key("/hubs/home/continueWatching"), None);
    }

    #[test]
    fn a_launch_item_id_is_read_as_a_jellyfin_guid() {
        let key = Some("f137a2dd21bbc1b99aa5c0f6bf02a805".to_string());
        assert_eq!(launch_item_key("f137a2dd21bbc1b99aa5c0f6bf02a805"), key);
        assert_eq!(launch_item_key("F137A2DD-21BB-C1B9-9AA5-C0F6BF02A805"), key, "dashes and case");
        for not_one in ["12345", "", "f137a2dd21bbc1b99aa5c0f6bf02a8", "f137a2dd21bbc1b99aa5c0f6bf02a805ff",
            "g137a2dd21bbc1b99aa5c0f6bf02a805"] {
            assert_eq!(launch_item_key(not_one), None, "{not_one}");
        }
    }

    fn ep(rk: &str, show: &str, last: i64) -> Metadata {
        Metadata { rating_key: rk.into(), grandparent_rating_key: show.into(), last_viewed_at: last, ..Default::default() }
    }
    fn keys(c: &MediaContainer) -> Vec<&str> {
        c.metadata.iter().map(|m| m.rating_key.as_str()).collect()
    }

    /// The deck is Resume, then each other show's Next Up episode.
    #[test]
    fn the_deck_adds_next_up_for_shows_not_already_on_it() {
        let resume = MediaContainer { metadata: vec![ep("m1", "", 50), ep("e1", "s1", 60)], ..Default::default() };
        let next = MediaContainer { metadata: vec![ep("e1", "s1", 0), ep("e2", "s1", 0), ep("e9", "s2", 0)], ..Default::default() };
        assert_eq!(keys(&deck_of(resume, Some(next))), ["m1", "e1", "e9"]);
    }

    #[test]
    fn plex_sorts_map_to_jellyfin_sorts() {
        assert_eq!(sort_of(""), (SortBy::Title, false));
        assert_eq!(sort_of("titleSort"), (SortBy::Title, false));
        assert_eq!(sort_of("addedAt:desc"), (SortBy::DateCreated, true));
        assert_eq!(sort_of("originallyAvailableAt:desc"), (SortBy::PremiereDate, true));
        assert_eq!(sort_of("audienceRating:desc"), (SortBy::CommunityRating, true));
        // A title sort is by the name shown, never the server's stale `SortName`, and so is every
        // other sort's tie-break.
        assert_eq!(SortBy::Title.as_str(), "Name");
        assert_eq!(SortBy::DateCreated.as_str(), "DateCreated,Name");
    }

    #[test]
    fn a_latest_window_keeps_its_slice_and_the_total() {
        let c = MediaContainer {
            metadata: (0..5).map(|i| Metadata { rating_key: i.to_string(), ..Default::default() }).collect(),
            ..Default::default()
        };
        let w = window(c, 2, 2);
        let keys: Vec<_> = w.metadata.iter().map(|m| m.rating_key.as_str()).collect();
        assert_eq!(keys, ["2", "3"]);
        assert_eq!((w.size, w.total_size, w.offset), (2, 5, 2));
    }
}

#[cfg(test)]
mod resume_tests {
    use super::{saved_user, user_record, with_user, SignedIn, USER_KEY};
    use plx_plex::plex::session::{ServerRef, Session, SourceRef};

    const SERVER: &str = "0123456789abcdef0123456789abcdef";

    fn user() -> SignedIn {
        SignedIn {
            user_id: "fedcba9876543210fedcba9876543210".into(),
            user_name: "Ana".into(),
            avatar_tag: Some("tag".into()),
            token: "token".into(),
            server_id: SERVER.into(),
        }
    }

    fn signed_in() -> Session {
        let source = |id: &str| SourceRef { machine_id: id.into(), ..Default::default() };
        Session {
            server: ServerRef { machine_id: SERVER.into(), ..Default::default() },
            sources: vec![source(SERVER), source("another")],
            ..Default::default()
        }
    }

    /// The user saved at sign-in comes back as the same signed-in user, with the token in hand,
    /// so a launch needs no `/Users/Me`; a sign-in saved before the user was has none.
    #[test]
    fn a_saved_user_resumes_without_asking_the_server() {
        let mut server = ServerRef { machine_id: SERVER.into(), ..Default::default() };
        assert!(saved_user(&server, "token").is_none(), "saved before the user was");
        server.extensions.0.insert(USER_KEY.into(), user_record(&user()));
        assert_eq!(saved_user(&server, "token"), Some(user()));
        let no_avatar = SignedIn { avatar_tag: None, ..user() };
        server.extensions.0.insert(USER_KEY.into(), user_record(&no_avatar));
        assert_eq!(saved_user(&server, "token"), Some(no_avatar));
        server.extensions.0.insert(USER_KEY.into(), serde_json::json!({ "id": "", "name": "Ana" }));
        assert!(saved_user(&server, "token").is_none(), "a record without an id");
    }

    /// The user is saved on the server record and on that server's own source, once, and never
    /// on a session that has moved to another server.
    #[test]
    fn the_user_is_saved_for_the_signed_in_server_only() {
        let record = user_record(&user());
        let next = with_user(&signed_in(), SERVER, &record).expect("saved");
        assert_eq!(next.server.extensions.0.get(USER_KEY), Some(&record));
        assert_eq!(next.sources[0].extensions.0.get(USER_KEY), Some(&record));
        assert!(next.sources[1].extensions.0.get(USER_KEY).is_none(), "another server's source");
        assert!(with_user(&next, SERVER, &record).is_none(), "already saved: nothing to write");
        assert!(with_user(&signed_in(), "another", &record).is_none(), "not the signed-in server");
        assert!(with_user(&signed_in(), "", &record).is_none());
    }

    /// A name or avatar changed on the server replaces the saved record (what a launch's refresh
    /// writes), so the next launch resumes with the new ones.
    #[test]
    fn a_changed_name_or_avatar_replaces_the_saved_user() {
        let saved = with_user(&signed_in(), SERVER, &user_record(&user())).expect("saved");
        let renamed = SignedIn { user_name: "new name".into(), avatar_tag: Some("tag2".into()), ..user() };
        let next = with_user(&saved, SERVER, &user_record(&renamed)).expect("changed: written");
        assert_eq!(saved_user(&next.server, "token"), Some(renamed));
    }

    /// The copies the screens read (the seated user, the who's-watching tile, the saved profile)
    /// take the new name and avatar too, even when the saved record already has them.
    #[test]
    fn a_changed_name_or_avatar_reaches_every_copy_the_screens_read() {
        use super::paths;
        use plx_plex::plex::session::{HomeUserRef, ProfileCreds, UserRef};
        let old = user();
        let id = old.user_id.clone();
        let stale = |title: &str| UserRef { uuid: id.clone(), title: title.into(), thumb: paths::user_image(&id, "tag"), ..Default::default() };
        let mut session = with_user(&signed_in(), SERVER, &user_record(&old)).expect("saved");
        session.user = stale("old name");
        session.home_users = vec![HomeUserRef { uuid: id.clone(), title: "old name".into(), thumb: paths::user_image(&id, "tag"), ..Default::default() }];
        session.profiles = vec![ProfileCreds { uuid: id.clone(), user: stale("old name"), server: session.server.clone(), ..Default::default() }];
        let renamed = SignedIn { user_name: "new name".into(), avatar_tag: Some("tag2".into()), ..old };
        // The record is already the new one (a refresh that ran before the copies were kept).
        session.server.extensions.0.insert(USER_KEY.into(), user_record(&renamed));
        let next = with_user(&session, SERVER, &user_record(&renamed)).expect("the copies are stale: written");
        let fresh = paths::user_image(&id, "tag2");
        assert_eq!((next.user.title.as_str(), next.user.thumb.as_str()), ("new name", fresh.as_str()));
        assert_eq!((next.home_users[0].title.as_str(), next.home_users[0].thumb.as_str()), ("new name", fresh.as_str()));
        assert_eq!((next.profiles[0].user.title.as_str(), next.profiles[0].user.thumb.as_str()), ("new name", fresh.as_str()));
        assert!(with_user(&next, SERVER, &user_record(&renamed)).is_none(), "everything current: nothing to write");
    }
}

#[cfg(test)]
mod grant_tests {
    use super::{prove_grant, MintRefusal, Unproved};
    use std::cell::Cell;

    const SERVER: &str = "0123456789abcdef0123456789abcdef";

    /// The saved server is granted when it answers as itself, before any credential goes out.
    #[test]
    fn the_saved_server_is_granted_when_it_answers_as_itself() {
        let (probes, mints) = (Cell::new(0), Cell::new(0));
        let proved = prove_grant(SERVER, || { probes.set(probes.get() + 1); Some(SERVER.into()) },
            || { mints.set(mints.get() + 1); Ok(()) });
        assert_eq!(proved, Ok(()));
        assert_eq!((probes.get(), mints.get()), (1, 1));
    }

    /// A launch's foreground moves the generations under the first mint: the server is asked again
    /// and granted under what is true now, once.
    #[test]
    fn a_stale_mint_is_proved_again_from_the_probe_once() {
        let (probes, mints) = (Cell::new(0), Cell::new(0));
        let proved = prove_grant(SERVER, || { probes.set(probes.get() + 1); Some(SERVER.into()) },
            || { mints.set(mints.get() + 1); if mints.get() == 1 { Err(MintRefusal::Stale) } else { Ok(()) } });
        assert_eq!(proved, Ok(()));
        assert_eq!((probes.get(), mints.get()), (2, 2), "the second mint rests on a second answer");

        let mints = Cell::new(0);
        assert_eq!(prove_grant(SERVER, || Some(SERVER.into()), || { mints.set(mints.get() + 1); Err(MintRefusal::Stale) }),
            Err(Unproved::Refused(MintRefusal::Stale)));
        assert_eq!(mints.get(), 2, "stale twice stays stale");
    }

    /// No answer, another server at the address, or any other refusal: nothing is granted, and
    /// only a stale mint is asked again.
    #[test]
    fn nothing_is_granted_without_the_saved_servers_own_answer() {
        assert_eq!(prove_grant(SERVER, || None, || panic!("granted with no answer")), Err(Unproved::NoAnswer));
        assert_eq!(prove_grant(SERVER, || Some("fedcba9876543210fedcba9876543210".into()), || panic!("granted to another server")),
            Err(Unproved::OtherServer));
        for refusal in [MintRefusal::NotConsented, MintRefusal::NotPrivateLiteral, MintRefusal::NotPlaintext] {
            let mints = Cell::new(0);
            assert_eq!(prove_grant(SERVER, || Some(SERVER.into()), || { mints.set(mints.get() + 1); Err(refusal) }),
                Err(Unproved::Refused(refusal)));
            assert_eq!(mints.get(), 1, "{refusal:?} is final");
        }
    }
}

#[cfg(test)]
mod grid_tests {
    use super::grid_types;

    #[test]
    fn the_type_menu_picks_what_a_library_lists() {
        assert_eq!(grid_types("movie", None), "Movie");
        assert_eq!(grid_types("show", Some("2")), "Series");
        assert_eq!(grid_types("show", Some("3")), "Season");
        assert_eq!(grid_types("show", Some("4")), "Episode");
        assert_eq!(grid_types("movie", Some("18")), "BoxSet");
        assert_eq!(grid_types("show", Some("18")), "BoxSet");
    }
}

#[cfg(test)]
mod sign_in_tests {
    use super::*;
    use crate::jellyfin::client::normalize_base;

    #[test]
    fn an_address_reads_back_as_the_base_it_was_shown_for() {
        for (base, shown) in [
            ("http://192.168.1.20:8096", "192.168.1.20:8096"),
            ("http://nas.local:80", "nas.local:80"),
            ("https://media.example:443/jellyfin", "https://media.example/jellyfin"),
            ("https://media.example:8920", "https://media.example:8920"),
        ] {
            assert_eq!(display_address(base), shown);
            assert_eq!(normalize_base(shown), base, "{shown} must read back as {base}");
        }
    }

    #[test]
    fn a_saved_base_keeps_the_path_the_origin_drops() {
        let mut server = plx_plex::plex::session::ServerRef {
            origin_url: "https://media.example:443".into(),
            ..Default::default()
        };
        assert_eq!(saved_base(&server), "https://media.example:443", "a record saved before the key");
        server.extensions.0.insert(BASE_KEY.into(), "https://media.example:443/jellyfin".into());
        assert_eq!(saved_base(&server), "https://media.example:443/jellyfin");
    }

    #[test]
    fn a_sign_in_request_is_taken_once_and_never_prints_its_credentials() {
        let request = SignInRequest {
            server: "nas.local".into(),
            method: SignInMethod::Password { username: "alice".into(), password: "hunter2".into() },
        };
        let shown = format!("{request:?}");
        assert!(!shown.contains("hunter2") && !shown.contains("alice"), "{shown}");
        request_sign_in(request.clone());
        assert_eq!(take_sign_in_request(), Some(request));
        assert_eq!(take_sign_in_request(), None);
    }
}
