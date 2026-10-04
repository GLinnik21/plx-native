//! **A roster commit must not blank the poster it does not change.** The session owner decides
//! what a roster result commits (its half is `auth::owner`'s scenario functions, which build the
//! commit plan); the registry changes that plan carries are executed here, under a poster slot
//! that is RESIDENT for the server, and the poster adapter
//! (`adapters::poster::resident_art_survives_for_test`) says whether that art still answers a draw
//! afterwards. The poster is an app adapter, so the grading lives in this layer.

use plx_session::auth::owner::scenarios::{
    activation_under_another_accounts_token, admin_boot_refresh_of_the_seated_profile,
    late_roster_of_the_seated_profile, refresh_under_another_accounts_token,
};
use plx_session::auth::owner::{RegistryPlan, RosterCommit};

/// **Blink C** (owner trace, 2026-09-30, who's-watching picker path): a SECOND `plex: 2
/// server(s) revoked — profile changed` 4-6 s after Home was drawn, with the picked profile
/// still seated — every poster `HIDDEN cause=grant_epoch`, every hub refetched. The switch's
/// own late `ProfileRoster` re-committed the already-seated profile as another `Switch`.
#[test]
fn a_late_profile_roster_for_the_seated_profile_keeps_resident_art() {
    let _g = plx_base::testlock::serial();
    plx_plex::plex::reset_servers_for_test();
    plx_plex::plex::grant::reset_for_test();
    let (sid, plan) = late_roster_of_the_seated_profile();
    let kept = crate::app::adapters::poster::resident_art_survives_for_test(sid, || {
        for p in &plan.registry {
            assert!(plx_session::auth::execute_session_registry(p, "synthetic-client"));
        }
    });
    assert!(kept, "the late roster of the already-seated profile revoked its art");
    assert!(matches!(&plan.registry[0],
        RegistryPlan::Install { commit: RosterCommit::Refresh { same_identity: true }, .. }),
        "the seated profile's own roster is a same-identity refresh, not a second switch");
    plx_plex::plex::grant::reset_for_test();
    plx_plex::plex::reset_servers_for_test();
}

/// **Blink B** (owner trace, 2026-09-30, ~11.6 s into an ordinary stored-session launch):
/// `plex: 2 server(s) revoked — profile changed` with nobody touching the profile, then every
/// poster on Home `HIDDEN` and every hub refetched. The boot's admin roster refresh found the
/// same seated profile's server under plex.tv's current grant (a token string different from
/// the stored one) and committed that as a PROFILE SWITCH — blanking every live client.
/// A refresh of the seated profile's roster is not a change of identity: the stored server's
/// resident art must survive the whole commit.
#[test]
fn an_admin_boot_refresh_of_the_seated_profile_keeps_resident_art() {
    const ROTATED_GRANT: &str = "plex-tv-grant-for-the-same-owner";
    let _g = plx_base::testlock::serial();
    plx_plex::plex::reset_servers_for_test();
    plx_plex::plex::grant::reset_for_test();
    let (sid, plan) = admin_boot_refresh_of_the_seated_profile(ROTATED_GRANT);

    let kept = crate::app::adapters::poster::resident_art_survives_for_test(sid, || {
        for p in &plan.registry {
            assert!(plx_session::auth::execute_session_registry(p, "synthetic-client"));
        }
    });
    assert!(kept, "a same-profile roster refresh revoked the stored server's art");
    let c = plx_plex::plex::client_for(sid).unwrap();
    assert!(c.image_transcode_path("/t", 2, 2, false).ends_with(&format!("X-Plex-Token={ROTATED_GRANT}")),
        "the refresh must still install plex.tv's current grant");
    plx_plex::plex::grant::reset_for_test();
    plx_plex::plex::reset_servers_for_test();
}

/// Review finding on the Refresh commit: `admin` is not "the account holder". The terminal
/// reconcile of the member account's roster installs the member's grants over the admin's
/// live tokens; nothing proves the two are one identity, so the art claimed under the admin
/// must not survive into the member-token session.
#[test]
fn a_refresh_under_another_accounts_token_does_not_keep_the_seated_profiles_art() {
    let _g = plx_base::testlock::serial();
    let (sid, plan) = refresh_under_another_accounts_token();
    let kept = crate::app::adapters::poster::resident_art_survives_for_test(sid, || {
        for p in &plan.registry {
            assert!(plx_session::auth::execute_session_registry(p, "synthetic-client"));
        }
    });
    assert!(!kept, "another account's grants were installed as a same-identity refresh");
    plx_plex::plex::grant::reset_for_test();
    plx_plex::plex::reset_servers_for_test();
}

/// The same gap one observation earlier: the roster worker's `Activate` progress for the
/// admin's server, carrying the member's grant, re-tokens the admin's live slot in place.
#[test]
fn an_activation_under_another_accounts_token_does_not_keep_the_seated_profiles_art() {
    let _g = plx_base::testlock::serial();
    let (sid, plan) = activation_under_another_accounts_token();
    let kept = crate::app::adapters::poster::resident_art_survives_for_test(sid, || {
        for p in &plan.registry {
            assert!(plx_session::auth::execute_session_registry(p, "synthetic-client"));
        }
    });
    assert!(!kept, "another account's grant was activated as a same-identity retoken");
    plx_plex::plex::grant::reset_for_test();
    plx_plex::plex::reset_servers_for_test();
}
