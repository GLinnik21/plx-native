//! IO-boundary observation of addressed Plex/platform facts, never an ambient damage queue.
use plx_machine::present::{Present, PresentEvent, Provenance, Scope};
use plx_plex::plex::{self, ServerId};

#[derive(Debug, PartialEq, Eq)]
struct ServerVisualInput {
    id: ServerId,
    facts: Option<&'static plex::ServerFacts>,
    info: plex::serverinfo::ServerInfoFacts,
    probe: Option<plex::probe::Outcome>,
    grant_epoch: u32,
    token_generation: u32,
    link: Option<plex::probe::Location>,
    ip: Option<plex::IpVersion>,
}

/// Actual domain values: immutable roster descriptions are borrowed, version strings Arc-cloned.
/// No lossy hashes, dirty generations, or worker-owned UI provenance.
#[derive(Debug, PartialEq, Eq)]
pub(super) struct PlexVisualInputs {
    current: ServerId,
    roster: u32,
    facts: u32,
    servers: Vec<ServerVisualInput>,
    preferences: Vec<u16>,
    collections: Vec<((u16, u32, i64), Option<u32>)>,
    dv: plx_platform::devcaps::dv::DvProbe,
}

impl PlexVisualInputs {
    fn capture() -> Self {
        let servers = plex::server_ids().filter_map(|id| {
            let client = plex::client_for(id)?;
            Some(ServerVisualInput {
                id, facts: plex::server_facts(id), info: plex::serverinfo::facts_of(id),
                probe: plex::server_probe_result(id), grant_epoch: client.grant_epoch(),
                token_generation: client.token_gen(), link: client.link(), ip: client.ip_version(),
            })
        }).collect();
        Self { current: plex::current_server(), roster: plex::server_roster_gen(),
            facts: plex::server_facts_gen(), servers,
            collections: crate::app::collections_probe::visual_facts(),
            preferences: plx_media::route::preference_values(), dv: plx_platform::devcaps::dv::probe() }
    }
}

impl super::Bridge {
    /// Called after result/input landings and before shot preparation/admission. These facts can
    /// change the host underneath a surface, so their observation always belongs to Page scope.
    pub(crate) fn observe_plex_visual_inputs(&mut self, present: &mut Present) {
        if self.home_io.is_some() { return; } // controlled inputs must not poll ambient Plex state
        let next = PlexVisualInputs::capture();
        if self.plex_visual_inputs.as_ref() != Some(&next) {
            present.note_in_scope(Scope::Page, PresentEvent::Damage(Provenance::Lifecycle));
            self.plex_visual_inputs = Some(next);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn addressed_probe_arrivals_damage_only_the_observing_owned_gate() {
        let _g = plx_base::testlock::serial();
        plex::reset_servers_for_test();
        let mut bridge = super::super::Bridge::for_visual_inputs_test();
        let mut gate = Present::new();
        bridge.observe_plex_visual_inputs(&mut gate);
        gate.take(0);
        gate.acknowledge(0);
        let id = plex::register_for_test("visual-observer", "127.0.0.1", 32400, "tok", "fixture");
        assert_ne!(id, ServerId::UNSET);
        bridge.observe_plex_visual_inputs(&mut gate);
        assert!(gate.take(1));
        gate.acknowledge(1);
        plex::publish_probe_result(id, plex::probe::Outcome::Unauthorized);
        assert!(!gate.take(2), "worker publication alone has no UI authority");
        gate.set_scope(Scope::Surface);
        bridge.observe_plex_visual_inputs(&mut gate);
        assert!(gate.page_damaged(), "server facts invalidate the host beneath a surface");
        assert!(gate.take(2), "the addressed outcome repaints before keepalive");
        gate.acknowledge(2);
        bridge.observe_plex_visual_inputs(&mut gate);
        assert!(!gate.take(3), "unchanged inputs do not continuously repaint");
        plex::reset_servers_for_test();
    }
}
