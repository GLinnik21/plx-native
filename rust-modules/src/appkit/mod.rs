//! Application widgets shared by several screens: compositions of `ui` components over application
//! types (the player, route selection, the metadata and browse stores, Plex session preferences).
//!
//! **Why this is not `ui/` and not `screens/`.** `ui/` is the widget LIBRARY and names no
//! application type (`ui/CLAUDE.md`'s layer rule), and a widget that reads `player::PlaybackState`
//! or `metadata::Stream` is application code. But it is not one screen's either: `player_hud` is
//! drawn by `screens::player` and `screens::detail` (the trailer's transport), `track_menu` by
//! `screens::player` and `screens::preferences`, `source_list` by `screens::onboard` and
//! `screens::library`. `ci/check-deps.sh`'s `sibling` gate forbids one screen family naming
//! another, so these cannot live under `screens/`; they live here, one layer below `screens` and
//! one above `media`, so that `ui` stays free of them (`ci/module-layers.ini`, `[appkit]`;
//! `docs/module-layers.md` step L10).
//!
//! **Direction.** This module may name every layer below it (`ui`, `data`, `media`, `session`, ...)
//! and never `screens` or `app`: a value the loop owns is passed in, and a behaviour a higher layer
//! owns is a hook it registers at boot (`more_menu::install_stats_reader`, installed by
//! `app::enter_application`). The files here moved out of `ui/` unchanged.
// **This blankets the whole `appkit` tree**, as `plx_ui`'s `lib.rs`'s own `allow(dead_code)` did for these
// files while they lived there (that comment records why it is wider than it reads): the moved
// files were never subject to the lint, and moving them must not make a different set of items
// "dead".
#![allow(dead_code)]

pub mod chapters_panel;
pub mod info_panel;
pub mod more_menu; // the player's `…` overflow popover (holds the Stats for nerds toggle)
pub mod player_hud;
pub(crate) mod skip_pill; // the Skip Intro / Skip Credits pill — a `ControlSlot` occupant of the player HUD's control row
pub mod source_list; // the Sources ROW MODEL, shared by the Library panel and that route
pub mod timing_capsule; // the on-video Subtitle Timing capsule (plan `subtitle-menu-capsule` §4)
pub mod track_menu;
pub mod up_next; // end-of-episode Up Next card + auto-advance countdown

