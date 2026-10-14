//! **The track-memory ladder, pure**: what [`pick_subtitle`] and [`audio_intents`] decide for a
//! file, a record and the rules below it (`track-memory-plan.md`, Priority schema). The same rows
//! graded through `build_stream`, with the route and the write-back provenance, are in
//! `decision_track_memory_tests`.

use super::*;
#[allow(unused_imports)]
use super::test_support::*;
use plx_data::metadata::{PlayingItem, Stream};
use plx_plex::plex::session::prefs::SubtitleTrackMemo;

/// An embedded text subtitle; `index == id`, so the ordinals read 0, 1, 2 in id order.
fn text_sub(id: i64, lang: &str, title: &str) -> Stream {
    Stream { title: title.into(), ..sub(id, id, lang, false) }
}

fn forced(mut s: Stream) -> Stream {
    s.forced = true;
    s
}

fn item(kind: &str, show_rk: &str, subs: Vec<Stream>) -> PlayingItem {
    PlayingItem { kind: kind.into(), show_rk: show_rk.into(), ..PlayingItem::with_subs(subs) }
}

fn episode(subs: Vec<Stream>) -> PlayingItem {
    item("episode", "show-1", subs)
}

fn track_memo(lang: &str) -> SubtitleMemo {
    SubtitleMemo::Track(SubtitleTrackMemo { lang: lang.into(), kind: "text".into(), ..Default::default() })
}

const NO_SHOW_RULES: fn() -> plx_plex::plex::ShowLangPrefs = plx_plex::plex::ShowLangPrefs::default;

/// The ladder with no account rule and no show rule: only memory and PMS's selection speak.
fn ladder(item: &PlayingItem, audio_lang: &str, memory: Option<&SubtitleMemo>) -> SubDecision {
    pick_subtitle(item, &NO_SHOW_RULES(), SubtitleLangPrefs::default(), audio_lang, None, memory)
}

/// "Always show, English" (account mode 2): what today's ladder would turn on by itself.
fn always_english() -> SubtitleLangPrefs<'static> {
    SubtitleLangPrefs { language: Some("eng"), mode: 2, forced: 0 }
}

// ---- subtitle rung 3: a remembered track -------------------------------------------------------

/// Q1: PMS's selected flag cannot tell an account auto-pick from a deliberate one, so a remembered
/// pick outranks it. The control row is today's pick.
#[test]
fn a_remembered_track_beats_the_pms_embedded_selection() {
    let item = episode(vec![text_sub(1, "eng", ""), server_selected(text_sub(2, "spa", ""))]);
    assert_eq!(
        ladder(&item, "jpn", None),
        SubDecision { pick: Some((2, 1)), source: SubPickSource::Server, miss: None },
        "control: with no record PMS's selection plays",
    );
    assert_eq!(
        ladder(&item, "jpn", Some(&track_memo("eng"))),
        SubDecision { pick: Some((1, 0)), source: SubPickSource::Memory, miss: None },
    );
}

/// A record that matches no track of this file DECLINES: the ladder falls to today's pick, never to
/// Off, and says which hard constraint failed.
#[test]
fn a_record_that_matches_nothing_gives_todays_pick() {
    let item = episode(vec![text_sub(1, "eng", ""), server_selected(text_sub(2, "spa", ""))]);
    let declined = ladder(&item, "jpn", Some(&track_memo("fra")));
    assert_eq!(declined.pick, Some((2, 1)), "PMS's selection, as without a record");
    assert_eq!(declined.source, SubPickSource::Server);
    assert_eq!(declined.miss, Some("language"));
    // the account's rule is what plays when PMS selected nothing
    let item = episode(vec![text_sub(1, "eng", ""), text_sub(2, "spa", "")]);
    let from_mode = pick_subtitle(
        &item, &NO_SHOW_RULES(), always_english(), "jpn", None, Some(&track_memo("fra")),
    );
    assert_eq!((from_mode.pick, from_mode.source, from_mode.miss), (Some((1, 0)), SubPickSource::Account, Some("language")));
}

/// `forced` is hard in both directions: a full-track record never lands on the forced track.
#[test]
fn the_forced_flag_is_hard() {
    let item = episode(vec![forced(text_sub(1, "eng", ""))]);
    let declined = ladder(&item, "eng", Some(&track_memo("eng")));
    assert_eq!((declined.pick, declined.miss), (None, Some("forced")), "the only English track is forced; the record is the full one");
    let forced_memo = SubtitleMemo::Track(SubtitleTrackMemo { lang: "eng".into(), forced: true, ..Default::default() });
    assert_eq!(ladder(&item, "eng", Some(&forced_memo)).pick, Some((1, 0)));
}

// ---- rung 3a: Off is absolute ------------------------------------------------------------------

/// The account says "English, always" and would turn a track on; the viewer's Off wins.
#[test]
fn a_remembered_off_beats_account_mode_2() {
    let item = episode(vec![text_sub(1, "eng", "")]);
    let account = always_english();
    assert_eq!(
        pick_subtitle(&item, &NO_SHOW_RULES(), account, "jpn", None, None).pick,
        Some((1, 0)),
        "control: the account rule turns English on",
    );
    assert_eq!(
        pick_subtitle(&item, &NO_SHOW_RULES(), account, "jpn", None, Some(&SubtitleMemo::Off)),
        SubDecision { pick: None, source: SubPickSource::Memory, miss: None },
    );
}

/// Off is absolute (owner): a forced track PMS selected and the account's forced-only setting both
/// stay hidden. Nothing falls through to them.
#[test]
fn a_remembered_off_hides_a_pms_selected_forced_track_and_the_accounts_forced_only() {
    let item = episode(vec![server_selected(forced(text_sub(1, "eng", ""))), text_sub(2, "eng", "")]);
    let account = SubtitleLangPrefs { language: Some("eng"), mode: 1, forced: 2 };
    let control = pick_subtitle(&item, &NO_SHOW_RULES(), account, "jpn", None, None);
    assert_eq!((control.pick, control.source), (Some((1, 0)), SubPickSource::Server), "control: PMS's forced track plays");
    let off = pick_subtitle(&item, &NO_SHOW_RULES(), account, "jpn", None, Some(&SubtitleMemo::Off));
    assert_eq!((off.pick, off.source), (None, SubPickSource::Memory));
}

// ---- rung 3b: Forced only ----------------------------------------------------------------------

#[test]
fn forced_only_shows_the_forced_track_in_the_language_of_the_audio_that_plays() {
    let item = episode(vec![
        text_sub(1, "eng", ""),
        forced(text_sub(2, "eng", "")),
        forced(text_sub(3, "spa", "")),
    ]);
    let memo = SubtitleMemo::ForcedOnly { lang: String::new() };
    assert_eq!(ladder(&item, "eng", Some(&memo)).pick, Some((2, 1)));
    assert_eq!(ladder(&item, "spa", Some(&memo)).pick, Some((3, 2)), "an audio switch re-points it");
    let decision = ladder(&item, "eng", Some(&memo));
    assert_eq!(decision.source, SubPickSource::Memory);
}

/// With no forced track in the audio's language Forced only shows NOTHING: not the full track, and
/// not the account's rule, which would have turned the full English track on.
#[test]
fn forced_only_with_no_forced_track_shows_nothing_and_never_falls_through() {
    let item = episode(vec![text_sub(1, "eng", ""), forced(text_sub(2, "spa", ""))]);
    let memo = SubtitleMemo::ForcedOnly { lang: String::new() };
    let decision = pick_subtitle(&item, &NO_SHOW_RULES(), always_english(), "eng", None, Some(&memo));
    assert_eq!(decision, SubDecision { pick: None, source: SubPickSource::Memory, miss: None });
}

/// The menu's own predicate: a track NAMED Forced is forced with the flag unset, and a commentary
/// is never the forced track whatever its flag.
#[test]
fn forced_only_uses_the_menus_forced_predicate_and_skips_a_commentary() {
    let memo = SubtitleMemo::ForcedOnly { lang: String::new() };
    let titled = episode(vec![text_sub(1, "eng", "English"), text_sub(2, "eng", "Forced")]);
    assert_eq!(ladder(&titled, "eng", Some(&memo)).pick, Some((2, 1)), "title-only forced");
    let commentary = episode(vec![forced(text_sub(1, "eng", "Director's Commentary")), text_sub(2, "eng", "")]);
    assert_eq!(ladder(&commentary, "eng", Some(&memo)).pick, None, "a flagged commentary is not the forced track");
}

/// The memo's language stands in only for audio that names none.
#[test]
fn forced_only_falls_back_to_the_memos_language_for_untagged_audio() {
    let item = episode(vec![forced(text_sub(1, "eng", "")), forced(text_sub(2, "spa", ""))]);
    let memo = SubtitleMemo::ForcedOnly { lang: "spa".into() };
    assert_eq!(ladder(&item, "", Some(&memo)).pick, Some((2, 1)));
    assert_eq!(ladder(&item, "eng", Some(&memo)).pick, Some((1, 0)), "tagged audio wins");
}

// ---- the rungs above memory --------------------------------------------------------------------

/// Rung 2: a subtitle the SERVER selected and the app draws itself. Memory is not consulted (a
/// record that matches nothing leaves no `miss`), and moving the selection off a downloaded
/// subtitle makes PMS delete it, so nothing may displace it.
#[test]
fn a_server_selected_sidecar_beats_memory() {
    let sidecar = Stream { key: "/library/streams/9".into(), selected: true, ..sub(9, 9, "eng", true) };
    let item = episode(vec![text_sub(1, "eng", ""), sidecar]);
    for memo in [track_memo("eng"), track_memo("fra"), SubtitleMemo::Off, SubtitleMemo::ForcedOnly { lang: String::new() }] {
        let decision = ladder(&item, "jpn", Some(&memo));
        assert_eq!(
            decision,
            SubDecision { pick: None, source: SubPickSource::Server, miss: None },
            "{memo:?}: the sidecar stays; the landing restores it",
        );
    }
}

/// Rung 1: this playback's own pick (a known retry) is the viewer's latest word.
#[test]
fn this_playbacks_own_pick_beats_memory() {
    let item = episode(vec![text_sub(1, "eng", ""), text_sub(2, "spa", "")]);
    let memo = track_memo("eng");
    let off = pick_subtitle(&item, &NO_SHOW_RULES(), SubtitleLangPrefs::default(), "jpn", Some(0), Some(&memo));
    assert_eq!(off, SubDecision { pick: None, source: SubPickSource::Override, miss: None });
    let spanish = pick_subtitle(&item, &NO_SHOW_RULES(), SubtitleLangPrefs::default(), "jpn", Some(2), Some(&memo));
    assert_eq!((spanish.pick, spanish.source), (Some((2, 1)), SubPickSource::Override));
}

/// Without a record the ladder is exactly today's, and says whose rule decided.
#[test]
fn without_a_record_the_ladder_is_todays() {
    let item = episode(vec![text_sub(1, "eng", ""), server_selected(text_sub(2, "spa", ""))]);
    assert_eq!(ladder(&item, "jpn", None).source, SubPickSource::Server);
    let none_selected = episode(vec![text_sub(1, "eng", "")]);
    let show = plx_plex::plex::ShowLangPrefs { audio: None, subtitle: Some("eng".into()), subtitle_mode: 2 };
    let shown = pick_subtitle(&none_selected, &show, SubtitleLangPrefs::default(), "jpn", None, None);
    assert_eq!((shown.pick, shown.source), (Some((1, 0)), SubPickSource::Show));
    let account = pick_subtitle(&none_selected, &NO_SHOW_RULES(), always_english(), "jpn", None, None);
    assert_eq!((account.pick, account.source), (Some((1, 0)), SubPickSource::Account));
    let off = ladder(&none_selected, "jpn", None);
    assert_eq!((off.pick, off.source, off.miss), (None, SubPickSource::Account, None));
}

// ---- the gate ----------------------------------------------------------------------------------

fn entry_for(show_rk: &str) -> TrackMemoryEntry {
    TrackMemoryEntry::new("machine", show_rk, None, Some(SubtitleMemo::Off))
}

/// Music and movies get nothing, and so does an episode that names no show: the memory is per SHOW.
#[test]
fn memory_is_for_episodes_of_a_known_show_only() {
    let memory = [entry_for("show-1")];
    assert!(show_memory(&memory, &item("episode", "show-1", Vec::new())).is_some());
    assert!(show_memory(&memory, &item("movie", "show-1", Vec::new())).is_none(), "a movie");
    assert!(show_memory(&memory, &item("track", "show-1", Vec::new())).is_none(), "music");
    assert!(show_memory(&memory, &item("clip", "show-1", Vec::new())).is_none(), "a trailer");
    assert!(show_memory(&memory, &item("", "show-1", Vec::new())).is_none(), "a server that sent no type");
    assert!(show_memory(&memory, &item("episode", "", Vec::new())).is_none(), "an episode of no known show");
    assert!(show_memory(&memory, &item("episode", "show-2", Vec::new())).is_none(), "another show");
}

// ---- the audio rung ----------------------------------------------------------------------------

fn audio(id: i64, lang: &str, codec: &str, selected: bool, default: bool) -> Stream {
    Stream { selected, ..trk(id, codec, lang, default) }
}

/// Audio rung 2 sits right after this playback's own pick and above PMS's selection, the show's
/// language and the account's.
#[test]
fn a_remembered_audio_track_outranks_pms_and_the_language_rules() {
    let tracks = [audio(1, "eng", "eac3", false, true), audio(2, "jpn", "eac3", false, false), audio(3, "fra", "eac3", true, false)];
    let prefs = AudioLangPrefs { remembered: Some(1), ..AudioLangPrefs::new(Some("fr"), Some("fr")) };
    assert_eq!(
        audio_intents(&tracks, prefs, 0)[..2],
        [AudioIntent::Selection(1), AudioIntent::Selection(2)],
        "the remembered track first, PMS's selected non-default second",
    );
    assert_eq!(pick_dp_audio_pref(&tracks, "eac3", prefs), Some((1, "eac3".into(), 2)));
    assert_eq!(
        audio_intents(&tracks, prefs, 3)[0],
        AudioIntent::Selection(2),
        "this playback's own pick is still first",
    );
    let unremembered = AudioLangPrefs::new(Some("fr"), Some("fr"));
    assert_eq!(pick_dp_audio_pref(&tracks, "eac3", unremembered), Some((2, "eac3".into(), 3)), "control: PMS's selection plays");
}

/// An index that is not in the track list (a stale resolve) is ignored, not trusted.
#[test]
fn a_remembered_index_outside_the_list_is_ignored() {
    let tracks = [audio(1, "eng", "eac3", false, true)];
    let prefs = AudioLangPrefs { remembered: Some(7), ..AudioLangPrefs::default() };
    assert_eq!(audio_intents(&tracks, prefs, 0), [AudioIntent::FileDefault]);
}

/// The re-encode and remux walk the SAME ranking, so the remembered track is also the one a server
/// conversion is asked for.
#[test]
fn the_remembered_audio_track_is_what_a_conversion_names() {
    let tracks = [audio(1, "eng", "dca", false, true), audio(2, "jpn", "dca", false, false)];
    let prefs = AudioLangPrefs { remembered: Some(1), ..AudioLangPrefs::default() };
    assert_eq!(encode_audio_id(false, 0, 0, &tracks, prefs), 2);
    assert_eq!(encode_audio_id(false, 0, 0, &tracks, AudioLangPrefs::default()), 1, "control: the file's default");
}

// ---- the log -----------------------------------------------------------------------------------

/// The line is the plan's grammar (`track-memory-plan.md` §4.2 item 6): the VERB first, the half
/// after it, so the substrings the Device step, FO's check and C3b's acceptance search for
/// (`track-memory: applied`, `track-memory: applied forced-only`, `track-memory: no match
/// (language)`, `track-memory: not shown (copy remux)`) are really in it.
#[test]
fn the_log_line_says_what_each_half_came_to() {
    let tracks = [audio(41, "jpn", "eac3", false, false)];
    let applied = SubDecision { pick: Some((7, 2)), source: SubPickSource::Memory, miss: None };
    let log = |memo: &SubtitleMemo, d: &SubDecision, audio: Option<&Result<usize, Miss>>| {
        memory_log(audio, &tracks, Some(memo), Some(d))
    };
    assert_eq!(log(&track_memo("eng"), &applied, None).as_deref(), Some("track-memory: applied subtitle (track 7)"));
    let off = SubDecision { pick: None, source: SubPickSource::Memory, miss: None };
    assert_eq!(log(&SubtitleMemo::Off, &off, None).as_deref(), Some("track-memory: applied off"));
    let forced_only = SubtitleMemo::ForcedOnly { lang: String::new() };
    assert_eq!(log(&forced_only, &off, None).as_deref(), Some("track-memory: applied forced-only:none"));
    assert_eq!(log(&forced_only, &applied, None).as_deref(), Some("track-memory: applied forced-only (track 7)"));
    let declined = SubDecision { pick: None, source: SubPickSource::Account, miss: Some("language") };
    assert_eq!(
        log(&track_memo("fra"), &declined, None).as_deref(),
        Some("track-memory: no match (language) subtitle"),
    );
    assert_eq!(memory_log(Some(&Ok(0)), &tracks, None, None).as_deref(), Some("track-memory: applied audio (track 41, jpn)"));
    assert_eq!(
        memory_log(Some(&Err("commentary")), &tracks, None, None).as_deref(),
        Some("track-memory: no match (commentary) audio"),
    );
    let higher = SubDecision { pick: Some((3, 1)), source: SubPickSource::Server, miss: None };
    assert_eq!(
        log(&track_memo("eng"), &higher, None).as_deref(),
        Some("track-memory: not used (a higher rung decided) subtitle"),
    );
    assert_eq!(memory_log(None, &tracks, None, None), None, "nothing remembered, nothing logged");
}

/// Both halves in one line: whatever APPLIED leads (so `track-memory: applied` is the line's prefix
/// whenever either half applied, and so is `applied forced-only` when Forced only applied, audio or
/// no audio), then what did not.
#[test]
fn applied_halves_lead_the_line() {
    let tracks = [audio(41, "jpn", "eac3", false, false)];
    let applied = SubDecision { pick: Some((7, 2)), source: SubPickSource::Memory, miss: None };
    let declined = SubDecision { pick: None, source: SubPickSource::Account, miss: Some("language") };
    let forced_only = SubtitleMemo::ForcedOnly { lang: String::new() };
    let line = |audio: Option<&Result<usize, Miss>>, memo: &SubtitleMemo, d: &SubDecision| {
        memory_log(audio, &tracks, Some(memo), Some(d)).expect("something was remembered")
    };

    let both = line(Some(&Ok(0)), &track_memo("eng"), &applied);
    assert_eq!(both, "track-memory: applied subtitle (track 7); applied audio (track 41, jpn)");
    let forced = line(Some(&Ok(0)), &forced_only, &applied);
    assert!(forced.starts_with("track-memory: applied forced-only"), "the FO check greps this: {forced}");

    let audio_only = line(Some(&Ok(0)), &track_memo("fra"), &declined);
    assert_eq!(audio_only, "track-memory: applied audio (track 41, jpn); no match (language) subtitle");
    let subtitle_only = line(Some(&Err("commentary")), &track_memo("eng"), &applied);
    assert_eq!(subtitle_only, "track-memory: applied subtitle (track 7); no match (commentary) audio");
    for l in [&both, &forced, &audio_only, &subtitle_only] {
        assert!(l.starts_with("track-memory: applied"), "the Device step greps this: {l}");
    }

    let neither = line(Some(&Err("commentary")), &track_memo("fra"), &declined);
    assert_eq!(neither, "track-memory: no match (language) subtitle; no match (commentary) audio");
    assert!(neither.starts_with("track-memory: no match (language)"), "the plan's wording for a decline: {neither}");
}

/// The route's half of the line: a remembered subtitle the route draws nothing of (C3b will burn it
/// on a re-encode, so only the copy remux stays), with the wording C3b's acceptance searches for.
#[test]
fn the_route_says_when_a_remembered_subtitle_is_not_shown() {
    assert_eq!(memory_not_shown_log(55001, true), "track-memory: not shown (copy remux) subtitle 55001");
    assert_eq!(memory_not_shown_log(55001, false), "track-memory: not shown (re-encode) subtitle 55001");
}
