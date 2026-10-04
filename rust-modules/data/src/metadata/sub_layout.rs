//! **The Subtitles panel's row model** (plan `subtitle-menu-capsule` §3,
//! `/tmp/dsplayer/player.html:1104-1162`): which tracks are "yours", how they group by language,
//! in what order the sections and rows fall, and what each row MEANS — all pure over plain values
//! (the playing item's streams, the demuxer's own tags, "your languages"), so every rule here is
//! host-tested without a `PlaybackSession`, a store or a `TableView`.
//!
//! `appkit::track_menu` turns a [`sub_sections`] answer into a keyed form (labels, badges, the
//! checkmark, the Timing and Style read-outs) and reads a focused row back through
//! [`SubRow::target`]. The model deliberately carries no checked track, offset or tone: those are
//! read-outs, so changing one never re-groups the list.
//!
//! The shape: Off and every single-track "yours" language sit under one "Subtitles" header; a
//! "yours" language with several tracks gets its own section, ranked full < SDH < forced <
//! commentary; everything else is "Other languages", ONE row on the root (a drill-in; the page behind it is
//! [`SubModel::other`], one [`OtherLang`] per language, sorted by name); and a headerless section holds Timing and Style (both omitted
//! under transcode). "Yours" is the pref language (if the play resolved under
//! one), the playing audio's language, and the current subtitle's own language, in that order
//! (`route::cur_sub_pref_lang`, gathered by `screens::player::overlay`).
use std::borrow::Cow;
use std::collections::HashMap;

use super::track_label::{self, Kind};
use super::Stream;

/// Image (bitmap) subtitle codecs — PGS/VobSub/DVD/DVB. The demuxer software-decodes these to
/// RGBA and the player composites them over the video, so they render on the direct-play path;
/// the menu tags the codec for clarity.
pub fn is_image_sub_codec(codec: &str) -> bool {
    image_codec_badge(codec).is_some()
}

/// What a Subtitles-panel row IS — one per drawn row. `appkit::track_menu` declares each row under it
/// (as its `TrackRow` identity), and every reader of a focused row matches on that rather than
/// re-deriving which section a row fell in. (The footnote naming why Timing/Style are dim is an
/// inert slot there, not a row with a target.)
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RowTarget {
    Off,
    /// A track row — the index into the playing item's subs list ([`super::PlayingItem::subs`]).
    Sub(usize),
    Timing,
    Style,
    /// The drill-in to the Other languages page.
    Other,
}

/// The short name an image subtitle's badge shows for its codec: Plex reports the demuxer's own
/// strings ("hdmv_pgs_subtitle", "dvd_subtitle", "dvb_subtitle"), far too long for a chip.
/// `None` for a text codec, which shows no format.
pub fn image_codec_badge(codec: &str) -> Option<&'static str> {
    match codec.to_ascii_lowercase().as_str() {
        "pgs" | "hdmv_pgs_subtitle" => Some("PGS"),
        "vobsub" | "dvd_subtitle" | "dvdsub" => Some("VOBSUB"),
        "dvb_subtitle" | "dvbsub" => Some("DVB"),
        _ => None,
    }
}

/// A badge a track row may show. At most ONE of Forced / SDH / External (`player.html:954`'s
/// priority: FORCED > SDH > EXTERNAL), and — owner, 2026-10-01 — an IMAGE subtitle always also
/// carries its codec ("PGS"), so the format reads on an SDH or forced bitmap track too; a text
/// subtitle shows no format. Hashable because it is part of the "identical tracks" key
/// [`SubTrack::ordinal`] is counted over.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum RowBadge {
    Forced,
    Sdh,
    External,
    /// An image codec's short display name ([`image_codec_badge`]: "PGS", "VOBSUB", "DVB"). Always
    /// last.
    Codec(&'static str),
}

/// One offered subtitle track, parsed once — the unit every section is built from.
#[derive(Clone, Debug, PartialEq)]
pub struct SubTrack {
    /// index into the playing item's subs list — what the row's [`RowTarget::Sub`] carries.
    pub i: usize,
    /// The display language (`Stream.lang`, or the catalog's "Unknown").
    pub lang: String,
    /// `SubLabel.source`, or the region fallback when that is empty (`track_label::region_detail`).
    pub detail: String,
    pub kind: Kind,
    pub badges: Vec<RowBadge>,
    /// `Some(n)` when this track is otherwise IDENTICAL (same lang, detail, badge) to at least one
    /// other offered track — an ordinal among the identical ones only (`player.html:959-962,1110`).
    pub ordinal: Option<u32>,
}

/// A section's header, as a meaning rather than a string — the caller owns the catalog words.
#[derive(Clone, Debug, PartialEq)]
pub enum SubHeader {
    /// "Subtitles": Off, then every single-track "yours" language that precedes the first
    /// multi-track one.
    Subtitles,
    /// A multi-track "yours" language: its name and track count.
    Language { name: String, tracks: usize },
    /// No header: a single-track "yours" language after a multi-track section, the Other languages
    /// drill-in, or Timing + Style.
    Bare,
}

/// One row of the model.
#[derive(Clone, Debug, PartialEq)]
pub enum SubRow {
    Off,
    /// A flat track row (label = its language): a single-track "yours" language.
    Flat(SubTrack),
    /// A row inside a multi-track "yours" language's own section (label = its source or kind).
    InLanguage(SubTrack),
    /// The drill-in to the Other languages page: the number of DISTINCT languages behind it (by
    /// the grouping the buckets use — "fre" and "fra" are one).
    OtherLanguages { languages: usize },
    Timing,
    /// The drill-in to the caption Style pages (Size, Position, Color).
    Style,
}

impl SubRow {
    pub fn target(&self) -> RowTarget {
        match self {
            SubRow::Off => RowTarget::Off,
            SubRow::Flat(t) | SubRow::InLanguage(t) => RowTarget::Sub(t.i),
            SubRow::Timing => RowTarget::Timing,
            SubRow::Style => RowTarget::Style,
            SubRow::OtherLanguages { .. } => RowTarget::Other,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct SubSection {
    pub header: SubHeader,
    pub rows: Vec<SubRow>,
}

/// **A language page's identity**: the Plex stream id of its language's first track in the item's
/// FULL subtitle list — a value that names the language however the list's positions shift when a
/// track leaves — plus the page's [`Self::slot`], its ordinal among the languages currently listed,
/// which only decides the row's focus key (`TrackRow::key`). Equality and hashing are the stream
/// alone: a slot that moved because another language came or went is still the same language.
#[derive(Clone, Copy, Debug)]
pub struct LangId {
    pub stream: i64,
    pub slot: usize,
}

impl PartialEq for LangId {
    fn eq(&self, other: &Self) -> bool {
        self.stream == other.stream
    }
}
impl Eq for LangId {}

/// One language on the Other languages page: its tracks, ranked, and the identity its drill-in
/// carries.
#[derive(Clone, Debug, PartialEq)]
pub struct OtherLang {
    /// The page identity: the stream id of this language's first track in the playing item's FULL
    /// subtitle list (offered or not, so a sidecar becoming offered does not move it), and its
    /// place on the page. See [`LangId`].
    pub id: LangId,
    /// The display language.
    pub name: String,
    /// Its offered tracks, full < SDH < forced < commentary, then list order. A single track is a
    /// direct pick row on the page; several are a drill-in.
    pub tracks: Vec<SubTrack>,
}

/// The whole Subtitles model: the root's sections and the Other languages page behind its row.
#[derive(Clone, Debug, PartialEq)]
pub struct SubModel {
    pub sections: Vec<SubSection>,
    /// A-Z by language name; empty when every offered track is "yours".
    pub other: Vec<OtherLang>,
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

/// A track's [`LangGroup`] and its code's canonical key.
fn group_of(s: &Stream, i: usize, lang: &str) -> (LangGroup, Option<Cow<'static, str>>) {
    let code = s.lang_code.trim();
    let key = super::lang_key(code);
    let group = match (&key, code.is_empty()) {
        (Some(k), _) => LangGroup::Code(k.clone()),
        (None, true) => LangGroup::Name(lang.to_string()),
        (None, false) => LangGroup::Alone(i),
    };
    (group, key)
}

/// The language a track row is grouped and drawn under: its own name, or "Unknown" when the
/// stream carries none. One rule for [`group_firsts`] and [`sub_tracks`], so a codeless track
/// groups by the same name it shows.
fn display_lang(s: &Stream) -> String {
    if s.lang.trim().is_empty() {
        plx_platform::i18n::msg::widgets_tracks_unknown().to_string()
    } else {
        s.lang.clone()
    }
}

/// Each group's FIRST track's stream id over the item's full list — offered or not, so a
/// language's identity never moves when a sidecar becomes offered or leaves, and never follows a
/// list position.
fn group_firsts(subs: &[Stream]) -> HashMap<LangGroup, i64> {
    let mut firsts = HashMap::new();
    for (i, s) in subs.iter().enumerate() {
        firsts.entry(group_of(s, i, &display_lang(s)).0).or_insert(s.id);
    }
    firsts
}

/// Parse every offered track once, fill in [`SubTrack::ordinal`] for tracks that would otherwise
/// draw the exact same row, and pair each with its [`LangGroup`] and its code's canonical key.
fn sub_tracks(
    subs: &[Stream],
    offered: &[usize],
    names: &super::track_names::TrackNames,
) -> Vec<(SubTrack, LangGroup, Option<Cow<'static, str>>)> {
    let mut out: Vec<(SubTrack, LangGroup, Option<Cow<'static, str>>)> = offered
        .iter()
        .filter_map(|&i| {
            let s = subs.get(i)?;
            let lang = display_lang(s);
            let container = names.sub(super::sub_render_ordinal(subs, i));
            let merged = track_label::track_name(&s.title, container, &lang);
            let label = track_label::parse(&merged, &lang, s.forced, s.sdh);
            let mut detail = label.source;
            if detail.is_empty() {
                if let Some(region) = track_label::region_detail(&s.language_tag) {
                    detail = region;
                }
            }
            // the panel's priority: one of FORCED > SDH > EXTERNAL, then the image codec if any
            let mut badges = match label.kind {
                Kind::Forced => vec![RowBadge::Forced],
                Kind::Sdh => vec![RowBadge::Sdh],
                _ if s.external => vec![RowBadge::External],
                _ => Vec::new(),
            };
            if let Some(name) = image_codec_badge(&s.codec) {
                badges.push(RowBadge::Codec(name));
            }
            let (group, key) = group_of(s, i, &lang);
            let track = SubTrack { i, lang, detail, kind: label.kind, badges, ordinal: None };
            Some((track, group, key))
        })
        .collect();

    type Identity = (String, String, Vec<RowBadge>);
    let identity = |t: &SubTrack| -> Identity { (t.lang.clone(), t.detail.clone(), t.badges.clone()) };
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
/// transcode burns captions server-side, so no client offset or style can reach them: Timing and
/// Style are both omitted).
pub fn sub_sections(
    subs: &[Stream],
    offered: &[usize],
    names: &super::track_names::TrackNames,
    yours: &[String],
    show_timing: bool,
) -> SubModel {
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

    // 3. "Other languages": ONE drill-in row in its own headerless section — the page behind it is
    // `other`, a row per distinct language (by the grouping the buckets use — "fre" and "fra" are
    // one), sorted by name.
    let firsts = group_firsts(subs);
    let mut other_langs: Vec<OtherLang> = Vec::new();
    let mut slot: HashMap<LangGroup, usize> = HashMap::new();
    for (t, group) in other {
        match slot.get(&group) {
            Some(&n) => other_langs[n].tracks.push(t),
            None => {
                slot.insert(group.clone(), other_langs.len());
                let stream = firsts.get(&group).copied().unwrap_or_else(|| subs.get(t.i).map_or(0, |s| s.id));
                let id = LangId { stream, slot: other_langs.len() };
                other_langs.push(OtherLang { id, name: t.lang.clone(), tracks: vec![t] });
            }
        }
    }
    if !other_langs.is_empty() {
        sections.push(SubSection {
            header: SubHeader::Bare,
            rows: vec![SubRow::OtherLanguages { languages: other_langs.len() }],
        });
    }

    // 4. A headerless section: Timing then Style — always a NEW section, never folded into
    // whatever came before. Both follow the same availability: a transcode burns captions in
    // server-side, so there is no client caption to offset or to style.
    if show_timing {
        sections.push(SubSection { header: SubHeader::Bare, rows: vec![SubRow::Timing, SubRow::Style] });
    }

    SubModel { sections, other: other_langs }
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::track_names::TrackNames;

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

    fn model(subs: &[Stream], codes: &[&str]) -> SubModel {
        let offered: Vec<usize> = (0..subs.len()).collect();
        sub_sections(subs, &offered, &TrackNames::new(), &yours(codes), true)
    }

    fn layout(subs: &[Stream], codes: &[&str]) -> Vec<SubSection> {
        model(subs, codes).sections
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
        let other = SubHeader::Bare; // the Other languages drill-in's own section

        // yours = [eng, rus]: English (single-track) is bucketed FIRST, so it folds into
        // "Subtitles" (still the untouched initial section) before Russian's own section
        // interrupts.
        let sections = layout(&subs, &["eng", "rus"]);
        assert_eq!(headers(&sections), [SubHeader::Subtitles, russian.clone(), other.clone(), SubHeader::Bare]);
        assert_eq!(sections[2].rows, [SubRow::OtherLanguages { languages: 1 }], "Other languages is one drill-in row");
        assert_eq!(sections[3].rows, [SubRow::Timing, SubRow::Style], "Timing and Style come after it");
        assert_eq!(flat_langs(&sections[0].rows), ["English"], "folded into Subtitles");

        // yours = [rus, eng]: the multi-track Russian bucket now outranks the single-track
        // English one, so Russian's section comes FIRST — once it has interrupted, English no
        // longer folds back into "Subtitles" and gets its own bare section instead
        // (`player.html`'s own `sectionsFor` rule: only the section BEFORE the first interruption
        // stays "Subtitles").
        let sections = layout(&subs, &["rus", "eng"]);
        assert_eq!(
            headers(&sections),
            [SubHeader::Subtitles, russian, SubHeader::Bare, other, SubHeader::Bare]
        );
        assert_eq!(flat_langs(&sections[2].rows), ["English"], "its own bare section, not Subtitles");
        assert_eq!(sections[4].rows, [SubRow::Timing, SubRow::Style], "the headerless Timing + Style section");
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
        let m = model(&subs, &[]);
        assert_eq!(m.sections[1].rows, [SubRow::OtherLanguages { languages: 1 }]);
        assert_eq!(m.other.len(), 1, "one French entry for both spellings");
        assert_eq!(m.other[0].tracks.len(), 2);
    }

    /// Tracks with no language code group by their display name, so two "Unknown" tracks count as
    /// one language — and never land in "yours", whatever `yours` holds.
    #[test]
    fn codeless_tracks_group_by_name_and_are_never_yours() {
        let subs = vec![stream(1, 0, "", "", ""), stream(2, 1, "", "", "")];
        let m = model(&subs, &["eng", ""]);
        assert_eq!(m.sections[1].rows, [SubRow::OtherLanguages { languages: 1 }]);
    }

    /// **Other languages is a page, not a section**: one entry per language sorted by name, a
    /// multi-track one ranked full < SDH < forced.
    #[test]
    fn other_languages_are_one_entry_per_language_sorted_by_name() {
        let subs = vec![
            stream(1, 0, "German", "deu", ""),
            stream(2, 1, "French", "fra", "SDH"),
            stream(3, 2, "Arabic", "ara", ""),
            stream(4, 3, "French", "fra", ""),
        ];
        let m = model(&subs, &[]);
        let names: Vec<&str> = m.other.iter().map(|o| o.name.as_str()).collect();
        assert_eq!(names, ["Arabic", "French", "German"]);
        let french: Vec<(usize, Kind)> = m.other[1].tracks.iter().map(|t| (t.i, t.kind)).collect();
        assert_eq!(french, [(3, Kind::Full), (1, Kind::Sdh)]);
        assert_eq!(m.sections[1].rows, [SubRow::OtherLanguages { languages: 3 }]);
        assert!(model(&subs[..0], &[]).other.is_empty());
    }

    /// **A language's identity is its first track's stream id in the FULL list**, offered or not:
    /// hiding the earlier track (a sidecar not offered on this route) does not move it.
    #[test]
    fn a_languages_identity_comes_from_the_full_list_not_the_offered_members() {
        let subs = vec![
            stream(1, 0, "French", "fra", ""),
            stream(2, 1, "German", "deu", ""),
            stream(3, 2, "French", "fra", "SDH"),
        ];
        let names = TrackNames::new();
        let all = sub_sections(&subs, &[0, 1, 2], &names, &[], true);
        let part = sub_sections(&subs, &[1, 2], &names, &[], true);
        assert_eq!(all.other.iter().find(|o| o.name == "French").unwrap().id.stream, 1);
        let french = part.other.iter().find(|o| o.name == "French").unwrap();
        assert_eq!((french.id.stream, french.tracks.len()), (1, 1), "the member that gave the key is not offered");
    }

    /// **An image subtitle's badge is a short display name, not Plex's raw codec string**:
    /// "HDMV_PGS_SUBTITLE" reads "PGS", "DVD_SUBTITLE" and "VOBSUB" read "VOBSUB", "DVB_SUBTITLE"
    /// reads "DVB". A text codec carries none.
    #[test]
    fn an_image_subtitles_badge_is_a_short_name() {
        let codecs = [
            ("pgs", Some("PGS")),
            ("hdmv_pgs_subtitle", Some("PGS")),
            ("vobsub", Some("VOBSUB")),
            ("dvd_subtitle", Some("VOBSUB")),
            ("dvdsub", Some("VOBSUB")),
            ("dvb_subtitle", Some("DVB")),
            ("dvbsub", Some("DVB")),
            ("srt", None),
        ];
        for (n, (codec, want)) in codecs.into_iter().enumerate() {
            let mut s = stream(n as i64 + 1, n as i64, "French", "fra", "");
            s.codec = codec.into();
            let m = model(&[s], &[]);
            let badge = m.other[0].tracks[0].badges.iter().find_map(|b| match b {
                RowBadge::Codec(c) => Some(*c),
                _ => None,
            });
            assert_eq!(badge, want, "{codec}");
        }
    }

    /// A language's identity is the stream id, so removing an earlier track moves its list
    /// positions but not what names it; its slot is its place on the page.
    #[test]
    fn a_languages_identity_survives_earlier_tracks_leaving() {
        let before = vec![
            stream(1, 0, "German", "deu", ""),
            stream(2, 1, "French", "fra", ""),
            stream(3, 2, "French", "fra", "SDH"),
        ];
        let after = &before[1..];
        let id = |subs: &[Stream]| model(subs, &[]).other.iter().find(|o| o.name == "French").unwrap().id;
        assert_eq!(id(&before), id(after));
        assert_eq!((id(&before).slot, id(after).slot), (0, 0), "A-Z: French sorts before German");
        let german = model(&before, &[]).other.iter().find(|o| o.name == "German").unwrap().id;
        assert_eq!((german.stream, german.slot), (1, 1));
    }

    #[test]
    fn row_targets_follow_the_rows_in_drawn_order() {
        let subs = vec![stream(1, 0, "English", "eng", ""), stream(2, 1, "French", "fra", "")];
        let targets: Vec<RowTarget> =
            layout(&subs, &["eng"]).iter().flat_map(|s| &s.rows).map(SubRow::target).collect();
        assert_eq!(
            targets,
            [RowTarget::Off, RowTarget::Sub(0), RowTarget::Other, RowTarget::Timing, RowTarget::Style]
        );
    }
}
