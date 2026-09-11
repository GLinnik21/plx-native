//! **The redaction pass** — the one place a diagnostic line is made safe to leave the device.
//!
//! Lifted out of `lab/snapshot.rs` unchanged on 2026-08-29, for two reasons that are worth keeping
//! written down.
//!
//! **It is no longer lab-only.** `crate::log` applies [`scrub_local`] to every line before the file
//! write, and the telemetry client applies the full [`scrub`] to everything that leaves. Both live
//! in a build that has no `lab-diagnostics` feature, so this module is UNGATED — which also means
//! its tests finally run in `make check`. They did not before: everything under `lab/` is
//! `#[cfg(feature = "lab-diagnostics")]`, so the 31 assertions guarding the one function the whole
//! privacy argument rests on were skipped by the default gate entirely.
//!
//! **There are two exits, and the difference is the whole design.**
//!
//! * [`scrub`] may return [`Scrubbed::Refuse`] — drop the line and count it. Correct for anything
//!   crossing the network, where a line that cannot be made safe must not be sent.
//! * [`scrub_local`] **rewrites only and never drops.** It is what the on-disk event log gets. A
//!   line silently vanishing from `plxnative-events.log` is strictly worse for debugging than a
//!   leaky one — that file is what `make run`, `tests/run.py`'s assertions and `crash-triage` all
//!   read, and `docs/agent-reference.md` calls it "the primary debugging surface".
//!
//! So: rewrite locally, rewrite-or-refuse remotely, and never a second scrubber anywhere.

/// What [`scrub`] decided about one record.
#[cfg(feature = "lab-diagnostics")]
pub(crate) enum Scrubbed {
    /// keep it, in this (possibly rewritten) form
    Keep(String),
    /// drop it entirely and count it — used where a line cannot be made safe by rewriting
    Refuse,
}

/// Header names whose VALUE is a credential wherever it appears. Matched case-insensitively and
/// followed to the end of the line: a header value has no in-line terminator to stop at, and half
/// a bearer token is still a bearer token.
const CREDENTIAL_HEADERS: [&str; 5] = [
    "authorization:",
    "cookie:",
    "set-cookie:",
    "x-plex-token:",
    "proxy-authorization:",
];

/// Query parameters whose value is a secret. `X-Plex-Token` is here too even though
/// [`crate::redact_tokens`] already caught it on the way in — this pass must be correct on its own,
/// because it is also what protects a record written by a future call site that bypasses the log.
const CREDENTIAL_PARAMS: [&str; 6] = [
    "x-plex-token=",
    "token=",
    "access_token=",
    "password=",
    "apikey=",
    "api_key=",
];

/// One record, made safe.
///
/// Four rewrites, in order, each narrower than dropping the line: header values, query-parameter
/// values, `plex.direct` hostnames (which ENCODE a household's LAN address in their leftmost
/// label), and any remaining `scheme://host` authority. A record that still contains a credential
/// header name after rewriting is refused outright rather than shipped — that can only happen if
/// the rewrite failed to find the value it was sure was there.
#[cfg(feature = "lab-diagnostics")]
pub(crate) fn scrub(line: &str) -> Scrubbed {
    scrub_with(line, &identities())
}

/// **The LOCAL exit — what `crate::log` applies to every line before the file write.**
///
/// Same rewrites as [`scrub`], with one difference that is the entire reason it exists: it
/// **never drops a line**. [`scrub`]'s `Refuse` arm is correct for the network, where a record
/// that cannot be made safe must not be sent; it is wrong for `plxnative-events.log`, which is
/// what `make run`, `tests/run.py`'s assertions and `crash-triage` read. A line that silently
/// vanishes from that file is worse for debugging than a leaky one — you cannot grep for the
/// absence of something you never knew was written.
///
/// Cost: this runs on `crate::log`'s path, which its own doc records as "a few times a second at
/// most, never per frame". Measure before assuming that stays true.
pub(crate) fn scrub_local(line: &str) -> String {
    scrub_local_with(line, &identities())
}

/// [`scrub_local`], against a caller-supplied identity list — the testable half.
pub(crate) fn scrub_local_with(line: &str, ids: &[String]) -> String {
    let s = crate::redact_tokens(line).into_owned();
    let s = scrub_headers(&s);
    let s = scrub_params(&s);
    let s = scrub_authority(&s);
    let s = scrub_addresses(&s);
    let s = scrub_viewing(&s);
    scrub_identities(&s, ids)
}

/// [`scrub`], against a caller-supplied identity list — the testable half. The list is what the
/// APP knows about this household (see [`identities`]); everything else here is a pure rewrite.
#[cfg(feature = "lab-diagnostics")]
pub(crate) fn scrub_with(line: &str, ids: &[String]) -> Scrubbed {
    let s = crate::redact_tokens(line).into_owned();
    let s = scrub_headers(&s);
    let s = scrub_params(&s);
    let s = scrub_authority(&s);
    let s = scrub_addresses(&s);
    let s = scrub_viewing(&s);
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
            if dots == 3
                && tok.split('.').all(|o| {
                    !o.is_empty() && o.len() <= 3 && o.parse::<u16>().is_ok_and(|n| n <= 255)
                })
            {
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
/// **Viewing identity** — what the household is watching or searching for.
///
/// This is LG's own Data Safety category, *"Content Viewing Information: Real-time TV viewing
/// information including VOD and movies"*, and the app's declaration has to be able to answer
/// **Not collected** to it. None of the four passes above touches it: they hunt credentials,
/// hosts, bare addresses and names taken from the session, and a programme title is none of those.
///
/// # This is a BACKSTOP, not the mechanism
///
/// The mechanism is that the call sites do not log it — `app.rs`'s Up Next line logs the
/// ratingKey and not the episode title, `search.rs` logs the query's LENGTH and not the query,
/// `player/mod.rs` logs a subtitle cue's duration and not its dialogue. That is safe by
/// construction and it is what makes the consent screen's "titles are not included" a fact about
/// the code.
///
/// What is left here is the generic shape a future call site can still leak through, which is
/// worth catching centrally because it is cheap and mechanical:
///
/// * **`plex://…` GUIDs** — a Plex GUID names the WORK, globally and stably. It is the single
///   most identifying token that can appear in this log, and it arrives from the server rather
///   than being typed at a call site, so a new site can pick one up without meaning to.
/// * **`q='…'`** — the search-field convention. Kept as a shape rather than trusting `search.rs`
///   to stay disciplined forever.
///
/// **`rk=` is deliberately NOT touched here, and that is the local/remote split doing its job.**
/// A ratingKey is server-local, this file is 0600, and it is the primary handle for triaging a
/// playback bug — `docs/distribution.md`'s own remediation for the title leak was *"logging
/// ratingKeys instead of titles"*. It is stripped on the way OUT instead, by [`scrub_remote_ids`],
/// because a ratingKey plus a server identity is viewing history.
fn scrub_viewing(s: &str) -> String {
    let s = replace_guids(s);
    replace_quoted_value(&s, "q='", '\'', "<query>")
}

/// `plex://episode/5d9c…` → `plex://<guid>`. The scheme survives because "we were resolving a
/// guid here" is the diagnostic; the identifier does not.
fn replace_guids(s: &str) -> String {
    const KEY: &str = "plex://";
    if !s.contains(KEY) {
        return s.to_string();
    }
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(at) = rest.find(KEY) {
        out.push_str(&rest[..at]);
        out.push_str("plex://<guid>");
        let after = &rest[at + KEY.len()..];
        // a guid runs to the first whitespace or quote — the same terminator set the log's other
        // value-shaped fields use
        let end = after
            .find(|c: char| c.is_whitespace() || c == '\'' || c == '"')
            .unwrap_or(after.len());
        rest = &after[end..];
    }
    out.push_str(rest);
    out
}

/// `key…value<close>` → `key<placeholder><close>`, for a value delimited by a known closer.
/// Used for `q='…'`; written generically because the next such shape should reuse it rather than
/// grow a fifth hand-rolled scanner in this file.
fn replace_quoted_value(s: &str, key: &str, close: char, placeholder: &str) -> String {
    if !s.contains(key) {
        return s.to_string();
    }
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(at) = rest.find(key) {
        out.push_str(&rest[..at + key.len()]);
        out.push_str(placeholder);
        let after = &rest[at + key.len()..];
        match after.find(close) {
            Some(end) => {
                out.push(close);
                rest = &after[end + close.len_utf8()..];
            }
            // unterminated: the value runs to end of line, so drop the remainder rather than ship it
            None => return out,
        }
    }
    out.push_str(rest);
    out
}

/// The household identity list, **cached**. Never reads the session file.
///
/// # This function is on `crate::log`'s hot path, and the obvious implementation deadlocks
///
/// It used to be `crate::plex::session::peek()`, which is correct and was fine while the only
/// caller was a once-per-upload snapshot on a worker thread. Putting it under `crate::log` made it
/// two separate disasters at once, both found by the host suite hanging rather than failing:
///
/// * **Deadlock.** `peek()` takes the session I/O mutex. Any code that holds that mutex and then
///   logs — which is most of `plex::session` and `auth` — waits on itself. The whole `auth::tests`
///   block hung on this.
/// * **Disk I/O per log line.** `peek()` walks up to five candidate paths and `read`s each. The
///   log is written a few times a second, so that is a syscall storm for a string rewrite.
///
/// So the list is PUSHED here by the session layer via [`set_identities`] whenever it changes, and
/// read under a short-lived lock this module owns end to end. Nothing inside that lock logs, so
/// there is no cycle to re-create.
///
/// **Cost of being a snapshot:** lines logged before the first `set_identities` are scrubbed
/// without household names. That is the correct trade — the names are not KNOWN before the session
/// loads, so there was nothing to redact — and every other pass (credentials, hosts, addresses,
/// viewing identity) is independent of this list and runs from the first line.
static IDENTITIES: std::sync::RwLock<Vec<String>> = std::sync::RwLock::new(Vec::new());

/// Publish the household names the scrubber should redact. Called by the session layer on load,
/// save, user switch and sign-out. **Must not log while holding anything** — see [`identities`].
pub(crate) fn set_identities(mut v: Vec<String>) {
    v.retain(|s| s.chars().count() >= MIN_IDENTITY);
    // Longest first: "Ada Family Media" must be replaced before a shorter substring of it is, or
    // the tail of the longer name survives as debris.
    v.sort_by_key(|s| std::cmp::Reverse(s.len()));
    v.dedup();
    if let Ok(mut g) = IDENTITIES.write() {
        *g = v;
    }
}

fn identities() -> Vec<String> {
    IDENTITIES.read().map(|g| g.clone()).unwrap_or_default()
}

/// The last gate: a line that STILL looks like it carries a credential after all four rewrites is
/// dropped rather than shipped.
///
/// It exists because the rewrites are string surgery on a format nobody validates — a credential
/// spelled some way none of them anticipated (`Bearer` with no header name in front of it, a
/// parameter separated by `;`) would sail through every one of them. Dropping a record costs one
/// line of a several-hundred-line document and is counted in the envelope; shipping it costs a
/// credential.
#[cfg(feature = "lab-diagnostics")]
fn still_looks_secret(s: &str) -> bool {
    let lower = s.to_ascii_lowercase();
    if lower.contains("bearer ") {
        return true;
    }
    CREDENTIAL_PARAMS.iter().any(|k| {
        lower
            .find(k)
            .is_some_and(|at| !s[at + k.len()..].starts_with("<redacted>"))
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
        let host_len = rest[after..]
            .find(|c: char| c == '/' || c == '?' || c.is_whitespace())
            .unwrap_or(rest.len() - after);
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
    // NB the `Refuse` half is gated to its only present caller (the lab bridge); Phase G widens
    // the gate to `telemetry` when the Sentry client becomes a second one.
    use super::*;

    /// Every rewrite assertion below runs through the LOCAL exit, so it is graded in the default
    /// `make check` rather than only under `--features lab-diagnostics`. The two exits share all
    /// five rewrites and differ only in whether they may drop a line, so this loses no coverage of
    /// the rewriting itself — and the drop behaviour has its own tests at the bottom.
    fn kept(s: &str) -> String {
        scrub_local(s)
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
        assert!(
            !kept("cookie: a=b").contains("a=b"),
            "matched case-insensitively"
        );
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
        assert!(
            !out.contains("plex.direct") && !out.contains("10-0-0-2"),
            "{out}"
        );
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
        let ids = vec![
            "Ada\u{2019}s Mac mini".to_string(),
            "abc123machineid".to_string(),
        ];
        let out = scrub_local_with(
            "auth: reached \"Ada\u{2019}s Mac mini\" 203.0.113.7:32400 (ours) via https://x/y",
            &ids,
        );
        assert!(!out.contains("203.0.113.7"), "{out}");
        assert!(
            !out.contains("32400"),
            "the port goes with the address: {out}"
        );
        assert!(!out.contains("Mac mini"), "{out}");
        assert!(
            out.contains("auth: reached") && out.contains("(ours)"),
            "the line is still a line: {out}"
        );
    }

    /// Loopback and the unspecified address SURVIVE — they identify nobody, and a networking bug
    /// report is often exactly about what the app bound locally.
    #[test]
    fn loopback_survives_the_address_rewrite() {
        assert_eq!(
            scrub_addresses("bound 127.0.0.1:8910 and 0.0.0.0:8911"),
            "bound 127.0.0.1:8910 and 0.0.0.0:8911"
        );
        assert_eq!(
            scrub_addresses("listening on 10.0.0.7:32400"),
            "listening on <addr>"
        );
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
        let out = scrub_local_with("route=home focus ok", &["ok".to_string()]);
        assert_eq!(out, "route=home focus ok");
    }

    /// **The LOCAL exit may never drop a line** — the one behavioural difference between the two
    /// exits, and the reason `scrub_local` exists at all. A record the remote exit refuses outright
    /// still reaches `plxnative-events.log`, rewritten as far as the passes can manage.
    ///
    /// Not a duplicate of the remote `Refuse` test in `lab::snapshot`: that one asserts the line is
    /// dropped AND counted; this one asserts the same input survives locally. A line silently
    /// vanishing from the primary debugging surface is worse than a leaky one, because you cannot
    /// grep for the absence of something you never knew was written.
    #[test]
    fn the_local_exit_never_drops_a_line() {
        let out = scrub_local("auth ok, Bearer eyJhbGciOi.abc");
        assert!(!out.is_empty(), "the line survived");
        assert!(
            out.starts_with("auth ok,"),
            "and it is still recognisably that line: {out}"
        );
    }

    /// Multi-byte text must not panic any of the four rewrites (the app logs remote tokens, item
    /// titles and firmware codenames).
    #[test]
    fn multibyte_text_does_not_panic_the_rewrites() {
        let out = kept("séance ☃ https://héte.example/pâth?token=Q1 — après");
        assert!(!out.contains("Q1"));
        assert!(out.contains("après"));
    }

    // ---- viewing identity: the class `scrub` was never built to catch ---------------------------

    /// **The generic shapes the backstop owns**, quoted from the call sites that produce them:
    ///
    /// * `viewstate.rs:524` / `metadata.rs:2102` — a Plex GUID names the WORK, globally and stably
    /// * `search.rs` — the `q='…'` convention
    ///
    /// This is LG's "Content Viewing Information" category, which the app's Data Safety
    /// declaration has to be able to answer **Not collected** to. None of the other four passes
    /// touches it: they hunt credentials, hosts, bare addresses and names taken from the SESSION,
    /// and a Plex GUID is none of those. Every case passes an EMPTY identity list, the state the
    /// app is in for the whole of boot.
    ///
    /// **`rk=` is deliberately not banned.** A ratingKey is server-local, this file is 0600, and it
    /// is the primary handle for triaging a playback bug — `distribution.md`'s own remediation for
    /// the title leak was *"logging ratingKeys instead of titles"*, which is what `app.rs` now
    /// does. It is a remote-exit concern: a ratingKey plus a server identity is viewing history,
    /// so it is stripped on the way out rather than on the way to disk.
    #[test]
    fn the_generic_viewing_identity_shapes_are_neutralised() {
        let cases: &[(&str, &str)] = &[
            (
                "viewstate: fanout plex://episode/5d9c085e6afb3d server 3f2a: holds 2",
                "5d9c085e6afb3d",
            ),
            (
                "altsrc: asked 3 source(s) for plex://movie/5d7768 -> 2 copy(ies)",
                "5d7768",
            ),
            ("search: q='breaking bad' state=Results", "breaking bad"),
            ("search: q='unterminated at end of line", "unterminated"),
        ];
        for (line, banned) in cases {
            let out = scrub_local_with(line, &[]);
            assert!(
                !out.contains(banned),
                "viewing identity survived\n  in:  {line}\n  out: {out}\n  found: {banned}"
            );
        }
    }

    /// The counterpart, and the reason the test above cannot be satisfied by deleting things: the
    /// diagnostic skeleton has to survive. A scrubber that rewrites every line to `<redacted>`
    /// passes the test above and destroys the file it is protecting.
    #[test]
    fn the_diagnostic_half_of_those_lines_survives() {
        let out = scrub_local_with(
            "viewstate: fanout plex://episode/5d9c08 server 3f2a: holds 2",
            &[],
        );
        assert!(
            out.starts_with("viewstate: fanout plex://"),
            "the SHAPE says what happened: {out}"
        );
        assert!(
            out.contains("holds 2"),
            "the outcome is the diagnostic: {out}"
        );
        let out = scrub_local_with("search: q='breaking bad' state=Results", &[]);
        assert!(
            out.contains("state=Results"),
            "the state machine is the diagnostic: {out}"
        );
    }

    /// **A TITLE CANNOT BE SCRUBBED, and this test exists to say so out loud.**
    ///
    /// There is no rewrite that distinguishes `'The One Where Ross Finds Out'` from
    /// `task: spawn 'labup' REFUSED` or `auth: 'probe' -> ok`. Redacting every single-quoted span
    /// would gut the log; redacting none of them lets a title through. So the mechanism for titles
    /// is **not** this module — it is that the call sites do not write them, which
    /// `no_log_call_site_interpolates_viewing_content` pins by reading the source.
    ///
    /// Asserting the limitation keeps it honest: if someone later teaches the scrubber to eat
    /// quoted spans, this test fails and they have to argue for it rather than discover the
    /// diagnostic loss in a bug report six months on.
    #[test]
    fn a_bare_quoted_title_is_explicitly_out_of_scope_for_the_scrubber() {
        let out = scrub_local_with("some line 'The One Where Ross Finds Out'", &[]);
        assert!(
            out.contains("The One Where Ross Finds Out"),
            "the scrubber is not the mechanism for titles — see this test's doc: {out}"
        );
    }

    /// The ban list `no_log_call_site_interpolates_viewing_content` reads: a banned substring in a
    /// `log(&format!(…))` call, why it is banned, and the `(file, receiver-spelling)` pairs that
    /// are exempt — household identity logged through a differently-spelled receiver, not viewing
    /// content. See that test's doc for why the exemption is scoped by FILE and not by substring
    /// alone.
    const BANNED: &[(&str, &str, &[(&str, &str)])] = &[
        ("ep_title", "the episode title — log `rk=` instead (app.rs)", &[]),
        (
            "q='",
            "the search query — log `q[{n}ch]` instead (search.rs)",
            &[],
        ),
        (
            ".title",
            "the item's own title — log `rk=`/`sid=` and the shape counts (metadata.rs)",
            &[("auth.rs", "tile.title")],
        ),
        (
            "title}",
            "the item's own title as an inline capture — log `rk=`/`sid=` instead",
            &[("auth.rs", "tile.title}")],
        ),
    ];

    /// Every [`BANNED`] hit in one `log(&format!(…))` call, as one formatted offence line per hit
    /// (empty if the call is clean). `path` decides which of a needle's file-scoped exemptions
    /// apply — an exception is struck from the call text FIRST (so a `tile.title` in a call cannot
    /// mask a `d.title` sitting beside it in the SAME call), but only when `path` ends with the
    /// exception's own file.
    fn banned_hits_in_call(path: &std::path::Path, line: usize, call: &str) -> Vec<String> {
        let mut offences = Vec::new();
        for (needle, why, exceptions) in BANNED {
            let mut rest = call.to_string();
            for (file, ex) in *exceptions {
                if path.ends_with(file) {
                    rest = rest.replace(ex, "");
                }
            }
            if rest.contains(needle) {
                offences.push(format!(
                    "{}:{line} logs {needle} — {why}\n    {}",
                    path.display(),
                    call.trim()
                ));
            }
        }
        offences
    }

    /// **Finding 1, phase 11 review (2026-09-10).** Red before the fix: the exception used to be a
    /// bare substring strip (`call.replace("tile.title", "")`) applied regardless of which file the
    /// call came from, so a `tile.title()` receiver OUTSIDE `auth.rs` — exactly what
    /// `ui::tile::Tile::title` (implemented for `pms::PmsMovie`, the library's own content item)
    /// would spell at a call site — was silently exempted too. Scoping the exception to `auth.rs`
    /// closes that: this call, attributed to an unrelated screen module, must still be caught.
    #[test]
    fn the_tile_title_exception_does_not_exempt_a_content_tile_outside_auth_rs() {
        let path = std::path::Path::new("rust-modules/src/ui/card_row.rs");
        let call = "log(&format!(\"card: opened {}\", tile.title()));";
        let hits = banned_hits_in_call(path, 1, call);
        assert!(
            !hits.is_empty(),
            "a `tile.title()` receiver outside auth.rs must not be exempted from the `.title` ban: {hits:?}"
        );
    }

    /// The counterpart: the exemption still holds where it is real — `auth.rs`'s own
    /// switch-diagnostic lines, which log the roster tile's name as household identity.
    #[test]
    fn the_tile_title_exception_still_applies_inside_auth_rs() {
        let path = std::path::Path::new("rust-modules/src/auth.rs");
        let call = "log(&format!(\"auth: switch '{}' -> ok\", tile.title));";
        assert!(
            banned_hits_in_call(path, 1, call).is_empty(),
            "the household-identity exception must still hold for its real, file-scoped case"
        );
    }

    /// **The mechanism for titles, pinned by reading the source.**
    ///
    /// The scrubber cannot catch a programme title (see the test above), so what actually keeps
    /// viewing content out of the log is that no call site writes it. That is a property of ~520
    /// `crate::log` sites, it is invisible to every other test in this suite, and it regresses the
    /// moment somebody adds `'{}'` with a title in it — which is exactly how the original leak got
    /// in. So it is asserted the only way it can be: by grepping the tree.
    ///
    /// The banned identifiers are the FIELDS that carry viewing content, not the words. Each one
    /// was a real leak:
    ///
    /// * `ep_title`   — `app.rs`'s Up Next line; logs `rk=` now (2026-08-29)
    /// * `q='`        — `search.rs`, seven sites; logs `q[Nch]` now (2026-08-29)
    /// * the subtitle cue's text — `player/mod.rs`; logs `len=` now (2026-08-29)
    /// * `.title` / `title}` — `metadata.rs`'s `detail:` line carried the item's own title in
    ///   quotes on every detail open, for as long as that line has existed (found 2026-09-10,
    ///   restructure phase 11). It logs `rk=`/`sid=` and the shape counts now.
    ///
    /// **The gate reads the whole `log(&format!(…))` CALL, not the one line the call opens on**,
    /// and that widening is what makes the fourth entry possible at all: the `detail:` line spells
    /// its format string on one line and its arguments on the two below, so a line-scoped grep
    /// could not have seen `d.title` however the needle was spelled. The two older needles are
    /// clean under the stricter read too — widening found no new hit for either.
    ///
    /// Deliberately NOT banned: `.name` and `.title` on a server or a USER. Those are household
    /// identity rather than viewing content, they are genuinely load-bearing in `auth.rs`'s
    /// diagnostics, and `scrub_identities` removes them from the line at write time using the list
    /// the session layer publishes. Two different problems, two different mechanisms — so the
    /// `.title` needle carries ONE exception, `tile.title`: a roster tile's title IS the profile's
    /// name, which is that identity case, and it is `auth.rs`'s seven switch-diagnostic lines.
    ///
    /// **The exception is scoped by FILE, not by receiver name alone** (fixed 2026-09-10, phase 11
    /// review, finding 1). It used to be a bare substring strip applied to every call in the tree —
    /// but `ui/tile.rs` defines `pub trait Tile { fn title(&self) -> &str; … }`, the library's own
    /// CONTENT item (implemented for `pms::PmsMovie`), whose idiomatic receiver name is also
    /// `tile`. A future `log(&format!("… {} …", tile.title()))` over a content tile would spell the
    /// exact exempted substring and this gate would stay silent on a real title leak. So [`BANNED`]
    /// pairs each exception with the file it is legitimate in — today that is `auth.rs` alone, the
    /// only file that logs a roster tile's own name — and [`banned_hits_in_call`] only strikes an
    /// exception when the call came from that file. A new module logging `tile.title` through some
    /// OTHER receiver is still the leak this entry exists for; only `auth.rs`'s switch diagnostics
    /// are exempt.
    #[test]
    fn no_log_call_site_interpolates_viewing_content() {
        // Walk the source of THIS crate. `file!()` is `src/diag/scrub.rs`, so the tree root is two
        // levels up — resolved from the manifest dir so it is independent of the working directory
        // the test runner happens to have.
        let src = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut offences: Vec<String> = Vec::new();
        let mut files = 0usize;
        walk(&src, &mut |path: &std::path::Path, text: &str| {
            // this file quotes every banned shape in its own documentation
            if path.ends_with("diag/scrub.rs") {
                return;
            }
            files += 1;
            for (n, call) in log_calls(text) {
                offences.extend(banned_hits_in_call(path, n, &call));
            }
        });
        assert!(
            files > 50,
            "the walk found only {files} source files — it is not reading the tree"
        );
        assert!(
            offences.is_empty(),
            "viewing content is interpolated into a log line:\n{}",
            offences.join("\n")
        );
    }

    /// Parenthesis depth over `chars`, counting only `(`/`)` that are OUTSIDE a string or char
    /// literal — the fix for finding 2 of the phase 11 review (2026-09-10). [`log_calls`] used to
    /// count bare `(`/`)` with no notion of a literal at all, on the claim that an unbalanced quote
    /// can only widen its span; a `)` inside a log call's own FORMAT STRING (a real shape in this
    /// tree — see `a_closing_paren_inside_the_format_string_does_not_truncate_the_call_span`)
    /// disproves that, since it can decrement the running depth to zero before the real closing
    /// paren, truncating the span short of the argument line.
    ///
    /// Tracks `\`-escapes inside both a `"…"` string and a `'…'` char literal, so `"a\"b"` and
    /// `'\''` do not mis-toggle. Deliberately does not special-case a raw string (`r#"…"#`): no
    /// `log(&format!` call in this tree uses one (checked 2026-09-10 — re-check before relying on
    /// this again if that changes). If one ever appears, the failure mode is the same SAFE one this
    /// whole scanner had before the fix: an unrecognised `#`/`"` pair is just ordinary characters
    /// here, so the span can only run long, never short. A bare `'` that does not close within a
    /// couple of characters (a lifetime — `'a`, `'static`) is left as an ordinary character rather
    /// than treated as opening a literal, so it cannot swallow real parens hunting for a closing
    /// quote that will never come.
    fn paren_depth_outside_literals(chars: &[char]) -> i32 {
        let mut depth = 0i32;
        let mut in_str = false;
        let mut i = 0usize;
        while i < chars.len() {
            let c = chars[i];
            if in_str {
                match c {
                    '\\' => i += 1, // the escaped character is never a closing quote
                    '"' => in_str = false,
                    _ => {}
                }
            } else if c == '"' {
                in_str = true;
            } else if c == '\'' {
                if chars.get(i + 1) == Some(&'\\') && chars.get(i + 3) == Some(&'\'') {
                    i += 3; // `'\X'` — land on the closing quote
                } else if chars.get(i + 2) == Some(&'\'') {
                    i += 2; // `'X'` — land on the closing quote
                }
                // else: a lifetime or stray apostrophe — leave it as an ordinary character
            } else {
                match c {
                    '(' => depth += 1,
                    ')' => depth -= 1,
                    _ => {}
                }
            }
            i += 1;
        }
        depth
    }

    /// Every `log(&format!(…))` / `log(&*format!(…))` call in one source file, as
    /// `(1-based line the call opens on, the WHOLE call flattened onto one line)`.
    ///
    /// The span ends where the parentheses opened by `log(` balance again, counted by
    /// [`paren_depth_outside_literals`] so a `(`/`)` inside a string or char literal cannot close
    /// (or falsely widen) it. A span that never balances is truncated at the end of the file.
    fn log_calls(text: &str) -> Vec<(usize, String)> {
        let lines: Vec<&str> = text.lines().collect();
        let mut out = Vec::new();
        let mut i = 0usize;
        while i < lines.len() {
            let Some(at) = lines[i]
                .find("log(&format!")
                .or_else(|| lines[i].find("log(&*format!"))
            else {
                i += 1;
                continue;
            };
            // start at the `(` of `log(`, so the depth walk below opens on it
            let start = at + "log".len();
            let mut buf: Vec<char> = lines[i][start..].chars().collect();
            buf.push(' ');
            let mut j = i;
            let mut depth = paren_depth_outside_literals(&buf);
            while depth > 0 && j + 1 < lines.len() {
                j += 1;
                buf.extend(lines[j].chars());
                buf.push(' ');
                depth = paren_depth_outside_literals(&buf);
            }
            out.push((i + 1, buf.into_iter().collect()));
            i = j + 1;
        }
        out
    }

    /// The span walker itself, on a shape this tree has and a line-scoped grep cannot see.
    #[test]
    fn a_log_call_is_read_whole_even_when_its_arguments_are_lines_below() {
        let src = "    crate::log(&format!(\n        \"detail: rk={} '{}'\",\n        d.rk, d.title\n    ));\n    let x = 1;\n";
        let calls = log_calls(src);
        assert_eq!(calls.len(), 1, "one call: {calls:?}");
        assert_eq!(calls[0].0, 1, "reported at the line the call OPENS on");
        assert!(calls[0].1.contains("d.title"), "the arguments are in the span: {}", calls[0].1);
        assert!(!calls[0].1.contains("let x"), "the span stops at the closing paren: {}", calls[0].1);
    }

    /// **Finding 2, phase 11 review (2026-09-10).** The doc above `log_calls` claimed an unbalanced
    /// quote can only WIDEN the span, never shorten it — true for a quote with no `(`/`)` inside,
    /// false the moment one appears. A `)` inside the FORMAT STRING on the call's own opening line
    /// decrements the naive counter early: two closing parens with no matching opens are enough to
    /// walk the running depth (2, after `log(` + `format!(`) down to 0 before the argument line —
    /// where `d.title` actually sits — is ever appended. That is exactly the multi-line shape
    /// `no_log_call_site_interpolates_viewing_content`'s `detail:` entry names as the reason the
    /// gate reads the whole call rather than one line; a `)` in the format string defeats the same
    /// widening from the opposite direction.
    ///
    /// Red before the fix: the format string here contains `))` with no matching opens, so the old
    /// depth walk reached 0 on the string's own line and truncated the span there — `d.title` was
    /// never appended and this assertion failed.
    #[test]
    fn a_closing_paren_inside_the_format_string_does_not_truncate_the_call_span() {
        let src = "    crate::log(&format!(\n        \"detail: rk={} )) still just the string\",\n        d.rk, d.title\n    ));\n    let x = 1;\n";
        let calls = log_calls(src);
        assert_eq!(calls.len(), 1, "one call: {calls:?}");
        assert!(
            calls[0].1.contains("d.title"),
            "a `)` inside the format string must not truncate the span before the argument line: {}",
            calls[0].1
        );
        assert!(
            !calls[0].1.contains("let x"),
            "the span still stops at the real closing paren: {}",
            calls[0].1
        );
    }

    fn walk(dir: &std::path::Path, f: &mut impl FnMut(&std::path::Path, &str)) {
        let Ok(rd) = std::fs::read_dir(dir) else {
            return;
        };
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                walk(&p, f);
            } else if p.extension().is_some_and(|x| x == "rs") {
                if let Ok(t) = std::fs::read_to_string(&p) {
                    f(&p, &t);
                }
            }
        }
    }
}
