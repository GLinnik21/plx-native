//! **The application's OWNED screens** (restructure spec §2.1 `screens/`, phase 5b): the Settings
//! family — the surface with its own stack, the root, Legal and its documents, Privacy & data
//! (consent, in both its modes) and Favourite libraries (onboard, in both its modes) — as
//! `Screen` impls the dispatcher mounts, steps, focuses through the engine, hit-tests through the
//! map and draws. No `static mut` here: every screen's state is a field of the instance the
//! container owns (§6.1), and the family's shared visual grammar is `ui/table_screen.rs`'s
//! components (5a) plus `ui/route_screen.rs`'s layout, which stays a library concern.
//!
//! Layer rule (§2.1): a screen names `ui/`, `stores/`, the data crates and this directory's
//! `registry`; never `app/` and never a sibling screen module — a rule `ci/check-deps.sh`'s
//! `layer` and `sibling` gates hold from phase 10. The `Arg` enum, the page alphabet and the
//! mounter's one match are `registry`'s since that phase; what is left in `app/bridge.rs` is the
//! concrete host and the rig it lends the dispatcher.

pub mod about_panel;
pub mod account_menu;
pub mod alt_sources;
pub mod tracks_panel;
pub mod consent;
pub mod collection;
#[cfg(test)]
mod cards_conformance_tests; // shared-card-sections Tier 2: every card screen through one black-box suite
pub mod detail;
pub mod person;
pub mod person_bio;
pub mod filmography;
pub mod home;
pub mod item_menu;
pub mod library;
pub mod search;
pub mod family;
pub mod legal;
pub mod login;
pub mod onboard;
pub mod clock_readout;
pub mod plaintext_question;
pub mod player;
pub mod profiles;
pub mod registry;
pub mod settings;
pub mod preferences;
