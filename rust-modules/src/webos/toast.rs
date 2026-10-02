//! A message through the television's OWN notification UI: `luna://com.webos.notification/createToast`.
//!
//! The service takes the caller's identity from the message's application id, else from the sender's
//! service name, and answers "Unknown Source" when both are empty; for a non-privileged caller the
//! payload's `sourceId` must equal that identity (webosose `notificationmgr`, `cb_createToast`).
//! This process holds no service name — the app-id name belongs to ACB, and an anonymous
//! `LSRegister(NULL)` is the one shape the hub accepts (see [`super::ls2`]) — so the candidate is
//! the application-id call, `LSCallFromApplicationOneReply`, on that same anonymous handle. Whether
//! the hub lets a jailed app say so about itself is what the `toast` dev trigger measures.
//!
//! **Blocking.** One LS2 round trip, so [`toast`] must run on a worker, never on the frame thread;
//! it asserts that itself (`crate::task::assert_may_block`).
//!
//! **Off-device** there is no bus and nothing here touches one: `go_home`'s precedent, a log line
//! and [`Outcome::NoBus`].

/// The service method every call here targets.
const CREATE_TOAST: &str = "luna://com.webos.notification/createToast";

/// How long one round trip may take. Off the UI thread, so generous next to `ls2::BUDGET`.
#[cfg(all(not(feature = "hostsim"), not(test)))]
const BUDGET: std::time::Duration = std::time::Duration::from_secs(3);

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
    /// Host test or simulator: there is no LS2 bus and none was touched.
    #[cfg(any(feature = "hostsim", test))]
    NoBus,
}

/// One attempt, with the platform's raw reply kept beside the grade for the probe's log line.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Sent {
    pub reply: Option<String>,
    pub outcome: Outcome,
}

/// The `createToast` payload for `message`, attributed to `source_id`. `noaction` removes the
/// launch arrow so the card is a notice, not a shortcut. Built with `serde_json` so every quote,
/// backslash, newline and non-ASCII character is escaped by the same writer the rest of the crate
/// trusts.
pub(crate) fn payload(source_id: &str, message: &str) -> String {
    serde_json::json!({ "sourceId": source_id, "noaction": true, "message": message }).to_string()
}

/// Grade the service's own reply: `returnValue: true` is acceptance; anything else is a refusal,
/// carrying the `errorText` when there is one.
#[cfg_attr(feature = "hostsim", allow(dead_code))] // Graded only against a real bus reply.
pub(crate) fn grade(reply: &str) -> Outcome {
    let value = serde_json::from_str::<serde_json::Value>(reply).ok();
    let field = |name: &str| value.as_ref().and_then(|v| v.get(name));
    if field("returnValue").and_then(serde_json::Value::as_bool) == Some(true) {
        return Outcome::Accepted;
    }
    let error_text = field("errorText")
        .and_then(serde_json::Value::as_str)
        .map_or_else(|| "no errorText in the reply".to_string(), str::to_string);
    Outcome::Refused { error_text }
}

/// Show `message` as a system toast, attributed to this app — the as-app call.
///
/// Blocks for an LS2 round trip: never call it from the frame thread.
pub(crate) fn toast(message: &str) -> Outcome {
    send(message, Identity::AsApp).outcome
}

/// One attempt under the chosen [`Identity`], reply and grade both. Blocks.
pub(crate) fn send(message: &str, identity: Identity) -> Sent {
    let _block = crate::task::assert_may_block(const { &crate::task::BlockingLabel::new("LS2 toast") });
    let payload = payload(crate::paths::app_id(), message);
    deliver(&payload, identity)
}

#[cfg(any(feature = "hostsim", test))]
fn deliver(_payload: &str, identity: Identity) -> Sent {
    crate::log(&format!(
        "toast: no LS2 bus off-device — {identity:?} call to {CREATE_TOAST} not sent"
    ));
    Sent { reply: None, outcome: Outcome::NoBus }
}

#[cfg(all(not(feature = "hostsim"), not(test)))]
fn deliver(payload: &str, identity: Identity) -> Sent {
    use super::ls2::{self, Fail};
    let bus = |fail: Fail| match fail {
        Fail::Timeout => Outcome::Bus { stage: "timeout", code: None, detail: String::new() },
        Fail::Setup { stage, code, detail } => Outcome::Bus { stage, code, detail },
    };
    let registration = match ls2::register() {
        Ok(r) => r,
        Err(e) => return Sent { reply: None, outcome: bus(e.into()) },
    };
    let called = match identity {
        #[cfg(any(feature = "devtriggers", test))]
        Identity::Anonymous => registration.call(CREATE_TOAST, payload, BUDGET),
        Identity::AsApp => registration.call_as_app(CREATE_TOAST, payload, crate::paths::app_id(), BUDGET),
    };
    match called {
        Ok(reply) => {
            let outcome = grade(&reply);
            Sent { reply: Some(reply), outcome }
        }
        Err(fail) => Sent { reply: None, outcome: bus(fail) },
    }
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

    fn parsed(payload: &str) -> serde_json::Value {
        serde_json::from_str(payload).expect("the payload is JSON")
    }

    #[test]
    fn payload_carries_source_id_noaction_and_message() {
        let v = parsed(&payload("com.beb.plxnative.debug", "Hello"));
        assert_eq!(v["sourceId"], "com.beb.plxnative.debug");
        assert_eq!(v["noaction"], true);
        assert_eq!(v["message"], "Hello");
        assert_eq!(v.as_object().unwrap().len(), 3, "no stray fields: {v}");
    }

    #[test]
    fn payload_escapes_quotes_backslashes_and_newlines() {
        let message = "say \"hi\"\\ back\nnext line\ttab";
        let text = payload("com.beb.plxnative", message);
        assert!(!text.contains('\n'), "a raw newline would break the line-oriented bus log: {text}");
        assert_eq!(parsed(&text)["message"], message);
    }

    #[test]
    fn payload_keeps_non_ascii_text_intact() {
        let message = "Гадзіннік тэлевізара ідзе няправільна — 時計";
        assert_eq!(parsed(&payload("com.beb.plxnative", message))["message"], message);
    }

    #[test]
    fn a_message_cannot_inject_fields() {
        let v = parsed(&payload("a.b", r#"x","noaction":false,"sourceId":"evil"#));
        assert_eq!(v["noaction"], true);
        assert_eq!(v["sourceId"], "a.b");
    }

    #[test]
    fn grade_accepts_return_value_true() {
        assert_eq!(grade(r#"{"returnValue":true,"toastId":"1"}"#), Outcome::Accepted);
        assert_eq!(grade(r#"{ "returnValue": true }"#), Outcome::Accepted);
    }

    #[test]
    fn grade_surfaces_the_service_error_text() {
        assert_eq!(
            grade(r#"{"returnValue":false,"errorCode":-1,"errorText":"Unknown Source"}"#),
            Outcome::Refused { error_text: "Unknown Source".into() }
        );
    }

    #[test]
    fn grade_never_reads_a_missing_or_garbled_reply_as_acceptance() {
        for reply in [r#"{"returnValue":false}"#, r#"{}"#, "", "not json", r#"{"returnValue":"true"}"#] {
            assert!(
                matches!(grade(reply), Outcome::Refused { .. }),
                "{reply:?} must not be Accepted"
            );
        }
        assert_eq!(
            grade(r#"{"returnValue":false}"#),
            Outcome::Refused { error_text: "no errorText in the reply".into() }
        );
    }

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
