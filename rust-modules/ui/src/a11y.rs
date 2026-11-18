//! The accessibility identifiers: a stable name for every focusable element, so a UI test can focus
//! or press an element BY ID instead of walking the keys to it (the a11y-ids plan, D1 and D2).
//!
//! A **full id** is `<ns>.<path>` (`home.shelf.2.card.5`, `detail.hero.play`, `nav.watchlist`).
//! `ns` is the owner screen's namespace (`[a-z][a-z0-9_-]*`: a route word is used as it is, `-`
//! included, so `under-posters` is a namespace); `path` is one or more segments, each a NAME
//! (`[a-z][a-z0-9_]*`) or a decimal INDEX that is a position in the collection the segment before it
//! names. Brackets are zsh glob characters, so there are none: a dot path is safe in a shell.
//!
//! **No segment is ever built from runtime text.** A path is built from `&'static str` names and
//! integer indices ([`A11yPath::name`], [`A11yPath::index`]), so an id is bounded by construction
//! and falls under the focus probe's logging rule. The one producer of an owned name is
//! [`A11yPath::parse_id`], whose result exists to be COMPARED with a projected path
//! ([`Focusable::a11y_find`]); it is never logged and never kept.
//!
//! An element's [`A11y::label`] and [`A11y::subject`] (the ratingKey of the title the element acts on) are runtime
//! text. They never reach a `LogLine` or a heartbeat; the state file a test reads back is where they
//! are written. A `press:<id>@<subject>` token records its `@<subject>` text in the recording's `in`
//! row, which on a press that resolves equals the element's subject, so a recording of a
//! subject-guarded press carries a ratingKey and is private like any other recording (and the closed
//! replay `ALPHABET.json` has no pattern for it, so such a recording cannot be committed as a fixture).
//!
//! The three hooks live on [`Focusable`] (defaulted, so nothing changes until a widget answers):
//! `a11y` projects a key to its [`A11y`], `a11y_elems` lists a group's keys in index order (what
//! `GroupSpec::len` only counts), and `a11y_find` is the inverse of `a11y`. [`default_walk`] answers
//! `a11y_find` for a widget with no cheaper way; an indexed widget parses the index out of the path
//! ([`A11yPath::segs`]) and answers `elem(i)` directly, bounds-checked, so it never scans.
//! [`conformance`] is the host-test suite every implementation runs.

use std::borrow::Cow;
use std::fmt;

use plx_machine::machine::{Cx, Host};

#[cfg(any(test, feature = "test-support"))]
use super::screen::At;
use super::screen::Focusable;

/// Is `s` a namespace: `[a-z][a-z0-9_-]*`? A route word qualifies as it is (`under-posters`).
pub fn is_ns(s: &str) -> bool {
    let mut b = s.bytes();
    matches!(b.next(), Some(c) if c.is_ascii_lowercase())
        && b.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'_' || c == b'-')
}

/// Is `s` a path NAME segment: `[a-z][a-z0-9_]*`? No hyphen: a name is also a Rust identifier's
/// shape, and the namespace is the only place a route word's `-` may appear.
pub fn is_name(s: &str) -> bool {
    let mut b = s.bytes();
    matches!(b.next(), Some(c) if c.is_ascii_lowercase())
        && b.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'_')
}

/// A decimal index segment: digits only, canonical (no leading zero but `0` itself), so one
/// element has exactly one spelling.
fn parse_index(s: &str) -> Option<usize> {
    let canonical = !s.is_empty()
        && s.bytes().all(|c| c.is_ascii_digit())
        && (s.len() == 1 || !s.starts_with('0'));
    if canonical { s.parse().ok() } else { None }
}

/// Why a string is not an id ([`A11yPath::parse_id`], [`A11yPath::parse_relative`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IdError {
    /// The empty string.
    Empty,
    /// The text before the first `.` is not a namespace.
    BadNs,
    /// A full id with a namespace and no path (`home`).
    NoPath,
    /// A segment is neither a name nor a decimal index (empty, upper case, a space, a bracket, a
    /// hyphen, a leading zero, ...).
    BadSegment,
}

impl fmt::Display for IdError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            IdError::Empty => "empty id",
            IdError::BadNs => "bad namespace (want [a-z][a-z0-9_-]*)",
            IdError::NoPath => "no path after the namespace",
            IdError::BadSegment => "bad path segment (want [a-z][a-z0-9_]* or a decimal index)",
        })
    }
}

/// One segment of a path, as [`A11yPath::segs`] hands it out: match on it to resolve a path.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Seg<'a> {
    Name(&'a str),
    Index(usize),
}

#[derive(Clone, PartialEq, Eq, Hash)]
enum Piece {
    // `Cow::Borrowed` for every segment a widget builds; `Owned` only out of `parse_*`.
    Name(Cow<'static, str>),
    Index(usize),
}

/// The most segments an id may carry after its namespace. The deepest id the application builds is
/// four (`detail.shelf.1.card.2`: a shelf, its index, a card, its index), so this leaves two spare for a
/// composite that nests one more container. [`conformance_report`] complains of a longer id, and the
/// replay alphabet's id pattern (`tests/fixtures/replay/ALPHABET.json`) repeats this number as its
/// repeat bound: a test in `plx_screens`' registry keeps the two equal, so an id that grew past it
/// would fail a build, not a recording at the moment somebody tried to commit it.
pub const MAX_ID_SEGS: usize = 6;

/// The path of one element: an optional namespace and the segments after it.
///
/// A widget's [`Focusable::a11y`] answers a RELATIVE path (no namespace); the owner view that
/// knows the screen qualifies it ([`A11yPath::qualified`]), so a shared widget never names the
/// screen it sits on. Equality is by content, so a path a widget built from `&'static str` names
/// equals the one [`A11yPath::parse_id`] read from a test's text. [`fmt::Display`] is the id.
#[derive(Clone, Default, PartialEq, Eq, Hash)]
pub struct A11yPath {
    ns: Option<Cow<'static, str>>,
    segs: Vec<Piece>,
}

impl A11yPath {
    /// The empty relative path: the start of a builder chain.
    pub fn new() -> Self {
        Self::default()
    }

    /// This path with a NAME segment pushed. `name` must match `[a-z][a-z0-9_]*` (checked in debug
    /// builds, and by [`conformance`] in every build).
    pub fn name(mut self, name: &'static str) -> Self {
        self.push_name(name);
        self
    }

    /// This path with an INDEX segment pushed: a position in the collection the segment before it
    /// names.
    pub fn index(mut self, i: usize) -> Self {
        self.push_index(i);
        self
    }

    pub fn push_name(&mut self, name: &'static str) {
        debug_assert!(is_name(name), "a11y path segment {name:?} is not [a-z][a-z0-9_]*");
        self.segs.push(Piece::Name(Cow::Borrowed(name)));
    }

    pub fn push_index(&mut self, i: usize) {
        self.segs.push(Piece::Index(i));
    }

    /// This path followed by `tail`'s segments (`tail`'s namespace, if any, is dropped): how a
    /// container prefixes the path its child answered (`A11yPath::new().name("shelf").index(2)`
    /// joined with the shelf's `card.5`).
    pub fn join(mut self, tail: &A11yPath) -> Self {
        self.segs.extend(tail.segs.iter().cloned());
        self
    }

    /// This path in namespace `ns`, when it has none yet; one that already has a namespace is
    /// returned as it is, so qualifying twice (a `PageWithStrip` and then the host) is harmless.
    /// `ns` is the owner's `Screen::a11y_ns`, or `nav` for the strip.
    pub fn qualified(mut self, ns: &'static str) -> Self {
        debug_assert!(is_ns(ns), "a11y namespace {ns:?} is not [a-z][a-z0-9_-]*");
        if self.ns.is_none() {
            self.ns = Some(Cow::Borrowed(ns));
        }
        self
    }

    /// The namespace, for a qualified path.
    pub fn ns(&self) -> Option<&str> {
        self.ns.as_deref()
    }

    /// This path without its namespace: what the owner view hands the widget under it.
    pub fn relative(&self) -> A11yPath {
        A11yPath { ns: None, segs: self.segs.clone() }
    }

    /// The segments, in order, after the namespace.
    pub fn segs(&self) -> impl ExactSizeIterator<Item = Seg<'_>> + '_ {
        self.segs.iter().map(|p| match p {
            Piece::Name(n) => Seg::Name(n),
            Piece::Index(i) => Seg::Index(*i),
        })
    }

    /// How many segments there are (the namespace is not one).
    pub fn len(&self) -> usize {
        self.segs.len()
    }

    pub fn is_empty(&self) -> bool {
        self.segs.is_empty()
    }

    /// The relative path left after dropping the first `n` segments (and the namespace): what a
    /// container hands the child whose `n`-segment prefix it matched. Empty past the end.
    pub fn tail(&self, n: usize) -> A11yPath {
        A11yPath { ns: None, segs: self.segs.iter().skip(n).cloned().collect() }
    }

    /// Reads a FULL id (`home.shelf.2.card.5`) into a qualified path: `ns()` is the namespace and
    /// `relative()` is the path under it. The result is for comparison, never for logging.
    pub fn parse_id(id: &str) -> Result<A11yPath, IdError> {
        if id.is_empty() {
            return Err(IdError::Empty);
        }
        let (ns, rest) = match id.split_once('.') {
            Some(parts) => parts,
            None if is_ns(id) => return Err(IdError::NoPath),
            None => return Err(IdError::BadNs),
        };
        if !is_ns(ns) {
            return Err(IdError::BadNs);
        }
        // `home.` is an empty segment, not an absent path.
        let mut path = match Self::parse_relative(rest) {
            Err(IdError::Empty) => return Err(IdError::BadSegment),
            other => other?,
        };
        path.ns = Some(Cow::Owned(ns.to_owned()));
        Ok(path)
    }

    /// Reads a RELATIVE path (`shelf.2.card.5`: segments only, at least one).
    pub fn parse_relative(path: &str) -> Result<A11yPath, IdError> {
        if path.is_empty() {
            return Err(IdError::Empty);
        }
        let mut out = A11yPath::new();
        for seg in path.split('.') {
            if let Some(i) = parse_index(seg) {
                out.segs.push(Piece::Index(i));
            } else if is_name(seg) {
                out.segs.push(Piece::Name(Cow::Owned(seg.to_owned())));
            } else {
                return Err(IdError::BadSegment);
            }
        }
        Ok(out)
    }
}

impl fmt::Display for A11yPath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut first = true;
        if let Some(ns) = &self.ns {
            f.write_str(ns)?;
            first = false;
        }
        for p in &self.segs {
            if !first {
                f.write_str(".")?;
            }
            first = false;
            match p {
                Piece::Name(n) => f.write_str(n)?,
                Piece::Index(i) => write!(f, "{i}")?,
            }
        }
        Ok(())
    }
}

impl fmt::Debug for A11yPath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "A11yPath({self})")
    }
}

/// What an element IS, for a test that looks for "the first button" or asserts a row's kind.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Role {
    Card,
    Button,
    Row,
    Tab,
    Field,
    Toggle,
    Text,
}

/// One element's projection: its id and what a test reads about it.
///
/// `label` and `subject` are runtime text (a card's title; the ratingKey of the title the press acts on, see
/// `SUBJECT_BEARING` in `tools/a11y.py`): they go to the state file,
/// never to a log line or heartbeat (a press's `@<subject>` guard is kept by a recording, see the
/// module doc).
/// `loaded` is false for a placeholder whose item has not arrived, and more generally for an element
/// that is **not pressable yet**: a screen whose OK would not land on its focused element for a few
/// frames (Home's hero/grid door, while the picture lags the key that crossed it) projects that element
/// not loaded, and a by-id press of it is refused as `unloaded`. The element's own `a11y` keeps its label
/// and subject, but the state file lists neither for an element that is not `loaded`
/// (`dispatch::snapshot::project_into`). The borrow is of the view the
/// projection was asked of; a view built per query returns [`A11y::into_owned`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct A11y<'a> {
    pub path: A11yPath,
    pub role: Role,
    pub label: Option<Cow<'a, str>>,
    pub subject: Option<Cow<'a, str>>,
    pub loaded: bool,
}

impl<'a> A11y<'a> {
    /// A loaded element with no label and no subject.
    pub fn new(path: A11yPath, role: Role) -> Self {
        Self { path, role, label: None, subject: None, loaded: true }
    }

    pub fn with_label(mut self, label: impl Into<Cow<'a, str>>) -> Self {
        self.label = Some(label.into());
        self
    }

    pub fn with_subject(mut self, subject: impl Into<Cow<'a, str>>) -> Self {
        self.subject = Some(subject.into());
        self
    }

    pub fn with_loaded(mut self, loaded: bool) -> Self {
        self.loaded = loaded;
        self
    }

    /// The same projection owning its text (allocates only for a borrowed label or subject): what
    /// a delegation through a per-query view returns, since the view is a temporary.
    pub fn into_owned(self) -> A11y<'static> {
        A11y {
            path: self.path,
            role: self.role,
            label: self.label.map(|c| Cow::Owned(c.into_owned())),
            subject: self.subject.map(|c| Cow::Owned(c.into_owned())),
            loaded: self.loaded,
        }
    }
}

/// The default [`Focusable::a11y_find`]: walks every group, every `a11y_elems` key of it, and
/// answers the first whose `a11y` path is `path`. Linear in the elements, which is why an indexed
/// widget overrides `a11y_find` instead.
pub fn default_walk<H: Host, F: Focusable<H> + ?Sized>(
    view: &F,
    path: &A11yPath,
    cx: &Cx<'_, H>,
) -> Option<H::Elem> {
    let mut groups = Vec::new();
    view.groups(cx, &mut groups);
    let mut elems = Vec::new();
    for g in &groups {
        elems.clear();
        view.a11y_elems(g.id, cx, &mut elems);
        for e in &elems {
            if view.a11y(e, cx).is_some_and(|a| a.path == *path) {
                return Some(*e);
            }
        }
    }
    None
}

/// Who owns input when a snapshot is taken: the `owner` word of the state file.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OwnerKind {
    /// A page owns input.
    Page,
    /// A modal surface owns input.
    Surface,
    /// The television keyboard is up: no element is addressable.
    Keyboard,
    /// No entry owns input (the tree has no page yet).
    None,
}

impl OwnerKind {
    /// The closed word the state file prints.
    pub fn word(self) -> &'static str {
        match self {
            OwnerKind::Page => "page",
            OwnerKind::Surface => "surface",
            OwnerKind::Keyboard => "keyboard",
            OwnerKind::None => "none",
        }
    }
}

/// One focus group of the owner view, as the state file lists it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GroupSnapshot {
    /// `GroupId.0`, unique within the owner view.
    pub group: u32,
    /// The id prefix its elements share (`home.shelf.2.card`: an element's id is the prefix, then
    /// its index; a one-element group names its parent, `home.hero`). Empty for a group no element
    /// of which projected.
    pub prefix: String,
    /// `GroupSpec::len`: how many elements the group declares, listed or not.
    pub len: usize,
    /// The role of the group's first listed element (`None` when none is listed).
    pub role: Option<Role>,
}

/// One element of the owner view, as the state file lists it.
#[derive(Clone, Debug, PartialEq)]
pub struct ElemSnapshot {
    /// The full id, `<ns>.<path>`.
    pub id: String,
    /// The group it belongs to (`GroupSnapshot::group`).
    pub group: u32,
    /// Its position in the group's `a11y_elems` list.
    pub index: usize,
    pub role: Role,
    /// Where it rests (`Focusable::place` at `SpringTarget`); `None` when the view places nothing
    /// for it.
    pub rest_rect: Option<super::Rect>,
    /// Does any of it show: `rest_rect ∩ clip ∩ viewport` has an area.
    pub visible: bool,
    pub focused: bool,
    pub loaded: bool,
    /// Runtime text, the state file only. An unloaded element carries none.
    pub label: Option<String>,
    pub subject: Option<String>,
}

/// **What `state:<nonce>` reads back** (the a11y-ids plan, D8): the owner view of the dispatcher at
/// one instant, owned and non-generic so the application can serialize it without `plx_ui` naming a
/// JSON type. There is no cap on the element list. `label` and `subject` are runtime text: this value
/// is for the state file and must not reach a `LogLine`, a heartbeat or a recording.
#[derive(Clone, Debug, PartialEq)]
pub struct A11ySnapshot {
    /// The dispatcher's frame index.
    pub frame: u64,
    /// The route: the top PAGE's heartbeat word (`Screen::name`), also while a surface owns input
    /// (`ns` names the owner); `None` when no page is mounted.
    pub route: Option<&'static str>,
    pub owner: OwnerKind,
    /// The owner screen's `a11y_ns`; `None` unless a page or a surface owns input.
    pub ns: Option<&'static str>,
    /// The focused element's full id (`None` when nothing is focused or it does not project).
    pub focused: Option<String>,
    /// Strip group first (when the page wears the strip), then the page's, in declaration order.
    pub groups: Vec<GroupSnapshot>,
    /// Every element of every group, group by group in index order.
    pub elems: Vec<ElemSnapshot>,
}

/// The complaints [`conformance`] has about `view`, one line each (empty when it conforms).
///
/// For every group `view.groups` declares: `a11y_elems` lists AT MOST `GroupSpec::len` keys, and at
/// LEAST one when `len` is above 0; every one places (`place(.., At::SpringTarget)` is `Some`: it is a real element of this view) and
/// projects `Some` (a path of one or more segments that re-parses to itself); the paths are unique
/// across the whole view; and `a11y_find` of each path answers that same key (the elements off
/// screen are listed too, so they are found). A view that declares no group has nothing to check.
///
/// **Why "at most", not "exactly".** `GroupSpec::len` is what focus seating reads, and it counts the
/// group's cells as the layout lays them out: a note, a separator or a pad's blank cell is a cell
/// but no focusable element, so it has no id and `a11y_elems` does not list it. A group's `len` may
/// therefore exceed the listing, and is never bent to equal it (that would move where focus seats);
/// a listing LONGER than `len` is still a fault, since an element the group does not count cannot
/// be one of its elements.
///
/// **Why a group that counts cells must list one.** "At most" alone is satisfied by a view that
/// answers no hook at all (every group lists nothing, so nothing is checked): the three hooks
/// would pass vacuously. A group with `len > 0` has a cell a reader can reach, so one element at
/// least has an id; a group whose cells are all blank is declared with `len` 0 or is not a group.
#[cfg(any(test, feature = "test-support"))]
pub fn conformance_report<H: Host, F: Focusable<H> + ?Sized>(view: &F, cx: &Cx<'_, H>) -> Vec<String> {
    use std::collections::HashSet;

    let mut complaints = Vec::new();
    let mut groups = Vec::new();
    view.groups(cx, &mut groups);
    let mut seen: HashSet<A11yPath> = HashSet::new();
    let mut elems = Vec::new();
    for g in &groups {
        elems.clear();
        view.a11y_elems(g.id, cx, &mut elems);
        if elems.len() > g.len {
            complaints.push(format!(
                "group {}: a11y_elems lists {} elements, more than GroupSpec.len {}",
                g.id.0, elems.len(), g.len
            ));
        }
        if g.len > 0 && elems.is_empty() {
            complaints.push(format!(
                "group {}: a11y_elems lists no element although GroupSpec.len is {} (the hook is not answered)",
                g.id.0, g.len
            ));
        }
        for (i, k) in elems.iter().enumerate() {
            if view.place(k, cx, At::SpringTarget).is_none() {
                complaints.push(format!("group {} element {i}: place(SpringTarget) is None", g.id.0));
            }
            let Some(a) = view.a11y(k, cx) else {
                complaints.push(format!("group {} element {i}: a11y is None", g.id.0));
                continue;
            };
            let id = a.path.to_string();
            if a.path.len() > MAX_ID_SEGS {
                complaints.push(format!(
                    "group {} element {i}: {id:?} has {} segments, more than MAX_ID_SEGS {MAX_ID_SEGS}",
                    g.id.0, a.path.len()
                ));
            }
            let reparsed = if a.path.ns().is_some() {
                A11yPath::parse_id(&id)
            } else {
                A11yPath::parse_relative(&id)
            };
            if reparsed.as_ref() != Ok(&a.path) {
                complaints.push(format!("group {} element {i}: {id:?} is not a well-formed id", g.id.0));
            }
            if !seen.insert(a.path.clone()) {
                complaints.push(format!("group {} element {i}: {id:?} is not unique in this view", g.id.0));
            }
            if view.a11y_find(&a.path, cx) != Some(*k) {
                complaints.push(format!("group {} element {i}: a11y_find({id:?}) is not that element", g.id.0));
            }
        }
    }
    complaints
}

/// Panics with every complaint of [`conformance_report`]. The suite a widget or a screen runs over
/// its own view in a host test.
#[cfg(any(test, feature = "test-support"))]
pub fn conformance<H: Host, F: Focusable<H> + ?Sized>(view: &F, cx: &Cx<'_, H>) {
    let complaints = conformance_report(view, cx);
    assert!(complaints.is_empty(), "a11y conformance:\n{}", complaints.join("\n"));
}

/// **Run [`conformance`] over one mounted screen and answer its namespace** (`Screen::a11y_ns`): the
/// one call every `<screen>_conforms` makes per mounting, so a mount cannot be run through
/// conformance without its namespace being read and checked too. It lives here, beside
/// [`conformance`], because it uses nothing a screen owns: a screen's test module names it as
/// `plx_ui::a11y::conform`, never through a sibling module.
#[cfg(any(test, feature = "test-support"))]
pub fn conform<H: Host, S: super::screen::Screen<H> + ?Sized>(view: &S, cx: &Cx<'_, H>) -> &'static str {
    conformance::<H, _>(view, cx);
    let ns = super::screen::Screen::<H>::a11y_ns(view);
    assert!(is_ns(ns), "{ns:?} is not a namespace");
    ns
}

/// What a covered screen's `<screen>_conforms` answers: the namespace of each mounting it ran
/// ([`conform`]).
#[cfg(any(test, feature = "test-support"))]
pub type Namespaces = Vec<&'static str>;

/// Every element `view` lists, as it projects, in group order and then index order: the whole id
/// set a test reads (an element that projects nothing is skipped; [`conformance`] is what complains
/// of that). For a test that asserts the names a page gives its sections.
#[cfg(any(test, feature = "test-support"))]
pub fn listing<H: Host, F: Focusable<H> + ?Sized>(view: &F, cx: &Cx<'_, H>) -> Vec<A11y<'static>> {
    let mut groups = Vec::new();
    view.groups(cx, &mut groups);
    let (mut elems, mut out) = (Vec::new(), Vec::new());
    for g in &groups {
        elems.clear();
        view.a11y_elems(g.id, cx, &mut elems);
        out.extend(elems.iter().filter_map(|k| view.a11y(k, cx)).map(A11y::into_owned));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixture::{FixtureHost, FixtureMeasure, FixtureView, FixtureViews};
    use crate::screen::{
        At, AxisMask, Dir, EdgeRule, ElemKind, GroupKind, GroupSpec, Placed, Seat, Step,
    };
    use crate::Rect;
    use plx_machine::machine::{
        EntryId, FocusKey, FocusRead, GroupId, InputOwner, Measure, PressRead, Tick,
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

    /// How a fixture row breaks the contract (`None`: it keeps it; `Blank`: it keeps it too).
    #[derive(Clone, Copy, PartialEq, Eq)]
    enum Fault {
        None,
        /// The last element projects nothing.
        Unprojected,
        /// The last two elements share one id.
        Duplicate,
        /// `a11y_elems` lists one element MORE than `GroupSpec.len` counts.
        Long,
        /// The last element places nowhere.
        Unplaced,
        /// `a11y_find` answers the neighbour.
        Misfound,
        /// `a11y_elems` lists nothing for a group that counts cells (a view that answers no hook).
        Silent,
        /// NOT a fault: `GroupSpec.len` counts one more cell than there are elements (a note, a
        /// separator, a pad's blank cell), so the listing is one short of it.
        Blank,
    }

    /// A row of `len` cards, `row.card.<i>`, in one group: the smallest hand-written `Focusable`.
    /// `indexed` makes it answer `a11y_find` the way an indexed widget does, by parsing the index.
    struct Row {
        len: usize,
        indexed: bool,
        fault: Fault,
    }

    impl Row {
        /// How many elements `a11y_elems` lists (and `a11y` answers).
        fn listed(&self) -> usize {
            if self.fault == Fault::Long {
                self.len + 1
            } else {
                self.len
            }
        }
        /// What `GroupSpec.len` says: the cells, which a `Blank` row has one more of than elements.
        fn declared(&self) -> usize {
            if self.fault == Fault::Blank {
                self.len + 1
            } else {
                self.len
            }
        }
        fn path(&self, i: usize) -> A11yPath {
            let i = if self.fault == Fault::Duplicate && i + 1 == self.len { i - 1 } else { i };
            A11yPath::new().name("card").index(i)
        }
    }

    impl Focusable<H> for Row {
        fn groups(&self, _cx: &Cx<'_, H>, out: &mut Vec<GroupSpec>) {
            out.push(GroupSpec {
                id: GroupId(7),
                kind: GroupKind::Row { wrap: false },
                seat: Seat::Nearest,
                reachable: AxisMask::BOTH,
                edge: [EdgeRule::Geometric; 4],
                extent: Rect::new(0.0, 0.0, 1920.0, 200.0),
                len: self.declared(),
                elem: ElemKind::Card,
            });
        }
        fn group_of(&self, key: &u32, _cx: &Cx<'_, H>) -> Option<GroupId> {
            ((*key as usize) < self.listed()).then_some(GroupId(7))
        }
        fn neighbour(&self, _key: FocusKey<u32>, _dir: Dir, _cx: &Cx<'_, H>) -> Step<u32> {
            Step::Edge
        }
        fn place(&self, key: &u32, _cx: &Cx<'_, H>, _at: At) -> Option<Placed> {
            let i = *key as usize;
            if i >= self.listed() || (self.fault == Fault::Unplaced && i + 1 == self.len) {
                return None;
            }
            let rect = Rect::new(i as f32 * 100.0, 0.0, 90.0, 200.0);
            Some(Placed { rect, rest_rect: rect, clip: rect, index: Some(i as u32) })
        }
        fn reconcile(&self, want: FocusKey<u32>, _cx: &Cx<'_, H>) -> FocusKey<u32> {
            want
        }
        fn seat(&self, _g: GroupId, _from: Placed, _cx: &Cx<'_, H>) -> FocusKey<u32> {
            FocusKey { entry: EntryId(0), elem: 0 }
        }
        fn a11y(&self, key: &u32, _cx: &Cx<'_, H>) -> Option<A11y<'_>> {
            let i = *key as usize;
            if i >= self.listed() || (self.fault == Fault::Unprojected && i + 1 == self.len) {
                return None;
            }
            Some(A11y::new(self.path(i), Role::Card))
        }
        fn a11y_elems(&self, g: GroupId, _cx: &Cx<'_, H>, out: &mut Vec<u32>) {
            if g == GroupId(7) && self.fault != Fault::Silent {
                out.extend(0..self.listed() as u32);
            }
        }
        fn a11y_find(&self, path: &A11yPath, cx: &Cx<'_, H>) -> Option<u32> {
            if !self.indexed {
                return default_walk(self, path, cx);
            }
            let mut segs = path.segs();
            match (segs.next(), segs.next(), segs.next()) {
                (Some(Seg::Name("card")), Some(Seg::Index(i)), None) if i < self.listed() => {
                    let k = if self.fault == Fault::Misfound { i + 1 } else { i };
                    Some(k as u32)
                }
                _ => None,
            }
        }
    }

    fn report(row: &Row) -> Vec<String> {
        let (m, v) = (FixtureMeasure, FixtureView::default());
        conformance_report(row, &cx(&m, &v))
    }

    /// **A hand-written `Focusable` that answers the three hooks conforms**, whether it takes the
    /// default walk or answers `a11y_find` by the index it parses, and the path it projects is the
    /// text a test types.
    #[test]
    fn a_hand_written_focusable_conforms_by_walk_and_by_index() {
        let (m, v) = (FixtureMeasure, FixtureView::default());
        let cx = cx(&m, &v);
        for indexed in [false, true] {
            let row = Row { len: 4, indexed, fault: Fault::None };
            conformance(&row, &cx);
            let want = A11yPath::parse_relative("card.2").unwrap();
            assert_eq!(row.a11y(&2, &cx).unwrap().path, want);
            assert_eq!(row.a11y_find(&want, &cx), Some(2));
            let past_the_end = A11yPath::parse_relative("card.4").unwrap();
            assert_eq!(row.a11y_find(&past_the_end, &cx), None);
        }
    }

    /// **Each of the six properties is checked**: an element that projects, that is unique, that
    /// places, that `a11y_find` answers, a listing no longer than `GroupSpec.len` and, for a group
    /// that counts any cell, no shorter than one element.
    #[test]
    fn conformance_names_each_broken_property() {
        for (fault, want) in [
            (Fault::Unprojected, "a11y is None"),
            (Fault::Duplicate, "not unique"),
            (Fault::Long, "more than GroupSpec.len"),
            (Fault::Unplaced, "place(SpringTarget) is None"),
            (Fault::Misfound, "is not that element"),
            (Fault::Silent, "lists no element"),
        ] {
            let got = report(&Row { len: 3, indexed: true, fault });
            assert!(got.iter().any(|c| c.contains(want)), "{want:?} not reported: {got:?}");
        }
        assert!(report(&Row { len: 3, indexed: false, fault: Fault::Duplicate })
            .iter()
            .any(|c| c.contains("not unique")));
    }

    /// **A group whose `len` counts a cell that is no element still conforms**: `len` is the layout's
    /// cells (a note, a separator, a pad's blank cell), which seating reads and which the ids never
    /// bend, so the listing may be shorter than it; only a listing LONGER than `len` is a fault. By
    /// walk and by index.
    #[test]
    fn a_group_whose_len_counts_a_non_element_cell_conforms() {
        let (m, v) = (FixtureMeasure, FixtureView::default());
        let cx = cx(&m, &v);
        for indexed in [false, true] {
            let row = Row { len: 3, indexed, fault: Fault::Blank };
            let mut groups = Vec::new();
            row.groups(&cx, &mut groups);
            assert_eq!(groups[0].len, 4, "the group counts the blank cell");
            let mut elems = Vec::new();
            row.a11y_elems(GroupId(7), &cx, &mut elems);
            assert_eq!(elems, [0, 1, 2], "the listing does not");
            assert_eq!(report(&row), Vec::<String>::new());
            conformance(&row, &cx);
        }
    }

    /// **A view that answers no hook does not pass vacuously**: a group that counts cells and lists
    /// no element is the one complaint, by walk and by index, and the same view with `len` 0 (a page
    /// whose data has not landed) has nothing to list.
    #[test]
    fn a_group_that_counts_cells_must_list_an_element() {
        for indexed in [false, true] {
            let got = report(&Row { len: 3, indexed, fault: Fault::Silent });
            assert_eq!(
                got,
                ["group 7: a11y_elems lists no element although GroupSpec.len is 3 (the hook is not answered)"],
                "indexed: {indexed}"
            );
        }
        assert!(report(&Row { len: 0, indexed: true, fault: Fault::Silent }).is_empty());
    }

    /// A view with no group has nothing to check (a page whose data has not landed).
    #[test]
    fn a_view_with_no_elements_conforms() {
        assert!(report(&Row { len: 0, indexed: false, fault: Fault::None }).is_empty());
    }

    /// **The grammar round-trips** (plan A test 5): a hyphenated namespace is accepted; whitespace,
    /// brackets, upper case, empty segments and a hyphen in a path segment are not.
    #[test]
    fn the_grammar_round_trips_and_refuses_what_a_shell_or_a_reader_would_mangle() {
        for id in ["home.shelf.2.card.5", "detail.hero.play", "nav.watchlist", "under-posters.row.0", "playback.audio_boost"] {
            let p = A11yPath::parse_id(id).unwrap_or_else(|e| panic!("{id}: {e}"));
            assert_eq!(p.to_string(), id);
            assert_eq!(A11yPath::parse_id(&p.to_string()), Ok(p));
        }
        let p = A11yPath::parse_id("under-posters.row.0").unwrap();
        assert_eq!(p.ns(), Some("under-posters"));
        assert_eq!(p.relative().to_string(), "row.0");
        assert_eq!(p.segs().collect::<Vec<_>>(), [Seg::Name("row"), Seg::Index(0)]);

        for (bad, why) in [
            ("home.shelf 2", IdError::BadSegment),
            ("home.shelf\t2", IdError::BadSegment),
            ("home shelf.2", IdError::BadNs),
            ("home.row[2]", IdError::BadSegment),
            ("home.Shelf", IdError::BadSegment),
            ("Home.shelf", IdError::BadNs),
            ("home..card", IdError::BadSegment),
            ("home.", IdError::BadSegment),
            (".home", IdError::BadNs),
            ("home.shelf-2", IdError::BadSegment),
            ("home.1shelf", IdError::BadSegment),
            ("home.02", IdError::BadSegment),
            ("home.-1", IdError::BadSegment),
            ("-home.shelf", IdError::BadNs),
            ("1home.shelf", IdError::BadNs),
            ("home", IdError::NoPath),
            ("Home", IdError::BadNs),
            ("", IdError::Empty),
        ] {
            assert_eq!(A11yPath::parse_id(bad), Err(why), "{bad:?}");
        }
        assert_eq!(A11yPath::parse_relative(""), Err(IdError::Empty));
        assert_eq!(A11yPath::parse_relative("a-b"), Err(IdError::BadSegment));
    }

    /// A path a widget built equals the one a test's text parses to, and the builder's composition
    /// (join, tail, qualified) is what a container needs.
    #[test]
    fn a_built_path_equals_a_parsed_one_and_composes() {
        let built = A11yPath::new().name("shelf").index(2).name("card").index(5).qualified("home");
        assert_eq!(built, A11yPath::parse_id("home.shelf.2.card.5").unwrap());
        assert_eq!(built.relative(), A11yPath::parse_relative("shelf.2.card.5").unwrap());
        assert_eq!(built.len(), 4);
        assert_eq!(built.tail(2), A11yPath::parse_relative("card.5").unwrap());
        assert!(built.tail(9).is_empty());

        let child = A11yPath::new().name("card").index(5);
        let joined = A11yPath::new().name("shelf").index(2).join(&child);
        assert_eq!(joined.to_string(), "shelf.2.card.5");
        // Qualifying is idempotent: the first namespace stands.
        assert_eq!(built.clone().qualified("nav"), built);
        // The same path in a hash set: ids are keys.
        let mut set = std::collections::HashSet::new();
        assert!(set.insert(built.clone()) && !set.insert(A11yPath::parse_id("home.shelf.2.card.5").unwrap()));
    }

    #[test]
    fn a_projection_can_own_its_text() {
        let label = String::from("The title");
        let a = A11y::new(A11yPath::new().name("card"), Role::Card)
            .with_label(label.as_str())
            .with_subject("12345")
            .with_loaded(false);
        let owned: A11y<'static> = a.clone().into_owned();
        assert_eq!(owned, a);
        assert_eq!(owned.label.as_deref(), Some("The title"));
        assert!(!owned.loaded);
    }
}
