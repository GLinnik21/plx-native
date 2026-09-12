//! Complete owned detail terminals at the consumer boundary.
use super::*;
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::sync::Mutex;
pub(super) mod float_bits {
    use serde::{Deserialize, Serializer, Deserializer};
    pub fn serialize<S: Serializer>(v: &f64, s: S) -> Result<S::Ok, S::Error> { s.serialize_u64(v.to_bits()) }
    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<f64, D::Error> {
        Ok(f64::from_bits(u64::deserialize(d)?))
    }
}
pub(super) mod blur_bits {
    use serde::{Serialize, Deserialize, Serializer, Deserializer};
    pub fn serialize<S: Serializer>(v: &[[f32;3];4], s: S) -> Result<S::Ok, S::Error> {
        v.map(|r| r.map(f32::to_bits)).serialize(s)
    }
    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<[[f32;3];4], D::Error> {
        Ok(<[[u32;3];4]>::deserialize(d)?.map(|r| r.map(f32::from_bits)))
    }
}
pub(super) mod server_id {
    use serde::{Deserialize, Serializer, Deserializer};
    pub fn serialize<S: Serializer>(sid: &crate::plex::ServerId, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_u16(sid.raw())
    }
    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<crate::plex::ServerId, D::Error> {
        Ok(crate::plex::ServerId::from_raw(u16::deserialize(d)?))
    }
}
#[derive(serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Reply {
    seq: u64,
    req: u32,
    terminal: bool,
    lane: ReplyLane,
}
#[derive(serde::Serialize, serde::Deserialize)]
enum ReplyLane { Data((u16, String), Option<Detail>), Dropped(u32), Refused(u32) }

struct Tracker {
    enabled: bool,
    seq: u64,
    active: BTreeMap<u32, bool>,
    pending: VecDeque<Reply>,
    failure: Option<&'static str>,
    boundary: u64,
}

static TRACKER: Mutex<Tracker> = Mutex::new(Tracker {
    enabled: false,
    seq: 0,
    active: BTreeMap::new(),
    pending: VecDeque::new(),
    failure: None,
    boundary: 0,
});

fn tracker() -> std::sync::MutexGuard<'static, Tracker> {
    TRACKER.lock().unwrap_or_else(|e| e.into_inner())
}

pub(crate) fn reset(enabled: bool) {
    *tracker() = Tracker { enabled, seq:0, active:BTreeMap::new(), pending:VecDeque::new(), failure:None, boundary:0 };
}

pub(super) fn admit(addr: crate::ui::machine::Addr)
    -> Result<(), crate::ui::landing::AdmissionError> {
    let mut tracker = tracker();
    let result = DETAIL_LANDING.admit(addr);
    if result.is_ok() && tracker.enabled && tracker.active.insert(addr.req.0, false).is_some() {
        tracker.failure = Some("duplicate tracked detail admission");
    }
    result
}

/// The worker and cancellation serialize on TRACKER before taking the Landing lock. Completions
/// visible here retire NOW, before another admission, while running workers retain their slots.
/// Record that boundary in the synchronous effect stream, not in the next pump's result batch.
pub(super) fn cancel_all() {
    let replay = crate::app::bootstrap::stores::replaying();
    let mut tracker = tracker();
    if tracker.enabled {
        tracker.boundary += 1;
        for cancelled in tracker.active.values_mut() { *cancelled = true; }
        for reply in &mut tracker.pending {
            reply.lane = ReplyLane::Dropped(reply.req);
        }
        let result = (|| {
            let observed = serde_json::to_value(&tracker.pending).map_err(|_| "invalid detail cancellation encoding")?;
            let value = crate::app::bootstrap::stores::detail_cancellation(tracker.boundary, observed)?;
            let retired: Vec<Reply> = serde_json::from_value(value).map_err(|_| "invalid detail cancellation replies")?;
            if replay {
                publish_replies(&mut tracker, &retired)?;
                if !tracker.pending.is_empty() { return Err("unrecorded local detail cancellation"); }
            } else { tracker.pending.clear(); }
            for reply in retired { tracker.active.remove(&reply.req); }
            Ok(())
        })();
        if let Err(reason) = result {
            tracker.failure = Some(reason);
            crate::app::bootstrap::stores::fail(reason);
        }
    }
    DETAIL_LANDING.clear();
}

fn push_terminal(tracker: &mut Tracker, req: u32, lane: ReplyLane) {
    if !tracker.enabled { return; }
    if !tracker.active.contains_key(&req) {
        tracker.failure = Some("unknown detail publication");
        return;
    }
    if tracker.pending.iter().any(|reply| reply.req == req) || tracker.pending.len() >= 4 {
        tracker.failure = Some("duplicate or excess detail publication");
        return;
    }
    tracker.seq = tracker.seq.wrapping_add(1);
    tracker.pending.push_back(Reply { seq:tracker.seq, req, terminal:true, lane });
}

pub(super) fn put(addr: crate::ui::machine::Addr, key: DetailKey, data: Option<Detail>) {
    let mut tracker = tracker();
    let shadow = if tracker.enabled {
        serde_json::to_value(&data).ok().and_then(|value| serde_json::from_value(value).ok())
    } else { None };
    let result = DETAIL_LANDING.put(addr, key.clone(), data);
    let lane = if tracker.active.get(&addr.req.0).copied().unwrap_or(false)
        || result == Err(crate::ui::landing::PublishError::Full) {
        Some(ReplyLane::Dropped(addr.req.0))
    } else if result.is_ok() {
        shadow.map(|data| ReplyLane::Data(((key.0).raw(), key.1), data))
    } else { None };
    if let Some(lane) = lane { push_terminal(&mut tracker, addr.req.0, lane); }
    else if tracker.enabled {
        tracker.failure = Some("failed detail publication");
    }
}

pub(super) fn refused(addr: crate::ui::machine::Addr) {
    let replay = crate::app::bootstrap::stores::replaying();
    let mut tracker = tracker();
    let result = DETAIL_LANDING.refused(addr);
    let lane = if tracker.active.get(&addr.req.0).copied().unwrap_or(false) {
        ReplyLane::Dropped(addr.req.0)
    } else { ReplyLane::Refused(addr.req.0) };
    if result.is_ok() {
        if replay && tracker.enabled {
            // The request path synthesizes this terminal already. Its publication sequence is
            // assigned when the recorded observation arrives: unsupplied worker completions may
            // precede this synchronous refusal. Never advance the replay sequence here.
            tracker.pending.push_back(Reply { seq:0, req:addr.req.0, terminal:true, lane });
        } else { push_terminal(&mut tracker, addr.req.0, lane); }
    }
    else if tracker.enabled { tracker.failure = Some("failed detail refusal publication"); }
}

fn matches_landed(reply: &Reply, landed: &crate::ui::landing::Landed<DetailKey, Option<Detail>>) -> bool {
    use crate::ui::landing::Lane;
    if reply.req != landed.addr.req.0 || reply.terminal != landed.terminal { return false; }
    match (&reply.lane, &landed.lane) {
        (ReplyLane::Data((sid, rk), expected), Lane::Data((actual_sid, actual_rk), actual)) =>
            *sid == actual_sid.raw() && rk == actual_rk
                && serde_json::to_value(expected).ok() == serde_json::to_value(actual).ok(),
        (ReplyLane::Dropped(expected), Lane::Dropped(actual)) => *expected == actual.0,
        (ReplyLane::Refused(expected), Lane::Refused(actual)) => *expected == actual.0,
        _ => false,
    }
}

pub(crate) fn validate(value: &serde_json::Value) -> Result<(), &'static str> {
    let replies: Vec<Reply> = serde_json::from_value(value.clone()).map_err(|_| "invalid detail replies")?;
    if replies.is_empty() || replies.len() > 4 || serde_json::to_value(&replies).ok().as_ref() != Some(value) {
        return Err("noncanonical detail replies");
    }
    let mut seen = BTreeSet::new();
    let mut last = None;
    for reply in &replies {
        if !reply.terminal { return Err("nonterminal detail reply"); }
        if !seen.insert(reply.req) { return Err("duplicate detail terminal"); }
        if last.is_some_and(|seq| reply.seq <= seq) { return Err("incoherent detail reply sequence"); }
        last = Some(reply.seq);
        match &reply.lane {
            ReplyLane::Dropped(req) | ReplyLane::Refused(req) if *req != reply.req =>
                return Err("mismatched detail control request"),
            ReplyLane::Data((sid, rk), Some(detail))
                if rk.is_empty() || detail.sid.raw() != *sid || detail.rk != *rk =>
                return Err("mismatched detail data identity"),
            ReplyLane::Data((_, rk), _) if rk.is_empty() => return Err("invalid detail data identity"),
            _ => {}
        }
    }
    Ok(())
}

pub(crate) fn validate_retired(value: &serde_json::Value) -> Result<(), &'static str> {
    validate(value)?;
    let replies: Vec<Reply> = serde_json::from_value(value.clone()).map_err(|_| "invalid detail cancellation replies")?;
    if replies.iter().any(|r| !matches!(r.lane, ReplyLane::Dropped(req) if req == r.req)) {
        return Err("non-dropped detail cancellation terminal");
    }
    Ok(())
}

pub(super) struct Drain {
    pub(super) landed: Vec<crate::ui::landing::Landed<DetailKey, Option<Detail>>>,
}

pub(super) fn drain_live(want: &Option<DetailKey>) -> Option<(Vec<Reply>, Drain)> {
    let mut tracker = tracker();
    let mut out = Vec::new();
    DETAIL_LANDING.take_for(&|_| true, &|key| want.as_ref().is_none_or(|wanted| key == wanted), &mut out);
    let replies: Vec<_> = tracker.pending.drain(..).collect();
    if out.iter().any(|landed| !replies.iter().any(|reply| matches_landed(reply, landed))) {
        tracker.failure = Some("detail drain did not match recorded completion");
    }
    for reply in &replies { tracker.active.remove(&reply.req); }
    if let Some(reason) = tracker.failure.take() { crate::app::bootstrap::stores::fail(reason); }
    if replies.is_empty() && out.is_empty() { None } else { Some((replies, Drain { landed:out })) }
}

/// Replay denied every worker launch. Each supplied terminal must retire a reservation that the
/// real request path admitted; publication errors are replay failures, never normalized away.
pub(super) fn supply(replies: Vec<Reply>, want: &Option<DetailKey>)
    -> Result<(Vec<Reply>, Drain), &'static str> {
    validate(&serde_json::to_value(&replies).map_err(|_| "invalid detail replies")?)?;
    let mut tracker = tracker();
    publish_replies(&mut tracker, &replies)?;
    if !tracker.pending.is_empty() { return Err("unconsumed local detail terminal"); }
    let mut out = Vec::new();
    DETAIL_LANDING.take_for(&|_| true, &|key| want.as_ref().is_none_or(|wanted| key == wanted), &mut out);
    if out.iter().any(|landed| !replies.iter().any(|reply| matches_landed(reply, landed))) {
        return Err("detail drain did not match supplied completion");
    }
    for reply in &replies { tracker.active.remove(&reply.req); }
    Ok((replies, Drain { landed:out }))
}

fn publish_replies(tracker: &mut Tracker, replies: &[Reply]) -> Result<(), &'static str> {
    for reply in replies {
        if reply.seq != tracker.seq.wrapping_add(1) { return Err("incoherent detail reply sequence"); }
        let cancelled = tracker.active.get(&reply.req).copied().ok_or("unknown detail publication")?;
        if cancelled && !matches!(reply.lane, ReplyLane::Dropped(req) if req == reply.req) {
            return Err("mismatched cancelled detail terminal");
        }
        if let Some(i) = tracker.pending.iter().position(|local| local.req == reply.req) {
            let local = &tracker.pending[i];
            if local.terminal != reply.terminal
                || serde_json::to_value(&local.lane).ok() != serde_json::to_value(&reply.lane).ok() {
                return Err("mismatched locally synthesized detail terminal");
            }
            tracker.pending.remove(i);
            tracker.seq = reply.seq;
            continue; // Already published by the real false-spawn path; consume it once below.
        }
        let addr = detail_addr(reply.req);
        let result = match &reply.lane {
            ReplyLane::Data((sid, rk), data) => {
                let data = serde_json::to_value(data).ok().and_then(|value| serde_json::from_value(value).ok())
                    .ok_or("invalid detail result encoding")?;
                DETAIL_LANDING.put(addr, (crate::plex::ServerId::from_raw(*sid), rk.clone()), data)
            }
            ReplyLane::Dropped(_) => DETAIL_LANDING.dropped(addr),
            ReplyLane::Refused(_) => DETAIL_LANDING.refused(addr),
        };
        result.map_err(|_| "failed detail publication")?;
        tracker.seq = reply.seq;
    }
    Ok(())
}

#[derive(Default)]
pub(crate) struct Validator {
    admitted: BTreeMap<u32, (bool, u16, String)>,
    last_seq: u64,
    last_boundary: u64,
    // Preflight visits a frame's effects before its results. A cancellation effect can describe
    // a completion AFTER that frame's pump. Compress consecutive retirements until the missing
    // pump batch is checked; at most four gaps can come from the four reserved completions.
    retired_ranges: VecDeque<(u64, u64)>,
}

impl Validator {
    pub(crate) fn admission(&mut self, value: &serde_json::Value) -> Result<(), &'static str> {
        if value["request"]["store"] == "metadata-cancel" {
            crate::app::bootstrap::stores::validate_admission(value, 0)?;
            let boundary = value["request"]["boundary"].as_u64().ok_or("invalid detail cancellation boundary")?;
            if boundary <= self.last_boundary { return Err("incoherent detail cancellation boundary"); }
            self.last_boundary = boundary;
            return self.complete(&value["request"]["retired"], true);
        }
        if value["request"]["store"] != "metadata" { return Ok(()); }
        let req = value["request"]["gen"].as_u64().and_then(|n| u32::try_from(n).ok())
            .ok_or("invalid detail admission request")?;
        let launched = value["admitted"].as_bool().ok_or("invalid detail admission answer")?;
        let sid = value["request"]["sid"].as_u64().and_then(|n| u16::try_from(n).ok())
            .ok_or("invalid detail admission identity")?;
        let rk = value["request"]["rk"].as_str().filter(|rk| !rk.is_empty())
            .ok_or("invalid detail admission identity")?.to_owned();
        if self.admitted.insert(req, (launched, sid, rk)).is_some() {
            return Err("duplicate detail admission");
        }
        Ok(())
    }

    pub(crate) fn completion(&mut self, value: &serde_json::Value) -> Result<(), &'static str> {
        self.complete(value, false)
    }

    fn complete(&mut self, value: &serde_json::Value, cancelled: bool) -> Result<(), &'static str> {
        validate(value)?;
        let replies: Vec<Reply> = serde_json::from_value(value.clone()).map_err(|_| "invalid detail replies")?;
        for reply in replies {
            if !cancelled && reply.seq != self.last_seq.wrapping_add(1) {
                return Err("incoherent detail reply sequence");
            }
            let (launched, sid, rk) = self.admitted.remove(&reply.req)
                .ok_or("unknown detail publication")?;
            if let ReplyLane::Data((actual_sid, actual_rk), _) = &reply.lane {
                if *actual_sid != sid || actual_rk != &rk {
                    return Err("mismatched detail admission identity");
                }
            }
            match (launched, &reply.lane) {
                (false, ReplyLane::Dropped(req)) if cancelled && *req == reply.req => {}
                (false, ReplyLane::Refused(req)) if *req == reply.req => {}
                (false, _) => return Err("mismatched synthesized detail refusal"),
                (true, ReplyLane::Refused(_)) => return Err("unexpected detail refusal"),
                (true, _) => {}
            }
            if cancelled {
                if reply.seq <= self.last_seq { return Err("incoherent detail reply sequence"); }
                match self.retired_ranges.back_mut() {
                    Some((_, end)) if reply.seq <= *end => return Err("incoherent detail reply sequence"),
                    Some((_, end)) if reply.seq == *end + 1 => *end = reply.seq,
                    _ => self.retired_ranges.push_back((reply.seq, reply.seq)),
                }
            } else { self.last_seq = reply.seq; }
            while self.retired_ranges.front().is_some_and(|(start, _)| *start == self.last_seq + 1) {
                self.last_seq = self.retired_ranges.pop_front().unwrap().1;
            }
            if self.retired_ranges.len() > 4 { return Err("excess detail cancellation sequence gaps"); }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn controlled(replay: bool) {
        crate::app::bootstrap::stores::reset_for_test();
        super::super::clear();
        DETAIL_GEN.store(0, std::sync::atomic::Ordering::SeqCst);
        DETAIL_DONE.store(0, std::sync::atomic::Ordering::SeqCst);
        let initial = crate::app::bootstrap::Initial::synthetic_home(1, 32498, None).unwrap();
        crate::app::bootstrap::stores::init(&initial, replay);
        reset(true);
    }

    fn request(rk: &str, launched: bool, replay: bool) -> u32 {
        let mut req = 0;
        request_detail_with_spawn(crate::plex::ServerId::from_raw(0), rk, |gen| {
            req = gen;
            crate::app::bootstrap::stores::admit(serde_json::json!({"store":"metadata",
                "sid":0,"rk":rk,"gen":gen,"client":1}), || {
                assert!(!replay, "replay must not execute the resource closure");
                launched
            })
        });
        req
    }

    #[test]
    fn p2_real_spawn_refusal_replays_exactly_once() {
        let _guard = crate::testlock::serial();
        use crate::app::bootstrap::stores;
        controlled(false);
        request("refused", false, false);
        assert!(!pump_detail());
        assert!(!detail_loading());
        let results = stores::take_results();
        let (admissions, failure) = stores::finish();
        assert_eq!(failure, None);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0]["data"][0]["seq"], 1);
        controlled(true);
        stores::begin(admissions.clone().into(), results.clone().into());
        let req = request("refused", false, true);
        let changed = pump_detail();
        let loading = detail_loading();
        let observed = stores::take_results();
        let outcome = stores::finish();
        let pending = tracker().pending.len();
        let seq = tracker().seq;
        // Always retire a failed replay's locally queued refusal before asserting (shared globals).
        super::super::clear();
        stores::reset_for_test();
        assert_eq!(outcome, (admissions, None));
        assert!(!changed);
        assert!(!loading);
        assert_eq!(observed, results);
        assert_eq!((pending, seq), (0, 1));
        assert_eq!(DETAIL_LANDING.inflight(detail_addr(req).to), 0);
    }

    #[test]
    fn p2_refusal_sequence_follows_worker_completion_and_cancel_consumes_once() {
        let _guard = crate::testlock::serial();
        use crate::app::bootstrap::stores;
        for cancelled in [false, true] {
            controlled(false);
            let old = request("old", true, false);
            let refused = request("refused", false, false);
            // The old worker finishes after the synchronous refusal but before the observation.
            land_detail(crate::plex::ServerId::from_raw(0), "old", old, None);
            if cancelled { super::super::clear(); }
            pump_detail();
            let results = stores::take_results();
            let (admissions, failure) = stores::finish();
            assert_eq!(failure, None);
            let mut validator = Validator::default();
            for admission in &admissions { validator.admission(admission).unwrap(); }
            for result in &results { validator.completion(&result["data"]).unwrap(); }
            controlled(true);
            stores::begin(admissions.clone().into(), results.clone().into());
            request("old", true, true);
            request("refused", false, true);
            if cancelled { super::super::clear(); }
            pump_detail();
            let outcome = stores::finish();
            assert_eq!(outcome, (admissions, None));
            assert_eq!(stores::take_results(), results);
            assert_eq!(tracker().seq, 2);
            assert!(tracker().pending.is_empty());
            assert_eq!(DETAIL_LANDING.inflight(detail_addr(refused).to), 0);
            stores::reset_for_test();
        }
    }

    #[test]
    fn p2_cancellation_validator_handles_result_before_later_same_frame_effect() {
        let _guard = crate::testlock::serial();
        use crate::app::bootstrap::stores;
        controlled(false);
        let first = request("first", true, false);
        land_detail(crate::plex::ServerId::from_raw(0), "first", first, None);
        pump_detail();
        let second = request("second", true, false);
        land_detail(crate::plex::ServerId::from_raw(0), "second", second, None);
        super::super::clear();
        let (admissions, failure) = stores::finish();
        let results = stores::take_results();
        stores::reset_for_test();
        assert_eq!(failure, None);
        // The recorder preflight enumerates effects before results within each frame.
        let mut validator = Validator::default();
        for admission in &admissions { validator.admission(admission).unwrap(); }
        for result in &results { validator.completion(&result["data"]).unwrap(); }
        assert_eq!(validator.last_seq, 2);
    }

    #[test]
    fn p2_publication_before_cancel_frees_capacity_before_next_pump() {
        let _guard = crate::testlock::serial();
        use crate::app::bootstrap::stores;
        controlled(false);
        let mut workers = Vec::new();
        for n in 0..4 { workers.push(request(&format!("item-{n}"), true, false)); }
        // Exactly three cancelled workers still running; the fourth has completed before clear.
        land_detail(crate::plex::ServerId::from_raw(0), "item-3", workers[3], None);
        let fifth = request("fifth", true, false);
        assert_ne!(fifth, 0, "recording admits the fifth without a pump");
        let (admissions, failure) = stores::finish();
        assert_eq!(failure, None);
        let recording_drops = DETAIL_LANDING.dropped_count();
        // Finish all actual reservations before resetting the test environment.
        for &req in workers.iter().take(3).chain(std::iter::once(&fifth)) {
            land_detail(crate::plex::ServerId::from_raw(0), "unused", req, None);
        }
        pump_detail();
        controlled(true);
        let before = DETAIL_LANDING.dropped_count();
        stores::begin(admissions.clone().into(), Default::default());
        for n in 0..4 { request(&format!("item-{n}"), true, true); }
        let replay_fifth = request("fifth", true, true);
        let outcome = stores::finish();
        let replay_drops = DETAIL_LANDING.dropped_count() - before;
        // Cleanup works on RED too, when the fifth was capacity-rejected.
        for req in 1..=5 { let _ = DETAIL_LANDING.dropped(detail_addr(req)); }
        DETAIL_LANDING.clear();
        stores::reset_for_test();
        assert_eq!(replay_fifth, fifth, "cancellation must retire at the boundary before admission");
        assert_eq!(outcome, (admissions, None));
        assert!(recording_drops > 0);
        assert_eq!(replay_drops, 1, "the cancellation drop is observed before the pump too");
    }

    fn dropped(seq: u64, req: u32) -> Reply {
        Reply { seq, req, terminal: true, lane: ReplyLane::Dropped(req) }
    }

    #[test]
    fn malformed_detail_terminal_batches_are_rejected() {
        let _guard = crate::testlock::serial();
        let valid = dropped(1, 7);
        let value = serde_json::to_value(&vec![valid]).unwrap();
        validate(&value).unwrap();

        let duplicate = serde_json::to_value(&vec![dropped(1, 7), dropped(2, 7)]).unwrap();
        assert_eq!(validate(&duplicate), Err("duplicate detail terminal"));

        let mut nonterminal = dropped(1, 7);
        nonterminal.terminal = false;
        assert_eq!(validate(&serde_json::to_value(vec![nonterminal]).unwrap()),
            Err("nonterminal detail reply"));

        let mut mismatched = dropped(1, 7);
        mismatched.lane = ReplyLane::Dropped(8);
        assert_eq!(validate(&serde_json::to_value(vec![mismatched]).unwrap()),
            Err("mismatched detail control request"));

        assert_eq!(validate(&serde_json::to_value(vec![dropped(2, 7), dropped(1, 8)]).unwrap()),
            Err("incoherent detail reply sequence"));

        reset(true);
        cancel_all();
        assert_eq!(supply(vec![dropped(1, 77)], &None).err(), Some("unknown detail publication"));

        let addr = detail_addr(7);
        admit(addr).unwrap();
        supply(vec![dropped(1, 7)], &None).unwrap();
        assert_eq!(supply(vec![dropped(2, 7)], &None).err(), Some("unknown detail publication"),
            "a second terminal cannot publish into an already consumed reservation");

        let addr = detail_addr(8);
        admit(addr).unwrap();
        cancel_all();
        let data = Reply { seq:2, req:8, terminal:true,
            lane:ReplyLane::Data((0,"8".into()), Some(Detail {
                sid:crate::plex::ServerId::from_raw(0), rk:"8".into(), ..Default::default()
            })) };
        assert_eq!(supply(vec![data], &None).err(), Some("mismatched cancelled detail terminal"));
        supply(vec![dropped(2, 8)], &None).unwrap();
        reset(false);

        let admission = |req, launched| serde_json::json!({"content_resource":true,
            "request":{"store":"metadata","sid":0,"rk":"8","gen":req,"client":1},
            "admitted":launched});
        let mut validator = Validator::default();
        validator.admission(&admission(9, false)).unwrap();
        let refused = Reply { seq:1, req:9, terminal:true, lane:ReplyLane::Refused(9) };
        validator.completion(&serde_json::to_value(vec![refused]).unwrap()).unwrap();

        let mut validator = Validator::default();
        validator.admission(&admission(10, true)).unwrap();
        let refused = Reply { seq:1, req:10, terminal:true, lane:ReplyLane::Refused(10) };
        assert_eq!(validator.completion(&serde_json::to_value(vec![refused]).unwrap()),
            Err("unexpected detail refusal"));
        let mut validator = Validator::default();
        validator.admission(&admission(12, true)).unwrap();
        let wrong_key = Reply { seq:1, req:12, terminal:true,
            lane:ReplyLane::Data((0,"different-key".into()), None) };
        assert_eq!(validator.completion(&serde_json::to_value(vec![wrong_key]).unwrap()),
            Err("mismatched detail admission identity"));
        let mut validator = Validator::default();
        assert_eq!(validator.completion(&serde_json::to_value(vec![dropped(1, 11)]).unwrap()),
            Err("unknown detail publication"));
    }

    #[test]
    fn detail_terminal_roundtrip_preserves_float_bits_and_reservations() {
        let _guard = crate::testlock::serial();
        reset(true);
        let sid = crate::plex::ServerId::from_raw(0);
        for seq in 1..=8 {
            let gen = crate::metadata::begin_detail_for_test(sid, "1001");
            let detail = Detail { sid, rk:"1001".into(), video_fps:-0.0,
                aspect_ratio:f64::from_bits(0x7ff8000000000013),
                blur:[[f32::from_bits(0x7fc00013), -0.0, f32::INFINITY];4], ..Default::default() };
            let replies = vec![Reply { seq, req:gen, terminal:true,
                lane:ReplyLane::Data((0,"1001".into()),Some(detail)) }];
            let bytes = serde_json::to_string(&replies).unwrap();
            let value = serde_json::from_str(&bytes).unwrap();
            validate(&value).unwrap();
            let decoded: Vec<Reply> = serde_json::from_value(value).unwrap();
            let landed = supply(decoded, &None).unwrap().1.landed;
            assert_eq!(landed.len(), 1);
            assert_eq!(DETAIL_LANDING.inflight(detail_addr(gen).to), 0);
            let crate::ui::landing::Lane::Data(_, Some(detail)) = &landed[0].lane else { panic!("detail") };
            assert_eq!(detail.video_fps.to_bits(), (-0.0f64).to_bits());
            assert_eq!(detail.aspect_ratio.to_bits(), 0x7ff8000000000013);
            assert_eq!(detail.blur[0][0].to_bits(), 0x7fc00013);
        }
        reset(false);
    }
}
