//! Registration ownership and request correlation. No transport, clock, worker or registry is read.
use super::uri_protocol::{parse_event, Ev, MediaId, Message, Method, UriCall};
use plx_platform::tv::uri::{CloseStatus, RequestId, SessionId, Shape, UriReply};
use std::collections::BTreeMap;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    Released,
    Opening,
    Open,
    Closing,
    Quarantined,
}

pub enum Event {
    Open(Shape),
    OpenAnswered {
        request: RequestId,
        result: Result<SessionId, Message>,
    },
    Call(UriCall),
    CallQueued {
        session: SessionId,
        request: RequestId,
        ok: bool,
        message: Message,
    },
    Reply(UriReply),
    Close {
        now_ms: u64,
        allowance_ms: u64,
    },
    CloseQueued {
        session: SessionId,
        ok: bool,
    },
    PollFailed {
        session: SessionId,
    },
    Status {
        session: SessionId,
        status: CloseStatus,
    },
    Tick {
        now_ms: u64,
    },
}

#[derive(Debug, PartialEq)]
pub enum Effect {
    Open {
        request: RequestId,
        shape: Shape,
    },
    Call {
        session: SessionId,
        request: RequestId,
        call: UriCall,
    },
    Close {
        session: SessionId,
    },
    Deliver {
        method: Method,
        ok: bool,
        message: Message,
    },
    Released,
    Fault,
}

struct Pending {
    call: UriCall,
    ack: Option<bool>,
    confirmed: bool,
}

pub struct Lifecycle {
    phase: Phase,
    session: Option<SessionId>,
    opening: Option<RequestId>,
    media_id: Option<MediaId>,
    next: u64,
    requests: BTreeMap<RequestId, Pending>,
    deadline: Option<u64>,
    faulted: bool,
    plane_observed: bool,
    release_reported: bool,
}
impl Default for Lifecycle {
    fn default() -> Self {
        Self::new()
    }
}
impl Lifecycle {
    pub fn new() -> Self {
        Self {
            phase: Phase::Released,
            session: None,
            opening: None,
            media_id: None,
            next: 0,
            requests: BTreeMap::new(),
            deadline: None,
            faulted: false,
            plane_observed: false,
            release_reported: false,
        }
    }
    pub fn plane_open(&self) -> bool {
        self.plane_observed
    }
    pub fn phase(&self) -> Phase {
        self.phase
    }
    pub fn session(&self) -> Option<SessionId> {
        self.session
    }
    pub fn released(&self) -> bool {
        self.phase == Phase::Released
    }
    fn request(&mut self) -> RequestId {
        // Exhaustion is unreachable in a bounded developer script, but never aliases an old call.
        self.next = self
            .next
            .checked_add(1)
            .expect("URI request identities exhausted");
        RequestId::new(self.next)
    }
    fn fault(&mut self, fx: &mut Vec<Effect>) {
        if !self.faulted {
            self.faulted = true;
            fx.push(Effect::Fault);
        }
    }
    pub fn step(&mut self, event: Event, fx: &mut Vec<Effect>) {
        match event {
            Event::Open(shape) if self.released() => {
                let request = self.request();
                self.opening = Some(request);
                self.phase = Phase::Opening;
                self.faulted = false;
                self.release_reported = false;
                fx.push(Effect::Open { request, shape });
            }
            Event::Open(_) => self.fault(fx),
            Event::OpenAnswered { request, result }
                if self.opening == Some(request)
                    && matches!(
                        self.phase,
                        Phase::Opening | Phase::Closing | Phase::Quarantined
                    ) =>
            {
                self.opening = None;
                match result {
                    Ok(session) => {
                        self.session = Some(session);
                        if matches!(self.phase, Phase::Closing | Phase::Quarantined) {
                            fx.push(Effect::Close { session });
                        }
                    }
                    Err(message) => {
                        let closing = matches!(self.phase, Phase::Closing | Phase::Quarantined);
                        self.phase = Phase::Released;
                        if closing {
                            self.release_reported = true;
                            fx.push(Effect::Released);
                        } else {
                            fx.push(Effect::Deliver {
                                method: Method::Register,
                                ok: false,
                                message,
                            });
                        }
                    }
                }
            }
            Event::Call(call) if self.phase == Phase::Open => {
                let request = self.request();
                let session = self.session.expect("open registration has identity");
                self.requests.insert(
                    request,
                    Pending {
                        call: call.clone(),
                        ack: None,
                        confirmed: false,
                    },
                );
                fx.push(Effect::Call {
                    session,
                    request,
                    call,
                });
            }
            Event::Call(call) => fx.push(Effect::Deliver {
                method: Method::of(call.method()),
                ok: false,
                message: Message::default(),
            }),
            Event::CallQueued {
                session,
                request,
                ok: false,
                mut message,
            } if self.session == Some(session) => {
                if let Some(pending) = self.requests.get_mut(&request) {
                    message.reply_request = Some(request);
                    pending.ack = Some(false);
                    if let UriCall::SelectTrack { kind, .. } = &pending.call {
                        message.reply_track = Some(*kind);
                    }
                    fx.push(Effect::Deliver {
                        method: Method::of(pending.call.method()),
                        ok: false,
                        message,
                    });
                }
            }
            Event::Reply(reply)
                if self.session == Some(reply.session)
                    && matches!(self.phase, Phase::Opening | Phase::Open) =>
            {
                let method = Method::of(reply.method);
                if method == Method::Dropped {
                    self.fault(fx);
                    return;
                }
                if method == Method::Register
                    && self.phase == Phase::Opening
                    && reply.request == RequestId::new(0)
                {
                    self.phase = Phase::Open;
                    fx.push(Effect::Deliver {
                        method,
                        ok: reply.ok,
                        message: if reply.ok {
                            Message::default()
                        } else {
                            Message::transport_failure(&reply.payload)
                        },
                    });
                    return;
                }
                let Some(pending) = self.requests.get_mut(&reply.request) else {
                    return;
                };
                if pending.call.method() != reply.method {
                    return;
                }
                let mut message = if reply.ok {
                    parse_event(&reply.payload)
                } else {
                    Message::transport_failure(&reply.payload)
                };
                message.reply_request = Some(reply.request);
                if message.identity_conflict
                    || (self.media_id.is_some()
                        && message.media_id.is_some()
                        && self.media_id != message.media_id)
                {
                    return;
                }
                if method == Method::Load
                    && reply.ok
                    && message.return_value != Some(false)
                    && self.media_id.is_none()
                {
                    self.media_id = message.media_id.clone();
                }
                if let UriCall::SelectTrack { kind, .. } = &pending.call {
                    message.reply_track = Some(*kind);
                }
                // Bus success is not the method's ACK. Subscription events do not acknowledge a call.
                if let Some(accepted) = message.return_value {
                    pending.ack = Some(reply.ok && accepted);
                }
                if !reply.ok {
                    pending.ack = Some(false);
                }
                // Only an explicit track type+index can attribute a subscription notification.
                // The subscription's request ID says nothing about which command caused it.
                if message.identity_conflict {
                    return;
                }
                if let Some((kind, index)) = message.selected_track {
                    let matches = self.requests.values().filter(|request| matches!(&request.call, UriCall::SelectTrack { kind: expected, index: expected_index, .. } if *expected == kind && *expected_index == index)).count();
                    for request in self.requests.values_mut().filter(|_| matches == 1) {
                        if matches!(&request.call, UriCall::SelectTrack { kind: expected, index: expected_index, .. } if *expected == kind && *expected_index == index)
                        {
                            request.confirmed = true;
                            if request.ack == Some(true) {
                                message.confirmed_track = Some(kind);
                            }
                        }
                    }
                }
                // A matching event may have preceded the ACK.
                if let Some(request) = self.requests.get(&reply.request) {
                    if request.confirmed && request.ack == Some(true) {
                        if let UriCall::SelectTrack { kind, .. } = request.call {
                            message.confirmed_track = Some(kind);
                        }
                    }
                }
                if message.normalized.contains(&Ev::Playing) {
                    self.plane_observed = true;
                }
                fx.push(Effect::Deliver {
                    method,
                    ok: reply.ok,
                    message,
                });
            }
            Event::Close {
                now_ms,
                allowance_ms,
            } => {
                if self.released() {
                    if !self.release_reported {
                        self.release_reported = true;
                        fx.push(Effect::Released);
                    }
                } else if !matches!(self.phase, Phase::Closing | Phase::Quarantined) {
                    self.phase = Phase::Closing;
                    self.deadline = Some(now_ms.saturating_add(allowance_ms));
                    if let Some(session) = self.session {
                        fx.push(Effect::Close { session });
                    }
                    // An outstanding Open still owes its addressed answer. Never release early.
                    else if self.opening.is_none() {
                        self.phase = Phase::Released;
                        self.release_reported = true;
                        fx.push(Effect::Released);
                    }
                }
            }
            Event::PollFailed { session } if self.session == Some(session) => {
                self.phase = Phase::Quarantined;
                self.fault(fx);
            }
            Event::CloseQueued { session, ok: false } if self.session == Some(session) => {
                self.phase = Phase::Quarantined;
                self.fault(fx);
            }
            Event::Status { session, status } if self.session == Some(session) => match status {
                CloseStatus::Released
                    if matches!(self.phase, Phase::Closing | Phase::Quarantined) =>
                {
                    self.phase = Phase::Released;
                    self.plane_observed = false;
                    self.session = None;
                    self.media_id = None;
                    self.requests.clear();
                    self.deadline = None;
                    self.release_reported = true;
                    fx.push(Effect::Released);
                }
                CloseStatus::Quarantined => {
                    self.phase = Phase::Quarantined;
                    self.fault(fx);
                }
                _ => {}
            },
            Event::Tick { now_ms }
                if self.phase == Phase::Closing
                    && self.deadline.is_some_and(|deadline| now_ms >= deadline) =>
            {
                self.phase = Phase::Quarantined;
                self.fault(fx);
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    const SID: SessionId = SessionId::new(1);
    fn step(machine: &mut Lifecycle, event: Event) -> Vec<Effect> {
        let mut fx = Vec::new();
        machine.step(event, &mut fx);
        fx
    }
    fn open() -> Lifecycle {
        let mut m = Lifecycle::new();
        let fx = step(&mut m, Event::Open(Shape::Named));
        let Effect::Open { request, .. } = fx[0] else {
            panic!("open")
        };
        step(
            &mut m,
            Event::OpenAnswered {
                request,
                result: Ok(SID),
            },
        );
        step(
            &mut m,
            Event::Reply(UriReply {
                session: SID,
                request: RequestId::new(0),
                method: "register",
                payload: String::new(),
                ok: true,
            }),
        );
        m
    }
    #[test]
    fn uri_lifecycle_close_is_idempotent_and_deadline_is_not_release() {
        let mut m = open();
        assert_eq!(
            step(
                &mut m,
                Event::Close {
                    now_ms: 10,
                    allowance_ms: 10_000
                }
            ),
            vec![Effect::Close { session: SID }]
        );
        assert!(step(
            &mut m,
            Event::Close {
                now_ms: 20,
                allowance_ms: 10_000
            }
        )
        .is_empty());
        assert!(!m.released());
        assert_eq!(
            step(&mut m, Event::Tick { now_ms: 10_010 }),
            vec![Effect::Fault]
        );
        assert_eq!(m.phase(), Phase::Quarantined);
        assert_eq!(m.session(), Some(SID));
        assert!(step(
            &mut m,
            Event::Status {
                session: SessionId::new(2),
                status: CloseStatus::Released
            }
        )
        .is_empty());
        assert!(!m.released());
        assert_eq!(
            step(
                &mut m,
                Event::Status {
                    session: SID,
                    status: CloseStatus::Released
                }
            ),
            vec![Effect::Released]
        );
        assert!(step(
            &mut m,
            Event::Status {
                session: SID,
                status: CloseStatus::Released
            }
        )
        .is_empty());
    }
    #[test]
    fn uri_lifecycle_close_failure_keeps_ownership_and_refuses_calls() {
        let mut m = open();
        step(
            &mut m,
            Event::Close {
                now_ms: 0,
                allowance_ms: 10_000,
            },
        );
        assert_eq!(
            step(
                &mut m,
                Event::Status {
                    session: SID,
                    status: CloseStatus::Quarantined
                }
            ),
            vec![Effect::Fault]
        );
        assert_eq!(m.session(), Some(SID));
        assert!(!m.released());
        assert!(matches!(
            step(
                &mut m,
                Event::Call(UriCall::Play {
                    media_id: "m".into()
                })
            )[0],
            Effect::Deliver { ok: false, .. }
        ));
        assert!(step(&mut m, Event::Open(Shape::Anon))
            .iter()
            .all(|e| !matches!(e, Effect::Open { .. })));
    }
    #[test]
    fn uri_lifecycle_repeated_methods_are_addressed_independently() {
        let mut m = open();
        let first = step(
            &mut m,
            Event::Call(UriCall::SelectTrack {
                media_id: "m".into(),
                kind: "text",
                index: 0,
            }),
        );
        let second = step(
            &mut m,
            Event::Call(UriCall::SelectTrack {
                media_id: "m".into(),
                kind: "audio",
                index: 1,
            }),
        );
        let Effect::Call { request: a, .. } = first[0] else {
            panic!()
        };
        let Effect::Call { request: b, .. } = second[0] else {
            panic!()
        };
        assert_ne!(a, b);
        for (request, kind) in [(b, "audio"), (a, "text")] {
            let fx = step(
                &mut m,
                Event::Reply(UriReply {
                    session: SID,
                    request,
                    method: "selectTrack",
                    payload: r#"{"returnValue":true}"#.into(),
                    ok: true,
                }),
            );
            assert!(
                matches!(&fx[0], Effect::Deliver { message, .. } if message.reply_track==Some(kind) && message.confirmed_track.is_none())
            );
        }
        assert!(step(
            &mut m,
            Event::Reply(UriReply {
                session: SessionId::new(99),
                request: a,
                method: "selectTrack",
                payload: r#"{"returnValue":false}"#.into(),
                ok: true
            })
        )
        .is_empty());
    }
    #[test]
    fn uri_lifecycle_overflow_fails_session_without_releasing_it() {
        let mut m = open();
        let fx = step(
            &mut m,
            Event::Reply(UriReply {
                session: SID,
                request: RequestId::new(0),
                method: "dropped",
                payload: "2".into(),
                ok: false,
            }),
        );
        assert_eq!(fx, vec![Effect::Fault]);
        assert!(!m.released());
    }
    #[test]
    fn uri_lifecycle_pending_open_close_waits_for_answer_then_closes_returned_session() {
        let mut m = Lifecycle::new();
        let fx = step(&mut m, Event::Open(Shape::Named));
        let Effect::Open { request, .. } = fx[0] else {
            panic!()
        };
        assert!(step(
            &mut m,
            Event::Close {
                now_ms: 0,
                allowance_ms: 10_000
            }
        )
        .is_empty());
        assert!(!m.released());
        assert_eq!(
            step(
                &mut m,
                Event::OpenAnswered {
                    request,
                    result: Ok(SID)
                }
            ),
            vec![Effect::Close { session: SID }]
        );
        assert!(!m.released());
        assert_eq!(
            step(
                &mut m,
                Event::Status {
                    session: SID,
                    status: CloseStatus::Released
                }
            ),
            vec![Effect::Released]
        );
    }
    fn call_request(m: &mut Lifecycle, call: UriCall) -> RequestId {
        let fx = step(m, Event::Call(call));
        let Effect::Call { request, .. } = fx[0] else {
            panic!()
        };
        request
    }
    fn reply(
        m: &mut Lifecycle,
        request: RequestId,
        method: &'static str,
        payload: &str,
    ) -> Vec<Effect> {
        step(
            m,
            Event::Reply(UriReply {
                session: SID,
                request,
                method,
                payload: payload.into(),
                ok: true,
            }),
        )
    }
    #[test]
    fn uri_lifecycle_wrong_media_identity_cannot_drive_plane_or_acknowledge_requests() {
        let mut m = open();
        let load = call_request(&mut m, UriCall::Load { window_id: None });
        reply(
            &mut m,
            load,
            "load",
            r#"{"returnValue":true,"mediaId":"owner"}"#,
        );
        let sub = call_request(
            &mut m,
            UriCall::Subscribe {
                media_id: "owner".into(),
            },
        );
        assert!(reply(
            &mut m,
            sub,
            "subscribe",
            r#"{"mediaId":"other","playing":true}"#
        )
        .is_empty());
        assert!(!m.plane_open());
        reply(
            &mut m,
            sub,
            "subscribe",
            r#"{"mediaId":"owner","playing":true}"#,
        );
        assert!(m.plane_open());
        step(
            &mut m,
            Event::Close {
                now_ms: 0,
                allowance_ms: 10_000,
            },
        );
        assert!(
            m.plane_open(),
            "logical stop cannot retract unresolved plane ownership"
        );
        step(
            &mut m,
            Event::Status {
                session: SID,
                status: CloseStatus::Released,
            },
        );
        assert!(!m.plane_open());
    }
    #[test]
    fn uri_lifecycle_generic_seek_and_track_events_cannot_identify_commands() {
        let mut m = open();
        let sub = call_request(
            &mut m,
            UriCall::Subscribe {
                media_id: "m".into(),
            },
        );
        let track = call_request(
            &mut m,
            UriCall::SelectTrack {
                media_id: "m".into(),
                kind: "text",
                index: 0,
            },
        );
        reply(&mut m, track, "selectTrack", r#"{"returnValue":true}"#);
        let fx = reply(
            &mut m,
            sub,
            "subscribe",
            r#"{"trackSelected":true,"seekDone":true}"#,
        );
        assert!(matches!(&fx[0],Effect::Deliver{message,..} if message.confirmed_track.is_none()));
        let fx = reply(
            &mut m,
            sub,
            "subscribe",
            r#"{"trackSelected":{"type":"text","index":0}}"#,
        );
        assert!(
            matches!(&fx[0],Effect::Deliver{message,..} if message.confirmed_track==Some("text"))
        );
        let repeated = call_request(
            &mut m,
            UriCall::SelectTrack {
                media_id: "m".into(),
                kind: "text",
                index: 0,
            },
        );
        reply(&mut m, repeated, "selectTrack", r#"{"returnValue":true}"#);
        let fx = reply(
            &mut m,
            sub,
            "subscribe",
            r#"{"trackSelected":{"type":"text","index":0}}"#,
        );
        assert!(
            matches!(&fx[0],Effect::Deliver{message,..} if message.confirmed_track.is_none()),
            "historical same-target commands make attribution ambiguous"
        );
    }
}
