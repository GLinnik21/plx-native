//! `trail` — the BACK trail: which pages are behind the one on screen.
//!
//! Sibling of [`nav`](crate::ui::nav), and the division between them is worth stating once: `nav`
//! owns the MOTION of a route change (the dip, and which chrome rides across it); this owns the
//! HISTORY of route changes. Neither knows the other exists.
//!
//! It replaces `app.rs`'s `opened_from_library` / `opened_from_person` pair, which were a precedence
//! LADDER — person outranks library outranks Home — with exactly one slot per screen KIND. A ladder
//! cannot represent a page standing on ANOTHER page of the same kind, and the detail page stands on
//! itself all the time: the episode filmstrip's text row and the Related shelf both open a second
//! detail page over the first. BACK from an episode page therefore fell past the show it was opened
//! from and landed on Home. A ladder needs a new rung for every screen that can stack; a stack does
//! not need one at all.
//!
//! Only [`Node::Detail`] and [`Node::Person`] ever stack in practice, so a real trail reads
//! `[Home, Library?, (Detail|Person)*]` — but nothing here enforces that shape, because not having
//! to know it is the whole value of a stack.
//!
//! **This module decides nothing about screens.** It owns the trail's ORDER and its bounds; putting
//! a page back on screen is `app.rs`'s `enter_node` ritual. That seam is the same one `item_menu.rs`
//! draws (it only REPORTS an `Action`; `app.rs` performs it), and here it is what makes the decision
//! host-testable at all: the BACK arm itself lives inside the SDL event loop, where no host test can
//! reach it, so everything it decides has to be liftable into a value first.
//!
//! Main-thread only — an `app.rs` run-loop LOCAL, exactly like the booleans it replaces. Navigation
//! history belongs to the loop that navigates, not to a static.

use crate::plex::ServerId;
use crate::ui::detail::Spot;

/// One page in the trail — everything needed to put it back WITHOUT the screen that pushed it.
///
/// The payloads are the arguments each screen's own entry point already takes, so re-entry is a
/// call rather than a reconstruction: [`Node::Person`]'s fields are `ui::person::reopen`'s
/// parameters (the header the cast row handed over — name and headshot — which is why a re-opened
/// person page renders before its fetch lands), and [`Node::Detail`]'s `(sid, rk)` is
/// `detail::open_rk`'s.
///
/// **Identity, never an index.** A hub refetch rebuilds the catalog wholesale (Continue Watching
/// re-sorts by `lastViewedAt`), so an index stored here would name a different movie by the time
/// BACK is pressed — the bug `detail::reselect` exists for. `detail::mount_rk` re-derives the row
/// from the rk through `pms::index_of_rk` at re-entry instead.
///
/// **…and identity is SERVER-SCOPED.** A ratingKey and a `personId` are both server-local integers
/// dense from 1, so once a shared server is registered a bare key names a page on no machine in
/// particular: browse the share, open one of its items, go deeper, press BACK — and the rk alone
/// re-opens *our* item with that number, a page the user has never seen. Every node therefore
/// carries the `ServerId` its key belongs to, and identity is asked through [`Node::same_page`].
///
/// That per-node `sid` is deliberately the answer to a question [`Trail::reset`] raises: its doc
/// calls a truncation the profile-switch guard, because "a new identity must never be able to walk
/// back into the previous one's pages". A SERVER switch has the same shape — but not the same
/// remedy, because the trail's other rule is that a cycle is a HISTORY (`a_cycle_is_a_history…`)
/// and a source switch does not revoke access to what is behind you. Scoping each node keeps the
/// history walkable and still lands every pop on the page it names; wiping it would spend the
/// user's history for a change of source they may undo in one press.
#[derive(Clone, PartialEq, Debug)]
pub(crate) enum Node {
    /// The root, and the only node BACK never leaves — see [`Trail::back`].
    Home,
    /// The browse grid. No payload: `browse.rs` holds the section, focus and scroll for as long as
    /// the profile does, so re-entry is a route flip (`library::enter` would re-query and lose it).
    Library,
    Person {
        /// the server `key` is a `personId` on — `crate::person::Person::sid`
        sid: ServerId,
        /// the LOCAL `personId` — `crate::person::Person::key`
        key: String,
        /// the `tagKey` guid plex.tv answers to. **Global**, unlike `key`: it is the metadata
        /// provider's id, identical on every server that ever heard of this person, which is why
        /// [`Node::same_page`] prefers it whenever both sides have one.
        guid: String,
        name: String,
        thumb: String,
    },
    /// The Search screen. **No payload, for [`Node::Library`]'s exact reason**: the query, the
    /// shelves, the zone and both cursors all live in `crate::search` and `ui::search`'s own
    /// statics, and nothing between opening a result and coming back disturbs them — so re-entry is
    /// a route flip that restores the page whole, focus included, without asking a single server
    /// again.
    ///
    /// **It stacks, and for one release it deliberately did not.** The reasoning then was that
    /// Search is a peer of Home reached from the shared strip, so its results stack on Home and
    /// nothing stacks on it — which is true of how you ARRIVE and wrong about what you do next.
    /// Opening a result therefore pushed `Detail` straight onto `Home`, and BACK out of it landed
    /// on the home screen with the query and every shelf gone. Reported from the couch as "back
    /// from search result throws to Home, not to search results"; the trail is `[Home, Search,
    /// Detail]` now and BACK walks it.
    ///
    /// Carrying the query here was the first fix and it was WRONG in a way worth recording, since
    /// it is the obvious shape: the node is built when the screen is ENTERED, and the term is typed
    /// afterwards — so the node holds the empty string the pill press arrived with, and BACK
    /// restored a blank field over the recents list. A payload that has to be refreshed as the page
    /// is used is a payload the page should have kept.
    Search,
    /// `spot` is where the page stood when the user navigated deeper — `Spot::default()` for a page
    /// that has been entered and not yet left (see [`Trail::set_top_spot`]).
    Detail {
        sid: ServerId,
        rk: String,
        spot: Spot,
    },
}

impl Node {
    /// Do two nodes name the SAME PAGE? Not `==`: a node also carries state that identity does not
    /// depend on (a [`Node::Detail`]'s `spot`, a [`Node::Person`]'s handed-over header), so a
    /// derived equality would answer "different page" for the same page scrolled elsewhere.
    ///
    /// **A person is matched by `guid` first.** The `tagKey` is plex.tv's and is global, so it
    /// identifies the person across every server that lists them — and it is the id that survives
    /// the same actor being `personId` 45 on one machine and 812 on another. Only when either side
    /// lacks one (the credit row carried no `tagKey`) does it fall back to the LOCAL `(sid, key)`
    /// pair, which is the server-scoped half.
    pub(crate) fn same_page(&self, other: &Node) -> bool {
        match (self, other) {
            (Node::Home, Node::Home)
            | (Node::Library, Node::Library)
            | (Node::Search, Node::Search) => true,
            (Node::Detail { sid: a, rk: x, .. }, Node::Detail { sid: b, rk: y, .. }) => {
                crate::plex::same_item((*a, x), (*b, y))
            }
            (
                Node::Person {
                    sid: a,
                    key: x,
                    guid: ga,
                    ..
                },
                Node::Person {
                    sid: b,
                    key: y,
                    guid: gb,
                    ..
                },
            ) => same_person((*a, x, ga), (*b, y, gb)),
            _ => false,
        }
    }
}

/// The PERSON-identity rule, as `(server, personId, tagKey)` on each side — pulled out of
/// [`Node::same_page`] so `app.rs`'s "is this person already loaded?" guard can ask it about the
/// live `person::Person` store without first building a throwaway [`Node`], and so the two can
/// never come to disagree.
///
/// The guid wins whenever BOTH sides have one (see [`Node::same_page`]); otherwise the local id
/// decides, scoped to its server. Comparing a present guid against a missing one is deliberately
/// NOT a match by guid — every guid-less credit row would then be the same person as every other.
pub(crate) fn same_person(a: (ServerId, &str, &str), b: (ServerId, &str, &str)) -> bool {
    if !a.2.is_empty() && !b.2.is_empty() {
        a.2 == b.2
    } else {
        crate::plex::same_item((a.0, a.1), (b.0, b.1))
    }
}

/// The trail, top = the page on screen.
pub(crate) struct Trail {
    stack: Vec<Node>,
}

impl Trail {
    /// Trail ceiling. A Related chain is unbounded (PMS's related hubs are near-symmetric, so
    /// A→B→A is two presses) and so is person→detail→person, which makes this a MEMORY bound rather
    /// than a correctness one: 16 nodes is a couple of kilobytes and deeper than any real session,
    /// and BACK still reaches the ROOT PRESS either way because the root is never dropped.
    pub(crate) const CAP: usize = 16;

    pub(crate) fn new() -> Self {
        Self {
            stack: vec![Node::Home],
        }
    }

    /// Enter a page. At [`CAP`](Self::CAP) the OLDEST non-root node is dropped rather than the push
    /// refused: a refused push would make BACK from the 17th page return to the 16th forever — a
    /// loop with no way out — while dropping from the bottom only shortens a history nobody walks
    /// that far back, and keeps Home reachable in at most `CAP` presses.
    pub(crate) fn push(&mut self, n: Node) {
        if self.stack.len() >= Self::CAP {
            self.stack.remove(1);
        }
        self.stack.push(n);
    }

    /// BACK: leave the top page and hand back the one under it. `None` at the ROOT — which is what
    /// keeps BACK at Home reaching THE ROOT PRESS, because `app.rs`'s BACK arm reaches its root
    /// branch by never consulting the trail on Home at all. (What that branch DOES has changed
    /// three times — quit, then a Cancel/Exit alert, and since 2026-09-03 the television's own Home
    /// with the app still running; see `app.rs::back_at_root`. What this module guarantees is
    /// unchanged and is the half that matters here: at the root there is nothing left to pop, so
    /// the press falls through to whatever the app does with a root BACK.)
    ///
    /// Returns by VALUE. The clone is two `String`s once per BACK press, and it buys the caller the
    /// right to touch the trail again inside the re-entry it is about to run.
    pub(crate) fn back(&mut self) -> Option<Node> {
        if self.stack.len() <= 1 {
            return None;
        }
        self.stack.pop();
        self.stack.last().cloned()
    }

    /// The page BACK would land on, WITHOUT leaving the one on screen — `None` at the root, exactly
    /// as [`back`](Self::back) declines there.
    ///
    /// It exists because the pop itself is deferred: a BACK press starts a page transition
    /// (`ui::nav`) and the trail only moves at the fade FLOOR, but the press frame already has to
    /// know whether the destination wears the shared top tab bar. Peeking is the whole of what the
    /// press needs, and keeping it a peek is what makes a second BACK inside the fade window
    /// withdraw the first rather than pop a node whose page is still on screen.
    pub(crate) fn under(&self) -> Option<&Node> {
        self.stack
            .len()
            .checked_sub(2)
            .and_then(|i| self.stack.get(i))
    }

    /// The user acted on Home (or landed there): the history behind them is spent. Truncates to the
    /// root — which is also the profile-switch guard, since a new identity must never be able to
    /// walk back into the previous one's pages.
    pub(crate) fn reset(&mut self) {
        self.stack.truncate(1);
    }

    /// Record where the page on screen was standing, just before navigating off it. A no-op unless
    /// the top is a [`Node::Detail`] — the only page whose place we can restore (the Library and the
    /// person page keep their own live state; Home is the root).
    pub(crate) fn set_top_spot(&mut self, s: Spot) {
        if let Some(Node::Detail { spot, .. }) = self.stack.last_mut() {
            *spot = s;
        }
    }

    /// Make the trail agree with a page we landed on WITHOUT navigating — the player exit and the
    /// Info card's jump-to-detail, neither of which is a trail node.
    ///
    /// Idempotent: no push when the top already names the same page, so an exit back onto the page
    /// playback started from does not double it — which is the ORDINARY case, since playing never
    /// moved the trail. Identity is [`Node::same_page`], so the server is part of the test for the
    /// reason [`Node`]'s doc gives: with a share registered, "the top already names this rk" can be
    /// true of the wrong page.
    ///
    /// **[`Node::Home`] is a truncation, not a push.** Home is `stack[0]` and the one node BACK
    /// never leaves, so landing there means everything that was behind the user is spent — pushing
    /// a second Home would put an extra BACK between them and the door, which is the one place
    /// where a wrong trail is not merely a misnavigation (see [`Trail::back`]).
    ///
    /// It generalises an `ensure_detail(sid, rk)` that took the two halves of a detail page and
    /// could express no other kind. That was exactly enough while playback returned to a detail
    /// page or to Home and to nothing else; it is the trail half of the same defect `app.rs`'s
    /// `Origin` fixes on the route side.
    pub(crate) fn ensure(&mut self, n: &Node) {
        if matches!(n, Node::Home) {
            self.reset();
            return;
        }
        if self.stack.last().map(|t| t.same_page(n)).unwrap_or(false) {
            return;
        }
        self.push(n.clone());
    }
}

// ---------------------------------------------------------------------------------------
#[cfg(test)]
mod tests {
    //! Pure and parallel — this module touches no crate global, which is the point of splitting it
    //! out of the BACK arm. What these CANNOT say is whether a restored page LOOKS like the one that
    //! was left; that is `detail.rs`'s `Spot` tests plus a device capture.
    use super::*;

    /// Two registry slots, never installed — [`ServerId::from_raw`] is a plain value, so the
    /// identity rules below are gradeable without a registry, a socket or the serial lock.
    const A: ServerId = ServerId::from_raw(0);
    const B: ServerId = ServerId::from_raw(1);

    fn det(rk: &str) -> Node {
        det_on(A, rk)
    }
    fn det_on(sid: ServerId, rk: &str) -> Node {
        Node::Detail {
            sid,
            rk: rk.to_string(),
            spot: Spot::default(),
        }
    }
    fn person(key: &str) -> Node {
        Node::Person {
            sid: A,
            key: key.into(),
            guid: String::new(),
            name: String::new(),
            thumb: String::new(),
        }
    }
    fn person_guid(sid: ServerId, key: &str, guid: &str) -> Node {
        Node::Person {
            sid,
            key: key.into(),
            guid: guid.into(),
            name: String::new(),
            thumb: String::new(),
        }
    }

    /// The executable form of "BACK at the Home root REACHES THE ROOT PRESS": the trail declines,
    /// and the arm falls through to its root branch (`app.rs::back_at_root`, which today shows the
    /// television's own Home). Whatever that branch does, this module's obligation is the same and
    /// nothing else in it may make it untrue: at the root there is nothing to pop.
    #[test]
    fn a_fresh_trail_is_the_root_and_back_there_declines() {
        let mut t = Trail::new();
        assert_eq!(t.stack, vec![Node::Home]);
        assert_eq!(t.back(), None);
        assert_eq!(
            t.stack,
            vec![Node::Home],
            "a declined pop must not consume the root either"
        );
    }

    /// The reported bug, as a value: a detail page opened on top of another detail page (the
    /// filmstrip's text row, or the Related shelf) must come back to the one it was opened FROM.
    #[test]
    fn detail_stacks_on_detail_and_pops_in_reverse() {
        let mut t = Trail::new();
        for rk in ["a", "b", "c"] {
            t.push(det(rk));
        }
        assert_eq!(t.back(), Some(det("b")));
        assert_eq!(t.back(), Some(det("a")));
        assert_eq!(t.back(), Some(Node::Home));
        assert_eq!(t.back(), None, "…and Home is still the floor");
    }

    /// **The reported bug, one screen over**: a result opened from Search must come back to the
    /// results. It landed on Home instead, because Search pushed no node and the detail page went
    /// straight onto the root — so the query, the shelves and the scroll were all gone with one
    /// press. The term rides the node, which is what makes the restore a restore.
    #[test]
    fn a_result_opened_from_search_comes_back_to_the_search() {
        let mut t = Trail::new();
        t.push(Node::Search);
        t.push(det("a"));
        assert_eq!(
            t.back(),
            Some(Node::Search),
            "BACK returns to the results, not to Home"
        );
        assert_eq!(
            t.back(),
            Some(Node::Home),
            "…and BACK again is Home, as a peer of it should be"
        );

        // A person reached from a Cast & Crew hit stacks the same way, and so does a detail page
        // opened from THAT — the two-deep case that is the whole reason this is a stack.
        let mut t = Trail::new();
        t.push(Node::Search);
        t.push(person("9"));
        t.push(det("b"));
        assert_eq!(t.back(), Some(person("9")));
        assert_eq!(t.back(), Some(Node::Search));

        assert!(Node::Search.same_page(&Node::Search));
        assert!(!Node::Search.same_page(&Node::Home));
        assert!(!Node::Search.same_page(&det("a")));
    }

    /// A Library-opened page whose Related shelf is used must not skip the page it came from on the
    /// way back — the "one level too far" failure the two booleans had (`opened_from_library` was
    /// still set on the SECOND detail page, so one BACK jumped straight to the grid).
    #[test]
    fn a_related_hop_off_a_library_page_comes_back_through_that_page() {
        let mut t = Trail::new();
        t.push(Node::Library);
        t.push(det("a"));
        t.push(det("b"));
        assert_eq!(t.back(), Some(det("a")));
        assert_eq!(t.back(), Some(Node::Library));
        assert_eq!(t.back(), Some(Node::Home));
    }

    /// person → detail → person → detail interleave, which is why this is ONE stack and not two:
    /// with a stack per kind, BACK would have to compare timestamps across them to know which of the
    /// two nearest nodes is actually nearer.
    #[test]
    fn person_and_detail_interleave_on_one_stack() {
        let mut t = Trail::new();
        t.push(det("a"));
        t.push(person("p1"));
        t.push(det("b"));
        t.push(person("p2"));
        assert_eq!(t.back(), Some(det("b")));
        assert_eq!(t.back(), Some(person("p1")));
        assert_eq!(t.back(), Some(det("a")));
        assert_eq!(t.back(), Some(Node::Home));
    }

    /// PMS's related hubs are near-symmetric, so A→B→A is two presses. Revisits are deliberately NOT
    /// collapsed: A→B→A is three VISITS, and folding the second A onto the first would send BACK
    /// from it past B, to a page the user has not been on since.
    #[test]
    fn a_cycle_is_a_history_not_a_loop() {
        let mut t = Trail::new();
        for rk in ["a", "b", "a", "b"] {
            t.push(det(rk));
        }
        assert_eq!(t.back(), Some(det("a")));
        assert_eq!(t.back(), Some(det("b")));
        assert_eq!(t.back(), Some(det("a")));
        assert_eq!(t.back(), Some(Node::Home));
    }

    /// The cap drops from the BOTTOM, never the top, and never the root — so a session that walks a
    /// cycle forever still terminates, in at most `CAP` presses, on Home.
    #[test]
    fn the_cap_drops_the_oldest_non_root_and_never_the_root() {
        let mut t = Trail::new();
        for i in 0..(Trail::CAP + 4) {
            t.push(det(&format!("d{i}")));
        }
        assert_eq!(t.stack.len(), Trail::CAP);
        assert_eq!(
            t.stack[0],
            Node::Home,
            "the root is what makes BACK reach the root press"
        );
        assert_eq!(
            t.stack[1],
            det(&format!("d{}", Trail::CAP + 4 - (Trail::CAP - 1)))
        );

        let mut steps = 0;
        while t.back().is_some() {
            steps += 1;
            assert!(steps < Trail::CAP, "a pop loop must reach the root");
        }
    }

    /// A spot describes a DETAIL page's place, so it is recorded only onto one. The other three
    /// nodes keep their own live state (or are the root), and writing onto them would be a value
    /// nothing ever reads.
    #[test]
    fn a_spot_is_recorded_only_on_a_detail_top() {
        let s = Spot {
            section: 2,
            col: 5,
            ..Default::default()
        };
        for top in [Node::Home, Node::Library, person("p1")] {
            let mut t = Trail::new();
            t.push(top.clone());
            t.set_top_spot(s.clone());
            assert_eq!(
                t.stack.last(),
                Some(&top),
                "{top:?} has no place to restore"
            );
        }
        let mut t = Trail::new();
        t.push(det("a"));
        t.set_top_spot(s.clone());
        assert_eq!(
            t.stack.last(),
            Some(&Node::Detail {
                sid: A,
                rk: "a".into(),
                spot: s
            })
        );
    }

    /// The player exit and the Info card's jump both LAND on a page without navigating to it.
    /// Landing back on the page playback started from must not double it; landing on a different one
    /// must push.
    #[test]
    fn ensure_is_idempotent_for_the_page_already_on_top() {
        let mut t = Trail::new();
        t.push(det("a"));
        t.ensure(&det_on(A, "a"));
        t.ensure(&det_on(A, "a"));
        assert_eq!(
            t.stack.len(),
            2,
            "the page playback started from is already the top"
        );

        t.ensure(&det_on(A, "b"));
        assert_eq!(t.stack.len(), 3);
        assert_eq!(t.back(), Some(det("a")));

        // …and over a non-Detail top it always pushes: there is nothing there to be the same page.
        let mut t = Trail::new();
        t.push(person("p1"));
        t.ensure(&det_on(A, "a"));
        assert_eq!(t.back(), Some(person("p1")));
    }

    /// [`Trail::ensure`] over the OTHER pages playback can now return to. Each is a no-op in the
    /// ordinary case, because playing never moved the trail — the page is still the top — and a
    /// push only when something put the app on a page nothing navigated to (a dev trigger boot).
    #[test]
    fn ensure_lands_every_kind_of_page_the_player_can_return_to() {
        for (top, want) in [(Node::Library, Node::Library), (Node::Search, Node::Search)] {
            let mut t = Trail::new();
            t.push(top);
            t.ensure(&want);
            assert_eq!(
                t.stack.len(),
                2,
                "the page is already the top — nothing to push"
            );
            assert_eq!(
                t.back(),
                Some(Node::Home),
                "…and one BACK off it is Home, as before"
            );
        }
        // A boot trigger that mounts the Library without navigating: the trail is bare, so the
        // return has to build the step it never took, or BACK would leave the app from the grid.
        let mut t = Trail::new();
        t.ensure(&Node::Library);
        assert_eq!(t.back(), Some(Node::Home));

        // The person page, matched through the same identity rule as everywhere else.
        let mut t = Trail::new();
        t.push(person("p1"));
        t.ensure(&person("p1"));
        assert_eq!(t.stack.len(), 2);
        t.ensure(&person("p2"));
        assert_eq!(t.stack.len(), 3, "a different person is a different page");
    }

    /// **BACK at the Home root REACHES THE ROOT PRESS** (the television's own Home since
    /// 2026-09-03; a Cancel/Exit alert before that, and the exit itself before that), so a return
    /// to Home is a TRUNCATION and never a push: a second `Node::Home` on the stack would put an
    /// extra BACK between the user and that press, which is the one place a wrong trail is not
    /// merely a misnavigation.
    ///
    /// The other half of the same rule: playing a Continue Watching card off Home and stopping it
    /// must leave the user exactly one BACK from the door, whatever page they had been on before
    /// they came back to Home.
    #[test]
    fn a_return_to_home_leaves_exactly_one_back_between_the_user_and_the_root_press() {
        let mut t = Trail::new();
        t.push(Node::Library);
        t.push(det("a"));
        t.ensure(&Node::Home);
        assert_eq!(
            t.stack,
            vec![Node::Home],
            "the history behind them is spent"
        );
        assert_eq!(t.back(), None, "…and BACK there is the door, not a pop");

        // Idempotent, so an exit onto Home from Home cannot deepen it either.
        let mut t = Trail::new();
        t.ensure(&Node::Home);
        t.ensure(&Node::Home);
        assert_eq!(t.stack, vec![Node::Home]);
        assert_eq!(t.back(), None);
    }

    /// The reported shape of the shared-server bug, as a value: browse the SHARE, open its item 42,
    /// press BACK — and land on a page you have never seen, because *our* server also has a 42. A
    /// trail node names `(server, key)`, so the two are two pages and the stack keeps both.
    #[test]
    fn one_rating_key_on_two_servers_is_two_pages_in_the_history() {
        let (ours, theirs) = (det_on(A, "42"), det_on(B, "42"));
        assert!(
            !ours.same_page(&theirs),
            "the same key on two servers is not the same page"
        );
        assert!(ours.same_page(&det_on(A, "42")));
        assert!(!ours.same_page(&det_on(A, "43")));

        let mut t = Trail::new();
        t.push(ours.clone());
        t.push(theirs.clone());
        assert_eq!(t.stack.len(), 3, "two pages, not one");
        assert_eq!(
            t.back(),
            Some(ours),
            "BACK returns to OUR 42, the page it was opened from"
        );

        // …and the same rule through `ensure`: landing on the share's 42 while ours is the
        // top must PUSH, not read as "already here" and leave the trail describing the wrong page.
        let mut t = Trail::new();
        t.push(det_on(A, "42"));
        t.ensure(&det_on(B, "42"));
        assert_eq!(t.stack.len(), 3);
        assert_eq!(t.back(), Some(det_on(A, "42")));
    }

    /// A person is matched by the plex.tv `tagKey` when both sides have one — it is GLOBAL, so the
    /// same actor is one page whichever server's credit row opened them (their local `personId`
    /// differs per machine). With no guid the fallback is the server-scoped `(sid, key)` pair, and
    /// two servers' person 45 are then two different people.
    #[test]
    fn a_person_is_the_same_page_by_guid_and_a_local_id_is_server_scoped() {
        let g = "5d77682aeb5d26001f1de4b0";
        assert!(
            person_guid(A, "45", g).same_page(&person_guid(B, "812", g)),
            "one guid is one person, whatever each server numbers them"
        );
        assert!(!person_guid(A, "45", g).same_page(&person_guid(A, "45", "other-guid")));
        // no guid on either side → the LOCAL id decides, and it only means anything per server
        assert!(person_guid(A, "45", "").same_page(&person_guid(A, "45", "")));
        assert!(!person_guid(A, "45", "").same_page(&person_guid(B, "45", "")));
        // one side missing a guid falls back too — comparing "" to a real guid would make every
        // guid-less credit row the same person as every other
        assert!(person_guid(A, "45", "").same_page(&person_guid(A, "45", g)));
        assert!(!person_guid(A, "45", "").same_page(&person_guid(B, "45", g)));
    }

    /// `same_page` is about the PAGE, not about the node's other state: a detail page scrolled
    /// somewhere else is the same page (which is what makes `ensure` idempotent after a
    /// player exit), and two different KINDS of page never match.
    #[test]
    fn same_page_ignores_the_place_and_never_crosses_kinds() {
        let scrolled = Node::Detail {
            sid: A,
            rk: "a".into(),
            spot: Spot {
                section: 3,
                col: 2,
                ..Default::default()
            },
        };
        assert!(
            det("a").same_page(&scrolled),
            "a spot is where you were, not which page it is"
        );
        assert_ne!(
            det("a"),
            scrolled,
            "…and `==` still separates them, which is why both exist"
        );

        assert!(Node::Home.same_page(&Node::Home));
        assert!(Node::Library.same_page(&Node::Library));
        for other in [Node::Home, Node::Library, person("a")] {
            assert!(
                !det("a").same_page(&other),
                "a detail page is not {other:?}"
            );
        }
    }

    /// [`Trail::under`] must answer exactly what the [`Trail::back`] that follows it will return —
    /// the press frame reads the peek to size the transition (does the destination wear the tab
    /// bar?) and the fade floor performs the pop, so a peek that disagreed with its pop would fade
    /// the wrong chrome. Graded across a whole walk to the root, including the decline there.
    #[test]
    fn under_is_what_the_next_back_will_return() {
        let mut t = Trail::new();
        t.push(Node::Library);
        t.push(det("a"));
        t.push(person("p1"));
        for _ in 0..4 {
            let peeked = t.under().cloned();
            assert_eq!(
                peeked,
                t.back(),
                "the peek and the pop must name the same page"
            );
        }
        assert_eq!(
            t.under(),
            None,
            "…at the root there is nothing under, and BACK declines"
        );
    }

    /// A peek is not a pop, which is what makes a BACK that never commits FREE. Two ways a press
    /// reaches the floor without popping, and both must leave the history untouched: a second BACK
    /// inside the 70 ms window (the transition is WITHDRAWN), and a request the commit frame drops
    /// because something else moved the app while it was fading (SUPERSEDED). Either way the page
    /// is still on screen, so the node describing it must still be on the trail.
    #[test]
    fn under_does_not_consume_the_trail() {
        let mut t = Trail::new();
        t.push(det("a"));
        t.push(det("b"));
        for _ in 0..5 {
            assert_eq!(t.under(), Some(&det("a")));
        }
        assert_eq!(
            t.stack,
            vec![Node::Home, det("a"), det("b")],
            "five presses, no history spent"
        );
        assert_eq!(
            t.back(),
            Some(det("a")),
            "…and the one that DOES commit still pops once"
        );
    }

    /// A spot recorded on the page being left must SURVIVE the page pushed over it, or a restore
    /// would put the user back at the top of a page they had scrolled — the whole point of carrying
    /// one.
    #[test]
    fn a_spot_survives_the_page_pushed_over_it() {
        let s = Spot {
            section: 2,
            col: 3,
            ep_text: true,
            saved_col: [0, 0, 3, 0, 0, 0],
            season: Some(2),
        };
        let mut t = Trail::new();
        t.push(det("show"));
        t.set_top_spot(s.clone());
        t.push(det("episode"));
        assert_eq!(
            t.back(),
            Some(Node::Detail {
                sid: A,
                rk: "show".into(),
                spot: s
            })
        );
    }
}
