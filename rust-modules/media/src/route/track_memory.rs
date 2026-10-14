//! **Track memory, the read path: the matchers.** A remembered pick is a DESCRIPTOR (language,
//! forced, SDH, codec, title, ...), not a stream id, because the next episode's file has other ids.
//! These pure functions turn a descriptor back into a track of THIS file, or say why they cannot.
//! They know nothing of the ladder they sit in ([`super::plan::pick_subtitle`] and
//! [`super::plan::audio_intents`] own the rung order) and nothing of the route that will carry the
//! answer: a matcher decides WHICH track, the carriage decides HOW (`track-memory-plan.md`, Priority
//! schema).
//!
//! * **Hard constraints decline, soft ones only rank.** A track that breaks a hard constraint
//!   (language, commentary and audio description for audio; language and `forced` for a subtitle)
//!   is never returned, and a record that matches none of the file's tracks DECLINES: the ladder
//!   falls through to the rungs below it and the episode plays as it would have, never to Off. The
//!   soft constraints (codec, channels, tag, title, SDH, kind, embedded first, file order) only
//!   choose between tracks that already satisfy the hard ones, and the first of the best wins.
//! * **"Forced" is the menu's own word.** A subtitle is forced to a matcher exactly when the track
//!   menu badges it FORCED: [`sub_traits`] runs the menu's parse over the merged name
//!   (`track_label::track_name` then `parse`, then `flags`), so a track titled "Forced" with no
//!   flag is forced, a flagged commentary is forced too (for a Track memo commentary only ranks),
//!   and a memo written from the menu matches back. At resolve time the demuxer's own names (an
//!   MP4 part has no PMS `title`) are not known yet, so the container name is empty.
//! * **Forced only** ([`match_forced_only`]) is a matcher of its own: the forced, non-commentary
//!   track in the language of the audio that is PLAYING (the memo's language only when the audio
//!   has none), or nothing. A commentary is never the answer to Forced only, however it is
//!   flagged.
use plx_data::metadata::sub_layout::is_image_sub_codec;
use plx_data::metadata::track_label::{self, Kind};
use plx_data::metadata::{lang_matches, Stream};
use plx_plex::plex::session::prefs::{AudioMemo, SubtitleTrackMemo};

/// Why a record matched no track: the log's `no match (<reason>)`.
pub(super) type Miss = &'static str;

/// What the track menu calls a subtitle, read off the stream the way the menu reads it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct SubTraits {
    /// `Stream.forced` or the name says forced (`track_label::flags`).
    pub forced: bool,
    /// `Stream.sdh` or the name says SDH (`track_label::flags`).
    pub sdh: bool,
    /// The name says commentary (`track_label::Kind::Commentary`), whatever the flags say.
    pub commentary: bool,
}

/// The menu's own predicate for one subtitle stream (critique r5 M4): the merged name, parsed, with
/// the structured flags folded in. The one place "forced" is decided for memory.
pub(super) fn sub_traits(s: &Stream) -> SubTraits {
    // The container name arrives only once the demuxer has the file open (`TrackNames`), after the
    // resolve; here the PMS title is all there is.
    let name = track_label::track_name(&s.title, "", &s.lang);
    let label = track_label::parse(&name, &s.lang, s.forced, s.sdh);
    let (forced, sdh) = track_label::flags(&label, s.forced, s.sdh);
    SubTraits { forced, sdh, commentary: label.kind == Kind::Commentary }
}

/// A language tag that names no language: empty, or the ISO "undetermined".
fn blank_lang(l: &str) -> bool {
    let l = l.trim();
    l.is_empty() || l.eq_ignore_ascii_case("und")
}

/// Is a stream's `languageCode` the language the memo recorded? The memo's 639-2 code decides
/// (`lang_matches` folds `fre`/`fra`); only a memo with no code falls back to its BCP 47 tag. A
/// memo that names no language matches only a stream that names none either.
fn same_language(memo_lang: &str, memo_tag: &str, stream_code: &str) -> bool {
    let memo = if blank_lang(memo_lang) { memo_tag } else { memo_lang };
    if blank_lang(memo) {
        return blank_lang(stream_code);
    }
    lang_matches(memo, stream_code)
}

/// A soft field agrees when the memo says nothing about it or says what the stream has.
fn agrees(memo: &str, have: &str) -> bool {
    let memo = memo.trim();
    memo.is_empty() || memo.eq_ignore_ascii_case(have.trim())
}

/// A memo title is cut to its bound at write time, so a stream's longer title still agrees when it
/// starts with the memo's.
fn title_agrees(memo: &str, have: &str) -> bool {
    let memo = memo.trim();
    memo.is_empty() || have.trim().to_lowercase().starts_with(&memo.to_lowercase())
}

/// The best of `candidates` by `key` (larger wins), the EARLIEST on a tie: file order is the last
/// tiebreak.
fn best_by<K: PartialOrd>(candidates: impl Iterator<Item = usize>, key: impl Fn(usize) -> K) -> Option<usize> {
    let mut best: Option<(usize, K)> = None;
    for i in candidates {
        let k = key(i);
        if best.as_ref().is_none_or(|(_, held)| k > *held) {
            best = Some((i, k));
        }
    }
    best.map(|(i, _)| i)
}

/// **The audio track of this file a remembered audio pick means.** Hard: the language, the
/// commentary-ness (`commentary_of`, the planner's own notion: a commentary-titled track that is
/// the only one of its language is just its audio) and audio description. Soft, in order: codec,
/// channels, tag, title, the file's default, then file order.
///
/// `Err` names the first hard constraint no track satisfied.
pub(super) fn match_audio(
    tracks: &[Stream], memo: &AudioMemo, commentary_of: impl Fn(usize) -> bool,
) -> Result<usize, Miss> {
    let language: Vec<usize> =
        (0..tracks.len()).filter(|&i| same_language(&memo.lang, &memo.tag, &tracks[i].lang_code)).collect();
    if language.is_empty() {
        return Err("language");
    }
    let kind: Vec<usize> = language.into_iter().filter(|&i| commentary_of(i) == memo.commentary).collect();
    if kind.is_empty() {
        return Err("commentary");
    }
    let described: Vec<usize> = kind.into_iter().filter(|&i| tracks[i].ad == memo.ad).collect();
    if described.is_empty() {
        return Err("audio description");
    }
    let key = |i: usize| {
        let t = &tracks[i];
        (
            agrees(&memo.codec, &t.codec),
            memo.channels == 0 || i64::from(memo.channels) == t.channels,
            agrees(&memo.tag, &t.language_tag),
            title_agrees(&memo.title, &t.title),
            t.default,
        )
    };
    best_by(described.into_iter(), key).ok_or("language")
}

/// **The subtitle track of this file a remembered Track pick means.** `eligible` is what the caller
/// can carry at all (a stream it can draw). Hard: the language and `forced` ([`sub_traits`]). Soft,
/// in order: SDH, commentary-ness, text/image kind, tag, title, an embedded track before a sidecar,
/// then file order.
pub(super) fn match_subtitle(
    subs: &[Stream], memo: &SubtitleTrackMemo, eligible: impl Fn(&Stream) -> bool,
) -> Result<usize, Miss> {
    let language: Vec<usize> = (0..subs.len())
        .filter(|&i| eligible(&subs[i]) && same_language(&memo.lang, &memo.tag, &subs[i].lang_code))
        .collect();
    if language.is_empty() {
        return Err("language");
    }
    let forced: Vec<usize> = language.into_iter().filter(|&i| sub_traits(&subs[i]).forced == memo.forced).collect();
    if forced.is_empty() {
        return Err("forced");
    }
    let memo_commentary = track_label::is_commentary(&memo.title);
    let key = |i: usize| {
        let s = &subs[i];
        let traits = sub_traits(s);
        (
            traits.sdh == memo.sdh,
            traits.commentary == memo_commentary,
            agrees(&memo.kind, if is_image_sub_codec(&s.codec) { "image" } else { "text" }),
            agrees(&memo.tag, &s.language_tag),
            title_agrees(&memo.title, &s.title),
            !s.external,
        )
    };
    best_by(forced.into_iter(), key).ok_or("language")
}

/// **The forced track Forced only means**: a forced ([`sub_traits`]), non-commentary, `eligible`
/// subtitle in the language of the audio that is PLAYING (`audio_lang`), or in `memo_lang` when the
/// audio names no language. Soft, in order: not SDH, embedded before a sidecar, file order. `None`
/// when there is no such track: Forced only then shows NOTHING, it never falls to a full track.
pub(super) fn match_forced_only(
    subs: &[Stream], audio_lang: &str, memo_lang: &str, eligible: impl Fn(&Stream) -> bool,
) -> Option<usize> {
    let language = if blank_lang(audio_lang) { memo_lang } else { audio_lang };
    if blank_lang(language) {
        return None;
    }
    let candidates = (0..subs.len()).filter(|&i| {
        let traits = sub_traits(&subs[i]);
        eligible(&subs[i]) && traits.forced && !traits.commentary && lang_matches(language, &subs[i].lang_code)
    });
    best_by(candidates, |i| (!sub_traits(&subs[i]).sdh, !subs[i].external))
}

#[cfg(test)]
#[path = "track_memory_tests.rs"]
mod tests;
