fn two_sources() -> (Vec<SrcGroup>, Vec<crate::browse::SrcRow>) {
    (
        vec![
            group(SourceState::Reachable, None, ""),
            SrcGroup {
                name: "nas-home".into(),
                handle: "friend".into(),
                state: SourceState::Reachable,
                tier: None,
            },
        ],
        vec![
            lib(0, 0, "Movies", true, false, true),
            lib(0, 1, "TV Shows", true, false, false),
            lib(1, 2, "Film Club", false, false, false),
        ],
    )
}

fn lib(src: usize, section: usize, title: &str, pinned: bool, last: bool, current: bool) -> crate::browse::SrcRow {
    crate::browse::SrcRow {
        src,
        section,
        title: title.into(),
        count_line: "26 films".into(),
        pinned,
        last_pinned: last,
        current,
    }
}

fn marks_for(sections: &[crate::ui::table::Section]) -> Vec<(bool, Option<String>)> {
    sections
        .iter()
        .flat_map(|section| section.rows.iter())
        .map(|row| (
            row.checked,
            row.value.clone().or_else(|| row.toggle.map(|on| if on { "On" } else { "Off" }.into())),
        ))
        .collect()
}

#[test]
fn browse_and_home_levels_keep_marks_values_and_actions_aligned() {
    let (groups, rows) = two_sources();
    let expected_actions = vec![
        SrcAction::Library(0),
        SrcAction::Library(1),
        SrcAction::Library(2),
    ];

    let (browse, browse_actions) = sections(Level::Browse, &groups, &rows, Tail::None);
    assert_eq!(marks_for(&browse), vec![(true, None), (false, None), (false, None)]);
    assert_eq!(browse_actions, expected_actions, "Browse actions follow the same library rows");
    assert_eq!(browse.iter().flat_map(|section| section.rows.iter()).count(), browse_actions.len());

    let (home, home_actions) = sections(Level::OnHome, &groups, &rows, Tail::None);
    assert_eq!(marks_for(&home), vec![
        (false, Some("On".into())),
        (false, Some("On".into())),
        (false, Some("Off".into())),
    ]);
    assert_eq!(home_actions, expected_actions, "OnHome actions stay aligned with the switch rows");
    assert_eq!(home.iter().flat_map(|section| section.rows.iter()).count(), home_actions.len());
}

#[test]
fn an_unreachable_group_dims_as_a_whole_and_unlearned_sources_are_omitted() {
    let (mut groups, mut rows) = two_sources();
    let (healthy, _) = sections(Level::OnHome, &groups, &rows, Tail::None);
    assert_eq!(healthy.len(), 2);
    assert!(!healthy[0].dim && !healthy[1].dim, "healthy groups remain live");

    groups[1].state = SourceState::Unreachable;
    rows[2].pinned = true;
    let (failed, _) = sections(Level::OnHome, &groups, &rows, Tail::None);
    assert_eq!((failed[1].header.as_str(), failed[1].accessory.as_str()), ("nas-home", "Not reachable · friend"));
    assert!(failed[1].dim, "the unreachable group header and rows dim together");
    assert!(!failed[0].dim, "the healthy group remains live");
    assert_eq!(failed[1].rows[0].toggle, Some(true), "unreachable does not silently un-favourite a row");

    let (without_learned_rows, _) = sections(Level::OnHome, &groups, &rows[..2], Tail::None);
    assert_eq!(without_learned_rows.len(), 1, "a source with no learned libraries has no empty group");
}

#[test]
fn the_last_pinned_value_dims_without_disabling_its_row_or_other_details() {
    let groups = vec![group(SourceState::Reachable, None, "")];
    let rows = vec![
        lib(0, 0, "Movies", true, true, true),
        lib(0, 1, "TV Shows", false, false, false),
    ];
    let (sections, actions) = sections(Level::OnHome, &groups, &rows, Tail::None);
    let last = &sections[0].rows[0];
    assert!(last.value_dim, "the last pinned value dims");
    assert!(!last.dim, "the row label remains live");
    assert_eq!(last.toggle, Some(true));
    assert_eq!(last.detail, "The app needs one library");
    assert_eq!(sections[0].rows[1].detail, "26 films", "other rows retain their count details");
    assert_eq!(actions, vec![SrcAction::Library(0), SrcAction::Library(1)]);
}
