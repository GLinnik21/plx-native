//! Tests for the previewed/identifier/policy documents this screen shows — what each
//! channel's preview and identifier text may and may not contain.

use super::*;
#[allow(unused_imports)]
use super::test_support::*;

// ---- the payload previews --------------------------------------------------------------

/// **The preview is the real payload.** Every event this build can emit, and every one of its
/// fields, must appear in front of the person being asked to consent to it.
#[test]
fn the_preview_shows_every_event_this_build_can_emit() {
    let text = preview();
    for s in crate::diag::schema::EVENT_SPECS {
        assert!(text.contains(s.name), "the payload preview does not show `{}`", s.name);
        for f in s.fields {
            assert!(
                text.contains(f.key),
                "the payload preview does not show `{}`'s field `{}`",
                s.name,
                f.key
            );
        }
    }
    for f in crate::diag::schema::CONTEXT_SPECS {
        assert!(text.contains(f.key), "the payload preview does not show context field `{}`", f.key);
    }
    for crash_field in ["stacktrace", "registers", "threads", "debug_meta", "image_size"] {
        assert!(text.contains(crash_field), "the native crash schema omits `{crash_field}`");
    }
    for fallback_field in ["C fault fallback", "Rust panic fallback", "fingerprint", "culprit"] {
        assert!(text.contains(fallback_field), "the fallback schema omits `{fallback_field}`");
    }
    for handled_field in [
        "Handled playback error",
        "handled",
        "breadcrumbs",
        "phase",
        "outcome",
        "requested_quality",
        "declared_rate",
        "media_rate",
        "picture presented",
        "seek requested",
        "quality selected",
        "delivery requested",
        "HLS request committed",
        "Original check phase",
        "playback failed",
    ] {
        assert!(text.contains(handled_field), "the handled playback-error schema omits `{handled_field}`");
    }
}

/// **The two identifier documents each name only their own channel's identifier.**
#[test]
fn each_identifier_document_shows_only_its_own_identifier() {
    let _g = crate::testlock::serial();
    let saved = consent::current();
    let errors_id = "e".repeat(32);
    let analytics_id = "a".repeat(32);
    let mut draws = 0;
    consent::install(consent::apply(&Consent::default(), true, true, || {
        draws += 1;
        Some(if draws == 1 { errors_id.clone() } else { analytics_id.clone() })
    }));
    let errors_doc = errors_id_document();
    let analytics_doc = analytics_id_document();
    assert!(errors_doc.contains(&errors_id) && !errors_doc.contains(&analytics_id));
    assert!(analytics_doc.contains(&analytics_id) && !analytics_doc.contains(&errors_id));
    assert!(errors_doc.contains(CONTACT_EMAIL) && analytics_doc.contains(CONTACT_EMAIL));

    consent::install(consent::apply(&Consent::default(), false, false, || None));
    assert!(errors_id_document().starts_with("NO CRASH REPORT ID"));
    assert!(analytics_id_document().starts_with("NO ANALYTICS ID"));
    restore_consent_snapshot(saved);
}

/// **Item 14's whole point: the crash document carries nothing from the usage channel.**
#[test]
fn the_crash_preview_carries_nothing_from_the_usage_channel() {
    let text = preview_crash();
    assert!(!text.contains("distinct_id"), "no PostHog envelope field belongs in the crash-only document");
    assert!(!text.contains("<project key>"), "no PostHog project key belongs in the crash-only document");
    for s in crate::diag::schema::EVENT_SPECS {
        let quoted = format!("\"event\": \"{}\"", s.name);
        assert!(!text.contains(&quoted), "usage event `{}` leaked into the crash preview", s.name);
    }
}

/// The mirror image: no Sentry envelope shape belongs in the usage document.
#[test]
fn the_usage_preview_carries_nothing_from_the_crash_channel() {
    let text = preview_usage();
    for sentry_only in [
        "exception",
        "stacktrace",
        "registers",
        "threads",
        "debug_meta",
        "Handled playback error",
        "C fault fallback",
        "Rust panic fallback",
    ] {
        assert!(!text.contains(sentry_only), "crash-only field `{sentry_only}` leaked into the usage preview");
    }
}

/// The one property in the payload a reader could not otherwise verify.
#[test]
fn the_preview_shows_the_anonymity_flag() {
    assert!(preview().contains("$process_person_profile"));
    assert!(preview().contains("false"));
}

/// **The preview carries no real identifier**, on either route: first run cannot mint one
/// before the usage answer, and Settings may already hold one but must never expose it here.
#[test]
fn the_preview_cannot_contain_a_real_identifier() {
    let text = preview();
    assert!(text.contains("created only when product analytics is enabled"), "the intro explains the placeholder");
    assert!(text.contains("<random id>"), "and the field itself is a placeholder");
    assert!(text.contains("created only when crash reports are enabled"), "the crash intro explains its placeholder");
    assert!(
        text.contains(crate::telemetry::native::PREVIEW_USER_ID),
        "and the crash-report id is shown as a placeholder"
    );
    let bytes: Vec<char> = text.chars().collect();
    let run = bytes
        .windows(32)
        .any(|w| w.iter().all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()));
    assert!(!run, "the preview contains something shaped like a real install id");
}

/// **The two Privacy-policy doors must open ONE document.** The legacy screen carried a
/// SECOND literal that had already drifted from `ui::legal`'s own copy — the bug the original
/// test was written against. This page cannot drift the same way: `PreviewKind::Policy`
/// builds its text by calling `legal::privacy_policy()` directly rather than holding a copy,
/// so the two are one function rather than two strings a reviewer has to keep in sync. The
/// assertion is kept anyway, as a regression pin on the WIRING — "it can't drift, it's the
/// same call" is exactly the kind of claim that quietly stops being true the next time
/// someone "simplifies" the match arm.
#[test]
fn both_privacy_policy_doors_open_the_same_document() {
    let idx = PreviewKind::ALL.iter().position(|k| *k == PreviewKind::Policy).unwrap() as u8;
    let page = PreviewPage::new(EntryId(1), idx);
    // `super::super::` rather than `crate::screens::`: from inside `mod tests` (nested one
    // level under `consent`) the relative spelling is two hops — `tests` -> `consent` ->
    // `screens` — which is the same distinction `screens::family`'s own test module documents
    // for the identical trap. The absolute form is what `ci/check-deps.sh`'s `sibling` gate
    // greps for; this is the same call, unrewritten in every other respect.
    assert_eq!(page.text, super::super::legal::privacy_policy());
}

// ---- first run: the answers are the action row, not table rows ------------------------

/// **The two answers are the ACTION ROW, and the list is only what you may read first.**
/// Pinned because the failure is silent and cosmetic-looking: put an answer back among the
/// rows and the screen still works, it just stops distinguishing deciding from reading.
#[test]
fn first_run_leaves_only_the_two_documents_in_its_reading_list() {
    let m = FixtureMeasure;
    let c = test_cx(&m);
    let (mut out, mut present) = sink();
    let crash = ConsentPage::first_run(EntryId(1), 0, &c, &mut mk_fx(&mut out, &mut present));
    assert_eq!(
        crash.rows,
        vec![RowId::PreviewCrash, RowId::Policy],
        "only the two readable documents remain in the list, and the preview is the crash \
         channel's own — Stage::Crash is where a fresh question always starts"
    );
    assert_eq!(crash.table.n_rows(), 2);
    assert_eq!(crash.band_labels().len(), 2, "first run always carries its two answers in the band");
}
