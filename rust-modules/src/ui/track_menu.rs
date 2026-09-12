//! In-player modal track menu: audio + subtitle pickers over the video, rendered on the reusable
//! animated `TableView` (Apple-TV "settings" look — a sliding pill selection, section header with
//! a codec accessory, per-row badges, a leading checkmark on the active track). app.rs routes
//! D-pad/OK/BACK here while the menu is open; LEFT/RIGHT switch between the Audio and Subtitles
//! panels. The selection commit (native audio switch / server transcode / burn) is unchanged
//! from the previous procedural version — only the presentation moved onto the table.
#![allow(dead_code)]
use crate::metadata;
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

/// The menu's whole state, owned by the container that mounts this panel — the modal PHASE and the
/// appear spring belong to `ui::containers::modal::ModalStack` now, not to this struct; `draw` takes
/// the appear fraction as a parameter instead of stepping its own [`Popover`].
pub(crate) struct TrackMenuState {
    tab: c_int, // 0=Audio, 1=Subtitles
    active_audio: c_int, // index into the playing item's audio list
    active_sub: c_int, // -1 = Off, else index into the playing item's subs list
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
    Audio { ordinal: c_int, codec: String, stream_id: i64 },
    Subtitle { render_ordinal: c_int, stream_id: i64 },
}

impl TrackMenuState {
    /// Build the menu focused on `tab` (0=Audio, 1=Subtitles) — the on-screen audio/subs icons
    /// pick a specific tab this way; the plain open path passes 0.
    pub(crate) fn new(ps: &crate::route::PlaybackSession, tab: c_int) -> Self {
        let mut s = TrackMenuState {
            tab,
            active_audio: 0,
            active_sub: -1,
            table: TableView::new(),
        };
        s.sync_item(ps);
        s.rebuild(ps, tab, false);
        s
    }

    /// The highlighted row, for the focus probe (`crate::focusprobe`) — a READ of the cursor the
    /// key ladder moves, and the reason it exists: `app.rs`'s UP/DOWN arm for this panel changes
    /// nothing else, so without this the fingerprint records the panel opening and closing and
    /// nothing between.
    pub(crate) fn sel(&self) -> i32 {
        self.table.sel
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
    pub(crate) fn audio_stream_id(&self) -> i64 {
        let i = self.active_audio();
        tracks()
            .and_then(|t| t.audio.get(i.max(0) as usize))
            .map(|s| s.id)
            .unwrap_or(0)
    }
    /// Plex stream id of the chosen subtitle track (for &subtitleStreamID), or 0 if Off
    pub(crate) fn sub_stream_id(&self) -> i64 {
        let i = self.active_sub();
        if i < 0 {
            return 0;
        }
        tracks()
            .and_then(|t| t.subs.get(i as usize))
            .map(|s| s.id)
            .unwrap_or(0)
    }

    /// selectable rows in a tab — Subtitles has a leading "Off" row
    fn n_rows(&self, ps: &crate::route::PlaybackSession, tab: c_int) -> c_int {
        if tab == 0 {
            n_audio()
        } else {
            visible_subs(ps).len() as c_int + 1
        }
    }
    /// the table row that should be focused when entering `tab` (its active selection)
    fn sel_for_tab(&self, ps: &crate::route::PlaybackSession, tab: c_int) -> c_int {
        if tab == 0 {
            self.active_audio().max(0)
        } else {
            let a = self.active_sub();
            // the row of the active subs-list index within the VISIBLE rows (+1 for Off)
            visible_subs(ps)
                .iter()
                .position(|&i| a >= 0 && i == a as usize)
                .map(|p| p as c_int + 1)
                .unwrap_or(0)
        }
    }

    /// Derive the checked tracks from the PLAYBACK state on every open — the route owns the truth
    /// (CUR_AUDIO_SID/CUR_SUB_SID, set by the start-of-play pick and every commit), so the menu can
    /// never show a stale or desynced checkmark: the auto-picked English/smart-DP track is checked
    /// on first open, a replayed item resets with the playback, and a prior pick round-trips by id.
    /// When no id is recorded (codec-default play), the file's flagged default is checked.
    /// Deliberately does NOT touch `tab`: [`TrackMenuState::new`] sets it directly.
    fn sync_item(&mut self, ps: &crate::route::PlaybackSession) {
        let (audio, sub) = match tracks() {
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
    pub(crate) fn focus_tab(&mut self, ps: &crate::route::PlaybackSession, tab: c_int) {
        if tab != self.tab {
            self.tab = tab;
            self.rebuild(ps, tab, false); // swap the whole list → snap the pill, no long glide
        }
    }

    /// commit the focused row as the active track for its tab — dismissing the panel afterward is
    /// the container's job now, not this method's.
    pub(crate) fn on_ok(&mut self, ps: &crate::route::PlaybackSession) -> Option<TrackCommit> {
        let tab = self.tab;
        let sel = self.table.sel;
        if tab == 0 {
            let changed = self.active_audio != sel;
            self.active_audio = sel;
            if changed {
                // the menu only reports the pick — native-switch vs re-transcode is route's policy.
                // The demuxer-facing index is the CONTAINER ordinal (audio_ordinal), not the row.
                if let Some(s) = tracks().and_then(|t| t.audio.get(sel.max(0) as usize)) {
                    let ord = tracks()
                        .map(|t| metadata::audio_ordinal(&t.audio, sel.max(0) as usize))
                        .unwrap_or(sel);
                    crate::diag::event(crate::diag::schema::DiagEvent::FeatureUsed {
                        feature: crate::diag::schema::Feature::AudioTrack,
                    });
                    return Some(TrackCommit::Audio {
                        ordinal: ord,
                        codec: s.codec.clone(),
                        stream_id: s.id,
                    });
                }
            }
            None
        } else {
            // row 0 = Off = -1; else map the visible row back to its subs-list index
            let vis = visible_subs(ps);
            let new_sub: c_int = if sel <= 0 {
                -1
            } else {
                vis.get((sel - 1) as usize)
                    .map(|&i| i as c_int)
                    .unwrap_or(-1)
            };
            let changed = self.active_sub != new_sub;
            self.active_sub = new_sub;
            // the client renderer takes the EMBEDDED-subtitle ordinal (what the demuxer
            // enumerates); an external pick (transcode-only row) renders nothing — it's burned
            let ridx = tracks()
                .filter(|_| new_sub >= 0)
                .map(|t| metadata::sub_render_ordinal(&t.subs, new_sub as usize))
                .unwrap_or(-1);
            if changed {
                crate::diag::event(crate::diag::schema::DiagEvent::FeatureUsed {
                    feature: crate::diag::schema::Feature::SubtitleTrack,
                });
            }
            Some(TrackCommit::Subtitle {
                render_ordinal: ridx,
                stream_id: self.sub_stream_id(),
            })
        }
    }

    fn build_audio(&self) -> Section {
        let mut sec = Section::new("Audio");
        let d = match tracks() {
            Some(t) => t,
            None => return sec,
        };
        let names = crate::player::SHARED.track_names.lock().unwrap();
        for (i, s) in d.audio.iter().enumerate() {
            let lang = if s.lang.is_empty() {
                "Unknown"
            } else {
                s.lang.as_str()
            };
            let label = if s.default {
                format!("Original: {lang}")
            } else {
                lang.to_string()
            };
            let mut row = Row::new(label).checked(i as c_int == self.active_audio());
            // a per-track descriptor so sibling tracks in the same language are distinguishable
            // (e.g. two Russian tracks: "Дубляж" vs "AC-3 5.1"). Prefer the stream title, else the
            // codec + channel layout.
            let name = track_name(
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

    fn build_subs(&self, ps: &crate::route::PlaybackSession) -> Section {
        let mut sec = Section::new("Subtitles");
        sec = sec.row(Row::new("Off").checked(self.active_sub() < 0));
        if let Some(t) = tracks() {
            let names = crate::player::SHARED.track_names.lock().unwrap();
            for i in visible_subs(ps) {
                let s = match t.subs.get(i) {
                    Some(s) => s,
                    None => continue,
                };
                let lang = if s.lang.is_empty() {
                    "Unknown"
                } else {
                    s.lang.as_str()
                };
                let mut row = Row::new(lang.to_string()).checked(i as c_int == self.active_sub());
                let name = track_name(
                    &s.title,
                    names.sub(crate::metadata::sub_render_ordinal(&t.subs, i)),
                    lang,
                );
                if !name.is_empty() {
                    row = row.detail(name);
                }
                if s.forced {
                    row = row.badge(Badge::Forced);
                }
                if s.sdh {
                    row = row.badge(Badge::Sdh);
                }
                if is_image_sub_codec(&s.codec) {
                    row = row.badge(Badge::Text(s.codec.to_uppercase()));
                }
                sec = sec.row(row);
            }
        }
        sec
    }

    fn rebuild(&mut self, ps: &crate::route::PlaybackSession, tab: c_int, slide: bool) {
        let sec = if tab == 0 {
            self.build_audio()
        } else {
            self.build_subs(ps)
        };
        let sel = self.sel_for_tab(ps, tab);
        self.table.set_sections(vec![sec], sel, slide);
    }

    /// The panel geometry — shared by `update` and `draw` so scrolling math matches.
    fn panel_rect(&self) -> Rect {
        let tab = self.tab;
        let pw = if tab == 0 { 560.0f32 } else { 448.0f32 }; // audio / subtitles (mockup panel widths)
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
        // modal scrim (dims the video plane showing through) + the appear fade/rise — the container
        // now drives the phase and the appear spring; this reproduces exactly what
        // `Popover::scrim(0.58)` and `Popover::content_painter(20.0)` used to draw.
        let dim = theme::scrim_black(0.58 * appear);
        Painter::root().rect(Rect::FULL, 0.0, dim, dim, 0.0);
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
fn tracks() -> Option<&'static metadata::PlayingItem> {
    metadata::playing()
}

fn n_audio() -> c_int {
    tracks().map(|t| t.audio.len()).unwrap_or(0) as c_int
}
/// Subtitle rows currently offered, as indices into the playing subs list. External/sidecar
/// subs are NOT in the container, so the client renderer can't show them on direct-play —
/// they're listed only while transcoding (the server can burn them).
fn visible_subs(ps: &crate::route::PlaybackSession) -> Vec<usize> {
    tracks()
        .map(|t| {
            t.subs
                .iter()
                .enumerate()
                .filter(|(_, s)| !s.external || crate::route::is_transcoding(ps))
                .map(|(i, _)| i)
                .collect()
        })
        .unwrap_or_default()
}

// ---- section building ----
use crate::metadata::friendly_codec; // the ONE codec→display-name map (shared with the Info card)

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
fn track_name(pms: &str, container: &str, lang: &str) -> String {
    for cand in [pms.trim(), container.trim()] {
        if !cand.is_empty() && !cand.eq_ignore_ascii_case(lang) {
            return cand.to_string();
        }
    }
    String::new()
}

/// "AC-3 5.1", "Dolby TrueHD 7.1", "DTS 5.1" — a compact codec + channel-layout descriptor.
fn audio_descriptor(s: &metadata::Stream) -> String {
    let codec = friendly_codec(&s.codec);
    let ch = if !s.layout.is_empty() {
        channel_short(&s.layout)
    } else if s.channels > 0 {
        match s.channels {
            1 => "Mono".to_string(),
            2 => "Stereo".to_string(),
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
        "mono" => "Mono".to_string(),
        "stereo" => "Stereo".to_string(),
        other => other.to_string(),
    }
}

/// The panel at its WIDEST and TALLEST, for the overscan audit ([`crate::ui::consts::SAFE`]) — the
/// audio tab's 560 and the full `top_min`→`bottom` span, since the measured height comes from a
/// `TableView` no host test can measure.
#[cfg(test)]
pub(crate) fn overscan_rects(out: &mut Vec<(&'static str, Rect)>) {
    let pw = 560.0f32;
    let (bottom, top_min) = (SCR_H - 316.0, 60.0);
    out.push((
        "track menu panel",
        Rect::new(
            crate::ui::player_hud::CTRL_RIGHT - pw,
            top_min,
            pw,
            bottom - top_min,
        ),
    ));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::player::TrackNames;

    /// **The case this exists for, in the server's own words.** Verified live 2026-08-22 against
    /// one PMS holding both containers: for the MP4 part of *Wicked* the server sends nine subtitle
    /// streams whose every semantic field is identical — same `codec`, `bitrate` 0, no `title`, no
    /// forced or SDH flag — so six of them arrive as the bare word `Русский` and the picker cannot
    /// tell a forced signs track from a full translation. The file names all nine.
    ///
    /// Graded as the property that matters rather than as six string comparisons: **no two rows of
    /// one language read the same.** A regression that dropped the container name, or preferred the
    /// language over it, collapses this set back to one distinct value and the assertion says so.
    #[test]
    fn an_mp4s_container_names_tell_apart_the_tracks_pms_reports_identically() {
        // exactly what the wire carries for every one of them: a language and nothing else
        let pms_title = "";
        let lang = "Русский";
        // …and exactly what the container carries, in file order
        let container = [
            "Форс. iTunes",
            "Форс. Jaskier песни",
            "Форс. Red Head Sound песни",
            "Полные iTunes",
            "Полные Jaskier",
            "Полные stirloo",
        ];
        let rows: Vec<String> = container
            .iter()
            .map(|c| track_name(pms_title, c, lang))
            .collect();
        assert_eq!(rows, container, "each row shows its own track's name");
        let distinct: std::collections::HashSet<&String> = rows.iter().collect();
        assert_eq!(
            distinct.len(),
            rows.len(),
            "no two rows of one language may read the same"
        );
    }

    /// The MKV control case, from the same server: PMS DOES parse Matroska's `title`, so the
    /// server's answer is used and the demuxer is not consulted — which is what keeps this working
    /// before playback has opened a file, and through a transcode, where there is no file to read.
    #[test]
    fn the_servers_own_title_wins_when_it_has_one() {
        assert_eq!(
            track_name("HDRezka Studio", "", "Русский"),
            "HDRezka Studio"
        );
        // …and it still wins when the demuxer also has one: the same tag, one source of truth
        assert_eq!(track_name("Forced", "Forced", "Русский"), "Forced");
    }

    /// A name that only repeats the row's own label is not a name — the row already says `English`
    /// in the label above, and a sub-line saying it again spends the row's second line to do it.
    /// Both sources are filtered, and the fallback continues past a rejected one rather than
    /// stopping: a server echoing the language must not mask a container that says something.
    #[test]
    fn a_name_that_only_repeats_the_language_is_not_shown() {
        assert_eq!(track_name("English", "", "English"), "");
        assert_eq!(
            track_name("english", "", "English"),
            "",
            "the guard is case-insensitive"
        );
        assert_eq!(track_name("", "", "English"), "");
        assert_eq!(
            track_name("English", "Full SDH", "English"),
            "Full SDH",
            "a useless PMS title falls through to the container's"
        );
        assert_eq!(
            track_name("  ", " Full ", "English"),
            "Full",
            "both sides are trimmed"
        );
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
