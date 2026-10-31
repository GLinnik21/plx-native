//! **`HudModel`: the per-frame, read-only snapshot of every value the HUD's presentation reads.**
//!
//! Before the split the draw path read process globals in the middle of drawing: `TX.paused`, the
//! scrub target, `playpos_ns()`, `seek_display_ns()`, `duration_ns()`, `loading(ps)` and the
//! subtitle picks and bitmap cue. The player screen is a machine and reads no global
//! (`docs/sans-io.md`), so those reads become fields of this value, which `src/app/run.rs` fills ONCE
//! a frame and pushes into the screen the way it already pushes `busy` and `transport`. The renderer
//! reads it through its `Frame`, and the scrub preview takes it directly; a draw reads no player global.
//!
//! Pure data: every field is `Copy`, there is no borrowed string and no allocation, so building one is
//! a dozen atomic loads and the snapshot costs nothing to hand down the draw. It carries no
//! `PartialEq` because of [`HudModel::bitmap_by_key`], a function pointer.
//!
//! The model holds exactly what presentation reads. What the HUD's STATE logic samples for itself
//! ([`super::slot`], [`super::busy`], [`super::transport_hidden`], `HudMotion::step`) stays in
//! `state.rs` and `motion.rs`: those run at input and Tick time, and moving them onto a model sampled
//! at prepare time would shift them by a frame.

use plx_plex::plex::session::{SubtitlePosition, SubtitleSize, SubtitleTone};

/// The image-subtitle cue lookup [`HudModel::bitmap_by_key`] injects: the canvas size and the
/// rects of the selected track's display set with this `start_ns` key.
pub type BitmapLookup = fn(i64) -> Option<(i32, i32, Vec<plx_media::player::SubRect>)>;

/// Every global the HUD's presentation reads, sampled once for the frame. The doc comment of each
/// field quotes the exact expression `src/app/run.rs` samples it with.
#[derive(Clone, Copy, Debug)]
pub struct HudModel {
    /// `plx_media::player::TX.paused.load(Relaxed)`: the viewer is logically paused. Drives the
    /// title's reveal and the playbar's state mark.
    pub paused: bool,
    /// `plx_media::player::TX.scrub_ns.load(Relaxed)`: the live scrub preview's target, -1 when no
    /// scrub is in progress.
    pub scrub_ns: i64,
    /// `plx_media::player::playpos_ns()`: the PUBLISHED playhead.
    pub playpos_ns: i64,
    /// `plx_media::player::seek_display_ns()`: the target a loading seek freezes the playhead at,
    /// -1 when none.
    pub seek_display_ns: i64,
    /// `plx_media::player::duration_ns()`.
    pub duration_ns: i64,
    /// `plx_media::player::loading(ps)`: a seek is resolving (the playbar shows `seek_display_ns`).
    pub loading: bool,
    /// `plx_media::route::subtitle_size()`: the viewer's caption size pick.
    pub sub_size: SubtitleSize,
    /// `plx_media::route::subtitle_position()`: the viewer's caption position pick.
    pub sub_position: SubtitlePosition,
    /// `plx_media::player::subtitle_tone()`: the caption ink's tone.
    pub sub_tone: SubtitleTone,
    /// `plx_media::player::desired_sub_idx()`: the selected subtitle track, -1 when off.
    pub sub_sel: i32,
    /// `plx_media::player::active_bitmap_key(plx_media::player::playpos_ns())`: the `start_ns` key
    /// of the selected track's image cue active now, `None` between cues.
    pub bitmap_key: Option<i64>,
    /// `Controls::now(ps)`: which of the Previous/Next pair the playing episode has, and whether
    /// Previous restarts. The enabled predicates of `Previous` and `Next` read it
    /// ([`super::command::HudCommand::enabled`]).
    pub controls: super::state::Controls,
    /// `plx_appkit::chapters_panel::has_chapters(meta)`: the playing item has chapters.
    pub chapters: bool,
    /// `plx_appkit::info_panel::detail_rk(meta).is_some()`: the playing item has a ratingKey to
    /// open the page of (`go_to_detail`).
    pub detail: bool,
    /// The viewer's Skip interval in whole seconds (`plx_media::route::skip_interval().ns() / 1e9`): the
    /// rung a `seek by: setting` control's `by_interval` face is picked at.
    pub skip_secs: u32,
    /// Stats for nerds is up (`app::diagnostics::enabled()`): the `on` of a `toggle_stats` face.
    pub stats: bool,
    /// `plx_media::player::bitmap_by_key`: the lookup `draw_subtitle_bitmap` calls when the key
    /// changes. Injected so this module never names the player.
    pub bitmap_by_key: BitmapLookup,
}

/// The lookup an idle model carries: no cue.
fn no_bitmap(_key: i64) -> Option<(i32, i32, Vec<plx_media::player::SubRect>)> {
    None
}

impl Default for HudModel {
    /// What an idle process reads from those globals: their initialisers. Not paused, no scrub and no
    /// seek target (-1), the playhead and duration at 0, nothing loading, the default caption picks
    /// (Medium, Low, white), no subtitle track (-1) and no cue.
    fn default() -> Self {
        Self {
            paused: false,
            scrub_ns: -1,
            playpos_ns: 0,
            seek_display_ns: -1,
            duration_ns: 0,
            loading: false,
            sub_size: SubtitleSize::default(),
            sub_position: SubtitlePosition::default(),
            sub_tone: SubtitleTone::default(),
            sub_sel: -1,
            controls: super::state::Controls::NONE,
            chapters: false,
            detail: false,
            skip_secs: 10,
            stats: false,
            bitmap_key: None,
            bitmap_by_key: no_bitmap,
        }
    }
}
