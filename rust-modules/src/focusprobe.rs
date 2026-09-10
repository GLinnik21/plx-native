//! **The focus fingerprint** — one line naming everything `app.rs`'s key ladder can move, written
//! to the event log only on the frames where it CHANGES.
//!
//! # Why this exists
//!
//! The once/sec heartbeat (`app.rs`, the `loop=` line) publishes `route=`, `overlay=` and `pos=`.
//! Most of the key ladder's arms move focus *within* a screen without touching any of those three:
//! UP on the home grid, LEFT along the detail hero's action row, DOWN through an open popover. A
//! (route × key) characterization harness built on the heartbeat alone would therefore pin the
//! handful of arms that change route and be blind to the rest.
//!
//! This module is the missing instrument. It samples the read-only accessors each screen already
//! exposes, formats them into one ordered line, and logs that line when — and only when — it
//! differs from the last one. What a key press did is then the diff between the fingerprint before
//! it and the fingerprint after.
//!
//! # Rules this module keeps
//!
//! **Read-only.** Every field comes from a getter the screen already exposes for the app's own
//! use. This module adds no accessor to any screen and edits none, and it drains no one-shot
//! mailbox — `detail::take_open_request`, `detail::take_alt_open` and `person::take_request` are
//! deliberately untouched, because reading one would consume a navigation.
//!
//! **It must not touch [`crate::ui::idle`].** It never calls `invalidate()`. Reporting to the frame
//! gate would hold every screen presenting forever, which would destroy the very idle behaviour a
//! harness built on this is meant to be able to observe.
//!
//! **Redaction.** The event log is pasted into public issue threads (`app/diagnostics.rs`'s module doc has
//! the reasoning, and `dev.rs`'s `DevServer::describe` has the incident). So the line carries
//! indices, enum tags, booleans, and — as the only identity — rating keys and the registry SLOT
//! number of a server. No title, no path, no URL, no server name, address or `machineIdentifier`,
//! no token. The enforcement is structural rather than a matter of care: every value written here
//! is either a number, a bool, a `&'static str` tag chosen in this file, or a rating key that goes
//! through [`push_rk`], which copies ASCII alphanumerics and nothing else.
//!
//! **Diffable.** Fixed field order, one line, no timestamps, no addresses, and no float is ever
//! printed — the two spring positions that decide a branch (Home's grid-dive snap, and the target beside
//! it) are reduced to the booleans the code itself compares them as. A field's PRESENCE is a
//! function of [`Screen`] alone, so two lines for the same screen always carry the same keys.
//!
//! # Cost
//!
//! [`armed`] resolves its trigger once and answers from a `OnceLock` afterwards, so an unarmed
//! build or an unarmed boot pays one atomic load and a branch per frame. Armed, a frame pays the
//! accessor reads for the CURRENT screen only — the `match` on [`Screen`] never reaches another
//! screen's getters — plus one `String` build of 40–160 bytes.
//!
//! Measured on the dev Mac at `opt-level = 2` (2026-08-15, 50k builds per screen, host accessors
//! over empty stores): **39–223 ns** per frame on every screen except the player, where the line
//! costs **~7.3 µs**. That one number is worth knowing before adding a field: **~6.4 µs of it is a
//! single filesystem `stat`**, inside [`crate::ui::player_hud::transport_hidden`] →
//! `player_hud::busy` → `dev::flag("failtest")`. The rest of the player's seventeen fields together
//! cost ~0.9 µs. It is paid deliberately: `transport_hidden` is the exact predicate the key
//! ladder's player arms test, and the alternative is a second derivation of a rule that module
//! keeps in one place. The player route is also the one route the idle gate excludes, so it draws
//! at full rate and 7 µs is ~0.04% of a 16.7 ms frame.
//!
//! # Arming it
//!
//! `touch /tmp/plxnative-focus` on the television (or in `PLXNATIVE_RUNTIME_DIR` under the
//! simulator), then read `focus ` lines out of `/tmp/plxnative-events.log`. The trigger is listed
//! in [`crate::dev`]'s `DIAG` set, so arming the observer does not also change which screen the app
//! boots to — the same reasoning `plxnative-noidle` carries, and it matters more here: a harness
//! that wants to characterize the who's-watching picker must not lose the picker by watching it.
//! `RELEASE=1` drops the `devtriggers` feature, `dev::flag` becomes `false`, and this whole surface
//! goes quiet.

use std::ffi::c_int;
use std::fmt::Write as _;
use std::sync::Mutex;

use crate::ui::player_hud::ControlSlot;

/// Which screen the frame ended on, as the probe dispatches on it.
///
/// `app.rs` maps its private `Route` onto this with an EXHAUSTIVE `match`, so a new route is a
/// compile error here rather than a screen that silently fingerprints as nothing. The route WORD
/// printed on the line is not taken from this type — it is `app.rs`'s own `rn`, handed to
/// [`sample`] separately, so the fingerprint's `route=` and the heartbeat's `route=` are the same
/// string built by the same expression and a harness can join the two lines on it.
#[derive(Clone, Copy)]
pub(crate) enum Screen {
    /// The QR sign-in (`screens::login::LoginScreen`), an OWNED screen since phase 6.
    ///
    /// **This screen is not cursor-free, and this variant said so for a while after it stopped
    /// being true.** Most of `LoginScreen` really has "no focus state of its own — the screen is
    /// a projection of the auth phase" (see `push_fields`'s comment on `phase`), but it grows
    /// exactly ONE focusable element while the escape/retry/restart control is on screen
    /// (`LoginScreen::has_control`, on the `ESCAPE_AFTER_MS`/`QR_ESCAPE_AFTER_MS` clocks) — and a
    /// (route × key) harness built on `phase=` alone cannot see OK land on that control, only the
    /// phase change a beat later once the press's effect resolves. Read off the focus engine's
    /// raw cursor exactly as [`Screen::Profiles`]/[`Screen::Onboard`] are, rather than a new
    /// accessor into `screens::login`: `LoginScreen::groups` publishes nothing at all while
    /// `!has_control()`, so there is nothing for the engine to seat and `focus_record()` reads
    /// back `None`; once the control exists it is the screen's ONLY focusable element
    /// (`Seat::First`, one group, `len: 1`), so `Some` means exactly "the control is focused"
    /// with no further grammar to decode — the same one-element shortcut `Screen::Login`'s
    /// sibling used to take before phase 6 gave it several.
    Login {
        has_control: bool,
    },
    /// The who's-watching picker (`screens::profiles`), an OWNED screen since phase 6 — so, like
    /// [`Screen::Onboard`] below, its field is handed IN rather than read out of a module global:
    /// `app/run.rs` reads the engine's raw cursor off `Dispatcher::focus_record` on the frame it
    /// samples, because the roster/footer/PIN-pad state this used to read straight off
    /// `ui::profiles`'s statics now lives on the owned screen's own focus engine, which this
    /// crate-level module has no way to decode into an "avatar" bool without importing
    /// `screens::profiles`'s own element vocabulary (this module sits below `screens/`, and
    /// naming it would invert the layer the restructure draws between them).
    Profiles {
        /// The engine's raw focus element (`FocusKey::elem`) for the picker, or `-1` with no
        /// input owner focused yet. **Grammar change from the legacy line**: the field used to be
        /// `avatar=<bool>` (`ui::profiles::focus_is_avatar()`), which could say "an avatar has
        /// focus" but not WHICH one, and could not see the Sign-out footer or the PIN pad move at
        /// all; `elem=<int>` is coarser in one direction (a reader still cannot decode which
        /// element number means what without `screens::profiles`'s own layout) and finer in
        /// another (every distinct focus stop — including the ones the old field was blind to —
        /// now prints a distinct number, so a (route × key) diff at least sees SOMETHING moved).
        /// `tests/focusfp.sh`'s committed fixtures encode the old grammar and need re-recording.
        elem: i32,
    },
    /// The first-run *Favorite libraries* route (`screens::onboard`), an OWNED screen since
    /// phase 5b — so unlike every other variant here its fields are handed IN rather than read
    /// out of a module global: the cursor lives in the focus engine, and `app/run.rs` reads it
    /// off `Dispatcher::focus_record` on the frame it samples.
    Onboard {
        /// The engine's key is a table ROW (below `registry::BAND`) rather than a band control.
        list: bool,
        /// That row, or `-1` when focus is on the action band. **This is where the grammar's
        /// VALUES changed**: `TableView::sel` retained the last row while focus sat on the pill,
        /// and the engine's cursor simply is not on a row then. The field set is identical, so
        /// anything parsing this line is unaffected; a committed fixture recorded before 5b is
        /// not, and has to be re-recorded.
        row: i32,
    },
    /// the home hero + grid
    Home,
    Library,
    Detail,
    Person,
    Search,
    /// `overlay` is the same word the heartbeat prints after `overlay=`, supplied by `app.rs` from
    /// its own exhaustive match on the private `Overlay` enum.
    Player {
        overlay: &'static str,
    },
}

// (`enum Host` and its `word`/`screen` impls stood here — the probe's mirror of `app.rs`'s private
// `MenuHost`, which turned `route=itemmenu` into ` over=<host>` plus one level of recursion into
// that host screen's own fields. It served BOTH popovers until phase 10, when each in turn became
// a `ModalStack` surface: a surface's host is the top PAGE, which the line already names as
// `route=`, and each panel's own fields ride on `content` like the player's four and the Library
// menu's — `app::bridge::content_probe`. With no popover left that is a route, there is nothing
// for this type to answer about.)

/// The player HUD's focus cursor, plus whether the transport is on screen at all.
///
/// A copy of the `HudNav` cursor `plex_run` holds as a local. The type is a non-`pub` item of
/// `app`, which `lib.rs` declares as a private `mod`, so it cannot be named from here — the same
/// boundary `Route` sits behind. `visible` is `app.rs`'s `hud_visible(…)` — the same
/// value its own OK and LEFT/RIGHT arms branch on — because the HUD's visibility decides whether a
/// press moves the cursor or merely reveals the bar.
#[derive(Clone, Copy)]
pub(crate) struct Hud {
    pub(crate) focus: c_int,
    pub(crate) btn: c_int,
    pub(crate) tab: c_int,
    pub(crate) visible: bool,
}

crate::dev::latched_flag!(
    /// Is the probe armed for this boot? Resolved once, from `/tmp/plxnative-focus`.
    ///
    /// Resolved once rather than per frame for two reasons: `tests/run.py` clears `/tmp/plxnative-*`
    /// between cases, so a later read could legitimately find the file gone mid-run; and a per-frame
    /// `exists()` is a syscall this is not worth paying. Call sites may check this before building
    /// arguments — [`sample`] checks it again, so the module is correct on its own.
    ///
    /// This body was hand-rolled here first; [`crate::dev::latched_flag`] is that body, moved to the
    /// module that owns the trigger surface so every per-frame `flag` caller can have it.
    pub(crate) fn armed = "focus";
);

/// Sample the current focus state and log it if it has moved since the last sample.
///
/// Call once per frame, AFTER the frame's input has been handled and the screen drawn, so what is
/// recorded is the state a key press has already moved rather than the state it is about to.
pub(crate) fn sample(ps: &crate::route::PlaybackSession, route: &str, screen: Screen, hud: Hud, ctrl: ControlSlot, content: &str) {
    if !armed() {
        return;
    }
    let line = fingerprint_content(ps, route, screen, hud, ctrl, content);
    static LAST: Mutex<Option<String>> = Mutex::new(None);
    let mut last = LAST.lock().unwrap_or_else(|e| e.into_inner());
    if last.as_deref() == Some(line.as_str()) {
        return;
    }
    crate::log(&line);
    *last = Some(line);
}

/// The fingerprint as a value, for the recorder's logical-state hash (`app::recorder`): the
/// legacy screens' focus state as one ordered line, whether or not the probe is armed.
///
/// **It is no longer the whole of that hash, and must not be treated as it.** Since phase 5b the
/// Settings family's state lives on the container tree, where this module cannot see it — every
/// one of those screens fingerprints as its route word and nothing else. `app::recorder`'s
/// `state_hash` folds `Dispatcher::state_hash` in beside this line for exactly that reason; a
/// replay graded on this alone would call a press that opened the wrong family page `SAME`.
pub(crate) fn line(ps: &crate::route::PlaybackSession, route: &str, screen: Screen, hud: Hud, ctrl: ControlSlot, content: &str) -> String {
    fingerprint_content(ps, route, screen, hud, ctrl, content)
}

/// Build the line. Split out from [`sample`] so its determinism and its grammar are host-testable
/// without a log file or a change-detection state.
#[cfg(test)]
fn fingerprint(ps: &crate::route::PlaybackSession, route: &str, screen: Screen, hud: Hud, ctrl: ControlSlot) -> String {
    fingerprint_content(ps, route, screen, hud, ctrl, "")
}

fn fingerprint_content(ps: &crate::route::PlaybackSession, route: &str, screen: Screen, hud: Hud, ctrl: ControlSlot, content: &str) -> String {
    let mut s = String::with_capacity(192);
    s.push_str("focus route=");
    s.push_str(route);
    push_fields(ps, &mut s, screen, hud, ctrl, content);
    // The tvOS click, which is route-agnostic: an OK over a card arms a press and the activation
    // commits from the per-frame loop on the spring-back, so "a press is in flight" is a state the
    // ladder put the app into and a state the NEXT key cancels.
    let _ = write!(s, " press={}", b(crate::ui::press::is_active()));
    s
}

/// One screen's own fields. Split out of [`fingerprint`] so a popover ROUTE could spend it on its
/// HOST — the popover's line is the host's state plus the panel's, and there is no other way to say
/// that without five copies of the host arms.
fn push_fields(ps: &crate::route::PlaybackSession, s: &mut String, screen: Screen, hud: Hud, ctrl: ControlSlot, content: &str) {
    match screen {
        Screen::Login { has_control } => {
            // The phase is still most of this screen's state — it is a projection of the auth
            // phase, and its `key` handler mostly drives `auth`, not a cursor — but it is not
            // ALL of it: see [`Screen::Login`]'s own doc for the one focusable control this used
            // to leave unreported, invisible to a (route × key) harness reading `phase=` alone
            // for the ~60/~12 seconds a stalled sign-in leaves it on screen with no phase change.
            let _ = write!(s, " phase={:?} ctl={}", crate::auth::phase(), b(has_control));
        }
        Screen::Profiles { elem } => {
            // See the variant's own doc for the grammar change this replaced (`avatar=<bool>` →
            // `elem=<int>`) and why: the picker's cursor is the engine's now, not a legacy static.
            let _ = write!(s, " elem={elem}");
        }
        Screen::Onboard { list, row } => {
            // Two focus stops and a row cursor — the whole of what a press can move here. The list
            // flag and the selection are read together because `TableView::list_focused` gates the
            // pill AND the ink: a fingerprint carrying only `row` could not tell a focused row
            // from the same row with focus parked on the action beside it. Both are the ENGINE's
            // answer now (`Screen::Onboard`'s doc), which is also why they arrive as fields: this
            // module reads module globals for every legacy screen and must not reach into the
            // dispatcher to get one screen's cursor.
            let _ = write!(s, " list={} row={row}", b(list));
        }
        Screen::Home => s.push_str(content),
        Screen::Library => s.push_str(content),
        Screen::Detail | Screen::Person => s.push_str(content),
        // Owned like Library/Detail/Person: the content string is built generically by the
        // caller (`super::bridge::content_probe`, from the mounted `SearchScreen`'s own
        // `LogicalState::probe` output), not read off a legacy global here.
        Screen::Search => s.push_str(content),
        // The panels' own fields ride on `content`, for the same reason Library's, Detail's and
        // Search's do: since restructure phase 9 each is the state of a MOUNTED INSTANCE
        // (`screens::player::overlay`), and this module cannot reach into the container — the
        // caller (`app::bridge::content_probe`) builds them from the surface that is up.
        Screen::Player { overlay } => {
            push_player(ps, s, overlay, hud, ctrl);
            s.push_str(content);
        }
    }
}


/// The player: the HUD cursor, what the control row currently holds, and each panel's own state.
fn push_player(ps: &crate::route::PlaybackSession, s: &mut String, overlay: &str, hud: Hud, ctrl: ControlSlot) {
    // `upnext` is the PLAYER INSTANCE's countdown since phase 9, so it arrives on `content` with
    // the panels' fields rather than being read off a module global here.
    let slot = match ctrl {
        ControlSlot::Discs => "discs",
        ControlSlot::Skip(_) => "skip",
        ControlSlot::UpNext(_) => "upnext",
    };
    // `hidden` is `player_hud::transport_hidden()`, read here for the reason `app.rs` reads it:
    // its arm `continue`s unconditionally, so while it is true the four overlay arms below it are
    // unreachable and every key but BACK is swallowed. A characterization run that could not see
    // this field would record those arms as dead code rather than as shadowed ones.
    let _ = write!(
        s,
        " ov={} hud={} f={} btn={} tab={} slot={} items={} hidden={}",
        overlay,
        b(hud.visible),
        hud.focus,
        hud.btn,
        hud.tab,
        slot,
        ctrl.items(),
        b(crate::ui::player_hud::transport_hidden(ps))
    );
    // Whether this item HAS chapters at all — a fact about the item, not about a panel, which is
    // why it stays here while each overlay's own open flag and cursor arrive on `content` from the
    // surface (`app::bridge::content_probe`).
    //
    // The detail page's *Track information* sheet used to be written here too, as
    // `tracks=`/`tpage=`. It never belonged: no Detail panel can be up on the player route, so
    // those two fields were constant for the whole of every player recording while the page that
    // opens the sheet recorded nothing. They are on `content_probe`'s Detail line now.
    let _ = write!(
        s,
        " haschap={}",
        b(crate::ui::chapters_panel::has_chapters())
    );
}

/// The focused row's identity: its server SLOT and its rating key, or `-` for nothing focused.
///
/// A rating key is a server-local integer dense from 1, so it names an item only together with the
/// server — and the slot number is a registry index (0, 1, …), not anything about the machine.
pub(crate) fn push_item(s: &mut String, m: Option<&crate::pms::PmsMovie>) {
    match m {
        Some(m) => {
            s.push_str(" sid=");
            push_sid(s, m.sid);
            s.push_str(" rk=");
            push_rk(s, &m.rk);
        }
        None => s.push_str(" sid=- rk=-"),
    }
}

/// A registry slot number, or `-` for [`crate::plex::ServerId::UNSET`].
///
/// Spelled as the absence it is rather than as `65535`: `UNSET` is the reserved value a page
/// carries before anything mounted on it, and printing the raw `u16` reads as slot 65535 — a
/// server — on a line whose other slot numbers are 0 and 1.
fn push_sid(s: &mut String, sid: crate::plex::ServerId) {
    if sid.is_set() {
        let _ = write!(s, "{}", sid.raw());
    } else {
        s.push('-');
    }
}

/// Append a rating key, keeping ASCII alphanumerics and dropping everything else.
///
/// The filter is the redaction rule made structural rather than assumed. `pms::parse_item` fills
/// `rk` from the wire's `ratingKey`, which PMS sends as a bare number — but this line goes to a log
/// that gets pasted into public issues, and a field that simply trusted the server's string would
/// be one upstream change away from carrying a path or a title into it. Bounded too, for the same
/// reason: a key is a handful of digits, so anything longer is not a key.
pub(crate) fn push_rk(s: &mut String, rk: &str) {
    let mut n = 0;
    for c in rk.chars() {
        if n == RK_MAX {
            s.push('~'); // truncated — say so rather than silently shortening an identity
            break;
        }
        if c.is_ascii_alphanumeric() {
            s.push(c);
            n += 1;
        }
    }
    if n == 0 {
        s.push('-');
    }
}
const RK_MAX: usize = 16;

/// `1`/`0`, so a boolean field is one character wide and sorts and diffs like the integers beside
/// it. `{}` on a `bool` would print `true`/`false`, which makes a column of them ragged.
fn b(v: bool) -> u8 {
    v as u8
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hud() -> Hud {
        Hud {
            focus: 0,
            btn: 0,
            tab: 0,
            visible: false,
        }
    }

    /// Every screen this app can end a frame on, so a grammar or determinism assertion covers the
    /// whole instrument rather than the one screen a test happened to pick.
    fn every_screen() -> Vec<(&'static str, Screen)> {
        vec![
            // both "no control on screen" and "the control has focus", because the two print
            // different values through one grammar — same reason `Screen::Profiles`'s two
            // entries exist, and the exact gap `Screen::Login`'s own doc says this used to leave
            ("login", Screen::Login { has_control: false }),
            ("login", Screen::Login { has_control: true }),
            // both a real cursor and "nothing focused yet", because the two print different
            // values through one grammar — same reason `Screen::Onboard`'s two entries exist
            ("profiles", Screen::Profiles { elem: 2 }),
            ("profiles", Screen::Profiles { elem: -1 }),
            // both focus zones, because the two print different values through one grammar
            ("onboard", Screen::Onboard { list: true, row: 0 }),
            ("onboard", Screen::Onboard { list: false, row: -1 }),
            ("home", Screen::Home),
            // (three `account` rows, one per bar-wearing HOST, stood here. The profile menu is a
            // `ModalStack` surface since phase 10: its host is the top PAGE, which this line
            // already names as `route=`, and its own fields ride on `content` — so there is no
            // `Screen::Account` and no second field set for a grammar assertion to cover.)
            // (five `itemmenu` rows, one per HOST, stood beside them and went the same way in
            // the same phase — its `imenu=`/`isel=`/`imsid=` fields are on `content` now too.)
            ("library", Screen::Library),
            ("detail", Screen::Detail),
            ("person", Screen::Person),
            ("search", Screen::Search),
            ("player", Screen::Player { overlay: "none" }),
            ("player", Screen::Player { overlay: "info" }),
        ]
    }

    /// **The property the whole instrument rests on**: with nothing moved between two samples, the
    /// two lines are byte-identical. A fingerprint that varied on its own would log every frame and
    /// a harness could not read a key press out of the diff.
    ///
    /// It takes the crate-wide serial lock because it walks every screen's accessors, several of
    /// which read process-global stores that other modules' tests mutate.
    #[test]
    fn a_fingerprint_is_stable_while_nothing_moves() {
        let ps = crate::route::PlaybackSession::IDLE;
        let _g = crate::testlock::serial();
        for (rn, sc) in every_screen() {
            let a = fingerprint(&ps, rn, sc, hud(), ControlSlot::Discs);
            let b = fingerprint(&ps, rn, sc, hud(), ControlSlot::Discs);
            assert_eq!(a, b, "{rn} fingerprinted differently twice in a row");
        }
    }

    /// The grammar, which is also the redaction enforcement: one line, `focus route=<word>` first,
    /// then `key=value` pairs whose values hold no whitespace, no `/` and no `?`. A title would
    /// bring a space, a URL or path would bring a slash, and either would fail here.
    #[test]
    fn the_line_is_one_ordered_row_of_safe_key_value_pairs() {
        let ps = crate::route::PlaybackSession::IDLE;
        let _g = crate::testlock::serial();
        for (rn, sc) in every_screen() {
            let line = fingerprint(&ps, rn, sc, hud(), ControlSlot::Discs);
            assert!(
                !line.contains('\n'),
                "{rn}: a fingerprint is ONE line: {line}"
            );
            let mut fields = line.split(' ');
            assert_eq!(fields.next(), Some("focus"), "{line}");
            for f in fields {
                let (k, v) = f
                    .split_once('=')
                    .unwrap_or_else(|| panic!("{rn}: {f:?} is not key=value in {line}"));
                assert!(
                    !k.is_empty() && k.chars().all(|c| c.is_ascii_lowercase()),
                    "{rn}: odd key {k:?}"
                );
                assert!(
                    v.chars()
                        .all(|c| !c.is_whitespace() && c != '/' && c != '?' && c != '&'),
                    "{rn}: value {v:?} could carry a path, query or title: {line}"
                );
            }
        }
    }

    /// A field's PRESENCE is a function of the screen alone — the property that lets a harness diff
    /// two fingerprints key by key instead of re-parsing a variable schema.
    #[test]
    fn one_screen_always_carries_the_same_keys() {
        let ps = crate::route::PlaybackSession::IDLE;
        let _g = crate::testlock::serial();
        let keys = |rn, sc, ctrl| {
            fingerprint(&ps, rn, sc, hud(), ctrl)
                .split(' ')
                .skip(1)
                .filter_map(|f| f.split_once('=').map(|(k, _)| k.to_string()))
                .collect::<Vec<_>>()
        };
        // the player's control row swaps occupants under the same cursor — the keys must not swap
        // with it, or every marker segment would look like a schema change
        let credits = crate::metadata::Marker {
            kind: crate::metadata::MarkerKind::Credits,
            start_ms: 1000,
            end_ms: 2000,
            final_seg: false,
        };
        let discs = keys(
            "player",
            Screen::Player { overlay: "none" },
            ControlSlot::Discs,
        );
        let up = keys(
            "player",
            Screen::Player { overlay: "none" },
            ControlSlot::UpNext(credits),
        );
        assert_eq!(
            discs, up,
            "the control row's occupant changed the SCHEMA, not just a value"
        );
        // …and a HUD cursor move is a value change, never a key change
        let moved = fingerprint(&ps, 
            "player",
            Screen::Player { overlay: "none" },
            Hud {
                focus: 2,
                btn: 1,
                tab: 1,
                visible: true,
            },
            ControlSlot::Discs,
        );
        let moved_keys: Vec<String> = moved
            .split(' ')
            .skip(1)
            .filter_map(|f| f.split_once('=').map(|(k, _)| k.to_string()))
            .collect();
        assert_eq!(discs, moved_keys);
    }

    /// A moved cursor must actually show up. This is the instrument's whole purpose, and it is the
    /// one assertion that fails if a future edit prints a constant where a getter belongs.
    #[test]
    fn moving_the_hud_cursor_changes_the_line() {
        let ps = crate::route::PlaybackSession::IDLE;
        let _g = crate::testlock::serial();
        let at = |f, btn, tab| {
            fingerprint(&ps, 
                "player",
                Screen::Player { overlay: "none" },
                Hud {
                    focus: f,
                    btn,
                    tab,
                    visible: true,
                },
                ControlSlot::Discs,
            )
        };
        assert_ne!(
            at(0, 0, 0),
            at(1, 0, 0),
            "the HUD focus row is not observable"
        );
        assert_ne!(
            at(1, 0, 0),
            at(1, 1, 0),
            "the control-row cursor is not observable"
        );
        assert_ne!(
            at(2, 0, 0),
            at(2, 0, 1),
            "the tab-row cursor is not observable"
        );
        assert_ne!(
            at(0, 0, 0),
            fingerprint(&ps, 
                "player",
                Screen::Player { overlay: "info" },
                Hud {
                    focus: 0,
                    btn: 0,
                    tab: 0,
                    visible: true
                },
                ControlSlot::Discs
            ),
            "the overlay tag is not observable"
        );
    }

    /// **The stalled-sign-in control appearing must actually show up.** This is the exact gap
    /// `Screen::Login`'s own doc names: `phase=` alone reports nothing for the ~12 s (a stalled
    /// spinner) or ~60 s (an unscanned QR code) the escape/restart control sits on screen with the
    /// phase unchanged, so a (route × key) harness reading `phase=` alone cannot see OK land on the
    /// control at all — only the phase change a beat later, once the press has already been acted
    /// on. This is the assertion that fails if a future edit prints a constant (`ctl=0` always, or
    /// the field dropped) where `has_control` belongs — the same shape as
    /// `moving_the_hud_cursor_changes_the_line` above, for this screen's own one cursor.
    #[test]
    fn the_login_screens_stalled_control_appearing_is_observable() {
        let ps = crate::route::PlaybackSession::IDLE;
        let _g = crate::testlock::serial();
        let without = fingerprint(&ps, "login", Screen::Login { has_control: false }, hud(), ControlSlot::Discs);
        let with = fingerprint(&ps, "login", Screen::Login { has_control: true }, hud(), ControlSlot::Discs);
        assert_ne!(
            without, with,
            "the login screen's escape/retry/restart control appearing is not observable"
        );
    }

    /// The rating-key filter is the structural half of the redaction rule, so it is graded against
    /// the shapes it exists to stop rather than against a well-formed key.
    #[test]
    fn a_rating_key_field_cannot_carry_a_path_a_title_or_a_token() {
        let mut s = String::new();
        push_rk(&mut s, "1804");
        assert_eq!(s, "1804");

        let mut s = String::new();
        push_rk(&mut s, "/library/metadata/1804?X-Plex-Token=SECRETVALUE");
        assert!(
            !s.contains('/') && !s.contains('?') && !s.contains('='),
            "a URL survived as {s:?}"
        );
        assert!(!s.contains("SECRET"), "a token survived as {s:?}");

        let mut s = String::new();
        push_rk(&mut s, "The Curse of the Were-Rabbit");
        assert!(
            !s.contains(' ') && !s.contains('-'),
            "a title survived as {s:?}"
        );

        // empty is `-`, never an empty field that would collapse two columns into one
        let mut s = String::new();
        push_rk(&mut s, "");
        assert_eq!(s, "-");

        // …and length is bounded, with the truncation SAID rather than silently applied
        let mut s = String::new();
        push_rk(&mut s, "01234567890123456789");
        assert_eq!(s, "0123456789012345~");

        // multi-byte input must not panic the scan (the app holds real Plex strings)
        let mut s = String::new();
        push_rk(&mut s, "sé☃ance42");
        assert_eq!(s, "sance42");
    }

    /// `armed()` is what every call site checks first, and it must answer the same thing for the
    /// whole boot. Without the latch a run that cleared `/tmp` mid-case would stop fingerprinting
    /// half way through and read as "focus stopped moving".
    #[test]
    fn armed_answers_once_for_the_whole_process() {
        let first = armed();
        for _ in 0..3 {
            assert_eq!(armed(), first);
        }
    }
}
