//! **The track memory's pure helpers, as a child of [`super`]**: what a resolve reads off the
//! remembered record once the ladder ([`super::pick_subtitle`]) has decided, with no clock, no
//! session and no I/O (the `wall` gate scans this file as it scans `plan.rs`).
//!
//! * the show's entry in the captured list ([`show_memory`]) and the remembered subtitle the ladder
//!   actually consulted ([`consulted_memory`]), with the mode a landing is in ([`landed_sub_mode`]);
//! * what a direct play would write back to the server ([`write_back_of`], and
//!   [`external_on_part`], W5's test, which the parked write and the re-match read too);
//! * the resolve's one `track-memory:` log line ([`memory_log`]) and the stream id of a remembered
//!   subtitle the ladder chose to draw ([`memory_subtitle_id`]).

use super::*;
use super::super::track_memory::memory_show;

/// The profile's remembered picks for the show being played, for an episode of a known show only.
/// `memory` is the env's capture, already filtered to this server and empty for a known retry.
pub(super) fn show_memory<'a>(
    memory: &'a [TrackMemoryEntry], playing: &plx_data::metadata::PlayingItem,
) -> Option<&'a TrackMemoryEntry> {
    memory_show(&playing.kind, &playing.show_rk).and_then(|show| memory.iter().find(|e| e.show_rk == show))
}

/// The remembered subtitle [`pick_subtitle`] actually consulted: none for this playback's own pick
/// (a retry) or when the server has a sidecar selected, which outranks memory. What a later
/// re-match may hold the landing to.
pub(super) fn consulted_memory<'a>(
    item: &plx_data::metadata::PlayingItem, override_id: Option<i64>, memo: Option<&'a SubtitleMemo>,
) -> Option<&'a SubtitleMemo> {
    memo.filter(|_| override_id.is_none() && plx_data::metadata::server_selected_sidecar(item).is_none())
}

/// **Which of the three subtitle states a landing is in** ([`SubMode`]). A retry carries the mode
/// its selection was in (`env.sub_mode`: the override is a stream id, which cannot say Forced only);
/// otherwise Forced only exactly when the remembered Forced only decided the landing, else the
/// state the picked stream itself implies.
pub(super) fn landed_sub_mode(decision: Option<&SubDecision>, consulted: Option<&SubtitleMemo>, env: &ResolveEnv) -> SubMode {
    let of_pick = || SubMode::of_stream(decision.and_then(|d| d.pick).map_or(0, |(id, _)| id));
    if env.subtitle_override.is_some() {
        return if env.sub_mode == SubMode::ForcedOnly { SubMode::ForcedOnly } else { of_pick() };
    }
    match (decision, consulted) {
        (Some(d), Some(SubtitleMemo::ForcedOnly { .. })) if d.source == SubPickSource::Memory => SubMode::ForcedOnly,
        _ => of_pick(),
    }
}

/// The stream id of a remembered subtitle the ladder chose to draw.
pub(super) fn memory_subtitle_id(decision: Option<&SubDecision>) -> Option<i64> {
    decision.filter(|d| d.source == SubPickSource::Memory).and_then(|d| d.pick).map(|(id, _)| id)
}

/// **What a direct play would write back to the server** (WB, W1-W5): the halves of a remembered
/// choice that were APPLIED and are what is on screen, on a fresh play only.
///   - audio: the remembered track is the one the direct play carries (a sibling or stand-in
///     chosen for its codec is not the choice, and writing it would record the stand-in);
///   - subtitle: the remembered subtitle (rung 3: a track, Off or Forced only) decided (`Id`, or
///     `Off` when nothing is to show), and the Part carries NO external stream at all. An external
///     may be another Home user's download (U24), or the viewer's own selected one that rung 2
///     cannot take (`server_selected_sidecar` wants a renderable sidecar: a PGS `.sup`, a VobSub, a
///     format outside the allow-list, a keyless stream, all of which fall through to memory), and
///     moving the selection off a download makes PMS discard it (W5).
///
/// A declined memory, a fall-through, a retry and a preview write nothing.
pub(super) fn write_back_of(
    env: &ResolveEnv, rk: &str, audio_memory: Option<&Result<usize, Miss>>,
    audio_sel: Option<&(i32, String, i64)>, sub: Option<&SubDecision>,
    playing: Option<&plx_data::metadata::PlayingItem>,
) -> Option<WriteBack> {
    let playing = playing.filter(|_| env.fresh_play && !rk.is_empty())?;
    let audio = audio_memory
        .and_then(|m| m.as_ref().ok())
        .and_then(|&i| playing.audio.get(i))
        .map(|t| t.id)
        .filter(|&id| id > 0 && audio_sel.is_some_and(|(_, _, picked)| *picked == id));
    let subtitle = sub.filter(|d| d.source == SubPickSource::Memory).map(|d| match d.pick {
        Some((id, _)) => SubWrite::Id(id),
        None => SubWrite::Off,
    });
    let subtitle = subtitle.filter(|_| {
        let external_on_part = external_on_part(&playing.subs);
        if external_on_part {
            crate::player::log("write-back: subtitle withheld (external on part)");
        }
        !external_on_part
    });
    (audio.is_some() || subtitle.is_some()).then_some(WriteBack { audio, subtitle })
}

/// W5's test: the Part carries an external stream (a sidecar or a download), so no subtitle half is
/// written for it. One predicate for the resolve ([`write_back_of`]) and for the parked write that
/// a later re-match re-points (`decision::repoint_parked_subtitle`), and for the re-match's own commit
/// when nothing is parked (`decision::rematch_subtitle_with_names`).
pub(in super::super) fn external_on_part(subs: &[plx_data::metadata::Stream]) -> bool {
    subs.iter().any(|s| s.external)
}

/// The one `track-memory:` line of a resolve: what each remembered half came to. `None` when the
/// profile remembered nothing for this show.
///
/// The grammar is fixed, the VERB first and the half after
/// it, because the Device step, FO's check and the Q-TOP (b) tests search for those substrings:
/// `applied audio (track N, lang)`, `applied subtitle (track N)`, `applied off`,
/// `applied forced-only (track N)` / `applied forced-only:none`, `no match (<reason>) <half>`,
/// `not used (a higher rung decided) subtitle`, and, once the route is known, `not shown (copy remux)
/// subtitle N` in the place of `applied subtitle` for a remembered pick a copy remux neither draws
/// nor burns (`not_shown`; Q-TOP (b)). Whatever APPLIED leads, so `track-memory: applied`
/// opens the line whenever either half applied, and the subtitle half goes before the audio half so
/// `track-memory: applied forced-only` does too when audio applied beside it.
pub(super) fn memory_log(
    audio: Option<&Result<usize, Miss>>, tracks: &[plx_data::metadata::Stream],
    memo: Option<&SubtitleMemo>, sub: Option<&SubDecision>, not_shown: bool,
) -> Option<String> {
    let mut applied = Vec::new();
    let mut rest = Vec::new();
    if let (Some(memo), Some(d)) = (memo, sub) {
        match (d.source, d.miss, d.pick, memo) {
            // Q-TOP (b): picked, but the route is a copy remux that neither draws nor burns it.
            (SubPickSource::Memory, _, Some((id, _)), _) if not_shown => {
                rest.push(format!("not shown (copy remux) subtitle {id}"))
            }
            (SubPickSource::Memory, _, Some((id, _)), SubtitleMemo::ForcedOnly { .. }) => {
                applied.push(format!("applied forced-only (track {id})"))
            }
            (SubPickSource::Memory, _, Some((id, _)), _) => applied.push(format!("applied subtitle (track {id})")),
            (SubPickSource::Memory, _, None, SubtitleMemo::ForcedOnly { .. }) => applied.push("applied forced-only:none".into()),
            (SubPickSource::Memory, _, None, _) => applied.push("applied off".into()),
            (_, Some(miss), _, _) => rest.push(format!("no match ({miss}) subtitle")),
            _ => rest.push("not used (a higher rung decided) subtitle".into()),
        }
    }
    match audio {
        Some(Ok(i)) => applied.push(format!("applied audio (track {}, {})", tracks[*i].id, tracks[*i].lang_code)),
        Some(Err(miss)) => rest.push(format!("no match ({miss}) audio")),
        None => {}
    }
    applied.extend(rest);
    (!applied.is_empty()).then(|| format!("track-memory: {}", applied.join("; ")))
}
