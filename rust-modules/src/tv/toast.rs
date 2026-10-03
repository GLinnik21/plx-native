//! A message through the television's OWN notification UI, as everything outside the port sees it.
//! The platform bus call itself is the port's (`webos::toast`); this is the vocabulary it answers
//! in and the one door callers go through.
//!
//! **Blocking.** One platform round trip, so [`toast`] must run on a worker, never on the frame
//! thread; it asserts that itself (`crate::task::assert_may_block`).
//!
//! **Without a platform bus** (the simulator, a host test, or no port installed) nothing here
//! touches one: a log line and [`Outcome::NoBus`].

/// How a call presents itself to the hub.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Identity {
    /// A plain anonymous call: no application id, no service name. The probe's control leg only.
    #[cfg(any(feature = "devtriggers", test))]
    Anonymous,
    /// `LSCallFromApplication…` carrying this app's id — what `luna-send -a <id>` does.
    AsApp,
}

/// What came of one attempt. Three distinct stories, because each is a different bug.
#[derive(Debug, PartialEq, Eq)]
// The simulator has no bus, so only `NoBus` is ever constructed there.
#[cfg_attr(feature = "hostsim", allow(dead_code))]
pub(crate) enum Outcome {
    /// The service said `returnValue: true`.
    Accepted,
    /// The service answered and said no; its own `errorText` travels with it.
    Refused { error_text: String },
    /// The bus never carried the call: the stage that failed, the hub's code and words if it gave
    /// any (`timeout` is a call that WAS sent and never answered).
    Bus { stage: &'static str, code: Option<i32>, detail: String },
    /// No platform bus: the simulator, a host test, or no port.
    NoBus,
}

/// One attempt, with the platform's raw reply kept beside the grade for the probe's log line.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Sent {
    pub reply: Option<String>,
    pub outcome: Outcome,
}

/// Show `message` as a system toast, attributed to this app — the as-app call.
///
/// Blocks for a platform round trip: never call it from the frame thread.
pub(crate) fn toast(message: &str) -> Outcome {
    send(message, Identity::AsApp).outcome
}

/// One attempt under the chosen [`Identity`], reply and grade both. Blocks.
pub(crate) fn send(message: &str, identity: Identity) -> Sent {
    let _block = crate::task::assert_may_block(const { &crate::task::BlockingLabel::new("LS2 toast") });
    (super::port().deliver_toast)(message, identity)
}

/// What an absent port answers: nothing was sent, and the log says so.
pub(super) fn deliver_without_port(_message: &str, identity: Identity) -> Sent {
    crate::eventlog::log(&format!(
        "toast: no platform port — {identity:?} toast not sent"
    ));
    Sent { reply: None, outcome: Outcome::NoBus }
}

/// The probe's log line for one attempt: the whole reply when the service answered (a refusal is
/// never summarised away), the stage/code/detail when the bus failed.
#[cfg(any(feature = "devtriggers", test))]
pub(crate) fn probe_line(label: &str, sent: &Sent) -> String {
    match (&sent.reply, &sent.outcome) {
        (Some(reply), _) => format!("toast-probe {label}: reply={reply}"),
        (None, Outcome::Bus { stage, code, detail }) => {
            let code = code.map_or_else(|| "none".to_string(), |c| c.to_string());
            format!("toast-probe {label}: fail stage={stage} code={code} detail={detail}")
        }
        (None, _) => format!("toast-probe {label}: no LS2 bus off-device"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn off_device_no_bus_is_touched() {
        let sent = send("hello", Identity::AsApp);
        assert_eq!(sent, Sent { reply: None, outcome: Outcome::NoBus });
        assert_eq!(toast("hello"), Outcome::NoBus);
        assert_eq!(send("hello", Identity::Anonymous).outcome, Outcome::NoBus);
    }

    #[test]
    fn probe_lines_name_the_reply_or_the_failure() {
        let answered = Sent {
            reply: Some(r#"{"returnValue":false,"errorText":"Unknown Source"}"#.into()),
            outcome: Outcome::Refused { error_text: "Unknown Source".into() },
        };
        assert_eq!(
            probe_line("as-app", &answered),
            r#"toast-probe as-app: reply={"returnValue":false,"errorText":"Unknown Source"}"#
        );
        let failed = Sent {
            reply: None,
            outcome: Outcome::Bus { stage: "call", code: Some(-1027), detail: "code -1027: denied".into() },
        };
        assert_eq!(
            probe_line("plain", &failed),
            "toast-probe plain: fail stage=call code=-1027 detail=code -1027: denied"
        );
        let timeout = Sent { reply: None, outcome: Outcome::Bus { stage: "timeout", code: None, detail: String::new() } };
        assert_eq!(probe_line("plain", &timeout), "toast-probe plain: fail stage=timeout code=none detail=");
        let off = Sent { reply: None, outcome: Outcome::NoBus };
        assert_eq!(probe_line("plain", &off), "toast-probe plain: no LS2 bus off-device");
    }
}
