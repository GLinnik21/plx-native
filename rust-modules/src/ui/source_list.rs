//! **The Sources ROW MODEL** — one list of libraries grouped by server, drawn on two surfaces.
//!
//! The Library toolbar's Source panel (`ui::library`, deliverable A) and the first-run route that
//! asks the same question before Home (`ui::onboard`, deliverable F) show the SAME list: your
//! servers, each server's libraries, and whether each one feeds Home. The canvas says so in as
//! many words — F "is the same list as A's On Home level, in the flow's own frame rather than a
//! panel" — so it is one builder, and the two screens differ only in the frame around it and in
//! whether the panel's own tail rides along.
//!
//! Building it twice would have been the ordinary thing to do and the wrong one: the two would
//! have drifted on exactly the details that make the list readable — which column carries a mark,
//! which carries a word, what an unreachable group says and where it says it — and each drift
//! would read as a bug on whichever screen you saw second.
//!
//! Pure over its inputs, so it is host-testable without a live section table (which is also why
//! `browse` hands out owned [`SrcGroup`](crate::browse::SrcGroup)/[`SrcRow`](crate::browse::SrcRow)
//! projections rather than borrows of its statics). [`Add`] keeps that property: the panel owns the
//! value, this module only turns it into rows, and the two statics at the bottom carry a request
//! ACROSS threads rather than holding any of the panel's state.
use crate::browse::{SourceState, SrcGroup};
use crate::plex::probe::Location;
use crate::ui::table::{Row, Section};
use std::sync::Mutex;

/// The two levels of the Sources panel, swapped by the pills at its top — the same swap the
/// player's track menu makes between Audio and Subtitles.
///
/// They differ in MEDIUM as well as in position, and that is the design's rule: a **mark** says
/// where you are, a **word** says what is set, and no row ever says both. So Browse draws one tick
/// and no words, On Home draws every row's word and no ticks — neither level mirrors the other's
/// marks.
///
/// The first-run route is [`Level::OnHome`] and nothing else: it has no current library to point
/// at, because it runs before there is a Library screen to have been on.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Level {
    /// a picker: one tick, on the library you are looking at; OK closes the panel
    Browse,
    /// toggles: `On`/`Off` at the trailing edge; OK flips and the panel stays open
    OnHome,
}

/// What a row of the Sources list does. Built beside the rows, indexed by the same global row
/// index the `TableView` reports — including the separator, which does nothing and can never be
/// focused, but still occupies an index.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum SrcAction {
    None,
    /// browse (Browse level) or pin (On Home level) this section
    Library(usize),
    Recheck,
    /// open the manual-address field ([`Add`]) — the panel's escape hatch for a server plex.tv
    /// never named
    AddOpen,
    /// OK on the address row itself: raise the keyboard on a field that is already open.
    ///
    /// **The same handler as [`Self::AddOpen`], and still its own variant.** These actions are the
    /// panel's index→meaning map — what a test reads to say which ROW it is looking at — and the
    /// two rows are not the same row: one is a closed drill-in that is always pressable, the other
    /// is the field itself, which goes inert the moment a request is in flight. Folding them would
    /// erase exactly the distinction the tests exist to pin.
    AddEdit,
    /// hand what has been typed to the backend ([`submit`])
    AddSubmit,
}

/// Does the list end with the panel's own tail — "Check for new shares" and the manual-address row?
///
/// The panel's does. **The first-run route's does not**, and that is a design call rather than an
/// omission: a share that arrives later must not reopen a first-run screen, so there is nothing
/// for a refresh to be FOR there — it appears unpinned in the Library chip's list, "which is where
/// 'Check for new shares' already lives" (`Shared Sources.dc.html` §F). The manual address follows
/// the same rule for a stronger reason: the first-run route runs when a roster HAS landed, so it
/// is never the screen a user with no discovered server is looking at.
///
/// It carries the [`Add`] state rather than being a bare marker, so the panel's tail is one
/// argument and `ui::onboard`'s call site — which has no field and never will — did not have to
/// learn about a state it cannot reach.
#[derive(Clone, Copy, Debug)]
pub(crate) enum Tail<'a> {
    Panel(&'a Add),
    None,
}

/// The word a group's state is said in — `None` for the two states that say nothing.
///
/// [`SourceState::NotProbed`] says nothing because nothing has gone wrong: a group only reaches
/// this list once its libraries have landed, so "nobody has dialled" here means the app has talked
/// to the server and simply not GRADED the connection. That is a fact about our own instrumentation
/// and stating it would put a warning on a working server.
///
/// [`SourceState::Reachable`] says nothing because it is the unremarkable case. What a working
/// group may still have to say is its TIER — see [`tier_word`].
fn state_word(s: SourceState) -> Option<&'static str> {
    match s {
        SourceState::NotProbed | SourceState::Reachable => None,
        SourceState::Unauthorized => Some(UNAUTHORIZED),
        SourceState::Unreachable => Some(UNREACHABLE),
    }
}

/// The server answered and **refused our token**: a sharing-grant problem, never a network one.
///
/// It has to read as a different KIND of fault from [`UNREACHABLE`], because it has a different
/// remedy and the wrong word costs the user an evening — "not reachable" sends somebody to look at
/// a router for something no router was ever part of. The remedy is in this same panel, two rows
/// down: *Check for new shares* is the `/api/v2/resources` refetch that reissues the per-(user,
/// server) `accessToken`, which is why this run does not have to carry an instruction as well.
const UNAUTHORIZED: &str = "Not authorized";
/// Did not answer at all — refused, timed out, or unresolvable.
const UNREACHABLE: &str = "Not reachable";

/// The word a WORKING group's connection tier is said in — `None` when there is nothing worth
/// saying.
///
/// [`Location::Local`] is silent by the same rule that keeps a healthy state silent: it is what a
/// server on your own network is expected to be, and annotating every group with it would be noise
/// on the common panel. The other two each buy the user an explanation they cannot get anywhere
/// else:
///
/// - **Relay** is the one this list exists to say. A relay connection is a ~2 Mbit/s tunnel the
///   server transcodes down to fit (`plex::probe`'s rule 3), so a library that plays at DVD quality
///   over a 200 Mbit link reads as a broken server unless something on screen says why.
/// - **Remote** is quieter but the same shape: it explains a slower first frame and a transcode on
///   a file that direct-plays at home.
fn tier_word(t: Location) -> Option<&'static str> {
    match t {
        Location::Local => None,
        Location::Remote => Some("Remote"),
        Location::Relay => Some("Relay"),
    }
}

/// May this group's libraries be OPENED? The question [`Section::dim`] actually asks.
///
/// **[`SrcGroup::reachable`] is still the read for three of the four states, and wrong for the
/// fourth** — which is the whole reason the state widened. That method answers the old two-state
/// question, so `NotProbed` comes back `true` (correctly: a group nobody has dialled must not open
/// dimmed) and so does `Unauthorized`, not because it is browsable but because a `bool` had no way
/// to say otherwise. A server that answered and refused our token has nothing behind it, so it dims
/// exactly as an unreachable one does; only the WORD differs, because only the remedy differs.
///
/// Written as an exhaustive match rather than `reachable() && state != Unauthorized`, so that a
/// fifth state has to be given an answer here instead of quietly inheriting one.
fn usable(g: &SrcGroup) -> bool {
    match g.state {
        SourceState::NotProbed | SourceState::Reachable | SourceState::Unreachable => g.reachable(),
        SourceState::Unauthorized => false,
    }
}

/// The group header's ACCESSORY: `[state] · [tier] · handle`, in that order.
///
/// **The state leads**, and that is load-bearing rather than cosmetic. The accessory is the last
/// run on the header line and is elided from the right ([`Section::accessory`]), so whatever is
/// last is what gives way — leading with the state means a 34-character plex.tv handle truncates
/// instead of the fact that the server is not answering.
///
/// The TIER is only spoken for a group that is working. On a dead one it is at best stale and at
/// worst misleading: "Not reachable · Relay · friend" reads as three problems where there is one,
/// and the tier that failed is not a fact about the server the user is being asked to look at.
fn accessory(g: &SrcGroup) -> String {
    let tier = (g.state == SourceState::Reachable).then(|| g.tier.and_then(tier_word)).flatten();
    let runs = [state_word(g.state), tier, (!g.handle.is_empty()).then_some(g.handle.as_str())];
    runs.iter().flatten().copied().collect::<Vec<_>>().join(" \u{b7} ")
}

/// Build the list.
///
/// The levels never mirror each other's marks. **Browse** is a picker: one tick, on the library you
/// are looking at, and no words. **On Home** is a set of switches: every row states its value as
/// the word `On`/`Off` at the trailing edge, and no ticks. A mark says where you are, a word says
/// what is set, and no row is allowed to say both.
///
/// Returns the sections beside one [`SrcAction`] per global row index — the separator included,
/// which does nothing but still occupies an index.
pub(crate) fn sections(
    level: Level,
    groups: &[crate::browse::SrcGroup],
    rows: &[crate::browse::SrcRow],
    tail: Tail,
) -> (Vec<Section>, Vec<SrcAction>) {
    let mut out: Vec<Section> = Vec::new();
    let mut acts: Vec<SrcAction> = Vec::new();
    for (gi, g) in groups.iter().enumerate() {
        let mine = rows.iter().filter(|r| r.src == gi);
        // a server whose libraries we have never learned contributes no group at all — a header
        // over nothing reads as broken, and D's answer for a source that cannot be reached on a
        // FIRST run is absence
        if mine.clone().next().is_none() {
            continue;
        }
        // the header is the MACHINE, its accessory the PERSON — the one place in the app a machine
        // is named, and the reason every other surface can say only the handle. A group that is not
        // working also SAYS so there, and says it FIRST — see [`accessory`]. It is a state in the
        // same register as the rows' own `On`/`Off`.
        let mut sec = Section::new(g.name.clone()).accessory(accessory(g)).dim(!usable(g));
        for r in mine {
            let mut row = Row::new(r.title.clone());
            row = match level {
                Level::Browse => row.checked(r.current).detail(r.count_line.clone()),
                Level::OnHome => row
                    .toggle(r.pinned)
                    // the LAST pinned library: the value dims, the label keeps live ink, and the
                    // sub-line states the rule. Not the whole row — dim means unavailable, and this
                    // is the library that works.
                    .value_dim(r.last_pinned)
                    .detail(if r.last_pinned {
                        "Home needs one library".to_string()
                    } else {
                        r.count_line.clone()
                    }),
            };
            acts.push(SrcAction::Library(r.section));
            sec = sec.row(row);
        }
        out.push(sec);
    }
    // …and the rows that are not libraries, last, under a separator. They ride BOTH levels, which
    // is what keeps their row counts — and therefore the panel's height — identical.
    let Tail::Panel(add) = tail else {
        return (out, acts);
    };
    // **The tail survives an EMPTY list**, and that is the case it matters most in. It used to hang
    // off `out.last_mut()`, so a roster where nothing has libraries yet — a stale or empty
    // `/api/v2/resources`, one server that has stopped answering — drew no tail at all: no way to
    // re-check the shares, and no way to type an address, in precisely the state both rows exist
    // for. With no group to append to it becomes its own headerless section, and the separator goes
    // with the group above rather than being drawn over nothing.
    let sep = out.last().is_some();
    let last = match out.last_mut() {
        Some(s) => s,
        None => {
            out.push(Section::new(""));
            out.last_mut().expect("just pushed")
        }
    };
    if sep {
        last.rows.push(Row::separator());
        acts.push(SrcAction::None);
    }
    // no leading glyph, deliberately: on the Browse level that column carries the picker's
    // tick, and an action mark in it would be a second grammar for one column
    last.rows.push(Row::new("Check for new shares"));
    acts.push(SrcAction::Recheck);
    for (row, act) in add.rows() {
        last.rows.push(row);
        acts.push(act);
    }
    (out, acts)
}

/// Where the cursor sits. On OPEN it lands on the library you are browsing; on a rebuild — a level
/// swap, a pin flip, a source's libraries landing — it HOLDS, because the two levels list the same
/// rows in the same order and the design's whole claim about the swap is that nothing moves.
pub(crate) fn sel(rows: &[crate::browse::SrcRow], keep: Option<i32>) -> i32 {
    keep.unwrap_or_else(|| rows.iter().position(|r| r.current).map(|i| i as i32).unwrap_or(0))
}

// ---- the manual address ----------------------------------------------------------------------
//
// **Why the app needs one at all.** Everything else in this list arrives from plex.tv: the roster
// names the servers, and a server nobody's roster names cannot be reached by this app at any
// address. That is fine until plex.tv is exactly what is not working — a signed-in account whose
// `/api/v2/resources` is empty or stale, a server whose owner has not finished claiming it, a
// network where plex.tv resolves and the server does not. The official webOS client has manual
// entry for those; we had nothing, and "nothing" here means the user cannot get to their own
// server sitting on the same switch.
//
// It is a ROW of this panel and not a screen. There is no `Route`, no popover of its own and no
// full-screen sheet (`docs/parity-gaps.md` records the menu idiom): the panel is already a popover
// over a `TableView`, the field is two of its rows, and the rows ride the tail beside "Check for
// new shares" — which is the other way this list answers "my server is missing".

/// The default PMS port, used when the typed address names none. It is not a secret and not a
/// discovery: every Plex server listens here unless its owner moved it.
const DEFAULT_PORT: u16 = 32400;

/// How long the panel waits for the backend before it says nothing answered.
///
/// Generous on purpose: a wrong address on a real network fails by TIMING OUT, and a field that
/// gives up faster than the transport does would report a failure while the dial was still in
/// flight and then have to take it back.
const WAIT_MS: f32 = 20_000.0;

/// The address row's placeholder — the SHAPE of the answer, in the position the answer will go.
const PLACEHOLDER: &str = "Server address";
/// The sub-line while there is nothing usable typed. It states the grammar rather than scolding:
/// this is the same line before the first character and after a wrong one, because "what should I
/// type" is the question in both cases.
///
/// **Short because the row is 640px wide and a sub-line ELIDES.** The first draft read "IP address
/// or hostname, and :port if it is not 32400" and came back off the simulator cut at "…if it is not
/// 3…" — a hint whose one number is the half that disappears. The port is stated by the row itself
/// the moment the address parses ([`Add::detail`]), which is where that fact belongs anyway: as a
/// read-out of what will actually be dialled, not as an instruction.
const HINT: &str = "IP address, or address:port";

/// One address, parsed. Everything the backend needs about what was typed and nothing it does not
/// — in particular no URL, because the transport builds its own and the two spellings drifting is
/// how a probe ends up dialling something the user never typed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Endpoint {
    /// a dotted quad, a bracket-less IPv6 literal, or a hostname
    pub(crate) host: String,
    pub(crate) port: u16,
    /// the user typed an `https://` scheme. Kept rather than dropped: it is the one thing about
    /// their intent the address itself cannot carry, and silently discarding it would send a
    /// cleartext probe at a server whose owner set `httpsRequired`.
    pub(crate) secure: bool,
}

/// What the address row is doing.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub(crate) enum Phase {
    /// closed — the tail shows one "Add a server" row and nothing else
    #[default]
    Closed,
    /// the field is open and taking characters
    Editing,
    /// handed to the backend; nothing has answered yet
    Waiting,
    /// nobody answered, or the backend said no
    Failed,
}

/// The manual-address row's whole state. **The panel owns the value** — this module builds rows
/// from it and never stores one, which is what keeps [`sections`] pure and host-testable.
#[derive(Clone, Debug, PartialEq, Default)]
pub(crate) struct Add {
    /// exactly what has been typed, in typing order
    text: String,
    phase: Phase,
    /// milliseconds spent in [`Phase::Waiting`] — see [`WAIT_MS`]
    waited: f32,
    /// which request [`Add::tick`] is waiting on; meaningful only in [`Phase::Waiting`]
    pending: Option<ReqId>,
}

impl Add {
    /// A closed field. `const` because the panel holds one in a `static` and `Default` is not
    /// const-callable there.
    pub(crate) const fn new() -> Self {
        Self { text: String::new(), phase: Phase::Closed, waited: 0.0, pending: None }
    }
    /// Is the field open? While it is, the panel routes typing to it and BACK closes IT rather than
    /// the panel.
    pub(crate) fn is_open(&self) -> bool {
        self.phase != Phase::Closed
    }
    /// Is the field taking characters right now? [`Phase::Waiting`] is not: a request is in flight
    /// and letting it change under the backend would mean answering about an address nobody sent.
    pub(crate) fn is_editing(&self) -> bool {
        matches!(self.phase, Phase::Editing | Phase::Failed)
    }
    pub(crate) fn text(&self) -> &str {
        &self.text
    }
    pub(crate) fn phase(&self) -> Phase {
        self.phase
    }
    /// Open the field, keeping whatever was typed before — a user who backed out to look at the
    /// list and came back has not asked to retype their address.
    pub(crate) fn open(&mut self) {
        if self.phase == Phase::Closed {
            self.phase = Phase::Editing;
        }
    }
    /// Close it and forget the attempt. BACK's answer, and the panel's on close.
    ///
    /// **A request already posted is NOT withdrawn**, deliberately: the user asked for that server,
    /// and a dial already in flight succeeding means the group simply appears in this list — which
    /// is the outcome they pressed for. What the close does end is this field's interest in the
    /// answer, and [`submit`] clears the mailbox before posting so a reply nobody is waiting for
    /// cannot be read as the NEXT attempt's.
    pub(crate) fn close(&mut self) {
        *self = Self::default();
    }
    /// A commit from the television's keyboard. Appends — there is no caret here, deliberately:
    /// `ui::search` owns the app's one full text field (caret, `◀`/`▶`, word replacement) and a
    /// second copy of it inside a table row would be the drift this directory exists to prevent.
    /// An address is a short token typed left to right, and the panel's own delete and clear keys
    /// cover the rest.
    pub(crate) fn push(&mut self, s: &str) {
        if !self.is_editing() {
            return;
        }
        self.phase = Phase::Editing; // typing after a failure is a new attempt
        self.text.push_str(s);
    }
    /// The keyboard's delete key ([`crate::ui::consts::SDLK_BACKSPACE`]) — one CHARACTER, not one
    /// byte, so a multi-byte hostname does not lose half of itself.
    pub(crate) fn backspace(&mut self) {
        if self.is_editing() {
            self.phase = Phase::Editing;
            self.text.pop();
        }
    }
    /// The keyboard's **Clear all** ([`crate::ui::consts::SDLK_CLEAR`]).
    pub(crate) fn clear(&mut self) {
        if self.is_editing() {
            self.phase = Phase::Editing;
            self.text.clear();
        }
    }
    /// The endpoint this field would submit, if what is typed is usable at all.
    pub(crate) fn endpoint(&self) -> Option<Endpoint> {
        parse_endpoint(&self.text)
    }
    /// Hand the address to the backend and start waiting. A no-op on an address that does not
    /// parse, which is also why the action row is inert then — the two must not be able to
    /// disagree.
    pub(crate) fn submit(&mut self) -> bool {
        let Some(ep) = self.endpoint() else { return false };
        self.pending = Some(submit(ep));
        self.phase = Phase::Waiting;
        self.waited = 0.0;
        true
    }
    /// One frame. Drains the backend's answer and ages a request nobody picked up.
    ///
    /// Returns whether anything changed, so the caller rebuilds the panel — and repaints it. A
    /// settled popover stops presenting (`ui::idle`), so a landing that does not say it moved
    /// arrives invisibly until the next key press.
    pub(crate) fn tick(&mut self, dt: f32) -> bool {
        let Some(want) = self.pending.filter(|_| self.phase == Phase::Waiting) else {
            return false;
        };
        match take_answer(want) {
            Some(true) => {
                // registered: the new server's group appears in the list itself on the next
                // rebuild, so the field has nothing left to say and gets out of the way
                self.close();
                return true;
            }
            Some(false) => {
                self.phase = Phase::Failed;
                return true;
            }
            None => {}
        }
        self.waited += dt * 1000.0;
        if self.waited >= WAIT_MS {
            self.phase = Phase::Failed;
            return true;
        }
        false
    }

    /// The tail rows this field contributes, with their actions.
    ///
    /// **Closed it is one row; open it is always two**, and the count holding still is deliberate:
    /// the panel is a popover whose height is measured from its rows, so a sub-line that came and
    /// went as you typed would resize the panel under the cursor. The address row therefore always
    /// carries a sub-line and the action row is always present, dim rather than absent when there
    /// is nothing to press.
    fn rows(&self) -> Vec<(Row, SrcAction)> {
        if self.phase == Phase::Closed {
            // the drill-in chevron, by the same rule `ui::account_menu` uses: a row that leads
            // somewhere carries it, a row that acts in place ("Check for new shares") does not
            return vec![(Row::new("Add a server").chevron(true), SrcAction::AddOpen)];
        }
        let typed = !self.text.is_empty();
        let field = Row::new(if typed { self.text.clone() } else { PLACEHOLDER.to_string() })
            // the placeholder is not the user's text, so it is not drawn as though it were
            .dim(!typed)
            .detail(self.detail());
        let ready = self.endpoint().is_some();
        let action = match self.phase {
            // a failed attempt keeps its address on screen and offers the app's own retry verb
            Phase::Failed => Row::new("Try again").dim(!ready),
            _ => Row::new("Connect").dim(!ready || self.phase == Phase::Waiting),
        };
        let act = if ready && self.phase != Phase::Waiting { SrcAction::AddSubmit } else { SrcAction::None };
        // …and the address row is inert in flight for the same reason it stops taking characters:
        // OK there raises the keyboard, and a keyboard over a field that refuses input is worse
        // than no keyboard at all.
        let field_act = if self.is_editing() { SrcAction::AddEdit } else { SrcAction::None };
        vec![(field, field_act), (action, act)]
    }

    /// The address row's sub-line — the only place this field can say anything.
    fn detail(&self) -> String {
        match (self.phase, self.endpoint()) {
            (Phase::Waiting, _) => "Connecting\u{2026}".to_string(),
            // no address is echoed back: it is already on the row above, and a read-out is
            // something a user photographs (`ui::library`'s redaction rule for the failure block)
            (Phase::Failed, _) => "No answer from that address".to_string(),
            // the port is the one thing about the request the typed text does not show, and the
            // default is exactly what a user gets wrong silently
            (_, Some(ep)) => format!("Port {}", ep.port),
            (_, None) => HINT.to_string(),
        }
    }
}

/// Parse a typed address into an [`Endpoint`], or `None` if it is not one.
///
/// Deliberately permissive about the SHAPE a user types and strict about what comes out: a scheme
/// and a trailing slash are accepted and folded away (people paste URLs), a port is optional, and a
/// bracketed IPv6 literal is unwrapped. What it will not do is guess — an empty host, a port that
/// is not a number or is zero, a space, or a path is `None`, and the panel's action row is inert
/// while it is.
pub(crate) fn parse_endpoint(s: &str) -> Option<Endpoint> {
    let s = s.trim();
    let (secure, rest) = match s {
        _ if has_prefix_ci(s, "https://") => (true, &s[8..]),
        _ if has_prefix_ci(s, "http://") => (false, &s[7..]),
        _ => (false, s),
    };
    let rest = rest.trim_end_matches('/');
    if rest.is_empty() || rest.contains('/') || rest.contains(char::is_whitespace) {
        return None;
    }
    // `[::1]:32400` — the brackets exist precisely because a v6 literal is all colons, so the host
    // has to be cut on the bracket before any port split can be attempted
    let (host, port_s) = if let Some(end) = rest.strip_prefix('[').and_then(|r| r.find(']').map(|i| i + 1)) {
        let (h, tail) = rest.split_at(end + 1);
        (&h[1..end], tail.strip_prefix(':'))
    } else {
        match rest.rsplit_once(':') {
            // an unbracketed literal with several colons is a bare IPv6 address, not host:port
            Some(_) if rest.matches(':').count() > 1 => (rest, None),
            Some((h, p)) => (h, Some(p)),
            None => (rest, None),
        }
    };
    let port = match port_s {
        Some(p) => p.parse::<u16>().ok().filter(|p| *p > 0)?,
        None => DEFAULT_PORT,
    };
    if host.is_empty() {
        return None;
    }
    // A colon in the HOST can only be an IPv6 literal — everything else that could carry one has
    // already been cut off above. So the two cases are graded by two different rules rather than by
    // one whitelist that lets `::32400` through as a machine name.
    let ok = if host.contains(':') {
        looks_like_v6(host)
    } else {
        host.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_'))
    };
    ok.then(|| Endpoint { host: host.to_string(), port, secure })
}

/// ASCII case-insensitive prefix test — `str::starts_with` is case-SENSITIVE and a pasted `HTTP://`
/// is the same address.
///
/// **`str::get`, never `&s[..n]`.** This runs on EVERY KEYSTROKE (the row's sub-line re-parses to
/// state the port), and a byte-index slice panics when `n` lands mid-codepoint — so a user typing a
/// hostname in Cyrillic or Japanese would take the app down from inside the SDL event loop the
/// moment their 7th or 8th byte was the middle of a character. `get` returns `None` on a
/// non-boundary instead, which is exactly the "this is not that prefix" answer.
fn has_prefix_ci(s: &str, p: &str) -> bool {
    s.get(..p.len()).is_some_and(|head| head.eq_ignore_ascii_case(p))
}

/// Could this run be a bare IPv6 literal? The test that decides whether a multi-colon string is one
/// address or a `host:port` pair that has gone wrong.
///
/// Without it the host whitelist accepted `:` unconditionally, so anything with two colons was
/// taken verbatim: `"::32400"` — one colon too many before a port — parsed as a HOST called
/// `::32400`, and the Connect row went live on it.
///
/// Deliberately a SHAPE test and not a parser: hex groups of one to four digits, separated by
/// colons, with at most one `::` run. That is enough to reject a typo and cheap enough to run on
/// every keystroke; a literal that passes here and is still not routable is the transport's to
/// discover, exactly as a mistyped hostname is.
fn looks_like_v6(s: &str) -> bool {
    if s.matches("::").count() > 1 {
        return false;
    }
    s.split(':').all(|g| g.is_empty() || (g.len() <= 4 && g.chars().all(|c| c.is_ascii_hexdigit())))
}

// ---- THE SEAM --------------------------------------------------------------------------------
//
// **The panel's job ends at a validated [`Endpoint`].** Dialling it, verifying that `/identity`
// answers with a `machineIdentifier`, merging that identifier against the servers the roster
// already named, obtaining a token and registering the result belongs to the transport lane — it
// is worker-thread work with a socket in it, and none of it can be host-tested from a row model.
//
// So the two halves meet at two mailboxes and nothing else. The panel posts one request with
// [`submit`] and shows `Connecting…`; whoever owns the dial takes it with [`take_request`], does
// the work off the main thread, and answers once with [`report`].
//
// **Nothing takes the request today**, and the field is honest about exactly that: it ages out
// after [`WAIT_MS`] and says nothing answered, which is literally true of a request nobody picked
// up and is also the right read-out once somebody does and the address is wrong.
//
// `Mutex` rather than the `static mut` this directory usually reaches for, because this is the one
// piece of the panel that is NOT main-thread-only — the backend answers from a worker.

/// Which attempt an answer belongs to.
///
/// **Clearing the mailbox at [`submit`] is not enough on its own**, and the gap is exactly one
/// worker: a dial of the PREVIOUS address is still in flight while the user corrects a typo and
/// presses Connect again, and its `report` lands in the window between the clear and the next
/// [`Add::tick`]. Without an id the panel reads that verdict as this address's — "no answer" for a
/// server that was never dialled, or worse, a success. The id is what makes a late answer
/// identifiable as late.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) struct ReqId(u64);

/// panel → backend: one pending address, replaced rather than queued (a second submit is a
/// correction of the first, not another server).
static REQUEST: Mutex<Option<(ReqId, Endpoint)>> = Mutex::new(None);
/// backend → panel: did that request's address become a registered server?
static ANSWER: Mutex<Option<(ReqId, bool)>> = Mutex::new(None);
/// The last id handed out. Monotone, so an id is never reused and a late answer can only ever be
/// STALE — never a collision with a later attempt.
static NEXT_ID: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

/// Post an address for the backend to dial, returning the id its answer must carry.
fn submit(ep: Endpoint) -> ReqId {
    let id = ReqId(NEXT_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed));
    if let Ok(mut g) = ANSWER.lock() {
        *g = None; // an answer nobody is waiting for is not this request's
    }
    if let Ok(mut g) = REQUEST.lock() {
        *g = Some((id, ep));
    }
    id
}

/// **The backend's half.** Take the address the user typed, if there is one waiting, together with
/// the id [`report`] must quote back.
#[cfg_attr(
    not(test),
    expect(dead_code, reason = "seam: the transport lane's dial calls this, and this attribute goes with it")
)]
pub(crate) fn take_request() -> Option<(ReqId, Endpoint)> {
    REQUEST.lock().ok().and_then(|mut g| g.take())
}

/// **The backend's half.** Answer the one request that was taken: `true` once the server is
/// registered and its libraries are on their way, `false` for anything else — a dial that failed,
/// an identity that did not check out, a token that was refused. The panel needs no reason string;
/// what it needs is to stop waiting.
#[cfg_attr(
    not(test),
    expect(dead_code, reason = "seam: the transport lane's dial calls this, and this attribute goes with it")
)]
pub(crate) fn report(id: ReqId, registered: bool) {
    if let Ok(mut g) = ANSWER.lock() {
        *g = Some((id, registered));
    }
    crate::ui::idle::invalidate();
}

/// The answer to `want`, if the one waiting is for that request. A verdict carrying any other id
/// belongs to an attempt this field has already replaced, so it is DROPPED here rather than read as
/// this one's — see [`ReqId`].
fn take_answer(want: ReqId) -> Option<bool> {
    let mut g = ANSWER.lock().ok()?;
    match *g {
        Some((id, ok)) if id == want => {
            *g = None;
            Some(ok)
        }
        // a stale verdict is taken too, so it cannot be re-examined on every later frame
        Some(_) => {
            *g = None;
            None
        }
        None => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::browse::{SourceState, SrcGroup};

    fn group(state: SourceState, tier: Option<Location>, handle: &str) -> SrcGroup {
        SrcGroup { name: "nas-home".into(), handle: handle.into(), state, tier }
    }

    /// **The four states, as four different sentences.** The two that say nothing say nothing for
    /// two different reasons (see [`state_word`]), and the two that speak must not be confusable:
    /// a 401 is a sharing-grant problem whose remedy is in this panel, and calling it "not
    /// reachable" sends the user to look at a router.
    #[test]
    fn every_source_state_gets_its_own_word() {
        let words: Vec<String> = [
            SourceState::NotProbed,
            SourceState::Reachable,
            SourceState::Unauthorized,
            SourceState::Unreachable,
        ]
        .iter()
        .map(|s| accessory(&group(*s, None, "friend")))
        .collect();
        assert_eq!(words[0], "friend", "nobody has dialled: nothing is wrong, so nothing is said");
        assert_eq!(words[1], "friend", "a working source states only its owner");
        assert_eq!(words[2], "Not authorized \u{b7} friend");
        assert_eq!(words[3], "Not reachable \u{b7} friend");
        assert_ne!(words[2], words[3], "the two faults are told apart, or the remedy is a guess");
    }

    /// The state LEADS, so the run that gives way under the accessory's elision is the handle — and
    /// a source with no handle (your own server) says the state alone rather than a stray middot.
    #[test]
    fn the_state_leads_and_an_absent_handle_leaves_no_separator() {
        assert_eq!(accessory(&group(SourceState::Unreachable, None, "")), "Not reachable");
        assert_eq!(accessory(&group(SourceState::Reachable, None, "")), "");
    }

    /// **Relay is the run this list exists to draw.** Local says nothing (it is what a server is
    /// expected to be), Remote and Relay each explain something the user would otherwise read as a
    /// broken server.
    #[test]
    fn the_winning_tier_is_stated_for_relay_and_remote_and_never_for_local() {
        let a = |t| accessory(&group(SourceState::Reachable, Some(t), "friend"));
        assert_eq!(a(Location::Local), "friend");
        assert_eq!(a(Location::Remote), "Remote \u{b7} friend");
        assert_eq!(a(Location::Relay), "Relay \u{b7} friend");
    }

    /// A tier is a fact about a connection that WORKS. On a dead source it is stale at best, and
    /// stacking it behind the fault reads as two problems where there is one.
    #[test]
    fn a_broken_source_states_its_fault_and_not_its_last_known_tier() {
        assert_eq!(
            accessory(&group(SourceState::Unreachable, Some(Location::Relay), "friend")),
            "Not reachable \u{b7} friend"
        );
    }

    /// **The dim is `usable`, never `reachable()`.** That method answers the OLD question, in which
    /// an answered-and-refused server comes back `true` — so a panel that dimmed on it would draw a
    /// group with nothing browsable behind it as though it were live.
    #[test]
    fn only_the_states_with_something_behind_them_stay_lit() {
        let u = |s| usable(&group(s, None, "friend"));
        assert!(u(SourceState::NotProbed), "a group nobody has dialled must not open dimmed");
        assert!(u(SourceState::Reachable));
        assert!(!u(SourceState::Unauthorized), "there is nothing browsable behind a 401");
        assert!(!u(SourceState::Unreachable));
        assert!(
            group(SourceState::Unauthorized, None, "friend").reachable(),
            "…and this is exactly the answer that made the fourth state worth its own arm"
        );
    }

    // ---- the manual address ------------------------------------------------------------------

    #[test]
    fn an_address_parses_with_or_without_a_port_a_scheme_or_a_slash() {
        let ep = |s| parse_endpoint(s).unwrap();
        assert_eq!(ep("192.0.2.10"), Endpoint { host: "192.0.2.10".into(), port: 32400, secure: false });
        assert_eq!(ep("192.0.2.10:8443").port, 8443);
        assert_eq!(ep(" plex.example.com/ ").host, "plex.example.com");
        assert!(ep("https://plex.example.com").secure, "a pasted scheme is intent, not noise");
        assert!(!ep("HTTP://192.0.2.10").secure, "…and the test on it is case-insensitive");
        assert_eq!(ep("[2001:db8::1]:32401"), Endpoint { host: "2001:db8::1".into(), port: 32401, secure: false });
        assert_eq!(ep("[2001:db8::1]").port, 32400, "a bare v6 literal still gets the default port");
        assert_eq!(ep("2001:db8::1").host, "2001:db8::1", "…bracketless too — several colons is not host:port");
    }

    /// What the field refuses. Each of these is a thing a user really types, and every one of them
    /// has to leave the action row inert rather than send a dial at something else.
    #[test]
    fn a_half_typed_address_is_not_an_endpoint() {
        for s in ["", "   ", ":32400", "192.0.2.10:", "192.0.2.10:0", "192.0.2.10:99999", "192.0.2.10:http",
                  "192.0.2.10 32400", "192.0.2.10/library", "http://", "my server",
                  // the multi-colon shapes the host whitelist used to wave through as "an IPv6
                  // literal": one colon too many before a port, and a v4 address with a port
                  // typed twice. Both went live on the Connect row.
                  "::32400", "1.2.3:4:5", "192.0.2.10:32400:1", "gg::1", "::12345"] {
            assert_eq!(parse_endpoint(s), None, "{s:?} must not parse");
        }
    }

    /// The row COUNT holds still while the field is open — the panel's height is measured from its
    /// rows, so a sub-line appearing as you type would resize a popover under the cursor.
    #[test]
    fn the_open_field_is_always_two_rows_and_the_closed_one_always_one() {
        let mut a = Add::default();
        assert_eq!(a.rows().len(), 1);
        a.open();
        for s in ["", "1", "192.0.2.10", "192.0.2.10:"] {
            a.text = s.to_string();
            assert_eq!(a.rows().len(), 2, "{s:?}");
            assert!(!a.rows()[0].0.detail.is_empty(), "the address row always carries its sub-line");
        }
    }

    /// The action row and the parse cannot disagree: it is pressable exactly when there is
    /// something to send, and a press that lands anyway is refused by [`Add::submit`] itself.
    #[test]
    fn connect_is_inert_until_the_address_parses() {
        let mut a = Add::default();
        a.open();
        a.push("192.0.2.1");
        assert_eq!(a.rows()[1].1, SrcAction::AddSubmit);
        assert!(!a.rows()[1].0.dim);
        a.push(":");
        assert_eq!(a.rows()[1].1, SrcAction::None, "a trailing colon is not an address");
        assert!(a.rows()[1].0.dim);
        assert!(!a.submit(), "and the commit refuses it too, not only the row");
        assert_eq!(a.phase(), Phase::Editing, "so nothing starts waiting");
    }

    /// Typing, deleting and clearing — and the one rule that is not obvious: a field in flight is
    /// not editable, because the address the backend was handed must still be the address on screen
    /// when it answers.
    #[test]
    fn the_field_takes_text_until_it_is_in_flight() {
        let mut a = Add::default();
        a.push("nope");
        assert_eq!(a.text(), "", "a closed field takes nothing");
        a.open();
        a.push("192.0.2.1");
        a.push("0");
        assert_eq!(a.text(), "192.0.2.10");
        a.backspace();
        assert_eq!(a.text(), "192.0.2.1");
        assert!(a.submit());
        assert_eq!(a.phase(), Phase::Waiting);
        a.push("9");
        a.backspace();
        a.clear();
        assert_eq!(a.text(), "192.0.2.1", "nothing moves while the backend holds the request");
    }

    /// Backspace is one CHARACTER, not one byte — a hostname is UTF-8 and half a codepoint is a
    /// panic waiting for an internationalised domain.
    #[test]
    fn backspace_takes_a_character() {
        let mut a = Add::default();
        a.open();
        a.push("höm");
        a.backspace();
        a.backspace();
        assert_eq!(a.text(), "h");
    }

    /// **The seam, both directions.** The panel posts exactly what was parsed; a `true` closes the
    /// field (the new server arrives as a GROUP, so the row has nothing left to say) and a `false`
    /// leaves the address on screen with a retry.
    #[test]
    fn the_backend_takes_the_request_and_its_answer_lands() {
        let _serial = crate::testlock::serial();
        let mut a = Add::default();
        a.open();
        a.push("192.0.2.10:32401");
        assert!(a.submit());
        let (id, ep) = take_request().expect("the address was posted");
        assert_eq!(ep, Endpoint { host: "192.0.2.10".into(), port: 32401, secure: false });
        assert_eq!(take_request(), None, "one request, taken once");

        assert!(!a.tick(0.016), "nothing has answered yet");
        report(id, false);
        assert!(a.tick(0.016));
        assert_eq!(a.phase(), Phase::Failed);
        assert_eq!(a.text(), "192.0.2.10:32401", "the address stays put — you are retrying it");
        assert_eq!(a.rows()[1].0.label, "Try again");

        assert!(a.submit(), "and a retry is a fresh request");
        let (id2, _) = take_request().expect("posted again");
        assert_ne!(id2, id, "…with an id of its own");
        report(id2, true);
        assert!(a.tick(0.016));
        assert_eq!(a.phase(), Phase::Closed, "registered: the field gets out of the list's way");
    }

    /// **A verdict for an attempt the user has already replaced is DROPPED**, which is the whole
    /// reason a request carries an id. Clearing the mailbox at `submit` cannot cover this on its
    /// own: the previous address is still being dialled on a worker, and its answer lands in the
    /// window between that clear and the next tick — as a "no answer" for a server nobody dialled,
    /// or, worse, as a success.
    #[test]
    fn an_answer_for_the_previous_attempt_is_not_read_as_this_ones() {
        let _serial = crate::testlock::serial();
        let mut a = Add::default();
        a.open();
        a.push("192.0.2.1");
        assert!(a.submit());
        let (stale, _) = take_request().expect("first attempt posted");

        a.phase = Phase::Editing; // the user corrects the address and presses Connect again
        a.push("0");
        assert!(a.submit());
        let (fresh, _) = take_request().expect("second attempt posted");

        report(stale, true); // the FIRST dial finally answers
        assert!(!a.tick(0.016), "the late verdict is not this attempt's");
        assert_eq!(a.phase(), Phase::Waiting, "…so the field is still waiting on its own");
        report(fresh, false);
        assert!(a.tick(0.016));
        assert_eq!(a.phase(), Phase::Failed, "and its own answer does land");
    }

    /// A request nobody picks up ages out and SAYS so, rather than spinning for ever. That is the
    /// literal truth today — the transport lane's dial does not exist yet — and it stays the right
    /// read-out once it does.
    #[test]
    fn a_request_nobody_answers_times_out() {
        let _serial = crate::testlock::serial();
        let mut a = Add::default();
        a.open();
        a.push("192.0.2.10");
        assert!(a.submit());
        take_request();
        assert!(!a.tick(WAIT_MS / 1000.0 - 0.1), "still in flight just short of the deadline");
        assert!(a.tick(0.2));
        assert_eq!(a.phase(), Phase::Failed);
        assert_eq!(a.rows()[0].0.detail, "No answer from that address");
        // …and typing again is a new attempt, not a stuck failure
        a.push("1");
        assert_eq!(a.phase(), Phase::Editing);
        assert_eq!(a.rows()[1].0.label, "Connect");
    }

    /// A stale answer cannot be read as this request's: submitting clears the mailbox first.
    #[test]
    fn a_submit_discards_an_answer_nobody_asked_for() {
        let _serial = crate::testlock::serial();
        report(ReqId(0), true); // an id `submit` will never hand out
        let mut a = Add::default();
        a.open();
        a.push("192.0.2.10");
        assert!(a.submit());
        assert!(!a.tick(0.016), "the leftover `true` was dropped, not adopted as success");
        assert_eq!(a.phase(), Phase::Waiting);
        let (id, _) = take_request().expect("posted");
        take_answer(id);
    }

    /// **The panic this field could take the app down with.** `parse_endpoint` runs on EVERY
    /// keystroke (the sub-line re-parses to state the port), and its scheme test byte-sliced the
    /// input — so a hostname typed in Cyrillic or Japanese, whose 7th or 8th byte is the middle of
    /// a character, panicked out of the SDL event loop. Each string here is one that lands a scheme
    /// prefix length mid-codepoint, and each is driven through `rows()`, which is what the panel
    /// actually calls.
    #[test]
    fn a_non_ascii_address_is_refused_and_never_panics() {
        for text in ["привет", "サーバー", "ütüütüü", "日本", "\u{1f600}\u{1f600}"] {
            let mut a = Add::default();
            a.open();
            a.push(text);
            assert_eq!(a.endpoint(), None, "{text:?} is not an address");
            let rows = a.rows(); // the call the panel makes — it must not panic either
            assert_eq!(rows.len(), 2);
            assert_eq!(rows[0].0.label, text, "…and it is still shown as typed");
            assert_eq!(rows[1].1, SrcAction::None, "…with nothing to press");
        }
    }

    /// The sub-line states the PORT once the address parses — the one fact about the request that
    /// the typed text does not show, and the one people get wrong silently.
    #[test]
    fn the_sub_line_states_the_port_that_will_be_used() {
        let mut a = Add::default();
        a.open();
        assert_eq!(a.detail(), HINT, "an empty field asks for the shape of the answer");
        a.push("192.0.2.10");
        assert_eq!(a.detail(), "Port 32400");
        a.push(":8443");
        assert_eq!(a.detail(), "Port 8443");
    }
}
