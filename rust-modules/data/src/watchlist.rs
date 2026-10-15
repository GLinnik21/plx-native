//! watchlist: the data layer of the plex.tv Watchlist page (docs/watchlist-plan.md §1.1-1.5, §1.10).
//!
//! CONTRACT FREEZE. The submodules below declare the types and signatures the store
//! (`stores::watchlist`, W2b) and the screens compile against. Bodies marked `unimplemented!` are
//! filled by W2a (`model`, `order`) and W2b (`ledger`); the SHAPE is fixed.
//!
//! * [`model`]: rows, the guid-to-server match table, [`model::TileState`].
//! * [`order`]: the title comparator (§1.10), sort and filter over stored rows.
//! * [`ledger`]: the intent ledger (optimistic flip, coalescing, revert).
pub mod ledger;
pub mod model;
pub mod order;
