//! **The accessibility read-back** (the a11y-ids plan, D8): [`Dispatcher::a11y_snapshot`] projects
//! the input owner's view (the page composed with the strip, [`Dispatcher::owner_view`]) into an
//! owned [`A11ySnapshot`] the application serializes for `state:<nonce>`.
//!
//! It is a PURE read: `&self`, no queue, no engine write, no frame requested. It reads the owner
//! through the same `Focusable` methods the engine asks (`groups`, `place`) plus the three a11y
//! hooks, with the context the engine's own queries are built with (`CxParts::cx`, then the owner
//! and the focus read of that entry), so what it lists is what resolution would see.
//!
//! The projection itself, [`project_into`], is a free function over any `Focusable`, so a host test
//! drives it with a hand-written view and needs no dispatcher.

use std::borrow::Cow;

use plx_machine::machine::{Cx, GroupId, Host, InputOwner};
use plx_machine::present::Scope;

use super::{Dispatcher, Rig, Split};
use crate::a11y::{A11y, A11ySnapshot, ElemSnapshot, GroupSnapshot, OwnerKind};
use crate::geom::IndexElem;
use crate::screen::{At, Focusable, Placed};
use crate::Rect;

/// The namespace of the shared strip (`nav.watchlist`). `PageWithStrip` qualifies its strip
/// elements with it; [`project_into`] repeats it as an idempotent fallback (`A11yPath::qualified`
/// keeps a namespace that is already set).
const STRIP_NS: &str = "nav";

impl<H: Host> Dispatcher<H>
where
    H::Elem: IndexElem,
{
    /// The owner view as the state file lists it: the route word (the top page's), who owns input,
    /// the focused full id, every group and every element (no cap), each with where it rests, whether
    /// any of it shows, whether it is focused and whether it is `loaded`: its item has arrived AND a
    /// press of it would act on it now (`loaded: false` is "not pressable yet": an unpaged slot, or the
    /// focused element of a screen whose OK would land elsewhere for a few frames, Home's hero/grid
    /// door lag; the by-id press refuses it as `unloaded`). An unloaded element lists no label and no
    /// subject.
    ///
    /// A keyboard or an owner-less tree lists no element: nothing is addressable then (the by-id
    /// resolution refuses `NoOwner`).
    pub fn a11y_snapshot(&self, rig: &mut dyn Rig<H>) -> A11ySnapshot {
        let entry = self.owner_entry();
        // The ROUTE is the top page's word (the heartbeat's `route=`), also while a surface owns
        // input; `ns` below names the owner.
        let route = self.top_screen().map(|s| s.name());
        let owner = if self.input.keyboard {
            OwnerKind::Keyboard
        } else if entry.is_none() {
            OwnerKind::None
        } else if matches!(self.input_scope(), Scope::Surface) {
            OwnerKind::Surface
        } else {
            OwnerKind::Page
        };
        let mut snap = A11ySnapshot {
            frame: self.frame,
            route,
            owner,
            ns: None,
            focused: None,
            groups: Vec::new(),
            elems: Vec::new(),
        };
        if !matches!(owner, OwnerKind::Page | OwnerKind::Surface) {
            return snap;
        }
        let Some(entry) = entry else { return snap };
        let Some(view) = Self::owner_view(&self.nav, entry) else { return snap };
        let ns = view.page.a11y_ns();
        snap.ns = Some(ns);
        let Split { views, measure, .. } = rig.split();
        let mut cx = self.parts(self.last_tick).cx::<H>(views, measure);
        cx.owner = InputOwner::Entry(entry);
        cx.focus = self.input.engine.read(cx.owner);
        let focus = self
            .input
            .engine
            .current(cx.owner)
            .filter(|key| key.entry == entry)
            .map(|key| key.elem);
        let strip = view.strip.as_ref().map(|(spec, _)| spec.id);
        project_into(&mut snap, &view, &cx, ns, strip, focus);
        snap
    }
}

/// Project `view` into `snap.groups`, `snap.elems` and `snap.focused`.
///
/// `ns` qualifies every element's id that has no namespace yet, except those of the `strip` group,
/// which take `nav`. An element a view lists but does not project (`a11y` answers `None`) is left
/// out; the group's `len` still counts it, so a reader sees the gap. **An element that is not `loaded`
/// carries no label and no subject in the state file, whatever its `a11y` says**: the file is what
/// a caller presses from, and "not loaded" there means "not pressable yet" (an item that has not
/// arrived, or a screen whose OK would not land on the element yet), so it names nothing to press.
pub(super) fn project_into<H: Host, F: Focusable<H> + ?Sized>(
    snap: &mut A11ySnapshot,
    view: &F,
    cx: &Cx<'_, H>,
    ns: &'static str,
    strip: Option<GroupId>,
    focus: Option<H::Elem>,
) {
    let mut specs = Vec::new();
    view.groups(cx, &mut specs);
    let mut keys = Vec::new();
    for spec in &specs {
        let home = if strip == Some(spec.id) { STRIP_NS } else { ns };
        keys.clear();
        view.a11y_elems(spec.id, cx, &mut keys);
        let first = snap.elems.len();
        for (index, key) in keys.iter().enumerate() {
            let Some(a) = view.a11y(key, cx) else { continue };
            let placed = view.place(key, cx, At::SpringTarget);
            let A11y { path, role, label, subject, loaded } = a;
            snap.elems.push(ElemSnapshot {
                id: path.qualified(home).to_string(),
                group: spec.id.0,
                index,
                role,
                rest_rect: placed.map(|p| p.rest_rect),
                visible: placed.as_ref().is_some_and(shows),
                focused: focus == Some(*key),
                loaded,
                label: label.filter(|_| loaded).map(Cow::into_owned),
                subject: subject.filter(|_| loaded).map(Cow::into_owned),
            });
        }
        let listed = &snap.elems[first..];
        let group = GroupSnapshot {
            group: spec.id.0,
            prefix: group_prefix(listed.iter().map(|e| e.id.as_str())),
            len: spec.len,
            role: listed.first().map(|e| e.role),
        };
        snap.groups.push(group);
    }
    snap.focused = snap.elems.iter().find(|e| e.focused).map(|e| e.id.clone()).or_else(|| {
        // The focus is on an element its group did not list: ask the view directly.
        let key = focus?;
        let home = match view.group_of(&key, cx) {
            Some(g) if strip == Some(g) => STRIP_NS,
            _ => ns,
        };
        view.a11y(&key, cx).map(|a| a.path.qualified(home).to_string())
    });
}

/// Does any of the element show? `rect ∩ clip` is §7.6's visible part, and the viewport is the
/// last clip of all. A rect with no area (an element parked out of the page) does not.
fn shows(p: &Placed) -> bool {
    let seen = p.rest_rect.intersect(p.clip).intersect(Rect::FULL);
    seen.w > 0.0 && seen.h > 0.0
}

/// The id prefix a group's elements share: the longest run of leading segments common to every id,
/// less the last when that would be the whole of the shortest id (so a one-element group names its
/// parent, and `home.shelf.2.card.0` .. `.7` share `home.shelf.2.card`). Empty for no ids.
fn group_prefix<'a>(ids: impl Iterator<Item = &'a str>) -> String {
    let mut common: Option<Vec<&'a str>> = None;
    let mut shortest = usize::MAX;
    for id in ids {
        let segs: Vec<&str> = id.split('.').collect();
        shortest = shortest.min(segs.len());
        common = Some(match common {
            None => segs,
            Some(have) => {
                let same = have.iter().zip(&segs).take_while(|(a, b)| a == b).count();
                have[..same].to_vec()
            }
        });
    }
    let Some(mut common) = common else { return String::new() };
    if common.len() >= shortest {
        common.pop();
    }
    common.join(".")
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::NoTap;
    use crate::a11y::{A11yPath, Role};
    use crate::containers::modal::Style;
    use crate::fixture::{booted, tick, FixtureArg, FixtureHost, FixtureMeasure, FixtureView, FixtureViews};
    use crate::screen::{AxisMask, Dir, EdgeRule, ElemKind, GroupKind, GroupSpec, Seat, Step};
    use plx_machine::machine::{
        EntryId, FocusKey, FocusRead, MachineId, Measure, NavOp, PressRead, Tick,
    };

    type H = FixtureHost;

    fn cx<'a>(m: &'a dyn Measure, v: &'a FixtureView) -> Cx<'a, H> {
        Cx {
            views: FixtureViews { store: v },
            tick: Tick::default(),
            measure: m,
            press: PressRead::default(),
            focus: FocusRead::default(),
            owner: InputOwner::Entry(EntryId(0)),
            modal: Default::default(),
            page_image: Default::default(),
            hold_hint: Default::default(),
        }
    }

    /// Two groups, the smallest hand-written `Focusable` with what a snapshot reads: group 1 is a
    /// row of `cards` cards `card.<i>` (element `i`) whose first two are placed on screen, the third
    /// far off it and the rest not placed at all, the last card not loaded; group 2 is one button
    /// `play` (element 100), placed with no area.
    struct View {
        cards: usize,
    }

    const CARDS: GroupId = GroupId(1);
    const BUTTON: GroupId = GroupId(2);

    impl Focusable<H> for View {
        fn groups(&self, _cx: &Cx<'_, H>, out: &mut Vec<GroupSpec>) {
            for (id, len, elem) in [(CARDS, self.cards, ElemKind::Card), (BUTTON, 1, ElemKind::Bare)] {
                out.push(GroupSpec {
                    id,
                    kind: GroupKind::Row { wrap: false },
                    seat: Seat::Nearest,
                    reachable: AxisMask::BOTH,
                    edge: [EdgeRule::Geometric; 4],
                    extent: Rect::new(0.0, 0.0, 1920.0, 200.0),
                    len,
                    elem,
                });
            }
        }
        fn group_of(&self, key: &u32, _cx: &Cx<'_, H>) -> Option<GroupId> {
            match *key {
                100 => Some(BUTTON),
                k if (k as usize) < self.cards => Some(CARDS),
                _ => None,
            }
        }
        fn neighbour(&self, _key: FocusKey<u32>, _dir: Dir, _cx: &Cx<'_, H>) -> Step<u32> {
            Step::Edge
        }
        fn place(&self, key: &u32, _cx: &Cx<'_, H>, _at: At) -> Option<Placed> {
            let (x, w) = match *key {
                0 | 1 => (*key as f32 * 300.0, 280.0),
                2 => (5000.0, 280.0),
                100 => (100.0, 0.0),
                _ => return None,
            };
            let rect = Rect::new(x, 10.0, w, 200.0);
            Some(Placed { rect, rest_rect: rect, clip: Rect::FULL, index: None })
        }
        fn reconcile(&self, want: FocusKey<u32>, _cx: &Cx<'_, H>) -> FocusKey<u32> {
            want
        }
        fn seat(&self, _g: GroupId, _from: Placed, _cx: &Cx<'_, H>) -> FocusKey<u32> {
            FocusKey { entry: EntryId(0), elem: 0 }
        }
        fn a11y(&self, key: &u32, _cx: &Cx<'_, H>) -> Option<A11y<'_>> {
            match *key {
                100 => Some(A11y::new(A11yPath::new().name("play"), Role::Button)),
                k if (k as usize) < self.cards => {
                    let a = A11y::new(A11yPath::new().name("card").index(k as usize), Role::Card)
                        .with_label("A Title")
                        .with_subject("12345");
                    Some(a.with_loaded(k as usize + 1 != self.cards))
                }
                _ => None,
            }
        }
        fn a11y_elems(&self, g: GroupId, _cx: &Cx<'_, H>, out: &mut Vec<u32>) {
            match g {
                CARDS => out.extend(0..self.cards as u32),
                BUTTON => out.push(100),
                _ => {}
            }
        }
    }

    fn project(view: &View, strip: Option<GroupId>, focus: Option<u32>) -> A11ySnapshot {
        let (m, v) = (FixtureMeasure, FixtureView::default());
        let mut snap = A11ySnapshot {
            frame: 3,
            route: Some("fixture"),
            owner: OwnerKind::Page,
            ns: Some("home"),
            focused: None,
            groups: Vec::new(),
            elems: Vec::new(),
        };
        project_into(&mut snap, view, &cx(&m, &v), "home", strip, focus);
        snap
    }

    fn elem<'a>(snap: &'a A11ySnapshot, id: &str) -> &'a ElemSnapshot {
        snap.elems.iter().find(|e| e.id == id).unwrap_or_else(|| panic!("{id} is not listed"))
    }

    /// **The snapshot names the focused id**, flags exactly that element, and qualifies every id
    /// with the owner's namespace.
    #[test]
    fn the_focused_element_is_named_and_flagged() {
        let snap = project(&View { cards: 4 }, None, Some(1));
        assert_eq!(snap.focused.as_deref(), Some("home.card.1"));
        assert_eq!(snap.elems.iter().filter(|e| e.focused).count(), 1);
        assert!(elem(&snap, "home.card.1").focused);
        assert_eq!(
            snap.elems.iter().map(|e| e.id.as_str()).collect::<Vec<_>>(),
            ["home.card.0", "home.card.1", "home.card.2", "home.card.3", "home.play"]
        );
        assert_eq!(project(&View { cards: 4 }, None, None).focused, None);
    }

    /// Groups are listed with their declared `len`, the id prefix their elements share and the
    /// role of the first.
    #[test]
    fn a_group_reports_its_prefix_len_and_role() {
        let snap = project(&View { cards: 4 }, None, None);
        assert_eq!(
            snap.groups,
            [
                GroupSnapshot { group: 1, prefix: "home.card".into(), len: 4, role: Some(Role::Card) },
                GroupSnapshot { group: 2, prefix: "home".into(), len: 1, role: Some(Role::Button) },
            ]
        );
        let empty = project(&View { cards: 0 }, None, None);
        assert_eq!(empty.groups[0], GroupSnapshot { group: 1, prefix: String::new(), len: 0, role: None });
    }

    /// The strip's group takes `nav`, the page's takes the owner's namespace, and the focus is
    /// named through the same rule when its group did not list it.
    #[test]
    fn the_strip_group_is_qualified_nav() {
        let snap = project(&View { cards: 2 }, Some(BUTTON), Some(100));
        assert_eq!(elem(&snap, "nav.play").group, 2);
        assert_eq!(elem(&snap, "home.card.0").group, 1);
        assert_eq!(snap.focused.as_deref(), Some("nav.play"));
        assert_eq!(snap.groups[1].prefix, "nav");
    }

    /// Where an element rests, and whether any of it shows: on screen, far off screen, not placed
    /// by the view at all, and placed with no area.
    #[test]
    fn rest_rect_and_visibility_come_from_the_spring_target() {
        let snap = project(&View { cards: 5 }, None, None);
        let on = elem(&snap, "home.card.1");
        assert_eq!(on.rest_rect, Some(Rect::new(300.0, 10.0, 280.0, 200.0)));
        assert!(on.visible);
        let off = elem(&snap, "home.card.2");
        assert!(off.rest_rect.is_some() && !off.visible, "off screen is listed, and does not show");
        let unplaced = elem(&snap, "home.card.3");
        assert_eq!((unplaced.rest_rect, unplaced.visible), (None, false));
        let flat = elem(&snap, "home.play");
        assert!(flat.rest_rect.is_some() && !flat.visible, "a rect with no area shows nothing");
    }

    /// **An unloaded element carries no label and no subject**; a loaded one carries both.
    #[test]
    fn an_unloaded_element_carries_no_text() {
        let snap = project(&View { cards: 3 }, None, None);
        let loaded = elem(&snap, "home.card.0");
        assert_eq!((loaded.label.as_deref(), loaded.subject.as_deref()), (Some("A Title"), Some("12345")));
        let unloaded = elem(&snap, "home.card.2");
        assert!(!unloaded.loaded);
        assert_eq!((unloaded.label.as_ref(), unloaded.subject.as_ref()), (None, None));
    }

    /// **Labels never reach a log line**: the projection holds the runtime text of every element and
    /// has no way to log at all (no `LogLine`, no event log, no rig `log`), so the only place that
    /// text can leave is the value it returns.
    #[test]
    fn the_projection_has_no_way_to_log() {
        let source = include_str!("snapshot.rs");
        let production = source.split("#[cfg(test)]\nmod tests").next().unwrap();
        for forbidden in ["LogLine", "Fx::Log", "eventlog", "println!", "rig.log"] {
            assert!(!production.contains(forbidden), "the projection names {forbidden}");
        }
    }

    #[test]
    fn the_group_prefix_is_the_shared_leading_segments() {
        let prefix = |ids: &[&str]| group_prefix(ids.iter().copied());
        assert_eq!(prefix(&["home.shelf.2.card.0", "home.shelf.2.card.7"]), "home.shelf.2.card");
        assert_eq!(prefix(&["home.hero.play", "home.hero.more"]), "home.hero");
        assert_eq!(prefix(&["home.hero.play"]), "home.hero");
        assert_eq!(prefix(&["nav.watchlist"]), "nav");
        assert_eq!(prefix(&["home.shelf", "home.shelf.0"]), "home");
        assert_eq!(prefix(&[]), "");
    }

    /// **Under a surface the route stays the page's** and the namespace is the surface's: the
    /// snapshot says who owns input (`owner`, `ns`) without losing which page is open (`route`).
    #[test]
    fn a_surface_owns_input_while_the_route_stays_the_pages() {
        let _serial = plx_base::testlock::serial();
        let (mut d, mut rig, _) = booted();
        let page = d.top_screen().expect("a page is mounted").name();
        d.nav.next_style = Style::Compact;
        d.request(MachineId::Nav, NavOp::Present(FixtureArg::Modal));
        d.frame(&mut rig, tick(16), vec![], vec![], &mut NoTap);
        let snap = d.a11y_snapshot(&mut rig);
        assert_eq!(snap.owner, OwnerKind::Surface);
        assert_eq!(snap.route, Some(page));
        assert_eq!(snap.ns, Some("settings"), "the fixture modal's word");
        assert_ne!(snap.ns, snap.route);
    }

    /// **The dispatcher's snapshot** of a booted fixture: it names the owner screen and the frame,
    /// is deterministic (a pure read), lists only ids of the owner's namespace or `nav`, and a
    /// keyboard in the way lists nothing.
    #[test]
    fn the_dispatcher_projects_its_owner_and_a_keyboard_lists_nothing() {
        use crate::containers::tabs::StripMember;
        use crate::widgets::{TAB_KEY_HOME, TAB_KEY_WATCHLIST};
        let (mut d, mut rig, _home) = booted();
        // the shared strip is composed: two pills, named by their destinations, ahead of the page's row
        d.nav.tabs.strip = vec![
            StripMember::new(TAB_KEY_HOME, Rect::new(100.0, 10.0, 160.0, 48.0)),
            StripMember::new(TAB_KEY_WATCHLIST, Rect::new(280.0, 10.0, 160.0, 48.0)),
        ];
        let name = d.top_screen().expect("a page is mounted").name();
        let snap = d.a11y_snapshot(&mut rig);
        assert_eq!((snap.route, snap.owner, snap.frame), (Some(name), OwnerKind::Page, d.frame_index()));
        let ns = snap.ns.expect("a page owns input");
        assert_eq!(ns, "home", "the fixture's Home answers its route word");
        // Not vacuous: a non-empty list, with the concrete ids of BOTH halves of the composed view
        // (`PageWithStrip` forwards the strip's pills under `nav` and the page's own under its word).
        let ids: Vec<&str> = snap.elems.iter().map(|e| e.id.as_str()).collect();
        assert!(!ids.is_empty(), "the booted page lists elements");
        for want in ["nav.home", "nav.watchlist", "home.row.0"] {
            assert!(ids.contains(&want), "{want} is listed: {ids:?}");
        }
        assert!(snap.elems.iter().all(|e| e.id.starts_with(&format!("{ns}.")) || e.id.starts_with("nav.")), "{ids:?}");
        assert!(snap.groups.iter().any(|g| g.prefix == "nav" && g.len == 2), "the strip is one group: {:?}", snap.groups);
        assert!(snap.groups.iter().any(|g| g.prefix == "home.row"), "{:?}", snap.groups);
        assert_eq!(snap.elems.iter().filter(|e| e.focused).count(), usize::from(snap.focused.is_some()));
        let focused = snap.focused.as_deref().expect("the booted page seats focus");
        assert!(ids.contains(&focused), "the focused id is one of the listed elements: {focused}");
        assert_eq!(snap, d.a11y_snapshot(&mut rig), "the projection reads, it does not move anything");

        d.input.keyboard = true;
        let keyed = d.a11y_snapshot(&mut rig);
        assert_eq!(keyed.owner, OwnerKind::Keyboard);
        assert!(keyed.groups.is_empty() && keyed.elems.is_empty() && keyed.focused.is_none());
        assert_eq!(keyed.route, Some(name), "the page beneath still has its word");
    }
}
