//! **The chip row: a run of facts and badges fitted into one line, dropping whole chips tail first**
//! (issue #26). Shared by the Info section's card and the HUD renderer's `facts` cell
//! (`player_hud::render`, `docs/hud-plugins.md` D10 and the package format's `facts` row): one
//! packer, so a package's facts line and classic's Info card cannot disagree about what fits.
//!
//! Moved out of `info_panel.rs` unchanged (a pure move: its tests moved with it and pin the same
//! values). The row is built in priority order, measured before it is drawn, and fitted by
//! `HStack::fit`, which keeps the longest prefix that fits.

use plx_ui::hstack::{Align, HStack, Item};
use plx_ui::theme;
use plx_ui::widgets::{badge, badge_w, BadgeStyle, DottedRun};
use plx_ui::Painter;
use std::os::raw::c_int;

/// the shared outlined chip in this panel's colours (TEXT_HEADING border/label over the card)
pub fn meta_badge(
    p: Painter,
    x: f32,
    cy: f32,
    text: &str,
    measure: &dyn plx_machine::machine::Measure,
) -> f32 {
    badge(
        p,
        x,
        cy,
        text,
        None,
        BadgeStyle::Outlined {
            col: theme::TEXT_HEADING,
            border: theme::OVERLAY_BORDER,
            bg: theme::SURFACE_PANEL,
        },
        measure,
    )
}

/// One candidate on the metadata/badge row — see [`ChipRow`] for why `draw` measures every chip
/// before drawing any of them, rather than drawing straight through and letting the row run under
/// the action-button column (issue #26).
pub struct Chip {
    /// What this chip IS and, for a text run, how it is inked — a data-bearing enum rather than a
    /// `kind` tag plus a `bold`/`col` pair that only ever meant something for one of the two
    /// variants: a `Badge` chip always draws through [`meta_badge`]'s own fixed style, so a flat
    /// `bold`/`col` field on every `Chip` would carry a value for `Badge` that nothing reads and
    /// that a future caller could plausibly set expecting it to matter.
    kind: ChipKind,
    /// This chip's own measured pixel width: a run's parts, dots and the air around them.
    w: f32,
}

pub enum ChipKind {
    /// A text run (the meta line, or the live playback fact): its parts only, since the dots
    /// between them and the air around them belong to `widgets::DottedRun` at draw time;
    /// `lead_dot` continues the run before it, and the two runs on this row are inked differently
    /// (see the call site).
    Run { parts: Vec<String>, lead_dot: bool, bold: c_int, col: [f32; 4] },
    /// An outlined capability badge ([`meta_badge`]) saying this.
    Badge(String),
}

impl Chip {
    /// A text run of `parts`, a dot between each pair and, with `lead_dot`, one before the first (a
    /// run that continues the one before it). Spaced exactly like Detail's identity line, by the
    /// same dotted-run idiom (`space::SM` every side of the dot), and measured by it.
    pub fn run(
        parts: &[String],
        lead_dot: bool,
        bold: c_int,
        col: [f32; 4],
        measure: &dyn plx_machine::machine::Measure,
    ) -> Self {
        let w = Self::dotted(bold, lead_dot, col).width(measure, parts);
        let parts = parts.to_vec();
        Self { kind: ChipKind::Run { parts, lead_dot, bold, col }, w }
    }

    /// The one dotted run a text chip is measured and drawn by.
    pub fn dotted(bold: c_int, lead_dot: bool, col: [f32; 4]) -> DottedRun<'static> {
        let run = DottedRun::new(theme::size::CAPTION, col, theme::space::SM).bold(bold);
        if lead_dot {
            run.lead_dot()
        } else {
            run
        }
    }

    /// An outlined badge saying `label`, as wide as [`badge_w`] says.
    pub fn badge(label: String, measure: &dyn plx_machine::machine::Measure) -> Self {
        let w = badge_w(&label, None, measure);
        Self { kind: ChipKind::Badge(label), w }
    }
}

/// How much a chip matters to the fit. They all share one rung: the row is built in order of
/// importance (the item's own facts, the live playback fact, the rating, the premium-audio tag, then
/// CC/SDH/AD), and equal priorities drop the LAST first (`HStack::fit`), so the row sheds from its
/// tail.
pub const CHIP_PRIORITY: u8 = 1;

/// The most chips the row can hold: one per kind it can show — the item's meta run, the live
/// playback fact, the rating, the premium-audio tag, CC, SDH and AD.
pub const MAX_CHIPS: usize = 7;

/// **The chip row: every candidate chip and its [`Item`], in priority order** — the input of
/// `HStack::fit` (issue #26). Fixed storage, because the card draws ~60 times a second and a row
/// of at most [`MAX_CHIPS`] kinds needs no heap of its own. Host-testable on purpose: no `Painter`,
/// no `plx_gfx::text::text_width` (that needs a live SDL2_ttf font the host test binary never loads;
/// see `text.rs::text_width`'s own doc on why measurement is the "impure half"). `draw` measures
/// every candidate chip FIRST — meta text (genres/year/duration), then the live playback fact, then
/// the rating/audio/CC/SDH/AD badges, in that fixed priority order (see the call site for why that
/// order) — and builds the row from those widths.
///
/// The spacing is Detail's identity line's: chips stand `space::SM` apart ([`ChipRow::row`]), a badge
/// leaves `space::XS` before the next badge (so a text run → first badge is `SM`, badge → badge
/// `XS`).
///
/// The fit keeps the longest prefix that fits and drops that chip and every lower-priority one
/// behind it, never skipping ahead to try a smaller later chip instead. Skipping ahead would let a
/// narrow low-priority badge bump a wide high-priority one for no reason a viewer could predict,
/// and would make the row's content depend on exactly how much air a drop happened to leave rather
/// than on priority alone — the row is meant to read the same way every time it truncates. The
/// room is `draw`'s `tw`: the action-button column's left edge minus this panel's own text-column
/// gutter, the same right edge the title and synopsis above this row already respect. That is the
/// actual fix for #26 — the CTA disappearing under a long chip list.
pub struct ChipRow {
    chips: [Option<Chip>; MAX_CHIPS],
    items: [Item; MAX_CHIPS],
    n: usize,
}

impl ChipRow {
    pub fn new() -> Self {
        Self { chips: [const { None }; MAX_CHIPS], items: [Item::absent(); MAX_CHIPS], n: 0 }
    }

    /// Append the next-lowest-priority chip.
    pub fn push(&mut self, chip: Chip) {
        debug_assert!(self.n < MAX_CHIPS, "one chip per kind");
        if self.n == MAX_CHIPS {
            return;
        }
        let item = Item::new(chip.w).priority(CHIP_PRIORITY);
        self.items[self.n] = match chip.kind {
            ChipKind::Badge(_) => item.after(theme::space::XS),
            ChipKind::Run { .. } => item,
        };
        self.chips[self.n] = Some(chip);
        self.n += 1;
    }

    /// The chips as one run from the text column's left edge `tx`, `space::SM` between neighbours.
    pub fn row(&self, tx: f32) -> HStack<'_> {
        HStack::new(&self.items[..self.n], theme::space::SM, Align::Leading(tx))
    }
}

/// Draw the chips `row` places (its survivors), centred on `my`: each text run through the shared
/// dotted-run idiom (parts in the run's ink, dots in the separator ink), each badge through
/// [`meta_badge`].
pub fn draw_chip_row(
    p: Painter,
    chips: &ChipRow,
    row: HStack<'_>,
    my: f32,
    measure: &dyn plx_machine::machine::Measure,
) {
    for placed in row.placed() {
        let Some(chip) = chips.chips[placed.i].as_ref() else { continue };
        match &chip.kind {
            ChipKind::Run { parts, lead_dot, bold, col } => {
                let y = plx_gfx::text::text_vcenter_y(theme::size::CAPTION, *bold, my);
                Chip::dotted(*bold, *lead_dot, *col).measured(measure).draw(p, parts, placed.x, y);
            }
            ChipKind::Badge(label) => {
                meta_badge(p, placed.x, my, label, measure);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{draw_chip_row, Chip, ChipKind, ChipRow};
    use plx_ui::fixture::FixtureMeasure;
    use plx_ui::theme::space;

    /// A text chip / badge chip of a given width, to grade the row's spacing and cut without a font.
    fn text(w: f32) -> Chip {
        Chip { kind: ChipKind::Run { parts: vec![], lead_dot: false, bold: 0, col: plx_ui::theme::TEXT_PRIMARY }, w }
    }
    fn badge(w: f32) -> Chip {
        Chip { kind: ChipKind::Badge(String::new()), w }
    }

    /// The row of `chips`, in priority order.
    fn row_of(chips: impl IntoIterator<Item = Chip>) -> ChipRow {
        let mut row = ChipRow::new();
        for chip in chips {
            row.push(chip);
        }
        row
    }

    /// How many of `chips` the row keeps in `room` px (what `draw` computes as `n`).
    fn kept_in(chips: impl IntoIterator<Item = Chip>, room: f32) -> usize {
        let chips = row_of(chips);
        let row = chips.row(0.0);
        row.kept(row.fit(room)).placed().count()
    }

    /// The row's spacing rule, checked directly rather than only through the cut: a text run → first
    /// badge is `space::SM`, a badge → badge is `space::XS` (the Detail identity line's own gaps —
    /// no raw 18 / 12), and a run → run is `SM` too (the fact run carries its own leading dot).
    #[test]
    fn the_rows_gaps_are_the_identity_lines_tokens() {
        let chips = row_of([text(100.0), text(50.0), badge(60.0), badge(70.0), badge(80.0)]);
        let spans: Vec<_> = chips.row(462.0).placed().collect();
        let gap = |i: usize| spans[i + 1].x - (spans[i].x + spans[i].w);
        assert_eq!(spans[0].x, 462.0);
        assert_eq!([gap(0), gap(1), gap(2), gap(3)], [space::SM, space::SM, space::XS, space::XS]);
    }

    /// **Issue #26, reproduced.** The exact geometry `draw` computes for a real card: `bx` (the
    /// action column's left edge) at 1460, this panel's own 34px gutter in front of it, and the
    /// text column starting (`tx`) at 462 — so `avail_w` below is the literal `tw` a real frame
    /// would pass to the fit. The chip list is what the issue names: an item with a
    /// meta line, a live playback fact, a rating, a premium audio tag and all three of CC/SDH/AD —
    /// "enough chips/actions... that they overlap the main CTA." Before this fix `draw` had no
    /// bound at all here, so every one of these drew and the row reached roughly 1060px into a
    /// 964px lane — squarely under the action buttons. The two assertions are the guarantee that
    /// replaces that: not everything fits, and whatever DOES fit never crosses into the CTA.
    #[test]
    fn chip_row_never_overlaps_the_action_column_with_a_long_chip_list() {
        let cta_x = 1460.0f32; // `bx` in `draw`
        let gutter = 34.0f32; // `bx - tright` in `draw`
        let text_x = 462.0f32; // `tx` in `draw`
        let avail_w = (cta_x - gutter) - text_x; // == `tw`, 964.0
        assert_eq!(avail_w, 964.0);

        // in this row's fixed priority order — meta, fact, rating, audio, CC, SDH, AD — mirroring
        // real measured widths at `size::CAPTION`.
        let chips = row_of([
            text(480.0),  // "Action · Adventure · 2019 · 1h 32m"
            text(300.0),  // "· Converting · HEVC"
            badge(90.0),  // rating badge
            badge(140.0), // audio badge ("Dolby Atmos")
            badge(60.0),  // CC
            badge(74.0),  // SDH
            badge(60.0),  // AD
        ]);
        let all = chips.row(text_x);
        assert!(all.width() > avail_w, "this list must not all fit in {avail_w}px — that is the bug being reproduced");
        let row = all.kept(all.fit(avail_w));
        assert!(row.placed().count() < 7);
        let end = row.placed().last().map_or(text_x, |s| s.x + s.w);
        assert!(end - text_x <= avail_w, "drawn chips must never reach the action column");
    }

    /// The packer never skips ahead to a smaller lower-priority chip once a higher-priority one
    /// does not fit — dropping stops the row at that chip, full stop. Chip 1 alone leaves no room
    /// for chip 2, even though chip 2 would fit on its own in the space chip 1 wanted.
    #[test]
    fn the_fit_drops_the_tail_rather_than_skipping_ahead() {
        let chips = [text(50.0), text(60.0), text(10.0)];
        // room 100: chip 0 (50) fits, chip 1 needs SM + 60 more (126 total, too much) — stop.
        // chip 2 alone would fit in the remaining 50px, but it is never reached.
        assert_eq!(kept_in(chips, 100.0), 1);
    }

    /// A single chip wider than the whole lane draws nothing rather than spilling past the room —
    /// there is no partial chip.
    #[test]
    fn a_single_oversized_chip_is_dropped_whole() {
        assert_eq!(kept_in([text(2000.0)], 964.0), 0);
    }

    /// Exact-fit boundary: a chip that lands EXACTLY on the room is kept, not dropped.
    #[test]
    fn an_exact_fit_is_kept() {
        assert_eq!(kept_in([text(100.0), text(50.0)], 150.0 + space::SM), 2);
        assert_eq!(kept_in([text(100.0), text(50.0)], 150.0 + space::SM - 1.0), 1);
    }

    /// No candidates, or no room at all — both are 0, not a panic.
    #[test]
    fn empty_or_zero_width_is_zero_chips() {
        assert_eq!(kept_in([], 964.0), 0);
        assert_eq!(kept_in([text(10.0)], 0.0), 0);
        assert_eq!(kept_in([text(10.0)], -5.0), 0);
    }

    /// **Q3: the info card's metadata line is spaced exactly like Detail's identity line** — read off
    /// the PAINTED runs. Each part and each dot stands one `space::SM` from its neighbour (the
    /// measured advance of the one before it), the dot is regular weight in the separator ink, and
    /// the fact run continues the meta run through one more dot with the same pad; then the first
    /// badge is `SM` after the text and the next badge `XS` after that. The runs stand at the
    /// caller's `x`, so a baked "   ·   " (or a raw 18 / 12) cannot hide.
    #[test]
    fn the_meta_line_is_spaced_like_the_identity_line() {
        use plx_machine::machine::Measure as _;
        let m = FixtureMeasure;
        let sz = plx_ui::theme::size::CAPTION;
        let w = |s: &str, bold: i32| m.width_str(s, sz, bold != 0);
        let ink = plx_ui::theme::TEXT_PRIMARY;
        let meta = Chip::run(&["Action".into(), "2019".into(), "1h 32m".into()], false, 1, ink, &m);
        let fact = Chip::run(&["Direct Play".into()], true, 0, plx_ui::theme::TEXT_TERTIARY, &m);
        let chips = row_of([meta, fact, Chip::badge("PG-13".into(), &m), Chip::badge("CC".into(), &m)]);
        let row = chips.row(462.0);
        let draw = || draw_chip_row(plx_ui::Painter::recording(), &chips, row, 600.0, &m);
        let runs = plx_ui::draw_census::capture_runs(draw);
        let rrects: Vec<_> = plx_ui::draw_census::capture(draw).into_iter().filter(|(tag, _)| *tag == 2).map(|(_, r)| r).collect();
        let texts: Vec<_> = runs.iter().map(|r| (r.text.clone(), r.at.x, r.bold, r.ink)).collect();
        let words: Vec<&str> = texts.iter().map(|(t, ..)| t.as_str()).collect();
        const DOT: &str = "\u{b7}";
        assert_eq!(words, ["Action", DOT, "2019", DOT, "1h 32m", DOT, "Direct Play", "PG-13", "CC"]);
        // each text run begins one SM after the advance of the one before it, in the row's one pad
        for pair in texts[..7].windows(2) {
            let advance = w(&pair[0].0, i32::from(pair[0].2));
            assert!((pair[1].1 - (pair[0].1 + advance) - space::SM).abs() < 0.5, "{pair:?}");
        }
        assert_eq!(texts[0].1, 462.0);
        // the dots are regular weight in the separator ink; the meta parts are bold
        for (text, _, bold, ink) in &texts[..7] {
            if text == DOT {
                assert!(!bold && *ink == plx_ui::theme::TEXT_SEPARATOR, "the dot idiom");
            }
        }
        assert!(texts[0].2 && !texts[6].2, "the item's facts are bold, the playback fact regular");
        // badges: the keyline rect of the first one stands SM after the last text run's advance, the
        // second XS after the first badge
        let keylines: Vec<_> = rrects.chunks(2).map(|c| c[0]).collect();
        assert_eq!(keylines.len(), 2);
        let last = &texts[6];
        assert!((keylines[0].x - (last.1 + w(&last.0, 0)) - space::SM).abs() < 0.5);
        assert!((keylines[1].x - (keylines[0].x + keylines[0].w) - space::XS).abs() < 0.5);
    }

    /// One hidden-item case: a lone part draws no dot at all, and a fact run with no meta run in front
    /// of it leads with none — no stray separator at the head of the row.
    #[test]
    fn a_lone_part_or_a_fact_without_a_meta_run_draws_no_dot() {
        use plx_machine::machine::Measure as _;
        let m = FixtureMeasure;
        let sz = plx_ui::theme::size::CAPTION;
        let ink = plx_ui::theme::TEXT_PRIMARY;
        let lone = Chip::run(&["2019".into()], false, 1, ink, &m);
        assert_eq!(lone.w, m.width_str("2019", sz, true), "one part: its width, no dot, no gap");
        let chips = row_of([lone, Chip::run(&["Direct Play".into()], false, 0, ink, &m)]);
        let runs = plx_ui::draw_census::capture_runs(|| {
            draw_chip_row(plx_ui::Painter::recording(), &chips, chips.row(0.0), 0.0, &m);
        });
        assert_eq!(runs.iter().map(|r| r.text.as_str()).collect::<Vec<_>>(), ["2019", "Direct Play"]);
        let first = m.width_str("2019", sz, true);
        assert!((runs[1].at.x - (runs[0].at.x + first) - space::SM).abs() < 0.5, "two runs stand one SM apart");
    }

    /// One slot-free fit case through the drawn row: dropping the tail removes its runs from the
    /// paint, not just from the count.
    #[test]
    fn a_dropped_chip_is_not_painted() {
        let m = FixtureMeasure;
        let chips = row_of([text(100.0), Chip::badge("CC".into(), &m)]);
        let all = chips.row(0.0);
        let row = all.kept(all.fit(100.0));
        let log = plx_ui::draw_census::capture(|| draw_chip_row(plx_ui::Painter::recording(), &chips, row, 0.0, &m));
        assert!(log.iter().all(|(tag, _)| *tag != 2), "the badge was dropped, so no rrect is drawn");
    }
}
