//! Session-controller sanity and the phase-6 worker observation-boundary guards (login,
//! profile-switch, and the full worker roster). The tests that drive a worker through the live
//! Session adapter, and the sign-out consent teardown through the `Bridge`, are
//! `app/session_worker_adapter_tests.rs`.

use super::*;
#[allow(unused_imports)]
use super::test_support::*;

#[test]
fn session_controller_has_no_process_global_owner() {
    let source = include_str!("auth.rs");
    let production = source.split("#[cfg(test)]\nmod tests").next().unwrap();
    for declaration in ["static CTL:", "static QR_GENERATION:", "static ENDPOINT_ADMISSION:",
        "static DELETE_LEFTOVERS:", "static PROGRESS:", "static AUTH_EPOCH:", "static ACTIVATION_GATE:"] {
        assert!(!production.contains(declaration), "global decision/queue remains: {declaration}");
    }
}

// ---- phase 6: LoginProgress / apply_progress ----
//
// `login_thread` used to be a writer of `Ctl` with the same authority as `start_login`/
// `cancel`/`restart`, from a thread none of those three synchronize with except by convention.
// These three tests pin the replacement: a stale observation is refused, a cancel cannot be
// overtaken by a success that was already in flight when it happened, and the worker functions
// themselves no longer contain the write at all.

#[test]
fn a_profile_delta_preserves_unrelated_newer_session_preferences() {
    let mut current = signed_in_as("u-adult");
    current.recent_searches.push(session::RecentSearches {
        user: "u-adult".into(),
        terms: vec!["newer preference".into()],
        extensions: Default::default(),
    });
    let next = signed_in_as("u-kid");
    merge_profile_delta(
        &mut current,
        ProfileDelta {
            server: next.server,
            sources: next.sources,
            user: next.user,
            cache: None,
        },
    );
    assert_eq!(current.user.uuid, "u-kid");
    assert_eq!(current.recent_searches.len(), 1);
    assert_eq!(current.recent_searches[0].terms, ["newer preference"]);
}

/// **The invariant phase 6 exists to establish, pinned by reading the source.** Nothing else
/// can see a regression here: a `with_ctl(|c| c.foo = …)` spliced back into `login_thread`
/// compiles cleanly, passes every OTHER test in this file (none of them spin up a real worker
/// thread against a real plex.tv — see the module doc), and only misbehaves on a device, under
/// contention nobody happened to be watching for. Modelled on `eventlog::scrub`'s
/// `no_log_call_site_interpolates_viewing_content`, which pins its own "the mechanism is that
/// nobody writes it" claim the same way.
///
/// **What this test actually checks, restated after a reviewer found the gap in the stronger
/// sentence this comment used to make ("no `with_ctl` call at all", full stop).** That claim
/// was true of the two DIRECT spellings this test greped for and false of a WRAPPED one: the
/// reviewer added `set_error_if_live(epoch, "…")` to `login_thread` — the exact pre-phase-6
/// call [`LoginProgress::Failed`]'s doc says is retired — and it reached `with_ctl` two hops
/// down (`set_error_if_live` → `set_error` → `with_ctl`) while this test kept reporting green,
/// because a two-hop wrapper call is neither `with_ctl(` nor `CTL.lock(` in the CALLER's own
/// text. Three checks run per function now, not two: the original direct-spelling pair, PLUS
/// [`calls_a_function_prefixed`] against `set_`, the naming family every `Ctl`-writing setter
/// in this file already belongs to — which catches `set_error_if_live(` (and `set_error(` on
/// its own) by the family, not by a name typed into this test.
///
/// **What is still NOT proven, and this says so rather than overclaiming again:** a wrapper
/// that reached `Ctl` under a name outside the `set_` convention would still slip past a
/// textual scan — closing that fully needs a real call-graph walk (or moving `Ctl` behind an
/// interface a worker's module cannot name at all), not a longer prefix list. This is a
/// materially stronger gate than the one it replaces, scoped to say exactly that.
///
/// This original QR-specific guard stays beside the broader R2A boundary below because it also
/// scans the helper chain called by login discovery. Profile, roster and endpoint workers are
/// covered by [`all_auth_worker_bodies_are_observation_only`].
#[test]
fn login_worker_functions_never_touch_ctl_directly() {
    let src = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/auth.rs"),
    )
    .expect("auth.rs must be readable from its own test");
    for name in [
        "login_worker_with_output",
        "mint_pin",
        "finish_sign_in",
        "discover_and_store",
        "rediscovery_worker_with_output",
    ] {
        let body = extract_fn_body(&src, name);
        assert!(
            !body.contains("with_ctl("),
            "`{name}` calls `with_ctl` — a phase-6 worker must observe and push a \
             `LoginProgress` instead of touching `Ctl` directly (read OR write):\n{body}"
        );
        assert!(
            !body.contains("CTL.lock("),
            "`{name}` locks `CTL` directly, bypassing `with_ctl` but not the rule it exists \
             to enforce"
        );
        for wrapper in ["set_error(", "set_error_if_live("] {
            assert!(
                !body.contains(wrapper),
                "`{name}` calls `{wrapper}` — a `Ctl`-writing wrapper this test used to be \
                 blind to, because it only greped for `with_ctl(`/`CTL.lock(` directly and \
                 this reaches `with_ctl` two calls down. A worker must push a `LoginProgress` \
                 and let `apply_progress` make the write on the main thread instead."
            );
        }
        assert!(
            !calls_a_function_prefixed(body, "set_"),
            "`{name}` calls a `set_`-prefixed function — this file's naming convention for \
             every `Ctl`-writing setter it has (`set_error`, `set_error_if_live`, \
             `set_pin_denied_for_test`). A NEW setter sharing that prefix is refused here by \
             the family it belongs to; see the doc above this test for the mutation that made \
             the narrower direct-spelling check insufficient:\n{body}"
        );
    }
}

/// Textual worker boundary: the actual instance-worker entry points AND their shared policy
/// bodies. Scanning a thin forwarding wrapper alone cannot constrain its callee. This is
/// an explicit list, not automatic call-graph coverage; new worker helpers must be added.
/// Workers may perform network/probe/PBKDF2 work and publish immutable observations;
/// application mutation belongs to the main-thread owner/resource acceptance path.
#[test]
fn all_auth_worker_bodies_are_observation_only() {
    let src = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/auth.rs"),
    )
    .expect("auth.rs must be readable from its own test");
    let forbidden = [
        "with_ctl(",
        "CTL.lock(",
        "with_live_epoch(",
        "session::load(",
        "session::peek(",
        "session::save(",
        "session::update(",
        "session::clear(",
        "session::set_current(",
        "activate_candidate(",
        "install_roster(",
        "publish_settled_probe(",
        "publish_settled_probes(",
        "plx_plex::plex::register_origin(",
        "plx_plex::plex::revoke_",
        "plx_plex::plex::finish_profile_switch(",
        "plx_plex::plex::publish_probe_result(",
        "plx_plex::plex::describe_server(",
    ];
    for name in [
        "discover_and_store",
        "login_worker_with_output",
        "rediscovery_worker_with_output",
        "home_roster_worker_with_output",
        "server_roster_worker_with_output",
        "profile_switch_worker_with_output",
        "profile_switch_worker_with_io",
        "run_session_work",
        "endpoint_work_fact",
        "endpoint_worker_with_io",
        "probe_endpoint_work",
        "offline_switch_outcome",
        "candidate_activation",
        "probe_profile_resource_live",
    ] {
        let body = extract_fn_body(&src, name);
        for call in forbidden {
            assert!(
                !body.contains(call),
                "auth worker `{name}` crosses the observation boundary through `{call}`:\n{body}"
            );
        }
    }
}

/// #132: the roster worker's grading. A refused identity (a managed profile's 401) and an answer
/// with nobody in it are plex.tv's VERDICT — `Some(vec![])` — which the owner reads out as
/// "switching isn't available"; no answer at all, a 5xx or an unreadable body is `None`, read out
/// as a connection problem. Before this, all of these were one `None`.
#[test]
fn roster_grading_tells_a_refused_identity_from_no_answer() {
    use plx_net::net::{RequestError, RequestFailure};
    let user = HomeUser { uuid: "synthetic-user".into(), title: "Synthetic".into(), ..Default::default() };
    assert_eq!(grade_roster(Ok(vec![user])).map(|users| users.len()), Some(1));
    assert_eq!(grade_roster(Ok(Vec::new())).map(|users| users.len()), Some(0));
    assert_eq!(grade_roster(Err(Ok(401))).map(|users| users.len()), Some(0));
    assert_eq!(grade_roster(Err(Ok(403))).map(|users| users.len()), Some(0));
    assert!(grade_roster(Err(Ok(503))).is_none());
    assert!(grade_roster(Err(Ok(200))).is_none(), "an unreadable 200 is no verdict");
    assert!(grade_roster(Err(Err(RequestFailure { cause: RequestError::TimedOut, status: None,
        body_limit: None, curl_rc: Some(28) }))).is_none());
}
