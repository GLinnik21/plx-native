//! plx_platform: what this television is and keeps, for the PlxNative application core.
//!
//! The webOS identity (`webos`), the storage helper's client and its wire protocol (`storage`), the
//! key stores (`keymanager`), the codec table (`devcaps`), the artwork cache (`imgcache`), the
//! localization catalog (`i18n`), the Cloud Lab session file (`labcfg`) and the interfaces the
//! webOS port fills at boot (`tv`). It is the `platform` layer of `ci/module-layers.ini`: it uses
//! `base` and `machine` and nothing else. `webos` and `keymanager` are also the port's
//! (`[port webos]`), and nothing outside the port names them.
//!
//! The crate owns its build script: it generates the `i18n::msg` catalog from `locales/` and the
//! install-identity `Flavor` enum `storage::state` includes (the storage helper includes the same
//! file, through the same generator). The application's version string is NOT known here; the
//! application hands it to `storage::diagnostics::start`.
//!
//! `test-support` exposes the `cfg(test)` seams other layers' tests build on (`i18n`'s thread
//! locale, `storage`'s commit-failure and unlink-fault injection and the in-process helper backend,
//! `tv`'s sandbox, home and secure-store seams) and keeps this crate's test-mode behaviour (the
//! English default locale, the absent-port fallback, the commit-failure hook) when a dependent's
//! tests build it. The application crate enables it in `[dev-dependencies]` only, so no shipped
//! build sees it.

pub mod devcaps; // what this SoC decodes — the TV's own codec table, read once at boot (the capability profile + direct-play gate derive from it)
pub mod i18n;
pub mod imgcache; // bounded persistent artwork cache shared by every image source
pub mod keymanager; // public LS2 key stores: keymanager3, legacy Palm service, or unavailable
pub mod labcfg; // `lab.json` (the Cloud Lab session config) and the two answers `ui/` and `screens/` ask of it: is this key the trigger, is the menu row on
// Stage A foundation: owner adapters connect these APIs in the next integration stage.
#[allow(dead_code)]
pub mod storage;
pub mod tv; // the television as everything outside the port sees it: the interfaces the webOS port fills at boot (step L15)
pub mod webos; // which webOS this set is — nyx's os_info.json, read once at boot (release + codename)
