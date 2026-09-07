//! The application's adapters (restructure spec §2.2): the OS/FFI resources — never logical
//! state — behind the effects the machines emit. Phase 3a lands the poster source; `net`, `disk`
//! and `sys` arrive with the stores that emit to them (phase 4).

pub(crate) mod poster;
