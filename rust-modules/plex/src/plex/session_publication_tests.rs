//! `CurrentProfile` publication: one writer, no resource-scope allocator, generation carried as-is.

#[allow(unused_imports)]
use super::*;
#[allow(unused_imports)]
use super::test_support::*;

#[test]
fn profile_publication_has_one_writer_and_no_resource_scope_allocator() {
    let source = include_str!("session.rs");
    let (publication, _) = source.split_once("/// Session file locations").expect("publication boundary");
    assert!(!publication.contains("pub fn set_current("));
    assert!(!publication.contains("wrapping_add("));
    // This revision wakes screen observers; it never supplies CurrentProfile::generation.
    let observer = "CURRENT_PUBLICATION.fetch_add(1, std::sync::atomic::Ordering::Relaxed);";
    assert_eq!(publication.matches(observer).count(), 1);
    assert!(!publication.replace(observer, "").contains("fetch_add("));
    assert_eq!(publication.matches("CURRENT.lock()").count(), 2);
    assert_eq!(publication.matches("let mut current = CURRENT.lock()").count(), 1);
    assert_eq!(publication.matches("*current = Some(").count(), 1);
    let (_, writer) = publication.split_once("impl ProfilePublisher {").expect("profile writer");
    let (writer, _) = writer.split_once("/// Resource fixtures").expect("production writer boundary");
    assert!(writer.contains("let mut current = CURRENT.lock()"));
    assert!(writer.contains("*current = Some(std::sync::Arc::new(CurrentProfile { user: user.clone(), generation, stamp }))"));
    assert!(writer.contains("*scoped = std::sync::Arc::new(CurrentProfile { user, generation, stamp })"));
}

#[test]
fn profile_publication_retains_owner_assigned_generation_with_old_read() {
    let _guard = plx_base::testlock::serial();
    let old = super::current_snapshot();
    let mt = unsafe { plx_base::task::MainThread::assume() };
    let mut publisher = super::ProfilePublisher::new(&mt);
    publisher.publish(Some(super::UserRef { uuid: "owner-a".into(), ..Default::default() }), 17, String::new());
    let a = super::current_snapshot();
    publisher.publish(Some(super::UserRef { uuid: "owner-b".into(), ..Default::default() }), 3, String::new());
    let b = super::current_snapshot();
    publisher.publish(old.user.clone(), old.generation, old.stamp.clone());
    assert_eq!(a.generation, 17);
    assert_eq!(a.user.as_ref().unwrap().uuid, "owner-a");
    assert_eq!(b.generation, 3, "resource publishes the supplied scope; it never increments one");
    assert_eq!(b.user.as_ref().unwrap().uuid, "owner-b");
}

#[test]
fn live_profile_publication_triggers_the_account_audio_warm_hook() {
    let _guard = plx_base::testlock::serial();
    let old = super::current_snapshot();
    let mt = unsafe { plx_base::task::MainThread::assume() };
    let mut publisher = super::ProfilePublisher::new(&mt);
    let observed = std::cell::RefCell::new(None);
    publisher.publish_with_warmer_for_test(Some(super::UserRef {
        id: 7, uuid: "guest".into(), plex_tv_token: Some("credential".into()),
        ..Default::default()
    }), 23, |user, generation| {
        let user = user.expect("a live signed-in publication");
        *observed.borrow_mut() = Some((user.id, user.uuid, generation));
    });
    publisher.publish(old.user.clone(), old.generation, old.stamp.clone());
    assert_eq!(*observed.borrow(), Some((7, "guest".into(), 23)));

    let mut scoped = super::ProfilePublisher::scoped(&mt);
    scoped.publish_with_warmer_for_test(Some(super::UserRef::default()), 24,
        |_, _| panic!("recording/replay publications must not warm the process-global cache"));
}
