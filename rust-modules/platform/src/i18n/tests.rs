use super::*;
#[test]
fn the_webos_release_line_reads_unknown_rather_than_inventing_a_release() {
    let known = crate::tv::device::Info { release: "4.10.2".into(), major: 4, ..Default::default() };
    assert_eq!(webos_release_line(&known), "webOS 4.10.2");
    assert_eq!(webos_release_line(&crate::tv::device::Info::default()), msg::browse_diagnostics_unknown_os());
}
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
    for tag in ["fr-FR", "fr_CA", "fr-BE"] {
        let cx = LocaleContext::resolve(Preference::System, Some(tag), None, None, None);
        assert_eq!(cx.language(), Language::Fr);
        assert_eq!(cx.preference(), Preference::System);
    }
    assert_eq!(
        LocaleContext::resolve(Preference::Fr, None, None, None, None).format_locale(),
        "fr-FR"
    );
    for tag in ["de-DE", "be-Latn", "fr-Cyrl", "garbage\r\nheader", "C"] {
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
    let fr = LocaleContext::resolve(Preference::Fr, None, None, None, None);
    assert_eq!(fr.plural(0), PluralCategory::One);
    assert_eq!(fr.plural(1), PluralCategory::One);
    assert_eq!(fr.plural(2), PluralCategory::Other);
    assert_eq!(fr.plural(1_000_000), PluralCategory::Many);
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
        let cov = plx_base::fontcov::shipped(name).as_ref().unwrap();
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

const MONTHS_EN_MEDIUM: [&str; 12] =
    ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
const MONTHS_EN_LONG: [&str; 12] = [
    "January", "February", "March", "April", "May", "June", "July", "August", "September",
    "October", "November", "December",
];
const MONTHS_ES_MEDIUM: [&str; 12] =
    ["ene", "feb", "mar", "abr", "may", "jun", "jul", "ago", "sept", "oct", "nov", "dic"];
const MONTHS_ES_LONG: [&str; 12] = [
    "enero", "febrero", "marzo", "abril", "mayo", "junio", "julio", "agosto", "septiembre",
    "octubre", "noviembre", "diciembre",
];
const MONTHS_FR_MEDIUM: [&str; 12] =
    ["janv.", "févr.", "mars", "avr.", "mai", "juin", "juil.", "août", "sept.", "oct.", "nov.", "déc."];
const MONTHS_FR_LONG: [&str; 12] = [
    "janvier", "février", "mars", "avril", "mai", "juin", "juillet", "août", "septembre",
    "octobre", "novembre", "décembre",
];
const MONTHS_BE_GENITIVE: [&str; 12] = [
    "студзеня", "лютага", "сакавіка", "красавіка", "траўня", "чэрвеня", "ліпеня", "жніўня",
    "верасня", "кастрычніка", "лістапада", "снежня",
];

fn month_captions(preference: Preference) -> Vec<(String, String)> {
    let cx = LocaleContext::resolve(preference, None, None, None, None);
    (1..=12)
        .map(|m| {
            (
                cx.month_day(m, 12, MonthWidth::Medium).unwrap(),
                cx.month_day(m, 12, MonthWidth::Long).unwrap(),
            )
        })
        .collect()
}

#[test]
fn english_month_day_captions_cover_every_month_at_both_widths() {
    let captions = month_captions(Preference::En);
    for (i, (medium, long)) in captions.iter().enumerate() {
        assert_eq!(medium, &format!("12 {}", MONTHS_EN_MEDIUM[i]));
        assert_eq!(long, &format!("12 {}", MONTHS_EN_LONG[i]));
    }
    assert_eq!(captions[2], ("12 Mar".to_string(), "12 March".to_string()));
}

#[test]
fn spanish_month_day_captions_use_de_only_in_the_long_width() {
    let captions = month_captions(Preference::Es);
    for (i, (medium, long)) in captions.iter().enumerate() {
        assert_eq!(medium, &format!("12 {}", MONTHS_ES_MEDIUM[i]));
        assert_eq!(long, &format!("12 de {}", MONTHS_ES_LONG[i]));
    }
    assert_eq!(captions[2].1, "12 de marzo");
}

#[test]
fn french_month_day_captions_cover_every_month_at_both_widths() {
    let captions = month_captions(Preference::Fr);
    for (i, (medium, long)) in captions.iter().enumerate() {
        assert_eq!(medium, &format!("12 {}", MONTHS_FR_MEDIUM[i]));
        assert_eq!(long, &format!("12 {}", MONTHS_FR_LONG[i]));
    }
    assert_eq!(captions[0].0, "12 janv.");
}

#[test]
fn belarusian_long_month_day_captions_take_the_genitive() {
    let captions = month_captions(Preference::Be);
    for (i, (_, long)) in captions.iter().enumerate() {
        assert_eq!(long, &format!("12 {}", MONTHS_BE_GENITIVE[i]));
    }
    assert_eq!(captions[2].1, "12 сакавіка");
    let cx = LocaleContext::resolve(Preference::Be, None, None, None, None);
    assert_eq!(cx.month_day(1, 1, MonthWidth::Long).as_deref(), Some("1 студзеня"));
    assert_eq!(cx.month_day(12, 31, MonthWidth::Long).as_deref(), Some("31 снежня"));
    // CLDR `be` abbreviated format: three letters, no dot, "мая" for May.
    assert_eq!(cx.month_day(3, 12, MonthWidth::Medium).as_deref(), Some("12 сак"));
    assert_eq!(cx.month_day(5, 9, MonthWidth::Medium).as_deref(), Some("9 мая"));
    assert_eq!(cx.month_day(4, 1, MonthWidth::Medium).as_deref(), Some("1 кра"));
    assert!(captions.iter().all(|(medium, _)| !medium.ends_with('.')));
}

#[test]
fn pseudo_locale_month_day_captions_follow_english() {
    let pseudo = LocaleContext::pseudo_for_test();
    assert_eq!(pseudo.month_day(3, 12, MonthWidth::Medium).as_deref(), Some("12 Mar"));
    assert_eq!(pseudo.month_day(3, 12, MonthWidth::Long).as_deref(), Some("12 March"));
}

#[test]
fn month_day_rejects_days_the_calendar_does_not_have() {
    let cx = LocaleContext::resolve(Preference::En, None, None, None, None);
    for (month, day) in [(0, 1), (13, 1), (1, 0), (1, 32), (2, 30), (4, 31), (6, 31)] {
        assert_eq!(cx.month_day(month, day, MonthWidth::Long), None, "{month}/{day}");
    }
    // The year is unknown, so 29 February is a real caption.
    assert_eq!(cx.month_day(2, 29, MonthWidth::Long).as_deref(), Some("29 February"));
}

#[test]
fn watchlist_strings_render_in_every_shipped_language() {
    let be = LocaleContext::resolve(Preference::Be, None, None, None, None);
    assert_eq!(msg::watchlist_action_add_in(&be), "Дадаць у спіс для прагляду");
    assert_eq!(msg::watchlist_action_on_list_in(&be), "У спісе для прагляду");
    let date = be.month_day(3, 12, MonthWidth::Long).unwrap();
    assert_eq!(msg::watchlist_state_coming_on_in(&be, &date), "Выйдзе 12 сакавіка");
    let es = LocaleContext::resolve(Preference::Es, None, None, None, None);
    let date = es.month_day(3, 12, MonthWidth::Long).unwrap();
    assert_eq!(msg::watchlist_state_coming_on_in(&es, &date), "Estreno el 12 de marzo");
    assert_eq!(msg::watchlist_action_remove_show_in(&es), "Quitar serie…");
    let fr = LocaleContext::resolve(Preference::Fr, None, None, None, None);
    let date = fr.month_day(3, 12, MonthWidth::Medium).unwrap();
    assert_eq!(msg::watchlist_state_releasing_on_in(&fr, &date), "Sortie le 12 mars");
    let en = LocaleContext::resolve(Preference::En, None, None, None, None);
    assert_eq!(msg::watchlist_page_sort_filter_summary_in(&en, "Default"), "Sort & Filter · Default");
    for preference in SHIPPED {
        let cx = LocaleContext::resolve(preference, None, None, None, None);
        assert!(!msg::watchlist_page_title_in(&cx).is_empty());
        assert!(!msg::watchlist_empty_list_in(&cx).is_empty());
        assert!(!msg::watchlist_empty_filtered_in(&cx).is_empty());
    }
}

/// Every Watchlist key the English catalog declares is present, non-empty and carries a
/// translator description, in all four shipped catalogs. The compiler already refuses a key
/// set that differs between languages; this also pins the shape of the files themselves.
#[test]
fn every_watchlist_key_exists_in_all_four_catalogs() {
    let catalogs = [
        ("en", include_str!("../../../../locales/en/watchlist.json")),
        ("es", include_str!("../../../../locales/es/watchlist.json")),
        ("be", include_str!("../../../../locales/be/watchlist.json")),
        ("fr", include_str!("../../../../locales/fr/watchlist.json")),
    ];
    let parse = |text: &str| -> serde_json::Map<String, serde_json::Value> {
        serde_json::from_str(text).expect("valid watchlist catalog")
    };
    let english = parse(catalogs[0].1);
    assert!(english.len() >= 30, "the Watchlist catalog shrank to {}", english.len());
    for (key, source) in &english {
        assert!(key.starts_with("watchlist."), "{key} is outside the Watchlist scope");
        assert!(
            source["description"].as_str().is_some_and(|d| !d.trim().is_empty()),
            "{key} has no translator description"
        );
    }
    for (tag, text) in &catalogs[1..] {
        let catalog = parse(text);
        let keys = |m: &serde_json::Map<String, serde_json::Value>| {
            m.keys().cloned().collect::<std::collections::BTreeSet<_>>()
        };
        assert_eq!(keys(&catalog), keys(&english), "{tag} watchlist keys differ from English");
        for (key, value) in &catalog {
            // A plural message is an object of forms; every form must be filled.
            let filled = |v: &serde_json::Value| v.as_str().is_some_and(|s| !s.trim().is_empty());
            let ok = match value.as_object() {
                Some(forms) => !forms.is_empty() && forms.values().all(filled),
                None => filled(value),
            };
            assert!(ok, "{tag} {key} is empty");
        }
    }
}
