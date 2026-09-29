//! In-player modal track menu: audio + subtitle pickers over the video, rendered on the reusable
//! animated `TableView` (Apple-TV "settings" look — a sliding pill selection, section header with
//! a codec accessory, per-row badges, a leading checkmark on the active track). app.rs routes
//! D-pad/OK/BACK here while the menu is open; LEFT/RIGHT switch between the Audio and Subtitles
//! panels. The selection commit (native audio switch / server transcode / burn) is unchanged
//! from the previous procedural version — only the presentation moved onto the table.
//!
//! **The Subtitles panel is grouped, not one flat list** (plan `subtitle-menu-capsule` §3,
//! `/tmp/dsplayer/player.html:1104-1162`): Off and every single-track "yours" language sit under
//! one "Subtitles" header; a "yours" language with several tracks gets its own section (header =
//! language, `accessory("N tracks")`), ranked full < SDH < forced < commentary; a headerless
//! section holds Timing and Color; and everything else falls under "Other languages". That
//! grouping is a pure DATA model, `metadata::sub_layout` (host-tested without a
//! `PlaybackSession`/`MetadataView` fixture); this module only turns it into `TableView` sections
//! ([`table_sections`]) and answers focus/OK over the flat [`RowTarget`] vec it yields. "Yours" is
//! the pref language (if the play resolved under one), the playing audio's language, and the
//! current subtitle's own language, in that order (`route::cur_sub_pref_lang`, carried in by
//! `screens::player::overlay`).
//!
//! **Timing** is a single row that reads out the current offset; OK on it does not step anything
//! here — it returns [`TrackOk::OpenTiming`], which `screens::player::overlay`'s `activate` turns
//! into a hand-off: it dismisses this panel and presents the Timing capsule overlay
//! (`OverlayKind::Timing`, `ui::timing_capsule`) in its place. The row is dim and inert while
//! subtitles are Off (OK there neither opens the capsule nor closes the panel), and the whole
//! section is omitted during a transcode, which burns captions into the picture where no
//! client-side offset can reach.
//!
//! **Color** is a single cycling row: OK steps `SubtitleTone::LADDER` with wrap and keeps the
//! panel open ([`TrackOk::Commit`]'s `keep_open`), so a run of presses is felt immediately — and
//! re-writes that one row's read-out in place rather than rebuilding the list.
use crate::metadata;
use crate::metadata::sub_layout::{self, RowBadge, SubHeader, SubRow, SubSection, SubTrack};
pub(crate) use crate::metadata::sub_layout::RowTarget;
use crate::metadata::track_label;
use crate::plex::session::SubtitleTone;
use crate::ui::consts::SCR_H;
use crate::ui::frame::Budget;
use crate::ui::geom::IndexElem;
use crate::ui::machine::{Cx, EntryId, FocusKey, GroupId, Host};
use crate::ui::popover::Popover;
use crate::ui::screen::{
    Activate, At, AxisMask, Dir, DrawFrame, EdgeRule, ElemKind, Focusable, GroupKind, GroupSpec,
    Hover, Part, Placed, Seat, Step, Stop,
};
use crate::ui::table::{Badge, Row, Section, TableView};
use crate::ui::theme;
use crate::ui::{Painter, Rect};
use std::os::raw::c_int;

/// The Subtitles panel's width — wider than Audio's, since a source detail line and an "N tracks"
/// accessory need more room than a bare language name.
const SUB_PANEL_W: f32 = 620.0;
/// The Audio panel's width — unchanged from before the grouped Subtitles redesign.
const AUDIO_PANEL_W: f32 = 560.0;

/// One drawn row of the Audio tab, by POSITION — the Audio-tab counterpart of [`RowTarget`],
/// which only ever describes a Subtitles row. [`TrackMenuState::build_audio`] is the only writer;
/// [`TrackMenuState::on_ok`]'s tab==0 arm dispatches on it instead of comparing `sel` against a
/// recomputed "row past the last track" boundary, the same shape the Subtitles tab already used.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum AudioRowTarget {
    /// A track row — the index into the playing item's audio list
    /// ([`crate::metadata::PlayingItem::audio`]).
    Track(usize),
    /// The Boost dialog toggle row (issue #266) — present only while [`TrackMenuState::enhance_shown`]
    /// is `Some`, immediately after the last [`Self::Track`].
    Boost,
    /// The Normalize loudness toggle row (issue #266) — present only while
    /// [`TrackMenuState::enhance_shown`] is `Some`, immediately after [`Self::Boost`].
    Loudness,
}

/// The menu's whole state, owned by the container that mounts this panel — the modal PHASE and the
/// appear spring belong to `ui::containers::modal::ModalStack` now, not to this struct; `draw` takes
/// the appear fraction as a parameter instead of stepping its own [`Popover`].
pub(crate) struct TrackMenuState {
    tab: c_int, // 0=Audio, 1=Subtitles
    active_audio: c_int, // index into the playing item's audio list
    active_sub: c_int, // -1 = Off, else index into the playing item's subs list
    /// The Subtitles tab's flat row → meaning map — one [`SubRow::target`] per drawn row, kept
    /// alongside the `Section`s it built so [`Self::on_ok`] reads back what a row IS by POSITION
    /// instead of re-deriving it (and instead of disagreeing with what was actually drawn, the way
    /// a fresh call to [`visible_subs`] could once a transcode starts). Empty while the Audio tab
    /// is built; see [`Self::audio_targets`] for its counterpart.
    targets: Vec<RowTarget>,
    /// The Audio tab's flat row → meaning map — [`AudioRowTarget`]'s counterpart to
    /// [`Self::targets`], built by [`Self::build_audio`] and read back by [`Self::on_ok`]. Empty
    /// while the Subtitles tab is built.
    audio_targets: Vec<AudioRowTarget>,
    /// The timing offset (ms) the Timing row reads out — seeded from the player on open. Kept
    /// locally (rather than re-reading the player's atomic on every draw) so the Timing capsule's
    /// eventual hand-off starts from what THIS panel showed, not from a commit the loop has not
    /// yet performed.
    offset_ms: i64,
    /// The caption tone the Color row reads out and cycles — seeded from the player on open, same
    /// reasoning as [`Self::offset_ms`]: a burst of OK presses in one frame must count from what
    /// this panel last drew, not from the global the loop has not yet written
    /// (`TrackCommit::SubtitleTone` is dispatched to the loop, not applied inline by `on_ok`).
    tone: SubtitleTone,
    /// "Your languages" this play resolved under, in PREFERENCE order — the pref's BCP-47 code (if
    /// the play resolved under one), the playing audio's language, and the current subtitle's own,
    /// exactly as `metadata::sub_layout::sub_sections`' `yours` parameter reads them (compared by
    /// `metadata::lang_key`, never by literal string equality). Owned rather than borrowed, so
    /// a rebuild (tab switch) needs nothing from the caller beyond `ps`/`meta`.
    yours: Vec<String>,
    /// The Audio tab's Plex Pass DSP toggle rows (issue #266) — `None` when they are not offered
    /// (I1/I2: absent, never greyed), else what they currently read out: "desired while pending,
    /// applied otherwise" (`route::displayed_audio_enhancements`), same reasoning as
    /// [`Self::offset_ms`]/[`Self::tone`] — a run of toggle presses inside one open counts from
    /// what THIS panel last drew, and [`Self::rebuild`] is the only writer, on every (re)build of
    /// the Audio tab (`new`/`focus_tab`).
    enhance_shown: Option<crate::plex::AudioEnhancements>,
    table: TableView, // main-thread only
}

/// **What the track menu DECIDED**, for the loop to perform (spec §2.2).
///
/// The panel owns its rows and its cursor; it does not own the playback, so it may not call
/// `route::commit_audio_selection` / `commit_subtitle_selection` itself — those take the session's
/// `&mut`, and a screen is only ever shown the frame's publication. `None` means the pick changed
/// nothing (audio only: a subtitle OK always republishes, because "Off" is a real choice that the
/// panel cannot distinguish from "unchanged" without knowing what the renderer currently has).
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum TrackCommit {
    /// The frozen `CarriedAudio` snapshot for the picked row (issue #266), built via
    /// `CarriedAudio::from_stream` from the exact `metadata::Stream` the row was drawn from.
    Audio(crate::route::CarriedAudio),
    /// The Audio tab's Boost dialog / Normalize loudness toggle rows (issue #266): the full
    /// preference after the flip, so the loop's `player::request_audio_enhancement` has both
    /// bits regardless of which row was pressed. Built from [`TrackMenuState::enhance_shown`],
    /// never re-derived from `ps` here — the panel owns its own rows, not the playback (see this
    /// enum's own doc).
    AudioEnhancement(crate::plex::AudioEnhancements),
    /// `sidecar_key` is `Some` when the pick is an EXTERNAL text subtitle the client can draw
    /// on direct play (`metadata::Stream::sidecar_renderable`): it has no demuxer ordinal
    /// (`render_ordinal` is -1), so the loop hands it to `player::sidecar` beside the unchanged
    /// route commit. `sidecar_codec` preserves ASS/SSA on download; a key need not have an
    /// extension. `None` — Off, or an embedded track — deselects any sidecar.
    Subtitle { render_ordinal: c_int, stream_id: i64, sidecar_key: Option<String>, sidecar_codec: String },
    /// The caption's tone. Not a track at all, but it is picked in this panel and it is the
    /// loop that performs it (`player::set_subtitle_tone` writes the session), like the two above.
    SubtitleTone(SubtitleTone),
    /// The caption's timing offset in ms (`player::set_subtitle_offset`) — produced by the Timing
    /// capsule overlay (plan §4), not by this panel: the Timing ROW here only opens that capsule
    /// ([`TrackOk::OpenTiming`]). The variant stays here because `TrackCommit` is the one
    /// player-state-commit type every subtitle control produces, capsule included.
    SubtitleOffset(i64),
}

/// **The whole outcome of OK on the focused row**, one level up from [`TrackCommit`] — what to
/// perform AND whether the panel stays, so the caller reads one value rather than asking twice
/// (once before `on_ok` rebuilt the rows under the cursor). The Timing row hands off to a
/// different overlay (`screens::player::overlay`'s Tracks→Timing transition, plan §4) — a
/// decision this panel can state but not perform, since it does not own the overlay stack.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum TrackOk {
    /// Perform `commit`. `keep_open` is true for Color, so a run of presses (cycling the ladder)
    /// is felt without a reopen-and-rewalk between them; every track pick closes the panel.
    Commit { commit: TrackCommit, keep_open: bool },
    /// Nothing changed (the already-playing audio track): close the panel.
    Dismiss,
    /// Open the Timing capsule overlay: `screens::player::overlay`'s `activate` dismisses the
    /// Tracks panel and asks for `OverlayKind::Timing` in its place.
    OpenTiming,
    /// The dim Timing row while subtitles are Off: OK does nothing, and so must not close the
    /// panel either.
    Inert,
}

impl TrackMenuState {
    /// Build the menu focused on `tab` (0=Audio, 1=Subtitles) — the on-screen audio/subs icons
    /// pick a specific tab this way; the plain open path passes 0. `yours` is "your languages" in
    /// preference order (pref, playing audio, current subtitle) — see [`Self::yours`].
    pub(crate) fn new(
        ps: &crate::route::PlaybackSession,
        meta: metadata::MetadataView<'_>,
        tab: c_int,
        yours: Vec<String>,
    ) -> Self {
        let mut s = TrackMenuState {
            tab,
            active_audio: 0,
            active_sub: -1,
            targets: Vec::new(),
            audio_targets: Vec::new(),
            offset_ms: crate::player::subtitle_offset_ms(),
            tone: crate::player::subtitle_tone(),
            yours,
            enhance_shown: None,
            table: TableView::new(),
        };
        s.sync_item(ps, meta);
        s.rebuild(ps, meta, tab, false);
        s
    }

    /// The highlighted row, for the focus probe (`crate::focusprobe`) — a READ of the cursor the
    /// key ladder moves, and the reason it exists: `app.rs`'s UP/DOWN arm for this panel changes
    /// nothing else, so without this the fingerprint records the panel opening and closing and
    /// nothing between.
    pub(crate) fn sel(&self) -> i32 {
        self.table.sel
    }

    /// The row alphabet at its current tab, for a caller that needs a row's index without
    /// duplicating this layout by hand (`screens::player::overlay_tests`'s Timing hand-off test).
    #[cfg(test)]
    pub(crate) fn targets(&self) -> &[RowTarget] {
        &self.targets
    }

    /// **Write back the engine's own focus cursor** (restructure phase 12): the Column group
    /// [`TrackMenuPart`] answers is the source of geometry, but the ENGINE owns the current
    /// element (§7.3 step 5) — the owner's `step` is the only place that mutates in response to a
    /// `FocusMoved`, and this is `screens::player::overlay::PlayerOverlayScreen::step`'s write.
    pub(crate) fn set_sel(&mut self, i: i32) {
        self.table.sel = i;
    }

    /// index into the playing item's audio list of the chosen audio track
    pub(crate) fn active_audio(&self) -> c_int {
        self.active_audio
    }
    /// -1 = subtitles off, else index into the playing item's subs list
    pub(crate) fn active_sub(&self) -> c_int {
        self.active_sub
    }
    /// Plex stream id of the chosen audio track (for &audioStreamID), or 0
    pub(crate) fn audio_stream_id(&self, meta: metadata::MetadataView<'_>) -> i64 {
        let i = self.active_audio();
        tracks(meta)
            .and_then(|t| t.audio.get(i.max(0) as usize))
            .map(|s| s.id)
            .unwrap_or(0)
    }
    /// Plex stream id of the chosen subtitle track (for &subtitleStreamID), or 0 if Off
    pub(crate) fn sub_stream_id(&self, meta: metadata::MetadataView<'_>) -> i64 {
        let i = self.active_sub();
        if i < 0 {
            return 0;
        }
        tracks(meta)
            .and_then(|t| t.subs.get(i as usize))
            .map(|s| s.id)
            .unwrap_or(0)
    }

    /// Derive the checked tracks from the PLAYBACK state on every open — the route owns the truth
    /// (CUR_AUDIO_SID/CUR_SUB_SID, set by the start-of-play pick and every commit), so the menu can
    /// never show a stale or desynced checkmark: the auto-picked default/smart-DP track is checked
    /// on first open, a replayed item resets with the playback, and a prior pick round-trips by id.
    /// When no id is recorded (codec-default play), the file's flagged default is checked.
    /// Deliberately does NOT touch `tab`: [`TrackMenuState::new`] sets it directly.
    fn sync_item(&mut self, ps: &crate::route::PlaybackSession, meta: metadata::MetadataView<'_>) {
        let (audio, sub) = match tracks(meta) {
            Some(t) => {
                let asid = crate::route::cur_audio_sid(ps);
                let audio = (asid > 0)
                    .then(|| t.audio.iter().position(|s| s.id == asid))
                    .flatten()
                    .or_else(|| t.audio.iter().position(|s| s.default))
                    .unwrap_or(0) as c_int;
                let ssid = crate::route::cur_sub_sid(ps);
                let sub = (ssid > 0)
                    .then(|| t.subs.iter().position(|s| s.id == ssid))
                    .flatten()
                    .map(|i| i as c_int)
                    .unwrap_or(-1);
                (audio, sub)
            }
            None => (0, -1),
        };
        self.active_audio = audio;
        self.active_sub = sub;
    }

    /// Focus an ABSOLUTE table row — the /tmp/plxnative-menupick trigger's contract ("row N").
    /// The interactive path always moves relatively; this exists because the initial focus is the
    /// ACTIVE row (derived from playback state), so a relative walk from it would land elsewhere.
    pub(crate) fn focus_row(&mut self, row: c_int) {
        for _ in 0..64 {
            if self.table.sel == row {
                break;
            }
            let before = self.table.sel;
            self.table.move_sel(if self.table.sel < row { 1 } else { -1 });
            if self.table.sel == before {
                break; // clamped at an end — row out of range
            }
        }
    }

    /// Resolve a NAMED Audio-tab target (`"boost"`/`"loudness"`) to its absolute table row, for
    /// the `/tmp/plxnative-menupick` trigger's named form — an alternative to a row number hand-
    /// derived from the item's track count, which is exactly the issue #266 PR4 bug: this harness
    /// once hardcoded the Normalize Loudness row from a WRONG assumed track count. Reading it back
    /// through [`Self::audio_targets`], the same map [`Self::on_ok`] dispatches on, means the name
    /// is correct however many tracks the item actually has. `None` when `name` is unrecognized,
    /// or recognized but not currently built (the DSP toggle rows are not offered right now).
    pub(crate) fn row_for_audio_target(&self, name: &str) -> Option<c_int> {
        let target = match name {
            "boost" => AudioRowTarget::Boost,
            "loudness" => AudioRowTarget::Loudness,
            _ => return None,
        };
        self.audio_targets.iter().position(|t| *t == target).map(|i| i as c_int)
    }

    /// Show `tab` (0=Audio, 1=Subtitles) on a menu that is ALREADY open — the second disc pressed
    /// while the first one's tab is showing. Same body as the LEFT/RIGHT arm below, which is why
    /// that arm calls this rather than repeating it.
    pub(crate) fn focus_tab(&mut self, ps: &crate::route::PlaybackSession, meta: metadata::MetadataView<'_>, tab: c_int) {
        if tab != self.tab {
            self.tab = tab;
            self.rebuild(ps, meta, tab, false); // swap the whole list → snap the pill, no long glide
        }
    }

    /// commit the focused row as the active track for its tab — dismissing the panel afterward is
    /// the container's job now, not this method's; the answer says whether it should.
    pub(crate) fn on_ok(&mut self, meta: metadata::MetadataView<'_>) -> TrackOk {
        let tab = self.tab;
        let sel = self.table.sel;
        if tab == 0 {
            // The two Plex Pass DSP rows (issue #266), appended after the audio tracks — see
            // `Self::build_audio`, which builds `self.audio_targets` alongside them, so a track
            // pick below is never mistaken for one of these by position arithmetic.
            return match self.audio_targets.get(sel.max(0) as usize).copied() {
                target @ (Some(AudioRowTarget::Boost) | Some(AudioRowTarget::Loudness)) => {
                    let mut a = self.enhance_shown.unwrap_or(crate::plex::AudioEnhancements::NONE);
                    if target == Some(AudioRowTarget::Boost) {
                        a.boost_dialog = !a.boost_dialog;
                    } else {
                        a.normalize_loudness = !a.normalize_loudness;
                    }
                    self.enhance_shown = Some(a);
                    if let Some(row) = self.table.row_mut(sel) {
                        row.toggle = Some(if target == Some(AudioRowTarget::Boost) {
                            a.boost_dialog
                        } else {
                            a.normalize_loudness
                        });
                    }
                    crate::diag::event(crate::diag::schema::DiagEvent::FeatureUsed {
                        feature: crate::diag::schema::Feature::AudioEnhancement,
                    });
                    TrackOk::Commit { commit: TrackCommit::AudioEnhancement(a), keep_open: true }
                }
                _ => {
                    let changed = self.active_audio != sel;
                    self.active_audio = sel;
                    if changed {
                        // the menu only reports the pick — native-switch vs re-transcode is
                        // route's policy. The demuxer-facing index is the CONTAINER ordinal
                        // (audio_ordinal), not the row.
                        if let Some(s) = tracks(meta).and_then(|t| t.audio.get(sel.max(0) as usize)) {
                            let ord = tracks(meta)
                                .map(|t| metadata::audio_ordinal(&t.audio, sel.max(0) as usize))
                                .unwrap_or(sel);
                            crate::diag::event(crate::diag::schema::DiagEvent::FeatureUsed {
                                feature: crate::diag::schema::Feature::AudioTrack,
                            });
                            return TrackOk::Commit {
                                commit: TrackCommit::Audio(crate::route::CarriedAudio::from_stream(s, ord)),
                                keep_open: false,
                            };
                        }
                    }
                    TrackOk::Dismiss
                }
            };
        }

        match self.targets.get(sel.max(0) as usize).copied() {
            Some(RowTarget::Color) => {
                // cycle with wrap: no track changes, so no `TrackCommit::Subtitle` — that one
                // always republishes, and re-committing the track would re-burn a transcode
                let n = SubtitleTone::LADDER.len() as u8;
                self.tone = SubtitleTone::from_index((self.tone.index() + 1) % n);
                // the panel stays up: re-write this row's read-out in place, focus unmoved — the
                // grouping does not depend on the tone, so nothing else is rebuilt
                if let Some(row) = self.table.row_mut(sel) {
                    row.value = Some(tone_label(self.tone).to_string());
                }
                TrackOk::Commit { commit: TrackCommit::SubtitleTone(self.tone), keep_open: true }
            }
            Some(RowTarget::Timing) if self.active_sub >= 0 => TrackOk::OpenTiming,
            Some(RowTarget::Timing) => TrackOk::Inert, // dim and inert while subtitles are Off
            target => {
                // Off (or a stale/out-of-range selection) → -1; else the row's own subs-list index
                let new_sub: c_int = match target {
                    Some(RowTarget::Sub(i)) => i as c_int,
                    _ => -1,
                };
                let changed = self.active_sub != new_sub;
                self.active_sub = new_sub;
                // the client renderer takes the EMBEDDED-subtitle ordinal (what the demuxer
                // enumerates); an external pick has no demux ordinal — it is drawn by the sidecar
                // renderer on direct play, or burned
                let ridx = tracks(meta)
                    .filter(|_| new_sub >= 0)
                    .map(|t| metadata::sub_render_ordinal(&t.subs, new_sub as usize))
                    .unwrap_or(-1);
                if changed {
                    crate::diag::event(crate::diag::schema::DiagEvent::FeatureUsed {
                        feature: crate::diag::schema::Feature::SubtitleTrack,
                    });
                }
                let sidecar = tracks(meta)
                    .filter(|_| new_sub >= 0)
                    .and_then(|t| t.subs.get(new_sub as usize))
                    .filter(|s| s.sidecar_renderable());
                TrackOk::Commit {
                    commit: TrackCommit::Subtitle {
                        render_ordinal: ridx,
                        stream_id: self.sub_stream_id(meta),
                        sidecar_key: sidecar.map(|s| s.key.clone()),
                        sidecar_codec: sidecar.map(|s| s.codec.clone()).unwrap_or_default(),
                    },
                    keep_open: false,
                }
            }
        }
    }

    /// The Audio tab's sections and row map: the track list, plus — only when
    /// [`Self::enhance_shown`] is `Some` (I1/I2: the row set itself is the gate, never a greyed
    /// row) — a second, headerless section carrying the two Plex Pass DSP toggles, the same "own
    /// section, no header" idiom the Subtitles tab's Timing/Color pair uses. The returned
    /// `Vec<AudioRowTarget>` names each row in the same order the sections draw them, mirroring
    /// [`Self::layout`]'s `(Vec<Section>, Vec<RowTarget>)` for the Subtitles tab.
    fn build_audio(&self, meta: metadata::MetadataView<'_>) -> (Vec<Section>, Vec<AudioRowTarget>) {
        let mut sec = Section::new(crate::i18n::msg::widgets_tracks_audio());
        let d = match tracks(meta) {
            Some(t) => t,
            None => return (vec![sec], Vec::new()),
        };
        let names = crate::player::SHARED.track_names.lock().unwrap();
        let mut targets = Vec::new();
        for (i, s) in d.audio.iter().enumerate() {
            let lang = if s.lang.is_empty() {
                crate::i18n::msg::widgets_tracks_unknown()
            } else {
                s.lang.as_str()
            };
            let label = if s.default {
                crate::i18n::msg::widgets_tracks_original(lang)
            } else {
                lang.to_string()
            };
            let mut row = Row::new(label).checked(i as c_int == self.active_audio());
            // a per-track descriptor so sibling tracks in the same language are distinguishable
            // (e.g. two Russian tracks: "Дубляж" vs "AC-3 5.1"). Prefer the stream title, else the
            // codec + channel layout.
            let name = track_label::track_name(
                &s.title,
                names.audio(crate::metadata::audio_ordinal(&d.audio, i)),
                lang,
            );
            let sub = if name.is_empty() {
                audio_descriptor(s)
            } else {
                name
            };
            if !sub.is_empty() {
                row = row.detail(sub);
            }
            if s.ad {
                row = row.badge(Badge::Ad);
            }
            sec = sec.row(row);
            targets.push(AudioRowTarget::Track(i));
        }
        let mut sections = vec![sec];
        if let Some(shown) = self.enhance_shown {
            let enh = Section::new("")
                .row(Row::new(crate::i18n::msg::widgets_tracks_boost_dialog()).toggle(shown.boost_dialog))
                .row(Row::new(crate::i18n::msg::widgets_tracks_normalize_loudness()).toggle(shown.normalize_loudness));
            sections.push(enh);
            targets.push(AudioRowTarget::Boost);
            targets.push(AudioRowTarget::Loudness);
        }
        (sections, targets)
    }

    /// Build the Subtitles tab's sections and row map from the CURRENT state — the one place the
    /// model (`metadata::sub_layout::sub_sections`) is asked, so what a row IS can never disagree
    /// with what was drawn.
    fn layout(&self, ps: &crate::route::PlaybackSession, meta: metadata::MetadataView<'_>) -> (Vec<Section>, Vec<RowTarget>) {
        let item = tracks(meta);
        let subs: &[metadata::Stream] = item.map(|t| t.subs.as_slice()).unwrap_or(&[]);
        let offered = visible_subs(ps, meta);
        let names = crate::player::SHARED.track_names.lock().unwrap();
        let model = sub_layout::sub_sections(subs, &offered, &names, &self.yours, !crate::route::is_transcoding(ps));
        table_sections(&model, self.active_sub, self.offset_ms, self.tone)
    }

    fn rebuild(&mut self, ps: &crate::route::PlaybackSession, meta: metadata::MetadataView<'_>, tab: c_int, slide: bool) {
        if tab == 0 {
            // I1/I2: the offer is read from the live route on every (re)build, so the rows are
            // simply ABSENT without Plex Pass (or an unknown subscription) — never drawn dim.
            self.rebuild_audio(crate::route::menu_enhancements(ps), meta, slide);
        } else {
            let (sections, targets) = self.layout(ps, meta);
            let sel = sel_for_targets(&targets, self.active_sub);
            self.targets = targets;
            self.audio_targets = Vec::new();
            self.table.set_sections(sections, sel, slide);
        }
    }

    /// The Audio tab's half of [`Self::rebuild`], taking the offer/displayed answer rather than
    /// recomputing it — `update`'s per-frame poll already has it fresh, and handing it here keeps
    /// `route::menu_enhancements(ps)` to exactly one call per rebuild instead of two.
    fn rebuild_audio(&mut self, enhance_shown: Option<crate::plex::AudioEnhancements>, meta: metadata::MetadataView<'_>, slide: bool) {
        self.targets = Vec::new();
        self.enhance_shown = enhance_shown;
        let (sections, targets) = self.build_audio(meta);
        self.audio_targets = targets;
        self.table.set_sections(sections, self.active_audio().max(0), slide);
    }

    /// The panel geometry — shared by `update` and `draw` so scrolling math matches.
    fn panel_rect(&self) -> Rect {
        let pw = if self.tab == 0 { AUDIO_PANEL_W } else { SUB_PANEL_W };
        // the transport control row's own right edge — one number for the discs and both panels
        let px = crate::ui::player_hud::CTRL_RIGHT - pw;
        // Bottom-anchored just above the control-button row (buttons top at SCR_H-288) with a clear gap.
        // The panel grows UPWARD from this fixed bottom edge, and its height is capped so the top never
        // crosses `top_min` — so a long list (an item with many audio dubs) SCROLLS inside the panel
        // instead of the panel itself spilling down over the buttons. Switching Audio↔Subtitles keeps
        // the bottom edge steady.
        let bottom = SCR_H - 316.0; // 764 — ~28px above the buttons
        let top_min = 60.0;
        let ph = self.table.measured_height().clamp(160.0, bottom - top_min);
        let py = bottom - ph; // ≥ top_min by construction
        Rect::new(px, py, pw, ph)
    }

    /// `ps`/`meta` are read only for the Audio tab, and only to notice a LIVE change: a request
    /// this menu itself fired settles asynchronously (the server's `EnhancementOutcome`, or a
    /// mid-play route change moving the family in or out of `Remux`), and the two rows must
    /// track that the moment it lands rather than freeze at whatever `on_ok`/`rebuild` last drew
    /// — otherwise a refusal leaves a row reading "On" for a preference the route already gave up
    /// on. `rebuild`'s own recomputation of `enhance_shown` is the single source of truth here
    /// too, so this only ever asks "did that answer change since last frame", never rebuilds it a
    /// second, divergent way.
    pub(crate) fn update(&mut self, dt: f32, ps: &crate::route::PlaybackSession, meta: metadata::MetadataView<'_>) {
        if self.tab == 0 {
            let live = crate::route::menu_enhancements(ps);
            if live != self.enhance_shown {
                self.rebuild_audio(live, meta, false);
            }
        }
        // `update` subtracts its own top/bottom padding now — pass the panel's raw height.
        let h = self.panel_rect().h;
        self.table.update(dt, h);
    }

    pub(crate) fn draw(&mut self, appear: f32, measure: &dyn crate::ui::machine::Measure) {
        // The appear fade/rise — the container drives the phase and the appear spring. The dim
        // over the video plane is the container's too (`PlayerOverlayScreen::scrim`,
        // `theme::underlay::DIM_PLAYER`), painted at the end of the player's page pass.
        let p = Painter::root()
            .alpha(appear)
            .translate(0.0, Popover::RISE * (1.0 - appear));
        let r = self.panel_rect();

        // frosted panel card — near-opaque dark (no true backdrop blur on the GLES plane, so a solid
        // dark card approximates it); only a hint of video shows through
        p.rect(r, 28.0, theme::PANEL_TOP, theme::PANEL_BOT, 0.0);

        self.table.draw(p, r, measure);
    }
}

/// **The Engine-shaped view of this popover** (restructure phase 12): one `Column` focus group
/// over the ACTIVE tab's rows, built fresh by `screens::player::overlay::PlayerOverlayScreen`
/// each frame from a `&TrackMenuState` — the same borrowed-view shape `ui::more_menu::MoreMenuPart`
/// and `ui::table_screen::TablePart` use for the other bare-`TableView` panels, so this popover
/// answers the same [`Focusable`]/[`Part`] query protocol they do. LEFT/RIGHT are NOT a move
/// within the group — they switch the whole row set to the other tab, which only the owning
/// screen can do (mirroring [`TrackMenuState::focus_tab`]), so both edges answer
/// [`EdgeRule::Screen`], the same idiom `TablePart` uses for a RIGHT edge the screen itself must
/// interpret.
///
/// **`state` is a SHARED reference, not `&mut`** — every [`Focusable`] method here is a pure read
/// (`&self`), and the screen's own `Focusable` impl only ever has `&self` too (the engine holds
/// screens behind `&dyn Screen`, §7.1's "the engine never mutates a screen"), so a mutable field
/// would make this type unconstructable from there. The actual PAINT (`TrackMenuState::draw`,
/// which needs `&mut` for its own lazy layout work) stays a direct call on the owned `Panel` from
/// `PlayerOverlayScreen::draw`'s `&mut self`; [`Part::draw`] below only registers stops, which is
/// read-only geometry like everything else in this impl.
pub(crate) struct TrackMenuPart<'a> {
    pub(crate) state: &'a TrackMenuState,
    pub(crate) entry: EntryId,
    pub(crate) group: GroupId,
}

impl<H: Host> Focusable<H> for TrackMenuPart<'_>
where
    H::Elem: IndexElem,
{
    fn groups(&self, _cx: &Cx<'_, H>, out: &mut Vec<GroupSpec>) {
        out.push(GroupSpec {
            id: self.group,
            kind: GroupKind::Column,
            seat: Seat::Remembered,
            reachable: AxisMask::VERTICAL,
            edge: [EdgeRule::Stop, EdgeRule::Stop, EdgeRule::Screen, EdgeRule::Screen],
            extent: self.state.panel_rect(),
            len: self.state.table.n_rows().max(0) as usize,
            elem: ElemKind::Bare,
        });
    }
    fn group_of(&self, key: &H::Elem, _cx: &Cx<'_, H>) -> Option<GroupId> {
        ((key.index()? as i32) < self.state.table.n_rows()).then_some(self.group)
    }
    fn neighbour(&self, key: FocusKey<H::Elem>, dir: Dir, _cx: &Cx<'_, H>) -> Step<H::Elem> {
        let Some(i) = key.elem.index() else {
            return Step::Edge;
        };
        let delta = match dir {
            Dir::Up => -1,
            Dir::Down => 1,
            _ => return Step::Edge, // Left/Right: the screen's own tab switch, via `EdgeRule::Screen`
        };
        match self.state.table.next_selectable(i as i32, delta) {
            Some(j) => Step::Move(FocusKey { entry: self.entry, elem: H::Elem::of_index(j as u32) }),
            None => Step::Edge,
        }
    }
    fn place(&self, key: &H::Elem, _cx: &Cx<'_, H>, _at: At) -> Option<Placed> {
        let i = key.index()?;
        let r = self.state.table.row_frame(self.state.panel_rect(), i as i32)?;
        Some(Placed {
            rect: r,
            rest_rect: r,
            clip: self.state.panel_rect(),
            index: Some(i),
        })
    }
    fn reconcile(&self, want: FocusKey<H::Elem>, _cx: &Cx<'_, H>) -> FocusKey<H::Elem> {
        let i = want.elem.index().unwrap_or(0) as i32;
        FocusKey {
            entry: self.entry,
            elem: H::Elem::of_index(self.state.table.settle(i).max(0) as u32),
        }
    }
    fn seat(&self, _g: GroupId, _from: Placed, _cx: &Cx<'_, H>) -> FocusKey<H::Elem> {
        FocusKey {
            entry: self.entry,
            elem: H::Elem::of_index(self.state.table.sel.max(0) as u32),
        }
    }
}

impl<H: Host> Part<H> for TrackMenuPart<'_>
where
    H::Elem: IndexElem,
{
    fn prepare(&mut self, _b: &mut Budget, _cx: &Cx<'_, H>) {}
    /// Registers every visible row's stop (§7.6); the panel's own paint happens directly on the
    /// owned `TrackMenuState` from `PlayerOverlayScreen::draw` (see the struct doc above).
    fn draw(&mut self, f: &mut DrawFrame<'_, '_, H>, _rect: Rect) {
        let p = Painter::root();
        let r = self.state.panel_rect();
        for i in 0..self.state.table.n_rows() {
            if self.state.table.next_selectable(i, 0) != Some(i) {
                continue;
            }
            if let Some(row) = self.state.table.row_frame(r, i) {
                f.stop(
                    p,
                    Stop {
                        key: FocusKey {
                            entry: self.entry,
                            elem: H::Elem::of_index(i as u32),
                        },
                        rect: row,
                        rest_rect: row,
                        clip: r,
                        hover: Hover::Focus,
                        activate: Activate::Direct,
                    },
                );
            }
        }
    }
}

/// The PLAYING item's track lists — the menu's ONLY data source. `metadata::current()` is the
/// detail page's item, which is the SHOW during an episode play (its lists are episode 1's) and
/// can be a different item entirely when playing straight from Home.
fn tracks<'a>(meta: metadata::MetadataView<'a>) -> Option<&'a metadata::PlayingItem> {
    meta.playing()
}

fn n_audio(meta: metadata::MetadataView<'_>) -> c_int {
    tracks(meta).map(|t| t.audio.len()).unwrap_or(0) as c_int
}
/// Subtitle rows offered on this route: text sidecars can be drawn on direct play;
/// all sidecars are offered during transcoding, when the server burns them.
fn visible_subs(ps: &crate::route::PlaybackSession, meta: metadata::MetadataView<'_>) -> Vec<usize> {
    tracks(meta)
        .map(|t| {
            t.subs
                .iter()
                .enumerate()
                .filter(|(_, s)| {
                    !s.external || s.sidecar_renderable() || crate::route::is_transcoding(ps)
                })
                .map(|(i, _)| i)
                .collect()
        })
        .unwrap_or_default()
}

/// Resolve the typed tone at the UI boundary; persisted values and technical logs stay stable.
fn tone_label(tone: SubtitleTone) -> &'static str {
    match tone {
        SubtitleTone::White => crate::i18n::msg::widgets_tracks_tone_white(),
        SubtitleTone::Silver => crate::i18n::msg::widgets_tracks_tone_silver(),
        SubtitleTone::LightGrey => crate::i18n::msg::widgets_tracks_tone_light_grey(),
        SubtitleTone::Grey => crate::i18n::msg::widgets_tracks_tone_grey(),
        SubtitleTone::DarkGrey => crate::i18n::msg::widgets_tracks_tone_dark_grey(),
        SubtitleTone::Charcoal => crate::i18n::msg::widgets_tracks_tone_charcoal(),
    }
}

/// An offset as localized signed seconds to the tenth (`ui::timing_capsule::offset_seconds_in`,
/// the one offset formatter).
fn format_offset(ms: i64) -> String {
    crate::ui::timing_capsule::offset_seconds_in(ms, true, crate::i18n::current())
}

/// The row whose `RowTarget` names the checked subtitle track (or `Off`), by position in
/// `targets` — the counterpart to the audio tab's `active_audio().max(0)`.
fn sel_for_targets(targets: &[RowTarget], active_sub: c_int) -> c_int {
    targets
        .iter()
        .position(|t| match t {
            RowTarget::Off => active_sub < 0,
            RowTarget::Sub(i) => active_sub >= 0 && *i == active_sub as usize,
            RowTarget::Timing | RowTarget::Color => false,
        })
        .unwrap_or(0) as c_int
}

// ---- section building ----
use crate::metadata::friendly_codec; // the ONE codec→display-name map (shared with the Info card)
use crate::metadata::track_label::Kind;

/// "AC-3 5.1", "Dolby TrueHD 7.1", "DTS 5.1" — a compact codec + channel-layout descriptor.
fn audio_descriptor(s: &metadata::Stream) -> String {
    let codec = friendly_codec(&s.codec);
    let ch = if !s.layout.is_empty() {
        channel_short(&s.layout)
    } else if s.channels > 0 {
        match s.channels {
            1 => crate::i18n::msg::widgets_tracks_mono().to_string(),
            2 => crate::i18n::msg::widgets_tracks_stereo().to_string(),
            n => format!("{}.{}", n - 1, if n >= 6 { 1 } else { 0 }),
        }
    } else {
        String::new()
    };
    match (codec.is_empty(), ch.is_empty()) {
        (false, false) => format!("{codec} {ch}"),
        (false, true) => codec,
        (true, false) => ch,
        _ => String::new(),
    }
}

/// map a Plex audioChannelLayout ("5.1(side)", "7.1") to a short "5.1"/"7.1"/"Stereo"
fn channel_short(layout: &str) -> String {
    let base = layout.split('(').next().unwrap_or(layout).trim();
    match base {
        "mono" => crate::i18n::msg::widgets_tracks_mono().to_string(),
        "stereo" => crate::i18n::msg::widgets_tracks_stereo().to_string(),
        other => other.to_string(),
    }
}

// ---- Subtitles-tab sections (the model is `metadata::sub_layout`, plan §3) ------------------

/// The drawn form of a row's one badge.
fn row_badge(b: &RowBadge) -> Badge {
    match b {
        RowBadge::Forced => Badge::Forced,
        RowBadge::Sdh => Badge::Sdh,
        RowBadge::External => Badge::Text(crate::i18n::msg::widgets_tracks_external_badge().to_string()),
        RowBadge::Codec(c) => Badge::Text(c.clone()),
    }
}

/// A flat row: label = language, detail = source/region (+ "Track N" when this track needed one),
/// one badge. Used for a single-track "yours" language, and for every "Other languages" row.
fn flat_row(t: &SubTrack, active_sub: c_int) -> Row {
    let mut row = Row::new(t.lang.clone()).checked(active_sub >= 0 && t.i == active_sub as usize);
    let mut parts: Vec<String> = Vec::new();
    if !t.detail.is_empty() {
        parts.push(t.detail.clone());
    }
    if let Some(n) = t.ordinal {
        parts.push(crate::i18n::msg::widgets_tracks_track_ordinal(n as i64));
    }
    if !parts.is_empty() {
        row = row.detail(parts.join(" \u{b7} "));
    }
    if let Some(b) = &t.badge {
        row = row.badge(row_badge(b));
    }
    row
}

/// A row inside a multi-track "yours" language section (`player.html:1110-1115`): label = the
/// source (+ "Track N" if needed), or — for a NAMELESS track — the kind word itself ("Forced",
/// "SDH", "Full", "Commentary"), in which case a Forced/SDH badge that would only repeat the
/// label is dropped.
fn in_lang_row(t: &SubTrack, active_sub: c_int) -> Row {
    let nth = t.ordinal.map(|n| crate::i18n::msg::widgets_tracks_track_ordinal(n as i64));
    let label = if !t.detail.is_empty() {
        match &nth {
            Some(n) => format!("{} \u{b7} {}", t.detail, n),
            None => t.detail.clone(),
        }
    } else if let Some(n) = &nth {
        if t.kind == Kind::Full {
            n.clone()
        } else {
            format!("{} \u{b7} {}", t.kind.fallback_label(), n)
        }
    } else {
        t.kind.fallback_label().to_string()
    };
    let mut row = Row::new(label).checked(active_sub >= 0 && t.i == active_sub as usize);
    let drop_badge_for_kind = t.detail.is_empty()
        && t.ordinal.is_none()
        && matches!(t.badge, Some(RowBadge::Forced) | Some(RowBadge::Sdh));
    if !drop_badge_for_kind {
        if let Some(b) = &t.badge {
            row = row.badge(row_badge(b));
        }
    }
    row
}

/// **Draw the Subtitles model** (`metadata::sub_layout::sub_sections`) as `TableView` sections —
/// the catalog words for each header, the checkmark on `active_sub` (-1 for Off), and the
/// Timing/Color read-outs (`offset_ms`, `tone`; Timing is dim while subtitles are Off).
///
/// Returns the sections AND a flat `targets` vec, one [`SubRow::target`] per row in the SAME
/// order the sections draw — [`TrackMenuState::on_ok`] reads back what a row IS from
/// `targets[sel]` rather than re-deriving it.
fn table_sections(
    model: &[SubSection],
    active_sub: c_int,
    offset_ms: i64,
    tone: SubtitleTone,
) -> (Vec<Section>, Vec<RowTarget>) {
    use crate::i18n::msg;
    let mut targets = Vec::new();
    let sections = model
        .iter()
        .map(|sec| {
            let mut out = match &sec.header {
                SubHeader::Subtitles => Section::new(msg::widgets_tracks_subtitles()),
                SubHeader::Language { name, tracks } => {
                    Section::new(name.clone()).accessory(msg::widgets_tracks_count(*tracks as i64))
                }
                SubHeader::Bare => Section::new(""),
                SubHeader::OtherLanguages { languages } => Section::new(msg::widgets_tracks_other_languages())
                    .accessory(msg::widgets_tracks_language_count(*languages as i64)),
            };
            for row in &sec.rows {
                targets.push(row.target());
                out = out.row(match row {
                    SubRow::Off => Row::new(msg::widgets_tracks_off()).checked(active_sub < 0),
                    SubRow::Flat(t) => flat_row(t, active_sub),
                    SubRow::InLanguage(t) => in_lang_row(t, active_sub),
                    SubRow::Timing => Row::new(msg::widgets_tracks_timing())
                        .value(format_offset(offset_ms))
                        .chevron(true)
                        .dim(active_sub < 0),
                    SubRow::Color => Row::new(msg::widgets_tracks_color()).value(tone_label(tone)),
                });
            }
            out
        })
        .collect();
    (sections, targets)
}

/// The panel at its WIDEST and TALLEST, for the overscan audit ([`crate::ui::consts::SAFE`]) — both
/// tab widths and the full `top_min`→`bottom` span, since the measured height comes from a
/// `TableView` no host test can measure.
#[cfg(test)]
pub(crate) fn overscan_rects(out: &mut Vec<(&'static str, Rect)>) {
    let (bottom, top_min) = (SCR_H - 316.0, 60.0);
    for (name, pw) in [
        ("track menu panel (audio)", AUDIO_PANEL_W),
        ("track menu panel (subtitles)", SUB_PANEL_W),
    ] {
        out.push((
            name,
            Rect::new(
                crate::ui::player_hud::CTRL_RIGHT - pw,
                top_min,
                pw,
                bottom - top_min,
            ),
        ));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::player::TrackNames;

    /// The model and its drawing in one call — what the panel shows for these inputs.
    #[allow(clippy::too_many_arguments)]
    fn sub_layout(
        subs: &[metadata::Stream],
        offered: &[usize],
        names: &TrackNames,
        yours: &[&str],
        active_sub: c_int,
        show_timing: bool,
        offset_ms: i64,
        tone: SubtitleTone,
    ) -> (Vec<Section>, Vec<RowTarget>) {
        let yours: Vec<String> = yours.iter().map(|y| y.to_string()).collect();
        let model = sub_layout::sub_sections(subs, offered, names, &yours, show_timing);
        table_sections(&model, active_sub, offset_ms, tone)
    }

    /// A store with `subs` installed as the playing item's subtitle list.
    fn store_with(subs: Vec<metadata::Stream>) -> crate::stores::metadata::MetadataStore {
        let mut store = crate::stores::metadata::MetadataStore::default();
        assert!(store.run(crate::stores::metadata::MetadataCmd::InstallPlaying(Some(
            metadata::PlayingItem::with_subs(subs)
        ))));
        store
    }

    /// A store with `audio` installed as the playing item's audio list — the audio-tab
    /// counterpart to [`store_with`]. `pub(super)`: `enhancement_menu_tests` below builds the
    /// same fixture shape for the Audio tab's DSP toggle rows (issue #266 PR 4).
    pub(super) fn store_with_audio(audio: Vec<metadata::Stream>) -> crate::stores::metadata::MetadataStore {
        let mut store = crate::stores::metadata::MetadataStore::default();
        let mut item = metadata::PlayingItem::with_subs(Vec::new());
        item.audio = audio;
        assert!(store.run(crate::stores::metadata::MetadataCmd::InstallPlaying(Some(item))));
        store
    }

    fn stream(id: i64, index: i64, lang: &str, lang_code: &str, title: &str) -> metadata::Stream {
        metadata::Stream {
            id,
            index,
            lang: lang.into(),
            lang_code: lang_code.into(),
            codec: "srt".into(),
            title: title.into(),
            ..Default::default()
        }
    }

    // ---- sub_layout: a single "yours" language is flat under "Subtitles" ----------------------

    #[test]
    fn a_single_track_yours_language_is_a_flat_row_under_subtitles() {
        let subs = vec![stream(1, 0, "Spanish", "spa", "")];
        let names = TrackNames::new();
        let (sections, targets) = sub_layout(&subs, &[0], &names, &["spa"], -1, true, 0, SubtitleTone::White);
        assert_eq!(sections[0].header, "Subtitles");
        assert_eq!(sections[0].rows.len(), 2, "Off + the one track");
        assert_eq!(sections[0].rows[1].label, "Spanish");
        assert_eq!(targets[1], RowTarget::Sub(0));
    }

    // ---- sub_layout: a nameless track reads as its kind, with no badge ------------------------

    #[test]
    fn a_nameless_track_in_a_multitrack_group_is_labelled_by_its_kind_with_no_badge() {
        let subs = vec![
            stream(1, 0, "Russian", "rus", ""), // full — keeps the bucket multi-track
            stream(2, 1, "Russian", "rus", "Форс."), // forced, nameless
        ];
        let offered: Vec<usize> = (0..subs.len()).collect();
        let names = TrackNames::new();
        let (sections, _targets) =
            sub_layout(&subs, &offered, &names, &["rus"], -1, true, 0, SubtitleTone::White);
        let forced_row = sections[1]
            .rows
            .iter()
            .find(|r| r.label == "Forced")
            .expect("a nameless forced track reads as its kind word");
        assert!(
            forced_row.badges.is_empty(),
            "the kind word already says Forced; the badge is dropped"
        );
    }

    // ---- sub_layout: identical tracks get "Track N" --------------------------------------------

    #[test]
    fn identical_tracks_in_one_group_are_told_apart_by_an_ordinal() {
        let subs = vec![
            stream(1, 0, "Russian", "rus", "iTunes"),
            stream(2, 1, "Russian", "rus", "iTunes"),
        ];
        let offered: Vec<usize> = (0..subs.len()).collect();
        let names = TrackNames::new();
        let (sections, _targets) =
            sub_layout(&subs, &offered, &names, &["rus"], -1, true, 0, SubtitleTone::White);
        let labels: Vec<&str> = sections[1].rows.iter().map(|r| r.label.as_str()).collect();
        assert_eq!(labels, ["iTunes \u{b7} Track 1", "iTunes \u{b7} Track 2"]);
    }

    // ---- sub_layout: one badge per row, by priority --------------------------------------------

    #[test]
    fn one_badge_per_row_by_priority_forced_over_sdh_over_external_over_codec() {
        let mk = |sdh: bool, external: bool, codec: &str| metadata::Stream {
            sdh,
            external,
            lang: "English".into(),
            lang_code: "eng".into(),
            codec: codec.into(),
            ..Default::default()
        };
        let names = TrackNames::new();

        let subs = vec![mk(true, false, "srt")];
        let (sections, _) = sub_layout(&subs, &[0], &names, &[], -1, true, 0, SubtitleTone::White);
        assert!(matches!(sections.last().unwrap().rows[0].badges.as_slice(), [Badge::Sdh]));

        let subs = vec![mk(false, true, "srt")];
        let (sections, _) = sub_layout(&subs, &[0], &names, &[], -1, true, 0, SubtitleTone::White);
        assert!(matches!(sections.last().unwrap().rows[0].badges.as_slice(), [Badge::Text(t)] if t == "EXTERNAL"));

        let subs = vec![mk(true, true, "srt")];
        let (sections, _) = sub_layout(&subs, &[0], &names, &[], -1, true, 0, SubtitleTone::White);
        assert!(
            matches!(sections.last().unwrap().rows[0].badges.as_slice(), [Badge::Sdh]),
            "SDH beats EXTERNAL"
        );

        let subs = vec![mk(false, false, "pgs")];
        let (sections, _) = sub_layout(&subs, &[0], &names, &[], -1, true, 0, SubtitleTone::White);
        assert!(matches!(sections.last().unwrap().rows[0].badges.as_slice(), [Badge::Text(t)] if t == "PGS"));
    }

    // ---- sub_layout: "Other languages", "N languages", name sort ------------------------------

    #[test]
    fn other_languages_are_flat_sorted_by_name_with_a_language_count_accessory() {
        let subs = vec![
            stream(1, 0, "German", "deu", ""),
            stream(2, 1, "Arabic", "ara", ""),
        ];
        let offered: Vec<usize> = (0..subs.len()).collect();
        let names = TrackNames::new();
        let (sections, _targets) =
            sub_layout(&subs, &offered, &names, &[], -1, true, 0, SubtitleTone::White);
        let other = sections.last().unwrap();
        assert_eq!(other.header, "Other languages");
        assert_eq!(other.accessory, "2 languages");
        let labels: Vec<&str> = other.rows.iter().map(|r| r.label.as_str()).collect();
        assert_eq!(labels, ["Arabic", "German"], "sorted by language name");

        let subs = vec![stream(1, 0, "German", "deu", "")];
        let (sections, _targets) = sub_layout(&subs, &[0], &names, &[], -1, true, 0, SubtitleTone::White);
        assert_eq!(sections.last().unwrap().accessory, "1 language");
    }

    /// **Every Subtitles-panel row fits the panel in every shipped language** — the grouped
    /// layout's section words, the kind fallbacks, the "Track N" ordinal and a region name beside
    /// each badge, at [`SUB_PANEL_W`], measured with the device's whole-pixel advances. (A source is
    /// server text and may elide; the fixture's sources are short so only app text is judged.)
    #[test]
    fn every_subtitles_row_fits_the_panel_in_every_language() {
        use crate::fontcov::advances::{ShippedMeasure, HEADROOM};
        use crate::i18n::{language_on_this_thread_for_test, Preference};
        let mut subs = vec![
            stream(1, 0, "Russian", "rus", "forced, DVD R5"),
            stream(2, 1, "Russian", "rus", "Netflix"),
            stream(3, 2, "Russian", "rus", ""),
            stream(4, 3, "Russian", "rus", ""),
            stream(5, 4, "Russian", "rus", "SDH"),
            stream(6, 5, "Russian", "rus", "Commentary"),
            stream(7, 6, "Spanish", "spa", ""),
            stream(8, 7, "Portuguese", "por", "Full SDH"),
        ];
        subs[6].language_tag = "es-419".into();
        subs[7].external = true;
        let offered: Vec<usize> = (0..subs.len()).collect();
        let names = TrackNames::new();
        let mut out = Vec::new();
        for language in [Preference::En, Preference::Es, Preference::Be] {
            let _guard = language_on_this_thread_for_test(language);
            let (sections, _) =
                sub_layout(&subs, &offered, &names, &["rus"], 1, true, -30_000, SubtitleTone::LightGrey);
            let mut table = TableView::new();
            table.set_sections(sections, 0, false);
            out.extend(table.elided_rows(SUB_PANEL_W, &ShippedMeasure, HEADROOM)
                .into_iter().map(|e| format!("{}: {e}", language.tag())));
        }
        assert!(out.is_empty(), "rows the panel would end in an ellipsis:\n  {}", out.join("\n  "));
    }

    /// **The pseudo-locale sweep of the grouped Subtitles panel**: every header, accessory, label,
    /// detail, value and badge it builds is catalog text (the pseudo-locale's `[!!` marker or its
    /// accented vowels), the fixture's own server values, or letter-free.
    #[test]
    fn every_app_owned_subtitles_string_comes_from_the_catalog() {
        let _pseudo = crate::i18n::pseudo_on_this_thread_for_test();
        // a title's own "Commentary" stays its source (the mock strips no commentary word), so it
        // is server text here like the rest
        let server = ["Russian", "Spanish", "Portuguese", "Netflix", "DVD R5", "Commentary"];
        let mut subs = vec![
            stream(1, 0, "Russian", "rus", "Netflix"),
            stream(2, 1, "Russian", "rus", ""),
            stream(3, 2, "Russian", "rus", ""),
            stream(4, 3, "Russian", "rus", "forced"),
            stream(5, 4, "Russian", "rus", "SDH"),
            stream(6, 5, "Russian", "rus", "Commentary"),
            stream(7, 6, "Spanish", "spa", ""),
            stream(8, 7, "Portuguese", "por", "DVD R5"),
            stream(9, 8, "", "", ""),
        ];
        subs[6].language_tag = "es-419".into();
        subs[7].external = true;
        let offered: Vec<usize> = (0..subs.len()).collect();
        let (sections, _) =
            sub_layout(&subs, &offered, &TrackNames::new(), &["rus"], 1, true, 300, SubtitleTone::Grey);
        let mut runs: Vec<String> = Vec::new();
        for sec in &sections {
            runs.push(sec.header.clone());
            runs.push(sec.accessory.clone());
            for row in &sec.rows {
                runs.push(row.label.clone());
                runs.push(row.detail.clone());
                runs.extend(row.value.clone());
                runs.extend(row.badges.iter().map(|b| b.text().to_string()));
            }
        }
        let pseudo = |run: &str| run.contains("[!!") || run.contains(['á', 'ë', 'ï', 'ö', 'ü']);
        let stray: Vec<&String> = runs
            .iter()
            .filter(|run| !pseudo(run))
            .filter(|run| {
                let mut rest = run.replace('\u{b7}', " ");
                for value in server {
                    rest = rest.replace(value, "");
                }
                rest.chars().any(char::is_alphabetic)
            })
            .collect();
        assert!(stray.is_empty(), "text drawn without the catalog: {stray:?}");
    }

    // ---- sub_layout: Timing absent under transcode, dim while Off -----------------------------

    #[test]
    fn timing_is_omitted_under_transcode_and_dim_while_subtitles_are_off() {
        let subs = vec![stream(1, 0, "English", "eng", "")];
        let names = TrackNames::new();

        let (sections, _) = sub_layout(&subs, &[0], &names, &[], -1, false, 0, SubtitleTone::White);
        assert!(
            sections.iter().flat_map(|s| &s.rows).all(|r| r.label != "Timing"),
            "a transcode burns captions; no client offset can reach them"
        );

        let (sections, _) = sub_layout(&subs, &[0], &names, &[], -1, true, 0, SubtitleTone::White);
        let timing = sections
            .iter()
            .flat_map(|s| &s.rows)
            .find(|r| r.label == "Timing")
            .expect("Timing row");
        assert!(timing.dim, "subtitles are Off");

        let (sections, _) = sub_layout(&subs, &[0], &names, &[], 0, true, 0, SubtitleTone::White);
        let timing = sections
            .iter()
            .flat_map(|s| &s.rows)
            .find(|r| r.label == "Timing")
            .expect("Timing row");
        assert!(!timing.dim);
    }

    // ---- track_menu: the targets mapping -------------------------------------------------------

    #[test]
    fn targets_map_flat_rows_to_off_sub_timing_and_color_in_drawn_order() {
        let _g = crate::testlock::serial();
        crate::player::sidecar::reset();
        crate::player::set_subtitle_offset(0);
        let ps = crate::route::PlaybackSession::IDLE;
        let store = store_with(vec![crate::metadata::Stream {
            id: 1,
            index: 0,
            lang: "English".into(),
            lang_code: "eng".into(),
            codec: "srt".into(),
            ..Default::default()
        }]);
        let menu = TrackMenuState::new(&ps, store.view(), 1, Vec::new());
        assert_eq!(
            menu.targets,
            vec![RowTarget::Off, RowTarget::Timing, RowTarget::Color, RowTarget::Sub(0)]
        );
    }

    // ---- track_menu: a rebuild lands on the checked sub inside a group -------------------------

    #[test]
    fn a_rebuild_lands_on_the_checked_sub_inside_a_multitrack_group() {
        let _g = crate::testlock::serial();
        crate::player::sidecar::reset();
        let ps = crate::route::PlaybackSession::IDLE;
        let store = store_with(vec![
            crate::metadata::Stream {
                id: 1,
                index: 0,
                lang: "Russian".into(),
                lang_code: "rus".into(),
                codec: "srt".into(),
                title: "iTunes".into(),
                ..Default::default()
            },
            crate::metadata::Stream {
                id: 2,
                index: 1,
                lang: "Russian".into(),
                lang_code: "rus".into(),
                codec: "srt".into(),
                title: "Netflix".into(),
                ..Default::default()
            },
        ]);
        let mut menu = TrackMenuState::new(&ps, store.view(), 1, vec!["rus".into()]);
        menu.active_sub = 1; // the second track is the checked one
        menu.rebuild(&ps, store.view(), 1, false);
        assert_eq!(menu.targets.get(menu.sel() as usize).copied(), Some(RowTarget::Sub(1)));
        assert_eq!(menu.sel(), sel_for_targets(&menu.targets, 1));
    }

    // ---- track_menu: sidecar, tone and Timing rows dispatch to their own outcomes -------------

    /// **A Subtitles-panel row is a track, Color, or Timing, never ambiguous — and the split is
    /// by `targets[sel]`.** Off + an embedded English track + an external French sidecar (none of
    /// them "yours", so all three land flat under "Subtitles"/"Other languages" respectively),
    /// then the headerless Timing/Color section.
    #[test]
    fn sidecar_and_settings_rows_map_to_their_own_commits_in_one_menu() {
        let _g = crate::testlock::serial();
        crate::player::sidecar::reset();
        crate::player::set_subtitle_offset(0);
        crate::player::restore_subtitle_tone(SubtitleTone::White);
        let ps = crate::route::PlaybackSession::IDLE;
        let store = store_with(vec![
            crate::metadata::Stream {
                id: 41,
                index: 0,
                lang: "English".into(),
                lang_code: "eng".into(),
                codec: "srt".into(),
                ..Default::default()
            },
            crate::metadata::Stream {
                id: 42,
                index: 1,
                lang: "French".into(),
                lang_code: "fre".into(),
                codec: "srt".into(),
                external: true,
                key: "/library/streams/42.srt".into(),
                ..Default::default()
            },
        ]);
        let mut menu = TrackMenuState::new(&ps, store.view(), 1, Vec::new());
        // Off(0), Timing(1), Color(2), English(3), French sidecar(4) — "Other languages" sorts
        // English before French, and the settings section always precedes it.
        assert_eq!(
            menu.targets,
            vec![
                RowTarget::Off,
                RowTarget::Timing,
                RowTarget::Color,
                RowTarget::Sub(0),
                RowTarget::Sub(1),
            ]
        );

        menu.focus_row(4);
        assert_eq!(
            menu.on_ok(store.view()),
            TrackOk::Commit {
                commit: TrackCommit::Subtitle {
                    render_ordinal: -1,
                    stream_id: 42,
                    sidecar_key: Some("/library/streams/42.srt".into()),
                    sidecar_codec: "srt".into(),
                },
                keep_open: false,
            }
        );

        menu.focus_row(2);
        assert_eq!(
            menu.on_ok(store.view()),
            TrackOk::Commit { commit: TrackCommit::SubtitleTone(SubtitleTone::LADDER[1]), keep_open: true }
        );

        menu.focus_row(1);
        assert_eq!(
            menu.on_ok(store.view()),
            TrackOk::OpenTiming,
            "the sidecar is now the active subtitle, so Timing is no longer inert"
        );
    }

    // ---- track_menu: Color cycles and wraps and keeps the panel open --------------------------

    #[test]
    fn color_cycles_the_tone_ladder_with_wrap_and_keeps_the_panel_open() {
        let _g = crate::testlock::serial();
        crate::player::sidecar::reset();
        crate::player::restore_subtitle_tone(SubtitleTone::White);
        let ps = crate::route::PlaybackSession::IDLE;
        let store = crate::stores::metadata::MetadataStore::default();
        let mut menu = TrackMenuState::new(&ps, store.view(), 1, Vec::new());
        let color_row = menu
            .targets
            .iter()
            .position(|t| *t == RowTarget::Color)
            .expect("Color row");
        menu.focus_row(color_row as c_int);

        let ladder = SubtitleTone::LADDER;
        for want in ladder.iter().cycle().skip(1).take(ladder.len()) {
            assert_eq!(
                menu.on_ok(store.view()),
                TrackOk::Commit { commit: TrackCommit::SubtitleTone(*want), keep_open: true }
            );
            // the read-out is re-written in place on the focused row
            let row = menu.table.row_mut(color_row as c_int).expect("Color row");
            assert_eq!(row.value.as_deref(), Some(tone_label(*want)));
        }
    }

    // ---- track_menu: Timing returns OpenTiming, and is inert while Off ------------------------

    #[test]
    fn timing_returns_open_timing_once_a_subtitle_is_active_and_is_inert_while_off() {
        let _g = crate::testlock::serial();
        crate::player::sidecar::reset();
        crate::player::set_subtitle_offset(0);
        let ps = crate::route::PlaybackSession::IDLE;
        let store = store_with(vec![crate::metadata::Stream {
            id: 1,
            index: 0,
            lang: "English".into(),
            lang_code: "eng".into(),
            codec: "srt".into(),
            ..Default::default()
        }]);
        let mut menu = TrackMenuState::new(&ps, store.view(), 1, Vec::new());
        let timing_row = menu
            .targets
            .iter()
            .position(|t| *t == RowTarget::Timing)
            .expect("Timing row");

        menu.focus_row(timing_row as c_int);
        assert_eq!(menu.on_ok(store.view()), TrackOk::Inert, "subtitles are Off: inert");

        let sub_row = menu
            .targets
            .iter()
            .position(|t| matches!(t, RowTarget::Sub(_)))
            .expect("a track row");
        menu.focus_row(sub_row as c_int);
        menu.on_ok(store.view());

        menu.focus_row(timing_row as c_int);
        assert_eq!(menu.on_ok(store.view()), TrackOk::OpenTiming);
    }

    // ---- format_offset ---------------------------------------------------------------------------

    #[test]
    fn an_offset_reads_as_signed_seconds_to_the_tenth() {
        assert_eq!(format_offset(0), "0.0 s");
        assert_eq!(format_offset(100), "+0.1 s");
        assert_eq!(format_offset(-100), "-0.1 s");
        assert_eq!(format_offset(1_300), "+1.3 s");
        assert_eq!(format_offset(-30_000), "-30.0 s");
    }

    /// **Position is the join, so an unnamed track must occupy a slot rather than be skipped.**
    /// `TrackNames` is dense by contract; this pins the reader's half of it — the N-th entry, an
    /// out-of-range index and the `-1` that `sub_render_ordinal` answers for an external sidecar
    /// all resolve without panicking, and the sidecar gets no name rather than its neighbour's.
    #[test]
    fn a_track_index_resolves_by_position_and_an_absent_one_is_empty_not_a_neighbour() {
        let n = TrackNames {
            audio: vec!["Дубляж".into(), String::new(), "Original".into()],
            subs: vec!["Forced".into(), "Full".into()],
        };
        assert_eq!(n.audio(0), "Дубляж");
        assert_eq!(n.audio(1), "", "an untagged track holds its slot");
        assert_eq!(
            n.audio(2),
            "Original",
            "…so the one after it is still its own"
        );
        assert_eq!(n.sub(1), "Full");
        assert_eq!(
            n.sub(-1),
            "",
            "an external sidecar is not in the container at all"
        );
        assert_eq!(n.sub(9), "", "past the end is empty, not a panic");
        // the empty store — every read before a demuxer has opened, and every read on the host
        assert_eq!(TrackNames::new().sub(0), "");
    }

    // ---- track_menu: audio OK commits a frozen CarriedAudio (issue #266) ----------------------

    /// Picking a different audio row must commit the exact `CarriedAudio` snapshot
    /// `CarriedAudio::from_stream` builds from the row's own `metadata::Stream` — not a bare
    /// stream id, which is what the pre-refactor `TrackCommit::Audio(i32, String, i64, i64)`
    /// forced every caller to reassemble by hand.
    #[test]
    fn audio_commit_carries_carried_audio() {
        let ps = crate::route::PlaybackSession::IDLE;
        let store = store_with_audio(vec![
            crate::metadata::Stream {
                id: 10,
                index: 0,
                codec: "aac".into(),
                channels: 2,
                default: true,
                ..Default::default()
            },
            crate::metadata::Stream {
                id: 20,
                index: 1,
                codec: "eac3".into(),
                channels: 8,
                profile: "dolby digital plus + dolby atmos".into(),
                can_normalize_loudness: true,
                ..Default::default()
            },
        ]);
        let mut menu = TrackMenuState::new(&ps, store.view(), 0, Vec::new());
        assert_eq!(menu.active_audio(), 0, "the default track opens checked");

        menu.focus_row(1);
        let outcome = menu.on_ok(store.view());
        assert_eq!(
            outcome,
            TrackOk::Commit {
                commit: TrackCommit::Audio(crate::route::CarriedAudio {
                    sid: 20,
                    ordinal: 1,
                    codec: "eac3".into(),
                    channels: 8,
                    can_normalize_loudness: true,
                    immersive: true,
                }),
                keep_open: false,
            }
        );
    }

    /// Re-picking the already-active row is not a change: no commit, same as the pre-refactor
    /// behaviour this test protects against a regression in.
    #[test]
    fn audio_reselecting_the_active_row_dismisses_without_a_commit() {
        let ps = crate::route::PlaybackSession::IDLE;
        let store = store_with_audio(vec![crate::metadata::Stream {
            id: 10,
            index: 0,
            codec: "aac".into(),
            channels: 2,
            default: true,
            ..Default::default()
        }]);
        let mut menu = TrackMenuState::new(&ps, store.view(), 0, Vec::new());
        menu.focus_row(0);
        assert_eq!(menu.on_ok(store.view()), TrackOk::Dismiss);
    }

    /// [`AudioRowTarget`]'s row map, with no enhancement offered: one [`AudioRowTarget::Track`]
    /// per audio track, in the same order they were drawn, and nothing else — varying the track
    /// count to prove the map tracks the list rather than assuming a fixed length.
    #[test]
    fn audio_targets_map_one_row_per_track_with_no_enhancement() {
        for n in [0usize, 1, 3] {
            let ps = crate::route::PlaybackSession::IDLE;
            let audio = (0..n)
                .map(|i| crate::metadata::Stream {
                    id: 10 + i as i64,
                    index: i as i64,
                    codec: "aac".into(),
                    channels: 2,
                    default: i == 0,
                    ..Default::default()
                })
                .collect();
            let store = store_with_audio(audio);
            let menu = TrackMenuState::new(&ps, store.view(), 0, Vec::new());
            let want: Vec<AudioRowTarget> = (0..n).map(AudioRowTarget::Track).collect();
            assert_eq!(menu.audio_targets, want, "n={n}");
        }
    }
}

/// Issue #266 PR 4: the Audio tab's Boost dialog / Normalize loudness toggle rows. The offer/
/// refusal gating (I1-I7) is graded once, pure, over `route::plan::enhancements_offered` by PR
/// 2/3's own suites; these tests instead pin the MENU's own contract on top of that predicate:
/// the rows are ABSENT (never greyed — I1/I2) exactly when the live route does not offer them,
/// PRESENT with the right labels/toggle-state when it does, and a press flips the right bit and
/// keeps the panel open.
#[cfg(test)]
mod enhancement_menu_tests {
    use super::*;
    use super::tests::store_with_audio;
    use crate::route::{enhancement_test_session, reset_player_control_for_test, EnhTestFixture};

    /// One playing audio track — enough for `tracks(meta)` to be `Some` so `build_audio` does not
    /// take its "no playing item" early return. The enhancement offer itself is driven entirely by
    /// the `PlaybackSession` (`EnhTestFixture`), never by this store.
    fn one_track_store() -> crate::stores::metadata::MetadataStore {
        store_with_audio(vec![crate::metadata::Stream {
            id: 501,
            index: 0,
            codec: "ac3".into(),
            channels: 2,
            default: true,
            ..Default::default()
        }])
    }

    /// Build the Audio tab against `route`. Caller holds `testlock::serial()` — `EnhTestFixture`
    /// touches the process-global server registry and (when `in_flight`) `PLAYER_CONTROL`.
    fn audio_tab(route: EnhTestFixture) -> (TrackMenuState, crate::route::PlaybackSession) {
        let (ps, _sid) = enhancement_test_session(route);
        let store = one_track_store();
        let menu = TrackMenuState::new(&ps, store.view(), 0, Vec::new());
        (menu, ps)
    }

    fn teardown(ps: &crate::route::PlaybackSession) {
        reset_player_control_for_test(ps);
        crate::plex::reset_servers_for_test();
    }

    // ---- absent: I1-I7 ---------------------------------------------------------------------

    #[test]
    fn enh_rows_absent_no_pass() {
        let _g = crate::testlock::serial();
        let (menu, ps) =
            audio_tab(EnhTestFixture { pass: crate::plex::serverinfo::Subscription::No, ..Default::default() });
        assert_eq!(menu.enhance_shown, None);
        assert_eq!(menu.table.sections.len(), 1, "track list only — no second section at all");
        teardown(&ps);
    }

    #[test]
    fn enh_rows_absent_unknown_subscription() {
        let _g = crate::testlock::serial();
        let (menu, ps) = audio_tab(EnhTestFixture {
            pass: crate::plex::serverinfo::Subscription::Unknown,
            ..Default::default()
        });
        assert_eq!(menu.enhance_shown, None);
        teardown(&ps);
    }

    #[test]
    fn enh_rows_absent_incapable_track() {
        let _g = crate::testlock::serial();
        let (menu, ps) = audio_tab(EnhTestFixture { carried_capable: Some(false), ..Default::default() });
        assert_eq!(menu.enhance_shown, None);
        teardown(&ps);
    }

    #[test]
    fn enh_rows_absent_dv() {
        let _g = crate::testlock::serial();
        let (menu, ps) = audio_tab(EnhTestFixture { dv_declared: true, ..Default::default() });
        assert_eq!(menu.enhance_shown, None);
        teardown(&ps);
    }

    #[test]
    fn enh_rows_absent_subtitle_shown() {
        let _g = crate::testlock::serial();
        let (menu, ps) = audio_tab(EnhTestFixture { subtitle_shown: true, ..Default::default() });
        assert_eq!(menu.enhance_shown, None);
        teardown(&ps);
    }

    /// A restored sidecar reads the same live fact as an explicit subtitle pick — `cur_sub_sid !=
    /// 0`, per `route::facts`'s own doc — so this is the identical input as the test above, named
    /// for the other production path that sets it.
    #[test]
    fn enh_rows_absent_sidecar_shown() {
        let _g = crate::testlock::serial();
        let (menu, ps) = audio_tab(EnhTestFixture { subtitle_shown: true, ..Default::default() });
        assert_eq!(menu.enhance_shown, None);
        teardown(&ps);
    }

    /// I5 excludes every non-Direct/Remux shape identically (HLS, a fixed rung, a relay); one
    /// `Other`-family route stands for the group, since the predicate cannot tell them apart.
    #[test]
    fn enh_rows_absent_hls() {
        let _g = crate::testlock::serial();
        let (menu, ps) = audio_tab(EnhTestFixture { remux: Some(false), ..Default::default() });
        assert_eq!(menu.enhance_shown, None);
        teardown(&ps);
    }

    #[test]
    fn enh_rows_absent_reencode_rung() {
        let _g = crate::testlock::serial();
        let (menu, ps) = audio_tab(EnhTestFixture { remux: Some(false), ..Default::default() });
        assert_eq!(menu.enhance_shown, None);
        teardown(&ps);
    }

    #[test]
    fn enh_rows_absent_relay() {
        let _g = crate::testlock::serial();
        let (menu, ps) = audio_tab(EnhTestFixture { remux: Some(false), ..Default::default() });
        assert_eq!(menu.enhance_shown, None);
        teardown(&ps);
    }

    /// A forced direct play (or a fixed rung/relay/non-Original MDE) never computes an
    /// `auto_original` candidate at all — `base_present: false` reproduces exactly that.
    #[test]
    fn enh_rows_absent_forced() {
        let _g = crate::testlock::serial();
        let (menu, ps) = audio_tab(EnhTestFixture { base_present: false, ..Default::default() });
        assert_eq!(menu.enhance_shown, None);
        teardown(&ps);
    }

    #[test]
    fn enh_rows_absent_refused() {
        let _g = crate::testlock::serial();
        let (menu, ps) = audio_tab(EnhTestFixture { refused: true, ..Default::default() });
        assert_eq!(menu.enhance_shown, None);
        teardown(&ps);
    }

    #[test]
    fn enh_rows_absent_server_default_audio() {
        let _g = crate::testlock::serial();
        let (menu, ps) = audio_tab(EnhTestFixture { carried_capable: None, ..Default::default() });
        assert_eq!(menu.enhance_shown, None);
        teardown(&ps);
    }

    // ---- present -----------------------------------------------------------------------------

    #[test]
    fn enh_rows_present_pass_capable_direct() {
        let _g = crate::testlock::serial();
        let (menu, ps) = audio_tab(EnhTestFixture { remux: None, ..Default::default() });
        assert!(menu.enhance_shown.is_some());
        assert_eq!(menu.table.sections.len(), 2, "track list + the headerless enhancement section");
        let enh = &menu.table.sections[1];
        assert_eq!(enh.header, "");
        assert_eq!(enh.rows.len(), 2);
        assert_eq!(enh.rows[0].label, crate::i18n::msg::widgets_tracks_boost_dialog());
        assert_eq!(enh.rows[1].label, crate::i18n::msg::widgets_tracks_normalize_loudness());
        teardown(&ps);
    }

    /// `TrackMenuState::row_for_audio_target` is the `/tmp/plxnative-menupick` named-target
    /// resolver: `"boost"`/`"loudness"` map to the two toggle rows AFTER the one track, and any
    /// other name is `None` rather than a guess — the same "unknown name, no commit" contract
    /// `menupick_arm` logs on.
    #[test]
    fn row_for_audio_target_resolves_boost_and_loudness_when_shown() {
        let _g = crate::testlock::serial();
        let (menu, ps) = audio_tab(EnhTestFixture { remux: None, ..Default::default() });
        assert_eq!(menu.row_for_audio_target("boost"), Some(1), "row 0 is the one track");
        assert_eq!(menu.row_for_audio_target("loudness"), Some(2));
        assert_eq!(menu.row_for_audio_target("normalize_loudness"), None, "the old op-name spelling is not a row name");
        assert_eq!(menu.row_for_audio_target("bogus"), None);
        teardown(&ps);
    }

    /// Without an offer, the DSP rows are not built at all, so their names resolve to nothing —
    /// never to a stale row from a previous build.
    #[test]
    fn row_for_audio_target_none_without_enhancement_rows() {
        let _g = crate::testlock::serial();
        let (menu, ps) =
            audio_tab(EnhTestFixture { pass: crate::plex::serverinfo::Subscription::No, ..Default::default() });
        assert_eq!(menu.row_for_audio_target("boost"), None);
        assert_eq!(menu.row_for_audio_target("loudness"), None);
        teardown(&ps);
    }

    #[test]
    fn enh_rows_present_pass_capable_enhanced_remux() {
        let _g = crate::testlock::serial();
        let (menu, ps) = audio_tab(EnhTestFixture {
            remux: Some(true),
            applied: crate::plex::AudioEnhancements { boost_dialog: true, normalize_loudness: false },
            ..Default::default()
        });
        assert!(menu.enhance_shown.is_some());
        let enh = &menu.table.sections[1];
        assert_eq!(enh.rows[0].toggle, Some(true));
        assert_eq!(enh.rows[1].toggle, Some(false));
        teardown(&ps);
    }

    // ---- row indices / toggling ---------------------------------------------------------------

    #[test]
    fn enh_rows_follow_audio_rows_indices_stable() {
        let _g = crate::testlock::serial();
        let (ps, _sid) = enhancement_test_session(EnhTestFixture::default());
        let store = store_with_audio(vec![
            crate::metadata::Stream {
                id: 501,
                index: 0,
                codec: "ac3".into(),
                channels: 2,
                default: true,
                ..Default::default()
            },
            crate::metadata::Stream { id: 502, index: 1, codec: "aac".into(), channels: 2, ..Default::default() },
        ]);
        let menu = TrackMenuState::new(&ps, store.view(), 0, Vec::new());
        assert_eq!(menu.table.sections[0].rows.len(), 2, "both tracks in the track section");
        let enh = &menu.table.sections[1];
        assert_eq!(enh.rows.len(), 2, "the toggle rows sit in their own section, right after the tracks");
        assert_eq!(
            menu.audio_targets,
            vec![AudioRowTarget::Track(0), AudioRowTarget::Track(1), AudioRowTarget::Boost, AudioRowTarget::Loudness],
            "the row map names both tracks, then Boost, then Loudness, in drawn order"
        );
        teardown(&ps);
    }

    #[test]
    fn enh_ok_toggles_and_keeps_open() {
        let _g = crate::testlock::serial();
        let (mut menu, ps) = audio_tab(EnhTestFixture::default());
        let store = one_track_store();
        menu.focus_row(1); // row 0 = the one audio track; row 1 = Boost dialog
        let outcome = menu.on_ok(store.view());
        assert_eq!(
            outcome,
            TrackOk::Commit {
                commit: TrackCommit::AudioEnhancement(crate::plex::AudioEnhancements {
                    boost_dialog: true,
                    normalize_loudness: false,
                }),
                keep_open: true,
            }
        );
        assert_eq!(menu.table.sections[1].rows[0].toggle, Some(true));

        // a second press on the SAME row flips it back, and the panel is still open to take it
        let outcome = menu.on_ok(store.view());
        assert_eq!(
            outcome,
            TrackOk::Commit {
                commit: TrackCommit::AudioEnhancement(crate::plex::AudioEnhancements::NONE),
                keep_open: true,
            }
        );
        teardown(&ps);
    }

    #[test]
    fn enh_row_shows_desired_while_pending_applied_otherwise() {
        let _g = crate::testlock::serial();
        // Settled (no user edit queued): the row reads what the contract actually APPLIED.
        let (menu, ps) = audio_tab(EnhTestFixture {
            applied: crate::plex::AudioEnhancements { boost_dialog: false, normalize_loudness: true },
            ..Default::default()
        });
        assert_eq!(
            menu.enhance_shown,
            Some(crate::plex::AudioEnhancements { boost_dialog: false, normalize_loudness: true }),
        );
        teardown(&ps);
        drop(_g);

        // In flight (a user edit queued, not yet settled): the row reads the DESIRED preference.
        let _g = crate::testlock::serial();
        let desired = crate::plex::AudioEnhancements { boost_dialog: true, normalize_loudness: true };
        crate::player::set_audio_enhancements(desired);
        let (menu, ps) = audio_tab(EnhTestFixture { in_flight: true, ..Default::default() });
        assert_eq!(menu.enhance_shown, Some(desired));
        crate::player::set_audio_enhancements(crate::plex::AudioEnhancements::NONE);
        teardown(&ps);
    }

    #[test]
    fn enh_row_stops_reading_on_once_a_live_refusal_settles() {
        let _g = crate::testlock::serial();
        // Opens reading Normalize Loudness ON — the same shape `on_ok`'s own optimistic
        // `self.enhance_shown = Some(a)` leaves a freshly-picked row in, before the server has
        // answered.
        let (mut menu, ps_ok) = audio_tab(EnhTestFixture {
            applied: crate::plex::AudioEnhancements { boost_dialog: false, normalize_loudness: true },
            ..Default::default()
        });
        assert_eq!(menu.table.sections[1].rows[1].toggle, Some(true));

        // The SAME playback settles as Refused (I5 excludes it from the offer entirely) — a LIVE
        // change this menu never caused, delivered exactly the way `PlayerOverlayScreen`'s Tick
        // handler feeds it: a fresh `&PlaybackSession` from the host every frame, not a rebuild
        // the panel triggers itself.
        let (ps_refused, _sid2) = enhancement_test_session(EnhTestFixture { refused: true, ..Default::default() });
        let store = one_track_store();
        menu.update(0.0, &ps_refused, store.view());
        assert_eq!(menu.enhance_shown, None, "a settled refusal must drop the offer, not leave a row reading On");
        assert_eq!(menu.table.sections.len(), 1, "the headerless DSP section goes with it");

        teardown(&ps_ok);
    }

    // ---- locale + width gates ------------------------------------------------------------------

    /// **No row label ever leaks a "Plex Pass" mention**, in any shipped locale — the rows are
    /// ordinary audio settings; the gate that hid them from everyone else is never named in
    /// prose the viewer who HAS them ever reads.
    #[test]
    fn enh_locale_values_never_mention_plex_pass() {
        use crate::i18n::{language_on_this_thread_for_test, Preference};
        for language in [Preference::En, Preference::Es, Preference::Be] {
            let _guard = language_on_this_thread_for_test(language);
            for value in [
                crate::i18n::msg::widgets_tracks_boost_dialog(),
                crate::i18n::msg::widgets_tracks_normalize_loudness(),
            ] {
                let lower = value.to_lowercase();
                assert!(!lower.contains("plex pass"), "{language:?}: {value:?} names the gate");
            }
        }
    }

    /// **Every enhancement row fits the Audio panel in every shipped language**, same discipline
    /// as `every_subtitles_row_fits_the_panel_in_every_language` above over the Subtitles panel.
    ///
    /// `locales/be/widgets.json`'s `normalize_loudness` reads "Нармалізацыя гуку" ("normalization
    /// of sound") rather than the more literal "Нармалізацыя гучнасці" ("normalization of
    /// loudness") on purpose: this test measures the literal phrase at 378px against this panel's
    /// 369px column — 9px over — while "гуку" measures under. Re-check with this test before
    /// changing the Belarusian string back; do not assume either phrase's width from the source
    /// text alone.
    #[test]
    fn enh_rows_fit_width_560_es_be() {
        use crate::fontcov::advances::{ShippedMeasure, HEADROOM};
        use crate::i18n::{language_on_this_thread_for_test, Preference};
        let mut out = Vec::new();
        for language in [Preference::En, Preference::Es, Preference::Be] {
            let _g = crate::testlock::serial();
            let _guard = language_on_this_thread_for_test(language);
            let (menu, ps) = audio_tab(EnhTestFixture {
                applied: crate::plex::AudioEnhancements { boost_dialog: true, normalize_loudness: true },
                ..Default::default()
            });
            out.extend(
                menu.table
                    .elided_rows(AUDIO_PANEL_W, &ShippedMeasure, HEADROOM)
                    .into_iter()
                    .map(|e| format!("{}: {e}", language.tag())),
            );
            teardown(&ps);
        }
        assert!(out.is_empty(), "rows the panel would end in an ellipsis:\n  {}", out.join("\n  "));
    }
}

#[cfg(test)]
mod focus_tests {
    use super::*;
    use crate::screens::registry::{AppFx, AppMsg, PageMemory};
    use crate::ui::machine::{FocusRead, InputOwner, PressRead, Tick};

    struct HostFixture;
    impl Host for HostFixture {
        type Arg = crate::ui::fixture::FixtureArg;
        type Fx = AppFx;
        type Msg = AppMsg;
        type Elem = u32;
        type Views<'a> = ();
        type Init = crate::ui::fixture::FixtureInit;
        type Memory = PageMemory;
    }

    fn with_cx<R>(entry: EntryId, test: impl FnOnce(&Cx<'_, HostFixture>) -> R) -> R {
        let measure = crate::ui::fixture::FixtureMeasure;
        test(&Cx {
            views: (),
            tick: Tick::default(),
            measure: &measure,
            focus: FocusRead::default(),
            press: PressRead::default(),
            owner: InputOwner::Entry(entry),
        })
    }

    /// A three-row Audio tab, built without a `PlaybackSession` or a playing item — nothing here
    /// reads either.
    fn three_row_menu() -> TrackMenuState {
        let mut sec = Section::new("Audio");
        for label in ["English", "Русский", "Français"] {
            sec = sec.row(Row::new(label));
        }
        let mut table = TableView::new();
        table.set_sections(vec![sec], 0, false);
        TrackMenuState {
            tab: 0,
            active_audio: 0,
            active_sub: -1,
            targets: Vec::new(),
            audio_targets: Vec::new(),
            offset_ms: 0,
            tone: SubtitleTone::White,
            yours: Vec::new(),
            enhance_shown: None,
            table,
        }
    }

    /// **UP/DOWN step by one row and clamp at both ends**, matching
    /// [`TrackMenuState::move_focus`]'s own clamp.
    #[test]
    fn up_down_step_by_one_and_clamp_at_both_ends() {
        let e = EntryId(5);
        let st = three_row_menu();
        let part = TrackMenuPart { state: &st, entry: e, group: GroupId(0) };
        with_cx(e, |cx| {
            let step = |i: u32, dir: Dir| {
                match <TrackMenuPart as Focusable<HostFixture>>::neighbour(
                    &part,
                    FocusKey { entry: e, elem: i },
                    dir,
                    cx,
                ) {
                    Step::Move(k) => Some(k.elem),
                    Step::Edge => None,
                }
            };
            assert_eq!(step(0, Dir::Down), Some(1));
            assert_eq!(step(2, Dir::Down), None, "the last row does not wrap");
            assert_eq!(step(0, Dir::Up), None, "the first row does not wrap");
            assert_eq!(step(1, Dir::Up), Some(0));
        });
    }

    /// **LEFT/RIGHT never move within the group** — they are the screen's own tab switch
    /// (`TrackMenuState::focus_tab`), which is why `neighbour` always answers `Step::Edge` for
    /// them and [`groups`] hands both edges to [`EdgeRule::Screen`].
    #[test]
    fn left_right_are_edges_the_screen_interprets_as_a_tab_switch() {
        let e = EntryId(5);
        let st = three_row_menu();
        let part = TrackMenuPart { state: &st, entry: e, group: GroupId(0) };
        with_cx(e, |cx| {
            assert!(matches!(
                <TrackMenuPart as Focusable<HostFixture>>::neighbour(
                    &part,
                    FocusKey { entry: e, elem: 1 },
                    Dir::Left,
                    cx,
                ),
                Step::Edge
            ));
            let mut groups = Vec::new();
            <TrackMenuPart as Focusable<HostFixture>>::groups(&part, cx, &mut groups);
            let g = groups.into_iter().next().expect("one group");
            assert!(matches!(g.edge[2], EdgeRule::Screen));
            assert!(matches!(g.edge[3], EdgeRule::Screen));
        });
    }

    /// `place` reports exactly the row rect `TableView::row_frame` — and so the old `draw` —
    /// paints at.
    #[test]
    fn place_matches_the_tables_own_row_frame() {
        let e = EntryId(5);
        let st = three_row_menu();
        let r = st.panel_rect();
        let want = st.table.row_frame(r, 2);
        let part = TrackMenuPart { state: &st, entry: e, group: GroupId(0) };
        with_cx(e, |cx| {
            let placed = <TrackMenuPart as Focusable<HostFixture>>::place(&part, &2u32, cx, At::Drawn);
            assert_eq!(
                placed.map(|p| (p.rect.x, p.rect.y, p.rect.w, p.rect.h)),
                want.map(|r| (r.x, r.y, r.w, r.h))
            );
        });
    }
}

#[cfg(test)]
mod localized_offset_tests {
    #[test]
    fn subtitle_timing_uses_locale_decimal_and_unit_without_changing_offset_sign() {
        use crate::i18n::{LocaleContext, Preference};
        for (preference, region, negative, positive) in [
            (Preference::En, "en-US", "-0.1 s", "+1.3 s"),
            (Preference::Es, "es-ES", "-0,1 s", "+1,3 s"),
            (Preference::Be, "be-BY", "-0,1 с", "+1,3 с"),
        ] {
            let locale = LocaleContext::resolve(preference, None, Some(region), None, None);
            assert_eq!(crate::ui::timing_capsule::offset_seconds_in(-100, true, &locale), negative);
            assert_eq!(crate::ui::timing_capsule::offset_seconds_in(1300, true, &locale), positive);
        }
    }
}
