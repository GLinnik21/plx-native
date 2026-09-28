//! **The Subtitles panel's row model** (plan `subtitle-menu-capsule` §3,
//! `/tmp/dsplayer/player.html:1104-1162`): which tracks are "yours", how they group by language,
//! in what order the sections and rows fall, and what each row MEANS — all pure over plain values
//! (the playing item's streams, the demuxer's own tags, "your languages"), so every rule here is
//! host-tested without a `PlaybackSession`, a store or a `TableView`.
//!
//! `ui::track_menu` turns a [`sub_sections`] answer into drawn `TableView` sections (labels,
//! badges, the checkmark, the Timing/Color read-outs) and reads a focused row back through
//! [`SubRow::target`]. The model deliberately carries no checked track, offset or tone: those are
//! read-outs, so changing one never re-groups the list.
//!
//! The shape: Off and every single-track "yours" language sit under one "Subtitles" header; a
//! "yours" language with several tracks gets its own section, ranked full < SDH < forced <
//! commentary; a headerless section holds Timing and Color; and everything else falls under
//! "Other languages", sorted by name. "Yours" is the pref language (if the play resolved under
//! one), the playing audio's language, and the current subtitle's own language, in that order
//! (`route::cur_sub_pref_lang`, gathered by `screens::player::overlay`).
use std::borrow::Cow;
use std::collections::HashMap;

use super::track_label::{self, Kind};
use super::Stream;

/// Image (bitmap) subtitle codecs — PGS/VobSub/DVD/DVB. The demuxer software-decodes these to
/// RGBA and the player composites them over the video, so they render on the direct-play path;
/// the menu tags the codec for clarity.
pub(crate) fn is_image_sub_codec(codec: &str) -> bool {
    matches!(
        codec.to_ascii_lowercase().as_str(),
        "pgs"
            | "hdmv_pgs_subtitle"
            | "vobsub"
            | "dvd_subtitle"
            | "dvdsub"
            | "dvb_subtitle"
            | "dvbsub"
    )
}

/// What a Subtitles-panel row IS, by POSITION — one entry per drawn row, in the exact order the
/// sections draw (`TableView::sel` is one flat index over all of them). Every reader of a focused
/// row matches on this rather than re-deriving which section a row fell in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RowTarget {
    Off,
    /// A track row — the index into the playing item's subs list ([`super::PlayingItem::subs`]).
    Sub(usize),
    Timing,
    Color,
}

/// The one badge a track row may show — never more than one (`player.html:954`'s priority:
/// FORCED > SDH > EXTERNAL > an image codec). Hashable because it is part of the "identical
/// tracks" key [`SubTrack::ordinal`] is counted over.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) enum RowBadge {
    Forced,
    Sdh,
    External,
    /// An image codec, upper-cased for display ("PGS").
    Codec(String),
}

/// One offered subtitle track, parsed once — the unit every section is built from.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct SubTrack {
    /// index into the playing item's subs list — what the row's [`RowTarget::Sub`] carries.
    pub(crate) i: usize,
    /// The display language (`Stream.lang`, or the catalog's "Unknown").
    pub(crate) lang: String,
    /// `SubLabel.source`, or the region fallback when that is empty (`track_label::region_detail`).
    pub(crate) detail: String,
    pub(crate) kind: Kind,
    pub(crate) badge: Option<RowBadge>,
    /// `Some(n)` when this track is otherwise IDENTICAL (same lang, detail, badge) to at least one
    /// other offered track — an ordinal among the identical ones only (`player.html:959-962,1110`).
    pub(crate) ordinal: Option<u32>,
}

/// A section's header, as a meaning rather than a string — the caller owns the catalog words.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum SubHeader {
    /// "Subtitles": Off, then every single-track "yours" language that precedes the first
    /// multi-track one.
    Subtitles,
    /// A multi-track "yours" language: its name and track count.
    Language { name: String, tracks: usize },
    /// No header: a single-track "yours" language after a multi-track section, or Timing + Color.
    Bare,
    /// "Other languages", with the number of DISTINCT languages under it.
    OtherLanguages { languages: usize },
}

/// One row of the model.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum SubRow {
    Off,
    /// A flat track row (label = its language): a single-track "yours" language, or any "Other
    /// languages" row.
    Flat(SubTrack),
    /// A row inside a multi-track "yours" language's own section (label = its source or kind).
    InLanguage(SubTrack),
    Timing,
    Color,
}

impl SubRow {
    pub(crate) fn target(&self) -> RowTarget {
        match self {
            SubRow::Off => RowTarget::Off,
            SubRow::Flat(t) | SubRow::InLanguage(t) => RowTarget::Sub(t.i),
            SubRow::Timing => RowTarget::Timing,
            SubRow::Color => RowTarget::Color,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct SubSection {
    pub(crate) header: SubHeader,
    pub(crate) rows: Vec<SubRow>,
}

/// What two tracks are grouped by: the canonical language ([`super::lang_key`], so "fre", "fra"
/// and "fr-CA" are one), or — for a track with no code at all — its display name, so "Unknown"
/// tracks still group with each other. A code that names no language (`lang_key` refuses it)
/// groups with nothing.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
enum LangGroup {
    Code(Cow<'static, str>),
    Name(String),
    Alone(usize),
}

/// Parse every offered track once, fill in [`SubTrack::ordinal`] for tracks that would otherwise
/// draw the exact same row, and pair each with its [`LangGroup`] and its code's canonical key.
fn sub_tracks(
    subs: &[Stream],
    offered: &[usize],
    names: &crate::player::TrackNames,
) -> Vec<(SubTrack, LangGroup, Option<Cow<'static, str>>)> {
    let mut out: Vec<(SubTrack, LangGroup, Option<Cow<'static, str>>)> = offered
        .iter()
        .filter_map(|&i| {
            let s = subs.get(i)?;
            let lang = if s.lang.trim().is_empty() {
                crate::i18n::msg::widgets_tracks_unknown().to_string()
            } else {
                s.lang.clone()
            };
            let container = names.sub(super::sub_render_ordinal(subs, i));
            let merged = track_label::track_name(&s.title, container, &lang);
            let label = track_label::parse(&merged, &lang, s.forced, s.sdh);
            let mut detail = label.source;
            if detail.is_empty() {
                if let Some(region) = track_label::region_detail(&s.language_tag) {
                    detail = region;
                }
            }
            // the panel's one-badge priority: FORCED > SDH > EXTERNAL > an image codec
            let badge = match label.kind {
                Kind::Forced => Some(RowBadge::Forced),
                Kind::Sdh => Some(RowBadge::Sdh),
                _ if s.external => Some(RowBadge::External),
                _ if is_image_sub_codec(&s.codec) => Some(RowBadge::Codec(s.codec.to_uppercase())),
                _ => None,
            };
            let code = s.lang_code.trim();
            let key = super::lang_key(code);
            let group = match (&key, code.is_empty()) {
                (Some(k), _) => LangGroup::Code(k.clone()),
                (None, true) => LangGroup::Name(lang.clone()),
                (None, false) => LangGroup::Alone(i),
            };
            let track = SubTrack { i, lang, detail, kind: label.kind, badge, ordinal: None };
            Some((track, group, key))
        })
        .collect();

    type Identity = (String, String, Option<RowBadge>);
    let identity = |t: &SubTrack| -> Identity { (t.lang.clone(), t.detail.clone(), t.badge.clone()) };
    let keys: Vec<Identity> = out.iter().map(|(t, ..)| identity(t)).collect();
    let mut totals: HashMap<&Identity, u32> = HashMap::new();
    for k in &keys {
        *totals.entry(k).or_insert(0) += 1;
    }
    let mut seen: HashMap<&Identity, u32> = HashMap::new();
    for ((t, ..), k) in out.iter_mut().zip(&keys) {
        if totals[k] > 1 {
            let n = seen.entry(k).or_insert(0);
            *n += 1;
            t.ordinal = Some(*n);
        }
    }
    out
}

/// **Group and order the Subtitles panel's rows.**
///
/// `subs` is the playing item's FULL subtitle list; `offered` is the subset this route offers
/// (sidecars only where they can be drawn or burned); `names` is the demuxer's own tag list;
/// `yours` is "your languages" in PREFERENCE order; `show_timing` is `!is_transcoding` (a
/// transcode burns captions server-side, so no client offset can reach them).
pub(crate) fn sub_sections(
    subs: &[Stream],
    offered: &[usize],
    names: &crate::player::TrackNames,
    yours: &[String],
    show_timing: bool,
) -> Vec<SubSection> {
    // Each "yours" entry's canonical language, once: a track is "yours" when its own key is one
    // of these, and a "yours" language ranks by the first position that names it.
    let yours: Vec<Cow<'static, str>> = yours.iter().filter_map(|y| super::lang_key(y)).collect();
    let yours_rank = |key: &Option<Cow<'static, str>>| key.as_ref().and_then(|k| yours.iter().position(|y| y == k));

    let mut mine: Vec<(SubTrack, LangGroup, usize)> = Vec::new();
    let mut other: Vec<(SubTrack, LangGroup)> = Vec::new();
    for (t, group, key) in sub_tracks(subs, offered, names) {
        match yours_rank(&key) {
            Some(rank) => mine.push((t, group, rank)),
            None => other.push((t, group)),
        }
    }
    mine.sort_by(|(a, ..), (b, ..)| a.kind.rank().cmp(&b.kind.rank()).then(a.i.cmp(&b.i)));
    other.sort_by_cached_key(|(t, _)| (t.lang.to_ascii_lowercase(), t.kind.rank(), t.i));

    // Bucket `mine` by language, first-seen order, then reorder the buckets by each one's position
    // in `yours` — the pref's language leads, then the playing audio's, then the current
    // subtitle's, exactly as `yours` states them.
    let mut index: HashMap<LangGroup, usize> = HashMap::new();
    let mut buckets: Vec<(usize, Vec<SubTrack>)> = Vec::new();
    for (t, group, rank) in mine {
        match index.get(&group) {
            Some(&b) => buckets[b].1.push(t),
            None => {
                index.insert(group, buckets.len());
                buckets.push((rank, vec![t]));
            }
        }
    }
    buckets.sort_by_key(|(rank, _)| *rank);

    // 1. "Subtitles": Off, then every single-track "yours" language, flat.
    let mut sections = vec![SubSection { header: SubHeader::Subtitles, rows: vec![SubRow::Off] }];

    // 2. Each multi-track "yours" language interrupts with its own section; a single-track
    // language folds back into "Subtitles" ONLY while that is still the last section built — once
    // a multi-track section has intervened, the next single-track language gets its own bare
    // (headerless) section instead, exactly mirroring `player.html`'s `sectionsFor`.
    for (_, bucket) in buckets {
        if bucket.len() > 1 {
            sections.push(SubSection {
                header: SubHeader::Language { name: bucket[0].lang.clone(), tracks: bucket.len() },
                rows: bucket.into_iter().map(SubRow::InLanguage).collect(),
            });
        } else {
            let row = SubRow::Flat(bucket.into_iter().next().expect("a bucket is never empty"));
            match sections.last_mut() {
                Some(last) if last.header == SubHeader::Subtitles => last.rows.push(row),
                _ => sections.push(SubSection { header: SubHeader::Bare, rows: vec![row] }),
            }
        }
    }

    // 3. A headerless section: Timing (omitted under transcode), then Color — always a NEW
    // section, never folded into whatever came before.
    let mut settings = Vec::with_capacity(2);
    if show_timing {
        settings.push(SubRow::Timing);
    }
    settings.push(SubRow::Color);
    sections.push(SubSection { header: SubHeader::Bare, rows: settings });

    // 4. "Other languages": flat rows, sorted by language name then rank, with the count of
    // DISTINCT languages (by the same grouping the buckets use — "fre" and "fra" are one).
    if !other.is_empty() {
        let languages = other.iter().map(|(_, g)| g).collect::<std::collections::HashSet<_>>().len();
        sections.push(SubSection {
            header: SubHeader::OtherLanguages { languages },
            rows: other.into_iter().map(|(t, _)| SubRow::Flat(t)).collect(),
        });
    }
    sections
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::player::TrackNames;

    fn stream(id: i64, index: i64, lang: &str, lang_code: &str, title: &str) -> Stream {
        Stream {
            id,
            index,
            lang: lang.into(),
            lang_code: lang_code.into(),
            codec: "srt".into(),
            title: title.into(),
            ..Default::default()
        }
    }

    fn yours(codes: &[&str]) -> Vec<String> {
        codes.iter().map(|c| c.to_string()).collect()
    }

    fn layout(subs: &[Stream], codes: &[&str]) -> Vec<SubSection> {
        let offered: Vec<usize> = (0..subs.len()).collect();
        sub_sections(subs, &offered, &TrackNames::new(), &yours(codes), true)
    }

    fn headers(sections: &[SubSection]) -> Vec<SubHeader> {
        sections.iter().map(|s| s.header.clone()).collect()
    }

    fn flat_langs(rows: &[SubRow]) -> Vec<&str> {
        rows.iter()
            .filter_map(|r| match r {
                SubRow::Flat(t) => Some(t.lang.as_str()),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn section_order_is_subtitles_then_multitrack_languages_then_settings_then_other() {
        let subs = vec![
            stream(1, 0, "English", "eng", ""),
            stream(2, 1, "Russian", "rus", "iTunes"),
            stream(3, 2, "Russian", "rus", "Netflix"), // multi-track "yours" → own section
            stream(4, 3, "French", "fre", ""),         // not "yours" → "Other languages"
        ];
        let russian = SubHeader::Language { name: "Russian".into(), tracks: 2 };
        let other = SubHeader::OtherLanguages { languages: 1 };

        // yours = [eng, rus]: English (single-track) is bucketed FIRST, so it folds into
        // "Subtitles" (still the untouched initial section) before Russian's own section
        // interrupts.
        let sections = layout(&subs, &["eng", "rus"]);
        assert_eq!(headers(&sections), [SubHeader::Subtitles, russian.clone(), SubHeader::Bare, other.clone()]);
        assert_eq!(flat_langs(&sections[0].rows), ["English"], "folded into Subtitles");

        // yours = [rus, eng]: the multi-track Russian bucket now outranks the single-track
        // English one, so Russian's section comes FIRST — once it has interrupted, English no
        // longer folds back into "Subtitles" and gets its own bare section instead
        // (`player.html`'s own `sectionsFor` rule: only the section BEFORE the first interruption
        // stays "Subtitles").
        let sections = layout(&subs, &["rus", "eng"]);
        assert_eq!(
            headers(&sections),
            [SubHeader::Subtitles, russian, SubHeader::Bare, SubHeader::Bare, other]
        );
        assert_eq!(flat_langs(&sections[2].rows), ["English"], "its own bare section, not Subtitles");
        assert_eq!(sections[3].rows, [SubRow::Timing, SubRow::Color], "the headerless Timing + Color section");
    }

    #[test]
    fn yours_flat_rows_follow_the_preference_order_pref_then_audio_then_current() {
        let subs = vec![
            stream(1, 0, "French", "fre", ""),
            stream(2, 1, "English", "eng", ""),
            stream(3, 2, "Russian", "rus", ""),
        ];
        // pref=rus, audio=eng, current=fre — every one a single track, so all three stay flat
        // under "Subtitles" and must read in THIS order, not file order.
        let sections = layout(&subs, &["rus", "eng", "fre"]);
        assert_eq!(sections[0].rows[0], SubRow::Off);
        assert_eq!(flat_langs(&sections[0].rows), ["Russian", "English", "French"]);
    }

    #[test]
    fn a_multitrack_yours_language_ranks_full_then_sdh_then_forced_then_commentary() {
        // file order deliberately scrambles the rank order: commentary, forced, full, sdh
        let subs = vec![
            stream(1, 0, "Russian", "rus", "Commentary"),
            stream(2, 1, "Russian", "rus", "Форс."),
            stream(3, 2, "Russian", "rus", ""),
            stream(4, 3, "Russian", "rus", "SDH"),
        ];
        let sections = layout(&subs, &["rus"]);
        assert_eq!(sections[1].header, SubHeader::Language { name: "Russian".into(), tracks: 4 });
        let kinds: Vec<Kind> = sections[1]
            .rows
            .iter()
            .map(|r| match r {
                SubRow::InLanguage(t) => t.kind,
                other => panic!("not a language row: {other:?}"),
            })
            .collect();
        assert_eq!(kinds, [Kind::Full, Kind::Sdh, Kind::Forced, Kind::Commentary]);
    }

    /// **One language, one group, whatever ISO 639-2 spelling each track carries** — "fre" (the
    /// B code) and "fra" (the T code) are both French, so they bucket together under one "French"
    /// section and count as one language under "Other languages".
    #[test]
    fn bibliographic_and_terminology_codes_of_one_language_group_together() {
        let subs = vec![
            stream(1, 0, "French", "fre", "iTunes"),
            stream(2, 1, "French", "fra", "Netflix"),
        ];
        let sections = layout(&subs, &["fra"]);
        assert_eq!(
            headers(&sections),
            [SubHeader::Subtitles, SubHeader::Language { name: "French".into(), tracks: 2 }, SubHeader::Bare],
            "one French section, not two flat rows"
        );
        let sections = layout(&subs, &[]);
        assert_eq!(sections.last().unwrap().header, SubHeader::OtherLanguages { languages: 1 });
    }

    /// Tracks with no language code group by their display name, so two "Unknown" tracks count as
    /// one language — and never land in "yours", whatever `yours` holds.
    #[test]
    fn codeless_tracks_group_by_name_and_are_never_yours() {
        let subs = vec![stream(1, 0, "", "", ""), stream(2, 1, "", "", "")];
        let sections = layout(&subs, &["eng", ""]);
        assert_eq!(sections.last().unwrap().header, SubHeader::OtherLanguages { languages: 1 });
    }

    #[test]
    fn row_targets_follow_the_rows_in_drawn_order() {
        let subs = vec![stream(1, 0, "English", "eng", "")];
        let targets: Vec<RowTarget> =
            layout(&subs, &[]).iter().flat_map(|s| &s.rows).map(SubRow::target).collect();
        assert_eq!(targets, [RowTarget::Off, RowTarget::Timing, RowTarget::Color, RowTarget::Sub(0)]);
    }
}
