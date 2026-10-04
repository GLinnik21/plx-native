//! plx_data: the data layer of the PlxNative application core.
//!
//! The content stores and the data modules they own: `stores` (one command vocabulary and one
//! machine per data store), `pms` (the Home catalog), `browse` (the per-section library catalog),
//! `metadata` (the item detail), `person`, `collection`, `search` and `viewstate` (the view-state
//! writes). It is the `data` layer of `ci/module-layers.ini`: it uses `base`, `machine`,
//! `platform`, `net`, `plex` and `telemetry` and nothing above them, so it names no screen, widget
//! or application type.
//!
//! `test-support` exposes the seams the layers above drive their tests through
//! (`pms::seed_for_test`, `metadata::set_current_for_test`, `browse::view::*::fixture`, ...) and
//! switches the `cfg(not(test))` arms a dependent's tests need in their test form. The application
//! crate enables it in `[dev-dependencies]` only, so no shipped build sees it.

pub mod browse; // Library browse: per-section paged catalog (sparse store + off-thread page fetches)
pub mod collection; // collection page model: tag resolution, header metadata and paged members
pub mod metadata; // item detail data layer (detail page): full metadata + seasons/episodes + cast + related
pub mod person; // person/actor page data layer: the header handed in by the cast row + /library/people/{id}/media
pub mod pms; // the Home catalog: hubs merged across every source
pub mod search; // Search data layer: /hubs/search fanned out across every source, merged into typed shelves
pub mod stores; // stores as machines (restructure phase 4): one command vocabulary + one step per data store
pub mod viewstate; // watched / unwatched / remove-from-deck: the PMS view-state WRITES, off the SDL thread
