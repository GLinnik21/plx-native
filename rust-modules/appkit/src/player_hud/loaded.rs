//! **The compiled HUD the player draws from**: a package's bytes parsed and compiled ONCE, never in the draw,
//! and handed to the screen as a value ([`LoadedHud`]).
//!
//! The owner (the application) builds one at boot (a label's width sets a pill's width, so the language is
//! part of the plan's cache key, [`CompileKey`]; the process's language is fixed, so that is the only
//! compile, but for a controlled run's and a dev package's, which come on the first player frame), and
//! pushes the [`Arc<Compiled>`] into the player page the way it pushes the frame's
//! [`HudModel`](super::model::HudModel).
//! Nothing here reads a registry on its own account: the language arrives as a [`LocaleContext`] (the
//! OWNER passes `plx_platform::i18n::current()`), the strings are built from it ([`AppLabels::in_language`]) and the
//! measurer is passed in. The draw ([`super::render`])
//! walks the result and computes nothing.
//!
//! `builtin:classic` is the only package that ships in this release, and it always loads (it is embedded
//! and a test parses and compiles it clean in every shipped language): [`load_classic`] cannot fail. A
//! dev package that does not parse is refused ([`DevStep::Report`]) and the owner keeps drawing classic.

use std::sync::Arc;

use plx_machine::machine::Measure;
use plx_platform::i18n::{Language, LocaleContext};

use crate::hud_format::compile::{compile, AppLabels, CompileCtx, CompileKey, Compiled};
use crate::hud_format::parse;

/// Why a package did not become a [`LoadedHud`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LoadError {
    /// the bytes did not parse; the count of findings
    Invalid(usize),
}

/// A package, compiled for one language: what the player page holds and draws.
#[derive(Clone, Debug)]
pub struct LoadedHud {
    /// the plan's cache key: the bytes' digest, the app release and the language
    pub key: CompileKey,
    /// the language the labels were taken in: a `Copy` enum, so [`DevPackage::refresh`] can ask "compiled for
    /// this language?" without building the key
    pub language: Language,
    pub compiled: Arc<Compiled>,
}

/// Parse and compile `bytes` for the language `cx` names (its labels, [`AppLabels::in_language`]) and `measure`.
pub fn load(bytes: &[u8], app_version: &str, cx: &LocaleContext, measure: &dyn Measure) -> Result<LoadedHud, LoadError> {
    let language = cx.language();
    let parsed = parse::parse(bytes);
    let Some(package) = parsed.package else {
        return Err(LoadError::Invalid(parsed.findings.len()));
    };
    let key = CompileKey { sha256: plx_base::sha256::sha256(bytes), app_version: app_version.to_owned(), lang: language.tag().to_owned() };
    Ok(LoadedHud { key, language, compiled: Arc::new(compile(&package, measure, CompileCtx { labels: AppLabels::in_language(cx) })) })
}

/// `builtin:classic`, the embedded package ([`crate::hud_format::CLASSIC_PACKAGE`]), compiled. It does not
/// return a `Result`: the bytes are `include_str!`'d and `classic_loads_with_no_findings_in_every_shipped_language`
/// parses and compiles them in all eleven languages, so a failure here is a build that no test passed.
pub fn load_classic(app_version: &str, cx: &LocaleContext, measure: &dyn Measure) -> LoadedHud {
    load(crate::hud_format::CLASSIC_PACKAGE.as_bytes(), app_version, cx, measure).expect("the embedded builtin:classic parses")
}

/// **`builtin:classic` for a test that builds a player page**: English on the fixture advances
/// (`FixtureMeasure`, the measurer every host rig lays out with), compiled once per call, outside any frame.
/// The ONE helper a test builder takes its `PlayerScreen::new` argument from.
#[cfg(any(test, feature = "test-support"))]
pub fn classic_for_test() -> Arc<Compiled> {
    load_classic("0.0.0-test", plx_platform::i18n::english(), &plx_ui::fixture::FixtureMeasure).compiled
}

/// **The dev trigger's value**: the path of the package to draw instead of classic, the first line of the
/// trigger file, trimmed. `None` for an empty file (nothing armed).
pub fn dev_package_path(value: &str) -> Option<&str> {
    let line = value.lines().next()?.trim();
    (!line.is_empty()).then_some(line)
}

/// What one [`DevPackage::refresh`] decided.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DevStep {
    /// the package is compiled for the language: [`DevPackage::hud`] answers it
    Ready,
    /// ...and this call compiled it (the first frame, or the language changed): the owner may say so
    Compiled,
    /// the package failed, and this is the one line to log about it
    Report(String),
    /// it failed before (for this language): classic draws, nothing to say
    Quiet,
}

/// **The dev package** (`plxnative-hud=<path>`): the bytes the owner read ONCE, what they compiled to for
/// the current language and what was already refused, so a bad file is neither re-read nor recompiled
/// every frame. The file read is the owner's (this module does no IO): it hands the bytes, or the read
/// error, to [`DevPackage::new`].
#[derive(Clone, Debug)]
pub struct DevPackage {
    path: String,
    /// the file's bytes, or why they could not be read
    bytes: Result<Vec<u8>, String>,
    loaded: Option<LoadedHud>,
    /// the language the package was last refused in (the bytes never change, so the key is the language)
    refused: Option<Language>,
    /// a read error has been reported
    told: bool,
}

impl DevPackage {
    pub fn new(path: String, bytes: Result<Vec<u8>, String>) -> Self {
        Self { path, bytes, loaded: None, refused: None, told: false }
    }

    /// The package compiled for the language of the last [`DevPackage::refresh`], when it loaded.
    pub fn hud(&self) -> Option<&LoadedHud> {
        self.loaded.as_ref()
    }

    /// Compile for `cx` when it has not been compiled for that language, remembering a refusal. The
    /// process's language is fixed, so in the app this compiles once; the language key keeps a test that
    /// changes its thread's locale honest.
    pub fn refresh(&mut self, app_version: &str, cx: &LocaleContext, measure: &dyn Measure) -> DevStep {
        let language = cx.language();
        let bytes = match &self.bytes {
            Ok(bytes) => bytes,
            Err(why) if !self.told => {
                self.told = true;
                return DevStep::Report(format!("{}: cannot read: {why}", self.path));
            }
            Err(_) => return DevStep::Quiet,
        };
        if self.loaded.as_ref().is_some_and(|h| h.language == language) {
            return DevStep::Ready;
        }
        if self.refused == Some(language) {
            return DevStep::Quiet;
        }
        match load(bytes, app_version, cx, measure) {
            Ok(hud) => {
                self.loaded = Some(hud);
                self.refused = None;
                DevStep::Compiled
            }
            Err(LoadError::Invalid(n)) => {
                self.loaded = None;
                self.refused = Some(language);
                DevStep::Report(format!("{}: not a package ({n} findings); classic draws", self.path))
            }
        }
    }
}

/// The dev package's state in the owner: unread until the first player frame, then armed or off.
#[derive(Clone, Debug, Default)]
pub enum DevHud {
    #[default]
    Unread,
    Off,
    Package(DevPackage),
}

#[cfg(test)]
mod tests {
    use super::*;
    use plx_base::fontcov::advances::ShippedMeasure;
    use plx_platform::i18n::SHIPPED;

    /// The embedded classic is a package the format accepts and the compiler lays out without a single
    /// finding (a number or gap keyed on a card term), in EVERY shipped language: a label's width is a
    /// pill's width, so the claim is per language.
    #[test]
    fn classic_loads_with_no_findings_in_every_shipped_language() {
        assert_eq!(SHIPPED.len(), 11, "the claim is about the eleven shipped languages");
        for language in SHIPPED {
            let cx = LocaleContext::resolve(language, None, None, None, None);
            let hud = load_classic("0.0.0-test", &cx, &ShippedMeasure);
            assert!(hud.compiled.findings().is_empty(), "{}: {:?}", language.tag(), hud.compiled.findings());
            assert!(!hud.compiled.states().is_empty(), "{}", language.tag());
            assert_eq!(hud.key.lang, hud.language.tag());
        }
    }

    /// **Classic answers every frame the player can be in**, in every shipped language: for every slot the
    /// HUD can hold, with and without chapters, as an episode or a movie, paused or playing, the compact
    /// state (`open == 0`) and its geometry exist. This is the proof that `frame_state` returning `None`
    /// cannot happen for `builtin:classic`: the player draws and places from the compile, and has no
    /// other layout behind it.
    #[test]
    fn classic_answers_every_frame_the_player_can_be_in() {
        use crate::hud_format::compile::SkipKind;
        use crate::hud_format::reach::Slot;
        let mut asked = 0;
        for language in SHIPPED {
            let cx = LocaleContext::resolve(language, None, None, None, None);
            let hud = load_classic("0.0.0-test", &cx, &ShippedMeasure);
            for slot in [Slot::Discs, Slot::UpNext, Slot::Skip] {
                for chapters in [false, true] {
                    for episode in [false, true] {
                        for paused in [false, true] {
                            asked += 1;
                            let at = format!("{} {slot:?} chapters={chapters} episode={episode} paused={paused}", language.tag());
                            let state = hud.compiled.lookup(0, slot, chapters, episode, paused).unwrap_or_else(|| panic!("no state for {at}"));
                            for skip in [SkipKind::Intro, SkipKind::Credits] {
                                assert_eq!(state.geometry(skip).band, hud.compiled.band(), "{at} {skip:?}");
                            }
                        }
                    }
                }
            }
        }
        assert_eq!(asked, SHIPPED.len() * 3 * 2 * 2 * 2, "the eleven languages x every combination");
    }

    /// The key is the bytes' digest: the same bytes in the same language are the same key, other bytes
    /// or another language are not, so the owner recompiles exactly when something changed.
    #[test]
    fn the_key_names_the_bytes_the_release_and_the_language() {
        let en = plx_platform::i18n::english();
        let a = load_classic("1.0", en, &ShippedMeasure).key;
        assert_eq!(a, load_classic("1.0", en, &ShippedMeasure).key);
        assert_ne!(a, load_classic("1.1", en, &ShippedMeasure).key, "the release is part of the key");
        let other = SHIPPED.into_iter().find(|p| p.language() != Some(plx_platform::i18n::Language::En)).expect("a shipped language other than English");
        let de = LocaleContext::resolve(other, None, None, None, None);
        assert_ne!(a, load_classic("1.0", &de, &ShippedMeasure).key, "the language is part of the key");
        assert_eq!(a.sha256, plx_base::sha256::sha256(crate::hud_format::CLASSIC_PACKAGE.as_bytes()));
    }

    /// A package that does not parse is refused, never half-loaded: the screen keeps the layout it has.
    #[test]
    fn a_package_that_does_not_parse_is_refused() {
        let err = load(b"{ not json", "1.0", plx_platform::i18n::english(), &ShippedMeasure);
        assert!(matches!(err, Err(LoadError::Invalid(n)) if n > 0));
    }
    /// The dev trigger's value is the package's path: its first line, trimmed. An empty file arms nothing.
    #[test]
    fn the_dev_trigger_names_a_path_or_nothing() {
        assert_eq!(dev_package_path("/tmp/x.plxhud.json"), Some("/tmp/x.plxhud.json"));
        assert_eq!(dev_package_path("  /tmp/x.plxhud.json \n second line"), Some("/tmp/x.plxhud.json"));
        assert_eq!(dev_package_path(""), None);
        assert_eq!(dev_package_path("   \n"), None);
    }

    /// A dev package that loads is what the screen gets; a bad file reports ONCE, is never compiled again
    /// (for that language), and leaves the owner on classic.
    #[test]
    fn a_dev_package_that_does_not_load_reports_once_and_is_not_recompiled() {
        let en = plx_platform::i18n::english();
        let mut bad = DevPackage::new("/x/bad.json".to_owned(), Ok(b"{ not json".to_vec()));
        assert!(bad.hud().is_none());
        assert!(matches!(bad.refresh("1.0", en, &ShippedMeasure), DevStep::Report(m) if m.contains("/x/bad.json")));
        assert_eq!(bad.refresh("1.0", en, &ShippedMeasure), DevStep::Quiet, "a remembered failure is not retried");
        assert_eq!(bad.refresh("1.0", en, &ShippedMeasure), DevStep::Quiet);
        assert!(bad.hud().is_none(), "classic draws");
        // another language is another key: it is tried (and fails) once more
        let other = SHIPPED.into_iter().find(|p| p.language() != Some(plx_platform::i18n::Language::En)).unwrap();
        let de = LocaleContext::resolve(other, None, None, None, None);
        assert!(matches!(bad.refresh("1.0", &de, &ShippedMeasure), DevStep::Report(_)));
        assert_eq!(bad.refresh("1.0", &de, &ShippedMeasure), DevStep::Quiet);
    }

    /// A file that cannot be read is reported once and never read again.
    #[test]
    fn a_dev_package_that_cannot_be_read_reports_once() {
        let en = plx_platform::i18n::english();
        let mut gone = DevPackage::new("/x/gone.json".to_owned(), Err("No such file".to_owned()));
        assert!(matches!(gone.refresh("1.0", en, &ShippedMeasure), DevStep::Report(m) if m.contains("No such file")));
        assert_eq!(gone.refresh("1.0", en, &ShippedMeasure), DevStep::Quiet);
        assert!(gone.hud().is_none());
    }

    /// A good dev package compiles once per language and is the same `Arc` every frame after, until the
    /// language changes.
    #[test]
    fn a_dev_package_compiles_once_per_language() {
        let en = plx_platform::i18n::english();
        let mut dev = DevPackage::new("/x/classic.json".to_owned(), Ok(crate::hud_format::CLASSIC_PACKAGE.as_bytes().to_vec()));
        assert_eq!(dev.refresh("1.0", en, &ShippedMeasure), DevStep::Compiled);
        let first = Arc::clone(&dev.hud().expect("loaded").compiled);
        assert_eq!(dev.refresh("1.0", en, &ShippedMeasure), DevStep::Ready);
        assert!(Arc::ptr_eq(&first, &dev.hud().unwrap().compiled), "not recompiled per frame");
        let other = SHIPPED.into_iter().find(|p| p.language() != Some(plx_platform::i18n::Language::En)).unwrap();
        let de = LocaleContext::resolve(other, None, None, None, None);
        assert_eq!(dev.refresh("1.0", &de, &ShippedMeasure), DevStep::Compiled);
        assert!(!Arc::ptr_eq(&first, &dev.hud().unwrap().compiled), "a language change recompiles");
    }
}
