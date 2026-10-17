//! watchlist: the data layer of the plex.tv Watchlist page (docs/watchlist-plan.md §1.1-1.5, §1.10).
//!
//! * [`model`]: rows, the guid-to-server match table, [`model::TileState`].
//! * [`order`]: the title comparator (§1.10), sort and filter over stored rows.
//! * [`ledger`]: the intent ledger (optimistic flip, coalescing, revert).
//! * this module: the identity a worker is handed ([`WatchlistAuth`]), the I/O seam
//!   ([`WatchlistIo`], live in production, a script in tests) and the worker bodies that turn a
//!   page of the service's list into rows plus the guid matches against the user's servers.
//!   `stores::watchlist` owns the state those workers land into.
//!
//! Nothing here names a token in a log line or a `Debug` form: [`WatchlistAuth`] hides its
//! credential, and the plex layer's transport logs status and endpoint shape only.
pub mod ledger;
pub mod model;
pub mod order;

use ledger::{Intent, PendingWrite, WriteOutcome};
use model::{Asked, Kind, Reach, ServerMatch, WatchRow};
use plx_plex::plex::account::AccountClient;
use plx_plex::plex::session::UserRef;
use plx_plex::plex::watchlist::{WatchItem, WatchKind, WatchPage, WatchlistError, MAX_PAGE};
use plx_plex::plex::{GuidKind, GuidLookupError, GuidMatch, ServerId, GUIDS_PER_REQUEST};
use std::collections::HashSet;

/// Who a worker acts for, captured on the MAIN thread at spawn (`session::plex_tv_credential`) and
/// owned by the worker: it never reads the session itself. `generation` is the profile generation
/// at capture; a landing tagged with an older one is dropped.
#[derive(Clone)]
pub struct WatchlistAuth {
    pub client_id: String,
    pub credential: String,
    pub user: UserRef,
    pub generation: u32,
}

impl std::fmt::Debug for WatchlistAuth {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WatchlistAuth").field("generation", &self.generation).finish_non_exhaustive()
    }
}

impl WatchlistAuth {
    /// MAIN THREAD. The active profile's account-service credential, or `None` when there is none
    /// (signed out, or a managed profile with no plex.tv token): the page then has no list to read.
    pub fn capture() -> Option<WatchlistAuth> {
        let current = plx_plex::plex::session::current_snapshot();
        let user = current.user.clone()?;
        let credential = plx_plex::plex::session::plex_tv_credential(&user)?;
        let client_id = plx_plex::plex::session::peek().client_id.clone();
        if client_id.is_empty() {
            return None;
        }
        Some(WatchlistAuth { client_id, credential, user, generation: current.generation })
    }

    /// Is this still the seated profile? Compared by id and uuid and the profile generation.
    pub fn is_current(&self) -> bool {
        let now = plx_plex::plex::session::current_snapshot();
        now.generation == self.generation
            && now.user.as_ref().is_some_and(|u| u.id == self.user.id && u.uuid == self.user.uuid)
    }

    fn client(&self) -> AccountClient {
        AccountClient::new(&self.client_id, Some(&self.credential))
    }
}

/// Everything a worker (and the store's frame pass) asks of the outside world. Production is
/// [`LiveIo`]; tests script it. `Reach` carries the roster and the reachability facts.
pub trait WatchlistIo: Reach + Send + Sync {
    /// MAIN THREAD: capture the identity for a new epoch.
    fn capture_auth(&self) -> Option<WatchlistAuth>;
    /// MAIN THREAD: is `auth` still the seated profile?
    fn is_current(&self, auth: &WatchlistAuth) -> bool;
    /// One page of the list. `if_none_match` is the `ETag` of the last COMPLETE read, handed over
    /// for page 0 only; an answer of `Err(WatchlistError::NotModified)` is the service saying the
    /// list is unchanged.
    fn page(&self, auth: &WatchlistAuth, start: u32, size: u32, if_none_match: Option<&str>)
        -> Result<WatchPage, WatchlistError>;
    fn released(&self, auth: &WatchlistAuth) -> Result<Vec<WatchItem>, WatchlistError>;
    fn write(&self, auth: &WatchlistAuth, hex: &str, intent: Intent) -> Result<(), WatchlistError>;
    fn find(&self, server: ServerId, kind: GuidKind, guids: &[String]) -> Result<Vec<GuidMatch>, GuidLookupError>;
    /// `plex::servers::roster_gen()`.
    fn roster_gen(&self) -> u32;
    /// `plex::servers::facts_gen()`: moves when reachability or the roster's facts do.
    fn facts_gen(&self) -> u32;
    /// Today as `YYYY-MM-DD` (UTC), the fallback "coming soon" test when the released list failed.
    fn today(&self) -> Option<String>;
    /// WORKER THREAD. The `guid` of the item `rk` on `server`'s own record
    /// (`client.metadata(rk).guid`): what a card row that carried no catalog id is resolved by
    /// (`plex::resolve_catalog_hex`). `None` when the server did not answer (the caller then forgets the
    /// attempt and asks again at the next menu; a guid that is not portable answers `Some`). The default answers
    /// nothing, so a script that never resolves needs no line for it.
    fn guid_of(&self, _server: ServerId, _rk: &str) -> Option<String> {
        None
    }
}

/// The production seam: libcurl through `plex::watchlist`, matching through the registered
/// servers' clients, reachability from the probe results.
pub struct LiveIo;

impl Reach for LiveIo {
    fn reachable(&self, server: ServerId) -> bool {
        matches!(plx_plex::plex::server_probe_result(server), Some(plx_plex::plex::probe::Outcome::Reachable))
    }
    fn roster(&self) -> Vec<ServerId> {
        plx_plex::plex::server_ids().collect()
    }
    fn copy_rank(&self, server: ServerId) -> u8 {
        if plx_plex::plex::current_server() == server {
            0
        } else if plx_plex::plex::server_facts(server).is_some_and(|f| f.owned) {
            1
        } else {
            2
        }
    }
}

impl WatchlistIo for LiveIo {
    fn capture_auth(&self) -> Option<WatchlistAuth> {
        WatchlistAuth::capture()
    }
    fn is_current(&self, auth: &WatchlistAuth) -> bool {
        auth.is_current()
    }
    fn page(&self, auth: &WatchlistAuth, start: u32, size: u32, if_none_match: Option<&str>)
        -> Result<WatchPage, WatchlistError>
    {
        let client = auth.client();
        let mut transport = client.watchlist_transport();
        let answer = client.watchlist_page(start, size, if_none_match, &mut transport);
        answer
    }
    fn released(&self, auth: &WatchlistAuth) -> Result<Vec<WatchItem>, WatchlistError> {
        let client = auth.client();
        let mut transport = client.watchlist_transport();
        let answer = client.watchlist_released(&mut transport);
        answer
    }
    fn write(&self, auth: &WatchlistAuth, hex: &str, intent: Intent) -> Result<(), WatchlistError> {
        let client = auth.client();
        let mut transport = client.watchlist_transport();
        let answer = match intent {
            Intent::Add => client.watchlist_add(hex, &mut transport),
            Intent::Remove => client.watchlist_remove(hex, &mut transport),
        };
        answer
    }
    fn find(&self, server: ServerId, kind: GuidKind, guids: &[String]) -> Result<Vec<GuidMatch>, GuidLookupError> {
        plx_plex::plex::client_for(server).ok_or(GuidLookupError::Unanswered)?.find_by_guids(kind, guids)
    }
    fn roster_gen(&self) -> u32 {
        plx_plex::plex::server_roster_gen()
    }
    fn facts_gen(&self) -> u32 {
        plx_plex::plex::server_facts_gen()
    }
    fn guid_of(&self, server: ServerId, rk: &str) -> Option<String> {
        plx_plex::plex::client_for(server)?.metadata(rk).map(|m| m.guid)
    }
    fn today(&self) -> Option<String> {
        let secs = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).ok()?.as_secs();
        Some(ymd_of_unix_days((secs / 86_400) as i64))
    }
}

/// Civil date of a day count since 1970-01-01 (Howard Hinnant's algorithm).
fn ymd_of_unix_days(days: i64) -> String {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}-{m:02}-{d:02}")
}

/// Which request a landing answers: the profile generation it was captured under, the store epoch
/// (a profile switch or reset rotates it) and the request id within the epoch.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Tag {
    pub profile_gen: u32,
    pub epoch: u32,
    pub req: u32,
}

/// What a worker brings back. Every variant is tagged by its [`Landed`].
#[derive(Clone, Debug)]
pub enum What {
    /// One page of the list, with the matches of its rows against every reachable server.
    Page {
        start: u32,
        total: Option<u32>,
        rows: Vec<WatchRow>,
        answers: Vec<Answer>,
        roster_gen: u32,
        /// Page 0's `ETag` (the validator for the next read), when the service sent one.
        etag: Option<String>,
    },
    /// Page 0 answered 304: the rows already held are the service's current list, so no page is
    /// read and nothing is replaced.
    NotModified,
    /// The page could not be read; the list stops here.
    PageFailed { start: u32, why: Failure },
    /// The released set as a guid set, or `None` when that read failed.
    Released { guids: Option<HashSet<String>> },
    /// A full re-match of the rows handed over (the roster moved).
    Rematch { answers: Vec<Answer>, roster_gen: u32 },
    /// Fresh answers from ONE server for some guids (a library change its feed reported): merged
    /// over the table, which is otherwise kept as it is.
    Delta { answers: Vec<Answer> },
    /// The list worker is finished (every page read or the cap reached).
    ListDone,
    /// The write worker's verdict.
    Written { write: PendingWrite, outcome: WriteOutcome },
}

#[derive(Clone, Debug)]
pub struct Landed {
    pub tag: Tag,
    pub what: What,
}

/// Why a list read stopped.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Failure {
    /// 401: the account credential is refused.
    Unauthorized,
    /// No answer from plex.tv.
    Offline,
    /// Any other refusal or an unreadable body.
    Other,
}

impl Failure {
    fn of(e: &WatchlistError) -> Failure {
        match e {
            WatchlistError::Unauthorized => Failure::Unauthorized,
            WatchlistError::Transport => Failure::Offline,
            _ => Failure::Other,
        }
    }
}

/// One server's answer for one guid.
pub type Answer = (String, ServerId, Asked);

/// `plex://movie|show/<hex>` to the 12 bytes of its hex, or `None` for anything else.
pub fn decode_hex(hex: &str) -> Option<[u8; 12]> {
    if hex.len() != 24 || !hex.is_ascii() {
        return None;
    }
    let mut out = [0u8; 12];
    for (i, b) in out.iter_mut().enumerate() {
        *b = u8::from_str_radix(&hex[2 * i..2 * i + 2], 16).ok()?;
    }
    Some(out)
}

/// The inverse of [`decode_hex`]: the 24 lowercase hex digits the service takes.
pub fn encode_hex(hex: &[u8; 12]) -> String {
    hex.iter().map(|b| format!("{b:02x}")).collect()
}

/// `plex://movie|show/<24 hex>`: the guid string a Remove write and a page's removal landing are keyed by.
pub fn guid_of(kind: model::Kind, hex: &[u8; 12]) -> String {
    let k = match kind {
        model::Kind::Movie => "movie",
        model::Kind::Show => "show",
    };
    format!("plex://{k}/{}", encode_hex(hex))
}

/// A stored row from a decoded item, or `None` for a kind the page does not show or an id the
/// service did not give in the form it takes. `order` is the row's server-side position.
pub fn row_of(item: &WatchItem, order: u32) -> Option<WatchRow> {
    let kind = match item.kind? {
        WatchKind::Movie => Kind::Movie,
        WatchKind::Show => Kind::Show,
        WatchKind::Other => return None,
    };
    let hex = decode_hex(&item.hex)?;
    Some(WatchRow {
        guid: item.guid.clone(),
        hex,
        kind,
        title: item.title.clone().unwrap_or_default(),
        title_sort: item.title_sort.clone().unwrap_or_default(),
        year: item.year,
        added_at: item.added_at,
        release: item.originally_available.clone(),
        rating: item.rating,
        // Settled by the released set when it lands; until then a row counts as released.
        released: true,
        order,
        thumb: item.thumb.clone().unwrap_or_default(),
    })
}

/// A lookup row's colours as bytes (the wire's hex colours are bytes, so this is exact).
fn quantize_blur(c: [[f32; 3]; 4]) -> crate::watchlist::model::Blur {
    c.map(|corner| corner.map(|v| (v.clamp(0.0, 1.0) * 255.0).round() as u8))
}

fn guid_kind(kind: Kind) -> GuidKind {
    match kind {
        Kind::Movie => GuidKind::Movie,
        Kind::Show => GuidKind::Show,
    }
}

/// Match `rows` (guid, kind) against every reachable server: per server and kind, chunks of
/// [`GUIDS_PER_REQUEST`]. A guid the server answered about is `Held` or `Absent`; a server that did
/// not answer a chunk leaves its guids unanswered (silent), so `Absent` is never inferred.
pub fn match_rows(io: &dyn WatchlistIo, rows: &[(String, Kind)]) -> Vec<Answer> {
    let mut out = Vec::new();
    for sid in io.roster().into_iter().filter(|s| io.reachable(*s)) {
        out.extend(match_on(io, sid, rows));
    }
    out
}

/// [`match_rows`] against ONE server (it is not asked whether the server is reachable).
pub fn match_on(io: &dyn WatchlistIo, sid: ServerId, rows: &[(String, Kind)]) -> Vec<Answer> {
    let mut out = Vec::new();
    for kind in [Kind::Movie, Kind::Show] {
        let mut seen = HashSet::new();
        let guids: Vec<String> = rows
            .iter()
            .filter(|(g, k)| *k == kind && g.starts_with("plex://") && seen.insert(g.clone()))
            .map(|(g, _)| g.clone())
            .collect();
        for chunk in guids.chunks(GUIDS_PER_REQUEST) {
            let Ok(found) = io.find(sid, guid_kind(kind), chunk) else { continue };
            for g in chunk {
                let hit = found.iter().find(|m| &m.guid == g);
                let held = hit.and_then(|m| m.rating_key.parse::<i64>().ok());
                match (hit.is_some(), held) {
                    (true, Some(rating_key)) => out.push((
                        g.clone(),
                        sid,
                        Asked::Held(ServerMatch {
                            server_id: sid,
                            rating_key,
                            blur: hit.and_then(|m| m.blur).map(quantize_blur),
                        }),
                    )),
                    (true, None) => {} // a key we cannot use: neither held nor absent
                    (false, _) => out.push((g.clone(), sid, Asked::Absent)),
                }
            }
        }
    }
    out
}

/// WORKER THREAD: read the list. Page 0, then the released set, then the remaining pages until
/// the server's own end (`start` reaches `totalSize`, a short page, or an empty one); each page is
/// matched against the servers before it is posted so a tile arrives with its state. Stops at the first page that fails. Always ends with [`What::ListDone`].
/// `known` is the guids already matched against every reachable server under roster generation
/// `known.1`; they are skipped while the roster has not moved (a new roster is a full re-match).
/// `etag` is the validator of the last complete read (`If-None-Match` on page 0 only): when the
/// service answers 304 the list is not read at all and [`What::NotModified`] stands in for every
/// page, but the released set is still read (a title's release moves it without moving the list).
pub fn run_list(
    io: &dyn WatchlistIo,
    auth: &WatchlistAuth,
    tag: Tag,
    known: (&HashSet<String>, u32),
    etag: Option<&str>,
    post: &mut dyn FnMut(Landed),
) {
    let mut start = 0u32;
    let mut page_no = 0u32;
    loop {
        let page = match io.page(auth, start, MAX_PAGE, etag.filter(|_| start == 0)) {
            Ok(p) => p,
            Err(WatchlistError::NotModified) if start == 0 && etag.is_some() => {
                post(Landed { tag, what: What::NotModified });
                let guids = io.released(auth).ok().map(|items| items.into_iter().map(|i| i.guid).collect());
                post(Landed { tag, what: What::Released { guids } });
                break;
            }
            Err(e) => {
                post(Landed { tag, what: What::PageFailed { start, why: Failure::of(&e) } });
                break;
            }
        };
        // Paging follows the wire: an entry the decoder dropped still held a slot.
        let got = page.raw;
        let rows: Vec<WatchRow> =
            page.items.iter().enumerate().filter_map(|(i, it)| row_of(it, start + i as u32)).collect();
        let roster_gen = io.roster_gen();
        let skip_known = known.1 == roster_gen;
        let keys: Vec<(String, Kind)> = rows
            .iter()
            .filter(|r| !(skip_known && known.0.contains(&r.guid)))
            .map(|r| (r.guid.clone(), r.kind))
            .collect();
        let answers = match_rows(io, &keys);
        let total = page.total;
        let etag = page.etag;
        post(Landed { tag, what: What::Page { start, total, rows, answers, roster_gen, etag } });
        if page_no == 0 {
            let guids = io.released(auth).ok().map(|items| items.into_iter().map(|i| i.guid).collect());
            post(Landed { tag, what: What::Released { guids } });
        }
        start += got;
        let more = match total {
            Some(t) => start < t,
            None => got >= MAX_PAGE,
        };
        // Ends on the server's answer (`start` at `totalSize`, or a short page). A page that
        // carried no entries advances nothing, so it also ends the loop: that is loop safety
        // against a misbehaving server, not a bound on the list.
        if got == 0 || !more {
            break;
        }
        page_no += 1;
    }
    post(Landed { tag, what: What::ListDone });
}

/// WORKER THREAD: the full re-match of `rows` after the roster moved.
pub fn run_rematch(io: &dyn WatchlistIo, rows: &[(String, Kind)], tag: Tag, post: &mut dyn FnMut(Landed)) {
    let roster_gen = io.roster_gen();
    let answers = match_rows(io, rows);
    post(Landed { tag, what: What::Rematch { answers, roster_gen } });
}

/// WORKER THREAD: ask ONE server again about `rows` after its library changed.
pub fn run_delta(io: &dyn WatchlistIo, sid: ServerId, rows: &[(String, Kind)], tag: Tag, post: &mut dyn FnMut(Landed)) {
    post(Landed { tag, what: What::Delta { answers: match_on(io, sid, rows) } });
}

/// WORKER THREAD: one write. A failure of any kind is `Failed`; the ledger reverts the flip.
pub fn run_write(io: &dyn WatchlistIo, auth: &WatchlistAuth, tag: Tag, write: PendingWrite, post: &mut dyn FnMut(Landed)) {
    let outcome = match io.write(auth, &encode_hex(&write.hex), write.intent) {
        Ok(()) => WriteOutcome::Landed,
        Err(_) => WriteOutcome::Failed,
    };
    post(Landed { tag, what: What::Written { write, outcome } });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_round_trips_and_rejects_the_wrong_shape() {
        let h = decode_hex("0123456789abcdef01234567").unwrap();
        assert_eq!(encode_hex(&h), "0123456789abcdef01234567");
        assert!(decode_hex("0123").is_none());
        assert!(decode_hex("zz23456789abcdef01234567").is_none());
        assert!(decode_hex("0123456789abcdef0123456é").is_none());
    }

    #[test]
    fn civil_dates_come_out_right() {
        assert_eq!(ymd_of_unix_days(0), "1970-01-01");
        assert_eq!(ymd_of_unix_days(19_782), "2024-02-29");
        assert_eq!(ymd_of_unix_days(20_001), "2024-10-05");
    }

    #[test]
    fn the_auth_debug_form_hides_the_credential() {
        let a = WatchlistAuth {
            client_id: "client".into(),
            credential: "SECRET".into(),
            user: UserRef::default(),
            generation: 3,
        };
        let shown = format!("{a:?}");
        assert!(!shown.contains("SECRET") && !shown.contains("client"));
    }
}
