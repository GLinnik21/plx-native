//! The legacy navigation model: `Route`, `Nav`/`NavReq`,
//! the trail helpers and the page-open/menu-open functions. Retired by phase 12 of the UI
//! restructure; moved out of `app.rs` verbatim in phase 1a (a pure move; `pub(crate)` widening
//! only).

use super::*;

// (`enum Overlay` stood here. The player's four panels are entries on the player page's own
// `ModalStack` since restructure phase 9 — `screens::player::overlay::OverlayKind` is what names
// them now, and the CONTAINER owns which one is up. A route field saying so as well was the second
// owner the phase exists to remove: `route` and the stack could disagree, and while they did, the
// key ladder, the pointer path and the draw each asked a different one of the two.)
// (`MenuHost` and its `probe_host` mirror stood here. The item context menu is a `ModalStack`
// SURFACE since restructure phase 10, so the "which screen is this popover sitting over" question
// has no answer to give: a surface is presented OVER the top page and never replaces it, which is
// what the six-variant enum existed to arrange by hand. The two bits an ACTION still reads —
// whether the item is a leaf of the season the detail page has loaded, and whether the hold
// happened on HOME's root — travel on `screens::registry::ItemMenuArg`, and the focus probe names
// the host as `route=` because it IS the route.)
// **`Route` itself moved to `screens::registry` in restructure phase 10** and is re-exported here,
// so every `super::Route` in `app/` reads exactly as it did. The move is the layer rule (§2.1): the
// concrete `ScreenArg` and the one `mount` match live in `screens/registry.rs` and are both written
// over this alphabet, and a screen may not name `app::` — which is the blocker that module's doc
// used to record. What is still the LOOP's is here: the trail mapping, the teardown table, the
// page-open helpers, and `route_word`'s heartbeat spelling in `app/mod.rs`. All of it retires in
// phase 12; the alphabet does not.
//
// `route_wears_tab_bar` went with it for the same reason — `AppArg::chrome` is its first reader —
// and is re-exported beside it.
pub(crate) use crate::screens::registry::{route_wears_tab_bar, Route};

/// The PAGE a trail node names — the ONE Node→[`Route`] mapping in the app. Both things
/// that have to know it read it here: `enter_node` flips the route through it after
/// mounting, and [`node_wears_tab_bar`] answers the chrome question by handing it to
/// [`route_wears_tab_bar`], so a node and the page it mounts can never answer differently.
///
/// A free fn and not `Node::route`, which is what it would rather be: `Node` belongs to
/// `ui::trail` (deliberately — the trail decides nothing about screens and cannot see `Route`),
/// so the inherent `impl` would be a foreign one, which `non_local_definitions` warns about.
pub(crate) fn node_route(n: &Node) -> Route {
    match n {
        Node::Home => Route::Home,
        Node::Library => Route::Library,
        Node::Search { .. } => Route::Search,
        Node::Person { .. } => Route::Person,
        Node::Detail { .. } => Route::Detail,
    }
}
/// The same question about a TRAIL node — what a BACK's destination wears, peeked before the
/// pop, and what a forward `Nav::Open` is about to put on screen. DERIVED from
/// [`route_wears_tab_bar`] through [`node_route`] rather than listing the node kinds a
/// second time: a node and the route it mounts are the same page, and the two lists had no
/// way to stay in step beyond someone noticing.
pub(crate) fn node_wears_tab_bar(n: &Node) -> bool {
    route_wears_tab_bar(node_route(n))
}
// (`page_of` stood here — "the PAGE a route draws", which for the two popover routes was the
// screen underneath. It was the identity function for every other variant, and since phase 10 it
// is the identity function for ALL of them: neither menu is a route any more, so a `Route` names
// exactly one page and every caller reads the route directly. Deleting it rather than leaving an
// identity wrapper is the point — a second name for "the page" is a place for a second answer to
// grow back.)

/// The page's TEARDOWN — what leaving it FOR GOOD has to run, handed to `ui::nav` so it
/// happens at the fade floor instead of on the press frame (see that module's doc: run
/// early, `detail::close`'s `metadata::clear` empties the page *during its own fade-out*).
///
/// WHEN a navigation asks for one is [`stays_on_trail`]'s question, not this function's: this is
/// only "what does leaving this page for good have to run".
///
/// Spelled out route by route, exactly as [`route_wears_tab_bar`] above it is and for the
/// same reason: with a `_ => None` catch-all, a new STACKING screen compiles with no
/// teardown at all and silently leaks the item it loaded, which is invisible until the page
/// it left behind reappears under the next one.
pub(crate) fn leave_of(r: Route) -> Option<fn()> {
    match r {
        Route::Detail => None,
        Route::Person => None,
        // Nothing loaded that outlives the page. Home and the Library keep their stores for
        // as long as the profile does (`browse.rs` is re-ENTERED, never re-queried — that is
        // why `Node::Library` carries no payload), Login/Profiles/Onboard are boot gates the app
        // leaves once, and a player session is torn down by its own exit path.
        //
        // Search USED to have one — its keyboard had to come down with the page, and the legacy
        // screen had no real Unmount lifecycle of its own to carry that. It is an OWNED screen
        // now (phase 7 Search cutover): `SearchScreen::step` already answers
        // `ScreenEvent::Unmount` by dropping its own keyboard, and that event is delivered by the
        // ordinary tree-retirement path every owned page's teardown already rides — the same one
        // Detail's `metadata::clear` and Person's `person::leave` used to need this callback for,
        // until they too became owned. Proven with a REAL route change through `bridge::frame`
        // (`app/search_owned_tests.rs`'s `leaving_owned_search_through_a_real_route_change_
        // releases_its_keyboard`), not an assumption: `d.input.keyboard` (and the real
        // `crate::textinput::stop()` behind it) goes false with no `leave_of` arm at all.
        Route::Home
        | Route::Library
        | Route::Login
        | Route::Profiles
        | Route::Onboard
        | Route::Search
        | Route::Player => None,
    }
}
/// Does this page STAY MOUNTED behind a forward navigation — is it a page the BACK trail can put
/// back? This is the whole rule for when [`leave_of`] is asked for, and the honest predicate is
/// **trail membership, not direction**.
///
/// A BACK always tears the page down: it is being left for good, by definition. A FORWARD
/// navigation is the interesting half, and the obvious generalisation ("carry the teardown either
/// way") is WRONG: Detail and Person stay on the trail, so closing one on the way deeper would
/// empty the page the user is about to press BACK to — the exact bug `leave_of`'s doc defends
/// against, and `nav`'s retarget rule is built around.
///
/// [`Route::Search`] is the case that made this a predicate rather than a `None`, and it USED TO
/// be the one route where the two questions this file otherwise collapses genuinely came apart:
/// it HAS a node and a result opened from it does stay on the trail, but the legacy screen's
/// keyboard-dismissal teardown (`leave_of`'s old `search::leave` arm) still had to ride every
/// forward navigation, since the legacy screen had no real Unmount lifecycle of its own to run it
/// from. Now that Search is an OWNED screen (phase 7), `leave_of(Route::Search)` is `None` exactly
/// like Home/Library/Detail/Person — `SearchScreen::step` drops its own keyboard on the ordinary
/// `Unmount` every owned page's teardown already rides — so `forward_leave(Route::Search)` is
/// `None` REGARDLESS of this predicate's answer for it. It stays `false` below only to keep
/// `Node::Search`'s absence from `every_trail_node_names_a_page_that_stays_on_the_trail`
/// (`app/mod.rs`) honest — Search is still deliberately not in that "BACK can put this back"
/// list — not because a teardown still depends on it.
pub(crate) fn stays_on_trail(r: Route) -> bool {
    match r {
        // exactly the `Node` variants (`node_route`'s domain): a forward navigation leaves these
        // standing behind the destination, which is what makes the common pop a route flip
        Route::Home | Route::Library | Route::Detail | Route::Person => true,
        // Has a `Node` (`Node::Search`) but is deliberately not counted as one BACK can put back —
        // see the doc above. `leave_of(Route::Search)` is `None`, so this answer no longer changes
        // `forward_leave`'s result for it either way.
        Route::Search => false,
        // Boot gates the app leaves once, and a player session torn down by its own exit path.
        // None of the four has a `leave_of` at all, so this answer is about being honest rather
        // than about having an effect.
        Route::Login | Route::Profiles | Route::Onboard | Route::Player => false,
    }
}
/// The teardown a FORWARD navigation off `cur` carries — [`stays_on_trail`] and [`leave_of`]
/// composed, so the two halves of the rule are stated once and cannot drift apart at the two call
/// sites (`nav_to` and `nav_open`).
pub(crate) fn forward_leave(cur: Route) -> Option<fn()> {
    if stays_on_trail(cur) {
        None
    } else {
        leave_of(cur)
    }
}

// ---- the screens, the transitions, and the playback rituals -----------------------------------
//
// Declared here rather than in `plex_run`'s body, where they were until now. Each is an item — an
// `fn`, a `struct`, an `enum`, a `const`, a `static` — and an item cannot capture, so every one
// already took what it reads from the loop as an argument. The move therefore changed no signature.
//
// The loop still owns the VALUES: `route`, `trail`, `nav_pending`, the HUD cursor and every
// input-state local are `plex_run` locals, handed in by reference wherever a helper writes one.
//
// They are NOT `pub`, and that is deliberate. `lib.rs` declares `mod app` private and nothing here
// is exported, so `Route` cannot be named from `ui/` — the boundary [`node_route`] above exists to
// bridge; see its doc, which describes the trail as deciding nothing about screens and unable to
// see a `Route`. `Nav` and `NavReq` sit behind the same wall.

// (`enum Modal` and `modal_of` stood here — "which panel owns the frame", derived from the route.
// The player's four panels left it in phase 9 and the card menu was the last entry; with that
// route gone the enum could only ever answer `None`, and the CONTAINER has been the real answer
// for both since (`Dispatcher::surface_up` / `top_surface_name`, which `bridge::owns_input` asks).
// A second answer derived from the route is exactly the drift these phases remove.)
// ---- a route change asked for: the request, and the calls that queue or withdraw one ----------
/// A route change the user has ASKED for but which has not been applied yet: the page is
/// fading out (`ui::nav`) and the fader's commit frame applies it. A TYPED value rather than
/// a boxed closure, and the newest simply overwrites the one before it — the shape
/// `library.rs`'s `Pending` already argues for (a fast double press must commit ONCE, to the
/// last thing pressed).
///
/// It carries every ARGUMENT the destination's entry point takes, because both halves of a
/// route change now happen at the fade floor, not just the flip. That is why the two
/// stacking arms hold a `Node`: a trail node already IS "everything needed to put this page
/// on screen without the screen that asked for it", so one type serves the push and the
/// mount, and `enter_node` is the one ritual for both directions.
#[derive(Clone)]
pub(crate) enum Nav {
    /// Home. `focus_pill` is the tab pill that held FOCUS on the way out, carried across so
    /// the pill the user is standing on is still the one under focus when Home takes over —
    /// which is a different question from the pill Home SELECTS ([`Nav::select_pill`],
    /// always the Home pill). One word for both is why they were named apart: on the way
    /// back from the Library the selection moves to Home while focus stays on `Movies`.
    ///
    /// **An IDENTITY, not an index** (Codex review, 2026-09-05). It survives a `nav` fade, and a
    /// pill can appear or disappear inside that window now that the favourite switch governs the
    /// strip — so an index picked up on the press frame could name a different destination by the
    /// time Home mounts. `Pill` cannot.
    Home {
        focus_pill: Option<crate::ui::widgets::Pill>,
    },
    /// The Library browse grid on a TYPE. Discovery later resolves it to an owned-first, then
    /// shared FAVOURITE library of that type.
    ///
    /// It carried the strip position until 2026-09-05, which was safe only while the strip was
    /// invariant. A pending navigation survives a page fade, and the favourite switch can remove a
    /// pill during one — so an index queued before the change would arrive naming a different
    /// type. The kind is what the destination actually is; see [`crate::ui::widgets::Pill`].
    Library(crate::browse::SecKind),
    /// A page that STACKS — a detail page or a person page. The [`Node`] is BOTH what
    /// mounts at the floor (through the very `enter_node` a BACK pop uses, whose re-open
    /// guard means "the page you asked for is already the one loaded" costs nothing) and
    /// what is then pushed onto the trail. `season` is the one mount a node cannot express:
    /// a SHOW opened with one season already selected, which a node has no field for
    /// because a trail node names a PAGE, not a tab inside one.
    Open { node: Node, season: Option<c_int> },
    /// The Search screen — a RETURN to it, which is why it carries nothing.
    ///
    /// It used to hold a `query: String` to seed the field with, and every one of the four
    /// interactive entries passed `String::new()`: the pill wiped the term the user was
    /// still reading, the shelves under it and both cursors, on a screen whose BACK-trail
    /// re-entry (`Node::Search`) deliberately preserves all three. The seed's only real
    /// caller was never this enum at all — `/tmp/plxnative-search=<q>` mounts through
    /// `search::enter` directly, with no transition to carry a payload — so the field
    /// existed to be empty. `search::resume` is what the commit arm calls now.
    Search,
    /// BACK off a stacking page: pop the trail at the floor and re-enter what was under it.
    /// The destination is deliberately NOT spelled out here — `enter_node` handles every
    /// node, and re-deriving it at the press would mean peeking a trail the pop re-reads
    /// anyway. `bar` is the one thing the PRESS frame has to know before the pop happens:
    /// whether the page underneath wears the shared top bar.
    Back { bar: bool },
}
impl Nav {
    /// The pill this destination SELECTS — what the shared tab row must read from the press
    /// frame on (`ui::nav::view_tab`). Not to be confused with `Nav::Home`'s `focus_pill`,
    /// which is where the remote's focus LANDS: arriving at Home always selects the Home
    /// pill (0), whatever pill the user was standing on when they left. `None` = leave the
    /// row to whatever screen owns it, which is right both for a destination that has no bar
    /// at all and for a BACK, where the page being restored answers for its own chrome
    /// (`library::view_section`) the moment it is mounted.
    pub(crate) fn select_pill(&self) -> Option<usize> {
        match self {
            Nav::Home { .. } => crate::ui::widgets::pill_of(Pill::Home),
            // a TYPE-tab index, not a section index: Movies and TV Shows exist before discovery.
            // Placed through `pill_of` rather than by a `+1` here — where the type pills start in
            // the row is the strip's business, not this
            // enum's, and the two must agree with what a CLICK on that pill resolves to.
            // `None` when that type's pill has just gone: nothing to select, and the strip's own
            // clamp keeps focus somewhere that exists.
            Nav::Library(kind) => crate::ui::widgets::pill_of(Pill::Section(*kind)),
            Nav::Search => crate::ui::widgets::pill_of(Pill::Search),
            Nav::Open { .. } | Nav::Back { .. } => None,
        }
    }
    /// Does the destination draw the shared top bar? Written as a `match` and not a
    /// `matches!` on purpose: a new destination is then a COMPILE ERROR here rather than a
    /// silent `false`, and a silent `false` is a bar that blinks out and back for no reason.
    pub(crate) fn wears_tab_bar(&self) -> bool {
        match self {
            Nav::Home { .. } | Nav::Library(_) | Nav::Search => true,
            // Detail and Person wear no bar today — but the NODE is the destination and can
            // answer for itself, so ask it rather than hard-coding the answer a new stacking
            // page would silently inherit.
            Nav::Open { node, .. } => node_wears_tab_bar(node),
            Nav::Back { bar } => *bar,
        }
    }
}
/// A queued [`Nav`] plus the route it was queued FROM. The `from` is the whole supersede
/// rule: a route change from any OTHER source (an async play resolve, the app-switch
/// lifecycle, a login landing) has moved the app somewhere the user can see, and a stale
/// request must not flip the screen out from under it. One equality test at the commit
/// covers every such site without any of them having to know this exists.
#[derive(Clone)]
pub(crate) struct NavReq {
    pub(crate) to: Nav,
    pub(crate) from: Route,
    /// Where the page being LEFT was standing, snapshotted at the PRESS (`detail::spot`'s
    /// own contract) and written onto its trail node at the floor. Carried rather than
    /// re-read at the commit because the user can still move focus during the 70 ms, and
    /// BACK must return them to where they pressed, not to where the fade found them.
    pub(crate) spot: Option<Spot>,
    pub(crate) entry: Option<crate::ui::machine::EntryId>,
    pub(crate) owner: Option<crate::ui::machine::InputOwner>,
    pub(crate) ret: Option<crate::ui::screen::ReturnState<u32, crate::screens::registry::PageMemory>>,
}

impl NavReq {
    pub(crate) fn is_current(&self, route: Route, entry: Option<crate::ui::machine::EntryId>, owner: Option<crate::ui::machine::InputOwner>) -> bool {
        self.from == route && self.entry.is_none_or(|id| entry == Some(id))
            && self.owner.is_none_or(|from| owner == Some(from))
    }
}

#[cfg(test)]
mod content_request_identity_tests {
    use super::*;
    use crate::ui::machine::{EntryId, InputOwner};

    #[test]
    fn cancelling_a_content_request_compares_entry_and_surface_not_route_kind() {
        let req = NavReq { to: Nav::Back { bar: false }, from: Route::Person, spot: None,
            entry: Some(EntryId(1)), owner: Some(InputOwner::Entry(EntryId(2))), ret: None };
        assert!(req.is_current(Route::Person, Some(EntryId(1)), Some(InputOwner::Entry(EntryId(2)))));
        assert!(!req.is_current(Route::Person, Some(EntryId(3)), Some(InputOwner::Entry(EntryId(2)))));
        assert!(!req.is_current(Route::Person, Some(EntryId(1)), Some(InputOwner::Entry(EntryId(4)))));
    }
}
/// Where the page being left is standing, for [`NavReq::spot`]. Only a detail page has a
/// place worth restoring (`Trail::set_top_spot` ignores every other node), so this is the
/// whole rule — no per-arm decision, and no call site that can forget it. On a BACK the
/// node it is recorded onto is the one about to be popped, so the write is simply spent;
/// that costs one struct copy and buys the rule its uniformity.
pub(crate) fn leaving_spot(_cur: Route) -> Option<Spot> {
    None // owned pages capture their engine focus through the bridge's ReturnState
}/// Ask for `to`, through the page cross-fade, carrying the outgoing page's teardown.
///
/// **Both halves of a route change land at the floor**: the outgoing page's teardown and
/// the incoming page's mount. That uniformity is the design — the alternative is a per-arm
/// judgement about which stores the screen still on screen happens to read, and the arm
/// that gets it wrong blanks a page in the middle of its own fade. It costs the ~70 ms of
/// `OUT_MS` before a detail fetch is issued, which the fade is spending anyway and the
/// page's own spinner already covers.
pub(crate) fn nav_req(cur: Route, to: Nav, leave: Option<fn()>, pending: &mut Option<NavReq>) {
    crate::ui::nav::begin(
        route_wears_tab_bar(cur) && to.wears_tab_bar(),
        to.select_pill(),
        leave,
    );
    *pending = Some(NavReq {
        to,
        from: cur,
        spot: leaving_spot(cur),
        entry: None,
        owner: None,
        ret: None,
    });
}
/// A FORWARD navigation. It carries a teardown only when the page it leaves is NOT one the
/// BACK trail can put back — see [`stays_on_trail`], which is where that rule and its two
/// wrong generalisations are argued.
pub(crate) fn nav_to(cur: Route, to: Nav, pending: &mut Option<NavReq>) {
    nav_req(cur, to, forward_leave(cur), pending);
}
/// Open a stacking page (detail / person) through the transition — the ONE forward entry to
/// both, so a new way in cannot push without routing or route without pushing. The mount
/// and the push both happen at the fade floor; see [`nav_req`].
pub(crate) fn nav_open(cur: Route, node: Node, season: Option<c_int>, pending: &mut Option<NavReq>) {
    nav_req(cur, Nav::Open { node, season }, forward_leave(cur), pending);
}
/// BACK off a stacking page, through the transition. The page IS being left for good, so
/// its teardown rides the request; the trail is only PEEKED here (`Trail::under`) and the
/// pop itself happens at the floor, so a second BACK inside the window withdraws this one
/// instead of popping a page that is still on screen.
pub(crate) fn nav_back(cur: Route, trail: &Trail, pending: &mut Option<NavReq>) {
    let bar = trail.under().map(node_wears_tab_bar).unwrap_or(false);
    nav_req(cur, Nav::Back { bar }, leave_of(cur), pending);
}
/// Withdraw a queued transition — but only one that is still THIS screen's to withdraw.
/// Returns whether there was one, so an input that cancelled NOTHING falls through to its
/// normal handling instead of being swallowed.
///
/// The `from == cur` test is the same supersede rule the commit applies, moved earlier: a
/// request whose origin route is no longer the one mounted is already dead (the commit will
/// drop it), so withdrawing it must not consume a press meant for the screen the user is
/// actually on. Without it a BACK could be spent un-asking an invisible transition instead
/// of leaving the player.
pub(crate) fn nav_cancel(cur: Route, pending: &mut Option<NavReq>) -> bool {
    if pending.as_ref().map(|r| r.from != cur).unwrap_or(true) {
        return false;
    }
    let did = crate::ui::nav::cancel();
    if did {
        *pending = None;
    }
    did
}

// ---- navigation targets, page entry, and the playback rituals ---------------------------------
/// A forward navigation to `rk`'s detail page, as a [`Nav`] destination. The ONE builder,
/// so the six ways in cannot drift in what they push: the node carries an EMPTY spot, which
/// is filled in only if the user later navigates deeper off the page (`Trail::set_top_spot`).
pub(crate) fn to_detail(sid: crate::plex::ServerId, rk: &str) -> Node {
    Node::Detail {
        sid,
        rk: rk.to_string(),
        spot: Spot::default(),
    }
}

/// Where a playback session RETURNS TO, as handed to [`start_playback`].
///
/// **This replaced a `from_detail: bool`, and the bool was a bug rather than a simplification.**
/// It answered one question — "was this launched from the detail page?" — and `exit_player` turned
/// it back into `if played_from_detail { Route::Detail } else { Route::Home }`, so every OTHER
/// screen that can start playback dropped the user on Home when they pressed BACK. That was
/// invisible while the two launch sites were the detail page and a Home card, and stopped being
/// invisible the moment the card context menu opened on the detail page's RELATED shelf: one page
/// then had two *Play from Start* rows, the filmstrip's returning to the page and the shelf's
/// returning to Home. The Library grid, Search and the person page had the same defect the whole
/// time and nobody had complained.
///
/// A [`Node`] rather than a `Route` because a route names a KIND of page and BACK has to land on
/// the RIGHT one: `Route::Detail` cannot say which item, and the played leaf's own detail is
/// mounted under the session by then, so re-deriving the page at exit reads the wrong item by
/// construction. The node is captured on the press frame, before any of that moves.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Origin {
    /// A fresh launch: Stop/BACK/EOS lands on this page.
    From(Node),
    /// Keep whatever the live session already returns to. Two callers, and both would be WRONG to
    /// re-capture: `play_up_next`'s auto-advance starts a new item while the player is already up
    /// (so "the page on screen" is the player, and the user chose nothing), and a PLAY key that
    /// resumes a session the app-switch lifecycle suspended is resuming the same session from a
    /// route that has been forced to Home in the meantime.
    Unchanged,
}

/// The page a launch from route `r` returns to — the PURE half of [`origin_here`], with the two
/// stacking screens' identities passed in.
///
/// Split out because it is the whole of the decision and none of it is reachable from a host test
/// otherwise: every launch site is inside the SDL event loop. `detail`/`person` are `None` when
/// that screen has nothing mounted, which falls back to Home — a return target must always name a
/// page, and Home is the one page that is always there.
///
/// The route IS the page since phase 10: neither menu is a route any more, so a launch from one is
/// dispatched with `app.route` already naming the screen the panel was standing on, and this needs
/// no `page_of` resolution ahead of it.
pub(crate) fn return_page(r: Route, detail: Option<Node>, person: Option<Node>) -> Node {
    match r {
        Route::Detail => detail.unwrap_or(Node::Home),
        Route::Person => person.unwrap_or(Node::Home),
        Route::Library => Node::Library,
        Route::Search => Node::Search,
        // Home is the root and the honest answer for the four boot gates as well. `Player` is
        // unreachable — every caller is a launch, which is off the player route by definition —
        // and lands here rather than being a variant the compiler makes anyone think about.
        Route::Home | Route::Login | Route::Profiles | Route::Onboard | Route::Player => {
            Node::Home
        }
    }
}

/// The page on screen NOW, as an [`Origin`] — [`return_page`] fed from the live stores.
///
/// The detail node carries the page's [`Spot`], so a return is a RESTORE (the Related tile the user
/// pressed on is still the focused one) rather than a fresh arrival at the hero. An empty mounted
/// rk means the page never mounted, which is not a page anyone can be returned to.
pub(crate) fn origin_here(r: Route, trail: &Trail) -> Origin {
    let node = trail.top().clone();
    let detail = matches!(&node, Node::Detail { .. }).then(|| node.clone());
    let person = matches!(&node, Node::Person { .. }).then(|| node.clone());
    Origin::From(return_page(r, detail, person))
}
/// Record where the session that is STARTING returns to.
///
/// One line, named because the `Unchanged` half is the whole of the auto-advance rule and is
/// otherwise unreachable from a test: `play_up_next` starts a new item while the player is already
/// up, so re-capturing "the page on screen" would rewrite the user's return target to the player
/// itself — and after two or three episodes the only honest answer to "where did I come from" would
/// have been thrown away. Applied only on a session that actually entered, so a refused start
/// leaves the live session's target alone as well.
pub(crate) fn set_origin(play_from: &mut Node, from: Origin) {
    if let Origin::From(n) = from {
        *play_from = n;
    }
}


/// Enter `rk`'s detail page with a HARD CUT — no transition. The one caller left is the
/// `/tmp/plxnative-detail` boot trigger, and the reason is the same one the Library boot
/// trigger gives: at boot there is no outgoing screen to replace, so a dip would fade the
/// page up out of nothing and read as a slow app rather than a navigated one. Every
/// INTERACTIVE way in goes through [`nav_open`] instead.
pub(crate) fn push_detail(trail: &mut Trail, route: &mut Route, sid: crate::plex::ServerId, rk: &str) {
    trail.push(to_detail(sid, rk));
    *route = Route::Detail;
}

/// The trail bookkeeping an item-menu navigation performs on the page it is LEAVING.
///
/// From HOME the menu is the user acting on the root, exactly as `home_activate` is, so the
/// history behind them is spent. That truncation stays on the PRESS frame while the push it
/// precedes moves to the fade floor, and the asymmetry is deliberate: Home is `stack[0]`, so a
/// reset to the root is idempotent and survives a withdrawn transition unharmed, whereas a PUSH or
/// a POP is history the user would actually lose.
///
/// From the DETAIL page there is nothing to do here — where that page was standing is
/// `NavReq::spot`'s job, recorded uniformly for every navigation off a detail page rather than by
/// this one arm remembering to.
///
/// **And nothing from the Library, Search or the person page either**, which is the answer a new
/// entry point wants by default: navigating out of the menu there is the same forward move the
/// tile's own OK makes (`open_library_card`, `search::on_ok`, `open_person_card`), so `nav_open`
/// stacks and BACK comes back to the grid or shelf the card is sitting on. Home is the exception
/// BECAUSE it is the root, not because it is a menu host.
///
/// `from_home` is the whole of what `MenuHost` was still deciding here — it was
/// `matches!(host, MenuHost::Home)` over a six-variant enum — and it rides on
/// `ItemMenuArg`/`ItemMenuReq` now (phase 10).
pub(crate) fn menu_leave(trail: &mut Trail, from_home: bool) {
    if from_home {
        trail.reset();
    }
}

/// Put page `n` on screen — the ONE entry, shared by every BACK pop AND by every forward
/// navigation onto a stacking page ([`Nav::Open`]). Always at the fade floor.
///
/// Each arm is `person::leave`'s old rule generalized: **re-open only if the page behind is
/// not still the one loaded.** That is what makes the common case free (a detail page opened
/// on top of a person page never disturbed `person`'s store, so BACK is a route flip) and
/// the deep case correct (a page closed two levels ago is re-fetched, by rk, through the
/// same `open_rk` every other entry point uses).
///
/// The same guard is exactly right FORWARD, which is why one function serves both
/// directions: a cast-row OK has already installed the person on the press frame (nothing
/// the detail page underneath reads, so it costs the outgoing page nothing) and must not
/// re-fetch it here; `home_activate`'s play-a-show arm has already mounted the detail page
/// blocking, because deciding play-vs-open required the loaded item. In both cases the
/// honest reading of the guard — "the page you asked for is already the one loaded" — is
/// the wanted no-op.
///
/// The MOUNT is per-node; the route flip is not — it is [`node_route`], applied once at the
/// end, so this function and `node_wears_tab_bar` cannot come to disagree about what page a
/// node is. The `match` stays exhaustive for the mounts themselves.
pub(crate) fn enter_node(n: &Node, route: &mut Route) {
    // The navigation container mounts or uncovers the entry at this commit.
    *route = node_route(n);
}

// `open_tile_menu` — the shared press-and-hold item-menu opener for the four card surfaces that
// are NOT Home (the Library grid, a Search result shelf, the person page's filmography, the
// detail page's RELATED shelf) — was retired here in the phase 7 Search cutover, its last caller.
// The other three had already migrated to their own owned-screen Fx request (`LibraryReq`,
// `PersonReq`, the detail page's own opener) before Search's `SearchReq::ItemMenu` (drained by
// `content::search_requests`) did the same; this function's body was byte-for-byte what those
// requests now do inline at their own call sites (`crate::ui::item_menu::open` +
// `*route = Route::ItemMenu { over: host }`), so nothing was left to share once the fourth caller
// was gone. `open_item_menu`, the same shape for the HOME GRID card, went with it in the phase 8
// Home cutover — `ui::home` (and its `movie_at`/`focused_card_rect`/`redraw_focused_card`) is
// deleted, and the owned Home screen opens the panel inline at its own press site the same way.
