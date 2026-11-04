//! The registry contract and gallery cache policy. No clock, transport, filesystem or profile IO.
//!
//! Only `Open` may begin a request: booting the package store cannot contact the registry.
//! Results carry a generation, so a late cache read or HTTP reply cannot replace a newer visit.
//! Cache writes are atomic whole-file effects, using the package store's candidate directories.
use std::collections::BTreeSet;
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use super::{valid_package_id, Candidate, Loc, ReadError, Semver, Sha, WriteOutcome};

/// One fixed serving origin. URLs in the index are checked against content-addressed paths here.
pub const ORIGIN: &str = "https://glinnik21.github.io/plxnative-huds";
/// Bounds one entire catalog response, including renderer provenance. Entries are never truncated.
pub const INDEX_BYTE_LIMIT: usize = 4 * 1024 * 1024;
/// The cache adds JSON escaping to the bounded UTF-8 index body.
pub const CACHE_BYTE_LIMIT: usize = INDEX_BYTE_LIMIT * 6 + 4096;
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
            && self.hud_format == "1.0"
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
    pub fn decode(bytes: &[u8]) -> Option<Self> {
        if bytes.len() > CACHE_BYTE_LIMIT {
            return None;
        }
        let cache: Cache = serde_json::from_slice(bytes).ok()?;
        if cache.protocol != 1 {
            return None;
        }
        let index = if cache.body.is_empty() && cache.checked_at.is_none() {
            None
        } else {
            Some(Arc::new(parse(cache.body.as_bytes())?))
        };
        Some(Self {
            checked_at: cache.checked_at,
            attempted_at: cache.attempted_at,
            etag: etag(cache.etag),
            body: cache.body.into(),
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
    body: Arc<str>,
    index: Option<Arc<Index>>,
}

impl Reply {
    /// Network-lane decoding, before the addressed result crosses back to the machine.
    pub fn http(status: u16, validator: Option<String>, body: Vec<u8>) -> Self {
        let index = (status == 200)
            .then(|| parse(&body))
            .flatten()
            .map(Arc::new);
        let body = if index.is_some() {
            String::from_utf8(body).expect("JSON is UTF-8")
        } else {
            String::new()
        };
        Self {
            status,
            etag: etag(validator),
            body: body.into(),
            index,
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
    fn default() -> Self {
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
}

impl Catalog {
    pub fn snapshot(&self) -> &Snapshot {
        &self.snapshot
    }

    pub fn step(&mut self, ev: Event, fx: &mut Vec<CatalogEffect>) -> bool {
        match ev {
            Event::Load { candidates } if matches!(self.phase, Phase::Idle) => {
                self.generation = self.generation.wrapping_add(1);
                self.candidates = candidates;
                self.allow_fetch = false;
                self.read(0, fx);
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
                self.read(0, fx);
                true
            }
            Event::Read { tag, result }
                if tag == self.generation && matches!(self.phase, Phase::Reading(_)) =>
            {
                let Phase::Reading(at) = self.phase else {
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
                self.read(at + 1, fx);
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
                        self.etag = etag(reply.etag).or_else(|| self.etag.take());
                    } else if reply.status == 200 {
                        if let Some(index) = reply.index {
                            self.snapshot.previous = self.snapshot.index.replace(index);
                            self.snapshot.checked_at = Some(self.now);
                            self.snapshot.refresh_failed = false;
                            self.etag = etag(reply.etag);
                            self.body = reply.body;
                        }
                    }
                }
                self.write(0, false, fx);
                true
            }
            Event::Written { tag, outcome }
                if tag == self.generation && matches!(self.phase, Phase::Writing(..)) =>
            {
                let Phase::Writing(at, fetch) = self.phase else {
                    unreachable!()
                };
                if outcome == WriteOutcome::Done {
                    self.after_write(fetch, fx)
                } else {
                    self.write(at + 1, fetch, fx)
                }
                true
            }
            _ => false,
        }
    }

    fn read(&mut self, at: usize, fx: &mut Vec<CatalogEffect>) {
        if let Some(candidate) = self.candidates.get(at) {
            self.phase = Phase::Reading(at);
            fx.push(CatalogEffect::Read {
                tag: self.generation,
                at: Loc {
                    root: candidate.dir.clone(),
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
            self.write(0, true, fx);
        } else {
            self.phase = Phase::Idle;
        }
    }

    fn after_write(&mut self, fetch: bool, fx: &mut Vec<CatalogEffect>) {
        if fetch {
            self.phase = Phase::Fetching;
            fx.push(CatalogEffect::Get {
                tag: self.generation,
                etag: self.etag.clone(),
            });
        } else {
            self.phase = Phase::Idle;
        }
    }

    fn write(&mut self, at: usize, fetch: bool, fx: &mut Vec<CatalogEffect>) {
        if let Some(candidate) = self.candidates.get(at) {
            let record = CacheRecord {
                checked_at: self.snapshot.checked_at,
                attempted_at: self.attempted_at,
                etag: self.etag.clone(),
                body: Arc::clone(&self.body),
                index: self.snapshot.index.clone(),
            };
            self.phase = Phase::Writing(at, fetch);
            fx.push(CatalogEffect::Write {
                tag: self.generation,
                at: Loc {
                    root: candidate.dir.clone(),
                    rel: CACHE_FILE.into(),
                },
                record,
            });
        } else {
            self.after_write(fetch, fx);
        }
    }
}

#[cfg(test)]
mod tests;
