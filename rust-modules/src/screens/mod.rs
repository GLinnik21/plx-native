//! **The application's OWNED screens** (restructure spec §2.1 `screens/`, phase 5b): the Settings
//! family — the surface with its own stack, the root, Legal and its documents, Privacy & data
//! (consent, in both its modes) and Favourite libraries (onboard, in both its modes) — as
//! `Screen` impls the dispatcher mounts, steps, focuses through the engine, hit-tests through the
//! map and draws. No `static mut` here: every screen's state is a field of the instance the
//! container owns (§6.1), and the family's shared visual grammar is `ui/table_screen.rs`'s
//! components (5a) plus `ui/route_screen.rs`'s layout, which stays a library concern.
//!
//! Layer rule (§2.1): a screen names `ui/`, `stores/`, the data crates and this directory's
//! `registry`; never `app/` and never a sibling screen module. The bundle's host-side half
//! (`AppHost`, the `Arg` enum, the mounter's match) is `app/bridge.rs`, because its `Arg` still
//! carries the legacy `Route` — see `registry`'s doc.

pub(crate) mod consent;
pub(crate) mod detail;
pub(crate) mod person;
pub(crate) mod filmography;
pub(crate) mod home;
pub(crate) mod library;
pub(crate) mod family;
pub(crate) mod legal;
pub(crate) mod login;
pub(crate) mod onboard;
pub(crate) mod profiles;
pub(crate) mod registry;
pub(crate) mod settings;
