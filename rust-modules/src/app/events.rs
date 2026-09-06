//! SDL event decoding and the remote-FIFO token synthesis — the raw-offset reads of LG's shifted
//! `SDL_KeyboardEvent`, the synthetic key/pointer/wheel events, and `dispatch_remote_token`.
//! Moved out of `app.rs` verbatim in phase 1a (a pure move; `pub(super)` widening only).

use super::*;

#[inline]
pub(super) fn rd_u32(ev: &[u8], off: usize) -> u32 {
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
pub(super) fn decode_key(ev: &[u8]) -> (u32, u32, u32) {
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
pub(super) const SYNTH_WINDOW: u32 = 0x504c_584b; // "PLXK"

/// The Magic Remote button a desktop keyboard stands in for, or 0.
///
/// Only the keys with NO sym equivalent need this. Navigation and OK/BACK already work on a
/// keyboard through `is_ok`/`is_back`, which accept RETURN/ESCAPE/'q' — those predicates were
/// always keyboard-capable, which is why the simulator needs no remapping layer for them.
#[inline]
pub(super) fn host_wcode(sym: u32) -> u32 {
    // ASCII literals spelled numerically: `b'p' as u32` is an expression, not a pattern.
    match sym {
        32 => crate::ui::consts::WCODE_PAUSE, // space
        112 => crate::ui::consts::WCODE_PLAY, // 'p'
        115 => crate::ui::consts::WCODE_STOP, // 's'
        8 => crate::ui::consts::WCODE_BACK,   // backspace
        _ => 0,
    }
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
pub(super) fn encode_key(sym: c_uint, wcode: c_uint, down: bool) -> [u8; 128] {
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
/// 0x101 shape (`state & 0x100 != 0`) `on_auto_repeat` requires, in whichever layout `decode_key`
/// reads. `encode_key` itself must never produce this (`key_bytes_round_trip` pins that a synthetic
/// EDGE "must never look like auto-repeat"), so it is a second, deliberately separate function
/// rather than a third argument threaded through the first — item 13's `holdrep:<name>` FIFO token
/// is the only caller, and it exists so a script can exercise `on_auto_repeat`'s
/// Settings/Consent/Legal forwarding without a real remote's own repeat cadence.
pub(super) fn encode_key_repeat(sym: c_uint, wcode: c_uint) -> [u8; 128] {
    let mut ev = encode_key(sym, wcode, true);
    if cfg!(feature = "hostsim") {
        ev[13] = 1; // the `repeat` byte `decode_key`'s hostsim arm folds into `state & 0x100`
    } else {
        let state = rd_u32(&ev, 16) | 0x100;
        ev[16..20].copy_from_slice(&state.to_ne_bytes());
    }
    ev
}

#[inline]
/// An SDL pointer event's position, converted from window pixels to the authored 1920x1080 canvas.
///
/// THE one place event coordinates enter the UI, so the conversion cannot be forgotten at a new
/// call site — there are nine, and patching them individually is how the tenth ends up wrong.
/// `surface::to_logical` is the identity while the drawable is 1920x1080, which it is on every
/// television seen so far.
pub(super) fn ptr_xy(ev: &[u8]) -> (f32, f32) {
    crate::surface::to_logical(rd_i32(ev, 20) as f32, rd_i32(ev, 24) as f32)
}

pub(super) fn rd_i32(ev: &[u8], off: usize) -> i32 {
    i32::from_ne_bytes([ev[off], ev[off + 1], ev[off + 2], ev[off + 3]])
}

pub(super) fn rd_f32(ev: &[u8], off: usize) -> f32 {
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
pub(super) fn remote_token_key(tok: &str) -> Option<(c_uint, c_uint)> {
    if let Some(rest) = tok.strip_prefix("k:") {
        let (s, w) = rest.split_once(',')?;
        return Some((s.parse().ok()?, w.parse().ok()?));
    }
    Some(match tok {
        "up" => (SDLK_UP, 0),
        "down" => (SDLK_DOWN, 0),
        "left" => (SDLK_LEFT, 0),
        "right" => (SDLK_RIGHT, 0),
        "ok" | "enter" | "select" => (SDLK_RETURN, 0), // is_ok()
        "back" | "esc" => (SDLK_ESCAPE, 0),            // is_back()
        // The pager's two spellings, and they are two tokens now rather than one pair carrying
        // both: a PAGE key is a keyboard's and arrives as a sym alone, the rocker is the remote's
        // and arrives as a wcode alone. The single pair they shared was `(SDLK_PAGEUP, 33)` — a
        // shape no real press has, since 33 is the digit `4` (`ui::consts`, where they were
        // retired), so half of what it drove was never the pager answering the rocker at all.
        "pageup" => (SDLK_PAGEUP, 0),
        "pagedown" => (SDLK_PAGEDOWN, 0),
        "chup" => (0, WCODE_CH_UP_KEY),
        "chdown" => (0, WCODE_CH_DOWN_KEY),
        "play" => (0, WCODE_PLAY),
        "pause" => (0, WCODE_PAUSE),
        "stop" => (0, WCODE_STOP),
        // The transport keys settled from LG's own scancode table (`ui::consts`' WCODE_REWIND doc).
        // Here for the same reason the edit keys below are: nothing else can press them headlessly.
        "ff" | "fastforward" => (0, crate::ui::consts::WCODE_FASTFORWARD),
        "rew" | "rewind" => (0, crate::ui::consts::WCODE_REWIND),
        "playpause" => (0, crate::ui::consts::WCODE_PLAYPAUSE),
        "exit" => (0, crate::ui::consts::WCODE_EXIT),
        // The system keyboard's own two edit keys (`ui::consts`' doc has the protocol). They are
        // here because they are otherwise UNREACHABLE without a human at the panel: no trigger
        // raises the keyboard and `SDL_PushEvent` cannot carry a text event on the simulator, so
        // without these the only grader for backspace and Clear all is somebody's thumb.
        "backspace" | "del" => (crate::ui::consts::SDLK_BACKSPACE, 42),
        "clear" => (crate::ui::consts::SDLK_CLEAR, 156),
        _ => return None,
    })
}

/// Synthesize a Magic-Remote pointer click at authored 1920x1080 coords (the browser
/// remote's click-on-the-stream): two motion events, then button down+up. The first
/// motion is a >=120px jitter so the accumulated pointer distance defeats the
/// D-pad-mode pointer gate (`Pointer::mot_accum < 120` swallows small motions after D-pad use);
/// the second lands on the target. The LG SDL fork's mouse events carry x@20 / y@24
/// (i32) — the only fields the handlers read.
///
/// Click only, deliberately: forwarding hover moved app focus on every pass of the
/// mouse over the streamed picture (parking it on a top-band tab pill, so the next
/// ENTER opened the library). The host page draws its own local crosshair instead.
pub(super) fn remote_synth_ptr(x: i32, y: i32) {
    let mut ev = [0u8; 128];
    let mut push = |et: u32, px: i32, py: i32| {
        // Authored coords go onto SDL's queue as WINDOW pixels, because that is what a real
        // pointer event carries and `ptr_xy` converts every one of them back. Skipping this would
        // transform the synthetic path twice on a scaled surface — and it is the path the whole
        // headless test harness clicks through, so it would fail in a way that looked like the UI.
        let (px, py) = crate::surface::to_physical(px as f32, py as f32);
        let (px, py) = (px.round() as i32, py.round() as i32);
        ev[0..4].copy_from_slice(&et.to_ne_bytes());
        ev[20..24].copy_from_slice(&px.to_ne_bytes());
        ev[24..28].copy_from_slice(&py.to_ne_bytes());
        unsafe { SDL_PushEvent(ev.as_ptr() as *const c_void) };
    };
    let jx = if x >= 200 { x - 200 } else { x + 200 };
    push(SDL_MOUSEMOTION, jx, y);
    push(SDL_MOUSEMOTION, x, y);
    push(SDL_MOUSEBUTTONDOWN, x, y);
    push(SDL_MOUSEBUTTONUP, x, y);
}

/// Synthesize a full remote-key press (key-down then key-up) and push both onto SDL's
/// own event queue, so the existing poll loop consumes them as if they came off the
/// wayland input path. The LG SDL fork's `SDL_KeyboardEvent` carries state@16 /
/// wcode@20 / sym@24 (native-endian; the TV is LE), and the handler reads press vs
/// release from `state & 0xff` — so the down carries state=1, the up state=0. Both
/// are required: a grid-card OK arms on down and *commits on release*.
pub(super) fn remote_synth_key(sym: c_uint, wcode: c_uint) {
    remote_synth_key_edge(sym, wcode, true);
    remote_synth_key_edge(sym, wcode, false);
}

/// ONE edge of a remote key press. Split out for the `okdown`/`okup` FIFO tokens, because a
/// **press-and-hold** is only expressible as two tokens with real time between them: the item menu
/// opens on `press::is_long`, which measures the interval between the down and the up. The paired
/// `remote_synth_key` above is this called twice back to back (a tap).
pub(super) fn remote_synth_key_edge(sym: c_uint, wcode: c_uint, down: bool) {
    let ev = encode_key(sym, wcode, down);
    unsafe { SDL_PushEvent(ev.as_ptr() as *const c_void) };
}

/// ONE hardware auto-repeat edge — item 13's `holdrep:<name>` FIFO token, which lets a script
/// exercise `on_auto_repeat`'s Settings/Consent/Legal forwarding (and the player scrubber's
/// existing continuous-scrub path) without a real remote's own repeat cadence. Only recognised as a
/// repeat by `on_auto_repeat`'s caller when `held_key.down_sym` already equals `sym` — i.e. after a
/// `holddown:<name>` and before its matching `holdup:<name>`, the same split `okdown`/`okup`
/// already uses for a press-and-hold.
pub(super) fn remote_synth_key_repeat(sym: c_uint, wcode: c_uint) {
    let ev = encode_key_repeat(sym, wcode);
    unsafe { SDL_PushEvent(ev.as_ptr() as *const c_void) };
}

/// Synthesize one Magic-Remote scroll-wheel tick — item 13's `wheel:<dy>` FIFO token, so a script
/// can drive the wheel with no mouse in the room. Encoded in the plain, real-SDL2 shape
/// (`Sint32 y` at `+20`) unconditionally: it is the READING side that has to branch by platform
/// now, not this one — see the wheel arm's own comment on why `+20` decodes to 0 on this host and
/// where the value actually lands after `SDL_PushEvent` round-trips it.
pub(super) fn remote_synth_wheel(dy: i32) {
    let mut ev = [0u8; 128];
    ev[0..4].copy_from_slice(&SDL_MOUSEWHEEL.to_ne_bytes());
    ev[20..24].copy_from_slice(&dy.to_ne_bytes());
    unsafe { SDL_PushEvent(ev.as_ptr() as *const c_void) };
}

/// Is this SDL event type INPUT — a key, text, pointer or wheel event — as opposed to a lifecycle,
/// window or quit event? The one classification `popover::host::input_scope` rests on: input under
/// an open modal is the modal's, a lifecycle event is the app's and may change the page beneath.
pub(super) fn is_input_event(et: u32) -> bool {
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

    /// The boundary `popover::host::input_scope` rests on: every input kind is in, and the
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
pub(super) fn dispatch_remote_token(tok: &str) -> bool {
    // As the SDL loop: a modal's input is its own. NOT for `pat:`, which is not input at all — it
    // changes the PAGE's ground directly, and a frozen host must be retaken to show it.
    let _own_input = if tok.starts_with("pat:") {
        None
    } else {
        crate::ui::popover::host::input_scope()
    };
    crate::ui::idle::invalidate(); // injected input is input like any other
                                   // pointer click token "ck:X,Y" — authored 1920x1080 coords
    if let Some(rest) = tok.strip_prefix("ck:") {
        let Some((xs, ys)) = rest.split_once(',') else {
            return false;
        };
        let (Ok(x), Ok(y)) = (xs.parse::<i32>(), ys.parse::<i32>()) else {
            return false;
        };
        log(&format!("remote: click {},{}", x, y));
        remote_synth_ptr(x.clamp(0, 1919), y.clamp(0, 1079));
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
        // `holddown:<name>` (physical press, arms `held_key.down_sym`), `holdrep:<name>` (one
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
        if crate::lab::menu_row_enabled() {
            crate::lab::request_upload("command");
            true
        } else {
            false
        }
    } else if let Some(spec) = tok.strip_prefix("pat:") {
        // `pat:flat:40` — swap the synthetic ground live for a one-session graded sweep.
        let ok = crate::ui::testpat::set(spec);
        if !ok {
            crate::log(&format!("remote: unrecognised pattern {spec:?}"));
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

