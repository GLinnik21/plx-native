//! **Every sign-in read-out reason fits its two-line slot, in every shipped language.**
//!
//! A page-placed `Failed` read-out wraps its reason into a reserved two-line slot
//! (`StatusOverlay::REASON_W`) and ellipsizes whatever is left, and *Details* shows diagnostics,
//! not the rest of the sentence. The Spanish and Belarusian "found your server, but not over HTTPS"
//! and "none of your servers answered" reasons lost their remedy — the half of the read-out that
//! says what to do — to that ellipsis while the English fitted. Measured with
//! [`crate::fontcov::advances::ShippedMeasure`], the device's own whole-pixel advances.

use crate::fontcov::advances::{ShippedMeasure, HEADROOM};
use crate::i18n::{language_on_this_thread_for_test, msg, Preference};
use crate::ui::widgets::StatusOverlay;
use std::ffi::CString;

/// Every `browse.auth.*` message: each is a sign-in or profile-switch failure's reason, drawn in
/// the read-out's reason slot. Arguments take a long-but-real value.
fn reasons() -> Vec<(&'static str, String)> {
    let (profile, server) = ("Alexandra", "Living Room Server");
    let mut out: Vec<(&'static str, String)> = vec![
        ("authority_failed", msg::browse_auth_authority_failed().into()),
        ("discovery_trouble", msg::browse_auth_discovery_trouble().into()),
        ("finish_failed", msg::browse_auth_finish_failed().into()),
        ("insecure", msg::browse_auth_insecure().into()),
        ("no_access", msg::browse_auth_no_access(profile)),
        ("no_servers", msg::browse_auth_no_servers().into()),
        ("no_servers_signed_in_as", msg::browse_auth_no_servers_signed_in_as("alexandra.konstantinopolskaya")),
        ("no_source_access", msg::browse_auth_no_source_access(profile)),
        ("offline_profile", msg::browse_auth_offline_profile().into()),
        ("plaintext_allowed", msg::browse_auth_plaintext_allowed().into()),
        ("plaintext_declined_signin", msg::browse_auth_plaintext_declined_signin().into()),
        ("plaintext_offer", msg::browse_auth_plaintext_offer().into()),
        ("plaintext_remote", msg::browse_auth_plaintext_remote().into()),
        ("plaintext_revoked_signin", msg::browse_auth_plaintext_revoked_signin().into()),
        ("plaintext_shared_allowed", msg::browse_auth_plaintext_shared_allowed(server)),
        ("plaintext_shared_declined_signin", msg::browse_auth_plaintext_shared_declined_signin(server)),
        ("plaintext_shared_insecure", msg::browse_auth_plaintext_shared_insecure(server)),
        ("plaintext_shared_offer", msg::browse_auth_plaintext_shared_offer(server)),
        ("plaintext_shared_remote", msg::browse_auth_plaintext_shared_remote(server)),
        ("plaintext_shared_revoked_signin", msg::browse_auth_plaintext_shared_revoked_signin(server)),
        ("plex_tls", msg::browse_auth_plex_tls().into()),
        ("plex_unavailable", msg::browse_auth_plex_unavailable().into()),
        ("plex_unreachable", msg::browse_auth_plex_unreachable().into()),
        ("profile_signin_refused", msg::browse_auth_profile_signin_refused(profile)),
        ("rediscover_failed", msg::browse_auth_rediscover_failed().into()),
        ("refused", msg::browse_auth_refused().into()),
        ("roster_refused", msg::browse_auth_roster_refused().into()),
        ("roster_unreachable", msg::browse_auth_roster_unreachable().into()),
        ("server_profile_refused", msg::browse_auth_server_profile_refused(profile, server)),
        ("servers_unreachable", msg::browse_auth_servers_unreachable().into()),
        ("signin_refused", msg::browse_auth_signin_refused().into()),
        ("start_failed", msg::browse_auth_start_failed().into()),
        ("switch_failed", msg::browse_auth_switch_failed().into()),
        ("switch_invalid", msg::browse_auth_switch_invalid().into()),
        ("switch_retry", msg::browse_auth_switch_retry().into()),
        ("timeout", msg::browse_auth_timeout().into()),
        ("unreachable", msg::browse_auth_unreachable().into()),
    ];
    for count in [1, 3, 12] {
        out.push(("plex_connect_retry", msg::browse_auth_plex_connect_retry(count)));
        out.push(("plex_dns_retry", msg::browse_auth_plex_dns_retry(count)));
    }
    out
}

#[test]
fn every_sign_in_reason_fits_the_read_out_slot_in_every_language() {
    let mut out = Vec::new();
    for language in [Preference::En, Preference::Es, Preference::Be] {
        let _guard = language_on_this_thread_for_test(language);
        for (key, text) in reasons() {
            let c = CString::new(text.as_str()).unwrap();
            if StatusOverlay::failed_reason_truncates(&c, &ShippedMeasure, HEADROOM) {
                out.push(format!("{} browse.auth.{key}: {text:?}", language.tag()));
            }
        }
    }
    assert!(out.is_empty(), "reasons the read-out would end in an ellipsis:\n  {}", out.join("\n  "));
}
