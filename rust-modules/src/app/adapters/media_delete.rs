//! Resource runner for an explicitly confirmed deletion. One operation at a time, no retries.
use plx_machine::machine::MachineId;
use plx_plex::plex::{self, ClientKey, DeleteResult};
use plx_screens::registry::MediaDeleteRequest;
use std::sync::mpsc::{self, Receiver, TryRecvError};

#[derive(Default)]
pub(crate) struct MediaDeleteRunner {
    pending: Option<(MachineId, MediaDeleteRequest, Receiver<DeleteResult>)>,
}
impl MediaDeleteRunner {
    pub fn start(&mut self, from: MachineId, req: MediaDeleteRequest) -> Option<DeleteResult> {
        if self.pending.is_some() { return Some(DeleteResult::Unavailable); }
        let profile = plex::session::current_snapshot();
        let Some(session) = plex::session::peek_settled() else { return Some(DeleteResult::IdentityChanged) };
        if profile.generation != req.profile || !session.active_profile_is_admin()
            || session.account_token.is_empty()
            || !plex::server_facts(req.sid).is_some_and(|f| f.owned) {
            return Some(DeleteResult::Forbidden);
        }
        let Some(client) = plex::client_for(req.sid).filter(|c| ClientKey::of(c) == req.key) else {
            return Some(DeleteResult::IdentityChanged);
        };
        let (send, recv) = mpsc::channel();
        let target = req.clone();
        if !plx_base::task::spawn_small("media-delete", move || {
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                client.delete_metadata(&target.rk, target.key)
            })).unwrap_or(DeleteResult::Unavailable);
            let _ = send.send(result);
            plx_machine::present::wake_from_worker();
        }) { return Some(DeleteResult::Unavailable); }
        self.pending = Some((from, req, recv));
        None
    }
    pub fn poll(&mut self) -> Option<(MachineId, MediaDeleteRequest, DeleteResult)> {
        let (_, _, recv) = self.pending.as_ref()?;
        let result = match recv.try_recv() {
            Ok(result) => result,
            Err(TryRecvError::Empty) => return None,
            Err(TryRecvError::Disconnected) => DeleteResult::Unavailable,
        };
        let (to, req, _) = self.pending.take()?;
        Some((to, req, result))
    }
}
