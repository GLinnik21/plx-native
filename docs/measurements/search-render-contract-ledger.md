# Search render/geometry contract ledger

Audit base: `900384bd` (`Capture Search publications at the dispatcher boundary`). Renderer lane
base: `bbaf28e0`; contract lane base `dbc70ab2`. This ledger reconciles the immutable legacy
contracts in `ui/search/{mod,field,results,empty,recents}.rs` with the owned screen in
`screens/search/`.

It is written in two passes. The renderer lane reconciled `{field,results,empty,recents}.rs` and
left every contract whose owner was the concurrently-moving screen marked **Blocked — parent**;
this pass owns those rows and the thirty in `ui/search/mod.rs`, so **no row is blocked any more**.

Status meanings:

- **Ported** — the same assertion semantics run against a production helper or a real step of the
  owned screen, and the named test is where they run now.
- **Covered** — an owned test that already existed asserts the same contract; the row names it.
- **Retired** — an implementation-history assertion, or a rule a shared component now owns whose
  own test is named in the row.

Tiers, because a row's destination is not a matter of taste:

- `screens/search/{layout,render}::tests` — pure geometry and copy.
- `screens/search/tests.rs` — `Focusable`/`Machine` contracts over a retained publication with a
  real `FocusEngine`: focus, placement, the hit geometry, motion.
- `app/search_owned_tests.rs` (Bridge) — anything whose subject is ORDERING across machines: the
  native keyboard latch, the shared strip, the press machine, the dispatcher's own gates.
- `search/{recents,scope}.rs`, `search.rs` — the store's own data and publication rules.

## Field (`ui/search/field.rs`)

| Legacy test | Reconciliation |
|---|---|
| `the_scope_block_is_one_line_because_the_minimum_hint_lives_in_the_field` | Ported in `render::tests`; uses `layout::SCOPE_H` and `ghost_shown`. |
| `ink_target_picks_pure_white_off_editing_and_the_editing_stop_on` | Ported; `ink_target` is the draw helper. |
| `focus_is_carried_by_a_wide_ink_step_and_nothing_else` | Ported; assertions use the real theme endpoints and `theme::cross`. |
| `issue_22s_exact_fill_shape_does_not_reappear_in_the_source` | Retired; the immutable audit identifies this as implementation-history coverage, not a migration contract. |
| `the_caret_is_visible_only_during_the_on_phase_of_an_editing_field` | Ported; `caret_shown` is the draw predicate. |
| `a_run_that_fits_starts_at_the_edge_and_the_caret_trails_it` | Ported; `run_layout`. |
| `an_overlong_run_slides_left_so_the_caret_lands_on_the_boxs_right_edge` | Ported; `run_layout`. |
| `with_no_caret_the_run_uses_the_whole_box` | Ported; `run_layout`. |
| `an_empty_run_keeps_the_designs_caret_gap` | Ported; `run_layout`. |
| `the_caret_follows_the_insertion_point_whether_or_not_the_run_has_slid` | Ported; `run_layout`. |
| `the_run_scrolls_to_the_caret_rather_than_to_the_tail` | Ported; `run_layout`. |
| `the_head_is_the_text_before_the_insertion_point` | Ported; `run_and_head`, now used by `Resources::prepare`. |
| `a_caret_inside_a_multi_byte_character_never_splits_it` | Ported; `run_and_head`. |
| `a_nul_cuts_the_run_and_the_caret_lands_inside_what_is_left` | Ported; `run_and_head`. |
| `a_box_too_narrow_for_the_caret_does_not_go_negative` | Ported; `run_layout`. |
| `a_blank_field_puts_the_caret_at_the_field_start_not_after_the_placeholder` | Ported; `run_caret_w` is used by `field`. |
| `the_clip_reserves_only_the_descent_centering_does_not_already_clear` | Ported; `descent_pad` is used by `field`. |
| `one_source_is_named_and_only_no_source_is_silent` | Ported; `scope_text`. |
| `two_live_sources_name_both_and_attribute_the_share` | Ported; `scope_text`. |
| `a_share_is_named_by_its_library_and_never_by_its_hostname` | Ported; `scope_text`. |
| `many_shares_collapse_to_a_count_rather_than_becoming_a_list` | Ported; `scope_text`. |
| `a_share_with_no_handle_is_named_but_not_attributed` | Ported; `scope_text`. |
| `an_unreachable_source_names_itself_and_what_is_left` | Ported; `scope_text`. |
| `your_own_server_going_quiet_reads_the_same_way` | Ported; `scope_text`. |
| `with_nothing_answering_there_is_no_results_from_clause_to_write` | Ported; `scope_text`. |
| `three_sources_list_plainly_and_attribute_none_of_them` | Ported; `scope_text`. |
| `two_shares_are_named_but_neither_is_attributed` | Ported; `scope_text`. |
| `an_unnamed_source_still_produces_a_sentence` | Ported; `scope_text`. |
| `an_undescribed_roster_does_not_call_every_machine_yours` | Ported; `scope_text`. |
| `the_memo_key_moves_when_a_source_goes_quiet_or_is_described` | Ported as `search::scope::tests::the_memo_key_moves_when_a_source_goes_quiet_or_is_described`, on the PUBLICATION `render::Resources::prepare` memoises against: a description and a source going quiet each move it, while the roster/section/reachability counters are asserted unmoved for a pure description. |
| `an_undescribed_equal_count_roster_replacement_moves_the_memo_key` | Covered by `search::scope::tests::equal_sized_roster_replacement_publishes_new_sources` — an equal-sized replacement publishes new sources because the key carries the exact ids, not a count. |

## Results (`ui/search/results.rs`)

| Legacy test | Reconciliation |
|---|---|
| `the_reserved_caption_band_holds_the_block_the_shared_component_draws` | Ported in `layout::tests`; `caption_band` is used by `block_h` and is checked against `TileLabel::height`. |
| `shelves_stack_by_their_own_block_heights_from_the_content_top` | Ported; `top`/`block_h`. |
| `the_first_shelfs_whole_row_clears_the_raised_keyboard` | Ported; `top`/`block_h`/`KEYBOARD_H`. |
| `a_shelf_scrolls_only_as_far_as_its_own_block_needs` | Ported; `reveal`, the same shared minimal-reveal helper used by the screen. |
| `a_tile_scrolled_under_the_chrome_is_not_a_pointer_target` | Ported as `screens::search::tests::a_tile_scrolled_under_the_chrome_is_not_a_pointer_target`, over `Focusable::place`'s clip and the real `HitMap`. It found a SHARED bug: `rect ∩ clip` fed `Rect::contains`, which is inclusive, so a stop clipped entirely away answered along its clip's edge and a zero-size stop answered at its corner. Fixed in `ui/hit.rs` (`a_stop_with_no_visible_area_is_not_a_target`) for every page, not in this screen. |
| `the_heading_states_its_count_and_annotates_only_a_borrowed_source` | Ported; `heading_flow` is now the draw/test expression, including count gap, source padding, ink and weight. |
| `the_owner_annotation_swaps_its_words_only_while_it_is_invisible` | Ported as `screens::search::tests::the_owner_annotation_swaps_its_words_only_while_it_is_invisible`, over three real registry sources. `OWNER_FLOOR` moved to `mod.rs` so the alpha the renderer refuses to draw below and the alpha the instance swaps its word at are one constant. |
| `a_settled_annotation_goes_quiet_and_a_moving_one_does_not` | Ported as `screens::search::tests::a_settled_annotation_goes_quiet_and_a_moving_one_does_not`. RED first: the spring reported to `ui::idle` and said nothing to the dispatcher's gate — `ran 0 frames`. The three springs this instance owns now step through `ui::motion::spring`. |
| `a_caption_identifies_the_result_and_names_a_borrowed_source_last` | Ported; `subtitle`. |
| `revealing_the_second_shelf_carries_the_query_field_under_the_track` | Ported as `screens::search::tests::revealing_the_second_shelf_carries_the_query_field_under_the_track`: revealing shelf 0 scrolls nothing, revealing shelf 1 carries the field wholly under the track, where the map answers no target for it. |

## Empty (`ui/search/empty.rs`)

| Legacy test | Reconciliation |
|---|---|
| `the_state_decides_what_is_said_not_the_emptiness` | Ported as `empty_state`; `Searching` remains silent, `Failed` remains a fault even with stale shelves, and landed results are owned by the shelf renderer. |
| `each_statement_keeps_the_name_of_the_region_it_replaces` | Ported as `header_of`; both headers are renderer constants. |
| `the_statement_centres_in_the_space_the_user_can_actually_see` | Ported; `layout::empty_band`. |
| `the_query_is_quoted_back_typographically_and_the_closing_quote_survives` | Ported; `no_results_line`. |

The loading/readout concern therefore has an explicit result: the legacy contract has no loading
spinner or loading sentence. Searching draws no statement; Failed draws the existing
`StatusOverlay` readout. Adding a spinner would be a new product seam, not a render-contract port.

## Recents (`ui/search/recents.rs`)

| Legacy test | Reconciliation |
|---|---|
| `remembering_a_term_moves_it_to_the_front_instead_of_duplicating_it` | Covered by the same-named test in `search::recents::tests` — the data module the phase moved this to, promotion and all. |
| `an_undrawable_term_never_reaches_the_store` | Covered by the same-named test in `search::recents::tests`. |
| `a_session_that_could_not_be_read_is_never_written_back` | Covered by the same-named test in `search::recents::tests`. |
| `terms_are_written_under_the_profile_that_searched_them` | Covered by the same-named test in `search::recents::tests`. |
| `the_cap_drops_the_oldest_term` | Covered by the same-named test in `search::recents::tests`; `layout::RECENT_CAP` is that module's own `CAP`. |
| `a_hand_edited_list_is_cleaned_up_on_the_way_in` | Covered by the same-named test in `search::recents::tests`. |
| `a_full_block_finishes_clear_of_the_raised_keyboard` | Ported in `layout::tests`; `recent_block_bottom` and the real `recents::CAP`/table geometry are used. |
| `the_clear_control_takes_focus_past_the_last_shown_term` | Ported as `screens::search::tests::the_recents_cursor_stops_on_the_clear_control`: ▼ walks every drawn term, caps on Clear, and Clear's own press clears this profile's history, drops the rows and hands focus back to the field. |

## Screen (`ui/search/mod.rs`)

The thirty rows the renderer lane's scope excluded. `a_multi_character_commit_at_the_end_…`
had already been relocated to `ui/text_buffer.rs` before this pass, which is why only 29 `#[test]`
bodies are in that file today; it is listed here to keep the census honest.

| Legacy test | Reconciliation |
|---|---|
| `the_handoff_answers_only_with_regions_that_are_drawn` | Ported as `screens::search::tests::down_from_the_field_reaches_only_a_region_that_is_drawn`. There is no `below_of` state machine any more: the screen publishes a GROUP only for a region it draws, so ▼ has nothing to link to. All four cases are graded through the real `FocusEngine`. |
| `down_from_the_field_with_nothing_below_it_stays_put` | Covered by the row above, whose first and third cases ARE this test — on the live store rather than on stub counts. |
| `the_strip_is_a_zone_with_its_own_cursor` | Ported as `app::bridge::tests::owned_search_walks_the_shared_strip_to_the_chip_and_back_to_the_field`. Search keeps no strip cursor: `TabContainer` publishes the members and the engine walks them, so the seam is what is left to grade — ▲ lands under Search's own pill, ▼ is the field. |
| `the_profile_chip_is_the_bars_leftmost_stop` | Ported by the same test (◀ reaches the chip and cannot underflow it, ▶ returns to the first pill). The chip's identity as a shared control is `app::bridge::tests::library_publishes_the_actual_container_strip`. |
| `a_vertical_step_between_shelves_keeps_the_visual_column` | Ported as `screens::search::tests::a_vertical_step_between_shelves_keeps_the_visual_column`, on `Seat::Projected` + `card_row::column_near_x` over the DRAWN rect rather than on a pure stepper. |
| `a_shelf_that_shrinks_under_the_cursor_re_seats_it` | Ported as `screens::search::tests::a_shelf_that_shrinks_under_the_cursor_re_seats_it`. Graded on one poster lattice (Movies/Shows/Collections), because with an episode still in the middle the visual column, not the clamp, decides the answer — that is the geometry working, and it would have hidden the clamp. |
| `focus_is_clamped_when_the_shelves_shrink_under_it` | Ported as `screens::search::tests::focus_is_clamped_when_the_shelves_shrink_under_it`: `reconcile` seats a too-deep cursor on the shrunken shelf's last item and falls back to the field when the region is gone, with the remembered slot intact. |
| `a_click_on_blank_ground_activates_nothing` | Retired as a Search test: the miss belongs to the shared map (`ui::hit::HitMap::resolve` answers `miss`, graded by `ui::hit::tests::a_foreign_entry_cannot_capture_the_active_owners_hit_or_suppress_a_miss`), and a miss never reaches a screen at all. The Search-specific half — a press on the field raises the panel — is in `screens::search::tests::the_panels_edit_keys_move_the_caret_clear_the_field_and_type_in_the_middle`. **One legacy nuance is deliberately not carried**: legacy made a CLICK on the field only ever raise while OK toggled. The dispatcher delivers `ScreenEvent::Activate` for both (a `Bare` element's OK and an `Activate::Immediate` pointer click are one event), so the distinction is no longer expressible at the screen; the toggle is the OK contract. |
| `a_zero_size_or_stale_region_rect_is_not_hittable` | Ported, in two places. The zero-size and clipped-away halves are `ui::hit::tests::a_stop_with_no_visible_area_is_not_a_target` — the shared map's rule, and a real fix rather than a copy. The "stale region" half is retired as structurally unreachable: recents and result rows are mutually exclusive publications in `sync`, and only DRAWN stops enter the map. "First match wins" is retired and inverted on purpose — painter order IS z now, so the map answers the topmost stop (`ui::hit::tests::the_later_stop_is_on_top`). |
| `a_query_below_the_stores_own_threshold_is_not_a_query` | Ported as `screens::search::tests::a_query_below_the_stores_own_threshold_keeps_the_remembered_terms`, on the live store's `Idle` and the screen's own `real_query`. |
| `moving_inside_an_emptied_result_set_falls_back_to_the_field` | Covered by `screens::search::tests::focus_is_clamped_when_the_shelves_shrink_under_it`, whose last block steps all four directions out of an emptied set. |
| `the_recents_cursor_stops_on_the_clear_control` | Ported under the same name in `screens::search::tests`; see the recents table above. |
| `the_shelf_flow_is_frozen_unless_the_shelves_hold_focus_with_the_keyboard_down` | Ported as `screens::search::tests::the_shelf_flow_is_frozen_unless_the_shelves_hold_focus_with_the_keyboard_down`. The owned screen holds no `scroll_frozen` predicate: raising the panel parks the flow and a step off the field DROPS the panel first, so the freeze is true by construction. |
| `the_scroll_spring_reports_while_it_runs_and_goes_quiet_at_rest` | Ported under the same name in `screens::search::tests`, on the dispatcher's `Present`. RED first (`a scrolling shelf must keep the panel awake`). |
| `backspace_only_bites_while_the_panel_is_up_and_takes_a_whole_character` | Ported inside `screens::search::tests::the_panels_edit_keys_…`: with the panel down the screen answers `Handled::No` and edits nothing; an empty field still consumes the key without issuing a store command. The whole-codepoint half is `ui::text_buffer`'s own `stale_offsets_and_every_unicode_step_stay_on_character_boundaries`. |
| `the_panels_edit_keys_move_the_caret_clear_the_field_and_type_in_the_middle` | Ported under the same name in `screens::search::tests`, in Cyrillic, over the real `TextEdit` vocabulary and the scoped store commands each edit emits. |
| `the_fields_focus_fade_runs_when_focus_leaves_it_and_settles` | Ported under the same name in `screens::search::tests`. RED first (`the fade must keep the panel awake while it travels (ran 1 frames)`). |
| `the_prediction_replaces_rather_than_doubling_the_partial_word` | Covered by `ui::text_buffer::tests::a_multi_character_commit_at_the_end_replaces_the_word_being_typed` (the rule) and `app::bridge::tests::owned_search_keeps_several_commits_while_the_frame_view_is_frozen` (the live store: `s`,`u`,`m`,`summer ` becomes `summer ` with the caret at 7 and the retained view unmoved). |
| `the_caret_blinks_and_reports_only_on_the_flip` | Ported under the same name in `screens::search::tests` — exactly two flips per cycle, each asking for a frame and no other frame asking, and nothing at all with the panel down. `clock_tests::caret_clock_keeps_fractional_milliseconds_and_reports_only_visible_flips` remains the arithmetic half. |
| `ok_raises_the_panel_and_leaving_the_field_drops_it` | Ported across two tests: `screens::search::tests::the_panels_edit_keys_…` (OK raises, OK again commits and files the term) and `…::the_shelf_flow_is_frozen_…` (a step off the field drops the panel). The "a ▼ that moved nothing must not dismiss" half is `app::bridge::tests::owned_search_opens_system_ownership_and_empty_down_keeps_editing`. |
| `back_dismisses_the_panel_once_and_then_leaves` | Covered by `app::bridge::tests::owned_search_opens_system_ownership_and_empty_down_keeps_editing` (the first BACK takes the keyboard down and the stack does not move) and `…::owned_search_result_keys_are_server_scoped_and_survive_same_query_reordering` (the second is `SearchReq::Back`). |
| `the_pill_returns_to_the_search_that_was_there_and_a_seed_replaces_it` | Ported as `screens::search::tests::a_mount_seats_the_field_and_parks_every_cursor_without_replacing_the_search`. There is no `resume`/`enter` pair: the store owns what the pill press used to reset, so a return is this instance mounting over whatever is published — query, generation and shelves untouched. The cursor half is `app::bridge::tests::owned_search_return_memory_reconstructs_positions_with_a_query_guard`. |
| `resuming_a_profile_that_has_never_searched_is_a_clean_mount` | Ported as the first block of the same test. |
| `dismissing_the_panel_from_outside_files_nothing_and_can_be_re_opened` | Covered by `app::bridge::tests::owned_search_external_departure_never_submits_the_draft` (four lifecycle events, no `RememberRecent`) and `…::owned_search_observed_keyboard_dismissal_releases_native_latch_and_reopens` (the native latch is released, so a later OK really reopens). |
| `a_control_byte_in_the_seed_never_reaches_the_query` | Ported as `search::tests::a_control_byte_in_a_seeded_query_never_reaches_the_store`, and MOVED: the filter now lives on `search::set_query`, the one entrance to the query, because the owned screen seeds through `stores::search` and a filter at the legacy mount would be deleted with it. RED first (`left: "wal\0lace"`). |
| `entering_seeds_the_field_and_parks_every_cursor` | Ported as the last block of `…::a_mount_seats_the_field_and_parks_every_cursor_without_replacing_the_search`: the seeded term is in the field with the caret at its live end and every cursor parked. |
| `an_empty_result_set_is_not_a_card` | Ported under the same name in `screens::search::tests`, as an `ElemKind` answer: no card group exists until a shelf lands, the field is `Bare`, and a person shelf is `Bare` too. |
| `the_fields_hit_rect_rides_the_scroll_and_stops_at_the_track` | Ported under the same name in `screens::search::tests`, over `place`'s clip and the real map: at rest it is `layout::FIELD` itself, half under the track only the visible half answers, fully under it nothing does. |
| `the_content_line_clears_the_documents_head` | Ported into `screens::search::layout::tests` under the same name, where `CONTENT_TOP`, `FIELD` and the scope line are all defined. |
| `a_multi_character_commit_at_the_end_replaces_the_word_being_typed` | Covered by `ui::text_buffer::tests::a_multi_character_commit_at_the_end_replaces_the_word_being_typed`; relocated with the shared buffer before this pass. |

## Verification boundary

Counts, taken rather than remembered (`cargo +nightly test --lib -- --list`, 2026-09-09): 61 tests
under `screens::search` — 33 render, 18 screen, 7 layout, 2 draft, 1 clock — beside 19 Bridge-tier
Search tests and 15 in `search::{recents,scope}`. Take them again rather than quoting this line;
every count written into this repository's prose has gone stale.

**Disposition of the 83 legacy bodies, counted off this file's own tables: 68 Ported, 13 Covered,
2 Retired, 0 still blocked.** The two retired are
`issue_22s_exact_fill_shape_does_not_reappear_in_the_source` (a grep for one historical spelling)
and `a_click_on_blank_ground_activates_nothing` (the miss is the shared map's, and never reaches a
screen). `a_zero_size_or_stale_region_rect_is_not_hittable` is counted Ported because its load-
bearing halves became a shared-map fix, but two of its clauses are retired inside that row: the
stale-region case is structurally unreachable now, and "first match wins" is deliberately inverted
because painter order IS z. This pass converted the renderer lane's 13 `Blocked — parent` rows and
added the 30 `ui/search/mod.rs` rows the earlier scope excluded.

Three regressions were found by porting, and each was RED before it was fixed, with the failure
quoted in its commit: the shared hit map answering for a stop with no visible area
(`left: Some(1) / right: None`), the instance's springs reporting to `ui::idle` and not to the
dispatcher's own gate (`a scrolling shelf must keep the panel awake`; `ran 0 frames` for the
annotation), and a seeded query's control bytes reaching the store (`left: "wal\0lace"`). The
renderer lane's own first regression stands as recorded: `clear_geometry_stays_at_the_recents_cap`
observed `clear(CAP + 1)` at `y=682` against the capped `y=622`.

**Verification boundary.** Everything here is Tier 1, the host suite, and that is the right tier
for geometry, copy, focus and store rules — but it is not a claim about pixels. Nothing in this
ledger has been seen on a television or in the simulator: text rasterization, the real widths the
elide and the scope line are measured at (the host `FixtureMeasure` is `bytes × size × 0.5`, not
FreeType), the glass ground under the document, and how any of it looks at 1080p are unproven here,
and the live cutover is a different lane's work — production still mounts the legacy page, so no
assertion above is evidence about what a user sees today.

Two motion boundaries are worth stating rather than leaving to be rediscovered. **(1)** The screen
reports the three springs it OWNS to the dispatcher's present gate; the shared components it
composes — `CardRow`'s scale/scroll springs, `PageGround`'s wash, `Xfade`'s ramp — still report to
`ui::idle` alone. That is systemic and pre-existing (Home and Detail compose the same components
the same way), and it is not a live regression because `app/bridge.rs` turns a dispatcher present
into an `idle::invalidate`, so the loop's gate sees both. Wiring those components to `Present` is
the shared components' own job, not a screen's. **(2)** Because of that, the "a settled Search asks
for no presents" assertions here are settled with respect to the gate they are graded on; the
fade's own ramp is deliberately run out in the fixture's `screen()` before any quiet assertion.
