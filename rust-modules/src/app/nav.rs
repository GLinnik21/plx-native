//! The legacy navigation model: `Route`, `Overlay`, `MenuHost`/`BarHost`, `Modal`, `Nav`/`NavReq`,
//! the trail helpers and the page-open/menu-open functions. Retired by phase 12 of the UI
//! restructure; moved out of `app.rs` verbatim in phase 1a (a pure move; `pub(super)` widening
//! only).

use super::*;

/// Exclusive route state machine (replaces 5 entangled bools). Overlays live INSIDE
/// Player because they only mean anything during playback; Detail and Player are mutually
/// exclusive. Deleting the old bools makes the compiler flag any un-migrated read.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Overlay {
    None,
    Menu,
    Info,
    Chapters,
    /// the `…` disc's overflow popover (`ui/more_menu.rs`)
    More,
}
/// Which screen a [`Route::ItemMenu`] popover is sitting over.
///
/// The menu is a popover on a LIVE screen, not a page of its own — the card and its row keep
/// drawing and animating behind it — so the route has to name the screen underneath, both to
/// go on drawing/updating it and to know where the popover closes back to.
///
/// **Read it through [`page_of`], never by `matches!`ing a variant.** Every question this file asks
/// about an `ItemMenu` — which page draws, which updates, which chrome it wears, what a navigation
/// off it tears down — is the answer for the screen underneath, and each one used to name a host by
/// hand. That is exactly what made adding a third host a five-site edit with silent failures at
/// each: a page falling through to `home_draw`, a tab bar disappearing mid-hold.
///
/// **Every screen with card tiles is a host.** It was Home and the detail filmstrip alone, while
/// the Library grid, Search's result shelves, the person page's filmography and the detail page's
/// Related shelf all ARM the same press (`press::begin` + `ok_armed`) — so a hold there dipped the
/// card, latched long, and then did nothing at all.
///
/// The Related shelf was the last of those and was excluded one round longer than the rest, on the
/// stated grounds that its tiles carried no `(ratingKey, watched)` pair to build rows from. That
/// was true of the STRUCT and never of the data — `/related` returns the same wire DTO as every
/// other listing — so the fix was upstream, in `metadata::Related`, and this became an ordinary
/// host.
///
/// **What remains excluded is excluded for a reason that does not dissolve**: a tile that is a
/// PERSON or a TAG has no ratingKey and no watch state, so every row this menu can build would be
/// absent and a hold would open an empty panel. That is the detail page's cast headshots and
/// Search's Cast & Crew / Collections rows (`search::Item::Tag` has no rating key at all). Do not
/// add them a host; there is nothing for it to show.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum MenuHost {
    /// a home shelf card
    Home,
    /// the detail page's episode filmstrip
    Detail,
    /// the detail page's RELATED shelf — the same page as [`MenuHost::Detail`] underneath, and a
    /// deliberately separate host because the ACTION means something different there.
    ///
    /// `Detail` is the filmstrip, whose rk is a leaf of the season this page has loaded: its Play
    /// from Start goes through `detail::play_episode_rk_from_start`, and its scrobble re-reads the
    /// page. A Related tile is neither of those things — it is a DIFFERENT item, a card row exactly
    /// like Home's or the Library grid's, and routing it through the filmstrip's arms would look
    /// for it among the loaded episodes, not find it, and do nothing at all. Folding the two into
    /// one variant is therefore the bug, not the simplification.
    Related,
    /// the Library browse grid
    Library,
    /// a Search result shelf (media tiles only)
    Search,
    /// the person page's Movies / Shows shelves
    Person,
}
impl MenuHost {
    /// the route the popover returns to when it closes
    pub(super) fn route(self) -> Route {
        match self {
            MenuHost::Home => Route::Home,
            // both detail-page hosts close back onto the page they stand on
            MenuHost::Detail | MenuHost::Related => Route::Detail,
            MenuHost::Library => Route::Library,
            MenuHost::Search => Route::Search,
            MenuHost::Person => Route::Person,
        }
    }
    /// Whether this host's item is **a leaf of the loaded season** — i.e. whether an action means
    /// the detail page's own episode path rather than the shared card-row one.
    ///
    /// The question `apply_item_action` asks twice (Play from Start, and whether the page must
    /// re-read itself after a scrobble), asked ONCE here so the two cannot drift apart. It was
    /// `matches!(host, MenuHost::Detail)` written out at both sites, which was exactly right while
    /// the filmstrip was the page's only menu — and silently wrong the moment a second one opened
    /// on the same page over an item that is not an episode at all.
    pub(super) fn is_loaded_episode(self) -> bool {
        matches!(self, MenuHost::Detail)
    }
}
/// Which screen a popover on the SHARED TOP BAR is sitting over — the three pages that wear the
/// bar, and so the three the profile chip can be pressed from.
///
/// [`MenuHost`]'s twin, and it exists for the same reason: the profile menu is a popover on a host
/// screen, so the route has to name the screen underneath — both to draw its stationary snapshot
/// and to know where the popover closes back to.
///
/// [`Route::Account`] was a UNIT variant while Home was the only screen whose chip could be
/// pressed, and every one of the dozen-odd places that read it therefore said Home outright: the
/// page under the panel ([`page_of`]), the dismissal's destination ([`key_account`] and the pointer
/// arm), and the host-page lifecycle arm. Making the chip a stop on all three
/// screens without this would have swapped the page under the popover to Home on the press frame —
/// a hard cut, no transition — and then dropped the user on Home when they dismissed it.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum BarHost {
    Home,
    Library,
    Search,
}
impl BarHost {
    /// the route the popover returns to when it closes
    pub(super) fn route(self) -> Route {
        match self {
            BarHost::Home => Route::Home,
            BarHost::Library => Route::Library,
            BarHost::Search => Route::Search,
        }
    }
    /// The bar-wearing page `r` is, if it is one — the list that decides where the chip can be
    /// pressed at all, so both halves of its activation (the key and the pointer) read it here
    /// instead of spelling three routes each.
    pub(super) fn of(r: Route) -> Option<Self> {
        match r {
            Route::Home => Some(BarHost::Home),
            Route::Library => Some(BarHost::Library),
            Route::Search => Some(BarHost::Search),
            _ => None,
        }
    }
}
/// [`MenuHost`] as the focus probe's own mirror of it. A free fn, not `MenuHost::probe`, for
/// `node_route`'s reason: `focusprobe::Host` is another module's type and an inherent `impl` here
/// would be a foreign one. Exhaustive, so a new host cannot fingerprint as the wrong screen.
pub(super) fn probe_host(h: MenuHost) -> crate::focusprobe::Host {
    match h {
        MenuHost::Home => crate::focusprobe::Host::Home,
        // both fingerprint as the detail page, because that is the page that is live under them
        MenuHost::Detail | MenuHost::Related => crate::focusprobe::Host::Detail,
        MenuHost::Library => crate::focusprobe::Host::Library,
        MenuHost::Search => crate::focusprobe::Host::Search,
        MenuHost::Person => crate::focusprobe::Host::Person,
    }
}
/// [`BarHost`] as the focus probe's mirror of it — [`probe_host`]'s twin, for the twin reason. The
/// probe has ONE `Host` vocabulary for "which page is live under this panel", and this is the
/// narrower popover's half of it: the three bar-wearing screens map onto three of its five, and the
/// two the account menu can never stand on are unreachable from here BY TYPE rather than by
/// comment.
pub(super) fn probe_bar_host(h: BarHost) -> crate::focusprobe::Host {
    match h {
        BarHost::Home => crate::focusprobe::Host::Home,
        BarHost::Library => crate::focusprobe::Host::Library,
        BarHost::Search => crate::focusprobe::Host::Search,
    }
}
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Route {
    Login,    // plex.tv sign-in (QR) — shown when there's no usable session
    Profiles, // "who's watching" Plex Home picker
    /// **"Which libraries do you want?"** — the *Favorite libraries* route (`screens::onboard`,
    /// an OWNED screen since phase 5b: this route still names the page, but the dispatcher mounts,
    /// steps, focuses and draws it, and the loop's ladders never see its keys), the
    /// third and last onboarding screen and the only one that is not about credentials: which of
    /// the granted libraries this profile wants, asked once PER PROFILE and only when the roster
    /// holds more than one. It asked "what goes on your Home?" until 2026-09-05, and both the words
    /// and the SCOPE changed: favourites fill Home's shelves, decide which type pills the top strip
    /// draws at all, and scope the Library's own Sources picker. The grant is untouched, and Search
    /// still reaches every granted library. Between the picker and
    /// Home, so a household member answers for themselves rather than inheriting the answer of
    /// whoever set the television up.
    Onboard,
    Home,
    /// `over` + the top-left profile menu popover (change profile / sign out). The chip is
    /// SHARED chrome, so the page underneath is whichever of the three wears the bar — see
    /// [`BarHost`].
    Account {
        over: BarHost,
    },
    /// `over` + the press-and-hold context menu popover (ui/item_menu.rs)
    ItemMenu {
        over: MenuHost,
    },
    Library, // owned screens/library page; sort/filter/source panels are LibraryMenu entries
    Detail,
    /// The person/actor page (screens/person.rs), reached by OK on a detail page's cast
    /// headshot. Exclusive with Detail like every other node — what is UNDER it is the BACK
    /// trail's business (`ui::trail`), not this enum's, which is exactly why the trail
    /// exists: a `Route` names one screen, and person→detail→person is three.
    Person,
    /// The Search screen (`ui/search/`). A PEER of Home and the Library, not a stacking
    /// page: it is reached from the strip's last pill and BACK from it returns to Home, so
    /// it needs no trail node of its own — what it OPENS stacks, but it does not.
    Search,
    Player {
        overlay: Overlay,
    },
}

/// Which routes draw the shared top tab bar — the ONE test behind `ui::nav`'s
/// continuous-chrome rule. Exhaustive for the same reason `Nav::wears_tab_bar` is: a new
/// screen must not be able to answer this by accident. (Both popovers draw a live page
/// underneath — `Account` one of the three bar screens, `ItemMenu over Home` Home — so the bar
/// is on screen there too; Detail and Person do not have one,
/// which is what makes every transition to or from them fade the bar with the page.)
pub(super) fn route_wears_tab_bar(r: Route) -> bool {
    match r {
        Route::Home | Route::Library | Route::Search => true,
        // Both popovers DERIVE the answer from the screen they are drawn ON, rather than
        // answering `true` outright: a `BarHost` that did not wear the bar could not make this
        // line a lie, and a menu over the Library wears the bar because the Library does — which
        // is the only way this stays right as hosts are added on either side.
        Route::Account { over } => route_wears_tab_bar(over.route()),
        Route::ItemMenu { over } => route_wears_tab_bar(over.route()),
        Route::Login
        | Route::Profiles
        | Route::Onboard
        | Route::Detail
        | Route::Person
        | Route::Player { .. } => false,
    }
}
/// The PAGE a trail node names — the ONE Node→[`Route`] mapping in the app. Both things
/// that have to know it read it here: `enter_node` flips the route through it after
/// mounting, and [`node_wears_tab_bar`] answers the chrome question by handing it to
/// [`route_wears_tab_bar`], so a node and the page it mounts can never answer differently.
///
/// A free fn and not `Node::route`, which is what it would rather be: `Node` belongs to
/// `ui::trail` (deliberately — the trail decides nothing about screens and cannot see `Route`),
/// so the inherent `impl` would be a foreign one, which `non_local_definitions` warns about.
pub(super) fn node_route(n: &Node) -> Route {
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
pub(super) fn node_wears_tab_bar(n: &Node) -> bool {
    route_wears_tab_bar(node_route(n))
}
/// The PAGE a route draws. Both popovers sit on a LIVE screen — an `ItemMenu` on the one holding
/// the card, an `Account` on whichever of the three wears the shared top bar — so the page being
/// left by a navigation out of either is the screen underneath, which is what both the teardown and
/// the spot below have to be asked about.
///
/// DRAW always asks it. UPDATE is a separate policy: an item menu keeps its anchored page live,
/// while the profile menu freezes its page and takes one cached glass snapshot of it.
pub(super) fn page_of(r: Route) -> Route {
    match r {
        Route::ItemMenu { over } => over.route(),
        Route::Account { over } => over.route(),
        other => other,
    }
}

/// Does the page named by [`page_of`] keep stepping while a surface above it owns interaction?
///
/// Drawing and updating are deliberately separate questions.  A compact popover still needs its
/// host pixels behind it, but neither the profile menu nor a full-screen Settings/first-run route
/// benefits from advancing an invisible focus tree. The profile menu uses cached glass for that
/// same lifetime, so no hidden animation or repeated snapshot work remains.
///
/// `full_screen_modal` is the CONTAINER's fold since phase 5b (`bridge::host_frozen`, i.e.
/// `modal::host_policy`'s `HostUpdate`), not two `is_open()` reads the loop had to remember to OR
/// together: the surfaces on the tree answer it, so a surface added later freezes its host by
/// declaring a `Style` rather than by being added to a condition here. `Route::Account` stays this
/// function's own, because the profile menu is still a legacy popover.
pub(super) fn host_page_updates(r: Route, full_screen_modal: bool) -> bool {
    !full_screen_modal && !matches!(r, Route::Account { .. })
}

#[cfg(test)]
mod host_page_lifecycle_tests {
    use super::*;

    #[test]
    fn full_screen_routes_and_the_profile_menu_freeze_the_hidden_page() {
        assert!(!host_page_updates(Route::Home, true));
        assert!(!host_page_updates(
            Route::Account {
                over: BarHost::Home,
            },
            false,
        ));
        assert!(host_page_updates(
            Route::ItemMenu {
                over: MenuHost::Home,
            },
            false,
        ));
    }
}

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
pub(super) fn leave_of(r: Route) -> Option<fn()> {
    match page_of(r) {
        Route::Detail => None,
        Route::Person => None,
        // Nothing loaded that outlives the page. Home and the Library keep their stores for
        // as long as the profile does (`browse.rs` is re-ENTERED, never re-queried — that is
        // why `Node::Library` carries no payload), Login/Profiles/Onboard are boot gates the app
        // leaves once, and a player session is torn down by its own exit path.
        Route::Home
        | Route::Library
        | Route::Login
        | Route::Profiles
        | Route::Onboard
        | Route::Player { .. } => None,
        // Search DOES have one, and it is not a store: the television's keyboard must come
        // down with the page. Dismissing it at the press instead would drop the panel a
        // frame early, while the screen it belongs to is still on screen behind it.
        Route::Search => Some(crate::ui::search::leave as fn()),
        // Unreachable: `page_of` has already resolved a popover onto the screen it sits on,
        // so neither of these ever arrives here. Listed rather than swept into a `_` so the
        // exhaustiveness above is real.
        Route::Account { .. } | Route::ItemMenu { .. } => None,
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
/// [`Route::Search`] is the case that made this a predicate rather than a `None`, and it is the
/// one route where the two questions this file otherwise collapses genuinely come apart. It HAS a
/// node now and a result opened from it does stay on the trail — but its teardown is
/// `search::leave`, which dismisses the TELEVISION'S KEYBOARD and drops nothing else, so running it
/// on the way deeper costs nothing and leaving it un-run risks a system panel floating over the
/// page you navigated to. The other three keep their teardown off a forward navigation because
/// theirs EMPTY the page a BACK is about to return to; this one has nothing to empty.
///
/// So `false` here does not mean "no node" any more. It means "leaving this screen always dismisses
/// its keyboard", and the trail push lives in the commit arm, which is where it always did.
pub(super) fn stays_on_trail(r: Route) -> bool {
    match page_of(r) {
        // exactly the `Node` variants (`node_route`'s domain): a forward navigation leaves these
        // standing behind the destination, which is what makes the common pop a route flip
        Route::Home | Route::Library | Route::Detail | Route::Person => true,
        // stays on the trail, but its teardown rides every exit — see the doc above
        Route::Search => false,
        // Boot gates the app leaves once, and a player session torn down by its own exit path.
        // None of the four has a `leave_of` at all, so this answer is about being honest rather
        // than about having an effect.
        Route::Login | Route::Profiles | Route::Onboard | Route::Player { .. } => false,
        // Unreachable: `page_of` resolves a popover onto the screen it sits on. Listed rather than
        // swept into a `_`, exactly as `leave_of` above.
        Route::Account { .. } | Route::ItemMenu { .. } => false,
    }
}
/// The teardown a FORWARD navigation off `cur` carries — [`stays_on_trail`] and [`leave_of`]
/// composed, so the two halves of the rule are stated once and cannot drift apart at the two call
/// sites (`nav_to` and `nav_open`).
pub(super) fn forward_leave(cur: Route) -> Option<fn()> {
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
// see a `Route`. `Nav`, `NavReq` and `Modal` sit behind the same wall.

// ---- the modal overlay: which panel owns the frame, and what its rows do ----------------------
/// Which panel owns the frame — the ONE place that decision lives, read by the pointer
/// arm (and, when the z bands land, the draw composition) so they cannot drift. The key
/// path was always modal for every overlay (each arm `continue`s); the CLICK path used to
/// special-case only Menu, so a click with the Info card up fell through onto the
/// partly-hidden transport's compile-time rects and started a blind scrub-seek.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Modal {
    None,
    Account,
    ItemMenu,
    Menu,
    Info,
    Chapters,
    More,
}
pub(super) fn modal_of(r: Route) -> Modal {
    match r {
        Route::Account { .. } => Modal::Account,
        Route::ItemMenu { .. } => Modal::ItemMenu,
        Route::Player {
            overlay: Overlay::Menu,
        } => Modal::Menu,
        Route::Player {
            overlay: Overlay::Info,
        } => Modal::Info,
        Route::Player {
            overlay: Overlay::Chapters,
        } => Modal::Chapters,
        Route::Player {
            overlay: Overlay::More,
        } => Modal::More,
        _ => Modal::None,
    }
}
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
pub(super) enum Nav {
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
    pub(super) fn select_pill(&self) -> Option<usize> {
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
    pub(super) fn wears_tab_bar(&self) -> bool {
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
pub(super) struct NavReq {
    pub(super) to: Nav,
    pub(super) from: Route,
    /// Where the page being LEFT was standing, snapshotted at the PRESS (`detail::spot`'s
    /// own contract) and written onto its trail node at the floor. Carried rather than
    /// re-read at the commit because the user can still move focus during the 70 ms, and
    /// BACK must return them to where they pressed, not to where the fade found them.
    pub(super) spot: Option<Spot>,
    pub(super) entry: Option<crate::ui::machine::EntryId>,
    pub(super) owner: Option<crate::ui::machine::InputOwner>,
    pub(super) ret: Option<crate::ui::screen::ReturnState<u32, crate::screens::registry::PageMemory>>,
}

impl NavReq {
    pub(super) fn is_current(&self, route: Route, entry: Option<crate::ui::machine::EntryId>, owner: Option<crate::ui::machine::InputOwner>) -> bool {
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
pub(super) fn leaving_spot(_cur: Route) -> Option<Spot> {
    None // owned pages capture their engine focus through the bridge's ReturnState
}/// Ask for `to`, through the page cross-fade, carrying the outgoing page's teardown.
///
/// **Both halves of a route change land at the floor**: the outgoing page's teardown and
/// the incoming page's mount. That uniformity is the design — the alternative is a per-arm
/// judgement about which stores the screen still on screen happens to read, and the arm
/// that gets it wrong blanks a page in the middle of its own fade. It costs the ~70 ms of
/// `OUT_MS` before a detail fetch is issued, which the fade is spending anyway and the
/// page's own spinner already covers.
pub(super) fn nav_req(cur: Route, to: Nav, leave: Option<fn()>, pending: &mut Option<NavReq>) {
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
pub(super) fn nav_to(cur: Route, to: Nav, pending: &mut Option<NavReq>) {
    nav_req(cur, to, forward_leave(cur), pending);
}
/// Open a stacking page (detail / person) through the transition — the ONE forward entry to
/// both, so a new way in cannot push without routing or route without pushing. The mount
/// and the push both happen at the fade floor; see [`nav_req`].
pub(super) fn nav_open(cur: Route, node: Node, season: Option<c_int>, pending: &mut Option<NavReq>) {
    nav_req(cur, Nav::Open { node, season }, forward_leave(cur), pending);
}
/// BACK off a stacking page, through the transition. The page IS being left for good, so
/// its teardown rides the request; the trail is only PEEKED here (`Trail::under`) and the
/// pop itself happens at the floor, so a second BACK inside the window withdraws this one
/// instead of popping a page that is still on screen.
pub(super) fn nav_back(cur: Route, trail: &Trail, pending: &mut Option<NavReq>) {
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
pub(super) fn nav_cancel(cur: Route, pending: &mut Option<NavReq>) -> bool {
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
pub(super) fn to_detail(sid: crate::plex::ServerId, rk: &str) -> Node {
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
pub(super) enum Origin {
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
/// [`page_of`] first, so a launch from a popover returns to the page the popover was drawn ON: the
/// item context menu's *Play from Start* is dispatched with the route already flipped back to its
/// host, but the account menu and a future panel need not be, and asking `page_of` costs nothing.
pub(super) fn return_page(r: Route, detail: Option<Node>, person: Option<Node>) -> Node {
    match page_of(r) {
        Route::Detail => detail.unwrap_or(Node::Home),
        Route::Person => person.unwrap_or(Node::Home),
        Route::Library => Node::Library,
        Route::Search => Node::Search,
        // Home is the root and the honest answer for the four boot gates as well. `Player` is
        // unreachable — every caller is a launch, which is off the player route by definition —
        // and lands here rather than being a variant the compiler makes anyone think about.
        Route::Home | Route::Login | Route::Profiles | Route::Onboard | Route::Player { .. } => {
            Node::Home
        }
        // …and the two popovers cannot reach this arm at all: `page_of` above resolved them.
        Route::Account { .. } | Route::ItemMenu { .. } => Node::Home,
    }
}

/// The page on screen NOW, as an [`Origin`] — [`return_page`] fed from the live stores.
///
/// The detail node carries the page's [`Spot`], so a return is a RESTORE (the Related tile the user
/// pressed on is still the focused one) rather than a fresh arrival at the hero. An empty mounted
/// rk means the page never mounted, which is not a page anyone can be returned to.
pub(super) fn origin_here(r: Route, trail: &Trail) -> Origin {
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
pub(super) fn set_origin(play_from: &mut Node, from: Origin) {
    if let Origin::From(n) = from {
        *play_from = n;
    }
}


/// Enter `rk`'s detail page with a HARD CUT — no transition. The one caller left is the
/// `/tmp/plxnative-detail` boot trigger, and the reason is the same one the Library boot
/// trigger gives: at boot there is no outgoing screen to replace, so a dip would fade the
/// page up out of nothing and read as a slow app rather than a navigated one. Every
/// INTERACTIVE way in goes through [`nav_open`] instead.
pub(super) fn push_detail(trail: &mut Trail, route: &mut Route, sid: crate::plex::ServerId, rk: &str) {
    trail.push(to_detail(sid, rk));
    *route = Route::Detail;
}

/// The trail bookkeeping an item-menu navigation performs on the page it is LEAVING.
///
/// Over HOME the popover is the user acting on the root, exactly as `home_activate` is, so
/// the history behind them is spent. That truncation stays on the PRESS frame while the
/// push it precedes moves to the fade floor, and the asymmetry is deliberate: Home is
/// `stack[0]`, so a reset to the root is idempotent and survives a withdrawn transition
/// unharmed, whereas a PUSH or a POP is history the user would actually lose.
///
/// Over the DETAIL page there is nothing to do here any more — where that page was standing
/// is `NavReq::spot`'s job now, recorded uniformly for every navigation off a detail page
/// rather than by this one arm remembering to.
///
/// **And nothing over the Library, Search or the person page either**, which is the answer a new
/// host wants by default: navigating out of the menu there is the same forward move the tile's own
/// OK makes (`open_library_card`, `search::on_ok`, `open_person_card`), so `nav_open` stacks and
/// BACK comes back to the grid or shelf the card is sitting on. Home is the exception BECAUSE it is
/// the root, not because it is a menu host.
pub(super) fn menu_leave(trail: &mut Trail, host: MenuHost) {
    if matches!(host, MenuHost::Home) {
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
pub(super) fn enter_node(n: &Node, route: &mut Route) {
    // The navigation container mounts or uncovers the entry at this commit.
    *route = node_route(n);
}

/// The same popover on a card surface that is NOT Home: the Library grid, a Search result shelf,
/// the person page's filmography and the detail page's RELATED shelf — all of which already arm the
/// identical press.
///
/// One function for the four because they differ in exactly two values — the focused row and the
/// [`Opener`] that draws it — and in nothing else. There is no `from_deck` on any of them: the
/// Continue Watching deck is a HOME hub, and offering to remove a Library tile from it would be a
/// row that appeared to work and changed nothing (`item_menu::build`'s own rule).
///
/// The Related shelf joining this list rather than `open_episode_menu` is the whole shape of that
/// fix: it sits on the detail page, but its tiles are OTHER items, so it is a card row like the
/// other three and not a leaf of the loaded season (see [`MenuHost::Related`]).
pub(super) fn open_tile_menu(
    route: &mut Route,
    host: MenuHost,
    item: Option<&crate::pms::PmsMovie>,
    opener: Opener,
    from_deck: bool,
) -> bool {
    let Some(m) = item else { return false };
    if !crate::ui::item_menu::has_actions(m) {
        return false;
    }
    // **`from_deck` is what puts *Remove from Continue Watching* in the panel**, and it was
    // hard-wired `false` here — correct while none of these four surfaces HAD a deck, and wrong
    // from the moment the Library grew the library's own Continue Watching shelf. The shelf
    // arrived, `library::focused_from_deck` was written to answer for it, and nothing called it:
    // the one row a section deck exists to offer was unreachable on the very hold that was added
    // to reach it. It is a parameter now, so a caller with a deck has to say so and a caller
    // without one says `false` in its own words.
    crate::ui::item_menu::open(m, from_deck, opener);
    *route = Route::ItemMenu { over: host };
    true
}
