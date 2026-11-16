//! The screenshot pipeline's arms — `make screenshots` (`tools/screenshots.py`), whose scene
//! manifest (`tests/screenshots/scenes.json`) names each documentation figure as a target STATE
//! and reaches it through these triggers and the older ones (`heropin`, `grid`, `itemmenu`,
//! `acct`, `detail`, `search`, `library`, `play`…), never through a key walk.
//!
//! Each arm here drives the SAME command path a user's key does (`LibraryCmd::FocusGrid` reseats
//! focus the way the grid's own navigation does; `LibraryCmd::OpenMenu` is OK on the header's
//! Sort & Filter control), and each logs one line when its state is REACHED rather than when it was asked for, so
//! the driver can verify from the event log that the picture it captured is the one the manifest
//! named. `libtype` chooses the listing the others then act on (the Show-menu type, or the
//! Collections / Recommended page the header pushes). Every arm gives up with a log line of its
//! own, so a scene whose data never arrives fails loudly instead of capturing the wrong screen: after
//! [`CEILING_MS`], except the two that wait on playback, `clockstop` (`CLOCKSTOP_CEILING_MS`) and
//! `autopause` (`AUTOPAUSE_CEILING_MS`), which wait up to a minute.
//!
//! Dev-only, like every arm in [`super`]: the reads go through `devtrig::read`, which is `None` at
//! compile time without the `devtriggers` feature.

use crate::app::run::Frame;
use crate::app::App;
use plx_data::browse::LibraryType;
use plx_screens::library::Listing;
use plx_screens::registry::{AppArg, LibraryCmd, LibraryMenuKind};

/// How long an arm keeps retrying for its data before it gives up and says so.
const CEILING_MS: u32 = 12_000;
/// How often an arm re-sends its command while waiting — often enough to act on the first frame
/// the data allows, rarely enough that a command already in flight is never doubled.
const RESEND_MS: u32 = 400;

/// Per-arm latches, owned by [`super::Scenarios`].
#[derive(Default)]
pub(crate) struct ScreenshotArms {
    libtype_done: bool,
    libtype_sent: Option<u32>,
    libgrid: SeatArm,
    libshelf: SeatArm,
    libmenu_done: bool,
    libmenu_sent: Option<u32>,
    /// `clockstop`'s position, read once (`None` = not read yet), and whether it was reached.
    clockstop: Option<Option<u32>>,
    #[cfg(feature = "hostsim")]
    clockstop_reached: bool,
    /// What `autopause` still owes the settled capture — see [`PauseDebt`].
    #[cfg(feature = "hostsim")]
    autopause: PauseDebt,
}

impl ScreenshotArms {
    /// Is any of these arms still on its way to its state? The settled capture waits while one is
    /// (`app::run`'s `shot::tick`): a screen can come to rest BEFORE an arm acts — a black player
    /// waiting on its seek — and a capture of that rest is the wrong picture. An arm that was never
    /// armed, reached its state or gave up (and logged it) is not pending. Simulator only, as the
    /// settled capture is.
    #[cfg(feature = "hostsim")]
    pub(crate) fn pending(&self) -> bool {
        !self.libtype_done
            || !self.libgrid.done
            || !self.libshelf.done
            || !self.libmenu_done
            || self.clockstop.is_none()
            || (matches!(self.clockstop, Some(Some(_))) && !self.clockstop_reached)
            || self.autopause != PauseDebt::Paid
    }
}

/// **What `autopause` still owes the settled capture: its Pause.**
///
/// The pause is the state a pause scene photographs (a `hold=` pause is photographed paused, so its
/// Resume is not owed), and it cannot land early: the arm only arms itself six seconds into the
/// run (`autopause_arm`), then waits for the playhead (`at=`). The capture's quiet is WALL clock
/// (`shot::tick`: nothing changed for `PLXNATIVE_SHOT_SETTLE` ms), and a host that is slow to start
/// playback satisfies it before the Pause: with the host busy the debug simulator runs a frame a
/// second (`fps=1`) and a Play start stalls the main thread for seconds (`main-thread hang`), so
/// the first tick after the stall finds the screen unchanged for longer than the quiet and
/// captures the player still loading — `shot: settled (7866 ms at rest, 7866 ms after the first
/// frame)`, no picture, no pause. So the capture waits on the debt, as it waits on a seat or a
/// clock stop ([`ScreenshotArms::pending`]).
///
/// `Unread` is the trigger not read yet, which counts as owed, as an unread `clockstop` does: until
/// the first frame has read it, nothing can say there is nothing to wait for.
#[cfg(feature = "hostsim")]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum PauseDebt {
    #[default]
    Unread,
    Owed,
    Paid,
}

#[cfg(feature = "hostsim")]
impl PauseDebt {
    /// The debt after one frame. `armed` reads whether a trigger asks for a pause at all (called
    /// once, while `Unread`); `edges_left` says the Pause has not landed (the arm is not armed yet,
    /// or armed and waiting on the playhead); `expired` is the ceiling
    /// ([`AUTOPAUSE_CEILING_MS`]), after which the arm stops holding the capture back so a scene
    /// whose playhead never arrives fails on its `expect` lines, with the arm's own give-up line
    /// in the log, instead of on the driver's timeout.
    fn step(self, armed: impl FnOnce() -> bool, edges_left: bool, expired: bool) -> Self {
        match self {
            Self::Unread if armed() => Self::Owed,
            Self::Unread => Self::Paid,
            Self::Owed if !edges_left || expired => Self::Paid,
            held => held,
        }
    }
}

/// How long `autopause` waits for its Pause, counted from the app's start. Far longer than
/// [`CEILING_MS`], which is for a screen that is already there: the playhead this waits on is
/// reached through a Load, an open and playback in real time from the resume point.
#[cfg(feature = "hostsim")]
const AUTOPAUSE_CEILING_MS: u32 = 60_000;

/// Has `autopause`'s Pause not landed yet? True while the arm has not armed itself (`pause_tried`
/// is its once-latch, set six seconds into the run on the Player route, whether or not a trigger
/// asked for a pause) and while it is armed and waiting on its playhead (`script_armed`, the
/// scheduled pause still in hand). Without the first clause the frames before the arm arms itself
/// would read as "nothing left to wait for", which is the flake this debt exists to close.
#[cfg(feature = "hostsim")]
fn pause_edges_left(pause_tried: bool, script_armed: bool) -> bool {
    !pause_tried || script_armed
}

/// Keep [`ScreenshotArms`]' `autopause` debt current. Called every frame by `each_frame`, ahead of
/// the arms that act on the player. Simulator only, as the settled capture is.
#[cfg(feature = "hostsim")]
pub(crate) fn autopause_debt(app: &mut App, fr: &Frame) {
    let sc = &mut app.scenarios;
    let edges_left = pause_edges_left(sc.pause_tried, sc.pause_script.is_some());
    let expired = fr.now.wrapping_sub(app.t0) > AUTOPAUSE_CEILING_MS;
    let before = sc.shots.autopause;
    sc.shots.autopause = before.step(|| crate::dev::pause_script().is_some(), edges_left, expired);
    if before == PauseDebt::Owed && sc.shots.autopause == PauseDebt::Paid && edges_left {
        plx_base::eventlog::log("autopause: gave up; the pause never landed");
    }
}

/// `/tmp/plxnative-stillclock=<ms>` — hold every free-running clock animator (`motion::Phase`:
/// spinners, stall timers) at `<ms>` elapsed. A waiting screen then draws one fixed picture and
/// comes to rest, which is what a settled capture needs; springs and ramps are untouched, they
/// settle on their own.
/// The hold itself only exists with `devtriggers` (`motion::hold_phase_clocks`), hence the gate on
/// the body rather than a second, empty twin of this function.
pub(crate) fn arm_stillclock() {
    #[cfg(feature = "devtriggers")]
    if let Some(v) = plx_base::devtrig::read("stillclock") {
        let ms = v.parse().unwrap_or(0);
        plx_machine::motion::hold_phase_clocks(Some(ms));
        plx_base::eventlog::log(&format!("motion: free-running clocks held at {ms} ms by /tmp/plxnative-stillclock"));
    }
}

/// `"<row>,<col>"` → `(row, col)`.
pub(super) fn parse_cell(v: &str) -> Option<(usize, usize)> {
    let (r, c) = v.split_once(',')?;
    Some((r.trim().parse().ok()?, c.trim().parse().ok()?))
}

/// Has the screen been at rest for `rest` ms? `None` asks for no wait at all.
///
/// An overlay presented over a page FREEZES that page (the popover host draws it from a snapshot),
/// so an arm that opens a menu while the page under it is still scrolling or still waiting for its
/// art photographs a half-landed page forever. The arms that open a surface over a page therefore
/// accept a rest period, and hold the surface back until the page has stopped changing. The signal
/// is `plx_machine::idle`'s change clock, which only the simulator keeps; on the television there is none,
/// the wait is skipped, and the arm behaves exactly as it did before it took a value.
pub(crate) fn at_rest(now: u32, rest: Option<u32>, last_change_ms: u32, current_changed: bool) -> bool {
    #[cfg(feature = "hostsim")]
    {
        rest.is_none_or(|ms| !current_changed && now.wrapping_sub(last_change_ms) >= ms)
    }
    #[cfg(not(feature = "hostsim"))]
    {
        let _ = (now, rest, last_change_ms, current_changed);
        true
    }
}

/// `"<kind>[,<rest ms>]"` → `(kind, rest)`. A malformed rest reads as no rest, not as no trigger.
fn parse_menu(v: &str) -> (&str, Option<u32>) {
    match v.split_once(',') {
        Some((kind, rest)) => (kind.trim(), rest.trim().parse().ok()),
        None => (v.trim(), None),
    }
}

/// Whether a resend is due: never sent, or sent at least [`RESEND_MS`] ago.
fn due(sent: Option<u32>, now: u32) -> bool {
    sent.is_none_or(|at| now.wrapping_sub(at) >= RESEND_MS)
}

/// What `plxnative-libtype` asks for: a Show-menu type of the page's own grid, or one of the two
/// pages the header's controls push over it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum LibTypeArm {
    Type(LibraryType),
    Page(Listing),
}

/// `"<name>"` → what it names.
fn parse_type(v: &str) -> Option<LibTypeArm> {
    Some(match v.trim() {
        "primary" | "movies" | "shows" => LibTypeArm::Type(LibraryType::Primary),
        "seasons" => LibTypeArm::Type(LibraryType::Seasons),
        "episodes" => LibTypeArm::Type(LibraryType::Episodes),
        "collections" => LibTypeArm::Page(Listing::Collections),
        "recommended" => LibTypeArm::Page(Listing::Recommended),
        _ => return None,
    })
}

/// The Library family of routes: the full grid and the two pages its header pushes over it.
fn on_library_page(app: &App) -> bool {
    app.route().is_library_family()
}

/// `/tmp/plxnative-libtype=<movies|shows|seasons|episodes|collections|recommended>` — on the
/// Library page, choose that Show-menu type the way its row does, or (collections, recommended)
/// press the header control that opens that page (the `library-collections` and
/// `library-recommended` scenarios), once the section's listing is there. Done when the page's
/// committed listing REPORTS that type, or the route is that page; `libgrid` and `libmenu` wait for
/// it, so a seat or a menu lands on the listing this trigger asked for.
pub(crate) fn libtype_arm(app: &mut App, fr: &Frame) {
    if app.scenarios.shots.libtype_done {
        return;
    }
    let Some(v) = plx_base::devtrig::read("libtype") else {
        app.scenarios.shots.libtype_done = true;
        return;
    };
    let Some(kind) = parse_type(&v) else {
        #[cfg(feature = "devtriggers")]
        plx_base::eventlog::log(&format!("BADTRIGGER libtype {v:?}: expected movies, shows, seasons, episodes, collections or recommended"));
        app.scenarios.shots.libtype_done = true;
        return;
    };
    let reached = match kind {
        LibTypeArm::Type(kind) => crate::app::bridge::Bridge::library_listed(&app.pages) == Some(kind),
        LibTypeArm::Page(Listing::Collections) => matches!(app.route(), AppArg::LibraryCollections { .. }),
        LibTypeArm::Page(_) => matches!(app.route(), AppArg::LibraryRecommended { .. }),
    };
    if reached {
        plx_base::eventlog::log(&format!("libtype: listing {}", v.trim()));
        app.scenarios.shots.libtype_done = true;
        return;
    }
    if fr.now.wrapping_sub(app.t0) > CEILING_MS {
        plx_base::eventlog::log(&format!("libtype: gave up; the listing never became {}", v.trim()));
        app.scenarios.shots.libtype_done = true;
        return;
    }
    if matches!(app.route(), AppArg::Library) && due(app.scenarios.shots.libtype_sent, fr.now) {
        app.scenarios.shots.libtype_sent = Some(fr.now);
        let command = match kind {
            LibTypeArm::Type(kind) => LibraryCmd::SetType(kind),
            LibTypeArm::Page(listing) => LibraryCmd::OpenListing(listing),
        };
        crate::app::bridge::Bridge::library_command(&mut app.pages, command);
    }
}

/// `/tmp/plxnative-libgrid=<row>,<col>` — on the Library page, seat focus on that grid card once
/// the grid has landed. Done when the page REPORTS focus there.
pub(crate) fn libgrid_arm(app: &mut App, fr: &Frame) {
    if !app.scenarios.shots.libtype_done {
        return;
    }
    let on_library = on_library_page(app);
    let arm = &mut app.scenarios.shots.libgrid;
    let Some((row, col)) = arm.pending("libgrid", || plx_base::devtrig::read("libgrid"), "focus seated at row {0} col {1}",
        fr, app.t0, || crate::app::bridge::Bridge::library_grid_position(&app.pages)) else { return };
    if on_library && arm.resend(fr.now) {
        crate::app::bridge::Bridge::library_command(&mut app.pages, LibraryCmd::FocusGrid { row, col });
    }
}

/// `/tmp/plxnative-libshelf=<shelf>,<col>` — on the Library page, seat focus on card `<col>` of
/// hub shelf `<shelf>` (0 is Continue Watching when there is one) once the shelves have landed.
/// Focus on a lower shelf scrolls the ones above it up under the tab bar. Done when the page
/// REPORTS focus there.
pub(crate) fn libshelf_arm(app: &mut App, fr: &Frame) {
    if !app.scenarios.shots.libtype_done {
        return;
    }
    let on_library = on_library_page(app);
    let arm = &mut app.scenarios.shots.libshelf;
    let Some((shelf, col)) = arm.pending("libshelf", || plx_base::devtrig::read("libshelf"), "focus seated on shelf {0} col {1}",
        fr, app.t0, || crate::app::bridge::Bridge::library_shelf_position(&app.pages)) else { return };
    if on_library && arm.resend(fr.now) {
        crate::app::bridge::Bridge::library_command(&mut app.pages, LibraryCmd::FocusShelf { shelf, col });
    }
}

/// One focus-seating arm's latch: reads its `<a>,<b>` trigger, reports when the page's focus is
/// there, gives up after [`CEILING_MS`], and paces the re-sends.
#[derive(Default)]
pub(crate) struct SeatArm {
    done: bool,
    sent: Option<u32>,
}

impl SeatArm {
    /// The cell still to be seated, or `None` once the arm is done (trigger absent, malformed,
    /// reached or given up — each logged). `read` reads the trigger `name`; `reached` is the log
    /// text after `<name>: `, with `{0}` and `{1}` for the cell; `at` says where the page's focus
    /// is now. Neither is called once the arm is done.
    fn pending(&mut self, name: &str, read: impl FnOnce() -> Option<String>, reached: &str, fr: &Frame, t0: u32,
        at: impl FnOnce() -> Option<(usize, usize)>) -> Option<(usize, usize)> {
        if self.done {
            return None;
        }
        let Some(v) = read() else {
            self.done = true;
            return None;
        };
        let Some(cell) = parse_cell(&v) else {
            #[cfg(feature = "devtriggers")]
            plx_base::eventlog::log(&format!("BADTRIGGER {name} {v:?}: expected <a>,<b>"));
            self.done = true;
            return None;
        };
        let say = |text: &str| text.replace("{0}", &cell.0.to_string()).replace("{1}", &cell.1.to_string());
        if at() == Some(cell) {
            plx_base::eventlog::log(&format!("{name}: {}", say(reached)));
            self.done = true;
            return None;
        }
        if fr.now.wrapping_sub(t0) > CEILING_MS {
            plx_base::eventlog::log(&format!("{name}: gave up; focus never reached {}", say("{0},{1}")));
            self.done = true;
            return None;
        }
        Some(cell)
    }

    /// Whether to send the command this frame; records the send when it is.
    fn resend(&mut self, now: u32) -> bool {
        let go = due(self.sent, now);
        if go {
            self.sent = Some(now);
        }
        go
    }
}

/// `/tmp/plxnative-libmenu=<sortfilter|sort|filter|type>[,<rest ms>]` — on the Library page, open that toolbar
/// menu, after `libgrid` (if armed) has seated its focus and, with a rest period, once the page has
/// stopped moving ([`at_rest`]). Done when the menu SURFACE is up.
pub(crate) fn libmenu_arm(app: &mut App, fr: &Frame) {
    if app.scenarios.shots.libmenu_done || !app.scenarios.shots.libgrid.done {
        return;
    }
    let Some(v) = plx_base::devtrig::read("libmenu") else {
        app.scenarios.shots.libmenu_done = true;
        return;
    };
    let (name, rest) = parse_menu(&v);
    let kind = match name {
        "sort" => LibraryMenuKind::Sort,
        "filter" => LibraryMenuKind::Filter,
        "type" => LibraryMenuKind::Type,
        "sortfilter" | "sort-filter" => LibraryMenuKind::SortFilter,
        _other => {
            #[cfg(feature = "devtriggers")]
            plx_base::eventlog::log(&format!("BADTRIGGER libmenu {_other:?}: expected sort, filter, type or sortfilter"));
            app.scenarios.shots.libmenu_done = true;
            return;
        }
    };
    if crate::app::bridge::library_menu_up(&app.pages) {
        plx_base::eventlog::log(&format!("libmenu: {name} menu up"));
        app.scenarios.shots.libmenu_done = true;
        return;
    }
    if fr.now.wrapping_sub(app.t0) > CEILING_MS {
        plx_base::eventlog::log(&format!("libmenu: gave up; the {name} menu never opened"));
        app.scenarios.shots.libmenu_done = true;
        return;
    }
    if on_library_page(app)
        && at_rest(fr.now, rest, app.pages.present.last_change_ms(),
            app.pages.present.changed() || plx_machine::motion::report::present_moving())
        && due(app.scenarios.shots.libmenu_sent, fr.now)
    {
        app.scenarios.shots.libmenu_sent = Some(fr.now);
        crate::app::bridge::Bridge::library_command(&mut app.pages, LibraryCmd::OpenMenu(kind));
    }
}

/// `/tmp/plxnative-clockstop=<ms>` — simulator only: stop the clock sink at that movie position
/// and leave the transport PLAYING, so the player holds a moment of playback that pausing would
/// change (the Up Next tile, which exists only while playing). Logs once when the playhead is
/// there. The stop is a stall of the sink's own making (`HostSink::stop_clock_at`), re-armed
/// every frame because a Load (a seek) starts a fresh fed timeline.
pub(crate) fn clockstop_arm(app: &mut App, fr: &Frame) {
    #[cfg(all(feature = "hostsim", feature = "play-feed"))]
    {
        let arms = &mut app.scenarios.shots;
        if arms.clockstop.is_none() {
            arms.clockstop = Some(plx_base::devtrig::read("clockstop").map(|v| match v.parse::<u32>() {
                Ok(ms) => Some(ms),
                Err(_) => {
                    plx_base::eventlog::log(&format!("BADTRIGGER clockstop {v:?}: expected <ms>"));
                    None
                }
            }).unwrap_or(None));
        }
        let Some(Some(ms)) = arms.clockstop else { return };
        if !matches!(app.route(), AppArg::Player) {
            return;
        }
        let at = i64::from(ms) * 1_000_000;
        crate::app::adapters::hostsim::stop_sim_clock_at(Some(at));
        let arms = &mut app.scenarios.shots;
        if arms.clockstop_reached {
            return;
        }
        if crate::app::playback::playpos(&app.player.session).is_some_and(|pos| pos >= at) {
            arms.clockstop_reached = true;
            plx_base::eventlog::log(&format!("clockstop: clock held at {ms} ms, still playing"));
        } else if fr.now.wrapping_sub(app.t0) > CLOCKSTOP_CEILING_MS {
            // Reached or not, the arm stops holding the capture back (`pending`).
            arms.clockstop_reached = true;
            plx_base::eventlog::log(&format!("clockstop: gave up; the playhead never reached {ms} ms"));
        }
    }
    #[cfg(not(all(feature = "hostsim", feature = "play-feed")))]
    {
        let _ = fr;
        if app.scenarios.shots.clockstop.is_none() {
            #[cfg(all(feature = "hostsim", not(feature = "play-feed")))]
            if plx_base::devtrig::read("clockstop").is_some() {
                plx_base::eventlog::log("clockstop: unavailable without Feed simulator clock");
            }
            app.scenarios.shots.clockstop = Some(None);
        }
    }
}

/// How long `clockstop` waits for the playhead. Longer than [`CEILING_MS`]: its position is
/// usually reached through `autoseek`, which only fires 12 s into a run.
#[cfg(all(feature = "hostsim", feature = "play-feed"))]
const CLOCKSTOP_CEILING_MS: u32 = 60_000;

#[cfg(test)]
mod tests {
    use super::{due, parse_cell, parse_menu, RESEND_MS};

    #[test]
    fn a_menu_trigger_is_a_kind_and_an_optional_rest() {
        assert_eq!(parse_menu("sort"), ("sort", None));
        assert_eq!(parse_menu("filter,800"), ("filter", Some(800)));
        assert_eq!(parse_menu(" sort , 1200 "), ("sort", Some(1200)));
        assert_eq!(parse_menu("sort,soon"), ("sort", None));
    }

    #[test]
    fn a_cell_is_row_comma_col() {
        assert_eq!(parse_cell("1,0"), Some((1, 0)));
        assert_eq!(parse_cell(" 2 , 5 "), Some((2, 5)));
        assert_eq!(parse_cell("2"), None);
        assert_eq!(parse_cell("a,b"), None);
    }

    #[test]
    fn a_command_is_resent_only_after_the_resend_gap() {
        assert!(due(None, 0));
        assert!(!due(Some(1_000), 1_000 + RESEND_MS - 1));
        assert!(due(Some(1_000), 1_000 + RESEND_MS));
        assert!(due(Some(u32::MAX - 10), RESEND_MS), "across the tick wrap");
    }

    /// The arms of a scene that armed nothing but `autopause`: everything else has landed or was
    /// never asked for, so `pending` is exactly the debt under test.
    #[cfg(feature = "hostsim")]
    fn only_autopause_unlanded() -> super::ScreenshotArms {
        let landed = || super::SeatArm { done: true, ..Default::default() };
        super::ScreenshotArms {
            libtype_done: true,
            libgrid: landed(),
            libshelf: landed(),
            libmenu_done: true,
            clockstop: Some(None),
            ..Default::default()
        }
    }

    /// The `player` scene's flake. Its settled capture fired on a quiet player that was still
    /// loading (the Pause could not even arm: it opens six seconds in), so the figure had no
    /// picture and no pause. A trigger that is armed holds the capture through every frame before
    /// its Pause lands, including the frames before the arm has armed itself, and releases it when
    /// the Pause has landed.
    #[cfg(feature = "hostsim")]
    #[test]
    fn an_armed_autopause_holds_the_settled_capture_until_its_pause_lands() {
        use super::PauseDebt;
        let mut arms = only_autopause_unlanded();
        assert!(arms.pending(), "unread: a frame that has not reached the arm owes nothing known");

        arms.autopause = arms.autopause.step(|| true, true, false);
        assert_eq!(arms.autopause, PauseDebt::Owed);
        assert!(arms.pending(), "armed, and the arm has not armed itself yet");

        for _ in 0..3 {
            arms.autopause = arms.autopause.step(|| panic!("the trigger is read once"), true, false);
            assert!(arms.pending(), "a loading player at rest is not the state the scene names");
        }

        arms.autopause = arms.autopause.step(|| panic!("the trigger is read once"), false, false);
        assert_eq!(arms.autopause, PauseDebt::Paid);
        assert!(!arms.pending(), "the Pause landed: the capture may take the rest it is waiting for");
    }

    /// The line that holds the frames BEFORE the arm arms itself: the arm's latch is not set and no
    /// pause is scheduled yet, which is the state of the first six seconds of every run. Reading
    /// that as "nothing left" paid the debt on the first frame and brought the flake back.
    #[cfg(feature = "hostsim")]
    #[test]
    fn autopause_stays_owed_on_the_frames_before_its_arm_arms_itself() {
        use super::{pause_edges_left, PauseDebt};
        assert!(pause_edges_left(false, false), "not armed yet: the Pause is still to come");
        assert!(pause_edges_left(true, true), "armed, waiting on its playhead");
        assert!(!pause_edges_left(true, false), "armed and the Pause landed (or no trigger asked)");

        let mut arms = only_autopause_unlanded();
        arms.autopause = arms.autopause.step(|| true, pause_edges_left(false, false), false);
        arms.autopause = arms.autopause.step(|| true, pause_edges_left(false, false), false);
        assert_eq!(arms.autopause, PauseDebt::Owed);
        assert!(arms.pending(), "a quiet player before the arm armed itself is not the scene's state");
    }

    #[cfg(feature = "hostsim")]
    #[test]
    fn a_scene_without_autopause_is_not_held_and_a_pause_that_never_lands_gives_up() {
        use super::PauseDebt;
        let mut arms = only_autopause_unlanded();
        arms.autopause = arms.autopause.step(|| false, true, false);
        assert_eq!(arms.autopause, PauseDebt::Paid, "no trigger, nothing owed");
        assert!(!arms.pending());

        let mut arms = only_autopause_unlanded();
        arms.autopause = arms.autopause.step(|| true, true, false);
        arms.autopause = arms.autopause.step(|| true, true, true);
        assert_eq!(arms.autopause, PauseDebt::Paid, "past the ceiling the arm stops holding the capture");
        assert!(!arms.pending());
    }
}
