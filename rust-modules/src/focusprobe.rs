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
//! **Redaction.** The event log is pasted into public issue threads (`ui/stats.rs`'s module doc has
//! the reasoning, and `dev.rs`'s `DevServer::describe` has the incident). So the line carries
//! indices, enum tags, booleans, and — as the only identity — rating keys and the registry SLOT
//! number of a server. No title, no path, no URL, no server name, address or `machineIdentifier`,
//! no token. The enforcement is structural rather than a matter of care: every value written here
//! is either a number, a bool, a `&'static str` tag chosen in this file, or a rating key that goes
//! through [`push_rk`], which copies ASCII alphanumerics and nothing else.
//!
//! **Diffable.** Fixed field order, one line, no timestamps, no addresses, and no float is ever
//! printed — the two spring positions that decide a branch (`home::snap_pos`, and the target beside
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
    Login,
    Profiles,
    /// the home hero + grid
    Home,
    /// Home, with the top-left profile popover over it
    Account,
    /// the press-and-hold card menu, over whichever screen the hold happened on
    ItemMenu {
        over: Host,
    },
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

/// Which live screen a [`Screen::ItemMenu`] popover is sitting on — the probe's mirror of `app.rs`'s
/// private `MenuHost`, mapped by the same exhaustive `match` that maps `Route`.
///
/// A type and not a bool: the menu had exactly two hosts and the fingerprint spelled them
/// `over_detail: bool`, which stops being expressible the moment there is a third. Every host is a
/// [`Screen`] in its own right, so the popover's line is the HOST's fields plus the panel's — see
/// [`Host::screen`].
#[derive(Clone, Copy)]
pub(crate) enum Host {
    Home,
    Detail,
    Library,
    Search,
    Person,
}

impl Host {
    /// The word printed after `over=`. Anything reading a schema off a `route=itemmenu` line needs
    /// it, because one route word now covers five different field sets.
    fn word(self) -> &'static str {
        match self {
            Host::Home => "home",
            Host::Detail => "detail",
            Host::Library => "library",
            Host::Search => "search",
            Host::Person => "person",
        }
    }
    /// The host as the screen it is — the popover sits on a LIVE screen, so the host's own focus is
    /// still part of the state (the tile the menu is about is the one still focused behind it) and
    /// its fields are exactly that screen's. Never [`Screen::ItemMenu`], which is what stops
    /// [`push_fields`]' one level of recursion from being a loop.
    fn screen(self) -> Screen {
        match self {
            Host::Home => Screen::Home,
            Host::Detail => Screen::Detail,
            Host::Library => Screen::Library,
            Host::Search => Screen::Search,
            Host::Person => Screen::Person,
        }
    }
}

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
pub(crate) fn sample(route: &str, screen: Screen, hud: Hud, ctrl: ControlSlot) {
    if !armed() {
        return;
    }
    let line = fingerprint(route, screen, hud, ctrl);
    static LAST: Mutex<Option<String>> = Mutex::new(None);
    let mut last = LAST.lock().unwrap_or_else(|e| e.into_inner());
    if last.as_deref() == Some(line.as_str()) {
        return;
    }
    crate::log(&line);
    *last = Some(line);
}

/// Build the line. Split out from [`sample`] so its determinism and its grammar are host-testable
/// without a log file or a change-detection state.
fn fingerprint(route: &str, screen: Screen, hud: Hud, ctrl: ControlSlot) -> String {
    let mut s = String::with_capacity(192);
    s.push_str("focus route=");
    s.push_str(route);
    push_fields(&mut s, screen, hud, ctrl);
    // The tvOS click, which is route-agnostic: an OK over a card arms a press and the activation
    // commits from the per-frame loop on the spring-back, so "a press is in flight" is a state the
    // ladder put the app into and a state the NEXT key cancels.
    let _ = write!(s, " press={}", b(crate::ui::press::is_active()));
    s
}

/// One screen's own fields. Split out of [`fingerprint`] so [`Screen::ItemMenu`] can spend it on its
/// HOST — the popover's line is the host's state plus the panel's, and there is no other way to say
/// that without five copies of the host arms.
fn push_fields(s: &mut String, screen: Screen, hud: Hud, ctrl: ControlSlot) {
    match screen {
        Screen::Login => {
            // `login.rs` holds no focus state of its own — the screen is a projection of the auth
            // phase, and its `key` handler drives `auth`, not a cursor. So the phase IS the state.
            let _ = write!(s, " phase={:?}", crate::auth::phase());
        }
        Screen::Profiles => {
            // The picker's one read-only focus accessor. It leaves the avatar INDEX, the Sign-out
            // footer and the whole PIN pad (its open flag and its own two-dimensional cursor)
            // unreadable from outside `profiles.rs` — so a fingerprint cannot tell one avatar from
            // another, and cannot see a keypad move at all.
            let _ = write!(s, " avatar={}", b(crate::ui::profiles::focus_is_avatar()));
        }
        Screen::Home => push_home(s),
        Screen::Account => {
            push_home(s);
            let _ = write!(s, " acct={} asel={}", b(crate::ui::account_menu::is_open()), crate::ui::account_menu::sel());
        }
        Screen::ItemMenu { over } => {
            // The HOST is named on the line, because `route=itemmenu` is one word for FIVE different
            // field sets: the popover sits on a LIVE screen, so the host's own focus is still part
            // of the state (the tile the menu is about is the one still focused behind it), and no
            // two hosts have the same fields to print. Anything reading a schema off this line needs
            // `over=` as well as `route=`.
            let _ = write!(s, " over={}", over.word());
            push_fields(s, over.screen(), hud, ctrl);
            let _ = write!(s, " imenu={} isel={} imsid=", b(crate::ui::item_menu::is_open()), crate::ui::item_menu::sel());
            push_sid(s, crate::ui::item_menu::item_sid());
        }
        Screen::Library => {
            let pill = crate::ui::library::focused_pill().map(|p| p as i64).unwrap_or(-1);
            let _ = write!(
                s,
                " pill={} card={} menu={}",
                pill,
                b(crate::ui::library::focus_is_card()),
                b(crate::ui::library::menu_open())
            );
            push_item(s, crate::ui::library::focused_item());
        }
        Screen::Detail => push_detail(s),
        Screen::Person => {
            let _ = write!(s, " card={}", b(crate::ui::person::focus_is_card()));
            push_item(s, crate::ui::person::focused_item());
        }
        Screen::Search => {
            // The whole state machine, from the snapshot the screen's own regions already draw off
            // (`search::view`) — so the fingerprint and the picture are built from one read. `zone`
            // is what the ladder's arms branch on, `editing` gates the field's caret AND freezes the
            // shelves, and `row`/`col`/`recent` are three cursors the ladder moves independently.
            // `shift` is a float and deliberately absent: it is derived from the focused row, so it
            // carries nothing the row does not, and a spring position would make this line jitter.
            let v = crate::ui::search::view();
            let _ = write!(
                s,
                " zone={:?} editing={} row={} col={} recent={} pill={} card={} below={:?} clear={}",
                v.zone,
                b(v.editing),
                v.row,
                v.col,
                v.recent,
                // the pill under the ring, from the SHARED bar's one answer. Still `pill=<int>` and
                // still -1 off the strip: `zone=` on this same line already tells the chip apart
                // from focus being off the bar, and `tests/keytable.json` is a recorded device
                // golden — renaming a field there costs a TV run to regenerate, for no new fact.
                match crate::ui::search::top_focus() {
                    crate::ui::widgets::TopFocus::Pill(i) => i as i64,
                    _ => -1,
                },
                b(crate::ui::search::focus_is_card()),
                crate::ui::search::below(),
                crate::ui::search::clear_index()
            );
        }
        Screen::Player { overlay } => push_player(s, overlay, hud, ctrl),
    }
}

/// Home's hero band and grid, which are one screen with two focus models.
///
/// Both cursors are printed whichever view is showing, because both are RETAINED: a DOWN out of the
/// hero leaves `hf` where it was and a UP back into it leaves `row`/`col` where they were, and the
/// ladder's next press acts on whichever the snap selects.
fn push_home(s: &mut String) {
    // Two booleans, not the floats. `snapt` is the snap spring's TARGET, which a press flips on the
    // press frame; `snapp` is its live POSITION crossing the same 0.5 the code compares it against.
    // They disagree for the length of the snap animation, and `app.rs`'s OK arm reads the POSITION
    // on purpose ("a quick DOWN→OK must still act on the hero shown") while its D-pad arm reads the
    // target — so a harness needs both to explain a dispatch, and neither may be a raw float,
    // which would jitter a fingerprint on every frame of the animation.
    let target_is_grid = crate::ui::home::snap_target() > 0.5;
    // Bound once and reused below: `home::col()` calls `home::row()` internally and both clamp
    // against a live hub-length lookup, so asking twice pays for that walk twice.
    let (row, col) = (crate::ui::home::row(), crate::ui::home::col());
    let _ = write!(
        s,
        " snapt={} snapp={} hf={} row={row} col={col}",
        b(target_is_grid),
        b(crate::ui::home::focus_is_card()),
        crate::ui::home::hero_focus()
    );
    // …and the item a press would act on, which is the hero's or the grid cell's by the TARGET.
    // NB the hero rotates on an 8 s timer of its own, so on the hero view this field can change
    // with no key involved. That is not noise — what Play would launch really did change.
    let item = if target_is_grid { crate::ui::home::movie_at(row, col) } else { crate::ui::home::hero_item() };
    push_item(s, item);
}

/// The detail page: its whole [`crate::ui::detail::Spot`], plus the panel that can be over it.
fn push_detail(s: &mut String) {
    let sp = crate::ui::detail::spot();
    let _ = write!(s, " sec={} col={} eptext={}", sp.section, sp.col, b(sp.ep_text));
    match sp.season {
        Some(n) => {
            let _ = write!(s, " season={n}");
        }
        None => s.push_str(" season=-"),
    }
    // The per-section column memory: six integers the ladder writes on every move OFF a section and
    // reads back on every move onto one, so a fingerprint without it cannot explain where a DOWN
    // then UP lands.
    s.push_str(" saved=");
    for (i, c) in sp.saved_col.iter().enumerate() {
        if i > 0 {
            s.push(',');
        }
        let _ = write!(s, "{c}");
    }
    let _ = write!(
        s,
        " card={} alt={} show={} sid=",
        b(crate::ui::detail::focus_is_card()),
        b(crate::ui::alt_sources::is_open()),
        b(crate::ui::detail::is_show())
    );
    push_sid(s, crate::ui::detail::mounted_sid());
    s.push_str(" rk=");
    push_rk(s, &crate::ui::detail::mounted_rk());
    // The focused EPISODE is a second identity on this page and not derivable from `col` alone: the
    // filmstrip is per season, and this reads `None` while a season is still loading.
    s.push_str(" ep=");
    match crate::ui::detail::focused_episode() {
        Some((rk, watched)) => {
            push_rk(s, &rk);
            let _ = write!(s, " epwatched={}", b(watched));
        }
        None => s.push_str("- epwatched=-"),
    }
}

/// The player: the HUD cursor, what the control row currently holds, and each panel's own state.
fn push_player(s: &mut String, overlay: &str, hud: Hud, ctrl: ControlSlot) {
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
        " ov={} hud={} f={} btn={} tab={} slot={} items={} hidden={} upnext={}",
        overlay,
        b(hud.visible),
        hud.focus,
        hud.btn,
        hud.tab,
        slot,
        ctrl.items(),
        b(crate::ui::player_hud::transport_hidden()),
        b(crate::ui::up_next::armed())
    );
    // The panels: whether each is open, ITS HIGHLIGHTED ROW, and for the track menu the two tracks
    // already committed. The row is what makes the four overlay arms characterizable at all — each
    // has an UP/DOWN branch that moves a cursor and changes nothing else, so with only the open
    // flags this line recorded a panel appearing and disappearing with a hole between. The four
    // `sel()` readers were added for this and do nothing else.
    let _ = write!(
        s,
        " menu={} msel={} taudio={} tsub={} info={} isel={} infolast={} chap={} csel={} haschap={} more={} osel={}",
        b(crate::ui::track_menu::is_open()),
        crate::ui::track_menu::sel(),
        crate::ui::track_menu::active_audio(),
        crate::ui::track_menu::active_sub(),
        b(crate::ui::info_panel::is_open()),
        crate::ui::info_panel::sel(),
        b(crate::ui::info_panel::at_last()),
        b(crate::ui::chapters_panel::is_open()),
        crate::ui::chapters_panel::sel(),
        b(crate::ui::chapters_panel::has_chapters()),
        b(crate::ui::more_menu::is_open()),
        crate::ui::more_menu::sel()
    );
}

/// The focused row's identity: its server SLOT and its rating key, or `-` for nothing focused.
///
/// A rating key is a server-local integer dense from 1, so it names an item only together with the
/// server — and the slot number is a registry index (0, 1, …), not anything about the machine.
fn push_item(s: &mut String, m: Option<&crate::pms::PmsMovie>) {
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
fn push_rk(s: &mut String, rk: &str) {
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
        Hud { focus: 0, btn: 0, tab: 0, visible: false }
    }

    /// The one screen precondition a host test does not get for free.
    ///
    /// `profiles::scene()` is an `expect("profiles::init not called")`, unlike every other screen
    /// here, which lazily insert. `plex_run` calls `profiles::init()` unconditionally at boot,
    /// before the loop this probe samples from, so the app cannot reach the fingerprint without it
    /// — a test walking every screen has to mount the same precondition, through the screen's own
    /// entry point rather than by reaching into its state. Held under `testlock::serial()` by every
    /// caller, because it writes a process global.
    fn mount_profiles() {
        crate::ui::profiles::init();
    }

    /// Every screen this app can end a frame on, so a grammar or determinism assertion covers the
    /// whole instrument rather than the one screen a test happened to pick.
    fn every_screen() -> Vec<(&'static str, Screen)> {
        vec![
            ("login", Screen::Login),
            ("profiles", Screen::Profiles),
            ("home", Screen::Home),
            ("account", Screen::Account),
            // one per HOST — the popover's field set is its host's, so five hosts are five
            // different `route=itemmenu` lines and the grammar assertions have to see all of them
            ("itemmenu", Screen::ItemMenu { over: Host::Home }),
            ("itemmenu", Screen::ItemMenu { over: Host::Detail }),
            ("itemmenu", Screen::ItemMenu { over: Host::Library }),
            ("itemmenu", Screen::ItemMenu { over: Host::Search }),
            ("itemmenu", Screen::ItemMenu { over: Host::Person }),
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
        let _g = crate::testlock::serial();
        mount_profiles();
        for (rn, sc) in every_screen() {
            let a = fingerprint(rn, sc, hud(), ControlSlot::Discs);
            let b = fingerprint(rn, sc, hud(), ControlSlot::Discs);
            assert_eq!(a, b, "{rn} fingerprinted differently twice in a row");
        }
    }

    /// The grammar, which is also the redaction enforcement: one line, `focus route=<word>` first,
    /// then `key=value` pairs whose values hold no whitespace, no `/` and no `?`. A title would
    /// bring a space, a URL or path would bring a slash, and either would fail here.
    #[test]
    fn the_line_is_one_ordered_row_of_safe_key_value_pairs() {
        let _g = crate::testlock::serial();
        mount_profiles();
        for (rn, sc) in every_screen() {
            let line = fingerprint(rn, sc, hud(), ControlSlot::Discs);
            assert!(!line.contains('\n'), "{rn}: a fingerprint is ONE line: {line}");
            let mut fields = line.split(' ');
            assert_eq!(fields.next(), Some("focus"), "{line}");
            for f in fields {
                let (k, v) = f.split_once('=').unwrap_or_else(|| panic!("{rn}: {f:?} is not key=value in {line}"));
                assert!(!k.is_empty() && k.chars().all(|c| c.is_ascii_lowercase()), "{rn}: odd key {k:?}");
                assert!(
                    v.chars().all(|c| !c.is_whitespace() && c != '/' && c != '?' && c != '&'),
                    "{rn}: value {v:?} could carry a path, query or title: {line}"
                );
            }
        }
    }

    /// A field's PRESENCE is a function of the screen alone — the property that lets a harness diff
    /// two fingerprints key by key instead of re-parsing a variable schema.
    #[test]
    fn one_screen_always_carries_the_same_keys() {
        let _g = crate::testlock::serial();
        let keys = |rn, sc, ctrl| {
            fingerprint(rn, sc, hud(), ctrl)
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
        let discs = keys("player", Screen::Player { overlay: "none" }, ControlSlot::Discs);
        let up = keys("player", Screen::Player { overlay: "none" }, ControlSlot::UpNext(credits));
        assert_eq!(discs, up, "the control row's occupant changed the SCHEMA, not just a value");
        // …and a HUD cursor move is a value change, never a key change
        let moved = fingerprint(
            "player",
            Screen::Player { overlay: "none" },
            Hud { focus: 2, btn: 1, tab: 1, visible: true },
            ControlSlot::Discs,
        );
        let moved_keys: Vec<String> =
            moved.split(' ').skip(1).filter_map(|f| f.split_once('=').map(|(k, _)| k.to_string())).collect();
        assert_eq!(discs, moved_keys);
    }

    /// A moved cursor must actually show up. This is the instrument's whole purpose, and it is the
    /// one assertion that fails if a future edit prints a constant where a getter belongs.
    #[test]
    fn moving_the_hud_cursor_changes_the_line() {
        let _g = crate::testlock::serial();
        let at = |f, btn, tab| {
            fingerprint(
                "player",
                Screen::Player { overlay: "none" },
                Hud { focus: f, btn, tab, visible: true },
                ControlSlot::Discs,
            )
        };
        assert_ne!(at(0, 0, 0), at(1, 0, 0), "the HUD focus row is not observable");
        assert_ne!(at(1, 0, 0), at(1, 1, 0), "the control-row cursor is not observable");
        assert_ne!(at(2, 0, 0), at(2, 0, 1), "the tab-row cursor is not observable");
        assert_ne!(
            at(0, 0, 0),
            fingerprint("player", Screen::Player { overlay: "info" }, Hud { focus: 0, btn: 0, tab: 0, visible: true }, ControlSlot::Discs),
            "the overlay tag is not observable"
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
        assert!(!s.contains('/') && !s.contains('?') && !s.contains('='), "a URL survived as {s:?}");
        assert!(!s.contains("SECRET"), "a token survived as {s:?}");

        let mut s = String::new();
        push_rk(&mut s, "The Curse of the Were-Rabbit");
        assert!(!s.contains(' ') && !s.contains('-'), "a title survived as {s:?}");

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
