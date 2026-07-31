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

use crate::ui::detail::Spot;

/// One page in the trail — everything needed to put it back WITHOUT the screen that pushed it.
///
/// The payloads are the arguments each screen's own entry point already takes, so re-entry is a
/// call rather than a reconstruction: [`Node::Person`]'s four fields are `ui::person::reopen`'s
/// parameters (the header the cast row handed over — name and headshot — which is why a re-opened
/// person page renders before its fetch lands), and [`Node::Detail`]'s `rk` is `detail::open_rk`'s.
///
/// **Identity, never an index.** A hub refetch rebuilds the catalog wholesale (Continue Watching
/// re-sorts by `lastViewedAt`), so an index stored here would name a different movie by the time
/// BACK is pressed — the bug `detail::reselect` exists for. `detail::mount_rk` re-derives the row
/// from the rk through `pms::index_of_rk` at re-entry instead.
#[derive(Clone, PartialEq, Debug)]
pub(crate) enum Node {
    /// The root, and the only node BACK never leaves — see [`Trail::back`].
    Home,
    /// The browse grid. No payload: `browse.rs` holds the section, focus and scroll for as long as
    /// the profile does, so re-entry is a route flip (`library::enter` would re-query and lose it).
    Library,
    Person {
        /// the LOCAL `personId` — `crate::person::Person::key`
        key: String,
        /// the `tagKey` guid plex.tv answers to
        guid: String,
        name: String,
        thumb: String,
    },
    /// `spot` is where the page stood when the user navigated deeper — `Spot::default()` for a page
    /// that has been entered and not yet left (see [`Trail::set_top_spot`]).
    Detail { rk: String, spot: Spot },
}

/// The trail, top = the page on screen.
pub(crate) struct Trail {
    stack: Vec<Node>,
}

impl Trail {
    /// Trail ceiling. A Related chain is unbounded (PMS's related hubs are near-symmetric, so
    /// A→B→A is two presses) and so is person→detail→person, which makes this a MEMORY bound rather
    /// than a correctness one: 16 nodes is a couple of kilobytes and deeper than any real session,
    /// and BACK terminates either way because the ROOT is never dropped.
    pub(crate) const CAP: usize = 16;

    pub(crate) fn new() -> Self {
        Self { stack: vec![Node::Home] }
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
    /// keeps BACK at Home exiting the app, because `app.rs`'s BACK arm reaches its `running = false`
    /// branch by never consulting the trail on Home at all.
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
        self.stack.len().checked_sub(2).and_then(|i| self.stack.get(i))
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

    /// Make the trail agree with a detail page we landed on WITHOUT navigating — the player exit and
    /// the Info card's jump-to-detail, neither of which is a trail node. Idempotent: no push when
    /// the top already names `rk`, so an exit back onto the page playback started from does not
    /// double it.
    pub(crate) fn ensure_detail(&mut self, rk: &str) {
        if matches!(self.stack.last(), Some(Node::Detail { rk: t, .. }) if t == rk) {
            return;
        }
        self.push(Node::Detail { rk: rk.to_string(), spot: Spot::default() });
    }
}

// ---------------------------------------------------------------------------------------
#[cfg(test)]
mod tests {
    //! Pure and parallel — this module touches no crate global, which is the point of splitting it
    //! out of the BACK arm. What these CANNOT say is whether a restored page LOOKS like the one that
    //! was left; that is `detail.rs`'s `Spot` tests plus a device capture.
    use super::*;

    fn det(rk: &str) -> Node {
        Node::Detail { rk: rk.to_string(), spot: Spot::default() }
    }
    fn person(key: &str) -> Node {
        Node::Person { key: key.into(), guid: String::new(), name: String::new(), thumb: String::new() }
    }

    /// The executable form of "BACK at the Home root EXITS THE APP": the trail declines, and the arm
    /// falls through to `running = false`. Nothing else in this module may make that untrue.
    #[test]
    fn a_fresh_trail_is_the_root_and_back_there_declines() {
        let mut t = Trail::new();
        assert_eq!(t.stack, vec![Node::Home]);
        assert_eq!(t.back(), None);
        assert_eq!(t.stack, vec![Node::Home], "a declined pop must not consume the root either");
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
        assert_eq!(t.stack[0], Node::Home, "the root is what makes BACK terminate");
        assert_eq!(t.stack[1], det(&format!("d{}", Trail::CAP + 4 - (Trail::CAP - 1))));

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
        let s = Spot { section: 2, col: 5, ..Default::default() };
        for top in [Node::Home, Node::Library, person("p1")] {
            let mut t = Trail::new();
            t.push(top.clone());
            t.set_top_spot(s.clone());
            assert_eq!(t.stack.last(), Some(&top), "{top:?} has no place to restore");
        }
        let mut t = Trail::new();
        t.push(det("a"));
        t.set_top_spot(s.clone());
        assert_eq!(t.stack.last(), Some(&Node::Detail { rk: "a".into(), spot: s }));
    }

    /// The player exit and the Info card's jump both LAND on a detail page without navigating to it.
    /// Landing back on the page playback started from must not double it; landing on a different one
    /// must push.
    #[test]
    fn ensure_detail_is_idempotent_for_the_page_already_on_top() {
        let mut t = Trail::new();
        t.push(det("a"));
        t.ensure_detail("a");
        t.ensure_detail("a");
        assert_eq!(t.stack.len(), 2, "the page playback started from is already the top");

        t.ensure_detail("b");
        assert_eq!(t.stack.len(), 3);
        assert_eq!(t.back(), Some(det("a")));

        // …and over a non-Detail top it always pushes: there is nothing there to be the same page.
        let mut t = Trail::new();
        t.push(person("p1"));
        t.ensure_detail("a");
        assert_eq!(t.back(), Some(person("p1")));
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
            assert_eq!(peeked, t.back(), "the peek and the pop must name the same page");
        }
        assert_eq!(t.under(), None, "…at the root there is nothing under, and BACK declines");
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
        assert_eq!(t.stack, vec![Node::Home, det("a"), det("b")], "five presses, no history spent");
        assert_eq!(t.back(), Some(det("a")), "…and the one that DOES commit still pops once");
    }

    /// A spot recorded on the page being left must SURVIVE the page pushed over it, or a restore
    /// would put the user back at the top of a page they had scrolled — the whole point of carrying
    /// one.
    #[test]
    fn a_spot_survives_the_page_pushed_over_it() {
        let s = Spot { section: 2, col: 3, ep_text: true, saved_col: [0, 0, 3, 0, 0, 0], season: Some(2) };
        let mut t = Trail::new();
        t.push(det("show"));
        t.set_top_spot(s.clone());
        t.push(det("episode"));
        assert_eq!(t.back(), Some(Node::Detail { rk: "show".into(), spot: s }));
    }
}
