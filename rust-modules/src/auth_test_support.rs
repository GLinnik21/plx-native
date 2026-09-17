//! Shared fixtures and helpers for the `auth` test modules split out below.

use super::*;
pub(super) use crate::plex::probe::Scheme;
pub(super) use std::cell::RefCell;
pub(super) use std::sync::Mutex;

pub(super) fn resource(json: &str) -> Resource {
    serde_json::from_str(json).expect("fixture parses")
}

/// A share with FOUR advertised addresses, which between them cover every case the probe loop
/// has to get right: the owner's LAN address (policy keeps only its TLS URI), a hostname the
/// transport resolves, and two public IPv4s so "the first one answered as somebody else" has a
/// second one to fall through to. Shaped on the live capture of 2026-08-11
/// (`docs/shared-servers.md` §2); the addresses are stand-ins, the arrangement is not.
pub(super) fn a_share() -> Resource {
    resource(
        r#"{"name":"nas-home","clientIdentifier":"bbbb2222","provides":"server","owned":false,
            "sourceTitle":"friend","publicAddressMatches":false,"httpsRequired":false,
            "accessToken":"tok-share","connections":[
              {"protocol":"https","address":"10.9.9.7","port":32400,
               "uri":"https://172-20-4-7.h.plex.direct:32400","local":true,"relay":false,"IPv6":false},
              {"protocol":"https","address":"media.example.internal","port":31234,
               "uri":"https://media.example.internal:31234","local":false,"relay":false,"IPv6":false},
              {"protocol":"https","address":"198.51.100.7","port":31234,
               "uri":"https://198-51-100-7.h.plex.direct:31234","local":false,"relay":false,"IPv6":false},
              {"protocol":"https","address":"203.0.113.9","port":31234,
               "uri":"https://203-0-113-9.h.plex.direct:31234","local":false,"relay":false,"IPv6":false}]}"#,
    )
}

/// A JSON `/identity` body naming `mid` — what a PMS answers a probe with.
pub(super) fn identity_json(mid: &str) -> Vec<u8> {
    format!(
        r#"{{"MediaContainer":{{"size":0,"machineIdentifier":"{mid}","version":"1.43.3"}}}}"#
    )
    .into_bytes()
}

/// A recording dial. Returns whatever the script says for an address, and remembers the order
/// it was asked — which is how "it stopped" and "it never tried that one" become assertions.
pub(super) struct Dialled {
    seen: RefCell<Vec<String>>,
    answers: Vec<(&'static str, i32, Vec<u8>)>,
}

impl Dialled {
    pub(super) fn new(answers: Vec<(&'static str, i32, Vec<u8>)>) -> Dialled {
        Dialled {
            seen: RefCell::new(Vec::new()),
            answers,
        }
    }
    /// Answers are keyed on the origin's HOST, which is the field that tells the two candidates
    /// of one connection apart: `203-0-113-9.h.plex.direct` is the advertised uri and
    /// `203.0.113.9` is the plaintext twin synthesized from the address behind it. A fixture can
    /// therefore say "the name answers and the address does not" (a reviewer over the internet)
    /// or the reverse (a LAN with no DNS), which is the axis this whole unit turns on.
    ///
    /// `seen` records `Origin::log_form` — the bare authority for plaintext, the whole URL for
    /// TLS — so a probe order that reads plausibly cannot hide which transport each step took.
    pub(super) fn dial(&self, o: &Origin) -> (i32, Vec<u8>) {
        self.seen.borrow_mut().push(o.log_form());
        match self.answers.iter().find(|(h, _, _)| *h == o.host()) {
            Some((s, st, b)) => {
                let _ = s;
                (*st, b.clone())
            }
            None => (0, Vec::new()), // nothing answered at that address
        }
    }
    pub(super) fn seen(&self) -> Vec<String> {
        self.seen.borrow().clone()
    }
}

pub(super) fn race_plan() -> ProbePlan {
    let candidate = |url: &str, address: &str, location: probe::Location| {
        let scheme = if url.starts_with("https://") {
            Scheme::Https
        } else {
            Scheme::Http
        };
        Candidate {
            url: url.into(),
            scheme,
            location,
            address: address.into(),
            port: 32400,
            ipv6: false,
            credential_eligible: scheme == Scheme::Https,
        }
    };
    ProbePlan {
        machine_id: "race-machine".into(),
        token: "race-token".into(),
        owned: true,
        name: "race-server".into(),
        source_title: None,
        candidates: vec![
            candidate(
                "https://192-0-2-10.h.plex.direct:32400",
                "192.0.2.10",
                probe::Location::Local,
            ),
            candidate(
                "https://203-0-113-9.h.plex.direct:32400",
                "203.0.113.9",
                probe::Location::Remote,
            ),
        ],
        policy: CredentialPolicy::HttpsOnly,
    }
}

pub(super) fn test_policy() -> ProbeDeadlines {
    ProbeDeadlines {
        local: Duration::from_secs(1),
        remote: Duration::from_secs(1),
    }
}

pub(super) fn threaded_spawn(_: usize, job: ProbeJob) -> bool {
    std::thread::spawn(job);
    true
}

/// The account this feature exists for, as `/api/v2/resources` really returns it: OUR server
/// (owned, LAN + public + relay) and the SHARE (not owned, the owner's 172.20 LAN, an internal
/// hostname, and one public IPv4). Shaped on the live capture of 2026-08-11
/// (`docs/shared-servers.md` §2) — the addresses are stand-ins, the arrangement is not, and the
/// share is listed FIRST because plex.tv's order is not ours to rely on.
pub(super) fn a_two_server_account() -> Vec<Resource> {
    serde_json::from_str(
        r#"[
          {"name":"nas-home","clientIdentifier":"bbbb2222","provides":"server","owned":false,
           "sourceTitle":"friend","ownerId":987654,"publicAddressMatches":false,
           "httpsRequired":false,"accessToken":"tok-share","connections":[
             {"protocol":"https","address":"10.9.9.7","port":32400,
              "uri":"https://172-20-4-7.h.plex.direct:32400","local":true,"relay":false,"IPv6":false},
             {"protocol":"https","address":"media.example.internal","port":31234,
              "uri":"https://media.example.internal:31234","local":false,"relay":false,"IPv6":false},
             {"protocol":"https","address":"203.0.113.9","port":31234,
              "uri":"https://203-0-113-9.h.plex.direct:31234","local":false,"relay":false,"IPv6":false}]},
          {"name":"Mac mini","clientIdentifier":"aaaa1111","provides":"server","owned":true,
           "sourceTitle":null,"ownerId":null,"publicAddressMatches":false,"httpsRequired":false,
           "accessToken":"tok-own","connections":[
             {"protocol":"https","address":"2001:db8::1","port":32400,
              "uri":"https://2001-db8--1.h.plex.direct:32400","local":true,"relay":false,"IPv6":true},
             {"protocol":"https","address":"192.168.0.10","port":32400,
              "uri":"https://192-168-0-10.h.plex.direct:32400","local":true,"relay":false,"IPv6":false},
             {"protocol":"https","address":"plex-relay.example.net","port":8443,
              "uri":"https://plex-relay.example.net:8443","local":false,"relay":true,"IPv6":false}]},
          {"name":"someone's iPad","clientIdentifier":"cccc3333","provides":"player,controller",
           "accessToken":"tok-pad","connections":[]}
        ]"#,
    )
    .expect("fixture parses")
}

/// A roster entry in the **LEGACY shape** — no stored `origin`, which is what every session
/// file on every television written before that field carries. `..Default::default()` is what
/// leaves it empty, so these fixtures also stand as the compatibility case: everything they
/// assert about registration and re-keying runs through `SourceRef::origin`'s fallback.
// ---- the offline profile seat, decided from the stored session alone ----

pub(super) fn cached_session(protected_pin: Option<&str>) -> Session {
    let mut s = Session {
        client_id: "cid".into(),
        account_token: "acct".into(),
        ..Default::default()
    };
    s.user = UserRef {
        uuid: "u-admin".into(),
        token: "admin-token".into(),
        ..Default::default()
    };
    s.server = ServerRef {
        machine_id: "ours".into(),
        address: "10.0.0.1".into(),
        port: 32400,
        token: "admin-token".into(),
        ..Default::default()
    };
    s.sources = vec![source("ours", true, "admin-token")];
    s.remember_profile(ProfileCreds {
        uuid: "u-admin".into(),
        user: s.user.clone(),
        server: s.server.clone(),
        sources: s.sources.clone(),
        pin: protected_pin.map(session::PinVerifier::new),
        extensions: Default::default(),
    });
    s.remember_profile(ProfileCreds {
        uuid: "u-kid".into(),
        user: UserRef {
            uuid: "u-kid".into(),
            token: "kid-token".into(),
            ..Default::default()
        },
        server: ServerRef {
            machine_id: "ours".into(),
            address: "10.0.0.1".into(),
            port: 32400,
            token: "kid-token".into(),
            ..Default::default()
        },
        sources: vec![source("ours", true, "kid-token")],
        pin: None,
        extensions: Default::default(),
    });
    s
}

pub(super) fn tile(uuid: &str, protected: bool) -> UserTile {
    UserTile {
        uuid: uuid.into(),
        title: uuid.into(),
        protected,
        ..Default::default()
    }
}

pub(super) fn source(machine_id: &str, owned: bool, token: &str) -> SourceRef {
    SourceRef {
        machine_id: machine_id.into(),
        name: machine_id.into(),
        shared_by: if owned {
            String::new()
        } else {
            "friend".into()
        },
        owned,
        address: "10.0.0.1".into(),
        port: 32400,
        token: token.into(),
        ..Default::default()
    }
}

/// The primary in the **LEGACY shape** — see [`source`] above.
pub(super) fn primary(machine_id: &str, address: &str, port: i64, token: &str) -> ServerRef {
    ServerRef {
        name: "Mac mini".into(),
        machine_id: machine_id.into(),
        address: address.into(),
        port,
        token: token.into(),
        ..Default::default()
    }
}

/// A signed-in device in the ordinary Plex Home arrangement: the adult profile carries the PIN,
/// the child's does not, and `uuid` picks which of them the stored session would resume as.
pub(super) fn signed_in_as(uuid: &str) -> Session {
    Session {
        client_id: "cid".into(),
        account_token: "acct".into(),
        server: ServerRef {
            name: "nas".into(),
            machine_id: "aaaa1111".into(),
            address: "192.168.0.10".into(),
            port: 32400,
            token: "tok-own".into(),
            ..Default::default()
        },
        user: UserRef {
            uuid: uuid.into(),
            title: "stored".into(),
            token: "tok-user".into(),
            ..Default::default()
        },
        home_users: vec![
            session::HomeUserRef {
                uuid: "u-adult".into(),
                title: "Gleb".into(),
                protected: true,
                admin: true,
                ..Default::default()
            },
            session::HomeUserRef {
                uuid: "u-kid".into(),
                title: "Kid".into(),
                ..Default::default()
            },
        ],
        ..Default::default()
    }
}

/// Extract one `fn NAME(` … `}` body, verbatim, from this file's OWN source. A tiny lexer —
/// tracking only whether it is inside a `"…"` string literal or a `//` line comment — rather
/// than a bare brace count, because several of these functions log a `format!("…{x}…")` whose
/// placeholder braces are balanced on their own and would otherwise silently agree with a real
/// count by coincidence; skipping string contents removes the coincidence instead of relying on
/// it.
pub(super) fn extract_fn_body<'a>(src: &'a str, name: &str) -> &'a str {
    let needle = format!("fn {name}(");
    let start = src
        .find(&needle)
        .unwrap_or_else(|| panic!("no `{needle}` in auth.rs — did it get renamed?"));
    let open = src[start..]
        .find('{')
        .map(|i| start + i)
        .unwrap_or_else(|| panic!("`{needle}` has no body"));
    let bytes = src.as_bytes();
    let (mut depth, mut i, mut in_string, mut in_comment) = (0i32, open, false, false);
    while i < bytes.len() {
        let c = bytes[i] as char;
        if in_comment {
            in_comment = c != '\n';
            i += 1;
            continue;
        }
        if in_string {
            if c == '\\' {
                i += 2;
                continue;
            }
            in_string = c != '"';
            i += 1;
            continue;
        }
        match c {
            '"' => in_string = true,
            '/' if bytes.get(i + 1) == Some(&b'/') => in_comment = true,
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return &src[open..=i];
                }
            }
            _ => {}
        }
        i += 1;
    }
    panic!("unterminated body for `{needle}`");
}

/// Does `body`'s raw text call some function whose name starts with `prefix` — an
/// `identifier(` whose identifier begins with `prefix`, found by a plain byte scan (this crate
/// carries no regex dependency)?
///
/// Used below to catch not one exact bypass spelling but the whole NAMING FAMILY it belongs
/// to: this file's `Ctl`-writing setters — `set_error`, `set_error_if_live`,
/// `set_pin_denied_for_test` — all share a `set_` prefix, so a FOURTH one sharing the
/// convention (a `set_phase`, say) is refused here by the family it belongs to, on the commit
/// that adds it, rather than only once someone remembers to type its exact name into a list.
pub(super) fn calls_a_function_prefixed(body: &str, prefix: &str) -> bool {
    let bytes = body.as_bytes();
    let is_ident = |b: u8| b.is_ascii_alphanumeric() || b == b'_';
    let mut i = 0;
    while let Some(rel) = body[i..].find(prefix) {
        let at = i + rel;
        // The match must START an identifier — the byte before it, if any, is not itself part
        // of one — so this cannot fire on `unset_error(` or on some longer name that merely
        // CONTAINS the prefix (`offset_ms(`, say, for a `set_` search).
        let starts_ident = at == 0 || !is_ident(bytes[at - 1]);
        if starts_ident {
            let mut j = at + prefix.len();
            while j < bytes.len() && is_ident(bytes[j]) {
                j += 1;
            }
            let mut k = j;
            while k < bytes.len() && (bytes[k] as char).is_whitespace() {
                k += 1;
            }
            if bytes.get(k) == Some(&b'(') {
                return true;
            }
        }
        i = at + prefix.len();
    }
    false
}
