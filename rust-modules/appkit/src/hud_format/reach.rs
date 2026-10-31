//! **Reachability over the HUD's state graph** (`docs/hud-plugins.md`, "Reachability over the HUD's state
//! graph"): R1, R3 and R4 as findings, R2 as DATA (the commands the app folds into More), over a parsed
//! [`Package`]. [`check`] is the one entry point; it then runs [`super::validate`]'s invariants.
//!
//! **The graph.** A state is (the open sections, the slot). The content conditions the package names
//! (`chapters.present`, `title.is_episode`, anywhere in a `when` or a keyed value) each take both values,
//! so at most four valuations; one that the package does not name holds (an unnamed predicate asks
//! nothing). A package that PLACES `previous` or `next` names `title.is_episode` too, whether or not it
//! writes the term: the command table makes the pair absent for a non-episode, so the movie form is a
//! valuation of every such package (the reference has two, not one). `transport.paused` is not a dimension: `toggle_play`'s keys flip it, so a control whose
//! placement depends on it counts as placed when either value places it. The root is (no section, Discs).
//! Sections: those the package lays out, a band whose `when` holds `panel.s`; `modal` opens one at a time,
//! `inline` any subset. Slots: Discs, Up Next and Skip. The edges are the app-owned ones the plan lists:
//! OK on a placed, enabled `toggle_section` control (Chapters is enabled by `chapters.present`); BACK, which
//! closes a section (a direction leaving a modal section's groups closes it too, to the same states); and
//! the slot, which changes involuntarily with the marker, any to any, in the compact state only: while a
//! section is open the row keeps the package's controls (D7), so the slot there is Discs. The states are
//! few and the walk deterministic: at most 4 valuations x (3 + 3) states.
//!
//! **Placement.** A control is placed in a state when every `when` on its way holds (its band's, its
//! cell's, the cells of a `vstack` or `rect` around it, its own), and, unless the slot is Discs, its
//! segment is not one of `up_next.replaces`. A strip card's cells are not focus stops and are not walked.
//!
//! **R2 folds** are computed from the package's own controls only: a folded More row is not an edge of
//! this graph, so a section's content can fold (`from_beginning` in an Info band whose opener is missing)
//! although a folded Info row would reach it. The row is then redundant, never missing. A section the package
//! does not lay out never folds: its More row would enter a state where nothing is drawn.
//!
//! **R1 checks presence only.** The size floor `MIN_CONTROL_SIZE` is a geometric bound and belongs to the
//! compiler, which has the rects; no value is invented here.
//!
//! Pure: no IO, no globals, no clock.

use std::collections::BTreeSet;

use super::parse::{Cond, Object, Package, Val};
use super::validate::{self, each_object};
use super::{codes, pointer_index, pointer_push, Finding};

/// A command the app folds into More's root when no reachable state places it, with the section it opens
/// when it is `toggle_section` (the two sections are two rows; `open_tracks` is ONE row whatever its tab).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Fold {
    pub command: &'static str,
    pub section: Option<&'static str>,
}

/// What [`check`] reports. (Not `Eq`: a [`Finding`] carries a context of `f32` rects.)
#[derive(Clone, Debug, PartialEq)]
pub struct Checked {
    /// Reachability errors R1, R3 and R4 in that order, then the invariants of [`super::validate`]; each
    /// group in document order. No fold is a finding.
    pub findings: Vec<Finding>,
    /// R2: the commands folded into More, in the order of the plan's command table (More lists them after
    /// Quality and Speed and before Options). Information, never a finding.
    pub folds: Vec<Fold>,
    /// The reachable states summed over the valuations: what bounds the walk, and a test pins it.
    pub states: usize,
}

/// The key-less commands a package may leave unplaced, in the plan's table order. `open_more` (R1),
/// `toggle_stats` (it has its own More row) and the keyed `toggle_play` and `seek` are not here.
const FOLDABLE: [Fold; 7] = [
    Fold { command: "previous", section: None },
    Fold { command: "next", section: None },
    Fold { command: "open_tracks", section: None },
    Fold { command: "toggle_section", section: Some("chapters") },
    Fold { command: "toggle_section", section: Some("info") },
    Fold { command: "from_beginning", section: None },
    Fold { command: "go_to_detail", section: None },
];

/// The sections: a bit of [`State::open`], the name `toggle_section` and `panel.<name>` use.
const SECTIONS: [(u8, &str); 2] = [(1, "chapters"), (2, "info")];

/// Check a parsed package: R1, R3, R4, the folds (R2) and the invariants of [`super::validate`].
pub fn check(package: &Package) -> Checked {
    let layout = Layout::read(&package.layout);
    let mut findings = Vec::new();
    let mut folded = [false; FOLDABLE.len()];
    let mut states = 0;
    let mut more_missing = None;

    for v in layout.valuations() {
        let reachable = layout.explore(v);
        states += reachable.len();
        for (i, fold) in FOLDABLE.iter().enumerate() {
            let placed = reachable.iter().any(|&s| {
                layout
                    .controls
                    .iter()
                    .any(|c| carries(c, fold) && layout.placed(c, s, v))
            });
            folded[i] |= !placed && layout.enabled(fold, v);
        }
        if more_missing.is_none() && !layout.more_is_placed(v) {
            more_missing = Some(v);
        }
    }

    if let Some(v) = more_missing {
        findings.push(layout.more_finding(v));
    }
    layout.opener_findings(&mut findings);
    layout.up_next_findings(&package.layout, &mut findings);
    findings.extend(validate::check(package));

    Checked {
        findings,
        folds: FOLDABLE
            .iter()
            .zip(folded)
            .filter_map(|(fold, folded)| folded.then_some(*fold))
            .collect(),
        states,
    }
}

/// What occupies the control row: the package's controls, the Up Next offer or the Skip pill.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Slot {
    Discs,
    UpNext,
    Skip,
}

const SLOTS: [Slot; 3] = [Slot::Discs, Slot::UpNext, Slot::Skip];

/// The open sections (a bitmask of [`SECTIONS`]) and the control slot.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) struct State {
    pub(super) open: u8,
    pub(super) slot: Slot,
}

impl State {
    const ROOT: State = State { open: 0, slot: Slot::Discs };

    /// A dense index: `open` is below 4, the slot below 3.
    fn index(self) -> usize {
        usize::from(self.open) * SLOTS.len() + self.slot as usize
    }
}

/// A valuation of the content conditions the package names; one it does not name holds.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) struct Valuation {
    pub(super) chapters: bool,
    pub(super) episode: bool,
}

/// A `when` on the way to a control, with the pointer of the `when` key.
#[derive(Clone)]
struct Gate<'a> {
    cond: &'a Cond,
    pointer: String,
}

struct Control<'a> {
    item: &'a Object,
    command: &'a str,
    /// The id of the segment the control stands in; `None` in an `actions` cell.
    segment: Option<&'a str>,
    gates: Vec<Gate<'a>>,
}

impl Control<'_> {
    fn section(&self) -> Option<&str> {
        self.item.get("section").and_then(Val::as_str)
    }
}

fn carries(control: &Control<'_>, fold: &Fold) -> bool {
    control.command == fold.command
        && fold.section.map_or(true, |s| control.section() == Some(s))
}

/// What the checks read of `layout`: the controls with their gates, the bands and segments, the sections
/// the package lays out, whether sections are modal, which segments the stand-ins replace.
pub(super) struct Layout<'a> {
    controls: Vec<Control<'a>>,
    band_ids: BTreeSet<&'a str>,
    segment_ids: BTreeSet<&'a str>,
    laid: u8,
    modal: bool,
    replaces: BTreeSet<&'a str>,
    pub(super) named_chapters: bool,
    /// `title.is_episode` appears in a condition, or the package places `previous` or `next`
    pub(super) named_episode: bool,
}

impl<'a> Layout<'a> {
    pub(super) fn read(layout: &'a Object) -> Layout<'a> {
        let mut this = Layout {
            controls: Vec::new(),
            band_ids: BTreeSet::new(),
            segment_ids: BTreeSet::new(),
            laid: 0,
            modal: layout.get("sections").and_then(Val::as_str) != Some("inline"),
            replaces: BTreeSet::new(),
            named_chapters: false,
            named_episode: false,
        };
        if let Some(Val::List(bands)) = layout.get("block") {
            for band in bands.iter().filter_map(Val::as_object) {
                if let Some(id) = band.get("id").and_then(Val::as_str) {
                    this.band_ids.insert(id);
                }
                if let Some(Val::Cond(cond)) = band.get("when") {
                    for (bit, name) in SECTIONS {
                        let panel = format!("panel.{name}");
                        if cond.terms.iter().any(|t| !t.negated && t.name == panel) {
                            this.laid |= bit;
                        }
                    }
                }
            }
        }
        if let Some(Val::Object(up_next)) = layout.get("up_next") {
            if let Some(Val::List(names)) = up_next.get("replaces") {
                this.replaces.extend(names.iter().filter_map(Val::as_str));
            }
        }
        let mut gates = Vec::new();
        this.collect(layout, &mut gates, None);
        let mut named = (false, false);
        scan_named(layout, &mut named);
        (this.named_chapters, this.named_episode) = named;
        // The command table makes the previous / next pair absent for a non-episode (or with neither
        // neighbour) whatever the package says, so a package that places either has the movie form
        // although it never writes `title.is_episode`: it names the condition by placing the pair.
        this.named_episode |= this
            .controls
            .iter()
            .any(|c| matches!(c.command, "previous" | "next"));
        this
    }

    fn collect(&mut self, object: &'a Object, gates: &mut Vec<Gate<'a>>, segment: Option<&'a str>) {
        let pushed = match object.get("when") {
            Some(Val::Cond(cond)) => {
                gates.push(Gate { cond, pointer: pointer_push(&object.pointer, "when") });
                true
            }
            _ => false,
        };
        match object.node {
            "item" => self.controls.push(Control {
                item: object,
                command: object.get("command").and_then(Val::as_str).unwrap_or(""),
                segment,
                gates: gates.clone(),
            }),
            // A strip card's cells draw over and under its art: no focus stop, so no control.
            "card" => {}
            node => {
                let mut segment = segment;
                if node == "segment" {
                    segment = object.get("id").and_then(Val::as_str);
                    if let Some(id) = segment {
                        self.segment_ids.insert(id);
                    }
                }
                for (_, value) in &object.fields {
                    self.collect_in(value, gates, segment);
                }
            }
        }
        if pushed {
            gates.pop();
        }
    }

    fn collect_in(&mut self, value: &'a Val, gates: &mut Vec<Gate<'a>>, segment: Option<&'a str>) {
        match value {
            Val::Object(object) => self.collect(object, gates, segment),
            Val::List(items) => {
                for item in items {
                    self.collect_in(item, gates, segment);
                }
            }
            _ => {}
        }
    }

    /// Both values of every content condition the package names, `false` first; a single `true` for one
    /// it does not name.
    pub(super) fn valuations(&self) -> Vec<Valuation> {
        let dim = |named: bool| if named { vec![false, true] } else { vec![true] };
        let mut out = Vec::new();
        for chapters in dim(self.named_chapters) {
            for episode in dim(self.named_episode) {
                out.push(Valuation { chapters, episode });
            }
        }
        out
    }

    pub(super) fn holds(&self, cond: &Cond, state: State, v: Valuation, paused: bool) -> bool {
        cond.terms.iter().all(|term| {
            let truth = match term.name.as_str() {
                "panel.chapters" => state.open & 1 != 0,
                "panel.info" => state.open & 2 != 0,
                "sections" => state.open != 0,
                "chapters.present" => v.chapters,
                "title.is_episode" => v.episode,
                "transport.paused" => paused,
                "slot.discs" => state.slot == Slot::Discs,
                "slot.up_next" => state.slot == Slot::UpNext,
                "slot.skip" => state.slot == Slot::Skip,
                // `item.*` is legal in a strip card only, whose cells are not walked.
                _ => false,
            };
            truth != term.negated
        })
    }

    /// The first `when` on the way to `control` that does not hold.
    fn blocker<'c>(
        &self,
        control: &'c Control<'a>,
        state: State,
        v: Valuation,
        paused: bool,
    ) -> Option<&'c Gate<'a>> {
        control
            .gates
            .iter()
            .find(|gate| !self.holds(gate.cond, state, v, paused))
    }

    /// Whether `control` is drawn and focusable in `state` under `v`, for either value of `paused`.
    fn placed(&self, control: &Control<'a>, state: State, v: Valuation) -> bool {
        let replaced = state.slot != Slot::Discs
            && control.segment.is_some_and(|s| self.replaces.contains(s));
        !replaced
            && [false, true]
                .into_iter()
                .any(|paused| self.blocker(control, state, v, paused).is_none())
    }

    /// Whether a folded row for the command would do something under `v`: Previous and Next need an
    /// episode, the chapters section needs chapters, and a section row needs a section the package lays
    /// out (a row that enters a section with no band would open nothing); the rest always.
    fn enabled(&self, fold: &Fold, v: Valuation) -> bool {
        let laid = |name| SECTIONS.iter().any(|&(bit, n)| n == name && self.laid & bit != 0);
        match (fold.command, fold.section) {
            ("previous" | "next", _) => v.episode,
            ("toggle_section", Some(name)) => laid(name) && (name != "chapters" || v.chapters),
            _ => true,
        }
    }

    /// Whether some `open_more` that opens More's root is placed in the root state under `v`.
    fn more_is_placed(&self, v: Valuation) -> bool {
        self.root_more()
            .any(|c| self.placed(c, State::ROOT, v))
    }

    fn root_more(&self) -> impl Iterator<Item = &Control<'a>> {
        self.controls.iter().filter(|c| {
            c.command == "open_more"
                && matches!(c.item.get("page").and_then(Val::as_str), None | Some("root"))
        })
    }

    /// The states reachable from the root under `v`, breadth first.
    pub(super) fn explore(&self, v: Valuation) -> Vec<State> {
        let mut seen = [false; 4 * SLOTS.len()];
        let mut order = vec![State::ROOT];
        seen[State::ROOT.index()] = true;
        let mut next = 0;
        while next < order.len() {
            let state = order[next];
            next += 1;
            for to in self.successors(state, v) {
                if !std::mem::replace(&mut seen[to.index()], true) {
                    order.push(to);
                }
            }
        }
        order
    }

    fn successors(&self, state: State, v: Valuation) -> Vec<State> {
        let mut opens = Vec::new();
        for (bit, name) in SECTIONS {
            let opener = self.laid & bit != 0
                && (name != "chapters" || v.chapters)
                && self.controls.iter().any(|c| {
                    c.command == "toggle_section"
                        && c.section() == Some(name)
                        && self.placed(c, state, v)
                });
            if opener {
                opens.push(if state.open & bit != 0 {
                    state.open & !bit
                } else if self.modal {
                    bit
                } else {
                    state.open | bit
                });
            }
        }
        // BACK closes the latest section, which is any one of them; a modal section has one.
        for (bit, _) in SECTIONS {
            if state.open & bit != 0 {
                opens.push(state.open & !bit);
            }
        }
        let mut out = Vec::new();
        for open in opens {
            // The stand-in waits behind an open section and takes its segments when the last one closes.
            let slots: &[Slot] = if open == 0 { &SLOTS } else { &[Slot::Discs] };
            out.extend(slots.iter().map(|&slot| State { open, slot }));
        }
        if state.open == 0 {
            out.extend(SLOTS.iter().map(|&slot| State { open: 0, slot }));
        }
        out
    }

    /// R1, for the valuation `v` under which More is not in the root state.
    fn more_finding(&self, v: Valuation) -> Finding {
        let hint = "place an open_more control, with no when, in a band and a cell that are always shown";
        let Some(more) = self.root_more().next() else {
            let (pointer, why) = match self
                .controls
                .iter()
                .find(|c| c.command == "open_more")
            {
                Some(other) => (
                    other.item.pointer.clone(),
                    "the open_more control opens a page, not More's root",
                ),
                None => ("/layout/block".to_owned(), "the package places no open_more control"),
            };
            return Finding::new(&codes::E0801, pointer, why.to_owned(), hint);
        };
        let blocker = [false, true]
            .into_iter()
            .find_map(|paused| self.blocker(more, State::ROOT, v, paused));
        let pointer = blocker.map_or_else(|| more.item.pointer.clone(), |g| g.pointer.clone());
        Finding::new(
            &codes::E0801,
            pointer,
            format!(
                "More is not placed in the root state ({}): a title would lose Stats for nerds",
                self.describe(v)
            ),
            hint,
        )
    }

    fn describe(&self, v: Valuation) -> String {
        let mut parts = Vec::new();
        if self.named_chapters {
            parts.push(format!("chapters.present is {}", v.chapters));
        }
        if self.named_episode {
            parts.push(format!("title.is_episode is {}", v.episode));
        }
        if parts.is_empty() {
            "in every state".to_owned()
        } else {
            format!("when {}", parts.join(" and "))
        }
    }

    /// R3: a `toggle_section` control and no band whose `when` holds `panel.<section>`.
    fn opener_findings(&self, out: &mut Vec<Finding>) {
        for control in self.controls.iter().filter(|c| c.command == "toggle_section") {
            let Some(section) = control.section() else {
                continue;
            };
            let laid = SECTIONS
                .iter()
                .any(|&(bit, name)| name == section && self.laid & bit != 0);
            if !laid {
                out.push(Finding::new(
                    &codes::E0802,
                    pointer_push(&control.item.pointer, "section"),
                    format!("no band is shown `when: \"panel.{section}\"`: this control opens nothing"),
                    format!("add a band with \"when\": \"panel.{section}\", or remove this control"),
                ));
            }
        }
    }

    /// R4: the part of "Up Next is whole" the parser cannot see: what `replaces` and `band` name exists.
    fn up_next_findings(&self, layout: &Object, out: &mut Vec<Finding>) {
        let Some(Val::Object(up_next)) = layout.get("up_next") else {
            return;
        };
        if let Some(Val::List(names)) = up_next.get("replaces") {
            let at = pointer_push(&up_next.pointer, "replaces");
            if names.is_empty() {
                out.push(Finding::new(
                    &codes::E0803,
                    at.clone(),
                    "Up Next replaces no segment, so it has no place to stand".to_owned(),
                    "name the segment of the control row the offer replaces, for example [\"row\"]",
                ));
            }
            for (i, name) in names.iter().enumerate() {
                let Some(name) = name.as_str() else { continue };
                if !self.segment_ids.contains(name) {
                    out.push(Finding::new(
                        &codes::E0803,
                        pointer_index(&at, i),
                        format!("Up Next replaces `{name}`, which is not a segment of this package"),
                        "name a segment id that a band's cell declares",
                    ));
                }
            }
        }
        if let Some(band) = up_next.get("band").and_then(Val::as_str) {
            if !self.band_ids.contains(band) {
                out.push(Finding::new(
                    &codes::E0803,
                    pointer_push(&up_next.pointer, "band"),
                    format!("Up Next is drawn in the band `{band}`, which is not a band of this package"),
                    "name a band id of layout.block",
                ));
            }
        }
    }
}

/// Which of `chapters.present` and `title.is_episode` appear anywhere under `root`: in a `when`, or as the
/// condition of a state-keyed value. (A placed `previous` / `next` names the second as well, which
/// [`Layout::read`] adds: it is a fact of the controls, not a term of a condition.)
fn scan_named(root: &Object, named: &mut (bool, bool)) {
    fn note(cond: &Cond, named: &mut (bool, bool)) {
        for term in &cond.terms {
            match term.name.as_str() {
                "chapters.present" => named.0 = true,
                "title.is_episode" => named.1 = true,
                _ => {}
            }
        }
    }
    each_object(root, &mut |object| {
        for (_, value) in &object.fields {
            match value {
                Val::Cond(cond) => note(cond, named),
                Val::Keyed(keyed) => keyed.cases.iter().for_each(|(cond, _)| note(cond, named)),
                _ => {}
            }
        }
    });
}
