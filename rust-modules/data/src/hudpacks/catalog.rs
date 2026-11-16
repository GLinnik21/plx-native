#![deny(clippy::wildcard_enum_match_arm)]
//! The registry contract and gallery cache policy. No clock, transport, filesystem or profile IO.
//!
//! Only `Open` may begin a request: booting the package store cannot contact the registry.
//! Results carry a generation, so a late cache read or HTTP reply cannot replace a newer visit.
//! Cache writes are atomic whole-file effects, using the package store's candidate directories.
//!
//! The gallery accepts only a SIGNED index. The registry publishes `index.signed`: one header line, a
//! single `\n`, then the exact bytes of that deploy's `index.json`; the header names the signing key and
//! carries an Ed25519 signature over those bytes. [`verify`] is the one function that turns such a body into
//! an [`Index`], and it runs wherever a body enters: on the network lane for a reply ([`Reply::http`]) and on
//! the storage worker for the cache file ([`CacheRecord::decode`]), never on the frame thread. The accepted
//! public keys are a parameter; production passes [`ACCEPTED_KEYS`], whose second entry is the rotation
//! standby (`docs/hud-plugins.md`, "Trust and safety").
use std::collections::BTreeSet;
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use super::{valid_package_id, Candidate, Loc, ReadError, Semver, Sha, WriteOutcome};

/// One fixed serving origin. URLs in the index are checked against content-addressed paths here.
pub const ORIGIN: &str = "https://huds.plxnative.com";
/// The signed index, served from [`ORIGIN`].
pub const SIGNED_FILE: &str = "index.signed";
/// Bounds one entire catalog response, including renderer provenance. Entries are never truncated.
pub const INDEX_BYTE_LIMIT: usize = 4 * 1024 * 1024;
/// Bounds the header line of a signed index (its `{"format":1,"key":...,"sig":...}` is 174 bytes).
pub const HEADER_BYTE_LIMIT: usize = 256;
/// The whole `index.signed` response: the header line, its newline and the index.
pub const SIGNED_BYTE_LIMIT: usize = HEADER_BYTE_LIMIT + 1 + INDEX_BYTE_LIMIT;
/// The cache adds JSON escaping to the bounded UTF-8 signed body.
pub const CACHE_BYTE_LIMIT: usize = SIGNED_BYTE_LIMIT * 6 + 4096;
const REFRESH_SECONDS: u64 = 24 * 60 * 60;
const CACHE_FILE: &str = "index-cache.json";

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
pub struct Preview {
    pub state: String,
    pub lang: String,
    pub file: String,
    pub sha256: String,
    pub url: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
pub struct FitChange {
    pub from: String,
    pub langs: Vec<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize, Serialize)]
pub struct AppRange {
    pub min: Option<String>,
    pub max: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
pub struct Package {
    pub id: String,
    pub version: String,
    pub package_format: String,
    pub hud_format: String,
    pub name: String,
    pub author: String,
    pub license: String,
    pub app: AppRange,
    pub sha256: String,
    pub bytes: usize,
    pub url: String,
    pub previews: Vec<Preview>,
    pub unfit: Vec<FitChange>,
}

/// Latest HUD document version understood by the runtime and registry filter.
pub const HUD_FORMAT: (u8, u8) = (1, 1);

impl Package {
    pub fn sha(&self) -> Sha {
        // Only the checked parser constructs the machine's packages.
        Sha::parse(&self.sha256).expect("checked catalog hash")
    }

    /// Latest transition at or before this app wins, including a later repaired translation.
    pub fn fits(&self, app: &str, language: &str) -> bool {
        let Some(app) = Semver::app_release(app) else {
            return false;
        };
        self.unfit
            .iter()
            .rev()
            .find(|f| Semver::parse(&f.from).is_some_and(|at| at <= app))
            .is_none_or(|f| !f.langs.iter().any(|l| l == language))
    }

    pub fn compatible(&self, app: &str, language: &str, pack_limit: usize) -> bool {
        let Some(app_version) = Semver::app_release(app) else {
            return false;
        };
        self.package_format == "1.0"
            && (0..=HUD_FORMAT.1).any(|minor| self.hud_format == format!("{}.{minor}", HUD_FORMAT.0))
            && self.bytes <= pack_limit
            && self
                .app
                .min
                .as_deref()
                .and_then(Semver::parse)
                .is_none_or(|v| v <= app_version)
            && self
                .app
                .max
                .as_deref()
                .and_then(Semver::parse)
                .is_none_or(|v| app_version <= v)
            && self.fits(app, language)
    }

    pub fn preview(&self, language: &str) -> Option<&Preview> {
        self.previews
            .iter()
            .find(|p| {
                p.lang == language
                    && matches!(
                        p.state.as_str(),
                        "compact" | "sections-0-movie-paused-no-chapters"
                    )
            })
            .or_else(|| {
                self.previews.iter().find(|p| {
                    p.lang == "en"
                        && matches!(
                            p.state.as_str(),
                            "compact" | "sections-0-movie-paused-no-chapters"
                        )
                })
            })
            .or_else(|| self.previews.first())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
pub struct Revocation {
    pub sha256: String,
    pub reason: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
pub struct Index {
    pub format: u32,
    /// UTC epoch seconds at which the publish run built the index. [`parse`] does not require it (a bare
    /// `index.json` has none in the fixtures); [`verify`] does, and refuses a signed index without one.
    #[serde(default)]
    pub serial: Option<u64>,
    pub entries: Vec<Package>,
    pub revoked: Vec<Revocation>,
}

fn word(value: &str, limit: usize) -> bool {
    !value.trim().is_empty() && value.len() <= limit && !value.chars().any(char::is_control)
}

/// Reject a complete malformed index; never accept a prefix or follow a URL supplied by an author.
pub fn parse(bytes: &[u8]) -> Option<Index> {
    if bytes.len() > INDEX_BYTE_LIMIT {
        return None;
    }
    let index: Index = serde_json::from_slice(bytes).ok()?;
    if index.format != 1 {
        return None;
    }
    let mut ids = BTreeSet::new();
    let mut revoked = BTreeSet::new();
    for r in &index.revoked {
        if Sha::parse(&r.sha256).is_none() || !revoked.insert(&r.sha256) || !word(&r.reason, 1024) {
            return None;
        }
    }
    for p in &index.entries {
        if !valid_package_id(&p.id)
            || !ids.insert(&p.id)
            || Semver::parse(&p.version).is_none()
            || !word(&p.name, 256)
            || !word(&p.author, 256)
            || !word(&p.license, 256)
            || !format_version(&p.package_format)
            || !format_version(&p.hud_format)
            || Sha::parse(&p.sha256).is_none()
            || p.bytes == 0
            || p.url != format!("{ORIGIN}/p/{}.plxhud", p.sha256)
            || p.previews.is_empty()
        {
            return None;
        }
        for bound in [&p.app.min, &p.app.max].into_iter().flatten() {
            Semver::parse(bound)?;
        }
        if let (Some(min), Some(max)) = (&p.app.min, &p.app.max) {
            if Semver::parse(min)? > Semver::parse(max)? {
                return None;
            }
        }
        let mut previous = None;
        for change in &p.unfit {
            let at = Semver::parse(&change.from)?;
            if previous.is_some_and(|old| old >= at) || change.langs.iter().any(|l| !language(l)) {
                return None;
            }
            previous = Some(at);
        }
        for art in &p.previews {
            if Sha::parse(&art.sha256).is_none()
                || art.file != format!("{}.png", art.sha256)
                || art.url != format!("{ORIGIN}/previews/{}/{}", p.sha256, art.file)
                || !word(&art.state, 64)
                || !language(&art.lang)
            {
                return None;
            }
        }
    }
    Some(index)
}

fn format_version(text: &str) -> bool {
    text.split_once('.').is_some_and(|(a, b)| {
        !a.is_empty()
            && !b.is_empty()
            && a.bytes().chain(b.bytes()).all(|c| c.is_ascii_digit())
            && (a.len() == 1 || !a.starts_with('0'))
            && (b.len() == 1 || !b.starts_with('0'))
    })
}

fn language(text: &str) -> bool {
    !text.is_empty()
        && text.len() <= 32
        && text.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'-')
}

/// Which slot of the rotation plan a key holds. Nothing branches on it: both keys verify the same way, and a
/// release changes the table, never this code (`docs/hud-plugins.md`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KeyRole {
    /// Signs today: its private half is the registry repository's `HUD_INDEX_ED25519_KEY` Actions secret.
    Primary,
    /// Held offline by the maintainer for a rotation; every shipped app already accepts it.
    Standby,
}

/// One Ed25519 public key the app accepts, named the way the signed header names it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AcceptedKey {
    pub role: KeyRole,
    /// The first 8 bytes of SHA-256 over `key` (16 lower-case hex digits in the header's `key`).
    pub id: [u8; 8],
    /// The raw 32-byte public key.
    pub key: [u8; 32],
}

/// The keys compiled into the app (the registry mirrors them as `keys.json` for its publisher's self-check).
/// A key leaves this table only in a release after the one that accepts its successor has shipped.
pub const ACCEPTED_KEYS: [AcceptedKey; 2] = [
    AcceptedKey {
        role: KeyRole::Primary,
        id: [0x4f, 0xe9, 0xb5, 0x3b, 0x66, 0x1f, 0xf2, 0xe8],
        key: [
            0x45, 0xef, 0x4d, 0xd7, 0xf0, 0xba, 0x0b, 0x05, 0x31, 0x01, 0x42, 0x45, 0xa4, 0xb9, 0xec, 0xbc,
            0x1b, 0x1c, 0x1e, 0xbc, 0x84, 0x8f, 0xa2, 0xb5, 0xd7, 0xae, 0x5b, 0xc5, 0x37, 0xc6, 0x65, 0x8a,
        ],
    },
    AcceptedKey {
        role: KeyRole::Standby,
        id: [0x43, 0x76, 0x41, 0xd8, 0x03, 0xb6, 0x93, 0x2b],
        key: [
            0xc8, 0x0a, 0x76, 0x13, 0xab, 0xd7, 0x6a, 0x59, 0x6c, 0x4c, 0x21, 0x87, 0x5c, 0x4a, 0xbb, 0x28,
            0x0c, 0x1b, 0x0e, 0x27, 0x9a, 0x79, 0xb2, 0xc4, 0x3e, 0x86, 0xce, 0x9e, 0x0b, 0x41, 0xa7, 0x92,
        ],
    },
];

/// Why a signed index was not accepted: the contract's acceptance steps, in order. [`cause`](Self::cause) is the
/// word the one log line carries.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refusal {
    /// The header or the framing is not exactly the contract's.
    Malformed,
    /// The header names a key this build does not accept.
    UnknownKey,
    /// The signature does not verify over the index bytes.
    BadSignature,
    /// The signed bytes are not an index [`parse`] accepts.
    Index,
    /// The index carries no `serial`.
    NoSerial,
    /// The `serial` is lower than the one already cached.
    Rollback,
}

impl Refusal {
    pub fn cause(self) -> &'static str {
        match self {
            Self::Malformed => "malformed",
            Self::UnknownKey => "unknown-key",
            Self::BadSignature => "bad-signature",
            Self::Index => "index",
            Self::NoSerial => "no-serial",
            Self::Rollback => "rollback",
        }
    }
}

/// A signed index that passed every step but the rollback comparison, which needs the cache.
#[derive(Debug, PartialEq, Eq)]
pub struct Verified {
    pub index: Arc<Index>,
    /// `index.serial`, which [`verify`] guarantees is present.
    pub serial: u64,
    /// The whole `index.signed` body, exactly as received: what the cache stores and re-verifies.
    pub body: Arc<str>,
}

/// The first line of `index.signed`: exactly these three keys, nothing else.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Header {
    format: u32,
    key: String,
    sig: String,
}

/// `text` as exactly `N` bytes of lower-case hex digits.
fn unhex<const N: usize>(text: &str) -> Option<[u8; N]> {
    if text.len() != 2 * N || !text.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')) {
        return None;
    }
    let mut out = [0u8; N];
    for (i, byte) in out.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&text[2 * i..2 * i + 2], 16).ok()?;
    }
    Some(out)
}

/// Accept a signed index, or say why not. The steps are the contract's, in its order, all or nothing: the framing
/// and header parse strictly (the header line at most [`HEADER_BYTE_LIMIT`], the index at most [`INDEX_BYTE_LIMIT`],
/// exactly the keys `format` = 1, `key` and `sig`), `key` names one of `keys`, the signature verifies over the index
/// bytes, [`parse`] accepts them, and they carry a `serial`. The rollback comparison needs the cache, so the machine
/// makes it ([`Catalog::step`]).
///
/// Verification is `ed25519_compact::PublicKey::verify` (pure Ed25519, RFC 8032, no prehash, no context), which is
/// strict: it refuses a signature whose `S` is not canonical (`sc_reject_noncanonical`), and a public key or an `R` that
/// is not a canonical encoding of a point of large order (`decode_point_negated`). Nothing here reads a clock or
/// a random source, and nothing here is called from the frame thread.
pub fn verify(signed: &[u8], keys: &[AcceptedKey]) -> Result<Verified, Refusal> {
    let head = &signed[..signed.len().min(HEADER_BYTE_LIMIT + 1)];
    let newline = head.iter().position(|b| *b == b'\n').ok_or(Refusal::Malformed)?;
    let (header, index) = (&head[..newline], &signed[newline + 1..]);
    if index.len() > INDEX_BYTE_LIMIT || header.first() != Some(&b'{') || header.last() != Some(&b'}') {
        return Err(Refusal::Malformed);
    }
    let header: Header = serde_json::from_slice(header).map_err(|_| Refusal::Malformed)?;
    let (Some(id), Some(signature)) = (unhex::<8>(&header.key), unhex::<64>(&header.sig)) else {
        return Err(Refusal::Malformed);
    };
    if header.format != 1 {
        return Err(Refusal::Malformed);
    }
    let key = keys.iter().find(|k| k.id == id).ok_or(Refusal::UnknownKey)?;
    ed25519_compact::PublicKey::new(key.key)
        .verify(index, &ed25519_compact::Signature::new(signature))
        .map_err(|_| Refusal::BadSignature)?;
    let parsed = parse(index).ok_or(Refusal::Index)?;
    let serial = parsed.serial.ok_or(Refusal::NoSerial)?;
    // The header is UTF-8 JSON and the index parsed as such, so the whole body is text.
    let body = std::str::from_utf8(signed).map_err(|_| Refusal::Malformed)?;
    Ok(Verified { index: Arc::new(parsed), serial, body: body.into() })
}

fn etag(value: Option<String>) -> Option<String> {
    value.filter(|v| !v.is_empty() && v.len() <= 1024 && !v.chars().any(char::is_control))
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Snapshot {
    pub index: Option<Arc<Index>>,
    /// The catalog a detail view may have opened while this visit's one refresh was in flight.
    pub previous: Option<Arc<Index>>,
    /// Last successful validation, UTC Unix seconds; retained after a failed refresh.
    pub checked_at: Option<u64>,
    pub fetching: bool,
    /// The last catalog refresh failed or was interrupted; this says nothing about current connectivity.
    pub refresh_failed: bool,
}

impl Snapshot {
    pub fn package(&self, sha: Sha) -> Option<&Package> {
        self.index
            .iter()
            .chain(self.previous.iter())
            .flat_map(|index| &index.entries)
            .find(|p| p.sha() == sha)
    }

    pub fn revoked(&self, sha: Sha) -> Option<&str> {
        self.index
            .as_ref()?
            .revoked
            .iter()
            .find(|r| r.sha256 == sha.hex())
            .map(|r| r.reason.as_str())
    }
}

#[derive(Deserialize, Serialize)]
struct Cache {
    protocol: u32,
    checked_at: Option<u64>,
    attempted_at: Option<u64>,
    etag: Option<String>,
    body: String,
}

/// A cache decoded and validated on the storage worker. Shared bodies and indexes keep publication
/// and state transitions constant-size; the frame thread never parses or escapes catalog JSON.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CacheRecord {
    checked_at: Option<u64>,
    attempted_at: Option<u64>,
    etag: Option<String>,
    body: Arc<str>,
    index: Option<Arc<Index>>,
}

impl CacheRecord {
    /// A cache file is read like a reply: its body is the whole `index.signed` and is verified again with the same
    /// function, so a file anyone could have written (or one a PR13-era build left, which holds a bare index)
    /// is refused, and with it the ETag it carried: the next request is unconditional. A record that only
    /// remembers an attempt (no body, never validated) has nothing to verify.
    pub fn decode(bytes: &[u8], keys: &[AcceptedKey]) -> Result<Self, Refusal> {
        if bytes.len() > CACHE_BYTE_LIMIT {
            return Err(Refusal::Malformed);
        }
        let cache: Cache = serde_json::from_slice(bytes).map_err(|_| Refusal::Malformed)?;
        if cache.protocol != 1 {
            return Err(Refusal::Malformed);
        }
        let (body, index): (Arc<str>, _) = if cache.body.is_empty() && cache.checked_at.is_none() {
            (cache.body.into(), None)
        } else {
            let verified: Verified = verify(cache.body.as_bytes(), keys)?;
            (verified.body, Some(verified.index))
        };
        Ok(Self {
            checked_at: cache.checked_at,
            attempted_at: cache.attempted_at,
            etag: etag(cache.etag),
            body,
            index,
        })
    }

    /// Called on the storage worker immediately before its atomic write.
    pub fn encode(&self) -> Vec<u8> {
        serde_json::to_vec(&Cache {
            protocol: 1,
            checked_at: self.checked_at,
            attempted_at: self.attempted_at,
            etag: self.etag.clone(),
            body: self.body.to_string(),
        })
        .expect("cache serializes")
    }
}

#[derive(Debug)]
pub enum Event {
    /// Boot reads persisted revocations, with no permission to request the network.
    Load {
        candidates: Vec<Candidate>,
    },
    Open {
        candidates: Vec<Candidate>,
        now: u64,
    },
    Read {
        tag: u64,
        result: Result<CacheRecord, ReadError>,
    },
    Reply {
        tag: u64,
        result: Result<Reply, ()>,
    },
    Written {
        tag: u64,
        outcome: WriteOutcome,
    },
}

#[derive(Debug)]
pub struct Reply {
    status: u16,
    etag: Option<String>,
    /// What [`verify`] made of a 200's body; any other status has none.
    signed: Option<Result<Verified, Refusal>>,
}

impl Reply {
    /// Network-lane decoding, before the addressed result crosses back to the machine: the signature is
    /// checked and the index parsed here, never on the frame thread.
    pub fn http(status: u16, validator: Option<String>, body: &[u8], keys: &[AcceptedKey]) -> Self {
        Self {
            status,
            etag: etag(validator),
            signed: (status == 200).then(|| verify(body, keys)),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CatalogEffect {
    Read {
        tag: u64,
        at: Loc,
    },
    Get {
        tag: u64,
        etag: Option<String>,
    },
    Write {
        tag: u64,
        at: Loc,
        record: CacheRecord,
    },
    /// A reply was refused and the cached state kept: the runner writes the one log line naming the cause.
    Refused {
        cause: Refusal,
    },
}

enum Phase {
    Idle,
    Reading(usize),
    Fetching,
    Writing(usize, bool),
}

pub struct Catalog {
    snapshot: Snapshot,
    candidates: Vec<Candidate>,
    phase: Phase,
    generation: u64,
    now: u64,
    attempted_at: Option<u64>,
    etag: Option<String>,
    body: Arc<str>,
    allow_fetch: bool,
}

impl Default for Catalog {
    fn default() -> Self { Self::new() }
}

impl Catalog {
    pub fn new() -> Self {
        Self {
            snapshot: Snapshot::default(),
            candidates: Vec::new(),
            phase: Phase::Idle,
            generation: 0,
            now: 0,
            attempted_at: None,
            etag: None,
            body: Arc::from(""),
            allow_fetch: false,
        }
    }

fn transition(&mut self, ev: Event, fx: &mut Vec<CatalogEffect>) -> bool {

        match ev {
            Event::Load { candidates } if matches!(self.phase, Phase::Idle) => {
                self.generation = self.generation.wrapping_add(1);
                self.candidates = candidates;
                self.allow_fetch = false;
                {
let (at,): (usize,) = (0,);

        if let Some(candidate) = self.candidates.get(at) {
            let root = candidate.dir.clone();
            { self.phase = Phase::Reading(at); };
            fx.push(CatalogEffect::Read {
                tag: self.generation,
                at: Loc {
                    root,
                    rel: CACHE_FILE.into(),
                },
            });
        } else if self.allow_fetch
            && self
                .attempted_at
                .is_none_or(|at| self.now < at || self.now - at >= REFRESH_SECONDS)
        {
            self.attempted_at = Some(self.now);
            self.snapshot.fetching = true;
            // Persist the attempt before dialing, so a restart during the request keeps backoff.
            {
let (at, fetch,): (usize, bool,) = (0, true,);

        if let Some(candidate) = self.candidates.get(at) {
            let root = candidate.dir.clone();
            let record = CacheRecord {
                checked_at: self.snapshot.checked_at,
                attempted_at: self.attempted_at,
                etag: self.etag.clone(),
                body: Arc::clone(&self.body),
                index: self.snapshot.index.clone(),
            };
            { self.phase = Phase::Writing(at, fetch); };
            fx.push(CatalogEffect::Write {
                tag: self.generation,
                at: Loc {
                    root,
                    rel: CACHE_FILE.into(),
                },
                record,
            });
        } else {
            {
let (fetch,): (bool,) = (fetch,);

        if fetch {
            { self.phase = Phase::Fetching; };
            fx.push(CatalogEffect::Get {
                tag: self.generation,
                etag: self.etag.clone(),
            });
        } else {
            { self.phase = Phase::Idle; };
        }

};
        }

};
        } else {
            { self.phase = Phase::Idle; };
        }

};
                true
            }
            Event::Open { candidates, now } => {
                if matches!(self.phase, Phase::Fetching | Phase::Writing(..)) {
                    return false;
                }
                self.allow_fetch = true;
                self.now = now;
                // A second view subscribes while the same request is pending; it must not restart it.
                if !matches!(self.phase, Phase::Idle) {
                    return false;
                }
                self.generation = self.generation.wrapping_add(1);
                self.now = now;
                self.candidates = candidates;
                {
let (at,): (usize,) = (0,);

        if let Some(candidate) = self.candidates.get(at) {
            let root = candidate.dir.clone();
            { self.phase = Phase::Reading(at); };
            fx.push(CatalogEffect::Read {
                tag: self.generation,
                at: Loc {
                    root,
                    rel: CACHE_FILE.into(),
                },
            });
        } else if self.allow_fetch
            && self
                .attempted_at
                .is_none_or(|at| self.now < at || self.now - at >= REFRESH_SECONDS)
        {
            self.attempted_at = Some(self.now);
            self.snapshot.fetching = true;
            // Persist the attempt before dialing, so a restart during the request keeps backoff.
            {
let (at, fetch,): (usize, bool,) = (0, true,);

        if let Some(candidate) = self.candidates.get(at) {
            let root = candidate.dir.clone();
            let record = CacheRecord {
                checked_at: self.snapshot.checked_at,
                attempted_at: self.attempted_at,
                etag: self.etag.clone(),
                body: Arc::clone(&self.body),
                index: self.snapshot.index.clone(),
            };
            { self.phase = Phase::Writing(at, fetch); };
            fx.push(CatalogEffect::Write {
                tag: self.generation,
                at: Loc {
                    root,
                    rel: CACHE_FILE.into(),
                },
                record,
            });
        } else {
            {
let (fetch,): (bool,) = (fetch,);

        if fetch {
            { self.phase = Phase::Fetching; };
            fx.push(CatalogEffect::Get {
                tag: self.generation,
                etag: self.etag.clone(),
            });
        } else {
            { self.phase = Phase::Idle; };
        }

};
        }

};
        } else {
            { self.phase = Phase::Idle; };
        }

};
                true
            }
            Event::Read { tag, result }
                if tag == self.generation && matches!(self.phase, Phase::Reading(_)) =>
            {
                let &Phase::Reading(at) = &self.phase else {
                    unreachable!()
                };
                if let Ok(cache) = result {
                    if cache.attempted_at >= self.attempted_at {
                        self.snapshot.index = cache.index;
                        self.snapshot.checked_at = cache.checked_at;
                        self.snapshot.refresh_failed =
                            cache.attempted_at > cache.checked_at || cache.checked_at.is_none();
                        self.attempted_at = cache.attempted_at;
                        self.etag = cache.etag;
                        self.body = cache.body;
                    }
                }
                {
let (at,): (usize,) = (at + 1,);

        if let Some(candidate) = self.candidates.get(at) {
            let root = candidate.dir.clone();
            { self.phase = Phase::Reading(at); };
            fx.push(CatalogEffect::Read {
                tag: self.generation,
                at: Loc {
                    root,
                    rel: CACHE_FILE.into(),
                },
            });
        } else if self.allow_fetch
            && self
                .attempted_at
                .is_none_or(|at| self.now < at || self.now - at >= REFRESH_SECONDS)
        {
            self.attempted_at = Some(self.now);
            self.snapshot.fetching = true;
            // Persist the attempt before dialing, so a restart during the request keeps backoff.
            {
let (at, fetch,): (usize, bool,) = (0, true,);

        if let Some(candidate) = self.candidates.get(at) {
            let root = candidate.dir.clone();
            let record = CacheRecord {
                checked_at: self.snapshot.checked_at,
                attempted_at: self.attempted_at,
                etag: self.etag.clone(),
                body: Arc::clone(&self.body),
                index: self.snapshot.index.clone(),
            };
            { self.phase = Phase::Writing(at, fetch); };
            fx.push(CatalogEffect::Write {
                tag: self.generation,
                at: Loc {
                    root,
                    rel: CACHE_FILE.into(),
                },
                record,
            });
        } else {
            {
let (fetch,): (bool,) = (fetch,);

        if fetch {
            { self.phase = Phase::Fetching; };
            fx.push(CatalogEffect::Get {
                tag: self.generation,
                etag: self.etag.clone(),
            });
        } else {
            { self.phase = Phase::Idle; };
        }

};
        }

};
        } else {
            { self.phase = Phase::Idle; };
        }

};
                true
            }
            Event::Reply { tag, result }
                if tag == self.generation && matches!(self.phase, Phase::Fetching) =>
            {
                self.snapshot.fetching = false;
                self.snapshot.refresh_failed = true;
                if let Ok(reply) = result {
                    if reply.status == 304 && self.snapshot.index.is_some() && self.etag.is_some() {
                        self.snapshot.checked_at = Some(self.now);
                        self.snapshot.refresh_failed = false;
                        if let Some(validator) = etag(reply.etag) { self.etag = Some(validator); }
                    } else if let Some(signed) = reply.signed {
                        // The one rule that needs the cache: an index may not be older than the verified one held.
                        let cached = self.snapshot.index.as_ref().and_then(|index| index.serial);
                        match signed {
                            Ok(v) if cached.is_none_or(|held| v.serial >= held) => {
                                self.snapshot.previous = self.snapshot.index.replace(v.index);
                                self.snapshot.checked_at = Some(self.now);
                                self.snapshot.refresh_failed = false;
                                self.etag = etag(reply.etag);
                                self.body = v.body;
                            }
                            Ok(_) => fx.push(CatalogEffect::Refused { cause: Refusal::Rollback }),
                            Err(cause) => fx.push(CatalogEffect::Refused { cause }),
                        }
                    }
                }
                {
let (at, fetch,): (usize, bool,) = (0, false,);

        if let Some(candidate) = self.candidates.get(at) {
            let root = candidate.dir.clone();
            let record = CacheRecord {
                checked_at: self.snapshot.checked_at,
                attempted_at: self.attempted_at,
                etag: self.etag.clone(),
                body: Arc::clone(&self.body),
                index: self.snapshot.index.clone(),
            };
            { self.phase = Phase::Writing(at, fetch); };
            fx.push(CatalogEffect::Write {
                tag: self.generation,
                at: Loc {
                    root,
                    rel: CACHE_FILE.into(),
                },
                record,
            });
        } else {
            {
let (fetch,): (bool,) = (fetch,);

        if fetch {
            { self.phase = Phase::Fetching; };
            fx.push(CatalogEffect::Get {
                tag: self.generation,
                etag: self.etag.clone(),
            });
        } else {
            { self.phase = Phase::Idle; };
        }

};
        }

};
                true
            }
            Event::Written { tag, outcome }
                if tag == self.generation && matches!(self.phase, Phase::Writing(..)) =>
            {
                let &Phase::Writing(at, fetch) = &self.phase else {
                    unreachable!()
                };
                if outcome == WriteOutcome::Done {
                    {
let (fetch,): (bool,) = (fetch,);

        if fetch {
            { self.phase = Phase::Fetching; };
            fx.push(CatalogEffect::Get {
                tag: self.generation,
                etag: self.etag.clone(),
            });
        } else {
            { self.phase = Phase::Idle; };
        }

}
                } else {
                    {
let (at, fetch,): (usize, bool,) = (at + 1, fetch,);

        if let Some(candidate) = self.candidates.get(at) {
            let root = candidate.dir.clone();
            let record = CacheRecord {
                checked_at: self.snapshot.checked_at,
                attempted_at: self.attempted_at,
                etag: self.etag.clone(),
                body: Arc::clone(&self.body),
                index: self.snapshot.index.clone(),
            };
            { self.phase = Phase::Writing(at, fetch); };
            fx.push(CatalogEffect::Write {
                tag: self.generation,
                at: Loc {
                    root,
                    rel: CACHE_FILE.into(),
                },
                record,
            });
        } else {
            {
let (fetch,): (bool,) = (fetch,);

        if fetch {
            { self.phase = Phase::Fetching; };
            fx.push(CatalogEffect::Get {
                tag: self.generation,
                etag: self.etag.clone(),
            });
        } else {
            { self.phase = Phase::Idle; };
        }

};
        }

}
                }
                true
            }
            Event::Load { .. } | Event::Read { .. } | Event::Reply { .. } | Event::Written { .. } => false,
        }

}
    pub fn snapshot(&self) -> &Snapshot {
        &self.snapshot
    }

    pub fn step(&mut self, ev: Event, fx: &mut Vec<CatalogEffect>) -> bool { self.transition(ev, fx) }






}

#[cfg(test)]
pub(crate) mod testing;
#[cfg(test)]
mod tests;
