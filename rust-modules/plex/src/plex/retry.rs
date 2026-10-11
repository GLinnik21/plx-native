//! **What a server's reachability retry is made of**: the hub fetch's backoff ladder and the
//! advisory request to re-discover a server's endpoint.
//!
//! Both began in the data layer (`pms::backoff_secs`, `stores::EndpointRefresh`), but the plaintext
//! grant's upgrade retry ([`super::grant::UpgradeRetry`]) steps the same ladder and answers with the
//! same request set, so a plex module could not name either one without naming upward. They live
//! here now and `pms` / `stores` re-export them: the data layer's own spelling is unchanged.

use super::ServerId;

/// The backoff ladder's ends. A TV parked on a sleeping server must keep trying — that IS the
/// feature — without ever becoming a request loop, so the wait doubles from `MIN` to a `MAX`
/// that still recovers within half a minute of the server coming back.
pub const RETRY_MIN_S: f32 = 2.0;
pub const RETRY_MAX_S: f32 = 30.0;

/// Wait before attempt `fails + 1`: 2s, 4s, 8s, 16s, then 30s forever. Pure — host-tested.
pub fn backoff_secs(fails: u32) -> f32 {
    super::account::backoff(fails.saturating_sub(1),
        std::time::Duration::from_secs_f32(RETRY_MIN_S),
        std::time::Duration::from_secs_f32(RETRY_MAX_S)).as_secs_f32()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EndpointRefresh { pub sid: ServerId }

/// Advisory requests in first-observation order. Invalid IDs are rejected, never remapped.
///
/// A `Vec`, not a fixed array: the set holds one entry per server that needs a refresh, and the
/// registry issues slot numbers past any fixed size. It is bounded by the number of servers, never
/// by a ceiling of its own.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
#[must_use]
pub struct EndpointRefreshSet {
    ids: Vec<ServerId>,
}

impl EndpointRefreshSet {
    pub fn insert(&mut self, request: EndpointRefresh) -> bool {
        if !request.sid.is_set() || self.ids.contains(&request.sid) { return false; }
        self.ids.push(request.sid);
        true
    }
    pub fn is_empty(&self) -> bool { self.ids.is_empty() }
    pub fn merge(&mut self, other: Self) {
        for request in other.iter() { self.insert(request); }
    }
    pub fn iter(&self) -> impl Iterator<Item = EndpointRefresh> + '_ {
        self.ids.iter().map(|&sid| EndpointRefresh { sid })
    }
    /// Push as many refreshes as this step has room for and hand back the rest in order, which
    /// the caller carries into its next step. `MAX_EMIT_PER_STEP` is a work bound on one step, so
    /// it is the emitter that yields to it: nothing is dropped, and a roster of any size drains.
    pub fn emit<H: EndpointRefreshHost>(mut self, fx: &mut plx_machine::machine::Effects<'_, H>) -> Self {
        let room = fx.remaining() as usize;
        let rest = self.ids.split_off(room.min(self.ids.len()));
        for request in self.iter() { fx.push(plx_machine::machine::Fx::App(H::endpoint_refresh(request))); }
        Self { ids: rest }
    }
}

/// A host that can turn one [`EndpointRefresh`] into its own effect, which is what
/// [`EndpointRefreshSet::emit`] needs of it. The app's `AppHost` implements it; `stores` re-exports
/// it as `StoreEffectHost`, the name its machines are written against.
pub trait EndpointRefreshHost: plx_machine::machine::Host {
    fn endpoint_refresh(request: EndpointRefresh) -> Self::Fx;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn endpoint_sets_preserve_first_observation_order_and_capacity() {
        let request = |id| EndpointRefresh { sid: ServerId::from_raw(id) };
        let mut first = EndpointRefreshSet::default();
        assert_eq!(first.iter().count(), 0);
        assert!(!first.insert(request(u16::MAX)));
        // A server past the old sixteen-slot table is a real request, not an invalid id.
        assert!(first.insert(request(40)));
        first.insert(request(3)); first.insert(request(1)); first.insert(request(3));
        let mut second = EndpointRefreshSet::default();
        second.insert(request(1)); second.insert(request(2)); second.insert(request(0));
        first.merge(second);
        assert_eq!(first.iter().map(|r| r.sid.raw()).collect::<Vec<_>>(), [40, 3, 1, 2, 0]);
        for id in 0..16 { first.insert(request(id)); }
        assert_eq!(first.iter().count(), 17);
        let copy = first.clone();
        first.merge(copy);
        assert_eq!(first.iter().count(), 17);
    }

    #[test]
    fn the_backoff_doubles_then_holds_at_the_ceiling() {
        assert_eq!(
            backoff_secs(1),
            RETRY_MIN_S,
            "the first retry is the shortest wait"
        );
        assert_eq!(backoff_secs(2), 4.0);
        assert_eq!(backoff_secs(3), 8.0);
        assert_eq!(backoff_secs(4), 16.0);
        assert_eq!(backoff_secs(5), RETRY_MAX_S, "32s is past the ceiling");
        assert_eq!(
            backoff_secs(99),
            RETRY_MAX_S,
            "and it never grows past it (nor overflows)"
        );
    }

    struct EmitHost;
    impl plx_machine::machine::Host for EmitHost {
        type Arg = plx_machine::machine::BareArg;
        type Fx = ServerId;
        type Msg = ();
        type Elem = u32;
        type Views<'a> = ();
        type Init = ();
        type Memory = ();
    }
    impl EndpointRefreshHost for EmitHost {
        fn endpoint_refresh(request: EndpointRefresh) -> ServerId { request.sid }
    }

    /// The work bound on one machine step stays; the emitter, not the bound, yields to it. Forty
    /// servers failing in one step are all refreshed across steps, none dropped.
    #[test]
    fn forty_failing_servers_are_all_refreshed_without_overrunning_one_step() {
        use plx_machine::machine::{Effects, Fx, MachineId, Stamped, MAX_EMIT_PER_STEP};
        let mut set = EndpointRefreshSet::default();
        for id in 0..40 { set.insert(EndpointRefresh { sid: ServerId::from_raw(id) }); }
        let mut present = plx_machine::present::Present::new();
        let mut seen = Vec::new();
        let mut steps = 0;
        while set.iter().next().is_some() {
            let mut buf: Vec<Stamped<EmitHost>> = Vec::new();
            let mut fx = Effects::new(&mut buf, MachineId::Nav, &mut present);
            set = set.emit(&mut fx);
            drop(fx);
            assert!(buf.len() as u32 <= MAX_EMIT_PER_STEP);
            seen.extend(buf.into_iter().map(|s| match s.fx { Fx::App(sid) => sid.raw(), _ => unreachable!() }));
            steps += 1;
            assert!(steps < 100, "the carry never drains");
        }
        assert_eq!(seen, (0..40).collect::<Vec<u16>>());
    }
}
