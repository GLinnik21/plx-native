//! What the loop reads of SDL's events, and the remote-FIFO token synthesis: the `HostEvent` a decoded
//! SDL event becomes, the `KeyPress` constructor, the token parser and `dispatch_remote_token`. The bytes
//! themselves (the raw-offset reads of LG's shifted `SDL_KeyboardEvent`, the synthetic key, pointer and
//! wheel events) are `plx_shell_sdl`'s, re-exported below.
//!
//! The one decode that yields a platform-neutral value is [`decode_host_event`]: SDL's quit,
//! application-lifecycle, key, text, pointer and wheel events become a [`HostEvent`], which the loop
//! consumes in one function. A key carries its canonical [`Action`] (`ui::consts`) beside the raw
//! pair; a pointer event carries authored coordinates, and a wheel tick its already-decoded count.

use super::*;
use plx_machine::machine::Edge;
// `RawKey` is `plx_ui::consts`' (a platform crate names it through `host`); this is the application's spelling.
pub(crate) use plx_ui::consts::RawKey;
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

// What SDL's bytes are, and how they are written back, is the stock-SDL shell's (`plx_shell_sdl`): the
// raw-offset readers and both layouts of the key, pointer, wheel and text events, the lifecycle codes
// the tape is written in, and the synthetic events pushed onto SDL's queue. The application keeps what
// needs its own vocabulary: the `HostEvent` the loop consumes, the `KeyPress` constructor, the desktop's
// key stand-in, and the token parser. Re-exported here so the `remote_synth_*` names every call site
// (the loop, the FIFO, the replay driver, the cursor-swap click) already uses stay what they are.
pub(crate) use plx_shell_sdl::event::{is_input_event, rd_u32, Decoded, Lifecycle, PointerEdge};
pub(crate) use plx_shell_sdl::inject::{
    synth_key as remote_synth_key, synth_key_edge as remote_synth_key_edge,
    synth_key_repeat as remote_synth_key_repeat, synth_lifecycle as remote_synth_lifecycle,
    synth_pointer as remote_synth_pointer, synth_ptr as remote_synth_ptr, synth_wheel as remote_synth_wheel,
};
// The byte writers are the loop tests' (they write the bytes a platform would deliver), and the two
// readers are the hostsim wheel test's (the stock layout's float).
#[cfg(all(test, feature = "hostsim"))]
pub(crate) use plx_shell_sdl::event::{rd_f32, rd_i32};
#[cfg(test)]
pub(crate) use plx_shell_sdl::inject::{encode_key, encode_key_repeat, encode_pointer};

/// The wcode a key press is read as: the one its bytes carried, else the desktop's stand-in for it.
///
/// The shell reports `None` only for a stock-SDL press that carries no injected wcode (the television's
/// layout always has one), and the stand-in is the only way a physical Mac keyboard reaches a key the
/// remote has and a keyboard does not (space = PAUSE). **Injected first, the stand-in only as a
/// fallback** is load-bearing and not a preference: `host_wcode(8)` is BACK, while the `backspace` token
/// is `(8, 42)`, so asking the stand-in first turns the panel's delete key into a navigation
/// (`plx_shell_sdl::event::decode_key` has the measurement behind the carrier). It lives here and not in
/// the shell because `host_wcode` is a `ui` table the shell may not name.
#[inline]
fn wcode_of(carried: Option<u32>, sym: u32) -> u32 {
    carried.unwrap_or_else(|| host_wcode(sym))
}

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

/// What the platform tells the loop: a key press, a text commit, the pointer, the wheel, the window
/// leaving or returning to the screen, and the host asking the application to end. App-private, unlike
/// [`Shell`](super::platform::Shell) and [`Surface`](super::platform::Surface), which `host` publishes: a
/// platform reports input through [`EventSink`](super::platform::EventSink) and the application converts it
/// to this type in [`HostQueue`] (`docs/platform-ports.md`, Target shapes 2 and 8).
///
/// The SDL drain decodes SDL's events into it ([`decode_host_event`]) and the loop consumes it in
/// ONE function (`run::ingest_host_event`), so a host that stops the application without SDL
/// (the hosted entry's `plx_host_stop`) arrives as the same [`HostEvent::Quit`] an `SDL_QUIT`
/// does and takes the same path, and a host with no SDL key pair sends a [`HostEvent::Key`] that
/// carries an [`Action`]. The pointer and the wheel are decoded here too (W5c), so what the loop reads
/// of a mouse or a Magic Remote is a [`HostEvent::Pointer`] in authored coordinates and a
/// [`HostEvent::Wheel`] count, never SDL's bytes or either layout's wheel offset.
///
/// There is no `Edit` event: nothing produces one. `SDL_TEXTEDITING` (the IME pre-edit) is read by
/// nothing and stays a log-only raw event, and the system keyboard's edit keys (caret, backspace,
/// clear-all) are ordinary [`HostEvent::Key`] actions. `Edit` arrives with its first producer (W9).
#[derive(Clone, Debug, PartialEq)]
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
    /// before it gets here (`plx_shell_sdl::text::decode`, the one place that knows the offsets).
    Text(String),
    /// One pointer event, at authored 1920x1080 coordinates (`plx_shell_sdl::event::ptr_xy`: the window's pixels
    /// converted exactly once, the identity while the drawable is the canvas). `edge` is what it did:
    /// moved, went down or came up. It is the event TYPE alone, for any button, as the loop has always
    /// read it. The plan's `buttons` and `shown` members are not carried, because today's SDL events
    /// give them no source the loop reads: whether a button is held is the loop's own state, set by
    /// the down and up edges (`Pointer::button_down`), and whether the pointer is shown is not in any
    /// SDL pointer event (LG's own hide arrives as a key, [`Action::PointerHidden`]; a show is a motion).
    Pointer { x: f32, y: f32, edge: PointerEdge },
    /// One tick of the wheel: its vertical count in the signed whole ticks the loop reads (a
    /// fractional trackpad tick that rounds to nothing is `0`, which the loop records and then
    /// ignores, as it always has). Decoded from whichever offset this host carries it at
    /// (`plx_shell_sdl::event::decode_wheel_dy`).
    Wheel { dy: i32 },
}

impl HostEvent {
    /// Is this the viewer's INPUT (a key, text, pointer or wheel event), as opposed to the window's
    /// (a lifecycle edge, quit)? The loop's activity bookkeeping asks it: input keeps a release-health
    /// session open and repaints as input damage, where the window's events are page damage. The same
    /// split [`is_input_event`] makes of an SDL event type, which `host_event_tests` pins them to.
    pub(crate) fn is_input(&self) -> bool {
        matches!(self, HostEvent::Key(_) | HostEvent::Text(_) | HostEvent::Pointer { .. } | HostEvent::Wheel { .. })
    }
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

    /// The press a decoded SDL key edge is: the raw pair (the carried wcode, else the desktop stand-in,
    /// [`wcode_of`]), its action, and the edge the fork's packed `state` names (low byte 1 is down, bit
    /// 0x100 is the auto-repeat; the same reading `bridge::key_input` makes of it). A key-up never
    /// carries the repeat bit here.
    fn from_decoded(state: u32, wcode: Option<u32>, sym: u32) -> Option<KeyPress> {
        let wcode = wcode_of(wcode, sym);
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

/// An SDL event decoded into a [`HostEvent`]: the shell's decode of the bytes (both layouts of every
/// event, `plx_shell_sdl::event::decode`) mapped onto the vocabulary the loop reads. `None` for
/// everything the shell leaves raw (the IME's pre-edit, and the SDL events nothing acts on), which the
/// loop reads raw.
#[inline]
pub(crate) fn decode_host_event(ev: &[u8]) -> Option<HostEvent> {
    host_event(plx_shell_sdl::event::decode(ev)?)
}

/// One decoded SDL event as a [`HostEvent`]. The only place a [`Decoded`] becomes the application's own
/// type, so the next platform's events arrive through a sibling of this function and not through SDL's
/// bytes (`docs/platform-ports.md`, Target shape 8).
fn host_event(decoded: Decoded) -> Option<HostEvent> {
    Some(match decoded {
        Decoded::Quit => HostEvent::Quit,
        Decoded::Key { state, wcode, sym } => HostEvent::Key(KeyPress::from_decoded(state, wcode, sym)?),
        Decoded::Text(text) => HostEvent::Text(text),
        Decoded::Pointer { x, y, edge } => HostEvent::Pointer { x, y, edge },
        Decoded::Wheel { dy } => HostEvent::Wheel { dy },
        Decoded::Lifecycle(edge) => HostEvent::Lifecycle(edge),
    })
}

/// What a platform's [`EventSink`](super::platform::EventSink) calls become: the [`HostEvent`]s the loop
/// ingests, in the order the platform made them. The one place a non-SDL platform's events become the
/// application's own type, beside [`host_event`] for SDL's (the two are the only constructors of a
/// [`HostEvent`], so the private event type has exactly two doors). A key goes through [`KeyPress::new`],
/// which refuses what is neither an action nor a pair, or an action and a pair that name two keys.
#[derive(Default)]
pub(crate) struct HostQueue(Vec<HostEvent>);

impl HostQueue {
    /// The queued events, oldest first.
    pub(crate) fn into_events(self) -> Vec<HostEvent> {
        self.0
    }
}

impl super::platform::EventSink for HostQueue {
    fn key(&mut self, action: Option<Action>, raw: Option<RawKey>, edge: Edge) -> bool {
        let Some(press) = KeyPress::new(action, edge, raw) else { return false };
        self.0.push(HostEvent::Key(press));
        true
    }
    fn text(&mut self, text: &str) {
        self.0.push(HostEvent::Text(text.to_owned()));
    }
    fn pointer(&mut self, x: f32, y: f32, edge: PointerEdge) {
        self.0.push(HostEvent::Pointer { x, y, edge });
    }
    fn wheel(&mut self, dy: i32) {
        self.0.push(HostEvent::Wheel { dy });
    }
    fn lifecycle(&mut self, edge: Lifecycle) {
        self.0.push(HostEvent::Lifecycle(edge));
    }
    fn quit(&mut self) {
        self.0.push(HostEvent::Quit);
    }
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
    use super::{encode_key, encode_key_repeat, wcode_of};
    use plx_ui::consts::{SDLK_DOWN, SDLK_RETURN, WCODE_BACK, WCODE_PAUSE};

    /// What the loop reads a key's bytes as: the shell's decode with this application's desktop
    /// stand-in applied where the bytes carry no wcode, the way `KeyPress::from_decoded` reads it.
    /// The shell's own tests grade the byte layout; these grade the pair that reaches the loop,
    /// stand-in priority included.
    fn decode_key(ev: &[u8]) -> (u32, u32, u32) {
        let (state, wcode, sym) = plx_shell_sdl::event::decode_key(ev);
        (state, wcode_of(wcode, sym), sym)
    }

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

    /// Everything the loop still reads raw stays raw: the IME's pre-edit (0x302, which nothing reads),
    /// the SDL events nothing acts on (terminating and low memory, which sit between quit and the
    /// background pair, and the window events), and the types either side of the pointer and wheel
    /// block (0x3ff and 0x404). Keys (0x300, 0x301), text (0x303), the pointer (0x400 to 0x402) and
    /// the wheel (0x403) are decoded: `key_event_tests` and `pointer_event_tests`.
    #[test]
    fn every_other_event_stays_on_the_raw_path() {
        for code in [
            0, 0x101, 0x102, 0x107, 0x200, 0x302, 0x3ff, 0x404, u32::MAX,
        ] {
            assert_eq!(decode_host_event(&event_of(code)), None, "{code:#x}");
        }
    }

    /// **`HostEvent::is_input` and `is_input_event` are one classification.** The loop asks the first
    /// of a decoded event and the second of the raw type, and `Dispatcher::input_scope` rests on the
    /// second, so a pointer or wheel event the first called window damage (or a lifecycle edge it called
    /// input) would repaint under the wrong owner. Every type that decodes is held to both: the window's
    /// are not input, the viewer's are. The IME's pre-edit (0x302) is input to the type table and decodes
    /// to nothing, so it has no `HostEvent` to compare.
    #[test]
    fn a_host_event_is_input_exactly_when_its_sdl_type_is() {
        let _serial = plx_base::testlock::serial();
        for (code, input) in [
            (0x100, false), (0x103, false), (0x104, false), (0x105, false), (0x106, false),
            (0x300, true), (0x301, true), (0x303, true),
            (0x400, true), (0x401, true), (0x402, true), (0x403, true),
        ] {
            let mut ev = event_of(code);
            if code == 0x300 || code == 0x301 {
                ev = super::encode_key(plx_ui::consts::SDLK_UP, 0, code == 0x300);
            }
            let event = decode_host_event(&ev).unwrap_or_else(|| panic!("{code:#x} decodes"));
            assert_eq!(event.is_input(), input, "{code:#x} as a HostEvent");
            assert_eq!(is_input_event(code), input, "{code:#x} as a type");
        }
    }
}

/// A pointer event and a wheel tick as a [`HostEvent`] (W5c): what the SDL decoder makes of the
/// bytes, in both layouts. The offsets are literals, written out here and not read back from the
/// decoder: they are the contract with SDL (and, for the pointer, with `docs/testing-player-pointer.md`'s
/// `offsetof` assertions).
#[cfg(test)]
mod pointer_event_tests {
    use super::{decode_host_event, encode_pointer, HostEvent, PointerEdge};

    const MOTION: u32 = 0x400;
    const DOWN: u32 = 0x401;
    const UP: u32 = 0x402;
    const WHEEL: u32 = 0x403;

    /// A pointer event as SDL lays it out, written by hand: `type` at +0, the window id at +8, the
    /// button at +16 and its state at +17, then `Sint32 x` at +20 and `Sint32 y` at +24. LG's fork and
    /// stock SDL2 agree on all of it (the keyboard event is the one that shifts), which is why the
    /// decoder has one reading for both.
    fn bytes(code: u32, button: u8, state: u8, x: i32, y: i32) -> [u8; 128] {
        let mut ev = [0u8; 128];
        ev[0..4].copy_from_slice(&code.to_ne_bytes());
        ev[8..12].copy_from_slice(&7u32.to_ne_bytes());
        ev[16] = button;
        ev[17] = state;
        ev[20..24].copy_from_slice(&x.to_ne_bytes());
        ev[24..28].copy_from_slice(&y.to_ne_bytes());
        ev
    }

    fn pointer(x: f32, y: f32, edge: PointerEdge) -> Option<HostEvent> {
        Some(HostEvent::Pointer { x, y, edge })
    }

    /// **The decode table.** Each of the three SDL types, at the corners and the middle of the canvas
    /// and just outside it (a drag past the edge reports negative and over-large positions), decodes
    /// to its edge and its position. The surface is the identity here (1920x1080, the default), so the
    /// position is what the bytes say; `ptr_xy`'s conversion off the identity is `plx_base::surface`'s.
    #[test]
    fn an_sdl_pointer_event_decodes_to_its_edge_and_its_position() {
        let _serial = plx_base::testlock::serial();
        for (code, edge) in [(MOTION, PointerEdge::Move), (DOWN, PointerEdge::Down), (UP, PointerEdge::Up)] {
            for (x, y) in [(0, 0), (960, 540), (1919, 1079), (1400, 870), (-3, 1085), (2000, -1)] {
                assert_eq!(decode_host_event(&bytes(code, 1, 1, x, y)), pointer(x as f32, y as f32, edge),
                    "{code:#x} at ({x}, {y})");
            }
        }
    }

    /// **The edge is the event type alone**, for any button and whatever its state byte says, as the
    /// loop has always taken it: a button-up carrying state 1, a button-down carrying state 0 and a
    /// motion with a button byte set are still an up, a down and a move. Bytes the decoder must not read
    /// (the window id, the bytes after `y`, the float where the wheel keeps its fraction) are noise here.
    #[test]
    fn the_edge_is_the_event_type_whatever_the_button_bytes_say() {
        let _serial = plx_base::testlock::serial();
        for button in [0u8, 1, 2, 3, 255] {
            for state in [0u8, 1] {
                for (code, edge) in [(MOTION, PointerEdge::Move), (DOWN, PointerEdge::Down), (UP, PointerEdge::Up)] {
                    let mut ev = bytes(code, button, state, 640, 360);
                    ev[28..64].fill(0xa5);
                    assert_eq!(decode_host_event(&ev), pointer(640.0, 360.0, edge), "{code:#x} button {button} state {state}");
                }
            }
        }
    }

    /// The bytes the loop's own synthesis writes (`encode_pointer`, which the FIFO's `ck:` `pm:` `pd:`
    /// `pu:` tokens and the replay driver push) decode to the position and edge they were built from.
    /// The encoder and the decoder shipped apart once for keys; this is what couples them for the pointer.
    #[test]
    fn the_synthesized_pointer_bytes_round_trip() {
        let _serial = plx_base::testlock::serial();
        for (code, edge) in [(MOTION, PointerEdge::Move), (DOWN, PointerEdge::Down), (UP, PointerEdge::Up)] {
            for (x, y) in [(0, 0), (1400, 870), (1919, 1079)] {
                assert_eq!(decode_host_event(&encode_pointer(code, x, y)), pointer(x as f32, y as f32, edge), "{code:#x}");
            }
        }
    }

    /// A wheel event as the television's SDL2 lays it out: `Sint32 y` at +20.
    fn wheel_lg(dy: i32) -> [u8; 128] {
        let mut ev = [0u8; 128];
        ev[0..4].copy_from_slice(&WHEEL.to_ne_bytes());
        ev[20..24].copy_from_slice(&dy.to_ne_bytes());
        ev
    }

    /// A wheel event as the simulator's stock SDL2 (sdl2-compat over SDL3) delivers it: the count in
    /// the float `preciseY` at +32, with the legacy integer at +20 left at zero.
    fn wheel_stock(precise_y: f32) -> [u8; 128] {
        let mut ev = [0u8; 128];
        ev[0..4].copy_from_slice(&WHEEL.to_ne_bytes());
        ev[32..36].copy_from_slice(&precise_y.to_ne_bytes());
        ev
    }

    fn wheel(dy: i32) -> Option<HostEvent> {
        Some(HostEvent::Wheel { dy })
    }

    /// **Each layout's wheel, and the one this build does not read.** The television's integer at +20
    /// is the count in the default build and is ignored in the simulator's; the simulator's float at +32
    /// is the count there (rounded to whole ticks) and is ignored on the television. A decoder that
    /// read both, or the wrong one, fails one of the two assertions in each build.
    #[test]
    fn a_wheel_tick_is_read_from_the_offset_this_hosts_sdl_carries_it_at() {
        for dy in [-40, -3, -1, 1, 2, 120] {
            if cfg!(feature = "hostsim") {
                assert_eq!(decode_host_event(&wheel_stock(dy as f32)), wheel(dy), "stock layout, {dy}");
                assert_eq!(decode_host_event(&wheel_lg(dy)), wheel(0), "the television's field is not this host's: {dy}");
            } else {
                assert_eq!(decode_host_event(&wheel_lg(dy)), wheel(dy), "LG layout, {dy}");
                assert_eq!(decode_host_event(&wheel_stock(dy as f32)), wheel(0), "the simulator's field is not read here: {dy}");
            }
        }
    }

    /// **The `+20` case.** `remote_synth_wheel`, the `wheel:<dy>` token's pusher, writes the television's
    /// shape (`Sint32 y` at +20) unconditionally, so the bytes it builds decode to the count on the
    /// television and to ZERO in the simulator (where `SDL_PushEvent` hands the value back at +32; the
    /// decoder's doc has the measurement). The zero is not a bug: it is why the decoder reads the other
    /// offset there. It is pinned so a "fix" that reads `+20` everywhere shows up here, in the simulator
    /// run, as a count that is no longer zero.
    #[test]
    fn the_television_shaped_wheel_the_fifo_token_builds_decodes_to_zero_in_the_simulator() {
        let mut token_shape = [0u8; 128];
        token_shape[0..4].copy_from_slice(&WHEEL.to_ne_bytes());
        token_shape[20..24].copy_from_slice(&(-2i32).to_ne_bytes());
        let expected = if cfg!(feature = "hostsim") { 0 } else { -2 };
        assert_eq!(decode_host_event(&token_shape), wheel(expected));
    }

    /// A fractional trackpad tick keeps the sign of what it rounds to, and a half tick or less is no
    /// tick (the loop records a zero and ignores it). Only the simulator's layout carries a fraction.
    #[cfg(feature = "hostsim")]
    #[test]
    fn a_fractional_trackpad_tick_rounds_to_whole_ticks() {
        for (precise, dy) in [(-0.4, 0), (0.4, 0), (-0.6, -1), (0.6, 1), (-1.4, -1), (2.5, 3), (-40.0, -40)] {
            assert_eq!(decode_host_event(&wheel_stock(precise)), wheel(dy), "{precise}");
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
    /// reader of the loop reads the action before W5d, so a wrong `action_of` leaves every native
    /// effect test green: this table, `input.rs`'s decode literals and the hosted door's round trip
    /// (`KeyPress::new`) are what see it. `native_key_effect_*` in `run.rs` grade what the consumers
    /// do with the pair.
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

/// A platform's `EventSink` calls become the same `HostEvent`s the SDL decode makes of the same events
/// (the one conversion a non-SDL platform's input goes through), in the order they were made, and a key
/// the application cannot name is refused, not queued.
#[cfg(test)]
mod host_queue_tests {
    use super::super::platform::EventSink;
    use super::{decode_host_event, encode_key, Action, Edge, HostEvent, HostQueue, KeyPress, Lifecycle, PointerEdge, RawKey};
    use plx_ui::consts::raw_pair;

    fn event_of(code: u32) -> [u8; 128] {
        let mut ev = [0u8; 128];
        ev[..4].copy_from_slice(&code.to_ne_bytes());
        ev
    }

    #[test]
    fn quit_and_the_lifecycle_edges_queue_what_the_sdl_decode_makes_of_their_events() {
        let mut queue = HostQueue::default();
        queue.quit();
        queue.lifecycle(Lifecycle::WillBackground);
        queue.lifecycle(Lifecycle::DidBackground);
        queue.lifecycle(Lifecycle::WillForeground);
        queue.lifecycle(Lifecycle::DidForeground);
        let decoded: Vec<HostEvent> =
            [0x100, 0x103, 0x104, 0x105, 0x106].iter().map(|&code| decode_host_event(&event_of(code)).expect("decoded")).collect();
        assert_eq!(queue.into_events(), decoded);
    }

    #[test]
    fn a_raw_pair_with_or_without_its_action_queues_the_press_the_sdl_decode_makes_of_those_bytes() {
        let (sym, wcode) = raw_pair(Action::Ok);
        let pair = RawKey { sym, wcode };
        let Some(decoded) = decode_host_event(&encode_key(sym, wcode, true)) else { panic!("a key event") };
        let mut queue = HostQueue::default();
        assert!(queue.key(None, Some(pair), Edge::Down), "a pair alone is a press");
        assert!(queue.key(Some(Action::Ok), Some(pair), Edge::Down), "and so is the pair with the action it names");
        assert_eq!(queue.into_events(), vec![decoded.clone(), decoded]);
    }

    #[test]
    fn an_action_alone_queues_a_press_whose_pair_is_the_one_a_remote_sends() {
        let mut queue = HostQueue::default();
        assert!(queue.key(Some(Action::Back), None, Edge::Up));
        let want = KeyPress::new(Some(Action::Back), Edge::Up, None).expect("Back names a key");
        let events = queue.into_events();
        assert_eq!(events, vec![HostEvent::Key(want)]);
        let HostEvent::Key(press) = &events[0] else { panic!("a key") };
        assert_eq!(press.pair(), RawKey { sym: raw_pair(Action::Back).0, wcode: raw_pair(Action::Back).1 });
    }

    #[test]
    fn a_key_the_application_cannot_name_is_refused_and_queues_nothing() {
        let (sym, wcode) = raw_pair(Action::Ok);
        let mut queue = HostQueue::default();
        assert!(!queue.key(None, None, Edge::Down), "neither an action nor a pair");
        assert!(!queue.key(Some(Action::Digit(10)), None, Edge::Down), "no pair spells Digit(10)");
        assert!(!queue.key(Some(Action::Back), Some(RawKey { sym, wcode }), Edge::Down), "an action and a pair of two keys");
        assert!(queue.into_events().is_empty());
    }

    #[test]
    fn text_pointer_and_wheel_queue_as_given_and_keep_their_order() {
        let mut queue = HostQueue::default();
        queue.text("wal");
        queue.pointer(960.0, 540.0, PointerEdge::Down);
        queue.wheel(-2);
        queue.text("");
        queue.pointer(1.0, 2.0, PointerEdge::Up);
        assert_eq!(
            queue.into_events(),
            vec![
                HostEvent::Text("wal".into()),
                HostEvent::Pointer { x: 960.0, y: 540.0, edge: PointerEdge::Down },
                HostEvent::Wheel { dy: -2 },
                HostEvent::Text(String::new()),
                HostEvent::Pointer { x: 1.0, y: 2.0, edge: PointerEdge::Up },
            ]
        );
    }
}
