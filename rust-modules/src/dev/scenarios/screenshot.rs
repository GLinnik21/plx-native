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
    libgrid_done: bool,
    libgrid_sent: Option<u32>,
    libmenu_done: bool,
    libmenu_sent: Option<u32>,
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
fn parse_cell(v: &str) -> Option<(usize, usize)> {
    let (r, c) = v.split_once(',')?;
    Some((r.trim().parse().ok()?, c.trim().parse().ok()?))
}

/// Whether a resend is due: never sent, or sent at least [`RESEND_MS`] ago.
fn due(sent: Option<u32>, now: u32) -> bool {
    sent.is_none_or(|at| now.wrapping_sub(at) >= RESEND_MS)
}

/// `/tmp/plxnative-libgrid=<row>,<col>` — on the Library page, seat focus on that grid card once
/// the grid has landed. Done when the page REPORTS focus there.
pub(crate) fn libgrid_arm(app: &mut App, fr: &Frame) {
    if app.scenarios.shots.libgrid_done {
        return;
    }
    let Some(v) = crate::dev::read("libgrid") else {
        app.scenarios.shots.libgrid_done = true;
        return;
    };
    let Some((row, col)) = parse_cell(&v) else {
        crate::log(&format!("BADTRIGGER libgrid {v:?}: expected <row>,<col>"));
        app.scenarios.shots.libgrid_done = true;
        return;
    };
    if crate::app::bridge::Bridge::library_grid_position(&app.pages) == Some((row, col)) {
        crate::log(&format!("libgrid: focus seated at row {row} col {col}"));
        app.scenarios.shots.libgrid_done = true;
        return;
    }
    if fr.now.wrapping_sub(app.t0) > CEILING_MS {
        crate::log(&format!("libgrid: gave up; focus never reached row {row} col {col}"));
        app.scenarios.shots.libgrid_done = true;
        return;
    }
    if matches!(app.route(), AppArg::Library) && due(app.scenarios.shots.libgrid_sent, fr.now) {
        app.scenarios.shots.libgrid_sent = Some(fr.now);
        crate::app::bridge::Bridge::library_command(&mut app.pages, LibraryCmd::FocusGrid { row, col });
    }
}

/// `/tmp/plxnative-libmenu=<sort|filter>` — on the Library page, open that toolbar menu, after
/// `libgrid` (if armed) has seated its focus. Done when the menu SURFACE is up.
pub(crate) fn libmenu_arm(app: &mut App, fr: &Frame) {
    if app.scenarios.shots.libmenu_done || !app.scenarios.shots.libgrid_done {
        return;
    }
    let Some(v) = crate::dev::read("libmenu") else {
        app.scenarios.shots.libmenu_done = true;
        return;
    };
    let kind = match v.as_str() {
        "sort" => LibraryMenuKind::Sort,
        "filter" => LibraryMenuKind::Filter,
        other => {
            crate::log(&format!("BADTRIGGER libmenu {other:?}: expected sort or filter"));
            app.scenarios.shots.libmenu_done = true;
            return;
        }
    };
    if crate::app::bridge::library_menu_up(&app.pages) {
        crate::log(&format!("libmenu: {v} menu up"));
        app.scenarios.shots.libmenu_done = true;
        return;
    }
    if fr.now.wrapping_sub(app.t0) > CEILING_MS {
        crate::log(&format!("libmenu: gave up; the {v} menu never opened"));
        app.scenarios.shots.libmenu_done = true;
        return;
    }
    if matches!(app.route(), AppArg::Library) && due(app.scenarios.shots.libmenu_sent, fr.now) {
        app.scenarios.shots.libmenu_sent = Some(fr.now);
        crate::app::bridge::Bridge::library_command(&mut app.pages, LibraryCmd::OpenMenu(kind));
    }
}

#[cfg(test)]
mod tests {
    use super::{due, parse_cell, RESEND_MS};

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
