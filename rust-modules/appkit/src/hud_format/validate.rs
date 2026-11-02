//! **The package-level invariants that need no geometry and no state graph** (`docs/hud-plugins.md`,
//! "Invariants"), over a parsed [`Package`]: I3 identity, I9's palette rule and I11's face ladder.
//! [`super::reach::check`] runs them after the reachability rules, so a caller has one entry point.
//!
//! **What is NOT here, because [`super::parse`] already enforces it** (a second check would be a second
//! copy of a rule, and a package that fails it never becomes a [`Package`]):
//! * I8, a control's rest and focus faces differ: `schema`'s `REST_FOCUS_RULES` refuse `rest: "solid"`
//!   with the default or stated `accent` focus (one face drawn twice) and require `focus` beside `solid`;
//!   every other pair differs by construction. `E0106` / `E0103` at the control or the Up Next button.
//! * I9's sizes: a size is a rung name (`E0201`) and `HUD_TITLE` is legal only on a text bound to
//!   `title.main` (`E0106`); the weights are one boolean.
//! * I11's `default`: a `seek {by: "setting"}` face map without it lacks a required state (`E0303`).
//! * I13: `layout.anchor` is a one-name enum (`E0201`).
//! * R4's two buttons and the countdown: required keys of `layout.up_next` (`E0103`).
//!
//! Pure: no IO, no globals.

use std::collections::BTreeMap;

use plx_media::route::SkipInterval;

use super::parse::{Object, Package, Val};
use super::schema::BUILTIN_CLASSIC_ID;
use super::{codes, pointer_push, Finding};

/// Every invariant of this module, over `package`, in the order: identity, palette, face ladder.
pub fn check(package: &Package) -> Vec<Finding> {
    let mut out = Vec::new();
    identity(&package.layout, &mut out);
    builtin_has_no_palette(package, &mut out);
    face_ladders(&package.layout, &mut out);
    playbar_chrome(&package.layout, &mut out);
    card_content(&package.layout, false, &mut out);
    out
}

/// `root` and every object under it, parents before children, in document order.
pub(super) fn each_object<'a>(root: &'a Object, f: &mut impl FnMut(&'a Object)) {
    f(root);
    for (_, value) in &root.fields {
        each_object_in(value, f);
    }
}

fn each_object_in<'a>(value: &'a Val, f: &mut impl FnMut(&'a Object)) {
    match value {
        Val::Object(object) => each_object(object, f),
        Val::List(items) => items.iter().for_each(|item| each_object_in(item, f)),
        _ => {}
    }
}

/// I3. A band id, a segment id and a control id are each unique among their own kind: a band and a
/// segment may share a name (`up_next.band` and `up_next.replaces` say which they mean, and classic's tabs
/// band holds a segment of the same name), a control and a band may too (the reference's `info`).
fn identity(layout: &Object, out: &mut Vec<Finding>) {
    let mut seen: BTreeMap<(&'static str, &str), &str> = BTreeMap::new();
    each_object(layout, &mut |object| {
        let kind = match object.node {
            "band" => "band",
            "segment" => "segment",
            "item" => "control",
            _ => return,
        };
        let Some(id) = object.get("id").and_then(Val::as_str) else {
            return;
        };
        let at = pointer_push(&object.pointer, "id");
        match seen.get(&(kind, id)) {
            None => {
                seen.insert((kind, id), &object.pointer);
            }
            Some(first) => out.push(Finding::new(
                &codes::E0901,
                at,
                format!("the {kind} id `{id}` is already used by the {kind} at {first}"),
                format!("give every band, segment and control an id of its own; rename this {kind}"),
            )),
        }
    });
}

/// I9. The built-in declares no palette: a palette is user data compiled into an ink, and the built-in
/// writes none (D12).
fn builtin_has_no_palette(package: &Package, out: &mut Vec<Finding>) {
    if package.id == BUILTIN_CLASSIC_ID && !package.palette.is_empty() {
        out.push(Finding::new(
            &codes::E0902,
            "/palette",
            format!("`{BUILTIN_CLASSIC_ID}` declares no palette"),
            "ink the built-in with theme tokens only, and delete \"palette\"",
        ));
    }
}

/// I11. A `by_interval` map answers for the viewer's Skip interval; a rung of that ladder it does not
/// list draws `default`. A warning, so a rung the app adds later never breaks a published package.
fn face_ladders(layout: &Object, out: &mut Vec<Finding>) {
    each_object(layout, &mut |object| {
        if object.node != "item" {
            return;
        }
        let setting = object.get("command").and_then(Val::as_str) == Some("seek")
            && object.get("by").and_then(Val::as_str) == Some("setting");
        let Some(Val::Object(map)) = object.get("face").filter(|_| setting) else {
            return;
        };
        let Some(Val::Object(ladder)) = map.get("by_interval") else {
            return;
        };
        let missing: Vec<String> = SkipInterval::LADDER
            .iter()
            .map(|rung| rung.seconds())
            .filter(|seconds| {
                !ladder
                    .fields
                    .iter()
                    .any(|(key, _)| key.parse::<i64>() == Ok(*seconds))
            })
            .map(|seconds| seconds.to_string())
            .collect();
        if !missing.is_empty() {
            out.push(Finding::new(
                &codes::W0903,
                ladder.pointer.clone(),
                format!(
                    "the by_interval face map has no face for the {} s Skip interval",
                    missing.join(", ")
                ),
                format!(
                    "add \"{}\": a face (that interval draws `default` until you do)",
                    missing[0]
                ),
            ));
        }
    });
}

/// A scrub fades transport chrome; its bar and preview must remain visible for the gesture's whole lifetime.
fn playbar_chrome(layout: &Object, out: &mut Vec<Finding>) {
    each_object(layout, &mut |band| {
        if band.node != "band" || band.get("chrome").and_then(Val::as_str) == Some("steady") {
            return;
        }
        let mut has_bar = false;
        each_object(band, &mut |cell| has_bar |= cell.node == "playbar");
        if has_bar {
            out.push(Finding::new(&codes::E0904, pointer_push(&band.pointer, "chrome"),
                "the playbar's band fades during scrub, hiding the bar and its preview while the gesture is active",
                "set this band's chrome to steady; put fading title text and controls in separate bands"));
        }
    });
}

/// A card is the Shelf's one focus stop; a descendant cannot own another focus group or scrub gesture.
fn card_content(object: &Object, inside_card: bool, out: &mut Vec<Finding>) {
    if inside_card && matches!(object.node, "segment" | "actions" | "playbar" | "strip") {
        out.push(Finding::new(&codes::E0905, object.pointer.clone(),
            format!("a chapter card cannot contain `{}`: the Shelf owns the card's focus and seeking action", object.node),
            "place controls, playbars and strips in bands outside the card; card overlay and below cells are decorative"));
        return;
    }
    let inside_card = inside_card || object.node == "card";
    fn visit(value: &Val, inside_card: bool, out: &mut Vec<Finding>) {
        match value {
            Val::Object(object) => card_content(object, inside_card, out),
            Val::List(items) => items.iter().for_each(|v| visit(v, inside_card, out)),
            _ => {}
        }
    }
    for (_, value) in &object.fields { visit(value, inside_card, out); }
}
