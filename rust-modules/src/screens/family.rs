//! **The Settings family's shared plumbing** (restructure spec §6.2 `SettingsSurface`, phase 5b):
//! the surface's INNER host and its argument vocabulary ([`SettingsPage`]), the conversions a
//! surface makes when it steps a page of its own stack under the outer dispatcher's context, and
//! the two things every page in the family does the same way — seating a `TableView` on the
//! engine's focus and naming its element keys.
//!
//! Not a screen: `screens/mod.rs`'s rule that a screen never names a sibling is what this module
//! exists to satisfy — Legal pushes a document, Privacy pushes a preview, the root pushes all of
//! them, and each names the destination through this vocabulary rather than through the module
//! that implements it.

use crate::ui::machine::{Canon, Chrome, Cx, FocusRead, Host, LogicalState, ScreenId};
use crate::ui::screen::ScreenArg;
use crate::ui::table::TableView;
use crate::ui::widgets::ControlPalette;

use super::registry::{AppFx, AppMsg};

/// A page of the surface's own stack (§6.2: root → Privacy | Legal | Favourites → Document).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum SettingsPage {
    /// The Settings root (`ui::settings`'s table).
    Root,
    /// Favorite libraries — the onboard screen in Settings mode.
    Favourites,
    /// Privacy & data — the consent screen in Settings mode.
    Privacy,
    /// The Legal index.
    Legal,
    /// About PlxNative, a document pushed straight from the root.
    About,
    /// One Legal document, by index into `screens::legal`'s page list.
    Document(u8),
    /// One Privacy preview document (`screens::consent::Preview`).
    Preview(u8),
    /// The FIRST-RUN consent question: one stage per page, the second pushed over the first.
    ConsentStage(u8),
}

impl ScreenArg for SettingsPage {
    fn chrome(&self) -> Chrome {
        Chrome::None
    }
    fn id(&self) -> ScreenId {
        ScreenId(match self {
            SettingsPage::Root => 100,
            SettingsPage::Favourites => 101,
            SettingsPage::Privacy => 102,
            SettingsPage::Legal => 103,
            SettingsPage::About => 104,
            SettingsPage::Document(_) => 105,
            SettingsPage::Preview(_) => 106,
            SettingsPage::ConsentStage(_) => 107,
        })
    }
    fn title(&self) -> Option<&str> {
        None
    }
    fn same_instance(&self, other: &Self) -> bool {
        self == other
    }
}

/// **A page's canonical encoding, for the surface's own logical state** (§5.4).
///
/// `screens::settings`'s `RouteSurface` hashes its inner stack entry by entry, and one entry is
/// distinguished from its neighbour by WHICH page it is — so both halves have to be written, and
/// they are written HERE rather than at the hash site so that a variant added to this enum is
/// answered by a census `match` beside its own declaration. A `_ =>` arm at a distant call site
/// would silently hash two different documents as one, which is exactly the class of miss the
/// recorder exists to catch.
///
/// The kind is [`ScreenArg::id`]'s number rather than a second numbering of the same question —
/// that is already the id the container keys page KIND on. The index is written for EVERY variant,
/// `0` where there is none, so a payload-less variant that later grows one cannot keep hashing
/// identically to its old self.
impl LogicalState for SettingsPage {
    fn write(&self, w: &mut Canon) {
        w.discriminant(ScreenArg::id(self).0);
        w.u8(match self {
            SettingsPage::Root
            | SettingsPage::Favourites
            | SettingsPage::Privacy
            | SettingsPage::Legal
            | SettingsPage::About => 0,
            SettingsPage::Document(i) | SettingsPage::Preview(i) | SettingsPage::ConsentStage(i) => *i,
        });
    }
    fn probe(&self, out: &mut String) {
        out.push_str(match self {
            SettingsPage::Root => "root",
            SettingsPage::Favourites => "favourites",
            SettingsPage::Privacy => "privacy",
            SettingsPage::Legal => "legal",
            SettingsPage::About => "about",
            SettingsPage::Document(_) => "document",
            SettingsPage::Preview(_) => "preview",
            SettingsPage::ConsentStage(_) => "stage",
        });
        if let SettingsPage::Document(i) | SettingsPage::Preview(i) | SettingsPage::ConsentStage(i) = self {
            out.push_str(&format!("[{i}]"));
        }
    }
}

/// The inner host: the same bundle, the family's own argument, no views and no initial
/// conditions of its own (the surface's `LogicalState` covers its pages).
pub(crate) struct InnerHost;

#[derive(Default)]
pub(crate) struct NoInit;

impl LogicalState for NoInit {
    fn write(&self, _w: &mut Canon) {}
    fn probe(&self, _out: &mut String) {}
}

impl Host for InnerHost {
    type Arg = SettingsPage;
    type Fx = AppFx;
    type Msg = AppMsg;
    type Elem = u32;
    type Views<'a> = ();
    type Init = NoInit;
    // No family page remembers anything on its own `ReturnState` today (`ui/machine.rs`'s
    // `Host::Memory` doc) — the surface's own `NavStack<InnerHost>` restores its child pages by
    // focus alone.
    type Memory = ();
}

/// The outer context as the inner pages see it: everything but the views, which the family's
/// pages never read through `Cx` (they read the stores' published functions directly).
/// The output lifetime is FREE (`'o`, bounded by the measure's): `Cx` is invariant in its
/// lifetime through the `Views` projection, so a caller must be able to shape one to a local
/// borrow to hand it to a `DrawFrame`.
pub(crate) fn inner_cx<'o, 'a: 'o, H: Host<Elem = u32>>(cx: &Cx<'a, H>) -> Cx<'o, InnerHost> {
    Cx {
        views: (),
        tick: cx.tick,
        measure: cx.measure,
        press: cx.press,
        focus: FocusRead { current: cx.focus.current },
        owner: cx.owner,
    }
}

/// Seat a table on the engine's focus: a row key parks the selection on that row and lights the
/// list; a key elsewhere (the band, the alert) dims it. Every page in the family answers
/// `FocusMoved` with this, so the drawn selection and the engine never disagree.
pub(crate) fn table_focus(table: &mut TableView, elem: u32) {
    if elem < super::registry::BAND && (elem as i32) < table.n_rows() {
        table.sel = elem as i32;
        table.list_focused = true;
    } else {
        table.list_focused = false;
    }
}

/// The band's group in every page of the family; the table is `GroupId(0)`, an alert `GroupId(2)`.
pub(crate) const TABLE_GROUP: GroupId = GroupId(0);
pub(crate) const BAND_GROUP: GroupId = GroupId(1);
pub(crate) const ALERT_GROUP: GroupId = GroupId(2);

use crate::ui::machine::GroupId;

thread_local! {
    /// The surface's ground palette, published for the frame so the pages' controls are keyed to
    /// the ground they sit on (what `settings::control_palette` answered). RENDER state: set by
    /// the surface at draw, read by the pages' draws, never hashed.
    static PALETTE: std::cell::Cell<Option<ControlPalette>> = const { std::cell::Cell::new(None) };
}

pub(crate) fn set_palette(p: ControlPalette) {
    PALETTE.with(|c| c.set(Some(p)));
}

pub(crate) fn palette() -> ControlPalette {
    PALETTE.with(|c| c.get()).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::table::{Row, Section};
    // Imported by its ABSOLUTE path, and that is the whole point. The module body above reaches
    // the registry as `super::registry`, because there `super` is `crate::screens` — but inside
    // this nested `mod tests` the same spelling means `family::registry`, which does not exist.
    // Copying a working path down one module level is how that breaks, and no gate but the test
    // build can see it, so name the module once here and let every test below say `registry::`.
    use crate::screens::registry;

    fn table_with_rows(n: i32) -> TableView {
        let mut t = TableView::new();
        let mut s = Section::new("Section");
        for i in 0..n {
            s = s.row(Row::new(format!("Row {i}")));
        }
        t.set_sections(vec![s], 0, false);
        t
    }

    /// A row key inside the table's own range parks the selection there and lights the list —
    /// every page in the family answers `FocusMoved` this way, so this is the one function that
    /// decides whether a page's drawn selection ever disagrees with the engine's own idea of
    /// where focus is.
    #[test]
    fn a_row_key_inside_the_table_seats_and_lights_it() {
        let mut t = table_with_rows(3);
        table_focus(&mut t, 2);
        assert_eq!(t.sel, 2);
        assert!(t.list_focused);
    }

    /// A key at or above [`registry::BAND`] is a band (or alert) control, never a row —
    /// the table must dim rather than light some row it does not actually have.
    #[test]
    fn a_band_key_dims_the_table_without_touching_its_selection() {
        let mut t = table_with_rows(3);
        t.sel = 1;
        t.list_focused = true;
        table_focus(&mut t, registry::BAND);
        assert!(!t.list_focused, "a band element must not read as a lit row");
        assert_eq!(t.sel, 1, "the row selection itself is untouched — only the light changes");
    }

    /// An element past the table's own row COUNT is dimmed too, even though its numeric value is
    /// below [`registry::BAND`] — a page whose row set just shrank (Favourite libraries
    /// after the last favourite is removed, say) must not light a row it no longer has rather
    /// than crashing on an out-of-range `sel`.
    #[test]
    fn a_key_below_the_band_but_past_the_row_count_still_dims() {
        let mut t = table_with_rows(2);
        table_focus(&mut t, 5);
        assert!(!t.list_focused);
    }

    /// [`super::SettingsPage`]'s `ScreenId`s are what a `NavStack` uses to decide whether two
    /// requests name "the same instance" (`same_instance` is bare equality here, but `NavOp::Root`
    /// elsewhere in the library also keys eviction bookkeeping off `id()`) — two variants sharing
    /// one id by a copy-paste slip would let the container conflate two different pages.
    #[test]
    fn every_settings_page_variant_has_its_own_screen_id() {
        let pages = [
            SettingsPage::Root,
            SettingsPage::Favourites,
            SettingsPage::Privacy,
            SettingsPage::Legal,
            SettingsPage::About,
            SettingsPage::Document(0),
            SettingsPage::Preview(0),
            SettingsPage::ConsentStage(0),
        ];
        let mut ids: Vec<u32> = pages.iter().map(|p| crate::ui::screen::ScreenArg::id(p).0).collect();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), pages.len(), "two SettingsPage variants must not share a ScreenId");
    }

    /// `Document`/`Preview`/`ConsentStage` carry an index that does not change WHICH kind of page
    /// they are — the id is the variant's, not the index's — so two different documents still
    /// name the same `ScreenId` (which is exactly right: `NavStack::apply`'s `NavOp::Root` arm
    /// compares `same_instance`, not `id()`, for identity, and `id()` alone is only ever a KIND).
    #[test]
    fn an_indexed_variant_s_id_does_not_vary_with_its_index() {
        use crate::ui::screen::ScreenArg;
        assert_eq!(SettingsPage::Document(0).id(), SettingsPage::Document(5).id());
        assert_eq!(SettingsPage::Preview(0).id(), SettingsPage::Preview(3).id());
        assert_eq!(SettingsPage::ConsentStage(0).id(), SettingsPage::ConsentStage(1).id());
        // …but `same_instance` still tells them apart, since it is bare equality here:
        assert!(!SettingsPage::Document(0).same_instance(&SettingsPage::Document(1)));
    }
}
