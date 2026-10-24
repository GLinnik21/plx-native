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
    for tag in ["it-IT", "it_CH", "it-SM"] {
        let cx = LocaleContext::resolve(Preference::System, Some(tag), None, None, None);
        assert_eq!(cx.language(), Language::It);
        assert_eq!(cx.preference(), Preference::System);
    }
    assert_eq!(
        LocaleContext::resolve(Preference::It, None, None, None, None).format_locale(),
        "it-IT"
    );
    // Portuguese resolves for every region except the excluded Brazilian one (registry
    // `excluded_regions`), which keeps the English fallback.
    for tag in ["pt-PT", "pt_PT.UTF-8", "pt", "pt-AO", "pt-MZ"] {
        let cx = LocaleContext::resolve(Preference::System, Some(tag), None, None, None);
        assert_eq!(cx.language(), Language::Pt, "{tag}");
        assert_eq!(cx.preference(), Preference::System);
    }
    assert_eq!(
        LocaleContext::resolve(Preference::System, Some("pt-AO"), None, None, None).format_locale(),
        "pt-AO"
    );
    let pt = LocaleContext::resolve(Preference::Pt, None, None, None, None);
    assert_eq!(pt.format_locale(), "pt-PT");
    assert_eq!(pt.decimal(45, 1), "4,5");
    for tag in ["pt-BR", "pt_BR.UTF-8", "pt-Latn-BR"] {
        assert_eq!(
            LocaleContext::resolve(Preference::System, Some(tag), None, None, None).language(),
            Language::En,
            "{tag}"
        );
    }
    for tag in ["de-DE", "de_AT.UTF-8", "de-CH", "de-LI"] {
        let cx = LocaleContext::resolve(Preference::System, Some(tag), None, None, None);
        assert_eq!(cx.language(), Language::De, "{tag}");
        assert_eq!(cx.preference(), Preference::System);
    }
    assert_eq!(
        LocaleContext::resolve(Preference::System, Some("de-AT"), None, None, None).format_locale(),
        "de-AT"
    );
    let de = LocaleContext::resolve(Preference::De, None, None, None, None);
    assert_eq!(de.format_locale(), "de-DE");
    assert_eq!(de.number(12345), "12.345");
    assert_eq!(de.decimal(45, 1), "4,5");
    for tag in ["hu-HU", "hu_HU.UTF-8", "hu"] {
        let cx = LocaleContext::resolve(Preference::System, Some(tag), None, None, None);
        assert_eq!(cx.language(), Language::Hu, "{tag}");
    }
    let hu = LocaleContext::resolve(Preference::Hu, None, None, None, None);
    assert_eq!(hu.format_locale(), "hu-HU");
    assert_eq!(hu.decimal(45, 1), "4,5");
    for tag in ["nl-NL", "be-Latn", "fr-Cyrl", "it-Cyrl", "de-Cyrl", "hu-Cyrl", "garbage\r\nheader", "C"] {
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
    // European Portuguese, not Brazilian: zero takes the plural form ("0 episódios").
    let pt = LocaleContext::resolve(Preference::Pt, None, None, None, None);
    assert_eq!(pt.plural(0), PluralCategory::Other);
    assert_eq!(pt.plural(1), PluralCategory::One);
    assert_eq!(pt.plural(2), PluralCategory::Other);
    assert_eq!(pt.plural(1_000_000), PluralCategory::Many);
    let de = LocaleContext::resolve(Preference::De, None, None, None, None);
    assert_eq!(de.plural(0), PluralCategory::Other);
    assert_eq!(de.plural(1), PluralCategory::One);
    assert_eq!(de.plural(2), PluralCategory::Other);
    assert_eq!(de.plural(1_000_000), PluralCategory::Other);
    let hu = LocaleContext::resolve(Preference::Hu, None, None, None, None);
    assert_eq!(hu.plural(0), PluralCategory::Other);
    assert_eq!(hu.plural(1), PluralCategory::One);
    assert_eq!(hu.plural(2), PluralCategory::Other);
    let it = LocaleContext::resolve(Preference::It, None, None, None, None);
    assert_eq!(it.plural(0), PluralCategory::Other);
    assert_eq!(it.plural(1), PluralCategory::One);
    assert_eq!(it.plural(2), PluralCategory::Other);
    assert_eq!(it.plural(1_000_000), PluralCategory::Many);
}

#[test]
fn russian_locale_resolves_cyrillic_tags_and_keeps_regional_formatting_independent() {
    for tag in ["ru", "ru-RU", "ru_RU.UTF-8", "ru-Cyrl-RU", "ru-KZ"] {
        let cx = LocaleContext::resolve(Preference::System, Some(tag), None, None, None);
        assert_eq!(cx.language(), Language::Ru, "{tag}");
    }
    let from_env = LocaleContext::resolve(Preference::System, None, None, None, Some("ru_RU.UTF-8"));
    assert_eq!(from_env.language(), Language::Ru);
    let ru = LocaleContext::resolve(Preference::Ru, None, None, None, None);
    assert_eq!(ru.format_locale(), "ru-RU");
    assert_eq!(ru.number(12345), "12\u{a0}345");
    assert_eq!(ru.decimal(45, 1), "4,5");
    assert_eq!(ru.date(2026, 10, 24).as_deref(), Some("24.10.2026"));
    let mixed = LocaleContext::resolve(Preference::Ru, Some("en-US"), Some("en-US"), None, None);
    assert_eq!(mixed.language(), Language::Ru);
    assert_eq!(mixed.number(12345), "12,345");
    for tag in ["ru-Latn", "ru-Latn-RU"] {
        assert_eq!(
            LocaleContext::resolve(Preference::System, Some(tag), None, None, None).language(),
            Language::En,
        );
    }
}

#[test]
fn russian_catalog_counts_follow_russian_plurals_even_with_english_formatting() {
    let ru = LocaleContext::resolve(Preference::Ru, None, Some("en-US"), None, None);
    for (count, expected, category) in [
        (0, "0 серий", PluralCategory::Many),
        (1, "1 серия", PluralCategory::One),
        (2, "2 серии", PluralCategory::Few),
        (5, "5 серий", PluralCategory::Many),
        (11, "11 серий", PluralCategory::Many),
        (12, "12 серий", PluralCategory::Many),
        (14, "14 серий", PluralCategory::Many),
        (21, "21 серия", PluralCategory::One),
        (22, "22 серии", PluralCategory::Few),
        (25, "25 серий", PluralCategory::Many),
        (101, "101 серия", PluralCategory::One),
        (111, "111 серий", PluralCategory::Many),
    ] {
        assert_eq!(ru.plural(count), category);
        assert_eq!(msg::browse_detail_episodes_in(&ru, count), expected);
    }
    let mut fraction = Decimal::from(15);
    fraction.multiply_pow10(-1);
    assert_eq!(ru.plural.category_for(&fraction), PluralCategory::Other);
}

#[test]
fn russian_episode_labels_keep_the_reviewed_latin_markers() {
    let ru = LocaleContext::resolve(Preference::Ru, None, None, None, None);
    assert_eq!(msg::core_season_in(&ru, 2), "S2");
    assert_eq!(msg::core_episode_in(&ru, 4), "E4");
    assert_eq!(msg::core_episode_ordinal_in(&ru, 4, 2), "S2, E4");
}

#[test]
fn russian_stats_menu_uses_the_native_reviewers_wording() {
    let ru = LocaleContext::resolve(Preference::Ru, None, None, None, None);
    assert_eq!(msg::widgets_menu_stats_in(&ru), "Статистика для гиков");
}

#[test]
fn russian_month_day_captions_use_format_months_at_both_widths() {
    let cx = LocaleContext::resolve(Preference::Ru, None, Some("en-US"), None, None);
    let medium = [
        "янв.", "февр.", "мар.", "апр.", "мая", "июн.", "июл.", "авг.", "сент.", "окт.", "нояб.", "дек.",
    ];
    let long = [
        "января", "февраля", "марта", "апреля", "мая", "июня", "июля", "августа", "сентября", "октября",
        "ноября", "декабря",
    ];
    for month in 1..=12 {
        let i = usize::from(month - 1);
        assert_eq!(cx.month_day(month, 12, MonthWidth::Medium), Some(format!("12 {}", medium[i])));
        assert_eq!(cx.month_day(month, 12, MonthWidth::Long), Some(format!("12 {}", long[i])));
    }
    assert_eq!(cx.month_day(2, 29, MonthWidth::Long).as_deref(), Some("29 февраля"));
    assert!(cx.month_day(2, 30, MonthWidth::Long).is_none());
    assert!(cx.month_day(0, 1, MonthWidth::Medium).is_none());
    assert!(cx.month_day(13, 1, MonthWidth::Medium).is_none());
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
const MONTHS_IT_MEDIUM: [&str; 12] =
    ["gen", "feb", "mar", "apr", "mag", "giu", "lug", "ago", "set", "ott", "nov", "dic"];
const MONTHS_IT_LONG: [&str; 12] = [
    "gennaio", "febbraio", "marzo", "aprile", "maggio", "giugno", "luglio", "agosto", "settembre",
    "ottobre", "novembre", "dicembre",
];
const MONTHS_PT_MEDIUM: [&str; 12] =
    ["jan.", "fev.", "mar.", "abr.", "mai.", "jun.", "jul.", "ago.", "set.", "out.", "nov.", "dez."];
const MONTHS_PT_LONG: [&str; 12] = [
    "janeiro", "fevereiro", "março", "abril", "maio", "junho", "julho", "agosto", "setembro",
    "outubro", "novembro", "dezembro",
];
const MONTHS_DE_MEDIUM: [&str; 12] =
    ["Jan.", "Feb.", "März", "Apr.", "Mai", "Juni", "Juli", "Aug.", "Sept.", "Okt.", "Nov.", "Dez."];
const MONTHS_DE_LONG: [&str; 12] = [
    "Januar", "Februar", "März", "April", "Mai", "Juni", "Juli", "August", "September", "Oktober",
    "November", "Dezember",
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
fn italian_month_day_captions_cover_every_month_at_both_widths() {
    let captions = month_captions(Preference::It);
    for (i, (medium, long)) in captions.iter().enumerate() {
        assert_eq!(medium, &format!("12 {}", MONTHS_IT_MEDIUM[i]));
        assert_eq!(long, &format!("12 {}", MONTHS_IT_LONG[i]));
    }
    assert_eq!(captions[2].1, "12 marzo");
}

#[test]
fn portuguese_month_day_captions_use_de_only_in_the_long_width() {
    let captions = month_captions(Preference::Pt);
    for (i, (medium, long)) in captions.iter().enumerate() {
        assert_eq!(medium, &format!("12 {}", MONTHS_PT_MEDIUM[i]));
        assert_eq!(long, &format!("12 de {}", MONTHS_PT_LONG[i]));
    }
    assert_eq!(captions[2].1, "12 de março");
}

#[test]
fn german_month_day_captions_write_the_day_as_an_ordinal() {
    let captions = month_captions(Preference::De);
    for (i, (medium, long)) in captions.iter().enumerate() {
        assert_eq!(medium, &format!("12. {}", MONTHS_DE_MEDIUM[i]));
        assert_eq!(long, &format!("12. {}", MONTHS_DE_LONG[i]));
    }
    assert_eq!(captions[2].1, "12. März");
}

#[test]
fn hungarian_month_day_captions_put_the_month_first_and_the_day_as_an_ordinal() {
    let captions = month_captions(Preference::Hu);
    assert_eq!(captions[2].1, "március 12.");
    assert_eq!(captions[2].0, "márc. 12.");
    assert_eq!(captions[11].1, "december 12.");
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
    assert_eq!(msg::watchlist_state_coming_on_in(&fr, &date), "Sortie le 12 mars");
    let en = LocaleContext::resolve(Preference::En, None, None, None, None);
    assert_eq!(msg::watchlist_page_sort_filter_in(&en), "Sort & Filter");
    for preference in SHIPPED {
        let cx = LocaleContext::resolve(preference, None, None, None, None);
        assert!(!msg::watchlist_page_title_in(&cx).is_empty());
        assert!(!msg::watchlist_empty_list_in(&cx).is_empty());
        assert!(!msg::watchlist_empty_filtered_in(&cx).is_empty());
    }
}

/// Every Watchlist key the English catalog declares is present, non-empty and carries a
/// translator description, in every shipped catalog. The compiler already refuses a key
/// set that differs between languages; this also pins the shape of the files themselves.
#[test]
fn every_watchlist_key_exists_in_every_shipped_catalog() {
    // English first (the source), then every other shipped language, read by registry tag.
    let read = |tag: &str| -> (String, String) {
        let path = format!("{}/../../locales/{tag}/watchlist.json", env!("CARGO_MANIFEST_DIR"));
        (tag.to_string(), std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{path}: {e}")))
    };
    let mut catalogs = vec![read("en")];
    catalogs.extend(Language::SHIPPED.iter().map(|l| read(l.tag())).filter(|(t, _)| t != "en"));
    let parse = |text: &str| -> serde_json::Map<String, serde_json::Value> {
        serde_json::from_str(text).expect("valid watchlist catalog")
    };
    let english = parse(&catalogs[0].1);
    assert!(english.len() >= 20, "the Watchlist catalog shrank to {}", english.len());
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

#[test]
fn declared_plural_categories_are_the_languages_own_cldr_categories() {
    for lang in Language::SHIPPED {
        let rules = PluralRules::try_new_cardinal(lang.tag().parse::<Locale>().unwrap().into())
            .expect("compiled plural data");
        let mut icu: Vec<String> = rules.categories().map(|c| format!("{c:?}").to_lowercase()).collect();
        icu.sort();
        let (_, declared) = Language::PLURALS.iter().find(|(l, _)| *l == lang).expect("declared");
        let mut declared: Vec<String> = declared.iter().map(|s| s.to_string()).collect();
        declared.sort();
        assert_eq!(declared, icu, "{}: languages.json plurals differ from CLDR", lang.tag());
    }
}

/// Baked ICU data falls back to root for a locale it lacks and still "succeeds", so a constructor
/// that returns Ok proves nothing: ask the data provider which locale actually answered.
#[test]
fn every_shipped_language_has_its_own_icu_data_not_root() {
    use icu_provider::prelude::*;
    fn answered_by<M: DataMarker>(
        provider: &impl DataProvider<M>,
        locale: &Locale,
    ) -> Option<DataLocale> {
        let data_locale = DataLocale::from(locale.clone());
        let id = DataIdentifierBorrowed::for_locale(&data_locale);
        let response = provider
            .load(DataRequest { id, ..Default::default() })
            .unwrap_or_else(|e| panic!("{locale}: {e}"));
        response.metadata.locale
    }
    // English is CLDR's root: its data is stored once, under und, so it answers for "en" legitimately.
    for lang in Language::SHIPPED.into_iter().filter(|l| *l != Language::En) {
        for tag in [lang.tag(), lang.regional()] {
            let locale: Locale = tag.parse().unwrap();
            for (what, got) in [
                ("decimal symbols", answered_by::<icu_decimal::provider::DecimalSymbolsV1>(&icu_decimal::provider::Baked, &locale)),
                ("plurals", answered_by::<icu_plurals::provider::PluralsCardinalV1>(&icu_plurals::provider::Baked, &locale)),
            ] {
                if let Some(got) = got {
                    assert_eq!(
                        got.language.as_str(),
                        lang.tag(),
                        "{tag}: {what} data was answered by {got}, not the language's own"
                    );
                }
            }
        }
    }
}

#[test]
fn registered_languages_keep_their_persisted_identity() {
    // A subset, never an equality: a release that shipped a language pins its (tag, row) here.
    const HISTORICAL: &[(&str, u32)] = &[("en", 1), ("es", 2), ("be", 3), ("fr", 5)];
    for (tag, row) in HISTORICAL {
        let p = Preference::from_tag(tag);
        assert!(SHIPPED.contains(&p), "{tag} is no longer shipped");
        assert_eq!(p.tag(), *tag);
        assert_eq!(p.settings_row(), *row, "{tag}: Settings row identity moved");
        assert_eq!(serde_json::to_string(&p).unwrap(), format!("\"{tag}\""));
    }
    for p in SHIPPED {
        assert_eq!(Preference::from_tag(p.tag()), p);
        assert_eq!(serde_json::to_string(&p).unwrap(), format!("\"{}\"", p.tag()));
        assert_eq!(serde_json::from_str::<Preference>(&format!("\"{}\"", p.tag())).unwrap(), p);
        assert_eq!(Preference::from_slot(p.slot()), p);
        assert_eq!(p.language().map(Language::tag), Some(p.tag()));
        assert!((1..1000).contains(&p.settings_row()));
    }
    assert_eq!(serde_json::to_string(&Preference::System).unwrap(), "\"system\"");
    assert_eq!(Preference::from_tag("xx"), Preference::System);
    assert_eq!(Preference::from_slot(Preference::System.slot()), Preference::System);
}

#[test]
fn every_registered_language_has_launcher_metadata() {
    let resources = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../pkg/resources");
    for p in SHIPPED.iter().filter(|p| **p != Preference::En) {
        assert!(
            resources.join(p.tag()).join("appinfo.json").is_file(),
            "pkg/resources/{}/appinfo.json: the launcher's name for this language (docs/localization.md#adding-a-language)",
            p.tag()
        );
    }
}

#[test]
fn english_only_plural_uses_english_rules() {
    let _be = language_on_this_thread_for_test(Preference::Be);
    // Belarusian plural(21) is One; the panel's English-only tally must still say "callbacks".
    assert_eq!(msg::browse_diagnostics_callbacks(21), "21 callbacks");
    assert_eq!(msg::browse_diagnostics_callbacks(1), "1 callback");
}

#[test]
fn english_only_ignores_pseudo() {
    let _pseudo = pseudo_on_this_thread_for_test();
    assert_eq!(msg::browse_diagnostics_nothing_demuxed(), "NOTHING demuxed");
    assert_eq!(msg::browse_diagnostics_nothing_demuxed_c().to_str(), Ok("NOTHING demuxed"));
}

#[test]
fn render_in_english_is_scoped_and_nests() {
    let _be = language_on_this_thread_for_test(Preference::Be);
    assert_eq!(current().language(), Language::Be);
    let inside = render_in_english(|| {
        assert_eq!(current().language(), Language::En);
        render_in_english(|| assert_eq!(current().language(), Language::En));
        assert_eq!(current().language(), Language::En);
        current().decimal(35, 1)
    });
    assert_eq!(inside, "3.5");
    assert_eq!(current().language(), Language::Be);
}
