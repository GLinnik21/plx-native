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
