//! SDL's events as the application reads them: the event-type codes, the raw-offset readers, the key,
//! pointer, wheel and text layouts of BOTH SDLs this project runs on, and the queue's `poll`/`push`.
//!
//! The decode stops at neutral values ([`Decoded`]): a key is `(state, wcode, sym)`, never an
//! `Action`, because naming one is the application's vocabulary (`ui::consts`) and a shell may not
//! name `ui`. The application maps a [`Decoded`] onto its own `HostEvent`; the tape's format is the
//! SDL code each [`Lifecycle`] edge arrived as ([`Lifecycle::sdl_code`]), so those pairs live here,
//! where the decode table is.
use crate::sys;
use plx_base::surface::to_logical;
use std::os::raw::c_void;

// event types
pub const SDL_QUIT: u32 = 0x100;
pub const SDL_KEYDOWN: u32 = 0x300;
pub const SDL_KEYUP: u32 = 0x301;
pub const SDL_MOUSEMOTION: u32 = 0x400;
pub const SDL_MOUSEBUTTONDOWN: u32 = 0x401;
pub const SDL_MOUSEBUTTONUP: u32 = 0x402;
pub const SDL_MOUSEWHEEL: u32 = 0x403;
/// The IME's in-progress COMPOSITION. Not acted on — the search field shows what has been
/// committed, so a preedit would put characters on screen the query does not contain — but LOGGED,
/// because the panel's word prediction is a replace and this is where its delete half would arrive
/// if it arrives at all. See the `"edit"` arm in the application's event ladder.
pub const SDL_TEXTEDITING: u32 = 0x302;
/// Text COMMITTED by the system keyboard — the application's `textinput`.
pub const SDL_TEXTINPUT: u32 = 0x303;

/// SDL's application events (`SDL_APP_*`), the four the loop acts on: the compositor taking the
/// screen (WILL and DID enter background) and giving it back (WILL and DID enter foreground).
pub const SDL_APP_WILLENTERBACKGROUND: u32 = 0x103;
pub const SDL_APP_DIDENTERBACKGROUND: u32 = 0x104;
pub const SDL_APP_WILLENTERFOREGROUND: u32 = 0x105;
pub const SDL_APP_DIDENTERFOREGROUND: u32 = 0x106;

/// Poll SDL's one process-wide event queue into `ev`. `true` when an event was written. SDL writes
/// at most `sizeof(SDL_Event)` (56 bytes) of the 128 the buffer holds, so the rest is whatever the
/// last, larger event left behind.
pub fn poll(ev: &mut [u8; 128]) -> bool {
    unsafe { sys::SDL_PollEvent(ev.as_mut_ptr() as *mut c_void) != 0 }
}

/// Push one event onto SDL's own queue, so the existing poll loop consumes it as if it had come off
/// the wayland input path. The only caller is `inject.rs`.
pub(crate) fn push(ev: &[u8; 128]) {
    unsafe { sys::SDL_PushEvent(ev.as_ptr() as *const c_void) };
}

#[inline]
pub fn rd_u32(ev: &[u8], off: usize) -> u32 {
    u32::from_ne_bytes([ev[off], ev[off + 1], ev[off + 2], ev[off + 3]])
}

#[inline]
pub fn rd_i32(ev: &[u8], off: usize) -> i32 {
    i32::from_ne_bytes([ev[off], ev[off + 1], ev[off + 2], ev[off + 3]])
}

#[inline]
pub fn rd_f32(ev: &[u8], off: usize) -> f32 {
    f32::from_ne_bytes([ev[off], ev[off + 1], ev[off + 2], ev[off + 3]])
}

/// A keyboard event's `(state, wcode, sym)`, decoded from the raw event bytes. The `wcode` is
/// `None` only on the host layout, for a press that carries no injected one: stock SDL has no webOS
/// keycode at all, so the application applies its desktop stand-in (`host_wcode`, a `ui` table this
/// crate may not name) exactly where this decode used to. The television's layout always has one.
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
/// and not a preference: `host_wcode(8)` is BACK, while the `backspace` token is `(8, 42)`, so
/// asking the stand-in first turns the panel's delete key into a navigation. That priority is why
/// this returns the injected wcode or `None` and never a stand-in of its own: the caller's
/// `unwrap_or_else(host_wcode)` can only ever apply to a press that carried none.
/// Clobbering `windowID` is safe here and nowhere else: this arm is `hostsim`-only, the simulator
/// has one window, and nothing in the event loop reads that field.
#[inline]
pub fn decode_key(ev: &[u8]) -> (u32, Option<u32>, u32) {
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
        let wcode = if injected != 0 { Some(injected) } else { None };
        (state, wcode, sym)
    } else {
        (rd_u32(ev, 16), Some(rd_u32(ev, 20)), rd_u32(ev, 24))
    }
}

/// The `windowID` a SYNTHETIC key event carries on the host, marking it as one — see [`decode_key`]
/// for why the wcode needs a marker beside it rather than standing on its own. Any value no real
/// window can have; SDL numbers windows from 1.
///
/// Defined unconditionally, like both halves of [`decode_key`]: the arm that uses it is behind
/// `cfg!` rather than `#[cfg]`, precisely so the configuration nobody is currently building cannot
/// rot.
pub const SYNTH_WINDOW: u32 = 0x504c_584b; // "PLXK"

/// What a pointer event did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PointerEdge {
    /// `SDL_MOUSEMOTION`.
    Move,
    /// `SDL_MOUSEBUTTONDOWN`, any button.
    Down,
    /// `SDL_MOUSEBUTTONUP`, any button.
    Up,
}

/// One edge of the window's visibility. Both background edges park the tree (the second finds
/// nothing left to park); WILL foreground un-parks the tree, and DID foreground alone reacquires the
/// window and restores playback. A platform that sends only DID background is enough on its own
/// (`WindowActivity`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Lifecycle {
    WillBackground,
    DidBackground,
    WillForeground,
    DidForeground,
}

impl Lifecycle {
    /// The SDL event type this edge arrives as, and so the code the SDL-coded readers take: the
    /// recorder's tape (`enc_lifecycle`, whose recorded codes ARE the tape format), the window gate
    /// and telemetry's crash steps and session. The inverse of [`decode`]'s table; the tests pin the
    /// four pairs.
    pub const fn sdl_code(self) -> u32 {
        match self {
            Lifecycle::WillBackground => SDL_APP_WILLENTERBACKGROUND,
            Lifecycle::DidBackground => SDL_APP_DIDENTERBACKGROUND,
            Lifecycle::WillForeground => SDL_APP_WILLENTERFOREGROUND,
            Lifecycle::DidForeground => SDL_APP_DIDENTERFOREGROUND,
        }
    }
}

/// One SDL event as neutral values. `Key` carries the bytes' own `(state, wcode, sym)` (see
/// [`decode_key`] for the `Option`), and the application turns that into its `KeyPress`.
#[derive(Clone, Debug, PartialEq)]
pub enum Decoded {
    /// `SDL_QUIT`.
    Quit,
    /// One edge of a key: the fork's packed `state` (low byte 1 is down, bit 0x100 is the
    /// auto-repeat), the wcode where the bytes carry one, and the sym.
    Key { state: u32, wcode: Option<u32>, sym: u32 },
    /// One commit from the system keyboard: the decoded string, possibly empty (SDL delivers a
    /// commit for the IME's own bookkeeping too, and the loop ignores an empty one).
    Text(String),
    /// One pointer event at authored 1920x1080 coordinates ([`ptr_xy`]).
    Pointer { x: f32, y: f32, edge: PointerEdge },
    /// One tick of the wheel in the signed whole ticks the loop reads ([`decode_wheel_dy`]).
    Wheel { dy: i32 },
    /// The window left or came back to the screen.
    Lifecycle(Lifecycle),
}

/// An SDL event decoded: `SDL_QUIT`, the four application lifecycle types, a key edge
/// (`SDL_KEYDOWN`/`SDL_KEYUP`, through [`decode_key`], so both layouts), a text commit
/// (`SDL_TEXTINPUT`, through [`crate::text::decode`]), a pointer event (motion, button down and
/// button up, through [`ptr_xy`]) and a wheel tick (through [`decode_wheel_dy`]). `None` for
/// everything else, which the loop reads raw (the IME's pre-edit, and the SDL events nothing acts
/// on). Beside [`decode_key`], the other place that knows SDL's event bytes.
#[inline]
pub fn decode(ev: &[u8]) -> Option<Decoded> {
    Some(match rd_u32(ev, 0) {
        SDL_QUIT => Decoded::Quit,
        SDL_KEYDOWN | SDL_KEYUP => {
            let (state, wcode, sym) = decode_key(ev);
            Decoded::Key { state, wcode, sym }
        }
        SDL_TEXTINPUT => Decoded::Text(crate::text::decode(ev)),
        SDL_MOUSEMOTION => pointer_event(ev, PointerEdge::Move),
        SDL_MOUSEBUTTONDOWN => pointer_event(ev, PointerEdge::Down),
        SDL_MOUSEBUTTONUP => pointer_event(ev, PointerEdge::Up),
        SDL_MOUSEWHEEL => Decoded::Wheel { dy: decode_wheel_dy(ev) },
        SDL_APP_WILLENTERBACKGROUND => Decoded::Lifecycle(Lifecycle::WillBackground),
        SDL_APP_DIDENTERBACKGROUND => Decoded::Lifecycle(Lifecycle::DidBackground),
        SDL_APP_WILLENTERFOREGROUND => Decoded::Lifecycle(Lifecycle::WillForeground),
        SDL_APP_DIDENTERFOREGROUND => Decoded::Lifecycle(Lifecycle::DidForeground),
        _ => return None,
    })
}

/// A pointer event's position and edge. The coordinates are read at +20 and +24 in both layouts (the
/// pointer offsets agree, [`ptr_xy`]), and the button at +16 and its state at +17 are not read: the
/// edge is the event type, for any button, as the loop has always taken it.
#[inline]
fn pointer_event(ev: &[u8], edge: PointerEdge) -> Decoded {
    let (x, y) = ptr_xy(ev);
    Decoded::Pointer { x, y, edge }
}

/// A wheel event's vertical tick count, in the signed whole ticks the loop reads.
///
/// **The host reads a DIFFERENT offset, and this one is not the LG-fork
/// shift `decode_key` documents — it is macOS `libSDL2` again being
/// sdl2-compat forwarding into SDL3.** `SDL_MouseWheelEvent` on real SDL2
/// (what the television runs) carries a plain `Sint32 y` at +20, which is
/// what production code always read. Measured 2026-09-02 by dumping the
/// polled bytes of an injected -40 tick on this host: `+20` came back 0, and
/// -40.0 arrived instead as the FLOAT `preciseY` field at `+32` — SDL2's own
/// struct gained that field in 2.0.18 for fractional trackpad scroll, and
/// sdl2-compat's round trip apparently only forwards it, leaving the legacy
/// integer field zeroed for both a genuine trackpad tick and anything
/// `SDL_PushEvent`d in this shape (item 13's `wheel:<dy>` FIFO token hit
/// this the first time anything synthesized a wheel event at all — nothing
/// needed one before). `cfg!`, not `#[cfg]`, so both arms keep compiling.
#[inline]
pub fn decode_wheel_dy(ev: &[u8]) -> i32 {
    if cfg!(feature = "hostsim") {
        rd_f32(ev, 32).round() as i32
    } else {
        rd_i32(ev, 20)
    }
}

/// An SDL pointer event's position, converted from window pixels to the authored 1920x1080 canvas.
///
/// THE one place event coordinates enter the UI, so the conversion cannot be forgotten at a new
/// call site: `pointer_event` is the only caller, and a second one would have to convert for itself.
/// `surface::to_logical` is the identity while the drawable is 1920x1080, which it is 1:1 unless
/// a webOS 1-3 owner picks a smaller Interface resolution (or the debug drawable trigger asks for one).
#[inline]
pub fn ptr_xy(ev: &[u8]) -> (f32, f32) {
    to_logical(rd_i32(ev, 20) as f32, rd_i32(ev, 24) as f32)
}

/// Is this SDL event type INPUT — a key, text, pointer or wheel event — as opposed to a lifecycle,
/// window or quit event? The one classification `Dispatcher::input_scope` rests on: input under
/// an open modal is the modal's, a lifecycle event is the app's and may change the page beneath.
pub fn is_input_event(et: u32) -> bool {
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
mod tests {
    use super::*;
    use crate::inject::{encode_key, encode_key_repeat, encode_pointer};

    fn event_of(code: u32) -> [u8; 128] {
        let mut ev = [0u8; 128];
        ev[..4].copy_from_slice(&code.to_ne_bytes());
        ev
    }

    /// The wcode a decode reports for an injected one: always on the television, and on the host
    /// only when the carrier is set (a wcode of 0 is "no injected wcode" there).
    fn carried(wcode: u32) -> Option<u32> {
        if cfg!(feature = "hostsim") && wcode == 0 {
            None
        } else {
            Some(wcode)
        }
    }

    /// `encode_key` and `decode_key` must agree, in whichever layout this build compiled. The
    /// regression test for a bug that shipped: the two ends disagreed about `SDL_KeyboardEvent`'s
    /// field offsets, so every remote-FIFO token was accepted, decoded into nonsense and silently
    /// dropped. Nothing in the compiler couples a reader and a writer of raw byte offsets, so this
    /// does. `make check` builds the television layout; a hostsim run grades the stock one.
    #[test]
    fn key_bytes_round_trip() {
        // `(8, 42)` is the backspace token and must survive as the pair it is: the application's
        // desktop stand-in claims sym 8, so a decode that consulted it first would turn the panel's
        // delete key into a navigation. Here that is: the injected wcode is reported as such.
        for (sym, wcode) in [
            (0x4000_0051, 0),
            (13, 0),
            (0, 72),
            (8, 482),
            (8, 42),
            (32, 44),
            (112, 19),
            (115, 22),
        ] {
            for down in [true, false] {
                let (state, got_wcode, got_sym) = decode_key(&encode_key(sym, wcode, down));
                assert_eq!(got_sym, sym, "sym lost (wcode={wcode}, down={down})");
                assert_eq!(got_wcode, carried(wcode), "wcode lost (sym={sym}, down={down})");
                assert_eq!(state & 0xff, u32::from(down), "press/release lost (sym={sym})");
                assert_eq!(state & 0x100, 0, "a synthetic edge must never look like auto-repeat");
            }
            let (state, got_wcode, got_sym) = decode_key(&encode_key_repeat(sym, wcode));
            assert_eq!((got_sym, got_wcode), (sym, carried(wcode)));
            assert_eq!(state, 0x101, "a repeat is a DOWN edge with the repeat bit");
        }
    }

    /// The key layout, byte for byte and written out a second time rather than through
    /// `encode_key`. LG's fork: `state` u32 at +16, wcode at +20, sym at +24. Stock SDL (the
    /// simulator): `state` u8 at +12, `repeat` u8 at +13, sym at +20, and a synthetic press carries
    /// its wcode in `mod` at +24 under the `SYNTH_WINDOW` marker at +8.
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
        assert_eq!(decode_key(&up), (1, carried(0), 0x4000_0052));
        assert_eq!(decode_key(&play), (1, Some(450), 0));
        assert_eq!(decode_key(&repeat), (0x101, carried(0), 0x4000_0052));
        // a desktop press (no marker) never carries a wcode, whatever `mod` holds
        if cfg!(feature = "hostsim") {
            let mut real = up;
            real[8..12].copy_from_slice(&1u32.to_ne_bytes());
            real[24..26].copy_from_slice(&0x1000u16.to_ne_bytes()); // a modifier bitmask
            assert_eq!(decode_key(&real).1, None);
        }
    }

    /// SDL's quit and application events, decoded. The SDL codes are literals: the table is the
    /// contract with SDL and with the tape, so it is not read back from the mapping it grades.
    #[test]
    fn quit_and_the_four_lifecycle_types_decode_and_the_lifecycle_codes_round_trip() {
        for (code, event) in [
            (0x100, Decoded::Quit),
            (0x103, Decoded::Lifecycle(Lifecycle::WillBackground)),
            (0x104, Decoded::Lifecycle(Lifecycle::DidBackground)),
            (0x105, Decoded::Lifecycle(Lifecycle::WillForeground)),
            (0x106, Decoded::Lifecycle(Lifecycle::DidForeground)),
        ] {
            assert_eq!(decode(&event_of(code)), Some(event.clone()), "{code:#x}");
            assert!(!is_input_event(code), "{code:#x}: a window event is never a modal's input");
            if let Decoded::Lifecycle(edge) = event {
                assert_eq!(edge.sdl_code(), code, "the code a decoded edge is read back as");
            }
        }
    }

    /// Everything the loop still reads raw stays raw: the IME's pre-edit (0x302), the SDL events
    /// nothing acts on, and the types either side of the pointer and wheel block.
    #[test]
    fn every_other_event_stays_on_the_raw_path() {
        for code in [0, 0x101, 0x102, 0x107, 0x200, 0x302, 0x3ff, 0x404, u32::MAX] {
            assert_eq!(decode(&event_of(code)), None, "{code:#x}");
        }
    }

    /// The boundary `Dispatcher::input_scope` rests on: every input kind is in, and the lifecycle
    /// events, window events and quit are out.
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

    /// A pointer event as SDL lays it out, written by hand: `type` at +0, the window id at +8, the
    /// button at +16 and its state at +17, then `Sint32 x` at +20 and `Sint32 y` at +24. LG's fork
    /// and stock SDL2 agree on all of it (the keyboard event is the one that shifts), which is why
    /// the decoder has one reading for both.
    fn pointer_bytes(code: u32, button: u8, state: u8, x: i32, y: i32) -> [u8; 128] {
        let mut ev = [0u8; 128];
        ev[0..4].copy_from_slice(&code.to_ne_bytes());
        ev[8..12].copy_from_slice(&7u32.to_ne_bytes());
        ev[16] = button;
        ev[17] = state;
        ev[20..24].copy_from_slice(&x.to_ne_bytes());
        ev[24..28].copy_from_slice(&y.to_ne_bytes());
        ev
    }

    fn pointer(x: f32, y: f32, edge: PointerEdge) -> Option<Decoded> {
        Some(Decoded::Pointer { x, y, edge })
    }

    const POINTERS: [(u32, PointerEdge); 3] = [
        (SDL_MOUSEMOTION, PointerEdge::Move),
        (SDL_MOUSEBUTTONDOWN, PointerEdge::Down),
        (SDL_MOUSEBUTTONUP, PointerEdge::Up),
    ];

    /// **The decode table.** Each of the three SDL types, at the corners and the middle of the
    /// canvas and just outside it (a drag past the edge reports negative and over-large positions),
    /// decodes to its edge and its position. The surface is the identity here (1920x1080, the
    /// default), so the position is what the bytes say.
    #[test]
    fn an_sdl_pointer_event_decodes_to_its_edge_and_its_position() {
        let _serial = plx_base::testlock::serial();
        for (code, edge) in POINTERS {
            for (x, y) in [(0, 0), (960, 540), (1919, 1079), (1400, 870), (-3, 1085), (2000, -1)] {
                assert_eq!(
                    decode(&pointer_bytes(code, 1, 1, x, y)),
                    pointer(x as f32, y as f32, edge),
                    "{code:#x} at ({x}, {y})"
                );
            }
        }
    }

    /// **The edge is the event type alone**, for any button and whatever its state byte says: a
    /// button-up carrying state 1, a button-down carrying state 0 and a motion with a button byte
    /// set are still an up, a down and a move. Bytes the decoder must not read are noise here.
    #[test]
    fn the_edge_is_the_event_type_whatever_the_button_bytes_say() {
        let _serial = plx_base::testlock::serial();
        for button in [0u8, 1, 2, 3, 255] {
            for state in [0u8, 1] {
                for (code, edge) in POINTERS {
                    let mut ev = pointer_bytes(code, button, state, 640, 360);
                    ev[28..64].fill(0xa5);
                    assert_eq!(decode(&ev), pointer(640.0, 360.0, edge), "{code:#x} button {button} state {state}");
                }
            }
        }
    }

    /// The bytes the synthesis writes (`encode_pointer`) decode to the position and edge they were
    /// built from. The encoder and the decoder shipped apart once for keys; this is what couples
    /// them for the pointer.
    #[test]
    fn the_synthesized_pointer_bytes_round_trip() {
        let _serial = plx_base::testlock::serial();
        for (code, edge) in POINTERS {
            for (x, y) in [(0, 0), (1400, 870), (1919, 1079), (-200, 400)] {
                let ev = encode_pointer(code, x, y);
                assert_eq!(rd_u32(&ev, 0), code);
                assert_eq!(decode(&ev), pointer(x as f32, y as f32, edge), "{code:#x}");
                if code != SDL_MOUSEMOTION {
                    assert_eq!(ev[16], 1, "the left button survives SDL's event queue");
                    assert_eq!(ev[17], u8::from(code == SDL_MOUSEBUTTONDOWN));
                }
            }
        }
    }

    /// A wheel event as the television's SDL2 lays it out: `Sint32 y` at +20.
    fn wheel_lg(dy: i32) -> [u8; 128] {
        let mut ev = event_of(SDL_MOUSEWHEEL);
        ev[20..24].copy_from_slice(&dy.to_ne_bytes());
        ev
    }

    /// A wheel event as the simulator's stock SDL2 (sdl2-compat over SDL3) delivers it: the count
    /// in the float `preciseY` at +32, with the legacy integer at +20 left at zero.
    fn wheel_stock(precise_y: f32) -> [u8; 128] {
        let mut ev = event_of(SDL_MOUSEWHEEL);
        ev[32..36].copy_from_slice(&precise_y.to_ne_bytes());
        ev
    }

    fn wheel(dy: i32) -> Option<Decoded> {
        Some(Decoded::Wheel { dy })
    }

    /// **Each layout's wheel, and the one this build does not read.** The television's integer at
    /// +20 is the count in the default build and is ignored in the simulator's; the simulator's
    /// float at +32 is the count there (rounded to whole ticks) and is ignored on the television.
    #[test]
    fn a_wheel_tick_is_read_from_the_offset_this_hosts_sdl_carries_it_at() {
        for dy in [-40, -3, -1, 1, 2, 120] {
            if cfg!(feature = "hostsim") {
                assert_eq!(decode(&wheel_stock(dy as f32)), wheel(dy), "stock layout, {dy}");
                assert_eq!(decode(&wheel_lg(dy)), wheel(0), "the television's field is not this host's: {dy}");
            } else {
                assert_eq!(decode(&wheel_lg(dy)), wheel(dy), "LG layout, {dy}");
                assert_eq!(decode(&wheel_stock(dy as f32)), wheel(0), "the simulator's field is not read here: {dy}");
            }
        }
    }

    /// A fractional trackpad tick keeps the sign of what it rounds to, and a half tick or less is
    /// no tick (the loop records a zero and ignores it). Only the simulator's layout carries one.
    #[cfg(feature = "hostsim")]
    #[test]
    fn a_fractional_trackpad_tick_rounds_to_whole_ticks() {
        for (precise, dy) in [(-0.4, 0), (0.4, 0), (-0.6, -1), (0.6, 1), (-1.4, -1), (2.5, 3), (-40.0, -40)] {
            assert_eq!(decode(&wheel_stock(precise)), wheel(dy), "{precise}");
        }
    }

    /// A key edge decodes with its packed state and the carrier's wcode, a text commit with this
    /// build's offset, and the events nothing reads stay `None`.
    #[test]
    fn a_key_and_a_text_commit_decode_to_their_neutral_values() {
        let key = decode(&encode_key(0x4000_0052, 0, true));
        assert_eq!(key, Some(Decoded::Key { state: 1, wcode: carried(0), sym: 0x4000_0052 }));
        assert_eq!(decode(&crate::text::encode_event("star wars")), Some(Decoded::Text("star wars".into())));
        assert_eq!(decode(&event_of(SDL_TEXTEDITING)), None);
    }
}
