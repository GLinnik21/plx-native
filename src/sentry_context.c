#include <sentry.h>

/*
 * Keep sentry_value_t on the C side of the Rust/C boundary. It is an opaque
 * union whose by-value ABI is easy to model incorrectly on 32-bit ARM; this
 * narrow wrapper exposes only ordinary NUL-terminated strings to Rust.
 *
 * This context deliberately describes firmware, not hardware. Model, board,
 * serial numbers and LG's device identifier are outside the crash schema.
 */
void
plx_sentry_set_webos_context(const char *name, const char *release,
    const char *codename, const char *api)
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
}
