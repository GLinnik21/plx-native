//! `discover.provider.plex.tv` — the plex.tv **metadata provider**, i.e. the biography half of a
//! person that the local PMS simply does not have.
//!
//! This is a THIRD service, beside the PMS `Client` (raw socket, local, `library.rs`) and the
//! plex.tv account API (`account.rs`). It shares the account API's transport and identity headers
//! exactly — same [`crate::net`] libcurl HTTPS (DNS + TLS, which the raw PMS socket has neither of),
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
//! The token is optional here (the endpoint answered 200 unauthenticated in the same probe), but we
//! send whatever the session holds anyway — an unauthenticated read is not a promise plex.tv has
//! made, and `AccountClient` already carries the token when there is one.
//!
//! **Not built: the filmography.** `GET {DISCOVER}/library/people/{tagKey}/credits` exists and
//! answers 200 with `CreditGroup[]` — `[{type,title,Credit:[{order, role, Metadata{…}}]}]`, e.g.
//! Peter Sallis's `actor`(222) / `appeared`(21) / `other`(3). It is deliberately unmodelled,
//! because those `Metadata` rows are **Discover** items: their `ratingKey` is a plex.tv guid and
//! their `thumb` a `image.tmdb.org` URL, so nothing in them opens a local detail page. Showing
//! them needs a screen that can say "not in your library" and a route that does something useful
//! when you press OK — a feature, not a fetch. (Note also that the group counts here are NOT
//! [`CreditType`]'s: that says 1745 actor credits where this returns 222.)
use super::account::AccountClient;
use serde::Deserialize;

const DISCOVER: &str = "https://discover.provider.plex.tv";

impl AccountClient {
    /// GET {DISCOVER}/library/people/{guid} — one person's biography record.
    ///
    /// `guid` is the **`tagKey`** (`"5d77682aeb5d26001f1de4b0"`), never the numeric tag id (see the
    /// module docs).
    ///
    /// `None` = the request FAILED (offline, TLS, timeout, unparseable body) — the caller should
    /// back off and retry. A person plex.tv has never heard of is NOT that: the provider answers
    /// 200 with an empty container, and that comes back here as a **default (all-empty) profile**,
    /// so the caller settles on it, stops retrying, and the page degrades to portrait + name.
    pub fn person_profile(&self, guid: &str) -> Option<PersonProfile> {
        let env: PersonEnvelope = self.get(&format!("{DISCOVER}/library/people/{guid}"))?;
        Some(
            env.media_container
                .metadata
                .into_iter()
                .next()
                .unwrap_or_default(),
        )
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
pub struct CreditType {
    #[serde(rename = "type", default)]
    pub kind: String,
    #[serde(default)]
    pub title: String,
}
