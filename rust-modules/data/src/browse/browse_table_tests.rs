//! Section addressing and sparse-page storage: `SectionAddress` epoch/identity guards and
//! `SparsePages` resize/growth/shrink behavior against flat storage.

use super::*;
#[allow(unused_imports)]
use super::test_support::*;

#[test]
fn addressed_instance_commit_validates_identity_and_selects_before_query() {
    use crate::stores::browse::{LibraryWork, QueryEdit, SectionAddress};

    let sid = super::ServerId::UNSET;
    let mut state = super::BrowseState::default();
    state.sources.push(a_source("machine-a", "", true));
    state.sections = vec![
        super::BrowseSection {
            src: 0,
            key: 7,
            title: "Films A".into(),
            kind: super::SecKind::Movie,
            count: 0,
            pinned: true,
        },
        super::BrowseSection {
            src: 0,
            key: 8,
            title: "Films B".into(),
            kind: super::SecKind::Movie,
            count: 0,
            pinned: true,
        },
    ];
    state.states = vec![super::SecState::default(), super::SecState::default()];
    let target = SectionAddress {
        epoch: state.table_epoch(),
        sid,
        section: 8,
    };
    let adapter = Arc::new(BrowseAdapter::default());

    assert!(!state.addressed_with_adapter(
        &adapter,
        SectionAddress {
            epoch: target.epoch.wrapping_add(1),
            ..target
        },
        LibraryWork::Commit {
            select: true,
            choice: false,
            query: Some(QueryEdit::Unwatched(true)),
        },
    ));
    assert_eq!(state.cur(), 0);
    assert_eq!(state.query_gen(), 0);
    assert!(state.addressed_with_adapter(
        &adapter,
        target,
        LibraryWork::Commit {
            select: true,
            choice: false,
            query: Some(QueryEdit::Unwatched(true)),
        },
    ));
    assert_eq!(state.cur(), 1);
    assert!(!state.states[0].unwatched);
    assert!(state.states[1].unwatched);
    assert_eq!(
        state.query_gen(),
        2,
        "selection and requery each supersede a landing"
    );
}
#[test]
fn retained_pages_copy_only_changed_page_and_keep_sparse_holes() {
    let mut items = super::SecItems::default();
    items.resize(20_000);
    let sid = super::ServerId::UNSET;
    let other = super::ServerId::from_raw(1);
    items.set(
        0,
        super::PmsMovie {
            sid,
            rk: "7".into(),
            ..Default::default()
        },
    );
    items.set(
        super::PAGE,
        super::PmsMovie {
            sid: other,
            rk: "7".into(),
            ..Default::default()
        },
    );
    let old = items.clone();
    assert!(super::Arc::ptr_eq(&old.pages, &items.pages));
    assert_eq!(items.pages.iter().flatten().count(), 2);
    assert!(!items.set_watched(sid, "missing", false));
    assert!(super::Arc::ptr_eq(&old.pages, &items.pages));
    assert!(items.set_watched(sid, "7", false));
    assert!(!super::Arc::ptr_eq(&old.pages, &items.pages));
    assert!(!super::Arc::ptr_eq(
        old.pages[0].as_ref().unwrap(),
        items.pages[0].as_ref().unwrap()
    ));
    assert!(super::Arc::ptr_eq(
        old.pages[1].as_ref().unwrap(),
        items.pages[1].as_ref().unwrap()
    ));
    assert!(!old.get(0).unwrap().unwatched);
    assert!(items.get(0).unwrap().unwatched);
    assert!(!items.get(super::PAGE).unwrap().unwatched);
    items.set(super::PAGE * 2, super::PmsMovie::default());
    assert!(old.get(super::PAGE * 2).is_none());
    assert!(old.page_missing(2));
    items.clear();
    assert_eq!(old.len(), 20_000);
    assert!(old.get(0).is_some());
}
#[test]
fn sparse_page_resize_matches_flat_storage_across_boundaries() {
    let mut items = super::SecItems::default();
    let mut flat: Vec<Option<String>> = Vec::new();
    for size in [1, 2, 59, 60, 61, 122, 61, 60, 59, 60, 61, 0, 1, 121] {
        let old = items.clone();
        let old_flat = flat.clone();
        items.resize(size);
        flat.resize(size, None);
        for i in 0..=size {
            assert_eq!(
                items.get(i).map(|m| &m.rk),
                flat.get(i).and_then(Option::as_ref)
            );
        }
        for i in 0..size {
            if i % 3 == 0 {
                let rk = format!("{size}-{i}");
                items.set(
                    i,
                    super::PmsMovie {
                        rk: rk.clone(),
                        ..Default::default()
                    },
                );
                flat[i] = Some(rk);
            }
        }
        for (p, chunk) in flat.chunks(super::PAGE).enumerate() {
            assert_eq!(items.page_missing(p), chunk.iter().any(Option::is_none));
        }
        assert!(!items.page_missing(size.div_ceil(super::PAGE)));
        for (i, expected) in old_flat.iter().enumerate() {
            assert_eq!(old.get(i).map(|m| &m.rk), expected.as_ref());
        }
    }
}
#[test]
fn sparse_page_growth_accepts_new_items() {
    let mut items = super::SecItems::default();
    items.resize(1);
    items.set(0, super::PmsMovie::default());
    items.resize(2);
    assert!(items.page_missing(0));
    items.set(
        1,
        super::PmsMovie {
            rk: "2".into(),
            ..Default::default()
        },
    );
    assert_eq!(items.get(1).map(|m| m.rk.as_str()), Some("2"));
    assert!(!items.page_missing(0));
}
#[test]
fn sparse_page_shrink_does_not_resurrect_removed_items() {
    let mut items = super::SecItems::from_vec(vec![
        Some(super::PmsMovie::default()),
        Some(super::PmsMovie::default()),
    ]);
    items.resize(1);
    items.resize(2);
    assert!(items.get(1).is_none());
    assert!(items.page_missing(0));
}
