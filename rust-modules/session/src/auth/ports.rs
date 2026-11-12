//! Live session job bodies and their clock adapters. No SessionMachine logic lives here.
use super::*;
use std::sync::mpsc;

fn clock_now() -> Instant {
    plx_base::step::assert_io_allowed("session clock");
    Instant::now()
}
fn clock_elapsed(started: Instant) -> Duration {
    plx_base::step::assert_io_allowed("session elapsed clock");
    started.elapsed()
}
#[cfg(test)]
mod scope_tests {
    use super::*;
    #[test]
    fn captured_clock_and_live_retry_clock_still_refuse_step_reads() {
        let now = admission_clock();
        let live = || true;
        let clock = LiveFnClock { live: &live, started: now() };
        let _allowed = plx_base::task::allow_blocking(const { &plx_base::task::BlockingLabel::new("fixture") });
        let step = plx_base::step::StepScope::enter();
        assert!(std::panic::catch_unwind(now).is_err());
        assert!(std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| RetryClock::elapsed(&clock))).is_err());
        drop(step);
        now();
        RetryClock::elapsed(&clock);
    }
}


pub(super) fn home_roster_worker_with_io(epoch: u64, expected: SessionIdentity, cid: String,
    token: String, output: &dyn owner::ObservationSink,
    mut home_users: impl FnMut(&AccountClient, Duration) -> Result<Vec<HomeUser>, CallEvidence>) {
    plx_base::step::assert_io_allowed("session::home_roster_worker_with_io");
    if !output.live() { return; }
    let ac = AccountClient::new(&cid, Some(&token));
    let mut clock = LiveRetryClock { output, started: clock_now() };
    let Some(users) = home_roster_with_io_and_clock(&ac, &mut clock, &mut home_users) else { return };
    output.terminal(AuthProgress::HomeRoster(HomeRosterProgress {
        epoch,
        expected,
        users,
    }));
}

pub(super) fn mint_pin(ac: &AccountClient, epoch: u64, generation: u32,
    output: &dyn owner::ObservationSink) -> Option<MintedCode> {
    plx_base::step::assert_io_allowed("session::mint_pin");
    if !output.live() { return None; }
    if generation > 1 {
        // Liveness only, no write — see the section doc above [`login_thread`]. This is the same
        // "stop wasting plex.tv calls on a dead flow" courtesy the old synchronous check made:
        // without it, the account call below would still burn a network round trip minting a code
        // nobody is left to scan.
        if !output.live() {
            return None;
        }
        if !output.progress(LoginProgress::CodeReplacing { epoch }.into()) { return None; }
    }
    let mut clock = LiveRetryClock { output, started: clock_now() };
    let created = retry_account_call(INTERACTIVE_ACCOUNT, &mut clock, |_, _, _| {},
        |remaining| scripted::signin_trouble_create()
            .unwrap_or_else(|| ac.create_pin_with(account_timeouts(remaining))));
    let created = match created.result {
        AccountCallEnd::Answer(pin) => Ok(pin),
        AccountCallEnd::Failed(evidence) => Err(evidence),
        AccountCallEnd::Cancelled => return None,
    };
    let pin = match created {
        Ok(p) if p.id != 0 && !p.code.is_empty() => p,
        failed => {
            // A 2xx that decoded into a pin with no id or code is still an answer: classed by
            // its status, which is the 2xx it was.
            let last = match failed { Err(evidence) => evidence, Ok(_) => Ok(200) };
            // Says what the internet is FOR here — signing in — because the one time this screen
            // appears with the link deliberately down is the first boot of a set that has never
            // signed in. Drawn as the reason under "Couldn't sign in" (`screens/login.rs`).
            output_failed(output,
                epoch,
                plx_platform::i18n::msg::browse_auth_plex_unreachable(),
                IncidentContext::new(IncidentKind::PinCreate, Some(last))
                    .with_link_state(0, None, generation),
                None,
            );
            return None;
        }
    };
    // **The lease starts HERE, not where the polling does.** plex.tv began counting the moment it
    // answered, and the QR fetch below is another request on `net::API`'s 25 s deadline — so a
    // clock started after it would let the poll run that much past the code's real death, which is
    // the same over-run in miniature that this whole change is about.
    let minted = clock_now();
    // Neither the id nor the code may be logged. `GET /api/v2/pins/{id}` is what RETURNS the
    // account token once the user authorizes (plex/account.rs `poll_pin`), so the id is a handle
    // that redeems a credential, and the code is what authorizes it — and this file is the one we
    // ask users to send us when something goes wrong. Log that we got here, not what we got.
    log(&format!(
        "auth: pin created (code {generation} of {MAX_PIN_GENERATIONS}, {}s to authorize)",
        pin_window(pin.expires_in).as_secs()
    ));
    // fetch the server-rendered QR PNG (the exact QR the official apps display); public, no token.
    let qr_url = if pin.qr.is_empty() {
        format!("{}/api/v2/pins/qr/{}", plx_plex::plex::account::plex_tv(), pin.code)
    } else {
        pin.qr.clone()
    };
    if !output.live() { return None; }
    let qr_png = plx_net::net::https_get_public(&qr_url)
        .filter(|r| r.ok())
        .map(|r| r.body)
        .unwrap_or_default();
    log(&format!("auth: qr png {} bytes", qr_png.len()));
    // Same liveness-only check as above, and the same reason: no point publishing a code the flow
    // this worker belongs to no longer exists to show. The owner's checked QR allocator runs
    // only on accepted CodeReady, not here on the producer.
    if !output.live() {
        return None;
    }
    if !output.progress(LoginProgress::CodeReady {
        epoch,
        code: pin.code.clone(),
        qr_png,
    }.into()) { return None; }
    Some(MintedCode {
        id: pin.id,
        expires_in: pin.expires_in,
        minted,
    })
}

pub(super) fn finish_sign_in(ac: &AccountClient, epoch: u64, server: ServerRef, sources: Vec<SourceRef>,
    output: &dyn owner::ObservationSink) {
    plx_base::step::assert_io_allowed("session::finish_sign_in");
    if !output.live() { return; }
    // Discovery already queued its activation observations before this SignedIn observation.
    // Only owner-accepted resource effects install clients; the historical "installed" log
    // label below describes the observed result, not proof of main-thread commit completion.
    // `log_form`, not `base()`: byte-identical to the `{addr}:{port}` this line always printed
    // for a plaintext origin (so an archived log stays comparable), and the whole URL as soon as
    // the scheme is worth saying. See `Origin::log_form`.
    log(&format!(
        "auth: PMS client installed {}",
        server.origin().log_form()
    ));

    // 4) Plex Home roster → who's-watching, or straight in if there's a single user. The roster is
    // kept on the session so it persists with the creds — the boot picker and every later
    // "Change profile" render from it instantly, online or not.
    //
    // A failed fetch still signs in (as a single user) and persists an empty roster, which the
    // session reads as "unknown" (`Session::account`) — but it now says WHY in the log, in the
    // same grading the Change-profile refresh uses, rather than a bare `n=0`.
    let mut clock = LiveRetryClock { output, started: clock_now() };
    let Some(users) = sign_in_home_users_with_clock(ac, epoch, output, &mut clock,
        |ac, remaining| ac.home_users_with(account_timeouts(remaining))) else { return };
    log(&format!("auth: home users n={}", users.len()));
    // One observation carrying everything the owner needs to commit the session at once —
    // see [`LoginProgress::SignedIn`] for why this used to be three separate `with_ctl` writes and
    // is now one.
    output.terminal(LoginProgress::SignedIn {
        epoch,
        server,
        sources,
        users,
    }.into());
}

pub(super) fn cancellable_wait_while(duration: Duration, live: impl Fn() -> bool) -> bool {
    plx_base::step::assert_io_allowed("session::cancellable_wait_while");
    const SLICE: Duration = Duration::from_secs(1);
    let deadline = clock_now() + duration;
    loop {
        if !live() { return false; }
        let now = clock_now();
        if now >= deadline { return true; }
        std::thread::sleep((deadline - now).min(SLICE));
    }
}

pub(super) fn race_batch(
    plan: &ProbePlan,
    indices: &[usize],
    dial: ProbeDial,
    spawn: &dyn Fn(usize, ProbeJob) -> bool,
    policy: ProbeDeadlines,
    activate: &mut dyn FnMut(&ProbePlan, &Candidate, &Origin),
) -> BatchResult {
    plx_base::step::assert_io_allowed("session::race_batch");
    let (tx, rx) = mpsc::channel::<ProbeMessage>();
    let mut pending = vec![None; plan.candidates.len()];
    let mut live = 0usize;

    for &index in indices {
        let c = &plan.candidates[index];
        let Some(origin) = dial_target(c) else {
            continue;
        };
        let started = clock_now();
        let budget = probe_deadline(c, policy);
        let deadline = started + budget;
        let state = Arc::new(AtomicU8::new(PROBE_PENDING));
        let tx = tx.clone();
        let dial = Arc::clone(&dial);
        let worker_state = Arc::clone(&state);
        let machine_id = plan.machine_id.clone();
        // Built here, at the dial that decides a winner — not only at `apply_candidate_activation`,
        // which pins the same way after one already has. `None` for a plaintext candidate (a pin
        // belongs to a TLS name only) or an unmatched/undecodable label; the request then resolves
        // through DNS exactly as before.
        let pin = plx_plex::plex::ResolvePin::for_origin(&origin, &c.address);
        let location = c.location;
        let job = Box::new(move || {
            let reply = dial(&origin, pin.as_ref(), budget);
            let untrusted = reply.untrusted();
            let (outcome, route) = reply.grade_learning(&machine_id, location);
            let on_time = clock_now() <= deadline;
            // Claim completion before publishing the message. If the coordinator expires first,
            // this result is inert. If this claim wins and the worker is descheduled before send,
            // the coordinator sees COMPLETED and waits for the already-decided result rather than
            // erasing it on its own later wall-clock sample.
            if worker_state
                .compare_exchange(
                    PROBE_PENDING,
                    PROBE_COMPLETED,
                    Ordering::AcqRel,
                    Ordering::Acquire,
                )
                .is_ok()
            {
                let _ = tx.send(ProbeMessage {
                    index,
                    on_time,
                    outcome,
                    route,
                    untrusted,
                });
            }
        });
        if spawn(index, job) {
            pending[index] = Some(PendingProbe { deadline, state });
            live += 1;
        }
    }
    // Only worker-held senders remain. If one panics (or an injected spawner accepts then drops its
    // job), disconnect settles the remaining pending set instead of parking the coordinator.
    drop(tx);

    let mut result = BatchResult {
        observed: vec![None; plan.candidates.len()],
        ..BatchResult::default()
    };
    let mut first_win: Option<Instant> = None;
    while live > 0 {
        // Drain results that completed on time BEFORE expiring by the coordinator's current clock.
        // Spawn setup and queue backlog are allowed to delay observation; `finished` is the fact
        // that decides whether the candidate met its own absolute deadline.
        loop {
            match rx.try_recv() {
                Ok(message) => settle_probe_message(
                    plan,
                    message,
                    &mut pending,
                    &mut live,
                    &mut result,
                    activate,
                ),
                Err(mpsc::TryRecvError::Empty) => break,
                Err(mpsc::TryRecvError::Disconnected) => {
                    if live > 0 {
                        live = 0;
                        pending.fill(None);
                    }
                    break;
                }
            }
        }
        if live == 0 {
            break;
        }
        let now = clock_now();
        for &index in indices {
            let expired = pending[index].as_ref().is_some_and(|p| {
                p.deadline <= now
                    && p.state
                        .compare_exchange(
                            PROBE_PENDING,
                            PROBE_EXPIRED,
                            Ordering::AcqRel,
                            Ordering::Acquire,
                        )
                        .is_ok()
            });
            if expired {
                pending[index] = None;
                live -= 1;
                result.observed[index] = Some(RouteOutcome::Timeout);
                let c = &plan.candidates[index];
                log(&format!(
                    "auth: '{}' probe timed out at {}:{}",
                    plan.name, c.address, c.port
                ));
            }
        }
        if live == 0 {
            break;
        }
        if first_win.is_none() && result.first.is_some() {
            first_win = Some(clock_now());
        }
        // A candidate whose worker already claimed completion owes a message that is about to
        // land, so it is never skipped; only one still dialling can be left behind.
        let undecided: Vec<usize> = indices
            .iter()
            .copied()
            .filter(|&i| {
                pending[i]
                    .as_ref()
                    .is_some_and(|p| p.state.load(Ordering::Acquire) == PROBE_PENDING)
            })
            .collect();
        if undecided.len() == live
            && race_decided(
                plan,
                &undecided,
                result.best.as_ref(),
                first_win.map(clock_elapsed),
                policy.grace,
            )
        {
            let mut left_behind = 0usize;
            for &index in &undecided {
                if let Some(p) = pending[index].take() {
                    if p
                        .state
                        .compare_exchange(
                            PROBE_PENDING,
                            PROBE_EXPIRED,
                            Ordering::AcqRel,
                            Ordering::Acquire,
                        )
                        .is_ok()
                    {
                        left_behind += 1;
                    }
                }
            }
            if left_behind > 0 {
                log(&format!(
                    "auth: '{}' settled on its first verified route; {left_behind} slower or lower-ranked address(es) not waited for",
                    plan.name
                ));
            }
            break;
        }
        let next = indices
            .iter()
            .filter_map(|&i| pending[i].as_ref())
            .filter(|p| p.state.load(Ordering::Acquire) == PROBE_PENDING)
            .map(|p| p.deadline)
            // Wake for the end of the grace too, but only while it is still ahead: a past one
            // would turn the wait below into a zero-timeout spin.
            .chain(first_win.map(|at| at + policy.grace).filter(|end| *end > clock_now()))
            .min();
        let received = match next {
            Some(next) => rx.recv_timeout(next.saturating_duration_since(clock_now())),
            // Every live worker has already claimed completion and owes exactly one message.
            // Blocking here avoids a zero-timeout spin in the tiny claim-before-send window.
            None => rx.recv().map_err(|_| mpsc::RecvTimeoutError::Disconnected),
        };
        match received {
            Ok(message) => settle_probe_message(
                plan,
                message,
                &mut pending,
                &mut live,
                &mut result,
                activate,
            ),
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                // Every sender is gone, so no pending candidate can ever report. This includes a
                // worker panic and an injected accepted-but-dropped job.
                live = 0;
                for slot in pending.iter_mut() {
                    *slot = None;
                }
            }
        }
    }
    result
}

pub(super) fn discover_and_store_with_resources(ac: &AccountClient, client_id: &str, epoch: u64,
    trigger: DiscoveryTrigger, ask: &PlaintextAsk, output: &dyn owner::ObservationSink,
    resources_call: impl FnMut(&AccountClient, Duration) -> Result<Vec<Resource>, CallEvidence>) -> Discovery {
    plx_base::step::assert_io_allowed("session::discover_and_store_with_resources");
    let mut clock = LiveRetryClock { output, started: clock_now() };
    discover_and_store_with_resources_and_clock(ac, client_id, epoch, trigger, ask, output,
        &mut clock, resources_call)
}

pub(super) fn server_roster_worker_with_output(sess: Session, epoch: u64, expected: SessionIdentity,
    household: Vec<i64>, ask: &PlaintextAsk, output: &dyn owner::ObservationSink) {
    plx_base::step::assert_io_allowed("session::server_roster_worker_with_output");
    if !output.live() { return; }
    let ac = AccountClient::new(&sess.client_id, Some(&sess.account_token));
    let mut clock = LiveRetryClock { output, started: clock_now() };
    let run = retry_account_call(BACKGROUND_ACCOUNT, &mut clock, |_, _, _| {},
        |remaining| ac.resources_with(account_timeouts(remaining)));
    let resources = match run.result {
        AccountCallEnd::Answer(resources) => resources,
        AccountCallEnd::Cancelled => return,
        AccountCallEnd::Failed(evidence) => {
            log(&format!("auth: server roster refresh could not list resources ({})",
                plx_plex::plex::account::describe_evidence(&evidence)));
            output.terminal(AuthProgress::ServerRoster(ServerRosterProgress {
                epoch,
                expected,
                outcome: ServerRosterOutcome::Unreachable,
            }));
            return;
        }
    };
    let mut activate = |plan: &ProbePlan, c: &Candidate, origin: &Origin| {
        let credit = credit_for_machine(&resources, &plan.machine_id, &household);
        let evidence = evidence_for_machine(&resources, &plan.machine_id);
        output.progress(AuthProgress::Registry(RegistryProgress::Activate {
            epoch,
            expected: Some(expected.clone()),
            candidate: candidate_activation(plan, c, origin, &credit, evidence),
        }));
    };
    let mut settled = Vec::new();
    // Unlike `activate` above, this does NOT also `output.progress(RegistryProgress::Settled)`
    // per server as the race runs. It used to, and the owner committed BOTH: the live progress
    // arm (`Observation::Registry` in `owner.rs`) planned one `RegistryPlan::Probe` per server as
    // it settled, and the terminal `ServerRosterOutcome` below planned the identical set again
    // through `settled`/`roster_plan` — every probe this worker ever runs was published twice.
    // This worker is a BACKGROUND refresh (`refresh_roster`) with no live race screen watching
    // it — unlike `discover_and_store`'s sign-in flow, which really does drive a Sources/QR
    // screen off `RegistryProgress::Activate` while candidates settle, and keeps publishing both
    // — so there is nothing here for the extra progress arrival to reach before the terminal
    // commit does the same work. `settled` alone, folded into the terminal outcome, is authoritative.
    let resolution = resolve_roster_live_while(
        &resources,
        &household,
        &sess.client_id,
        ask,
        &mut activate,
        &mut |plan, verdict| {
            settled.push(settled_probe_of(plan, verdict));
        },
        &|| output.live(),
    );
    let found = match resolution.outcome {
        Resolved::Reached(found) => found,
        _ => {
            // R2/A5: a background refresh that found nothing to REGISTER may still have PROBED
            // something worth recording — carry it rather than throwing `settled` away, so the
            // owner can commit it as a registry-only `RegistryPlan::Probe` (`owner.rs`).
            output.terminal(AuthProgress::ServerRoster(ServerRosterProgress {
                epoch,
                expected,
                outcome: ServerRosterOutcome::NoReachable { settled },
            }));
            return;
        }
    };
    let admitted_machine_id = resolution.admitted_machine_id
        .expect("a reached roster has one authenticated primary");
    output.terminal(AuthProgress::ServerRoster(ServerRosterProgress {
        epoch,
        expected,
        outcome: ServerRosterOutcome::Reconcile {
            resources,
            found,
            admitted_machine_id,
            household,
            settled,
        },
    }));
}

pub(super) fn probe_endpoint_work(
    id: ServerId,
    machine_id: &str,
    sess: &Session,
    mut resources: impl FnMut(&AccountClient, Duration) -> Result<Vec<Resource>, CallEvidence>,
    probe: impl FnOnce(&Resource, &[i64]) -> (Option<SourceRef>, SettledProbe),
    live: &dyn Fn() -> bool,
) -> (Option<SourceRef>, Option<SettledProbe>) {
    plx_base::step::assert_io_allowed("session::probe_endpoint_work");
    // Nothing was actually probed on any of the early exits below — plex.tv never answered, or
    // this machine is no longer among its resources — so there is no verdict to report at all:
    // fabricating `Unreachable` here would widen a real, more specific probe result
    // (`InsecureOnly`/`Unauthorized`) into "Not reachable" once it reached the registry.
    if !live() { return (None, None); }
    let ac = AccountClient::new(&sess.client_id, Some(&sess.account_token));
    let mut clock = LiveFnClock { live, started: clock_now() };
    let run = retry_account_call(BACKGROUND_ACCOUNT, &mut clock, |_, _, _| {},
        |remaining| resources(&ac, remaining));
    let resources = match run.result {
        AccountCallEnd::Answer(resources) => resources,
        AccountCallEnd::Cancelled => return (None, None),
        AccountCallEnd::Failed(evidence) => {
            log(&format!(
                "auth: endpoint refresh for source {} could not list resources ({})",
                id.raw(),
                plx_plex::plex::account::describe_evidence(&evidence)
            ));
            return (None, None);
        }
    };
    let Some(resource) = resources
        .iter()
        .find(|resource| resource.is_server() && resource.client_identifier == machine_id)
    else {
        log(&format!(
            "auth: endpoint refresh for source {} found no matching resource",
            id.raw()
        ));
        return (None, None);
    };
    if !live() { return (None, None); }
    let (fresh, probe) = probe(resource, &sess.household_ids());
    (fresh, Some(probe))
}

pub fn profile_switch_worker_with_io(
    epoch: u64,
    expected: SessionIdentity,
    stored: Session,
    tile: UserTile,
    pin: Option<String>,
    recently_unreachable: bool,
    output: &dyn owner::ObservationSink,
    io: &mut impl ProfileWorkIo,
) {
    plx_base::step::assert_io_allowed("session::profile_switch_worker_with_io");
    if !output.live() { return; }
    let cid = stored.client_id.clone();
    let account_token = stored.account_token.clone();
    let ac = AccountClient::new(&cid, Some(&account_token));
    let cache_first = stored.cached_profile(&tile.uuid).is_some() && recently_unreachable;
    let outcome = if cache_first {
        log("auth: switch — plex.tv was unreachable moments ago, trying the cached credentials first");
        SwitchOutcome::Unreachable
    } else {
        io.switch(&ac, &tile.uuid, pin.as_deref())
    };
    let user = match outcome {
        SwitchOutcome::Switched(user) => user,
        SwitchOutcome::Refused(status) => {
            log(&format!("auth: profile switch -> refused (HTTP {status})"));
            let (error, pin_denied) = switch_failure(pin.is_some());
            output.terminal(AuthProgress::ProfileSwitch(ProfileSwitchProgress {
                epoch,
                expected,
                outcome: ProfileSwitchOutcomeProgress::Failed { error, pin_denied },
            }));
            return;
        }
        SwitchOutcome::Unreachable => {
            let outcome = offline_switch_outcome(&stored, &tile, pin.as_deref());
            output.terminal(AuthProgress::ProfileSwitch(ProfileSwitchProgress {
                epoch,
                expected,
                outcome,
            }));
            return;
        }
    };
    if !output.live() { return; }
    let profile_account = AccountClient::new(&cid, Some(&user.auth_token));
    let mut clock = LiveRetryClock { output, started: clock_now() };
    let run = retry_account_call(PROFILE_SWITCH_ACCOUNT, &mut clock, |_, _, _| {},
        |_| io.resources(&profile_account));
    let resources = match run.result {
        AccountCallEnd::Answer(resources) => resources,
        AccountCallEnd::Cancelled => return,
        AccountCallEnd::Failed(evidence) => {
            log(&format!("auth: profile resources request failed ({})",
                plx_plex::plex::account::describe_evidence(&evidence)));
            // A refusal is an answer, not a dead link: "check the connection" would send the
            // person to a router that is fine. The PIN was already accepted by this point, so
            // this is never the PIN flash either.
            let error = if plx_plex::plex::account::refused_identity(&evidence).is_some() {
                plx_platform::i18n::msg::browse_auth_profile_signin_refused(&tile.title)
            } else {
                plx_platform::i18n::msg::browse_auth_switch_failed().into()
            };
            output.terminal(AuthProgress::ProfileSwitch(ProfileSwitchProgress {
                epoch,
                expected,
                outcome: ProfileSwitchOutcomeProgress::Failed {
                    error,
                    pin_denied: false,
                },
            }));
            return;
        }
    };
    let grants = ordered_profile_grants(&resources);
    if grants.is_empty() {
        output.terminal(AuthProgress::ProfileSwitch(ProfileSwitchProgress {
            epoch,
            expected,
            outcome: ProfileSwitchOutcomeProgress::Failed {
                error: plx_platform::i18n::msg::browse_auth_no_access(&tile.title),
                pin_denied: false,
            },
        }));
        return;
    }

    let household = stored.household_ids();
    let mut admission_budget = AdmissionBudget::new(io.admission_budget());
    let admission_clock = io.admission_clock();
    let mut order = grants.clone();
    if let Some(pos) = order
        .iter()
        .position(|&i| resources[i].client_identifier == stored.server.machine_id)
    {
        order.swap(0, pos);
    }
    let mut reached = Vec::new();
    let mut probes: Vec<SettledProbe> = Vec::new();
    let mut probed = vec![false; resources.len()];
    let mut admission_failures = Vec::new();
    let mut selected_mid = None;
    for &i in &order {
        probed[i] = true;
        let mut rejected_origins = Vec::new();
        loop {
            if !output.live() { return; }
            let (mut winner, mut settled) =
                io.probe_after(&resources[i], &household, &rejected_origins);
            if !output.live() { return; }
            if winner.is_none() {
                if let Some(cached) = stored.sources.iter()
                    .find(|source| source.machine_id == resources[i].client_identifier) {
                    if let Some((cached_winner, cached_settled)) =
                        io.probe_cached(&resources[i], cached, &household, &rejected_origins) {
                        winner = cached_winner;
                        settled = cached_settled.after_fresh(&settled);
                    }
                }
            }
            if !output.live() { return; }
            if let Some(previous) = probes.iter_mut()
                .find(|probe| probe.machine_id == resources[i].client_identifier) {
                if previous.outcome != Outcome::Reachable || settled.outcome == Outcome::Reachable {
                    *previous = settled;
                }
            } else {
                probes.push(settled);
            }
            let Some(winner) = winner else { break };
            let origin = winner.origin_url.clone();
            if rejected_origins.contains(&origin) { break; }
            let admission = admission_budget.attempt_on(admission_clock, |deadline|
                io.admit_until(&winner, &cid, deadline));
            if admission == plx_plex::plex::EndpointAdmission::Usable {
                selected_mid = Some(winner.machine_id.clone());
                reached.push(winner);
                break;
            }
            log(&format!("auth: profile switch — resource[{i}] did not admit this profile ({admission:?})"));
            admission_failures.push((resources[i].name.clone(), admission));
            if admission_budget.exhausted() { break; }
            rejected_origins.push(origin);
        }
        if selected_mid.is_some() {
            break;
        }
    }
    let cached_sources = profile_sources(&stored.sources, &reached, &resources, &household);
    let initial = cached_sources.clone();
    let Some(primary) =
        selected_mid.and_then(|mid| initial.iter().find(|s| s.machine_id == mid).cloned())
    else {
        // S7: a grant that verified but only over plaintext is not the same failure as "you were
        // never given this server" — it names a fixable cause (HTTPS to the server), and the
        // ordinary copy sends the user to ask their friend for access they already have.
        let insecure_only = probes.iter().any(|p| p.outcome == Outcome::InsecureOnly)
            || admission_failures.iter().any(|(_, evidence)|
                *evidence == plx_plex::plex::EndpointAdmission::InsecureOnly);
        let refusal = admission_failures.iter().find_map(|(name, evidence)| match evidence {
            plx_plex::plex::EndpointAdmission::Refused(status) => Some((name, *status)),
            _ => None,
        });
        let malformed = admission_failures.iter().any(|(_, evidence)|
            *evidence == plx_plex::plex::EndpointAdmission::Malformed);
        // Why each server that no route verified could not be used. A probe that verified (and was
        // then refused at admission, or only over plaintext) is not in here: the arms above own it.
        let mut causes = FailureCauses::default();
        for probe in &probes {
            if let Some(cause) = probe.cause() { causes.push(&probe.machine_id, cause); }
        }
        let probe_cause = causes.worst();
        log(&format!(
            "auth: profile switch -> {}",
            if insecure_only { "verified only over plaintext" }
            else if refusal.is_some() { "server refused the profile credential" }
            else if malformed { "server returned a malformed authenticated response" }
            else if !admission_failures.is_empty() { "no server admitted the profile" }
            else { match probe_cause {
                Some(Cause::TlsUntrusted { .. }) => "a server's certificate is not trusted by this TV",
                Some(Cause::Unauthorized) => "no server access",
                Some(Cause::Unreachable) | None => "no server answered",
            } },
        ));
        if !(insecure_only || refusal.is_some() || malformed || !admission_failures.is_empty()) {
            log(&causes.log_line());
        }
        output.terminal(AuthProgress::ProfileSwitch(ProfileSwitchProgress {
            epoch,
            expected,
            outcome: ProfileSwitchOutcomeProgress::Failed {
                error: if insecure_only {
                    discovery_insecure_only_message().to_owned()
                } else if let Some((name, _)) = refusal {
                    plx_platform::i18n::msg::browse_auth_server_profile_refused(&tile.title, &name)
                } else if malformed {
                    plx_platform::i18n::msg::browse_auth_switch_invalid().into()
                } else if !admission_failures.is_empty() {
                    plx_platform::i18n::msg::browse_auth_switch_failed().into()
                } else {
                    // Closed precedence, strongest first: a certificate this TV does not trust (the
                    // person can fix it by updating), a 401 (the profile has no grant — the ONLY
                    // case `no_source_access` is true of), then silence. A probe that left no cause at
                    // all cannot occur once a server was probed; it reads as silence, never as a refusal.
                    match probe_cause {
                        Some(Cause::TlsUntrusted { .. }) => plx_platform::i18n::msg::browse_auth_tls_untrusted().into(),
                        Some(Cause::Unauthorized) => plx_platform::i18n::msg::browse_auth_no_source_access(&tile.title),
                        Some(Cause::Unreachable) | None => plx_platform::i18n::msg::browse_auth_servers_unreachable().into(),
                    }
                },
                pin_denied: false,
            },
        }));
        return;
    };

    log("auth: profile switch -> ok (per-user server token)");
    let server = server_ref(&primary);
    let plex_tv_token = (!user.auth_token.is_empty()).then(|| user.auth_token.clone());
    let user = UserRef {
        id: user.id,
        uuid: seated_uuid(&user, &tile),
        title: user.title,
        thumb: tile.thumb,
        token: primary.token.clone(),
        plex_tv_token,
        extensions: Default::default(),
    };
    if !output.live() { return; }
    // Carry forward whatever this uuid's PRIOR seating cached in `extensions` — the serde-flatten
    // catch-all for fields this build does not model (forward compatibility with a newer build
    // that wrote this session). `remember_profile` below replaces that uuid's whole cache entry,
    // so building a fresh one with `Default::default()` here silently erased it (Copilot review on
    // PR #105, finding 3).
    let carried_extensions = stored
        .profiles
        .iter()
        .find(|p| p.uuid == user.uuid)
        .map(|p| p.extensions.clone())
        .unwrap_or_default();
    let cache = ProfileCreds {
        uuid: user.uuid.clone(),
        user: user.clone(),
        server: server.clone(),
        sources: cached_sources,
        pin: pin
            .as_deref()
            .filter(|value| !value.is_empty())
            .map(session::PinVerifier::new),
        extensions: carried_extensions,
    };
    let next_identity = SessionIdentity {
        client_id: expected.client_id.clone(),
        account_token: expected.account_token.clone(),
        profile_uuid: user.uuid.clone(),
    };
    if !output.progress(AuthProgress::ProfileSwitch(ProfileSwitchProgress {
        epoch,
        expected,
        outcome: ProfileSwitchOutcomeProgress::Ready {
            delta: ProfileDelta {
                server,
                sources: initial,
                user,
                cache: Some(cache),
            },
            probes: probes.clone(),
        },
    })) { return; }

    for &i in &grants {
        if probed[i] {
            continue;
        }
        if !output.live() { return; }
        io.gap();
        if !output.live() { return; }
        let (mut winner, mut settled) = io.probe_after(&resources[i], &household, &[]);
        if !output.live() { return; }
        if winner.is_none() {
            if let Some(cached) = stored.sources.iter()
                .find(|source| source.machine_id == resources[i].client_identifier) {
                if let Some((cached_winner, cached_settled)) =
                    io.probe_cached(&resources[i], cached, &household, &[]) {
                    winner = cached_winner;
                    settled = cached_settled.after_fresh(&settled);
                }
            }
        }
        if !output.live() { return; }
        if let Some(previous) = probes.iter_mut()
            .find(|probe| probe.machine_id == resources[i].client_identifier) {
            if previous.outcome != Outcome::Reachable || settled.outcome == Outcome::Reachable {
                *previous = settled;
            }
        } else {
            probes.push(settled);
        }
        if let Some(winner) = winner { reached.push(winner); }
    }
    output.terminal(AuthProgress::ProfileRoster(ProfileRosterProgress {
        epoch,
        expected: next_identity,
        resources,
        reached,
        probes,
    }));
}

pub(super) struct LiveRetryClock<'a> {
    pub(super) output: &'a dyn owner::ObservationSink,
    pub(super) started: Instant,
}

impl RetryClock for LiveRetryClock<'_> {
    fn elapsed(&self) -> Duration { clock_elapsed(self.started) }
    fn wait(&mut self, duration: Duration) -> bool {
        cancellable_wait_while(duration, || self.output.live())
    }
}

pub(super) struct LiveFnClock<'a> { pub(super) live: &'a dyn Fn() -> bool, pub(super) started: Instant }

impl RetryClock for LiveFnClock<'_> {
    fn elapsed(&self) -> Duration { clock_elapsed(self.started) }
    fn wait(&mut self, duration: Duration) -> bool {
        cancellable_wait_while(duration, self.live)
    }
}

pub(super) struct LivePin<'a> {
    pub(super) ac: &'a AccountClient,
    pub(super) id: i64,
    pub(super) output: &'a dyn owner::ObservationSink,
    pub(super) started: Instant,
    pub(super) epoch: u64,
    /// Which code of the flow this is, 1-based — the report's code generation.
    pub(super) generation: u32,
}

impl PinWatch for LivePin<'_> {
    fn poll(&mut self) -> PinPoll {
        scripted::signin_trouble_poll().unwrap_or_else(|| self.ac.poll_pin(self.id))
    }
    fn wait(&mut self, d: Duration) -> bool {
        // SLICED, so a cancel is noticed within a slice however far the backoff has grown. The
        // worker's answer would be discarded anyway, but a thread that lingers for the whole of a
        // 16 s backoff after the user has left the screen is a thread the next flow shares the
        // device with.
        cancellable_wait_while(d, || self.output.live())
    }
    fn elapsed(&self) -> Duration {
        clock_elapsed(self.started)
    }
    fn link_trouble(&mut self, stall: Option<Stall>) {
        let trouble = stall.map(|s| {
            IncidentContext::new(IncidentKind::LinkStalled, Some(s.last))
                .with_link_state(s.unanswered, Some(s.failing_for), self.generation)
        });
        self.output.progress(LoginProgress::LinkTrouble { epoch: self.epoch, trouble }.into());
    }
}

impl AdmissionBudget {
    pub(super) fn attempt(
        &mut self,
        request: impl FnOnce(Instant) -> plx_plex::plex::EndpointAdmission,
    ) -> plx_plex::plex::EndpointAdmission {
    plx_base::step::assert_io_allowed("session::attempt");
        self.attempt_on(clock_now, request)
    }
}

pub(super) fn admission_clock() -> fn() -> Instant {
    plx_base::step::assert_io_allowed("session::admission_clock"); clock_now }
