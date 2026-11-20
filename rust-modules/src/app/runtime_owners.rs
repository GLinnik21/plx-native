//! Application-owned logical children and their resource executors.
//!
//! This tree is not a dispatcher rig. A rig may borrow the individual children for a turn,
//! but cannot construct, replace or retain an application owner through a publication.

use super::adapters;
use super::bridge::{Bridge, BridgeRetained, ConsentMachine};

pub(crate) struct SessionOwner {
    pub(super) machine: plx_session::auth::SessionMachine,
    pub(super) adapter: adapters::session::SessionAdapter,
}

impl SessionOwner {
    pub(super) fn new(
        initial: plx_session::auth::SessionInit,
        adapter: adapters::session::SessionAdapter,
    ) -> Self {
        Self { machine: plx_session::auth::SessionMachine::from_init(initial), adapter }
    }
}

impl Drop for SessionOwner {
    fn drop(&mut self) {
        self.adapter.cancel_all();
    }
}

pub(crate) struct ConsentOwner {
    pub(super) machine: ConsentMachine,
    pub(super) adapter: adapters::consent::ConsentAdapter,
}

impl ConsentOwner {
    pub(super) fn new(
        initial: plx_telemetry::telemetry::consent::Consent,
        adapter: adapters::consent::ConsentAdapter,
    ) -> Self {
        Self { machine: ConsentMachine::from_initial(initial), adapter }
    }
}

pub(crate) struct StoreOwners {
    pub(super) machines: plx_data::stores::Stores,
    pub(super) runner: adapters::stores::StoreRunner,
}

/// Owns the logical HUD store and its addressed resource executor for one App.
pub(crate) struct HudRuntimeOwner {
    pub(super) driver: adapters::huds::HudDriver,
}

pub(crate) struct MounterIo {
    pub(super) mounter: plx_screens::registry::AppMounter,
}

/// Physical children of one App. No Rc, shared mutable aggregate, or rig Deref.
/// Playback's machine and native adapter remain App.player and App.adapters.player.
pub(crate) struct RuntimeOwners {
    pub(super) session: SessionOwner,
    pub(super) consent: ConsentOwner,
    pub(super) stores: StoreOwners,
    pub(super) mounting: MounterIo,
    pub(super) hud: HudRuntimeOwner,
}

impl RuntimeOwners {
    pub(super) fn new(
        session: plx_session::auth::SessionInit,
        session_adapter: adapters::session::SessionAdapter,
        consent: plx_telemetry::telemetry::consent::Consent,
        consent_adapter: adapters::consent::ConsentAdapter,
        stores: plx_data::stores::Stores,
        runner: adapters::stores::StoreRunner,
    ) -> Self {
        Self {
            session: SessionOwner::new(session, session_adapter),
            consent: ConsentOwner::new(consent, consent_adapter),
            stores: StoreOwners { machines: stores, runner },
            mounting: MounterIo { mounter: Default::default() },
            hud: HudRuntimeOwner { driver: adapters::huds::HudDriver::off() },
        }
    }

    pub(crate) fn metadata_view(&self) -> plx_data::metadata::MetadataView<'_> {
        self.stores.machines.metadata_view()
    }

    pub(crate) fn metadata_mut(&mut self) -> &mut plx_data::stores::metadata::MetadataStore {
        &mut self.stores.machines.metadata
    }

    pub(crate) fn person_view(&self) -> plx_data::person::PersonView<'_> {
        self.stores.machines.person_view()
    }

    pub(crate) fn auth_read(&self) -> plx_session::auth::SessionRead<'_> {
        self.session.machine.read()
    }

    #[cfg(all(feature = "hostsim", feature = "devtriggers"))]
    pub(crate) fn landgate(&self) -> &plx_machine::landgate::Gate {
        &self.stores.machines.landgate
    }

    #[cfg(all(feature = "hostsim", feature = "devtriggers"))]
    pub(crate) fn open_claims(&self) -> Vec<&'static str> {
        use plx_data::stores::StoreId;
        let mut open = Vec::new();
        if self.stores.machines.hubs.owed() { open.push(StoreId::Hubs.name()); }
        if self.stores.machines.browse.borrow().discovery_owed() { open.push(StoreId::Browse.name()); }
        open
    }

    /// Disjoint mutable loans end with this turn; no owner is moved into the capability.
    pub(crate) fn bridge<'a>(&'a mut self, retained: &'a mut BridgeRetained) -> Bridge<'a> {
        Bridge {
            session: &mut self.session.machine,
            session_adapter: &mut self.session.adapter,
            consent: &mut self.consent.machine,
            consent_adapter: &mut self.consent.adapter,
            stores: &mut self.stores.machines,
            store_runner: &mut self.stores.runner,
            mounter: &mut self.mounting.mounter,
            huds: &mut self.hud.driver,
            retained,
        }
    }
}

/// Host fixtures own the same children as App and lend the same capability.
/// Deliberately no Deref or borrowed-value-returning constructor compatibility.
#[cfg(test)]
pub(crate) struct Harness {
    runtime: RuntimeOwners,
    retained: BridgeRetained,
}

#[cfg(test)]
impl Harness {
    pub(crate) fn new(now_us: fn() -> u64) -> Self {
        let (runtime, retained) = BridgeRetained::for_test(now_us);
        Self { runtime, retained }
    }

    pub(crate) fn from_parts(runtime: RuntimeOwners, retained: BridgeRetained) -> Self {
        Self { runtime, retained }
    }

    pub(crate) fn bridge(&mut self) -> Bridge<'_> {
        self.runtime.bridge(&mut self.retained)
    }

    pub(crate) fn with_bridge<T>(&mut self, turn: impl FnOnce(&mut Bridge<'_>) -> T) -> T {
        turn(&mut self.bridge())
    }

    pub(crate) fn stores(&self) -> &plx_data::stores::Stores {
        &self.runtime.stores.machines
    }

    pub(crate) fn huds(&self) -> &adapters::huds::HudDriver {
        &self.runtime.hud.driver
    }

    pub(crate) fn retained(&self) -> &BridgeRetained {
        &self.retained
    }

    pub(crate) fn metadata_view(&self) -> plx_data::metadata::MetadataView<'_> {
        self.runtime.metadata_view()
    }

    pub(crate) fn metadata_mut(&mut self) -> &mut plx_data::stores::metadata::MetadataStore {
        self.runtime.metadata_mut()
    }

    pub(crate) fn person_view(&self) -> plx_data::person::PersonView<'_> {
        self.runtime.person_view()
    }

    pub(crate) fn auth_read(&self) -> plx_session::auth::SessionRead<'_> {
        self.runtime.auth_read()
    }

    pub(crate) fn views(&self) -> super::bridge::AppViews<'_> {
        self.retained.fixture_views(&self.runtime.session.machine, &self.runtime.stores.machines)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_turn_lends_the_actual_nine_physical_fields() {
        let _serial = plx_base::testlock::serial();
        let (mut runtime, mut retained) = BridgeRetained::for_test(|| 0);
        let session = std::ptr::from_ref(&runtime.session.machine);
        let session_adapter = std::ptr::from_ref(&runtime.session.adapter);
        let consent = std::ptr::from_ref(&runtime.consent.machine);
        let consent_adapter = std::ptr::from_ref(&runtime.consent.adapter);
        let stores = std::ptr::from_ref(&runtime.stores.machines);
        let runner = std::ptr::from_ref(&runtime.stores.runner);
        let mounter = std::ptr::from_ref(&runtime.mounting.mounter);
        let hud = std::ptr::from_ref(&runtime.hud.driver);
        let publications = std::ptr::from_ref(&retained);
        {
            let mut turn = runtime.bridge(&mut retained);
            assert!(std::ptr::eq(session, &*turn.session));
            assert!(std::ptr::eq(session_adapter, &*turn.session_adapter));
            assert!(std::ptr::eq(consent, &*turn.consent));
            assert!(std::ptr::eq(consent_adapter, &*turn.consent_adapter));
            assert!(std::ptr::eq(stores, &*turn.stores));
            assert!(std::ptr::eq(runner, &*turn.store_runner));
            assert!(std::ptr::eq(mounter, &*turn.mounter));
            assert!(std::ptr::eq(hud, &*turn.huds));
            assert!(std::ptr::eq(publications, &*turn.retained));
            turn.publish_today(Some([2026, 11, 16]));
        }
        assert_eq!(runtime.bridge(&mut retained).views().today, Some([2026, 11, 16]));
        assert!(std::ptr::eq(session, &runtime.session.machine));
        assert!(std::ptr::eq(stores, &runtime.stores.machines));
    }

    #[test]
    fn two_hud_runtime_owners_keep_selection_and_reset_independent() {
        let _serial = plx_base::testlock::serial();
        let (mut first, mut first_retained) = BridgeRetained::for_test(|| 0);
        let (mut second, mut second_retained) = BridgeRetained::for_test(|| 0);
        let root = std::env::temp_dir().join(format!("plx-runtime-hud-{}", std::process::id()));
        let classic = || plx_appkit::player_hud::loaded::load_classic(
            "0.10.0", plx_os::i18n::english(), &plx_ui::fixture::FixtureMeasure);
        first.hud.driver = adapters::huds::HudDriver::started_in(root.join("first"), classic(), "0.10.0");
        second.hud.driver = adapters::huds::HudDriver::started_in(root.join("second"), classic(), "0.10.0");
        first.hud.driver.refresh("first-choice".into());
        second.hud.driver.refresh("second-choice".into());
        let second_revision = second.hud.driver.revision();
        let second_store = std::ptr::from_ref(second.hud.driver.store());
        let second_catalog = second.hud.driver.store().catalog().clone();
        let compiled = std::sync::Arc::clone(&second.mounting.mounter.player_compiled);
        {
            let driver = std::ptr::from_ref(&first.hud.driver);
            let turn = first.bridge(&mut first_retained);
            assert!(std::ptr::eq(driver, &*turn.huds));
            turn.huds.select(plx_data::hudpacks::BUILTIN_CLASSIC.into());
            assert_eq!(turn.huds.store().selected(), plx_data::hudpacks::BUILTIN_CLASSIC);
        } // Dropping a transient loan must not replace or cancel its App child.
        assert_eq!(second.hud.driver.store().selected(), "second-choice");
        assert_eq!(second.hud.driver.revision(), second_revision);
        assert_eq!(second.hud.driver.store().catalog(), &second_catalog);
        first.hud = HudRuntimeOwner { driver: adapters::huds::HudDriver::off() };
        assert!(!first.hud.driver.started());
        assert!(second.hud.driver.started());
        let turn = second.bridge(&mut second_retained);
        assert!(std::ptr::eq(second_store, turn.huds.store()));
        assert!(std::sync::Arc::ptr_eq(&compiled, &turn.mounter.player_compiled));
        assert_eq!(turn.huds.store().selected(), "second-choice");
    }

    #[test]
    fn two_owned_harnesses_keep_publications_and_child_resets_independent() {
        let _serial = plx_base::testlock::serial();
        let mut first = Harness::new(|| 0);
        let mut second = Harness::new(|| 0);
        first.with_bridge(|bridge| bridge.publish_today(Some([2026, 11, 16])));
        assert!(second.person_view().current().is_none());
        assert_eq!(second.bridge().views().today, None);
        let second_session = second.bridge().session_subhash();
        let second_consent = second.bridge().consent_subhash();
        let first_session = first.bridge().session_subhash();
        let first_stores = std::ptr::from_ref(&first.runtime.stores.machines);
        first.runtime.consent = ConsentOwner::new(
            plx_telemetry::telemetry::consent::Consent {
                asked_version: 73, ..Default::default()
            }, adapters::consent::ConsentAdapter::fixture());
        assert_ne!(first.bridge().consent_subhash(), second_consent);
        assert_eq!(first.bridge().session_subhash(), first_session);
        assert!(std::ptr::eq(first_stores, &first.runtime.stores.machines));
        first.runtime.session = SessionOwner::new(
            plx_session::auth::SessionInit::captured(Default::default()),
            adapters::session::SessionAdapter::fixture());
        let first_consent = first.bridge().consent_subhash();
        first.runtime.stores = StoreOwners {
            machines: Default::default(), runner: adapters::stores::StoreRunner::held(),
        };
        assert_eq!(first.bridge().consent_subhash(), first_consent);
        assert_eq!(first.bridge().views().today, Some([2026, 11, 16]));
        assert_eq!(second.bridge().session_subhash(), second_session);
        assert_eq!(second.bridge().consent_subhash(), second_consent);
        assert_eq!(second.bridge().views().today, None);
    }
}

