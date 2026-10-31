//! **The HUD's logic: no pixels.** Which control occupies the row ([`slot`], [`ControlSlot`],
//! [`Controls`]), the busy and transport marks ([`Busy`], [`TransportMark`]), the failure read-out's
//! rows and actions ([`FailureReadout`], [`Action`]) and the scrub tuning ([`scrub_speed`],
//! [`scrub_clamp_target`], [`SkipIntervalGuard`]).
//!
//! `mod.rs` re-exports all of it (`pub use state::*`), so every item keeps its `player_hud::NAME`
//! path. Where a layout is needed to answer (a rect, a drawn width) the answer lives in
//! [`super::classic`], which implements the geometry methods of these types; this file names no
//! layout constant.
use plx_ui::widgets::StatusKind;
use std::ffi::CString;
use std::os::raw::c_int;

/// The position the playbar SHOWS: a loading seek's target, else the live scrub preview, else the
/// real playhead. One function because the draw ([`Playbar::live`]) and the clock prewarm
/// ([`ClockWarm::queue`](super::motion::ClockWarm::queue)) must resolve the same value, or the prewarm warms the wrong string.
pub(super) fn display_pos_ns(loading: bool, seekdisp: i64, scrub: i64, livepos: i64) -> i64 {
    if loading && seekdisp >= 0 {
        seekdisp
    } else if scrub >= 0 {
        scrub
    } else {
        livepos
    }
}

/// Stable slots: Previous, Next, Subtitles, Audio, More. The first pair can be absent.
pub const BTN_N: i32 = 5;
pub const BTN_PREVIOUS: i32 = 0;
pub const BTN_NEXT: i32 = 1;
pub const BTN_SUBTITLES: i32 = 2;
pub const BTN_AUDIO: i32 = 3;

/// Index of the `…` overflow disc within the row — the LAST one. Exported because `app.rs` routes
/// its OK and its click, and a second literal `2` over there is exactly the drift `ControlSlot`
/// was introduced to stop.
pub const BTN_MORE: i32 = BTN_N - 1;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action { Previous, Next, Subtitles, Audio, More }

/// A frame's episode-button eligibility, availability and restart glyph, sampled once.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Controls {
    pub previous: bool,
    pub next: bool,
    pub restart: bool,
}
impl Controls {
    pub const NONE: Self = Self { previous: false, next: false, restart: false };
    pub fn now(ps: &plx_media::route::PlaybackSession) -> Self {
        if plx_media::route::cur_kind(ps) != "episode" { return Self::NONE; }
        let (previous, next) = plx_media::route::queue_neighbors(ps);
        if !(previous || next) { return Self::NONE; }
        let restart = plx_media::route::previous_restarts(plx_media::player::intended_pos_ns(ps));
        Self::for_item(true, previous, next, restart)
    }
    pub fn for_item(episode: bool, previous: bool, next: bool, restart: bool) -> Self {
        if !episode || !(previous || next) { return Self::NONE; }
        Self { previous: previous || restart, next, restart }
    }
    /// Whether the Previous/Next pair is on the row at all: [`Self::for_item`] only ever builds a
    /// pair with at least one of its two buttons live.
    pub fn pair(self) -> bool {
        self.previous || self.next
    }
    pub fn enabled(self, i: i32) -> bool {
        match i { BTN_PREVIOUS => self.previous, BTN_NEXT => self.next,
            BTN_SUBTITLES..=BTN_MORE => true, _ => false }
    }
    pub fn primary(self) -> i32 {
        if self.enabled(BTN_NEXT) { BTN_NEXT }
        else if self.enabled(BTN_PREVIOUS) { BTN_PREVIOUS }
        else { BTN_SUBTITLES }
    }
    pub fn action(self, i: i32) -> Option<Action> {
        if !self.enabled(i) { return None; }
        match i { BTN_PREVIOUS => Some(Action::Previous), BTN_NEXT => Some(Action::Next),
            BTN_SUBTITLES => Some(Action::Subtitles), BTN_AUDIO => Some(Action::Audio), BTN_MORE => Some(Action::More), _ => None }
    }
    /// What a press of control `i` asks the app to do: its entry of [`super::command::CLASSIC_COMMANDS`],
    /// `None` for a control that is absent or disabled (it has no stop to press).
    pub fn command(self, i: i32) -> Option<super::command::HudCommand> {
        if !self.enabled(i) { return None; }
        super::command::CLASSIC_COMMANDS.get(usize::try_from(i).ok()?).copied()
    }
    pub fn icon(self, i: i32) -> plx_ui::icons::Icon {
        use plx_ui::icons::Icon;
        match i { BTN_PREVIOUS if self.restart => Icon::Restart, BTN_PREVIOUS => Icon::Previous,
            BTN_NEXT => Icon::Next, BTN_AUDIO => Icon::Audio, BTN_MORE => Icon::More, _ => Icon::Cc }
    }
}

/// What currently occupies the transport's right-hand control row.
///
/// This is the one concept the skip / Up Next feature introduces, and it exists as a TYPE because
/// the alternative — re-deriving the three-way choice at each of the draw, the OK handler, the
/// pointer handler, the LEFT/RIGHT pin and `icon_hit` — encodes its precedence in the order of five
/// separate if-chains, with nothing to keep them agreeing and nothing to test.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ControlSlot {
    /// the ordinary Subtitles, Audio and More discs
    Discs(Controls),
    /// a marker segment is under the playhead
    Skip(crate::skip_pill::Prompt),
    /// …and the show has another episode queued, which outranks skipping the credits. Carries the
    /// segment for the same reason `Skip` does — so the row has a stable IDENTITY.
    UpNext(plx_data::metadata::Marker),
}

impl ControlSlot {
    /// Stable slots in this occupant. An absent or disabled disc has no focus stop; `enabled`
    /// and the compile's `items` define those independently of the stable indices.
    pub fn items(self) -> c_int {
        match self {
            ControlSlot::Discs(_) => BTN_N,
            // Watch Credits + Next Episode — the one stand-in with a pair
            ControlSlot::UpNext(_) => 2,
            ControlSlot::Skip(_) => 1,
        }
    }
    /// Where the focus ring parks when this row APPEARS. The discs prefer an enabled Next, then
    /// the first enabled disc. Up Next's primary is drawn on the RIGHT; the countdown's
    /// cancel rule is a steady state (`up_next::countdown_may_run`), so parking on item 0 there
    /// would disarm the timer on the frame after it armed.
    pub fn primary_btn(self) -> c_int {
        match self {
            ControlSlot::UpNext(_) => crate::up_next::PRIMARY_BTN,
            ControlSlot::Discs(controls) => controls.primary(),
            _ => 0,
        }
    }
    pub fn enabled(self, idx: i32) -> bool {
        match self { Self::Discs(controls) => controls.enabled(idx), _ => (0..self.items()).contains(&idx) }
    }
    /// The command a press of item `idx` performs, when the row holds the discs. A stand-in's
    /// items (the Skip pill, the Up Next pair) are app-owned and carry no [`HudCommand`](super::command::HudCommand): what
    /// they do depends on the segment they offer.
    pub fn command(self, idx: i32) -> Option<super::command::HudCommand> {
        match self { Self::Discs(controls) => controls.command(idx), _ => None }
    }
    /// whether the Subtitles/Audio discs are the current occupant
    pub fn is_discs(self) -> bool {
        matches!(self, ControlSlot::Discs(_))
    }
    /// Which SEGMENT this row is offering, if any — the row's identity for "have I already offered
    /// this?". Deliberately not the `ControlSlot` itself: `active_marker` is gated on `is_playing`,
    /// so a momentary drop out of Playing mid-segment reads as "no segment" and flips the slot to
    /// `Discs` and back. Keyed on the segment, that round trip is not a new offer; keyed on the
    /// slot, it was — and every flicker re-raised the HUD over an intro the user was just watching.
    pub fn offer(self) -> Option<(plx_data::metadata::MarkerKind, i64)> {
        match self {
            ControlSlot::Discs(_) => None,
            ControlSlot::Skip(pr) => Some((pr.marker.kind, pr.marker.start_ms)),
            ControlSlot::UpNext(m) => Some((m.kind, m.start_ms)),
        }
    }
}

/// PURE precedence: given the segment under the playhead and whether the queue has a successor,
/// which control owns the row. Host-testable, and the ONLY place the ordering is written down.
///
/// Up Next outranks Skip Credits deliberately: with somewhere to go, "next episode" is the better
/// offer, and Skip Credits stays for the last episode of a show, where there is nowhere to go.
/// Only a `final` credits marker (one that runs to the end of the item) qualifies: a mid-item
/// credits segment is followed by more of the episode, so it keeps the Skip Credits pill.
///
/// The Next episode preference only changes the offer for that final-credits-with-a-successor
/// case: `AfterCredits` and `Off` offer nothing there (the discs stay), because the successor is
/// not announced until the stream ends, if then. The last episode and films keep Skip Credits.
pub fn slot_for(marker: Option<plx_data::metadata::Marker>, has_next: bool, mode: plx_media::route::NextEpisodeMode) -> ControlSlot {
    match marker {
        Some(m) => {
            let pr = crate::skip_pill::prompt_for(m);
            if has_next && m.kind == plx_data::metadata::MarkerKind::Credits && m.final_seg {
                match mode {
                    plx_media::route::NextEpisodeMode::Countdown => ControlSlot::UpNext(m),
                    plx_media::route::NextEpisodeMode::AfterCredits | plx_media::route::NextEpisodeMode::Off => ControlSlot::Discs(Controls::NONE),
                }
            } else {
                ControlSlot::Skip(pr)
            }
        }
        None => ControlSlot::Discs(Controls::NONE),
    }
}

/// Sample the live globals ONCE and resolve the row. Call this once per frame and pass the result
/// around: `playpos_ns` is written by LG's media thread and `player::pump` runs between the input
/// handlers and the draw, so re-deriving per call site let a keypress dispatch to a control that
/// the same frame then declined to draw.
pub fn slot(ps: &plx_media::route::PlaybackSession, meta: plx_data::metadata::MetadataView<'_>) -> ControlSlot {
    let has_next = plx_media::route::up_next(ps).is_some();
    let mode = plx_media::route::next_episode_mode();
    // The data layer does not read the player: the playhead is sampled here, once, and handed in.
    let head = plx_data::metadata::Playhead {
        playing: plx_media::player::is_playing(ps),
        pos_ns: plx_media::player::playpos_ns(),
        dur_ns: plx_media::player::duration_ns(),
    };
    // Server marker first; the synthesized tail only exists where credits DETECTION does not
    // (a Plex Pass server feature) — see `metadata::synthesized_tail_marker` — and only to feed
    // the Up Next countdown, so it is not even computed in the other modes.
    let m = meta.active_marker(head).or_else(|| {
        (mode == plx_media::route::NextEpisodeMode::Countdown)
            .then(|| meta.synthesized_tail_marker(head, has_next))
            .flatten()
    });
    let slot = slot_for(m, has_next, mode);
    if slot.is_discs() { ControlSlot::Discs(Controls::now(ps)) } else { slot }
}

/// PURE edge: has a stand-in just vanished OUT FROM UNDER the focus ring, so the ring has to go
/// back to the scrubber? `focused` is whether the control row currently holds focus. `was_standin`
/// is the occupant on the last OVERLAY-FREE frame, not simply the last frame — the caller only
/// samples it while the transport is bare, so an edge that happens behind an open track menu / Info
/// card is held and fires on the frame the overlay closes, rather than being lost.
///
/// It exists because the row swapping from a Skip pill back to the discs leaves focus on the row
/// with `btn` still 0, so the next OK opens the SUBTITLES menu instead of toggling pause.
///
/// **It is an EDGE, not a steady state**, and that is the whole reason it is a named function with
/// a test. Written inline as `is_discs() && focused` it is also true on every frame of a user who
/// deliberately walked UP to the Subtitles/Audio discs — the ring is then yanked back to the
/// scrubber the same frame it arrives, so focus can never rest on a disc and OK on one is
/// unreachable by remote. The pointer path never saw it, which is why the on-device suite did not.
pub fn standin_left_the_ring(was_standin: bool, now: ControlSlot, focused: bool) -> bool {
    was_standin && now.is_discs() && focused
}

/// Which surface owns the "the pipeline is working" signal. There is exactly ONE, ever.
///
/// Two of them used to fire together for the whole of every load and every seek: the centred
/// [`StatusOverlay`] was gated on `is_busy() && frames() == 0`, but `pump` zeroes `frames` as part
/// of APPLYING a seek — so the guard that was meant to say "we have never had a picture" was true
/// for the entire seek, and the transport's own spinner (gated on plain `is_busy()`) was up beside
/// it. Two indicators for one fact read as two facts, which is exactly how it was reported.
///
/// It carries the caption rather than letting each draw re-read `state().caption()`: `Resolving` is
/// derived from `route::play_pending()`, an off-thread flag, so a second read could disagree with
/// the kind this value was chosen for.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Busy {
    /// nobody — frames are on the panel, or there is no session
    None,
    /// the centred [`StatusOverlay`] at [`readout_frame`], carrying its treatment + its caption
    Readout(StatusKind, &'static core::ffi::CStr),
    /// the inline spinner beside the elapsed clock
    Transport,
}

/// PURE: which surface owns the busy signal, given the playback state and whether this SESSION has
/// ever presented a frame (`player::seen_frame` — NOT `frames() > 0`, see [`Busy`]).
///
/// **The rule: whichever surface owns the thing being waited for.** No picture yet → the wait is
/// about the whole panel, so the whole panel says so. A picture is up and only the POSITION is in
/// flight → the wait is about the playhead, so the mark goes on the playhead, which the HUD has
/// already frozen at the seek target. `Error` → the centred read-out either way; there is no
/// picture and no position, only a message.
///
/// It takes `seen_frame` rather than the state alone because `Buffering` is genuinely ambiguous —
/// it is both the cold-start tail AND the 1-3 frame tail of every seek (between prime→Play, where
/// `engine` clears `seeking`, and the first presented frame). Keyed on the state alone, one of
/// those two flashes the wrong surface every time.
pub fn busy_surface(ps: &plx_media::route::PlaybackSession, st: plx_media::player::PlaybackState, seen_frame: bool) -> Busy {
    busy_surface_with(ps, st, seen_frame, plx_media::player::audio_switching())
}

/// [`busy_surface`] with the audio-switch fact given: while the rebuild loading is the one the
/// viewer's audio-track pick asked for, the read-out says "Switching audio…" instead of
/// "Buffering…" (`player::busy_caption_with`). Any other rebuild keeps the plain caption.
pub fn busy_surface_with(
    ps: &plx_media::route::PlaybackSession,
    st: plx_media::player::PlaybackState,
    seen_frame: bool,
    audio_switch: bool,
) -> Busy {
    use plx_media::player::PlaybackState;
    match st {
        // not `st.caption()`: the Error caption is shaped by WHY (an audio-only stream names the
        // server; see `player::error_shape`), which a method on the bare state cannot know.
        PlaybackState::Error => Busy::Readout(StatusKind::Failed, plx_media::player::error_caption(ps)),
        s if s.is_busy() && !seen_frame => Busy::Readout(StatusKind::Working, plx_media::player::busy_caption_with(st, audio_switch)),
        s if s.is_busy() => Busy::Transport,
        _ => Busy::None,
    }
}

/// PURE: does the read-out own the WHOLE frame, so [`render::draw`](super::render::draw) draws nothing at all?
///
/// **Only a failure**, and the asymmetry is the point. A `Failed` read-out is terminal: there is no
/// picture and no position, so every piece of transport under it is a lie — `Player Screen.dc.html`
/// hides both the transport (`hudDisplay:none`) and the bottom tabs (`tabsDisplay:none`) on that
/// variant. A `Working` read-out is the opposite: the pipeline is mid-flight, the position is real,
/// and the transport is what the user reads the moment the first frame lands. Hiding it there would
/// blank the HUD through every cold start and every pre-roll — which is why this asks the KIND and
/// not merely "is a read-out up".
pub fn readout_owns_frame(busy: Busy) -> bool {
    matches!(busy, Busy::Readout(StatusKind::Failed, _))
}

/// The same question for `crate::screens::player::PlayerScreen::handle_key`, which asks it
/// FIRST, before any transport arm: is the transport (and every panel over it) absent from the
/// frame right now?
///
/// It was `app/run.rs`'s own guard, high in that ladder's chain, until restructure phase 12
/// (PX-PLAYER) retired the loop's player input path — so the precedence that used to be an arm's
/// HEIGHT is one condition at the top of one function.
///
/// A control that is not drawn must not be activatable — the rule `PlayerScreen::record_stops` keeps for
/// the pointer, at the row's own altitude. Without this the failure was hidden but
/// still drivable: `start_playback` stamps a ~4.5 s HUD linger on the way in, so a `/decision`
/// refusal lands with `hud_visible` true and focus parked on the scrubber, and two blind presses
/// (DOWN, OK) opened the Info card over the read-out from a tab row nothing had painted.
///
/// It resamples [`busy`] rather than taking one, because the event loop runs before the frame's
/// single resolve exists; both reads are of the same main-thread state within one iteration.
pub fn transport_hidden(ps: &plx_media::route::PlaybackSession) -> bool {
    readout_owns_frame(busy(ps))
}

/// Sample the live globals ONCE and resolve the owner. Call this once per frame and pass the result
/// to both [`render::draw`](super::render::draw) and [`draw_readout`] — the same discipline [`slot`] keeps, and for the same
/// reason: two independent derivations of one three-way choice is how the two indicators drifted
/// apart in the first place.
pub fn busy(ps: &plx_media::route::PlaybackSession) -> Busy {
    // dev: `/tmp/plxnative-failtest` forces the failure read-out — the other half of
    // `player::failtest_arm`, which shapes WHICH failure. It is forced HERE, on the one impure
    // sampler, rather than in `player::state()`: the pump acts on that state, and a dev switch
    // that made the engine believe it had failed would be testing a different thing than the
    // screen. `busy_surface` stays pure and ungated, so what draws is still the real rule.
    if plx_base::devtrig::flag("failtest") {
        return Busy::Readout(StatusKind::Failed, plx_media::player::error_caption(ps));
    }
    busy_surface(ps, plx_media::player::state(ps), plx_media::player::seen_frame())
}

// ---- the transport STATE READ-OUT (the glyph slot just past the elapsed clock) ---------------
//
// One slot, one glyph, four states. It is a READ-OUT and not an action toggle — there is no
// transport button row in this app and a standing owner decision forbids adding one, so nothing
// here is focusable, hit-tested or in `BTN_N`. The glyphs are the design system's transport
// family (`Icon::Rewind` / `Icon::Pause` / `Icon::FastForward`, all in one 14-unit band, with
// `Icon::Play` swapping in for `Pause`), which is what keeps the slot from shifting optical
// weight as the state flips under a running clock.

/// How long [`TransportMark::Play`] stands after a resume — "a couple of seconds", per the owner:
/// the mark answers *did that press land*, and a play glyph held for the whole film would be
/// saying "playing" to someone who is watching a moving picture.
pub const PLAY_MARK_MS: u32 = 2_000;

/// What the state read-out shows this frame. See [`transport_mark`] for the rule.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TransportMark {
    /// playing steadily — the slot is EMPTY, which is the common case and the point of the whole
    /// design: a mark that is always up says nothing
    None,
    /// paused
    Pause,
    /// the playhead is travelling BACKWARDS (scrub, hop or seek burst)
    Rewind,
    /// the playhead is travelling FORWARDS
    FastForward,
    /// resumed within the last [`PLAY_MARK_MS`]
    Play,
    /// the inline [`Spinner`] — the pipeline is being waited on with NO travel to show
    Working,
}

/// PURE: which mark the state read-out wears, given the transport's live state.
///
/// * `paused` — `player::TX.paused`.
/// * `busy` — the frame's single resolve ([`busy_surface`]); only [`Busy::Transport`] reaches here,
///   the other two being the centred read-out's business.
/// * `scrubbing` — `player::TX.scrub_ns >= 0`, i.e. a LIVE preview the user is dragging. It is its
///   own argument because a preview is not busy: the pipeline is still playing the old position
///   happily while the playhead is dragged around, so `busy` alone cannot see it.
/// * `travel_ns` / `pos_ns` — the DISPLAYED playhead against the PUBLISHED one. Their difference is
///   the direction, and it is deliberately derived from position rather than from the keycode that
///   caused it: one expression then covers a LEFT/RIGHT scrub, a chapter or marker hop and a
///   rapid-seek burst, none of which agree about which key (if any) was pressed.
/// * `since_play_ms` — ms since the last paused→playing edge, `None` if this session has never been
///   resumed. See [`note_transport`], which is where that clock is sampled (NOT here, and not in
///   the draw).
///
/// **Precedence: travel, then the pipeline, then paused, then the resume mark.** Travel outranks
/// `paused` because a paused scrub is still a scrub — that is also the order the slot has always
/// had, when its two states were spinner-over-pause.
///
/// **What happened to the spinner** (the slot's only occupant during a seek until now): it stays,
/// narrowed to what it actually means. `Busy::Transport` is *the pipeline is being waited on with a
/// picture already up*, which is *`Seeking`* — the seek the user asked for — but also *`Buffering`*,
/// a re-buffer nobody asked for. Those are two facts and `player_hud`'s own rule at [`Busy`] is that
/// two indicators for one fact read as two facts; the inverse is just as true, so one indicator for
/// two facts reads as one. The direction glyph therefore takes the slot whenever there IS a
/// direction to show (it says everything the spinner said, plus which way), and the spinner keeps
/// every busy frame that has none — a mid-play re-buffer, and the prime→first-frame tail of a seek
/// after the published position has already caught up with the target. Drawing both was never an
/// option: it is one slot, and they would overlap.
pub fn transport_mark(
    paused: bool,
    busy: Busy,
    scrubbing: bool,
    travel_ns: i64,
    pos_ns: i64,
    since_play_ms: Option<u32>,
) -> TransportMark {
    let seeking = busy == Busy::Transport;
    if scrubbing || seeking {
        match travel_ns.cmp(&pos_ns) {
            core::cmp::Ordering::Greater => return TransportMark::FastForward,
            core::cmp::Ordering::Less => return TransportMark::Rewind,
            // no direction to show: the seek's target and the published position agree, which is
            // the tail of a landed seek and the whole of a re-buffer. Fall through to the spinner.
            //
            // NB the comparison is against the PLAYHEAD, so it reports NET travel rather than the
            // instantaneous drag: dragging back from +100s to +50s while still ahead of the
            // playhead keeps reading FastForward. That is deliberate and is the more useful of the
            // two — it answers "which way will I jump when I let go", and the alternative flickers
            // the glyph every time a thumb wobbles on the stick.
            core::cmp::Ordering::Equal => {}
        }
    }
    if seeking {
        return TransportMark::Working;
    }
    if paused {
        return TransportMark::Pause;
    }
    match since_play_ms {
        Some(ms) if ms < PLAY_MARK_MS => TransportMark::Play,
        _ => TransportMark::None,
    }
}


// ---- the failure read-out -------------------------------------------------------------------
//
// **The shared page read-out, not a layout of its own.** A failed playback is a page that failed,
// exactly as Home's, a Library section's and the sign-in read-out are, so it is the same
// component at the same placement: `StatusOverlay::page` hangs the verdict from
// `FULL_ANCHOR_TOP` (540) with its 112 px glyph 44 px above it, the reason in the reserved
// two-line slot, and a row of real controls stacked directly under it. Until 2026-09-28 this was
// a bespoke composition (a 96 px glyph, a primary-ink verdict, three centred prose lines and key
// caps drawn into the sentences, a support line at the same weight as the reason); the owner's
// photograph of it is why it is not any more.
//
// What each block says is decided in ONE place per question:
//  * the controls — `player::failure_actions`, the table that offers an action only if it can
//    change the outcome;
//  * the words — `player::error_now`'s `ErrorShape`, resolved to one reason sentence and at most
//    one quiet note here (`FailureReadout::of`);
//  * the support facts (version · firmware · set · failure code) — one dim footer line at the
//    foot of the screen, always; there is no *Details* control.

/// The subscription's name in the one note that states it ("This server has no Plex Pass"). A
/// product name, so it is not translated.
const PLEX_PASS: &str = "Plex Pass";

/// Everything the failure read-out draws, resolved once from the live failure. The draw, the
/// pointer stops and the key ladder all build it the same way, so the row a click lands on is the
/// row that was drawn.
pub struct FailureReadout {
    pub(super) glyph: plx_ui::icons::Icon,
    pub(super) reason: CString,
    pub(super) note: Option<CString>,
    pub(super) actions: Vec<plx_media::player::FailureAction>,
    pub(super) footer: CString,
}

/// A control's label. Sentence case, as every `StatusOverlay` row is (the sign-in read-out's *Try
/// again*): these are capsule buttons, not the ALL-CAPS clickable TEXT marks.
pub fn failure_action_label(a: plx_media::player::FailureAction) -> &'static std::ffi::CStr {
    use plx_platform::i18n::msg;
    use plx_media::player::FailureAction as A;
    match a {
        A::PlayAutomatically => msg::widgets_failure_play_auto_c(),
        A::TryAgain => msg::browse_action_retry_c(),
        A::ChangeQuality => msg::widgets_failure_change_quality_c(),
        A::Repair => msg::widgets_repair_action_c(),
        A::Back => msg::settings_back_c(),
    }
}

/// The glyph above the verdict, by cause — read off the same `FailureKind` the words come from, so
/// the two cannot disagree (the rule `StatusOverlay::page` states for its callers).
fn failure_glyph(kind: plx_media::player::FailureKind) -> plx_ui::icons::Icon {
    use plx_media::player::FailureKind as K;
    use plx_ui::icons::Icon;
    match kind {
        K::DecisionRefused | K::NoVideoTranscodeTarget => Icon::ServerBadgeXmark,
        K::JailMissingRtkmem => Icon::LockBadgeAlert,
        K::LoadTimeout | K::ReleaseTimeout => Icon::ClockBadgeAlert,
        _ => Icon::Alert,
    }
}

impl FailureReadout {
    /// The read-out for the failure on screen now (main thread).
    pub fn now(ps: &plx_media::route::PlaybackSession) -> Self {
        let mut e = plx_media::player::error_now(ps);
        if e.kind == plx_media::player::FailureKind::JailMissingRtkmem {
            use plx_platform::tv::sandbox::State;
            match ps.repair_status {
                State::Idle => {}
                State::Running => {
                    e.readout = plx_platform::i18n::msg::widgets_repair_running();
                    e.detail = plx_platform::i18n::msg::widgets_repair_wait().into();
                }
                State::Repaired => {
                    e.readout = plx_platform::i18n::msg::widgets_repair_completed();
                    e.detail = plx_platform::i18n::msg::widgets_repair_reopen().into();
                }
                State::Failed(reason) => {
                    e.readout = plx_platform::i18n::msg::widgets_repair_failed();
                    e.detail = repair_failure_message(reason).into();
                }
            }
        }
        let actions = plx_media::player::failure_actions(e.kind, plx_media::player::failure_context(ps));
        Self::of(&e, actions, plx_media::player::support_line(e.kind))
    }

    /// PURE: the read-out for one failure shape.
    ///
    /// **Say it once.** The reason is ONE sentence: what happened and why. Under Force Direct
    /// Play that is the route's own verdict ("Force Direct Play is on, and this video format can't
    /// play without conversion"), which already names both — the old generic "Force Direct Play
    /// could not play this stream" above it said the same thing a second time. The shape's
    /// `detail` then goes to exactly one place, the note under the row (the server's own sentence,
    /// the sandbox's remedy, a repair's progress) — unless it is the Force verdict already said as
    /// the reason. The support facts are always the footer.
    pub fn of(
        e: &plx_media::player::ErrorShape,
        actions: Vec<plx_media::player::FailureAction>,
        support: String,
    ) -> Self {
        use plx_media::player::FailureKind;
        let policy = e.kind == FailureKind::PlaybackPolicy && !e.detail.is_empty();
        let reason: &str = if policy { &e.detail } else { e.readout };
        let note: Option<String> = if e.no_pass {
            Some(plx_platform::i18n::msg::widgets_failure_no_pass(PLEX_PASS))
        } else if policy || e.detail.is_empty() {
            None
        } else {
            Some(e.detail.to_string())
        };
        Self {
            glyph: failure_glyph(e.kind),
            reason: CString::new(reason).unwrap_or_default(),
            note: note.and_then(|n| CString::new(n).ok()),
            actions,
            footer: CString::new(support).unwrap_or_default(),
        }
    }

    /// The row, in draw order.
    pub fn actions(&self) -> &[plx_media::player::FailureAction] {
        &self.actions
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn reason(&self) -> &str {
        self.reason.to_str().unwrap_or("")
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn note(&self) -> Option<&str> {
        self.note.as_deref().and_then(|n| n.to_str().ok())
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn footer(&self) -> &str {
        self.footer.to_str().unwrap_or("")
    }

}

// ---- element addresses for the Engine's hit map / `Focusable` groups (restructure phase 12) --
//
// `PlayerScreen` (`screens/player/mod.rs`) registers one `ui::screen::Stop` per element of its
// `Focusable` groups (`record_stops`, in z-order, at `place`'s rect), keyed on these addresses, instead of the
// old `app/run.rs` ladder calling `icon_hit`/`scrub_hit` on the raw pointer position by hand. The
// GEOMETRY those functions describe is unchanged — only how a caller LEARNS it: a registered `Stop`
// (and its mirror in `PlayerScreen`'s own `Focusable::place`) rather than a bare function the
// frame loop had to know to call. `scrub_hit` is GONE for that reason (phase 12), and `icon_hit`
// went with the hand-written arrangement (the compile's `items` are the geometry now). Kept as plain `u32` constants,
// not an enum, so `PlayerScreen` can address a control-row/tab-row ITEM by `BASE + index` exactly
// as the old `icon_hit`'s `0..BTN_N` scan did.
pub const ELEM_SCRUB: u32 = 0;
/// `+ 0..items()` for whatever occupies the row — the discs, the Skip pill, or both Up Next
/// buttons, each at its own rect (`HudGeometry::items`).
///
/// **A control's element is `ELEM_CONTROL_BASE + ordinal`, in an open-ended range**: the ordinal
/// is the control's place in its layout (classic's five discs are 0..5), and there is no upper
/// bound short of [`ELEM_CHAPTER_BASE`]. The base sits above every other fixed element (the tabs
/// at 20, the failure row at 30, the repair card at 40 000 in `screens::player`), so the range
/// can grow without meeting one of them. `hud.nav.btn` holds the ordinal; [`control_elem`] and
/// [`row_index`] are the two directions of the mapping.
pub const ELEM_CONTROL_BASE: u32 = 0x1_0000;
/// The first element of the chapter strip's own range, at the top of `u32` (the strip's cards
/// are addressed `ELEM_CHAPTER_BASE + index`, a later change of this plan). Reserved here so the
/// open-ended control range ends before it and the two can never collide.
pub const ELEM_CHAPTER_BASE: u32 = 0xFFFF_0000;
/// `+ 0..=1` — Info, then Chapters when the item has any.
pub const ELEM_TAB_BASE: u32 = 20;
/// The element of the control with this ordinal.
pub const fn control_elem(ordinal: i32) -> u32 {
    ELEM_CONTROL_BASE + ordinal as u32
}
/// The control-row item index an element addresses, `None` for any element off the row.
pub fn row_index(elem: u32) -> Option<i32> {
    (ELEM_CONTROL_BASE..ELEM_CHAPTER_BASE).contains(&elem).then(|| (elem - ELEM_CONTROL_BASE) as i32)
}
/// `+ 0..actions().len()` — the failure read-out's row, one element per
/// [`plx_media::player::FailureAction`] in [`FailureReadout::actions`] order: the only controls a
/// FAILED playback draws.
pub const ELEM_FAILURE_BASE: u32 = 30;
/// One past the failure row's last possible element.
pub const ELEM_FAILURE_END: u32 = ELEM_FAILURE_BASE + plx_ui::widgets::STATUS_ROW_MAX as u32;

/// Is the failure read-out's row ON SCREEN — a failure owns the frame. The row always holds at
/// least *Back*, so a failure always has something to press.
pub fn failure_row_drawn(busy: Busy) -> bool {
    readout_owns_frame(busy)
}

/// Product copy is resolved here; the worker's technical error identity stays unchanged.
fn repair_failure_message(reason: plx_platform::tv::sandbox::Failure) -> &'static str {
    use plx_platform::tv::sandbox::Failure;
    match reason {
        Failure::StartFailed => plx_platform::i18n::msg::widgets_repair_start_failed(),
        Failure::HbcUnavailable => plx_platform::i18n::msg::widgets_repair_hbc_unavailable(),
        Failure::NotRoot => plx_platform::i18n::msg::widgets_repair_not_root(),
        Failure::CommandFailed => plx_platform::i18n::msg::widgets_repair_command_failed(),
        Failure::Timeout => plx_platform::i18n::msg::widgets_repair_timeout(),
        Failure::Unreadable => plx_platform::i18n::msg::widgets_repair_unreadable(),
        Failure::Unsupported => plx_platform::i18n::msg::widgets_repair_unsupported(),
    }
}

/// How long a transport lingers after the input that raised it — the player HUD's
/// (`screens::player::input::HUD_LINGER_MS`) and the detail page's full-trailer transport alike,
/// so a trailer's controls leave the screen on the same beat a film's do.
pub const LINGER_MS: u32 = 4500;

// ---- scrub tuning, shared by the player HUD's scrubber and the trailer transport's own --------
//
// Both hold-to-scrub gestures want the same feel — a press jumps `scrub_step_ns()`; holding engages
// a continuous scrub stepping `SCRUB_BASE`→`SCRUB_MAX` via [`scrub_speed`] (playback-seconds per real-second).
// The key-hold half of the state (direction, ramp, key-bounce debounce, lost-keyup net) is ONE type,
// `crate::held_key::HeldKey`, which `screens::player::input::Scrub` and
// `screens::detail::trailer::Transport` both embed; it lives in `appkit` because
// `ci/check-deps.sh`'s `sibling` gate forbids a file under `screens/detail/` from naming
// `crate::screens::player` at all, the same rule that keeps `LINGER_MS` above defined here rather
// than in `screens::player::input` for `trailer.rs` to import from a sibling. What stays per screen
// is the preview position, the trailer's tap debounce and the HUD's reveal/drag flags.
// `screens::player::input::Scrub::clamp_target` is a thin call-through to
// [`scrub_clamp_target`] below so its own many call sites are untouched.
/// What one fresh press hops: the Skip interval preference (10 s unless Settings changed it),
/// read per press so a pick takes effect in a film already playing.
pub fn scrub_step_ns() -> i64 {
    plx_media::route::skip_interval().ns()
}
/// Playback-seconds per real-second at the start of a hold, and the speed it doubles from.
pub const SCRUB_BASE: f32 = 60.0;
/// How long a hold dwells at each speed before doubling — long enough to land on a spot at the
/// current speed, short enough that crossing a film does not take a held thumb.
pub const SCRUB_STEP_MS: u32 = 1000;
/// The fastest a hold gets (reached after three doublings, three seconds of holding; the remote's key repeat only
/// starts about half a second after the press, so the first second is already spent).
pub const SCRUB_MAX: f32 = 480.0;

/// The hold-to-scrub speed after `held_ms` of holding: [`SCRUB_BASE`] doubling every
/// [`SCRUB_STEP_MS`] up to [`SCRUB_MAX`]. Stepped rather than linear so each speed lasts long
/// enough to be read and aimed at, and so the ramp is a ladder the user can feel climb; both
/// hold-to-scrub gestures (the player's and the trailer's) call this one function.
pub fn scrub_speed(held_ms: u32) -> f32 {
    let doublings = (held_ms / SCRUB_STEP_MS).min(16);
    (SCRUB_BASE * (1u32 << doublings) as f32).min(SCRUB_MAX)
}
// tap released → commit after this (further taps accumulate into one Load/reload). The TRAILER
// transport's debounce (`screens::detail::trailer`); the player's D-pad has none — its keys only
// move a target that OK confirms. Immediate tap jumps use SeekBurst; it
// retains requested travel without delaying the commit.
pub const TAP_COMMIT_MS: u32 = 450;
pub const SCRUB_LOST_MS: u32 = 400; // holding but no repeat this long → lost keyup (the trailer commits, the player lets go)
/// How long a key-up of a HELD scrub waits before it counts as the release. The remote's key
/// driver was measured delivering `Up` and a fresh `Down` 0-17 ms apart in the middle of a hold; a
/// thumb cannot release and press again that fast, so a same-direction `Down` inside this window
/// is the same hold. Short enough that a true release lands imperceptibly later.
pub const SCRUB_RELEASE_DEBOUNCE_MS: u32 = 50;
/// How long the lost-keyup net waits after such a bounce: the re-press restarts the remote's
/// ~500 ms auto-repeat delay (measured 499-519 ms), so no repeat can arrive sooner.
pub const SCRUB_REPEAT_RESTART_MS: u32 = 700;

/// Sets the Skip interval for one test and puts the prior value back on drop. The caller holds
/// `testlock::serial()`: the live value is a process global, like every preference.
#[cfg(any(test, feature = "test-support"))]
pub struct SkipIntervalGuard(plx_media::route::SkipInterval);
#[cfg(any(test, feature = "test-support"))]
impl SkipIntervalGuard {
    pub fn set(interval: plx_media::route::SkipInterval) -> Self {
        let prior = plx_media::route::skip_interval();
        plx_media::route::restore_skip_interval(interval);
        Self(prior)
    }
}
#[cfg(any(test, feature = "test-support"))]
impl Drop for SkipIntervalGuard {
    fn drop(&mut self) {
        plx_media::route::restore_skip_interval(self.0);
    }
}

/// Where a scrub target may legally land: never before zero, and never inside the last three
/// seconds, which is a seek past the point the pipeline can prime from.
pub fn scrub_clamp_target(ns: i64, duration_ns: i64) -> i64 {
    let cap = duration_ns - 3_000_000_000;
    let ns = ns.max(0);
    if cap > 0 && ns > cap {
        cap
    } else {
        ns
    }
}

