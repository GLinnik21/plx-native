//! **The compiled HUD the player draws from**: a package's bytes parsed and compiled ONCE, outside any
//! frame, and handed to the screen as a value ([`LoadedHud`]).
//!
//! The owner (the application) builds one at boot and again when the language changes (a label's width
//! sets a pill's width, so the language is part of the plan's cache key, [`CompileKey`]), and pushes the
//! [`Arc<Compiled>`] into the player page the way it pushes the frame's [`HudModel`](super::model::HudModel).
//! Nothing here reads a registry on its own account: the language arrives as a [`LocaleContext`] (the
//! OWNER passes `plx_platform::i18n::current()`), the strings are built from it ([`labels_in`]) and the
//! measurer is passed in. The draw ([`super::render`])
//! walks the result and computes nothing.
//!
//! `builtin:classic` is the only package that ships in this release; a failure to load whatever the
//! owner asked for (a dev package that does not parse) leaves the screen with `None`, which draws the
//! hand-written classic layout as it always has.

use std::sync::Arc;

use plx_machine::machine::Measure;
use plx_platform::i18n::{msg, Language, LocaleContext};

use crate::hud_format::compile::{compile, AppLabels, CompileCtx, CompileKey, Compiled};
use crate::hud_format::parse;

/// **The app's own strings in the language `cx` names**: the production constructor of [`AppLabels`].
/// The language is an ARGUMENT, so nothing here reads the process-wide current locale: the app layer
/// passes `plx_platform::i18n::current()` when it compiles (outside the frame), a test passes the
/// context it is grading, and the compiler receives the result as data.
pub fn labels_in(cx: &LocaleContext) -> AppLabels {
    AppLabels {
        tab_info: msg::widgets_player_info_in(cx).to_owned(),
        tab_chapters: msg::widgets_player_chapters_in(cx).to_owned(),
        next_episode: msg::widgets_next_episode_in(cx).to_owned(),
        watch_credits: msg::widgets_next_credits_in(cx).to_owned(),
        skip_intro: msg::widgets_skip_intro_in(cx).to_owned(),
        skip_credits: msg::widgets_skip_credits_in(cx).to_owned(),
        from_beginning: msg::widgets_info_from_beginning_in(cx).to_owned(),
        go_to_show: msg::widgets_info_go_show_in(cx).to_owned(),
        go_to_movie: msg::widgets_info_go_movie_in(cx).to_owned(),
    }
}

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
    /// the language the labels were taken in: comparing a `Copy` enum per frame is how the owner sees
    /// a language change without building the key
    pub language: Language,
    pub compiled: Arc<Compiled>,
}

/// Parse and compile `bytes` for the language `cx` names (its labels, [`labels_in`]) and `measure`.
pub fn load(bytes: &[u8], app_version: &str, cx: &LocaleContext, measure: &dyn Measure) -> Result<LoadedHud, LoadError> {
    let language = cx.language();
    let parsed = parse::parse(bytes);
    let Some(package) = parsed.package else {
        return Err(LoadError::Invalid(parsed.findings.len()));
    };
    let key = CompileKey { sha256: plx_base::sha256::sha256(bytes), app_version: app_version.to_owned(), lang: language.tag().to_owned() };
    Ok(LoadedHud { key, language, compiled: Arc::new(compile(&package, measure, CompileCtx { labels: labels_in(cx) })) })
}

/// `builtin:classic`, the embedded package ([`crate::hud_format::CLASSIC_PACKAGE`]), compiled.
pub fn load_classic(app_version: &str, cx: &LocaleContext, measure: &dyn Measure) -> Result<LoadedHud, LoadError> {
    load(crate::hud_format::CLASSIC_PACKAGE.as_bytes(), app_version, cx, measure)
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
        assert_eq!(SHIPPED.len(), 10, "the claim is about the ten shipped languages");
        for language in SHIPPED {
            let cx = LocaleContext::resolve(language, None, None, None, None);
            let hud = load_classic("0.0.0-test", &cx, &ShippedMeasure)
                .unwrap_or_else(|e| panic!("classic did not load in {}: {e:?}", language.tag()));
            assert!(hud.compiled.findings().is_empty(), "{}: {:?}", language.tag(), hud.compiled.findings());
            assert!(!hud.compiled.states().is_empty(), "{}", language.tag());
            assert_eq!(hud.key.lang, hud.language.tag());
        }
    }

    /// The key is the bytes' digest: the same bytes in the same language are the same key, other bytes
    /// or another language are not, so the owner recompiles exactly when something changed.
    #[test]
    fn the_key_names_the_bytes_the_release_and_the_language() {
        let en = plx_platform::i18n::english();
        let a = load_classic("1.0", en, &ShippedMeasure).unwrap().key;
        assert_eq!(a, load_classic("1.0", en, &ShippedMeasure).unwrap().key);
        assert_ne!(a, load_classic("1.1", en, &ShippedMeasure).unwrap().key, "the release is part of the key");
        let other = SHIPPED.into_iter().find(|p| p.language() != Some(plx_platform::i18n::Language::En)).expect("a shipped language other than English");
        let de = LocaleContext::resolve(other, None, None, None, None);
        assert_ne!(a, load_classic("1.0", &de, &ShippedMeasure).unwrap().key, "the language is part of the key");
        assert_eq!(a.sha256, plx_base::sha256::sha256(crate::hud_format::CLASSIC_PACKAGE.as_bytes()));
    }

    /// A package that does not parse is refused, never half-loaded: the screen keeps the layout it has.
    #[test]
    fn a_package_that_does_not_parse_is_refused() {
        let err = load(b"{ not json", "1.0", plx_platform::i18n::english(), &ShippedMeasure);
        assert!(matches!(err, Err(LoadError::Invalid(n)) if n > 0));
    }
}
