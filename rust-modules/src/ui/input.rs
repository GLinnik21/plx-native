//! The `Input` machine's HOME (restructure spec §2.2): the press machine today; key-repeat edge
//! detection, pointer visibility + `dpad_mode`, the hit map and the focus engine join it in
//! phases 2–3b. It is an `App` field — the ONE owner of the press — and the legacy key ladders
//! reach it as `&mut Press` parameters rather than through a global (spec §14, the press facade).
use super::press::Press;

pub(crate) struct Input {
    pub(crate) press: Press,
}

impl Input {
    pub(crate) const fn new() -> Self {
        Self {
            press: Press::new(),
        }
    }
}
