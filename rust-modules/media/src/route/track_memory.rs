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
//!   soft constraints (codec, channels, tag, title, SDH, kind, the memo's own embedded-or-sidecar,
//!   file order) only choose between tracks that already satisfy the hard ones, and the first of
//!   the best wins.
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
//! * **When the container's names arrive** ([`with_container_names`]) the same matchers run a
//!   second time over streams whose `title` is the MERGED name (`track_label::track_name`), so a
//!   forced track only the demuxer's tag reveals (an MP4 part has no PMS `title`) is found by the
//!   one predicate and not a second copy of it. [`forced_only_pick`] is that answer for the menu's
//!   choice and the audio follow; [`rematch_with_names`] is the first decoded frame's.
//!
//! **The write half is here too** ([`memo_of_audio`], [`memo_of_subtitle`]): the descriptor a menu
//! pick is remembered as, built from the exact `Stream` the row was drawn from with the SAME
//! predicates the matchers read (`sub_traits`, `audio_is_commentary`), so a memo written from the
//! menu is a memo the matchers find the same track by. A builder that read the raw `Stream.forced`
//! instead would write `forced: false` for a track the menu badges FORCED by its name, and the
//! next episode's hard constraint would decline that very track.
use plx_data::metadata::sub_layout::is_image_sub_codec;
use plx_data::metadata::track_label::{self, Kind};
use plx_data::metadata::track_names::TrackNames;
use plx_data::metadata::{lang_matches, sub_render_ordinal, PlayingItem, Stream};
use plx_plex::plex::session::prefs::{AudioMemo, SubtitleMemo, SubtitleTrackMemo};

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

/// **What a pick of audio track `i` of `tracks` is remembered as**, or `None` for an index that
/// names no track. The commentary-ness is the planner's own notion ([`super::plan::audio_is_commentary`]:
/// titled one AND not the only track of its language), the same one [`match_audio`] is handed.
pub fn memo_of_audio(tracks: &[Stream], i: usize) -> Option<AudioMemo> {
    let s = tracks.get(i)?;
    Some(AudioMemo {
        lang: s.lang_code.clone(),
        tag: s.language_tag.clone(),
        commentary: super::plan::audio_is_commentary(tracks, i),
        ad: s.ad,
        codec: s.codec.clone(),
        // A count over the memo's bound reads as unknown (0) when the entry is normalised.
        channels: u8::try_from(s.channels).unwrap_or(0),
        title: s.title.clone(),
    })
}

/// **What a pick of the subtitle track `s` is remembered as** (Off is [`SubtitleMemo::Off`], which
/// needs no stream). `forced` and `sdh` are the menu's own reading ([`sub_traits`]), not the raw
/// flags; `external` is the stream's, so a sidecar is told from an embedded track of the same name.
pub fn memo_of_subtitle(s: &Stream) -> SubtitleMemo {
    let traits = sub_traits(s);
    SubtitleMemo::Track(SubtitleTrackMemo {
        lang: s.lang_code.clone(),
        tag: s.language_tag.clone(),
        forced: traits.forced,
        sdh: traits.sdh,
        kind: if is_image_sub_codec(&s.codec) { "image" } else { "text" }.to_string(),
        external: s.external,
        title: s.title.clone(),
    })
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
/// in order: SDH, commentary-ness, text/image kind, tag, title, the memo's own kind of file (an
/// embedded track for an embedded memo, a sidecar for an external one: `memo.external`), then file
/// order. A caller that can carry only embedded tracks for an embedded memo never sees a sidecar;
/// one that can carry a sidecar for an external memo gets the sidecar before an embedded track of
/// the same language.
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
            s.external == memo.external,
        )
    };
    best_by(forced.into_iter(), key).ok_or("language")
}

/// **The language Forced only matches in**: the playing audio's, or `fallback` (the one the memo, or
/// the row's last choice, recorded) when the audio names none (empty, or the ISO "und"). The one
/// rule of the resolve, the audio follow and the Subtitles menu, which also remembers and reads out
/// this language.
pub fn forced_only_language<'a>(audio_lang: &'a str, fallback: &'a str) -> &'a str {
    if blank_lang(audio_lang) { fallback } else { audio_lang }
}

/// **The forced track Forced only means**: a forced ([`sub_traits`]), non-commentary, `eligible`
/// subtitle in the language of the audio that is PLAYING (`audio_lang`), or in `memo_lang` when the
/// audio names no language. Soft, in order: not SDH, embedded before a sidecar, file order. `None`
/// when there is no such track: Forced only then shows NOTHING, it never falls to a full track.
pub(super) fn match_forced_only(
    subs: &[Stream], audio_lang: &str, memo_lang: &str, eligible: impl Fn(&Stream) -> bool,
) -> Option<usize> {
    let language = forced_only_language(audio_lang, memo_lang);
    if blank_lang(language) {
        return None;
    }
    let candidates = (0..subs.len()).filter(|&i| {
        let traits = sub_traits(&subs[i]);
        eligible(&subs[i]) && traits.forced && !traits.commentary && lang_matches(language, &subs[i].lang_code)
    });
    best_by(candidates, |i| (!sub_traits(&subs[i]).sdh, !subs[i].external))
}

/// **Which of the three subtitle states the playing item is in** (`track-memory-plan.md` §4.9).
/// `cur_sub_sid` alone cannot tell Forced only from a pick of the very forced track it drew, and
/// the two part ways the moment the audio changes: a track stays, Forced only follows the audio's
/// language.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SubMode {
    /// A track is on screen because it was picked (the viewer's, a retry's, the ladder's).
    #[default]
    Track,
    /// Nothing is on screen because Off was chosen or decided.
    Off,
    /// The forced track in the language of the audio that is playing, or nothing.
    ForcedOnly,
}

impl SubMode {
    /// The mode an ORDINARY pick of `stream_id` (`0` = Off) puts the playback in. A derived pick
    /// (the audio follow, the names re-match) puts it back to [`SubMode::ForcedOnly`] itself.
    pub fn of_stream(stream_id: i64) -> Self {
        if stream_id == 0 { SubMode::Off } else { SubMode::Track }
    }
}

/// A subtitle memo consulted at a direct-play landing whose answer may change once the container's
/// names are known ([`rematch_with_names`]).
#[derive(Clone, Debug, PartialEq)]
pub struct Rematch {
    pub(super) memo: SubtitleMemo,
    /// The subtitle stream id the landing put on screen (`0` = none). The re-match runs only while
    /// that is still what is shown: a viewer who picked meanwhile has decided.
    pub(super) landed_sub: i64,
}

/// **What the two re-matches that run AFTER the landing need of the playing item**, parked on the
/// session at the landing. Neither has the item: the audio follow runs inside
/// `commit_audio_selection(ps, audio)` and the container-name re-match inside the pump, and the
/// metadata store that holds the item is not theirs to read. The streams are a small clone, taken
/// once per landing; the session's published copy leaves it out.
#[derive(Clone, Default)]
pub struct TrackContext {
    pub(super) subs: Vec<Stream>,
    /// `(stream id, language code)` of every audio stream: a new audio's language by its id.
    pub(super) audio: Vec<(i64, String)>,
    /// The language of the audio that played at the landing, for an audio the list cannot name.
    pub(super) audio_lang: String,
    /// The language Forced only falls back to when the audio names none: the one the memo (or the
    /// row's choice) recorded.
    pub(super) forced_lang: String,
    /// Owed once, at the first decoded frame of a direct play; `None` after, or when no remembered
    /// track decided the landing.
    pub(super) rematch: Option<Rematch>,
}

impl TrackContext {
    /// The context of a landing of `item` under `audio_lang`, with `memo` the remembered subtitle
    /// the ladder consulted (`None` for a retry, a server-selected sidecar or no memory).
    pub(super) fn of_landing(item: &PlayingItem, audio_lang: &str, memo: Option<&SubtitleMemo>) -> Self {
        let forced_lang = match memo {
            Some(SubtitleMemo::ForcedOnly { lang }) => lang.clone(),
            _ => String::new(),
        };
        TrackContext {
            subs: item.subs.clone(),
            audio: item.audio.iter().map(|s| (s.id, s.lang_code.clone())).collect(),
            audio_lang: audio_lang.to_string(),
            forced_lang,
            rematch: None,
        }
    }

    /// Keep the fallback language the outgoing landing held, for a RETRY of Forced only: it names
    /// its stream by id and consults no memory, so the memo that supplied the language is not in the
    /// resolve. A language this landing already has (from the memo it consulted) wins.
    pub(super) fn keep_forced_lang(&mut self, carried: &str) {
        if self.forced_lang.is_empty() {
            self.forced_lang = carried.to_string();
        }
    }

    /// Owe the first-frame re-match for `memo` when it is one the names can change: a Track or a
    /// Forced only (Off has nothing to find). `landed_sub` is what the landing shows.
    pub(super) fn owe_rematch(&mut self, memo: Option<&SubtitleMemo>, landed_sub: i64) {
        self.rematch = memo
            .filter(|m| !matches!(m, SubtitleMemo::Off))
            .map(|memo| Rematch { memo: memo.clone(), landed_sub });
    }

    /// The language of audio stream `sid`, or the landing's audio when the list does not name it.
    pub(super) fn audio_lang_of(&self, sid: i64) -> &str {
        self.audio.iter().find(|(id, _)| *id == sid).map_or(self.audio_lang.as_str(), |(_, l)| l.as_str())
    }
}

/// The subtitles of `subs` with each `title` replaced by the MERGED track name
/// (`track_label::track_name`: the PMS title, else the container's tag for its ordinal). The
/// matchers read a name through [`sub_traits`] alone, so handing them these streams is the whole of
/// making them names-aware: one forced/SDH/commentary predicate, not two.
fn with_container_names(subs: &[Stream], names: &TrackNames) -> Vec<Stream> {
    subs.iter()
        .enumerate()
        .map(|(i, s)| Stream {
            title: track_label::track_name(&s.title, names.sub(sub_render_ordinal(subs, i)), &s.lang),
            ..s.clone()
        })
        .collect()
}

/// The subtitle a resolve of this app can draw: embedded and a codec the demuxer renders. What
/// `plan`'s memory rung matches against, and so what every later re-match of it does.
fn drawable(s: &Stream) -> bool {
    super::plan::drawable_embedded(s)
}

/// **The forced track Forced only means, with the container's names**: an index into `subs`, or
/// `None` for nothing. The menu's choice (the row's read-out and what OK commits) and the audio
/// follow use this; the resolve, which has no names yet, uses [`match_forced_only`] directly.
pub fn forced_only_pick(subs: &[Stream], names: &TrackNames, audio_lang: &str, memo_lang: &str) -> Option<usize> {
    match_forced_only(&with_container_names(subs, names), audio_lang, memo_lang, drawable)
}

/// What a re-match with the container's names concluded, as the stream to show.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Rematched {
    /// The stream id to show (`0` = none) and its demuxer ordinal (`-1` = none).
    pub id: i64,
    pub ordinal: i32,
}

/// **The first-frame re-match** (`track-memory-plan.md` §4.9): what `memo` picks over streams the
/// container's names have now been joined to, when that differs from `landed_sub`, what the landing
/// showed. A Forced only memo can come to nothing (`id` 0); a Track memo that still matches nothing
/// leaves the landing as it was. `audio_lang` is the language of the audio that plays.
pub(super) fn rematch_with_names(
    subs: &[Stream], names: &TrackNames, memo: &SubtitleMemo, audio_lang: &str, landed_sub: i64,
) -> Option<Rematched> {
    let named = with_container_names(subs, names);
    let picked = match memo {
        SubtitleMemo::Off => return None,
        SubtitleMemo::ForcedOnly { lang } => Some(match_forced_only(&named, audio_lang, lang, drawable)),
        SubtitleMemo::Track(track) => match_subtitle(&named, track, drawable).ok().map(Some),
    }?;
    let (id, ordinal) = picked
        .map(|i| (subs[i].id, sub_render_ordinal(subs, i)))
        .filter(|&(id, ordinal)| id > 0 && ordinal >= 0)
        .unwrap_or((0, -1));
    (id != landed_sub).then_some(Rematched { id, ordinal })
}

#[cfg(test)]
#[path = "track_memory_tests.rs"]
mod tests;
