//! **Diagnostics plumbing shared by every channel that reports something off this device.**
//!
//! Three pieces, lifted out of `lab/` on 2026-08-29 when a second consumer appeared. They were
//! written for the Cloud Lab bridge, they were correct, and none of them was lab-shaped:
//!
//! * [`scrub`] — the redaction pass. **Ungated**, because `crate::log` calls
//!   [`scrub::scrub_local`] on every line in every build. See that module's doc for why there are
//!   two exits and why only the remote one may drop a line.
//! * [`ring`] — the bounded in-memory record ring, tapped one call below `redact_tokens`.
//! * [`zlib`] — `dlopen`'d `compress2` plus a gzip envelope, in its own one-symbol table.
//!
//! **`ring` and `zlib` stay behind a feature, `scrub` does not.** A build with neither
//! `lab-diagnostics` nor `telemetry` has nothing to put in a ring and nothing to compress, but it
//! still writes a log file — and the whole point of moving `scrub` here was that its assertions
//! run in the default `make check`, which `lab/`'s cfg had been quietly excluding them from.
//!
//! There is ONE scrubber. If a future channel needs different redaction, it takes a different exit
//! from this module rather than a second implementation of it.

pub(crate) mod scrub;

// Gated to their present consumer. Phase G/H widen these to
// `any(feature = "lab-diagnostics", feature = "telemetry")` when the Sentry and PostHog clients
// become second callers — deliberately not widened ahead of a caller, because `warnings = "deny"`
// turns "compiled but unused" into a build error, and that is the check doing the work here.
#[cfg(feature = "lab-diagnostics")]
pub(crate) mod ring;
#[cfg(feature = "lab-diagnostics")]
pub(crate) mod zlib;
