//! Pure ordering and recovery budget for an already resolved finite offer set. The route owner
//! supplies qualification and stable candidate identities; codec names and API acknowledgments
//! cannot manufacture either. Choosing an offer performs no IO and does not acquire a native lease.

use plx_playback_contract::{BackendKind, Fidelity};

/// Stable within the request's offer set, including reloads and refreshed capability evidence.
/// This names a source/stream/transformation recipe, rather than a transient URL or attempt ID.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OfferId(pub u64);

impl OfferId {
    pub const URI_ORIGINAL: Self = Self(1);
    pub const FEED_ORIGINAL: Self = Self(2);
    pub const FEED_APP_AUDIO: Self = Self(3);
    pub const FEED_PMS_REMUX: Self = Self(4);
    pub const FEED_PMS_ENCODE: Self = Self(5);
    pub const FEED_PMS_HLS: Self = Self(6);
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Qualification { Unknown, Refused, Qualified }

/// Qualification is supplied for this exact source and required controls/release behavior.
/// Exact describes the application's original-stream handoff, not an AVR bitstream guarantee.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Offer {
    pub id: OfferId,
    pub backend: BackendKind,
    pub video: Fidelity,
    pub audio: Fidelity,
    pub qualification: Qualification,
}

impl Offer {
    fn exact(self) -> bool { self.video == Fidelity::Exact && self.audio == Fidelity::Exact }
    fn rank(self) -> (bool, bool) { (!self.exact(), self.backend != BackendKind::Uri) }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Selection {
    Open(Offer),
    /// Some remaining evidence is unknown. Do not report an unsupported codec as a finding.
    Unqualified,
    Exhausted,
}

/// One recovery episode. Keep this value through automatic reloads and capability refreshes.
/// Only an explicitly new episode/request constructs a fresh budget. The supplied catalog is
/// finite and cannot grow as failures arrive, so same-backend PMS fallbacks are finite too.
pub struct BackendPolicy {
    offers: Vec<Offer>,
    tried: Vec<bool>,
    force_original: bool,
    last_backend: Option<BackendKind>,
    crossed: bool,
}

impl BackendPolicy {
    pub fn offer(&self, backend: BackendKind, id: OfferId) -> Option<Offer> {
        self.offers.iter().copied().find(|offer| offer.backend == backend && offer.id == id)
    }

    /// Finite recipes for one selected source and its selected streams. The URI offer begins
    /// unknown: a feed declaration, PMS decision, or native queue answer cannot qualify it.
    /// A resolve qualifies only the Feed recipe it actually prepared.
    pub fn for_request(force_original: bool) -> Self {
        let recipes = [
            (OfferId::URI_ORIGINAL, BackendKind::Uri, Fidelity::Exact, Fidelity::Exact),
            // Auto's native feed can rewrite an original packet (for example P7 or DTS core),
            // so only Forced's no-transform path may claim exact wire fidelity here.
            (OfferId::FEED_ORIGINAL, BackendKind::Feed,
                if force_original { Fidelity::Exact } else { Fidelity::Compatible },
                if force_original { Fidelity::Exact } else { Fidelity::Compatible }),
            (OfferId::FEED_APP_AUDIO, BackendKind::Feed, Fidelity::Exact, Fidelity::Compatible),
            (OfferId::FEED_PMS_REMUX, BackendKind::Feed, Fidelity::Compatible, Fidelity::Compatible),
            (OfferId::FEED_PMS_ENCODE, BackendKind::Feed, Fidelity::Compatible, Fidelity::Compatible),
            (OfferId::FEED_PMS_HLS, BackendKind::Feed, Fidelity::Compatible, Fidelity::Compatible),
        ];
        Self::new(recipes.into_iter().map(|(id, backend, video, audio)| Offer {
            id, backend, video, audio, qualification: Qualification::Unknown,
        }).collect(), force_original)
    }

    /// A newer route plan replaces unspent Feed evidence without replenishing a spent recipe.
    /// No URI evidence is touched by a Feed capability refresh or PMS decision.
    pub fn qualify_planned_feed(&mut self, id: OfferId) {
        for offer in &mut self.offers {
            if offer.backend == BackendKind::Feed {
                offer.qualification = if offer.id == id {
                    Qualification::Qualified
                } else {
                    Qualification::Unknown
                };
            }
        }
    }

    pub fn new(offers: Vec<Offer>, force_original: bool) -> Self {
        Self { tried: vec![false; offers.len()], offers, force_original,
            last_backend: None, crossed: false }
    }

    fn permitted(&self, offer: Offer) -> bool {
        (!self.force_original || offer.exact())
            && !(self.crossed && self.last_backend.is_some_and(|last| last != offer.backend))
    }

    /// Inspect the next choice without consuming it. Busy admission and staged-source expiry
    /// must not spend a candidate or silently grant a different native owner.
    pub fn choose(&self) -> Selection {
        let available = self.offers.iter().copied().zip(&self.tried)
            .filter(|(offer, tried)| !**tried && self.permitted(*offer));
        if let Some(offer) = available.clone()
            .filter(|(offer, _)| offer.qualification == Qualification::Qualified)
            .map(|(offer, _)| offer).min_by_key(|offer| offer.rank()) {
            return Selection::Open(offer);
        }
        if available.into_iter().any(|(offer, _)| offer.qualification == Qualification::Unknown) {
            Selection::Unqualified
        } else { Selection::Exhausted }
    }

    /// The common owner admitted exactly the offered candidate. Refused/busy unrelated Starts
    /// cannot spend this episode. A native open that subsequently fails still spends it.
    pub fn admitted(&mut self, backend: BackendKind, id: OfferId) -> bool {
        let Selection::Open(offer) = self.choose() else { return false };
        if (offer.backend, offer.id) != (backend, id) { return false; }
        if self.last_backend.is_some_and(|last| last != backend) { self.crossed = true; }
        self.last_backend = Some(backend);
        // Duplicate recipes cannot buy retries by appearing twice in a refreshed server offer.
        for (candidate, tried) in self.offers.iter().zip(&mut self.tried) {
            if (candidate.backend, candidate.id) == (backend, id) { *tried = true; }
        }
        true
    }

    /// Replace evidence for an existing recipe, preserving all spent attempts and handoffs.
    /// Unknown/Refused are distinct; a capability change does not append a new candidate.
    pub fn qualify(&mut self, backend: BackendKind, id: OfferId, evidence: Qualification) {
        for candidate in &mut self.offers {
            if (candidate.backend, candidate.id) == (backend, id) {
                candidate.qualification = evidence;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn offer(id: u64, backend: BackendKind, exact: bool) -> Offer {
        Offer { id: OfferId(id), backend, video: Fidelity::Exact,
            audio: if exact { Fidelity::Exact } else { Fidelity::Compatible },
            qualification: Qualification::Qualified }
    }
    fn consume(policy: &mut BackendPolicy, expected: Offer) {
        assert_eq!(policy.choose(), Selection::Open(expected));
        assert!(policy.admitted(expected.backend, expected.id));
    }

    #[test]
    fn original_handoff_precedes_conversion_and_uri_wins_within_each_fidelity() {
        let (uri, feed) = (BackendKind::Uri, BackendKind::Feed);
        let mut policy = BackendPolicy::new(vec![offer(1, uri, false), offer(2, feed, true),
            offer(3, feed, false), offer(4, uri, true)], false);
        consume(&mut policy, offer(4, uri, true));
        consume(&mut policy, offer(2, feed, true));
        // Crossing back would be a second handoff; finite feed offers can still be tried.
        consume(&mut policy, offer(3, feed, false));
        assert_eq!(policy.choose(), Selection::Exhausted);
    }

    #[test]
    fn force_never_silently_converts_and_unknown_is_not_refused() {
        let mut original = offer(1, BackendKind::Uri, true);
        original.qualification = Qualification::Unknown;
        let mut policy = BackendPolicy::new(vec![offer(2, BackendKind::Feed, false), original], true);
        assert_eq!(policy.choose(), Selection::Unqualified);
        policy.qualify(original.backend, original.id, Qualification::Refused);
        assert_eq!(policy.choose(), Selection::Exhausted);
    }

    #[test]
    fn capability_refresh_and_duplicate_offers_cannot_replenish_spent_candidates() {
        let first = offer(1, BackendKind::Uri, true);
        let mut policy = BackendPolicy::new(vec![first, first], false);
        assert_eq!(policy.choose(), policy.choose(), "inspection does not consume admission");
        assert!(!policy.admitted(BackendKind::Feed, first.id));
        consume(&mut policy, first);
        policy.qualify(first.backend, first.id, Qualification::Refused);
        policy.qualify(first.backend, first.id, Qualification::Qualified);
        assert_eq!(policy.choose(), Selection::Exhausted);
    }

    #[test]
    fn no_qualified_uri_offer_preserves_a_qualified_feed_fallback() {
        let mut uri = offer(1, BackendKind::Uri, true);
        uri.qualification = Qualification::Unknown;
        let feed = offer(2, BackendKind::Feed, true);
        let mut policy = BackendPolicy::new(vec![uri, feed], false);
        consume(&mut policy, feed);
        policy.qualify(uri.backend, uri.id, Qualification::Qualified);
        consume(&mut policy, Offer { qualification: Qualification::Qualified, ..uri });
        assert_eq!(policy.choose(), Selection::Exhausted);
    }

    #[test]
    fn production_catalog_never_invents_uri_qualification_or_replenishes_recovery() {
        let mut policy = BackendPolicy::for_request(false);
        policy.qualify_planned_feed(OfferId::FEED_ORIGINAL);
        let Selection::Open(feed) = policy.choose() else { panic!("prepared Feed must open") };
        assert_eq!((feed.backend, feed.id), (BackendKind::Feed, OfferId::FEED_ORIGINAL));
        assert!(policy.admitted(feed.backend, feed.id));

        // A new resolved rung qualifies its own fixed recipe. Refreshing the earlier recipe
        // cannot buy a retry, even when URI remains unknown and its source was never staged.
        policy.qualify_planned_feed(OfferId::FEED_PMS_REMUX);
        let Selection::Open(remux) = policy.choose() else { panic!("remux rung must open") };
        assert_eq!(remux.id, OfferId::FEED_PMS_REMUX);
        assert!(policy.admitted(remux.backend, remux.id));
        policy.qualify_planned_feed(OfferId::FEED_ORIGINAL);
        assert_eq!(policy.choose(), Selection::Unqualified);
    }

    #[test]
    fn qualified_original_uri_can_handoff_from_feed_but_cannot_return() {
        let mut policy = BackendPolicy::for_request(true);
        policy.qualify_planned_feed(OfferId::FEED_ORIGINAL);
        let Selection::Open(feed) = policy.choose() else { panic!("forced feed must open") };
        assert!(policy.admitted(feed.backend, feed.id));
        policy.qualify(BackendKind::Uri, OfferId::URI_ORIGINAL, Qualification::Qualified);
        let Selection::Open(uri) = policy.choose() else { panic!("qualified URI handoff must open") };
        assert_eq!(uri.id, OfferId::URI_ORIGINAL);
        assert!(policy.admitted(uri.backend, uri.id));
        policy.qualify_planned_feed(OfferId::FEED_ORIGINAL);
        assert_eq!(policy.choose(), Selection::Exhausted);
    }
}
