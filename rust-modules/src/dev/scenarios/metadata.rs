//! Metadata dev IO belongs to the host, never to the data pump or source projection.
use plx_data::metadata::{AltStandInAnswer, AltStandInReq};
use plx_plex::plex::{self, Client, ServerId};
use std::sync::OnceLock;

static SOURCE: OnceLock<Option<String>> = OnceLock::new();
thread_local! {
    static SOURCE_INPUT: std::cell::RefCell<Option<Option<String>>> = const { std::cell::RefCell::new(None) };
}

/// Scoped explicit host input for scripted callers; an explicit None is neutral, not ambient.
#[allow(dead_code)]
pub(crate) fn with_metadata_source<R>(source: Option<String>, run: impl FnOnce() -> R) -> R {
    struct Restore(Option<Option<String>>);
    impl Drop for Restore {
        fn drop(&mut self) { SOURCE_INPUT.with(|s| { s.replace(self.0.take()); }); }
    }
    let _restore = Restore(SOURCE_INPUT.with(|s| s.replace(Some(source))));
    run()
}

pub(crate) fn metadata_source(controlled: bool) -> Option<String> {
    source_selected(controlled, || source_with(false, &SOURCE, || plx_base::devtrig::read("shared")))
}

/// Explicit host input for held/scripted execution: no scope means no ambient source.
#[allow(dead_code)]
pub(crate) fn metadata_source_scripted(controlled: bool) -> Option<String> {
    source_selected(controlled, || None)
}

fn source_selected(controlled: bool, fallback: impl FnOnce() -> Option<String>) -> Option<String> {
    // Suppression precedes scoped input as well as cache/IO selection.
    if controlled { return None; }
    if let Some(source) = SOURCE_INPUT.with(|s| s.borrow().clone()) { return source; }
    fallback()
}

fn source_with(controlled: bool, latch: &OnceLock<Option<String>>, read: impl FnOnce() -> Option<String>) -> Option<String> {
    // Suppress before BOTH the latch and IO, even when a live source was previously latched.
    if controlled { return None; }
    latch.get_or_init(read).clone()
}

pub(crate) fn metadata_alt_stand_in(request: AltStandInReq, library: Option<&str>) -> AltStandInAnswer {
    let list = stand_in_with(library, || plx_base::devtrig::read("shared"), || {
        let here = plex::current_server();
        slot_with(here, plex::server_ids(), plex::client_for,
            || plx_base::devtrig::read("token"),
            |client, token| register_with(client, token, plex::register_origin))
            .map(|theirs| (here, theirs))
    }, |handle, library, here, theirs| {
        let list = plx_data::metadata::alt_stand_in(handle, library, &request.rk, &request.res, request.dur_ms, here, theirs);
        plx_base::eventlog::log(&format!("altsources: stand-in for rk={} on slot {} (dev)", request.rk, theirs.raw()));
        list
    });
    request.answer(list)
}

// Shared is read once per due request, including absent-library/unarmed requests.
fn stand_in_with<T>(library: Option<&str>, read: impl FnOnce() -> Option<String>,
    slots: impl FnOnce() -> Option<(ServerId, ServerId)>,
    build: impl FnOnce(&str, &str, ServerId, ServerId) -> T) -> Option<T> {
    let handle = read().filter(|s| !s.is_empty())?;
    let library = library?;
    let (here, theirs) = slots()?;
    Some(build(&handle, library, here, theirs))
}

fn register_with(client: &Client, token: &str,
    register: impl FnOnce(&str, &plex::Origin, &str, Option<&plex::ResolvePin>, plex::ConnectionFacts) -> ServerId) -> ServerId {
    register(&format!("standin-{}", client.machine_id()), client.origin(), token,
        client.resolve_pin(), plex::ConnectionFacts { tier: client.link(), ip: client.ip_version() })
}

fn slot_with(here: ServerId, ids: impl IntoIterator<Item = ServerId>,
    client: impl Fn(ServerId) -> Option<&'static Client>,
    token: impl FnOnce() -> Option<String>,
    register: impl FnOnce(&Client, &str) -> ServerId) -> Option<ServerId> {
    if !here.is_set() { return None; }
    if let Some(id) = ids.into_iter().find(|id| *id != here && client(*id).is_some()) {
        return Some(id);
    }
    let current = client(here)?;
    let token = token().filter(|s| !s.is_empty())?;
    let id = register(current, &token);
    id.is_set().then_some(id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    #[test]
    fn source_control_precedes_cache_and_io_and_preserves_armed_empty() {
        let latch = OnceLock::new();
        assert_eq!(source_with(true, &latch, || panic!("controlled IO")), None);
        assert!(latch.get().is_none());
        assert_eq!(source_with(false, &latch, || Some(String::new())), Some(String::new()));
        assert_eq!(source_with(true, &latch, || panic!("cached controlled IO")), None);
        assert_eq!(source_with(false, &latch, || panic!("second read")), Some(String::new()));
        let unarmed = OnceLock::new();
        assert_eq!(source_with(false, &unarmed, || None), None);
        assert_eq!(source_with(false, &unarmed, || panic!("unarmed is latched")), None);
    }

    #[test]
    fn scripted_source_is_neutral_without_initializing_ambient_latch_and_scopes_win() {
        let ambient_before = SOURCE.get().cloned();
        let latch = OnceLock::new();
        let reads = Cell::new(0);
        let ambient = || source_with(false, &latch, || {
            reads.set(reads.get() + 1);
            Some("ambient".into())
        });
        assert_eq!(metadata_source_scripted(false), None);
        // Exercise the same selection seam with an explicit neutral fallback.
        assert_eq!(source_selected(false, || None), None);
        assert!(latch.get().is_none());
        for source in [None, Some(String::new()), Some("scoped".into())] {
            with_metadata_source(source.clone(), || {
                assert_eq!(source_selected(false, ambient), source);
                assert_eq!(metadata_source(false), source);
                assert_eq!(metadata_source_scripted(false), source);
                assert_eq!(source_selected(true, || panic!("controlled fallback")), None);
                assert_eq!(metadata_source(true), None);
                assert_eq!(metadata_source_scripted(true), None);
            });
        }
        assert!(latch.get().is_none());
        assert_eq!(reads.get(), 0);
        assert_eq!(metadata_source_scripted(false), None, "scope restored to neutral");
        assert_eq!(SOURCE.get().cloned(), ambient_before, "scripted reads never initialize the ambient cache");
        assert_eq!(source_selected(false, ambient), Some("ambient".into()));
        assert_eq!(reads.get(), 1);
    }

    #[test]
    fn due_requests_read_once_before_library_rejection_and_empty_is_not_a_standin() {
        let reads = Cell::new(0);
        for handle in [None, Some(String::new()), Some("owner".into())] {
            let list: Option<()> = stand_in_with(None, || { reads.set(reads.get()+1); handle },
                || panic!("no library must not resolve slots"), |_, _, _, _| panic!("no list"));
            assert!(list.is_none());
        }
        assert_eq!(reads.get(), 3);
        let list: Option<()> = stand_in_with(Some("Movies"), || Some(String::new()),
            || panic!("empty handle must not resolve slots"), |_, _, _, _| panic!("empty handle"));
        assert!(list.is_none());
    }

    #[test]
    fn existing_slot_precedes_token_and_fallback_preserves_tls_and_switch_target() {
        let _serial = plx_base::testlock::serial();
        plex::reset_servers_for_test();
        let origin = plex::Origin::parse("https://127-0-0-1.hash.plex.direct:32400").unwrap();
        let pin = plex::ResolvePin::for_origin(&origin, "127.0.0.1").unwrap();
        let connection = plex::ConnectionFacts { tier: Some(plex::probe::Location::Local), ip: None };
        let here = plex::register_pinned_with_client_id("origin", &origin, "old", Some(&pin), "cid", connection);
        assert!(plex::set_current(here));
        let reads = Cell::new(0);
        let theirs = slot_with(here, vec![here], plex::client_for,
            || { reads.set(reads.get()+1); Some("replacement".into()) },
            |client, token| register_with(client, token, |machine, origin, token, pin, facts| {
                assert_eq!(machine, "standin-origin");
                assert!(origin.is_tls());
                assert_eq!(origin, client.origin());
                assert_eq!(pin, client.resolve_pin());
                assert_eq!(facts.tier, client.link());
                assert_eq!(facts.ip, client.ip_version());
                plex::register_pinned_with_client_id(machine, origin, token, pin, "cid", facts)
            })).unwrap();
        assert_ne!(here, theirs);
        assert_eq!(reads.get(), 1);
        assert_eq!(slot_with(here, vec![here, theirs], plex::client_for,
            || panic!("existing target must win"), |_, _| panic!("must reuse slot")), Some(theirs));
        assert!(plex::set_current(theirs));
        assert_eq!(plex::current_server(), theirs);
        assert_eq!(plex::client_for(theirs).unwrap().origin(), &origin);
        assert_eq!(plex::client_for(theirs).unwrap().resolve_pin(), Some(&pin));
        plex::reset_servers_for_test();
    }

    #[test]
    fn fallback_rejects_empty_token_and_unset_registration() {
        let _serial = plx_base::testlock::serial();
        plex::reset_servers_for_test();
        let here = plex::register_for_test("one", "127.0.0.1", 9, "t", "cid");
        assert_eq!(slot_with(ServerId::UNSET, vec![here], plex::client_for,
            || panic!("unset current cannot read token"), |_, _| panic!("unset current cannot register")), None);
        for token in [None, Some(String::new())] {
            assert_eq!(slot_with(here, vec![here], plex::client_for, || token,
                |_, _| panic!("no usable token")), None);
        }
        assert_eq!(slot_with(here, vec![here], plex::client_for, || Some("t".into()),
            |_, _| ServerId::UNSET), None);
        plex::reset_servers_for_test();
    }

    #[test]
    fn missing_current_client_does_not_read_token_or_register() {
        assert_eq!(slot_with(ServerId::UNSET, Vec::new(), |_| None,
            || panic!("no client"), |_, _| panic!("no registration")), None);
    }
}
