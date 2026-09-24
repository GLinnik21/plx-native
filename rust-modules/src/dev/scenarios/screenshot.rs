//! The screenshot pipeline's arms — `make screenshots` (`tools/screenshots.py`), whose scene
//! manifest (`tests/screenshots/scenes.json`) names each documentation figure as a target STATE
//! and reaches it through these triggers and the older ones (`heropin`, `grid`, `itemmenu`,
//! `acct`, `detail`, `search`, `library`, `play`…), never through a key walk.
//!
//! Each arm here drives the SAME command path a user's key does (`LibraryCmd::FocusGrid` reseats
//! focus the way the grid's own navigation does; `LibraryCmd::OpenMenu` is OK on the toolbar
//! chip), and each logs one line when its state is REACHED rather than when it was asked for, so
//! the driver can verify from the event log that the picture it captured is the one the manifest
//! named. Every arm gives up after [`CEILING_MS`] with a log line of its own, so a scene whose data
//! never arrives fails loudly instead of capturing the wrong screen.
//!
//! Dev-only, like every arm in [`super`]: the reads go through `dev::read`, which is `None` at
//! compile time without the `devtriggers` feature.

use crate::app::run::Frame;
use crate::app::App;
use crate::screens::registry::{AppArg, LibraryCmd, LibraryMenuKind};

/// How long an arm keeps retrying for its data before it gives up and says so.
const CEILING_MS: u32 = 12_000;
/// How often an arm re-sends its command while waiting — often enough to act on the first frame
/// the data allows, rarely enough that a command already in flight is never doubled.
const RESEND_MS: u32 = 400;

/// Per-arm latches, owned by [`super::Scenarios`].
#[derive(Default)]
pub(crate) struct ScreenshotArms {
    libgrid: SeatArm,
    libshelf: SeatArm,
    libmenu_done: bool,
    libmenu_sent: Option<u32>,
    /// `clockstop`'s position, read once (`None` = not read yet), and whether it was reached.
    clockstop: Option<Option<u32>>,
    #[cfg(feature = "hostsim")]
    clockstop_reached: bool,
}

impl ScreenshotArms {
    /// Is any of these arms still on its way to its state? The settled capture waits while one is
    /// (`app::run`'s `shot::tick`): a screen can come to rest BEFORE an arm acts — a black player
    /// waiting on its seek — and a capture of that rest is the wrong picture. An arm that was never
    /// armed, reached its state or gave up (and logged it) is not pending. Simulator only, as the
    /// settled capture is.
    #[cfg(feature = "hostsim")]
    pub(crate) fn pending(&self) -> bool {
        !self.libgrid.done
            || !self.libshelf.done
            || !self.libmenu_done
            || self.clockstop.is_none()
            || (matches!(self.clockstop, Some(Some(_))) && !self.clockstop_reached)
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
    if let Some(v) = crate::dev::read("stillclock") {
        let ms = v.parse().unwrap_or(0);
        crate::ui::motion::hold_phase_clocks(Some(ms));
        crate::log(&format!("motion: free-running clocks held at {ms} ms by /tmp/plxnative-stillclock"));
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
/// is `ui::idle`'s change clock, which only the simulator keeps; on the television there is none,
/// the wait is skipped, and the arm behaves exactly as it did before it took a value.
pub(crate) fn at_rest(now: u32, rest: Option<u32>) -> bool {
    #[cfg(feature = "hostsim")]
    {
        rest.is_none_or(|ms| now.wrapping_sub(crate::ui::idle::last_change_ms()) >= ms)
    }
    #[cfg(not(feature = "hostsim"))]
    {
        let _ = (now, rest);
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

/// `/tmp/plxnative-libgrid=<row>,<col>` — on the Library page, seat focus on that grid card once
/// the grid has landed. Done when the page REPORTS focus there.
pub(crate) fn libgrid_arm(app: &mut App, fr: &Frame) {
    let on_library = matches!(app.route(), AppArg::Library);
    let arm = &mut app.scenarios.shots.libgrid;
    let Some((row, col)) = arm.pending("libgrid", || crate::dev::read("libgrid"), "focus seated at row {0} col {1}",
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
    let on_library = matches!(app.route(), AppArg::Library);
    let arm = &mut app.scenarios.shots.libshelf;
    let Some((shelf, col)) = arm.pending("libshelf", || crate::dev::read("libshelf"), "focus seated on shelf {0} col {1}",
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
            crate::log(&format!("BADTRIGGER {name} {v:?}: expected <a>,<b>"));
            self.done = true;
            return None;
        };
        let say = |text: &str| text.replace("{0}", &cell.0.to_string()).replace("{1}", &cell.1.to_string());
        if at() == Some(cell) {
            crate::log(&format!("{name}: {}", say(reached)));
            self.done = true;
            return None;
        }
        if fr.now.wrapping_sub(t0) > CEILING_MS {
            crate::log(&format!("{name}: gave up; focus never reached {}", say("{0},{1}")));
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

/// `/tmp/plxnative-libmenu=<sort|filter>[,<rest ms>]` — on the Library page, open that toolbar
/// menu, after `libgrid` (if armed) has seated its focus and, with a rest period, once the page has
/// stopped moving ([`at_rest`]). Done when the menu SURFACE is up.
pub(crate) fn libmenu_arm(app: &mut App, fr: &Frame) {
    if app.scenarios.shots.libmenu_done || !app.scenarios.shots.libgrid.done {
        return;
    }
    let Some(v) = crate::dev::read("libmenu") else {
        app.scenarios.shots.libmenu_done = true;
        return;
    };
    let (name, rest) = parse_menu(&v);
    let kind = match name {
        "sort" => LibraryMenuKind::Sort,
        "filter" => LibraryMenuKind::Filter,
        _other => {
            #[cfg(feature = "devtriggers")]
            crate::log(&format!("BADTRIGGER libmenu {_other:?}: expected sort or filter"));
            app.scenarios.shots.libmenu_done = true;
            return;
        }
    };
    if crate::app::bridge::library_menu_up(&app.pages) {
        crate::log(&format!("libmenu: {name} menu up"));
        app.scenarios.shots.libmenu_done = true;
        return;
    }
    if fr.now.wrapping_sub(app.t0) > CEILING_MS {
        crate::log(&format!("libmenu: gave up; the {name} menu never opened"));
        app.scenarios.shots.libmenu_done = true;
        return;
    }
    if matches!(app.route(), AppArg::Library)
        && at_rest(fr.now, rest)
        && due(app.scenarios.shots.libmenu_sent, fr.now)
    {
        app.scenarios.shots.libmenu_sent = Some(fr.now);
        crate::app::bridge::Bridge::library_command(&mut app.pages, LibraryCmd::OpenMenu(kind));
    }
}

/// `/tmp/plxnative-clockstop=<ms>` — simulator only: stop the clock sink at that movie position
/// and leave the transport PLAYING, so the player holds a moment of playback that pausing would
/// change (the Up Next tile, which exists only while playing). Logs once when the playhead is
/// there. The stop is a stall of the sink's own making (`ffi_host.rs::stop_clock_at`), re-armed
/// every frame because a Load (a seek) starts a fresh fed timeline.
pub(crate) fn clockstop_arm(app: &mut App, fr: &Frame) {
    #[cfg(feature = "hostsim")]
    {
        let arms = &mut app.scenarios.shots;
        if arms.clockstop.is_none() {
            arms.clockstop = Some(crate::dev::read("clockstop").map(|v| match v.parse::<u32>() {
                Ok(ms) => Some(ms),
                Err(_) => {
                    crate::log(&format!("BADTRIGGER clockstop {v:?}: expected <ms>"));
                    None
                }
            }).unwrap_or(None));
        }
        let Some(Some(ms)) = arms.clockstop else { return };
        if !matches!(app.route(), AppArg::Player) {
            return;
        }
        let at = i64::from(ms) * 1_000_000;
        crate::player::stop_sim_clock_at(Some(at));
        let arms = &mut app.scenarios.shots;
        if arms.clockstop_reached {
            return;
        }
        if crate::app::playback::playpos() >= at {
            arms.clockstop_reached = true;
            crate::log(&format!("clockstop: clock held at {ms} ms, still playing"));
        } else if fr.now.wrapping_sub(app.t0) > CLOCKSTOP_CEILING_MS {
            // Reached or not, the arm stops holding the capture back (`pending`).
            arms.clockstop_reached = true;
            crate::log(&format!("clockstop: gave up; the playhead never reached {ms} ms"));
        }
    }
    #[cfg(not(feature = "hostsim"))]
    {
        let _ = fr;
        app.scenarios.shots.clockstop = Some(None);
    }
}

/// How long `clockstop` waits for the playhead. Longer than [`CEILING_MS`]: its position is
/// usually reached through `autoseek`, which only fires 12 s into a run.
#[cfg(feature = "hostsim")]
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
}
