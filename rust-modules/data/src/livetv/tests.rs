use super::*;
use crate::stores::{Admission, StoreId, StoreJob, Stores};

fn sid(n: u16) -> ServerId { ServerId::from_raw(n) }
fn keys(n: u16) -> ClientKeys {
    ClientKeys::from_keys((0..n).map(|n| (sid(n), ClientKey { instance: n as u32 + 1, token: 1, grant: 1 })))
}
fn take(stores: &mut Stores) -> (Job<LiveTvReq>, LiveTvPort) {
    let mut jobs = stores.take_jobs_of(StoreId::LiveTv);
    assert_eq!(jobs.len(), 1);
    let job = jobs.pop().unwrap();
    assert_eq!(job.admission(), Admission::Direct);
    let StoreJob::LiveTv(job, port) = job else { panic!("Live TV emits its own job") };
    (job, port)
}
fn guide(numbers: &[&str], end: i64) -> Guide {
    Guide { dvr: "7".into(), provider: "tv.plex.providers.epg.xmltv:7".into(), refresh_at: end, retry_cd: 0, channels: numbers.iter()
        .map(|number| LiveChannel { dvr: "7".into(), channel: format!("C{number}"), vcn: number.to_string(),
            name: "Channel".into(), thumb: "/logo".into(), now: "Show".into(), now_thumb: "/show".into(), ends_at: end }).collect() }
}
fn complete(guides: Vec<Guide>) -> ReadResult { ReadResult { guides, roster: None, complete: true, retry: false } }
fn answer(stores: &mut Stores, result: Option<Vec<Guide>>) {
    let (job, port) = take(stores);
    finish_with(&port, job.addr, job.req, |_| result.map(complete));
}

#[test]
fn livetv_initial_job_and_ready_publication_keep_the_whole_sorted_lineup() {
    let mut stores = Stores::default();
    let keys = keys(2);
    assert!(!stores.livetv_pump(&keys, 100));
    assert_eq!(stores.livetv_view().status, LiveTvStatus::Loading);
    let jobs = stores.take_jobs_of(StoreId::LiveTv);
    assert_eq!(jobs.len(), 2, "every registered server is asked, independently of Home");
    for job in jobs {
        assert_eq!(job.store(), StoreId::LiveTv);
        let StoreJob::LiveTv(job, port) = job else { unreachable!() };
        let (sid, key) = job.req.server();
        assert_eq!(keys.key(sid), Some(key));
        assert!(matches!(job.req.work, Work::Discover));
        assert_eq!(job.req.now, 100);
        assert_eq!(job.addr.to, MachineId::Store(StoreId::LiveTv.ord()));
        let numbers: Vec<_> = (1..=30).rev().map(|n| (n + sid.raw() as i32 * 30).to_string()).collect();
        finish_with(&port, job.addr, job.req, |_| Some(complete(vec![guide(&numbers.iter().map(String::as_str).collect::<Vec<_>>(), 200)])));
    }
    assert!(stores.livetv_pump(&keys, 100));
    let view = stores.livetv_view();
    assert_eq!(view.status, LiveTvStatus::Ready);
    assert_eq!(view.channels.len(), 60, "no 24-card cap on the lineup");
    assert_eq!(view.channels.iter().map(|c| c.vcn.parse::<usize>().unwrap()).collect::<Vec<_>>(), (1..=60).collect::<Vec<_>>());
    assert_eq!(view.tiles.len(), 60);
    assert_eq!(view.tiles[0].title, "1 Channel");
    assert_eq!(view.tiles[0].show_title, "Show");
    assert_eq!(view.tiles[0].thumb, "/logo");
    assert_eq!(live_channel_of(&view.tiles[0]), Some(("7", "C1")));
    assert_eq!(channel_name_of(&view.tiles[0]), Some("Channel"));
    let revision = view.revision();
    assert!(stores.take_notices().iter().any(|(id, _)| *id == StoreId::LiveTv));
    assert!(!stores.livetv_pump(&keys, 100));
    assert_eq!(stores.livetv_view().revision(), revision);
    assert!(stores.take_jobs_of(StoreId::LiveTv).is_empty());
    assert!(stores.take_notices().is_empty());
}

#[test]
fn livetv_subchannel_numbers_publish_in_guide_order() {
    let mut stores = Stores::default(); let keys = keys(1);
    stores.livetv_pump(&keys, 100);
    answer(&mut stores, Some(vec![guide(&["10.1", "2.1", "2"], 200)]));
    assert!(stores.livetv_pump(&keys, 100));
    assert_eq!(stores.livetv_view().channels.iter().map(|c| c.vcn.as_str()).collect::<Vec<_>>(), ["2", "2.1", "10.1"]);
}

#[test]
fn livetv_answered_no_dvr_is_empty_and_does_not_retry() {
    let mut stores = Stores::default(); let keys = keys(1);
    stores.livetv_pump(&keys, 100);
    answer(&mut stores, Some(Vec::new()));
    assert!(stores.livetv_pump(&keys, 100));
    assert_eq!(stores.livetv_view().status, LiveTvStatus::Empty);
    for _ in 0..RETRY_FRAMES + 1 { stores.livetv_pump(&keys, 500); }
    assert!(!stores.has_jobs());
}

#[test]
fn livetv_failure_and_refusal_back_off_and_retry_once() {
    for refusal in [false, true] {
        let mut stores = Stores::default(); let keys = keys(1);
        stores.livetv_pump(&keys, 100);
        let (job, port) = take(&mut stores);
        if refusal { port.refused(job.addr); } else { finish_with(&port, job.addr, job.req, |_| None); }
        assert!(stores.livetv_pump(&keys, 100));
        assert_eq!(stores.livetv_view().status, LiveTvStatus::Failed);
        for _ in 0..RETRY_FRAMES - 1 {
            stores.livetv_pump(&keys, 100);
            assert!(!stores.has_jobs(), "no per-frame launch loop");
        }
        stores.livetv_pump(&keys, 100);
        let (job, port) = take(&mut stores);
        assert!(matches!(job.req.work, Work::Discover));
        port.refused(job.addr);
        // A second failure in a row waits twice as long before the next read.
        for _ in 0..2 * RETRY_FRAMES {
            stores.livetv_pump(&keys, 100);
            assert!(!stores.has_jobs(), "the wait doubles after a second failure");
        }
        stores.livetv_pump(&keys, 100);
        let (job, port) = take(&mut stores);
        finish_with(&port, job.addr, job.req, |_| Some(complete(vec![guide(&["1"], 200)])));
        stores.livetv_pump(&keys, 100);
        assert_eq!(stores.livetv_view().status, LiveTvStatus::Ready);
    }
}

#[test]
fn livetv_missing_key_marks_failed_without_emitting_or_spinning() {
    let mut keys = ClientKeys::default();
    keys.refill_roster(ServerId::UNSET, 1, 1, [sid(0)], |_| None, |_| None, |_| None);
    let mut stores = Stores::default();
    assert!(stores.livetv_pump(&keys, 100));
    assert_eq!(stores.livetv_view().status, LiveTvStatus::Failed);
    for _ in 0..RETRY_FRAMES { stores.livetv_pump(&keys, 100); assert!(!stores.has_jobs()); }
}

#[test]
fn livetv_airing_end_emits_one_grid_only_refresh_and_failure_keeps_lineup() {
    let mut stores = Stores::default(); let keys = keys(1);
    stores.livetv_pump(&keys, 100);
    answer(&mut stores, Some(vec![guide(&["10", "2"], 200)]));
    stores.livetv_pump(&keys, 199);
    assert!(!stores.has_jobs());
    stores.livetv_pump(&keys, 200);
    let (job, port) = take(&mut stores);
    assert!(matches!(&job.req.work, Work::Grid(g) if g.dvr == "7" && g.channels.len() == 2));
    assert_eq!(job.req.now, 200);
    for _ in 0..5 { stores.livetv_pump(&keys, 210); assert!(!stores.has_jobs()); }
    finish_with(&port, job.addr, job.req, |_| None);
    let previous = stores.livetv_view().channels.to_vec();
    let revision = stores.livetv_view().revision();
    stores.livetv_pump(&keys, 210);
    assert_eq!(stores.livetv_view().status, LiveTvStatus::Ready);
    assert_eq!(stores.livetv_view().channels, previous);
    assert_eq!(stores.livetv_view().revision(), revision);
    for _ in 0..RETRY_FRAMES - 1 { stores.livetv_pump(&keys, 210); assert!(!stores.has_jobs()); }
    stores.livetv_pump(&keys, 210);
    let (job, port) = take(&mut stores);
    assert!(matches!(job.req.work, Work::Grid(_)));
    let mut next = guide(&["10", "2"], 300);
    next.channels[0].now = "Next show".into();
    finish_with(&port, job.addr, job.req, |_| Some(complete(vec![next])));
    assert!(stores.livetv_pump(&keys, 211));
    assert!(stores.livetv_view().revision() > revision);
    assert_eq!(stores.livetv_view().channels.len(), 2);
    assert!(stores.livetv_view().channels.iter().any(|c| c.now == "Next show"));
}

#[test]
fn livetv_fanout_bounds_requests_and_eventually_asks_every_registered_server() {
    let mut stores = Stores::default(); let keys = keys(11);
    let mut seen = Vec::new();
    for _ in 0..4 {
        stores.livetv_pump(&keys, 100);
        let jobs = stores.take_jobs_of(StoreId::LiveTv);
        assert!(jobs.len() <= crate::stores::fanout::WIDTH);
        for job in jobs {
            let StoreJob::LiveTv(job, port) = job else { unreachable!() };
            seen.push(job.req.sid);
            finish_with(&port, job.addr, job.req, |_| Some(complete(Vec::new())));
        }
    }
    seen.sort_by_key(|s| s.raw());
    assert_eq!(seen, (0..11).map(sid).collect::<Vec<_>>());
    assert_eq!(stores.livetv_view().status, LiveTvStatus::Empty);
}

#[test]
fn livetv_panicking_worker_answers_failure_and_reset_orphans_the_old_port() {
    let mut stores = Stores::default(); let keys = keys(1);
    stores.livetv_pump(&keys, 100);
    let (job, port) = take(&mut stores);
    finish_with(&port, job.addr, job.req, |_| panic!("read panicked"));
    stores.livetv_pump(&keys, 100);
    assert_eq!(stores.livetv_view().status, LiveTvStatus::Failed);
    stores.livetv_run(LiveTvCmd::Retry, &keys, 100);
    let (old, old_port) = take(&mut stores);
    stores.livetv_run(LiveTvCmd::Reset, &keys, 100);
    let (new, new_port) = take(&mut stores);
    assert_ne!(old_port, new_port);
    finish_with(&old_port, old.addr, old.req, |_| Some(complete(vec![guide(&["1"], 200)])));
    stores.livetv_pump(&keys, 100);
    assert_eq!(stores.livetv_view().status, LiveTvStatus::Loading);
    finish_with(&new_port, new.addr, new.req, |_| Some(complete(Vec::new())));
    stores.livetv_pump(&keys, 100);
    assert_eq!(stores.livetv_view().status, LiveTvStatus::Empty);
}

struct Script {
    calls: std::cell::RefCell<Vec<String>>,
    second_dvr: bool,
    fail_lineup: std::cell::Cell<bool>,
    fail_grid: std::cell::Cell<bool>,
    empty_grid: bool,
    empty_lineup: bool,
}
impl Script {
    fn new() -> Self { Self { calls: Default::default(), second_dvr: false, fail_lineup: false.into(), fail_grid: false.into(), empty_grid: false, empty_lineup: false } }
}
impl LiveTvIo for Script {
    fn dvrs(&self) -> Option<Vec<plx_plex::plex::Dvr>> {
        self.calls.borrow_mut().push("dvrs".into());
        Some((7..=if self.second_dvr { 8 } else { 7 }).map(|id| plx_plex::plex::Dvr {
            key: id.to_string(), epg_identifier: format!("tv.plex.providers.epg.xmltv:{id}"), ..Default::default()
        }).collect())
    }
    fn channels(&self, provider: &str) -> Option<Vec<plx_plex::plex::EpgChannel>> {
        self.calls.borrow_mut().push(format!("channels:{provider}"));
        if self.fail_lineup.get() && provider.ends_with('8') { return None; }
        if self.empty_lineup { return Some(Vec::new()); }
        Some(vec![plx_plex::plex::EpgChannel { grid_key: "C2".into(), vcn: "2".into(), title: "2 Channel".into(), thumb: "/logo".into() }])
    }
    fn on_now(&self, provider: &str, now: i64) -> Option<Vec<plx_plex::plex::Metadata>> {
        self.calls.borrow_mut().push(format!("grid:{provider}:{now}"));
        if self.fail_grid.get() { None }
        else if self.empty_grid { Some(Vec::new()) }
        else { Some(vec![serde_json::from_value(serde_json::json!({"title":"Programme","thumb":"/programme",
            "Media":[{"channelVcn":"2","channelIdentifier":"C2","endsAt":now + 50}]})).unwrap()]) }
    }
}
fn execute(stores: &mut Stores, io: &Script) {
    let (job, port) = take(stores);
    finish_with(&port, job.addr, job.req, |req| read(io, req));
}

#[test]
fn livetv_missing_initial_grid_preserves_lineup_and_retries_only_the_grid() {
    let mut stores = Stores::default(); let keys = keys(1); let io = Script::new();
    io.fail_grid.set(true);
    stores.livetv_pump(&keys, 100);
    execute(&mut stores, &io);
    stores.livetv_pump(&keys, 100);
    assert_eq!(stores.livetv_view().status, LiveTvStatus::Ready);
    assert_eq!(stores.livetv_view().channels[0].now, "");
    assert_eq!(stores.livetv_view().channels[0].now_thumb, "/logo");
    io.fail_grid.set(false); io.calls.borrow_mut().clear();
    for _ in 0..RETRY_FRAMES { stores.livetv_pump(&keys, 101); }
    execute(&mut stores, &io);
    stores.livetv_pump(&keys, 101);
    assert_eq!(io.calls.borrow().as_slice(), ["grid:tv.plex.providers.epg.xmltv:7:101"]);
    assert_eq!(stores.livetv_view().channels[0].now, "Programme");
    assert_eq!(stores.livetv_view().channels[0].now_thumb, "/programme");
}

#[test]
fn livetv_empty_successful_grid_is_sampled_again_when_programming_can_begin() {
    let mut stores = Stores::default(); let keys = keys(1); let mut io = Script::new();
    io.empty_grid = true;
    stores.livetv_pump(&keys, 100); execute(&mut stores, &io); stores.livetv_pump(&keys, 100);
    stores.livetv_pump(&keys, 159); assert!(!stores.has_jobs());
    stores.livetv_pump(&keys, 160);
    let (job, port) = take(&mut stores);
    assert!(matches!(job.req.work, Work::Grid(_)));
    port.refused(job.addr);
}

#[test]
fn livetv_one_failed_dvr_lineup_does_not_drop_the_dvr_that_answered() {
    let mut stores = Stores::default(); let keys = keys(1); let mut io = Script::new();
    io.second_dvr = true; io.fail_lineup.set(true);
    stores.livetv_pump(&keys, 100); execute(&mut stores, &io); stores.livetv_pump(&keys, 100);
    assert_eq!(stores.livetv_view().status, LiveTvStatus::Ready);
    assert_eq!(stores.livetv_view().channels.len(), 1);
    assert_eq!(stores.livetv_view().channels[0].dvr, "7");
    io.fail_lineup.set(false);
    for _ in 0..RETRY_FRAMES { stores.livetv_pump(&keys, 101); }
    execute(&mut stores, &io); stores.livetv_pump(&keys, 101);
    assert_eq!(stores.livetv_view().channels.len(), 2);
}

#[test]
fn livetv_refreshes_the_earliest_expired_dvr_and_reset_removal_notifies() {
    let mut stores = Stores::default(); let keys = keys(1);
    stores.livetv_pump(&keys, 100);
    let first = guide(&["10"], 250);
    let mut earlier = guide(&["2"], 200); earlier.dvr = "8".into();
    earlier.channels[0].dvr = "8".into(); earlier.provider = "tv.plex.providers.epg.xmltv:8".into();
    answer(&mut stores, Some(vec![first, earlier])); stores.livetv_pump(&keys, 100);
    stores.livetv_pump(&keys, 300);
    let (job, port) = take(&mut stores);
    assert!(matches!(&job.req.work, Work::Grid(g) if g.dvr == "8"));
    port.refused(job.addr);
    stores.take_notices();
    let revision = stores.livetv_view().revision();
    assert!(stores.livetv_run(LiveTvCmd::Reset, &keys, 300));
    assert_eq!(stores.livetv_view().status, LiveTvStatus::Loading);
    assert!(stores.livetv_view().channels.is_empty());
    assert!(stores.livetv_view().revision() > revision);
    assert!(stores.take_notices().iter().any(|(id, _)| *id == StoreId::LiveTv));
}

#[test]
fn livetv_reset_with_no_keys_clears_the_publication_and_notifies() {
    let mut stores = Stores::default(); let keys = keys(1);
    stores.livetv_pump(&keys, 100); answer(&mut stores, Some(vec![guide(&["2"], 200)]));
    stores.livetv_pump(&keys, 100); stores.take_notices();
    let revision = stores.livetv_view().revision();
    assert!(stores.livetv_run(LiveTvCmd::Reset, &ClientKeys::default(), 101));
    assert_eq!(stores.livetv_view().status, LiveTvStatus::Loading);
    assert!(stores.livetv_view().channels.is_empty());
    assert!(stores.livetv_view().revision() > revision);
    assert!(stores.take_notices().iter().any(|(id, _)| *id == StoreId::LiveTv));
}

#[test]
fn livetv_a_failed_grid_does_not_starve_another_expired_dvr() {
    let mut stores = Stores::default(); let keys = keys(1);
    stores.livetv_pump(&keys, 100);
    let first = guide(&["1"], 200);
    let mut second = guide(&["2"], 250); second.dvr = "8".into();
    second.channels[0].dvr = "8".into();
    answer(&mut stores, Some(vec![first, second])); stores.livetv_pump(&keys, 100);
    stores.livetv_pump(&keys, 300);
    let (job, port) = take(&mut stores);
    assert!(matches!(&job.req.work, Work::Grid(g) if g.dvr == "7"));
    finish_with(&port, job.addr, job.req, |_| None);
    stores.livetv_pump(&keys, 300);
    let (job, port) = take(&mut stores);
    assert!(matches!(&job.req.work, Work::Grid(g) if g.dvr == "8"));
    port.refused(job.addr);
}

#[test]
fn livetv_a_successful_empty_lineup_retires_channels_during_partial_discovery() {
    let mut stores = Stores::default(); let keys = keys(1); let mut io = Script::new();
    io.second_dvr = true; io.fail_lineup.set(true);
    stores.livetv_pump(&keys, 100); execute(&mut stores, &io); stores.livetv_pump(&keys, 100);
    assert_eq!(stores.livetv_view().channels.len(), 1);
    io.empty_lineup = true;
    for _ in 0..RETRY_FRAMES { stores.livetv_pump(&keys, 101); }
    execute(&mut stores, &io); stores.livetv_pump(&keys, 101);
    assert!(stores.livetv_view().channels.is_empty());
}

#[test]
fn livetv_partial_discovery_does_not_block_an_expired_known_guide() {
    let mut stores = Stores::default(); let keys = keys(1); let mut io = Script::new();
    io.second_dvr = true; io.fail_lineup.set(true);
    stores.livetv_pump(&keys, 100); execute(&mut stores, &io); stores.livetv_pump(&keys, 100);
    stores.livetv_pump(&keys, 151);
    let (job, port) = take(&mut stores);
    assert!(matches!(&job.req.work, Work::Grid(g) if g.dvr == "7"));
    port.refused(job.addr);
}

#[test]
fn livetv_play_title_preserves_the_unnumbered_name_and_stale_guides_do_not_spin() {
    for number in ["", "ABC", "10"] {
        let mut channel = Channel::of(sid(0), &guide(&[number], 200).channels[0]);
        channel.name = "Family Channel".into();
        assert_eq!(channel_name_of(&channel_tile(&channel)), Some("Family Channel"));
    }
    assert_eq!(Guide::next_refresh(&guide(&["1"], 200).channels, 201), 261);
}

#[test]
fn an_armed_landing_gate_asks_no_live_tv_lineup() {
    // A recording or replay owns the run and its tape carries no Live TV read.
    let mut stores = Stores::default();
    stores.landgate.arm_recording();
    let keys = keys(2);
    assert!(!stores.livetv_pump(&keys, 100));
    assert!(stores.take_jobs_of(StoreId::LiveTv).is_empty());
    assert!(stores.livetv_view().channels.is_empty());
    stores.landgate.disarm();
    assert!(!stores.livetv_pump(&keys, 100));
    assert_eq!(stores.take_jobs_of(StoreId::LiveTv).len(), 2);
}
