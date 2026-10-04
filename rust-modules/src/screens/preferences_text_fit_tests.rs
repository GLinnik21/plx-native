//! **Every Playback / Audio & Subtitles field row, and every picker level it opens onto, fits its
//! column in every shipped language.** Lives beside the page (not in `settings_text_fit_tests.rs`)
//! because `ci/check-deps.sh` forbids a screen from naming a sibling screen.
//!
//! The sweeps are a SUM over each field's own readouts and details, never a product of fields:
//! [`field_form`] builds the field list, [`field_options`] a picker level.

use super::*;
use plx_base::fontcov::advances::ShippedMeasure as M;
use plx_platform::i18n::{language_on_this_thread_for_test, SHIPPED};
use crate::plex::account::AudioPreferences;
use crate::route::available_quality_ladder;
use plx_ui::table::{Section, TableView};
use plx_machine::machine::Measure;

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
    let _serial = plx_base::testlock::serial(); // Subtitle Size/Position read the route globals below
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
        for mode in crate::route::NextEpisodeMode::LADDER {
            crate::route::restore_next_episode_mode(mode);
            check_field_section(&format!("{tag} next_episode={mode:?}"), &playback(Quality::Original, DirectPlayMode::Auto), &mut out);
        }
        crate::route::restore_next_episode_mode(crate::route::NextEpisodeMode::Countdown);
        for interval in crate::route::SkipInterval::LADDER {
            crate::route::restore_skip_interval(interval);
            check_field_section(&format!("{tag} skip_interval={interval:?}"), &playback(Quality::Original, DirectPlayMode::Auto), &mut out);
        }
        crate::route::restore_skip_interval(crate::route::SkipInterval::Seconds10);

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
    plx_ui::table::assert_no_fit_failures(&out);
}

/// Every picker level's rows, through `field_options`, including the full language
/// catalog once per shipped language. A picker row has the whole panel to itself, so this is a
/// separate check from the field row's trailing value.
#[test]
fn every_picker_level_fits_its_column_in_every_language() {
    let _serial = plx_base::testlock::serial();
    let mut out = Vec::new();
    for language in SHIPPED {
        let _guard = language_on_this_thread_for_test(language);
        let tag = language.tag();
        for kind in [Kind::Playback, Kind::AudioSubtitles] {
            let prefs = AudioPreferences::default();
            let fields: &[PickerKind] = match kind {
                Kind::Playback => &[PickerKind::Quality, PickerKind::DirectPlay, PickerKind::SubtitleSize, PickerKind::SubtitlePosition, PickerKind::NextEpisode, PickerKind::SkipInterval],
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
    plx_ui::table::assert_no_fit_failures(&out);
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
                    .row(Row::new(plx_platform::i18n::msg::settings_playback_quality())
                        .value(plx_platform::i18n::msg::settings_audio_not_set()).chevron(true))
                    .row(Row::new(plx_platform::i18n::msg::settings_playback_direct_play()).value(value).chevron(true)),
            );
            out.extend(table.app_fit_failures(frame_w(), &format!("{} playback ({value})", language.tag())));
            let direct_play_row = &table.sections[0].rows[1];
            let cols = table.row_columns(direct_play_row, frame_w(), &M);
            let value_natural = M.width_str(value, theme::size::LABEL, true);
            let label_natural = M.width_str(plx_platform::i18n::msg::settings_playback_direct_play(), theme::size::HEADLINE, true);
            assert!(cols.value_w + 1.0 >= value_natural,
                "{} {value:?}: an app-owned Direct Play value must never be elided ({} px column vs {value_natural} px natural)",
                language.tag(), cols.value_w);
            assert!(cols.label_w + 1.0 >= label_natural,
                "{} {value:?}: the Direct Play label must stay whole ({} px column vs {label_natural} px natural)",
                language.tag(), cols.label_w);
        }
    }
    plx_ui::table::assert_no_fit_failures(&out);
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
        assert_eq!(readouts[0], plx_platform::i18n::msg::settings_audio_manual(), "{}", language.tag());
        assert_eq!(readouts[1], plx_platform::i18n::msg::settings_audio_foreign_short(), "{}", language.tag());
        assert_eq!(readouts[2], plx_platform::i18n::msg::settings_audio_always(), "{}", language.tag());
        let want = match language.tag() {
            "en" => "When audio isn't in the subtitle language",
            "es" => "Audio fuera del idioma de subtítulos",
            _ => "Калі гук не на мове субцітраў",
        };
        assert_eq!(readouts[1], want, "{}", language.tag());
    }
}

/// The copy as `RouteLayout::draw_narrative` draws it under the title.
fn copy_view<'a>(copy: &'a str) -> plx_ui::text_view::TextView<'a> {
    let size = theme::size::LABEL;
    plx_ui::text_view::TextView::new(copy, size, theme::TEXT_READING)
        .leading(size as f32 + theme::space::XS).max_lines(12).with_measure(&M)
}

/// Every explanation a field list or a picker can show under its title fits the left column
/// COMPLETE in every shipped language: never cut at the line cap, never taller than the room
/// between the title and the action band, and short enough to read as a note (at most
/// [`MAX_COPY_LINES`] lines). A picker's own explanation, the Force note that replaces it while
/// Force Direct Play is on, and a status line with the Force note appended are all covered.
#[test]
fn every_explanation_under_a_title_fits_the_column_in_every_language() {
    const MAX_COPY_LINES: usize = 5;
    let _serial = plx_base::testlock::serial();
    let layout = RouteLayout::screen();
    let all_fields = [PickerKind::Quality, PickerKind::DirectPlay, PickerKind::SubtitleSize, PickerKind::SubtitlePosition,
        PickerKind::NextEpisode, PickerKind::SkipInterval, PickerKind::AudioLanguage, PickerKind::SubtitleMode,
        PickerKind::SubtitleLanguage, PickerKind::ForcedSubtitles];
    let mut out = Vec::new();
    for language in SHIPPED {
        let _guard = language_on_this_thread_for_test(language);
        let tag = language.tag();
        let mut subjects = vec![Subject::Page(Kind::Playback), Subject::Page(Kind::AudioSubtitles)];
        subjects.extend(all_fields.map(Subject::Picker));
        for subject in subjects {
            let title = match subject {
                Subject::Page(kind) => kind.title(),
                Subject::Picker(field) => field.title(),
            };
            for direct_play in [DirectPlayMode::Auto, DirectPlayMode::Forced] {
                for status in ["", plx_platform::i18n::msg::settings_playback_save_failed()] {
                    let copy = copy_text(subject, status, direct_play);
                    let view = copy_view(&copy);
                    let top = layout.narrative_copy_frame(true, title, layout.action.y, &M).y;
                    let room = layout.action.y - theme::space::XL - top;
                    let (lines, h) = (view.line_count(layout.narrative.w), view.measure_h(layout.narrative.w));
                    let at = format!("{tag} {subject:?} direct_play={direct_play:?} status={status:?}");
                    if view.truncates(layout.narrative.w) { out.push(format!("{at}: the copy is cut at the line cap")); }
                    if h > room { out.push(format!("{at}: the copy is {h}px tall, the column has {room}px")); }
                    // A status line is the failure text, not an explanation: only the explanation is held to the note length.
                    if status.is_empty() && lines > MAX_COPY_LINES { out.push(format!("{at}: {lines} lines, more than {MAX_COPY_LINES}")); }
                }
            }
        }
    }
    assert!(out.is_empty(), "{}", out.join("\n"));
}

/// Each local picker explains ITS setting: no two of the six read the same, none is the page's
/// blurb, and only the Default quality picker carries the More-menu sentence (the page blurb
/// names no single setting). Pinned per language so a translation cannot drift back to the page
/// text or copy a neighbour.
#[test]
fn each_local_picker_has_its_own_explanation_in_every_language() {
    let _serial = plx_base::testlock::serial();
    let fields = [PickerKind::Quality, PickerKind::DirectPlay, PickerKind::SubtitleSize, PickerKind::SubtitlePosition,
        PickerKind::NextEpisode, PickerKind::SkipInterval];
    for language in SHIPPED {
        let _guard = language_on_this_thread_for_test(language);
        let tag = language.tag();
        let page = copy_text(Subject::Page(Kind::Playback), "", DirectPlayMode::Auto).into_owned();
        let copies: Vec<String> = fields.iter().map(|&f| copy_text(Subject::Picker(f), "", DirectPlayMode::Auto).into_owned()).collect();
        for (i, copy) in copies.iter().enumerate() {
            assert!(!copy.is_empty(), "{tag} {:?}", fields[i]);
            assert_ne!(*copy, page, "{tag} {:?} must not repeat the page blurb", fields[i]);
            assert!(copies.iter().enumerate().all(|(j, other)| i == j || other != copy), "{tag} {:?} reads like a sibling", fields[i]);
        }
        assert_eq!(copies[0], plx_platform::i18n::msg::settings_playback_quality_copy(), "{tag}");
        // Force Direct Play overrides only what it changes: the page list, Quality and Direct Play.
        let forced = |subject| copy_text(subject, "", DirectPlayMode::Forced).into_owned();
        for subject in [Subject::Page(Kind::Playback), Subject::Picker(PickerKind::Quality), Subject::Picker(PickerKind::DirectPlay)] {
            assert_eq!(forced(subject), plx_platform::i18n::msg::settings_playback_force_note(), "{tag} {subject:?}");
        }
        for (i, &field) in fields.iter().enumerate().skip(2) {
            assert_eq!(forced(Subject::Picker(field)), copies[i], "{tag} {field:?} is untouched by Force");
        }
        // The account pickers keep the account note under every mode.
        for field in [PickerKind::AudioLanguage, PickerKind::SubtitleMode, PickerKind::SubtitleLanguage, PickerKind::ForcedSubtitles] {
            assert_eq!(copy_text(Subject::Picker(field), "", DirectPlayMode::Forced), plx_platform::i18n::msg::settings_audio_account_note(), "{tag} {field:?}");
        }
        // A status wins, with the Force note appended only where Force matters.
        let status = plx_platform::i18n::msg::settings_playback_save_failed();
        assert_eq!(copy_text(Subject::Picker(PickerKind::SkipInterval), status, DirectPlayMode::Forced), status, "{tag}");
        assert_eq!(copy_text(Subject::Picker(PickerKind::DirectPlay), status, DirectPlayMode::Forced),
            format!("{status}\n\n{}", plx_platform::i18n::msg::settings_playback_force_note()), "{tag}");
        assert_eq!(copy_text(Subject::Picker(PickerKind::DirectPlay), status, DirectPlayMode::Auto), status, "{tag}");
    }
}
