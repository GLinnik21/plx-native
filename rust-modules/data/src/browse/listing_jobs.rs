//! The Library's listing reads as store jobs: the grid's pages and the filter menus' value lists.
//!
//! A Browse step that wants a listing page, or a section's genre or first-letter list, raises that
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

use super::{
    genre_entry, fetch_listing_page, DirectoryResult, GenreEntry, LibraryType, ListingPage, PageResult,
    Restore, SectionQuery, PAGE,
};
use plx_machine::job::Port;
use plx_machine::machine::{Addr, MachineId, RequestId};
use plx_plex::plex::{Client, ClientKey, LibrarySection, MediaContainer, ServerId};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::Arc;

// ---- the directory read -----------------------------------------------------------------------

/// Which of a section's two value lists a [`BrowseDirectoryReq`] reads. Each has its own claim and
/// mailbox on the adapter, and the port a job leaves with knows which.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Directory {
    /// `/library/sections/{key}/genre`: the Filter menu's genre list.
    Genres,
    /// `/library/sections/{key}/firstCharacter`: the A-Z rail's letters and their counts.
    Letters,
}

impl Directory {
    /// The directory's name on the wire.
    fn wire(self) -> &'static str {
        match self {
            Self::Genres => "genre",
            Self::Letters => "firstCharacter",
        }
    }

    pub(super) fn busy(self, adapter: &super::BrowseAdapter) -> bool {
        match self {
            Self::Genres => adapter.genres.busy(),
            Self::Letters => adapter.letters.busy(),
        }
    }

    pub(super) fn claim(self, adapter: &super::BrowseAdapter, id: u32) {
        match self {
            Self::Genres => adapter.genres.claim(id),
            Self::Letters => adapter.letters.claim(id),
        }
    }

    fn refuse_for(self, adapter: &super::BrowseAdapter, id: u32) {
        match self {
            Self::Genres => adapter.genres.refuse_for(id),
            Self::Letters => adapter.letters.refuse_for(id),
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
    /// The `type=` the section's current listing is asked for, if any.
    metadata_type: Option<i64>,
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
    /// The request's answer (a panicked read answers an empty list). One read of each directory is
    /// out at a time (its claim), so the answer replaces whatever is waiting.
    fn done(&self, _addr: Addr, out: BrowseDirectoryAnswer) {
        match out.0 {
            DirectoryAnswer::Genres(result) => self.adapter.genres.post(result, |_| true),
            DirectoryAnswer::Letters(result) => self.adapter.letters.post(result, |_| true),
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
    pub(super) fn new(sid: ServerId, key: ClientKey, epoch: u32, sec: usize, section: i64,
        library_type: LibraryType, metadata_type: Option<i64>, which: Directory) -> Self {
        Self { sid, key, epoch, sec, section, library_type, metadata_type, which }
    }
}

/// The request's IO body and its completion: read the directory through `client` and post the
/// answer to `port`. The answer is posted OUTSIDE the guard, so a read that panics lands as an empty
/// list, as a read that found nothing does.
pub fn finish_browse_directory_job(port: &BrowseDirectoryPort, addr: Addr, client: &'static Client,
    req: BrowseDirectoryReq) {
    let (section, metadata_type, which) = (req.section, req.metadata_type, req.which);
    finish_directory_with(port, addr, client, req, move || {
        client.section_directory(section, which.wire(), metadata_type)
    });
}

/// [`finish_browse_directory_job`] over `read`, the one call that reaches the server: the seam a
/// test swaps for a read that panics.
fn finish_directory_with(port: &BrowseDirectoryPort, addr: Addr, client: &'static Client,
    req: BrowseDirectoryReq, read: impl FnOnce() -> Option<MediaContainer>) {
    let BrowseDirectoryReq { key, epoch, sec, library_type, which, .. } = req;
    let answer = match which {
        Directory::Genres => DirectoryAnswer::Genres(DirectoryResult {
            epoch, sec, client, token_gen: key.token, library_type, list: directory_list(read, genre_entry),
        }),
        Directory::Letters => DirectoryAnswer::Letters(DirectoryResult {
            epoch, sec, client, token_gen: key.token, library_type, list: directory_list(read, letter_entry),
        }),
    };
    port.done(addr, BrowseDirectoryAnswer(answer));
}

/// The rows `read` brings back, projected; empty when the read failed or panicked.
fn directory_list<T>(read: impl FnOnce() -> Option<MediaContainer>,
    project: fn(&LibrarySection) -> Option<T>) -> Vec<T> {
    catch_unwind(AssertUnwindSafe(|| {
        let mut values = Vec::new();
        if let Some(container) = read() {
            values.extend(container.directory.iter().filter_map(project));
        }
        values
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
