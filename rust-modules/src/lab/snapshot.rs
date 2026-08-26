//! The uploaded document: **one envelope line, then one JSON record per ring line** (JSONL).
//!
//! The consumer is a coding agent, not a person, so the format is machine-readable end to end and
//! the envelope carries the structured state that no log line states in one place. It is built in
//! this one file on purpose — that is what makes "what may appear in an upload" a rule with a
//! single edit point rather than a habit spread over the modules that produce the data.
//!
//! # What may appear, and what may not
//!
//! The envelope is assembled from [`crate::player::Diag`], [`crate::webos`] and
//! [`crate::devcaps`], whose fields are numbers, bools, enums and short platform strings.
//! `ui::stats`'s module doc states the rule those types already live under and the reasoning
//! behind each clause; it applies here unchanged and for a stronger reason, since an upload
//! crosses the public internet rather than a room:
//!
//! * **no URL and no path** — the PMS token rides in the query string of every playback and image
//!   URL, so a URL-shaped field is a guaranteed credential leak rather than a possible one;
//! * **no credential at any length** — omitted, never masked, because a PMS token is short and
//!   shape-indistinguishable from an ordinary opaque id;
//! * **no stable identity** — not the server's friendly name (commonly the owner's first name),
//!   not its `machineIdentifier` (a permanent household fingerprint), not its address;
//! * **no viewing identity** — what is playing appears only as its technical shape.
//!
//! # Defence in depth: [`scrub`]
//!
//! Ring records are ordinary log lines, and the log's own policy — *no call site formats a URL into
//! a line* — has been violated before (`crate::redact_tokens`'s doc carries that history: one
//! `-> {url}` in `route::retranscode`, reached by an ordinary audio-track switch, live for months).
//! So every record passes a second, broader pass on the way out. It is deliberately not the same
//! function as the log's: that one is a hot-path backstop for one parameter name, this one is a
//! wider sweep that runs once per upload on a worker thread and can afford to be thorough.
use crate::lab::ring::Rec;
use serde::Serialize;

// ---- the envelope -----------------------------------------------------------------------------

#[derive(Serialize)]
pub(crate) struct Envelope {
    /// always `"envelope"`, so a reader can dispatch on line kind without counting lines
    pub kind: &'static str,
    /// which upload this is within the session, from 1
    pub seq: u32,
    pub session: String,
    /// what triggered it: `"key"` or `"menu"` — the field that settles the colour-button question
    pub reason: String,
    /// the ring clock at the moment of the snapshot (= app uptime in ms)
    pub sent_at_ms: u32,
    pub app: App,
    pub device: Device,
    pub caps: Caps,
    pub player: Player,
    /// route the app was on when the button was pressed
    pub route: &'static str,
    /// ring records evicted since the last snapshot — a non-zero value means the window was too
    /// small and the interesting part may be missing
    pub dropped: u64,
    /// records refused by [`scrub`] outright (see [`Scrubbed::Refuse`])
    pub refused: u64,
    pub records: usize,
}

#[derive(Serialize)]
pub(crate) struct App {
    pub version: &'static str,
    pub id: &'static str,
    pub flavour: &'static str,
    pub features: Vec<&'static str>,
    pub uptime_ms: u32,
}

#[derive(Serialize)]
pub(crate) struct Device {
    pub webos_release: String,
    pub webos_codename: String,
    pub webos_api: String,
    pub webos_name: String,
    pub model: String,
    pub board: String,
    pub hw_revision: String,
}

/// What the SoC's own table says it decodes ([`crate::devcaps`]) — the field that separates "this
/// firmware refuses the stream" from "this set was never going to decode it".
#[derive(Serialize)]
pub(crate) struct Caps {
    pub hevc: bool,
    pub hevc_max_w: u32,
    pub hevc_max_h: u32,
    pub vp9: bool,
    /// the direct-playable audio subset, in `plex::DP_AUDIO_CODECS`'s comma form
    pub audio: String,
}

/// The playback state, out of one consistent [`crate::player::Diag`] read.
///
/// Enums are sent as the STRINGS the diagnostics panel prints rather than as their raw
/// discriminants: the receiving agent should not have to hold this crate's numbering in its head,
/// and the raw number is meaningless without it.
#[derive(Serialize)]
pub(crate) struct Player {
    pub vp_mode: &'static str,
    pub window_id: String,
    pub acb_ok: bool,
    pub stage: u8,
    pub load_completed: bool,
    pub load_failed: bool,
    pub load_video_codec: &'static str,
    pub load_audio_codec: &'static str,
    pub feed_state: &'static str,
    pub feed_is_fault: bool,
    pub video_w: i32,
    pub video_h: i32,
    pub pos_ns: i64,
    pub dur_ns: i64,
    pub frames: i32,
    pub seen_frame: bool,
    pub fed_v: i64,
    pub fed_a: i64,
    pub aq_video: i64,
    pub aq_audio: i64,
    pub cb_count: u32,
    pub cb_err: i32,
    pub cb_err_at: u32,
    pub http_status: i32,
    pub net_rx: i64,
    pub abr_mode: u8,
    pub abr_kbps: i64,
    pub abr_net_kbps: i64,
    pub abr_buffer_ms: i64,
    pub abr_action: u8,
    pub abr_why: u8,
}

#[derive(Serialize)]
struct Line<'a> {
    t_ms: u32,
    m: &'a str,
}

impl From<&crate::player::Diag> for Player {
    fn from(d: &crate::player::Diag) -> Self {
        Player {
            vp_mode: d.vp_mode_str(),
            window_id: d.window_id.clone(),
            acb_ok: d.acb_ok,
            stage: d.stage,
            load_completed: d.load_completed,
            load_failed: d.load_failed,
            load_video_codec: d.load_v_str(),
            load_audio_codec: d.load_a_str(),
            feed_state: d.feed_state_str(),
            feed_is_fault: d.feed_is_fault(),
            video_w: d.video_w,
            video_h: d.video_h,
            pos_ns: d.pos_ns,
            dur_ns: d.dur_ns,
            frames: d.frames,
            seen_frame: d.seen_frame,
            fed_v: d.fed_v,
            fed_a: d.fed_a,
            aq_video: d.aq_video,
            aq_audio: d.aq_audio,
            cb_count: d.cb_count,
            cb_err: d.cb_err,
            cb_err_at: d.cb_err_at,
            http_status: d.http_status,
            net_rx: d.net_rx,
            abr_mode: d.abr_mode,
            abr_kbps: d.abr_kbps,
            abr_net_kbps: d.abr_net_kbps,
            abr_buffer_ms: d.abr_buffer_ms,
            abr_action: d.abr_action,
            abr_why: d.abr_why,
        }
    }
}

/// Which cargo features this binary was built with — the first question asked of any log whose
/// behaviour looks wrong for the code, and one an uploaded document can answer for itself.
fn features() -> Vec<&'static str> {
    let mut v = vec!["lab-diagnostics"];
    if cfg!(feature = "devtools") {
        v.push("devtools");
    }
    if cfg!(feature = "devtriggers") {
        v.push("devtriggers");
    }
    v
}

/// Build the whole body. **Main thread**: `player::diag()` is main-thread by contract, and the
/// ring clone is a memcpy of at most [`crate::lab::ring::MAX_BYTES`].
pub(crate) fn build(seq: u32, reason: &str, session: &str, route: &'static str) -> String {
    let d = crate::player::diag();
    let (recs, dropped) = crate::lab::ring::take();
    body(seq, reason, session, route, &d, recs, dropped)
}

/// The serialisation half, split out so `make check` can grade the document without a television:
/// `Diag::default()` is the never-started session and needs no Starfish symbols.
pub(crate) fn body(
    seq: u32,
    reason: &str,
    session: &str,
    route: &'static str,
    d: &crate::player::Diag,
    recs: Vec<Rec>,
    dropped: u64,
) -> String {
    let now = crate::lab::ring::t_ms();
    let mut lines: Vec<String> = Vec::with_capacity(recs.len() + 1);
    let mut refused = 0u64;
    let mut kept: Vec<Line> = Vec::with_capacity(recs.len());
    let scrubbed: Vec<(u32, String)> = recs
        .iter()
        .filter_map(|r| match scrub(&r.msg) {
            Scrubbed::Keep(s) => Some((r.t_ms, s)),
            Scrubbed::Refuse => {
                refused += 1;
                None
            }
        })
        .collect();
    for (t_ms, m) in &scrubbed {
        kept.push(Line { t_ms: *t_ms, m });
    }
    let env = Envelope {
        kind: "envelope",
        seq,
        session: session.to_string(),
        reason: reason.to_string(),
        sent_at_ms: now,
        app: App {
            version: env!("CARGO_PKG_VERSION"),
            id: crate::paths::app_id(),
            flavour: crate::paths::flavour().unwrap_or("stable"),
            features: features(),
            uptime_ms: now,
        },
        device: device(),
        caps: caps(),
        player: Player::from(d),
        route,
        dropped,
        refused,
        records: kept.len(),
    };
    // `to_string` on a struct of numbers and short strings cannot fail; if it somehow did, an
    // empty envelope line would make the whole upload unreadable, so the fallback still says what
    // happened in the same shape.
    lines.push(
        serde_json::to_string(&env)
            .unwrap_or_else(|_| r#"{"kind":"envelope","error":"envelope did not serialise"}"#.into()),
    );
    for l in &kept {
        if let Ok(s) = serde_json::to_string(l) {
            lines.push(s);
        }
    }
    lines.join("\n")
}

fn device() -> Device {
    let i = crate::webos::info();
    let d = crate::webos::device();
    Device {
        webos_release: i.release.clone(),
        webos_codename: i.codename.clone(),
        webos_api: i.api.clone(),
        webos_name: i.name.clone(),
        model: d.model.clone(),
        board: d.board.clone(),
        hw_revision: d.hw_revision.clone(),
    }
}

fn caps() -> Caps {
    let c = crate::devcaps::caps();
    Caps {
        hevc: c.hevc,
        hevc_max_w: c.hevc_max.0,
        hevc_max_h: c.hevc_max.1,
        vp9: c.vp9,
        audio: c.audio.clone(),
    }
}

// ---- the second redaction pass ------------------------------------------------------------

/// What [`scrub`] decided about one record.
pub(crate) enum Scrubbed {
    /// keep it, in this (possibly rewritten) form
    Keep(String),
    /// drop it entirely and count it — used where a line cannot be made safe by rewriting
    Refuse,
}

/// Header names whose VALUE is a credential wherever it appears. Matched case-insensitively and
/// followed to the end of the line: a header value has no in-line terminator to stop at, and half
/// a bearer token is still a bearer token.
const CREDENTIAL_HEADERS: [&str; 5] =
    ["authorization:", "cookie:", "set-cookie:", "x-plex-token:", "proxy-authorization:"];

/// Query parameters whose value is a secret. `X-Plex-Token` is here too even though
/// [`crate::redact_tokens`] already caught it on the way in — this pass must be correct on its own,
/// because it is also what protects a record written by a future call site that bypasses the log.
const CREDENTIAL_PARAMS: [&str; 6] =
    ["x-plex-token=", "token=", "access_token=", "password=", "apikey=", "api_key="];

/// One record, made safe.
///
/// Four rewrites, in order, each narrower than dropping the line: header values, query-parameter
/// values, `plex.direct` hostnames (which ENCODE a household's LAN address in their leftmost
/// label), and any remaining `scheme://host` authority. A record that still contains a credential
/// header name after rewriting is refused outright rather than shipped — that can only happen if
/// the rewrite failed to find the value it was sure was there.
pub(crate) fn scrub(line: &str) -> Scrubbed {
    scrub_with(line, &identities())
}

/// [`scrub`], against a caller-supplied identity list — the testable half. The list is what the
/// APP knows about this household (see [`identities`]); everything else here is a pure rewrite.
pub(crate) fn scrub_with(line: &str, ids: &[String]) -> Scrubbed {
    let s = crate::redact_tokens(line).into_owned();
    let s = scrub_headers(&s);
    let s = scrub_params(&s);
    let s = scrub_authority(&s);
    let s = scrub_addresses(&s);
    let s = scrub_identities(&s, ids);
    if still_looks_secret(&s) {
        return Scrubbed::Refuse;
    }
    Scrubbed::Keep(s)
}

/// **A BARE ADDRESS, outside any URL** — `203.0.113.7:32400`, `10.0.0.2`, an IPv6 literal.
///
/// [`scrub_authority`] only sees an address that follows `://`, and the device test found the gap
/// immediately: `plex: server slot 0 re-pointed to 203.0.113.7:32400` is not a URL, and it puts a
/// household's LAN topology in an upload that crosses the public internet. The port goes with the
/// address, because a port is only diagnostic in company with the address it is on.
///
/// **Loopback and the unspecified address survive**, and that is deliberate: `127.0.0.1` and
/// `0.0.0.0` identify nobody, and blanking them would destroy the lines that say what the app
/// bound locally — which is exactly what a networking bug report is about. Same list
/// `outbound-guard.py` treats as generic.
fn scrub_addresses(s: &str) -> String {
    let keep = |a: &str| a.starts_with("127.") || a == "0.0.0.0" || a == "::1";
    let mut out = String::with_capacity(s.len());
    let b = s.as_bytes();
    let mut i = 0;
    while i < b.len() {
        // an address starts at a digit that is not preceded by one (or by a dot, or a colon we
        // already consumed) — so `v1.2` and a version string are not addresses
        let boundary = i == 0 || !matches!(b[i - 1], b'0'..=b'9' | b'.' | b':' | b'-');
        if boundary && b[i].is_ascii_digit() {
            let mut j = i;
            let mut dots = 0;
            while j < b.len() && (b[j].is_ascii_digit() || b[j] == b'.') {
                if b[j] == b'.' {
                    dots += 1;
                }
                j += 1;
            }
            let tok = &s[i..j];
            if dots == 3 && tok.split('.').all(|o| !o.is_empty() && o.len() <= 3 && o.parse::<u16>().is_ok_and(|n| n <= 255)) {
                // …and its port, if it has one
                let mut k = j;
                if k < b.len() && b[k] == b':' {
                    let mut m = k + 1;
                    while m < b.len() && b[m].is_ascii_digit() {
                        m += 1;
                    }
                    if m > k + 1 {
                        k = m;
                    }
                }
                if keep(tok) {
                    out.push_str(&s[i..k]);
                } else {
                    out.push_str("<addr>");
                }
                i = k;
                continue;
            }
            out.push_str(tok);
            i = j;
            continue;
        }
        let ch = s[i..].chars().next().unwrap_or('\0');
        out.push(ch);
        i += ch.len_utf8();
    }
    out
}

/// Shortest identity worth replacing. Below this a name matches inside ordinary words.
const MIN_IDENTITY: usize = 4;

/// **The names and ids of THIS household**, replaced wherever they appear.
///
/// The one clause of §6 that no generic rewrite can enforce: a server's friendly name defaults to
/// the owner's hostname — the device test's first upload carried `auth: reached "Ada's Mac mini"`
/// — and a `machineIdentifier` is a permanent household fingerprint. Neither has a recognisable
/// SHAPE, so the only way to redact them is to know them, which this process does.
///
/// Short values are skipped: a two-character profile title would match inside ordinary words and
/// turn the whole document into `<name>`. That is a real limit and it is the honest trade — the
/// alternative is a scrubber that destroys the log it is protecting.
fn scrub_identities(s: &str, ids: &[String]) -> String {
    let mut out = s.to_string();
    for id in ids {
        // The length floor lives HERE and not only in `identities`, so it holds for any caller —
        // it is a correctness rule about substring replacement, not a property of one list.
        if id.chars().count() >= MIN_IDENTITY && out.contains(id.as_str()) {
            out = out.replace(id.as_str(), "<name>");
        }
    }
    out
}

/// What this install knows about its own household, longest first so a name that contains another
/// is replaced whole. Read once per upload from the persisted session — `peek`, never `load`, so
/// producing a snapshot can never WRITE the session file.
fn identities() -> Vec<String> {
    let sess = crate::plex::session::peek();
    let mut v: Vec<String> = Vec::new();
    let mut push = |s: &str| {
        if s.chars().count() >= MIN_IDENTITY {
            v.push(s.to_string());
        }
    };
    push(&sess.server.name);
    push(&sess.server.machine_id);
    push(&sess.user.title);
    for u in &sess.home_users {
        push(&u.title);
        push(&u.uuid);
    }
    for src in &sess.sources {
        push(&src.name);
        push(&src.machine_id);
        push(&src.shared_by);
    }
    v.sort_by_key(|s| std::cmp::Reverse(s.len()));
    v.dedup();
    v
}

/// The last gate: a line that STILL looks like it carries a credential after all four rewrites is
/// dropped rather than shipped.
///
/// It exists because the rewrites are string surgery on a format nobody validates — a credential
/// spelled some way none of them anticipated (`Bearer` with no header name in front of it, a
/// parameter separated by `;`) would sail through every one of them. Dropping a record costs one
/// line of a several-hundred-line document and is counted in the envelope; shipping it costs a
/// credential.
fn still_looks_secret(s: &str) -> bool {
    let lower = s.to_ascii_lowercase();
    if lower.contains("bearer ") {
        return true;
    }
    CREDENTIAL_PARAMS.iter().any(|k| {
        lower.find(k).is_some_and(|at| !s[at + k.len()..].starts_with("<redacted>"))
    })
}

/// `Authorization: Bearer abc` → `Authorization: <redacted>`. To end of line, deliberately.
fn scrub_headers(s: &str) -> String {
    let lower = s.to_ascii_lowercase();
    let mut cut = None;
    for h in CREDENTIAL_HEADERS {
        if let Some(at) = lower.find(h) {
            let end = at + h.len();
            cut = Some(cut.map_or(end, |c: usize| c.min(end)));
        }
    }
    match cut {
        Some(end) => format!("{} <redacted>", &s[..end]),
        None => s.to_string(),
    }
}

/// `?token=abc&x=1` → `?token=<redacted>&x=1`. The value ends at the next `&` or whitespace, so
/// the diagnostic half of the line survives — the reasoning `redact_tokens` gives for the same
/// choice: truncating from the parameter onward hides the fields that made the line worth logging.
fn scrub_params(s: &str) -> String {
    let mut out = s.to_string();
    loop {
        let lower = out.to_ascii_lowercase();
        let Some((at, key)) = CREDENTIAL_PARAMS
            .iter()
            .filter_map(|k| lower.find(k).map(|i| (i, *k)))
            .filter(|(i, k)| {
                // the value must not already be the placeholder, or this loops forever
                !out[i + k.len()..].starts_with("<redacted>")
            })
            .min_by_key(|(i, _)| *i)
        else {
            return out;
        };
        let vstart = at + key.len();
        let vlen = out[vstart..]
            .find(|c: char| c == '&' || c.is_whitespace())
            .unwrap_or(out.len() - vstart);
        out.replace_range(vstart..vstart + vlen, "<redacted>");
    }
}

/// Any `scheme://authority` becomes `scheme://<host>`, keeping the path.
///
/// This is the clause that covers what no parameter list can: a PMS address, a `plex.direct` name
/// (whose leftmost label is a hex-encoded LAN address, i.e. a household fingerprint), a friend's
/// shared server, a port that identifies a deployment. The PATH is what makes a line diagnostic —
/// `/video/:/transcode/universal/start.mkv` — and it stays.
fn scrub_authority(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(at) = rest.find("://") {
        let after = at + 3;
        let host_len =
            rest[after..].find(|c: char| c == '/' || c == '?' || c.is_whitespace()).unwrap_or(rest.len() - after);
        if host_len == 0 {
            out.push_str(&rest[..after]);
            rest = &rest[after..];
            continue;
        }
        out.push_str(&rest[..after]);
        out.push_str("<host>");
        rest = &rest[after + host_len..];
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kept(s: &str) -> String {
        match scrub(s) {
            Scrubbed::Keep(s) => s,
            Scrubbed::Refuse => panic!("refused: {s}"),
        }
    }

    /// The exact shape the log's own backstop was written for still cannot survive this one.
    #[test]
    fn a_plex_token_never_survives_either_pass() {
        let out = kept("retranscode rk=42 -> http://10.0.0.2:32400/video/:/transcode/universal/start.mkv?protocol=http&X-Plex-Token=aBcD1234xyzQ");
        assert!(!out.contains("aBcD1234xyzQ"));
        assert!(out.contains("start.mkv"), "the diagnostic half survives");
    }

    /// A header value is a credential to END OF LINE — half a bearer token is still one.
    #[test]
    fn a_credential_header_is_cut_to_end_of_line() {
        let out = kept("hdr Authorization: Bearer ey.JhbGciOi.abc def");
        assert_eq!(out, "hdr Authorization: <redacted>");
        assert!(!kept("Set-Cookie: sid=9f3; Path=/").contains("9f3"));
        assert!(!kept("cookie: a=b").contains("a=b"), "matched case-insensitively");
    }

    /// Query parameters this app never logs today, and would leak if it started.
    #[test]
    fn secret_query_parameters_are_cut_at_the_separator() {
        let out = kept("GET /login?password=hunter2&user=x ok");
        assert!(!out.contains("hunter2"));
        assert!(out.contains("user=x") && out.ends_with(" ok"));
        assert!(!kept("?access_token=AAA&b=1").contains("AAA"));
    }

    /// Two secrets on one line: the rewrite loop must terminate and catch both.
    #[test]
    fn several_parameters_on_one_line_all_go_and_the_loop_ends() {
        let out = kept("a?token=AAA b?apikey=BBB");
        assert!(!out.contains("AAA") && !out.contains("BBB"), "{out}");
        assert_eq!(out.matches("<redacted>").count(), 2);
    }

    /// The address clause: a `plex.direct` name encodes a LAN address in its leftmost label, and a
    /// PMS host is a household's location. The path is the diagnostic half and stays.
    #[test]
    fn a_host_is_replaced_and_the_path_is_kept() {
        let out = kept("open https://10-0-0-2.abc123.plex.direct:32400/library/sections?x=1");
        assert!(!out.contains("plex.direct") && !out.contains("10-0-0-2"), "{out}");
        assert!(out.contains("/library/sections"), "{out}");
        assert!(out.starts_with("open https://<host>"), "{out}");
    }

    /// An ordinary line is untouched — this pass runs over every record of every upload, and a
    /// scrubber that mangles healthy lines destroys the thing it is protecting.
    #[test]
    fn ordinary_lines_pass_through_unchanged() {
        for line in [
            "feed v#12 reply=Ok",
            "load: v=H265 a=AC3 PLUS fps=23.976 dv=8.1 atmos=0",
            "loop=62 fps=0 pos=41",
            "acb bind rv=0 planeId=1",
        ] {
            assert_eq!(kept(line), line);
        }
    }

    /// **The two leaks the DEVICE test found**, on the first real upload — neither of which any
    /// host test had been written to look for, and both of which §6 promises are impossible.
    #[test]
    fn a_bare_address_and_a_household_name_are_both_redacted() {
        let ids = vec!["Ada\u{2019}s Mac mini".to_string(), "abc123machineid".to_string()];
        let out = match scrub_with(
            "auth: reached \"Ada\u{2019}s Mac mini\" 203.0.113.7:32400 (ours) via https://x/y",
            &ids,
        ) {
            Scrubbed::Keep(s) => s,
            Scrubbed::Refuse => panic!("refused"),
        };
        assert!(!out.contains("203.0.113.7"), "{out}");
        assert!(!out.contains("32400"), "the port goes with the address: {out}");
        assert!(!out.contains("Mac mini"), "{out}");
        assert!(out.contains("auth: reached") && out.contains("(ours)"), "the line is still a line: {out}");
    }

    /// Loopback and the unspecified address SURVIVE — they identify nobody, and a networking bug
    /// report is often exactly about what the app bound locally.
    #[test]
    fn loopback_survives_the_address_rewrite() {
        assert_eq!(scrub_addresses("bound 127.0.0.1:8910 and 0.0.0.0:8911"),
                   "bound 127.0.0.1:8910 and 0.0.0.0:8911");
        assert_eq!(scrub_addresses("listening on 10.0.0.7:32400"), "listening on <addr>");
    }

    /// Things that LOOK like addresses and are not: a version, a frame rate, a timestamp, a
    /// resolution. A scrubber that eats these destroys the document it is protecting.
    #[test]
    fn version_like_numbers_are_not_addresses() {
        for line in [
            "pms: server 0 version=1.41.0.8992",
            "load: v=H265 fps=23.976",
            "surface: window=1920x1080 scale=0.500",
            "feed v#12 reply=Ok",
            "webos: release=4.10.2 major=4",
        ] {
            assert_eq!(scrub_addresses(line), line, "mangled: {line}");
        }
    }

    /// A short identity is NOT applied — it would match inside ordinary words. Stated as a test
    /// because it is a deliberate limit rather than an oversight.
    #[test]
    fn a_very_short_name_is_left_alone_by_design() {
        let out = match scrub_with("route=home focus ok", &["ok".to_string()]) {
            Scrubbed::Keep(s) => s,
            Scrubbed::Refuse => panic!(),
        };
        assert_eq!(out, "route=home focus ok");
    }

    /// A credential shape none of the four rewrites anticipated is DROPPED, not shipped — and the
    /// drop is counted in the envelope so the document says a line is missing.
    #[test]
    fn an_unanticipated_credential_shape_is_refused_outright() {
        assert!(matches!(scrub("auth ok, Bearer eyJhbGciOi.abc"), Scrubbed::Refuse));
        let recs = vec![
            Rec { t_ms: 1, msg: "keep me".into() },
            Rec { t_ms: 2, msg: "Bearer eyJ.abc".into() },
        ];
        let doc = body(1, "key", "s", "home", &crate::player::Diag::default(), recs, 0);
        assert!(!doc.contains("eyJ.abc"), "{doc}");
        let env: serde_json::Value = serde_json::from_str(doc.split('\n').next().unwrap()).unwrap();
        assert_eq!(env["refused"], 1);
        assert_eq!(env["records"], 1);
    }

    /// Multi-byte text must not panic any of the four rewrites (the app logs remote tokens, item
    /// titles and firmware codenames).
    #[test]
    fn multibyte_text_does_not_panic_the_rewrites() {
        let out = kept("séance ☃ https://héte.example/pâth?token=Q1 — après");
        assert!(!out.contains("Q1"));
        assert!(out.contains("après"));
    }

    /// The document's SHAPE: line 1 is the envelope, one line per kept record, and the counts in
    /// the envelope describe the lines that follow it.
    #[test]
    fn the_document_is_one_envelope_line_then_one_line_per_record() {
        let recs = vec![
            Rec { t_ms: 10, msg: "first".into() },
            Rec { t_ms: 20, msg: "second".into() },
        ];
        let doc = body(3, "key", "a1b2c3d4", "player", &crate::player::Diag::default(), recs, 7);
        let lines: Vec<&str> = doc.split('\n').collect();
        assert_eq!(lines.len(), 3);
        let env: serde_json::Value = serde_json::from_str(lines[0]).expect("envelope is JSON");
        assert_eq!(env["kind"], "envelope");
        assert_eq!(env["seq"], 3);
        assert_eq!(env["reason"], "key");
        assert_eq!(env["dropped"], 7);
        assert_eq!(env["records"], 2);
        assert_eq!(env["route"], "player");
        assert_eq!(env["app"]["version"], env!("CARGO_PKG_VERSION"));
        // the never-started session reads honestly rather than as a healthy one
        assert_eq!(env["player"]["vp_mode"], "NONE — no video path");
        assert_eq!(env["player"]["feed_state"], "— nothing fed yet");
        let rec: serde_json::Value = serde_json::from_str(lines[2]).expect("record is JSON");
        assert_eq!(rec["t_ms"], 20);
        assert_eq!(rec["m"], "second");
    }

    /// Every line is independently parseable — that is the whole point of JSONL, and a record
    /// containing a newline, a quote or a control character must not break the frame.
    #[test]
    fn a_record_with_hostile_characters_stays_one_json_line() {
        let recs = vec![Rec { t_ms: 1, msg: "a\nb\t\"c\"\\d".into() }];
        let doc = body(1, "menu", "s", "home", &crate::player::Diag::default(), recs, 0);
        let lines: Vec<&str> = doc.split('\n').collect();
        assert_eq!(lines.len(), 2, "the embedded newline did not split the record");
        let rec: serde_json::Value = serde_json::from_str(lines[1]).expect("still JSON");
        assert_eq!(rec["m"], "a\nb\t\"c\"\\d");
    }
}
