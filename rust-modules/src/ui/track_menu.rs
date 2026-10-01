//! In-player modal track menu: audio + subtitle pickers over the video, rendered on the reusable
//! animated `TableView` (Apple-TV "settings" look — a sliding pill selection, section header with
//! a codec accessory, per-row badges, a leading checkmark on the active track). app.rs routes
//! D-pad/OK/BACK here while the menu is open; LEFT/RIGHT switch between the Audio and Subtitles
//! panels on a root (on the Subtitles tab LEFT pops a sub-page and RIGHT on a Nav row enters it;
//! BACK pops a sub-page before it dismisses). The selection commit (native audio switch / server transcode / burn) is unchanged
//! from the previous procedural version — only the presentation moved onto the table.
//!
//! **The Subtitles panel is grouped, not one flat list** (plan `subtitle-menu-capsule` §3,
//! `/tmp/dsplayer/player.html:1104-1162`): Off and every single-track "yours" language sit under
//! one "Subtitles" header; a "yours" language with several tracks gets its own section (header =
//! language, `accessory("N tracks")`), ranked full < SDH < forced < commentary; everything else is
//! ONE "Other languages" row (a drill-in to [`TrackPage::OtherLanguages`], one row per language
//! A-Z; a multi-track language there drills in again to [`TrackPage::Language`], its tracks
//! ranked); and a headerless section holds Timing and Style. That
//! grouping is a pure DATA model, `metadata::sub_layout` (host-tested without a
//! `PlaybackSession`/`MetadataView` fixture); this module only turns it into `TableView` sections
//! ([`table_form`]) and answers focus/OK by the focused row's [`TrackRow`] identity. "Yours" is
//! the pref language (if the play resolved under one), the playing audio's language, and the
//! current subtitle's own language, in that order (`route::cur_sub_pref_lang`, carried in by
//! `screens::player::overlay`).
//!
//! **Both tabs are keyed declarative forms** (`ui::form`, `docs/player-submenus.md`): each row is
//! declared once with its semantic [`TrackRow`] identity, a stable [`RowKey`] (a family base plus
//! the track's index in the playing item's list, which is fixed for the item's lifetime, never the
//! list position), its kind and its [`Row`]. The focus layer's element is the row's key, so a row
//! added above another (a subtitle offered mid-play, the enhancement pair returning) moves no key,
//! and a rebuild restores the viewer's row by id.
//!
//! **Timing** is a single value row that reads out the current offset (no chevron: it is a
//! hand-off, not a page); OK on it does not step anything here — it returns
//! [`TrackOk::OpenTiming`], which `screens::player::overlay`'s `activate` turns into a hand-off: it
//! dismisses this panel and presents the Timing capsule overlay (`OverlayKind::Timing`,
//! `ui::timing_capsule`) in its place. The row is dim and inert while subtitles are Off (OK there
//! neither opens the capsule nor closes the panel), and Timing together with Style is omitted
//! during an ordinary transcode, which burns captions into the picture where no client-side offset
//! or style can reach. When the live route is instead this app's OWN Plex Pass audio-enhancement
//! Burn (M7), both stay visible — dim, with a one-line reason (`Row::note`) — so the viewer who
//! turned Boost dialog / Normalize loudness on sees why the control is locked rather than finding
//! it simply gone.
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
//! **A sub-page never outlives the root it was built on.** The root's [`SubSig`] — subs
//! fingerprint, active index, renderer kind, transcoding, own-burn and enhancement route — is
//! stored when the root is built and compared on every live poll; a mismatch refreshes the root in
//! place, and a sub-page in place too — it pops straight to the root only when its availability no
//! longer holds (the renderer, or whether Style is shown / locked). The replay canon
//! ([`TrackMenuState::canon`]) carries the tab, the page path with each return key, and the
//! selected key.
//!
//! **The Other languages flow** is data-driven by [`TrackMenuState::other`] (one `OtherLang` per
//! language, from the same `sub_layout` answer as the root), so a page lists exactly what its
//! opener promised. A language is identified by the Plex stream id of its first track in the item's
//! FULL list ([`LangId`], carried by [`TrackRow::OpenLang`] and [`TrackPage::Language`]), never a
//! list position, so tracks leaving or arriving cannot relabel an open page. A track pick on any
//! page commits and dismisses like a root pick. A page whose listing is gone (the language left the
//! offered list) pops to the root ([`TrackMenuState::pages_hold`]); a language page that drops to a
//! single track stays open, and popping it lands on that track's direct row.
use crate::metadata;
use crate::metadata::sub_layout::{self, LangId, OtherLang, RowBadge, RowTarget, SubHeader, SubModel, SubRow, SubTrack};
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
use crate::ui::form::{Activation, Form, FormId, FormSection, FormTable, RowKey, RowKind};
use crate::ui::panel_motion::PanelMotion;
use crate::ui::table::{Badge, Row, Section, TableView};
use crate::ui::table_screen::BAND_BASE;
use crate::ui::theme;
use crate::ui::{Painter, Rect};
use std::os::raw::c_int;


/// **What a focusable row of either tab IS** — the identity the [`FormTable`] resolves a press, a
/// focus move and a rebuild's landing by, never a position. The Audio tab's track rows, the two Plex
/// Pass DSP toggles and the Subtitles tab's rows (root, Style page and pickers) share one alphabet
/// so one [`FormTable`] serves every page. The non-selectable footnotes under the DSP pair and under
/// Style are inert slots ([`FormSection::note`]) and have no identity at all.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TrackRow {
    /// An Audio-tab track row — the index into the playing item's audio list
    /// ([`crate::metadata::PlayingItem::audio`]).
    Audio(usize),
    /// The Boost dialog toggle row (issue #266) — present whenever [`TrackMenuState::enhance_shown`]
    /// is `Some` (offered) OR [`TrackMenuState::enhance_disabled`] is (dim, with a reason),
    /// immediately after the last [`Self::Audio`].
    Boost,
    /// The Normalize loudness toggle row (issue #266), immediately after [`Self::Boost`].
    Loudness,
    /// The Subtitles tab's Off row.
    SubOff,
    /// A Subtitles-tab track row — the index into the playing item's subs list.
    Sub(usize),
    /// The Timing row (hands off to the Timing capsule).
    Timing,
    /// The Style drill-in on the Subtitles root ([`TrackPage::Style`]).
    Style,
    /// The Other languages drill-in on the Subtitles root ([`TrackPage::OtherLanguages`]).
    OpenOther,
    /// Other languages page: a multi-track language's drill-in ([`TrackPage::Language`]). Carries
    /// the language's identity — the stream id of its first track in the item's FULL list
    /// ([`LangId`]) — so it stays that language when tracks leave, arrive or the offered subset
    /// changes; the id's slot only picks the row's focus key.
    OpenLang(LangId),
    /// Style page: the drill-in to one field's picker ([`TrackPage::Picker`]); reads out the
    /// field's current value.
    OpenField(StyleField),
    /// A picker page's choice: the field and the rung's index on that field's ladder.
    Choice(StyleField, usize),
}

impl From<RowTarget> for TrackRow {
    fn from(t: RowTarget) -> Self {
        match t {
            RowTarget::Off => TrackRow::SubOff,
            RowTarget::Sub(i) => TrackRow::Sub(i),
            RowTarget::Timing => TrackRow::Timing,
            RowTarget::Style => TrackRow::Style,
            RowTarget::Other => TrackRow::OpenOther,
        }
    }
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
/// page stack, not a value of this type (`docs/player-submenus.md`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TrackPage {
    /// Size, Position and Color, each a drill-in showing its current value.
    Style,
    /// One field's picker: a checked [`TrackRow::Choice`] per rung.
    Picker(StyleField),
    /// One row per language the viewer has not marked as theirs, A-Z: a single-track language is a
    /// direct pick row, a multi-track one a drill-in to its [`Self::Language`] page.
    OtherLanguages,
    /// One multi-track language's tracks (ranked), a pick row each; the payload is the language's
    /// [`LangId::stream`].
    Language(i64),
}

impl TrackPage {
    /// The title band's text; a language page is titled by its language (`other` is the page
    /// model, [`TrackMenuState::other`]).
    fn title(self, other: &[OtherLang]) -> String {
        match self {
            Self::Style => crate::i18n::msg::widgets_tracks_style().to_string(),
            Self::Picker(field) => field.label().to_string(),
            Self::OtherLanguages => crate::i18n::msg::widgets_tracks_other_languages().to_string(),
            Self::Language(stream) => {
                other.iter().find(|o| o.id.stream == stream).map(|o| o.name.clone()).unwrap_or_default()
            }
        }
    }

    /// A stable small number for the replay canon. Never reordered: recordings hash it.
    fn code(self) -> u32 {
        match self {
            Self::Style => 1,
            Self::Picker(field) => 0x10 + field.ordinal(),
            Self::OtherLanguages => 0x20,
            Self::Language(stream) => 0x2000_0000 | (stream as u32 & 0x00FF_FFFF),
        }
    }
}

/// The pointer-only key of the page title band ("< STYLE"): OUTSIDE the form's key range (at the
/// ceiling), so it is never a row and never in the D-pad column. A click on it pops.
pub(crate) const TITLE_KEY: u32 = BAND_BASE;

/// The hand-assigned focus key of each row: a family base per kind of row plus the track's own
/// index, all far below the band. None of these is a position, so a menu whose rows reorder (a
/// track list that sorts differently, the DSP pair appearing) moves no key. Free families for the
/// language pages: `0x0008_0000` the Other languages drill-in, `0x0009_0000 +` the slot a language
/// holds on that page.
impl FormId for TrackRow {
    fn key(&self) -> RowKey {
        // a picker's rungs: one 256-wide block per field (a ladder is a handful of rungs)
        let rung = |f: StyleField, i: usize| 0x0007_0000 + f.ordinal() * 0x100 + (i as u32).min(0xFF);
        RowKey(match *self {
            TrackRow::Audio(i) => 0x0001_0000 + i as u32,
            TrackRow::Boost => 0x0002_0000,
            TrackRow::Loudness => 0x0002_0001,
            TrackRow::SubOff => 0x0003_0000,
            TrackRow::Sub(i) => 0x0004_0000 + i as u32,
            TrackRow::Timing => 0x0005_0000,
            TrackRow::Style => 0x0005_0001,
            TrackRow::OpenField(f) => 0x0006_0000 + f.ordinal(),
            TrackRow::Choice(f, i) => rung(f, i),
            TrackRow::OpenOther => 0x0008_0000,
            TrackRow::OpenLang(lang) => 0x0009_0000 + (lang.slot as u32).min(0xFFFF),
        })
    }
}

/// Both tabs' form: the action type is `()` (the id says what a row does); `Dest` is the page a
/// Nav row opens.
type TrackForm = Form<TrackRow, (), TrackPage>;
type TrackTable = FormTable<TrackRow, (), TrackPage>;

/// [`TrackMenuState::enh_state`]'s answer: the Audio tab's Boost/Loudness offer as displayed, the
/// route it rides, the reason it is disabled, and the live subtitle effect its note names.
type EnhState = (
    Option<crate::plex::AudioEnhancements>,
    Option<crate::route::EnhancementRoute>,
    Option<crate::route::DisabledReason>,
    crate::route::SubtitleEffect,
);

/// **A tab root's natural panel rect** — the layout of `table` as the panel would hug it. Each
/// tab hugs its own rows (shared menu rule); the right edge is fixed, so switching tabs moves only
/// the left edge.
fn table_natural(table: &TableView, measure: &dyn crate::ui::machine::Measure) -> Rect {
    let pw = table.menu_panel_width(measure);
    // the transport control row's own right edge — one number for the discs and both panels
    let px = crate::ui::player_hud::CTRL_RIGHT - pw;
    // Bottom-anchored just above the control-button row (buttons top at SCR_H-288) with a clear gap.
    // The panel grows UPWARD from this fixed bottom edge, and its height is capped so the top never
    // crosses `top_min` — so a long list (an item with many audio dubs) SCROLLS inside the panel
    // instead of the panel itself spilling down over the buttons. Switching Audio↔Subtitles keeps
    // the bottom edge steady.
    let bottom = SCR_H - 316.0; // 764 — ~28px above the buttons
    // A note row wraps, so its line count depends on the width: it is resolved HERE, against the
    // same `measure` and `pw` the panel is sized with, so no caller (`update`, hit-testing,
    // `draw`) can read the count a rebuild left stale. Idempotent and a few short strings.
    table.fit_notes(pw, measure);
    let top_min = 60.0;
    let ph = table.measured_height().clamp(160.0, bottom - top_min);
    let py = bottom - ph; // ≥ top_min by construction
    Rect::new(px, py, pw, ph)
}

/// **What a pushed page remembers of the page beneath it**: the opener's id and the scroll the
/// list was left at, so a pop ([`FormTable::restore`]) brings the page back exactly as it was.
#[derive(Clone, Copy, Debug)]
struct Saved {
    /// The page this entry opened.
    page: TrackPage,
    /// The row that opened it, on the page beneath.
    return_id: TrackRow,
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

impl SubSig {
    /// What a pushed Style page's availability was built from: the renderer (Size / Position reach
    /// only text), whether the app's own burn locks Style, and whether an ordinary server burn
    /// omits it (`transcoding && !own_burn`, the gate [`TrackMenuState::layout`] shows Style by).
    /// A page is popped when this changes and refreshed in place otherwise.
    fn page_availability(&self) -> (SubRenderer, bool, bool) {
        (self.renderer, self.own_burn, self.transcoding && !self.own_burn)
    }
}

/// The menu's whole state, owned by the container that mounts this panel — the modal PHASE and the
/// appear spring belong to `ui::containers::modal::ModalStack` now, not to this struct; `draw` takes
/// the appear fraction as a parameter instead of stepping its own [`Popover`].
pub(crate) struct TrackMenuState {
    tab: c_int, // 0=Audio, 1=Subtitles
    active_audio: c_int, // index into the playing item's audio list
    active_sub: c_int, // -1 = Off, else index into the playing item's subs list
    /// Both tabs' rows, declared once each as a [`TrackForm`] and resolved by [`TrackRow`]
    /// identity — [`Self::on_ok`] reads back what the focused row IS from its id rather than from a
    /// position (and so cannot disagree with what was drawn, the way a fresh call to
    /// [`visible_subs`] could once a transcode starts). Whichever tab is built owns the table; a
    /// tab switch replaces it whole ([`Self::rebuild`]).
    form: TrackTable,
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
    /// The Other languages page's model — one entry per language, as of the last root build or
    /// live poll. The Other languages and language pages draw from it, so what a page lists and
    /// what its opener promised cannot disagree, and a language whose entry is gone pops its page.
    other: Vec<OtherLang>,
    /// The subtitle indices the Subtitles ROOT lists as track rows, in display order — what the
    /// harness's `track:N` counts first ([`Self::sub_track_for_target`]).
    root_tracks: Vec<usize>,
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
    /// [`Self::rebuild_audio`] stashes the focused [`TrackRow::Boost`]/[`TrackRow::Loudness`]
    /// here the moment those rows are about to disappear, and restores it — in preference to
    /// reading `table.sel` back — the moment they reappear. Reading `table.sel` at that point
    /// instead would be wrong: while the rows are gone `table.sel` sits on whatever the fallback
    /// (or the ENGINE's own raw index clamp while the row count was smaller — see
    /// [`TrackMenuPart::reconcile`]) landed on, which names an unrelated track once the enhancement
    /// rows are back. `None` once consumed, or when nothing needs remembering.
    sticky_audio_target: Option<TrackRow>,
    /// The card's resize and the page slide ([`crate::ui::panel_motion`]): the layout target is
    /// cached there, the top/left edges spring to it, and a push or pop slides the two pages.
    motion: PanelMotion,
    /// Whether [`Self::warm_other_tab`] has queued the other tab's root strings for this menu.
    other_tab_warmed: bool,
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
    /// OK does nothing and must not close the panel either: the dim Timing row while subtitles are
    /// Off, a locked Timing / Style row, a disabled Size / Position row, and a re-pick of the
    /// rung that is already checked.
    Inert,
}

impl Drop for TrackMenuState {
    /// A closed menu cannot switch tabs, so its other tab's warm ([`Self::warm_other_tab`]) is
    /// work for nobody.
    fn drop(&mut self) {
        if self.other_tab_warmed {
            crate::text::clear_background_prewarm();
        }
    }
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
            form: TrackTable::new(BAND_BASE),
            offset_ms: crate::player::subtitle_offset_ms(),
            tone: crate::player::subtitle_tone(),
            size: crate::route::subtitle_size(),
            position: crate::route::subtitle_position(),
            pages: Vec::new(),
            renderer: SubRenderer::Text,
            sub_sig: None,
            other: Vec::new(),
            root_tracks: Vec::new(),
            yours,
            enhance_shown: None,
            enhance_route: None,
            enhance_disabled: None,
            enhance_subtitle_effect: crate::route::SubtitleEffect::None,
            sticky_audio_target: None,
            motion: PanelMotion::new(),
            other_tab_warmed: false,
        };
        s.form.table.min_panel_w = theme::layout::PLAYER_MENU_MIN_W;
        s.sync_item(ps, meta);
        s.rebuild(ps, meta, tab, false);
        s
    }

    /// The highlighted row, for the focus probe (`crate::focusprobe`) — a READ of the cursor the
    /// key ladder moves, and the reason it exists: `app.rs`'s UP/DOWN arm for this panel changes
    /// nothing else, so without this the fingerprint records the panel opening and closing and
    /// nothing between.
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

    /// What the `submenuosc` trigger needs to choose its next key: the tab (0 Audio, 1 Subtitles),
    /// how many pages are pushed above the root, and whether the root offers Other languages.
    pub(crate) fn osc_probe(&self) -> (c_int, usize, bool) {
        (self.tab, self.pages.len(), self.form.index_of(&TrackRow::OpenOther).is_some())
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

    /// Was the Subtitles root last built under the app's own live burn (what locks Timing and Style)?
    #[cfg(test)]
    pub(crate) fn own_burn_built(&self) -> bool {
        self.sub_sig.as_ref().is_some_and(|sig| sig.own_burn)
    }

    /// The highlighted row's id (`None` on a note or an empty list).
    #[cfg(test)]
    pub(crate) fn selected_id(&self) -> Option<TrackRow> {
        self.form.selected_id().copied()
    }

    /// The focusable rows at the current tab, in drawn order, by identity — for a test that needs
    /// to name a row without duplicating this layout by hand (`screens::player::overlay_tests`'s
    /// Timing hand-off test).
    #[cfg(test)]
    pub(crate) fn ids(&self) -> Vec<TrackRow> {
        (0..self.form.table.n_rows() as usize).filter_map(|i| self.form.id_at(i).copied()).collect()
    }

    /// Every focusable row's focus element, in drawn order.
    #[cfg(test)]
    pub(crate) fn keys(&self) -> Vec<u32> {
        self.ids().iter().map(|id| id.key().0).collect()
    }

    /// The focus element (a row's [`RowKey`] number) of the row `id` names, if this tab has it.
    #[cfg(test)]
    pub(crate) fn key_of(&self, id: TrackRow) -> Option<u32> {
        self.form.index_of(&id).map(|_| id.key().0)
    }

    /// Move focus onto the row `id` names (a test's, or the `submenuosc` trigger's, way of
    /// pressing DOWN to it); `false` when this tab has no such row.
    pub(crate) fn focus_id(&mut self, id: TrackRow) -> bool {
        self.focus_key(id.key().0);
        self.form.selected_id() == Some(&id)
    }

    /// **Write back the engine's own focus cursor** (restructure phase 12): the Column group
    /// [`TrackMenuPart`] answers is the source of geometry, but the ENGINE owns the current
    /// element (§7.3 step 5) — the owner's `step` is the only place that mutates in response to a
    /// `FocusMoved`, and this is `screens::player::overlay::PlayerOverlayScreen::step`'s write.
    /// The element is a row's [`RowKey`] number; a key this tab does not know moves nothing.
    pub(crate) fn focus_key(&mut self, elem: u32) {
        if let Some(i) = self.form.index_of_key(RowKey(elem)) {
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
    /// checkmark and its Style/Timing dim state both kept whatever `Self::layout` baked at open,
    /// disagreeing with the route by the time a capture actually looked at it. `rebuild`'s own
    /// subtitle arm always re-homes the cursor onto the checked row — correct for an open or a tab
    /// switch, wrong for a background poll that must not steal focus from wherever the viewer's
    /// cursor actually is (the exact focus-desync class `refresh_audio`'s doc names)
    /// — so this refreshes the form, which restores the current row by [`TrackRow`] in the
    /// freshly built list instead of snapping to the active track, the same fix `refresh_audio` applies for the Audio
    /// tab's own poll.
    ///
    /// **It rebuilds on a [`SubSig`] change**, not on the two values it once compared: the subs
    /// list, the active index, the renderer kind, transcoding, and the enhancement route and
    /// subtitle effect. On the root that is a refresh in place. ON A SUB-PAGE the page is refreshed in
    /// place too (focus kept by id) and popped to the root only when its availability no longer
    /// holds ([`SubSig::page_availability`]: the renderer, or whether Style is shown / locked), so a
    /// Style page never outlives the renderer or the lock its Size and Position rows were built for
    /// while a track list or enhancement change it does not read leaves it alone.
    fn poll_subtitle_state(&mut self, ps: &crate::route::PlaybackSession, meta: metadata::MetadataView<'_>) {
        let live_sub = Self::derive_active_sub(ps, meta);
        let sig = self.sub_sig_for(ps, meta, live_sub);
        if self.sub_sig.as_ref() == Some(&sig) {
            return;
        }
        self.active_sub = live_sub;
        let Some(first) = self.pages.first().copied() else {
            let form = self.layout(ps, meta);
            // not the viewer's row any more (its track left the offered list): the checked row
            self.form.refresh_with(form, None, Some(&self.active_sub_id()));
            return;
        };
        // the pages' own data is read BEFORE judging them: a language whose entry is gone is as
        // unavailable as a changed renderer
        self.other = self.sub_model(ps, meta, true).other;
        let availability_moved =
            self.sub_sig.as_ref().is_some_and(|built| built.page_availability() != sig.page_availability());
        if availability_moved || !self.pages_hold() {
            // The page was opened on a root that no longer holds (the renderer changed under
            // Size/Position, a burn landed or left, the language it lists left the offered list):
            // pop to the root, restoring the opener and scroll the stack saved at its first push.
            self.pages.clear();
            let form = self.layout(ps, meta);
            self.form.restore(form, Some(&first.return_id), first.scroll);
            self.form.table.set_title(None);
            self.motion.cancel_slide();
        } else {
            // The page still holds: remember what the root is now built from (the pop back
            // rebuilds it) and refresh this page in place, focus kept by id.
            self.sub_sig = Some(sig);
            if let Some(page) = self.pages.last().map(|s| s.page) {
                let form = self.page_form(page);
                // a language can change shape under the viewer (one track <-> several): the row
                // they were on follows it to its new one
                let prefer = (page == TrackPage::OtherLanguages)
                    .then(|| self.form.selected_id().map(|id| self.other_row_for(*id)))
                    .flatten();
                self.form.refresh_with(form, prefer.as_ref(), None);
            }
        }
    }

    /// Does every pushed page still have what it lists? An Other languages page needs a language
    /// to list; a language page needs ITS language (the same [`LangId`], whatever its tracks'
    /// positions are now) still offered, with however many tracks it has left.
    fn pages_hold(&self) -> bool {
        self.pages.iter().all(|saved| match saved.page {
            TrackPage::OtherLanguages => !self.other.is_empty(),
            TrackPage::Language(stream) => self.other_lang(stream).is_some(),
            TrackPage::Style | TrackPage::Picker(_) => true,
        })
    }

    /// The Subtitles root's rebuild signature for `active`, read fresh off the route and the
    /// item. [`Self::layout`] stores the same value it builds from.
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

    /// The language entry of the Other languages model that [`LangId::stream`] names.
    fn other_lang(&self, stream: i64) -> Option<&OtherLang> {
        self.other.iter().find(|o| o.id.stream == stream)
    }

    /// The row that stands for a language on the Other languages page NOW: its direct pick row
    /// while it offers one track, its drill-in while it offers several. A language changes shape
    /// under a viewer (a sidecar becomes offered or leaves), and the row they were on must follow
    /// it, not be looked up in a shape that no longer exists. Any other id is returned unchanged.
    fn other_row_for(&self, id: TrackRow) -> TrackRow {
        let lang = match id {
            TrackRow::OpenLang(l) => self.other_lang(l.stream),
            TrackRow::Sub(i) => self.other.iter().find(|o| o.tracks.iter().any(|t| t.i == i)),
            _ => None,
        };
        lang.map_or(id, Self::other_lang_row)
    }

    /// A language's row on the Other languages page: the pick row of its only track, or its
    /// drill-in.
    fn other_lang_row(o: &OtherLang) -> TrackRow {
        match o.tracks.as_slice() {
            [t] => TrackRow::Sub(t.i),
            _ => TrackRow::OpenLang(o.id),
        }
    }

    /// The Subtitles ROOT's row carrying the checkmark: the active track, or Off — or, when the
    /// active track is not listed on the root at all (its language is not "yours": a codeless or
    /// unrecognised-language track, or one a live change moved), the Other languages row that
    /// stands for it.
    fn active_sub_id(&self) -> TrackRow {
        match self.active_sub {
            i if i >= 0 && self.other.iter().any(|o| o.tracks.iter().any(|t| t.i == i as usize)) => TrackRow::OpenOther,
            i if i >= 0 => TrackRow::Sub(i as usize),
            _ => TrackRow::SubOff,
        }
    }

    /// Focus an ABSOLUTE table row — the /tmp/plxnative-menupick trigger's contract ("row N"), the
    /// one dev-only entry that speaks positions (the named targets below do not). The interactive
    /// path always moves relatively; this exists because the initial focus is the ACTIVE row
    /// (derived from playback state), so a relative walk from it would land elsewhere.
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
    /// once hardcoded the Normalize Loudness row from a WRONG assumed track count. Reading it back
    /// through the form, the same identities [`Self::on_ok`] dispatches on, means the name
    /// is correct however many tracks the item actually has. `None` when `name` is unrecognized,
    /// or recognized but not currently built (the DSP toggle rows are not offered right now).
    pub(crate) fn row_for_audio_target(&self, name: &str) -> Option<c_int> {
        let target = match name {
            "boost" => TrackRow::Boost,
            "loudness" => TrackRow::Loudness,
            _ => return None,
        };
        self.form.index_of(&target).map(|i| i as c_int)
    }

    /// Resolve a NAMED Subtitles-tab target to a TRACK, for the `/tmp/plxnative-menupick` trigger:
    /// `"track:N"` is the N-th (0-based) track in PAGE ORDER — the root's track rows first, then the
    /// Other languages page A-Z with a multi-track language expanded into its own ranked page —
    /// never Off, Timing or Style. It answers the track's index in the item's list, not a table
    /// row: a track behind Other languages has no row on the page that is showing, and
    /// [`Self::commit_sub_track`] commits by that index. A hand-written row number drifts every
    /// time the panel gains or loses a row (the `subtitle_text_srt` case picked row 3, which became
    /// the Style row); a position in this order cannot. `None` for an unrecognized name or an N past
    /// the last track.
    pub(crate) fn sub_track_for_target(&self, name: &str) -> Option<usize> {
        let n: usize = name.strip_prefix("track:")?.trim().parse().ok()?;
        self.root_tracks.iter().copied().chain(self.other.iter().flat_map(|o| o.tracks.iter().map(|t| t.i))).nth(n)
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
    /// the container's job now, not this method's; the answer says whether it should. The row is
    /// read back by its [`TrackRow`] (never by position), and a row the form declared DISABLED (the
    /// dim Timing/Style under a live burn, a Size/Position row the renderer cannot reach) is inert
    /// at the form layer.
    pub(crate) fn on_ok(&mut self, meta: metadata::MetadataView<'_>) -> TrackOk {
        let tab = self.tab;
        let sel = self.form.table.sel;
        let focused = self.form.selected_id().copied();
        if tab == 0 {
            // The two Plex Pass DSP rows (issue #266), after the audio tracks — see
            // `Self::audio_form`, which declares them beside the tracks, so a track pick below is
            // never mistaken for one of these by position arithmetic.
            return match focused {
                // A `Disabled` pair is drawn dim, reading Off, and OK on it is a no-op — same
                // "focusable but inert" shape `TrackRow::Timing` already uses while subtitles are
                // Off. (The footnote under the pair is an inert slot: no key, never focused.)
                Some(TrackRow::Boost | TrackRow::Loudness) if self.enhance_disabled.is_some() => {
                    TrackOk::Inert
                }
                target @ (Some(TrackRow::Boost) | Some(TrackRow::Loudness)) => {
                    let mut a = self.enhance_shown.unwrap_or(crate::plex::AudioEnhancements::NONE);
                    if target == Some(TrackRow::Boost) {
                        a.boost_dialog = !a.boost_dialog;
                    } else {
                        a.normalize_loudness = !a.normalize_loudness;
                    }
                    self.enhance_shown = Some(a);
                    if let Some(row) = self.form.table.row_mut(sel) {
                        row.toggle = Some(if target == Some(TrackRow::Boost) {
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
                Some(TrackRow::Audio(i)) => {
                    let pick = i as c_int;
                    let changed = self.active_audio != pick;
                    self.active_audio = pick;
                    if changed {
                        // the menu only reports the pick — native-switch vs re-transcode is
                        // route's policy. The demuxer-facing index is the CONTAINER ordinal
                        // (audio_ordinal), not the row.
                        if let Some(s) = tracks(meta).and_then(|t| t.audio.get(i)) {
                            let ord = tracks(meta)
                                .map(|t| metadata::audio_ordinal(&t.audio, i))
                                .unwrap_or(pick);
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
                _ => TrackOk::Dismiss,
            };
        }

        let Some(id) = focused else { return TrackOk::Inert };
        match self.form.activate(sel.max(0) as usize) {
            // M7 follow-up: while the live route is actually burning a subtitle into the picture,
            // Timing and Style are declared disabled (`Self::layout`) — the text is already in
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
            TrackRow::Choice(field, rung) => self.pick_style(field, rung),
            // Read LIVE, not from the form: a pick in this same open (`active_sub` written below,
            // no rebuild) must make the next OK on Timing open the capsule.
            TrackRow::Timing if self.active_sub >= 0 => TrackOk::OpenTiming,
            TrackRow::Timing => TrackOk::Inert, // dim and inert while subtitles are Off
            TrackRow::SubOff => self.commit_sub(-1, meta),
            TrackRow::Sub(i) => self.commit_sub(i as c_int, meta),
            _ => TrackOk::Inert,
        }
    }

    /// Commit the subtitle track at `i` in the item's list as the active one, exactly as OK on its
    /// row does ([`Self::on_ok`]): by the track's own index, wherever its row sits (the root, the
    /// Other languages page or a language page), so a caller that names a track never needs the row
    /// to be on the current page.
    pub(crate) fn commit_sub_track(&mut self, i: usize, meta: metadata::MetadataView<'_>) -> TrackOk {
        self.commit_sub(i as c_int, meta)
    }

    /// The subtitle commit behind OK on Off (`-1`) or on a track row: records the pick, and answers
    /// the commit.
    fn commit_sub(&mut self, new_sub: c_int, meta: metadata::MetadataView<'_>) -> TrackOk {
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
            TrackPage::OtherLanguages => self.other_form(),
            TrackPage::Language(stream) => self.language_form(stream),
        }
    }

    /// The Other languages page: one row per language, A-Z. A single-track language is a direct
    /// pick row (checked when it is the active track); a multi-track one is a drill-in reading out
    /// its track count, or — when the active track is inside it — a check and that variant.
    fn other_form(&self) -> TrackForm {
        let active = self.active_sub;
        let sec = self.other.iter().fold(FormSection::new(""), |sec, lang| match lang.tracks.as_slice() {
            [t] => sec.item(TrackRow::Sub(t.i), RowKind::Choice, (), flat_row(t, active)),
            tracks => {
                let current = tracks.iter().find(|t| active >= 0 && t.i == active as usize);
                let value = match current {
                    Some(t) => in_lang_label(t),
                    None => crate::i18n::msg::widgets_tracks_count(tracks.len() as i64),
                };
                let row = Row::new(lang.name.clone()).value(value).checked(current.is_some());
                sec.item(TrackRow::OpenLang(lang.id), RowKind::Nav(TrackPage::Language(lang.id.stream)), (), row)
            }
        });
        Form::new().section(sec)
    }

    /// A language page: its tracks ranked full < SDH < forced < commentary, a pick row each.
    fn language_form(&self, stream: i64) -> TrackForm {
        let active = self.active_sub;
        let tracks = self.other_lang(stream).map(|o| o.tracks.as_slice()).unwrap_or_default();
        let sec = tracks.iter().fold(FormSection::new(""), |sec, t| {
            sec.item(TrackRow::Sub(t.i), RowKind::Choice, (), in_lang_row(t, active))
        });
        Form::new().section(sec)
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
                TrackRow::OpenField(StyleField::Size),
                RowKind::Nav(TrackPage::Picker(StyleField::Size)),
                (),
                field_row(StyleField::Size),
            )
            .disabled(!reaches)
            .item(
                TrackRow::OpenField(StyleField::Position),
                RowKind::Nav(TrackPage::Picker(StyleField::Position)),
                (),
                field_row(StyleField::Position),
            )
            .disabled(!reaches)
            .item(
                TrackRow::OpenField(StyleField::Color),
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
        let current = TrackRow::Choice(field, self.current_rung(field));
        let sec = (0..field.rungs()).fold(FormSection::new(""), |sec, rung| {
            sec.choice(TrackRow::Choice(field, rung), (), Row::new(field.rung_label(rung)), |id| *id == current)
        });
        Form::new().section(sec)
    }

    /// The explicit initial focus of a pushed page: the Style page opens on Size, a picker on its
    /// checked rung, Other languages on the checked row (the active track's, or its language's
    /// drill-in) else the first, a language page on the active variant if it is inside, else its
    /// first track. `None` (a language already gone) leaves it to the table's opening row.
    fn page_initial(&self, page: TrackPage) -> Option<TrackRow> {
        let active = usize::try_from(self.active_sub).ok();
        let holds_active = |o: &OtherLang| active.is_some_and(|a| o.tracks.iter().any(|t| t.i == a));
        match page {
            TrackPage::Style => Some(TrackRow::OpenField(StyleField::Size)),
            TrackPage::Picker(field) => Some(TrackRow::Choice(field, self.current_rung(field))),
            TrackPage::OtherLanguages => {
                self.other.iter().find(|o| holds_active(o)).or(self.other.first()).map(Self::other_lang_row)
            }
            TrackPage::Language(stream) => {
                let tracks = &self.other_lang(stream)?.tracks;
                let t = tracks.iter().find(|t| active == Some(t.i)).or(tracks.first())?;
                Some(TrackRow::Sub(t.i))
            }
        }
    }

    /// The title band's text of `page`.
    fn page_title(&self, page: TrackPage) -> String {
        page.title(&self.other)
    }

    /// Open `page` above the current one: remember the opener and its scroll, install the page's
    /// rows (the pill snaps, the scroll returns to the top, focus lands on
    /// [`Self::page_initial`]) and its title band.
    fn push(&mut self, page: TrackPage) {
        let Some(return_id) = self.form.selected_id().copied() else { return };
        self.pages.push(Saved { page, return_id, scroll: self.form.table.scroll_pos() });
        let leaving = self.leave_page();
        let form = self.page_form(page);
        self.form.open(form, self.page_initial(page).as_ref());
        // after the sections: the band moves every row, so the pill is re-jumped onto the focused one
        self.form.table.set_title(Some(self.page_title(page)));
        self.motion.begin_slide(leaving, 1.0);
    }

    /// Take the page that is showing OUT of the form, whole, leaving a blank table of the same
    /// kind for the next page to be built into: the page slide draws the old one once more, so it
    /// is moved, never cloned.
    fn leave_page(&mut self) -> TableView {
        let blank = self.form.table.blank_like();
        std::mem::replace(&mut self.form.table, blank)
    }

    /// Pop the top page: the page beneath comes back exactly as it was left — the opener focused
    /// by id, the scroll reinstated ([`FormTable::restore`]). `false` at the root (nothing popped).
    pub(crate) fn pop(&mut self, ps: &crate::route::PlaybackSession, meta: metadata::MetadataView<'_>) -> bool {
        let Some(saved) = self.pages.pop() else { return false };
        let leaving = self.leave_page();
        self.motion.begin_slide(leaving, -1.0);
        match self.pages.last().map(|s| s.page) {
            None => {
                let form = self.layout(ps, meta);
                self.form.restore(form, Some(&saved.return_id), saved.scroll);
                self.form.table.set_title(None);
            }
            Some(page) => {
                let form = self.page_form(page);
                // the opener may have changed shape while its page was open: a language that
                // dropped to one track is a direct row now (and the reverse)
                let return_id =
                    if page == TrackPage::OtherLanguages { self.other_row_for(saved.return_id) } else { saved.return_id };
                self.form.restore(form, Some(&return_id), saved.scroll);
                self.form.table.set_title(Some(self.page_title(page)));
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

    /// The Audio tab as a form: the track list, plus — whenever [`Self::enhance_shown`] is `Some`
    /// OR [`Self::enhance_disabled`] is `Some` (every gate but no Plex Pass is now a visible
    /// reason, owner direction 2026-09-29) — a second, headerless section carrying the two Plex
    /// Pass DSP toggles (enabled, or dim with their reason), the same "own section, no header"
    /// idiom the Subtitles tab's Timing/Style pair uses, plus an optional non-selectable note (an
    /// inert slot: it takes a layout row and has no identity). Mirrors [`Self::layout`] for the
    /// Subtitles tab.
    fn audio_form(&self, meta: metadata::MetadataView<'_>) -> TrackForm {
        self.audio_form_for(
            meta,
            (self.enhance_shown, self.enhance_route, self.enhance_disabled, self.enhance_subtitle_effect),
        )
    }

    /// [`Self::audio_form`] against an explicit enhancement answer ([`Self::enh_state`]'s shape),
    /// so the Audio root can be built while the menu shows Subtitles and has not stored one.
    fn audio_form_for(&self, meta: metadata::MetadataView<'_>, enh: EnhState) -> TrackForm {
        let (enhance_shown, enhance_route, enhance_disabled, enhance_subtitle_effect) = enh;
        let mut sec = FormSection::new(crate::i18n::msg::widgets_tracks_audio());
        let d = match tracks(meta) {
            Some(t) => t,
            None => return Form::new().section(sec),
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
            sec = sec.item(TrackRow::Audio(i), RowKind::Choice, (), row);
        }
        let mut form = Form::new().section(sec);
        if let Some(shown) = enhance_shown {
            let mut enh = FormSection::new("")
                .item(
                    TrackRow::Boost,
                    RowKind::Toggle,
                    (),
                    Row::new(crate::i18n::msg::widgets_tracks_boost_dialog()).toggle(shown.boost_dialog),
                )
                .item(
                    TrackRow::Loudness,
                    RowKind::Toggle,
                    (),
                    Row::new(crate::i18n::msg::widgets_tracks_normalize_loudness()).toggle(shown.normalize_loudness),
                );
            if let Some(route) = enhance_route {
                if let Some(note) = Self::enh_note_text(route, enhance_subtitle_effect) {
                    enh = enh.note(note);
                }
            }
            form = form.section(enh);
        } else if let Some(reason) = enhance_disabled {
            // Every gate but "no Plex Pass" is now a visible reason (owner direction, 2026-09-29):
            // the rows stay in the list, dim and reading Off, with a one-line non-selectable
            // footnote naming why. "No Plex Pass" is the ONE absence that stays a silent gap (I1/I2).
            let enh = FormSection::new("")
                .item(
                    TrackRow::Boost,
                    RowKind::Toggle,
                    (),
                    Row::new(crate::i18n::msg::widgets_tracks_boost_dialog()).toggle(false).dim(true),
                )
                .item(
                    TrackRow::Loudness,
                    RowKind::Toggle,
                    (),
                    Row::new(crate::i18n::msg::widgets_tracks_normalize_loudness()).toggle(false).dim(true),
                )
                .note(Self::enh_reason_text(reason));
            form = form.section(enh);
        }
        form
    }

    /// Build the Subtitles root's form from the CURRENT state — the one place the model
    /// (`metadata::sub_layout::sub_sections`) is asked, so what a row IS can never disagree with
    /// what was drawn. Also stores the [`SubSig`] and renderer the root is built from, for the
    /// poll and the Style page's locks.
    fn layout(&mut self, ps: &crate::route::PlaybackSession, meta: metadata::MetadataView<'_>) -> TrackForm {
        // M7 follow-up: while the live route is actually burning a subtitle in, Timing and Style
        // stay drawn (dim, with a reason) instead of being omitted the way an ordinary
        // non-enhancement transcode omits both — a viewer who turned the enhancement on must still
        // see why the control they had is gone, not just find it missing.
        let sig = self.sub_sig_for(ps, meta, self.active_sub);
        self.renderer = sig.renderer;
        self.sub_sig = Some(sig);
        let (form, model) = self.sub_root_form(ps, meta);
        self.root_tracks = model
            .sections
            .iter()
            .flat_map(|sec| &sec.rows)
            .filter_map(|row| match row {
                SubRow::Flat(t) | SubRow::InLanguage(t) => Some(t.i),
                _ => None,
            })
            .collect();
        self.other = model.other;
        form
    }

    /// The Subtitles root's form and the model it was built from, storing nothing: [`Self::layout`]
    /// keeps what it needs from the model, [`Self::warm_other_tab`] only draws the form.
    fn sub_root_form(&self, ps: &crate::route::PlaybackSession, meta: metadata::MetadataView<'_>) -> (TrackForm, SubModel) {
        let locked = crate::route::live_is_own_burn(ps);
        let show_timing = !crate::route::is_transcoding(ps) || locked;
        let model = self.sub_model(ps, meta, show_timing);
        (table_form(&model, self.active_sub, self.offset_ms, locked), model)
    }

    /// The Subtitles model for the current item and route — the one place the root and the pages
    /// behind it are asked for, so what the root's Other languages row promises and what its page
    /// lists come from the same answer.
    fn sub_model(&self, ps: &crate::route::PlaybackSession, meta: metadata::MetadataView<'_>, show_timing: bool) -> SubModel {
        let subs: &[metadata::Stream] = tracks(meta).map(|t| t.subs.as_slice()).unwrap_or(&[]);
        let offered = visible_subs(ps, meta);
        let names = crate::player::SHARED.track_names.lock().unwrap();
        sub_layout::sub_sections(subs, &offered, &names, &self.yours, show_timing)
    }

    /// The [`TrackRow::Boost`]/[`TrackRow::Loudness`] pair's three inputs, read fresh
    /// off the live route in one place — [`Self::rebuild`] and [`Self::update`] both need exactly
    /// this triple, and computing it once here keeps them from independently re-deriving it (and
    /// risking disagreement).
    fn enh_state(ps: &crate::route::PlaybackSession) -> EnhState {
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

    fn rebuild(&mut self, ps: &crate::route::PlaybackSession, meta: metadata::MetadataView<'_>, tab: c_int, slide: bool) {
        if tab == 0 {
            let (shown, route, disabled, subtitle_effect) = Self::enh_state(ps);
            self.rebuild_audio(shown, route, disabled, subtitle_effect, meta, slide);
        } else {
            let form = self.layout(ps, meta);
            let keep = self.active_sub_id();
            if slide {
                self.form.set_sliding(form, Some(&keep));
            } else {
                self.form.set(form, Some(&keep));
            }
        }
        // an open or a tab switch always lands on a tab's root; the card resizes to it, the
        // content just stops sliding
        self.pages.clear();
        self.form.table.set_title(None);
        self.motion.cancel_slide();
    }

    /// The Audio tab's half of [`Self::rebuild`], taking the offer/displayed answer rather than
    /// recomputing it — `update`'s per-frame poll already has it fresh, and handing it here keeps
    /// [`Self::enh_state`] to exactly one call per rebuild instead of two.
    ///
    /// **Preserves the focused row by its [`TrackRow`] identity** rather than always
    /// snapping to the checked track — the fix for a reported focus desync: `update`'s live poll
    /// calls this every time the route's enhancement answer changes (the server settling an
    /// optimistic Boost/Loudness flip, or a mid-play route change), and that used to re-home
    /// the table's cursor (and the drawn pill) onto the active-audio row unconditionally. The
    /// ENGINE's own cursor only ever moves in response to a `FocusMoved`
    /// (`screens::player::overlay::PlayerOverlayScreen::step`'s write via [`Self::focus_key`]), and
    /// a poll-driven rebuild fires no such event — so the highlight jumped to the checked language
    /// row while the engine's focus stayed on the toggle row the viewer was actually on, and the
    /// next UP/DOWN/OK acted on a row nothing showed as selected. Looking the previous row up by
    /// its [`TrackRow`] and reusing its NEW position keeps the table's cursor exactly where the
    /// engine still thinks it is whenever that row still exists (the common case: toggling a
    /// bit does not remove or reorder rows). A tab switch/open always reaches here too, but the
    /// form then holds the OTHER tab's rows (or none), so the lookup misses and the fallback
    /// below — landing on the checked track — is exactly the existing open/switch behaviour.
    fn rebuild_audio(
        &mut self,
        enhance_shown: Option<crate::plex::AudioEnhancements>,
        enhance_route: Option<crate::route::EnhancementRoute>,
        enhance_disabled: Option<crate::route::DisabledReason>,
        enhance_subtitle_effect: crate::route::SubtitleEffect,
        meta: metadata::MetadataView<'_>,
        slide: bool,
    ) {
        let prev_target = self.form.selected_id().copied();
        // The offer is about to VANISH (Some -> None): bank the toggle row identity before the
        // rebuild drops it, so a later return restores it instead of reading the table's cursor
        // back — see `Self::sticky_audio_target`'s own doc for why that would be wrong.
        if self.enhance_shown.is_some() && enhance_shown.is_none() {
            if let Some(t @ (TrackRow::Boost | TrackRow::Loudness)) = prev_target {
                self.sticky_audio_target = Some(t);
            }
        }
        self.enhance_shown = enhance_shown;
        self.enhance_route = enhance_route;
        self.enhance_disabled = enhance_disabled;
        self.enhance_subtitle_effect = enhance_subtitle_effect;
        let form = self.audio_form(meta);
        // The offer is back: prefer the banked identity over `prev_target` (which names whatever
        // row the cursor happened to sit on while the rows were gone) whenever it still exists.
        let restored = if enhance_shown.is_some() { self.sticky_audio_target.take() } else { None };
        let keep = restored
            .or(prev_target)
            .filter(|t| form.contains(t))
            .unwrap_or(TrackRow::Audio(self.active_audio().max(0) as usize));
        if slide {
            self.form.set_sliding(form, Some(&keep));
        } else {
            self.form.set(form, Some(&keep));
        }
    }

    /// The panel's NATURAL geometry — the layout of the page that is showing, shared by `update`,
    /// `draw` and the focus queries so scrolling math matches. Cached against the table's
    /// [`TableView::layout_rev`]: text is measured when the table changed, not once per caller per
    /// frame. What is on screen is [`Self::shown_rect`], which springs toward this.
    fn panel_rect(&self, measure: &dyn crate::ui::machine::Measure) -> Rect {
        self.motion.natural(self.form.table.layout_rev(), || table_natural(&self.form.table, measure))
    }

    /// The card as it is drawn this frame: its top and left edges on their springs toward
    /// [`Self::panel_rect`], the bottom and right on the anchor.
    fn shown_rect(&self, measure: &dyn crate::ui::machine::Measure) -> Rect {
        self.motion.shown(self.panel_rect(measure))
    }

    /// **Is the panel mid-transition** — resizing to a new layout or sliding a page? The player
    /// overlay holds the pointer while this is true (`Screen::pointer_held`): hover and clicks are
    /// swallowed rather than resolved against pages in motion.
    pub(crate) fn transitioning(&self) -> bool {
        self.motion.transitioning()
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
                self.rebuild_audio(shown, route, disabled, subtitle_effect, meta, false);
            }
        } else {
            self.poll_subtitle_state(ps, meta);
        }
        // `update` subtracts its own top/bottom padding now — pass the panel's raw height.
        let natural = self.panel_rect(measure);
        self.form.table.update(dt, natural.h);
        self.motion.step(dt, natural);
        self.motion.prewarm_text(natural, &self.form.table, measure);
        self.warm_other_tab(ps, meta, measure);
    }

    /// **Queue the OTHER tab's root strings once the panel is idle**, so a tab switch does not
    /// meet them cold. The live page's prewarm cannot cover a switch: the key rebuilds the table
    /// after this frame's `update`, so its draw rasterised the new tab's strings itself, on the
    /// TV `textx8:9.5` inside a 24.8–29.0 ms frame on the first switch to Audio (2026-10-01).
    /// Done once per menu, on a frame with no page slide, no resize and an empty queue, so it
    /// neither competes with the live page's own strings nor adds to the open frame. The work is
    /// only building the form and recording it; the presenting side's drain uploads it, one
    /// string a frame with no live strings queued ([`PanelMotion::prewarm_background_text`]).
    /// Nothing waits on that queue, so a sub-page walked meanwhile only delays it; a closed menu
    /// drops it ([`Drop`]).
    fn warm_other_tab(
        &mut self,
        ps: &crate::route::PlaybackSession,
        meta: metadata::MetadataView<'_>,
        measure: &dyn crate::ui::machine::Measure,
    ) {
        if self.other_tab_warmed
            || !self.pages.is_empty()
            || self.motion.transitioning()
            || crate::text::prewarm_pending()
        {
            return;
        }
        self.other_tab_warmed = true;
        let form = if self.tab == 0 {
            self.sub_root_form(ps, meta).0
        } else {
            self.audio_form_for(meta, Self::enh_state(ps))
        };
        let mut other = TrackTable::new(BAND_BASE);
        other.table.min_panel_w = theme::layout::PLAYER_MENU_MIN_W;
        other.set(form, None);
        let natural = table_natural(&other.table, measure);
        PanelMotion::prewarm_background_text(natural, &other.table, measure);
    }

    pub(crate) fn draw(&mut self, appear: f32, measure: &dyn crate::ui::machine::Measure) {
        // The appear fade/rise — the container drives the phase and the appear spring. The dim
        // over the video plane is the container's too (`PlayerOverlayScreen::scrim`,
        // `theme::underlay::DIM_PLAYER`), painted at the end of the player's page pass.
        let p = Painter::root()
            .alpha(appear)
            .translate(0.0, Popover::RISE * (1.0 - appear));
        // frosted panel card — near-opaque dark (no true backdrop blur on the GLES plane, so a solid
        // dark card approximates it); only a hint of video shows through. ONE background at the
        // animated rect, the page(s) under its clip (`PanelMotion::draw`).
        self.motion.draw(p, self.panel_rect(measure), 28.0, &self.form.table, measure);
    }
}

/// **The Engine-shaped view of this popover** (restructure phase 12): one `Column` focus group
/// over the ACTIVE tab's rows, built fresh by `screens::player::overlay::PlayerOverlayScreen`
/// each frame from a `&TrackMenuState` — the same borrowed-view shape `ui::more_menu::MoreMenuPart`
/// and `ui::table_screen::TablePart` use for the other bare-`TableView` panels, so this popover
/// answers the same [`Focusable`]/[`Part`] query protocol they do. LEFT/RIGHT are NOT a move
/// within the group — LEFT pops a sub-page, else switches the whole row set to the other tab, and
/// RIGHT enters a Nav row, else switches tab; both replace the rows, which only the owning screen
/// can do (mirroring [`TrackMenuState::on_left`] / [`TrackMenuState::on_right`]), so both edges answer
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
            extent: self.state.panel_rect(_cx.measure),
            len: self.state.form.focusable_len(),
            elem: ElemKind::Bare,
        });
    }
    fn group_of(&self, key: &H::Elem, _cx: &Cx<'_, H>) -> Option<GroupId> {
        self.state.form.index_of_key(RowKey(key.index()?)).map(|_| self.group)
    }
    fn neighbour(&self, key: FocusKey<H::Elem>, dir: Dir, _cx: &Cx<'_, H>) -> Step<H::Elem> {
        let Some(from) = key.elem.index() else {
            return Step::Edge;
        };
        let delta = match dir {
            Dir::Up => -1,
            Dir::Down => 1,
            _ => return Step::Edge, // Left/Right: the screen's own tab switch, via `EdgeRule::Screen`
        };
        match self.state.form.step_key(RowKey(from), delta) {
            Some(k) => Step::Move(FocusKey { entry: self.entry, elem: H::Elem::of_index(k.0) }),
            None => Step::Edge,
        }
    }
    fn place(&self, key: &H::Elem, cx: &Cx<'_, H>, _at: At) -> Option<Placed> {
        // Where the stop is DRAWN: the page's slide offset on x and the animated card as the clip,
        // the same numbers `Part::draw` registers (replay compares the two). `rest_rect` is the
        // layout, where the row settles.
        let natural = self.state.panel_rect(cx.measure);
        let clip = self.state.shown_rect(cx.measure);
        let dx = self.state.motion.live_dx();
        let placed = |rest: Rect, index: Option<u32>| Placed {
            rect: Rect::new(rest.x + dx, rest.y, rest.w, rest.h),
            rest_rect: rest,
            clip,
            index,
        };
        // the title band's pointer-only key: not a row, but replay and hit validation must be able
        // to place it (`docs/player-submenus.md`)
        if key.index() == Some(TITLE_KEY) {
            return Some(placed(self.state.form.table.title_rect(natural)?, None));
        }
        let i = self.state.form.index_of_key(RowKey(key.index()?))? as u32;
        Some(placed(self.state.form.table.row_frame(natural, i as i32)?, Some(i)))
    }
    fn reconcile(&self, want: FocusKey<H::Elem>, _cx: &Cx<'_, H>) -> FocusKey<H::Elem> {
        // Trust the panel's OWN cursor (`table.sel`), not `want`. The cursor is where
        // `rebuild`/`rebuild_audio` already decided focus belongs — including a live poll rebuild
        // that drops and later restores the enhancement rows (issue #266's follow-up,
        // `Self::sticky_audio_target`) — so it is the identity-aware answer; `want` may name a row
        // of the OTHER tab (a tab switch replaces the whole form) or one that has left, and there is
        // no neighbour of a key to slide to. The cursor's row is reported by its KEY.
        let _ = want;
        let table = &self.state.form.table;
        let key = self
            .state
            .form
            .key_at(table.settle(table.sel).max(0) as usize)
            .or_else(|| self.state.form.opening_key());
        FocusKey { entry: self.entry, elem: H::Elem::of_index(key.map_or(0, |k| k.0)) }
    }
    fn seat(&self, _g: GroupId, _from: Placed, _cx: &Cx<'_, H>) -> FocusKey<H::Elem> {
        let key = self.state.form.selected_key().or_else(|| self.state.form.opening_key());
        FocusKey { entry: self.entry, elem: H::Elem::of_index(key.map_or(0, |k| k.0)) }
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
        let r = self.state.panel_rect(f.measure);
        // only the ACTIVE page registers stops, where it is drawn this frame: the slide's x offset
        // and the animated card as the clip (`PanelMotion`); the pointer is held while either moves
        let dx = self.state.motion.live_dx();
        let clip = self.state.shown_rect(f.measure);
        let moved = |rect: Rect| Rect::new(rect.x + dx, rect.y, rect.w, rect.h);
        for i in 0..self.state.form.table.n_rows() {
            let Some(key) = self.state.form.key_at(i as usize) else {
                continue;
            };
            if let Some(row) = self.state.form.table.row_frame(r, i) {
                f.stop(
                    p,
                    Stop {
                        key: FocusKey {
                            entry: self.entry,
                            elem: H::Elem::of_index(key.0),
                        },
                        rect: moved(row),
                        rest_rect: row,
                        clip,
                        hover: Hover::Focus,
                        activate: Activate::Direct,
                    },
                );
            }
        }
        // "< STYLE": a click pops. A pointer-only stop — no hover focus, never in the D-pad column.
        if let Some(band) = self.state.form.table.title_rect(r) {
            f.stop(
                p,
                Stop {
                    key: FocusKey { entry: self.entry, elem: H::Elem::of_index(TITLE_KEY) },
                    rect: moved(band),
                    rest_rect: band,
                    clip,
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
        RowBadge::Codec(c) => Badge::Text((*c).to_string()),
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
    for b in &t.badges {
        row = row.badge(row_badge(b));
    }
    row
}

/// A row inside a multi-track "yours" language section (`player.html:1110-1115`): label = the
/// source (+ "Track N" if needed), or — for a NAMELESS track — the kind word itself ("Forced",
/// "SDH", "Full", "Commentary"), in which case a Forced/SDH badge that would only repeat the
/// label is dropped.
fn in_lang_row(t: &SubTrack, active_sub: c_int) -> Row {
    let mut row = Row::new(in_lang_label(t)).checked(active_sub >= 0 && t.i == active_sub as usize);
    // a Forced/SDH chip that would only repeat the kind word the label already is, is dropped; the
    // image codec never is
    let label_is_kind = t.detail.is_empty() && t.ordinal.is_none();
    for b in &t.badges {
        if !(label_is_kind && matches!(b, RowBadge::Forced | RowBadge::Sdh)) {
            row = row.badge(row_badge(b));
        }
    }
    row
}

/// A track's label inside its language: the source (+ "Track N" if needed), or the kind word for a
/// nameless one. Also the value a language's drill-in reads out for its active variant.
fn in_lang_label(t: &SubTrack) -> String {
    let nth = t.ordinal.map(|n| crate::i18n::msg::widgets_tracks_track_ordinal(n as i64));
    if !t.detail.is_empty() {
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
    }
}

/// **Declare the Subtitles root** from its model (`metadata::sub_layout::sub_sections`) as a keyed
/// form — the catalog words for each header, the checkmark on `active_sub` (-1 for Off), and the
/// Timing read-out (`offset_ms`). Each row is declared once: a track carries its subs-list index
/// as its id and key; Timing is dim while subtitles are Off and hands off to the capsule (so it has
/// no chevron); Style and Other languages are Nav rows, the drill-ins to [`TrackPage::Style`] and
/// [`TrackPage::OtherLanguages`] (the latter reads out the language count, or — when the active
/// track lives behind it — a check and that track's language).
///
/// `locked` (M7 follow-up): while the live route is actually burning a subtitle into the picture,
/// Timing and Style do nothing — the text is already in the pixels — so they are DISABLED (dim,
/// focusable so the viewer can read why, inert under OK and RIGHT at the form layer) and one
/// non-selectable [`FormSection::note`] naming why follows Style, the same "visible, dim, plain
/// reason" idiom the Audio tab's own Boost dialog / Normalize loudness rows use when THEY are
/// disabled.
fn table_form(model: &SubModel, active_sub: c_int, offset_ms: i64, locked: bool) -> TrackForm {
    use crate::i18n::msg;
    let active_other =
        model.other.iter().find(|o| o.tracks.iter().any(|t| active_sub >= 0 && t.i == active_sub as usize));
    model.sections.iter().fold(Form::new(), |form, sec| {
        let head = match &sec.header {
            SubHeader::Subtitles => Section::new(msg::widgets_tracks_subtitles()),
            SubHeader::Language { name, tracks } => {
                Section::new(name.clone()).accessory(msg::widgets_tracks_count(*tracks as i64))
            }
            SubHeader::Bare => Section::new(""),
        };
        let out = sec.rows.iter().fold(FormSection::from_head(head), |out, row| {
            // the identity comes from the model's own row target, never from the position
            let id = TrackRow::from(row.target());
            match row {
                SubRow::Off => out.item(
                    id,
                    RowKind::Choice,
                    (),
                    Row::new(msg::widgets_tracks_off()).checked(active_sub < 0),
                ),
                SubRow::Flat(t) => out.item(id, RowKind::Choice, (), flat_row(t, active_sub)),
                SubRow::InLanguage(t) => out.item(id, RowKind::Choice, (), in_lang_row(t, active_sub)),
                SubRow::OtherLanguages { languages } => {
                    let (value, checked) = match active_other {
                        Some(o) => (o.name.clone(), true),
                        None => (languages.to_string(), false),
                    };
                    out.item(
                        id,
                        RowKind::Nav(TrackPage::OtherLanguages),
                        (),
                        Row::new(msg::widgets_tracks_other_languages()).value(value).checked(checked),
                    )
                }
                SubRow::Timing => out
                    .item(
                        id,
                        RowKind::Button,
                        (),
                        Row::new(msg::widgets_tracks_timing()).value(format_offset(offset_ms)).dim(active_sub < 0),
                    )
                    .disabled(locked),
                SubRow::Style => {
                    let out = out
                        .item(id, RowKind::Nav(TrackPage::Style), (), Row::new(msg::widgets_tracks_style()))
                        .disabled(locked);
                    if locked {
                        out.note(msg::widgets_tracks_style_locked_note())
                    } else {
                        out
                    }
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
    use crate::player::TrackNames;
    use crate::ui::table::TableView;

    /// The focus element of the `i`-th Audio row — a test names a row by its identity, never by
    /// where it happens to sit.
    fn audio_key(i: usize) -> u32 {
        TrackRow::Audio(i).key().0
    }

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
    ) -> (Vec<Section>, Vec<Option<TrackRow>>) {
        let yours: Vec<String> = yours.iter().map(|y| y.to_string()).collect();
        let model = sub_layout::sub_sections(subs, offered, names, &yours, show_timing);
        let mut built = FormTable::new(crate::ui::table_screen::BAND_BASE);
        built.set(table_form(&model, active_sub, offset_ms, false), None);
        let ids = (0..built.table.n_rows() as usize).map(|i| built.id_at(i).copied()).collect();
        (std::mem::take(&mut built.table.sections), ids)
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
        assert_eq!(targets[1], Some(TrackRow::Sub(0)));
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
    fn one_kind_badge_per_row_by_priority_forced_over_sdh_over_external_and_an_image_codec_always_adds_its_own() {
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
        let (sections, _) = sub_layout(&subs, &[0], &names, &["eng"], -1, true, 0);
        assert!(matches!(sections[0].rows[1].badges.as_slice(), [Badge::Sdh]));

        let subs = vec![mk(false, true, "srt")];
        let (sections, _) = sub_layout(&subs, &[0], &names, &["eng"], -1, true, 0);
        assert!(matches!(sections[0].rows[1].badges.as_slice(), [Badge::Text(t)] if t == "EXTERNAL"));

        let subs = vec![mk(true, true, "srt")];
        let (sections, _) = sub_layout(&subs, &[0], &names, &["eng"], -1, true, 0);
        assert!(
            matches!(sections[0].rows[1].badges.as_slice(), [Badge::Sdh]),
            "SDH beats EXTERNAL"
        );

        let subs = vec![mk(false, false, "pgs")];
        let (sections, _) = sub_layout(&subs, &[0], &names, &["eng"], -1, true, 0);
        assert!(matches!(sections[0].rows[1].badges.as_slice(), [Badge::Text(t)] if t == "PGS"));

        // an image subtitle keeps its format whatever else it is: the codec chip follows the kind
        let subs = vec![mk(true, false, "pgs")];
        let (sections, _) = sub_layout(&subs, &[0], &names, &["eng"], -1, true, 0);
        assert!(
            matches!(sections[0].rows[1].badges.as_slice(), [Badge::Sdh, Badge::Text(t)] if t == "PGS"),
            "SDH + PGS"
        );
        // a text subtitle never shows a format, external or not
        let subs = vec![mk(false, true, "ass")];
        let (sections, _) = sub_layout(&subs, &[0], &names, &["eng"], -1, true, 0);
        assert!(matches!(sections[0].rows[1].badges.as_slice(), [Badge::Text(t)] if t == "EXTERNAL"));
    }

    // ---- sub_layout: "Other languages" is ONE drill-in row on the root -------------------------

    #[test]
    fn other_languages_are_one_chevron_row_reading_the_language_count() {
        let subs = vec![
            stream(1, 0, "German", "deu", ""),
            stream(2, 1, "Arabic", "ara", ""),
            stream(3, 2, "Arabic", "ara", "SDH"),
        ];
        let offered: Vec<usize> = (0..subs.len()).collect();
        let (sections, ids) = sub_layout(&subs, &offered, &TrackNames::new(), &[], -1, true, 0);
        assert_eq!(ids, [Some(TrackRow::SubOff), Some(TrackRow::OpenOther), Some(TrackRow::Timing), Some(TrackRow::Style)]);
        let row = &sections[1].rows[0];
        assert_eq!((row.label.as_str(), row.value.as_deref()), ("Other languages", Some("2")), "two languages, three tracks");
        assert!(!row.checked, "no active track behind it");
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
        let menu = TrackMenuState::new(&ps, store.view(), 1, vec!["eng".into()]);
        assert_eq!(
            menu.ids(),
            vec![TrackRow::SubOff, TrackRow::Sub(0), TrackRow::Timing, TrackRow::Style]
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
        assert_eq!(menu.form.selected_id().copied(), Some(TrackRow::Sub(1)));
    }

    /// `sub_track_for_target("track:N")` names the N-th TRACK, never Off/Timing/Style: the
    /// `subtitle_text_srt` manifest case once hard-coded row 3, which stopped being a track when
    /// the panel's layout changed (it resolved to Style and committed nothing).
    #[test]
    fn sub_track_for_target_finds_tracks_and_skips_off_timing_and_color() {
        let _g = crate::testlock::serial();
        crate::player::sidecar::reset();
        let ps = crate::route::PlaybackSession::IDLE;
        let store = store_with(vec![
            stream(1, 0, "English", "eng", "A"),
            stream(2, 1, "French", "fra", "B"),
        ]);
        let menu = TrackMenuState::new(&ps, store.view(), 1, vec!["eng".into(), "fra".into()]);
        assert_eq!(menu.sub_track_for_target("track:0"), Some(0));
        assert_eq!(menu.sub_track_for_target("track:1"), Some(1));
        assert_eq!(menu.sub_track_for_target("track:2"), None, "past the last track");
        assert_eq!(menu.sub_track_for_target("boost"), None);
        assert_eq!(menu.sub_track_for_target("track:x"), None);
    }

    // ---- track_menu: sidecar, tone and Timing rows dispatch to their own outcomes -------------

    /// **A Subtitles-panel row is a track, Style, or Timing, never ambiguous — and the split is
    /// by `targets[sel]`.** Off + an embedded English track + an external French sidecar (both
    /// "yours", so both land flat under "Subtitles"), then the headerless Timing/Style section.
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
        let mut menu = TrackMenuState::new(&ps, store.view(), 1, vec!["eng".into(), "fre".into()]);
        // Off(0), English(1), French sidecar(2), then the headerless Timing(3) / Style(4) section
        assert_eq!(
            menu.ids(),
            vec![
                TrackRow::SubOff,
                TrackRow::Sub(0),
                TrackRow::Sub(1),
                TrackRow::Timing,
                TrackRow::Style,
            ]
        );

        menu.focus_row(2);
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

        menu.focus_row(3);
        assert_eq!(
            menu.on_ok(store.view()),
            TrackOk::OpenTiming,
            "the sidecar is now the active subtitle, so Timing is no longer inert"
        );

        // Style -> Color -> the second tone: a Nav row pushes, a choice commits and the panel stays
        menu.focus_row(4);
        assert_eq!(menu.on_ok(store.view()), TrackOk::Navigated);
        assert_eq!(menu.on_ok(store.view()), TrackOk::Navigated, "Size is first and live for a text subtitle");
        assert_eq!(menu.page_path(), [TrackPage::Style, TrackPage::Picker(StyleField::Size)]);
        assert!(menu.pop(&ps, store.view()));
        menu.focus_key(TrackRow::OpenField(StyleField::Color).key().0);
        assert_eq!(menu.on_ok(store.view()), TrackOk::Navigated);
        menu.focus_key(TrackRow::Choice(StyleField::Color, 1).key().0);
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
        let mut menu = TrackMenuState::new(&ps, store.view(), 1, vec!["eng".into()]);
        let timing_row = menu
            .form
            .index_of(&TrackRow::Timing)
            .expect("Timing row");

        menu.focus_row(timing_row as c_int);
        assert_eq!(menu.on_ok(store.view()), TrackOk::Inert, "subtitles are Off: inert");

        let sub_row = menu
            .ids()
            .iter()
            .position(|t| matches!(t, TrackRow::Sub(_)))
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

    /// [`TrackRow`]'s Audio-tab identities, with no enhancement offered: one [`TrackRow::Audio`]
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
            let want: Vec<TrackRow> = (0..n).map(TrackRow::Audio).collect();
            assert_eq!(menu.ids(), want, "n={n}");
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
            menu.ids(),
            vec![TrackRow::Audio(0), TrackRow::Audio(1), TrackRow::Boost, TrackRow::Loudness],
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

    /// **A rebuilt page's strings are rasterised in `update`, ahead of the draw.** On the player
    /// route the draw's first framebuffer command waits ~15 ms for a free back buffer, so text a
    /// new page met cold in its draw stacked on top of that wait: a 26.7 ms frame on the TV
    /// (`clear:12.8 … textx8:7.4`) on the first visit to each Tracks sub-page. `PanelMotion::
    /// prewarm_text` walks the live table through the recorder in `update`, once per table layout;
    /// the queue is drained on the presenting side (`app::run::prepare_window`) and NEVER in
    /// `update`, because a frame that does not present uploads nothing (spec §10).
    #[test]
    fn update_rasterises_the_live_pages_text_before_the_draw() {
        use crate::ui::fixture::FixtureMeasure as M;
        let _g = crate::testlock::serial();
        let (mut menu, ps) = audio_tab(EnhTestFixture::default());
        let store = one_track_store();
        crate::text::reset_prewarm_for_test();
        // a held modal's leftover must not eat the drain's budget
        crate::text::queue_prewarm(c"stale held string".as_ptr(), 24, 0);
        menu.update(0.0, &M, &ps, store.view());
        let labels: Vec<String> =
            menu.form.table.sections.iter().flat_map(|s| s.rows.iter()).map(|r| r.label.clone()).collect();
        assert!(!labels.is_empty(), "premise: the Audio tab has rows");
        // `update` alone is a frame that may not present: it records, it uploads nothing.
        assert!(crate::text::prewarm_pending(), "update queued the page's strings");
        for label in &labels {
            assert!(
                !crate::text::prewarm_resident_any_size_for_test(label.as_bytes()),
                "{label:?} was uploaded by update, which may run on a frame that does not present"
            );
        }
        // The presenting side's drain rasterises them, up to its counted budget.
        crate::ui::panel_motion::PanelMotion::drain_queued_text();
        let resident =
            labels.iter().filter(|l| crate::text::prewarm_resident_any_size_for_test(l.as_bytes())).count();
        assert!(resident > 0, "the drain rasterised none of the page's strings");
        assert!(
            !crate::text::prewarm_resident_any_size_for_test(b"stale held string"),
            "the walk's queue must replace, not extend, what a held surface left"
        );

        // The same layout is walked once, not every frame.
        crate::text::reset_prewarm_for_test();
        menu.update(0.016, &M, &ps, store.view());
        assert!(
            !crate::text::prewarm_resident_any_size_for_test(labels[0].as_bytes()),
            "an unchanged layout was walked again"
        );
        teardown(&ps);
    }

    /// **An idle menu has the OTHER tab's strings resident before the first switch.** The key
    /// rebuilds the table after the frame's `update`, so the live page's prewarm cannot cover a
    /// tab switch: on the TV the first switch to Audio drew `textx8:9.5` in a 24.8–29.0 ms frame.
    #[test]
    fn an_idle_menu_warms_the_other_tabs_strings_before_a_switch() {
        use crate::ui::fixture::FixtureMeasure as M;
        let _g = crate::testlock::serial();
        let (ps, _sid) = enhancement_test_session(EnhTestFixture::default());
        let mut store = crate::stores::metadata::MetadataStore::default();
        let sub = |id: i64, index: i64, lang: &str, code: &str| metadata::Stream {
            id,
            index,
            lang: lang.into(),
            lang_code: code.into(),
            codec: "srt".into(),
            ..Default::default()
        };
        let mut item = metadata::PlayingItem::with_subs(vec![
            sub(601, 2, "Spanish", "spa"),
            sub(602, 3, "Czech", "ces"),
        ]);
        item.audio = vec![metadata::Stream { id: 501, index: 0, codec: "ac3".into(), channels: 2, default: true, ..Default::default() }];
        assert!(store.run(crate::stores::metadata::MetadataCmd::InstallPlaying(Some(item))));
        let mut menu = TrackMenuState::new(&ps, store.view(), 0, Vec::new());
        crate::text::reset_prewarm_for_test();
        // A second of presented frames on the Audio tab: open, settle, drain. The first frame
        // drains the live page; every later one drains the other tab's strings in the
        // background, one string a frame (on the TV the whole tab in one frame was
        // `warmdrain:11.4`, a 21.3 ms frame right after the open).
        for frame in 0..60 {
            menu.update(0.016, &M, &ps, store.view());
            let drained = crate::ui::panel_motion::PanelMotion::drain_queued_text();
            // The draw drops whatever the live queue still holds (`ui::dispatch`, every frame
            // with no page warm), which on the TV left the background warm one string deep.
            crate::text::clear_prewarm();
            if frame > 0 {
                assert!(drained <= 1, "frame {frame} rasterised {drained} background strings");
            }
        }
        menu.focus_tab(&ps, store.view(), 1);
        let labels: Vec<String> = menu
            .form
            .table
            .sections
            .iter()
            .flat_map(|s| s.rows.iter())
            .map(|r| r.label.clone())
            .filter(|l| !l.is_empty())
            .collect();
        assert!(!labels.is_empty(), "premise: the Subtitles tab has rows");
        let cold: Vec<&String> =
            labels.iter().filter(|l| !crate::text::prewarm_resident_any_size_for_test(l.as_bytes())).collect();
        assert!(cold.is_empty(), "the switch met these Subtitles labels cold: {cold:?}");
        teardown(&ps);
    }

    /// **A page walked while the other tab's warm is still draining does not cancel it.** A live
    /// walk replaces the live queue and the draw empties it, but the background queue is neither:
    /// on the TV the osc's first sub-page push, a few frames after the open, used to wipe the
    /// one-a-frame warm, and the first switch to Audio met `textx7:6.6` cold.
    #[test]
    fn a_live_walk_that_interrupts_the_other_tabs_warm_requeues_it() {
        use crate::ui::fixture::FixtureMeasure as M;
        let _g = crate::testlock::serial();
        let (ps, _sid) = enhancement_test_session(EnhTestFixture::default());
        let mut store = crate::stores::metadata::MetadataStore::default();
        let sub = |id: i64, index: i64, lang: &str, code: &str| metadata::Stream {
            id,
            index,
            lang: lang.into(),
            lang_code: code.into(),
            codec: "srt".into(),
            ..Default::default()
        };
        let mut item = metadata::PlayingItem::with_subs(vec![
            sub(601, 2, "Spanish", "spa"),
            sub(602, 3, "Czech", "ces"),
        ]);
        item.audio = vec![
            metadata::Stream {
                id: 501,
                index: 0,
                lang: "English".into(),
                codec: "ac3".into(),
                channels: 2,
                default: true,
                ..Default::default()
            },
            metadata::Stream {
                id: 502,
                index: 1,
                lang: "German".into(),
                codec: "aac".into(),
                channels: 6,
                ..Default::default()
            },
        ];
        assert!(store.run(crate::stores::metadata::MetadataCmd::InstallPlaying(Some(item))));
        let mut menu = TrackMenuState::new(&ps, store.view(), 1, Vec::new());
        crate::text::reset_prewarm_for_test();
        let frame = |menu: &mut TrackMenuState| {
            menu.update(0.016, &M, &ps, store.view());
            crate::ui::panel_motion::PanelMotion::drain_queued_text();
            // What the drain left of the live queue, the draw drops (`ui::dispatch`).
            crate::text::clear_prewarm();
        };
        // The open drains the Subtitles root; the next frame queues Audio and drains one string.
        frame(&mut menu);
        frame(&mut menu);
        assert!(crate::text::background_prewarm_pending(), "premise: the Audio warm is draining");
        // Into Other languages and straight back, the way the osc walks it.
        let nav = (menu.form.table.sections.iter())
            .flat_map(|s| s.rows.iter())
            .position(|r| r.label == "Other languages");
        menu.focus_row(nav.expect("premise: an Other languages row") as c_int);
        menu.on_right(&ps, store.view());
        assert!(!menu.pages.is_empty(), "premise: the page opened");
        for _ in 0..40 {
            frame(&mut menu);
        }
        menu.on_left(&ps, store.view());
        for _ in 0..80 {
            frame(&mut menu);
        }
        menu.focus_tab(&ps, store.view(), 0);
        let cold: Vec<String> = menu
            .form
            .table
            .sections
            .iter()
            .flat_map(|s| s.rows.iter())
            .map(|r| r.label.clone())
            .filter(|l| {
                !l.is_empty() && !crate::text::prewarm_resident_any_size_for_test(l.as_bytes())
            })
            .collect();
        assert!(cold.is_empty(), "the switch met these Audio labels cold: {cold:?}");
        teardown(&ps);
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
    /// `rebuild_audio` always re-homed `table.sel` onto the checked audio track, so the drawn
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
        assert_eq!(menu.ids()[1], TrackRow::Boost, "fixture shape: row 1 is Boost");
        assert!(menu.focus_id(TrackRow::Boost));

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
            menu.form.selected_id().copied(),
            Some(TrackRow::Boost),
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
    /// against), and the ENGINE's own reconcile can independently clamp its stale remembered key
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
            menu.ids(),
            vec![TrackRow::Audio(0), TrackRow::Audio(1), TrackRow::Boost, TrackRow::Loudness],
            "fixture shape: two tracks, then Boost, then Loudness"
        );
        // The engine's focus lands on Boost, the way a real UP/DOWN's `FocusMoved` write-back does.
        assert!(menu.focus_id(TrackRow::Boost));

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
        assert!(menu.focus_id(TrackRow::Audio(1)));

        // The offer returns (the subtitle switched off again) — the same live poll this menu never
        // triggered itself.
        let (ps_shown, _sid_shown) = enhancement_test_session(EnhTestFixture::default());
        let store_shown = two_tracks();
        menu.update(0.0, &crate::ui::fixture::FixtureMeasure, &ps_shown, store_shown.view());

        assert!(menu.enhance_shown.is_some(), "fixture shape: the offer is back");
        assert_eq!(
            menu.form.selected_id().copied(),
            Some(TrackRow::Boost),
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
        let menu = TrackMenuState::new(&ps, store.view(), 1, vec!["eng".into()]);
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
        assert!(menu.own_burn_built(), "the live route is burning this subtitle in");

        let timing_i = menu.form.index_of(&TrackRow::Timing).expect("Timing row present");
        let style_i = menu.form.index_of(&TrackRow::Style).expect("Style row present");
        let rows = flat_rows(&menu);
        assert!(rows[timing_i].dim, "Timing is dim under a live burn");
        assert!(rows[style_i].dim, "Style is dim under a live burn");

        let note_i = style_i + 1;
        assert_eq!(menu.form.id_at(note_i), None, "a note is an inert slot with no id");
        assert_eq!(rows[note_i].label, crate::i18n::msg::widgets_tracks_style_locked_note());
        assert!(rows[note_i].sep, "a note row is non-selectable");

        // The track rows themselves stay live: Off, and the embedded subtitle, neither dim.
        let off_i = menu.form.index_of(&TrackRow::SubOff).expect("Off row present");
        assert!(!rows[off_i].dim);
        let sub_i = menu.form.index_of(&TrackRow::Sub(0)).expect("Sub(0) row present");
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
        assert!(!menu.own_burn_built());
        let timing_i = menu.form.index_of(&TrackRow::Timing).expect("Timing row present");
        assert!(!flat_rows(&menu)[timing_i].dim);
        assert!(!menu.form.table.sections.iter().flat_map(|s| s.rows.iter()).any(|r| r.is_note()));
        teardown(&ps);
    }

    /// OK on the dimmed Timing/Style rows is a no-op (`TrackOk::Inert`), the same "focusable but
    /// inert" contract `TrackRow::Timing` already had while subtitles are Off — it must not open
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

        let timing_i = menu.form.index_of(&TrackRow::Timing).unwrap();
        menu.focus_row(timing_i as c_int);
        assert_eq!(menu.on_ok(store.view()), TrackOk::Inert);

        let style_i = menu.form.index_of(&TrackRow::Style).unwrap();
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
        let off_i = menu.form.index_of(&TrackRow::SubOff).unwrap();
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
        assert!(!menu.own_burn_built(), "not a burn yet");

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
        assert!(menu.own_burn_built(), "Color/Timing must lock once the live route is really a Burn");
        let style_i = menu.form.index_of(&TrackRow::Style).expect("Style row present");
        assert!(flat_rows(&menu)[style_i].dim, "Style must actually redraw dim, not just flag it internally");
        let off_i = menu.form.index_of(&TrackRow::SubOff).expect("Off row present");
        assert!(!flat_rows(&menu)[off_i].checked, "Off must no longer read checked");
        let sub_i = menu.form.index_of(&TrackRow::Sub(0)).expect("Sub(0) row present");
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

    /// The focus element of the `i`-th Audio row, named by identity.
    fn audio_key(i: usize) -> u32 {
        TrackRow::Audio(i).key().0
    }

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
        let mut sec = FormSection::new("Audio");
        for (i, label) in ["English", "Русский", "Français"].into_iter().enumerate() {
            sec = sec.item(TrackRow::Audio(i), RowKind::Choice, (), Row::new(label));
        }
        let mut form = FormTable::new(crate::ui::table_screen::BAND_BASE);
        form.set(TrackForm::new().section(sec), None);
        TrackMenuState {
            tab: 0,
            active_audio: 0,
            active_sub: -1,
            form,
            offset_ms: 0,
            tone: SubtitleTone::White,
            size: SubtitleSize::Medium,
            position: SubtitlePosition::Low,
            pages: Vec::new(),
            renderer: SubRenderer::Text,
            sub_sig: None,
            other: Vec::new(),
            root_tracks: Vec::new(),
            yours: Vec::new(),
            enhance_shown: None,
            enhance_route: None,
            enhance_disabled: None,
            enhance_subtitle_effect: crate::route::SubtitleEffect::None,
            sticky_audio_target: None,
            motion: PanelMotion::new(),
            other_tab_warmed: false,
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
            assert_eq!(step(audio_key(0), Dir::Down), Some(audio_key(1)));
            assert_eq!(step(audio_key(2), Dir::Down), None, "the last row does not wrap");
            assert_eq!(step(audio_key(0), Dir::Up), None, "the first row does not wrap");
            assert_eq!(step(audio_key(1), Dir::Up), Some(audio_key(0)));
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
                    FocusKey { entry: e, elem: audio_key(1) },
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
            let placed = <TrackMenuPart as Focusable<HostFixture>>::place(&part, &audio_key(2), cx, At::Drawn);
            assert_eq!(
                placed.map(|p| (p.rect.x, p.rect.y, p.rect.w, p.rect.h)),
                want.map(|r| (r.x, r.y, r.w, r.h))
            );
        });
    }

    /// **Reordering the menu moves no focus key.** The rows are the same three identities in the
    /// opposite order: the focused row stays focused (by identity, through `FormTable::set`), its
    /// key is the one it always had, `seat` hands the engine that key, `place` finds it at its new
    /// position, and a DOWN step from it goes to whichever row now sits below.
    #[test]
    fn reordering_the_rows_moves_no_focus_key() {
        let e = EntryId(5);
        let mut st = three_row_menu();
        st.form.table.sel = 2; // Audio(2), the last row
        let mut sec = FormSection::new("Audio");
        for i in [2usize, 1, 0] {
            sec = sec.item(TrackRow::Audio(i), RowKind::Choice, (), Row::new(format!("row {i}")));
        }
        st.form.set(TrackForm::new().section(sec), st.form.selected_id().copied().as_ref());
        assert_eq!(st.form.selected_id(), Some(&TrackRow::Audio(2)), "focus followed the identity");
        assert_eq!(st.form.table.sel, 0, "…to its new position");

        let part = TrackMenuPart { state: &st, entry: e, group: GroupId(0) };
        with_cx(e, |cx| {
            let seat = <TrackMenuPart as Focusable<HostFixture>>::seat(
                &part,
                GroupId(0),
                Placed { rect: Rect::new(0.0, 0.0, 1.0, 1.0), rest_rect: Rect::new(0.0, 0.0, 1.0, 1.0), clip: Rect::new(0.0, 0.0, 1.0, 1.0), index: None },
                cx,
            );
            assert_eq!(seat.elem, audio_key(2), "the engine is told the key the row always had");
            let placed = <TrackMenuPart as Focusable<HostFixture>>::place(&part, &audio_key(2), cx, At::Drawn)
                .expect("the moved row still places");
            assert_eq!(placed.index, Some(0));
            let Step::Move(next) = <TrackMenuPart as Focusable<HostFixture>>::neighbour(
                &part,
                FocusKey { entry: e, elem: audio_key(2) },
                Dir::Down,
                cx,
            ) else {
                panic!("DOWN from the first row moves")
            };
            assert_eq!(next.elem, audio_key(1), "stepping follows the NEW order");
        });
    }

    /// **The follow-up gap.** A row count that shrinks and grows back (the Audio tab's enhancement
    /// rows vanishing under a live poll rebuild, then returning) can leave the ENGINE's own
    /// remembered focus index stale relative to `table.sel`: `TrackMenuState::rebuild_audio`
    /// restores `table.sel` onto the toggle row's new position (`sticky_audio_target`), but the
    /// engine has no way to learn that unless `reconcile` actually reports it. Before the fix,
    /// `reconcile` answered `settle(want)` — clamping the ENGINE's own possibly-stale index — which
    /// only differs from `want` when that raw index is now literally out of range, so a mere
    /// position change the panel already resolved (not a shrink past it) went unreported and the
    /// engine's remembered element stayed wrong. `reconcile` must instead always answer the panel's
    /// own `table.sel`, so the engine adopts it whenever it disagrees.
    #[test]
    fn reconcile_reports_the_panels_own_cursor_not_a_clamp_of_the_engines_stale_index() {
        let e = EntryId(5);
        let mut st = three_row_menu();
        // The panel's own rebuild has already moved `table.sel` to row 2 (e.g. `sticky_audio_target`
        // restoring focus onto the toggle row once the enhancement rows came back).
        st.form.table.sel = 2;
        let part = TrackMenuPart { state: &st, entry: e, group: GroupId(0) };
        with_cx(e, |cx| {
            // The engine still remembers row 0 — a perfectly in-range index for this 3-row table,
            // so the old clamp-`want` implementation would answer it back UNCHANGED.
            let want = FocusKey { entry: e, elem: audio_key(0) };
            let got = <TrackMenuPart as Focusable<HostFixture>>::reconcile(&part, want, cx);
            assert_eq!(
                got.elem, audio_key(2),
                "reconcile must follow the panel's own table.sel, not echo back an in-range `want`"
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

#[cfg(test)]
mod keyed_form_tests {
    use super::tests::{store_with, store_with_audio, stream};
    use super::*;
    use crate::ui::form::RowKeys;
    use crate::route::{enhancement_test_session, reset_player_control_for_test, EnhTestFixture};

    fn audio(id: i64, index: i64, default: bool) -> metadata::Stream {
        metadata::Stream { id, index, codec: "aac".into(), channels: 2, default, ..Default::default() }
    }

    fn key_of(menu: &TrackMenuState, id: TrackRow) -> Option<RowKey> {
        menu.form.index_of(&id).and_then(|i| menu.form.key_at(i))
    }

    #[test]
    fn keys_are_distinct_across_both_tabs_and_below_the_ceiling() {
        let mut ids = vec![TrackRow::SubOff, TrackRow::Timing, TrackRow::Style, TrackRow::Boost, TrackRow::Loudness];
        for field in StyleField::ALL {
            ids.push(TrackRow::OpenField(field));
            ids.extend((0..field.rungs()).map(|i| TrackRow::Choice(field, i)));
        }
        ids.extend((0..40).flat_map(|i| [TrackRow::Sub(i), TrackRow::Audio(i)]));
        let keys: Vec<u32> = ids.iter().map(|i| i.key().0).collect();
        for (n, a) in keys.iter().enumerate() {
            assert!(*a < BAND_BASE);
            assert!(!keys[n + 1..].contains(a), "duplicate key {a:#x}");
        }
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
        assert_eq!(menu.ids(), vec![TrackRow::Audio(0), TrackRow::Audio(1)]);
        menu.focus_key(TrackRow::Audio(1).key().0);
        let before = key_of(&menu, TrackRow::Audio(1));

        let (ps_offered, _s2) = enhancement_test_session(EnhTestFixture::default());
        menu.update(0.0, &crate::ui::fixture::FixtureMeasure, &ps_offered, store.view());

        assert!(menu.enhance_shown.is_some(), "premise: the pair was inserted");
        assert_eq!(menu.ids().len(), 4, "two tracks + Boost + Loudness");
        assert_eq!(menu.selected_id(), Some(TrackRow::Audio(1)));
        assert_eq!(key_of(&menu, TrackRow::Audio(1)), before, "the track's key did not move");
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
        three.push(stream(3, 2, "Arabic", "ara", "")); // "yours" ranks Arabic first
        let yours = || vec!["ara".to_string(), "eng".to_string(), "fra".to_string()];
        let (a, b) = (store_with(two), store_with(three));
        let before = TrackMenuState::new(&ps, a.view(), 1, yours());
        let after = TrackMenuState::new(&ps, b.view(), 1, yours());
        assert!(after.form.index_of(&TrackRow::Sub(2)) < after.form.index_of(&TrackRow::Sub(0)),
            "premise: the new track is drawn ABOVE the existing ones");
        for id in [TrackRow::SubOff, TrackRow::Timing, TrackRow::Style, TrackRow::Sub(0), TrackRow::Sub(1)] {
            assert!(key_of(&before, id).is_some(), "{id:?} is built");
            assert_eq!(key_of(&before, id), key_of(&after, id), "{id:?} kept its key");
        }
    }

    /// **A language's drill-in keeps its identity when tracks are added** — a track of another
    /// language that sorts above it, a second track of its own language, a later track of a new
    /// language: `OpenLang` is the stream id of the language's first track, never a list position.
    /// Its KEY is its ordinal on the page (what the focus engine needs: unique, below the ceiling),
    /// so it follows the language when another sorts above it, and the form's `reseat` carries the
    /// engine along.
    #[test]
    fn an_open_lang_is_the_same_row_when_tracks_are_added() {
        let _g = crate::testlock::serial();
        crate::player::sidecar::reset();
        let ps = crate::route::PlaybackSession::IDLE;
        let base = vec![
            stream(1, 0, "German", "deu", ""),
            stream(2, 1, "French", "fra", ""),
            stream(3, 2, "French", "fra", "SDH"),
        ];
        let mut more = base.clone();
        more.push(stream(4, 3, "Arabic", "ara", "")); // sorts above French on the page
        more.push(stream(5, 4, "French", "fra", "Commentary"));
        let other_page = |subs: Vec<metadata::Stream>| {
            let store = store_with(subs);
            let mut menu = TrackMenuState::new(&ps, store.view(), 1, Vec::new());
            menu.push(TrackPage::OtherLanguages);
            menu
        };
        let (before, after) = (other_page(base), other_page(more));
        let french = TrackRow::OpenLang(LangId { stream: 2, slot: 0 });
        assert!(key_of(&before, french).is_some(), "French is a drill-in on the page");
        assert!(after.form.index_of(&french).is_some(), "the same identity is on the page after");
        assert!(after.form.index_of(&TrackRow::Sub(3)) < after.form.index_of(&french), "premise: Arabic is drawn above it");
        let keys = after.keys();
        assert_eq!(keys.len(), keys.iter().collect::<std::collections::HashSet<_>>().len(), "keys are unique on the page");
        assert_ne!(key_of(&after, french), key_of(&after, TrackRow::OpenOther));
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
        assert_eq!(menu.selected_id(), Some(TrackRow::Audio(1)));
        assert_eq!(menu.sel(), 1);

        // Subtitles: Off when none is active, else the active track (even inside "Other languages")
        let subs = vec![stream(1, 0, "English", "eng", ""), stream(2, 1, "French", "fra", "")];
        let store = store_with(subs);
        let mut menu = TrackMenuState::new(&ps, store.view(), 1, vec!["eng".into(), "fra".into()]);
        assert_eq!(menu.selected_id(), Some(TrackRow::SubOff));
        menu.active_sub = 1;
        menu.rebuild(&ps, store.view(), 1, false);
        assert_eq!(menu.selected_id(), Some(TrackRow::Sub(1)));
        menu.focus_tab(&ps, store.view(), 0); // a tab switch re-opens on that tab's own active row
        menu.focus_tab(&ps, store.view(), 1);
        assert_eq!(menu.selected_id(), Some(TrackRow::Sub(1)));
    }
}

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
        let menu = TrackMenuState::new(&ps, store.view(), 1, vec!["eng".into()]);
        (menu, ps, store)
    }

    fn open_text() -> (TrackMenuState, crate::route::PlaybackSession, crate::stores::metadata::MetadataStore) {
        open_with("srt", SubtitleEffect::Sidecar)
    }

    fn focus_id(menu: &mut TrackMenuState, id: TrackRow) {
        let i = menu.form.index_of(&id).unwrap_or_else(|| panic!("{id:?} is not on this page"));
        menu.focus_row(i as c_int);
        assert_eq!(menu.selected_id(), Some(id));
    }

    fn row_of(menu: &TrackMenuState, id: TrackRow) -> &Row {
        let i = menu.form.index_of(&id).unwrap_or_else(|| panic!("{id:?} is not on this page"));
        menu.form.table.sections.iter().flat_map(|s| &s.rows).nth(i).unwrap()
    }

    /// OK on Style pushes the Style page, focus on Size by id; OK on Size pushes its picker with
    /// the checked rung focused; LEFT pops each back onto the row that opened it.
    #[test]
    fn push_lands_on_an_explicit_id_and_pop_restores_the_opener() {
        let _g = crate::testlock::serial();
        let (mut menu, ps, store) = open_text();
        focus_id(&mut menu, TrackRow::Style);
        assert_eq!(menu.on_ok(store.view()), TrackOk::Navigated);
        assert_eq!(menu.page_path(), [TrackPage::Style]);
        assert_eq!(menu.selected_id(), Some(TrackRow::OpenField(StyleField::Size)));
        assert_eq!(menu.form.table.title(), Some(TrackPage::Style.title(&[]).as_str()));

        focus_id(&mut menu, TrackRow::OpenField(StyleField::Position));
        assert_eq!(menu.on_ok(store.view()), TrackOk::Navigated);
        assert_eq!(menu.page_path(), [TrackPage::Style, TrackPage::Picker(StyleField::Position)]);
        assert_eq!(
            menu.selected_id(),
            Some(TrackRow::Choice(StyleField::Position, menu.position.index() as usize)),
            "a picker opens on its checked rung"
        );

        menu.on_left(&ps, store.view());
        assert_eq!(menu.page_path(), [TrackPage::Style]);
        assert_eq!(menu.selected_id(), Some(TrackRow::OpenField(StyleField::Position)), "the opener, by id");
        menu.on_left(&ps, store.view());
        assert!(menu.page_path().is_empty());
        assert_eq!(menu.selected_id(), Some(TrackRow::Style));
        assert_eq!(menu.form.table.title(), None, "the root has no title band");
        teardown(&ps);
    }

    /// RIGHT on a Nav row enters the page exactly as OK does; RIGHT off one is the tab switch.
    #[test]
    fn right_on_a_nav_row_pushes_and_left_at_the_root_switches_tab() {
        let _g = crate::testlock::serial();
        let (mut menu, ps, store) = open_text();
        focus_id(&mut menu, TrackRow::Style);
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
        focus_id(&mut menu, TrackRow::Style);
        for _ in 0..240 {
            menu.update(0.016, &crate::ui::fixture::FixtureMeasure, &ps, store.view());
        }
        let left_at = menu.form.table.scroll_pos();
        assert!(left_at > 0.0, "the premise: 40 tracks push Style below the fold ({left_at})");
        assert_eq!(menu.on_ok(store.view()), TrackOk::Navigated);
        assert!(menu.form.table.scroll_pos() < left_at, "a page starts at its top");
        assert!(menu.pop(&ps, store.view()));
        assert_eq!(menu.form.table.scroll_pos(), left_at);
        assert_eq!(menu.selected_id(), Some(TrackRow::Style));
        teardown(&ps);
    }

    /// Picking a rung commits it live, keeps the panel and page open and moves the checkmark; the
    /// already-checked rung is inert.
    #[test]
    fn a_picker_pick_commits_live_and_moves_the_checkmark() {
        let _g = crate::testlock::serial();
        let (mut menu, ps, store) = open_text();
        focus_id(&mut menu, TrackRow::Style);
        assert_eq!(menu.on_ok(store.view()), TrackOk::Navigated);
        assert_eq!(menu.on_ok(store.view()), TrackOk::Navigated, "Size");
        let current = menu.current_rung(StyleField::Size);
        assert!(row_of(&menu, TrackRow::Choice(StyleField::Size, current)).checked);
        assert_eq!(menu.on_ok(store.view()), TrackOk::Inert, "the focused rung is the checked one");

        let other = (current + 1) % StyleField::Size.rungs();
        focus_id(&mut menu, TrackRow::Choice(StyleField::Size, other));
        assert_eq!(
            menu.on_ok(store.view()),
            TrackOk::Commit {
                commit: TrackCommit::SubtitleSize(SubtitleSize::from_index(other as u8)),
                keep_open: true
            }
        );
        assert_eq!(menu.page_path().len(), 2, "the page stays");
        assert!(row_of(&menu, TrackRow::Choice(StyleField::Size, other)).checked);
        assert!(!row_of(&menu, TrackRow::Choice(StyleField::Size, current)).checked);
        teardown(&ps);
    }

    /// The Style page reads the current value of each field on its Nav row.
    #[test]
    fn the_style_page_shows_each_fields_current_value() {
        let _g = crate::testlock::serial();
        let (mut menu, ps, store) = open_text();
        focus_id(&mut menu, TrackRow::Style);
        menu.on_ok(store.view());
        for field in StyleField::ALL {
            let row = row_of(&menu, TrackRow::OpenField(field));
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
            focus_id(&mut menu, TrackRow::Style);
            assert_eq!(menu.on_ok(store.view()), TrackOk::Navigated, "{codec}: Style itself is live");
            let locked = renderer != SubRenderer::Text;
            for field in [StyleField::Size, StyleField::Position] {
                assert_eq!(row_of(&menu, TrackRow::OpenField(field)).dim, locked, "{codec} {field:?}");
            }
            assert!(!row_of(&menu, TrackRow::OpenField(StyleField::Color)).dim, "{codec}: Color stays live");
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
                focus_id(&mut menu, TrackRow::OpenField(StyleField::Size));
                assert_eq!(menu.on_ok(store.view()), TrackOk::Inert, "{codec}: a dim Size row opens nothing");
                menu.on_right(&ps, store.view());
                assert_eq!(menu.page_path(), [TrackPage::Style], "{codec}: RIGHT is inert on it too");
            }
            focus_id(&mut menu, TrackRow::OpenField(StyleField::Color));
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
        assert!(!has(&menu, TrackRow::Timing) && !has(&menu, TrackRow::Style));
        teardown(&ps);
        // direct play: both, live
        let (menu, ps, _store) = open_text();
        assert!(has(&menu, TrackRow::Timing) && has(&menu, TrackRow::Style));
        assert!(!row_of(&menu, TrackRow::Style).dim);
        teardown(&ps);
    }

    /// **A Style page never outlives what it was built for**: a change of the
    /// page's availability (here the renderer kind) while on a sub-page pops to the root (opener and
    /// scroll restored); an unchanged signature leaves the page alone; on the root any change
    /// refreshes in place.
    #[test]
    fn a_rebuild_signature_mismatch_on_a_sub_page_pops_to_the_root() {
        let _g = crate::testlock::serial();
        let (mut menu, ps, store) = open_with("srt", SubtitleEffect::Sidecar);
        focus_id(&mut menu, TrackRow::Style);
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
        assert_eq!(menu.selected_id(), Some(TrackRow::Style), "on the row that opened it");
        assert_eq!(menu.form.table.title(), None);
        teardown(&ps);
    }

    /// **Only an availability change pops a sub-page.** A subs fingerprint change (a track offered
    /// mid-play) or an enhancement-route change that leaves the renderer and the Style gate alone
    /// refreshes the open Size picker in place: same page, same focused row, new signature stored.
    #[test]
    fn a_signature_change_that_keeps_the_pages_availability_refreshes_in_place() {
        let _g = crate::testlock::serial();
        let (mut menu, ps, store) = open_text();
        focus_id(&mut menu, TrackRow::Style);
        menu.on_ok(store.view());
        focus_id(&mut menu, TrackRow::OpenField(StyleField::Size));
        menu.on_ok(store.view());
        let picker = [TrackPage::Style, TrackPage::Picker(StyleField::Size)];
        assert_eq!(menu.page_path(), picker);
        let focused = TrackRow::Choice(StyleField::Size, 0);
        focus_id(&mut menu, focused);
        let before = menu.sub_sig.clone();

        // a second subtitle offered mid-play: the fingerprint moves, the renderer does not
        let store2 = store_with(vec![stream(999, 0, "English", "eng", ""), stream(1000, 1, "French", "fra", "")]);
        menu.update(0.016, &crate::ui::fixture::FixtureMeasure, &ps, store2.view());
        assert_ne!(menu.sub_sig, before, "the new fingerprint is stored");
        assert_eq!(menu.page_path(), picker, "the picker stays");
        assert_eq!(menu.selected_id(), Some(focused), "on the row the viewer was on");
        teardown(&ps);

        // the enhancement's subtitle effect moves (an embedded track is now the one on screen), still no own burn
        let (mut menu, ps, store) = open_text();
        focus_id(&mut menu, TrackRow::Style);
        menu.on_ok(store.view());
        focus_id(&mut menu, TrackRow::OpenField(StyleField::Size));
        menu.on_ok(store.view());
        focus_id(&mut menu, focused);
        let before = menu.sub_sig.clone();
        teardown(&ps);
        let (ps2, _sid) = enhancement_test_session(EnhTestFixture {
            remux: None,
            subtitle_effect: SubtitleEffect::Embedded,
            ..Default::default()
        });
        menu.update(0.016, &crate::ui::fixture::FixtureMeasure, &ps2, store.view());
        assert_ne!(menu.sub_sig, before, "the enhancement route is part of the signature");
        assert_eq!(menu.page_path(), picker, "the picker stays");
        assert_eq!(menu.selected_id(), Some(focused));
        teardown(&ps2);
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

    /// **The locked-renderer notes wrap to two lines at the player-menu floor (three in Spanish).** A Style page is a few
    /// short rows; without [`theme::layout::PLAYER_MENU_MIN_W`] it shrank to its labels and the
    /// note wrapped to three lines in a sliver.
    #[test]
    fn the_style_note_fits_two_lines_at_the_player_menu_floor() {
        use crate::i18n::{language_on_this_thread_for_test, Preference};
        let measure = crate::fontcov::advances::ShippedMeasure;
        for language in [Preference::En, Preference::Es, Preference::Be] {
            let _g = crate::testlock::serial();
            let _guard = language_on_this_thread_for_test(language);
            for codec in ["pgs", "ass"] {
                let (mut menu, ps, _store) = open_with(codec, SubtitleEffect::Sidecar);
                menu.push(TrackPage::Style);
                let w = menu.form.table.menu_panel_width(&measure);
                assert!(w >= theme::layout::PLAYER_MENU_MIN_W, "{language:?}: the page is not a sliver ({w})");
                menu.form.table.fit_notes(w, &measure);
                let lines = menu.form.table.sections.iter().flat_map(|s| &s.rows)
                    .filter(|r| r.is_note()).map(|r| r.note_lines.get()).max().expect("a note row");
                // Spanish's longer wording needs ~520px for two lines; at the 440 floor it takes three
                // (measured), English and Belarusian take two
                let allowed = if language == Preference::Es { 3 } else { 2 };
                assert!(lines <= allowed, "{language:?} {codec}: the note takes {lines} lines at {w}px");
                teardown(&ps);
            }
        }
    }

    /// The title band is a pointer-only stop: its key is recognised, no row owns it.
    #[test]
    fn the_title_key_is_the_bands_and_no_rows() {
        let _g = crate::testlock::serial();
        let (mut menu, ps, store) = open_text();
        focus_id(&mut menu, TrackRow::Style);
        menu.on_ok(store.view());
        assert!(TrackMenuState::is_title_key(TITLE_KEY));
        assert!(menu.keys().iter().all(|k| !TrackMenuState::is_title_key(*k)));
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
        focus_id(&mut menu, TrackRow::Style);
        let root = hash(&menu);
        menu.on_ok(store.view());
        let style = hash(&menu);
        focus_id(&mut menu, TrackRow::OpenField(StyleField::Color));
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

/// **The Other languages page and the language pages** (`docs/player-submenus.md`, PR 3).
#[cfg(test)]
mod language_page_tests {
    use super::tests::{store_with, stream};
    use super::*;
    use crate::route::reset_player_control_for_test;

    fn teardown(ps: &crate::route::PlaybackSession) {
        reset_player_control_for_test(ps);
        crate::plex::reset_servers_for_test();
    }

    fn pgs(mut s: metadata::Stream) -> metadata::Stream {
        s.codec = "pgs".into();
        s
    }

    /// English is "yours" (index 0, a root row); Dutch (PGS) and German are single-track others;
    /// French has a full, an SDH and a forced PGS track.
    fn subs() -> Vec<metadata::Stream> {
        vec![
            stream(10, 0, "English", "eng", ""),
            stream(11, 1, "French", "fra", ""),
            stream(12, 2, "French", "fra", "SDH"),
            pgs(stream(13, 3, "French", "fra", "Forced")),
            stream(14, 4, "German", "deu", ""),
            pgs(stream(15, 5, "Dutch", "nld", "")),
        ]
    }

    /// [`subs`] plus Italian: a text full track and a VobSub SDH one (`dvd_subtitle` is Plex's
    /// own name for it), the long raw codec the badge must shorten.
    fn subs_with_vobsub() -> Vec<metadata::Stream> {
        let mut v = subs();
        v.push(stream(16, 6, "Italian", "ita", "Netflix"));
        let mut sdh = stream(17, 7, "Italian", "ita", "Netflix");
        sdh.codec = "dvd_subtitle".into();
        sdh.sdh = true;
        v.push(sdh);
        v
    }

    fn open(subs: Vec<metadata::Stream>) -> (TrackMenuState, crate::route::PlaybackSession, crate::stores::metadata::MetadataStore) {
        let _ = crate::player::sidecar::reset();
        let ps = crate::route::PlaybackSession::IDLE;
        let store = store_with(subs);
        let menu = TrackMenuState::new(&ps, store.view(), 1, vec!["eng".into()]);
        (menu, ps, store)
    }

    fn focus_id(menu: &mut TrackMenuState, id: TrackRow) {
        let i = menu.form.index_of(&id).unwrap_or_else(|| panic!("{id:?} is not on this page"));
        menu.focus_row(i as c_int);
        assert_eq!(menu.selected_id(), Some(id));
    }

    fn row_of(menu: &TrackMenuState, id: TrackRow) -> &Row {
        let i = menu.form.index_of(&id).unwrap_or_else(|| panic!("{id:?} is not on this page"));
        menu.form.table.sections.iter().flat_map(|s| &s.rows).nth(i).unwrap()
    }

    /// OK on the root's Other languages row opens the page A-Z (a single-track language is a pick
    /// row, French a drill-in reading its track count); OK on French opens its page, ranked; LEFT
    /// pops each back onto the row that opened it, by id, and the title follows the page.
    #[test]
    fn push_and_pop_walk_root_other_languages_and_a_language() {
        let _g = crate::testlock::serial();
        let (mut menu, ps, store) = open(subs());
        assert_eq!(menu.ids()[..3], [TrackRow::SubOff, TrackRow::Sub(0), TrackRow::OpenOther]);
        assert_eq!(row_of(&menu, TrackRow::OpenOther).value.as_deref(), Some("3"), "Dutch, French, German");

        focus_id(&mut menu, TrackRow::OpenOther);
        assert_eq!(menu.on_ok(store.view()), TrackOk::Navigated);
        assert_eq!(menu.page_path(), [TrackPage::OtherLanguages]);
        assert_eq!(menu.form.table.title(), Some("Other languages"));
        assert_eq!(
            menu.ids(),
            [TrackRow::Sub(5), TrackRow::OpenLang(LangId { stream: 11, slot: 1 }), TrackRow::Sub(4)],
            "Dutch, French, German"
        );
        assert_eq!(menu.selected_id(), Some(TrackRow::Sub(5)), "nothing checked: the first row");
        assert_eq!(row_of(&menu, TrackRow::OpenLang(LangId { stream: 11, slot: 1 })).value.as_deref(), Some("3 tracks"));

        focus_id(&mut menu, TrackRow::OpenLang(LangId { stream: 11, slot: 1 }));
        assert_eq!(menu.on_ok(store.view()), TrackOk::Navigated);
        assert_eq!(menu.page_path(), [TrackPage::OtherLanguages, TrackPage::Language(11)]);
        assert_eq!(menu.form.table.title(), Some("French"));
        assert_eq!(
            menu.ids(),
            [TrackRow::Sub(1), TrackRow::Sub(2), TrackRow::Sub(3)],
            "full, SDH, forced"
        );

        assert!(menu.pop(&ps, store.view()));
        assert_eq!(menu.selected_id(), Some(TrackRow::OpenLang(LangId { stream: 11, slot: 1 })), "the opener, by id");
        assert_eq!(menu.form.table.title(), Some("Other languages"));
        assert!(menu.pop(&ps, store.view()));
        assert_eq!(menu.selected_id(), Some(TrackRow::OpenOther), "the root's row");
        assert_eq!(menu.form.table.title(), None);
        teardown(&ps);
    }

    /// RIGHT on a drill-in enters it like OK; LEFT on a page pops, never switches tab.
    #[test]
    fn right_enters_a_language_and_left_leaves_it() {
        let _g = crate::testlock::serial();
        let (mut menu, ps, store) = open(subs());
        focus_id(&mut menu, TrackRow::OpenOther);
        menu.on_right(&ps, store.view());
        focus_id(&mut menu, TrackRow::OpenLang(LangId { stream: 11, slot: 1 }));
        menu.on_right(&ps, store.view());
        assert_eq!(menu.page_path(), [TrackPage::OtherLanguages, TrackPage::Language(11)]);
        menu.on_left(&ps, store.view());
        menu.on_left(&ps, store.view());
        assert!(menu.page_path().is_empty());
        assert_eq!(menu.tab, 1, "still the Subtitles tab");
        teardown(&ps);
    }

    /// **Only image subtitles carry a format badge**: PGS shows one, a text track (SRT) none — on
    /// the Other languages page and on a language page alike.
    #[test]
    fn the_format_badge_is_only_on_image_subtitles() {
        let _g = crate::testlock::serial();
        let (mut menu, ps, _store) = open(subs_with_vobsub());
        menu.push(TrackPage::OtherLanguages);
        let badges = |m: &TrackMenuState, id| row_of(m, id).badges.iter().map(|b| b.text().to_string()).collect::<Vec<_>>();
        assert_eq!(badges(&menu, TrackRow::Sub(5)), ["PGS"], "Dutch is an image subtitle");
        assert!(badges(&menu, TrackRow::Sub(4)).is_empty(), "German SRT shows no format");
        assert!(badges(&menu, TrackRow::OpenLang(LangId { stream: 11, slot: 1 })).is_empty(), "a drill-in is not a track");
        menu.push(TrackPage::Language(11));
        assert!(badges(&menu, TrackRow::Sub(1)).is_empty(), "French full is SRT");
        assert!(badges(&menu, TrackRow::Sub(2)).is_empty(), "the SDH chip would only repeat its label");
        assert_eq!(row_of(&menu, TrackRow::Sub(2)).label, "SDH");
        assert_eq!(badges(&menu, TrackRow::Sub(3)), ["PGS"], "a forced image track still shows its format");
        assert_eq!(row_of(&menu, TrackRow::Sub(3)).label, "Forced");
        // a VobSub SDH track: its raw codec ("dvd_subtitle") shows as the short name, after the kind
        menu.push(TrackPage::Language(16));
        assert!(badges(&menu, TrackRow::Sub(6)).is_empty(), "Italian text shows no format");
        assert_eq!(badges(&menu, TrackRow::Sub(7)), ["SDH", "VOBSUB"]);
        // a row whose only badge could be the codec: the same French forced track, unlabelled
        let (mut menu2, ps2, _s2) = open(vec![
            stream(1, 0, "French", "fra", ""),
            pgs(stream(2, 1, "French", "fra", "")),
        ]);
        menu2.push(TrackPage::OtherLanguages);
        menu2.push(TrackPage::Language(1));
        assert!(badges(&menu2, TrackRow::Sub(0)).is_empty());
        assert_eq!(badges(&menu2, TrackRow::Sub(1)), ["PGS"]);
        teardown(&ps);
        teardown(&ps2);
    }

    /// **Initial focus**: Other languages opens on the checked row (the language's drill-in when
    /// the active track is inside one), a language page on its active variant, else on its first.
    #[test]
    fn a_page_opens_on_the_active_track_when_it_is_inside() {
        let _g = crate::testlock::serial();
        let (mut menu, ps, store) = open(subs());
        menu.active_sub = 2; // French SDH
        menu.rebuild(&ps, store.view(), 1, false);
        assert!(row_of(&menu, TrackRow::OpenOther).checked, "the active track lives behind it");
        assert_eq!(row_of(&menu, TrackRow::OpenOther).value.as_deref(), Some("French"));
        focus_id(&mut menu, TrackRow::OpenOther);
        menu.on_ok(store.view());
        assert_eq!(menu.selected_id(), Some(TrackRow::OpenLang(LangId { stream: 11, slot: 1 })), "the language holding the active track");
        let french = row_of(&menu, TrackRow::OpenLang(LangId { stream: 11, slot: 1 }));
        assert!(french.checked);
        assert_eq!(french.value.as_deref(), Some("SDH"), "reads the active variant");
        menu.on_ok(store.view());
        assert_eq!(menu.selected_id(), Some(TrackRow::Sub(2)), "the active variant");
        assert!(row_of(&menu, TrackRow::Sub(2)).checked);

        // a single-track active language: its pick row on Other languages
        let (mut menu, ps2, store) = open(subs());
        menu.active_sub = 4; // German
        menu.rebuild(&ps2, store.view(), 1, false);
        focus_id(&mut menu, TrackRow::OpenOther);
        menu.on_ok(store.view());
        assert_eq!(menu.selected_id(), Some(TrackRow::Sub(4)));

        // the active track is NOT in the language: its first track
        let (mut menu, ps3, store) = open(subs());
        menu.active_sub = 0; // English, on the root
        menu.push(TrackPage::OtherLanguages);
        menu.push(TrackPage::Language(11));
        assert_eq!(menu.selected_id(), Some(TrackRow::Sub(1)));
        teardown(&ps);
        teardown(&ps2);
        teardown(&ps3);
        let _ = store;
    }

    /// A pick on a language page commits like a root pick does and dismisses the panel.
    #[test]
    fn a_pick_on_a_language_page_commits_and_dismisses() {
        let _g = crate::testlock::serial();
        let (mut menu, ps, store) = open(subs());
        menu.push(TrackPage::OtherLanguages);
        menu.push(TrackPage::Language(11));
        focus_id(&mut menu, TrackRow::Sub(2));
        match menu.on_ok(store.view()) {
            TrackOk::Commit { commit: TrackCommit::Subtitle { stream_id, .. }, keep_open } => {
                assert_eq!(stream_id, 12);
                assert!(!keep_open, "a track pick at any depth dismisses");
            }
            other => panic!("expected a subtitle commit, got {other:?}"),
        }
        assert_eq!(menu.active_sub, 2);
        teardown(&ps);
    }

    /// French as sidecars the client can draw only while each has a `key`: an empty one is not
    /// offered on this route, which is how a language leaves the offered list under a fixed item.
    fn sidecar_french(keys: [&str; 3]) -> Vec<metadata::Stream> {
        let mut v = subs();
        for (n, key) in keys.iter().enumerate() {
            v[1 + n].external = true;
            v[1 + n].codec = "srt".into();
            v[1 + n].key = key.to_string();
        }
        v
    }

    /// **A language page never outlives its language**: the live offered list loses French while
    /// its page is open, so the stack pops to the root (opener and root restored by id). A change
    /// that leaves the language offered (one of its tracks going away) refreshes the page in place
    /// and keeps the viewer's row.
    #[test]
    fn a_vanished_language_pops_to_the_root_and_a_changed_one_refreshes_in_place() {
        let _g = crate::testlock::serial();
        let store = store_with(sidecar_french(["/a.srt", "/b.srt", "/c.srt"]));
        let _ = crate::player::sidecar::reset();
        let ps = crate::route::PlaybackSession::IDLE;
        let mut menu = TrackMenuState::new(&ps, store.view(), 1, vec!["eng".into()]);
        focus_id(&mut menu, TrackRow::OpenOther);
        menu.on_ok(store.view());
        focus_id(&mut menu, TrackRow::OpenLang(LangId { stream: 11, slot: 1 }));
        menu.on_ok(store.view());
        assert_eq!(menu.ids().len(), 3);
        focus_id(&mut menu, TrackRow::Sub(2));
        let measure = crate::ui::fixture::FixtureMeasure;
        menu.update(0.016, &measure, &ps, store.view());
        assert_eq!(menu.page_path(), [TrackPage::OtherLanguages, TrackPage::Language(11)], "an unchanged signature leaves it");

        // one French sidecar is no longer offered: still a page, refreshed in place
        let store2 = store_with(sidecar_french(["/a.srt", "/b.srt", ""]));
        menu.update(0.016, &measure, &ps, store2.view());
        assert_eq!(menu.page_path(), [TrackPage::OtherLanguages, TrackPage::Language(11)], "French is still offered");
        assert_eq!(menu.selected_id(), Some(TrackRow::Sub(2)), "on the row the viewer was on");
        assert_eq!(menu.ids().len(), 2, "the departed track left the page");

        // none of French is offered: its page cannot be listed any more
        let store3 = store_with(sidecar_french(["", "", ""]));
        menu.update(0.016, &measure, &ps, store3.view());
        assert!(menu.page_path().is_empty(), "popped to the root");
        assert_eq!(menu.selected_id(), Some(TrackRow::OpenOther), "on the row that opened the stack");
        assert_eq!(menu.form.table.title(), None);
        teardown(&ps);
    }

    /// With no language left to list, an open Other languages page pops too.
    #[test]
    fn the_other_languages_page_pops_when_nothing_is_left_to_list() {
        let _g = crate::testlock::serial();
        let (mut menu, ps, store) = open(subs());
        focus_id(&mut menu, TrackRow::OpenOther);
        menu.on_ok(store.view());
        let store2 = store_with(vec![stream(10, 0, "English", "eng", "")]);
        menu.update(0.016, &crate::ui::fixture::FixtureMeasure, &ps, store2.view());
        assert!(menu.page_path().is_empty());
        assert!(menu.form.index_of(&TrackRow::OpenOther).is_none(), "and the root no longer offers it");
        teardown(&ps);
    }

    /// Open the Other languages page, then `name`'s drill-in, the way a viewer's keys would.
    fn open_language_named(menu: &mut TrackMenuState, store: &crate::stores::metadata::MetadataStore, name: &str) {
        focus_id(menu, TrackRow::OpenOther);
        assert_eq!(menu.on_ok(store.view()), TrackOk::Navigated);
        let at = (0..menu.form.table.n_rows().max(0) as usize)
            .find(|&i| {
                matches!(menu.form.id_at(i), Some(TrackRow::OpenLang(_)))
                    && menu.form.table.sections.iter().flat_map(|s| &s.rows).nth(i).is_some_and(|r| r.label == name)
            })
            .unwrap_or_else(|| panic!("{name} has no drill-in on the Other languages page"));
        menu.focus_row(at as c_int);
        assert_eq!(menu.on_ok(store.view()), TrackOk::Navigated);
    }

    /// **Finding 1: the harness can pick a subtitle that sits behind Other languages.** An item
    /// whose subs are all non-"yours" has no root track row at all; `track:N` still resolves, and
    /// commits by the track's own id.
    #[test]
    fn the_harness_picks_a_subtitle_behind_other_languages() {
        let _g = crate::testlock::serial();
        let (mut menu, ps, store) = open(vec![stream(21, 0, "German", "deu", ""), stream(22, 1, "Dutch", "nld", "")]);
        // page order: Dutch (A-Z) is the first Other-languages row, German the second
        let i = menu.sub_track_for_target("track:0").expect("track:0 resolves behind Other languages");
        assert_eq!(i, 1, "track N follows page order, root tracks first");
        match menu.commit_sub_track(i, store.view()) {
            TrackOk::Commit { commit: TrackCommit::Subtitle { stream_id, .. }, .. } => assert_eq!(stream_id, 22),
            other => panic!("expected a subtitle commit, got {other:?}"),
        }
        assert_eq!(menu.active_sub, 1);
        assert_eq!(menu.sub_track_for_target("track:1"), Some(0));
        assert_eq!(menu.sub_track_for_target("track:2"), None);
        teardown(&ps);
    }

    /// `track:N` counts root tracks first, then Other languages in page order, expanding a
    /// multi-track language's own ranked page.
    #[test]
    fn the_harness_track_order_is_root_then_other_pages_expanded() {
        let _g = crate::testlock::serial();
        let (menu, ps, _store) = open(subs());
        // root: English(0); Other A-Z: Dutch(5), French full/SDH/forced (1,2,3), German(4)
        let order: Vec<_> = (0..7).map(|n| menu.sub_track_for_target(&format!("track:{n}"))).collect();
        assert_eq!(order, [Some(0), Some(5), Some(1), Some(2), Some(3), Some(4), None]);
        teardown(&ps);
    }

    /// **Finding 2: the menu opens on Other languages when the active track is behind it** — a
    /// codeless or unrecognised-language subtitle is never "yours", so its own row is not on the
    /// root.
    #[test]
    fn opening_with_the_active_track_behind_other_languages_focuses_that_row() {
        let _g = crate::testlock::serial();
        let (mut menu, ps, store) = open(vec![stream(10, 0, "English", "eng", ""), stream(11, 1, "Unknown", "", "")]);
        menu.active_sub = 1;
        menu.rebuild(&ps, store.view(), 1, false);
        assert_eq!(menu.selected_id(), Some(TrackRow::OpenOther));
        teardown(&ps);
    }

    /// The same landing on the live poll's fallback, when the viewer's own row left the root.
    #[test]
    fn the_poll_fallback_lands_on_other_languages_when_the_active_track_is_behind_it() {
        use crate::route::{enhancement_test_session, EnhTestFixture, SubtitleEffect};
        let _g = crate::testlock::serial();
        let _ = crate::player::sidecar::reset();
        // cur_sub_sid is 999: no such stream yet
        let (ps, _sid) = enhancement_test_session(EnhTestFixture { subtitle_effect: SubtitleEffect::Embedded, ..Default::default() });
        let yours = vec!["eng".into(), "spa".into(), "fra".into()];
        let store = store_with(vec![
            stream(10, 0, "English", "eng", ""),
            stream(11, 1, "Spanish", "spa", ""),
            stream(12, 2, "French", "fra", ""),
        ]);
        let mut menu = TrackMenuState::new(&ps, store.view(), 1, yours);
        focus_id(&mut menu, TrackRow::Sub(2));
        // the three languages leave, and the playing subtitle (999) is a codeless track
        let store2 = store_with(vec![stream(999, 0, "Unknown", "", "")]);
        menu.update(0.016, &crate::ui::fixture::FixtureMeasure, &ps, store2.view());
        assert_eq!(menu.active_sub, 0);
        assert_eq!(menu.selected_id(), Some(TrackRow::OpenOther), "the viewer's row is gone: the checked row, which is Other languages");
        teardown(&ps);
    }

    /// **Finding 3: a language page is its language, not a list position.** Two earlier tracks
    /// leave while French's page is open and Dutch now starts where French used to; the page must
    /// stay French (it must never show Dutch under a French stack).
    #[test]
    fn a_language_page_survives_earlier_tracks_leaving() {
        let _g = crate::testlock::serial();
        let before = vec![
            stream(10, 0, "English", "eng", ""),
            stream(11, 1, "German", "deu", ""),
            stream(12, 2, "French", "fra", ""),
            stream(13, 3, "French", "fra", "SDH"),
            stream(14, 4, "Dutch", "nld", ""),
            stream(15, 5, "Dutch", "nld", "SDH"),
        ];
        let (mut menu, ps, store) = open(before);
        open_language_named(&mut menu, &store, "French");
        assert_eq!(menu.form.table.title(), Some("French"));
        let after = store_with(vec![
            stream(12, 0, "French", "fra", ""),
            stream(13, 1, "French", "fra", "SDH"),
            stream(14, 2, "Dutch", "nld", ""),
            stream(15, 3, "Dutch", "nld", "SDH"),
        ]);
        menu.update(0.016, &crate::ui::fixture::FixtureMeasure, &ps, after.view());
        if !menu.page_path().is_empty() {
            assert_eq!(menu.form.table.title(), Some("French"), "a page stays its language");
            assert_eq!(menu.ids(), [TrackRow::Sub(0), TrackRow::Sub(1)], "and lists its tracks");
        }
        teardown(&ps);
    }

    /// If the page's language is gone altogether the page pops to the root.
    #[test]
    fn a_language_page_pops_when_its_language_is_gone_whatever_the_indices() {
        let _g = crate::testlock::serial();
        let (mut menu, ps, store) = open(subs());
        open_language_named(&mut menu, &store, "French");
        let after = store_with(vec![
            stream(10, 0, "English", "eng", ""),
            stream(14, 1, "German", "deu", ""),
            pgs(stream(15, 2, "Dutch", "nld", "")),
            stream(16, 3, "Dutch", "nld", "SDH"),
        ]);
        menu.update(0.016, &crate::ui::fixture::FixtureMeasure, &ps, after.view());
        assert!(menu.page_path().is_empty(), "French is gone: popped to the root");
        teardown(&ps);
    }

    /// **Finding 4, shrink**: French drops to one offered track while its page is open. The page
    /// stays (a language page lists whatever its language still offers), and a pop returns to the
    /// Other languages page on French's now-direct row.
    #[test]
    fn a_language_page_that_drops_to_one_track_stays_and_pops_onto_the_single_row() {
        let _g = crate::testlock::serial();
        let store = store_with(sidecar_french(["/a.srt", "/b.srt", "/c.srt"]));
        let _ = crate::player::sidecar::reset();
        let ps = crate::route::PlaybackSession::IDLE;
        let mut menu = TrackMenuState::new(&ps, store.view(), 1, vec!["eng".into()]);
        open_language_named(&mut menu, &store, "French");
        let store2 = store_with(sidecar_french(["/a.srt", "", ""]));
        menu.update(0.016, &crate::ui::fixture::FixtureMeasure, &ps, store2.view());
        assert_eq!(menu.page_path().len(), 2, "the page stays open at one track");
        assert_eq!(menu.form.table.title(), Some("French"));
        assert_eq!(menu.ids(), [TrackRow::Sub(1)]);
        assert!(menu.pop(&ps, store2.view()));
        assert_eq!(menu.page_path(), [TrackPage::OtherLanguages]);
        assert_eq!(menu.selected_id(), Some(TrackRow::Sub(1)), "French is a direct row now");
        teardown(&ps);
    }

    /// **Finding 4, grow**: French is a single direct row on the Other languages page and gains
    /// tracks while the viewer sits on it; focus follows it to its drill-in.
    #[test]
    fn a_single_track_language_that_grows_keeps_the_focus_on_its_new_row() {
        let _g = crate::testlock::serial();
        let store = store_with(sidecar_french(["/a.srt", "", ""]));
        let _ = crate::player::sidecar::reset();
        let ps = crate::route::PlaybackSession::IDLE;
        let mut menu = TrackMenuState::new(&ps, store.view(), 1, vec!["eng".into()]);
        focus_id(&mut menu, TrackRow::OpenOther);
        menu.on_ok(store.view());
        focus_id(&mut menu, TrackRow::Sub(1));
        let store2 = store_with(sidecar_french(["/a.srt", "/b.srt", "/c.srt"]));
        menu.update(0.016, &crate::ui::fixture::FixtureMeasure, &ps, store2.view());
        assert_eq!(menu.page_path(), [TrackPage::OtherLanguages]);
        let french = menu.ids().into_iter().find(|id| matches!(id, TrackRow::OpenLang(_)));
        assert!(french.is_some(), "French is a drill-in now");
        assert_eq!(menu.selected_id(), french, "French, in its new shape");
        teardown(&ps);
    }

    /// The canon tells the three new pages and their openers apart.
    #[test]
    fn the_canon_tells_the_language_pages_apart() {
        let _g = crate::testlock::serial();
        let hash = |menu: &TrackMenuState| {
            let mut c = Canon::new();
            menu.canon(&mut c);
            c.finish()
        };
        let (mut menu, ps, store) = open(subs());
        focus_id(&mut menu, TrackRow::OpenOther);
        let root = hash(&menu);
        menu.on_ok(store.view());
        let other = hash(&menu);
        focus_id(&mut menu, TrackRow::OpenLang(LangId { stream: 11, slot: 1 }));
        let other_french = hash(&menu);
        menu.on_ok(store.view());
        let french = hash(&menu);
        let all = [root, other, other_french, french];
        for (i, a) in all.iter().enumerate() {
            for b in &all[i + 1..] {
                assert_ne!(a, b);
            }
        }
        teardown(&ps);
    }

    /// **Every new page fits the panel in every shipped language**: the root with its Other
    /// languages row (also while the active track lives behind it), the page, and a language page,
    /// judged by the same gates as the rest of the menu.
    #[test]
    fn the_language_pages_fit_the_panel_in_every_language() {
        use crate::i18n::{language_on_this_thread_for_test, Preference};
        let mut out = Vec::new();
        for language in [Preference::En, Preference::Es, Preference::Be] {
            let _g = crate::testlock::serial();
            let _guard = language_on_this_thread_for_test(language);
            let (mut menu, ps, store) = open(subs_with_vobsub());
            let judge = |menu: &TrackMenuState, out: &mut Vec<_>| {
                out.extend(menu.form.table.menu_cap_failure(&crate::fontcov::advances::ShippedMeasure, language.tag()));
                out.extend(menu.form.table.app_fit_failures(crate::ui::table::MENU_MAX_W, language.tag()));
                out.extend(menu.form.table.app_fit_failures_hugged(language.tag()));
            };
            judge(&menu, &mut out);
            for active in [-1, 2] {
                menu.active_sub = active;
                menu.rebuild(&ps, store.view(), 1, false);
                judge(&menu, &mut out);
                menu.push(TrackPage::OtherLanguages);
                judge(&menu, &mut out);
                menu.push(TrackPage::Language(11));
                judge(&menu, &mut out);
                menu.pop(&ps, store.view());
                menu.push(TrackPage::Language(16)); // the VobSub + SDH row
                judge(&menu, &mut out);
            }
            teardown(&ps);
        }
        crate::ui::table::assert_no_fit_failures(&out);
    }
}

/// **The panel's resize and page slide** (`ui::panel_motion`, `docs/player-submenus.md`,
/// "Animation"): running vs resting, an interrupted transition, the layout the card lands on, and
/// the Audio/Subtitles tab switch.
#[cfg(test)]
mod motion_tests {
    use super::tests::{store_with, stream};
    use super::*;
    use crate::fontcov::advances::ShippedMeasure;
    use crate::route::reset_player_control_for_test;

    const DT: f32 = 1.0 / 60.0;

    fn teardown(ps: &crate::route::PlaybackSession) {
        reset_player_control_for_test(ps);
        crate::plex::reset_servers_for_test();
    }

    fn subs() -> Vec<metadata::Stream> {
        vec![
            stream(10, 0, "English", "eng", ""),
            stream(11, 1, "French", "fra", ""),
            stream(12, 2, "French", "fra", "SDH"),
            stream(14, 4, "German", "deu", ""),
        ]
    }

    fn open() -> (TrackMenuState, crate::route::PlaybackSession, crate::stores::metadata::MetadataStore) {
        let _ = crate::player::sidecar::reset();
        let ps = crate::route::PlaybackSession::IDLE;
        let store = store_with(subs());
        let mut menu = TrackMenuState::new(&ps, store.view(), 1, vec!["eng".into()]);
        // the first update places the card AT its layout: the open is the appear animation's
        frame(&mut menu, &ps, &store);
        assert!(!menu.transitioning(), "a freshly opened panel is at rest");
        (menu, ps, store)
    }

    /// One loop frame, as the dispatcher runs it: forget last frame's motion, then update. Returns
    /// whether a spring moved (what keeps the present gate awake).
    fn frame(menu: &mut TrackMenuState, ps: &crate::route::PlaybackSession, store: &crate::stores::metadata::MetadataStore) -> bool {
        crate::ui::idle::frame_begin(DT);
        menu.update(DT, &ShippedMeasure, ps, store.view());
        crate::ui::idle::present_moving()
    }

    fn run(menu: &mut TrackMenuState, ps: &crate::route::PlaybackSession, store: &crate::stores::metadata::MetadataStore, n: usize) {
        for _ in 0..n {
            frame(menu, ps, store);
        }
    }

    fn shown(menu: &TrackMenuState) -> Rect {
        menu.shown_rect(&ShippedMeasure)
    }

    fn natural(menu: &TrackMenuState) -> Rect {
        menu.panel_rect(&ShippedMeasure)
    }

    fn push_style(menu: &mut TrackMenuState, store: &crate::stores::metadata::MetadataStore) {
        let i = menu.form.index_of(&TrackRow::Style).expect("Style row");
        menu.focus_row(i as c_int);
        assert_eq!(menu.on_ok(store.view()), TrackOk::Navigated);
    }

    /// **A push animates, reports motion each frame it runs and stops asking for frames at rest.**
    #[test]
    fn a_push_is_animating_then_asks_for_no_more_frames_at_rest() {
        let _g = crate::testlock::serial();
        let (mut menu, ps, store) = open();
        let root = natural(&menu);
        push_style(&mut menu, &store);
        assert!(menu.transitioning(), "the page slide is running the moment the page is pushed");
        assert!(frame(&mut menu, &ps, &store), "a running transition keeps the present gate awake");
        assert!(menu.motion.sliding());
        assert_ne!(shown(&menu), natural(&menu), "the card is between the two layouts");
        assert_ne!(natural(&menu), root, "the Style page has its own layout");

        let mut frames_moving = 1;
        while frame(&mut menu, &ps, &store) {
            frames_moving += 1;
            assert!(frames_moving < 240, "the transition never settles");
        }
        assert!((6..120).contains(&frames_moving), "a transition spans a handful of frames, not {frames_moving}");
        assert!(!menu.transitioning());
        assert!(!menu.motion.sliding());
        assert_eq!(shown(&menu), natural(&menu), "the card lands EXACTLY on the layout");
        // idle is truly idle: a settled panel steps without a single frame requested
        for _ in 0..30 {
            assert!(!frame(&mut menu, &ps, &store), "a settled panel must not ask for frames");
        }
        teardown(&ps);
    }

    /// **A push then a pop mid-slide reverses from where it is and settles on the root.** The
    /// page that was arriving becomes the one returned to, carrying its own alpha, and the page
    /// that was live leaves from its own: no layer's alpha steps at the swap.
    #[test]
    fn an_interrupted_push_then_pop_reverses_and_settles_at_the_root_rect() {
        let _g = crate::testlock::serial();
        let (mut menu, ps, store) = open();
        let root = natural(&menu);
        let root_shown = shown(&menu);
        assert_eq!(root_shown, root);
        push_style(&mut menu, &store);
        run(&mut menu, &ps, &store, 11);
        let mid = shown(&menu);
        assert_ne!(mid, root, "mid-slide the card is not at the root layout");
        let before = menu.motion.live_dx();
        assert!(before > 0.0, "a push arrives from the right: {before}");
        let (leaving_before, live_before) = menu.motion.alphas();
        assert!(live_before > 0.0 && live_before < 1.0, "caught mid-fade: {live_before}");

        assert!(menu.pop(&ps, store.view()));
        assert_eq!(shown(&menu), mid, "the reversal continues from the drawn rect, no jump");
        assert!(menu.motion.sliding(), "the slide reverses rather than ending");
        let (leaving_after, live_after) = menu.motion.alphas();
        assert_eq!(leaving_after, vec![live_before], "the page that was live leaves from its own alpha");
        let revived = leaving_before.last().copied().unwrap_or(0.0);
        assert_eq!(live_after, revived, "the page returned to continues from its own alpha");
        let after = menu.motion.live_dx();
        assert!(after <= 0.0, "a pop returns the root from the left, not the right: {after}");
        assert!(menu.transitioning());

        let mut n = 0;
        while frame(&mut menu, &ps, &store) {
            n += 1;
            assert!(n < 240, "the reversed transition never settles");
        }
        assert_eq!(shown(&menu), root, "settles on the root layout");
        assert!(!menu.transitioning());
        assert!(menu.pages.is_empty());
        assert_eq!(menu.selected_id(), Some(TrackRow::Style), "the opener is focused again");
        teardown(&ps);
    }

    /// **The two pages never read on top of each other**: on every frame of a push, a pop and an
    /// interrupted one, at most one layer is above the gate alpha (`panel_motion::GATE`), and the leaving page draws no
    /// focus pill (the arriving page owns the only one).
    #[test]
    fn the_two_pages_are_never_both_above_the_gate_and_only_one_draws_a_pill() {
        let _g = crate::testlock::serial();
        let (mut menu, ps, store) = open();
        let check = |menu: &TrackMenuState, what: &str| {
            let (leaving, live) = menu.motion.alphas();
            let loudest = leaving.iter().copied().fold(0.0_f32, f32::max);
            assert!(loudest.min(live) <= crate::ui::panel_motion::GATE + 1e-4, "{what}: leaving {leaving:?} and live {live} overlap");
            assert_eq!(menu.motion.leaving_pills(), 0, "{what}: a leaving page draws a focus pill");
        };
        push_style(&mut menu, &store);
        let mut saw_leaving_only = false;
        let mut saw_live_only = false;
        for i in 0..90 {
            frame(&mut menu, &ps, &store);
            check(&menu, &format!("push frame {i}"));
            let (leaving, live) = menu.motion.alphas();
            saw_leaving_only |= !leaving.is_empty() && live == 0.0;
            saw_live_only |= leaving.is_empty() && live > 0.0 && live < 1.0;
        }
        assert!(saw_leaving_only, "the outgoing page is alone on screen at first");
        assert!(saw_live_only, "and the incoming page alone at the end of its fade");
        assert!(menu.pop(&ps, store.view()));
        for i in 0..90 {
            frame(&mut menu, &ps, &store);
            check(&menu, &format!("pop frame {i}"));
        }
        // interrupted at every depth of a push
        for stop in [1, 3, 6, 9, 12, 20] {
            push_style(&mut menu, &store);
            run(&mut menu, &ps, &store, stop);
            check(&menu, &format!("before the interrupt at {stop}"));
            assert!(menu.pop(&ps, store.view()));
            check(&menu, &format!("right after the interrupt at {stop}"));
            for i in 0..90 {
                frame(&mut menu, &ps, &store);
                check(&menu, &format!("interrupted at {stop}, frame {i}"));
            }
        }
        teardown(&ps);
    }

    /// **A push during a push is continuous too**: the page that was arriving leaves from the
    /// alpha and offset it had, the older leaving page keeps fading, and the new page starts clear.
    #[test]
    fn a_push_during_a_push_steps_no_layer() {
        let _g = crate::testlock::serial();
        let (mut menu, ps, store) = open();
        push_style(&mut menu, &store);
        run(&mut menu, &ps, &store, 12);
        let (old_leaving, live) = menu.motion.alphas();
        let dx = menu.motion.live_dx();
        assert!(live > 0.0 && live < 1.0, "mid-fade: {live}");
        let i = menu.form.index_of(&TrackRow::OpenField(StyleField::Size)).expect("Size row");
        menu.focus_row(i as c_int);
        assert_eq!(menu.on_ok(store.view()), TrackOk::Navigated);
        let (leaving, new_live) = menu.motion.alphas();
        assert_eq!(new_live, 0.0, "the new page arrives from transparent");
        assert_eq!(leaving.last().copied(), Some(live), "the page that was arriving leaves from its own alpha");
        for (kept, was) in leaving.iter().zip(old_leaving.iter()) {
            assert_eq!(kept, was, "an older leaving page is not touched by the swap");
        }
        assert_eq!(menu.motion.leaving_pills(), 0);
        assert_ne!(dx, 0.0);
        run(&mut menu, &ps, &store, 240);
        assert!(!menu.motion.sliding());
        assert_eq!(shown(&menu), natural(&menu));
        teardown(&ps);
    }

    /// Rapid repeated pushes and pops (a held key, a double press) never stack layers or leave the
    /// card off its layout.
    #[test]
    fn rapid_pushes_and_pops_leave_one_slide_and_land_on_the_logical_page() {
        let _g = crate::testlock::serial();
        let (mut menu, ps, store) = open();
        let root = natural(&menu);
        for _ in 0..4 {
            push_style(&mut menu, &store);
            frame(&mut menu, &ps, &store);
            assert!(menu.pop(&ps, store.view()));
            frame(&mut menu, &ps, &store);
        }
        // and a push that goes deeper while the first is still sliding
        push_style(&mut menu, &store);
        run(&mut menu, &ps, &store, 2);
        let i = menu.form.index_of(&TrackRow::OpenField(StyleField::Size)).expect("Size row");
        menu.focus_row(i as c_int);
        assert_eq!(menu.on_ok(store.view()), TrackOk::Navigated);
        assert_eq!(menu.pages.len(), 2);
        run(&mut menu, &ps, &store, 240);
        assert!(!menu.transitioning());
        assert_eq!(shown(&menu), natural(&menu));
        assert!(menu.pop(&ps, store.view()));
        assert!(menu.pop(&ps, store.view()));
        run(&mut menu, &ps, &store, 240);
        assert_eq!(shown(&menu), root);
        assert!(!menu.transitioning());
        teardown(&ps);
    }

    /// An Audio/Subtitles tab switch resizes the card with the same spring, and the content
    /// does not slide.
    #[test]
    fn a_tab_switch_animates_the_height() {
        let _g = crate::testlock::serial();
        let (mut menu, ps, store) = open();
        let tall = natural(&menu);
        menu.focus_tab(&ps, store.view(), 0);
        let short = natural(&menu);
        assert!(short.h < tall.h, "the empty Audio tab is shorter than the Subtitles tab: {} < {}", short.h, tall.h);
        assert_eq!(short.y + short.h, tall.y + tall.h, "the bottom edge is the anchor");
        assert_eq!(short.x + short.w, tall.x + tall.w, "and so is the right edge");
        assert!(frame(&mut menu, &ps, &store), "the resize is motion");
        let first = shown(&menu);
        assert!(first.h > short.h && first.h <= tall.h, "mid-resize the card is between the two heights: {first:?}");
        assert!(!menu.motion.sliding(), "a tab switch swaps the content, it does not slide it");
        assert_eq!(menu.motion.live_dx(), 0.0);
        run(&mut menu, &ps, &store, 240);
        assert_eq!(shown(&menu), short);
        // and back
        menu.focus_tab(&ps, store.view(), 1);
        assert!(frame(&mut menu, &ps, &store));
        run(&mut menu, &ps, &store, 240);
        assert_eq!(shown(&menu), tall);
        assert!(!frame(&mut menu, &ps, &store));
        teardown(&ps);
    }

    /// The layout target is measured when the table changed and not otherwise.
    #[test]
    fn the_layout_is_cached_until_the_table_changes() {
        let _g = crate::testlock::serial();
        let (mut menu, ps, store) = open();
        let rev = menu.form.table.layout_rev();
        let a = natural(&menu);
        run(&mut menu, &ps, &store, 3);
        assert_eq!(menu.form.table.layout_rev(), rev, "update/draw/place do not touch the table's layout");
        assert_eq!(natural(&menu), a);
        push_style(&mut menu, &store);
        assert_ne!(menu.form.table.layout_rev(), rev, "a page change moves the revision");
        teardown(&ps);
    }
}
