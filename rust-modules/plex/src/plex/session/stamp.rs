//! **The account stamp** a per-account RECORD carries (`profile_prefs`, `track_memory`,
//! `subtitle_offsets`): which account's data the record is.
//!
//! A stamp of the plex.tv account TOKEN (what `grant::account_key` computes, and what these records
//! carried until #830) lives exactly as long as one sign-in, so a person who signed in again read
//! their own memory as a stranger's and the next write deleted it. The stamp follows the OWNER
//! instead, the plex.tv account that administers the Plex Home the TV is signed in to
//! ([`owner_identity`](super::owner_identity), the identity the login-over-another-account check
//! already uses). Three forms, all strings in the existing `account` key:
//!
//! | form | when | example |
//! |---|---|---|
//! | `""` | no account token (signed out, a dev token): nothing is stamped, nothing is seated | |
//! | 16 hex | a token and no nameable owner (the roster is not fetched yet, or names no admin): the token fingerprint, exactly what every older build wrote | `6e75236040ff06d4` |
//! | `o1:` + 16 hex | the roster names the admin: the first eight bytes of SHA-256 over `plx-owner:` and the identity | `o1:19afc47bcea99412` |
//!
//! `o1` is a version tag. The hash is one-way and domain-tagged, like `account_key`; the plex.tv id
//! is a small number every co-sharer of a server sees as its `ownerId`, so this hides nothing the
//! file's other content does not already show, and it is not salted (`client_id` sits in the same
//! public half, so a salt would add nothing). The format is pinned by golden values in the tests:
//! nothing can re-derive an old stamp, so a drift would orphan every record a device has written.
//!
//! **Adoption.** A record stamped by an older build, or in a window in which the owner was not yet
//! known, carries the token fingerprint. [`adoptable_stamps`] names the stamps a session may adopt
//! into its own: the fingerprint of ITS token and the other spelling of ITS roster's admin, and
//! nothing else, only while its own stamp is the owner form. A different owner can therefore never
//! adopt another's records: the only stamp equal to this token's fingerprint was written by a
//! session holding this token, which belongs to one account.
//!
//! Everything here is pure: no IO, no clock, no global.
use super::{owner_identities, owner_identity, Session};
use crate::plex::grant;

/// The domain tag of the owner form (the consent key's is `plx-consent:`).
const OWNER_DOMAIN: &[u8] = b"plx-owner:";

fn owner_form(identity: &str) -> String {
    format!("o1:{}", grant::fingerprint(OWNER_DOMAIN, identity))
}

/// The one stamp a per-account record carries: empty without an account token, the owner form when
/// the roster names its admin, else the token fingerprint (the owner is unknown, which is exactly
/// the behaviour before the owner form existed, never worse).
pub fn account_stamp(session: &Session) -> String {
    if session.account_token.is_empty() {
        return String::new();
    }
    match owner_identity(&session.home_users) {
        Some(identity) => owner_form(&identity),
        None => grant::account_key(&session.account_token),
    }
}

/// Whether `stamp` is in the owner family: `o`, one or more digits (the version), then `:`. A later
/// `o2:` form is foreign to this build but still the owner family, which is what keeps a write made
/// while the owner is unknown from deleting it.
pub fn is_owner_stamp(stamp: &str) -> bool {
    let Some(rest) = stamp.strip_prefix('o') else { return false };
    let Some((version, _)) = rest.split_once(':') else { return false };
    !version.is_empty() && version.bytes().all(|b| b.is_ascii_digit())
}

/// The stamps whose records `session` adopts as its own: the fingerprint of its token and the other
/// spellings of its roster's admin, minus its own stamp. Empty unless the session's stamp is the
/// owner form: with the owner unknown there is nothing to migrate toward.
pub(super) fn adoptable_stamps(session: &Session) -> Vec<String> {
    let own = account_stamp(session);
    if !is_owner_stamp(&own) {
        return Vec::new();
    }
    let mut from = vec![grant::account_key(&session.account_token)];
    from.extend(owner_identities(&session.home_users).iter().map(|identity| owner_form(identity)));
    from.retain(|stamp| *stamp != own);
    let mut unique: Vec<String> = Vec::with_capacity(from.len());
    for stamp in from {
        if !unique.contains(&stamp) {
            unique.push(stamp);
        }
    }
    unique
}

impl Session {
    /// This session with every adoptable record re-stamped to [`account_stamp`]: the records an
    /// older build, or a window in which the owner was not yet known, stamped with this token's
    /// fingerprint (or with the other spelling of this roster's admin). Pure, idempotent, and a
    /// no-op while the owner is unknown. It runs wherever a `Session` is built from storage
    /// (`read_locked`, `read_legacy_filtered_locked`) and in the write funnel
    /// (`save_locked_with_authority`), so every reader sees the adopted stamps and the next write
    /// persists them; nothing writes just to adopt.
    pub(super) fn adopt_stamps(mut self) -> Session {
        let from = adoptable_stamps(&self);
        if !from.is_empty() {
            let to = account_stamp(&self);
            self.prefs.restamp(&from, &to);
        }
        self
    }

    /// Whether [`adopt_stamps`](Self::adopt_stamps) would change this session: a scan, no clone.
    pub(super) fn adoption_pending(&self) -> bool {
        let from = adoptable_stamps(self);
        !from.is_empty() && self.prefs.restamp_pending(&from)
    }
}
