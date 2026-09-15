#include <sentry.h>

/*
 * Keep sentry_value_t on the C side of the Rust/C boundary. It is an opaque
 * union whose by-value ABI is easy to model incorrectly on 32-bit ARM; this
 * narrow wrapper exposes only ordinary NUL-terminated strings and integer scalars to Rust.
 *
 * Firmware and hardware compatibility classes are separate contexts. Neither
 * accepts a serial number, LG device identifier, address or account value.
 * The only identity the scope ever carries is the random crash-report id
 * below, and only as `user.id`.
 */
void
plx_sentry_set_webos_context(const char *name, const char *release,
    const char *codename, const char *api, const char *model,
    const char *soc, const char *hardware_revision, const char *rtkmem,
    const char *install)
{
    sentry_value_t context = sentry_value_new_object();
    sentry_value_set_by_key(
        context, "type", sentry_value_new_string("webos"));
    if (name && name[0]) {
        sentry_value_set_by_key(
            context, "name", sentry_value_new_string(name));
    }
    if (release && release[0]) {
        sentry_value_set_by_key(
            context, "release", sentry_value_new_string(release));
    }
    if (codename && codename[0]) {
        sentry_value_set_by_key(
            context, "codename", sentry_value_new_string(codename));
    }
    if (api && api[0]) {
        sentry_value_set_by_key(
            context, "api", sentry_value_new_string(api));
    }
    sentry_set_context("webos", context);

    sentry_value_t hardware = sentry_value_new_object();
    sentry_value_set_by_key(
        hardware, "type", sentry_value_new_string("hardware"));
    if (model && model[0]) {
        sentry_value_set_by_key(
            hardware, "model", sentry_value_new_string(model));
    }
    if (soc && soc[0]) {
        sentry_value_set_by_key(
            hardware, "soc", sentry_value_new_string(soc));
    }
    if (hardware_revision && hardware_revision[0]) {
        sentry_value_set_by_key(hardware, "revision",
            sentry_value_new_string(hardware_revision));
    }
    /*
     * issue #74: two more closed-enum sandbox facts, from the same source
     * as PostHog's usage envelope (never a free-text probe result, never
     * the install path) — so a chassis's crash-at-start rate is queryable
     * by sandbox on both the Sentry and PostHog sides.
     */
    if (rtkmem && rtkmem[0]) {
        sentry_value_set_by_key(
            hardware, "rtkmem", sentry_value_new_string(rtkmem));
    }
    if (install && install[0]) {
        sentry_value_set_by_key(
            hardware, "install", sentry_value_new_string(install));
    }
    sentry_set_context("hardware", hardware);
}

/*
 * The crash-report identifier, as Sentry's `user.id` and nothing else of
 * `user`. A NULL or empty id clears it. Each call makes the native backend
 * rewrite the daemon's base-event file, which is how the value reaches a
 * report for a fault this process never gets to handle.
 */
void
plx_sentry_set_user_id(const char *id)
{
    if (!id || !id[0]) {
        sentry_remove_user();
        return;
    }
    sentry_value_t user = sentry_value_new_object();
    sentry_value_set_by_key(user, "id", sentry_value_new_string(id));
    sentry_set_user(user);
}

/* Sparse, typed window diagnostics. The caller accepts only closed stage labels and optional
 * booleans / SDL version bytes. Negative integers mean absent. No sentry_value_t crosses FFI. */
void
plx_sentry_window_breadcrumb(const char *stage, int playing,
    int major, int minor, int patch, int display, int surface)
{
    sentry_value_t crumb = sentry_value_new_breadcrumb("state", stage);
    sentry_value_set_by_key(crumb, "category", sentry_value_new_string("window"));
    sentry_value_set_by_key(crumb, "level", sentry_value_new_string("info"));
    sentry_value_t data = sentry_value_new_object();
    if (playing >= 0) {
        sentry_value_set_by_key(data, "playing", sentry_value_new_bool(playing != 0));
    }
    if (display >= 0) {
        sentry_value_set_by_key(data, "display", sentry_value_new_bool(display != 0));
    }
    if (surface >= 0) {
        sentry_value_set_by_key(data, "surface", sentry_value_new_bool(surface != 0));
    }
    if (major >= 0 && minor >= 0 && patch >= 0) {
        sentry_value_set_by_key(data, "sdl_major", sentry_value_new_int32(major));
        sentry_value_set_by_key(data, "sdl_minor", sentry_value_new_int32(minor));
        sentry_value_set_by_key(data, "sdl_patch", sentry_value_new_int32(patch));
    }
    sentry_value_set_by_key(crumb, "data", data);
    sentry_add_breadcrumb(crumb);
}
