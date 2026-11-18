use crate::clock::{ClockEvent, ClockSync, PongOutcome};
use crate::code;
use crate::json::{read, Json, Text, MAX_SAFE_INT};
use crate::media::{self, Direction};
use crate::message::{decode, Incoming, InvalidMessage, Outgoing, PlayerState, Received};
use crate::relays::{self, Relay};
use crate::room::{valid_snapshot, Snapshot, MAX_OFFSET_MS};
use crate::text::{valid_client, valid_name};
use crate::timeline::{Phase, Timeline};
use std::path::PathBuf;

type Runner = fn(&Json) -> Json;

const RUNNERS: [(&str, Runner); 8] = [
    ("names", names),
    ("code-routing", code_routing),
    ("relay-list", relay_list),
    ("media-check", media_check),
    ("same-title", same_title),
    ("room-check", room_check),
    ("clock-sync", clock_sync),
    ("timeline", timeline),
];

fn fixtures() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("fixtures")
}

fn load(path: PathBuf) -> Json {
    let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    read(&text).unwrap_or_else(|e| panic!("{}: {e}", path.display())).value
}

fn string(value: &str) -> Json {
    Json::Str(Text::new(value))
}

fn object(members: Vec<(&str, Json)>) -> Json {
    Json::Object(members.into_iter().map(|(name, value)| (Text::new(name), value)).collect())
}

fn valid(answer: bool) -> Json {
    object(vec![("valid", Json::Bool(answer))])
}

fn same(a: &Json, b: &Json) -> bool {
    match (a, b) {
        (Json::Null, Json::Null) => true,
        (Json::Bool(x), Json::Bool(y)) => x == y,
        (Json::Str(x), Json::Str(y)) => x.value == y.value,
        (Json::Array(x), Json::Array(y)) => x.len() == y.len() && x.iter().zip(y).all(|(p, q)| same(p, q)),
        (Json::Object(x), Json::Object(y)) => x.len() == y.len() && x.iter().all(|(k, v)| y.iter().any(|(k2, v2)| k.value == k2.value && same(v, v2))),
        _ => matches!((a.as_number(), b.as_number()), (Some(p), Some(q)) if p == q),
    }
}

fn field<'a>(input: &'a Json, name: &str) -> &'a Json {
    input.get(name).unwrap_or(&Json::Null)
}

fn names(input: &Json) -> Json {
    let value = field(input, "value");
    valid(match field(input, "field").as_text().map(|t| t.value.as_str()) {
        Some("name") => valid_name(value),
        Some("client") => valid_client(value),
        _ => false,
    })
}

fn code_routing(input: &Json) -> Json {
    let typed = field(input, "code").as_text().filter(|t| !t.unpaired);
    let Some(code) = typed.and_then(|t| code::normalize(&t.value)) else {
        return object(vec![("error", string("invalid_code"))]);
    };
    let relays: Vec<Relay> = match field(input, "relays") {
        Json::Array(entries) => entries
            .iter()
            .map(|entry| Relay {
                id: field(entry, "id").as_text().map(|t| t.value.clone()).unwrap_or_default(),
                name: String::new(),
                url: String::new(),
                prefixes: field(entry, "prefixes").as_text().map(|t| t.value.clone()).unwrap_or_default(),
            })
            .collect(),
        _ => Vec::new(),
    };
    let relay = code::relay_for(&code, &relays).map_or(Json::Null, |relay| string(&relay.id));
    object(vec![("code", string(&code)), ("relay", relay)])
}

fn relay_list(input: &Json) -> Json {
    match relays::validate(field(input, "list")) {
        Some(list) => object(vec![(
            "relays",
            Json::Array(
                list.iter()
                    .map(|r| object(vec![("id", string(&r.id)), ("name", string(&r.name)), ("url", string(&r.url)), ("prefixes", string(&r.prefixes))]))
                    .collect(),
            ),
        )]),
        None => object(vec![("error", string("invalid_list"))]),
    }
}

fn media_check(input: &Json) -> Json {
    let direction = match field(input, "on").as_text().map(|t| t.value.as_str()) {
        Some("send") => Direction::Send,
        _ => Direction::Receive,
    };
    valid(media::check(field(input, "media"), direction))
}

fn same_title(input: &Json) -> Json {
    let (a, b) = (field(input, "a"), field(input, "b"));
    let forward = media::same_title(a, b);
    assert_eq!(forward, media::same_title(b, a), "same-title is symmetric");
    object(vec![("same", Json::Bool(forward))])
}

fn room_check(input: &Json) -> Json {
    valid(valid_snapshot(field(input, "room"), Snapshot::Room))
}

fn clock_sync(input: &Json) -> Json {
    let mut clock = ClockSync::new();
    let mut pongs = Vec::new();
    let events = match field(input, "events") {
        Json::Array(events) => events.as_slice(),
        _ => &[],
    };
    for event in events {
        let at_ms = field(event, "atMs").as_int().unwrap_or(0);
        let mut outcomes = Vec::new();
        if let Some(id) = field(event, "ping").as_int() {
            clock.step(ClockEvent::PingSent { id, at_ms }, &mut outcomes);
            continue;
        }
        let id = field(event, "pong").as_int().unwrap_or(-1);
        match field(event, "serverTime").as_int().filter(|time| *time >= 0) {
            Some(server_time) => clock.step(ClockEvent::PongReceived { id, server_time, at_ms }, &mut outcomes),
            None => outcomes.push(PongOutcome::Ignored),
        }
        for outcome in outcomes {
            let best = clock.best();
            let result = match outcome {
                PongOutcome::Accepted => "accepted",
                PongOutcome::Stepped => "stepped",
                PongOutcome::Ignored => "ignored",
            };
            pongs.push(object(vec![
                ("result", string(result)),
                ("offsetMs", best.map_or(Json::Null, |b| Json::Int(b.offset_ms))),
                ("rttMs", best.map_or(Json::Null, |b| Json::Int(b.rtt_ms))),
            ]));
        }
    }
    object(vec![("pongs", Json::Array(pongs))])
}

fn timeline(input: &Json) -> Json {
    let room = field(input, "room");
    let int = |value: &Json| value.as_int().unwrap_or(0);
    let timeline = Timeline {
        phase: field(room, "phase").as_text().and_then(|t| Phase::parse(&t.value)).unwrap_or(Phase::Idle),
        position_ms: int(field(room, "positionMs")),
        at_ms: int(field(room, "atMs")),
        rate: field(room, "rate").as_number().unwrap_or(1.0),
    };
    let position = timeline.at(int(field(input, "durationMs")), int(field(input, "serverNowMs")));
    object(vec![("positionMs", Json::Int(position.position_ms)), ("running", Json::Bool(position.running))])
}

fn run_table(name: &str, runner: Runner) {
    let table = load(fixtures().join("vectors").join(format!("{name}.json")));
    assert!(matches!(table.get("table"), Some(Json::Str(t)) if t.value == name));
    let Some(Json::Array(cases)) = table.get("cases") else { panic!("{name}: no cases") };
    let failed: Vec<String> = cases
        .iter()
        .filter(|case| !same(&runner(field(case, "input")), field(case, "expect")))
        .map(|case| field(case, "name").as_text().map(|t| t.value.clone()).unwrap_or_default())
        .collect();
    assert!(!cases.is_empty() && failed.is_empty(), "{name}: {} of {} cases fail: {failed:?}", failed.len(), cases.len());
}

#[test]
fn every_vendored_table_has_a_runner() {
    let mut vendored: Vec<String> = std::fs::read_dir(fixtures().join("vectors"))
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().trim_end_matches(".json").to_owned())
        .collect();
    vendored.sort();
    let mut run: Vec<String> = RUNNERS.iter().map(|(name, _)| (*name).to_owned()).collect();
    run.sort();
    assert_eq!(vendored, run);
}

#[test]
fn names_table() {
    run_table("names", names);
}

#[test]
fn code_routing_table() {
    run_table("code-routing", code_routing);
}

#[test]
fn relay_list_table() {
    run_table("relay-list", relay_list);
}

#[test]
fn media_check_table() {
    run_table("media-check", media_check);
}

#[test]
fn same_title_table() {
    run_table("same-title", same_title);
}

#[test]
fn room_check_table() {
    run_table("room-check", room_check);
}

#[test]
fn clock_sync_table() {
    run_table("clock-sync", clock_sync);
}

#[test]
fn timeline_table() {
    run_table("timeline", timeline);
}

fn descriptor_examples(dir: &str) -> Vec<(String, Json)> {
    let mut out = Vec::new();
    for entry in std::fs::read_dir(fixtures().join("examples").join(dir)).unwrap() {
        let name = entry.unwrap().file_name().to_string_lossy().into_owned();
        if name.contains("duplicate") || name.contains("invalid-utf8") {
            continue;
        }
        let document = load(fixtures().join("examples").join(dir).join(&name));
        if name.starts_with("descriptor.") {
            out.push((name, document));
        } else if let Some(descriptor) = document.get("media") {
            out.push((name, descriptor.clone()));
        }
    }
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

#[test]
fn every_valid_descriptor_example_passes_the_send_checks() {
    let examples = descriptor_examples("valid");
    let refused: Vec<_> = examples.iter().filter(|(_, d)| !media::check(d, Direction::Send)).map(|(n, _)| n).collect();
    assert_eq!(examples.len(), 13);
    assert!(refused.is_empty(), "{refused:?}");
}

#[test]
fn every_invalid_descriptor_example_fails_the_send_checks() {
    let examples = descriptor_examples("invalid");
    let accepted: Vec<_> = examples.iter().filter(|(_, d)| media::check(d, Direction::Send)).map(|(n, _)| n).collect();
    assert_eq!(examples.len(), 28);
    assert!(accepted.is_empty(), "{accepted:?}");
}

fn room_with_padding(bytes: usize) -> String {
    format!(
        r#"{{"type":"room","rev":2,"code":"K7MQ2X","epoch":0,"phase":"idle","positionMs":0,"atMs":0,"rate":1,"control":"host","locked":false,"waitingOn":[],"participants":[{{"peerId":"p1","name":"Edde","client":"Example/1.0","host":true,"connected":true,"player":"idle","match":null,"offsetMs":0}}],"pad":"{}"}}"#,
        "x".repeat(bytes)
    )
}

#[test]
fn a_relay_message_of_32768_bytes_and_more_is_read() {
    for size in [32_768, 65_536] {
        let text = room_with_padding(size);
        assert!(text.len() > size);
        assert!(matches!(decode(&text), Received::Message(Incoming::Room { .. })));
    }
}

#[test]
fn ints_go_out_without_a_fraction_even_from_a_double() {
    let media = read(r#"{"kind":"video","title":"Clip","durationMs":1000.0,"year":2.0e3}"#).unwrap().value;
    let sent = Outgoing::SetMedia { media, position_ms: Some(0) }.encode().unwrap();
    assert_eq!(sent, r#"{"type":"setMedia","media":{"kind":"video","title":"Clip","year":2000,"durationMs":1000},"positionMs":0}"#);
    assert_eq!(Outgoing::Seek { position_ms: 61_234 }.encode().unwrap(), r#"{"type":"seek","positionMs":61234}"#);
}

#[test]
fn values_out_of_bounds_are_refused_before_sending() {
    assert_eq!(Outgoing::Seek { position_ms: -1 }.encode(), Err(InvalidMessage("positionMs")));
    assert!(Outgoing::Seek { position_ms: MAX_SAFE_INT + 1 }.encode().is_err());
    assert!(Outgoing::Rate { rate: 2.5 }.encode().is_err());
    assert!(Outgoing::Rate { rate: f64::NAN }.encode().is_err());
    let status = |offset_ms| Outgoing::Status { epoch: 0, player: PlayerState::Idle, report: None, offset_ms, rtt_ms: None };
    assert!(status(MAX_OFFSET_MS).encode().is_ok());
    assert!(status(MAX_OFFSET_MS + 1).encode().is_err());
}

#[test]
fn unpaired_surrogates_are_refused_before_sending() {
    let title = read(r#"{"kind":"video","title":"Clip\ud800","durationMs":1000}"#).unwrap().value;
    assert_eq!(Outgoing::SetMedia { media: title, position_ms: None }.encode(), Err(InvalidMessage("media")));
    let ext = read(r#"{"kind":"video","title":"Clip","durationMs":1000,"ext":{"a.b":{"k\udc00":1}}}"#).unwrap().value;
    assert!(Outgoing::SetMedia { media: ext, position_ms: None }.encode().is_err());
}

#[test]
fn unknown_members_of_a_descriptor_are_not_sent() {
    let media = read(r#"{"kind":"video","title":"Clip","durationMs":1,"rating":5,"file":{"name":"c.mp4","hash":"x"}}"#).unwrap().value;
    let sent = Outgoing::SetMedia { media, position_ms: None }.encode().unwrap();
    assert_eq!(sent, r#"{"type":"setMedia","media":{"kind":"video","title":"Clip","durationMs":1,"file":{"name":"c.mp4"}}}"#);
}

#[test]
fn a_message_over_8192_bytes_is_not_sent() {
    let media = read(&format!(r#"{{"kind":"video","title":"{}","durationMs":1}}"#, "x".repeat(4000))).unwrap().value;
    assert!(Outgoing::SetMedia { media, position_ms: None }.encode().is_ok());
    let padded = Outgoing::Kick { peer_id: "p".repeat(9000) };
    assert_eq!(padded.encode(), Err(InvalidMessage("message size")));
}

#[test]
fn relay_messages_that_fail_their_rules_are_malformed_and_others_ignored() {
    assert!(matches!(decode(r#"{"type":"pong","id":1}"#), Received::Malformed("pong")));
    assert!(matches!(decode(r#"{"type":"error","code":"forbidden","message":"x"}"#), Received::Malformed("error")));
    assert!(matches!(decode(r#"{"type":"closed","reason":""}"#), Received::Malformed("closed")));
    assert!(matches!(decode(r#"{"type":"future"}"#), Received::Ignored));
    assert!(matches!(decode(r#"[1]"#), Received::Ignored));
    assert!(matches!(decode(r#"{"type":"pong","id":"a-17","serverTime":918273001}"#), Received::Message(Incoming::Pong { .. })));
}
