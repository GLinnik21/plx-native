//! The Library's listing reads as store jobs: the grid's pages and the filter menus' value lists.
//!
//! A Browse step that wants a listing page, or a section's genre list, first-letter list or a value
//! sort's band counts, raises that
//! read's claim, takes the section's [`ClientKey`] from the borrowed [`ClientKeys`] and **leaves the
//! read as a job on the store's outbox** ([`StoreJob::BrowsePage`], [`StoreJob::BrowseDirectory`]).
//! Nothing here starts a worker or reads the server registry: the application's runner starts the
//! job, only if the registry still holds the client the step decided on ([`BrowsePageReq::server`]),
//! and hands the body that client. The body reads the server through it and posts ONE answer to the
//! job's port ([`finish_browse_page_job`], [`finish_browse_directory_job`]); a read that could not
//! start is refused instead ([`Port::refused`]), which lowers its claim in the call that refused it,
//! and the store's next step settles it ([`super::BrowseState`], `settle_refused_reads`).
//!
//! The landings stay where they were (`pump_owned_with_gate`, `land_directory_owned_with_gate`) and
//! keep their identity commit (`commit_if_current` / `commit_reachability_if_current`): an answer
//! carries the client the body ran through and the key's credential generation, so a client that
//! was re-pointed or retokened while the read was out is not the server the answer is blamed on.
//!
//! Both kinds are [`crate::stores::Admission::Direct`]: they are never on a tape, and a controlled
//! Home run never emits them (`bootstrap/effects.rs` accepts only `Browse(Reset)` and
//! `Browse(Discovery(..))` and refuses `StoreWork::Browse`).

use super::bands::{self, BandFamily, BandRail, Day, Zone};
use super::{
    genre_entry, fetch_listing_page, DirectoryResult, GenreEntry, LettersKey, LibraryType, ListingPage,
    PageResult, Restore, SecKind, SectionQuery, PAGE,
};
use plx_machine::job::Port;
use plx_machine::machine::{Addr, MachineId, RequestId};
use plx_plex::plex::{Client, ClientKey, LibrarySection, MediaContainer, Oldest, SectionTotal, ServerId};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::Arc;

// ---- the directory read -----------------------------------------------------------------------

/// Which of a section's value lists a [`BrowseDirectoryReq`] reads. Each has its own claim and
/// mailbox on the adapter, and the port a job leaves with knows which.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Directory {
    /// `/library/sections/{key}/genre`: the Filter menu's genre list.
    Genres,
    /// `/library/sections/{key}/firstCharacter`: the A-Z rail's letters and their counts.
    Letters,
    /// `/library/sections/{key}/all`, count-only, once per band edge of a value sort: the A-Z rail's
    /// bands and where they start ([`finish_bands_with`]).
    Bands,
}

impl Directory {
    /// The directory's name on the wire.
    fn wire(self) -> &'static str {
        match self {
            Self::Genres => "genre",
            Self::Letters => "firstCharacter",
            Self::Bands => "all",
        }
    }

    pub(super) fn busy(self, adapter: &super::BrowseAdapter) -> bool {
        match self {
            Self::Genres => adapter.genres.busy(),
            Self::Letters => adapter.letters.busy(),
            Self::Bands => adapter.bands.busy(),
        }
    }

    pub(super) fn claim(self, adapter: &super::BrowseAdapter, id: u32) {
        match self {
            Self::Genres => adapter.genres.claim(id),
            Self::Letters => adapter.letters.claim(id),
            Self::Bands => adapter.bands.claim(id),
        }
    }

    fn refuse_for(self, adapter: &super::BrowseAdapter, id: u32) {
        match self {
            Self::Genres => adapter.genres.refuse_for(id),
            Self::Letters => adapter.letters.refuse_for(id),
            Self::Bands => adapter.bands.refuse_for(id),
        }
    }
}

/// A first-letter row as the A-Z rail's entry: its letter and the count behind it; `None` for a row
/// with no key or no members.
fn letter_entry(directory: &LibrarySection) -> Option<(String, i64)> {
    (!directory.key.is_empty() && directory.size > 0).then(|| (directory.title.clone(), directory.size))
}

/// The one request a [`StoreJob::BrowseDirectory`](crate::stores::StoreJob::BrowseDirectory)
/// carries: everything the worker needs, captured by the step. `key` is the client the step decided
/// on; the runner starts the request only if the registry still holds that client at the launch.
#[derive(Debug, PartialEq)]
pub struct BrowseDirectoryReq {
    sid: ServerId,
    key: ClientKey,
    /// The section table's epoch when the step asked: a landing across a `Reset` is dropped.
    epoch: u32,
    /// The section's index in the table when the step asked.
    sec: usize,
    /// The section's key on its server.
    section: i64,
    library_type: LibraryType,
    /// The wire terms the read is scoped by: the listing's `type=` for the genre list, and for the
    /// letters and the bands the very terms the grid's page read sends (`SecState::query_filters`).
    filters: Vec<(String, String)>,
    /// What a letters or bands read counts, to come back with its answer ([`LettersKey`], which
    /// names the band family); `None` for genres.
    letters: Option<LettersKey>,
    which: Directory,
}

impl BrowseDirectoryReq {
    /// The server to ask and the client the step decided to ask it through.
    pub fn server(&self) -> (ServerId, ClientKey) {
        (self.sid, self.key)
    }
}

/// A directory request's answer, opaque to the runner: the result the step lands.
pub struct BrowseDirectoryAnswer(DirectoryAnswer);

enum DirectoryAnswer {
    Genres(DirectoryResult<GenreEntry>),
    Letters(DirectoryResult<(String, i64)>),
    /// A bands read brings back ONE value in `list`: the rail, or nothing for a listing with no rail.
    Bands(DirectoryResult<BandRail>),
}

/// Where a directory request's ONE answer goes: the store's mailbox, captured with the job when it
/// was emitted. A store that has since been `Reset` holds a new adapter, and this port still reaches
/// the one the request was made from, the retired one nobody pumps. Two ports are equal when they
/// reach the same adapter's same directory.
#[derive(Clone)]
pub struct BrowseDirectoryPort {
    adapter: Arc<super::BrowseAdapter>,
    which: Directory,
}

impl BrowseDirectoryPort {
    pub(super) fn of(adapter: &Arc<super::BrowseAdapter>, which: Directory) -> Self {
        Self { adapter: Arc::clone(adapter), which }
    }
}

impl PartialEq for BrowseDirectoryPort {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.adapter, &other.adapter) && self.which == other.which
    }
}

impl std::fmt::Debug for BrowseDirectoryPort {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "BrowseDirectoryPort({:p}, {:?})", Arc::as_ptr(&self.adapter), self.which)
    }
}

impl Port<BrowseDirectoryAnswer> for BrowseDirectoryPort {
    /// The request's answer (a failed or panicked read answers `ok: false`). One read of each
    /// directory is out at a time (its claim), so the answer replaces whatever is waiting.
    fn done(&self, _addr: Addr, out: BrowseDirectoryAnswer) {
        match out.0 {
            DirectoryAnswer::Genres(result) => self.adapter.genres.post(result, |_| true),
            DirectoryAnswer::Letters(result) => self.adapter.letters.post(result, |_| true),
            DirectoryAnswer::Bands(result) => self.adapter.bands.post(result, |_| true),
        }
    }

    /// The request was never started, so nothing will land and nothing will release its claim:
    /// hand the claim back and remember the refusal ([`crate::stores::Fetch::refuse_for`]); the
    /// store settles it at the head of its next step. A refusal that comes after a newer request has
    /// claimed leaves that claim alone.
    fn refused(&self, addr: Addr) {
        self.which.refuse_for(&self.adapter, addr.req.0);
    }
}

/// A listing request's address: the store and the id its step gave it. Each read (a directory, the
/// page) counts its own requests behind its own claim, so a genre and a letters request may share a
/// number, and so may the hubs and page counters, which address the same store: an `Addr` is not an
/// identity across kinds. Every answer and refusal routes through its own job's port, which says
/// which read it is.
pub(super) fn listing_addr(id: u32) -> Addr {
    Addr { to: MachineId::Store(crate::stores::StoreId::Browse.ord()), req: RequestId(id) }
}

impl BrowseDirectoryReq {
    #[allow(clippy::too_many_arguments)]
    pub(super) fn new(sid: ServerId, key: ClientKey, epoch: u32, sec: usize, section: i64,
        library_type: LibraryType, filters: Vec<(String, String)>, letters: Option<LettersKey>,
        which: Directory) -> Self {
        Self { sid, key, epoch, sec, section, library_type, filters, letters, which }
    }
}

/// The request's IO body and its completion: read the directory through `client` and post the
/// answer to `port`. The answer is posted OUTSIDE the guard, so a read that panics lands like one that
/// failed (`ok: false`), never as the empty list of a read that found nothing. `zone` is the viewer's
/// time zone, which only the application's runner can read (`plx_platform::i18n`): a bands read of a
/// calendar family converts its edges with it, and nothing else uses it.
pub fn finish_browse_directory_job(port: &BrowseDirectoryPort, addr: Addr, client: &'static Client,
    req: BrowseDirectoryReq, zone: Zone) {
    let (section, which, filters) = (req.section, req.which, req.filters.clone());
    if which == Directory::Bands {
        finish_bands_with(port, addr, client, req, zone, move |terms| client.section_total(section, terms),
            move |terms, field| client.section_oldest(section, terms, field));
        return;
    }
    finish_directory_with(port, addr, client, req, move || {
        client.section_directory(section, which.wire(), &filters)
    });
}

/// [`finish_browse_directory_job`] over `read`, the one call that reaches the server: the seam a
/// test swaps for a read that panics.
fn finish_directory_with(port: &BrowseDirectoryPort, addr: Addr, client: &'static Client,
    req: BrowseDirectoryReq, read: impl FnOnce() -> Option<MediaContainer>) {
    let BrowseDirectoryReq { key, epoch, sec, library_type, letters, which, .. } = req;
    let answer = match which {
        Directory::Genres => {
            let list = directory_list(read, genre_entry);
            DirectoryAnswer::Genres(DirectoryResult {
                epoch, sec, client, token_gen: key.token, library_type, ok: list.is_some(),
                list: list.unwrap_or_default(), letters: None,
            })
        }
        Directory::Letters => {
            let list = directory_list(read, letter_entry);
            DirectoryAnswer::Letters(DirectoryResult {
                epoch, sec, client, token_gen: key.token, library_type, ok: list.is_some(),
                list: list.unwrap_or_default(), letters,
            })
        }
        Directory::Bands => {
            // `finish_browse_directory_job` sends a bands read to `finish_bands_with`: reaching this
            // arm is a caller's mistake, and it answers a read that failed rather than strand the claim.
            debug_assert!(false, "a bands read reached the directory body");
            DirectoryAnswer::Bands(DirectoryResult {
                epoch, sec, client, token_gen: key.token, library_type, ok: false, list: Vec::new(), letters,
            })
        }
    };
    port.done(addr, BrowseDirectoryAnswer(answer));
}

/// What a bands read found: the counts of every band (a rail, or none for a listing the rail cannot
/// band), or no counts at all because a read did not answer.
enum BandsRead {
    /// `Some(rail)`, or `None`: nothing to band, or counts that were not trusted. Neither changes
    /// when the same question is asked again, so it lands as an answer (`SecState::land_bands`).
    Counted(Option<BandRail>),
    /// A read failed (the server did not answer, or answered something that is no count): the
    /// back-off asks again.
    Failed,
}

/// [`finish_browse_directory_job`] for a bands read, over `probe`, the one call that reaches the
/// server: it takes the wire terms of one count-only read (the listing's filters, and for a band's
/// edge the range term after them) and answers the number of items they match, or that the server
/// ignored the count-only window ([`SectionTotal`]). The seam a test swaps
/// for a model of the server, or for a read that panics; the real one is `Client::section_total`.
///
/// A calendar family also reads the oldest value of its field first, through `oldest` (the terms of
/// the listing it is read over, and the date field to sort by), and converts its edges with `zone`;
/// `today` is the day the key was asked on (`LettersKey::today`).
///
/// The reads are sequential and ALL OR NOTHING: a rail built from some of its counts would point at
/// the wrong rows, so one read that fails fails the answer (`ok: false`: the same back-off as the
/// letters). They are NOT asked beside one another (`task::beside`): a calendar is a dozen months and a
/// year apiece, each a count-only read the server answers without rows (3 ms on a loopback server), the
/// re-read of the total at the end only means something if the reads ran in order, and nothing is
/// shown until all are in. A fan-out is a change of this function alone, if a remote server ever
/// measures too slow. The answer is posted OUTSIDE the guard, so a body that panics lands like a read
/// that failed.
fn finish_bands_with(port: &BrowseDirectoryPort, addr: Addr, client: &'static Client,
    req: BrowseDirectoryReq, zone: Zone, probe: impl Fn(&[(String, String)]) -> Option<SectionTotal>,
    oldest: impl Fn(&[(String, String)], &str) -> Option<Oldest>) {
    let BrowseDirectoryReq { key, epoch, sec, library_type, filters, letters, .. } = req;
    let family = letters.as_ref().and_then(|asked| asked.family);
    let today = letters.as_ref().and_then(|asked| asked.today);
    let read = catch_unwind(AssertUnwindSafe(|| read_bands(family, today, zone, &filters, probe, oldest)))
        .unwrap_or(BandsRead::Failed);
    let (ok, list) = match read {
        BandsRead::Counted(rail) => (true, rail.into_iter().collect()),
        BandsRead::Failed => (false, Vec::new()),
    };
    port.done(addr, BrowseDirectoryAnswer(DirectoryAnswer::Bands(DirectoryResult {
        epoch, sec, client, token_gen: key.token, library_type, ok, list, letters,
    })));
}

/// Why a bands read stopped before it had a rail.
enum Stop {
    /// A read did not answer, answered something that is no count, or the listing moved under the
    /// counts: the back-off asks again.
    Failed,
    /// The server answered rows to a count-only read: it ignores the window, and answers so every
    /// time. A verdict, not retried until the key moves.
    Ignored,
}

/// The counts behind a rail: the size of the listing (the grid's terms alone), then, for a calendar
/// family, the oldest year ([`ladder_of`]), then, for each band of the family's ladder, how many items
/// sit at or above its edge, then the size again. The reads see the SAME filter terms the page request
/// sends, in the same order, and the size is read in the same pass as the counts, never taken from the
/// page, which another request may have moved.
///
/// The reads are sequential, so a library scan can land between them and move the listing under the
/// counts; to the offsets that looks like a count above the total or one that falls as the edge falls.
/// The size is therefore read once more after the last band, and an answer that differs from the first
/// is a [`Stop::Failed`] (asked again after the back-off), never a verdict about the counts.
///
/// A listing with nothing in it has nothing to band and is not read further. Counts that
/// [`bands::offsets`] does not trust while the size held still (every band as large as the listing:
/// the server ignored the field; or counts that fall as the edge falls) give no rail and say why in
/// the log, and so does a server that answers rows to a count-only read, a calendar family with no day,
/// no value in its field, an extent after this year ([`bands::extent_year`]; one before 1900 only
/// starts the bands at 1900) or an edge the zone cannot convert.
fn read_bands(family: Option<BandFamily>, today: Option<Day>, zone: Zone, filters: &[(String, String)],
    probe: impl Fn(&[(String, String)]) -> Option<SectionTotal>,
    oldest: impl Fn(&[(String, String)], &str) -> Option<Oldest>) -> BandsRead {
    let Some(family) = family else {
        debug_assert!(false, "a bands read for no family");
        plx_base::eventlog::log("rail: a bands read for no family — no rail");
        return BandsRead::Counted(None);
    };
    match count_bands(family, today, zone, filters, probe, oldest) {
        Ok(rail) => BandsRead::Counted(rail),
        Err(Stop::Failed) => BandsRead::Failed,
        Err(Stop::Ignored) => {
            plx_base::eventlog::log(&format!("rail: {family:?} the server answered rows to a count-only read — no rail"));
            BandsRead::Counted(None)
        }
    }
}

/// The ladder `family` counts over, or `None` for no rail (said in the log): a fixed ladder as it is, a
/// calendar family's bands from `today` and the year of its oldest value. The oldest value is ONE read
/// of the first row of `sort=<field>:asc:nullsLast` over the page's own terms (a show family that
/// sorts by its newest episode reads the section's oldest EPISODE instead,
/// [`bands::ExtentRead::episodes`]); a read that did not answer fails the answer.
fn ladder_of(family: BandFamily, today: Option<Day>, zone: Zone, filters: &[(String, String)],
    oldest: impl Fn(&[(String, String)], &str) -> Option<Oldest>) -> Result<Option<Vec<bands::Band>>, Stop> {
    let Some(extent) = family.extent_read() else {
        let ladder = family.fixed_ladder();
        if ladder.is_none() {
            debug_assert!(false, "a bands read for {family:?}, which has no fixed ladder");
            plx_base::eventlog::log(&format!("rail: no fixed band ladder for {family:?} — no rail"));
        }
        return Ok(ladder);
    };
    let Some(today) = today else {
        plx_base::eventlog::log(&format!("rail: {family:?} has no date to count by — no rail"));
        return Ok(None);
    };
    let episodes = LibraryType::Episodes.type_terms(SecKind::Show);
    let terms = if extent.episodes { &episodes[..] } else { filters };
    let year = match oldest(terms, extent.field).ok_or(Stop::Failed)? {
        Oldest::Year(year) => Some(year),
        Oldest::Instant(epoch) => (zone.local_date)(epoch).map(|[year, ..]| year),
        Oldest::Nothing => None,
    };
    let Some(year) = year else {
        plx_base::eventlog::log(&format!("rail: {family:?} no item has a usable value in {} — no rail", extent.field));
        return Ok(None);
    };
    let Some(year) = bands::extent_year(year, today) else {
        plx_base::eventlog::log(&format!("rail: {family:?} the oldest value is in the year {year}, after today — no rail"));
        return Ok(None);
    };
    let ladder = bands::plan(family, today, year);
    if ladder.is_empty() {
        plx_base::eventlog::log(&format!("rail: {family:?} no bands for {today:?} — no rail"));
        return Ok(None);
    }
    Ok(Some(ladder))
}

fn count_bands(family: BandFamily, today: Option<Day>, zone: Zone, filters: &[(String, String)],
    probe: impl Fn(&[(String, String)]) -> Option<SectionTotal>,
    oldest: impl Fn(&[(String, String)], &str) -> Option<Oldest>) -> Result<Option<BandRail>, Stop> {
    let count = |terms: &[(String, String)]| match probe(terms) {
        Some(SectionTotal::Count(count)) => u32::try_from(count).map_err(|_| Stop::Failed),
        Some(SectionTotal::Ignored) => Err(Stop::Ignored),
        None => Err(Stop::Failed),
    };
    let total = count(filters)?;
    if total == 0 {
        return Ok(None);
    }
    let Some(ladder) = ladder_of(family, today, zone, filters, oldest)? else { return Ok(None) };
    // A fixed ladder has no calendar edge, so the converter is never asked of it; a local edge the
    // zone cannot convert leaves the whole rail without a probe (a rail of some bands would lie).
    let Some(edges) = bands::probes(&ladder, &zone.local_midnight) else {
        plx_base::eventlog::log(&format!("rail: {family:?} has an edge without a probe — no rail"));
        return Ok(None);
    };
    let mut at_or_above = Vec::with_capacity(edges.len());
    for edge in &edges {
        let (field, value) = edge.query_pair();
        let mut terms = filters.to_vec();
        terms.push((field, value.to_owned()));
        at_or_above.push(count(&terms)?);
    }
    let after = count(filters)?;
    if after != total {
        plx_base::eventlog::log(&format!("rail: {family:?} the listing moved while it was counted ({total} then {after})"));
        return Err(Stop::Failed);
    }
    match BandRail::new(&ladder, &at_or_above, total) {
        Ok(rail) => Ok(Some(rail)),
        Err(reject) => {
            plx_base::eventlog::log(&format!(
                "rail: {family:?} counts not trusted ({reject:?}; total {total}, counts {at_or_above:?}) — no rail"));
            Ok(None)
        }
    }
}

/// The rows `read` brings back, projected; `None` when the read failed or panicked. An answer with
/// no rows is `Some(vec![])`: a filter that matches nothing is a list, not a fault.
fn directory_list<T>(read: impl FnOnce() -> Option<MediaContainer>,
    project: fn(&LibrarySection) -> Option<T>) -> Option<Vec<T>> {
    catch_unwind(AssertUnwindSafe(|| {
        read().map(|container| container.directory.iter().filter_map(project).collect())
    }))
    .unwrap_or_default()
}

// ---- the page read ----------------------------------------------------------------------------

/// The one request a [`StoreJob::BrowsePage`](crate::stores::StoreJob::BrowsePage) carries:
/// everything the worker needs, captured by the step (a worker never reads the machine). `key` is
/// the client the step decided on; the runner starts the request only if the registry still holds
/// that client at the launch.
#[derive(Debug, PartialEq)]
pub struct BrowsePageReq {
    sid: ServerId,
    key: ClientKey,
    /// The query generation the page is for ([`super::BrowseState::query_gen`]): the landing is
    /// dropped when the listing has been re-asked since.
    gen: u32,
    sec: usize,
    /// The first item of the page.
    start: usize,
    /// The section's key on its server.
    section: i64,
    /// `sort=` value, empty for the server's default.
    sort: String,
    filters: Vec<(String, String)>,
    include_meta: bool,
    confirm_sort: bool,
    restore: Option<Restore>,
}

impl BrowsePageReq {
    /// The server to ask and the client the step decided to ask it through.
    pub fn server(&self) -> (ServerId, ClientKey) {
        (self.sid, self.key)
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn new(sid: ServerId, key: ClientKey, gen: u32, sec: usize, start: usize, section: i64,
        sort: String, filters: Vec<(String, String)>, include_meta: bool, confirm_sort: bool,
        restore: Option<Restore>) -> Self {
        Self { sid, key, gen, sec, start, section, sort, filters, include_meta, confirm_sort, restore }
    }
}

/// A page request's answer, opaque to the runner: the result the step lands.
pub struct BrowsePageAnswer(PageResult);

/// Where a page request's ONE answer goes: the store's mailbox, captured with the job when it was
/// emitted (see [`BrowseDirectoryPort`] for the retired adapter). Two ports are equal when they
/// reach the same adapter.
#[derive(Clone)]
pub struct BrowsePagePort(Arc<super::BrowseAdapter>);

impl BrowsePagePort {
    pub(super) fn of(adapter: &Arc<super::BrowseAdapter>) -> Self {
        Self(Arc::clone(adapter))
    }
}

impl PartialEq for BrowsePagePort {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

impl std::fmt::Debug for BrowsePagePort {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "BrowsePagePort({:p})", Arc::as_ptr(&self.0))
    }
}

impl Port<BrowsePageAnswer> for BrowsePagePort {
    /// The request's answer (a panicked read answers a failed page, `total < 0`). One page read is
    /// out at a time (its claim), so the answer replaces whatever is waiting.
    fn done(&self, _addr: Addr, out: BrowsePageAnswer) {
        self.0.page.post(out.0, |_| true);
    }

    /// The request was never started: hand the claim back and remember the refusal, as for a
    /// directory request ([`BrowseDirectoryPort::refused`]). The page is still the first one
    /// missing, so the next step asks for it again.
    fn refused(&self, addr: Addr) {
        self.0.page.refuse_for(addr.req.0);
    }
}

/// The request's IO body and its completion: read the page through `client` and post the answer to
/// `port`. The answer is posted OUTSIDE the guard, so a read that panics lands as a FAILED page
/// (`total < 0`: the listing keeps what it had and the pump backs off), never as an empty one.
pub fn finish_browse_page_job(port: &BrowsePagePort, addr: Addr, client: &'static Client, req: BrowsePageReq) {
    finish_page_with(port, addr, client, req, fetch_listing_page);
}

/// [`finish_browse_page_job`] over `read`, the one call that reaches the server: the seam a test
/// swaps for a read that panics.
fn finish_page_with(port: &BrowsePagePort, addr: Addr, client: &'static Client, req: BrowsePageReq,
    read: impl FnOnce(&Client, ServerId, &SectionQuery<'_>, bool, Option<&Restore>) -> ListingPage) {
    let BrowsePageReq { sid, key, gen, sec, start, section, sort, filters, include_meta, confirm_sort, restore } = req;
    let page = catch_unwind(AssertUnwindSafe(|| {
        let query = SectionQuery {
            section_key: section, sort: &sort, filters: &filters, start: start as i64, size: PAGE as i64,
            include_meta,
        };
        read(client, sid, &query, confirm_sort, restore.as_ref())
    }))
    .unwrap_or_else(|_| ListingPage::failed());
    port.done(addr, BrowsePageAnswer(PageResult {
        client, token_gen: key.token, gen, sec, start, items: page.items, total: page.total,
        sorts: page.sorts, restored: page.restored, genres: page.genres, genre: page.genre,
        resolved: page.resolved,
    }));
}

#[cfg(test)]
#[path = "listing_jobs_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "bands_job_tests.rs"]
mod bands_tests;

#[cfg(test)]
#[path = "bands_calendar_tests.rs"]
mod bands_calendar_tests;
