//! **The television's own screensaver, answered for by the app**, as everything outside the port
//! sees it.
//!
//! webOS does not blank a foreground native app on its own (`app/boot.rs`, the note above
//! `SDL_VIDEO_ALLOW_SCREENSAVER`), so a set left on Home holds the whole UI at full brightness. The
//! platform does have a door for this: a client subscribes to tvpower's
//! `registerScreenSaverRequest`, the television ASKS it each time its own idle timer wants the
//! screensaver ("state": "Active", with a timestamp), and the client answers through
//! `responseScreenSaverRequest` with `ack` true (go ahead) or false (not now). That keeps the
//! WHEN with the television — its own timer, the viewer's own settings — and leaves the app one
//! fact to supply: is a video playing? A playing video says no; Home, a detail page, Settings or a
//! PAUSED video say yes, which is exactly where a still image would otherwise sit on the panel.
//!
//! The protocol is Kodi's webOS port (`xbmc/windowing/wayland/OSScreenSaverWebOS.cpp`), which
//! uses it to say no while it plays; here the answer depends on the playback.
//!
//! The port's agent (`webos::screensaver`) holds the subscription on its own thread and calls
//! [`reply`] for every message; the app loop keeps [`set_video_playing`] current each frame.
//! Without a platform bus (the simulator, a host test) [`start`] does nothing.

use std::sync::atomic::{AtomicBool, Ordering::Relaxed};

/// The subscription the television asks through.
pub const REGISTER: &str = "luna://com.webos.service.tvpower/power/registerScreenSaverRequest";
/// Where an answer goes.
pub const RESPOND: &str = "luna://com.webos.service.tvpower/power/responseScreenSaverRequest";

/// Is a video playing right now? Written by the frame loop, read by the port's agent thread.
static PLAYING: AtomicBool = AtomicBool::new(false);

/// The frame loop's one fact: a video (or a trailer) is playing, not paused.
pub fn set_video_playing(playing: bool) {
    PLAYING.store(playing, Relaxed);
}

/// What [`set_video_playing`] last said.
pub fn video_playing() -> bool {
    PLAYING.load(Relaxed)
}

/// Start answering the television's screensaver requests. Once, at boot; the port's agent runs on
/// its own thread and never blocks the caller.
pub fn start() {
    (super::port().start_screensaver)()
}

/// The subscription's payload, naming this app as the client the television asks.
pub fn register_payload(client: &str) -> String {
    serde_json::json!({ "subscribe": true, "clientName": client }).to_string()
}

/// One message on the subscription, read.
#[derive(Debug, PartialEq)]
pub enum Message {
    /// The television wants its screensaver: answer with this message's timestamp.
    Ask { timestamp: serde_json::Value },
    /// The subscription was accepted (or any other `returnValue: true` that asks nothing).
    Accepted,
    /// The service refused, in its own words; nothing more will come on this subscription.
    Refused(String),
    /// Not JSON, or nothing this reads.
    Unreadable,
}

/// Read one message the subscription delivered.
pub fn read(payload: &str) -> Message {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(payload) else {
        return Message::Unreadable;
    };
    if v.get("returnValue").and_then(|r| r.as_bool()) == Some(false) {
        let text = v.get("errorText").and_then(|t| t.as_str()).unwrap_or("no errorText");
        let code = v.get("errorCode").map(|c| c.to_string()).unwrap_or_default();
        return Message::Refused(format!("{code} {text}").trim().to_string());
    }
    match (v.get("state").and_then(|s| s.as_str()), v.get("timestamp")) {
        (Some("Active"), Some(ts)) => Message::Ask { timestamp: ts.clone() },
        _ if v.get("returnValue").is_some() || v.get("subscribed").is_some() => Message::Accepted,
        _ => Message::Unreadable,
    }
}

/// The answer to an [`Message::Ask`]: go ahead unless a video is playing.
pub fn reply(client: &str, timestamp: &serde_json::Value, playing: bool) -> String {
    serde_json::json!({ "clientName": client, "ack": !playing, "timestamp": timestamp }).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_subscription_names_this_app() {
        let p: serde_json::Value = serde_json::from_str(&register_payload("com.example.app")).unwrap();
        assert_eq!(p["subscribe"], true);
        assert_eq!(p["clientName"], "com.example.app");
    }

    #[test]
    fn a_request_is_read_and_answered_with_its_own_timestamp() {
        let ask = read(r#"{"returnValue":true,"state":"Active","timestamp":1697532000}"#);
        let Message::Ask { timestamp } = ask else { panic!("{ask:?}") };
        let yes: serde_json::Value = serde_json::from_str(&reply("app", &timestamp, false)).unwrap();
        assert_eq!((yes["ack"].as_bool(), yes["timestamp"].as_i64(), yes["clientName"].as_str()),
            (Some(true), Some(1_697_532_000), Some("app")));
        let no: serde_json::Value = serde_json::from_str(&reply("app", &timestamp, true)).unwrap();
        assert_eq!(no["ack"], false, "a playing video says not now");
    }

    /// A timestamp is handed back exactly as it came, whatever JSON type the firmware uses for it.
    #[test]
    fn a_string_timestamp_goes_back_unchanged() {
        let Message::Ask { timestamp } = read(r#"{"state":"Active","timestamp":"2023-10-17 08:00:00"}"#) else { panic!() };
        let r: serde_json::Value = serde_json::from_str(&reply("app", &timestamp, false)).unwrap();
        assert_eq!(r["timestamp"], "2023-10-17 08:00:00");
    }

    #[test]
    fn the_other_messages_are_told_apart() {
        assert_eq!(read(r#"{"returnValue":true,"subscribed":true}"#), Message::Accepted);
        assert_eq!(read(r#"{"returnValue":true,"state":"Inactive","timestamp":1}"#), Message::Accepted);
        assert_eq!(
            read(r#"{"returnValue":false,"errorCode":-1,"errorText":"Unknown method"}"#),
            Message::Refused("-1 Unknown method".into())
        );
        assert_eq!(read("not json"), Message::Unreadable);
        assert_eq!(read(r#"{"state":"Active"}"#), Message::Unreadable, "an ask without a timestamp cannot be answered");
    }

    #[test]
    fn the_playing_flag_reads_back() {
        let _serial = plx_base::testlock::serial();
        set_video_playing(true);
        assert!(video_playing());
        set_video_playing(false);
        assert!(!video_playing());
    }
}
