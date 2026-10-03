//! **Every sign-in read-out reason fits its two-line slot, in every shipped language.**
//!
//! A page-placed `Failed` read-out wraps its reason into a reserved two-line slot
//! (`StatusOverlay::REASON_W`) and ellipsizes whatever is left, and *Details* shows diagnostics,
//! not the rest of the sentence. The Spanish and Belarusian "found your server, but not over HTTPS"
//! and "none of your servers answered" reasons lost their remedy — the half of the read-out that
//! says what to do — to that ellipsis while the English fitted. Measured with
//! [`plx_base::fontcov::advances::ShippedMeasure`], the device's own whole-pixel advances.

use plx_base::fontcov::advances::ShippedMeasure;
use crate::ui::fit::HEADROOM;
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

/// **The failed read-out says who signed in only when it was told a name.** A name composes the
/// two-line reason; no name, a blank one, or no measure-able name keeps the session's caption.
#[test]
fn the_failed_reason_names_the_account_or_keeps_the_caption() {
    let _guard = language_on_this_thread_for_test(Preference::En);
    let caption = msg::browse_auth_no_servers();
    assert_eq!(super::failed_reason(caption, Some("alexandra"), &ShippedMeasure),
        "Signed in as alexandra.\nThis Plex account has no server yet.");
    for none in [None, Some(""), Some("  \n ")] {
        assert_eq!(super::failed_reason(caption, none, &ShippedMeasure), caption, "{none:?}");
    }
}

/// **The "no server yet" reason keeps its two lines for every name.** Line 1 is always the
/// sentence naming the account and fits the reason column (it never wraps); a short name is left
/// alone; a long one is shortened with an ellipsis and the sentence keeps its final period; line 2
/// is always the plain no-server sentence; a blank name has no line 1 at all, so the caller says
/// `browse.auth.no_servers` instead. Graded in every shipped language with the device's advances.
#[test]
fn the_signed_in_reason_names_the_account_on_one_line_for_every_name_length() {
    use plx_machine::machine::Measure;
    let column = StatusOverlay::REASON_W * HEADROOM;
    for language in [Preference::En, Preference::Es, Preference::Be] {
        let _guard = language_on_this_thread_for_test(language);
        let second = msg::browse_auth_no_servers();
        for name in ["alexandra", "alexandra.konstantinopolskaya",
            "alexandra.konstantinopolskaya.with.a.very.long.name.indeed",
            "ААААААААААААААААААААААААААААААААААААААААААААААААААААААААААААААААААААААААААААААА"] {
            let reason = super::signed_in_reason(name, &ShippedMeasure).expect("a name gives a reason");
            let (first, rest) = reason.split_once('\n').unwrap_or_else(|| panic!("{language:?} {name}: no break"));
            assert_eq!(rest, second, "{language:?} {name}: line 2 is the plain sentence");
            assert!(!rest.contains('\n') && !first.contains('\n'), "{language:?} {name}");
            let width = ShippedMeasure.width_str(first, crate::ui::theme::size::BODY, false);
            assert!(width <= column, "{language:?} {name}: line 1 is {width}px of {column}px: {first:?}");
            assert!(first.ends_with('.'), "{language:?} {name}: the sentence keeps its period: {first:?}");
            if name == "alexandra" {
                assert_eq!(reason, msg::browse_auth_no_servers_signed_in_as(name), "a short name is untouched");
            }
            if first.contains('\u{2026}') {
                assert!(!first.ends_with("\u{2026}."), "{language:?} {name}: the ellipsis sits against the period: {first:?}");
                let at = first.find('\u{2026}').unwrap();
                assert!(at > 3 && first[at + 3..].chars().count() > 3, "{language:?} {name}: not cut in the middle: {first:?}");
                assert!(name.chars().count() > 20, "{language:?} {name}: shortened without need");
                assert!(!first.contains(name), "{language:?}: the full name survived");
            } else {
                assert!(first.contains(name), "{language:?} {name}: {first:?}");
            }
        }
        // the long name really was cut, in the language that says the most around it
        let cut = super::signed_in_reason("alexandra.konstantinopolskaya.with.a.very.long.name.indeed", &ShippedMeasure).unwrap();
        assert!(cut.lines().next().unwrap().contains('\u{2026}'), "{language:?}: {cut:?}");
        // Cut from the middle, both ends of the name kept, the period left alone.
        if language == Preference::En {
            assert_eq!(super::signed_in_reason("Maximilian.Wolfgang.Kowalczyk.MMWWMMWWMMWWMMWWAB", &ShippedMeasure).as_deref(),
                Some("Signed in as Maximilian.Wolfgang.\u{2026}.MMWWMMWWMMWWMMWWAB.\nThis Plex account has no server yet."),
                "a 48-character name is cut in the middle, not before the sentence's period");
        }
        assert_eq!(super::signed_in_reason("", &ShippedMeasure), None);
        assert_eq!(super::signed_in_reason("  \n\t ", &ShippedMeasure), None);
        // whitespace inside a name cannot break the first line
        let spaced = super::signed_in_reason("Alex\nandra  K", &ShippedMeasure).unwrap();
        assert_eq!(spaced.matches('\n').count(), 1, "{spaced:?}");
    }
}

/// **A column too narrow for even the bare sentence names nobody.** The reason is `None`, so the
/// caller shows `browse.auth.no_servers`, rather than the full unshortened name overflowing.
#[test]
fn the_signed_in_reason_is_none_when_the_sentence_leaves_the_name_no_room() {
    struct Wide;
    impl plx_machine::machine::Measure for Wide {
        fn width(&self, text: &std::ffi::CStr, _size: i32, _bold: bool) -> f32 { text.to_bytes().len() as f32 * 100.0 }
        fn cap_h(&self, size: i32) -> f32 { size as f32 * 0.7 }
        fn line_h(&self, size: i32) -> f32 { size as f32 * 1.2 }
    }
    assert_eq!(super::signed_in_reason("alexandra", &Wide), None);
}

// ---- moved from `auth`'s discovery tests ---------------------------------------------------
//
// These grade `auth`'s read-out copy against `StatusOverlay`'s reason column, so they need the UI
// library and sit in the first layer that has both.

/// **Every insecure-only reason fits the read-out's two-line slot, and names its action.** The
/// failed read-out's reason is drawn at `StatusOverlay::REASON_W` and never grows past two lines
/// (`StatusOverlay::reason_view`); a longer one is cut, and the part cut is the tail — which is
/// where every one of these says what to do. The host has no LG font, so the check is twofold: the
/// fixture measurer's wrap at the real width and size, and a character budget
/// ([`READOUT_REASON_BUDGET`]) that holds with room for the real face's wider glyphs. The shared
/// forms are measured with a long owner name. The owner-approved
/// `discovery_insecure_only_message()` predates the budget and is kept byte-identical; it is held to
/// the measured wrap only.
#[test]
fn every_insecure_only_reason_fits_two_lines_and_names_its_action() {
    use crate::auth::{discovery_insecure_only_message, plaintext_copy, ReadoutSurface};
    use crate::plex::grant::PlaintextVerdict;
    use crate::plex::probe::PlaintextEligibility;
    use crate::plex::session::PlaintextChoice;
    use crate::ui::text_view::TextView;
    const READOUT_REASON_BUDGET: usize = 125;
    let fits = |text: &str| !TextView::new(text, crate::ui::theme::size::BODY, crate::ui::theme::TEXT_SECONDARY)
        .max_lines(2)
        .with_measure(&crate::ui::fixture::FixtureMeasure)
        .truncates(StatusOverlay::REASON_W);
    assert!(fits(discovery_insecure_only_message()));
    let mut seen = 0;
    for owner in ["", "a-longish-owner18"] {
        for eligibility in [PlaintextEligibility::Eligible, PlaintextEligibility::NotLocal,
            PlaintextEligibility::NotPrivateAddress, PlaintextEligibility::HttpsAnswered] {
            for choice in [PlaintextChoice::Undecided, PlaintextChoice::Allowed, PlaintextChoice::Declined,
                PlaintextChoice::Revoked] {
                for surface in [ReadoutSurface::SignIn, ReadoutSurface::SignedIn] {
                    let v = PlaintextVerdict {
                        machine_id: "m".into(), name: "nas".into(), shared_by: owner.into(),
                        eligibility, choice,
                    };
                    let copy = plaintext_copy(Some(&v), surface);
                    if copy == discovery_insecure_only_message() { continue; }
                    seen += 1;
                    assert!(copy.chars().count() <= READOUT_REASON_BUDGET, "{} chars: {copy}", copy.chars().count());
                    assert!(fits(&copy), "wraps past two lines: {copy}");
                    if v.offers() {
                        assert!(copy.contains("without encryption") && copy.contains("this network")
                            || copy.contains("Settings \u{2192} Unencrypted connections")
                            || copy.contains("Try again"), "{copy}");
                        let action = match (choice, surface) {
                            (PlaintextChoice::Undecided, _) => "Select Connect",
                            (PlaintextChoice::Allowed, _) | (_, ReadoutSurface::SignIn) => "Select Try again",
                            (_, ReadoutSurface::SignedIn) => "Settings \u{2192} Unencrypted connections",
                        };
                        assert!(copy.contains(action), "{choice:?}/{surface:?}: {copy}");
                    }
                }
            }
        }
    }
    assert!(seen > 20);
}

#[test]
fn localized_plaintext_copy_preserves_owner_names_and_the_complete_named_action() {
    use crate::auth::{plaintext_copy_in, ReadoutSurface};
    use crate::i18n::LocaleContext;
    use crate::plex::grant::PlaintextVerdict;
    use crate::plex::probe::PlaintextEligibility;
    use crate::plex::session::PlaintextChoice;
    use crate::ui::text_view::TextView;
    use std::ffi::CStr;
    /// `FixtureMeasure`'s half-em advance per UNICODE SCALAR rather than per UTF-8 byte. Its byte
    /// count doubles every Cyrillic letter, so it would grade Belarusian against a font no device
    /// has; this grades every locale on exactly the advance the English copy is held to.
    struct ScalarMeasure;
    impl plx_machine::machine::Measure for ScalarMeasure {
        fn width(&self, text: &CStr, size: i32, _bold: bool) -> f32 {
            text.to_string_lossy().chars().count() as f32 * size as f32 * 0.5
        }
        fn cap_h(&self, size: i32) -> f32 { size as f32 * 0.7 }
        fn line_h(&self, size: i32) -> f32 { size as f32 * 1.2 }
    }
    for (preference, connect, retry, settings_path) in [
        (Preference::En, "Connect", "Try again", "Settings → Unencrypted connections"),
        (Preference::Es, "Conectar", "Reintentar", "Ajustes → Conexiones sin cifrar"),
        (Preference::Be, "Злучыцца", "Паспрабаваць зноў", "Налады → Злучэнні без шыфравання"),
    ] {
        let locale = LocaleContext::resolve(preference, None, None, None, None);
        for owner in ["", "a-longish-owner18"] {
            for choice in [PlaintextChoice::Undecided, PlaintextChoice::Allowed,
                PlaintextChoice::Declined, PlaintextChoice::Revoked] {
                for surface in [ReadoutSurface::SignIn, ReadoutSurface::SignedIn] {
                    let verdict = PlaintextVerdict { machine_id: "fixture".into(), name: "fixture".into(),
                        shared_by: owner.into(), eligibility: PlaintextEligibility::Eligible, choice };
                    let text = plaintext_copy_in(Some(&verdict), surface, &locale);
                    let action = match (choice, surface) {
                        (PlaintextChoice::Undecided, _) => connect,
                        (PlaintextChoice::Allowed, _) | (_, ReadoutSurface::SignIn) => retry,
                        (_, ReadoutSurface::SignedIn) => settings_path,
                    };
                    assert!(text.contains(action), "{preference:?}/{choice:?}/{surface:?}: {text}");
                    if !owner.is_empty() {
                        assert_eq!(text.matches(owner).count(), 1, "the owner remains literal metadata");
                        if preference != Preference::En {
                            assert!(!text.contains("’s"), "English possessive grammar must not leak: {text}");
                        }
                    }
                    assert!(text.chars().count() <= 125, "reason budget: {preference:?}: {text}");
                    assert!(!TextView::new(&text, crate::ui::theme::size::BODY, crate::ui::theme::TEXT_SECONDARY)
                        .max_lines(2).with_measure(&ScalarMeasure)
                        .truncates(StatusOverlay::REASON_W), "complete action must fit: {text}");
                }
            }
        }
    }
}
