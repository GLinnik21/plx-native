//! **Every Playback / Audio & Subtitles field row, and every picker level it opens onto, fits its
//! column in every shipped language.** Lives beside the page (not in `settings_text_fit_tests.rs`)
//! because `ci/check-deps.sh` forbids a screen from naming a sibling screen.
//!
//! The sweeps are a SUM over each field's own readouts and details, never a product of fields:
//! [`field_form`] builds the field list, [`field_options`] a picker level.

use super::*;
use crate::fontcov::advances::ShippedMeasure as M;
use crate::i18n::{language_on_this_thread_for_test, SHIPPED};
use crate::plex::account::AudioPreferences;
use crate::route::available_quality_ladder;
use crate::ui::table::{Section, TableView};
use crate::ui::machine::Measure;

fn frame_w() -> f32 {
    RouteLayout::screen().sectioned_table().w
}

fn playback(quality: Quality, direct_play: DirectPlayMode) -> FieldListInputs<'static> {
    FieldListInputs { kind: Kind::Playback, quality, direct_play, prefs: None, busy: false, show_retry: false }
}

fn audio(prefs: Option<&AudioPreferences>) -> FieldListInputs<'_> {
    FieldListInputs {
        kind: Kind::AudioSubtitles, quality: Quality::Original, direct_play: DirectPlayMode::Auto,
        prefs, busy: false, show_retry: false,
    }
}

fn table_of(section: Section) -> TableView {
    let mut table = TableView::new();
    table.compact = false;
    table.set_sections(vec![section], 0, false);
    table
}

/// Build `inputs` through the real [`field_form`] and collect the app-owned findings, tagged
/// with `tag`. A finding already reported for this UI language (the tag's first word) is not
/// repeated under a later tag: the rows a sweep does not vary repeat unchanged.
fn check_field_section(tag: &str, inputs: &FieldListInputs<'_>, out: &mut Vec<String>) {
    let mut form: FormTable<RowId, Action, SettingsPage> = FormTable::new(BAND);
    form.table.compact = false;
    form.set(field_form(inputs), None);
    let ui_language = tag.split_whitespace().next().unwrap_or(tag);
    for e in form.table.app_fit_failures(frame_w(), tag) {
        let finding = e.split_once(": ").map_or(e.as_str(), |(_, f)| f);
        if !out.iter().any(|o| o.starts_with(ui_language) && o.ends_with(finding)) {
            out.push(e);
        }
    }
}

/// Every value readout and detail line each field can show, through [`field_form`] directly.
#[test]
fn every_field_readout_and_detail_fits_its_column_in_every_language() {
    let _serial = crate::testlock::serial(); // Subtitle Size/Position read the route globals below
    let mut out = Vec::new();
    for language in SHIPPED {
        let _guard = language_on_this_thread_for_test(language);
        let tag = language.tag();

        for &quality in available_quality_ladder() {
            check_field_section(&format!("{tag} quality={quality:?}"), &playback(quality, DirectPlayMode::Auto), &mut out);
        }
        // Forced adds the "overridden" detail line to the Quality row.
        check_field_section(&format!("{tag} quality overridden detail"), &playback(Quality::Original, DirectPlayMode::Forced), &mut out);
        for direct_play in [DirectPlayMode::Auto, DirectPlayMode::Forced, DirectPlayMode::Disabled] {
            check_field_section(&format!("{tag} direct_play={direct_play:?}"), &playback(Quality::Original, direct_play), &mut out);
        }
        for size in crate::route::SubtitleSize::LADDER {
            crate::route::restore_subtitle_size(size);
            check_field_section(&format!("{tag} subtitle_size={size:?}"), &playback(Quality::Original, DirectPlayMode::Auto), &mut out);
        }
        crate::route::restore_subtitle_size(crate::route::SubtitleSize::Medium);
        for position in crate::route::SubtitlePosition::LADDER {
            crate::route::restore_subtitle_position(position);
            check_field_section(&format!("{tag} subtitle_position={position:?}"), &playback(Quality::Original, DirectPlayMode::Auto), &mut out);
        }
        crate::route::restore_subtitle_position(crate::route::SubtitlePosition::Low);

        check_field_section(&format!("{tag} retry row"), &FieldListInputs { show_retry: true, ..audio(None) }, &mut out);

        // Every catalog language as the Audio/Subtitle Language read-out (app data, so judged).
        let mut prefs = AudioPreferences::default();
        for l in crate::plex::languages::LANGUAGES {
            prefs.stated_language = Some(l.code.to_string());
            prefs.subtitle_language = Some(l.code.to_string());
            check_field_section(&format!("{tag} language={}", l.code), &audio(Some(&prefs)), &mut out);
        }
        prefs.auto_select_audio = Some(false);
        check_field_section(&format!("{tag} audio selection off detail"), &audio(Some(&prefs)), &mut out);

        let mut prefs = AudioPreferences::default();
        for mode in 0..3 {
            prefs.subtitle_mode = mode;
            check_field_section(&format!("{tag} subtitle_mode={mode}"), &audio(Some(&prefs)), &mut out);
        }
        let mut prefs = AudioPreferences::default();
        for forced in 0..4 {
            prefs.subtitle_forced = forced;
            check_field_section(&format!("{tag} subtitle_forced={forced}"), &audio(Some(&prefs)), &mut out);
        }
    }
    crate::ui::table::assert_no_fit_failures(&out);
}

/// Every picker level's rows, through `field_options`, including the full language
/// catalog once per shipped language. A picker row has the whole panel to itself, so this is a
/// separate check from the field row's trailing value.
#[test]
fn every_picker_level_fits_its_column_in_every_language() {
    let _serial = crate::testlock::serial();
    let mut out = Vec::new();
    for language in SHIPPED {
        let _guard = language_on_this_thread_for_test(language);
        let tag = language.tag();
        for kind in [Kind::Playback, Kind::AudioSubtitles] {
            let prefs = AudioPreferences::default();
            let fields: &[PickerKind] = match kind {
                Kind::Playback => &[PickerKind::Quality, PickerKind::DirectPlay, PickerKind::SubtitleSize, PickerKind::SubtitlePosition],
                Kind::AudioSubtitles => &[PickerKind::AudioLanguage, PickerKind::SubtitleMode, PickerKind::SubtitleLanguage, PickerKind::ForcedSubtitles],
            };
            for &field in fields {
                let current = resolve_value(field, Quality::Original, DirectPlayMode::Auto, Some(&prefs));
                let section = field_options(field, Quality::Original, DirectPlayMode::Auto, Some(&prefs)).into_iter()
                    .fold(Section::new(""), |section, (label, value)| section.row(Row::new(&label).checked(value == current)));
                out.extend(table_of(section).app_fit_failures(frame_w(), &format!("{tag} {field:?} picker")));
            }
        }
    }
    crate::ui::table::assert_no_fit_failures(&out);
}

/// The Direct Play row's trailing read-out (`direct_play_readout`, the function `field_form`
/// calls) fits beside its label without eliding either, in every language and mode. The picker
/// keeps the long strings; `ui::table::tests::row_columns_still_elides_an_unshortened_long_value`
/// covers a value nothing shortened.
#[test]
fn direct_play_readout_fits_beside_its_label_in_every_language() {
    let mut out = Vec::new();
    for language in SHIPPED {
        let _guard = language_on_this_thread_for_test(language);
        for mode in [DirectPlayMode::Auto, DirectPlayMode::Forced, DirectPlayMode::Disabled] {
            let value = direct_play_readout(mode);
            let table = table_of(
                Section::new("")
                    .row(Row::new(crate::i18n::msg::settings_playback_quality())
                        .value(crate::i18n::msg::settings_audio_not_set()).chevron(true))
                    .row(Row::new(crate::i18n::msg::settings_playback_direct_play()).value(value).chevron(true)),
            );
            out.extend(table.app_fit_failures(frame_w(), &format!("{} playback ({value})", language.tag())));
            let direct_play_row = &table.sections[0].rows[1];
            let cols = table.row_columns(direct_play_row, frame_w(), &M);
            let value_natural = M.width_str(value, theme::size::LABEL, true);
            let label_natural = M.width_str(crate::i18n::msg::settings_playback_direct_play(), theme::size::HEADLINE, true);
            assert!(cols.value_w + 1.0 >= value_natural,
                "{} {value:?}: an app-owned Direct Play value must never be elided ({} px column vs {value_natural} px natural)",
                language.tag(), cols.value_w);
            assert!(cols.label_w + 1.0 >= label_natural,
                "{} {value:?}: the Direct Play label must stay whole ({} px column vs {label_natural} px natural)",
                language.tag(), cols.label_w);
        }
    }
    crate::ui::table::assert_no_fit_failures(&out);
}

/// The Subtitles row's read-out for the foreign-audio mode names the SUBTITLE language (it once
/// read as "my language", which sounds like the audio row), and the other two modes keep the
/// picker's own label. Pinned in every shipped language; the fit sweep above covers the width.
#[test]
fn the_subtitle_mode_readouts_are_pinned_in_every_language() {
    for language in SHIPPED {
        let _guard = language_on_this_thread_for_test(language);
        let mut prefs = AudioPreferences::default();
        let readouts: Vec<String> = (0..3).map(|mode| {
            prefs.subtitle_mode = mode;
            field_readout(PickerKind::SubtitleMode, Quality::Original, DirectPlayMode::Auto, Some(&prefs))
        }).collect();
        assert_eq!(readouts[0], crate::i18n::msg::settings_audio_manual(), "{}", language.tag());
        assert_eq!(readouts[1], crate::i18n::msg::settings_audio_foreign_short(), "{}", language.tag());
        assert_eq!(readouts[2], crate::i18n::msg::settings_audio_always(), "{}", language.tag());
        let want = match language.tag() {
            "en" => "When audio isn't in the subtitle language",
            "es" => "Audio fuera del idioma de subtítulos",
            _ => "Калі гук не на мове субцітраў",
        };
        assert_eq!(readouts[1], want, "{}", language.tag());
    }
}
