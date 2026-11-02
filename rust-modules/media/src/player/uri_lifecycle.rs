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
    AcbTouched { session: SessionId },
    AcbUnloadDispatched { session: SessionId },
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
pub enum Fx {
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
    UnloadAcb { session: SessionId },
    AcbQuarantined,
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
    acb_touched: bool,
    acb_unload_requested: bool,
    bus_released: bool,
    acb_tail_done: bool,
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
            acb_touched: false,
            acb_unload_requested: false,
            bus_released: false,
            acb_tail_done: false,
        }
    }
    pub fn poll_session(&self) -> Option<SessionId> { self.session.filter(|_| !self.bus_released) }
    pub fn acb_exit_quarantine(&self) -> bool { self.acb_touched && self.bus_released && self.acb_tail_done && self.phase == Phase::Quarantined }
    pub fn acb_tail_due(&self, session: SessionId) -> bool {
        self.session == Some(session) && self.bus_released && self.acb_unload_requested && !self.acb_tail_done
    }
    pub fn plane_open(&self) -> bool {
        self.plane_observed || self.acb_touched
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
    fn fault(&mut self, fx: &mut Vec<Fx>) {
        if !self.faulted {
            self.faulted = true;
            fx.push(Fx::Fault);
        }
    }
    pub fn step(&mut self, event: Event, fx: &mut Vec<Fx>) {
        match event {
            Event::Open(shape) if self.released() => {
                let request = self.request();
                self.opening = Some(request);
                self.phase = Phase::Opening;
                self.faulted = false;
                self.release_reported = false;
                fx.push(Fx::Open { request, shape });
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
                            fx.push(Fx::Close { session });
                        }
                    }
                    Err(message) => {
                        let closing = matches!(self.phase, Phase::Closing | Phase::Quarantined);
                        self.phase = Phase::Released;
                        if closing {
                            self.release_reported = true;
                            fx.push(Fx::Released);
                        } else {
                            fx.push(Fx::Deliver {
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
                fx.push(Fx::Call {
                    session,
                    request,
                    call,
                });
            }
            Event::Call(call) => fx.push(Fx::Deliver {
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
                    fx.push(Fx::Deliver {
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
                    fx.push(Fx::Deliver {
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
                if message.acb_source.as_ref().is_some_and(|source| self.media_id.as_ref() != Some(&source.media_id)) { message.acb_source = None; message.video_info = None; }
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
                fx.push(Fx::Deliver {
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
                        fx.push(Fx::Released);
                    }
                } else if !matches!(self.phase, Phase::Closing | Phase::Quarantined) {
                    self.phase = Phase::Closing;
                    self.deadline = Some(now_ms.saturating_add(allowance_ms));
                    if let Some(session) = self.session {
                        fx.push(Fx::Close { session });
                    }
                    // An outstanding Open still owes its addressed answer. Never release early.
                    else if self.opening.is_none() {
                        self.phase = Phase::Released;
                        self.release_reported = true;
                        fx.push(Fx::Released);
                    }
                }
            }
            Event::AcbTouched {session} if self.session == Some(session) && self.phase == Phase::Open => { self.acb_touched = true; }
            Event::AcbUnloadDispatched {session} if self.session == Some(session) && self.bus_released && self.acb_unload_requested && !self.acb_tail_done => {
                self.acb_tail_done = true;
                self.phase = Phase::Quarantined;
                fx.push(Fx::AcbQuarantined);
            }
            Event::PollFailed { session } if self.session == Some(session) && !self.bus_released => {
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
                    if self.acb_touched {
                        self.bus_released = true;
                        self.deadline = None;
                        if !self.acb_unload_requested {
                            self.acb_unload_requested = true;
                            fx.push(Fx::UnloadAcb {session});
                        }
                        return;
                    }
                    self.phase = Phase::Released;
                    self.plane_observed = false;
                    self.session = None;
                    self.media_id = None;
                    self.requests.clear();
                    self.deadline = None;
                    self.release_reported = true;
                    fx.push(Fx::Released);
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
    fn step(machine: &mut Lifecycle, event: Event) -> Vec<Fx> {
        let mut fx = Vec::new();
        machine.step(event, &mut fx);
        fx
    }
    fn open() -> Lifecycle {
        let mut m = Lifecycle::new();
        let fx = step(&mut m, Event::Open(Shape::Named));
        let Fx::Open { request, .. } = fx[0] else {
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
            vec![Fx::Close { session: SID }]
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
            vec![Fx::Fault]
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
            vec![Fx::Released]
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
            vec![Fx::Fault]
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
            Fx::Deliver { ok: false, .. }
        ));
        assert!(step(&mut m, Event::Open(Shape::Anon))
            .iter()
            .all(|e| !matches!(e, Fx::Open { .. })));
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
        let Fx::Call { request: a, .. } = first[0] else {
            panic!()
        };
        let Fx::Call { request: b, .. } = second[0] else {
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
                matches!(&fx[0], Fx::Deliver { message, .. } if message.reply_track==Some(kind) && message.confirmed_track.is_none())
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
        assert_eq!(fx, vec![Fx::Fault]);
        assert!(!m.released());
    }
    #[test]
    fn uri_lifecycle_pending_open_close_waits_for_answer_then_closes_returned_session() {
        let mut m = Lifecycle::new();
        let fx = step(&mut m, Event::Open(Shape::Named));
        let Fx::Open { request, .. } = fx[0] else {
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
            vec![Fx::Close { session: SID }]
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
            vec![Fx::Released]
        );
    }
    fn call_request(m: &mut Lifecycle, call: UriCall) -> RequestId {
        let fx = step(m, Event::Call(call));
        let Fx::Call { request, .. } = fx[0] else {
            panic!()
        };
        request
    }
    fn reply(
        m: &mut Lifecycle,
        request: RequestId,
        method: &'static str,
        payload: &str,
    ) -> Vec<Fx> {
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
        assert!(matches!(&fx[0],Fx::Deliver{message,..} if message.confirmed_track.is_none()));
        let fx = reply(
            &mut m,
            sub,
            "subscribe",
            r#"{"trackSelected":{"type":"text","index":0}}"#,
        );
        assert!(
            matches!(&fx[0],Fx::Deliver{message,..} if message.confirmed_track==Some("text"))
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
            matches!(&fx[0],Fx::Deliver{message,..} if message.confirmed_track.is_none()),
            "historical same-target commands make attribution ambiguous"
        );
    }
    #[test]
    fn uri_review_regression_wrong_media_refusal_cannot_affect_owner() {
        let mut m = open();
        let load = call_request(&mut m, UriCall::Load { window_id: None });
        reply(
            &mut m,
            load,
            "load",
            r#"{"returnValue":true,"mediaId":"owner"}"#,
        );
        let play = call_request(
            &mut m,
            UriCall::Play {
                media_id: "owner".into(),
            },
        );
        let wrong = reply(
            &mut m,
            play,
            "play",
            r#"{"returnValue":false,"mediaId":"other"}"#,
        );
        assert!(
            wrong.is_empty(),
            "another media's refusal reached the owner: {wrong:?}"
        );
        let own = reply(
            &mut m,
            play,
            "play",
            r#"{"returnValue":false,"mediaId":"owner"}"#,
        );
        assert!(
            matches!(&own[..], [Fx::Deliver { method: Method::Play, message, .. }] if message.accepted() == Some(false))
        );
    }
    #[test]
    fn uri_acb_bus_release_runs_unloaded_once_then_retains_plane_quarantine() {
        for overdue in [false,true] {
            let mut m=open();
            step(&mut m,Event::AcbTouched{session:SID});
            step(&mut m,Event::Close{now_ms:0,allowance_ms:10_000});
            if overdue { step(&mut m,Event::Tick{now_ms:10_001}); }
            assert_eq!(step(&mut m,Event::Status{session:SID,status:CloseStatus::Released}),[Fx::UnloadAcb{session:SID}]);
            assert!(m.acb_tail_due(SID)); assert!(!m.released()); assert!(m.plane_open());
            assert!(step(&mut m,Event::Status{session:SID,status:CloseStatus::Released}).is_empty());
            assert_eq!(step(&mut m,Event::AcbUnloadDispatched{session:SID}),[Fx::AcbQuarantined]);
            assert!(step(&mut m,Event::AcbUnloadDispatched{session:SID}).is_empty());
            assert_eq!(m.phase(),Phase::Quarantined); assert!(!m.released()); assert!(!m.acb_tail_due(SID));
            assert!(m.acb_exit_quarantine()); assert!(m.poll_session().is_none());
            assert!(step(&mut m,Event::Tick{now_ms:100_000}).is_empty());
            assert!(step(&mut m,Event::Status{session:SID,status:CloseStatus::Released}).is_empty());
            assert!(step(&mut m,Event::Open(Shape::Anon)).iter().all(|fx|!matches!(fx,Fx::Open{..})));
        }
    }

    #[test]
    fn uri_acb_review_other_video_info_never_reaches_waiting_owner() {
        let mut m=open();
        let load=call_request(&mut m,UriCall::Load{window_id:None});
        reply(&mut m,load,"load",r#"{"returnValue":true,"mediaId":"owner"}"#);
        let sub=call_request(&mut m,UriCall::Subscribe{media_id:"owner".into()});
        for payload in [
            r#"{"videoInfo":{"mediaId":"other","width":1280,"height":720,"frameRate":24}}"#,
            r#"{"mediaId":"owner","videoInfo":{"mediaId":"other","width":1280,"height":720,"frameRate":24}}"#,
            r#"{"mediaId":"owner","videoInfo":{"mediaId":42,"width":1280,"height":720,"frameRate":24}}"#,
        ] {
            assert!(reply(&mut m,sub,"subscribe",payload).is_empty(),"foreign facts reached owner: {payload}");
        }
    }

}
