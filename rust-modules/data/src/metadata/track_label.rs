//! **The one track-name parser**, shared by the in-player Subtitles menu (`appkit::track_menu`) and
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
//! `appkit::track_menu` (which already names `crate::metadata`).

/// A track's kind, in the SINGLE priority a Subtitles-panel row cares about (rank, badge, and the
/// fallback label a nameless multi-track row shows). The mock's rank is exactly this order:
/// `n.commentary ? 3 : forced ? 2 : sdh ? 1 : 0` (`player.html:953`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
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
    pub fn rank(self) -> u8 {
        match self {
            Kind::Full => 0,
            Kind::Sdh => 1,
            Kind::Forced => 2,
            Kind::Commentary => 3,
        }
    }

    /// The word a NAMELESS track in a multi-track group is labelled by, when its own detail is
    /// empty (`player.html:1111`: `t.forced ? "Forced" : t.sdh ? "SDH" : "Full"`). A commentary
    /// track without a name of its own reads the same way. Catalog text: this is drawn.
    pub fn fallback_label(self) -> &'static str {
        match self {
            Kind::Full => plx_platform::i18n::msg::widgets_tracks_kind_full(),
            Kind::Sdh => plx_platform::i18n::msg::widgets_badge_sdh(),
            Kind::Forced => plx_platform::i18n::msg::widgets_tracks_forced(),
            Kind::Commentary => plx_platform::i18n::msg::widgets_tracks_kind_commentary(),
        }
    }
}

/// What a Subtitles-panel row draws for one track: the SOURCE ("iTunes", "DVD R5", or a region
/// name when nothing else says whose track this is) and its [`Kind`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SubLabel {
    pub source: String,
    pub kind: Kind,
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

/// Whether a track title names a commentary track (Latin or Cyrillic): [`parse`]'s test, shared
/// with the audio pickers so "a commentary" means one thing everywhere.
pub fn is_commentary(title: &str) -> bool {
    let lower = title.to_lowercase();
    COMMENTARY_MARKS.iter().any(|m| lower.contains(m))
}

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

/// One word of a title, with its byte span in the ORIGINAL title — the span is what lets
/// [`parse`] re-slice the source from the title itself (keeping the commas, brackets and spacing
/// inside the kept run) rather than re-joining words with single spaces.
#[derive(Clone, Debug)]
struct Word {
    text: String,
    start: usize,
    end: usize,
}

/// Split a title into words the way the mock does: brackets/parens, `,`, `/`, `|` and a
/// space-hyphen-space all separate; any OTHER whitespace run also separates; a bare hyphen
/// inside a word ("Blu-Ray", "SDH-Colored") stays part of it.
fn split_words(name: &str) -> Vec<Word> {
    let mut words = Vec::new();
    let mut start: Option<usize> = None;
    for (at, ch) in name.char_indices() {
        let is_sep = ch.is_whitespace() || matches!(ch, '[' | ']' | '(' | ')' | ',' | '/' | '|');
        match (is_sep, start) {
            (true, Some(s)) => {
                words.push(Word { text: name[s..at].to_string(), start: s, end: at });
                start = None;
            }
            (false, None) => start = Some(at),
            _ => {}
        }
    }
    if let Some(s) = start {
        words.push(Word { text: name[s..].to_string(), start: s, end: name.len() });
    }
    // A lone "-" only ever appears here as the remnant of a " - " separator (a hyphen INSIDE a
    // word never got split off it above), so it is never a track's own name.
    words.into_iter().filter(|w| w.text != "-").collect()
}

/// Phrases whose words look like kind words but together NAME a track: "Signs & Songs" is the
/// anime release convention for a typesetting track, not a forced-subtitle marker — "signs" alone
/// is in [`FORCED_WORDS`], so without this the edge strip would eat it and badge the track FORCED.
/// Matched lower-cased on consecutive words, at either edge, before any kind word is stripped.
const NAMING_PHRASES: &[&[&str]] = &[&["signs", "&", "songs"], &["signs", "and", "songs"], &["signs", "songs"], &["songs", "&", "signs"], &["songs", "and", "signs"]];

/// Does a [`NAMING_PHRASES`] entry start at `words[0]` (`from_back == false`) or end at the last
/// word (`from_back == true`)?
fn naming_phrase_at_edge(words: &[Word], from_back: bool) -> bool {
    NAMING_PHRASES.iter().any(|phrase| {
        let n = phrase.len();
        if words.len() < n {
            return false;
        }
        let run = if from_back { &words[words.len() - n..] } else { &words[..n] };
        run.iter().zip(phrase.iter()).all(|(w, p)| normalize_word(&w.text) == *p)
    })
}

/// Strip leading kind words (front of `words`) — multi-word phrases first, so a phrase whose
/// pieces don't individually match anything is still recognised, then generic word-by-word.
/// Stops at the first word/phrase that is NOT a kind word: a kind word mid-phrase is never
/// touched, because by then it is no longer at the front. A [`NAMING_PHRASES`] run at the front
/// stops it too.
fn strip_leading(words: &mut Vec<Word>) -> Option<Kind> {
    let mut found: Option<Kind> = None;
    loop {
        if naming_phrase_at_edge(words, false) {
            break;
        }
        if words.len() >= 2 {
            let joined = format!("{} {}", words[0].text, words[1].text).to_lowercase();
            if SDH_PHRASES.contains(&joined.as_str()) {
                found = Some(fold_kind(found, Kind::Sdh));
                words.drain(0..2);
                continue;
            }
        }
        match words.first().and_then(|w| classify_word(&w.text)) {
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
fn strip_trailing(words: &mut Vec<Word>) -> Option<Kind> {
    let mut found: Option<Kind> = None;
    loop {
        if naming_phrase_at_edge(words, true) {
            break;
        }
        let n = words.len();
        if n >= 2 {
            let joined = format!("{} {}", words[n - 2].text, words[n - 1].text).to_lowercase();
            if SDH_PHRASES.contains(&joined.as_str()) {
                found = Some(fold_kind(found, Kind::Sdh));
                words.truncate(n - 2);
                continue;
            }
        }
        match words.last().and_then(|w| classify_word(&w.text)) {
            Some(k) => {
                found = Some(fold_kind(found, k));
                words.pop();
            }
            None => break,
        }
    }
    found
}

/// The kept words' span of the ORIGINAL title (`player.html:453-456`): from the first kept word's
/// start to the last one's end, so the commas, slashes and brackets BETWEEN kept words survive
/// ("forced, Studio, Remux" keeps "Studio, Remux"). Square brackets become spaces and whitespace
/// runs collapse, as in the mock; a leading/trailing separator the span caught is trimmed; and a
/// round bracket is kept only when the span holds its partner — the closing `)` of "Director's Cut
/// (Extended)" sits just past the last word, so it is pulled in rather than cut off.
fn reslice(name: &str, words: &[Word]) -> String {
    let (Some(first), Some(last)) = (words.first(), words.last()) else {
        return String::new();
    };
    let start = first.start;
    let mut end = last.end;
    let opens = |t: &str| t.matches('(').count();
    let closes = |t: &str| t.matches(')').count();
    while opens(&name[start..end]) > closes(&name[start..end]) {
        match name[end..].find(')') {
            Some(off) if name[end..end + off].chars().all(|c| c.is_whitespace() || c == ')') => end += off + 1,
            _ => break,
        }
    }
    let span: String = name[start..end].replace(['[', ']'], " ");
    let collapsed = span.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut out = collapsed.trim_matches(|c: char| c.is_whitespace() || matches!(c, ',' | '/' | '-' | '|')).to_string();
    // an unpartnered bracket at either edge is separator debris, not part of the name
    if out.starts_with('(') && opens(&out) > closes(&out) {
        out.remove(0);
    }
    if out.ends_with(')') && closes(&out) > opens(&out) {
        out.pop();
    }
    out.trim().to_string()
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
///
/// `lang` is the track's display language. A source that only repeats it ("English SDH" on an
/// English track, "Русский форсированные" on a Russian one) is dropped once the kind words are
/// gone — the row already says its language, the same rule [`track_name`] applies to a whole
/// title, but compared with a full Unicode case fold, since the source is often Cyrillic.
pub fn parse(name: &str, lang: &str, forced_flag: bool, sdh_flag: bool) -> SubLabel {
    let commentary = is_commentary(name);
    let mut words = split_words(name);
    let front = strip_leading(&mut words);
    let back = if words.is_empty() { None } else { strip_trailing(&mut words) };
    let text_kind = match (front, back) {
        (Some(a), Some(b)) => Some(fold_kind(Some(a), b)),
        (Some(a), None) => Some(a),
        (None, Some(b)) => Some(b),
        (None, None) => None,
    };
    let mut source = reslice(name, &words);
    if !lang.trim().is_empty() && source.to_lowercase() == lang.trim().to_lowercase() {
        source.clear();
    }

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

/// **A track's `(forced, SDH)` pair**: the structured `Stream.forced`/`Stream.sdh` flags, OR'd with
/// whatever the parsed [`SubLabel::kind`] says. The one rule for the callers that show both halves
/// (the Tracks panel's detail line, which lists a forced SDH file as both); `parse` itself keeps
/// only the single highest-priority [`Kind`].
pub fn flags(label: &SubLabel, forced: bool, sdh: bool) -> (bool, bool) {
    (forced || label.kind == Kind::Forced, sdh || label.kind == Kind::Sdh)
}

// ---- title merge (moved from `appkit::track_menu::track_name`) ----------------------------------

/// **The one name a track row shows, from the two places a name can come from.**
///
/// `pms` is `Stream.title` — what the server parsed out of the container — and `container` is what
/// OUR demuxer read out of the same file (`track_names::TrackNames`, published by `ff.rs`). They are the
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
pub fn track_name(pms: &str, container: &str, lang: &str) -> String {
    for cand in [pms.trim(), container.trim()] {
        if !cand.is_empty() && !cand.eq_ignore_ascii_case(lang) {
            return cand.to_string();
        }
    }
    String::new()
}

// ---- region fallback (`player.html:950-951`) -------------------------------------------------

/// A small curated table of BCP-47 region subtags this app is likely to see on a subtitle track,
/// mapped to the catalog accessor for the region's name (`widgets.region.*`; the English values
/// match what the mock's `Intl.DisplayNames(["en"], {type:"region"})` prints). Not exhaustive — a
/// region this table does not know simply contributes no fallback, exactly as the mock's own
/// `try`/`catch` around a `DisplayNames` miss does.
const REGIONS: &[(&str, fn() -> &'static str)] = {
    use plx_platform::i18n::msg as m;
    &[
        ("GB", m::widgets_region_gb),
        ("US", m::widgets_region_us),
        ("BR", m::widgets_region_br),
        ("PT", m::widgets_region_pt),
        ("ES", m::widgets_region_es),
        ("MX", m::widgets_region_mx),
        ("FR", m::widgets_region_fr),
        ("BE", m::widgets_region_be),
        ("CA", m::widgets_region_ca),
        ("DE", m::widgets_region_de),
        ("AT", m::widgets_region_at),
        ("CH", m::widgets_region_ch),
        ("CN", m::widgets_region_cn),
        ("TW", m::widgets_region_tw),
        ("HK", m::widgets_region_hk),
        ("IN", m::widgets_region_in),
        ("AU", m::widgets_region_au),
        ("RU", m::widgets_region_ru),
        ("UA", m::widgets_region_ua),
        ("IT", m::widgets_region_it),
        ("JP", m::widgets_region_jp),
        ("KR", m::widgets_region_kr),
        ("NL", m::widgets_region_nl),
        // UN M.49's numeric area code, the one numeric region a subtitle tag commonly carries
        ("419", m::widgets_region_latin_america),
    ]
};

/// The region-name fallback for a BCP-47 tag ("es-419" → "Latin America", "en-GB" → "United
/// Kingdom", "zh-Hant-TW" → "Taiwan") — used ONLY when a track's parsed source is empty, exactly
/// as the mock reads: *"a region is only ours to name when the name said nothing"*
/// (`player.html:949`). The region is the first subtag after the language that is two letters or
/// three digits; a four-letter SCRIPT subtag ("Hant") is skipped over, never read as a region.
pub fn region_detail(tag: &str) -> Option<String> {
    let region = tag.split(['-', '_']).skip(1).find(|t| {
        (t.len() == 2 && t.chars().all(|c| c.is_ascii_alphabetic())) || (t.len() == 3 && t.chars().all(|c| c.is_ascii_digit()))
    })?;
    REGIONS
        .iter()
        .find(|(code, _)| code.eq_ignore_ascii_case(region))
        .map(|(_, name)| name().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_commentary_reads_latin_and_cyrillic_titles() {
        assert!(is_commentary("Director's Commentary"));
        assert!(is_commentary("English (AC3 2.0) COMMENTARY"));
        assert!(is_commentary("Комментарии режиссёра"));
        assert!(!is_commentary("Original"));
        assert!(!is_commentary(""));
    }

    // ---- the moved `track_name` cases (were `appkit::track_menu::tests`) ------------------------

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
        let l = parse("Forced Anna's Full Edit", "", false, false);
        assert_eq!(l.source, "Anna's Full Edit", "the mid-phrase \"Full\" is part of the source");
        assert_eq!(l.kind, Kind::Forced);

        let l = parse("Director's Cut SDH", "", false, false);
        assert_eq!(l.source, "Director's Cut");
        assert_eq!(l.kind, Kind::Sdh);

        // a kind word that is not at either edge never strips
        let l = parse("Anna Forced Edit", "", false, false);
        assert_eq!(l.source, "Anna Forced Edit");
        assert_eq!(l.kind, Kind::Full);
    }

    // ---- the multi-word SDH phrase (the mock's own split bug, fixed here) --------------------

    #[test]
    fn a_multi_word_sdh_phrase_is_recognised_at_either_edge() {
        let l = parse("для слабослышащих", "", false, false);
        assert_eq!(l.source, "");
        assert_eq!(l.kind, Kind::Sdh);

        let l = parse("Director cut для слабослышащих", "", false, false);
        assert_eq!(l.source, "Director cut");
        assert_eq!(l.kind, Kind::Sdh);

        // mid-phrase: still not touched
        let l = parse("A для слабослышащих B", "", false, false);
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
            let l = parse(title, "", false, false);
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
            let l = parse(title, "", false, false);
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
            let l = parse(title, "", false, false);
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
            let l = parse(title, "", forced_flag, sdh_flag);
            assert_eq!(l.source, source, "title {title:?}");
            assert_eq!(l.kind, kind, "title {title:?}");
        }
    }

    #[test]
    fn sub_sets_homealone_the_floor_case() {
        // nothing anywhere: no title, no flags — every track reads as an unmarked Full with an
        // empty source, which is the fallback-label floor `appkit::track_menu::in_lang_row` has to
        // draw something for.
        let l = parse("", "", false, false);
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
        assert_eq!(region_detail("zh-Hant-TW").as_deref(), Some("Taiwan"), "the region past a script subtag");
        assert_eq!(region_detail("xx-ZZ").as_deref(), None, "a region this curated table doesn't know");
    }

    // ---- review round: source re-slicing, language echo, naming phrases ----------------------

    #[test]
    fn the_source_is_resliced_from_the_title_keeping_its_inner_punctuation() {
        let l = parse("forced, Studio, Remux", "", false, false);
        assert_eq!(l.source, "Studio, Remux");
        assert_eq!(l.kind, Kind::Forced);
        let l = parse("Director's Cut (Extended)", "", false, false);
        assert_eq!(l.source, "Director's Cut (Extended)");
        assert_eq!(l.kind, Kind::Full);
        let l = parse("SDH / Director's Cut (Extended)", "", false, false);
        assert_eq!(l.source, "Director's Cut (Extended)");
        assert_eq!(l.kind, Kind::Sdh);
    }

    #[test]
    fn a_source_that_only_repeats_the_language_is_dropped() {
        for (title, lang, kind) in [
            ("English SDH", "English", Kind::Sdh),
            ("English (Forced)", "English", Kind::Forced),
            ("Русский форсированные", "Русский", Kind::Forced),
            ("русский форсированные", "Русский", Kind::Forced),
        ] {
            let l = parse(title, lang, false, false);
            assert_eq!(l.source, "", "title {title:?}");
            assert_eq!(l.kind, kind, "title {title:?}");
        }
        assert_eq!(parse("English Remux", "English", false, false).source, "English Remux");
    }

    #[test]
    fn signs_and_songs_names_a_track_and_is_not_a_forced_marker() {
        for title in ["Signs & Songs", "Signs and Songs", "Songs & Signs", "Signs/Songs"] {
            let l = parse(title, "", false, false);
            assert_eq!(l.kind, Kind::Full, "title {title:?}");
            assert!(!l.source.is_empty(), "title {title:?}");
        }
        assert_eq!(parse("Signs & Songs", "", false, false).source, "Signs & Songs");
        let l = parse("Forced Signs & Songs", "", false, false);
        assert_eq!((l.source.as_str(), l.kind), ("Signs & Songs", Kind::Forced));
        assert_eq!(parse("Signs", "", false, false).kind, Kind::Forced, "the word alone still is one");
    }
}
