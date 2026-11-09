//! The bus-identity ladder: which name an attempt speaks to `com.webos.media` as. No transport, clock,
//! worker or registry is read; the product driver (`backend`) and the developer probe (`uriprobe`)
//! both ask this one decision and carry out the answer.
//!
//! The first rung is the named client (`Shape::Named`), the name the Developer Mode role file grants
//! outbound permission to the media service. A set that does not grant it refuses in one of two
//! places: the registration itself (`-1027 Invalid permissions`), or the first call the registered
//! name makes (the `load`, answered "Denied method call"). Either is one refused rung, and the one
//! rung after it is the plain anonymous client the rest of `ls2` uses. A refusal of the anonymous
//! rung ends the ladder: there is no third identity. A rung whose first call was accepted settles
//! it; the caller never asks again for that attempt.

use plx_platform::tv::uri::Shape;
use plx_playback_contract::Failure;

/// The identities, in the order they are tried.
pub const RUNGS: [Shape; 2] = [Shape::Named, Shape::Anon];

/// How one rung came out.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// The identity registered and the media service took its first call.
    Granted,
    /// The identity was refused at register, or its first call was denied.
    Refused,
}

/// What the attempt does next.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Step {
    /// Register as this identity (the next rung, or the first when nothing has been tried).
    Register(Shape),
    /// This identity was granted: the attempt goes on as it.
    Settled(Shape),
    /// Every rung was refused: the attempt fails with this, the refusal a single refused
    /// registration always reported.
    Failed(Failure),
}

/// The rung an attempt starts on.
pub fn first() -> Shape {
    RUNGS[0]
}

/// The step after the rungs `tried` so far, one [`Outcome`] per rung in [`RUNGS`] order.
pub fn decide(tried: &[Outcome]) -> Step {
    for (rung, outcome) in RUNGS.iter().zip(tried) {
        if *outcome == Outcome::Granted {
            return Step::Settled(*rung);
        }
    }
    match RUNGS.get(tried.len()) {
        Some(rung) => Step::Register(*rung),
        None => Step::Failed(Failure::Refused),
    }
}

/// The word a log line uses for a rung.
pub fn label(shape: Shape) -> &'static str {
    match shape {
        Shape::Named => "named",
        Shape::Anon => "anon",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use Outcome::{Granted, Refused};

    #[test]
    fn nothing_tried_registers_the_named_client_first() {
        assert_eq!(decide(&[]), Step::Register(Shape::Named));
        assert_eq!(first(), Shape::Named);
    }

    #[test]
    fn a_refused_named_client_gives_way_to_the_anonymous_one_once() {
        assert_eq!(decide(&[Refused]), Step::Register(Shape::Anon));
    }

    #[test]
    fn a_granted_rung_settles_the_ladder_on_that_identity() {
        assert_eq!(decide(&[Granted]), Step::Settled(Shape::Named));
        assert_eq!(decide(&[Refused, Granted]), Step::Settled(Shape::Anon));
    }

    #[test]
    fn both_rungs_refused_is_the_typed_refusal_and_never_a_third_identity() {
        assert_eq!(decide(&[Refused, Refused]), Step::Failed(Failure::Refused));
        assert_eq!(decide(&[Refused, Refused, Refused]), Step::Failed(Failure::Refused));
    }

    #[test]
    fn the_rungs_are_the_probes_measured_order_with_no_third() {
        assert_eq!(RUNGS, [Shape::Named, Shape::Anon]);
        assert_eq!((label(Shape::Named), label(Shape::Anon)), ("named", "anon"));
    }
}
