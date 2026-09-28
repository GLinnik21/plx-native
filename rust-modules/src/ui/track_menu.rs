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
//! section holds Timing and Color; and everything else falls under "Other languages". [`sub_layout`]
//! builds this pure — subs/names/yours in, sections plus a flat [`RowTarget`] vec out — so it is
//! host-tested without a `PlaybackSession`/`MetadataView` fixture. "Yours" is the pref language
//! (if the play resolved under one), the playing audio's language, and the current subtitle's own
//! language, in that order (`route::cur_sub_pref_lang`, carried in by `screens::player::overlay`).
//!
//! **Timing** is a single row that reads out the current offset; OK on it does not step anything
//! here — it returns [`TrackOk::OpenTiming`], which `screens::player::overlay` turns into a
//! hand-off to the Timing capsule overlay (`OverlayKind::Timing`, not yet wired — see the
//! `// lane B:` marker there). The row is dim and inert while subtitles are Off, and the whole
//! section is omitted during a transcode, which burns captions into the picture where no
//! client-side offset can reach.
//!
//! **Color** is a single cycling row: OK steps `SubtitleTone::LADDER` with wrap and keeps the
//! panel open ([`TrackMenuState::ok_keeps_open`]), so a run of presses is felt immediately.
#![allow(dead_code)]
use crate::metadata;
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

/// The menu's whole state, owned by the container that mounts this panel — the modal PHASE and the
/// appear spring belong to `ui::containers::modal::ModalStack` now, not to this struct; `draw` takes
/// the appear fraction as a parameter instead of stepping its own [`Popover`].
pub(crate) struct TrackMenuState {
    tab: c_int, // 0=Audio, 1=Subtitles
    active_audio: c_int, // index into the playing item's audio list
    active_sub: c_int, // -1 = Off, else index into the playing item's subs list
    /// The flat row → meaning map for the CURRENTLY BUILT tab's table — [`sub_layout`]'s second
    /// return value, kept alongside the `Section`s it built so [`Self::on_ok`], [`Self::sel_for_tab`]
    /// and [`Self::ok_keeps_open`] read back what a row IS by POSITION instead of re-deriving it
    /// (and instead of disagreeing with what was actually drawn, the way a fresh call to
    /// [`visible_subs`] could once a transcode starts). Empty on the Audio tab, which has no such
    /// indirection.
    targets: Vec<RowTarget>,
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
    /// exactly as [`sub_layout`]'s `yours` parameter reads them (compared with
    /// `metadata::lang_matches`, never by literal string equality). Owned rather than borrowed, so
    /// a rebuild (tab switch) needs nothing from the caller beyond `ps`/`meta`.
    yours: Vec<String>,
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
    Audio { ordinal: c_int, codec: String, stream_id: i64, channels: i64 },
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

/// **What OK on the focused row means**, one level up from [`TrackCommit`]: most rows commit a
/// pick outright, but the Timing row instead hands off to a different overlay
/// (`screens::player::overlay`'s Tracks→Timing transition, plan §4) — a decision this panel can
/// state but not perform, since it does not own the overlay stack.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum TrackOk {
    Commit(TrackCommit),
    /// Open the Timing capsule overlay. A no-op placeholder until lane B wires
    /// `OverlayKind::Timing` in `screens::player::overlay` — returning this from [`TrackMenuState::on_ok`]
    /// compiles and is exhaustively matched there, but nothing opens the capsule yet.
    OpenTiming,
}

/// One flat Subtitles-panel row, by POSITION — `sub_layout`'s second return value, one entry per
/// row in the exact order its `Section`s draw (`TableView::sel` is one flat index over all of
/// them). Replaces the old `tone_base`/`offset_base` split-by-arithmetic scheme: every reader
/// (`on_ok`, `sel_for_tab`, `ok_keeps_open`) matches on `targets[sel]` instead of re-deriving which
/// section a row fell in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RowTarget {
    Off,
    /// A track row — the index into the playing item's subs list ([`metadata::PlayingItem::subs`]).
    Sub(usize),
    Timing,
    Color,
}

impl TrackMenuState {
    /// Build the menu focused on `tab` (0=Audio, 1=Subtitles) — the on-screen audio/subs icons
    /// pick a specific tab this way; the plain open path passes 0. `yours` is "your languages" in
    /// preference order (pref, playing audio, current subtitle) — see [`sub_layout`]'s doc.
    pub(crate) fn new(
        ps: &crate::route::PlaybackSession,
        meta: metadata::MetadataView<'_>,
        tab: c_int,
        yours: &[&str],
    ) -> Self {
        let mut s = TrackMenuState {
            tab,
            active_audio: 0,
            active_sub: -1,
            targets: Vec::new(),
            offset_ms: crate::player::subtitle_offset_ms(),
            tone: crate::player::subtitle_tone(),
            yours: yours.iter().map(|y| y.to_string()).collect(),
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

    /// selectable rows in `tab` — the Audio tab is one row per track; the Subtitles tab is
    /// whatever [`sub_layout`] built, which `n_rows` re-derives when `tab` is not the one
    /// currently built (`self.targets` only ever holds the built tab's answer).
    fn n_rows(&self, ps: &crate::route::PlaybackSession, meta: metadata::MetadataView<'_>, tab: c_int) -> c_int {
        if tab == 0 {
            n_audio(meta)
        } else {
            let (_, targets) = self.layout(ps, meta);
            targets.len() as c_int
        }
    }
    /// the table row that should be focused when entering `tab` (its active selection) — on the
    /// Subtitles tab this is the row whose target is `Sub(active_sub)`, or `Off` when subtitles
    /// are off, found by position in the freshly built `targets` (plan §3).
    fn sel_for_tab(&self, ps: &crate::route::PlaybackSession, meta: metadata::MetadataView<'_>, tab: c_int) -> c_int {
        if tab == 0 {
            self.active_audio().max(0)
        } else {
            let (_, targets) = self.layout(ps, meta);
            sel_for_targets(&targets, self.active_sub)
        }
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
    /// the container's job now, not this method's.
    pub(crate) fn on_ok(&mut self, ps: &crate::route::PlaybackSession, meta: metadata::MetadataView<'_>) -> Option<TrackOk> {
        let tab = self.tab;
        let sel = self.table.sel;
        if tab == 0 {
            let changed = self.active_audio != sel;
            self.active_audio = sel;
            if changed {
                // the menu only reports the pick — native-switch vs re-transcode is route's policy.
                // The demuxer-facing index is the CONTAINER ordinal (audio_ordinal), not the row.
                if let Some(s) = tracks(meta).and_then(|t| t.audio.get(sel.max(0) as usize)) {
                    let ord = tracks(meta)
                        .map(|t| metadata::audio_ordinal(&t.audio, sel.max(0) as usize))
                        .unwrap_or(sel);
                    crate::diag::event(crate::diag::schema::DiagEvent::FeatureUsed {
                        feature: crate::diag::schema::Feature::AudioTrack,
                    });
                    return Some(TrackOk::Commit(TrackCommit::Audio {
                        ordinal: ord,
                        codec: s.codec.clone(),
                        stream_id: s.id,
                        channels: s.channels,
                    }));
                }
            }
            return None;
        }

        match self.targets.get(sel.max(0) as usize).copied() {
            Some(RowTarget::Color) => {
                // cycle with wrap: no track changes, so no `TrackCommit::Subtitle` — that one
                // always republishes, and re-committing the track would re-burn a transcode
                let n = SubtitleTone::LADDER.len() as u8;
                self.tone = SubtitleTone::from_index((self.tone.index() + 1) % n);
                // the panel stays up (`ok_keeps_open`): redraw the read-out in place, focus unmoved
                let (sections, targets) = self.layout(ps, meta);
                self.targets = targets;
                self.table.set_sections(sections, sel, true);
                Some(TrackOk::Commit(TrackCommit::SubtitleTone(self.tone)))
            }
            Some(RowTarget::Timing) if self.active_sub >= 0 => Some(TrackOk::OpenTiming),
            Some(RowTarget::Timing) => None, // dim and inert while subtitles are Off
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
                Some(TrackOk::Commit(TrackCommit::Subtitle {
                    render_ordinal: ridx,
                    stream_id: self.sub_stream_id(meta),
                    sidecar_key: sidecar.map(|s| s.key.clone()),
                    sidecar_codec: sidecar.map(|s| s.codec.clone()).unwrap_or_default(),
                }))
            }
        }
    }

    fn build_audio(&self, meta: metadata::MetadataView<'_>) -> Section {
        let mut sec = Section::new(crate::i18n::msg::widgets_tracks_audio());
        let d = match tracks(meta) {
            Some(t) => t,
            None => return sec,
        };
        let names = crate::player::SHARED.track_names.lock().unwrap();
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
        }
        sec
    }

    /// Build the Subtitles tab's sections and row map from the CURRENT state — the one call site
    /// every subtitle-tab reader (`rebuild`, `on_ok`, `n_rows`, `sel_for_tab`) goes through, so
    /// they can never disagree about what a row is.
    fn layout(&self, ps: &crate::route::PlaybackSession, meta: metadata::MetadataView<'_>) -> (Vec<Section>, Vec<RowTarget>) {
        let item = tracks(meta);
        let subs: &[metadata::Stream] = item.map(|t| t.subs.as_slice()).unwrap_or(&[]);
        let offered = visible_subs(ps, meta);
        let names = crate::player::SHARED.track_names.lock().unwrap();
        let yours: Vec<&str> = self.yours.iter().map(String::as_str).collect();
        sub_layout(
            subs,
            &offered,
            &names,
            &yours,
            self.active_sub,
            !crate::route::is_transcoding(ps),
            self.offset_ms,
            self.tone,
        )
    }

    fn rebuild(&mut self, ps: &crate::route::PlaybackSession, meta: metadata::MetadataView<'_>, tab: c_int, slide: bool) {
        if tab == 0 {
            self.targets = Vec::new();
            self.table.set_sections(vec![self.build_audio(meta)], self.active_audio().max(0), slide);
        } else {
            let (sections, targets) = self.layout(ps, meta);
            let sel = sel_for_targets(&targets, self.active_sub);
            self.targets = targets;
            self.table.set_sections(sections, sel, slide);
        }
    }

    /// Whether OK on the focused row leaves the panel OPEN: Color does, so a run of presses
    /// (cycling the ladder) is felt without a reopen-and-rewalk between them; so does the dim,
    /// inert Timing row while subtitles are Off, since OK there does nothing at all. A live Timing
    /// row does not — picking it hands off to a different overlay ([`TrackOk::OpenTiming`]).
    pub(crate) fn ok_keeps_open(&self) -> bool {
        self.tab == 1
            && match self.targets.get(self.table.sel.max(0) as usize) {
                Some(RowTarget::Color) => true,
                // the dim Timing row while subtitles are Off is INERT: OK does nothing, so it
                // must not close the panel either
                Some(RowTarget::Timing) => self.active_sub < 0,
                _ => false,
            }
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

    pub(crate) fn update(&mut self, dt: f32) {
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

/// An offset as localized signed seconds to the tenth; examples below use English formatting.
/// ASCII hyphen-minus rather than U+2212, which the UI font is not guaranteed to carry.
fn format_offset(ms: i64) -> String {
    format_offset_in(ms, crate::i18n::current())
}

fn format_offset_in(ms: i64, locale: &crate::i18n::LocaleContext) -> String {
    let sign = match ms.signum() {
        1 => "+",
        -1 => "-",
        _ => "",
    };
    let tenths = (ms.unsigned_abs() / 100) as i64;
    crate::i18n::msg::core_seconds_in(locale, &format!("{sign}{}", locale.decimal(tenths, 1)))
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

// ---- Subtitles-tab grouping (`sub_layout`, plan §3) -------------------------------------------

/// One badge a Subtitles-panel row may show — never more than one (`player.html:954`'s priority:
/// FORCED > SDH > EXTERNAL > an image codec). A thin local mirror of `ui::table::Badge` because
/// that type does not derive `Clone`/`Eq` and this one needs to be compared (for the
/// "identical tracks" ordinal grouping) before it is ever turned into a drawn `Badge`.
#[derive(Clone, Debug, PartialEq, Eq)]
enum RowBadge {
    Forced,
    Sdh,
    External,
    Codec(String),
}
impl RowBadge {
    fn badge(&self) -> Badge {
        match self {
            RowBadge::Forced => Badge::Forced,
            RowBadge::Sdh => Badge::Sdh,
            RowBadge::External => Badge::Text(crate::i18n::msg::widgets_tracks_external_badge().to_string()),
            RowBadge::Codec(c) => Badge::Text(c.clone()),
        }
    }
}

/// One subtitle track's LAYOUT input, computed once from `metadata::Stream` + the container's own
/// tag, before grouping — [`sub_layout`] groups a `Vec` of these rather than re-parsing per row.
#[derive(Clone)]
struct SubTrackInfo {
    /// index into the playing item's subs list — what a built row's `RowTarget::Sub` carries.
    i: usize,
    lang: String,
    /// lower-cased `Stream.lang_code`, `""` when unset (never matched by `lang_matches`, which
    /// refuses an empty tag on either side).
    code: String,
    /// `SubLabel.source`, or the region fallback when that is empty (`track_label::region_detail`).
    detail: String,
    kind: Kind,
    badge: Option<RowBadge>,
    /// `Some(n)` when this track is otherwise IDENTICAL (same lang, detail, badge) to at least one
    /// other offered track — an ordinal among the identical ones only (`player.html:959-962,1110`).
    ordinal: Option<u32>,
}

/// Parse every offered track into a [`SubTrackInfo`] and fill in [`SubTrackInfo::ordinal`] for
/// tracks that would otherwise draw the exact same row.
fn sub_tracks(subs: &[metadata::Stream], offered: &[usize], names: &crate::player::TrackNames) -> Vec<SubTrackInfo> {
    let mut out: Vec<SubTrackInfo> = offered
        .iter()
        .filter_map(|&i| {
            let s = subs.get(i)?;
            let lang = if s.lang.trim().is_empty() {
                crate::i18n::msg::widgets_tracks_unknown().to_string()
            } else {
                s.lang.clone()
            };
            let container = names.sub(metadata::sub_render_ordinal(subs, i));
            let merged = track_label::track_name(&s.title, container, &lang);
            let label = track_label::parse(&merged, &lang, s.forced, s.sdh);
            let mut detail = label.source;
            if detail.is_empty() {
                if let Some(region) = track_label::region_detail(&s.language_tag) {
                    detail = region;
                }
            }
            // the panel's one-badge priority: FORCED > SDH > EXTERNAL > an image codec
            let badge = if label.kind == Kind::Forced {
                Some(RowBadge::Forced)
            } else if label.kind == Kind::Sdh {
                Some(RowBadge::Sdh)
            } else if s.external {
                Some(RowBadge::External)
            } else if is_image_sub_codec(&s.codec) {
                Some(RowBadge::Codec(s.codec.to_uppercase()))
            } else {
                None
            };
            Some(SubTrackInfo {
                i,
                lang,
                code: s.lang_code.trim().to_ascii_lowercase(),
                detail,
                kind: label.kind,
                badge,
                ordinal: None,
            })
        })
        .collect();

    let key = |t: &SubTrackInfo| (t.lang.clone(), t.detail.clone(), badge_key(&t.badge));
    let mut totals: std::collections::HashMap<(String, String, String), u32> = std::collections::HashMap::new();
    for t in &out {
        *totals.entry(key(t)).or_insert(0) += 1;
    }
    let mut seen: std::collections::HashMap<(String, String, String), u32> = std::collections::HashMap::new();
    for t in &mut out {
        let k = key(t);
        if totals[&k] > 1 {
            let n = seen.entry(k).or_insert(0);
            *n += 1;
            t.ordinal = Some(*n);
        }
    }
    out
}

fn badge_key(b: &Option<RowBadge>) -> String {
    match b {
        None => String::new(),
        Some(RowBadge::Forced) => "forced".to_string(),
        Some(RowBadge::Sdh) => "sdh".to_string(),
        Some(RowBadge::External) => "external".to_string(),
        Some(RowBadge::Codec(c)) => format!("codec:{c}"),
    }
}

/// Are two tracks the same LANGUAGE? By `metadata::lang_matches` on their codes — so an ISO
/// 639-2/B "fre" and a 639-2/T "fra" (or a regional "fr-CA") land in ONE group rather than two
/// sections both headed "French". Two tracks without a code fall back to their display name, so
/// "Unknown" tracks still group with each other.
fn same_language(a: &SubTrackInfo, b: &SubTrackInfo) -> bool {
    if a.code.is_empty() || b.code.is_empty() {
        a.code.is_empty() && b.code.is_empty() && a.lang == b.lang
    } else {
        metadata::lang_matches(&a.code, &b.code)
    }
}

/// Is `code` one of `yours` (`metadata::lang_matches`, never literal equality — "fra"/"fre" and a
/// regional "fr-CA" all name French)? An empty code never matches — an "Unknown" track can never
/// silently land in "yours".
fn is_yours(code: &str, yours: &[&str]) -> bool {
    !code.is_empty() && yours.iter().any(|y| !y.trim().is_empty() && metadata::lang_matches(y, code))
}
/// `code`'s position in `yours` (the preference order a "yours" language group is itself
/// ordered by) — `usize::MAX` (sorts last) when nothing in `yours` matches, which `is_yours`
/// already ruled out for anything actually grouped as "mine".
fn yours_rank(code: &str, yours: &[&str]) -> usize {
    yours
        .iter()
        .position(|y| !y.trim().is_empty() && metadata::lang_matches(y, code))
        .unwrap_or(usize::MAX)
}

/// A flat row: label = language, detail = source/region (+ "Track N" when this track needed one),
/// one badge. Used for a single-track "yours" language, and for every "Other languages" row.
fn flat_row(t: &SubTrackInfo, active_sub: c_int) -> Row {
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
        row = row.badge(b.badge());
    }
    row
}

/// A row inside a multi-track "yours" language section (`player.html:1110-1115`): label = the
/// source (+ "Track N" if needed), or — for a NAMELESS track — the kind word itself ("Forced",
/// "SDH", "Full", "Commentary"), in which case a Forced/SDH badge that would only repeat the
/// label is dropped.
fn in_lang_row(t: &SubTrackInfo, active_sub: c_int) -> Row {
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
            row = row.badge(b.badge());
        }
    }
    row
}

/// **Group and order the Subtitles panel's rows** (plan §3, `player.html:1104-1162`) — pure over
/// plain values, so it is host-tested without a `PlaybackSession`/`MetadataView`/store fixture.
///
/// `subs` is the playing item's FULL subtitle list; `offered` is [`visible_subs`]'s answer (the
/// offered subset once sidecars-only-on-direct-play is applied); `names` is the demuxer's own tag
/// list, joined the same way [`TrackMenuState::build_audio`] joins the Audio tab's; `yours` is
/// "your languages" in PREFERENCE order; `active_sub` is the subs-list index of the checked track
/// (-1 for Off); `show_timing` is `!is_transcoding` (a transcode burns captions server-side, so no
/// client offset can reach them); `offset_ms`/`tone` are what the Timing/Color rows read out.
///
/// Returns the sections to draw AND a flat `targets` vec, one entry per row in the SAME order the
/// sections draw — every other reader (`on_ok`, `sel_for_tab`, `ok_keeps_open`) reads back what a
/// row IS from `targets[sel]` rather than re-deriving it.
pub(crate) fn sub_layout(
    subs: &[metadata::Stream],
    offered: &[usize],
    names: &crate::player::TrackNames,
    yours: &[&str],
    active_sub: c_int,
    show_timing: bool,
    offset_ms: i64,
    tone: SubtitleTone,
) -> (Vec<Section>, Vec<RowTarget>) {
    let all = sub_tracks(subs, offered, names);
    let (mut mine, mut other): (Vec<_>, Vec<_>) = all.into_iter().partition(|t| is_yours(&t.code, yours));
    mine.sort_by(|a, b| a.kind.rank().cmp(&b.kind.rank()).then(a.i.cmp(&b.i)));
    other.sort_by(|a, b| {
        a.lang
            .to_ascii_lowercase()
            .cmp(&b.lang.to_ascii_lowercase())
            .then(a.kind.rank().cmp(&b.kind.rank()))
            .then(a.i.cmp(&b.i))
    });

    // Bucket `mine` by language code, first-seen order, then reorder the buckets by each one's
    // position in `yours` — the pref's language leads, then the playing audio's, then the current
    // subtitle's, exactly as `yours` states them.
    let mut lang_buckets: Vec<(String, Vec<SubTrackInfo>)> = Vec::new();
    for t in mine.drain(..) {
        match lang_buckets.iter_mut().find(|(_, bucket)| same_language(&bucket[0], &t)) {
            Some((_, bucket)) => bucket.push(t),
            None => lang_buckets.push((t.code.clone(), vec![t])),
        }
    }
    lang_buckets.sort_by_key(|(code, _)| yours_rank(code, yours));

    // 1. "Subtitles": Off, then every single-track "yours" language, flat.
    let mut sections = vec![Section::new(crate::i18n::msg::widgets_tracks_subtitles())
        .row(Row::new(crate::i18n::msg::widgets_tracks_off()).checked(active_sub < 0))];
    let mut targets = vec![RowTarget::Off];

    // 2. Each multi-track "yours" language interrupts with its own section; a single-track
    // language folds back into "Subtitles" ONLY while that is still the last section built — once
    // a multi-track section has intervened, the next single-track language gets its own bare
    // (headerless) section instead, exactly mirroring `player.html`'s `sectionsFor`.
    for (_, bucket) in lang_buckets {
        if bucket.len() > 1 {
            let mut sec = Section::new(bucket[0].lang.clone())
                .accessory(crate::i18n::msg::widgets_tracks_count(bucket.len() as i64));
            for t in &bucket {
                sec = sec.row(in_lang_row(t, active_sub));
                targets.push(RowTarget::Sub(t.i));
            }
            sections.push(sec);
        } else {
            let t = &bucket[0];
            targets.push(RowTarget::Sub(t.i));
            let row = flat_row(t, active_sub);
            let reuse = sections
                .last()
                .is_some_and(|s| !s.header.is_empty() && s.accessory.is_empty());
            if reuse {
                sections.last_mut().unwrap().rows.push(row);
            } else {
                sections.push(Section::new("").row(row));
            }
        }
    }

    // 3. A headerless section: Timing (omitted under transcode, dim and inert while Off), then
    // Color — always a NEW section, never folded into whatever came before.
    let mut settings = Section::new("");
    if show_timing {
        settings = settings.row(
            Row::new(crate::i18n::msg::widgets_tracks_timing())
                .value(format_offset(offset_ms))
                .chevron(true)
                .dim(active_sub < 0),
        );
        targets.push(RowTarget::Timing);
    }
    settings = settings.row(Row::new(crate::i18n::msg::widgets_tracks_color()).value(tone_label(tone)));
    targets.push(RowTarget::Color);
    sections.push(settings);

    // 4. "Other languages": flat rows, sorted by language name then rank, with a language-count
    // accessory (distinct `lang_code`s, not track count).
    if !other.is_empty() {
        // distinct LANGUAGES, by the same matcher the buckets use — "fre" and "fra" are one
        let mut langs: Vec<&SubTrackInfo> = Vec::new();
        for t in &other {
            if !langs.iter().any(|l| same_language(l, t)) {
                langs.push(t);
            }
        }
        let mut sec = Section::new(crate::i18n::msg::widgets_tracks_other_languages())
            .accessory(crate::i18n::msg::widgets_tracks_language_count(langs.len() as i64));
        for t in &other {
            sec = sec.row(flat_row(t, active_sub));
            targets.push(RowTarget::Sub(t.i));
        }
        sections.push(sec);
    }

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

    // ---- sub_layout: section order -----------------------------------------------------------

    #[test]
    fn section_order_is_subtitles_then_multitrack_languages_then_settings_then_other() {
        let subs = vec![
            stream(1, 0, "English", "eng", ""),
            stream(2, 1, "Russian", "rus", "iTunes"),
            stream(3, 2, "Russian", "rus", "Netflix"), // multi-track "yours" → own section
            stream(4, 3, "French", "fre", ""),         // not "yours" → "Other languages"
        ];
        let offered: Vec<usize> = (0..subs.len()).collect();
        let names = TrackNames::new();

        // yours = [eng, rus]: English (single-track) is bucketed FIRST, so it folds into
        // "Subtitles" (still the untouched initial section) before Russian's own section
        // interrupts.
        let (sections, _targets) =
            sub_layout(&subs, &offered, &names, &["eng", "rus"], -1, true, 0, SubtitleTone::White);
        let headers: Vec<&str> = sections.iter().map(|s| s.header.as_str()).collect();
        assert_eq!(headers, ["Subtitles", "Russian", "", "Other languages"]);
        assert_eq!(sections[0].rows[1].label, "English", "folded into Subtitles");
        assert_eq!(sections[1].accessory, "2 tracks");
        assert_eq!(sections[3].accessory, "1 language");

        // yours = [rus, eng]: the multi-track Russian bucket now outranks the single-track
        // English one, so Russian's section comes FIRST — once it has interrupted, English no
        // longer folds back into "Subtitles" and gets its own bare section instead
        // (`player.html`'s own `sectionsFor` rule: only the section BEFORE the first interruption
        // stays "Subtitles").
        let (sections, _targets) =
            sub_layout(&subs, &offered, &names, &["rus", "eng"], -1, true, 0, SubtitleTone::White);
        let headers: Vec<&str> = sections.iter().map(|s| s.header.as_str()).collect();
        assert_eq!(headers, ["Subtitles", "Russian", "", "", "Other languages"]);
        assert_eq!(sections[2].rows[0].label, "English", "its own bare section, not Subtitles");
        assert_eq!(sections[3].rows.len(), 2, "the headerless Timing + Color section");
    }

    // ---- sub_layout: "yours" order ------------------------------------------------------------

    #[test]
    fn yours_flat_rows_follow_the_preference_order_pref_then_audio_then_current() {
        let subs = vec![
            stream(1, 0, "French", "fre", ""),
            stream(2, 1, "English", "eng", ""),
            stream(3, 2, "Russian", "rus", ""),
        ];
        let offered: Vec<usize> = (0..subs.len()).collect();
        let names = TrackNames::new();
        // pref=rus, audio=eng, current=fre — every one a single track, so all three stay flat
        // under "Subtitles" and must read in THIS order, not file order.
        let (sections, _targets) =
            sub_layout(&subs, &offered, &names, &["rus", "eng", "fre"], -1, true, 0, SubtitleTone::White);
        let labels: Vec<&str> = sections[0].rows.iter().map(|r| r.label.as_str()).collect();
        assert_eq!(labels, ["Off", "Russian", "English", "French"]);
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

    // ---- sub_layout: a multi-track section, "N tracks" and rank order -------------------------

    #[test]
    fn a_multitrack_yours_language_ranks_full_then_sdh_then_forced_then_commentary() {
        // file order deliberately scrambles the rank order: commentary, forced, full, sdh
        let subs = vec![
            stream(1, 0, "Russian", "rus", "Commentary"),
            stream(2, 1, "Russian", "rus", "Форс."),
            stream(3, 2, "Russian", "rus", ""),
            stream(4, 3, "Russian", "rus", "SDH"),
        ];
        let offered: Vec<usize> = (0..subs.len()).collect();
        let names = TrackNames::new();
        let (sections, _targets) =
            sub_layout(&subs, &offered, &names, &["rus"], -1, true, 0, SubtitleTone::White);
        assert_eq!(sections[1].header, "Russian");
        assert_eq!(sections[1].accessory, "4 tracks");
        let labels: Vec<&str> = sections[1].rows.iter().map(|r| r.label.as_str()).collect();
        assert_eq!(labels, ["Full", "SDH", "Forced", "Commentary"]);
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

    /// **One language, one group, whatever ISO 639-2 spelling each track carries** — "fre" (the
    /// B code) and "fra" (the T code) are both French, so they bucket together under one "French"
    /// section and count as one language under "Other languages".
    #[test]
    fn bibliographic_and_terminology_codes_of_one_language_group_together() {
        let subs = vec![
            stream(1, 0, "French", "fre", "iTunes"),
            stream(2, 1, "French", "fra", "Netflix"),
        ];
        let offered: Vec<usize> = (0..subs.len()).collect();
        let names = TrackNames::new();
        let (sections, _) = sub_layout(&subs, &offered, &names, &["fra"], -1, true, 0, SubtitleTone::White);
        let headers: Vec<&str> = sections.iter().map(|s| s.header.as_str()).collect();
        assert_eq!(headers, ["Subtitles", "French", ""], "one French section, not two flat rows");
        assert_eq!(sections[1].accessory, "2 tracks");

        let (sections, _) = sub_layout(&subs, &offered, &names, &[], -1, true, 0, SubtitleTone::White);
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
        let mut store = crate::stores::metadata::MetadataStore::default();
        assert!(store.run(crate::stores::metadata::MetadataCmd::InstallPlaying(Some(
            crate::metadata::PlayingItem {
                sid: crate::plex::ServerId::from_raw(0),
                rk: "rk".into(),
                show_rk: String::new(),
                audio: Vec::new(),
                subs: vec![crate::metadata::Stream {
                    id: 1,
                    index: 0,
                    lang: "English".into(),
                    lang_code: "eng".into(),
                    codec: "srt".into(),
                    ..Default::default()
                }],
                video_fps: 0.0,
                width: 0,
                height: 0,
                bitrate: 0,
                dovi: Default::default(),
                markers: Vec::new(),
                chapters: Vec::new(),
                blur: None,
            },
        ))));
        let menu = TrackMenuState::new(&ps, store.view(), 1, &[]);
        assert_eq!(
            menu.targets,
            vec![RowTarget::Off, RowTarget::Timing, RowTarget::Color, RowTarget::Sub(0)]
        );
    }

    // ---- track_menu: sel_for_tab lands on the checked sub inside a group ----------------------

    #[test]
    fn sel_for_tab_lands_on_the_checked_sub_inside_a_multitrack_group() {
        let _g = crate::testlock::serial();
        crate::player::sidecar::reset();
        let ps = crate::route::PlaybackSession::IDLE;
        let mut store = crate::stores::metadata::MetadataStore::default();
        assert!(store.run(crate::stores::metadata::MetadataCmd::InstallPlaying(Some(
            crate::metadata::PlayingItem {
                sid: crate::plex::ServerId::from_raw(0),
                rk: "rk".into(),
                show_rk: String::new(),
                audio: Vec::new(),
                subs: vec![
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
                ],
                video_fps: 0.0,
                width: 0,
                height: 0,
                bitrate: 0,
                dovi: Default::default(),
                markers: Vec::new(),
                chapters: Vec::new(),
                blur: None,
            },
        ))));
        let mut menu = TrackMenuState::new(&ps, store.view(), 1, &["rus"]);
        menu.active_sub = 1; // the second track is the checked one
        let sel = menu.sel_for_tab(&ps, store.view(), 1);
        assert_eq!(menu.targets.get(sel as usize).copied(), Some(RowTarget::Sub(1)));
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
        let mut store = crate::stores::metadata::MetadataStore::default();
        assert!(store.run(crate::stores::metadata::MetadataCmd::InstallPlaying(Some(
            crate::metadata::PlayingItem {
                sid: crate::plex::ServerId::from_raw(0),
                rk: "rk".into(),
                show_rk: String::new(),
                audio: Vec::new(),
                subs: vec![
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
                ],
                video_fps: 0.0,
                width: 0,
                height: 0,
                bitrate: 0,
                dovi: Default::default(),
                markers: Vec::new(),
                chapters: Vec::new(),
                blur: None,
            },
        ))));
        let mut menu = TrackMenuState::new(&ps, store.view(), 1, &[]);
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
            menu.on_ok(&ps, store.view()),
            Some(TrackOk::Commit(TrackCommit::Subtitle {
                render_ordinal: -1,
                stream_id: 42,
                sidecar_key: Some("/library/streams/42.srt".into()),
                sidecar_codec: "srt".into(),
            }))
        );

        menu.focus_row(2);
        assert_eq!(
            menu.on_ok(&ps, store.view()),
            Some(TrackOk::Commit(TrackCommit::SubtitleTone(SubtitleTone::LADDER[1])))
        );
        assert!(menu.ok_keeps_open());

        menu.focus_row(1);
        assert_eq!(
            menu.on_ok(&ps, store.view()),
            Some(TrackOk::OpenTiming),
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
        let mut menu = TrackMenuState::new(&ps, store.view(), 1, &[]);
        let color_row = menu
            .targets
            .iter()
            .position(|t| *t == RowTarget::Color)
            .expect("Color row");
        menu.focus_row(color_row as c_int);

        let ladder = SubtitleTone::LADDER;
        for want in ladder.iter().cycle().skip(1).take(ladder.len()) {
            assert_eq!(
                menu.on_ok(&ps, store.view()),
                Some(TrackOk::Commit(TrackCommit::SubtitleTone(*want)))
            );
            assert!(menu.ok_keeps_open());
        }
    }

    // ---- track_menu: Timing returns OpenTiming, and is inert while Off ------------------------

    #[test]
    fn timing_returns_open_timing_once_a_subtitle_is_active_and_is_inert_while_off() {
        let _g = crate::testlock::serial();
        crate::player::sidecar::reset();
        crate::player::set_subtitle_offset(0);
        let ps = crate::route::PlaybackSession::IDLE;
        let mut store = crate::stores::metadata::MetadataStore::default();
        assert!(store.run(crate::stores::metadata::MetadataCmd::InstallPlaying(Some(
            crate::metadata::PlayingItem {
                sid: crate::plex::ServerId::from_raw(0),
                rk: "rk".into(),
                show_rk: String::new(),
                audio: Vec::new(),
                subs: vec![crate::metadata::Stream {
                    id: 1,
                    index: 0,
                    lang: "English".into(),
                    lang_code: "eng".into(),
                    codec: "srt".into(),
                    ..Default::default()
                }],
                video_fps: 0.0,
                width: 0,
                height: 0,
                bitrate: 0,
                dovi: Default::default(),
                markers: Vec::new(),
                chapters: Vec::new(),
                blur: None,
            },
        ))));
        let mut menu = TrackMenuState::new(&ps, store.view(), 1, &[]);
        let timing_row = menu
            .targets
            .iter()
            .position(|t| *t == RowTarget::Timing)
            .expect("Timing row");

        menu.focus_row(timing_row as c_int);
        assert_eq!(menu.on_ok(&ps, store.view()), None, "subtitles are Off: inert");

        let sub_row = menu
            .targets
            .iter()
            .position(|t| matches!(t, RowTarget::Sub(_)))
            .expect("a track row");
        menu.focus_row(sub_row as c_int);
        menu.on_ok(&ps, store.view());

        menu.focus_row(timing_row as c_int);
        assert_eq!(menu.on_ok(&ps, store.view()), Some(TrackOk::OpenTiming));
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
            offset_ms: 0,
            tone: SubtitleTone::White,
            yours: Vec::new(),
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
            assert_eq!(super::format_offset_in(-100, &locale), negative);
            assert_eq!(super::format_offset_in(1300, &locale), positive);
        }
    }
}
