//! SDL event decoding and the remote-FIFO token synthesis — the raw-offset reads of LG's shifted
//! `SDL_KeyboardEvent`, the synthetic key/pointer/wheel events, and `dispatch_remote_token`.
//! Moved out of `app.rs` verbatim in phase 1a (a pure move; `pub(crate)` widening only).
//!
//! The one decode that yields a platform-neutral value is [`decode_host_event`]: SDL's quit,
//! application-lifecycle, key and text events become a [`HostEvent`], which the loop consumes in one
//! function. A key carries its canonical [`Action`] (`ui::consts`) beside the raw pair.

use super::*;
use plx_machine::machine::Edge;
use plx_ui::consts::{action_of, raw_pair, Action};

/// Preserve one IME commit and its place among key events. Desktop text does not imply an
/// on-screen panel; the owning field decides whether it is editing when delivery reaches it.
pub(crate) fn text_inputs(text: &str, panel: bool, at: plx_machine::machine::Tick,
    source: plx_machine::machine::Source) -> Vec<plx_machine::machine::InputEvent<u32>> {
    use plx_machine::machine::{InputEvent, InputKind, TextEdit};
    if text.is_empty() { return Vec::new(); }
    let mut events = Vec::with_capacity(if panel { 2 } else { 1 });
    if panel { events.push(InputEvent { at, source, kind: InputKind::SystemKeyboard(true) }); }
    events.push(InputEvent { at, source, kind: InputKind::Text(TextEdit::Commit(text.into())) });
    events
}

#[inline]
pub(crate) fn rd_u32(ev: &[u8], off: usize) -> u32 {
    u32::from_ne_bytes([ev[off], ev[off + 1], ev[off + 2], ev[off + 3]])
}

/// A keyboard event's `(state, wcode, sym)`, decoded from the raw event bytes.
///
/// **The two SDLs disagree about this struct, and nothing warns you.** LG's fork writes
/// `state` (u32) at +16, the webOS keycode at +20 and the SDL sym at +24. Stock SDL2 —
/// what the host simulator links — has `SDL_KeyboardEvent { type, timestamp, windowID,
/// state:u8@12, repeat:u8@13, pad, pad, keysym{ scancode:u32@16, sym:i32@20, … } }`, so
/// every field the app reads is at a different offset and there is no webOS keycode at all.
/// Reading the fork's offsets out of a stock event yields the window id as a keystate and
/// a scancode as a sym — plausible-looking garbage rather than a crash.
///
/// `cfg!` rather than `#[cfg]` deliberately: both arms stay compiled on both platforms, so
/// the one nobody is currently building cannot rot. This is the single site that knows the
/// layout — `rd_u32`'s callers elsewhere read pointer events, whose offsets already agree.
///
/// **The `wcode` this returns is LG's SCANCODE**, which is worth saying because the name suggests
/// otherwise. Every field measured off the dev set is the SDL scancode of the key beside it —
/// backspace 42, Clear 156, ◀/▶ 80/79, OK 40 — so the fork's shift puts an ordinary
/// `SDL_Keysym { scancode, sym }` at +20/+24 rather than inventing a webOS keycode namespace. That
/// is why the codes above SDL's own range (450 play, 482 back, 505 exit) carry no sym at all: they
/// are LG-private scancodes the keymap has no keycode for. `docs/remote-keys.md` has the account.
///
/// # The simulator's carrier moved, because the old one was silently DEAD
///
/// A synthetic press from the remote FIFO has to get its wcode across `SDL_PushEvent`, and on the
/// host that queue is not SDL2's: macOS `libSDL2` is **sdl2-compat forwarding into SDL3**, so every
/// pushed event is converted out and back. Measured 2026-08-23 by dumping the polled bytes of a
/// press whose every spare field carried a distinct value:
///
/// | field                        | offset | survives |
/// |------------------------------|--------|----------|
/// | `windowID`                   |  +8    | yes      |
/// | `padding2` / `padding3`      | +14/15 | no       |
/// | `keysym.scancode`            | +16    | **no** — comes back 0 |
/// | `keysym.mod`                 | +24    | yes      |
/// | `keysym.unused`              | +28    | **no** — comes back 1 |
///
/// It used to be `unused`, so **every wcode-ONLY token was dead**: `play`, `pause`, `stop`, `ff`,
/// `rew`, `playpause`, `exit`, `chup`, `chdown` all decoded as wcode 1 and did nothing at all, and
/// `tests/keytable.json` recorded three of them as `moved: false` — which reads as "that key does
/// nothing on that screen" and actually meant the press never arrived. An instrument silent by
/// construction: prove it can see the thing before reading its silence.
///
/// `scancode` would have been the honest home (it is what a wcode IS) and it is the one field the
/// compat layer recomputes. So a synthetic press carries the value in `mod` and a MARKER in
/// `windowID` ([`SYNTH_WINDOW`]) — two fields, because the value alone cannot say whether it is
/// one: a real press puts its modifier bitmask in `mod`, so `mod != 0` means "shift is down" at
/// least as often as it means "injected". The marker restores the priority the `unused` field used
/// to express — **injected first, the desktop stand-in only as a fallback** — which is load-bearing
/// and not a preference: `host_wcode(8)` is BACK, while `remote_token_key`'s `backspace` token is
/// `(8, 42)`, so asking the stand-in first turns the panel's delete key into a navigation.
/// Clobbering `windowID` is safe here and nowhere else: this arm is `hostsim`-only, the simulator
/// has one window, and nothing in the event loop reads that field.
#[inline]
pub(crate) fn decode_key(ev: &[u8]) -> (u32, u32, u32) {
    if cfg!(feature = "hostsim") {
        let pressed = *ev.get(12).unwrap_or(&0) as u32;
        let repeat = *ev.get(13).unwrap_or(&0) as u32;
        let sym = rd_u32(ev, 20);
        // Rebuild the fork's packed state byte-for-byte: low byte pressed(1)/released(0),
        // bit 0x100 auto-repeat. Everything downstream tests exactly those two.
        let state = pressed | if repeat != 0 { 0x100 } else { 0 };
        let injected = if rd_u32(ev, 8) == SYNTH_WINDOW {
            u32::from(u16::from_ne_bytes([ev[24], ev[25]]))
        } else {
            0 // a real desktop press: `mod` there is the modifier bitmask, not a wcode
        };
        // The stand-in is the only way a physical Mac keyboard reaches a key the remote has and a
        // keyboard does not (space = PAUSE), and it applies to real presses alone.
        let wcode = if injected != 0 {
            injected
        } else {
            host_wcode(sym)
        };
        (state, wcode, sym)
    } else {
        (rd_u32(ev, 16), rd_u32(ev, 20), rd_u32(ev, 24))
    }
}

/// The `windowID` a SYNTHETIC key event carries on the host, marking it as one — see [`decode_key`]
/// for why the wcode needs a marker beside it rather than standing on its own. Any value no real
/// window can have; SDL numbers windows from 1.
///
/// Defined unconditionally, like both halves of [`decode_key`]: the arm that uses it is behind
/// `cfg!` rather than `#[cfg]`, precisely so the configuration nobody is currently building cannot
/// rot.
pub(crate) const SYNTH_WINDOW: u32 = 0x504c_584b; // "PLXK"

/// The Magic Remote button a desktop keyboard stands in for, or 0.
///
/// Only the keys with NO sym equivalent need this. Navigation and OK/BACK already work on a
/// keyboard through `is_ok`/`is_back`, which accept RETURN/ESCAPE/'q' — those predicates were
/// always keyboard-capable, which is why the simulator needs no remapping layer for them.
#[inline]
pub(crate) fn host_wcode(sym: u32) -> u32 {
    // ASCII literals spelled numerically: `b'p' as u32` is an expression, not a pattern.
    match sym {
        32 => plx_ui::consts::WCODE_PAUSE, // space
        112 => plx_ui::consts::WCODE_PLAY, // 'p'
        115 => plx_ui::consts::WCODE_STOP, // 's'
        8 => plx_ui::consts::WCODE_BACK,   // backspace
        _ => 0,
    }
}

/// SDL's application events (`SDL_APP_*`), the four the loop acts on: the compositor taking the
/// screen (WILL and DID enter background) and giving it back (WILL and DID enter foreground).
const SDL_APP_WILLENTERBACKGROUND: u32 = 0x103;
const SDL_APP_DIDENTERBACKGROUND: u32 = 0x104;
const SDL_APP_WILLENTERFOREGROUND: u32 = 0x105;
const SDL_APP_DIDENTERFOREGROUND: u32 = 0x106;

/// What the platform tells the loop: a key press, a text commit, the window leaving or returning to
/// the screen, and the host asking the application to end. App-private, like
/// [`Shell`](super::platform::Shell): no platform reaches it by a type of its own until the
/// platform contract is settled (`docs/platform-ports.md`, Target shape 2).
///
/// The SDL drain decodes SDL's events into it ([`decode_host_event`]) and the loop consumes it in
/// ONE function (`run::ingest_host_event`), so a host that stops the application without SDL
/// (the hosted entry's `plx_host_stop`) arrives as the same [`HostEvent::Quit`] an `SDL_QUIT`
/// does and takes the same path, and a host with no SDL key pair sends a [`HostEvent::Key`] that
/// carries an [`Action`]. Pointer and wheel are not here yet: they keep their raw path in
/// `run::ingest_sdl_event_with_window` until their own step (W5c) moves them.
///
/// There is no `Edit` event: nothing produces one. `SDL_TEXTEDITING` (the IME pre-edit) is read by
/// nothing and stays a log-only raw event, and the system keyboard's edit keys (caret, backspace,
/// clear-all) are ordinary [`HostEvent::Key`] actions. `Edit` arrives with its first producer (W9).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum HostEvent {
    /// The window left or came back to the screen.
    Lifecycle(Lifecycle),
    /// The host asks the application to end (`SDL_QUIT` on SDL platforms).
    Quit,
    /// One edge of a key.
    Key(KeyPress),
    /// One commit from the platform's text input (the television's own keyboard): the decoded
    /// string, possibly empty (SDL delivers a commit for the IME's own bookkeeping too, and the
    /// loop ignores an empty one as it always has). The platform's own text layout is decoded
    /// before it gets here (`textinput::decode`, the one place that knows the offsets).
    Text(String),
}

/// The raw `(sym, wcode)` pair a key arrived as. Kept beside the [`Action`] because three things
/// still read it: a press with no action (an unsupported key flows on as exactly the pair it was),
/// the lab trigger key (named by `lab.json`, not by an action), and the recorder's tape, whose
/// format IS the pair. A platform with no SDL pair sends none, and the loop builds
/// [`raw_pair`] for its action.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct RawKey {
    pub sym: c_uint,
    pub wcode: c_uint,
}

/// One edge of one key, as a platform hands it to the loop: the canonical [`Action`] where the key
/// has one, the [`Edge`], and the raw pair where the platform has one.
///
/// **A press is self-consistent, and that is a type's guarantee, not a convention:** the fields are
/// private to this module and [`KeyPress::new`] makes the action and the pair one key. It refuses a
/// press that carries neither field, a stated action that is not what [`action_of`] makes of the raw
/// pair beside it, and an action alone whose [`raw_pair`] does not read back as that action
/// (`Digit(10)` names no key); a raw pair with no stated action has the action [`action_of`] reads
/// out of it. So the loop never reads a key it cannot name, and the action and the pair can never
/// disagree about which key was pressed. An SDL press always has a raw pair (its action is what
/// [`action_of`] makes of it, `None` for an unbound key); a host that knows only the meaning sends
/// the action alone and [`KeyPress::pair`] expands it with [`raw_pair`], which is the pair a
/// remote's press of that key would have been.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct KeyPress {
    action: Option<Action>,
    edge: Edge,
    raw: Option<RawKey>,
}

impl KeyPress {
    /// A press of `action` and/or `raw`, or `None` when they do not make one key:
    /// * with a raw pair, the action is [`action_of`] of it (`None` for a pair no action names: an
    ///   unsupported key still flows on as the pair it was), and an `action` that is stated and
    ///   differs is refused;
    /// * with no raw pair, `action` must be set and its [`raw_pair`] must read back as it, which
    ///   refuses `Digit(10..=255)`, an action no pair spells.
    pub(crate) fn new(action: Option<Action>, edge: Edge, raw: Option<RawKey>) -> Option<KeyPress> {
        let action = match raw {
            Some(RawKey { sym, wcode }) => {
                let read = action_of(sym, wcode);
                if action.is_some() && action != read {
                    return None;
                }
                read
            }
            None => {
                let named = action?;
                let (sym, wcode) = raw_pair(named);
                if action_of(sym, wcode) != Some(named) {
                    return None;
                }
                Some(named)
            }
        };
        Some(KeyPress { action, edge, raw })
    }

    /// The press an SDL key event is: the raw pair as `decode_key` reads it, its action, and the
    /// edge the fork's packed `state` names (low byte 1 is down, bit 0x100 is the auto-repeat; the
    /// same reading `bridge::key_input` makes of it). A key-up never carries the repeat bit here.
    pub(crate) fn from_sdl(ev: &[u8]) -> Option<KeyPress> {
        let (state, wcode, sym) = decode_key(ev);
        let edge = if (state & 0xff) != 1 {
            Edge::Up
        } else if state & 0x100 != 0 {
            Edge::Repeat
        } else {
            Edge::Down
        };
        KeyPress::new(action_of(sym, wcode), edge, Some(RawKey { sym, wcode }))
    }

    /// What the press means. Read by the tests only for now: the loop's ladder reads [`pair`](Self::pair),
    /// which every platform's press has, and the screens that move onto the action (W5d) make this
    /// a production accessor then.
    #[cfg(test)]
    pub(crate) fn action(&self) -> Option<Action> {
        self.action
    }

    /// The pair the loop reads: the raw one a platform sent, else the one [`raw_pair`] gives the action.
    pub(crate) fn pair(&self) -> RawKey {
        self.raw.unwrap_or_else(|| {
            let (sym, wcode) = raw_pair(self.action.expect("a KeyPress carries an action or a raw pair"));
            RawKey { sym, wcode }
        })
    }

    /// The fork's packed key state for this edge, the form `bridge::key_input` and the recorder read:
    /// low byte 1 for a press, 0x100 on an auto-repeat.
    pub(crate) fn state(&self) -> u32 {
        match self.edge {
            Edge::Down => 1,
            Edge::Repeat => 1 | 0x100,
            Edge::Up => 0,
        }
    }
}

/// One edge of the window's visibility. Both background edges park the tree (the second finds
/// nothing left to park); WILL foreground un-parks the tree, and DID foreground alone reacquires the
/// window and restores playback. A platform that sends only DID background is enough on its own
/// (`WindowActivity`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Lifecycle {
    WillBackground,
    DidBackground,
    WillForeground,
    DidForeground,
}

impl Lifecycle {
    /// The SDL event type this edge arrives as, and so the code the SDL-coded readers take: the
    /// recorder's tape (`enc_lifecycle`, whose recorded codes ARE the tape format), the window gate
    /// and telemetry's crash steps and session. The inverse of [`decode_host_event`]'s table;
    /// `host_event_tests` pins the four pairs.
    pub(crate) const fn sdl_code(self) -> u32 {
        match self {
            Lifecycle::WillBackground => SDL_APP_WILLENTERBACKGROUND,
            Lifecycle::DidBackground => SDL_APP_DIDENTERBACKGROUND,
            Lifecycle::WillForeground => SDL_APP_WILLENTERFOREGROUND,
            Lifecycle::DidForeground => SDL_APP_DIDENTERFOREGROUND,
        }
    }
}

/// An SDL event decoded into a [`HostEvent`]: `SDL_QUIT`, the four application lifecycle types, a
/// key edge (`SDL_KEYDOWN`/`SDL_KEYUP`, through [`decode_key`], so both layouts) and a text commit
/// (`SDL_TEXTINPUT`, through `textinput::decode`). `None` for everything else, which the loop reads
/// raw (pointer, wheel, the IME's pre-edit, and the SDL events nothing acts on). Beside
/// [`decode_key`], the other place that knows SDL's event bytes.
#[inline]
pub(crate) fn decode_host_event(ev: &[u8]) -> Option<HostEvent> {
    Some(match rd_u32(ev, 0) {
        SDL_QUIT => HostEvent::Quit,
        SDL_KEYDOWN | SDL_KEYUP => HostEvent::Key(KeyPress::from_sdl(ev)?),
        SDL_TEXTINPUT => HostEvent::Text(crate::textinput::decode(ev)),
        SDL_APP_WILLENTERBACKGROUND => HostEvent::Lifecycle(Lifecycle::WillBackground),
        SDL_APP_DIDENTERBACKGROUND => HostEvent::Lifecycle(Lifecycle::DidBackground),
        SDL_APP_WILLENTERFOREGROUND => HostEvent::Lifecycle(Lifecycle::WillForeground),
        SDL_APP_DIDENTERFOREGROUND => HostEvent::Lifecycle(Lifecycle::DidForeground),
        _ => return None,
    })
}

/// The bytes a synthetic key event needs, in whichever layout [`decode_key`] reads.
///
/// **The inverse of `decode_key`, and the pair is only correct together.** They already shipped
/// disagreeing once: the simulator accepted every FIFO token and never moved, because this end
/// wrote LG's fork layout while the reading end had been taught stock SDL2's. Nothing in the
/// compiler couples them, so `key_bytes_round_trip` below is what does.
///
/// Pure, and separate from the `SDL_PushEvent` that consumes it, precisely so that test can run on
/// the host — `make check` links no SDL.
pub(crate) fn encode_key(sym: c_uint, wcode: c_uint, down: bool) -> [u8; 128] {
    let mut ev = [0u8; 128];
    ev[0..4].copy_from_slice(&if down { SDL_KEYDOWN } else { SDL_KEYUP }.to_ne_bytes());
    if cfg!(feature = "hostsim") {
        ev[12] = u8::from(down); // state
        ev[13] = 0; // repeat
        ev[20..24].copy_from_slice(&sym.to_ne_bytes());
        // The wcode rides `SDL_Keysym.mod` (event offset +24, a `Uint16`), under a marker in
        // `windowID` that says this press is synthetic at all. Several tokens carry ONLY a wcode
        // (`pause` is sym 0, wcode 72), so deriving it from the sym is not an option.
        // `decode_key`'s doc has the measurement behind both fields — the short version is that of
        // the spare places to put a value, these two are the ones sdl2-compat does not discard.
        ev[8..12].copy_from_slice(&SYNTH_WINDOW.to_ne_bytes());
        ev[24..26].copy_from_slice(&(wcode as u16).to_ne_bytes());
    } else {
        ev[16..20].copy_from_slice(&if down { 1u32 } else { 0 }.to_ne_bytes()); // state
        ev[20..24].copy_from_slice(&wcode.to_ne_bytes());
        ev[24..28].copy_from_slice(&sym.to_ne_bytes());
    }
    ev
}

/// The bytes a synthetic HARDWARE AUTO-REPEAT edge carries — `encode_key`'s down edge with the
/// 0x101 shape (`state & 0x100 != 0`) the loop's repeat arm requires, in whichever layout
/// `decode_key` reads. `encode_key` itself must never produce this (`key_bytes_round_trip` pins
/// that a synthetic EDGE "must never look like auto-repeat"), so it is a second, deliberately
/// separate function rather than a third argument threaded through the first — item 13's
/// `holdrep:<name>` FIFO token is the only caller, and it exists so a script can exercise the
/// `Edge::Repeat` a screen receives without a real remote's own repeat cadence.
pub(crate) fn encode_key_repeat(sym: c_uint, wcode: c_uint) -> [u8; 128] {
    let mut ev = encode_key(sym, wcode, true);
    if cfg!(feature = "hostsim") {
        ev[13] = 1; // the `repeat` byte `decode_key`'s hostsim arm folds into `state & 0x100`
    } else {
        let state = rd_u32(&ev, 16) | 0x100;
        ev[16..20].copy_from_slice(&state.to_ne_bytes());
    }
    ev
}

/// **One native key edge as the loop hands it to the tree**, for the effect-level guards (W5b): the
/// SDL bytes a television or the simulator delivers for the pair `(sym, wcode)` (the literals a
/// test writes down, never an `Action`'s `raw_pair`), decoded by [`decode_host_event`], and the
/// `InputEvent` that `run::ingest_key` pushes for that press (`bridge::key_input` of the press's
/// pair and its packed state). A consumer fed this event answers what it answers a real key.
#[cfg(test)]
pub(crate) fn native_key_input(sym: c_uint, wcode: c_uint, down: bool, at: plx_machine::machine::Tick)
    -> (KeyPress, plx_machine::machine::InputEvent<u32>) {
    let Some(HostEvent::Key(press)) = decode_host_event(&encode_key(sym, wcode, down)) else {
        panic!("SDL key bytes decode to a key press");
    };
    let RawKey { sym, wcode } = press.pair();
    (press, super::bridge::key_input(sym, wcode, press.state(), at, plx_machine::machine::Source::Sdl))
}

#[inline]
/// An SDL pointer event's position, converted from window pixels to the authored 1920x1080 canvas.
///
/// THE one place event coordinates enter the UI, so the conversion cannot be forgotten at a new
/// call site — there are nine, and patching them individually is how the tenth ends up wrong.
/// `surface::to_logical` is the identity while the drawable is 1920x1080, which it is 1:1 unless
/// a webOS 1-3 owner picks a smaller Interface resolution (or the debug drawable trigger asks for one).
pub(crate) fn ptr_xy(ev: &[u8]) -> (f32, f32) {
    plx_base::surface::to_logical(rd_i32(ev, 20) as f32, rd_i32(ev, 24) as f32)
}

pub(crate) fn rd_i32(ev: &[u8], off: usize) -> i32 {
    i32::from_ne_bytes([ev[off], ev[off + 1], ev[off + 2], ev[off + 3]])
}

pub(crate) fn rd_f32(ev: &[u8], off: usize) -> f32 {
    f32::from_ne_bytes([ev[off], ev[off + 1], ev[off + 2], ev[off + 3]])
}

/// Map a remote-control token (from the `crate::remote` FIFO) to the `(sym, wcode)` a
/// real Magic-Remote press would carry — the pair the ONE key handler already matches
/// (see `ui::consts`). Returns None for an unknown token. Kept deliberately small: the
/// core nav set + OK/BACK + the transport keys that testing needs.
///
/// **Plus one escape hatch, `k:<sym>,<wcode>`, which is the only way to press a key this map does
/// NOT name.** That is not a convenience: the whole point of LG checklist item 40 is what an
/// *unsupported* key does, and a named-token map can by construction never send one. It also
/// covers the keys that are bound but have no business getting a mnemonic — the digits, the
/// channel rocker's raw codes — and lets a device question be rehearsed against the simulator
/// first (`k:0,269` is HOME, `k:53,34` is the digit `5` exactly as the television spells it).
/// Both fields are DECIMAL and both are required, because a pair with one field guessed is the
/// bug class `decode_key` exists to prevent. `tools/keytable.py` drives its unsupported-key and
/// pager rows through this.
pub(crate) fn remote_token_key(tok: &str) -> Option<(c_uint, c_uint)> {
    if let Some(rest) = tok.strip_prefix("k:") {
        let (s, w) = rest.split_once(',')?;
        return Some((s.parse().ok()?, w.parse().ok()?));
    }
    // Every named token is an `Action` and its pair is `raw_pair`'s, the ONE place a pair is built
    // from a key's meaning (`ui::consts`), so what the FIFO presses and what a platform that sends
    // an action presses cannot drift. The channel rocker is the one named token that is not an
    // action's pair: it is the remote's CH key, a wcode alone, where `PageUp` is a keyboard's sym.
    let action = match tok {
        "up" => Action::Up,
        "down" => Action::Down,
        "left" => Action::Left,
        "right" => Action::Right,
        "ok" | "enter" | "select" => Action::Ok, // is_ok()
        "back" | "esc" => Action::Back,          // is_back()
        // The pager's two spellings, and they are two tokens rather than one pair carrying both: a
        // PAGE key is a keyboard's and arrives as a sym alone, the rocker is the remote's and
        // arrives as a wcode alone. The single pair they once shared was `(SDLK_PAGEUP, 33)` — a
        // shape no real press has, since 33 is the digit `4` (`ui::consts`, where they were
        // retired), so half of what it drove was never the pager answering the rocker at all.
        "pageup" => Action::PageUp,
        "pagedown" => Action::PageDown,
        "chup" => return Some((0, WCODE_CH_UP_KEY)),
        "chdown" => return Some((0, WCODE_CH_DOWN_KEY)),
        "play" => Action::Play,
        "pause" => Action::Pause,
        "stop" => Action::Stop,
        // The transport keys settled from LG's own scancode table (`ui::consts`' WCODE_REWIND doc).
        // Here for the same reason the edit keys below are: nothing else can press them headlessly.
        "ff" | "fastforward" => Action::FastForward,
        "rew" | "rewind" => Action::Rewind,
        "playpause" => Action::PlayPause,
        "exit" => Action::Exit,
        // The system keyboard's own two edit keys (`ui::consts`' doc has the protocol). They are
        // here because they are otherwise UNREACHABLE without a human at the panel: no trigger
        // raises the keyboard and `SDL_PushEvent` cannot carry a text event on the simulator, so
        // without these the only grader for backspace and Clear all is somebody's thumb.
        "backspace" | "del" => Action::Backspace,
        "clear" => Action::ClearAll,
        _ => return None,
    };
    Some(raw_pair(action))
}

/// **`focus:<id>`, `press:<id>` and `press:<id>@<subject>`**, the by-id tokens of the remote FIFO and
/// Lab Control: focus (or press) the element an accessibility id names, and not the one a key walk
/// would arrive at. `None` is "not a by-id token" (the token goes on to `dispatch_remote_token`);
/// `Some(Err(why))` is a by-id token that names nothing a recording could replay, refused here
/// with a finite reason that carries no token text.
///
/// The input is never acted on in this function or in the ingress: the dispatcher resolves it at
/// its ordered delivery position (`plx_ui::dispatch`, the by-id module), so every key ahead of it
/// has already acted. The press edges are the `ok` token's own pair, so a replayed press is the
/// key a remote OK is. Producers only are `devtriggers`-gated; the input type is not.
#[cfg(feature = "devtriggers")]
pub(crate) fn by_id_token(token: &str) -> Option<Result<plx_machine::machine::InputKind<u32>, &'static str>> {
    use plx_machine::machine::{InputKind, OkTap};
    let (press, rest) = if let Some(rest) = token.strip_prefix("press:") {
        (true, rest)
    } else if let Some(rest) = token.strip_prefix("focus:") {
        (false, rest)
    } else {
        return None;
    };
    // An id has no `@`, so the first one starts the subject.
    let (id, expect) = match rest.split_once('@') {
        Some((id, subject)) if press => (id, Some(subject)),
        Some(_) => return Some(Err("a focus token takes no subject")),
        None => (rest, None),
    };
    if plx_ui::a11y::A11yPath::parse_id(id).is_err() {
        return Some(Err("malformed accessibility id"));
    }
    if expect.is_some_and(|subject| !plx_ui::dispatch::by_id_subject_ok(subject)) {
        return Some(Err("malformed press subject"));
    }
    let tap = if press {
        let Some((sym, wcode)) = remote_token_key("ok") else { return Some(Err("no ok key")) };
        Some(OkTap { sym, wcode })
    } else {
        None
    };
    Some(Ok(InputKind::ById { id: id.into(), press: tap, expect: expect.map(Into::into) }))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RemotePointer {
    Click,
    Move,
    Down,
    Up,
}

/// FIFO and Lab Control share one pointer alphabet. Coordinates are authored integer pixels;
/// malformed/overflowing values are refused, and off-canvas values retain `ck:`'s edge clamp.
fn remote_token_pointer(tok: &str) -> Option<(RemotePointer, i32, i32)> {
    let (name, coords) = tok.split_once(':')?;
    let kind = match name {
        "ck" => RemotePointer::Click,
        "pm" => RemotePointer::Move,
        "pd" => RemotePointer::Down,
        "pu" => RemotePointer::Up,
        _ => return None,
    };
    let (x, y) = coords.split_once(',')?;
    Some((kind,
        x.parse::<i32>().ok()?.clamp(0, plx_base::surface::LOGICAL_W as i32 - 1),
        y.parse::<i32>().ok()?.clamp(0, plx_base::surface::LOGICAL_H as i32 - 1)))
}

/// SDL pointer bytes shared by clicks, held-pointer FIFO edges and recorded-event replay.
/// The inverse of `ptr_xy`: convert authored coordinates exactly once, including window scaling
/// and letterboxing. Replay may carry a real off-canvas motion, so only the token parser clamps.
/// ABI: SDL's `SDL_events.h` (stock and the NDK's) mouse-event declarations put four
/// Uint32 fields before button@16/state@17 and Sint32 x@20/y@24. ARM32 NDK syntax-only
/// `offsetof` assertions verified these offsets; see `docs/testing-player-pointer.md`.
fn encode_pointer(et: u32, x: i32, y: i32) -> [u8; 128] {
    let mut ev = [0u8; 128];
    let (px, py) = plx_base::surface::to_physical(x as f32, y as f32);
    ev[0..4].copy_from_slice(&et.to_ne_bytes());
    ev[20..24].copy_from_slice(&(px.round() as i32).to_ne_bytes());
    ev[24..28].copy_from_slice(&(py.round() as i32).to_ne_bytes());
    if matches!(et, SDL_MOUSEBUTTONDOWN | SDL_MOUSEBUTTONUP) {
        ev[16] = 1; // SDL_BUTTON_LEFT
        ev[17] = u8::from(et == SDL_MOUSEBUTTONDOWN);
    }
    ev
}

/// Synthesize a Magic-Remote pointer click at authored 1920x1080 coords (the browser
/// remote's click-on-the-stream): two motion events, then button down+up. The first
/// motion is a >=120px jitter so the accumulated pointer distance defeats the
/// D-pad-mode pointer gate (`Pointer::mot_accum < 120` swallows small motions after D-pad use);
/// the second lands on the target. The LG SDL fork's mouse events carry x@20 / y@24
/// (i32) — the only fields the handlers read.
///
/// The browser still sends clicks only: forwarding hover moved app focus on every pass of the
/// mouse over the streamed picture. A driver can explicitly send `pm:`/`pd:`/`pu:` instead to
/// separate HUD reveal from a press or to hold a drag across frames. Those edges add no jitter.
pub(crate) fn remote_synth_ptr(x: i32, y: i32) {
    let jx = if x >= 200 { x - 200 } else { x + 200 };
    remote_synth_pointer(SDL_MOUSEMOTION, jx, y);
    remote_synth_pointer(SDL_MOUSEMOTION, x, y);
    remote_synth_pointer(SDL_MOUSEBUTTONDOWN, x, y);
    remote_synth_pointer(SDL_MOUSEBUTTONUP, x, y);
}

/// ONE pointer event in authored coordinates, shared by the FIFO and replay. Unlike `ck:`,
/// these primitives add no motion or release: `pd:` stays held until a separate `pu:` arrives.
pub(crate) fn remote_synth_pointer(et: u32, x: i32, y: i32) {
    let ev = encode_pointer(et, x, y);
    unsafe { SDL_PushEvent(ev.as_ptr() as *const c_void) };
}

/// One app lifecycle event (`0x103`–`0x106`), as the compositor would send it — the replay
/// driver's re-injection of a recorded background/foreground edge.
pub(crate) fn remote_synth_lifecycle(code: u32) {
    let mut ev = [0u8; 128];
    ev[0..4].copy_from_slice(&code.to_ne_bytes());
    unsafe { SDL_PushEvent(ev.as_ptr() as *const c_void) };
}

/// A remote token that acts DIRECTLY rather than by pushing SDL events — the recorder writes
/// these as `token` records, and the replay driver re-dispatches them. It is one of four fates:
/// the SDL-synthesised kinds (a key, a click) are recorded where they are polled; a direct token
/// (this predicate) is recorded as a `token` row; `focus:<id>` / `press:<id>[@<subject>]`
/// (`by_id_token`) become one typed `InputKind::ById` input that the tap records, not a token
/// row; and `state:<nonce>` is not recorded at all. `txt:` is here because its payload never
/// becomes an SDL event on this host (`dispatch_remote_token`'s arm says why).
pub(crate) fn token_is_direct(tok: &str) -> bool {
    #[cfg(feature = "devtriggers")]
    if crate::remote::HangProbe::parse(tok).is_some() {
        return true;
    }
    tok == "shot"
        || tok == "diag"
        || tok == "diagnostics"
        || tok.starts_with("pat:")
        || tok.starts_with("txt:")
}

/// Synthesize a full remote-key press (key-down then key-up) and push both onto SDL's
/// own event queue, so the existing poll loop consumes them as if they came off the
/// wayland input path. The LG SDL fork's `SDL_KeyboardEvent` carries state@16 /
/// wcode@20 / sym@24 (native-endian; the TV is LE), and the handler reads press vs
/// release from `state & 0xff` — so the down carries state=1, the up state=0. Both
/// are required: a grid-card OK arms on down and *commits on release*.
pub(crate) fn remote_synth_key(sym: c_uint, wcode: c_uint) {
    remote_synth_key_edge(sym, wcode, true);
    remote_synth_key_edge(sym, wcode, false);
}

/// ONE edge of a remote key press. Split out for the `okdown`/`okup` FIFO tokens, because a
/// **press-and-hold** is only expressible as two tokens with real time between them: the item menu
/// opens on `press::is_long`, which measures the interval between the down and the up. The paired
/// `remote_synth_key` above is this called twice back to back (a tap).
pub(crate) fn remote_synth_key_edge(sym: c_uint, wcode: c_uint, down: bool) {
    let ev = encode_key(sym, wcode, down);
    unsafe { SDL_PushEvent(ev.as_ptr() as *const c_void) };
}

/// ONE hardware auto-repeat edge — item 13's `holdrep:<name>` FIFO token, which lets a script
/// exercise a held key's `Edge::Repeat` (the Settings family's paced focus walk, the player
/// scrubber's continuous scrub) without a real remote's own repeat cadence. Only recognised as a
/// repeat by the loop's key arm when `App::down_sym` already equals `sym` — i.e. after a
/// `holddown:<name>` and before its matching `holdup:<name>`, the same split `okdown`/`okup`
/// already uses for a press-and-hold.
pub(crate) fn remote_synth_key_repeat(sym: c_uint, wcode: c_uint) {
    let ev = encode_key_repeat(sym, wcode);
    unsafe { SDL_PushEvent(ev.as_ptr() as *const c_void) };
}

/// Synthesize one Magic-Remote scroll-wheel tick — item 13's `wheel:<dy>` FIFO token, so a script
/// can drive the wheel with no mouse in the room. Encoded in the plain, real-SDL2 shape
/// (`Sint32 y` at `+20`) unconditionally: it is the READING side that has to branch by platform
/// now, not this one — see the wheel arm's own comment on why `+20` decodes to 0 on this host and
/// where the value actually lands after `SDL_PushEvent` round-trips it.
pub(crate) fn remote_synth_wheel(dy: i32) {
    let mut ev = [0u8; 128];
    ev[0..4].copy_from_slice(&SDL_MOUSEWHEEL.to_ne_bytes());
    ev[20..24].copy_from_slice(&dy.to_ne_bytes());
    unsafe { SDL_PushEvent(ev.as_ptr() as *const c_void) };
}

/// Is this SDL event type INPUT — a key, text, pointer or wheel event — as opposed to a lifecycle,
/// window or quit event? The one classification `Dispatcher::input_scope` rests on: input under
/// an open modal is the modal's, a lifecycle event is the app's and may change the page beneath.
pub(crate) fn is_input_event(et: u32) -> bool {
    matches!(
        et,
        SDL_KEYDOWN
            | SDL_KEYUP
            | SDL_TEXTINPUT
            | SDL_TEXTEDITING
            | SDL_MOUSEMOTION
            | SDL_MOUSEBUTTONDOWN
            | SDL_MOUSEBUTTONUP
            | SDL_MOUSEWHEEL
    )
}

#[cfg(test)]
mod input_event_tests {
    use super::*;

    #[test]
    fn pointer_tokens_validate_coordinates_and_keep_click_edge_clamping() {
        for (prefix, kind) in [("ck", RemotePointer::Click), ("pm", RemotePointer::Move),
            ("pd", RemotePointer::Down), ("pu", RemotePointer::Up)] {
            assert_eq!(remote_token_pointer(&format!("{prefix}:1400,870")), Some((kind, 1400, 870)));
            assert_eq!(remote_token_pointer(&format!("{prefix}:-2147483648,2147483647")),
                Some((kind, 0, 1079)));
            for invalid in ["", "1", "1,", ",1", "1,2,3", "NaN,1", "1.5,2", "2147483648,2"] {
                assert_eq!(remote_token_pointer(&format!("{prefix}:{invalid}")), None);
            }
        }
        assert_eq!(remote_token_pointer("pointer:1,2"), None);
    }

    #[test]
    fn pointer_bytes_round_trip_through_the_common_coordinate_decoder() {
        let _serial = plx_base::testlock::serial();
        for et in [SDL_MOUSEMOTION, SDL_MOUSEBUTTONDOWN, SDL_MOUSEBUTTONUP] {
            for (x, y) in [(0, 0), (1400, 870), (1919, 1079), (-200, 400)] {
                let ev = encode_pointer(et, x, y);
                assert_eq!(rd_u32(&ev, 0), et);
                assert_eq!(ptr_xy(&ev), (x as f32, y as f32));
                if et != SDL_MOUSEMOTION {
                    assert_eq!(ev[16], 1, "the left button survives SDL's event queue");
                    assert_eq!(ev[17], u8::from(et == SDL_MOUSEBUTTONDOWN));
                }
            }
        }
    }

    #[cfg(feature = "devtriggers")]
    #[test]
    fn hang_probes_are_direct_recorder_tokens() {
        for token in ["hang:1", "hang-raw:1", "hang:5001"] {
            assert!(token_is_direct(token));
        }
        for token in ["down", "hang:", "hang:-1", "hang-raw:bad"] {
            assert!(!token_is_direct(token));
        }
    }

    /// **The by-id tokens parse to the one input the dispatcher resolves**: the id, the `ok` token's
    /// own key pair for a press, an optional subject, and a refusal (never a guess) for anything a
    /// recording could not replay.
    #[cfg(feature = "devtriggers")]
    #[test]
    fn by_id_tokens_parse_to_one_input_and_refuse_what_cannot_replay() {
        use plx_machine::machine::InputKind;
        let ok = remote_token_key("ok").expect("the ok token");
        let parsed = |token: &str| by_id_token(token).expect("a by-id token").map(|kind| match kind {
            InputKind::ById { id, press, expect } =>
                (id.to_string(), press.map(|tap| (tap.sym, tap.wcode)), expect.map(|s| s.to_string())),
            other => panic!("{other:?}"),
        });
        assert_eq!(parsed("focus:home.row.2"), Ok(("home.row.2".into(), None, None)));
        assert_eq!(parsed("press:detail.row.0"), Ok(("detail.row.0".into(), Some(ok), None)));
        assert_eq!(parsed("press:nav.tab.1@rk77"), Ok(("nav.tab.1".into(), Some(ok), Some("rk77".into()))));
        for token in ["focus:", "press:", "focus:Home.row.1", "focus:home", "focus:home..row", "press:home.row.01",
            "focus:home.row.1@rk1", "press:home.row.1@", "press:home.row.1@a b", "press:home.row.1@a@b"] {
            assert!(by_id_token(token).is_some_and(|parsed| parsed.is_err()), "{token}");
        }
        for token in ["ok", "down", "focus", "press", "pat:x", "txt:a", "k:13,0", "shot"] {
            assert!(by_id_token(token).is_none(), "{token} is not a by-id token");
        }
        // a by-id input is recorded as an input at its tap, never as a direct recorder token
        assert!(!token_is_direct("focus:home.row.2") && !token_is_direct("press:home.row.2"));
    }

    /// The boundary `Dispatcher::input_scope` rests on: every input kind is in, and the
    /// lifecycle events (background/foreground, `0x103`–`0x106`), window events and quit are out.
    #[test]
    fn input_events_are_the_modals_and_lifecycle_events_are_the_apps() {
        for et in [
            SDL_KEYDOWN,
            SDL_KEYUP,
            SDL_TEXTINPUT,
            SDL_TEXTEDITING,
            SDL_MOUSEMOTION,
            SDL_MOUSEBUTTONDOWN,
            SDL_MOUSEBUTTONUP,
            SDL_MOUSEWHEEL,
        ] {
            assert!(is_input_event(et), "{et:#x} is input");
        }
        for et in [SDL_QUIT, 0x101, 0x103, 0x104, 0x105, 0x106, 0x200] {
            assert!(!is_input_event(et), "{et:#x} is the app's, not a modal's");
        }
    }
}

/// Dispatch one synthetic-input token. Shared by the SSH-only development FIFO and Lab Control's
/// outbound HTTPS command channel, so a cloud command cannot grow a second interpretation of
/// `down`, raw key pairs, pointer coordinates or text input beside the one the harness uses.
///
/// `true` means the token was accepted and injected/requested, not that the screen necessarily
/// changed — pressing DOWN at the bottom of a list is still a successfully delivered command.
pub(crate) fn dispatch_remote_token(tok: &str, ps: &plx_media::route::PlaybackSession,
    pa: &plx_media::player::adapter::PlayerAdapter, present: &mut plx_machine::present::Present,
    scope: plx_machine::present::Scope) -> bool {
    let scope = if tok.starts_with("pat:") { plx_machine::present::Scope::Page } else { scope };
    present.note_in_scope(scope, plx_machine::present::PresentEvent::Damage(plx_machine::present::Provenance::Input));
    if let Some((kind, x, y)) = remote_token_pointer(tok) {
        let name = match kind {
            RemotePointer::Click => "click",
            RemotePointer::Move => "pointer move",
            RemotePointer::Down => "pointer down",
            RemotePointer::Up => "pointer up",
        };
        log(&format!("remote: {name} {x},{y}"));
        match kind {
            RemotePointer::Click => remote_synth_ptr(x, y),
            RemotePointer::Move => remote_synth_pointer(SDL_MOUSEMOTION, x, y),
            RemotePointer::Down => remote_synth_pointer(SDL_MOUSEBUTTONDOWN, x, y),
            RemotePointer::Up => remote_synth_pointer(SDL_MOUSEBUTTONUP, x, y),
        }
        true
    } else if cfg!(feature = "hostsim") && tok == "shot" {
        // Simulator only. Screenshotting has to be a TOKEN rather than a launch option, because
        // the interesting frame is the one AFTER driving, and `PLXNATIVE_SHOT_FRAME` is fixed
        // before the app starts — worse, presented frames only accrue when something repaints
        // (the idle gate), so no frame number can be predicted from outside. This makes
        // `down down right ok shot` a single composable line.
        #[cfg(feature = "hostsim")]
        crate::shot::request();
        true
    } else if tok == "okdown" || tok == "okup" {
        // The two halves of OK let a driver hold it past press::LONG_MS and reach the item menu.
        remote_synth_key_edge(SDLK_RETURN, 0, tok == "okdown");
        true
    } else if let Some(spec) = tok.strip_prefix("wheel:") {
        // Item 13: `wheel:<dy>` drives Settings/Consent/Legal's wheel arm (and every other route's)
        // without a mouse in the room.
        match spec.parse::<i32>() {
            Ok(dy) => {
                remote_synth_wheel(dy);
                true
            }
            Err(_) => false,
        }
    } else if let Some(name) = tok.strip_prefix("holddown:") {
        // Item 13's press-and-hold triple, generalising `okdown`/`okup` to any named key so a
        // script can drive a genuine long-press and its hardware auto-repeats with no device:
        // `holddown:<name>` (physical press, arms `App::down_sym`), `holdrep:<name>` (one
        // 0x101 repeat edge, as many times as the script wants), `holdup:<name>` (release).
        match remote_token_key(name) {
            Some((sym, wcode)) => {
                remote_synth_key_edge(sym, wcode, true);
                true
            }
            None => false,
        }
    } else if let Some(name) = tok.strip_prefix("holdrep:") {
        match remote_token_key(name) {
            Some((sym, wcode)) => {
                remote_synth_key_repeat(sym, wcode);
                true
            }
            None => false,
        }
    } else if let Some(name) = tok.strip_prefix("holdup:") {
        match remote_token_key(name) {
            Some((sym, wcode)) => {
                remote_synth_key_edge(sym, wcode, false);
                true
            }
            None => false,
        }
    } else if tok == "diag" || tok == "diagnostics" {
        if plx_os::labcfg::menu_row_enabled() {
            crate::lab::request_upload("command", ps, pa);
            true
        } else {
            false
        }
    } else if let Some(spec) = tok.strip_prefix("pat:") {
        // `pat:flat:40` — swap the synthetic ground live for a one-session graded sweep.
        let ok = plx_ui::testpat::set(spec);
        if !ok {
            plx_base::eventlog::log(&format!("remote: unrecognised pattern {spec:?}"));
        }
        ok
    } else if let Some(text) = tok.strip_prefix("txt:") {
        // `txt:star+wars` — commit text as the system keyboard's IME would. `+` stands for a
        // space because the FIFO protocol is whitespace-delimited. Handed directly to textinput:
        // pushing a synthetic SDL_TEXTINPUT crashes sdl2-compat because SDL2 and SDL3 disagree
        // about whether that payload is inline bytes or a pointer (the full measurement is in the
        // original FIFO call-site history and `textinput.rs`).
        let ev = crate::textinput::encode_event(&text.replace('+', " "));
        crate::textinput::on_event(&ev);
        log(&format!(
            "txt: decoded {:?} pending={}",
            crate::textinput::decode(&ev),
            crate::textinput::pending()
        ));
        true
    } else if let Some((sym, wcode)) = remote_token_key(tok) {
        remote_synth_key(sym, wcode);
        true
    } else {
        log(&format!("remote: unknown token {tok:?}"));
        false
    }
}

#[cfg(test)]
mod key_layout_tests {
    use super::{decode_key, encode_key, encode_key_repeat};
    use plx_ui::consts::{SDLK_DOWN, SDLK_RETURN, WCODE_BACK, WCODE_PAUSE};

    /// `encode_key` and `decode_key` must agree, in whichever layout this build compiled.
    ///
    /// This is the regression test for a bug that shipped: the two ends disagreed about
    /// `SDL_KeyboardEvent`'s field offsets, so every remote-FIFO token was accepted, decoded into
    /// nonsense, and silently dropped — no error on either side. Nothing in the compiler couples a
    /// reader and a writer of raw byte offsets, so this does.
    ///
    /// `make check` builds the television layout, so that is the one graded by default; a
    /// `--features hostsim` test run grades the stock-SDL2 one. Both arms are compiled either way
    /// (they are `cfg!`, not `#[cfg]`), so neither can rot.
    #[test]
    fn key_bytes_round_trip() {
        // The wcode-only case is the one that breaks a sym-derived mapping, and the one a naive
        // host layout loses: `pause` carries no sym at all.
        //
        // **`(8, 42)` is the case that MATTERS and it was missing.** It is the `backspace` token,
        // and 8 is one of the four syms `host_wcode` maps a desktop key onto — to `WCODE_BACK`.
        // The only sym-plus-wcode case here used to be `(8, WCODE_BACK)`, the single pair where
        // the stand-in and the carrier agree, so a decode that consulted the stand-in FIRST passed
        // this test while turning the panel's delete key into a navigation. Every one of those
        // four syms belongs here for the same reason.
        for (sym, wcode) in [
            (SDLK_DOWN, 0),
            (SDLK_RETURN, 0),
            (0, WCODE_PAUSE),
            (8, WCODE_BACK),
            (8, 42),   // backspace: sym 8, SDL_SCANCODE_BACKSPACE — NOT BACK
            (32, 44),  // space, 'p', 's': the other three syms the stand-in claims, each
            (112, 19), // beside its own real scancode, which must survive unchanged
            (115, 22),
        ] {
            for down in [true, false] {
                let ev = encode_key(sym, wcode, down);
                let (state, got_wcode, got_sym) = decode_key(&ev);
                assert_eq!(got_sym, sym, "sym lost (wcode={wcode}, down={down})");
                assert_eq!(got_wcode, wcode, "wcode lost (sym={sym}, down={down})");
                assert_eq!(
                    state & 0xff,
                    u32::from(down),
                    "press/release lost — the low byte is what every handler tests (sym={sym})"
                );
                assert_eq!(
                    state & 0x100,
                    0,
                    "a synthetic edge must never look like auto-repeat"
                );
            }
        }
    }

    /// `encode_key_repeat`'s twin of the round trip above: a `holdrep:<name>` token must decode as
    /// a genuine hardware auto-repeat (`state & 0x100 != 0`), the exact shape `on_auto_repeat`'s
    /// caller gates on (`state & 0x100 != 0 && sym == held_key.down_sym`) — the one case
    /// `key_bytes_round_trip` just pinned an ordinary edge must NEVER produce.
    #[test]
    fn encode_key_repeat_round_trips_as_a_hardware_repeat() {
        for (sym, wcode) in [(SDLK_DOWN, 0), (0, WCODE_PAUSE), (8, 42)] {
            let ev = encode_key_repeat(sym, wcode);
            let (state, got_wcode, got_sym) = decode_key(&ev);
            assert_eq!(got_sym, sym, "sym lost (wcode={wcode})");
            assert_eq!(got_wcode, wcode, "wcode lost (sym={sym})");
            assert_eq!(state & 0xff, 1, "a repeat is a DOWN edge, not a release");
            assert_eq!(
                state & 0x100,
                0x100,
                "must decode as auto-repeat, or `on_auto_repeat` never sees it (sym={sym})"
            );
        }
    }

    /// **`k:<sym>,<wcode>` — the only token that can press a key the map does NOT name**, which is
    /// what LG checklist item 40 needs: a named-token map can by construction never send an
    /// unsupported key. Both fields are required and decimal; a half-parsed pair must be REFUSED
    /// rather than silently become a press of something else, because the drain's `else` logs an
    /// unknown token and a wrong pair would log nothing at all.
    #[test]
    fn the_raw_key_token_carries_both_fields_or_none() {
        use super::remote_token_key;
        assert_eq!(
            remote_token_key("k:0,269"),
            Some((0, 269)),
            "HOME, which nothing else can send"
        );
        assert_eq!(
            remote_token_key("k:53,34"),
            Some((53, 34)),
            "the digit 5 as the TV spells it"
        );
        for bad in [
            "k:", "k:1", "k:1,", "k:,1", "k:a,1", "k:1,b", "k:1,2,3", "k:-1,2", "k: 1,2",
        ] {
            assert_eq!(
                remote_token_key(bad),
                None,
                "{bad:?} must not become a keypress"
            );
        }
        // …and it must not shadow the named tokens or the other prefixed ones.
        assert!(
            remote_token_key("ck:10,20").is_none(),
            "a click token is not a key token"
        );
        assert_eq!(
            remote_token_key("chup"),
            Some((0, plx_ui::consts::WCODE_CH_UP_KEY))
        );
        assert_eq!(
            remote_token_key("pageup"),
            Some((plx_ui::consts::SDLK_PAGEUP, 0))
        );
    }
}

/// SDL's quit and application events, decoded into the `HostEvent` the loop consumes (W5a). The SDL
/// codes are literals: the table is the contract with SDL and with the tape, so it is not read back
/// from the mapping it grades.
#[cfg(test)]
mod host_event_tests {
    use super::{decode_host_event, is_input_event, HostEvent, Lifecycle};

    fn event_of(code: u32) -> [u8; 128] {
        let mut ev = [0u8; 128];
        ev[..4].copy_from_slice(&code.to_ne_bytes());
        ev
    }

    #[test]
    fn quit_and_the_four_lifecycle_types_decode_and_the_lifecycle_codes_round_trip() {
        for (code, event) in [
            (0x100, HostEvent::Quit),
            (0x103, HostEvent::Lifecycle(Lifecycle::WillBackground)),
            (0x104, HostEvent::Lifecycle(Lifecycle::DidBackground)),
            (0x105, HostEvent::Lifecycle(Lifecycle::WillForeground)),
            (0x106, HostEvent::Lifecycle(Lifecycle::DidForeground)),
        ] {
            assert_eq!(decode_host_event(&event_of(code)), Some(event.clone()), "{code:#x}");
            assert!(!is_input_event(code), "{code:#x}: a host event is the app's, never a modal's input");
            if let HostEvent::Lifecycle(edge) = event {
                assert_eq!(edge.sdl_code(), code, "the code a decoded edge is read back as");
            }
        }
    }

    /// Everything the loop still reads raw stays raw: no pointer or wheel event decodes (0x400 to
    /// 0x403, until W5c), nor the IME's pre-edit (0x302, which nothing reads), nor the SDL events
    /// nothing acts on (terminating and low memory, which sit between quit and the background pair,
    /// and the window events). Keys (0x300, 0x301) and text (0x303) are decoded: `key_event_tests`.
    #[test]
    fn every_other_event_stays_on_the_raw_path() {
        for code in [
            0, 0x101, 0x102, 0x107, 0x200, 0x302, 0x400, 0x401, 0x402, 0x403, u32::MAX,
        ] {
            assert_eq!(decode_host_event(&event_of(code)), None, "{code:#x}");
        }
    }
}

/// A key edge and a text commit as a [`HostEvent`] (W5b): what the SDL decoder makes of the bytes,
/// the pairs the FIFO's named tokens press, and the one guarantee the constructor gives.
#[cfg(test)]
mod key_event_tests {
    use super::{
        decode_host_event, encode_key, encode_key_repeat, remote_token_key, Action, Edge, HostEvent,
        KeyPress, RawKey,
    };
    use plx_ui::consts::{raw_pair, SDLK_CLEAR, SDLK_DOWN, SDLK_RETURN, SDLK_UP, WCODE_BACK, WCODE_PLAY};

    fn press(action: Option<Action>, edge: Edge, sym: u32, wcode: u32) -> HostEvent {
        HostEvent::Key(KeyPress::new(action, edge, Some(RawKey { sym, wcode })).expect("a raw pair names a press"))
    }

    /// An SDL key event decodes to its edge, its raw pair (always kept) and its action (`None` for a
    /// press the app does not bind), for the layout this build reads. The press is built with
    /// `encode_key`/`encode_key_repeat`, so this is the round trip; the byte-literal test below pins
    /// the layout itself.
    #[test]
    fn an_sdl_key_decodes_to_its_edge_its_raw_pair_and_its_action() {
        let table: [(u32, u32, Option<Action>); 9] = [
            (SDLK_UP, 0, Some(Action::Up)),
            (SDLK_RETURN, 0, Some(Action::Ok)),
            (0, WCODE_BACK, Some(Action::Back)),
            (0, WCODE_PLAY, Some(Action::Play)),
            (b'4' as u32, 33, Some(Action::Digit(4))),
            (8, 42, Some(Action::Backspace)),
            (SDLK_CLEAR, 156, Some(Action::ClearAll)),
            (0, 504, None),
            (0, 0, None),
        ];
        for (sym, wcode, action) in table {
            assert_eq!(decode_host_event(&encode_key(sym, wcode, true)), Some(press(action, Edge::Down, sym, wcode)), "({sym},{wcode}) down");
            assert_eq!(decode_host_event(&encode_key(sym, wcode, false)), Some(press(action, Edge::Up, sym, wcode)), "({sym},{wcode}) up");
            assert_eq!(decode_host_event(&encode_key_repeat(sym, wcode)), Some(press(action, Edge::Repeat, sym, wcode)), "({sym},{wcode}) repeat");
        }
    }

    /// The layout, byte for byte and written out a second time rather than through `encode_key`.
    /// LG's fork: `state` u32 at +16, wcode at +20, sym at +24. Stock SDL (the simulator): `state` u8
    /// at +12, `repeat` u8 at +13, sym at +20, and a synthetic press carries its wcode in `mod` at
    /// +24 under the `SYNTH_WINDOW` marker at +8 (`decode_key`'s doc has the measurement).
    #[test]
    fn the_key_layout_is_pinned_byte_for_byte() {
        // UP (0x40000052) with no wcode, and PLAY (wcode 450 = 0x1c2) with no sym.
        let (up_sym, play_wcode) = ([0x52, 0x00, 0x00, 0x40], [0xc2, 0x01, 0x00, 0x00]);
        let mut up = [0u8; 128];
        let mut play = [0u8; 128];
        let mut repeat = [0u8; 128];
        for ev in [&mut up, &mut play, &mut repeat] {
            ev[0..4].copy_from_slice(&0x300u32.to_ne_bytes());
        }
        if cfg!(feature = "hostsim") {
            for ev in [&mut up, &mut play, &mut repeat] {
                ev[12] = 1;
                ev[8..12].copy_from_slice(&0x504c_584bu32.to_ne_bytes());
            }
            up[20..24].copy_from_slice(&up_sym);
            play[24..26].copy_from_slice(&450u16.to_ne_bytes());
            repeat[20..24].copy_from_slice(&up_sym);
            repeat[13] = 1;
        } else {
            for ev in [&mut up, &mut play, &mut repeat] {
                ev[16] = 1;
            }
            up[24..28].copy_from_slice(&up_sym);
            play[20..24].copy_from_slice(&play_wcode);
            repeat[24..28].copy_from_slice(&up_sym);
            repeat[17] = 1; // state 0x101
        }
        assert_eq!(decode_host_event(&up), Some(press(Some(Action::Up), Edge::Down, SDLK_UP, 0)));
        assert_eq!(decode_host_event(&play), Some(press(Some(Action::Play), Edge::Down, 0, 450)));
        assert_eq!(decode_host_event(&repeat), Some(press(Some(Action::Up), Edge::Repeat, SDLK_UP, 0)));
    }

    /// A release never carries the auto-repeat bit into the event: `Edge::Up` is `Edge::Up`, and the
    /// tape's `repeat` for it is false (the SDL drivers only stamp 0x100 on a press).
    #[test]
    fn a_release_is_an_up_edge_whatever_else_is_stamped() {
        let mut ev = encode_key(SDLK_DOWN, 0, false);
        if cfg!(feature = "hostsim") {
            ev[13] = 1;
        } else {
            ev[17] = 1;
        }
        let Some(HostEvent::Key(key)) = decode_host_event(&ev) else { panic!("a key event") };
        assert_eq!(key.state(), 0);
        assert_eq!(key, KeyPress::new(Some(Action::Down), Edge::Up, Some(RawKey { sym: SDLK_DOWN, wcode: 0 })).unwrap());
    }

    /// The constructor's guarantee: a press names an action or a raw pair, and an action-only press
    /// reads as the pair `raw_pair` gives it, which is what a remote's press of that key would be.
    /// A raw pair that came with an action keeps being the pair read.
    #[test]
    fn a_press_names_an_action_or_a_raw_pair_and_reads_back_as_a_pair() {
        assert!(KeyPress::new(None, Edge::Down, None).is_none(), "a press that names nothing is refused");
        for action in [Action::Up, Action::Pause, Action::Rewind, Action::PageDown, Action::Backspace, Action::Digit(3)] {
            let only = KeyPress::new(Some(action), Edge::Down, None).unwrap();
            let (sym, wcode) = raw_pair(action);
            assert_eq!(only.pair(), RawKey { sym, wcode }, "{action:?}");
            assert_eq!(only.action(), Some(action));
        }
        let both = KeyPress::new(Some(Action::Back), Edge::Down, Some(RawKey { sym: 8, wcode: 482 })).unwrap();
        assert_eq!(both.pair(), RawKey { sym: 8, wcode: 482 }, "the simulator's desktop Backspace stays the pair it was");
        let raw_only = KeyPress::new(None, Edge::Repeat, Some(RawKey { sym: 0, wcode: 504 })).unwrap();
        assert_eq!((raw_only.action(), raw_only.pair()), (None, RawKey { sym: 0, wcode: 504 }));
        assert_eq!(
            [Edge::Down, Edge::Repeat, Edge::Up]
                .map(|edge| KeyPress::new(Some(Action::Ok), edge, None).unwrap().state()),
            [1, 0x101, 0]
        );
    }

    /// **A press is one key: its action and its pair cannot disagree.** An action alone must read
    /// back from its own `raw_pair` (`Digit(10)`, which `raw_pair` spells as `(0, 0)`, does not and
    /// is refused, whatever else rides beside it); a stated action must be what `action_of` reads
    /// out of the pair it comes with (the pairs below are real keys, each beside another key's
    /// action); and a pair with no stated action has the action `action_of` reads out of it,
    /// `PointerHidden` for the LG remote's pointer-hide wcode included.
    #[test]
    fn a_press_is_one_key_and_its_action_and_pair_cannot_disagree() {
        for n in [10u8, 11, 100, 255] {
            assert!(KeyPress::new(Some(Action::Digit(n)), Edge::Down, None).is_none(), "Digit({n}) names no key");
            assert!(KeyPress::new(Some(Action::Digit(n)), Edge::Down, Some(RawKey { sym: 0, wcode: 0 })).is_none(),
                "Digit({n}) is no action of the unbound pair (0, 0)");
        }
        assert!(KeyPress::new(Some(Action::Digit(9)), Edge::Down, None).is_some(), "9 is a number key");
        assert_eq!(Action::digit(10), None);
        for (action, sym, wcode, why) in [
            (Action::Play, 0, 72, "PAUSE's wcode"),
            (Action::Pause, 0, 450, "PLAY's wcode"),
            (Action::PageUp, 0x4000_004e, 0, "PAGE DOWN's sym"),
            (Action::PageDown, 0x4000_004b, 0, "PAGE UP's sym"),
            (Action::Left, 0, 452, "REWIND, which is not a plain LEFT"),
            (Action::Rewind, 0x4000_0050, 0, "LEFT, which is not REWIND"),
            (Action::Digit(4), b'5' as u32, 0, "the digit 5"),
            (Action::Ok, 0, 504, "an unbound colour button"),
            (Action::PointerHidden, 0, 504, "an unbound colour button"),
        ] {
            assert!(KeyPress::new(Some(action), Edge::Down, Some(RawKey { sym, wcode })).is_none(),
                "{action:?} beside {why} is two keys");
        }
        // Agreeing, in either spelling, is accepted: the simulator's desktop Backspace carries BACK's wcode.
        assert!(KeyPress::new(Some(Action::Back), Edge::Down, Some(RawKey { sym: 8, wcode: 482 })).is_some());
        assert!(KeyPress::new(Some(Action::Play), Edge::Down, Some(RawKey { sym: 0, wcode: 450 })).is_some());
        // A pair with no stated action has the action it reads as, so a raw-only press is the press an SDL event is.
        for (sym, wcode, action) in [
            (0, 0x1e4, Some(Action::PointerHidden)),
            (0, 450, Some(Action::Play)),
            (0x4000_004b, 0, Some(Action::PageUp)),
            (0, 300, Some(Action::PageUp)),
            (0, 504, None),
        ] {
            let press = KeyPress::new(None, Edge::Down, Some(RawKey { sym, wcode })).unwrap();
            assert_eq!((press.action(), press.pair()), (action, RawKey { sym, wcode }), "({sym}, {wcode})");
        }
    }

    /// **A native key decodes to the action it means.** Every pair here is what a television or the
    /// simulator sends, written out as a LITERAL (no `raw_pair`, no constant), so a wrong `action_of`
    /// reading cannot agree with itself: PLAY is not PAUSE, REWIND is not a plain LEFT, PAGE UP and
    /// CH+ are not PAGE DOWN and CH-, the number keys read from the sym alone, and a wcode-only
    /// number press is no action (the PIN pad reads it through `pin_digit` instead). No production
    /// reader of the loop reads the action before W5d, so nothing but this table sees `action_of` go
    /// wrong today; `native_key_effect_*` in `run.rs` grade what the consumers do with the pair.
    #[test]
    fn native_key_effect_the_native_pairs_decode_to_the_actions_they_mean() {
        let table: &[(u32, u32, Option<Action>)] = &[
            (0x4000_0052, 0, Some(Action::Up)),
            (0x4000_0051, 0, Some(Action::Down)),
            (0x4000_0050, 0, Some(Action::Left)),
            (0x4000_004f, 0, Some(Action::Right)),
            (13, 0, Some(Action::Ok)),
            (0, 482, Some(Action::Back)),
            (27, 0, Some(Action::Back)),
            (8, 482, Some(Action::Back)),
            (0, 450, Some(Action::Play)),
            (0, 72, Some(Action::Pause)),
            (0, 261, Some(Action::PlayPause)),
            (415, 0, Some(Action::Pause)),
            (0, 415, Some(Action::Pause)),
            (19, 0, Some(Action::Play)),
            (0, 402, Some(Action::Play)),
            (0, 413, Some(Action::Stop)),
            (0, 505, Some(Action::Exit)),
            (0, 452, Some(Action::Rewind)),
            (0, 451, Some(Action::FastForward)),
            (0x4000_004b, 0, Some(Action::PageUp)),
            (0, 300, Some(Action::PageUp)),
            (0x4000_004e, 0, Some(Action::PageDown)),
            (0, 301, Some(Action::PageDown)),
            (8, 42, Some(Action::Backspace)),
            (0x4000_009c, 156, Some(Action::ClearAll)),
            (0, 0x1e4, Some(Action::PointerHidden)),
            (48, 0, Some(Action::Digit(0))),
            (49, 0, Some(Action::Digit(1))),
            (50, 0, Some(Action::Digit(2))),
            (51, 0, Some(Action::Digit(3))),
            (52, 0, Some(Action::Digit(4))),
            (53, 0, Some(Action::Digit(5))),
            (54, 0, Some(Action::Digit(6))),
            (55, 0, Some(Action::Digit(7))),
            (56, 0, Some(Action::Digit(8))),
            (57, 0, Some(Action::Digit(9))),
            (57, 33, Some(Action::Digit(9))),
            (0, 48, None),
            (0, 53, None),
            (0, 57, None),
            (47, 0, None),
            (58, 0, None),
            (0, 504, None),
            (0, 269, None),
            (0, 270, None),
            (97, 4, None),
            (0, 0, None),
            (0, 299, None),
            (0, 302, None),
            (0x4000_004c, 0, None),
        ];
        for &(sym, wcode, action) in table {
            let Some(HostEvent::Key(press)) = decode_host_event(&encode_key(sym, wcode, true)) else {
                panic!("({sym}, {wcode}) decodes to a key press");
            };
            assert_eq!(press.action(), action, "({sym}, {wcode}) means {action:?}");
            assert_eq!(press.pair(), RawKey { sym, wcode }, "({sym}, {wcode}) keeps the pair it arrived as");
        }
    }

    /// **The pairs the FIFO's named tokens press are the ones they have always pressed**, written out
    /// as literals: they are what `tests/keytable.json` and the recorded tapes carry, and the fold
    /// through `raw_pair` must not move one. The rocker's two tokens and `k:` are not actions' pairs
    /// and stay literal.
    #[test]
    fn the_fifo_tokens_press_the_pairs_they_always_pressed() {
        let (up, down, left, right) = (0x4000_0052u32, 0x4000_0051u32, 0x4000_0050u32, 0x4000_004fu32);
        let (pageup, pagedown, clear) = (0x4000_004bu32, 0x4000_004eu32, 0x4000_009cu32);
        for (tok, pair) in [
            ("up", (up, 0)),
            ("down", (down, 0)),
            ("left", (left, 0)),
            ("right", (right, 0)),
            ("ok", (13, 0)),
            ("enter", (13, 0)),
            ("select", (13, 0)),
            ("back", (27, 0)),
            ("esc", (27, 0)),
            ("pageup", (pageup, 0)),
            ("pagedown", (pagedown, 0)),
            ("chup", (0, 300)),
            ("chdown", (0, 301)),
            ("play", (0, 450)),
            ("pause", (0, 72)),
            ("stop", (0, 413)),
            ("ff", (0, 451)),
            ("fastforward", (0, 451)),
            ("rew", (0, 452)),
            ("rewind", (0, 452)),
            ("playpause", (0, 261)),
            ("exit", (0, 505)),
            ("backspace", (8, 42)),
            ("del", (8, 42)),
            ("clear", (clear, 156)),
            ("k:0,504", (0, 504)),
            ("k:53,34", (53, 34)),
        ] {
            assert_eq!(remote_token_key(tok), Some(pair), "{tok}");
        }
        assert_eq!(remote_token_key("home"), None, "an unknown token names no key");
        assert_eq!(remote_token_key("k:1"), None, "`k:` needs both fields");
    }

    /// `SDL_TEXTINPUT` decodes to its string at THIS build's offset (LG's fork, +16; stock SDL, +12),
    /// written out as literals: ASCII, a CJK commit (3 bytes a character), a full unterminated
    /// 32-byte array, and the empty commit SDL sends for the IME's bookkeeping, which is still a
    /// `Text` (the loop ignores it).
    #[test]
    fn a_text_commit_decodes_at_this_builds_offset() {
        let off = if cfg!(feature = "hostsim") { 12 } else { 16 };
        let text = |bytes: &[u8]| {
            let mut ev = [0u8; 128];
            ev[0..4].copy_from_slice(&0x303u32.to_ne_bytes());
            ev[off..off + bytes.len()].copy_from_slice(bytes);
            decode_host_event(&ev)
        };
        assert_eq!(text(b"wal"), Some(HostEvent::Text("wal".into())));
        assert_eq!(text("星球".as_bytes()), Some(HostEvent::Text("星球".into())));
        assert_eq!(text(&[b'a'; 32]), Some(HostEvent::Text("a".repeat(32))));
        assert_eq!(text(b""), Some(HostEvent::Text(String::new())));
    }
}
