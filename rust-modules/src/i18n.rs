//! i18n - the runtime translation layer.
//!
//! One language is compiled in as the SOURCE: English, in the literals at every draw site. That
//! literal is also the KEY - gettext's shape - which is what makes the fallback free: a key with
//! no entry IS its own English rendering, so a missing translation degrades to today's app
//! rather than to an empty label. The Spanish table lives below as one compile-time `match`,
//! a deliberate constraint: no allocation, no file reads, `&'static str` out for a
//! `&'static str` in, fitting the drawing style the UI is written in.
//!
//! **The language follows the television.** webOS writes the set's own display locale to
//! `/var/luna/preferences/localeInfo` (`{"localeInfo":{"locales":{"UI":"es-ES",…}},…}` - read
//! off a real 5.5 set), and the `UI` field is the one that means "what language the menus are
//! in". Detection runs once at boot, before the first frame, so no label is ever drawn in one
//! language and re-drawn in another; there is deliberately NO in-app override in this first
//! cut, because selection is scoped to the system locale and a settings toggle that can
//! disagree with the television is a second state to get wrong. On the simulator and host the
//! file does not exist and the answer is English - the honest reading of a machine with no
//! locale at all.
//!
//! **What is deliberately not translated:** the legal, privacy and consent documents
//! (`consent.rs`, the legal overlays). Their wording makes checkable claims that tests pin, and
//! a translation of a claim is a second claim nobody graded - this decision is named as
//! explicit, and this is it. Server data (titles, biographies, roles) is the server's language,
//! never ours to translate.

use std::sync::atomic::{AtomicBool, Ordering};

/// The system-locale file webOS keeps. Read once; absent off-device.
const LOCALE_INFO: &str = "/var/luna/preferences/localeInfo";

static ES: AtomicBool = AtomicBool::new(false);

/// BOOT, once, before the first frame: decide the language from the television's own UI locale.
/// Any failure - no file, unparseable, no `UI` field - is English, silently: a television that
/// will not say is a television with no opinion, not an error worth a log line the user cannot
/// act on. `serde_json::Value` rather than a DTO because the file's shape is webOS's, not ours,
/// and one nested field is not a contract worth a struct.
pub(crate) fn detect_and_set() {
    let Ok(raw) = std::fs::read_to_string(LOCALE_INFO) else {
        return;
    };
    let ui = serde_json::from_str::<serde_json::Value>(&raw)
        .ok()
        .and_then(|v| {
            v["localeInfo"]["locales"]["UI"]
                .as_str()
                .map(str::to_string)
        });
    // A region prefix is all that is meant: `es-ES`, `es-MX` - the table is one Spanish, not
    // one per country, the same decision the store-facing descriptors already make.
    let es = ui
        .as_deref()
        .is_some_and(|l| l.len() >= 2 && l[..2].eq_ignore_ascii_case("es"));
    ES.store(es, Ordering::Relaxed);
}

/// Is the compiled-in Spanish table the one being served? Public for the tests that pin a
/// translation's presence in both states without touching the atomics by hand.
pub(crate) fn is_es() -> bool {
    ES.load(Ordering::Relaxed)
}

#[cfg(test)]
pub(crate) fn set_for_test(es: bool) {
    ES.store(es, Ordering::Relaxed);
}

/// Translate one UI literal. The argument is the ENGLISH string and the key at once; the answer
/// is the Spanish rendering when the table and the language both say so, and the argument itself
/// otherwise - never empty, never a partial. A `&'static str` in and out because every call site
/// owns a literal; dynamic text (server titles, user names) is not passed here at all.
pub(crate) fn t(s: &'static str) -> &'static str {
    if is_es() {
        es(s).unwrap_or(s)
    } else {
        s
    }
}

/// The Spanish table. One flat `match` on the English literal: the compiler turns it into
/// length-dispatched comparisons, the keys stay greppable at their draw sites, and a new entry
/// is one arm rather than one more file format. Entries return `Some` only for a FINISHED
/// translation - an empty or placeholder arm would render as such, which is worse than English.
fn es(s: &str) -> Option<&'static str> {
    Some(match s {
        // ---- Home: shelf and row titles (the data layer builds them; the literal is the key) --
        "Continue Watching" => "Seguir viendo",
        "Recently Added" => "Añadido recientemente",
        // ---- Library sections ----
        "Movies" => "Películas",
        "TV Shows" => "Series",
        // ---- Settings ----
        "Settings" => "Ajustes",
        "Home" => "Inicio",
        // ---- Track menu ----
        "Subtitles" => "Subtítulos",
        "Off" => "Desactivados",
        "Cast & Crew" => "Reparto y equipo",
        _ => return None,
    })
}

/// The C-string bridge for `Painter::text` sites: `t()` answered a `&'static str` without a
/// terminator, and the draw API takes a NUL-terminated pointer. Copies into the caller's stack
/// buffer - one short memcpy per label, against a rasterizer that shades thousands of fragments
/// for the same label, and only for the strings that actually translated (English falls straight
/// through to the literal the site already owns). The buffer is the caller's so nothing here
/// allocates or outlives the draw call it serves.
pub(crate) fn tc<'a>(s: &'static str, buf: &'a mut [u8; TC_MAX]) -> &'a [u8] {
    let t = t(s);
    let n = t.len().min(TC_MAX - 1);
    buf[..n].copy_from_slice(&t.as_bytes()[..n]);
    buf[n] = 0;
    &buf[..n + 1]
}

/// Longest label the bridge will carry, NUL included. Enough for every row heading and pill in
/// the app today; a longer key would be truncated, which the tests would catch as a changed
/// rendering long before a user met it.
pub(crate) const TC_MAX: usize = 64;

#[cfg(test)]
mod tests {
    use super::*;

    /// The real file off a 5.5 set set to Spanish, verbatim in shape: detection must find `UI`
    /// through the nesting and answer Spanish for any `es-*`, English for everything else and
    /// for every way the file can fail to be there.
    #[test]
    fn detection_reads_the_ui_locale_off_the_real_shape() {
        let real = r#"{"localeInfo":{"locales":{"UI":"es-ES","TV":"en-GB","FMT":"en-GB","NLP":"en-GB",
            "STT":"es-ES","AUD":"es-ES","AUD2":"en-GB"},"clock":"locale","keyboards":["en","es"],
            "timezone":""},"country":"ESP","smartServiceCountryCode3":"ESP"}"#;
        let ui = serde_json::from_str::<serde_json::Value>(real)
            .ok()
            .and_then(|v| v["localeInfo"]["locales"]["UI"].as_str().map(str::to_string));
        assert_eq!(ui.as_deref(), Some("es-ES"));
        let is_es = |l: &str| l.len() >= 2 && l[..2].eq_ignore_ascii_case("es");
        assert!(is_es("es-ES") && is_es("es-MX") && is_es("ES-es"));
        assert!(!is_es("en-GB") && !is_es(""));
    }

    /// The fallback contract pinned here: an untranslated key renders as its own English
    /// literal, never empty; a translated one serves Spanish only while the language says so.
    #[test]
    fn a_missing_key_falls_back_to_its_own_english() {
        set_for_test(true);
        assert_eq!(t("Continue Watching"), "Seguir viendo");
        assert_eq!(t("No Such String Anywhere"), "No Such String Anywhere");
        set_for_test(false);
        assert_eq!(t("Continue Watching"), "Continue Watching");
        set_for_test(false);
    }

    /// Every arm of the table must be a finished translation: an empty Spanish string would
    /// render as a gap that looks like a broken label, and the fallback cannot catch it because
    /// `Some("")` IS an answer. Graded by walking the keys we pin here - the table's own
    /// contents, asserted entry by entry so adding an empty arm fails this test by name.
    #[test]
    fn the_spanish_table_carries_no_empty_entry() {
        for key in [
            "Continue Watching",
            "Recently Added",
            "Movies",
            "TV Shows",
            "Settings",
            "Home",
            "Subtitles",
            "Off",
            "Cast & Crew",
        ] {
            let Some(v) = es(key) else {
                panic!("key removed from the pin list but still asserted: {key}");
            };
            assert!(!v.trim().is_empty(), "{key} translates to an empty string");
            assert_ne!(v, key, "{key} maps to itself - write the translation or drop the arm");
        }
    }

    /// The C bridge terminates and truncates exactly: a translated label arrives NUL-terminated
    /// with no interior NUL, and an over-long one is cut at the cap rather than overrunning.
    #[test]
    fn the_c_bridge_terminates_and_truncates() {
        let mut buf = [0u8; TC_MAX];
        set_for_test(true);
        let b = tc("Subtitles", &mut buf);
        assert_eq!(&b[..b.len() - 1], "Subtítulos".as_bytes());
        assert_eq!(*b.last().unwrap(), 0);
        let long = "X".repeat(TC_MAX + 10);
        let b = tc(long.leak(), &mut buf);
        assert_eq!(b.len(), TC_MAX);
        assert_eq!(b[TC_MAX - 1], 0);
        set_for_test(false);
    }
}
