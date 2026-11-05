//! The one place a service event becomes log text. The `uriprobe` developer probe writes it after
//! its `uriprobe: t=<ms> ` stamp and the product backend after its `uri-trace: t=<ms> ` stamp, so a
//! harness that reads one stream reads the other: the words below are what the graders match.
//!
//! Pure: it reads the decoded [`Message`] and nothing else (no clock, no payload, no URL). Which
//! `currentTime` reports are written at all is each caller's policy, bounded by
//! [`CLOCK_LOG_EVERY_MS`]; only their TEXT is shared.
use crate::uri_protocol::{Ev, Message, Seen};

/// A moved `currentTime` is written at most this often (the first move always is).
pub(crate) const CLOCK_LOG_EVERY_MS: u64 = 1_000;

/// The text of one event `seen` in `message`, after the stamp. `first_report` marks a first
/// `currentTime` that has not moved yet; no other event reads it.
///
/// An event the message did not normalize (a supported name in a shape that carries none of its
/// facts) reads `ev=<name> (unparsed)`; the detail of a parsed one is the whitelisted scalars
/// already formatted in `seen`, and an event with none is its bare name.
pub(crate) fn event_text(message: &Message, seen: &Seen, first_report: bool) -> String {
    let name = seen.ev.name();
    if !message.normalized.contains(&seen.ev) {
        return unparsed_text(name);
    }
    match seen.ev {
        Ev::Error => {
            let code = message.error_code.map_or_else(|| "?".to_owned(), |c| c.to_string());
            format!("ev=error code={code} text=\"{}\"", message.error_text.as_deref().unwrap_or_default())
        }
        Ev::CurrentTime => match message.current_time {
            Some(value) if first_report => format!("ev=currentTime value={value} (first report)"),
            Some(value) => format!("ev=currentTime value={value}"),
            None => unparsed_text(name),
        },
        _ => format!("ev={name} {}", seen.detail).trim_end().to_owned(),
    }
}

/// An event word the service sent that the whitelist does not know, or knows in no readable shape.
pub(crate) fn unparsed_text(name: &str) -> String {
    format!("ev={name} (unparsed)")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::uri_protocol::parse_event;

    /// Every event of `payload`, each as the text both callers write for it.
    fn texts(payload: &str) -> Vec<String> {
        let message = parse_event(payload);
        message.seen.iter().map(|seen| event_text(&message, seen, false)).collect()
    }

    #[test]
    fn it_reproduces_the_probes_event_strings() {
        // Each payload and string is one the probe's own transcript tests pin (uriprobe.rs).
        for (payload, want) in [
            (r#"{"mediaId":"_m1","videoInfo":{"width":1920,"height":1080,"codec":"H264","frameRate":{"num":24000,"den":1001}}}"#,
                "ev=videoInfo width=1920 height=1080 codec=H264 frameRate=24000/1001"),
            (r#"{"mediaId":"_m1","videoInfo":{"width":1280,"height":720,"codec":"H264","currentBitrate":2000000,"contentBitrate":8000000}}"#,
                "ev=videoInfo width=1280 height=720 codec=H264 currentBitrate=2000000 contentBitrate=8000000"),
            (r#"{"mediaId":"_m1","bufferRange":{"beginTime":0,"endTime":58,"remainingTime":12,"percent":40}}"#,
                "ev=bufferRange beginTime=0 endTime=58 remainingTime=12 percent=40"),
            (r#"{"mediaId":"_m1","bufferRange":{"beginTime":3,"endTime":9}}"#,
                "ev=bufferRange beginTime=3 endTime=9"),
            (r#"{"mediaId":"_m1","streamingInfo":{"instantBitrate":8000000,"totalBitrate":2000000.5,"host":"192.168.1.5"}}"#,
                "ev=streamingInfo instantBitrate=8000000 totalBitrate=2000000.5"),
            (r#"{"mediaId":"_m1","streamingInfo":{"instantBitrate":true,"totalBitrate":4000000}}"#,
                "ev=streamingInfo totalBitrate=4000000"),
            (r#"{"mediaId":"_m1","streamingInfo":{"host":"192.168.1.5"}}"#, "ev=streamingInfo"),
            (r#"{"playing":true}"#, "ev=playing"),
            (r#"{"loadCompleted":true,"mediaId":"_m1"}"#, "ev=loadCompleted"),
            (r#"{"paused":true}"#, "ev=paused"),
            (r#"{"seekDone":true}"#, "ev=seekDone"),
            (r#"{"trackSelected":true}"#, "ev=trackSelected"),
            (r#"{"mediaId":"_m1","trackSelected":{"type":"audio","index":1}}"#,
                "ev=trackSelected type=audio index=1"),
            (r#"{"trackSelected":{"type":"text","index":0,"mediaId":"_m1"}}"#,
                "ev=trackSelected type=text index=0"),
            // a kind the whitelist does not know, or an index that is no number, is not repeated
            (r#"{"trackSelected":{"type":"video","index":2}}"#, "ev=trackSelected"),
            (r#"{"trackSelected":{"type":"audio","index":"1"}}"#, "ev=trackSelected"),
            (r#"{"endOfStream":true}"#, "ev=endOfStream"),
            (r#"{"bufferingStart":true}"#, "ev=bufferingStart"),
            (r#"{"bufferingEnd":true}"#, "ev=bufferingEnd"),
            (r#"{"currentTime":{"currentTime":1040,"mediaId":"_m1"}}"#, "ev=currentTime value=1040"),
            (r#"{"currentTime":50}"#, "ev=currentTime value=50"),
        ] {
            assert_eq!(texts(payload), [want], "{payload}");
        }
    }

    #[test]
    fn a_first_unmoved_clock_report_says_so_and_an_unreadable_one_is_unparsed() {
        let message = parse_event(r#"{"currentTime":{"currentTime":40,"mediaId":"_m1"}}"#);
        let seen = &message.seen[0];
        assert_eq!(event_text(&message, seen, true), "ev=currentTime value=40 (first report)");
        assert_eq!(event_text(&message, seen, false), "ev=currentTime value=40");
        for payload in [r#"{"currentTime":{"mediaId":"_m1"}}"#, r#"{"currentTime":"soon"}"#] {
            assert_eq!(texts(payload), ["ev=currentTime (unparsed)"], "{payload}");
        }
    }

    #[test]
    fn an_error_names_its_code_and_scrubbed_text() {
        assert_eq!(texts(r#"{"mediaId":"_m1","error":{"errorCode":-7,"errorText":"unsupported codec"}}"#),
            ["ev=error code=-7 text=\"unsupported codec\""]);
    }

    #[test]
    fn the_unknown_marker_is_the_same_unparsed_form() {
        assert_eq!(unparsed_text("unknown"), "ev=unknown (unparsed)");
    }
}
