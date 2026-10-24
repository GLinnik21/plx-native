//! `discover.provider.plex.tv` — the plex.tv **metadata provider**, i.e. the biography half of a
//! person that the local PMS simply does not have.
//!
//! This is a THIRD service, beside the PMS `Client` (raw socket, local, `library.rs`) and the
//! plex.tv account API (`account.rs`). It shares the account API's transport and identity headers
//! exactly — same [`plx_net::net`] libcurl HTTPS (DNS + TLS, which the raw PMS socket has neither of),
//! same `X-Plex-*` header set — so it is written as an `impl AccountClient` block here rather than
//! as a fourth client type, the same way `library.rs`/`hubs.rs` add `impl Client` blocks to the PMS
//! client. Every call is **blocking**; run it on a worker thread.
//!
//! **Why this exists.** `GET /library/people/{id}` on the LOCAL server returns only the tag record
//! (`{id, filter, tag, tagType, tagKey, thumb}`) — no summary, no dates. That fact was right, and
//! for a while it was recorded as "Plex has no biography", which was wrong: the biography lives
//! here. Verified live 2026-07-29 against Idina Menzel and Peter Sallis.
//!
//! Three wire facts worth keeping, each of which cost a probe:
//! * **Only the `tagKey` guid addresses a person here.** The numeric `Tag::id` PMS also accepts for
//!   `/library/people/{id}/media` returns `404 {"message":"Invalid value provided for metadataId!"}`
//!   — the two id spaces are the local library's and plex.tv's, and only the guid crosses.
//! * **`Accept: application/json` is load-bearing.** Without it the provider answers **XML**
//!   (`content-type: application/xml`), exactly like PMS does — [`AccountClient`]'s header set
//!   already sends it, which is one more reason to ride that client rather than hand-roll one.
//! * **An unknown person is a 200, not a 404** — `totalSize:0` with no `Metadata`. That is an
//!   ANSWER ("plex.tv has never heard of them"), not a failure, and [`AccountClient::person_profile`]
//!   maps it to a DEFAULT profile so the caller settles instead of retrying forever.
//!
//! **Where this host lives is not decided here.** [`discover`] (`account.rs`, beside `plex_tv()`) is the
//! ONE base, shared with the Watchlist (`watchlist.rs`): the real provider, or in a `devtriggers`
//! build the loopback stand-in named by `plxnative-discover` — or, with that unset, the one
//! `plxnative-plextv` already redirects plex.tv to, since the mock answers both on one listener — and
//! a dev-token boot (`plxnative-token`) with neither armed REFUSES these two reads, logged, instead
//! of dialling the real provider with an identity that has no account behind it. That is a standing
//! decision (fail closed; see `account::permit_read` for the cost: such a boot shows no biography and
//! no filmography credits). Both routes are built by [`person_profile_url`] / [`person_credits_url`]
//! and the gate sits in front of each request; `tests/mock_pms.py`'s Discover stand-in answers them
//! with synthetic data. The refusal is fixed for the life of the process, so the person store asks
//! [`person_profile_refused`] / [`person_credits_refused`] BEFORE it launches the read and settles its
//! page on a refusal rather than retrying it as a failure.
//!
//! The token is optional here (the endpoint answered 200 unauthenticated in the same probe), but we
//! send whatever the session holds anyway — an unauthenticated read is not a promise plex.tv has
//! made, and `AccountClient` already carries the token when there is one.
//!
//! **The filmography, and why it took a SCREEN rather than a fetch.** `GET
//! {discover()}/library/people/{tagKey}/credits` answers 200 with `CreditGroup[]` —
//! `[{type,title,Credit:[{order, role, Metadata{…}}]}]`, e.g. Peter Sallis's `actor`(222) /
//! `appeared`(21) / `other`(3). This module doc used to say it was deliberately unmodelled, and
//! the reason it gave is still the whole design constraint rather than an objection that has been
//! withdrawn: those `Metadata` rows are **Discover** items, so their `ratingKey` is a plex.tv guid
//! and their `thumb` an `image.tmdb.org` URL, and nothing in them opens a local detail page by
//! itself. What made it shippable is `plx_ui::filmography` — a screen that says which credits
//! you actually hold, by joining each row's `guid` against the `/media` answers the person page
//! already has, and presses through to a detail page on exactly those. A row with no server behind
//! it is a provider fact and draws no chevron.
//!
//! Two counts here are NOT interchangeable, and spending the wrong one is a promise the list cannot
//! keep. [`CreditType`] (on the PROFILE) says 1745 actor credits; this endpoint returns 222. The
//! tabs and the Filmography entry row spend the GROUP's own count — the number of rows that
//! actually exist to be scrolled — never the profile's.
//!
//! **The container key is `CreditGroup`, and the group's own shape is not what the name suggests.**
//! Measured 2026-09-06 against a real response: `MediaContainer.CreditGroup[]`, each group carrying
//! `title` ("Actor") + `type` ("actor") + `Credit[]` — and **no `size`**, so a group's count IS its
//! row count. Each `Credit` is `{order, role, Metadata}` where `Metadata` is ONE item, and that item
//! carries **no `guid`**: its `ratingKey` is the bare catalog ID that PMS spells
//! `plex://movie/<id>`. See [`CreditItem::rating_key`], which is the join key, and
//! [`CreditItem::thumb`], which is an absolute URL on another host.
//!
//! **And unlike the profile, this call REQUIRES a token** (measured 2026-09-05: no `X-Plex-Token`
//! → `401 {"error":"Unauthorized","message":"You must provide a token!"}`, where
//! `/library/people/{guid}` is recorded above as answering 200 unauthenticated). The token that
//! satisfies it is the plex.tv ACCOUNT token, which is what makes the filmography unreachable on
//! any automated boot: those sign in with `/tmp/plxnative-token`, a *server* token, and leave
//! `Session::account_token` empty. That is the same wall `/tmp/plxnative-personbio` exists for, one
//! step further along — the biography degrades to a blank line, the filmography degrades to
//! nothing at all — and it is why `/tmp/plxnative-personcredits` had to be written.
use super::account::{discover, discover_read_allowed, AccountClient};
use super::models::de_i64;
use serde::Deserialize;

/// `{discover()}/library/people/{guid}` — the one place the profile URL is spelled, on the base the
/// Watchlist shares ([`discover`]): the real provider, or the dev build's loopback stand-in.
fn person_profile_url(guid: &str) -> String {
    format!("{}/library/people/{guid}", discover())
}

/// `{discover()}/library/people/{guid}/credits` — the one place the filmography URL is spelled.
fn person_credits_url(guid: &str) -> String {
    format!("{}/library/people/{guid}/credits", discover())
}

/// Would [`AccountClient::person_profile`] refuse the read for `guid` before building a request?
///
/// **A refusal is the END of the question, not a failure.** It is fixed at boot (the trigger files
/// and the dev-token boot are read once), so the person store asks this on its own thread, before it
/// spawns a worker, and settles the page when it is `true`; the read is never retried. It goes
/// through the same gate the request does ([`discover_read_allowed`]), so the refusal is logged once
/// per endpoint shape here as well. A release build never refuses.
pub fn person_profile_refused(guid: &str) -> bool {
    !discover_read_allowed(&person_profile_url(guid))
}

/// [`person_profile_refused`] for the filmography read.
pub fn person_credits_refused(guid: &str) -> bool {
    !discover_read_allowed(&person_credits_url(guid))
}

impl AccountClient {
    /// GET {discover()}/library/people/{guid} — one person's biography record.
    ///
    /// `guid` is the **`tagKey`** (`"5d77682aeb5d26001f1de4b0"`), never the numeric tag id (see the
    /// module docs).
    ///
    /// `None` = the request FAILED (offline, TLS, timeout, unparseable body) — the caller should
    /// back off and retry — or was REFUSED by the dev-token gate (the same `None`: a caller that
    /// must not retry a refusal asks [`person_profile_refused`] first, as the person store does).
    /// A person plex.tv has never heard of is NOT a failure: the provider answers
    /// 200 with an empty container, and that comes back here as a **default (all-empty) profile**,
    /// so the caller settles on it, stops retrying, and the page degrades to portrait + name.
    pub fn person_profile(&self, guid: &str) -> Option<PersonProfile> {
        let url = person_profile_url(guid);
        if !discover_read_allowed(&url) {
            return None;
        }
        let env: PersonEnvelope = self.get(&url)?;
        Some(
            env.media_container
                .metadata
                .into_iter()
                .next()
                .unwrap_or_default(),
        )
    }

    /// GET {discover()}/library/people/{guid}/credits — the person's FILMOGRAPHY, as the provider's
    /// own departments.
    ///
    /// Same `guid` rule, same `Accept` rule and the same three-way answer as
    /// [`AccountClient::person_profile`], which is why they sit together: `None` is a FAILURE the
    /// caller must retry, and an EMPTY vector is the answer "plex.tv has no credits for them" —
    /// which the person page settles on rather than spinning against.
    ///
    /// It is one request for the whole filmography (222 rows for Peter Sallis), not a page per
    /// department: the provider sends every group in one body, and the screen's tabs are those
    /// groups.
    pub fn person_credits(&self, guid: &str) -> Option<Vec<CreditGroup>> {
        let url = person_credits_url(guid);
        if !discover_read_allowed(&url) {
            return None;
        }
        let env: CreditsEnvelope = self.get(&url)?;
        // **A body that carried NO recognized container key is a FAILURE, not an empty career** —
        // and keeping those apart is the whole reason [`CreditsContainer`]'s fields are `Option`
        // rather than `#[serde(default)]` vectors.
        //
        // Every field here defaults, so an unexpected or drifted shape deserializes perfectly into
        // an empty vector. Returned as `Some(vec![])` that is indistinguishable from "plex.tv has
        // no credits for them": the store marks the person `credited`, stops retrying, and the
        // Filmography entry never appears — a silent, permanent, unreportable wrong answer on a
        // schema this repo has never measured. Answering `None` instead puts it on the ONE path the
        // crate already has for a body it cannot read: the caller backs off and retries, exactly as
        // it does for a transport error, and the retries are visible in the log. That is the same
        // bargain [`AccountClient::person_profile`] strikes for an unparseable body, and it is the
        // conservative direction — a page that keeps asking is recoverable, a page that has decided
        // the person has no career is not.
        let Some(groups) = env.media_container.credit_group else {
            plx_base::eventlog::log(&format!(
                "person: credits guid={guid} — 200 with no CreditGroup container; treating as a \
                 failure, not an empty career"
            ));
            return None;
        };
        plx_base::eventlog::log(&format!(
            "person: credits groups={} rows={}",
            groups.len(),
            groups.iter().map(|g| g.credits.len()).sum::<usize>()
        ));
        Some(groups)
    }
}

// ---- serde DTOs (only the fields the page consumes; all optional to tolerate shape drift) ----

#[derive(Deserialize, Default)]
struct PersonEnvelope {
    #[serde(rename = "MediaContainer", default)]
    media_container: PersonContainer,
}

#[derive(Deserialize, Default)]
struct PersonContainer {
    #[serde(rename = "Metadata", default)]
    metadata: Vec<PersonProfile>,
}

/// One person as plex.tv knows them. Every field is optional: a living person has no
/// [`died_at`](PersonProfile::died_at), a person plex.tv has no record for has nothing at all, and
/// the page is composed to read as finished in both cases.
///
/// Deliberately NOT modelled: `slug` / `ratingKey` / `metadataId` / `Image[]` / `External[]`.
/// `External[]` is the social handles (`facebook`/`instagram`/`twitter`) — see the person page's
/// module docs for why a TV does not show them. Serde ignores unknown fields, so adding one back is
/// a field, not a migration.
#[derive(Deserialize, Default)]
#[derive(serde::Serialize)]
pub struct PersonProfile {
    #[serde(default)]
    pub title: String,
    /// The biography — the whole reason this endpoint is called. Several paragraphs of prose,
    /// `\n\n`-separated, usually ending in a Wikipedia CC-BY-SA attribution sentence.
    #[serde(default)]
    pub summary: String,
    /// ISO `YYYY-MM-DD`. Absent → unknown, never "not born".
    #[serde(rename = "bornAt", default)]
    pub born_at: String,
    /// ISO `YYYY-MM-DD`, present only for someone who has died.
    #[serde(rename = "diedAt", default)]
    pub died_at: String,
    #[serde(rename = "birthPlace", default)]
    pub birth_place: String,
    /// The single department Plex leads with ("Acting"). The roles LINE comes from
    /// [`credit_types`](PersonProfile::credit_types) instead — it is the full list.
    #[serde(rename = "knownFor", default)]
    pub known_for: String,
    /// Headshot on `metadata-static.plex.tv`. Usually the same URL the local `Role[]` row carries,
    /// so the page keeps drawing the one it was handed rather than swapping textures mid-fetch.
    #[serde(default)]
    pub thumb: String,
    /// The person's departments, wire order = most credits first: `[{type:"actor",title:"Actor"},
    /// {type:"producer",title:"Producer"}, …]`. This is Plex's "Actor, Producer" roles line, and
    /// (via a `count` we deliberately do not model yet) the counts its Filmography tabs show.
    #[serde(rename = "CreditType", default)]
    pub credit_types: Vec<CreditType>,
}

/// One department a person is credited in. `title` is the display form — except when the provider
/// has no display name for a department, where it repeats the raw `type` verbatim
/// (`"costume-makeup"`, seen live on Peter Sallis). `crate::person::roles_line` is what cleans that
/// up; do not print `title` raw.
#[derive(Deserialize, Default)]
#[derive(serde::Serialize)]
pub struct CreditType {
    #[serde(rename = "type", default)]
    pub kind: String,
    #[serde(default)]
    pub title: String,
}

// ---- the filmography ---------------------------------------------------------------------------

#[derive(Deserialize, Default)]
struct CreditsEnvelope {
    #[serde(rename = "MediaContainer", default)]
    media_container: CreditsContainer,
}

/// **One key, measured** — `MediaContainer.CreditGroup` (device log 2026-09-06,
/// `person: credits key=CreditGroup groups=3 rows=154`). It was modelled as two `Option` fields,
/// `CreditGroup` and `Metadata`, precisely so the first real response could say which; it did, and
/// the loser is gone, as that note promised.
///
/// It stays an `Option` rather than a `#[serde(default)]` vector for the half of the reasoning
/// that was never about the spelling: `None` vs `Some(vec![])` is what lets
/// [`AccountClient::person_credits`] tell a body it could not read from a person with no credits.
#[derive(Deserialize, Default)]
struct CreditsContainer {
    #[serde(rename = "CreditGroup")]
    credit_group: Option<Vec<CreditGroup>>,
}

/// One DEPARTMENT of a person's filmography — `actor`, `appeared`, `other`, `writer`, … in the
/// provider's own order, which is most-credits-first.
///
/// `title` is the display form and repeats the raw `type` slug when the provider has no display
/// name for a department (`"costume-makeup"`, live on Peter Sallis) — exactly the trap
/// [`CreditType`] carries, and it is cleaned by the same [`crate::person::pretty_department`].
#[derive(Deserialize, Default)]
#[derive(serde::Serialize)]
pub struct CreditGroup {
    #[serde(rename = "type", default)]
    pub kind: String,
    #[serde(default)]
    pub title: String,
    /// The group's own row count where the provider states one, `0` where it does not — in which
    /// case `Credit.len()` IS the count. Never [`CreditType`]'s number; see the module doc.
    #[serde(default, deserialize_with = "de_i64")]
    pub size: i64,
    #[serde(rename = "Credit", default)]
    pub credits: Vec<Credit>,
}

/// One credit inside a department: what they were in it, and the item itself.
#[derive(Deserialize, Default)]
#[derive(serde::Serialize)]
pub struct Credit {
    /// The billing order the provider sorted the cast by. Modelled because it is on the wire and
    /// costs nothing; the screen sorts by YEAR, so nothing reads it yet.
    #[serde(default, deserialize_with = "de_i64")]
    pub order: i64,
    /// The character or the job — `"Wallace (voice)"`, `"Self"`, `"Soundtrack"`. Empty where the
    /// provider names none, and the row then carries no second line rather than an empty one.
    #[serde(default)]
    pub role: String,
    /// **A single object on the wire, not an array**, which is why this is an `Option`: a credit is
    /// one item. A credit that somehow carries none is dropped by the store rather than drawn as a
    /// row with no title.
    #[serde(rename = "Metadata", default)]
    pub item: Option<CreditItem>,
}

/// The item a credit is FOR — a **Discover** row, not a library one. Its `rating_key` is a plex.tv
/// id and its `thumb` an `image.tmdb.org` URL, so neither addresses anything on a PMS; what does
/// cross is [`guid`](CreditItem::guid), which is the metadata provider's global id and the key both
/// sides of the availability join speak (`plex::Metadata::guid` on the library side).
#[derive(Deserialize, Default)]
#[derive(serde::Serialize)]
pub struct CreditItem {
    #[serde(rename = "type", default)]
    pub kind: String,
    #[serde(default)]
    pub title: String,
    /// `0` is the ONLY absent value PMS and the provider have for a year, so "announced" and "the
    /// metadata is missing" are the same wire fact. The screen states neither: an undated credit
    /// reads as an em dash and sorts to the bottom.
    #[serde(default, deserialize_with = "de_i64")]
    pub year: i64,
    /// **THE join key, and it is NOT a guid** — a bare catalog ID, `5d7768295af944001f1f7477`.
    ///
    /// This endpoint carries **no `guid` field at all** (measured 2026-09-06; the item's whole key
    /// set is `art`/`key`/`originallyAvailableAt`/`publicPagesURL`/`ratingKey`/`slug`/`thumb`/
    /// `title`/`type`/`year`). PMS states the same identity as `plex://movie/<this>`, so the join
    /// is against the TAIL of a local row's guid — `person::guid_tail`. A `guid: String` was
    /// modelled here at first, defaulted to empty on every row, and the availability join it fed
    /// therefore matched NOTHING while every count around it looked healthy: `joinable=5`, 544 rows
    /// drawn, not one of them markable. If a `guid` ever does appear on this wire, prefer it —
    /// but do not model it again without a response in front of you.
    #[serde(rename = "ratingKey", default)]
    pub rating_key: String,
    /// **An ABSOLUTE URL on somebody else's host** — `https://image.tmdb.org/…` or
    /// `https://metadata-static.plex.tv/…`, never a PMS path. That is not a reason to skip the
    /// artwork: `app::adapters::poster::built_key` URL-encodes exactly such a URL into a
    /// `/photo/:/transcode?url=…` request and the SERVER fetches it, which is the same path the
    /// Search screen's `actor` headshots already take (`app/adapters/poster.rs`'s own tests pin it). This
    /// module's first draft asserted the opposite in a comment — "a tmdb URL no PMS transcoder will
    /// serve" — and hard-coded an empty plate on the strength of it, which is why the Filmography
    /// shipped with no posters at all. Nothing was measured before that claim was written down.
    #[serde(default)]
    pub thumb: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::account::{probe_gets, REAL_DISCOVER};

    /// Run `f` with the dev triggers `armed` (name, content) written in a private trigger root.
    /// The resolver is read per call under test (`account::discover_resolved`), so this cannot
    /// leak into `watchlist`'s real-host test or any other thread.
    fn with_triggers<R>(armed: &[(&str, &str)], f: impl FnOnce() -> R) -> R {
        plx_base::devtrig::with_private_triggers(|| {
            for (name, content) in armed {
                std::fs::write(plx_base::devtrig::path(name), content).unwrap();
            }
            f()
        })
    }

    /// The reproduction: a dev build with the Discover stand-in armed must build BOTH person routes
    /// on it, as the Watchlist already does. Both used to be spelled on the real host.
    #[test]
    fn person_urls_are_built_on_the_armed_stand_in() {
        if !plx_base::devtrig::ENABLED {
            return;
        }
        with_triggers(&[("discover", "http://127.0.0.1:41999")], || {
            assert_eq!(person_profile_url("abc"), "http://127.0.0.1:41999/library/people/abc");
            assert_eq!(person_credits_url("abc"), "http://127.0.0.1:41999/library/people/abc/credits");
        });
    }

    /// A build whose plex.tv is redirected must not fall through to the real Discover host: the
    /// person routes follow the `plextv` stand-in when no `discover` trigger is armed, and an
    /// explicit `discover` trigger still wins.
    #[test]
    fn a_redirected_plex_tv_carries_the_person_routes_with_it() {
        if !plx_base::devtrig::ENABLED {
            return;
        }
        with_triggers(&[("plextv", "http://127.0.0.1:41998")], || {
            assert_eq!(person_profile_url("abc"), "http://127.0.0.1:41998/library/people/abc");
            assert_eq!(person_credits_url("abc"), "http://127.0.0.1:41998/library/people/abc/credits");
        });
        with_triggers(&[("plextv", "http://127.0.0.1:41998"), ("discover", "http://localhost:41997")], || {
            assert_eq!(person_profile_url("abc"), "http://localhost:41997/library/people/abc");
        });
        // A trigger that is not a loopback is never followed, wherever it is armed.
        with_triggers(&[("plextv", "https://evil.example"), ("discover", "http://10.0.0.5:1")], || {
            assert_eq!(person_profile_url("abc"), format!("{}/library/people/abc", REAL_DISCOVER));
        });
    }

    /// Nothing armed: the real provider, byte for byte what the inline URLs were.
    #[test]
    fn with_nothing_armed_the_person_urls_are_the_real_provider() {
        with_triggers(&[], || {
            assert_eq!(person_profile_url("5d77682aeb5d26001f1de4b0"),
                "https://discover.provider.plex.tv/library/people/5d77682aeb5d26001f1de4b0");
            assert_eq!(person_credits_url("5d77682aeb5d26001f1de4b0"),
                "https://discover.provider.plex.tv/library/people/5d77682aeb5d26001f1de4b0/credits");
        });
    }

    /// **The ACTUAL request path.** `person_profile` / `person_credits` hand the transport
    /// (`AccountClient::get`) a URL; asked through the public methods, with the transport replaced
    /// by a recorder that opens no socket ([`probe_gets`], so it can dial nothing whatever the code
    /// under test builds), that URL must sit on the stand-in. Spelling the real host in either
    /// method again turns this red, which the URL-helper tests above cannot do.
    #[test]
    fn the_person_reads_hand_the_transport_a_url_on_the_armed_stand_in() {
        if !plx_base::devtrig::ENABLED {
            return;
        }
        let client = AccountClient::new("synthetic-client", None);
        let read = |armed: &[(&str, &str)]| {
            with_triggers(armed, || probe_gets(|| (client.person_profile("abc"), client.person_credits("abc"))).1)
        };
        for (armed, base) in [
            (vec![("discover", "http://127.0.0.1:41999")], "http://127.0.0.1:41999"),
            (vec![("plextv", "http://127.0.0.1:41998")], "http://127.0.0.1:41998"),
            (vec![("plextv", "http://127.0.0.1:41998"), ("discover", "http://localhost:41997")], "http://localhost:41997"),
        ] {
            assert_eq!(read(&armed),
                [format!("{base}/library/people/abc"), format!("{base}/library/people/abc/credits")], "{armed:?}");
        }
        let real = REAL_DISCOVER;
        assert_eq!(read(&[]), [format!("{real}/library/people/abc"), format!("{real}/library/people/abc/credits")],
            "nothing armed: the real provider, and only then");
    }

    /// The refusal as the person store meets it: asked BEFORE a worker exists, through the same gate
    /// the request goes through. A refused read asks the transport nothing (the recorder sees no
    /// URL), and the same two reads go out once a stand-in is armed or the boot is not a dev-token
    /// boot. The decision itself (fail closed) is stated at `account::permit_read`.
    #[test]
    fn a_refused_person_read_is_reported_before_and_sends_nothing() {
        if !plx_base::devtrig::ENABLED {
            return;
        }
        let client = AccountClient::new("synthetic-client", None);
        let ask = |armed: &[(&str, &str)]| with_triggers(armed, || {
            let refused = (person_profile_refused("abc"), person_credits_refused("abc"));
            let (_, urls) = probe_gets(|| (client.person_profile("abc"), client.person_credits("abc")));
            (refused, urls.len())
        });
        assert_eq!(ask(&[("token", "t")]), ((true, true), 0), "dev-token boot, no stand-in: refused, nothing sent");
        assert_eq!(ask(&[("token", "t"), ("discover", "http://127.0.0.1:41999")]), ((false, false), 2));
        assert_eq!(ask(&[("token", "t"), ("plextv", "http://127.0.0.1:41998")]), ((false, false), 2));
        assert_eq!(ask(&[]), ((false, false), 2), "an interactive boot reads");
    }

    /// A dev-token boot (the harness identity: an injected server token, no account) with no
    /// stand-in is REFUSED before a request exists; with a stand-in, or without the token trigger,
    /// the read goes ahead. Asked through the gate rather than `person_profile`, which would reach
    /// libcurl if the gate were wrong.
    #[test]
    fn a_dev_token_boot_never_reaches_the_real_provider() {
        if !plx_base::devtrig::ENABLED {
            return;
        }
        let url = person_profile_url("5d77682aeb5d26001f1de4b0");
        assert!(url.starts_with("https://discover.provider.plex.tv/"), "{url}");
        with_triggers(&[("token", "t")], || {
            assert!(!super::super::account::discover_read_allowed(&url), "refused, no stand-in");
        });
        with_triggers(&[("token", "t"), ("discover", "http://127.0.0.1:41999")], || {
            assert!(super::super::account::discover_read_allowed(&person_profile_url("x")), "stand-in armed");
        });
        with_triggers(&[("token", "t"), ("plextv", "http://127.0.0.1:41998")], || {
            assert!(super::super::account::discover_read_allowed(&person_profile_url("x")), "plextv stand-in armed");
        });
        with_triggers(&[("token", "")], || {
            assert!(super::super::account::discover_read_allowed(&url), "an empty token trigger is no dev-token boot");
        });
        with_triggers(&[], || {
            assert!(super::super::account::discover_read_allowed(&url), "an interactive boot reads");
        });
    }
}
