use crate::code::is_room_code;
use crate::json::{read, Json, Text, MAX_SAFE_INT};
use crate::media::{self, Direction};
use crate::room::{is_peer_id, valid_snapshot, Snapshot, MAX_OFFSET_MS, MAX_RATE, MIN_RATE};
use crate::text::{valid_client, valid_name};

pub const MAX_MESSAGE_BYTES: usize = 8192;
pub const MAX_RELAY_MESSAGE_BYTES: usize = 65_536;
const RELAY_TYPES: [&str; 5] = ["joined", "room", "pong", "error", "closed"];
const TOKEN_LENGTH: usize = 43;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PingId {
    Text(String),
    Number(i64),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlayerState {
    Idle,
    Loading,
    Ready,
    Buffering,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MatchState {
    Searching,
    Matched,
    Mismatched,
    Unavailable,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MatchReport {
    pub state: MatchState,
    pub delta_ms: Option<i64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Control {
    Host,
    Everyone,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Outgoing {
    Create { name: String, client: String },
    Join { code: String, name: String, client: String, create: bool },
    Resume { code: String, peer_id: String, token: String },
    Ping { id: PingId },
    Status { epoch: i64, player: PlayerState, report: Option<MatchReport>, offset_ms: i64, rtt_ms: Option<i64> },
    Play,
    Pause { position_ms: Option<i64> },
    Seek { position_ms: i64 },
    Rate { rate: f64 },
    SetMedia { media: Json, position_ms: Option<i64> },
    ClearMedia,
    SetControl { mode: Control },
    TransferHost { peer_id: String },
    Kick { peer_id: String },
    Lock { locked: bool },
    Leave,
    End,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InvalidMessage(pub &'static str);

fn text(value: &str) -> Json {
    Json::Str(Text::new(value))
}

fn int_in(value: i64, min: i64, max: i64, rule: &'static str) -> Result<Json, InvalidMessage> {
    if (min..=max).contains(&value) {
        Ok(Json::Int(value))
    } else {
        Err(InvalidMessage(rule))
    }
}

fn position(value: i64) -> Result<Json, InvalidMessage> {
    int_in(value, 0, MAX_SAFE_INT, "positionMs")
}

fn checked(value: &str, rule: fn(&Json) -> bool, what: &'static str) -> Result<Json, InvalidMessage> {
    let value = text(value);
    if rule(&value) {
        Ok(value)
    } else {
        Err(InvalidMessage(what))
    }
}

fn code(value: &str) -> Result<Json, InvalidMessage> {
    if is_room_code(value) {
        Ok(text(value))
    } else {
        Err(InvalidMessage("code"))
    }
}

impl PlayerState {
    pub fn name(self) -> &'static str {
        match self {
            PlayerState::Idle => "idle",
            PlayerState::Loading => "loading",
            PlayerState::Ready => "ready",
            PlayerState::Buffering => "buffering",
        }
    }
}

impl MatchState {
    pub fn name(self) -> &'static str {
        match self {
            MatchState::Searching => "searching",
            MatchState::Matched => "matched",
            MatchState::Mismatched => "mismatched",
            MatchState::Unavailable => "unavailable",
        }
    }
}

fn report(report: Option<&MatchReport>) -> Result<Json, InvalidMessage> {
    let Some(report) = report else { return Ok(Json::Null) };
    let mut members = vec![(Text::new("state"), text(report.state.name()))];
    if let Some(delta) = report.delta_ms {
        members.push((Text::new("deltaMs"), int_in(delta, -MAX_SAFE_INT, MAX_SAFE_INT, "deltaMs")?));
    }
    Ok(Json::Object(members))
}

impl Outgoing {
    pub fn type_name(&self) -> &'static str {
        match self {
            Outgoing::Create { .. } => "create",
            Outgoing::Join { .. } => "join",
            Outgoing::Resume { .. } => "resume",
            Outgoing::Ping { .. } => "ping",
            Outgoing::Status { .. } => "status",
            Outgoing::Play => "play",
            Outgoing::Pause { .. } => "pause",
            Outgoing::Seek { .. } => "seek",
            Outgoing::Rate { .. } => "rate",
            Outgoing::SetMedia { .. } => "setMedia",
            Outgoing::ClearMedia => "clearMedia",
            Outgoing::SetControl { .. } => "setControl",
            Outgoing::TransferHost { .. } => "transferHost",
            Outgoing::Kick { .. } => "kick",
            Outgoing::Lock { .. } => "lock",
            Outgoing::Leave => "leave",
            Outgoing::End => "end",
        }
    }

    fn members(&self) -> Result<Vec<(&'static str, Json)>, InvalidMessage> {
        Ok(match self {
            Outgoing::Create { name, client } => vec![("name", checked(name, valid_name, "name")?), ("client", checked(client, valid_client, "client")?)],
            Outgoing::Join { code: room, name, client, create } => {
                let mut members = vec![("code", code(room)?), ("name", checked(name, valid_name, "name")?), ("client", checked(client, valid_client, "client")?)];
                if *create {
                    members.push(("create", Json::Bool(true)));
                }
                members
            }
            Outgoing::Resume { code: room, peer_id, token } => {
                vec![("code", code(room)?), ("peerId", checked(peer_id, is_peer_id, "peerId")?), ("token", checked(token, is_token, "token")?)]
            }
            Outgoing::Ping { id: PingId::Text(id) } => vec![("id", text(id))],
            Outgoing::Ping { id: PingId::Number(id) } => vec![("id", int_in(*id, -MAX_SAFE_INT, MAX_SAFE_INT, "id")?)],
            Outgoing::Status { epoch, player, report: match_report, offset_ms, rtt_ms } => {
                let mut members = vec![
                    ("epoch", int_in(*epoch, 0, MAX_SAFE_INT, "epoch")?),
                    ("player", text(player.name())),
                    ("match", report(match_report.as_ref())?),
                    ("offsetMs", int_in(*offset_ms, -MAX_OFFSET_MS, MAX_OFFSET_MS, "offsetMs")?),
                ];
                if let Some(rtt) = rtt_ms {
                    members.push(("rttMs", int_in(*rtt, 0, MAX_SAFE_INT, "rttMs")?));
                }
                members
            }
            Outgoing::Pause { position_ms: Some(at) } => vec![("positionMs", position(*at)?)],
            Outgoing::Seek { position_ms } => vec![("positionMs", position(*position_ms)?)],
            Outgoing::Rate { rate } if (MIN_RATE..=MAX_RATE).contains(rate) => vec![("rate", Json::Float(*rate))],
            Outgoing::Rate { .. } => return Err(InvalidMessage("rate")),
            Outgoing::SetMedia { media: descriptor, position_ms } => {
                if media::check(descriptor, Direction::Send).is_err() {
                    return Err(InvalidMessage("media"));
                }
                let mut members = vec![("media", media::known_members(descriptor))];
                if let Some(at) = position_ms {
                    members.push(("positionMs", position(*at)?));
                }
                members
            }
            Outgoing::SetControl { mode: Control::Host } => vec![("mode", text("host"))],
            Outgoing::SetControl { mode: Control::Everyone } => vec![("mode", text("everyone"))],
            Outgoing::TransferHost { peer_id } | Outgoing::Kick { peer_id } => vec![("peerId", checked(peer_id, is_peer_id, "peerId")?)],
            Outgoing::Lock { locked } => vec![("locked", Json::Bool(*locked))],
            Outgoing::Pause { position_ms: None } | Outgoing::Play | Outgoing::ClearMedia | Outgoing::Leave | Outgoing::End => Vec::new(),
        })
    }

    pub fn encode(&self) -> Result<String, InvalidMessage> {
        let mut members = vec![(Text::new("type"), text(self.type_name()))];
        members.extend(self.members()?.into_iter().map(|(name, value)| (Text::new(name), value)));
        let encoded = Json::Object(members).compact();
        if encoded.len() > MAX_MESSAGE_BYTES {
            return Err(InvalidMessage("message size"));
        }
        Ok(encoded)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Incoming {
    Joined { code: String, peer_id: String, token: String, room: Json, media: Option<Json>, server_time: i64 },
    Room { room: Json, media: Option<Json> },
    Pong { id: PingId, server_time: i64 },
    Error { code: String, message: String, re: Option<String> },
    Closed { reason: String },
}

#[derive(Debug, Clone, PartialEq)]
pub enum Received {
    Message(Incoming),
    Ignored,
    Malformed(&'static str),
    TooLarge,
}

fn plain_string(value: Option<&Json>) -> Option<String> {
    value.and_then(Json::as_text).filter(|text| !text.unpaired).map(|text| text.value.clone())
}

fn any_string(value: Option<&Json>) -> Option<String> {
    value.and_then(Json::as_text).map(|text| text.value.clone())
}

fn non_empty(value: Option<&Json>) -> Option<String> {
    any_string(value).filter(|value| !value.is_empty())
}

fn server_time(message: &Json) -> Option<i64> {
    message.get("serverTime").and_then(Json::as_int).filter(|time| *time >= 0)
}

fn is_token(value: &Json) -> bool {
    value.as_text().is_some_and(|text| text.value.len() == TOKEN_LENGTH && text.value.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-'))
}

fn joined_media(value: &Json) -> Option<Option<Json>> {
    match value {
        Json::Null => Some(None),
        descriptor if media::check(descriptor, Direction::Receive).is_ok() => Some(Some(media::known_members(descriptor))),
        _ => None,
    }
}

fn decode_joined(message: &Json) -> Received {
    let code = plain_string(message.get("code")).filter(|code| is_room_code(code));
    let you = message.get("you");
    let peer_id = you.and_then(|you| you.get("peerId")).filter(|id| is_peer_id(id)).and_then(|id| plain_string(Some(id)));
    let token = you.and_then(|you| you.get("token")).filter(|token| is_token(token)).and_then(|token| plain_string(Some(token)));
    let room = message.get("room").filter(|room| valid_snapshot(room, Snapshot::Joined));
    let media = message.get("media").and_then(joined_media);
    match (code, peer_id, token, room, media, server_time(message)) {
        (Some(code), Some(peer_id), Some(token), Some(room), Some(media), Some(server_time)) => {
            Received::Message(Incoming::Joined { code, peer_id, token, room: room.clone(), media, server_time })
        }
        _ => Received::Malformed("joined"),
    }
}

fn decode_pong(message: &Json) -> Received {
    let id = match message.get("id") {
        Some(Json::Str(id)) if !id.unpaired => Some(PingId::Text(id.value.clone())),
        Some(id) => id.as_int().map(PingId::Number),
        None => None,
    };
    match (id, server_time(message)) {
        (Some(id), Some(server_time)) => Received::Message(Incoming::Pong { id, server_time }),
        _ => Received::Malformed("pong"),
    }
}

fn decode_error(message: &Json) -> Received {
    let re = match message.get("re") {
        Some(Json::Null) => Some(None),
        Some(re) => any_string(Some(re)).map(Some),
        None => None,
    };
    match (non_empty(message.get("code")), any_string(message.get("message")), re) {
        (Some(code), Some(text), Some(re)) => Received::Message(Incoming::Error { code, message: text, re }),
        _ => Received::Malformed("error"),
    }
}

pub fn decode(input: &str) -> Received {
    if input.len() > MAX_RELAY_MESSAGE_BYTES {
        return Received::TooLarge;
    }
    let Ok(read) = read(input) else { return Received::Ignored };
    let message = read.value;
    let Some(kind) = plain_string(message.get("type")) else { return Received::Ignored };
    let Some(&kind) = RELAY_TYPES.iter().find(|known| **known == kind) else { return Received::Ignored };
    if message.repeats_a_member_name() {
        return Received::Malformed(kind);
    }
    match kind {
        "joined" => decode_joined(&message),
        "room" if valid_snapshot(&message, Snapshot::Room) => {
            let media = message.get("media").cloned();
            Received::Message(Incoming::Room { room: message, media })
        }
        "room" => Received::Malformed("room"),
        "pong" => decode_pong(&message),
        "error" => decode_error(&message),
        "closed" => match non_empty(message.get("reason")) {
            Some(reason) => Received::Message(Incoming::Closed { reason }),
            None => Received::Malformed("closed"),
        },
        _ => Received::Ignored,
    }
}
