//! Subtitle search & download for the PLAYING item — the model behind the player's Subtitles →
//! Search page. The server contract is `docs/pms-api.md` §8; read it before changing a job here,
//! because four of its findings are load-bearing in this file:
//!
//! * **Candidate keys are ephemeral.** Each search mints new ids and a stale key is accepted and
//!   silently does nothing. So a download names its result set by GENERATION
//!   ([`SubSearchCmd::Download`]'s `gen`), and a press against a replaced set is refused here
//!   rather than sent.
//! * **The download answers 200 with an EMPTY body**, so nothing names the stream it creates. The
//!   download job reads the item's subtitle stream ids BEFORE the PUT, and the stream is found
//!   afterwards by diffing ([`identify`]).
//! * **The install is ASYNCHRONOUS.** The 200 means accepted; the stream appears on a later read.
//!   So an accepted download WAITS and re-reads the item every [`POLL_FRAMES`], up to
//!   [`POLL_ATTEMPTS`] times, and an install that never shows is [`DownloadPhase::Unconfirmed`] —
//!   honest, not a failure: the file may well be on the server, we just could not see it land.
//! * **A 3-letter language code crashes the server with a 500.** [`fold_language`] maps whatever
//!   the caller has (`"nld"`, `"en-GB"`) onto the 2-letter code first, and a language with no
//!   2-letter spelling is refused locally ([`SearchFailure::BadLanguage`]) instead of being sent.
//!
//! ## Shape
//!
//! The house idiom (`search.rs`'s module doc names it): a generation, ONE single-flight
//! [`crate::stores::Fetch`] mailbox, a per-frame pump, a backoff counter. Search, download and the
//! install poll are three kinds of job through that one mailbox, one at a time — they can never
//! overlap, because a download is only accepted on a READY result set and the poll only follows an
//! accepted download. The landing is a multi-kind enum after `person.rs`'s `Landing`, and every
//! arm keeps a failure distinguishable from an empty answer: "the agent found nothing" is
//! [`SearchStatus::Ready`] with no hits, never [`SearchStatus::Failed`].
//!
//! `ServerId` and the rating key are captured at the spawn site and the `&'static Client` resolved
//! there (`plex/CLAUDE.md`'s rule), so a worker cannot search a server the viewer has left.
use plx_plex::plex::subtitles::{SubtitleAddOutcome, SubtitleSearchOutcome};
use plx_plex::plex::{Client, ServerId, SubtitleSearch};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::Arc;

/// Frames between reads of the item while an accepted download installs (~1.5 s at 60 fps). The
/// first read waits this long too: the probe saw a read straight after the PUT miss the stream.
pub const POLL_FRAMES: u32 = 90;
/// Reads before an accepted download that never appears is called [`DownloadPhase::Unconfirmed`]
/// (~15 s in all). A bound, not a measurement: the probe saw the install land within one
/// human-paced retry, and an agent fetching from OpenSubtitles can be slower than that.
pub const POLL_ATTEMPTS: u32 = 10;
/// The spacing applied when a spawn cannot happen (no client registered for the server yet).
const RETRY_FRAMES: u32 = 120;

/// One candidate the agent offered, projected on the WORKER so no wire DTO crosses the mailbox.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SubHit {
    /// The download handle — valid only for the search that produced it (module doc).
    pub key: String,
    /// The provider's release name. Also how [`identify`] recognises the stream it installs.
    pub title: String,
    pub provider: String,
    /// The server's display name for the language ("Nederlands").
    pub language: String,
    /// The server's 3-letter code ("nld"). Display/matching only — never sent back as a query.
    pub language_code: String,
    pub codec: String,
    pub score: i64,
    pub hearing_impaired: bool,
    pub forced: bool,
}

/// What the page should be saying about the SEARCH — not the same question as "are there hits".
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SearchStatus {
    /// Nothing open.
    Idle,
    /// A search is owed or out, and no answer for it has arrived.
    Searching,
    /// The server answered — possibly with no hits, which is an ANSWER ("no subtitles found").
    Ready,
    /// A fault; [`SubSearchView::failure`] says which.
    Failed,
}

/// Why a search failed. The page draws each differently: only `Transport` offers a retry, because
/// a 403 retried is a 403.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SearchFailure {
    Transport,
    Denied,
    Missing,
    /// No 2-letter code exists for the language, so nothing was sent (module doc).
    BadLanguage,
}

/// The stream an accepted download created, once a re-read has shown it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InstalledStream {
    pub id: i64,
    /// `/library/streams/{id}` — the sidecar key the player's existing fetch already renders.
    pub key: String,
    pub codec: String,
    pub language_code: String,
}

/// One download's progress. `hit` indexes the result set of the generation it was pressed on.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DownloadPhase {
    None,
    /// The PUT is owed or out.
    Sending { hit: usize },
    /// The server accepted it; re-reading the item until the new stream shows. `before` is the
    /// item's subtitle stream ids read just ahead of the PUT.
    Waiting { hit: usize, before: Vec<i64>, attempts: u32 },
    Installed { hit: usize, stream: InstalledStream },
    /// Accepted, but no new stream showed within [`POLL_ATTEMPTS`] reads.
    Unconfirmed { hit: usize },
    /// Refused (`denied`) or never delivered. The hits stay on screen — one bad file is not a
    /// failed search.
    Failed { hit: usize, denied: bool },
}

impl DownloadPhase {
    /// A download is on the wire or installing — no second one, and no language change, until it
    /// settles.
    pub fn busy(&self) -> bool {
        matches!(self, DownloadPhase::Sending { .. } | DownloadPhase::Waiting { .. })
    }
}

/// Every mutation of the subtitle-search model.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SubSearchCmd {
    /// Open (or keep) the search for one item. Idempotent for the same item and language, so the
    /// menu re-opening shows the answer already paid for instead of searching again.
    Open { sid: ServerId, rk: String, lang: String },
    /// Search again in another language. Refused while a download is busy.
    SetLanguage(String),
    /// Install hit `hit` of the result set published under generation `gen`. Refused when that set
    /// has been replaced — its keys are dead (module doc).
    Download { gen: u32, hit: usize },
    /// Search again after a TRANSPORT failure. Nothing else retries.
    Retry,
    /// The play ended: drop the model. A download the server already accepted still lands on the
    /// server; only our tracking of it ends.
    Close,
    Reset,
}

struct Open {
    sid: ServerId,
    rk: String,
    /// The folded 2-letter code; empty only when the fold failed.
    lang: String,
    status: SearchStatus,
    failure: Option<SearchFailure>,
    hits: Vec<SubHit>,
    download: DownloadPhase,
}

impl Open {
    fn new(sid: ServerId, rk: String, lang: &str) -> Self {
        let mut open = Open { sid, rk, lang: String::new(), status: SearchStatus::Searching,
            failure: None, hits: Vec::new(), download: DownloadPhase::None };
        open.search_for(lang);
        open
    }

    /// Start (or refuse) a search for `lang` on this item.
    fn search_for(&mut self, lang: &str) {
        self.hits.clear();
        self.download = DownloadPhase::None;
        match fold_language(lang) {
            Some(code) => {
                self.lang = code;
                self.status = SearchStatus::Searching;
                self.failure = None;
            }
            None => {
                self.lang = String::new();
                self.status = SearchStatus::Failed;
                self.failure = Some(SearchFailure::BadLanguage);
            }
        }
    }
}

/// The 2-letter code PMS will accept for `lang`, or `None`. [`crate::metadata::two_letter_code`]
/// finds the ISO 639-1 spelling in the same table `lang_key` groups by (`"nld"`/`"dut"` → `"nl"`,
/// `"en-GB"` → `"en"`), and the plex layer's guard then refuses anything still not two letters.
///
/// NOT `lang_key`: its key is the lexicographically smallest spelling, which for Dutch is
/// `"dut"` — a stable grouping key, and exactly the kind of 3-letter code that 500s the server.
pub fn fold_language(lang: &str) -> Option<String> {
    let code = crate::metadata::two_letter_code(lang)?;
    plx_plex::plex::subtitles::query_language(&code)
}

/// The language a search opens on: the viewer's subtitle preference, else the first of their other
/// languages (`yours`, in preference order), else English — each candidate only if it
/// [`fold_language`]s, so a preference with no two-letter code falls through to one that can be
/// searched instead of opening on a refusal.
pub fn default_language<S: AsRef<str>>(pref: Option<&str>, yours: impl IntoIterator<Item = S>) -> String {
    pref.into_iter()
        .map(str::to_string)
        .chain(yours.into_iter().map(|l| l.as_ref().to_string()))
        .find(|l| fold_language(l).is_some())
        .unwrap_or_else(|| "en".to_string())
}

/// A read-only view of the model, borrowed per frame like `MetadataView`.
#[derive(Clone, Copy)]
pub struct SubSearchView<'a> {
    open: Option<&'a Open>,
    gen: u32,
}

impl<'a> SubSearchView<'a> {
    pub fn status(self) -> SearchStatus { self.open.map_or(SearchStatus::Idle, |o| o.status) }
    pub fn failure(self) -> Option<SearchFailure> { self.open.and_then(|o| o.failure) }
    pub fn hits(self) -> &'a [SubHit] { self.open.map_or(&[], |o| &o.hits) }
    /// The folded 2-letter code being searched; empty when nothing is open or the fold failed.
    pub fn lang(self) -> &'a str { self.open.map_or("", |o| &o.lang) }
    pub fn download(self) -> Option<&'a DownloadPhase> { self.open.map(|o| &o.download) }
    pub fn item(self) -> Option<(ServerId, &'a str)> { self.open.map(|o| (o.sid, o.rk.as_str())) }
    /// The generation a [`SubSearchCmd::Download`] must quote.
    pub fn gen(self) -> u32 { self.gen }

    /// Nothing open — what a host that offers no subtitle search hands a screen.
    pub const IDLE: SubSearchView<'static> = SubSearchView { open: None, gen: 0 };

    /// An owned, comparable copy of the model AS IT CONCERNS `(sid, rk)`: the model holds one item
    /// (the last one searched), and a search that belongs to a different item — the previous
    /// episode's — must read as nothing open, never as this item's results.
    pub fn snapshot_for(self, sid: ServerId, rk: &str) -> SubSearchSnapshot {
        let mine = self.open.filter(|o| plx_plex::plex::same_item((o.sid, &o.rk), (sid, rk)));
        match mine {
            None => SubSearchSnapshot::idle(self.gen),
            Some(o) => SubSearchSnapshot {
                status: o.status, failure: o.failure, hits: o.hits.clone(), lang: o.lang.clone(),
                download: o.download.clone(), gen: self.gen,
            },
        }
    }
}

/// What a panel keeps of the model for one item, compared per frame so a landing refreshes the page
/// it is on and nothing else does. `gen` is the generation a download press must quote.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SubSearchSnapshot {
    pub status: SearchStatus,
    pub failure: Option<SearchFailure>,
    pub hits: Vec<SubHit>,
    pub lang: String,
    pub download: DownloadPhase,
    pub gen: u32,
}

impl SubSearchSnapshot {
    pub fn idle(gen: u32) -> Self {
        SubSearchSnapshot { status: SearchStatus::Idle, failure: None, hits: Vec::new(),
            lang: String::new(), download: DownloadPhase::None, gen }
    }
}

#[derive(Default)]
pub struct SubSearchState {
    open: Option<Open>,
    generation: u32,
    retry_cd: u32,
}

impl SubSearchState {
    pub fn view(&self) -> SubSearchView<'_> {
        SubSearchView { open: self.open.as_ref(), gen: self.generation }
    }

    pub fn generation(&self) -> u32 { self.generation }

    /// Bump the generation (a late landing is discarded), drop the mailbox and release its claim.
    /// It does not stop a running worker — `stores::Fetch`'s doc prices that.
    fn supersede(&mut self, adapter: &SubSearchAdapter) {
        self.generation = self.generation.wrapping_add(1);
        adapter.fetch.clear();
        self.retry_cd = 0;
    }

    pub fn run(&mut self, adapter: &SubSearchAdapter, cmd: SubSearchCmd) -> bool {
        match cmd {
            SubSearchCmd::Open { sid, rk, lang } => {
                if let Some(open) = self.open.as_ref()
                    .filter(|o| plx_plex::plex::same_item((o.sid, &o.rk), (sid, &rk))) {
                    // the same item: keep the answer unless the language really changed
                    if fold_language(&lang).unwrap_or_default() == open.lang { return false; }
                    return self.run(adapter, SubSearchCmd::SetLanguage(lang));
                }
                self.supersede(adapter);
                self.open = Some(Open::new(sid, rk, &lang));
                true
            }
            SubSearchCmd::SetLanguage(lang) => {
                let Some(open) = self.open.as_ref() else { return false };
                if open.download.busy() { return false; }
                if fold_language(&lang).is_some_and(|code| code == open.lang) { return false; }
                self.supersede(adapter);
                self.open.as_mut().unwrap().search_for(&lang);
                true
            }
            SubSearchCmd::Download { gen, hit } => {
                let Some(open) = self.open.as_mut() else { return false };
                if gen != self.generation || open.status != SearchStatus::Ready
                    || hit >= open.hits.len() || open.download.busy() {
                    return false;
                }
                open.download = DownloadPhase::Sending { hit };
                self.retry_cd = 0;
                true
            }
            SubSearchCmd::Retry => {
                let Some(open) = self.open.as_mut() else { return false };
                if open.failure != Some(SearchFailure::Transport) { return false; }
                open.status = SearchStatus::Searching;
                open.failure = None;
                self.retry_cd = 0;
                true
            }
            SubSearchCmd::Close | SubSearchCmd::Reset => {
                self.supersede(adapter);
                self.open.take().is_some()
            }
        }
    }

    /// Once a frame: count the backoff down, land whatever arrived, spawn what is owed.
    pub fn pump_with_gate(&mut self, adapter: &Arc<SubSearchAdapter>,
        gate: &plx_machine::landgate::Gate) -> bool {
        if self.retry_cd > 0 { self.retry_cd -= 1; }
        let mut changed = false;
        let mail = crate::stores::take_landing(gate, crate::stores::StoreId::SubtitleSearch,
            || adapter.fetch.take());
        if let Some(mail) = mail {
            // every landing repaints, the failure branch included — a spinner already answered
            // must not wait for the next keypress
            plx_machine::idle::invalidate();
            if mail.gen == self.generation { changed |= self.apply(mail.what); }
        }
        self.maybe_spawn(adapter);
        changed
    }

    fn job(&self) -> Option<Job> {
        let open = self.open.as_ref()?;
        if open.status == SearchStatus::Searching {
            return Some(Job::Search { lang: open.lang.clone() });
        }
        match &open.download {
            DownloadPhase::Sending { hit } => Some(Job::Download { key: open.hits[*hit].key.clone() }),
            DownloadPhase::Waiting { .. } => Some(Job::Refresh),
            _ => None,
        }
    }

    fn maybe_spawn(&mut self, adapter: &Arc<SubSearchAdapter>) {
        if adapter.fetch.busy() || self.retry_cd > 0 { return; }
        let Some(job) = self.job() else { return };
        let open = self.open.as_ref().expect("a job implies an open item");
        let (sid, rk) = (open.sid, open.rk.clone());
        let Some(client) = plx_plex::plex::client_for(sid) else {
            // no client for this server (yet): back off rather than spin; nothing is claimed
            self.retry_cd = RETRY_FRAMES;
            return;
        };
        let generation = self.generation;
        let fallback = job.failure();
        adapter.fetch.claim();
        let worker = Arc::clone(adapter);
        let spawned = plx_base::task::spawn_small("subsearch", move || {
            // the mailbox is filled OUTSIDE the guard, so a panicking job lands as a FAILURE and
            // never as "the agent found nothing"
            let what = catch_unwind(AssertUnwindSafe(|| run_job(client, &rk, job)))
                .unwrap_or(fallback);
            worker.land(generation, what);
        });
        if !spawned {
            // nothing will ever land to release this claim; without the release the page would
            // sit on a spinner forever. The pump retries after the backoff.
            adapter.fetch.release();
            self.retry_cd = RETRY_FRAMES;
        }
    }

    fn apply(&mut self, landing: Landing) -> bool {
        let Some(open) = self.open.as_mut() else { return false };
        match landing {
            Landing::Search(result) => {
                if open.status != SearchStatus::Searching { return false; }
                match result {
                    Ok(hits) => {
                        open.hits = hits;
                        open.status = SearchStatus::Ready;
                        open.failure = None;
                    }
                    Err(failure) => {
                        open.hits.clear();
                        open.status = SearchStatus::Failed;
                        open.failure = Some(failure);
                    }
                }
                true
            }
            Landing::Download(result) => {
                let DownloadPhase::Sending { hit } = open.download else { return false };
                open.download = match result {
                    Ok(before) => {
                        // the install is asynchronous: give it a beat before the first read
                        self.retry_cd = POLL_FRAMES;
                        DownloadPhase::Waiting { hit, before, attempts: 0 }
                    }
                    Err(denied) => DownloadPhase::Failed { hit, denied },
                };
                true
            }
            Landing::Streams(rows) => {
                let DownloadPhase::Waiting { hit, before, attempts } = &open.download else {
                    return false;
                };
                let (hit, attempts) = (*hit, *attempts + 1);
                let title = open.hits.get(hit).map_or("", |h| h.title.as_str());
                let found = rows.as_deref().and_then(|rows| identify(rows, before, title));
                open.download = match found {
                    Some(stream) => DownloadPhase::Installed { hit, stream },
                    None if attempts >= POLL_ATTEMPTS => DownloadPhase::Unconfirmed { hit },
                    None => {
                        self.retry_cd = POLL_FRAMES;
                        let before = std::mem::take(match &mut open.download {
                            DownloadPhase::Waiting { before, .. } => before,
                            _ => unreachable!(),
                        });
                        DownloadPhase::Waiting { hit, before, attempts }
                    }
                };
                true
            }
        }
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn apply_for_test(&mut self, landing: Landing) -> bool { self.apply(landing) }
    #[cfg(any(test, feature = "test-support"))]
    pub fn job_for_test(&self) -> Option<Job> { self.job() }
    #[cfg(any(test, feature = "test-support"))]
    pub fn retry_for_test(&self) -> u32 { self.retry_cd }
}

/// One subtitle stream of the item, as the install poll reads it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StreamRow {
    pub id: i64,
    pub key: String,
    pub codec: String,
    pub language_code: String,
    pub title: String,
}

/// Which of `rows` is the stream our download created. **Pure** — the rule the probe forced: the
/// PUT names nothing, so the answer is a stream that was not there before. Among several new ones,
/// the one carrying the picked candidate's release name wins (PMS copies it into `title`); failing
/// that, a lone new stream is ours; failing that, the newest id. No new stream → `None`.
pub fn identify(rows: &[StreamRow], before: &[i64], picked_title: &str) -> Option<InstalledStream> {
    let new: Vec<&StreamRow> = rows.iter()
        .filter(|r| !before.contains(&r.id) && !r.key.is_empty())
        .collect();
    let row = (!picked_title.is_empty())
        .then(|| new.iter().find(|r| r.title == picked_title).copied())
        .flatten()
        .or_else(|| (new.len() == 1).then(|| new[0]))
        .or_else(|| new.iter().max_by_key(|r| r.id).copied())?;
    Some(InstalledStream { id: row.id, key: row.key.clone(), codec: row.codec.clone(),
        language_code: row.language_code.clone() })
}

#[derive(Debug, PartialEq, Eq)]
pub enum Job {
    Search { lang: String },
    Download { key: String },
    Refresh,
}

impl Job {
    /// What this job lands as when its worker panics: always a failure of ITS kind.
    fn failure(&self) -> Landing {
        match self {
            Job::Search { .. } => Landing::Search(Err(SearchFailure::Transport)),
            Job::Download { .. } => Landing::Download(Err(false)),
            Job::Refresh => Landing::Streams(None),
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum Landing {
    Search(Result<Vec<SubHit>, SearchFailure>),
    /// `Ok` = accepted, carrying the item's subtitle stream ids read before the PUT. `Err(denied)`.
    Download(Result<Vec<i64>, bool>),
    /// One re-read of the item's subtitle streams; `None` = the read failed (counts as a miss).
    Streams(Option<Vec<StreamRow>>),
}

/// WORKER THREAD.
fn run_job(client: &'static Client, rk: &str, job: Job) -> Landing {
    match job {
        Job::Search { lang } => {
            let query = SubtitleSearch { language: lang, hearing_impaired: false, forced: false };
            Landing::Search(match client.search_subtitles(rk, &query) {
                SubtitleSearchOutcome::Ok(container) => Ok(hits_of(&container.stream)),
                SubtitleSearchOutcome::Denied => Err(SearchFailure::Denied),
                SubtitleSearchOutcome::Missing => Err(SearchFailure::Missing),
                SubtitleSearchOutcome::BadLanguage => Err(SearchFailure::BadLanguage),
                SubtitleSearchOutcome::Transport => Err(SearchFailure::Transport),
            })
        }
        Job::Download { key } => {
            // the BEFORE read: without it a new stream could never be told from an old one
            let Some(before) = subtitle_rows(client, rk) else { return Landing::Download(Err(false)) };
            let before = before.iter().map(|r| r.id).collect();
            Landing::Download(match client.add_subtitle(rk, &key) {
                SubtitleAddOutcome::Ok => Ok(before),
                SubtitleAddOutcome::Denied => Err(true),
                SubtitleAddOutcome::Missing | SubtitleAddOutcome::BadKey
                | SubtitleAddOutcome::Transport => Err(false),
            })
        }
        Job::Refresh => Landing::Streams(subtitle_rows(client, rk)),
    }
}

/// The item's subtitle streams across every Media/Part, or `None` when the read failed.
fn subtitle_rows(client: &Client, rk: &str) -> Option<Vec<StreamRow>> {
    let item = client.metadata(rk)?;
    Some(item.media.iter().flat_map(|m| &m.part).flat_map(|p| &p.stream)
        .filter(|s| s.stream_type == 3)
        .map(|s| StreamRow { id: s.id, key: s.key.clone(), codec: s.codec.clone(),
            language_code: s.language_code.clone(), title: s.title.clone() })
        .collect())
}

/// Project the agent's rows into hits, best score first. The sort is STABLE, so the server's own
/// order survives among equal scores. A row without a key cannot be downloaded and is dropped.
pub fn hits_of(rows: &[plx_plex::plex::Stream]) -> Vec<SubHit> {
    let mut hits: Vec<SubHit> = rows.iter()
        .filter(|s| s.stream_type == 3 && !s.key.is_empty())
        .map(|s| SubHit {
            key: s.key.clone(),
            title: s.title.clone(),
            provider: s.provider_title.clone(),
            language: s.language.clone(),
            language_code: s.language_code.clone(),
            codec: s.codec.clone(),
            score: s.score,
            hearing_impaired: s.hearing_impaired != 0,
            forced: s.forced != 0,
        })
        .collect();
    hits.sort_by(|a, b| b.score.cmp(&a.score));
    hits
}

struct Mail { gen: u32, what: Landing }

/// The worker-visible half: one single-flight mailbox for every kind of job.
#[derive(Default)]
pub struct SubSearchAdapter { fetch: crate::stores::Fetch<Mail> }

impl SubSearchAdapter {
    /// WORKER THREAD: post unless newer mail is already waiting.
    fn land(&self, generation: u32, what: Landing) {
        self.fetch.post(Mail { gen: generation, what }, |old| old.gen < generation);
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn land_for_test(&self, generation: u32, what: Landing) { self.land(generation, what); }
    #[cfg(any(test, feature = "test-support"))]
    pub fn busy_for_test(&self) -> bool { self.fetch.busy() }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// **Regression (ultrareview, 2026-10-05):** a subtitle preference with no two-letter code
    /// (Filipino, `fil`) was taken as the Search page's default unchecked, so the page opened on
    /// "can't be searched in this language" although the viewer's other languages would have
    /// searched fine. Every candidate is held to the same fold; the first that folds wins.
    #[test]
    fn the_default_search_language_skips_a_preference_that_cannot_be_searched() {
        assert!(fold_language("fil").is_none(), "the premise: Filipino has no 2-letter code");
        assert_eq!(default_language(Some("fil"), ["nld".to_string()]), "nld");
        assert_eq!(default_language(Some("fil"), ["fil".to_string()]), "en", "nothing folds: English");
        assert_eq!(default_language(Some("es"), ["nld".to_string()]), "es", "a searchable preference wins");
        assert_eq!(default_language(Some(""), Vec::<String>::new()), "en");
        assert_eq!(default_language(None, ["eng".to_string()]), "eng");
    }

    fn hit(key: &str, title: &str, score: i64) -> SubHit {
        SubHit { key: key.into(), title: title.into(), provider: "OpenSubtitles".into(),
            language: "Nederlands".into(), language_code: "nld".into(), codec: "srt".into(),
            score, hearing_impaired: false, forced: false }
    }

    fn row(id: i64, title: &str) -> StreamRow {
        StreamRow { id, key: format!("/library/streams/{id}"), codec: "srt".into(),
            language_code: "nld".into(), title: title.into() }
    }

    /// A model opened on one item, with no worker ever spawned (`apply` is driven directly).
    fn opened(lang: &str) -> (SubSearchState, Arc<SubSearchAdapter>) {
        let adapter = Arc::new(SubSearchAdapter::default());
        let mut state = SubSearchState::default();
        assert!(state.run(&adapter, SubSearchCmd::Open {
            sid: ServerId::from_raw(1), rk: "506548".into(), lang: lang.into() }));
        (state, adapter)
    }

    fn ready(hits: Vec<SubHit>) -> (SubSearchState, Arc<SubSearchAdapter>) {
        let (mut state, adapter) = opened("nl");
        state.apply_for_test(Landing::Search(Ok(hits)));
        (state, adapter)
    }

    /// The server 500s on a 3-letter code, and a subtitle stream reports itself as one — so the
    /// fold is what stands between the stream's own language and a crash on someone's server.
    #[test]
    fn every_spelling_of_a_language_folds_to_the_two_letter_code_the_server_accepts() {
        for (input, want) in [("nld", "nl"), ("dut", "nl"), ("nl", "nl"), ("eng", "en"),
            ("en-GB", "en"), ("NL", "nl")] {
            assert_eq!(fold_language(input).as_deref(), Some(want), "{input:?}");
        }
        assert_eq!(fold_language(""), None);
        // the trap this test was written after: the grouping key is the LEXICOGRAPHIC minimum,
        // so Dutch keys to "dut" — a fold built on `lang_key` would send a code that 500s
        assert_eq!(crate::metadata::lang_key("nl").as_deref(), Some("dut"));
    }

    /// A language with no 2-letter spelling is refused BEFORE a search is owed: nothing is sent.
    #[test]
    fn an_unfoldable_language_fails_locally_and_owes_no_request() {
        let (state, _) = opened("tlh");
        assert_eq!(state.view().status(), SearchStatus::Failed);
        assert_eq!(state.view().failure(), Some(SearchFailure::BadLanguage));
        assert_eq!(state.job_for_test(), None, "a refused language must not reach the wire");
    }

    #[test]
    fn opening_owes_a_search_in_the_folded_language() {
        let (state, _) = opened("nld");
        assert_eq!(state.view().status(), SearchStatus::Searching);
        assert_eq!(state.job_for_test(), Some(Job::Search { lang: "nl".into() }));
    }

    /// The invariant `search.rs` exists for: no hits is an ANSWER, not a fault.
    #[test]
    fn an_empty_answer_is_ready_and_not_failed() {
        let (state, _) = ready(vec![]);
        assert_eq!(state.view().status(), SearchStatus::Ready);
        assert_eq!(state.view().failure(), None);
        assert!(state.view().hits().is_empty());
    }

    /// A 403 is drawn without a retry, and an explicit Retry must not resurrect it either.
    #[test]
    fn a_denied_search_is_not_retryable() {
        let (mut state, adapter) = opened("nl");
        state.apply_for_test(Landing::Search(Err(SearchFailure::Denied)));
        assert_eq!(state.view().failure(), Some(SearchFailure::Denied));
        assert!(!state.run(&adapter, SubSearchCmd::Retry), "retrying a 403 gets a 403");
        assert_eq!(state.job_for_test(), None);
    }

    #[test]
    fn a_transport_failure_retries_only_when_asked() {
        let (mut state, adapter) = opened("nl");
        state.apply_for_test(Landing::Search(Err(SearchFailure::Transport)));
        assert_eq!(state.job_for_test(), None, "no automatic retry on a timer");
        assert!(state.run(&adapter, SubSearchCmd::Retry));
        assert_eq!(state.job_for_test(), Some(Job::Search { lang: "nl".into() }));
    }

    /// Re-opening the menu on the same item and language keeps the answer already paid for.
    #[test]
    fn reopening_the_same_item_keeps_the_answer() {
        let (mut state, adapter) = ready(vec![hit("/library/streams/1", "a", 5)]);
        let gen = state.generation();
        assert!(!state.run(&adapter, SubSearchCmd::Open {
            sid: ServerId::from_raw(1), rk: "506548".into(), lang: "nld".into() }));
        assert_eq!(state.generation(), gen);
        assert_eq!(state.view().hits().len(), 1);
    }

    /// A slow answer for the old language must never repopulate the new one's results.
    ///
    /// Serial: the pump ends by spawning the search the NEW language owes whenever
    /// `plex::client_for` answers for this server, and that registry is process-global — a
    /// parallel test with a client installed for server 1 made the claim busy again here
    /// (intermittent, 2026-10-04/05). Under the lock no other test's client is installed.
    #[test]
    fn a_landing_from_a_superseded_generation_is_discarded() {
        let _serial = plx_base::testlock::serial();
        let (mut state, adapter) = opened("nl");
        let old = state.generation();
        assert!(state.run(&adapter, SubSearchCmd::SetLanguage("en".into())));
        adapter.land_for_test(old, Landing::Search(Ok(vec![hit("/library/streams/9", "old", 1)])));
        state.pump_with_gate(&adapter, &plx_machine::landgate::Gate::default());
        assert_eq!(state.view().status(), SearchStatus::Searching, "the stale answer is dropped");
        assert!(state.view().hits().is_empty());
        assert!(!adapter.busy_for_test(), "the take still released the claim");
    }

    /// Candidate keys die with their search: a press quoting a replaced generation is refused.
    #[test]
    fn a_download_against_a_replaced_result_set_is_refused() {
        let (mut state, adapter) = ready(vec![hit("/library/streams/1", "a", 5)]);
        let stale = state.generation();
        state.run(&adapter, SubSearchCmd::SetLanguage("en".into()));
        state.apply_for_test(Landing::Search(Ok(vec![hit("/library/streams/2", "b", 5)])));
        assert!(!state.run(&adapter, SubSearchCmd::Download { gen: stale, hit: 0 }));
        assert!(state.run(&adapter, SubSearchCmd::Download { gen: state.generation(), hit: 0 }));
        assert_eq!(state.job_for_test(), Some(Job::Download { key: "/library/streams/2".into() }));
    }

    /// One download at a time, and no language change while one installs.
    #[test]
    fn a_busy_download_refuses_a_second_and_a_language_change() {
        let (mut state, adapter) = ready(vec![hit("/library/streams/1", "a", 5),
            hit("/library/streams/2", "b", 4)]);
        let gen = state.generation();
        assert!(state.run(&adapter, SubSearchCmd::Download { gen, hit: 0 }));
        assert!(!state.run(&adapter, SubSearchCmd::Download { gen, hit: 1 }));
        assert!(!state.run(&adapter, SubSearchCmd::SetLanguage("en".into())));
    }

    /// The install is asynchronous: an accepted download WAITS a beat before reading, then keeps
    /// reading until the stream shows, then names it.
    #[test]
    fn an_accepted_download_polls_until_the_new_stream_appears() {
        let (mut state, adapter) = ready(vec![hit("/library/streams/1929514", "rel.NOGRP", 2301)]);
        state.run(&adapter, SubSearchCmd::Download { gen: state.generation(), hit: 0 });
        state.apply_for_test(Landing::Download(Ok(vec![1884886, 1884899])));
        assert_eq!(state.retry_for_test(), POLL_FRAMES, "no read straight after the PUT");
        assert_eq!(state.job_for_test(), Some(Job::Refresh));

        // first read: not there yet (what the probe actually saw)
        state.apply_for_test(Landing::Streams(Some(vec![row(1884886, ""), row(1884899, "")])));
        assert!(matches!(state.view().download(), Some(DownloadPhase::Waiting { attempts: 1, .. })));

        // second read: it landed
        state.apply_for_test(Landing::Streams(Some(vec![row(1884886, ""), row(1884899, ""),
            row(1929519, "rel.NOGRP")])));
        assert_eq!(state.view().download(), Some(&DownloadPhase::Installed { hit: 0,
            stream: InstalledStream { id: 1929519, key: "/library/streams/1929519".into(),
                codec: "srt".into(), language_code: "nld".into() } }));
    }

    #[test]
    fn an_install_that_never_shows_is_unconfirmed_not_failed() {
        let (mut state, adapter) = ready(vec![hit("/library/streams/1", "a", 5)]);
        state.run(&adapter, SubSearchCmd::Download { gen: state.generation(), hit: 0 });
        state.apply_for_test(Landing::Download(Ok(vec![10])));
        for _ in 0..POLL_ATTEMPTS {
            state.apply_for_test(Landing::Streams(Some(vec![row(10, "")])));
        }
        assert_eq!(state.view().download(), Some(&DownloadPhase::Unconfirmed { hit: 0 }));
        assert_eq!(state.job_for_test(), None, "the poll stops");
    }

    /// A refused download marks only that download; the search results stay.
    #[test]
    fn a_denied_download_keeps_the_results() {
        let (mut state, adapter) = ready(vec![hit("/library/streams/1", "a", 5)]);
        state.run(&adapter, SubSearchCmd::Download { gen: state.generation(), hit: 0 });
        state.apply_for_test(Landing::Download(Err(true)));
        assert_eq!(state.view().download(), Some(&DownloadPhase::Failed { hit: 0, denied: true }));
        assert_eq!(state.view().status(), SearchStatus::Ready);
        assert_eq!(state.view().hits().len(), 1);
    }

    /// The identification rule, on its own. The PUT names nothing, so this is the whole answer.
    #[test]
    fn the_installed_stream_is_the_new_one_preferring_the_picked_release() {
        let before = [1, 2];
        assert_eq!(identify(&[row(1, ""), row(2, "")], &before, "x"), None, "nothing new");
        assert_eq!(identify(&[row(1, ""), row(7, "other")], &before, "x").unwrap().id, 7,
            "a lone new stream is ours even without a title match");
        assert_eq!(identify(&[row(5, "mine"), row(9, "other")], &before, "mine").unwrap().id, 5,
            "the picked release name wins over a newer id");
        assert_eq!(identify(&[row(5, "a"), row(9, "b")], &before, "x").unwrap().id, 9,
            "otherwise the newest");
    }

    /// Best match first, but the server's order survives among ties.
    #[test]
    fn hits_rank_by_score_and_drop_rows_without_a_key() {
        let rows: Vec<plx_plex::plex::Stream> = serde_json::from_str::<plx_plex::plex::Envelope>(
            r#"{"MediaContainer":{"Stream":[
                {"id":1,"key":"/library/streams/1","streamType":3,"score":"479","title":"a"},
                {"id":2,"key":"/library/streams/2","streamType":3,"score":"2301","title":"b"},
                {"id":3,"key":"","streamType":3,"score":"9999","title":"no key"},
                {"id":4,"key":"/library/streams/4","streamType":3,"score":"479","title":"c"}]}}"#)
            .unwrap().media_container.stream;
        let titles: Vec<String> = hits_of(&rows).into_iter().map(|h| h.title).collect();
        assert_eq!(titles, ["b", "a", "c"]);
    }

    #[test]
    fn close_drops_the_model_and_discards_what_was_in_flight() {
        let (mut state, adapter) = opened("nl");
        let gen = state.generation();
        assert!(state.run(&adapter, SubSearchCmd::Close));
        assert_eq!(state.view().status(), SearchStatus::Idle);
        adapter.land_for_test(gen, Landing::Search(Ok(vec![])));
        assert!(!state.pump_with_gate(&adapter, &plx_machine::landgate::Gate::default()));
    }
}
