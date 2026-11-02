//! Host/TV registry instruments. Serialization is shared; file reads/writes are the outer tool shell.
//! No worker, machine state, playback request, or per-frame instrumentation is introduced.

use plx_appkit::hud_format::lint::{text_metrics, LintLang};
#[cfg(any(test, feature = "hostsim"))]
use plx_appkit::hud_format::schema;
use plx_machine::machine::Measure;
use serde_json::{json, Value};
use std::collections::BTreeMap;

/// Font identity is part of a text-width measurement, including every installed fallback face.
pub(crate) fn font_hashes() -> Result<BTreeMap<String, String>, String> {
    let path = plx_gfx::text::app_font_path(false);
    let directory = path.parent().ok_or("font has no parent directory")?;
    let mut hashes = BTreeMap::new();
    for item in std::fs::read_dir(directory).map_err(|e| e.to_string())? {
        let path = item.map_err(|e| e.to_string())?.path();
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        if name.starts_with("appfont") && name.ends_with(".ttf") {
            let content = std::fs::read(&path).map_err(|e| e.to_string())?;
            let digest = plx_base::sha256::sha256(&content);
            hashes.insert(
                name.to_owned(),
                digest.iter().map(|b| format!("{b:02x}")).collect(),
            );
        }
    }
    if !hashes.contains_key("appfont.ttf") || !hashes.contains_key("appfont-bold.ttf") {
        return Err("the shipped regular and bold faces are required".into());
    }
    Ok(hashes)
}

#[cfg(any(test, feature = "hostsim"))]
pub(crate) fn info() -> Value {
    json!({"protocol":1,"app_version":env!("PLX_VERSION"),"source_sha":env!("PLX_BUILD_SHA"),
        "formats":[format!("{}.{}",schema::CURRENT.0,schema::CURRENT.1)],
        "languages":LintLang::shipped().iter().map(|l| l.tag).collect::<Vec<_>>(),
        "rungs":schema::SIZE_RUNGS.iter().map(|(name,size)| (*name,*size)).collect::<BTreeMap<_,_>>()})
}

/// Preview automation reads the compiler's actual controls instead of reproducing HUD layout.
#[cfg(any(test, feature = "hostsim"))]
pub(crate) fn inspect(
    text: &str,
    measure: &dyn Measure,
    langs: &[LintLang],
) -> Result<Value, String> {
    use plx_appkit::hud_format::{
        compile::{compile, CompileCtx},
        lint::{slot_name, state_name},
        parse,
    };
    let parsed = parse::parse(text.as_bytes());
    let package = parsed
        .package
        .ok_or("cannot inspect a package that does not parse")?;
    let mut languages = Vec::new();
    for lang in langs {
        let compiled = compile(
            &package,
            measure,
            CompileCtx {
                labels: lang.labels.clone(),
            },
        );
        let mut valuations = Vec::new();
        for open in 0..4 {
            for slot in [
                plx_appkit::hud_format::reach::Slot::Discs,
                plx_appkit::hud_format::reach::Slot::UpNext,
                plx_appkit::hud_format::reach::Slot::Skip,
            ] {
                for bits in 0..16 {
                    let key = plx_appkit::hud_format::compile::StateKey {
                        open,
                        slot,
                        chapters: bits & 1 != 0,
                        episode: bits & 2 != 0,
                        real_episode: Some(bits & 4 != 0),
                        paused: bits & 8 != 0,
                    };
                    if let Some(state) = compiled.lookup_content(
                        open,
                        slot,
                        key.chapters,
                        key.episode,
                        key.real_episode.unwrap(),
                        key.paused,
                    ) {
                        valuations.push((key, state));
                    }
                }
            }
        }
        let states: Vec<Value> = valuations.iter().map(|(key,state)| {
            let scrub = state.ops.iter().find_map(|op| match op {
                plx_appkit::hud_format::compile::DrawOp::Playbar { grab, .. } =>
                    Some([grab.x,grab.y,grab.w,grab.h]),
                _ => None,
            });
            let controls: Vec<Value> = state.controls.iter().map(|c| json!({
                "id":c.id,"command":c.command,"section":c.section,"label":c.label,
                "menu":match c.act {
                    Some(plx_appkit::player_hud::command::HudCommand::OpenTracks(_)) => Some("tracks"),
                    Some(plx_appkit::player_hud::command::HudCommand::OpenMore(_)) => Some("more"),
                    _ => None,
                },
                "action":c.is_action,"rect":[c.rect.x,c.rect.y,c.rect.w,c.rect.h]
            })).collect();
            json!({"state":state_name(*key),"slot":slot_name(*key),"open":key.open,
                "chapters":key.chapters,"navigation_pair":key.episode,
                "episode":key.real_episode,"paused":key.paused,"controls":controls,"scrub":scrub})
        }).collect();
        languages.push(json!({"lang":lang.tag,"states":states}));
    }
    Ok(json!({"protocol":1,"languages":languages}))
}

/// Identical corpus and protocol on the simulator and TV; the supplied measurer is their actual one.
pub(crate) fn metrics(
    measure: &dyn Measure,
    langs: &[LintLang],
    fonts: &BTreeMap<String, String>,
) -> Value {
    let rows: Vec<Value> = text_metrics(measure, langs).into_iter().map(|m| json!({
        "lang":m.lang,"rung":m.rung,"size":m.size,"bold":m.bold,"role":m.role,"text":m.text,"width":m.width
    })).collect();
    json!({"protocol":1,"app_version":env!("PLX_VERSION"),"source_sha":env!("PLX_BUILD_SHA"),
        "font_hashes":fonts,"metrics":rows})
}

/// Registry CI passes a measured profile explicitly. Shipping playback never loads this file.
#[cfg(any(test, feature = "hostsim"))]
pub(crate) fn skew_profile(text: &str) -> Result<BTreeMap<i32, f32>, String> {
    let profile: Value =
        serde_json::from_str(text).map_err(|e| format!("invalid text-skew JSON: {e}"))?;
    if profile.get("protocol").and_then(Value::as_u64) != Some(1) {
        return Err("text-skew profile must use protocol 1".into());
    }
    let map = profile
        .get("by_rung")
        .and_then(Value::as_object)
        .ok_or("text-skew profile needs by_rung")?;
    if map.len() != schema::SIZE_RUNGS.len() {
        return Err("text-skew profile must name every supported rung".into());
    }
    let mut result = BTreeMap::new();
    for (name, value) in map {
        let size =
            schema::size_rung(name).ok_or_else(|| format!("unknown text-skew rung {name}"))?;
        let width = value.as_f64().ok_or("text-skew margin must be a number")?;
        let pixels = width as f32;
        if width < 0.0 || !pixels.is_finite() {
            return Err("text-skew margin must be finite and nonnegative".into());
        }
        result.insert(size, pixels);
    }
    Ok(result)
}

/// One diagnostic boot dump, using the TV's initialized TTF measurer. The caller supplies the
/// runtime-root output path and owns the trigger. No file is consulted by normal playback.
#[cfg(feature = "devtriggers")]
pub(crate) fn dump_metrics(path: &std::path::Path) -> Result<usize, String> {
    let report = metrics(
        &plx_gfx::text::TtfMeasure,
        &LintLang::shipped(),
        &font_hashes()?,
    );
    let rows = report["metrics"].as_array().map_or(0, Vec::len);
    let text = serde_json::to_vec(&report).map_err(|e| e.to_string())?;
    let temp = path.with_extension("tmp");
    std::fs::write(&temp, text).map_err(|e| e.to_string())?;
    std::fs::rename(temp, path).map_err(|e| e.to_string())?;
    Ok(rows)
}

#[cfg(test)]
mod tests {
    use super::*;
    use plx_base::fontcov::advances::ShippedMeasure;

    #[test]
    fn corpus_has_every_language_rung_weight_and_parameterized_label() {
        let langs = LintLang::shipped();
        let rows = text_metrics(&ShippedMeasure, &langs);
        let mut keys = std::collections::BTreeSet::new();
        for row in &rows {
            assert!(row.width.is_finite() && row.width > 0.0);
            assert!(keys.insert((row.lang, row.rung, row.bold, row.role.as_str())));
        }
        for lang in &langs {
            for &(rung, _) in schema::SIZE_RUNGS {
                for bold in [false, true] {
                    for role in [
                        "chapter.9",
                        "kicker.9",
                        "next.seconds.10",
                        "clock.max.true",
                        "command.tracks_subtitles",
                    ] {
                        assert!(
                            keys.contains(&(lang.tag, rung, bold, role)),
                            "{}/{rung}/{bold}/{role}",
                            lang.tag
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn calibration_requires_complete_finite_profiles() {
        let by_rung: BTreeMap<_, _> = schema::SIZE_RUNGS.iter().map(|(n, _)| (*n, 2)).collect();
        let mut value = json!({"protocol":1,"by_rung":by_rung});
        assert_eq!(
            skew_profile(&value.to_string()).unwrap().len(),
            schema::SIZE_RUNGS.len()
        );
        value["by_rung"]["BODY"] = json!(-1);
        assert!(skew_profile(&value.to_string()).is_err());
        assert!(skew_profile(r#"{"protocol":1,"by_rung":{}}"#).is_err());
        assert!(skew_profile("not json").is_err());
    }
}
