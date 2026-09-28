//! **The one track-name parser**, shared by the in-player Subtitles menu (`ui::track_menu`) and
//! the Tracks information panel (`screens::tracks_panel`). Ported from the approved Claude Design
//! mock's `parseTrackName` (`player.html:430-460`), which the design record settles as the
//! reference for the language-grouped Subtitles panel (`docs/../subtitle-menu-capsule` plan §1).
//!
//! A track's title from PMS mixes two different questions run together: what KIND of track it
//! is (a fixed, small vocabulary — FORCED, SDH, "full", a commentary track) and WHOSE it is (free
//! text: "iTunes", "DVD R5", "Е. Воронин"). The kind becomes a badge, or nothing at all when it
//! only restates that every unmarked track already is ("full"/"Полные"); what is left is the
//! SOURCE, and that is the detail line a Subtitles row draws under its language.
//!
//! Pure: strings and flags in, [`SubLabel`] out. No `metadata::Stream`, no `ui::` type — so it is
//! reachable from `screens::tracks_panel` (which never imports `ui::`) as well as
//! `ui::track_menu` (which already names `crate::metadata`).

/// A track's kind, in the SINGLE priority a Subtitles-panel row cares about (rank, badge, and the
/// fallback label a nameless multi-track row shows). The mock's rank is exactly this order:
/// `n.commentary ? 3 : forced ? 2 : sdh ? 1 : 0` (`player.html:953`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Kind {
    /// Neither forced, SDH, nor commentary — including a track whose title said "full"/"Полные"
    /// in so many words. The word itself is dropped from the source (`KIND_FULL` is never kept),
    /// so a title that said nothing else than "full" ends up indistinguishable from one that said
    /// nothing at all — which is correct: every unmarked track already is the full one.
    Full,
    Sdh,
    Forced,
    Commentary,
}

impl Kind {
    /// `player.html:955-956`'s ordering, full < SDH < forced < commentary, as an integer a caller
    /// can sort multiple tracks of one language by.
    pub(crate) fn rank(self) -> u8 {
        match self {
            Kind::Full => 0,
            Kind::Sdh => 1,
            Kind::Forced => 2,
            Kind::Commentary => 3,
        }
    }

    /// The word a NAMELESS track in a multi-track group is labelled by, when its own detail is
    /// empty (`player.html:1111`: `t.forced ? "Forced" : t.sdh ? "SDH" : "Full"`). A commentary
    /// track without a name of its own reads the same way.
    pub(crate) fn fallback_label(self) -> &'static str {
        match self {
            Kind::Full => "Full",
            Kind::Sdh => "SDH",
            Kind::Forced => "Forced",
            Kind::Commentary => "Commentary",
        }
    }
}

/// What a Subtitles-panel row draws for one track: the SOURCE ("iTunes", "DVD R5", or a region
/// name when nothing else says whose track this is) and its [`Kind`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SubLabel {
    pub(crate) source: String,
    pub(crate) kind: Kind,
}

// ---- leading/trailing kind-word stripping (`player.html:435-452`) ---------------------------

/// Forced-track spellings, lower-cased and with a trailing `.` already stripped (`форс.` and
/// `форс` are the same word to this table — see [`normalize_word`]).
const FORCED_WORDS: &[&str] = &["forced", "forsed", "форс", "форсированные", "форсовані", "sign", "signs"];
/// SDH spellings, single-word only — the two-word Russian phrase is [`SDH_PHRASES`], checked
/// BEFORE the generic word-by-word strip (this is the fix for the mock's split bug: naive
/// whitespace tokenising breaks "для слабослышащих" into two words, neither of which matches this
/// table alone, so the mock never detects it).
const SDH_WORDS: &[&str] = &["sdh", "cc", "hi", "sdh-colored"];
/// Multi-word phrases that must be matched as a WHOLE before the generic tokeniser ever sees
/// their pieces. Checked lower-cased against two adjacent words joined by one space.
const SDH_PHRASES: &[&str] = &["для слабослышащих"];
const FULL_WORDS: &[&str] = &["full", "полные", "полный", "повні", "повнi", "complete"];
/// Substring test over the WHOLE original title, not a word — `player.html:439`'s
/// `/commentary|комментари/i`.
const COMMENTARY_MARKS: &[&str] = &["commentary", "комментари"];

fn normalize_word(w: &str) -> String {
    // Unicode-aware: `to_ascii_lowercase` leaves Cyrillic untouched (it only folds A-Z), so
    // "Форс." would never match the table's lower-case "форс" — the bug `sub_sets_wicked` pins.
    w.trim_end_matches('.').to_lowercase()
}

/// One word's kind, if the word IS one — never a substring test, so "Fullscreen" or "Forcedly"
/// (hypothetical, but the point stands) are not silently eaten.
fn classify_word(w: &str) -> Option<Kind> {
    let n = normalize_word(w);
    if FORCED_WORDS.contains(&n.as_str()) {
        Some(Kind::Forced)
    } else if SDH_WORDS.contains(&n.as_str()) {
        Some(Kind::Sdh)
    } else if FULL_WORDS.contains(&n.as_str()) {
        Some(Kind::Full)
    } else {
        None
    }
}

/// Split a title into words the way the mock does: brackets/parens, `,`, `/`, `|` and a
/// space-hyphen-space all separate; any OTHER whitespace run also separates; a bare hyphen
/// inside a word ("Blu-Ray", "SDH-Colored") stays part of it.
fn split_words(name: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut cur = String::new();
    for ch in name.chars() {
        let is_sep = ch.is_whitespace() || matches!(ch, '[' | ']' | '(' | ')' | ',' | '/' | '|');
        if is_sep {
            if !cur.is_empty() {
                words.push(std::mem::take(&mut cur));
            }
        } else {
            cur.push(ch);
        }
    }
    if !cur.is_empty() {
        words.push(cur);
    }
    // A lone "-" only ever appears here as the remnant of a " - " separator (a hyphen INSIDE a
    // word never got split off it above), so it is never a track's own name.
    words.into_iter().filter(|w| w != "-").collect()
}

/// Strip leading kind words (front of `words`) — multi-word phrases first, so a phrase whose
/// pieces don't individually match anything is still recognised, then generic word-by-word.
/// Stops at the first word/phrase that is NOT a kind word: a kind word mid-phrase is never
/// touched, because by then it is no longer at the front.
fn strip_leading(words: &mut Vec<String>) -> Option<Kind> {
    let mut found: Option<Kind> = None;
    loop {
        if words.len() >= 2 {
            let joined = format!("{} {}", words[0], words[1]).to_lowercase();
            if SDH_PHRASES.contains(&joined.as_str()) {
                found = Some(fold_kind(found, Kind::Sdh));
                words.drain(0..2);
                continue;
            }
        }
        match words.first().and_then(|w| classify_word(w)) {
            Some(k) => {
                found = Some(fold_kind(found, k));
                words.remove(0);
            }
            None => break,
        }
    }
    found
}

/// The trailing mirror of [`strip_leading`].
fn strip_trailing(words: &mut Vec<String>) -> Option<Kind> {
    let mut found: Option<Kind> = None;
    loop {
        let n = words.len();
        if n >= 2 {
            let joined = format!("{} {}", words[n - 2], words[n - 1]).to_lowercase();
            if SDH_PHRASES.contains(&joined.as_str()) {
                found = Some(fold_kind(found, Kind::Sdh));
                words.truncate(n - 2);
                continue;
            }
        }
        match words.last().and_then(|w| classify_word(w)) {
            Some(k) => {
                found = Some(fold_kind(found, k));
                words.pop();
            }
            None => break,
        }
    }
    found
}

/// Combine two kind observations from the SAME title (a leading AND a trailing strip may each
/// find something) by the panel's own priority: forced beats SDH beats full — matching the
/// mock's flat booleans, where `forced`/`sdh` are each `||`-accumulated across every stripped
/// word rather than the last one winning.
fn fold_kind(prev: Option<Kind>, next: Kind) -> Kind {
    match (prev, next) {
        (Some(Kind::Forced), _) | (_, Kind::Forced) => Kind::Forced,
        (Some(Kind::Sdh), _) | (_, Kind::Sdh) => Kind::Sdh,
        (Some(k), _) => k,
        (None, k) => k,
    }
}

/// Parse a MERGED track title (see [`track_name`] for what "merged" means) into its [`SubLabel`],
/// folding in the structured `Stream.forced`/`Stream.sdh` flags — set on 2 of 164 real-world
/// Russian subtitle parts probed live, so the text is still the primary signal, but a server flag
/// must never be silently dropped either (`player.html:947`: `forced = !!(flags & 1) || n.forced`).
pub(crate) fn parse(name: &str, forced_flag: bool, sdh_flag: bool) -> SubLabel {
    let commentary = {
        let lower = name.to_lowercase();
        COMMENTARY_MARKS.iter().any(|m| lower.contains(m))
    };
    let mut words = split_words(name);
    let front = strip_leading(&mut words);
    let back = if words.is_empty() { None } else { strip_trailing(&mut words) };
    let text_kind = match (front, back) {
        (Some(a), Some(b)) => Some(fold_kind(Some(a), b)),
        (Some(a), None) => Some(a),
        (None, Some(b)) => Some(b),
        (None, None) => None,
    };
    let source = words.join(" ");

    let forced = forced_flag || text_kind == Some(Kind::Forced);
    let sdh = sdh_flag || text_kind == Some(Kind::Sdh);
    let kind = if commentary {
        Kind::Commentary
    } else if forced {
        Kind::Forced
    } else if sdh {
        Kind::Sdh
    } else {
        Kind::Full
    };
    SubLabel { source, kind }
}

// ---- title merge (moved from `ui::track_menu::track_name`) ----------------------------------

/// **The one name a track row shows, from the two places a name can come from.**
///
/// `pms` is `Stream.title` — what the server parsed out of the container — and `container` is what
/// OUR demuxer read out of the same file (`player::TrackNames`, published by `ff.rs`). They are the
/// same tag seen twice, so they do not disagree in practice; the order matters for a different
/// reason. PMS's copy exists **before playback starts** and survives a transcode, while the
/// demuxer's only exists on direct play and only once the file is open — so the server's answer is
/// preferred when it has one, and the file's is what fills the hole when it does not.
///
/// That hole is the whole point: **for an MP4 part PMS sends no `title` at all.** Matroska spells
/// the tag `title` and MP4 spells it `name`, and Plex's parser maps only the first (verified live
/// against one server holding both). So the six Russian tracks of a nine-track MP4 arrive with
/// nothing to tell them apart, while the file itself says `Форс. iTunes`, `Полные Jaskier`,
/// `Полные stirloo`.
///
/// **A name equal to the language is discarded**, from either source, because a row already says
/// its language in the label above: a sub-line reading `English` under `English` spends the row's
/// second line to repeat it. `eq_ignore_ascii_case` is deliberately ASCII-only and stays that way —
/// it is a cheap guard against `English`/`english`, not a Unicode fold, and the case it must not
/// get wrong is the one where the two differ.
pub(crate) fn track_name(pms: &str, container: &str, lang: &str) -> String {
    for cand in [pms.trim(), container.trim()] {
        if !cand.is_empty() && !cand.eq_ignore_ascii_case(lang) {
            return cand.to_string();
        }
    }
    String::new()
}

// ---- region fallback (`player.html:950-951`) -------------------------------------------------

/// A small curated table of BCP-47 region subtags this app is likely to see on a subtitle track,
/// mapped to the name `Intl.DisplayNames` would print (verified against the mock's own
/// `Intl.DisplayNames(["en"], {type:"region"})` calls). Not exhaustive — a region this table does
/// not know simply contributes no fallback, exactly as the mock's own `try`/`catch` around a
/// `DisplayNames` miss does.
const REGIONS: &[(&str, &str)] = &[
    ("GB", "United Kingdom"),
    ("US", "United States"),
    ("BR", "Brazil"),
    ("PT", "Portugal"),
    ("ES", "Spain"),
    ("MX", "Mexico"),
    ("FR", "France"),
    ("BE", "Belgium"),
    ("CA", "Canada"),
    ("DE", "Germany"),
    ("AT", "Austria"),
    ("CH", "Switzerland"),
    ("CN", "China"),
    ("TW", "Taiwan"),
    ("HK", "Hong Kong"),
    ("IN", "India"),
    ("AU", "Australia"),
    ("RU", "Russia"),
    ("UA", "Ukraine"),
    ("IT", "Italy"),
    ("JP", "Japan"),
    ("KR", "South Korea"),
    ("NL", "Netherlands"),
];

/// The region-name fallback for a BCP-47 tag ("es-419" → "Latin America", "en-GB" → "United
/// Kingdom") — used ONLY when a track's parsed source is empty, exactly as the mock reads: *"a
/// region is only ours to name when the name said nothing"* (`player.html:949`).
pub(crate) fn region_detail(tag: &str) -> Option<String> {
    let region = tag.split(['-', '_']).nth(1)?;
    if region.is_empty() {
        return None;
    }
    if region.eq_ignore_ascii_case("419") {
        return Some("Latin America".to_string());
    }
    REGIONS
        .iter()
        .find(|(code, _)| code.eq_ignore_ascii_case(region))
        .map(|(_, name)| (*name).to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- the moved `track_name` cases (were `ui::track_menu::tests`) ------------------------

    #[test]
    fn an_mp4s_container_names_tell_apart_the_tracks_pms_reports_identically() {
        let pms_title = "";
        let lang = "Русский";
        let container = [
            "Форс. iTunes",
            "Форс. Jaskier песни",
            "Форс. Red Head Sound песни",
            "Полные iTunes",
            "Полные Jaskier",
            "Полные stirloo",
        ];
        let rows: Vec<String> = container.iter().map(|c| track_name(pms_title, c, lang)).collect();
        assert_eq!(rows, container, "each row shows its own track's name");
        let distinct: std::collections::HashSet<&String> = rows.iter().collect();
        assert_eq!(distinct.len(), rows.len(), "no two rows of one language may read the same");
    }

    #[test]
    fn the_servers_own_title_wins_when_it_has_one() {
        assert_eq!(track_name("HDRezka Studio", "", "Русский"), "HDRezka Studio");
        assert_eq!(track_name("Forced", "Forced", "Русский"), "Forced");
    }

    #[test]
    fn a_name_that_only_repeats_the_language_is_not_shown() {
        assert_eq!(track_name("English", "", "English"), "");
        assert_eq!(track_name("english", "", "English"), "", "the guard is case-insensitive");
        assert_eq!(track_name("", "", "English"), "");
        assert_eq!(
            track_name("English", "Full SDH", "English"),
            "Full SDH",
            "a useless PMS title falls through to the container's"
        );
        assert_eq!(track_name("  ", " Full ", "English"), "Full", "both sides are trimmed");
    }

    // ---- leading/trailing stripping, not mid-phrase ------------------------------------------

    #[test]
    fn leading_and_trailing_kind_words_are_stripped_but_not_a_kind_word_mid_phrase() {
        let l = parse("Forced Anna's Full Edit", false, false);
        assert_eq!(l.source, "Anna's Full Edit", "the mid-phrase \"Full\" is part of the source");
        assert_eq!(l.kind, Kind::Forced);

        let l = parse("Director's Cut SDH", false, false);
        assert_eq!(l.source, "Director's Cut");
        assert_eq!(l.kind, Kind::Sdh);

        // a kind word that is not at either edge never strips
        let l = parse("Anna Forced Edit", false, false);
        assert_eq!(l.source, "Anna Forced Edit");
        assert_eq!(l.kind, Kind::Full);
    }

    // ---- the multi-word SDH phrase (the mock's own split bug, fixed here) --------------------

    #[test]
    fn a_multi_word_sdh_phrase_is_recognised_at_either_edge() {
        let l = parse("для слабослышащих", false, false);
        assert_eq!(l.source, "");
        assert_eq!(l.kind, Kind::Sdh);

        let l = parse("Director cut для слабослышащих", false, false);
        assert_eq!(l.source, "Director cut");
        assert_eq!(l.kind, Kind::Sdh);

        // mid-phrase: still not touched
        let l = parse("A для слабослышащих B", false, false);
        assert_eq!(l.source, "A для слабослышащих B");
        assert_eq!(l.kind, Kind::Full);
    }

    // ---- SUB_SETS fixtures (`/tmp/dsplayer/player.html`, snatch/frozen/wicked/hobbit/homealone;
    // wallace is the 100-track "every language once" set and carries no kind words at all, so it
    // adds no coverage here) — ground truth computed by running the mock's own `parseTrackName`
    // over the identical titles (`node -e` against `player.ref.html`'s literal source).

    #[test]
    fn sub_sets_snatch() {
        let cases: &[(&str, bool, &str, Kind)] = &[
            ("forced, DVD R5", true, "DVD R5", Kind::Forced),
            ("forced, Позитив Мультимедиа", true, "Позитив Мультимедиа", Kind::Forced),
            ("Позитив Мультимедиа", false, "Позитив Мультимедиа", Kind::Full),
            ("DVD R5", false, "DVD R5", Kind::Full),
            ("Netflix", false, "Netflix", Kind::Full),
            ("Д. Пучков aka Гоблин", false, "Д. Пучков aka Гоблин", Kind::Full),
            ("full, Netflix", false, "Netflix", Kind::Full),
            ("forced", true, "", Kind::Forced),
            ("full", false, "", Kind::Full),
            ("SDH", false, "", Kind::Sdh),
        ];
        for &(title, forced, source, kind) in cases {
            let l = parse(title, false, false);
            assert_eq!(l.source, source, "title {title:?}");
            assert_eq!(l.kind, kind, "title {title:?}");
            assert_eq!(l.kind == Kind::Forced, forced, "title {title:?}");
        }
    }

    #[test]
    fn sub_sets_frozen() {
        let cases: &[(&str, &str, Kind)] = &[
            ("Forced", "", Kind::Forced),
            ("Full [iTunes]", "iTunes", Kind::Full),
            ("Full [Notabenoid]", "Notabenoid", Kind::Full),
            ("Full", "", Kind::Full),
            ("SDH", "", Kind::Sdh),
            ("SDH-Colored", "", Kind::Sdh),
        ];
        for &(title, source, kind) in cases {
            let l = parse(title, false, false);
            assert_eq!(l.source, source, "title {title:?}");
            assert_eq!(l.kind, kind, "title {title:?}");
        }
    }

    #[test]
    fn sub_sets_wicked() {
        let cases: &[(&str, &str, Kind)] = &[
            ("Форс. iTunes", "iTunes", Kind::Forced),
            ("Форс. Jaskier песни", "Jaskier песни", Kind::Forced),
            ("Форс. Red Head Sound песни", "Red Head Sound песни", Kind::Forced),
            ("Полные iTunes", "iTunes", Kind::Full),
            ("Полные Jaskier", "Jaskier", Kind::Full),
            ("Полные stirloo", "stirloo", Kind::Full),
            ("Full", "", Kind::Full),
            ("Full SDH", "", Kind::Sdh),
            ("Повнi iTunes", "iTunes", Kind::Full),
        ];
        for &(title, source, kind) in cases {
            let l = parse(title, false, false);
            assert_eq!(l.source, source, "title {title:?}");
            assert_eq!(l.kind, kind, "title {title:?}");
        }
    }

    #[test]
    fn sub_sets_hobbit() {
        let cases: &[(&str, bool, bool, &str, Kind)] = &[
            ("forced", false, false, "", Kind::Forced),
            ("full / Blu-Ray", false, false, "Blu-Ray", Kind::Full),
            ("full / по дубляжу", false, false, "по дубляжу", Kind::Full),
            ("full / Е. Воронин", false, false, "Е. Воронин", Kind::Full),
            ("full", false, false, "", Kind::Full),
            // this one's PMS `forced`/`sdh` flags are 0 in every case above but flags=2 (SDH) on
            // the real fixture's last English track (`SUB_SETS.hobbit`'s `[…, 2, "SDH", …]`)
            ("SDH", false, true, "", Kind::Sdh),
        ];
        for &(title, forced_flag, sdh_flag, source, kind) in cases {
            let l = parse(title, forced_flag, sdh_flag);
            assert_eq!(l.source, source, "title {title:?}");
            assert_eq!(l.kind, kind, "title {title:?}");
        }
    }

    #[test]
    fn sub_sets_homealone_the_floor_case() {
        // nothing anywhere: no title, no flags — every track reads as an unmarked Full with an
        // empty source, which is the fallback-label floor `sub_layout` has to draw something for.
        let l = parse("", false, false);
        assert_eq!(l.source, "");
        assert_eq!(l.kind, Kind::Full);
    }

    // ---- region fallback -----------------------------------------------------------------------

    #[test]
    fn the_region_fallback_only_applies_when_the_source_is_empty() {
        assert_eq!(region_detail("es-419").as_deref(), Some("Latin America"));
        assert_eq!(region_detail("en-GB").as_deref(), Some("United Kingdom"));
        assert_eq!(region_detail("pt-BR").as_deref(), Some("Brazil"));
        assert_eq!(region_detail("en").as_deref(), None, "no region subtag at all");
        assert_eq!(region_detail("zh-Hant").as_deref(), None, "a 4-letter SCRIPT subtag, not a region");
        assert_eq!(region_detail("xx-ZZ").as_deref(), None, "a region this curated table doesn't know");
    }
}
