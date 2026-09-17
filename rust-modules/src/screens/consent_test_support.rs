//! Shared fixtures and helpers for the `screens::consent` test modules split out below.

use super::*;

pub(super) use crate::screens::registry::BAND;
pub(super) use crate::ui::fixture::FixtureMeasure;
pub(super) use crate::ui::machine::{
    Edge, FocusRead, InputOwner, InstanceId, PressId, PressRead, PresentHandle, Source, Stamped, Tick,
};
pub(super) use crate::ui::present::Present;

/// A bare `Cx<InnerHost>` for constructing or stepping a page with no SDL, no GL and no
/// television. Consent does not inspect the retained Browse directory, so the explicit empty
/// fixture is sufficient; the nested host still carries the same view type as production.
pub(super) fn test_cx(m: &FixtureMeasure) -> Cx<'_, InnerHost> {
    Cx {
        views: crate::stores::browse::DirectoryView::empty_for_test(),
        tick: Tick::default(),
        measure: m,
        press: PressRead::default(),
        focus: FocusRead::default(),
        owner: InputOwner::Entry(EntryId(0)),
    }
}


/// A fresh effects sink's two halves, owned by the test so a page's `step`/constructor can be
/// called more than once against the SAME buffer — the shape `RouteSurface::run_inner` builds
/// for a real inner page (`Effects::from_handle`), minus the surface around it. The instance
/// id is a placeholder: every `Fx::Deliver` this file emits targets one for the same reason
/// `row_commit`'s Delete arm does — whatever surface forwards it re-addresses it to itself.
pub(super) fn sink() -> (Vec<Stamped<InnerHost>>, Present) {
    (Vec::new(), Present::new())
}

pub(super) fn mk_fx<'a>(out: &'a mut Vec<Stamped<InnerHost>>, present: &'a mut Present) -> Effects<'a, InnerHost> {
    Effects::from_handle(out, MachineId::Instance(InstanceId(0)), PresentHandle::of(present))
}


pub(super) fn key_down(key: Key) -> ScreenEvent<InnerHost> {
    ScreenEvent::Input(InputEvent {
        at: Tick::default(),
        source: Source::Script,
        kind: InputKind::Key { key, sym: 0, wcode: 0, edge: Edge::Down, at_edge: false },
    })
}


/// The shape `ui/dispatch.rs`'s edge-rule redelivery manufactures when the band's LEFT edge
/// resolves to `EdgeRule::Nav(Back)`: the key is rewritten to `Key::Back` and `at_edge` is
/// forced `true`, which is the ONLY way a real page ever sees that combination — a person's
/// own BACK press is always built with `at_edge: false` (`app/bridge.rs`). Named for what it
/// represents rather than what it literally is, so a test reads as "LEFT off the band's
/// leading control", not as an unexplained `Key::Back`.
pub(super) fn synthetic_left_edge_back() -> ScreenEvent<InnerHost> {
    ScreenEvent::Input(InputEvent {
        at: Tick::default(),
        source: Source::Script,
        kind: InputKind::Key { key: Key::Back, sym: 0, wcode: 0, edge: Edge::Down, at_edge: true },
    })
}


/// Undo a test's temporary `consent::install`, best-effort. **This is not a full restore.**
/// `telemetry::consent` exposes `install` but no `uninstall`, so when the global held `None`
/// before the test — nobody has loaded a decision this process yet — there is no way to put
/// it back; the closest reachable state from here on is `Some(Consent::default())`, silently,
/// for every test that runs afterward in this binary. `consent::current`'s own doc says why
/// that distinction usually matters ("`None` means nothing has been loaded yet — distinct
/// from 'a decision that allows nothing'"), and one production call site really does branch on
/// it (`telemetry::mod.rs`'s `flush_soon`: `let Some(c) = consent::current() else { return }`).
/// It is harmless for every GATE in this crate, which all fail closed on `None` exactly as
/// they do on an explicit refusal — this is `[[test-suite-global-pollution]]`'s general shape,
/// not a correctness bug in the screen. The clean fix is a `#[cfg(test)] fn uninstall()` on
/// `telemetry::consent` (not this lane's file) so every call below could restore
/// unconditionally instead of only when `saved` was already `Some`; centralised here so the
/// gap is documented once rather than re-explained at each of the three call sites that used
/// to inline this same conditional.
pub(super) fn restore_consent_snapshot(saved: Option<Consent>) {
    if let Some(saved) = saved {
        consent::install(saved);
    }
}


/// Whether any effect in `out` asks the engine to re-seat on `g` — the mechanism this page
/// uses instead of moving focus itself (`row_commit`'s Delete arm, `alert_answer`, and the
/// two `request_band_focus` call sites all emit exactly this shape).
pub(super) fn requests_group(out: &[Stamped<InnerHost>], g: GroupId) -> bool {
    out.iter().any(|s| {
        matches!(
            &s.fx,
            Fx::Deliver(
                _,
                Delivery::Screen(ScreenEvent::Enter(Enter::Fresh {
                    focus: FocusTarget::ContainerGroup(got),
                })),
            ) if *got == g
        )
    })
}
