use super::*;

/// How the media reaches this television, as PMS classifies it. Not a preference — a different
/// amount of prior knowledge, which is why bootstrap branches on it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum LinkKind {
    Local,
    Remote,
    Relay,
}

/// Why bootstrap chose what it chose. Printed verbatim into the event log: the startup decision is
/// the one nobody can re-run, so it has to explain itself the first time.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum BootstrapReason {
    /// A verified LAN carrying a file the device can play. No probe: a measurement to prove that a
    /// local network can carry local media would only cost the viewer a second of black screen.
    LocalDirect,
    /// Original is not technically possible at all — codec, container, burn-in, or PMS offering no
    /// playable source URL.
    OriginalInfeasible,
    /// Relay. Plex's relay is bandwidth-limited by design, so Original is not a candidate and
    /// measuring it would be theatre.
    RelayLimited,
    /// A bounded probe delivered source bytes at least as fast as playback consumes them.
    ProbeSustainable,
    /// The probe completed below the source consumption rate. Its VALUE is still the best evidence there is,
    /// and it picks the starting rung.
    ProbeBelowRequirement,
    /// The probe never finished inside its budget, or the source bitrate is unknown, so there is
    /// nothing to reason from. Conservative HLS, and playback still starts — a link this client
    /// could not measure is not a reason to refuse to play.
    ProbeInconclusive,
}

/// The startup state, plus what to hand the steady-state controller.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct BootstrapDecision {
    pub(crate) original: bool,
    /// The starting rung when `original` is false. Chosen from the same catalog steady-state
    /// selection uses, so a 17 Mbit/s probe on a 60 Mbit/s file opens at a 12-14 Mbps rendition
    /// instead of at an emergency floor it would then have to climb out of for a minute.
    pub(crate) rung: Rung,
    pub(crate) reason: BootstrapReason,
    /// The probe, as a weak prior for the live estimator — so the first HLS segment refines a
    /// measurement instead of starting from nothing. `None` when there was no usable probe.
    pub(crate) prior: Option<CapacityEstimate>,
}

/// **Cold start, where every estimator is empty and the viewer is looking at a black screen.**
///
/// This is a separate decision from steady state and must not pretend otherwise: there is no
/// history, no buffer, no production evidence, and a strict latency budget on acquiring any. So it
/// branches on how much is knowable for free, and its worst case is "start conservative HLS and
/// let the real controller recover", never "hold the screen black until the link is proven".
pub(crate) fn bootstrap(
    link: LinkKind,
    original_feasible: bool,
    source_kbps: u32,
    probe: Option<CapacityObservation>,
    catalog: &HlsActuatorCatalog,
    policy: &AbrPolicy,
) -> BootstrapDecision {
    let fallback_rung = catalog
        .best_for_budget(policy_startup_floor_kbps(policy))
        .or_else(|| catalog.feasible().next())
        .map(|candidate| candidate.rung)
        .unwrap_or(Rung::P480);
    let deny = |reason| BootstrapDecision {
        original: false,
        rung: fallback_rung,
        reason,
        prior: None,
    };
    if !original_feasible {
        return deny(BootstrapReason::OriginalInfeasible);
    }
    match link {
        LinkKind::Local => BootstrapDecision {
            original: true,
            rung: fallback_rung,
            reason: BootstrapReason::LocalDirect,
            prior: None,
        },
        LinkKind::Relay => deny(BootstrapReason::RelayLimited),
        LinkKind::Remote => {
            let Some(probe) = probe.filter(|p| p.kbps > 0 && source_kbps > 0) else {
                return deny(BootstrapReason::ProbeInconclusive);
            };
            if !probe.completed {
                // A deadline-censored prefix has no sustained-rate meaning. Its opening socket
                // burst can be arbitrarily fast before the response stalls, so it identifies
                // neither an opening actuator nor a steady-state prior.
                return deny(BootstrapReason::ProbeInconclusive);
            }
            let sustainable =
                original_sustainable(source_kbps, probe.kbps, probe.completed, policy);
            let mut prior = CapacityEstimate::default();
            prior.update(probe);
            // Explicitly weak: the probe measured the SOURCE request over this link, and the HLS
            // segments about to arrive are a different request to a server doing different work.
            prior.demote_to_prior();
            BootstrapDecision {
                original: sustainable,
                rung: startup_rung(probe.kbps, catalog, fallback_rung),
                reason: if sustainable {
                    BootstrapReason::ProbeSustainable
                } else {
                    BootstrapReason::ProbeBelowRequirement
                },
                prior: Some(prior),
            }
        }
    }
}

/// Choose the first HLS actuator when an existing playback is handed back to Auto.
///
/// This is not a cold start: throwing the route away and applying [`policy_startup_floor_kbps`]
/// would discard two kinds of evidence the playback may already own.  Let `F` be the feasible
/// catalog, `W(r)` its calibrated wire demand, `r_c` the currently playing fixed rung, and `C_p`
/// the carried posterior's conservative capacity.  The re-entry point is
///
/// `arg max W(r)` over the union of `{r_c in F}`, `{r in F: W(r) <= C_p}`, and the ordinary
/// unknown-link fallback.
///
/// Thus Auto never begins below the control point it is replacing, may immediately reclaim a
/// higher rung already supported by its own posterior, and invents no connection-speed claim when
/// neither exists.  Source/device feasibility is applied before all three terms.
pub(crate) fn hls_reentry_rung(
    current: Option<Rung>,
    prior: Option<CapacityEstimate>,
    catalog: &HlsActuatorCatalog,
    policy: &AbrPolicy,
) -> Rung {
    let fallback = catalog
        .best_for_budget(policy_startup_floor_kbps(policy))
        .or_else(|| catalog.feasible().next());
    let current = current.and_then(|rung| catalog.feasible().find(|c| c.rung == rung));
    let posterior =
        prior.and_then(|estimate| catalog.best_for_budget(estimate.conservative_kbps()));
    fallback
        .into_iter()
        .chain(current)
        .chain(posterior)
        .max_by_key(|candidate| candidate.expected_wire_kbps)
        .map(|candidate| candidate.rung)
        .unwrap_or(Rung::P480)
}

/// Pick the highest rendition whose declared demand does not exceed the service the completed
/// probe actually achieved.  Startup reserve is handled as media time by the player; multiplying
/// the rate by an unrelated fraction would be a second, dimensionally hidden reserve.
pub(crate) fn startup_rung(
    measured_kbps: u32,
    catalog: &HlsActuatorCatalog,
    fallback: Rung,
) -> Rung {
    catalog
        .best_for_budget(measured_kbps)
        .map(|candidate| candidate.rung)
        .unwrap_or(fallback)
}

/// The opening rung when nothing at all is known — one the link almost certainly carries, chosen
/// so the first upshift has real evidence behind it rather than being an immediate correction.
pub(crate) fn policy_startup_floor_kbps(_policy: &AbrPolicy) -> u32 {
    Rung::P480.kbps()
}
