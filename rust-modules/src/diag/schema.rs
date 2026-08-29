//! **The allowlist of what may ever be reported off this television, as a TYPE.**
//!
//! `PRIVACY.md` promises that titles, ratingKeys, search terms, subtitle text, server names and
//! addresses are never sent. This file is what makes that a checkable statement rather than a
//! good intention: every reportable event is a variant of [`DiagEvent`], every field of every
//! variant is a number, a bool or a `&'static str` from a fixed table, and one exhaustive
//! serializer turns them into a wire record. **There is no field a caller can put a runtime string
//! into**, so a call site cannot leak one by accident the way `log(&format!(…))` could — which is
//! not hypothetical here, it is the bug the previous commit in this area had to fix by hand across
//! seven call sites.
//!
//! # Why an enum, and not the macro the plan first proposed
//!
//! The first design was `diag::event!(name, k = v, …)`, which stringifies an identifier at the
//! call site. That gives the *appearance* of an allowlist and none of the substance: any call site
//! could mint any event with any field, and "no free-text field exists in the type" was false
//! because there was no type. A reviewer caught it. One enum in one file, exhaustively matched by
//! one serializer, delivers the property the macro only claimed — and it is less code.
//!
//! # UNGATED, deliberately
//!
//! This module compiles in every build, including one with no telemetry feature at all, for the
//! reason `diag::scrub` was moved out of `lab/`: **its tests are the guarantee, and tests behind a
//! feature the default gate does not build are tests that never run.** That is not a hypothetical
//! either — `scrub`'s 31 assertions sat unexecuted for as long as they existed. What IS gated is
//! everything that would SEND one of these.
//!
//! # Adding a variant
//!
//! Three things move together and the tests fail if they do not: the variant, its arm in
//! [`serialize`], and its row in `PRIVACY.md`'s schema table. That last one is
//! [`the_privacy_document_lists_every_event`], and it is the mechanism behind the promise that the
//! document is written *before* the thing ships rather than after.
//!
//! **A variant may not carry a `String`.** [`no_variant_can_carry_a_runtime_string`] greps this
//! file for one. If a new event genuinely needs text, it needs a bounded enum instead — that is
//! the whole design, and the answer to "but this one is safe" is that every leak in this
//! repository's history was written by somebody who had just finished thinking that.

/// One reportable event. See the module doc before adding a variant.
///
/// **Device facts are deliberately absent.** Model, board, firmware, app version and locale are
/// per-SESSION constants, not per-event facts: they belong in the envelope a sender builds once,
/// not repeated on every record. Putting them here would also have been the plan's own
/// over-fingerprinting finding arriving by the back door — a stable, rare, identifying tuple
/// attached to every single event.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DiagEvent {
    /// The app reached its event loop. A marker with no fields — "how many launches" is the
    /// question, and everything that would qualify it is a session constant.
    AppLaunch,
    /// A screen was entered. `screen` comes from `app.rs`'s own route-name table, the same
    /// `&'static str` the heartbeat and the lab envelope print, so the three cannot disagree.
    RouteEntered { screen: &'static str },
    /// plex.tv sign-in finished and the app reached Home or the profile picker.
    ///
    /// **No outcome field, and that is a finding rather than a simplification.** The plan called
    /// for `signin.completed` / `signin.failed` with a reason enum; going to wire it, the failure
    /// sites turn out not to exist. `auth::pin_denied` — the obvious candidate — is about the
    /// PROFILE pin on a Plex Home switch, not about the plex.tv link code, and there is no place
    /// in the login poll that distinguishes "denied" from "expired" from "the network went away".
    /// So a reason enum here would have been three names describing nothing.
    ///
    /// The right order is to make the app *observe* those outcomes first and report them second.
    /// Until it does, this stays a marker and the interesting half — "how many people get stuck on
    /// the QR screen" — is answered by the ABSENCE of this event after an `app.launch`.
    SignInCompleted,

    // ---- playback -----------------------------------------------------------------------------
    //
    // Four events for one attempt, joined by `playback_id`. Without that join a funnel cannot
    // connect a failure to the start it belongs to, and "how often does playback fail" becomes two
    // unrelated counters.
    //
    // **`playback_id` is a random number minted per attempt, and it is not an identity.** It is
    // reset every time and never stored, so it cannot link two playbacks on one television, let
    // alone two televisions. It exists to make the four events of ONE attempt joinable inside a
    // session, which is exactly as much as a funnel needs.
    //
    // **Every descriptive field is a BUCKET, and that is a privacy decision rather than a
    // simplification.** Exact duration + exact raster + exact frame rate + codec is enough to
    // identify a specific file in a specific library; the same fields as classes answer every
    // question this channel exists to answer ("does 4K HEVC fail more than 1080p h264") and
    // identify nothing.
    /// **A viewer pressed Play.** The denominator: `started / requested` is the success rate, and a
    /// `requested` with no `started` after it is the failure this channel exists to see — the one
    /// an owner reports as "it just sat there".
    ///
    /// It fires at the PRESS, not where the plan lands, and carries no `mode` as a consequence. The
    /// first draft put it at the engine's `load:` seam, which a `/decision` refusal never reaches —
    /// so the earliest and most certain failure there is would have produced a `failed` with no
    /// `requested` before it, i.e. a funnel that silently under-counts exactly the case it was
    /// built for. Direct-play-versus-transcode is not yet knowable at the press; both `started` and
    /// `failed` carry it, which is where the question is actually asked.
    PlaybackRequested { playback_id: i64 },
    /// The first frame of this attempt reached the panel — the transition into `Playing`, once.
    PlaybackStarted {
        playback_id: i64,
        mode: &'static str,
        /// `sd` / `hd` / `fhd` / `uhd`, never the raster.
        raster: &'static str,
        /// A fixed rung, never the measured rate.
        fps: &'static str,
        video: &'static str,
        audio: &'static str,
        /// How long from `requested` to a picture, as a class.
        startup: &'static str,
    },
    /// This attempt failed, once. `kind` is `player::FailureKind`'s stable code — never the
    /// on-screen wording, which is prose and will be re-worded.
    PlaybackFailed { playback_id: i64, mode: &'static str, kind: &'static str },
    /// A real teardown — the viewer stopped, or the item ran out. **Not** a seek, a reload or an
    /// app-switch suspend, all of which end an ENGINE without ending a playback.
    PlaybackEnded { playback_id: i64, mode: &'static str, watched: &'static str },
}

/// A serialised field value. The set is the whole vocabulary, and the important thing about it is
/// what is ABSENT: there is no `String` arm, which is what makes "a runtime string cannot reach
/// the wire" a property of the type rather than a rule people remember to follow.
///
/// **`Int` arrived with the playback events, exactly as this doc predicted it would** — and it
/// carries only `playback_id`, which is a random number minted per attempt. `Bool` is still not
/// here: nothing declared today is a flag, and an arm no variant produces is a vocabulary that
/// describes nothing, which is how an allowlist stops being one.
///
/// Note what `Int` did NOT bring with it. The raster and the frame rate `playback.started` reports
/// are `Str` buckets, not numbers, for the reason that event's own comment gives.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Value {
    /// From a fixed table in this crate — never a runtime-built string. The guarantee is not the
    /// `'static` lifetime, which a leaked allocation would satisfy; it is that every producer in
    /// [`serialize`] is a literal or a `match` over an enum.
    Str(&'static str),
    /// A number. The only producer today is `playback_id` — see [`DiagEvent`]'s playback block for
    /// why a random per-attempt integer is not an identifier.
    Int(i64),
}

/// One event's name and fields, ready for a sender to wrap in whatever envelope it needs.
///
/// Returning the pair rather than a JSON string keeps this file free of any one vendor's format:
/// the Sentry and PostHog bodies differ in shape, and both are built from this. That was a
/// prediction when this function was written and is now load-bearing — `telemetry::posthog` calls
/// it to build both of PostHog's two endpoint shapes, which put the identity in different places,
/// from one description of the event. It carried an `#[allow(dead_code)]` until that caller
/// existed; the attribute is gone rather than left behind, because a stale allowance is how a
/// genuinely dead function later hides in plain sight.
pub(crate) fn serialize(e: DiagEvent) -> (&'static str, Vec<(&'static str, Value)>) {
    match e {
        DiagEvent::AppLaunch => ("app.launch", Vec::new()),
        DiagEvent::RouteEntered { screen } => ("route.entered", vec![("screen", Value::Str(screen))]),
        DiagEvent::SignInCompleted => ("signin.completed", Vec::new()),
        DiagEvent::PlaybackRequested { playback_id } => {
            ("playback.requested", vec![("playback_id", Value::Int(playback_id))])
        }
        DiagEvent::PlaybackStarted { playback_id, mode, raster, fps, video, audio, startup } => (
            "playback.started",
            vec![
                ("playback_id", Value::Int(playback_id)),
                ("mode", Value::Str(mode)),
                ("raster", Value::Str(raster)),
                ("fps", Value::Str(fps)),
                ("video", Value::Str(video)),
                ("audio", Value::Str(audio)),
                ("startup", Value::Str(startup)),
            ],
        ),
        DiagEvent::PlaybackFailed { playback_id, mode, kind } => (
            "playback.failed",
            vec![
                ("playback_id", Value::Int(playback_id)),
                ("mode", Value::Str(mode)),
                ("kind", Value::Str(kind)),
            ],
        ),
        DiagEvent::PlaybackEnded { playback_id, mode, watched } => (
            "playback.ended",
            vec![
                ("playback_id", Value::Int(playback_id)),
                ("mode", Value::Str(mode)),
                ("watched", Value::Str(watched)),
            ],
        ),
    }
}

/// Every event name this build can emit, for the document test and for a sender that wants to
/// declare its schema up front. Kept beside [`serialize`] so a new variant that forgets one is
/// caught by the round-trip test rather than by a reader.
#[allow(dead_code)] // still test-only: its caller is a schema declaration the sender has not
// needed yet, unlike `serialize`, which the PostHog body now calls for real
pub(crate) const EVENT_NAMES: &[&str] = &[
    "app.launch",
    "route.entered",
    "signin.completed",
    "playback.requested",
    "playback.started",
    "playback.failed",
    "playback.ended",
];

#[cfg(test)]
mod tests {
    use super::*;

    /// One instance of every variant. A new variant that is not added here makes the exhaustive
    /// match below fail to compile, which is the point — this list cannot silently fall behind.
    fn every_variant() -> Vec<DiagEvent> {
        // The match is what forces this list to stay complete: adding a variant without adding it
        // here is a compile error, not a quietly weaker test.
        let all = vec![
            DiagEvent::AppLaunch,
            DiagEvent::RouteEntered { screen: "home" },
            DiagEvent::SignInCompleted,
            DiagEvent::PlaybackRequested { playback_id: 7 },
            DiagEvent::PlaybackStarted {
                playback_id: 7,
                mode: "direct",
                raster: "fhd",
                fps: "24",
                video: "h264",
                audio: "ac3",
                startup: "1-3s",
            },
            DiagEvent::PlaybackFailed { playback_id: 7, mode: "transcode", kind: "no_video_track" },
            DiagEvent::PlaybackEnded { playback_id: 7, mode: "direct", watched: "most" },
        ];
        for e in &all {
            match e {
                DiagEvent::AppLaunch => {}
                DiagEvent::RouteEntered { .. } => {}
                DiagEvent::SignInCompleted => {}
                DiagEvent::PlaybackRequested { .. } => {}
                DiagEvent::PlaybackStarted { .. } => {}
                DiagEvent::PlaybackFailed { .. } => {}
                DiagEvent::PlaybackEnded { .. } => {}
            }
        }
        all
    }

    /// Every variant serialises, to a name that is in [`EVENT_NAMES`], with no duplicate field
    /// keys. The name list is what a sender declares and what the privacy document lists, so a
    /// variant whose name is missing from it would ship undeclared.
    #[test]
    fn every_variant_serialises_to_a_declared_name() {
        for e in every_variant() {
            let (name, fields) = serialize(e);
            assert!(EVENT_NAMES.contains(&name), "{name} is not in EVENT_NAMES");
            let mut keys: Vec<&str> = fields.iter().map(|(k, _)| *k).collect();
            keys.sort_unstable();
            let n = keys.len();
            keys.dedup();
            assert_eq!(keys.len(), n, "{name} has a duplicate field key");
        }
    }

    /// …and every declared name is actually produced by some variant. Otherwise `EVENT_NAMES`
    /// grows names nothing emits, and a sender declaring a schema would announce events that
    /// cannot happen.
    #[test]
    fn every_declared_name_is_produced_by_a_variant() {
        let produced: Vec<&str> = every_variant().into_iter().map(|e| serialize(e).0).collect();
        for n in EVENT_NAMES {
            assert!(produced.contains(n), "{n} is declared but no variant produces it");
        }
    }

    /// **THE GUARANTEE.** No variant may carry a `String`, and no field value may be one — that is
    /// what makes `PRIVACY.md`'s "titles, search terms and server names are not included" a
    /// statement about the TYPE rather than about how careful the call sites are.
    ///
    /// Greps this file's own source, in the same spirit as
    /// `diag::scrub`'s `no_log_call_site_interpolates_viewing_content`: the property is structural
    /// and no unit test of behaviour can express it, because the failure is a variant that does
    /// not exist yet.
    #[test]
    fn no_variant_can_carry_a_runtime_string() {
        let src = include_str!("schema.rs");
        // Only the declaration region — the test module below legitimately handles strings.
        let decls = src.split("#[cfg(test)]").next().expect("the file has a body");
        for (i, line) in decls.lines().enumerate() {
            let code = line.split("//").next().unwrap_or("");
            assert!(
                !code.contains("String") && !code.contains("Cow<"),
                "line {} introduces an owned string into the schema: {line}",
                i + 1
            );
        }
    }

    /// The privacy document lists every event this build can emit.
    ///
    /// `PRIVACY.md` promises, as a binding term, that the literal structure sent is documented
    /// there *before* it ships. This is that promise as a test: add a variant, and the document
    /// has to gain a row in the same change or `make check` fails. It reads the file from the
    /// repository root rather than embedding a copy, so the two cannot drift.
    #[test]
    fn the_privacy_document_lists_every_event() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("rust-modules has a parent")
            .join("PRIVACY.md");
        let doc = std::fs::read_to_string(&root).expect("PRIVACY.md is readable");
        for n in EVENT_NAMES {
            assert!(
                doc.contains(&format!("`{n}`")),
                "PRIVACY.md does not list the event `{n}` — the document is the promise, so it \
                 changes in the same commit as the schema"
            );
        }
    }
}
