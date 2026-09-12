//! Resource boundary for the controlled content domain.
use serde_json::{json, Value};
use std::cell::RefCell;
use std::collections::{BTreeMap, VecDeque};

#[derive(Default)]
struct Tape {
    active: bool,
    replay: bool,
    credits: Option<u32>,
    admissions: VecDeque<Value>,
    supplied: VecDeque<Value>,
    requests: Vec<Value>,
    results: Vec<Value>,
    failure: Option<&'static str>,
    // The legacy mailbox can discard superseded answers. Bound retained admission evidence;
    // exceeding this controlled-domain budget fails the run instead of accepting unbound mail.
    person: BTreeMap<(u32, u32), u32>,
}
thread_local! { static TAPE: RefCell<Tape> = RefCell::new(Tape::default()); }

pub(crate) fn init(initial: &super::Initial, replay: bool) {
    TAPE.with(|t| *t.borrow_mut() = Tape { active: true, replay,
        credits: initial.content.as_ref().map(|v| v.personcredits), ..Default::default() });
    crate::metadata::record::reset(initial.content.is_some());
}
pub(crate) fn active() -> bool { TAPE.with(|t| t.borrow().active) }
pub(crate) fn replaying() -> bool { TAPE.with(|t| t.borrow().active && t.borrow().replay) }
pub(crate) fn credits() -> Option<u32> { TAPE.with(|t| t.borrow().credits) }
pub(crate) fn begin(admissions: VecDeque<Value>, results: VecDeque<Value>) {
    TAPE.with(|t| {
        let mut t = t.borrow_mut();
        if !t.admissions.is_empty() || !t.supplied.is_empty() {
            t.failure = Some("unconsumed content resource input");
        }
        t.admissions = admissions; t.supplied = results;
    });
}
pub(crate) fn admit(request: Value, launch: impl FnOnce() -> bool) -> bool {
    if !active() { return launch(); }
    let replay = TAPE.with(|t| t.borrow().replay);
    let answer = if replay {
        TAPE.with(|t| {
            let mut t = t.borrow_mut();
            let Some(value) = t.admissions.pop_front() else {
                t.failure = Some("missing content resource admission"); return false;
            };
            if value["request"] != request || !value["admitted"].is_boolean() {
                t.failure = Some("mismatched content resource admission"); return false;
            }
            value["admitted"].as_bool().unwrap()
        })
    } else { launch() };
    TAPE.with(|t| {
        let mut t = t.borrow_mut();
        if answer && request["store"] == "person" {
            let key = request["slot"].as_u64().and_then(|v| u32::try_from(v).ok())
                .zip(request["gen"].as_u64().and_then(|v| u32::try_from(v).ok()));
            if let Some(key) = key {
                if t.person.values().copied().sum::<u32>() >= 4 * (crate::plex::MAX_SERVERS * 3 + 2) as u32 {
                    t.failure = Some("person admission evidence capacity exceeded");
                } else { *t.person.entry(key).or_default() += 1; }
            } else { t.failure = Some("invalid person admission binding"); }
        }
        t.requests.push(json!({"content_resource":true, "request":request, "admitted":answer}));
    });
    answer
}

/// Synchronous cancellation observations belong in the ordered resource-effect stream. Empty
/// boundaries carry no bytes; their ordinal still advances so a retirement cannot slide to a
/// different cancellation in the same frame. The callback/landing lock never runs under TAPE.
pub(crate) fn detail_cancellation(boundary: u64, retired: Value) -> Result<Value, &'static str> {
    if !active() { return Ok(retired); }
    TAPE.with(|t| {
        let mut t = t.borrow_mut();
        let value = if t.replay {
            let Some(front) = t.admissions.front() else { return Ok(json!([])); };
            if front["request"]["store"] != "metadata-cancel" { return Ok(json!([])); }
            let recorded = front["request"]["boundary"].as_u64().ok_or("invalid detail cancellation boundary")?;
            if recorded > boundary { return Ok(json!([])); }
            if recorded != boundary { return Err("mismatched detail cancellation boundary"); }
            let value = t.admissions.pop_front().unwrap();
            validate_admission(&value, 0)?;
            value
        } else {
            if retired.as_array().is_some_and(Vec::is_empty) { return Ok(retired); }
            json!({"content_resource":true,"request":{"store":"metadata-cancel",
                "boundary":boundary,"retired":retired},"admitted":true})
        };
        let retired = value["request"]["retired"].clone();
        t.requests.push(value);
        Ok(retired)
    })
}

fn person_completion(t: &mut Tape, slot: u32, data: &Value) -> Result<(), &'static str> {
    crate::person::validate_record(slot, data)?;
    let gen = data["gen"].as_u64().and_then(|v| u32::try_from(v).ok())
        .ok_or("invalid person reply generation")?;
    let key = (slot, gen);
    let count = t.person.get_mut(&key).ok_or("unadmitted person terminal")?;
    *count -= 1;
    if *count == 0 { t.person.remove(&key); }
    Ok(())
}
/// Called at the original consumer, including its empty-poll path. Replay never calls take.
pub(crate) fn poll<T: serde::Serialize + serde::de::DeserializeOwned>(
    store: &str, slot: u32, take: impl FnOnce() -> Option<T>) -> Option<T> {
    if !active() { return take(); }
    let replay = TAPE.with(|t| t.borrow().replay);
    let answer = if replay {
        TAPE.with(|t| {
            let mut t = t.borrow_mut();
            if t.supplied.front().is_none_or(|v| v["store"] != store || v["slot"] != slot) {
                return None;
            }
            let value = t.supplied.pop_front().unwrap();
            match serde_json::from_value(value["data"].clone()) {
                Ok(v) => Some(v),
                Err(_) => { t.failure = Some("invalid content resource result"); None }
            }
        })
    } else { take() };
    if let Some(answer) = &answer {
        match serde_json::to_value(answer) {
            Ok(data) => {
                let accepted = TAPE.with(|t| {
                    let mut t = t.borrow_mut();
                    if store == "person" {
                        if let Err(reason) = person_completion(&mut t, slot, &data) {
                            t.failure = Some(reason);
                            return false;
                        }
                    }
                    t.results.push(json!({"kind":"content", "store":store, "slot":slot, "data":data}));
                    true
                });
                if !accepted { return None; }
            }
            Err(_) => TAPE.with(|t| t.borrow_mut().failure = Some("invalid content result encoding")),
        }
    }
    answer
}
pub(crate) fn poll_apply<T: serde::Serialize + serde::de::DeserializeOwned, O>(
    store: &str, slot: u32, take: impl FnOnce() -> Option<(T, O)>,
    apply: impl FnOnce(T) -> Result<(T, O), &'static str>,
) -> Option<O> {
    if !active() { return take().map(|(_, output)| output); }
    let replay = TAPE.with(|t| t.borrow().replay);
    let answer = if replay {
        TAPE.with(|t| {
            let mut t = t.borrow_mut();
            if t.supplied.front().is_none_or(|v| v["store"] != store || v["slot"] != slot) {
                return None;
            }
            let value = t.supplied.pop_front().unwrap();
            let decoded = match serde_json::from_value(value["data"].clone()) {
                Ok(value) => value,
                Err(_) => { t.failure = Some("invalid content resource result"); return None; }
            };
            match apply(decoded) {
                Ok(answer) => Some(answer),
                Err(reason) => { t.failure = Some(reason); None }
            }
        })
    } else { take() };
    let Some((observed, output)) = answer else { return None };
    match serde_json::to_value(observed) {
        Ok(data) => TAPE.with(|t| t.borrow_mut().results.push(
            json!({"kind":"content", "store":store, "slot":slot, "data":data}))),
        Err(_) => TAPE.with(|t| t.borrow_mut().failure = Some("invalid content result encoding")),
    }
    Some(output)
}
pub(crate) fn fail(reason: &'static str) {
    if active() { TAPE.with(|t| t.borrow_mut().failure = Some(reason)); }
}
pub(crate) fn take_results() -> Vec<Value> { TAPE.with(|t| std::mem::take(&mut t.borrow_mut().results)) }
pub(crate) fn validate_result(value: &Value) -> Result<(crate::stores::StoreId, u32), &'static str> {
    let slot = value["slot"].as_u64().and_then(|n| u32::try_from(n).ok()).ok_or("invalid content slot")?;
    if value.as_object().is_none_or(|o| o.len() != 4) || value["kind"] != "content" {
        return Err("invalid content result");
    }
    let store = match value["store"].as_str() {
        Some("metadata") if slot == 0 => {
            crate::metadata::record::validate(&value["data"])?;
            crate::stores::StoreId::Metadata
        }
        Some("person") => {
            crate::person::validate_record(slot, &value["data"])?;
            crate::stores::StoreId::Person
        }
        _ => return Err("unsupported content result"),
    };
    Ok((store, slot))
}
pub(crate) fn validate_admission(value: &Value, client: u32) -> Result<(), &'static str> {
    fn keys(value: &Value, keys: &[&str]) -> bool {
        value.as_object().is_some_and(|v| v.len() == keys.len() && keys.iter().all(|k| v.contains_key(*k)))
    }
    if !keys(value, &["content_resource","request","admitted"])
        || value["content_resource"] != true || !value["admitted"].is_boolean() {
        return Err("invalid content admission");
    }
    let r = &value["request"];
    if r["store"] == "metadata-cancel" {
        if !keys(r, &["store","boundary","retired"])
            || !r["boundary"].as_u64().is_some_and(|v| v > 0) || value["admitted"] != true {
            return Err("invalid detail cancellation boundary");
        }
        return crate::metadata::record::validate_retired(&r["retired"]);
    }
    if r["gen"].as_u64().and_then(|n| u32::try_from(n).ok()).is_none_or(|n| n == 0) {
        return Err("invalid content generation");
    }
    match r["store"].as_str() {
        Some("metadata") if keys(r, &["store","sid","rk","gen","client"])
            && r["sid"] == 0 && r["client"] == client && r["rk"].as_str().is_some_and(|s| !s.is_empty()) => {}
        Some("person") => {
            let slot = r["slot"].as_u64().ok_or("invalid person admission slot")?;
            if slot >= (crate::plex::MAX_SERVERS * 3 + 2) as u64
                || !r["guid"].is_string() || r["arg"].as_array().is_none_or(|v|
                    v.is_empty() || v.iter().any(|v| !v.is_string())) {
                return Err("invalid person admission");
            }
            let global = slot >= (crate::plex::MAX_SERVERS * 3) as u64;
            if global {
                if !keys(r, &["store","slot","gen","arg","guid"]) { return Err("invalid global person admission"); }
            } else if !keys(r, &["store","slot","gen","arg","guid","local","client","sid"])
                || r["sid"] != 0 || slot >= 3 || r["client"] != client || !r["local"].is_string() {
                return Err("invalid local person admission");
            }
        }
        _ => return Err("unsupported content admission"),
    }
    Ok(())
}
pub(crate) fn finish() -> (Vec<Value>, Option<&'static str>) {
    TAPE.with(|t| {
        let mut t = t.borrow_mut();
        if !t.admissions.is_empty() || !t.supplied.is_empty() {
            t.failure = Some("unconsumed content resource input");
        }
        (std::mem::take(&mut t.requests), t.failure)
    })
}

#[cfg(test)]
pub(crate) fn reset_for_test() {
    TAPE.with(|t| *t.borrow_mut() = Tape::default());
    crate::metadata::record::reset(false);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn p2_person_slot_kinds_are_exhaustive_and_duplicate_mail_is_rejected() {
        let _guard = crate::testlock::serial();
        let last_local = (crate::plex::MAX_SERVERS * 3) as u32;
        for slot in 0..last_local + 2 {
            let expected = if slot == last_local { "Profile" }
                else if slot == last_local + 1 { "Credits" }
                else { ["Resolve", "Media", "Roles"][(slot % 3) as usize] };
            for kind in ["Resolve", "Media", "Roles", "Profile", "Credits"] {
                let data = json!({"gen":7,"what":{kind:null}});
                assert_eq!(crate::person::validate_record(slot, &data).is_ok(), kind == expected,
                    "slot={slot}, kind={kind}");
            }
        }
        let initial = super::super::Initial::synthetic_home(1, 32498, None).unwrap();
        init(&initial, true);
        let slot = last_local + 1;
        let request = json!({"store":"person","slot":slot,"gen":7,"arg":["person"],"guid":"guid"});
        let result = json!({"kind":"content","store":"person","slot":slot,
            "data":{"gen":7,"what":{"Credits":null}}});
        begin([json!({"content_resource":true,"request":request,"admitted":true})].into(),
            [result.clone(), result].into());
        assert!(admit(request, || panic!("replay launched worker")));
        assert!(poll::<Value>("person", slot, || panic!("live mailbox")).is_some());
        assert!(poll::<Value>("person", slot, || panic!("live mailbox")).is_none());
        assert_eq!(take_results().len(), 1);
        assert_eq!(finish().1, Some("unadmitted person terminal"));
        reset_for_test();
    }

    #[test]
    fn p2_person_null_variant_mutation_cannot_be_graded() {
        let _guard = crate::testlock::serial();
        let initial = super::super::Initial::synthetic_home(1, 32498, None).unwrap();
        let slot = (crate::plex::MAX_SERVERS * 3 + 1) as u32;
        let request = json!({"store":"person","slot":slot,"gen":7,"arg":["person"],"guid":"guid"});
        let admission = json!({"content_resource":true,"request":request,"admitted":true});
        for (kind, gen, admitted, valid) in [
            ("Credits", 7, true, true), ("Roles", 7, true, false),
            ("Credits", 8, true, false), ("Credits", 7, false, false),
        ] {
            init(&initial, true);
            let mut answer = admission.clone();
            answer["admitted"] = json!(admitted);
            let data = json!({"gen":gen,"what":{kind:null}});
            begin([answer].into(), [json!({"kind":"content","store":"person","slot":slot,"data":data})].into());
            assert_eq!(admit(request.clone(), || panic!("replay launched worker")), admitted);
            let got = poll::<Value>("person", slot, || panic!("replay took real mailbox"));
            let graded = take_results();
            let failure = finish().1;
            reset_for_test();
            assert_eq!(got.is_some(), valid, "{kind}, gen={gen}, admitted={admitted}");
            assert_eq!(!graded.is_empty(), valid, "invalid terminal must not grade itself");
            assert_eq!(failure.is_none(), valid);
        }
    }
}
