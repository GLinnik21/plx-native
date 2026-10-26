//! Immutable per-launch localization. Catalog source is JSON; generated accessors are typed.
use icu_decimal::{input::Decimal, DecimalFormatter};
use icu_locale::Locale;
use icu_plurals::{PluralCategory, PluralRules};
use serde::{Deserialize, Deserializer, Serialize};
use std::sync::OnceLock;

pub const CONTRIBUTE_URL: &str =
    "https://github.com/GLinnik21/plx-native/blob/main/docs/localization.md";

// `Preference`, `Language`, `SHIPPED` and every table over them are generated from
// `locales/languages.json` (see `platform/build_support/catalog.rs`); this file holds only the
// behavior around them. A shipped tag is never renamed: it is what the preferences store.
include!(concat!(env!("OUT_DIR"), "/languages.rs"));
impl<'de> Deserialize<'de> for Preference {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let v = serde_json::Value::deserialize(d)?;
        Ok(Self::from_tag(v.as_str().unwrap_or("system")))
    }
}
impl Preference {
    /// Serde's `skip_serializing_if`: System is what an absent preference already means.
    pub fn is_system(&self) -> bool {
        *self == Self::System
    }
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Clock {
    Locale,
    H12,
    H24,
}

pub struct LocaleContext {
    preference: Preference,
    language: Language,
    format_locale: String,
    clock: Clock,
    plural: PluralRules,
    decimal: DecimalFormatter,
    date: icu_datetime::FixedCalendarDateTimeFormatter<
        icu_calendar::Gregorian,
        icu_datetime::fieldsets::YMD,
    >,
}
impl LocaleContext {
    /// Exercise expanded catalogs without changing the process-wide locale or environment.
    #[cfg(any(test, feature = "test-support"))]
    pub fn pseudo_for_test() -> Self {
        let mut context = Self::resolve(Preference::En, None, None, None, None);
        context.language = Language::Pseudo;
        context
    }

    pub fn resolve(
        preference: Preference,
        ui: Option<&str>,
        fmt: Option<&str>,
        clock: Option<&str>,
        env: Option<&str>,
    ) -> Self {
        let selected = if preference == Preference::System {
            ui.and_then(normalize)
                .or_else(|| env.and_then(normalize))
                .unwrap_or_else(|| "en".into())
        } else {
            preference.tag().into()
        };
        let language = Language::parse(&selected);
        let format_locale = fmt
            .and_then(normalize)
            .or_else(|| {
                let selected = selected.parse::<Locale>().ok()?;
                (selected.id.language.as_str() == language.tag() && selected.id.region.is_some())
                    .then(|| selected.to_string())
            })
            .unwrap_or_else(|| language.regional().into());
        let locale = format_locale.parse::<Locale>().expect("validated locale");
        let decimal = DecimalFormatter::try_new(locale.clone().into(), Default::default())
            .unwrap_or_else(|_| {
                DecimalFormatter::try_new(Default::default(), Default::default())
                    .expect("compiled English number data")
            });
        let date = icu_datetime::FixedCalendarDateTimeFormatter::try_new(
            locale.into(),
            icu_datetime::fieldsets::YMD::short()
                .with_year_style(icu_datetime::options::YearStyle::Full),
        )
        .unwrap_or_else(|_| {
            icu_datetime::FixedCalendarDateTimeFormatter::try_new(
                Default::default(),
                icu_datetime::fieldsets::YMD::short()
                    .with_year_style(icu_datetime::options::YearStyle::Full),
            )
            .expect("compiled English date data")
        });
        // The regional locale, not the bare tag: pt-PT counts 0 as plural where bare `pt` (pt-BR) does not.
        let plural =
            PluralRules::try_new_cardinal(language.regional().parse::<Locale>().unwrap().into())
                .expect("compiled catalog plural data");
        Self {
            preference,
            language,
            format_locale,
            clock: match clock {
                Some("12") => Clock::H12,
                Some("24") => Clock::H24,
                _ => Clock::Locale,
            },
            plural,
            decimal,
            date,
        }
    }
    pub fn preference(&self) -> Preference {
        self.preference
    }
    pub fn language(&self) -> Language {
        self.language
    }
    pub fn format_locale(&self) -> &str {
        &self.format_locale
    }
    pub fn clock(&self) -> Clock {
        self.clock
    }
    pub fn plural(&self, n: i64) -> PluralCategory {
        self.plural.category_for(n.unsigned_abs())
    }
    pub fn number(&self, n: i64) -> String {
        self.decimal.format(&Decimal::from(n)).to_string()
    }
    pub fn decimal(&self, n: i64, scale: i16) -> String {
        let mut d = Decimal::from(n);
        d.multiply_pow10(-scale);
        d.pad_end(-scale);
        self.decimal.format(&d).to_string()
    }
    pub fn date(&self, year: i32, month: u8, day: u8) -> Option<String> {
        let d = icu_calendar::Date::try_new_gregorian(year, month, day).ok()?;
        Some(self.date.format(&d).to_string())
    }
    /// Day and month with no year, for release captions: "12 Mar" / "12 March" in English,
    /// "12 de marzo" in Spanish, "12 mars" / "12 janv." in French, "12 marzo" in Italian,
    /// "12 de março" in European Portuguese, "12. März" in German, "március 12." in Hungarian, and in Belarusian and Russian the genitive
    /// after the day ("12 сакавіка", "12 марта") with the abbreviated form for medium. Hand-written tables, not
    /// ICU month data, so no name data is compiled in for the shipped languages. The year is
    /// unknown, so 29 February is a valid day; a day the calendar lacks returns `None`.
    pub fn month_day(&self, month: u8, day: u8, width: MonthWidth) -> Option<String> {
        // Validate against a leap year first: `month - 1` below is only reached for 1..=12.
        icu_calendar::Date::try_new_gregorian(2024, month, day).ok()?;
        let i = usize::from(month - 1);
        // Hungarian puts the month first and the day after it, with the ordinal dot: "március 12.".
        if self.language == Language::Hu {
            let names = match width {
                MonthWidth::Medium => &HU_MEDIUM,
                MonthWidth::Long => &HU_LONG,
            };
            return Some(format!("{} {day}.", names[i]));
        }
        let (names, joiner) = match (self.language, width) {
            (Language::En | Language::Pseudo, MonthWidth::Medium) => (&EN_MEDIUM, " "),
            (Language::En | Language::Pseudo, MonthWidth::Long) => (&EN_LONG, " "),
            (Language::Es, MonthWidth::Medium) => (&ES_MEDIUM, " "),
            (Language::Es, MonthWidth::Long) => (&ES_LONG, " de "),
            (Language::Be, MonthWidth::Medium) => (&BE_MEDIUM, " "),
            (Language::Be, MonthWidth::Long) => (&BE_GENITIVE, " "),
            (Language::Ru, MonthWidth::Medium) => (&RU_MEDIUM, " "),
            (Language::Ru, MonthWidth::Long) => (&RU_GENITIVE, " "),
            (Language::Fr, MonthWidth::Medium) => (&FR_MEDIUM, " "),
            (Language::Fr, MonthWidth::Long) => (&FR_LONG, " "),
            (Language::It, MonthWidth::Medium) => (&IT_MEDIUM, " "),
            (Language::It, MonthWidth::Long) => (&IT_LONG, " "),
            (Language::Pt, MonthWidth::Medium) => (&PT_MEDIUM, " "),
            (Language::Pt, MonthWidth::Long) => (&PT_LONG, " de "),
            (Language::De, MonthWidth::Medium) => (&DE_MEDIUM, ". "),
            (Language::De, MonthWidth::Long) => (&DE_LONG, ". "),
            (Language::Hu, _) => unreachable!("Hungarian returned above"),
        };
        Some(format!("{day}{joiner}{}", names[i]))
    }
}

/// Width of a month-and-day caption: `Medium` is the abbreviated month, `Long` the full one.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MonthWidth {
    Medium,
    Long,
}

/// CLDR `ru` format months: genitive after a day, including abbreviated "мая".
const RU_MEDIUM: [&str; 12] = [
    "янв.", "февр.", "мар.", "апр.", "мая", "июн.", "июл.", "авг.", "сент.", "окт.", "нояб.", "дек.",
];
const RU_GENITIVE: [&str; 12] = [
    "января", "февраля", "марта", "апреля", "мая", "июня", "июля", "августа", "сентября", "октября",
    "ноября", "декабря",
];

/// CLDR `hu` abbreviated format months, with their dots.
const HU_MEDIUM: [&str; 12] =
    ["jan.", "febr.", "márc.", "ápr.", "máj.", "jún.", "júl.", "aug.", "szept.", "okt.", "nov.", "dec."];
const HU_LONG: [&str; 12] = [
    "január", "február", "március", "április", "május", "június", "július", "augusztus", "szeptember",
    "október", "november", "december",
];
const EN_MEDIUM: [&str; 12] =
    ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
const EN_LONG: [&str; 12] = [
    "January", "February", "March", "April", "May", "June", "July", "August", "September",
    "October", "November", "December",
];
const ES_MEDIUM: [&str; 12] =
    ["ene", "feb", "mar", "abr", "may", "jun", "jul", "ago", "sept", "oct", "nov", "dic"];
const ES_LONG: [&str; 12] = [
    "enero", "febrero", "marzo", "abril", "mayo", "junio", "julio", "agosto", "septiembre",
    "octubre", "noviembre", "diciembre",
];
/// CLDR `be` abbreviated format months: three letters, no dot, and "мая" (the genitive) for May.
const BE_MEDIUM: [&str; 12] =
    ["сту", "лют", "сак", "кра", "мая", "чэр", "ліп", "жні", "вер", "кас", "ліс", "сне"];
/// Genitive, because a day number governs the month in Belarusian ("12 сакавіка").
const BE_GENITIVE: [&str; 12] = [
    "студзеня", "лютага", "сакавіка", "красавіка", "траўня", "чэрвеня", "ліпеня", "жніўня",
    "верасня", "кастрычніка", "лістапада", "снежня",
];
const FR_MEDIUM: [&str; 12] =
    ["janv.", "févr.", "mars", "avr.", "mai", "juin", "juil.", "août", "sept.", "oct.", "nov.", "déc."];
const FR_LONG: [&str; 12] = [
    "janvier", "février", "mars", "avril", "mai", "juin", "juillet", "août", "septembre",
    "octobre", "novembre", "décembre",
];
const IT_MEDIUM: [&str; 12] =
    ["gen", "feb", "mar", "apr", "mag", "giu", "lug", "ago", "set", "ott", "nov", "dic"];
const IT_LONG: [&str; 12] = [
    "gennaio", "febbraio", "marzo", "aprile", "maggio", "giugno", "luglio", "agosto", "settembre",
    "ottobre", "novembre", "dicembre",
];
const PT_MEDIUM: [&str; 12] =
    ["jan.", "fev.", "mar.", "abr.", "mai.", "jun.", "jul.", "ago.", "set.", "out.", "nov.", "dez."];
const PT_LONG: [&str; 12] = [
    "janeiro", "fevereiro", "março", "abril", "maio", "junho", "julho", "agosto", "setembro",
    "outubro", "novembro", "dezembro",
];
/// German writes the day as an ordinal: "12. März".
const DE_MEDIUM: [&str; 12] =
    ["Jan.", "Feb.", "März", "Apr.", "Mai", "Juni", "Juli", "Aug.", "Sept.", "Okt.", "Nov.", "Dez."];
const DE_LONG: [&str; 12] = [
    "Januar", "Februar", "März", "April", "Mai", "Juni", "Juli", "August", "September", "Oktober",
    "November", "Dezember",
];
/// Reject control bytes and invalid tags before they can become HTTP headers. POSIX suffixes
/// are only used at this input boundary; callers retain canonical BCP-47 tags.
pub fn normalize(s: &str) -> Option<String> {
    if !s.is_ascii() || s.bytes().any(|b| b.is_ascii_control()) {
        return None;
    }
    let base = s.trim().split(['.', '@']).next()?.replace('_', "-");
    if base.eq_ignore_ascii_case("C") || base.eq_ignore_ascii_case("POSIX") {
        return None;
    }
    let l = base.parse::<Locale>().ok()?;
    (l.id.language.as_str() != "und").then(|| l.to_string())
}
/// `webOS 4.10.2`, or the English "webOS unknown" read-out when the firmware file could not
/// be read — the release is the one field a stranger's report needs, and "unknown" is the honest
/// reading of an empty one rather than a plausible default. Shared by the support lines of the
/// failure read-out and of the sign-in report.
pub fn webos_release_line(info: &crate::tv::device::Info) -> String {
    if info.major == 0 {
        msg::browse_diagnostics_unknown_os().to_string()
    } else {
        format!("webOS {}", info.release)
    }
}
static CURRENT: OnceLock<LocaleContext> = OnceLock::new();
static SAVED_PREFERENCE: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(0);

/// The confirmed next-launch setting. Reading it never touches credential storage.
pub fn saved_preference() -> Preference {
    Preference::from_slot(SAVED_PREFERENCE.load(std::sync::atomic::Ordering::Acquire))
}

pub fn set_saved_preference(value: Preference) {
    SAVED_PREFERENCE.store(value.slot(), std::sync::atomic::Ordering::Release);
}

#[cfg(any(test, feature = "test-support"))]
pub fn saved_preference_for_test(value: Preference) -> Preference {
    plx_base::testlock::assert_held("saved language preference");
    let previous = saved_preference();
    set_saved_preference(value);
    previous
}

thread_local! {
    static THREAD_LOCALE: std::cell::Cell<Option<&'static LocaleContext>> = const { std::cell::Cell::new(None) };
}

/// The fixed English context behind every `"translate": false` message: English plural rules and
/// en-US number formatting, whatever the UI language or the TV's format locale. The generated
/// accessors of an English-only message call this instead of [`current`].
pub fn english() -> &'static LocaleContext {
    static ENGLISH: OnceLock<LocaleContext> = OnceLock::new();
    ENGLISH.get_or_init(|| LocaleContext::resolve(Preference::En, None, Some("en-US"), Some("24"), None))
}

/// Run `f` with [`current`] answering [`english`] on THIS thread, then restore what it answered
/// before (so it nests, and nests under the test overrides). It is how the diagnostics panel
/// composes its shared, translated helpers (units, decimals, quality labels) in English.
///
/// **Nothing reached inside `f` may cache localized text process-wide** (a `OnceLock<String>`
/// first touched here would freeze English for the whole app). None does today; keep it so.
pub fn render_in_english<R>(f: impl FnOnce() -> R) -> R {
    struct Restore(Option<&'static LocaleContext>);
    impl Drop for Restore {
        fn drop(&mut self) {
            THREAD_LOCALE.with(|slot| slot.set(self.0));
        }
    }
    let _restore = Restore(THREAD_LOCALE.with(|slot| slot.replace(Some(english()))));
    f()
}

/// Resolve every catalog accessor on THIS test thread in the expanded pseudo-locale until the
/// guard drops. Tests run in parallel, so the process-wide [`current`] must stay English for every
/// other test; a thread-local override is what lets one test draw a whole screen in `[!! … !!]`.
#[cfg(any(test, feature = "test-support"))]
pub fn pseudo_on_this_thread_for_test() -> ThreadLocaleGuard {
    static PSEUDO: OnceLock<LocaleContext> = OnceLock::new();
    let pseudo = PSEUDO.get_or_init(LocaleContext::pseudo_for_test);
    ThreadLocaleGuard(THREAD_LOCALE.with(|slot| slot.replace(Some(pseudo))))
}

/// Resolve every catalog accessor on THIS test thread in one shipped UI language until the guard
/// drops — the pseudo-locale's thread-local door, for tests that measure real translations.
#[cfg(any(test, feature = "test-support"))]
pub fn language_on_this_thread_for_test(preference: Preference) -> ThreadLocaleGuard {
    static CONTEXTS: [OnceLock<LocaleContext>; Language::SHIPPED.len()] =
        [const { OnceLock::new() }; Language::SHIPPED.len()];
    // System resolves to English here, as a test without a platform locale does.
    let slot = &CONTEXTS[preference.language().unwrap_or(Language::En).index()];
    let cx = slot.get_or_init(|| LocaleContext::resolve(preference, None, None, None, None));
    ThreadLocaleGuard(THREAD_LOCALE.with(|slot| slot.replace(Some(cx))))
}

#[cfg(any(test, feature = "test-support"))]
pub struct ThreadLocaleGuard(Option<&'static LocaleContext>);

#[cfg(any(test, feature = "test-support"))]
impl Drop for ThreadLocaleGuard {
    fn drop(&mut self) {
        THREAD_LOCALE.with(|slot| slot.set(self.0));
    }
}

pub fn current() -> &'static LocaleContext {
    if let Some(cx) = THREAD_LOCALE.with(std::cell::Cell::get) {
        return cx;
    }
    #[cfg(any(test, feature = "test-support"))]
    {
        CURRENT.get_or_init(|| LocaleContext::resolve(Preference::En, None, None, None, None))
    }
    #[cfg(not(any(test, feature = "test-support")))]
    {
        CURRENT
            .get()
            .expect("locale initialized before any screen or Plex request")
    }
}
// Always compiled, though no test calls it: the application's boot calls it from code that is
// itself `cfg(not(test))`, and that code builds against this crate WITH `test-support` whenever
// cargo unifies a dependent's dev-dependency features into a non-test build of the dependent.
pub fn initialize(preference: Preference, controlled: bool) {
    set_saved_preference(preference);
    // Record/replay must not depend on the TV or process environment. The preference is part
    // of the captured session; System resolves to English and formatting is fixed in both modes.
    if controlled {
        let _ = CURRENT.set(LocaleContext::resolve(
            preference,
            Some("en-US"),
            Some("en-US"),
            Some("24"),
            None,
        ));
        return;
    }
    let info = platform_locale();
    let env = ["LC_ALL", "LC_MESSAGES", "LANG"]
        .iter()
        .find_map(|k| std::env::var(k).ok().filter(|s| !s.is_empty()));
    let (ui, fmt, clock) = info
        .as_ref()
        .map(|v| (v.ui.as_deref(), v.fmt.as_deref(), v.clock.as_deref()))
        .unwrap_or_default();
    #[cfg(feature = "hostsim")]
    let simulated_format = std::env::var("PLXNATIVE_FORMAT_LOCALE").ok();
    #[cfg(feature = "hostsim")]
    let fmt = simulated_format.as_deref().or(fmt);
    #[allow(unused_mut)]
    let mut cx = LocaleContext::resolve(preference, ui, fmt, clock, env.as_deref());
    #[cfg(feature = "hostsim")]
    if let Ok(forced) = std::env::var("PLXNATIVE_LOCALE") {
        cx = LocaleContext::resolve(
            Preference::from_tag(&forced),
            Some(&forced),
            fmt,
            clock,
            None,
        );
        if forced == "qps-ploc" {
            cx.language = Language::Pseudo;
        }
    }
    plx_base::eventlog::log(&format!(
        "locale: source={} preference={} ui={} format={} clock={:?}",
        if info.is_some() {
            "settings"
        } else if env.is_some() {
            "environment"
        } else {
            "fallback"
        },
        cx.preference().tag(),
        cx.language().tag(),
        cx.format_locale(),
        cx.clock()
    ));
    if CURRENT.set(cx).is_err() {
        plx_base::eventlog::log("locale: initialization already completed");
    }
}
#[derive(Default)]
struct SystemLocale {
    ui: Option<String>,
    fmt: Option<String>,
    clock: Option<String>,
}
fn parse_reply(raw: &str) -> Option<SystemLocale> {
    let v: serde_json::Value = serde_json::from_str(raw).ok()?;
    if v["returnValue"].as_bool() != Some(true) {
        return None;
    }
    let info = &v["settings"]["localeInfo"];
    if !info.is_object() {
        return None;
    }
    Some(SystemLocale {
        ui: info["locales"]["UI"].as_str().and_then(normalize),
        fmt: info["locales"]["FMT"].as_str().and_then(normalize),
        clock: info["clock"].as_str().map(str::to_string),
    })
}
fn platform_locale() -> Option<SystemLocale> {
    match crate::tv::system_locale() {
        crate::tv::LocaleReply::NoPlatform => None,
        crate::tv::LocaleReply::Reply(raw) => {
            let info = parse_reply(&raw);
            if info.is_none() {
                plx_base::eventlog::log("locale: settings refused or returned malformed localeInfo");
            }
            info
        }
        crate::tv::LocaleReply::Unavailable => {
            plx_base::eventlog::log("locale: settings unavailable; using fallback");
            None
        }
    }
}

// Every generated key has str/CStr and explicit-context variants; not every consumer needs all
// four. Keep the allowance on this generated API rather than suppressing handwritten warnings.
#[allow(dead_code)]
pub mod msg {
    include!(concat!(env!("OUT_DIR"), "/messages.rs"));
}
#[cfg(test)]
#[allow(dead_code)]
#[path = "../../build_support/catalog.rs"]
mod catalog_tests;
#[cfg(test)]
mod tests;
