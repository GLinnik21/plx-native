//! In-player modal track menu: audio + subtitle pickers over the video, rendered on the reusable
//! animated `TableView` (Apple-TV "settings" look — a sliding pill selection, section header with
//! a codec accessory, per-row badges, a leading checkmark on the active track). app.rs routes
//! D-pad/OK/BACK here while the menu is open; LEFT/RIGHT switch between the Audio and Subtitles
//! panels. The selection commit (native audio switch / server transcode / burn) is unchanged
//! from the previous procedural version — only the presentation moved onto the table.
#![allow(dead_code)]
use crate::metadata;
use crate::ui::consts::{SCR_H, SCR_W, SDLK_DOWN, SDLK_LEFT, SDLK_RIGHT, SDLK_UP};
use crate::ui::popover::Popover;
use crate::ui::table::{Badge, Row, Section, TableView};
use crate::ui::theme;
use crate::ui::Rect;
use std::os::raw::c_int;
use std::ptr::{addr_of, addr_of_mut};

static mut POP: Popover = Popover::new(); // shared open/appear choreography
static mut TAB: c_int = 0; // 0=Audio, 1=Subtitles
static mut ACTIVE_AUDIO: c_int = 0; // index into the playing item's audio list
static mut ACTIVE_SUB: c_int = -1; // -1 = Off, else index into the playing item's subs list
static mut TABLE: TableView = TableView::new(); // main-thread only

fn table() -> &'static mut TableView {
    unsafe { &mut *addr_of_mut!(TABLE) }
}
fn pop() -> &'static mut Popover {
    unsafe { &mut *addr_of_mut!(POP) }
}

/// The PLAYING item's track lists — the menu's ONLY data source. `metadata::current()` is the
/// detail page's item, which is the SHOW during an episode play (its lists are episode 1's) and
/// can be a different item entirely when playing straight from Home.
fn tracks() -> Option<&'static metadata::PlayingItem> {
    metadata::playing()
}

pub(crate) fn is_open() -> bool {
    unsafe { (*addr_of!(POP)).is_open() }
}
/// index into the playing item's audio list of the chosen audio track
pub(crate) fn active_audio() -> c_int {
    unsafe { addr_of!(ACTIVE_AUDIO).read() }
}
/// -1 = subtitles off, else index into the playing item's subs list
pub(crate) fn active_sub() -> c_int {
    unsafe { addr_of!(ACTIVE_SUB).read() }
}
/// Plex stream id of the chosen audio track (for &audioStreamID), or 0
pub(crate) fn audio_stream_id() -> i64 {
    let i = active_audio();
    tracks().and_then(|t| t.audio.get(i.max(0) as usize)).map(|s| s.id).unwrap_or(0)
}
/// Plex stream id of the chosen subtitle track (for &subtitleStreamID), or 0 if Off
pub(crate) fn sub_stream_id() -> i64 {
    let i = active_sub();
    if i < 0 {
        return 0;
    }
    tracks().and_then(|t| t.subs.get(i as usize)).map(|s| s.id).unwrap_or(0)
}

fn n_audio() -> c_int {
    tracks().map(|t| t.audio.len()).unwrap_or(0) as c_int
}
/// Subtitle rows currently offered, as indices into the playing subs list. External/sidecar
/// subs are NOT in the container, so the client renderer can't show them on direct-play —
/// they're listed only while transcoding (the server can burn them).
fn visible_subs() -> Vec<usize> {
    tracks()
        .map(|t| {
            t.subs
                .iter()
                .enumerate()
                .filter(|(_, s)| !s.external || crate::route::is_transcoding())
                .map(|(i, _)| i)
                .collect()
        })
        .unwrap_or_default()
}
/// selectable rows in a tab — Subtitles has a leading "Off" row
fn n_rows(tab: c_int) -> c_int {
    if tab == 0 {
        n_audio()
    } else {
        visible_subs().len() as c_int + 1
    }
}
/// the table row that should be focused when entering `tab` (its active selection)
fn sel_for_tab(tab: c_int) -> c_int {
    if tab == 0 {
        active_audio().max(0)
    } else {
        let a = active_sub();
        // the row of the active subs-list index within the VISIBLE rows (+1 for Off)
        visible_subs()
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
/// Deliberately does NOT touch TAB: open_tab() has already chosen the tab when this runs.
fn sync_item() {
    let (audio, sub) = match tracks() {
        Some(t) => {
            let asid = crate::route::cur_audio_sid();
            let audio = (asid > 0)
                .then(|| t.audio.iter().position(|s| s.id == asid))
                .flatten()
                .or_else(|| t.audio.iter().position(|s| s.default))
                .unwrap_or(0) as c_int;
            let ssid = crate::route::cur_sub_sid();
            let sub = (ssid > 0)
                .then(|| t.subs.iter().position(|s| s.id == ssid))
                .flatten()
                .map(|i| i as c_int)
                .unwrap_or(-1);
            (audio, sub)
        }
        None => (0, -1),
    };
    unsafe {
        addr_of_mut!(ACTIVE_AUDIO).write(audio);
        addr_of_mut!(ACTIVE_SUB).write(sub);
    }
}

pub(crate) fn open() {
    sync_item();
    let tab = unsafe { addr_of!(TAB).read() };
    rebuild(tab, false);
    pop().open();
}
/// open focused on a specific tab (used by the on-screen audio/subs icons)
pub(crate) fn open_tab(tab: c_int) {
    unsafe { addr_of_mut!(TAB).write(tab) }
    open();
}
pub(crate) fn close() {
    pop().close();
}

/// Focus an ABSOLUTE table row — the /tmp/plxnative-menupick trigger's contract ("row N").
/// The interactive path always moves relatively; this exists because the initial focus is the
/// ACTIVE row (derived from playback state), so a relative walk from it would land elsewhere.
pub(crate) fn focus_row(row: c_int) {
    let t = table();
    for _ in 0..64 {
        if t.sel == row {
            break;
        }
        let before = t.sel;
        t.move_sel(if t.sel < row { 1 } else { -1 });
        if t.sel == before {
            break; // clamped at an end — row out of range
        }
    }
}

pub(crate) fn move_focus(sym: c_int) {
    let sym = sym as u32;
    let tab = unsafe { addr_of!(TAB).read() };
    if sym == SDLK_UP {
        table().move_sel(-1);
    } else if sym == SDLK_DOWN {
        table().move_sel(1);
    } else if sym == SDLK_LEFT || sym == SDLK_RIGHT {
        let nt = if sym == SDLK_LEFT { 0 } else { 1 };
        if nt != tab {
            unsafe { addr_of_mut!(TAB).write(nt) }
            rebuild(nt, false); // swap the whole list → snap the pill, no long glide
        }
    }
}

/// commit the focused row as the active track for its tab, then close
pub(crate) fn on_ok() {
    let tab = unsafe { addr_of!(TAB).read() };
    let sel = table().sel;
    pop().close();
    if tab == 0 {
        let changed = unsafe { addr_of!(ACTIVE_AUDIO).read() } != sel;
        unsafe { addr_of_mut!(ACTIVE_AUDIO).write(sel) }
        if changed {
            // the menu only reports the pick — native-switch vs re-transcode is route's policy.
            // The demuxer-facing index is the CONTAINER ordinal (audio_ordinal), not the row.
            if let Some(s) = tracks().and_then(|t| t.audio.get(sel.max(0) as usize)) {
                let ord = tracks()
                    .map(|t| metadata::audio_ordinal(&t.audio, sel.max(0) as usize))
                    .unwrap_or(sel);
                crate::route::commit_audio_selection(ord, &s.codec, s.id);
            }
        }
    } else {
        // row 0 = Off = -1; else map the visible row back to its subs-list index
        let vis = visible_subs();
        let new_sub: c_int = if sel <= 0 {
            -1
        } else {
            vis.get((sel - 1) as usize).map(|&i| i as c_int).unwrap_or(-1)
        };
        unsafe { addr_of_mut!(ACTIVE_SUB).write(new_sub) }
        // the client renderer takes the EMBEDDED-subtitle ordinal (what the demuxer
        // enumerates); an external pick (transcode-only row) renders nothing — it's burned
        let ridx = tracks()
            .filter(|_| new_sub >= 0)
            .map(|t| metadata::sub_render_ordinal(&t.subs, new_sub as usize))
            .unwrap_or(-1);
        crate::route::commit_subtitle_selection(ridx, sub_stream_id());
    }
}

// ---- section building ----
use crate::metadata::friendly_codec; // the ONE codec→display-name map (shared with the Info card)

/// Image (bitmap) subtitle codecs — PGS/VobSub/DVD/DVB. The demuxer software-decodes these to
/// RGBA and the player composites them over the video, so they render on the direct-play path;
/// the menu tags the codec for clarity.
pub(crate) fn is_image_sub_codec(codec: &str) -> bool {
    matches!(
        codec.to_ascii_lowercase().as_str(),
        "pgs" | "hdmv_pgs_subtitle" | "vobsub" | "dvd_subtitle" | "dvdsub" | "dvb_subtitle" | "dvbsub"
    )
}

fn build_audio() -> Section {
    let mut sec = Section::new("Audio");
    let d = match tracks() {
        Some(t) => t,
        None => return sec,
    };
    for (i, s) in d.audio.iter().enumerate() {
        let lang = if s.lang.is_empty() { "Unknown" } else { s.lang.as_str() };
        let label = if s.default { format!("Original: {lang}") } else { lang.to_string() };
        let mut row = Row::new(label).checked(i as c_int == active_audio());
        // a per-track descriptor so sibling tracks in the same language are distinguishable
        // (e.g. two Russian tracks: "Дубляж" vs "AC-3 5.1"). Prefer the stream title, else the
        // codec + channel layout.
        let sub = if !s.title.is_empty() && !s.title.eq_ignore_ascii_case(lang) {
            s.title.clone()
        } else {
            audio_descriptor(s)
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

fn build_subs() -> Section {
    let mut sec = Section::new("Subtitles");
    sec = sec.row(Row::new("Off").checked(active_sub() < 0));
    if let Some(t) = tracks() {
        for i in visible_subs() {
            let s = match t.subs.get(i) {
                Some(s) => s,
                None => continue,
            };
            let lang = if s.lang.is_empty() { "Unknown" } else { s.lang.as_str() };
            let mut row = Row::new(lang.to_string()).checked(i as c_int == active_sub());
            if !s.title.is_empty() && !s.title.eq_ignore_ascii_case(lang) {
                row = row.detail(s.title.clone());
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

fn rebuild(tab: c_int, slide: bool) {
    let sec = if tab == 0 { build_audio() } else { build_subs() };
    table().set_sections(vec![sec], sel_for_tab(tab), slide);
}

// ---- panel geometry (shared by update + draw so scrolling math matches) ----
fn panel_rect() -> Rect {
    let tab = unsafe { addr_of!(TAB).read() };
    let pw = if tab == 0 { 560.0f32 } else { 448.0f32 }; // audio / subtitles (mockup panel widths)
    let px = SCR_W - 80.0 - pw; // right:80 (mockup)
    // Bottom-anchored just above the control-button row (buttons top at SCR_H-288) with a clear gap.
    // The panel grows UPWARD from this fixed bottom edge, and its height is capped so the top never
    // crosses `top_min` — so a long list (Toy Story's many audio dubs) SCROLLS inside the panel
    // instead of the panel itself spilling down over the buttons. Switching Audio↔Subtitles keeps
    // the bottom edge steady.
    let bottom = SCR_H - 316.0; // 764 — ~28px above the buttons
    let top_min = 60.0;
    let ph = table().measured_height().clamp(160.0, bottom - top_min);
    let py = bottom - ph; // ≥ top_min by construction
    Rect::new(px, py, pw, ph)
}

pub(crate) fn update(dt: f32) {
    if !is_open() {
        return;
    }
    pop().update(dt);
    let ph = panel_rect().h;
    table().update(dt, ph - 40.0); // minus the panel's top/bottom padding
}

pub(crate) fn draw() {
    if !is_open() {
        return;
    }
    // modal scrim (dims the video plane showing through) + the appear fade/rise, via the shared
    // Popover choreography.
    let p = pop().painter(0.58, 20.0);
    let r = panel_rect();

    // frosted panel card — near-opaque dark (no true backdrop blur on the GLES plane, so a solid
    // dark card approximates it); only a hint of video shows through
    p.rect(r, 28.0, theme::PANEL_TOP, theme::PANEL_BOT, 0.0);

    table().draw(p, r);
}
