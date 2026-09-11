//! The frame scheduler (spec §8): `Budget` — admission control, not sampling (§8.1) — the
//! `RenderSet` residency check (§8.3), and `glass::GlassPlan`, the glass chain's half of §8.3:
//! the ONE shared backdrop-refresh cadence and the surfaces whose visible lifetime belongs to no
//! screen.
//!
//! Phase 2-i / 2 ship the MINIMAL type: the struct, `take(class)` in the clock-before-every-take
//! shape, one class (`Poster`, quota 3 per frame) and `has_queued_work`. Phase 11 adds the
//! remaining classes with their device-measured `worst_us` and the solo-frame rule. The shape is
//! fixed now because §2.2 makes `Budget` the owner of the poster quota and §3.3 step 8 reads
//! `has_queued_work` from it.
//!
//! `render_set.rs` is the OTHER half of §8.3 and is not the budget at all: the list of every
//! render alive in one frame and the three rules over it, all three of which can now fail —
//! `RENDER_BYTES_MAX` is derived from the device's memory measurement rather than a placeholder,
//! and screens state what they hold through `Screen::render_report`. §8.1's text OCCUPANCY bound
//! lives where the cache does, in `text.rs` (`begin_frame`/`live_this_frame`/`take_evicted_hot`),
//! not here.
#![allow(dead_code)] // phase 2-i: no consumer until phase 2 (spec §13)

mod budget;
pub(crate) mod glass;
mod render_set;

pub use budget::*;
pub use render_set::*;
