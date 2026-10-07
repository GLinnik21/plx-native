//! `plx_ui::cards` — the shared card-section module of the shared-card-sections plan. PR 0 lands
//! only its black-box conformance drivers; the primitives arrive in later PRs.

#[cfg(any(test, feature = "test-support"))]
pub mod conformance;
