use crate::code::CODE_ALPHABET;
use crate::json::{read, Json};
use crate::text::plain;
use std::collections::BTreeSet;

pub const PUBLIC_LIST_URL: &str = "https://openwatch.party/relays.json";
pub const EMBEDDED_PUBLIC_LIST: &str = include_str!("../fixtures/relays.json");
pub const MAX_RELAY_ID_LENGTH: usize = 32;
pub const MAX_RELAY_NAME_LENGTH: usize = 64;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Relay {
    pub id: String,
    pub name: String,
    pub url: String,
    pub prefixes: String,
}

fn valid_id(id: &str) -> bool {
    (1..=MAX_RELAY_ID_LENGTH).contains(&id.len())
        && !id.starts_with('-')
        && id.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

pub fn valid_url(url: &str, allow_ws: bool) -> bool {
    let rest = match (url.strip_prefix("wss://"), url.strip_prefix("ws://")) {
        (Some(rest), _) => rest,
        (None, Some(rest)) if allow_ws => rest,
        _ => return false,
    };
    let host_length = rest.find('/').unwrap_or(rest.len());
    host_length > 0 && rest.ends_with("/v1/ws") && rest.bytes().all(|b| (0x21..=0x7E).contains(&b) && b != b'?' && b != b'#')
}

pub fn validate(list: &Json) -> Option<Vec<Relay>> {
    list.members()?;
    let Some(Json::Array(entries)) = list.get("relays") else { return None };
    if entries.is_empty() {
        return None;
    }
    let mut ids = BTreeSet::new();
    let mut symbols = BTreeSet::new();
    let mut relays = Vec::new();
    for entry in entries {
        entry.members()?;
        let id = entry.get("id")?.as_text()?;
        let name = entry.get("name")?.as_text()?;
        let url = entry.get("url")?.as_text()?;
        let prefixes = entry.get("prefixes")?.as_text()?;
        let fresh_symbols = prefixes.value.chars().all(|c| CODE_ALPHABET.contains(c) && symbols.insert(c));
        let valid = !id.unpaired
            && valid_id(&id.value)
            && ids.insert(id.value.clone())
            && plain(name, MAX_RELAY_NAME_LENGTH)
            && !url.unpaired
            && valid_url(&url.value, false)
            && !prefixes.unpaired
            && !prefixes.value.is_empty()
            && fresh_symbols;
        if !valid {
            return None;
        }
        relays.push(Relay { id: id.value.clone(), name: name.value.clone(), url: url.value.clone(), prefixes: prefixes.value.clone() });
    }
    Some(relays)
}

pub fn embedded_public_list() -> Option<Vec<Relay>> {
    validate(&read(EMBEDDED_PUBLIC_LIST).ok()?.value)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_embedded_public_list_is_valid_and_kept_whole() {
        let ids: Vec<_> = embedded_public_list().unwrap().into_iter().map(|relay| relay.id).collect();
        assert_eq!(ids, ["na", "eu", "asia"]);
    }

    #[test]
    fn a_ws_url_passes_only_where_allowed() {
        assert!(valid_url("ws://192.168.1.5:8080/owp/v1/ws", true));
        assert!(!valid_url("ws://192.168.1.5:8080/owp/v1/ws", false));
        assert!(!valid_url("wss:///v1/ws", false));
        assert!(!valid_url("wss://a.example/v1/ws?x", false));
    }

    #[test]
    fn the_v1_ws_ending_comes_after_the_host() {
        assert!(valid_url("wss://h/v1/ws", false));
        assert!(!valid_url("wss://v1/ws", false));
        assert!(!valid_url("ws://v1/ws", true));
        assert!(!valid_url("wss://hv1/ws", false));
    }
}
