//! `CurrentProfile` publication: one writer, no resource-scope allocator, generation carried as-is.

#[allow(unused_imports)]
use super::*;
#[allow(unused_imports)]
use super::test_support::*;

#[test]
fn profile_publication_has_one_writer_and_no_resource_scope_allocator() {
    let source = include_str!("session.rs");
    let publication = source.split("/// Session file locations").next().unwrap();
    assert!(!publication.contains("pub fn set_current("));
    assert!(!publication.contains("wrapping_add("));
    assert!(!publication.contains("fetch_add("));
    assert_eq!(publication.matches("*CURRENT.lock()").count(), 1);
    let writer = publication.split("impl ProfilePublisher {").nth(1).unwrap()
        .split("/// Resource fixtures").next().unwrap();
    assert!(writer.contains("*CURRENT.lock()"));
    assert!(writer.contains("CurrentProfile { user, generation }"));
}

#[test]
fn profile_publication_retains_owner_assigned_generation_with_old_read() {
    let _guard = crate::testlock::serial();
    let old = super::current_snapshot();
    let mt = unsafe { crate::task::MainThread::assume() };
    let mut publisher = super::ProfilePublisher::new(&mt);
    publisher.publish(Some(super::UserRef { uuid: "owner-a".into(), ..Default::default() }), 17);
    let a = super::current_snapshot();
    publisher.publish(Some(super::UserRef { uuid: "owner-b".into(), ..Default::default() }), 3);
    let b = super::current_snapshot();
    publisher.publish(old.user.clone(), old.generation);
    assert_eq!(a.generation, 17);
    assert_eq!(a.user.as_ref().unwrap().uuid, "owner-a");
    assert_eq!(b.generation, 3, "resource publishes the supplied scope; it never increments one");
    assert_eq!(b.user.as_ref().unwrap().uuid, "owner-b");
}

#[test]
fn live_profile_publication_triggers_the_account_audio_warm_hook() {
    let _guard = crate::testlock::serial();
    let old = super::current_snapshot();
    let mt = unsafe { crate::task::MainThread::assume() };
    let mut publisher = super::ProfilePublisher::new(&mt);
    let observed = std::cell::RefCell::new(None);
    publisher.publish_with_warmer_for_test(Some(super::UserRef {
        id: 7, uuid: "guest".into(), plex_tv_token: Some("credential".into()),
        ..Default::default()
    }), 23, |user, generation| {
        let user = user.expect("a live signed-in publication");
        *observed.borrow_mut() = Some((user.id, user.uuid, generation));
    });
    publisher.publish(old.user.clone(), old.generation);
    assert_eq!(*observed.borrow(), Some((7, "guest".into(), 23)));

    let mut scoped = super::ProfilePublisher::scoped(&mt);
    scoped.publish_with_warmer_for_test(Some(super::UserRef::default()), 24,
        |_, _| panic!("recording/replay publications must not warm the process-global cache"));
}
