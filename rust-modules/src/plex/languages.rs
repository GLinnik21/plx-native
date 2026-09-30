//! Plex Web's account language picker, bundled for offline Settings navigation.
//!
//! Data source: Plex Web 4.160.0, main bundle module 12060 (native display names).
//! https://app.plex.tv/desktop/js/main-8792-79c4e04db2360a62e838-plex-4.160.0-75ddd7b.js
//! Membership: account form in chunk 4030 includes two- or five-character codes, excluding
//! `xx` (Unknown) and `xn` (None). Its broader catalog's `es-419` is therefore not offered.
//! https://app.plex.tv/desktop/js/chunk-4030-108232e3535fafc4f3ac-plex-4.160.0-75ddd7b.js
//! Names and regional codes are data, not translated application strings. This array preserves
//! the native-name `localeCompare` ordering captured with the source; no runtime fetch is needed.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Language {
    pub code: &'static str,
    pub name: &'static str,
}

pub(crate) const LANGUAGES: &[Language] = &[
    Language { code: "ab", name: "Abkhazian" },
    Language { code: "om", name: "Afaan Oromoo" },
    Language { code: "aa", name: "Afaraf" },
    Language { code: "af", name: "Afrikaans" },
    Language { code: "ak", name: "Akan" },
    Language { code: "am", name: "Amharic" },
    Language { code: "an", name: "Aragonés" },
    Language { code: "as", name: "Assamese" },
    Language { code: "ig", name: "Asụsụ Igbo" },
    Language { code: "gn", name: "Avañeʼẽ" },
    Language { code: "ae", name: "Avesta" },
    Language { code: "ay", name: "Aymar aru" },
    Language { code: "az", name: "Azərbaycan dili" },
    Language { code: "id", name: "Bahasa Indonesia" },
    Language { code: "ms", name: "Bahasa Melayu" },
    Language { code: "bm", name: "Bamanankan" },
    Language { code: "jv", name: "Basa Jawa" },
    Language { code: "su", name: "Basa Sunda" },
    Language { code: "bn", name: "Bengali" },
    Language { code: "bi", name: "Bislama" },
    Language { code: "bs", name: "Bosanski jezik" },
    Language { code: "br", name: "Brezhoneg" },
    Language { code: "my", name: "Burmese" },
    Language { code: "ca", name: "Català" },
    Language { code: "cs", name: "Čeština" },
    Language { code: "ch", name: "Chamoru" },
    Language { code: "ny", name: "ChiCheŵa" },
    Language { code: "sn", name: "ChiShona" },
    Language { code: "co", name: "Corsu" },
    Language { code: "cy", name: "Cymraeg" },
    Language { code: "da", name: "Dansk" },
    Language { code: "se", name: "Davvisámegiella" },
    Language { code: "de", name: "Deutsch" },
    Language { code: "nv", name: "Diné bizaad" },
    Language { code: "dv", name: "Divehi" },
    Language { code: "et", name: "Eesti" },
    Language { code: "na", name: "Ekakairũ Naoero" },
    Language { code: "en", name: "English" },
    Language { code: "en-GB", name: "English (UK)" },
    Language { code: "es", name: "Español" },
    Language { code: "eo", name: "Esperanto" },
    Language { code: "eu", name: "Euskara" },
    Language { code: "ee", name: "Eʋegbe" },
    Language { code: "to", name: "Faka Tonga" },
    Language { code: "fo", name: "Føroyskt" },
    Language { code: "fr", name: "Français" },
    Language { code: "fr-CA", name: "Français Canadien" },
    Language { code: "fy", name: "Frysk" },
    Language { code: "ff", name: "Fulfulde" },
    Language { code: "ga", name: "Gaeilge" },
    Language { code: "gv", name: "Gaelg" },
    Language { code: "sm", name: "Gagana faʻa Samoa" },
    Language { code: "gd", name: "Gàidhlig" },
    Language { code: "gl", name: "Galego" },
    Language { code: "ki", name: "Gĩkũyũ" },
    Language { code: "ho", name: "Hiri Motu" },
    Language { code: "hr", name: "Hrvatski" },
    Language { code: "io", name: "Ido" },
    Language { code: "rw", name: "Ikinyarwanda" },
    Language { code: "ia", name: "Interlingua" },
    Language { code: "ie", name: "Interlingue" },
    Language { code: "ik", name: "Iñupiaq" },
    Language { code: "nr", name: "IsiNdebele (North)" },
    Language { code: "nd", name: "IsiNdebele (South)" },
    Language { code: "xh", name: "IsiXhosa" },
    Language { code: "zu", name: "IsiZulu" },
    Language { code: "is", name: "Íslenska" },
    Language { code: "it", name: "Italiano" },
    Language { code: "mh", name: "Kajin M̧ajeļ" },
    Language { code: "kl", name: "Kalaallisut" },
    Language { code: "kr", name: "Kanuri" },
    Language { code: "kw", name: "Kernewek" },
    Language { code: "km", name: "Khmer" },
    Language { code: "kg", name: "KiKongo" },
    Language { code: "rn", name: "KiRundi" },
    Language { code: "sw", name: "Kiswahili" },
    Language { code: "ht", name: "Kreyòl ayisyen" },
    Language { code: "kj", name: "Kuanyama" },
    Language { code: "ku", name: "Kurdî" },
    Language { code: "lo", name: "Lao" },
    Language { code: "la", name: "Latine" },
    Language { code: "lv", name: "Latviešu valoda" },
    Language { code: "lb", name: "Lëtzebuergesch" },
    Language { code: "lt", name: "Lietuvių kalba" },
    Language { code: "li", name: "Limburgs" },
    Language { code: "ln", name: "Lingála" },
    Language { code: "lu", name: "Luba-Katanga" },
    Language { code: "lg", name: "Luganda" },
    Language { code: "hu", name: "Magyar" },
    Language { code: "mg", name: "Malagasy fiteny" },
    Language { code: "ml", name: "Malayalam" },
    Language { code: "mt", name: "Malti" },
    Language { code: "mo", name: "Moldavian" },
    Language { code: "nl", name: "Nederlands" },
    Language { code: "no", name: "Norsk" },
    Language { code: "nb", name: "Norsk bokmål" },
    Language { code: "nn", name: "Norsk nynorsk" },
    Language { code: "oc", name: "Occitan" },
    Language { code: "uz", name: "Oʻzbek" },
    Language { code: "oj", name: "Ojibwe" },
    Language { code: "or", name: "Oriya" },
    Language { code: "hz", name: "Otjiherero" },
    Language { code: "ng", name: "Owambo" },
    Language { code: "pl", name: "Polski" },
    Language { code: "pt", name: "Português" },
    Language { code: "pb", name: "Português (Brasil) [deprecated]" },
    Language { code: "pt-BR", name: "Português Brasileiro" },
    Language { code: "ty", name: "Reo Tahiti" },
    Language { code: "ro", name: "Română" },
    Language { code: "rm", name: "Rumantsch grischun" },
    Language { code: "qu", name: "Runa Simi" },
    Language { code: "sc", name: "Sardu" },
    Language { code: "za", name: "Saɯ cueŋƅ" },
    Language { code: "st", name: "Sesotho" },
    Language { code: "tn", name: "Setswana" },
    Language { code: "sq", name: "Shqip" },
    Language { code: "si", name: "Sinhala" },
    Language { code: "ss", name: "SiSwati" },
    Language { code: "sk", name: "Slovenčina" },
    Language { code: "sl", name: "Slovenščina" },
    Language { code: "so", name: "Soomaaliga" },
    Language { code: "fi", name: "Suomeksi" },
    Language { code: "sv", name: "Svenska" },
    Language { code: "mi", name: "Te reo Māori" },
    Language { code: "te", name: "Telugu" },
    Language { code: "vi", name: "Tiếng Việt" },
    Language { code: "ti", name: "Tigrinya" },
    Language { code: "ve", name: "Tshivenḓa" },
    Language { code: "tr", name: "Türkçe" },
    Language { code: "tk", name: "Türkmen" },
    Language { code: "tw", name: "Twi" },
    Language { code: "ug", name: "Uyƣurqə" },
    Language { code: "vo", name: "Volapük" },
    Language { code: "fj", name: "Vosa Vakaviti" },
    Language { code: "wa", name: "Walon" },
    Language { code: "tl", name: "Wikang Tagalog" },
    Language { code: "wo", name: "Wollof" },
    Language { code: "ts", name: "Xitsonga" },
    Language { code: "sg", name: "Yângâ tî sängö" },
    Language { code: "yo", name: "Yorùbá" },
    Language { code: "el", name: "Ελληνικά" },
    Language { code: "av", name: "авар мацӀ" },
    Language { code: "ba", name: "башҡорт теле" },
    Language { code: "be", name: "Беларуская" },
    Language { code: "bg", name: "български език" },
    Language { code: "os", name: "ирон æвзаг" },
    Language { code: "kv", name: "коми кыв" },
    Language { code: "ky", name: "кыргыз тили" },
    Language { code: "kk", name: "Қазақ тілі" },
    Language { code: "mk", name: "македонски јазик" },
    Language { code: "mn", name: "монгол" },
    Language { code: "ce", name: "нохчийн мотт" },
    Language { code: "ru", name: "русский язык" },
    Language { code: "sr", name: "српски језик" },
    Language { code: "tt", name: "татарча" },
    Language { code: "tg", name: "тоҷикӣ" },
    Language { code: "uk", name: "українська" },
    Language { code: "cv", name: "чӑваш чӗлхи" },
    Language { code: "cu", name: "ѩзыкъ словѣньскъ" },
    Language { code: "ka", name: "ქართული" },
    Language { code: "hy", name: "Հայերեն" },
    Language { code: "yi", name: "ייִדיש" },
    Language { code: "he", name: "עברית" },
    Language { code: "ur", name: "اردو" },
    Language { code: "ar", name: "العربية" },
    Language { code: "ps", name: "پښتو" },
    Language { code: "fa", name: "فارسی" },
    Language { code: "ha", name: "هَوُسَ" },
    Language { code: "ks", name: "कश्मीरी" },
    Language { code: "ne", name: "नेपाली" },
    Language { code: "pi", name: "पाऴि" },
    Language { code: "bh", name: "भोजपुरी" },
    Language { code: "mr", name: "मराठी" },
    Language { code: "sa", name: "संस्कृतम्" },
    Language { code: "sd", name: "सिन्धी" },
    Language { code: "hi", name: "हिन्दी" },
    Language { code: "pa", name: "ਪੰਜਾਬੀ" },
    Language { code: "gu", name: "ગુજરાતી" },
    Language { code: "ta", name: "தமிழ்" },
    Language { code: "kn", name: "ಕನ್ನಡ" },
    Language { code: "th", name: "ไทย" },
    Language { code: "bo", name: "བོད་ཡིག" },
    Language { code: "dz", name: "རྫོང་ཁ" },
    Language { code: "iu", name: "ᐃᓄᒃᑎᑐᑦ" },
    Language { code: "cr", name: "ᓀᐦᐃᔭᐍᐏᐣ" },
    Language { code: "ko", name: "한국어" },
    Language { code: "ii", name: "ꆈꌠ꒿ Nuosuhxop" },
    Language { code: "zh", name: "中文" },
    Language { code: "ja", name: "日本語" },
    Language { code: "zh-TW", name: "臺語" },
];

/// Codes Plex Web's own catalog still lists but marks `[deprecated]`, each with the entry that
/// replaced it. An old account can still carry one, so it stays in [`LANGUAGES`] (the recorded
/// membership); it is just never offered ([`picker`]) or shown under its deprecated name
/// ([`label`] reads the replacement's).
const DEPRECATED: &[(&str, &str)] = &[("pb", "pt-BR")];

/// `code`, or the code that replaced it when Plex has deprecated it. Unfamiliar codes pass through.
pub(crate) fn canonical(code: &str) -> &str {
    DEPRECATED.iter().find(|(old, _)| *old == code).map_or(code, |(_, new)| new)
}

/// The catalog a picker offers: every entry except the deprecated ones.
pub(crate) fn picker() -> impl Iterator<Item = &'static Language> {
    LANGUAGES.iter().filter(|language| canonical(language.code) == language.code)
}

/// Preserve unfamiliar account values verbatim, including region codes from newer clients. A
/// deprecated code reads as its replacement's name, never with the catalog's `[deprecated]` suffix.
pub(crate) fn label(code: &str) -> &str {
    let code = canonical(code);
    LANGUAGES.iter().find(|language| language.code == code).map_or(code, |language| language.name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_matches_the_recorded_account_picker_membership() {
        assert_eq!(LANGUAGES.len(), 190);
        let codes: std::collections::HashSet<_> = LANGUAGES.iter().map(|v| v.code).collect();
        assert_eq!(codes.len(), LANGUAGES.len());
        assert!(LANGUAGES.iter().all(|v| matches!(v.code.len(), 2 | 5) && !v.name.is_empty()));
        assert!(!["xx", "xn", "es-419"].iter().any(|v| codes.contains(v)));
    }

    #[test]
    fn a_deprecated_code_is_never_offered_and_reads_as_its_replacement() {
        assert!(LANGUAGES.iter().any(|v| v.code == "pb"), "the recorded catalog keeps it");
        assert!(picker().all(|v| v.code != "pb" && !v.name.contains("[deprecated]")));
        assert_eq!(picker().count(), LANGUAGES.len() - DEPRECATED.len());
        assert_eq!(canonical("pb"), "pt-BR");
        assert_eq!(canonical("pt-BR"), "pt-BR");
        assert_eq!(label("pb"), "Português Brasileiro");
        assert!(DEPRECATED.iter().all(|(_, new)| picker().any(|v| v.code == *new)));
    }

    #[test]
    fn regional_names_and_unfamiliar_saved_values_are_preserved() {
        assert_eq!(label("fr-CA"), "Français Canadien");
        assert_eq!(label("en-GB"), "English (UK)");
        assert_eq!(label("es-419"), "es-419");
        assert_eq!(label("new-language"), "new-language");
        assert_eq!(label(""), "");
    }
}
