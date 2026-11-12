//! **The viewer's colour pick on an AUTHORED ASS/SSA header** (pure): pure white is the default ink.
//!
//! A text subtitle's header is synthesized (`text_ass::header`) and carries the pick as its primary
//! colour. A track the file's author styled (an embedded ASS/SSA track, an ASS sidecar, a SubRip file
//! the server or a tool converted to ASS) arrives with its own `[V4+ Styles]`, and a converted
//! SubRip file is exactly the case the pick has to reach: its one style is plain white. The rule is
//! the same one the SubRip `<font color="#ffffff">` tag follows (`text_ass`): an author who wrote
//! pure white wrote the default ink, not a colour choice, so the viewer's colour pick takes it.
//!
//! [`recolour`] rewrites the `PrimaryColour` of every style whose primary is exactly white (alpha 0,
//! RGB `FFFFFF`, whichever spelling: `&H00FFFFFF`, `&HFFFFFF`, `&H00ffffff&`, `$00FFFFFF`, or the SSA
//! decimal `16777215`) to the pick. A style with any other primary colour, every inline `\c` / `\1c`
//! override (an explicit white one included), the outline and shadow colours and everything else in
//! the header are left byte for byte. `SecondaryColour` is karaoke's pre-highlight colour: it follows
//! only when it is pure white too, so a style whose sweep runs between two different colours keeps
//! the colour it sweeps from, and one whose two are both white (the sweep invisible) stays one ink.
//!
//! [`takes_colour`] is the same test as a fact: does the header carry a style the pick would reach?
//! The track classification publishes it so the Style page can say when the pick cannot show.
//!
//! The one place the result reaches libass is the track's source (`ass_source` for an embedded
//! track, `sidecar` for a script): it holds the file's own bytes and the recoloured copy the renderer
//! is given, and a change of pick swaps the track in place through the restyle path a text track
//! uses (`ass::restyles`). Greys are a draw-time tint and never reach this module
//! ([`text_style::header_tone`] normalizes them to white, which changes nothing here).

use super::text_style::primary_colour;
use plx_plex::plex::session::SubtitleTone;

/// One style line's colour fields, as byte ranges of the header.
struct StyleColours {
    primary: (usize, usize),
    secondary: Option<(usize, usize)>,
}

/// Is the colour field `field` pure opaque white, in any spelling libass reads?
fn is_white(field: &[u8]) -> bool {
    let field = field.trim_ascii();
    let (digits, radix) = match field {
        [b'&', b'h' | b'H', rest @ ..] => (rest, 16),
        [b'$', rest @ ..] => (rest, 16),
        _ => (field, 10),
    };
    let mut value: u64 = 0;
    let mut any = false;
    for &b in digits {
        let Some(d) = (b as char).to_digit(radix) else { break };
        value = value.saturating_mul(u64::from(radix)).saturating_add(u64::from(d));
        any = true;
    }
    any && value == 0x00FF_FFFF
}

/// The columns a `Format:` line names: (PrimaryColour, SecondaryColour, field count).
fn columns(line: &[u8]) -> Option<(usize, Option<usize>, usize)> {
    let names: Vec<&[u8]> = line.split(|&b| b == b',').map(|n| n.trim_ascii()).collect();
    let at = |want: &str| names.iter().position(|n| n.eq_ignore_ascii_case(want.as_bytes()));
    Some((at("PrimaryColour")?, at("SecondaryColour"), names.len()))
}

/// The colour fields of a `Style:` line starting at `base` (the first byte after the colon):
/// the `count` comma-separated fields, the last one running to the end of the line.
fn style_colours(line: &[u8], base: usize, primary: usize, secondary: Option<usize>, count: usize) -> Option<StyleColours> {
    let mut starts = vec![0usize];
    for (i, &b) in line.iter().enumerate() {
        if b == b',' && starts.len() < count {
            starts.push(i + 1);
        }
    }
    if starts.len() < count {
        return None;
    }
    let span = |field: usize| {
        let from = starts[field];
        let to = starts.get(field + 1).map_or(line.len(), |next| next - 1);
        (base + from, base + to)
    };
    Some(StyleColours { primary: span(primary), secondary: secondary.map(span) })
}

/// Walk the style sections of `header` and call `each` with every style's colour fields.
fn for_each_style(header: &[u8], mut each: impl FnMut(&[u8], StyleColours)) {
    let mut in_styles = false;
    let mut cols: Option<(usize, Option<usize>, usize)> = None;
    let mut at = 0;
    while at < header.len() {
        let end = header[at..].iter().position(|&b| b == b'\n').map_or(header.len(), |n| at + n + 1);
        let line = &header[at..end];
        let text = line.trim_ascii_start();
        let skipped = line.len() - text.len();
        if text.first() == Some(&b'[') {
            let name = text.trim_ascii_end();
            in_styles = [&b"[V4+ Styles]"[..], b"[V4 Styles]", b"[V4++ Styles]"].iter().any(|s| name.eq_ignore_ascii_case(s));
            cols = None;
        } else if in_styles {
            if let Some(rest) = strip_key(text, b"Format:") {
                cols = columns(rest);
            } else if let (Some(rest), Some((p, s, n))) = (strip_key(text, b"Style:"), cols) {
                let base = at + skipped + (text.len() - rest.len());
                if let Some(colours) = style_colours(rest, base, p, s, n) {
                    each(header, colours);
                }
            }
        }
        at = end;
    }
}

fn strip_key<'a>(text: &'a [u8], key: &[u8]) -> Option<&'a [u8]> {
    (text.len() >= key.len() && text[..key.len()].eq_ignore_ascii_case(key)).then(|| &text[key.len()..])
}

/// Does `header` hold a style whose primary colour is pure white: one the viewer's colour pick would
/// take? An image-free track with no such style (every style coloured by its author) shows the pick
/// nowhere.
pub fn takes_colour(header: &[u8]) -> bool {
    let mut found = false;
    for_each_style(header, |h, s| found |= is_white(&h[s.primary.0..s.primary.1]));
    found
}

/// `header` with every white-primary style's colour set to `tone`'s, or `None` when nothing changes
/// (the pick is white or a grey, or no style is white-primary): the caller keeps the file's own bytes.
pub fn recolour(header: &[u8], tone: SubtitleTone) -> Option<Vec<u8>> {
    tone.text_rgb8()?;
    let ink = primary_colour(tone);
    let mut edits: Vec<(usize, usize)> = Vec::new();
    for_each_style(header, |h, s| {
        if !is_white(&h[s.primary.0..s.primary.1]) {
            return;
        }
        edits.push(s.primary);
        if let Some(secondary) = s.secondary.filter(|r| is_white(&h[r.0..r.1])) {
            edits.push(secondary);
        }
    });
    if edits.is_empty() {
        return None;
    }
    let mut out = Vec::with_capacity(header.len() + edits.len() * 4);
    let mut at = 0;
    for (from, to) in edits {
        out.extend_from_slice(&header[at..from]);
        // keep the field's own padding, replace the colour it spelled
        let field = &header[from..to];
        let lead = field.len() - field.trim_ascii_start().len();
        let trail = field.len() - field.trim_ascii_end().len();
        out.extend_from_slice(&field[..lead]);
        out.extend_from_slice(ink.as_bytes());
        out.extend_from_slice(&field[field.len() - trail..]);
        at = to;
    }
    out.extend_from_slice(&header[at..]);
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    const YELLOW: &str = "&H001AE6FF";

    fn script(styles: &[&str]) -> Vec<u8> {
        let mut s = String::from("[Script Info]\nScriptType: v4.00+\n\n[V4+ Styles]\nFormat: Name, Fontname, Fontsize, PrimaryColour, SecondaryColour, OutlineColour, BackColour, Bold, Italic, Underline, StrikeOut, ScaleX, ScaleY, Spacing, Angle, BorderStyle, Outline, Shadow, Alignment, MarginL, MarginR, MarginV, Encoding\n");
        for style in styles {
            s.push_str(style);
            s.push('\n');
        }
        s.push_str("\n[Events]\nFormat: Layer, Start, End, Style, Name, MarginL, MarginR, MarginV, Effect, Text\nDialogue: 0,0:00:01.00,0:00:04.00,Default,,0,0,0,,{\\c&HFFFFFF&}Hello\n");
        s.into_bytes()
    }

    fn style(name: &str, primary: &str, secondary: &str) -> String {
        format!("Style: {name},Arial,48,{primary},{secondary},&H00000000,&H80000000,0,0,0,0,100,100,0,0,1,2,1,2,10,10,10,1")
    }

    fn text(bytes: &[u8]) -> String {
        String::from_utf8(bytes.to_vec()).unwrap()
    }

    #[test]
    fn every_spelling_of_pure_opaque_white_is_the_default_ink() {
        for white in ["&H00FFFFFF", "&H00ffffff", "&HFFFFFF", "&H00FFFFFF&", "&hFFFFFF", "$00FFFFFF", "16777215", " &H00FFFFFF "] {
            assert!(is_white(white.as_bytes()), "{white:?}");
        }
        // not white: another colour, a transparent or half-transparent white, an off-white, nothing at all
        for other in ["&H00FFFFFE", "&H01FFFFFF", "&H80FFFFFF", "&HFFFFFFFF", "&H00FF0000", "16777214", "4294967295", "&H", "", "white"] {
            assert!(!is_white(other.as_bytes()), "{other:?}");
        }
    }

    #[test]
    fn a_white_primary_takes_the_pick_and_every_other_style_is_left_as_it_was() {
        let header = script(&[
            &style("Default", "&H00FFFFFF", "&H000000FF"),
            &style("Blue", "&H00FF0000", "&H00FFFFFF"),
            &style("Sign", "&H00FFFFFF", "&H00FFFFFF"),
            &style("Faded", "&H80FFFFFF", "&H00FFFFFF"),
        ]);
        let out = text(&recolour(&header, SubtitleTone::Yellow).expect("two styles are white"));
        let lines: Vec<&str> = out.lines().filter(|l| l.starts_with("Style:")).collect();
        assert!(lines[0].starts_with(&format!("Style: Default,Arial,48,{YELLOW},&H000000FF,")), "{}", lines[0]);
        assert!(lines[1].starts_with("Style: Blue,Arial,48,&H00FF0000,&H00FFFFFF,"), "a blue primary stays blue, and its white secondary stays: {}", lines[1]);
        assert!(lines[2].starts_with(&format!("Style: Sign,Arial,48,{YELLOW},{YELLOW},")), "both white: one ink: {}", lines[2]);
        assert!(lines[3].starts_with("Style: Faded,Arial,48,&H80FFFFFF,&H00FFFFFF,"), "a translucent white is the author's choice: {}", lines[3]);
        // everything else is byte for byte: outlines, shadows, the events and their inline white override
        let strip = |s: &str| s.lines().filter(|l| !l.starts_with("Style: Default") && !l.starts_with("Style: Sign")).collect::<Vec<_>>().join("\n");
        assert_eq!(strip(&out), strip(&text(&header)));
        assert!(out.contains("Style: Default,Arial,48,&H001AE6FF,&H000000FF,&H00000000,&H80000000,0,0,0,0,100,100,0,0,1,2,1,2,10,10,10,1"));
        assert!(out.contains("{\\c&HFFFFFF&}Hello"), "an inline override, an explicit white one too, is never touched");
    }

    #[test]
    fn nothing_changes_for_white_a_grey_or_a_track_with_no_white_style() {
        let white = script(&[&style("Default", "&H00FFFFFF", "&H00FFFFFF")]);
        for tone in [SubtitleTone::White, SubtitleTone::Silver, SubtitleTone::Grey, SubtitleTone::Charcoal] {
            assert_eq!(recolour(&white, tone), None, "{tone:?} is a draw-time tint, not a header colour");
        }
        let coloured = script(&[&style("Default", "&H0000FFFF", "&H00FFFFFF")]);
        assert_eq!(recolour(&coloured, SubtitleTone::Yellow), None, "no white primary, nothing for the pick to take");
        assert!(takes_colour(&white) && !takes_colour(&coloured));
    }

    #[test]
    fn the_rewrite_is_idempotent_and_the_file_is_never_what_is_rewritten_twice() {
        let header = script(&[&style("Default", "&H00FFFFFF", "&H00FFFFFF"), &style("Alt", "&H00FFFFFF", "&H0000FF00")]);
        let once = recolour(&header, SubtitleTone::Yellow).unwrap();
        assert_eq!(recolour(&once, SubtitleTone::Yellow), None, "yellow is not white: a second pass finds nothing");
        assert!(!takes_colour(&once), "the pick has been taken");
    }

    #[test]
    fn ssa_v4_styles_decimal_colours_crlf_and_padding_are_read_and_kept() {
        let ssa = "[Script Info]\r\nScriptType: v4.00\r\n\r\n[V4 Styles]\r\nFormat: Name, Fontname, Fontsize, PrimaryColour, SecondaryColour, TertiaryColour, BackColour, Bold\r\nStyle: Default, Arial, 20, 16777215, 255, 0, 0, 0\r\nStyle: Red,Arial,20,255,255,0,0,0\r\n\r\n[Events]\r\nDialogue: Marked=0,0:00:01.00,0:00:02.00,Default,,0,0,0,,Hi\r\n";
        let out = text(&recolour(ssa.as_bytes(), SubtitleTone::Yellow).unwrap());
        assert!(out.contains("Style: Default, Arial, 20, &H001AE6FF, 255, 0, 0, 0\r\n"), "{out:?}");
        assert!(out.contains("Style: Red,Arial,20,255,255,0,0,0\r\n"));
        assert_eq!(out.replace("&H001AE6FF", "16777215"), ssa);
    }

    #[test]
    fn only_a_styles_section_is_read() {
        // a Format/Style look-alike in the events, and a header with no styles section at all
        let events = "[Script Info]\n\n[Events]\nFormat: Name, PrimaryColour\nStyle: Default,&H00FFFFFF\n";
        assert!(!takes_colour(events.as_bytes()));
        assert_eq!(recolour(b"", SubtitleTone::Yellow), None);
        assert_eq!(recolour(b"[V4+ Styles]\nStyle: Default,Arial\n", SubtitleTone::Yellow), None, "a style line before any Format line has no columns");
    }
}
