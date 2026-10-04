use super::*;
use std::sync::Arc;

#[test]
fn remembered_reads_reuse_immutable_entry_scoped_snapshots() {
    let mut engine = FocusEngine::<u32>::new();
    let a = EntryId(1);
    let b = EntryId(2);
    let group = GroupId(7);
    engine.remember_projected(a, group, 11);
    engine.remember_projected(b, group, 22);
    let first = engine.remembered_snapshot(a);
    assert!(Arc::ptr_eq(&first, &engine.remembered_snapshot(a)), "reads must not rebuild the memory list");
    engine.remember_projected(a, group, 11);
    assert!(Arc::ptr_eq(&first, &engine.remembered_snapshot(a)), "unchanged writes retain the projection");
    engine.remember_projected(b, group, 23);
    assert!(Arc::ptr_eq(&first, &engine.remembered_snapshot(a)), "another entry cannot invalidate this snapshot");
    engine.remember_projected(a, group, 12);
    assert_eq!(&*first, &[(group, 11)], "a context already handed to a screen remains immutable");
    assert_eq!(&*engine.remembered_snapshot(a), &[(group, 12)]);
    assert_eq!(&*engine.remembered_snapshot(b), &[(group, 23)]);
    engine.restore_remembered(a, &[(GroupId(9), 99)]);
    assert_eq!(&*engine.remembered_snapshot(a), &[(GroupId(9), 99)]);
    engine.forget(a);
    assert!(engine.remembered_snapshot(a).is_empty());
    assert_eq!(&*engine.remembered_snapshot(b), &[(group, 23)]);
    assert_eq!(&*first, &[(group, 11)]);
}
