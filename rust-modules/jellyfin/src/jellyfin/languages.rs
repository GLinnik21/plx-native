//! Language codes between Jellyfin's spelling and the app's.
//!
//! Jellyfin stores a user's language preferences as ISO 639-2 bibliographic codes (`por`, `fre`,
//! `ger`), the first column of the server's own `Localization/iso6392.txt`, plus regional codes of
//! its own (`pob`, `pop`, `frc`). The app's language preferences and pickers use Plex's account
//! codes: ISO 639-1 (`pt`, `fr`) and a few regional tags (`pt-BR`, `fr-CA`).
//! Data: jellyfin `release-10.10.z`, `Emby.Server.Implementations/Localization/iso6392.txt`.

/// `(app code, Jellyfin code)` for every language of the server's list that has a two-letter code.
const CODES: &[(&str, &str)] = &[
    ("aa", "aar"), ("ab", "abk"), ("ae", "ave"), ("af", "afr"), ("ak", "aka"), ("am", "amh"),
    ("an", "arg"), ("ar", "ara"), ("as", "asm"), ("av", "ava"), ("ay", "aym"), ("az", "aze"),
    ("ba", "bak"), ("be", "bel"), ("bg", "bul"), ("bh", "bih"), ("bi", "bis"), ("bm", "bam"),
    ("bn", "ben"), ("bo", "tib"), ("br", "bre"), ("bs", "bos"), ("ca", "cat"), ("ce", "che"),
    ("ch", "cha"), ("co", "cos"), ("cr", "cre"), ("cs", "cze"), ("cu", "chu"), ("cv", "chv"),
    ("cy", "wel"), ("da", "dan"), ("de", "ger"), ("dv", "div"), ("dz", "dzo"), ("ee", "ewe"),
    ("el", "gre"), ("en", "eng"), ("eo", "epo"), ("es", "spa"), ("et", "est"), ("eu", "baq"),
    ("fa", "per"), ("ff", "ful"), ("fi", "fin"), ("fj", "fij"), ("fo", "fao"), ("fr", "fre"),
    ("fy", "fry"), ("ga", "gle"), ("gd", "gla"), ("gl", "glg"), ("gn", "grn"), ("gu", "guj"),
    ("gv", "glv"), ("ha", "hau"), ("he", "heb"), ("hi", "hin"), ("ho", "hmo"), ("hr", "hrv"),
    ("ht", "hat"), ("hu", "hun"), ("hy", "arm"), ("hz", "her"), ("ia", "ina"), ("id", "ind"),
    ("ie", "ile"), ("ig", "ibo"), ("ii", "iii"), ("ik", "ipk"), ("io", "ido"), ("is", "ice"),
    ("it", "ita"), ("iu", "iku"), ("ja", "jpn"), ("jv", "jav"), ("ka", "geo"), ("kg", "kon"),
    ("ki", "kik"), ("kj", "kua"), ("kk", "kaz"), ("kl", "kal"), ("km", "khm"), ("kn", "kan"),
    ("ko", "kor"), ("kr", "kau"), ("ks", "kas"), ("ku", "kur"), ("kv", "kom"), ("kw", "cor"),
    ("ky", "kir"), ("la", "lat"), ("lb", "ltz"), ("lg", "lug"), ("li", "lim"), ("ln", "lin"),
    ("lo", "lao"), ("lt", "lit"), ("lu", "lub"), ("lv", "lav"), ("mg", "mlg"), ("mh", "mah"),
    ("mi", "mao"), ("mk", "mac"), ("ml", "mal"), ("mn", "mon"), ("mr", "mar"), ("ms", "may"),
    ("mt", "mlt"), ("my", "bur"), ("na", "nau"), ("nb", "nob"), ("nd", "nde"), ("ne", "nep"),
    ("ng", "ndo"), ("nl", "dut"), ("nn", "nno"), ("no", "nor"), ("nr", "nbl"), ("nv", "nav"),
    ("ny", "nya"), ("oc", "oci"), ("oj", "oji"), ("om", "orm"), ("or", "ori"), ("os", "oss"),
    ("pa", "pan"), ("pi", "pli"), ("pl", "pol"), ("ps", "pus"), ("pt", "por"), ("qu", "que"),
    ("rm", "roh"), ("rn", "run"), ("ro", "rum"), ("ru", "rus"), ("rw", "kin"), ("sa", "san"),
    ("sc", "srd"), ("sd", "snd"), ("se", "sme"), ("sg", "sag"), ("si", "sin"), ("sk", "slo"),
    ("sl", "slv"), ("sm", "smo"), ("sn", "sna"), ("so", "som"), ("sq", "alb"), ("sr", "srp"),
    ("ss", "ssw"), ("st", "sot"), ("su", "sun"), ("sv", "swe"), ("sw", "swa"), ("ta", "tam"),
    ("te", "tel"), ("tg", "tgk"), ("th", "tha"), ("ti", "tir"), ("tk", "tuk"), ("tl", "tgl"),
    ("tn", "tsn"), ("to", "ton"), ("tr", "tur"), ("ts", "tso"), ("tt", "tat"), ("tw", "twi"),
    ("ty", "tah"), ("ug", "uig"), ("uk", "ukr"), ("ur", "urd"), ("uz", "uzb"), ("ve", "ven"),
    ("vi", "vie"), ("vo", "vol"), ("wa", "wln"), ("wo", "wol"), ("xh", "xho"), ("yi", "yid"),
    ("yo", "yor"), ("za", "zha"), ("zh", "chi"), ("zu", "zul"),
];

/// The regional codes both sides name.
const REGIONAL: &[(&str, &str)] = &[("pt-BR", "pob"), ("fr-CA", "frc")];

/// Other spellings a configuration may carry, and the code they read as: Jellyfin's `pop`
/// (Portugal), which has no app code of its own, and the ISO 639-2 terminology codes another client
/// may have written in place of the bibliographic ones Jellyfin's list uses.
const ALIASES: &[(&str, &str)] = &[
    ("pop", "por"),
    ("bod", "tib"), ("ces", "cze"), ("cym", "wel"), ("deu", "ger"), ("ell", "gre"), ("eus", "baq"),
    ("fas", "per"), ("fra", "fre"), ("hye", "arm"), ("isl", "ice"), ("kat", "geo"), ("mkd", "mac"),
    ("mri", "mao"), ("msa", "may"), ("mya", "bur"), ("nld", "dut"), ("ron", "rum"), ("scc", "srp"),
    ("slk", "slo"), ("sqi", "alb"), ("zho", "chi"),
];

/// A Jellyfin language code as the app's. A code the tables do not know passes through as it is,
/// so a preference they cannot name is still shown, and saved back, unchanged.
pub fn to_app(code: &str) -> String {
    let code = code.trim();
    let lower = code.to_ascii_lowercase();
    let bib = ALIASES.iter().find(|(t, _)| *t == lower).map_or(lower.as_str(), |(_, b)| b);
    REGIONAL
        .iter()
        .chain(CODES)
        .find(|(_, jf)| *jf == bib)
        .map_or_else(|| code.to_string(), |(app, _)| (*app).to_string())
}

/// An app language code as Jellyfin's. A regional tag Jellyfin has no code for (`en-GB`, `zh-TW`)
/// saves as its language's (`eng`, `chi`).
pub fn to_jellyfin(code: &str) -> String {
    let code = plx_plex::plex::languages::canonical(code.trim());
    if let Some((_, jf)) = REGIONAL.iter().find(|(app, _)| app.eq_ignore_ascii_case(code)) {
        return (*jf).to_string();
    }
    let primary = code.split(['-', '_']).next().unwrap_or("").to_ascii_lowercase();
    CODES
        .iter()
        .find(|(app, _)| *app == primary)
        .map_or_else(|| code.to_string(), |(_, jf)| (*jf).to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn common_languages_map_both_ways() {
        for (app, jf) in [("pt", "por"), ("en", "eng"), ("fr", "fre"), ("de", "ger"), ("es", "spa"), ("ja", "jpn"), ("zh", "chi")] {
            assert_eq!(to_app(jf), app);
            assert_eq!(to_jellyfin(app), jf);
        }
    }

    #[test]
    fn regional_and_alternative_spellings() {
        assert_eq!((to_app("pob"), to_jellyfin("pt-BR")), ("pt-BR".to_string(), "pob".to_string()));
        assert_eq!((to_app("frc"), to_jellyfin("fr-CA")), ("fr-CA".to_string(), "frc".to_string()));
        assert_eq!(to_app("pop"), "pt");
        assert_eq!((to_app("deu"), to_app("FRA"), to_app(" nld ")), ("de".into(), "fr".into(), "nl".into()));
        assert_eq!(
            (to_jellyfin("en-GB"), to_jellyfin("zh-TW"), to_jellyfin("pb")),
            ("eng".into(), "chi".into(), "pob".into())
        );
    }

    #[test]
    fn unknown_codes_pass_through() {
        assert_eq!(to_app("fil"), "fil");
        assert_eq!(to_jellyfin("xx-YY"), "xx-YY");
        assert_eq!((to_app(""), to_jellyfin("")), (String::new(), String::new()));
    }
}
