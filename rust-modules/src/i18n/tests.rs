use super::*;
#[test]
fn locale_resolution_separates_language_from_formatting() {
    let regional = LocaleContext::resolve(Preference::System, Some("en-GB"), None, None, None);
    assert_eq!(regional.format_locale(), "en-GB");
    assert_eq!(regional.date(2026, 9, 27).as_deref(), Some("27/09/2026"));
    let cx = LocaleContext::resolve(
        Preference::Be,
        Some("es-ES"),
        Some("de-DE"),
        Some("24"),
        None,
    );
    assert_eq!(cx.format_locale(), "de-DE");
    assert_eq!(cx.language(), Language::Be);
    assert_eq!(cx.preference(), Preference::Be);
    assert_eq!(cx.number(12345), "12.345");
    assert_eq!(cx.decimal(45, 1), "4,5");
    assert_eq!(cx.clock(), Clock::H24);
    assert_eq!(cx.date(2026, 9, 27).as_deref(), Some("27.09.2026"));
    assert_eq!(
        LocaleContext::resolve(Preference::System, Some("es_MX"), None, None, None).language(),
        Language::Es
    );
    assert_eq!(
        LocaleContext::resolve(Preference::System, None, None, None, Some("be_BY.UTF-8"))
            .language(),
        Language::Be
    );
    for tag in ["fr-FR", "be-Latn", "garbage\r\nheader", "C"] {
        assert_eq!(
            LocaleContext::resolve(Preference::System, Some(tag), None, None, None).language(),
            Language::En
        );
    }
}
#[test]
fn plural_rules_follow_the_message_language() {
    let be = LocaleContext::resolve(Preference::Be, None, Some("en-US"), None, None);
    for n in [1, 21, 101] {
        assert_eq!(be.plural(n), PluralCategory::One);
    }
    for n in [2, 3, 4, 22, 103] {
        assert_eq!(be.plural(n), PluralCategory::Few);
    }
    for n in [0, 5, 11, 12, 14, 20, 111] {
        assert_eq!(be.plural(n), PluralCategory::Many);
    }
    let mut fraction = Decimal::from(15);
    fraction.multiply_pow10(-1);
    assert_eq!(be.plural.category_for(&fraction), PluralCategory::Other);
    let es = LocaleContext::resolve(Preference::Es, None, None, None, None);
    assert_eq!(es.plural(1), PluralCategory::One);
    assert_eq!(es.plural(2), PluralCategory::Other);
    assert_eq!(es.plural(1_000_000), PluralCategory::Many);
}

#[test]
fn belarusian_library_count_messages_render_the_reviewed_forms() {
    let locale = LocaleContext::resolve(Preference::Be, None, Some("en-GB"), None, None);
    for (count, expected) in [
        (1, "1 фільм"),
        (2, "2 фільмы"),
        (5, "5 фільмаў"),
        (11, "11 фільмаў"),
        (21, "21 фільм"),
    ] {
        assert_eq!(msg::browse_person_films_in(&locale, count), expected);
    }
}
#[test]
fn preferences_soft_parse_without_losing_the_session() {
    for raw in [
        r#"{}"#,
        r#"{"language":"future"}"#,
        r#"{"language":42}"#,
        r#"{"language":null}"#,
    ] {
        let s: crate::plex::session::Session = serde_json::from_str(raw).unwrap();
        assert_eq!(s.language, Preference::System);
    }
    let s: crate::plex::session::Session = serde_json::from_str(r#"{"language":"be"}"#).unwrap();
    assert_eq!(s.language, Preference::Be);
    assert_eq!(serde_json::to_value(&s).unwrap()["language"], "be");
}
#[test]
fn settings_replies_are_typed_and_refusals_do_not_look_like_success() {
    let s=parse_reply(r#"{"returnValue":true,"settings":{"localeInfo":{"locales":{"UI":"be_BY","FMT":"es-ES"},"clock":"12"}}}"#).unwrap();
    assert_eq!(s.ui.as_deref(), Some("be-BY"));
    assert_eq!(s.fmt.as_deref(), Some("es-ES"));
    assert_eq!(s.clock.as_deref(), Some("12"));
    for s in [
        "",
        "not json",
        r#"{"returnValue":false}"#,
        r#"{"returnValue":true,"settings":{}}"#,
    ] {
        assert!(parse_reply(s).is_none());
    }
    assert!(normalize("es-ES\r\nX-Test: injected").is_none());
    assert!(normalize("POSIX").is_none());
}
#[test]
fn all_catalog_glyphs_exist_in_both_shipped_text_faces() {
    for name in ["appfont.ttf", "appfont-bold.ttf"] {
        let cov = crate::fontcov::shipped(name).as_ref().unwrap();
        for s in msg::CORPUS.iter().copied().chain(["ІіЎўЁё’"]) {
            for ch in s.chars().filter(|c| !c.is_control()) {
                assert!(
                    cov.contains(ch as u32),
                    "{name}: missing {ch:?} U+{:04X}",
                    ch as u32
                );
            }
        }
    }
}

#[test]
fn pseudo_locale_exercises_expansion_without_changing_arguments() {
    let mut cx = LocaleContext::resolve(Preference::En, None, None, None, None);
    cx.language = Language::Pseudo;
    let text = msg::core_shared_by_in(&cx, "name {literal}");
    assert!(text.starts_with("[!! "));
    assert!(text.contains("name {literal}"));
    assert!(msg::core_system_default_in(&cx).len() > "System default".len());
    assert!(!msg::settings_language_contribute_body_in(&cx).contains("github.com"),
        "only the guide caption belongs to the translated catalog, never its address");
}
#[test]
fn captured_language_changes_the_recorded_session_hash() {
    use crate::ui::machine::LogicalState;
    let a = crate::auth::SessionInit::captured(crate::plex::session::Session::default());
    let mut b = a.clone();
    b.persisted.language = Preference::Be;
    assert_ne!(a.hash(), b.hash());
}
