//! The television's window and bus, as the frame loop drives them: the SDL window and Wayland
//! surface, the opaque-region hint, the platform bus pump and the frame probe. Each call is a hop
//! through the installed port; with no port installed every one is a no-op.
use std::os::raw::c_void;

/// Hand the port the SDL window, once, at boot — `textinput::bind`'s shape and for its reason: the
/// window is created deep inside `plex_run` and the platform needs it a long way from there.
pub(crate) fn bind_window(win: *mut c_void) {
    (super::port().bind_window)(win)
}

/// Take the surface the window presents through.
pub(crate) fn grab(win: *mut c_void) {
    (super::port().grab_surface)(win)
}

/// Let go of the surface, without destroying SDL's objects; foreground takes it again.
pub(crate) fn release() {
    (super::port().release_surface)()
}

/// Arm the opaque-region hint at boot.
pub(crate) fn arm_opaque_region() {
    (super::port().arm_opaque_region)()
}

/// Route the opaque region for the frame about to present: `player` is whether the video plane
/// shows through.
pub(crate) fn opaque_route(player: bool) {
    (super::port().opaque_route)(player)
}

/// Drop the opaque-region hint.
pub(crate) fn clear_opaque_region() {
    (super::port().clear_opaque_region)()
}

/// Service the platform bus once; the frame loop calls this every frame.
pub(crate) fn pump_bus() {
    (super::port().pump_bus)()
}

/// Ask for this frame's presentation callback. Call on a PRESENTING frame, before the swap.
pub(crate) fn frame_probe_request() {
    (super::port().frame_probe_request)()
}

/// The frame's first framebuffer command is next: the wait for a back buffer starts here.
pub(crate) fn frame_probe_waiting() {
    (super::port().frame_probe_waiting)()
}

/// The frame's first framebuffer command has returned: the back buffer is acquired.
pub(crate) fn frame_probe_acquired() {
    (super::port().frame_probe_acquired)()
}

/// The probe's log fields since the previous line; `""` unarmed.
pub(crate) fn frame_probe_fields() -> String {
    (super::port().frame_probe_fields)()
}
