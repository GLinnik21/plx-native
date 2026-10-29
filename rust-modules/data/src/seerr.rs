//! seerr: the data layer of the household's Seerr server (docs/seerr.md).
//!
//! * this module: what a job asks ([`SeerrOp`]), what it brings back ([`SeerrData`]), the I/O seam
//!   ([`SeerrIo`], live in production, a script in tests) and the job body ([`run_op`]), which signs
//!   in when it holds no session and signs in once more when the server forgot the one it held.
//! * `stores::seerr` owns the state those jobs land into.
//!
//! The identity is the seated profile's plex.tv credential, taken from the [`WatchlistInput`] the
//! Bridge reads (the same input the two plex.tv stores take): Seerr signs that Plex account in and
//! answers with a session cookie. Nothing here names the credential, the cookie, the address or a
//! title in a log line or a `Debug` form.
//!
//! [`WatchlistInput`]: crate::watchlist::WatchlistInput
use plx_plex::plex::seerr::{live_transport, SeerrClient};
pub use plx_plex::plex::seerr::{
    default_server, ArrOptions, ArrServer, Availability, DiscoverRow, MediaKind, MyRequest, Page, QualityProfile,
    RequestOptions, RequestStatus, RootFolder, Season, SeerrError, SeerrSession, SeerrUser, Title, TitleDetail,
};

/// What one job asks of the server.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SeerrOp {
    /// Sign in afresh (Settings' Connect, a new address, a new profile).
    Connect,
    Search { query: String, page: u32 },
    Discover { row: DiscoverRow, page: u32 },
    Detail { kind: MediaKind, tmdb: i64 },
    MyRequests { page: u32 },
    /// The Radarr (a movie) or Sonarr (a show) servers a request can go to.
    Servers { kind: MediaKind },
    /// One of those servers' quality profiles and root folders.
    ServerOptions { kind: MediaKind, server: i64 },
    /// `POST /api/v1/request`. A show names its seasons; a movie none. `options` names where it goes
    /// (each `None` is the server's to decide).
    Request { kind: MediaKind, tmdb: i64, seasons: Vec<i32>, options: RequestOptions },
}

/// What a job that ran brought back.
#[derive(Clone, Debug, PartialEq)]
pub enum SeerrData {
    Connected,
    Titles(Page<Title>),
    Detail(TitleDetail),
    Requests(Page<MyRequest>),
    Servers(Vec<ArrServer>),
    ServerOptions(ArrOptions),
    Requested,
}

/// Everything a job asks of the outside world. Production is [`LiveSeerrIo`]; tests script it.
pub trait SeerrIo: Send + Sync {
    /// WORKER THREAD: sign the Plex account in at the owner's `address`.
    fn sign_in(&self, address: &str, plex_token: &str) -> Result<(SeerrSession, SeerrUser), SeerrError>;
    /// WORKER THREAD: one call as `session`. Never asked to run [`SeerrOp::Connect`].
    fn run(&self, address: &str, session: &SeerrSession, user: &SeerrUser, op: &SeerrOp) -> Result<SeerrData, SeerrError>;
}

/// The production seam: libcurl through `plex::seerr`.
pub struct LiveSeerrIo;

impl SeerrIo for LiveSeerrIo {
    fn sign_in(&self, address: &str, plex_token: &str) -> Result<(SeerrSession, SeerrUser), SeerrError> {
        let client = SeerrClient::for_address(address).ok_or(SeerrError::BadAddress)?;
        let mut transport = live_transport();
        let answer = client.sign_in(plex_token, &mut transport);
        answer
    }

    fn run(&self, address: &str, session: &SeerrSession, user: &SeerrUser, op: &SeerrOp) -> Result<SeerrData, SeerrError> {
        let client = SeerrClient::for_address(address).ok_or(SeerrError::BadAddress)?;
        let mut t = live_transport();
        let answer = match op {
            SeerrOp::Connect => client.me(session, &mut t).map(|_| SeerrData::Connected),
            SeerrOp::Search { query, page } => client.search(session, query, *page, &mut t).map(SeerrData::Titles),
            SeerrOp::Discover { row, page } => client.discover(session, *row, *page, &mut t).map(SeerrData::Titles),
            SeerrOp::Detail { kind, tmdb } => client.detail(session, *kind, *tmdb, &mut t).map(SeerrData::Detail),
            SeerrOp::MyRequests { page } => client.my_requests(session, user.id, *page, &mut t).map(SeerrData::Requests),
            SeerrOp::Servers { kind } => client.servers(session, *kind, &mut t).map(SeerrData::Servers),
            SeerrOp::ServerOptions { kind, server } =>
                client.server_options(session, *kind, *server, &mut t).map(SeerrData::ServerOptions),
            SeerrOp::Request { kind, tmdb, seasons, options } =>
                client.request(session, *kind, *tmdb, seasons, options, &mut t).map(|()| SeerrData::Requested),
        };
        answer
    }
}

/// What a job did to the session it was handed.
#[derive(Clone, Debug, PartialEq)]
pub enum SessionChange {
    /// It used the one it was handed.
    Kept,
    /// It signed in (it had none, it was a Connect, or the server had forgotten the old one).
    Fresh(SeerrSession, SeerrUser),
    /// It tried to sign in and could not: the answer's error is the sign-in's, and says why.
    Lost,
}

/// One job's result: what became of the session, and the answer.
#[derive(Clone, Debug, PartialEq)]
pub struct SeerrOutcome {
    pub session: SessionChange,
    pub result: Result<SeerrData, SeerrError>,
}

/// The job body. [`SeerrOp::Connect`] always signs in afresh; any other op signs in only when it
/// was handed no session. A held session the server answers 401 to (it expired, or the server
/// restarted) is signed in once more and the call made again; a second 401 is the answer. A write
/// that met a 401 never reached the server, so asking again cannot request twice.
pub fn run_op(io: &dyn SeerrIo, address: &str, plex_token: &str, held: Option<&(SeerrSession, SeerrUser)>, op: &SeerrOp)
    -> SeerrOutcome
{
    let sign_in = || io.sign_in(address, plex_token);
    let lost = |e| SeerrOutcome { session: SessionChange::Lost, result: Err(e) };
    let fresh = match (op, held) {
        (SeerrOp::Connect, _) | (_, None) => match sign_in() {
            Ok(session) => Some(session),
            Err(e) => return lost(e),
        },
        _ => None,
    };
    let change = |fresh: Option<(SeerrSession, SeerrUser)>| match fresh {
        Some((session, user)) => SessionChange::Fresh(session, user),
        None => SessionChange::Kept,
    };
    if *op == SeerrOp::Connect {
        return SeerrOutcome { session: change(fresh), result: Ok(SeerrData::Connected) };
    }
    let (session, user) = fresh.as_ref().or(held).expect("signed in above when nothing was held");
    match io.run(address, session, user, op) {
        Err(SeerrError::Unauthorized) if fresh.is_none() => match sign_in() {
            Ok(again) => {
                let result = io.run(address, &again.0, &again.1, op);
                SeerrOutcome { session: change(Some(again)), result }
            }
            Err(e) => lost(e),
        },
        result => SeerrOutcome { session: change(fresh), result },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// A server that signs in with `sign_ins` in order and answers `runs` in order, recording the
    /// session each call was made as.
    #[derive(Default)]
    struct Script {
        sign_ins: Mutex<Vec<Result<(SeerrSession, SeerrUser), SeerrError>>>,
        runs: Mutex<Vec<Result<SeerrData, SeerrError>>>,
        seen: Mutex<Vec<String>>,
    }

    impl SeerrIo for Script {
        fn sign_in(&self, _: &str, token: &str) -> Result<(SeerrSession, SeerrUser), SeerrError> {
            self.seen.lock().unwrap().push(format!("sign_in {token}"));
            self.sign_ins.lock().unwrap().remove(0)
        }
        fn run(&self, _: &str, session: &SeerrSession, _: &SeerrUser, op: &SeerrOp) -> Result<SeerrData, SeerrError> {
            assert_ne!(*op, SeerrOp::Connect, "Connect never reaches run");
            self.seen.lock().unwrap().push(format!("run as {}", if *session == s("old") { "old" } else { "new" }));
            self.runs.lock().unwrap().remove(0)
        }
    }

    fn s(cookie: &str) -> SeerrSession {
        SeerrSession::for_test(cookie)
    }

    fn user() -> SeerrUser {
        SeerrUser { id: 1, name: "s00000001".into(), permissions: 0 }
    }

    fn script(sign_ins: Vec<Result<(SeerrSession, SeerrUser), SeerrError>>, runs: Vec<Result<SeerrData, SeerrError>>) -> Script {
        Script { sign_ins: Mutex::new(sign_ins), runs: Mutex::new(runs), ..Default::default() }
    }

    const OP: SeerrOp = SeerrOp::MyRequests { page: 1 };

    fn fresh() -> SessionChange {
        SessionChange::Fresh(s("new"), user())
    }

    #[test]
    fn connect_always_signs_in_afresh_and_makes_no_other_call() {
        let io = script(vec![Ok((s("new"), user()))], vec![]);
        let out = run_op(&io, "a", "tok", Some(&(s("old"), user())), &SeerrOp::Connect);
        assert_eq!(out, SeerrOutcome { session: fresh(), result: Ok(SeerrData::Connected) });
        assert_eq!(*io.seen.lock().unwrap(), ["sign_in tok"]);
    }

    #[test]
    fn a_call_with_no_session_signs_in_first_and_one_with_a_session_does_not() {
        let io = script(vec![Ok((s("new"), user()))], vec![Ok(SeerrData::Requested)]);
        let out = run_op(&io, "a", "tok", None, &OP);
        assert_eq!(out.session, fresh());
        assert_eq!(*io.seen.lock().unwrap(), ["sign_in tok", "run as new"]);

        let io = script(vec![], vec![Ok(SeerrData::Requested)]);
        let out = run_op(&io, "a", "tok", Some(&(s("old"), user())), &OP);
        assert_eq!(out, SeerrOutcome { session: SessionChange::Kept, result: Ok(SeerrData::Requested) });
        assert_eq!(*io.seen.lock().unwrap(), ["run as old"]);
    }

    #[test]
    fn a_failing_call_with_a_good_session_keeps_it() {
        let io = script(vec![], vec![Err(SeerrError::Forbidden)]);
        let out = run_op(&io, "a", "tok", Some(&(s("old"), user())), &OP);
        assert_eq!(out, SeerrOutcome { session: SessionChange::Kept, result: Err(SeerrError::Forbidden) },
            "a request this user may not make is not a lost connection");
    }

    #[test]
    fn a_forgotten_session_is_signed_in_once_more_and_asked_again() {
        let io = script(vec![Ok((s("new"), user()))], vec![Err(SeerrError::Unauthorized), Ok(SeerrData::Requested)]);
        let out = run_op(&io, "a", "tok", Some(&(s("old"), user())), &OP);
        assert_eq!(out, SeerrOutcome { session: fresh(), result: Ok(SeerrData::Requested) });
        assert_eq!(*io.seen.lock().unwrap(), ["run as old", "sign_in tok", "run as new"]);
    }

    #[test]
    fn a_re_sign_in_the_server_refuses_loses_the_session() {
        let io = script(vec![Err(SeerrError::Forbidden)], vec![Err(SeerrError::Unauthorized)]);
        let out = run_op(&io, "a", "tok", Some(&(s("old"), user())), &OP);
        assert_eq!(out, SeerrOutcome { session: SessionChange::Lost, result: Err(SeerrError::Forbidden) });
    }

    #[test]
    fn a_second_refusal_is_the_answer_and_a_fresh_session_is_never_retried() {
        let io = script(vec![Ok((s("new"), user()))], vec![Err(SeerrError::Unauthorized), Err(SeerrError::Unauthorized)]);
        let out = run_op(&io, "a", "tok", Some(&(s("old"), user())), &OP);
        assert_eq!(out.result, Err(SeerrError::Unauthorized));
        assert_eq!(io.seen.lock().unwrap().len(), 3, "one re-sign-in, no loop");

        let io = script(vec![Ok((s("new"), user()))], vec![Err(SeerrError::Unauthorized)]);
        let out = run_op(&io, "a", "tok", None, &OP);
        assert_eq!(out.result, Err(SeerrError::Unauthorized));
        assert_eq!(*io.seen.lock().unwrap(), ["sign_in tok", "run as new"], "a session minted for this call is not retried");
    }

    #[test]
    fn a_refused_sign_in_is_the_answer() {
        let io = script(vec![Err(SeerrError::Forbidden)], vec![]);
        let out = run_op(&io, "a", "tok", None, &OP);
        assert_eq!(out, SeerrOutcome { session: SessionChange::Lost, result: Err(SeerrError::Forbidden) });
    }
}
