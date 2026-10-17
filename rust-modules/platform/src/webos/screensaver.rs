//! The port's half of `tv::screensaver`: one thread that holds tvpower's
//! `registerScreenSaverRequest` subscription and answers every request the television sends.
//!
//! Its own thread, because the subscription has to be answered whenever the television asks —
//! including while the app is backgrounded and its frame loop is asleep — and because each answer
//! is a bus round trip that must never land on the frame thread. The thread blocks on its own
//! registration's glib context ([`ls2::Registration::wait`]) and does nothing else.
//!
//! Every message is logged (a request comes every few minutes at most), so the event log says
//! whether this firmware lets the app register, whether it ever asks, and what it said to each
//! answer: the evidence a new firmware is judged by.

#[cfg(all(not(feature = "hostsim"), not(any(test, feature = "test-support"))))]
pub fn start() {
    if !plx_base::task::spawn_small("screensaver", run) {
        plx_base::eventlog::log("screensaver: could not start its thread — the television's screensaver stays off");
    }
}

/// No bus off the television.
#[cfg(any(feature = "hostsim", test, feature = "test-support"))]
pub fn start() {}

#[cfg(all(not(feature = "hostsim"), not(any(test, feature = "test-support"))))]
fn run() {
    use crate::tv::screensaver::{read, register_payload, reply, video_playing, Message, REGISTER, RESPOND};
    use plx_base::eventlog::log;
    /// An answer is one small call; a bus that cannot carry it in this long will not carry it later.
    const BUDGET: std::time::Duration = std::time::Duration::from_secs(2);

    let registration = match super::ls2::register() {
        Ok(r) => r,
        Err(e) => {
            log(&format!("screensaver: no bus registration ({e})"));
            return;
        }
    };
    let app = plx_base::paths::app_id();
    let feed = match registration.subscribe_as_app(REGISTER, &register_payload(app), app) {
        Ok(f) => f,
        Err(super::ls2::Fail::Setup { stage, detail, .. }) => {
            log(&format!("screensaver: subscribe failed stage={stage} ({detail})"));
            return;
        }
        Err(super::ls2::Fail::Timeout) => {
            log("screensaver: subscribe timed out");
            return;
        }
    };
    loop {
        registration.wait();
        for payload in feed.take() {
            match read(&payload) {
                Message::Ask { timestamp } => {
                    let playing = video_playing();
                    let answer = reply(app, &timestamp, playing);
                    let outcome = match registration.call_as_app(RESPOND, &answer, app, BUDGET) {
                        Ok(r) => r.split_whitespace().collect::<Vec<_>>().join(" "),
                        Err(super::ls2::Fail::Timeout) => "timed out".into(),
                        Err(super::ls2::Fail::Setup { stage, detail, .. }) => format!("failed stage={stage} ({detail})"),
                    };
                    log(&format!(
                        "screensaver: the television asked; answered {} → {outcome}",
                        if playing { "not now (playing)" } else { "go ahead" }
                    ));
                }
                Message::Accepted => log(&format!("screensaver: subscription → {}", payload.trim())),
                Message::Refused(why) => {
                    // Nothing more comes on a refused subscription; the thread's work is over.
                    log(&format!("screensaver: the television refused the subscription ({why})"));
                    return;
                }
                Message::Unreadable => log(&format!("screensaver: unreadable message {}", payload.trim())),
            }
        }
    }
}
