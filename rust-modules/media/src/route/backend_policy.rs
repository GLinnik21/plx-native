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
    pub const URI_PMS_REMUX: Self = Self(7);
    pub const URI_PMS_ENCODE: Self = Self(8);
    pub const URI_PMS_HLS: Self = Self(9);
    /// Original candidate's own progressive PMS remux after an HLS/remux route. It is a
    /// distinct source recipe from the route being replaced, even when both use start.mkv.
    pub const FEED_ORIGINAL_TRIAL_REMUX: Self = Self(10);
    /// A refused audio lane rebuilt with a PMS-converted audio track. The requested source
    /// transformation differs from an ordinary remux even when both copy the video.
    pub const FEED_REFUSAL_AUDIO: Self = Self(11);
    pub const FEED_REFUSAL_VIDEO: Self = Self(12);
    pub const FEED_REFUSAL_BOTH: Self = Self(13);
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
    fn rank(self) -> (u8, bool, u64) {
        // Auto's Feed Original may be wire-compatible rather than bit-exact, yet it is still
        // an original-source handoff and must precede a server conversion.
        let tier = if self.exact() || (self.backend == BackendKind::Feed
            && self.id == OfferId::FEED_ORIGINAL) { 0 }
            else if self.backend == BackendKind::Uri { 1 }
            else { 2 };
        (tier, self.backend != BackendKind::Uri, self.id.0)
    }
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
    last_offer: Option<(BackendKind, OfferId)>,
    crossed: bool,
    rollback_used: bool,
}

impl BackendPolicy {
    pub fn offer(&self, backend: BackendKind, id: OfferId) -> Option<Offer> {
        self.offers.iter().copied().find(|offer| offer.backend == backend && offer.id == id)
    }

    /// A physical seek or foreground reload may reopen only the recipe most recently admitted
    /// on this backend. A handoff to the other backend retires this exception, even though the
    /// old recipe remains spent in the finite recovery catalog.
    pub fn can_reopen(&self, backend: BackendKind, id: OfferId) -> bool {
        self.last_offer == Some((backend, id))
    }

    /// A failed Original trial may restore one previously admitted recipe on the same backend.
    /// This is a bounded rollback, not another recovery candidate or a return handoff.
    pub fn can_rollback_existing(&self, backend: BackendKind, id: OfferId) -> bool {
        !self.rollback_used && self.last_backend == Some(backend)
            && self.last_offer != Some((backend, id))
            && self.offers.iter().zip(&self.tried).any(|(offer, tried)|
                *tried && offer.backend == backend && offer.id == id)
    }

    pub fn admit_rollback_existing(&mut self, backend: BackendKind, id: OfferId) -> bool {
        if !self.can_rollback_existing(backend, id) { return false; }
        self.rollback_used = true;
        self.last_offer = Some((backend, id));
        true
    }

    /// A deliberate quality transition (the Auto worker's fallback to HLS and its return to the
    /// Original, which its own probes, dwell and switch budget govern) is no recovery candidate:
    /// it may move the route between recipes this request already admitted on this backend, any
    /// number of times, because it spends nothing. A recipe never admitted, or one on the other
    /// backend, is still a candidate and is spent once.
    pub fn can_revisit(&self, backend: BackendKind, id: OfferId) -> bool {
        self.last_backend == Some(backend)
            && self.offers.iter().zip(&self.tried).any(|(offer, tried)|
                *tried && offer.backend == backend && offer.id == id)
    }

    /// The common owner admitted a [`can_revisit`](Self::can_revisit) recipe: it becomes the last
    /// one admitted, so a seek or a foreground restore reopens it and a failed trial rolls back
    /// from it. No budget moves.
    pub fn admit_revisit(&mut self, backend: BackendKind, id: OfferId) -> bool {
        if !self.can_revisit(backend, id) { return false; }
        self.last_offer = Some((backend, id));
        true
    }

    /// Finite recipes for one selected source and its selected streams. The URI offer begins
    /// unknown: a feed declaration, PMS decision, or native queue answer cannot qualify it.
    /// A resolve qualifies only the Feed recipe it actually prepared.
    pub fn for_request(force_original: bool) -> Self {
        let recipes = [
            (OfferId::URI_ORIGINAL, BackendKind::Uri, Fidelity::Exact, Fidelity::Exact),
            // Auto's native feed can rewrite an original packet (for example P7 or DTS core) and its
            // app decode changes the audio, so each is the wire-compatible handoff. Forced converts
            // nothing on a server (the planner offers it no PMS recipe), so for Forced's policy its
            // original handoff and the app-decoded audio count as exact: those are the recipes it admits.
            (OfferId::FEED_ORIGINAL, BackendKind::Feed,
                if force_original { Fidelity::Exact } else { Fidelity::Compatible },
                if force_original { Fidelity::Exact } else { Fidelity::Compatible }),
            (OfferId::FEED_APP_AUDIO, BackendKind::Feed, Fidelity::Exact,
                if force_original { Fidelity::Exact } else { Fidelity::Compatible }),
            (OfferId::FEED_PMS_REMUX, BackendKind::Feed, Fidelity::Compatible, Fidelity::Compatible),
            (OfferId::FEED_PMS_ENCODE, BackendKind::Feed, Fidelity::Compatible, Fidelity::Compatible),
            (OfferId::FEED_PMS_HLS, BackendKind::Feed, Fidelity::Compatible, Fidelity::Compatible),
            (OfferId::URI_PMS_REMUX, BackendKind::Uri, Fidelity::Compatible, Fidelity::Compatible),
            (OfferId::URI_PMS_ENCODE, BackendKind::Uri, Fidelity::Compatible, Fidelity::Compatible),
            (OfferId::URI_PMS_HLS, BackendKind::Uri, Fidelity::Compatible, Fidelity::Compatible),
            (OfferId::FEED_ORIGINAL_TRIAL_REMUX, BackendKind::Feed,
                Fidelity::Compatible, Fidelity::Compatible),
            (OfferId::FEED_REFUSAL_AUDIO, BackendKind::Feed,
                Fidelity::Compatible, Fidelity::Compatible),
            (OfferId::FEED_REFUSAL_VIDEO, BackendKind::Feed,
                Fidelity::Compatible, Fidelity::Compatible),
            (OfferId::FEED_REFUSAL_BOTH, BackendKind::Feed,
                Fidelity::Compatible, Fidelity::Compatible),
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
            last_backend: None, last_offer: None, crossed: false, rollback_used: false }
    }

    fn permitted(&self, offer: Offer) -> bool {
        (!self.force_original || offer.exact())
            && !(self.crossed && self.last_backend.is_some_and(|last| last != offer.backend))
    }

    /// Whether a backend still has a finite recipe the next resolve may qualify. Unknown
    /// evidence is possible, never proof that a particular native source is supported.
    /// A refused or spent recipe and a forbidden second backend crossing cannot rescue playback.
    pub fn has_remaining_backend(&self, backend: BackendKind) -> bool {
        self.offers.iter().zip(&self.tried).any(|(offer, tried)|
            offer.backend == backend && !*tried && self.permitted(*offer)
                && offer.qualification != Qualification::Refused)
    }

    /// Whether one fixed recipe may be source-qualified by the next resolve. This is a
    /// prospect, not admission; it excludes a spent/refused recipe even when another recipe on
    /// the same backend remains available. A native seek/reload uses `can_reopen` separately.
    pub fn may_qualify(&self, backend: BackendKind, id: OfferId) -> bool {
        self.offers.iter().zip(&self.tried).any(|(offer, tried)|
            (offer.backend, offer.id) == (backend, id) && !*tried
                && offer.qualification != Qualification::Refused && self.permitted(*offer))
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
        self.last_offer = Some((backend, id));
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
    fn a_viewers_opt_in_qualifies_the_uri_original_and_off_keeps_the_feed_order() {
        let uri_original = |policy: &BackendPolicy| policy.offer(BackendKind::Uri, OfferId::URI_ORIGINAL).unwrap();
        // Off: the resolve qualifies only the Feed recipe it prepared; the URI offer stays Unknown.
        let mut off = BackendPolicy::for_request(false);
        off.qualify_planned_feed(OfferId::FEED_ORIGINAL);
        assert_eq!(uri_original(&off).qualification, Qualification::Unknown);
        let Selection::Open(chosen) = off.choose() else { panic!("Feed is the only qualified offer") };
        assert_eq!((chosen.backend, chosen.id), (BackendKind::Feed, OfferId::FEED_ORIGINAL));
        // On: the production qualifier's verdict qualifies the URI original, which then wins its tier.
        let mut on = BackendPolicy::for_request(false);
        on.qualify_planned_feed(OfferId::FEED_ORIGINAL);
        on.qualify(BackendKind::Uri, OfferId::URI_ORIGINAL, Qualification::Qualified);
        let Selection::Open(chosen) = on.choose() else { panic!("a qualified offer is open") };
        assert_eq!((chosen.backend, chosen.id), (BackendKind::Uri, OfferId::URI_ORIGINAL));
        assert!(on.admitted(BackendKind::Uri, OfferId::URI_ORIGINAL));
        // A URI failure takes the one handoff to Feed, and the opt-in buys no second crossing.
        on.qualify(BackendKind::Uri, OfferId::URI_ORIGINAL, Qualification::Refused);
        let Selection::Open(chosen) = on.choose() else { panic!("the handoff is open") };
        assert_eq!((chosen.backend, chosen.id), (BackendKind::Feed, OfferId::FEED_ORIGINAL));
        assert!(on.admitted(BackendKind::Feed, OfferId::FEED_ORIGINAL));
        on.qualify(BackendKind::Uri, OfferId::URI_PMS_REMUX, Qualification::Qualified);
        assert!(!matches!(on.choose(), Selection::Open(_)), "no second crossing");
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
    fn a_revisit_moves_between_admitted_recipes_without_spending_anything() {
        let (uri, feed) = (BackendKind::Uri, BackendKind::Feed);
        let mut policy = BackendPolicy::new(vec![offer(1, feed, true), offer(2, feed, false),
            offer(3, feed, false)], false);
        assert!(!policy.can_revisit(feed, OfferId(1)), "nothing admitted yet");
        consume(&mut policy, offer(1, feed, true));
        assert!(!policy.can_revisit(feed, OfferId(2)), "a recipe nothing admitted is a candidate");
        assert!(!policy.admit_revisit(feed, OfferId(2)));
        consume(&mut policy, offer(2, feed, false));
        for _ in 0..2 {
            for id in [OfferId(1), OfferId(2)] {
                assert!(policy.admit_revisit(feed, id));
                assert!(policy.can_reopen(feed, id), "the revisited recipe is the last admitted");
            }
        }
        assert_eq!(policy.choose(), Selection::Open(offer(3, feed, false)),
            "revisiting spent nothing: the unspent recipe is still the next candidate");
        assert!(!policy.can_revisit(uri, OfferId(1)), "the other backend was never admitted");
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
    fn source_eligibility_and_rank_keep_exact_feed_before_compatible_uri_before_feed_conversion() {
        let mut policy = BackendPolicy::for_request(false);
        policy.qualify_planned_feed(OfferId::FEED_PMS_REMUX);
        policy.qualify(BackendKind::Uri, OfferId::URI_PMS_REMUX, Qualification::Qualified);
        assert!(matches!(policy.choose(), Selection::Open(offer)
            if (offer.backend, offer.id) == (BackendKind::Uri, OfferId::URI_PMS_REMUX)));
        policy.qualify_planned_feed(OfferId::FEED_ORIGINAL);
        assert!(matches!(policy.choose(), Selection::Open(offer)
            if (offer.backend, offer.id) == (BackendKind::Feed, OfferId::FEED_ORIGINAL)));
        policy.qualify_planned_feed(OfferId::FEED_PMS_REMUX);
        assert!(policy.admitted(BackendKind::Uri, OfferId::URI_PMS_REMUX));
        assert!(!policy.may_qualify(BackendKind::Uri, OfferId::URI_PMS_REMUX),
            "a refreshed URL cannot refill this spent source recipe");
        assert!(policy.may_qualify(BackendKind::Uri, OfferId::URI_PMS_ENCODE),
            "another finite compatible recipe remains independently eligible");

        let forced = BackendPolicy::for_request(true);
        assert!(!forced.may_qualify(BackendKind::Uri, OfferId::URI_PMS_REMUX));
        assert!(forced.may_qualify(BackendKind::Uri, OfferId::URI_ORIGINAL));
    }

    #[test]
    fn rollback_restores_one_prior_same_backend_recipe_without_replenishing_recovery() {
        let mut policy = BackendPolicy::for_request(false);
        policy.qualify_planned_feed(OfferId::FEED_PMS_HLS);
        assert!(policy.admitted(BackendKind::Feed, OfferId::FEED_PMS_HLS));
        policy.qualify_planned_feed(OfferId::FEED_ORIGINAL);
        assert!(policy.admitted(BackendKind::Feed, OfferId::FEED_ORIGINAL));
        assert!(!policy.can_reopen(BackendKind::Feed, OfferId::FEED_PMS_HLS));
        assert!(policy.admit_rollback_existing(BackendKind::Feed, OfferId::FEED_PMS_HLS));
        assert!(!policy.can_reopen(BackendKind::Feed, OfferId::FEED_ORIGINAL));
        assert!(!policy.admit_rollback_existing(BackendKind::Feed, OfferId::FEED_ORIGINAL));
        assert_eq!(policy.choose(), Selection::Unqualified);
    }

    #[test]
    fn original_trial_remux_is_distinct_from_spent_initial_remux_and_rollback_is_bounded() {
        let mut policy = BackendPolicy::for_request(false);
        policy.qualify_planned_feed(OfferId::FEED_PMS_REMUX);
        assert!(policy.admitted(BackendKind::Feed, OfferId::FEED_PMS_REMUX));
        policy.qualify_planned_feed(OfferId::FEED_ORIGINAL_TRIAL_REMUX);
        assert!(matches!(policy.choose(), Selection::Open(offer)
            if offer.id == OfferId::FEED_ORIGINAL_TRIAL_REMUX));
        assert!(policy.admitted(BackendKind::Feed, OfferId::FEED_ORIGINAL_TRIAL_REMUX));
        assert!(policy.can_reopen(BackendKind::Feed, OfferId::FEED_ORIGINAL_TRIAL_REMUX));
        policy.qualify_planned_feed(OfferId::FEED_ORIGINAL_TRIAL_REMUX);
        assert_eq!(policy.choose(), Selection::Unqualified,
            "a refreshed URL for the same Original source recipe cannot buy a second trial");
        policy.qualify_planned_feed(OfferId::FEED_PMS_REMUX);
        assert!(policy.admit_rollback_existing(BackendKind::Feed, OfferId::FEED_PMS_REMUX));
        assert!(!policy.admit_rollback_existing(BackendKind::Feed, OfferId::FEED_ORIGINAL_TRIAL_REMUX));
        assert_eq!(policy.choose(), Selection::Unqualified,
            "neither remux recipe regains a recovery open after rollback");
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

    #[test]
    fn force_admits_the_app_decoded_audio_recipe_and_no_server_conversion() {
        let mut forced = BackendPolicy::for_request(true);
        forced.qualify_planned_feed(OfferId::FEED_APP_AUDIO);
        assert_eq!(forced.choose(), Selection::Open(forced.offer(BackendKind::Feed, OfferId::FEED_APP_AUDIO)
            .expect("the catalog lists it")), "main's Force decodes a stereo lossy track in the app");
        assert!(forced.admitted(BackendKind::Feed, OfferId::FEED_APP_AUDIO));
        for id in [OfferId::FEED_PMS_REMUX, OfferId::FEED_PMS_ENCODE, OfferId::FEED_PMS_HLS] {
            assert!(!forced.may_qualify(BackendKind::Feed, id), "Force converts nothing on a server");
        }
    }

    #[test]
    fn remaining_feed_prospect_respects_force_spent_recipes_and_one_crossing() {
        let mut forced = BackendPolicy::for_request(true);
        forced.qualify(BackendKind::Uri, OfferId::URI_ORIGINAL, Qualification::Qualified);
        assert!(forced.admitted(BackendKind::Uri, OfferId::URI_ORIGINAL));
        assert!(forced.has_remaining_backend(BackendKind::Feed),
            "the unknown exact Feed original may be resolved");
        forced.qualify(BackendKind::Feed, OfferId::FEED_ORIGINAL, Qualification::Refused);
        assert!(forced.has_remaining_backend(BackendKind::Feed),
            "the app-decoded audio is still Force's own recipe");
        forced.qualify(BackendKind::Feed, OfferId::FEED_APP_AUDIO, Qualification::Refused);
        assert!(!forced.has_remaining_backend(BackendKind::Feed),
            "Force cannot use a compatible conversion as a rescue");

        let mut crossed = BackendPolicy::for_request(false);
        crossed.qualify_planned_feed(OfferId::FEED_ORIGINAL);
        assert!(crossed.admitted(BackendKind::Feed, OfferId::FEED_ORIGINAL));
        crossed.qualify(BackendKind::Uri, OfferId::URI_ORIGINAL, Qualification::Qualified);
        assert!(crossed.admitted(BackendKind::Uri, OfferId::URI_ORIGINAL));
        assert!(!crossed.has_remaining_backend(BackendKind::Feed),
            "a Feed→URI handoff cannot buy a reverse handoff");
    }
}
