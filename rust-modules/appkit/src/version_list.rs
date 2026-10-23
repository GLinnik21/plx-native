//! **The Play Version list**: one row per version (`Media[]` entry) of a movie or episode, drawn
//! on two surfaces — the item menu's Play Version page (`screens::item_menu`) and the panel the
//! Detail hero's Play Version disc opens (`screens::versions_panel`). One builder, so the two can
//! never word one version two ways.
//!
//! Pure over its inputs ([`MediaVersion`]s and the caller's row identity and action), so the
//! labels and the fit are host-testable without a menu or a page.
use plx_data::pms::MediaVersion;
use plx_ui::form::{FormId, FormSection, RowKind};
use plx_ui::table::Row;

/// The list's heading: "Play Version".
pub fn title() -> &'static str {
    plx_platform::i18n::msg::browse_menu_play_version_title()
}

/// **The list as one section**, in the server's order: row `n` is `versions[n]`, identified by
/// `id(n)` and reporting `act(&versions[n])`. Every row is a button that plays.
pub fn section<Id: FormId, A, D>(
    versions: &[MediaVersion],
    id: impl Fn(u16) -> Id,
    act: impl Fn(&MediaVersion) -> A,
) -> FormSection<Id, A, D> {
    let mut sec = FormSection::new(title());
    for (n, (v, (label, detail))) in versions.iter().zip(labels(versions)).enumerate() {
        let mut row = Row::new(label);
        if !detail.is_empty() {
            row = row.detail(detail);
        }
        sec = sec.item(id(n as u16), RowKind::Button, act(v), row);
    }
    sec
}

/// What each version's row says: `"25.2 Mbps · 4K"`, over the facts that DIFFER between the
/// versions (video codec and HDR, audio, container, size), so two rows never read alike for a
/// reason the list does not show and a fact every version shares is not repeated. Pure.
pub fn labels(versions: &[MediaVersion]) -> Vec<(String, String)> {
    let video = |v: &MediaVersion| {
        let codec = crate::info_panel::video_codec_name(&v.vcodec);
        if v.hdr { format!("{codec} HDR") } else { codec }
    };
    let audio = |v: &MediaVersion| {
        let codec = plx_data::metadata::friendly_codec(&v.acodec);
        let channels = match v.channels {
            1 => plx_platform::i18n::msg::widgets_tracks_mono().to_string(),
            2 => plx_platform::i18n::msg::widgets_tracks_stereo().to_string(),
            6 => "5.1".to_string(),
            8 => "7.1".to_string(),
            _ => String::new(),
        };
        join_present(&[&codec, &channels], " ")
    };
    let container = |v: &MediaVersion| v.container.to_ascii_uppercase();
    let size = |v: &MediaVersion| if v.size > 0 { plx_ui::fmt::bytes(v.size, (1, 0, 0)) } else { String::new() };
    let facts: [&dyn Fn(&MediaVersion) -> String; 4] = [&video, &audio, &container, &size];
    let differs: Vec<bool> = facts
        .iter()
        .map(|f| versions.iter().map(|v| f(v)).collect::<std::collections::HashSet<_>>().len() > 1)
        .collect();
    versions
        .iter()
        .map(|v| {
            let rate = if v.kbps > 0 { plx_ui::fmt::bitrate(v.kbps) } else { String::new() };
            let res = plx_ui::fmt::resolution(&v.resolution, v.width, v.height).unwrap_or_default();
            let label = join_present(&[&rate, &res], " \u{b7} ");
            let shown: Vec<String> = facts
                .iter()
                .zip(&differs)
                .filter(|(_, d)| **d)
                .map(|(f, _)| f(v))
                .collect();
            let shown: Vec<&str> = shown.iter().map(String::as_str).collect();
            (label, join_present(&shown, " \u{b7} "))
        })
        .collect()
}

fn join_present(parts: &[&str], sep: &str) -> String {
    parts.iter().filter(|p| !p.is_empty()).copied().collect::<Vec<_>>().join(sep)
}

/// A pair of versions that differ in video and size and share audio and container: the fixture
/// both surfaces' tests read, so a row the menu fits and the panel fits are the same row.
#[cfg(any(test, feature = "test-support"))]
pub fn sample() -> Vec<MediaVersion> {
    let common = MediaVersion { acodec: "eac3".into(), channels: 6, container: "mkv".into(), ..Default::default() };
    vec![
        MediaVersion {
            id: plx_plex::plex::MediaId(3082), kbps: 25_200, resolution: "4k".into(), vcodec: "hevc".into(),
            size: 30 << 30, hdr: true, ..common.clone()
        },
        MediaVersion {
            id: plx_plex::plex::MediaId(3086), kbps: 10_900, resolution: "1080".into(), vcodec: "h264".into(),
            size: 8 << 30, ..common
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use plx_ui::form::{Form, FormTable, RowKey};
    use std::convert::Infallible;

    #[derive(Clone, Copy, PartialEq, Eq, Debug)]
    struct Pos(u16);
    impl FormId for Pos {
        fn key(&self) -> RowKey {
            RowKey(u32::from(self.0))
        }
    }

    /// The label is rate and resolution; the sub-line names only what tells the versions apart
    /// (here the video codec and HDR, and the size), never a fact they share (audio, container).
    #[test]
    fn a_version_row_says_what_differs() {
        let _g = plx_platform::i18n::language_on_this_thread_for_test(plx_platform::i18n::Preference::En);
        let rows = labels(&sample());
        assert_eq!(rows[0].0, "25.2 Mbps \u{b7} 4K");
        assert_eq!(rows[1].0, "10.9 Mbps \u{b7} 1080p");
        assert_eq!(rows[0].1, format!("HEVC HDR \u{b7} {}", plx_ui::fmt::bytes(30 << 30, (1, 0, 0))));
        assert_eq!(rows[1].1, format!("H.264 \u{b7} {}", plx_ui::fmt::bytes(8 << 30, (1, 0, 0))));
        let same = vec![sample()[0].clone(), sample()[0].clone()];
        assert!(labels(&same).iter().all(|(_, detail)| detail.is_empty()), "nothing differs, nothing to say");
    }

    /// The list fits a menu panel in every shipped language, measured with the device's advances.
    #[test]
    fn the_list_fits_in_every_language() {
        use plx_platform::i18n::{language_on_this_thread_for_test, SHIPPED};
        let measure = plx_base::fontcov::advances::ShippedMeasure;
        let mut out = Vec::new();
        for language in SHIPPED {
            let _guard = language_on_this_thread_for_test(language);
            let mut form: FormTable<Pos, plx_plex::plex::MediaId, Infallible> = FormTable::new(plx_ui::table_screen::BAND_BASE);
            form.set(Form::new().section(section(&sample(), Pos, |v| v.id)), None);
            form.table.compact = false;
            let tag = format!("{} versions", language.tag());
            out.extend(form.table.menu_cap_failure(&measure, &tag));
            out.extend(form.table.app_fit_failures(plx_ui::table::MENU_MAX_W, &tag));
            out.extend(form.table.app_fit_failures_hugged(&tag));
        }
        plx_ui::table::assert_no_fit_failures(&out);
    }
}
