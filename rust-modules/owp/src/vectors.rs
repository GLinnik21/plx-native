use crate::clock::{ClockEvent, ClockSync, PongOutcome};
use crate::code;
use crate::json::{read, Json, Text, MAX_SAFE_INT};
use crate::media::{self, Direction, MediaRule, MAX_EXT_BYTES, MAX_MEDIA_BYTES};
use crate::message::{decode, Incoming, InvalidMessage, Outgoing, PingId, PlayerState, Received, MAX_RELAY_MESSAGE_BYTES};
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
    let typed = field(input, "code").as_text().filter(|t| !t.unpaired).map(|t| t.value.as_str()).unwrap_or_default();
    let Some(code) = code::normalize(typed) else {
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
    let relay = code::relay_for(typed, &relays).map_or(Json::Null, |relay| string(&relay.id));
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
    valid(media::check(field(input, "media"), direction).is_ok())
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
        if name.contains("invalid-utf8") {
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
    let refused: Vec<_> = examples.iter().filter(|(_, d)| media::check(d, Direction::Send).is_err()).map(|(n, _)| n).collect();
    assert_eq!(examples.len(), 13);
    assert!(refused.is_empty(), "{refused:?}");
}

#[test]
fn every_invalid_descriptor_example_fails_the_send_checks() {
    let examples = descriptor_examples("invalid");
    let accepted: Vec<_> = examples.iter().filter(|(_, d)| media::check(d, Direction::Send).is_ok()).map(|(n, _)| n).collect();
    assert_eq!(examples.len(), 31);
    assert!(accepted.is_empty(), "{accepted:?}");
}

fn room_with_padding(bytes: usize) -> String {
    format!(
        r#"{{"type":"room","rev":2,"code":"K7MQ2X","epoch":0,"phase":"idle","positionMs":0,"atMs":0,"rate":1,"control":"host","locked":false,"waitingOn":[],"participants":[{{"peerId":"p1","name":"Edde","client":"Example/1.0","host":true,"connected":true,"player":"idle","match":null,"offsetMs":0}}],"pad":"{}"}}"#,
        "x".repeat(bytes)
    )
}

fn room_of_length(length: usize) -> String {
    let text = room_with_padding(length - room_with_padding(0).len());
    assert_eq!(text.len(), length);
    text
}

#[test]
fn a_relay_message_of_32768_bytes_and_more_is_read() {
    for length in [32_768, 40_000, MAX_RELAY_MESSAGE_BYTES] {
        assert!(matches!(decode(&room_of_length(length)), Received::Message(Incoming::Room { .. })), "{length}");
    }
}

#[test]
fn a_relay_message_over_the_cap_is_refused_before_the_scan() {
    assert_eq!(decode(&room_of_length(MAX_RELAY_MESSAGE_BYTES + 1)), Received::TooLarge);
    assert_eq!(decode(&"x".repeat(MAX_RELAY_MESSAGE_BYTES + 1)), Received::TooLarge);
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
    let padded = Outgoing::Ping { id: PingId::Text("p".repeat(9000)) };
    assert_eq!(padded.encode(), Err(InvalidMessage("message size")));
}

#[test]
fn a_descriptor_that_repeats_a_member_name_is_not_sent() {
    let ext = read(r#"{"kind":"movie","title":"Arrival","durationMs":6960000,"ext":{"com.example":{"server":"1a2b3c","server":"9z8y7x"}}}"#).unwrap().value;
    assert_eq!(Outgoing::SetMedia { media: ext.clone(), position_ms: None }.encode(), Err(InvalidMessage("media")));
    assert_eq!(media::check(&ext, Direction::Receive), Ok(()));
    let ids = read(r#"{"kind":"movie","title":"Heat","durationMs":1,"ids":{"imdb":"tt1","\u0069mdb":"tt2"}}"#).unwrap().value;
    assert_eq!(media::check(&ids, Direction::Send), Err(MediaRule::RepeatedName));
}

const EPISODE: &str = r#"{"kind":"episode","title":"Pilot","year":2004,"ids":{"tvdb":"127131"},"series":{"title":"Lost","year":2004,"ids":{"tvdb":"73739"}},"season":1,"episode":1,"durationMs":2520000,"file":{"sizeBytes":1,"oshash":"8e245d9679d31e12","name":"Lost.S01E01.mkv"},"ext":{"com.example":{"item":"4711"}}}"#;

fn set(json: &mut Json, path: &[&str], value: Json) {
    let (Json::Object(members), Some((name, rest))) = (json, path.split_first()) else { panic!("no object at {path:?}") };
    match (members.iter_mut().find(|(key, _)| key.value == *name), rest.is_empty()) {
        (Some((_, member)), true) => *member = value,
        (Some((_, member)), false) => set(member, rest, value),
        (None, true) => members.push((Text { value: (*name).into(), unpaired: false }, value)),
        (None, false) => panic!("no member {name}"),
    }
}

fn checked(document: &str) -> [Result<(), MediaRule>; 2] {
    let media = read(document).unwrap().value;
    [media::check(&media, Direction::Receive), media::check(&media, Direction::Send)]
}

#[test]
fn a_descriptor_that_fails_names_the_rule_it_breaks() {
    let cases: [(&[&str], &str, MediaRule); 16] = [
        (&["kind"], r#""show""#, MediaRule::Kind),
        (&["title"], r#""""#, MediaRule::Title),
        (&["year"], "1.5", MediaRule::Year),
        (&["ids", "tvdb"], r#""""#, MediaRule::Ids),
        (&["series"], "null", MediaRule::Series),
        (&["series", "title"], "7", MediaRule::SeriesTitle),
        (&["series", "year"], r#""2004""#, MediaRule::SeriesYear),
        (&["series", "ids"], "[]", MediaRule::SeriesIds),
        (&["season"], "-1", MediaRule::Season),
        (&["episode"], "-1", MediaRule::Episode),
        (&["durationMs"], "0", MediaRule::DurationMs),
        (&["file"], r#""Lost.mkv""#, MediaRule::File),
        (&["file", "sizeBytes"], "-1", MediaRule::FileSizeBytes),
        (&["file", "oshash"], r#""8E245D9679D31E12""#, MediaRule::FileOshash),
        (&["file", "name"], r#""..""#, MediaRule::FileName),
        (&["ext", "example"], "1", MediaRule::Ext),
    ];
    assert_eq!(checked(EPISODE), [Ok(()), Ok(())]);
    for (path, value, rule) in cases {
        let mut media = read(EPISODE).unwrap().value;
        set(&mut media, path, read(value).unwrap().value);
        let broken = [media::check(&media, Direction::Receive), media::check(&media, Direction::Send)];
        assert_eq!(broken, [Err(rule), Err(rule)], "{path:?} = {value}");
    }
    assert_eq!(checked("[]"), [Err(MediaRule::Object), Err(MediaRule::Object)]);
    assert_eq!(checked(r#"{"kind":"episode","title":"Pilot","durationMs":1}"#), [Err(MediaRule::Series), Err(MediaRule::Series)]);
}

#[test]
fn a_descriptor_that_breaks_several_rules_names_the_first() {
    assert_eq!(checked(r#"{"kind":"show","title":"","durationMs":0}"#)[1], Err(MediaRule::Kind));
    assert_eq!(checked(r#"{"kind":"movie","title":"","durationMs":0}"#)[1], Err(MediaRule::Title));
    assert_eq!(checked(r#"{"kind":"movie","title":"Heat","durationMs":0,"file":7}"#)[1], Err(MediaRule::DurationMs));
}

#[test]
fn the_size_rules_apply_only_to_sending() {
    let ext = "x".repeat(MAX_EXT_BYTES);
    let title = "x".repeat(MAX_MEDIA_BYTES);
    let long_ext = format!(r#"{{"kind":"video","title":"Clip","durationMs":1,"ext":{{"a.b":"{ext}"}}}}"#);
    let long_title = format!(r#"{{"kind":"video","title":"{title}","durationMs":1}}"#);
    let both = format!(r#"{{"kind":"video","title":"{title}","durationMs":1,"ext":{{"a.b":"{ext}"}}}}"#);
    assert_eq!(checked(&long_ext), [Ok(()), Err(MediaRule::ExtSize)]);
    assert_eq!(checked(&long_title), [Ok(()), Err(MediaRule::Size)]);
    assert_eq!(checked(&both), [Ok(()), Err(MediaRule::ExtSize)]);
}

#[test]
fn a_number_json_cannot_carry_in_ext_is_not_sent() {
    let infinite = r#"{"kind":"video","title":"Clip","durationMs":1,"ext":{"a.b":{"n":[1e400]}}}"#;
    assert_eq!(checked(infinite), [Ok(()), Err(MediaRule::ExtNumber)]);
    assert_eq!(checked(r#"{"kind":"video","title":"Clip","durationMs":1,"ext":{"a.b":-1e400}}"#), [Ok(()), Err(MediaRule::ExtNumber)]);
    assert_eq!(checked(r#"{"kind":"video","title":"Clip","durationMs":1,"ext":{"a.b":{"n":1e400,"n":1}}}"#)[1], Err(MediaRule::RepeatedName));
    let ext = "x".repeat(MAX_EXT_BYTES);
    let long = format!(r#"{{"kind":"video","title":"Clip","durationMs":1,"ext":{{"a.b":[1e400,"{ext}"]}}}}"#);
    assert_eq!(checked(&long)[1], Err(MediaRule::ExtNumber));
    let media = read(infinite).unwrap().value;
    assert_eq!(Outgoing::SetMedia { media, position_ms: None }.encode(), Err(InvalidMessage("media")));
}

const JOINED: &str = r#"{"type":"joined","code":"D4XN7P","you":{"peerId":"p1","token":"q3Jm8f0lYc2Kx9T4bWz7RuNs6dVhA1eGpLo5iCyZk0E"},"room":{"rev":1,"code":"D4XN7P","epoch":0,"phase":"idle","positionMs":0,"atMs":15023,"rate":1,"control":"host","locked":false,"waitingOn":[],"participants":[{"peerId":"p1","name":"Edde","client":"Example/1.31.0","host":true,"connected":true,"player":"idle","match":null,"offsetMs":0}],"action":{"kind":"join","by":"p1"}},"media":null,"serverTime":15023}"#;

#[test]
fn an_action_in_a_joined_room_is_an_unknown_member() {
    assert!(matches!(decode(JOINED), Received::Message(Incoming::Joined { .. })));
}

#[test]
fn a_joined_descriptor_keeps_only_its_known_members() {
    let media = r#"{"futureField":1,"kind":"episode","title":"Pilot","ids":{"tvdb":"127131","trakt":"4"},"series":{"futureField":"x","title":"Lost","ids":{"tvdb":"73739","trakt":"5"}},"season":1,"episode":1,"durationMs":2520000,"file":{"sizeBytes":1,"futureField":{"x":1}},"ext":{"com.example":{"future":[1],"x":{"y":null}}}}"#;
    let known = r#"{"kind":"episode","title":"Pilot","ids":{"tvdb":"127131","trakt":"4"},"series":{"title":"Lost","ids":{"tvdb":"73739","trakt":"5"}},"season":1,"episode":1,"durationMs":2520000,"file":{"sizeBytes":1},"ext":{"com.example":{"future":[1],"x":{"y":null}}}}"#;
    let joined = JOINED.replace(r#""media":null"#, &format!(r#""media":{media}"#));
    let Received::Message(Incoming::Joined { media: Some(kept), .. }) = decode(&joined) else { panic!("not a joined with media: {joined}") };
    assert_eq!(kept, read(known).unwrap().value);
}

#[test]
fn a_peer_id_or_token_off_its_pattern_is_not_sent() {
    let token = "q3Jm8f0lYc2Kx9T4bWz7RuNs6dVhA1eGpLo5iCyZk0E";
    let resume = |peer_id: &str, token: &str| Outgoing::Resume { code: "D4XN7P".into(), peer_id: peer_id.into(), token: token.into() }.encode();
    assert!(resume("p_1-A", token).is_ok());
    assert_eq!(resume("", token), Err(InvalidMessage("peerId")));
    assert_eq!(resume("p 1", token), Err(InvalidMessage("peerId")));
    assert_eq!(resume("p1", &token[1..]), Err(InvalidMessage("token")));
    assert_eq!(resume("p1", &format!("{}!", &token[1..])), Err(InvalidMessage("token")));
    assert_eq!(Outgoing::TransferHost { peer_id: "p".repeat(17) }.encode(), Err(InvalidMessage("peerId")));
    assert_eq!(Outgoing::Kick { peer_id: "p/1".into() }.encode(), Err(InvalidMessage("peerId")));
    assert!(Outgoing::Kick { peer_id: "p".repeat(16) }.encode().is_ok());
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

fn room_with(member: &str) -> String {
    room_with_padding(0).replace(r#""pad":"""#, member)
}

#[test]
fn unknown_members_are_ignored_at_every_depth() {
    let pong = r#"{"type":"pong","id":1,"serverTime":5,"future":{"a":[1,2]}}"#;
    assert_eq!(decode(pong), Received::Message(Incoming::Pong { id: PingId::Number(1), server_time: 5 }));
    let room = room_with_padding(0).replace(r#""offsetMs":0}"#, r#""offsetMs":0,"badge":"gold"}"#);
    assert!(matches!(decode(&room), Received::Message(Incoming::Room { .. })));
}

#[test]
fn escaped_names_and_values_read_as_their_text() {
    let pong = r#"{"type":"pong","id":"a\"b\\c","serverTime":5}"#;
    assert_eq!(decode(pong), Received::Message(Incoming::Pong { id: PingId::Text(r#"a"b\c"#.into()), server_time: 5 }));
}

#[test]
fn a_lone_surrogate_fails_a_checked_field_and_not_an_unknown_one() {
    assert_eq!(decode(&room_with_padding(0).replace(r#""peerId":"p1""#, r#""peerId":"p\ud800""#)), Received::Malformed("room"));
    assert!(matches!(decode(r#"{"type":"pong","id":1,"serverTime":5,"note":"\ud800"}"#), Received::Message(Incoming::Pong { .. })));
}

#[test]
fn error_closed_and_action_kind_keep_a_lone_surrogate_as_a_replacement() {
    let error = r#"{"type":"error","code":"busy\ud800","message":"\udc00","re":"join\ud83d"}"#;
    let replaced = Incoming::Error { code: "busy\u{fffd}".into(), message: "\u{fffd}".into(), re: Some("join\u{fffd}".into()) };
    assert_eq!(decode(error), Received::Message(replaced));
    assert_eq!(decode(r#"{"type":"closed","reason":"kicked\ud800"}"#), Received::Message(Incoming::Closed { reason: "kicked\u{fffd}".into() }));
    let room = room_with(r#""action":{"kind":"\ud800","by":"p1"}"#);
    assert!(matches!(decode(&room), Received::Message(Incoming::Room { .. })));
}

#[test]
fn a_relay_message_that_repeats_a_member_name_is_malformed_under_its_type() {
    assert_eq!(decode(r#"{"type":"pong","id":1,"id":2,"serverTime":5}"#), Received::Malformed("pong"));
    assert_eq!(decode(&room_with_padding(0).replace(r#""locked":false"#, r#""locked":false,"locked":true"#)), Received::Malformed("room"));
    let ext = room_with(r#""media":{"kind":"video","title":"Clip","durationMs":1,"ext":{"a.b":{"k":1,"k":2}}}"#);
    assert_eq!(decode(&ext), Received::Malformed("room"));
    let joined = r#"{"type":"joined","code":"D4XN7P","code":"D4XN7P","you":{"peerId":"p1","token":"q3Jm8f0lYc2Kx9T4bWz7RuNs6dVhA1eGpLo5iCyZk0E"},"media":null,"serverTime":1}"#;
    assert_eq!(decode(joined), Received::Malformed("joined"));
}

#[test]
fn a_repeated_name_in_an_unknown_type_or_a_non_object_is_ignored() {
    assert_eq!(decode(r#"{"type":"future","x":1,"x":2}"#), Received::Ignored);
    assert_eq!(decode(r#"[{"a":1,"a":2}]"#), Received::Ignored);
    assert_eq!(decode(r#"{"x":1,"x":2}"#), Received::Ignored);
}

#[test]
fn numbers_past_their_range_fail_their_field() {
    assert_eq!(decode(r#"{"type":"pong","id":1,"serverTime":1e400}"#), Received::Malformed("pong"));
    assert_eq!(decode(r#"{"type":"pong","id":1,"serverTime":9007199254740992}"#), Received::Malformed("pong"));
    assert_eq!(decode(&room_with_padding(0).replace(r#""rev":2"#, r#""rev":1e400"#)), Received::Malformed("room"));
    assert_eq!(decode(&room_with_padding(0).replace(r#""rev":2"#, r#""rev":9007199254740992"#)), Received::Malformed("room"));
    let ext = room_with(r#""media":{"kind":"video","title":"Clip","durationMs":1,"ext":{"a.b":{"n":1e400}}}"#);
    assert!(matches!(decode(&ext), Received::Message(Incoming::Room { .. })));
}

#[test]
fn a_deep_ext_reads_up_to_the_recursion_limit() {
    let media = |depth: usize| {
        let nested = format!("{}1{}", r#"{"a":"#.repeat(depth), "}".repeat(depth));
        room_with(&format!(r#""media":{{"kind":"video","title":"Clip","durationMs":1,"ext":{{"a.b":{nested}}}}}"#))
    };
    assert!(matches!(decode(&media(100)), Received::Message(Incoming::Room { .. })));
    assert_eq!(decode(&media(5_000)), Received::Ignored);
}

#[test]
fn unknown_error_codes_closed_reasons_and_action_kinds_stay_strings() {
    let error = r#"{"type":"error","code":"queue_full","message":"later","re":"join"}"#;
    assert_eq!(decode(error), Received::Message(Incoming::Error { code: "queue_full".into(), message: "later".into(), re: Some("join".into()) }));
    assert_eq!(decode(r#"{"type":"closed","reason":"maintenance"}"#), Received::Message(Incoming::Closed { reason: "maintenance".into() }));
    let text = room_with(r#""action":{"kind":"confetti","by":"p1"}"#);
    let Received::Message(Incoming::Room { room, .. }) = decode(&text) else { panic!("{text}") };
    let kind = room.get("action").and_then(|action| action.get("kind")).and_then(Json::as_text).map(|kind| kind.value.as_str());
    assert_eq!(kind, Some("confetti"));
}
