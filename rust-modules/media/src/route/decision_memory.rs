//! **The track memory's effects, as a child of [`super`]** (the session-bound half of the memory
//! whose pure half is [`super::super::track_memory`] and [`super::super::plan`]): what the playing
//! session recalls and remembers, and tells the server about a remembered choice.
//!
//! * **Scope and recall**: [`memory_scope`] (whose memory) and the two reads a resolve takes
//!   ([`recalled_track_memory`], [`remembered_subtitle_offset`]) with the offset's one write
//!   ([`persist_subtitle_offset`]).
//! * **The write-back** (W6, WB): the remembered choice a landing applied that the server does not
//!   have yet, parked on the session and sent once on the first decoded frame
//!   ([`ParkedWriteBack`], [`send_parked_write_back`], [`on_decoded_frame`]).
//! * **The picks** the track menu reports, which are remembered when the route took them
//!   ([`commit_audio_pick`], [`commit_subtitle_pick`], [`commit_forced_only_pick`]), and the commits
//!   the playback derives from the Forced only mode, which remember nothing
//!   ([`follow_audio_with_forced_only`], [`rematch_subtitle_with_names`]).
//!
//! The `PlaybackSession` fields these read and write stay in `decision.rs`, where the struct is
//! declared; this file sees them as a child module of it.

use super::*;
use plx_plex::plex::session::prefs::{AudioMemo, SubtitleMemo, TrackMemoryEntry};
use plx_plex::plex::session::SubtitleOffsetEntry;
use super::super::track_memory::{forced_only_shown, memory_show, rematch_with_names, subtitle_pick_changed};

/// **Whose memory this is**: the active profile's key and the machine id of `sid`'s server, the two
/// halves of every key of the memory door. `None` until the server is registered and has said its
/// machine id (a blank one names no server, so nothing could be found or kept under it).
fn memory_scope(sid: ServerId) -> Option<(String, String)> {
    let machine_id = plx_plex::plex::client_for(sid)?.machine_id().to_string();
    if machine_id.is_empty() {
        return None;
    }
    Some((plx_plex::plex::session::current_profile_key(), machine_id))
}

/// **Everything the active profile remembered about its shows on `sid`'s server** (the track
/// memory), through the memory door's mirror. MAIN THREAD, like the rest of the snapshot: the
/// resolve worker must not read the mirror. A play raised off a shelf does not know its show until
/// the item is fetched, so the whole list rides along and `build_stream` picks the entry by
/// `playing.show_rk`. Empty until the server is registered and has said its machine id.
pub(super) fn recalled_track_memory(sid: ServerId) -> Vec<TrackMemoryEntry> {
    let Some((profile, machine_id)) = memory_scope(sid) else {
        return Vec::new();
    };
    recall_all::<TrackMemoryEntry>(&profile)
        .into_iter()
        .filter(|e| e.machine_id == machine_id)
        .collect()
}

/// This profile's remembered subtitle sync-timing correction for `rk` on `sid`'s server, if this
/// television has ever tuned one — the read half of [`persist_subtitle_offset`], through the memory
/// door's mirror ([`recall`]), so a correction made a moment ago is found whether or not its file
/// write has landed yet. `None` when the server is not (yet) registered, which a resolve about to
/// fail for the same reason would find out anyway.
pub(super) fn remembered_subtitle_offset(sid: ServerId, rk: &str) -> Option<i64> {
    let (profile, machine_id) = memory_scope(sid)?;
    recall::<SubtitleOffsetEntry>(&profile, &(machine_id, rk.to_string())).map(|e| e.offset_ms)
}

/// Remember the viewer's subtitle sync-timing correction for the CURRENTLY PLAYING item, so the
/// next resume of this exact file starts from where they left it (`remembered_subtitle_offset`'s
/// own doc). It goes through the memory door ([`remember`]): the mirror takes it now, on this
/// thread, and the file write is queued on the storage worker like every other in-player
/// preference edit (`player::set_subtitle_tone`) — the live atomic
/// ([`crate::player::set_subtitle_offset`]) is already applied by the time this queues, so a slow
/// or lost write costs only a restart's worth of memory, never this resume.
pub fn persist_subtitle_offset(ps: &PlaybackSession, offset_ms: i64) {
    let Some((user, machine_id)) = memory_scope(cur_sid(ps)) else {
        return;
    };
    let rk = cur_rk(ps);
    let key = (machine_id.clone(), rk.clone());
    // Only the offset is the viewer's pick: whatever else the entry carries (a key a newer build
    // wrote) is the list's, so it is taken from the entry that is there when this runs.
    remember::<SubtitleOffsetEntry>(&user, key, move |current| {
        (offset_ms != 0).then(|| SubtitleOffsetEntry {
            machine_id: machine_id.clone(),
            rating_key: rk.clone(),
            offset_ms,
            extensions: current.map(|e| e.extensions.clone()).unwrap_or_default(),
        })
    });
}

/// **A remembered choice a landing applied and the server has not been told** (W6), parked on
/// the session until the landing's first decoded frame. Everything the send needs rides here
/// because the playing item moves into the metadata store at the landing: the identity of the
/// route it belongs to, the halves (already filtered by W7 against the Part's own streams) and the
/// selection the viewer was looking at when it landed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(in super::super) struct ParkedWriteBack {
    sid: ServerId,
    part_id: i64,
    /// The landing's stream URL: the parked write belongs to THAT direct play. A recovery flight, a
    /// quality change or a rebuild that replaced the route in place does not run [`apply_plan`], so
    /// it cannot clear this; the URL (and `is_transcoding`) is how the send knows it is stale.
    url: String,
    audio: Option<i64>,
    subtitle: Option<SubWrite>,
    /// The subtitle the Part already has selected for the viewer (resolve time), if any. An
    /// audio-only write names it unchanged: re-selecting it moves nothing and keeps a downloaded
    /// stream alive (W5), and with none selected the field is omitted so no explicit Off is created.
    keep_subtitle: Option<i64>,
    /// The Part carries an external stream (W5), so a subtitle half is withheld whatever the
    /// subtitle on screen becomes; `subtitle: None` alone cannot say that (a declined memory and a
    /// half PMS already agrees with are `None` too).
    external_on_part: bool,
    /// `(audio, subtitle)` stream ids the viewer had on screen at the landing. Any difference at
    /// the first frame means a pick (or a recovery) took over, and that pick's own PUT carries the
    /// live selection, so there is nothing left to write.
    ///
    /// Read from the SESSION once the landing is complete ([`settle_landing_selection`]), never from
    /// the plan: the landing restores the server's selected sidecar over the plan's `sub_sid`, and
    /// on a direct play that is not a remux (Direct Play = Forced, a Dolby Vision base layer that
    /// cannot stand alone) the plan never names it.
    landed: (i64, i64),
}

/// What [`apply_plan`] parks for `plan` (W6, W7): `plan.write_back` less the halves PMS already
/// agrees with, `None` for a plan that is not a playable landing or has nothing left to write.
///
/// W7 compares each half with the Part's streams in `plan.playing`. An `Id` half is skipped only
/// when that stream is already `selected` AND the playing item was fetched fresh this resolve
/// (`plan.playing_fresh`): a cached detail page may predate an earlier write, and re-sending the
/// selection PMS holds is harmless. An `Off` half is skipped whenever the copy shows nothing selected,
/// fresh or not, so a stale cache can never create an explicit Off.
pub(super) fn parked_write_back(plan: &Plan) -> Option<ParkedWriteBack> {
    let wb = plan.write_back.filter(|_| !plan.url.is_empty() && plan.part_id > 0)?;
    let playing = plan.playing.as_ref()?;
    let fresh = plan.playing_fresh;
    let selected_sub = playing.subs.iter().find(|s| s.selected).map(|s| s.id);
    let audio = wb.audio.filter(|id| !(fresh && playing.audio.iter().any(|a| a.id == *id && a.selected)));
    let subtitle = wb.subtitle.filter(|half| match *half {
        SubWrite::Id(id) => !(fresh && playing.subs.iter().any(|s| s.id == id && s.selected)),
        SubWrite::Off => selected_sub.is_some(),
    });
    if audio.is_none() && subtitle.is_none() {
        return None;
    }
    Some(ParkedWriteBack {
        sid: plan.sid,
        part_id: plan.part_id,
        url: plan.url.clone(),
        audio,
        subtitle,
        keep_subtitle: selected_sub,
        external_on_part: external_on_part(&playing.subs),
        landed: (0, 0),
    })
}

/// Record what the finished landing has on screen as the selection the first frame's two readers
/// hold it to: the parked write ([`ParkedWriteBack::landed`]) and the owed container-name re-match
/// ([`Rematch::landed_sub`]). [`apply_plan`] calls it after the restore of the server's sidecar, the
/// last step that can change `cur_audio` or `cur_sub_sid` before the first frame.
///
/// Both read the SESSION, never the plan: on a direct play that is not a remux the plan names no
/// sidecar (`plan.sub_sid` is 0) while the restore has put one on screen, so a comparison made
/// against the plan would call the landing's own subtitle "changed".
pub(super) fn settle_landing_selection(ps: &mut PlaybackSession) {
    let live = (cur_audio_sid(ps), cur_sub_sid(ps));
    if let Some(parked) = ps.write_back.as_mut() {
        parked.landed = live;
    }
    if let Some(owed) = ps.track_ctx.as_mut().and_then(|c| c.rematch.as_mut()) {
        owed.landed_sub = live.1;
    }
}

/// Why the parked write no longer describes `ps`, if it does not: the ONE predicate both readers of
/// a parked write ask ([`send_parked_write_back`] and [`repoint_parked_subtitle`]), so they never
/// disagree about whether the write will be sent. In this order:
///   - `"the route changed"`: the route it was parked for is gone, a different URL or any transcode
///     (a recovery flight or a rebuild replaced it, and that route's own start PUT is the write, W3);
///   - `"the remembered pick is not shown"`: `cur_memory_note`;
///   - `"the selection on screen changed"`: the selection on screen is no longer the landed one, the
///     viewer picked something, and that pick's PUT already carries the live ids.
fn parked_write_stale(ps: &PlaybackSession, parked: &ParkedWriteBack) -> Option<&'static str> {
    if is_transcoding(ps) || ps.url != parked.url || cur_sid(ps) != parked.sid || cur_part_id(ps) != parked.part_id {
        return Some("the route changed");
    }
    if ps.cur_memory_note {
        return Some("the remembered pick is not shown");
    }
    ((cur_audio_sid(ps), cur_sub_sid(ps)) != parked.landed).then_some("the selection on screen changed")
}

/// Take the parked write if it still describes `ps`; otherwise it is spent: dropped, with the reason
/// logged, and `None`. `Option::take` is the once-per-play guard (W8).
fn take_live_parked_write(ps: &mut PlaybackSession) -> Option<ParkedWriteBack> {
    let parked = ps.write_back.take()?;
    match parked_write_stale(ps, &parked) {
        None => Some(parked),
        Some(why) => {
            crate::player::log(&format!("track-memory: write-back dropped ({why})"));
            None
        }
    }
}

/// **Send the parked write-back, once, on the landing's first decoded frame** (W6). Called by
/// the pump's decoded-frame branch ([`on_decoded_frame`]); MAIN THREAD, and it only QUEUES (the
/// serial selection worker does the PUT), so a slow server never stalls the frame.
///
/// The value is dropped, not sent, when [`parked_write_stale`] says it no longer describes the
/// session: the route changed, a remembered pick is not shown, or the viewer picked something.
///
/// The subtitle field is the half's own id (`0` for Off), else the Part's own selected subtitle,
/// else omitted. It is never `cur_sub_sid`, which on a fall-through can be a pick PMS never had (W2).
pub fn send_parked_write_back(ps: &mut PlaybackSession) {
    let Some(parked) = take_live_parked_write(ps) else {
        return;
    };
    let subtitle = match parked.subtitle {
        Some(SubWrite::Id(id)) => Some(id),
        Some(SubWrite::Off) => Some(0),
        None => parked.keep_subtitle,
    };
    crate::player::log(&format!(
        "track-memory: write-back part={} audio={} sub={}",
        parked.part_id,
        parked.audio.map_or_else(|| "-".to_string(), |a| a.to_string()),
        subtitle.map_or_else(|| "-".to_string(), |s| s.to_string()),
    ));
    queue_selection(parked.sid, parked.part_id, parked.audio.unwrap_or(0), subtitle, true);
}

/// **Carry the parked write to a subtitle the first frame's re-match put on screen** (W6,
/// FO). The container-name re-match runs on the decoded-frame branch BEFORE
/// [`send_parked_write_back`] and commits a derived subtitle; without this the commit would queue a
/// PUT of its own, change `cur_sub_sid` under the parked write (whose `landed` check would then drop
/// it, the remembered audio half with it) and reach PMS through the manual-pick path, which applies
/// neither W5 nor W7.
///
/// **Returns whether the parked write will carry `sub_sid` to the server**, which is what makes the
/// caller queue NO PUT of its own: the audio half is untouched, the subtitle half is `sub_sid` (`0`
/// is Off) under W5 and W7, and the live selection is the landed one again. It is `false`, and the
/// caller commits as an ordinary derived selection that PUTs for itself, when nothing is parked
/// (nothing was applied at the landing) or when the parked write is already stale
/// ([`parked_write_stale`], read BEFORE `landed` is touched): the send would drop it, and a re-match
/// that had trusted it would leave the server holding the landing's subtitle while the screen shows
/// the re-matched one. A stale write is spent here (dropped, and logged as the send would).
///   - W5: a Part with an external stream keeps its subtitle half withheld (the audio-only write
///     then names the Part's own selection); the memory still shows on screen.
///   - W7: an Off over a Part with nothing selected would create an explicit Off, so that half is
///     dropped, and a write left with no half at all is cleared (nothing is left to send, and the
///     server already agrees with the screen). A re-matched id PMS already holds is sent anyway:
///     re-selecting it is harmless.
pub fn repoint_parked_subtitle(ps: &mut PlaybackSession, sub_sid: i64) -> bool {
    let Some(mut parked) = take_live_parked_write(ps) else {
        return false;
    };
    parked.landed.1 = sub_sid;
    if !parked.external_on_part {
        parked.subtitle = match sub_sid {
            id if id > 0 => Some(SubWrite::Id(id)),
            _ => parked.keep_subtitle.map(|_| SubWrite::Off),
        };
    }
    if parked.audio.is_some() || parked.subtitle.is_some() {
        ps.write_back = Some(parked);
    }
    true
}

/// Test-only: park a write-back on `ps` as a landing would, naming the session's own route and
/// selection, so a test of the PUMP can grade that the decoded-frame branch (and no earlier frame)
/// takes it.
#[cfg(any(test, feature = "test-support"))]
pub fn park_write_back_for_test(ps: &mut PlaybackSession) {
    ps.write_back = Some(ParkedWriteBack {
        sid: cur_sid(ps),
        part_id: cur_part_id(ps),
        url: ps.url.clone(),
        audio: Some(cur_audio_sid(ps)),
        subtitle: None,
        keep_subtitle: None,
        external_on_part: false,
        landed: (cur_audio_sid(ps), cur_sub_sid(ps)),
    });
}

/// Test-only: a write-back is still parked on `ps`.
#[cfg(any(test, feature = "test-support"))]
pub fn write_back_parked(ps: &PlaybackSession) -> bool {
    ps.write_back.is_some()
}

/// One half of a show's track memory, as the track menu's pick describes it.
#[derive(Clone)]
enum TrackPick {
    Audio(AudioMemo),
    Subtitle(SubtitleMemo),
}

/// **Remember a track pick for the playing show: the track memory's one write.** Gated by the
/// same predicate the read side applies ([`memory_show`]: an `episode` with a known show),
/// so what is written is exactly what a later resolve can read; a movie, a trailer or an item the
/// server did not place in a show writes nothing. The seat and the tenure are `remember`'s to
/// refuse: nothing is written for a run that seated nobody (a dev token), and a refused write
/// leaves the mirror as it was.
///
/// The closure keeps the OTHER half of the entry as it is when it runs (against the mirror now,
/// and against the file's entry when the queued write runs): an audio pick must not erase the
/// subtitle the viewer chose a minute ago, nor the other way round.
fn remember_track_pick(ps: &PlaybackSession, pick: TrackPick) -> bool {
    let Some(show) = memory_show(cur_kind(ps), cur_show_rk(ps)) else {
        return false;
    };
    let Some((user, machine_id)) = memory_scope(cur_sid(ps)) else {
        return false;
    };
    let show = show.to_string();
    remember::<TrackMemoryEntry>(&user, (machine_id.clone(), show.clone()), move |current| {
        let mut entry = current.cloned().unwrap_or_else(|| TrackMemoryEntry::new(&machine_id, &show, None, None));
        match &pick {
            TrackPick::Audio(memo) => entry.audio = Some(memo.clone()),
            TrackPick::Subtitle(memo) => entry.subtitle = Some(memo.clone()),
        }
        Some(entry)
    })
}

/// **The track menu's audio pick: commit it, and remember it if the route took it.** `memo` is the
/// descriptor of the picked row ([`memo_of_audio`](super::super::memo_of_audio)); `None` is a commit that is
/// not a viewer's choice and remembers nothing. The write is made only when
/// [`commit_audio_selection`] accepted the pick, so the Forced refusal (a track the route will not
/// play) is never remembered. Returns whether the route accepted it.
///
/// Automatic picks do NOT come through here: the start-of-play auto-pick, a retry, a rescue and
/// Switch to Auto call [`commit_audio_selection`] or build a plan directly, and write no memory.
/// A pick deferred behind a pending Original handoff was accepted, and so remembered, when the
/// viewer made it; [`apply_deferred_original_effects`] only replays it and writes nothing.
pub fn commit_audio_pick(ps: &mut PlaybackSession, audio: CarriedAudio, memo: Option<AudioMemo>) -> bool {
    let accepted = commit_audio_selection(ps, audio);
    if accepted {
        if let Some(memo) = memo {
            remember_track_pick(ps, TrackPick::Audio(memo));
        }
    }
    accepted
}

/// **The track menu's subtitle pick: commit it, and remember it.** `memo` is
/// [`SubtitleMemo::Off`] for Off, or [`memo_of_subtitle`](super::super::memo_of_subtitle) of the picked
/// track (an embedded track, a sidecar, a fresh download); `None` remembers nothing. A subtitle
/// commit has no refusal, so every one that carries a memo and CHANGES the subtitle is written.
/// The commit is always made (an OK on the checked row republishes), but a pick of the stream the
/// route already shows is the menu being closed, not chosen: the route put that stream there
/// (the ladder, a rescue, an earlier pick), and an automatic pick is never written. Off is stored
/// as Off: it beats the account's rule and the server's embedded selection next episode (only a
/// server-selected sidecar the app can draw ranks above memory, `plan::pick_subtitle`).
///
/// **Leaving Forced only is a change** whatever stream it lands on: the very forced track it drew,
/// or Off while no forced track matched, has the stream id the session already shows, but the
/// state is different, so the pick is remembered. (Entering Forced only is
/// [`commit_forced_only_pick`], which carries its own `changed`.)
pub fn commit_subtitle_pick(
    ps: &mut PlaybackSession,
    sub_idx: i32,
    stream_id: i64,
    client_renderable: bool,
    memo: Option<SubtitleMemo>,
) {
    // Read before the commit, which writes the stream it was handed and the mode. Leaving Forced
    // only is a change even onto the stream it drew (the forced track's own row) or onto nothing
    // (Off while no forced track matched): same stream id, different state.
    let changed = subtitle_pick_changed(ps.cur_sub_sid, stream_id, ps.sub_mode == SubMode::ForcedOnly);
    commit_subtitle_selection(ps, sub_idx, stream_id, client_renderable);
    if let Some(memo) = memo.filter(|_| changed) {
        remember_track_pick(ps, TrackPick::Subtitle(memo));
    }
}

/// **The track menu's Forced only pick: commit what it matched, hold the mode, remember it.**
/// `sub_idx`/`stream_id` are the forced track the menu matched for the audio that plays
/// ([`forced_only_pick`](super::super::forced_only_pick)), or `-1`/`0` when nothing matched: Forced only
/// then shows nothing, and is still the mode. `lang` is the language the show remembers (the drawn
/// track's, else the audio's) and the one the mode falls back to for untagged audio. `changed` is
/// the menu's: false for an OK on the row that was already checked, which recommits what is on
/// screen (the menu passes the stream it shows) and leaves the mode's language and the memory alone.
pub fn commit_forced_only_pick(
    ps: &mut PlaybackSession,
    sub_idx: i32,
    stream_id: i64,
    client_renderable: bool,
    lang: &str,
    changed: bool,
) {
    commit_subtitle_selection(ps, sub_idx, stream_id, client_renderable);
    ps.sub_mode = SubMode::ForcedOnly;
    if changed {
        if let Some(ctx) = ps.track_ctx.as_mut() {
            ctx.forced_lang = lang.to_string();
        }
        remember_track_pick(ps, TrackPick::Subtitle(SubtitleMemo::ForcedOnly { lang: lang.to_string() }));
    }
}

/// **The track menu's subtitle pick, together with the sidecar it may name**: the route commit and
/// remembered write of [`commit_subtitle_pick`], then the sidecar draw that goes with it. The app's
/// `commit_track` hands the menu's `TrackCommit::Subtitle` here field by field (the route cannot name
/// the track menu above it). `sidecar_key` is `Some` for an EXTERNAL text subtitle the client can
/// draw on direct play, `sidecar_codec` is its codec (it preserves ASS/SSA on download), and `None`
/// (Off, or an embedded track) deselects any sidecar.
pub fn commit_subtitle_and_sidecar(
    ps: &mut PlaybackSession,
    render_ordinal: i32,
    stream_id: i64,
    sidecar_key: Option<String>,
    sidecar_codec: String,
    memo: Option<SubtitleMemo>,
) {
    // What the client can draw itself — an embedded ordinal or an external sidecar — is
    // what an Original route (issue #266's candidate) can carry without a burn.
    let client_renderable = render_ordinal >= 0 || sidecar_key.is_some();
    commit_subtitle_pick(ps, render_ordinal, stream_id, client_renderable, memo);
    // An EXTERNAL pick has no demuxer ordinal (`render_ordinal` is -1, so the embedded
    // renderer is off) — on direct play `player::sidecar` fetches and draws it instead.
    // While transcoding the commit above already asked for a burn and the sidecar draw
    // is silenced for as long as that is true, so selecting here is harmless and means
    // the line survives the playback going BACK to direct play.
    match sidecar_key {
        Some(key) => crate::player::sidecar::select(cur_sid(ps), stream_id, key, sidecar_codec),
        None => crate::player::sidecar::deselect(),
    }
}

/// **The track menu's Forced only pick, together with the sidecar it drops**: [`commit_forced_only_pick`]
/// and then the deselect of any sidecar an earlier pick drew. The track is always an embedded one
/// the demuxer draws (a forced track the client cannot demux is not matched), so a sidecar from an
/// earlier pick is dropped as for Off.
pub fn commit_forced_only_and_sidecar(
    ps: &mut PlaybackSession,
    render_ordinal: i32,
    stream_id: i64,
    lang: &str,
    changed: bool,
) {
    commit_forced_only_pick(ps, render_ordinal, stream_id, render_ordinal >= 0, lang, changed);
    crate::player::sidecar::deselect();
}

/// A subtitle commit the PLAYBACK derives from the mode, not one the viewer made: the audio follow
/// and the container-name re-match. It goes through the one commit every pick does (the burn
/// refresh, the selection PUT, the side reader) and writes no memory; `forced_only` puts the mode
/// back, which the ordinary commit settles from the stream alone. `put` is the commit's own
/// direct-play selection PUT: the audio follow sends it, the re-match does not while a live
/// write-back that carries its result is parked, nor over a Part with an external stream (W5).
fn commit_derived_subtitle(ps: &mut PlaybackSession, picked: (i64, i32), forced_only: bool, put: bool) {
    let (id, ordinal) = picked;
    commit_subtitle_selection_inner(ps, ordinal, id, ordinal >= 0, put);
    if forced_only {
        ps.sub_mode = SubMode::ForcedOnly;
    }
}

/// **The audio follow**: while Forced only is the mode, a new audio re-points it at the forced
/// track in that audio's language (English audio the English forced track, Japanese the Japanese
/// one), or at nothing when there is none. Nothing happens off Forced only; while the landing's
/// note says a copy remux shows no remembered subtitle, since a derived pick would turn that into a
/// re-encode; or while the audio is only deferred behind an Original handoff (the deferred commit
/// follows when it lands).
pub(super) fn follow_audio_with_forced_only(ps: &mut PlaybackSession, audio_sid: i64) {
    if ps.sub_mode != SubMode::ForcedOnly || ps.cur_memory_note || original_recovery_pending() {
        return;
    }
    let Some(ctx) = ps.track_ctx.as_ref() else { return };
    let shown = {
        let names = crate::player::SHARED.track_names_lenient();
        forced_only_shown(&ctx.subs, &names, ctx.audio_lang_of(audio_sid), &ctx.forced_lang)
    };
    if shown.id != ps.cur_sub_sid {
        commit_derived_subtitle(ps, (shown.id, shown.ordinal), true, true);
    }
}

/// **The first decoded frame's re-match** (FO; the pump calls it once per frame, and it does
/// its work once per landing). On a direct play the demuxer's track names are known only once the
/// file is open, so a remembered subtitle the resolve could not find by name (an MP4 part's PMS
/// streams carry no `title`) is matched once more with them. When that differs from what the landing
/// put on screen, it is committed as a derived selection with NO memory write. Skipped when the
/// viewer picked a subtitle meanwhile ([`commit_subtitle_selection`] ends the owed re-match).
///
/// Runs BEFORE the write-back's send (WB): the server is told what is on screen.
///
/// **It returns what it committed**: `Some(id)` is the stream now on screen (`0` = nothing: Forced
/// only came to nothing), `None` that it committed nothing (not owed, nothing found, or the viewer
/// picked first).
///
/// **The server is told once.** While a live write-back is parked the re-match first hands its
/// result to it ([`repoint_parked_subtitle`]), which keeps the remembered audio half and applies W5
/// (no subtitle half while the Part carries an unselected external stream) and W7 (no explicit Off
/// over a Part with nothing selected) to the re-matched subtitle; the commit then queues no
/// selection PUT of its own, and [`send_parked_write_back`] sends the one write right after, naming
/// what is on screen. With nothing parked (a retry, a landing that applied no remembered half, a
/// write PMS already agrees with), or a parked write that went stale first (the route changed, the
/// viewer picked), the commit is an ordinary one and PUTs as any subtitle change does, so the server
/// always hears the subtitle on screen. **W5 holds for that PUT too**: over a Part that carries an
/// external stream (a download, maybe another Home user's) the commit PUTs nothing, because moving
/// the selection off a download makes PMS discard it; the re-matched subtitle still shows.
pub fn rematch_subtitle_with_names(ps: &mut PlaybackSession) -> Option<i64> {
    // Every frame after the first asks: the common answer is no, and costs no lock.
    if !ps.track_ctx.as_ref().is_some_and(|c| c.rematch.is_some()) {
        return None;
    }
    let owed = ps.track_ctx.as_mut().and_then(|c| c.rematch.take())?;
    // Only while the landing's subtitle is still what is shown, and never over a sidecar: the
    // container's names describe its embedded tracks, and a sidecar on screen (the profile's
    // remembered one, restored at the landing) was matched against the whole list already. A
    // names-only match over embedded tracks would replace it with the same language's embedded track.
    if ps.cur_sub_sid != owed.landed_sub || ps.cur_sub_sidecar {
        return None;
    }
    let ctx = ps.track_ctx.as_ref()?;
    let rematched = {
        let names = crate::player::SHARED.track_names_lenient();
        rematch_with_names(&ctx.subs, &names, &owed.memo, ctx.audio_lang_of(cur_audio_sid(ps)), owed.landed_sub)
    };
    if let Some(found) = rematched {
        crate::player::log(&format!(
            "track-memory: re-matched with the container's names (track {})",
            found.id
        ));
        let forced_only = matches!(owed.memo, SubtitleMemo::ForcedOnly { .. });
        // Before the commit, which changes `cur_sub_sid` under the parked write's landed pair.
        let carried_by_write_back = repoint_parked_subtitle(ps, found.id);
        // W5, the Part's own streams: the landing's subtitle half is withheld over any external.
        let external = ps.track_ctx.as_ref().is_some_and(|c| external_on_part(&c.subs));
        commit_derived_subtitle(ps, (found.id, found.ordinal), forced_only, !carried_by_write_back && !external);
    }
    rematched.map(|found| found.id)
}

/// **Everything the session does on a decoded frame, in the order that is its contract** (the pump's
/// `frames > 0` branch calls it once per frame; every step is once per landing and costs nothing
/// after). Both the pump and the write-back tests call this one function, so the order is graded
/// where it runs:
///   1. [`rematch_subtitle_with_names`]: the demuxer's track names exist only once the file is open,
///      and the re-match must settle what is on screen BEFORE anything tells the server. While a
///      write-back is parked it re-points that write's subtitle half and queues no PUT of its own.
///   2. [`confirm_resume_presented`]: the replacement route shows a frame.
///   3. [`send_parked_write_back`]: the one write names the remembered audio and the subtitle the
///      re-match settled on. Swapped with step 1 the send would go first, naming the landing's
///      subtitle, and the re-match would then PUT again through the manual path (two writes, no W5,
///      no W7).
pub fn on_decoded_frame(ps: &mut PlaybackSession) {
    rematch_subtitle_with_names(ps);
    confirm_resume_presented(ps);
    send_parked_write_back(ps);
}
