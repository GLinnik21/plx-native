//! Persisted login session — what makes the client **offline-first**. After the one-time online
//! login (account token → server discovery → profile switch), the chosen server's verified
//! [`Origin`] and the profile's token are written here. A stable build can therefore resume a
//! stored HTTPS origin without plex.tv when it remains reachable; an explicit developer-trigger
//! build may also resume a plaintext HTTP origin for lab use. Lives in the writable app dir (device-only; never in the
//! repo). The token fields are secrets — this file's contents are never logged.
//!
//! ## One server, and then the ROSTER
//!
//! [`Session::server`] is still the primary — the one address `can_go_local` runs on and the one
//! `app.rs` boots against — and refresh keeps its origin/tier aligned with the roster. Beside it,
//! [`Session::sources`] records **every**
//! server discovery reached, ours and every share, each with its own address and its own
//! per-(user, server) token, because a shared server is a separate authority that answers 401 to
//! anybody else's credential (`docs/shared-servers.md` §2b). A single-server account writes one
//! entry there and behaves exactly as it always has.
//!
//! **Nothing here carries a timestamp**, deliberately: this TV's wall clock runs ~3 h skewed
//! (`docs/agent-reference.md`), so a stored "last seen" would be a number that cannot be compared with
//! anything and would invite an expiry rule built on it.
use super::origin::Origin;
use super::probe::Location;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Deserializer, Serialize};
use std::sync::Mutex;

/// The signed-in profile, in-memory for the UI (the Home profile chip reads this). Set by the boot
/// gate (from the stored session) and on every profile switch, so it survives an offline boot.
static CURRENT: Mutex<Option<UserRef>> = Mutex::new(None);
/// Bumped on every [`set_current`]; per-frame readers (the Home profile chip) snapshot by
/// generation instead of re-cloning the UserRef every frame.
static CURRENT_GEN: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

/// Install the active profile for the UI (or clear it on sign-out with `None`).
pub fn set_current(u: Option<UserRef>) {
    if let Ok(mut g) = CURRENT.lock() {
        *g = u;
    }
    CURRENT_GEN.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
}
/// The active profile (name + avatar), if any. Empty title = the owner with no Plex Home selection.
pub fn current() -> Option<UserRef> {
    CURRENT.lock().ok().and_then(|g| g.clone())
}
/// The profile generation (see [`set_current`]).
pub fn current_gen() -> u32 {
    CURRENT_GEN.load(std::sync::atomic::Ordering::Relaxed)
}

/// Session file locations, best first — see [`crate::paths::session_candidates`] for why this is a
/// SEARCH ORDER rather than the single constant it used to be. The short version: webOS picks one
/// of two jail profiles by install prefix, and they disagree about which directories are writable,
/// so the one hardcoded path was correct under Developer Mode and did not exist under a Homebrew
/// Channel install — where `save()` then dropped the error and the user re-did the QR sign-in on
/// every boot, with a fresh `X-Plex-Client-Identifier` each time.
///
/// The first entry is still deliberately OUTSIDE the app install dir: appinstalld replaces
/// `applications/com.beb.plxnative/` wholesale on every ipk (re)install, which silently signed the
/// user out when the file lived there.
#[cfg(not(test))]
fn auth_paths() -> Vec<std::path::PathBuf> {
    crate::paths::session_candidates()
}

/// The test build's [`auth_paths`]: the real search order until a test redirects it to a file of
/// its own (see `tests::TempSession`). A `#[cfg(test)]` global, so a shipped binary has neither the
/// static nor the branch — the file this module writes on a television is decided by `paths.rs` and
/// by nothing else.
///
/// It exists because there is no other way to exercise the writing half at all: every candidate
/// `paths.rs` offers is either a device path that does not exist on the dev Mac or — for
/// `in_app_dir` — the directory the test binary itself is running from, which is a real writable
/// path, so a careless test would leave a credentials-shaped file in `target/`.
#[cfg(test)]
static TEST_FILE: Mutex<Option<std::path::PathBuf>> = Mutex::new(None);

#[cfg(test)]
fn auth_paths() -> Vec<std::path::PathBuf> {
    match TEST_FILE.lock().unwrap_or_else(|e| e.into_inner()).clone() {
        Some(p) => vec![p],
        None => crate::paths::session_candidates(),
    }
}

/// Point this module's file at `p`, or back at the real search order with `None`.
///
/// `pub(crate)` because the writing half is no longer only this module's business: `browse`'s
/// per-profile Home selection round-trips through this file, and grading THAT end to end is the
/// only way to catch the shape of bug it exists to prevent (one profile's answer overwriting
/// another's), which no in-memory fixture can see.
///
/// The caller owes the same discipline `tests::TempSession` documents: hold
/// [`crate::testlock::serial`] for the whole test, because this is a crate global and several
/// modules reach `session::load` indirectly.
#[cfg(test)]
pub(crate) fn redirect_for_test(p: Option<std::path::PathBuf>) {
    *TEST_FILE.lock().unwrap_or_else(|e| e.into_inner()) = p;
}

/// The full persisted session. Empty fields mean "not logged in yet" for that stage.
#[derive(Serialize, Deserialize, Default, Clone)]
pub struct Session {
    /// Stable `X-Plex-Client-Identifier` — generated once, reused forever (plex.tv binds the pin
    /// and the authorized-device entry to it).
    #[serde(default)]
    pub client_id: String,
    /// plex.tv account token (for online: re-discovery, home-users, switch). Not used for PMS.
    #[serde(default)]
    pub account_token: String,
    #[serde(default)]
    pub server: ServerRef,
    #[serde(default)]
    pub user: UserRef,
    /// The Plex Home roster as of the last successful fetch — lets the who's-watching picker
    /// render instantly on every boot (and offline) instead of waiting on a plex.tv round-trip.
    ///
    /// Soft-parsed for the same reason [`Session::sources`] is: one managed user whose stored
    /// `thumb` came back as a JSON `null` would otherwise fail the whole `Session` and sign the
    /// device out on every boot, to fix an avatar.
    #[serde(default, deserialize_with = "de_soft_vec")]
    pub home_users: Vec<HomeUserRef>,
    /// **Every server this identity can browse**, as of the last successful discovery — ours and
    /// each share, best-address-first per entry. Additive: [`Session::server`] stays the primary,
    /// and this list holds it too (as the `owned` entry) so a reader needs only one surface.
    ///
    /// Soft-parsed (see [`de_soft_vec`]) — a corrupt or unreadable entry costs that entry, never
    /// the `Session`, because failing the whole file here is a silent sign-out at every boot for
    /// a feature nobody has used yet.
    #[serde(default, deserialize_with = "de_soft_vec")]
    pub sources: Vec<SourceRef>,
    /// Which libraries each PROFILE chose to see **on Home**. Browsing is governed by the grant,
    /// not by this: pinning is the only *setting* of the three states a source has (granted /
    /// pinned / reachable — `docs/shared-servers.md` §6).
    ///
    /// **Keyed by profile, and that is the whole point of the shape** — the same lesson
    /// [`Session::recent_searches`] beside it records, learned the same way. It was a bare
    /// `Vec<PinnedLib>` hanging off the `Session`, which is one per INSTALL: a household where one
    /// person wants a friend's films on their front door and another does not could not express it,
    /// and switching profile left the previous person's shelves in place. The owner's ruling
    /// (2026-08-21) is explicit — "it is separate for each profile" — and a shared television is
    /// exactly where that matters.
    ///
    /// **An absent entry means "never asked", not "nothing pinned"** — the same trap `home_users`
    /// documents, and why [`HomePins`] records both sides of the answer rather than one list.
    ///
    /// Soft-parsed (see [`de_soft_vec`]) like every list in this struct: one hand-edited entry
    /// costs that entry, never the credentials.
    #[serde(default, deserialize_with = "de_soft_vec")]
    pub home_pins: Vec<HomePins>,
    /// The search terms actually searched, most recent first — what the Search screen's
    /// empty-query state offers back (`crate::ui::search::recents` owns the cap, the
    /// de-duplication and the ordering; this is only where they rest).
    ///
    /// **Keyed by PROFILE, and that is the whole point of the shape.** They lived here as a bare
    /// `Vec<String>` for one commit, which made them the account's rather than the person's — so
    /// after a Plex Home switch the next person's empty search screen offered back what the
    /// previous one had looked for. A search history is about as personal as watch state, which
    /// this product already scopes per user, and a shared television is exactly where that
    /// matters.
    ///
    /// Clearing on a switch would also have fixed the leak, and is the wrong fix: it costs you
    /// your own history every time you hand the remote over and take it back.
    ///
    /// They live in this file rather than one of their own because it is the file cleared on
    /// sign-out, so they go with the credentials they belong to instead of being left for whoever
    /// signs in next.
    ///
    /// Soft-parsed (see [`de_soft_vec`]) for the reason every list in this struct is: a hand-edited
    /// or half-written entry must cost that entry and nothing more. Failing the `Session` over a
    /// search term would sign the device out on every boot.
    #[serde(default, deserialize_with = "de_soft_vec")]
    pub recent_searches: Vec<RecentSearches>,
    /// The install's playback-quality preference. `None` is deliberately distinct from an
    /// explicit value: every session written before this field existed lands there and must keep
    /// the old **Original** behaviour rather than being migrated onto automatic playback.
    ///
    /// A newly-created session writes an explicit default through [`PlaybackQuality::fresh_default`].
    /// That default may become Auto only when the playback owner exposes a positive readiness
    /// gate. The integrated HLS prime/swap path opens it for fresh installs; old files remain
    /// Original because their absent field is not reinterpreted. Unknown or malformed future
    /// values soften to `None`, and therefore Original,
    /// instead of making the credentials file fail to parse.
    #[serde(default, deserialize_with = "de_soft_playback_quality")]
    pub(crate) playback_quality: Option<PlaybackQuality>,
    /// **Device-wide ambient memory**: the last hero `UltraBlurColors` envelope Home actually
    /// rendered on this television, so a route in the Settings/first-run family that opens
    /// BEFORE Home has fetched anything this boot — first-run consent moved ahead of the
    /// profile picker is the case that motivated this — can still seed its frozen ground from
    /// real light instead of falling all the way to the design system's authored atmosphere
    /// (`theme::ROUTE_GROUND_FALLBACK`). See [`crate::ui::route_screen::RouteGround::draw_home`],
    /// the only reader, and [`record_last_hero`], its one writer.
    ///
    /// Not keyed by profile: it says nothing about content history, only about what colour light
    /// this SET last showed, which is why it lives beside `client_id` rather than in a per-profile
    /// section like [`Session::home_pins`].
    #[serde(default)]
    pub(crate) last_hero_blur: Option<[[f32; 3]; 4]>,
}

/// Remember the hero envelope Home is showing right now, best-effort, for [`Session::last_hero_blur`].
///
/// Cheap to call on every route-ground latch: [`update`] is a single read-modify-write, and this
/// skips the write entirely when the stored envelope already matches, so parking on the same hero
/// for minutes costs nothing beyond the initial read. A session with no `client_id` yet (nothing
/// signed in) is a deliberate no-op — see [`update`]'s doc — which is fine here: there is no
/// pre-Home route to seed before an account exists.
///
/// Returns whether the file was actually rewritten — `false` both when nothing is signed in yet
/// ([`update`]'s own no-op rule) and when the stored envelope already matches, which is how a test
/// can grade the skip without inspecting file bytes.
pub(crate) fn record_last_hero(blur: [[f32; 3]; 4]) -> bool {
    update(|cur| {
        if cur.last_hero_blur == Some(blur) {
            return None;
        }
        let mut next = cur.clone();
        next.last_hero_blur = Some(blur);
        Some(next)
    })
}

/// The last hero envelope recorded by [`record_last_hero`], or `None` on a fresh device that has
/// never rendered one.
pub(crate) fn last_hero() -> Option<[[f32; 3]; 4]> {
    load().last_hero_blur
}

/// The persisted playback-quality modes. The spelling on disk is explicit rather than derived
/// from Rust variant names: these strings are a file-format contract and must survive refactors.
#[derive(Serialize, Deserialize, Default, Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PlaybackQuality {
    /// Automatic adaptation. It is offered only after the playback readiness gate opens.
    #[serde(rename = "auto")]
    Auto,
    /// No ceiling: the source's original quality and the legacy playback behaviour.
    #[default]
    #[serde(rename = "original")]
    Original,
    /// 1080p at 20 Mbps — cap large 4K sources while preserving high-rate HD.
    #[serde(rename = "1080p_20_mbps")]
    P1080High,
    /// 1080p at 8 Mbps.
    #[serde(rename = "1080p_8_mbps")]
    P1080,
    /// 720p at 4 Mbps.
    #[serde(rename = "720p_4_mbps")]
    P720,
    /// 720p at 2 Mbps.
    #[serde(rename = "720p_2_mbps")]
    P720Low,
    /// 480p at 720 kbps.
    #[serde(rename = "480p_720_kbps")]
    P480,
}

impl PlaybackQuality {
    /// A missing field in an OLD file is handled by [`Session::playback_quality`] and is always
    /// Original. This is only for a genuinely NEW file, where Auto is allowed to become the
    /// default after (and only after) its whole playback path declares itself ready.
    pub(crate) fn fresh_default(auto_ready: bool) -> Self {
        if auto_ready {
            Self::Auto
        } else {
            Self::Original
        }
    }
}

/// One profile's search history.
#[derive(Serialize, Deserialize, Default, Clone, Debug, PartialEq, Eq)]
#[serde(default)]
pub struct RecentSearches {
    /// The Plex Home user's `uuid`, or **empty for the account owner** with no Home selection —
    /// the same "empty means the owner" convention [`SourceRef`] uses for a handle. `uuid` and not
    /// `id`, because it is the identity that survives a roster refetch.
    pub user: String,
    pub terms: Vec<String>,
}

/// One persisted who's-watching tile (avatar + PIN flag; no tokens live here).
///
/// `#[serde(default)]` on the CONTAINER, so a missing field costs that field. Per-field it covered
/// only the two flags, which meant a tile written by a build that did not have `thumb` yet — or one
/// hand-edited on the TV — failed the whole `Session`, i.e. signed the device out. The same
/// reasoning applies to every struct in this file: it is a file we read on the boot path, and the
/// cost of one unexpected shape must never be the credentials.
#[derive(Serialize, Deserialize, Default, Clone)]
#[serde(default)]
pub struct HomeUserRef {
    pub uuid: String,
    pub title: String,
    pub thumb: String,
    pub protected: bool,
    pub admin: bool,
}

/// The PRIMARY server's coordinates — the one `can_go_local` boots on. `origin` is the verified
/// HTTP(S) authority; `address`:`port` remains its diagnostic/legacy fallback. `token` is that
/// server's access token (fallback when no managed-user token is set). Every server, including
/// this one, is also in [`Session::sources`].
#[derive(Serialize, Deserialize, Default, Clone)]
#[serde(default)] // a missing field costs that field, never the session — see [`HomeUserRef`]
pub struct ServerRef {
    pub name: String,
    pub machine_id: String,
    /// The dotted quad (or v6 literal, or hostname) discovery recorded. **Diagnostic, and the
    /// LEGACY fallback** — see [`ServerRef::origin`], which is what anything dialling reads.
    pub address: String,
    pub port: i64,
    pub token: String,
    /// The connection tier that won the last completed probe. `None` in legacy files and whenever
    /// an address was restored without being re-probed. Lenient on disk: an unknown future tier is
    /// lost as metadata, never allowed to make the primary session fail to parse.
    #[serde(default, deserialize_with = "de_soft_location")]
    pub tier: Option<Location>,
    /// **Where this server is, as a URL** — `"http://192.0.2.10:32400"`. Written since the origin
    /// model landed; **empty in every file written before it**, which is the whole reason
    /// [`ServerRef::origin`] has a fallback rather than an `Option`.
    ///
    /// It is a serialized [`Origin`] and not a `scheme` beside `address` because the two are not
    /// interchangeable: the host a TLS certificate is issued for is the `plex.direct` NAME, which
    /// `address` never holds (`origin.rs`). Storing the URL keeps the file legible to a human
    /// editing it on the television, which the struct-shaped alternative does not.
    ///
    /// **The `_url` suffix is not decoration**: this is the raw string, [`ServerRef::origin`] is
    /// the parsed value, and naming both `origin` would put a silent mix-up two characters away at
    /// every use. The FILE's key stays `origin`, which is what a human editing it reads.
    #[serde(default, rename = "origin")]
    pub origin_url: String,
}

impl ServerRef {
    /// **Where the primary server is.** [`ServerRef::origin`] when the file has one, else the
    /// legacy `http://{address}:{port}` — which is exactly what a file written before that field
    /// existed meant, and what every reader of this struct did with those two fields by hand.
    ///
    /// **TOTAL, unlike [`SourceRef::origin`].** The asymmetry is deliberate. A roster entry has
    /// [`SourceRef::usable`] in front of every caller, so `None` there costs one entry. This is
    /// the PRIMARY: `app.rs`'s boot gate and `auth::cancel` read it unconditionally, gated only by
    /// [`Session::can_go_local`], so a `None` here would be a NEW refusal on a path that has never
    /// had one — a silent sign-out at boot, which is the failure this whole field exists to avoid.
    /// The gate stays where it is, and the `port as i32` below is the same cast those readers were
    /// already doing, kept in one documented place instead of three.
    pub fn origin(&self) -> Origin {
        Origin::parse(&self.origin_url)
            .unwrap_or_else(|| Origin::http(&self.address, self.port as i32))
    }
}

/// One server this identity can browse — our own or a friend's share. What discovery resolved:
/// the identity to key it on, the address that actually **answered**, and the credential that
/// server accepts.
///
/// Deliberately NOT `Debug`: `token` is a live per-(user, server) PMS access token, and a derived
/// `Debug` is exactly how a secret reaches a log by accident (`dev::DevServer` says the same).
/// [`SourceRef::describe`] is the only formatter, and it prints everything but the token.
#[derive(Serialize, Deserialize, Default, Clone)]
#[serde(default)]
pub struct SourceRef {
    /// `machineIdentifier` — the ONLY stable identity, and the registry key. An address moves
    /// (LAN ↔ remote, DHCP, relay); this does not.
    pub machine_id: String,
    /// The machine name ("nas-home"). Settings surfaces only — a person is named by `shared_by`.
    pub name: String,
    /// The OWNER's plex.tv handle (`sourceTitle`), empty on our own server. The one string the
    /// browsing UI ever says about a share: "Shared by friend".
    pub shared_by: String,
    /// False ⇒ shared with us. A preference (ours sorts first, ours is `current`), never a wall.
    pub owned: bool,
    /// The address that answered `/identity` with the right `machineIdentifier` — not the first
    /// one advertised. An unmatched share's advertised local address may be this only through its
    /// TLS URI, after certificate and machine-identity verification; its plaintext form is gated.
    ///
    /// **Diagnostic metadata, and the LEGACY fallback.** It is what [`SourceRef::describe`] prints
    /// and what the Sources panel says; it is *not* what a connection is built from — that is
    /// [`SourceRef::origin`], and for an https server the two genuinely differ (`origin.rs`).
    pub address: String,
    pub port: i64,
    /// This identity's per-(user, server) `accessToken` for THIS server. A secret — never logged.
    /// Our own server's token gets a 401 from a share, which is why one token cannot serve both.
    pub token: String,
    /// The winning connection tier. It is restored onto `Client::link` only after registration,
    /// because re-pointing publishes a fresh client whose link starts unknown.
    #[serde(default, deserialize_with = "de_soft_location")]
    pub tier: Option<Location>,
    /// **Where this server is, as a URL** — the [`Origin`] the probe accepted, serialized. Empty
    /// in every file written before the field existed; [`SourceRef::origin`] falls back to
    /// `http://{address}:{port}` for those, which is what they meant. See [`ServerRef::origin`]
    /// for why that fallback exists at all, and [`ServerRef::origin_url`] for the `_url` suffix.
    #[serde(default, rename = "origin")]
    pub origin_url: String,
}

impl SourceRef {
    /// Everything about this source except the token, for the event log. The machine id is left
    /// out entirely — it is a permanent household fingerprint (`ui::stats`), and the event log is
    /// the file we ask users to send us.
    pub fn describe(&self) -> String {
        let who = if self.owned {
            "ours".to_string()
        } else {
            format!("shared by {}", self.shared_by)
        };
        format!("{:?} {}:{} ({who})", self.name, self.address, self.port)
    }
    /// Enough to dial: an address, a **dialable** port, and the credential that server accepts.
    ///
    /// The port goes through [`probe::dial_port`](super::probe::dial_port) rather than a bare
    /// `> 0`, because this is the gate `auth::install_roster` filters on before `register(…,
    /// s.port as i32, …)` — and the session file is not a trusted input: it is JSON on disk that a
    /// hand edit, a truncated write or an older build can leave holding anything an `i64` can hold.
    /// An out-of-range port wraps in that cast; here it costs the entry instead, and `de_soft_vec`
    /// already establishes that one bad roster entry costs that entry and never the session.
    pub fn usable(&self) -> bool {
        self.origin().is_some() && !self.token.is_empty()
    }

    /// **Where to dial this source**, `None` when there is nothing dialable written down.
    ///
    /// [`SourceRef::origin`] when the file has one, else the legacy `http://{address}:{port}` — an
    /// entry written before the field existed, which is every entry in every session file on every
    /// television today. The port still goes through
    /// [`probe::dial_port`](super::probe::dial_port) on that path, for the reason
    /// [`SourceRef::usable`] gives: this file is JSON on disk that a hand edit or an older build
    /// can leave holding anything an `i64` can hold, and `port as i32` WRAPS.
    ///
    /// `Option`, unlike [`ServerRef::origin`], because every caller here is already behind
    /// [`SourceRef::usable`] — so `None` costs one roster entry, which is the rule `de_soft_vec`
    /// establishes for this whole struct.
    pub fn origin(&self) -> Option<Origin> {
        if !self.origin_url.is_empty() {
            return Origin::parse(&self.origin_url);
        }
        if self.address.is_empty() {
            return None;
        }
        super::probe::dial_port(self.port).map(|p| Origin::http(&self.address, p))
    }
}

/// One library the user answered about, named the only way a library CAN be named across two
/// servers: the server's machine id plus that server's own section key. Section keys are
/// server-local integers starting at 1 — both servers in the measured pair have a section `1`
/// (`docs/shared-servers.md` §2), so a bare key identifies nothing.
#[derive(Serialize, Deserialize, Default, Clone, Debug, PartialEq, Eq)]
#[serde(default)]
pub struct PinnedLib {
    pub machine_id: String,
    pub key: i64,
}

/// **One profile's answer to "what goes on your Home?"** — the first-run route's record
/// (`Shared Sources.dc.html` deliverable F), and what the Library's Sources panel writes back
/// every time a switch is flipped.
///
/// **Both sides are recorded, and that is the field this type exists for.** A single "these are
/// pinned" list cannot tell *turned off* from *not answered about*, and the two must not be one
/// value: libraries arrive over time — a share whose server was slow to answer, a library the
/// owner created last week — and one that lands after the question was put has to fall on its own
/// DEFAULT (yours On, a friend's Off), not silently Off because it was absent from a list written
/// before it existed. That is also exactly what makes the design's "a share arriving later does
/// not reopen this screen" honest: it appears, unpinned, and the user finds it in the Sources
/// panel rather than being asked again.
#[derive(Serialize, Deserialize, Default, Clone, Debug, PartialEq, Eq)]
#[serde(default)]
pub struct HomePins {
    /// The Plex Home user's `uuid`, or **empty for the account owner** with no Home selection —
    /// the same convention [`RecentSearches`] uses, and for the same reason: `uuid` and not `id`,
    /// because it is the identity that survives a roster refetch. A profile is keyed by something
    /// durable, never by its position in the roster, which reshuffles.
    pub user: String,
    /// The first-run question has been PUT to this profile. Separate from the two lists because a
    /// profile can be asked and answer with the defaults untouched, which writes nothing new —
    /// and being asked twice is precisely what a first-run screen must never do.
    pub asked: bool,
    /// libraries this profile turned ON …
    pub on: Vec<PinnedLib>,
    /// … and the ones it turned OFF. See the type doc: absent from both is "never answered for".
    pub off: Vec<PinnedLib>,
}

impl HomePins {
    /// This profile's recorded answer for one library: `Some(on)`, or `None` when the question was
    /// never put about *this* library and the caller owes it a default.
    pub fn answer(&self, machine_id: &str, key: i64) -> Option<bool> {
        let names =
            |v: &Vec<PinnedLib>| v.iter().any(|p| p.machine_id == machine_id && p.key == key);
        if machine_id.is_empty() {
            // An unknown machine id must not match the entries that have none either — the same
            // guard [`Session::source`] carries, and the same failure it avoids: one library
            // answering for every library on every server nobody has identified yet.
            return None;
        }
        match (names(&self.on), names(&self.off)) {
            (true, _) => Some(true),
            (false, true) => Some(false),
            (false, false) => None,
        }
    }
}

/// The last-selected Plex Home user. `token` is the per-user token PMS scopes watch state by — it
/// keeps working against the LAN server offline once cached here.
#[derive(Serialize, Deserialize, Default, Clone)]
#[serde(default)] // a missing field costs that field, never the session — see [`HomeUserRef`]
pub struct UserRef {
    pub id: i64,
    pub uuid: String,
    pub title: String,
    pub thumb: String,
    pub token: String,
}

/// A list that degrades **element by element** instead of taking the whole [`Session`] with it.
///
/// `#[serde(default)]` covers a field that is ABSENT. It does not cover one that is present and
/// the wrong shape — a `null`, a string where an array belongs, one entry whose `port` was
/// hand-edited to `"32400"` — and any of those fails the enclosing struct. For a `Session` that
/// failure is not "the roster is empty": [`peek`] then finds no candidate that parses, `load`
/// mints a fresh `client_id`, and the user is signed out and re-scanning a QR code on every boot,
/// for a stale list nothing had read yet.
///
/// So: decode to a `Value` (which for JSON can only fail on input the whole file would fail on),
/// keep the entries that are the right shape, and drop the ones that are not.
fn de_soft_vec<'de, D, T>(d: D) -> Result<Vec<T>, D::Error>
where
    D: Deserializer<'de>,
    T: DeserializeOwned,
{
    let Ok(v) = serde_json::Value::deserialize(d) else {
        return Ok(Vec::new());
    };
    Ok(match v {
        serde_json::Value::Array(items) => items
            .into_iter()
            .filter_map(|it| serde_json::from_value::<T>(it).ok())
            .collect(),
        // a null, an object, a string: not a list, so there is no list. Not an error.
        _ => Vec::new(),
    })
}

/// A persisted tier is diagnostic/policy metadata, not a credential gate. Missing, null,
/// malformed, or from a newer build therefore means "unknown" rather than failing the enclosing
/// `ServerRef` (which would turn one hand edit into a silent sign-out).
fn de_soft_location<'de, D>(d: D) -> Result<Option<Location>, D::Error>
where
    D: Deserializer<'de>,
{
    let Ok(v) = serde_json::Value::deserialize(d) else {
        return Ok(None);
    };
    Ok(serde_json::from_value::<Option<Location>>(v).unwrap_or(None))
}

/// Playback quality is a preference, not a credential gate. A value written by a newer build or
/// damaged by a hand edit therefore degrades to the legacy-safe Original mode rather than making
/// the enclosing [`Session`] disappear.
fn de_soft_playback_quality<'de, D>(d: D) -> Result<Option<PlaybackQuality>, D::Error>
where
    D: Deserializer<'de>,
{
    let Ok(v) = serde_json::Value::deserialize(d) else {
        return Ok(None);
    };
    Ok(serde_json::from_value::<Option<PlaybackQuality>>(v).unwrap_or(None))
}

impl Session {
    /// The effective persisted playback quality. Absence is the literal legacy migration rule:
    /// builds that predate the field played Original, so they continue to play Original.
    pub(crate) fn playback_quality(&self) -> PlaybackQuality {
        self.playback_quality.unwrap_or(PlaybackQuality::Original)
    }

    /// Record an explicit user choice while leaving every unrelated session field intact.
    pub(crate) fn with_playback_quality(&self, quality: PlaybackQuality) -> Self {
        let mut next = self.clone();
        next.playback_quality = Some(quality);
        next
    }

    /// True once we have a LAN server + a usable PMS token — i.e. we can run offline.
    ///
    /// The PORT is part of "we have a server", and this is the only gate in front of it: every
    /// resume path (`app.rs`'s boot gate, `auth::cancel`) reads `server.port as i32` straight into
    /// `plex::install` on the strength of this answer. A port outside `1..=65535` wraps in that
    /// cast into a plausible one, so it is refused here — the app lands on sign-in, which is the
    /// honest report for a session it cannot dial, rather than talking to a port nobody named. It
    /// also covers `port` simply being ABSENT from an older file (`#[serde(default)]` = 0), which
    /// could never have connected either.
    pub fn can_go_local(&self) -> bool {
        self.server_dialable() && !self.pms_token().is_empty()
    }

    /// Is the primary's address one this app could actually open a socket to?
    ///
    /// Split out of [`Session::can_go_local`] because [`ServerRef::origin`] is deliberately total
    /// (see its doc) — so the refusal that used to be implicit in reading `address`/`port` has to
    /// be stated somewhere, and this is it. A stored ORIGIN is judged by whether it parses at all
    /// (`Origin::parse` refuses an undialable port and a scheme this app does not speak); a legacy
    /// file with no origin is judged exactly as before.
    ///
    /// **It asks "is there a supported address written down", not whether the network answers
    /// now.** `Origin::parse` accepts the two schemes the control and media transports implement,
    /// plus hostname/IPv4/IPv6 authorities with dialable ports. Reachability is measured after
    /// restore by the ordinary request/probe paths; refusing an offline but well-formed session
    /// here would wrongly send its user back to the QR flow.
    fn server_dialable(&self) -> bool {
        if !self.server.origin_url.is_empty() {
            return Origin::parse(&self.server.origin_url).is_some();
        }
        !self.server.address.is_empty() && super::probe::dial_port(self.server.port).is_some()
    }
    /// The token PMS calls use: the switched managed-user token if we have one, else the server
    /// access token (owner).
    ///
    /// **This is the PRIMARY server's token and no other's.** A share is a separate authority and
    /// answers 401 to it; its own credential is [`SourceRef::token`], keyed by machine id.
    pub fn pms_token(&self) -> &str {
        if !self.user.token.is_empty() {
            &self.user.token
        } else {
            &self.server.token
        }
    }

    /// **Is the profile currently watching the one [`Session::account_token`] belongs to?**
    ///
    /// That token is the account OWNER's (the Plex Home admin's). It is written once, by the QR
    /// sign-in, and a profile switch never replaces it — the switched user's own account token is
    /// fetched, used for one `/api/v2/resources`, and dropped. So anything asked of plex.tv with it
    /// is answered ABOUT THE OWNER: every `accessToken` that comes back is the owner's
    /// per-(user, server) grant, and a restricted profile's answer would have been a shorter list.
    /// A caller that installs those tokens while somebody else is watching has swapped identities
    /// under them, which is why this exists as a gate rather than as a display fact.
    ///
    /// `true` for the owner with or without Plex Home; `false` for a managed profile — **and false
    /// when the roster cannot say.** "We cannot prove this is the owner" and "this is the owner"
    /// must not be one value on a question whose wrong answer is another identity's credentials:
    /// `home_users` is empty for "never fetched" as well as for "no Plex Home"
    /// (see [`Session::account`]), so the two are only told apart by a uuid actually being set.
    pub fn active_profile_is_admin(&self) -> bool {
        if self.user.uuid.is_empty() {
            // No Plex Home selection was ever made, so there is no managed profile to be: auth's
            // single-user path enters Home on the owner's own server token without writing one.
            return true;
        }
        self.home_users
            .iter()
            .find(|u| u.uuid == self.user.uuid)
            .map(|u| u.admin)
            .unwrap_or(false)
    }

    /// **Is the profile this session would resume as behind a PIN?**
    ///
    /// The other flag on the same roster row as [`Session::active_profile_is_admin`], read for the
    /// one question the boot who's-watching picker has to answer: may BACK out of it silently
    /// reinstate what is on disk? A PIN-protected profile is one plex.tv validates a code for on
    /// every switch (`auth::submit_pin` → `AccountClient::switch_user`), so resuming it without
    /// one hands out precisely the session the PIN exists to gate — see [`crate::auth::cancel`].
    ///
    /// It answers the OPPOSITE way to `active_profile_is_admin` when the roster cannot say, and
    /// for the same reason: on each question, "we cannot prove it" must land on the side whose
    /// wrong answer costs nothing. There it is somebody else's credentials, so an unknown uuid is
    /// not the owner; here it is a bypassed PIN, so an unknown uuid is treated as protected. The
    /// cost of being wrong is one profile pick — the picker is still fully usable, and its
    /// *Sign out* pill is reachable with the roster empty.
    ///
    /// **An EMPTY uuid answers TRUE**, and it is the case worth spelling out, because it reads as
    /// the harmless one ("no profile chosen, so no PIN to be behind") and is the opposite. A
    /// sign-in ABANDONED at the who's-watching picker persists exactly that shape: `auth`'s
    /// `login_thread` saves the account token, the server and the roster the moment they exist —
    /// deliberately, so that walking away does not cost the whole sign-in — and no profile has been
    /// picked. Such a session's [`Session::pms_token`] falls back to the OWNER's server token, and
    /// the next boot raises a picker over it (the gate needs a roster of more than one user, which
    /// that file has). So "no profile chosen" is not "no PIN": it is *nobody has said who they
    /// are*, and the picker is that question — which is why it belongs on the same side as an
    /// unknown uuid rather than opposite it.
    pub fn active_profile_is_protected(&self) -> bool {
        if self.user.uuid.is_empty() {
            return true; // see above — nobody has said who they are
        }
        self.home_users
            .iter()
            .find(|u| u.uuid == self.user.uuid)
            .map(|u| u.protected)
            .unwrap_or(true)
    }

    /// One source by `machineIdentifier` — the only key that identifies a server.
    pub fn source(&self, machine_id: &str) -> Option<&SourceRef> {
        if machine_id.is_empty() {
            return None; // an unknown id must not match the entries that have none either
        }
        self.sources.iter().find(|s| s.machine_id == machine_id)
    }
    /// Our own server's entry in the roster, if discovery reached one.
    pub fn owned_source(&self) -> Option<&SourceRef> {
        self.sources.iter().find(|s| s.owned)
    }
    /// The shares — every source that is not ours, in discovery order.
    pub fn shared_sources(&self) -> impl Iterator<Item = &SourceRef> {
        self.sources.iter().filter(|s| !s.owned)
    }
    /// One profile's Home selection, or `None` for a profile that has never been asked. The
    /// difference is load-bearing — see [`Session::home_pins`].
    pub fn pins_for(&self, user: &str) -> Option<&HomePins> {
        self.home_pins.iter().find(|p| p.user == user)
    }

    /// Replace one profile's answer, leaving every OTHER profile's alone. A method rather than a
    /// field assignment at the call site for [`Session::set_recents_for`]'s reason: the writer
    /// holds a whole `Session`, and the obvious `Session { home_pins: mine, ..s }` would silently
    /// delete everybody else's selection.
    pub fn set_pins_for(&mut self, user: &str, pins: HomePins) {
        match self.home_pins.iter_mut().find(|p| p.user == user) {
            Some(slot) => *slot = pins,
            None => self.home_pins.push(pins),
        }
    }

    /// One profile's search terms — empty for a profile that has never searched, which is the same
    /// answer as "never chosen" and needs no distinction here.
    pub fn recents_for(&self, user: &str) -> &[String] {
        self.recent_searches
            .iter()
            .find(|r| r.user == user)
            .map(|r| &r.terms[..])
            .unwrap_or(&[])
    }

    /// Replace one profile's terms, leaving every OTHER profile's alone. That last part is the
    /// reason this is a method rather than a field assignment at the call site: the writer holds a
    /// whole `Session` and the obvious `Session { recent_searches: mine, ..s }` would silently
    /// delete everybody else's history.
    pub fn set_recents_for(&mut self, user: &str, terms: Vec<String>) {
        if let Some(r) = self.recent_searches.iter_mut().find(|r| r.user == user) {
            r.terms = terms;
        } else if !terms.is_empty() {
            self.recent_searches.push(RecentSearches {
                user: user.to_string(),
                terms,
            });
        }
    }
}

/// Which profile's history is in play: the active Plex Home user's `uuid`, or `""` for the owner
/// with no Home selection. One accessor, so the reader and the writer cannot key on different
/// things — which would look exactly like the leak this scoping exists to prevent.
pub fn current_profile_key() -> String {
    current().map(|u| u.uuid).unwrap_or_default()
}

/// **The one lock this file has**, and the only authority over it. Every public entry point in
/// this module takes it, so a read-modify-write held across [`update`] is atomic against every
/// other writer there is: the server-roster worker (`auth::refresh_roster`), the
/// who's-watching roster worker (`auth::start_switch`), the profile-switch and sign-in saves on
/// the main thread (`auth::take_ready`, `auth`'s login thread), and the search-recents flush
/// worker (`ui::search::recents`).
///
/// They were all unsynchronized — `recents` kept a `WRITING` mutex, which serialized recents
/// against recents and against nothing else, and no `auth` writer took anything at all. Two
/// failures came of it, both silent and both read by the user as something else entirely:
///
/// * a **lost update**. The roster worker re-reads the file ("a profile pick may have landed
///   meanwhile" — its own comment), the pick lands *after* that read, and the worker's save puts
///   the pre-switch profile back. The next boot resumes as the wrong person, with that person's
///   watch state, which reads as a server problem.
/// * a **torn file**. `save` truncated in place, so two interleaved writes produced JSON that
///   [`peek`] cannot parse — and an unparseable session file is not "a stale roster", it is no
///   `client_id`, no token and a QR code on the next boot. A silent sign-out, caused by a search
///   term landing at the same moment as a roster refresh.
///
/// The lock closes the second only together with the atomic write in [`write_atomic`]: one
/// process's threads are serialized here, but a reader outside this module (or a crash mid-write)
/// still sees whatever is on disk, and only a rename can promise that is a whole file.
///
/// **Not reentrant** — a plain `Mutex`. Nothing called from inside [`update`]'s closure may call
/// back into this module.
///
/// It is held across the whole write, [`write_atomic`]'s `sync_all` included, so a reader that
/// takes it can be parked for as long as the flash takes. That is affordable because of who the
/// readers are — a keypress (`ui::account_menu::open`), a boot, and one read-out that was already
/// doing an `fs::read` per frame (`ui::library`'s failed-source labels). **Do not add a per-frame
/// reader of this file**; the answer for that is a snapshot keyed on something cheap, the way
/// `ui::search::recents` caches by [`current_gen`].
static IO: Mutex<()> = Mutex::new(());

fn io() -> std::sync::MutexGuard<'static, ()> {
    // Poison is stepped over: a panic in one writer must not turn every later save into a panic of
    // its own, which on this path would mean losing the credentials rather than a stale file.
    IO.lock().unwrap_or_else(|e| e.into_inner())
}

/// Read the persisted session and nothing else — **no minting, no write.** For readers that merely
/// want to know what the session says (the account surfaces): [`load`]'s client-id minting means a
/// read can turn into a `save`, so a file that momentarily fails to parse would be overwritten with
/// a bare client_id — a silent sign-out. That is an acceptable trade on the boot path, which must
/// end up with an id; it is not one on a path a keypress can reach. Falls back to the
/// pre-relocation path (migration), same as `load`.
pub fn peek() -> Session {
    let _io = io();
    peek_locked()
}

/// [`peek`] with the lock already held — the read half every entry point here shares.
fn peek_locked() -> Session {
    match read_locked() {
        ReadState::Ready { session, .. } => session,
        ReadState::Missing | ReadState::Locked => Session::default(),
    }
}

const SECURE_FORMAT: &str = "plxnative-secure-session";

#[derive(Deserialize, Serialize)]
struct SecureEnvelope {
    format: String,
    version: u8,
    sealed: crate::keymanager::Sealed,
}

enum ReadState {
    Missing,
    Ready {
        session: Session,
        plaintext: bool,
    },
    /// A recognized encrypted file whose device key is temporarily or permanently unavailable.
    /// It must shadow every lower-priority candidate: treating it as corrupt and then writing a
    /// fresh client id would destroy the only copy of the credentials.
    Locked,
}

/// The first usable candidate, retaining whether an encrypted file exists but cannot be opened.
fn read_locked() -> ReadState {
    for path in auth_paths() {
        let Some(bytes) = read_owned_regular(&path) else {
            continue;
        };
        if let Ok(envelope) = serde_json::from_slice::<SecureEnvelope>(&bytes) {
            if envelope.format == SECURE_FORMAT && envelope.version == 1 {
                let Some(plain) = crate::keymanager::open(&envelope.sealed) else {
                    crate::log("session: secure file is present but its device key is unavailable");
                    return ReadState::Locked;
                };
                return serde_json::from_slice(&plain)
                    .map(|session| ReadState::Ready {
                        session,
                        plaintext: false,
                    })
                    .unwrap_or(ReadState::Locked);
            }
        }
        if identifies_secure_envelope(&bytes) {
            crate::log("session: unsupported or damaged secure envelope is locked");
            return ReadState::Locked;
        }
        if let Ok(session) = serde_json::from_slice(&bytes) {
            return ReadState::Ready {
                session,
                plaintext: true,
            };
        }
    }
    ReadState::Missing
}

fn identifies_secure_envelope(bytes: &[u8]) -> bool {
    serde_json::from_slice::<serde_json::Value>(bytes)
        .ok()
        .and_then(|v| v.as_object().cloned())
        .is_some_and(|o| {
            o.get("format").and_then(serde_json::Value::as_str) == Some(SECURE_FORMAT)
                || (o.contains_key("sealed") && o.contains_key("version"))
        })
}

fn has_secure_locked() -> bool {
    auth_paths()
        .iter()
        .any(|path| read_owned_regular(path).is_some_and(|b| identifies_secure_envelope(&b)))
}

/// Seed a quality only for a genuinely absent file. A parsable legacy file remains distinguishable
/// even when it omitted `client_id`; otherwise opening the Auto gate in a future build would turn
/// that old install into a fresh one merely because its identifier also needed repair.
fn seed_fresh_quality(s: &mut Session, persisted: bool, auto_ready: bool) {
    if !persisted && s.playback_quality.is_none() {
        s.playback_quality = Some(PlaybackQuality::fresh_default(auto_ready));
    }
}

/// **Hand the scrubber this household's names**, so `crate::log` can redact them without ever
/// touching this module.
///
/// The scrubber used to call [`peek`] per line, which took [`IO`] and read the file — a deadlock
/// against every writer here (`save_locked` logs while holding the lock) and a syscall storm on
/// the log path besides. Ownership is inverted now: the session layer PUSHES on every change and
/// `diag::scrub` keeps a cached snapshot.
///
/// Called on load, on save and on a successful `update`, i.e. everywhere the set of names can
/// move — including a user switch and a roster refresh, both of which land through `update`.
fn publish_identities(s: &Session) {
    let mut v: Vec<String> = vec![
        s.server.name.clone(),
        s.server.machine_id.clone(),
        s.user.title.clone(),
    ];
    for u in &s.home_users {
        v.push(u.title.clone());
        v.push(u.uuid.clone());
    }
    for src in &s.sources {
        v.push(src.name.clone());
        v.push(src.machine_id.clone());
        v.push(src.shared_by.clone());
    }
    crate::diag::scrub::set_identities(v);
}

/// Load the persisted session, ensuring a stable `client_id` exists (generated + saved on first
/// boot). Never returns an error — a missing/corrupt file degrades to a fresh, logged-out session.
/// Falls back to the pre-relocation path once and re-saves at the new one (migration).
pub fn load() -> Session {
    let _io = io();
    let read = read_locked();
    let persisted = !matches!(read, ReadState::Missing);
    let locked = matches!(read, ReadState::Locked);
    let plaintext = matches!(
        read,
        ReadState::Ready {
            plaintext: true,
            ..
        }
    );
    let mut s = match read {
        ReadState::Ready { session, .. } => session,
        ReadState::Missing | ReadState::Locked => Session::default(),
    };
    seed_fresh_quality(&mut s, persisted, crate::route::auto_quality_ready());
    if s.client_id.is_empty() {
        s.client_id = new_client_id();
        if !locked {
            save_locked(&s);
        }
    } else if plaintext {
        // Offer every plaintext session to the Key Manager immediately. This also moves a
        // parsable legacy-path file to the preferred location; without a usable service it stays
        // an atomic mode-0600 plaintext fallback.
        save_locked(&s);
    }
    publish_identities(&s);
    s
}

/// **One read-modify-write of the session file, under [`IO`], as a single atomic step.** This is
/// the door for anything that changes PART of the file — the roster, the search terms — and the
/// only way to write one without racing the other writers.
///
/// `edit` is handed what is on disk *right now* and answers with what should replace it, or `None`
/// to leave the file exactly as it is. Returns whether anything was written. The closure runs with
/// the lock held, so it must be quick and it must not call back into this module (see [`IO`]).
///
/// **A file with no `client_id` refuses the cycle before `edit` ever runs.** [`peek_locked`] hands
/// back a default `Session` both for "no file yet" and for "the file did not parse", and writing
/// one field onto that default would truncate a live session — the silent sign-out again, this
/// time caused by the fix for it. `client_id` is minted once by [`load`] on the boot path and is
/// never empty afterwards, so it is exactly the test for "something real came back". A caller with
/// no session on disk simply keeps its change in memory for the run, which is what both of today's
/// callers already wanted.
pub fn update(edit: impl FnOnce(&Session) -> Option<Session>) -> bool {
    let _io = io();
    let cur = peek_locked();
    if cur.client_id.is_empty() {
        return false;
    }
    match edit(&cur) {
        Some(next) => {
            save_locked(&next);
            true
        }
        None => false,
    }
}

/// Persist the session (best-effort; a write failure is non-fatal — we just re-login next boot).
///
/// **A whole-file REPLACE.** Use it only where the caller genuinely owns the entire file — the
/// sign-in flow and the profile switch, which built their `Session` from this same file moments
/// earlier. Anything changing one field of a file somebody else also writes must go through
/// [`update`], or it overwrites their change with whatever it last read.
///
/// **Credentials at rest: device-key encryption when an authenticated public Key Manager is
/// available, and 0600 in every case.** The probe uses TV 24+'s
/// `com.webos.service.keymanager3`. The legacy `com.palm.keymanager` service is not used because
/// its AES-CFB interface cannot authenticate ciphertext. A firmware that does not expose or permit
/// keymanager3 keeps the compatible 0600 plaintext fallback. An existing encrypted file is never
/// downgraded merely because its service is temporarily unavailable.
///
/// The mode is set in `open(2)`'s own argument — never create-then-chmod. `fs::write` creates with
/// `0666 & !umask` (0644 here), so a fallback token file would be readable by every other uid from
/// the instant it hit the disk. Passing the mode through `OpenOptionsExt` means it never *exists*
/// in a permissive mode, which a chmod after the write cannot promise.
pub fn save(s: &Session) {
    let _io = io();
    save_locked(s);
}

/// [`save`] with the lock already held.
fn save_locked(s: &Session) {
    // Before the write, not after: a failed persist still means these names are live in THIS run,
    // and the log wants them redacted either way.
    publish_identities(s);
    let Ok(json) = serde_json::to_vec_pretty(s) else {
        return;
    };
    if let Some(sealed) = crate::keymanager::seal(&json) {
        let envelope = SecureEnvelope {
            format: SECURE_FORMAT.to_string(),
            version: 1,
            sealed,
        };
        let Ok(protected) = serde_json::to_vec_pretty(&envelope) else {
            return;
        };
        for winner in auth_paths() {
            if write_atomic(&winner, &protected) {
                // A successful migration must not leave an older plaintext token file at a
                // lower-priority jail path where another uid can recover it.
                for stale in auth_paths().into_iter().filter(|p| p != &winner) {
                    remove_temp_siblings(&stale);
                    let _ = std::fs::remove_file(stale);
                }
                return;
            }
        }
        crate::log("session: key manager succeeded but the protected file could not be written");
        return;
    }
    // Never turn an already protected session back into plaintext because a service was
    // temporarily unavailable during a save. Preserve the previous ciphertext instead.
    if has_secure_locked() {
        crate::log("session: preserving the existing secure file; refusing a plaintext downgrade");
        return;
    }
    // Try each candidate; the first that accepts the write wins. A total failure is still
    // non-fatal — but it is LOGGED, because the symptom (sign in again, every boot, forever) is
    // otherwise indistinguishable from a server-side auth problem and impossible to report.
    for path in auth_paths() {
        if write_atomic(&path, &json) {
            return;
        }
    }
    crate::log(
        "session: could not persist to ANY candidate path — login will not survive a reboot",
    );
}

/// Write `json` to `path` so that whatever reads it sees the WHOLE previous file or the WHOLE new
/// one — never a truncated one, and never bytes of both. The `plxnative.new` → `mv` dance the
/// Makefile's deploy does, for the same reason and against a worse loss: the file being replaced
/// here is the credentials.
///
/// The old `O_TRUNC` in place had two windows, and the second is the one that took the file. A
/// reader between the truncate and the `write_all` sees zero bytes; a power cut or a kill in that
/// same gap leaves zero bytes *on disk*, and `peek` reads both as "no session" — sign in again.
///
/// The tmp file is a **sibling**, named off the resolved path. `rename(2)` is
/// only atomic within one filesystem, and the webOS jail's writable directories are separate mounts
/// (`/media/developer`, `/media/internal`, the app dir — see [`auth_paths`]); a tmp under `/tmp`
/// would demote this to a cross-device copy, i.e. exactly the truncate-in-place it replaces. Its
/// suffix is random and opened with `create_new` + `O_NOFOLLOW`: the module lock serializes our
/// writers, but it does not serialize another uid able to create a sibling entry.
///
/// `sync_all` before the rename and on the parent after it is what makes the promise survive the
/// plug being pulled, which on a
/// television is an ordinary way to end a session: without it the rename can be visible while the
/// data behind it is not, and the file that comes back is the empty one. It costs a flush of a
/// couple of kilobytes on a path that runs at sign-in, at a profile switch, at a roster change and
/// at a committed search term — never per frame.
///
/// The 0600 mode is [`save`]'s rule applied one file earlier: the secret must never *exist* in a
/// permissive mode, and the tmp file is where it exists first.
/// `pub(crate)` since 2026-08-29 so `crate::telemetry` writes its file the same way rather than
/// growing a second implementation of this. It is a generic 0600 atomic write that happens to live
/// beside its first caller; the alternative was two copies of a routine whose whole value is that
/// its failure modes have already been found once, on the file holding the credentials.
pub(crate) fn write_atomic(path: &std::path::Path, json: &[u8]) -> bool {
    use std::io::Write;
    use std::os::unix::fs::MetadataExt;
    let Some(parent) = path.parent() else {
        return false;
    };
    if let Ok(meta) = std::fs::symlink_metadata(path) {
        if !meta.file_type().is_file() || meta.uid() != unsafe { libc::geteuid() } {
            return false;
        }
    }
    let Some((tmp, mut f)) = create_private_temp(path) else {
        return false;
    };
    let written = f.write_all(json).is_ok() && f.sync_all().is_ok();
    drop(f); // the rename must not race our own open handle on a filesystem that cares
    if written && std::fs::rename(&tmp, path).is_ok() {
        if let Ok(dir) = std::fs::File::open(parent) {
            let _ = dir.sync_all();
        }
        remove_temp_siblings(path);
        return true;
    }
    // Leave no half-written credentials behind under a name the next writer would overwrite
    // anyway — and none at all if this candidate turned out to be unwritable.
    let _ = std::fs::remove_file(&tmp);
    false
}

fn create_private_temp(path: &std::path::Path) -> Option<(std::path::PathBuf, std::fs::File)> {
    use std::os::unix::fs::OpenOptionsExt;
    for attempt in 0..16u64 {
        let tmp = random_tmp_path(path, attempt)?;
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(&tmp)
        {
            Ok(file) => return Some((tmp, file)),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(_) => return None,
        }
    }
    None
}

fn random_tmp_path(path: &std::path::Path, attempt: u64) -> Option<std::path::PathBuf> {
    use std::io::Read;
    static FALLBACK: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    let mut nonce = [0u8; 8];
    if std::fs::File::open("/dev/urandom")
        .and_then(|mut f| f.read_exact(&mut nonce))
        .is_err()
    {
        nonce = FALLBACK
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            .wrapping_add(attempt)
            .to_ne_bytes();
    }
    let mut name = path.file_name()?.to_os_string();
    name.push(format!(".tmp.{:016x}", u64::from_ne_bytes(nonce)));
    Some(path.with_file_name(name))
}

pub(crate) fn read_owned_regular(path: &std::path::Path) -> Option<Vec<u8>> {
    use std::io::Read;
    use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
    let mut file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(path)
        .ok()?;
    let meta = file.metadata().ok()?;
    if !meta.file_type().is_file() || meta.uid() != unsafe { libc::geteuid() } {
        return None;
    }
    const MAX_FILE: u64 = 4 * 1024 * 1024;
    let mut bytes = Vec::new();
    file.by_ref()
        .take(MAX_FILE + 1)
        .read_to_end(&mut bytes)
        .ok()?;
    (bytes.len() as u64 <= MAX_FILE).then_some(bytes)
}

fn remove_temp_siblings(path: &std::path::Path) {
    use std::os::unix::fs::MetadataExt;
    if let Some(legacy) = tmp_path(path) {
        let _ = std::fs::remove_file(legacy);
    }
    let (Some(parent), Some(file_name)) = (path.parent(), path.file_name()) else {
        return;
    };
    let prefix = format!("{}.tmp.", file_name.to_string_lossy());
    let Ok(entries) = std::fs::read_dir(parent) else {
        return;
    };
    for entry in entries.flatten() {
        if !entry.file_name().to_string_lossy().starts_with(&prefix) {
            continue;
        }
        if let Ok(meta) = std::fs::symlink_metadata(entry.path()) {
            if meta.file_type().is_symlink() || meta.uid() == unsafe { libc::geteuid() } {
                let _ = std::fs::remove_file(entry.path());
            }
        }
    }
}

/// The sibling [`write_atomic`] writes through, for a resolved candidate path. One definition
/// because [`clear`] has to delete the same file, and a sign-out that missed it by spelling the
/// suffix differently would leave a live account token on the disk.
fn tmp_path(path: &std::path::Path) -> Option<std::path::PathBuf> {
    let mut name = path.file_name()?.to_os_string();
    name.push(".tmp");
    Some(path.with_file_name(name))
}

/// Clear the persisted session (sign-out) — removes the file; a fresh `client_id` is minted next
/// load. The old-path copy goes too, or the migration fallback would resurrect the stale session.
///
/// Takes [`IO`] like every other entry point, and that is not tidiness: a sign-out racing an
/// in-flight worker's read-modify-write would otherwise delete the file and have the worker put it
/// straight back, account token and all.
pub fn clear() {
    let _io = io();
    // Every candidate, not just the one we happen to write today: leaving a copy at any other
    // location would let `peek`'s search resurrect the stale session on the next boot. The `.tmp`
    // siblings go too — `peek` cannot read one, so it is not a resurrection risk, but a sign-out
    // that leaves a live account token in a file on a rooted television is not a sign-out.
    for path in auth_paths() {
        if let Some(bytes) = read_owned_regular(&path) {
            if let Ok(envelope) = serde_json::from_slice::<SecureEnvelope>(&bytes) {
                if envelope.format == SECURE_FORMAT && envelope.version == 1 {
                    crate::keymanager::remove(&envelope.sealed.backend, &envelope.sealed.key);
                }
            }
        }
        remove_temp_siblings(&path);
        let _ = std::fs::remove_file(path);
    }
}

/// A v4-ish UUID from `/dev/urandom` (no `uuid` crate). Only uniqueness/stability matter — plex.tv
/// just needs a value it can key the device on.
fn new_client_id() -> String {
    use std::io::Read;
    let mut b = [0u8; 16];
    // bounded read — /dev/urandom is a char device with no EOF, so read_exact (not fs::read).
    if let Ok(mut f) = std::fs::File::open("/dev/urandom") {
        let _ = f.read_exact(&mut b);
    }
    b[6] = (b[6] & 0x0f) | 0x40; // version 4
    b[8] = (b[8] & 0x3f) | 0x80; // variant
    format!(
        "{:02x}{:02x}{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7], b[8], b[9], b[10], b[11], b[12], b[13], b[14], b[15]
    )
}

// ---- What the account surfaces are allowed to SAY about the user ----

/// The account facts the UI may state — see [`Session::account`]. An account surface must word
/// itself from THIS, never from [`current`] alone: that profile is a bare `UserRef::default()` —
/// empty title, empty thumb — for every account **without Plex Home**, because auth's single-user
/// path enters Home without ever writing one. Reading that emptiness as "signed out" is how a
/// signed-in owner ends up being offered "Sign in".
///
/// Converted: `ui/account_menu.rs`, and — since 2026-08-23 — the shared top bar's profile chip
/// (`ui/widgets.rs` `profile_chip`), which was the remaining half of the bug. Both now word
/// themselves through ONE resolver, `ui::account_menu::chip_label`, so the chip and the menu it
/// opens cannot disagree about the same account again.
pub struct Account {
    /// **This device** holds a session: a plex.tv account token, or at least a server + PMS token
    /// it can stream on. The opposite of "offer them Sign in". Note it describes the session ON
    /// DISK, not the identity currently in use: an automated boot on `/tmp/plxnative-token` streams
    /// on an injected token yet still reports the stored account here — deliberately, because the
    /// stored account is exactly what a "Sign out" would clear.
    pub signed_in: bool,
    /// Profile switching is possible. It needs the **plex.tv account token**: both the Plex Home
    /// roster and the per-user tokens come from plex.tv, so a server-only session cannot switch
    /// (`auth::start_switch` refuses one outright). Deliberately NOT gated on the roster length —
    /// see `home_users`' note on why an empty roster means "unknown", not "there are none".
    pub can_switch: bool,
    /// Who we may say the user is: the active managed profile, else the account owner off the
    /// persisted roster. `None` = signed in but nameless (no roster has ever landed), which is a
    /// missing name and not a missing user — say "Account", never "Sign in".
    pub name: Option<String>,
}

impl Session {
    /// The account facts for the UI, from the persisted session plus the in-memory active profile
    /// (`active`, i.e. [`current`]). The profile is the better name once a managed user has been
    /// picked; the persisted roster's `admin` entry is what names an owner who has no Plex Home
    /// and therefore never got a profile written at all.
    ///
    /// **`home_users` being empty means "unknown", not "none".** It is only ever filled by a
    /// sign-in or a "Change profile", and a *failed* fetch persists an empty vec
    /// (`auth.rs`'s `home_users().unwrap_or_default()`), so "never fetched", "fetch failed" and
    /// "genuinely empty" are one value. Anything deciding on it must treat empty as "ask" — which
    /// is why [`Account::can_switch`] keeps the switch row: that row is what re-fetches the roster,
    /// and hiding it on an empty one would be a one-way door out of a Plex Home created later.
    pub fn account(&self, active: Option<&UserRef>) -> Account {
        let named = |t: &str| Some(t.to_string()).filter(|t| !t.is_empty());
        // the roster hop searches for a NAMED admin, then any named entry — a `find(admin)` whose
        // hit happens to carry an empty title must not swallow the answer sitting behind it, which
        // is the same shape of bug this whole function exists to fix.
        let roster = || {
            let named_admin = self
                .home_users
                .iter()
                .find(|u| u.admin && !u.title.is_empty());
            named_admin
                .or_else(|| self.home_users.iter().find(|u| !u.title.is_empty()))
                .map(|u| u.title.clone())
        };
        let name = active
            .and_then(|u| named(&u.title))
            .or_else(|| named(&self.user.title))
            .or_else(roster);
        Account {
            signed_in: !self.account_token.is_empty() || self.can_go_local(),
            can_switch: !self.account_token.is_empty(),
            name,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The file a signed-in device holds today, once discovery has reached two servers. Written
    /// as literal JSON rather than by serialising a `Session`, because the thing under test is
    /// what happens when the bytes on disk are not what this build expects.
    fn two_server_json() -> &'static str {
        r#"{"client_id":"cid-1","account_token":"acct",
            "server":{"name":"Mac mini","machine_id":"aaaa1111","address":"192.168.0.10",
                      "port":32400,"token":"tok-own"},
            "user":{"id":7,"uuid":"u-7","title":"Gleb","thumb":"","token":"tok-user"},
            "home_users":[{"uuid":"u-7","title":"Gleb","thumb":"","protected":false,"admin":true}],
            "sources":[
              {"machine_id":"aaaa1111","name":"Mac mini","shared_by":"","owned":true,
               "address":"192.168.0.10","port":32400,"token":"tok-own"},
              {"machine_id":"bbbb2222","name":"nas-home","shared_by":"friend","owned":false,
               "address":"203.0.113.9","port":31234,"token":"tok-share"}],
            "home_pins":[{"user":"u-7","asked":true,
                          "on":[{"machine_id":"bbbb2222","key":1}],
                          "off":[{"machine_id":"aaaa1111","key":1}]}]}"#
    }

    /// **THE COMPATIBILITY GATE: a session file written by 0.4.1 must still boot.**
    ///
    /// That build knew nothing about origins — it wrote `address` and `port` and no more — and
    /// every signed-in television in the world is holding one of these files right now. If
    /// `Session::server` failed to carry through, the cost is not a degraded feature: `app.rs`'s
    /// boot gate runs on `can_go_local()`, so the app would land on the QR sign-in screen on
    /// **every boot for every existing user**, which is a silent sign-out that no test above this
    /// one can see (the roster lists are soft-parsed — `de_soft_vec` — but the primary is not a
    /// disposable entry, and nothing soft-parses a MISSING field into a different meaning).
    ///
    /// Written as literal 0.4.1-shaped JSON rather than by serialising a `Session`, because the
    /// thing under test is precisely that today's struct is not what wrote those bytes.
    #[test]
    fn a_session_file_written_before_origins_existed_still_boots_as_plain_http() {
        // Byte-for-byte the shape 0.4.1 wrote: no `origin` on the primary, none on any source.
        let v041 = r#"{"client_id":"cid-1","account_token":"acct",
            "server":{"name":"Mac mini","machine_id":"aaaa1111","address":"192.168.0.10",
                      "port":32400,"token":"tok-own"},
            "user":{"id":7,"uuid":"u-7","title":"Gleb","thumb":"","token":"tok-user"},
            "sources":[
              {"machine_id":"aaaa1111","name":"Mac mini","shared_by":"","owned":true,
               "address":"192.168.0.10","port":32400,"token":"tok-own"},
              {"machine_id":"bbbb2222","name":"nas-home","shared_by":"friend","owned":false,
               "address":"203.0.113.9","port":31234,"token":"tok-share"}]}"#;
        let s: Session = serde_json::from_str(v041).expect("a 0.4.1 session file still parses");

        // the boot gate itself — this is the assertion whose failure is the silent sign-out
        assert!(
            s.can_go_local(),
            "a 0.4.1 session must still reach Home without a QR code"
        );

        // …and it boots against exactly the address it always did, as plain http
        let o = s.server.origin();
        assert_eq!(o.base(), "http://192.168.0.10:32400");
        assert_eq!((o.host(), o.port()), ("192.168.0.10", 32400));
        assert!(!o.is_tls(), "nothing in that file ever meant TLS");

        // every roster entry too, including the share on its non-default port
        assert!(
            s.sources.iter().all(|x| x.usable()),
            "{:#?}",
            s.sources.len()
        );
        assert_eq!(
            s.owned_source().unwrap().origin().unwrap().base(),
            "http://192.168.0.10:32400"
        );
        assert_eq!(
            s.source("bbbb2222").unwrap().origin().unwrap().base(),
            "http://203.0.113.9:31234"
        );
    }

    /// Tier persistence is additive: old files have no field, and a value written by a future
    /// build must not make the PRIMARY fail to parse (which would route a signed-in TV to QR).
    #[test]
    fn a_stored_tier_round_trips_and_unknown_tiers_degrade_to_unknown() {
        let legacy: Session =
            serde_json::from_str(two_server_json()).expect("the legacy shape parses");
        assert_eq!(legacy.server.tier, None);
        assert!(legacy.sources.iter().all(|s| s.tier.is_none()));

        let json = r#"{"client_id":"c","server":{"address":"192.0.2.10","port":32400,
                      "token":"t","tier":"future-tier"},
                    "sources":[{"machine_id":"m","address":"192.0.2.10","port":32400,
                      "token":"t","tier":"relay"}]}"#;
        let s: Session =
            serde_json::from_str(json).expect("an unknown primary tier is soft metadata");
        assert!(
            s.can_go_local(),
            "unknown tier metadata cannot silently sign the device out"
        );
        assert_eq!(s.server.tier, None);
        assert_eq!(
            s.sources[0].tier,
            Some(super::super::probe::Location::Relay)
        );

        let encoded = serde_json::to_value(ServerRef {
            tier: Some(super::super::probe::Location::Remote),
            ..Default::default()
        })
        .unwrap();
        assert_eq!(
            encoded["tier"], "remote",
            "the file stays human-readable and stable"
        );
    }

    /// A missing quality field is an OLD install, not an invitation to adopt a new default. The
    /// literal is deliberately pre-feature JSON; serialising today's `Session` would always write
    /// whatever today's struct thinks and could not grade the migration boundary.
    #[test]
    fn a_legacy_session_with_no_quality_stays_original() {
        let s: Session = serde_json::from_str(two_server_json()).expect("the legacy file parses");
        assert_eq!(
            s.playback_quality, None,
            "absence remains distinguishable on disk"
        );
        assert_eq!(
            s.playback_quality(),
            PlaybackQuality::Original,
            "legacy playback does not become Auto"
        );
    }

    /// Quality is a preference beside credentials, never a reason to discard them. This is the
    /// scalar counterpart of the roster/tier soft parsers: unknown future names, null and the
    /// wrong JSON shape all keep the session and conservatively mean Original.
    #[test]
    fn invalid_or_future_quality_is_soft_and_conservative() {
        for value in [r#""future_auto_v2""#, "null", r#"{"mode":"auto"}"#, "42"] {
            let json = format!(
                r#"{{"client_id":"c","account_token":"acct",
                     "server":{{"address":"192.168.0.10","port":32400,"token":"t"}},
                     "playback_quality":{value}}}"#
            );
            let s: Session = serde_json::from_str(&json)
                .expect("bad preference metadata cannot fail credentials");
            assert_eq!(s.account_token, "acct");
            assert!(s.can_go_local());
            assert_eq!(s.playback_quality(), PlaybackQuality::Original, "{value}");
        }
    }

    #[test]
    fn every_explicit_quality_mode_round_trips_by_stable_name() {
        let cases = [
            (PlaybackQuality::Auto, "auto"),
            (PlaybackQuality::Original, "original"),
            (PlaybackQuality::P1080High, "1080p_20_mbps"),
            (PlaybackQuality::P1080, "1080p_8_mbps"),
            (PlaybackQuality::P720, "720p_4_mbps"),
            (PlaybackQuality::P720Low, "720p_2_mbps"),
            (PlaybackQuality::P480, "480p_720_kbps"),
        ];
        for (quality, wire) in cases {
            let s = Session {
                playback_quality: Some(quality),
                ..Session::default()
            };
            let json = serde_json::to_value(&s).unwrap();
            assert_eq!(json["playback_quality"], wire);
            let again: Session = serde_json::from_value(json).unwrap();
            assert_eq!(again.playback_quality(), quality);
        }
    }

    #[test]
    fn a_fresh_install_defaults_to_auto_only_after_readiness() {
        assert_eq!(
            PlaybackQuality::fresh_default(false),
            PlaybackQuality::Original
        );
        assert_eq!(PlaybackQuality::fresh_default(true), PlaybackQuality::Auto);

        let mut absent = Session::default();
        seed_fresh_quality(&mut absent, false, true);
        assert_eq!(
            absent.playback_quality,
            Some(PlaybackQuality::Auto),
            "only the no-file path may adopt a newly ready Auto default"
        );

        // Literal legacy JSON with neither field. Its empty client id will be repaired by `load`,
        // but that is not evidence of a fresh install and must not seed Auto even after readiness.
        let mut legacy: Session =
            serde_json::from_str(r#"{"account_token":"still-a-real-file"}"#).unwrap();
        seed_fresh_quality(&mut legacy, true, true);
        assert!(legacy.client_id.is_empty());
        assert_eq!(legacy.playback_quality, None);
        assert_eq!(legacy.playback_quality(), PlaybackQuality::Original);
    }

    /// The other side of the gate: once an origin IS written down it is what gets dialled, and it
    /// beats the address pair beside it. That is not a tie-break for its own sake — for an https
    /// server the two genuinely differ (the certificate is issued for the `plex.direct` NAME, not
    /// for the quad), so reading the pair would connect and then fail validation.
    #[test]
    fn a_stored_origin_beats_the_address_pair_beside_it() {
        let json = r#"{"client_id":"c","account_token":"a",
            "server":{"machine_id":"aaaa1111","address":"203.0.113.9","port":31234,"token":"t",
                      "origin":"https://203-0-113-9.hash.plex.direct:31234"},
            "sources":[{"machine_id":"aaaa1111","owned":true,"address":"203.0.113.9","port":31234,
                        "token":"t","origin":"https://203-0-113-9.hash.plex.direct:31234"}]}"#;
        let s: Session = serde_json::from_str(json).expect("parses");

        let o = s.server.origin();
        assert_eq!(
            o.host(),
            "203-0-113-9.hash.plex.direct",
            "the name TLS validates against"
        );
        assert!(o.is_tls());
        assert_eq!(
            s.server.address, "203.0.113.9",
            "…and the quad survives as the diagnostic half"
        );
        assert!(
            s.can_go_local(),
            "an https primary is still a session this device holds"
        );
        assert_eq!(
            s.sources[0].origin().unwrap(),
            o,
            "the roster entry says the same thing"
        );

        // and it round-trips: what we write back is what we would read next boot
        let again: Session =
            serde_json::from_slice(&serde_json::to_vec(&s).unwrap()).expect("re-read");
        assert_eq!(again.server.origin(), o);
    }

    /// A stored origin that cannot be dialled is refused rather than silently repaired. The port
    /// is the case that really arrives — the session file is JSON on disk that a hand edit or an
    /// older build can leave holding anything an `i64` can hold, and `4_294_999_696 as i32` is
    /// **32400**, so "repair it to the default" means dialling a port nobody wrote down.
    #[test]
    fn an_undialable_stored_origin_is_refused_not_repaired() {
        let bad = |origin: &str| {
            let json = format!(
                r#"{{"client_id":"c","account_token":"a",
                     "server":{{"address":"192.168.0.10","port":32400,"token":"t","origin":"{origin}"}},
                     "sources":[{{"machine_id":"m","address":"192.168.0.10","port":32400,"token":"t",
                                  "origin":"{origin}"}}]}}"#
            );
            serde_json::from_str::<Session>(&json).expect("the file still parses")
        };
        for origin in [
            "http://192.168.0.10:4294999696",
            "ftp://192.168.0.10:21",
            "http://",
        ] {
            let s = bad(origin);
            assert!(!s.can_go_local(), "{origin} is not something to boot on");
            assert!(
                !s.sources[0].usable(),
                "{origin} is not something to register"
            );
        }
    }

    /// The roster survives a write/read cycle intact — including the two facts that make a share
    /// usable at all: its OWN address (never the owner's LAN one) and its OWN token.
    #[test]
    fn the_roster_round_trips_through_the_session_file_format() {
        let s: Session = serde_json::from_str(two_server_json()).expect("a normal session parses");
        let s: Session = serde_json::from_slice(&serde_json::to_vec(&s).unwrap()).expect("re-read");

        assert_eq!(s.sources.len(), 2);
        let own = s.owned_source().expect("our own server is in the roster");
        assert_eq!(
            (own.machine_id.as_str(), own.address.as_str()),
            ("aaaa1111", "192.168.0.10")
        );
        assert!(
            own.shared_by.is_empty(),
            "an owned server has no owner to name"
        );

        let share = s
            .source("bbbb2222")
            .expect("keyed by machineIdentifier, not by index");
        assert_eq!((share.address.as_str(), share.port), ("203.0.113.9", 31234));
        assert_eq!(
            share.token, "tok-share",
            "the sharing grant, not the account token"
        );
        assert_eq!(share.shared_by, "friend");
        assert!(!share.owned && share.usable());
        assert_eq!(s.shared_sources().count(), 1);

        let mine = s
            .pins_for("u-7")
            .expect("the Home selection is keyed by PROFILE");
        assert!(mine.asked);
        assert_eq!(mine.answer("bbbb2222", 1), Some(true));
        // section keys are server-local: both servers have a section 1, so the key alone matches
        // nothing on its own
        assert_eq!(
            mine.answer("aaaa1111", 1),
            Some(false),
            "an answer names a server AND a key"
        );
        assert_eq!(
            mine.answer("bbbb2222", 9),
            None,
            "a library nobody was asked about"
        );
        assert!(
            s.pins_for("u-9").is_none(),
            "another profile has an answer of its own, or none"
        );
        assert!(s.source("").is_none() && s.source("nope").is_none());

        // and the token is not printable by accident — `describe` is the only formatter there is
        assert!(
            !share.describe().contains("tok-share"),
            "{}",
            share.describe()
        );
        assert!(share.describe().contains("friend") && share.describe().contains("203.0.113.9"));
    }

    /// **The sign-out bug this list is shaped to avoid.** A `sources` array that is corrupt, the
    /// wrong type, or absent entirely must cost the roster and nothing else — `#[serde(default)]`
    /// alone does not do that, because it covers an ABSENT field and not a present, malformed one,
    /// and the failure mode is not "an empty roster" but a `Session` that will not parse: no
    /// account token, no server, a freshly minted client id, and a QR code to scan on every boot.
    #[test]
    fn a_corrupt_or_absent_roster_never_costs_the_session() {
        // one entry with a hand-mangled port, beside a perfectly good one
        let mixed = r#"{"client_id":"cid-1","account_token":"acct",
            "server":{"name":"m","machine_id":"aaaa1111","address":"192.168.0.10","port":32400,"token":"t"},
            "sources":[{"machine_id":"aaaa1111","port":{"oops":true}},
                       {"machine_id":"bbbb2222","name":"nas-home","owned":false,
                        "address":"203.0.113.9","port":31234,"token":"tok-share"}],
            "home_pins":"not a list"}"#;
        let s: Session = serde_json::from_str(mixed).expect("a bad entry must not fail the file");
        assert_eq!(s.account_token, "acct", "the credentials are still here");
        assert!(s.can_go_local(), "and the device can still stream");
        assert_eq!(
            s.sources.len(),
            1,
            "the malformed entry dropped, the good one landed"
        );
        assert_eq!(s.sources[0].machine_id, "bbbb2222");
        assert!(
            s.home_pins.is_empty(),
            "a string where a list belongs is no list, not an error"
        );

        // the whole field as an explicit null, and the whole field missing (every session file
        // written before this landed) — both are simply a session with no roster yet
        for json in [
            r#"{"client_id":"c","server":{"address":"192.168.0.10","port":32400,"token":"t"},"sources":null}"#,
            r#"{"client_id":"c","server":{"address":"192.168.0.10","port":32400,"token":"t"}}"#,
        ] {
            let s: Session = serde_json::from_str(json).expect("null and absent both parse");
            assert!(s.sources.is_empty() && s.home_pins.is_empty());
            assert!(
                s.can_go_local(),
                "the primary server is what boot runs on, roster or not"
            );
        }
    }

    /// **A port is `i64` on disk and `i32` at the socket, and the narrowing used to be a bare
    /// cast.** `4_294_999_696 as i32` is **32400** — the most ordinary port there is — so a session
    /// file holding a number no port can be would have had the app quietly dial a server nobody
    /// wrote down. `#[serde(default)]` cannot catch it either: the field parses fine, it is the
    /// value that is impossible.
    ///
    /// Both gates the value reaches are stated here, because they fail differently and one does not
    /// imply the other: a bad ROSTER entry costs that entry (`usable`, which
    /// `auth::install_roster` filters on before registering), while a bad PRIMARY costs the resume
    /// (`can_go_local`, the one gate in front of `plex::install`) and lands the app on sign-in.
    #[test]
    fn a_port_no_socket_could_take_is_refused_rather_than_wrapped() {
        let s: Session = serde_json::from_str(
            r#"{"client_id":"cid-1","account_token":"acct",
                "server":{"machine_id":"aaaa1111","address":"192.168.0.10","port":32400,"token":"t"},
                "sources":[{"machine_id":"aaaa1111","owned":true,"address":"192.168.0.10",
                            "port":4294999696,"token":"tok-own"},
                           {"machine_id":"bbbb2222","owned":false,"address":"203.0.113.9",
                            "port":31234,"token":"tok-share"}]}"#,
        )
        .unwrap();
        assert!(
            !s.sources[0].usable(),
            "32400 is what that number wraps to — it must not be dialled"
        );
        assert!(
            s.sources[1].usable(),
            "…and the entry beside it is untouched"
        );
        assert!(
            s.can_go_local(),
            "the PRIMARY is fine, so boot still resumes"
        );

        // …and the same number on the primary costs the resume instead, rather than dialling 32400
        let bad: Session = serde_json::from_str(
            r#"{"client_id":"c","server":{"address":"192.168.0.10","port":4294999696,"token":"t"}}"#,
        )
        .unwrap();
        assert!(
            !bad.can_go_local(),
            "an undialable primary sends the user to sign-in, honestly"
        );
        // an absent port is the same answer for the same reason: it could never have connected
        let none: Session = serde_json::from_str(
            r#"{"client_id":"c","server":{"address":"192.168.0.10","token":"t"}}"#,
        )
        .unwrap();
        assert!(!none.can_go_local());
    }

    /// One server must behave exactly as it did before the roster existed: the primary
    /// `server`/`user` pair is what `can_go_local` and `pms_token` read, and the roster is a
    /// record beside it, never a second source of truth that could disagree.
    #[test]
    fn a_single_server_session_behaves_as_it_always_has() {
        let mut s: Session = serde_json::from_str(
            r#"{"client_id":"cid-1","account_token":"acct",
                "server":{"name":"Mac mini","machine_id":"aaaa1111","address":"192.168.0.10",
                          "port":32400,"token":"tok-own"},
                "sources":[{"machine_id":"aaaa1111","name":"Mac mini","owned":true,
                            "address":"192.168.0.10","port":32400,"token":"tok-own"}]}"#,
        )
        .unwrap();
        assert!(s.can_go_local());
        assert_eq!(
            s.pms_token(),
            "tok-own",
            "no managed user picked yet → the server token"
        );
        s.user.token = "tok-user".into();
        assert_eq!(
            s.pms_token(),
            "tok-user",
            "a switched profile's token wins, as before"
        );
        // the roster agrees with the primary rather than competing with it
        assert_eq!(
            s.owned_source().map(|x| x.address.as_str()),
            Some(s.server.address.as_str())
        );
        assert_eq!(s.shared_sources().count(), 0);
        assert!(s.account(None).signed_in && s.account(None).can_switch);
    }

    /// The Search screen's recent terms are ordinary session content: they survive a write/read
    /// cycle in order, including the non-ASCII ones this household actually searches.
    #[test]
    fn the_recent_search_terms_round_trip_through_the_session_file_format() {
        let s: Session = serde_json::from_str(
            r#"{"client_id":"cid-1","recent_searches":[
                 {"user":"uu-1","terms":["wallace","Гладиатор","the curse"]}]}"#,
        )
        .expect("a session carrying terms parses");
        let s: Session = serde_json::from_slice(&serde_json::to_vec(&s).unwrap()).expect("re-read");
        assert_eq!(
            s.recents_for("uu-1"),
            ["wallace", "Гладиатор", "the curse"],
            "most recent first, in order"
        );

        // absent entirely — every session file written before this landed
        let s: Session = serde_json::from_str(r#"{"client_id":"c"}"#).unwrap();
        assert!(s.recent_searches.is_empty());
    }

    /// **One profile cannot read another's history, and cannot delete it either.** A search
    /// history is as personal as watch state, and a television is the one place several people
    /// share an install — so this is scoped rather than cleared on a switch, which would have
    /// stopped the leak at the price of losing your own list every time you handed the remote over.
    #[test]
    fn a_profiles_search_history_is_its_own() {
        let mut s = Session {
            client_id: "cid".into(),
            ..Default::default()
        };
        s.set_recents_for("uu-a", vec!["gromit".into()]);
        s.set_recents_for("uu-b", vec!["эдем".into()]);

        assert_eq!(s.recents_for("uu-a"), ["gromit"]);
        assert_eq!(s.recents_for("uu-b"), ["эдем"]);
        assert!(
            s.recents_for("uu-never-searched").is_empty(),
            "an unknown profile reads empty, not someone else's"
        );
        // the owner with no Plex Home selection keys on "" and is nobody else
        assert!(s.recents_for("").is_empty());

        // …and a write for one leaves the others intact — the bug `set_recents_for` exists to make
        // unwriteable, since the obvious `Session { recent_searches: mine, ..s }` deletes everybody.
        s.set_recents_for("uu-a", vec!["wallace".into(), "gromit".into()]);
        assert_eq!(s.recents_for("uu-a"), ["wallace", "gromit"]);
        assert_eq!(
            s.recents_for("uu-b"),
            ["эдем"],
            "the other profile's history survived the write"
        );
    }

    /// And they degrade the same way every other list here does: one malformed term costs that
    /// term, never the credentials sitting beside it. A search term must never be able to sign the
    /// device out.
    #[test]
    fn a_corrupt_search_term_costs_that_term_and_not_the_session() {
        let s: Session = serde_json::from_str(
            r#"{"client_id":"cid-1","account_token":"acct",
                "server":{"address":"192.168.0.10","port":32400,"token":"t"},
                "recent_searches":[{"user":"u","terms":["wallace","gromit"]},null,42,"nope"]}"#,
        )
        .expect("a bad term must not fail the file");
        assert_eq!(
            s.recents_for("u"),
            ["wallace", "gromit"],
            "the three bad entries dropped"
        );
        assert_eq!(s.account_token, "acct");
        assert!(s.can_go_local(), "and the device can still stream");

        // the whole field the wrong type is no list, not an error
        let s: Session = serde_json::from_str(r#"{"client_id":"c","recent_searches":"wallace"}"#)
            .expect("a string where a list belongs parses");
        assert!(s.recent_searches.is_empty());
    }

    /// **Whose token is `account_token`, and is that who is watching?** It is the account OWNER's,
    /// written once by the QR sign-in and never replaced by a profile switch — so a roster refresh
    /// made with it answers about the owner, and installing those per-server tokens while a managed
    /// profile is signed in swaps identities under them. For a RESTRICTED profile it also re-adds
    /// the shares `auth::retoken` had correctly made tokenless, which is a re-grant and not a refresh.
    #[test]
    fn only_the_account_owners_own_profile_may_refresh_the_roster_with_the_account_token() {
        let home = |uuid: &str| Session {
            client_id: "cid".into(),
            account_token: "acct".into(),
            user: UserRef {
                uuid: uuid.into(),
                ..Default::default()
            },
            home_users: vec![
                HomeUserRef {
                    uuid: "u-owner".into(),
                    title: "Gleb".into(),
                    admin: true,
                    ..Default::default()
                },
                HomeUserRef {
                    uuid: "u-kid".into(),
                    title: "Kid".into(),
                    admin: false,
                    ..Default::default()
                },
            ],
            ..Default::default()
        };
        assert!(
            home("u-owner").active_profile_is_admin(),
            "the owner's own tile"
        );
        assert!(
            !home("u-kid").active_profile_is_admin(),
            "a managed profile is not the account"
        );

        // An account with no Plex Home never writes a profile at all — auth's single-user path
        // enters Home on the owner's server token — so an empty uuid IS the owner.
        let solo = Session {
            client_id: "cid".into(),
            account_token: "acct".into(),
            ..Default::default()
        };
        assert!(solo.active_profile_is_admin());

        // …but an unknown uuid is NOT the owner. `home_users` is empty for "never fetched" as much
        // as for "no Plex Home" (see `Session::account`), and on a question whose wrong answer is
        // somebody else's credentials, "cannot prove it" must not read as "yes".
        let mut unknown = home("u-kid");
        unknown.home_users.clear();
        assert!(!unknown.active_profile_is_admin());
        assert!(!home("u-nobody").active_profile_is_admin());
    }

    /// **The stored profile's PIN flag — what the boot picker's BACK is gated on.** The escalation
    /// it exists to close: the adult profile carries the PIN, the app is signed in as them, a child
    /// boots it, and BACK out of the who's-watching picker reinstated that session with no code
    /// entered at all (`auth::cancel`).
    ///
    /// The two "the roster cannot say" answers deliberately disagree with the test above's. An
    /// unknown uuid is NOT the owner, because that question's wrong answer is somebody else's
    /// credentials; the same uuid IS treated as protected, because this question's wrong answer is
    /// a bypassed PIN and being wrong the other way costs one profile pick.
    #[test]
    fn a_stored_profile_behind_a_pin_is_reported_as_protected() {
        let home = |uuid: &str| Session {
            client_id: "cid".into(),
            account_token: "acct".into(),
            user: UserRef {
                uuid: uuid.into(),
                ..Default::default()
            },
            home_users: vec![
                HomeUserRef {
                    uuid: "u-owner".into(),
                    title: "Gleb".into(),
                    admin: true,
                    protected: true,
                    ..Default::default()
                },
                HomeUserRef {
                    uuid: "u-kid".into(),
                    title: "Kid".into(),
                    ..Default::default()
                },
            ],
            ..Default::default()
        };
        assert!(
            home("u-owner").active_profile_is_protected(),
            "the adult tile carries the PIN"
        );
        assert!(
            !home("u-kid").active_profile_is_protected(),
            "a managed profile with no PIN"
        );

        // **A session that names NO profile answers protected too**, which is the half that reads
        // as harmless and is not: it is what a sign-in abandoned at the picker leaves on disk (the
        // account token, the server and the roster are persisted the moment they exist; the pick
        // never happened), and `pms_token()` on it is the OWNER's server token. The very next boot
        // raises a picker over that file — a roster of >1 is exactly what it has — so answering
        // "not protected" here put the owner's credentials behind BACK by a second road.
        let mut abandoned = home("u-owner");
        abandoned.user = UserRef::default();
        assert!(
            abandoned.active_profile_is_protected(),
            "no profile chosen is not 'no PIN to be behind'"
        );
        let solo = Session {
            client_id: "cid".into(),
            account_token: "acct".into(),
            ..Default::default()
        };
        assert!(solo.active_profile_is_protected());

        // …and a uuid the roster does not name is treated as protected.
        let mut unknown = home("u-owner");
        unknown.home_users.clear();
        assert!(unknown.active_profile_is_protected());
        assert!(home("u-nobody").active_profile_is_protected());
    }

    // ---- The FILE half: one writer at a time, and a whole file or none of it -------------------
    //
    // Everything below drives the real `save`/`peek`/`update` against a real file, so it needs a
    // file it may have. `TempSession` redirects [`TEST_FILE`] — a crate global, which is why every
    // test here holds `crate::testlock::serial()` for its whole body (`src/lib.rs`): several
    // modules call `session::load` indirectly, and one running in parallel would read and WRITE
    // the file being graded.

    /// Point this module's file at a directory of this test's own, and take it back on drop.
    struct TempSession {
        dir: std::path::PathBuf,
    }

    impl TempSession {
        fn new(tag: &str) -> TempSession {
            // `env::temp_dir()` is right HERE and wrong in `dev.rs` (whose test warns against it):
            // there a literal path stops meeting a read that resolves its own root, while this
            // test is choosing the path that BOTH halves resolve to.
            let dir = std::env::temp_dir()
                .join(format!("plxnative-session-{}-{tag}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir); // a previous run that died mid-test
            std::fs::create_dir_all(&dir).expect("a writable temp dir");
            super::redirect_for_test(Some(dir.join("auth.json")));
            TempSession { dir }
        }
        fn file(&self) -> std::path::PathBuf {
            self.dir.join("auth.json")
        }
        fn tmp(&self) -> std::path::PathBuf {
            self.dir.join("auth.json.tmp")
        }
    }

    impl Drop for TempSession {
        fn drop(&mut self) {
            super::redirect_for_test(None);
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    fn signed_in() -> Session {
        Session {
            client_id: "cid-1".into(),
            account_token: "acct".into(),
            ..Default::default()
        }
    }

    /// A save lands as a WHOLE file — written to a sibling tmp and renamed over — leaving nothing
    /// behind, and the credentials are never on disk in a mode another uid can read (this box is
    /// rooted and `/media/developer` is world-readable). The tmp is where the secret exists first,
    /// so the 0600 rule has to reach it too.
    #[test]
    fn a_save_lands_whole_and_leaves_no_temporary_behind() {
        use std::os::unix::fs::PermissionsExt;
        let _g = crate::testlock::serial();
        let t = TempSession::new("whole");

        save(&signed_in());
        assert_eq!(peek().account_token, "acct", "and it reads back");
        assert!(
            !t.tmp().exists(),
            "the tmp file is renamed, not left beside the session"
        );
        let mode = std::fs::metadata(t.file()).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "credentials at rest");

        // a sign-out takes the tmp with it: `peek` cannot read one, but a live account token left
        // in a file on a rooted television is not a sign-out
        std::fs::write(t.tmp(), b"{}").unwrap();
        clear();
        assert!(!t.file().exists() && !t.tmp().exists());
    }

    /// **The route ground's one persisted seed.** A fresh device has recorded nothing, a real
    /// hero is remembered across the read-modify-write cycle `update` uses everywhere else, and
    /// recording the SAME envelope again is a no-op rather than a second disk write.
    #[test]
    fn last_hero_blur_round_trips_and_skips_a_redundant_write() {
        let _g = crate::testlock::serial();
        let _t = TempSession::new("last-hero");
        save(&signed_in());
        assert_eq!(last_hero(), None, "a fresh device has shown no hero yet");

        let envelope = [[0.1, 0.2, 0.3]; 4];
        assert!(record_last_hero(envelope), "a new envelope is a real write");
        assert_eq!(last_hero(), Some(envelope));

        assert!(
            !record_last_hero(envelope),
            "recording the same envelope again must not touch the file"
        );

        let second = [[0.9, 0.8, 0.7]; 4];
        assert!(record_last_hero(second), "a genuinely different hero writes");
        assert_eq!(last_hero(), Some(second), "…and replaces the stored one");
    }

    /// A temporary LS2/key-store failure must never turn ciphertext back into plaintext or make
    /// `load` overwrite it with a newly minted, logged-out client id. The host has no Luna bus,
    /// which is the exact unavailable-key condition this policy has to survive.
    #[test]
    fn an_unopenable_secure_session_is_preserved_without_plaintext_downgrade() {
        let _g = crate::testlock::serial();
        let t = TempSession::new("secure-locked");
        let envelope = SecureEnvelope {
            format: SECURE_FORMAT.to_string(),
            version: 1,
            sealed: crate::keymanager::Sealed {
                backend: crate::keymanager::Backend::Keymanager3,
                key: "plxnative.session.v1".to_string(),
                iv: "AAAAAAAAAAAAAAAAAAAAAA==".to_string(),
                data: "c2VjcmV0".to_string(),
            },
        };
        let original = serde_json::to_vec_pretty(&envelope).unwrap();
        std::fs::write(t.file(), &original).unwrap();

        let loaded = load();
        assert!(
            !loaded.client_id.is_empty(),
            "the run still gets an ephemeral id"
        );
        assert_eq!(std::fs::read(t.file()).unwrap(), original);

        save(&signed_in());
        assert_eq!(
            std::fs::read(t.file()).unwrap(),
            original,
            "an unavailable service cannot leak the replacement session as plaintext"
        );
    }

    #[test]
    fn an_unknown_secure_envelope_version_is_locked_and_never_rewritten_as_plaintext() {
        let _g = crate::testlock::serial();
        let t = TempSession::new("secure-future-version");
        let original = br#"{
  "format": "plxnative-secure-session",
  "version": 2,
  "sealed": {
    "backend": "keymanager3",
    "key": "plxnative.session.v2",
    "iv": "future-iv",
    "data": "future-ciphertext"
  }
}"#;
        std::fs::write(t.file(), original).unwrap();

        let loaded = load();
        assert!(
            !loaded.client_id.is_empty(),
            "the run still gets an ephemeral id"
        );
        assert_eq!(
            std::fs::read(t.file()).unwrap(),
            original,
            "rollback must preserve an envelope it does not understand"
        );

        save(&signed_in());
        assert_eq!(
            std::fs::read(t.file()).unwrap(),
            original,
            "a future secure envelope must shadow every plaintext replacement"
        );
    }

    #[test]
    fn a_precreated_tmp_symlink_cannot_redirect_session_bytes() {
        use std::os::unix::fs::symlink;
        let _g = crate::testlock::serial();
        let t = TempSession::new("tmp-symlink");
        let victim = t.dir.join("attacker-readable");
        std::fs::write(&victim, b"unchanged").unwrap();
        symlink(&victim, t.tmp()).unwrap();

        save(&signed_in());

        assert_eq!(std::fs::read(&victim).unwrap(), b"unchanged");
        assert_eq!(peek().account_token, "acct");
    }

    #[test]
    fn a_quality_choice_persists_without_replacing_other_session_state() {
        let _g = crate::testlock::serial();
        let _t = TempSession::new("quality");
        let mut s = signed_in();
        s.sources.push(SourceRef {
            machine_id: "server-a".into(),
            token: "server-token".into(),
            address: "192.168.0.10".into(),
            port: 32400,
            ..Default::default()
        });
        save(&s);

        assert!(update(|cur| Some(
            cur.with_playback_quality(PlaybackQuality::P720)
        )));
        let landed = peek();
        assert_eq!(landed.playback_quality(), PlaybackQuality::P720);
        assert_eq!(landed.account_token, "acct");
        assert_eq!(landed.sources.len(), 1);
        assert_eq!(landed.sources[0].machine_id, "server-a");
    }

    #[test]
    fn loading_legacy_json_without_an_id_repairs_only_the_id_not_the_quality() {
        let _g = crate::testlock::serial();
        let t = TempSession::new("legacy-no-id");
        std::fs::write(t.file(), br#"{"account_token":"legacy-account"}"#).unwrap();

        let loaded = load();
        assert!(
            !loaded.client_id.is_empty(),
            "the ordinary identifier repair still happens"
        );
        assert_eq!(loaded.account_token, "legacy-account");
        assert_eq!(loaded.playback_quality(), PlaybackQuality::Original);
        assert_eq!(
            loaded.playback_quality, None,
            "a parsable old file is not fresh and must not acquire a default choice"
        );

        let saved: Session = serde_json::from_slice(&std::fs::read(t.file()).unwrap()).unwrap();
        assert_eq!(saved.playback_quality(), PlaybackQuality::Original);
        assert_eq!(saved.playback_quality, None);
    }

    #[test]
    fn loading_with_no_file_records_the_gated_fresh_default() {
        let _g = crate::testlock::serial();
        let t = TempSession::new("fresh-quality");
        assert!(!t.file().exists());

        let loaded = load();
        assert_eq!(
            loaded.playback_quality,
            Some(PlaybackQuality::Auto),
            "the production readiness gate gives only a genuinely fresh install Auto"
        );
        let saved: Session = serde_json::from_slice(&std::fs::read(t.file()).unwrap()).unwrap();
        assert_eq!(
            saved.playback_quality,
            Some(PlaybackQuality::Auto),
            "freshness is decided once and stored explicitly"
        );
    }

    /// **Two writers, one file, and neither may lose the other's work.** Each thread runs exactly
    /// the read-modify-write cycle the two real writers run — `auth`'s roster refresh growing
    /// `sources`, the search-recents worker growing one profile's terms — and when they are done
    /// every update from both must be in the file.
    ///
    /// This is the bug in its own shape: the roster worker re-read the file, a profile pick landed
    /// after that read, and its save put the pre-switch profile back — the next boot resuming as
    /// the wrong person. `update` makes the read and the write one step under one lock, so the
    /// interleaving that loses an update cannot be constructed.
    #[test]
    fn concurrent_read_modify_writes_never_lose_an_update() {
        let _g = crate::testlock::serial();
        let _t = TempSession::new("lost-update");
        save(&signed_in());

        // A dozen each is plenty and is deliberately not more: every cycle ends in the `sync_all`
        // that makes the rename mean something, and on this host that is an `F_FULLFSYNC` — the
        // whole host suite is meant to cost well under a second.
        const N: usize = 12;
        std::thread::scope(|sc| {
            sc.spawn(|| {
                for i in 0..N {
                    update(|s| {
                        let mut next = s.clone();
                        next.sources.push(SourceRef {
                            machine_id: format!("m{i}"),
                            address: "192.168.0.10".into(),
                            port: 32400,
                            token: "tok".into(),
                            ..Default::default()
                        });
                        Some(next)
                    });
                }
            });
            sc.spawn(|| {
                for i in 0..N {
                    update(|s| {
                        let mut next = s.clone();
                        let mut terms = next.recents_for("uu-1").to_vec();
                        terms.push(format!("term-{i}"));
                        next.set_recents_for("uu-1", terms);
                        Some(next)
                    });
                }
            });
        });

        let s = peek();
        assert_eq!(s.client_id, "cid-1", "the credentials survived every cycle");
        assert_eq!(s.account_token, "acct");
        assert_eq!(
            s.sources.len(),
            N,
            "a roster entry was overwritten by the other writer"
        );
        assert_eq!(
            s.recents_for("uu-1").len(),
            N,
            "a search term was overwritten by the other writer"
        );
    }

    /// **A reader outside the lock never sees half a session.** The reader here deliberately does
    /// NOT go through `peek` — that takes the same lock, so it could not observe a torn file even
    /// if `save` still truncated in place. It reads the path the way everything else on the device
    /// does, which is also the window a crash or a power cut reads through: with `O_TRUNC` the
    /// bytes at that path are empty for as long as the write takes, and an unparseable session
    /// file is a QR code on the next boot, not a stale roster.
    #[test]
    fn a_reader_outside_the_lock_never_sees_half_a_session() {
        let _g = crate::testlock::serial();
        let t = TempSession::new("torn");
        save(&signed_in());

        let done = std::sync::atomic::AtomicBool::new(false);
        std::thread::scope(|sc| {
            sc.spawn(|| {
                for i in 0..20 {
                    update(|s| {
                        let mut next = s.clone();
                        // a payload big enough that one `write_all` is several pages — a torn read
                        // must not depend on the file happening to be tiny
                        next.home_users.push(HomeUserRef {
                            uuid: format!("uuid-{i}"),
                            title: format!("A profile with a long enough name to be worth {i} bytes"),
                            thumb: format!("https://plex.direct/photo/:/transcode?url=library%2Fmetadata%2F{i}"),
                            ..Default::default()
                        });
                        Some(next)
                    });
                }
                done.store(true, std::sync::atomic::Ordering::Release);
            });
            let file = t.file();
            let mut reads = 0u32;
            while !done.load(std::sync::atomic::Ordering::Acquire) {
                let bytes = std::fs::read(&file).expect("the path always names a complete file");
                let s: Session = serde_json::from_slice(&bytes)
                    .unwrap_or_else(|e| panic!("torn session file after {reads} clean reads: {e}"));
                assert_eq!(
                    s.client_id, "cid-1",
                    "a partial read is a signed-out device"
                );
                reads += 1;
            }
        });
        assert_eq!(peek().home_users.len(), 20);
    }

    /// `update` must never CREATE a session. A missing or unparseable file reads back as a default
    /// `Session`, and writing one field onto that leaves a `client_id`-less file where a live
    /// session used to be — the silent sign-out every list in this struct is soft-parsed to
    /// prevent, arriving instead by the door built to fix it. It is also what a sign-out racing a
    /// background worker would otherwise produce: `clear()` removes the file, and the worker in
    /// flight puts a roster back with no credentials under it.
    #[test]
    fn update_refuses_a_file_that_holds_no_session() {
        let _g = crate::testlock::serial();
        let t = TempSession::new("refuse");

        // no file at all — the state straight after `clear()`
        assert!(!update(|s| Some(Session {
            account_token: "acct".into(),
            ..s.clone()
        })));
        assert!(
            !t.file().exists(),
            "a refused cycle must not create the file it refused to write"
        );

        // a file that does not parse: the same answer, and the bytes are left alone rather than
        // replaced with a freshly minted session
        std::fs::write(t.file(), b"{ not json").unwrap();
        assert!(!update(|_| Some(signed_in())));
        assert_eq!(std::fs::read(t.file()).unwrap(), b"{ not json");
    }

    /// The roster's own leniency must not weaken the roster the picker draws from: a managed user
    /// whose stored `thumb` is a `null` costs that user, not the session.
    #[test]
    fn a_malformed_home_user_costs_that_tile_and_not_the_session() {
        let s: Session = serde_json::from_str(
            r#"{"client_id":"c","home_users":[{"uuid":"a","title":"A","thumb":null},
                                              {"uuid":"b","title":"B","thumb":"","admin":true}]}"#,
        )
        .expect("one bad tile must not fail the file");
        assert_eq!(s.home_users.len(), 1);
        assert_eq!(s.account(None).name.as_deref(), Some("B"));
    }
}
