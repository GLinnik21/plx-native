//! Subtitle search & download (impl Client) — the agent-backed "Search subtitles" flow.
//!
//! **Read `docs/pms-api.md` §8, not `docs/plex-openapi.json`.** The spec is wrong about this
//! endpoint in three ways and silent in a fourth: it calls the operation "Add a subtitle", marks it
//! `admin`-scoped, gives it no response schema, and documents no download parameter. Measured live
//! 2026-10-04, it is a SEARCH returning candidates in `Stream[]`, it answers on a server the
//! account does not own, and the download is a `PUT` carrying the candidate's key.
//!
//! Two server behaviours shape this module, and both are guards rather than preferences:
//!
//! * **A 3-letter language code returns HTTP 500** — a bare-HTML unhandled exception, not a Plex
//!   error body. So [`query_language`] folds to the 2-letter primary subtag and REFUSES anything
//!   else rather than putting a request on the wire that crashes somebody's server.
//! * **Candidate keys are ephemeral.** Each search mints new ids, and a download issued with a key
//!   from an earlier search answers `200` and silently does nothing. A caller must pass a key from
//!   the search it is acting on; this layer cannot detect staleness, so the rule lives in the
//!   caller's doc and in `docs/pms-api.md` §8.
use super::client::{Client, JsonStatusOutcome, QueryBuilder};
use super::models::MediaContainer;
use super::params::SubtitleSearch;

/// A subtitle search preserves the answers the UI must present distinctly. Every other failure —
/// no response, an unexpected status, a malformed 2xx body — is one retryable `Transport`.
///
/// `Denied` is deliberately its own arm and must never be retried on a timer: a 403 retried is a
/// 403, and the page has to say so instead of spinning.
pub enum SubtitleSearchOutcome {
    Ok(MediaContainer),
    Denied,
    Missing,
    /// The language could not be expressed as the 2-letter code PMS accepts, so nothing was sent.
    /// Never a server answer — see the module doc for why this one is refused locally.
    BadLanguage,
    Transport,
}

/// The download's answer. `Ok` means the server ACCEPTED the request, not that the subtitle has
/// landed: the install is asynchronous (`docs/pms-api.md` §8), and the created stream appears on a
/// later read of the item. A caller that refreshes once, immediately, will reliably see nothing.
pub enum SubtitleAddOutcome {
    Ok,
    Denied,
    Missing,
    /// The candidate key was not a `/library/streams/{id}` path, so nothing was sent.
    BadKey,
    Transport,
}

/// What one PMS answer means, independent of which operation asked. Pure so the whole table is
/// gradeable on the host without a socket.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum Verdict {
    Ok,
    Denied,
    Missing,
    Transport,
}

/// `status` as the operations read it. `reached` is false when nothing answered at all; `usable`
/// is false when a 2xx body could not be parsed, which is a fault and not an empty answer.
pub(super) fn verdict(reached: bool, status: i32, usable: bool) -> Verdict {
    if !reached {
        return Verdict::Transport;
    }
    match status {
        401 | 403 => Verdict::Denied,
        404 => Verdict::Missing,
        200..=299 if usable => Verdict::Ok,
        _ => Verdict::Transport,
    }
}

/// The `language` value PMS will accept: the lower-cased 2-letter primary subtag, or `None`.
///
/// `"nl"`, `"NL"`, `"en-GB"` and `"pt_BR"` all answer a 2-letter code. `"nld"`, `"eng"`, `""` and
/// `"x"` answer `None` — **`None` is a refusal to send, not a fallback**, because a 3-letter code
/// does not merely fail here: it returns a bare-HTML HTTP 500 from the server (`docs/pms-api.md`
/// §8). Folding a 3-letter spelling onto its 2-letter one needs the spelling table in the data
/// layer (`metadata::two_letter_code`), which this crate sits below; callers fold first and this
/// guards.
pub fn query_language(code: &str) -> Option<String> {
    let primary = code.trim().split(['-', '_']).next().unwrap_or("");
    let two = primary.len() == 2 && primary.bytes().all(|b| b.is_ascii_alphabetic());
    two.then(|| primary.to_ascii_lowercase())
}

/// A candidate key is server data and may only ever be the one shape the download is for. Mirrors
/// `library::sidecar_key_allowed`, and for the same reason: a key is interpolated into a path.
fn candidate_key_allowed(key: &str) -> bool {
    let Some(tail) = key.strip_prefix("/library/streams/") else {
        return false;
    };
    let (id, ext) = tail.split_once('.').unwrap_or((tail, ""));
    !id.is_empty()
        && id.len() <= 20
        && id.bytes().all(|b| b.is_ascii_digit())
        && ext.len() <= 10
        && ext.bytes().all(|b| b.is_ascii_alphanumeric())
}

/// The search path, split out so its shape is gradeable without a `Client` or a socket.
///
/// `hearingImpaired`/`forced` are sent even at `0`, which is NOT a breach of the zero rule
/// (`docs/pms-api.md` §3b): that rule is about an optional number whose absence means "unset",
/// while these two are explicit filter flags the measured request carried as `0` and PMS answered.
fn search_path(rating_key: &str, language: &str, hearing_impaired: bool, forced: bool) -> String {
    QueryBuilder::new(format!("/library/metadata/{rating_key}/subtitles"))
        .str("language", language)
        .int("hearingImpaired", i64::from(hearing_impaired))
        .int("forced", i64::from(forced))
        .build()
}

impl Client {
    /// `GET /library/metadata/{rk}/subtitles` — the agent's candidates for one item.
    ///
    /// An `Ok` container with zero `Stream[]` rows is the server's ANSWER ("the agent found
    /// nothing"), never a fault; the caller must keep the two apart or it tells the viewer
    /// something untrue about their library.
    pub fn search_subtitles(
        &self,
        rating_key: &str,
        query: &SubtitleSearch,
    ) -> SubtitleSearchOutcome {
        let Some(language) = query_language(&query.language) else {
            return SubtitleSearchOutcome::BadLanguage;
        };
        let path = search_path(rating_key, &language, query.hearing_impaired, query.forced);
        match self.get_json_status(&path) {
            JsonStatusOutcome::Transport => SubtitleSearchOutcome::Transport,
            JsonStatusOutcome::Response { status, parsed } => {
                match verdict(true, status, parsed.is_some()) {
                    Verdict::Ok => parsed.map_or(SubtitleSearchOutcome::Transport, |container| {
                        SubtitleSearchOutcome::Ok(container)
                    }),
                    Verdict::Denied => SubtitleSearchOutcome::Denied,
                    Verdict::Missing => SubtitleSearchOutcome::Missing,
                    Verdict::Transport => SubtitleSearchOutcome::Transport,
                }
            }
        }
    }

    /// `PUT /library/metadata/{rk}/subtitles?key={candidate}` — ask the server to install one
    /// candidate. Answers 200 with an EMPTY body, so there is nothing to parse and nothing that
    /// names the stream it creates; the caller identifies it by diffing the item's subtitle
    /// streams on a LATER read (the install is asynchronous — `docs/pms-api.md` §8).
    ///
    /// `candidate_key` must come from the search being acted on; a stale key is accepted and
    /// silently does nothing.
    pub fn add_subtitle(&self, rating_key: &str, candidate_key: &str) -> SubtitleAddOutcome {
        if !candidate_key_allowed(candidate_key) {
            return SubtitleAddOutcome::BadKey;
        }
        let path = QueryBuilder::new(format!("/library/metadata/{rating_key}/subtitles"))
            .str("key", candidate_key)
            .build();
        let status = self.put(&path);
        match verdict(status >= 0, status, true) {
            Verdict::Ok => SubtitleAddOutcome::Ok,
            Verdict::Denied => SubtitleAddOutcome::Denied,
            Verdict::Missing => SubtitleAddOutcome::Missing,
            Verdict::Transport => SubtitleAddOutcome::Transport,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plex::models::Envelope;

    /// One row as PMS actually answers it, trimmed from the live capture of 2026-10-04 and
    /// ANONYMISED (`docs/shared-servers.md`'s rule: no household's library in a public repo).
    /// `score` is a STRING here because that is what the server sends — the whole point.
    const SEARCH_BODY: &[u8] = br#"{"MediaContainer":{"size":2,"Stream":[
        {"id":1929523,"key":"/library/streams/1929523","streamType":3,"codec":"srt",
         "format":"srt","canAutoSync":false,"language":"Nederlands","languageCode":"nld",
         "languageTag":"nld","providerTitle":"OpenSubtitles","score":"2301",
         "sourceKey":"/library/streams/6923797","title":"a.release.name",
         "displayTitle":"Nederlands","extendedDisplayTitle":"a.release.name (Nederlands SRT)"},
        {"id":1929524,"key":"/library/streams/1929524","streamType":3,"codec":"srt",
         "score":609,"providerTitle":null,"sourceKey":null,"extendedDisplayTitle":null}
    ]}}"#;

    /// The server crashes on a 3-letter code, so this guard is the difference between a refusal
    /// and a 500 on somebody else's machine. It also pins the region fold and the case fold.
    #[test]
    fn a_language_is_folded_to_two_letters_or_refused_outright() {
        for (input, want) in [("nl", "nl"), ("NL", "nl"), ("en-GB", "en"), ("pt_BR", "pt")] {
            assert_eq!(query_language(input).as_deref(), Some(want), "{input:?}");
        }
        // every one of these would be sent verbatim by a naive caller, and `nld`/`eng` are
        // exactly what a subtitle stream reports itself as
        for bad in ["nld", "eng", "", "x", "e1", " ", "english"] {
            assert_eq!(query_language(bad), None, "{bad:?} must never reach the wire");
        }
    }

    /// The flags ride at `0` deliberately; the language is the folded form, never the caller's.
    #[test]
    fn the_search_path_carries_the_folded_language_and_both_flags() {
        assert_eq!(
            search_path("506548", "nl", false, false),
            "/library/metadata/506548/subtitles?language=nl&hearingImpaired=0&forced=0"
        );
        assert_eq!(
            search_path("1", "en", true, true),
            "/library/metadata/1/subtitles?language=en&hearingImpaired=1&forced=1"
        );
    }

    /// A key is interpolated into a path, so it may only ever be the shape the download is for.
    #[test]
    fn a_candidate_key_must_be_a_library_streams_path() {
        assert!(candidate_key_allowed("/library/streams/1929523"));
        assert!(candidate_key_allowed("/library/streams/1929523.srt"));
        for bad in [
            "/library/streams/../../etc/passwd",
            "/library/streams/",
            "/library/streams/12?x=1",
            "/library/parts/5",
            "https://evil.example/library/streams/1",
            "",
        ] {
            assert!(!candidate_key_allowed(bad), "{bad:?} must be refused");
        }
    }

    /// The four answers the UI presents differently, and the two that collapse into one retryable
    /// fault. A 2xx whose body would not parse is a FAULT, never an empty answer.
    #[test]
    fn the_verdict_separates_denied_missing_and_transport() {
        assert_eq!(verdict(true, 200, true), Verdict::Ok);
        assert_eq!(verdict(true, 204, true), Verdict::Ok);
        assert_eq!(verdict(true, 401, true), Verdict::Denied);
        assert_eq!(verdict(true, 403, true), Verdict::Denied);
        assert_eq!(verdict(true, 404, true), Verdict::Missing);
        assert_eq!(verdict(true, 500, true), Verdict::Transport);
        assert_eq!(verdict(true, 200, false), Verdict::Transport);
        assert_eq!(verdict(false, -1, true), Verdict::Transport);
    }

    /// **The blast-radius test.** `score` arrives as a string on every real row; a strict field
    /// would fail the whole container and the screen would read "no results" instead of a fault.
    ///
    /// The second row carries a NUMERIC score and an explicit `null` in the three fields this
    /// change added, because `#[serde(default)]` covers an ABSENT field and does nothing for one
    /// that is present and null. The live capture showed no nulls on this endpoint, so the nulls
    /// here are insurance against a field PMS starts sending empty — deliberately confined to the
    /// fields this module owns rather than asserting a tolerance the shared `Stream` never had.
    #[test]
    fn a_string_score_parses_and_a_null_field_costs_only_that_field() {
        let container = serde_json::from_slice::<Envelope>(SEARCH_BODY)
            .expect("the lenient adapters carry a string score and the nulls this module added")
            .media_container;
        let hits = &container.stream;
        assert_eq!(hits.len(), 2, "both rows survive");

        assert_eq!(hits[0].score, 2301, "a STRING score reads as the number");
        assert_eq!(hits[0].provider_title, "OpenSubtitles");
        assert_eq!(hits[0].key, "/library/streams/1929523");
        assert_eq!(hits[0].format, "srt");
        assert_eq!(hits[0].source_key, "/library/streams/6923797");
        assert_eq!(hits[0].stream_type, 3);
        assert!(!hits[0].can_auto_sync);

        assert_eq!(hits[1].score, 609, "a NUMERIC score reads the same way");
        assert_eq!(hits[1].provider_title, "", "a null costs its own field only");
        assert_eq!(hits[1].source_key, "");
        assert_eq!(hits[1].extended_display_title, "");
        assert_eq!(hits[1].title, "", "an ABSENT field is the ordinary default");
        assert_eq!(hits[1].id, 1929524, "and the row's other fields survive the nulls");
    }
}
