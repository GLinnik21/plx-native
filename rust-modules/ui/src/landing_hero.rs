//! Landing-page geometry and shade: Detail's ladder and atmospheric ramp, and Home's ladder,
//! metadata helpers and combined edge and shelf field.

use super::hstack::Item;
use super::vstack::{Anchor, VSpan, VStack};
use super::widgets::{WashRamp, HERO_BASE_SCRIM_Y0};
use super::{consts::SCR_H, theme, Rect};

pub const COL_W: f32 = theme::hero::TEXT_W;
pub const KNEE_Y: f32 = theme::hero::SCRIM_MID_Y;
pub const MID_WEIGHT: f32 = theme::hero::SCRIM_MID_A / theme::hero::SCRIM_FOOT_A;
/// How much a bound preview raises the shade behind the title. Detail's bottom ramp uses its own
/// `preview_base_scrim` strength. 1.35 is the video-bound row the legibility table grades.
///
/// The player publishes the same number as `player::preview::PREVIEW_FIELD` (its `View::field` once
/// a picture is up). `player` may not name `ui` and `ui` may not name `player`, so each owns a copy
/// and `screens::detail`'s `preview_plane_tests` pin them equal.
pub const PREVIEW_FIELD: f32 = 1.35;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct HeroStack {
    pub title: VSpan,
    pub meta: VSpan,
    pub ratings: Option<VSpan>,
    pub synopsis: Option<VSpan>,
    pub facts: VSpan,
    pub actions: VSpan,
}

/// Detail's measured ladder, pinned at the foot of its title (`title_bottom`) and growing down from
/// there. Identity and ratings form a tight group; prose and facts get block air, and facts stand
/// close to their action. Absent rows leave no phantom gap. Home does not stack these rows (it has
/// [`home_rows`]).
pub fn hero_rows(title_h: f32, meta_h: f32, ratings_h: Option<f32>, synopsis_h: Option<f32>, title_bottom: f32) -> HeroStack {
    let items = [
        Item::new(title_h),
        Item::new(meta_h).before(theme::space::SM),
        ratings_h.map_or(Item::absent(), |h| Item::new(h).before(theme::space::XS)),
        synopsis_h.map_or(Item::absent(), |h| Item::new(h).before(theme::space::LG)),
        // TextView measures full leading boxes. Their trailing air belongs to the prose;
        // the proposal spends the larger block gap above it, not a second time below it.
        Item::new(theme::hero::FACTS_H).before(if synopsis_h.is_some() { theme::space::MD } else { theme::space::LG }),
        Item::new(0.0).before(theme::space::MD),
    ];
    let stack = VStack::new(&items, 0.0, Anchor::Top(title_bottom - title_h));
    let span = |i| stack.span(i).map(|(y, h)| VSpan { i, y, h, box_y: y });
    HeroStack {
        title: span(0).unwrap(), meta: span(1).unwrap(), ratings: span(2),
        synopsis: span(3), facts: span(4).unwrap(), actions: span(5).unwrap(),
    }
}

/// Home's hero ladder (owner's board CW-13 v2): the title, ONE block under it, and the action row,
/// all growing up from [`theme::hero::HOME_ACTIONS_Y`]. Home owns its ladder because it dropped the
/// identity line, the ratings row and the facts line that [`hero_rows`] stacks; Detail keeps those
/// and its own anchor, and this function never moves it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct HomeStack {
    pub title: VSpan,
    /// An episode's one-line label, or the movie's / show's synopsis (at most
    /// [`theme::hero::HOME_SYNOPSIS_LINES`] lines); `None` when the item has neither.
    pub block: Option<VSpan>,
    /// A zero-height span at the action row's top ([`theme::hero::HOME_ACTIONS_Y`]).
    pub actions: VSpan,
}

/// The height of Home's one-line episode label: the synopsis' own leading, so a one-line synopsis
/// and an episode label stand on the same line box.
pub const HOME_LABEL_H: f32 = crate::HERO_SYN_LEAD;

/// Home's ladder for a title `title_h` tall over a block `block_h` tall (`None`: no block). The
/// block's foot is [`theme::hero::HOME_ACTIONS_GAP`] above the actions and the title's foot is
/// [`theme::hero::HOME_TITLE_GAP`] above the block; an absent block leaves no phantom gap, so the
/// title then stands the 30 px above the actions itself.
pub fn home_rows(title_h: f32, block_h: Option<f32>) -> HomeStack {
    let items = [
        Item::new(title_h),
        block_h.map_or(Item::absent(), |h| Item::new(h).before(theme::hero::HOME_TITLE_GAP)),
        Item::new(0.0).before(theme::hero::HOME_ACTIONS_GAP),
    ];
    let stack = VStack::new(&items, 0.0, Anchor::Bottom(theme::hero::HOME_ACTIONS_Y));
    let span = |i| stack.span(i).map(|(y, h)| VSpan { i, y, h, box_y: y });
    HomeStack { title: span(0).unwrap(), block: span(1), actions: span(2).unwrap() }
}

/// Home's reading copy: the shared reading text ([`crate::hero_synopsis`]'s `LABEL` 26 on a 36 px
/// leading in `TEXT_READING`) cut to [`theme::hero::HOME_SYNOPSIS_LINES`] lines. What Home sets in it
/// is a hero's premise ([`first_sentence`] of its summary), not the whole synopsis, which stays on
/// Details. Draw it at [`theme::hero::HOME_SYNOPSIS_W`] wide.
pub fn home_synopsis(summary: &str) -> crate::text_view::TextView<'_> {
    crate::hero_synopsis(summary, "").max_lines(theme::hero::HOME_SYNOPSIS_LINES)
}

/// The top of the tiles of the shelf under Home's hero at rest ([`crate::consts::PEEK_Y`] and the
/// card's own drop): 768, where [`shelf_fade`] begins until the page scrolls.
pub const SHELF_TOP: f32 = crate::consts::PEEK_Y + crate::consts::CARD_DY;

/// **Home's shelf fade** (owner's board CW-13 v2): the hero photograph, darkened across the WHOLE
/// width of the shelf under it so it reaches near-black at the Continue Watching row's caption line.
/// The captions stand straight on the backdrop with no plate behind them, so whatever bright thing
/// the photograph holds behind them (a rock behind a progress bar, behind "5m left") has to be gone
/// there, on every card, at every width, with no block to place behind each one.
///
/// A full-width vertical ramp of [`theme::SCRIM_INK`], the same [`WashRamp`] the ground has always
/// carried per vertex in its one pass (`AmbientWash::draw_ground`), in two segments: nothing at the
/// tiles' top ([`SHELF_TOP`]), [`theme::shade::SHELF_MID_A`] at the tiles' mid-line (an ease in, so the
/// start has no visible edge), and [`theme::shade::SHELF_FOOT_A`] at the caption line's top
/// ([`crate::still_footer::Place::Below`], 1012), held from there to the panel's foot. It is the hero's,
/// so it fades with the hero (`hero_a`, which is 0 once the page has scrolled to the shelves) and
/// never touches Detail, which has no such shelf. `tiles_top` is the live top of the first shelf's
/// tiles ([`SHELF_TOP`] at rest), so the fade rides the page as the dive moves the shelf. [`home_shade`] joins it
/// with the side shade before either reaches the renderer.
pub fn shelf_fade(tiles_top: f32, hero_a: f32) -> WashRamp {
    let k = hero_a.clamp(0.0, 1.0);
    let tiles = Rect::new(0.0, tiles_top, crate::consts::SCR_W, crate::card_row::RowStyle::STILL.h);
    WashRamp {
        ink: theme::scrim(1.0),
        stops: [
            (tiles.y, 0.0),
            (tiles.y + tiles.h * 0.5, theme::shade::SHELF_MID_A * k),
            (crate::still_footer::Place::Below.line_top(tiles), theme::shade::SHELF_FOOT_A * k),
        ],
    }
}

/// Home's fixed left shade and live shelf ramp use a single ink field, across both carousel art
/// layers when the hero slides. The field stays put while title and controls move across it.
pub fn home_shade(shelf: WashRamp, hero_a: f32) -> crate::widgets::WashScrim {
    let k = hero_a.clamp(0.0, 1.0);
    edge_wash(shelf, theme::shade::HOME_SIDE_X, theme::shade::HOME_SIDE_Y, None,
        theme::shade::HOME_SIDE_A * k, k)
}

/// Detail's continuous edge shade. During the logo's move the left fade gives way to a shallow
/// full-width top fade, and the bottom ramp moves down to the remaining copy.
pub fn detail_shade(visible: f32, preview_field: f32, preview_base: f32, logo_progress: f32) -> crate::widgets::WashScrim {
    let base = (visible * preview_base).clamp(0.0, 1.0);
    let field = (visible * preview_field).clamp(0.0, PREVIEW_FIELD);
    let [top, knee, mid, foot] = base_scrim_ramp(base);
    let t = logo_progress.clamp(0.0, 1.0);
    let lerp = |from, to| from + (to - from) * t;
    let [preview_top, preview_knee] = theme::shade::DETAIL_PREVIEW_FOOT_Y;
    let (top, knee) = (lerp(top, preview_top), lerp(knee, preview_knee));
    let ramp = WashRamp { ink: theme::scrim(1.0), stops: [(top, 0.0), (knee, mid), (SCR_H, foot)] };
    let rise = theme::shade::DETAIL_PREVIEW_RISE * t;
    let top_peak = theme::shade::DETAIL_PREVIEW_TOP_A * field * t;
    let top_fade = (top_peak > 0.0).then_some((theme::shade::DETAIL_PREVIEW_TOP_Y, top_peak));
    edge_wash(ramp, theme::shade::DETAIL_SIDE_X,
        theme::shade::DETAIL_SIDE_Y.map(|y| y - rise), top_fade,
        theme::shade::DETAIL_SIDE_A * field * (1.0 - t), base.max(field.min(1.0)))
}

fn edge_wash(ramp: WashRamp, horizontal: [f32; 2], vertical: [f32; 2], top: Option<([f32; 2], f32)>,
    peak: f32, strength: f32) -> crate::widgets::WashScrim {
    use plx_gfx::gfx::scrim::{EdgeShade, GroundShade};
    let field = if strength > 0.0 {
        GroundShade::from_edge(EdgeShade::new(horizontal, vertical, top, ramp.stops,
            peak, theme::shade::EDGE_JOIN * strength))
    } else {
        GroundShade::NONE
    };
    crate::widgets::WashScrim { ink: ramp.ink, field }
}

/// **A hero's premise: the first sentence of its summary** (Home's fallback line when identity and progress do not apply).
/// The sentence ends at the first `.`, `!`, `?` or `…` that is followed by whitespace
/// and a letter that starts one (an uppercase letter, or a letter of a script that has no case), or
/// by the end of the text, and at a CJK full stop `。` `！` `？` wherever it stands. Closing quotes
/// and brackets after the mark, and a run of marks (`...`, `?!`), belong to the sentence. The
/// sentence is returned as a slice of `text` with its end mark, trimmed; the text of no sentence end
/// is returned whole (the caller elides it at its own line count).
///
/// What is NOT an end, because cutting there would break the sentence: a period after a title or
/// another common abbreviation (`Dr.`, `Mr.`, `St.`, `vs.`), after a single initial (`J. K.`), or
/// inside a dotted abbreviation (`U.S.`, `e.g.`, `a.m.`); a mark followed by a lowercase letter, a
/// digit or no space (`7.5`, `No. 5`); and any mark inside an open bracket or quotation. The rule
/// errs toward NOT cutting: a sentence that stays long is elided at two lines, a sentence cut in the
/// middle is wrong.
pub fn first_sentence(text: &str) -> &str {
    let text = text.trim();
    // Bracket and quote depth, so a mark inside `(...)` or a quotation does not end the sentence.
    let (mut depth, mut straight) = (0i32, false);
    let stop = |c: char| matches!(c, '.' | '!' | '?' | '\u{2026}' | '\u{3002}' | '\u{ff01}' | '\u{ff1f}');
    let cjk = |c: char| matches!(c, '\u{3002}' | '\u{ff01}' | '\u{ff1f}');
    let closer = |c: char| matches!(c, '"' | '\'' | '\u{201d}' | '\u{2019}' | '\u{bb}' | ')' | ']' | '\u{300d}' | '\u{300f}' | '\u{ff09}' | '\u{3011}');
    let opener = |c: char| matches!(c, '"' | '\'' | '\u{201c}' | '\u{2018}' | '\u{ab}' | '(' | '[' | '\u{bf}' | '\u{a1}' | '\u{300c}' | '\u{300e}' | '\u{ff08}' | '\u{3010}');
    for (i, c) in text.char_indices() {
        match c {
            '(' | '[' | '\u{201c}' | '\u{ab}' | '\u{300c}' | '\u{300e}' | '\u{ff08}' | '\u{3010}' => depth += 1,
            ')' | ']' | '\u{201d}' | '\u{bb}' | '\u{300d}' | '\u{300f}' | '\u{ff09}' | '\u{3011}' => depth = (depth - 1).max(0),
            '"' => straight = !straight,
            _ => {}
        }
        if !stop(c) || depth > 0 || straight {
            continue;
        }
        // the whole run of marks and closing quotes after this one is part of the sentence
        let end = i + text[i..].find(|c: char| !(stop(c) || closer(c))).unwrap_or(text.len() - i);
        if cjk(c) {
            return &text[..end];
        }
        let rest = &text[end..];
        let starts_one = |s: &str| {
            let s = s.trim_start_matches(opener);
            s.chars().next().is_some_and(|c| c.is_alphabetic() && !c.is_lowercase())
        };
        let boundary = rest.is_empty() || (rest.starts_with(char::is_whitespace) && starts_one(rest.trim_start()));
        // a single `.` can be an abbreviation's own; a run (`...`) or any other mark cannot
        let single = c == '.' && !text[..i].ends_with(stop) && !text[i + c.len_utf8()..].starts_with(stop);
        if boundary && !(single && ends_in_abbreviation(&text[..i])) {
            return &text[..end];
        }
    }
    text
}

/// Does `before`, the text up to a period, end in an abbreviation or an initial that period belongs to?
fn ends_in_abbreviation(before: &str) -> bool {
    /// Titles and common abbreviations that are followed by a name or a noun (lowercase, no period).
    const ABBREVIATIONS: &[&str] = &[
        "mr", "mrs", "ms", "mx", "dr", "prof", "sr", "jr", "st", "mt", "ft", "gen", "col", "capt", "cpt", "lt", "sgt", "cmdr",
        "maj", "adm", "gov", "sen", "rep", "rev", "hon", "pres", "vs", "fig", "inc", "ltd", "corp", "mme", "mlle",
    ];
    let word = before.rsplit(char::is_whitespace).next().unwrap_or("");
    let word = word.trim_start_matches(|c: char| !c.is_alphanumeric());
    // `U.S`, `Ph.D`, `e.g`, `a.m`: short runs of letters between periods (a decimal, `7.5`, is not one)
    let dotted = word.contains('.')
        && word.split('.').all(|part| (1..=3).contains(&part.chars().count()) && part.chars().all(char::is_alphabetic));
    dotted
        || (word.chars().count() == 1 && word.chars().all(char::is_alphabetic))
        || ABBREVIATIONS.contains(&word.to_lowercase().as_str())
}

pub fn base_scrim_bottom_a(hero_a: f32) -> f32 { theme::hero::SCRIM_FOOT_A * hero_a.clamp(0.0, 1.0) }

pub fn base_scrim_ramp(hero_a: f32) -> [f32; 4] {
    let foot = base_scrim_bottom_a(hero_a);
    [HERO_BASE_SCRIM_Y0, KNEE_Y, foot * MID_WEIGHT, foot]
}

pub fn base_scrim_a(y: f32, hero_a: f32) -> f32 {
    let foot = base_scrim_bottom_a(hero_a);
    let mid = foot * MID_WEIGHT;
    if y <= HERO_BASE_SCRIM_Y0 { 0.0 }
    else if y < KNEE_Y { mid * (y - HERO_BASE_SCRIM_Y0) / (KNEE_Y - HERO_BASE_SCRIM_Y0) }
    else { mid + (foot - mid) * ((y - KNEE_Y) / (SCR_H - KNEE_Y)).clamp(0.0, 1.0) }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Detail's ladder with no ratings row, the title's foot at 566.
    fn hero_stack(title_h: f32, meta_h: f32, synopsis_h: Option<f32>) -> HeroStack {
        hero_rows(title_h, meta_h, None, synopsis_h, 566.0)
    }

    #[test]
    fn the_hero_text_is_one_ladder_with_an_absent_synopsis() {
        let with = hero_stack(120.0, 34.0, Some(72.0));
        assert_eq!(with.title.y + with.title.h, 566.0);
        assert_eq!(with.meta.y, 582.0);
        assert_eq!(with.synopsis.unwrap().y, 656.0);
        assert_eq!(with.facts.y, 752.0);
        assert_eq!(with.actions.y, 800.0);
        let bare = hero_stack(120.0, 34.0, None);
        assert!(bare.synopsis.is_none());
        assert_eq!(bare.facts.y - (bare.meta.y + bare.meta.h), theme::space::LG);
        assert_eq!(with.facts.y - bare.facts.y, 72.0 + theme::space::MD, "the synopsis block and its air are all an absent one gives back");
    }

    /// The owner's v2 ladder, pixel for pixel: an episode's one-line label tops out at 602 with the
    /// 120 px logo's foot at 586, a two-line synopsis at 566 with the title's foot at 550, a one-line
    /// synopsis comes down a line (and its title with it), and every block ends at 638.
    #[test]
    fn home_ladder_positions() {
        let title_h = theme::logo::HERO_BAND_H;
        let one = home_rows(title_h, Some(HOME_LABEL_H));
        assert_eq!(one.actions.y, 668.0);
        let b = one.block.unwrap();
        assert_eq!((b.y, b.y + b.h), (602.0, 638.0), "the label's line box ends 30 above the actions");
        assert_eq!(one.title.y + one.title.h, 586.0, "the logo's foot is 16 above the label");
        let two = home_rows(title_h, Some(2.0 * HOME_LABEL_H));
        let b = two.block.unwrap();
        assert_eq!((b.y, b.y + b.h), (566.0, 638.0));
        assert_eq!(two.title.y + two.title.h, 550.0);
        assert_eq!(two.title.y + two.title.h, one.title.y + one.title.h - HOME_LABEL_H, "the second line moves the title up by one line");
        let bare = home_rows(title_h, None);
        assert!(bare.block.is_none());
        assert_eq!(bare.title.y + bare.title.h, 638.0, "no block: the title stands the 30 above the actions");
    }

    /// Detail is untouched by Home's ladder: its title stands at 446 over a 566 foot and its action
    /// row at 838, nowhere near Home's.
    #[test]
    fn detail_ladder_is_not_home_ladder() {
        let detail = hero_rows(120.0, 34.0, Some(30.0), Some(72.0), 566.0);
        assert_eq!((detail.title.y, detail.actions.y), (446.0, 838.0));
        assert_ne!(home_rows(120.0, Some(72.0)).actions.y, detail.actions.y);
    }

    #[test]
    fn pdf_synopsis_padding_preserves_the_facts_and_action_anchors() {
        for ratings in [None, Some(30.0)] {
            let rows = hero_rows(120.0, 34.0, ratings, Some(72.0), 566.0);
            let previous = rows.ratings.unwrap_or(rows.meta);
            let synopsis = rows.synopsis.unwrap();
            assert_eq!(synopsis.y - (previous.y + previous.h), 40.0,
                "the proposal gives prose a full block gap above it");
            assert_eq!(rows.facts.y - (synopsis.y + synopsis.h), 24.0,
                "the synopsis line boxes already include trailing leading");
            assert_eq!(rows.actions.y - (rows.facts.y + rows.facts.h), 24.0);
            assert_eq!(rows.title.y + rows.title.h, 566.0);
            assert_eq!(rows.facts.y, if ratings.is_some() { 790.0 } else { 752.0 });
            assert_eq!(rows.actions.y, if ratings.is_some() { 838.0 } else { 800.0 });
        }
    }
}

#[cfg(test)]
#[test]
fn detail_base_shade_keeps_its_three_stops() {
    let origin = HERO_BASE_SCRIM_Y0;
    for strength in [0.0, 0.25, 1.0] {
        for (y, alpha) in [(origin, 0.0), (KNEE_Y, theme::hero::SCRIM_MID_A), (SCR_H, theme::hero::SCRIM_FOOT_A)] {
            let want = alpha * strength;
            assert!((base_scrim_a(y, strength) - want).abs() < 1e-6);
            assert!((crate::detail_layout::base_scrim_a(y, strength) - want).abs() < 1e-6);
        }
    }
    assert_eq!(base_scrim_a(0.0, 1.0), 0.0);
    assert_eq!(base_scrim_a(SCR_H + 100.0, 1.0), theme::hero::SCRIM_FOOT_A);
}

#[cfg(test)]
#[test]
fn pdf_proposal_home_and_detail_use_the_same_column() {
    assert_eq!(COL_W, crate::detail_layout::HERO_TEXT_W);
}

#[cfg(test)]
mod premise_tests {
    use super::first_sentence;

    /// **The first sentence ends at the first `.`, `!`, `?` or `…` followed by a space and a capital**,
    /// keeping its mark (and a run of marks, and the quotes and brackets that close after it).
    #[test]
    fn the_first_sentence_ends_at_the_first_stop_before_a_capital() {
        let cases = [
            ("A lone woman crosses frozen wastes. She finds a dragon. It follows.", "A lone woman crosses frozen wastes."),
            ("Who is she? Nobody knows.", "Who is she?"),
            ("Run! Now.", "Run!"),
            ("She waits\u{2026} Then he falls. End.", "She waits\u{2026}"),
            ("Wait... What now? Nothing.", "Wait..."),
            ("Really?! Yes.", "Really?!"),
            ("Stop. \"Go,\" she said.", "Stop."),
            ("One sentence only.", "One sentence only."),
            ("  Padded.  Two.  ", "Padded."),
        ];
        for (text, want) in cases {
            assert_eq!(first_sentence(text), want, "{text:?}");
        }
    }

    /// **A mark that is not a sentence end is not cut on**: abbreviations, initials, dotted
    /// abbreviations, a lowercase or digit after the mark, no space after it, and a mark inside an
    /// open bracket or quotation. (A decimal is none of these: `3.5` is not an abbreviation.)
    #[test]
    fn a_mark_that_does_not_end_a_sentence_is_not_cut_on() {
        let cases = [
            ("Dr. Jones searches for the idol. He finds it.", "Dr. Jones searches for the idol."),
            ("Mr. and Mrs. Smith hunt each other. Then they marry.", "Mr. and Mrs. Smith hunt each other."),
            ("A tour of St. Louis begins. It ends.", "A tour of St. Louis begins."),
            ("Prof. Xavier and Capt. America meet. Tea follows.", "Prof. Xavier and Capt. America meet."),
            ("J. K. Rowling reads aloud. The crowd listens.", "J. K. Rowling reads aloud."),
            ("U.S. agents chase a thief. He escapes.", "U.S. agents chase a thief."),
            ("They meet at 9 a.m. in Paris. Then it rains.", "They meet at 9 a.m. in Paris."),
            ("Pets (cats, dogs, etc.) run wild. The end.", "Pets (cats, dogs, etc.) run wild."),
            ("Rated 7.5 by critics. More follow.", "Rated 7.5 by critics."),
            ("Rated 3.5. Then it fell.", "Rated 3.5."),
            ("It is No. 5 on the list. Fine.", "It is No. 5 on the list."),
            ("Nothing here e.g. a cat.", "Nothing here e.g. a cat."),
            ("He said (loudly. Very.) and left. Done.", "He said (loudly. Very.) and left."),
            ("\u{201c}Run. Now.\u{201d} she said. Fine.", "\u{201c}Run. Now.\u{201d} she said."),
            ("Visit example.com for more. Thanks.", "Visit example.com for more."),
            ("It ends. then it goes on. Fine.", "It ends. then it goes on."),
            ("It was called \"Sintel.\" Nobody knew why.", "It was called \"Sintel.\" Nobody knew why."),
        ];
        for (text, want) in cases {
            assert_eq!(first_sentence(text), want, "{text:?}");
        }
    }

    /// **CJK full stops end a sentence wherever they stand**, but not inside a quotation; a caseless
    /// script's sentences end before a letter that has no case, as a cased script's end before a capital.
    #[test]
    fn cjk_full_stops_and_caseless_scripts_end_a_sentence() {
        let cases = [
            ("一个年轻的女人穿越荒原。她找到了龙。", "一个年轻的女人穿越荒原。"),
            ("你是谁？没有人知道。", "你是谁？"),
            ("走！现在。", "走！"),
            ("「行こう。」と言った。次の文。", "「行こう。」と言った。"),
            ("한 여자가 얼음의 세계를 찾는다. 그녀는 용을 찾는다.", "한 여자가 얼음의 세계를 찾는다."),
            ("Привет. Как дела?", "Привет."),
        ];
        for (text, want) in cases {
            assert_eq!(first_sentence(text), want, "{text:?}");
        }
    }

    /// With no sentence end the whole text comes back (the caller elides it at its two lines), empty
    /// and blank text have no sentence, and the answer is always a prefix of the trimmed text.
    #[test]
    fn with_no_end_the_whole_text_comes_back() {
        assert_eq!(first_sentence("A summary with no end mark at all"), "A summary with no end mark at all");
        assert_eq!(first_sentence("It trails off, mid-thought and then more words"), "It trails off, mid-thought and then more words");
        assert_eq!(first_sentence(""), "");
        assert_eq!(first_sentence("   \n "), "");
        assert_eq!(first_sentence("."), ".");
        assert_eq!(first_sentence("Dr."), "Dr.");
        assert_eq!(first_sentence("Line one.\nLine two."), "Line one.", "a newline is whitespace");
        for text in ["Fin.", "Ok! Ok?", "。", "a. B", "\"unclosed. Next. Then", "Line one.\nLine two."] {
            let got = first_sentence(text);
            assert!(text.trim().starts_with(got) && !got.is_empty(), "{text:?} -> {got:?}");
        }
    }
}
