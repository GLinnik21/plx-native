//! **The Plex layer's door onto the origin types, and the credential policy that is about them.**
//!
//! [`Origin`] (scheme + host + port), [`Scheme`], [`ResolvePin`], [`split`], [`url_host`] and
//! [`plex_direct_literal`] are defined in [`plx_net::net::origin`] and re-exported here, so every
//! Plex caller keeps naming `plex::Origin` and `plex::origin::split`. They moved down because the
//! transport — `net`'s request funnel and `stream`'s redirect follower — has to read them, and a
//! transport must not name the layer above it (`docs/module-layers.md`, step L6). Their own
//! documentation, the bracket rule and the `plex.direct` pin rules included, is there.
//!
//! What stays is [`CredentialPolicy`]: whether a credential may ride an origin is a Plex decision
//! (`super::grant` holds the live half), and it only READS an [`Origin`].

pub use plx_net::net::origin::{plex_direct_literal, split, url_host, Origin, ResolvePin, Scheme};

/// **The build's half of "may a credential ride this origin".** The question itself has ONE
/// answer, `super::grant::credential_allowed` (or `grant::allowed_under` in a pure function that
/// receives this value): this policy, OR a live consented plaintext grant for that exact origin
/// (PLX-NATIVE-10). Every caller that used to carry its own `cfg!(feature = "devtriggers")` (the
/// http guard, the curlio media twin, the probe activation gate) now goes through it, and
/// [`CredentialPolicy::build`] is the ONLY place the cfg still lives.
///
/// A store build is [`CredentialPolicy::HttpsOnly`]: by policy a token may only ride a TLS origin
/// (a grant is the one exception, and it lives in `grant`, not here). A
/// developer build (`devtriggers`) is [`CredentialPolicy::AllowPlaintext`]: the rule exists so a
/// lane with no TLS server of its own can still exercise the credentialed path against a plain
/// `dev::DevServer` — unless `/tmp/plxnative-storepolicy` was armed at boot, which
/// gives a developer build the store's `HttpsOnly` so the consent flow (reachable only under it)
/// can be exercised in the sim and on the TV. The trigger only ever tightens, and a store build
/// has no trigger to read. Nothing about WHICH origin is eligible ever depends on where the policy came
/// from — a pure function receives it as a parameter. [`build`](CredentialPolicy::build) itself is
/// no longer just the two discovery live edges (`auth::resolve_roster_live_while`,
/// `auth::probe_profile_resource_live`): the registry asks it too, at every write that admits an
/// origin — `servers::register_origin`, `servers::register_captured_origin_with_connection` and
/// `servers::register_pinned_with_client_id` — so a stored origin is graded by the same rule a
/// discovered one is, and the transport boundary (`http::credential_transport_allowed`) still asks
/// it a third time, per request, as the backstop if either ever admitted one wrongly.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CredentialPolicy {
    /// A store build: only a TLS origin may carry a credential by policy — the one exception, a
    /// live consented plaintext grant, is `super::grant`'s, never this type's.
    HttpsOnly,
    /// A developer build: any origin may, matching every build's behaviour before this type
    /// existed.
    AllowPlaintext,
}

#[cfg(all(feature = "devtriggers", not(test)))]
plx_base::devtrig::latched_flag!(
    /// `/tmp/plxnative-storepolicy` — a developer build takes the store's
    /// [`CredentialPolicy::HttpsOnly`], read once at boot, so the PLX-NATIVE-10 consent flow (which
    /// `AllowPlaintext` never needs) is reachable in the sim and on the TV. Absent from a store
    /// build, and never read by a host test: a stray file on a developer's machine must not change
    /// what the suite grades.
    fn store_policy_forced = "storepolicy";
);
#[cfg(not(all(feature = "devtriggers", not(test))))]
fn store_policy_forced() -> bool {
    false
}

impl CredentialPolicy {
    /// The build's own policy — **the only `cfg!` for this rule in the whole crate.** Every other
    /// caller either receives this value as a parameter (a pure function) or calls this directly
    /// at a live edge; nothing else re-derives it.
    pub fn build() -> Self {
        Self::of_build(cfg!(feature = "devtriggers"), store_policy_forced())
    }

    /// [`build`](Self::build)'s rule, pure: a developer build allows plaintext unless the store
    /// policy was forced; anything else is `HttpsOnly`. Forcing can only tighten.
    pub(crate) const fn of_build(devtriggers: bool, store_forced: bool) -> Self {
        if devtriggers && !store_forced {
            CredentialPolicy::AllowPlaintext
        } else {
            CredentialPolicy::HttpsOnly
        }
    }

    /// May a credential ride `origin` under this policy? TLS always; plaintext only under
    /// [`CredentialPolicy::AllowPlaintext`].
    pub fn may_carry_credential(self, origin: &Origin) -> bool {
        origin.is_tls() || self == CredentialPolicy::AllowPlaintext
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// T1: the store-policy trigger makes a developer build `HttpsOnly`, and nothing can loosen a
    /// store build.
    #[test]
    fn the_store_policy_trigger_only_tightens() {
        assert_eq!(CredentialPolicy::of_build(true, false), CredentialPolicy::AllowPlaintext);
        assert_eq!(CredentialPolicy::of_build(true, true), CredentialPolicy::HttpsOnly);
        assert_eq!(CredentialPolicy::of_build(false, false), CredentialPolicy::HttpsOnly);
        assert_eq!(CredentialPolicy::of_build(false, true), CredentialPolicy::HttpsOnly);
    }
}
