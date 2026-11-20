//! Synthetic input: the bytes of a key, pointer, wheel or lifecycle event in whichever layout
//! [`crate::event`] reads, and the pushes that put them on SDL's own queue so the existing poll loop
//! consumes them as if they had come off the wayland input path. These are the crate's only
//! `SDL_PushEvent` sites. The remote FIFO, Lab Control and the replay driver call them (through the
//! application's token parser, which owns the vocabulary), and so does the production cursor-swap OK.
//!
//! **A synthetic text event is never pushed**: see `text.rs`. `encode_event` builds the bytes and the
//! application's `txt:` token hands them to the decode directly.
//!
//! The encoders are pure and separate from the pushes that consume them, so their tests need no
//! queue; one test here does drive SDL's real queue (`EventsOnly`).
use crate::event::{
    push, rd_u32, SDL_KEYDOWN, SDL_KEYUP, SDL_MOUSEBUTTONDOWN, SDL_MOUSEBUTTONUP, SDL_MOUSEMOTION,
    SDL_MOUSEWHEEL, SYNTH_WINDOW,
};
use std::os::raw::c_uint;

/// The bytes a synthetic key event needs, in whichever layout [`crate::event::decode_key`] reads.
///
/// **The inverse of `decode_key`, and the pair is only correct together.** They already shipped
/// disagreeing once: the simulator accepted every FIFO token and never moved, because this end
/// wrote LG's fork layout while the reading end had been taught stock SDL2's. Nothing in the
/// compiler couples them, so `key_bytes_round_trip` (in `event.rs`) is what does.
///
/// Pure, and separate from the `SDL_PushEvent` that consumes it, so that test needs no queue and
/// runs on any host.
pub fn encode_key(sym: c_uint, wcode: c_uint, down: bool) -> [u8; 128] {
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
pub fn encode_key_repeat(sym: c_uint, wcode: c_uint) -> [u8; 128] {
    let mut ev = encode_key(sym, wcode, true);
    if cfg!(feature = "hostsim") {
        ev[13] = 1; // the `repeat` byte `decode_key`'s hostsim arm folds into `state & 0x100`
    } else {
        let state = rd_u32(&ev, 16) | 0x100;
        ev[16..20].copy_from_slice(&state.to_ne_bytes());
    }
    ev
}

/// SDL pointer bytes shared by clicks, held-pointer FIFO edges and recorded-event replay.
/// The inverse of `ptr_xy`: convert authored coordinates exactly once, including window scaling
/// and letterboxing. Replay may carry a real off-canvas motion, so only the token parser clamps.
/// ABI: SDL's `SDL_events.h` (stock and the NDK's) mouse-event declarations put four
/// Uint32 fields before button@16/state@17 and Sint32 x@20/y@24. ARM32 NDK syntax-only
/// `offsetof` assertions verified these offsets; see `docs/testing-player-pointer.md`.
pub fn encode_pointer(et: u32, x: i32, y: i32) -> [u8; 128] {
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

/// The bytes of one app lifecycle event (`0x103`–`0x106`), as the compositor would send it.
pub fn encode_lifecycle(code: u32) -> [u8; 128] {
    let mut ev = [0u8; 128];
    ev[0..4].copy_from_slice(&code.to_ne_bytes());
    ev
}

/// The bytes of one scroll-wheel tick, in the plain, real-SDL2 shape (`Sint32 y` at `+20`)
/// unconditionally: it is the READING side that has to branch by platform now, not this one — see
/// [`crate::event::decode_wheel_dy`] on why `+20` decodes to 0 on this host and where the value
/// actually lands after `SDL_PushEvent` round-trips it.
pub fn encode_wheel(dy: i32) -> [u8; 128] {
    let mut ev = [0u8; 128];
    ev[0..4].copy_from_slice(&SDL_MOUSEWHEEL.to_ne_bytes());
    ev[20..24].copy_from_slice(&dy.to_ne_bytes());
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
pub fn synth_ptr(x: i32, y: i32) {
    let jx = if x >= 200 { x - 200 } else { x + 200 };
    synth_pointer(SDL_MOUSEMOTION, jx, y);
    synth_pointer(SDL_MOUSEMOTION, x, y);
    synth_pointer(SDL_MOUSEBUTTONDOWN, x, y);
    synth_pointer(SDL_MOUSEBUTTONUP, x, y);
}

/// ONE pointer event in authored coordinates, shared by the FIFO and replay. Unlike `ck:`,
/// these primitives add no motion or release: `pd:` stays held until a separate `pu:` arrives.
pub fn synth_pointer(et: u32, x: i32, y: i32) {
    push(&encode_pointer(et, x, y));
}

/// One app lifecycle event (`0x103`–`0x106`), as the compositor would send it — the replay
/// driver's re-injection of a recorded background/foreground edge.
pub fn synth_lifecycle(code: u32) {
    push(&encode_lifecycle(code));
}

/// Synthesize a full remote-key press (key-down then key-up) and push both onto SDL's
/// own event queue, so the existing poll loop consumes them as if they came off the
/// wayland input path. The LG SDL fork's `SDL_KeyboardEvent` carries state@16 /
/// wcode@20 / sym@24 (native-endian; the TV is LE), and the handler reads press vs
/// release from `state & 0xff` — so the down carries state=1, the up state=0. Both
/// are required: a grid-card OK arms on down and *commits on release*.
pub fn synth_key(sym: c_uint, wcode: c_uint) {
    synth_key_edge(sym, wcode, true);
    synth_key_edge(sym, wcode, false);
}

/// ONE edge of a remote key press. Split out for the `okdown`/`okup` FIFO tokens, because a
/// **press-and-hold** is only expressible as two tokens with real time between them: the item menu
/// opens on `press::is_long`, which measures the interval between the down and the up. The paired
/// [`synth_key`] above is this called twice back to back (a tap).
pub fn synth_key_edge(sym: c_uint, wcode: c_uint, down: bool) {
    push(&encode_key(sym, wcode, down));
}

/// ONE hardware auto-repeat edge — item 13's `holdrep:<name>` FIFO token, which lets a script
/// exercise a held key's `Edge::Repeat` (the Settings family's paced focus walk, the player
/// scrubber's continuous scrub) without a real remote's own repeat cadence. Only recognised as a
/// repeat by the loop's key arm when `App::down_sym` already equals `sym` — i.e. after a
/// `holddown:<name>` and before its matching `holdup:<name>`, the same split `okdown`/`okup`
/// already uses for a press-and-hold.
pub fn synth_key_repeat(sym: c_uint, wcode: c_uint) {
    push(&encode_key_repeat(sym, wcode));
}

/// Synthesize one Magic-Remote scroll-wheel tick — item 13's `wheel:<dy>` FIFO token, so a script
/// can drive the wheel with no mouse in the room. See [`encode_wheel`] for the layout.
pub fn synth_wheel(dy: i32) {
    push(&encode_wheel(dy));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::{decode_wheel_dy, rd_i32, SDL_QUIT};

    #[test]
    fn a_key_event_carries_its_type_and_a_repeat_is_never_a_plain_edge() {
        assert_eq!(rd_u32(&encode_key(5, 6, true), 0), SDL_KEYDOWN);
        assert_eq!(rd_u32(&encode_key(5, 6, false), 0), SDL_KEYUP);
        assert_ne!(encode_key(5, 6, true), encode_key_repeat(5, 6));
        assert_eq!(rd_u32(&encode_key_repeat(5, 6), 0), SDL_KEYDOWN);
    }

    /// The `wheel:` token writes the television's shape unconditionally: the count at +20 and the
    /// float the simulator's decoder reads left empty, which is why it decodes to zero there.
    #[test]
    fn the_wheel_bytes_are_the_television_shape_whatever_the_build() {
        let ev = encode_wheel(-3);
        assert_eq!(rd_u32(&ev, 0), SDL_MOUSEWHEEL);
        assert_eq!(rd_i32(&ev, 20), -3);
        assert_eq!(decode_wheel_dy(&ev), if cfg!(feature = "hostsim") { 0 } else { -3 });
    }

    #[test]
    fn a_lifecycle_event_is_its_code_and_nothing_else() {
        let ev = encode_lifecycle(0x104);
        assert_eq!(rd_u32(&ev, 0), 0x104);
        assert!(ev[4..].iter().all(|&b| b == 0));
        assert_eq!(rd_u32(&encode_lifecycle(SDL_QUIT), 0), SDL_QUIT);
    }

    /// **The queue really carries what is pushed.** A click is four events through SDL's own queue:
    /// a motion at least 120 px away (it defeats the D-pad-mode pointer gate), the motion onto the
    /// target, then the button down and up, in that order. This is the one test that reaches
    /// `SDL_PushEvent` and `SDL_PollEvent`, which is what links `sys.rs`'s event externs for real.
    #[test]
    fn a_click_is_four_events_in_order_through_sdl_s_own_queue() {
        use crate::event::{decode, poll, Decoded, PointerEdge};
        let _serial = plx_base::testlock::serial();
        let _events = crate::testing::EventsOnly::init();
        let mut ev = [0u8; 128];
        while poll(&mut ev) {} // whatever an earlier test left behind
        synth_ptr(1400, 870);
        let mut got = Vec::new();
        while poll(&mut ev) {
            got.push(decode(&ev));
        }
        let edges: Vec<_> = got
            .iter()
            .map(|d| match d {
                Some(Decoded::Pointer { x, y, edge }) => (*edge, *x, *y),
                other => panic!("a click pushes pointer events only, got {other:?}"),
            })
            .collect();
        assert_eq!(
            edges.iter().map(|e| e.0).collect::<Vec<_>>(),
            [PointerEdge::Move, PointerEdge::Move, PointerEdge::Down, PointerEdge::Up]
        );
        assert!((edges[0].1 - 1400.0).abs() >= 120.0, "the first motion is the jitter: {:?}", edges[0]);
        for e in &edges[1..] {
            assert_eq!((e.1, e.2), (1400.0, 870.0), "the rest land on the target");
        }
    }
}
