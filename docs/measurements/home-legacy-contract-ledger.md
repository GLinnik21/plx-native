# Home legacy contract ledger

Audit base: `dbc70ab2` (the lane base). This ledger reconciles the 34 `#[test]`s of the retired
`rust-modules/src/ui/home.rs` (3632 lines, 19 `static mut`) with the owned Home screen
(`rust-modules/src/screens/home/{mod,tests.rs}`) and the components that took the geometry with it.
Every legacy test was read against the owned test of the same name before it was dispositioned; a
matching NAME was never taken as evidence.

Status meanings:

- **Ported** — the same assertion semantics run against the owned screen or the component that now
  owns the arithmetic. Some are byte-identical; where the helper was renamed or the call shape
  changed, the reconciliation column says so.
- **Already covered** — the contract survives, but its owner moved out of Home entirely and an
  existing test elsewhere subsumes it. The subsuming test is named and was read.
- **Retired** — the assertion was about a mechanism the migration deleted (the packed `c_int` focus
  mirror and its setter, `step_row`, `Grid::vert`), not about a behaviour. Where a *bound* the
  retired assertion also happened to pin survives, the surviving assertion is named.

Counts: **27 ported, 3 already covered, 4 retired** — 34 total.

## The 34 tests of `ui/home.rs`

| Legacy test | Reconciliation |
|---|---|
| `n_hubs_clamps_the_server_count_to_the_shelf_array` | Ported; owned test of the same name, same `n_hubs_of` helper. Its dropped `n_hubs_of(MAX_HUBS) == MAX_HUBS` leg is asserted by the owned `vert_cannot_walk_past_the_shelf_array`. |
| `every_tab_pill_round_trips_through_the_focus_packing` | Retired; the packed negative-`c_int` focus space (`hero_focus_for_pill`/`hero_pill_index`) went with the `static mut` focus mirror. The successor no-aliasing contract — every strip element decodes to exactly one destination request and no page element (`HERO_PLAY_ELEM`, `HERO_INFO_ELEM`, `FIRST_ITEM_ELEM`) decodes as one — is the owned test of the same name. |
| `top_band_focus_walks_to_the_last_section_whatever_the_count` | Already covered; the walk belongs to the `FocusEngine` over the strip group now. `app::chrome::tests::four_libraries_on_two_servers_publish_two_type_destinations` pins the member order and that the last stop is Search; `ui::focus::tests::every_stop_times_every_direction_on_the_fixture_trees` pins that a move never lands on a stop that is not drawn. The owned test of this name keeps the element ordering the walk is over. |
| `set_hero_focus_clamps_onto_the_last_drawable_pill` | Retired; `set_hero_focus` was the mirror's setter and its clamp went with it. The engine holds focus and clamps at a group's ends (`every_stop_times_every_direction_on_the_fixture_trees`); the hero row's own extent — two stops, and no strip group published by the screen — is the owned test of this name. |
| `the_pager_is_not_a_focus_stop_and_the_rows_end_pages_instead` | Ported; owned test of the same name asserts RIGHT at the hero row's end is `Outcome::Edge(EdgeRule::Screen)`, that `flip` moves the carousel, and that focus does not move doing it. The legacy `HERO_NBTN == 2` / `hero_btns.len() == 2` legs are the owned `set_hero_focus_clamps_onto_the_last_drawable_pill`'s `groups[0].len == 2`, and the hit half is `drawn_stops_feed_the_real_hit_map_with_scoped_card_keys`. |
| `the_top_band_reports_the_chip_and_the_pills_as_one_answer` | Already covered; the `TopFocus::Chip`/`Pill`/`Away` read-out moved to `app::chrome::ChromeSnapshot::focus` and is pinned by `app::chrome::tests::published_bar_focus_uses_destination_identity_not_position` (chip, both pills, and page elements 0/1 as `Away`). The owned test of this name keeps the STRIP↔hero link pair the walk between them needs. |
| `the_top_band_walks_permanent_pills_not_the_section_table` | Already covered; `app::chrome::tests::four_libraries_on_two_servers_publish_two_type_destinations` seeds the same two-source table and asserts four libraries publish Home + two TYPE pills + Search, with Search last. |
| `step_row_stays_inside_the_addressable_rows` | Retired; `step_row` was the mirror's row stepper and is deleted. The vertical bounds it stood for are the engine's links: owned `down_from_the_first_shelf_chooses_the_next_shelf_not_the_folded_hero` and `down_from_the_last_shelf_never_reenters_the_offscreen_hero`. The owned test of this name pins the row's horizontal ends as `Step::Edge`. |
| `vert_cannot_walk_past_the_shelf_array` | Retired; `Grid::vert` walked `static mut fr` and is deleted. The array bound it pinned survives as the owned test of this name (`n_hubs_of` clamped at `MAX_HUBS`), and the walk itself as `down_from_the_last_shelf_never_reenters_the_offscreen_hero`. |
| `the_status_readout_tells_loading_empty_and_failed_apart` | Ported; owned test of the same name, same `status_read` helper, now taking a `HubsView` instead of reading the statics. All five legs kept (Working/Failed/Empty, the Retry action's presence, and both shelved states silencing the read-out). |
| `no_shelves_means_no_grid_snap` | Ported; owned test of the same name, same `pinned_snap`. |
| `the_status_screen_takes_ok_but_never_the_top_band` | Ported; owned test of the same name drives the real `ScreenEvent::Activate` step instead of `status_takes`, asserting the Retry `StoreCmd` on the status action, no Retry from any strip element, and that a populated Home's OK is a Play again. |
| `pointer_hit_column_matches_the_drawn_card_at_every_snap_phase` | Ported; same `card_x`/`col_at` sweep over the same three scrolls × three snap phases × 24 columns. |
| `the_card_anchor_rect_tracks_the_drawn_card_through_scroll_and_snap` | Ported; byte-identical. |
| `the_wash_floors_on_the_app_surface_and_never_dims_toward_black` | Ported; byte-identical. |
| `the_grid_end_of_the_dive_is_the_focused_tiles_ground` | Ported; byte-identical. |
| `prefetch_order_wraps_and_never_warms_the_page_on_screen` | Ported; identical but for `c_int` → `i32`. |
| `the_prefetch_is_armed_only_from_a_settled_hero` | Ported; byte-identical. |
| `the_ground_is_skipped_only_when_opaque_art_covers_it` | Ported; byte-identical. |
| `the_home_hero_logo_never_reaches_the_top_bar` | Ported; same arithmetic over `hero_syn_h`, `hero_logo::band_h`, `hero_stack_top` and `TOP_BAR_BOTTOM`, message text dropped. |
| `the_hero_logo_key_is_the_shows_for_an_episode` | Ported; byte-identical. |
| `the_continue_watching_caption_promises_time_left_only_when_the_bar_is_drawn` | Ported; byte-identical. |
| `the_first_shelfs_raised_heading_settles_clear_of_the_profile_chip` | Ported; same `heading_top(0, 0, settled_scroll(5, 0, from_below(5))) >= top_band_bottom()`. |
| `no_shelf_heading_settles_inside_the_shared_top_band` | Ported; the same `MAX_HUBS` × focus-row × row sweep, with `settle_range`'s two ends spelled out as the two `settled_scroll` calls it returned. |
| `every_settled_row_keeps_its_focused_label_block_above_the_overscan_bottom` | Ported; same sweep, same `GRID_TOP_Y`/`shelf_top_settled`/`CARD_DY`/`CARD_H`/`UNDER_LABEL_H`/`MARGIN_Y` arithmetic. |
| `the_grids_resting_top_is_the_highest_a_shelf_may_settle` | Ported; same `row_reveal_band` and top-band air assertions. |
| `a_shelf_with_no_source_draws_exactly_the_title_and_nothing_else` | Ported; the local `flow` helper is now `heading_flow` over `card_row::heading_flow`, the production helper the owned screen draws through. |
| `a_shared_source_extends_the_heading_past_the_title` | Ported; same, all six legs kept verbatim. |
| `each_heading_run_carries_its_own_size_weight_and_ink` | Ported; same. |
| `a_hero_from_our_own_server_draws_no_source_run_at_all` | Ported; byte-identical, over the owned `meta_source_flow`. |
| `a_borrowed_hero_states_its_owner_as_the_last_run_on_the_line` | Ported; byte-identical. |
| `the_hero_source_run_keeps_the_lines_rung_and_takes_one_step_of_ink` | Ported; byte-identical. |
| `an_over_long_handle_truncates_rather_than_wrapping` | Ported; byte-identical. |
| `the_meta_lines_bound_keeps_the_run_inside_the_hero_wedge` | Ported; byte-identical. |

## What went with the file, and where it landed

`ui/home.rs` defined nothing that survived only in it. Every item the retirement had to place was
already defined by its proper owner, and the legacy copy was the duplicate:

| Legacy item | Owner today |
|---|---|
| `Backdrop`, `HERO_WASH_W`, `HERO_CTRL_D`, `HERO_PREFETCH`, `prefetch_order`, `meta_source_flow` | `screens/home/mod.rs` (its own definitions, since phase 8) |
| `HERO_BASE_SCRIM_Y0`, `HERO_CTRL_GAP` (`CTRL_GAP`), `redraw_profile_chip` | `ui/widgets.rs` |
| `base_scrim_a` and the hero scrim curve | `ui/landing_hero.rs` |
| `MAX_HUBS` / `MAX_ITEMS` | `pms::MAX_SHELVES` / `pms::MAX_SHELF_ITEMS`; `screens/home` reads them from there, as `ui/home.rs` did |
| the heading flow (`flow`) | `ui/card_row.rs::heading_flow` |

## The API the deletion orphaned

`ui/home.rs` was the last production caller of `pms`'s static-catalog read API. Production reads the
retained publication (`pms::hubs_snapshot()` → `HubsView`) and nothing else now, so these were
removed with it: `pms::{pool, hero_pool_len, hero_pool_item, hero_pool_source, hub_title,
hub_source, hub_len, hub_identity, hub_identity_in, hub_is_continue, pump}`,
`stores::hubs::pump` and `ui::widgets::is_search_pill`.

Nothing they asserted was lost. The readers survive verbatim as helpers inside `pms`'s own test
module, so every landing, back-off, merge and attribution contract in that file is unchanged;
`hub_len` stays at module scope behind `#[cfg(test)]` because `app/bridge.rs`'s and
`app/recorder.rs`'s tests assert a landing with it; and `hub_identity_in`'s two identity contracts
(`home_group_identity_uses_provider_and_server_not_title_or_position`,
`merged_home_deck_identity_survives_a_different_leading_server`) are ported onto
`stable_hub_identity`, the one identity function the publication itself uses, rather than kept
beside a duplicate of it. **Both were watched RED against that function before being called a
port**, each against the break it is meant to catch: sourcing the sid from the catalog's first
item instead of the hub's own `start` fails the provider test (`ServerId(0)` where `ServerId(1)`
is owed) and leaves the merged-deck one green, because Continue Watching's identity is
server-independent by design; gating the `home.continue` arm on `start == 0` is what fails that
one (`Identifier { sid: ServerId(1) }` where `ContinueWatching` is owed). A simulated red, not a
historical one — the defect never shipped. `stores::hubs::pump` was documented as the "legacy callers' combined
pass"; the live path is the adapter's `take_results`/`land` plus the store's own `tick`, and
`pms`'s tests keep the combined pass as a local driver. `is_search_pill` had no caller left and its
doc named the legacy top-band walk test as its only reason to exist.

## Gate deltas

`grep -rn 'static mut' rust-modules/src/ui | wc -l` — **177 → 157**. That grep is not restricted to
`*.rs`, so −20 is 19 declarations (every one in `ui/home.rs`; the file carried 19, not the 15 the
lane was scoped for) plus one prose mention in `ui/CLAUDE.md` that told a reader to take a `FOCUS`
mutex for `static mut fr`/`fc`. Restricted to `*.rs` it is **174 → 155**, −19 exactly.

Host suite: 2813 runnable before, 2779 after — exactly the 34 deleted tests, with the three ignored
unchanged. `make check`'s two passes are 2779 default / 2800 hostsim, 0 failed.
