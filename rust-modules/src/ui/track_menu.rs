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
//! section holds Timing and Style; and everything else falls under "Other languages". That
//! grouping is a pure DATA model, `metadata::sub_layout` (host-tested without a
//! `PlaybackSession`/`MetadataView` fixture); this module only turns it into `TableView` sections
//! ([`sub_form`]) and answers focus/OK by the row's [`TrackRowId`]. "Yours" is
//! the pref language (if the play resolved under one), the playing audio's language, and the
//! current subtitle's own language, in that order (`route::cur_sub_pref_lang`, carried in by
//! `screens::player::overlay`).
//!
//! **Both tabs are keyed declarative forms** (`ui::form`, `docs/player-submenus.md`): each row is
//! declared once with its semantic [`TrackRowId`], a stable [`RowKey`] (the tab's base plus the
//! track's index in the playing item's list, which is fixed for the item's lifetime, never the
//! list position), its kind and its [`Row`], and the [`FormTable`] replaces the old hand-built
//! `Vec<Section>` with its parallel row-target vectors. The focus layer's element is the row's
//! key, so a row added above another (a subtitle offered mid-play, the enhancement pair returning)
//! moves no key, and a rebuild restores the viewer's row by id.
//!
//! **Timing** is a single value row that reads out the current offset (no chevron: it is a hand-off,
//! not a page); OK on it does not step anything here — it returns [`TrackOk::OpenTiming`], which
//! `screens::player::overlay`'s `activate` turns into a hand-off: it dismisses this panel and
//! presents the Timing capsule overlay (`OverlayKind::Timing`, `ui::timing_capsule`) in its place.
//! The row is dim and inert while subtitles are Off (OK there neither opens the capsule nor closes
//! the panel), and Timing together with Style is omitted during an ordinary transcode, which burns
//! captions into the picture where no client-side offset or style can reach. When the live route is
//! instead this app's OWN Plex Pass audio-enhancement Burn (M7), both stay visible — dim, with a
//! one-line reason (`Row::note`) — so the viewer who turned Boost dialog / Normalize loudness on
//! sees why the control is locked rather than finding it simply gone.
//!
//! **Style is a drill-in, and the Subtitles tab is a page stack** (`docs/player-submenus.md`). The
//! Style row is a [`RowKind::Nav`] onto [`TrackPage::Style`], whose Size, Position and Color rows
//! each read out their current value and push a picker page ([`TrackPage::Picker`]) of
//! [`RowKind::Choice`] rows with the current rung checked. A push remembers the opener's id and the
//! scroll ([`Saved`]); a pop ([`TrackMenuState::pop`], LEFT or BACK, or a click on the title band,
//! whose pointer-only stop is [`TITLE_KEY`]) restores both, so focus returns to the row that opened
//! the page by id. OK or RIGHT on a Nav row pushes ([`TrackMenuState::on_right`]); LEFT on the root
//! is still the tab switch and BACK on the root dismisses. Every page opens on an explicit id
//! ([`TrackMenuState::page_initial`]). A picker pick commits live and leaves the panel and page
//! open ([`TrackOk::Commit`]'s `keep_open`), so a run of picks is felt at once.
//!
//! **What Style can reach depends on the active renderer** ([`SubRenderer`]): only the client's
//! plain-text caption follows Size and Position, so under an image (PGS/VobSub) or native ASS/SSA
//! subtitle those two rows are dim, focusable and inert, with a separate note each. Color stays
//! live under every renderer: the subtitle ink tints bitmaps and ASS alike. Size and Position are
//! persisted by `route::select_subtitle_size` / `select_subtitle_position` (live value first, a
//! persist-only write second); Color by `player::set_subtitle_tone`.
//!
//! **A sub-page never outlives the root it was built on.** The root's [`SubSig`] — subs fingerprint,
//! active index, renderer kind, transcoding, own-burn and enhancement route — is stored when the
//! root is built and compared on every live poll; a mismatch refreshes the root in place, or pops a
//! sub-page straight to the root. The replay canon ([`TrackMenuState::canon`]) carries the tab, the
//! page path with each return id, and the selected key.
use crate::metadata;
use crate::metadata::sub_layout::{self, RowBadge, SubHeader, SubRow, SubSection, SubTrack};
use crate::metadata::track_label;
use crate::plex::session::{SubtitlePosition, SubtitleSize, SubtitleTone};
use crate::ui::consts::SCR_H;
use crate::ui::frame::Budget;
use crate::ui::geom::IndexElem;
use crate::ui::machine::{Canon, Cx, EntryId, FocusKey, GroupId, Host};
use crate::ui::popover::Popover;
use crate::ui::screen::{
    Activate, At, AxisMask, Dir, DrawFrame, EdgeRule, ElemKind, Focusable, GroupKind, GroupSpec,
    Hover, Part, Placed, Seat, Step, Stop,
};
use crate::ui::form::{Activation, Form, FormId, FormSection, FormTable, RowKey, RowKeys, RowKind};
use crate::ui::table::{Badge, Row, Section};
use crate::ui::table_screen::BAND_BASE;
use crate::ui::theme;
use crate::ui::{Painter, Rect};
use std::os::raw::c_int;


/// What a row of either tab IS — its semantic identity, stable across rebuilds, and the only
/// thing [`TrackMenuState::on_ok`] dispatches on (the id carries what the old `RowTarget` /
/// `AudioRowTarget` position maps carried, so the form's action type is `()`: there is no second
/// classification to drift from this one). Notes are inert slots with no id.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TrackRowId {
    /// Subtitles: the "Off" row.
    Off,
    /// Subtitles: a track row — the index into the playing item's subs list
    /// ([`crate::metadata::PlayingItem::subs`]).
    SubTrack(usize),
    /// Subtitles: the Timing row (hands off to the capsule).
    Timing,
    /// Subtitles root: the Style drill-in ([`TrackPage::Style`]).
    Style,
    /// Style page: the drill-in to one field's picker ([`TrackPage::Picker`]); reads out the
    /// field's current value.
    OpenField(StyleField),
    /// A picker page's choice: the field and the rung's index on that field's ladder.
    Choice(StyleField, usize),
    /// Audio: a track row — the index into the playing item's audio list
    /// ([`crate::metadata::PlayingItem::audio`]).
    AudioTrack(usize),
    /// Audio: the Boost dialog toggle (issue #266), present whenever
    /// [`TrackMenuState::enhance_shown`] or [`TrackMenuState::enhance_disabled`] is `Some`.
    Boost,
    /// Audio: the Normalize loudness toggle, right after [`Self::Boost`].
    Loudness,
}

/// One caption style field: the Style page lists one drill-in per field, and each opens a picker
/// page of that field's ladder with the current rung checked.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum StyleField {
    Size,
    Position,
    Color,
}

impl StyleField {
    pub(crate) const ALL: [StyleField; 3] = [StyleField::Size, StyleField::Position, StyleField::Color];

    fn ordinal(self) -> u32 {
        self as u32
    }

    fn label(self) -> &'static str {
        use crate::i18n::msg;
        match self {
            Self::Size => msg::widgets_tracks_style_size(),
            Self::Position => msg::widgets_tracks_style_position(),
            Self::Color => msg::widgets_tracks_color(),
        }
    }

    /// How many rungs this field's ladder has.
    fn rungs(self) -> usize {
        match self {
            Self::Size => SubtitleSize::LADDER.len(),
            Self::Position => SubtitlePosition::LADDER.len(),
            Self::Color => SubtitleTone::LADDER.len(),
        }
    }

    /// The localized name of rung `i` of this field's ladder.
    fn rung_label(self, i: usize) -> &'static str {
        match self {
            Self::Size => subtitle_size_label(SubtitleSize::from_index(i as u8)),
            Self::Position => subtitle_position_label(SubtitlePosition::from_index(i as u8)),
            Self::Color => tone_label(SubtitleTone::from_index(i as u8)),
        }
    }
}

/// **A drill-in page of the Subtitles tab** — the form's `Dest`. The Subtitles root is the empty
/// page stack, not a value of this type. PR 3's language pages are added here as further variants
/// (`docs/player-submenus.md`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TrackPage {
    /// Size, Position and Color, each a drill-in showing its current value.
    Style,
    /// One field's picker: a checked [`TrackRowId::Choice`] per rung.
    Picker(StyleField),
}

impl TrackPage {
    /// The title band's text.
    fn title(self) -> &'static str {
        match self {
            Self::Style => crate::i18n::msg::widgets_tracks_style(),
            Self::Picker(field) => field.label(),
        }
    }

    /// A stable small number for the replay canon. Never reordered: recordings hash it.
    fn code(self) -> u32 {
        match self {
            Self::Style => 1,
            Self::Picker(field) => 0x10 + field.ordinal(),
        }
    }
}

/// Each tab's key base. The focus layer's element is a [`RowKey`]'s number, so the bases keep the
/// two tabs' keys disjoint: an engine key held from the other tab never names a row here.
const AUDIO_KEY_BASE: u32 = 0x100;
const SUB_KEY_BASE: u32 = 0x200;
/// The Style pages' rows: disjoint from both tabs' keys, so an engine key held from the root (or
/// from another page) never names a row of the page that just opened.
const PAGE_KEY_BASE: u32 = 0x300;
/// A picker's choices sit past the Style page's three drill-ins, one 16-wide block per field.
const CHOICE_KEY_AT: u32 = 0x10;
const CHOICES_PER_FIELD: u32 = 0x10;
/// Fixed rows sit below this offset inside a base; a track sits at `base + TRACK_KEY_AT + index`.
const TRACK_KEY_AT: u32 = 0x10;
/// A track index never reaches the next base (a list that long is not a real item); saturating
/// keeps every key under [`BAND_BASE`], the ceiling the form enforces.
const TRACK_KEY_MAX: u32 = 0xEF;
/// The pointer-only key of the page title band ("< STYLE"): OUTSIDE the form's key range (at the
/// ceiling), so it is never a row and never in the D-pad column. A click on it pops.
pub(crate) const TITLE_KEY: u32 = BAND_BASE;

impl FormId for TrackRowId {
    /// Hand-assigned, never the row's list position: a track's key is its index in the item's
    /// list (fixed for the item), every other row has a constant.
    fn key(&self) -> RowKey {
        let track = |i: usize| TRACK_KEY_AT + (i as u32).min(TRACK_KEY_MAX - TRACK_KEY_AT);
        RowKey(match *self {
            Self::Off => SUB_KEY_BASE,
            Self::Timing => SUB_KEY_BASE + 1,
            Self::Style => SUB_KEY_BASE + 2,
            Self::SubTrack(i) => SUB_KEY_BASE + track(i),
            Self::OpenField(f) => PAGE_KEY_BASE + f.ordinal(),
            Self::Choice(f, i) => {
                PAGE_KEY_BASE + CHOICE_KEY_AT + f.ordinal() * CHOICES_PER_FIELD + (i as u32).min(CHOICES_PER_FIELD - 1)
            }
            Self::Boost => AUDIO_KEY_BASE,
            Self::Loudness => AUDIO_KEY_BASE + 1,
            Self::AudioTrack(i) => AUDIO_KEY_BASE + track(i),
        })
    }
}

/// Both tabs' form: the action type is `()` (the id says what a row does); `Dest` is the page a
/// Nav row opens.
type TrackForm = Form<TrackRowId, (), TrackPage>;
type TrackTable = FormTable<TrackRowId, (), TrackPage>;

/// **What a pushed page remembers of the page beneath it**: the opener's id and the scroll the
/// list was left at, so a pop ([`FormTable::restore`]) brings the page back exactly as it was.
#[derive(Clone, Copy, Debug)]
struct Saved {
    /// The page this entry opened.
    page: TrackPage,
    /// The row that opened it, on the page beneath.
    return_id: TrackRowId,
    scroll: f32,
}

/// The renderer the ACTIVE subtitle is drawn by — what decides whether the caption's Size and
/// Position can reach it. Only the client's plain-text caption draw follows them
/// (`ui::player_hud::draw_subtitle_message`); an image subtitle keeps its own bitmap geometry and
/// native ASS/SSA its authored layout. The subtitle INK tints all three, so Color is always live.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SubRenderer {
    Text,
    Image,
    Styled,
}

impl SubRenderer {
    fn of_codec(codec: &str) -> Self {
        if sub_layout::is_image_sub_codec(codec) {
            Self::Image
        } else if codec.eq_ignore_ascii_case("ass") || codec.eq_ignore_ascii_case("ssa") {
            Self::Styled
        } else {
            Self::Text
        }
    }

    /// The note under the Style rows, when Size and Position cannot reach this renderer.
    fn note(self) -> Option<&'static str> {
        match self {
            Self::Text => None,
            Self::Image => Some(crate::i18n::msg::widgets_tracks_style_image_note()),
            Self::Styled => Some(crate::i18n::msg::widgets_tracks_style_styled_note()),
        }
    }
}

/// **What the Subtitles root's rows and locks were built from** — the rebuild signature. A live
/// poll compares it to the current answers and rebuilds when any input changed: the subs list
/// (stream ids and whether each is offered on this route), the active index, the renderer kind,
/// whether the route is transcoding, and the enhancement route and subtitle effect (what the Style
/// lock and Timing's omission read).
#[derive(Clone, Debug, PartialEq)]
struct SubSig {
    subs: Vec<(i64, bool)>,
    active: c_int,
    renderer: SubRenderer,
    transcoding: bool,
    own_burn: bool,
    enhancement: Option<crate::route::EnhancementRoute>,
    effect: crate::route::SubtitleEffect,
}

/// The menu's whole state, owned by the container that mounts this panel — the modal PHASE and the
/// appear spring belong to `ui::containers::modal::ModalStack` now, not to this struct; `draw` takes
/// the appear fraction as a parameter instead of stepping its own [`Popover`].
pub(crate) struct TrackMenuState {
    tab: c_int, // 0=Audio, 1=Subtitles
    active_audio: c_int, // index into the playing item's audio list
    active_sub: c_int, // -1 = Off, else index into the playing item's subs list
    /// The timing offset (ms) the Timing row reads out — seeded from the player on open. Kept
    /// locally (rather than re-reading the player's atomic on every draw) so the Timing capsule's
    /// eventual hand-off starts from what THIS panel showed, not from a commit the loop has not
    /// yet performed.
    offset_ms: i64,
    /// The caption tone the Color rows read out and check — seeded from the player on open, same
    /// reasoning as [`Self::offset_ms`]: what this panel last drew must not depend on the global
    /// the loop has not yet written (`TrackCommit::SubtitleTone` is dispatched to the loop, not
    /// applied inline by `on_ok`). The size and position below follow the same rule.
    tone: SubtitleTone,
    /// The caption size the Size rows read out and check.
    size: SubtitleSize,
    /// The caption position the Position rows read out and check.
    position: SubtitlePosition,
    /// The pages pushed above the Subtitles root, outermost first (empty = the root). Each entry
    /// carries the opener and scroll of the page beneath it.
    pages: Vec<Saved>,
    /// The renderer of the ACTIVE subtitle, captured when the root is (re)built — what the Style
    /// page's Size and Position lock reads.
    renderer: SubRenderer,
    /// What the Subtitles root was last built from; a live poll rebuilds when it moves.
    sub_sig: Option<SubSig>,
    /// "Your languages" this play resolved under, in PREFERENCE order — the pref's BCP-47 code (if
    /// the play resolved under one), the playing audio's language, and the current subtitle's own,
    /// exactly as `metadata::sub_layout::sub_sections`' `yours` parameter reads them (compared by
    /// `metadata::lang_key`, never by literal string equality). Owned rather than borrowed, so
    /// a rebuild (tab switch) needs nothing from the caller beyond `ps`/`meta`.
    yours: Vec<String>,
    /// The Audio tab's Plex Pass DSP toggle rows (issue #266) — `None` when they are not offered
    /// at all (Hidden or Disabled), else what they currently read out: "desired while pending,
    /// applied otherwise" (`route::displayed_audio_enhancements`), same reasoning as
    /// [`Self::offset_ms`]/[`Self::tone`] — a run of toggle presses inside one open counts from
    /// what THIS panel last drew, and [`Self::rebuild`] is the only writer, on every (re)build of
    /// the Audio tab (`new`/`focus_tab`).
    enhance_shown: Option<crate::plex::AudioEnhancements>,
    /// The route [`Self::enhance_shown`] would take, kept alongside it (`Some` iff `enhance_shown`
    /// is `Some`) — the Audio tab's consequence note (M7: a burned subtitle, a dropped Dolby Vision
    /// declaration) reads the flavour, not just whether the toggle is on.
    enhance_route: Option<crate::route::EnhancementRoute>,
    /// Why the toggle is drawn dim with a reason instead of offered — `Some` only when the owner's
    /// "hidden stays ONLY for no Plex Pass" direction (2026-09-29) applies a plain-language reason
    /// instead of the older silent absence (I1/I2 now covers the Plex-Pass gate alone).
    /// `None` together with [`Self::enhance_shown`]'s `None` means the rows are HIDDEN entirely.
    enhance_disabled: Option<crate::route::DisabledReason>,
    /// What is on screen right now (M7) — read alongside [`Self::enhance_route`] purely for the
    /// Audio tab's consequence NOTE, which needs to tell "no subtitle" apart from "an unaffected
    /// sidecar" even though [`enhancement_availability`](crate::route::EnhancementAvailability)
    /// folds both into the same [`crate::route::EnhancementRoute::Remux`].
    enhance_subtitle_effect: crate::route::SubtitleEffect,
    /// The Audio tab's focused row identity, banked across a live rows-VANISH: the enhancement
    /// offer can drop for a poll or two on a route change this menu never asked for (a subtitle
    /// switched on mid-play, a momentary refusal) and return before the viewer presses anything.
    /// [`Self::refresh_audio`] stashes the focused [`TrackRowId::Boost`]/[`TrackRowId::Loudness`]
    /// here the moment those rows are about to disappear, and hands it to
    /// [`FormTable::refresh_with`] as the PREFERRED landing the moment they reappear. Restoring
    /// by the table's own selected id instead would be wrong: while the rows are gone the form
    /// landed the selection on a neighbouring TRACK, which is what it would keep once the
    /// enhancement rows are back. `None` once consumed, or when nothing needs remembering.
    sticky_audio_target: Option<TrackRowId>,
    /// M7 follow-up: is the live route actually burning a subtitle into the picture RIGHT NOW
    /// (`route::live_is_own_burn`)? Captured on every (re)build of the Subtitles tab
    /// ([`Self::sub_form`]) so the poll can tell when it changed. While true, Timing and Style are
    /// declared DISABLED in the form (dim with a one-line reason, inert under OK at the form
    /// layer): the text is already in the pixels, and nothing this panel does can reach it.
    /// Track-selection rows are unaffected — picking another subtitle (or Off) re-routes normally.
    sub_style_locked: bool,
    /// The active page's rows, their ids, keys and the table that draws them — installed whole on
    /// open / tab switch / push ([`FormTable::open`]), reinstated on a pop
    /// ([`FormTable::restore`]) and rebuilt in place on a live poll ([`FormTable::refresh_with`]).
    form: TrackTable, // main-thread only
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
    /// The caption's size, picked on the Style > Size page: the loop publishes it live and
    /// persists it (`route::select_subtitle_size`).
    SubtitleSize(SubtitleSize),
    /// The caption's vertical position, picked on the Style > Position page
    /// (`route::select_subtitle_position`).
    SubtitlePosition(SubtitlePosition),
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
    /// Perform `commit`. `keep_open` is true for a Style pick (the viewer is watching the caption
    /// change, so a run of picks needs no reopen-and-rewalk between them) and for the Audio
    /// toggles; every track pick closes the panel.
    Commit { commit: TrackCommit, keep_open: bool },
    /// A Nav row opened a page ([`TrackMenuState::push`]): the panel stays, its rows replaced.
    Navigated,
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
            offset_ms: crate::player::subtitle_offset_ms(),
            tone: crate::player::subtitle_tone(),
            size: crate::route::subtitle_size(),
            position: crate::route::subtitle_position(),
            pages: Vec::new(),
            renderer: SubRenderer::Text,
            sub_sig: None,
            yours,
            enhance_shown: None,
            enhance_route: None,
            enhance_disabled: None,
            enhance_subtitle_effect: crate::route::SubtitleEffect::None,
            sticky_audio_target: None,
            sub_style_locked: false,
            form: TrackTable::new(BAND_BASE),
        };
        s.sync_item(ps, meta);
        s.rebuild(ps, meta, tab);
        s
    }

    /// The highlighted row's INDEX, for the focus probe (`crate::focusprobe`) and the replay
    /// canon — a READ of the cursor the key ladder moves, and the reason it exists: `app.rs`'s
    /// UP/DOWN arm for this panel changes nothing else, so without this the fingerprint records
    /// the panel opening and closing and nothing between.
    pub(crate) fn sel(&self) -> i32 {
        self.form.table.sel
    }

    /// **The replay canon** of this panel: the tab, the page path (each pushed page and the row
    /// that opened it) and the selected row's [`RowKey`] — not its index, which moves when a row
    /// is added above it. `screens::player::overlay` writes this in place of the bare row index, so
    /// a replay tells Subtitles from Audio, the root from a Style page, and two return stacks
    /// apart.
    pub(crate) fn canon(&self, c: &mut Canon) {
        c.u32(self.tab as u32).u32(self.pages.len() as u32);
        for saved in &self.pages {
            c.u32(saved.page.code()).u32(saved.return_id.key().0);
        }
        c.u32(self.form.key_at(self.form.table.sel.max(0) as usize).map_or(u32::MAX, |k| k.0));
    }

    /// The pages pushed above the Subtitles root, outermost first, for tests and probes.
    #[cfg(test)]
    pub(crate) fn page_path(&self) -> Vec<TrackPage> {
        self.pages.iter().map(|s| s.page).collect()
    }

    /// Is this the pointer-only key of the title band ([`TITLE_KEY`])?
    pub(crate) fn is_title_key(elem: u32) -> bool {
        elem == TITLE_KEY
    }

    /// The highlighted row's id (`None` on a note or an empty list).
    #[cfg(test)]
    pub(crate) fn selected_id(&self) -> Option<TrackRowId> {
        self.form.selected_id().copied()
    }

    /// The focus element (key number) of every focusable row, in drawn order.
    #[cfg(test)]
    pub(crate) fn row_keys(&self) -> Vec<u32> {
        (0..self.form.table.n_rows().max(0) as usize).filter_map(|i| self.form.key_at(i)).map(|k| k.0).collect()
    }

    /// Every drawn row's id in drawn order (`None` at an inert note), for tests that pin a tab's
    /// shape without hard-coding a row index.
    #[cfg(test)]
    pub(crate) fn row_ids(&self) -> Vec<Option<TrackRowId>> {
        (0..self.form.table.n_rows().max(0) as usize).map(|i| self.form.id_at(i).copied()).collect()
    }

    /// **Write back the engine's own focus cursor** (restructure phase 12): the Column group
    /// [`TrackMenuPart`] answers is the source of geometry, but the ENGINE owns the current
    /// element (§7.3 step 5) — the owner's `step` is the only place that mutates in response to a
    /// `FocusMoved`, and this is `screens::player::overlay::PlayerOverlayScreen::step`'s write.
    /// `elem` is a row's [`RowKey`] number; a key the tab does not know (the other tab's, a row
    /// that left) moves nothing, and the form records what the engine holds so a rebuild that
    /// lands elsewhere can ask the engine to follow ([`FormTable::note_engine_key`]).
    pub(crate) fn focus_key(&mut self, elem: u32) {
        let key = RowKey(elem);
        let at = self.form.index_of_key(key);
        self.form.note_engine_key(at.map(|_| key));
        if let Some(i) = at {
            self.form.table.sel = i as i32;
        }
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

    /// Derive the checked tracks from the PLAYBACK state — the route owns the truth
    /// (CUR_AUDIO_SID/CUR_SUB_SID, set by the start-of-play pick and every commit): the auto-picked
    /// default/smart-DP track is checked on first open, a replayed item resets with the playback,
    /// and a prior pick round-trips by id. When no id is recorded (codec-default play), the file's
    /// flagged default is checked. Free function (no `&self`) so both [`Self::sync_item`] (on
    /// every open) and the live poll below (PR #309 field report) derive the SAME pair the same
    /// way — the desync that report caught was exactly two readers of this answer drifting apart.
    fn derive_active(ps: &crate::route::PlaybackSession, meta: metadata::MetadataView<'_>) -> (c_int, c_int) {
        (Self::derive_active_audio(ps, meta), Self::derive_active_sub(ps, meta))
    }

    /// The Audio tab's half of [`Self::derive_active`].
    fn derive_active_audio(ps: &crate::route::PlaybackSession, meta: metadata::MetadataView<'_>) -> c_int {
        let Some(t) = tracks(meta) else { return 0 };
        let asid = crate::route::cur_audio_sid(ps);
        (asid > 0)
            .then(|| t.audio.iter().position(|s| s.id == asid))
            .flatten()
            .or_else(|| t.audio.iter().position(|s| s.default))
            .unwrap_or(0) as c_int
    }

    /// The Subtitles tab's half of [`Self::derive_active`] — what the live poll scans alone, the
    /// audio half being discarded there.
    fn derive_active_sub(ps: &crate::route::PlaybackSession, meta: metadata::MetadataView<'_>) -> c_int {
        let Some(t) = tracks(meta) else { return -1 };
        let ssid = crate::route::cur_sub_sid(ps);
        (ssid > 0)
            .then(|| t.subs.iter().position(|s| s.id == ssid))
            .flatten()
            .map(|i| i as c_int)
            .unwrap_or(-1)
    }

    /// [`Self::derive_active`] on every open — the menu can never show a stale or desynced
    /// checkmark at the moment it appears. Deliberately does NOT touch `tab`:
    /// [`TrackMenuState::new`] sets it directly.
    fn sync_item(&mut self, ps: &crate::route::PlaybackSession, meta: metadata::MetadataView<'_>) {
        let (audio, sub) = Self::derive_active(ps, meta);
        self.active_audio = audio;
        self.active_sub = sub;
    }

    /// The Subtitles tab's half of the live poll `Self::update` runs every tick, mirroring the
    /// Audio tab's own `enh_state` poll just above it. Issue #309's field report: a subtitle pick
    /// that reroutes the play to (or away from) the enhancement's own Burn lands `active_sub`
    /// at once (`Self::on_ok`'s own optimistic write), but `sub_style_locked` can only become true
    /// once the Burn's `/decision` round trip actually answers (`route::decision::retranscode_as`,
    /// a real network call) — seconds later. A panel that stays open across that window (the
    /// diagnostic `screens::player::overlay::pick_track_row` trigger deliberately does, "so a
    /// capture can show the picked track") was built and never touched again, so its drawn
    /// checkmark and its Style/Timing dim state both kept whatever `Self::sub_form` baked at open,
    /// disagreeing with the route by the time a capture actually looked at it. `rebuild`'s own
    /// subtitle arm always re-homes the cursor onto the checked row — correct for an open or a tab
    /// switch, wrong for a background poll that must not steal focus from wherever the viewer's
    /// cursor actually is (the exact focus-desync class `refresh_audio`'s doc names)
    /// — so this refreshes the form, which restores the current row by [`TrackRowId`] in the
    /// freshly built list instead of snapping to the active track, the same fix `refresh_audio` applies for the Audio
    /// tab's own poll.
    ///
    /// **It rebuilds on a [`SubSig`] change**, not on the two values it once compared: the subs
    /// list, the active index, the renderer kind, transcoding, and the enhancement route and
    /// subtitle effect. On the root that is a refresh in place; ON A SUB-PAGE the page is popped to
    /// the root (its availability came from the root it was opened on), so a Style page never
    /// outlives the renderer or the lock that its Size and Position rows were built for.
    fn poll_subtitle_state(&mut self, ps: &crate::route::PlaybackSession, meta: metadata::MetadataView<'_>) {
        let live_sub = Self::derive_active_sub(ps, meta);
        let sig = self.sub_sig_for(ps, meta, live_sub);
        if self.sub_sig.as_ref() == Some(&sig) {
            return;
        }
        self.active_sub = live_sub;
        match self.pages.first().copied() {
            None => {
                let form = self.sub_form(ps, meta);
                // not the viewer's row any more (its track left the offered list): the checked row
                self.form.refresh_with(form, None, Some(&self.active_sub_id()));
            }
            Some(first) => {
                // The page was opened on a root that no longer holds (the renderer changed under
                // Size/Position, a burn landed, the list moved): pop to the root, restoring the
                // opener and scroll the stack saved at its first push.
                self.pages.clear();
                let form = self.sub_form(ps, meta);
                self.form.restore(form, Some(&first.return_id), first.scroll);
                self.form.table.set_title(None);
            }
        }
    }

    /// The Subtitles root's rebuild signature for `active`, read fresh off the route and the
    /// item. [`Self::sub_form`] stores the same value it builds from.
    fn sub_sig_for(&self, ps: &crate::route::PlaybackSession, meta: metadata::MetadataView<'_>, active: c_int) -> SubSig {
        let item = tracks(meta);
        let offered = visible_subs(ps, meta);
        let subs = item
            .map(|t| t.subs.iter().enumerate().map(|(i, s)| (s.id, offered.contains(&i))).collect())
            .unwrap_or_default();
        let renderer = item
            .and_then(|t| t.subs.get(usize::try_from(active).ok()?))
            .map_or(SubRenderer::Text, |s| SubRenderer::of_codec(&s.codec));
        let (_, enhancement, _, effect) = Self::enh_state(ps);
        SubSig {
            subs,
            active,
            renderer,
            transcoding: crate::route::is_transcoding(ps),
            own_burn: crate::route::live_is_own_burn(ps),
            enhancement,
            effect,
        }
    }

    /// The Subtitles row carrying the checkmark: the active track, or Off.
    fn active_sub_id(&self) -> TrackRowId {
        match self.active_sub {
            i if i >= 0 => TrackRowId::SubTrack(i as usize),
            _ => TrackRowId::Off,
        }
    }

    /// Focus an ABSOLUTE table row — the /tmp/plxnative-menupick trigger's contract ("row N"), the
    /// one place a row POSITION is still the address, because the harness file names one. The
    /// interactive path always moves relatively; this exists because the initial focus is the
    /// ACTIVE row (derived from playback state), so a relative walk from it would land elsewhere.
    /// The cursor it leaves is read back by id ([`Self::on_ok`]), never by the number.
    pub(crate) fn focus_row(&mut self, row: c_int) {
        for _ in 0..64 {
            if self.form.table.sel == row {
                break;
            }
            let before = self.form.table.sel;
            self.form.table.move_sel(if self.form.table.sel < row { 1 } else { -1 });
            if self.form.table.sel == before {
                break; // clamped at an end — row out of range
            }
        }
    }

    /// Resolve a NAMED Audio-tab target (`"boost"`/`"loudness"`) to its absolute table row, for
    /// the `/tmp/plxnative-menupick` trigger's named form — an alternative to a row number hand-
    /// derived from the item's track count, which is exactly the issue #266 PR4 bug: this harness
    /// once hardcoded the Normalize Loudness row from a WRONG assumed track count. Looked up by
    /// [`TrackRowId`], the identity [`Self::on_ok`] dispatches on, the name is correct however many
    /// tracks the item actually has. `None` when `name` is unrecognized, or recognized but not
    /// currently built (the DSP toggle rows are not offered right now).
    pub(crate) fn row_for_audio_target(&self, name: &str) -> Option<c_int> {
        let id = match name {
            "boost" => TrackRowId::Boost,
            "loudness" => TrackRowId::Loudness,
            _ => return None,
        };
        self.form.index_of(&id).map(|i| i as c_int)
    }

    /// Resolve a NAMED Subtitles-tab target to its absolute table row, for the
    /// `/tmp/plxnative-menupick` trigger: `"track:N"` is the N-th (0-based) TRACK row in display
    /// order, skipping Off, Timing, Style and the footnote. A hand-written row number drifts every
    /// time the panel gains or loses a row (the `subtitle_text_srt` case picked row 3, which became
    /// the Style row); reading the position back through the row ids, the same identity
    /// [`Self::on_ok`] dispatches on, cannot. `None` for an unrecognized name or an N past the
    /// last track.
    pub(crate) fn row_for_sub_target(&self, name: &str) -> Option<c_int> {
        let n: usize = name.strip_prefix("track:")?.trim().parse().ok()?;
        (0..self.form.table.n_rows().max(0) as usize)
            .filter(|&row| matches!(self.form.id_at(row), Some(TrackRowId::SubTrack(_))))
            .nth(n)
            .map(|row| row as c_int)
    }

    /// Show `tab` (0=Audio, 1=Subtitles) on a menu that is ALREADY open — the second disc pressed
    /// while the first one's tab is showing. Same body as the LEFT/RIGHT arm below, which is why
    /// that arm calls this rather than repeating it.
    pub(crate) fn focus_tab(&mut self, ps: &crate::route::PlaybackSession, meta: metadata::MetadataView<'_>, tab: c_int) {
        if tab != self.tab {
            self.tab = tab;
            self.rebuild(ps, meta, tab); // swap the whole list → snap the pill, no long glide
        }
    }

    /// commit the focused row as the active track for its tab — dismissing the panel afterward is
    /// the container's job now, not this method's; the answer says whether it should. The row is
    /// read back by its [`TrackRowId`] (never by position), and a row the form declared DISABLED
    /// (the dim enhancement pair, Timing/Style under a live burn) is inert at the form layer.
    pub(crate) fn on_ok(&mut self, meta: metadata::MetadataView<'_>) -> TrackOk {
        let sel = self.form.table.sel.max(0) as usize;
        let id = self.form.selected_id().copied();
        if self.tab == 0 {
            let Some(id) = id else { return TrackOk::Dismiss };
            if self.form.activate(sel).is_none() {
                // A `Disabled` pair is drawn dim, reading Off, and OK on it is a no-op — the same
                // "focusable but inert" shape Timing uses while subtitles are Off.
                return TrackOk::Inert;
            }
            return match id {
                TrackRowId::Boost | TrackRowId::Loudness => {
                    let mut a = self.enhance_shown.unwrap_or(crate::plex::AudioEnhancements::NONE);
                    if id == TrackRowId::Boost {
                        a.boost_dialog = !a.boost_dialog;
                    } else {
                        a.normalize_loudness = !a.normalize_loudness;
                    }
                    self.enhance_shown = Some(a);
                    if let Some(row) = self.form.table.row_mut(sel as i32) {
                        row.toggle = Some(if id == TrackRowId::Boost {
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
                TrackRowId::AudioTrack(i) => {
                    let changed = self.active_audio != i as c_int;
                    self.active_audio = i as c_int;
                    if changed {
                        // the menu only reports the pick — native-switch vs re-transcode is
                        // route's policy. The demuxer-facing index is the CONTAINER ordinal
                        // (audio_ordinal), not the row.
                        if let Some(s) = tracks(meta).and_then(|t| t.audio.get(i)) {
                            let ord = tracks(meta)
                                .map(|t| metadata::audio_ordinal(&t.audio, i))
                                .unwrap_or(i as c_int);
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
                _ => TrackOk::Inert,
            };
        }

        let Some(id) = id else { return TrackOk::Inert };
        match self.form.activate(sel) {
            // M7 follow-up: while the live route is actually burning a subtitle into the picture,
            // Timing and Style are declared disabled (`Self::sub_form`) — the text is already in
            // the pixels — and so are Size/Position under an image or styled subtitle: OK on any
            // of them is a no-op.
            None => return TrackOk::Inert,
            Some(Activation::Push(dest)) => {
                self.push(dest);
                return TrackOk::Navigated;
            }
            Some(Activation::Action(())) => {}
        }
        match id {
            TrackRowId::Choice(field, rung) => self.pick_style(field, rung),
            // Read LIVE, not from the form: a pick in this same open (`active_sub` written below,
            // no rebuild) must make the next OK on Timing open the capsule.
            TrackRowId::Timing if self.active_sub >= 0 => TrackOk::OpenTiming,
            TrackRowId::Timing => TrackOk::Inert, // dim and inert while subtitles are Off
            TrackRowId::Off | TrackRowId::SubTrack(_) => {
                // Off → -1; else the row's own subs-list index
                let new_sub: c_int = match id {
                    TrackRowId::SubTrack(i) => i as c_int,
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
            _ => TrackOk::Inert,
        }
    }

    /// A Style picker's pick: the field's new rung, committed live, the panel and page staying so a
    /// run of picks is felt at once. The page's checkmark moves by refreshing it in place (scroll
    /// and focus kept). The already-checked rung is inert: nothing changes, nothing is written.
    fn pick_style(&mut self, field: StyleField, rung: usize) -> TrackOk {
        let commit = match field {
            StyleField::Size => {
                let size = SubtitleSize::from_index(rung as u8);
                if size == self.size {
                    return TrackOk::Inert;
                }
                self.size = size;
                TrackCommit::SubtitleSize(size)
            }
            StyleField::Position => {
                let position = SubtitlePosition::from_index(rung as u8);
                if position == self.position {
                    return TrackOk::Inert;
                }
                self.position = position;
                TrackCommit::SubtitlePosition(position)
            }
            StyleField::Color => {
                let tone = SubtitleTone::from_index(rung as u8);
                if tone == self.tone {
                    return TrackOk::Inert;
                }
                self.tone = tone;
                TrackCommit::SubtitleTone(tone)
            }
        };
        if let Some(page) = self.pages.last().map(|s| s.page) {
            let form = self.page_form(page);
            self.form.refresh(form);
        }
        TrackOk::Commit { commit, keep_open: true }
    }

    /// The rung of `field` the panel currently reads out and checks.
    fn current_rung(&self, field: StyleField) -> usize {
        let rung = match field {
            StyleField::Size => self.size.index(),
            StyleField::Position => self.position.index(),
            StyleField::Color => self.tone.index(),
        };
        rung as usize
    }

    /// The form of a pushed page, from the panel's own read-outs.
    fn page_form(&self, page: TrackPage) -> TrackForm {
        match page {
            TrackPage::Style => self.style_form(),
            TrackPage::Picker(field) => self.picker_form(field),
        }
    }

    /// The Style page: one drill-in per field, each reading out its current value. Under an image
    /// or styled subtitle Size and Position are disabled (dim, focusable, inert) and one note names
    /// why; Color is live under every renderer — the subtitle ink tints bitmaps and ASS alike.
    fn style_form(&self) -> TrackForm {
        let field_row = |field: StyleField| {
            Row::new(field.label()).value(field.rung_label(self.current_rung(field)))
        };
        let reaches = self.renderer == SubRenderer::Text;
        let mut sec = FormSection::new("")
            .item(
                TrackRowId::OpenField(StyleField::Size),
                RowKind::Nav(TrackPage::Picker(StyleField::Size)),
                (),
                field_row(StyleField::Size),
            )
            .disabled(!reaches)
            .item(
                TrackRowId::OpenField(StyleField::Position),
                RowKind::Nav(TrackPage::Picker(StyleField::Position)),
                (),
                field_row(StyleField::Position),
            )
            .disabled(!reaches)
            .item(
                TrackRowId::OpenField(StyleField::Color),
                RowKind::Nav(TrackPage::Picker(StyleField::Color)),
                (),
                field_row(StyleField::Color),
            );
        if let Some(note) = self.renderer.note() {
            sec = sec.note(note);
        }
        Form::new().section(sec)
    }

    /// A picker page: one choice per rung of `field`'s ladder, the current one checked.
    fn picker_form(&self, field: StyleField) -> TrackForm {
        let current = TrackRowId::Choice(field, self.current_rung(field));
        let sec = (0..field.rungs()).fold(FormSection::new(""), |sec, rung| {
            sec.choice(TrackRowId::Choice(field, rung), (), Row::new(field.rung_label(rung)), |id| *id == current)
        });
        Form::new().section(sec)
    }

    /// The explicit initial focus of a pushed page: the Style page opens on Size, a picker on its
    /// checked rung.
    fn page_initial(&self, page: TrackPage) -> TrackRowId {
        match page {
            TrackPage::Style => TrackRowId::OpenField(StyleField::Size),
            TrackPage::Picker(field) => TrackRowId::Choice(field, self.current_rung(field)),
        }
    }

    /// Open `page` above the current one: remember the opener and its scroll, install the page's
    /// rows (the pill snaps, the scroll returns to the top, focus lands on
    /// [`Self::page_initial`]) and its title band.
    fn push(&mut self, page: TrackPage) {
        let Some(return_id) = self.form.selected_id().copied() else { return };
        self.pages.push(Saved { page, return_id, scroll: self.form.table.scroll_pos() });
        let form = self.page_form(page);
        self.form.open(form, Some(&self.page_initial(page)));
        // after the sections: the band moves every row, so the pill is re-jumped onto the focused one
        self.form.table.set_title(Some(page.title().to_string()));
    }

    /// Pop the top page: the page beneath comes back exactly as it was left — the opener focused
    /// by id, the scroll reinstated ([`FormTable::restore`]). `false` at the root (nothing popped).
    pub(crate) fn pop(&mut self, ps: &crate::route::PlaybackSession, meta: metadata::MetadataView<'_>) -> bool {
        let Some(saved) = self.pages.pop() else { return false };
        match self.pages.last().map(|s| s.page) {
            None => {
                let form = self.sub_form(ps, meta);
                self.form.restore(form, Some(&saved.return_id), saved.scroll);
                self.form.table.set_title(None);
            }
            Some(page) => {
                let form = self.page_form(page);
                self.form.restore(form, Some(&saved.return_id), saved.scroll);
                self.form.table.set_title(Some(page.title().to_string()));
            }
        }
        true
    }

    /// **LEFT**: pop a sub-page, else (on a root) the tab switch as before.
    pub(crate) fn on_left(&mut self, ps: &crate::route::PlaybackSession, meta: metadata::MetadataView<'_>) {
        if !self.pop(ps, meta) {
            self.focus_tab(ps, meta, 0);
        }
    }

    /// **RIGHT**: on a Nav row it enters — the same as OK, and inert on a disabled one at the form
    /// layer — else the tab switch as before.
    pub(crate) fn on_right(&mut self, ps: &crate::route::PlaybackSession, meta: metadata::MetadataView<'_>) {
        let sel = self.form.table.sel.max(0) as usize;
        if matches!(self.form.binding_at(sel).map(|b| &b.kind), Some(RowKind::Nav(_))) {
            if let Some(Activation::Push(dest)) = self.form.activate(sel) {
                self.push(dest);
            }
        } else {
            self.focus_tab(ps, meta, 1);
        }
    }

    /// The plain-language reason a `Disabled` pair is drawn dim (owner direction, 2026-09-29): every
    /// gate but "no Plex Pass" now says why instead of vanishing.
    fn enh_reason_text(reason: crate::route::DisabledReason) -> String {
        use crate::route::DisabledReason;
        match reason {
            DisabledReason::NotAnalyzed => crate::i18n::msg::widgets_tracks_enh_reason_not_analyzed(),
            DisabledReason::DolbyVisionUnusable => crate::i18n::msg::widgets_tracks_enh_reason_dv_unusable(),
            DisabledReason::DolbyVisionSubtitle => crate::i18n::msg::widgets_tracks_enh_reason_dv_subtitle(),
            DisabledReason::NotOriginalQuality => crate::i18n::msg::widgets_tracks_enh_reason_quality(),
            DisabledReason::ServerRefused => crate::i18n::msg::widgets_tracks_enh_reason_refused(),
        }
        .to_string()
    }

    /// The plain-language consequence note an `Offered` pair carries (M7) — `None` for the
    /// ordinary case (no subtitle on screen), since there is nothing to say.
    fn enh_note_text(
        route: crate::route::EnhancementRoute,
        subtitle_effect: crate::route::SubtitleEffect,
    ) -> Option<String> {
        use crate::route::{EnhancementRoute, SubtitleEffect};
        match route {
            EnhancementRoute::Burn => Some(crate::i18n::msg::widgets_tracks_enh_note_burn().to_string()),
            EnhancementRoute::RemuxDropsDolbyVision => {
                Some(crate::i18n::msg::widgets_tracks_enh_note_dv_off().to_string())
            }
            EnhancementRoute::Remux if subtitle_effect == SubtitleEffect::Sidecar => {
                Some(crate::i18n::msg::widgets_tracks_enh_note_sidecar().to_string())
            }
            EnhancementRoute::Remux => None,
        }
    }

    /// The Audio tab's form: the track list, plus — whenever [`Self::enhance_shown`] is `Some` OR
    /// [`Self::enhance_disabled`] is `Some` (every gate but no Plex Pass is now a visible reason,
    /// owner direction 2026-09-29) — a second, headerless section carrying the two Plex Pass DSP
    /// toggles (enabled, or disabled: dim, focusable, inert, with their reason), the same "own
    /// section, no header" idiom the Subtitles tab's Timing/Style pair uses, plus an optional
    /// non-selectable note row. Each track is keyed by its index in the item's audio list.
    fn audio_form(&self, meta: metadata::MetadataView<'_>) -> TrackForm {
        let mut sec = FormSection::new(crate::i18n::msg::widgets_tracks_audio());
        let Some(d) = tracks(meta) else {
            return Form::new().section(sec);
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
            sec = sec.item(TrackRowId::AudioTrack(i), RowKind::Choice, (), row);
        }
        let form = Form::new().section(sec);
        let boost = crate::i18n::msg::widgets_tracks_boost_dialog();
        let loudness = crate::i18n::msg::widgets_tracks_normalize_loudness();
        if let Some(shown) = self.enhance_shown {
            let mut enh = FormSection::new("")
                .item(TrackRowId::Boost, RowKind::Toggle, (), Row::new(boost).toggle(shown.boost_dialog))
                .item(TrackRowId::Loudness, RowKind::Toggle, (), Row::new(loudness).toggle(shown.normalize_loudness));
            if let Some(note) = self
                .enhance_route
                .and_then(|route| Self::enh_note_text(route, self.enhance_subtitle_effect))
            {
                enh = enh.note(note);
            }
            form.section(enh)
        } else if let Some(reason) = self.enhance_disabled {
            // Every gate but "no Plex Pass" is now a visible reason (owner direction, 2026-09-29):
            // the rows stay in the list, dim and reading Off, with a one-line non-selectable
            // footnote naming why. "No Plex Pass" is the ONE absence that stays a silent gap (I1/I2).
            form.section(
                FormSection::new("")
                    .item(TrackRowId::Boost, RowKind::Toggle, (), Row::new(boost).toggle(false))
                    .disabled(true)
                    .item(TrackRowId::Loudness, RowKind::Toggle, (), Row::new(loudness).toggle(false))
                    .disabled(true)
                    .note(Self::enh_reason_text(reason)),
            )
        } else {
            form
        }
    }

    /// Build the Subtitles tab's form from the CURRENT state — the one place the model
    /// (`metadata::sub_layout::sub_sections`) is asked, so what a row IS can never disagree with
    /// what was drawn. Also writes [`Self::sub_style_locked`] (M7 follow-up).
    fn sub_form(&mut self, ps: &crate::route::PlaybackSession, meta: metadata::MetadataView<'_>) -> TrackForm {
        let item = tracks(meta);
        let subs: &[metadata::Stream] = item.map(|t| t.subs.as_slice()).unwrap_or(&[]);
        let offered = visible_subs(ps, meta);
        let names = crate::player::SHARED.track_names.lock().unwrap();
        // M7 follow-up: while the live route is actually burning a subtitle in, Timing and Style
        // stay drawn (dim, with a reason) instead of being omitted the way an ordinary
        // non-enhancement transcode omits both — a viewer who turned the enhancement on must still
        // see why the control they had is gone, not just find it missing.
        let locked = crate::route::live_is_own_burn(ps);
        self.sub_style_locked = locked;
        let sig = self.sub_sig_for(ps, meta, self.active_sub);
        self.renderer = sig.renderer;
        self.sub_sig = Some(sig);
        let show_timing = !crate::route::is_transcoding(ps) || locked;
        let model = sub_layout::sub_sections(subs, &offered, &names, &self.yours, show_timing);
        sub_form(&model, self.active_sub, self.offset_ms, locked)
    }

    /// The [`TrackRowId::Boost`]/[`TrackRowId::Loudness`] pair's three inputs, read fresh
    /// off the live route in one place — [`Self::rebuild`] and [`Self::update`] both need exactly
    /// this triple, and computing it once here keeps them from independently re-deriving it (and
    /// risking disagreement).
    fn enh_state(
        ps: &crate::route::PlaybackSession,
    ) -> (
        Option<crate::plex::AudioEnhancements>,
        Option<crate::route::EnhancementRoute>,
        Option<crate::route::DisabledReason>,
        crate::route::SubtitleEffect,
    ) {
        use crate::route::EnhancementAvailability;
        let subtitle_effect = crate::route::live_subtitle_effect(ps);
        match crate::route::menu_enhancement_availability(ps) {
            EnhancementAvailability::Hidden => (None, None, None, subtitle_effect),
            EnhancementAvailability::Offered(route) => (
                Some(crate::route::displayed_audio_enhancements(ps)),
                Some(route),
                None,
                subtitle_effect,
            ),
            EnhancementAvailability::Disabled(reason) => (None, None, Some(reason), subtitle_effect),
        }
    }

    /// Install `tab`'s form for an OPEN or a tab switch: the pill snaps, the scroll returns to the
    /// top and focus lands on the ACTIVE track (Subtitles: the active track, or Off; Audio: the
    /// active audio track) — the explicit initial id, not whatever row a previous tab left.
    fn rebuild(&mut self, ps: &crate::route::PlaybackSession, meta: metadata::MetadataView<'_>, tab: c_int) {
        if tab == 0 {
            let (shown, route, disabled, subtitle_effect) = Self::enh_state(ps);
            self.set_enhancement(shown, route, disabled, subtitle_effect);
            self.sticky_audio_target = None; // a fresh open has no banked row to return to
            let form = self.audio_form(meta);
            self.form.open(form, Some(&self.active_audio_id()));
        } else {
            let form = self.sub_form(ps, meta);
            self.form.open(form, Some(&self.active_sub_id()));
        }
        // an open or a tab switch always lands on a tab's root
        self.pages.clear();
        self.form.table.set_title(None);
    }

    /// The Audio row carrying the checkmark.
    fn active_audio_id(&self) -> TrackRowId {
        TrackRowId::AudioTrack(self.active_audio().max(0) as usize)
    }

    fn set_enhancement(
        &mut self,
        shown: Option<crate::plex::AudioEnhancements>,
        route: Option<crate::route::EnhancementRoute>,
        disabled: Option<crate::route::DisabledReason>,
        subtitle_effect: crate::route::SubtitleEffect,
    ) {
        self.enhance_shown = shown;
        self.enhance_route = route;
        self.enhance_disabled = disabled;
        self.enhance_subtitle_effect = subtitle_effect;
    }

    /// The Audio tab's LIVE half of the rebuild, taking the offer/displayed answer rather than
    /// recomputing it — `update`'s per-frame poll already has it fresh, and handing it here keeps
    /// [`Self::enh_state`] to exactly one call per rebuild instead of two.
    ///
    /// **Preserves the focused row by its [`TrackRowId`]** ([`FormTable::refresh_with`]) rather
    /// than snapping to the checked track — the fix for a reported focus desync: `update`'s live
    /// poll calls this every time the route's enhancement answer changes (the server settling an
    /// optimistic Boost/Loudness flip, or a mid-play route change), and that used to re-home the
    /// pill onto the active-audio row unconditionally while the ENGINE's focus stayed on the toggle
    /// row the viewer was actually on, so the next UP/DOWN/OK acted on a row nothing showed as
    /// selected. The table keeps its scroll, the pill slides to wherever the id now sits, and when
    /// the form's landing differs from the engine's key the form asks the engine to follow
    /// ([`FormTable::reseat`]). The checked track is the fallback when the viewer's row is gone.
    fn refresh_audio(
        &mut self,
        enhance_shown: Option<crate::plex::AudioEnhancements>,
        enhance_route: Option<crate::route::EnhancementRoute>,
        enhance_disabled: Option<crate::route::DisabledReason>,
        enhance_subtitle_effect: crate::route::SubtitleEffect,
        meta: metadata::MetadataView<'_>,
    ) {
        // The offer is about to VANISH (Some -> None): bank the toggle row identity before the
        // rebuild drops it, so a later return restores it — see `Self::sticky_audio_target`.
        if self.enhance_shown.is_some() && enhance_shown.is_none() {
            if let Some(t @ (TrackRowId::Boost | TrackRowId::Loudness)) = self.form.selected_id().copied() {
                self.sticky_audio_target = Some(t);
            }
        }
        self.set_enhancement(enhance_shown, enhance_route, enhance_disabled, enhance_subtitle_effect);
        let form = self.audio_form(meta);
        // The offer is back: prefer the banked identity over the id the table happens to sit on
        // (a neighbouring track, where the form landed while the rows were gone).
        let restored = if enhance_shown.is_some() { self.sticky_audio_target.take() } else { None };
        self.form.refresh_with(form, restored.as_ref(), Some(&self.active_audio_id()));
    }

    /// The panel geometry — shared by `update` and `draw` so scrolling math matches.
    fn panel_rect(&self, measure: &dyn crate::ui::machine::Measure) -> Rect {
        // Each tab hugs its own rows (shared menu rule); the right edge is fixed, so switching
        // tabs moves only the left edge.
        let pw = self.form.table.menu_panel_width(measure);
        // the transport control row's own right edge — one number for the discs and both panels
        let px = crate::ui::player_hud::CTRL_RIGHT - pw;
        // Bottom-anchored just above the control-button row (buttons top at SCR_H-288) with a clear gap.
        // The panel grows UPWARD from this fixed bottom edge, and its height is capped so the top never
        // crosses `top_min` — so a long list (an item with many audio dubs) SCROLLS inside the panel
        // instead of the panel itself spilling down over the buttons. Switching Audio↔Subtitles keeps
        // the bottom edge steady.
        let bottom = SCR_H - 316.0; // 764 — ~28px above the buttons
        let ph = self.panel_h(measure, pw);
        let py = bottom - ph; // ≥ top_min by construction
        Rect::new(px, py, pw, ph)
    }

    /// The panel's height at width `pw`. A note row wraps, so its line count depends on the
    /// width: it is resolved HERE, against the same `measure` and `pw` the panel is sized with,
    /// so no caller (`update`, hit-testing, `draw`) can read the count a rebuild left stale.
    /// Idempotent and a few short strings per call.
    fn panel_h(&self, measure: &dyn crate::ui::machine::Measure, pw: f32) -> f32 {
        self.form.table.fit_notes(pw, measure);
        let (bottom, top_min) = (SCR_H - 316.0, 60.0);
        self.form.table.measured_height().clamp(160.0, bottom - top_min)
    }

    /// `ps`/`meta` are read only for the Audio tab, and only to notice a LIVE change: a request
    /// this menu itself fired settles asynchronously (the server's `EnhancementOutcome`, or a
    /// mid-play route change moving the family in or out of `Remux`), and the two rows must
    /// track that the moment it lands rather than freeze at whatever `on_ok`/`rebuild` last drew
    /// — otherwise a refusal leaves a row reading "On" for a preference the route already gave up
    /// on. `rebuild`'s own recomputation of `enhance_shown` is the single source of truth here
    /// too, so this only ever asks "did that answer change since last frame", never rebuilds it a
    /// second, divergent way.
    pub(crate) fn update(
        &mut self,
        dt: f32,
        measure: &dyn crate::ui::machine::Measure,
        ps: &crate::route::PlaybackSession,
        meta: metadata::MetadataView<'_>,
    ) {
        if self.tab == 0 {
            let (shown, route, disabled, subtitle_effect) = Self::enh_state(ps);
            if shown != self.enhance_shown
                || route != self.enhance_route
                || disabled != self.enhance_disabled
                || subtitle_effect != self.enhance_subtitle_effect
            {
                self.refresh_audio(shown, route, disabled, subtitle_effect, meta);
            }
        } else {
            self.poll_subtitle_state(ps, meta);
        }
        // `update` subtracts its own top/bottom padding now — pass the panel's raw height.
        let h = self.panel_h(measure, self.form.table.menu_panel_width(measure));
        self.form.table.update(dt, h);
    }

    pub(crate) fn draw(&mut self, appear: f32, measure: &dyn crate::ui::machine::Measure) {
        // The appear fade/rise — the container drives the phase and the appear spring. The dim
        // over the video plane is the container's too (`PlayerOverlayScreen::scrim`,
        // `theme::underlay::DIM_PLAYER`), painted at the end of the player's page pass.
        let p = Painter::root()
            .alpha(appear)
            .translate(0.0, Popover::RISE * (1.0 - appear));
        let r = self.panel_rect(measure);

        // frosted panel card — near-opaque dark (no true backdrop blur on the GLES plane, so a solid
        // dark card approximates it); only a hint of video shows through
        p.rect(r, 28.0, theme::PANEL_TOP, theme::PANEL_BOT, 0.0);

        self.form.table.draw(p, r, measure);
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

impl TrackMenuPart<'_> {
    /// The row-index element the geometry below understands for focus element `e`; `None` when
    /// the active tab's form does not know the key (the other tab's, a row that left). Mirrors
    /// `ui::table_screen::TablePart`'s translation: the element is a [`RowKey`]'s number.
    fn to_index<E: IndexElem>(&self, e: &E) -> Option<usize> {
        self.state.form.index_of_key(RowKey(e.index()?))
    }

    /// The focus element for row index `i` (its [`RowKey`]).
    fn to_key<E: IndexElem>(&self, i: usize) -> Option<E> {
        self.state.form.key_at(i).map(|k| E::of_index(k.0))
    }
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
            extent: self.state.panel_rect(_cx.measure),
            len: self.state.form.table.n_rows().max(0) as usize,
            elem: ElemKind::Bare,
        });
    }
    fn group_of(&self, key: &H::Elem, _cx: &Cx<'_, H>) -> Option<GroupId> {
        self.to_index(key).map(|_| self.group)
    }
    fn neighbour(&self, key: FocusKey<H::Elem>, dir: Dir, _cx: &Cx<'_, H>) -> Step<H::Elem> {
        let Some(i) = self.to_index(&key.elem) else {
            return Step::Edge;
        };
        let delta = match dir {
            Dir::Up => -1,
            Dir::Down => 1,
            _ => return Step::Edge, // Left/Right: the screen's own tab switch, via `EdgeRule::Screen`
        };
        match self
            .state
            .form
            .table
            .next_selectable(i as i32, delta)
            .and_then(|j| self.to_key::<H::Elem>(j as usize))
        {
            Some(elem) => Step::Move(FocusKey { entry: self.entry, elem }),
            None => Step::Edge,
        }
    }
    fn place(&self, key: &H::Elem, cx: &Cx<'_, H>, _at: At) -> Option<Placed> {
        // the title band's pointer-only key: not a row, but replay and hit validation must be able
        // to place it (`docs/player-submenus.md`)
        if key.index() == Some(TITLE_KEY) {
            let r = self.state.form.table.title_rect(self.state.panel_rect(cx.measure))?;
            return Some(Placed { rect: r, rest_rect: r, clip: self.state.panel_rect(cx.measure), index: None });
        }
        let i = self.to_index(key)?;
        let r = self.state.form.table.row_frame(self.state.panel_rect(cx.measure), i as i32)?;
        Some(Placed {
            rect: r,
            rest_rect: r,
            clip: self.state.panel_rect(cx.measure),
            index: Some(i as u32),
        })
    }
    fn reconcile(&self, want: FocusKey<H::Elem>, _cx: &Cx<'_, H>) -> FocusKey<H::Elem> {
        // A rebuild's identity landing wins over the engine's key (`FormTable::reseat`: a live
        // poll moved the selection to a row the engine is not on, e.g. the enhancement pair
        // vanishing under a focused toggle); else the engine's key if the form still knows it;
        // else the table's OWN cursor, never row 0 and never a clamp of a stale index. `table.sel`
        // is where `open`/`refresh_with` decided focus belongs.
        let form = &self.state.form;
        let at = RowKeys::reseat(form)
            .and_then(|r| form.index_of_key(r))
            .or_else(|| self.to_index(&want.elem))
            .unwrap_or_else(|| form.table.sel.max(0) as usize);
        let settled = form.table.settle(at as i32).max(0) as usize;
        FocusKey {
            entry: self.entry,
            elem: self.to_key(settled).unwrap_or(want.elem),
        }
    }
    fn seat(&self, _g: GroupId, _from: Placed, _cx: &Cx<'_, H>) -> FocusKey<H::Elem> {
        let form = &self.state.form;
        FocusKey {
            entry: self.entry,
            elem: self
                .to_key(form.table.sel.max(0) as usize)
                .unwrap_or_else(|| H::Elem::of_index(0)),
        }
    }
}

impl<H: Host> Part<H> for TrackMenuPart<'_>
where
    H::Elem: IndexElem,
{
    fn prepare(&mut self, _b: &mut Budget, _cx: &Cx<'_, H>) {}
    /// Registers every visible row's stop (§7.6), keyed by the row's [`RowKey`]; the panel's own
    /// paint happens directly on the owned `TrackMenuState` from `PlayerOverlayScreen::draw` (see
    /// the struct doc above).
    fn draw(&mut self, f: &mut DrawFrame<'_, '_, H>, _rect: Rect) {
        let p = Painter::root();
        let r = self.state.panel_rect(f.measure);
        let table = &self.state.form.table;
        for i in 0..table.n_rows() {
            if table.next_selectable(i, 0) != Some(i) {
                continue;
            }
            let (Some(row), Some(elem)) = (table.row_frame(r, i), self.to_key::<H::Elem>(i as usize)) else {
                continue;
            };
            f.stop(
                p,
                Stop {
                    key: FocusKey { entry: self.entry, elem },
                    rect: row,
                    rest_rect: row,
                    clip: r,
                    hover: Hover::Focus,
                    activate: Activate::Direct,
                },
            );
        }
        // "< STYLE": a click pops. A pointer-only stop — no hover focus, never in the D-pad column.
        if let Some(band) = table.title_rect(r) {
            f.stop(
                p,
                Stop {
                    key: FocusKey { entry: self.entry, elem: H::Elem::of_index(TITLE_KEY) },
                    rect: band,
                    rest_rect: band,
                    clip: r,
                    hover: Hover::Ignore,
                    activate: Activate::Direct,
                },
            );
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

/// The localized name of a caption size rung — the one place both the player's Style pages and
/// Settings' Playback read it.
pub(crate) fn subtitle_size_label(size: SubtitleSize) -> &'static str {
    use crate::i18n::msg;
    match size {
        SubtitleSize::Small => msg::settings_playback_subtitle_size_small(),
        SubtitleSize::Medium => msg::settings_playback_subtitle_size_medium(),
        SubtitleSize::Large => msg::settings_playback_subtitle_size_large(),
        SubtitleSize::ExtraLarge => msg::settings_playback_subtitle_size_extra_large(),
    }
}

/// The localized name of a caption position rung, shared like [`subtitle_size_label`].
pub(crate) fn subtitle_position_label(position: SubtitlePosition) -> &'static str {
    use crate::i18n::msg;
    match position {
        SubtitlePosition::Low => msg::settings_playback_subtitle_position_low(),
        SubtitlePosition::Middle => msg::settings_playback_subtitle_position_middle(),
        SubtitlePosition::High => msg::settings_playback_subtitle_position_high(),
    }
}

/// An offset as localized signed seconds to the tenth (`ui::timing_capsule::offset_seconds_in`,
/// the one offset formatter).
fn format_offset(ms: i64) -> String {
    crate::ui::timing_capsule::offset_seconds_in(ms, true, crate::i18n::current())
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

/// **Declare the Subtitles root** from its model (`metadata::sub_layout::sub_sections`) as a keyed
/// form — the catalog words for each header, the checkmark on `active_sub` (-1 for Off), and the
/// Timing read-out (`offset_ms`). Each row is declared once: a track carries its subs-list index
/// as its id and key; Timing is dim while subtitles are Off and hands off to the capsule (so it has
/// no chevron); Style is the one Nav row, the drill-in to [`TrackPage::Style`].
///
/// `locked` (M7 follow-up): while the live route is actually burning a subtitle into the picture,
/// Timing and Style do nothing — the text is already in the pixels — so they are DISABLED (dim,
/// focusable so the viewer can read why, inert under OK and RIGHT at the form layer) and one
/// non-selectable [`FormSection::note`] naming why follows Style, the same "visible, dim, plain
/// reason" idiom the Audio tab's own Boost dialog / Normalize loudness rows use when THEY are
/// disabled.
fn sub_form(model: &[SubSection], active_sub: c_int, offset_ms: i64, locked: bool) -> TrackForm {
    use crate::i18n::msg;
    model.iter().fold(Form::new(), |form, sec| {
        let head = match &sec.header {
            SubHeader::Subtitles => Section::new(msg::widgets_tracks_subtitles()),
            SubHeader::Language { name, tracks } => {
                Section::new(name.clone()).accessory(msg::widgets_tracks_count(*tracks as i64))
            }
            SubHeader::Bare => Section::new(""),
            SubHeader::OtherLanguages { languages } => Section::new(msg::widgets_tracks_other_languages())
                .accessory(msg::widgets_tracks_language_count(*languages as i64)),
        };
        let out = sec.rows.iter().fold(FormSection::from_head(head), |out, row| match row {
            SubRow::Off => out.item(
                TrackRowId::Off,
                RowKind::Choice,
                (),
                Row::new(msg::widgets_tracks_off()).checked(active_sub < 0),
            ),
            SubRow::Flat(t) => out.item(TrackRowId::SubTrack(t.i), RowKind::Choice, (), flat_row(t, active_sub)),
            SubRow::InLanguage(t) => {
                out.item(TrackRowId::SubTrack(t.i), RowKind::Choice, (), in_lang_row(t, active_sub))
            }
            SubRow::Timing => out
                .item(
                    TrackRowId::Timing,
                    RowKind::Button,
                    (),
                    Row::new(msg::widgets_tracks_timing()).value(format_offset(offset_ms)).dim(active_sub < 0),
                )
                .disabled(locked),
            SubRow::Style => {
                let out = out
                    .item(TrackRowId::Style, RowKind::Nav(TrackPage::Style), (), Row::new(msg::widgets_tracks_style()))
                    .disabled(locked);
                if locked {
                    out.note(msg::widgets_tracks_style_locked_note())
                } else {
                    out
                }
            }
        });
        form.section(out)
    })
}

/// The panel at its WIDEST ([`crate::ui::table::MENU_MAX_W`], the shared cap — either tab may hug
/// up to it) and TALLEST, for the overscan audit ([`crate::ui::consts::SAFE`]) — the full
/// `top_min`→`bottom` span, since the measured height comes from a `TableView` no host test can
/// measure.
#[cfg(test)]
pub(crate) fn overscan_rects(out: &mut Vec<(&'static str, Rect)>) {
    let (bottom, top_min) = (SCR_H - 316.0, 60.0);
    let pw = crate::ui::table::MENU_MAX_W;
    out.push((
        "track menu panel (widest)",
        Rect::new(crate::ui::player_hud::CTRL_RIGHT - pw, top_min, pw, bottom - top_min),
    ));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::table::TableView;
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
    ) -> (Vec<Section>, Vec<Option<TrackRowId>>) {
        let yours: Vec<String> = yours.iter().map(|y| y.to_string()).collect();
        let model = sub_layout::sub_sections(subs, offered, names, &yours, show_timing);
        let mut table = TrackTable::new(BAND_BASE);
        table.set(sub_form(&model, active_sub, offset_ms, false), None);
        let ids = (0..table.table.n_rows().max(0) as usize).map(|i| table.id_at(i).copied()).collect();
        (std::mem::take(&mut table.table.sections), ids)
    }

    /// A store with `subs` installed as the playing item's subtitle list. `pub(super)`:
    /// `enhancement_menu_tests` below reuses it for the Subtitles tab under a live Burn (M7).
    pub(super) fn store_with(subs: Vec<metadata::Stream>) -> crate::stores::metadata::MetadataStore {
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

    pub(super) fn stream(id: i64, index: i64, lang: &str, lang_code: &str, title: &str) -> metadata::Stream {
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
        let (sections, targets) = sub_layout(&subs, &[0], &names, &["spa"], -1, true, 0);
        assert_eq!(sections[0].header, "Subtitles");
        assert_eq!(sections[0].rows.len(), 2, "Off + the one track");
        assert_eq!(sections[0].rows[1].label, "Spanish");
        assert_eq!(targets[1], Some(TrackRowId::SubTrack(0)));
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
            sub_layout(&subs, &offered, &names, &["rus"], -1, true, 0);
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
            sub_layout(&subs, &offered, &names, &["rus"], -1, true, 0);
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
        let (sections, _) = sub_layout(&subs, &[0], &names, &[], -1, true, 0);
        assert!(matches!(sections.last().unwrap().rows[0].badges.as_slice(), [Badge::Sdh]));

        let subs = vec![mk(false, true, "srt")];
        let (sections, _) = sub_layout(&subs, &[0], &names, &[], -1, true, 0);
        assert!(matches!(sections.last().unwrap().rows[0].badges.as_slice(), [Badge::Text(t)] if t == "EXTERNAL"));

        let subs = vec![mk(true, true, "srt")];
        let (sections, _) = sub_layout(&subs, &[0], &names, &[], -1, true, 0);
        assert!(
            matches!(sections.last().unwrap().rows[0].badges.as_slice(), [Badge::Sdh]),
            "SDH beats EXTERNAL"
        );

        let subs = vec![mk(false, false, "pgs")];
        let (sections, _) = sub_layout(&subs, &[0], &names, &[], -1, true, 0);
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
            sub_layout(&subs, &offered, &names, &[], -1, true, 0);
        let other = sections.last().unwrap();
        assert_eq!(other.header, "Other languages");
        assert_eq!(other.accessory, "2 languages");
        let labels: Vec<&str> = other.rows.iter().map(|r| r.label.as_str()).collect();
        assert_eq!(labels, ["Arabic", "German"], "sorted by language name");

        let subs = vec![stream(1, 0, "German", "deu", "")];
        let (sections, _targets) = sub_layout(&subs, &[0], &names, &[], -1, true, 0);
        assert_eq!(sections.last().unwrap().accessory, "1 language");
    }

    /// **Every Subtitles-panel row fits the panel in every shipped language** — the grouped
    /// layout's section words, the kind fallbacks, the "Track N" ordinal and a region name beside
    /// each badge, against [`MENU_MAX_W`](crate::ui::table::MENU_MAX_W), measured with the device's whole-pixel advances. (A source is
    /// server text and may elide; the fixture's sources are short so only app text is judged.)
    #[test]
    fn every_subtitles_row_fits_the_panel_in_every_language() {
        use crate::i18n::{language_on_this_thread_for_test, SHIPPED};
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
        for language in SHIPPED {
            let _guard = language_on_this_thread_for_test(language);
            let (sections, _) =
                sub_layout(&subs, &offered, &names, &["rus"], 1, true, -60_000);
            let mut table = TableView::new();
            table.set_sections(sections, 0, false);
            out.extend(table.menu_cap_failure(&crate::fontcov::advances::ShippedMeasure, language.tag()));
            out.extend(table.app_fit_failures(crate::ui::table::MENU_MAX_W, language.tag()));
            out.extend(table.app_fit_failures_hugged(language.tag()));
        }
        crate::ui::table::assert_no_fit_failures(&out);
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
            sub_layout(&subs, &offered, &TrackNames::new(), &["rus"], 1, true, 300);
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

        let (sections, _) = sub_layout(&subs, &[0], &names, &[], -1, false, 0);
        assert!(
            sections.iter().flat_map(|s| &s.rows).all(|r| r.label != "Timing"),
            "a transcode burns captions; no client offset can reach them"
        );

        let (sections, _) = sub_layout(&subs, &[0], &names, &[], -1, true, 0);
        let timing = sections
            .iter()
            .flat_map(|s| &s.rows)
            .find(|r| r.label == "Timing")
            .expect("Timing row");
        assert!(timing.dim, "subtitles are Off");

        let (sections, _) = sub_layout(&subs, &[0], &names, &[], 0, true, 0);
        let timing = sections
            .iter()
            .flat_map(|s| &s.rows)
            .find(|r| r.label == "Timing")
            .expect("Timing row");
        assert!(!timing.dim);
    }

    // ---- track_menu: the targets mapping -------------------------------------------------------

    #[test]
    fn targets_map_flat_rows_to_off_sub_timing_and_style_in_drawn_order() {
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
            menu.row_ids(),
            vec![Some(TrackRowId::Off), Some(TrackRowId::Timing), Some(TrackRowId::Style), Some(TrackRowId::SubTrack(0))]
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
        menu.rebuild(&ps, store.view(), 1);
        assert_eq!(menu.selected_id(), Some(TrackRowId::SubTrack(1)));
    }

    /// `row_for_sub_target("track:N")` names the N-th TRACK row, never Off/Timing/Style: the
    /// `subtitle_text_srt` manifest case once hard-coded row 3, which stopped being a track when
    /// the panel's layout changed (it resolved to Color and committed nothing).
    #[test]
    fn row_for_sub_target_finds_track_rows_and_skips_off_timing_and_color() {
        let _g = crate::testlock::serial();
        crate::player::sidecar::reset();
        let ps = crate::route::PlaybackSession::IDLE;
        let store = store_with(vec![
            stream(1, 0, "English", "eng", "A"),
            stream(2, 1, "French", "fra", "B"),
        ]);
        let menu = TrackMenuState::new(&ps, store.view(), 1, vec![]);
        let row_of = |t: TrackRowId| menu.form.index_of(&t).map(|r| r as c_int);
        assert_eq!(menu.row_for_sub_target("track:0"), row_of(TrackRowId::SubTrack(0)));
        assert_eq!(menu.row_for_sub_target("track:1"), row_of(TrackRowId::SubTrack(1)));
        assert!(menu.row_for_sub_target("track:0").unwrap() >= 1, "row 0 is Off");
        assert_eq!(menu.row_for_sub_target("track:2"), None, "past the last track");
        assert_eq!(menu.row_for_sub_target("boost"), None);
        assert_eq!(menu.row_for_sub_target("track:x"), None);
    }

    // ---- track_menu: sidecar, tone and Timing rows dispatch to their own outcomes -------------

    /// **A Subtitles-panel row is a track, Style, or Timing, never ambiguous — and the split is
    /// by `targets[sel]`.** Off + an embedded English track + an external French sidecar (none of
    /// them "yours", so all three land flat under "Subtitles"/"Other languages" respectively),
    /// then the headerless Timing/Style section.
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
        // Off(0), Timing(1), Style(2), English(3), French sidecar(4) — "Other languages" sorts
        // English before French, and the settings section always precedes it.
        assert_eq!(
            menu.row_ids(),
            vec![
                Some(TrackRowId::Off),
                Some(TrackRowId::Timing),
                Some(TrackRowId::Style),
                Some(TrackRowId::SubTrack(0)),
                Some(TrackRowId::SubTrack(1)),
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

        menu.focus_row(1);
        assert_eq!(
            menu.on_ok(store.view()),
            TrackOk::OpenTiming,
            "the sidecar is now the active subtitle, so Timing is no longer inert"
        );

        // Style -> Color -> the second tone: a Nav row pushes, a choice commits and the panel stays
        menu.focus_row(2);
        assert_eq!(menu.on_ok(store.view()), TrackOk::Navigated);
        assert_eq!(menu.on_ok(store.view()), TrackOk::Navigated, "Size is first and live for a text subtitle");
        assert_eq!(menu.page_path(), [TrackPage::Style, TrackPage::Picker(StyleField::Size)]);
        let ps = crate::route::PlaybackSession::IDLE;
        assert!(menu.pop(&ps, store.view()));
        menu.focus_key(TrackRowId::OpenField(StyleField::Color).key().0);
        assert_eq!(menu.on_ok(store.view()), TrackOk::Navigated);
        menu.focus_key(TrackRowId::Choice(StyleField::Color, 1).key().0);
        assert_eq!(
            menu.on_ok(store.view()),
            TrackOk::Commit { commit: TrackCommit::SubtitleTone(SubtitleTone::LADDER[1]), keep_open: true }
        );
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
        let timing_row = menu.form.index_of(&TrackRowId::Timing)
            .expect("Timing row");

        menu.focus_row(timing_row as c_int);
        assert_eq!(menu.on_ok(store.view()), TrackOk::Inert, "subtitles are Off: inert");

        let sub_row = (0..menu.form.table.n_rows() as usize).position(|i| matches!(menu.form.id_at(i), Some(TrackRowId::SubTrack(_))))
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
        assert_eq!(format_offset(-60_000), "-60.0 s");
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

    /// The Audio tab's row ids, with no enhancement offered: one [`TrackRowId::AudioTrack`]
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
            let want: Vec<Option<TrackRowId>> = (0..n).map(|i| Some(TrackRowId::AudioTrack(i))).collect();
            assert_eq!(menu.row_ids(), want, "n={n}");
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

    /// One playing audio track — enough for `tracks(meta)` to be `Some` so `audio_form` does not
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
        assert_eq!(menu.form.table.sections.len(), 1, "track list only — no second section at all");
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

    /// M7: a known-but-not-yet-analyzed (or definitively incapable) carried track reads the same to
    /// a viewer either way — Disabled(NotAnalyzed), not Hidden (owner direction, 2026-09-29).
    #[test]
    fn enh_rows_disabled_incapable_track() {
        let _g = crate::testlock::serial();
        let (menu, ps) = audio_tab(EnhTestFixture { carried_capable: Some(false), ..Default::default() });
        assert_eq!(menu.enhance_disabled, Some(crate::route::DisabledReason::NotAnalyzed));
        let note = menu.form.table.sections[1].rows.last().unwrap();
        assert_eq!(note.label, crate::i18n::msg::widgets_tracks_enh_reason_not_analyzed());
        teardown(&ps);
    }

    /// M7: a usable-base-layer Dolby Vision source no longer hides the rows — the enhanced remux
    /// simply never declares DV (`fill_direct_plan` is the only place that ever does), so turning
    /// the toggle on plays the HDR10 base picture instead. The note says so in plain language.
    #[test]
    fn enh_rows_offered_dv_drops_declaration() {
        let _g = crate::testlock::serial();
        let (menu, ps) = audio_tab(EnhTestFixture { dv_declared: true, ..Default::default() });
        assert_eq!(menu.enhance_route, Some(crate::route::EnhancementRoute::RemuxDropsDolbyVision));
        assert!(menu.enhance_shown.is_some());
        let note = menu.form.table.sections[1].rows.last().unwrap();
        assert_eq!(note.label, crate::i18n::msg::widgets_tracks_enh_note_dv_off());
        teardown(&ps);
    }

    /// P5 (or P7 with an enhancement layer): no copy of the base layer is ever correct, so there is
    /// nothing the enhancement's remux could decorate — Disabled, not Hidden (owner direction,
    /// 2026-09-29).
    #[test]
    fn enh_rows_disabled_dv_unusable_base() {
        let _g = crate::testlock::serial();
        let (menu, ps) = audio_tab(EnhTestFixture { dv_base_unusable: true, ..Default::default() });
        assert_eq!(menu.enhance_disabled, Some(crate::route::DisabledReason::DolbyVisionUnusable));
        assert_eq!(menu.enhance_shown, None);
        let note = menu.form.table.sections[1].rows.last().unwrap();
        assert_eq!(note.label, crate::i18n::msg::widgets_tracks_enh_reason_dv_unusable());
        teardown(&ps);
    }

    /// A declared DV source with an embedded subtitle on screen: PMS measured copying the video
    /// regardless of a burn request and silently dropping the subtitle — Disabled, telling the
    /// viewer to turn subtitles off to use it.
    #[test]
    fn enh_rows_disabled_dv_with_embedded_subtitle() {
        let _g = crate::testlock::serial();
        let (menu, ps) = audio_tab(EnhTestFixture {
            dv_declared: true,
            subtitle_effect: crate::route::SubtitleEffect::Embedded,
            ..Default::default()
        });
        assert_eq!(menu.enhance_disabled, Some(crate::route::DisabledReason::DolbyVisionSubtitle));
        let note = menu.form.table.sections[1].rows.last().unwrap();
        assert_eq!(note.label, crate::i18n::msg::widgets_tracks_enh_reason_dv_subtitle());
        teardown(&ps);
    }

    /// M7: an embedded subtitle no longer withdraws the offer (I6) — it routes to a forced re-
    /// encode that burns it in, and the toggle stays enabled with a plain-language note.
    #[test]
    fn enh_rows_offered_embedded_subtitle_burns() {
        let _g = crate::testlock::serial();
        let (menu, ps) = audio_tab(EnhTestFixture {
            subtitle_effect: crate::route::SubtitleEffect::Embedded,
            ..Default::default()
        });
        assert_eq!(menu.enhance_route, Some(crate::route::EnhancementRoute::Burn));
        assert!(menu.enhance_shown.is_some());
        let note = menu.form.table.sections[1].rows.last().unwrap();
        assert_eq!(note.label, crate::i18n::msg::widgets_tracks_enh_note_burn());
        teardown(&ps);
    }

    /// An external (sidecar) subtitle the client draws itself is unaffected by the enhancement —
    /// still an ordinary remux, with a reassuring note.
    #[test]
    fn enh_rows_offered_sidecar_subtitle_unaffected() {
        let _g = crate::testlock::serial();
        let (menu, ps) = audio_tab(EnhTestFixture {
            subtitle_effect: crate::route::SubtitleEffect::Sidecar,
            ..Default::default()
        });
        assert_eq!(menu.enhance_route, Some(crate::route::EnhancementRoute::Remux));
        assert!(menu.enhance_shown.is_some());
        let note = menu.form.table.sections[1].rows.last().unwrap();
        assert_eq!(note.label, crate::i18n::msg::widgets_tracks_enh_note_sidecar());
        teardown(&ps);
    }

    /// I5 excludes every non-Direct/Remux shape identically (HLS, a fixed rung, a relay); one
    /// `Other`-family route stands for the group, since the predicate cannot tell them apart. M7:
    /// visible-and-dim, not hidden — "only at Original quality" is a plain reason.
    #[test]
    fn enh_rows_disabled_hls() {
        let _g = crate::testlock::serial();
        let (menu, ps) = audio_tab(EnhTestFixture { remux: Some(false), ..Default::default() });
        assert_eq!(menu.enhance_disabled, Some(crate::route::DisabledReason::NotOriginalQuality));
        let note = menu.form.table.sections[1].rows.last().unwrap();
        assert_eq!(note.label, crate::i18n::msg::widgets_tracks_enh_reason_quality());
        teardown(&ps);
    }

    #[test]
    fn enh_rows_disabled_reencode_rung() {
        let _g = crate::testlock::serial();
        let (menu, ps) = audio_tab(EnhTestFixture { remux: Some(false), ..Default::default() });
        assert_eq!(menu.enhance_disabled, Some(crate::route::DisabledReason::NotOriginalQuality));
        teardown(&ps);
    }

    #[test]
    fn enh_rows_disabled_relay() {
        let _g = crate::testlock::serial();
        let (menu, ps) = audio_tab(EnhTestFixture { remux: Some(false), ..Default::default() });
        assert_eq!(menu.enhance_disabled, Some(crate::route::DisabledReason::NotOriginalQuality));
        teardown(&ps);
    }

    /// A forced direct play (or a fixed rung/relay/non-Original MDE) never computes an
    /// `auto_original` candidate at all — `base_present: false` reproduces exactly that.
    #[test]
    fn enh_rows_disabled_forced() {
        let _g = crate::testlock::serial();
        let (menu, ps) = audio_tab(EnhTestFixture { base_present: false, ..Default::default() });
        assert_eq!(menu.enhance_disabled, Some(crate::route::DisabledReason::NotOriginalQuality));
        teardown(&ps);
    }

    #[test]
    fn enh_rows_disabled_refused() {
        let _g = crate::testlock::serial();
        let (menu, ps) = audio_tab(EnhTestFixture { refused: true, ..Default::default() });
        assert_eq!(menu.enhance_disabled, Some(crate::route::DisabledReason::ServerRefused));
        let note = menu.form.table.sections[1].rows.last().unwrap();
        assert_eq!(note.label, crate::i18n::msg::widgets_tracks_enh_reason_refused());
        teardown(&ps);
    }

    #[test]
    fn enh_rows_disabled_server_default_audio() {
        let _g = crate::testlock::serial();
        let (menu, ps) = audio_tab(EnhTestFixture { carried_capable: None, ..Default::default() });
        assert_eq!(menu.enhance_disabled, Some(crate::route::DisabledReason::NotAnalyzed));
        teardown(&ps);
    }

    // ---- present -----------------------------------------------------------------------------

    #[test]
    fn enh_rows_present_pass_capable_direct() {
        let _g = crate::testlock::serial();
        let (menu, ps) = audio_tab(EnhTestFixture { remux: None, ..Default::default() });
        assert!(menu.enhance_shown.is_some());
        assert_eq!(menu.form.table.sections.len(), 2, "track list + the headerless enhancement section");
        let enh = &menu.form.table.sections[1];
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
        let enh = &menu.form.table.sections[1];
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
        assert_eq!(menu.form.table.sections[0].rows.len(), 2, "both tracks in the track section");
        let enh = &menu.form.table.sections[1];
        assert_eq!(enh.rows.len(), 2, "the toggle rows sit in their own section, right after the tracks");
        assert_eq!(
            menu.row_ids(),
            vec![
                Some(TrackRowId::AudioTrack(0)),
                Some(TrackRowId::AudioTrack(1)),
                Some(TrackRowId::Boost),
                Some(TrackRowId::Loudness),
            ],
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
        assert_eq!(menu.form.table.sections[1].rows[0].toggle, Some(true));

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
        assert_eq!(menu.form.table.sections[1].rows[1].toggle, Some(true));

        // The SAME playback settles as Refused (I5 excludes it from the offer entirely) — a LIVE
        // change this menu never caused, delivered exactly the way `PlayerOverlayScreen`'s Tick
        // handler feeds it: a fresh `&PlaybackSession` from the host every frame, not a rebuild
        // the panel triggers itself.
        let (ps_refused, _sid2) = enhancement_test_session(EnhTestFixture { refused: true, ..Default::default() });
        let store = one_track_store();
        menu.update(0.0, &crate::ui::fixture::FixtureMeasure, &ps_refused, store.view());
        assert_eq!(
            menu.enhance_shown, None,
            "a settled refusal must drop the optimistic On reading, not leave a row reading On"
        );
        // M7: the refusal is now a plain-language `Disabled` reason, not a vanished section — the
        // rows stay, dim, reading Off, with a one-line note naming why.
        assert_eq!(menu.enhance_disabled, Some(crate::route::DisabledReason::ServerRefused));
        assert_eq!(menu.form.table.sections.len(), 2, "the headerless DSP section stays, dim, with its reason");
        assert_eq!(menu.form.table.sections[1].rows[1].toggle, Some(false));
        assert!(menu.form.table.sections[1].rows[1].dim);

        teardown(&ps_ok);
    }

    /// **A note that appears in a rebuild is sized on that same `update`.** A note's line count
    /// depends on the panel width, so it is resolved against the measure `update` now carries;
    /// before, the count a rebuild left was read by the panel height until the NEXT draw measured
    /// it, so a wrapped note's panel was one frame short.
    #[test]
    fn a_note_added_by_a_live_rebuild_sizes_the_panel_on_the_same_update() {
        use crate::ui::fixture::FixtureMeasure as M;
        let _g = crate::testlock::serial();
        let (mut menu, ps_ok) = audio_tab(EnhTestFixture {
            applied: crate::plex::AudioEnhancements { boost_dialog: false, normalize_loudness: true },
            ..Default::default()
        });
        assert!(!menu.form.table.sections.iter().flat_map(|s| s.rows.iter()).any(|r| r.is_note()), "premise: no note yet");
        let (ps_refused, _sid) = enhancement_test_session(EnhTestFixture { refused: true, ..Default::default() });
        let store = one_track_store();
        menu.update(0.0, &M, &ps_refused, store.view());
        // no draw, no panel_rect in between: read the table as `update` left it
        let after_update = menu.form.table.measured_height();
        let pw = menu.form.table.menu_panel_width(&M);
        menu.form.table.fit_notes(pw, &M);
        let note = menu.form.table.sections.iter().flat_map(|s| s.rows.iter()).find(|r| r.is_note()).expect("the refusal note");
        assert!(note.note_lines.get() >= 2, "premise: the note wraps at {pw}");
        assert_eq!(after_update, menu.form.table.measured_height(), "update left the note at a stale line count");
        teardown(&ps_ok);
    }

    /// **The reported bug.** A viewer holds the Audio tab open with the Boost dialog row FOCUSED
    /// (not necessarily checked — a toggle row is never the checked track) and presses OK; the
    /// server settles the request asynchronously, and the next frame's live poll
    /// (`TrackMenuState::update`) sees the answer change and rebuilds. Before the fix,
    /// `rebuild_audio` (now `refresh_audio`) always re-homed `table.sel` onto the checked audio track, so the drawn
    /// highlight jumped there while the ENGINE's own focus — which only moves on an actual
    /// `FocusMoved`, never fired by this poll — stayed on the toggle row: the visual cursor and the
    /// row the next OK/UP/DOWN actually acts on disagreed. `focus_key` here stands in for the
    /// engine's write-back exactly as `screens::player::overlay::PlayerOverlayScreen::step` performs
    /// it on a real `FocusMoved`, so `menu.sel()` staying put after `update` is the proof the
    /// engine's remembered element and the drawn cursor still name the same row.
    #[test]
    fn live_update_preserves_focus_on_the_toggled_row_not_the_checked_track() {
        let _g = crate::testlock::serial();
        let (mut menu, ps_before) = audio_tab(EnhTestFixture {
            applied: crate::plex::AudioEnhancements { boost_dialog: false, normalize_loudness: false },
            ..Default::default()
        });
        // Row 0 is the one audio track (checked/active); row 1 is Boost dialog. Move the ENGINE's
        // focus there the way a real UP press's `FocusMoved` write-back does.
        menu.focus_key(TrackRowId::Boost.key().0);
        assert_eq!(menu.form.id_at(1), Some(&TrackRowId::Boost), "fixture shape: row 1 is Boost");

        // The SAME playback settles Boost dialog ON — a LIVE change this menu did not itself
        // request (mirrors the server's async `EnhancementOutcome` landing), delivered the way
        // `update` is fed every frame: a fresh `&PlaybackSession`, not a rebuild the panel triggers.
        let (ps_after, _sid2) = enhancement_test_session(EnhTestFixture {
            applied: crate::plex::AudioEnhancements { boost_dialog: true, normalize_loudness: false },
            ..Default::default()
        });
        let store = one_track_store();
        menu.update(0.0, &crate::ui::fixture::FixtureMeasure, &ps_after, store.view());

        assert_eq!(
            menu.sel(),
            1,
            "the toggle row stays focused across a live poll rebuild, not snapped to the checked track"
        );
        assert_eq!(
            menu.selected_id(),
            Some(TrackRowId::Boost),
            "and the row at that position is still, logically, the same Boost row"
        );

        teardown(&ps_before);
    }

    /// **The follow-up gap the previous fix left open.** The offer can VANISH entirely for a poll
    /// or two and return before the viewer acts. M7 narrowed what can cause that: a subtitle
    /// appearing no longer withdraws the offer at all (it routes to Burn or Remux instead, still
    /// drawn); the ONE thing that still flips the rows fully absent is the Plex Pass fact itself
    /// (I1/I2), which is what this fixture now simulates. While the rows are gone, `table.sel`
    /// falls back to the checked track (there is no Boost/Loudness row left to preserve identity
    /// against), and the ENGINE's own reconcile can independently clamp its stale remembered index
    /// into the smaller row count and write a DIFFERENT row back via `focus_key` — exactly the way
    /// `PlayerOverlayScreen::step`'s `FocusMoved` arm does on a real device. Simulating that clamp
    /// here (rather than the checked-track fallback) proves the fix reads back the identity that
    /// was banked before the vanish, not whatever `table.sel` happens to hold once the rows return.
    #[test]
    fn a_rows_vanish_and_return_restores_focus_on_the_toggle_row_not_wherever_the_clamp_landed() {
        let _g = crate::testlock::serial();
        let two_tracks = || {
            store_with_audio(vec![
                crate::metadata::Stream {
                    id: 501,
                    index: 0,
                    codec: "ac3".into(),
                    channels: 2,
                    default: true,
                    ..Default::default()
                },
                crate::metadata::Stream { id: 502, index: 1, codec: "aac".into(), channels: 2, ..Default::default() },
            ])
        };
        let (ps_before, _sid_before) = enhancement_test_session(EnhTestFixture::default());
        let store = two_tracks();
        let mut menu = TrackMenuState::new(&ps_before, store.view(), 0, Vec::new());
        assert_eq!(
            menu.row_ids(),
            vec![
                Some(TrackRowId::AudioTrack(0)),
                Some(TrackRowId::AudioTrack(1)),
                Some(TrackRowId::Boost),
                Some(TrackRowId::Loudness),
            ],
            "fixture shape: two tracks, then Boost, then Loudness"
        );
        // The engine's focus lands on Boost, the way a real UP/DOWN's `FocusMoved` write-back does.
        menu.focus_key(TrackRowId::Boost.key().0);

        // The offer vanishes for a frame — under M7 only a Plex Pass flip does that (I1/I2); every
        // other gate that used to hide the rows is now a visible `Disabled` reason instead.
        let (ps_hidden, _sid_hidden) = enhancement_test_session(EnhTestFixture {
            pass: crate::plex::serverinfo::Subscription::No,
            ..Default::default()
        });
        let store_hidden = two_tracks();
        menu.update(0.0, &crate::ui::fixture::FixtureMeasure, &ps_hidden, store_hidden.view());
        assert_eq!(menu.enhance_shown, None, "fixture shape: no Plex Pass withdraws the offer entirely (I1/I2)");

        // The ENGINE's own reconcile runs the same frame right after this poll (§7.3 step 6): its
        // stale remembered index (2, Boost) is now out of range for the 2-row table and clamps to
        // the last row — Track(1), not the checked Track(0) the fallback above chose. Simulate that
        // write-back exactly as `live_update_preserves_focus_on_the_toggled_row_not_the_checked_track`
        // simulates a real `FocusMoved` via `focus_key`.
        menu.focus_key(TrackRowId::AudioTrack(1).key().0);

        // The offer returns (the subtitle switched off again) — the same live poll this menu never
        // triggered itself.
        let (ps_shown, _sid_shown) = enhancement_test_session(EnhTestFixture::default());
        let store_shown = two_tracks();
        menu.update(0.0, &crate::ui::fixture::FixtureMeasure, &ps_shown, store_shown.view());

        assert!(menu.enhance_shown.is_some(), "fixture shape: the offer is back");
        assert_eq!(
            menu.selected_id(),
            Some(TrackRowId::Boost),
            "a rows-vanish-and-return round trip must restore focus to the row the viewer was \
             actually on, not wherever the vanished frame's engine-side clamp happened to land"
        );

        teardown(&ps_before);
    }

    // ---- locale + width gates ------------------------------------------------------------------

    /// **No row label ever leaks a "Plex Pass" mention**, in any shipped locale — the rows are
    /// ordinary audio settings; the gate that hid them from everyone else is never named in
    /// prose the viewer who HAS them ever reads.
    #[test]
    fn enh_locale_values_never_mention_plex_pass() {
        use crate::i18n::{language_on_this_thread_for_test, SHIPPED};
        for language in SHIPPED {
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

    /// **No new M7 note or reason string ever uses the engineering jargon it exists to translate
    /// away from** ("remux", "transcode", "burn", "re-encode", "sidecar", "analyzed audio stream",
    /// "base layer", "HDR10") — in any shipped locale. These strings are read by a viewer who has
    /// never heard the word "remux" and should not need to.
    #[test]
    fn enh_notes_and_reasons_never_use_jargon() {
        use crate::i18n::{language_on_this_thread_for_test, Preference};
        const BANNED: &[&str] = &[
            "remux",
            "transcode",
            "burn",
            "re-encode",
            "reencode",
            "sidecar",
            "analyzed audio stream",
            "base layer",
            "hdr10",
        ];
        for language in [Preference::En, Preference::Es, Preference::Be] {
            let _guard = language_on_this_thread_for_test(language);
            for (name, value) in [
                ("enh_note_sidecar", crate::i18n::msg::widgets_tracks_enh_note_sidecar()),
                ("enh_note_burn", crate::i18n::msg::widgets_tracks_enh_note_burn()),
                ("enh_note_dv_off", crate::i18n::msg::widgets_tracks_enh_note_dv_off()),
                ("enh_reason_not_analyzed", crate::i18n::msg::widgets_tracks_enh_reason_not_analyzed()),
                ("enh_reason_dv_unusable", crate::i18n::msg::widgets_tracks_enh_reason_dv_unusable()),
                ("enh_reason_dv_subtitle", crate::i18n::msg::widgets_tracks_enh_reason_dv_subtitle()),
                ("enh_reason_quality", crate::i18n::msg::widgets_tracks_enh_reason_quality()),
                ("enh_reason_refused", crate::i18n::msg::widgets_tracks_enh_reason_refused()),
                ("style_locked_note", crate::i18n::msg::widgets_tracks_style_locked_note()),
            ] {
                let lower = value.to_lowercase();
                for word in BANNED {
                    assert!(
                        !lower.contains(word),
                        "{language:?}/{name}: {value:?} uses the banned engineering term {word:?}"
                    );
                }
            }
        }
    }

    /// **Every enhancement row fits the Audio panel in every shipped language**, same discipline
    /// as `every_subtitles_row_fits_the_panel_in_every_language` above over the Subtitles panel.
    ///
    /// `locales/be/widgets.json`'s `normalize_loudness` reads "Нармалізацыя гуку" ("normalization
    /// of sound") rather than the more literal "Нармалізацыя гучнасці" ("normalization of
    /// loudness") on purpose: the literal phrase was chosen against when the Audio panel was a fixed
    /// 560px with a 369px column, where it measured 378px (9px over) and "гуку" measured under.
    /// This test now grades every row at the shared menu cap (`MENU_MAX_W`) and at the width the
    /// hugged popover actually gets. Re-check with this test before changing the Belarusian
    /// string back; do not assume either phrase's width from the source text alone.
    #[test]
    fn enh_rows_fit_menu_cap_es_be() {
        use crate::i18n::{language_on_this_thread_for_test, SHIPPED};
        let mut out = Vec::new();
        for language in SHIPPED {
            let _g = crate::testlock::serial();
            let _guard = language_on_this_thread_for_test(language);
            let (menu, ps) = audio_tab(EnhTestFixture {
                applied: crate::plex::AudioEnhancements { boost_dialog: true, normalize_loudness: true },
                ..Default::default()
            });
            out.extend(menu.form.table.menu_cap_failure(&crate::fontcov::advances::ShippedMeasure, language.tag()));
            out.extend(menu.form.table.app_fit_failures(crate::ui::table::MENU_MAX_W, language.tag()));
            out.extend(menu.form.table.app_fit_failures_hugged(language.tag()));
            teardown(&ps);
        }
        crate::ui::table::assert_no_fit_failures(&out);
    }

    // ---- M7 follow-up: the Subtitles tab under a live Burn ------------------------------------

    /// Build the Subtitles tab against `route`, with one embedded subtitle track whose PMS id
    /// (999) matches `enhancement_test_session`'s own `cur_sub_sid` for a non-`None`
    /// `subtitle_effect` — the Subtitles-tab counterpart of [`audio_tab`].
    fn subtitles_tab(route: EnhTestFixture) -> (TrackMenuState, crate::route::PlaybackSession) {
        let (ps, _sid) = enhancement_test_session(route);
        let store = super::tests::store_with(vec![super::tests::stream(999, 0, "English", "eng", "")]);
        let menu = TrackMenuState::new(&ps, store.view(), 1, Vec::new());
        (menu, ps)
    }

    fn flat_rows(menu: &TrackMenuState) -> Vec<&Row> {
        menu.form.table.sections.iter().flat_map(|s| &s.rows).collect()
    }

    /// **The failing case this fix closes**: while the audio enhancement is actually burning the
    /// on-screen (embedded) subtitle into the picture, the Subtitles tab's Timing and Style rows
    /// must stay VISIBLE (not omitted the way an ordinary transcode omits Timing), drawn dim, with
    /// one plain-language note — the text is already in the video, and neither control can reach
    /// it. The track-selection rows (Off, the embedded track itself) are unaffected.
    #[test]
    fn subtitles_tab_dims_timing_and_color_under_live_burn() {
        let _g = crate::testlock::serial();
        let (menu, ps) = subtitles_tab(EnhTestFixture {
            subtitle_effect: crate::route::SubtitleEffect::Embedded,
            applied: crate::plex::AudioEnhancements { boost_dialog: true, normalize_loudness: false },
            applied_burn: true,
            ..Default::default()
        });
        assert!(menu.sub_style_locked, "the live route is burning this subtitle in");

        let timing_i = menu.form.index_of(&TrackRowId::Timing).expect("Timing row present");
        let style_i = menu.form.index_of(&TrackRowId::Style).expect("Style row present");
        let rows = flat_rows(&menu);
        assert!(rows[timing_i].dim, "Timing is dim under a live burn");
        assert!(rows[style_i].dim, "Style is dim under a live burn");

        let note_i = style_i + 1;
        assert_eq!(menu.form.id_at(note_i), None, "a note is an inert slot with no id");
        assert_eq!(rows[note_i].label, crate::i18n::msg::widgets_tracks_style_locked_note());
        assert!(rows[note_i].sep, "a note row is non-selectable");

        // The track rows themselves stay live: Off, and the embedded subtitle, neither dim.
        let off_i = menu.form.index_of(&TrackRowId::Off).expect("Off row present");
        assert!(!rows[off_i].dim);
        let sub_i = menu.form.index_of(&TrackRowId::SubTrack(0)).expect("Sub(0) row present");
        assert!(!rows[sub_i].dim);

        teardown(&ps);
    }

    /// Offered-but-not-applied (the enhancement toggle is off, or the offer is merely available)
    /// must NOT lock the rows — only an actually-applied Burn does.
    #[test]
    fn subtitles_tab_timing_and_color_stay_live_when_not_applied() {
        let _g = crate::testlock::serial();
        // `remux: None` (Direct family) so this is not itself "a transcode" — isolates the case
        // from `timing_is_omitted_under_transcode_and_dim_while_subtitles_are_off`'s own coverage
        // of an ordinary (non-enhancement) transcode omitting Timing outright.
        let (menu, ps) = subtitles_tab(EnhTestFixture {
            remux: None,
            subtitle_effect: crate::route::SubtitleEffect::Embedded,
            ..Default::default()
        });
        assert!(!menu.sub_style_locked);
        let timing_i = menu.form.index_of(&TrackRowId::Timing).expect("Timing row present");
        assert!(!flat_rows(&menu)[timing_i].dim);
        assert!(!menu.form.table.sections.iter().flat_map(|s| s.rows.iter()).any(|r| r.is_note()));
        teardown(&ps);
    }

    /// OK on the dimmed Timing/Style rows is a no-op (`TrackOk::Inert`), the same "focusable but
    /// inert" contract `TrackRowId::Timing` already had while subtitles are Off — it must not open
    /// the Timing capsule or push Style while the server owns the picture.
    #[test]
    fn subtitles_ok_on_locked_timing_and_color_is_inert() {
        let _g = crate::testlock::serial();
        let (mut menu, ps) = subtitles_tab(EnhTestFixture {
            subtitle_effect: crate::route::SubtitleEffect::Embedded,
            applied: crate::plex::AudioEnhancements { boost_dialog: true, normalize_loudness: false },
            applied_burn: true,
            ..Default::default()
        });
        let store = super::tests::store_with(vec![super::tests::stream(999, 0, "English", "eng", "")]);

        let timing_i = menu.form.index_of(&TrackRowId::Timing).unwrap();
        menu.focus_row(timing_i as c_int);
        assert_eq!(menu.on_ok(store.view()), TrackOk::Inert);

        let style_i = menu.form.index_of(&TrackRowId::Style).unwrap();
        menu.focus_row(style_i as c_int);
        assert_eq!(menu.on_ok(store.view()), TrackOk::Inert);
        assert!(menu.page_path().is_empty(), "a locked Style row opens no page");

        teardown(&ps);
    }

    /// Turning the subtitle Off while a Burn is live is still a live pick, not inert — the panel
    /// must keep re-routing normally; only Timing/Style are locked.
    #[test]
    fn subtitles_off_stays_live_under_a_burn() {
        let _g = crate::testlock::serial();
        let (mut menu, ps) = subtitles_tab(EnhTestFixture {
            subtitle_effect: crate::route::SubtitleEffect::Embedded,
            applied: crate::plex::AudioEnhancements { boost_dialog: true, normalize_loudness: false },
            applied_burn: true,
            ..Default::default()
        });
        let store = super::tests::store_with(vec![super::tests::stream(999, 0, "English", "eng", "")]);
        let off_i = menu.form.index_of(&TrackRowId::Off).unwrap();
        menu.focus_row(off_i as c_int);
        match menu.on_ok(store.view()) {
            TrackOk::Commit { commit: TrackCommit::Subtitle { render_ordinal, stream_id, .. }, keep_open } => {
                assert_eq!(render_ordinal, -1);
                assert_eq!(stream_id, 0);
                assert!(!keep_open);
            }
            other => panic!("expected a live Subtitle commit, got {other:?}"),
        }
        teardown(&ps);
    }

    /// **PR #309 field report**: a TV screenshot taken ~14s after a subtitle pick showed the
    /// Subtitles menu still open — "Full" (the embedded track) focused, but the checkmark still on
    /// "Off" and Color not dimmed. The pick's own optimistic write (`Self::on_ok`) lands
    /// `active_sub` at once, but the Burn it triggers is a real `/decision` network round trip
    /// (`route::decision::retranscode_as`) that only lands `live_is_own_burn` seconds later — the
    /// gap between the pick and the screenshot. A panel built before that round trip landed, and
    /// left open across it the way the diagnostic `screens::player::overlay::pick_track_row`
    /// trigger deliberately does ("the trigger exists to leave the chosen track's panel on screen
    /// for a capture"), never rebuilt its checked row or its lock — until `Self::update`'s new
    /// per-tick poll (mirroring the Audio tab's own live poll just above it) started catching it.
    #[test]
    fn subtitles_tab_poll_catches_a_burn_that_lands_after_the_panel_opened() {
        let _g = crate::testlock::serial();
        // Opened before the pick: subtitle Off, no burn yet — the same cold-start shape
        // `subtitle_first_pick_while_plain_enhanced_remux_burns_it` (route/decision_audio_
        // enhancement_tests.rs) drives before its own live pick.
        let (mut menu, _ps_before) = subtitles_tab(EnhTestFixture {
            subtitle_effect: crate::route::SubtitleEffect::None,
            applied: crate::plex::AudioEnhancements { boost_dialog: false, normalize_loudness: true },
            applied_burn: false,
            ..Default::default()
        });
        assert_eq!(menu.active_sub, -1, "off is checked before the pick");
        assert!(!menu.sub_style_locked, "not a burn yet");

        // Seconds later: the SAME session's route has actually landed the Burn (a fresh
        // `PlaybackSession` standing in for the live one having moved on while this menu instance
        // sat untouched — `ps` is process-external state the menu never owns a copy of).
        let (ps_after, _sid) = enhancement_test_session(EnhTestFixture {
            subtitle_effect: crate::route::SubtitleEffect::Embedded,
            applied: crate::plex::AudioEnhancements { boost_dialog: false, normalize_loudness: true },
            applied_burn: true,
            ..Default::default()
        });
        let store = super::tests::store_with(vec![super::tests::stream(999, 0, "English", "eng", "")]);
        menu.update(0.016, &crate::ui::fixture::FixtureMeasure, &ps_after, store.view());

        assert_eq!(menu.active_sub, 0, "the embedded track must read checked once the route shows it");
        assert!(menu.sub_style_locked, "Color/Timing must lock once the live route is really a Burn");
        let style_i = menu.form.index_of(&TrackRowId::Style).expect("Style row present");
        assert!(flat_rows(&menu)[style_i].dim, "Style must actually redraw dim, not just flag it internally");
        let off_i = menu.form.index_of(&TrackRowId::Off).expect("Off row present");
        assert!(!flat_rows(&menu)[off_i].checked, "Off must no longer read checked");
        let sub_i = menu.form.index_of(&TrackRowId::SubTrack(0)).expect("Sub(0) row present");
        assert!(flat_rows(&menu)[sub_i].checked, "the embedded track must read checked, not Off");

        teardown(&ps_after);
    }

    /// The locked note fits the Subtitles panel in every shipped language, same discipline as
    /// `enh_rows_fit_menu_cap_es_be` over the Audio panel.
    #[test]
    fn subtitles_locked_note_fits_menu_cap_es_be() {
        use crate::i18n::{language_on_this_thread_for_test, Preference};
        let mut out = Vec::new();
        for language in [Preference::En, Preference::Es, Preference::Be] {
            let _g = crate::testlock::serial();
            let _guard = language_on_this_thread_for_test(language);
            let (menu, ps) = subtitles_tab(EnhTestFixture {
                subtitle_effect: crate::route::SubtitleEffect::Embedded,
                applied: crate::plex::AudioEnhancements { boost_dialog: true, normalize_loudness: false },
                applied_burn: true,
                ..Default::default()
            });
            out.extend(menu.form.table.menu_cap_failure(&crate::fontcov::advances::ShippedMeasure, language.tag()));
            out.extend(menu.form.table.app_fit_failures(crate::ui::table::MENU_MAX_W, language.tag()));
            out.extend(menu.form.table.app_fit_failures_hugged(language.tag()));
            teardown(&ps);
        }
        crate::ui::table::assert_no_fit_failures(&out);
    }

    /// **The Spanish locked-style note WRAPS instead of running off the panel**: the fit gate now
    /// judges the note row, and its row is as tall as its wrapped lines.
    #[test]
    fn spanish_locked_note_wraps_within_the_subtitles_panel() {
        use crate::ui::machine::Measure;
        use crate::fontcov::advances::{ShippedMeasure, HEADROOM};
        use crate::i18n::{language_on_this_thread_for_test, Preference};
        let _g = crate::testlock::serial();
        let _guard = language_on_this_thread_for_test(Preference::Es);
        let (menu, ps) = subtitles_tab(EnhTestFixture {
            subtitle_effect: crate::route::SubtitleEffect::Embedded,
            applied: crate::plex::AudioEnhancements { boost_dialog: true, normalize_loudness: false },
            applied_burn: true,
            ..Default::default()
        });
        let note = crate::i18n::msg::widgets_tracks_style_locked_note();
        let line = ShippedMeasure.width_str(&note, crate::ui::theme::size::CAPTION, false);
        let before = menu.form.table.measured_height();
        let pw = menu.form.table.menu_panel_width(&ShippedMeasure);
        let issues = menu.form.table.fit_report(pw, &ShippedMeasure, HEADROOM);
        assert!(issues.iter().all(|i| i.origin != crate::ui::table::Origin::App), "{issues:?}");
        assert!(line > pw, "the premise: one line of it is wider than the panel");
        assert!(menu.form.table.measured_height() > before, "the panel grows by the wrapped note's extra lines");
        teardown(&ps);
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

    /// The focus element of the i-th audio track: the row's KEY, never its position.
    fn k(i: usize) -> u32 {
        TrackRowId::AudioTrack(i).key().0
    }

    fn three_row_form() -> TrackForm {
        let mut sec = FormSection::new("Audio");
        for (i, label) in ["English", "Русский", "Français"].into_iter().enumerate() {
            sec = sec.item(TrackRowId::AudioTrack(i), RowKind::Choice, (), Row::new(label));
        }
        Form::new().section(sec)
    }

    /// A three-row Audio tab, built without a `PlaybackSession` or a playing item — nothing here
    /// reads either.
    fn three_row_menu() -> TrackMenuState {
        let mut form = TrackTable::new(BAND_BASE);
        form.open(three_row_form(), None);
        TrackMenuState {
            tab: 0,
            active_audio: 0,
            active_sub: -1,
            offset_ms: 0,
            tone: SubtitleTone::White,
            size: SubtitleSize::Medium,
            position: SubtitlePosition::Low,
            pages: Vec::new(),
            renderer: SubRenderer::Text,
            sub_sig: None,
            yours: Vec::new(),
            enhance_shown: None,
            enhance_route: None,
            enhance_disabled: None,
            enhance_subtitle_effect: crate::route::SubtitleEffect::None,
            sticky_audio_target: None,
            sub_style_locked: false,
            form,
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
            let step = |i: usize, dir: Dir| {
                match <TrackMenuPart as Focusable<HostFixture>>::neighbour(
                    &part,
                    FocusKey { entry: e, elem: k(i) },
                    dir,
                    cx,
                ) {
                    Step::Move(k) => Some(k.elem),
                    Step::Edge => None,
                }
            };
            assert_eq!(step(0, Dir::Down), Some(k(1)));
            assert_eq!(step(2, Dir::Down), None, "the last row does not wrap");
            assert_eq!(step(0, Dir::Up), None, "the first row does not wrap");
            assert_eq!(step(1, Dir::Up), Some(k(0)));
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
                    FocusKey { entry: e, elem: k(1) },
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
        let r = st.panel_rect(&crate::ui::fixture::FixtureMeasure);
        let want = st.form.table.row_frame(r, 2);
        let part = TrackMenuPart { state: &st, entry: e, group: GroupId(0) };
        with_cx(e, |cx| {
            let placed = <TrackMenuPart as Focusable<HostFixture>>::place(&part, &k(2), cx, At::Drawn);
            assert_eq!(
                placed.map(|p| (p.rect.x, p.rect.y, p.rect.w, p.rect.h)),
                want.map(|r| (r.x, r.y, r.w, r.h))
            );
        });
    }

    /// **The follow-up gap.** A rebuild can land the selection on a row the ENGINE is not on (the
    /// Audio tab's enhancement rows vanishing under a live poll, then returning onto the banked
    /// toggle): the engine only learns it if `reconcile` reports it. The form records the landing
    /// ([`FormTable::reseat`], set by the rebuild because the engine's last key no longer names the
    /// landed row), and `reconcile` answers that key in preference to the engine's own in-range
    /// `want`; with no pending landing it echoes the engine's key for a row it still knows.
    #[test]
    fn reconcile_follows_a_rebuilds_landing_over_the_engines_remembered_key() {
        let e = EntryId(5);
        let mut st = three_row_menu();
        st.focus_key(k(0)); // the engine holds row 0, as after a `FocusMoved`
        with_cx(e, |cx| {
            let part = TrackMenuPart { state: &st, entry: e, group: GroupId(0) };
            let want = FocusKey { entry: e, elem: k(0) };
            let got = <TrackMenuPart as Focusable<HostFixture>>::reconcile(&part, want, cx);
            assert_eq!(got.elem, k(0), "no rebuild landed elsewhere: the engine's key stands");
        });
        // a live refresh lands on row 2 (the banked row returning) without a FocusMoved
        st.form.refresh_with(three_row_form(), Some(&TrackRowId::AudioTrack(2)), None);
        with_cx(e, |cx| {
            let part = TrackMenuPart { state: &st, entry: e, group: GroupId(0) };
            let want = FocusKey { entry: e, elem: k(0) };
            let got = <TrackMenuPart as Focusable<HostFixture>>::reconcile(&part, want, cx);
            assert_eq!(got.elem, k(2), "reconcile must follow the rebuild's landing, not echo the stale key");
        });
    }
}

/// The keyed-form contract (PR 2a of `docs/player-submenus.md`): a row's focus key is its id's,
/// stable across rebuilds, and the initial focus is an explicit id.
#[cfg(test)]
mod keyed_form_tests {
    use super::tests::{store_with, store_with_audio, stream};
    use super::*;
    use crate::route::{enhancement_test_session, reset_player_control_for_test, EnhTestFixture};

    fn audio(id: i64, index: i64, default: bool) -> metadata::Stream {
        metadata::Stream { id, index, codec: "aac".into(), channels: 2, default, ..Default::default() }
    }

    fn key_of(menu: &TrackMenuState, id: TrackRowId) -> Option<RowKey> {
        menu.form.index_of(&id).and_then(|i| menu.form.key_at(i))
    }

    #[test]
    fn keys_are_distinct_across_both_tabs_and_below_the_ceiling() {
        let mut ids = vec![TrackRowId::Off, TrackRowId::Timing, TrackRowId::Style, TrackRowId::Boost, TrackRowId::Loudness];
        for field in StyleField::ALL {
            ids.push(TrackRowId::OpenField(field));
            ids.extend((0..field.rungs()).map(|i| TrackRowId::Choice(field, i)));
        }
        ids.extend((0..40).flat_map(|i| [TrackRowId::SubTrack(i), TrackRowId::AudioTrack(i)]));
        let keys: Vec<u32> = ids.iter().map(|i| i.key().0).collect();
        for (n, a) in keys.iter().enumerate() {
            assert!(*a < BAND_BASE);
            assert!(!keys[n + 1..].contains(a), "duplicate key {a:#x}");
        }
        // an absurd index saturates instead of spilling into the other tab's band
        assert!(TrackRowId::AudioTrack(1 << 20).key().0 < SUB_KEY_BASE);
    }

    /// **A live refresh that inserts the enhancement rows leaves focus on the same audio track**,
    /// found by id, with no pending reseat (the engine's key still names the landed row).
    #[test]
    fn a_live_refresh_inserting_the_enhancement_rows_keeps_focus_on_the_same_audio_track() {
        let _g = crate::testlock::serial();
        let store = store_with_audio(vec![audio(501, 0, true), audio(502, 1, false)]);
        let (ps_hidden, _s1) = enhancement_test_session(EnhTestFixture {
            pass: crate::plex::serverinfo::Subscription::No,
            ..Default::default()
        });
        let mut menu = TrackMenuState::new(&ps_hidden, store.view(), 0, Vec::new());
        assert_eq!(menu.row_ids(), vec![Some(TrackRowId::AudioTrack(0)), Some(TrackRowId::AudioTrack(1))]);
        menu.focus_key(TrackRowId::AudioTrack(1).key().0);
        let before = key_of(&menu, TrackRowId::AudioTrack(1));

        let (ps_offered, _s2) = enhancement_test_session(EnhTestFixture::default());
        menu.update(0.0, &crate::ui::fixture::FixtureMeasure, &ps_offered, store.view());

        assert!(menu.enhance_shown.is_some(), "premise: the pair was inserted");
        assert_eq!(menu.row_ids().len(), 4, "two tracks + Boost + Loudness");
        assert_eq!(menu.selected_id(), Some(TrackRowId::AudioTrack(1)));
        assert_eq!(key_of(&menu, TrackRowId::AudioTrack(1)), before, "the track's key did not move");
        assert_eq!(RowKeys::reseat(&menu.form), None, "the engine's key still names the landed row");
        reset_player_control_for_test(&ps_hidden);
        crate::plex::reset_servers_for_test();
    }

    /// **Adding a subtitle track to the offered list moves no other row's key**, even though the
    /// added track sorts above them on screen (its key is its subs-list index, not its position).
    #[test]
    fn focus_keys_are_stable_when_a_subtitle_track_is_added() {
        let _g = crate::testlock::serial();
        crate::player::sidecar::reset();
        let ps = crate::route::PlaybackSession::IDLE;
        let two = vec![stream(1, 0, "English", "eng", ""), stream(2, 1, "French", "fra", "")];
        let mut three = two.clone();
        three.push(stream(3, 2, "Arabic", "ara", "")); // "Other languages" sorts Arabic first
        let (a, b) = (store_with(two), store_with(three));
        let before = TrackMenuState::new(&ps, a.view(), 1, Vec::new());
        let after = TrackMenuState::new(&ps, b.view(), 1, Vec::new());
        assert!(after.form.index_of(&TrackRowId::SubTrack(2)) < after.form.index_of(&TrackRowId::SubTrack(0)),
            "premise: the new track is drawn ABOVE the existing ones");
        for id in [TrackRowId::Off, TrackRowId::Timing, TrackRowId::Style, TrackRowId::SubTrack(0), TrackRowId::SubTrack(1)] {
            assert!(key_of(&before, id).is_some(), "{id:?} is built");
            assert_eq!(key_of(&before, id), key_of(&after, id), "{id:?} kept its key");
        }
    }

    /// **Initial focus is the active track on both tabs** — and Off when no subtitle is active.
    #[test]
    fn initial_focus_lands_on_the_active_track_on_both_tabs() {
        let _g = crate::testlock::serial();
        crate::player::sidecar::reset();
        let ps = crate::route::PlaybackSession::IDLE;
        // Audio: the flagged default (IDLE records no sid) is the SECOND track
        let store = store_with_audio(vec![audio(501, 0, false), audio(502, 1, true), audio(503, 2, false)]);
        let menu = TrackMenuState::new(&ps, store.view(), 0, Vec::new());
        assert_eq!(menu.selected_id(), Some(TrackRowId::AudioTrack(1)));
        assert_eq!(menu.sel(), 1);

        // Subtitles: Off when none is active, else the active track (even inside "Other languages")
        let subs = vec![stream(1, 0, "English", "eng", ""), stream(2, 1, "French", "fra", "")];
        let store = store_with(subs);
        let mut menu = TrackMenuState::new(&ps, store.view(), 1, Vec::new());
        assert_eq!(menu.selected_id(), Some(TrackRowId::Off));
        menu.active_sub = 1;
        menu.rebuild(&ps, store.view(), 1);
        assert_eq!(menu.selected_id(), Some(TrackRowId::SubTrack(1)));
        menu.focus_tab(&ps, store.view(), 0); // a tab switch re-opens on that tab's own active row
        menu.focus_tab(&ps, store.view(), 1);
        assert_eq!(menu.selected_id(), Some(TrackRowId::SubTrack(1)));
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

/// **The Style drill-in**: the page stack inside the Subtitles tab — push and pop identity, what a
/// pop restores, the locks per renderer kind, the rebuild signature, and the picks.
#[cfg(test)]
mod style_page_tests {
    use super::tests::{store_with, stream};
    use super::*;
    use crate::route::{enhancement_test_session, reset_player_control_for_test, EnhTestFixture, SubtitleEffect};

    fn teardown(ps: &crate::route::PlaybackSession) {
        reset_player_control_for_test(ps);
        crate::plex::reset_servers_for_test();
    }

    /// A direct-play route with the subtitle `codec` active (or none when `effect` is `None`).
    fn open_with(
        codec: &str,
        effect: SubtitleEffect,
    ) -> (TrackMenuState, crate::route::PlaybackSession, crate::stores::metadata::MetadataStore) {
        let (ps, _sid) = enhancement_test_session(EnhTestFixture {
            remux: None,
            subtitle_effect: effect,
            ..Default::default()
        });
        let mut s = stream(999, 0, "English", "eng", "");
        s.codec = codec.into();
        let store = store_with(vec![s]);
        let menu = TrackMenuState::new(&ps, store.view(), 1, Vec::new());
        (menu, ps, store)
    }

    fn open_text() -> (TrackMenuState, crate::route::PlaybackSession, crate::stores::metadata::MetadataStore) {
        open_with("srt", SubtitleEffect::Sidecar)
    }

    fn focus_id(menu: &mut TrackMenuState, id: TrackRowId) {
        let i = menu.form.index_of(&id).unwrap_or_else(|| panic!("{id:?} is not on this page"));
        menu.focus_row(i as c_int);
        assert_eq!(menu.selected_id(), Some(id));
    }

    fn row_of(menu: &TrackMenuState, id: TrackRowId) -> &Row {
        let i = menu.form.index_of(&id).unwrap_or_else(|| panic!("{id:?} is not on this page"));
        menu.form.table.sections.iter().flat_map(|s| &s.rows).nth(i).unwrap()
    }

    /// OK on Style pushes the Style page, focus on Size by id; OK on Size pushes its picker with
    /// the checked rung focused; LEFT pops each back onto the row that opened it.
    #[test]
    fn push_lands_on_an_explicit_id_and_pop_restores_the_opener() {
        let _g = crate::testlock::serial();
        let (mut menu, ps, store) = open_text();
        focus_id(&mut menu, TrackRowId::Style);
        assert_eq!(menu.on_ok(store.view()), TrackOk::Navigated);
        assert_eq!(menu.page_path(), [TrackPage::Style]);
        assert_eq!(menu.selected_id(), Some(TrackRowId::OpenField(StyleField::Size)));
        assert_eq!(menu.form.table.title(), Some(TrackPage::Style.title().as_ref()));

        focus_id(&mut menu, TrackRowId::OpenField(StyleField::Position));
        assert_eq!(menu.on_ok(store.view()), TrackOk::Navigated);
        assert_eq!(menu.page_path(), [TrackPage::Style, TrackPage::Picker(StyleField::Position)]);
        assert_eq!(
            menu.selected_id(),
            Some(TrackRowId::Choice(StyleField::Position, menu.position.index() as usize)),
            "a picker opens on its checked rung"
        );

        menu.on_left(&ps, store.view());
        assert_eq!(menu.page_path(), [TrackPage::Style]);
        assert_eq!(menu.selected_id(), Some(TrackRowId::OpenField(StyleField::Position)), "the opener, by id");
        menu.on_left(&ps, store.view());
        assert!(menu.page_path().is_empty());
        assert_eq!(menu.selected_id(), Some(TrackRowId::Style));
        assert_eq!(menu.form.table.title(), None, "the root has no title band");
        teardown(&ps);
    }

    /// RIGHT on a Nav row enters the page exactly as OK does; RIGHT off one is the tab switch.
    #[test]
    fn right_on_a_nav_row_pushes_and_left_at_the_root_switches_tab() {
        let _g = crate::testlock::serial();
        let (mut menu, ps, store) = open_text();
        focus_id(&mut menu, TrackRowId::Style);
        menu.on_right(&ps, store.view());
        assert_eq!(menu.page_path(), [TrackPage::Style]);
        assert!(!menu.pop(&ps, store.view()) || menu.page_path().is_empty());
        assert!(!menu.pop(&ps, store.view()), "BACK at the root pops nothing: the caller dismisses");
        menu.on_left(&ps, store.view());
        assert_eq!(menu.tab, 0, "LEFT at the root is still the tab switch");
        teardown(&ps);
    }

    /// A pop reinstates the scroll the page was left at, not the top.
    #[test]
    fn pop_restores_the_scroll_the_root_was_left_at() {
        let _g = crate::testlock::serial();
        let (ps, _sid) = enhancement_test_session(EnhTestFixture {
            remux: None,
            subtitle_effect: SubtitleEffect::Sidecar,
            ..Default::default()
        });
        let subs: Vec<_> = (0..40).map(|i| stream(999 + i, i, "English", "eng", &format!("Track {i}"))).collect();
        let store = store_with(subs);
        let mut menu = TrackMenuState::new(&ps, store.view(), 1, vec!["eng".to_string()]);
        focus_id(&mut menu, TrackRowId::Style);
        for _ in 0..240 {
            menu.update(0.016, &crate::ui::fixture::FixtureMeasure, &ps, store.view());
        }
        let left_at = menu.form.table.scroll_pos();
        assert!(left_at > 0.0, "the premise: 40 tracks push Style below the fold ({left_at})");
        assert_eq!(menu.on_ok(store.view()), TrackOk::Navigated);
        assert!(menu.form.table.scroll_pos() < left_at, "a page starts at its top");
        assert!(menu.pop(&ps, store.view()));
        assert_eq!(menu.form.table.scroll_pos(), left_at);
        assert_eq!(menu.selected_id(), Some(TrackRowId::Style));
        teardown(&ps);
    }

    /// Picking a rung commits it live, keeps the panel and page open and moves the checkmark; the
    /// already-checked rung is inert.
    #[test]
    fn a_picker_pick_commits_live_and_moves_the_checkmark() {
        let _g = crate::testlock::serial();
        let (mut menu, ps, store) = open_text();
        focus_id(&mut menu, TrackRowId::Style);
        assert_eq!(menu.on_ok(store.view()), TrackOk::Navigated);
        assert_eq!(menu.on_ok(store.view()), TrackOk::Navigated, "Size");
        let current = menu.current_rung(StyleField::Size);
        assert!(row_of(&menu, TrackRowId::Choice(StyleField::Size, current)).checked);
        assert_eq!(menu.on_ok(store.view()), TrackOk::Inert, "the focused rung is the checked one");

        let other = (current + 1) % StyleField::Size.rungs();
        focus_id(&mut menu, TrackRowId::Choice(StyleField::Size, other));
        assert_eq!(
            menu.on_ok(store.view()),
            TrackOk::Commit {
                commit: TrackCommit::SubtitleSize(SubtitleSize::from_index(other as u8)),
                keep_open: true
            }
        );
        assert_eq!(menu.page_path().len(), 2, "the page stays");
        assert!(row_of(&menu, TrackRowId::Choice(StyleField::Size, other)).checked);
        assert!(!row_of(&menu, TrackRowId::Choice(StyleField::Size, current)).checked);
        teardown(&ps);
    }

    /// The Style page reads the current value of each field on its Nav row.
    #[test]
    fn the_style_page_shows_each_fields_current_value() {
        let _g = crate::testlock::serial();
        let (mut menu, ps, store) = open_text();
        focus_id(&mut menu, TrackRowId::Style);
        menu.on_ok(store.view());
        for field in StyleField::ALL {
            let row = row_of(&menu, TrackRowId::OpenField(field));
            assert_eq!(row.label, field.label());
            assert_eq!(row.value.as_deref(), Some(field.rung_label(menu.current_rung(field))));
        }
        teardown(&ps);
    }

    /// Locks per renderer kind: plain text leaves all three live; an image subtitle dims Size and
    /// Position with the image note; ASS dims them with its own; Color stays live in every case,
    /// and a dimmed row is inert for OK.
    #[test]
    fn size_and_position_lock_by_renderer_kind_and_color_never_does() {
        let _g = crate::testlock::serial();
        for (codec, renderer, note) in [
            ("srt", SubRenderer::Text, None),
            ("pgs", SubRenderer::Image, Some(crate::i18n::msg::widgets_tracks_style_image_note())),
            ("ass", SubRenderer::Styled, Some(crate::i18n::msg::widgets_tracks_style_styled_note())),
        ] {
            let (mut menu, ps, store) = open_with(codec, SubtitleEffect::Sidecar);
            assert_eq!(menu.renderer, renderer, "{codec}");
            focus_id(&mut menu, TrackRowId::Style);
            assert_eq!(menu.on_ok(store.view()), TrackOk::Navigated, "{codec}: Style itself is live");
            let locked = renderer != SubRenderer::Text;
            for field in [StyleField::Size, StyleField::Position] {
                assert_eq!(row_of(&menu, TrackRowId::OpenField(field)).dim, locked, "{codec} {field:?}");
            }
            assert!(!row_of(&menu, TrackRowId::OpenField(StyleField::Color)).dim, "{codec}: Color stays live");
            let notes: Vec<_> = menu
                .form
                .table
                .sections
                .iter()
                .flat_map(|s| &s.rows)
                .filter(|r| r.is_note())
                .map(|r| r.label.to_string())
                .collect();
            assert_eq!(notes, note.map(|n| n.to_string()).into_iter().collect::<Vec<_>>(), "{codec}");
            if locked {
                focus_id(&mut menu, TrackRowId::OpenField(StyleField::Size));
                assert_eq!(menu.on_ok(store.view()), TrackOk::Inert, "{codec}: a dim Size row opens nothing");
                menu.on_right(&ps, store.view());
                assert_eq!(menu.page_path(), [TrackPage::Style], "{codec}: RIGHT is inert on it too");
            }
            focus_id(&mut menu, TrackRowId::OpenField(StyleField::Color));
            assert_eq!(menu.on_ok(store.view()), TrackOk::Navigated, "{codec}: Color opens its picker");
            teardown(&ps);
        }
    }

    /// Style is dimmed with the locked note only for the actual own burn, and follows Timing's
    /// availability: an ordinary transcode omits both, an own burn keeps both drawn and dim.
    #[test]
    fn style_follows_timings_availability() {
        let _g = crate::testlock::serial();
        let has = |menu: &TrackMenuState, id| menu.form.index_of(&id).is_some();
        // an ordinary transcode that is not the own burn: neither row
        let (ps, _) = enhancement_test_session(EnhTestFixture { remux: Some(false), ..Default::default() });
        let store = store_with(vec![stream(999, 0, "English", "eng", "")]);
        let menu = TrackMenuState::new(&ps, store.view(), 1, Vec::new());
        assert!(!has(&menu, TrackRowId::Timing) && !has(&menu, TrackRowId::Style));
        teardown(&ps);
        // direct play: both, live
        let (menu, ps, _store) = open_text();
        assert!(has(&menu, TrackRowId::Timing) && has(&menu, TrackRowId::Style));
        assert!(!row_of(&menu, TrackRowId::Style).dim);
        teardown(&ps);
    }

    /// **A Style page never outlives what it was built for**: a signature change while on a
    /// sub-page pops to the root (opener and scroll restored); an unchanged signature leaves the
    /// page alone; on the root the same change refreshes in place.
    #[test]
    fn a_rebuild_signature_mismatch_on_a_sub_page_pops_to_the_root() {
        let _g = crate::testlock::serial();
        let (mut menu, ps, store) = open_with("srt", SubtitleEffect::Sidecar);
        focus_id(&mut menu, TrackRowId::Style);
        menu.on_ok(store.view());
        menu.update(0.016, &crate::ui::fixture::FixtureMeasure, &ps, store.view());
        assert_eq!(menu.page_path(), [TrackPage::Style], "an unchanged signature leaves the page");

        // the same subtitle, now an image one: the renderer kind moved under the page
        let mut image = stream(999, 0, "English", "eng", "");
        image.codec = "pgs".into();
        let store2 = store_with(vec![image]);
        menu.update(0.016, &crate::ui::fixture::FixtureMeasure, &ps, store2.view());
        assert!(menu.page_path().is_empty(), "popped to the root");
        assert_eq!(menu.renderer, SubRenderer::Image);
        assert_eq!(menu.selected_id(), Some(TrackRowId::Style), "on the row that opened it");
        assert_eq!(menu.form.table.title(), None);
        teardown(&ps);
    }

    /// **Every Style surface fits the panel in every shipped language**: the Subtitles root with
    /// its Style row, the Style page under each renderer kind (Size and Position dim with their
    /// notes), and each picker page, judged by the same gates as the rest of the menu.
    #[test]
    fn every_style_page_fits_the_panel_in_every_language() {
        use crate::i18n::{language_on_this_thread_for_test, Preference};
        let mut out = Vec::new();
        for language in [Preference::En, Preference::Es, Preference::Be] {
            let _g = crate::testlock::serial();
            let _guard = language_on_this_thread_for_test(language);
            for codec in ["srt", "pgs", "ass"] {
                let (mut menu, ps, store) = open_with(codec, SubtitleEffect::Sidecar);
                let mut judge = |menu: &TrackMenuState| {
                    out.extend(menu.form.table.menu_cap_failure(&crate::fontcov::advances::ShippedMeasure, language.tag()));
                    out.extend(menu.form.table.app_fit_failures(crate::ui::table::MENU_MAX_W, language.tag()));
                    out.extend(menu.form.table.app_fit_failures_hugged(language.tag()));
                };
                judge(&menu);
                menu.push(TrackPage::Style);
                judge(&menu);
                for field in StyleField::ALL {
                    menu.push(TrackPage::Picker(field));
                    judge(&menu);
                    assert!(menu.pop(&ps, store.view()));
                }
                teardown(&ps);
            }
        }
        crate::ui::table::assert_no_fit_failures(&out);
    }

    /// The title band is a pointer-only stop: its key is recognised, no row owns it.
    #[test]
    fn the_title_key_is_the_bands_and_no_rows() {
        let _g = crate::testlock::serial();
        let (mut menu, ps, store) = open_text();
        focus_id(&mut menu, TrackRowId::Style);
        menu.on_ok(store.view());
        assert!(TrackMenuState::is_title_key(TITLE_KEY));
        assert!(menu.row_keys().iter().all(|k| !TrackMenuState::is_title_key(*k)));
        teardown(&ps);
    }

    /// The replay canon carries the tab, the page path, each opener and the selected key: two
    /// states that differ in any of them hash apart.
    #[test]
    fn the_canon_tells_pages_openers_and_selections_apart() {
        let _g = crate::testlock::serial();
        let hash = |menu: &TrackMenuState| {
            let mut c = Canon::new();
            menu.canon(&mut c);
            c.finish()
        };
        let (mut menu, ps, store) = open_text();
        focus_id(&mut menu, TrackRowId::Style);
        let root = hash(&menu);
        menu.on_ok(store.view());
        let style = hash(&menu);
        focus_id(&mut menu, TrackRowId::OpenField(StyleField::Color));
        let style_color = hash(&menu);
        menu.on_ok(store.view());
        let picker = hash(&menu);
        let all = [root, style, style_color, picker];
        for (i, a) in all.iter().enumerate() {
            for b in &all[i + 1..] {
                assert_ne!(a, b);
            }
        }
        menu.on_left(&ps, store.view());
        assert_eq!(hash(&menu), style_color, "a pop returns to the recorded state");
        teardown(&ps);
    }
}
