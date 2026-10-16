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
    for tag in ["de-DE", "be-Latn", "fr-Cyrl", "it-Cyrl", "garbage\r\nheader", "C"] {
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
    let it = LocaleContext::resolve(Preference::It, None, None, None, None);
    assert_eq!(it.plural(0), PluralCategory::Other);
    assert_eq!(it.plural(1), PluralCategory::One);
    assert_eq!(it.plural(2), PluralCategory::Other);
    assert_eq!(it.plural(1_000_000), PluralCategory::Many);
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
