//! Which sound output the television is playing through (`tv_speaker`, `bt_soundbar`, ...), as
//! everything outside the port sees it. The bus call itself is the port's (`webos::sound_output`);
//! this is the vocabulary it answers in, the parse, and the one door callers go through.
//!
//! **Blocking.** One platform round trip, so [`output`] must run on a worker, never on the frame
//! thread; it asserts that itself (`plx_base::task::assert_may_block`). The playback engine reads
//! it from the Load thread, once per PCM Load, only on a stack whose lead follows the output
//! (`liblxa`, `audiosink_new`; see `player::engine::pcm_lead_plan`); other stacks never ask.
//!
//! **Not a bounded wait.** The call has a 300 ms budget; the bus registration before it and the
//! unregistration after it do not (`webos::ls2`), so a wedged luna hub delays the caller by as long
//! as the hub takes, not by 300 ms.

/// What the platform's settings service said. [`parse_output`] owns the reading of it.
#[cfg_attr(any(feature = "hostsim", test, feature = "test-support"), allow(dead_code))] // Reply/Unavailable: TV arm only
pub enum Reply { NoPlatform, Unavailable, Reply(String) }

/// The active sound output's name as the firmware spells it, or `None` when there is no platform,
/// the call failed, or the answer was not one. Blocks.
pub fn output() -> Option<String> {
    let _block = plx_base::task::assert_may_block(const { &plx_base::task::BlockingLabel::new("LS2 sound output") });
    let started = std::time::Instant::now();
    match (super::port().sound_output)() {
        Reply::NoPlatform => None,
        Reply::Unavailable => {
            plx_base::eventlog::log(&format!("sound: output unavailable after {}ms", started.elapsed().as_millis()));
            None
        }
        Reply::Reply(raw) => {
            let name = parse_output(&raw);
            plx_base::eventlog::log(&format!(
                "sound: output {} read in {}ms",
                name.as_deref().unwrap_or("unreadable"),
                started.elapsed().as_millis()
            ));
            name
        }
    }
}

/// `getSystemSettings {"category":"sound","keys":["soundOutput"]}`'s reply, reduced to the output's
/// name. PURE.
pub fn parse_output(raw: &str) -> Option<String> {
    let v: serde_json::Value = serde_json::from_str(raw).ok()?;
    if v["returnValue"].as_bool() != Some(true) {
        return None;
    }
    let name = v["settings"]["soundOutput"].as_str()?;
    let spelled_like_one = (1..=32).contains(&name.len())
        && name.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_');
    spelled_like_one.then(|| name.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_verified_replies_name_their_output() {
        let speaker = r#"{"category":"sound","method":"getSystemSettings","settings":{"soundOutput":"tv_speaker"},"returnValue":true}"#;
        let headphones = r#"{"category":"sound","method":"getSystemSettings","settings":{"soundOutput":"bt_soundbar"},"returnValue":true}"#;
        assert_eq!(parse_output(speaker).as_deref(), Some("tv_speaker"));
        assert_eq!(parse_output(headphones).as_deref(), Some("bt_soundbar"));
    }

    #[test]
    fn a_reply_that_is_not_an_answer_names_nothing() {
        for (why, raw) in [
            ("no key", r#"{"category":"sound","settings":{},"returnValue":true}"#),
            ("no settings", r#"{"returnValue":true}"#),
            ("refused", r#"{"returnValue":false,"errorText":"denied"}"#),
            ("refused with a value", r#"{"settings":{"soundOutput":"tv_speaker"},"returnValue":false}"#),
            ("no returnValue", r#"{"settings":{"soundOutput":"tv_speaker"}}"#),
            ("not a string", r#"{"settings":{"soundOutput":3},"returnValue":true}"#),
            ("empty", r#"{"settings":{"soundOutput":""},"returnValue":true}"#),
            ("garbage", "not json at all"),
            ("empty body", ""),
            ("an array", "[1,2]"),
        ] {
            assert_eq!(parse_output(raw), None, "{why}");
        }
    }

    #[test]
    fn a_name_that_could_not_be_an_output_is_refused() {
        let wrap = |name: &str| format!(r#"{{"settings":{{"soundOutput":"{name}"}},"returnValue":true}}"#);
        assert_eq!(parse_output(&wrap("external_arc")).as_deref(), Some("external_arc"));
        assert_eq!(parse_output(&wrap("tv speaker")), None, "a space");
        assert_eq!(parse_output(&wrap("TV_Speaker")), None, "upper case is not how the firmware spells one");
        assert_eq!(parse_output(&wrap(&"a".repeat(33))), None, "longer than any output name");
        assert_eq!(parse_output(&wrap(&"a".repeat(32))).as_deref(), Some(&*"a".repeat(32)));
    }

    #[test]
    fn off_device_no_bus_is_touched() {
        assert_eq!(output(), None);
    }
}
