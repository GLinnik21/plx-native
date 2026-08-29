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

/// **The registry: one declaration per event, carrying its name, its fields and what each field
/// may hold.** `PRIVACY.md`'s schema table is rendered from this and checked against it, and so is
/// [`serialize`].
///
/// It replaced a NAME LIST plus a grep. The name list caught a variant that shipped undeclared and
/// could say nothing about fields — so a field added to an existing event changed what leaves the
/// television with no document, and no test, noticing. And the in-app notice was guarded by
/// grepping `src/telemetry` for a call to `net::post_ca`, which is a proxy for "can this build
/// send" and answers nothing about WHAT. A registry is the thing itself: the promise `PRIVACY.md`
/// makes is about fields and their domains, so that is what is declared, once, here.
///
/// `domain` is prose and it is the column a reader of the privacy document actually reads. It is
/// held beside the field rather than in the document because the document is the OUTPUT — written
/// there, the two drift, and the one that goes stale is the one nobody compiles.
///
/// **`#[cfg(test)]`, and that is the honest shape rather than a compromise.** This is a
/// SPECIFICATION, checked against the implementation; at runtime [`serialize`] *is* the schema, and
/// nothing needs a second copy of it in the shipped binary. The alternative was an
/// `#[allow(dead_code)]`, and this file's own doc argues against exactly that: a standing allowance
/// is how a genuinely dead declaration later hides in plain sight. Every comparison that gives this
/// registry its value — against `serialize`, against `PRIVACY.md`, against the consent screen's
/// preview — is a test, and `make check` runs them all.
#[cfg(test)]
pub(crate) const EVENT_SPECS: &[EventSpec] = &[
    EventSpec { name: "app.launch", fields: &[] },
    EventSpec {
        name: "route.entered",
        fields: &[F { key: "screen", domain: "one of a fixed list of screen names" }],
    },
    EventSpec { name: "signin.completed", fields: &[] },
    EventSpec { name: "playback.requested", fields: &[F { key: "playback_id", domain: PLAYBACK_ID }] },
    EventSpec {
        name: "playback.started",
        fields: &[
            F { key: "playback_id", domain: PLAYBACK_ID },
            F { key: "mode", domain: MODE },
            F { key: "raster", domain: "`sd` / `hd` / `fhd` / `uhd` / `unknown` — never the raster" },
            F { key: "fps", domain: "a fixed rung: `24`/`25`/`30`/`50`/`60`/`100`/`other`/`unknown` — never the measured rate" },
            F { key: "video", domain: "a codec name from a fixed table; anything else is `other`" },
            F { key: "audio", domain: "a codec name from a fixed table; anything else is `other`" },
            F { key: "startup", domain: "`<1s` / `1-3s` / `3-10s` / `10s+` — never the interval" },
        ],
    },
    EventSpec {
        name: "playback.failed",
        fields: &[
            F { key: "playback_id", domain: PLAYBACK_ID },
            F { key: "mode", domain: MODE },
            F { key: "kind", domain: "`decision_refused` / `no_video_transcode_target` / `no_video_track` / `unspecified`" },
        ],
    },
    EventSpec {
        name: "playback.ended",
        fields: &[
            F { key: "playback_id", domain: PLAYBACK_ID },
            F { key: "mode", domain: MODE },
            F { key: "watched", domain: "`abandoned` / `some` / `most` / `finished` — never a position or a duration" },
        ],
    },
];

#[cfg(test)]
const PLAYBACK_ID: &str = "a random number minted per attempt, never stored and never reused";
#[cfg(test)]
const MODE: &str = "`direct` or `transcode`";

/// One event's contract. See [`EVENT_SPECS`].
#[cfg(test)]
pub(crate) struct EventSpec {
    pub name: &'static str,
    pub fields: &'static [F],
}

/// One field's contract: its key, and what it may hold in the words the privacy document prints.
#[cfg(test)]
pub(crate) struct F {
    pub key: &'static str,
    pub domain: &'static str,
}

/// The schema table exactly as `PRIVACY.md` carries it. **The document is the OUTPUT** — a test
/// asserts the file contains this verbatim and prints the block on failure, so the fix to a stale
/// document is a paste rather than an act of authorship.
#[cfg(test)]
pub(crate) fn privacy_table() -> String {
    let mut out = String::from("| event | fields |\n|---|---|\n");
    for spec in EVENT_SPECS {
        let fields = if spec.fields.is_empty() {
            "*(none)*".to_string()
        } else {
            spec.fields
                .iter()
                .map(|f| format!("`{}` — {}", f.key, f.domain))
                .collect::<Vec<_>>()
                .join("; ")
        };
        out.push_str(&format!("| `{}` | {fields} |\n", spec.name));
    }
    out
}

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

    /// **What is SENT is exactly what is DECLARED — name and every field key, in order.**
    ///
    /// This is what the old name list could not do. It caught a variant that shipped with no
    /// declaration and said nothing about fields, so adding a field to an existing event changed
    /// what leaves the television with neither the document nor a test noticing. The registry makes
    /// that a compile-adjacent failure: the serialiser and the declaration are compared key by key.
    #[test]
    fn every_variant_sends_exactly_the_fields_it_declares() {
        for e in every_variant() {
            let (name, fields) = serialize(e);
            let spec = EVENT_SPECS
                .iter()
                .find(|s| s.name == name)
                .unwrap_or_else(|| panic!("{name} is not declared in EVENT_SPECS"));
            let sent: Vec<&str> = fields.iter().map(|(k, _)| *k).collect();
            let declared: Vec<&str> = spec.fields.iter().map(|f| f.key).collect();
            assert_eq!(sent, declared, "{name} sends fields it does not declare, or vice versa");
            let mut keys = sent.clone();
            keys.sort_unstable();
            let n = keys.len();
            keys.dedup();
            assert_eq!(keys.len(), n, "{name} has a duplicate field key");
        }
    }

    /// …and every declared event is actually produced by some variant. Otherwise the registry — and
    /// with it the privacy document — grows entries describing events that cannot happen, which is
    /// a different kind of untrue document from an incomplete one and no better.
    #[test]
    fn every_declared_event_is_produced_by_a_variant() {
        let produced: Vec<&str> = every_variant().into_iter().map(|e| serialize(e).0).collect();
        for s in EVENT_SPECS {
            assert!(produced.contains(&s.name), "{} is declared but no variant produces it", s.name);
        }
    }

    /// **No field's declared domain may name a thing that must never be sent.** A cheap check on
    /// prose, and it is aimed at the one way a registry could rot into decoration: somebody adds a
    /// field whose domain honestly says "the item title", the document renders it, and the row
    /// reads as though it had been reviewed.
    #[test]
    fn no_declared_domain_admits_content() {
        for s in EVENT_SPECS {
            for f in s.fields {
                let d = f.domain.to_ascii_lowercase();
                for banned in
                    ["title", "search", "query", "path", "url", "address", "rating key", "server name"]
                {
                    assert!(
                        !d.contains(banned),
                        "{}.{} declares a domain mentioning {banned:?}: {}",
                        s.name,
                        f.key,
                        f.domain
                    );
                }
            }
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
        // **Exactly the region the claim is about**: the event type, the value vocabulary and the
        // serialiser. It used to be everything above the test module, which is wider than the
        // property — `privacy_table` renders a DOCUMENT and legitimately returns a `String`, and a
        // guard that has to be argued with is one somebody eventually deletes. Narrowing it here
        // rather than adding an exemption keeps the failure meaning one thing.
        let from = src.find("pub(crate) enum DiagEvent").expect("the event type");
        let to = src.find("pub(crate) const EVENT_SPECS").expect("the registry");
        let decls = &src[from..to];
        for (i, line) in decls.lines().enumerate() {
            let code = line.split("//").next().unwrap_or("");
            assert!(
                !code.contains("String") && !code.contains("Cow<"),
                "line {} of the schema's declaration region introduces an owned string: {line}",
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
    fn the_privacy_document_carries_the_generated_table() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("rust-modules has a parent")
            .join("PRIVACY.md");
        let doc = std::fs::read_to_string(&root).expect("PRIVACY.md is readable");
        let want = privacy_table();
        assert!(
            doc.contains(&want),
            "PRIVACY.md's schema table is not the one this build would send. The document is the \
             OUTPUT of `diag::schema::EVENT_SPECS`, so the fix is to paste the block below over \
             the table in PRIVACY.md — not to edit the registry to match the document.\n\n{want}"
        );
    }
}
