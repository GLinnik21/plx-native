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
    for tag in ["sv-SE", "be-Latn", "fr-Cyrl", "it-Cyrl", "de-Cyrl", "hu-Cyrl", "garbage\r\nheader", "C"] {
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

#[test]
fn polish_month_day_captions_use_genitive_and_abbreviations() {
    let cx = LocaleContext::resolve(Preference::System, Some("pl-PL"), None, None, None);
    let expected = [
        ("12 sty", "12 stycznia"), ("12 lut", "12 lutego"), ("12 mar", "12 marca"),
        ("12 kwi", "12 kwietnia"), ("12 maj", "12 maja"), ("12 cze", "12 czerwca"),
        ("12 lip", "12 lipca"), ("12 sie", "12 sierpnia"), ("12 wrz", "12 września"),
        ("12 paź", "12 października"), ("12 lis", "12 listopada"), ("12 gru", "12 grudnia"),
    ];
    for (i, (medium, long)) in expected.iter().enumerate() {
        let month = (i + 1) as u8;
        assert_eq!(cx.month_day(month, 12, MonthWidth::Medium).as_deref(), Some(*medium));
        assert_eq!(cx.month_day(month, 12, MonthWidth::Long).as_deref(), Some(*long));
    }
    assert_eq!(cx.month_day(2, 29, MonthWidth::Long).as_deref(), Some("29 lutego"));
    assert!(cx.month_day(2, 30, MonthWidth::Long).is_none());
}

#[test]
fn polish_catalog_counts_follow_polish_plurals_even_with_english_formatting() {
    let pl = LocaleContext::resolve(Preference::Pl, None, Some("en-US"), None, None);
    for (count, expected, category) in [
        (0, "0 odcinków", PluralCategory::Many),
        (1, "1 odcinek", PluralCategory::One),
        (2, "2 odcinki", PluralCategory::Few),
        (5, "5 odcinków", PluralCategory::Many),
        (11, "11 odcinków", PluralCategory::Many),
        (12, "12 odcinków", PluralCategory::Many),
        (14, "14 odcinków", PluralCategory::Many),
        (21, "21 odcinków", PluralCategory::Many),
        (22, "22 odcinki", PluralCategory::Few),
        (25, "25 odcinków", PluralCategory::Many),
        (101, "101 odcinków", PluralCategory::Many),
        (112, "112 odcinków", PluralCategory::Many),
        (122, "122 odcinki", PluralCategory::Few),
    ] {
        assert_eq!(pl.plural(count), category);
        assert_eq!(msg::browse_detail_episodes_in(&pl, count), expected);
    }
    let mut fraction = Decimal::from(15);
    fraction.multiply_pow10(-1);
    assert_eq!(pl.plural.category_for(&fraction), PluralCategory::Other);
}

#[test]
fn polish_system_locale_resolves_with_regional_formatting_and_latin_script() {
    for tag in ["pl", "pl-PL", "pl_PL.UTF-8"] {
        let cx = LocaleContext::resolve(Preference::System, Some(tag), None, None, None);
        assert_eq!(cx.language(), Language::Pl, "{tag}");
        assert_eq!(cx.preference(), Preference::System);
    }
    let pl = LocaleContext::resolve(Preference::Pl, None, None, None, None);
    assert_eq!(pl.format_locale(), "pl-PL");
    assert_eq!(pl.decimal(45, 1), "4,5");
    assert_eq!(pl.date(2026, 10, 25).as_deref(), Some("25.10.2026"));
    assert_eq!(LocaleContext::resolve(Preference::System, Some("pl-Cyrl"), None, None, None).language(), Language::En);
}

#[test]
fn sort_timestamp_calendar_conversion_rejects_unknown_and_unrepresentable_values() {
    assert_eq!(super::local_calendar_date(0), None);
    assert_eq!(super::local_calendar_date(-1), None);
    assert_eq!(super::local_calendar_date(i64::MAX), None);
    let [year, month, day] = super::local_calendar_date(1700000000).unwrap();
    assert_eq!((year, month), (2023, 11));
    assert!((14..=15).contains(&day), "calendar date follows the local timezone");
}
#[test]
fn named_month_dates_keep_regional_order_and_validate_calendar_days() {
    let ru = LocaleContext::resolve(Preference::Ru, None, None, None, None);
    let en = LocaleContext::resolve(Preference::En, None, None, None, None);
    let gb = LocaleContext::resolve(Preference::En, None, Some("en-GB"), None, None);
    let russian = ru.date_medium(2026, 6, 25).unwrap();
    assert!(russian.starts_with("25 июн.") && russian.contains("2026"), "{russian}");
    assert_eq!(en.date_medium(2026, 6, 25).as_deref(), Some("Jun 25, 2026"));
    assert_eq!(gb.date_medium(2026, 6, 25).as_deref(), Some("25 Jun 2026"));
    assert!(ru.date_medium(2026, 2, 29).is_none());
    assert!(ru.date_medium(2024, 2, 29).is_some());
}

#[test]
fn named_month_dates_follow_interface_language_when_tv_format_language_differs() {
    for format in ["en-GB", "en-US"] {
        for preference in [Preference::Ru, Preference::System] {
            let cx = LocaleContext::resolve(preference, Some("ru-RU"), Some(format), None, None);
            assert_eq!(cx.format_locale(), format, "numeric formats retain the TV preference");
            let date = cx.date_medium(2026, 6, 25).unwrap();
            assert!(date.starts_with("25 июн.") && !date.contains("Jun"), "{format}: {date}");
        }
    }
    let english = LocaleContext::resolve(Preference::En, Some("ru-RU"), Some("ru-RU"), None, None);
    assert_eq!(english.date_medium(2026, 6, 25).as_deref(), Some("Jun 25, 2026"));
}

#[test]
fn named_month_dates_match_icu_for_shipped_languages_and_regional_variants() {
    // Compare all months against ICU's full data, including region-specific abbreviations and
    // fallback regions. Only the test binary links the full Gregorian month-name database.
    for preference in SHIPPED {
        let regions = (b'A'..=b'Z')
            .flat_map(|a| (b'A'..=b'Z').map(move |b| format!("{}{}", a as char, b as char)))
            .chain(["", "001", "150", "419"].map(str::to_owned));
        for region in regions {
            let locale = if region.is_empty() {
                preference.tag().to_owned()
            } else {
                format!("{}-{region}", preference.tag())
            };
            let cx = LocaleContext::resolve(preference, None, Some(&locale), None, None);
            let oracle = icu_datetime::FixedCalendarDateTimeFormatter::try_new(
                locale.parse::<icu_locale::Locale>().unwrap().into(),
                icu_datetime::fieldsets::YMD::medium()
                    .with_year_style(icu_datetime::options::YearStyle::Full),
            )
            .unwrap();
            for month in 1..=12 {
                let date = icu_calendar::Date::try_new_gregorian(2026, month, 9).unwrap();
                assert_eq!(
                    cx.date_medium(2026, month, 9).unwrap(),
                    oracle.format(&date).to_string(),
                    "{locale}, month {month}"
                );
            }
        }
    }
}

#[test]
fn numeric_dates_match_icu_without_month_name_data() {
    // Short date patterns must stay numeric, even when the TV format language differs from the UI.
    let languages = (b'a'..=b'z')
        .flat_map(|a| (b'a'..=b'z').map(move |b| format!("{}{}", a as char, b as char)))
        .chain((b'a'..=b'z').flat_map(|a| {
            (b'a'..=b'z').flat_map(move |b| {
                (b'a'..=b'z').map(move |c| format!("{}{}{}", a as char, b as char, c as char))
            })
        }));
    for locale in languages {
        let Some(locale) = normalize(&locale) else {
            continue;
        };
        let cx = LocaleContext::resolve(Preference::En, None, Some(&locale), None, None);
        let oracle = icu_datetime::FixedCalendarDateTimeFormatter::try_new(
            locale.parse::<icu_locale::Locale>().unwrap().into(),
            icu_datetime::fieldsets::YMD::short()
                .with_year_style(icu_datetime::options::YearStyle::Full),
        )
        .unwrap();
        for (month, day) in [(1, 9), (6, 25), (9, 1), (12, 31)] {
            let date = icu_calendar::Date::try_new_gregorian(2026, month, day).unwrap();
            assert_eq!(
                cx.date(2026, month, day).unwrap(),
                oracle.format(&date).to_string(),
                "{locale}"
            );
        }
    }
}

#[test]
fn month_year_is_the_ymmm_skeleton_in_every_shipped_language_and_never_numeric() {
    for preference in SHIPPED {
        let cx = LocaleContext::resolve(preference, None, None, None, None);
        let tag = cx.language().tag();
        assert!(cx.month_year(2024, 13).is_none() && cx.month_year(2024, 0).is_none(), "{tag}");
        let mut seen: Vec<String> = Vec::new();
        for month in 1..=12 {
            let text = cx.month_year(2024, month).expect("a real month");
            // The year is the only number in it: the month is a name, in the language's own letters.
            assert!(text.contains("2024"), "{tag} {month}: {text}");
            let name = text.replacen("2024", "", 1);
            assert!(!name.chars().any(|c| c.is_ascii_digit() || c == '/'), "{tag} {month}: {text}");
            assert!(name.chars().any(char::is_alphabetic), "{tag} {month}: {text}");
            assert!(!seen.contains(&text), "{tag} {month}: {text} repeats a month");
            seen.push(text);
        }
    }
}

#[test]
fn month_short_is_the_month_year_names_month_in_every_shipped_language() {
    for preference in SHIPPED {
        let cx = LocaleContext::resolve(preference, None, None, None, None);
        let tag = cx.language().tag();
        assert!(cx.month_short(0).is_none() && cx.month_short(13).is_none() && cx.month_short(u8::MAX).is_none(), "{tag}");
        let mut seen: Vec<String> = Vec::new();
        for month in 1..=12 {
            let name = cx.month_short(month).expect("a real month");
            assert!(!name.is_empty() && name.chars().all(|c| !c.is_ascii_digit() && c != '/'), "{tag} {month}: {name:?}");
            // The one selection: the month `month_year` writes before the year is this name.
            let year_text = cx.month_year(2024, month).expect("a real month");
            assert!(year_text.contains(&name), "{tag} {month}: {name:?} is not the month of {year_text:?}");
            assert!(!seen.contains(&name), "{tag} {month}: {name:?} repeats a month");
            seen.push(name);
        }
    }
}

#[test]
fn month_short_reads_as_the_boards_do_and_is_stand_alone_in_russian_and_belarusian() {
    for preference in SHIPPED {
        let cx = LocaleContext::resolve(preference, None, None, None, None);
        match cx.language() {
            Language::En => assert_eq!(cx.month_short(10).as_deref(), Some("Oct")),
            Language::Ru => {
                assert_eq!(cx.month_short(10).as_deref(), Some("окт."));
                assert_eq!(cx.month_short(5).as_deref(), Some("май"), "stand-alone, not the genitive \"мая\"");
            }
            Language::Be => {
                assert_eq!(cx.month_short(4).as_deref(), Some("кра"));
                assert_eq!(cx.month_short(5).as_deref(), Some("май"));
            }
            _ => {}
        }
    }
}

/// CLDR 47 (the data `icu_datetime` 2.1 is built from) `ru` stand-alone abbreviated months, what its
/// yMMM skeleton (`LLL y 'г'.`) spells: "март", "май", "июнь" and "июль" differ from the format
/// forms "мар.", "мая", "июн." and "июл." that follow a day.
const MONTHS_RU_STANDALONE: [&str; 12] =
    ["янв.", "февр.", "март", "апр.", "май", "июнь", "июль", "авг.", "сент.", "окт.", "нояб.", "дек."];
/// CLDR 47 `be` stand-alone abbreviated months (yMMM is `LLL y`): only May differs from the format
/// form ("май", where "мая" follows a day). "май" is the nominative of skarnik.by and verbum.by.
const MONTHS_BE_STANDALONE: [&str; 12] =
    ["сту", "лют", "сак", "кра", "май", "чэр", "ліп", "жні", "вер", "кас", "ліс", "сне"];

#[test]
fn month_year_names_the_month_stand_alone_where_the_language_declines_it() {
    for preference in SHIPPED {
        let cx = LocaleContext::resolve(preference, None, None, None, None);
        let standalone = match cx.language() {
            Language::Ru => &MONTHS_RU_STANDALONE,
            Language::Be => &MONTHS_BE_STANDALONE,
            // Every other shipped language writes the same abbreviated month after a day and before a
            // year (CLDR yMMM is `MMM y` or `LLL y` over identical format and stand-alone names).
            _ => continue,
        };
        let tag = cx.language().tag();
        for (i, name) in standalone.iter().enumerate() {
            let month = u8::try_from(i + 1).unwrap();
            let expected = format!("{name} 2024\u{a0}г.");
            assert_eq!(cx.month_year(2024, month).as_deref(), Some(expected.as_str()), "{tag} {month}");
        }
        // May is the month whose two forms differ in both languages: the nominative before a year,
        // the genitive "мая" after a day, and the caption after a day is not touched.
        assert_eq!(cx.month_year(2024, 5).as_deref(), Some("май 2024\u{a0}г."), "{tag}");
        assert_eq!(cx.month_day(5, 9, MonthWidth::Medium).as_deref(), Some("9 мая"), "{tag}");
    }
}

#[test]
fn today_is_a_calendar_date_from_the_wall_clock() {
    let [year, month, day] = local_calendar_today().expect("a representable date");
    assert!(year >= 2026 && (1..=12).contains(&month) && (1..=31).contains(&day));
}

#[test]
fn dutch_month_day_captions_cover_every_month_at_both_widths() {
    const MEDIUM: [&str; 12] =
        ["jan", "feb", "mrt", "apr", "mei", "jun", "jul", "aug", "sep", "okt", "nov", "dec"];
    const LONG: [&str; 12] = [
        "januari", "februari", "maart", "april", "mei", "juni", "juli", "augustus", "september",
        "oktober", "november", "december",
    ];
    let captions = month_captions(Preference::Nl);
    for (i, (medium, long)) in captions.iter().enumerate() {
        assert_eq!(medium, &format!("12 {}", MEDIUM[i]));
        assert_eq!(long, &format!("12 {}", LONG[i]));
    }
    assert_eq!(captions[2], ("12 mrt".to_string(), "12 maart".to_string()));
    let cx = LocaleContext::resolve(Preference::Nl, None, None, None, None);
    assert_eq!(cx.month_day(2, 29, MonthWidth::Long).as_deref(), Some("29 februari"));
    assert!(cx.month_day(2, 30, MonthWidth::Long).is_none());
}

#[test]
fn dutch_catalog_counts_follow_dutch_plurals() {
    let nl = LocaleContext::resolve(Preference::Nl, None, None, None, None);
    for (count, expected, category) in [
        (0, "0 afleveringen", PluralCategory::Other),
        (1, "1 aflevering", PluralCategory::One),
        (2, "2 afleveringen", PluralCategory::Other),
        (21, "21 afleveringen", PluralCategory::Other),
    ] {
        assert_eq!(nl.plural(count), category);
        assert_eq!(msg::browse_detail_episodes_in(&nl, count), expected);
    }
}

#[test]
fn dutch_system_locale_resolves_with_regional_formatting() {
    for tag in ["nl", "nl-NL", "nl_NL.UTF-8", "nl-BE"] {
        let cx = LocaleContext::resolve(Preference::System, Some(tag), None, None, None);
        assert_eq!(cx.language(), Language::Nl, "{tag}");
        assert_eq!(cx.preference(), Preference::System);
    }
    let nl = LocaleContext::resolve(Preference::Nl, None, None, None, None);
    assert_eq!(nl.format_locale(), "nl-NL");
    assert_eq!(nl.decimal(45, 1), "4,5");
}

#[test]
fn a_local_midnight_is_refused_for_a_date_that_is_not_on_the_calendar() {
    for date in [[2026, 0, 1], [2026, 13, 1], [2026, 1, 0], [2026, 1, 32], [2026, 2, 29], [2026, 4, 31], [2026, -1, 5],
        [2026, 6, -3], [i32::MIN, 1, 1]] {
        assert_eq!(local_midnight(date), None, "{date:?}");
    }
    assert!(local_midnight([2024, 2, 29]).is_some(), "a leap day exists");
}

/// The first moment of a date in the zone under test, read in a child process: libc's zone state is
/// process-wide, so each zone gets a fresh process (the Library's sort-date test does the same) and the
/// other tests of this crate never see `TZ` change. The child is this very test with `TZ` and the
/// expected stamps in its environment; the parent runs it once per zone and checks that it passed.
#[test]
fn local_midnight_follows_the_zone_and_the_clock_change_of_each_date() {
    // Literals from an independent calendar (Python's `calendar.timegm`), shifted by hand by the zone's
    // offset on that date: never from this code. Each case is `TZ`, then date and stamp pairs.
    type Case = (&'static str, &'static [([i32; 3], i64)]);
    const CASES: [Case; 5] = [
        ("UTC0", &[([2026, 3, 8], 1_772_928_000), ([2026, 3, 9], 1_773_014_400)]),
        ("JST-9", &[([2026, 3, 8], 1_772_895_600), ([2026, 3, 9], 1_772_982_000)]),
        // The US clock change of 2026: forward on 8 March (the 8th starts at -5, the 9th at -4, so the
        // two midnights are 23 hours apart) and back on 1 November (-4 on the 1st, -5 on the 2nd: 25).
        ("EST5EDT,M3.2.0,M11.1.0", &[
            ([2026, 3, 8], 1_772_946_000), ([2026, 3, 9], 1_773_028_800),
            ([2026, 11, 1], 1_793_505_600), ([2026, 11, 2], 1_793_595_600),
        ]),
        // Zones whose clocks change AT MIDNIGHT, where `mktime(00:00, tm_isdst = -1)` has no single right
        // answer. Chile-shaped (UTC-4, DST -3, both changes at 24:00 on a Saturday): Sunday 6 September
        // has no 00:00 (04:00Z, std, is 01:00 DST) so its first instant is that 04:00Z, and the Saturday
        // before it begins at its own 00:00 (04:00Z, unshifted); 23:00 of Saturday 4 April happens twice
        // and Sunday 5 April begins at 04:00Z once, the Saturday at 00:00 DST (03:00Z).
        ("<-04>4<-03>,M9.1.6/24,M4.1.6/24", &[
            ([2026, 9, 5], 1_788_580_800), ([2026, 9, 6], 1_788_667_200),
            ([2026, 4, 4], 1_775_271_600), ([2026, 4, 5], 1_775_361_600),
        ]),
        // US Central with the fall-back at 01:00 DST: 00:00-01:00 of 1 November happens twice, 05:00Z
        // (CDT) and 06:00Z (CST); the day begins at the first.
        ("CST6CDT,M3.2.0,M11.1.0/1", &[([2026, 11, 1], 1_793_509_200), ([2026, 11, 2], 1_793_599_200)]),
    ];
    const CHILD: &str = "PLX_LOCAL_MIDNIGHT_CASE";
    if let Ok(case) = std::env::var(CHILD) {
        let (_, expected) = CASES[case.parse::<usize>().unwrap()];
        for &(date, stamp) in expected {
            assert_eq!(local_midnight(date), Some(stamp), "{date:?}");
            // …and it is the first moment of that date, to the second, in the zone the OS reads
            assert_eq!(local_calendar_date(stamp), Some(date), "{date:?} at its midnight");
            assert_eq!(local_calendar_date(stamp - 1).map(|day| day != date), Some(true), "{date:?} a second before");
        }
        return;
    }
    // libtest names a test without its crate, and exits 0 when the name matches nothing: the child must
    // say it ran one test and passed it.
    let name = format!(
        "{}::local_midnight_follows_the_zone_and_the_clock_change_of_each_date",
        module_path!().split_once("::").unwrap().1
    );
    for (index, (zone, _)) in CASES.iter().enumerate() {
        let out = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", &name, "--test-threads=1"])
            .env("TZ", zone).env(CHILD, index.to_string()).output().unwrap();
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(out.status.success() && stdout.contains("1 passed"),
            "local midnight failed (or ran nothing) under {zone}:\n{stdout}\n{}", String::from_utf8_lossy(&out.stderr));
    }
}
