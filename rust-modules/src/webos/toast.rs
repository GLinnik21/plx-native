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
//! **Blocking.** One LS2 round trip, so [`crate::tv::toast::toast`] must run on a worker, never on
//! the frame thread; `crate::tv::toast::send` asserts that itself (`crate::task::assert_may_block`).
//!
//! **Off-device** there is no bus and nothing here touches one: `go_home`'s precedent, a log line
//! and [`Outcome::NoBus`].
//!
//! The vocabulary ([`Identity`], [`Outcome`], [`Sent`]) lives in `tv::toast`; this module is the
//! port's half, [`deliver`], plus the pure payload and grade that only it uses.

use crate::tv::toast::{Identity, Outcome, Sent};

/// The service method every call here targets.
const CREATE_TOAST: &str = "luna://com.webos.notification/createToast";

/// How long one round trip may take. Off the UI thread, so generous next to `ls2::BUDGET`.
#[cfg(all(not(feature = "hostsim"), not(test)))]
const BUDGET: std::time::Duration = std::time::Duration::from_secs(3);

/// The `createToast` payload for `message`, attributed to `source_id`. `noaction` removes the
/// launch arrow so the card is a notice, not a shortcut. Built with `serde_json` so every quote,
/// backslash, newline and non-ASCII character is escaped by the same writer the rest of the crate
/// trusts.
#[cfg_attr(feature = "hostsim", allow(dead_code))] // Built only for a real bus call.
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

/// One attempt under the chosen [`Identity`], reply and grade both. Blocks. The port's
/// `deliver_toast`: `tv::toast::send` is the door callers use.
#[cfg(any(feature = "hostsim", test))]
pub(crate) fn deliver(_message: &str, identity: Identity) -> Sent {
    crate::eventlog::log(&format!(
        "toast: no LS2 bus off-device — {identity:?} call to {CREATE_TOAST} not sent"
    ));
    Sent { reply: None, outcome: Outcome::NoBus }
}

/// The on-device arm of [`deliver`] above.
#[cfg(all(not(feature = "hostsim"), not(test)))]
pub(crate) fn deliver(message: &str, identity: Identity) -> Sent {
    use super::ls2::{self, Fail};
    let payload = payload(crate::paths::app_id(), message);
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
        Identity::Anonymous => registration.call(CREATE_TOAST, &payload, BUDGET),
        Identity::AsApp => registration.call_as_app(CREATE_TOAST, &payload, crate::paths::app_id(), BUDGET),
    };
    match called {
        Ok(reply) => {
            let outcome = grade(&reply);
            Sent { reply: Some(reply), outcome }
        }
        Err(fail) => Sent { reply: None, outcome: bus(fail) },
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
}
