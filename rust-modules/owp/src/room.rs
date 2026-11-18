use crate::code::is_room_code;
use crate::json::Json;
use crate::media::MAX_DURATION_MS;
use crate::text::{valid_client, valid_name};
use std::collections::BTreeSet;

pub const MAX_OFFSET_MS: i64 = 604_800_000;
pub const MAX_PEER_ID_LENGTH: usize = 16;
pub const MIN_RATE: f64 = 0.5;
pub const MAX_RATE: f64 = 2.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Snapshot {
    Room,
    Joined,
}

pub fn is_peer_id(value: &Json) -> bool {
    value.as_text().is_some_and(|text| {
        (1..=MAX_PEER_ID_LENGTH).contains(&text.value.len()) && text.value.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
    })
}

fn one_of(value: Option<&Json>, allowed: &[&str]) -> bool {
    value.and_then(Json::as_text).is_some_and(|text| allowed.contains(&text.value.as_str()))
}

fn boolean(value: Option<&Json>) -> bool {
    matches!(value, Some(Json::Bool(_)))
}

fn valid_match(value: Option<&Json>) -> bool {
    match value {
        Some(Json::Null) => true,
        Some(report @ Json::Object(_)) => {
            one_of(report.get("state"), &["searching", "matched", "mismatched", "unavailable"]) && report.get("deltaMs").is_none_or(|d| d.as_int().is_some())
        }
        _ => false,
    }
}

fn valid_participant(entry: &Json) -> bool {
    entry.members().is_some()
        && entry.get("peerId").is_some_and(is_peer_id)
        && entry.get("name").is_some_and(valid_name)
        && entry.get("client").is_some_and(valid_client)
        && boolean(entry.get("host"))
        && boolean(entry.get("connected"))
        && one_of(entry.get("player"), &["idle", "loading", "ready", "buffering"])
        && valid_match(entry.get("match"))
        && entry.get("offsetMs").and_then(Json::as_int).is_some_and(|ms| ms.abs() <= MAX_OFFSET_MS)
}

fn valid_action(action: &Json) -> bool {
    action.members().is_some()
        && action.get("kind").and_then(Json::as_text).is_some_and(|text| !text.value.is_empty())
        && (matches!(action.get("by"), Some(Json::Null)) || action.get("by").is_some_and(is_peer_id))
}

fn valid_waiting_on(room: &Json, waiting: bool) -> bool {
    let Some(Json::Array(items)) = room.get("waitingOn") else { return false };
    let mut seen = BTreeSet::new();
    items.iter().all(|item| is_peer_id(item) && item.as_text().is_some_and(|text| seen.insert(text.value.clone()))) && (items.is_empty() || waiting)
}

fn valid_participants(room: &Json) -> bool {
    let Some(Json::Array(items)) = room.get("participants") else { return false };
    !items.is_empty() && items.iter().all(valid_participant) && items.iter().filter(|p| matches!(p.get("host"), Some(Json::Bool(true)))).count() == 1
}

pub fn valid_snapshot(room: &Json, snapshot: Snapshot) -> bool {
    let phase = room.get("phase").and_then(Json::as_text).map(|text| text.value.as_str());
    let action = match (room.get("action"), snapshot) {
        (Some(action), Snapshot::Room) => valid_action(action),
        (None, _) | (Some(_), Snapshot::Joined) => true,
    };
    room.members().is_some()
        && room.get("rev").and_then(Json::as_int).is_some_and(|rev| rev >= 1)
        && room.get("code").and_then(Json::as_text).is_some_and(|code| !code.unpaired && is_room_code(&code.value))
        && room.get("epoch").and_then(Json::as_int).is_some_and(|epoch| epoch >= 0)
        && room.get("atMs").and_then(Json::as_int).is_some_and(|at| at >= 0)
        && matches!(phase, Some("idle" | "paused" | "waiting" | "playing"))
        && room.get("positionMs").and_then(Json::as_int).is_some_and(|ms| (0..=MAX_DURATION_MS).contains(&ms) && (phase != Some("idle") || ms == 0))
        && room.get("rate").and_then(Json::as_number).is_some_and(|rate| (MIN_RATE..=MAX_RATE).contains(&rate))
        && one_of(room.get("control"), &["host", "everyone"])
        && boolean(room.get("locked"))
        && valid_waiting_on(room, phase == Some("waiting"))
        && action
        && valid_participants(room)
}
