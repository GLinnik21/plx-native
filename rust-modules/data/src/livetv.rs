//! Live TV's server-fanned read model. Steps receive keys and time; only the job body reads PMS.
use crate::pms::PmsMovie;
use crate::stores::fanout::{Fanout, Standing};
use plx_machine::job::{Job, Port};
use plx_machine::machine::{Addr, MachineId, RequestId};
use plx_plex::plex::livetv::{self, LiveChannel};
use plx_plex::plex::{Client, ClientKey, ClientKeys, ServerId};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::Arc;

const RETRY_FRAMES: u32 = 120;
/// A guide with no current airing is sampled again so a later programme can appear.
const GUIDE_POLL_SECONDS: i64 = 60;
pub const KIND_LIVE_CHANNEL: i32 = 5;
pub const LIVE_TV_HUB: &str = "home.livetv";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LiveTvStatus { Loading, Ready, Empty, Failed }

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Channel {
    pub sid: ServerId,
    pub dvr: String,
    pub channel: String,
    pub vcn: String,
    pub name: String,
    pub thumb: String,
    pub now: String,
    pub now_thumb: String,
    pub ends_at: i64,
}

impl Channel {
    fn of(sid: ServerId, ch: &LiveChannel) -> Self {
        Self { sid, dvr: ch.dvr.clone(), channel: ch.channel.clone(), vcn: ch.vcn.clone(),
            name: ch.name.clone(), thumb: ch.thumb.clone(), now: ch.now.clone(),
            now_thumb: ch.now_thumb.clone(), ends_at: ch.ends_at }
    }
}

pub fn channel_tile(ch: &Channel) -> PmsMovie {
    PmsMovie { sid: ch.sid, kind: KIND_LIVE_CHANNEL, rk: format!("livetv:{}:{}", ch.dvr, ch.channel),
        title: format!("{} {}", ch.vcn, ch.name).trim().to_owned(), show_title: ch.now.clone(),
        // The unnumbered identity is retained for Play, including a channel with no vcn.
        summary: ch.name.clone(), thumb: ch.thumb.clone(), still: ch.thumb.clone(), ..Default::default() }
}

pub fn live_channel_of(m: &PmsMovie) -> Option<(&str, &str)> {
    if m.kind != KIND_LIVE_CHANNEL { return None; }
    let (dvr, channel) = m.rk.strip_prefix("livetv:")?.split_once(':')?;
    (livetv::is_path_id(dvr) && livetv::is_path_id(channel)).then_some((dvr, channel))
}

/// A tile's channel name, without its displayed virtual number.
pub fn channel_name_of(m: &PmsMovie) -> Option<&str> {
    live_channel_of(m)?;
    Some(&m.summary)
}

#[derive(Clone, Copy)]
pub struct LiveTvView<'a> {
    pub status: LiveTvStatus,
    pub channels: &'a [Channel],
    pub tiles: &'a [Arc<PmsMovie>],
    revision: u32,
}
impl<'a> LiveTvView<'a> {
    pub fn revision(self) -> u32 { self.revision }
    #[cfg(any(test, feature = "test-support"))]
    pub fn for_test(status: LiveTvStatus, channels: &'a [Channel], tiles: &'a [Arc<PmsMovie>], revision: u32) -> Self {
        Self { status, channels, tiles, revision }
    }
    pub const LOADING: Self = Self { status: LiveTvStatus::Loading, channels: &[], tiles: &[], revision: 0 };
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LiveTvCmd { Reset, Retry }

#[derive(Clone, Debug, PartialEq, Eq)]
struct Guide { dvr: String, provider: String, channels: Vec<LiveChannel>, refresh_at: i64, retry_cd: u32 }
impl Guide {
    fn due(&self, now: i64) -> bool { self.retry_cd == 0 && now >= self.refresh_at }
    fn next_refresh(channels: &[LiveChannel], now: i64) -> i64 {
        channels.iter().filter_map(|c| (c.ends_at > now).then_some(c.ends_at)).min()
            .unwrap_or_else(|| now.saturating_add(GUIDE_POLL_SECONDS))
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Work { Discover, Grid(Guide) }

#[derive(Debug, PartialEq, Eq)]
pub struct LiveTvReq { sid: ServerId, key: ClientKey, now: i64, work: Work }
impl LiveTvReq {
    pub fn server(&self) -> (ServerId, ClientKey) { (self.sid, self.key) }
}

struct ReadResult { guides: Vec<Guide>, roster: Option<Vec<String>>, complete: bool, retry: bool }
pub struct LiveTvAnswer { result: Option<ReadResult>, grid: bool }
struct Mail { request: u32, answer: LiveTvAnswer }
#[derive(Default)]
pub struct LiveTvAdapter { fetch: crate::stores::Fetch<Mail> }

#[derive(Clone)]
pub struct LiveTvPort(Arc<LiveTvAdapter>);
impl PartialEq for LiveTvPort {
    fn eq(&self, other: &Self) -> bool { Arc::ptr_eq(&self.0, &other.0) }
}
impl std::fmt::Debug for LiveTvPort {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "LiveTvPort({:p})", Arc::as_ptr(&self.0))
    }
}
impl Port<LiveTvAnswer> for LiveTvPort {
    fn done(&self, addr: Addr, answer: LiveTvAnswer) {
        self.0.fetch.post(Mail { request: addr.req.0, answer }, |old| addr.req.0 >= old.request);
    }
    fn refused(&self, addr: Addr) { self.0.fetch.refuse_for(addr.req.0); }
}

struct Source {
    sid: ServerId,
    key: Option<ClientKey>,
    adapter: Arc<LiveTvAdapter>,
    request: u32,
    guides: Vec<Guide>,
    reading_grid: Option<String>,
    answered: bool,
    complete: bool,
    failed: bool,
    retry_cd: u32,
}
impl Source {
    fn new(sid: ServerId, key: Option<ClientKey>) -> Self {
        Self { sid, key, adapter: Arc::new(Default::default()), request: 0, guides: Vec::new(),
            reading_grid: None, answered: false, complete: false, failed: false, retry_cd: 0 }
    }
    fn work(&self, now: i64) -> Option<Work> {
        self.guides.iter().filter(|g| g.due(now)).min_by_key(|g| g.refresh_at).cloned().map(Work::Grid)
            .or_else(|| (!self.complete && self.retry_cd == 0).then_some(Work::Discover))
    }
    fn failed(&mut self) {
        self.failed = true;
        if let Some(dvr) = self.reading_grid.take() {
            if let Some(guide) = self.guides.iter_mut().find(|g| g.dvr == dvr) { guide.retry_cd = RETRY_FRAMES; }
        } else { self.retry_cd = RETRY_FRAMES; }
    }
    fn standing(&self, now: i64) -> Standing {
        if self.adapter.fetch.busy() { Standing::Running }
        else if (!self.complete && self.retry_cd == 0) || self.guides.iter().any(|g| g.due(now)) { Standing::Wanting }
        else { Standing::Idle }
    }
}

pub struct LiveTvState {
    sources: Vec<Source>,
    channels: Vec<Channel>,
    tiles: Vec<Arc<PmsMovie>>,
    status: LiveTvStatus,
    revision: u32,
    fanout: Fanout,
    pub(crate) jobs: Vec<crate::stores::StoreJob>,
}
impl Default for LiveTvState {
    fn default() -> Self {
        Self { sources: Vec::new(), channels: Vec::new(), tiles: Vec::new(), status: LiveTvStatus::Loading,
            revision: 0, fanout: Default::default(), jobs: Vec::new() }
    }
}
impl LiveTvState {
    pub fn view(&self) -> LiveTvView<'_> {
        LiveTvView { status: self.status, channels: &self.channels, tiles: &self.tiles, revision: self.revision }
    }
    pub fn run(&mut self, cmd: LiveTvCmd, gate: &plx_machine::landgate::Gate, keys: &ClientKeys, now: i64) -> bool {
        let reset = matches!(cmd, LiveTvCmd::Reset);
        match cmd {
            LiveTvCmd::Reset => {
                self.sources.clear();
                self.jobs.clear();
                self.fanout = Fanout::default();
            }
            LiveTvCmd::Retry => {
                for s in &mut self.sources {
                    s.retry_cd = 0;
                    for guide in &mut s.guides { guide.retry_cd = 0; }
                }
            }
        }
        let changed = self.pump(gate, keys, now);
        changed || (reset && self.publish())
    }
    pub fn pump(&mut self, gate: &plx_machine::landgate::Gate, keys: &ClientKeys, now: i64) -> bool {
        let slots: Vec<_> = keys.slots().filter(|s| *s != ServerId::UNSET).collect();
        let before = self.sources.len();
        self.sources.retain(|s| slots.contains(&s.sid));
        let mut dirty = before != self.sources.len();
        for sid in slots {
            let key = keys.key(sid);
            match self.sources.iter_mut().find(|s| s.sid == sid) {
                Some(s) if s.key != key => { *s = Source::new(sid, key); dirty = true; }
                Some(_) => {}
                None => { self.sources.push(Source::new(sid, key)); dirty = true; }
            }
        }
        for s in &mut self.sources {
            s.retry_cd = s.retry_cd.saturating_sub(1);
            for guide in &mut s.guides { guide.retry_cd = guide.retry_cd.saturating_sub(1); }
            let adapter = Arc::clone(&s.adapter);
            let fetch = &adapter.fetch;
            let mail = crate::stores::take_landing_owed(gate, crate::stores::StoreId::LiveTv,
                || fetch.busy(), || fetch.take_current(|m| m.request));
            if let Some(mail) = mail.filter(|m| m.request == s.request) {
                dirty = true;
                match mail.answer.result {
                    Some(result) => {
                        s.reading_grid = None;
                        if let Some(roster) = result.roster { s.guides.retain(|g| roster.contains(&g.dvr)); }
                        if mail.answer.grid || !result.complete {
                            for guide in result.guides {
                                if let Some(old) = s.guides.iter_mut().find(|g| g.dvr == guide.dvr) { *old = guide; }
                                else { s.guides.push(guide); }
                            }
                        } else { s.guides = result.guides; }
                        if result.retry && !mail.answer.grid { s.retry_cd = RETRY_FRAMES; }
                        s.answered = true;
                        if !mail.answer.grid { s.complete = result.complete; }
                        s.failed = result.retry;
                    }
                    None => s.failed(),
                }
            }
            if fetch.take_refused(s.request) { s.failed(); dirty = true; }
        }
        let mut turn = self.fanout.turn(self.sources.iter().enumerate().map(|(i, s)| (i, s.standing(now))));
        for (i, s) in self.sources.iter_mut().enumerate() {
            if s.standing(now) != Standing::Wanting || !turn.admit(i) { continue; }
            let Some(key) = s.key else {
                dirty |= !s.failed;
                s.failed = true;
                s.retry_cd = RETRY_FRAMES;
                turn.finished();
                continue;
            };
            let work = s.work(now).expect("a wanting Live TV source has work");
            s.reading_grid = match &work { Work::Discover => None, Work::Grid(g) => Some(g.dvr.clone()) };
            s.request = s.request.wrapping_add(1);
            s.adapter.fetch.claim(s.request);
            self.jobs.push(crate::stores::StoreJob::LiveTv(Job {
                addr: Addr { to: MachineId::Store(crate::stores::StoreId::LiveTv.ord()), req: RequestId(s.request) },
                req: LiveTvReq { sid: s.sid, key, now, work },
            }, LiveTvPort(Arc::clone(&s.adapter))));
        }
        dirty && self.publish()
    }
    fn publish(&mut self) -> bool {
        let mut channels: Vec<_> = self.sources.iter().flat_map(|s| s.guides.iter()
            .flat_map(move |g| g.channels.iter().map(move |c| Channel::of(s.sid, c)))).collect();
        channels.sort_by(|a, b| livetv::vcn_order(&a.vcn, &b.vcn));
        let status = if !channels.is_empty() { LiveTvStatus::Ready }
            else if self.sources.iter().any(|s| s.failed) { LiveTvStatus::Failed }
            else if self.sources.is_empty() || self.sources.iter().any(|s| !s.answered) { LiveTvStatus::Loading }
            else { LiveTvStatus::Empty };
        if channels == self.channels && status == self.status { return false; }
        self.tiles = channels.iter().map(|c| Arc::new(channel_tile(c))).collect();
        self.channels = channels;
        self.status = status;
        self.revision = self.revision.wrapping_add(1);
        plx_machine::idle::invalidate();
        true
    }
    #[cfg(any(test, feature = "test-support"))]
    pub(crate) fn install_for_test(&mut self, channels: Vec<Channel>) {
        self.status = if channels.is_empty() { LiveTvStatus::Empty } else { LiveTvStatus::Ready };
        self.tiles = channels.iter().map(|c| Arc::new(channel_tile(c))).collect();
        self.channels = channels;
        self.revision = self.revision.wrapping_add(1);
    }
}

/// The only IO in this model; even a panic answers once so the next pump releases the claim.
pub fn finish_livetv_job(port: &LiveTvPort, addr: Addr, client: &'static Client, req: LiveTvReq) {
    finish_with(port, addr, req, |req| read(client, req));
}
fn finish_with(port: &LiveTvPort, addr: Addr, req: LiveTvReq,
    read: impl FnOnce(&LiveTvReq) -> Option<ReadResult>) {
    let grid = matches!(req.work, Work::Grid(_));
    let result = catch_unwind(AssertUnwindSafe(|| read(&req))).unwrap_or(None);
    port.done(addr, LiveTvAnswer { result, grid });
}
trait LiveTvIo {
    fn dvrs(&self) -> Option<Vec<plx_plex::plex::Dvr>>;
    fn channels(&self, provider: &str) -> Option<Vec<plx_plex::plex::EpgChannel>>;
    fn on_now(&self, provider: &str, now: i64) -> Option<Vec<plx_plex::plex::Metadata>>;
}
impl LiveTvIo for Client {
    fn dvrs(&self) -> Option<Vec<plx_plex::plex::Dvr>> { self.live_tv_dvrs() }
    fn channels(&self, provider: &str) -> Option<Vec<plx_plex::plex::EpgChannel>> { self.epg_channels(provider) }
    fn on_now(&self, provider: &str, now: i64) -> Option<Vec<plx_plex::plex::Metadata>> { self.epg_on_now(provider, now) }
}
fn read(client: &impl LiveTvIo, req: &LiveTvReq) -> Option<ReadResult> {
    match &req.work {
        Work::Discover => {
            let dvrs = client.dvrs()?;
            let dvrs: Vec<_> = livetv::usable_dvrs(&dvrs).collect();
            let mut result = ReadResult { guides: Vec::new(), roster: Some(dvrs.iter().map(|d| d.key.clone()).collect()), complete: true, retry: false };
            for dvr in dvrs {
                let Some(channels) = client.channels(&dvr.epg_identifier) else {
                    result.complete = false; result.retry = true; continue;
                };
                let mut channels = livetv::lineup(dvr, &channels, &[]);
                if channels.is_empty() {
                    result.guides.push(Guide { dvr: dvr.key.clone(), provider: dvr.epg_identifier.clone(), channels,
                        refresh_at: i64::MAX, retry_cd: 0 });
                    continue;
                }
                let airings = client.on_now(&dvr.epg_identifier, req.now);
                if let Some(airings) = airings.as_deref() { livetv::update_on_now(&mut channels, airings); }
                let refresh_at = if airings.is_some() { Guide::next_refresh(&channels, req.now) }
                    else { result.retry = true; req.now };
                result.guides.push(Guide { dvr: dvr.key.clone(), provider: dvr.epg_identifier.clone(), channels, refresh_at, retry_cd: if airings.is_some() { 0 } else { RETRY_FRAMES } });
            }
            Some(result)
        }
        Work::Grid(guide) => {
            let now = client.on_now(&guide.provider, req.now)?;
            let mut updated = guide.clone();
            livetv::update_on_now(&mut updated.channels, &now);
            updated.refresh_at = Guide::next_refresh(&updated.channels, req.now);
            Some(ReadResult { guides: vec![updated], roster: None, complete: true, retry: false })
        }
    }
}

#[cfg(test)]
mod tests;
